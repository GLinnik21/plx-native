//! Owned Search page. Input owns focus; this instance owns editing, motion and render caches.
//! Integration fixtures mount this implementation; live cutover still awaits native adapter wiring.
mod draft;
mod layout;
mod render;
mod memory;
pub(crate) use memory::Memory;

use std::borrow::Cow;
use crate::search::{Item, Kind};
use crate::screens::registry::{AppFx, HomeTab, PageMemory, SearchLike, SearchReq};
use crate::stores::{StoreCmd, StoreId};
use crate::stores::search::SearchCmd;
use crate::ui::card_row::CardRow;
use crate::ui::frame::Budget;
use crate::ui::machine::{Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId,
    Handled, InputKind, InputOwner, InstanceId, Key, LogicalState, Machine, MachineId, TextEdit};
use crate::ui::present::Provenance;
use crate::ui::screen::{At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget,
    Focusable, GroupKind, GroupSpec, Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step};
use crate::ui::{Rect, Spring};
use draft::Draft;

const FIELD: u32 = 1;
const CLEAR: u32 = 2;
const FIELD_GROUP: GroupId = GroupId(0x5345_4100);
const RECENTS_GROUP: GroupId = GroupId(0x5345_4101);
const CLEAR_GROUP: GroupId = GroupId(0x5345_4102);
const STRIP: GroupId = crate::ui::containers::tabs::STRIP;
const BLINK_MS: u32 = 530;
const BLINK_US: u32 = BLINK_MS * 1000;

pub(crate) const SHAPE: &str = "SearchScreen{entry:u32,instance:u32,draft:{text:str,caret:u64,profile:u32,pending:bool},mounted:bool,editing:bool,blink_us:u32,hot:Spring,scroll:Spring,scroll_target:f32,next_elem:u32,query_gen:u32,recent_clear_pending:bool,content_dirty:bool,fade:Xfade,ground:PageGround,owner_row:Option<u64>,owner:str,owner_alpha:Spring,restored:Option<SearchMemory>,keys:[SearchKey],recents:[u32],rows:[{kind:u32,group:u32,elems:[u32],motion:CardRow}]}";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Identity {
    Recent(String),
    Media(Kind, crate::plex::ServerId, String),
    Tag(Kind, crate::plex::ServerId, String),
    Slot(Kind, u32, usize),
}
#[derive(Clone, Debug)]
struct KeyEntry { identity: Identity, elem: u32, group: GroupId, slot: usize }
struct Row { kind: Kind, group: GroupId, elems: Vec<u32>, motion: CardRow }

pub(crate) struct SearchScreen {
    entry: EntryId,
    instance: InstanceId,
    draft: Draft,
    mounted: bool,
    editing: bool,
    blink_us: u32,
    hot: Spring,
    scroll: Spring,
    scroll_target: f32,
    keys: Vec<KeyEntry>,
    next_elem: u32,
    rows: Vec<Row>,
    recents: Vec<u32>,
    query_gen: u32,
    recent_clear_pending: bool,
    publication: Option<crate::search::view::SearchSnapshot>,
    content_dirty: bool,
    fade: crate::ui::xfade::Xfade,
    ground: crate::ui::widgets::PageGround,
    owner_row: Option<usize>,
    owner: String,
    owner_alpha: Spring,
    restored: Option<Memory>,
    render: render::Resources,
}

impl SearchScreen {
    pub(crate) fn new(entry: EntryId, instance: InstanceId) -> Self {
        Self { entry, instance, draft: Draft::new(0, ""), mounted: false, editing: false,
            blink_us: 0, hot: Spring::at(1.0), scroll: Spring::at(0.0), scroll_target: 0.0,
            keys: Vec::new(), next_elem: 10, rows: Vec::new(), recents: Vec::new(), query_gen: 0,
            recent_clear_pending: false, publication: None, content_dirty: true,
            fade: crate::ui::xfade::Xfade::new(), ground: crate::ui::widgets::PageGround::new(),
            owner_row: None, owner: String::new(), owner_alpha: Spring::at(0.0), restored: None, render: Default::default() }
    }
    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: self.entry, elem } }
    fn real_query(&self) -> bool { crate::search::terms(self.draft.query()).is_some() }
    fn field_hot<H: SearchLike>(&self, cx: &Cx<'_, H>) -> bool {
        self.editing || cx.focus.current == Some(self.key(FIELD))
    }
    fn intern(&mut self, identity: Identity, group: GroupId, slot: usize) -> u32 {
        if let Some(key) = self.keys.iter_mut().find(|key| key.identity == identity) {
            key.group = group; key.slot = slot;
            return key.elem;
        }
        let elem = self.next_elem;
        self.next_elem = self.next_elem.checked_add(1).expect("Search element space exhausted");
        self.keys.push(KeyEntry { identity, elem, group, slot });
        elem
    }
    fn store<H: SearchLike>(&self, command: SearchCmd, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(command))));
    }
    fn reseat<H: SearchLike>(&self, target: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Deliver(MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: target }))));
    }
    fn remember<H: SearchLike>(&self, fx: &mut Effects<'_, H>) {
        if self.real_query() {
            self.store(SearchCmd::RememberRecent { profile_generation: self.draft.profile(),
                term: self.draft.query().trim().into() }, fx);
        }
    }
    fn keyboard<H: SearchLike>(&mut self, up: bool, commit: bool, fx: &mut Effects<'_, H>) {
        if self.editing == up { return; }
        if !up && commit { self.remember(fx); }
        self.editing = up;
        self.blink_us = 0;
        if up { self.draft.to_end(); }
        fx.push(Fx::App(AppFx::Search(SearchReq::Keyboard { up })));
        fx.invalidate(Provenance::Input);
    }
    fn edit<H: SearchLike>(&mut self, edit: &TextEdit, fx: &mut Effects<'_, H>) {
        if let Some(query) = self.draft.edit(edit) {
            self.store(SearchCmd::SetQueryScoped { profile_generation: self.draft.profile(), query }, fx);
            self.rows.clear();
            self.scroll_target = 0.0;
            self.content_dirty = true;
            self.fade.reload();
        }
        self.blink_us = 0;
        fx.invalidate(Provenance::Input);
    }

    fn sync<H: SearchLike>(&mut self, cx: &Cx<'_, H>, notified: bool, fx: &mut Effects<'_, H>) {
        let view = H::search(cx);
        let pending = self.draft.pending();
        let profile = view.recents().generation();
        let candidate = self.restored.take();
        // Even a refused bookmark belongs to this entry's old element-id space. Never mint
        // those ids again for a different query and accidentally validate its carried focus.
        if let Some(memory) = &candidate { self.next_elem = self.next_elem.max(memory.next_elem); }
        let restored = candidate.filter(|memory| memory.profile == profile && memory.query == view.query_gen());
        if !self.mounted {
            self.draft = Draft::new(profile, view.query());
            self.mounted = true;
        } else if self.draft.observe(profile, view.query(), notified) {
            self.keyboard(false, false, fx);
            self.keys.clear();
            self.rows.clear(); self.recents.clear();
            self.scroll.jump(0.0); self.scroll_target = 0.0;
            self.recent_clear_pending = false;
            self.content_dirty = true;
        }
        if let Some(memory) = &restored {
            self.keys = memory.keys.clone(); self.next_elem = memory.next_elem;
            self.scroll.jump(memory.scroll); self.scroll_target = memory.scroll;
        }
        if notified && view.recents().terms().is_empty() && self.recent_clear_pending {
            self.recent_clear_pending = false;
            self.content_dirty = true;
        }
        if self.publication.as_ref().is_some_and(|old| view.same_publication(old))
            && !self.content_dirty && pending == self.draft.pending() { return; }
        self.publication = Some(view.snapshot());
        self.content_dirty = false;
        let query_changed = self.query_gen != view.query_gen();
        self.query_gen = view.query_gen();
        if query_changed && restored.is_none() {
            self.keys.retain(|key| matches!(key.identity, Identity::Recent(_)));
        }
        self.recents.clear();
        if !self.real_query() && !self.recent_clear_pending {
            for (slot, term) in view.recents().terms().iter().enumerate() {
                let elem = self.intern(Identity::Recent(term.clone()), RECENTS_GROUP, slot);
                self.recents.push(elem);
            }
        }
        if self.draft.pending() || !self.real_query() {
            self.rows.clear();
            return;
        }
        if query_changed {
            self.rows.clear();
            self.fade.reload();
        }
        let mut previous = std::mem::take(&mut self.rows);
        for shelf in view.shelves() {
            let group = layout::group(shelf.kind);
            let mut elems = Vec::with_capacity(shelf.items.len());
            for (slot, item) in shelf.items.iter().enumerate() {
                let identity = match item {
                    Item::Media(media) if !media.rk.is_empty() => Identity::Media(shelf.kind, media.sid, media.rk.clone()),
                    Item::Tag(tag) if !tag.tag_key.is_empty() || (!tag.id.is_empty() && tag.id != "0") =>
                        Identity::Tag(shelf.kind, tag.sid, if tag.tag_key.is_empty() { tag.id.clone() } else { tag.tag_key.clone() }),
                    _ => Identity::Slot(shelf.kind, view.query_gen(), slot),
                };
                elems.push(self.intern(identity, group, slot));
            }
            let motion = previous.iter().position(|row| row.kind == shelf.kind)
                .map(|i| previous.remove(i).motion).unwrap_or_else(CardRow::new);
            self.rows.push(Row { kind: shelf.kind, group, elems, motion });
        }
        if let Some(memory) = restored {
            for row in &mut self.rows {
                if let Some((_, x)) = memory.rows.iter().find(|(kind, _)| *kind == row.kind) {
                    row.motion.restore_scroll(*x, row.elems.len(), &layout::style(row.kind));
                }
            }
        }
    }

    fn activate<H: SearchLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if let Some(tab) = elem.checked_sub(crate::ui::dispatch::STRIP_BASE) {
            let request = match tab {
                0 => SearchReq::Tab(HomeTab::Home), 1 => SearchReq::Tab(HomeTab::Movies),
                2 => SearchReq::Tab(HomeTab::Shows), 3 => return Handled::Yes,
                4 => SearchReq::Account, _ => return Handled::No,
            };
            self.keyboard(false, true, fx);
            fx.push(Fx::App(AppFx::Search(request)));
            return Handled::Yes;
        }
        if elem == FIELD {
            self.keyboard(!self.editing, true, fx);
            return Handled::Yes;
        }
        if elem == CLEAR && !self.recents.is_empty() {
            self.store(SearchCmd::ClearRecents { profile_generation: self.draft.profile() }, fx);
            self.recent_clear_pending = true; self.recents.clear();
            self.content_dirty = true;
            self.reseat(FocusTarget::Elem(self.key(FIELD)), fx);
            return Handled::Yes;
        }
        if self.recents.contains(&elem) {
            let term = self.keys.iter().find_map(|key| match &key.identity {
                Identity::Recent(term) if key.elem == elem => Some(term.clone()), _ => None,
            });
            if let Some(term) = term {
                if let Some(query) = self.draft.replace(&term) {
                    self.store(SearchCmd::SetQueryScoped { profile_generation: self.draft.profile(), query }, fx);
                }
                self.remember(fx);
                self.rows.clear(); self.recents.clear();
                self.content_dirty = true;
                self.reseat(FocusTarget::Elem(self.key(FIELD)), fx);
            }
            return Handled::Yes;
        }
        for (row_index, row) in self.rows.iter().enumerate() {
            let Some(col) = row.elems.iter().position(|key| *key == elem) else { continue };
            let view = H::search(cx);
            let Some(shelf) = view.shelves().get(row_index) else { return Handled::No };
            let Some(item) = shelf.items.get(col) else { return Handled::No };
            match item {
                Item::Media(media) if !media.rk.is_empty() => {
                    self.remember(fx);
                    fx.push(Fx::App(AppFx::Search(if held {
                        SearchReq::ItemMenu { sid: media.sid, rk: media.rk.clone() }
                    } else { SearchReq::Detail { sid: media.sid, rk: media.rk.clone() } })));
                }
                Item::Tag(tag) if row.kind == Kind::Person && !held => {
                    let key = if tag.id.is_empty() || tag.id == "0" { &tag.tag_key } else { &tag.id };
                    if !key.is_empty() {
                        self.remember(fx);
                        fx.push(Fx::App(AppFx::Search(SearchReq::Person { sid: tag.sid, key: key.clone(),
                            guid: tag.tag_key.clone(), name: tag.name.clone(), thumb: tag.thumb.clone() })));
                    }
                }
                _ => {} // Collections are intentionally informative, not an invented detail route.
            }
            return Handled::Yes;
        }
        Handled::No
    }
}

impl<H: SearchLike> Machine<H> for SearchScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let acknowledged = matches!(ev, ScreenEvent::Mount)
            || matches!(ev, ScreenEvent::StoreChanged(store, _) if *store == StoreId::Search.ord());
        self.sync(cx, acknowledged, fx);
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Search(memory)) => { self.restore(memory); self.sync(cx, true, fx); }
            ScreenEvent::Mount => {
                self.fade.mount();
                self.reseat(FocusTarget::Elem(self.key(FIELD)), fx);
            }
            ScreenEvent::WillLeave(_) | ScreenEvent::Unmount => self.keyboard(false, true, fx),
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current { return self.activate(key.elem, false, cx, fx); }
            }
            ScreenEvent::PressHold(_) => {
                if let Some(key) = cx.focus.current { return self.activate(key.elem, true, cx, fx); }
            }
            ScreenEvent::Input(input) => match &input.kind {
                InputKind::SystemKeyboard(up) => { self.editing = *up; self.blink_us = 0; }
                InputKind::Text(edit) if self.editing || matches!(cx.owner, InputOwner::System(_)) => self.edit(edit, fx),
                InputKind::Key { key, sym, edge, .. } if *edge != Edge::Up => {
                    if self.editing {
                        let edit = match (*key, *sym) {
                            (Key::Left, _) => Some(TextEdit::Left), (Key::Right, _) => Some(TextEdit::Right),
                            (_, crate::ui::consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
                            (_, crate::ui::consts::SDLK_CLEAR) => Some(TextEdit::Clear), _ => None,
                        };
                        if let Some(edit) = edit { self.edit(&edit, fx); return Handled::Yes; }
                        if *key == Key::Back { self.keyboard(false, false, fx); return Handled::Yes; }
                        if *key == Key::Ok { self.keyboard(false, true, fx); return Handled::Yes; }
                        if *key == Key::Down {
                            if let Some(group) = self.first_content() {
                                self.keyboard(false, true, fx);
                                self.reseat(FocusTarget::ContainerGroup(group), fx);
                            }
                            return Handled::Yes;
                        }
                    }
                    if *key == Key::Up && cx.focus.current == Some(self.key(FIELD)) {
                        self.keyboard(false, true, fx);
                        self.reseat(FocusTarget::Elem(self.key(crate::ui::dispatch::STRIP_BASE + 3)), fx);
                        return Handled::Yes;
                    }
                    if *key == Key::Back {
                        fx.push(Fx::App(AppFx::Search(SearchReq::Tab(HomeTab::Home))));
                        return Handled::Yes;
                    }
                    return Handled::No;
                }
                _ => return Handled::No,
            },
            ScreenEvent::Tick(tick) => self.tick(*tick, cx, fx),
            ScreenEvent::FocusMoved { to, .. } => {
                if to.elem != FIELD { self.keyboard(false, true, fx); }
                self.reveal(*to, cx);
                fx.invalidate(Provenance::Input);
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }
}

impl SearchScreen {
    fn kinds(&self) -> ([Kind; 5], usize) {
        let mut kinds = [Kind::Movie; 5];
        for (i, row) in self.rows.iter().enumerate() { kinds[i] = row.kind; }
        (kinds, self.rows.len())
    }
    fn first_content(&self) -> Option<GroupId> {
        if !self.recents.is_empty() { Some(RECENTS_GROUP) } else { self.rows.first().map(|row| row.group) }
    }
    fn elem_at(&self, group: GroupId, index: usize) -> Option<u32> {
        if group == FIELD_GROUP { return (index == 0).then_some(FIELD); }
        if group == CLEAR_GROUP { return (!self.recents.is_empty() && index == 0).then_some(CLEAR); }
        if group == RECENTS_GROUP {
            return self.recents.get(index).copied();
        }
        self.rows.iter().find(|row| row.group == group)?.elems.get(index).copied()
    }
    fn index(&self, elem: u32) -> Option<(GroupId, usize)> {
        if elem == FIELD { return Some((FIELD_GROUP, 0)); }
        if elem == CLEAR && !self.recents.is_empty() { return Some((CLEAR_GROUP, 0)); }
        if let Some(i) = self.recents.iter().position(|key| *key == elem) { return Some((RECENTS_GROUP, i)); }
        self.rows.iter().find_map(|row| row.elems.iter().position(|key| *key == elem).map(|i| (row.group, i)))
    }
    fn row_rect(&self, row: usize, col: usize, at: At) -> Rect {
        let (kinds, n) = self.kinds();
        let shelf = &self.rows[row];
        let style = layout::style(shelf.kind);
        let scroll = if at == At::Drawn { self.scroll.pos } else { self.scroll_target };
        let origin = layout::top(&kinds[..n], row, |i| self.rows[i].motion.band_expand());
        crate::ui::card_row::tile_rect(col, style.margin_x, style.w + style.gap, shelf.motion.scroll_x(),
            origin + layout::HEAD_TO_ROW - scroll, (style.w, style.h))
    }
    fn reveal<H: SearchLike>(&mut self, key: FocusKey<u32>, _cx: &Cx<'_, H>) {
        let focused = self.rows.iter().position(|row| row.elems.contains(&key.elem));
        let (kinds, n) = self.kinds();
        self.scroll_target = if self.editing { 0.0 } else {
            focused.map_or(0.0, |i| layout::reveal(self.scroll_target, &kinds[..n], i))
        };
    }
    fn tick<H: SearchLike>(&mut self, tick: crate::ui::machine::Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if self.step_blink(tick.dt_us) { fx.invalidate(Provenance::Input); }
        self.hot.step(if self.field_hot(cx) { 1.0 } else { 0.0 }, crate::ui::consts::K_SCALE, tick.dt());
        for row in &mut self.rows {
            let focused = if self.editing { None } else {
                cx.focus.current.and_then(|key| row.elems.iter().position(|elem| *elem == key.elem))
            };
            row.motion.update(row.elems.len(), focused, &layout::style(row.kind), tick.dt());
        }
        if let Some(key) = cx.focus.current { self.reveal(key, cx); }
        self.scroll.step(self.scroll_target, crate::ui::consts::K_SCROLL, tick.dt());
        self.fade.tick(tick.dt(), !self.draft.pending() && H::search(cx).state() != crate::search::State::Searching);
        let colours = cx.focus.current.and_then(|key| self.rows.iter().enumerate().find_map(|(row, model)|
            model.elems.iter().position(|elem| *elem == key.elem).and_then(|col|
                H::search(cx).shelves().get(row)?.items.get(col)).and_then(|item| match item {
                    Item::Media(media) if media.has_blur => Some(media.blur), _ => None,
                })));
        self.ground.key(colours, crate::ui::widgets::PageGround::CARD_W, tick.dt());
        let target = cx.focus.current.and_then(|key| self.rows.iter().enumerate().find_map(|(row, model)|
            model.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col))));
        let view = H::search(cx);
        let handle = target.and_then(|(row, col)| view.shelves().get(row)?.items.get(col))
            .and_then(|item| view.scope().sources().iter().find(|source| source.sid == item.sid()))
            .map_or("", |source| source.handle.as_str());
        let target_row = target.map(|(row, _)| row);
        let settled = self.owner_row == target_row && self.owner == handle;
        self.owner_alpha.step(if settled && !self.owner.is_empty() { 1.0 } else { 0.0 },
            crate::ui::consts::K_SCALE, tick.dt());
        if !settled && self.owner_alpha.pos < 0.02 {
            self.owner_row = target_row;
            self.owner.clear(); self.owner.push_str(handle);
            fx.invalidate(Provenance::Input);
        }
    }

    fn step_blink(&mut self, dt_us: u32) -> bool {
        if !self.editing { self.blink_us = 0; return false; }
        let was = self.blink_us < BLINK_US;
        self.blink_us = ((self.blink_us as u64 + dt_us as u64) % (2 * BLINK_US) as u64) as u32;
        was != (self.blink_us < BLINK_US)
    }
}

fn group(id: GroupId, kind: GroupKind, len: usize, extent: Rect, elem: ElemKind, seat: Seat) -> GroupSpec {
    GroupSpec { id, kind, seat, reachable: AxisMask::BOTH, edge: [EdgeRule::Geometric, EdgeRule::Geometric,
        EdgeRule::Stop, EdgeRule::Stop], extent, len, elem }
}

impl<H: SearchLike> Focusable<H> for SearchScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let field = Rect::new(layout::FIELD.x, layout::FIELD.y - self.scroll_target, layout::FIELD.w, layout::FIELD.h);
        out.push(group(FIELD_GROUP, GroupKind::Row { wrap: false }, 1, field, ElemKind::Bare, Seat::First));
        if !self.recents.is_empty() {
            out.push(group(RECENTS_GROUP, GroupKind::Column, self.recents.len(),
                layout::recent(0, self.scroll_target), ElemKind::Bare, Seat::Remembered));
            let mut clear = group(CLEAR_GROUP, GroupKind::Row { wrap: false }, 1,
                layout::clear(self.recents.len(), self.scroll_target, cx.measure), ElemKind::Control, Seat::First);
            clear.edge[1] = EdgeRule::Stop;
            out.push(clear);
        }
        for (i, row) in self.rows.iter().enumerate() {
            let elem = if matches!(row.kind, Kind::Movie | Kind::Show | Kind::Episode) { ElemKind::Card } else { ElemKind::Bare };
            out.push(group(row.group, GroupKind::Row { wrap: false }, row.elems.len(), self.row_rect(i, 0, At::SpringTarget), elem, Seat::Projected));
        }
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> { self.index(*elem).map(|p| p.0) }
    fn neighbour(&self, from: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some((group, index)) = self.index(from.elem) else { return Step::Edge };
        let next = match (group == RECENTS_GROUP, dir) {
            (true, Dir::Up) | (false, Dir::Left) => index.checked_sub(1),
            (true, Dir::Down) | (false, Dir::Right) => index.checked_add(1),
            _ => None,
        };
        next.and_then(|i| self.elem_at(group, i)).map_or(Step::Edge, |elem| Step::Move(self.key(elem)))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let (group, index) = self.index(*elem)?;
        let scroll = if at == At::Drawn { self.scroll.pos } else { self.scroll_target };
        let rect = if group == FIELD_GROUP { Rect::new(layout::FIELD.x, layout::FIELD.y - scroll, layout::FIELD.w, layout::FIELD.h) }
        else if group == CLEAR_GROUP { layout::clear(self.recents.len(), scroll, cx.measure) }
        else if group == RECENTS_GROUP { layout::recent(index, scroll) }
        else { self.row_rect(self.rows.iter().position(|row| row.group == group)?, index, at) };
        let painted = if at == At::Drawn {
            self.rows.iter().find(|row| row.group == group).map_or(rect, |row| rect.scaled(row.motion.scale(index)))
        } else { rect };
        Some(Placed { rect: painted, rest_rect: rect, clip: Rect::FULL, index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, H>) -> FocusKey<u32> {
        if want.entry == self.entry && self.index(want.elem).is_some() { return want; }
        if let Some(old) = self.keys.iter().find(|key| key.elem == want.elem) {
            let len = if old.group == RECENTS_GROUP { self.recents.len() } else {
                self.rows.iter().find(|row| row.group == old.group).map_or(0, |row| row.elems.len())
            };
            if let Some(elem) = self.elem_at(old.group, old.slot.min(len.saturating_sub(1))) { return self.key(elem); }
        }
        self.key(FIELD)
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if cx.focus.current.is_some_and(|key| key.elem == FIELD) {
            return cx.focus.remembered(group).filter(|elem| self.index(*elem).is_some())
                .or_else(|| self.elem_at(group, 0)).map_or(self.key(FIELD), |elem| self.key(elem));
        }
        if let Some(row) = self.rows.iter().find(|row| row.group == group) {
            let style = layout::style(row.kind);
            let index = crate::ui::card_row::column_near_x(from.rect.cx(), style.margin_x, style.w + style.gap,
                style.w, row.motion.scroll_x(), row.elems.len(), from.index.unwrap_or(0) as usize);
            return self.elem_at(group, index).map_or(self.key(FIELD), |elem| self.key(elem));
        }
        self.elem_at(group, 0).map_or(self.key(FIELD), |elem| self.key(elem))
    }
}

impl<H: SearchLike> Screen<H> for SearchScreen {
    fn name(&self) -> &'static str { "search" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, cx: &Cx<'_, H>) {
        self.render.prepare(self.draft.query(), self.draft.caret(), &self.owner, cx);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) { render::draw(self, f); }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn memory(&self) -> PageMemory { PageMemory::Search(self.page_memory()) }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link { from: STRIP, dir: Dir::Down, to: FIELD_GROUP });
        out.push(Link { from: FIELD_GROUP, dir: Dir::Up, to: STRIP });
        if let Some(first) = self.first_content() {
            out.push(Link { from: FIELD_GROUP, dir: Dir::Down, to: first });
            out.push(Link { from: first, dir: Dir::Up, to: FIELD_GROUP });
        }
        if !self.recents.is_empty() {
            out.push(Link { from: RECENTS_GROUP, dir: Dir::Down, to: CLEAR_GROUP });
            out.push(Link { from: CLEAR_GROUP, dir: Dir::Up, to: RECENTS_GROUP });
        }
        for pair in self.rows.windows(2) {
            out.push(Link { from: pair[0].group, dir: Dir::Down, to: pair[1].group });
            out.push(Link { from: pair[1].group, dir: Dir::Up, to: pair[0].group });
        }
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> { Some(self) }
}

impl LogicalState for SearchScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).u32(self.instance.0);
        self.draft.write(c);
        c.bool(self.mounted).bool(self.editing).u32(self.blink_us);
        for spring in [&self.hot, &self.scroll] { c.f32(spring.pos).f32(spring.vel); }
        c.f32(self.scroll_target).u32(self.next_elem).u32(self.query_gen)
            .bool(self.recent_clear_pending).bool(self.content_dirty);
        self.fade.write(c);
        self.ground.write_motion(c);
        c.option(self.owner_row, |c, row| { c.u64(row as u64); });
        c.str(&self.owner).f32(self.owner_alpha.pos).f32(self.owner_alpha.vel);
        c.option(self.restored.as_ref(), |c, memory| { memory.write(c); });
        c.seq(self.keys.len());
        for key in &self.keys { key.write(c); }
        c.seq(self.recents.len());
        for elem in &self.recents { c.u32(*elem); }
        c.seq(self.rows.len());
        for row in &self.rows {
            c.u32(layout::ordinal(row.kind)).u32(row.group.0).seq(row.elems.len());
            for elem in &row.elems { c.u32(*elem); }
            row.motion.write_motion(c);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("search entry={} editing={} pending={} rows={} recents={} caret={}",
            self.entry.0, self.editing, self.draft.pending(), self.rows.len(), self.recents.len(), self.draft.caret()));
    }
}

impl KeyEntry {
    fn write(&self, c: &mut Canon) {
        match &self.identity {
            Identity::Recent(term) => { c.u32(0).str(term); }
            Identity::Media(kind, sid, rk) => { c.u32(1).u32(layout::ordinal(*kind)).u32(sid.raw() as u32).str(rk); }
            Identity::Tag(kind, sid, id) => { c.u32(2).u32(layout::ordinal(*kind)).u32(sid.raw() as u32).str(id); }
            Identity::Slot(kind, generation, slot) => { c.u32(3).u32(layout::ordinal(*kind)).u32(*generation).u64(*slot as u64); }
        }
        c.u32(self.elem).u32(self.group.0).u64(self.slot as u64);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn caret_clock_keeps_fractional_milliseconds_and_reports_only_visible_flips() {
        let mut screen = SearchScreen::new(EntryId(1), InstanceId(1));
        screen.editing = true;
        for _ in 0..1059 { assert!(!screen.step_blink(500)); }
        assert_eq!(screen.blink_us, 529_500);
        assert!(screen.step_blink(500));
        assert!(!screen.step_blink(500));
        assert!(screen.step_blink(BLINK_US - 500));
        assert_eq!(screen.blink_us, 0);
        assert!(!screen.step_blink(10 * BLINK_US));
        screen.editing = false;
        for _ in 0..100 { assert!(!screen.step_blink(16_667)); }
        assert_eq!(screen.blink_us, 0);
    }
}
