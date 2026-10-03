//! Collection-page data model: one physically owned state machine, one generation-stamped
//! mailbox, and one small worker at a time. Resolution, header metadata, and paged children are
//! separate jobs so a tag-only route can publish its resolved ratingKey before later requests
//! finish. All network work runs off the frame thread; landings are applied by [`CollectionState::pump_with_gate`].

use crate::plex::collections::{resolve_tag, CollectionOutcome, CollectionRef};
use crate::plex::ServerId;
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::sync::Arc;

pub(crate) const PAGE_SIZE: usize = 60;
const RETRY_FRAMES: u32 = 120;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectionTarget {
    pub(crate) id: CollectionRef,
    /// Number of children the visible grid currently asks the store to make available.
    pub(crate) want: usize,
}

/// The order a collection lists its members in — its owner's `collectionSort`, which the page names
/// over its grid ("Items · Release order"). An order the server did not state is not guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CollectionOrder {
    Release,
    Title,
    Custom,
}

impl CollectionOrder {
    pub(crate) fn of(collection_sort: Option<i64>) -> Option<Self> {
        match collection_sort? {
            0 => Some(Self::Release),
            1 => Some(Self::Title),
            2 => Some(Self::Custom),
            _ => None,
        }
    }
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
    /// The member order the header stated, if it stated one.
    pub(crate) order: Option<CollectionOrder>,
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
            child_count: 0, order: None, items: Vec::new(), total: 0, status: CollectionStatus::Loading,
            more: true, offset: 0, want, header_ready: false, client_key }
    }

    /// Whether the header (title, art, summary) has landed — before it, an empty `thumb` is not
    /// yet an answer.
    pub(crate) fn header_ready(&self) -> bool { self.header_ready }
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

    pub(crate) fn pump_with_gate(&mut self, adapter: &Arc<CollectionAdapter>, gate: &plx_machine::landgate::Gate) -> bool {
        let mut changed = self.refresh_if_client_changed(adapter);
        if self.retry_cd > 0 { self.retry_cd -= 1; }
        let reply = crate::stores::tape::take_store_landing(
            gate, crate::stores::StoreId::Collection, "collection", 0, &adapter.fetch);
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
        let spawned = crate::stores::tape::admit(request, || plx_base::task::spawn_small("collection", move || {
            let what = catch_unwind(|| run_job(client, sid, job)).unwrap_or(Landing::Transport);
            worker_adapter.land(generation, what);
        }));
        if !spawned { adapter.fetch.release(); }
    }

    fn apply(&mut self, landing: Landing) -> bool {
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
    fn of(row: &crate::plex::Metadata) -> Self {
        Self { title: row.title.clone(), thumb: row.thumb.clone(), summary: row.summary.clone(),
            child_count: row.child_count.max(0) as usize, order: CollectionOrder::of(row.collection_sort) }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum Landing {
    /// The collection's header; `rk` is `Some` when it answers a tag route's resolution.
    Header { rk: Option<String>, head: Header },
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

/// A read's page, or the landing its failure is: the one mapping of the server's non-page answers.
fn answered(outcome: CollectionOutcome) -> Result<crate::plex::MediaContainer, Landing> {
    match outcome {
        CollectionOutcome::Ok(page) => Ok(page),
        CollectionOutcome::Denied => Err(Landing::Denied),
        CollectionOutcome::Missing => Err(Landing::Missing),
        CollectionOutcome::Transport => Err(Landing::Transport),
    }
}

fn run_job(client: &'static crate::plex::Client, sid: ServerId, job: Job) -> Landing {
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
                let items = page.metadata.iter().filter(|row| crate::pms::listable(&row.kind))
                    .map(|row| member(row, sid)).collect();
                Landing::Page { start, got, items, total }
            }
        })
    };
    run().unwrap_or_else(|failed| failed)
}

#[cfg(test)]
fn header(title: &str, child_count: usize) -> Header {
    Header { title: title.into(), thumb: String::new(), summary: String::new(), child_count, order: None }
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

#[derive(Default)]
pub(crate) struct CollectionAdapter { fetch: crate::stores::Fetch<Mail> }

impl CollectionAdapter {
    fn land(&self, generation: u32, what: Landing) {
        self.fetch.post(Mail { gen: generation, what }, |old| old.gen < generation);
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
        self.land(generation, Landing::Header { rk: Some(rk.into()), head: header("Resolved", 0) });
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

    /// The page names the member order the collection's owner chose, from the header's
    /// `collectionSort` (string-encoded, like every PMS number); an unstated or unknown order is
    /// not guessed.
    #[test]
    fn the_header_reads_the_collections_member_order() {
        let head = |body: &str| {
            let row: crate::plex::Metadata = serde_json::from_str(body).expect("parses");
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
        adapter.land(generation, Landing::Header { rk: None, head: header("Set", 130) });
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
        adapter.land(generation, Landing::Header { rk: None, head: header("Clips", 3) });
        assert!(state.take_landing_for_test(&adapter));
        adapter.land(generation, Landing::Page { start: 0, got: 3, items: Vec::new(), total: 3 });
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
        let item = &state.view().current().unwrap().items[0];
        assert!(item.watched && !item.unwatched);
    }
}
