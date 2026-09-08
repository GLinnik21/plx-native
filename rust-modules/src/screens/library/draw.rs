//! Library paint consumes the same placement queries as keyboard and pointer navigation.
use std::ffi::CString;
use super::*;
use crate::ui::card_row;
use crate::ui::screen::{Activate, Hover, Stop};
use crate::ui::theme;
use crate::ui::widgets::{Art, Button, TabPill, StatusOverlay, StatusKind};
use crate::ui::{Env, View, on_axis};

impl LibraryScreen {
    pub(super) fn draw_page<H: LibraryLike>(&mut self, f: &mut DrawFrame<'_, H>) {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha());
        let env = Env::inert();
        let directory = H::directory(f.cx);
        for (index, (elem, section)) in self.libraries.iter().enumerate() {
            let label = CString::new(directory.sections().get(*section).map(|s| s.row.title.as_str()).unwrap_or("More")).unwrap_or_default();
            let rect = self.library_rect(index, f.cx);
            if !on_axis(rect.y, rect.h, SCR_H, 0.0) { continue; }
            let selected = directory.current() == Some(*section);
            let focused = f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == *elem);
            TabPill::new(label.as_ptr(), theme::size::BODY, rect).plated()
                .mix(f32::from(focused), f32::from(selected)).draw(&env, p);
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
            let listing = H::listing(f.cx);
            let queued = self.pending.grid().filter(|(target, _)| target.matches(listing))
                .and_then(|(_, action)| match action { GridAction::Sort { key, .. } => listing.sorts().iter().find(|sort| &sort.key == key), _ => None });
            let sort_label = queued.or_else(|| listing.sorts().get(listing.sort_index()))
                .map(|sort| sort.title.as_str()).unwrap_or("Title");
            let sort_label = CString::new(format!("Sort: {sort_label}")).unwrap_or_default();
            for (elem, label) in [(SORT, sort_label.as_c_str()), (FILTER, c"Filter")] {
                if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                    Button::new(label.as_ptr(), theme::size::BODY, placed.rect)
                        .focused(f.focus.current.is_some_and(|key| key.elem == elem)).draw(&env, p);
                    self.stop(elem, f);
                }
            }
            card_row::draw_heading(p, "All", "", MARGIN_X,
                CONTENT_TOP + self.layout.grid_block_top() - self.scroll.pos, layout::GRID_RIGHT - MARGIN_X);
        }
        if self.readout != Readout::Grid {
            let (text, kind) = match self.readout {
                Readout::Failed => (c"Couldn't load this library", StatusKind::Failed),
                Readout::Empty => (c"Nothing here matches", StatusKind::Empty),
                Readout::Loading => (c"Loading…", StatusKind::Working),
                Readout::Grid => unreachable!(),
            };
            let mut status = StatusOverlay::new(Rect::FULL, text, kind);
            if self.readout == Readout::Failed { status = status.action(c"Try again"); }
            status.draw(&env, p);
            if self.readout == Readout::Failed { self.stop(RETRY, f); }
        }
        // MasterDetail is the production render composition, sharing children with focus queries.
        self.pair.draw(f, Rect::FULL);
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
