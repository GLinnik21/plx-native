//! The page header: the title (DISPLAY 48 bold, DS8 tracking), the count beside it (Q7) and the one
//! `Sort & Filter · <summary>` pill (plan §1.2).
use plx_ui::{Painter, Rect};

/// What the header shows.
pub struct HeaderFace<'a> {
    pub title: &'a str,
    /// Q7: the count beside the title; `None` while the list has not landed.
    pub count: Option<usize>,
    /// `WatchlistOptionsState::summary`.
    pub summary: &'a str,
    pub pill_focused: bool,
}

/// Draw the header into `frame`. STUB (W45H): draws nothing yet.
pub fn draw_header(_p: Painter, _frame: Rect, _face: &HeaderFace<'_>) {}
