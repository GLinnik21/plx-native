//! Background trailer preview. Shares the engine, not the session services: no PlayQueue, no
//! timeline reporter, no scrobble. Watch-state writes are the promise that is kept. `/decision`
//! still creates Activity, which is a server write and is not a watch-state write.
//!
//! **Interaction, once the picture is up and the viewer presses UP.** The detail page stays
//! mounted and there is no route change — the player route never adopts this engine. ALL of the
//! page's chrome fades to zero, the action row included, and the page draws a trailer transport
//! in its place (`screens::detail::trailer`): the `Trailer` kicker over the item's title, the
//! playbar and the state read-out, drawn from `ui::player_hud`'s own pieces. What a trailer does
//! NOT get is the rest of the HUD — no quality, subtitle, audio or Info control, no tabs and no
//! track menus: a preview has no PlayQueue, no timeline reporter and no watch state, and a
//! control that writes one has no business on it. OK and PLAYPAUSE pause and resume it
//! ([`transport`]); the controls auto-hide on the HUD's own linger and any key brings them back.
//! BACK and DOWN collapse the mode back to background autoplay. A second dwell on an item that
//! has a trailer starts from the beginning again. None of the cache facts is a replay suppressor.
//!
//! **LEFT/RIGHT scrub, through this machine's own admitted path — not `player::request_seek`.**
//! Every non-in-place seek path in `player::engine` falls back to `reload_at` — a fresh Starfish
//! `Load` — which is exactly why this used to be refused outright: an un-admitted reload spends a
//! 64 KiB slot outside [`CYCLE_BUDGET`]'s accounting, and a reload that then fails would be
//! observed as an admitted-Load failure, arming the process-wide breaker and ending trailer
//! autoplay for every later item in the session. [`seek`] closes both halves of that hazard rather
//! than routing around it: [`Machine::admit_seek`] counts the reload against [`CYCLE_BUDGET`] the
//! moment `reload_at` actually issues it (mirroring [`admit`](Machine::admit)'s own timing), and
//! [`Machine::fail_admitted`] does not arm the breaker for a failure that lands after this
//! session's first picture — a seek that goes wrong inside a trailer that already proved it plays
//! is a LOCAL failure (this preview stops, [`view`] goes back to [`View::STILL`], and
//! `screens::detail`'s own `!view.picture` rule falls the page back out of full-trailer mode),
//! never a reason to disable autoplay for the rest of the session. **Budget exhaustion is the
//! conservative case**: [`seek`] refuses before calling `reload_at` at all and the trailer keeps
//! playing exactly where it was — a seek is a nice-to-have on a preview that still has no watch
//! state to protect, and the budget is shared with every later item's autoplay, so it is not worth
//! spending one of the last slots on it. `seek` never calls `player::request_seek`: no
//! `route::note_user_seek_intent`, no `report::note_seek_for(playback_trace_generation())` — a
//! preview has no trace generation, and the watch-state promise this file opens with covers a
//! seek exactly like every other write. Pausing has none of this to begin with: it is
//! `player::pause`/`resume` on a live engine, and `TX.reset()` on a real stop clears the flag, so
//! a collapsed or ended preview cannot strand it.
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

    /// An admitted Load that then failed. This is what arms the breaker — UNLESS this session has
    /// already shown a picture before now (`picture_ms.is_some()`), which only happens once a
    /// seek's own `reload_at` was admitted ([`admit_seek`]) partway through an already-playing
    /// trailer. That failure says "this seek went wrong", not "this trailer cannot play" — the
    /// trailer already proved the opposite — so it must not disable autoplay for every later item
    /// in the session. Either way the cycle already spent stays spent and this session stops: an
    /// admission refusal (never admitted in the first place) must not come through here.
    pub(crate) fn fail_admitted(&mut self, num: u32) {
        if self.admitted && !self.breaker && self.picture_ms.is_none() {
            self.breaker = true;
        }
        self.last_num = num;
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    /// Would a user-driven seek's reload have room in the shared budget? A pure query so [`seek`]
    /// can refuse BEFORE calling `engine::reload_at` at all — the conservative
    /// budget-exhaustion answer this file's module doc commits to: refuse the seek and leave the
    /// trailer playing exactly where it was, rather than spend one of the last
    /// [`CYCLE_BUDGET`] slots on a control nobody needs the session to survive.
    pub(crate) fn seek_budget_ok(&self) -> bool {
        self.cycles < CYCLE_BUDGET
    }

    /// **The seek's reload was actually issued** (`engine::reload_at` returned `Started`): count
    /// it against [`CYCLE_BUDGET`] the same moment [`admit`](Self::admit) would, and put the phase
    /// back in `Loading` for exactly the span the cold-start Load spends there — [`bound`] and
    /// [`picture`] carry it back to `Playing` once the reload lands, same as any other admitted
    /// Load. Callable only once [`seek_budget_ok`] has already said yes; the caller
    /// ([`seek`]) also gates on [`bound_or_playing`], so this only ever runs from `Binding` or
    /// `Playing` — a picture has already been shown, which is what lets [`fail_admitted`] tell
    /// this reload's failure apart from the trailer never having started at all.
    pub(crate) fn admit_seek(&mut self) {
        self.phase = Phase::Loading;
        self.admitted = true;
        self.cycles = self.cycles.saturating_add(1);
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

    /// A presented frame. Returns the `preview=` line for the FIRST one this session ever shows;
    /// every later call (this session's own reload landing after a seek, or a redundant call once
    /// already `Playing`) still moves the phase to `Playing` — that part is not once-only, or a
    /// seek's reload would land with a picture up and no way back out of `Loading`/`Binding` — but
    /// answers `None`, since the line is a "started playing" fact, not a "still playing" one.
    pub(crate) fn picture(&mut self, now_ms: u32) -> Option<String> {
        if !matches!(self.phase, Phase::Loading | Phase::Binding | Phase::Playing) {
            return None;
        }
        let first = self.picture_ms.is_none();
        self.phase = Phase::Playing;
        if !first {
            return None;
        }
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

    /// **`Loading`/`Binding` count as "picture up" too, once this session has shown one before.**
    /// A seek's `reload_at` briefly leaves `Playing` for `Loading`→`Binding` on its way back
    /// ([`admit_seek`]/[`bound`]/[`picture`]) — if this returned [`View::STILL`] for that span,
    /// `screens::detail`'s own `!view.picture` rule would read it as the trailer ending and pop
    /// the page out of full-trailer mode for the fraction of a second the reload takes, then back
    /// in once the next frame lands. `picture_ms.is_some()` is what tells that span apart from the
    /// COLD start's own `Fetching`/`Loading`, before this session has ever shown anything.
    pub(crate) fn view(&self) -> View {
        let picture = self.picture_ms.is_some()
            && matches!(self.phase, Phase::Loading | Phase::Binding | Phase::Playing);
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

crate::dev::latched_flag!(
    /// `/tmp/plxnative-nopreview` — disable background trailer autoplay. Latched: `enabled()`
    /// runs on `preview_tick`'s every-frame path (via `blocked`) while the detail hero holds
    /// focus, and a `dev::flag` read is a `stat(2)` syscall per call.
    fn nopreview_armed = "nopreview";
);

pub(crate) fn enabled() -> bool {
    // `peek()` is safe on this every-frame path (via `blocked`) because it is cached now: a hit
    // never takes `session::IO`, which on the television guards a `recv(2)` round trip to the
    // storage helper, measured at ~27 ms/frame here before the cache existed (2026-09-18, the
    // detail-page 60->26 fps regression). See the doc on `session::IO`/`session::CACHE`.
    !nopreview_armed() && crate::plex::session::peek().trailer_autoplay()
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

/// Is `phase` an actual bound-or-playing session — as against merely non-Idle? A free function of
/// `Phase` alone (not the singleton) so it is host-testable with no global/`testlock` involved, the
/// same way every other `Machine`/`Phase` fact in this file is. See [`bound_or_playing`]'s doc for
/// why this, and not [`occupies`]'s wider `phase != Idle`, is the question [`transport`] must ask.
fn phase_bound_or_playing(phase: Phase) -> bool {
    matches!(phase, Phase::Binding | Phase::Playing)
}

/// Is there a session actually bound to the plane or already showing a frame? Unlike
/// [`occupies`] (`phase != Idle`, true for Fetching/Loading/Abandoning/Stopping too), this is
/// specifically `Binding | Playing` — [`transport`] gates on this, not `occupies`, because a press
/// racing the machine's own stop (an in-flight fetch, an admitted-but-unbound Load, an abandoned
/// one) is exactly the race its own doc says must be refused, and all three of those are
/// "occupied" without a live engine a pause/resume could reach.
pub(crate) fn bound_or_playing() -> bool {
    with_mut(|m| phase_bound_or_playing(m.phase))
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

/// Test-only: force the process-wide singleton straight into `phase`, skipping the normal
/// transition ladder (`start`/`admit`/`bound`/`picture`/…) so a test can probe a phase-gated
/// predicate ([`bound_or_playing`], `DetailScreen::full_trailer()`) without wiring a live session
/// end to end. Reset with [`reset_for_test`] before the guard (`testlock::serial()`) that must
/// surround both calls is dropped, so no state leaks to whichever test the process runs next.
#[cfg(test)]
pub(crate) fn set_phase_for_test(phase: Phase) {
    with_mut(|m| m.phase = phase);
}

/// Test-only: [`set_phase_for_test`] plus the `picture_ms` half `view()` also checks, skipping
/// `enabled()`/session plumbing and a real Starfish Load so a screen-level test can exercise
/// `DetailScreen::full_trailer()`-gated behavior.
#[cfg(test)]
pub(crate) fn force_playing_for_test() {
    set_phase_for_test(Phase::Playing);
    with_mut(|m| m.picture_ms = Some(0));
}

/// Test-only: undo [`force_playing_for_test`] (or any other singleton mutation) back to a fresh
/// `Machine`.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    with_mut(|m| *m = Machine::default());
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
        // KNOWN GAP (code review, unresolved): this poll has no timeout. If the native load
        // thread never finishes (the k5lp DirectVoInit hang class this app already has a general
        // budget for on the live path, `pump::NATIVE_LOAD_BUDGET`), `finished` stays false
        // forever, `PlayerAdapter::is_live()` stays true forever, and `start_bufferfeed`'s
        // double-start guard then refuses every later Play for the rest of the session. Adding a
        // budget here is NOT a drop-in of that same mechanism: `engine::teardown` re-checks
        // `finished` itself and bails out identically while the thread is still running (see its
        // `is_preview` arm), so forcing a teardown past a timeout means abandoning a native
        // resource an OS thread may still be executing against — a real memory-safety question
        // (does the leaked `SfSlot` pattern in `src/starfish.c` make that safe, or does it not)
        // that needs `player/CLAUDE.md`'s device verification + fw-compat review, not a desk fix.
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

/// Is the live preview's transport paused? The engine is shared, so this is the same
/// `player::TX.paused` the player HUD reads, qualified by a preview actually being what occupies
/// it — with no session there is nothing paused, whatever the flag last said.
pub(crate) fn paused() -> bool {
    occupies() && crate::player::TX.paused.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pause or resume the live preview session — full-trailer mode's OK/PLAY/PAUSE, performed by the
/// loop because it needs the `MainThread` token a screen may not hold. `play` is
/// `Some(true)`/`Some(false)` for the remote's dedicated keys and `None` for a toggle.
///
/// Refused unless a preview really is the live session: the detail page emits this from a key
/// press, and a press that races the machine's own stop (EOS, an abandoned Load, a scroll that
/// hands the plane back) must not reach whatever the engine holds next. Returns whether the
/// transport ended up in the requested state, as `set_transport_paused` defines it.
///
/// [`bound_or_playing`], not [`occupies`]: `occupies` is true for the whole non-Idle span,
/// including Fetching/Loading/Abandoning/Stopping, and a press landing in any of those IS the
/// race this doc says must be refused — there is no bound engine yet (or no longer) for a
/// pause/resume to reach.
pub(crate) fn transport(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    play: Option<bool>,
) -> bool {
    if !crate::route::is_preview(ps) || !bound_or_playing() || !pa.is_live() {
        return false;
    }
    let want = crate::app::lifecycle::transport_target(play, crate::app::lifecycle::paused());
    crate::app::lifecycle::set_transport_paused(pa, want)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeekOutcome {
    /// `engine::reload_at` issued the reload; the picture will catch up once it lands.
    Started,
    /// Refused before anything was touched — not the live bound/playing session, or
    /// [`Machine::seek_budget_ok`] said no. Nothing was torn down: the trailer keeps playing
    /// exactly where it already was.
    Refused,
    /// `engine::reload_at`'s own synchronous start failed. Unlike `Refused`, `reload_at` tears the
    /// old engine down BEFORE this can be known (its own doc), so there is no "keep playing where
    /// it was" left to fall back to: the preview stops. The native call never got a slot
    /// (`refuse_admission`'s own meaning), so nothing is charged against the budget and the
    /// breaker stays closed.
    Failed,
}

/// **A user-driven LEFT/RIGHT seek inside a playing trailer.** This is the ONLY place a preview's
/// picture is ever moved to a position it did not arrive at on its own — see this file's module
/// doc for why every other seek path (`player::request_seek`) is wrong for it.
///
/// Refused unless a preview really is the live, bound/playing session — the same race
/// [`transport`] refuses, for the same reason. Past that gate the shared budget is the only other
/// question: [`Machine::seek_budget_ok`] is checked BEFORE `engine::reload_at` is ever called, so
/// an exhausted budget never spends anything — see [`SeekOutcome::Refused`]. Only once
/// `reload_at` reports it actually issued the Load does this admit the cycle
/// ([`Machine::admit_seek`]); a synchronous start failure is [`refuse_admission`], not
/// [`fail_admitted`] — the native call never ran, so it is not itself evidence the trailer can't
/// play.
pub(crate) fn seek(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    target_ns: i64,
) -> SeekOutcome {
    if !crate::route::is_preview(ps) || !bound_or_playing() || !pa.is_live() {
        return SeekOutcome::Refused;
    }
    if !with_mut(|m| m.seek_budget_ok()) {
        return SeekOutcome::Refused;
    }
    match super::engine::reload_at(ps, pa, target_ns) {
        super::engine::ReloadOutcome::Started => {
            with_mut(Machine::admit_seek);
            SeekOutcome::Started
        }
        super::engine::ReloadOutcome::NoRoute => SeekOutcome::Refused,
        super::engine::ReloadOutcome::StartFailed => {
            super::engine::stop_bufferfeed(ps, pa);
            with_mut(Machine::refuse_admission);
            crate::route::clear_preview(ps);
            SeekOutcome::Failed
        }
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

    /// **The invariant `transport`'s own doc claims: a press racing the machine's own stop must be
    /// refused.** `occupies()` (`phase != Idle`) would pass Fetching/Loading/Abandoning/Stopping
    /// through — all of them mid-race, none of them a live engine a pause/resume could reach.
    /// `bound_or_playing` (what `transport` actually gates on) must accept only Binding/Playing.
    #[test]
    fn bound_or_playing_excludes_every_race_occupies_would_let_through() {
        for phase in [
            Phase::Idle,
            Phase::Fetching,
            Phase::Loading,
            Phase::Abandoning,
            Phase::Stopping,
        ] {
            assert!(
                !phase_bound_or_playing(phase),
                "{phase:?} must not let a transport press through"
            );
        }
        for phase in [Phase::Binding, Phase::Playing] {
            assert!(phase_bound_or_playing(phase), "{phase:?} is a live, reachable session");
        }
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

    /// **Requirement 1's own gate: [`seek`] must refuse a user-driven seek once the shared budget
    /// is spent, BEFORE it ever calls `engine::reload_at`.** [`Machine::seek_budget_ok`] is the
    /// pure predicate that answer comes from — this pins it directly, the same way
    /// `the_budget_stops_new_starts_after_the_source_ceiling` pins `start`'s own ceiling check,
    /// without needing a live engine to prove the query answers `false` at exactly `CYCLE_BUDGET`
    /// admitted cycles and `true` below it.
    #[test]
    fn seek_budget_ok_runs_out_at_the_same_ceiling_a_cold_start_does() {
        let mut m = Machine::default();
        for i in 0..CYCLE_BUDGET {
            assert!(m.seek_budget_ok(), "cycle {i}: budget must still allow a seek");
            assert_eq!(m.start(sid(), "rk", i, true), Start::Accepted);
            m.admit();
            m.stopped();
        }
        assert!(
            !m.seek_budget_ok(),
            "every cycle spent — a seek must refuse rather than spend the process' last slots"
        );
    }

    /// **Requirement 2's own hazard: an admitted SEEK's failure must not arm the process-wide
    /// breaker the way a cold start's admitted-Load failure does.** The only thing that tells the
    /// two apart is `picture_ms` — this session already proved it can show a picture before the
    /// seek's own `reload_at` was ever admitted, so a later `fail_admitted` reads as "this seek
    /// went wrong", not "this trailer cannot play", and leaves autoplay armed for every later item.
    /// Contrast with `an_admitted_then_failed_load_arms_the_breaker` above, which is the SAME
    /// `fail_admitted` call on a machine that has never shown a picture — the breaker DOES arm
    /// there, which is exactly the case this one must not regress into.
    #[test]
    fn a_failed_seek_after_a_shown_picture_does_not_arm_the_breaker() {
        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "rk", 0, true), Start::Accepted);
        m.admit();
        m.bound();
        assert!(m.picture(100).is_some(), "the trailer must have proven it can show a picture");
        assert_eq!(m.phase(), Phase::Playing);

        // The seek's own admitted reload, then its failure — `fail_admitted`, not
        // `refuse_admission`: `reload_at` DID issue the Load (this is the async-failure path
        // `pump.rs`'s existing failure-observation machinery drives for every admitted Load).
        m.admit_seek();
        assert_eq!(m.cycles(), 2, "the seek's reload is admitted against the budget too");
        m.fail_admitted(601);

        assert!(
            !m.breaker_open(),
            "a seek gone wrong inside an already-proven trailer must not disable autoplay \
             for every later item this session"
        );
        assert_eq!(m.phase(), Phase::Idle, "the failed seek still stops this session's preview");
        assert_eq!(
            m.start(sid(), "other", 1, true),
            Start::Accepted,
            "the next item's autoplay must still be armed"
        );
    }

    /// **Requirement 3, the trivial half: `seek` refuses outright, and touches nothing, when there
    /// is no live bound/playing preview to seek within** — the same race [`transport`] refuses for
    /// the same reason. `TX.seek_reqs` is the counter `player::request_seek` bumps on every call;
    /// this pins that `preview::seek` never becomes a second path to it, which is the whole of the
    /// watch-state promise as it applies to a seek (`route::note_user_seek_intent` and
    /// `report::note_seek_for` live behind that same one call). The `Started`/`StartFailed` arms of
    /// `seek` reach `engine::reload_at`, which needs a live native session this host test cannot
    /// build without the `hostsim` feature — those remain proven by `arm_seek`'s own body (it only
    /// touches engine-level `SHARED` atomics) and by device verification, not by this test.
    ///
    /// Gated on `hostsim`, like `player::engine`'s own `PlayerAdapter`-constructing tests
    /// (`lifecycle_clock_tests`, `replay_after_stop_tests`): building one at all pulls in the
    /// Starfish/ACB `dynlib!` symbols, which only resolve under that feature — `make check` runs
    /// the full suite a second time with it on for exactly this class of test.
    #[test]
    #[cfg(feature = "hostsim")]
    fn seek_refuses_and_touches_no_user_seek_bookkeeping_with_no_live_preview() {
        let _serial = crate::testlock::serial();
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = crate::player::adapter::PlayerAdapter::new(unsafe {
            crate::task::MainThread::assume()
        });
        assert!(!pa.is_live(), "test requires an empty native-session slot");
        let before = crate::player::TX.seek_reqs.load(std::sync::atomic::Ordering::Relaxed);

        let outcome = seek(&mut ps, &mut pa, 5_000_000_000);

        assert_eq!(outcome, SeekOutcome::Refused);
        assert_eq!(
            crate::player::TX.seek_reqs.load(std::sync::atomic::Ordering::Relaxed),
            before,
            "preview::seek must never go through player::request_seek's counter"
        );
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
