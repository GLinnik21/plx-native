//! Collection-page data model: one physically owned state machine, one generation-stamped
//! mailbox, and one small worker at a time. Resolution, header metadata, and paged children are
//! separate jobs so a tag-only route can publish its resolved ratingKey before later requests
//! finish. All network work runs off the frame thread; landings are applied by [`CollectionState::pump_with_gate`].

use crate::plex::collections::{resolve_tag, CollectionOutcome, CollectionRef};
use crate::plex::ServerId;
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const PAGE_SIZE: usize = 60;
const RETRY_FRAMES: u32 = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectionTarget {
    pub(crate) id: CollectionRef,
    /// Number of children the visible grid currently asks the store to make available.
    pub(crate) want: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CollectionStatus {
    #[default]
    Loading,
    Ready,
    Empty,
    Unavailable,
    Failed,
}

pub(crate) struct Collection {
    /// The identity the page was opened with; `id.rk` is filled in once a tag route resolves.
    pub(crate) id: CollectionRef,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) thumb: String,
    pub(crate) child_count: usize,
    pub(crate) items: Vec<PmsMovie>,
    pub(crate) total: usize,
    pub(crate) status: CollectionStatus,
    pub(crate) more: bool,
    /// Server rows consumed so far — the next page's `X-Plex-Container-Start`. NOT
    /// `items.len()`: `run_job` drops rows that are not `pms::listable` (a clip, an artist), so
    /// the grid can hold fewer members than the server has handed over, and paging from
    /// `items.len()` would re-request rows already seen and append them twice.
    offset: usize,
    want: usize,
    header_ready: bool,
    client_key: Option<(u32, u32)>,
}

impl Collection {
    /// A collection opened (or reopened) with nothing landed yet: its title is the link's name
    /// until the header replaces it.
    fn loading(id: CollectionRef, want: usize, client_key: Option<(u32, u32)>) -> Self {
        Self { title: id.name.clone(), id, summary: String::new(), thumb: String::new(),
            child_count: 0, items: Vec::new(), total: 0, status: CollectionStatus::Loading,
            more: true, offset: 0, want, header_ready: false, client_key }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CollectionView<'a> { current: Option<&'a Collection> }

impl<'a> CollectionView<'a> {
    pub(crate) fn current(self) -> Option<&'a Collection> { self.current }
}

pub(crate) struct CollectionState {
    current: Option<Collection>,
    generation: u32,
    retry_cd: u32,
}

impl Default for CollectionState {
    fn default() -> Self { Self { current: None, generation: 0, retry_cd: 0 } }
}

impl CollectionState {
    pub(crate) fn view(&self) -> CollectionView<'_> { CollectionView { current: self.current.as_ref() } }

    fn supersede(&mut self, adapter: &CollectionAdapter) {
        self.generation = self.generation.checked_add(1).expect("collection generation exhausted");
        adapter.fetch.clear();
        self.retry_cd = 0;
    }

    pub(crate) fn run(&mut self, adapter: &Arc<CollectionAdapter>, cmd: crate::stores::collection::CollectionCmd) -> bool {
        use crate::stores::collection::CollectionCmd;
        match cmd {
            CollectionCmd::Open { target } => {
                if let Some(current) = self.current.as_mut().filter(|c| c.id.same_collection(&target.id)) {
                    let old = current.want;
                    current.want = current.want.max(target.want);
                    let retry = current.status == CollectionStatus::Failed;
                    if retry {
                        current.status = CollectionStatus::Loading;
                        self.retry_cd = 0;
                    }
                    return current.want != old || retry;
                }
                self.supersede(adapter);
                let client_key = crate::plex::client_for(target.id.sid).map(|c| (c.instance_gen(), c.token_gen()));
                self.current = Some(Collection::loading(target.id, target.want.max(PAGE_SIZE), client_key));
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
                for item in c.items.iter_mut()
                    .filter(|item| crate::plex::same_item((item.sid, &item.rk), (sid, &rk))) {
                    crate::pms::set_watched(item, on);
                    hit = true;
                }
                hit
            }
        }
    }

    fn refresh_if_client_changed(&mut self, adapter: &CollectionAdapter) -> bool {
        let Some(c) = self.current.as_ref() else { return false };
        let now = crate::plex::client_for(c.id.sid).map(|x| (x.instance_gen(), x.token_gen()));
        if now == c.client_key { return false; }
        let (id, want) = (c.id.clone(), c.want);
        self.supersede(adapter);
        self.current = Some(Collection::loading(id, want, now));
        true
    }

    pub(crate) fn pump_with_gate(&mut self, adapter: &Arc<CollectionAdapter>, gate: &crate::ui::landgate::Gate) -> bool {
        let mut changed = self.refresh_if_client_changed(adapter);
        if self.retry_cd > 0 { self.retry_cd -= 1; }
        let reply = if crate::app::bootstrap::stores::active() {
            let reply = crate::app::bootstrap::stores::poll("collection", 0, || adapter.fetch.take());
            if reply.is_some() { gate.landed(crate::stores::StoreId::Collection.ord()); }
            reply
        } else {
            crate::stores::take_landing(gate, crate::stores::StoreId::Collection, || adapter.fetch.take())
        };
        if let Some(reply) = reply {
            crate::ui::idle::invalidate();
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
        if c.more && c.items.len() < c.want {
            return Some(Job::Children { rk: c.id.rk.clone(), start: c.offset });
        }
        None
    }

    fn maybe_spawn(&mut self, adapter: &Arc<CollectionAdapter>) {
        if adapter.fetch.busy() || self.retry_cd > 0 { return; }
        let Some(job) = self.job() else { return };
        let Some(c) = self.current.as_ref() else { return };
        let Some(client) = crate::plex::client_for(c.id.sid) else {
            self.retry_cd = RETRY_FRAMES;
            if c.items.is_empty() { self.current.as_mut().unwrap().status = CollectionStatus::Failed; }
            return;
        };
        let generation = self.generation;
        let sid = c.id.sid;
        adapter.fetch.claim();
        let worker_adapter = Arc::clone(adapter);
        let request = serde_json::json!({"store":"collection","slot":0,"gen":generation,
            "sid":sid.raw(),"client":client.instance_gen(),"job":job});
        let spawned = crate::app::bootstrap::stores::admit(request, || crate::task::spawn_small("collection", move || {
            let what = catch_unwind(|| run_job(client, sid, job)).unwrap_or(Landing::Transport);
            worker_adapter.land(generation, what);
        }));
        if !spawned { adapter.fetch.release(); }
    }

    fn apply(&mut self, landing: Landing) -> bool {
        let Some(c) = self.current.as_mut() else { return false };
        match landing {
            Landing::Resolved { rk, title, thumb, summary, child_count } => {
                c.id.rk = rk;
                if !title.is_empty() { c.title = title; }
                c.thumb = thumb;
                c.summary = summary;
                c.child_count = child_count;
                true
            }
            Landing::Header { title, thumb, summary, child_count } => {
                if !title.is_empty() { c.title = title; }
                c.thumb = thumb;
                c.summary = summary;
                c.child_count = child_count;
                c.header_ready = true;
                c.status = CollectionStatus::Loading;
                true
            }
            Landing::Page { start, got, items, total } => {
                if start != c.offset { return false; }
                c.offset = start + got;
                c.total = total.max(c.offset);
                c.items.extend(items);
                // A page of zero rows ends the listing whatever `totalSize` claimed, or a server
                // that over-reports would be asked for the same empty offset forever.
                c.more = got > 0 && c.offset < c.total;
                c.status = if !c.items.is_empty() { CollectionStatus::Ready }
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
                if c.items.is_empty() { c.status = CollectionStatus::Failed; }
                true
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u32 { self.generation }

    #[cfg(test)]
    pub(crate) fn install_for_test(&mut self, items: Vec<PmsMovie>, status: CollectionStatus) {
        let Some(c) = self.current.as_mut() else { return };
        c.title = if c.id.name.is_empty() { "Collection".into() } else { c.id.name.clone() };
        c.summary = "A collection summary long enough for screen layout tests.".into();
        c.child_count = items.len();
        c.total = items.len();
        c.offset = items.len();
        c.items = items;
        c.header_ready = true;
        c.more = false;
        c.status = status;
    }

    #[cfg(test)]
    pub(crate) fn edit_for_test(&mut self, edit: impl FnOnce(&mut Collection)) {
        if let Some(c) = self.current.as_mut() { edit(c); }
    }

    #[cfg(test)]
    pub(crate) fn take_landing_for_test(&mut self, adapter: &CollectionAdapter) -> bool {
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum Landing {
    Resolved { rk: String, title: String, thumb: String, summary: String, child_count: usize },
    Header { title: String, thumb: String, summary: String, child_count: usize },
    /// `got` is the server rows this page consumed; `items` only the listable ones among them.
    Page { start: usize, got: usize, items: Vec<PmsMovie>, total: usize },
    Denied,
    Missing,
    Transport,
}

/// One collection member as a PORTRAIT grid cell. `parse_item` gives an episode its show's poster;
/// a collection that holds episodes of several seasons reads better on the SEASON's poster, so an
/// episode takes `parentThumb` first and falls back to the show's. Its own 16:9 still is never
/// the portrait art — with both posters absent the cell draws the neutral placeholder rather
/// than a cropped landscape frame.
pub(crate) fn member(row: &crate::plex::Metadata, sid: ServerId) -> PmsMovie {
    let mut item = parse_item(row, sid);
    if item.kind == 3 {
        item.thumb = if !row.parent_thumb.is_empty() { row.parent_thumb.clone() }
            else { row.grandparent_thumb.clone() };
    }
    item
}

fn row_header(row: &crate::plex::Metadata) -> (String, String, String, usize) {
    (row.title.clone(), row.thumb.clone(), row.summary.clone(), row.child_count.max(0) as usize)
}

fn run_job(client: &'static crate::plex::Client, sid: ServerId, job: Job) -> Landing {
    match job {
        Job::Resolve { sec, tag, name } => {
            let mut start = 0i64;
            loop {
                match client.section_collections(sec, start, PAGE_SIZE as i64) {
                    CollectionOutcome::Ok(page) => {
                        if let Some(row) = resolve_tag(&page.metadata, tag, &name) {
                            let (title, thumb, summary, child_count) = row_header(row);
                            return Landing::Resolved { rk: row.rating_key.clone(), title, thumb, summary, child_count };
                        }
                        let got = page.metadata.len() as i64;
                        let total = page.total_size.max(page.size).max(0);
                        if got == 0 || start + got >= total { return Landing::Missing; }
                        start += got;
                    }
                    CollectionOutcome::Denied => return Landing::Denied,
                    CollectionOutcome::Missing => return Landing::Missing,
                    CollectionOutcome::Transport => return Landing::Transport,
                }
            }
        }
        Job::Header { rk } => match client.collection(&rk) {
            CollectionOutcome::Ok(page) => match page.metadata.first() {
                Some(row) => {
                    let (title, thumb, summary, child_count) = row_header(row);
                    Landing::Header { title, thumb, summary, child_count }
                }
                None => Landing::Missing,
            },
            CollectionOutcome::Denied => Landing::Denied,
            CollectionOutcome::Missing => Landing::Missing,
            CollectionOutcome::Transport => Landing::Transport,
        },
        Job::Children { rk, start } => match client.collection_children(&rk, start as i64, PAGE_SIZE as i64) {
            CollectionOutcome::Ok(page) => {
                let total = page.total_size.max(page.size).max(0) as usize;
                let got = page.metadata.len();
                let items = page.metadata.iter().filter(|row| crate::pms::listable(&row.kind))
                    .map(|row| member(row, sid)).collect();
                Landing::Page { start, got, items, total }
            }
            CollectionOutcome::Denied => Landing::Denied,
            CollectionOutcome::Missing => Landing::Missing,
            CollectionOutcome::Transport => Landing::Transport,
        },
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Mail { gen: u32, what: Landing }

pub(crate) fn validate_record(slot: u32, value: &serde_json::Value) -> Result<(), &'static str> {
    if slot != 0 { return Err("invalid collection slot"); }
    let mail: Mail = serde_json::from_value(value.clone()).map_err(|_| "invalid collection reply")?;
    if mail.gen == 0 { return Err("invalid collection reply generation"); }
    if serde_json::to_value(&mail).ok().as_ref() != Some(value) { return Err("noncanonical collection reply"); }
    Ok(())
}

struct Fetch { in_flight: AtomicBool, slot: Mutex<Option<Mail>> }

impl Default for Fetch {
    fn default() -> Self { Self { in_flight: AtomicBool::new(false), slot: Mutex::new(None) } }
}

impl Fetch {
    fn busy(&self) -> bool { self.in_flight.load(Ordering::SeqCst) }
    fn claim(&self) { self.in_flight.store(true, Ordering::SeqCst); }
    fn release(&self) { self.in_flight.store(false, Ordering::SeqCst); }
    fn take(&self) -> Option<Mail> {
        let mail = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        self.release();
        Some(mail)
    }
    fn clear(&self) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.release();
    }
}

#[derive(Default)]
pub(crate) struct CollectionAdapter { fetch: Fetch }

impl CollectionAdapter {
    fn land(&self, generation: u32, what: Landing) {
        let mut slot = self.fetch.slot.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_none_or(|old| old.gen < generation) { *slot = Some(Mail { gen: generation, what }); }
    }

    #[cfg(test)]
    pub(crate) fn land_status_for_test(&self, generation: u32, status: CollectionStatus) {
        let what = match status {
            CollectionStatus::Empty => Landing::Page { start: 0, got: 0, items: Vec::new(), total: 0 },
            CollectionStatus::Unavailable => Landing::Denied,
            CollectionStatus::Failed => Landing::Transport,
            _ => Landing::Missing,
        };
        self.land(generation, what);
    }

    #[cfg(test)]
    pub(crate) fn land_resolved_for_test(&self, generation: u32, rk: &str) {
        self.land(generation, Landing::Resolved { rk: rk.into(), title: "Resolved".into(),
            thumb: String::new(), summary: String::new(), child_count: 0 });
    }


    #[cfg(test)]
    pub(crate) fn land_missing_for_test(&self, generation: u32) {
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

    fn row(kind: &str, thumb: &str, parent: &str, grandparent: &str) -> crate::plex::Metadata {
        crate::plex::Metadata { rating_key: "9".into(), kind: kind.into(), title: "Pilot".into(),
            thumb: thumb.into(), parent_thumb: parent.into(), grandparent_thumb: grandparent.into(),
            parent_index: 3, index: 4, grandparent_title: "Show".into(), ..Default::default() }
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

    #[test]
    fn paging_asks_for_the_next_page_only_when_the_grid_wants_more() {
        let adapter = Arc::new(CollectionAdapter::default());
        let mut state = CollectionState::default();
        let target = set_target("50001", 7, "Set");
        state.run(&adapter, CollectionCmd::Open { target: target.clone() });
        assert!(matches!(state.job(), Some(Job::Header { .. })));
        let generation = state.generation();
        adapter.land(generation, Landing::Header { title: "Set".into(), thumb: String::new(),
            summary: String::new(), child_count: 130 });
        assert!(state.take_landing_for_test(&adapter));
        assert!(matches!(state.job(), Some(Job::Children { start: 0, .. })));
        let page = (0..PAGE_SIZE).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, items: page, total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.items.len(), c.more, c.status), (PAGE_SIZE, true, CollectionStatus::Ready));
        assert!(state.job().is_none(), "the grid has not asked for more yet");

        let mut more = target;
        more.want = 2 * PAGE_SIZE;
        assert!(state.run(&adapter, CollectionCmd::Open { target: more }),
            "a larger want on the same identity is a change, not a reopen");
        assert_eq!(state.generation(), generation, "paging does not supersede the collection");
        assert!(matches!(state.job(), Some(Job::Children { start, .. }) if start == PAGE_SIZE));
        adapter.land(generation, Landing::Page { start: 0, got: 0, items: Vec::new(), total: 130 });
        assert!(!state.take_landing_for_test(&adapter), "a page for the wrong offset is dropped");
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
        adapter.land(generation, Landing::Header { title: "Set".into(), thumb: String::new(),
            summary: String::new(), child_count: 130 });
        assert!(state.take_landing_for_test(&adapter));
        let listed = (0..PAGE_SIZE - 5).map(|i| PmsMovie { rk: i.to_string(), ..Default::default() }).collect();
        adapter.land(generation, Landing::Page { start: 0, got: PAGE_SIZE, items: listed, total: 130 });
        assert!(state.take_landing_for_test(&adapter));
        state.run(&adapter, CollectionCmd::Open { target: CollectionTarget {
            want: 2 * PAGE_SIZE, ..set_target("50001", 7, "Set") } });
        assert!(matches!(state.job(), Some(Job::Children { start, .. }) if start == PAGE_SIZE),
            "the next page starts after every row the server sent, listed or not");

        // A collection whose every row is unlisted ends Empty rather than paging forever.
        let mut state = CollectionState::default();
        state.run(&adapter, CollectionCmd::Open { target: set_target("50002", 8, "Clips") });
        let generation = state.generation();
        adapter.land(generation, Landing::Header { title: "Clips".into(), thumb: String::new(),
            summary: String::new(), child_count: 3 });
        assert!(state.take_landing_for_test(&adapter));
        adapter.land(generation, Landing::Page { start: 0, got: 3, items: Vec::new(), total: 3 });
        assert!(state.take_landing_for_test(&adapter));
        let c = state.view().current().unwrap();
        assert_eq!((c.status, c.more), (CollectionStatus::Empty, false));
        assert!(state.job().is_none());
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
        let item = &state.view().current().unwrap().items[0];
        assert!(item.watched && !item.unwatched);
    }
}
