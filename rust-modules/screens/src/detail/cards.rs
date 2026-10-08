//! The four card shelves of [`super::DetailScreen`] (Related, the member collection, Extras and
//! Cast), as content for `plx_ui::cards::Shelf`: each shelf's cards read over the published
//! [`Detail`] and the page's element interning. The screen keeps what is not a card — the
//! headings, the vertical flow, the focus groups and the press actions.
use std::collections::HashMap;

use plx_data::metadata::Detail;
use plx_data::pms::PmsMovie;
use plx_machine::machine::{EntryId, FocusKey, Host, Measure, PressRead};
use plx_ui::cards::{self as ui_cards, RowStyle, TileLabel};
use plx_ui::cards::{CardSource, Tile};
use plx_ui::widgets::Art;
use plx_ui::Painter;

use super::{cast, collection, extras, related};
use crate::registry::tile_facts;

/// Which of the page's shelves a [`Cards`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Which {
    Related,
    Collection,
    Extras,
    Cast,
}

impl Which {
    /// The shelf's tile style, the one its group, its placement and its draw all read.
    pub(super) const fn style(self) -> &'static RowStyle {
        match self {
            Which::Related | Which::Collection => &RowStyle::HOME,
            Which::Extras => &RowStyle::EPISODE,
            Which::Cast => &RowStyle::CAST,
        }
    }
}

/// One shelf's cards. `key_by_local` and `local_by_key` are the page's published projections
/// ([`super::DetailScreen::sync_keys`]): card `i` is the local element `Which`'s `elem(i)` names,
/// shown under the interned engine key the projection maps it to.
pub(super) struct Cards<'a> {
    which: Which,
    d: &'a Detail,
    key_by_local: &'a HashMap<u32, u32>,
    local_by_key: &'a HashMap<u32, u32>,
    len: usize,
    /// The top of the tiles and the live press dip, which the cast names under them need.
    row_y: f32,
    entry: EntryId,
    press: PressRead<u32>,
}

impl<'a> Cards<'a> {
    pub(super) fn new(
        which: Which,
        d: &'a Detail,
        key_by_local: &'a HashMap<u32, u32>,
        local_by_key: &'a HashMap<u32, u32>,
    ) -> Self {
        let mut cards = Self { which, d, key_by_local, local_by_key, len: 0, row_y: 0.0, entry: EntryId(0), press: PressRead::default() };
        let n = match which {
            Which::Related => d.related.len().min(512),
            Which::Collection => collection::len(d),
            Which::Extras => extras::len(d),
            Which::Cast => d.credits_len().min(512),
        };
        // The projections are rebuilt on every landing (`StoreChanged` -> `sync_keys`), but the app
        // pumps the Metadata store and then draws in the same loop turn, one frame before that
        // notice reaches the screen (`app::run`: `loop_requests`, then `draw`). For that draw the
        // published list can outrun the keys: show the keyed prefix, never a card with no key and
        // not the whole shelf blanked (which would also snap a focused pop to rest).
        let keyed = |i: usize| cards.local(i).is_some_and(|local| key_by_local.contains_key(&local));
        cards.len = if n == 0 || keyed(n - 1) { n } else { (0..n).take_while(|&i| keyed(i)).count() };
        cards
    }

    /// Where the shelf's tiles are drawn (`row_y`, in the painter's space) and the frame's press,
    /// whose dip lands on one tile only: the pressed one, focused or not.
    pub(super) fn drawn_at(mut self, row_y: f32, entry: EntryId, press: PressRead<u32>) -> Self {
        self.row_y = row_y;
        self.entry = entry;
        self.press = press;
        self
    }

    fn local(&self, i: usize) -> Option<u32> {
        match self.which {
            Which::Related => related::elem(i),
            Which::Collection => collection::elem(i),
            Which::Extras => extras::elem(i),
            Which::Cast => cast::elem(i),
        }
    }

    fn locate(&self, local: u32) -> Option<usize> {
        match self.which {
            Which::Related => related::locate(local),
            Which::Collection => collection::locate(local),
            Which::Extras => extras::locate(local),
            Which::Cast => cast::locate(local),
        }
    }

    fn movies(&self) -> &'a [PmsMovie] {
        match self.which {
            Which::Related => &self.d.related,
            Which::Collection => collection::members(self.d),
            Which::Extras | Which::Cast => &[],
        }
    }
}

impl<H: Host<Elem = u32>> CardSource<H> for Cards<'_> {
    fn len(&self) -> usize {
        self.len
    }

    fn elem(&self, i: usize) -> u32 {
        let local = self.local(i).unwrap_or_default();
        self.key_by_local.get(&local).copied().unwrap_or(local)
    }

    fn index_of(&self, e: &u32) -> Option<usize> {
        let local = *self.local_by_key.get(e)?;
        self.locate(local).filter(|&i| i < self.len)
    }

    fn art(&self, i: usize) -> Art<'_> {
        match self.which {
            Which::Related | Which::Collection => Art::Poster(self.movies().get(i).map(tile_facts::of)),
            Which::Extras => extras::art(self.d, i),
            Which::Cast => cast::art(self.d, i),
        }
    }

    fn label(&self, i: usize) -> TileLabel {
        match self.which {
            Which::Related | Which::Collection => TileLabel::title(&self.movies()[i].title),
            Which::Extras => extras::label(self.d, i),
            // the names are drawn under every headshot (`overlay`), not as the focused block
            Which::Cast => TileLabel::default(),
        }
    }

    fn progress(&self, i: usize) -> Option<f32> {
        match self.which {
            Which::Related | Which::Collection => self.movies().get(i).and_then(|m| m.resume_frac()),
            Which::Extras | Which::Cast => None,
        }
    }

    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        if self.which != Which::Cast {
            return;
        }
        // The name sits on the slot's own centre, dropped by the focus pop alone: the tile's scale
        // less the press dip it was drawn with (only the pressed tile carries one).
        let slot = ui_cards::tile_rect(i, plx_ui::consts::MARGIN_X, cast::SLOT, 0.0, 0.0,
            (RowStyle::CAST.w, RowStyle::CAST.h));
        let key = FocusKey { entry: self.entry, elem: CardSource::<H>::elem(self, i) };
        let pop = tile.scale / self.press.dip_of(&key);
        cast::draw_label(p, self.d, i, slot.x + RowStyle::CAST.w * 0.5, self.row_y - p.dy(), tile.focused, pop, measure);
    }
}
