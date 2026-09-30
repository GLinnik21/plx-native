//! `imgtrace` — the per-image timeline of the poster pipeline, armed by `/tmp/plxnative-imgtrace`
//! (`dev::scenarios::imgtrace_armed`). Inert unless armed; without `devtriggers` the flag is
//! `false` at compile time and every entry point returns at its first line.
//!
//! **What it answers.** A tile that draws its placeholder and then its picture looks the same to a
//! viewer whether (a) the picture is simply arriving for the first time, or (b) the picture WAS on
//! screen and went away. The trace keeps one record per drawn identity — `(server, path, w, h,
//! png)`, the arguments a draw resolves with, hashed to an opaque 8-hex-digit id (never a URL, a
//! path or a token) — and writes two kinds of line:
//!
//! ```text
//! imgtrace: k=1a2b3c4d shown n=1 frames=9 ms=141 probes=4 maxgap=5f unknown=1 moving=0 why=declined_new:1,loading:2 claim=+1f/+16ms worker=+17ms disk=hit decode=+29ms handoff=+3f draw=+9f after=first
//! imgtrace: k=1a2b3c4d HIDDEN n=1 shown_frames=212 gap=1f cause=token_gen
//! ```
//!
//! `shown` closes a placeholder episode: every offset is from the episode's FIRST draw probe;
//! `unknown`/`moving` count the probes the card-motion gate declined before the claim; `after`
//! names what ended the previous episode (`first` for a first appearance). `HIDDEN` is the true
//! blink: a draw that found no texture for an identity whose previous draw had one, with the last
//! recorded loss (`evicted`, `refused`, `recycled`, `token_gen`, `cache_gen`, `key_changed`,
//! `failed`) or `unknown`, and `gap`, the frames since that identity was last drawn — a tile that
//! scrolled away and came back has a gap, a blink in place has `gap=1f`.
//!
//! Rate: at most two lines per episode per identity, and [`LINE_CAP`] for the whole process.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use super::PT_CAP;

/// Whole-process ceiling on emitted lines, so a thrashing screen cannot flood the event log.
pub(super) const LINE_CAP: u32 = 4000;

/// The opaque identity of a drawn image: what the draw asked for, never the built request.
pub(super) fn draw_id(srv: u16, path: &str, w: i32, h: i32, png: bool) -> u64 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    (srv, path, w, h, png).hash(&mut s);
    s.finish()
}

/// The opaque identity of the built request (the source's store key).
pub(super) fn key_id(srv: u16, key: &[u8]) -> u64 {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    (srv, key).hash(&mut s);
    s.finish()
}

/// Why a probe that missed did not claim, as the card-motion gate saw it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Gate {
    Unknown,
    Moving,
    Open,
}

#[derive(Default)]
struct Tile {
    key: u64,
    /// First probe of the current placeholder episode.
    start: Option<(u64, u64)>,
    unknown: u32,
    moving: u32,
    /// Textureless draws of this episode — with `frames`, how often the tile was drawn at all.
    probes: u32,
    /// The longest run of frames this episode in which the tile was not drawn at all: a page
    /// held as an image (a transition's snapshot) draws no cards, so it requests nothing.
    max_gap: u64,
    /// What the store answered each textureless probe (`lookup`'s return path), counted.
    why: Vec<(&'static str, u32)>,
    claim: Option<(u64, u64)>,
    worker_ms: Option<u64>,
    disk: Option<&'static str>,
    decoded_ms: Option<u64>,
    handoff: Option<u64>,
    /// Frame + ms the current shown episode began, if the last probe had a texture.
    shown: Option<(u64, u64)>,
    last_probe: u64,
    loss: Option<&'static str>,
    episodes: u32,
}

/// The pure state machine. Every method takes the frame serial and the millisecond clock
/// explicitly, so a host test drives it without the store, the clock or the log.
#[derive(Default)]
pub(super) struct Tracker {
    tiles: HashMap<u64, Tile>,
    /// slot index → (draw id, slot generation) of the claim that last bound it.
    slots: Vec<Option<(u64, u32)>>,
    lines: u32,
    suppressed: u32,
}

impl Tracker {
    fn emit(&mut self, out: &mut Vec<String>, line: String) {
        if self.lines < LINE_CAP {
            self.lines += 1;
            out.push(line);
        } else {
            if self.suppressed == 0 {
                out.push(format!("imgtrace: line cap {LINE_CAP} reached; further lines suppressed"));
            }
            self.suppressed += 1;
        }
    }

    fn bound(&mut self, slot: usize, gen: u32) -> Option<&mut Tile> {
        let (id, g) = (*self.slots.get(slot)?)?;
        if g != gen {
            return None;
        }
        self.tiles.get_mut(&id)
    }

    /// A draw probe of `id` (built request `key`) on `frame`: `shown` = it resolved a texture.
    #[cfg(test)]
    pub(super) fn probe(&mut self, id: u64, key: u64, shown: bool, gate: Gate, frame: u64, ms: u64) -> Vec<String> {
        self.probe_why(id, key, shown, gate, "", frame, ms)
    }

    /// [`Tracker::probe`] with the store's answer (`why`, empty when unknown).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn probe_why(&mut self, id: u64, key: u64, shown: bool, gate: Gate, why: &'static str,
        frame: u64, ms: u64) -> Vec<String> {
        let mut out = Vec::new();
        let t = self.tiles.entry(id).or_default();
        if t.key != 0 && t.key != key {
            t.loss = Some("key_changed");
        }
        t.key = key;
        let gap = frame.saturating_sub(t.last_probe);
        t.last_probe = frame;
        match (t.shown, shown) {
            (Some(_), true) | (None, false) => {}
            (Some((f0, _)), false) => {
                t.episodes += 1;
                let line = format!(
                    "imgtrace: k={:08x} HIDDEN n={} shown_frames={} gap={}f cause={}",
                    id as u32,
                    t.episodes,
                    frame.saturating_sub(f0),
                    gap,
                    t.loss.unwrap_or("unknown"),
                );
                // A re-claim made by THIS probe (lookup runs before the trace sees the draw) belongs
                // to the new episode; the old episode's stages do not.
                let fresh = t.claim.filter(|c| c.0 >= f0).is_some();
                let old = std::mem::take(t);
                *t = Tile { key, last_probe: frame, loss: old.loss, episodes: old.episodes, ..Tile::default() };
                if fresh {
                    (t.claim, t.worker_ms, t.disk, t.decoded_ms, t.handoff) =
                        (old.claim, old.worker_ms, old.disk, old.decoded_ms, old.handoff);
                }
                self.emit(&mut out, line);
            }
            (None, true) => {
                let (f0, m0) = t.start.unwrap_or((frame, ms));
                let rel_f = |f: Option<u64>| f.map_or("-".to_string(), |f| format!("+{}f", f.saturating_sub(f0)));
                let rel_ms = |m: Option<u64>| m.map_or("-".to_string(), |m| format!("+{}ms", m.saturating_sub(m0)));
                let line = format!(
                    "imgtrace: k={:08x} shown n={} frames={} ms={} probes={} maxgap={}f unknown={} moving={} why={} claim={}/{} worker={} disk={} decode={} handoff={} draw=+{}f after={}",
                    id as u32,
                    t.episodes + 1,
                    frame.saturating_sub(f0),
                    ms.saturating_sub(m0),
                    t.probes,
                    t.max_gap,
                    t.unknown,
                    t.moving,
                    if t.why.is_empty() { "-".to_string() } else {
                        t.why.iter().map(|(w, n)| format!("{w}:{n}")).collect::<Vec<_>>().join(",")
                    },
                    rel_f(t.claim.map(|c| c.0)),
                    rel_ms(t.claim.map(|c| c.1)),
                    rel_ms(t.worker_ms),
                    t.disk.unwrap_or("-"),
                    rel_ms(t.decoded_ms),
                    rel_f(t.handoff),
                    frame.saturating_sub(f0),
                    if t.episodes == 0 { "first" } else { t.loss.unwrap_or("unknown") },
                );
                t.shown = Some((frame, ms));
                t.loss = None;
                self.emit(&mut out, line);
            }
        }
        let t = self.tiles.get_mut(&id).expect("inserted above");
        if !shown {
            if t.start.is_some() {
                t.max_gap = t.max_gap.max(gap);
            }
            t.start.get_or_insert((frame, ms));
            t.probes += 1;
            if !why.is_empty() {
                match t.why.iter_mut().find(|(w, _)| *w == why) {
                    Some((_, n)) => *n += 1,
                    None => t.why.push((why, 1)),
                }
            }
            if t.claim.is_none() {
                match gate {
                    Gate::Unknown => t.unknown += 1,
                    Gate::Moving => t.moving += 1,
                    Gate::Open => {}
                }
            }
        }
        out
    }

    /// `slot` (now at generation `gen`) was claimed for `id`, fresh or re-armed.
    pub(super) fn claim(&mut self, id: u64, slot: usize, gen: u32, frame: u64, ms: u64) {
        if self.slots.len() < PT_CAP {
            self.slots.resize(PT_CAP, None);
        }
        if let Some(s) = self.slots.get_mut(slot) {
            *s = Some((id, gen));
        }
        let t = self.tiles.entry(id).or_default();
        t.claim = Some((frame, ms));
        t.worker_ms = None;
        t.disk = None;
        t.decoded_ms = None;
        t.handoff = None;
    }

    pub(super) fn worker_start(&mut self, slot: usize, gen: u32, ms: u64) {
        if let Some(t) = self.bound(slot, gen) { t.worker_ms = Some(ms); }
    }
    pub(super) fn disk(&mut self, slot: usize, gen: u32, what: &'static str) {
        if let Some(t) = self.bound(slot, gen) { t.disk.get_or_insert(what); }
    }
    pub(super) fn decoded(&mut self, slot: usize, gen: u32, ms: u64) {
        if let Some(t) = self.bound(slot, gen) { t.decoded_ms = Some(ms); }
    }
    pub(super) fn handoff(&mut self, slot: usize, gen: u32, frame: u64) {
        if let Some(t) = self.bound(slot, gen) { t.handoff = Some(frame); }
    }
    /// The source lost `slot`'s picture for `cause`. Recorded against whatever drew it; the next
    /// textureless draw reports it.
    pub(super) fn lost(&mut self, slot: usize, gen: u32, cause: &'static str) {
        if let Some(t) = self.bound(slot, gen) { t.loss = Some(cause); }
    }
    /// A loss known by identity rather than slot (a generation mismatch found at claim time).
    pub(super) fn lost_id(&mut self, id: u64, cause: &'static str) {
        if let Some(t) = self.tiles.get_mut(&id) {
            if t.shown.is_some() || t.episodes > 0 { t.loss = Some(cause); }
        }
    }
}

struct Global {
    t: Tracker,
    frame: u64,
}
static TRACE: Mutex<Option<Global>> = Mutex::new(None);

std::thread_local! {
    /// MAIN thread: the draw id the probe/warm now running is for. `lookup` reads it at claim time.
    static CURRENT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// MAIN thread: which return path the running `lookup` took, for the probe that called it.
    static OUTCOME: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

#[inline]
pub(super) fn armed() -> bool {
    crate::dev::scenarios::imgtrace_armed()
}

fn with<R>(f: impl FnOnce(&mut Tracker, u64, u64) -> R) -> R {
    let mut g = TRACE.lock().unwrap_or_else(|e| e.into_inner());
    let g = g.get_or_insert_with(|| Global { t: Tracker::default(), frame: 0 });
    // The frame budget's performance counter (`diag::heartbeat::now_us`): monotonic, callable from
    // the workers that report stages (`app::clock` is the main thread's frame time), and the
    // instrument clock `app/` already reads in place of `Instant`. Every line prints differences
    // only, so its origin does not matter.
    let ms = crate::diag::heartbeat::now_us() / 1000;
    let frame = g.frame;
    f(&mut g.t, frame, ms)
}

fn log_all(lines: Vec<String>) {
    for l in lines {
        crate::log(&l);
    }
}

pub(super) fn begin_frame() {
    if !armed() { return; }
    with(|_, _, _| ());
    if let Some(g) = TRACE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() { g.frame += 1; }
}
pub(super) fn set_current(id: u64) {
    CURRENT.with(|c| c.set(id));
    OUTCOME.with(|c| c.set(""));
}
pub(super) fn outcome(why: &'static str) {
    if armed() { OUTCOME.with(|c| c.set(why)); }
}
pub(super) fn current() -> u64 {
    CURRENT.with(|c| c.get())
}
pub(super) fn probe(id: u64, key: u64, shown: bool, gate: Gate) {
    if !armed() { return; }
    let why = OUTCOME.with(|c| c.replace(""));
    let lines = with(|t, f, ms| t.probe_why(id, key, shown, gate, why, f, ms));
    log_all(lines);
}
pub(super) fn claim(id: u64, slot: usize, gen: u32) {
    if !armed() || id == 0 { return; }
    with(|t, f, ms| t.claim(id, slot, gen, f, ms));
}
pub(super) fn worker_start(slot: usize, gen: u32) {
    if !armed() { return; }
    with(|t, _, ms| t.worker_start(slot, gen, ms));
}
pub(super) fn disk(slot: usize, gen: u32, what: &'static str) {
    if !armed() { return; }
    with(|t, _, _| t.disk(slot, gen, what));
}
pub(super) fn decoded(slot: usize, gen: u32) {
    if !armed() { return; }
    with(|t, _, ms| t.decoded(slot, gen, ms));
}
pub(super) fn handoff(slot: usize, gen: u32) {
    if !armed() { return; }
    with(|t, f, _| t.handoff(slot, gen, f));
}
pub(super) fn lost(slot: usize, gen: u32, cause: &'static str) {
    if !armed() { return; }
    with(|t, _, _| t.lost(slot, gen, cause));
}
pub(super) fn lost_id(id: u64, cause: &'static str) {
    if !armed() || id == 0 { return; }
    with(|t, _, _| t.lost_id(id, cause));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_appearance_reports_every_stage_relative_to_the_first_probe() {
        let mut t = Tracker::default();
        assert!(t.probe(7, 70, false, Gate::Unknown, 10, 1000).is_empty());
        assert!(t.probe(7, 70, false, Gate::Open, 11, 1016).is_empty());
        t.claim(7, 3, 1, 11, 1016);
        t.worker_start(3, 1, 1020);
        t.disk(3, 1, "hit");
        t.decoded(3, 1, 1040);
        t.handoff(3, 1, 13);
        assert!(t.probe(7, 70, false, Gate::Open, 13, 1050).is_empty());
        let l = t.probe(7, 70, true, Gate::Open, 14, 1066);
        assert_eq!(l.len(), 1);
        assert_eq!(
            l[0],
            "imgtrace: k=00000007 shown n=1 frames=4 ms=66 probes=3 maxgap=2f unknown=1 moving=0 why=- claim=+1f/+16ms worker=+20ms disk=hit decode=+40ms handoff=+3f draw=+4f after=first"
        );
        // steady state writes nothing
        assert!(t.probe(7, 70, true, Gate::Open, 15, 1082).is_empty());
    }

    #[test]
    fn a_picture_that_goes_away_in_place_is_reported_with_its_cause() {
        let mut t = Tracker::default();
        t.probe(9, 90, false, Gate::Open, 1, 0);
        t.claim(9, 0, 5, 1, 0);
        t.probe(9, 90, true, Gate::Open, 2, 16);
        t.probe(9, 90, true, Gate::Open, 3, 32);
        t.lost(0, 5, "evicted");
        let l = t.probe(9, 90, false, Gate::Open, 4, 48);
        assert_eq!(l, vec!["imgtrace: k=00000009 HIDDEN n=1 shown_frames=2 gap=1f cause=evicted".to_string()]);
        t.claim(9, 0, 6, 4, 48);
        let l = t.probe(9, 90, true, Gate::Open, 6, 80);
        assert!(l[0].contains(" n=2 frames=2 ") && l[0].ends_with("after=evicted"), "{}", l[0]);
    }

    /// The shape the simulator recorded on a warm launch (2026-09-30): the page is captured on
    /// frame 6 (placement unknown → declined), held as an image until frame 52, and only then
    /// drawn live and claimed. `maxgap` is what names the held page.
    #[test]
    fn a_page_held_as_an_image_shows_as_one_long_gap_before_the_claim() {
        let mut t = Tracker::default();
        t.probe_why(1, 10, false, Gate::Unknown, "declined_new", 6, 77);
        t.probe_why(1, 10, false, Gate::Unknown, "declined_new", 52, 841);
        t.claim(1, 0, 1, 53, 857);
        t.probe_why(1, 10, false, Gate::Open, "", 53, 857);
        t.handoff(0, 1, 54);
        let l = t.probe_why(1, 10, true, Gate::Open, "ready", 54, 875);
        assert!(l[0].contains(" frames=48 ms=798 probes=3 maxgap=46f unknown=2 moving=0 why=declined_new:2 claim=+47f/+780ms"), "{}", l[0]);
    }

    /// `lookup` runs before the trace sees the draw, so the probe that discovers a lost picture
    /// may already have re-claimed it. That claim belongs to the new episode.
    #[test]
    fn a_reclaim_by_the_probe_that_finds_the_loss_is_kept_for_the_next_episode() {
        let mut t = Tracker::default();
        t.claim(2, 1, 1, 1, 0);
        t.probe(2, 20, true, Gate::Open, 2, 16);
        t.lost(1, 1, "evicted");
        t.claim(2, 1, 1, 9, 144);
        t.probe(2, 20, false, Gate::Open, 9, 144);
        t.decoded(1, 1, 150);
        let l = t.probe(2, 20, true, Gate::Open, 10, 160);
        assert!(l[0].contains("claim=+0f/+0ms") && l[0].contains("decode=+6ms"), "{}", l[0]);
    }

    #[test]
    fn a_stale_slot_generation_cannot_charge_another_identity() {
        let mut t = Tracker::default();
        t.probe(1, 10, false, Gate::Open, 1, 0);
        t.claim(1, 0, 1, 1, 0);
        t.probe(1, 10, true, Gate::Open, 2, 16);
        // slot 0 recycled to identity 2 at gen 2; a late loss for gen 1 must not reach either
        t.claim(2, 0, 2, 3, 32);
        t.lost(0, 1, "evicted");
        let l = t.probe(1, 10, false, Gate::Open, 4, 48);
        assert!(l[0].ends_with("cause=unknown"), "{}", l[0]);
    }

    #[test]
    fn a_changed_request_for_the_same_draw_is_named() {
        let mut t = Tracker::default();
        t.probe(4, 40, false, Gate::Open, 1, 0);
        t.probe(4, 40, true, Gate::Open, 2, 16);
        let l = t.probe(4, 41, false, Gate::Open, 3, 32);
        assert!(l[0].ends_with("cause=key_changed"), "{}", l[0]);
    }

    #[test]
    fn a_generation_loss_is_only_charged_to_an_identity_that_was_shown() {
        let mut t = Tracker::default();
        t.probe(5, 50, false, Gate::Open, 1, 0);
        t.lost_id(5, "token_gen");
        let l = t.probe(5, 50, true, Gate::Open, 2, 16);
        assert!(l[0].ends_with("after=first"), "{}", l[0]);
        t.lost_id(5, "token_gen");
        let l = t.probe(5, 50, false, Gate::Open, 3, 32);
        assert!(l[0].ends_with("gap=1f cause=token_gen"), "{}", l[0]);
    }

    #[test]
    fn the_line_cap_bounds_output_and_says_so_once() {
        let mut t = Tracker { lines: LINE_CAP - 1, ..Tracker::default() };
        let mut all = Vec::new();
        for f in 0..6u64 {
            all.extend(t.probe(3, 30, f % 2 == 1, Gate::Open, f, f * 16));
        }
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all[1].contains("line cap"));
    }
}
