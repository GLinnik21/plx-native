//! Dolby Vision configuration records and H.264/HEVC packet framing: AVCC-to-
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
