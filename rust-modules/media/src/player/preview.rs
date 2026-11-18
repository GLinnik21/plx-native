//! Background trailer preview. Shares the engine, not the session services: no PlayQueue, no
//! timeline reporter, no scrobble. Watch-state writes are the promise that is kept. `/decision`
//! still creates Activity, which is a server write and is not a watch-state write.
//!
//! **Interaction, once the picture is up and the viewer presses UP.** The detail page stays
//! mounted and there is no route change — the player route never adopts this engine. ALL of the
//! page's chrome fades to zero, the action row included, and the page draws a trailer transport
//! in its place (`screens::detail::trailer`): the `Trailer` kicker over the item's title, the
//! playbar and the state read-out, drawn from `appkit::player_hud`'s own pieces. What a trailer does
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
//! `player::pause`/`resume` on a live engine, and a real stop's `stop_transport` clears the flag, so
//! a collapsed or ended preview cannot strand it.
//!
//! Sound stays on. A preview has no mute control: the per-pipeline `setVolume` the sink exposes is
//! used for one thing only, the silent paused-seek preroll (`player::mute_for_preroll_on`), and is not
//! a preview or UI feature. The Settings toggle is the only sound control.
//!
//! **Cycle budget is source arithmetic, not a measured leak.** Spike 0a never produced a delta.
//! `sf_load` keeps a 64 KiB slot (`src/starfish.c`) and the conservative RSS headroom cited by
//! the plan is about 958 KiB (`ui/frame/render_set.rs`). 958 / 64 is 14.96, so the ceiling is 14
//! admitted Loads, under the ratio rather than on it. Do not quote 14 as a television result.

use plx_plex::plex::ServerId;
use plx_telemetry::telemetry::classes::PreviewRefusal;

/// Admitted Loads per process, from the source arithmetic above. Not a device measurement.
pub const CYCLE_BUDGET: u32 = 14;

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
pub const DWELL_S: f32 = 2.0;

/// Scroll, in px, at which the opaque cover is fully up and the plane is released.
pub const COVER_SCROLL: f32 = 160.0;

/// Per-item negative facts. The breaker is process-wide and is not stored here, so evicting an
/// item cannot clear it and a breaker-open lookup is not mistaken for "this trailer refused".
const CACHE_CAP: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fact {
    NoExtra,
    RefusedDirect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Fetching,
    Loading,
    Abandoning,
    Binding,
    Playing,
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Accepted,
    Disabled,
    Cached(Fact),
    BreakerOpen,
    BudgetSpent,
    Busy,
}

/// How much a bound preview multiplies the hero scrim curve: the [`View::field`] of a picture that
/// is up. 1.35 is the video-bound row the legibility table grades.
///
/// `ui::landing_hero::PREVIEW_FIELD` is the same number from the other side: the scrim curve
/// that consumes it is `ui`'s, and `ui` may not name `player` (nor `player` name `ui`), so each
/// owns a copy and `screens::detail`'s `preview_plane_tests` pin them equal.
pub const PREVIEW_FIELD: f32 = 1.35;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// Art texture alpha over the plane. 1 is a still, 0 is picture only.
    pub art: f32,
    /// Meta line and synopsis. Logo, title and the control row do not use this.
    pub prose: f32,
    /// Raised field strength. 1 until a picture is bound, then [`PREVIEW_FIELD`].
    pub field: f32,
    /// True once a frame has been presented, so hero chrome must not sample the framebuffer.
    pub picture: bool,
    pub playing: bool,
}

impl View {
    pub const STILL: Self = Self {
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
pub struct Machine {
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
    /// The last `blocked` answer that was logged, so the reason is written once per change.
    told: Option<(u32, String, Option<Start>)>,
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
            told: None,
        }
    }
}

impl Machine {
    #[cfg(any(test, feature = "test-support"))]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn breaker_open(&self) -> bool {
        self.breaker
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn cycles(&self) -> u32 {
        self.cycles
    }

    /// A second dwell is allowed. Cache hits refuse to start, they do not suppress replay of an
    /// item that actually has a playable extra.
    pub fn start(&mut self, sid: ServerId, rk: &str, now_ms: u32, enabled: bool) -> Start {
        if let Some(refusal) = self.block_reason(sid, rk, enabled) {
            self.nops += 1;
            return refusal;
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

    /// The standing refusal for `rk`, if any — the one list both [`start`](Self::start) and
    /// [`blocked`] answer from. `Busy` is not here: it is about this moment, not about the item.
    fn block_reason(&self, sid: ServerId, rk: &str, enabled: bool) -> Option<Start> {
        if !enabled {
            Some(Start::Disabled)
        } else if self.breaker {
            Some(Start::BreakerOpen)
        } else if self.cycles >= CYCLE_BUDGET {
            Some(Start::BudgetSpent)
        } else {
            self.fact(sid, rk).map(Start::Cached)
        }
    }

    pub fn fact(&self, sid: ServerId, rk: &str) -> Option<Fact> {
        let sid = u32::from(sid.raw());
        self.cache
            .iter()
            .find(|e| e.sid == sid && e.rk == rk)
            .map(|e| e.fact)
    }

    pub fn remember(&mut self, sid: ServerId, rk: &str, fact: Fact) {
        self.remember_raw(u32::from(sid.raw()), rk, fact);
    }

    fn remember_raw(&mut self, sid: u32, rk: &str, fact: Fact) {
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
    pub fn admit(&mut self) {
        if self.phase == Phase::Fetching || self.phase == Phase::Loading {
            self.phase = Phase::Loading;
            if !self.admitted {
                self.admitted = true;
                self.cycles = self.cycles.saturating_add(1);
            }
        }
    }

    /// `sf_load` refused while a slot was still live. Expected under Abandoning. Not a failure.
    pub fn refuse_admission(&mut self) {
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    pub fn refuse_direct(&mut self, sid: ServerId, rk: &str) {
        self.remember(sid, rk, Fact::RefusedDirect);
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    /// This machine's own request landed with no URL. Remembered against the key [`start`]
    /// recorded — the item that was asked for — so the page does not re-resolve it every dwell.
    pub fn refuse_landing(&mut self) {
        if let Some((sid, rk)) = self.key.take() {
            self.remember_raw(sid, &rk, Fact::RefusedDirect);
        }
        self.nops += 1;
        self.phase = Phase::Idle;
        self.admitted = false;
    }

    pub fn no_extra(&mut self, sid: ServerId, rk: &str) {
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
    pub fn fail_admitted(&mut self, num: u32) {
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
    pub fn seek_budget_ok(&self) -> bool {
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
    pub fn admit_seek(&mut self) {
        self.phase = Phase::Loading;
        self.admitted = true;
        self.cycles = self.cycles.saturating_add(1);
    }

    /// In-flight Load is left to land unbound. The join waits until the media thread has returned.
    pub fn abandon(&mut self) {
        if matches!(self.phase, Phase::Fetching | Phase::Loading | Phase::Binding) {
            self.phase = Phase::Abandoning;
        } else if self.phase != Phase::Idle {
            self.phase = Phase::Stopping;
        }
    }

    pub fn bound(&mut self) {
        if self.phase == Phase::Loading {
            self.phase = Phase::Binding;
        }
    }

    /// A presented frame. Returns the `preview=` line for the FIRST one this session ever shows;
    /// every later call (this session's own reload landing after a seek, or a redundant call once
    /// already `Playing`) still moves the phase to `Playing` — that part is not once-only, or a
    /// seek's reload would land with a picture up and no way back out of `Loading`/`Binding` — but
    /// answers `None`, since the line is a "started playing" fact, not a "still playing" one.
    pub fn picture(&mut self, now_ms: u32) -> Option<String> {
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

    pub fn eos(&mut self) {
        if self.phase == Phase::Playing {
            self.phase = Phase::Stopping;
        }
    }

    pub fn stopped(&mut self) {
        self.phase = Phase::Idle;
        self.admitted = false;
        self.key = None;
    }

    /// [`halt`] has asked the engine to stop. `still_live` is whether it is still installed
    /// afterwards — a Load that had not returned is left to land unbound rather than joined.
    /// Still installed means `Abandoning` whatever the phase was — including Idle, when the
    /// machine had already lost track of the session — so `occupies()` holds a Play until
    /// [`after_pump`] joins the returned Load.
    pub fn halted(&mut self, still_live: bool) {
        if still_live {
            self.phase = Phase::Abandoning;
        } else {
            self.stopped();
        }
    }

    /// May a preview landing start an engine? A landing is the answer to [`start`]'s own
    /// request, so only a machine still `Fetching` is waiting for one.
    pub fn expects_landing(&self) -> bool {
        self.phase == Phase::Fetching
    }

    /// **`Loading`/`Binding` count as "picture up" too, once this session has shown one before.**
    /// A seek's `reload_at` briefly leaves `Playing` for `Loading`→`Binding` on its way back
    /// ([`admit_seek`]/[`bound`]/[`picture`]) — if this returned [`View::STILL`] for that span,
    /// `screens::detail`'s own `!view.picture` rule would read it as the trailer ending and pop
    /// the page out of full-trailer mode for the fraction of a second the reload takes, then back
    /// in once the next frame lands. `picture_ms.is_some()` is what tells that span apart from the
    /// COLD start's own `Fetching`/`Loading`, before this session has ever shown anything.
    pub fn view(&self) -> View {
        let picture = self.picture_ms.is_some()
            && matches!(self.phase, Phase::Loading | Phase::Binding | Phase::Playing);
        if !picture {
            return View::STILL;
        }
        View {
            art: 0.0,
            prose: 0.0,
            field: PREVIEW_FIELD,
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
pub fn accepts_direct_play(direct_play: bool, part_nonempty: bool, adaptive: bool) -> bool {
    direct_play && part_nonempty && !adaptive
}

/// **Which condition refused a preview** (Sentry PLX-NATIVE-23), or `None` when
/// [`accepts_direct_play`] accepts it. The first failing condition, in this fixed order:
///
/// 1. no part: [`PreviewRefusal::NoPart`];
/// 2. `direct_play_offered` was false: [`PreviewRefusal::NotDirect`];
/// 3. a direct play WAS on offer and `adaptive` (Auto chose HLS) is what refused it:
///    [`PreviewRefusal::Adaptive`];
/// 4. anything else, a late demotion such as an enhancement's remux: [`PreviewRefusal::NotDirect`].
///
/// `direct_play_offered` is the route's direct-play answer BEFORE Auto's override, not the
/// `direct_play` the gate sees: Auto sets that false whenever it picks HLS, so reading the reason
/// off the gate's own inputs would call every Auto-quality refusal `adaptive`, including a preview
/// the server would have transcoded anyway.
pub fn refusal(
    direct_play: bool,
    direct_play_offered: bool,
    part_nonempty: bool,
    adaptive: bool,
) -> Option<PreviewRefusal> {
    if accepts_direct_play(direct_play, part_nonempty, adaptive) {
        return None;
    }
    Some(if !part_nonempty {
        PreviewRefusal::NoPart
    } else if direct_play_offered && adaptive {
        PreviewRefusal::Adaptive
    } else {
        PreviewRefusal::NotDirect
    })
}

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-nopreview` — disable background trailer autoplay. Latched: `enabled()`
    /// runs on `preview_tick`'s every-frame path (via `blocked`) while the detail hero holds
    /// focus, and a `devtrig::flag` read is a `stat(2)` syscall per call.
    fn nopreview_armed = "nopreview";
);

/// The seated profile's hero trailer autoplay switch (P1c). A [`crate::route::LiveCell`] restored by
/// `route::restore_profile_rows` (at boot for nobody seated, then at every seat change), and saved
/// through the one write door (`route::choose`, Settings > System). Seeded ON, the product default,
/// so a read before anything has restored it is what the install did before the setting existed.
pub(crate) static TRAILER_AUTOPLAY: crate::route::LiveCell<bool> =
    crate::route::LiveCell::new("hero trailer autoplay preference");

/// Whether the hero may autoplay its trailer: the switch of the SEATED profile (its own pick, else
/// the TV's older key, else on), unless the dev trigger disarms it.
///
/// Read every frame (via `blocked`) while the detail hero holds focus, so it reads a cell: no
/// lock, no session read. (Before the switch was per profile it read `session::peek()`, which is
/// cached; the storage-helper round trip that cache exists to avoid cost ~27 ms/frame on the
/// television in 2026-09, the detail-page 60->26 fps regression. See `session::IO`/`session::CACHE`.)
pub fn enabled() -> bool {
    !nopreview_armed() && TRAILER_AUTOPLAY.get()
}

/// Put the switch where a restore says without saving it: what `route::restore_profile_rows`
/// does, and what a test uses to stand the cell somewhere known.
#[cfg(any(test, feature = "test-support"))]
pub fn restore_trailer_autoplay(on: bool) {
    TRAILER_AUTOPLAY.set(on);
}

fn slot() -> &'static std::sync::Mutex<Machine> {
    static MACHINE: std::sync::OnceLock<std::sync::Mutex<Machine>> = std::sync::OnceLock::new();
    MACHINE.get_or_init(|| std::sync::Mutex::new(Machine::default()))
}

fn with_mut<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

pub fn view() -> View {
    with_mut(|m| m.view())
}

/// Run one machine transition and log it when the phase actually moved — the only record a
/// field report has of why autoplay stopped, or never started again.
fn transition(why: &str, f: impl FnOnce(&mut Machine)) {
    let moved = with_mut(|m| {
        let from = m.phase;
        let rk = m.key.as_ref().map(|(_, rk)| rk.clone()).unwrap_or_default();
        f(m);
        (from != m.phase).then(|| (from, m.phase, rk))
    });
    if let Some((from, to, rk)) = moved {
        crate::player::log(&format!("preview: {from:?}->{to:?} ({why}) rk={rk}"));
    }
}

/// Called every frame the hero holds focus. The reason is logged once per change of item or
/// reason, not per frame: a page that never autoplays says why exactly once.
pub fn blocked(sid: ServerId, rk: &str) -> bool {
    let enabled = enabled();
    let told = with_mut(|m| {
        let reason = m.block_reason(sid, rk, enabled);
        let sid = u32::from(sid.raw());
        // Compared before anything is allocated: this runs every frame.
        if m.told.as_ref().is_some_and(|(s, r, why)| *s == sid && r == rk && *why == reason) {
            return (reason, false);
        }
        m.told = Some((sid, rk.to_owned(), reason));
        (reason, true)
    });
    if let (Some(reason), true) = told {
        crate::player::log(&format!("preview: blocked rk={rk} reason={reason:?}"));
    }
    told.0.is_some()
}

/// Has the preview session now running, or the one that last ran, put a picture on the panel?
/// The machine keeps the fact past its own retirement (`picture_ms` is cleared only by the next
/// [`Machine::start`]), which is what lets a failure report — observed a frame after the session
/// retired — say that a trailer which played and then failed did start.
pub fn picture_shown() -> bool {
    with_mut(|m| m.picture_ms.is_some())
}

pub fn occupies() -> bool {
    with_mut(|m| m.phase != Phase::Idle)
}

/// True when a preview session is the one the engine must not join yet.
pub fn abandoning() -> bool {
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
pub fn bound_or_playing() -> bool {
    with_mut(|m| phase_bound_or_playing(m.phase))
}

pub fn request_start(sid: ServerId, rk: &str, now_ms: u32) -> Start {
    let (start, phase) = with_mut(|m| (m.start(sid, rk, now_ms, enabled()), m.phase));
    crate::player::log(&format!("preview: start rk={rk} -> {start:?} phase={phase:?}"));
    start
}

pub fn note_no_extra(sid: ServerId, rk: &str) {
    transition("no extra", |m| m.no_extra(sid, rk));
}

pub fn note_refused_direct(sid: ServerId, rk: &str) {
    transition("refused direct", |m| m.refuse_direct(sid, rk));
}

pub fn note_admitted() {
    transition("admitted", Machine::admit);
}

pub fn note_admission_refused() {
    transition("admission refused", Machine::refuse_admission);
}

pub fn note_failed(num: u32) {
    transition("Load failed", |m| m.fail_admitted(num));
}

pub fn note_picture(now_ms: u32) {
    let mut line = None;
    transition("picture", |m| line = m.picture(now_ms));
    if let Some(line) = line {
        crate::player::log(&line);
    }
}

/// A preview landing is about to start an engine. Refused — and logged — when the machine is no
/// longer waiting for it: the page that asked has since stopped it (a halt while the resolve
/// was in flight), and an engine started now would play with nothing tracking it.
pub fn expects_landing() -> bool {
    let (expects, phase) = with_mut(|m| (m.expects_landing(), m.phase));
    if !expects {
        crate::player::log(&format!("preview: landing discarded phase={phase:?}"));
    }
    expects
}

/// Test-only: force the process-wide singleton straight into `phase`, skipping the normal
/// transition ladder (`start`/`admit`/`bound`/`picture`/…) so a test can probe a phase-gated
/// predicate ([`bound_or_playing`], `DetailScreen::full_trailer()`) without wiring a live session
/// end to end. Reset with [`reset_for_test`] before the guard (`testlock::serial()`) that must
/// surround both calls is dropped, so no state leaks to whichever test the process runs next.
#[cfg(any(test, feature = "test-support"))]
pub fn set_phase_for_test(phase: Phase) {
    with_mut(|m| m.phase = phase);
}

/// Test-only: the singleton machine's phase.
#[cfg(any(test, feature = "test-support"))]
pub fn phase_for_test() -> Phase {
    with_mut(|m| m.phase)
}

/// Test-only: while set, [`retire`] finds the preview's Load not yet returned, as it does for a
/// real trailer abandoned mid-Load, so a halt leaves the machine `Abandoning` (`occupies()` true)
/// without an installed engine. Cleared by [`reset_for_test`].
#[cfg(any(test, feature = "test-support"))]
pub fn hold_load_for_test(on: bool) {
    LOAD_STILL_OUT.store(on, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(any(test, feature = "test-support"))]
static LOAD_STILL_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(any(test, feature = "test-support"))]
fn load_still_out() -> bool { LOAD_STILL_OUT.load(std::sync::atomic::Ordering::SeqCst) }

#[cfg(not(any(test, feature = "test-support")))]
fn load_still_out() -> bool { false }

/// Test-only: [`set_phase_for_test`] plus the `picture_ms` half `view()` also checks, skipping
/// `enabled()`/session plumbing and a real Starfish Load so a screen-level test can exercise
/// `DetailScreen::full_trailer()`-gated behavior.
#[cfg(any(test, feature = "test-support"))]
pub fn force_playing_for_test() {
    set_phase_for_test(Phase::Playing);
    with_mut(|m| m.picture_ms = Some(0));
}

/// Test-only: undo [`force_playing_for_test`] (or any other singleton mutation) back to a fresh
/// `Machine`.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_test() {
    hold_load_for_test(false);
    with_mut(|m| *m = Machine::default());
}

/// What [`after_pump`] owes the machine and the engine this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    /// Nothing to end. Bind/picture bookkeeping still runs for a live preview.
    Nothing,
    /// The preview engine played to its end: stop it and return to Idle.
    Ended,
    /// A preview Load failed before any frame.
    Failed,
    /// The machine tracks a session that no longer exists — a landing replaced the preview
    /// session, or its engine was torn down by a path that is not this file's.
    Stale,
    /// The preview's own landing produced no engine (`/decision` refused, not a direct play).
    Refused,
}

/// The facts [`after_pump`] reads, gathered so [`settle`] can be a pure function of them.
#[derive(Clone, Copy, Debug)]
pub struct PumpFacts {
    pub phase: Phase,
    /// `route::is_preview` — the installed session is a preview.
    pub preview: bool,
    /// An engine is installed.
    pub live: bool,
    /// A resolve is in flight (`route::play_pending`).
    pub resolving: bool,
    pub ended: bool,
    pub failed: bool,
    pub seen_frame: bool,
}

/// Decide what [`after_pump`] must do. Abandoning is not asked here — it waits on the Load
/// thread, which is not a fact this function can see.
///
/// **The installed session, not the machine, says whose engine it is.** The machine is this
/// file's bookkeeping about a session; when the two disagree the machine is the one that is
/// wrong, and every rule below moves the machine, never someone else's engine.
pub fn settle(f: PumpFacts) -> Settle {
    if !f.preview {
        // `Fetching` before the landing is the normal resolve window: `ps.preview` is only
        // installed when the plan lands. Anything else is a machine whose session is gone.
        return match f.phase {
            Phase::Idle => Settle::Nothing,
            Phase::Fetching if f.resolving => Settle::Nothing,
            _ => Settle::Stale,
        };
    }
    if !f.live {
        return match f.phase {
            Phase::Fetching if f.resolving => Settle::Nothing,
            // The landing arrived and started nothing: `/decision` refused it.
            Phase::Fetching => Settle::Refused,
            // No engine, whatever the machine thought: release it and the preview flag.
            _ => Settle::Stale,
        };
    }
    // A live preview engine. Its end and its failure are this file's to act on even when the
    // machine lost track of it — nothing else will stop it.
    if f.ended {
        Settle::Ended
    } else if f.failed && !f.seen_frame {
        Settle::Failed
    } else {
        Settle::Nothing
    }
}

/// After the engine pump, every frame playback may run — NOT only while an engine is started:
/// a refused landing and a stale machine have no engine at all, and are exactly what used to
/// strand the machine outside Idle (and every later autoplay with it). Finishes an abandoned Load
/// once the media thread has returned, logs the first picture, and arms the breaker if an
/// admitted Load failed before any frame.
///
/// **It only ever stops a PREVIEW engine.** A machine that is out of step with the installed
/// session ([`Settle::Stale`]) is reset without touching the engine: that engine is a film's.
pub fn after_pump(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    now_ms: u32,
) {
    let preview = crate::route::is_preview(ps);
    if !preview && !occupies() {
        return;
    }
    if abandoning() {
        // The Load thread returns on its own thread at any instant, after the pump's last sample
        // (`pump` ends on one): sample again, so this frame's `finished` is the thread's, not the
        // frame's earlier one. `pump` does not run at all in a frame the player is not started.
        pa.publish_feed_attempt_view(ps);
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
        let finished = ps.readout().raw().load.load_thread_finished;
        if finished {
            retire(ps, pa, "abandoned Load returned");
        }
        return;
    }
    let live = pa.is_live();
    let seen_frame = crate::player::seen_frame(ps) == Some(true);
    if preview && live && occupies() {
        if seen_frame {
            note_picture(now_ms);
        } else if ps.readout().raw().load.stage.is_some_and(|stage| stage >= plx_playback_contract::feed::Stage::Playing) {
            transition("bound", Machine::bound);
        }
    }
    let facts = PumpFacts {
        phase: with_mut(|m| m.phase),
        preview,
        live,
        resolving: crate::route::play_pending(),
        ended: crate::player::feed_ended(ps),
        failed: ps.readout().raw().flags.load_failed,
        seen_frame,
    };
    match settle(facts) {
        Settle::Nothing => {}
        Settle::Ended => {
            crate::player::log("preview: EOS — stopping the preview engine");
            transition("eos", Machine::eos);
            retire(ps, pa, "eos");
        }
        Settle::Failed => {
            transition("admitted Load failed", |m| m.fail_admitted(601));
            retire(ps, pa, "admitted Load failed");
        }
        Settle::Stale => {
            // Never the engine: when the session is not a preview it is someone else's.
            transition(
                if preview { "engine gone" } else { "session replaced" },
                Machine::stopped,
            );
            if preview {
                crate::route::clear_preview(ps);
            }
        }
        Settle::Refused => {
            transition("landing refused", Machine::refuse_landing);
            crate::route::clear_preview(ps);
        }
    }
}

/// Is the live preview's transport paused? The engine is shared, so this is the same transport
/// Pause the session's readout holds for the player HUD, qualified by a preview actually being what
/// occupies it — with no session there is nothing paused, whatever the readout last said.
pub fn paused(ps: &crate::route::PlaybackSession) -> bool {
    occupies() && super::transport::paused(ps)
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
pub fn transport(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    play: Option<bool>,
) -> bool {
    if !crate::route::is_preview(ps) || !bound_or_playing() || !pa.is_live() {
        return false;
    }
    let want = super::transport::transport_target(play, super::transport::viewer_paused_for(ps, pa));
    super::transport::set_transport_paused(ps, pa, want)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekOutcome {
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
pub fn seek(
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
            with_mut(Machine::refuse_admission);
            retire(ps, pa, "seek reload failed");
            SeekOutcome::Failed
        }
    }
}

/// Stop a live preview. A Load that has not returned is abandoned rather than joined.
///
/// **Only a preview session's engine is ever stopped here.** With no preview installed, a
/// machine still `Fetching` is released so its landing is discarded ([`expects_landing`]), and
/// any other non-Idle phase is stale; neither is a reason to tear down whatever engine the
/// session does hold — that one belongs to a film.
pub fn halt(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    if !crate::route::is_preview(ps) {
        if occupies() {
            transition("halt before landing", Machine::stopped);
        }
        return;
    }
    transition("halt", Machine::abandon);
    retire(ps, pa, "halt");
}

/// **Stop a preview session's engine — the one way every preview exit does it** (EOS, a failed
/// Load or open, a failed seek, a halt, an abandoned Load that has finally returned).
///
/// `engine::teardown` defers a preview Load that has not returned rather than join it on the main
/// thread, so the engine may still be installed afterwards. The session then stays a preview and
/// the machine `Abandoning` until [`after_pump`] finishes it: clearing the flag under a live
/// engine is how a failed trailer open was once rescued onto an HLS transcode that reported a
/// timeline to the viewer's account.
pub fn retire(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    why: &'static str,
) {
    if pa.is_live() {
        super::engine::stop_bufferfeed(ps, pa);
    }
    let still_live = pa.is_live() || load_still_out();
    transition(why, |m| m.halted(still_live));
    if !still_live {
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
    /// the same reason. The readout's seek epoch is the counter `player::request_seek` bumps on every
    /// call; this pins that `preview::seek` never becomes a second path to it, which is the whole of the
    /// watch-state promise as it applies to a seek (`route::note_user_seek_intent` and
    /// `report::note_seek_for` live behind that same one call). The `Started`/`StartFailed` arms of
    /// `seek` reach `engine::reload_at`, which needs a live native session this host test cannot
    /// build without the `hostsim` feature — those remain proven by `ArmOpen`'s own body (it only
    /// touches engine-level `FEED_SHARED` atomics) and by device verification, not by this test.
    ///
    /// Gated on `hostsim`, like `player::engine`'s own `PlayerAdapter`-constructing tests
    /// (`lifecycle_clock_tests`, `replay_after_stop_tests`): it needs the simulator's video sink
    /// (`ffi_host::HostSink`) to hold a native session, which the default build's `NoSink` does
    /// not — `make check` runs the full suite a second time with it on for exactly this class of test.
    #[test]
    #[cfg(feature = "hostsim")]
    fn seek_refuses_and_touches_no_user_seek_bookkeeping_with_no_live_preview() {
        let _serial = plx_base::testlock::serial();
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        assert!(!pa.is_live(), "test requires an empty native-session slot");
        let before = ps.readout().seek_epoch();

        let outcome = seek(&mut ps, &mut pa, 5_000_000_000);

        assert_eq!(outcome, SeekOutcome::Refused);
        assert_eq!(
            ps.readout().seek_epoch(),
            before,
            "preview::seek must never go through player::request_seek's counter"
        );
        assert_eq!(ps.readout().pending_seek(), None, "and leaves no seek waiting");
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
        let policy = plx_plex::plex::link_policy(Some(plx_plex::plex::probe::Location::Relay));
        assert!(!accepts_direct_play(policy.direct_play, true, false));
        assert!(accepts_direct_play(true, true, false));
        assert!(!accepts_direct_play(true, true, true));
        assert!(!accepts_direct_play(true, false, false));
    }

    /// Every input combination: the reason exists exactly when the gate refuses, and names the
    /// first failing condition in the documented order. `adaptive` implies a false `direct_play`
    /// (Auto's override), which is why only the reachable rows are enumerated for `adaptive`.
    #[test]
    fn the_refusal_reason_is_the_first_failing_condition_in_a_fixed_order() {
        use PreviewRefusal::{Adaptive, NoPart, NotDirect};
        for direct_play in [false, true] {
            for offered in [false, true] {
                for part in [false, true] {
                    for adaptive in [false, true] {
                        let got = refusal(direct_play, offered, part, adaptive);
                        assert_eq!(got.is_none(), accepts_direct_play(direct_play, part, adaptive),
                            "gate and reason disagree: dp={direct_play} offered={offered} part={part} adaptive={adaptive}");
                    }
                }
            }
        }
        // 1. A missing part outranks everything, including Auto's HLS and a refused direct play.
        assert_eq!(refusal(true, true, false, false), Some(NoPart));
        assert_eq!(refusal(false, true, false, true), Some(NoPart));
        assert_eq!(refusal(false, false, false, true), Some(NoPart));
        // 2. No direct play on offer before Auto chose: `not_direct`, even if Auto also went HLS.
        assert_eq!(refusal(false, false, true, false), Some(NotDirect));
        assert_eq!(refusal(false, false, true, true), Some(NotDirect));
        // 3. A direct play was on offer and Auto's bandwidth decision refused it.
        assert_eq!(refusal(false, true, true, true), Some(Adaptive));
        // 4. On offer, Auto kept the original, and a later demotion (an enhancement remux) refused it.
        assert_eq!(refusal(false, true, true, false), Some(NotDirect));
        // None: the plain direct play a preview takes.
        assert_eq!(refusal(true, true, true, false), None);
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
        assert!((view.field - PREVIEW_FIELD).abs() < 1e-6);
    }

    fn facts(phase: Phase) -> PumpFacts {
        PumpFacts {
            phase,
            preview: true,
            live: true,
            resolving: false,
            ended: false,
            failed: false,
            seen_frame: true,
        }
    }

    /// Field report, 0.7.0 prep: a trailer logged `EOS reached … → ended` and then ran on for
    /// three minutes, because the EOS stop was gated on the MACHINE's phase. A preview engine at
    /// its end is stopped whatever the machine believes.
    #[test]
    fn a_preview_engine_at_its_end_is_stopped_whatever_phase_the_machine_is_in() {
        for phase in [
            Phase::Idle,
            Phase::Fetching,
            Phase::Loading,
            Phase::Binding,
            Phase::Playing,
            Phase::Stopping,
        ] {
            let f = PumpFacts { ended: true, ..facts(phase) };
            assert_eq!(settle(f), Settle::Ended, "{phase:?}");
        }
    }

    /// The same field report's other half: a later detail page never autoplayed. A machine that
    /// outlives its session — a landing replaced the preview, or its engine went away through a
    /// path that is not this file's — must return to Idle, and must NEVER stop or fail the
    /// engine that is now there: that engine is a film's.
    #[test]
    fn a_machine_that_outlived_its_session_is_released_without_touching_the_engine() {
        for phase in [Phase::Loading, Phase::Binding, Phase::Playing, Phase::Stopping] {
            let film = PumpFacts { preview: false, ..facts(phase) };
            assert_eq!(settle(film), Settle::Stale, "{phase:?}: film engine live");
            let failing_film = PumpFacts { failed: true, seen_frame: false, ..film };
            assert_eq!(settle(failing_film), Settle::Stale, "{phase:?}: a film's Load failure is not the preview's");
            let ending_film = PumpFacts { ended: true, ..film };
            assert_eq!(settle(ending_film), Settle::Stale, "{phase:?}: a film's EOS is the player's");
            let gone = PumpFacts { live: false, ..facts(phase) };
            assert_eq!(settle(gone), Settle::Stale, "{phase:?}: preview engine torn down elsewhere");
        }
        // Fetching with no preview installed yet is the normal resolve window; once nothing is
        // resolving any more, the request was superseded and nothing will ever land for it.
        let resolving = PumpFacts { preview: false, live: false, resolving: true, ..facts(Phase::Fetching) };
        assert_eq!(settle(resolving), Settle::Nothing);
        assert_eq!(settle(PumpFacts { resolving: false, ..resolving }), Settle::Stale);
    }

    /// A `/decision` that refuses a preview (not a direct play) lands with no URL, `pump_play`
    /// returns nothing, and the loop's refusal handling never ran — so the machine sat in
    /// `Fetching`, `occupies()` stayed true, and no later page could dwell.
    #[test]
    fn a_refused_preview_landing_releases_the_machine_and_the_next_item_starts() {
        let landed = PumpFacts { live: false, resolving: false, ..facts(Phase::Fetching) };
        assert_eq!(settle(landed), Settle::Refused);
        assert_eq!(settle(PumpFacts { resolving: true, ..landed }), Settle::Nothing);

        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "trailer", 0, true), Start::Accepted);
        m.refuse_landing();
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(
            m.start(sid(), "trailer", 1, true),
            Start::Cached(Fact::RefusedDirect),
            "the refused item is not re-resolved every dwell"
        );
        assert_eq!(m.start(sid(), "next", 2, true), Start::Accepted);
    }

    /// Halting a preview whose Load has not returned leaves the engine installed. The machine
    /// must say so (`Abandoning`, so `occupies()` holds a Play) even if it had already lost
    /// track of the session — otherwise a Play goes straight at the installed engine and fails.
    #[test]
    fn a_halt_that_leaves_the_engine_installed_keeps_the_machine_abandoning() {
        let mut m = Machine::default();
        m.abandon();
        m.halted(true);
        assert_eq!(m.phase(), Phase::Abandoning);
        m.halted(false);
        assert_eq!(m.phase(), Phase::Idle);
    }

    /// A page that halts its preview while the resolve is still in flight must not have the
    /// landing start an engine later: nothing would track it.
    #[test]
    fn only_a_machine_still_fetching_accepts_its_landing() {
        let mut m = Machine::default();
        assert!(!m.expects_landing(), "nothing was requested");
        assert_eq!(m.start(sid(), "rk", 0, true), Start::Accepted);
        assert!(m.expects_landing());
        m.abandon();
        m.stopped();
        assert!(!m.expects_landing(), "halted before the landing");
    }

    #[test]
    fn a_disabled_toggle_never_leaves_idle() {
        let mut m = Machine::default();
        assert_eq!(m.start(sid(), "rk", 0, false), Start::Disabled);
        assert_eq!(m.phase(), Phase::Idle);
        assert_eq!(m.cycles(), 0);
    }
}

/// The retire path of a preview whose Load has not returned, against a real Engine and the real
/// Feed: the join is deferred while the Load thread runs, and the abandoned-Load poll finishes it.
#[cfg(all(test, feature = "hostsim"))]
mod abandoned_load_tests {
    use super::*;
    use plx_playback_contract::feed::Stage;
    use plx_play_feed::shared::FEED_SHARED;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            reset_for_test();
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// A preview Engine on a ready host object, still in its Load stage, whose Load thread parks
    /// until `release` is set. Bound to the fixture attempt, addressed in `ps`.
    fn loading_preview(ps: &mut crate::route::PlaybackSession,
        release: &Arc<AtomicBool>) -> crate::player::adapter::PlayerAdapter {
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
        let mut eng = crate::player::engine::streaming_engine_for_test(epoch);
        let attempt = crate::route::RouteStartAttempt::fixture();
        eng.attempt = attempt.contract_id();
        eng.stage = Stage::Loading;
        let hold = Arc::clone(release);
        eng.load_th = plx_base::task::spawn("media", move || {
            while !hold.load(Ordering::SeqCst) { std::thread::sleep(std::time::Duration::from_millis(1)); }
        });
        assert!(eng.load_th.is_some(), "the Load stand-in thread started");
        pa.install(eng);
        ps.begin_attempt_view(attempt, crate::player::attempt_view::Backend::Feed);
        crate::route::mark_preview_for_test(ps);
        pa
    }

    /// Wait (bounded) until the Engine's Load thread has returned.
    fn await_load_return(pa: &mut crate::player::adapter::PlayerAdapter) {
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pa.engine().is_none_or(|eng| eng.load_th.as_ref().is_none_or(|th| th.is_finished())) {
            assert!(std::time::Instant::now() < give_up, "the Load thread never returned");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// The Feed answers the join question in the call that acts on it: deferred (and marked
    /// abandoned) only for a session still in its Load stage whose thread is unfinished.
    #[test]
    fn the_feed_defers_a_preview_join_only_for_an_unfinished_load() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let _cleanup = Cleanup;
        let release = Arc::new(AtomicBool::new(false));
        let mut pa = loading_preview(&mut ps, &release);
        assert!(!pa.engine().unwrap().preview_abandon);
        assert!(pa.defer_preview_join(), "Load stage, thread running");
        assert!(pa.engine().unwrap().preview_abandon, "the deferral marks the preview abandoned");

        pa.engine().unwrap().preview_abandon = false;
        pa.engine().unwrap().stage = Stage::Playing;
        assert!(!pa.defer_preview_join(), "past the Load stage the join is the stop's");
        assert!(!pa.engine().unwrap().preview_abandon, "and nothing is marked");

        pa.engine().unwrap().stage = Stage::Loading;
        release.store(true, Ordering::SeqCst);
        await_load_return(&mut pa);
        assert!(!pa.defer_preview_join(), "the Load thread has returned");
        assert!(!pa.engine().unwrap().preview_abandon);
        drop(pa.take());
    }

    /// **The Load thread returns after the pump's last sample** (`pump`'s S2): the abandoning arm
    /// of `after_pump` must retire the preview in that same frame, so it re-samples first. Reading
    /// the frame's last sample would carry `load_thread_finished == false` until the next pump.
    #[test]
    fn a_load_that_returns_after_the_pump_is_retired_in_the_same_frame() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let _cleanup = Cleanup;
        let release = Arc::new(AtomicBool::new(false));
        let mut pa = loading_preview(&mut ps, &release);
        set_phase_for_test(Phase::Loading);

        halt(&mut ps, &mut pa);
        assert!(abandoning(), "the halt abandoned the preview");
        assert!(pa.is_live(), "the Load had not returned: the Engine stays installed");

        // The frame: the pump's last sample (S2) still sees the Load thread running.
        pa.publish_feed_attempt_view(&mut ps);
        assert!(!ps.readout().raw().load.load_thread_finished, "PRECONDITION: S2 saw it running");
        after_pump(&mut ps, &mut pa, 0);
        assert!(abandoning() && pa.is_live(), "still running: nothing retires yet");

        // The Load returns between S2 and the next `after_pump`, which runs in the same frame.
        release.store(true, Ordering::SeqCst);
        await_load_return(&mut pa);
        after_pump(&mut ps, &mut pa, 1);
        assert!(!pa.is_live(), "the abandoning arm retires the preview in the frame the Load returned");
        assert!(!abandoning());
        crate::player::route_start::finish_release_for_test(&mut pa);
    }

    /// Lets the Load stand-in return when a test ends, whichever way it ends.
    struct LetLoadReturn(Arc<AtomicBool>);
    impl Drop for LetLoadReturn {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    /// **The exit waits for a deferred preview's Load.** `app::run::shutdown` stops the playback and
    /// then asks `exit_native`; a preview whose Load has not returned is left installed by that stop
    /// (`defer_preview_join`), so the release machine owes nothing and the exit used to answer `Clean`
    /// over a Load still inside firmware, with no Unload ever issued. A Load that returns inside the
    /// exit's deadline gets the deferred stop and its Unload, and only then is the display clean.
    #[test]
    fn exit_over_a_deferred_preview_waits_for_its_load_and_releases() {
        use crate::player::route_start::{exit_native_with, stop_bufferfeed, NativeExit};
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let _cleanup = Cleanup;
        let release = Arc::new(AtomicBool::new(false));
        let _let_return = LetLoadReturn(Arc::clone(&release));
        let mut pa = loading_preview(&mut ps, &release);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.is_live() && pa.engine().unwrap().preview_abandon,
            "PRECONDITION: the stop deferred and left the Engine installed");
        let unloads = plx_play_feed::ffi_host::unload_calls_for_test();
        // Each idle step is where the Load stand-in returns, as the real one does between polls.
        let exit = exit_native_with(&mut ps, &mut pa, std::time::Instant::now, || {
            release.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(2));
        });
        assert_eq!(exit, NativeExit::Clean);
        assert!(!pa.is_live(), "the deferred stop ran: the Engine left the adapter");
        assert_eq!(plx_play_feed::ffi_host::unload_calls_for_test(), unloads + 1,
            "the native object was Unloaded before the display was called clean");
        assert!(!pa.has_pending_release() && !pa.release_busy());
    }

    /// **A Load that never returns is leaked, never dropped under its thread.** At the exit's
    /// deadline the installed Engine (the Load payload the thread reads) is taken and forgotten,
    /// and the display is left to the OS.
    #[test]
    fn exit_over_a_deferred_preview_whose_load_never_returns_leaks_the_engine() {
        use crate::player::route_start::{exit_native_with, stop_bufferfeed, NativeExit};
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let _cleanup = Cleanup;
        let release = Arc::new(AtomicBool::new(false));
        let _let_return = LetLoadReturn(Arc::clone(&release));
        let mut pa = loading_preview(&mut ps, &release);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.is_live() && pa.engine().unwrap().preview_abandon,
            "PRECONDITION: the stop deferred and left the Engine installed");
        let unloads = plx_play_feed::ffi_host::unload_calls_for_test();
        let dropped = plx_play_feed::feed_state::engines_dropped_for_test();
        let t0 = std::time::Instant::now();
        let mut ticks = 0u64;
        let exit = exit_native_with(&mut ps, &mut pa,
            || { ticks += 1; t0 + std::time::Duration::from_secs(5 * ticks) }, || {});
        assert_eq!(exit, NativeExit::WorkerInFirmware);
        assert!(!pa.is_live() && pa.engine().is_none(), "the Engine left the adapter");
        assert_eq!(plx_play_feed::feed_state::engines_dropped_for_test(), dropped,
            "leaked, not dropped under the thread that reads its payload");
        assert_eq!(plx_play_feed::ffi_host::unload_calls_for_test(), unloads,
            "no Unload is issued over a Load still inside the call");
    }
}
