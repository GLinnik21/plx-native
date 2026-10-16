//! The page's full-screen read-outs, anchored at `StatusOverlay::FULL_ANCHOR_TOP` (540), DS3 bright
//! verdict, no glyph (plan §1.2; boards WatchlistEmpty, WatchlistFiltered).
use plx_ui::{Painter, Rect};

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
    /// TODO(W45R): `NoToken` has no catalog key yet ("Watchlist isn't available for this profile");
    /// the integrator adds `watchlist.empty.no_token` to the string tables and this arm reads it.
    pub fn verdict(self) -> &'static str {
        match self {
            Readout::Empty => plx_platform::i18n::msg::watchlist_empty_list(),
            Readout::Filtered => plx_platform::i18n::msg::watchlist_empty_filtered(),
            Readout::NoToken => "Watchlist isn't available for this profile",
        }
    }
}

/// Draw `readout` into `frame` (the page rect). STUB (W45R): draws nothing yet.
pub fn draw_readout(_readout: Readout, _p: Painter, _frame: Rect) {}
