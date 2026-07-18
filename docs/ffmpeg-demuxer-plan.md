# libavformat demuxer plan — replacing `mkv.rs`

**Status: EXECUTED (all phases A–E landed; 2026-07-18).** `ff.rs` is the only demuxer;
`mkv.rs`, the `cues_thread`/Cue-index apparatus, the second HTTP connection (`hs2`), and the
`/tmp/plxnative-demux=mkv` bisect trigger are all deleted. Two knowing deviations from the spec
below: the direct-play seek evolved past §3.2 into a Kodi-style **in-place** seek (flush + reopen
+ `av_seek_frame`, then `setTimeToDecode`+`sendSegmentEvent` on the first post-seek keyframe —
see `pump.rs`/`feed_stream`), and the feed is **two-lane** (separate video/audio `AuQueue`s, not
the single queue assumed here). This file is kept as the design record/ABI reference.

**Mandate:** quality + stability, not a throwaway prototype.
**Target:** LG webOS 4.5, 32-bit ARM, FFmpeg **3.3** ABI (SONAMEs `libavformat.so.57` /
`libavcodec.so.57` / `libavutil.so.55` = versions 57.71.100 / 57.89.100 / 55.58.100). The boot
smoke test in `ff.rs::smoke()` already logs `avformat=57.71.100`, proving the stub-`.so` link
resolves to the device's real libraries at runtime.

> **Version note:** the SONAMEs are FFmpeg **n3.3** ("Hilbert"), not n3.4. Every struct and
> codec-ID we touch is byte-identical between 3.3 and 3.4 (diffed), so the ABI below is valid
> either way. Where a field/enum depends on a compile-time `FF_API_*` flag it is called out and
> gated behind the on-device `offsetof` probe (Phase A). Do **not** skip Phase A.

The demuxer's entire external contract is narrow: everything downstream flows through **one
function** — `aq_push(q, data, len, pts, key, es)` — plus three published scalars
(`SHARED.duration_ns`, `SHARED.file_size`, and today `SHARED.segment_pos`/cues, both deleted here).
All the rich `MkvCtx` fields are read *only inside* `mkv.rs`; a replacement need not reproduce them.

---

## 0. The contract a new demuxer must satisfy (the invariant)

Push, into the single `AuQueue`:

- **Video AUs** (`es=1`): H264/HEVC **Annex-B** — every NAL prefixed with a `00 00 00 01` start
  code; **SPS/PPS (H264) or VPS/SPS/PPS (HEVC) prepended at every keyframe**; `key=1` iff the AU
  is an IDR (H264 NAL type 5) / IRAP (HEVC NAL types 16–23). One `aq_push` = one coded picture.
- **Audio AUs** (`es=2`, `key=1` always): **raw** elementary-stream frames — AC3/EAC3 syncframes,
  **raw AAC (no ADTS)** — bytes verbatim from `av_read_frame`, no container framing, no BSF.
- **PTS in nanoseconds of content time**: `pts_ns = av_rescale_q(pkt.pts, stream.time_base,
  {1, 1_000_000_000})`, falling back to `pkt.dts` when `pkt.pts == AV_NOPTS_VALUE`. Never feed dts.
- Honor the **6 MiB `aq_push` backpressure** (it blocks the producer; a libavformat read-loop
  calling `aq_push` inherits this pacing for free).
- Publish `SHARED.duration_ns` (from `AVFormatContext.duration`, µs → ns) and `SHARED.file_size`.
- Expose a **time-based seek** (`av_seek_frame`, BACKWARD) that replaces the byte-Range reopen +
  the MKV Cue index (both deletable).

`es ∈ {1,2}` only — there is no `es=3`. Text subtitles do **not** enter the queue; if we later
demux direct-play soft subs they go straight to `crate::player::push_subtitle_text(...)`.

**Why the FFmpeg BSFs satisfy the video contract exactly:** `h264_mp4toannexb` /
`hevc_mp4toannexb` do (i) length-prefix → start-code conversion and (ii) insert the
`extradata`-derived parameter sets ahead of every IDR/IRAP — precisely what `mkv_handle_block`
hand-rolls (`mkv.rs:557-581`). Two harmless, non-semantic deltas: the BSF writes a 3-byte
`00 00 01` for interior NALs (4-byte for the first NAL + param-set blob), and it suppresses a
param-set prepend only when the AU already carries them in-band (never the case for Plex
CodecPrivate MKVs). The Starfish decoder scans for `00 00 01` and accepts both; duplicate param
sets are harmless. **Audio needs no BSF** — matroska's `av_read_frame` returns de-laced raw frames
already; do **not** apply `aac_adtstoasc`.

---

## 1. The FFI module (`ff.rs`)

`ff.rs` currently holds only the version smoke test. Extend it with the structs, externs, and
constants below. All structs are `#[repr(C)]`. **Bold NEEDED** fields are load-bearing (we
dereference them); the rest exist to make `sizeof`/offsets correct.

### 1.1 Opaque handles (pointer-only, never dereferenced)
```rust
pub enum AVClass {}        pub enum AVInputFormat {}   pub enum AVOutputFormat {}
pub enum AVIOContext {}    pub enum AVBufferRef {}     pub enum AVCodecContext {}
pub enum AVDictionary {}   pub enum AVCodec {}         pub enum AVBitStreamFilter {}
pub enum AVBSFInternal {}  pub enum AVStreamInternal {} pub enum AVCodecParserContext {}
```

### 1.2 Structs (exact n3.3 field order; sizes are 32-bit ARM/AAPCS)

```rust
#[repr(C)] #[derive(Clone, Copy)]
pub struct AVRational { pub num: c_int, pub den: c_int }

#[repr(C)]
pub struct AVPacketSideData { pub data: *mut u8, pub size: c_int, pub type_: c_int }

// sizeof = 72. Idiomatic path is av_packet_alloc, but the size is public ABI.
#[repr(C)]
pub struct AVPacket {
    pub buf: *mut AVBufferRef,            // +0
    pub pts: i64,                         // +8   stream.time_base units, or AV_NOPTS_VALUE
    pub dts: i64,                         // +16
    pub data: *mut u8,                    // +24  ** NEEDED (payload) **
    pub size: c_int,                      // +28  ** NEEDED **
    pub stream_index: c_int,              // +32  ** NEEDED (route) **
    pub flags: c_int,                     // +36  ** NEEDED (AV_PKT_FLAG_KEY) **
    pub side_data: *mut AVPacketSideData, // +40
    pub side_data_elems: c_int,           // +44
    pub duration: i64,                    // +48
    pub pos: i64,                         // +56
    pub convergence_duration: i64,        // +64  (FF_API_CONVERGENCE_DURATION, avcodec<59)
}

// sizeof = 136
#[repr(C)]
pub struct AVCodecParameters {
    pub codec_type: c_int,               // +0   ** NEEDED **
    pub codec_id: c_int,                 // +4   ** NEEDED **
    pub codec_tag: u32,                  // +8
    pub extradata: *mut u8,              // +12  ** NEEDED (AVCC/HVCC → BSF) **
    pub extradata_size: c_int,           // +16  ** NEEDED **
    pub format: c_int,                   // +20
    pub bit_rate: i64,                   // +24
    pub bits_per_coded_sample: c_int,    // +32
    pub bits_per_raw_sample: c_int,      // +36
    pub profile: c_int,                  // +40
    pub level: c_int,                    // +44
    pub width: c_int,                    // +48  ** NEEDED (log/verify; optional payload dims) **
    pub height: c_int,                   // +52  ** NEEDED **
    pub sample_aspect_ratio: AVRational, // +56
    pub field_order: c_int,              // +64
    pub color_range: c_int,              // +68
    pub color_primaries: c_int,          // +72
    pub color_trc: c_int,                // +76
    pub color_space: c_int,              // +80
    pub chroma_location: c_int,          // +84
    pub video_delay: c_int,              // +88
    pub channel_layout: u64,             // +96
    pub channels: c_int,                 // +104
    pub sample_rate: c_int,              // +108 ** NEEDED (log) **
    pub block_align: c_int,              // +112
    pub frame_size: c_int,               // +116
    pub initial_padding: c_int,          // +120
    pub trailing_padding: c_int,         // +124
    pub seek_preroll: c_int,             // +128
}

#[repr(C)] pub struct AVFrac { pub val: i64, pub num: i64, pub den: i64 } // FF_API_LAVF_FRAC
#[repr(C)] pub struct AVProbeData {
    pub filename: *const c_char, pub buf: *mut u8, pub buf_size: c_int, pub mime_type: *const c_char,
}
```

`AVStream` **must** be reproduced in full because `codecpar` is the **last field** (offset **+708**)
after a large "internal but ABI" block — the single riskiest struct. Copy it verbatim from the
research reference (fields `index`+0 … `time_base`+40 … `codecpar`+708, `sizeof=712`), including
`codec` (`FF_API_LAVF_AVCTX`), `pts: AVFrac` (`FF_API_LAVF_FRAC`), the `attached_pic: AVPacket`
by value at +104, `pts_buffer:[i64;17]`, `pts_reorder_error:[i64;17]`,
`pts_reorder_error_count:[u8;17]` (note the re-alignment: the `[u8;17]` at +648 ends at +665, the
following `i64 last_dts_for_order_check` re-aligns to +672). **NEEDED** reads:
`index`(+0), `time_base`(+40 → PTS conversion), `codecpar`(+708).

`AVFormatContext` is **truncated after the head** — we only ever hold a library-returned pointer and
read leading fields. **Never stack-allocate it.**
```rust
#[repr(C)]
pub struct AVFormatContext {
    pub av_class: *const AVClass,   // +0
    pub iformat: *mut AVInputFormat,// +4
    pub oformat: *mut AVOutputFormat,//+8
    pub priv_data: *mut c_void,     // +12
    pub pb: *mut AVIOContext,       // +16  ** NEEDED (set for custom AVIO) **
    pub ctx_flags: c_int,           // +20
    pub nb_streams: c_uint,         // +24  ** NEEDED **
    pub streams: *mut *mut AVStream,// +28  ** NEEDED (streams[i]) **
    pub filename: [c_char; 1024],   // +32  (char[1024] in 3.3/3.4, NOT char* url)
    pub start_time: i64,            // +1056
    pub duration: i64,              // +1064 ** NEEDED (AV_TIME_BASE units → duration_ns) **
    // ... ~60 more fields omitted. Do NOT construct this yourself.
}
```
> We do **not** reach `AVFormatContext.flags` / `.interrupt_callback` (deep in the tail, offset
> unverified). The design avoids needing them: custom-AVIO teardown/seek interruption is handled
> inside our own `read_packet` callback (§3), not via `AVFMT_FLAG_CUSTOM_IO` / `AVIOInterruptCB`.
> If a demuxer requires `AVFMT_FLAG_CUSTOM_IO` set (it does not for the pre-alloc'd-`pb` path,
> §3.1), extend the struct to `flags` only after verifying its offset on-device.

```rust
#[repr(C)]
pub struct AVBSFContext {
    pub av_class: *const AVClass,         // +0
    pub filter: *const AVBitStreamFilter, // +4
    pub internal: *mut AVBSFInternal,     // +8
    pub priv_data: *mut c_void,           // +12
    pub par_in: *mut AVCodecParameters,   // +16  ** NEEDED (fill before init) **
    pub par_out: *mut AVCodecParameters,  // +20
    pub time_base_in: AVRational,         // +24  ** NEEDED (set before init) **
    pub time_base_out: AVRational,        // +32
}
```

### 1.3 externs
```rust
#[link(name = "avformat")] extern "C" {
    fn av_register_all();                                    // REQUIRED in 3.x, once, before open
    fn avformat_network_init() -> c_int;                    // only if libav* does the HTTP (Option B)
    fn avformat_alloc_context() -> *mut AVFormatContext;    // needed for the custom-AVIO path
    fn avformat_open_input(ps: *mut *mut AVFormatContext, url: *const c_char,
                           fmt: *mut AVInputFormat, options: *mut *mut AVDictionary) -> c_int;
    fn avformat_find_stream_info(ic: *mut AVFormatContext, options: *mut *mut AVDictionary) -> c_int;
    fn av_find_best_stream(ic: *mut AVFormatContext, type_: c_int, wanted: c_int, related: c_int,
                           decoder_ret: *mut *mut AVCodec, flags: c_int) -> c_int;
    fn av_read_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
    fn av_seek_frame(s: *mut AVFormatContext, stream_index: c_int, ts: i64, flags: c_int) -> c_int;
    fn avformat_close_input(s: *mut *mut AVFormatContext);
    // custom AVIO (avio.h)
    fn avio_alloc_context(buffer: *mut u8, buffer_size: c_int, write_flag: c_int, opaque: *mut c_void,
        read_packet: Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        write_packet: Option<extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int>,
        seek: Option<extern "C" fn(*mut c_void, i64, c_int) -> i64>) -> *mut AVIOContext;
}
#[link(name = "avcodec")] extern "C" {
    fn av_packet_alloc() -> *mut AVPacket;
    fn av_packet_free(pkt: *mut *mut AVPacket);
    fn av_packet_unref(pkt: *mut AVPacket);
    fn avcodec_parameters_copy(dst: *mut AVCodecParameters, src: *const AVCodecParameters) -> c_int;
    fn av_bsf_get_by_name(name: *const c_char) -> *const AVBitStreamFilter;
    fn av_bsf_alloc(f: *const AVBitStreamFilter, ctx: *mut *mut AVBSFContext) -> c_int;
    fn av_bsf_init(ctx: *mut AVBSFContext) -> c_int;
    fn av_bsf_send_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_receive_packet(ctx: *mut AVBSFContext, pkt: *mut AVPacket) -> c_int;
    fn av_bsf_free(ctx: *mut *mut AVBSFContext);
}
#[link(name = "avutil")] extern "C" {
    fn av_malloc(size: usize) -> *mut c_void;               // AVIO buffer alloc
    fn av_freep(ptr: *mut c_void);                          // frees *ptr, sets it NULL
    fn av_rescale_q(a: i64, bq: AVRational, cq: AVRational) -> i64; // PTS → ns
}
```
Every FFmpeg symbol added here **must** be appended to the matching `stub/*.c` (an empty
`void avio_alloc_context(void){}` body suffices — only the *name* matters; the real symbol loads on
the TV via the SONAME). Missing a stub symbol = link failure on the host.

ABI notes: every `enum` argument/field is a plain 32-bit `int`. `av_read_frame` returns 0 / `<0`
(`AVERROR_EOF` at end). A returned packet **owns a ref** — release with `av_packet_unref` before the
next read. `av_bsf_send_packet` **takes ownership of and resets** its packet.

### 1.4 constants
```rust
pub const AVMEDIA_TYPE_VIDEO: c_int = 0;
pub const AVMEDIA_TYPE_AUDIO: c_int = 1;
pub const AV_CODEC_ID_H264: c_int = 28;   // FF_API_XVMC present (avutil<56); 4.x = 27
pub const AV_CODEC_ID_HEVC: c_int = 174;  // 4.x = 173
pub const AV_CODEC_ID_AAC:  c_int = 0x15002;
pub const AV_CODEC_ID_AC3:  c_int = 0x15003;
pub const AV_CODEC_ID_EAC3: c_int = 0x15029;
pub const AV_PKT_FLAG_KEY: c_int = 0x0001;
pub const AVSEEK_FLAG_BACKWARD: c_int = 1;
pub const AVERROR_EOF: c_int = -541478725;   // FOURCC-derived, arch-independent
pub const AVERROR_EAGAIN: c_int = -11;       // AVERROR(EAGAIN), Linux/glibc
pub const AV_NOPTS_VALUE: i64 = i64::MIN;
pub const AV_TIME_BASE: i64 = 1_000_000;     // AVFormatContext.duration unit
pub const AVSEEK_SIZE: c_int = 0x10000;      // seek_cb whence: return stream size
pub const NS_TB: AVRational = AVRational { num: 1, den: 1_000_000_000 };
```

**MUST verify on-device (Phase A):** log `codec_id`, `width`, `height`, `sample_rate` for a known
HEVC title (expect `codec_id=174`, `width=3840`, `height=1920`). If `codec_id` reads `173`, the
device libavcodec was built with `FF_API_XVMC` disabled — regenerate the video codec IDs (and only
those; the `0x15xxx` audio IDs are immune). Also run the `offsetof` probe (§8).

---

## 2. The demuxer flow (`demux()` in the read thread)

```
av_register_all()                                   // once, process-global (guard with Once)
alloc AVIOContext over stream.rs  (§3.1)            // custom I/O: read_cb + seek_cb over http_*
fmt = avformat_alloc_context();  (*fmt).pb = avio
avformat_open_input(&fmt, NULL, NULL, NULL)         // url NULL: pb already set → uses our I/O
avformat_find_stream_info(fmt, NULL)                // fills codecpar/extradata/durations/index
v = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0)
a = pick_audio(fmt)                                 // av_find_best_stream, or route-driven (§4)
publish: SHARED.duration_ns = (*fmt).duration * 1000   // µs → ns  (replaces stream_read side-publish)
         SHARED.file_size    = content_length from the AVIO opaque
// set up the video BSF once:
name = if codec_id==HEVC {"hevc_mp4toannexb"} else {"h264_mp4toannexb"}
bsf  = av_bsf_alloc(av_bsf_get_by_name(name))
avcodec_parameters_copy((*bsf).par_in, (*streams[v]).codecpar)   // hands it the AVCC/HVCC extradata
(*bsf).time_base_in = (*streams[v]).time_base
av_bsf_init(bsf)                                    // par_out now Annex-B; filter built param sets

pkt = av_packet_alloc(); out = av_packet_alloc()
loop {
    // seek check happens via read_cb interrupt (§3) — on interrupt av_read_frame<0, disambiguate
    r = av_read_frame(fmt, pkt)
    if r < 0 {
        if pending_seek { do_seek(fmt, v, bsf); continue }   // §3
        if aborted      { break }
        break                                                // real EOF
    }
    let si = (*pkt).stream_index
    if si == v {
        av_bsf_send_packet(bsf, pkt)                         // takes+resets pkt
        loop {
            let rr = av_bsf_receive_packet(bsf, out)
            if rr == AVERROR_EAGAIN || rr == AVERROR_EOF { break }
            if rr < 0 { break }
            let key = if (*out).flags & AV_PKT_FLAG_KEY != 0 { 1 } else { 0 }
            let pts = pts_ns(out, streams[v])                // §2.1
            aq_push(q, (*out).data, (*out).size, pts, key, 1)   // es=1, blocks on 6MiB cap
            av_packet_unref(out)
        }
    } else if si == a {
        let pts = pts_ns(pkt, streams[a])
        aq_push(q, (*pkt).data, (*pkt).size, pts, 1, 2)      // es=2 raw, key=1
        av_packet_unref(pkt)
    } else {
        av_packet_unref(pkt)
    }
    if aq_is_aborted(q) { break }
}
aq_set_eof(q)
// cleanup §5
```

### 2.1 PTS conversion
```rust
unsafe fn pts_ns(pkt: *const AVPacket, st: *const AVStream) -> i64 {
    let t = if (*pkt).pts != AV_NOPTS_VALUE { (*pkt).pts } else { (*pkt).dts };
    if t == AV_NOPTS_VALUE { return 0; }
    av_rescale_q(t, (*st).time_base, NS_TB)
}
```
matroska sets `time_base = {TimecodeScale, 1e9}`, so this yields the **same** value as
`mkv.rs`'s `(cluster_ts+rel)*tscale`. The `feed_stream` rebase (`pts_shift`) is downstream and
unchanged — it only requires **monotonic content-time-ns PTS + an accurate `key` flag**.

### 2.2 audio-stream selection
`av_find_best_stream(fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0)` is correct for the common single-
track case. For a Plex direct-play file with multiple audio tracks where the user selected a
specific one, prefer routing: match `(*streams[i]).codecpar.codec_id` (and, if needed, the track's
metadata language) against `route::stream_acodec()` / the selected `audioStreamID`. For a transcode
stream there is exactly one audio track, so `av_find_best_stream` suffices. Start with
`av_find_best_stream`; add route-matching only if multi-track direct-play needs it.

---

## 3. Seeking — `av_seek_frame` replaces the byte-Range reopen

### 3.1 Custom AVIO over `stream.rs` (Option A — recommended)

Keep the proven raw-socket transport (numeric IP, no DNS, `Connection: close`, 15s `SO_RCVTIMEO`)
and give libavformat **seekability** so `av_seek_frame` and the matroska demuxer's own index work.

```rust
struct AvioState {
    hs: *mut HttpStream,          // the Engine-owned demux socket box (via raw ptr, like today)
    host: CString, port: c_int, path: CString,
    off: i64,                     // current absolute byte offset (advanced by read_cb)
    size: i64,                    // content_length of the first full-file open (for AVSEEK_SIZE)
}
extern "C" fn read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    let s = &mut *(op as *mut AvioState);
    // interrupt point: bail out of a blocked av_read_frame for seek OR teardown
    if aq_is_aborted(s.q) || SHARED.seek_to_ns.load(Relaxed) >= 0 { return AVERROR_EOF; }
    let r = http_read(s.hs, dst, n);
    if r <= 0 { return AVERROR_EOF; }      // EOF or unblocked-by-close
    s.off += r as i64; r
}
extern "C" fn seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let s = &mut *(op as *mut AvioState);
    if whence == AVSEEK_SIZE { return s.size; }
    let target = match whence { SEEK_SET=>offset, SEEK_CUR=>s.off+offset, SEEK_END=>s.size+offset, _=>return -1 };
    http_close(s.hs);                                            // reopen the socket with a byte Range
    let range = CString::new(format!("Range: bytes={target}-\r\n")).unwrap();
    if http_open(s.hs, s.host.as_ptr(), s.port, s.path.as_ptr(), range.as_ptr(), "GET") != 0 { return -1; }
    s.off = target; target
}
let buf = av_malloc(65536) as *mut u8;      // AVIO owns this buffer
let avio = avio_alloc_context(buf, 65536, 0, &mut state, Some(read_cb), None, Some(seek_cb));
```
With `seek_cb` non-NULL, `avformat_find_stream_info` reads the MKV `SeekHead`/`Cues` itself and
`av_seek_frame` seeks by byte through `seek_cb` — **the entire `cues_thread` apparatus is replaced.**

### 3.2 The seek path (direct-play)

The pump publishes a **target ns**; the demux thread performs the actual `av_seek_frame` (FFmpeg
requires the seek and `av_read_frame` on the same thread). Sequence:

- **Pump (main thread)**, on `TX.seek_to_ns >= 0` (direct-play branch):
  `sf_flush → sf_set_playtime(0) → sf_play`; `drain_aq(eng)` (frees the queue and cond-signals
  `not_full`, so any `aq_push` blocked on the 6 MiB cap wakes and returns); store the ns target to
  `SHARED.seek_to_ns` (a new **i64 ns** atomic, replacing the byte-valued `seek_byte` for
  direct-play); `http_close(hs)` to unblock a socket blocked in `recv`; set `eng.rebase_pending =
  true`, `eng.max_fed_pts = 0`, `SHARED.playpos_ns = t`.
- **Demux thread**: `read_cb` returns `AVERROR_EOF` (it saw `seek_to_ns >= 0`), so `av_read_frame`
  returns `<0`; the loop consults the atomics, sees a pending seek, and calls:
  ```rust
  let ns = SHARED.seek_to_ns.swap(-1, Acquire);
  let ts = av_rescale_q(ns, NS_TB, (*streams[v]).time_base);   // ns → video time_base
  av_seek_frame(fmt, v, ts, AVSEEK_FLAG_BACKWARD);             // lands on/before nearest keyframe
  // no av_bsf_flush in 3.3/3.4 — the H264/HEVC filters self-recover at the first post-seek
  // IDR/IRAP (they buffer nothing across packets). For a hard reset: av_bsf_free + realloc/init.
  ```
  `av_seek_frame` calls `seek_cb`, which reopens the socket at the index-derived byte offset. The
  read loop resumes; the first packet delivered is a keyframe at/before the target.

### 3.3 Why this fixes the HEVC seek-resync corruption

The hand-rolled path (`mkv_seek_run`) reopens the HTTP stream at a **CBR-estimated (or coarse Cue)
byte**, then **byte-scans for the next Cluster ID** (`1F 43 B6 75`) and resyncs there. Two failure
modes it had: (a) the Cluster it lands on is not guaranteed to start with the codec's parameter sets
in decode order, and for HEVC an IRAP without a fresh VPS/SPS/PPS in front produces a corrupt
decode; (b) the byte estimate can land mid-Cluster and the scan can desync on payload bytes that
alias the Cluster ID. `av_seek_frame(BACKWARD)` instead uses libavformat's **exact frame index**
(built from the container's Cues/SeekHead during `find_stream_info`), landing on a real keyframe
packet; the `hevc_mp4toannexb` BSF then **prepends VPS/SPS/PPS to that IRAP** on the very first
post-seek packet — so the decoder always receives a self-contained keyframe. No byte-scan, no
guessing, no missing param sets. The `feed_stream` rebase (first `es==1 && key!=0` AU →
`pts_shift = -pts`) latches on that clean keyframe exactly as before.

### 3.4 `rebase` / `disp_base` interplay (unchanged)

- **Direct-play seek:** file PTS *is* content time, `disp_base = 0`. After `av_seek_frame` the first
  fed keyframe's PTS ≈ target; `feed_stream` sets `pts_shift = -pts` so the fed clock restarts near
  0 against the flush-reset Starfish clock; displayed position = `fed - pts_shift` tracks content.
  Identical to today's direct-play semantics — only the *mechanism* that produced the keyframe
  changed (index seek vs byte-scan).
- **Transcode seek / audio-switch / retranscode:** logically unchanged. These are a **brand-new
  stream** (`start.mkv?...&offset=SECS`, 0-based PTS), so they stay a **close + reopen** of the
  whole `AVFormatContext` on the `next_url` — not an `av_seek_frame`. `disp_base = SECS*1e9` still
  supplies the content offset the transcode PTS loses. The demux thread's outer loop handles this
  exactly as it handles `next_url` today (§4).

---

## 4. Integration — file-by-file changes

### `rust-modules/src/ff.rs`
- Add everything in §1. Keep `smoke()`. Add the `demux()` entry the thread calls, plus the AVIO
  glue, BSF setup, and cleanup. This module becomes the demuxer (its doc-comment already says so).

### `rust-modules/src/player/threads.rs`
- **`stream_thread` (60-144):** replace the **entire body** with the §2 flow. Keep the signature
  `(host, port, path, aq: SendPtr<AuQueue>, hs: SendPtr<HttpStream>)` and the trailing
  `aq_set_eof(aq_p)`. The 12 MiB `scratch` malloc (73-76) is **gone** (the BSF owns its buffers).
  The `MkvCtx` box is gone. The outer `loop` **stays** — it still handles the transcode
  `next_url` reopen (close `AVFormatContext`, reopen on the new URL) and `reparse_next`. The
  direct-play byte-`Range` reopen arm (116-136) is replaced by the in-loop `av_seek_frame` (§3.2);
  the `next_url` arm remains.
- **`StreamRead`/`stream_read`/`hs2_read` (21-36):** repurpose — `stream_read`'s job (wrap
  `http_read` + side-publish `duration_ns`) is now split: `read_cb` (§3.1) wraps `http_read`; the
  `duration_ns` publish moves to right after `avformat_find_stream_info`. Delete `hs2_read`.
- **`cues_thread` (148-214) + `CueSink` (40-53) + `cue_cb`:** **DELETE entirely.** `av_seek_frame`
  + libavformat's own index replace the manual time→byte Cue index. This also removes the second
  HTTP connection (`hs2`).
- `timeline_thread` / `subs_thread` / `timeline_path` / `push_vtt_cue`: **unchanged.**

### `rust-modules/src/player/engine.rs`
- **`start_bufferfeed` spawn block (218-249):** remove the `cues_th` spawn (238-243) and the `hs2`
  box (211, 225, 228) + `Engine.hs2` field. Keep `hs` (the demux socket, now wrapped by AVIO).
  **Payload/codec logic (189-207) unchanged** — see below.
- **`cue_byte_for` (391-410):** **DELETE** (only caller is `pump.rs:96`).
- **`feed_stream` (431-483):** **NO CHANGE.** It is demuxer-agnostic; the PTS-timeline contract
  (rebase on first post-seek keyframe, `pts_shift`, stale-drop past 2 s) lives here and is
  satisfied by monotonic content-ns PTS + accurate `key`.
- `drain_aq`, `Engine`/`Source`, `stop_bufferfeed`: unchanged except dropping the `hs2`/`cues_th`
  fields and their join (338-339) — `cues_th` join goes away with the thread.

**Load payload — no change required.** `start_bufferfeed` builds the payload from **Plex metadata**
(`route::stream_vcodec()`/`stream_acodec()`, `engine.rs:189-207`) *before* demux starts, and the
payload dims are only the **sink envelope** — the pipeline reads true decode dims from the in-band
SPS/VPS (`engine.rs:113-114`). So the demuxer feeds nothing to the payload. **Optional
improvement** (not required): source codec + dims from `codecpar` (`codec_id → "H264"/"H265"`,
`codec_id → "AC3"/"EAC3"/"AAC"`, `width/height`) to drop the `route::stream_vcodec/acodec`
dependency — but that requires moving payload construction *after* `avformat_find_stream_info`
(open the input on the main thread, or defer `sf_load`). Defer this to a later cleanup.

### `rust-modules/src/player/pump.rs`
- **Direct-play seek branch (93-101):** replace `cue_byte_for(t)` / the CBR byte estimate /
  `SHARED.seek_byte.store(byte)` with `SHARED.seek_to_ns.store(t, Release)` (§3.2). Keep the
  `http_close(hs)` interrupt (102-104) — it now unblocks `read_cb` to bail out of `av_read_frame`.
  `rebase_pending`/`max_fed_pts`/`playpos_ns`/`frames` handling (108-111) unchanged.
- **Transcode seek arm (77-92)** and **retranscode/audio-switch arm (28-55):** unchanged — they
  publish `next_url` + `seek_byte=0` + close the demux; the demux thread's outer loop reopens the
  `AVFormatContext`. (`seek_byte` stays as the transcode "reopen from byte 0" trigger; only the
  direct-play *value* semantics move to the new `seek_to_ns` atomic.)
- ACB-bind state machine (118-154) and feed dispatch (159-165): unchanged.

### `rust-modules/src/player/shared.rs`
- **Add** `seek_to_ns: AtomicI64` (init `-1`; direct-play demux seek target in ns) to `Shared`,
  its `new()`, and `reset_session()`.
- **Delete** (dead once cues are gone): `cues`, `cues_ready`, `cues_abort`, `segment_pos`,
  `hs2_ptr`, and `CueEnt`. Update `reset_session()` accordingly. `next_url`, `reparse_next`,
  `disp_base`, `file_size`, `duration_ns`, `hs_ptr` **stay**. `seek_byte` **stays** (transcode
  reopen trigger).

### `rust-modules/src/aq.rs`, `stream.rs`
- **`aq.rs`: NO CHANGE** — the queue is the stable seam.
- **`stream.rs`: NO CHANGE** — `http_open/read/close` are reused by the AVIO callbacks and by all
  non-demux code (`pms.rs`, `posters.rs`, `route.rs`, timeline/subs threads). Only the *demux
  input* now flows through `read_cb`/`seek_cb`.

### `cues_thread` still needed? — **No.** libavformat builds its own frame index from the
container's Cues/SeekHead during `avformat_find_stream_info`; `av_seek_frame` uses it. Delete the
thread, its socket (`hs2`), and the `CueEnt` table.

---

## 5. Resource lifecycle, error handling, thread model

**Thread model (unchanged shape):**
- **Demux thread** (`stream_thread`) exclusively owns the `AVFormatContext`, `AVBSFContext`, the two
  `AVPacket`s, and the `AVIOContext`. All `av_*` demux/seek calls happen on it. It never touches
  `Engine` fields (confinement preserved).
- **Main/pump thread** publishes seek intent via atomics (`SHARED.seek_to_ns`, `next_url`,
  `seek_byte`) and interrupts via `http_close(hs)`. It never calls any `av_*` function.
- **Load thread / timeline thread / subs thread:** unchanged.
- Cross-thread comms stay atomics/`Mutex` in `SHARED`; `HttpStream` box lives in `Engine`, outlives
  the threads (Engine drops after join), reachable by the pump via `SHARED.hs_ptr` for interrupts —
  exactly as today.

**Cleanup order (demux thread exit — no leaks, no UB):**
```
av_bsf_free(&mut bsf);                 // frees par_in/par_out + internal
av_packet_free(&mut pkt);              // each was av_packet_unref'd after use
av_packet_free(&mut out);
avformat_close_input(&mut fmt);        // frees streams/codecpar/index; does NOT free our AVIO buffer
av_freep(&mut avio.buffer_field);      // our AVIO: free the av_malloc'd buffer, then the context
av_freep(&mut avio);                   // (3.3/3.4 has no avio_context_free; free manually)
aq_set_eof(q);
```
> Because we set `(*fmt).pb` ourselves on a pre-alloc'd context, `avformat_close_input` will **not**
> free our custom `AVIOContext` — we own it and free it manually (`av_freep` the buffer then the
> context). Do **not** double-free: `avformat_close_input` frees the internal-I/O `pb` only when it
> allocated it. (If a future refactor lets `avformat_open_input` open the URL itself — Option B —
> then `avformat_close_input` owns `pb`; don't free it yourself.)

**Error handling (every call checked):**
- `avformat_open_input < 0` → log, `aq_set_eof`, exit thread cleanly (matches today's `http_open`
  failure degradation — never a panic).
- `avformat_find_stream_info < 0`, `av_find_best_stream < 0` (no video) → log, teardown, exit.
- `av_bsf_get_by_name == NULL` (filter not compiled in) → log and fail; verify presence in Phase A
  (`strings libavcodec.so.57 | grep mp4toannexb`).
- `av_read_frame`: `AVERROR_EOF` → normal end; other `<0` → disambiguate seek vs abort vs error.
- `av_bsf_receive_packet`: drain until `AVERROR_EAGAIN`/`AVERROR_EOF`; other `<0` → log, skip.
- All `CString::new(...).unwrap_or_default()` for URL parts (interior-NUL degradation), matching
  the current thread's no-panic policy.
- Teardown races: `aq_abort` + `http_close(hs)` make `read_cb` return `AVERROR_EOF` promptly, so
  `av_read_frame` returns and the thread reaches its cleanup even mid-network-read.

---

## 6. What to DELETE, and when

- **`rust-modules/src/mkv.rs` (entire file, 1125 lines):** delete in **Phase E**, only after Phases
  B–D prove the libavformat path decodes, plays audio, and seeks on-device. Remove `mod mkv;` from
  `lib.rs`, the `use crate::mkv::MkvCtx` in `threads.rs`, and any `mkv_*` references.
- **`cues_thread` + `CueSink` + `cue_cb`** (threads.rs), **`cue_byte_for`** (engine.rs),
  **`CueEnt` + cues/cues_ready/cues_abort/segment_pos + hs2_ptr** (shared.rs), **`hs2`** box/field
  (engine.rs): delete in **Phase D** alongside the seek switch (they only served the old Cue-index
  seek).
- Keep: `aq.rs`, `stream.rs`, `webvtt.rs`, everything in `route.rs`/`pms.rs`, the whole ACB/Starfish
  path, `subs_thread`, `timeline_thread`.

---

## 7. Phased, on-device-verifiable plan

Each phase deploys with `make test` and inspects `/tmp/plxnative-events.log` (fetched automatically).
Use a known **HEVC 3840×1920** title and a known **H264+AC3** title.

- **Phase A — ABI verification (no behavior change).** Add the §1 structs/externs/consts to `ff.rs`
  behind a `ff_probe()` called from boot. It opens the demo URL via custom AVIO,
  `avformat_find_stream_info`, and **logs** `nb_streams`, and for each stream
  `codec_type/codec_id/width/height/sample_rate/time_base`. Cross-check with the `offsetof` C probe
  (§8) run on-device. **Check:** HEVC title logs `codec_id=174, 3840x1920`; H264 logs
  `codec_id=28`; `av_bsf_get_by_name` returns non-NULL for both mp4toannexb filters. If `codec_id`
  or offsets differ, fix the structs before proceeding. *No `mkv.rs` change yet.*
- **Phase B — video read-loop → Annex-B feed → decode.** Wire the §2 flow behind a
  `/tmp/plxnative-demux=ff` trigger (fall back to `mkv.rs` otherwise) so the two paths are bisectable.
  Video only (route audio to `aq` off / feed `es=1` only). **Check:** video plane shows the movie
  (`tools/capture-screen.sh out.png DISPLAY`); log shows `feed v#… reply=O`; `RECEIVE_GOOD_VIDEO`
  behavior matches the mkv path. Verify HEVC decodes (the 3840×1920 title) and H264 decodes.
- **Phase C — audio.** Add the `es=2` raw-frame branch. **Check:** audio plays in sync; no
  `SOUND_ERROR_019` (audio must never reach ACB — it doesn't, it goes to `aq`/Starfish); AC3, EAC3,
  and AAC titles all play (AAC fed raw, no ADTS).
- **Phase D — seek.** Add `SHARED.seek_to_ns`, the pump direct-play switch (§3.2), the demux
  `av_seek_frame`, and the AVIO `seek_cb`. Delete `cues_thread`/`cue_byte_for`/`hs2`/cue state.
  **Check:** LEFT/RIGHT scrub-seek on the **HEVC** title lands cleanly with **no corruption/green
  blocks** (the bug this fixes) — capture the frame after a seek; `/tmp/plxnative-autoseek` for headless.
  Verify direct-play *and* transcode seek, and audio-switch/retranscode still work.
- **Phase E — remove `mkv.rs`.** Delete the file + the `plxnative-demux` bisect trigger; make `ff` the
  only path. **Check:** full interactive pass — shelf → OK → play → pause → seek → BACK — on H264,
  HEVC, and a transcode title; clean teardown (no leak/crash in the log; app-switch reload intact).

---

## 8. Risks & mitigations

1. **ABI offset mismatch (highest).** `AVStream.codecpar` at +708 sits after a large internal
   block; any transcription slip moves it. **Mitigation:** the Phase A `offsetof` C probe compiled
   against the TV's real headers (or `pahole`/`gdb ptype` on the device `.so`). Expected:
   `AVStream.size=712, index=0, time_base=40, codecpar=708`; `AVFormatContext nb_streams=24,
   streams=28, duration=1064`; `AVCodecParameters.size=136, codec_id=4, width=48, channels=104`;
   `AVPacket.size=72`. Any deviation ⇒ device built with different `FF_API_*`/`--disable-*` ⇒
   regenerate structs (add a `#[cfg]`-gated variant) before Phase B.
2. **Codec-ID enum shift.** `H264/HEVC = 28/174` assume `FF_API_XVMC` present (avutil<56). If the
   device build disabled it, they shift to `27/173`. **Mitigation:** Phase A logs `codec_id` for a
   known title; adjust the two video constants if needed (audio `0x15xxx` immune).
3. **BSF not compiled in / not prepending param sets.** If `av_bsf_get_by_name` returns NULL, or a
   keyframe decodes green, the filter/param-set assumption failed. **Mitigation:** `strings
   libavcodec.so.57 | grep mp4toannexb` in Phase A; Phase B/D visually confirm keyframe decode.
   Fallback: hand-roll length→Annex-B + param-set prepend from `codecpar->extradata` (reuse the
   `mkv_parse_avcc`/`mkv_parse_hvcc` logic) — byte-identical to the old path, no BSF dependency.
4. **HTTP via AVIO vs the raw socket.** `seek_cb` reopens the TCP connection with a `Range:` header
   on every seek (and possibly a few times during `find_stream_info` probing) — each is a fresh PMS
   request. Acceptable (seeks are user-initiated, infrequent), but note the reconnect cost.
   `find_stream_info` may read more of the stream than `mkv_run` did (larger startup latency);
   bound it with `probesize`/`analyzeduration` demuxer options via `av_dict_set` if startup is slow.
   Option B (libavformat's own `http` protocol) would give native range seeking but changes network
   semantics (DNS, `AVIOInterruptCB` for interrupt) — defer unless AVIO reconnect cost hurts.
5. **Threading — seek/read on the same thread.** `av_seek_frame` and `av_read_frame` **must** run on
   the demux thread. The pump only publishes atomics + `http_close`; `read_cb` polls the atomics to
   bail out of a blocked `av_read_frame`. **Do not** call any `av_*` from the pump. The
   `AVFormatContext` is single-thread-owned; no locking needed around it.
6. **`AVERROR(EAGAIN) = -11`** assumes Linux/glibc errno (webOS is Linux → correct). Confirm by
   logging the raw `av_bsf_receive_packet` return at end-of-drain in Phase B.
7. **Stub symbols.** Every new FFmpeg extern must be added to the matching `stub/*.c`, or the host
   link fails. Empty bodies suffice (names only). Includes `avio_alloc_context`, `av_malloc`,
   `av_freep`, `av_rescale_q`, `av_bsf_*`, `avformat_alloc_context`.
8. **Double-free of AVIO.** With the pre-alloc'd-`pb` custom-I/O path, we own and free the
   `AVIOContext`; `avformat_close_input` frees the rest. Free the AVIO buffer then the context via
   `av_freep`; never let both paths free `pb`.

### On-device `offsetof` probe (Phase A, run once)
```c
#include <libavformat/avformat.h>
#include <stdio.h>
int main(void){
  printf("AVStream size=%zu index=%zu time_base=%zu codecpar=%zu\n", sizeof(AVStream),
    offsetof(AVStream,index), offsetof(AVStream,time_base), offsetof(AVStream,codecpar));
  printf("AVFmtCtx nb_streams=%zu streams=%zu duration=%zu\n", offsetof(AVFormatContext,nb_streams),
    offsetof(AVFormatContext,streams), offsetof(AVFormatContext,duration));
  printf("AVCodecPar size=%zu codec_id=%zu extradata=%zu width=%zu channels=%zu\n",
    sizeof(AVCodecParameters), offsetof(AVCodecParameters,codec_id),
    offsetof(AVCodecParameters,extradata), offsetof(AVCodecParameters,width),
    offsetof(AVCodecParameters,channels));
  printf("AVPacket size=%zu\n", sizeof(AVPacket)); return 0; }
```
Expected: `AVStream size=712 index=0 time_base=40 codecpar=708`; `AVFmtCtx nb_streams=24 streams=28
duration=1064`; `AVCodecPar size=136 codec_id=4 extradata=12 width=48 channels=104`; `AVPacket
size=72`.
