//! Subtitles through the FFmpeg the app SHIPS — the component list and the ASS ingestion path are
//! the seams under test.
//!
//! The host suite has no FFmpeg to call, so most `ff` tests stay off `av_*`. These cannot: what
//! they grade is a property of `ci/build-ffmpeg.sh`'s configure line (zlib-compressed PGS tracks)
//! or of bytes only the real demuxer produces (an ASS header and packets that arrive NUL-terminated,
//! through both the main subtitle ingestion and the HTTP side reader). So they load the host build of
//! that same FFmpeg (`HOST=1 ci/build-ffmpeg.sh`, staged into `pkg/` by `make check-ffmpeg`), and the
//! ASS ones also need the staged libass; all are `#[ignore]`d everywhere else — an honest "not run" in
//! `make check`, never a pass that executed nothing. `plx_media_codecs::host_test_support`
//! owns the host binding guard shared with real-demux fixtures (`ff_subside`, `ff_late_audio_tests`).

use plx_play_feed::shared::FEED_SHARED;
use super::*;
use crate::player::subtitle_cue::{SideTracks, SubCue};
use crate::player::SHARED;
use crate::player::{ass, ass_source};
use plx_media_codecs::subtitle_extract::{collect_manifest, detach_fonts, AssTrack};
use plx_playback_contract::demux::DemuxSubtitle;
use plx_playback_contract::subtitle::{CaptionRetention, DecodeKind, Manifest, RawCue, TrackInfo};
use std::cell::Cell;
use std::sync::Arc;

const TERMINATED_ASS_HEADER: &[u8] =
    include_bytes!("../../../tests/fixtures/ass-terminated-header.bin");

fn fixture_retention() -> CaptionRetention {
    CaptionRetention { playhead_ns: Some(0), seek_target_ns: None }
}

struct AssFixture {
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
    subs: SubTracks,
    presentation: SideTracks,
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
        let infos = SubTracks::inspect(fmt, (*fmt).streams);
        let presentation = SideTracks::new(&infos);
        let plan = presentation.decode_plan();
        let subs = SubTracks::open((*fmt).streams, infos, plan);
        assert_eq!(subs.entries().len(), 2);
        Self { fmt, pkt, subs, presentation }
    }

    unsafe fn sources(&self) -> u64 {
        let tracks: Vec<_> = self.subs.entries().iter().zip(self.subs.plan()).map(|(info, kind)| AssTrack {
            stream_index: info.stream_index,
            authored: matches!(kind, DecodeKind::Ass { .. }),
        }).collect();
        let manifest = collect_manifest((*self.fmt).streams, &tracks,
            ass_source::MAX_HEADER_BYTES, detach_fonts(self.fmt));
        crate::player::subtitle_cue::begin_sources("fixture:ass-header", self.presentation.kinds(), manifest)
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
                if let Some(raw) = self.subs.decode(pos, self.pkt,
                    *(*self.fmt).streams.add(si as usize), true, crate::player::SUB_BITMAP_BUDGET) {
                    self.presentation.decode(raw).push(generation, fixture_retention());
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
        if let Some(frame) = ass::request(source.clone(), now_ms, 320, 180, 320, 180, ass::Pace::PAUSED) {
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
    FEED_SHARED.playpos_ns.store(0, Ordering::Relaxed);
    unsafe {
        let d = AssFixture::open();
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs.entries()[0].stream_index as usize));
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
    use super::subside::Anchor;
    use plx_play_feed::side_reader::{ReadCfg, RunEnd, SideStop};
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
    FEED_SHARED.playpos_ns.store(0, Ordering::Relaxed);
    let reader = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let url = format!("http://127.0.0.1:{port}/library/parts/1/ass-header.mkv");
            let cfg = ReadCfg {
                url: &url,
                offset_ns: 0,
                ordinal: 0,
                kbps: 0,
                clock_only: false,
            };
            let anchor = Anchor::of(0, &[0]);
            let mut delta = Some(0);
            super::subside::read_once(&cfg, &stop, &anchor, &mut delta, &|_| 0, &|| 0, &|| (0, 0), &fixture_retention, &|_| {})
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
        FEED_SHARED.playpos_ns.store(0, Ordering::Relaxed);
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
                payload: payload.into(),
            }
            .push(generation, fixture_retention());
            SubCue::Ass {
                track: 0,
                start: 1_000_000_000,
                end: 3_000_000_000,
                payload: b"0,0,Default,,0,0,0,,VALID CAPTION"[..].into(),
            }
            .push(generation, fixture_retention());
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
                payload: payload.into(),
            }
            .push(generation, fixture_retention());
        }
        let oversized = vec![0; 256 * 1024 + 1];
        SubCue::Ass {
            track: 0,
            start: 1_000_000_000,
            end: 3_000_000_000,
            payload: oversized.as_slice().into(),
        }
        .push(generation, fixture_retention());
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
            payload: b"0,0,Default,,0,0,0,,MULTIPLE TERMINATORS\0\0\0"[..].into(),
        }
        .push(generation, fixture_retention());
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
        let cp = stream_codecpar(*(*d.fmt).streams.add(d.subs.entries()[0].stream_index as usize));
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

pub(crate) use plx_media_codecs::host_test_support::HostFfmpeg;

/// Compatibility delegate for existing feed integration fixtures. The codec
/// crate owns the one host loader and restores the ABI gate on guard drop.
pub(crate) fn bind_host_ffmpeg() -> HostFfmpeg {
    plx_media_codecs::host_test_support::bind_host_ffmpeg()
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
/// Image tracks go through the same lower decoder and common cue presentation as the demux loop,
/// keyed by file-order ordinal.
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
    FEED_SHARED.playpos_ns.store(0, Ordering::Relaxed);
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
        let infos = SubTracks::inspect(fmt, streams);
        let presentation = SideTracks::new(&infos);
        let plan = presentation.decode_plan();
        let kinds: Vec<bool> = plan.iter().map(|kind| matches!(kind, DecodeKind::Bitmap)).collect();
        assert_eq!(
            kinds,
            [true, true, true, true, false],
            "the fixture's five tracks"
        );
        let mut subs = SubTracks::open(streams, infos, plan);

        let pkt = av_packet_alloc();
        while av_read_frame(fmt, pkt) >= 0 {
            let si = (*pkt).stream_index;
            if let Some(pos) = subs.position(si) {
                if matches!(subs.plan()[pos], DecodeKind::Bitmap) {
                    if let Some(raw) = subs.decode(pos, pkt, *streams.add(si as usize), true, crate::player::SUB_BITMAP_BUDGET) {
                        presentation.decode(raw).push(0, fixture_retention());
                    }
                } else {
                    let n = (*pkt).size.max(0) as usize;
                    text_payloads.push(std::slice::from_raw_parts((*pkt).data, n).to_vec());
                }
            }
            av_packet_unref(pkt);
        }
        let mut p = pkt;
        av_packet_free(&mut p);
        subs.close();
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

/// When, relative to the demux loop's two per-packet subtitle steps, the selection turns on:
/// `SubTracks::release_if_selected` (every packet read) and `SubTracks::on_packet` (a subtitle
/// stream's packet), the two calls `ff::demux` makes and these tests make.
#[derive(Clone, Copy, PartialEq)]
enum Selection {
    On,
    OffThenOn,
    /// On from the first packet at or after this time (ns), seen by the release check.
    OffUntil(i64),
    /// On from the first packet at or after this time (ns), but only AFTER that packet's release
    /// check: the selection lands between the check and the decode, the race the decode gate closes.
    LandsAfterReleaseCheck(i64),
    OffThenSeekThenOn,
}

/// The subtitle owner `demux` talks to (`DemuxSubtitle`) with the selection as a flag the fixture
/// flips and every cue going to the common presentation, counting the image cues that were emitted
/// while the selection was off.
struct FixtureSubtitle<'a> {
    on: &'a Cell<bool>,
    presentation: &'a SideTracks,
    image_cues_emitted_while_off: usize,
}

impl DemuxSubtitle for FixtureSubtitle<'_> {
    fn plan(&mut self, _: &[TrackInfo]) -> Vec<DecodeKind> { Vec::new() }
    fn max_header_bytes(&self) -> usize { 0 }
    fn begin_sources(&mut self, _: &str, _: Manifest) -> Option<u64> { None }
    fn record_tracks(&self) {}
    fn selected_track(&self) -> i32 { if self.on.get() { 0 } else { -1 } }
    fn bitmap_budget(&self) -> usize { crate::player::SUB_BITMAP_BUDGET }
    fn emit(&mut self, generation: u64, cue: RawCue) {
        if matches!(cue, RawCue::Bitmap { .. }) && !self.on.get() { self.image_cues_emitted_while_off += 1; }
        self.presentation.decode(cue).push(generation, fixture_retention());
    }
    fn seek(&mut self, _: u64) {}
}

/// The cues `mkv_zlib_subs.mkv`'s image tracks put in the store as `(track, start_ns, end_ns,
/// rects)`, and how many image cues were emitted while the selection was off, when every packet is
/// read the way the demux loop reads them under `selection`: `release_if_selected` on every packet
/// read, then `on_packet` for a subtitle stream's. It calls the `SubTracks` methods `ff::demux`
/// calls, so what it grades is what the loop does, not a copy of it.
unsafe fn present_zlib_fixture(selection: Selection) -> (Vec<(i32, i64, i64, usize)>, usize) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mkv_zlib_subs.mkv")
        .canonicalize()
        .expect("fixture present");
    let url = CString::new(path.to_str().unwrap()).unwrap();
    SHARED.sub_bitmaps.lock().unwrap().clear();
    FEED_SHARED.playpos_ns.store(0, Ordering::Relaxed);

    let mut fmt: *mut AVFormatContext = std::ptr::null_mut();
    assert!(
        avformat_open_input(&mut fmt, url.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0,
        "open the fixture"
    );
    let infos = SubTracks::inspect(fmt, (*fmt).streams);
    let presentation = SideTracks::new(&infos);
    let plan = presentation.decode_plan();
    let mut subs = SubTracks::open((*fmt).streams, infos, plan);
    let on = Cell::new(selection == Selection::On);
    let mut subtitle = FixtureSubtitle { on: &on, presentation: &presentation, image_cues_emitted_while_off: 0 };

    let pkt = av_packet_alloc();
    while av_read_frame(fmt, pkt) >= 0 {
        let si = (*pkt).stream_index;
        let st = *(*fmt).streams.add(si as usize);
        let read_ns = plx_media_codecs::subtitle_extract::pts_ns_opt(pkt, st);
        let reached = |at_ns: i64| read_ns.is_some_and(|t| t >= at_ns);
        if let Selection::OffUntil(at_ns) = selection {
            if reached(at_ns) { on.set(true); }
        }
        subs.release_if_selected(fmt, &mut subtitle, 0);
        if let Selection::LandsAfterReleaseCheck(at_ns) = selection {
            if reached(at_ns) { on.set(true); }
        }
        if let Some(pos) = subs.position(si) {
            subs.on_packet(pos, pkt, fmt, &mut subtitle, 0, || Some(fixture_retention()));
        }
        av_packet_unref(pkt);
    }
    let mut p = pkt;
    av_packet_free(&mut p);

    if selection == Selection::OffThenSeekThenOn {
        // Without this the "nothing lands" result below would hold for a backlog that was never kept.
        assert!(subs.holds_images(), "the packets read while Off are held");
        subs.discard_held();
    }
    // The first packet after the selection: everything held is decoded before it.
    on.set(true);
    subs.release_if_selected(fmt, &mut subtitle, 0);
    assert!(!subs.holds_images(), "releasing leaves nothing held");
    let emitted_while_off = subtitle.image_cues_emitted_while_off;
    subs.close();
    avformat_close_input(&mut fmt);

    let mut cues: Vec<(i32, i64, i64, usize)> = SHARED
        .sub_bitmaps
        .lock()
        .unwrap()
        .iter()
        .map(|c| (c.track, c.start_ns, c.end_ns, c.rects.len()))
        .collect();
    cues.sort();
    SHARED.sub_bitmaps.lock().unwrap().clear();
    (cues, emitted_while_off)
}

/// **An image track turned on mid-play shows the lines the demuxer had already read.** The demuxer
/// reads ahead of the playhead; with subtitles Off the image packets of that window were dropped, so
/// a track chosen at 232 s showed nothing until the read-ahead had been used up (10 s), while a TEXT
/// track, whose cues are kept whatever the selection, answered at once. Held packets decode, in file
/// order, to exactly the cues the same packets give when they are decoded as they are read.
///
/// The fixture's image tracks (PGS and VobSub, each also zlib-compressed) carry one cue at 0 s.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn image_packets_read_while_subtitles_are_off_decode_when_one_is_turned_on() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    unsafe {
        let (live, _) = present_zlib_fixture(Selection::On);
        assert!(
            live.iter().any(|cue| cue.0 == 0) && live.iter().any(|cue| cue.0 == 2),
            "the control: the PGS and VobSub tracks decode when read live, got {live:?}"
        );
        let (after_flip, emitted_while_off) = present_zlib_fixture(Selection::OffThenOn);
        assert_eq!(emitted_while_off, 0, "nothing is decoded while Off");
        assert_eq!(
            after_flip, live,
            "the image cue read while Off must be emitted once an image track is on"
        );
    }
}

/// The same flip with the demuxer part-way through the file, which is how it happens on the
/// television: the 0 s display set was read while Off, the track is turned on, and the 8 s clear
/// then arrives live. The held set must be decoded BEFORE the clear, or the clear would close
/// nothing and the line would never be drawn.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn a_track_turned_on_between_two_packets_gets_the_held_set_before_the_live_one() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    unsafe {
        let (live, _) = present_zlib_fixture(Selection::On);
        let (mid_read, emitted_while_off) = present_zlib_fixture(Selection::OffUntil(4_000_000_000));
        assert_eq!(emitted_while_off, 0, "nothing is decoded while Off");
        assert!(
            mid_read.iter().any(|cue| cue.0 == 0 && cue.1 == 0 && cue.2 == 8_000_000_000),
            "the PGS line is drawn from 0 s to the clear at 8 s, got {mid_read:?}"
        );
        assert_eq!(mid_read, live, "held first, live after, exactly as if it had been on throughout");
    }
}

/// A seek moves the playhead to a place the held packets do not describe: they are discarded, and
/// the selection that follows emits nothing of them.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn a_seek_discards_the_image_packets_held_while_subtitles_were_off() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    unsafe {
        let (cues, _) = present_zlib_fixture(Selection::OffThenSeekThenOn);
        assert!(cues.is_empty(), "no stale cue lands after a seek, got {cues:?}");
    }
}

/// The race the demux loop's decode gate closes. The selection turns on after a packet's release
/// check and before its decode; the held display set has not been released, so decoding this packet
/// now would put the 8 s clear ahead of the 0 s set it closes: the clear would close nothing, and
/// the set, decoded after it, would be drawn open-ended or not at all. The packet is held instead
/// and decoded with the rest at the next packet.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn a_selection_landing_between_the_release_check_and_the_decode_keeps_file_order() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    unsafe {
        let (live, _) = present_zlib_fixture(Selection::On);
        let (raced, emitted_while_off) = present_zlib_fixture(Selection::LandsAfterReleaseCheck(4_000_000_000));
        assert_eq!(emitted_while_off, 0, "nothing is decoded while Off");
        assert!(
            raced.iter().any(|cue| cue.0 == 0 && cue.1 == 0 && cue.2 == 8_000_000_000),
            "the PGS line is drawn from 0 s to the clear at 8 s, got {raced:?}"
        );
        assert_eq!(raced, live, "the held set first, the clear after, exactly as if it had been on throughout");
    }
}
