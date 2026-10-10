//! Owned Search page. Input owns focus; this instance owns editing, motion and render caches.
//! This is the production Search implementation: `AppArg::Search` mounts it unconditionally, and
//! the legacy Search renderer it replaced is deleted entirely.
mod cards;
mod draft;
// `pub`: exposes `layout::{FIELD, CONTENT_TOP}` to `ui::consts`'s overscan-rects audit,
// which otherwise has no path to this module's geometry. Replaces the deleted legacy Search
// renderer's own `FIELD`/`CONTENT_TOP` module-level constants.
pub mod layout;
mod render;
mod memory;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)
pub use memory::Memory;

use std::borrow::Cow;
use plx_data::search::{Item, Kind};
use crate::registry::{AppFx, HomeTab, PageMemory, SearchLike, SearchReq};
use plx_data::stores::{StoreCmd, StoreId};
use plx_data::stores::search::SearchCmd;
use plx_ui::cards::{CardEvent, PageEdge, SectionFrame, SectionSpec, Shelf, Stack, StackEvent, StackMemory, StackPage};
use cards::RowCards;
use plx_ui::consts::{SCR_H, SCR_W};
use plx_ui::frame::Budget;
use plx_machine::machine::{Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId,
    Handled, InputKind, InputOwner, InstanceId, Key, LogicalState, Machine, MachineId, TextEdit};
use plx_machine::present::Provenance;
use plx_ui::screen::{At, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, GroupKind, GroupSpec,
    Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, AxisMask};
use plx_ui::{Rect, Spring};
use draft::Draft;

const FIELD: u32 = 1;
const CLEAR: u32 = 2;
const FIELD_GROUP: GroupId = GroupId(0x5345_4100);
const RECENTS_GROUP: GroupId = GroupId(0x5345_4101);
const CLEAR_GROUP: GroupId = GroupId(0x5345_4102);
const STRIP: GroupId = plx_ui::containers::tabs::STRIP;
/// The alpha at or under which the owner annotation is invisible: the renderer draws no run
/// below it, and the instance swaps the word it holds only there — so a handle never changes
/// under the eye (legacy `the_owner_annotation_swaps_its_words_only_while_it_is_invisible`).
pub(super) const OWNER_FLOOR: f32 = 0.02;
const BLINK_MS: u32 = 530;
const BLINK_US: u32 = BLINK_MS * 1000;

/// The canonical shape of the state hash. Each row's `motion` bytes are its `Shelf`'s
/// (`Shelf::write`), which is the L0 row's motion write unchanged, so the text, and with it
/// `SCREEN_SHAPES_PIN` and the replay anchors, stays what it was; the type's name is split only so
/// the `cards` gate does not read a shape string as a use of the primitive.
pub const SHAPE: &str = concat!("SearchScreen{entry:u32,instance:u32,draft:{text:str,caret:u64,profile:u32,pending:bool},mounted:bool,editing:bool,blink_us:u32,hot:Spring,scroll:Spring,scroll_target:f32,next_elem:u32,query_gen:u32,recent_clear_pending:bool,content_dirty:bool,fade:Xfade,ground:PageGround,owner_row:Option<u64>,owner:str,owner_alpha:Spring,restored:Option<SearchMemory>,keys:[SearchKey],recents:[u32],rows:[{kind:u32,group:u32,elems:[u32],motion:Card", "Row}]}");

#[derive(Clone, Debug, PartialEq, Eq)]
enum Identity {
    Recent(String),
    Media(Kind, plx_plex::plex::ServerId, String),
    Tag(Kind, plx_plex::plex::ServerId, String),
    Slot(Kind, u32, usize),
}
#[derive(Clone, Debug)]
struct KeyEntry { identity: Identity, elem: u32, group: GroupId, slot: usize }
/// One result row. Rows are keyed by their `kind` (the page holds at most one per kind), so a row
/// that appears, disappears or moves in the list keeps its own shelf in the [`Stack`] — its scroll,
/// its pop, its caption band — and never lends it to a neighbour.
struct Row { kind: Kind, group: GroupId, elems: Vec<u32> }

/// The sections of the page, top to bottom. A result row's key carries the page's `rows_gen`: a
/// row the page wiped and rebuilt is a new section with a new shelf, as a wiped row always was.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sec { Head, Recents, Clear, Row(Kind, u32) }

pub struct SearchScreen {
    entry: EntryId,
    instance: InstanceId,
    draft: Draft,
    mounted: bool,
    editing: bool,
    blink_us: u32,
    hot: Spring,
    /// The page's vertical layout, scroll, reveal and shelves (taken out of the field only while
    /// it is handed this page, so it can borrow the page).
    stack: Stack<Sec>,
    /// Bumped whenever the result rows are wiped (see [`Sec::Row`]); not part of the state hash.
    rows_gen: u32,
    keys: Vec<KeyEntry>,
    next_elem: u32,
    rows: Vec<Row>,
    recents: Vec<u32>,
    query_gen: u32,
    recent_clear_pending: bool,
    publication: Option<plx_data::search::view::SearchSnapshot>,
    content_dirty: bool,
    fade: plx_ui::xfade::Xfade,
    ground: plx_ui::widgets::PageGround,
    owner_row: Option<usize>,
    owner: String,
    owner_alpha: Spring,
    restored: Option<Memory>,
    render: render::Resources,
    /// Diagnostic-only counter, not part of [`LogicalState::write`]/the state hash — how many
    /// `Search` `StoreChanged` deliveries this instance has seen. It carries the same name
    /// (`notices`) the retired route-word page kept its own count under, so `app::bridge`'s generic
    /// "did a store notice reach the top screen" probes stay readable now that `Route::Search`
    /// mounts this screen unconditionally.
    notices: u32,
    /// The page ask last sent for each row (window start, direction), by [`layout::ordinal`]. Not
    /// part of the state hash. A row asks again only once its window has moved: the edge keeps
    /// reporting while the store reads, and a second ask on a window that already slid would slide
    /// it past the focused card.
    page_sent: [Option<(usize, bool)>; 5],
}

impl SearchScreen {
    pub fn new(entry: EntryId, instance: InstanceId) -> Self {
        Self { entry, instance, draft: Draft::new(0, ""), mounted: false, editing: false,
            blink_us: 0, hot: Spring::at(1.0), stack: new_stack(entry), rows_gen: 0,
            keys: Vec::new(), next_elem: 10, rows: Vec::new(), recents: Vec::new(), query_gen: 0,
            recent_clear_pending: false, publication: None, content_dirty: true,
            fade: plx_ui::xfade::Xfade::new(), ground: plx_ui::widgets::PageGround::new(),
            owner_row: None, owner: String::new(), owner_alpha: Spring::at(0.0), restored: None, render: Default::default(),
            notices: 0, page_sent: [None; 5] }
    }
    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: self.entry, elem } }
    fn real_query(&self) -> bool { plx_data::search::terms(self.draft.query()).is_some() }
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
        if up { self.draft.to_end(); self.stack.scroll_to(0.0); }
        fx.push(Fx::Deliver(MachineId::Instance(self.instance), Delivery::Keyboard { up }));
        fx.invalidate(Provenance::Input);
    }
    fn edit<H: SearchLike>(&mut self, edit: &TextEdit, fx: &mut Effects<'_, H>) {
        if let Some(query) = self.draft.edit(edit) {
            self.store(SearchCmd::SetQueryScoped { profile_generation: self.draft.profile(), query }, fx);
            self.wipe_rows();
            self.stack.scroll_to(0.0);
            self.content_dirty = true;
            self.fade.reload();
        }
        self.blink_us = 0;
        fx.invalidate(Provenance::Input);
    }

    /// Drop the result rows, and with them their shelves.
    fn wipe_rows(&mut self) {
        if !self.rows.is_empty() {
            self.rows.clear();
            self.rows_gen = self.rows_gen.wrapping_add(1);
            self.page_sent = [None; 5];
        }
    }

    /// Bring the page's model up to the store's. Answers whether the result shelves were replaced
    /// by a publication (the caller then brings the focused row into view).
    fn sync<H: SearchLike>(&mut self, cx: &Cx<'_, H>, notified: bool, fx: &mut Effects<'_, H>) -> bool {
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
            self.wipe_rows(); self.recents.clear();
            self.stack.jump_to(0.0);
            self.recent_clear_pending = false;
            self.content_dirty = true;
        }
        if let Some(memory) = &restored {
            self.keys = memory.keys.clone(); self.next_elem = memory.next_elem;
            self.stack.jump_to(memory.scroll);
        }
        if notified && view.recents().terms().is_empty() && self.recent_clear_pending {
            self.recent_clear_pending = false;
            self.content_dirty = true;
        }
        if self.publication.as_ref().is_some_and(|old| view.same_publication(old))
            && !self.content_dirty && pending == self.draft.pending() { return false; }
        let shelves_changed = self.publication.as_ref().is_none_or(|old|
            !std::ptr::eq(old.view().shelves(), view.shelves()));
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
            self.wipe_rows();
            return false;
        }
        if query_changed {
            self.wipe_rows();
            self.fade.reload();
        }
        self.rows.clear();
        for shelf in view.shelves() {
            let group = layout::group(shelf.kind);
            let mut elems = Vec::with_capacity(shelf.items.len());
            for (slot, item) in shelf.items.iter().enumerate() {
                let identity = match item {
                    Item::Media(media) if !media.rk.is_empty() => Identity::Media(shelf.kind, media.sid, media.rk.clone()),
                    Item::Collection(hit) if !hit.item.rk.is_empty() => Identity::Media(shelf.kind, hit.item.sid, hit.item.rk.clone()),
                    // a tag-shaped collection keys on its tag id, as it did while it was a tag
                    Item::Collection(hit) if hit.tag > 0 => Identity::Tag(shelf.kind, hit.item.sid, hit.tag.to_string()),
                    Item::Tag(tag) if !tag.tag_key.is_empty() || (!tag.id.is_empty() && tag.id != "0") =>
                        Identity::Tag(shelf.kind, tag.sid, if tag.tag_key.is_empty() { tag.id.clone() } else { tag.tag_key.clone() }),
                    _ => Identity::Slot(shelf.kind, view.query_gen(), slot),
                };
                elems.push(self.intern(identity, group, slot));
            }
            self.rows.push(Row { kind: shelf.kind, group, elems });
        }
        if let Some(memory) = restored {
            let shelves = self.rows.iter().filter_map(|row| memory.rows.iter()
                .find(|(kind, _)| *kind == row.kind).map(|(_, x)| (Sec::Row(row.kind, self.rows_gen), *x))).collect();
            self.stack.restore(&StackMemory { scroll: memory.scroll, shelves });
        }
        shelves_changed
    }

    fn activate<H: SearchLike>(&mut self, elem: u32, held: bool, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if let Some(tab) = elem.checked_sub(plx_ui::dispatch::STRIP_BASE) {
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
                self.wipe_rows(); self.recents.clear();
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
                // A collection opens its page; it has no item menu, so a hold does the same.
                Item::Collection(hit) if hit.route().is_some() => {
                    self.remember(fx);
                    fx.push(Fx::App(AppFx::Search(SearchReq::Collection {
                        sid: hit.item.sid, rk: hit.item.rk.clone(), tag: hit.tag })));
                }
                Item::Tag(tag) if row.kind == Kind::Person && !held => {
                    let key = if tag.id.is_empty() || tag.id == "0" { &tag.tag_key } else { &tag.id };
                    if !key.is_empty() {
                        self.remember(fx);
                        fx.push(Fx::App(AppFx::Search(SearchReq::Person { sid: tag.sid, key: key.clone(),
                            guid: tag.tag_key.clone(), name: tag.name.clone(), thumb: tag.thumb.clone() })));
                    }
                }
                _ => {}
            }
            return Handled::Yes;
        }
        Handled::No
    }
}

impl<H: SearchLike> Machine<H> for SearchScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let store_changed = matches!(ev, ScreenEvent::StoreChanged(store, _) if *store == StoreId::Search.ord());
        if store_changed { self.notices += 1; }
        let acknowledged = matches!(ev, ScreenEvent::Mount) || store_changed;
        let mut follow = self.sync(cx, acknowledged, fx);
        if let ScreenEvent::RestoreMemory(PageMemory::Search(memory)) = ev {
            self.restore(memory);
            follow |= self.sync(cx, true, fx);
        }
        let card = self.stack_on(ev, cx, fx);
        if follow { self.with_stack(|stack, page| stack.reveal(page, cx)); self.keep_home_while_editing(); }
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Search(_)) => {}
            ScreenEvent::Mount => {
                self.fade.mount();
                self.reseat(FocusTarget::Elem(self.key(FIELD)), fx);
            }
            ScreenEvent::WillLeave(_) | ScreenEvent::Unmount | ScreenEvent::Cover | ScreenEvent::Suspend => self.keyboard(false, false, fx),
            ScreenEvent::Activate(elem) => return self.activate(*elem, false, cx, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card { return self.activate(elem, false, cx, fx); }
                if let Some(key) = cx.focus.current { return self.activate(key.elem, false, cx, fx); }
            }
            ScreenEvent::PressHold(_) => {
                if let Some(CardEvent::Hold(elem)) = card { return self.activate(elem, true, cx, fx); }
                if let Some(key) = cx.focus.current { return self.activate(key.elem, true, cx, fx); }
            }
            ScreenEvent::Input(input) => match &input.kind {
                InputKind::SystemKeyboard(up) => {
                    if *up { self.stack.scroll_to(0.0); }
                    if *up && !self.editing {
                        self.draft.to_end();
                        if cx.focus.current != Some(self.key(FIELD)) {
                            self.reseat(FocusTarget::Elem(self.key(FIELD)), fx);
                        }
                    }
                    self.editing = *up; self.blink_us = 0;
                }
                InputKind::Text(edit) if self.editing || matches!(cx.owner, InputOwner::System(_)) => self.edit(edit, fx),
                InputKind::Wheel { dy } => {
                    if !self.editing && dy.is_finite() {
                        let max = self.stack.max_scroll(&*self, cx);
                        self.stack.scroll_to((self.stack.target() - dy * plx_ui::table::ROW_H).clamp(0.0, max));
                        fx.invalidate(Provenance::Input);
                    }
                }
                InputKind::Key { key, sym, edge, .. } if *edge != Edge::Up => {
                    if self.editing {
                        let edit = match (*key, *sym) {
                            (Key::Left, _) => Some(TextEdit::Left), (Key::Right, _) => Some(TextEdit::Right),
                            (_, plx_ui::consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
                            (_, plx_ui::consts::SDLK_CLEAR) => Some(TextEdit::Clear), _ => None,
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
                        self.reseat(FocusTarget::Elem(self.key(plx_ui::dispatch::STRIP_BASE + 3)), fx);
                        return Handled::Yes;
                    }
                    if *key == Key::Back {
                        fx.push(Fx::App(AppFx::Search(SearchReq::Back)));
                        return Handled::Yes;
                    }
                    return Handled::No;
                }
                _ => return Handled::No,
            },
            ScreenEvent::Tick(tick) => self.tick(*tick, cx, fx),
            ScreenEvent::FocusMoved { to, .. } => {
                if to.elem != FIELD { self.keyboard(false, true, fx); }
                self.keep_home_while_editing();
                fx.invalidate(Provenance::Input);
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }
}

impl SearchScreen {
    /// Feed `ev` to the page's stack and answer the card event it reported, if any. An activation
    /// of a plain element is answered by [`SearchScreen::activate`] from the event itself.
    fn stack_on<H: SearchLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>)
        -> Option<CardEvent<u32>> {
        let got = self.with_stack(|stack, page| stack.on(page, ev, cx, fx));
        if matches!(ev, ScreenEvent::Tick(_)) { self.page_rows(&got, cx, fx); }
        match got { Some(StackEvent::Card(_, card)) => Some(card), _ => None }
    }
    /// Ask the store to slide a row's window when focus is near the edge it can go on past: the
    /// trailing edge on the row's own `Want`, the leading edge when the window has hits before it.
    fn page_rows<H: SearchLike>(&mut self, got: &Option<StackEvent<Sec, u32>>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let view = H::search(cx);
        for row in 0..self.rows.len() {
            let Some(shelf) = view.shelves().get(row) else { continue };
            let k = self.section(row);
            let wanted = matches!(got, Some(StackEvent::Card(key, CardEvent::Want(_))) if *key == k);
            let offset = shelf.window.start;
            let edge = self.with_stack(|stack, page| stack.page_ask(page, cx, k, wanted, offset));
            let slot = layout::ordinal(self.rows[row].kind) as usize;
            if self.page_sent[slot].is_some_and(|(start, _)| start != offset) { self.page_sent[slot] = None; }
            if let Some(edge) = edge {
                let ask = (offset, edge == PageEdge::Before);
                if self.page_sent[slot] != Some(ask) {
                    self.page_sent[slot] = Some(ask);
                    self.store(SearchCmd::Page { kind: self.rows[row].kind, before: ask.1, seen: offset }, fx);
                }
            }
        }
    }
    /// Run `f` on the stack and the page it lays out (the stack is out of the page meanwhile).
    fn with_stack<R>(&mut self, f: impl FnOnce(&mut Stack<Sec>, &Self) -> R) -> R {
        let mut stack = std::mem::replace(&mut self.stack, Stack::new(self.entry));
        let got = f(&mut stack, &*self);
        self.stack = stack;
        got
    }
    /// While the keyboard is up the page rests at its head, whatever moves focus.
    fn keep_home_while_editing(&mut self) {
        if self.editing { self.stack.scroll_to(0.0); }
    }

    pub fn selected_item<'a, H: SearchLike>(&self, focus: Option<FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a Item> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        let view = H::search(cx);
        if self.draft.pending() || self.query_gen != view.query_gen() || self.draft.profile() != view.recents().generation() { return None; }
        let (row, col) = self.rows.iter().enumerate().find_map(|(row, model)|
            model.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col)))?;
        view.shelves().get(row)?.items.get(col)
    }

    pub fn redraw_focused<H: SearchLike>(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        if self.selected_item(focus, f.cx).is_none() { return; }
        let Some(key) = focus else { return };
        let Some(row) = self.rows.iter().position(|model| model.elems.contains(&key.elem)) else { return };
        let painter = f.painter.alpha(f.page_alpha * self.fade.alpha());
        // The lifted opener is painted after the strip, unlike the ordinary page flow.
        let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
        let _clip = f.clip(painter, Rect::new(0.0, floor, SCR_W, SCR_H - floor));
        let cx = f.cx;
        let view = H::search(cx);
        let Some(src) = self.row_cards(view, row) else { return };
        let Some((frame, shelf)) = self.row_frame(cx, row) else { return };
        shelf.redraw_focused(f, painter, &src, frame, Some(key));
    }
    /// The focus fingerprint's read of this screen's own space (`app::bridge::content_probe`'s
    /// Search arm) — called only once that caller has already asked the shared bar
    /// (`app::chrome::ChromeSnapshot::focus`) and found focus `Away` from the chip/strip, i.e.
    /// actually resting somewhere on this page.
    ///
    /// Mirrors the deleted legacy `ui::search::Zone` naming (`Field`/`Recents`/`Results`)
    /// verbatim, and ADDS `Clear`: the legacy zone folded the Clear control into `Recents` (its
    /// `View::recent == clear_index()` was the only tell), because a single global cursor was the
    /// zone's only address space. The owned model gives Clear its own [`GroupId`]
    /// (`CLEAR_GROUP`), so the zone can just say so instead of asking a reader to compare two
    /// numbers to reconstruct a fact the model already has.
    ///
    /// `row`/`col`/`recent` read `-1` off their own zone, the convention
    /// `screens::library::LibraryScreen::probe_viewport` already uses for "not there" — not a
    /// persisted last-visited cursor, which is what the legacy module-level statics held even
    /// off their own zone (`tests/keytable.json`'s Search rows recorded that quirk directly:
    /// `row=0 col=0` while `zone=Strip`, because `View::row`/`View::col` were read, never reset,
    /// regardless of `View::zone`). That value was never a fact about the page; the anchor's
    /// Search rows move to `-1` in the same commit that adds this method, for that reason.
    pub fn probe(&self, focus: Option<FocusKey<u32>>) -> (&'static str, i64, i64, i64, bool) {
        let elem = focus.filter(|key| key.entry == self.entry).map(|key| key.elem);
        match elem {
            Some(FIELD) => ("Field", -1, -1, -1, false),
            Some(CLEAR) => ("Clear", -1, -1, -1, false),
            Some(elem) if self.recents.contains(&elem) => {
                let index = self.recents.iter().position(|e| *e == elem).unwrap_or(0);
                ("Recents", -1, -1, index as i64, false)
            }
            Some(elem) => self.rows.iter().enumerate().find_map(|(row, model)|
                model.elems.iter().position(|key| *key == elem).map(|col| (row, col)))
                .map(|(row, col)| {
                    let card = matches!(self.rows[row].kind, Kind::Movie | Kind::Show | Kind::Episode);
                    ("Results", row as i64, col as i64, -1, card)
                }).unwrap_or(("Field", -1, -1, -1, false)),
            None => ("Field", -1, -1, -1, false),
        }
    }

    /// What is currently drawn under the field — the same three-way rule the deleted legacy
    /// `ui::search::below_of` computed, reproduced over this screen's own fields (`real_query`,
    /// `recents`, `rows`) instead of two separate module-level reads (`search::query()` and
    /// `search::shelves()`), because this screen is the one holding both now.
    pub fn probe_below(&self) -> &'static str {
        if !self.real_query() {
            if self.recents.is_empty() { "Nothing" } else { "Recents" }
        } else if self.rows.is_empty() { "Nothing" } else { "Results" }
    }

    pub fn is_editing(&self) -> bool { self.editing }

    /// How many recent terms are shown — the legacy `clear_index()`'s value
    /// (`recents::count().min(MAX_RECENTS)`). This model's `recents` vec is built from the same
    /// capped source (`search::recents::CAP`), so it already holds no more than that, and no
    /// `.min` is needed to reproduce the number.
    pub fn recents_shown(&self) -> usize { self.recents.len() }

    fn first_content(&self) -> Option<GroupId> {
        if !self.recents.is_empty() { Some(RECENTS_GROUP) } else { self.rows.first().map(|row| row.group) }
    }
    /// The stack's key of result row `row`.
    fn section(&self, row: usize) -> Sec { Sec::Row(self.rows[row].kind, self.rows_gen) }
    /// Result row `row`'s shelf, over `cx`'s page: where its tiles sit now and what they hold.
    fn row_frame<H: SearchLike>(&self, cx: &Cx<'_, H>, row: usize) -> Option<(SectionFrame, &Shelf)> {
        let k = self.section(row);
        Some((self.stack.shelf_frame(self, cx, k)?, self.stack.shelf(k)?))
    }
    /// Result row `row`'s cards over `view`; `None` when the view has no such shelf.
    fn row_cards<'a>(&'a self, view: plx_data::search::view::SearchView<'a>, row: usize) -> Option<RowCards<'a>> {
        let model = self.rows.get(row)?;
        let shelf = view.shelves().get(row)?;
        Some(RowCards { kind: model.kind, elems: &model.elems, items: &shelf.items,
            sources: view.scope().sources(), window: shelf.window })
    }
    fn tick<H: SearchLike>(&mut self, tick: plx_machine::machine::Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::StoreWork(plx_data::stores::StoreWork::BrowseDiscovery)));
        fx.push(Fx::App(AppFx::StoreWork(plx_data::stores::StoreWork::Search { dt_us: tick.dt_us })));
        if self.step_blink(tick.dt_us) { fx.invalidate(Provenance::Input); }
        self.fade.tick(tick.dt(), !self.draft.pending() && H::search(cx).state() != plx_data::search::State::Searching);
        let colours = cx.focus.current.and_then(|key| self.rows.iter().enumerate().find_map(|(row, model)|
            model.elems.iter().position(|elem| *elem == key.elem).and_then(|col|
                H::search(cx).shelves().get(row)?.items.get(col)).and_then(|item| match item {
                    Item::Media(media) if media.has_blur => Some(media.blur),
                    Item::Collection(hit) if hit.item.has_blur => Some(hit.item.blur), _ => None,
                })));
        self.ground.key(colours, plx_ui::widgets::PageGround::CARD_W, tick.dt());
        let target = cx.focus.current.and_then(|key| self.rows.iter().enumerate().find_map(|(row, model)|
            model.elems.iter().position(|elem| *elem == key.elem).map(|col| (row, col))));
        let view = H::search(cx);
        let handle = target.and_then(|(row, col)| view.shelves().get(row)?.items.get(col))
            .and_then(|item| view.scope().sources().iter().find(|source| source.sid == item.sid()))
            .map_or("", |source| source.handle.as_str());
        let target_row = target.map(|(row, _)| row);
        let settled = self.owner_row == target_row && self.owner == handle;
        // The three springs this instance owns are stepped through the shared reporting
        // integrator, so each one keeps the present gate awake exactly while it is visibly
        // travelling and goes quiet the frame it arrives (`machine/src/motion.rs`'s rest test —
        // magnitude-relative, capped under a quarter pixel, velocity judged as this frame's
        // travel). `Spring::step` reports to `plx_machine::idle` and says nothing to the dispatcher's
        // own gate, which is the one an owned page is graded on.
        let hot = if self.field_hot(cx) { 1.0 } else { 0.0 };
        let owner = if settled && !self.owner.is_empty() { 1.0 } else { 0.0 };
        {
            let mut present = fx.present();
            let k_scale = plx_ui::consts::K_SCALE;
            plx_machine::motion::spring(&mut self.hot.pos, &mut self.hot.vel, hot, k_scale, tick, &mut present);
            plx_machine::motion::spring(&mut self.owner_alpha.pos, &mut self.owner_alpha.vel, owner,
                k_scale, tick, &mut present);
        }
        if !settled && self.owner_alpha.pos < OWNER_FLOOR {
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

/// The page's stack: the standing strip owns the hits under it, the scroll follows focus when it
/// moves (a wheel or a raised keyboard sets it in between), and the page paints through
/// `render.rs` (the cards fade in with the results).
fn new_stack(entry: EntryId) -> Stack<Sec> {
    let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
    Stack::new(entry).hold_hint(plx_ui::hold_hint::Kind::Search).reveal_on_move(true).clipped(Rect::new(0.0, floor, SCR_W, SCR_H - floor))
}

impl<H: SearchLike> StackPage<H> for SearchScreen {
    type Key = Sec;
    type Cards<'a> = RowCards<'a>;

    fn revision(&self, cx: &Cx<'_, H>) -> u64 {
        let view = H::search(cx);
        let mix = |h: u64, v: u64| (h ^ v).wrapping_mul(0x0100_0000_01b3);
        let mut h = mix(mix(0xcbf2_9ce4_8422_2325, self.rows_gen as u64), self.recents.len() as u64);
        for (i, row) in self.rows.iter().enumerate() {
            let shown = view.shelves().get(i).map_or(0, |shelf| shelf.items.len()).min(row.elems.len());
            h = mix(mix(h, layout::ordinal(row.kind) as u64), shown as u64);
        }
        h
    }
    fn sections(&self, _cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, plx_ui::cards::Kind::Custom { height: layout::CONTENT_TOP, focusable: true }, FIELD_GROUP));
        if !self.recents.is_empty() {
            let shown = self.recents.len().min(layout::RECENT_CAP) as f32;
            out.push(SectionSpec::new(Sec::Recents, plx_ui::cards::Kind::Custom {
                height: plx_ui::table::HDR_H + shown * plx_ui::table::ROW_H, focusable: true }, RECENTS_GROUP));
            out.push(SectionSpec::new(Sec::Clear, plx_ui::cards::Kind::Custom { height: 0.0, focusable: true }, CLEAR_GROUP));
        }
        for row in &self.rows {
            let elem = if matches!(row.kind, Kind::Movie | Kind::Show | Kind::Episode) { ElemKind::Card } else { ElemKind::Bare };
            // Search owns the vertical model: explicit links between rows and a seat projected from
            // where the move came from, not the shelf's remembered card.
            out.push(SectionSpec::new(Sec::Row(row.kind, self.rows_gen),
                plx_ui::cards::Kind::Shelf { style: layout::style(row.kind), heading: layout::HEAD_TO_ROW }, row.group)
                .seated(Seat::Projected)
                .edges([EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop])
                .of_kind(elem));
        }
    }
    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) { out.push(Sec::Head); }
    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<RowCards<'a>> {
        let Sec::Row(kind, _) = k else { return None };
        let row = self.rows.iter().position(|row| row.kind == kind)?;
        self.row_cards(H::search(cx), row)
    }
    /// Only a library item opens an item menu on a hold; a collection hit opens its page.
    fn card_has_menu(&self, cx: &Cx<'_, H>, _k: Sec, elem: &u32) -> bool {
        let key = FocusKey { entry: self.entry, elem: *elem };
        matches!(self.selected_item(Some(key), cx), Some(Item::Media(media)) if crate::registry::item_has_menu(media))
    }
    fn elem_of(&self, k: Sec) -> Option<u32> {
        match k { Sec::Head => Some(FIELD), Sec::Clear if !self.recents.is_empty() => Some(CLEAR), _ => None }
    }
    fn plain_len(&self, k: Sec) -> usize {
        match k { Sec::Recents => self.recents.len(), Sec::Head => 1, Sec::Clear => (!self.recents.is_empty()) as usize, Sec::Row(..) => 0 }
    }
    fn plain_elem(&self, k: Sec, n: usize) -> Option<u32> {
        match k { Sec::Recents => self.recents.get(n).copied(), _ => if n == 0 { StackPage::<H>::elem_of(self, k) } else { None } }
    }
    fn plain_step(&self, k: Sec, at: usize, dir: Dir) -> Option<usize> {
        match (k, dir) {
            (Sec::Recents, Dir::Up) => at.checked_sub(1),
            (Sec::Recents, Dir::Down) => at.checked_add(1),
            _ => None,
        }
    }
    fn focus_rect(&self, cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            Sec::Recents => layout::recent(0, layout::CONTENT_TOP - section.y),
            Sec::Clear => layout::clear_below(section.y, cx.measure),
            _ => Rect::new(layout::FIELD.x, layout::FIELD.y + section.y, layout::FIELD.w, layout::FIELD.h),
        }
    }
    fn element_rect(&self, cx: &Cx<'_, H>, k: Sec, n: usize, section: Rect) -> Rect {
        if k == Sec::Recents { layout::recent(n, layout::CONTENT_TOP - section.y) } else { self.focus_rect(cx, k, section) }
    }
    fn plain_group(&self, _cx: &Cx<'_, H>, k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        match k {
            Sec::Recents => group(id, GroupKind::Column, self.recents.len(), extent, ElemKind::Bare, Seat::Remembered),
            Sec::Clear => {
                let mut clear = group(id, GroupKind::Row { wrap: false }, 1, extent, ElemKind::Control, Seat::First);
                clear.edge[1] = EdgeRule::Stop;
                clear
            }
            _ => group(id, GroupKind::Row { wrap: false }, 1, extent, ElemKind::Bare, Seat::First),
        }
    }
    /// Entered from the field, a group takes the card focus last left on in it, else its first.
    fn seat_override(&self, cx: &Cx<'_, H>, k: Sec, _from: Placed) -> Option<u32> {
        if !cx.focus.current.is_some_and(|key| key.elem == FIELD) { return None; }
        let group = match k { Sec::Recents => RECENTS_GROUP, Sec::Clear => CLEAR_GROUP, Sec::Head => FIELD_GROUP, Sec::Row(kind, _) => layout::group(kind) };
        let valid = |elem: &u32| match k {
            Sec::Row(kind, _) => self.rows.iter().any(|row| row.kind == kind && row.elems.contains(elem)),
            _ => (0..StackPage::<H>::plain_len(self, k)).any(|n| StackPage::<H>::plain_elem(self, k, n) == Some(*elem)),
        };
        let first = match k {
            Sec::Row(kind, _) => self.rows.iter().find(|row| row.kind == kind).and_then(|row| row.elems.first().copied()),
            _ => StackPage::<H>::plain_elem(self, k, 0),
        };
        Some(cx.focus.remembered(group).filter(valid).or(first).unwrap_or(FIELD))
    }
    /// Recents and the Clear control are part of the head's block: the page rests at its top.
    fn reveal_with(&self, k: Sec) -> Option<Sec> { matches!(k, Sec::Recents | Sec::Clear).then_some(Sec::Head) }
    fn reveal_margin(&self, _k: Sec) -> f32 { layout::CONTENT_TOP }
    fn shelf_foot(&self, _k: Sec) -> f32 { plx_ui::consts::UNDER_LABEL_AIR }
    fn wide_extent(&self, _k: Sec) -> bool { false }
    /// A shown element that went away is replaced by the one now at its slot (the last of a
    /// shrunken group), else the field.
    fn recover(&self, _cx: &Cx<'_, H>, want: &u32) -> Option<u32> {
        let old = self.keys.iter().find(|key| key.elem == *want);
        let elems: &[u32] = match old {
            Some(old) if old.group == RECENTS_GROUP => &self.recents,
            Some(old) => self.rows.iter().find(|row| row.group == old.group).map_or(&[], |row| &row.elems),
            None => &[],
        };
        let slot = old.map_or(0, |old| old.slot);
        Some(plx_ui::cards::clamp_slot(slot, elems.len()).map_or(FIELD, |slot| elems[slot]))
    }
}

plx_ui::focusable_via_view!(SearchScreen, H: [SearchLike], view);

impl SearchScreen {
    /// The page's `Focusable` (see `focusable_via_view!` above): the stack's view over the page.
    fn view<H: SearchLike>(&self) -> impl plx_ui::screen::Focusable<H> + '_ {
        self.stack.view(self)
    }
}

impl<H: SearchLike> Screen<H> for SearchScreen {
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: Option<At>) -> Option<plx_ui::screen::FocusedCard<'a>> {
        let item = self.selected_item(focus, cx)?;
        let placed = focus.zip(at).and_then(|(key, at)| plx_ui::screen::Focusable::<H>::place(self, &key.elem, cx, at));
        Some(plx_ui::screen::FocusedCard::new(item, placed))
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>) {
        SearchScreen::redraw_focused::<H>(self, f, focus)
    }
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
        c.f32(self.hot.pos).f32(self.hot.vel);
        self.stack.write_scroll(c);
        c.u32(self.next_elem).u32(self.query_gen)
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
            if let Some(shelf) = self.stack.shelf(Sec::Row(row.kind, self.rows_gen)) { shelf.write(c); }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("search entry={} editing={} pending={} rows={} recents={} caret={} notices={}",
            self.entry.0, self.editing, self.draft.pending(), self.rows.len(), self.recents.len(), self.draft.caret(),
            self.notices));
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
