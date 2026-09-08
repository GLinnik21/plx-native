//! Library paint consumes the same placement queries as keyboard and pointer navigation.
use super::*;
use crate::ui::card_row;
use crate::ui::screen::{Activate, Hover, Stop};
use crate::ui::theme;
use crate::ui::widgets::{Art, TabPill, StatusOverlay, StatusKind};
use crate::ui::value_chip::ValueChip;
use crate::ui::{Env, View, on_axis};

impl LibraryScreen {
    pub(super) fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, H>) {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        self.ground.draw(f.painter.alpha(f.page_alpha), Rect::FULL);
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let env = Env::inert();
        let source_chip = self.source_chip(f.cx);
        if source_chip.is_none() && !self.libraries.is_empty() {
            self.library_capsules.draw(p, self.library_rect(0, f.cx).y, crate::ui::widgets::StatusOverlay::CTRL_H,
                crate::ui::widgets::TabGround::Plated { pop: self.library_pop.scale_with(0, f.press.scale) });
        }
        for (index, (elem, section)) in self.libraries.iter().enumerate() {
            let label = self.library_label(*section, f.cx);
            let rect = self.library_rect(index, f.cx);
            if !on_axis(rect.y, rect.h, SCR_H, 0.0) { continue; }
            if let Some(chip) = &source_chip {
                ValueChip::new(chip.name, &chip.value, chip.note.as_deref(), rect)
                    .focused(f.focus.current.is_some_and(|key| key.elem == *elem)).draw(&env, p);
            } else {
                let (focused, selected) = self.library_capsules.mixes((rect.x, rect.w));
                TabPill::new(label.as_ptr(), theme::size::BODY, rect).plated()
                    .mix(focused, selected).draw(&env, p);
            }
            self.stop(*elem, f);
        }
        for (index, row) in self.shelves.iter().enumerate() {
            let Some(shelf) = H::section_hubs(f.cx).shelves().get(index) else { continue };
            let origin = self.layout.shelf_y(index, self.scroll.pos);
            if !on_axis(origin - crate::ui::consts::TITLE_DY, self.layout.shelf_pitch(index), SCR_H, 0.0) { continue; }
            card_row::draw_heading(p, &shelf.title, "", MARGIN_X,
                origin - crate::ui::consts::TITLE_DY - row.motion.lift(), layout::GRID_RIGHT - MARGIN_X);
            let focused = f.focus.current.and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
            for (col, &elem) in row.elems.iter().enumerate() {
                if focused == Some(col) { continue; }
                self.draw_shelf_tile(index, col, false, f);
                self.stop(elem, f);
            }
            if let Some(col) = focused {
                self.draw_shelf_tile(index, col, true, f);
                self.stop(row.elems[col], f);
            }
        }
        if self.layout.grid_head {
            for elem in [SORT, FILTER] {
                let chip = self.toolbar_chip(elem, f.cx);
                if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                    ValueChip::new(chip.name, &chip.value, chip.note.as_deref(), placed.rect)
                        .focused(f.focus.current.is_some_and(|key| key.elem == elem)).draw(&env, p);
                    self.stop(elem, f);
                }
            }
            card_row::draw_heading(p, "All", "", MARGIN_X,
                CONTENT_TOP + self.layout.grid_block_top() - self.scroll.pos, layout::GRID_RIGHT - MARGIN_X);
        }
        if self.readout == Readout::Loading {
            // Preserve the Library's standalone loading spinner, outside either content fade.
            crate::ui::widgets::Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
                .phase(f.cx.tick.ms).draw(&env, f.painter.alpha(f.page_alpha));
        } else if self.readout != Readout::Grid {
            let (text, reason) = self.status_text(f.cx);
            let kind = match self.readout {
                Readout::Failed => StatusKind::Failed,
                Readout::Empty => StatusKind::Empty,
                Readout::Loading => StatusKind::Working,
                Readout::Grid => unreachable!(),
            };
            let mut status = StatusOverlay::new(self.status_frame(), &text, kind)
                .phase(f.cx.tick.ms).focused(f.focus.current.is_some_and(|key| key.elem == RETRY));
            if let Some(reason) = &reason { status = status.reason(reason); }
            if self.readout == Readout::Failed { status = status.action(c"Try again"); }
            let alpha = if self.readout == Readout::Empty { self.page_fade.alpha() * self.grid_fade.alpha() } else { 1.0 };
            status.draw(&env, f.painter.alpha(f.page_alpha * alpha));
            if self.readout == Readout::Failed { self.stop(RETRY, f); }
        }
        // MasterDetail is the production render composition, sharing children with focus queries.
        draw_faded_part(&mut self.pair, f, self.page_fade.alpha() * self.grid_fade.alpha());
    }

    fn stop<H: LibraryLike>(&self, elem: u32, f: &mut DrawFrame<'_, H>) {
        let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { return };
        f.stop(f.painter, Stop {
            key: self.key(elem), rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
            hover: Hover::Focus, activate: Activate::Press,
        });
    }

    fn draw_shelf_tile<H: LibraryLike>(&self, row: usize, col: usize, focused: bool, f: &DrawFrame<'_, H>) {
        let Some(shelf) = H::section_hubs(f.cx).shelves().get(row) else { return };
        let Some(item) = shelf.items.get(col) else { return };
        let model = &self.shelves[row];
        let style = row_style(model);
        let mut rect = self.shelf_rect(row, col);
        let scale = model.motion.scale(col) * if focused && f.press.scale > 0.0 { f.press.scale } else { 1.0 };
        if focused && f.press.scale > 0.0 { rect = rect.scaled(f.press.scale); }
        if !on_axis(rect.x, rect.w, SCR_W, 32.0) { return; }
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let art = if shelf.landscape { Art::Still(Some(item)) } else { Art::Poster(Some(item)) };
        let resume = if shelf.landscape { None } else { item.resume_frac() };
        if focused {
            card_row::draw_focused(p, art, rect, scale, style, resume,
                &shelf_label(shelf, col).revealed(model.motion.band_reveal()));
        } else {
            card_row::draw_tile(p, art, rect, scale, style, resume);
        }
        if shelf.landscape {
            crate::ui::widgets::still_overlay(p, item, rect, style.tile_radius(rect, scale), shelf.is_continue);
        }
    }

    pub(crate) fn redraw_focused<H: LibraryLike>(&self, f: &mut DrawFrame<'_, H>, focus: Option<FocusKey<u32>>) {
        let Some(key) = focus.filter(|key| key.entry == self.entry) else { return };
        if let Some((row, col)) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col))) {
            self.draw_shelf_tile(row, col, true, f);
        }
    }
}

pub(super) fn draw_faded_part<H: LibraryLike>(part: &mut impl Part<H>, f: &mut DrawFrame<'_, H>, alpha: f32) {
    let parent = f.page_alpha;
    f.page_alpha = parent * alpha;
    part.draw(f, Rect::FULL);
    f.page_alpha = parent;
}

fn shelf_label(shelf: &crate::browse::section_hubs::Shelf, col: usize) -> card_row::TileLabel {
    let Some(item) = shelf.items.get(col) else { return card_row::TileLabel::title("") };
    if shelf.landscape {
        let name = if item.title.is_empty() || item.title == item.show_title {
            crate::ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64)
        } else { item.title.clone() };
        let fact = if shelf.is_continue && item.resume_frac().is_some() {
            crate::ui::fmt::time_left(item.dur_ns / 1_000_000 - item.resume_ms)
        } else if item.aired.is_empty() && item.year <= 0 { String::new() }
        else { crate::ui::fmt::pretty_date(&item.aired, item.year as i64) };
        return if fact.is_empty() { card_row::TileLabel::title(&name) }
        else { card_row::TileLabel::titled(&name, &fact) };
    }
    let mut label = if shelf.is_continue { card_row::TileLabel::played(&item.title) }
        else { card_row::TileLabel::title(&item.title) };
    label.caption = card_row::focused_caption(item, shelf.is_continue);
    label
}
