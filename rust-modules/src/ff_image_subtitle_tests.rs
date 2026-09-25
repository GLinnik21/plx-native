//! Subtitles through the FFmpeg the app SHIPS — the component list is the seam under test.
//!
//! Every other `ff` test stays off `av_*`, because the host suite has no FFmpeg to call. This one
//! cannot: what it grades is a property of `ci/build-ffmpeg.sh`'s configure line, which no pure
//! function can see. So it loads the host build of that same FFmpeg (`HOST=1 ci/build-ffmpeg.sh`,
//! staged into `pkg/` by `make check-ffmpeg`) and is `#[ignore]`d everywhere else — an honest
//! "not run" in `make check`, never a pass that executed nothing.

use super::*;

/// Where the host FFmpeg was staged: `PLX_FFMPEG_DIR` (what `make check-ffmpeg` passes), else the
/// checkout's own `pkg/`.
fn host_ffmpeg_dir() -> std::path::PathBuf {
    std::env::var_os("PLX_FFMPEG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pkg"))
}

/// Bind the three libraries exactly as `load_libraries` does — dependency order, one directory —
/// and hold the build to the ABI table this file's offsets were checked against.
fn bind_host_ffmpeg() {
    let dir = host_ffmpeg_dir();
    for (what, verdict) in [
        ("avutil", avutil::load(Some(&dir))),
        ("avcodec", avcodec::load(Some(&dir))),
        ("avformat", avformat::load(Some(&dir))),
    ] {
        assert!(
            matches!(verdict, crate::dynlib::Loaded::Ok(_)),
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
/// `rect_to_rgba`, `sub_canvas`, `push_subtitle_bitmap` — keyed by file-order ordinal as the demux
/// loop keys them.
#[test]
#[ignore = "needs the host build of the bundled FFmpeg — run by `make check-ffmpeg`"]
fn mkvmerge_zlib_subtitle_tracks_decode_like_uncompressed_ones() {
    let _g = crate::testlock::serial();
    bind_host_ffmpeg();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/mkv_zlib_subs.mkv")
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
