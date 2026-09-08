//! The concrete master/detail pair: A–Z rail (master) and six-column listing (detail).

use std::ffi::CString;

use crate::screens::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, SCR_H};
use crate::ui::frame::Budget;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Part,
    Placed, Seat, Step, Stop,
};
use crate::ui::theme;
use crate::ui::widgets::Art;
use crate::ui::Rect;

use super::identity::KeyRegistry;
use super::layout::{
    Layout, COLS, CONTENT_TOP, GRID_RIGHT, MAX_LETTERS, RAIL_PITCH,
    RAIL_TRACK_W,
};

pub(super) const GRID_GROUP: GroupId = GroupId(0x4c49_4201);
pub(super) const RAIL_GROUP: GroupId = GroupId(0x4c49_4202);
const NO_HOLES: &[(usize, usize)] = &[];
const GRID_STYLE: RowStyle = RowStyle::HOME.with_right_reserve(crate::ui::consts::SCR_W - GRID_RIGHT);

pub(super) struct GridPart {
    entry: EntryId,
    group: GroupId,
    pub(super) elems: Vec<u32>,
    known: Vec<(u32, usize)>,
    identity: Option<(u32, crate::plex::ServerId, i64, u32)>,
    layout: Layout,
    scroll: f32,
    target_layout: Layout,
    scroll_target: f32,
    snapshot: Option<crate::stores::browse::ListingSnapshot>,
}

impl GridPart {
    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        Self {
            entry, group,
            elems: Vec::new(),
            known: Vec::new(),
            identity: None,
            layout: Layout::new(false, &[], 0, false),
            scroll: 0.0,
            target_layout: Layout::new(false, &[], 0, false),
            scroll_target: 0.0,
            snapshot: None,
        }
    }

    pub(super) fn set_geometry(&mut self, layout: Layout, scroll: f32, target_layout: Layout, scroll_target: f32) {
        self.layout = layout;
        self.scroll = scroll;
        self.target_layout = target_layout;
        self.scroll_target = scroll_target;
    }

    pub(super) fn restore_keys(&mut self, keys: &KeyRegistry) {
        self.known = keys.keys().iter().filter(|key| matches!(key.identity,
            LibraryIdentity::Grid { .. } | LibraryIdentity::GridSlot { .. }))
            .map(|key| (key.elem, key.last_index as usize)).collect();
    }

    pub(super) fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry) {
        let view = H::listing(cx);
        if self.snapshot.as_ref().is_some_and(|old| view.same_items(old.view())) { return; }
        self.snapshot = Some(view.retain());
        let Some(id) = view.id() else {
            self.elems.clear();
            self.identity = None;
            return;
        };
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        let total = view.total().max(0) as usize;
        let stamp = (id.epoch, id.sid, id.section, id.query);
        if self.identity != Some(stamp) || self.elems.len() != total {
            self.elems.clear();
            self.elems.reserve(total);
            for index in 0..total {
                let identity = item_identity(view, &section, id.query, index);
                self.elems.push(keys.register(identity, self.group, index));
            }
            self.identity = Some(stamp);
        } else {
            for (index, elem) in self.elems.iter_mut().enumerate() {
                let identity = item_identity(view, &section, id.query, index);
                *elem = keys.register(identity, self.group, index);
            }
        }
        for (index, &elem) in self.elems.iter().enumerate() {
            if let Some(old) = self.known.iter_mut().find(|(key, _)| *key == elem) {
                old.1 = index;
            } else {
                self.known.push((elem, index));
            }
        }
    }

    pub(super) fn index_of(&self, elem: u32) -> Option<usize> {
        self.elems.iter().position(|&key| key == elem)
    }

    pub(super) fn elem_at(&self, index: usize) -> Option<u32> {
        self.elems.get(index).copied()
    }

    pub(super) fn fallback_for(&self, elem: u32) -> Option<u32> {
        let index = self.known.iter().find(|(key, _)| *key == elem).map(|(_, index)| *index)?;
        self.elems.get(index.min(self.elems.len().saturating_sub(1))).copied()
    }

    pub(super) fn rect_at(&self, index: usize, focused: bool, press: f32) -> Rect {
        let row = index / COLS;
        let col = index % COLS;
        let scale = if focused {
            RowStyle::HOME.focus_scale * if press > 0.0 { press } else { 1.0 }
        } else {
            1.0
        };
        Rect::new(Layout::grid_x(col), self.layout.row_y(row, self.scroll), CARD_W, CARD_H).scaled(scale)
    }

    pub(super) fn visible_window(&self) -> (usize, usize) {
        let (lo, hi) = self.layout.visible_rows(self.scroll);
        (lo.saturating_mul(COLS), hi.saturating_mul(COLS).min(self.elems.len()))
    }
}

fn item_identity(
    view: crate::stores::browse::ListingView<'_>,
    section: &LibrarySectionIdentity,
    query: u32,
    index: usize,
) -> LibraryIdentity {
    match view.item(index) {
        Some(item) if !item.rk.is_empty() => LibraryIdentity::Grid {
            section: section.clone(),
            sid: item.sid,
            rk: item.rk.clone(),
        },
        _ => LibraryIdentity::GridSlot {
            section: section.clone(),
            query,
            slot: index as u32,
        },
    }
}

impl<H: LibraryLike> Focusable<H> for GridPart {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.elems.is_empty() { return; }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Grid { cols: COLS, holes: NO_HOLES },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(MARGIN_X, self.target_layout.row_y(0, self.scroll_target), GRID_RIGHT - MARGIN_X, CARD_H),
            len: self.elems.len(),
            elem: ElemKind::Card,
        });
    }

    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.index_of(*key).map(|_| self.group)
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.index_of(key.elem) else { return Step::Edge };
        let row = index / COLS;
        let col = index % COLS;
        let next = match dir {
            Dir::Left => col.checked_sub(1).map(|c| row * COLS + c),
            Dir::Right => (col + 1 < COLS).then_some(index + 1),
            Dir::Up => row.checked_sub(1).map(|r| r * COLS + col),
            Dir::Down => Some((row + 1) * COLS + col),
        }
        .filter(|&i| i < self.elems.len());
        next.map_or(Step::Edge, |i| Step::Move(FocusKey { entry: key.entry, elem: self.elems[i] }))
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let index = self.index_of(*key)?;
        let focused = cx.focus.current.is_some_and(|focus| focus.entry == self.entry && focus.elem == *key);
        let (rect, rest_rect) = match at {
            At::Drawn => (self.rect_at(index, focused, cx.press.scale), self.rect_at(index, focused, 1.0)),
            At::SpringTarget => {
                let rect = Rect::new(Layout::grid_x(index % COLS), self.target_layout.row_y(index / COLS, self.scroll_target), CARD_W, CARD_H)
                    .scaled(if focused { RowStyle::HOME.focus_scale } else { 1.0 });
                (rect, rect)
            }
        };
        Some(Placed {
            rect,
            rest_rect,
            clip: Rect::new(MARGIN_X - 32.0, CONTENT_TOP, GRID_RIGHT - MARGIN_X + 32.0, SCR_H - CONTENT_TOP),
            index: Some(index as u32),
        })
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.index_of(want.elem).is_some() { return want; }
        self.fallback_for(want.elem)
            .or_else(|| self.elems.first().copied())
            .map(|elem| FocusKey { entry: want.entry, elem })
            .unwrap_or(want)
    }

    fn seat(&self, _group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let index = from.index.unwrap_or(0) as usize;
        FocusKey {
            entry: self.entry,
            elem: self.elems.get(index.min(self.elems.len().saturating_sub(1))).copied().unwrap_or(0),
        }
    }
}

impl<H: LibraryLike> Part<H> for GridPart {
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut crate::ui::screen::DrawFrame<'_, H>, _rect: Rect) {
        let view = H::listing(f.cx);
        let focus = f.focus.current.filter(|key| key.entry == self.entry);
        let (lo, hi) = self.visible_window();
        let p = f.painter.alpha(f.page_alpha);
        for index in lo..hi {
            let Some(item) = view.item(index) else { continue };
            let selected = focus.is_some_and(|key| key.elem == self.elems[index]);
            if selected { continue; }
            card_row::draw_tile(p, Art::Poster(Some(item)), self.rect_at(index, false, 1.0), 1.0, &GRID_STYLE, item.resume_frac());
        }
        if let Some(key) = focus {
            if let Some(index) = self.index_of(key.elem) {
                if let Some(item) = view.item(index) {
                    let rect = self.rect_at(index, true, f.press.scale);
                    let label = card_row::TileLabel::title(&item.title);
                    card_row::draw_focused(p, Art::Poster(Some(item)), rect, RowStyle::HOME.focus_scale, &GRID_STYLE, item.resume_frac(), &label);
                }
            }
        }
        for index in lo..hi {
            let elem = self.elems[index];
            let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { continue };
            f.stop(f.painter, Stop {
                key: FocusKey { entry: self.entry, elem },
                rect: placed.rect,
                rest_rect: placed.rest_rect,
                clip: placed.clip,
                hover: Hover::Focus,
                activate: Activate::Press,
            });
        }
    }
}

pub(super) struct RailPart {
    entry: EntryId,
    group: GroupId,
    pub(super) elems: Vec<u32>,
    labels: Vec<CString>,
    starts: Vec<usize>,
    rect: Rect,
    scroll: f32,
}

impl RailPart {
    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        Self { entry, group, elems: Vec::new(), labels: Vec::new(), starts: Vec::new(), rect: Rect::new(0.0, 0.0, 0.0, 0.0), scroll: 0.0 }
    }

    pub(super) fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry, layout: Layout, document_scroll: f32) {
        let view = H::listing(cx);
        self.rect = layout.rail_rect(document_scroll);
        let Some(id) = view.id() else {
            self.elems.clear();
            self.labels.clear();
            self.starts.clear();
            return;
        };
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        self.elems.clear();
        self.labels.clear();
        self.starts.clear();
        let mut start = 0usize;
        for (index, (label, count)) in view.letters().iter().take(MAX_LETTERS).enumerate() {
            self.starts.push(start);
            start = start.saturating_add((*count).max(0) as usize);
            self.elems.push(keys.register(
                LibraryIdentity::Rail { section: section.clone(), label: label.clone() },
                self.group,
                index,
            ));
            self.labels.push(CString::new(label.as_str()).unwrap_or_default());
        }
        if !view.rail_available() || self.elems.len() < 2 {
            self.elems.clear();
            self.labels.clear();
            self.starts.clear();
        }
        self.reveal_current(cx.focus.current);
    }

    fn reveal_current(&mut self, focus: Option<FocusKey<u32>>) {
        let Some(index) = focus.and_then(|key| self.elems.iter().position(|&elem| elem == key.elem)) else { return };
        let visible = (self.rect.h / RAIL_PITCH).floor().max(1.0) as usize;
        if index < self.scroll as usize {
            self.scroll = index as f32;
        } else if index >= self.scroll as usize + visible {
            self.scroll = (index + 1 - visible) as f32;
        }
        self.scroll = self.scroll.clamp(0.0, self.elems.len().saturating_sub(visible) as f32);
    }

    pub(super) fn letter_for_item(&self, index: usize) -> usize {
        self.starts.iter().enumerate().rev().find(|(_, start)| **start <= index).map(|(i, _)| i).unwrap_or(0)
    }

    pub(super) fn start_for_elem(&self, elem: u32) -> Option<usize> {
        self.elems.iter().position(|&key| key == elem).and_then(|index| self.starts.get(index).copied())
    }
}

impl<H: LibraryLike> Focusable<H> for RailPart {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.elems.is_empty() { return; }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::First,
            reachable: AxisMask::HORIZONTAL,
            edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop],
            extent: self.rect,
            len: self.elems.len(),
            elem: ElemKind::Bare,
        });
    }

    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.elems.contains(key).then_some(self.group)
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.elems.iter().position(|&elem| elem == key.elem) else { return Step::Edge };
        let next = match dir {
            Dir::Up => index.checked_sub(1),
            Dir::Down => (index + 1 < self.elems.len()).then_some(index + 1),
            Dir::Left | Dir::Right => None,
        };
        next.map_or(Step::Edge, |i| Step::Move(FocusKey { entry: key.entry, elem: self.elems[i] }))
    }

    fn place(&self, key: &u32, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let index = self.elems.iter().position(|&elem| elem == *key)?;
        let y = self.rect.y + (index as f32 - self.scroll) * RAIL_PITCH;
        let rect = Rect::new(self.rect.x, y, RAIL_TRACK_W, RAIL_PITCH);
        Some(Placed { rect, rest_rect: rect, clip: self.rect, index: Some(index as u32) })
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.elems.contains(&want.elem) { want }
        else { self.elems.first().copied().map(|elem| FocusKey { entry: want.entry, elem }).unwrap_or(want) }
    }

    fn seat(&self, _group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let letter = self.letter_for_item(from.index.unwrap_or(0) as usize);
        FocusKey { entry: self.entry, elem: self.elems.get(letter).copied().unwrap_or(0) }
    }
}

impl<H: LibraryLike> Part<H> for RailPart {
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut crate::ui::screen::DrawFrame<'_, H>, _rect: Rect) {
        if self.elems.is_empty() { return; }
        let p = f.painter.alpha(f.page_alpha);
        let _clip = f.clip(p, self.rect);
        for (index, label) in self.labels.iter().enumerate() {
            let elem = self.elems[index];
            let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { continue };
            if placed.rect.y + placed.rect.h < self.rect.y || placed.rect.y > self.rect.y + self.rect.h { continue; }
            let focused = f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == elem);
            if focused {
                p.rect(
                    placed.rect.inset(3.0),
                    17.0,
                    theme::OVERLAY_FOCUS_PILL,
                    theme::OVERLAY_FOCUS_PILL,
                    0.0,
                );
            }
            crate::ui::label::Label::new(
                label.as_ptr(),
                theme::size::CAPTION,
                if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY },
            )
            .h(crate::ui::label::HAlign::Center)
            .draw(p, placed.rect);
            f.stop(f.painter, Stop {
                key: FocusKey { entry: self.entry, elem },
                rect: placed.rect,
                rest_rect: placed.rest_rect,
                clip: placed.clip,
                hover: Hover::Focus,
                activate: Activate::Direct,
            });
        }
    }
}
