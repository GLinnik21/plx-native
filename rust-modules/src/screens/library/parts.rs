//! The concrete master/detail pair: A–Z rail (master) and six-column listing (detail).

pub(super) use super::rail::RailPart;

use crate::screens::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::{CARD_H, CARD_W, MARGIN_X, SCR_H};
use crate::ui::frame::Budget;
use crate::ui::machine::{Cx, EntryId, FocusKey, GroupId};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Part,
    Placed, Seat, Step, Stop,
};
use crate::ui::widgets::Art;
use crate::ui::Rect;

use super::identity::KeyRegistry;
use super::layout::{
    Layout, COLS, CONTENT_TOP, GRID_RIGHT,
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
    pub(super) fn clear_projection(&mut self) {
        self.elems.clear();
        self.identity = None;
        self.snapshot = None;
    }

    pub(super) const SHAPE: &'static str = "LibraryGrid{group:u32,elems:[u32],known:[(elem:u32,index:u32)],identity:Option<(epoch:u32,sid:u32,section:u64,query:u32)>,layout:LibraryLayout,scroll:f32,target_layout:LibraryLayout,scroll_target:f32}";

    pub(super) fn write(&self, c: &mut crate::ui::machine::Canon) {
        // The retained snapshot is a read-publication cache, not another cursor. Its placement
        // projection and identity are traversed below; its Arc address never enters logical state.
        let Self { entry: _, group, elems, known, identity, layout, scroll, target_layout, scroll_target, snapshot: _ } = self;
        c.u32(group.0).seq(elems.len());
        for elem in elems { c.u32(*elem); }
        c.seq(known.len());
        for (elem, index) in known { c.u32(*elem).u32(*index as u32); }
        c.option(*identity, |c, (epoch, sid, section, query)| {
            c.u32(epoch).u32(u32::from(sid.raw())).u64(section as u64).u32(query);
        });
        layout.write(c); c.f32(*scroll);
        target_layout.write(c); c.f32(*scroll_target);
    }

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

    pub(super) fn record_stops<H: LibraryLike>(&self, f: &mut crate::ui::screen::DrawFrame<'_, H>) {
        let (lo, hi) = self.visible_window();
        for index in lo..hi {
            let elem = self.elems[index];
            let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { continue };
            f.stop(f.painter, Stop { key: FocusKey { entry: self.entry, elem },
                rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
                hover: Hover::Focus, activate: Activate::Press });
        }
    }

    pub(super) fn draw_focused<H: LibraryLike>(&self, f: &crate::ui::screen::DrawFrame<'_, H>, focus: Option<FocusKey<u32>>) {
        let Some(index) = focus.filter(|key| key.entry == self.entry).and_then(|key| self.index_of(key.elem)) else { return };
        let Some(item) = H::listing(f.cx).item(index) else { return };
        let p = f.painter.alpha(f.page_alpha);
        let rect = self.rect_at(index, true, f.press.scale);
        let label = card_row::TileLabel::title(&item.title);
        card_row::draw_focused(p, Art::Poster(Some(item)), rect, RowStyle::HOME.focus_scale,
            &GRID_STYLE, item.resume_frac(), &label);
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
        self.draw_focused(f, focus);
        self.record_stops(f);
    }
}
