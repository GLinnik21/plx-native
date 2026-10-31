//! [`SampleRow`] — a still poster row for a page that illustrates one: a few tiles, one of them
//! lifted and captioned, drawn by the SAME tile and label painters the shelves and the Library grid
//! use. The Settings > Under posters page AND its Details picker are its callers, so what those
//! pages show beneath a poster is the card the shelves draw, not a screen-local imitation of it.
//!
//! The row stands in a [`SampleFrame`]: the panel (a faint fill and a hairline rim), the scissor
//! that clips the outer tiles to it, the dimming of every tile but the lifted one (a column: its
//! value line dims with it) and an optional caption at the panel's foot.
//!
//! It is geometry and paint only: no springs (a still picture has no motion), no focus (the engine's
//! focus is on the page's rows), no hit stops. The wrapper exists so a screen names no L0 tile
//! painter, as [`AvatarRow`](super::AvatarRow) does for the circular row.

use std::hash::{Hash, Hasher};

use plx_machine::machine::Measure;

use crate::card_row::{self, RowStyle, SortValue, TileLabel};
use crate::placeholder::{self, Reason};
use crate::screen::ClipScope;
use crate::tile::TileKind;
use crate::widgets::{Art, ArtImage};
use crate::{theme, Painter, Rect};

/// The panel's corner radius.
const FRAME_RAD: f32 = 24.0;
/// The panel's fill: black at 20%, over the page.
const FRAME_FILL_A: f32 = 0.20;
/// The panel's rim, inside its edge.
const FRAME_RIM_W: f32 = 1.0;
/// The air between the panel's bottom edge and its foot caption's line box.
const FOOT_BOTTOM: f32 = 32.0;
/// The foot caption's line box: the MICRO rung and its leading.
const FOOT_LINE_H: f32 = 28.0;

/// **The panel a [`SampleRow`] stands in.** `rect` is the panel (the row's tiles are clipped to it,
/// so a row wider than the panel shows its outer tiles cut by the edges), `row` is where the row
/// of tiles stands (see [`SampleRow::tile`]), `rest_alpha` is how much of a tile that is not the
/// lifted one shows, and `foot` is an optional caption centred at the panel's foot, in the
/// tertiary ink at the MICRO rung.
#[derive(Clone, Copy)]
pub struct SampleFrame<'a> {
    pub rect: Rect,
    pub row: Rect,
    pub rest_alpha: f32,
    pub foot: Option<&'a str>,
}

impl SampleFrame<'_> {
    /// The panel under the row: a faint fill and a hairline rim, painted before any tile.
    fn panel(&self, p: Painter) {
        p.rect(self.rect, FRAME_RAD, theme::scrim_black(FRAME_FILL_A), theme::scrim_black(FRAME_FILL_A), 0.0);
        p.rring(self.rect, FRAME_RAD, FRAME_RIM_W, theme::HAIRLINE);
    }

    /// The caption at the panel's foot, centred, never cut: it is judged against the panel less its sides.
    fn draw_foot(&self, p: Painter, measure: &dyn Measure) {
        let Some(text) = self.foot else { return };
        let size = theme::size::MICRO;
        let (run, _) = measure.fit_line_measured(text, self.rect.w - 2.0 * theme::space::MD, size, false);
        let cy = self.rect.y + self.rect.h - FOOT_BOTTOM - FOOT_LINE_H * 0.5;
        p.text(run.as_ptr(), self.rect.cx(), plx_gfx::text::text_vcenter_y(size, 0, cy), size, theme::TEXT_TERTIARY, 1, 0);
    }
}

/// A still row of tiles drawn with the shelves' own painters.
///
/// **The picture is painted once.** The panel, its rim and the tiles (the lifted one with its focus
/// shadow and sheen) are a PICTURE that changes only when its inputs do ([`SampleRow::cache_key`]:
/// the geometry, and each tile's art and state marks), and it is the part that costs the GPU - a
/// translucent panel and a lifted tile through the focus program - and the part both Under posters
/// pages draw at once while one slides over the other. It is drawn into a texture once per key and
/// put on the frame as one quad ([`Painter::cached_region`]). The TEXT stays live over it: the label
/// under the lifted tile, the sort values and the foot caption, which differ from page to page
/// (the picker's hovered caption is not the stored one, its foot is not the page's) and which a
/// quad would only blur.
pub struct SampleRow;

impl SampleRow {
    /// The settled rect of tile `i` of `n` (tiles of `sty`'s size, `sty.gap` apart): the row is
    /// centred in `area` and rests on its top edge.
    pub fn tile(sty: &RowStyle, n: usize, i: usize, area: Rect) -> Rect {
        let width = n as f32 * sty.w + n.saturating_sub(1) as f32 * sty.gap;
        let x = area.x + (area.w - width) * 0.5 + i as f32 * (sty.w + sty.gap);
        Rect::new(x, area.y, sty.w, sty.h)
    }

    /// The rect tile `i` is drawn at when it is the lifted one: its settled rect at the style's
    /// focus scale.
    pub fn lifted(sty: &RowStyle, n: usize, i: usize, area: Rect) -> Rect {
        Self::tile(sty, n, i, area).scaled(sty.focus_scale)
    }

    /// Paint `tiles` as a row in `frame`. Tile `focused` is lifted, drawn last (its z-order is the
    /// shelves') and wears `label`; every other tile is at rest, dimmed to `frame.rest_alpha`, and
    /// wears none.
    pub fn draw<'a>(
        p: Painter,
        frame: &SampleFrame<'_>,
        sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>,
        focused: usize,
        label: &TileLabel,
        measure: &dyn Measure,
    ) {
        let arts: Vec<Art<'a>> = tiles.collect();
        let n = arts.len();
        Self::framed(p, frame, sty, &arts, focused, measure, |p| {
            // Over the picture, as `draw_focused` ends: the label hangs under the lifted tile.
            if focused < n {
                card_row::draw_label(p, Self::lifted(sty, n, focused, frame.row), sty.focus_scale, sty, label, measure);
            }
        });
    }

    /// Paint `tiles` as a library grid shows them while values are on: every tile wears its sort
    /// value line (`value(i)`) at rest, and the lifted one wears it bold, through the same painter
    /// a real grid draws with ([`card_row::draw_sort_value`]). A tile at rest is a dimmed column:
    /// its value line dims with it.
    pub fn draw_values<'a>(p: Painter, frame: &SampleFrame<'_>, sty: &RowStyle,
        tiles: impl ExactSizeIterator<Item = Art<'a>>, focused: usize,
        mut value: impl FnMut(usize) -> SortValue, measure: &dyn Measure) {
        let arts: Vec<Art<'a>> = tiles.collect();
        let n = arts.len();
        Self::framed(p, frame, sty, &arts, focused, measure, |p| {
            // Over every tile, as the grid draws them: the lifted card's shadow reaches the value line.
            for i in 0..n {
                let (rect, s, layer) = if i == focused {
                    (Self::lifted(sty, n, i, frame.row), sty.focus_scale, p)
                } else {
                    (Self::tile(sty, n, i, frame.row), 1.0, p.alpha(frame.rest_alpha))
                };
                card_row::draw_sort_value(layer, rect, s, sty, &value(i), measure, i == focused);
            }
        });
    }

    /// The frame's panel and tiles as one cached picture ([`Painter::cached_region`]), then `text`
    /// clipped to the frame, then its foot caption. The art of every tile is asked for ONCE, up front,
    /// in the order the tiles are painted (the resting ones, the lifted one last): the key needs the
    /// answer before anything is drawn, and the draw is handed it.
    fn framed(p: Painter, frame: &SampleFrame<'_>, sty: &RowStyle, arts: &[Art<'_>], focused: usize,
        measure: &dyn Measure, text: impl FnOnce(Painter)) {
        let led = Self::lead(p, sty, frame, arts, focused);
        let key = Self::cache_key(frame, sty, arts, &led, focused);
        let painted = p.cached_region(key, frame.rect, |q| {
            frame.panel(q);
            let _clip = ClipScope::open_in(q, frame.rect);
            Self::tiles(q, frame, sty, arts, &led, focused);
        });
        if !painted {
            // The picture came from the texture, so the skeletons in it were not drawn: count them as
            // the draw would (`widgets::card_named_led`), or the frame's placeholder debt reads zero.
            for (art, image) in arts.iter().zip(&led) {
                if let Art::Poster(m) = art {
                    let neutral = m.is_some_and(|m| m.kind == TileKind::Collection && m.thumb.is_empty());
                    if image.tex == 0 && !neutral {
                        placeholder::note(p, Reason::CardSkeleton, m.map_or("", |m| m.thumb));
                    }
                }
            }
        }
        {
            let _clip = ClipScope::open_in(p, frame.rect);
            text(p);
        }
        frame.draw_foot(p, measure);
    }

    /// Ask for every tile's art, in paint order (the resting tiles, then the lifted one), each once.
    fn lead(p: Painter, sty: &RowStyle, frame: &SampleFrame<'_>, arts: &[Art<'_>], focused: usize) -> Vec<ArtImage> {
        let n = arts.len();
        let mut led = vec![ArtImage::NONE; n];
        for (i, art) in arts.iter().enumerate().filter(|(i, _)| *i != focused) {
            led[i] = card_row::lead_focused(p, art, Self::tile(sty, n, i, frame.row), 1.0);
        }
        if focused < n {
            led[focused] = card_row::lead_focused(p, &arts[focused], Self::lifted(sty, n, focused, frame.row), sty.focus_scale);
        }
        led
    }

    /// The tiles of a row without their label, the lifted one last, the others dimmed.
    fn tiles(p: Painter, frame: &SampleFrame<'_>, sty: &RowStyle, arts: &[Art<'_>], led: &[ArtImage], focused: usize) {
        let n = arts.len();
        let rest = p.alpha(frame.rest_alpha);
        for (i, art) in arts.iter().enumerate().filter(|(i, _)| *i != focused) {
            card_row::draw_tile_led(rest, *art, Self::tile(sty, n, i, frame.row), 1.0, sty, None, Some(led[i]));
        }
        if focused < n {
            card_row::draw_focused_tile_led(p, arts[focused], Self::lifted(sty, n, focused, frame.row), sty.focus_scale, sty, None, Some(led[focused]));
        }
    }

    /// **The name of the picture [`tiles`](Self::tiles) and the panel draw**: every input of it, and
    /// nothing of the text over it. `None` while the picture is not settled - a tile's art is still
    /// dissolving in over its skeleton ([`ArtImage::shown`] below 1) - so it is drawn live until it
    /// is, rather than frozen mid-fade into the texture.
    ///
    /// The inputs: the panel's rect (its rim and fill are constants), the row's rect, the style's
    /// size, gap, radius and focus scale, which tile is lifted, how much of the rest shows, and per
    /// tile the texture it resolved to and its decoded size (the crop) and what else the card draws
    /// from its facts: the watched disc, the resume bar, the collection's name and fan.
    fn cache_key(frame: &SampleFrame<'_>, sty: &RowStyle, arts: &[Art<'_>], led: &[ArtImage], focused: usize) -> Option<u64> {
        if led.iter().any(|image| image.shown < 1.0) {
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in [frame.rect, frame.row] {
            [r.x, r.y, r.w, r.h].map(f32::to_bits).hash(&mut h);
        }
        [frame.rest_alpha, sty.w, sty.h, sty.gap, sty.radius, sty.focus_scale].map(f32::to_bits).hash(&mut h);
        (sty.circular, focused, arts.len()).hash(&mut h);
        for (art, image) in arts.iter().zip(led) {
            (image.tex, image.w.to_bits(), image.h.to_bits()).hash(&mut h);
            Self::hash_art(art, &mut h);
        }
        Some(h.finish())
    }

    /// What of `art` the card draws besides its texture.
    fn hash_art(art: &Art<'_>, h: &mut impl Hasher) {
        std::mem::discriminant(art).hash(h);
        match art {
            Art::Poster(m) | Art::Still(m) => m
                .map(|m| {
                    (
                        (m.src, m.thumb, m.still, m.art, m.kind as u8, m.title),
                        (m.watched, m.unwatched, m.composite_thumb, m.child_count, m.season_index, m.ep_index),
                        m.resume.map(|r| r.frac.to_bits()),
                    )
                })
                .hash(h),
            Art::Thumb { sid, key, res } | Art::Person { sid, key, res } => (*sid, *key, res.0, res.1).hash(h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw_census::capture_runs;
    use crate::fixture::FixtureMeasure;

    #[test]
    fn the_row_is_centred_and_the_lifted_tile_grows_about_its_own_centre() {
        let sty = RowStyle { w: 100.0, h: 150.0, gap: 20.0, ..RowStyle::HOME };
        let area = Rect::new(10.0, 40.0, 500.0, 300.0);
        let tiles: Vec<Rect> = (0..3).map(|i| SampleRow::tile(&sty, 3, i, area)).collect();
        let left = tiles[0].x - area.x;
        let right = area.x + area.w - (tiles[2].x + tiles[2].w);
        assert!((left - right).abs() < 1e-3, "centred: {left} vs {right}");
        assert!((tiles[1].x - (tiles[0].x + 120.0)).abs() < 1e-3, "one gap between tiles");
        assert!(tiles.iter().all(|t| t.y == area.y && t.h == 150.0));
        let up = SampleRow::lifted(&sty, 3, 1, area);
        let (c0, c1) = ((tiles[1].x + tiles[1].w * 0.5), (up.x + up.w * 0.5));
        assert!((c0 - c1).abs() < 1e-3 && up.w > tiles[1].w);
    }

    /// The board's frame: Library cards 35 px apart in a 660 px panel stand at -80 / 205 / 490, the
    /// outer two cut by the panel's edges and only the middle one wholly inside.
    #[test]
    fn a_row_wider_than_its_frame_stands_at_the_boards_offsets_with_the_outer_tiles_cut() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let frame = Rect::new(96.0, 470.0, 660.0, 556.0);
        let row = Rect::new(frame.x, frame.y + 40.0, frame.w, sty.h);
        let xs: Vec<f32> = (0..3).map(|i| SampleRow::tile(&sty, 3, i, row).x - frame.x).collect();
        for (got, want) in xs.iter().zip([-80.0, 205.0, 490.0]) {
            assert!((got - want).abs() < 1e-3, "{xs:?}");
        }
        assert_eq!((sty.w, sty.h), (250.0, 375.0), "the Library's own card");
        let inside = |r: Rect| r.x >= frame.x && r.x + r.w <= frame.x + frame.w;
        assert!(!inside(SampleRow::tile(&sty, 3, 0, row)) && !inside(SampleRow::tile(&sty, 3, 2, row)));
        assert!(inside(SampleRow::lifted(&sty, 3, 1, row)), "the lifted tile is whole");
    }

    #[test]
    fn the_foot_caption_is_centred_micro_tertiary_32_px_above_the_frames_bottom_and_absent_without_one() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let rect = Rect::new(96.0, 470.0, 660.0, 556.0);
        let frame = |foot| SampleFrame { rect, row: Rect::new(rect.x, rect.y + 40.0, rect.w, sty.h), rest_alpha: 0.35, foot };
        let draw = |foot| capture_runs(|| {
            SampleRow::draw(Painter::recording(), &frame(foot), &sty, (0..3).map(|_| Art::Poster(None)), 1, &TileLabel::default(),
                &FixtureMeasure);
        });
        let runs = draw(Some("Sorted by Year"));
        let foot = runs.iter().find(|r| r.text == "Sorted by Year").expect("the foot caption is drawn");
        assert_eq!((foot.size, foot.bold, foot.ink), (theme::size::MICRO, false, theme::TEXT_TERTIARY));
        assert!((foot.at.x - rect.cx()).abs() < 1e-3 && foot.align == 1, "centred on the frame: {:?}", foot.at);
        let bottom = rect.y + rect.h;
        assert!(foot.at.y < bottom - FOOT_BOTTOM && foot.at.y > bottom - FOOT_BOTTOM - FOOT_LINE_H - 8.0, "its line box ends 32 px above the foot: {:?}", foot.at);
        assert!(draw(None).iter().all(|r| r.text != "Sorted by Year"), "no foot, no caption");
    }

    #[test]
    fn a_tile_at_rest_is_dimmed_with_its_value_line_and_the_lifted_column_is_not() {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let rect = Rect::new(96.0, 470.0, 660.0, 556.0);
        let frame = SampleFrame { rect, row: Rect::new(rect.x, rect.y + 40.0, rect.w, sty.h), rest_alpha: 0.35, foot: None };
        let value = |i: usize| SortValue { text: ["2026", "2025", "2024"][i].to_string(), ..Default::default() };
        let runs = capture_runs(|| {
            SampleRow::draw_values(Painter::recording(), &frame, &sty, (0..3).map(|_| Art::Poster(None)), 1, value, &FixtureMeasure);
        });
        let ink = |text: &str| runs.iter().filter(|r| r.text == text).map(|r| r.ink[3]).fold(0.0f32, f32::max);
        assert!(ink("2026") <= 0.35 + 1e-3 && ink("2026") > 0.0, "the left value dims with its tile: {}", ink("2026"));
        assert!(ink("2024") <= 0.35 + 1e-3 && ink("2024") > 0.0, "and the right: {}", ink("2024"));
        assert!(ink("2025") > 0.9, "the lifted column's value is whole: {}", ink("2025"));
    }

    // ---- the cached picture: what names it ----

    use crate::tile::TileFacts;
    use plx_gfx::gfx::{KeyedSlots, RegionPlan};

    fn board() -> (SampleFrame<'static>, RowStyle) {
        let sty = RowStyle { gap: 35.0, ..RowStyle::HOME };
        let rect = Rect::new(96.0, 470.0, 660.0, 556.0);
        (SampleFrame { rect, row: Rect::new(rect.x, rect.y + 40.0, rect.w, sty.h), rest_alpha: 0.35, foot: None }, sty)
    }

    fn facts(thumb: &str) -> TileFacts<'_> {
        TileFacts { thumb, src: 3, ..Default::default() }
    }

    fn image(tex: u32) -> ArtImage {
        ArtImage { tex, w: 250.0, h: 375.0, shown: 1.0 }
    }

    fn key_of(frame: &SampleFrame<'_>, sty: &RowStyle, arts: &[Art<'_>], led: &[ArtImage], focused: usize) -> Option<u64> {
        SampleRow::cache_key(frame, sty, arts, led, focused)
    }

    /// The same inputs are the same name, however often they are asked; every input the picture is
    /// drawn from is part of it. (The label, the values and the foot are not: they are drawn live over it.)
    #[test]
    fn each_input_of_the_picture_changes_its_key_and_the_text_over_it_does_not() {
        let (frame, sty) = board();
        let arts = [Art::Poster(Some(facts("/a"))), Art::Poster(Some(facts("/b"))), Art::Poster(Some(facts("/c")))];
        let led = [image(5), image(6), image(7)];
        let base = key_of(&frame, &sty, &arts, &led, 1).expect("a settled picture has a name");
        assert_eq!(key_of(&frame, &sty, &arts, &led, 1), Some(base), "the same inputs are the same name");

        // geometry and style
        let moved = SampleFrame { rect: Rect::new(97.0, 470.0, 660.0, 556.0), ..frame };
        let dimmer = SampleFrame { rest_alpha: 0.30, ..frame };
        let wider = RowStyle { gap: 36.0, ..sty };
        let bigger = RowStyle { focus_scale: sty.focus_scale + 0.01, ..sty };
        assert_ne!(key_of(&moved, &sty, &arts, &led, 1), Some(base), "the panel's place");
        assert_ne!(key_of(&dimmer, &sty, &arts, &led, 1), Some(base), "how much of a resting tile shows");
        assert_ne!(key_of(&frame, &wider, &arts, &led, 1), Some(base), "the gap");
        assert_ne!(key_of(&frame, &bigger, &arts, &led, 1), Some(base), "the lift");
        assert_ne!(key_of(&frame, &sty, &arts, &led, 0), Some(base), "which tile is lifted");
        assert_ne!(key_of(&frame, &sty, &arts[..2], &led[..2], 1), Some(base), "how many tiles");

        // art: arrival (the texture), its decoded size, which poster, and the marks on it
        let arrived = [image(5), image(6), image(8)];
        let recut = [image(5), ArtImage { w: 300.0, ..image(6) }, image(7)];
        assert_ne!(key_of(&frame, &sty, &arts, &arrived, 1), Some(base), "a poster arriving");
        assert_ne!(key_of(&frame, &sty, &arts, &recut, 1), Some(base), "its crop");
        let other = [arts[0], Art::Poster(Some(facts("/b2"))), arts[2]];
        assert_ne!(key_of(&frame, &sty, &other, &led, 1), Some(base), "another poster on the same texture");
        let seen = [arts[0], Art::Poster(Some(TileFacts { watched: true, ..facts("/b") })), arts[2]];
        let half = [arts[0], Art::Poster(Some(TileFacts { resume: Some(crate::tile::Resume { frac: 0.4, left_ms: 1 }), ..facts("/b") })), arts[2]];
        assert_ne!(key_of(&frame, &sty, &seen, &led, 1), Some(base), "the watched disc");
        assert_ne!(key_of(&frame, &sty, &half, &led, 1), Some(base), "the resume bar");

        // the text over it is not part of it
        let footed = SampleFrame { foot: Some("The library you last opened"), ..frame };
        assert_eq!(key_of(&footed, &sty, &arts, &led, 1), Some(base), "the foot caption is drawn live");
    }

    /// Art that has arrived and is still dissolving in over its skeleton is not a settled picture:
    /// it has no name, so it is drawn live until it is whole.
    #[test]
    fn a_picture_with_art_mid_dissolve_has_no_name() {
        let (frame, sty) = board();
        let arts = [Art::Poster(Some(facts("/a"))), Art::Poster(Some(facts("/b"))), Art::Poster(Some(facts("/c")))];
        let mid = [image(5), ArtImage { shown: 0.4, ..image(6) }, image(7)];
        assert_eq!(key_of(&frame, &sty, &arts, &mid, 1), None);
        let done = [image(5), ArtImage { shown: 1.0, ..image(6) }, image(7)];
        assert!(key_of(&frame, &sty, &arts, &done, 1).is_some());
    }

    /// The draw-count pin, end to end on the host: the names of a scripted run of frames go through
    /// the slot table `region_paint` serves from. Twenty frames of the same inputs are ONE render;
    /// each changed input is one more; a frame mid-dissolve has no name and renders nothing (it is
    /// drawn live); and the two pages of a push - same picture, so the same name - share the render.
    #[test]
    fn the_same_inputs_render_once_each_change_renders_again_and_a_dissolve_draws_live() {
        let (frame, sty) = board();
        let arts = [Art::Poster(Some(facts("/a"))), Art::Poster(Some(facts("/b"))), Art::Poster(Some(facts("/c")))];
        let mut slots = KeyedSlots::<2>::new();
        let (renders_c, live_c) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let mut frame_of = |led: &[ArtImage], frame: &SampleFrame<'_>, pages: usize| {
            for _ in 0..pages {
                match key_of(frame, &sty, &arts, led, 1) {
                    None => live_c.set(live_c.get() + 1),
                    Some(k) => {
                        if let RegionPlan::Render(i) = slots.plan(k) {
                            renders_c.set(renders_c.get() + 1);
                            slots.filled(i, k);
                        }
                    }
                }
            }
        };
        let renders = || renders_c.get();
        let live = || live_c.get();
        let resting = [image(5), image(6), image(7)];
        for _ in 0..20 {
            frame_of(&resting, &frame, 1);
        }
        assert_eq!(renders(), 1, "twenty frames, one render");
        for _ in 0..12 {
            frame_of(&resting, &frame, 2); // a push: both pages, one picture
        }
        assert_eq!(renders(), 1, "both pages of a push are served by the one render");
        let mid = [image(5), ArtImage { shown: 0.5, ..image(6) }, image(7)];
        frame_of(&mid, &frame, 1);
        assert_eq!((renders(), live()), (1, 1), "a frame mid-dissolve is drawn live, and fills nothing");
        let arrived = [image(5), image(6), image(8)];
        frame_of(&arrived, &frame, 1);
        assert_eq!(renders(), 2, "the settled picture is rendered once it is whole");
        frame_of(&arrived, &SampleFrame { rest_alpha: 0.5, ..frame }, 1);
        assert_eq!(renders(), 3, "a changed input renders again");
        frame_of(&arrived, &frame, 1);
        assert_eq!(renders(), 3, "and the one before it is still held (two slots)");
    }

    /// On the host no GL is linked: the picture is drawn live, once, with the page's own painter and
    /// the caller is told it ran (so it books what a draw books).
    #[test]
    fn without_a_gl_context_the_picture_is_drawn_live_exactly_once() {
        let mut runs = 0;
        let ran = Painter::root().cached_region(Some(1), Rect::new(0.0, 0.0, 10.0, 10.0), |_| runs += 1);
        assert!(ran && runs == 1);
        let ran = Painter::root().cached_region(None, Rect::new(0.0, 0.0, 10.0, 10.0), |_| runs += 1);
        assert!(ran && runs == 2, "an unnamed picture is always live");
    }
}
