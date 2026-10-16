//! The page's full-screen read-outs, anchored at `StatusOverlay::FULL_ANCHOR_TOP` (540), DS3 bright
//! verdict, no glyph (plan §1.2; boards WatchlistEmpty, WatchlistFiltered).
//!
//! The faces are the shipped `Failed` read-out's, read off `StatusOverlay::verdict_face` and
//! `reason_face` (bold `TITLE` in `TEXT_PRIMARY`, then `BODY` in `TEXT_SECONDARY`), so a change to
//! the design system's read-out moves these with it. The boards draw the same two lines at the same
//! y (540 and 622); this module places them by the shipped bands (the reason `space::SM` under the
//! verdict's line box), which is the position `StatusOverlay` gives a page-placed read-out.
use plx_machine::machine::Measure;
use plx_ui::label::{HAlign, Label};
use plx_ui::text_view::TextView;
use plx_ui::widgets::{LegacyMeasure, StatusKind, StatusOverlay};
use plx_ui::{theme, Painter, Rect};
use std::ffi::CString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readout {
    /// The list is empty.
    Empty,
    /// The filters narrowed the list to nothing.
    Filtered,
    /// Q2: a managed profile without a plex.tv token has no list to read.
    NoToken,
}

impl Readout {
    /// The verdict sentence.
    /// TODO(integrator): `NoToken` has no catalog key yet ("Watchlist isn't available for this
    /// profile"); with `watchlist.empty.no_token` added to the string tables this arm reads it.
    pub fn verdict(self) -> &'static str {
        match self {
            Readout::Empty => plx_platform::i18n::msg::watchlist_empty_list(),
            Readout::Filtered => plx_platform::i18n::msg::watchlist_empty_filtered(),
            Readout::NoToken => "Watchlist isn't available for this profile",
        }
    }

    /// The line under the verdict (the boards' body copy), or none.
    /// TODO(integrator): English until `watchlist.empty.list_body` / `watchlist.empty.filtered_body`
    /// exist in the four string tables; the board texts are "Titles you add to your Watchlist on any
    /// device show up here." and "Change Sort & Filter to see the rest of your Watchlist.".
    pub fn body(self) -> Option<&'static str> {
        match self {
            Readout::Empty => Some("Titles you add to your Watchlist on any device show up here."),
            Readout::Filtered => Some("Change Sort & Filter to see the rest of your Watchlist."),
            Readout::NoToken => None,
        }
    }
}

/// The verdict's and the reason's bands for a read-out drawn into `frame` (the page rect). The
/// verdict's line box starts at [`StatusOverlay::FULL_ANCHOR_TOP`] in screen space whatever the
/// frame says; the reason hangs `space::SM` below it and is [`StatusOverlay::REASON_W`] wide.
pub fn bands(frame: Rect, measure: &dyn Measure) -> (Rect, Rect) {
    let (cap_sz, _, _) = StatusOverlay::verdict_face(StatusKind::Failed);
    let (reason_sz, _) = StatusOverlay::reason_face(StatusKind::Failed);
    let cap = Rect::new(frame.x, StatusOverlay::FULL_ANCHOR_TOP, frame.w, measure.line_h(cap_sz));
    let w = StatusOverlay::REASON_W.min(frame.w);
    let reason = Rect::new(frame.cx() - w * 0.5, cap.y + cap.h + theme::space::SM, w, 2.0 * measure.line_h(reason_sz));
    (cap, reason)
}

/// Draw `readout` into `frame` (the page rect).
pub fn draw_readout(readout: Readout, p: Painter, frame: Rect) {
    let measure = LegacyMeasure;
    let (cap_sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
    let (reason_sz, reason_ink) = StatusOverlay::reason_face(StatusKind::Failed);
    let (cap, reason) = bands(frame, &measure);
    let verdict = CString::new(readout.verdict()).unwrap_or_default();
    let label = Label::new(verdict.as_ptr(), cap_sz, ink).h(HAlign::Center);
    if bold { label.bold() } else { label }.draw(p, cap);
    if let Some(body) = readout.body() {
        TextView::new(body, reason_sz, reason_ink).h(HAlign::Center).max_lines(2).with_measure(&measure).draw(p, reason);
    }
}
