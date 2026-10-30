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

/// **An H.264 I-frame that is not an IDR is a keyframe when the container says so.** A Blu-ray
/// remux (Brink of War, 2026-10-26) has one IDR, its first frame; every later random access
/// point is a non-IDR I-slice behind a recovery point, which Matroska flags as a keyframe and its
/// Cues index. With only IDR counted, every seek landed on one of them and the post-seek rebase
/// waited for a keyframe that never came: the player sat on "Buffering…" at the target for good.
#[test]
fn h264_a_flagged_non_idr_i_frame_is_a_keyframe_and_gets_the_parameter_set() {
    // slice header: first_mb_in_slice ue(0) = '1', slice_type ue(7) (I, all slices) = '0001000'
    let i_slice: &[u8] = &[0x21, 0x88, 0xAA, 0xBB];
    let buf = avcc(&[i_slice]);
    let param = [0, 0, 0, 1, 0x67, 0x42];
    let (key, out) = to_annexb_flagged(&buf, &param);
    assert!(key, "a container keyframe whose slice is I is a random access point");
    assert_eq!(&out[..param.len()], &param, "and carries the parameter sets like an IDR");
    // slice_type ue(2) (I) = '011'
    assert!(to_annexb_flagged(&avcc(&[&[0x21, 0xB0, 0x00]]), &[]).0, "slice_type 2 is I too");
}

/// The container's flag alone does not make a P- or B-slice a keyframe (a muxer that flags every
/// block must not let a seek land on a frame that references pictures the decoder never saw),
/// and an unflagged I-slice is not one either: only an IDR needs no flag.
#[test]
fn h264_a_flagged_p_slice_and_an_unflagged_i_slice_are_not_keyframes() {
    // slice_type ue(0) (P) = '1' -> header byte '1' '1' ... = 0xC0
    assert!(!to_annexb_flagged(&avcc(&[&[0x21, 0xC0, 0x00]]), &[]).0, "a flagged P-slice");
    // slice_type ue(6) (B, all slices) = '00111' -> '1' '00111' '00' = 0x9C
    assert!(!to_annexb_flagged(&avcc(&[&[0x01, 0x9C, 0x00]]), &[]).0, "a flagged B-slice");
    assert!(!to_annexb(&avcc(&[&[0x21, 0x88, 0xAA]]), false, &[]).0, "an unflagged I-slice");
}

/// An unflagged I-slice IS a keyframe when its AU repeats the SPS ahead of it: that is how a
/// Blu-ray stream marks a random access point, whatever its muxer flagged.
#[test]
fn h264_an_i_slice_behind_an_in_band_sps_is_a_keyframe() {
    let (sps, pps): (&[u8], &[u8]) = (&[0x67, 0x4D, 0x40, 0x29], &[0x68, 0xEE, 0x3C]);
    assert!(to_annexb(&avcc(&[sps, pps, &[0x21, 0x88, 0xAA]]), false, &[]).0, "SPS + PPS + I-slice");
    assert!(!to_annexb(&avcc(&[sps, pps, &[0x21, 0xC0, 0x00]]), false, &[]).0, "SPS + PPS + P-slice");
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

// -- Dolby Vision Profile 7 -> 8.1 on the feed path -------------------------------------
//
// HEVC NAL header: F(1) type(6) layer_hi(1) | layer_lo(5) tid(3). A base-layer TRAIL_R slice is
// `02 01`; an RPU (UNSPEC62) is `7C 01`; a single-track dual layer wraps each enhancement-layer
// NAL in UNSPEC63, `7E 01`; the same slice carried in layer 1 instead is `02 09`.

/// Only a dual layer with a cross-compatible base layer is converted: the demuxer's reading of
/// the file has to agree with `Dovi::converts_to_p81`, which decided the route and the payload.
#[test]
fn only_a_cross_compatible_profile_7_record_arms_the_conversion() {
    let p7 = parse_dovi_conf(&dovi_bytes(7, 6, 1, 1, 1, 6)).unwrap();
    assert!(dovi_converts_to_p81(&p7));
    let p7_no_bl = parse_dovi_conf(&dovi_bytes(7, 6, 1, 1, 1, 0)).unwrap();
    assert!(!dovi_converts_to_p81(&p7_no_bl), "no base layer to leave on screen");
    let p7_single = parse_dovi_conf(&dovi_bytes(7, 6, 1, 0, 1, 6)).unwrap();
    assert!(!dovi_converts_to_p81(&p7_single), "no enhancement layer, nothing to convert");
    for (profile, compat) in [(5, 0), (8, 1), (8, 4)] {
        let d = parse_dovi_conf(&dovi_bytes(profile, 6, 1, 0, 1, compat)).unwrap();
        assert!(!dovi_converts_to_p81(&d), "P{profile} compat {compat}");
    }
}

#[test]
fn the_layer_id_reads_both_halves_of_the_nal_header() {
    assert_eq!(hevc_layer_id(&[0x02, 0x01]), 0);
    assert_eq!(hevc_layer_id(&[0x02, 0x09]), 1);
    assert_eq!(hevc_layer_id(&[0x03, 0x01]), 32, "bit 0 of byte 0 is the HIGH bit");
    assert_eq!(hevc_layer_id(&[0x02]), 0, "a runt NAL is never mistaken for an EL");
}

#[test]
fn an_enhancement_layer_nal_is_recognised_by_either_spelling() {
    assert!(is_dv_enhancement_layer(&[0x7E, 0x01, 0x40, 0x01]), "UNSPEC63 wrapper");
    assert!(is_dv_enhancement_layer(&[0x02, 0x09]), "a layer-1 slice");
    assert!(!is_dv_enhancement_layer(&[0x02, 0x01]), "a base-layer slice");
    assert!(!is_dv_enhancement_layer(&[0x7C, 0x01]), "the RPU is not the enhancement layer");
    assert!(!is_dv_enhancement_layer(&[0x7E]), "a runt NAL");
}

/// Armed, the enhancement layer never reaches the pipeline, whichever way it is carried, and
/// the base layer is emitted byte for byte.
#[test]
fn an_armed_profile_7_feed_drops_the_enhancement_layer() {
    let bl = [0x02u8, 0x01, 0xAA];
    let el_wrapped = [0x7Eu8, 0x01, 0x02, 0x01, 0xBB];
    let el_layer1 = [0x02u8, 0x09, 0xBB];
    let buf = avcc(&[&bl, &el_wrapped, &el_layer1]);
    let (_, out) = to_annexb_p7(&buf, &[]);
    assert_eq!(out, vec![0, 0, 0, 1, 0x02, 0x01, 0xAA]);
}

/// The rewrite is `dovi_tool -m 2`'s, byte for byte: dovi_tool's own MEL test RPU comes out as
/// the 8.1 RPU dovi_tool's test suite expects of that conversion (`tests/fixtures/dovi-p7-*`, from
/// quietvoid/dovi_tool `assets/tests/`), and the packet carries it in the RPU's place.
#[test]
fn an_armed_feed_rewrites_profile_7_rpus_as_dovi_tool_does() {
    let orig = rpu_fixture_nal(include_bytes!("../../../tests/fixtures/dovi-p7-mel-rpu.bin"));
    let want = rpu_fixture_nal(include_bytes!("../../../tests/fixtures/dovi-p7-mel-to-81-rpu.bin"));
    assert_eq!(rewrite_dv_rpu_p7_to_p81(&orig).as_deref(), Some(&want[..]));
    let bl = [0x02u8, 0x01, 0xAA];
    let (_, out) = to_annexb_p7(&avcc(&[&bl, &orig]), &[]);
    let mut expect = vec![0, 0, 0, 1, 0x02, 0x01, 0xAA, 0, 0, 0, 1];
    expect.extend_from_slice(&want);
    assert_eq!(out, expect);
}

/// A full enhancement layer's RPU converts too. dovi_tool keeps no mode-2 output for its FEL
/// asset (its `fel_to_81.bin` is the mapping-preserving mode 5), so this holds the profile the
/// result parses back as rather than its bytes: what the panel is told is 8.1 must read as 8.
#[test]
fn a_full_enhancement_layer_rpu_converts_to_profile_8() {
    let orig = rpu_fixture_nal(include_bytes!("../../../tests/fixtures/dovi-p7-fel-rpu.bin"));
    let parse = |nal: &[u8]| dolby_vision::rpu::dovi_rpu::DoviRpu::parse_unspec62_nalu(nal).unwrap();
    assert_eq!(parse(&orig).dovi_profile, 7);
    let out = rewrite_dv_rpu_p7_to_p81(&orig).expect("a FEL RPU converts");
    assert_eq!(&out[..2], &[0x7C, 0x01], "still an UNSPEC62 NAL");
    assert_eq!(parse(&out).dovi_profile, 8);
}

/// An RPU that is already single-layer is handed back unchanged, not re-encoded.
#[test]
fn a_profile_8_rpu_is_left_as_it_was() {
    let p8 = rpu_fixture_nal(include_bytes!("../../../tests/fixtures/dovi-p7-mel-to-81-rpu.bin"));
    assert_eq!(rewrite_dv_rpu_p7_to_p81(&p8).as_deref(), Some(&p8[..]));
}

/// An RPU the converter cannot parse is DROPPED, never forwarded as-is: a Profile 8
/// declaration followed by a Profile 7 RPU is worse than a frame with no RPU at all.
#[test]
fn an_unconvertible_rpu_is_dropped_on_an_armed_feed() {
    let bl = [0x02u8, 0x01, 0xAA];
    let bad_rpu = [0x7Cu8, 0x01, 0x00, 0x00, 0x00];
    let buf = avcc(&[&bl, &bad_rpu]);
    let (_, out) = to_annexb_p7(&buf, &[]);
    assert_eq!(out, vec![0, 0, 0, 1, 0x02, 0x01, 0xAA]);
}

/// Unarmed (every stream that is not a converted Profile 7) nothing changes: enhancement-layer
/// NALs and RPUs pass through exactly as they always did.
#[test]
fn an_unarmed_feed_is_untouched_by_the_conversion() {
    let el_wrapped = [0x7Eu8, 0x01, 0xBB];
    let el_layer1 = [0x02u8, 0x09, 0xBB];
    let rpu = [0x7Cu8, 0x01, 0x19, 0x08];
    let buf = avcc(&[&el_wrapped, &el_layer1, &rpu]);
    let (_, out) = to_annexb(&buf, true, &[]);
    assert_eq!(
        out,
        vec![
            0, 0, 0, 1, 0x7E, 0x01, 0xBB, 0, 0, 0, 1, 0x02, 0x09, 0xBB, 0, 0, 0, 1, 0x7C, 0x01,
            0x19, 0x08
        ]
    );
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

// -- VP9: pass-through -----------------------------------------------------------------

#[test]
fn a_vp9_packet_is_handed_over_byte_for_byte_and_the_key_flag_survives() {
    // A keyframe's first byte: frame marker 0b10, profile 0, show_existing_frame 0, frame_type 0.
    let key = [0x82, 0x49, 0x83, 0x42, 0x00, 0x01];
    let mut out = vec![0xFF; 3]; // stale bytes from the previous packet must not leak through
    assert!(vp9_packet_to_frame(&key, true, &mut out));
    assert_eq!(out, key);
    let inter = [0x86, 0x00, 0x40];
    assert!(!vp9_packet_to_frame(&inter, false, &mut out));
    assert_eq!(out, inter);
}

#[test]
fn a_vp9_superframe_is_not_split_or_trimmed() {
    // Two frames under a superframe index: marker 0b110 | (size_bytes-1)<<3 | (frames-1) = 0xc1,
    // sizes 3 and 2 as one byte each, then the same marker byte again.
    let sf = [0x82, 0x00, 0x01, 0x86, 0x02, 0xc1, 0x03, 0x02, 0xc1];
    let mut out = Vec::new();
    assert!(!vp9_packet_to_frame(&sf, false, &mut out));
    assert_eq!(out, sf);
}

#[test]
fn an_empty_vp9_packet_stays_empty() {
    let mut out = vec![1, 2, 3];
    assert!(!vp9_packet_to_frame(&[], false, &mut out));
    assert!(out.is_empty());
}

// -- video framing dispatch -------------------------------------------------------------

#[test]
fn every_fed_codec_has_a_framing_and_nothing_else_does() {
    assert_eq!(VideoFraming::of_codec_id(AV_CODEC_ID_H264), Some(VideoFraming::AnnexB { hevc: false }));
    assert_eq!(VideoFraming::of_codec_id(AV_CODEC_ID_HEVC), Some(VideoFraming::AnnexB { hevc: true }));
    assert_eq!(VideoFraming::of_codec_id(AV_CODEC_ID_AV1), Some(VideoFraming::Av1));
    assert_eq!(VideoFraming::of_codec_id(AV_CODEC_ID_VP9), Some(VideoFraming::Vp9));
    // any other id (0 = none, 139 = VP8, 2 = MPEG-2, 0x15002 = AAC) is refused, never Annex-B
    for id in [0, 2, 139, 1, 0x15002, -1] {
        assert_eq!(VideoFraming::of_codec_id(id), None, "{id}");
    }
}

/// The demuxer's framing and the Load's declaration are one table: every framing maps to the
/// `LoadVideo` whose diag code the agreement check compares, and that is the same enum the Load is
/// built from, so the two cannot be edited apart.
#[test]
fn a_framing_names_the_load_declaration_it_goes_with() {
    use crate::player::engine::LoadVideo;
    for (id, lv) in [
        (AV_CODEC_ID_H264, LoadVideo::H264),
        (AV_CODEC_ID_HEVC, LoadVideo::H265),
        (AV_CODEC_ID_AV1, LoadVideo::Av1),
        (AV_CODEC_ID_VP9, LoadVideo::Vp9),
    ] {
        let f = VideoFraming::of_codec_id(id).unwrap();
        assert_eq!(f.load_video(), lv, "{id}");
        // and the route's own spelling of that codec round-trips to the same declaration
        let spelling = match lv {
            LoadVideo::H264 => "h264",
            LoadVideo::H265 => "hevc",
            LoadVideo::Av1 => "av1",
            LoadVideo::Vp9 => "vp9",
        };
        assert_eq!(LoadVideo::of_stream(spelling), Some(lv));
    }
}

/// An HEVC prefix-SEI NAL (type 39) holding `messages`, each `(payloadType, payload)`.
fn sei_nal(messages: &[(u8, &[u8])]) -> Vec<u8> {
    let mut nal = vec![39 << 1, 1];
    for &(ptype, payload) in messages {
        nal.push(ptype);
        nal.push(payload.len() as u8);
        nal.extend_from_slice(payload);
    }
    nal.push(0x80);
    nal
}

const HDR10PLUS: &[u8] = &[0xb5, 0x00, 0x3c, 0x00, 0x01, 0x04, 0x01];

fn is_hdr10plus_sei(nal: &[u8]) -> bool {
    strip_hdr10plus_sei(nal) != SeiStrip::Keep
}

#[test]
fn hdr10plus_is_found_alone_and_behind_another_message() {
    assert!(is_hdr10plus_sei(&sei_nal(&[(4, HDR10PLUS)])));
    // A mastering-display message first: only the first message used to be read.
    assert!(is_hdr10plus_sei(&sei_nal(&[(137, &[0u8; 24]), (4, HDR10PLUS)])));
}

#[test]
fn static_hdr_metadata_and_other_t35_messages_are_kept() {
    let both = sei_nal(&[(137, &[0u8; 24]), (144, &[0u8; 4])]);
    assert_eq!(strip_hdr10plus_sei(&both), SeiStrip::Keep);
    // Dolby Vision RPU-adjacent or other T.35 payloads from another provider stay.
    let other = sei_nal(&[(4, &[0xb5, 0x00, 0x31, 0x44])]);
    assert_eq!(strip_hdr10plus_sei(&other), SeiStrip::Keep);
    // A non-SEI NAL (IDR slice) is never touched.
    assert_eq!(strip_hdr10plus_sei(&[19 << 1, 1, 0xb5, 0x00, 0x3c]), SeiStrip::Keep);
}

#[test]
fn static_hdr_metadata_sharing_a_nal_with_hdr10plus_survives() {
    let mastering = [0x11u8; 24];
    let nal = sei_nal(&[(137, &mastering), (4, HDR10PLUS)]);
    assert_eq!(strip_hdr10plus_sei(&nal), SeiStrip::Rewrite(sei_nal(&[(137, &mastering)])));
    // HDR10+ first, static metadata after, and a suffix SEI keeps its NAL type.
    let mut nal = sei_nal(&[(4, HDR10PLUS), (144, &[1, 2, 3, 4])]);
    nal[0] = 40 << 1;
    let mut want = sei_nal(&[(144, &[1, 2, 3, 4])]);
    want[0] = 40 << 1;
    assert_eq!(strip_hdr10plus_sei(&nal), SeiStrip::Rewrite(want));
}

#[test]
fn an_sei_of_only_hdr10plus_is_dropped() {
    assert_eq!(strip_hdr10plus_sei(&sei_nal(&[(4, HDR10PLUS)])), SeiStrip::Drop);
    assert_eq!(strip_hdr10plus_sei(&sei_nal(&[(4, HDR10PLUS), (4, HDR10PLUS)])), SeiStrip::Drop);
}

#[test]
fn a_kept_message_gets_its_emulation_prevention_back() {
    let payload = [0, 0, 0, 7, 0, 0, 1, 9, 0, 0, 2, 0, 0, 4];
    let nal = sei_nal(&[(5, &payload), (4, HDR10PLUS)]);
    let SeiStrip::Rewrite(out) = strip_hdr10plus_sei(&nal) else {
        panic!("expected a rewrite");
    };
    let mut want = vec![39 << 1, 1, 5, payload.len() as u8];
    want.extend_from_slice(&[0, 0, 3, 0, 7, 0, 0, 3, 1, 9, 0, 0, 3, 2, 0, 0, 4]);
    want.push(0x80);
    assert_eq!(out, want);
    // and it unescapes back to the original message
    assert_eq!(sei_rbsp(&out[2..]), [&[5u8, payload.len() as u8][..], &payload, &[0x80]].concat());
}

#[test]
fn sei_sizes_count_bytes_after_emulation_prevention_is_removed() {
    // A first payload of 00 00 00 is carried as 00 00 03 00 in the NAL; reading the size against
    // the raw bytes would land one byte short of the HDR10+ message.
    let mut nal = vec![39 << 1, 1, 5, 3, 0, 0, 3, 0];
    nal.extend_from_slice(&[4, HDR10PLUS.len() as u8]);
    nal.extend_from_slice(HDR10PLUS);
    nal.push(0x80);
    assert!(is_hdr10plus_sei(&nal));
    // the kept 00 00 00 payload is escaped again exactly as it came
    assert_eq!(
        strip_hdr10plus_sei(&nal),
        SeiStrip::Rewrite(vec![39 << 1, 1, 5, 3, 0, 0, 3, 0, 0x80])
    );
}

#[test]
fn a_truncated_sei_never_panics_and_only_a_seen_header_drops_it() {
    let full = sei_nal(&[(137, &[0u8; 24]), (4, HDR10PLUS)]);
    // First byte of the HDR10+ payload: 2 header bytes + (type, size, 24) + (type, size).
    let plus_payload = 2 + 2 + 24 + 2;
    for n in 0..full.len() {
        let got = strip_hdr10plus_sei(&full[..n]);
        if n < plus_payload + 3 {
            // the T.35 header is not all there: never HDR10+, left as received
            assert_eq!(got, SeiStrip::Keep, "prefix {n}");
        } else {
            // header seen but the list is cut: dropped whole, never rewritten
            assert_eq!(got, SeiStrip::Drop, "prefix {n}");
        }
    }
    // oversized payloadSize running past the end
    assert_eq!(strip_hdr10plus_sei(&[39 << 1, 1, 4, 200, 0xb5, 0x00, 0x3c]), SeiStrip::Drop);
    assert_eq!(strip_hdr10plus_sei(&[39 << 1, 1, 4, 200, 0xb5]), SeiStrip::Keep);
}
