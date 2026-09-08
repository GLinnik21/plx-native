//! Library instance: document motion and transactions belong here; focus belongs to Input.
mod identity;
mod layout;
mod parts;
mod transactions;
mod draw;
pub(crate) mod menu;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use crate::browse::{SecFetch, SecKind};
use crate::screens::registry::{
    AppFx, AppMsg, HomeTab, LibraryCmd, LibraryIdentity, LibraryLike, LibraryMemory,
    LibraryReq, LibrarySectionIdentity, PageMemory,
};
use crate::stores::{StoreCmd, StoreId, StoreWork};
use crate::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use crate::ui::card_row::{CardRow, RowStyle};
use crate::ui::consts::{MARGIN_X, SCR_W, SCR_H, K_SCROLL, CARD_DY};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InstanceId, Key, LogicalState, Machine, MachineId,
};
use crate::ui::master_detail::{
    Follow, MasterDetail, MasterDetailGroups, MasterDetailLayout, MasterDetailPolicy, MasterSide,
    Outcome as MasterOutcome, Region,
};
use crate::ui::present::Provenance;
use crate::ui::screen::{
    At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable,
    GroupKind, GroupSpec, Link, Part, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
};
use crate::ui::{Rect, Spring};
use crate::ui::xfade::Xfade;
use identity::{KeyRegistry, KeyRegion, region_of_elem};
use layout::{Layout, Block, COLS, CONTENT_TOP, MAX_SHELVES};
use parts::{GridPart, RailPart, GRID_GROUP, RAIL_GROUP};
use transactions::{PendingTransactions, SectionTarget, GridTarget, GridAction};

pub(crate) const LIBRARY_GROUP: GroupId = GroupId(0x4c49_4210);
pub(crate) const TOOLBAR_GROUP: GroupId = GroupId(0x4c49_4211);
const STATUS_GROUP: GroupId = GroupId(0x4c49_4212);
const SORT: u32 = 1;
const FILTER: u32 = 2;
const RETRY: u32 = 3;
const MORE: u32 = 4;
const STRIP: GroupId = crate::ui::containers::tabs::STRIP;

pub(crate) const SHAPE: [&str; 2] = [
    "LibraryScreen{kind:u32,scroll:{pos:f32,vel:f32},scroll_target:f32,restore_scroll:Option<f32>,live:bool,initial:bool,epoch:Option<u32>,query:Option<u32>,shelf_publication:Option<(HubsId{epoch:u32,sid:u32,section:u64},revision:u64)>,page_fade:Xfade{phase:u8,t:f32},grid_fade:Xfade{phase:u8,t:f32},pair:MasterDetailState{side:u32,follow:u32,band:u32,door:Option<u32>},memory:PageMemory::Library,shelves:[{id:str,group:u32,motion:CardRow}],pending:PendingTransactions}",
    transactions::SHAPE,
];

struct Regions;
impl<H: LibraryLike> crate::ui::master_detail::KeyRegion<H> for Regions {
    fn region(&self, elem: &u32, _: &Cx<'_, H>) -> Option<Region> {
        match region_of_elem(*elem) {
            Some(KeyRegion::Grid) => Some(Region::Detail),
            Some(KeyRegion::Rail) => Some(Region::Master),
            _ => None,
        }
    }
}

struct Shelf {
    id: String,
    group: GroupId,
    elems: Vec<u32>,
    landscape: bool,
    motion: CardRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readout { Loading, Empty, Failed, Grid }

fn readout(table: SecFetch, sections: usize, fetch: SecFetch, total: i64) -> Readout {
    if total < 0 {
        match table {
            SecFetch::Failed => return Readout::Failed,
            SecFetch::Ready if sections == 0 => return Readout::Empty,
            _ => {}
        }
    }
    match fetch {
        SecFetch::Loading => Readout::Loading,
        SecFetch::Failed if total < 0 => Readout::Failed,
        SecFetch::Failed => Readout::Grid,
        SecFetch::Ready if total == 0 => Readout::Empty,
        SecFetch::Ready => Readout::Grid,
    }
}

/// A retained page body, also remountable from its entry-owned LibraryMemory.
pub(crate) struct LibraryScreen {
    entry: EntryId,
    instance: InstanceId,
    kind: SecKind,
    keys: KeyRegistry,
    pair: MasterDetail<RailPart, GridPart, Regions>,
    libraries: Vec<(u32, usize)>,
    shelves: Vec<Shelf>,
    section: Option<LibrarySectionIdentity>,
    epoch: Option<u32>,
    query: Option<u32>,
    shelf_publication: Option<(crate::browse::section_hubs::HubsId, u64)>,
    layout: Layout,
    target_layout: Layout,
    scroll: Spring,
    scroll_target: f32,
    restore_scroll: Option<f32>,
    pending: PendingTransactions,
    page_fade: Xfade,
    grid_fade: Xfade,
    readout: Readout,
    live: bool,
    initial: bool,
}

impl LibraryScreen {
    pub(crate) fn new(entry: EntryId, instance: InstanceId, kind: SecKind) -> Self {
        let layout = Layout::new(false, &[], 0, false);
        Self {
            entry, instance, kind, keys: KeyRegistry::default(),
            pair: MasterDetail::new(
                RailPart::new(entry), GridPart::new(entry),
                MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL },
                MasterDetailGroups { master: RAIL_GROUP, detail: GRID_GROUP },
                MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
            ),
            libraries: Vec::new(), shelves: Vec::new(), section: None, epoch: None, query: None,
            shelf_publication: None, layout, target_layout: layout,
            scroll: Spring::at(0.0), scroll_target: 0.0, restore_scroll: None,
            pending: PendingTransactions::default(), page_fade: Xfade::new(), grid_fade: Xfade::new(),
            readout: Readout::Loading, live: true, initial: true,
        }
    }

    pub(crate) fn restore(&mut self, memory: &LibraryMemory) {
        self.keys = KeyRegistry::restore(memory);
        self.pair.detail.restore_keys(&self.keys);
        self.section = memory.section.clone();
        self.restore_scroll = Some(memory.scroll);
        self.scroll.jump(memory.scroll);
        self.scroll_target = memory.scroll;
        self.initial = false;
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: self.entry, elem } }

    fn reseat<H: LibraryLike>(&self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Deliver(MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    fn address<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<SectionAddress> {
        let id = H::listing(cx).id()?;
        Some(SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section })
    }

    /// User intent follows the incoming page while the committed listing is still fading out.
    fn requested_address<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<SectionAddress> {
        self.pending.section().map(|section| SectionAddress {
            epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
        }).or_else(|| self.address(cx))
    }

    fn store<H: LibraryLike>(&self, target: SectionAddress, work: LibraryWork, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Browse,
            StoreCmd::Browse(BrowseCmd::Addressed { target, work }))));
    }

    fn sync<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let directory = H::directory(cx);
        let identity = listing.id().map(|id| LibrarySectionIdentity { sid: id.sid, key: id.section });
        let changed = self.section != identity || self.epoch != directory.epoch();
        if changed {
            self.shelves.clear();
            self.shelf_publication = None;
            self.section = identity.clone();
            self.epoch = directory.epoch();
            self.scroll.jump(self.restore_scroll.unwrap_or(0.0));
            self.scroll_target = self.scroll.pos;
        }
        self.query = listing.id().map(|id| id.query);
        self.readout = readout(directory.source_fetch(), directory.sections().len(), listing.fetch(), listing.total());
        self.libraries.clear();
        if let Some(current) = directory.current() {
            self.kind = directory.sections()[current].kind;
            for (index, section) in directory.favorite_sections_for(current) {
                let Some(sid) = section.sid else { continue };
                let elem = self.keys.register(
                    LibraryIdentity::Library(LibrarySectionIdentity { sid, key: section.key }),
                    LIBRARY_GROUP, self.libraries.len());
                self.libraries.push((elem, index));
            }
            let widths: Vec<_> = self.libraries.iter().map(|(_, index)|
                cx.measure.width(&std::ffi::CString::new(directory.sections()[*index].row.title.as_str()).unwrap_or_default(), crate::ui::theme::size::BODY, false) + 48.0).collect();
            let selected = self.libraries.iter().position(|(_, index)| *index == current).unwrap_or(0);
            let (start, len) = layout::library_window(&widths, selected, layout::GRID_RIGHT - MARGIN_X, 16.0, 96.0, 8);
            if len < self.libraries.len() {
                self.libraries = self.libraries[start..start + len].to_vec();
                self.libraries.push((MORE, usize::MAX));
            }
        }
        let hubs = H::section_hubs(cx);
        let publication = hubs.id().zip(hubs.revision());
        if self.shelf_publication != publication {
            let mut old = std::mem::take(&mut self.shelves);
            if let Some(section) = &identity {
                for (index, shelf) in hubs.shelves().iter().take(MAX_SHELVES).enumerate() {
                    let group = GroupId(self.keys.register(
                        LibraryIdentity::Control { section: section.clone(), kind: "shelf-group".into(), key: shelf.id.clone() },
                        GroupId(0), index));
                    let elems = shelf.items.iter().enumerate().map(|(col, item)| {
                        let identity = if item.rk.is_empty() {
                            LibraryIdentity::ShelfSlot { section: section.clone(), hub: shelf.id.clone(), publication: self.query.unwrap_or(0), slot: col as u32 }
                        } else {
                            LibraryIdentity::Shelf { section: section.clone(), hub: shelf.id.clone(), sid: item.sid, rk: item.rk.clone() }
                        };
                        self.keys.register(identity, group, col)
                    }).collect();
                    let motion = old.iter().position(|row| row.id == shelf.id)
                        .map(|i| old.remove(i).motion).unwrap_or_else(CardRow::new);
                    self.shelves.push(Shelf { id: shelf.id.clone(), group, elems, landscape: shelf.landscape, motion });
                }
            }
            self.shelf_publication = publication;
        }
        self.pair.detail.refresh(cx, &mut self.keys);
        self.relayout(cx.focus.current);
        self.pair.master.refresh(cx, &mut self.keys, self.layout, self.scroll.pos);
    }

    fn relayout(&mut self, focus: Option<FocusKey<u32>>) {
        let pitches: Vec<_> = self.shelves.iter().map(|row| layout::shelf_pitch(row.landscape, row.motion.band_expand())).collect();
        let targets: Vec<_> = self.shelves.iter().map(|row| layout::shelf_pitch(row.landscape,
            f32::from(focus.is_some_and(|key| row.elems.contains(&key.elem))))).collect();
        let rows = self.pair.detail.elems.len().div_ceil(COLS);
        let grid_head = rows > 0 || self.grid_fade.is_swapping();
        self.layout = if self.readout == Readout::Failed {
            Layout::failed(!self.libraries.is_empty(), &pitches)
        } else { Layout::new(!self.libraries.is_empty(), &pitches, rows, grid_head) };
        self.target_layout = Layout::new(!self.libraries.is_empty(), &targets, rows, grid_head);
        self.layout.status = self.readout == Readout::Failed;
        self.target_layout.status = self.layout.status;
        self.pair.detail.set_geometry(self.layout, self.scroll.pos, self.target_layout, self.scroll_target);
    }

    fn first_group(&self) -> GroupId {
        match self.layout.first() {
            Some(Block::LibraryRow) => LIBRARY_GROUP,
            Some(Block::Shelf(index)) => self.shelves[index].group,
            Some(Block::Status) => STATUS_GROUP,
            Some(Block::Toolbar | Block::Grid(_)) => TOOLBAR_GROUP,
            None => STRIP,
        }
    }

    fn reveal<H: LibraryLike>(&mut self, key: FocusKey<u32>, by: By, cx: &Cx<'_, H>) {
        self.relayout(Some(key));
        let want = if let Some(index) = self.pair.detail.index_of(key.elem) {
            Some(self.target_layout.row_reveal(index / COLS))
        } else if let Some(index) = self.shelves.iter().position(|row| row.elems.contains(&key.elem)) {
            Some(self.target_layout.shelf_reveal(index))
        } else if key.elem == SORT || key.elem == FILTER {
            Some(self.target_layout.grid_block_top().clamp(0.0, self.target_layout.max_scroll()))
        } else if self.libraries.iter().any(|(elem, _)| *elem == key.elem) || key.elem == RETRY {
            Some(0.0)
        } else { None };
        if let Some(want) = want {
            self.scroll_target = want;
            if by == By::Restore {
                if let Some(saved) = self.restore_scroll.take() {
                    self.scroll_target = saved.clamp(0.0, self.target_layout.max_scroll());
                }
                self.scroll.jump(self.scroll_target);
            }
        }
        self.relayout(Some(key));
        self.pair.master.refresh(cx, &mut self.keys, self.layout, self.scroll.pos);
    }

    fn follow<H: LibraryLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let Some(index) = self.pair.master.start_for_elem(elem) else { return };
        let Some(target) = self.pair.detail.elem_at(index) else { return };
        fx.remember(GRID_GROUP, target);
        self.scroll_target = self.target_layout.row_reveal(index / COLS);
        fx.invalidate(Provenance::Input);
    }

    pub(crate) fn focused_item<'a, H: LibraryLike>(&self, focus: Option<FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a crate::pms::PmsMovie> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        if let Some(index) = self.pair.detail.index_of(key.elem) { return H::listing(cx).item(index); }
        let (row, col) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col)))?;
        H::section_hubs(cx).shelves().get(row)?.items.get(col)
    }

    pub(crate) fn grid_position(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        self.pair.detail.index_of(key.elem).map(|index| (index / COLS, index % COLS))
    }

    fn from_deck<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> bool {
        self.shelves.iter().position(|row| row.elems.contains(&elem))
            .and_then(|row| H::section_hubs(cx).shelves().get(row)).is_some_and(|row| row.is_continue)
    }

    fn activate<H: LibraryLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if [SORT, FILTER, MORE].contains(&elem) {
            if let (Some(target), Some(placed)) = (self.requested_address(cx), <Self as Focusable<H>>::place(self, &elem, cx, At::SpringTarget)) {
                let r = placed.rest_rect;
                fx.push(Fx::App(AppFx::Library(LibraryReq::Menu {
                    kind: match elem { SORT => crate::screens::registry::LibraryMenuKind::Sort, MORE => crate::screens::registry::LibraryMenuKind::Sources, _ => crate::screens::registry::LibraryMenuKind::Filter },
                    anchor: [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()], target,
                })));
            }
            return Handled::Yes;
        }
        if let Some((_, index)) = self.libraries.iter().find(|(key, _)| *key == elem) {
            let directory = H::directory(cx);
            let section = &directory.sections()[*index];
            if let (Some(epoch), Some(sid)) = (directory.epoch(), section.sid) {
                self.pending.request_section(SectionTarget {
                    epoch, index: *index,
                    identity: LibrarySectionIdentity { sid, key: section.key }, kind: section.kind,
                });
                self.page_fade.reload();
                fx.invalidate(Provenance::Input);
            }
            return Handled::Yes;
        }
        if region_of_elem(elem) == Some(KeyRegion::Rail) { self.follow(elem, fx); return Handled::Yes; }
        if let Some(item) = self.focused_item(Some(self.key(elem)), cx).filter(|item| !item.rk.is_empty()) {
            let from_deck = self.from_deck(elem, cx);
            let req = if held {
                LibraryReq::ItemMenu { sid: item.sid, rk: item.rk.clone(), from_deck }
            } else if from_deck {
                LibraryReq::Play { sid: item.sid, rk: item.rk.clone(), resume_ns: crate::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000) }
            } else { LibraryReq::Detail { sid: item.sid, rk: item.rk.clone() } };
            fx.push(Fx::App(AppFx::Library(req)));
            return Handled::Yes;
        }
        let strip = crate::ui::dispatch::STRIP_BASE;
        let req = match elem.checked_sub(strip) {
            Some(0) => Some(LibraryReq::Tab(HomeTab::Home)),
            Some(1) => Some(LibraryReq::Tab(HomeTab::Movies)),
            Some(2) => Some(LibraryReq::Tab(HomeTab::Shows)),
            Some(3) => Some(LibraryReq::Tab(HomeTab::Search)),
            Some(4) => Some(LibraryReq::Account),
            _ => None,
        };
        if let Some(req) = req { fx.push(Fx::App(AppFx::Library(req))); return Handled::Yes; }
        if elem == RETRY {
            if let Some(target) = self.address(cx) { self.store(target, LibraryWork::Retry, fx); }
            return Handled::Yes;
        }
        Handled::No
    }

    fn flush<H: LibraryLike>(&mut self, fx: &mut Effects<'_, H>) {
        let section = self.pending.take_section(self.epoch.unwrap_or(0));
        let (_, grid) = self.pending.flush();
        let selected = section.as_ref().map(|section| SectionAddress {
            epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
        });
        let query = grid.map(|(target, action)| (SectionAddress {
            epoch: target.epoch, sid: target.sid, section: target.section,
        }, match action {
            GridAction::Sort { key, desc } => crate::stores::browse::QueryEdit::Sort { key, desc },
            GridAction::Unwatched { desired } => crate::stores::browse::QueryEdit::Unwatched(desired),
            GridAction::Genre { id } => crate::stores::browse::QueryEdit::Genre(id),
        }));
        if let Some(target) = selected {
            let query = query.filter(|(address, _)| *address == target).map(|(_, edit)| edit);
            self.store(target, LibraryWork::Commit { select: true, choice: true, query }, fx);
        } else if let Some((target, query)) = query {
            self.store(target, LibraryWork::Commit { select: false, choice: false, query: Some(query) }, fx);
        }
    }

    fn command<H: LibraryLike>(&mut self, command: LibraryCmd, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match command {
            LibraryCmd::FocusGrid { row, col } => {
                if col >= COLS { return Handled::No; }
                let Some(index) = row.checked_mul(COLS).and_then(|i| i.checked_add(col)) else { return Handled::No };
                let Some(elem) = self.pair.detail.elem_at(index) else { return Handled::No };
                self.reseat(FocusTarget::Elem(self.key(elem)), fx);
            }
            LibraryCmd::FocusToolbar => self.reseat(FocusTarget::ContainerGroup(TOOLBAR_GROUP), fx),
            LibraryCmd::ItemMenu => return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, true, cx, fx)),
            LibraryCmd::Page(direction) => {
                let Some((row, col)) = self.grid_position(cx.focus.current) else { return Handled::No };
                let rows = self.pair.detail.elems.len().div_ceil(COLS);
                let next = row.saturating_add_signed(direction.signum() as isize * 2).min(rows.saturating_sub(1));
                if let Some(elem) = self.pair.detail.elem_at((next * COLS + col).min(self.pair.detail.elems.len().saturating_sub(1))) {
                    self.reseat(FocusTarget::Elem(self.key(elem)), fx);
                }
            }
            LibraryCmd::Sweep => {
                let (row, col) = self.grid_position(cx.focus.current).unwrap_or((0, 0));
                return self.command(LibraryCmd::FocusGrid { row: (row + 1) % self.pair.detail.elems.len().div_ceil(COLS).max(1), col }, cx, fx);
            }
            LibraryCmd::SwitchStep(step) => {
                if step.is_multiple_of(4) { self.command(LibraryCmd::FocusToolbar, cx, fx); }
                if let Some(target) = GridTarget::from_view(H::listing(cx)) {
                    self.pending.request_grid(target, GridAction::Unwatched { desired: step % 2 == 0 });
                    self.grid_fade.reload();
                }
            }
        }
        Handled::Yes
    }
}

impl<H: LibraryLike> Machine<H> for LibraryScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) => { self.sync(cx); }
            ScreenEvent::RestoreMemory(PageMemory::Library(memory)) => { self.restore(memory); self.sync(cx); }
            ScreenEvent::Enter(_) => { self.live = true; self.sync(cx); }
            ScreenEvent::Cover => { self.live = false; }
            ScreenEvent::Uncover => { self.live = true; self.sync(cx); }
            ScreenEvent::WillLeave(_) => { self.flush(fx); }
            ScreenEvent::App(AppMsg::Library(command)) => return self.command(*command, cx, fx),
            ScreenEvent::App(AppMsg::LibraryEdit { target, edit }) => {
                use crate::stores::browse::QueryEdit;
                if Some(target.epoch) != self.epoch { return Handled::No; }
                let target = self.pending.section().map(|section| SectionAddress {
                    epoch: section.epoch, sid: section.identity.sid, section: section.identity.key,
                }).unwrap_or(*target);
                let action = match edit {
                    QueryEdit::Sort { key, desc } => GridAction::Sort { key: key.clone(), desc: *desc },
                    QueryEdit::Unwatched(desired) => GridAction::Unwatched { desired: *desired },
                    QueryEdit::Genre(id) => GridAction::Genre { id: id.clone() },
                };
                self.pending.request_grid(GridTarget { epoch: target.epoch, sid: target.sid, section: target.section, query: self.query.unwrap_or(0) }, action);
                self.grid_fade.reload();
            }
            ScreenEvent::App(AppMsg::LibrarySelect(target)) => {
                let directory = H::directory(cx);
                if directory.epoch() == Some(target.epoch) {
                    if let Some(index) = directory.sections().iter().position(|section| section.sid == Some(target.sid) && section.key == target.section) {
                        self.pending.request_section(SectionTarget { epoch: target.epoch, index, identity: LibrarySectionIdentity { sid: target.sid, key: target.section }, kind: directory.sections()[index].kind });
                        self.page_fade.reload();
                    }
                }
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let from_group = from.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                let to_group = <Self as Focusable<H>>::group_of(self, &to.elem, cx);
                let outcome = self.pair.focus_moved(from_group, to_group, *to);
                // Projected entry names the letter but preserves the exact remembered grid item.
                // Only a move within the rail (or direct pointer entry) jumps to its first title.
                if matches!(outcome, MasterOutcome::Follow(_))
                    && (from_group == Some(RAIL_GROUP) || *by == By::Pointer) {
                    self.follow(to.elem, fx);
                }
                self.reveal(*to, *by, cx);
            }
            ScreenEvent::Tick(tick) => {
                self.sync(cx);
                // Compact menus leave their host's transaction clock live while owning input.
                let ready = self.readout != Readout::Loading;
                let page_commit = self.page_fade.tick(tick.dt(), ready);
                let grid_commit = self.grid_fade.tick(tick.dt(), ready);
                if page_commit || (grid_commit && self.pending.section().is_none()) { self.flush(fx); }
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    for row in &mut self.shelves {
                        let col = focused.and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
                        row.motion.update(row.elems.len(), col, if row.landscape { &RowStyle::EPISODE } else { &RowStyle::HOME }, dt);
                    }
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    self.relayout(focused);
                    if self.initial && self.layout.first().is_some() {
                        self.initial = false;
                        let (index, scroll) = H::listing(cx).saved_view();
                        self.scroll.jump(scroll);
                        self.scroll_target = scroll;
                        let focus = match self.layout.seat_for_scroll(scroll, index / COLS) {
                            Some(Block::Grid(row)) => self.pair.detail.elem_at(row * COLS + index % COLS)
                                .map(|elem| FocusTarget::Elem(self.key(elem))),
                            Some(Block::Shelf(row)) => self.shelves.get(row).map(|row| FocusTarget::ContainerGroup(row.group)),
                            _ => None,
                        }.unwrap_or(FocusTarget::ContainerGroup(self.first_group()));
                        self.reseat(focus, fx);
                    }
                    if let Some(target) = self.address(cx) {
                        let (lo, hi) = self.layout.visible_rows(self.scroll.pos);
                        self.store(target, LibraryWork::Want { lo: lo.saturating_sub(1) * COLS, hi: (hi + 1) * COLS }, fx);
                        self.store(target, LibraryWork::Letters, fx);
                        let at_head = self.scroll.pos.abs() < 0.5 && self.scroll_target == 0.0
                            && focused.is_none_or(|key| region_of_elem(key.elem) != Some(KeyRegion::Grid));
                        self.store(target, LibraryWork::Hubs { may_publish: at_head && !self.grid_fade.is_swapping() }, fx);
                    }
                    fx.push(Fx::App(AppFx::StoreWork(StoreWork::Browse)));
                }
            }
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressHold(_) => {
                return self.command(LibraryCmd::ItemMenu, cx, fx);
            }
            ScreenEvent::PressCommit(_) => {
                return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, false, cx, fx));
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key { sym, wcode, edge: Edge::Down, .. } = input.kind {
                    if let Some(dir) = crate::ui::consts::page_dir(sym, wcode) {
                        return self.command(LibraryCmd::Page(dir), cx, fx);
                    }
                }
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    let cancelled = self.page_fade.cancel() | self.grid_fade.cancel();
                    if cancelled { self.pending.cancel(); return Handled::Yes; }
                    if self.scroll_target > 0.5 {
                        self.scroll_target = 0.0;
                        self.reseat(FocusTarget::ContainerGroup(self.first_group()), fx);
                    } else { fx.push(Fx::App(AppFx::Library(LibraryReq::BackToHome { kind: self.kind }))); }
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: LibraryLike> Focusable<H> for LibraryScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if !self.libraries.is_empty() {
            out.push(row_group(LIBRARY_GROUP, self.libraries.len(), Rect::new(MARGIN_X, CONTENT_TOP - self.scroll.pos, layout::GRID_RIGHT - MARGIN_X, 52.0), ElemKind::Control));
        }
        for (index, row) in self.shelves.iter().enumerate() {
            out.push(row_group(row.group, row.elems.len(), Rect::new(MARGIN_X, self.target_layout.shelf_y(index, self.scroll_target) + CARD_DY, SCR_W - 2.0 * MARGIN_X, row_style(row).h), ElemKind::Card));
        }
        if self.layout.grid_head {
            out.push(row_group(TOOLBAR_GROUP, 2, self.toolbar_rect(), ElemKind::Control));
        }
        if self.readout == Readout::Failed {
            out.push(row_group(STATUS_GROUP, 1, self.status_rect(), ElemKind::Control));
        }
        if !self.pair.detail.elems.is_empty() && !self.pair.master.elems.is_empty() {
            self.pair.groups(cx, out);
        } else {
            self.pair.detail.groups(cx, out);
        }
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        if self.libraries.iter().any(|(key, _)| key == elem) { return Some(LIBRARY_GROUP); }
        if let Some(row) = self.shelves.iter().find(|row| row.elems.contains(elem)) { return Some(row.group); }
        if self.layout.grid_head && [SORT, FILTER].contains(elem) { return Some(TOOLBAR_GROUP); }
        if self.readout == Readout::Failed && *elem == RETRY { return Some(STATUS_GROUP); }
        self.pair.group_of(elem, cx)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if matches!(region_of_elem(key.elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.neighbour(key, dir, cx);
        }
        let elems = self.row_elems(self.group_of(&key.elem, cx));
        let Some(index) = elems.iter().position(|elem| *elem == key.elem) else { return Step::Edge };
        let next = match dir { Dir::Left => index.checked_sub(1), Dir::Right => Some(index + 1), _ => None };
        next.and_then(|i| elems.get(i).copied()).map_or(Step::Edge, |elem| Step::Move(self.key(elem)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if matches!(region_of_elem(*elem), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            return self.pair.place(elem, cx, at);
        }
        let rect = if let Some(index) = self.libraries.iter().position(|(key, _)| key == elem) {
            self.library_rect(index, cx)
        } else if let Some((row, col)) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|key| key == elem).map(|col| (row, col))) {
            self.shelf_rect_at(row, col, cx, at)
        } else if *elem == SORT && self.layout.grid_head { self.toolbar_rect() }
        else if *elem == FILTER && self.layout.grid_head { { let r = self.toolbar_rect(); Rect::new(r.x + 280.0, r.y, r.w, r.h) } }
        else if *elem == RETRY && self.readout == Readout::Failed { self.status_rect() }
        else { return None };
        Some(Placed { rect, rest_rect: rect, clip: Rect::new(0.0, crate::ui::widgets::TOP_BAR_BOTTOM, SCR_W, SCR_H - crate::ui::widgets::TOP_BAR_BOTTOM), index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if matches!(self.keys.region(want.elem).or_else(|| region_of_elem(want.elem)), Some(KeyRegion::Grid | KeyRegion::Rail)) {
            // Ownership is a typed identity query, even when group_of no longer places the key.
            let next = self.pair.reconcile(want, cx);
            if self.pair.place(&next.elem, cx, At::SpringTarget).is_some() { return next; }
        }
        if self.place(&want.elem, cx, At::SpringTarget).is_some() { return want; }
        if let Some((group, index)) = self.keys.last_place(want.elem) {
            let elems = self.row_elems(Some(group));
            if let Some(elem) = elems.get(index.min(elems.len().saturating_sub(1))) { return self.key(*elem); }
        }
        let group = self.first_group();
        self.row_elems(Some(group)).first().copied().map(|elem| self.key(elem))
            .unwrap_or_else(|| self.key(crate::ui::dispatch::STRIP_BASE))
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if group == GRID_GROUP || group == RAIL_GROUP { return self.pair.seat(group, from, cx); }
        let elems = self.row_elems(Some(group));
        let elem = elems.iter().filter_map(|elem| self.place(elem, cx, At::SpringTarget)
            .map(|placed| (*elem, (placed.rect.cx() - from.rect.cx()).abs())))
            .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(elem, _)| elem)
            .unwrap_or(crate::ui::dispatch::STRIP_BASE);
        self.key(elem)
    }
}

impl LibraryScreen {
    fn row_elems(&self, group: Option<GroupId>) -> Vec<u32> {
        match group {
            Some(LIBRARY_GROUP) => self.libraries.iter().map(|(elem, _)| *elem).collect(),
            Some(TOOLBAR_GROUP) => vec![SORT, FILTER],
            Some(STATUS_GROUP) => vec![RETRY],
            Some(group) => self.shelves.iter().find(|row| row.group == group).map(|row| row.elems.clone()).unwrap_or_default(),
            None => Vec::new(),
        }
    }
    fn toolbar_rect(&self) -> Rect {
        Rect::new(MARGIN_X, CONTENT_TOP + self.layout.grid_block_top() + crate::ui::consts::TITLE_DY + CARD_DY - self.scroll.pos, 250.0, 52.0)
    }
    fn status_rect(&self) -> Rect { Rect::new(SCR_W * 0.5 - 110.0, 650.0, 220.0, 52.0) }
    fn shelf_rect(&self, index: usize, col: usize) -> Rect {
        let row = &self.shelves[index];
        let style = row_style(row);
        crate::ui::card_row::tile_rect(col, MARGIN_X, style.w + style.gap, row.motion.scroll_x(),
            self.layout.shelf_y(index, self.scroll.pos) + CARD_DY, (style.w, style.h)).scaled(row.motion.scale(col))
    }
    fn shelf_rect_at<H: LibraryLike>(&self, index: usize, col: usize, cx: &Cx<'_, H>, at: At) -> Rect {
        if at == At::Drawn {
            let rect = self.shelf_rect(index, col);
            return if cx.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == self.shelves[index].elems[col])
                && cx.press.scale > 0.0 { rect.scaled(cx.press.scale) } else { rect };
        }
        let row = &self.shelves[index];
        let style = row_style(row);
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
        let x = focused.map(|col| crate::ui::card_row::scroll_into_view(row.motion.scroll_x(), col,
            row.elems.len(), style.w, style.gap, SCR_W - 2.0 * MARGIN_X)).unwrap_or(row.motion.scroll_x());
        crate::ui::card_row::tile_rect(col, MARGIN_X, style.w + style.gap, x,
            self.target_layout.shelf_y(index, self.scroll_target) + CARD_DY, (style.w, style.h))
            .scaled(if focused == Some(col) { style.focus_scale } else { 1.0 })
    }
    fn library_rect<H: LibraryLike>(&self, index: usize, cx: &Cx<'_, H>) -> Rect {
        let directory = H::directory(cx);
        let widths = self.libraries.iter().map(|(_, section)| {
            let title = directory.sections().get(*section).map(|s| s.row.title.as_str()).unwrap_or("More");
            cx.measure.width(&std::ffi::CString::new(title).unwrap_or_default(), crate::ui::theme::size::BODY, false) + 48.0
        }).collect::<Vec<_>>();
        Rect::new(MARGIN_X + widths.iter().take(index).map(|width| width + 16.0).sum::<f32>(),
            CONTENT_TOP - self.scroll.pos, widths.get(index).copied().unwrap_or(100.0), 52.0)
    }
}
fn row_style(row: &Shelf) -> &'static RowStyle { if row.landscape { &RowStyle::EPISODE } else { &RowStyle::HOME } }
fn row_group(id: GroupId, len: usize, extent: Rect, elem: ElemKind) -> GroupSpec {
    GroupSpec { id, kind: GroupKind::Row { wrap: false }, seat: Seat::Remembered,
        reachable: AxisMask::VERTICAL, edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop], extent, len, elem }
}

impl<H: LibraryLike> Screen<H> for LibraryScreen {
    fn name(&self) -> &'static str { "library" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) { self.pair.prepare(b, cx); }
    fn draw(&mut self, f: &mut DrawFrame<'_, H>) { self.draw_page(f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory {
        PageMemory::Library(self.keys.remember(self.section.clone(), self.scroll.pos,
            self.shelves.iter().map(|row| (row.id.clone(), row.motion.scroll_x())).collect()))
    }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link { from: STRIP, dir: Dir::Down, to: self.first_group() });
        self.pair.links(out);
        out.push(Link { from: TOOLBAR_GROUP, dir: Dir::Right, to: RAIL_GROUP });
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
}

impl LogicalState for LibraryScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(match self.kind { SecKind::Movie => 0, SecKind::Show => 1 });
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.scroll_target);
        c.option(self.restore_scroll, |c, value| { c.f32(value); });
        c.bool(self.live).bool(self.initial);
        c.option(self.epoch, |c, value| { c.u32(value); });
        c.option(self.query, |c, value| { c.u32(value); });
        c.option(self.shelf_publication, |c, (id, revision)| {
            c.u32(id.epoch).u32(u32::from(id.sid.raw())).u64(id.section as u64).u64(revision);
        });
        self.page_fade.write(c);
        self.grid_fade.write(c);
        self.pair.state().write(c);
        self.pending.write(c);
        PageMemory::Library(self.keys.remember(self.section.clone(), self.scroll.pos, Vec::new())).write(c);
        c.seq(self.shelves.len());
        for row in &self.shelves { c.str(&row.id).u32(row.group.0); row.motion.write_motion(c); }
    }
    fn probe(&self, out: &mut String) { out.push_str("library"); }
}
