//! Subtitles through the FFmpeg the app SHIPS — the component list and the ASS ingestion path are
//! the seams under test.
//!
//! The host suite has no FFmpeg to call, so most `ff` tests stay off `av_*`. These cannot: what
//! they grade is a property of `ci/build-ffmpeg.sh`'s configure line (zlib-compressed PGS tracks)
//! or of bytes only the real demuxer produces (an ASS header and packets that arrive NUL-terminated,
//! through both the main subtitle ingestion and the HTTP side reader). So they load the host build of
//! that same FFmpeg (`HOST=1 ci/build-ffmpeg.sh`, staged into `pkg/` by `make check-ffmpeg`), and the
//! ASS ones also need the staged libass; all are `#[ignore]`d everywhere else — an honest "not run" in
//! `make check`, never a pass that executed nothing. `bind_host_ffmpeg` is shared with the `ff` tests
//! that run a real demux (`ff_subside`, `ff_late_audio_tests`).

use super::subs::SubCue;
use super::*;
use crate::player::{ass, ass_source};
use std::sync::Arc;

const TERMINATED_ASS_HEADER: &[u8] =
    include_bytes!("../../../tests/fixtures/ass-terminated-header.bin");

struct AssFixture {
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
    subs: SubTracks,
}

impl AssFixture {
    unsafe fn open() -> Self {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ass-terminated-header.mkv");
        let path = CString::new(path.to_str().unwrap()).unwrap();
        let mut fmt = std::ptr::null_mut();
        assert!(
            avformat_open_input(
                &mut fmt,
                path.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ) >= 0
        );
        let pkt = av_packet_alloc();
        assert!(!pkt.is_null());
        let subs = SubTracks::open(fmt, (*fmt).streams);
        assert_eq!(subs.entries().len(), 2);
        Self { fmt, pkt, subs }
    }

    unsafe fn sources(&self) -> u64 {
        begin_ass_sources(
            "fixture:ass-header",
            self.fmt,
            (*self.fmt).streams,
            self.subs.entries(),
        )
    }

    unsafe fn read(&self, generation: u64) {
        while av_read_frame(self.fmt, self.pkt) >= 0 {
            let si = (*self.pkt).stream_index;
            if let Some(pos) = self.subs.position(si) {
                assert!(
                    !(*self.pkt).data.is_null() && (*self.pkt).size > 0,
                    "a fixture subtitle packet must contain data"
                );
                let raw = std::slice::from_raw_parts((*self.pkt).data, (*self.pkt).size as usize);
                assert_eq!(raw.last() == Some(&0), pos == 0);
                if let Some(cue) =
                    self.subs
                        .decode(pos, self.pkt, *(*self.fmt).streams.add(si as usize))
                {
                    cue.push(generation);
                }
            }
            av_packet_unref(self.pkt);
        }
    }
}

impl Drop for AssFixture {
    fn drop(&mut self) {
        unsafe {
            self.subs = SubTracks::empty();
            av_packet_free(&mut self.pkt);
            avformat_close_input(&mut self.fmt);
        }
    }
}

struct AssReset;
impl Drop for AssReset {
    fn drop(&mut self) {
        ass::clear();
        ass_source::reset();
        crate::player::request_subtitle(-1);
    }
}

fn ass_frame(source: Arc<ass::Source>, now_ms: i64, visible: bool) -> Arc<ass::Frame> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(frame) = ass::request(source.clone(), now_ms, 320, 180, 320, 180) {
            if frame.error.is_some() || !frame.rects.is_empty() == visible {
                return frame;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ASS frame did not arrive at {now_ms} ms"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn assert_ass_caption(source: &Arc<ass::Source>, now_ms: i64) -> Arc<ass::Frame> {
    let frame = ass_frame(source.clone(), now_ms, true);
    assert_eq!(
        frame.error, None,
        "the terminated ASS header and packets must render their captions"
    );
    assert!(
        frame
            .rects
            .iter()
            .any(|r| r.rgba.chunks_exact(4).any(|p| p[3] != 0)),
        "caption pixels are visible"
    );
    frame
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts"]
fn terminated_ass_header_and_packets_render_through_main_subtitle_ingestion() {
    let _g = plx_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    crate::player::request_subtitle(0);
    SHARED.playpos_ns.store(0, Ordering::Relaxed);
    unsafe {
        let d = AssFixture::open();
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs.entries()[0].0 as usize));
        assert_eq!((*cp).extradata_size, 521);
        assert_eq!(
            std::slice::from_raw_parts((*cp).extradata, 521),
            TERMINATED_ASS_HEADER
        );
        let generation = d.sources();
        d.read(generation);
        let terminated = ass_source::selected(0).unwrap();
        let control = ass_source::selected(1).unwrap();
        let frame = assert_ass_caption(&terminated, 2000);
        let control_frame = assert_ass_caption(&control, 2000);
        assert_eq!(
            frame.rects, control_frame.rects,
            "a terminator does not change caption pixels"
        );
        for source in [&terminated, &control] {
            let ass::Content::Embedded { header, events, .. } = &source.content else {
                panic!()
            };
            assert_eq!(header.as_ref(), &TERMINATED_ASS_HEADER[..520]);
            assert_eq!(
                events[0].payload.as_ref(),
                b"0,0,Default,,0,0,0,,FIRST TEST CAPTION"
            );
            assert_eq!(
                events[1].payload.as_ref(),
                b"1,0,Default,,0,0,0,,SECOND TEST CAPTION"
            );
            assert_eq!(
                events
                    .iter()
                    .map(|e| (e.start_ms, e.duration_ms))
                    .collect::<Vec<_>>(),
                [(1730, 2000), (5000, 2000)]
            );
            assert_eq!(ass_frame(source.clone(), 4000, false).error, None);
            assert_ass_caption(source, 6000);
            assert_eq!(ass_frame(source.clone(), 8000, false).error, None);
            assert_ass_caption(source, 2000);
        }
        ass::clear();
        assert_ass_caption(&terminated, 2000);
        let reopened_generation = d.sources();
        let reopened = ass_source::selected(0).unwrap();
        assert_ne!(reopened.id, terminated.id);
        assert_ass_caption(&reopened, 2000);
        ass_source::seek(reopened_generation);
        assert_ass_caption(&ass_source::selected(0).unwrap(), 6000);
    }
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts"]
fn terminated_ass_header_and_packets_render_through_http_side_reader() {
    use super::subside::{Anchor, ReadCfg, RunEnd, SideStop};
    let _g = plx_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    static STOP_SERVER: AtomicBool = AtomicBool::new(false);
    STOP_SERVER.store(false, Ordering::Release);
    let body = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/ass-terminated-header.mkv"),
    )
    .unwrap();
    let (port, server) = super::test_support::serve_file(body, &STOP_SERVER);
    let stop = SideStop::new();
    ass_source::reset();
    crate::player::request_subtitle(0);
    crate::player::set_subtitle_offset(0);
    SHARED.playpos_ns.store(0, Ordering::Relaxed);
    let reader = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let url = format!("http://127.0.0.1:{port}/library/parts/1/ass-header.mkv");
            let cfg = ReadCfg {
                url: &url,
                offset_ns: 0,
                ordinal: 0,
                kbps: 0,
            };
            let anchor = Anchor::of(0, &[0]);
            let mut delta = Some(0);
            super::subside::read_once(&cfg, &stop, &anchor, &mut delta, &|_| 0, &|| 0, &|| (0, 0))
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let source = loop {
        if let Some(source) = ass_source::selected(0) {
            if matches!(&source.content, ass::Content::Embedded { events, .. } if events.len() == 2)
            {
                break Some(source);
            }
        }
        if reader.is_finished() || std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    stop.trigger();
    reader.thread().unpark();
    let end = reader.join();
    STOP_SERVER.store(true, Ordering::Release);
    server.join().unwrap();
    let source = source.expect("the side reader must deliver both ASS packets");
    assert_eq!(end.unwrap(), RunEnd::Stopped);
    assert_ass_caption(&source, 2000);
    assert_eq!(ass_frame(source.clone(), 4000, false).error, None);
    assert_ass_caption(&source, 6000);
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts"]
fn ass_packet_terminators_do_not_weaken_validation() {
    let _g = plx_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    crate::player::request_subtitle(0);
    unsafe {
        let d = AssFixture::open();
        SHARED.playpos_ns.store(0, Ordering::Relaxed);
        for payload in [
            b"0,0,Default,,0,0,0,,bad\0caption\0".as_slice(),
            b"\0caption",
        ] {
            ass_source::reset();
            let generation = d.sources();
            SubCue::Ass {
                track: 0,
                start: 1_000_000_000,
                end: 3_000_000_000,
                payload,
            }
            .push(generation);
            SubCue::Ass {
                track: 0,
                start: 1_000_000_000,
                end: 3_000_000_000,
                payload: b"0,0,Default,,0,0,0,,VALID CAPTION",
            }
            .push(generation);
            let source = ass_source::selected(0).unwrap();
            let ass::Content::Embedded { events, .. } = &source.content else {
                panic!()
            };
            assert_eq!(events.len(), 1, "the malformed packet must be dropped");
            assert_ass_caption(&source, 2000);
        }
        ass_source::reset();
        let generation = d.sources();
        for payload in [b"".as_slice(), b"\0", b"\0\0\0"] {
            SubCue::Ass {
                track: 0,
                start: 1_000_000_000,
                end: 3_000_000_000,
                payload,
            }
            .push(generation);
        }
        let oversized = vec![0; 256 * 1024 + 1];
        SubCue::Ass {
            track: 0,
            start: 1_000_000_000,
            end: 3_000_000_000,
            payload: &oversized,
        }
        .push(generation);
        let source = ass_source::selected(0).unwrap();
        let ass::Content::Embedded { events, .. } = &source.content else {
            panic!()
        };
        assert!(
            events.is_empty(),
            "empty and oversized packets must be dropped"
        );
        SubCue::Ass {
            track: 0,
            start: 1_000_000_000,
            end: 3_000_000_000,
            payload: b"0,0,Default,,0,0,0,,MULTIPLE TERMINATORS\0\0\0",
        }
        .push(generation);
        assert_ass_caption(&ass_source::selected(0).unwrap(), 2000);
    }
}

#[test]
#[ignore = "requires the bundled host FFmpeg, native libass and packaged fonts"]
fn ass_header_terminators_do_not_weaken_validation() {
    let _g = plx_base::testlock::serial();
    let _reset = AssReset;
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    unsafe {
        let d = AssFixture::open();
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs.entries()[0].0 as usize));
        // FFmpeg allocates zeroed AV_INPUT_BUFFER_PADDING_SIZE bytes after extradata.
        (*cp).extradata_size = 523;
        d.sources();
        let source = ass_source::selected(0).unwrap();
        let ass::Content::Embedded { header, .. } = &source.content else {
            panic!()
        };
        assert_eq!(header.as_ref(), &TERMINATED_ASS_HEADER[..520]);
        *(*cp).extradata.add(20) = 0;
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        std::ptr::write_bytes((*cp).extradata, 0, 523);
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        (*cp).extradata_size = 0;
        d.sources();
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        let allocated = (*cp).extradata;
        (*cp).extradata = std::ptr::null_mut();
        (*cp).extradata_size = 521;
        d.sources();
        (*cp).extradata = allocated;
        assert_eq!(
            ass_frame(ass_source::selected(0).unwrap(), 2000, false).error,
            Some(ass::Fault::InvalidTrack)
        );
        (*cp).extradata_size = ass_source::MAX_HEADER_BYTES as i32 + 1;
        d.sources();
        assert!(
            ass_source::selected(0).is_none(),
            "the raw size bound runs before copying or trimming"
        );
    }
}

/// Where the host FFmpeg was staged: `PLX_FFMPEG_DIR` (what `make check-ffmpeg` passes), else the
/// checkout's own `pkg/`.
pub(super) fn host_ffmpeg_dir() -> std::path::PathBuf {
    std::env::var_os("PLX_FFMPEG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pkg"))
}

#[test]
#[ignore = "requires the bundled host FFmpeg; run make check-ffmpeg"]
fn host_ffmpeg_binding_restores_the_abi_gate() {
    let _g = plx_base::testlock::serial();
    assert!(!abi_ok(), "host tests must leave the ABI gate closed");
    {
        let _ffmpeg = bind_host_ffmpeg();
        assert!(abi_ok());
        {
            let _nested = bind_host_ffmpeg();
            assert!(abi_ok());
        }
        assert!(abi_ok(), "an inner binding must preserve the outer binding");
    }
    assert!(!abi_ok(), "the next host test must start with a closed ABI gate");
}

pub(super) struct HostFfmpeg {
    previous_abi_ok: bool,
}

impl Drop for HostFfmpeg {
    fn drop(&mut self) {
        ABI_OK.store(self.previous_abi_ok, Ordering::Relaxed);
    }
}

/// Bind the host libraries in dependency order and check their ABI majors.
/// Keep the guard until all readers stop so the next test gets the previous ABI state.
#[must_use]
pub(super) fn bind_host_ffmpeg() -> HostFfmpeg {
    plx_base::testlock::assert_held("bind_host_ffmpeg");
    let dir = host_ffmpeg_dir();
    for (what, verdict) in [
        ("avutil", avutil::load(Some(&dir))),
        ("avcodec", avcodec::load(Some(&dir))),
        ("avformat", avformat::load(Some(&dir))),
    ] {
        assert!(
            matches!(verdict, plx_base::dynlib::Loaded::Ok(_)),
            "{what}: no host FFmpeg in {} — run `make check-ffmpeg`",
            dir.display()
        );
    }
    let majors = unsafe {
        (
            avformat_version() >> 16,
            avcodec_version() >> 16,
            avutil_version() >> 16,
        )
    };
    assert_eq!(
        majors,
        (63, 63, 61),
        "the staged FFmpeg is not the one ff.rs is built for"
    );
    HostFfmpeg {
        previous_abi_ok: ABI_OK.swap(true, Ordering::Relaxed),
    }
}

/// **A subtitle track muxed by mkvmerge is zlib-compressed, and the bundled FFmpeg must undo it.**
///
/// mkvmerge's default for `S_HDMV/PGS` and `S_VOBSUB` tracks is Matroska ContentCompression with
/// zlib, so that is what nearly every disc remux in the wild carries. libavformat's matroska
/// demuxer only inflates it when FFmpeg was configured WITH zlib; `--disable-autodetect` had
/// quietly dropped it, and without it the demuxer logs "Unsupported encoding type" and hands the
/// decoder the still-compressed bytes (`78 da …`). pgssubdec parses those as segments, finds no
/// display set, and returns got=0 on every packet — playback is perfect and not one `image cue`
/// is ever produced, which is exactly what the television showed on two different libraries.
///
/// The fix is in the demuxer, so it is graded for every consumer of a compressed track, not only
/// the one that was reported. The fixture's five tracks, in file order:
///   0. PGS, compressed as mkvmerge writes it by default;
///   1. the same PGS with `--compression 0:none` — the shape ffmpeg's own muxer produces, and the
///      only one the synthetic harness media ever exercised (the control);
///   2. VobSub transcoded from that PGS, compressed (mkvmerge's default for VobSub too);
///   3. the same VobSub uncompressed (its control);
///   4. an SRT forced to `--compression 0:zlib` — text tracks take the demux loop's payload path,
///      not a decoder, so for them the proof is the packet bytes themselves.
///
/// Regenerate from `tests/fixtures/`:
/// `python3 -c 'import make_fixtures as m, pathlib; m.pgs_build(pathlib.Path("s.sup"), 9)'`
/// (one cue at 0 s, cleared at 8 s); `ffmpeg -f sup -i s.sup -map 0 -c:s dvdsub -s 1920x1080 v.mkv`;
/// an SRT `t.srt` holding one cue `ZLIB TEXT CUE`; then `mkvmerge -o mkv_zlib_subs.mkv --no-date
/// --disable-track-statistics-tags s.sup --compression 0:none s.sup v.mkv --compression 0:none
/// v.mkv --compression 0:zlib t.srt`.
///
/// The image tracks go through the app's own decode path — `open_sub_decoder`, `decode_bitmap_cue`,
/// `rect_to_indexed`, `sub_canvas`, `push_subtitle_bitmap` — keyed by file-order ordinal as the demux
/// loop keys them.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn mkvmerge_zlib_subtitle_tracks_decode_like_uncompressed_ones() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mkv_zlib_subs.mkv")
        .canonicalize()
        .expect("fixture present");
    let url = CString::new(path.to_str().unwrap()).unwrap();

    SHARED.sub_bitmaps.lock().unwrap().clear();
    SHARED.playpos_ns.store(0, Ordering::Relaxed);
    let mut text_payloads: Vec<Vec<u8>> = Vec::new();
    unsafe {
        let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
        assert!(
            avformat_open_input(
                &mut fmt,
                url.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ) >= 0,
            "open the fixture"
        );
        let streams = (*fmt).streams;
        // (stream index, kind, decoder) in FILE order — the demux loop's `sub_streams`
        let mut subs: Vec<(c_int, SubKind, *mut AVCodecContext)> = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let cp = stream_codecpar(*streams.add(i as usize));
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                let k = sub_kind((*cp).codec_id);
                let dec = if k == SubKind::Bitmap {
                    let d = open_sub_decoder(cp);
                    assert!(
                        !d.is_null(),
                        "the bundled build decodes subtitle stream #{i}"
                    );
                    d
                } else {
                    std::ptr::null_mut()
                };
                subs.push((i as c_int, k, dec));
            }
        }
        let kinds: Vec<bool> = subs.iter().map(|(_, k, _)| *k == SubKind::Bitmap).collect();
        assert_eq!(
            kinds,
            [true, true, true, true, false],
            "the fixture's five tracks"
        );

        let pkt = av_packet_alloc();
        while av_read_frame(fmt, pkt) >= 0 {
            let si = (*pkt).stream_index;
            if let Some(pos) = subs.iter().position(|(s, _, _)| *s == si) {
                let (_, kind, dec) = subs[pos];
                if kind == SubKind::Bitmap {
                    decode_bitmap_cue(dec, pkt, pos as c_int, *streams.add(si as usize));
                } else {
                    let n = (*pkt).size.max(0) as usize;
                    text_payloads.push(std::slice::from_raw_parts((*pkt).data, n).to_vec());
                }
            }
            av_packet_unref(pkt);
        }
        let mut p = pkt;
        av_packet_free(&mut p);
        for (_, _, dec) in subs.iter_mut() {
            if !dec.is_null() {
                avcodec_free_context(dec);
            }
        }
        avformat_close_input(&mut fmt);
    }

    let v = SHARED.sub_bitmaps.lock().unwrap();
    let cues = |track: i32| -> Vec<(i64, i64, usize)> {
        v.iter()
            .filter(|c| c.track == track)
            .map(|c| (c.start_ns, c.end_ns, c.rects.len()))
            .collect()
    };
    let canvas = |track: i32| v.iter().find(|c| c.track == track).map(|c| (c.cw, c.ch));
    let (pgs_zlib, pgs_plain, vob_zlib, vob_plain) = (cues(0), cues(1), cues(2), cues(3));
    let pgs_canvas = canvas(0);
    drop(v);
    SHARED.sub_bitmaps.lock().unwrap().clear();

    // the controls: uncompressed tracks decode, so the fixture and the decode path are sound
    assert_eq!(
        pgs_plain.len(),
        1,
        "uncompressed PGS: its one cue, got {pgs_plain:?}"
    );
    assert_eq!(
        vob_plain.len(),
        1,
        "uncompressed VobSub: its one cue, got {vob_plain:?}"
    );
    // the regression: the same display sets, zlib-compressed the way mkvmerge writes them
    assert_eq!(
        pgs_zlib, pgs_plain,
        "the zlib-compressed PGS track must decode to the same cues as the uncompressed one"
    );
    assert_eq!(
        pgs_canvas,
        Some((1920, 1080)),
        "the PGS cue carries its authoring canvas"
    );
    assert_eq!(
        vob_zlib, vob_plain,
        "the zlib-compressed VobSub track must decode to the same cues as the uncompressed one"
    );
    // ...and a compressed TEXT track reaches the payload path as text, not as `78 da …`
    assert_eq!(text_payloads.len(), 1, "the SRT track's one packet");
    assert_eq!(
        String::from_utf8_lossy(&text_payloads[0]),
        "ZLIB TEXT CUE",
        "the zlib-compressed SRT packet must arrive inflated"
    );
}
