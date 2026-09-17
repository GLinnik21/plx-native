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
    /// anything else the mode owns — it puts the controls back on screen and does nothing more.
    Reveal,
}

/// PURE: what `key` does in full-trailer mode.
///
/// `sym`/`wcode` are the raw pair, classified by [`consts::classify`] for the keys the four-way
/// `machine::Key` alphabet cannot name (PLAY, PAUSE, PLAYPAUSE and the FF/REW alternates). The
/// machine key is preferred where it HAS an answer, because that is the value the input engine
/// itself navigated by.
///
/// **LEFT/RIGHT reveal; they do not seek.** A preview session's seek would fall back to a fresh
/// `Load` on several paths (`INPLACE_SEEK_OK` cleared, a refused or stale native Pause, a stuck
/// in-place seek) and a preview's Loads are budgeted and breaker-guarded one at a time by
/// `player::preview::Machine` — a reload spends a Starfish slot the machine never admitted, and a
/// reload that then fails is charged to the machine as an admitted-Load failure, which opens the
/// process-wide breaker for every later item. See `player/preview.rs`'s module doc.
pub(super) fn trailer_key(
    key: crate::ui::machine::Key,
    sym: u32,
    wcode: u32,
) -> Option<TrailerKey> {
    use crate::ui::machine::Key as MKey;
    let key = match key {
        MKey::Up | MKey::Left | MKey::Right => return Some(TrailerKey::Reveal),
        MKey::Down | MKey::Back => return Some(TrailerKey::Collapse),
        MKey::Ok => return Some(TrailerKey::Toggle),
        MKey::Other => consts::classify(sym, wcode),
    };
    match key {
        consts::Key::Play => Some(TrailerKey::Play),
        consts::Key::Pause => Some(TrailerKey::Pause),
        consts::Key::PlayPause | consts::Key::Ok => Some(TrailerKey::Toggle),
        consts::Key::Up | consts::Key::Left { .. } | consts::Key::Right { .. } => {
            Some(TrailerKey::Reveal)
        }
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
    /// the UP hint's eased alpha
    pub(super) hint: f32,
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
}

impl Transport {
    pub(super) const IDLE: Self = Self {
        until_ms: 0,
        alpha: 0.0,
        hint: 0.0,
        play_at: None,
        was_paused: false,
        now_ms: 0,
    };

    /// A key arrived: put the controls back on screen for a full linger.
    pub(super) fn reveal(&mut self) {
        self.until_ms = self.now_ms.wrapping_add(LINGER_MS);
    }

    /// Full-trailer mode is over: the transport goes with it, and the resume mark it was carrying
    /// belongs to a session the viewer has stopped looking at.
    pub(super) fn dismiss(&mut self) {
        self.until_ms = 0;
        self.play_at = None;
    }

    /// Is the linger still running? Read by the page's own tests instead of the deadline, which
    /// says nothing without the clock reading it was taken from.
    #[cfg(test)]
    pub(super) fn revealed(&self) -> bool {
        self.now_ms < self.until_ms
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
        let moved = super::ease(&mut self.alpha, f32::from(shown), dt)
            | super::ease(&mut self.hint, f32::from(hint), dt);
        // A visible transport over a RUNNING trailer is a moving clock and a moving playbar, so it
        // owes the frame gate a report even when no alpha changed this tick. A paused one is
        // static and deliberately owes nothing.
        moved || (shown && !paused)
    }

    /// What the state read-out wears. `Busy::None`: a preview never seeks and has no scrub
    /// gesture, so the two marks that need one cannot arise, and its buffering is not a wait the
    /// viewer asked for.
    pub(super) fn mark(&self, paused: bool) -> TransportMark {
        crate::ui::player_hud::transport_mark(
            paused,
            crate::ui::player_hud::Busy::None,
            false,
            0,
            0,
            self.play_at.map(|at| self.now_ms.wrapping_sub(at)),
        )
    }

    /// Draw the trailer transport. `film_title` is the ITEM's own (the trailer extra's parent);
    /// `extra_title` is the trailer extra's own PMS title. [`transport_title`] picks which one
    /// actually goes under the `Trailer` kicker — the same pairing the player HUD gives a trailer
    /// played as a feature. Nothing is drawn once it has faded out.
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
        let kicker = CString::new(crate::metadata::TRAILER_CONTEXT).unwrap_or_default();
        let title = CString::new(transport_title(film_title, extra_title)).unwrap_or_default();
        crate::ui::player_hud::draw_scrim(p);
        crate::ui::player_hud::draw_title(
            p,
            crate::ui::player_hud::Kicker::Context(kicker.as_ptr()),
            title.as_ptr(),
        );
        crate::ui::player_hud::draw_playbar(
            p,
            Playbar {
                pos_ns: crate::player::playpos_ns(),
                dur_ns: crate::player::duration_ns(),
                // Nothing focuses the trailer's playbar and nothing scrubs it: it is a READ-OUT
                // of where the trailer is, which is what the thin tick means everywhere else too.
                knob: Knob::Tick,
                mark: self.mark(paused),
                now: self.now_ms,
            },
            measure,
        );
    }

    /// Draw the "press UP for full screen" hint, its line starting at `x` and centred on `cy` —
    /// the shared [`KeyHint`], wearing the remote's own arrow rather than the word UP.
    pub(super) fn draw_hint(
        &self,
        p: Painter,
        x: f32,
        cy: f32,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        if self.hint <= 0.01 {
            return;
        }
        KeyHint::glyph(c"Press", Icon::ChevronUp, c"for full screen").draw(
            p.alpha(self.hint),
            x,
            cy,
            measure,
        );
    }
}

/// How much air the hint keeps between itself and the action row it follows — one block step
/// ([`theme::space::MD`]), the same rung every other label-under-a-row in the page uses.
pub(super) const HINT_GAP: f32 = theme::space::MD;

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
        assert_eq!(trailer_key(MKey::Left, 0, 0), Some(TrailerKey::Reveal));
        assert_eq!(trailer_key(MKey::Right, 0, 0), Some(TrailerKey::Reveal));
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
        // A transport-coded LEFT/RIGHT (the remote's REW/FF) reveals like its plain twin — it
        // must NOT seek, for the reload reasons in `trailer_key`'s own doc.
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_FASTFORWARD),
            Some(TrailerKey::Reveal)
        );
        assert_eq!(
            trailer_key(MKey::Other, 0, consts::WCODE_REWIND),
            Some(TrailerKey::Reveal)
        );
        assert_eq!(trailer_key(MKey::Other, 0, consts::WCODE_EXIT), None);
        assert_eq!(trailer_key(MKey::Other, 0, consts::WCODE_STOP), None);
        assert_eq!(trailer_key(MKey::Other, 0, 0), None, "an unbound key is not ours");
    }

    #[test]
    fn the_hint_belongs_to_background_autoplay_alone() {
        assert!(hint_shown(true, false, true), "picture up, unpromoted, hero focused");
        assert!(!hint_shown(true, true, true), "promoted: the hint's own mode is on");
        assert!(!hint_shown(false, false, true), "no picture, nothing to go full screen with");
        assert!(!hint_shown(true, false, false), "focus left the hero; UP means something else");
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
        assert_eq!(t.mark(true), TransportMark::Pause);
        // the resume edge
        let (_, now) = run(&mut t, now, 1, true, false, false);
        assert_eq!(t.mark(false), TransportMark::Play);
        let (_, _) = run(&mut t, now, 200, true, false, false);
        assert_eq!(
            t.mark(false),
            TransportMark::None,
            "a play glyph held for the whole trailer says nothing"
        );
    }
}
