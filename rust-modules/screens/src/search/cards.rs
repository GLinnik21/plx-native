//! One result row's cards, for `plx_ui::cards::Shelf`: the row's items read over the page's
//! element interning and the roster of sources the results came from.
use super::*;
use crate::registry::tile_facts;
use plx_data::search::scope::ScopeSource;
use plx_machine::machine::Measure;
use plx_ui::cards::TileLabel;
use plx_ui::cards::{CardSource, Tile};
use plx_ui::widgets::Art;
use plx_ui::Painter;

/// Row `kind`'s cards. `items[i]` is shown as `elems[i]`; the two are built together by
/// [`SearchScreen::sync`], and a view that has since moved on shows as fewer cards, never as a
/// card with no item.
pub struct RowCards<'a> {
    pub(super) kind: Kind,
    pub(super) elems: &'a [u32],
    pub(super) items: &'a [Item],
    pub(super) sources: &'a [ScopeSource],
    /// Where these cards sit in the row's hits ([`plx_data::search::Window`]).
    pub(super) window: plx_data::search::Window,
    /// The row's committed slides ([`plx_data::search::Shelf::epoch`]).
    pub(super) epoch: u32,
}

impl RowCards<'_> {
    /// The server handle credited under a card of a source that is not the household's own.
    fn handle(&self, item: &Item) -> &str {
        self.sources.iter().find(|source| source.sid == item.sid() && !source.household)
            .map_or("", |source| source.handle.as_str())
    }
}

impl<H: SearchLike> CardSource<H> for RowCards<'_> {
    fn len(&self) -> usize { self.elems.len().min(self.items.len()) }
    fn elem(&self, i: usize) -> u32 { self.elems[i] }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.elems.iter().position(|elem| elem == e).filter(|&i| i < self.items.len())
    }
    fn art(&self, i: usize) -> Art<'_> {
        self.items.get(i).map_or(Art::Poster(None), |item| render::tile_art(self.kind, item))
    }
    fn label(&self, i: usize) -> TileLabel {
        match self.items.get(i) {
            Some(item) => TileLabel::titled(item.title(), &render::subtitle(self.kind, item, self.handle(item))),
            None => TileLabel::default(),
        }
    }
    fn progress(&self, i: usize) -> Option<f32> {
        match self.items.get(i) {
            Some(Item::Media(media)) if self.kind != Kind::Episode => media.resume_frac(),
            _ => None,
        }
    }
    fn more(&self) -> bool { self.window.after }
    fn page_epoch(&self) -> u32 { self.epoch }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        if let (Kind::Episode, Some(Item::Media(media))) = (self.kind, self.items.get(i)) {
            plx_ui::widgets::still_overlay(p, &tile_facts::of(media), tile.rect, tile.radius, false, measure);
        }
    }
}
