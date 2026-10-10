//! Collection-page data model: one physically owned state machine, one generation-stamped
//! mailbox, and one small worker at a time. Resolution, header metadata, and paged children are
//! separate jobs so a tag-only route can publish its resolved ratingKey before later requests
//! finish. All network work runs off the frame thread; landings are applied by [`CollectionState::pump_with_gate`].

use plx_plex::plex::collections::{resolve_tag, CollectionOutcome, CollectionRef};
use plx_plex::plex::ServerId;
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
    client_key: Option<(u32, u32)>,
}

impl Collection {
    /// A collection opened (or reopened) with nothing landed yet: its title is the link's name
    /// until the header replaces it.
    fn loading(id: CollectionRef, want: usize, client_key: Option<(u32, u32)>) -> Self {
        Self { title: id.name.clone(), id, summary: String::new(), thumb: String::new(),
            child_count: 0, order: None, pages: PageCache::default(), read: 0, total: 0,
            status: CollectionStatus::Loading, more: true, wanted: 0..want, focus: None, restore: None,
            budget: FRONTIER_ASK, header_ready: false, client_key }
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

    /// Every row held, with its absolute index.
    pub fn loaded_rows(&self) -> impl Iterator<Item = (usize, &PmsMovie)> + '_ { self.pages.loaded_rows() }

    /// One kept-row count per page read, for a screen to save with its position and send back as
    /// [`CollectionCmd::Restore`](crate::stores::collection::CollectionCmd::Restore).
    pub fn kept_counts(&self) -> Vec<u8> { self.pages.kept_counts() }

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
    current: Option<Collection>,
    generation: u32,
    /// The content counter [`CollectionView::revision`] reads.
    revision: u64,
    retry_cd: u32,
}

impl Default for CollectionState {
    fn default() -> Self { Self { current: None, generation: 0, revision: 0, retry_cd: 0 } }
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

    pub fn run(&mut self, adapter: &Arc<CollectionAdapter>, cmd: crate::stores::collection::CollectionCmd) -> bool {
        let changed = self.run_cmd(adapter, cmd);
        self.revision += u64::from(changed);
        changed
    }

    fn run_cmd(&mut self, adapter: &Arc<CollectionAdapter>, cmd: crate::stores::collection::CollectionCmd) -> bool {
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
                let client_key = plx_plex::plex::client_for(target.id.sid).map(|c| (c.instance_gen(), c.token_gen()));
                self.current = Some(Collection::loading(target.id, target.want.max(PAGE_SIZE), client_key));
                true
            }
            CollectionCmd::Window { wanted, focus, restore } => {
                let Some(c) = self.current.as_mut() else { return false };
                let changed = c.wanted != wanted || c.focus != focus || c.restore != restore;
                (c.wanted, c.focus, c.restore) = (wanted, focus, restore);
                c.budget = FRONTIER_ASK;
                changed
            }
            CollectionCmd::Restore { total, counts } => {
                // The counts describe a listing of `total` items, one entry per page; any other
                // shape is not this listing's directory and is not guessed at.
                let Some(c) = self.current.as_mut() else { return false };
                if counts.len() != total.div_ceil(PAGE_SIZE) { return false; }
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
            CollectionCmd::SetWatchedLocal { sid, rk, on } => {
                let Some(c) = self.current.as_mut() else { return false };
                let mut hit = false;
                for i in 0..c.shown() {
                    c.pages.edit(i, |item| {
                        if plx_plex::plex::same_item((item.sid, &item.rk), (sid, &rk)) {
                            crate::pms::set_watched(item, on);
                            hit = true;
                        }
                    });
                }
                hit
            }
        }
    }

    fn refresh_if_client_changed(&mut self, adapter: &CollectionAdapter) -> bool {
        let Some(c) = self.current.as_ref() else { return false };
        let now = plx_plex::plex::client_for(c.id.sid).map(|x| (x.instance_gen(), x.token_gen()));
        if now == c.client_key { return false; }
        let (id, want) = (c.id.clone(), c.wanted.end);
        let (window, focus, restore) = (c.wanted.clone(), c.focus, c.restore);
        self.supersede(adapter);
        let mut fresh = Collection::loading(id, want, now);
        (fresh.wanted, fresh.focus, fresh.restore) = (window, focus, restore);
        self.current = Some(fresh);
        self.revision += 1;
        true
    }

    pub fn pump_with_gate(&mut self, adapter: &Arc<CollectionAdapter>, gate: &plx_machine::landgate::Gate) -> bool {
        let mut changed = self.refresh_if_client_changed(adapter);
        if self.retry_cd > 0 { self.retry_cd -= 1; }
        let reply = crate::stores::tape::take_store_landing(
            gate, crate::stores::StoreId::Collection, "collection", 0, &adapter.fetch, |m| m.gen);
        if let Some(reply) = reply {
            plx_machine::idle::invalidate();
            if reply.gen == self.generation { changed |= self.apply(reply.what); }
        }
        self.maybe_spawn(adapter);
        changed
    }

    fn job(&self) -> Option<Job> {
        let c = self.current.as_ref()?;
        if c.status == CollectionStatus::Unavailable || c.status == CollectionStatus::Empty { return None; }
        if c.id.rk.is_empty() {
            return (c.id.sec != 0 && c.id.tag != 0).then(|| Job::Resolve {
                sec: c.id.sec, tag: c.id.tag, name: c.id.name.clone(),
            });
        }
        if !c.header_ready { return Some(Job::Header { rk: c.id.rk.clone() }); }
        // A page the screen shows or restores to, read earlier and since evicted: a re-read at its
        // own offset, which the directory's known count makes safe.
        if let Some(page) = c.wanted_pages().into_iter().find(|&p| p < c.read) {
            return Some(Job::Children { rk: c.id.rk.clone(), start: page * PAGE_SIZE });
        }
        // Otherwise the frontier, in order, while the window needs indices it has not counted.
        if c.more && c.budget > 0 && c.need() > c.shown() {
            return Some(Job::Children { rk: c.id.rk.clone(), start: c.read * PAGE_SIZE });
        }
        None
    }

    fn maybe_spawn(&mut self, adapter: &Arc<CollectionAdapter>) {
        if adapter.fetch.busy() || self.retry_cd > 0 { return; }
        let Some(job) = self.job() else { return };
        let Some(c) = self.current.as_ref() else { return };
        let Some(client) = plx_plex::plex::client_for(c.id.sid) else {
            self.retry_cd = RETRY_FRAMES;
            if c.shown() == 0 {
                self.current.as_mut().unwrap().status = CollectionStatus::Failed;
                self.revision += 1;
            }
            return;
        };
        let generation = self.generation;
        let sid = c.id.sid;
        adapter.fetch.claim(generation);
        let worker_adapter = Arc::clone(adapter);
        let request = serde_json::json!({"store":"collection","slot":0,"gen":generation,
            "sid":sid.raw(),"client":client.instance_gen(),"job":job});
        let spawned = crate::stores::tape::admit(request, || plx_base::task::spawn_small("collection", move || {
            let what = catch_unwind(|| run_job(client, sid, job)).unwrap_or(Landing::Transport);
            worker_adapter.land(generation, what);
        }));
        if !spawned { adapter.fetch.release(); }
    }

    fn apply(&mut self, landing: Landing) -> bool {
        let changed = self.apply_landing(landing);
        self.revision += u64::from(changed);
        changed
    }

    fn apply_landing(&mut self, landing: Landing) -> bool {
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
                c.order = head.order;
                c.header_ready = true;
                c.status = CollectionStatus::Loading;
                true
            }
            Landing::Page { start, got, pages, total } => {
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum Job {
    Resolve { sec: i64, tag: i64, name: String },
    Header { rk: String },
    Children { rk: String, start: usize },
}

/// A collection row's header fields, as both the tag resolution and the metadata GET read them.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Header {
    title: String,
    thumb: String,
    summary: String,
    child_count: usize,
    #[serde(default)]
    order: Option<CollectionOrder>,
}

impl Header {
    fn of(row: &plx_plex::plex::Metadata) -> Self {
        Self { title: row.title.clone(), thumb: row.thumb.clone(), summary: row.summary.clone(),
            child_count: row.child_count.max(0) as usize, order: CollectionOrder::of(row.collection_sort) }
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

fn run_job(client: &'static plx_plex::plex::Client, sid: ServerId, job: Job) -> Landing {
    let run = || -> Result<Landing, Landing> {
        Ok(match job {
            Job::Resolve { sec, tag, name } => {
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
            Job::Header { rk } => answered(client.collection(&rk))?.metadata.first()
                .map_or(Landing::Missing, |row| Landing::Header { rk: None, head: Header::of(row) }),
            Job::Children { rk, start } => {
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
    Header { title: title.into(), thumb: String::new(), summary: String::new(), child_count, order: None }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Mail { gen: u32, what: Landing }

pub fn validate_record(slot: u32, value: &serde_json::Value) -> Result<(), &'static str> {
    if slot != 0 { return Err("invalid collection slot"); }
    let mail: Mail = serde_json::from_value(value.clone()).map_err(|_| "invalid collection reply")?;
    if mail.gen == 0 { return Err("invalid collection reply generation"); }
    if serde_json::to_value(&mail).ok().as_ref() != Some(value) { return Err("noncanonical collection reply"); }
    Ok(())
}

#[derive(Default)]
pub struct CollectionAdapter { fetch: crate::stores::Fetch<Mail> }

impl CollectionAdapter {
    fn land(&self, generation: u32, what: Landing) {
        self.fetch.post(Mail { gen: generation, what }, |old| old.gen < generation);
    }

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
mod tests {
    use super::*;
    use crate::stores::collection::CollectionCmd;

    fn set_target(rk: &str, tag: i64, name: &str) -> CollectionTarget {
        CollectionTarget { id: CollectionRef { sid: ServerId::UNSET, rk: rk.into(), sec: 1, tag,
            name: name.into() }, want: PAGE_SIZE }
    }

    fn row(kind: &str, thumb: &str, parent: &str, grandparent: &str) -> plx_plex::plex::Metadata {
        plx_plex::plex::Metadata { rating_key: "9".into(), kind: kind.into(), title: "Pilot".into(),
            thumb: thumb.into(), parent_thumb: parent.into(), grandparent_thumb: grandparent.into(),
            parent_index: 3, index: 4, grandparent_title: "Show".into(), ..Default::default() }
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
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
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
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let generation = state.generation();
        adapter.fetch.claim(generation);
        let gate = plx_machine::landgate::Gate::default();
        gate.arm_dump(std::time::Duration::from_secs(20));
        let worker = Arc::clone(&adapter);
        let join = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(25));
            worker.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        });
        assert!(state.pump_with_gate(&adapter, &gate),
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
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let stale = state.generation();
        adapter.fetch.claim(stale); // request A is out
        state.run(&adapter, CollectionCmd::Open { target: set_target("50002", 7, "Other") }); // supersedes A
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
        assert!(state.pump_with_gate(&adapter, &gate),
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
        state.run(&adapter, CollectionCmd::Open { target: target.clone() });
        assert!(matches!(state.job(), Some(Job::Header { .. })));
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        assert!(state.take_landing_for_test(&adapter));
        assert!(matches!(state.job(), Some(Job::Children { start: 0, .. })));
        let page = (0..PAGE_SIZE).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, pages: vec![page], total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.shown(), c.more, c.status), (PAGE_SIZE, true, CollectionStatus::Ready));
        assert!(state.job().is_none(), "the grid has not asked for more yet");

        let mut more = target;
        more.want = 2 * PAGE_SIZE;
        assert!(state.run(&adapter, CollectionCmd::Open { target: more }),
            "a larger want on the same identity is a change, not a reopen");
        assert_eq!(state.generation(), generation, "paging does not supersede the collection");
        assert!(matches!(state.job(), Some(Job::Children { start, .. }) if start == PAGE_SIZE));
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
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
        assert!(state.take_landing_for_test(&adapter));
        let listed = (0..PAGE_SIZE - 5).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, pages: vec![listed], total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        state.run(&adapter, CollectionCmd::Open { target: CollectionTarget {
            want: 2 * PAGE_SIZE, ..set_target("50001", 7, "Set") } });
        assert!(matches!(state.job(), Some(Job::Children { start, .. }) if start == PAGE_SIZE),
            "the next page starts after every row the server sent, listed or not");

        // A collection whose every row is unlisted ends Empty rather than paging forever.
        let mut state = CollectionState::default();
        state.run(&adapter, CollectionCmd::Open { target: set_target("50002", 8, "Clips") });
        let generation = state.generation();
        adapter.land(generation, Landing::Header { rk: None, head: header("Clips", 3) });
        assert!(state.take_landing_for_test(&adapter));
        adapter.land(generation, Landing::Page { start: 0, got: 3, pages: vec![vec![]], total: 3 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.status, c.more), (CollectionStatus::Empty, false));
        assert!(state.job().is_none());
    }

    /// A tag route resolves from the section's collection listing, whose row IS the collection's
    /// header — the resolution lands it, so the next job is the first page, not a second GET of
    /// `/library/metadata/{rk}` for the same fields.
    #[test]
    fn a_tag_resolution_lands_the_header_and_pages_next() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run(&adapter, CollectionCmd::Open { target: set_target("", 7, "Set") });
        assert!(matches!(state.job(), Some(Job::Resolve { tag: 7, .. })));
        adapter.land_resolved_for_test(state.generation(), "50007");
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.id.rk.as_str(), c.title.as_str()), ("50007", "Resolved"));
        assert!(matches!(state.job(), Some(Job::Children { start: 0, .. })), "no second header request");
    }

    #[test]
    fn a_local_watched_edit_flips_the_member_in_place() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        state.install_for_test(vec![PmsMovie { rk: "a".into(), unwatched: true, ..Default::default() }],
            CollectionStatus::Ready);
        assert!(!state.run(&adapter, CollectionCmd::SetWatchedLocal { sid: ServerId::UNSET,
            rk: "b".into(), on: true }), "an item not on the page changes nothing");
        assert!(state.run(&adapter, CollectionCmd::SetWatchedLocal { sid: ServerId::UNSET,
            rk: "a".into(), on: true }));
        let item = &state.view().current().unwrap().item(0).unwrap();
        assert!(item.watched && !item.unwatched);
    }

    /// A collection opened and its header landed, reading nothing yet.
    fn opened(total: usize) -> (CollectionState, Arc<CollectionAdapter>) {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        state.run(&adapter, CollectionCmd::Open { target: set_target("50001", 7, "Set") });
        adapter.land(state.generation(), Landing::Header { rk: None, head: header("Set", total) });
        assert!(state.take_landing_for_test(&adapter));
        (state, adapter)
    }

    fn movie(rk: usize) -> PmsMovie {
        PmsMovie { rk: rk.to_string(), ..Default::default() }
    }

    /// Answers every request the store makes, as a server that honours paging: server row `s` is
    /// `rk` `s`, and a row `keep` rejects is filtered out of its page. Records each request's start.
    fn serve(state: &mut CollectionState, adapter: &Arc<CollectionAdapter>, total: usize,
             keep: &dyn Fn(usize) -> bool, requests: &mut Vec<usize>) {
        while let Some(Job::Children { start, .. }) = state.job() {
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
        state.run(adapter, CollectionCmd::Window { wanted, focus, restore });
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
        let counts = state.view().current().unwrap().pages.kept_counts();
        assert!(counts.iter().any(|&k| k < 60), "a filtered listing has short pages");

        // Kept rows per page are short, so index 3000 sits on a later server page than 3000 / 60.
        let page_of = |index: usize| {
            let (mut p, mut rest) = (0, index);
            while rest >= usize::from(counts[p]) { rest -= usize::from(counts[p]); p += 1; }
            p
        };
        let pages: Vec<usize> = (page_of(3000)..=page_of(3119)).map(|p| p * PAGE_SIZE).collect();
        assert!(pages[0] > 3000, "the window is not where an unfiltered listing would put it");

        let (mut back, adapter) = opened(total);
        assert!(back.run(&adapter, CollectionCmd::Restore { total, counts }));
        let requests = show(&mut back, &adapter, total, 3000..3120, Some(3000), Some(3000), &keep);
        assert_eq!(requests, pages, "only the pages the window covers");
        let kept: Vec<usize> = (0..total).filter(|&s| keep(s)).collect();
        assert!((3000..3120).all(|i| rk_at(&back, i) == Some(kept[i].to_string())));
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
            while let Some(Job::Children { start, .. }) = state.job() {
                let rows: Vec<Vec<PmsMovie>> = (0..total).map(movie).collect::<Vec<_>>()
                    .chunks(PAGE_SIZE).map(|page| page.to_vec()).collect();
                adapter.land(state.generation(), Landing::Page { start, got: total, pages: rows, total });
                assert!(state.take_landing_for_test(&adapter));
                answers += 1;
                assert!(answers < 20);
            }
            answers
        };
        state.run(&adapter, CollectionCmd::Window { wanted: 300..360, focus: Some(300), restore: None });
        assert_eq!(whole(&mut state), 1, "one answer carries the whole listing");
        assert_eq!(rk_at(&state, 300).as_deref(), Some("300"));
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED, "the rest is dropped");
        assert_eq!(rk_at(&state, 1200), None, "page 20 was not wanted");

        state.run(&adapter, CollectionCmd::Window { wanted: 1200..1260, focus: Some(1200), restore: None });
        assert_eq!(whole(&mut state), 1, "a re-read is asked for and answered whole again");
        assert_eq!(rk_at(&state, 1200).as_deref(), Some("1200"));
        assert!(loaded(&state) <= crate::stores::page_cache::MAX_LOADED);
    }
}
