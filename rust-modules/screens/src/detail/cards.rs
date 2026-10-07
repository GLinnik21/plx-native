//! The four card shelves of [`super::DetailScreen`] (Related, the member collection, Extras and
//! Cast), as content for `plx_ui::cards::Shelf`: each shelf's cards read over the published
//! [`Detail`] and the page's element interning. The screen keeps what is not a card — the
//! headings, the vertical flow, the focus groups and the press actions.
use std::collections::HashMap;

use plx_data::metadata::Detail;
use plx_data::pms::PmsMovie;
use plx_machine::machine::Host;
use plx_ui::card_row::{RowStyle, TileLabel};
use plx_ui::cards::CardSource;
use plx_ui::widgets::Art;

use super::{collection, extras, related};
use crate::registry::tile_facts;

/// Which of the page's shelves a [`Cards`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Which {
    Related,
    Collection,
    Extras,
}

impl Which {
    /// The shelf's tile style, the one its group, its placement and its draw all read.
    pub(super) const fn style(self) -> &'static RowStyle {
        match self {
            Which::Related | Which::Collection => &RowStyle::HOME,
            Which::Extras => &RowStyle::EPISODE,
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
}

impl<'a> Cards<'a> {
    pub(super) fn new(
        which: Which,
        d: &'a Detail,
        key_by_local: &'a HashMap<u32, u32>,
        local_by_key: &'a HashMap<u32, u32>,
    ) -> Self {
        let mut cards = Self { which, d, key_by_local, local_by_key, len: 0 };
        let n = match which {
            Which::Related => d.related.len().min(512),
            Which::Collection => collection::len(d),
            Which::Extras => extras::len(d),
        };
        // The projections are rebuilt on every landing; a page whose items outran them shows none
        // rather than a card with no key.
        let published = n > 0 && cards.local(n - 1).is_some_and(|local| key_by_local.contains_key(&local));
        cards.len = if published { n } else { 0 };
        cards
    }

    fn local(&self, i: usize) -> Option<u32> {
        match self.which {
            Which::Related => related::elem(i),
            Which::Collection => collection::elem(i),
            Which::Extras => extras::elem(i),
        }
    }

    fn locate(&self, local: u32) -> Option<usize> {
        match self.which {
            Which::Related => related::locate(local),
            Which::Collection => collection::locate(local),
            Which::Extras => extras::locate(local),
        }
    }

    fn movies(&self) -> &'a [PmsMovie] {
        match self.which {
            Which::Related => &self.d.related,
            Which::Collection => collection::members(self.d),
            Which::Extras => &[],
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
        }
    }

    fn label(&self, i: usize) -> TileLabel {
        match self.which {
            Which::Related | Which::Collection => TileLabel::title(&self.movies()[i].title),
            Which::Extras => extras::label(self.d, i),
        }
    }

    fn progress(&self, i: usize) -> Option<f32> {
        match self.which {
            Which::Related | Which::Collection => self.movies().get(i).and_then(|m| m.resume_frac()),
            Which::Extras => None,
        }
    }
}
