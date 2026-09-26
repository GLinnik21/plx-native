//! **External (sidecar) subtitles on the direct-play path** — the `.srt` beside the film.
//!
//! The demuxer only ever sees the streams INSIDE the container it opened, so a sidecar was
//! unreachable on direct play and the track menu hid it (`docs/parity-gaps.md`, "Sidecar
//! (external) subtitle streams are unreachable on the direct-play path"). This is the missing
//! producer: fetch the whole file from PMS once. Plain captions answer [`active`]; ASS/SSA
//! keeps its complete script for [`ass_source`] and the native styled renderer. Styles,
//! overlapping events, drawings and embedded fonts must never pass through plain-text parsing.
//!
//! # Why its own store, and not `SHARED.sub_cues`
//!
//! That store is a WINDOW: `push_subtitle_text` drops everything more than the latest delay + 2 s
//! (32 s) behind the playhead (`player::subtitle_floor_ns`) and caps at 512, because the demuxer
//! refills it as it reads. A sidecar arrives once, whole —
//! 1,000-2,000 cues for a feature — and nothing re-reads it after a backward seek. Plain cues
//! are kept in full, sorted, and looked up by time; a styled script stays intact for libass.
//!
//! # The clock
//!
//! A sidecar's timestamps are file time, and on DIRECT PLAY `playpos_ns` is file time too (the
//! rebase keeps it so across seeks). On a TRANSCODE the server burns the selection into the
//! picture instead, so [`active`] answers nothing while its caller says the session is
//! transcoding — otherwise a direct play that becomes a transcode mid-film (a DTS audio pick)
//! would show the line twice. The fact is PASSED IN rather than read: the playback session is
//! the frame's publication, and the draw that calls this already holds it.
//!
//! The draw asks on the SUBTITLE clock (`player::subtitle_clock_ns`, the playhead less the
//! viewer's timing offset). Because the whole file is here, a sidecar is the one kind of track
//! that can be ADVANCED as well as delayed — [`selected`] is what `player::subtitle_offset_range_ms`
//! reads to offer it -30 s..+30 s, where an embedded track gets a delay only.
//!
//! # Threading
//!
//! [`select`]/[`deselect`]/[`active`] are main-thread calls; the fetch runs on a `task` worker and
//! lands under the one mutex, fenced by a generation so an answer for a pick the viewer has
//! already moved off is dropped rather than installed. One worker drains a single latest-pick
//! slot, so repeated picks cannot accumulate threads or downloads.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cue {
    pub(crate) start_ns: i64,
    pub(crate) end_ns: i64,
    pub(crate) text: String,
}

/// Why nothing is being drawn for a selected sidecar — said ON SCREEN for a few seconds, in the
/// caption's own place, because a picked subtitle that silently shows nothing is
/// indistinguishable from a film with a quiet first minute.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Failure {
    Fetch,
    Empty,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Failure::Fetch => "Couldn't load this subtitle from the server",
            Failure::Empty => "This subtitle file has no readable lines",
        }
    }
}

/// Stream ids belong to a server; the codec controls how that server is asked for the file.
/// Keep all of that identity beside the cache so identical ids on two servers cannot alias.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    server: crate::plex::ServerId,
    stream_id: i64,
    key: String,
    codec: String,
}

enum Content {
    Plain(Vec<Cue>),
    Ass { source: Arc<super::ass::Source>, font_generation: Option<u64> },
}

struct State {
    want: Option<Selection>,
    generation: u64,
    pending: Option<(u64, Selection)>,
    running: bool,
    /// The complete selected file, kept across Off→On so re-picking is instant.
    loaded: Option<(Selection, Content)>,
    failed: Option<(Selection, Failure, Instant)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    want: None,
    generation: 0,
    pending: None,
    running: false,
    loaded: None,
    failed: None,
});

/// How long a [`Failure`] stays on screen.
const FAILURE_SHOWN: Duration = Duration::from_secs(6);
/// A sanity bound on one file; a feature film's SRT is a couple of thousand cues.
const MAX_CUES: usize = 20_000;

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Select the sidecar `stream_id`, fetching `key` from `server` unless that file is already
/// loaded. MAIN THREAD.
pub(crate) fn select(server: crate::plex::ServerId, stream_id: i64, key: String, codec: String) {
    select_with_fetch(server, stream_id, key, codec, |server, key, codec| {
        crate::plex::client_for(server).and_then(|c| c.sidecar_subtitle(key, codec))
    });
}

fn select_with_fetch(
    server: crate::plex::ServerId, stream_id: i64, key: String, codec: String,
    fetch: impl Fn(crate::plex::ServerId, &str, &str) -> Option<Vec<u8>> + Send + 'static,
) {
    if stream_id <= 0 || key.is_empty() {
        deselect();
        return;
    }
    let selection = Selection { server, stream_id, key, codec: codec.to_ascii_lowercase() };
    {
        let mut st = state();
        st.generation = st.generation.wrapping_add(1);
        st.want = Some(selection.clone());
        st.failed = None;
        st.pending = None;
        if matches!(&st.loaded, Some((source, _)) if *source == selection) {
            return;
        }
        st.loaded = None;
        st.pending = Some((st.generation, selection.clone()));
        if st.running { return; }
        st.running = true;
    }
    let spawned = crate::task::spawn_small("sidecar", move || loop {
        let (gen, selection) = {
            let mut st = state();
            let Some(request) = st.pending.take() else {
                st.running = false;
                return;
            };
            request
        };
        let stream_id = selection.stream_id;
        super::log(&format!("sidecar: fetching stream {stream_id}"));
        let body = fetch(selection.server, &selection.key, &selection.codec);
        let outcome = match body {
            None => Err(Failure::Fetch),
            Some(bytes) => decode(bytes, &selection.codec),
        };
        let mut st = state();
        if st.generation != gen {
            continue; // check and publication share the selection/reset lock
        }
        match outcome {
            Ok(content) => {
                match &content {
                    Content::Plain(cues) => super::log(&format!("sidecar: stream {stream_id} ready, {} cues", cues.len())),
                    Content::Ass { .. } => super::log(&format!("sidecar: stream {stream_id} ready, styled ASS")),
                }
                st.loaded = Some((selection, content));
            }
            Err(why) => {
                super::log(&format!("sidecar: stream {stream_id} FAILED ({why:?})"));
                st.failed = Some((selection, why, Instant::now()));
            }
        }
        crate::ui::present::wake_from_worker();
    });
    if !spawned {
        let mut st = state();
        st.running = false;
        st.pending = None;
        st.failed = Some((selection, Failure::Fetch, Instant::now()));
    }
}

/// Stop drawing a sidecar (Off, or an embedded track was picked). The loaded file is KEPT, so
/// turning the same one back on is instant. MAIN THREAD.
pub(crate) fn deselect() {
    let mut st = state();
    st.generation = st.generation.wrapping_add(1);
    st.pending = None;
    st.want = None;
    st.failed = None;
}

/// Is a sidecar the selected subtitle? True from the pick, before its file has arrived — the
/// question is which KIND of track the viewer chose (it decides the timing offset's range,
/// `player::subtitle_offset_range_ms`), not whether a cue is ready.
pub(crate) fn selected() -> bool {
    state().want.is_some()
}

/// Mark `stream_id` as the selected sidecar without fetching anything — for tests of what the
/// selection KIND decides (the timing offset's range), which never draw a cue.
#[cfg(test)]
pub(crate) fn select_without_fetch_for_test(stream_id: i64) {
    let mut st = state();
    st.generation = st.generation.wrapping_add(1);
    st.pending = None;
    st.want = Some(Selection {
        server: crate::plex::ServerId::UNSET, stream_id, key: String::new(), codec: String::new(),
    });
    st.failed = None;
}

/// A new item is starting: nothing of the previous one's may survive. MAIN THREAD.
pub(crate) fn reset() {
    let mut st = state();
    st.generation = st.generation.wrapping_add(1);
    st.pending = None;
    st.want = None;
    st.loaded = None;
    st.failed = None;
}

/// **Honour a sidecar the SERVER already has selected for this part** — picked here in an earlier
/// session, or on another Plex client. The embedded twin is `route::pick_dp_subtitle`, which
/// leaves an external selection off because nothing could render it; now something can.
/// Direct play only (the caller's gate): a transcode start keeps subtitles off, as before.
pub(crate) fn restore_server_selection(server: crate::plex::ServerId, meta: crate::metadata::MetadataView<'_>) -> Option<i64> {
    let item = meta.playing()?;
    if let Some(s) = item.subs.iter().find(|s| s.selected && s.sidecar_renderable()) {
        super::log(&format!("server-selected sidecar subtitle: sid={}", s.id));
        select(server, s.id, s.key.clone(), s.codec.clone());
        return Some(s.id);
    }
    None
}

/// The line to draw at `now_ns`, if a sidecar is selected and this is a direct play.
pub(crate) fn active(now_ns: i64, transcoding: bool) -> Option<String> {
    let st = state();
    if transcoding {
        return None; // a transcode BURNS the selection; drawing it too would double the line
    }
    let want = st.want.as_ref()?;
    if let Some((source, why, at)) = &st.failed {
        if source == want && at.elapsed() < FAILURE_SHOWN {
            return Some(why.message().to_string());
        }
    }
    match &st.loaded {
        Some((source, Content::Plain(cues))) if source == want => cue_at(cues, now_ns).map(|c| c.text.clone()),
        _ => None,
    }
}

/// The selected styled script on a direct play. It is the same immutable source through seeks
/// and Off→On; render requests carry the current clock, so no cue is lost on a backward seek.
pub(crate) fn ass_source(transcoding: bool) -> Option<Arc<super::ass::Source>> {
    if transcoding { return None; }
    let mut st = state();
    let State { want, loaded, .. } = &mut *st;
    let want = want.as_ref()?;
    match loaded {
        Some((selection, Content::Ass { source, font_generation })) if selection == want => {
            // The file can arrive before its movie's demuxer discovers attachments. Preserve
            // both buffers by Arc and revise only when that immutable font publication changes.
            // The font store never acquires the sidecar lock, so this short snapshot has one
            // lock order; no native work runs under either lock.
            let (generation, fonts) = super::ass_source::font_snapshot();
            if *font_generation != Some(generation) {
                let super::ass::Content::Script { bytes, .. } = &source.content else { return None; };
                *source = Arc::new(super::ass::Source {
                    id: source.id, revision: source.revision.wrapping_add(1),
                    content: super::ass::Content::Script { bytes: bytes.clone(), fonts },
                });
                *font_generation = Some(generation);
            }
            Some(source.clone())
        }
        _ => None,
    }
}

fn is_ass_script(text: &str) -> bool {
    text.lines().any(|line| line.trim().eq_ignore_ascii_case("[Events]"))
        && text.lines().any(|line| line.trim_start().get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Dialogue:")))
}

fn decode(bytes: Vec<u8>, codec: &str) -> Result<Content, Failure> {
    if bytes.len() > crate::plex::SIDECAR_MAX_BYTES { return Err(Failure::Empty); }
    let text = String::from_utf8_lossy(&bytes);
    if is_ass_script(&text) {
        // Validate only the presence of events. libass owns the script grammar, including
        // style/format declarations, overlapping dialogue and vector drawing events.
        return Ok(Content::Ass {
            source: Arc::new(super::ass::Source {
                id: super::ass::next_source_id(), revision: 1,
                content: super::ass::Content::Script { bytes: bytes.into(), fonts: Arc::from([]) },
            }),
            font_generation: None,
        });
    }
    // A server conversion or error must not silently turn an ASS selection into plain text.
    if codec.eq_ignore_ascii_case("ass") || codec.eq_ignore_ascii_case("ssa") {
        return Err(Failure::Empty);
    }
    let cues = parse(&bytes);
    if cues.is_empty() { Err(Failure::Empty) } else { Ok(Content::Plain(cues)) }
}

/// The newest-starting cue covering `now_ns` in a list sorted by start — the same "newest wins"
/// rule `active_subtitle` applies to the embedded store. Search the bounded whole-file store:
/// a long-running sign may outlive any number of shorter dialogue cues.
fn cue_at(cues: &[Cue], now_ns: i64) -> Option<&Cue> {
    let after = cues.partition_point(|c| c.start_ns <= now_ns);
    cues[..after]
        .iter()
        .rev()
        .find(|c| now_ns < c.end_ns)
}

// ---- parsing (pure) ---------------------------------------------------------------------------

/// Parse a whole subtitle file into sorted cues. SubRip and WebVTT share one reader (a timing
/// line containing `-->`, text until the next blank line; `,` or `.` before the milliseconds,
/// hours optional). ASS/SSA belongs to the native renderer and is never flattened here.
pub(crate) fn parse(bytes: &[u8]) -> Vec<Cue> {
    if bytes.len() > crate::plex::SIDECAR_MAX_BYTES { return Vec::new(); }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    if is_ass_script(text) { return Vec::new(); }
    let mut cues = parse_srt(text);
    cues.retain(|c| c.end_ns > c.start_ns && !c.text.is_empty());
    for cue in &mut cues {
        if let Some((end, _)) = cue.text.char_indices().nth(4096) {
            cue.text.truncate(end);
        }
    }
    cues.sort_by_key(|c| c.start_ns);
    cues.truncate(MAX_CUES);
    cues
}

fn parse_srt(text: &str) -> Vec<Cue> {
    fn flush(cur: &mut Option<(i64, i64, String)>, out: &mut Vec<Cue>) {
        if let Some((start_ns, end_ns, raw)) = cur.take() {
            out.push(Cue {
                start_ns,
                end_ns,
                text: super::sub_text(raw.as_bytes()),
            });
        }
    }
    let mut out = Vec::new();
    let mut cur: Option<(i64, i64, String)> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some((start, end)) = timing_line(line) {
            flush(&mut cur, &mut out);
            cur = Some((start, end, String::new()));
        } else if line.trim().is_empty() {
            flush(&mut cur, &mut out);
        } else if let Some((_, _, raw)) = cur.as_mut() {
            if !raw.is_empty() {
                raw.push('\n');
            }
            raw.push_str(line);
        }
        // anything else is outside a cue: the counter line, `WEBVTT`, a NOTE — not text
    }
    flush(&mut cur, &mut out);
    out
}

/// `00:01:02,500 --> 00:01:05,000` (SubRip), `01:02.500 --> 01:05.000 line:0` (WebVTT).
fn timing_line(line: &str) -> Option<(i64, i64)> {
    let (a, b) = line.split_once("-->")?;
    let start = timestamp(a.trim())?;
    let end = timestamp(b.split_whitespace().next()?)?;
    Some((start, end))
}

/// `[H+:]MM:SS[,.]fff` → ns. The fraction is read as written (`.5` is half a second, `.50` too).
fn timestamp(s: &str) -> Option<i64> {
    let (clock, frac) = match s.rsplit_once([',', '.']) {
        Some((c, f)) => (c, f),
        None => (s, ""),
    };
    let mut secs: i64 = 0;
    let mut fields = 0;
    for part in clock.split(':') {
        let v: i64 = part.trim().parse().ok().filter(|v| *v >= 0)?;
        secs = secs.checked_mul(60)?.checked_add(v)?;
        fields += 1;
    }
    if !(2..=3).contains(&fields) {
        return None;
    }
    let mut frac_ns: i64 = 0;
    let mut scale: i64 = 100_000_000;
    for ch in frac.chars().take(9) {
        frac_ns += ch.to_digit(10)? as i64 * scale;
        scale /= 10;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn finish_download() {
        let until = Instant::now() + Duration::from_secs(2);
        while state().running && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!state().running, "sidecar worker finished");
    }

    const STYLED_SCRIPT: &[u8] = b"[Script Info]\nScriptType: v4.00+\n[Events]\n\
        Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\pos(300,100)\\c&H0000FF&}SIGN\n";

    #[test]
    fn styled_sidecar_source_survives_seek_and_off_on_but_not_new_items() {
        let _guard = crate::testlock::serial();
        reset();
        finish_download();
        let server = crate::plex::ServerId::from_raw(0);
        select_with_fetch(server, 42, "/library/streams/42".into(), "ass".into(),
            |_, _, _| Some(STYLED_SCRIPT.to_vec()));
        finish_download();
        let first = ass_source(false).expect("native script available");
        assert!(ass_source(true).is_none(), "a server burn silences the native script");
        for clock in [2_000_000_000, 80_000_000_000, 1_000_000_000] {
            assert_eq!(active(clock, false), None, "ASS never enters the plain renderer");
            assert_eq!(ass_source(false).unwrap().id, first.id, "seek keeps the whole script");
        }
        deselect();
        assert!(!selected());
        assert!(ass_source(false).is_none());
        select_with_fetch(server, 42, "/library/streams/42".into(), "ASS".into(),
            |_, _, _| panic!("the same cached script must not be downloaded again"));
        assert!(Arc::ptr_eq(&ass_source(false).unwrap(), &first));
        reset();
        assert!(ass_source(false).is_none(), "new item drops the old source");
    }

    #[test]
    fn styled_sidecar_adopts_font_attachments_that_arrive_after_its_download() {
        let _guard = crate::testlock::serial();
        reset();
        finish_download();
        super::super::ass_source::reset();
        select_with_fetch(crate::plex::ServerId::from_raw(0), 42,
            "/library/streams/42".into(), "ass".into(), |_, _, _| Some(STYLED_SCRIPT.to_vec()));
        finish_download();
        let before = ass_source(false).unwrap();
        let font = super::super::ass::Font { name: "sign.ttf".into(), data: Arc::from(&b"font"[..]) };
        // Fonts belong to the media even when it has no embedded ASS track.
        super::super::ass_source::begin("fixture", Vec::new(), vec![font]);
        let after = ass_source(false).unwrap();
        assert_eq!(after.id, before.id, "the script's identity is stable");
        assert!(after.revision > before.revision, "new fonts invalidate native rasterization");
        let super::super::ass::Content::Script { bytes: old_bytes, fonts: old_fonts } = &before.content else { panic!() };
        let super::super::ass::Content::Script { bytes, fonts } = &after.content else { panic!() };
        assert!(old_fonts.is_empty());
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].name, "sign.ttf");
        assert!(Arc::ptr_eq(bytes, old_bytes), "script bytes must not be recopied per font update");
        assert!(Arc::ptr_eq(fonts, &super::super::ass_source::font_snapshot().1));
        assert!(Arc::ptr_eq(&ass_source(false).unwrap(), &after), "unchanged frames reuse the source");
        reset();
        super::super::ass_source::reset();
    }

    #[test]
    fn identical_stream_ids_on_different_servers_do_not_reuse_the_sidecar() {
        let _guard = crate::testlock::serial();
        reset();
        finish_download();
        let key = "/library/streams/42";
        for server in [0, 1] {
            select_with_fetch(crate::plex::ServerId::from_raw(server), 42, key.into(), "srt".into(),
                move |_, _, _| Some(format!("00:00:01 --> 00:00:03\nserver {server}\n").into_bytes()));
            finish_download();
            assert_eq!(active(2_000_000_000, false), Some(format!("server {server}")));
        }
        reset();
    }

    #[test]
    fn styled_sidecars_never_draw_a_flattened_second_caption() {
        let _guard = crate::testlock::serial();
        reset();
        let until = Instant::now() + Duration::from_secs(2);
        while state().running && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!state().running);
        let script = b"[Script Info]\nScriptType: v4.00+\n[Events]\n\
            Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\pos(300,100)\\c&H0000FF&}SIGN\n";
        select_with_fetch(crate::plex::ServerId::from_raw(0), 42,
            "/library/streams/42.ass".into(), "ass".into(), move |_, _, _| Some(script.to_vec()));
        let until = Instant::now() + Duration::from_secs(2);
        while state().running && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        let plain_caption = active(2_000_000_000, false);
        reset();
        assert_eq!(plain_caption, None, "styled ASS belongs to its native renderer");
    }

    #[test]
    fn sidecar_picks_share_one_worker_and_drop_abandoned_answers() {
        let _guard = crate::testlock::serial();
        reset();
        // A prior route test may have left its now-abandoned worker finishing a no-client
        // result. Let that worker retire before installing this test's controlled transport.
        let until = Instant::now() + Duration::from_secs(2);
        while state().running && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!state().running);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(Mutex::new(release_rx));
        let fetch = {
            let calls = calls.clone();
            move |_: crate::plex::ServerId, key: &str, _: &str| {
                let n = calls.fetch_add(1, Relaxed);
                started_tx.send(()).unwrap();
                if n == 0 { release_rx.lock().unwrap().recv().unwrap(); }
                Some(format!("00:00:01 --> 00:00:03\n{key}\n").into_bytes())
            }
        };
        let sid = crate::plex::ServerId::from_raw(0);
        select_with_fetch(sid, 1, "old".into(), "srt".into(), fetch.clone());
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        reset();
        select_with_fetch(sid, 1, "new".into(), "srt".into(), fetch);
        let parallel = started_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        release_tx.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while active(2_000_000_000, false).as_deref() != Some("new") && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(active(2_000_000_000, false).as_deref(), Some("new"));
        reset();
        assert!(!parallel, "an abandoned fetch must finish before another worker is started");
    }

    #[test]
    fn sidecar_download_refuses_an_oversized_body() {
        let mut body = b"00:00:01 --> 00:00:03\n".to_vec();
        body.resize(4 * 1024 * 1024 + 1, b'x');
        assert!(parse(&body).is_empty(), "oversized sidecars must not allocate a parsed copy");
        assert!(decode(body, "ass").is_err(), "native scripts obey the same download bound");
    }

    #[test]
    fn sidecar_cue_text_is_bounded_before_reaching_the_ui() {
        let body = format!("00:00:01 --> 00:00:03\n{}", "é".repeat(8192));
        let cues = parse(body.as_bytes());
        assert!(cues[0].text.chars().count() <= 4096);
    }

    #[test]
    fn sidecar_long_cue_survives_many_short_overlaps() {
        let mut cues = vec![Cue { start_ns: 0, end_ns: 1000, text: "sign".into() }];
        cues.extend((1..40).map(|n| Cue { start_ns: n, end_ns: n + 1, text: "dialogue".into() }));
        assert_eq!(cue_at(&cues, 100).map(|c| c.text.as_str()), Some("sign"));
    }

    const S: i64 = 1_000_000_000;

    /// The ordinary file: counters, CRLF, a BOM, italics, a two-line cue — and the counter line
    /// must never leak into the text of the cue before it.
    #[test]
    fn a_subrip_file_becomes_clean_sorted_cues() {
        let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i>\r\nthere\r\n\r\n\
                   2\r\n00:01:00,250 --> 00:01:02,000\r\n{\\an8}Second line\r\n";
        let cues = parse(srt.as_bytes());
        assert_eq!(
            cues,
            vec![
                Cue { start_ns: S, end_ns: 3 * S + S / 2, text: "Hello\nthere".into() },
                Cue { start_ns: 60 * S + S / 4, end_ns: 62 * S, text: "Second line".into() },
            ]
        );
    }

    /// PMS is asked for SubRip but may answer in whatever it has. WebVTT differs in three ways
    /// that matter — a header, `.` before the milliseconds with the hours optional, and cue
    /// settings after the end time — and none of them may cost a cue or reach the text.
    #[test]
    fn a_webvtt_file_parses_through_the_same_reader() {
        let vtt = "WEBVTT\n\nNOTE made by hand\n\n01:02.500 --> 01:05.000 line:0 position:50%\nHi\n\n\
                   1:00:00.000 --> 1:00:01.000\nAn hour in\n";
        let cues = parse(vtt.as_bytes());
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start_ns, cues[0].end_ns), (62 * S + S / 2, 65 * S));
        assert_eq!(cues[0].text, "Hi");
        assert_eq!(cues[1].start_ns, 3600 * S);
    }

    #[test]
    fn styled_script_keeps_headers_overrides_and_overlapping_events() {
        let script = b"[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nStyle: Sign,Example\n[Events]\n\
            Dialogue: 0,0:00:01.00,0:00:03.00,Sign,,0,0,0,,{\\pos(300,100)}SIGN\n\
            Dialogue: 1,0:00:01.00,0:00:03.00,Default,,0,0,0,,{\\k20}Dialogue\n";
        for codec in ["ass", "SSA"] {
            let Content::Ass { source, .. } = decode(script.to_vec(), codec).unwrap() else {
                panic!("styled script must remain native ASS");
            };
            let super::super::ass::Content::Script { bytes, .. } = &source.content else { panic!() };
            assert_eq!(bytes.as_ref(), script);
        }
        assert!(parse(script).is_empty(), "the plain parser must not flatten a styled script");
        assert!(decode(b"1\n00:00:01,000 --> 00:00:03,000\nSIGN\n".to_vec(), "ass").is_err(),
            "a converted response must not silently discard styling");
    }

    /// Out-of-order files exist (merged fan subs); lookup is a binary search, so order is owed.
    /// A cue that ends before it starts, or says nothing, is dropped rather than drawn.
    #[test]
    fn cues_are_sorted_and_the_unusable_ones_dropped() {
        let srt = "2\n00:00:10,000 --> 00:00:11,000\nlater\n\n1\n00:00:01,000 --> 00:00:02,000\nsooner\n\n\
                   3\n00:00:05,000 --> 00:00:04,000\nbackwards\n\n4\n00:00:20,000 --> 00:00:21,000\n<i></i>\n";
        let cues = parse(srt.as_bytes());
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["sooner", "later"]);
    }

    /// Garbage in, nothing out — an HTML error page or an image subtitle must not panic and must
    /// not produce cues, because "no cues" is what turns into the on-screen explanation.
    #[test]
    fn a_body_that_is_not_a_subtitle_yields_no_cues() {
        assert!(parse(b"<html><body>501 Not Implemented</body></html>").is_empty());
        assert!(parse(&[0xff, 0xfe, 0x00, 0x01, 0x02]).is_empty());
        assert!(parse(b"").is_empty());
        assert!(parse(b"1\n99:99:99,999 --> nonsense\ntext\n").is_empty());
    }

    /// The lookup is what a SEEK depends on: any position, in any order, answers from the whole
    /// file — the property the windowed embedded store cannot give a sidecar.
    #[test]
    fn a_position_finds_its_cue_wherever_the_playhead_came_from() {
        let cues = parse(
            b"1\n00:00:01,000 --> 00:00:03,000\na\n\n2\n00:00:02,000 --> 00:00:04,000\nb\n\n\
              3\n00:10:00,000 --> 00:10:02,000\nc\n",
        );
        let at = |ns: i64| cue_at(&cues, ns).map(|c| c.text.as_str());
        assert_eq!(at(0), None);
        assert_eq!(at(S + S / 2), Some("a"));
        assert_eq!(at(2 * S + S / 2), Some("b"), "where two overlap, the newer one wins");
        assert_eq!(at(3 * S + S / 2), Some("b"));
        assert_eq!(at(5 * S), None, "a gap is silence");
        assert_eq!(at(601 * S), Some("c"));
        assert_eq!(at(S + S / 2), Some("a"), "…and back again after a backward seek");
        assert_eq!(at(3 * S), Some("b"), "an end time is exclusive");
    }

    #[test]
    fn timestamps_accept_both_separators_and_refuse_nonsense() {
        assert_eq!(timestamp("00:00:01,000"), Some(S));
        assert_eq!(timestamp("00:01.5"), Some(S + S / 2));
        assert_eq!(timestamp("1:02:03.250"), Some(3723 * S + S / 4));
        assert_eq!(timestamp("12"), None, "a bare counter line is not a time");
        assert_eq!(timestamp("a:b:c"), None);
        assert_eq!(timestamp("1:2:3:4"), None);
        assert_eq!(timestamp("-1:00:00,000"), None);
    }
}
