//! Dolby Vision configuration records and H.264/HEVC/AV1 packet framing: AVCC-to-
//! Annex B conversion, NAL bounds checks, and keyframe detection.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **Profile 5** — single-layer IPT-PQ. `bl_compat = 0` ("none") is the field that says the
/// base layer is not displayable by a decoder that ignores the RPU, and it is the whole
/// reason this record is read at all.
#[test]
fn a_profile_5_record_parses_with_no_base_layer_compatibility() {
    let d = parse_dovi_conf(&dovi_bytes(5, 6, 1, 0, 1, 0)).expect("nine bytes is a record");
    assert_eq!(d.dv_profile, 5);
    assert_eq!(d.dv_bl_signal_compatibility_id, 0);
    assert_eq!(d.el_present_flag, 0);
    assert_eq!(d.rpu_present_flag, 1);
    assert_eq!(d.bl_present_flag, 1);
    assert_eq!(d.dv_level, 6);
    assert_eq!(d.dv_version_major, 1);
}

/// **Profile 7** — dual layer. The enhancement-layer flag is what identifies it; note the
/// compatibility id is 6 here (the value the dev server reports for its P7 item), so a reader
/// that only looked at `bl_compat == 0` would call this file fine.
#[test]
fn a_profile_7_record_parses_with_an_enhancement_layer() {
    let d = parse_dovi_conf(&dovi_bytes(7, 6, 1, 1, 1, 6)).expect("nine bytes is a record");
    assert_eq!(d.dv_profile, 7);
    assert_eq!(d.el_present_flag, 1);
    assert_eq!(
        d.dv_bl_signal_compatibility_id, 6,
        "NOT 0 — the trap this test exists to hold"
    );
}

/// **Profile 8.1** — the base layer IS HDR10, which `bl_compat = 1` is exactly the statement
/// of. Nothing about this file should change behaviour anywhere.
#[test]
fn a_profile_8_1_record_parses_as_hdr10_compatible() {
    let d = parse_dovi_conf(&dovi_bytes(8, 6, 1, 0, 1, 1)).expect("nine bytes is a record");
    assert_eq!(d.dv_profile, 8);
    assert_eq!(d.dv_bl_signal_compatibility_id, 1);
    assert_eq!(d.el_present_flag, 0);
}

/// The ABSENT case, and the short one. A file with no Dolby Vision has no side-data entry at
/// all, which `dovi_conf` reports as `None` without ever reaching here; what this pins is the
/// other way in — a TRUNCATED payload must not be read as a partial record, because nine
/// bytes taken out of a seven-byte allocation is a heap overread that returns a plausible
/// profile number rather than crashing.
#[test]
fn a_short_record_is_not_a_partial_record() {
    assert_eq!(parse_dovi_conf(&[]), None);
    assert_eq!(
        parse_dovi_conf(&[1, 0, 5, 6, 1, 0, 1, 0]),
        None,
        "eight bytes is not nine"
    );
    // exactly nine is the boundary, and it is inclusive
    assert!(parse_dovi_conf(&dovi_bytes(5, 6, 1, 0, 1, 0)).is_some());
    // a LONGER payload is fine and expected — a future FFmpeg may append fields, and the
    // nine we read keep their meaning
    let mut long = dovi_bytes(8, 6, 1, 0, 1, 1);
    long.extend_from_slice(&[0xAA; 7]);
    assert_eq!(parse_dovi_conf(&long).map(|d| d.dv_profile), Some(8));
}

/// Every field at its own offset: nine distinct byte values in, nine distinct values out.
/// A transposition of any adjacent pair — the one mistake a hand-written record parse is
/// actually prone to — fails here and nowhere else.
#[test]
fn every_field_reads_from_its_own_byte() {
    let d = parse_dovi_conf(&[10, 11, 12, 13, 14, 15, 16, 17, 18]).unwrap();
    assert_eq!(d.dv_version_major, 10);
    assert_eq!(d.dv_version_minor, 11);
    assert_eq!(d.dv_profile, 12);
    assert_eq!(d.dv_level, 13);
    assert_eq!(d.rpu_present_flag, 14);
    assert_eq!(d.el_present_flag, 15);
    assert_eq!(d.bl_present_flag, 16);
    assert_eq!(d.dv_bl_signal_compatibility_id, 17);
    assert_eq!(d.dv_md_compression, 18);
}

// -- nal_end: the 32-bit bounds guard -------------------------------------------------

#[test]
fn nal_end_accepts_a_nal_that_fits() {
    assert_eq!(nal_end(4, 10, 64), Some(14));
    assert_eq!(
        nal_end(4, 60, 64),
        Some(64),
        "a NAL ending exactly at `size` is valid"
    );
}

#[test]
fn nal_end_rejects_empty_and_overrun() {
    assert_eq!(
        nal_end(4, 0, 64),
        None,
        "a zero-length NAL terminates the walk"
    );
    assert_eq!(
        nal_end(4, 61, 64),
        None,
        "one byte past the end is rejected"
    );
}

/// Documents the defect `nal_end` exists to prevent. `usize` is 32 bits on the TV, so the
/// guard that shipped — `i + nl > size` — WRAPPED for a length near u32::MAX, passed its own
/// bounds check, and panicked the demux thread inside the slice. This assertion cannot fail
/// on a 64-bit host (the wrap is unreachable here), so it is a documentation test, not a
/// regression gate: the real gate is that `nal_end` is a named function whose doc says not to
/// inline it back. Both halves are asserted so the delta is unambiguous to a future reader.
#[test]
fn nal_end_rejects_what_the_old_32bit_guard_accepted() {
    let (i, nl, size) = (4usize, 0xFFFF_FFFCusize, 64usize);
    let old_guard_overruns = (i as u32).wrapping_add(nl as u32) > size as u32;
    assert!(
        !old_guard_overruns,
        "on 32-bit the old guard computed 0 and let this through"
    );
    assert_eq!(
        nal_end(i, nl, size),
        None,
        "the width-explicit guard rejects it on every target"
    );
}

// -- packet_to_annexb ------------------------------------------------------------------

#[test]
fn h264_idr_is_a_keyframe_and_gets_the_parameter_set_prepended() {
    // nal_unit_type is the low 5 bits of byte 0; type 5 == IDR.
    let buf = avcc(&[&[0x65, 0xAA, 0xBB]]);
    let param = [0u8, 0, 0, 1, 0x67, 0x42];
    let (key, out) = to_annexb(&buf, false, &param);
    assert!(key, "0x65 & 0x1f == 5 is an IDR");
    assert!(
        out.starts_with(&param),
        "a keyframe AU must carry the SPS/PPS"
    );
    assert_eq!(&out[param.len()..], &[0, 0, 0, 1, 0x65, 0xAA, 0xBB]);
}

#[test]
fn h264_non_idr_is_not_a_keyframe_and_gets_no_parameter_set() {
    let buf = avcc(&[&[0x41, 0x01]]); // type 1, non-IDR slice
    let (key, out) = to_annexb(&buf, false, &[0xDE, 0xAD]);
    assert!(!key);
    assert_eq!(
        out,
        vec![0, 0, 0, 1, 0x41, 0x01],
        "no parameter set on a non-keyframe"
    );
}

#[test]
fn hevc_irap_range_is_detected_as_a_keyframe() {
    // HEVC nal type is bits 1..6 of byte 0; IRAP is 16..=23.
    for t in [16u8, 19, 23] {
        let buf = avcc(&[&[t << 1, 0x01, 0x02]]);
        assert!(to_annexb(&buf, true, &[]).0, "HEVC type {t} is IRAP");
    }
    for t in [1u8, 15, 24] {
        let buf = avcc(&[&[t << 1, 0x01, 0x02]]);
        assert!(!to_annexb(&buf, true, &[]).0, "HEVC type {t} is not IRAP");
    }
}

#[test]
fn every_nal_is_emitted_with_a_start_code() {
    let buf = avcc(&[&[0x41, 0x01], &[0x41, 0x02], &[0x41, 0x03]]);
    let (_, out) = to_annexb(&buf, false, &[]);
    assert_eq!(
        out,
        vec![
            0, 0, 0, 1, 0x41, 0x01, 0, 0, 0, 1, 0x41, 0x02, 0, 0, 0, 1, 0x41, 0x03
        ]
    );
}

/// A length field that claims more bytes than the packet holds must truncate cleanly rather
/// than panic — this is the ordinary shape of a corrupt or mid-transfer-truncated AU.
#[test]
fn a_length_past_the_end_truncates_instead_of_panicking() {
    let mut buf = avcc(&[&[0x41, 0x01]]);
    buf.extend_from_slice(&0xFFFF_FF00u32.to_be_bytes()); // absurd length, no payload
    buf.extend_from_slice(&[0x41, 0x02]);
    let (key, out) = to_annexb(&buf, false, &[]);
    assert!(!key);
    assert_eq!(
        out,
        vec![0, 0, 0, 1, 0x41, 0x01],
        "the good NAL survives, the bad one stops the walk"
    );
}

#[test]
fn a_runt_packet_is_rejected_before_any_indexing() {
    let (key, out) = to_annexb(&[0x00, 0x00], false, &[0xFF]);
    assert!(!key);
    assert!(out.is_empty(), "size < nls + 1 must bail before the walk");
}

// -- AV1: leb128 ----------------------------------------------------------------------

#[test]
fn leb128_reads_single_and_multi_byte_values() {
    assert_eq!(read_leb128(&[0x00]), Some((0, 1)));
    assert_eq!(read_leb128(&[0x7f]), Some((127, 1)));
    assert_eq!(read_leb128(&[0x80, 0x01]), Some((128, 2)));
}

#[test]
fn leb128_rejects_truncation_and_missing_terminator() {
    assert_eq!(read_leb128(&[0x80]), None, "a continuation with no next byte");
    assert_eq!(read_leb128(&[]), None);
    assert_eq!(
        read_leb128(&[0x80; 8]),
        None,
        "eight continuation bytes and still no terminator"
    );
}

#[test]
fn leb128_rejects_values_above_u32_max() {
    // AV1 section 4.10.5: a leb128 value is at most 32 bits.
    assert_eq!(read_leb128(&[0xff, 0xff, 0xff, 0xff, 0x7f]), None);
    assert_eq!(
        read_leb128(&[0xff, 0xff, 0xff, 0xff, 0x0f]),
        Some((0xffff_ffff, 5)),
        "u32::MAX itself is the boundary and is accepted"
    );
}

#[test]
fn leb128_write_round_trips() {
    for v in [0u64, 127, 128, 16383, u32::MAX as u64] {
        let mut out = Vec::new();
        write_leb128(v, &mut out);
        assert_eq!(
            read_leb128(&out),
            Some((v, out.len())),
            "value {v} must read back from its own encoding"
        );
    }
    let mut out = Vec::new();
    write_leb128(200, &mut out);
    assert_eq!(out, vec![0xc8, 0x01], "minimal encoding of 200");
}

// -- AV1: walk_obus -------------------------------------------------------------------

fn obu_types(b: &[u8]) -> Vec<u8> {
    walk_obus(b)
        .expect("a well-formed OBU run")
        .iter()
        .map(|s| s.obu_type)
        .collect()
}

#[test]
fn walk_obus_reads_a_temporal_delimiter_and_a_frame() {
    // TD (type 2, size 0) then a FRAME (type 6, size 3).
    let b = [0x12, 0x00, 0x32, 0x03, 0xAA, 0xBB, 0xCC];
    assert_eq!(obu_types(&b), vec![2, 6]);
}

#[test]
fn walk_obus_of_nothing_is_nothing() {
    assert_eq!(walk_obus(&[]).map(|v| v.len()), Some(0));
}

#[test]
fn walk_obus_reads_the_extension_header() {
    // 0x36: type 6, extension flag set, size flag set. Extension byte 0x07, size 1.
    let b = [0x36, 0x07, 0x01, 0xAA];
    let spans = walk_obus(&b).expect("extension header is two bytes");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].header_len, 2);
    assert_eq!(spans[0].payload_start, 3);
    assert_eq!(spans[0].end, 4);
    assert!(
        walk_obus(&[0x36]).is_none(),
        "a truncated extension byte is malformed"
    );
}

#[test]
fn walk_obus_rejects_an_overrunning_size() {
    assert_eq!(walk_obus(&[0x32, 0x05, 0x01, 0x02]), None);
}

#[test]
fn walk_obus_rejects_the_forbidden_bit() {
    assert_eq!(walk_obus(&[0x80 | 0x12, 0x00]), None);
}

#[test]
fn walk_obus_rejects_a_size_wider_than_u32() {
    let b = [0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f];
    assert_eq!(walk_obus(&b), None);
}

// -- AV1: parse_av1c ------------------------------------------------------------------

#[test]
fn av1c_yields_the_sequence_header_verbatim() {
    let ed = [0x81, 0x00, 0x0c, 0x00, 0x0a, 0x02, 0xAA, 0xBB];
    assert_eq!(parse_av1c(&ed), vec![0x0a, 0x02, 0xAA, 0xBB]);
}

#[test]
fn av1c_sizes_a_sizeless_sequence_header() {
    // 0x08 is a sequence header with no size field; it runs to the end of the record.
    let ed = [0x81, 0x00, 0x0c, 0x00, 0x08, 0xAA, 0xBB];
    assert_eq!(parse_av1c(&ed), vec![0x0a, 0x02, 0xAA, 0xBB]);
}

#[test]
fn av1c_puts_metadata_after_the_sequence_header() {
    // 0x2a is a metadata OBU (type 5) with a one-byte payload.
    let ed = [
        0x81, 0x00, 0x0c, 0x00, 0x0a, 0x02, 0xAA, 0xBB, 0x2a, 0x01, 0xCC,
    ];
    assert_eq!(
        parse_av1c(&ed),
        vec![0x0a, 0x02, 0xAA, 0xBB, 0x2a, 0x01, 0xCC]
    );
}

#[test]
fn av1c_metadata_alone_is_never_inserted() {
    let ed = [0x81, 0x00, 0x0c, 0x00, 0x2a, 0x01, 0xCC];
    assert!(parse_av1c(&ed).is_empty());
}

#[test]
fn av1c_short_or_absent_record_is_empty() {
    assert!(parse_av1c(&[]).is_empty());
    assert!(parse_av1c(&[0x81, 0x00, 0x0c]).is_empty(), "three bytes is not a record");
}

#[test]
fn av1c_accepts_raw_obus_without_the_record_header() {
    // First byte has the marker bit clear, so this is the OBU run itself.
    assert_eq!(
        parse_av1c(&[0x0a, 0x02, 0xAA, 0xBB]),
        vec![0x0a, 0x02, 0xAA, 0xBB]
    );
}

// -- AV1: default framing -------------------------------------------------------------

const AV1_CFG: [u8; 4] = [0x0a, 0x02, 0xAA, 0xBB];

fn frame(out: &mut Vec<u8>, pkt: &[u8], key: bool, f: Av1Framing) -> bool {
    av1_packet_to_tu(pkt, key, &AV1_CFG, f, out)
}

#[test]
fn a_keyframe_without_td_or_sequence_gets_both_prepended() {
    // A FRAME OBU (0x32) with a size field, and nothing else.
    let pkt = [0x32, 0x01, 0xEE];
    let mut out = Vec::new();
    assert!(frame(&mut out, &pkt, true, Av1Framing::DEFAULT));
    let mut want = AV1_TD.to_vec();
    want.extend_from_slice(&AV1_CFG);
    want.extend_from_slice(&pkt);
    assert_eq!(out, want);
}

#[test]
fn a_non_key_packet_gets_only_the_temporal_delimiter() {
    let pkt = [0x32, 0x01, 0xEE];
    let mut out = Vec::new();
    assert!(!frame(&mut out, &pkt, false, Av1Framing::DEFAULT));
    let mut want = AV1_TD.to_vec();
    want.extend_from_slice(&pkt);
    assert_eq!(out, want);
}

#[test]
fn a_packet_that_already_carries_td_and_sequence_is_passed_through() {
    let pkt = [0x12, 0x00, 0x0a, 0x02, 0xAA, 0xBB];
    let mut out = Vec::new();
    assert!(frame(&mut out, &pkt, true, Av1Framing::DEFAULT));
    assert_eq!(out, pkt);
}

#[test]
fn a_keyframe_with_td_but_no_sequence_gets_the_config_after_the_td() {
    let pkt = [0x12, 0x00, 0x32, 0x01, 0xEE];
    let mut out = Vec::new();
    assert!(frame(&mut out, &pkt, true, Av1Framing::DEFAULT));
    let mut want = vec![0x12, 0x00];
    want.extend_from_slice(&AV1_CFG);
    want.extend_from_slice(&[0x32, 0x01, 0xEE]);
    assert_eq!(out, want);
}

#[test]
fn a_sizeless_last_obu_gets_a_size_field() {
    // 0x30 is a FRAME with no size field: it runs to the end of the packet.
    let pkt = [0x30, 0xAA, 0xBB, 0xCC];
    let mut out = Vec::new();
    frame(&mut out, &pkt, false, Av1Framing::DEFAULT);
    assert_eq!(out, vec![0x12, 0x00, 0x32, 0x03, 0xAA, 0xBB, 0xCC]);
}

#[test]
fn a_sizeless_obu_gets_a_multi_byte_size_field() {
    let mut pkt: Vec<u8> = vec![0x30];
    pkt.extend(std::iter::repeat(0x55).take(200));
    let mut out = Vec::new();
    frame(&mut out, &pkt, false, Av1Framing::DEFAULT);
    let mut want: Vec<u8> = vec![0x12, 0x00, 0x32, 0xc8, 0x01];
    want.extend(std::iter::repeat(0x55).take(200));
    assert_eq!(out, want, "200 bytes encode as c8 01");
}

#[test]
fn a_sizeless_extension_obu_keeps_its_extension_byte() {
    // 0x34: FRAME, extension flag set, no size field.
    let pkt = [0x34, 0x07, 0xAA];
    let mut out = Vec::new();
    frame(&mut out, &pkt, false, Av1Framing::DEFAULT);
    assert_eq!(out, vec![0x12, 0x00, 0x36, 0x07, 0x01, 0xAA]);
}

// -- AV1: trigger effects -------------------------------------------------------------

#[test]
fn notd_drops_only_the_temporal_delimiter() {
    let pkt = [0x32, 0x01, 0xEE];
    let f = Av1Framing::from_trigger(Some("notd"));
    let mut out = Vec::new();
    frame(&mut out, &pkt, true, f);
    let mut want = AV1_CFG.to_vec();
    want.extend_from_slice(&pkt);
    assert_eq!(out, want);
    assert!(!out.starts_with(&AV1_TD));
}

#[test]
fn noseq_drops_only_the_config_block() {
    let pkt = [0x32, 0x01, 0xEE];
    let f = Av1Framing::from_trigger(Some("noseq"));
    let mut out = Vec::new();
    frame(&mut out, &pkt, true, f);
    let mut want = AV1_TD.to_vec();
    want.extend_from_slice(&pkt);
    assert_eq!(out, want);
}

#[test]
fn notd_and_noseq_leave_the_packet_with_its_last_obu_sized() {
    let f = Av1Framing::from_trigger(Some("notd,noseq"));
    let mut out = Vec::new();
    frame(&mut out, &[0x30, 0xEE], true, f);
    assert_eq!(out, vec![0x32, 0x01, 0xEE]);
}

#[test]
fn raw_passes_the_packet_through_untouched() {
    let pkt = [0x30, 0xEE];
    let f = Av1Framing::from_trigger(Some("raw"));
    let mut out = Vec::new();
    assert!(frame(&mut out, &pkt, true, f));
    assert_eq!(out, pkt, "no TD, no config, no size field added");
}

// -- AV1: malformed packets -----------------------------------------------------------

#[test]
fn a_malformed_packet_passes_through_and_keeps_its_key_flag() {
    let pkt = [0x80, 0x00];
    let mut out = Vec::new();
    assert!(frame(&mut out, &pkt, true, Av1Framing::DEFAULT));
    assert_eq!(out, pkt, "never dropped, never reframed");
    assert!(!frame(&mut out, &pkt, false, Av1Framing::DEFAULT));
    assert_eq!(out, pkt);
}

// -- AV1: trigger parsing -------------------------------------------------------------

#[test]
fn from_trigger_without_a_value_is_the_default() {
    assert_eq!(Av1Framing::from_trigger(None), Av1Framing::DEFAULT);
}

#[test]
fn from_trigger_reads_each_token_as_one_flag() {
    assert_eq!(
        Av1Framing::from_trigger(Some("notd")),
        Av1Framing { td: false, seq: true, raw: false }
    );
    assert_eq!(
        Av1Framing::from_trigger(Some("noseq")),
        Av1Framing { td: true, seq: false, raw: false }
    );
    assert_eq!(
        Av1Framing::from_trigger(Some("raw")),
        Av1Framing { td: true, seq: true, raw: true }
    );
}

#[test]
fn from_trigger_accepts_comma_and_space_separated_tokens() {
    let both = Av1Framing { td: false, seq: false, raw: false };
    assert_eq!(Av1Framing::from_trigger(Some("notd,noseq")), both);
    assert_eq!(Av1Framing::from_trigger(Some("noseq notd")), both);
}

#[test]
fn from_trigger_ignores_unknown_tokens() {
    assert_eq!(Av1Framing::from_trigger(Some("garbage")), Av1Framing::DEFAULT);
    assert_eq!(
        Av1Framing::from_trigger(Some("notd,garbage")),
        Av1Framing { td: false, seq: true, raw: false }
    );
}
