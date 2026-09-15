//! Background trailer preview. Shares the engine, not the session services: no PlayQueue, no
//! timeline reporter, no scrobble. Watch-state writes are the promise that is kept. `/decision`
//! still creates Activity, which is a server write and is not a watch-state write.
//!
//! **Interaction, once the picture is up and the viewer presses UP.** The detail page stays
//! mounted. Chrome fades to zero. There is no route change and the player route never adopts the
//! engine, so there is no HUD, scrubber, subtitle, or track menu. OK still activates whatever
//! control was focused (Play starts the feature, after this session is stopped). BACK collapses
//! the chrome and stays on the page. A second dwell on an item that has a trailer starts from
//! the beginning again. None of the cache facts is a replay suppressor.
//!
//! Sound stays on. The bound Starfish surface has no mute. The Settings toggle is the only
//! sound control, and that is a platform limit.
//!
//! **Cycle budget is source arithmetic, not a measured leak.** Spike 0a never produced a delta.
//! `sf_load` keeps a 64 KiB slot (`src/starfish.c`) and the conservative RSS headroom cited by
//! the plan is about 958 KiB (`ui/frame/render_set.rs`). 958 / 64 is 14.96, so the ceiling is 14
//! admitted Loads, under the ratio rather than on it. Do not quote 14 as a television result.

use crate::plex::ServerId;

/// Admitted Loads per process, from the source arithmetic above. Not a device measurement.
pub(crate) const CYCLE_BUDGET: u32 = 14;

/// How long the hero must sit still before a preview is requested. Was 4.5 — cut to 2.0 to make
/// autoplay feel responsive (Apple TV/Netflix hover-preview territory), matching a browsing pause
/// rather than a long, deliberate stop.
///
/// **This is the documented fallback of a two-option design, not the option that was fully
/// investigated.** `docs/trailer-ux-plan.md` §2.1 asks whether the felt latency (`dwell + Load
/// time`) can be cut further by decoupling when the fetch STARTS from when the frame is REVEALED —
/// i.e. start `request_preview` on a short fetch-commit threshold while holding the screen's Idle
/// presentation for a separate, longer minimum reveal delay. That mechanism was not implemented:
/// it needs real device data to know whether a short fetch-commit threshold actually reduces
/// false-starts against `CYCLE_BUDGET` or merely spends the same 14 cycles faster on browsed-past
/// items (the cost the plan's own review flagged and did not resolve on paper). A single dwell
/// timer is the safe, already-understood mechanism; if the investigation above is carried out and
/// finds the two-timer approach worth the complexity, it replaces this constant rather than adding
/// a parallel path — nothing else in the trailer UI depends on which mechanism sets `view.picture`.
pub(crate) const DWELL_S: f32 = 2.0;

/// Scroll, in px, at which the opaque cover is fully up and the plane is released.
pub(crate) const COVER_SCROLL: f32 = 160.0;

/// Per-item negative facts. The breaker is process-wide and is not stored here, so evicting an
/// item cannot clear it and a breaker-open lookup is not mistaken for "this trailer refused".
const CACHE_CAP: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fact {
    NoExtra,
    RefusedDirect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Fetching,
    Loading,
    Abandoning,
    Binding,
    Playing,
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    Accepted,
    Disabled,
    Cached(Fact),
    BreakerOpen,
    BudgetSpent,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    /// Art texture alpha over the plane. 1 is a still, 0 is picture only.
    pub art: f32,
    /// Meta line and synopsis. Logo, title and the control row do not use this.
    pub prose: f32,
    /// Raised field strength. 1 until a picture is bound, then [`crate::ui::landing_hero::PREVIEW_FIELD`].
    pub field: f32,
    /// True once a frame has been presented, so hero chrome must not sample the framebuffer.
    pub picture: bool,
    pub playing: bool,
}

impl View {
    pub(crate) const STILL: Self = Self {
        art: 1.0,
        prose: 1.0,
        field: 1.0,
        picture: false,
        playing: false,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    sid: u32,
    rk: String,
    fact: Fact,
}

/// Host-testable machine. The app loop drives it. Tests drive it the same way, with no Starfish.
#[derive(Debug)]
pub(crate) struct Machine {
    phase: Phase,
    cycles: u32,
    breaker: bool,
    nops: u32,
    cache: Vec<Entry>,
    admitted: bool,
    started_ms: u32,
    picture_ms: Option<u32>,
    last_num: u32,
    key: Option<(u32, String)>,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            cycles: 0,
            breaker: false,
            nops: 0,
            cache: Vec::new(),
            admitted: false,
            started_ms: 0,
            picture_ms: None,
            last_num: 0,
            key: None,
        }
    }
}

impl Machine {
    #[cfg(test)]
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(test)]
    pub(crate) fn breaker_open(&self) -> bool {
        self.breaker
    }

    #[cfg(test)]
    pub(crate) fn cycles(&self) -> u32 {
        self.cycles
    }

    /// A second dwell is allowed. Cache hits refuse to start, they do not suppress replay of an
    /// item that actually has a playable extra.
    pub(crate) fn start(&mut self, sid: ServerId, rk: &str, now_ms: u32, enabled: bool) -> Start {
        if !enabled {
            self.nops += 1;
            return Start::Disabled;
        }
        if self.breaker {
            self.nops += 1;
            return Start::BreakerOpen;
        }
        if self.cycles >= CYCLE_BUDGET {
            self.nops += 1;
            return Start::BudgetSpent;
        }
        if let Some(fact) = self.fact(sid, rk) {
            self.nops += 1;
            return Start::Cached(fact);
        }
        if self.phase != Phase::Idle {
            return Start::Busy;
        }
        self.phase = Phase::Fetching;
        self.admitted = false;
        self.picture_ms = None;
        self.last_num = 0;
        self.started_ms = now_ms;
        self.key = Some((u32::from(sid.raw()), rk.to_owned()));
        Start::Accepted
    }

    pub(crate) fn fact(&self, sid: ServerId, rk: &str) -> Option<Fact> {
        let sid = u32::from(sid.raw());
        self.cache
            .iter()
            .find(|e| e.sid == sid && e.rk == rk)
            .map(|e| e.fact)
    }

    pub(crate) fn remember(&mut self, sid: ServerId, rk: &str, fact: Fact) {
        let sid = u32::from(sid.raw());
        if let Some(existing) = self.cache.iter_mut().find(|e| e.sid == sid && e.rk == rk) {
            existing.fact = fact;
            return;
        }
        if self.cache.len() >= CACHE_CAP {
            self.cache.remove(0);
        }
        self.cache.push(Entry {
            sid,
            rk: rk.to_owned(),
            fact,
        });
    }

    /// The Load was admitted. Counts against the budget. Does not arm the breaker.
    pub(crate) fn admit(&mut self) {
        if self.phase == Phase::Fetching || self.phase == Phase::Loading {
            self.phase = Phase::Loading;
            if !self.admitted {
                self.admitted = true;
                self.cycles = self.cycles.saturating_add(1);
            }
        }
    }

    /// `sf_load` refused while a slot was still live. Expected under Abandoning. Not a failure.
    pub(crate) fn refuse_admission(&mut self) {
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    pub(crate) fn refuse_direct(&mut self, sid: ServerId, rk: &str) {
        self.remember(sid, rk, Fact::RefusedDirect);
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    pub(crate) fn no_extra(&mut self, sid: ServerId, rk: &str) {
        self.remember(sid, rk, Fact::NoExtra);
        self.nops += 1;
        self.phase = Phase::Idle;
        self.key = None;
    }

    /// An admitted Load that then failed. This is what arms the breaker. An admission refusal must
    /// not come through here.
    pub(crate) fn fail_admitted(&mut self, num: u32) {
        if self.admitted && !self.breaker {
            self.breaker = true;
        }
        self.last_num = num;
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    /// In-flight Load is left to land unbound. The join waits until the media thread has returned.
    pub(crate) fn abandon(&mut self) {
        if matches!(self.phase, Phase::Fetching | Phase::Loading | Phase::Binding) {
            self.phase = Phase::Abandoning;
        } else if self.phase != Phase::Idle {
            self.phase = Phase::Stopping;
        }
    }

    pub(crate) fn bound(&mut self) {
        if self.phase == Phase::Loading {
            self.phase = Phase::Binding;
        }
    }

    /// First presented frame. Returns the `preview=` line once.
    pub(crate) fn picture(&mut self, now_ms: u32) -> Option<String> {
        if self.picture_ms.is_some() {
            return None;
        }
        if !matches!(self.phase, Phase::Loading | Phase::Binding | Phase::Playing) {
            return None;
        }
        self.phase = Phase::Playing;
        self.picture_ms = Some(now_ms);
        Some(self.line(now_ms.saturating_sub(self.started_ms)))
    }

    pub(crate) fn eos(&mut self) {
        if self.phase == Phase::Playing {
            self.phase = Phase::Stopping;
        }
    }

    pub(crate) fn stopped(&mut self) {
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    pub(crate) fn view(&self) -> View {
        let picture = self.picture_ms.is_some() && self.phase == Phase::Playing;
        if !picture {
            return View::STILL;
        }
        View {
            art: 0.0,
            prose: 0.0,
            field: crate::ui::landing_hero::PREVIEW_FIELD,
            picture: true,
            playing: true,
        }
    }

    fn line(&self, dwell_ms: u32) -> String {
        format!(
            "preview= dwell_ms={dwell_ms} nop={} num={}",
            self.nops, self.last_num
        )
    }
}

/// Direct-play only. A transcode, remux, or adaptive decision is a refusal, which is how a relay
/// link (policy denies direct play) is refused without a second check.
pub(crate) fn accepts_direct_play(direct_play: bool, part_nonempty: bool, adaptive: bool) -> bool {
    direct_play && part_nonempty && !adaptive
}

/// Defer the media join while an in-flight Load has not returned. Joining it on the main thread
/// is the stall Abandoning exists to avoid.
pub(crate) fn defer_media_join(loading: bool, thread_finished: bool) -> bool {
    loading && !thread_finished
}

pub(crate) fn enabled() -> bool {
    !crate::dev::flag("nopreview") && crate::plex::session::peek().trailer_autoplay()
}

fn slot() -> &'static std::sync::Mutex<Machine> {
    static MACHINE: std::sync::OnceLock<std::sync::Mutex<Machine>> = std::sync::OnceLock::new();
    MACHINE.get_or_init(|| std::sync::Mutex::new(Machine::default()))
}

fn with_mut<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub(crate) fn view() -> View {
    with_mut(|m| m.view())
}

pub(crate) fn blocked(sid: ServerId, rk: &str) -> bool {
    with_mut(|m| {
        !enabled() || m.breaker || m.cycles >= CYCLE_BUDGET || m.fact(sid, rk).is_some()
    })
}

pub(crate) fn occupies() -> bool {
    with_mut(|m| m.phase != Phase::Idle)
}

/// True when a preview session is the one the engine must not join yet.
pub(crate) fn abandoning() -> bool {
    with_mut(|m| m.phase == Phase::Abandoning)
}

pub(crate) fn request_start(sid: ServerId, rk: &str, now_ms: u32) -> Start {
    with_mut(|m| m.start(sid, rk, now_ms, enabled()))
}

pub(crate) fn note_no_extra(sid: ServerId, rk: &str) {
    with_mut(|m| m.no_extra(sid, rk));
}

pub(crate) fn note_refused_direct(sid: ServerId, rk: &str) {
    with_mut(|m| m.refuse_direct(sid, rk));
}

pub(crate) fn note_admitted() {
    with_mut(|m| m.admit());
}

pub(crate) fn note_admission_refused() {
    with_mut(|m| m.refuse_admission());
}

pub(crate) fn note_failed(num: u32) {
    with_mut(|m| m.fail_admitted(num));
}

pub(crate) fn note_abandon() {
    with_mut(|m| m.abandon());
}

pub(crate) fn note_stopped() {
    with_mut(|m| m.stopped());
}

pub(crate) fn note_picture(now_ms: u32) {
    if let Some(line) = with_mut(|m| m.picture(now_ms)) {
        crate::player::log(&line);
    }
}

pub(crate) fn note_eos() {
    with_mut(|m| m.eos());
}

/// After the engine pump. Finishes an abandoned Load once the media thread has returned, logs
/// the first picture, and arms the breaker if an admitted Load failed before any frame.
pub(crate) fn after_pump(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    now_ms: u32,
) {
    if !crate::route::is_preview(ps) && !occupies() {
        return;
    }
    let failed = super::SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire);
    if abandoning() {
        let finished = pa
            .engine()
            .and_then(|e| e.load_th.as_ref())
            .is_none_or(|t| t.is_finished());
        if finished {
            super::engine::stop_bufferfeed(ps, pa);
            note_stopped();
            crate::route::clear_preview(ps);
        }
        return;
    }
    if crate::player::seen_frame() && occupies() {
        note_picture(now_ms);
    } else if occupies() {
        let loaded = pa.engine().is_some_and(|eng| eng.stage >= super::shared::Stage::Playing);
        if loaded {
            with_mut(|m| m.bound());
        }
    }
    if crate::player::ended() && occupies() {
        note_eos();
        super::engine::stop_bufferfeed(ps, pa);
        note_stopped();
        crate::route::clear_preview(ps);
        return;
    }
    if failed && occupies() && !crate::player::seen_frame() {
        note_failed(601);
        if pa.is_live() {
            super::engine::stop_bufferfeed(ps, pa);
        }
        crate::route::clear_preview(ps);
    }
}

/// Stop a live preview. A Load that has not returned is abandoned rather than joined.
pub(crate) fn halt(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    if !crate::route::is_preview(ps) && !occupies() {
        return;
    }
    note_abandon();
    super::engine::stop_bufferfeed(ps, pa);
    if !pa.is_live() {
        note_stopped();
        crate::route::clear_preview(ps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> ServerId {
        ServerId::from_raw(3)
    }

    #[test]
    fn every_phase_is_reachable_and_a_leave_during_load_abandons() {
        let mut m = Machine::default();
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.start(sid(), "rk", 1000, true), Start::Accepted);
        assert_eq!(m.phase(), Phase::Fetching);
        m.admit();
        assert_eq!(m.phase(), Phase::Loading);
        assert_eq!(m.cycles(), 1);
        m.abandon();
        assert_eq!(m.phase(), Phase::Abandoning);
        m.stopped();
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.start(sid(), "rk", 2000, true), Start::Accepted);
        m.admit();
        m.bound();
        assert_eq!(m.phase(), Phase::Binding);
        let line = m.picture(2500).expect("first picture logs once");
        assert!(line.starts_with("preview= dwell_ms=500 "));
        assert!(m.picture(2600).is_none(), "the line is one per picture");
        assert_eq!(m.phase(), Phase::Playing);
        m.eos();
        assert_eq!(m.phase(), Phase::Stopping);
        m.stopped();
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn an_admission_refusal_does_not_arm_the_breaker_or_spend_the_budget() {
        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "rk", 0, true), Start::Accepted);
        m.refuse_admission();
        assert!(!m.breaker_open());
        assert_eq!(m.cycles(), 0);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.start(sid(), "rk", 1, true), Start::Accepted);
    }

    #[test]
    fn an_admitted_then_failed_load_arms_the_breaker() {
        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "rk", 0, true), Start::Accepted);
        m.admit();
        m.fail_admitted(601);
        assert!(m.breaker_open());
        assert_eq!(m.start(sid(), "other", 1, true), Start::BreakerOpen);
    }

    #[test]
    fn the_budget_stops_new_starts_after_the_source_ceiling() {
        let mut m = Machine::default();
        for i in 0..CYCLE_BUDGET {
            assert_eq!(m.start(sid(), "rk", i, true), Start::Accepted);
            m.admit();
            m.stopped();
        }
        assert_eq!(m.start(sid(), "rk", 99, true), Start::BudgetSpent);
        assert!(!m.breaker_open(), "exhaustion is not a teardown anomaly");
    }

    #[test]
    fn the_three_facts_stay_distinct_and_item_facts_evict() {
        let mut m = Machine::default();
        m.remember(sid(), "none", Fact::NoExtra);
        m.remember(sid(), "nope", Fact::RefusedDirect);
        assert_eq!(m.start(sid(), "none", 0, true), Start::Cached(Fact::NoExtra));
        assert_eq!(
            m.start(sid(), "nope", 0, true),
            Start::Cached(Fact::RefusedDirect)
        );
        m.fail_admitted(0);
        assert!(!m.breaker_open(), "a failure that was never admitted is not the breaker");
        assert_eq!(m.start(sid(), "arm", 1, true), Start::Accepted);
        m.admit();
        m.fail_admitted(601);
        assert!(m.breaker_open());
        assert_eq!(m.start(sid(), "later", 0, true), Start::BreakerOpen);
        let mut full = Machine::default();
        for i in 0..CACHE_CAP {
            full.remember(sid(), &format!("k{i}"), Fact::NoExtra);
        }
        full.remember(sid(), "new", Fact::RefusedDirect);
        assert_eq!(full.fact(sid(), "k0"), None, "oldest item fact evicts");
        assert_eq!(full.fact(sid(), "new"), Some(Fact::RefusedDirect));
        assert_eq!(
            full.start(sid(), "k1", 0, true),
            Start::Cached(Fact::NoExtra),
            "a cached miss is not a replay suppressor for a different item"
        );
        assert_eq!(full.start(sid(), "fresh", 0, true), Start::Accepted);
    }

    #[test]
    fn the_preview_line_keeps_the_pinned_shape() {
        let mut m = Machine::default();
        m.start(sid(), "rk", 10, true);
        m.admit();
        let line = m.picture(410).unwrap();
        // `tests/run.py` parses player lines by shape. A drifting format fails here, not silently
        // in a soak that stops matching.
        let rest = line
            .strip_prefix("preview= ")
            .expect("preview= prefix");
        let mut dwell = None;
        let mut nop = None;
        let mut num = None;
        for field in rest.split(' ') {
            let (key, value) = field.split_once('=').expect("key=value");
            assert!(value.chars().all(|c| c.is_ascii_digit()), "{field}");
            match key {
                "dwell_ms" => dwell = Some(value),
                "nop" => nop = Some(value),
                "num" => num = Some(value),
                other => panic!("unexpected field {other}"),
            }
        }
        assert_eq!(dwell, Some("400"));
        assert_eq!(nop, Some("0"));
        assert_eq!(num, Some("0"));
    }

    #[test]
    fn a_relay_link_is_not_a_direct_play() {
        let policy = crate::plex::link_policy(Some(crate::plex::probe::Location::Relay));
        assert!(!accepts_direct_play(policy.direct_play, true, false));
        assert!(accepts_direct_play(true, true, false));
        assert!(!accepts_direct_play(true, true, true));
        assert!(!accepts_direct_play(true, false, false));
    }

    #[test]
    fn abandon_defers_the_join_until_the_load_thread_has_returned() {
        assert!(defer_media_join(true, false));
        assert!(!defer_media_join(true, true));
        assert!(!defer_media_join(false, false));
    }

    #[test]
    fn chrome_recedes_only_after_a_picture() {
        let mut m = Machine::default();
        assert_eq!(m.view(), View::STILL);
        m.start(sid(), "rk", 0, true);
        m.admit();
        assert!(!m.view().picture);
        m.picture(1000);
        let view = m.view();
        assert!(view.picture);
        assert_eq!(view.prose, 0.0);
        assert_eq!(view.art, 0.0);
        assert!((view.field - crate::ui::landing_hero::PREVIEW_FIELD).abs() < 1e-6);
    }

    #[test]
    fn a_disabled_toggle_never_leaves_idle() {
        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "rk", 0, false), Start::Disabled);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.cycles(), 0);
    }
}
