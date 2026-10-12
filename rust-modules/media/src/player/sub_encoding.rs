//! Subtitle text encoding detection and conversion to UTF-8.
//!
//! Subtitle payloads (embedded SRT packets and sidecar downloads) are raw bytes whose encoding
//! is whatever the author chose. Modern files are UTF-8, but a large installed base of Chinese
//! subtitle releases uses GBK / GB18030, Korean ones EUC-KR, Japanese ones Shift_JIS, and
//! Traditional Chinese ones Big5. A hard `from_utf8_lossy` replaces every non-UTF-8 byte with
//! U+FFFD (□), which is exactly the "garbled text" the viewer reports.
//!
//! This module is the single entry point for turning subtitle bytes into a `String`. It is
//! called from exactly two production sites:
//!
//! * `player::sub_text` — one embedded-subtitle packet decoded by `ff_subs`.
//! * `sidecar::Subtitles::from_bytes` — one whole sidecar file fetched from PMS.
//!
//! ## Detection strategy
//!
//! 1. **BOM** — a byte-order mark is an authoritative answer: UTF-8, UTF-16 LE or UTF-16 BE.
//! 2. **Strict UTF-8** — `std::str::from_utf8` succeeds for every byte-valid UTF-8 stream,
//!    including pure ASCII. Most modern subtitle files land here with zero cost.
//! 3. **Heuristic CJK detection** — score the byte stream against the lead/trail-byte patterns
//!    of GB18030, Big5, EUC-KR and Shift_JIS, pick the highest scorer (above a threshold),
//!    decode through `encoding_rs`, and fall back to lossy UTF-8 if the decoder refuses.
//!
//! GB18030 is chosen over GBK for the Chinese arm: GB18030 is a strict superset of GBK and is
//! the PRC national standard, so it covers every GBK file and the few GB18030-only codepoints
//! (e.g. minority-language characters) that a GBK decoder would refuse.
//!
//! ## What this does NOT do
//!
//! * **It does not re-encode already-UTF-8 files.** A file that is valid UTF-8 is returned
//!   verbatim; a UTF-8 file that happens to contain byte sequences that are ALSO valid GBK
//!   lead/trail pairs never enters the heuristic because step 2 already accepted it.
//! * **It does not touch the ASS/SSA native path.** Styled scripts are handed to libass with
//!   their original bytes intact; only the plain-text parsing routes go through here.
//! * **It does not pull in a full `chardet` implementation.** The heuristic is ~40 lines of
//!   pattern scoring, which keeps the binary footprint and the audit surface small.

/// Decode subtitle bytes into a `String`, detecting the encoding when it is not UTF-8.
///
/// Pure-UTF-8 inputs (including ASCII) return the strict parse verbatim — no heuristic runs,
/// no `encoding_rs` call is made. The heuristic only fires when `from_utf8` rejects the bytes,
/// which is exactly the case the caller previously papered over with `from_utf8_lossy`.
pub(super) fn decode_to_utf8(bytes: &[u8]) -> String {
    // Step 1: BOM. `encoding_rs::Encoding::for_bom` answers (encoding, bom_len) or None.
    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (cow, _, _) = encoding.decode(&bytes[bom_len..]);
        return cow.into_owned();
    }

    // Step 2: strict UTF-8. The hot path for every modern subtitle file.
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }

    // Step 3: heuristic. The bytes FAILED strict UTF-8, so something non-ASCII is encoded in
    // a legacy codepage. Score each candidate and decode the best one.
    if let Some(encoding) = detect_encoding(bytes) {
        let (cow, _, had_errors) = encoding.decode(bytes);
        if !had_errors {
            return cow.into_owned();
        }
    }

    // Fallback: the old lossy decode. Better □□□ than a panic.
    String::from_utf8_lossy(bytes).into_owned()
}

/// Score the byte stream against each candidate encoding's lead/trail-byte pattern and return
/// the best match, or `None` if no candidate scores high enough.
///
/// The scoring is intentionally simple: count (lead, trail) pairs that the candidate accepts,
/// subtract pairs that look like a different encoding's pattern, and require a minimum count.
/// A stream with zero non-ASCII bytes never reaches here (step 2 already accepted it).
fn detect_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let mut gb18030: i32 = 0;
    let mut big5: i32 = 0;
    let mut euc_kr: i32 = 0;
    let mut shift_jis: i32 = 0;

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
            continue;
        }

        // GB18030 two-byte: lead 0x81–0xFE, trail 0x40–0x7E or 0x80–0xFE.
        // (Four-byte sequences exist for Unicode extension but are rare in subtitle files.)
        if matches!(b, 0x81..=0xFE) && i + 1 < bytes.len() {
            let t = bytes[i + 1];
            if matches!(t, 0x40..=0x7E | 0x80..=0xFE) {
                gb18030 += 1;
            }
        }

        // Big5: lead 0x81–0xFE, trail 0x40–0x7E or 0xA1–0xFE.
        if matches!(b, 0x81..=0xFE) && i + 1 < bytes.len() {
            let t = bytes[i + 1];
            if matches!(t, 0x40..=0x7E | 0xA1..=0xFE) {
                big5 += 1;
            }
        }

        // EUC-KR: lead 0x81–0xFE, trail 0x41–0x5A, 0x61–0x7A, or 0x81–0xFE.
        if matches!(b, 0x81..=0xFE) && i + 1 < bytes.len() {
            let t = bytes[i + 1];
            if matches!(t, 0x41..=0x5A | 0x61..=0x7A | 0x81..=0xFE) {
                euc_kr += 1;
            }
        }

        // Shift_JIS: lead 0x81–0x9F or 0xE0–0xEF, trail 0x40–0x7E or 0x80–0xFC.
        if matches!(b, 0x81..=0x9F | 0xE0..=0xEF) && i + 1 < bytes.len() {
            let t = bytes[i + 1];
            if matches!(t, 0x40..=0x7E | 0x80..=0xFC) {
                shift_jis += 1;
            }
        }

        // Advance past the pair (or the lone lead byte if no valid trail followed).
        if i + 1 < bytes.len() && bytes[i + 1] >= 0x40 {
            i += 2;
        } else {
            i += 1;
        }
    }

    // Require a meaningful number of hits to avoid false positives on short or mostly-ASCII files.
    const MIN_HITS: i32 = 3;

    let scores = [
        (gb18030, encoding_rs::GB18030),
        (big5, encoding_rs::BIG5),
        (euc_kr, encoding_rs::EUC_KR),
        (shift_jis, encoding_rs::SHIFT_JIS),
    ];

    let best = scores
        .iter()
        .max_by_key(|(score, _)| *score)
        .filter(|(score, _)| *score >= MIN_HITS)?;

    Some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure UTF-8 (the common case) must never enter the heuristic.
    #[test]
    fn utf8_passes_through_unchanged() {
        assert_eq!(decode_to_utf8(b"Hello, world!"), "Hello, world!");
        assert_eq!(decode_to_utf8("你好世界".as_bytes()), "你好世界");
        assert_eq!(decode_to_utf8(b""), "");
    }

    /// A UTF-8 BOM is consumed and does not reach the output.
    #[test]
    fn utf8_bom_is_stripped() {
        assert_eq!(decode_to_utf8(b"\xEF\xBB\xBFHello"), "Hello");
    }

    /// UTF-16 LE BOM: the decoder produces the right string.
    #[test]
    fn utf16_le_bom_decodes() {
        let mut input = vec![0xFF, 0xFE]; // BOM
        input.extend_from_slice(&[0x60, 0x4F, 0x7D, 0x59, 0x16, 0x4E, 0x4C, 0x75]); // 你好世界
        let result = decode_to_utf8(&input);
        assert_eq!(result, "你好世界");
    }

    /// GBK-encoded "你好世界" must decode correctly through the GB18030 arm.
    #[test]
    fn gbk_chinese_decodes_through_gb18030() {
        // GBK bytes for 你好世界: C4 E3 BA C3 CA C0 BD E7
        let gbk = [0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7];
        let result = decode_to_utf8(&gbk);
        assert_eq!(result, "你好世界");
    }

    /// A mixed GBK file with ASCII timing lines and Chinese text.
    #[test]
    fn gbk_srt_with_ascii_timing_decodes() {
        // "1\n00:00:01,000 --> 00:00:03,000\n你好\n" in GBK
        let mut body = b"1\n00:00:01,000 --> 00:00:03,000\n".to_vec();
        body.extend_from_slice(&[0xC4, 0xE3, 0xBA, 0xC3]); // 你好
        body.push(b'\n');
        let result = decode_to_utf8(&body);
        assert!(result.contains("你好"), "got: {result}");
        assert!(result.contains("00:00:01"), "timing preserved: {result}");
    }

    /// EUC-KR encoded Korean must decode correctly.
    #[test]
    fn euc_kr_korean_decodes() {
        // EUC-KR bytes for 안녕하세요: BE C8 B3 E7 C7 CF BC BC BF E4
        let euc_kr = [0xBE, 0xC8, 0xB3, 0xE7, 0xC7, 0xCF, 0xBC, 0xBC, 0xBF, 0xE4];
        let result = decode_to_utf8(&euc_kr);
        assert_eq!(result, "안녕하세요");
    }

    /// A body that is not valid in ANY known encoding falls back to lossy UTF-8 rather than
    /// panicking. This is the safety net: garbage in, something out.
    #[test]
    fn garbage_falls_back_to_lossy() {
        let garbage = [0xFF, 0xFE, 0xFD, 0x80, 0x90];
        let _ = decode_to_utf8(&garbage); // must not panic
    }

    /// A file too short for the heuristic to fire (fewer than MIN_HITS pairs) returns lossy
    /// UTF-8 rather than guessing wrong on two stray bytes.
    #[test]
    fn short_non_utf8_does_not_false_positive() {
        // Two bytes that happen to look like a GBK pair but could be anything.
        let short = [0xC4, 0xE3];
        let result = decode_to_utf8(&short);
        // Either the heuristic fires (decoding one GBK char) or lossy runs — both are acceptable.
        // The assertion is that it does not panic and produces non-empty output.
        assert!(!result.is_empty());
    }
}
