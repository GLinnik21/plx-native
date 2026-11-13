//! The **Skip Intro / Skip Credits** control. While the playhead sits inside one of the playing
//! leaf's server markers (`metadata::playing_markers`, fetched with `?includeMarkers=1`) this
//! button TAKES THE PLACE of the transport's Subtitles/Audio discs — same row, same right edge,
//! same height — and the discs return the moment the segment ends.
//!
//! Replacing them rather than joining them is the design call: one wide unmissable target in the
//! spot the eye already goes for transport controls. The cost is that CC/Audio are covered for the
//! length of the segment, so BACK over the pill DISMISSES the offer for that segment and puts the
//! discs back (`PlayerScreen::dismiss_offer` asks `DismissOffer` for Manual, or `CancelAutoSkip`
//! for Automatic; both retire the segment in the metadata store) — the row stays at five
//! controls and the viewer never has to wait an intro out to change tracks. The button only exists
//! while the HUD is up, which is why `app.rs` RAISES the HUD once when a segment begins and parks
//! focus here. Manual skips on OK. Automatic is a PAIR, like Up Next's: the filled button is the skip the
//! five-second clock will perform (OK performs it now, and focus starts on it), and the unfilled Watch button
//! one key to its left cancels the clock.
#![allow(dead_code)]
use plx_data::metadata::{self, MarkerKind};
use plx_ui::theme;
use plx_ui::widgets::{Button, ControlGround};
use plx_ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_int;

/// The skip performed by a Manual press or Automatic expiry. A Watch press cancels instead.
/// The distinction is the `final` flag on a credits marker: an
/// ordinary segment is a seek and playback continues past it, but a `final` one runs to the end of
/// the item, so "skip" means **the episode is over** — seeking to its end would race the decoder
/// against its own last frames for no benefit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SkipAction {
    /// jump to this position (ns) and keep playing
    Seek(i64),
    /// the item is finished — hand off to the end-of-playback path (next episode, or leave)
    Finish,
}

/// The offer the button is currently making. Carries the marker's TYPED kind, not its label:
/// asking "is the playhead in a credits segment?" by string-comparing user-facing copy made a pure
/// copy-edit — the sort of thing a design pass does — silently disable Up Next and auto-advance,
/// with no compile error and nothing to fail.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Prompt {
    /// the segment itself — carried so activating the button can retire it (`metadata::mark_skipped`)
    pub marker: metadata::Marker,
    pub kind: MarkerKind,
    pub action: SkipAction,
    pub automatic: bool,
}

impl Prompt {
    /// The FILLED button's copy — the action the prompt performs (a press now, or the clock at its
    /// end): Skip intro / Skip credits, for the manual pill and the Automatic prompt alike.
    pub fn label(&self) -> &'static str {
        match self.kind {
            MarkerKind::Intro => plx_platform::i18n::msg::widgets_skip_intro(),
            MarkerKind::Credits => plx_platform::i18n::msg::widgets_skip_credits(),
        }
    }
    /// The Automatic prompt's unfilled button, which cancels the countdown: Watch intro / Watch credits.
    /// Item 0 of the row, to the left of the filled action ([`crate::up_next::BTN_CREDITS`]).
    pub fn watch_label(&self) -> &'static str {
        match self.kind {
            MarkerKind::Intro => plx_platform::i18n::msg::widgets_watch_intro(),
            MarkerKind::Credits => plx_platform::i18n::msg::widgets_next_credits(),
        }
    }
}

/// PURE: the offer a given segment makes. Takes the marker rather than reading the playhead, so
/// the precedence it feeds ([`crate::player_hud::slot_for`]) is host-testable and the whole frame
/// decides from ONE playhead sample — `playpos_ns` is written by LG's media thread, and re-reading
/// it per call site let the input path and the draw path disagree within a single frame.
///
/// Markers belong to the PLAYING leaf (`metadata::playing()`), never to `metadata::current()`:
/// during a show-page episode play `current()` is the SHOW, and offering episode 1's intro timing
/// during episode 5 is the same identity bug the track store exists to prevent.
pub fn prompt_for(m: metadata::Marker) -> Prompt {
    Prompt {
        marker: m,
        automatic: false,
        kind: m.kind,
        action: match m.kind {
            // a `final` credits segment runs to the end of the item, so "skip" means the episode is
            // over — seeking to its end would race the decoder against its own last frames
            MarkerKind::Credits if m.final_seg => SkipAction::Finish,
            _ => SkipAction::Seek(m.end_ms * 1_000_000),
        },
    }
}

/// **Draw the prompt** at the rects the caller already has: the compiled package's pill(s) (`Standin::skip`,
/// one per face, and `Standin::watch` for the Automatic ones), placed at compile time, in the control row's
/// place. The manual prompt is the one pill. The Automatic prompt is the Up Next pair's arrangement: the
/// unfilled Watch button (item 0) cancels, the FILLED Skip (item 1) carries the countdown sweep and is what
/// the clock performs, so pressing it acts now; `btn` says which one holds the ring.
#[allow(clippy::too_many_arguments)]
pub fn draw_at(row: &crate::player_hud::motion::HudMotion, p: Painter, pr: Prompt, slot: Rect, watch: Option<Rect>, focused: bool, btn: c_int, progress: Option<f32>, press: plx_machine::machine::PressRead<u32>) {
    let Ok(label) = CString::new(pr.label()) else {
        return;
    };
    // No leading icon: the label alone carries it, and a chevron on a control that does not
    // navigate anywhere was reading as "more" rather than "skip".
    let (pop, ring) = if pr.automatic { (row.scale(crate::up_next::BTN_NEXT, press), focused && btn == crate::up_next::BTN_NEXT) } else { (row.scale(0, press), focused) };
    // It stands in the transport discs' own slot, over the video plane and on the HUD's ramp,
    // so it wears their ground as well as their pop — see `ControlGround`.
    let button = Button::new(label.as_ptr(), theme::size::BODY, slot).scale(pop).focused(ring).ground(ControlGround::Unkeyed);
    let button = if let Some(fraction) = progress { button.progress(fraction) } else { button };
    button.draw(&Env::inert(), p);
    if let (true, Some(watch), Ok(cancel)) = (pr.automatic, watch, CString::new(pr.watch_label())) {
        Button::new(cancel.as_ptr(), theme::size::BODY, watch)
            .scale(row.scale(crate::up_next::BTN_CREDITS, press))
            .focused(focused && btn == crate::up_next::BTN_CREDITS)
            .ground(ControlGround::Unkeyed)
            .draw(&Env::inert(), p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(kind: MarkerKind) -> metadata::Marker {
        metadata::Marker { kind, start_ms: 1_000, end_ms: 60_000, final_seg: false }
    }

    /// The Automatic prompt draws BOTH buttons, the filled one reading the skip it performs; the manual
    /// pill is the one. Either way the label of the filled button is the action, never "Watch".
    #[test]
    fn the_automatic_prompt_draws_watch_beside_the_filled_skip_and_the_manual_one_the_pill_alone() {
        let _serial = plx_base::testlock::serial();
        let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let row = crate::player_hud::motion::HudMotion::new();
        let (watch, skip) = (Rect::new(900.0, 800.0, 200.0, 64.0), Rect::new(1116.0, 800.0, 232.0, 64.0));
        for (kind, filled, cancel) in [(MarkerKind::Intro, "Skip Intro", "Watch Intro"), (MarkerKind::Credits, "Skip Credits", "Watch Credits")] {
            let drawn = |automatic: bool| {
                let prompt = Prompt { automatic, ..prompt_for(marker(kind)) };
                let mut runs = Vec::new();
                plx_ui::record_walk(|| plx_ui::draw_census::capture(|| {
                    runs = plx_ui::draw_census::capture_runs(|| {
                        draw_at(&row, Painter::root(), prompt, skip, automatic.then_some(watch), true, crate::up_next::BTN_NEXT, automatic.then_some(0.4), Default::default());
                    });
                }));
                runs
            };
            let texts = |automatic| drawn(automatic).into_iter().map(|r| r.text).collect::<Vec<_>>();
            assert_eq!(texts(false), [filled], "{kind:?}: the manual prompt is the one pill");
            assert_eq!(texts(true).len(), 2, "{kind:?}: the Automatic prompt is a pair");
            assert!(texts(true).contains(&filled.to_owned()) && texts(true).contains(&cancel.to_owned()), "{:?}", texts(true));
        }
    }
}
