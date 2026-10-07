//! Library paint consumes the same placement queries as keyboard and pointer navigation.
use super::*;
use crate::registry::tile_facts;
use plx_ui::card_row;
use plx_ui::cards::{CardSource, Tile};
use plx_machine::machine::{Host, Measure};
use plx_ui::screen::{Activate, Hover, Stop};
use plx_ui::theme;
use plx_ui::widgets::Art;
use plx_ui::value_chip::ValueChip;
use plx_ui::{Env, View, on_axis};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer { Grid, Document, Rail }

/// Paint and stop registration share one back-to-front order.
fn layers(mut visit: impl FnMut(Layer)) {
    for layer in [Layer::Grid, Layer::Document, Layer::Rail] { visit(layer); }
}

fn shelf_on_screen(origin: f32, pitch: f32) -> bool {
    on_axis(origin - plx_ui::consts::TITLE_DY, pitch, SCR_H, 0.0)
}

/// Reject a whole control band before creating labels or measuring individual pills. The
/// full-width envelope preserves travelling capsules; shared cast tokens cover their shadows.
/// It is deliberately wider than the controls, so no text metrics are needed to reject it.
fn document_band_visible(p: plx_ui::Painter, y: f32, height: f32, pop: f32) -> bool {
    let pad = theme::CONTROL_CAST_FOCUS.iter()
        .map(|(dy, blur, _)| dy.abs() + blur).fold(0.0f32, f32::max) + 1.0;
    let mut bounds = Rect::new(0.0, y, SCR_W, height)
        .scaled(pop.max(1.0) * p.scale().max(1.0)).inset(-pad);
    bounds.x += p.dx();
    bounds.y += p.dy();
    let visible = bounds.intersect(Rect::FULL);
    visible.w > 0.0 && visible.h > 0.0
        && (plx_ui::frame::backdrop::discovering()
            || !plx_gfx::gfx::culled(bounds.x, bounds.y, bounds.w, bounds.h))
}

#[cfg(test)]
mod layer_tests {
    #[test]
    fn document_band_culling_preserves_partial_labels_focus_and_shadow_edges() {
        let _guard = plx_base::testlock::serial();
        let p = plx_ui::Painter::root();
        let height = plx_ui::widgets::StatusOverlay::CTRL_H;
        let pad = plx_ui::theme::CONTROL_CAST_FOCUS.iter()
            .map(|(dy, blur, _)| dy.abs() + blur).fold(0.0f32, f32::max) + 1.0;
        assert!(super::document_band_visible(p, -height + 1.0, height, 1.0));
        assert!(super::document_band_visible(p, -height - pad + 1.0, height, 1.0),
            "a focus shadow can remain on the panel after the control itself left");
        let above = -height - pad - 1.0;
        assert!(!super::document_band_visible(p, above, height, 1.0));
        assert!(super::document_band_visible(p, above, height, plx_ui::widgets::CTRL_FOCUS_SCALE),
            "the focused capsule's growth belongs in the paint envelope");
        assert!(super::document_band_visible(p.translate(0.0, 300.0), above, height, 1.0));
        assert!(!super::document_band_visible(p, super::SCR_H + pad + 1.0, height, 1.0));
        assert!(!super::document_band_visible(p, -2000.0, super::layout::GRID_HEAD_H, 1.0));
    }

    #[test]
    fn document_band_culling_keeps_visible_controls_during_backdrop_discovery() {
        let _guard = plx_base::testlock::serial();
        let _discovery = plx_ui::frame::backdrop::discover(
            std::rc::Rc::new(std::cell::RefCell::new(Default::default())));
        assert!(plx_gfx::gfx::culled(0.0, 0.0, 100.0, 100.0),
            "discovery suppresses GL draws without suppressing paint declarations");
        assert!(super::document_band_visible(plx_ui::Painter::root(),
            super::CONTENT_TOP, super::layout::GRID_HEAD_H, 1.0));
    }

    #[test]
    fn shelf_culling_keeps_the_visible_band_above_the_centered_tab_track() {
        use plx_ui::consts::{CARD_H, ROW_PITCH, SCR_H, TITLE_DY};
        let y = plx_ui::widgets::TOP_BAR_BOTTOM - CARD_H - 8.0;
        assert!(y + CARD_H > 0.0);
        assert!(super::shelf_on_screen(y, ROW_PITCH));
        assert!(!super::shelf_on_screen(TITLE_DY - ROW_PITCH - 1.0, ROW_PITCH));
        assert!(!super::shelf_on_screen(SCR_H + TITLE_DY + 1.0, ROW_PITCH));
    }

    #[test]
    fn library_paint_and_stop_order_keeps_controls_above_grid_and_rail_above_document() {
        use super::{layers, Layer};
        let mut seen = Vec::new();
        layers(|layer| seen.push(layer));
        assert_eq!(seen, [Layer::Grid, Layer::Document, Layer::Rail]);
        for layer in [Layer::Grid, Layer::Document, Layer::Rail] {
            assert_eq!(seen.iter().filter(|&&found| found == layer).count(), 1);
        }
    }
}

impl LibraryScreen {
    pub(super) fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        // **The Library's own phase names**, in the `hm.*`/`st.*` family — `/tmp/plxnative-cpuprof`
        // times each on the render thread (inclusive wall, no `glFinish`, every phase at once) and
        // `--graphics-profile --profile-phase <name>` samples one. They exist because the owned
        // screen shipped with NONE: the 2026-09-09 `fps:library-switch` profile read `frame.ui`
        // 24.5 ms and `main.ui` 22.4 with nothing named beneath, so the whole page was one opaque
        // block and the only attribution left was the draw-MASK census, which can subtract a class
        // but cannot say which part of the screen spent it.
        //
        // `lb.ground` is the full-screen wash the mask census implicated (masking `ambient` moved
        // `loop=` 43-45 → 57); `lb.document` brackets the chips, the shelf band and the status
        // read-out with `lb.shelves` inside it for the card rows alone; `lb.grid` is the poster
        // wall's windowed rows and `lb.rail` the letter rail.
        plx_ui::profile::phase("lb.clear", || {
            plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        });
        plx_ui::profile::phase("lb.ground", || {
            self.ground.draw(f.painter.alpha(f.page_alpha), Rect::FULL);
        });
        let layout = self.pair.layout();
        let alpha = self.page_fade.alpha() * self.grid_fade.alpha();
        layers(|layer| match layer {
            Layer::Grid => plx_ui::profile::phase("lb.grid", || {
                draw_faded_part_at(&mut self.pair.detail, f, layout.detail, alpha)
            }),
            Layer::Document => plx_ui::profile::phase("lb.document", || self.draw_document(f)),
            Layer::Rail => plx_ui::profile::phase("lb.rail", || {
                // Decoration derives from the CURRENT engine key; no remembered cursor is copied.
                let index = f.focus.current.filter(|key| key.entry == self.entry)
                    .and_then(|key| self.pair.detail.index_of(key.elem));
                let parent = f.page_alpha;
                f.page_alpha = parent * alpha;
                self.pair.master.draw_with_current(f, layout.master, index);
                f.page_alpha = parent;
            }),
        });
        self.plaintext_alert.draw(f, self.entry);
    }

    /// The shelves whose band can meet the panel at the drawn scroll. Everything above and below
    /// is skipped without being looked at; `shelf_on_screen` stays the exact test for the edge
    /// shelves the window keeps. A shelf outside this window is outside that test too, so the
    /// focused row needs no separate visit.
    pub(super) fn shelf_window(&self) -> std::ops::Range<usize> {
        let base = CONTENT_TOP + self.layout.shelf_origin(&self.run, 0) - self.scroll.pos
            - plx_ui::consts::TITLE_DY;
        self.run.window(-base, SCR_H - base)
    }

    fn draw_document<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let env = Env::inert();
        self.draw_library_controls(f);
        plx_ui::profile::phase("lb.shelves", || {
            for index in self.shelf_window() {
                #[cfg(test)] self.shelf_visits.set(self.shelf_visits.get() + 1);
                let Some(row) = self.shelves.get(index) else { continue };
                let Some(shelf) = H::section_hubs(f.cx).shelves().get(index) else { continue };
                let origin = self.layout.shelf_y(&self.run, index, self.scroll.pos);
                if !shelf_on_screen(origin, self.layout.shelf_pitch(&self.run, index)) { continue; }
                let heading_y = origin - plx_ui::consts::TITLE_DY - row.cards.heading_lift();
                if let Some(heading) = self.heading_widget(index, f.cx) {
                    let focused = f.focus.current.is_some_and(|key| key.entry == self.entry
                        && Some(key.elem) == row.heading_elem());
                    heading.draw(p, MARGIN_X, heading_y, f32::from(focused), &heading.measure(f.measure), f.measure);
                } else {
                    card_row::draw_heading(p, &shelf.title, "", MARGIN_X,
                        heading_y, layout::GRID_RIGHT - MARGIN_X, f.measure);
                }
                row.cards.paint(f, self.shelf_painter(f), &self.hub_src(index, f.cx), self.shelf_frame(index));
            }
        });
        self.draw_grid_header(f);
        if self.readout == Readout::Loading {
            // Preserve the Library's standalone loading spinner, outside either content fade.
            plx_ui::placeholder::note(f.painter, plx_ui::placeholder::Reason::LibrarySpinner, "");
            plx_ui::widgets::Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
                .phase(f.cx.tick.ms).draw(&env, f.painter.alpha(f.page_alpha));
        } else if self.readout != Readout::Grid {
            let (text, reason) = self.status_text(f.cx);
            let status = self.status_overlay(f.cx, &text, reason.as_deref());
            let alpha = if self.readout == Readout::Empty { self.page_fade.alpha() * self.grid_fade.alpha() } else { 1.0 };
            status.draw_measured(&env, f.painter.alpha(f.page_alpha * alpha), f.cx.measure);
        }
        if !self.plaintext_alert.visible() {
            // the question owns the pointer while it is up; nothing under it is a target
            self.record_document_stops(f);
        }
    }

    fn draw_library_controls<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let pop = self.library_pop.scale_with(0, f.press.scale);
        let y = CONTENT_TOP - self.scroll.pos - self.shelves.first().map_or(0.0, |row| row.cards.heading_lift());
        if self.libraries.is_empty()
            || !document_band_visible(p, y, plx_ui::widgets::StatusOverlay::CTRL_H, pop) { return; }
        // Singleton selectors are hidden for every profile in `sync`; the remaining controls
        // are always the library pill strip — drawn through the one shared strip path.
        plx_ui::widgets::draw_strip(p, &self.library_capsules, &self.library_lays(f.cx), y,
            plx_ui::widgets::StatusOverlay::CTRL_H, 0.0,
            plx_ui::widgets::TabGround::Plated { pop });
    }

    fn draw_grid_header<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let y = CONTENT_TOP + self.layout.grid_block_top() - self.scroll.pos;
        if !self.layout.grid_head || !document_band_visible(p, y, layout::GRID_HEAD_H, 1.0) { return; }
        let env = Env::inert();
        for &elem in self.toolbar_elems() {
            let chip = self.toolbar_chip(elem, f.cx);
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                ValueChip::new(chip.name, &chip.value, chip.note.as_deref(), placed.rect)
                    .focused(f.focus.current.is_some_and(|key| key.elem == elem)).draw(&env, p);
            }
        }
        card_row::draw_heading(p, plx_platform::i18n::msg::browse_library_all(), "", MARGIN_X,
            y, layout::GRID_RIGHT - MARGIN_X, f.measure);
    }

    fn record_document_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        if !f.records_stops() { return; }
        for (index, (elem, _)) in self.libraries.iter().enumerate() {
            let rect = self.library_rect(index, f.cx);
            if on_axis(rect.y, rect.h, SCR_H, 0.0) { self.stop(*elem, f); }
        }
        for index in self.shelf_window() {
            #[cfg(test)] self.shelf_visits.set(self.shelf_visits.get() + 1);
            let Some(row) = self.shelves.get(index) else { continue };
            let origin = self.layout.shelf_y(&self.run, index, self.scroll.pos);
            if !shelf_on_screen(origin, self.layout.shelf_pitch(&self.run, index)) { continue; }
            let cx = f.cx;
            row.cards.record_stops(f, f.painter, &self.hub_src(index, cx), self.shelf_frame(index));
            // After the row's cards, so the heading wins where a popped card's glow overlaps it.
            if let (Some(elem), Some(heading), Some(rect)) = (row.heading_elem(),
                self.heading_widget(index, f.cx), self.heading_rect(index, f.cx, At::Drawn)) {
                heading.stop(f, rect, self.key(elem));
            }
        }
        if self.layout.grid_head { for &elem in self.toolbar_elems() { self.stop(elem, f); } }
        if self.readout == Readout::Failed { self.stop(RETRY, f); }
    }

    /// The production stop producers without rasterization, for the no-SDL dispatcher fixture.
    #[cfg(test)]
    pub fn record_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        layers(|layer| match layer {
            Layer::Grid => self.pair.detail.record_stops(f),
            Layer::Document => self.record_document_stops(f),
            Layer::Rail => self.pair.master.record_stops(f),
        });
    }

    fn stop<H: LibraryLike>(&self, elem: u32, f: &mut DrawFrame<'_, '_, H>) {
        let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { return };
        f.stop(f.painter, Stop {
            key: self.key(elem), rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
            hover: Hover::Focus, activate: Activate::Press,
        });
    }

    /// The painter the shelves' cards are drawn through: the page's alpha, both fades.
    fn shelf_painter<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) -> plx_ui::Painter {
        f.painter.alpha(f.page_alpha * self.page_fade.alpha())
    }

    pub fn redraw_focused<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        let Some(key) = focus.filter(|key| key.entry == self.entry) else { return };
        let Some(placed) = <Self as Focusable<H>>::place(self, &key.elem, f.cx, At::Drawn) else { return };
        let _clip = f.clip(f.painter, placed.clip);
        if self.pair.detail.index_of(key.elem).is_some() {
            let parent = f.page_alpha;
            f.page_alpha *= self.page_fade.alpha() * self.grid_fade.alpha();
            self.pair.detail.draw_focused(f, Some(key));
            f.page_alpha = parent;
            return;
        }
        if let Some(index) = self.shelves.iter().position(|row| row.elems.contains(&key.elem)) {
            let p = self.shelf_painter(f);
            let cx = f.cx;
            self.shelves[index].cards.redraw_focused(f, p, &self.hub_src(index, cx), self.shelf_frame(index), Some(key));
        }
    }
}

/// One hub shelf's cards for the shared section component: the published hub's items read through
/// the page's element ids. Built from borrows (no copy), so a call can hold it while the shelf's
/// own state is borrowed mutably.
pub(super) struct HubSrc<'a> {
    pub(super) elems: &'a [u32],
    /// `None` while the page still names a shelf the store has not published (a frame between a
    /// store change and the next `sync`): its cards draw nothing.
    pub(super) shelf: Option<&'a plx_data::browse::section_hubs::Shelf>,
}

impl HubSrc<'_> {
    fn item(&self, i: usize) -> Option<&plx_data::pms::PmsMovie> { self.shelf?.items.get(i) }
}

impl<H: Host<Elem = u32>> CardSource<H> for HubSrc<'_> {
    fn len(&self) -> usize { self.elems.len() }
    fn elem(&self, i: usize) -> u32 { self.elems.get(i).copied().unwrap_or(0) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.elems.iter().position(|elem| elem == e) }
    /// A card whose item the hub has not published draws nothing (its stop still registers).
    fn loaded(&self, i: usize) -> bool { self.item(i).is_some() }
    fn art(&self, i: usize) -> Art<'_> {
        match (self.item(i), self.shelf) {
            (Some(item), Some(shelf)) if shelf.landscape => Art::Still(Some(tile_facts::of(item))),
            (Some(item), _) => Art::Poster(Some(tile_facts::of(item))),
            _ => Art::Poster(None),
        }
    }
    fn label(&self, i: usize) -> card_row::TileLabel {
        self.shelf.map_or_else(|| card_row::TileLabel::title(""), |shelf| shelf_label(shelf, i))
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.shelf.filter(|shelf| !shelf.landscape).and_then(|_| self.item(i)).and_then(|item| item.resume_frac())
    }
    /// A landscape shelf's episode still carries its show line, and the Continue Watching deck its
    /// play mark when a press plays.
    fn overlay(&self, p: plx_ui::Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        let (Some(shelf), Some(item)) = (self.shelf, self.item(i)) else { return };
        if shelf.landscape {
            plx_ui::widgets::still_overlay(p, &tile_facts::of(item), tile.rect, tile.radius,
                shelf.is_continue && plx_media::route::deck_press().press_plays(), measure);
        }
    }
}

fn draw_faded_part_at<H: LibraryLike>(part: &mut impl Part<H>, f: &mut DrawFrame<'_, '_, H>, rect: Rect, alpha: f32) {
    let parent = f.page_alpha;
    f.page_alpha = parent * alpha;
    part.draw(f, rect);
    f.page_alpha = parent;
}

pub(super) fn shelf_label(shelf: &plx_data::browse::section_hubs::Shelf, col: usize) -> card_row::TileLabel {
    let Some(item) = shelf.items.get(col) else { return card_row::TileLabel::title("") };
    if shelf.landscape {
        let name = if item.title.is_empty() || item.title == item.show_title {
            plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        let fact = if shelf.is_continue && item.resume_frac().is_some() {
            plx_ui::fmt::time_left(item.dur_ns / 1_000_000 - item.resume_ms)
        } else if item.aired.is_empty() && item.year <= 0 { String::new() }
        else { plx_ui::fmt::pretty_date(&item.aired, item.year as i64) };
        return if fact.is_empty() { card_row::TileLabel::title(&name) }
        else { card_row::TileLabel::titled(&name, &fact) };
    }
    let mut label = if shelf.is_continue && plx_media::route::deck_press().press_plays() { card_row::TileLabel::played(&item.title) }
        else { card_row::TileLabel::title(&item.title) };
    label.caption = card_row::focused_caption(&tile_facts::of(item), shelf.is_continue);
    label
}
