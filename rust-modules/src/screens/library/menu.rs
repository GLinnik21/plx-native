//! A registered Library menu surface. Navigation owns its lifetime, phase, and input scope.
use std::borrow::Cow;
use crate::screens::registry::{AppFx, AppMsg, LibraryLike, LibraryMenuArg, LibraryMenuKind};
use crate::stores::browse::{BrowseCmd, LibraryWork, QueryEdit, SectionAddress};
use crate::stores::{StoreCmd, StoreId};
use crate::ui::frame::Budget;
use crate::ui::machine::{Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, MachineId, NavOp};
use crate::ui::screen::{Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec, Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassState};
use crate::ui::Rect;

// The compact Library menu's existing corner geometry.
const PANEL_RADIUS: f32 = 20.0;

#[derive(Clone)]
enum Action { Edit(QueryEdit), Genre, Select(SectionAddress), Recheck }

pub(crate) struct LibraryMenu {
    entry: EntryId,
    arg: LibraryMenuArg,
    kind: LibraryMenuKind,
    rows: Vec<(u32, String, Action)>,
    identities: Vec<String>,
    table: TableView,
    stamp: String,
    glass: GlassState,
}

impl LibraryMenu {
    pub(crate) fn new(entry: EntryId, arg: LibraryMenuArg) -> Self {
        Self { entry, kind: arg.kind, arg, rows: Vec::new(), identities: Vec::new(),
            table: TableView::new(), stamp: String::new(), glass: GlassState::new() }
    }
    fn frame(&self) -> Rect {
        let [x, y, _, h] = self.arg.anchor.map(f32::from_bits);
        let height = self.table.measured_height().clamp(120.0, 740.0);
        Rect::new(x.clamp(96.0, 1174.0), (y + h + 16.0).clamp(96.0, 984.0 - height), 650.0, height)
    }
    fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let mut entries = Vec::new();
        let mut sections = Vec::new();
        let mut selected = 0;
        let title = match self.kind {
            LibraryMenuKind::Sort => "Sort by",
            LibraryMenuKind::Filter => "Filter",
            LibraryMenuKind::Genre => "Genre",
            LibraryMenuKind::Sources => "Libraries",
        };
        let mut section = Section::new(title);
        match self.kind {
            LibraryMenuKind::Sort => {
                for (i, sort) in listing.sorts().iter().enumerate() {
                    let active = i == listing.sort_index();
                    if active { selected = i; }
                    let desc = if active { !listing.sort_desc() } else { sort.default_desc };
                    let mut row = Row::new(&sort.title).checked(active);
                    if active { row = row.ticon(if listing.sort_desc() { crate::ui::icons::Icon::ChevronDown } else { crate::ui::icons::Icon::ChevronUp }); }
                    section = section.row(row);
                    entries.push((format!("sort:{}", sort.key), Action::Edit(QueryEdit::Sort { key: sort.key.clone(), desc })));
                }
            }
            LibraryMenuKind::Filter => {
                section = section.row(Row::new("Unwatched only").toggle(listing.unwatched()))
                    .row(Row::new("Genre").value(listing.genre().map(|g| g.title.as_str()).unwrap_or("All")).chevron(true));
                entries.push(("unwatched".into(), Action::Edit(QueryEdit::Unwatched(!listing.unwatched()))));
                entries.push(("genre".into(), Action::Genre));
            }
            LibraryMenuKind::Genre => {
                section = section.row(Row::new("All Genres").checked(listing.genre().is_none()));
                entries.push(("genre:all".into(), Action::Edit(QueryEdit::Genre(None))));
                for (i, genre) in listing.genres().iter().enumerate() {
                    let active = listing.genre().is_some_and(|current| current.id == genre.id);
                    if active { selected = i + 1; }
                    section = section.row(Row::new(&genre.title).checked(active));
                    entries.push((format!("genre:{}", genre.id), Action::Edit(QueryEdit::Genre(Some(genre.id.clone())))));
                }
            }
            LibraryMenuKind::Sources => {
                let directory = H::directory(cx);
                if let Some(current) = directory.current() {
                    for (_, candidate) in directory.favorite_sections_for(current) {
                        let Some(sid) = candidate.sid else { continue };
                        section = section.row(Row::new(&candidate.row.title).checked(candidate.row.current)
                            .detail(&candidate.row.count_line));
                        entries.push((format!("section:{}:{}", sid.raw(), candidate.key),
                            Action::Select(SectionAddress { epoch: directory.epoch().unwrap_or(0), sid, section: candidate.key })));
                    }
                }
                section = section.row(Row::new("Check for new shares"));
                entries.push(("recheck".into(), Action::Recheck));
            }
        }
        let stamp = format!("{:?}:{}:{}:{:?}", self.kind, listing.sort_index(), listing.unwatched(), entries.iter().map(|(key, _)| key).collect::<Vec<_>>());
        if self.stamp == stamp { return; }
        self.stamp = stamp;
        self.rows.clear();
        for (identity, action) in entries {
            let key = if let Some(i) = self.identities.iter().position(|old| old == &identity) { i }
                else { self.identities.push(identity.clone()); self.identities.len() - 1 };
            self.rows.push((key as u32, identity, action));
        }
        sections.push(section);
        self.table.compact = true;
        self.table.set_sections(sections, selected as i32, false);
    }
    fn activate<H: LibraryLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(action) = self.rows.iter().find(|(key, _, _)| *key == elem).map(|(_, _, action)| action.clone()) else { return };
        match action {
            Action::Genre => { self.kind = LibraryMenuKind::Genre; self.stamp.clear(); self.refresh(cx); }
            Action::Edit(edit) => {
                let close = !matches!(edit, QueryEdit::Unwatched(_));
                fx.push(Fx::Deliver(MachineId::Instance(self.arg.host),
                    Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit { target: self.arg.target, edit }))));
                if close { fx.push(Fx::Nav(NavOp::Dismiss(self.entry))); }
            }
            Action::Select(target) => {
                fx.push(Fx::Deliver(MachineId::Instance(self.arg.host),
                    Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(target)))));
                fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
            }
            Action::Recheck => fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RecheckShares)))),
        }
    }
}
impl<H: LibraryLike> Machine<H> for LibraryMenu {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount | ScreenEvent::StoreChanged(..) | ScreenEvent::Enter(_) => self.refresh(cx),
            ScreenEvent::Tick(tick) => {
                self.refresh(cx);
                if self.kind == LibraryMenuKind::Genre {
                    fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
                        target: self.arg.target, work: LibraryWork::Genres,
                    }))));
                }
                self.table.sel = cx.focus.current.and_then(|focus| self.rows.iter().position(|(key, _, _)| *key == focus.elem)).map_or(-1, |i| i as i32);
                self.table.update(tick.dt(), self.frame().h);
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.table.sel = self.rows.iter().position(|(key, _, _)| *key == to.elem).map_or(-1, |i| i as i32);
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, cx, fx),
            ScreenEvent::PressCommit(_) => if let Some(key) = cx.focus.current { self.activate(key.elem, cx, fx); },
            ScreenEvent::Input(input) => if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                if self.kind == LibraryMenuKind::Genre { self.kind = LibraryMenuKind::Filter; self.stamp.clear(); self.refresh(cx); }
                else { fx.push(Fx::Nav(NavOp::Dismiss(self.entry))); }
                return Handled::Yes;
            },
            _ => {}
        }
        Handled::No
    }
}
impl<H: LibraryLike> Focusable<H> for LibraryMenu {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec { id: GroupId(0), kind: GroupKind::Column, seat: Seat::Remembered,
            reachable: AxisMask::BOTH, edge: [EdgeRule::Stop; 4], extent: self.frame(), len: self.rows.len(), elem: ElemKind::Bare });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> { self.rows.iter().any(|(key, _, _)| key == elem).then_some(GroupId(0)) }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.rows.iter().position(|(elem, _, _)| *elem == key.elem) else { return Step::Edge };
        let next = match dir { Dir::Up => index.checked_sub(1), Dir::Down => Some(index + 1), _ => None };
        next.and_then(|i| self.rows.get(i)).map_or(Step::Edge, |(elem, _, _)| Step::Move(FocusKey { entry: self.entry, elem: *elem }))
    }
    fn place(&self, elem: &u32, _: &Cx<'_, H>, _: At) -> Option<Placed> {
        let index = self.rows.iter().position(|(key, _, _)| key == elem)?;
        let rect = self.table.row_frame(self.frame(), index as i32)?;
        Some(Placed { rect, rest_rect: rect, clip: self.frame(), index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() { want }
        else { FocusKey { entry: self.entry, elem: self.rows.first().map_or(0, |row| row.0) } }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: self.rows.get(self.table.sel.max(0) as usize).map_or(0, |row| row.0) }
    }
}
impl<H: LibraryLike> Screen<H> for LibraryMenu {
    fn name(&self) -> &'static str { "library_menu" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) { Glass::CACHED.prepare(&mut self.glass, false); }
    fn draw(&mut self, f: &mut DrawFrame<'_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        Glass::CACHED.panel(p, self.frame(), 0.0, PANEL_RADIUS);
        self.table.draw(p, self.frame());
        for (elem, _, _) in &self.rows {
            if let Some(placed) = <Self as Focusable<H>>::place(self, elem, f.cx, At::Drawn) {
                f.stop(p, Stop { key: FocusKey { entry: self.entry, elem: *elem },
                    rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
                    hover: Hover::Focus, activate: Activate::Immediate });
            }
        }
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
}
impl LogicalState for LibraryMenu {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c); c.u32(self.kind as u32);
        c.seq(self.identities.len());
        for identity in &self.identities { c.str(identity); }
    }
    fn probe(&self, out: &mut String) { out.push_str("library_menu"); }
}
