//! The **Skip Intro / Skip Credits** control. While the playhead sits inside one of the playing
//! leaf's server markers (`metadata::playing_markers`, fetched with `?includeMarkers=1`) this
//! button TAKES THE PLACE of the transport's Subtitles/Audio discs — same row, same right edge,
//! same height — and the discs return the moment the segment ends.
//!
//! Replacing them rather than joining them is the design call: one wide unmissable target in the
//! spot the eye already goes for transport controls. The cost is that CC/Audio are unreachable for
//! the length of the segment, and that the button only exists while the HUD is up — which is why
//! `app.rs` RAISES the HUD once when a segment begins and parks focus here, so a bare OK still
//! skips in one press instead of three.
#![allow(dead_code)]
use crate::metadata::{self, MarkerKind};
use crate::ui::theme;
use crate::ui::widgets::Button;
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;

/// What pressing the button does. The distinction is the `final` flag on a credits marker: an
/// ordinary segment is a seek and playback continues past it, but a `final` one runs to the end of
/// the item, so "skip" means **the episode is over** — seeking to its end would race the decoder
/// against its own last frames for no benefit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SkipAction {
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
pub(crate) struct Prompt {
    /// the segment itself — carried so activating the button can retire it (`metadata::mark_skipped`)
    pub(crate) marker: metadata::Marker,
    pub(crate) kind: MarkerKind,
    pub(crate) action: SkipAction,
}

impl Prompt {
    /// The button's copy — presentation, derived from the kind at the point of drawing.
    pub(crate) fn label(&self) -> &'static str {
        match self.kind {
            MarkerKind::Intro => "Skip Intro",
            MarkerKind::Credits => "Skip Credits",
        }
    }
}

/// PURE: the offer a given segment makes. Takes the marker rather than reading the playhead, so
/// the precedence it feeds ([`super::player_hud::slot_for`]) is host-testable and the whole frame
/// decides from ONE playhead sample — `playpos_ns` is written by LG's media thread, and re-reading
/// it per call site let the input path and the draw path disagree within a single frame.
///
/// Markers belong to the PLAYING leaf (`metadata::playing()`), never to `metadata::current()`:
/// during a show-page episode play `current()` is the SHOW, and offering episode 1's intro timing
/// during episode 5 is the same identity bug the track store exists to prevent.
pub(crate) fn prompt_for(m: metadata::Marker) -> Prompt {
    Prompt {
        marker: m,
        kind: m.kind,
        action: match m.kind {
            // a `final` credits segment runs to the end of the item, so "skip" means the episode is
            // over — seeking to its end would race the decoder against its own last frames
            MarkerKind::Credits if m.final_seg => SkipAction::Finish,
            _ => SkipAction::Seek(m.end_ms * 1_000_000),
        },
    }
}

/// The button's rect — the SHARED control-row slot, so it and Up Next cannot drift apart.
pub(crate) fn rect(pr: Prompt) -> Rect {
    super::player_hud::ctrl_slot(pr.label())
}

/// Draw the button in the control row. Called by `player_hud` INSTEAD of the two discs.
pub(crate) fn draw(p: Painter, pr: Prompt, focused: bool) {
    let Ok(label) = CString::new(pr.label()) else { return };
    // No leading icon: the label alone carries it, and a chevron on a control that does not
    // navigate anywhere was reading as "more" rather than "skip".
    // Its slot's only item, so index 0 — the pop is the control ROW's (`player_hud::row_pop`),
    // shared with the transport discs this pill stands in for.
    Button::new(label.as_ptr(), theme::size::BODY, rect(pr))
        .scale(crate::ui::player_hud::row_pop(0))
        .focused(focused)
        .draw(&Env::inert(), p);
}
