//! **Full-trailer mode's transport, and the hint that leads to it.**
//!
//! Full-trailer mode (UP with a trailer picture up and focus on the hero) takes the whole page
//! off the screen — logo, meta, synopsis, scrims AND the action row — and puts a trailer
//! transport in its place: the `Trailer` kicker over a title ([`transport_title`] — the item's
//! own by default, the trailer extra's when THAT says something the kicker doesn't), the playbar,
//! the clocks and the state read-out. Those are the player HUD's own pieces drawn through
//! `ui::player_hud::{draw_scrim, draw_title, draw_playbar}`, so the two transports cannot drift;
//! what a trailer does NOT get is the rest of the HUD — no quality, subtitle, audio or Info
//! control, no tabs and no track menus. A preview session has no PlayQueue, no timeline reporter
//! and no watch state, and a control that writes one has no business on it.
//!
//! Everything here is a pure function of state the screen already holds, or a small value type
//! the screen owns — so the key policy, the auto-hide and the hint's visibility are all host-
//! testable without driving the process-wide `player::preview` singleton or a live engine.

use crate::ui::icons::Icon;
use crate::ui::player_hud::{Knob, Playbar, TransportMark};
use crate::ui::widgets::KeyHint;
use crate::ui::{consts, theme, Painter};
use std::ffi::CString;

/// How long the trailer transport lingers after the key that raised it, and how long
/// [`TransportMark::Play`] stands after a resume: the player HUD's own two constants, so the
/// trailer's controls leave the screen — and its resume mark clears — on the same beat a film's do.
use crate::ui::player_hud::{LINGER_MS, PLAY_MARK_MS};

/// Which title the transport shows under the `Trailer` kicker: the FILM/SHOW title by default, so
/// a trailer whose own PMS-scanned title is the boilerplate "Trailer" does not repeat the kicker
/// word right underneath it — the case docs/trailer-ux-plan.md calls out as true "on most
/// servers". The extra's own title wins only when it says something the kicker doesn't already,
/// compared trimmed and case-insensitively so "trailer"/"Trailer "/"TRAILER" all count as the same
/// non-information while "Official Trailer" or "Teaser 2" do not.
pub(super) fn transport_title<'a>(film_title: &'a str, extra_title: &'a str) -> &'a str {
    let trimmed = extra_title.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(crate::metadata::TRAILER_CONTEXT) {
        film_title
    } else {
        extra_title
    }
}

/// What a key does while full-trailer mode owns the page. Deliberately exhaustive over the keys
/// the mode CONSUMES: a key with no arm here is not the trailer's and falls through to the page's
/// ordinary handling (EXIT above all — see `screens::player`'s same carve-out).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TrailerKey {
    /// OK and PLAYPAUSE: pause a playing trailer, resume a paused one.
    Toggle,
    /// the remote's dedicated PLAY / PAUSE keys, which are each one direction of that toggle
    Play,
    Pause,
    /// BACK and DOWN: back to background autoplay, page chrome and all.
    Collapse,
    /// UP — the mode's own promotion key, pressed again: puts the controls back on screen and
    /// does nothing more.
    Reveal,
    /// LEFT/RIGHT: hold-to-scrub, forward = `true` (RIGHT). Acted on every edge — `Down` for the
    /// fixed hop, `Repeat` to engage the continuous ramp, `Up` to commit — unlike every other
    /// variant here, which only the `Down` edge drives. See [`trailer_key`]'s own doc for why this
    /// is safe to commit now where it was refused outright before.
    Scrub(bool),
}

/// PURE: what `key` does in full-trailer mode.
///
/// `sym`/`wcode` are the raw pair, classified by [`consts::classify`] for the keys the four-way
/// `machine::Key` alphabet cannot name (PLAY, PAUSE, PLAYPAUSE and the FF/REW alternates). The
/// machine key is preferred where it HAS an answer, because that is the value the input engine
/// itself navigated by.
///
/// **LEFT/RIGHT scrub.** They used to only reveal the controls: a preview's seek falls back to a
/// fresh Starfish `Load` on several paths (`INPLACE_SEEK_OK` cleared, a refused or stale native
/// Pause, a stuck in-place seek), and an un-admitted reload spends a budgeted Starfish slot the
/// machine never counted, with a failure charged as an admitted-Load failure that opens the
/// process-wide breaker for every later item. `player::preview::seek` closes that hazard with its
/// own admitted-reload accounting rather than this key ladder routing around it — see
/// `player/preview.rs`'s module doc for the budget/breaker rules a commit now goes through.
pub(super) fn trailer_key(
    key: crate::ui::machine::Key,
    sym: u32,
    wcode: u32,
) -> Option<TrailerKey> {
    use crate::ui::machine::Key as MKey;
    let key = match key {
        MKey::Up => return Some(TrailerKey::Reveal),
        MKey::Left => return Some(TrailerKey::Scrub(false)),
        MKey::Right => return Some(TrailerKey::Scrub(true)),
        MKey::Down | MKey::Back => return Some(TrailerKey::Collapse),
        MKey::Ok => return Some(TrailerKey::Toggle),
        MKey::Other => consts::classify(sym, wcode),
    };
    match key {
        consts::Key::Play => Some(TrailerKey::Play),
        consts::Key::Pause => Some(TrailerKey::Pause),
        consts::Key::PlayPause | consts::Key::Ok => Some(TrailerKey::Toggle),
        consts::Key::Up => Some(TrailerKey::Reveal),
        consts::Key::Left { .. } => Some(TrailerKey::Scrub(false)),
        consts::Key::Right { .. } => Some(TrailerKey::Scrub(true)),
        consts::Key::Down | consts::Key::Back => Some(TrailerKey::Collapse),
        // EXIT ends the process and STOP is the loop's; PointerHidden is a notification, not a
        // press. None of the three is the trailer's to swallow.
        _ => None,
    }
}

/// PURE: is the UP hint on screen? Only while a trailer is playing in the BACKGROUND with the
/// hero focused — the exact state UP acts on. It leaves on promotion (the mode it advertises is
/// now on), on the picture going away, and when focus walks off the hero row, because UP means
/// something else there.
pub(super) fn hint_shown(picture: bool, promoted: bool, hero_active: bool) -> bool {
    picture && !promoted && hero_active
}

/// PURE: is the trailer transport on screen? The player HUD's own rule
/// (`screens::player::input::hud_visible`): inside the linger deadline, or PAUSED — a paused
/// picture with no chrome at all says nothing about why it stopped.
pub(super) fn controls_shown(full_trailer: bool, now: u32, until: u32, paused: bool) -> bool {
    full_trailer && (paused || now < until)
}

/// The hint's own "text drawn straight onto artwork" legibility floor — the same concern
/// [`theme::SCRIM_TEXT_A`] already names for the hero's synopsis/title over the same backdrop, and
/// close to the owner's ask of "roughly 60-70%" (0.72; its own doc calls 0.60 the floor that still
/// clears 3:1 everywhere). Reused rather than a fresh magic float: the hint is exactly the case
/// that token exists for, one more line sitting on the video.
pub(super) const HINT_ALPHA_PEAK: f32 = theme::SCRIM_TEXT_A;
/// The quieter alpha the hint eases down to once it has stood for one [`LINGER_MS`] without going
/// away — half of [`HINT_ALPHA_PEAK`], so it stays legible but stops competing for attention once
/// the viewer has had time to read it. A fraction of the peak rather than a second independent
/// float, so retuning the peak keeps the two in proportion.
pub(super) const HINT_ALPHA_REST: f32 = HINT_ALPHA_PEAK * 0.5;

/// PURE: what the hint's eased alpha should be chasing this frame.
///
/// `elapsed_ms` is `None` while the hint is down (nothing to measure since) and `Some` for how
/// long it has been continuously up otherwise ([`Transport::update`]'s `hint_since`). Full
/// strength while it is newly up — the viewer's attention is still on it — then the dimmer
/// [`HINT_ALPHA_REST`] once [`LINGER_MS`] has passed, the same beat the transport's own controls
/// linger for. `hint=false` always wins to 0.0 regardless of `elapsed_ms`, which only ever has a
/// stale reading in that case (the caller clears it on the same edge).
pub(super) fn hint_alpha_target(hint: bool, elapsed_ms: Option<u32>) -> f32 {
    if !hint {
        0.0
    } else if elapsed_ms.is_some_and(|ms| ms >= LINGER_MS) {
        HINT_ALPHA_REST
    } else {
        HINT_ALPHA_PEAK
    }
}

/// The full-trailer transport's own animation state. Presentation only — like the rest of the
/// page's preview scalars it is not hashed into `LogicalState`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Transport {
    /// The auto-hide DEADLINE in `Tick::ms`, not a countdown — the player HUD's idiom
    /// (`PlayerScreen::hud.until`), and the one `ci/check-deps.sh`'s `dt` gate requires of an
    /// animator here: absolute clock readings, never a per-frame delta accumulated into state.
    until_ms: u32,
    /// the transport's eased alpha
    pub(super) alpha: f32,
    /// the UP hint's eased alpha — eases toward [`HINT_ALPHA_PEAK`] on appearing, then toward the
    /// quieter [`HINT_ALPHA_REST`] once it has stood for one [`LINGER_MS`] without going away, and
    /// toward 0 the moment [`hint_shown`] turns false. See [`hint_alpha_target`].
    pub(super) hint: f32,
    /// When the hint most recently transitioned false→true, so [`hint_alpha_target`] can tell how
    /// long it has been continuously up. `None` while it is down.
    hint_since: Option<u32>,
    /// When the last paused→playing edge happened, so the read-out can wear
    /// [`TransportMark::Play`] for [`PLAY_MARK_MS`], exactly as the HUD's `TransportRow::play_at`
    /// makes it.
    play_at: Option<u32>,
    /// last tick's pause state, which is the only way to see that edge
    was_paused: bool,
    /// The tick clock this transport was last stepped at. The playbar's own read-outs take an
    /// absolute `now` (the HUD passes `SDL_GetTicks` through `Tick::ms`), and neither `draw` nor a
    /// key arm has a clock of its own — a screen's paint takes no `Tick`, and a press is at most
    /// one frame away from the tick before it.
    now_ms: u32,
    // ---- LEFT/RIGHT's hold-to-scrub gesture ----------------------------------------------------
    //
    // The same idiom `screens::player::input::Scrub` drives the real HUD's scrubber with — a
    // fresh press hops `SCRUB_STEP_NS`, a HELD key (auto-repeat) engages a continuous ramp
    // (`SCRUB_BASE`→`SCRUB_MAX`), and the release commits at once for a hold or arms a short
    // debounce for a tap — but not the same STATE TYPE: `ci/check-deps.sh`'s `sibling` gate
    // forbids this module from naming `crate::screens::player` at all. The tuning constants and
    // the clamp formula ARE shared, from `ui::player_hud` (see that module's own doc for why).
    // `scrub_ns < 0` means no gesture is in progress; a caller reads it via [`Self::scrubbing`]
    // rather than the raw field, matching how `until_ms`/`revealed` are read through a method.
    scrub_dir: i32,        // -1 back / +1 forward / 0 = no scrub in progress
    scrub_hold: bool,      // a repeat arrived → continuous accelerating scrub engaged
    scrub_hold_since: u32, // when that hold engaged — the acceleration ramp is measured from here
    scrub_t: u32,          // last continuous-advance tick
    scrub_alive: u32,      // last held (repeat) event — for the lost-keyup safety commit
    scrub_commit_at: u32,  // tap released → commit at this tick (0 = none)
    scrub_ns: i64,         // the PREVIEW position, in ns; -1 = not scrubbing
}

impl Transport {
    pub(super) const IDLE: Self = Self {
        until_ms: 0,
        alpha: 0.0,
        hint: 0.0,
        hint_since: None,
        play_at: None,
        was_paused: false,
        now_ms: 0,
        scrub_dir: 0,
        scrub_hold: false,
        scrub_hold_since: 0,
        scrub_t: 0,
        scrub_alive: 0,
        scrub_commit_at: 0,
        scrub_ns: -1,
    };

    /// A key arrived: put the controls back on screen for a full linger.
    pub(super) fn reveal(&mut self) {
        self.until_ms = self.now_ms.wrapping_add(LINGER_MS);
    }

    /// Full-trailer mode is over: the transport goes with it, the resume mark it was carrying
    /// belongs to a session the viewer has stopped looking at, and any scrub gesture in flight is
    /// abandoned uncommitted — leaving it live would keep [`step_scrub_hold`](Self::step_scrub_hold)
    /// firing in the background after the page nobody is looking at has already collapsed.
    pub(super) fn dismiss(&mut self) {
        self.until_ms = 0;
        self.play_at = None;
        self.cancel_scrub();
    }

    /// Is a scrub gesture (held or a tap awaiting its debounce) in progress right now? What
    /// [`draw`](Self::draw) reads to decide whether the playbar/read-out follow the preview
    /// instead of the live position.
    pub(super) fn scrubbing(&self) -> bool {
        self.scrub_ns >= 0
    }

    /// Discard the gesture without committing — a refused seek (budget spent, or the mode ending
    /// mid-hold) leaves playback exactly where it already was.
    pub(super) fn cancel_scrub(&mut self) {
        self.scrub_ns = -1;
        self.scrub_dir = 0;
        self.scrub_hold = false;
        self.scrub_commit_at = 0;
    }

    /// A fresh LEFT/RIGHT: the fixed hop, same as the player HUD's `key_scrub_fresh`'s `Jump` arm.
    /// There is no `Reveal`-only first press here (unlike the HUD, which a LEFT/RIGHT can arrive
    /// at hidden): full-trailer mode's transport is already up the instant it is entered
    /// ([`reveal`](Self::reveal) fires on promotion), so every press is a real gesture. `dur_ns`
    /// is `player::duration_ns()`, `live_ns` is `player::playpos_ns()` — passed in rather than
    /// read here so this stays a pure function of state the caller already has, like the rest of
    /// this type.
    pub(super) fn scrub_fresh(&mut self, fwd: bool, now: u32, dur_ns: i64, live_ns: i64) {
        if dur_ns <= 0 {
            return;
        }
        if self.scrub_dir == 0 {
            self.scrub_t = now;
            self.scrub_hold_since = now;
            self.scrub_ns = live_ns;
        }
        self.scrub_commit_at = 0; // more input → cancel a pending tap commit
        self.scrub_alive = now;
        if !self.scrub_hold {
            let step = if fwd {
                crate::ui::player_hud::SCRUB_STEP_NS
            } else {
                -crate::ui::player_hud::SCRUB_STEP_NS
            };
            self.scrub_ns =
                crate::ui::player_hud::scrub_clamp_target(self.scrub_ns.max(0) + step, dur_ns);
        }
        self.scrub_dir = if fwd { 1 } else { -1 };
    }

    /// A hardware auto-repeat while a direction is held: port of the HUD's `key_scrub_repeat`.
    pub(super) fn scrub_repeat(&mut self, now: u32) {
        if self.scrub_dir != 0 && !self.scrub_hold {
            self.scrub_hold = true;
            self.scrub_hold_since = now;
            self.scrub_t = now;
        }
        if self.scrub_dir != 0 {
            self.scrub_alive = now;
            self.scrub_commit_at = 0;
        }
    }

    /// The continuous accelerating advance while a direction is held, and the lost-keyup safety
    /// net — same formula and constants as the player HUD's `step_scrub_hold`. Returns the commit
    /// target once `SCRUB_LOST_MS` has passed with no repeat (a dropped keyup, exactly as the
    /// HUD's own safety commit exists for); `None` otherwise. The caller asks `player::preview` to
    /// seek there — this type only tracks the gesture, it does not perform the seek.
    pub(super) fn step_scrub_hold(&mut self, now: u32, dur_ns: i64) -> Option<i64> {
        if self.scrub_dir == 0 || !self.scrub_hold {
            return None;
        }
        let held = now.wrapping_sub(self.scrub_hold_since) as f32 / 1000.0;
        let speed = (crate::ui::player_hud::SCRUB_BASE + crate::ui::player_hud::SCRUB_ACCEL * held)
            .min(crate::ui::player_hud::SCRUB_MAX);
        let mut sdt = now.wrapping_sub(self.scrub_t) as f32 / 1000.0;
        if sdt > 0.1 {
            sdt = 0.1;
        }
        let was = self.scrub_ns;
        self.scrub_ns = crate::ui::player_hud::scrub_clamp_target(
            was + (self.scrub_dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64,
            dur_ns,
        );
        self.scrub_t = now;
        self.reveal();
        if now.wrapping_sub(self.scrub_alive) > crate::ui::player_hud::SCRUB_LOST_MS {
            let target = self.scrub_ns;
            self.cancel_scrub();
            return Some(target);
        }
        None
    }

    /// Key-up: a hold commits at once; a plain tap arms the debounce instead, so a rapid burst
    /// coalesces into one seek — the HUD's own `key_scrub_release`, minus the `reveal`-only-press
    /// case it needs and this transport does not (see [`scrub_fresh`](Self::scrub_fresh)'s doc).
    pub(super) fn scrub_release(&mut self, now: u32) -> Option<i64> {
        if self.scrub_dir == 0 {
            return None;
        }
        if self.scrub_hold {
            let target = self.scrub_ns;
            self.cancel_scrub();
            Some(target)
        } else {
            // A tap → commit on a short debounce so a quick burst accumulates first. `dir`/`hold`
            // are deliberately left alone (not `cancel_scrub`): a same-direction tap arriving
            // before the debounce fires must see `scrub_dir != 0` and keep accumulating from the
            // preview already in flight, exactly like the HUD's own `key_scrub_release` tap arm.
            self.scrub_commit_at = now.wrapping_add(crate::ui::player_hud::TAP_COMMIT_MS).max(1);
            None
        }
    }

    /// The tap debounce, stepped every frame — the HUD's own `step_tap_commit`. Fires once
    /// `commit_at` has passed, returning the target to commit; a pending commit whose gesture was
    /// cancelled in the meantime (`scrub_ns` already `-1`) simply retires with nothing to give.
    pub(super) fn step_tap_commit(&mut self, now: u32) -> Option<i64> {
        if self.scrub_commit_at == 0 || now.wrapping_sub(self.scrub_commit_at) >= 0x8000_0000 {
            return None;
        }
        let target = (self.scrub_ns >= 0).then_some(self.scrub_ns);
        self.cancel_scrub();
        target
    }

    /// Is the linger still running? Read by the page's own tests instead of the deadline, which
    /// says nothing without the clock reading it was taken from.
    #[cfg(test)]
    pub(super) fn revealed(&self) -> bool {
        self.now_ms < self.until_ms
    }

    /// The in-flight scrub preview position, for `screens::detail`'s own tests — which live
    /// outside this module and so cannot read the private `scrub_ns` field directly. Only
    /// meaningful while [`scrubbing`](Self::scrubbing) is true.
    #[cfg(test)]
    pub(super) fn preview_ns_for_test(&self) -> i64 {
        self.scrub_ns
    }

    /// One frame. Returns whether anything moved (the caller's `PresentEvent::Motion`), which is
    /// TRUE while the transport is up over a running trailer — the clocks and the playbar advance
    /// every frame — and false once it has faded out, so a hidden transport does not hold the
    /// frame gate open.
    pub(super) fn update(
        &mut self,
        now: u32,
        dt: f32,
        full_trailer: bool,
        paused: bool,
        hint: bool,
    ) -> bool {
        // A pause holds the controls on screen by itself ([`controls_shown`]), so the deadline
        // travels with it — otherwise the resume would hide them on the very frame it set the
        // picture moving again, which is the one frame the viewer is looking at them.
        if self.was_paused && paused {
            self.until_ms = self.until_ms.wrapping_add(now.wrapping_sub(self.now_ms));
        }
        self.now_ms = now;
        if self.was_paused && !paused && full_trailer {
            self.play_at = Some(now);
        }
        self.was_paused = paused;
        if !full_trailer {
            self.dismiss();
        }
        let shown = controls_shown(full_trailer, now, self.until_ms, paused);
        if self
            .play_at
            .is_some_and(|at| now.wrapping_sub(at) >= PLAY_MARK_MS)
        {
            self.play_at = None;
        }
        // The hint's own clock: when it stood up (false→true edge) so `hint_alpha_target` can
        // tell how long it has been up continuously, cleared the moment it goes away so a later
        // reappearance starts a fresh peak rather than resuming an old one.
        self.hint_since = hint.then(|| self.hint_since.unwrap_or(now));
        let hint_elapsed = self.hint_since.map(|since| now.wrapping_sub(since));
        let moved = super::ease(&mut self.alpha, f32::from(shown), dt)
            | super::ease(&mut self.hint, hint_alpha_target(hint, hint_elapsed), dt);
        // A visible transport over a RUNNING trailer is a moving clock and a moving playbar, so it
        // owes the frame gate a report even when no alpha changed this tick. A paused one is
        // static and deliberately owes nothing.
        moved || (shown && !paused)
    }

    /// What the state read-out wears. `Busy::None`: a committed seek's reload is a fresh Starfish
    /// `Load`, not the in-place seek machinery `Busy::Transport` describes, and `view.picture`
    /// staying true across it (`player::preview::View`'s own doc) means the transport never
    /// observes it as a wait — the FastForward/Rewind marks below come from the scrub preview's
    /// own travel instead, exactly the `scrubbing`/`travel_ns` arm [`transport_mark`] carries for
    /// the player HUD's pointer drag.
    ///
    /// [`transport_mark`]: crate::ui::player_hud::transport_mark
    pub(super) fn mark(&self, paused: bool, live_ns: i64) -> TransportMark {
        let scrubbing = self.scrubbing();
        crate::ui::player_hud::transport_mark(
            paused,
            crate::ui::player_hud::Busy::None,
            scrubbing,
            if scrubbing { self.scrub_ns } else { live_ns },
            live_ns,
            self.play_at.map(|at| self.now_ms.wrapping_sub(at)),
        )
    }

    /// Draw the trailer transport. `film_title` is the ITEM's own (the trailer extra's parent);
    /// `extra_title` is the trailer extra's own PMS title. [`transport_title`] picks which one
    /// actually goes under the `Trailer` kicker — the same pairing the player HUD gives a trailer
    /// played as a feature. Nothing is drawn once it has faded out.
    ///
    /// While a LEFT/RIGHT scrub is in flight the playbar and the elapsed/remaining read-out follow
    /// the PREVIEW position instead of the live one — same as the player HUD's scrubber — falling
    /// back to the live position the instant the gesture ends (commit or cancel alike): a commit's
    /// own `engine::arm_seek` republishes the live position at the target immediately, so there is
    /// no frame where the two disagree.
    pub(super) fn draw(
        &self,
        p: Painter,
        film_title: &str,
        extra_title: &str,
        paused: bool,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        if self.alpha <= 0.01 {
            return;
        }
        let p = p.alpha(self.alpha);
        crate::ui::player_hud::draw_scrim(p);
        // `if let Ok`, not `.unwrap_or_default()` — the player HUD's own rule (5a221a54): a title
        // with an interior NUL skips the title block rather than drawing an empty line under the
        // kicker. The transport below it still draws; only the unprintable text is dropped.
        if let Ok(title) = CString::new(transport_title(film_title, extra_title)) {
            crate::ui::player_hud::draw_title(
                p,
                crate::ui::player_hud::Kicker::Context(crate::metadata::TRAILER_CONTEXT_C.as_ptr()),
                title.as_ptr(),
            );
        }
        let live_ns = crate::player::playpos_ns();
        crate::ui::player_hud::draw_playbar(
            p,
            Playbar {
                pos_ns: if self.scrubbing() { self.scrub_ns } else { live_ns },
                dur_ns: crate::player::duration_ns(),
                // The trailer's playbar is not FOCUSABLE and a pointer cannot drag it — the tick
                // knob everywhere else on this transport is a read-out — but LEFT/RIGHT do move
                // it now, same as the player HUD's own scrubber.
                knob: Knob::Tick,
                mark: self.mark(paused, live_ns),
                now: self.now_ms,
            },
            measure,
        );
    }

    /// Draw the "`[^] Full screen`" hint, HORIZONTALLY CENTRED on the screen and vertically
    /// centred on `cy` — the shared [`KeyHint`], wearing the remote's own arrow rather than the
    /// word UP, glyph FIRST and no `Press`/`for` filler (2026-09-17 shortening: same predicate,
    /// same placement, words only). `cy` is the caller's: since 2026-09-18 it shares the header
    /// line's own centre (the logo/title row) rather than sitting under the action row, so the
    /// hint reads as a third element on that line — logo, hint, whatever else shares it — and not
    /// as page furniture pushed down over the video.
    pub(super) fn draw_hint(&self, p: Painter, cy: f32, measure: &dyn crate::ui::machine::Measure) {
        if self.hint <= 0.01 {
            return;
        }
        let hint = KeyHint::glyph(c"", Icon::ChevronUp, c"Full screen");
        let x = hint_cx(hint.width(measure));
        hint.draw(p.alpha(self.hint), x, cy, measure);
    }
}

/// Where the hint's LEFT edge lands for a line `width` wide, so the whole line is horizontally
/// centred on the screen. Split out from [`Transport::draw_hint`] so the centring itself is
/// host-testable without a `Measure` or a live draw.
pub(super) fn hint_cx(width: f32) -> f32 {
    (consts::SCR_W - width) * 0.5
}

/// Where the hint sits VERTICALLY: dead centre of the header row `[row_top, row_top + row_h)` —
/// the same line the preview-shrunk logo/title occupies at the top of the screen (and, per the
/// mock, whatever sits at that row's right edge). Centring on the row rather than anchoring to its
/// top or bottom is what keeps a taller glyph+label stack from growing down over the video: a
/// `KeyHint` is already vertically symmetric about its own `cy` ([`KeyHint::draw`]'s contract), so
/// handing it the row's centre is the whole fix.
pub(super) fn hint_cy(row_top: f32, row_h: f32) -> f32 {
    row_top + row_h * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::Key as MKey;

    /// **The "Trailer / Trailer" duplicate case.** When the extra's own PMS title is the
    /// boilerplate "Trailer" (however it's cased/spaced — most servers scan trailers this way),
    /// the transport must show the FILM/SHOW title instead, so it doesn't repeat the kicker word
    /// right under it. An empty extra title (no extra resolved yet) falls back the same way.
    #[test]
    fn transport_title_falls_back_to_the_film_title_when_the_extras_own_title_is_the_kicker() {
        for extra in ["Trailer", "trailer", " TRAILER ", "  Trailer  ", ""] {
            assert_eq!(
                transport_title("Inception", extra),
                "Inception",
                "extra_title={extra:?}"
            );
        }
    }

    /// **The informative case.** An extra with its own distinct title (a numbered teaser, an
    /// international cut, …) says something the "Trailer" kicker doesn't, so it wins over the
    /// film title.
    #[test]
    fn transport_title_uses_the_extras_own_title_when_it_differs_meaningfully_from_the_kicker() {
        for extra in ["Official Trailer", "Teaser 2", "International Trailer"] {
            assert_eq!(
                transport_title("Inception", extra),
                extra,
                "extra_title={extra:?}"
            );
        }
    }

    /// The key policy, key by key. `Reveal` is the default for a direction the mode owns; the two
    /// collapse keys and the three transport keys are the exceptions, and EXIT/STOP are nobody's.
    #[test]
    fn the_mode_owns_the_transport_and_direction_keys_and_nothing_else() {
        assert_eq!(trailer_key(MKey::Ok, 0, 0), Some(TrailerKey::Toggle));
        assert_eq!(trailer_key(MKey::Back, 0, 0), Some(TrailerKey::Collapse));
        assert_eq!(trailer_key(MKey::Down, 0, 0), Some(TrailerKey::Collapse));
        assert_eq!(trailer_key(MKey::Up, 0, 0), Some(TrailerKey::Reveal));
        assert_eq!(trailer_key(MKey::Left, 0, 0), Some(TrailerKey::Scrub(false)));
        assert_eq!(trailer_key(MKey::Right, 0, 0), Some(TrailerKey::Scrub(true)));
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_PLAYPAUSE),
            Some(TrailerKey::Toggle)
        );
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_PLAY),
            Some(TrailerKey::Play)
        );
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_PAUSE),
            Some(TrailerKey::Pause)
        );
        // A transport-coded LEFT/RIGHT (the remote's REW/FF) scrubs exactly like its plain
        // twin — `consts::Key::Left { .. }`/`Right { .. }` do not distinguish `alt`, and
        // `player::preview::seek`'s admitted-reload accounting makes the reload hazard the old
        // "reveal, never seek" rule guarded against safe to take from either key.
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_FASTFORWARD),
            Some(TrailerKey::Scrub(true))
        );
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_REWIND),
            Some(TrailerKey::Scrub(false))
        );
        assert_eq!(trailer_key(MKey::Other, 0, consts::WCODE_EXIT), None);
        assert_eq!(trailer_key(MKey::Other, 0, consts::WCODE_STOP), None);
        assert_eq!(trailer_key(MKey::Other, 0, 0), None, "an unbound key is not ours");
    }

    /// **Requirement 4's key-ladder half: a fresh LEFT/RIGHT hops the fixed step, and a plain tap
    /// (key-up with no auto-repeat ever arriving) does not commit at once** — it arms the
    /// [`TAP_COMMIT_MS`](crate::ui::player_hud::TAP_COMMIT_MS) debounce instead, so a rapid burst
    /// of taps coalesces into one seek rather than issuing a reload per press.
    #[test]
    fn a_fresh_press_hops_the_fixed_step_and_a_tap_arms_the_debounce_instead_of_committing_at_once()
    {
        let mut t = Transport::IDLE;
        let dur = 120_000_000_000i64;
        let live = 30_000_000_000i64;

        t.scrub_fresh(true, 0, dur, live);
        assert!(t.scrubbing(), "a fresh press starts a gesture");
        assert_eq!(
            t.scrub_ns,
            live + crate::ui::player_hud::SCRUB_STEP_NS,
            "one fixed hop forward from the live position"
        );

        assert_eq!(t.scrub_release(0), None, "a tap does not commit at once");
        assert!(t.scrubbing(), "the preview stays up while the debounce runs");
        assert_eq!(t.step_tap_commit(100), None, "the debounce has not elapsed yet");

        let target = t.step_tap_commit(crate::ui::player_hud::TAP_COMMIT_MS);
        assert_eq!(
            target,
            Some(live + crate::ui::player_hud::SCRUB_STEP_NS),
            "the debounce commits the accumulated target once it elapses"
        );
        assert!(!t.scrubbing(), "committing ends the gesture");
    }

    /// **Requirement 4's other half: a HELD LEFT/RIGHT accumulates continuously via the auto-repeat
    /// ramp, and commits AT ONCE on key-up** — unlike a tap, a hold does not wait for the debounce,
    /// matching the player HUD's own `key_scrub_release`.
    #[test]
    fn a_held_press_advances_continuously_and_commits_at_once_on_release() {
        let mut t = Transport::IDLE;
        let dur = 600_000_000_000i64;
        let live = 100_000_000_000i64;

        t.scrub_fresh(true, 0, dur, live);
        let after_hop = t.scrub_ns;
        let mut now = 0u32;
        t.scrub_repeat(now);
        // The hardware auto-repeat pulses roughly every 100ms; `step_scrub_hold` is stepped every
        // frame (16ms) in between, same cadence `preview_tick` drives it at. Each repeat refreshes
        // `scrub_alive`, well inside `SCRUB_LOST_MS` (400ms), so the ramp must never self-commit.
        for _ in 0..5 {
            for _ in 0..6 {
                now += FRAME_MS;
                assert_eq!(
                    t.step_scrub_hold(now, dur),
                    None,
                    "a live repeat stream must not self-commit"
                );
            }
            t.scrub_repeat(now);
        }
        assert!(t.scrub_ns > after_hop, "the hold ramp advances the preview forward");

        let before_release = t.scrub_ns;
        assert_eq!(
            t.scrub_release(now),
            Some(before_release),
            "a held direction commits at once on key-up"
        );
        assert!(!t.scrubbing(), "committing ends the gesture");
    }

    /// The lost-keyup safety net: if the hardware auto-repeat stream silently stops (a dropped
    /// keyup — the same failure mode the player HUD's own `step_scrub_hold` guards against),
    /// `step_scrub_hold` commits on its own once `SCRUB_LOST_MS` has passed with no repeat.
    #[test]
    fn a_dropped_keyup_still_commits_once_the_repeat_stream_goes_quiet() {
        let mut t = Transport::IDLE;
        let dur = 600_000_000_000i64;
        let live = 100_000_000_000i64;

        t.scrub_fresh(false, 0, dur, live);
        t.scrub_repeat(0);
        assert_eq!(t.step_scrub_hold(100, dur), None, "well inside the alive window");
        let target = t.step_scrub_hold(100 + crate::ui::player_hud::SCRUB_LOST_MS + 1, dur);
        assert!(target.is_some(), "no repeat for SCRUB_LOST_MS — the safety net commits");
        assert!(!t.scrubbing(), "committing ends the gesture");
    }

    /// **Requirement 4's auto-hide half: the transport must not auto-hide while a hold-scrub is in
    /// progress.** `step_scrub_hold` reveals on every step it takes (mirroring the player HUD), so
    /// driving it every frame — exactly as `preview_tick` does — must keep `revealed()` true well
    /// past one ordinary [`LINGER_MS`] even though nothing else is refreshing the linger.
    #[test]
    fn the_transport_does_not_auto_hide_while_a_hold_scrub_is_in_progress() {
        let mut t = Transport::IDLE;
        t.reveal();
        let dur = 600_000_000_000i64;
        let live = 100_000_000_000i64;
        let mut now = 0u32;
        t.scrub_fresh(true, now, dur, live);
        t.scrub_repeat(now);
        // 400 frames * 16ms ~= 6.4s, comfortably past LINGER_MS (4.5s) if nothing kept reviving it.
        // A repeat every ~96ms (well inside SCRUB_LOST_MS) keeps the hold alive throughout, same
        // as a real hardware auto-repeat stream would.
        for _ in 0..66 {
            for _ in 0..6 {
                now += FRAME_MS;
                t.step_scrub_hold(now, dur);
                t.update(now, FRAME_S, true, false, false);
            }
            t.scrub_repeat(now);
        }
        assert!(t.revealed(), "a live hold must not let the linger expire under it");
    }

    #[test]
    fn the_hint_belongs_to_background_autoplay_alone() {
        assert!(hint_shown(true, false, true), "picture up, unpromoted, hero focused");
        assert!(!hint_shown(true, true, true), "promoted: the hint's own mode is on");
        assert!(!hint_shown(false, false, true), "no picture, nothing to go full screen with");
        assert!(!hint_shown(true, false, false), "focus left the hero; UP means something else");
    }

    /// The hint's own anchor, pure geometry: horizontally centred on the screen for whatever
    /// width its line measures, and vertically dead centre of the header row it now shares with
    /// the preview-shrunk logo (and whatever the mock puts at that row's other end) — not below
    /// the row, not growing down over the video.
    #[test]
    fn the_hint_is_centred_on_the_screen_and_on_the_header_rows_own_centre_line() {
        for width in [200.0_f32, 420.0, 640.0] {
            let x = hint_cx(width);
            assert_eq!(
                x + width * 0.5,
                consts::SCR_W * 0.5,
                "width={width}: the line's own centre must land on the screen's centre"
            );
        }
        for (row_top, row_h) in [(54.0_f32, 54.0), (0.0, 100.0), (12.0, 36.0)] {
            let cy = hint_cy(row_top, row_h);
            assert_eq!(
                cy, row_top + row_h * 0.5,
                "the hint shares the header row's own centre line, not an offset below it"
            );
            assert!(cy > row_top, "the centre line sits inside the row, not above it");
            assert!(cy < row_top + row_h, "…nor below it");
        }
    }

    #[test]
    fn a_paused_trailer_keeps_its_controls_and_a_playing_one_auto_hides() {
        assert!(controls_shown(true, 1_000, 5_500, false), "inside the linger");
        assert!(!controls_shown(true, 6_000, 5_500, false), "the linger ran out");
        assert!(controls_shown(true, 6_000, 5_500, true), "paused outlives the deadline");
        assert!(!controls_shown(false, 1_000, 5_500, true), "not in full-trailer mode at all");
    }

    /// One frame at 60 Hz, as the page's `preview_tick` delivers it: an absolute clock and the
    /// seconds between two readings of it.
    const FRAME_MS: u32 = 16;
    const FRAME_S: f32 = FRAME_MS as f32 / 1000.0;

    /// Run `frames` frames from `now`, answering the last one's motion report and the clock it
    /// ended at.
    fn run(
        t: &mut Transport,
        now: u32,
        frames: u32,
        full_trailer: bool,
        paused: bool,
        hint: bool,
    ) -> (bool, u32) {
        let mut moved = false;
        let mut now = now;
        for _ in 0..frames {
            now += FRAME_MS;
            moved = t.update(now, FRAME_S, full_trailer, paused, hint);
        }
        (moved, now)
    }

    #[test]
    fn the_transport_fades_in_on_a_reveal_and_out_when_the_mode_ends() {
        let mut t = Transport::IDLE;
        t.reveal();
        assert!(t.revealed(), "a reveal opens the linger");
        let (_, now) = run(&mut t, 0, 100, true, false, false);
        assert!(t.alpha > 0.9, "alpha={}", t.alpha);
        assert!(t.revealed(), "100 frames is well inside one linger");
        // Collapsing the mode dismisses it, and it fades back out.
        let (_, _) = run(&mut t, now, 200, false, false, false);
        assert!(t.alpha < 0.01, "alpha={}", t.alpha);
        assert!(!t.revealed(), "the mode took its controls with it");
    }

    /// The auto-hide: a playing trailer's controls leave after one linger, and the animator goes
    /// quiet once they are gone — the second half is the idle-gate obligation every animator here
    /// owes (`ui/CLAUDE.md`).
    #[test]
    fn a_playing_trailer_hides_its_controls_and_then_stops_reporting() {
        let mut t = Transport::IDLE;
        t.reveal();
        let (motion, _) = run(&mut t, 0, 1000, true, false, false);
        assert!(!t.revealed(), "the linger expired");
        assert!(t.alpha < 0.01, "the controls faded out");
        assert!(!motion, "a hidden transport must not hold the frame gate open");
        // A PAUSED one is up and static: still no motion, but still on screen.
        let mut paused = Transport::IDLE;
        paused.reveal();
        let (motion, _) = run(&mut paused, 0, 1000, true, true, false);
        assert!(paused.alpha > 0.9, "a paused trailer keeps its controls");
        assert!(!motion, "…and a still picture with a still clock reports nothing");
    }

    /// **A pause does not spend the linger.** The deadline travels with the paused frames, so the
    /// resume that follows finds the controls up and keeps them up for the rest of their time,
    /// rather than hiding them on the frame the picture starts moving again.
    #[test]
    fn the_linger_does_not_run_while_the_trailer_is_paused() {
        let mut t = Transport::IDLE;
        t.reveal();
        // Sit paused for well over one linger…
        let (_, now) = run(&mut t, 0, 600, true, true, false);
        assert!(t.alpha > 0.9, "still up, because paused");
        // …then resume: the controls are still inside their deadline.
        let (_, _) = run(&mut t, now, 1, true, false, false);
        assert!(t.revealed(), "the pause must not have spent the linger");
    }

    /// The resume mark: [`TransportMark::Play`] stands for [`PLAY_MARK_MS`] after the
    /// paused→playing edge and then gets out of the way, exactly as it does on the player.
    #[test]
    fn the_read_out_marks_pause_always_and_a_resume_only_briefly() {
        let mut t = Transport::IDLE;
        t.reveal();
        let (_, now) = run(&mut t, 0, 1, true, true, false);
        assert_eq!(t.mark(true, 0), TransportMark::Pause);
        // the resume edge
        let (_, now) = run(&mut t, now, 1, true, false, false);
        assert_eq!(t.mark(false, 0), TransportMark::Play);
        let (_, _) = run(&mut t, now, 200, true, false, false);
        assert_eq!(
            t.mark(false, 0),
            TransportMark::None,
            "a play glyph held for the whole trailer says nothing"
        );
    }

    /// **The hint fades on its own eased clock, same as the transport row.** `update`'s `hint`
    /// argument feeds the same `ease` as `alpha`, but every `run` call above this test passes
    /// `hint=false`, so nothing ever moved the field at all: a flag that never changes would pass
    /// just as well as a real fade. Driven here in the background-autoplay context the hint
    /// actually appears in (`full_trailer=false`, mirroring `hint_shown`'s own case).
    #[test]
    fn the_hint_fades_in_and_out_on_its_own_eased_clock() {
        let mut t = Transport::IDLE;
        assert_eq!(t.hint, 0.0, "starts hidden");
        let (_, now) = run(&mut t, 0, 100, false, false, true);
        assert!(
            t.hint > HINT_ALPHA_PEAK - 0.05,
            "hint={} should have eased up near its peak {}", t.hint, HINT_ALPHA_PEAK
        );
        let (_, _) = run(&mut t, now, 100, false, false, false);
        assert!(t.hint < 0.01, "hint={} should have eased back out", t.hint);
    }

    /// **The dim-after-a-few-seconds behaviour.** After standing at peak for one HUD
    /// [`LINGER_MS`] — the same beat the transport's own controls linger for — the hint eases
    /// DOWN to its quieter [`HINT_ALPHA_REST`] rather than staying at full strength or cutting
    /// out, and then goes quiet for the idle gate once it has settled there (the same discipline
    /// [`the_hint_fades_in_and_out_on_its_own_eased_clock`] already pins for the appear/leave
    /// edges).
    #[test]
    fn the_hint_dims_to_its_resting_alpha_after_one_linger_and_then_goes_quiet() {
        let mut t = Transport::IDLE;
        // Long enough to clear one linger AND let the ease converge on the dimmer target.
        let frames = LINGER_MS / FRAME_MS + 100;
        let (_, now) = run(&mut t, 0, frames, false, false, true);
        assert!(
            (t.hint - HINT_ALPHA_REST).abs() < 0.01,
            "hint={} should have settled on the resting alpha {}", t.hint, HINT_ALPHA_REST
        );
        let moved = t.update(now + FRAME_MS, FRAME_S, false, false, true);
        assert!(!moved, "a settled hint must not hold the frame gate open");
    }

    /// A hint that goes away and comes back (focus left the hero and returned, or the picture
    /// dropped and resumed) starts its peak/dim clock over — it does not resume dimming from
    /// wherever the last visit left off.
    #[test]
    fn a_hint_that_reappears_restarts_at_peak_rather_than_resuming_its_old_dim_clock() {
        let mut t = Transport::IDLE;
        let frames = LINGER_MS / FRAME_MS + 100;
        let (_, now) = run(&mut t, 0, frames, false, false, true);
        assert!((t.hint - HINT_ALPHA_REST).abs() < 0.01, "settled dim before the gap");
        // Hidden for a while (focus left the hero)...
        let (_, now) = run(&mut t, now, 150, false, false, false);
        assert!(t.hint < 0.01, "fully hidden in the gap");
        // ...and shown again: a fresh peak, not a resumed dim.
        let (_, _) = run(&mut t, now, 30, false, false, true);
        assert!(
            t.hint > HINT_ALPHA_REST + 0.05,
            "hint={} should be climbing back toward peak, not sitting at the old resting value",
            t.hint
        );
    }
}
