//! [`AvatarRow`] — the profile picker's circular avatar row, the one public wrapper over the L0
//! `card_row` primitives. It is not a card section: the
//! roster has no captions, no hold menu and no paging, so it is `Focusable` geometry plus a
//! spring cache and the two tile bodies, kept here so the picker names no L0 primitive.

use plx_machine::machine::{EntryId, GroupId, Measure};

use crate::card_row::{self, CardRow, RowStyle, TileLabel};
use crate::geom::Shelf;
use crate::widgets::{Art, DipLatch};
use crate::{Painter, Rect};

/// The avatar row's animation cache (focus-scale and scroll springs) and its centring margin. A
/// render cache, not logical state: the engine owns WHICH avatar is focused.
pub struct AvatarRow {
    row: CardRow,
    sty: RowStyle,
    dip: DipLatch,
}

impl AvatarRow {
    /// The avatars' style: a circular tile, the same shelf motion the poster rows use.
    pub const STYLE: RowStyle = RowStyle::PROFILES;

    pub fn new() -> Self {
        Self { row: CardRow::new(), sty: Self::STYLE, dip: DipLatch::default() }
    }

    /// The first tile's left edge before scroll (the picker centres a short roster).
    pub fn set_margin_x(&mut self, x: f32) {
        self.sty = Self::STYLE;
        self.sty.margin_x = x;
    }

    /// Advance the springs one tick towards `focused` of `n` avatars.
    pub fn update(&mut self, n: usize, focused: Option<usize>, dt: f32) {
        self.row.update(n, focused, &self.sty, dt);
    }

    pub fn scroll_x(&self) -> f32 {
        self.row.scroll_x()
    }

    /// The row as the frame's `Focusable` view: the tile formula `draw` places by.
    pub fn focusable(&self, n: usize, row_y: f32, group: GroupId, entry: EntryId, extent: Rect) -> Shelf<'_> {
        Shelf {
            row: &self.row,
            n,
            sty: &self.sty,
            row_y,
            size: (self.sty.w, self.sty.h),
            pitch: self.sty.w + self.sty.gap,
            group,
            entry,
            extent,
        }
    }

    /// Paint avatar `i` whose settled rect is `base`; returns the drawn (scaled) rect, which is
    /// also the hit-map stop's. The click dip folds into the PRESSED avatar's pop (latched while the
    /// press is off rest, see [`DipLatch`]: the row has no keys to match `PressRead::owner`), so an
    /// abandoned press springs back on the avatar that was pressed. The focused one is the one the
    /// caller paints last.
    pub fn draw(&self, p: Painter, i: usize, base: Rect, art: Art, focused: bool, measure: &dyn Measure) -> Rect {
        let dip = self.dip.factor(i, focused.then_some(i), crate::press::scale());
        if focused {
            let sc = self.row.scale(i) * dip;
            let rect = base.scaled(sc);
            card_row::draw_focused(p, art, rect, sc, &self.sty, None, &TileLabel::default(), measure);
            rect
        } else {
            let sc = self.row.scale(i) * dip;
            let rect = base.scaled(sc);
            card_row::draw_tile(p, art, rect, sc, &self.sty, None);
            rect
        }
    }
}

impl Default for AvatarRow {
    fn default() -> Self {
        Self::new()
    }
}
