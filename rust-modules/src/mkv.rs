//! Streaming Matroska/EBML demuxer -> H264 Annex-B AUs + raw audio frames pushed to
//! the au_queue (was src/mkv.c); parses SeekHead/Cues for the seek index. The player
//! fills an `MkvCtx` and calls mkv_run/mkv_seek_run/mkv_parse_cues; ebml_id/ebml_size
//! are also used by the cue preflight. Bytes come from the read callback. Entry points
//! catch_unwind so a parse bug is a clean failure, never a panic across a callback.
use crate::aq::{aq_is_aborted, aq_push, AuQueue};
use std::os::raw::{c_int, c_long, c_uint, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub(crate) type MkvByteReader = Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>;
pub(crate) type MkvCueCb = Option<extern "C" fn(*mut c_void, i64, i64)>;

// Layout MUST match `mkv_ctx` in src/mkv.h. Fields pub(crate) so the Rust player
// engine (the old playback.c consumer) can configure the ctx + read its outputs.
#[repr(C)]
pub struct MkvCtx {
    pub(crate) read: MkvByteReader,
    pub(crate) ud: *mut c_void,
    pub(crate) pos: i64,
    pub(crate) eof: c_int,
    pub(crate) tscale: i64,
    pub(crate) duration_ns: i64,
    pub(crate) segment_pos: i64,
    pub(crate) cues_pos: i64,
    pub(crate) header_only: c_int,
    pub(crate) cue_cb: MkvCueCb,
    pub(crate) cue_ud: *mut c_void,
    pub(crate) vtrack: c_int,
    pub(crate) is_h264: c_int,
    pub(crate) nal_len_size: c_int,
    pub(crate) sps_pps: [u8; 2048],
    pub(crate) sps_pps_len: c_int,
    pub(crate) atrack: c_int,
    pub(crate) has_audio: c_int,
    pub(crate) acodec: [u8; 8],
    pub(crate) audio_frame_ns: i64,
    pub(crate) q: *mut AuQueue,
    pub(crate) scratch: *mut u8,
    pub(crate) scratch_cap: c_int,
    pub(crate) naus: c_long,
    pub(crate) nkey: c_long,
    pub(crate) naus_a: c_long,
    pub(crate) debug: c_int,
    pub(crate) laced_seen: c_int,
    // subtitle tracks (text only), recorded in document order for client-side rendering.
    // The active track is chosen by index via crate::player::desired_sub_idx().
    pub(crate) strack_nums: [i64; 16],
    pub(crate) strack_ass: [u8; 16], // 1 = ASS/SSA (strip fields+codes), 0 = SRT/plain UTF-8
    pub(crate) nsub: c_int,
    // HEVC (V_MPEGH/ISO/HEVC) demux alongside H264: is_hevc gates the video branch, and the
    // coded dimensions (from the Video element) feed the Starfish Load payload. Appended at the
    // end so existing field offsets are unchanged.
    pub(crate) is_hevc: c_int,
    pub(crate) vwidth: c_int,
    pub(crate) vheight: c_int,
}

// ---- byte source ----
unsafe fn msrc_read(c: *mut MkvCtx, dst: *mut u8, n: c_int) -> c_int {
    let mut got = 0;
    while got < n {
        let r = match (*c).read {
            Some(f) => f((*c).ud, dst.add(got as usize), n - got),
            None => 0,
        };
        if r <= 0 {
            (*c).eof = 1;
            break;
        }
        got += r;
    }
    (*c).pos += got as i64;
    got
}

unsafe fn msrc_skip(c: *mut MkvCtx, n: i64) -> i64 {
    let mut tmp = [0u8; 8192];
    let mut left = n;
    while left > 0 {
        let chunk = if left > tmp.len() as i64 { tmp.len() as c_int } else { left as c_int };
        let r = msrc_read(c, tmp.as_mut_ptr(), chunk);
        if r <= 0 {
            break;
        }
        left -= r as i64;
    }
    n - left
}

// ---- EBML primitives ----
pub(crate) fn ebml_id(c: *mut MkvCtx, id: *mut c_uint, idlen: *mut c_int) -> c_int {
    unsafe {
        let mut b0 = 0u8;
        if msrc_read(c, &mut b0, 1) != 1 {
            return 0;
        }
        let mut len = 1;
        let mut mask = 0x80u8;
        while b0 & mask == 0 {
            mask >>= 1;
            len += 1;
            if len > 4 {
                return 0;
            }
        }
        let mut v = b0 as c_uint;
        for _ in 1..len {
            let mut b = 0u8;
            if msrc_read(c, &mut b, 1) != 1 {
                return 0;
            }
            v = (v << 8) | b as c_uint;
        }
        if !id.is_null() {
            *id = v;
        }
        if !idlen.is_null() {
            *idlen = len;
        }
        1
    }
}

pub(crate) fn ebml_size(c: *mut MkvCtx, size: *mut i64, szlen: *mut c_int) -> c_int {
    unsafe {
        let mut b0 = 0u8;
        if msrc_read(c, &mut b0, 1) != 1 {
            return 0;
        }
        let mut len = 1;
        let mut mask = 0x80u8;
        while b0 & mask == 0 {
            mask >>= 1;
            len += 1;
            if len > 8 {
                return 0;
            }
        }
        let low = mask - 1;
        let mut v = (b0 & low) as i64;
        let mut all_ones = (b0 & low) == low;
        for _ in 1..len {
            let mut b = 0u8;
            if msrc_read(c, &mut b, 1) != 1 {
                return 0;
            }
            v = (v << 8) | b as i64;
            if b != 0xFF {
                all_ones = false;
            }
        }
        if !size.is_null() {
            *size = if all_ones { -1 } else { v };
        }
        if !szlen.is_null() {
            *szlen = len;
        }
        1
    }
}

unsafe fn ebml_uint(c: *mut MkvCtx, n: i64) -> i64 {
    let mut b = [0u8; 8];
    if n < 1 || n > 8 {
        msrc_skip(c, n);
        return 0;
    }
    if msrc_read(c, b.as_mut_ptr(), n as c_int) != n as c_int {
        return 0;
    }
    let mut v = 0i64;
    for i in 0..n as usize {
        v = (v << 8) | b[i] as i64;
    }
    v
}

unsafe fn ebml_float(c: *mut MkvCtx, n: i64) -> f64 {
    let mut b = [0u8; 8];
    if n != 4 && n != 8 {
        msrc_skip(c, n);
        return 0.0;
    }
    if msrc_read(c, b.as_mut_ptr(), n as c_int) != n as c_int {
        return 0.0;
    }
    if n == 4 {
        let u = ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | b[3] as u32;
        f32::from_bits(u) as f64
    } else {
        let mut u = 0u64;
        for i in 0..8 {
            u = (u << 8) | b[i] as u64;
        }
        f64::from_bits(u)
    }
}

const SPS_CAP: usize = 1024;

// slice over the ctx's sps_pps array without an implicit reference to *c
unsafe fn sps_mut(c: *mut MkvCtx) -> &'static mut [u8] {
    std::slice::from_raw_parts_mut(std::ptr::addr_of_mut!((*c).sps_pps) as *mut u8, SPS_CAP)
}

// ---- avcC -> Annex-B SPS/PPS + NAL length size ----
unsafe fn mkv_parse_avcc(c: *mut MkvCtx, p: &[u8]) {
    let len = p.len();
    if len < 7 || p[0] != 1 {
        return;
    }
    (*c).nal_len_size = (p[4] & 0x03) as c_int + 1;
    let sps = sps_mut(c);
    let cap = SPS_CAP;
    let mut o = 5usize;
    let mut out = 0usize;
    let nsps = (p[o] & 0x1f) as usize;
    o += 1;
    for _ in 0..nsps {
        if o + 2 > len {
            break;
        }
        let l = ((p[o] as usize) << 8) | p[o + 1] as usize;
        o += 2;
        if o + l > len || out + 4 + l > cap {
            break;
        }
        sps[out] = 0; sps[out + 1] = 0; sps[out + 2] = 0; sps[out + 3] = 1;
        out += 4;
        sps[out..out + l].copy_from_slice(&p[o..o + l]);
        out += l;
        o += l;
    }
    if o >= len {
        (*c).sps_pps_len = out as c_int;
        return;
    }
    let npps = p[o] as usize;
    o += 1;
    for _ in 0..npps {
        if o + 2 > len {
            break;
        }
        let l = ((p[o] as usize) << 8) | p[o + 1] as usize;
        o += 2;
        if o + l > len || out + 4 + l > cap {
            break;
        }
        sps[out] = 0; sps[out + 1] = 0; sps[out + 2] = 0; sps[out + 3] = 1;
        out += 4;
        sps[out..out + l].copy_from_slice(&p[o..o + l]);
        out += l;
        o += l;
    }
    (*c).sps_pps_len = out as c_int;
}

// ---- hvcC -> Annex-B VPS/SPS/PPS + NAL length size (HEVCDecoderConfigurationRecord) ----
// ISO/IEC 14496-15 §8.3.3.1: 23-byte fixed prefix (the 6-byte constraint-flags field pushes
// lengthSizeMinusOne to byte 21, unlike avcC where it's byte 4), then numArrays at byte 22,
// then arrays of (NAL-type byte, u16 count, count × (u16 len + NAL)). Keep VPS(32)/SPS(33)/
// PPS(34), skip SEI (39/40).
unsafe fn mkv_parse_hvcc(c: *mut MkvCtx, p: &[u8]) {
    let len = p.len();
    if len < 23 || p[0] != 1 {
        return;
    }
    (*c).nal_len_size = (p[21] & 0x03) as c_int + 1;
    let num_arrays = p[22] as usize;
    let sps = sps_mut(c);
    let cap = SPS_CAP;
    let mut o = 23usize;
    let mut out = 0usize;
    for _ in 0..num_arrays {
        if o + 3 > len {
            break;
        }
        let nal_type = p[o] & 0x3f;
        let num_nalus = ((p[o + 1] as usize) << 8) | p[o + 2] as usize;
        o += 3;
        let keep = matches!(nal_type, 32 | 33 | 34); // VPS / SPS / PPS
        for _ in 0..num_nalus {
            if o + 2 > len {
                return;
            }
            let l = ((p[o] as usize) << 8) | p[o + 1] as usize;
            o += 2;
            if o + l > len {
                return;
            }
            if keep && out + 4 + l <= cap {
                sps[out] = 0; sps[out + 1] = 0; sps[out + 2] = 0; sps[out + 3] = 1;
                out += 4;
                sps[out..out + l].copy_from_slice(&p[o..o + l]);
                out += l;
            }
            o += l;
        }
    }
    (*c).sps_pps_len = out as c_int;
}

// ---- Video element (0xE0): read the coded PixelWidth/PixelHeight for the Load payload ----
unsafe fn mkv_parse_video(c: *mut MkvCtx, size: i64, vw: &mut i32, vh: &mut i32) {
    let mut consumed = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0xB0 {
            *vw = ebml_uint(c, sz) as i32;
        } else if id == 0xBA {
            *vh = ebml_uint(c, sz) as i32;
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
    }
}

// ---- lacing: split a laced block body into frames. lacing 0 none,1 Xiph,2 fixed,3 EBML
fn mkv_unlace(fd: &[u8], lacing: i32, off: &mut [i32], sz: &mut [i32], maxf: usize) -> usize {
    let fl = fd.len() as i32;
    if lacing == 0 {
        if maxf < 1 || fl <= 0 {
            return 0;
        }
        off[0] = 0;
        sz[0] = fl;
        return 1;
    }
    if fl < 1 {
        return 0;
    }
    let mut nf = fd[0] as usize + 1;
    let mut p: i32 = 1;
    if nf > maxf {
        nf = maxf;
    }
    if lacing == 2 {
        if nf == 0 {
            return 0;
        }
        let each = (fl - p) / nf as i32;
        for i in 0..nf {
            off[i] = p + i as i32 * each;
            sz[i] = each;
        }
        return nf;
    }
    if lacing == 1 {
        for i in 0..nf - 1 {
            let mut s = 0i32;
            while p < fl && fd[p as usize] == 0xFF {
                s += 255;
                p += 1;
            }
            if p < fl {
                s += fd[p as usize] as i32;
                p += 1;
            }
            sz[i] = s;
        }
        let mut o = p;
        for i in 0..nf - 1 {
            off[i] = o;
            o += sz[i];
        }
        off[nf - 1] = o;
        sz[nf - 1] = fl - o;
        return nf;
    }
    // lacing == 3 EBML: first = unsigned vint, rest = prev + signed vint delta
    let b0 = fd[p as usize];
    let mut ll = 1i32;
    let mut mk = 0x80u8;
    while b0 & mk == 0 {
        mk >>= 1;
        ll += 1;
        if ll > 8 {
            return 0;
        }
    }
    let mut first = (b0 & (mk - 1)) as i64;
    for k in 1..ll {
        first = (first << 8) | fd[(p + k) as usize] as i64;
    }
    p += ll;
    let mut prev = first;
    sz[0] = first as i32;
    for i in 1..nf - 1 {
        let c0 = fd[p as usize];
        let mut m = 1i32;
        let mut m2 = 0x80u8;
        while c0 & m2 == 0 {
            m2 >>= 1;
            m += 1;
            if m > 8 {
                return 0;
            }
        }
        let mut v = (c0 & (m2 - 1)) as i64;
        for k in 1..m {
            v = (v << 8) | fd[(p + k) as usize] as i64;
        }
        p += m;
        prev += v - ((1i64 << (7 * m - 1)) - 1);
        sz[i] = prev as i32;
    }
    let mut o = p;
    for i in 0..nf - 1 {
        off[i] = o;
        o += sz[i];
    }
    off[nf - 1] = o;
    sz[nf - 1] = fl - o;
    nf
}

// ---- one (Simple)Block -> AU(s) -> queue ----
/// Matroska track number of the currently-selected subtitle (by desired index), or -1
unsafe fn active_sub_track(c: *mut MkvCtx) -> i64 {
    let idx = crate::player::desired_sub_idx();
    if idx < 0 || idx >= (*c).nsub {
        return -1;
    }
    (*c).strack_nums[idx as usize]
}
unsafe fn active_sub_ass(c: *mut MkvCtx) -> bool {
    let idx = crate::player::desired_sub_idx();
    idx >= 0 && idx < (*c).nsub && (*c).strack_ass[idx as usize] != 0
}

/// `bdur` = BlockDuration in tscale units (-1 if none / SimpleBlock).
unsafe fn mkv_handle_block(c: *mut MkvCtx, blk: &[u8], cluster_ts: i64, bdur: i64) {
    let len = blk.len() as i32;
    if len < 4 {
        return;
    }
    // track number: EBML vint (marker stripped)
    let b0 = blk[0];
    let mut tl = 1i32;
    let mut mask = 0x80u8;
    while b0 & mask == 0 {
        mask >>= 1;
        tl += 1;
        if tl > 8 {
            return;
        }
    }
    let mut track = (b0 & (mask - 1)) as i64;
    for i in 1..tl as usize {
        track = (track << 8) | blk[i] as i64;
    }
    let mut p = tl;
    if p + 3 > len {
        return;
    }
    let rel = (((blk[p as usize] as u16) << 8) | blk[(p + 1) as usize] as u16) as i16 as i64;
    p += 2;
    let flags = blk[p as usize];
    p += 1;

    // subtitle track -> a text cue for client-side rendering (direct-play only; a
    // transcoded stream carries no subs). A distinct track from audio/video.
    let sub_track = active_sub_track(c);
    if sub_track >= 0 && track == sub_track {
        let payload = &blk[p as usize..];
        let start = (cluster_ts + rel) * (*c).tscale;
        let end = start + if bdur > 0 { bdur * (*c).tscale } else { 4_000_000_000 };
        crate::player::push_subtitle_cue(start, end, payload, active_sub_ass(c));
        return;
    }

    // audio track: unpack lacing, feed each raw frame (es=2)
    if (*c).has_audio != 0 && track as c_int == (*c).atrack {
        let afd = &blk[p as usize..];
        let mut aoff = [0i32; 128];
        let mut asz = [0i32; 128];
        let nf = mkv_unlace(afd, ((flags >> 1) & 0x03) as i32, &mut aoff, &mut asz, 128);
        let base = (cluster_ts + rel) * (*c).tscale;
        let afl = afd.len() as i32;
        for i in 0..nf {
            if asz[i] <= 0 || aoff[i] + asz[i] > afl {
                continue;
            }
            let apts = base + i as i64 * (*c).audio_frame_ns;
            (*c).naus_a += 1;
            if !(*c).q.is_null() {
                aq_push(
                    (*c).q,
                    afd.as_ptr().add(aoff[i] as usize),
                    asz[i],
                    apts,
                    1,
                    2,
                );
            }
        }
        return;
    }

    if track as c_int != (*c).vtrack || ((*c).is_h264 == 0 && (*c).is_hevc == 0) {
        return;
    }
    if (flags >> 1) & 0x03 != 0 {
        (*c).laced_seen += 1; // skip laced video (rare)
        return;
    }

    let fd = &blk[p as usize..];
    let fl = fd.len() as i32;
    let ns = (*c).nal_len_size;
    let hevc = (*c).is_hevc != 0;
    // pass 1: keyframe? H264 IDR = NAL type 5; HEVC IRAP (BLA/IDR/CRA) = NAL types 16..=23.
    // At a keyframe we prepend the param sets (H264 SPS/PPS or HEVC VPS/SPS/PPS) + mark es=1 key.
    let mut key = 0i32;
    let mut i = 0i32;
    while i + ns <= fl {
        let mut nal_len = 0i64;
        for k in 0..ns {
            nal_len = (nal_len << 8) | fd[(i + k) as usize] as i64;
        }
        i += ns;
        if nal_len <= 0 || i as i64 + nal_len > fl as i64 {
            break;
        }
        let b0 = fd[i as usize];
        let is_key = if hevc { (16..=23).contains(&((b0 >> 1) & 0x3f)) } else { (b0 & 0x1f) == 5 };
        if is_key {
            key = 1;
            break;
        }
        i += nal_len as i32;
    }
    // assemble AU into scratch
    let scap = (*c).scratch_cap;
    let need = fl + (fl / 32 + 4) + (if key != 0 { (*c).sps_pps_len } else { 0 }) + 64;
    if need > scap {
        crate::player::log(&format!("mkv: video AU dropped (need={need} > scratch_cap={scap}) — raise cap"));
        return;
    }
    let scratch = std::slice::from_raw_parts_mut((*c).scratch, scap as usize);
    let mut out = 0usize;
    if key != 0 && (*c).sps_pps_len > 0 {
        let spl = (*c).sps_pps_len as usize;
        let sps = sps_mut(c);
        scratch[..spl].copy_from_slice(&sps[..spl]);
        out = spl;
    }
    let mut i = 0i32;
    while i + ns <= fl {
        let mut nal_len = 0i64;
        for k in 0..ns {
            nal_len = (nal_len << 8) | fd[(i + k) as usize] as i64;
        }
        i += ns;
        if nal_len <= 0 || i as i64 + nal_len > fl as i64 {
            break;
        }
        if out + 4 + nal_len as usize > scap as usize {
            break;
        }
        scratch[out] = 0; scratch[out + 1] = 0; scratch[out + 2] = 0; scratch[out + 3] = 1;
        out += 4;
        scratch[out..out + nal_len as usize].copy_from_slice(&fd[i as usize..i as usize + nal_len as usize]);
        out += nal_len as usize;
        i += nal_len as i32;
    }
    if out == 0 {
        return;
    }
    let pts = (cluster_ts + rel) * (*c).tscale;
    (*c).naus += 1;
    if key != 0 {
        (*c).nkey += 1;
    }
    if !(*c).q.is_null() {
        aq_push((*c).q, (*c).scratch, out as c_int, pts, key, 1); // es=1 video
    }
}

// ---- element tree walk ----
unsafe fn mkv_parse_track_entry(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    let mut tnum = -1i32;
    let mut ttype = -1i32;
    let mut codecid = [0u8; 40];
    let mut cidlen = 0usize;
    let mut cp = [0u8; 1024];
    let mut cplen = 0usize;
    let mut vw = 0i32;
    let mut vh = 0i32;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0xD7 {
            tnum = ebml_uint(c, sz) as i32;
        } else if id == 0x83 {
            ttype = ebml_uint(c, sz) as i32;
        } else if id == 0x86 {
            let n = if sz < 39 { sz as i32 } else { 39 };
            msrc_read(c, codecid.as_mut_ptr(), n);
            cidlen = n as usize;
            if sz > n as i64 {
                msrc_skip(c, sz - n as i64);
            }
        } else if id == 0x63A2 {
            let n = if sz < cp.len() as i64 { sz as i32 } else { cp.len() as i32 };
            msrc_read(c, cp.as_mut_ptr(), n);
            cplen = n as usize;
            if sz > n as i64 {
                msrc_skip(c, sz - n as i64);
            }
        } else if id == 0xE0 {
            mkv_parse_video(c, sz, &mut vw, &mut vh);
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
    }
    let cid = &codecid[..cidlen];
    if ttype == 1 && (*c).vtrack < 0 {
        (*c).vtrack = tnum;
        (*c).vwidth = vw;
        (*c).vheight = vh;
        if cid.starts_with(b"V_MPEG4/ISO/AVC") {
            (*c).is_h264 = 1;
            if cplen > 0 {
                mkv_parse_avcc(c, &cp[..cplen]);
            }
        } else if cid.starts_with(b"V_MPEGH/ISO/HEVC") {
            (*c).is_hevc = 1;
            if cplen > 0 {
                mkv_parse_hvcc(c, &cp[..cplen]);
            }
        }
    } else if ttype == 2 && (*c).atrack < 0 {
        if cid.starts_with(b"A_AC3") {
            set_acodec(c, b"AC3");
            (*c).audio_frame_ns = 32_000_000;
            (*c).has_audio = 1;
        } else if cid.starts_with(b"A_EAC3") {
            set_acodec(c, b"EAC3");
            (*c).audio_frame_ns = 32_000_000;
            (*c).has_audio = 1;
        } else if cid.starts_with(b"A_AAC") {
            set_acodec(c, b"AAC");
            (*c).audio_frame_ns = 21_333_333;
            (*c).has_audio = 1;
        }
        if (*c).has_audio != 0 {
            (*c).atrack = tnum;
        }
    } else if ttype == 17 && (*c).nsub < 16 {
        // subtitle track — record TEXT subs (SRT/ASS) in document order for
        // client-side rendering; image subs (PGS/VOBSUB) are skipped.
        let is_srt = cid.starts_with(b"S_TEXT/UTF8") || cid.starts_with(b"S_TEXT/ASCII");
        let is_ass = cid.starts_with(b"S_TEXT/ASS") || cid.starts_with(b"S_TEXT/SSA");
        if is_srt || is_ass {
            let i = (*c).nsub as usize;
            (*c).strack_nums[i] = tnum as i64;
            (*c).strack_ass[i] = is_ass as u8;
            (*c).nsub += 1;
        }
    }
}

unsafe fn set_acodec(c: *mut MkvCtx, s: &[u8]) {
    let ac = std::slice::from_raw_parts_mut(std::ptr::addr_of_mut!((*c).acodec) as *mut u8, 8);
    for x in ac.iter_mut() {
        *x = 0;
    }
    let n = s.len().min(7);
    ac[..n].copy_from_slice(&s[..n]);
}

unsafe fn read_block(c: *mut MkvCtx, sz: i64, cluster_ts: i64) {
    if sz >= 0 && sz <= (*c).scratch_cap as i64 {
        let blk = libc::malloc(sz as usize) as *mut u8;
        if !blk.is_null() {
            msrc_read(c, blk, sz as c_int);
            mkv_handle_block(c, std::slice::from_raw_parts(blk, sz as usize), cluster_ts, -1);
            libc::free(blk as *mut c_void);
        } else {
            msrc_skip(c, sz);
        }
    } else if sz >= 0 {
        msrc_skip(c, sz);
    }
}

unsafe fn mkv_parse_cluster(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    let mut cluster_ts = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0xE7 {
            cluster_ts = ebml_uint(c, sz);
        } else if id == 0xA3 {
            read_block(c, sz, cluster_ts); // SimpleBlock
        } else if id == 0xA0 {
            // BlockGroup -> Block (+ optional BlockDuration for a subtitle cue's end).
            // Buffer the Block and read BlockDuration (either order), then handle once.
            let mut bc = 0i64;
            let mut bdur = -1i64;
            let mut bptr: *mut u8 = std::ptr::null_mut();
            let mut blen = 0i64;
            while sz < 0 || bc < sz {
                let mut bid = 0u32;
                let mut bil = 0i32;
                let mut bsz = 0i64;
                let mut bsl = 0i32;
                if ebml_id(c, &mut bid, &mut bil) == 0 {
                    break;
                }
                if ebml_size(c, &mut bsz, &mut bsl) == 0 {
                    break;
                }
                bc += (bil + bsl) as i64;
                if bid == 0xA1 && bptr.is_null() && bsz >= 0 && bsz <= (*c).scratch_cap as i64 {
                    bptr = libc::malloc(bsz as usize) as *mut u8;
                    if !bptr.is_null() {
                        msrc_read(c, bptr, bsz as c_int);
                        blen = bsz;
                    } else {
                        msrc_skip(c, bsz);
                    }
                } else if bid == 0x9B && bsz >= 0 {
                    bdur = ebml_uint(c, bsz);
                } else if bsz >= 0 {
                    msrc_skip(c, bsz);
                } else {
                    break;
                }
                if bsz >= 0 {
                    bc += bsz;
                }
            }
            if !bptr.is_null() {
                mkv_handle_block(c, std::slice::from_raw_parts(bptr, blen as usize), cluster_ts, bdur);
                libc::free(bptr as *mut c_void);
            }
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
        if aq_is_aborted((*c).q) {
            break;
        }
    }
}

unsafe fn mkv_parse_info(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0x2AD7B1 {
            (*c).tscale = ebml_uint(c, sz);
        } else if id == 0x4489 {
            let d = ebml_float(c, sz);
            let ts = if (*c).tscale > 0 { (*c).tscale } else { 1_000_000 };
            (*c).duration_ns = (d * ts as f64) as i64;
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
    }
}

unsafe fn mkv_parse_tracks(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0xAE {
            mkv_parse_track_entry(c, sz);
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
    }
}

unsafe fn mkv_parse_seekhead(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0x4DBB {
            // Seek
            let mut tgt = 0u32;
            let mut pos = -1i64;
            let mut sc = 0i64;
            while sz < 0 || sc < sz {
                let mut i2 = 0u32;
                let mut il2 = 0i32;
                let mut s2 = 0i64;
                let mut sl2 = 0i32;
                if ebml_id(c, &mut i2, &mut il2) == 0 {
                    break;
                }
                if ebml_size(c, &mut s2, &mut sl2) == 0 {
                    break;
                }
                sc += (il2 + sl2) as i64;
                if i2 == 0x53AB {
                    let mut b = [0u8; 4];
                    let n = if s2 < 4 { s2 as i32 } else { 4 };
                    msrc_read(c, b.as_mut_ptr(), n);
                    if s2 > n as i64 {
                        msrc_skip(c, s2 - n as i64);
                    }
                    for k in 0..n as usize {
                        tgt = (tgt << 8) | b[k] as u32;
                    }
                } else if i2 == 0x53AC {
                    pos = ebml_uint(c, s2);
                } else if s2 >= 0 {
                    msrc_skip(c, s2);
                } else {
                    break;
                }
                if s2 >= 0 {
                    sc += s2;
                }
            }
            if tgt == 0x1C53BB6B && pos >= 0 {
                (*c).cues_pos = pos;
            }
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
    }
}

fn mkv_parse_cues_inner(c: *mut MkvCtx, size: i64) {
    unsafe {
        let mut consumed = 0i64;
        while size < 0 || consumed < size {
            let mut id = 0u32;
            let mut il = 0i32;
            let mut sz = 0i64;
            let mut sl = 0i32;
            if ebml_id(c, &mut id, &mut il) == 0 {
                break;
            }
            if ebml_size(c, &mut sz, &mut sl) == 0 {
                break;
            }
            consumed += (il + sl) as i64;
            if id == 0xBB {
                // CuePoint
                let mut ctime = -1i64;
                let mut cbyte = -1i64;
                let mut sc = 0i64;
                while sz < 0 || sc < sz {
                    let mut i2 = 0u32;
                    let mut il2 = 0i32;
                    let mut s2 = 0i64;
                    let mut sl2 = 0i32;
                    if ebml_id(c, &mut i2, &mut il2) == 0 {
                        break;
                    }
                    if ebml_size(c, &mut s2, &mut sl2) == 0 {
                        break;
                    }
                    sc += (il2 + sl2) as i64;
                    if i2 == 0xB3 {
                        ctime = ebml_uint(c, s2);
                    } else if i2 == 0xB7 {
                        let mut tc = 0i64;
                        while s2 < 0 || tc < s2 {
                            let mut i3 = 0u32;
                            let mut il3 = 0i32;
                            let mut s3 = 0i64;
                            let mut sl3 = 0i32;
                            if ebml_id(c, &mut i3, &mut il3) == 0 {
                                break;
                            }
                            if ebml_size(c, &mut s3, &mut sl3) == 0 {
                                break;
                            }
                            tc += (il3 + sl3) as i64;
                            if i3 == 0xF1 {
                                cbyte = ebml_uint(c, s3);
                            } else if s3 >= 0 {
                                msrc_skip(c, s3);
                            } else {
                                break;
                            }
                            if s3 >= 0 {
                                tc += s3;
                            }
                        }
                    } else if s2 >= 0 {
                        msrc_skip(c, s2);
                    } else {
                        break;
                    }
                    if s2 >= 0 {
                        sc += s2;
                    }
                }
                if ctime >= 0 && cbyte >= 0 {
                    if let Some(cb) = (*c).cue_cb {
                        cb((*c).cue_ud, ctime, cbyte);
                    }
                }
            } else if sz >= 0 {
                msrc_skip(c, sz);
            } else {
                break;
            }
            if sz >= 0 {
                consumed += sz;
            }
        }
    }
}

pub(crate) fn mkv_parse_cues(c: *mut MkvCtx, size: i64) {
    if c.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| mkv_parse_cues_inner(c, size)));
}

unsafe fn mkv_parse_segment(c: *mut MkvCtx, size: i64) {
    let mut consumed = 0i64;
    while size < 0 || consumed < size {
        let mut id = 0u32;
        let mut il = 0i32;
        let mut sz = 0i64;
        let mut sl = 0i32;
        if ebml_id(c, &mut id, &mut il) == 0 {
            break;
        }
        if ebml_size(c, &mut sz, &mut sl) == 0 {
            break;
        }
        consumed += (il + sl) as i64;
        if id == 0x1549A966 {
            mkv_parse_info(c, sz);
        } else if id == 0x1654AE6B {
            mkv_parse_tracks(c, sz);
        } else if id == 0x114D9B74 {
            mkv_parse_seekhead(c, sz);
        } else if id == 0x1F43B675 {
            if (*c).header_only != 0 {
                return; // Cue preflight stops at the first Cluster
            }
            mkv_parse_cluster(c, sz);
        } else if sz >= 0 {
            msrc_skip(c, sz);
        } else {
            break;
        }
        if sz >= 0 {
            consumed += sz;
        }
        if aq_is_aborted((*c).q) {
            break;
        }
    }
}

pub(crate) fn mkv_seek_run(c: *mut MkvCtx) {
    if c.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        const CID: [u8; 4] = [0x1F, 0x43, 0xB6, 0x75];
        let mut matched = 0usize;
        let mut b = 0u8;
        while (*c).eof == 0 {
            if msrc_read(c, &mut b, 1) != 1 {
                return;
            }
            if b == CID[matched] {
                matched += 1;
                if matched == 4 {
                    let mut sz = 0i64;
                    let mut sl = 0i32;
                    if ebml_size(c, &mut sz, &mut sl) == 0 {
                        return;
                    }
                    mkv_parse_cluster(c, sz);
                    mkv_parse_segment(c, -1); // remaining clusters to EOF
                    return;
                }
            } else {
                matched = if b == CID[0] { 1 } else { 0 };
            }
        }
    }));
}

pub(crate) fn mkv_run(c: *mut MkvCtx) -> c_int {
    if c.is_null() {
        return -1;
    }
    let r = catch_unwind(AssertUnwindSafe(|| unsafe {
        if (*c).tscale <= 0 {
            (*c).tscale = 1_000_000;
        }
        if (*c).audio_frame_ns <= 0 {
            (*c).audio_frame_ns = 32_000_000;
        }
        (*c).vtrack = -1;
        (*c).atrack = -1;
        let mut id = 0u32;
        let mut il = 0i32;
        while ebml_id(c, &mut id, &mut il) != 0 {
            let mut sz = 0i64;
            let mut sl = 0i32;
            if ebml_size(c, &mut sz, &mut sl) == 0 {
                break;
            }
            if id == 0x18538067 {
                (*c).segment_pos = (*c).pos;
                mkv_parse_segment(c, sz);
                if (*c).header_only != 0 {
                    break;
                }
            } else if sz >= 0 {
                msrc_skip(c, sz);
            } else {
                break;
            }
            if (*c).eof != 0 {
                break;
            }
            if aq_is_aborted((*c).q) {
                break;
            }
        }
        if (*c).naus > 0 {
            0
        } else {
            -1
        }
    }));
    r.unwrap_or(-1)
}
