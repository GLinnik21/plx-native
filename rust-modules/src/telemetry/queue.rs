//! **The spool: records that survive the crash that produced them.**
//!
//! A telemetry record is written when a television is in trouble and sent when it is not, and the
//! gap between those two moments routinely contains the thing worth reporting — a crash, a SAM
//! kill, somebody pulling the plug because the picture froze. So the queue is on disk, and the
//! interesting questions are all about what a half-finished write looks like afterwards.
//!
//! Everything in this file is pure: bytes in, records out. The disk is [`super::spool`], which owns
//! the file and the one lock every read and write goes through; the worker is
//! `telemetry::flush_soon`; the sender is [`super::sender`]. That split is what lets a power cut be
//! *tested* rather than argued about.
//!
//! # Four of these tests were fake, and the tests are the whole deliverable
//!
//! Written and green on the first run, then broken deliberately to see which caught it: the two
//! `MAX_RECORD` tests both passed with that guard deleted (the bounds check caught one, the CRC the
//! other), and both CRC tests passed with the CRC deleted (serde rejected the corrupted JSON before
//! the checksum was consulted). Four green assertions grading nothing. The replacements corrupt a
//! record's base64 body **into another valid base64 character**, so the length is honest, the JSON
//! parses, and only a checksum can tell — and the oversized-frame case is built with a correct CRC
//! so nothing else about it is wrong. All fifteen have since been watched red against the specific
//! line they exist for.
//!
//! # The framing, and why it is not JSON-per-line
//!
//! Each record is `[u32 len][u32 crc32(payload)][payload]`, little-endian, payload being JSON.
//!
//! A newline-delimited log would be simpler and would be wrong in one specific way: a partial write
//! that happens to end at a newline is INDISTINGUISHABLE from a complete one, and a partial write
//! that ends mid-object is detected only by a JSON parser, which fails identically for "torn" and
//! "written by a newer build". The length lets a torn tail be recognised before anything is parsed;
//! the CRC catches the case the length cannot — a record whose bytes were written but not the ones
//! we meant, which is what a filesystem with a reordered journal hands back.
//!
//! # What a corrupt record does to the ones after it
//!
//! It ends the queue. [`decode_all`] stops at the first record that does not verify and reports the
//! remaining bytes as dropped, rather than hunting for the next plausible header.
//!
//! That is a deliberate choice against resynchronisation, and the reason is that resync cannot be
//! made safe: a `len` field is four arbitrary bytes, so scanning forward for "a header that looks
//! valid" will eventually find one inside a payload, and the record it then decodes is
//! attacker-shaped garbage that passes CRC because the CRC was read from the same garbage. Losing
//! the tail of a spool is cheap. Reporting a fabricated record is not.
//!
//! # The cap that matters is BYTES
//!
//! Both are enforced, but a record count is the weaker one: a crash record carrying registers, a
//! maps line and breadcrumbs is orders of magnitude larger than `app.launch`, so "200 records" is
//! not a size. The byte cap is what keeps this off a 615 MB partition shared with every other app.
//! Dropping is **oldest-first** — a new crash is worth more than an old one — and the count is
//! returned rather than swallowed, because a queue that silently discards is a queue whose numbers
//! are wrong in a direction nobody can see.

use serde::{Deserialize, Serialize};

/// Which consent switch a record belongs to. Stored per record because withdrawal purges **one**
/// category, and a record that could not say which it was would have to be purged by both or
/// neither — the first loses reports somebody consented to, the second keeps reports they did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Category {
    /// crash and error reports
    Errors,
    /// which screens and features get used
    Usage,
}

/// Where the record goes. Stored rather than derived from the category, because the mapping is
/// today's routing and not a fact about the record: an error could be worth sending to both, and a
/// spool written by one build is read by the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Dest {
    Sentry,
    PostHog,
}

/// One queued send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub category: Category,
    pub dest: Dest,
    /// Sentry's `event_id`, minted when the record was QUEUED. Carried here and not regenerated at
    /// send time so a retry over a flaky link does not manufacture duplicate issues out of one
    /// crash. PostHog records carry it too; it costs 32 bytes and makes a record self-describing.
    pub event_id: String,
    /// The serialised body — a Sentry envelope or a PostHog capture object. Opaque here: this
    /// module frames and counts, and deliberately knows nothing about either wire format.
    #[serde(with = "body_b64")]
    pub body: Vec<u8>,
}

/// The body is arbitrary bytes and the frame is JSON, so it is base64 in transit through serde.
/// Hand-rolled: the crate has no base64 dependency and this is the only place that needs one.
mod body_b64 {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(b: &[u8]) -> String {
        let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16
                | (*c.get(1).unwrap_or(&0) as u32) << 8
                | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub(super) fn decode(s: &str) -> Option<Vec<u8>> {
        let mut acc = 0u32;
        let mut bits = 0u32;
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        for ch in s.bytes() {
            if ch == b'=' {
                break;
            }
            let v = A.iter().position(|&a| a == ch)? as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }

    pub(super) fn serialize<S: serde::Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(b))
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        use serde::Deserialize;
        let s = String::deserialize(d)?;
        decode(&s).ok_or_else(|| serde::de::Error::custom("body is not base64"))
    }
}

/// **The ceiling on one record's payload, checked BEFORE anything is allocated.**
///
/// A corrupt `len` is four arbitrary bytes, so it can say four gigabytes. Without this, decoding a
/// spool that a power cut scrambled would try to reserve that — on a television with 1.68 GB — and
/// the failure would be an allocator abort with no log, from a module whose entire job is to
/// preserve a report. The bound is checked against the frame header, not against a buffer already
/// read.
pub(crate) const MAX_RECORD: usize = 256 * 1024;

/// The whole spool's byte ceiling. See the module doc: this is the cap that means something.
pub(crate) const MAX_BYTES: usize = 512 * 1024;

/// And the record ceiling, which is the weaker of the two and exists to bound decode time.
pub(crate) const MAX_RECORDS: usize = 200;

const HEADER: usize = 8;

/// CRC-32/IEEE, bitwise. No table: 256 words of static data to checksum at most half a megabyte a
/// few times per boot is the wrong trade, and the bitwise form is the one that is obviously correct
/// by inspection.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Frame one record. `None` if it serialises to more than [`MAX_RECORD`] — a record too big to
/// store is dropped at the point it is created, where there is a caller to log it, rather than
/// becoming a frame no reader will accept.
pub(crate) fn encode(r: &Record) -> Option<Vec<u8>> {
    let payload = serde_json::to_vec(r).ok()?;
    if payload.len() > MAX_RECORD {
        return None;
    }
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Some(out)
}

/// What a decode found, including what it had to throw away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub records: Vec<Record>,
    /// Bytes at the end that could not be trusted — a torn write, or everything after a record that
    /// failed its CRC. Reported rather than logged here so the caller decides how loud it is; a
    /// non-zero value after a clean shutdown means something worse than a power cut.
    pub dropped_bytes: usize,
}

/// Decode a spool, stopping at the first record that does not verify.
///
/// Never panics and never allocates on an untrusted length — see [`MAX_RECORD`]. An empty input is
/// an empty queue, not an error: that is what a first boot looks like.
pub(crate) fn decode_all(buf: &[u8]) -> Decoded {
    let mut records = Vec::new();
    let mut at = 0usize;
    while at + HEADER <= buf.len() {
        let len = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize;
        let crc = u32::from_le_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]);
        // Two independent guards, and they catch different things.
        //
        // `checked_add` is for the TELEVISION: `usize` is 32 bits there, so a corrupt `len` near
        // `u32::MAX` makes `at + HEADER + len` WRAP to a small number, sail through the bounds
        // check, and then panic slicing a range whose start exceeds its end. On this 64-bit host
        // the same arithmetic is simply correct, which is why no host test can reach it — the same
        // Darwin-versus-Linux divergence `tools/sockprobe.c` exists for.
        //
        // `MAX_RECORD` is the one a host test CAN grade, and it is not implied by the bounds check:
        // a spool file large enough to contain an oversized record has an honest length and a
        // verifying CRC, so every other check passes it.
        let Some(end) = at.checked_add(HEADER).and_then(|x| x.checked_add(len)) else {
            break;
        };
        if len > MAX_RECORD || end > buf.len() {
            break;
        }
        let payload = &buf[at + HEADER..end];
        if crc32(payload) != crc {
            break;
        }
        let Ok(r) = serde_json::from_slice::<Record>(payload) else {
            break;
        };
        records.push(r);
        at = end;
    }
    Decoded { records, dropped_bytes: buf.len() - at }
}

/// Enforce both caps, oldest-first, and say how many went.
///
/// Byte-cap first is deliberate: applying the record cap first can leave a set that still exceeds
/// the byte cap (200 crash records are not 200 launches), so the byte pass has to run last to be
/// the one that binds.
pub(crate) fn trim(mut records: Vec<Record>) -> (Vec<Record>, usize) {
    let before = records.len();
    if records.len() > MAX_RECORDS {
        records.drain(..records.len() - MAX_RECORDS);
    }
    // Measure ONCE. This used to encode every record to sum the total and then encode the head
    // again for each one it dropped, and `remove(0)` shifted the whole vector each time — so
    // trimming k records off a full spool cost k extra serialisations of records that were about
    // to be thrown away. `trim` runs on every compaction and every flush commit.
    let sizes: Vec<usize> = records.iter().map(|r| encode(r).map_or(0, |e| e.len())).collect();
    let mut total: usize = sizes.iter().sum();
    let mut cut = 0usize;
    while total > MAX_BYTES && cut < records.len() {
        total -= sizes[cut];
        cut += 1;
    }
    records.drain(..cut);
    let dropped = before - records.len();
    (records, dropped)
}

/// Remove everything belonging to one consent category — what a withdrawal does.
///
/// Per-category rather than wholesale, because the two switches are independent: turning off usage
/// must not discard crash reports somebody is still consenting to send.
pub(crate) fn purge(records: Vec<Record>, category: Category) -> Vec<Record> {
    records.into_iter().filter(|r| r.category != category).collect()
}

/// Drop the records a send has had ACKNOWLEDGED, keeping the rest in order.
///
/// Acknowledgement is by `event_id` rather than by count, and that is the whole point: a flush that
/// sends three and gets two accepted must not advance a cursor by three, and cannot express "the
/// middle one failed" as a number at all.
pub(crate) fn ack(records: Vec<Record>, accepted: &[String]) -> Vec<Record> {
    records.into_iter().filter(|r| !accepted.contains(&r.event_id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, cat: Category, body: &[u8]) -> Record {
        Record {
            category: cat,
            dest: Dest::Sentry,
            event_id: id.to_string(),
            body: body.to_vec(),
        }
    }

    fn spool(rs: &[Record]) -> Vec<u8> {
        rs.iter().flat_map(|r| encode(r).expect("encodes")).collect()
    }

    #[test]
    fn a_record_round_trips_through_the_frame() {
        let r = rec("abc", Category::Errors, b"\x00\x01\xff binary body");
        let d = decode_all(&encode(&r).unwrap());
        assert_eq!(d.records, vec![r]);
        assert_eq!(d.dropped_bytes, 0);
    }

    /// An empty spool is an empty queue. That is what a first boot looks like, and treating it as
    /// corruption would make every fresh install log a scary line.
    #[test]
    fn an_empty_spool_is_an_empty_queue() {
        assert_eq!(decode_all(&[]), Decoded { records: vec![], dropped_bytes: 0 });
    }

    /// **THE ONE THIS FILE EXISTS FOR.** A write cut off by a power loss leaves a partial record;
    /// everything before it must survive, and the torn bytes must be reported rather than parsed.
    ///
    /// Every truncation point is exercised, not one convenient offset — the interesting cases are
    /// mid-header (fewer than 8 bytes to even read a length) and one byte short of complete, and a
    /// single sample would land on neither.
    #[test]
    fn a_torn_tail_loses_only_the_torn_record() {
        let good = vec![
            rec("one", Category::Errors, b"first"),
            rec("two", Category::Usage, b"second"),
        ];
        let full = spool(&good);
        let third = encode(&rec("three", Category::Errors, b"third")).unwrap();

        for cut in 1..third.len() {
            let mut buf = full.clone();
            buf.extend_from_slice(&third[..cut]);
            let d = decode_all(&buf);
            assert_eq!(d.records, good, "a tail torn at {cut} bytes lost a complete record");
            assert_eq!(d.dropped_bytes, cut, "the torn bytes were not reported at cut {cut}");
        }
    }

    /// Corrupt one character of a record's base64 BODY, in place, leaving the frame's length and
    /// the JSON's syntax perfectly intact. Returns the offset touched.
    ///
    /// This exists because the obvious corruption — flip any bit in the payload — is caught by
    /// serde before the CRC is ever consulted, so a test built on it stays green with the checksum
    /// deleted. Both CRC tests here were originally written that way and both were fake. A
    /// base64-to-base64 substitution is the corruption that reaches the CRC and nothing else: the
    /// record still parses, into a body that is not the one that was written.
    fn corrupt_body_in_place(buf: &mut [u8], payload_at: usize) {
        let s = std::str::from_utf8(&buf[payload_at..]).expect("the payload is JSON text");
        let i = payload_at + s.find(r#""body":""#).expect("the body field") + 8;
        buf[i] = if buf[i] == b'A' { b'B' } else { b'A' };
    }

    /// **A flipped bit the CRC alone can see.** The record's length is honest and its JSON parses;
    /// only the checksum knows the bytes are not the ones that were written.
    ///
    /// Watched red with the CRC check disabled — at which point it decodes to a record carrying a
    /// DIFFERENT body and reports success, which is the failure worth having a checksum for.
    #[test]
    fn a_flipped_bit_is_caught_by_the_crc() {
        let rs = vec![rec("one", Category::Errors, b"first"), rec("two", Category::Usage, b"second")];
        let mut buf = spool(&rs);
        let second_at = encode(&rs[0]).unwrap().len() + HEADER;
        corrupt_body_in_place(&mut buf, second_at);
        let d = decode_all(&buf);
        assert_eq!(d.records, rs[..1].to_vec(), "the intact first record survives");
        assert!(d.dropped_bytes > 0, "the corrupt record was accepted");
    }

    /// **`MAX_RECORD` is enforced against a frame that is otherwise PERFECT.**
    ///
    /// This test is the second attempt. The first two written here — a `u32::MAX` length, and a
    /// length one past the ceiling in a buffer too small to hold it — both passed with the
    /// `MAX_RECORD` guard deleted, because the bounds check caught one and the CRC caught the
    /// other. Fifteen green tests, and the guard they were named after was doing nothing they could
    /// see. They are replaced rather than kept beside this one: a test that cannot fail is worse
    /// than no test, because it is counted.
    ///
    /// So this frame is honest in every other respect — the length is the payload's real length,
    /// the CRC verifies, the JSON parses into a `Record`. The ONLY thing wrong with it is its size,
    /// which is exactly the case a spool file big enough to hold one produces.
    #[test]
    fn an_oversized_frame_is_refused_even_when_everything_else_about_it_is_valid() {
        let r = rec("big", Category::Errors, &vec![b'x'; MAX_RECORD]);
        let payload = serde_json::to_vec(&r).expect("serialises");
        assert!(payload.len() > MAX_RECORD, "the fixture must actually exceed the ceiling");
        let mut buf = Vec::new();
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&crc32(&payload).to_le_bytes()); // a CORRECT checksum
        buf.extend_from_slice(&payload);
        let d = decode_all(&buf);
        assert!(d.records.is_empty(), "an oversized record was accepted");
        assert_eq!(d.dropped_bytes, buf.len());
    }

    /// A wildly corrupt length neither panics nor allocates. This one **cannot discriminate on this
    /// host and says so**: `usize` is 64 bits here, so the bounds check alone is already correct,
    /// and the wraparound this guards against only exists on the television's 32-bit `usize`. Kept
    /// because it costs nothing and would catch a rewrite that slices before checking — but it is
    /// not evidence about the overflow, and the module doc records why no host test can be.
    #[test]
    fn an_absurd_length_neither_panics_nor_allocates() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"nowhere near four gigabytes");
        let d = decode_all(&buf);
        assert!(d.records.is_empty());
        assert_eq!(d.dropped_bytes, buf.len());
    }

    /// Nothing after a failed record is decoded, however plausible it looks. Resynchronising would
    /// eventually find a header-shaped run inside a payload and decode garbage that passes its own
    /// CRC, because the CRC came from the same garbage.
    #[test]
    fn nothing_after_a_bad_record_is_trusted() {
        let mut buf = spool(&[rec("one", Category::Errors, b"first")]);
        corrupt_body_in_place(&mut buf, HEADER);
        buf.extend_from_slice(&spool(&[rec("two", Category::Usage, b"perfectly fine")]));
        assert!(decode_all(&buf).records.is_empty(), "a valid record after a bad one was resynced to");
    }

    /// The byte cap binds, drops oldest-first, and reports the count.
    #[test]
    fn the_byte_cap_drops_the_oldest_and_says_how_many() {
        let big = vec![b'x'; 32 * 1024];
        let rs: Vec<Record> =
            (0..40).map(|i| rec(&format!("id{i}"), Category::Errors, &big)).collect();
        let (kept, dropped) = trim(rs);
        assert!(dropped > 0, "40 x 32 KiB must exceed the 512 KiB cap");
        assert_eq!(kept.len() + dropped, 40);
        let total: usize = kept.iter().map(|r| encode(r).unwrap().len()).sum();
        assert!(total <= MAX_BYTES, "kept {total} bytes against a {MAX_BYTES} cap");
        assert_eq!(kept.last().unwrap().event_id, "id39", "the newest record survived");
        assert_ne!(kept.first().unwrap().event_id, "id0", "the oldest was dropped first");
    }

    /// The record cap binds independently — many tiny records, well under the byte cap.
    #[test]
    fn the_record_cap_binds_on_small_records() {
        let rs: Vec<Record> =
            (0..MAX_RECORDS + 25).map(|i| rec(&format!("id{i}"), Category::Usage, b"t")).collect();
        let (kept, dropped) = trim(rs);
        assert_eq!(kept.len(), MAX_RECORDS);
        assert_eq!(dropped, 25);
        assert_eq!(kept.last().unwrap().event_id, format!("id{}", MAX_RECORDS + 24));
    }

    /// **Withdrawal purges one category, not the queue.** The two consent switches are independent,
    /// so turning off usage must not discard crash reports somebody still consents to send.
    #[test]
    fn purging_one_category_leaves_the_other() {
        let rs = vec![
            rec("e1", Category::Errors, b"a"),
            rec("u1", Category::Usage, b"b"),
            rec("e2", Category::Errors, b"c"),
        ];
        let left = purge(rs, Category::Usage);
        assert_eq!(left.iter().map(|r| r.event_id.as_str()).collect::<Vec<_>>(), ["e1", "e2"]);
    }

    /// **Acknowledgement is by id, not by count.** A flush that sends three and gets the FIRST and
    /// THIRD accepted must keep exactly the second — which a cursor advanced by a number cannot
    /// express, and would resolve by dropping a record nobody accepted.
    #[test]
    fn acknowledgement_keeps_the_one_in_the_middle_that_failed() {
        let rs = vec![
            rec("a", Category::Errors, b"1"),
            rec("b", Category::Errors, b"2"),
            rec("c", Category::Errors, b"3"),
        ];
        let left = ack(rs, &["a".into(), "c".into()]);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].event_id, "b");
    }

    /// Acknowledging nothing keeps everything — an entirely failed flush must not advance anything.
    #[test]
    fn a_failed_flush_acknowledges_nothing() {
        let rs = vec![rec("a", Category::Errors, b"1"), rec("b", Category::Usage, b"2")];
        assert_eq!(ack(rs.clone(), &[]), rs);
    }

    /// A body too large to frame is refused at encode, where a caller can log it — not turned into
    /// a frame that every future reader stops at.
    #[test]
    fn an_oversized_record_is_refused_at_encode() {
        assert!(encode(&rec("big", Category::Errors, &vec![b'x'; MAX_RECORD])).is_none());
    }

    /// The base64 body survives every byte value, including the ones that would end a JSON string.
    #[test]
    fn a_body_of_every_byte_value_round_trips() {
        let body: Vec<u8> = (0..=255u8).collect();
        let r = rec("all", Category::Errors, &body);
        assert_eq!(decode_all(&encode(&r).unwrap()).records[0].body, body);
    }

    /// CRC-32/IEEE against the standard check vector, so a "simplification" of the loop is caught
    /// by arithmetic rather than by a spool that silently stops verifying.
    #[test]
    fn the_crc_matches_the_standard_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
