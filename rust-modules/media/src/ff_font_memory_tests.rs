//! How many copies of a file's font attachments the process holds once the demuxer has handed them
//! over (issue #1328: 17 attached fonts, 83 MB, ~490 MB resident on a television with 1.6 GB).
//!
//! A Matroska font attachment is read by libavformat once and then kept in three places for as long
//! as the format context lives: the demuxer's own parsed element (`matroskadec`'s `EbmlBin`, freed at
//! `read_close`), the stream's `codecpar->extradata`, and the stream's internal codec context
//! (`avformat_open_input` ends in `update_stream_avctx`, which `avcodec_parameters_to_context`-copies
//! the extradata of EVERY stream, an attachment's included). The app then copied the fonts once more
//! into the source store. Only the `codecpar` copy is the app's to free (`detach_fonts`); the element
//! buffer is private to the demuxer, and the codec context's is private to libavformat until
//! `avformat_find_stream_info` rebuilds it from the codec parameters (the main demuxer runs it, so
//! it ends with the element buffer alone; the side reader does not, and keeps the context's copy
//! too). Those are the copies of the demux half; the renderer's own are `player/ass/tests.rs`.
//!
//! The meter is the C heap (`plx_base::testheap`): FFmpeg `malloc`s these buffers itself. Each
//! measurement runs in a fresh single-threaded process whose input the PARENT made, so neither
//! another test's allocations nor the making of the fixture is in the number, and a copy shows as
//! a multiple of the fonts' bytes: the assertions are on that multiple.
//! `PLX_ASS_FONT_FIXTURE=<file.mkv>` measures a real file instead of the generated one, e.g. the
//! report's `tests/fixtures/make_ass_stress.py --fonts-over-budget` MKV (411 MB, never committed).
//!
//! Needs the host FFmpeg, libass is not involved, and a system `ffmpeg` to generate the fixture, so
//! it is `#[ignore]`d everywhere but `make check-ffmpeg`.

use super::*;
use crate::player::ass_source;
use crate::player::subtitle_cue::{begin_sources, SideTracks};
use plx_base::testheap;
use plx_media_codecs::subtitle_extract::{collect_manifest, detach_fonts, AssTrack, Job, ReadOutcome, ReadSeek};
use plx_playback_contract::subtitle::DecodeKind;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MIB: usize = 1024 * 1024;
const FACES: usize = 6;
const FACE_BYTES: usize = 8 * MIB;
const FIXTURE_ENV: &str = "PLX_ASS_FONT_FIXTURE";
const SIDE_TEST: &str = "ff::subtitle_charset_tests::font_memory::the_side_reader_hands_the_fonts_over_without_keeping_a_copy";
const MAIN_TEST: &str = "ff::subtitle_charset_tests::font_memory::the_main_demuxer_hands_the_fonts_over_without_keeping_a_copy";

/// A file read in place: no byte of it lives in the heap this test measures.
struct FileSource(std::fs::File, i64);

impl ReadSeek for FileSource {
    fn read(&mut self, dst: &mut [u8]) -> ReadOutcome {
        match self.0.read(dst) {
            Ok(0) => ReadOutcome::End,
            Ok(n) => ReadOutcome::Bytes(n),
            Err(_) => ReadOutcome::Failed,
        }
    }
    fn seek(&mut self, absolute: i64) -> bool {
        u64::try_from(absolute).is_ok_and(|to| self.0.seek(SeekFrom::Start(to)).is_ok())
    }
    fn size(&self) -> i64 { self.1 }
    fn cancelled(&self) -> bool { false }
}

/// A scratch directory removed when the test is done with it, however it ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Six 8 MiB faces attached to a 2 s clip with one ASS track, made in `dir` with the system `ffmpeg`
/// (`None`: there is none, and the test says so and stops).
fn make_fixture(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let inter = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pkg/appfont.ttf")).ok()?;
    let mut args: Vec<String> = ["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:r=10:d=2", "-i", "sub.ass"]
        .iter().map(|s| (*s).to_owned()).collect();
    args.extend(["-map", "0:v", "-map", "1:0", "-c:v", "mpeg4", "-c:s", "copy"].map(str::to_owned));
    for i in 0..FACES {
        let face = crate::player::ass::synth_fonts::rename(&inter, &format!("Plx Heap {i}"), "Regular", Some(FACE_BYTES));
        std::fs::write(dir.join(format!("heap-{i}.ttf")), face).ok()?;
        args.extend(["-attach".to_owned(), format!("heap-{i}.ttf"), format!("-metadata:s:t:{i}"), "mimetype=font/ttf".to_owned()]);
    }
    args.push("out.mkv".to_owned());
    std::fs::write(
        dir.join("sub.ass"),
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 180\n\n[V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,Plx Heap 0,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,2,0,0,0,1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,hello\n",
    ).ok()?;
    let made = std::process::Command::new("ffmpeg").current_dir(dir).args(&args).status().ok()?;
    made.success().then(|| dir.join("out.mkv"))
}

/// The heap a measurement added, over `base`, as the numbers the parent test asserts on.
fn report_over(base: usize, what: &str, now: usize, peak: usize) {
    testheap::report(&format!("{what}_held"), now.saturating_sub(base) as u64);
    testheap::report(&format!("{what}_peak"), peak.saturating_sub(base) as u64);
}

/// The MKV the parent made, in the child.
fn fixture_in_child() -> PathBuf {
    std::env::var_os(FIXTURE_ENV).map(PathBuf::from).expect("the parent test hands the fixture over")
}

/// `Job::open` and `manifest`, as the side reader does (`ff_subside::open_side`), then the hand-over
/// to the store; and again after the job is closed, so the store's own copy is told apart from
/// libavformat's.
fn measure_side_reader() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    crate::player::request_subtitle(0);
    let file = std::fs::File::open(fixture_in_child()).expect("the fixture opens");
    let size = file.metadata().expect("the fixture has a size").len() as i64;
    let base = testheap::live_bytes().expect("a host whose malloc can be asked");
    let ((held, closed, font_bytes), peak) = testheap::peak_during(|| {
        let mut job = Job::open(Box::new(FileSource(file, size)), true).ok().expect("the side reader opens the fixture");
        let tracks = SideTracks::new(job.tracks());
        job.configure(tracks.decode_plan());
        let manifest = job.manifest(ass_source::MAX_HEADER_BYTES);
        let font_bytes = manifest.font_bytes;
        begin_sources("fixture:font-memory", tracks.kinds(), manifest);
        let held = testheap::live_bytes().unwrap();
        drop(job);
        (held, testheap::live_bytes().unwrap(), font_bytes)
    });
    testheap::report("font_bytes", font_bytes as u64);
    report_over(base, "open", held, peak);
    testheap::report("store_only", closed.saturating_sub(base) as u64);
}

/// `avformat_open_input`, `avformat_find_stream_info`, then the manifest: the order `ff.rs` opens the
/// film it plays in.
fn measure_main_demuxer() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
    crate::player::request_subtitle(0);
    let path = CString::new(fixture_in_child().to_str().unwrap()).unwrap();
    let base = testheap::live_bytes().expect("a host whose malloc can be asked");
    let ((held, font_bytes), peak) = testheap::peak_during(|| unsafe {
        let mut fmt = avformat_alloc_context();
        assert!(avformat_open_input(&mut fmt, path.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
        let fonts = detach_fonts(fmt);
        assert!(avformat_find_stream_info(fmt, std::ptr::null_mut()) >= 0);
        let streams = (*fmt).streams;
        let infos = SubTracks::inspect(fmt, streams);
        let presentation = SideTracks::new(&infos);
        let tracks: Vec<_> = infos.iter().zip(presentation.decode_plan()).map(|(info, kind)| AssTrack {
            stream_index: info.stream_index,
            authored: matches!(kind, DecodeKind::Ass { .. }),
        }).collect();
        let manifest = collect_manifest(streams, &tracks, ass_source::MAX_HEADER_BYTES, fonts);
        let font_bytes = manifest.font_bytes;
        begin_sources("fixture:font-memory", presentation.kinds(), manifest);
        let held = testheap::live_bytes().unwrap();
        avformat_close_input(&mut fmt);
        (held, font_bytes)
    });
    testheap::report("font_bytes", font_bytes as u64);
    report_over(base, "open", held, peak);
    testheap::report("store_only", testheap::live_bytes().unwrap().saturating_sub(base) as u64);
}

/// `bytes` as a multiple of the font bytes, for a failure message.
fn copies(bytes: u64, fonts: u64) -> f64 {
    bytes as f64 / fonts as f64
}

/// The fresh process's numbers for `test`; `None` in the child itself (which measures and reports)
/// and when there is no `ffmpeg` to generate the fixture (said, like the other generated-MKV tests
/// do, and not a pass that measured nothing).
fn numbers(test: &str, tag: &str, child: fn()) -> Option<HashMap<String, u64>> {
    if testheap::is_child() {
        child();
        return None;
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("plx-fontmem-{}-{tag}", std::process::id())));
    let mkv = match std::env::var_os(FIXTURE_ENV) {
        Some(given) => PathBuf::from(given),
        None => match make_fixture(&scratch.0) {
            Some(made) => made,
            None => {
                eprintln!("SKIPPED: no system ffmpeg to generate the fixture");
                return None;
            }
        },
    };
    Some(testheap::in_fresh_process_with_env(test, &[(FIXTURE_ENV, mkv.as_os_str())]))
}

/// What libavformat holds once the manifest is taken, as a multiple of the fonts: everything live
/// but the store's own copy.
fn reader_copies(n: &HashMap<String, u64>) -> f64 {
    copies(n["open_held"].saturating_sub(n["store_only"]), n["font_bytes"])
}

fn assert_fonts_handed_over(n: &HashMap<String, u64>, what: &str, libavformat_keeps: f64) {
    let fonts = n["font_bytes"];
    eprintln!(
        "{what}: {:.0} MiB of fonts; libavformat holds {:.2}x, the open peaks at {:.2}x, the store alone holds {:.2}x",
        fonts as f64 / MIB as f64,
        reader_copies(n),
        copies(n["open_peak"], fonts),
        copies(n["store_only"], fonts)
    );
    assert!(fonts >= (FACES * FACE_BYTES) as u64 || std::env::var_os(FIXTURE_ENV).is_some(), "the fixture's fonts: {fonts}");
    // What libavformat keeps for itself (`libavformat_keeps` copies, with half a copy of room) is out
    // of the app's reach; `codecpar->extradata`, the copy over that, is the app's and the hand-over
    // frees it.
    assert!(
        reader_copies(n) < libavformat_keeps + 0.5,
        "{what} holds the {:.0} MiB of fonts {:.2} more times after the hand-over, not {libavformat_keeps}: `codecpar->extradata` is still there",
        fonts as f64 / MIB as f64,
        reader_copies(n)
    );
    assert!(
        copies(n["open_peak"], fonts) < 3.5,
        "{what} peaked at {:.2} times the fonts: the hand-over holds a copy beside the three libavformat made",
        copies(n["open_peak"], fonts)
    );
    assert!(
        copies(n["store_only"], fonts) < 1.25,
        "after {what} closes the store alone holds them, {:.2} times",
        copies(n["store_only"], fonts)
    );
}

/// The side reader (`Job`: `avformat_open_input` alone) used to hold the fonts three times once it had
/// handed them to the store (and a fourth time in the store); now libavformat's element buffer and
/// codec context are the two.
#[test]
#[ignore = "needs the bundled host FFmpeg and a system ffmpeg; run make check-ffmpeg"]
fn the_side_reader_hands_the_fonts_over_without_keeping_a_copy() {
    let Some(n) = numbers(SIDE_TEST, "side", measure_side_reader) else { return };
    assert_fonts_handed_over(&n, "the open side reader", 2.0);
}

/// The main demuxer takes the fonts right after `avformat_open_input`, then runs
/// `avformat_find_stream_info`, which rebuilds the stream's codec context from the now empty codec
/// parameters: libavformat is left with the element buffer alone.
#[test]
#[ignore = "needs the bundled host FFmpeg and a system ffmpeg; run make check-ffmpeg"]
fn the_main_demuxer_hands_the_fonts_over_without_keeping_a_copy() {
    let Some(n) = numbers(MAIN_TEST, "main", measure_main_demuxer) else { return };
    assert_fonts_handed_over(&n, "the open demuxer", 1.0);
}
