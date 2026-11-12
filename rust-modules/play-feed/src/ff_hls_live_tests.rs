//! The actual master -> media cursor, with playlist responses in memory: no native codec or wire.
use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

const MASTER: &str = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmedia.m3u8\n";
const LIVE_ENTRY: &str = "/start.m3u8?path=%2Flivetv%2Fsessions%2F9b9fe727-0000-46db-b94c-ef512e3e61d2&X-Plex-Token=fixture";
const LIBRARY_ENTRY: &str = "/start.m3u8?path=%2Flibrary%2Fmetadata%2F5&X-Plex-Token=fixture";

struct Playlists {
    replies: VecDeque<(&'static str, String)>,
    body: VecDeque<u8>,
    size: i64,
}

impl RangeReader for Playlists {
    fn reopen_until(&mut self, url: &str, _: Option<std::time::Instant>,
        _: &mut dyn plx_base::checkpoint::Checkpoint) -> Result<(), range_io::OpenErr> {
        let (path, text) = self.replies.pop_front().expect("unexpected playlist request");
        assert!(url.split('?').next().unwrap().ends_with(&format!("/{path}")), "{url}");
        self.size = text.len() as i64;
        self.body = text.into_bytes().into();
        Ok(())
    }
    fn read_until(&mut self, dst: &mut [u8], _: Option<std::time::Instant>,
        _: &mut dyn plx_base::checkpoint::Checkpoint) -> c_int {
        let n = dst.len().min(self.body.len());
        for byte in &mut dst[..n] { *byte = self.body.pop_front().unwrap(); }
        n as c_int
    }
    fn size(&self) -> i64 { self.size }
    fn status(&self) -> c_int { 200 }
    fn body_complete(&self) -> bool { self.body.is_empty() }
    fn body_failure(&self) -> Option<range_io::BodyFailure> { None }
    fn body_receipt(&mut self) -> plx_net::stream::BodyReceipt {
        plx_net::stream::BodyReceipt { ahead: self.body.len() as i64, finished: true, stepped: false }
    }
    fn drain_available(&mut self, _: &mut [u8]) -> c_int { unreachable!() }
    fn seek(&mut self, _: i64) -> bool { unreachable!() }
    fn abort(&self) { unreachable!() }
}

struct NoDial;
impl RangeFactory for NoDial {
    fn reserve_open(&self) -> Result<range_io::OpenReservation, range_io::OpenErr> { unreachable!() }
    fn open_reserved(&self, _: &str, _: i64, _: range_io::OpenReservation)
        -> Result<Box<dyn RangeReader>, range_io::OpenErr> { unreachable!() }
    fn open_reserved_checked(&self, _: &str, _: i64, _: range_io::OpenReservation,
        _: Option<std::time::Instant>, _: &mut dyn plx_base::checkpoint::Checkpoint)
        -> Result<Box<dyn RangeReader>, range_io::OpenErr> { unreachable!() }
}

#[derive(Default)]
struct Observer { durations: Mutex<Vec<i64>> }
impl DemuxObserver for Observer {
    fn attempt(&self) -> plx_playback_contract::AttemptId { plx_playback_contract::AttemptId::new(0, 0) }
    fn playpos_ns(&self) -> Option<i64> { Some(0) }
    fn caption_retention(&self) -> Option<plx_playback_contract::subtitle::CaptionRetention> { None }
    fn hold_epoch(&self) -> Option<u64> { Some(0) }
    fn rebuffering(&self) -> Option<bool> { Some(false) }
    fn audio_expected(&self) -> Option<bool> { Some(true) }
    fn duration_ns(&self) -> Option<i64> { Some(0) }
    fn declared_video_code(&self) -> Option<u8> { Some(0) }
    fn declared_audio_code(&self) -> Option<u8> { Some(0) }
    fn paused(&self) -> Option<bool> { Some(false) }
    fn pause_clock(&self) -> Option<plx_playback_contract::demux::PauseClock> { None }
    fn hls_buffer_ms(&self) -> Option<Option<i64>> { Some(None) }
    fn hls_buffer_clocks(&self) -> Option<HlsBufferClocks> { None }
    fn remaining_playback_ms(&self) -> Option<i64> { Some(0) }
    fn user_sequence(&self) -> Option<u64> { Some(0) }
    fn with_active(&self, f: &mut dyn FnMut()) -> bool { f(); true }
    fn publish(&self, effect: DemuxCallbackEffect) -> bool {
        if let DemuxCallbackEffect::DurationNs(ns) = effect { self.durations.lock().unwrap().push(ns); }
        true
    }
}

fn media(count: u64, kind: &str, end_list: bool) -> String {
    let mut text = format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n{kind}");
    for n in 0..count { text.push_str(&format!("#EXTINF:1,\n{n}.ts\n")); }
    if end_list { text.push_str("#EXT-X-ENDLIST\n"); }
    text
}

fn net(replies: Vec<(&'static str, String)>, observer: Arc<Observer>) -> HlsNet {
    HlsNet {
        hs: std::ptr::null_mut(),
        curl: Some(Box::new(Playlists { replies: replies.into(), body: VecDeque::new(), size: 0 })),
        range: Arc::new(NoDial),
        credential_gate: |_, _, _| true,
        observer,
        carry: None,
    }
}

fn open(aq: &mut AuQueue, net: &mut HlsNet, path: &str) -> HlsCursor {
    hls_cursor_open(&Origin::parse("https://192.0.2.10:32400").unwrap(),
        path, aq, net, true, None).unwrap()
}

#[test]
fn a_master_live_playlist_starts_and_reopens_at_the_edge_without_publishing_duration() {
    let observer = Arc::new(Observer::default());
    let mut net = net(vec![
        ("start.m3u8", MASTER.into()), ("media.m3u8", media(12, "", false)),
        ("media.m3u8", media(13, "", false)),
        ("start.m3u8", MASTER.into()), ("media.m3u8", media(20, "", false)),
    ], observer.clone());
    let mut aq = crate::aq::aq_new(1024);
    let mut cursor = open(&mut aq, &mut net, LIVE_ENTRY);
    assert_eq!(cursor.declared_bps, 1_000_000);
    for sequence in 9..13 {
        assert_eq!(hls_cursor_next(&mut cursor, &mut *aq, &mut net, None).unwrap().unwrap().sequence,
            sequence, "initial open must start three seconds behind the edge, then follow growth");
    }
    assert!(cursor.live);
    assert!(net.carry.is_none(), "transcodes do not inherit a tuner's segment state");
    let mut reopened = open(&mut aq, &mut net, LIVE_ENTRY);
    assert_eq!(hls_cursor_next(&mut reopened, &mut *aq, &mut net, None).unwrap().unwrap().sequence, 17);
    assert!(observer.durations.lock().unwrap().is_empty(), "playlist growth is no content duration");
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn library_playlists_keep_their_start_offset_and_duration_even_while_growing() {
    for (kind, end_list) in [("", true), ("#EXT-X-PLAYLIST-TYPE:EVENT\n", true),
        ("#EXT-X-PLAYLIST-TYPE:VOD\n", true), ("#EXT-X-PLAYLIST-TYPE:VOD\n", false),
        ("", false)] {
        let observer = Arc::new(Observer::default());
        let text = media(12, &format!("{kind}#EXT-X-START:TIME-OFFSET=2\n"), end_list);
        let mut net = net(vec![("start.m3u8", MASTER.into()), ("media.m3u8", text)], observer.clone());
        let mut aq = crate::aq::aq_new(1024);
        let mut cursor = open(&mut aq, &mut net, LIBRARY_ENTRY);
        assert_eq!(hls_cursor_next(&mut cursor, &mut *aq, &mut net, None).unwrap().unwrap().sequence, 2);
        assert!(!cursor.live);
        assert_eq!(*observer.durations.lock().unwrap(), [12_000_000_000]);
        crate::aq::aq_destroy(&mut *aq);
    }
}

#[test]
fn an_open_event_playlist_uses_the_live_edge() {
    let observer = Arc::new(Observer::default());
    let mut net = net(vec![("start.m3u8", MASTER.into()),
        ("media.m3u8", media(12, "#EXT-X-PLAYLIST-TYPE:EVENT\n", false))], observer.clone());
    let mut aq = crate::aq::aq_new(1024);
    let mut cursor = open(&mut aq, &mut net, LIVE_ENTRY);
    assert_eq!(hls_cursor_next(&mut cursor, &mut *aq, &mut net, None).unwrap().unwrap().sequence, 9);
    assert!(cursor.live);
    assert!(observer.durations.lock().unwrap().is_empty());
    crate::aq::aq_destroy(&mut *aq);
}

#[test]
fn a_live_source_is_named_by_the_transcoder_path_not_by_growth() {
    for live in [LIVE_ENTRY,
        "/start.m3u8?path=/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2&X-Plex-Token=fixture"] {
        assert!(is_live_session_source(live), "{live}");
    }
    for library in ["/start.m3u8?path=%2Flibrary%2Fmetadata%2F5&X-Plex-Token=t",
        "/start.m3u8?X-Plex-Token=t", "/start.m3u8", "/start.m3u8?subpath=%2Flivetv%2Fsessions%2Fx"] {
        assert!(!is_live_session_source(library), "{library}");
    }
}
