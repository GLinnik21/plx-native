//! Collection-page data model: one physically owned state machine, one generation-stamped
//! mailbox, and one fetch at a time. Resolution, header metadata, and paged children are
//! separate requests so a tag-only route can publish its resolved ratingKey before later requests
//! finish. The step starts nothing: it emits the next request as a [`StoreJob`](crate::stores::StoreJob)
//! (`CollectionReq`, with the [`ClientKey`] it decided on) and the application's runner starts it
//! on a worker, off the frame thread. It reads no registry either: which client a server slot
//! holds is an argument ([`ClientKeys`]), filled by the host immediately before the step.
//! Landings are applied by [`CollectionState::pump_with_gate`].

use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::collections::{resolve_tag, CollectionOutcome, CollectionRef};
use plx_plex::plex::{ClientKey, ClientKeys, ServerId};
use crate::pms::{parse_item, PmsMovie};
use crate::stores::page_cache::{Keep, PageCache, PAGE};
use std::ops::Range;
use std::panic::catch_unwind;
use std::sync::Arc;

pub const PAGE_SIZE: usize = PAGE;
const RETRY_FRAMES: u32 = 120;
/// Frontier requests one ask may cause. A restore without kept counts reads forward this many pages
/// at a time, so a deep restore never fires a burst of requests the screen did not wait for.
const FRONTIER_ASK: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionTarget {
    pub id: CollectionRef,
    /// Number of children the visible grid currently asks the store to make available.
    pub want: usize,
}

/// The order a collection lists its members in — its owner's `collectionSort`, which the page names
/// over its grid ("Items · Release order"). An order the server did not state is not guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CollectionOrder {
    Release,
    Title,
    Custom,
}

impl CollectionOrder {
    pub fn of(collection_sort: Option<i64>) -> Option<Self> {
        match collection_sort? {
            0 => Some(Self::Release),
            1 => Some(Self::Title),
            2 => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectionStatus {
    #[default]
    Loading,
    Ready,
    Empty,
    Unavailable,
    Failed,
}

pub struct Collection {
    /// The identity the page was opened with; `id.rk` is filled in once a tag route resolves.
    pub id: CollectionRef,
    pub title: String,
    pub summary: String,
    pub thumb: String,
    pub child_count: usize,
    /// The PMS `subtype` the header stated (`movie`, `show`, …), which names what the members are.
    pub subtype: Option<String>,
    /// The member order the header stated, if it stated one.
    pub order: Option<CollectionOrder>,
    /// The rows by absolute index. A hole (a page not read, or evicted) reads as `None`.
    pages: PageCache<PmsMovie>,
    /// Pages read in order so far, which is the server's page frontier. Their kept counts are
    /// known; a page at or past it is read only as the frontier.
    read: usize,
    pub total: usize,
    pub status: CollectionStatus,
    pub more: bool,
    /// The indices the screen shows or is about to show, from the last [`CollectionCmd::Window`]
    /// (or the `want` of an `Open`).
    wanted: Range<usize>,
    focus: Option<usize>,
    restore: Option<usize>,
    /// Frontier requests left before the next ask. An ask ([`CollectionCmd::Window`] or `Open`)
    /// refills it, so a restore without kept counts reads forward a bounded stretch at a time.
    budget: u8,
    header_ready: bool,
    /// The client the listing was read through, `None` when its server held none.
    client_key: Option<ClientKey>,
    /// The store's request epoch this listing was opened under (see [`Collection::generation`]).
    generation: u32,
}

impl Collection {
    /// A collection opened (or reopened) with nothing landed yet, under the store's request epoch
    /// `generation`: its title is the link's name until the header replaces it.
    fn loading(id: CollectionRef, want: usize, client_key: Option<ClientKey>, generation: u32) -> Self {
        Self { title: id.name.clone(), id, summary: String::new(), thumb: String::new(),
            child_count: 0, subtype: None, order: None, pages: PageCache::default(), read: 0, total: 0,
            status: CollectionStatus::Loading, more: true, wanted: 0..want, focus: None, restore: None,
            budget: FRONTIER_ASK, header_ready: false, client_key, generation }
    }

    /// Whether the header (title, art, summary) has landed — before it, an empty `thumb` is not
    /// yet an answer.
    pub fn header_ready(&self) -> bool { self.header_ready }

    /// The member at absolute index `i`, or `None` for a hole.
    pub fn item(&self, i: usize) -> Option<&PmsMovie> { self.pages.get(i) }

    /// How many indices are known: the kept rows of every page read so far. An evicted row still
    /// counts, so the index of every row the user has reached stays the same.
    pub fn shown(&self) -> usize { self.pages.first_index_of(self.read).min(self.total) }

    /// The index of the loaded row that is `(sid, rk)`, if its page is held. An evicted row has no
    /// index to give until its page is read again.
    pub fn position_of(&self, sid: ServerId, rk: &str) -> Option<usize> {
        self.pages.loaded_rows().find(|(_, m)| plx_plex::plex::same_item((m.sid, &m.rk), (sid, rk))).map(|(i, _)| i)
    }

    /// The loaded members inside the window the screen asked for (see [`CollectionCmd::Window`]).
    pub(crate) fn held_keys(&self, keys: &mut crate::stores::HeldKeys) {
        for (i, m) in self.pages.loaded_rows() {
            if self.wanted.contains(&i) { crate::stores::push_held(keys, m.sid, &m.rk); }
        }
    }

    /// Every row held, with its absolute index.
    pub fn loaded_rows(&self) -> impl Iterator<Item = (usize, &PmsMovie)> + '_ { self.pages.loaded_rows() }

    /// One kept-row count per page of the listing, for a screen to save with its position and its
    /// [`generation`](Self::generation) and send back as
    /// [`CollectionCmd::Restore`](crate::stores::collection::CollectionCmd::Restore).
    pub fn kept_counts(&self) -> Vec<u8> { self.pages.kept_counts() }

    /// Which listing this is, for a page to save with its [`kept_counts`](Self::kept_counts): the
    /// store's request epoch when the listing was opened. Only a store superseding its listing moves
    /// it (another collection opened, a close or reset, a rebuild for a changed client), never
    /// paging, so a page handing it back in [`CollectionCmd::Restore`](crate::stores::collection::CollectionCmd::Restore)
    /// says whether the store still holds the listing the page was saved from.
    pub fn generation(&self) -> u32 { self.generation }

    /// Pages the screen shows or is restoring to whose rows are not held, below the frontier.
    fn wanted_pages(&self) -> Vec<usize> {
        let mut pages: Vec<usize> = self.pages.missing(self.wanted.start, self.wanted.end).collect();
        for i in [self.focus, self.restore].into_iter().flatten() {
            if let Some((p, _)) = self.pages.locate(i) {
                if !self.pages.is_loaded(p) { pages.push(p); }
            }
        }
        pages
    }

    /// The count of leading indices the screen needs known.
    fn need(&self) -> usize {
        self.focus.into_iter().chain(self.restore).map(|i| i + 1).fold(self.wanted.end, usize::max)
    }

    /// Test support: moves the window and evicts as a landing would.
    #[cfg(any(test, feature = "test-support"))]
    pub fn evict_for_test(&mut self, wanted: Range<usize>, focus: Option<usize>, restore: Option<usize>) {
        (self.wanted, self.focus, self.restore) = (wanted.clone(), focus, restore);
        self.pages.evict(&Keep { wanted, focus, restore });
    }

    /// Test support: the pages a [`CollectionCmd::Window`](crate::stores::collection::CollectionCmd::Window)
    /// asks for (the window's, the focus's and the restore target's, if not held) land from `all`,
    /// as a server that honours paging would answer them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fill_wanted_for_test(&mut self, all: &[PmsMovie]) {
        for p in self.wanted_pages() {
            let rows = all.iter().skip(p * PAGE_SIZE).take(PAGE_SIZE).cloned().collect();
            self.pages.set_page(p, rows);
        }
    }

    /// Test support: every row, read through `edit`, and put back (a hole reads as absent).
    #[cfg(any(test, feature = "test-support"))]
    pub fn edit_items_for_test(&mut self, edit: impl FnOnce(&mut Vec<PmsMovie>)) {
        let mut items: Vec<PmsMovie> = (0..self.shown()).filter_map(|i| self.item(i).cloned()).collect();
        edit(&mut items);
        let more = self.more;
        self.replace_items_for_test(items);
        self.more = more;
    }

    /// Test support: replaces the listing with `items`, all of them read, as a server that honours
    /// paging would have returned them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_items_for_test(&mut self, items: Vec<PmsMovie>) {
        self.total = items.len();
        self.pages = PageCache::default();
        self.pages.set_total(self.total);
        self.read = 0;
        let mut rows = items.into_iter();
        for p in 0..self.total.div_ceil(PAGE_SIZE) {
            self.pages.set_page(p, rows.by_ref().take(PAGE_SIZE).collect());
            self.read = p + 1;
        }
        self.more = false;
    }
}

#[derive(Clone, Copy, Default)]
pub struct CollectionView<'a> { current: Option<&'a Collection>, revision: u64 }

impl<'a> CollectionView<'a> {
    pub fn current(self) -> Option<&'a Collection> { self.current }
    /// The content counter: bumped whenever the collection's items, summary, status or `more`
    /// change (or the collection itself is replaced). A reader derives its per-member state again
    /// when this moves; the request epoch (`generation`) does not move on paging, and an item
    /// count or summary length can stay put while the content behind it changes.
    pub fn revision(self) -> u64 { self.revision }
}

pub struct CollectionState {
    /// The publish order of view-state edits, shared by `stores::Stores`.
    pub(crate) ledger: crate::viewledger::ViewLedger,
    /// Where in that order the read now out (single flight) was issued; its landing is made again
    /// with every edit published since.
    stamp: crate::viewledger::Stamp,
    current: Option<Collection>,
    generation: u32,
    /// The content counter [`CollectionView::revision`] reads.
    revision: u64,
    retry_cd: u32,
    /// The requests the step asked the application to start, in order, until the owner takes them
    /// (`CollectionStore::take_jobs`).
    pub(crate) jobs: Vec<crate::stores::StoreJob>,
}

impl Default for CollectionState {
    fn default() -> Self {
        Self { ledger: Default::default(), stamp: 0, current: None, generation: 0, revision: 0, retry_cd: 0,
            jobs: Vec::new() }
    }
}

impl CollectionState {
    pub fn view(&self) -> CollectionView<'_> {
        CollectionView { current: self.current.as_ref(), revision: self.revision }
    }

    fn supersede(&mut self, adapter: &CollectionAdapter) {
        self.generation = self.generation.checked_add(1).expect("collection generation exhausted");
        adapter.fetch.clear();
        self.retry_cd = 0;
    }

    /// Run a command. `keys` is the registry's answer as of this call (see [`ClientKeys`]): an
    /// `Open` records the client its listing is read through.
    pub fn run(&mut self, adapter: &Arc<CollectionAdapter>, cmd: crate::stores::collection::CollectionCmd,
               keys: &ClientKeys) -> bool {
        let changed = self.run_cmd(adapter, cmd, keys);
        self.revision += u64::from(changed);
        changed
    }

    fn run_cmd(&mut self, adapter: &Arc<CollectionAdapter>, cmd: crate::stores::collection::CollectionCmd,
               keys: &ClientKeys) -> bool {
        use crate::stores::collection::CollectionCmd;
        match cmd {
            CollectionCmd::Open { target } => {
                if let Some(current) = self.current.as_mut().filter(|c| c.id.same_collection(&target.id)) {
                    let old = current.wanted.end;
                    current.wanted.end = current.wanted.end.max(target.want);
                    current.budget = FRONTIER_ASK;
                    let retry = current.status == CollectionStatus::Failed;
                    if retry {
                        current.status = CollectionStatus::Loading;
                        self.retry_cd = 0;
                    }
                    return current.wanted.end != old || retry;
                }
                self.supersede(adapter);
                let client_key = keys.key(target.id.sid);
                self.current = Some(Collection::loading(target.id, target.want.max(PAGE_SIZE), client_key, self.generation));
                true
            }
            CollectionCmd::Window { wanted, focus, restore } => {
                let Some(c) = self.current.as_mut() else { return false };
                let changed = c.wanted != wanted || c.focus != focus || c.restore != restore;
                (c.wanted, c.focus, c.restore) = (wanted, focus, restore);
                c.budget = FRONTIER_ASK;
                changed
            }
            CollectionCmd::Restore { total, counts, generation } => {
                // The counts describe a listing of `total` items, one entry per page; any other
                // shape is not this listing's directory and is not guessed at.
                let Some(c) = self.current.as_mut() else { return false };
                if counts.len() != total.div_ceil(PAGE_SIZE) { return false; }
                // A page whose surface was closed over it (a menu, a sheet, Back from Detail) is
                // handed the directory it saved while this store still holds the listing it was
                // saved from: the same generation, however much has been read since (a page landed
                // while covered, a moved total). Nothing is to be restored: the rows are held, and
                // the store's own counts, read position and paging stand. Equal counts would not
                // prove it, for a store rebuilt since (a retoken, a server re-point) reads the same
                // numbers off the same server; a different generation takes the saved directory.
                if c.generation == generation { return false; }
                c.total = total;
                c.pages.set_total(total);
                c.pages.restore_kept_counts(&counts);
                c.read = counts.len();
                c.more = false;
                c.status = if c.shown() > 0 { CollectionStatus::Ready } else { c.status };
                true
            }
            CollectionCmd::Close | CollectionCmd::Reset => {
                self.supersede(adapter);
                self.current.take().is_some()
            }
            // The item menu's Mark watched/unwatched reaches every store that can be drawing the
            // item (`viewstate`'s fan-out); a member's disc must flip on this page too, not one
            // refetch later.
            CollectionCmd::ApplyView { at, edit } => {
                // A member has no deck to leave.
                if let crate::viewstate::ViewEdit::LeftDeck = edit { return false }
                let Some(c) = self.current.as_mut() else { return false };
                let mut hit = false;
                for i in 0..c.shown() {
                    c.pages.edit(i, |item| {
                        // `changes`: the server's `Facts`/`Offset` that agree touch nothing
                        if at.covers(item) && edit.changes(item) {
                            edit.apply(item);
                            hit = true;
                        }
                    });
                }
                hit
            }
        }
    }

    fn refresh_if_client_changed(&mut self, adapter: &CollectionAdapter, keys: &ClientKeys) -> bool {
        let Some(c) = self.current.as_ref() else { return false };
        let now = keys.key(c.id.sid);
        if now == c.client_key { return false; }
        let (id, want) = (c.id.clone(), c.wanted.end);
        let (window, focus, restore) = (c.wanted.clone(), c.focus, c.restore);
        self.supersede(adapter);
        let mut fresh = Collection::loading(id, want, now, self.generation);
        (fresh.wanted, fresh.focus, fresh.restore) = (window, focus, restore);
        self.current = Some(fresh);
        self.revision += 1;
        true
    }

    /// The pump: rebuild for a changed client, take the landing, and emit the next request as a job.
    /// `keys` is the registry's answer as of this call. The job is NOT started here; the owner
    /// takes it (`CollectionStore::take_jobs`) and owes it to a runner in the same turn.
    pub fn pump_with_gate(&mut self, adapter: &Arc<CollectionAdapter>, gate: &plx_machine::landgate::Gate,
                          keys: &ClientKeys) -> bool {
        let mut changed = self.refresh_if_client_changed(adapter, keys);
        if self.retry_cd > 0 { self.retry_cd -= 1; }
        let reply = crate::stores::tape::take_store_landing(
            gate, crate::stores::StoreId::Collection, "collection", 0, &adapter.fetch, |m| m.gen);
        if let Some(reply) = reply {
            plx_machine::idle::invalidate();
            if reply.gen == self.generation { changed |= self.apply(reply.what); }
        }
        self.maybe_spawn(adapter, keys);
        changed
    }

    fn work(&self) -> Option<Work> {
        let c = self.current.as_ref()?;
        if c.status == CollectionStatus::Unavailable || c.status == CollectionStatus::Empty { return None; }
        if c.id.rk.is_empty() {
            return (c.id.sec != 0 && c.id.tag != 0).then(|| Work::Resolve {
                sec: c.id.sec, tag: c.id.tag, name: c.id.name.clone(),
            });
        }
        if !c.header_ready { return Some(Work::Header { rk: c.id.rk.clone() }); }
        // A page the screen shows or restores to, read earlier and since evicted: a re-read at its
        // own offset, which the directory's known count makes safe.
        if let Some(page) = c.wanted_pages().into_iter().find(|&p| p < c.read) {
            return Some(Work::Children { rk: c.id.rk.clone(), start: page * PAGE_SIZE });
        }
        // Otherwise the frontier, in order, while the window needs indices it has not counted.
        if c.more && c.budget > 0 && c.need() > c.shown() {
            return Some(Work::Children { rk: c.id.rk.clone(), start: c.read * PAGE_SIZE });
        }
        None
    }

    /// The next request, if the window needs one and nothing is out: claim the fetch and emit the
    /// request as a job. A server that holds no client has nothing to ask, so the page reads
    /// Failed (when it shows nothing yet) and the next attempt is held off.
    fn maybe_spawn(&mut self, adapter: &Arc<CollectionAdapter>, keys: &ClientKeys) {
        if adapter.fetch.busy() || self.retry_cd > 0 { return; }
        let Some(work) = self.work() else { return };
        let Some(c) = self.current.as_ref() else { return };
        let sid = c.id.sid;
        let Some(key) = keys.key(sid) else {
            self.retry_cd = RETRY_FRAMES;
            if c.shown() == 0 {
                self.current.as_mut().unwrap().status = CollectionStatus::Failed;
                self.revision += 1;
            }
            return;
        };
        let generation = self.generation;
        adapter.fetch.claim(generation);
        self.stamp = self.ledger.stamp();
        self.jobs.push(crate::stores::StoreJob::Collection(
            Job { addr: collection_addr(generation), req: CollectionReq { sid, key, work } },
            CollectionPort::of(adapter),
        ));
    }

    fn apply(&mut self, landing: Landing) -> bool {
        let changed = self.apply_landing(landing);
        self.revision += u64::from(changed);
        changed
    }

    fn apply_landing(&mut self, landing: Landing) -> bool {
        let (ledger, stamp) = (self.ledger.clone(), self.stamp);
        let Some(c) = self.current.as_mut() else { return false };
        match landing {
            Landing::Header { rk, head } => {
                // A tag route's resolution arrives with the row it was resolved from, which IS the
                // header: no second GET for the same fields.
                if let Some(rk) = rk { c.id.rk = rk; }
                if !head.title.is_empty() { c.title = head.title; }
                c.thumb = head.thumb;
                c.summary = head.summary;
                c.child_count = head.child_count;
                c.subtype = head.subtype;
                c.order = head.order;
                c.header_ready = true;
                c.status = CollectionStatus::Loading;
                true
            }
            Landing::Page { start, got, mut pages, total } => {
                // The read describes the server from when it was issued: edits published since
                // are made again on its rows (`crate::viewledger`).
                ledger.patch(stamp, pages.iter_mut().flatten());
                // A page is read at its own offset: the frontier, or a re-read of a page whose
                // count is known. Anything past the frontier was never asked for in order.
                if start % PAGE_SIZE != 0 { return false; }
                // A server that ignores paging answers the whole listing from its first row, so
                // its chunks start at 0 whatever offset was asked (see `run_job`).
                let base = if got > PAGE_SIZE { 0 } else { start / PAGE_SIZE };
                if base > c.read { return false; }
                // A re-read is asked only for a hole, so an answer for a page that still holds its
                // rows is stale. A whole-listing answer is not one page and always lands.
                if got <= PAGE_SIZE && base < c.read && c.pages.is_loaded(base) { return false; }
                let before = c.read;
                c.total = total.max(base * PAGE_SIZE + got);
                c.pages.set_total(c.total);
                let chunks = pages.len();
                for (k, rows) in pages.into_iter().enumerate() {
                    c.pages.set_page(base + k, rows);
                }
                c.read = c.read.max(base + chunks).min(c.pages.page_count());
                if c.read > before {
                    c.budget = c.budget.saturating_sub(1);
                }
                if base >= before {
                    // A page of zero rows ends the listing whatever `totalSize` claimed, or a server
                    // that over-reports would be asked for the same empty offset forever.
                    c.more = got > 0 && c.read * PAGE_SIZE < c.total;
                }
                c.pages.evict(&Keep { wanted: c.wanted.clone(), focus: c.focus, restore: c.restore });
                c.status = if c.shown() > 0 { CollectionStatus::Ready }
                    else if c.more { CollectionStatus::Loading }
                    else { CollectionStatus::Empty };
                true
            }
            Landing::Denied | Landing::Missing => {
                c.status = CollectionStatus::Unavailable;
                c.more = false;
                true
            }
            Landing::Transport => {
                self.retry_cd = RETRY_FRAMES;
                if c.shown() == 0 { c.status = CollectionStatus::Failed; }
                true
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn generation(&self) -> u32 { self.generation }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, items: Vec<PmsMovie>, status: CollectionStatus) {
        let Some(c) = self.current.as_mut() else { return };
        self.revision += 1;
        c.title = if c.id.name.is_empty() { "Collection".into() } else { c.id.name.clone() };
        c.summary = "A collection summary long enough for screen layout tests.".into();
        c.child_count = items.len();
        c.replace_items_for_test(items);
        c.header_ready = true;
        c.more = false;
        c.status = status;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn edit_for_test(&mut self, edit: impl FnOnce(&mut Collection)) {
        if let Some(c) = self.current.as_mut() { edit(c); self.revision += 1; }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn take_landing_for_test(&mut self, adapter: &CollectionAdapter) -> bool {
        let Some(mail) = adapter.fetch.take() else { return false };
        mail.gen == self.generation && self.apply(mail.what)
    }
}

/// What one request asks the server for. Serialized into the tape's admission record
/// (`CollectionReq::admission`), where the externally tagged variant names are part of the
/// recorded bytes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
enum Work {
    Resolve { sec: i64, tag: i64, name: String },
    Header { rk: String },
    Children { rk: String, start: usize },
}

/// The one request a [`StoreJob::Collection`](crate::stores::StoreJob::Collection) carries:
/// everything the worker needs, captured by the step. `key` is the client the step decided on; a
/// runner starts the request only if the registry still holds that client at the launch.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionReq {
    sid: ServerId,
    key: ClientKey,
    work: Work,
}

impl CollectionReq {
    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) { (self.sid, self.key) }

    /// The tape's admission record for this request, a pure function of the job: the shape a
    /// recording holds and a replay compares byte for byte (`stores::tape::admit`).
    pub fn admission(&self, addr: Addr) -> serde_json::Value {
        serde_json::json!({"store":"collection","slot":0,"gen":addr.req.0,
            "sid":self.sid.raw(),"client":self.key.instance,"job":self.work})
    }
}

/// A request's answer, opaque to the runner: the landing the step applies. The runner never
/// names [`Landing`].
pub struct CollectionAnswer(Landing);

/// Where a collection request's ONE answer goes: the store's landing, captured with the job when it
/// was emitted. A store that has since been `Reset` holds a new adapter, and this port still lands
/// into the one the request was made from, the retired one nobody pumps. Two ports are equal when
/// they reach the same adapter.
#[derive(Clone)]
pub struct CollectionPort(Arc<CollectionAdapter>);

impl CollectionPort {
    pub(crate) fn of(adapter: &Arc<CollectionAdapter>) -> Self { Self(Arc::clone(adapter)) }
}

impl PartialEq for CollectionPort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}

impl std::fmt::Debug for CollectionPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CollectionPort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<CollectionAnswer> for CollectionPort {
    /// The request's answer (a failed read answers `Transport`). Stamped with the request's
    /// generation, so the pump discards an answer for a listing it has since superseded.
    fn done(&self, addr: Addr, out: CollectionAnswer) {
        self.0.land(addr.req.0, out.0);
    }

    /// The request was never started, so nothing will land and nothing will release its claim:
    /// hand it back, so the next pump asks again. A refusal that comes after a newer request has
    /// claimed leaves that claim alone ([`Fetch::release_for`](crate::stores::Fetch::release_for)).
    fn refused(&self, addr: Addr) {
        self.0.fetch.release_for(addr.req.0);
    }
}

/// A collection request's address: its store and the listing generation it was made for. One
/// request is out at a time (`maybe_spawn`'s claim), so no two are in flight at one address.
fn collection_addr(generation: u32) -> Addr {
    Addr { to: MachineId::Store(crate::stores::StoreId::Collection.ord()), req: RequestId(generation) }
}

/// The request's IO body and its completion: read the server through `client` and post the answer,
/// or its failure, to `port`. A read that unwinds still answers (`Transport`), so the claim is
/// always released by the take of its answer.
pub fn finish_collection_job(port: &CollectionPort, addr: Addr, client: &'static plx_plex::plex::Client,
                             req: CollectionReq) {
    let CollectionReq { sid, work, .. } = req;
    let what = catch_unwind(|| run_job(client, sid, work)).unwrap_or(Landing::Transport);
    port.done(addr, CollectionAnswer(what));
}

/// A collection row's header fields, as both the tag resolution and the metadata GET read them.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Header {
    title: String,
    thumb: String,
    summary: String,
    child_count: usize,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    order: Option<CollectionOrder>,
}

impl Header {
    fn of(row: &plx_plex::plex::Metadata) -> Self {
        Self { title: row.title.clone(), thumb: row.thumb.clone(), summary: row.summary.clone(),
            child_count: row.child_count.max(0) as usize, subtype: Some(row.subtype.clone()).filter(|s| !s.is_empty()),
            order: CollectionOrder::of(row.collection_sort) }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum Landing {
    /// The collection's header; `rk` is `Some` when it answers a tag route's resolution.
    Header { rk: Option<String>, head: Header },
    /// `got` is the server rows the response consumed. `pages[k]` holds the listable rows of server
    /// page `k` of the response (the first `PAGE_SIZE` rows, the next, and so on), so an answer
    /// that ignores paging lands as every page it covers.
    Page { start: usize, got: usize, pages: Vec<Vec<PmsMovie>>, total: usize },
    Denied,
    Missing,
    Transport,
}

/// One collection member as a PORTRAIT grid cell. `parse_item` gives an episode its show's poster;
/// a collection that holds episodes of several seasons reads better on the SEASON's poster, so an
/// episode takes `parentThumb` first and falls back to the show's. Its own 16:9 still is never
/// the portrait art — with both posters absent the cell draws the neutral placeholder rather
/// than a cropped landscape frame.
pub fn member(row: &plx_plex::plex::Metadata, sid: ServerId) -> PmsMovie {
    let mut item = parse_item(row, sid);
    if item.kind == 3 {
        item.thumb = if !row.parent_thumb.is_empty() { row.parent_thumb.clone() }
            else { row.grandparent_thumb.clone() };
    }
    item
}

/// A read's page, or the landing its failure is: the one mapping of the server's non-page answers.
fn answered(outcome: CollectionOutcome) -> Result<plx_plex::plex::MediaContainer, Landing> {
    match outcome {
        CollectionOutcome::Ok(page) => Ok(page),
        CollectionOutcome::Denied => Err(Landing::Denied),
        CollectionOutcome::Missing => Err(Landing::Missing),
        CollectionOutcome::Transport => Err(Landing::Transport),
    }
}

fn run_job(client: &'static plx_plex::plex::Client, sid: ServerId, work: Work) -> Landing {
    let run = || -> Result<Landing, Landing> {
        Ok(match work {
            Work::Resolve { sec, tag, name } => {
                let mut start = 0i64;
                loop {
                    let page = answered(client.section_collections(sec, start, PAGE_SIZE as i64))?;
                    if let Some(row) = resolve_tag(&page.metadata, tag, &name) {
                        break Landing::Header { rk: Some(row.rating_key.clone()), head: Header::of(row) };
                    }
                    let got = page.metadata.len() as i64;
                    let total = page.total_size.max(page.size).max(0);
                    if got == 0 || start + got >= total { break Landing::Missing; }
                    start += got;
                }
            }
            Work::Header { rk } => answered(client.collection(&rk))?.metadata.first()
                .map_or(Landing::Missing, |row| Landing::Header { rk: None, head: Header::of(row) }),
            Work::Children { rk, start } => {
                let page = answered(client.collection_children(&rk, start as i64, PAGE_SIZE as i64))?;
                let total = page.total_size.max(page.size).max(0) as usize;
                let got = page.metadata.len();
                let pages = page.metadata.chunks(PAGE_SIZE).map(|rows| rows.iter()
                    .filter(|row| crate::pms::listable(&row.kind)).map(|row| member(row, sid)).collect())
                    .collect();
                Landing::Page { start, got, pages, total }
            }
        })
    };
    run().unwrap_or_else(|failed| failed)
}

#[cfg(any(test, feature = "test-support"))]
fn header(title: &str, child_count: usize) -> Header {
    Header { title: title.into(), thumb: String::new(), summary: String::new(), child_count, subtype: None, order: None }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Mail { gen: u32, what: Landing }

pub fn validate_record(slot: u32, value: &serde_json::Value) -> Result<(), &'static str> {
    if slot != 0 { return Err("invalid collection slot"); }
    let mail: Mail = serde_json::from_value(value.clone()).map_err(|_| "invalid collection reply")?;
    if mail.gen == 0 { return Err("invalid collection reply generation"); }
    if serde_json::to_value(&mail).ok().as_ref() != Some(value) && reread(&mail).as_ref() != Some(value) { return Err("noncanonical collection reply"); }
    Ok(())
}

#[derive(Default)]
pub struct CollectionAdapter { fetch: crate::stores::Fetch<Mail> }

impl CollectionAdapter {
    fn land(&self, generation: u32, what: Landing) {
        self.fetch.post(Mail { gen: generation, what }, |old| old.gen < generation);
    }

    /// Is a request's claim held? (A test's view of the single flight.)
    #[cfg(any(test, feature = "test-support"))]
    pub fn busy_for_test(&self) -> bool { self.fetch.busy() }

    /// Is an answer waiting to be taken? (A test's view of the mailbox, without taking it.)
    #[cfg(any(test, feature = "test-support"))]
    pub fn has_mail_for_test(&self) -> bool { self.fetch.has_mail() }

    #[cfg(any(test, feature = "test-support"))]
    pub fn land_status_for_test(&self, generation: u32, status: CollectionStatus) {
        let what = match status {
            CollectionStatus::Empty => Landing::Page { start: 0, got: 0, pages: vec![], total: 0 },
            CollectionStatus::Unavailable => Landing::Denied,
            CollectionStatus::Failed => Landing::Transport,
            _ => Landing::Missing,
        };
        self.land(generation, what);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn land_resolved_for_test(&self, generation: u32, rk: &str) {
        self.land(generation, Landing::Header { rk: Some(rk.into()), head: header("Resolved", 0) });
    }


    #[cfg(any(test, feature = "test-support"))]
    pub fn land_missing_for_test(&self, generation: u32) {
        self.land(generation, Landing::Missing);
    }
}


#[cfg(test)]
#[path = "collection_job_tests.rs"]
mod job_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::collection::CollectionCmd;

    impl CollectionState {
        /// A command run when no server holds a client: what the registry answers for the
        /// `ServerId::UNSET` these tests name, so an `Open` records no client.
        fn run_bare(&mut self, adapter: &Arc<CollectionAdapter>, cmd: CollectionCmd) -> bool {
            self.run(adapter, cmd, &ClientKeys::default())
        }
    }

    fn set_target(rk: &str, tag: i64, name: &str) -> CollectionTarget {
        CollectionTarget { id: CollectionRef { sid: ServerId::UNSET, rk: rk.into(), sec: 1, tag,
            name: name.into() }, want: PAGE_SIZE }
    }

    fn row(kind: &str, thumb: &str, parent: &str, grandparent: &str) -> plx_plex::plex::Metadata {
        plx_plex::plex::Metadata { rating_key: "9".into(), kind: kind.into(), title: "Pilot".into(),
            thumb: thumb.into(), parent_thumb: parent.into(), grandparent_thumb: grandparent.into(),
            parent_index: Some(3), index: 4, grandparent_title: "Show".into(), ..Default::default() }
    }

    /// The page names the member order the collection's owner chose, from the header's
    /// `collectionSort` (string-encoded, like every PMS number); an unstated or unknown order is
    /// not guessed.
    #[test]
    fn the_header_reads_the_collections_member_order() {
        let head = |body: &str| {
            let row: plx_plex::plex::Metadata = serde_json::from_str(body).expect("parses");
            Header::of(&row).order
        };
        assert_eq!(head(r#"{"title":"Saga","collectionSort":"0"}"#), Some(CollectionOrder::Release));
        assert_eq!(head(r#"{"title":"Saga","collectionSort":1}"#), Some(CollectionOrder::Title));
        assert_eq!(head(r#"{"title":"Saga","collectionSort":"2"}"#), Some(CollectionOrder::Custom));
        assert_eq!(head(r#"{"title":"Saga"}"#), None);
        assert_eq!(head(r#"{"title":"Saga","collectionSort":"9"}"#), None);
    }

    #[test]
    fn an_episode_member_wears_its_season_poster_then_its_show_poster_never_its_still() {
        let sid = ServerId::UNSET;
        let m = member(&row("episode", "/still", "/season", "/show"), sid);
        assert_eq!((m.kind, m.thumb.as_str()), (3, "/season"));
        assert_eq!((m.season_index, m.ep_index, m.show_title.as_str()), (3, 4, "Show"));
        assert_eq!(member(&row("episode", "/still", "", "/show"), sid).thumb, "/show");
        assert_eq!(member(&row("episode", "/still", "", ""), sid).thumb, "",
            "a landscape still is never cropped into a portrait cell");
        assert_eq!(member(&row("movie", "/poster", "", ""), sid).thumb, "/poster");
    }

    /// `generation` is the request epoch (paging leaves it); `revision` is the content counter
    /// (a header and a page both move it, a dropped landing does not).
    #[test]
    fn the_revision_follows_content_while_the_generation_follows_requests() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let (generation, opened) = (state.generation(), state.view().revision());
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        assert!(state.take_landing_for_test(&adapter));
        let header_rev = state.view().revision();
        assert!(header_rev > opened, "a header landing is a content change");
        let page = (0..PAGE_SIZE).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, pages: vec![page], total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        let paged_rev = state.view().revision();
        assert!(paged_rev > header_rev, "a page landing is a content change");
        assert_eq!(state.generation(), generation, "…while paging leaves the request epoch alone");
        adapter.land(generation, Landing::Page { start: 0, got: 0, pages: vec![], total: 130 });
        assert!(!state.take_landing_for_test(&adapter));
        assert_eq!(state.view().revision(), paged_rev, "a dropped landing changes nothing");
    }

    /// DUMP MODE at the Collection site, through the real pump: a request that is out lands on the
    /// pump that runs, however late the worker posts (25 ms here). A plain take (the conversion
    /// reverted) returns empty and the first assertion fails. A superseded request is the next test.
    #[test]
    fn dump_mode_a_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let generation = state.generation();
        adapter.fetch.claim(generation);
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_secs(20));
        let worker = Arc::clone(&adapter);
        let join = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            worker.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        });
        assert!(state.pump_with_gate(&adapter, &gate, &ClientKeys::default()),
            "the header the pump owed must be taken by the pump that asked");
        assert!(state.view().current().unwrap().header_ready);
        assert!(!adapter.fetch.busy(), "the TAKE released the claim");
        join.join().unwrap();
    }

    /// A superseded request's stale answer arrives FIRST: it must neither end the dump wait nor
    /// release the NEW request's claim, so the new answer lands on the pump that was owed it.
    #[test]
    fn dump_mode_a_superseded_requests_stale_answer_does_not_release_the_new_claim() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let stale = state.generation();
        adapter.fetch.claim(stale); // request A is out
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50002", 7, "Other") }); // supersedes A
        let current = state.generation();
        assert_ne!(stale, current);
        adapter.fetch.claim(current); // request B is out
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_secs(20));
        let worker = Arc::clone(&adapter);
        let join = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            worker.land(stale, Landing::Header { rk: None, head: header("Set", 130) });
            std::thread::sleep(std::time::Duration::from_millis(25));
            worker.land(current, Landing::Header { rk: None, head: header("Other", 7) });
        });
        assert!(state.pump_with_gate(&adapter, &gate, &ClientKeys::default()),
            "B was out when the pump ran, so B's answer must land on this pump");
        assert!(state.view().current().unwrap().header_ready);
        assert!(!adapter.fetch.busy(), "B's own answer released B's claim");
        join.join().unwrap();
    }

    #[test]
    fn paging_asks_for_the_next_page_only_when_the_grid_wants_more() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        let target = set_target("50001", 7, "Set");
        state.run_bare(&adapter, CollectionCmd::Open { target: target.clone() });
        assert!(matches!(state.work(), Some(Work::Header { .. })));
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        assert!(state.take_landing_for_test(&adapter));
        assert!(matches!(state.work(), Some(Work::Children { start: 0, .. })));
        let page = (0..PAGE_SIZE).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, pages: vec![page], total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.shown(), c.more, c.status), (PAGE_SIZE, true, CollectionStatus::Ready));
        assert!(state.work().is_none(), "the grid has not asked for more yet");

        let mut more = target;
        more.want = 2 * PAGE_SIZE;
        assert!(state.run_bare(&adapter, CollectionCmd::Open { target: more }),
            "a larger want on the same identity is a change, not a reopen");
        assert_eq!(state.generation(), generation, "paging does not supersede the collection");
        assert!(matches!(state.work(), Some(Work::Children { start, .. }) if start == PAGE_SIZE));
        adapter.land(generation, Landing::Page { start: 5 * PAGE_SIZE, got: 0, pages: vec![], total: 130 });
        assert!(!state.take_landing_for_test(&adapter), "a page past the frontier is dropped");
    }

    /// Regression: paging used `items.len()` as the next server offset, so a page whose rows were
    /// not all `listable` (a clip in a mixed collection) re-requested rows it had already
    /// consumed and appended them twice. The offset is the server's, not the grid's.
    #[test]
    fn a_page_with_unlisted_rows_advances_by_the_rows_the_server_sent() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        assert!(state.take_landing_for_test(&adapter));
        let listed = (0..PAGE_SIZE - 5).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, pages: vec![listed], total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        state.run_bare(&adapter, CollectionCmd::Open { target: CollectionTarget {
            want: 2 * PAGE_SIZE, ..set_target("50001", 7, "Set") } });
        assert!(matches!(state.work(), Some(Work::Children { start, .. }) if start == PAGE_SIZE),
            "the next page starts after every row the server sent, listed or not");

        // A collection whose every row is unlisted ends Empty rather than paging forever.
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50002", 8, "Clips") });
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Clips", 3) });
        assert!(state.take_landing_for_test(&adapter));
        adapter.land(generation, Landing::Page { start: 0, got: 3, pages: vec![vec![]], total: 3 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.status, c.more), (CollectionStatus::Empty, false));
        assert!(state.work().is_none());
    }

    /// A tag route resolves from the section's collection listing, whose row IS the collection's
    /// header — the resolution lands it, so the next job is the first page, not a second GET of
    /// `/library/metadata/{rk}` for the same fields.
    #[test]
    fn a_tag_resolution_lands_the_header_and_pages_next() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("", 7, "Set") });
        assert!(matches!(state.work(), Some(Work::Resolve { tag: 7, .. })));
        adapter.land_resolved_for_test(state.generation(), "50007");
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.id.rk.as_str(), c.title.as_str()), ("50007", "Resolved"));
        assert!(matches!(state.work(), Some(Work::Children { start: 0, .. })), "no second header request");
    }

    #[test]
    fn a_local_watched_edit_flips_the_member_in_place() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        state.install_for_test(vec![PmsMovie { rk: "a".into(), unwatched: true, ..Default::default() }],
            CollectionStatus::Ready);
        assert!(!state.run_bare(&adapter, CollectionCmd::ApplyView {
            at: crate::viewstate::ViewAt::leaf(ServerId::UNSET, "b"),
            edit: crate::viewstate::ViewEdit::Watched(true) }), "an item not on the page changes nothing");
        assert!(state.run_bare(&adapter, CollectionCmd::ApplyView {
            at: crate::viewstate::ViewAt::leaf(ServerId::UNSET, "a"),
            edit: crate::viewstate::ViewEdit::Watched(true) }));
        let item = &state.view().current().unwrap().item(0).unwrap();
        assert!(item.watched && !item.unwatched);
    }

    #[test]
    fn the_servers_facts_and_offset_patch_a_member_only_where_they_differ() {
        use crate::viewstate::{ViewAt, ViewEdit};
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        state.install_for_test(vec![
            PmsMovie { rk: "a".into(), unwatched: true, resume_ms: 4, ..Default::default() },
            PmsMovie { rk: "b".into(), unwatched: true, ..Default::default() },
        ], CollectionStatus::Ready);
        let mut run = |rk: &str, edit| state.run_bare(&adapter, CollectionCmd::ApplyView {
            at: ViewAt::leaf(ServerId::UNSET, rk), edit });
        let facts = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0 };
        assert!(run("a", facts));
        assert!(!run("a", facts), "facts that agree with the member change nothing");
        assert!(run("b", ViewEdit::Offset { ms: 55 }));
        assert!(!run("b", ViewEdit::Offset { ms: 55 }));
        let c = state.view().current().unwrap();
        let (a, b) = (c.item(0).unwrap(), c.item(1).unwrap());
        assert_eq!((a.watched, a.unwatched, a.resume_ms), (true, false, 0));
        assert_eq!((b.watched, b.unwatched, b.resume_ms), (false, true, 55), "an offset leaves the watched state alone");
    }

    /// A collection opened and its header landed, reading nothing yet.
    fn opened(total: usize) -> (CollectionState, Arc<CollectionAdapter>) {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run_bare(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        adapter.land(state.generation(), Landing::Header { rk: None, head: header("Set", total) });
        assert!(state.take_landing_for_test(&adapter));
        (state, adapter)
    }

    /// The collection closed and opened again, as a page entered anew does it: the store supersedes
    /// the listing it held and holds nothing of the new one yet but its header.
    fn reopen(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize) {
        state.run_bare(adapter, CollectionCmd::Close);
        state.run_bare(adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        adapter.land(state.generation(), Landing::Header { rk: None, head: header("Set", total) });
        assert!(state.take_landing_for_test(adapter));
    }

    /// A retoken or a server re-point while a surface covers the page: the pump rebuilds the listing
    /// from its first page (`refresh_if_client_changed`), keeping the window the page asked for. The
    /// listing was read through client `(1, 1)` and the keys now answer `(2, 1)` for its server.
    fn rebuild(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize) {
        state.current.as_mut().unwrap().client_key = Some(ClientKey { instance: 1, token: 1 });
        let now = ClientKeys::from_keys([(ServerId::UNSET, ClientKey { instance: 2, token: 1 })]);
        assert!(state.refresh_if_client_changed(adapter, &now));
        adapter.land(state.generation(), Landing::Header { rk: None, head: header("Set", total) });
        assert!(state.take_landing_for_test(adapter));
    }

    /// What a page saves of the listing it reads (`kept_counts` and `generation`) and hands back in a
    /// [`CollectionCmd::Restore`].
    fn directory(state: &CollectionState) -> (usize, Vec<u8>, u32) {
        let c = state.view().current().unwrap();
        (c.total, c.kept_counts(), c.generation())
    }

    fn movie(rk: usize) -> PmsMovie {
        PmsMovie { rk: rk.to_string(), ..Default::default() }
    }

    /// Answers every request the store makes, as a server that honours paging: server row `s` is
    /// `rk` `s`, and a row `keep` rejects is filtered out of its page. Records each request's start.
    fn serve(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize,
             keep: &dyn Fn(usize) -> bool, requests: &mut Vec<usize>) {
        while let Some(Work::Children { start, .. }) = state.work() {
            requests.push(start);
            let got = PAGE_SIZE.min(total - start);
            let rows = (start..start + got).filter(|&s| keep(s)).map(movie).collect();
            adapter.land(state.generation(), Landing::Page { start, got, pages: vec![rows], total });
            assert!(state.take_landing_for_test(adapter));
        }
    }

    /// Moves the window to `wanted`, asks for what it needs, and returns the requests made.
    fn show(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize,
            wanted: Range<usize>, focus: Option<usize>, restore: Option<usize>, keep: &dyn Fn(usize) -> bool) -> Vec<usize> {
        state.run_bare(adapter, CollectionCmd::Window { wanted, focus, restore });
        let mut requests = Vec::new();
        serve(state, adapter, total, keep, &mut requests);
        requests
    }

    fn rk_at(state: &CollectionState, i: usize) -> Option<String> {
        state.view().current().unwrap().item(i).map(|m| m.rk.clone())
    }

    fn loaded(state: &CollectionState) -> usize {
        state.view().current().unwrap().pages.loaded_pages()
    }

    /// Walks a `total`-item listing from the top to its end one 60-row window at a time.
    fn walk_down(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize,
                 keep: &dyn Fn(usize) -> bool) -> usize {
        let mut peak = 0;
        for start in (0..total).step_by(45) {
            let wanted = start..(start + PAGE_SIZE).min(total);
            show(state, adapter, total, wanted.clone(), Some(start), None, keep);
            peak = peak.max(loaded(state));
            // A filtered listing has fewer rows than the server's total, so the last window can
            // reach past the final row; every index the store counts must read, and it counts to
            // the window's end unless the listing ended first.
            let c = state.view().current().unwrap();
            let known = c.shown();
            assert!(known >= wanted.end || !c.more, "window {wanted:?} counted to {known}");
            assert!(wanted.clone().filter(|&i| i < known).all(|i| rk_at(state, i).is_some()), "window {wanted:?} reads");
        }
        peak
    }

    #[test]
    fn a_5000_item_collection_walked_to_the_end_holds_at_most_eight_pages() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        let peak = walk_down(&mut state, &adapter, total, &|_| true);
        assert!(peak <= crate::stores::page_cache::MAX_LOADED, "peak {peak} pages loaded");
        let c = state.view().current().unwrap();
        assert_eq!((c.shown(), c.more), (total, false), "every index is counted");
    }

    #[test]
    fn scrolling_back_up_re_reads_evicted_pages_and_every_index_returns_the_same_item() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        walk_down(&mut state, &adapter, total, &|_| true);
        let mut re_reads = 0;
        for start in (0..total).step_by(45).rev() {
            let wanted = start..(start + PAGE_SIZE).min(total);
            re_reads += show(&mut state, &adapter, total, wanted.clone(), Some(start), None, &|_| true).len();
            assert!(wanted.clone().all(|i| rk_at(&state, i) == Some(i.to_string())), "window {wanted:?} reads");
        }
        assert!(re_reads > 0, "evicted pages were read again");
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED);
    }

    /// Page 1 keeps 57 rows (its last three are filtered out), so every index from 117 on moves by
    /// three, and no index at or before the focus does.
    #[test]
    fn a_filtered_page_shifts_later_indices_by_three_and_never_one_at_or_before_the_focus() {
        let total = 300;
        let (mut state, adapter) = opened(total);
        let keep = |s: usize| !(117..=119).contains(&s);
        show(&mut state, &adapter, total, 0..200, Some(30), None, &keep);
        assert!((0..117).all(|i| rk_at(&state, i) == Some(i.to_string())), "indices before the shift read as before");
        assert_eq!(rk_at(&state, 117).as_deref(), Some("120"), "unfiltered this was row 117");
        assert_eq!(state.view().current().unwrap().shown(), 237);
    }

    #[test]
    fn a_restore_with_saved_counts_reads_only_the_target_pages() {
        let total = 5_000;
        let keep = |s: usize| s % 7 != 3;
        let (mut state, adapter) = opened(total);
        walk_down(&mut state, &adapter, total, &keep);
        let (_, counts, generation) = directory(&state);
        assert!(counts.iter().any(|&k| k < 60), "a filtered listing has short pages");

        // Kept rows per page are short, so index 3000 sits on a later server page than 3000 / 60.
        let page_of = |index: usize| {
            let (mut p, mut rest) = (0, index);
            while rest >= usize::from(counts[p]) { rest -= usize::from(counts[p]); p += 1; }
            p
        };
        let pages: Vec<usize> = (page_of(3000)..=page_of(3119)).map(|p| p * PAGE_SIZE).collect();
        assert!(pages[0] > 3000, "the window is not where an unfiltered listing would put it");

        reopen(&mut state, &adapter, total);
        assert!(state.run_bare(&adapter, CollectionCmd::Restore { total, counts, generation }));
        let requests = show(&mut state, &adapter, total, 3000..3120, Some(3000), Some(3000), &keep);
        assert_eq!(requests, pages, "only the pages the window covers");
        let kept: Vec<usize> = (0..total).filter(|&s| keep(s)).collect();
        assert!((3000..3120).all(|i| rk_at(&state, i) == Some(kept[i].to_string())));
    }

    /// Where a store's reading stands: the pages read, whether the frontier goes on, the indices
    /// counted, the pages whose rows are held and the content revision.
    fn standing(state: &CollectionState) -> (usize, bool, usize, usize, u64) {
        let view = state.view();
        let c = view.current().unwrap();
        (c.read, c.more, c.shown(), c.pages.loaded_pages(), view.revision())
    }

    /// A surface closed over a live page (a card's hold menu, a sheet) or a Back from Detail hands
    /// the page its own saved directory while the store still holds the listing it was saved from.
    /// That restores nothing: the rows stay held (no member turns into a not-yet-loaded card), and in
    /// a long, partly read list the read position stays where the reading left it and paging stays
    /// on, instead of "read" to the last page with the frontier switched off.
    #[test]
    fn a_restore_of_the_listing_the_store_holds_changes_nothing_in_a_long_partly_read_list() {
        let total = 5_000;
        let keep = |s: usize| s % 7 != 3;
        let (mut state, adapter) = opened(total);
        show(&mut state, &adapter, total, 0..120, Some(0), None, &keep);
        let (_, counts, generation) = directory(&state);
        assert_eq!(counts.len(), total.div_ceil(PAGE_SIZE));
        let before = standing(&state);
        let (read, more, shown, loaded_pages, _) = before;
        assert!(read >= 2 && read < counts.len() && more, "a long list read only part of the way: {before:?}");
        assert!(counts[..read].iter().all(|&k| k < 60) && loaded_pages == read, "short pages, every one held: {before:?}");

        assert!(!state.run_bare(&adapter, CollectionCmd::Restore { total, counts: counts.clone(), generation }),
            "the store holds this listing: nothing changed");
        assert_eq!(standing(&state), before, "no row dropped, the read position and paging untouched");
        assert!((0..shown).all(|i| rk_at(&state, i).is_some()), "every member the store counted is still held");

        let requests = show(&mut state, &adapter, total, 0..shown + PAGE_SIZE, Some(0), None, &keep);
        assert!(!requests.is_empty(), "paging is still on");
        assert_eq!(requests, (read..read + requests.len()).map(|p| p * PAGE_SIZE).collect::<Vec<_>>(),
            "the frontier goes on in order from the page it had reached");

        // A store opened again since is another listing, so it takes the saved directory whole.
        reopen(&mut state, &adapter, total);
        assert!(state.run_bare(&adapter, CollectionCmd::Restore { total, counts: counts.clone(), generation }));
        let (read, more, _, loaded_pages, _) = standing(&state);
        assert_eq!((read, more, loaded_pages), (counts.len(), false, 0), "the directory, no rows");
        assert_eq!(state.view().current().unwrap().kept_counts(), counts);
    }

    /// A directory saved from another listing is a restore: a page whose kept count differs becomes
    /// a hole (its rows no longer match its count), and the pages that agree keep their rows.
    #[test]
    fn a_restore_of_another_listings_directory_drops_only_the_pages_whose_counts_changed() {
        let total = 300;
        let (mut state, adapter) = opened(total);
        show(&mut state, &adapter, total, 0..300, Some(0), None, &|_| true);
        let (_, mut counts, generation) = directory(&state);
        counts[1] = 57;
        rebuild(&mut state, &adapter, total);
        show(&mut state, &adapter, total, 0..300, Some(0), None, &|_| true);
        assert_eq!(loaded(&state), 5);
        assert!(state.run_bare(&adapter, CollectionCmd::Restore { total, counts: counts.clone(), generation }));
        assert_eq!(loaded(&state), 4, "the page whose count changed is a hole");
        assert_eq!(state.view().current().unwrap().kept_counts(), counts);
        assert_eq!(rk_at(&state, 0).as_deref(), Some("0"), "a page that agrees keeps its rows");
        assert_eq!(rk_at(&state, 60), None);
        assert_eq!(rk_at(&state, 117).as_deref(), Some("120"), "the pages after the changed one keep their rows, read from their new first index");
    }

    /// A retoken or a server re-point while a surface covers the page rebuilds the store from its
    /// first page (`refresh_if_client_changed`); the covered page's own per-frame pump then reads the
    /// header and a stretch of the frontier, so the NEW store can hold the very counts the page saved
    /// (an unfiltered list: every page 60, the total the same) without being the listing they were
    /// saved from. The Back is a full restore of the target page, as #605 designed, not a no-op that
    /// leaves the store reading forward from page 8 to page 50 with focus held on skeletons.
    #[test]
    fn a_restore_after_the_store_was_rebuilt_while_covered_takes_the_saved_directory() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        for start in (0..=3000).step_by(45) {
            show(&mut state, &adapter, total, start..start + PAGE_SIZE, Some(start), None, &|_| true);
        }
        let (saved_total, counts, generation) = directory(&state);
        let (read, more, ..) = standing(&state);
        assert!(more && read < counts.len() && counts.iter().all(|&k| k == 60), "deep in an unfiltered list: read {read}");

        rebuild(&mut state, &adapter, total);
        let mut frontier = Vec::new();
        serve(&mut state, &adapter, total, &|_| true, &mut frontier);
        assert_eq!(frontier.len(), usize::from(FRONTIER_ASK), "the covered page's pump read a stretch of the frontier");
        let (now_total, now_counts, now_generation) = directory(&state);
        assert_eq!((now_total, &now_counts), (saved_total, &counts), "the new store holds the saved counts");
        assert_ne!(now_generation, generation, "…as another listing");

        assert!(state.run_bare(&adapter, CollectionCmd::Restore { total, counts: counts.clone(), generation }),
            "a store rebuilt since the page was saved is not the listing the directory came from");
        let c = state.view().current().unwrap();
        assert_eq!((c.read, c.more), (counts.len(), false), "the directory is whole again");
        let requests = show(&mut state, &adapter, total, 3000..3120, Some(3000), Some(3000), &|_| true);
        assert_eq!(requests, vec![3000, 3060], "only the target's pages, not a read forward from the frontier");
        assert_eq!(rk_at(&state, 3000).as_deref(), Some("3000"));
    }

    /// The listing keeps being read while a surface covers the page (`/children` reads continue under
    /// a menu or Detail): a short page lands, so the store's counts are newer than the page's. The
    /// Back is not a restore of the older counts over them (the landed page's count back to 60, its
    /// rows dropped and read again, the read position jumped to the end, paging off).
    #[test]
    fn a_short_page_landing_while_covered_survives_the_restore() {
        let total = 300;
        let keep = |s: usize| !(120..123).contains(&s);
        let (mut state, adapter) = opened(total);
        show(&mut state, &adapter, total, 0..120, Some(0), None, &keep);
        let (_, counts, generation) = directory(&state);
        assert_eq!(standing(&state).0, 2, "two pages read, the third is the frontier");

        // Covered: the frontier goes on and page 2 lands with three rows filtered out.
        show(&mut state, &adapter, total, 0..150, Some(0), None, &keep);
        let before = standing(&state);
        assert_eq!(state.view().current().unwrap().kept_counts()[2], 57, "the landed page counts its kept rows");
        assert_eq!((before.0, before.1), (3, true), "three pages read, paging on");

        assert!(!state.run_bare(&adapter, CollectionCmd::Restore { total, counts, generation }),
            "the store is still the listing the page was saved from: its newer counts stand");
        assert_eq!(standing(&state), before, "the landed page keeps its count and rows; read and paging stay");
        assert_eq!(rk_at(&state, 119).as_deref(), Some("119"));
        assert_eq!(rk_at(&state, 120).as_deref(), Some("123"), "page 2's rows are held at the indices they landed at");
    }

    /// The focused card's page and a pending restore's target page survive the eviction a far-away
    /// window triggers, so the card under focus never becomes a hole.
    #[test]
    fn eviction_keeps_the_focused_and_the_restore_target_pages() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        for start in (0..=3000).step_by(45) {
            show(&mut state, &adapter, total, start..start + PAGE_SIZE, Some(start), None, &|_| true);
        }
        assert!(rk_at(&state, 3000).is_some() && rk_at(&state, 2700).is_some());
        // Reading two pages for a window at the top pushes the load over the cap. A kept page is
        // not read again, so the requests are those two pages and nothing else.
        let requests = show(&mut state, &adapter, total, 60..180, Some(3000), Some(2700), &|_| true);
        assert_eq!(requests, vec![60, 120], "no page the screen holds was dropped and re-read");
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED);
        assert_eq!(rk_at(&state, 3000).as_deref(), Some("3000"), "the focused page is kept");
        assert_eq!(rk_at(&state, 2700).as_deref(), Some("2700"), "the restore target's page is kept");
    }

    /// `Window` names indices of a listing that does not move, so unlike a relative page ask it
    /// needs no identity guard: it is a plain assignment. One computed from a view that the frame's
    /// landing has since replaced is overwritten by the next tick's (the screen sends on change),
    /// and saying a window again changes and requests nothing.
    #[test]
    fn a_window_ask_is_absolute_and_idempotent_so_a_stale_one_is_simply_overwritten() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        show(&mut state, &adapter, total, 300..360, Some(300), None, &|_| true);
        show(&mut state, &adapter, total, 0..60, Some(0), None, &|_| true);
        let wanted = |state: &CollectionState| state.current.as_ref().map(|c| (c.wanted.clone(), c.focus, c.restore));
        let after_stale = wanted(&state);
        assert_eq!(after_stale, Some((0..60, Some(0), None)), "the last ask stands whatever came before it");
        assert!(!state.run_bare(&adapter, CollectionCmd::Window { wanted: 0..60, focus: Some(0), restore: None }),
            "saying it again changes nothing");
        assert_eq!(show(&mut state, &adapter, total, 0..60, Some(0), None, &|_| true), Vec::<usize>::new(),
            "and asks for no page twice");
        assert_eq!(wanted(&state), after_stale);
    }

    #[test]
    fn a_restore_without_kept_counts_reads_forward_eight_requests_per_ask() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        let first = show(&mut state, &adapter, total, 3000..3060, Some(3000), Some(3000), &|_| true);
        assert_eq!(first, (0..8).map(|p| p * PAGE_SIZE).collect::<Vec<_>>(),
            "the frontier, in order, up to the budget");
        let mut asks = 1;
        while rk_at(&state, 3000).is_none() {
            let more = show(&mut state, &adapter, total, 3000..3060, Some(3000), Some(3000), &|_| true);
            assert!(more.len() <= 8, "{} requests on one ask", more.len());
            asks += 1;
            assert!(asks < 20, "the restore never arrived");
        }
        assert_eq!(asks, 7, "51 pages at 8 per ask");
        assert_eq!(rk_at(&state, 3000).as_deref(), Some("3000"));
    }

    #[test]
    fn a_server_that_ignores_paging_keeps_the_wanted_pages_and_drops_the_rest() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        let whole = |state: &mut CollectionState| -> usize {
            let mut answers = 0;
            while let Some(Work::Children { start, .. }) = state.work() {
                let rows: Vec<Vec<PmsMovie>> = (0..total).map(movie).collect::<Vec<_>>()
                    .chunks(PAGE_SIZE).map(|page| page.to_vec()).collect();
                adapter.land(state.generation(), Landing::Page { start, got: total, pages: rows, total });
                assert!(state.take_landing_for_test(&adapter));
                answers += 1;
                assert!(answers < 20);
            }
            answers
        };
        state.run_bare(&adapter, CollectionCmd::Window { wanted: 300..360, focus: Some(300), restore: None });
        assert_eq!(whole(&mut state), 1, "one answer carries the whole listing");
        assert_eq!(rk_at(&state, 300).as_deref(), Some("300"));
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED, "the rest is dropped");
        assert_eq!(rk_at(&state, 1200), None, "page 20 was not wanted");

        state.run_bare(&adapter, CollectionCmd::Window { wanted: 1200..1260, focus: Some(1200), restore: None });
        assert_eq!(whole(&mut state), 1, "a re-read is asked for and answered whole again");
        assert_eq!(rk_at(&state, 1200).as_deref(), Some("1200"));
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED);
    }

    /// A read that fails leaves the same job pending, so the store asks again after its cooldown
    /// with no new window: it never gives a page up.
    #[test]
    fn a_page_read_that_fails_is_the_same_job_again_after_every_failure() {
        let (mut state, adapter) = opened(300);
        state.run_bare(&adapter, CollectionCmd::Window { wanted: 60..120, focus: Some(60), restore: None });
        let job = |s: &CollectionState| match s.work() { Some(Work::Children { start, .. }) => Some(start), _ => None };
        let first = job(&state);
        assert_eq!(first, Some(0), "the frontier, in order");
        for _ in 0..5 {
            adapter.land(state.generation(), Landing::Transport);
            state.take_landing_for_test(&adapter);
            assert_eq!(job(&state), first, "still the page that failed");
            assert!(state.retry_cd > 0, "after a cooldown");
        }
    }

    /// The frontier reads `FRONTIER_ASK` pages per window command and then waits for the next one;
    /// the screen's grid repeats its ask while cards in view are unread, which is what sends it.
    #[test]
    fn the_frontier_waits_for_the_next_window_command_once_its_budget_is_spent() {
        let total = 5_000;
        let (mut state, adapter) = opened(total);
        let ask = |s: &mut CollectionState| s.run_bare(&adapter, CollectionCmd::Window { wanted: 3000..3060, focus: Some(3000), restore: Some(3000) });
        ask(&mut state);
        let mut requests = Vec::new();
        serve(&mut state, &adapter, total, &|_| true, &mut requests);
        assert_eq!(requests.len(), usize::from(FRONTIER_ASK));
        assert!(state.work().is_none(), "idle with the target unread");
        assert!(!ask(&mut state), "the same window changes nothing in the view");
        assert!(state.work().is_some(), "but is an ask: the budget is back");
    }
}

/// `mail` re-serialized and read back the way the recording's segment reader parses numbers.
/// Without `float_roundtrip` an f32 widened to f64 (9.600000381469727) reads back one ulp off, so
/// comparing `to_value` against the parsed file would refuse a recording of ordinary mock ratings.
fn reread(mail: &impl serde::Serialize) -> Option<serde_json::Value> {
    let text = serde_json::to_value(mail).ok()?.to_string();
    serde_json::from_str(&text).ok()
}
