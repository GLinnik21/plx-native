//! **External (sidecar) subtitles on the direct-play path** — the `.srt` beside the film.
//!
//! The demuxer only ever sees the streams INSIDE the container it opened, so a sidecar was
//! unreachable on direct play and the track menu hid it (`docs/parity-gaps.md`, "Sidecar
//! (external) subtitle streams are unreachable on the direct-play path"). This is the missing
//! producer: fetch the whole file from PMS once, parse it, and answer [`active`] from it. The
//! RENDER side is untouched — `ui::player_hud::draw_subtitles` asks here first and
//! `player::active_subtitle` second, so a sidecar cue is wrapped, outlined and lifted clear of the
//! HUD exactly as an embedded one is.
//!
//! # Why its own store, and not `SHARED.sub_cues`
//!
//! That store is a WINDOW: `push_subtitle_text` drops everything more than 2 s behind the playhead
//! and caps at 512, because the demuxer refills it as it reads. A sidecar arrives once, whole —
//! 1,000-2,000 cues for a feature — and nothing re-reads it after a backward seek. So the file is
//! kept in full, sorted, and looked up by time.
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
//! # Threading
//!
//! [`select`]/[`deselect`]/[`active`] are main-thread calls; the fetch runs on a `task` worker and
//! lands under the one mutex, fenced by a generation so an answer for a pick the viewer has
//! already moved off is dropped rather than installed. One worker drains a single latest-pick
//! slot, so repeated picks cannot accumulate threads or downloads.
use std::sync::Mutex;
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

struct State {
    /// The Plex stream id the viewer wants drawn; 0 = no sidecar selected.
    want: i64,
    generation: u64,
    pending: Option<(u64, crate::plex::ServerId, i64, String)>,
    running: bool,
    /// The parsed file, keyed by its stream id. Kept across Off→On so re-picking is instant.
    loaded: Option<(i64, Vec<Cue>)>,
    failed: Option<(i64, Failure, Instant)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    want: 0,
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
pub(crate) fn select(server: crate::plex::ServerId, stream_id: i64, key: String) {
    select_with_fetch(server, stream_id, key, |server, key| {
        crate::plex::client_for(server).and_then(|c| c.sidecar_subtitle(key))
    });
}

fn select_with_fetch(
    server: crate::plex::ServerId, stream_id: i64, key: String,
    fetch: impl Fn(crate::plex::ServerId, &str) -> Option<Vec<u8>> + Send + 'static,
) {
    if stream_id <= 0 || key.is_empty() {
        deselect();
        return;
    }
    {
        let mut st = state();
        st.generation = st.generation.wrapping_add(1);
        st.want = stream_id;
        st.failed = None;
        st.pending = None;
        if matches!(&st.loaded, Some((id, _)) if *id == stream_id) {
            return;
        }
        st.loaded = None;
        st.pending = Some((st.generation, server, stream_id, key));
        if st.running { return; }
        st.running = true;
    }
    let spawned = crate::task::spawn_small("sidecar", move || loop {
        let (gen, server, stream_id, key) = {
            let mut st = state();
            let Some(request) = st.pending.take() else {
                st.running = false;
                return;
            };
            request
        };
        super::log(&format!("sidecar: fetching stream {stream_id}"));
        let body = fetch(server, &key);
        let outcome = match body {
            None => Err(Failure::Fetch),
            Some(bytes) => {
                let cues = parse(&bytes);
                if cues.is_empty() {
                    super::log(&format!("sidecar: {} bytes, no cues parsed", bytes.len()));
                    Err(Failure::Empty)
                } else {
                    Ok(cues)
                }
            }
        };
        let mut st = state();
        if st.generation != gen {
            continue; // check and publication share the selection/reset lock
        }
        match outcome {
            Ok(cues) => {
                super::log(&format!("sidecar: stream {stream_id} ready, {} cues", cues.len()));
                st.loaded = Some((stream_id, cues));
            }
            Err(why) => {
                super::log(&format!("sidecar: stream {stream_id} FAILED ({why:?})"));
                st.failed = Some((stream_id, why, Instant::now()));
            }
        }
    });
    if !spawned {
        let mut st = state();
        st.running = false;
        st.pending = None;
        st.failed = Some((stream_id, Failure::Fetch, Instant::now()));
    }
}

/// Stop drawing a sidecar (Off, or an embedded track was picked). The parsed file is KEPT, so
/// turning the same one back on is instant. MAIN THREAD.
pub(crate) fn deselect() {
    let mut st = state();
    st.generation = st.generation.wrapping_add(1);
    st.pending = None;
    st.want = 0;
    st.failed = None;
}

/// A new item is starting: nothing of the previous one's may survive. MAIN THREAD.
pub(crate) fn reset() {
    let mut st = state();
    st.generation = st.generation.wrapping_add(1);
    st.pending = None;
    st.want = 0;
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
        select(server, s.id, s.key.clone());
        return Some(s.id);
    }
    None
}

/// The line to draw at `now_ns`, if a sidecar is selected and this is a direct play.
pub(crate) fn active(now_ns: i64, transcoding: bool) -> Option<String> {
    let st = state();
    if st.want == 0 || transcoding {
        return None; // a transcode BURNS the selection; drawing it too would double the line
    }
    if let Some((id, why, at)) = st.failed {
        if id == st.want && at.elapsed() < FAILURE_SHOWN {
            return Some(why.message().to_string());
        }
    }
    match &st.loaded {
        Some((id, cues)) if *id == st.want => cue_at(cues, now_ns).map(|c| c.text.clone()),
        _ => None,
    }
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
/// hours optional); an ASS/SSA script is read from its `Dialogue:` events. PMS is ASKED for UTF-8
/// SubRip, but whichever form it actually sent is what gets parsed.
pub(crate) fn parse(bytes: &[u8]) -> Vec<Cue> {
    if bytes.len() > crate::plex::SIDECAR_MAX_BYTES { return Vec::new(); }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let mut cues = if text.contains("[Events]") && text.contains("Dialogue:") {
        parse_ass(text)
    } else {
        parse_srt(text)
    };
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
                text: super::sub_text(raw.as_bytes(), false),
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

/// `Dialogue: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text` — the text is the
/// tenth field and may itself contain commas. Override blocks and `\N` are `sub_text`'s.
fn parse_ass(text: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("Dialogue:") else {
            continue;
        };
        let f: Vec<&str> = rest.splitn(10, ',').collect();
        if f.len() < 10 {
            continue;
        }
        let (Some(start_ns), Some(end_ns)) = (timestamp(f[1].trim()), timestamp(f[2].trim())) else {
            continue;
        };
        out.push(Cue {
            start_ns,
            end_ns,
            text: super::sub_text(f[9].trim_end_matches('\r').as_bytes(), false),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    #[test]
    fn sidecar_picks_share_one_worker_and_drop_abandoned_answers() {
        let _guard = crate::testlock::serial();
        reset();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Arc::new(Mutex::new(release_rx));
        let fetch = {
            let calls = calls.clone();
            move |_: crate::plex::ServerId, key: &str| {
                let n = calls.fetch_add(1, Relaxed);
                started_tx.send(()).unwrap();
                if n == 0 { release_rx.lock().unwrap().recv().unwrap(); }
                Some(format!("00:00:01 --> 00:00:03\n{key}\n").into_bytes())
            }
        };
        let sid = crate::plex::ServerId::from_raw(0);
        select_with_fetch(sid, 1, "old".into(), fetch.clone());
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        reset();
        select_with_fetch(sid, 1, "new".into(), fetch);
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

    /// An ASS script's text is its TENTH field and keeps its own commas; centiseconds are a
    /// fraction, not milliseconds (`.50` is half a second).
    #[test]
    fn an_ass_script_is_read_from_its_dialogue_events() {
        let ass = "[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, \
                   MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:10.50,0:00:12.00,Default,,0,0,0,,{\\i1}Well, yes\\Nindeed\n";
        let cues = parse(ass.as_bytes());
        assert_eq!(
            cues,
            vec![Cue { start_ns: 10 * S + S / 2, end_ns: 12 * S, text: "Well, yes\nindeed".into() }]
        );
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
