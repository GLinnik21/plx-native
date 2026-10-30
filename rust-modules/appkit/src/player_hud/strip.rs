//! **The chapters strip of a compiled HUD, drawn by `cards::Shelf`** (`docs/hud-plugins.md` D11). The
//! compiler places the strip ([`DrawOp::Strip`]: the viewport, the card, the pitch, the radius and where
//! its name and time stand under a card); this module turns that into the Shelf's inputs and the Shelf
//! paints it, so the pop, the cull, the minimal scroll and the focused name's marquee are the Shelf's and
//! not the strip's own.
//!
//! **What the Shelf draws, and what the source adds.** The Shelf draws every card, the focused one popped
//! and ringed ([`RowStyle::rim`]), and the focused card's NAME as its label block, hung where the strip's
//! names stand ([`RowStyle::label_drop`]) from the card's leading edge ([`RowStyle::label_start`]), so a name
//! wider than the card glides instead of eliding. [`Cards::overlay`] draws the rest on EVERY card: the time
//! (all of them) and the name of an unfocused card (elided, still), in the strip's inks.
//!
//! **The seat and the scroll are the Shelf's.** The screen feeds the Shelf its events
//! ([`super::motion::HudMotion::strip`]); a draw only paints. Which chapter is focused is the engine's
//! focus (`Cx::focus` naming an element of this entry), so a screen that registers the strip's group is the
//! one that moves it (PR8 stage 2).
use std::ffi::CString;
use std::marker::PhantomData;

use plx_machine::machine::{Host, Measure};
use plx_ui::cards::{CardSource, Rim, RowStyle, Tile, TileLabel};
use plx_ui::geom::IndexElem;
use plx_ui::theme;
use plx_ui::widgets::{Art, CARD_FOCUS_SCALE};
use plx_ui::{Painter, Rect};

use crate::chapters_panel::CH_FOCUS_RING_W;
use crate::hud_format::compile::{CompiledState, DrawOp};
use super::model::HudModel;
use super::state::ELEM_CHAPTER_BASE;
use plx_data::metadata::Chapter;

/// The art resolution a chapter still is asked for at: the episode picker's, and the strip's since it drew.
const STILL_RES: (std::os::raw::c_int, std::os::raw::c_int) = (480, 270);

/// What a strip op says about the strip: the viewport, a card at slot 0, the pitch, the corner and where the
/// name and the time stand below a card (from the card's bottom edge to the text's top).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub rect: Rect,
    pub card: Rect,
    pub pitch: f32,
    pub radius: f32,
    pub name_drop: f32,
    pub time_drop: f32,
}

impl Geometry {
    /// The strip an op describes: its two labels' drops are read from the card template's `item.title` and
    /// `item.start` texts (a package without them draws no label, so its drops are 0).
    pub fn of(op: &DrawOp) -> Option<Geometry> {
        let DrawOp::Strip { rect, card, pitch, radius, template, .. } = op else { return None };
        let drop_of = |bind: &str| {
            template.iter().find_map(|c| match &c.op {
                DrawOp::Text { bind: b, rect: r, .. } if b == bind => Some(r.y - (card.y + card.h)),
                _ => None,
            })
        };
        Some(Geometry {
            rect: *rect,
            card: *card,
            pitch: *pitch,
            radius: *radius,
            name_drop: drop_of("item.title").unwrap_or(0.0),
            time_drop: drop_of("item.start").unwrap_or(0.0),
        })
    }

    /// The Shelf's style for this strip: the card's size, the gap and corner, the focus pop the strip has
    /// always had, the rim over the video plane, labels from the card's leading edge and the name hung where
    /// the op stands it.
    pub fn style(&self) -> RowStyle {
        RowStyle {
            w: self.card.w,
            h: self.card.h,
            gap: self.pitch - self.card.w,
            margin_x: self.card.x,
            radius: self.radius,
            focus_scale: CARD_FOCUS_SCALE,
            ..RowStyle::HOME
        }
        .with_rim(Rim { width: CH_FOCUS_RING_W, ink: theme::CONTROL_RIM_FOCUS_UNKEYED })
        .with_leading_labels()
        .with_label_drop(self.name_drop)
    }
}

/// **The strip a state places**: its strip op's [`Geometry`], `None` for a state with no strip. The ONE read of
/// the op the renderer (which paints with it) and the screen (which steps the Shelf, places its stops and
/// travels by it) share, so they cannot describe two strips.
pub fn strip_of(state: &CompiledState) -> Option<Geometry> {
    state.ops.iter().find_map(Geometry::of)
}

/// **The cards a strip shows**: the playing item's chapters at `geo`, `None` for an item with none. Built the one
/// way the draw and the screen both build it.
pub fn cards_for<'a, E>(
    geo: Geometry,
    meta: plx_data::metadata::MetadataView<'a>,
    ps: &plx_media::route::PlaybackSession,
) -> Option<Cards<'a, E>> {
    let chapters = crate::chapters_panel::chapters(meta);
    if chapters.is_empty() {
        return None;
    }
    Some(Cards::new(chapters, plx_media::route::item_sid(plx_media::route::cur_sid(ps)).raw(), geo))
}

/// **The chapter that holds the position the HUD shows** (D6: opening Chapters seats focus there): the last
/// chapter that has started at the model's displayed playhead (the scrub preview, a loading seek's target, else
/// the playhead), the first when none has. From the frame's `HudModel`, not the global, so what the seat reads
/// is what the playbar draws.
pub fn chapter_at_shown(chapters: &[Chapter], model: &HudModel) -> usize {
    let shown = super::state::display_pos_ns(model.loading, model.seek_display_ns, model.scrub_ns, model.playpos_ns);
    let ms = shown / 1_000_000;
    chapters.iter().rposition(|c| c.start_ms <= ms).unwrap_or(0)
}

/// The playing item's chapters as the Shelf's cards. Generic over the host's element, which only has to be
/// an index (a chapter is its position).
pub struct Cards<'a, E> {
    chapters: &'a [Chapter],
    /// the server the stills live on
    sid: u16,
    geo: Geometry,
    elem: PhantomData<fn() -> E>,
}

impl<'a, E> Cards<'a, E> {
    pub fn new(chapters: &'a [Chapter], sid: u16, geo: Geometry) -> Self {
        Cards { chapters, sid, geo, elem: PhantomData }
    }

    /// The position chapter `i` starts at, in ns (what a press on its card seeks to).
    pub fn start_ns(&self, i: usize) -> Option<i64> {
        self.chapters.get(i).map(|c| c.start_ms.saturating_mul(1_000_000))
    }

    /// The card focus lands on when the strip opens: the chapter that holds the position the HUD shows
    /// ([`chapter_at_shown`]).
    pub fn seat_at(&self, model: &HudModel) -> usize {
        chapter_at_shown(self.chapters, model)
    }

    /// The name a chapter shows: its title, or "Chapter N" when the server sent none.
    fn name(&self, i: usize) -> String {
        let ch = &self.chapters[i];
        if ch.title.trim().is_empty() {
            plx_platform::i18n::msg::widgets_chapters_number(ch.index)
        } else {
            ch.title.clone()
        }
    }
}

impl<H: Host> CardSource<H> for Cards<'_, H::Elem>
where
    H::Elem: IndexElem,
{
    fn len(&self) -> usize {
        self.chapters.len()
    }
    fn elem(&self, i: usize) -> H::Elem {
        H::Elem::of_index(ELEM_CHAPTER_BASE + i as u32)
    }
    fn index_of(&self, e: &H::Elem) -> Option<usize> {
        e.index().and_then(|i| i.checked_sub(ELEM_CHAPTER_BASE)).map(|i| i as usize).filter(|&i| i < self.chapters.len())
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Thumb { sid: self.sid, key: &self.chapters[i].thumb, res: STILL_RES }
    }
    /// The focused card's name: the Shelf hangs it under the card and glides it when it does not fit.
    fn label(&self, i: usize) -> TileLabel {
        TileLabel { title: CString::new(self.name(i)).ok(), ..TileLabel::default() }
    }
    fn holdable(&self, _i: usize) -> bool {
        false
    }
    /// The time on every card and the name on every card but the focused one (whose name is the block's), at
    /// the card's LEADING edge and the drops the op stands them at: unscaled, so a pop never moves them.
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        let card = tile.rect.scaled(1.0 / tile.scale);
        let (x, bottom) = (card.x, card.y + card.h);
        if !tile.focused {
            let shown = plx_gfx::text::elide_by(&self.name(i), self.geo.card.w, false, |t| {
                measure.width_str(t, theme::size::LABEL, true)
            });
            if let Ok(name) = CString::new(shown) {
                p.text(name.as_ptr(), x, bottom + self.geo.name_drop, theme::size::LABEL, theme::TEXT_SECONDARY, 0, 1);
            }
        }
        if let Ok(time) = CString::new(plx_ui::fmt::clock(self.chapters[i].start_ms)) {
            p.text(time.as_ptr(), x, bottom + self.geo.time_drop, theme::size::CAPTION, theme::TEXT_SECONDARY, 0, 0);
        }
    }
}
