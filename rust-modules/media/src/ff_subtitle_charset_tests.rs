//! A GBK-encoded subrip track muxed into Matroska, read through the real demuxer (issue #628).
//!
//! `tests/fixtures/gbk-subrip.mkv` is a 588-byte file made with
//! `ffmpeg -i gbk.srt -map 0 -c:s copy -metadata:s:s:0 language=chi gbk-subrip.mkv` from a GBK
//! `.srt` of two Chinese cues (the first reads "你好，世界！这是中文字幕。", the second
//! "简体中文测试"): the packets keep the GBK bytes and the track says `chi`. Before the charset step
//! every cue read as a row of U+FFFD. Like `ff_image_subtitle_tests` it needs the host FFmpeg
//! (`make check-ffmpeg`), so it is `#[ignore]`d everywhere else.
//!
//! Text cues are rendered by libass now, so what is asserted is the ASS event the cue became: the
//! converted body of each cue in the track's source, which must be the Chinese text (and nothing
//! else) for the charset step to have worked.

use super::image_subtitle_tests::bind_host_ffmpeg;
use super::*;
use crate::player::ass::Content;
use crate::player::ass_source;

#[test]
#[ignore = "requires the bundled host FFmpeg; run make check-ffmpeg"]
fn a_gbk_subrip_track_reads_as_chinese_not_replacement_characters() {
    let _g = plx_base::testlock::serial();
    let _ffmpeg = bind_host_ffmpeg();
    ass_source::reset();
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
        let generation = begin_ass_sources("fixture:gbk-subrip", fmt, (*fmt).streams, subs.entries());
        let mut pushed = 0;
        while av_read_frame(fmt, pkt) >= 0 {
            if let Some(pos) = subs.position((*pkt).stream_index) {
                let st = *(*fmt).streams.add((*pkt).stream_index as usize);
                if let Some(cue) = subs.decode(pos, pkt, st) {
                    cue.push(generation);
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
    let source = ass_source::selected(0).expect("the subrip track is registered as a text track");
    assert!(source.origin.is_text());
    let Content::Embedded { events, .. } = &source.content else { panic!("an embedded source") };
    // `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`: the text is after the 8th comma.
    let bodies: Vec<String> = events
        .iter()
        .map(|e| {
            let packet = std::str::from_utf8(&e.payload).expect("an ASS packet is UTF-8");
            packet.splitn(9, ',').nth(8).unwrap().to_owned()
        })
        .collect();
    crate::player::request_subtitle(-1);
    ass_source::reset();
    assert_eq!(bodies, ["你好，世界！这是中文字幕。", "简体中文测试"]);
    assert!(bodies.iter().all(|b| !b.contains('\u{fffd}')));
}
