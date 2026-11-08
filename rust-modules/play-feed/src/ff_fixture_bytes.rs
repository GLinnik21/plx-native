//! Owned wire fixtures for packet framing tests.
use super::*;

pub fn avcc(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        v.extend_from_slice(&(n.len() as u32).to_be_bytes());
        v.extend_from_slice(n);
    }
    v
}

pub fn to_annexb(buf: &[u8], is_hevc: bool, param: &[u8]) -> (bool, Vec<u8>) {
    let (key, out) = to_annexb_class(buf, is_hevc, param);
    (key.is_key(), out)
}

/// [`to_annexb`] with the kind of keyframe the packet is ([`Keyframe`]), not just whether it is one.
pub fn to_annexb_class(buf: &[u8], is_hevc: bool, param: &[u8]) -> (Keyframe, Vec<u8>) {
    let mut out = Vec::new();
    let key =
        unsafe { packet_to_annexb(buf.as_ptr(), buf.len(), 4, is_hevc, false, false, param, &mut out) };
    (key, out)
}

/// [`to_annexb`] for an H.264 packet the container flags as a keyframe (`AV_PKT_FLAG_KEY`).
pub fn to_annexb_flagged(buf: &[u8], param: &[u8]) -> (bool, Vec<u8>) {
    let mut out = Vec::new();
    let key = unsafe { packet_to_annexb(buf.as_ptr(), buf.len(), 4, false, false, true, param, &mut out) };
    (key.is_key(), out)
}

/// [`to_annexb`] for an HEVC stream with the Dolby Vision Profile 7 -> 8.1 conversion armed.
pub fn to_annexb_p7(buf: &[u8], param: &[u8]) -> (bool, Vec<u8>) {
    let mut out = Vec::new();
    let key = unsafe { packet_to_annexb(buf.as_ptr(), buf.len(), 4, true, true, false, param, &mut out) };
    (key.is_key(), out)
}

/// One of `tests/fixtures/dovi-p7-*-rpu.bin` (an RPU as `dovi_tool` writes it: a 4-byte start
/// code, then the payload after the `7C 01` NAL header) as the `UNSPEC62` NAL a demuxed packet
/// carries.
pub fn rpu_fixture_nal(file: &[u8]) -> Vec<u8> {
    assert_eq!(&file[..4], &[0, 0, 0, 1], "a dovi_tool RPU file starts with a start code");
    let mut nal = vec![0x7C, 0x01];
    nal.extend_from_slice(&file[4..]);
    nal
}

// -- parse_dovi_conf: the Dolby Vision configuration record ---------------------------
//
// The record is nine plain bytes, so these fixtures ARE the wire format — no builder, no
// FFmpeg. That is the point: `dovi_conf`'s pointer walk cannot be host-tested (dlopen returns
// None on Darwin, so a test naming it would pass without executing one line of it), but the
// parse is where a field could be transposed, and the parse is pure.

/// The nine bytes as the header lays them out, so a fixture reads like the spec table.
pub fn dovi_bytes(profile: u8, level: u8, rpu: u8, el: u8, bl: u8, compat: u8) -> Vec<u8> {
    vec![1, 0, profile, level, rpu, el, bl, compat, 0]
}
