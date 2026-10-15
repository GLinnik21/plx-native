//! A GBK-encoded subrip track muxed into Matroska, read through the real demuxer (issue #628).
//!
//! `tests/fixtures/gbk-subrip.mkv` is a 588-byte file made with
//! `ffmpeg -i gbk.srt -map 0 -c:s copy -metadata:s:s:0 language=chi gbk-subrip.mkv` from a GBK
//! `.srt` of two Chinese cues (the first reads "你好，世界！这是中文字幕。", the second
//! "简体中文测试"): the packets keep the GBK bytes and the track says `chi`. Before the charset step
//! every cue read as a row of U+FFFD. Like `ff_image_subtitle_tests` it needs the host FFmpeg
//! (`make check-ffmpeg`), so it is `#[ignore]`d everywhere else.

use super::image_subtitle_tests::bind_host_ffmpeg;
use super::*;

#[test]
#[ignore = "requires the bundled host FFmpeg; run make check-ffmpeg"]
fn a_gbk_subrip_track_reads_as_chinese_not_replacement_characters() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    crate::player::SHARED.sub_cues.lock().unwrap().clear();
    crate::player::request_subtitle(0);
    unsafe {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gbk-subrip.mkv");
        let path = CString::new(path.to_str().unwrap()).unwrap();
        let mut fmt = std::ptr::null_mut();
        assert!(avformat_open_input(&mut fmt, path.as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) >= 0);
        let mut pkt = av_packet_alloc();
        assert!(!pkt.is_null());
        assert!(!fmt.is_null());
        let mut subs = SubTracks::open(fmt, (*fmt).streams);
        assert_eq!(subs.entries().len(), 1, "the fixture has one subtitle track");
        let mut pushed = 0;
        while av_read_frame(fmt, pkt) >= 0 {
            if let Some(pos) = subs.position((*pkt).stream_index) {
                let st = *(*fmt).streams.add((*pkt).stream_index as usize);
                if let Some(cue) = subs.decode(pos, pkt, st) {
                    cue.push(0);
                    pushed += 1;
                }
            }
            av_packet_unref(pkt);
        }
        assert_eq!(pushed, 2, "both cues");
        subs.close();
        av_packet_free(&mut pkt);
        avformat_close_input(&mut fmt);
    }
    let first = crate::player::active_subtitle_span(1_500_000_000).expect("cue 1 is up at 1.5 s").0;
    let second = crate::player::active_subtitle_span(4_500_000_000).expect("cue 2 is up at 4.5 s").0;
    crate::player::request_subtitle(-1);
    crate::player::SHARED.sub_cues.lock().unwrap().clear();
    assert_eq!(first, "你好，世界！这是中文字幕。");
    assert_eq!(second, "简体中文测试");
    assert!(!first.contains('\u{fffd}') && !second.contains('\u{fffd}'));
}
