//! Library instance: document motion and transactions belong here; focus belongs to Input.
mod identity;
mod bookmark;
mod layout;
mod parts;
mod rail;
#[cfg(test)]
mod rail_tests;
mod transactions;
mod draw;
mod toolbar;
mod status;
pub(crate) mod menu;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod window_tests;
#[cfg(test)]
mod labels_tests;
#[cfg(test)]
mod deferred_tests;
#[cfg(test)]
mod navigation_tests;
#[cfg(test)]
mod shelf_action_tests;

use std::borrow::Cow;
use crate::browse::{SecFetch, SecKind};
use crate::screens::registry::{
    AppFx, AppMsg, HomeTab, LibraryCmd, LibraryIdentity, LibraryLike, LibraryMemory,
    LibraryReq, LibrarySectionIdentity, LibraryViewport, PageMemory,
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

pub(crate) const SHAPE: [&str; 8] = [
    "LibraryScreen{entry:u32,instance:u32,kind:u32,wanted_kind:Option<u32>,scroll:{pos:f32,vel:f32},scroll_target:f32,restore_scroll:Option<f32>,live:bool,initial:bool,sweep_down:bool,epoch:Option<u32>,query:Option<u32>,grid_reset_pending:bool,shelf_publication:Option<(HubsId{epoch:u32,sid:u32,section:u64},revision:u64)>,page_fade:Xfade{phase:u8,t:f32},grid_fade:Xfade{phase:u8,t:f32},pair:MasterDetailState{side:u32,follow:u32,band:u32,door:Option<u32>},pending:PendingTransactions,ground_seeded:bool,ground:PageGround,chrome:LibraryChrome,memory:PageMemory::Library,viewport_cache:[LibraryViewport],shelves:[{id:str,group:u32,landscape:bool,elems:[u32],motion:CardRow}],libraries:[(elem:u32,section:u32)],readout:u32,layout:LibraryLayout,target_layout:LibraryLayout,grid:LibraryGrid,rail:LibraryRail}",
    transactions::SHAPE,
    crate::ui::widgets::PageGround::SHAPE,
    crate::ui::widgets::TabStrip::SHAPE,
    "LibraryChrome{capsules:TabStrip,pop:CtlPop{sp:[Spring{pos:f32,vel:f32}],focused:Option<u32>},pair_groups:{master:u32,detail:u32}}",
    Layout::SHAPE,
    GridPart::SHAPE,
    RailPart::SHAPE,
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
    wanted_kind: Option<SecKind>,
    keys: KeyRegistry,
    pair: MasterDetail<RailPart, GridPart, Regions>,
    libraries: Vec<(u32, usize)>,
    shelves: Vec<Shelf>,
    section: Option<LibrarySectionIdentity>,
    epoch: Option<u32>,
    query: Option<u32>,
    grid_reset_pending: bool,
    shelf_publication: Option<(crate::browse::section_hubs::HubsId, u64)>,
    layout: Layout,
    target_layout: Layout,
    scroll: Spring,
    scroll_target: f32,
    restore_scroll: Option<f32>,
    viewports: Vec<LibraryViewport>,
    pending: PendingTransactions,
    page_fade: Xfade,
    grid_fade: Xfade,
    readout: Readout,
    live: bool,
    initial: bool,
    sweep_down: bool,
    // Paint-only state: neither capsule travel nor ambient colours choose focus or activation.
    library_capsules: crate::ui::widgets::TabStrip,
    library_pop: crate::ui::widgets::CtlPop<1>,
    ground: crate::ui::widgets::PageGround,
    ground_seeded: bool,
}

impl LibraryScreen {
    pub(crate) fn new(entry: EntryId, instance: InstanceId, kind: SecKind) -> Self {
        let layout = Layout::new(false, &[], 0, false);
        Self {
            entry, instance, kind, wanted_kind: Some(kind), keys: KeyRegistry::default(),
            pair: MasterDetail::new(
                RailPart::new(entry, RAIL_GROUP), GridPart::new(entry, GRID_GROUP),
                MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL },
                MasterDetailGroups { master: RAIL_GROUP, detail: GRID_GROUP },
                MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
            ),
            libraries: Vec::new(), shelves: Vec::new(), section: None, epoch: None, query: None, grid_reset_pending: false,
            shelf_publication: None, layout, target_layout: layout,
            scroll: Spring::at(0.0), scroll_target: 0.0, restore_scroll: None,
            viewports: Vec::new(),
            pending: PendingTransactions::default(), page_fade: Xfade::new(), grid_fade: Xfade::new(),
            readout: Readout::Loading, live: true, initial: true, sweep_down: true,
            library_capsules: crate::ui::widgets::TabStrip::new(),
            library_pop: crate::ui::widgets::CtlPop::new(),
            ground: crate::ui::widgets::PageGround::new(), ground_seeded: false,
        }
    }

    pub(crate) fn restore(&mut self, memory: &LibraryMemory) {
        self.wanted_kind = None;
        self.keys = KeyRegistry::restore(memory);
        self.pair.detail.restore_keys(&self.keys);
        self.section = memory.section.clone();
        self.epoch = memory.epoch;
        self.query = memory.query;
        self.grid_reset_pending = memory.grid_reset_pending;
        if self.grid_reset_pending { self.grid_fade.mount(); }
        self.viewports = memory.viewports.clone();
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
        let epoch = listing.id().map(|id| id.epoch).or(directory.epoch());
        let changed = self.section != identity || self.epoch != epoch;
        if changed { self.grid_reset_pending = false; }
        else if self.query.is_some() && self.query != listing.id().map(|id| id.query) {
            self.grid_reset_pending = true;
            self.restore_scroll = None;
            if !self.grid_fade.is_swapping() { self.grid_fade.mount(); }
        }
        if let (Some(section), Some(epoch)) = (&identity, epoch) {
            let mut group = |kind: &str| GroupId(self.keys.register(LibraryIdentity::Control {
                section: section.clone(), kind: kind.into(), key: epoch.to_string(),
            }, GroupId(0), 0));
            let groups = MasterDetailGroups { master: group("rail-group"), detail: group("grid-group") };
            if self.pair.groups_config() != groups {
                self.pair = MasterDetail::new(
                    RailPart::new(self.entry, groups.master), GridPart::new(self.entry, groups.detail),
                    MasterDetailLayout { master: Rect::FULL, detail: Rect::FULL }, groups,
                    MasterDetailPolicy::new(MasterSide::Right, Follow::Live), Regions,
                );
                self.pair.detail.restore_keys(&self.keys);
            }
        }
        if changed {
            if self.section.is_some() {
                self.initial = true;
                if !self.page_fade.is_swapping() {
                    // This was not our page transaction: the outgoing publication is already gone.
                    self.page_fade.mount();
                    self.grid_fade = Xfade::new();
                    self.pending.cancel();
                }
            }
            if let Some(viewport) = self.current_viewport() {
                self.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
                self.viewports.push(viewport);
            }
            self.ground_seeded = false;
            self.shelves.clear();
            self.shelf_publication = None;
            self.section = identity.clone();
            self.epoch = epoch;
            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && Some(&view.section) == identity.as_ref());
            self.restore_scroll = saved.map(|view| view.scroll).or_else(||
                cx.focus.remembered(self.pair.groups_config().detail).is_none()
                    .then(|| listing.cursor().map(|cursor| cursor.scroll)).flatten());
            self.scroll.jump(self.restore_scroll.unwrap_or(0.0));
            self.scroll_target = self.scroll.pos;
        }
        self.query = listing.id().map(|id| id.query);
        let table = if directory.sections().is_empty() { directory.discovery() } else { directory.source_fetch() };
        self.readout = readout(table, directory.sections().len(), listing.fetch(), listing.total());
        self.libraries.clear();
        if let Some(current) = directory.current() {
            let kind = directory.sections()[current].kind;
            if self.wanted_kind.is_none() || self.wanted_kind == Some(kind) {
                self.kind = kind;
                self.wanted_kind = None;
            }
        }
        if let Some(current) = self.view_section(cx) {
            for (index, section) in directory.favorite_sections_for(current) {
                let Some(sid) = section.sid else { continue };
                let elem = self.keys.register(
                    LibraryIdentity::Library(LibrarySectionIdentity { sid, key: section.key }),
                    LIBRARY_GROUP, self.libraries.len());
                self.libraries.push((elem, index));
            }
            let widths: Vec<_> = self.library_lays(cx).iter().map(|lay|
                crate::ui::widgets::strip_pill_rect(lay, 0.0, crate::ui::widgets::StatusOverlay::CTRL_H).w).collect();
            let selected = self.libraries.iter().position(|(_, index)| *index == current).unwrap_or(0);
            let more = std::ffi::CString::new(format!("+{}", widths.len().saturating_sub(1))).unwrap_or_default();
            let more_w = cx.measure.width(&more, crate::ui::theme::size::BODY, true) + 2.0 * crate::ui::widgets::STRIP_PAD;
            let (start, len) = layout::library_window(&widths, selected, layout::GRID_RIGHT - MARGIN_X,
                crate::ui::widgets::STRIP_GAP_WIDE, more_w, layout::MAX_LIBRARY_PILLS);
            if len < self.libraries.len() {
                self.libraries = self.libraries[start..start + len].to_vec();
                self.libraries.push((MORE, usize::MAX));
            }
            if self.libraries.len() == 1 && !directory.sections()[current].borrowed
                && !(self.readout == Readout::Failed && directory.sources().len() > 1) {
                self.libraries.clear();
            }
        }
        if let Some(kind) = self.wanted_kind.filter(|kind|
            directory.current().map(|i| directory.sections()[i].kind) != Some(*kind)) {
            self.shelves.clear();
            self.shelf_publication = None;
            self.pair.detail.clear_projection();
            self.pair.master.clear_projection();
            self.grid_fade = Xfade::new();
            self.readout = if directory.preferred(kind).is_some() { Readout::Loading }
                else { readout(directory.kind_fetch(kind), 0, directory.kind_fetch(kind), -1) };
            self.relayout(cx.focus.current);
            return;
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
                        .map(|i| old.remove(i).motion).unwrap_or_else(|| {
                            let mut motion = CardRow::new();
                            let saved = self.viewports.iter().find(|view| Some(view.epoch) == epoch && view.section == *section)
                                .and_then(|view| view.shelves.iter().find(|(id, _)| *id == shelf.id));
                            if let Some((_, x)) = saved {
                                motion.restore_scroll(*x, shelf.items.len(), if shelf.landscape { &RowStyle::EPISODE } else { &RowStyle::HOME });
                            }
                            motion
                        });
                    self.shelves.push(Shelf { id: shelf.id.clone(), group, elems, landscape: shelf.landscape, motion });
                }
            }
            self.shelf_publication = publication;
        }
        self.pair.detail.refresh(cx, &mut self.keys);
        self.relayout(cx.focus.current);
        self.pair.master.refresh(cx, &mut self.keys);
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
        if self.grid_reset_pending {
            self.scroll_target = self.target_layout.row_reveal(0);
            self.scroll.jump(self.scroll_target);
        }
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

    fn current_viewport(&self) -> Option<LibraryViewport> {
        let shelves = if self.shelf_publication.is_some() {
            self.shelves.iter().map(|row| (row.id.clone(), row.motion.scroll_x())).collect()
        } else {
            self.viewports.iter().find(|view| Some(view.epoch) == self.epoch && Some(&view.section) == self.section.as_ref())
                .map(|view| view.shelves.clone()).unwrap_or_default()
        };
        Some(LibraryViewport { epoch: self.epoch?, section: self.section.clone()?, scroll: self.scroll.pos,
            shelves })
    }

    fn page_memory(&self) -> LibraryMemory {
        let mut memory = self.keys.remember(self.section.clone(), self.scroll.pos,
            self.shelves.iter().map(|row| (row.id.clone(), row.motion.scroll_x())).collect());
        memory.epoch = self.epoch;
        memory.query = self.query;
        memory.grid_reset_pending = self.grid_reset_pending;
        memory.viewports = self.viewports.clone();
        if let Some(viewport) = self.current_viewport() {
            memory.viewports.retain(|old| old.epoch != viewport.epoch || old.section != viewport.section);
            memory.viewports.push(viewport);
        }
        memory
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
        self.pair.master.refresh(cx, &mut self.keys);
    }

    fn follow<H: LibraryLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let Some(index) = self.pair.master.start_for_elem(elem) else { return };
        let Some(target) = self.pair.detail.elem_at(index) else { return };
        fx.remember(self.pair.groups_config().detail, target);
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

    pub(crate) fn probe_viewport(&self, focus: Option<FocusKey<u32>>) -> (&'static str, i32, i32, f32, f32) {
        if let Some((row, col)) = self.grid_position(focus) { return ("grid", row as i32, col as i32, 0.0, self.scroll.pos); }
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        for (row, shelf) in self.shelves.iter().enumerate() {
            if let Some(col) = elem.and_then(|elem| shelf.elems.iter().position(|key| *key == elem)) {
                return ("shelf", row as i32, col as i32, shelf.motion.scroll_x(), self.scroll.pos);
            }
        }
        let region = match elem {
            Some(SORT | FILTER) => "toolbar", Some(RETRY) => "status",
            Some(elem) if region_of_elem(elem) == Some(KeyRegion::Rail) => "rail",
            Some(elem) if elem >= crate::ui::dispatch::STRIP_BASE => "strip",
            Some(_) => "library", None => "none",
        };
        (region, -1, -1, 0.0, self.scroll.pos)
    }

    fn from_deck<H: LibraryLike>(&self, elem: u32, cx: &Cx<'_, H>) -> bool {
        self.shelves.iter().position(|row| row.elems.contains(&elem))
            .and_then(|row| H::section_hubs(cx).shelves().get(row)).is_some_and(|row| row.is_continue)
    }

    fn activate<H: LibraryLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let source_chip = self.source_chip(cx).is_some() && self.libraries.first().is_some_and(|(key, _)| *key == elem);
        if [SORT, FILTER, MORE].contains(&elem) || source_chip {
            if let (Some(target), Some(placed)) = (self.requested_address(cx), <Self as Focusable<H>>::place(self, &elem, cx, At::SpringTarget)) {
                let r = placed.rest_rect;
                fx.push(Fx::App(AppFx::Library(LibraryReq::Menu {
                    kind: if source_chip || elem == MORE { crate::screens::registry::LibraryMenuKind::Sources }
                        else if elem == SORT { crate::screens::registry::LibraryMenuKind::Sort }
                        else { crate::screens::registry::LibraryMenuKind::Filter },
                    anchor: [r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()], target,
                })));
            }
            return Handled::Yes;
        }
        if let Some((_, index)) = self.libraries.iter().find(|(key, _)| *key == elem) {
            let directory = H::directory(cx);
            let index = *index;
            if Some(index) == self.pending.section().map(|section| section.index).or(directory.current()) { return Handled::Yes; }
            if Some(index) == directory.current() && self.pending.section().is_some() {
                self.pending.cancel();
                self.page_fade.cancel();
                self.grid_fade.cancel();
                return Handled::Yes;
            }
            let section = &directory.sections()[index];
            if let (Some(epoch), Some(sid)) = (directory.epoch(), section.sid) {
                self.save_cursor(cx, fx);
                self.pending.request_section(SectionTarget {
                    epoch, index,
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
            if let Some(target) = self.address(cx) {
                self.store(target, LibraryWork::Retry, fx);
            } else {
                let directory = H::directory(cx);
                if let (Some(epoch), Some((sid, _))) = (directory.epoch(), directory.source()) {
                    fx.push(Fx::App(AppFx::Store(StoreId::Browse,
                        StoreCmd::Browse(BrowseCmd::RetrySource { epoch, sid: *sid }))));
                }
            }
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
            LibraryCmd::Enter(kind) => {
                self.save_cursor(cx, fx);
                self.flush(fx);
                self.wanted_kind = Some(kind);
                self.initial = true;
                self.page_fade.mount();
                self.sync(cx);
            }
            #[cfg(test)]
            LibraryCmd::FocusGrid { row, col } => {
                if col >= COLS { return Handled::No; }
                let Some(index) = row.checked_mul(COLS).and_then(|i| i.checked_add(col)) else { return Handled::No };
                let Some(elem) = self.pair.detail.elem_at(index) else { return Handled::No };
                self.initial = false; // An explicit owned command supersedes the pending boot seat.
                self.reseat(FocusTarget::Elem(self.key(elem)), fx);
            }
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
                let current = cx.focus.current.filter(|key| key.entry == self.entry);
                let group = current.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                if self.sweep_down && self.grid_position(current).is_some_and(|(row, _)|
                    row + 1 >= self.pair.detail.elems.len().div_ceil(COLS)) {
                    self.sweep_down = false;
                } else if !self.sweep_down && group == Some(self.first_group()) {
                    self.sweep_down = true;
                }
                fx.push(Fx::Deliver(MachineId::Instance(self.instance), Delivery::Screen(
                    ScreenEvent::Input(crate::ui::machine::InputEvent {
                        at: cx.tick, source: crate::ui::machine::Source::Script,
                        kind: InputKind::Key { key: if self.sweep_down { Key::Down } else { Key::Up },
                            sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
                    }))));
            }
            LibraryCmd::SwitchStep(step) => {
                match step % 14 {
                    0 if H::directory(cx).sections().iter().any(|section| section.kind == SecKind::Show) =>
                        return self.command(LibraryCmd::Enter(SecKind::Show), cx, fx),
                    1 => return self.command(LibraryCmd::Enter(SecKind::Movie), cx, fx),
                    2 => return self.activate(SORT, false, cx, fx),
                    5 | 6 => {
                        if let Some(target) = GridTarget::from_view(H::listing(cx)) {
                            let current = self.pending.grid().and_then(|(pending, action)| {
                                if pending != &target { return None; }
                                match action { GridAction::Unwatched { desired } => Some(*desired), _ => None }
                            }).unwrap_or(H::listing(cx).unwatched());
                            let address = SectionAddress { epoch: target.epoch, sid: target.sid, section: target.section };
                            return self.step(&ScreenEvent::App(AppMsg::LibraryEdit {
                                target: address, edit: crate::stores::browse::QueryEdit::Unwatched(!current),
                            }), cx, fx);
                        }
                    }
                    7 => return self.activate(FILTER, false, cx, fx),
                    12 | 13 => {
                        let letter = if step % 14 == 12 { self.pair.master.elems.last() } else { self.pair.master.elems.first() };
                        if let Some(elem) = letter { self.follow(*elem, fx); }
                    }
                    // Menu steps are delivered to their actual input-owning entry by the bridge.
                    _ => {}
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
            ScreenEvent::WillLeave(_) => { self.save_cursor(cx, fx); self.flush(fx); }
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
                if matches!(&action, GridAction::Unwatched { desired } if *desired == H::listing(cx).unwatched())
                    && self.pending.section().is_none() && self.address(cx) == Some(target)
                    && self.grid_fade.cancel() {
                    self.pending.cancel_grid();
                    return Handled::Yes;
                }
                self.pending.request_grid(GridTarget { epoch: target.epoch, sid: target.sid, section: target.section, query: self.query.unwrap_or(0) }, action);
                self.grid_fade.reload();
            }
            ScreenEvent::App(AppMsg::LibrarySelect(target)) => {
                let directory = H::directory(cx);
                if directory.epoch() == Some(target.epoch) {
                    if let Some(index) = directory.sections().iter().position(|section| section.sid == Some(target.sid) && section.key == target.section) {
                        self.save_cursor(cx, fx);
                        self.pending.request_section(SectionTarget { epoch: target.epoch, index, identity: LibrarySectionIdentity { sid: target.sid, key: target.section }, kind: directory.sections()[index].kind });
                        self.page_fade.reload();
                    }
                }
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                if matches!(by, By::Dir | By::Pointer) { self.initial = false; }
                let from_group = from.and_then(|key| <Self as Focusable<H>>::group_of(self, &key.elem, cx));
                let to_group = <Self as Focusable<H>>::group_of(self, &to.elem, cx);
                let outcome = self.pair.focus_moved(from_group, to_group, *to);
                // Projected entry names the letter but preserves the exact remembered grid item.
                // Only a move within the rail (or direct pointer entry) jumps to its first title.
                if matches!(outcome, MasterOutcome::Follow(_))
                    && (from_group == Some(self.pair.groups_config().master) || *by == By::Pointer) {
                    self.follow(to.elem, fx);
                }
                self.reveal(*to, *by, cx);
            }
            ScreenEvent::Tick(tick) => {
                self.sync(cx);
                if self.grid_reset_pending {
                    if let Some(elem) = self.pair.detail.elem_at(0) {
                        fx.remember(self.pair.groups_config().detail, elem);
                        self.grid_reset_pending = false;
                        if cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Grid)) {
                            self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                        }
                    } else if self.readout == Readout::Empty {
                        self.grid_reset_pending = false;
                    }
                }
                let current_grid = cx.focus.current.filter(|key| key.entry == self.entry)
                    .and_then(|key| self.pair.detail.index_of(key.elem));
                self.pair.master.advance(cx, current_grid, self.live && self.readout == Readout::Grid, tick.dt());
                if let Some(kind) = self.wanted_kind {
                    let directory = H::directory(cx);
                    if let Some(section) = directory.preferred(kind).and_then(|i| directory.sections().get(i)) {
                        if let (Some(sid), Some(epoch)) = (section.sid, directory.epoch()) {
                            self.store(SectionAddress { epoch, sid, section: section.key },
                                LibraryWork::Commit { select: true, choice: false, query: None }, fx);
                        }
                    } else {
                        self.readout = readout(directory.kind_fetch(kind), 0, directory.kind_fetch(kind), -1);
                    }
                }
                // Compact menus leave their host's transaction clock live while owning input.
                let waiting_for_kind = self.wanted_kind.is_some_and(|kind| {
                    let directory = H::directory(cx);
                    directory.preferred(kind).is_some() || directory.kind_fetch(kind) == SecFetch::Loading
                });
                let ready = self.readout != Readout::Loading && !waiting_for_kind;
                let page_commit = self.page_fade.tick(tick.dt(), ready);
                let grid_commit = self.grid_fade.tick(tick.dt(), ready);
                if page_commit || (grid_commit && self.pending.section().is_none()) { self.flush(fx); }
                if self.live {
                    let dt = tick.dt();
                    let focused = cx.focus.current;
                    let chosen = self.pending.section().map(|section| section.index).or(H::directory(cx).current());
                    let selected = self.libraries.iter().position(|(_, section)| Some(*section) == chosen).map_or(-1, |i| i as i32);
                    let focused_library = focused.filter(|key| key.entry == self.entry)
                        .and_then(|key| self.libraries.iter().position(|(elem, _)| *elem == key.elem)).map_or(-1, |i| i as i32);
                    let spans: Vec<_> = (0..self.libraries.len()).map(|i| {
                        let rect = self.library_rect(i, cx); (rect.x, rect.w)
                    }).collect();
                    self.library_capsules.update(selected, focused_library, |i| spans.get(i).copied(), crate::ui::widgets::SelMark::Travels, dt);
                    self.library_pop.step((focused_library >= 0).then_some(0), dt);
                    let colours = self.focused_item(focused, cx).filter(|item| item.has_blur).map(|item| item.blur)
                        .or_else(|| (!self.ground_seeded).then(|| H::listing(cx).item(0).filter(|item| item.has_blur).map(|item| item.blur)).flatten());
                    self.ground_seeded |= colours.is_some();
                    self.ground.key(colours, crate::ui::widgets::PageGround::CARD_W, dt);
                    for row in &mut self.shelves {
                        let col = focused.and_then(|key| row.elems.iter().position(|elem| *elem == key.elem));
                        row.motion.update(row.elems.len(), col, if row.landscape { &RowStyle::EPISODE } else { &RowStyle::HOME }, dt);
                    }
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    self.relayout(focused);
                    if self.initial && self.layout.first().is_some() && self.seed_cursor(cx, fx) {
                        self.initial = false;
                        let group = match self.layout.seat_for_scroll(self.scroll.pos, self.layout.visible_rows(self.scroll.pos).0) {
                            Some(Block::Grid(_)) => self.pair.groups_config().detail,
                            Some(Block::Shelf(index)) => self.shelves[index].group,
                            _ => self.first_group(),
                        };
                        if group == LIBRARY_GROUP {
                            if let Some((elem, _)) = self.libraries.iter().find(|(_, index)| Some(*index) == self.view_section(cx)) {
                                fx.remember(LIBRARY_GROUP, *elem);
                            }
                        }
                        self.reseat(FocusTarget::ContainerGroup(group), fx);
                    }
                }
                // The dispatcher ticks the top page beneath a compact menu, but not buried pages.
                // Cover pauses its control motion; it must not stop the query just committed above.
                if let Some(target) = self.address(cx).filter(|_| self.wanted_kind.is_none()) {
                    let (lo, hi) = self.layout.visible_rows(self.scroll.pos);
                    self.store(target, LibraryWork::Want { lo: lo.saturating_sub(1) * COLS, hi: (hi + 1) * COLS }, fx);
                    self.store(target, LibraryWork::Letters, fx);
                    let at_head = self.scroll.pos.abs() < 1.0 && self.scroll.vel.abs() < 1.0 && self.scroll_target.abs() < 0.5;
                    fx.push(Fx::App(AppFx::Library(LibraryReq::PublishShelves {
                        target, at_head, hidden_page: self.page_fade.is_swapping() && self.page_fade.alpha() <= 0.01,
                    })));
                }
                fx.push(Fx::App(AppFx::StoreWork(StoreWork::Browse)));
            }
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressHold(_) => {
                return self.command(LibraryCmd::ItemMenu, cx, fx);
            }
            ScreenEvent::PressCommit(_) => {
                return cx.focus.current.map_or(Handled::No, |key| self.activate(key.elem, false, cx, fx));
            }
            ScreenEvent::Input(input) => {
                if matches!(input.kind, InputKind::Key { key: Key::Ok, edge: Edge::Down, .. })
                    && cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Rail)) {
                    self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                    return Handled::Yes;
                }
                if let InputKind::Wheel { dy } = input.kind {
                    self.scroll_target = (self.scroll_target - dy * layout::GRID_PITCH)
                        .clamp(0.0, self.target_layout.max_scroll());
                    fx.invalidate(Provenance::Input);
                    return Handled::Yes;
                }
                if let InputKind::Key { sym, wcode, edge: Edge::Down, .. } = input.kind {
                    if let Some(dir) = crate::ui::consts::page_dir(sym, wcode) {
                        return self.command(LibraryCmd::Page(dir), cx, fx);
                    }
                }
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    let cancelled = self.page_fade.cancel() | self.grid_fade.cancel();
                    if cancelled { self.pending.cancel(); return Handled::Yes; }
                    if cx.focus.current.is_some_and(|key| key.entry == self.entry && region_of_elem(key.elem) == Some(KeyRegion::Rail)) {
                        self.reseat(FocusTarget::ContainerGroup(self.pair.groups_config().detail), fx);
                        return Handled::Yes;
                    }
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
            out.push(row_group(TOOLBAR_GROUP, 2, self.toolbar_chip_rect(SORT, cx, At::SpringTarget), ElemKind::Control));
        }
        if self.readout == Readout::Failed {
            if let Some(rect) = self.status_rect(cx) {
                out.push(GroupSpec { edge: [EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
                    ..row_group(STATUS_GROUP, 1, rect, ElemKind::Control) });
            }
        }
        if !self.pair.detail.elems.is_empty() && self.pair.master.eligible(cx) {
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
        let mut rest_rect = None;
        let rect = if let Some(index) = self.libraries.iter().position(|(key, _)| key == elem) {
            self.library_rect(index, cx)
        } else if let Some((row, col)) = self.shelves.iter().enumerate().find_map(|(row, shelf)|
            shelf.elems.iter().position(|key| key == elem).map(|col| (row, col))) {
            if at == At::Drawn { rest_rect = Some(self.shelf_rect(row, col)); }
            self.shelf_rect_at(row, col, cx, at)
        } else if matches!(*elem, SORT | FILTER) && self.layout.grid_head { self.toolbar_chip_rect(*elem, cx, at) }
        else if *elem == RETRY && self.readout == Readout::Failed { self.status_rect(cx)? }
        else { return None };
        Some(Placed { rect, rest_rect: rest_rect.unwrap_or(rect), clip: Rect::new(0.0, crate::ui::widgets::TOP_BAR_BOTTOM, SCR_W, SCR_H - crate::ui::widgets::TOP_BAR_BOTTOM), index: None })
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
        if group == self.pair.groups_config().detail || group == self.pair.groups_config().master { return self.pair.seat(group, from, cx); }
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
        let y = CONTENT_TOP - self.scroll.pos - self.shelves.first().map_or(0.0, |row| row.motion.lift());
        if let Some(chip) = self.source_chip(cx) {
            return Rect::new(MARGIN_X, y, chip.width(cx.measure), 52.0);
        }
        self.library_lays(cx).get(index).map(|lay|
            crate::ui::widgets::strip_pill_rect(lay, y, crate::ui::widgets::StatusOverlay::CTRL_H))
            .unwrap_or(Rect::new(MARGIN_X, y, 0.0, 0.0))
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
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) { self.draw_page(f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory {
        PageMemory::Library(self.page_memory())
    }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link { from: STRIP, dir: Dir::Down, to: self.first_group() });
        out.push(Link { from: self.first_group(), dir: Dir::Up, to: STRIP });
        // A continuous document has adjacent blocks even while its offscreen geometry
        // passes the floating strip. Geometric ranking must not skip an intervening block.
        let mut document = Vec::new();
        if !self.libraries.is_empty() { document.push(LIBRARY_GROUP); }
        document.extend(self.shelves.iter().map(|row| row.group));
        if self.layout.grid_head { document.push(TOOLBAR_GROUP); }
        if self.readout == Readout::Failed { document.push(STATUS_GROUP); }
        if !self.pair.detail.elems.is_empty() { document.push(self.pair.groups_config().detail); }
        for pair in document.windows(2) {
            out.push(Link { from: pair[0], dir: Dir::Down, to: pair[1] });
            out.push(Link { from: pair[1], dir: Dir::Up, to: pair[0] });
        }
        self.pair.links(out);
        out.push(Link { from: TOOLBAR_GROUP, dir: Dir::Right, to: self.pair.groups_config().master });
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
}

impl LogicalState for LibraryScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).u32(self.instance.0);
        c.u32(match self.kind { SecKind::Movie => 0, SecKind::Show => 1 });
        c.option(self.wanted_kind, |c, kind| { c.u32(match kind { SecKind::Movie => 0, SecKind::Show => 1 }); });
        c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.scroll_target);
        c.option(self.restore_scroll, |c, value| { c.f32(value); });
        c.bool(self.live).bool(self.initial).bool(self.sweep_down);
        c.option(self.epoch, |c, value| { c.u32(value); });
        c.option(self.query, |c, value| { c.u32(value); });
        c.bool(self.grid_reset_pending);
        c.option(self.shelf_publication, |c, (id, revision)| {
            c.u32(id.epoch).u32(u32::from(id.sid.raw())).u64(id.section as u64).u64(revision);
        });
        self.page_fade.write(c);
        self.grid_fade.write(c);
        self.pair.state().write(c);
        c.u32(self.pair.groups_config().master.0).u32(self.pair.groups_config().detail.0);
        self.pending.write(c);
        c.bool(self.ground_seeded);
        self.ground.write_motion(c);
        self.library_capsules.write_motion(c);
        self.library_pop.write_motion(c);
        PageMemory::Library(self.page_memory()).write(c);
        c.seq(self.viewports.len());
        for viewport in &self.viewports { viewport.write(c); }
        c.seq(self.shelves.len());
        for row in &self.shelves {
            let Shelf { id, group, elems, landscape, motion } = row;
            c.str(id).u32(group.0).bool(*landscape).seq(elems.len());
            for elem in elems { c.u32(*elem); }
            motion.write_motion(c);
        }
        c.seq(self.libraries.len());
        for (elem, section) in &self.libraries { c.u32(*elem).u32(*section as u32); }
        c.u32(match self.readout { Readout::Loading => 0, Readout::Empty => 1, Readout::Failed => 2, Readout::Grid => 3 });
        self.layout.write(c);
        self.target_layout.write(c);
        self.pair.detail.write(c);
        self.pair.master.write(c);
    }
    fn probe(&self, out: &mut String) { out.push_str("library"); }
}
