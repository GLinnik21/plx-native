
const fn set_cur(i: usize) {}
//! browse — the Library screen's per-section paged catalog.
//!
//! Production ownership is in `stores::browse::BrowseStore`, one instance per app Bridge. This
//! module supplies the core state transitions and immutable publication types; every stateful
//! operation requires an explicit `BrowseState` or `BrowseStore` receiver.
//!
//! Sibling of `pms.rs`'s hub catalog, which stays hub-only (256-cap, rebuilt wholesale by
//! `pms`'s worker-driven hub catalog); this store pages arbitrarily large sections without blocking the
//! main loop. Data model: one [`SecItems`] per section — a PAGE-CHUNKED table sized to the
//! listing's `totalSize`, a page (`PAGE` items) allocated only when it lands (restructure phase
//! 4's O(result) rule) — filled page-by-page by ONE background fetch at a
//! time using the season-switch idiom from `metadata.rs` — [`crate::task::spawn_small`] + a
//! `Mutex` mailbox + generation atomics (a re-query supersedes in-flight landings), applied
//! on the main thread by [`pump`] once a frame while the Library screen is up.
//!
//! The sort/filter MENUS are server-driven: the first page of a section is requested with
//! `includeMeta=1` and the response's `Meta.Type[]` supplies the Sort entries; the genre
//! value list is fetched lazily (`kick_genres`) when the filter menu first opens. Nothing
//! menu-shaped is hardcoded — a music section would bring its own sorts.
//!
//! [`BrowseState`] is main-thread-only; worker threads touch only their owning store adapter's
//! mailboxes + atomics and the `&'static` Plex client.
//!
//! ## The table addresses (SOURCE, section), not a section
//!
//! A section key is only unique within one server: measured 2026-08-11 against a real share, our
//! own server's section `1` and the friend's section `1` are different libraries, and each server
//! answers 401 to the other's token. So the table is a flat `Vec<BrowseSection>` whose every entry
//! names its [`BrowseSource`], and every fetch is issued through `client_for(source.sid)` captured
//! AT THE SPAWN SITE — never `client()` read inside a worker, which would dial whichever server
//! happened to be current when the thread got scheduled.
//!
//! **It grows by APPEND and never by rebuild**, which is what keeps the page mailbox sound. A page
//! landing is blamed on a section INDEX (`PageResult.sec`), so an index that moved under an
//! in-flight fetch would splice one library's items into another's store. The old
//! the old synchronous discovery's early-return was the only thing preventing that; appending is the property
//! that replaces it, and it holds for every source that lands later rather than only for the
//! second call.
use crate::plex::{SectionQuery, ServerId};
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
#[cfg(test)]
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Page size for section listings. Two grid screens' worth (10 rows × 6) — big enough that a
/// full-screen scroll rarely waits, small enough that a page parse stays invisible on-frame.
const PAGE: usize = 60;

/// Frames between attempts to reach a source whose discovery failed. Far longer than the page
/// retry (`RETRY_CD`, ~2 s): a page retry is racing a user looking at a spinner, while an
/// unreachable SHARE is a state the Sources list states in words and nobody is waiting on. Each
/// attempt can also park a worker in `connect(2)` for its full timeout, so a short backoff would
/// keep one thread permanently occupied for a server that is simply switched off.
const SRC_RETRY_CD: u32 = 600; // ~10 s at 60 fps

// ---- the granted roster: the SOURCE dimension of the table ----------------------------------

/// **How a source's last dial ended** — the widened form of what used to be one `bool`.
///
/// A bool could say "answered" or "did not", and the Sources list said exactly those two things.
/// It could not say the three things a user needs told apart, and which the prober already
/// distinguishes ([`crate::plex::probe::Outcome`]): nobody has dialled yet, the server answered but
/// refused our token, and the server did not answer at all. Those want different words and, for the
/// middle one, a different remedy — a 401 is a sharing-grant problem that re-fetching
/// `/api/v2/resources` fixes, and telling the user their friend's server is unreachable sends them
/// to look at a router for something that was never a network fault.
///
/// Auth's server-race settlement populates all four states through the registry. Ordinary browse
/// requests still carry only the old answered/did-not-answer bit; they may clear a failure with a
/// success, but cannot erase the more specific 401 without another identity probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum SourceState {
    /// Registered, never dialled. Distinct from [`Self::Unreachable`]: a source nobody has tried is
    /// not a source that failed, and the group must not open dimmed. This is the DEFAULT for the
    /// same reason the old `reachable: true` seed was optimistic.
    #[default]
    NotProbed,
    /// Answered, and it was the machine we asked for.
    Reachable,
    /// Answered with 401. A token problem, never a network one — the remedy is a fresh
    /// `/api/v2/resources`, not another address.
    ///
    Unauthorized,
    /// Did not answer: refused, timed out, or unresolvable.
    Unreachable,
}

/// One SOURCE the table is addressed by — a server this account has been granted. Comes from the
/// [server registry](crate::plex::server_ids), which is the granted roster: a server is registered
/// only once plex.tv (or the `plxnative-servers` dev trigger) handed us a token for it.
#[derive(Clone)]
pub(crate) struct BrowseSource {
    /// The registry slot every fetch for this source's sections is issued through.
    pub(crate) sid: ServerId,
    /// Exact client lifecycle behind `sid`; slot ids survive repoint and therefore are not enough
    /// to decide whether completed discovery still belongs here.
    client_addr: usize,
    token_gen: u32,
    /// The server's `machineIdentifier` — the ONLY key a Home selection can be PERSISTED under
    /// (`plex::pins`), because a roster position reshuffles and an address moves. `""` until the
    /// registry has learned it, which is a source whose pins live for this run only.
    pub(crate) machine_id: String,
    /// This account owns the server. Not derivable from an empty [`BrowseSource::handle`] — a
    /// share whose `sourceTitle` plex.tv did not send is still a share — and it is the whole input
    /// to the first-run default (yours On, a friend's Off).
    pub(crate) owned: bool,
    /// The MACHINE name ("nas-home") — the Sources list's group header, and the only place in the
    /// app a machine is named. Learned from the roster, else from the server naming itself
    /// (`Client::friendly_name`); `""` until one of those lands.
    pub(crate) name: String,
    /// The owner's plex.tv handle ("friend"); **empty on your own server**, where the absence of an
    /// owner is drawn as the absence of a run rather than as an empty one.
    pub(crate) handle: String,
    /// How its last dial ended. One of the design's three orthogonal states — *granted* (the
    /// roster's answer), *pinned* (the only control), *reachable* (a fact about now). A source that
    /// has stopped answering keeps every section it had learned and every pin on them: its group
    /// dims whole and still reads `On`, because nothing was unpinned. Hiding it would read as a
    /// revoked share.
    ///
    /// Was a `bool`; see [`SourceState`] for why it is not any more. Auth publishes the precise
    /// discovery result through the server registry; ordinary browse requests write their coarser
    /// answer through [`BrowseSource::set_reachable`].
    pub(crate) state: SourceState,
    /// Which tier of connection actually won — local, remote, or Plex's relay tunnel. Restored
    /// from the persisted winner at boot and replaced by auth when a new race settles. The Sources
    /// list only renders it beside [`SourceState::Reachable`], so an offline source retains the
    /// route metadata needed for retry/playback policy without claiming that route works now.
    pub(crate) tier: Option<crate::plex::probe::Location>,
    /// its `/library/sections` has landed — sections are appended exactly once per source
    sections_done: bool,
    /// its per-library item counts have landed (the row sub-line's "185 films")
    counts_done: bool,
    /// frames before the next discovery attempt after a failure (main-thread; [`pump`] counts down)
    retry_cd: u32,
}

impl BrowseSource {
    /// Did the last dial succeed? The old `bool`, preserved as a QUESTION so that widening the
    /// field did not have to become a behaviour change at the same time.
    ///
    /// **`NotProbed` answers `true`**, which looks generous and is the behaviour being preserved
    /// exactly: the field it replaced was seeded `reachable: true` on registration, with the
    /// comment *"a source nobody has dialled yet is not a source that failed, and the whole group
    /// would otherwise open dimmed"*. Anything that needs to tell "not yet" from "yes" must read
    /// [`BrowseSource::state`] and say so, which is the entire reason the state exists.
    pub(crate) fn reachable(&self) -> bool {
        !matches!(self.state, SourceState::Unreachable)
    }
    /// Record a generic PMS request that answered or did not. A failed request cannot distinguish
    /// HTTP status from transport/parse failure, so it preserves an Unauthorized result supplied
    /// by the identity prober; a successful request is enough evidence to clear any failure.
    #[cfg(test)]
    pub(crate) fn set_reachable(&mut self, ok: bool) {
        // A generic PMS request folds status/transport/parse errors into one `None`, so it cannot
        // disprove the more specific 401 the identity prober already observed. Only a successful
        // request clears Unauthorized; the auth coordinator can explicitly replace it with a
        // later aggregate Unreachable result through the registry.
        if ok {
            self.state = SourceState::Reachable;
        } else if self.state != SourceState::Unauthorized {
            self.state = SourceState::Unreachable;
        }
    }
    /// Mirror the registry's canonical result after it atomically merged a generic request with
    /// any more-specific identity-probe answer.
    fn set_probe_outcome(&mut self, outcome: crate::plex::probe::Outcome) {
        self.state = source_state(Some(outcome));
    }
}

fn source_state(outcome: Option<crate::plex::probe::Outcome>) -> SourceState {
    match outcome {
        None => SourceState::NotProbed,
        Some(crate::plex::probe::Outcome::Reachable) => SourceState::Reachable,
        Some(crate::plex::probe::Outcome::Unauthorized) => SourceState::Unauthorized,
        Some(
            crate::plex::probe::Outcome::WrongServer | crate::plex::probe::Outcome::Unreachable,
        ) => SourceState::Unreachable,
    }
}

fn source_snapshot(sid: ServerId) -> Option<(SourceState, Option<crate::plex::probe::Location>)> {
    let client = crate::plex::client_for(sid)?;
    let token_gen = client.token_gen();
    // Probe publication follows set_link, so read the acquire-backed result before the tier. The
    // second lookup rejects a re-point or in-place retoken between those two reads.
    let state = source_state(crate::plex::server_probe_result(sid));
    let tier = client.link();
    crate::plex::client_for(sid)
        .filter(|now| std::ptr::eq(*now, client) && now.token_gen() == token_gen)
        .map(|_| (state, tier))
}

// ---- section table (discovered per source) ---------------------------------------------------

/// One browsable library section (movie or show), from one source's `GET /library/sections`.
#[derive(Clone)]
pub(crate) struct BrowseSection {
    /// index into [`BrowseState`]'s source table — the server half of this row's address. A bare `key` names two
    /// different libraries the moment a second server is granted.
    pub(crate) src: usize,
    pub(crate) key: i64,
    pub(crate) title: String,
    /// The library's TYPE. A real type and not the `is_show: bool` this replaced, because the tab
    /// projection asks "does any owned library have this KIND" ([`tabs`]) — and with two values that
    /// question cannot tell Music from Movies, so a friend's music library would fold onto your
    /// *Movies* pill, which is the one case the projection exists to get right.
    pub(crate) kind: SecKind,
    /// The library's own item count, unfiltered — the Sources row's "185 films". `-1` until the
    /// count probe lands. Deliberately NOT [`SecState::total`], which is the count of the CURRENT
    /// QUERY: with an unwatched filter on, that number describes what you are looking at and would
    /// misdescribe the library in a list whose whole job is naming libraries.
    pub(crate) count: i64,
    /// **Is this library a FAVOURITE?** The user's one control — *Favorite libraries* in the words
    /// they read, `pinned` in the identifiers, which were deliberately not renamed with it (the
    /// persisted `home_pins` key is a ROLLBACK hazard, not an upgrade one).
    ///
    /// **It governed Home ALONE until 2026-09-05 and this comment said so.** It now governs every
    /// browsing surface: Home's shelves, whether this library's TYPE gets a tab pill at all
    /// ([`tab_has_favorite`], so a type whose last favourite is switched off is out of the strip),
    /// and the Library's own Sources picker ([`source_rows`]). What still comes from the GRANT and
    /// not from this bit: access itself, the grid, sort and the A–Z rail — all downstream of a
    /// library you have already chosen — and Search, which stays grant-wide and only RANKS
    /// favourite-library hits first.
    ///
    /// Your own libraries start favourite and a friend's start favourite only where you own no
    /// library of that type ([`crate::plex::pins::default_on`]); the last favourite cannot be
    /// turned off, or the app has nothing.
    pub(crate) pinned: bool,
}

/// One sort-menu entry (from `Meta.Type[].Sort` — server-driven).
#[derive(Clone)]
pub(crate) struct SortEntry {
    pub(crate) key: String,   // "titleSort"
    pub(crate) title: String, // "Title"
    pub(crate) default_desc: bool,
}

/// One genre value (tag id + display title), from the section's `/genre` value list.
#[derive(Clone)]
pub(crate) struct GenreEntry {
    pub(crate) id: String,
    pub(crate) title: String,
}

/// What the last page fetch for a section produced — Loading / Ready / **Failed**, per SECTION
/// because that is the grain this store's state already has.
///
/// The irony is worth recording once: `pms.rs`'s hub fetch runs this same three-state machine and
/// its own doc says it was "Modelled on `browse.rs`'s page store, deliberately, because it already
/// learned both lessons this needed" — the copy took the two lessons (a failed fetch must never
/// overwrite a populated store; a fast-failing network is held off by a countdown) and then added
/// the state the ORIGINAL never had. Without it a failed first page left [`SecState::total`] at -1
/// and armed nothing but the cooldown, so [`loading_initial`] stayed true forever and the Library
/// grid spun with no way out — on the user's own server, for any failed fetch.
///
/// `Failed` describes the last FETCH, not the store: a mid-scroll page failure on a populated
/// section is Failed with items still on screen, which is why the screen's read-out projects this
/// state and the store TOGETHER (`screens::library`'s `readout`, where that whole decision lives as
/// one pure function) rather than reading the state alone — the same rule `pms::HubState` and
/// `StatusKind::Empty` state, that an empty answer is an answer and only a fault is a fault.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SecFetch {
    /// no page fetch for this section's current query has produced an ANSWER yet.
    ///
    /// Not "a fetch is in flight": once a query has failed the state stays `Failed` while
    /// `RETRY_CD` counts down AND while the retry itself is out, so the user reads one steady
    /// "couldn't load this" rather than a spinner blinking back every two seconds. `Loading` is
    /// therefore the FIRST attempt only, and a query returns to it exactly once — at `requery`.
    Loading,
    /// the server answered — the store is whatever it says, possibly legitimately empty
    Ready,
    /// the fetch failed (network/parse/panic); whatever was already in the store is untouched
    /// and [`maybe_spawn`] is counting `RETRY_CD` down to the next automatic attempt
    Failed,
}

pub(crate) mod record;
pub(crate) mod section_hubs;
pub(crate) mod view;

/// A tier-three application bookmark, frozen only at navigation boundaries. It is not
/// current focus; a live Library entry keeps its authoritative memory in FocusEngine.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cursor {
    pub at: CursorAt,
    pub scroll: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CursorAt {
    ItemKey {
        sid: ServerId,
        rk: String,
        slot: usize,
    },
    SlotIndex(usize),
}

/// Per-section browse state: the current query, the server-driven menus, the sparse item
/// store, the library's own published shelves, and the remembered view (focus/scroll survive
/// leaving the screen — state amnesia is the official app's loudest complaint).
#[derive(Clone)]
struct SecState {
    // query
    sort_idx: usize,
    sort_desc: bool,
    unwatched: bool,
    genre: Option<Arc<GenreEntry>>,
    // menus (kept across re-queries)
    sorts: Arc<Vec<SortEntry>>,
    genres: Arc<Vec<GenreEntry>>,
    genres_done: bool, // a genre fetch LANDED (even empty) — kick_genres won't re-spawn
    /// per-letter (label, count) in titleSort order, from `/firstCharacter` — the letter rail.
    /// Counts describe the UNFILTERED title listing, so the rail only shows in that state.
    letters: Arc<Vec<(String, i64)>>,
    letters_done: bool,
    /// **The library's OWN shelves** — `Library Recommended`, as its server's owner arranged it.
    /// A field here rather than a store of its own because this struct is already the per-section
    /// aggregate; `section_hubs`' module doc argues it, and `reset()` clearing it on a profile
    /// switch is the consequence that matters most.
    hubs: section_hubs::SecHubs,
    // data
    fetch: SecFetch, // what the last page fetch for this section did
    total: i64,      // -1 = unknown (first fetch of this query still out)
    items: SecItems,
    cursor: Option<Arc<Cursor>>,
}

/// A section's items, CHUNKED BY PAGE (restructure phase 4, the O(result) rule of spec §5.2 —
/// `docs/stores-as-machines.md` §2.6). The outer vector holds one slot per page of `PAGE`
/// items and a page is allocated only when its items land, so sizing the store to a listing's
/// `totalSize` on the main thread costs `total / PAGE` words rather than an `Option<PmsMovie>`
/// per item in the library — the previous shape allocated every slot of a 20 000-title section
/// in the drain, on the first page's landing. `get`/`set` index by absolute item index exactly
/// as the flat vector did; `page_missing` is the fetch scan, over pages.
/// Cloning retains the page table in O(1). Writes copy only the table of page handles and
/// changed pages, never the whole loaded catalog. This is the Library read-view backing.
#[derive(Clone, Default)]
struct SecItems {
    pages: Arc<Vec<Option<Arc<Vec<Option<PmsMovie>>>>>>,
    len: usize,
}

impl SecItems {
    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
    fn clear(&mut self) {
        self.pages = Arc::default();
        self.len = 0;
    }
    /// Size to the listing: pages are kept where they still fit, dropped past the new end.
    fn resize(&mut self, total: usize) {
        if total == self.len {
            return;
        }
        self.len = total;
        let pages = Arc::make_mut(&mut self.pages);
        pages.resize_with(total.div_ceil(PAGE), || None);
        // Forget a truncated tail, even when the listing later grows in the same page.
        if !total.is_multiple_of(PAGE) {
            if let Some(Some(page)) = pages.last_mut() {
                if page.len() > total % PAGE {
                    Arc::make_mut(page).truncate(total % PAGE);
                }
            }
        }
    }
    fn get(&self, i: usize) -> Option<&PmsMovie> {
        if i >= self.len {
            return None;
        }
        self.pages.get(i / PAGE)?.as_ref()?.get(i % PAGE)?.as_ref()
    }
    /// Place `m` at `i`; a page is allocated on its first item. Out of range is ignored (the
    /// listing shrank under a fetch, which the next page reconciles).
    fn set(&mut self, i: usize, m: PmsMovie) {
        if i >= self.len {
            return;
        }
        let Some(slot) = Arc::make_mut(&mut self.pages).get_mut(i / PAGE) else {
            return;
        };
        let n = PAGE.min(self.len - (i / PAGE) * PAGE);
        let page = slot.get_or_insert_with(|| Arc::new((0..n).map(|_| None).collect()));
        let page = Arc::make_mut(page);
        page.resize_with(n, || None);
        if let Some(cell) = page.get_mut(i % PAGE) {
            *cell = Some(m);
        }
    }
    /// Does page `p` (items `p*PAGE ..`) have a slot not yet filled?
    fn page_missing(&self, p: usize) -> bool {
        match self.pages.get(p) {
            None => false,
            Some(None) => true,
            Some(Some(page)) => {
                page.len() < PAGE.min(self.len - p * PAGE) || page.iter().any(|o| o.is_none())
            }
        }
    }
    /// A read scan first: an optimistic edit must not clone unrelated retained pages.
    fn set_watched(&mut self, sid: ServerId, rk: &str, on: bool) -> bool {
        let matches = |m: &PmsMovie| crate::plex::same_item((m.sid, &m.rk), (sid, rk));
        let mut hit = false;
        for p in 0..self.pages.len() {
            if !self.pages[p]
                .as_ref()
                .is_some_and(|page| page.iter().flatten().any(matches))
            {
                continue;
            }
            let page = Arc::make_mut(&mut self.pages)[p].as_mut().unwrap();
            for m in Arc::make_mut(page)
                .iter_mut()
                .flatten()
                .filter(|m| matches(m))
            {
                crate::pms::set_watched(m, on);
                hit = true;
            }
        }
        hit
    }
    #[cfg(test)]
    fn from_vec(v: Vec<Option<PmsMovie>>) -> Self {
        let mut s = SecItems::default();
        s.resize(v.len());
        for (i, m) in v.into_iter().enumerate() {
            if let Some(m) = m {
                s.set(i, m);
            }
        }
        s
    }
}

impl Default for SecState {
    fn default() -> Self {
        SecState {
            sort_idx: 0,
            sort_desc: false,
            unwatched: false,
            genre: None,
            sorts: Arc::default(),
            genres: Arc::default(),
            genres_done: false,
            letters: Arc::default(),
            letters_done: false,
            hubs: Default::default(),
            fetch: SecFetch::Loading,
            total: -1,
            items: SecItems::default(),
            cursor: None,
        }
    }
}

/// The main-thread state of Browse. Worker mailboxes and their single-flight atomics live in the
/// sibling [`BrowseAdapter`]; everything whose identity belongs to a Browse instance lives here.
#[derive(Clone)]
pub(crate) struct BrowseState {
    sources: Vec<BrowseSource>,
    sections: Vec<BrowseSection>,
    states: Vec<SecState>,
    cur: usize,
    /// Wanted item-index range (inclusive lo, exclusive hi) — set by the grid each frame from its
    /// visible rows + lookahead; [`pump`] fetches the first missing page inside it.
    want: (usize, usize),
    gen: u32,
    sections_gen: u32,
    epoch: u32,
    src_facts_gen: u32,
    tabs_gen: u32,
    tab_shape: u32,
    retry_cd: u32,
    remembered: Vec<(SecKind, String, i64)>,
    recorded: Option<crate::plex::session::HomePins>,
}

/// Worker-facing half of one Browse store. Every spawned job captures this adapter, so a result
/// can only land in the store that admitted the job even when another Bridge is alive.
pub(crate) struct BrowseAdapter {
    fetching: AtomicBool,
    genre_fetching: AtomicBool,
    letters_fetching: AtomicBool,
    src_fetching: AtomicBool,
    page_result: Mutex<Option<PageResult>>,
    genre_result: Mutex<Option<DirectoryResult<GenreEntry>>>,
    letter_result: Mutex<Option<DirectoryResult<(String, i64)>>>,
    src_result: Mutex<Option<(u32, usize, SrcLanding)>>,
    hubs: section_hubs::HubAdapter,
}

pub(crate) struct RosterSync {
    pub(crate) changed: bool,
    pub(crate) retire_adapter: bool,
}

impl Default for BrowseAdapter {
    fn default() -> Self {
        Self {
            fetching: AtomicBool::new(false),
            genre_fetching: AtomicBool::new(false),
            letters_fetching: AtomicBool::new(false),
            src_fetching: AtomicBool::new(false),
            page_result: Mutex::new(None),
            genre_result: Mutex::new(None),
            letter_result: Mutex::new(None),
            src_result: Mutex::new(None),
            hubs: Default::default(),
        }
    }
}

impl BrowseAdapter {
    fn clear(&self) {
        *self.page_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.genre_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.letter_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.src_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.hubs.result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        for flag in [&self.fetching, &self.genre_fetching, &self.letters_fetching,
            &self.src_fetching, &self.hubs.fetching] {
            flag.store(false, Ordering::SeqCst);
        }
    }
}

impl Default for BrowseState {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            sections: Vec::new(),
            states: Vec::new(),
            cur: 0,
            want: (0, 0),
            gen: 0,
            sections_gen: 0,
            epoch: 0,
            src_facts_gen: 0,
            tabs_gen: 1,
            tab_shape: u32::MAX,
            retry_cd: 0,
            remembered: Vec::new(),
            recorded: None,
        }
    }
}

impl BrowseState {
    pub(crate) fn discovery_needs_pump(&self, adapter: &BrowseAdapter) -> bool {
        if adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            return true;
        }
        let live: Vec<ServerId> = crate::plex::server_ids().collect();
        if live.len() != self.sources.len()
            || self.sources.iter().zip(&live).any(|(source, sid)| source.sid != *sid) {
            return true;
        }
        for source in &self.sources {
            let now = crate::plex::client_for(source.sid);
            if source.client_addr != now.map_or(0, |client| client as *const _ as usize)
                || source.token_gen != now.map_or(0, |client| client.token_gen()) {
                return true;
            }
            if let Some((state, tier)) = source_snapshot(source.sid) {
                if source.state != state || source.tier != tier {
                    return true;
                }
            }
            if let Some(facts) = crate::plex::server_facts(source.sid) {
                if (source.name.is_empty() && !facts.name.is_empty())
                    || source.handle != facts.handle || source.owned != facts.owned {
                    return true;
                }
            }
            if source.machine_id.is_empty() || source.retry_cd > 0 {
                return true;
            }
        }
        !adapter.src_fetching.load(Ordering::SeqCst)
            && self.sources.iter().any(|source| !source.sections_done || !source.counts_done)
    }

    pub(crate) fn pump_needs_work(&self, adapter: &BrowseAdapter) -> bool {
        if self.discovery_needs_pump(adapter)
            || self.retry_cd > 0
            || adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            || adapter.genre_result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            || adapter.letter_result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            || adapter.hubs.result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            || self.states.iter().any(|state| state.hubs.needs_tick()) {
            return true;
        }
        if adapter.fetching.load(Ordering::SeqCst) {
            return false;
        }
        let current = self.cur();
        let Some(state) = self.states.get(current) else { return false };
        if state.total < 0 {
            return true;
        }
        let (lo, hi) = self.want;
        let first = lo / PAGE;
        let last = hi.saturating_sub(1) / PAGE;
        (first..=last).any(|page| state.items.page_missing(page))
    }

    fn sources(&self) -> &[BrowseSource] {
        &self.sources
    }
    fn sections(&self) -> &[BrowseSection] {
        &self.sections
    }
    fn states(&self) -> &[SecState] {
        &self.states
    }
    fn states_mut(&mut self) -> &mut Vec<SecState> {
        &mut self.states
    }
    fn source_mut(&mut self, i: usize) -> Option<&mut BrowseSource> {
        self.sources.get_mut(i)
    }
    fn state_mut(&mut self, i: usize) -> Option<&mut SecState> {
        self.states.get_mut(i)
    }
    fn cur_state(&self) -> Option<&SecState> {
        self.states.get(self.cur())
    }
    pub(crate) fn cur(&self) -> usize {
        self.cur.min(self.sections.len().saturating_sub(1))
    }
    fn section_sid(&self, i: usize) -> Option<ServerId> {
        let section = self.sections.get(i)?;
        self.sources.get(section.src).map(|source| source.sid)
    }
    fn section_kind(&self, i: usize) -> Option<SecKind> {
        self.sections.get(i).map(|s| s.kind)
    }
    fn query_gen(&self) -> u32 {
        self.gen
    }
    fn sections_gen(&self) -> u32 {
        self.sections_gen
    }
    pub(crate) fn table_epoch(&self) -> u32 {
        self.epoch
    }
    pub(crate) fn source_list_gen(&self) -> u32 {
        self.sections_gen.wrapping_add(self.src_facts_gen)
    }
    fn bump_sections_gen(&mut self) {
        self.sections_gen = self.sections_gen.wrapping_add(1);
        self.refresh_tab_shape();
    }
    fn bump_source_facts_gen(&mut self) {
        self.src_facts_gen = self.src_facts_gen.wrapping_add(1);
    }
    fn bump_gen(&mut self) -> u32 {
        self.gen = self.gen.wrapping_add(1);
        self.gen
    }
    fn requery(&mut self) {
        self.bump_gen();
        let current = self.cur();
        if let Some(state) = self.states.get_mut(current) {
            state.fetch = SecFetch::Loading;
            state.total = -1;
            state.items.clear();
            state.cursor = None;
        }
    }
    fn set_cur(&mut self, i: usize) {
        if i >= self.sections.len() || i == self.cur() {
            return;
        }
        self.cur = i;
        self.bump_gen();
        activate_source_of(i);
    }
    fn want(&mut self, lo: usize, hi: usize) {
        self.want = (lo, hi);
    }
    pub(crate) fn resolve_section(&self, epoch: u32, sid: ServerId, key: i64) -> Option<usize> {
        if epoch != self.table_epoch() {
            return None;
        }
        self.sections.iter().enumerate().find_map(|(i, section)| {
            (section.key == key && self.section_sid(i) == Some(sid)).then_some(i)
        })
    }
    fn save_cursor(&mut self, index: usize, cursor: Cursor) -> bool {
        let Some(state) = self.states.get_mut(index) else {
            return false;
        };
        if state.cursor.as_deref() == Some(&cursor) {
            return false;
        }
        state.cursor = Some(Arc::new(cursor));
        true
    }
    fn sorts(&self) -> &[SortEntry] {
        self.cur_state()
            .map(|state| state.sorts.as_slice())
            .unwrap_or(&[])
    }
    fn genres(&self) -> &[GenreEntry] {
        self.cur_state()
            .map(|state| state.genres.as_slice())
            .unwrap_or(&[])
    }
    fn set_sort(&mut self, index: usize) {
        let current = self.cur();
        let Some(state) = self.states.get_mut(current) else {
            return;
        };
        if index >= state.sorts.len() {
            return;
        }
        if index == state.sort_idx {
            state.sort_desc = !state.sort_desc;
        } else {
            state.sort_idx = index;
            state.sort_desc = state.sorts[index].default_desc;
        }
        self.requery();
    }
    fn set_sort_by_key(&mut self, key: &str, desc: bool) -> bool {
        let Some(index) = self.sorts().iter().position(|sort| sort.key == key) else {
            return false;
        };
        self.set_sort(index);
        let current = self.cur();
        if let Some(state) = self.states.get_mut(current) {
            state.sort_desc = desc;
        }
        true
    }
    fn set_unwatched(&mut self, on: bool) -> bool {
        let current = self.cur();
        let Some(state) = self.states.get_mut(current) else {
            return false;
        };
        if state.unwatched == on {
            return true;
        }
        state.unwatched = on;
        self.requery();
        true
    }
    fn set_genre(&mut self, index: Option<usize>) {
        let current = self.cur();
        let Some(state) = self.states.get_mut(current) else {
            return;
        };
        state.genre = index
            .and_then(|i| state.genres.get(i).cloned())
            .map(Arc::new);
        self.requery();
    }
    fn set_genre_by_id(&mut self, id: Option<&str>) -> bool {
        match id {
            None => {
                self.set_genre(None);
                true
            }
            Some(id) => match self.genres().iter().position(|genre| genre.id == id) {
                Some(index) => {
                    self.set_genre(Some(index));
                    true
                }
                None => false,
            },
        }
    }
    fn set_watched_local(&mut self, sid: ServerId, rk: &str, on: bool) -> bool {
        let mut hit = false;
        for state in &mut self.states {
            hit |= state.items.set_watched(sid, rk, on);
        }
        hit
    }
    fn note_library_choice(&mut self, i: usize) {
        let (Some(kind), Some(section)) = (self.section_kind(i), self.sections.get(i)) else {
            return;
        };
        let Some(machine) = self
            .sources
            .get(section.src)
            .map(|source| source.machine_id.clone())
        else {
            return;
        };
        let key = section.key;
        self.remembered
            .retain(|(remembered, _, _)| *remembered != kind);
        if !machine.is_empty() {
            self.remembered.push((kind, machine.clone(), key));
        }
        let user = crate::plex::session::current_profile_key();
        let wire = kind.wire();
        crate::plex::session::update(|current| {
            let mut next = current.clone();
            let slot = match next
                .last_library
                .iter_mut()
                .find(|library| library.user == user)
            {
                Some(library) => library,
                None => {
                    next.last_library.push(crate::plex::session::LastLibrary {
                        user: user.clone(),
                        libs: Vec::new(),
                    });
                    next.last_library.last_mut()?
                }
            };
            slot.set(wire, &machine, key);
            Some(next)
        });
    }
    fn cur_source_idx(&self) -> Option<usize> {
        self.sections
            .get(self.cur())
            .map(|section| section.src)
            .or_else(|| {
                let sid = crate::plex::current_server();
                self.sources.iter().position(|source| source.sid == sid)
            })
            .filter(|&index| index < self.sources.len())
    }
    fn retry_cur_source(&mut self) {
        self.retry_cd = 0;
        if let Some(index) = self.cur_source_idx() {
            if let Some(source) = self.sources.get_mut(index) {
                source.retry_cd = 0;
            }
        }
    }
    fn retry_source(&mut self, epoch: u32, sid: ServerId) -> bool {
        if self.table_epoch() != epoch {
            return false;
        }
        let Some(index) = self.sources.iter().position(|source| source.sid == sid) else {
            return false;
        };
        if self.cur_source_idx() == Some(index) {
            self.retry_cd = 0;
        }
        self.sources[index].retry_cd = 0;
        true
    }
    fn apply_source_outcome(
        &mut self,
        src: usize,
        client: &'static crate::plex::Client,
        outcome: crate::plex::probe::Outcome,
    ) -> bool {
        if self.sources.get(src).map(|source| source.sid) != Some(client.id()) {
            return false;
        }
        let next = source_state(Some(outcome));
        let Some(source) = self
            .sources
            .get_mut(src)
            .filter(|source| source.sid == client.id())
        else {
            return false;
        };
        if source.state == next {
            return true;
        }
        source.set_probe_outcome(outcome);
        if outcome == crate::plex::probe::Outcome::Reachable {
            source.retry_cd = 0;
        }
        self.bump_source_facts_gen();
        crate::ui::idle::invalidate();
        true
    }
    fn kick_directory<T: Send + 'static>(
        &self,
        adapter: &Arc<BrowseAdapter>,
        done: bool,
        flag: fn(&BrowseAdapter) -> &AtomicBool,
        mail: fn(&BrowseAdapter) -> &Mutex<Option<DirectoryResult<T>>>,
        dir: &'static str,
        project: fn(&crate::plex::LibrarySection) -> Option<T>,
    ) {
        let current = self.cur();
        if self.states.get(current).is_none() || done {
            return;
        }
        let Some(sid) = self.section_sid(current) else {
            return;
        };
        let Some(client) = crate::plex::client_for(sid) else {
            return;
        };
        let token_gen = client.token_gen();
        if flag(&adapter).swap(true, Ordering::SeqCst) {
            return;
        }
        let key = self.sections[current].key;
        let epoch = self.table_epoch();
        let worker_adapter = Arc::clone(&adapter);
        let spawned = crate::task::spawn_small("directory", move || {
            let list = catch_unwind(|| {
                let mut values = Vec::new();
                if let Some(container) = client.section_directory(key, dir) {
                    values.extend(container.directory.iter().filter_map(project));
                }
                values
            })
            .unwrap_or_default();
            *mail(&worker_adapter).lock().unwrap_or_else(|e| e.into_inner()) = Some(DirectoryResult {
                epoch,
                sec: current,
                client,
                token_gen,
                list,
            });
        });
        if !spawned {
            flag(&adapter).store(false, Ordering::SeqCst);
        }
    }
    fn kick_genres(&self, adapter: &Arc<BrowseAdapter>) {
        let done = self
            .cur_state()
            .map(|state| state.genres_done)
            .unwrap_or(true);
        self.kick_directory(adapter, done, |a| &a.genre_fetching, |a| &a.genre_result, "genre", |directory| {
            (!directory.key.is_empty() && !directory.title.is_empty()).then(|| GenreEntry {
                id: directory.key.clone(),
                title: directory.title.clone(),
            })
        });
    }
    fn kick_letters(&self, adapter: &Arc<BrowseAdapter>) {
        let done = self
            .cur_state()
            .map(|state| state.letters_done)
            .unwrap_or(true);
        self.kick_directory(
            adapter,
            done,
            |a| &a.letters_fetching,
            |a| &a.letter_result,
            "firstCharacter",
            |directory| {
                (!directory.key.is_empty() && directory.size > 0)
                    .then(|| (directory.title.clone(), directory.size))
            },
        );
    }
    pub(crate) fn addressed_with_adapter(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
        target: crate::stores::browse::SectionAddress,
        work: crate::stores::browse::LibraryWork,
    ) -> bool {
        use crate::stores::browse::{LibraryWork, QueryEdit};
        let Some(index) = self.resolve_section(target.epoch, target.sid, target.section) else {
            return false;
        };
        match work {
            LibraryWork::SaveCursor { query, cursor } => {
                query == self.query_gen() && self.save_cursor(index, cursor)
            }
            LibraryWork::Commit {
                select,
                choice,
                query,
            } => {
                let switched = self.cur() != index;
                if select {
                    self.set_cur(index);
                }
                if self.cur() != index {
                    return false;
                }
                if choice {
                    self.note_library_choice(index);
                    if switched {
                        crate::diag::event(crate::diag::schema::DiagEvent::FeatureUsed {
                            feature: crate::diag::schema::Feature::LibrarySwitch,
                        });
                    }
                }
                match query {
                    Some(QueryEdit::Sort { key, desc }) => self.set_sort_by_key(&key, desc),
                    Some(QueryEdit::Unwatched(on)) => self.set_unwatched(on),
                    Some(QueryEdit::Genre(id)) => self.set_genre_by_id(id.as_deref()),
                    None => true,
                }
            }
            LibraryWork::Hubs { may_publish } => {
                self.hubs_kick(index, adapter);
                self.hubs_commit_staged(index, may_publish)
            }
            work => {
                if self.cur() != index {
                    return false;
                }
                match work {
                    LibraryWork::Want { lo, hi } => self.want(lo, hi),
                    LibraryWork::Letters => self.kick_letters(adapter),
                    LibraryWork::Genres => self.kick_genres(adapter),
                    LibraryWork::Retry => self.retry_cur_source(),
                    LibraryWork::Commit { .. }
                    | LibraryWork::Hubs { .. }
                    | LibraryWork::SaveCursor { .. } => unreachable!(),
                }
                true
            }
        }
    }
    fn pinned_count(&self) -> usize {
        self.sections.iter().filter(|s| s.pinned).count()
    }
    #[cfg(test)]
    pub(crate) fn pinned_for_test(&self, index: usize) -> bool {
        self.sections.get(index).is_some_and(|section| section.pinned)
    }
    #[cfg(test)]
    fn toggle_pin(&mut self, index: usize) -> bool {
        if self.pinned_for_test(index) && self.pinned_count() == 1 {
            return false;
        }
        let Some(section) = self.sections.get_mut(index) else {
            return false;
        };
        section.pinned = !section.pinned;
        self.record_pins(true);
        self.bump_sections_gen();
        crate::ui::idle::invalidate();
        true
    }
    #[cfg(test)]
    pub(crate) fn prepare_discovery_replay_for_test(
        &mut self,
        adapter: &BrowseAdapter,
        sid: ServerId,
        epoch: u32,
    ) {
        self.reset_owned(adapter);
        self.sync_roster_owned();
        let source = self.sources.iter_mut().find(|source| source.sid == sid)
            .expect("registered replay source");
        // The replay fixture starts from the same already-known name on both passes. The live
        // pass publishes the landed name into the process registry; deriving the replay seed from
        // that mutated registry would otherwise change `want_name` in frame zero.
        source.name = "original".into();
        source.sections_done = false;
        source.counts_done = true;
        self.epoch = epoch;
        adapter.src_fetching.store(false, Ordering::SeqCst);
    }
    #[cfg(test)]
    pub(crate) fn discovery_policy_for_test(&self, adapter: &BrowseAdapter) -> (bool, u32, bool) {
        let source = self.sources.first().expect("a replay source");
        (
            adapter.src_fetching.load(Ordering::SeqCst),
            source.retry_cd,
            source.sections_done,
        )
    }
    fn library_pins(&self) -> Vec<(usize, i64, bool)> {
        let mut out: Vec<(usize, i64, bool)> = self.sections
            .iter()
            .map(|s| (s.src, s.key, s.pinned))
            .collect();
        let Some(rec) = self.recorded.as_ref() else { return out };
        for (si, src) in self.sources.iter().enumerate() {
            if src.machine_id.is_empty() || self.sections.iter().any(|s| s.src == si) {
                continue;
            }
            for (lib, on) in rec.on.iter().map(|lib| (lib, true))
                .chain(rec.off.iter().map(|lib| (lib, false))) {
                if lib.machine_id == src.machine_id {
                    out.push((si, lib.key, on));
                }
            }
        }
        out
    }
    fn favorite_sections(&self) -> Vec<(ServerId, i64, bool)> {
        self.library_pins().into_iter().filter_map(|(source, key, favorite)| {
            self.sources.get(source).map(|row| (row.sid, key, favorite))
        }).collect()
    }
    fn tab_has_favorite(&self, kind: SecKind) -> bool {
        self.sections.iter().any(|s| s.kind == kind && s.pinned)
    }
    fn tab_kinds(&self) -> impl Iterator<Item = SecKind> + '_ {
        TAB_KINDS.into_iter().filter(|&kind| self.tab_has_favorite(kind))
    }
    fn tab_of_kind(&self, kind: SecKind) -> Option<usize> {
        self.tab_kinds().position(|candidate| candidate == kind)
    }
    fn tab_kind(&self, tab: usize) -> Option<SecKind> {
        self.tab_kinds().nth(tab)
    }
    fn remembered_section(&self, kind: SecKind) -> Option<usize> {
        let want = self.remembered.iter().find(|(candidate, _, _)| *candidate == kind)?;
        self.sections.iter().position(|section| {
            section.kind == kind && section.pinned && section.key == want.2
                && self.sources.get(section.src).map(|source| source.machine_id.as_str())
                    == Some(want.1.as_str())
        })
    }
    fn section_of_kind(&self, kind: SecKind) -> Option<usize> {
        if let Some(section) = self.remembered_section(kind) {
            return Some(section);
        }
        self.sections.iter().enumerate()
            .filter(|(_, section)| section.kind == kind && section.pinned)
            .min_by_key(|(_, section)| {
                !self.sources.get(section.src).map(|source| source.owned).unwrap_or(false)
            })
            .map(|(index, _)| index)
    }
    fn tab_section(&self, tab: usize) -> Option<usize> {
        self.section_of_kind(self.tab_kind(tab)?)
    }
    fn kind_state(&self, kind: SecKind) -> SecFetch {
        if let Some(index) = self.sections.iter().position(|section| section.kind == kind) {
            return self.states.get(index).map(|state| state.fetch).unwrap_or(SecFetch::Loading);
        }
        if self.sources.iter().any(|source| source.reachable() && !source.sections_done)
            || self.sources.is_empty() {
            SecFetch::Loading
        } else if self.sources.iter().any(|source| !source.sections_done) {
            SecFetch::Failed
        } else {
            SecFetch::Ready
        }
    }
    fn section_sid_is_borrowed(&self, index: usize) -> bool {
        self.sections.get(index).and_then(|section| self.sources.get(section.src))
            .map(|source| !source.owned).unwrap_or(false)
    }
    fn source_groups(&self) -> Vec<SrcGroup> {
        self.sources.iter().map(|source| SrcGroup {
            name: source.name.clone(), handle: source.handle.clone(), state: source.state,
            tier: source.tier,
        }).collect()
    }
    fn rows_where(&self, keep: impl Fn(&BrowseSection) -> bool) -> Vec<SrcRow> {
        let last = self.pinned_count() == 1;
        let current = self.cur();
        self.sections.iter().enumerate().filter(|(_, section)| keep(section))
            .map(|(index, section)| SrcRow {
                src: section.src, section: index, title: section.title.clone(),
                count_line: count_line(section.count, section.kind), pinned: section.pinned,
                last_pinned: last && section.pinned, current: index == current,
            }).collect()
    }
    fn all_source_rows(&self) -> Vec<SrcRow> {
        self.rows_where(|_| true)
    }
    #[cfg(test)]
    fn source_rows_for(&self, index: usize) -> Vec<SrcRow> {
        let Some(kind) = self.section_kind(index) else {
            return Vec::new();
        };
        self.rows_where(|section| section.kind == kind && section.pinned)
    }
    #[cfg(test)]
    fn rail_available(&self) -> bool {
        let Some(state) = self.cur_state() else {
            return false;
        };
        let title_asc = match state.sorts.get(state.sort_idx) {
            Some(sort) => sort.key == "titleSort" && !state.sort_desc,
            None => true,
        };
        title_asc && !state.unwatched && state.genre.is_none() && state.letters.len() > 1
    }
    fn discovery_state(&self) -> SecFetch {
        if !self.sections.is_empty() || self.sources.iter().all(|source| source.sections_done) {
            SecFetch::Ready
        } else if self.sources.iter().any(|source| source.reachable() && !source.sections_done)
            || self.sources.is_empty() {
            SecFetch::Loading
        } else {
            SecFetch::Failed
        }
    }
    fn cur_source_state(&self) -> SecFetch {
        let Some(source) = self.cur_source_idx().and_then(|index| self.sources.get(index)) else {
            return SecFetch::Loading;
        };
        if !source.reachable() {
            SecFetch::Failed
        } else if source.sections_done {
            SecFetch::Ready
        } else {
            SecFetch::Loading
        }
    }
    fn load_remembered(&mut self, session: &crate::plex::session::Session, user: &str) {
        self.remembered = session.last_library.iter().find(|library| library.user == user)
            .map(|library| library.libs.iter().filter_map(|target| {
                SecKind::from_wire(&target.kind)
                    .map(|kind| (kind, target.machine_id.clone(), target.key))
            }).collect()).unwrap_or_default();
    }
    fn lib_refs(&self) -> Vec<crate::plex::pins::LibRef<'_>> {
        let owns_type = |kind: SecKind| self.sections.iter().any(|section| {
            section.kind == kind
                && self.sources.get(section.src).map(|source| source.owned).unwrap_or(true)
        });
        self.sections.iter().map(|section| {
            let (machine_id, owned) = self.sources.get(section.src)
                .map(|source| (source.machine_id.as_str(), source.owned)).unwrap_or(("", true));
            crate::plex::pins::LibRef {
                machine_id, key: section.key, owned, own_type: owns_type(section.kind),
            }
        }).collect()
    }
    fn repoint_cur(&mut self) {
        let current = self.cur();
        if self.sections.get(current).map(|section| section.pinned).unwrap_or(false) {
            return;
        }
        let want = self.section_kind(current).and_then(|kind| self.section_of_kind(kind))
            .or_else(|| self.sections.iter().position(|section| section.pinned));
        if let Some(index) = want {
            self.set_cur(index);
        }
    }
    fn resolve_pins_from(&mut self, session: &crate::plex::session::Session, user: &str) {
        self.load_remembered(session, user);
        let record = session.pins_for(user).cloned();
        let want = {
            let libraries = self.lib_refs();
            crate::plex::pins::resolve(&libraries, record.as_ref())
        };
        self.recorded = record;
        let mut moved = false;
        for (index, on) in want.into_iter().enumerate() {
            if let Some(section) = self.sections.get_mut(index) {
                moved |= section.pinned != on;
                section.pinned = on;
            }
        }
        if moved {
            self.repoint_cur();
        }
    }
    fn resolve_pins(&mut self) {
        let session = crate::plex::session::peek();
        let user = crate::plex::session::current_profile_key();
        self.resolve_pins_from(&session, &user);
    }
    fn append_sections_with(&mut self, source: usize, list: Vec<(i64, String, SecKind)>,
        preferences: Option<&crate::plex::session::Session>) {
        let fresh: Vec<_> = list.into_iter().filter(|(key, _, _)| {
            !self.sections.iter().any(|section| section.src == source && section.key == *key)
        }).collect();
        if fresh.is_empty() {
            return;
        }
        for (key, title, kind) in fresh {
            self.sections.push(BrowseSection {
                src: source, key, title, kind, count: -1, pinned: false,
            });
            self.states.push(SecState::default());
        }
        if let Some(session) = preferences {
            self.resolve_pins_from(session, "");
        } else {
            self.resolve_pins();
        }
        self.bump_sections_gen();
        crate::ui::idle::invalidate();
    }
    fn record_pins(&mut self, asked: bool) {
        let libraries = self.lib_refs();
        let on: Vec<bool> = self.sections.iter().map(|section| section.pinned).collect();
        let user = crate::plex::session::current_profile_key();
        let fresh = crate::plex::pins::record(&user, asked, &libraries, &on);
        let mut written = None;
        crate::plex::session::update(|session| {
            let record = crate::plex::pins::carry_forward(
                fresh.clone(), session.pins_for(&user), &libraries);
            let mut next = session.clone();
            next.set_pins_for(&user, record.clone());
            written = Some(record);
            Some(next)
        });
        if let Some(record) = written {
            self.recorded = Some(record);
        }
    }
    fn apply_pins(&mut self, edits: &[(usize, bool)]) {
        let mut changed = false;
        for &(index, on) in edits {
            if let Some(section) = self.sections.get_mut(index) {
                if section.pinned != on {
                    section.pinned = on;
                    changed = true;
                }
            }
        }
        self.record_pins(true);
        if changed {
            self.repoint_cur();
            self.bump_sections_gen();
            crate::ui::idle::invalidate();
        }
    }
    fn retry_discovery(&mut self) {
        for source in &mut self.sources {
            if !source.sections_done {
                source.retry_cd = 0;
            }
        }
        crate::ui::idle::invalidate();
    }
    fn apply_discovery(
        &mut self,
        epoch: u32,
        source_index: usize,
        landing: SrcLanding,
        preferences: Option<&crate::plex::session::Session>,
        adapter: &BrowseAdapter,
    ) -> crate::stores::StoreOutcome {
        crate::ui::idle::invalidate();
        if epoch != self.table_epoch() {
            return Default::default();
        }
        adapter.src_fetching.store(false, Ordering::SeqCst);
        let SrcLanding { client, token_gen, name, what } = landing;
        let ok = match &what {
            SrcWhat::Sections(list) => list.is_some(),
            SrcWhat::Counts(counts) => !counts.is_empty(),
        };
        let fact_name = (!name.is_empty()).then_some(name.as_str());
        let committed = crate::plex::commit_reachability_if_current(
            client.id(), client, token_gen, ok, fact_name, |outcome| {
                if !self.apply_source_outcome(source_index, client, outcome) {
                    return false;
                }
                if !name.is_empty() {
                    if let Some(source) = self.source_mut(source_index) {
                        source.name = name.clone();
                    }
                    self.bump_source_facts_gen();
                }
                match what {
                    SrcWhat::Sections(list) => {
                        let answered = list.is_some();
                        self.append_sections_with(
                            source_index, list.unwrap_or_default(), preferences);
                        if let Some(source) = self.source_mut(source_index) {
                            source.sections_done = answered;
                            source.retry_cd = if answered { 0 } else { SRC_RETRY_CD };
                        }
                        if !answered {
                            let who = self.sources.get(source_index)
                                .map(|source| source.name.clone()).unwrap_or_default();
                            crate::log(&format!(
                                "browse: source {source_index} ({who}) did not answer — its group reads unreachable"
                            ));
                        }
                    }
                    SrcWhat::Counts(counts) => {
                        let answered = !counts.is_empty();
                        for section in self.sections.iter_mut()
                            .filter(|section| section.src == source_index) {
                            if let Some((_, count)) =
                                counts.iter().find(|(key, _)| *key == section.key) {
                                section.count = *count;
                            }
                        }
                        if answered {
                            self.bump_source_facts_gen();
                        }
                        if let Some(source) = self.source_mut(source_index) {
                            source.counts_done = answered;
                            source.retry_cd = if answered { 0 } else { SRC_RETRY_CD };
                        }
                    }
                }
                true
            },
        );
        if committed != Some(true) {
            return Default::default();
        }
        let mut endpoints = crate::stores::EndpointRefreshSet::default();
        if !ok {
            endpoints.insert(crate::stores::EndpointRefresh { sid: client.id() });
        }
        crate::stores::StoreOutcome { changed: true, endpoints }
    }
    fn refresh_tab_shape(&mut self) {
        let mask = TAB_KINDS.iter().enumerate().fold(0u32, |mask, (i, &kind)| {
            if self.tab_has_favorite(kind) {
                mask | (1 << i)
            } else {
                mask
            }
        });
        if mask != self.tab_shape {
            self.tab_shape = mask;
            self.tabs_gen = self.tabs_gen.wrapping_add(1);
        }
    }
    fn tabs_gen(&self) -> u32 {
        self.tabs_gen
    }
    #[cfg(test)]
    fn reset(&mut self) {
        self.reset_with(|| {});
    }
    fn reset_with(&mut self, clear_adapters: impl FnOnce()) {
        self.bump_gen();
        self.sections_gen = self.sections_gen.wrapping_add(1);
        self.epoch = self.epoch.wrapping_add(1);
        // Preserve the legacy reset's ordering: identities retire first, then adapter mailboxes
        // and claims, then the visible tables/profile memory are cleared.
        clear_adapters();
        self.sources = Vec::new();
        self.sections = Vec::new();
        self.states = Vec::new();
        self.recorded = None;
        self.remembered = Vec::new();
        self.tab_shape = u32::MAX;
        self.cur = 0;
        self.retry_cd = 0;
        self.refresh_tab_shape();
    }
    fn reset_owned(&mut self, adapter: &BrowseAdapter) {
        self.reset_with(|| adapter.clear());
    }
    pub(crate) fn sync_roster_owned(&mut self) -> RosterSync {
        let live: Vec<ServerId> = crate::plex::server_ids().collect();
        let retire_adapter = self.sources.iter().any(|source| !live.contains(&source.sid));
        let mut changed = retire_adapter;
        if retire_adapter {
            self.reset_with(|| {});
        }
        let known = self.sources.len();
        for sid in live {
            match self.sources.iter().position(|source| source.sid == sid) {
                Some(index) => {
                    let now = crate::plex::client_for(sid);
                    let client_addr = now.map_or(0, |client| client as *const _ as usize);
                    let token_gen = now.map_or(0, |client| client.token_gen());
                    let mut changes = 0;
                    let source = &mut self.sources[index];
                    if source.client_addr != client_addr || source.token_gen != token_gen {
                        source.client_addr = client_addr;
                        source.token_gen = token_gen;
                        source.sections_done = false;
                        source.counts_done = false;
                        source.retry_cd = 0;
                        changes += 1;
                    }
                    if let Some((state, tier)) = source_snapshot(sid) {
                        if source.state != state || source.tier != tier {
                            source.state = state;
                            source.tier = tier;
                            changes += 1;
                        }
                    }
                    if let Some(facts) = crate::plex::server_facts(sid) {
                        if source.name.is_empty() && !facts.name.is_empty() {
                            source.name = facts.name.clone();
                            changes += 1;
                        }
                        if source.handle != facts.handle || source.owned != facts.owned {
                            source.handle = facts.handle.clone();
                            source.owned = facts.owned;
                            changes += 1;
                        }
                    }
                    let machine_id = machine_of(sid);
                    if source.machine_id != machine_id {
                        source.machine_id = machine_id;
                        changes += 1;
                    }
                    self.src_facts_gen = self.src_facts_gen.wrapping_add(changes);
                    if changes != 0 {
                        changed = true;
                        crate::ui::idle::invalidate();
                    }
                }
                None => {
                    let facts = crate::plex::server_facts(sid);
                    let owned = facts.map(|facts| facts.owned).unwrap_or(true);
                    let (name, handle) = facts.map(|facts| {
                        (facts.name.clone(), facts.handle.clone())
                    }).unwrap_or_default();
                    let Some((state, tier)) = source_snapshot(sid) else { continue };
                    self.sources.push(BrowseSource {
                        sid,
                        client_addr: crate::plex::client_for(sid)
                            .map_or(0, |client| client as *const _ as usize),
                        token_gen: crate::plex::client_for(sid)
                            .map_or(0, |client| client.token_gen()),
                        machine_id: machine_of(sid),
                        owned, name, handle, state, tier,
                        sections_done: false, counts_done: false, retry_cd: 0,
                    });
                    self.bump_source_facts_gen();
                    changed = true;
                }
            }
        }
        if self.sources.len() != known {
            crate::log(&format!("browse: roster now {} source(s)", self.sources.len()));
        }
        RosterSync { changed, retire_adapter }
    }
    fn recheck_shares(&mut self) {
        for source in &mut self.sources {
            source.retry_cd = 0;
            source.sections_done = false;
            source.counts_done = false;
        }
        crate::ui::idle::invalidate();
    }
    pub(crate) fn run_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
        cmd: crate::stores::browse::BrowseCmd,
    ) -> bool {
        use crate::stores::browse::BrowseCmd;
        match cmd {
            BrowseCmd::Discovery(result) => {
                record::apply_to(self, adapter, &result, None).changed
            }
            BrowseCmd::Addressed { target, work } => {
                self.addressed_with_adapter(adapter, target, work)
            }
            BrowseCmd::RetrySource { epoch, sid } => self.retry_source(epoch, sid),
            #[cfg(test)]
            BrowseCmd::SetCur(index) => {
                self.set_cur(index);
                true
            }
            BrowseCmd::RecheckShares => {
                self.recheck_shares();
                true
            }
            BrowseCmd::ApplyPins(edits) => {
                self.apply_pins(&edits);
                true
            }
            BrowseCmd::RetryDiscovery => {
                self.retry_discovery();
                true
            }
            BrowseCmd::Reset => {
                self.reset_owned(adapter);
                true
            }
            BrowseCmd::HubsInvalidateAll => {
                self.hubs_invalidate_all(adapter);
                true
            }
            BrowseCmd::SetWatchedLocal { sid, rk, on } => {
                let listing = self.set_watched_local(sid, &rk, on);
                let hubs = self.hubs_set_watched_local(sid, &rk, on);
                listing || hubs
            }
            BrowseCmd::LeftTheDeck { sid, rk } => self.hubs_left_the_deck(sid, &rk),
        }
    }
    fn maybe_discover_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
        launch: &mut dyn FnMut(DiscoveryRequest) -> bool,
    ) {
        for source in &mut self.sources {
            source.retry_cd = source.retry_cd.saturating_sub(1);
        }
        if adapter.src_fetching.load(Ordering::SeqCst) {
            return;
        }
        let ready = |source: &BrowseSource| source.retry_cd == 0;
        let mut pick = self.sources.iter().enumerate().find_map(|(index, source)| {
            (ready(source) && !source.sections_done).then(|| {
                (index, source.sid, SrcJob::Sections, source.name.is_empty())
            })
        });
        if pick.is_none() {
            for index in 0..self.sources.len() {
                if !ready(&self.sources[index]) || self.sources[index].counts_done {
                    continue;
                }
                let keys: Vec<i64> = self.sections.iter()
                    .filter(|section| section.src == index).map(|section| section.key).collect();
                if keys.is_empty() {
                    self.sources[index].counts_done = true;
                    continue;
                }
                pick = Some((index, self.sources[index].sid, SrcJob::Counts(keys),
                    self.sources[index].name.is_empty()));
                break;
            }
        }
        let Some((source, sid, job, want_name)) = pick else { return };
        let Some(client) = crate::plex::client_for(sid) else { return };
        let request = DiscoveryRequest {
            epoch: self.table_epoch(), si: source, client, token_gen: client.token_gen(),
            job, want_name, adapter: Arc::clone(adapter),
        };
        adapter.src_fetching.store(true, Ordering::SeqCst);
        if !launch(request) {
            self.discovery_spawn_refused_owned(adapter, source);
        }
    }
    fn discovery_spawn_refused_owned(&mut self, adapter: &BrowseAdapter, source: usize) {
        adapter.src_fetching.store(false, Ordering::SeqCst);
        if let Some(source) = self.source_mut(source) {
            source.retry_cd = SRC_RETRY_CD;
        }
    }
    fn land_discovery_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
    ) -> crate::stores::StoreOutcome {
        let taken = crate::stores::take_landing(crate::stores::StoreId::Browse, || {
            adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()).take()
        });
        let Some((epoch, source, landing)) = taken else { return Default::default() };
        self.apply_discovery(epoch, source, landing, None, adapter)
    }
    fn land_directory_owned<T>(
        &mut self,
        flag: &AtomicBool,
        mail: &Mutex<Option<DirectoryResult<T>>>,
        apply: impl FnOnce(&mut SecState, Vec<T>),
    ) -> bool {
        let taken = crate::stores::take_landing(crate::stores::StoreId::Browse, || {
            mail.lock().unwrap_or_else(|e| e.into_inner()).take()
        });
        let Some(result) = taken else { return false };
        crate::ui::idle::invalidate();
        flag.store(false, Ordering::SeqCst);
        if result.epoch != self.table_epoch() {
            return false;
        }
        let DirectoryResult { sec, client, token_gen, list, .. } = result;
        crate::plex::commit_if_current(client.id(), client, token_gen, || {
            if self.section_sid(sec) == Some(client.id()) {
                if let Some(state) = self.state_mut(sec) {
                    apply(state, list);
                    return true;
                }
            }
            false
        }).unwrap_or(false)
    }
    fn maybe_spawn_owned(&mut self, adapter: &Arc<BrowseAdapter>) {
        if adapter.fetching.load(Ordering::SeqCst) || self.retry_cd > 0 {
            return;
        }
        let current = self.cur();
        let Some(state) = self.states.get(current) else { return };
        let Some(section) = self.sections.get(current) else { return };
        let start = if state.total < 0 {
            0
        } else {
            let (lo, hi) = self.want;
            let hi = hi.min(state.total as usize);
            let mut page = (lo / PAGE) * PAGE;
            let mut found = None;
            while page < hi {
                if state.items.page_missing(page / PAGE) {
                    found = Some(page);
                    break;
                }
                page += PAGE;
            }
            let Some(found) = found else { return };
            found
        };
        let include_meta = state.sorts.is_empty();
        let sort = state.sorts.get(state.sort_idx)
            .map(|sort| format!("{}:{}", sort.key,
                if state.sort_desc { "desc" } else { "asc" }))
            .unwrap_or_default();
        let mut filters = Vec::new();
        if state.unwatched {
            filters.push((match section.kind {
                SecKind::Show => "unwatchedLeaves",
                SecKind::Movie => "unwatched",
            }.to_string(), "1".to_string()));
        }
        if let Some(genre) = &state.genre {
            filters.push(("genre".to_string(), genre.id.clone()));
        }
        let gen = self.query_gen();
        let key = section.key;
        let Some(sid) = self.section_sid(current) else { return };
        let Some(client) = crate::plex::client_for(sid) else { return };
        let token_gen = client.token_gen();
        adapter.fetching.store(true, Ordering::SeqCst);
        let worker_adapter = Arc::clone(adapter);
        let spawned = crate::task::spawn_small("page", move || {
            let (items, total, sorts) = catch_unwind(|| {
                let query = SectionQuery {
                    section_key: key, sort: &sort, filters: &filters,
                    start: start as i64, size: PAGE as i64, include_meta,
                };
                let Some(container) = client.section_items_query(&query) else {
                    return (Vec::new(), -1, None);
                };
                let items = container.metadata.iter().map(|item| parse_item(item, sid)).collect();
                let total = if container.total_size > 0 {
                    container.total_size
                } else {
                    start as i64 + container.metadata.len() as i64
                };
                let sorts = container.meta.as_ref().and_then(|meta| {
                    meta.types.iter().find(|kind| kind.active != 0)
                        .or_else(|| meta.types.first()).map(|kind| kind.sort.iter()
                            .filter(|sort| !sort.key.is_empty()).map(|sort| SortEntry {
                                key: sort.key.clone(),
                                title: if sort.title.is_empty() {
                                    sort.key.clone()
                                } else {
                                    sort.title.clone()
                                },
                                default_desc: sort.default_direction == "desc",
                            }).collect())
                });
                (items, total, sorts)
            }).unwrap_or((Vec::new(), -1, None));
            *worker_adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(PageResult {
                    client, token_gen, gen, sec: current, start, items, total, sorts,
                });
        });
        if !spawned {
            adapter.fetching.store(false, Ordering::SeqCst);
        }
    }
    pub(crate) fn controlled_discover_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
        launch: &mut dyn FnMut(DiscoveryRequest) -> bool,
    ) {
        self.maybe_discover_owned(adapter, launch);
    }
    pub(crate) fn discover_pump_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
    ) -> crate::stores::StoreOutcome {
        let outcome = self.land_discovery_owned(adapter);
        self.maybe_discover_owned(adapter, &mut execute_discovery);
        outcome
    }
    pub(crate) fn pump_owned(
        &mut self,
        adapter: &Arc<BrowseAdapter>,
    ) -> crate::stores::StoreOutcome {
        let mut changed = false;
        self.retry_cd = self.retry_cd.saturating_sub(1);
        let discovery = self.land_discovery_owned(adapter);
        changed |= discovery.changed;
        let endpoints = discovery.endpoints;
        self.maybe_discover_owned(adapter, &mut execute_discovery);
        changed |= self.hubs_land(adapter);
        changed |= self.hubs_tick_all(adapter);
        changed |= self.land_directory_owned(
            &adapter.genre_fetching, &adapter.genre_result, |state, list| {
            state.genres_done = true;
            if state.genres.is_empty() {
                state.genres = Arc::new(list);
            }
        });
        changed |= self.land_directory_owned(
            &adapter.letters_fetching, &adapter.letter_result, |state, list| {
                state.letters_done = true;
                if state.letters.is_empty() {
                    state.letters = Arc::new(list);
                }
            });
        let page = crate::stores::take_landing(crate::stores::StoreId::Browse, || {
            adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()).take()
        });
        if let Some(result) = page {
            crate::ui::idle::invalidate();
            adapter.fetching.store(false, Ordering::SeqCst);
            if let Some(source) = self.sections.get(result.sec).map(|section| section.src) {
                let client = result.client;
                let token_gen = result.token_gen;
                let _ = crate::plex::commit_reachability_if_current(
                    client.id(), client, token_gen, result.total >= 0, None, |outcome| {
                        if !self.apply_source_outcome(source, client, outcome) {
                            return false;
                        }
                        if result.total < 0 {
                            self.retry_cd = 120;
                            if result.gen == self.query_gen() {
                                if let Some(state) = self.state_mut(result.sec) {
                                    if state.fetch != SecFetch::Failed {
                                        state.fetch = SecFetch::Failed;
                                        changed = true;
                                    }
                                }
                            }
                        } else if result.gen == self.query_gen() {
                            if let Some(state) = self.state_mut(result.sec) {
                                state.fetch = SecFetch::Ready;
                                if let Some(sorts) = result.sorts {
                                    if state.sorts.is_empty() {
                                        state.sorts = Arc::new(sorts);
                                    }
                                }
                                if state.total != result.total {
                                    state.total = result.total;
                                    state.items.resize(state.total as usize);
                                }
                                for (offset, item) in result.items.into_iter().enumerate() {
                                    state.items.set(result.start + offset, item);
                                }
                                changed = true;
                            }
                        }
                        true
                    });
            }
        }
        self.maybe_spawn_owned(adapter);
        crate::stores::StoreOutcome { changed, endpoints }
    }
}

// ---- fetch plumbing (generation + single-flight + mailboxes) --------------------------------

/// Bumped whenever the section table's SHAPE changes — a source's sections appended, or the whole
/// table wiped by [`reset`]. Label/measurement caches keyed on the table (the tab strip's pill
/// widths, the rail's letters) invalidate on it. Because the table only ever GROWS, a cache keyed
/// on this is complete: no existing entry can have changed under it.
/// The table's IDENTITY epoch — bumped by [`reset`] and by nothing else, i.e. exactly when the
/// signed-in account changes and every index in the table stops meaning what it meant.
///
/// Landings blamed on a section INDEX gate on this rather than on the section-shape generation: an APPEND from
/// one source must not discard a landing in flight for another, and it cannot invalidate one
/// either, because appending never moves an existing index.
/// Frames left before another page fetch may spawn after a FAILED one (main-thread; pump
/// decrements). Stops a fast-failing network from spawning a worker per frame.

struct PageResult {
    /// Exact registry lifecycle the worker dialled. Section/query generations do not move when a
    /// slot is re-pointed or retokened, so both pointer identity and token generation are needed.
    client: &'static crate::plex::Client,
    token_gen: u32,
    gen: u32,
    sec: usize,
    start: usize,
    items: Vec<PmsMovie>,
    /// totalSize of the listing; **negative = the fetch FAILED** — pump must not touch the
    /// store (a transient network error once wiped a whole populated section to "empty").
    total: i64,
    sorts: Option<Vec<SortEntry>>, // Some when the fetch carried includeMeta=1
}
// menu-data landings carry the table EPOCH so a landing spawned before a [`reset`] (profile
// switch) can never populate the NEW user's state at the same index
struct DirectoryResult<T> {
    epoch: u32,
    sec: usize,
    client: &'static crate::plex::Client,
    token_gen: u32,
    list: Vec<T>,
}

/// What a source-discovery worker brings back, per SOURCE — named by its index, which appending
/// can never move.
///
/// `name` rides EVERY landing because either worker phase may be the first response that teaches
/// us the server's own display name.
#[derive(Clone)]
struct SrcLanding {
    /// Exact registry lifecycle the worker dialled. Slot id alone survives both re-point and
    /// profile changes; pointer identity catches the former and token_gen catches in-place retoken.
    client: &'static crate::plex::Client,
    token_gen: u32,
    /// `GET /`'s `friendlyName`, or "" when it was already known or the server did not answer
    name: String,
    what: SrcWhat,
}
#[derive(Clone)]
enum SrcWhat {
    /// `GET /library/sections`. `None` is the FAILURE sentinel — the source is marked unreachable
    /// and whatever sections it had already contributed are left exactly where they are.
    Sections(Option<Vec<(i64, String, SecKind)>>),
    /// The unfiltered item count per library, **by section KEY** rather than by index: the table
    /// may have grown between the spawn and the landing, and a key is stable inside one source.
    Counts(Vec<(i64, i64)>),
}

/// Legacy synchronous harness retained only for lifecycle regression fixtures. Production section
/// discovery is uniformly `kick → worker → mailbox → commit` via [`discover_pump`].
#[cfg(test)]
fn ensure_sections_with(
    state: &mut BrowseState,
    fetch: impl FnOnce(&crate::plex::Client) -> Option<Vec<(i64, String, SecKind)>>,
) -> usize {
    state.sync_roster_owned();
    let cur_sid = crate::plex::current_server();
    let Some(si) = state.sources().iter().position(|s| s.sid == cur_sid) else {
        return state.sections().len();
    };
    if state.sources()[si].sections_done {
        return state.sections().len();
    }
    let Some(client) = crate::plex::client_for(cur_sid) else {
        return state.sections().len();
    };
    let token_gen = client.token_gen();
    let found = catch_unwind(AssertUnwindSafe(|| fetch(client))).unwrap_or(None);
    let ok = found.is_some();
    let committed = crate::plex::commit_reachability_if_current(
        cur_sid,
        client,
        token_gen,
        ok,
        None,
        |outcome| {
            if state.sources().get(si).map(|s| s.sid) != Some(client.id()) {
                return false;
            }
            state.append_sections_with(si, found.unwrap_or_default(), None);
            if let Some(s) = state.source_mut(si) {
                s.sections_done = ok;
                s.set_probe_outcome(outcome);
                s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
            }
            true
        },
    );
    if committed != Some(true) {
        return state.sections().len();
    }
    state.sections().len()
}

/// `MediaContainer.Directory[]` → the (key, title, kind) rows this app can browse. The ONE
/// projection, shared by the blocking discovery above and the worker below, so the two can never
/// disagree about which sections exist.
///
/// `artist`/`photo` are KEPT. They used to be dropped here as "not browsable", and that quietly
/// disabled the one growth case the tab projection is written for: a friend sharing a type you do
/// not own can only add a pill if that type reaches the table at all. They browse like any other
/// section (the listing, its server-driven sorts and the A–Z rail are type-agnostic), and this
/// account's own `/hubs` already puts a *Recently Added Music* shelf on Home, so the content was at
/// the top level before it had a tab. What is still missing is the level BELOW the grid — an artist
/// opens the movie detail page, which has nothing to play — and that belongs to whoever builds the
/// music level, not to the strip.
fn project_sections(mc: &crate::plex::MediaContainer) -> Vec<(i64, String, SecKind)> {
    mc.directory
        .iter()
        .filter_map(|d| {
            let kind = SecKind::from_wire(&d.kind)?; // a type this product has no level for at all
            d.key
                .parse::<i64>()
                .ok()
                .map(|k| (k, d.title.clone(), kind))
        })
        .collect()
}

/// A library section's TYPE — the product's closed type list, and the unit the tab projection
/// ([`tabs`]) compares by.
///
/// It replaced an `is_show: bool`, and the reason is the projection rather than tidiness: "does any
/// owned library have this kind" is the test that decides whether a friend's library gets its own
/// pill, and with one value a second type would silently ride the *Movies* pill and its content
/// would be unreachable from the strip.
///
/// **Movies and shows are the whole list, deliberately.** `artist` and `photo` briefly appeared
/// here — the reasoning was that a friend sharing a type you do not own is the growth case the tab
/// projection exists for, and dropping those at the wire made that case unreachable. It shipped, and
/// a *Music* tab duly appeared on the dev set (owner verdict, 2026-08-14: "Music was just a tab in a
/// mockup — remove it completely"). The reasoning was sound and the conclusion was still wrong,
/// because the growth case is only worth reaching for a type the app can actually PLAY: below the
/// grid an artist opens the movie detail page, which has nothing to play, so the pill led to a dead
/// end that looked like a feature. Re-add a variant here in the commit that builds its level, not
/// before — the projection is ready for it and needs no change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SecKind {
    Movie,
    Show,
}

impl SecKind {
    /// The wire's `Directory.type`, or `None` for a type this product draws no level for —
    /// `artist`, `photo`, and everything else PMS can serve. See the type's own doc: this returning
    /// `None` is what keeps an unplayable library out of the strip, the Sources panel and the grid
    /// in one place, rather than at three call sites that can disagree.
    pub(crate) fn from_wire(s: &str) -> Option<SecKind> {
        match s {
            "movie" => Some(SecKind::Movie),
            "show" => Some(SecKind::Show),
            _ => None,
        }
    }
    /// The wire code this type was parsed FROM — the inverse of [`from_wire`](Self::from_wire), and
    /// the key `plex::session::TypedLib` records a remembered library under. A `&'static str` from
    /// this table rather than an enum discriminant, so a reordered `SecKind` cannot silently
    /// repoint a record an older build wrote.
    pub(crate) fn wire(self) -> &'static str {
        match self {
            SecKind::Movie => "movie",
            SecKind::Show => "show",
        }
    }
    /// The Sources row's count noun ("187 films") — plural, and the singular-less form the row
    /// falls back to when no count has landed is [`SecKind::plural`].
    pub(crate) fn noun(self) -> &'static str {
        match self {
            SecKind::Movie => "films",
            SecKind::Show => "shows",
        }
    }
    /// The same thing as a standalone label ("Films"), for a row whose count has not landed yet.
    pub(crate) fn plural(self) -> &'static str {
        match self {
            SecKind::Movie => "Films",
            SecKind::Show => "TV shows",
        }
    }
}

/// Point the app's CURRENT server at the source of section `i`, and drop the per-server state that
/// belonged to the old one.
///
/// **This is the seam that per-item `ServerId` retires** (`docs/shared-servers.md` §5 steps 2–3),
/// and it is here because without it the Sources list is a trap rather than a feature. The grid
/// itself is fetched through `client_for(sid)`, but a `PmsMovie` carries no server: `posters` fetch
/// from `client()`, and OK on a card resolves its ratingKey through `client()` too — and ratingKeys
/// are server-local, so a friend's card would quietly open, and play, a DIFFERENT title of yours
/// with the same number. Moving `current` with the browsed library makes every one of those agree
/// again, which is `docs/shared-servers.md` §5's named "cheap variant": one active server at a time.
///
/// What it costs is stated rather than hidden: Home's catalog belongs to the server it was fetched
/// from, so it is dropped and re-armed (`pms::reset` — `pms::pump` refetches on the next frames,
/// asynchronously, so nothing blocks), and the person page's shelves with it. The poster memo needs
/// no help: it compares a token generation, and two servers never share one (`plex::servers`).
fn activate_source_of(_i: usize) {
    // **Browsing a friend's library does NOT re-point the app.** This used to `set_current` to that
    // section's server and then wipe Home's catalog, the person store and the PlayQueue identity —
    // which is exactly what the owner hit on the device (2026-08-14): opening a shared library
    // replaced the whole Home page with the friend's content, and once left the tab strip showing
    // only their library, because `ensure_sections` discovers the CURRENT server and the strip had
    // just been re-pointed at theirs.
    //
    // It was a deliberate stopgap and it said so: `PmsMovie` carried no `ServerId`, so OK on a
    // borrowed card would have opened one of OUR films with the same ratingKey, and re-pointing was
    // the cheap way to make the ids line up. Threading `ServerId` through the stored rows retired
    // it — the promise its own comment made. Every consumer now addresses the server by DATA:
    // the page fetch dials `client_for(section_sid(..))`, rows are stamped at parse, and
    // `open_library_card` opens `to_detail(mm.sid, &mm.rk)`.
    //
    // "Current" is the SESSION's server — whose Home you are on, whose PlayQueue identity is in
    // play. Browsing is not a session change, and the two only looked like one thing while there
    // was a single server. Kept as a named no-op rather than deleted at the call site so the next
    // person to reach for a re-point here finds this note first.
}
// ---- the TAB projection: which sections get a pill in the shared top strip -------------------
//
// **A pill is a TYPE, never a person.** Discovery resolves each type to an owned library first,
// then the first shared library of that type.
//
// The consequence is the property B was written for: the strip is a constant width at one friend
// or at ten. Put source in the strip instead and three friends measure 2133px against a 1540 track.
// Source lives in the Library chip instead, so adding people never reshapes this strip.
//
// **What DOES reshape it is the favourite switch, and that is the change of 2026-09-05.** Movies
// and TV Shows were permanent destinations — `tab_count` was a constant 2 and `tabs_gen` a
// constant 1 — because the switch governed Home alone. It governs the whole app now, so a type
// whose last favourite library is switched off draws no pill: keeping the pill and drawing an
// empty screen behind it is the "tab that leads to nothing" the design rejects, arrived at from
// the other direction, and the switch is the user's own instruction not to be shown that content.
//
// The cost is that a strip POSITION is no longer a stable name for a destination. That is why
// `ui::widgets::Pill::Section` carries a `SecKind` and not a tab index, and why anything holding a
// strip cursor across frames must hold a `Pill` rather than a `usize`.

/// Every type this product has a pill for, in strip order. A pill is only DRAWN when the
/// favourite set can fill it — see [`tab_kinds`] — so this is the vocabulary, not the strip.
pub(crate) const TAB_KINDS: [SecKind; 2] = [SecKind::Movie, SecKind::Show];

// ---- favourites: the ONE control, it governs the WHOLE APP, and it is PER PROFILE ------------
//
// The user reads it as **Favorite libraries**; the identifier here stays `pinned` and the persisted
// key stays `home_pins`, deliberately — renaming the key would break ROLLBACK rather than upgrade
// (a serde alias reads the old name, but the next whole-`Session` write emits only the new one and
// an older build then silently applies defaults).
//
// **It governed Home ALONE until 2026-09-05 and this header said so.** Owner's direction: the
// setting affects the whole app. Three browsing surfaces read it now — Home's shelves
// (`pms::item_pinned`), the top tab STRIP (`tab_has_favorite`: a type with no favourite draws no
// pill at all) and the Library's own Sources picker (`source_rows`). `all_source_rows` stays
// unscoped and is the Favorite libraries editor's list, which is the one way a non-favourite comes
// back. Search deliberately stays GRANT-scoped and only RANKS by this: a browsing preference is not
// an authorization boundary, and removing results would turn "I don't browse this often" into
// "this does not exist".
//
// The rules are `plex::pins` — pure, host-graded, and deliberately holding no store. This half is
// the plumbing: project the section table into what those rules take, apply what they answer, and
// persist an answer against the profile that gave it.
//
// **Per profile is the whole shape.** The persisted selection used to hang off the `Session`,
// which is one per INSTALL, so a household could hold exactly one opinion about a friend's films.
// Owner's ruling, 2026-08-21: "it is separate for each profile." A switch needs no code of its own
// to honour it — `install_pms` calls [`reset`], discovery re-runs, and [`resolve_pins`] reads the
// NEW profile's record — which is exactly why the resolve is a whole-table function rather than a
// per-row default applied once at append.

/// **The current profile's persisted answer, as last read from disk** — the half of the selection
/// the section table cannot express.
///
/// The table only ever holds sources that have answered, while Home/Library/Search/Onboard
/// enumerate the granted roster asynchronously. During that window a share can have no row here;
/// `pms::feeds_home` must still recover its recorded answer by machine identity.
/// Their answer is on disk keyed by machine, which is exactly the join that settles it.
///
/// `None` means "nothing has been read yet", never "nothing was recorded" — the same distinction
/// [`library_pins`] and `Session::home_pins` both turn on. Written by [`resolve_pins`] and
/// [`record_pins`], cleared by [`reset`]; never read from the file per frame.

/// One source's `machineIdentifier` as the registry knows it, `""` while nobody has learned it.
fn machine_of(sid: ServerId) -> String {
    crate::plex::client_for(sid)
        .map(|c| c.machine_id().to_string())
        .unwrap_or_default()
}

/// The section table as the pin rules see it, in table order.
// ---- the Sources list's data, projected ------------------------------------------------------
//
// Two plain owned types rather than borrows of the statics, for one reason worth stating: the
// panel's ROW MODEL — which level draws a tick and which draws a word — is the part that must be
// host-tested, and a test can build these by hand. Handing out `&BrowseSource` would make that
// impossible without a live section table.

/// One server's group in the Sources list.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct SrcGroup {
    /// the MACHINE name — the header
    pub(crate) name: String,
    /// the owner's handle — the header's accessory; empty on your own server, where the header
    /// carries no accessory at all
    pub(crate) handle: String,
    /// How its last dial ended — [`SourceState::Unreachable`] dims the WHOLE group, header
    /// included, and states it there.
    ///
    /// Was `reachable: bool`. The renderer still asks the old question through
    /// [`SrcGroup::reachable`]; widening what it can be told is what lets that renderer grow a
    /// third and fourth word without this projection changing again.
    pub(crate) state: SourceState,
    /// Which tier won — local, remote or relay. The Sources list is the surface that should say it:
    /// "relay" explains a 2 Mbit/s ceiling that otherwise reads as a broken server.
    pub(crate) tier: Option<crate::plex::probe::Location>,
}

impl SrcGroup {
    /// The old two-state question. See [`BrowseSource::reachable`] — `NotProbed` answers `true`
    /// here too, and for the same reason: a group nobody has dialled must not open dimmed.
    pub(crate) fn reachable(&self) -> bool {
        !matches!(self.state, SourceState::Unreachable)
    }
}

/// One library row in the Sources list.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct SrcRow {
    /// which group it belongs to
    pub(crate) src: usize,
    /// the section it opens
    pub(crate) section: usize,
    pub(crate) title: String,
    /// "185 films" once the count has landed, else the library's type word
    pub(crate) count_line: String,
    pub(crate) pinned: bool,
    /// the only pinned library left — its value dims and its sub-line states the rule
    pub(crate) last_pinned: bool,
    /// the library being browsed — the Browse level's single tick
    pub(crate) current: bool,
}

fn count_line(count: i64, kind: SecKind) -> String {
    if count >= 0 {
        format!("{count} {}", kind.noun())
    } else {
        kind.plural().to_string()
    }
}

/// Re-check the roster: adopt anything newly registered, and re-arm discovery for every source
/// that failed. The Sources list's last row.
///
/// It cannot ask plex.tv for shares the app was never granted — that fetch belongs to whoever
/// owns the roster ingest, and this is where it hooks in. What it does today is the half that is
/// ours: a friend who has switched their server back on stops being unreachable on the next pump
/// instead of after the ten-second backoff.
// ---- source discovery, off the main thread ---------------------------------------------------

/// What a discovery worker is being asked for. Two phases per source, one worker at a time within
/// the owning BrowseStore: the roster is a handful of servers and none of it is on a user's
/// critical path.
enum SrcJob {
    /// its section list
    Sections,
    /// the unfiltered item count of each of its libraries, by section key
    Counts(Vec<i64>),
}

pub(crate) struct DiscoveryRequest {
    epoch: u32,
    si: usize,
    client: &'static crate::plex::Client,
    token_gen: u32,
    job: SrcJob,
    want_name: bool,
    adapter: Arc<BrowseAdapter>,
}

impl DiscoveryRequest {
    pub(crate) fn descriptor(&self) -> serde_json::Value {
        serde_json::json!({
            "epoch": self.epoch,
            "source": self.si,
            "sid": self.client.id().raw(),
            "client": self.client.instance_gen(),
            "token_gen": self.token_gen,
            "name": self.want_name,
            "sections": matches!(self.job, SrcJob::Sections),
            "counts": match &self.job {
                SrcJob::Sections => Vec::new(),
                SrcJob::Counts(keys) => keys.clone(),
            },
        })
    }
}

pub(crate) fn execute_discovery(request: DiscoveryRequest) -> bool {
    let DiscoveryRequest {
        epoch,
        si,
        client,
        token_gen,
        job,
        want_name,
        adapter,
    } = request;
    let is_sections = matches!(job, SrcJob::Sections);
    spawn_discovery(move || {
        let landing = catch_unwind(|| {
            // the server naming ITSELF, so a roster that never reached plex.tv still heads its
            // group with a machine name. One request, once, per source.
            let name = if want_name {
                client.friendly_name().unwrap_or_default()
            } else {
                String::new()
            };
            let what = match job {
                SrcJob::Sections => {
                    SrcWhat::Sections(client.sections().map(|mc| project_sections(&mc)))
                }
                SrcJob::Counts(keys) => {
                    let mut out = Vec::new();
                    for k in keys {
                        // size=0: PMS answers with `totalSize` and no items at all, so a
                        // library's count costs a header rather than a page.
                        let q = SectionQuery {
                            section_key: k,
                            sort: "",
                            filters: &[],
                            start: 0,
                            size: 0,
                            include_meta: false,
                        };
                        if let Some(mc) = client.section_items_query(&q) {
                            out.push((k, mc.total_size));
                        }
                    }
                    SrcWhat::Counts(out)
                }
            };
            SrcLanding {
                client,
                token_gen,
                name,
                what,
            }
        })
        .unwrap_or_else(|_| {
            // a panicking fetch is a FAILURE of the job it was doing, never a success of another:
            // reporting a panicked count probe as a failed section list would drop the source's
            // whole library list on the floor.
            let what = if is_sections {
                SrcWhat::Sections(None)
            } else {
                SrcWhat::Counts(Vec::new())
            };
            SrcLanding {
                client,
                token_gen,
                name: String::new(),
                what,
            }
        });
        *adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((epoch, si, landing));
    })
}

fn spawn_discovery(job: impl FnOnce() + Send + 'static) -> bool {
    #[cfg(test)]
    if REFUSE_DISCOVERY_FOR_TEST.with(|flag| flag.get()) {
        return false;
    }
    crate::task::spawn_small("sources", job)
}

#[cfg(test)]
thread_local! {
    static REFUSE_DISCOVERY_FOR_TEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn with_refused_discovery_for_test<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REFUSE_DISCOVERY_FOR_TEST.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(REFUSE_DISCOVERY_FOR_TEST.with(|flag| flag.replace(true)));
    f()
}

/// Supply a transport observation, retaining the actual source epoch and captured lifecycle.
#[cfg(test)]
pub(crate) fn queue_discovery_for_owner_test(
    state: &mut BrowseState,
    adapter: &Arc<BrowseAdapter>,
    client: &'static crate::plex::Client,
    token_gen: u32,
    ok: bool,
) {
    crate::testlock::assert_held("discovery observation fixture");
    let _ = state.sync_roster_owned();
    let si = state.sources.iter().position(|source| source.sid == client.id()).unwrap();
    let what = SrcWhat::Sections(ok.then(Vec::new));
    *adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((
        state.table_epoch(),
        si,
        SrcLanding {
            client,
            token_gen,
            name: String::new(),
            what,
        },
    ));
}

#[cfg(test)]
pub(crate) fn seed_items_for_owner_test(state: &mut BrowseState, n: usize) {
    crate::testlock::assert_held("browse's section table (seed_items_for_test)");
    let c = state.cur();
    let sid = state.section_sid(c).unwrap_or_default();
    if let Some(st) = state.state_mut(c) {
        st.total = n as i64;
        st.items = SecItems::from_vec(
            (0..n)
                .map(|i| {
                    Some(crate::pms::PmsMovie {
                        sid,
                        rk: format!("{}", i + 1),
                        title: format!("Item {i}"),
                        thumb: format!("/t/{i}"),
                        ..Default::default()
                    })
                })
                .collect(),
        );
        st.fetch = SecFetch::Ready;
    }
}

#[cfg(test)]
pub(crate) fn seed_sources_for_owner_test(
    state: &mut BrowseState,
    n: usize,
    reachable: bool,
) {
    crate::testlock::assert_held("an owned browse section table (seed_sources_for_test)");
    state.reset_with(|| {});
    let current = crate::plex::current_server();
    state.sources = (0..n)
        .map(|index| BrowseSource {
            sid: if index == 0 {
                current
            } else {
                ServerId::from_raw(index as u16)
            },
            client_addr: 0,
            token_gen: 0,
            machine_id: format!("mach-{index}"),
            owned: index == 0,
            name: if index == 0 {
                "nas-home".into()
            } else {
                "film-club".into()
            },
            handle: if index == 0 {
                String::new()
            } else {
                "friend".into()
            },
            state: if reachable {
                SourceState::Reachable
            } else {
                SourceState::Unreachable
            },
            tier: None,
            sections_done: reachable,
            counts_done: true,
            retry_cd: 0,
        })
        .collect();
}

#[cfg(test)]
pub(crate) fn seed_pins_for_owner_test(state: &mut BrowseState, pinned: &[bool]) {
    crate::testlock::assert_held("an owned browse section table (seed_pins_for_test)");
    state.reset_with(|| {});
    state.sources = vec![BrowseSource {
        sid: crate::plex::current_server(),
        client_addr: 0,
        token_gen: 0,
        machine_id: "mach-test".into(),
        owned: true,
        name: "nas-home".into(),
        handle: String::new(),
        state: SourceState::Reachable,
        tier: None,
        sections_done: true,
        counts_done: true,
        retry_cd: 0,
    }];
    state.sections = pinned
        .iter()
        .enumerate()
        .map(|(index, &on)| BrowseSection {
            src: 0,
            key: index as i64 + 1,
            title: format!("Library {index}"),
            kind: SecKind::Movie,
            count: -1,
            pinned: on,
        })
        .collect();
    state.states.resize_with(state.sections.len(), SecState::default);
    state.bump_sections_gen();
}

#[cfg(test)]
pub(crate) fn set_pinned_for_owner_test(state: &mut BrowseState, index: usize, on: bool) {
    crate::testlock::assert_held("an owned browse section table (set_pinned_for_test)");
    if let Some(section) = state.sections.get_mut(index) {
        section.pinned = on;
    }
}

#[cfg(test)]
pub(crate) fn land_pin_for_owner_test(state: &mut BrowseState, pinned: bool) {
    crate::testlock::assert_held("an owned browse section table (land_pin_for_test)");
    let key = state.sections.len() as i64 + 1;
    state.sections.push(BrowseSection {
        src: 0,
        key,
        title: format!("Library {}", state.sections.len()),
        kind: SecKind::Movie,
        count: -1,
        pinned,
    });
    state.states.push(SecState::default());
    state.bump_sections_gen();
}

#[cfg(test)]
pub(crate) fn seed_two_source_table_for_owner_test(state: &mut BrowseState) {
    crate::testlock::assert_held("an owned browse section table (seed_two_source_table_for_test)");
    crate::plex::session::forget_pins_for_test(&crate::plex::session::current_profile_key());
    state.reset_with(|| {});
    state.sources = vec![
        tests::a_source("mac-mini", "", true),
        tests::a_source("nas-home", "friend", true),
    ];
    state.append_sections_with(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
        None,
    );
    state.append_sections_with(
        1,
        vec![
            (1, "Film Club".into(), SecKind::Movie),
            (2, "Film Club".into(), SecKind::Show),
        ],
        None,
    );
}

#[cfg(test)]
pub(crate) fn seed_registered_table_for_owner_test(
    state: &mut BrowseState,
    sids: [ServerId; 2],
) {
    crate::testlock::assert_held("an owned browse section table (seed_registered_table_for_test)");
    seed_two_source_table_for_owner_test(state);
    for (index, sid) in sids.into_iter().enumerate() {
        let client = crate::plex::client_for(sid).expect("registered fixture source");
        let source = state.source_mut(index).unwrap();
        source.sid = sid;
        source.client_addr = client as *const _ as usize;
        source.token_gen = client.token_gen();
        crate::plex::describe_server(sid, &source.name, &source.handle, source.owned);
    }
    for section in &mut state.states {
        section.letters_done = true;
    }
}

#[cfg(test)]
pub(crate) fn append_section_for_owner_test(
    state: &mut BrowseState,
    source: usize,
    key: i64,
    title: &str,
    kind: SecKind,
) {
    crate::testlock::assert_held("an owned browse section table (append_section_for_test)");
    state.append_sections_with(source, vec![(key, title.into(), kind)], None);
}

#[cfg(test)]
pub(crate) fn seed_letter_counts_for_owner_test(
    state: &mut BrowseState,
    letters: &[(&str, i64)],
) {
    crate::testlock::assert_held("an owned browse section table (seed_letter_counts_for_test)");
    let current = state.cur();
    if let Some(section) = state.state_mut(current) {
        section.letters = Arc::new(
            letters
                .iter()
                .map(|(label, count)| ((*label).into(), *count))
                .collect(),
        );
        section.letters_done = true;
    }
}

#[cfg(test)]
pub(crate) fn seed_query_choices_for_owner_test(
    state: &mut BrowseState,
    sorts: Vec<SortEntry>,
    genres: Vec<GenreEntry>,
) {
    crate::testlock::assert_held("an owned browse section table (seed_query_choices_for_test)");
    let current = state.cur();
    if let Some(section) = state.state_mut(current) {
        section.sorts = Arc::new(sorts);
        section.genres = Arc::new(genres);
        section.genres_done = true;
    }
}

#[cfg(test)]
pub(crate) fn prepare_page_for_owner_test(state: &mut BrowseState, sid: ServerId) {
    for source in &mut state.sources {
        source.sections_done = true;
        source.counts_done = true;
    }
    state.cur = state.sections.iter().position(|section| {
        state.sources.get(section.src).map(|source| source.sid) == Some(sid)
    }).expect("a section for the requested server");
    state.want = (0, 1);
    let current = state.cur();
    let section = state.state_mut(current).unwrap();
    section.fetch = SecFetch::Loading;
    section.total = -1;
    section.items.clear();
}

#[cfg(test)]
pub(crate) fn queue_genre_for_owner_test(
    state: &mut BrowseState,
    adapter: &BrowseAdapter,
    client: &'static crate::plex::Client,
) {
    prepare_page_for_owner_test(state, client.id());
    let sec = state.cur();
    state.state_mut(sec).unwrap().total = 0;
    adapter.genre_fetching.store(true, Ordering::SeqCst);
    *adapter.genre_result.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(DirectoryResult {
            epoch: state.table_epoch(), sec, client, token_gen: client.token_gen(),
            list: vec![GenreEntry { id: "new".into(), title: "New Genre".into() }],
        });
}

#[cfg(test)]
pub(crate) fn adapter_has_page_for_test(adapter: &BrowseAdapter) -> bool {
    adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

#[cfg(test)]
pub(crate) fn adapter_fetching_for_test(adapter: &BrowseAdapter) -> bool {
    adapter.fetching.load(Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn set_adapter_src_fetching_for_test(adapter: &BrowseAdapter, fetching: bool) {
    adapter.src_fetching.store(fetching, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn adapter_src_fetching_for_test(adapter: &BrowseAdapter) -> bool {
    adapter.src_fetching.load(Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn queue_page_failure_for_owner_test(
    state: &mut BrowseState,
    adapter: &BrowseAdapter,
    client: &'static crate::plex::Client,
) {
    prepare_page_for_owner_test(state, client.id());
    let sec = state.cur();
    adapter.fetching.store(true, Ordering::SeqCst);
    *adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(PageResult {
        client, token_gen: client.token_gen(), gen: state.query_gen(), sec, start: 0,
        items: Vec::new(), total: -1, sorts: None,
    });
}

#[cfg(test)]
pub(crate) fn set_adapter_fetching_for_test(adapter: &BrowseAdapter, fetching: bool) {
    adapter.fetching.store(fetching, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn spawn_owned_page_for_test(
    state: &BrowseState,
    adapter: &Arc<BrowseAdapter>,
    client: &'static crate::plex::Client,
    title: &str,
) -> (std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>) {
    let sec = state.cur();
    let sid = state.section_sid(sec).expect("an active test section");
    assert_eq!(sid, client.id());
    let gen = state.query_gen();
    let token_gen = client.token_gen();
    let title = title.to_string();
    let worker_adapter = Arc::clone(adapter);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(0);
    adapter.fetching.store(true, Ordering::SeqCst);
    assert!(crate::task::spawn_small("browse-owner-test", move || {
        release_rx.recv().expect("test releases worker");
        *worker_adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(PageResult {
                client, token_gen, gen, sec, start: 0,
                items: vec![PmsMovie { sid, title, ..Default::default() }],
                total: 1, sorts: None,
            });
        done_tx.send(()).expect("test receives worker completion");
    }));
    (release_tx, done_rx)
}

#[cfg(test)]
mod tests {
    #[test]
    fn addressed_instance_commit_validates_identity_and_selects_before_query() {
        use crate::stores::browse::{LibraryWork, QueryEdit, SectionAddress};

        let sid = super::ServerId::UNSET;
        let mut state = super::BrowseState::default();
        state.sources.push(a_source("machine-a", "", true));
        state.sections = vec![
            super::BrowseSection {
                src: 0,
                key: 7,
                title: "Films A".into(),
                kind: super::SecKind::Movie,
                count: 0,
                pinned: true,
            },
            super::BrowseSection {
                src: 0,
                key: 8,
                title: "Films B".into(),
                kind: super::SecKind::Movie,
                count: 0,
                pinned: true,
            },
        ];
        state.states = vec![super::SecState::default(), super::SecState::default()];
        let target = SectionAddress {
            epoch: state.table_epoch(),
            sid,
            section: 8,
        };
        let adapter = Arc::new(BrowseAdapter::default());

        assert!(!state.addressed_with_adapter(
            &adapter,
            SectionAddress {
                epoch: target.epoch.wrapping_add(1),
                ..target
            },
            LibraryWork::Commit {
                select: true,
                choice: false,
                query: Some(QueryEdit::Unwatched(true)),
            },
        ));
        assert_eq!(state.cur(), 0);
        assert_eq!(state.query_gen(), 0);
        assert!(state.addressed_with_adapter(
            &adapter,
            target,
            LibraryWork::Commit {
                select: true,
                choice: false,
                query: Some(QueryEdit::Unwatched(true)),
            },
        ));
        assert_eq!(state.cur(), 1);
        assert!(!state.states[0].unwatched);
        assert!(state.states[1].unwatched);
        assert_eq!(
            state.query_gen(),
            2,
            "selection and requery each supersede a landing"
        );
    }

    #[test]
    fn retained_pages_copy_only_changed_page_and_keep_sparse_holes() {
        let mut items = super::SecItems::default();
        items.resize(20_000);
        let sid = super::ServerId::UNSET;
        let other = super::ServerId::from_raw(1);
        items.set(
            0,
            super::PmsMovie {
                sid,
                rk: "7".into(),
                ..Default::default()
            },
        );
        items.set(
            super::PAGE,
            super::PmsMovie {
                sid: other,
                rk: "7".into(),
                ..Default::default()
            },
        );
        let old = items.clone();
        assert!(super::Arc::ptr_eq(&old.pages, &items.pages));
        assert_eq!(items.pages.iter().flatten().count(), 2);
        assert!(!items.set_watched(sid, "missing", false));
        assert!(super::Arc::ptr_eq(&old.pages, &items.pages));
        assert!(items.set_watched(sid, "7", false));
        assert!(!super::Arc::ptr_eq(&old.pages, &items.pages));
        assert!(!super::Arc::ptr_eq(
            old.pages[0].as_ref().unwrap(),
            items.pages[0].as_ref().unwrap()
        ));
        assert!(super::Arc::ptr_eq(
            old.pages[1].as_ref().unwrap(),
            items.pages[1].as_ref().unwrap()
        ));
        assert!(!old.get(0).unwrap().unwatched);
        assert!(items.get(0).unwrap().unwatched);
        assert!(!items.get(super::PAGE).unwrap().unwatched);
        items.set(super::PAGE * 2, super::PmsMovie::default());
        assert!(old.get(super::PAGE * 2).is_none());
        assert!(old.page_missing(2));
        items.clear();
        assert_eq!(old.len(), 20_000);
        assert!(old.get(0).is_some());
    }

    #[test]
    fn sparse_page_resize_matches_flat_storage_across_boundaries() {
        let mut items = super::SecItems::default();
        let mut flat: Vec<Option<String>> = Vec::new();
        for size in [1, 2, 59, 60, 61, 122, 61, 60, 59, 60, 61, 0, 1, 121] {
            let old = items.clone();
            let old_flat = flat.clone();
            items.resize(size);
            flat.resize(size, None);
            for i in 0..=size {
                assert_eq!(
                    items.get(i).map(|m| &m.rk),
                    flat.get(i).and_then(Option::as_ref)
                );
            }
            for i in 0..size {
                if i % 3 == 0 {
                    let rk = format!("{size}-{i}");
                    items.set(
                        i,
                        super::PmsMovie {
                            rk: rk.clone(),
                            ..Default::default()
                        },
                    );
                    flat[i] = Some(rk);
                }
            }
            for (p, chunk) in flat.chunks(super::PAGE).enumerate() {
                assert_eq!(items.page_missing(p), chunk.iter().any(Option::is_none));
            }
            assert!(!items.page_missing(size.div_ceil(super::PAGE)));
            for (i, expected) in old_flat.iter().enumerate() {
                assert_eq!(old.get(i).map(|m| &m.rk), expected.as_ref());
            }
        }
    }

    #[test]
    fn sparse_page_growth_accepts_new_items() {
        let mut items = super::SecItems::default();
        items.resize(1);
        items.set(0, super::PmsMovie::default());
        items.resize(2);
        assert!(items.page_missing(0));
        items.set(
            1,
            super::PmsMovie {
                rk: "2".into(),
                ..Default::default()
            },
        );
        assert_eq!(items.get(1).map(|m| m.rk.as_str()), Some("2"));
        assert!(!items.page_missing(0));
    }

    #[test]
    fn sparse_page_shrink_does_not_resurrect_removed_items() {
        let mut items = super::SecItems::from_vec(vec![
            Some(super::PmsMovie::default()),
            Some(super::PmsMovie::default()),
        ]);
        items.resize(1);
        items.resize(2);
        assert!(items.get(1).is_none());
        assert!(items.page_missing(0));
    }

    use super::*;

    struct TestBrowse {
        state: BrowseState,
        adapter: Arc<BrowseAdapter>,
    }

    impl Default for TestBrowse {
        fn default() -> Self {
            Self {
                state: BrowseState::default(),
                adapter: Arc::new(BrowseAdapter::default()),
            }
        }
    }

    impl TestBrowse {
        fn reset(&mut self) {
            self.state.reset_owned(&self.adapter);
        }

        fn pump(&mut self) -> crate::stores::StoreOutcome {
            let roster = self.state.sync_roster_owned();
            if roster.retire_adapter {
                self.adapter = Arc::new(BrowseAdapter::default());
            }
            let mut outcome = self.state.pump_owned(&self.adapter);
            outcome.changed |= roster.changed;
            outcome
        }

        fn discover_pump(&mut self) -> crate::stores::StoreOutcome {
            self.state.discover_pump_owned(&self.adapter)
        }

        fn sync_roster(&mut self) {
            if self.state.sync_roster_owned().retire_adapter {
                self.adapter = Arc::new(BrowseAdapter::default());
            }
        }

        fn seed_sources(&mut self, sources: Vec<BrowseSource>) {
            self.reset();
            self.state.sources = sources;
        }

        fn append_sections(&mut self, source: usize, sections: Vec<(i64, String, SecKind)>) {
            self.state.append_sections_with(source, sections, None);
        }

        fn section_title(&self, index: usize) -> &str {
            self.state.sections.get(index).map_or("", |section| section.title.as_str())
        }

        fn section_count(&self) -> usize {
            self.state.sections.len()
        }

        fn pinned(&self, index: usize) -> bool {
            self.state.pinned_for_test(index)
        }

        fn source_rows(&self) -> Vec<SrcRow> {
            let Some(kind) = self.state.section_kind(self.state.cur()) else {
                return Vec::new();
            };
            self.state.rows_where(|section| section.kind == kind && section.pinned)
        }

        fn kind_position(&self, index: usize) -> Option<(usize, usize)> {
            let kind = self.state.section_kind(index)?;
            if !self.pinned(index) {
                return None;
            }
            let (mut position, mut count) = (0, 0);
            for (candidate, section) in self.state.sections.iter().enumerate() {
                if section.kind != kind || !section.pinned {
                    continue;
                }
                if candidate == index {
                    position = count;
                }
                count += 1;
            }
            (count > 1).then_some((position + 1, count))
        }

        fn tab_count(&self) -> usize {
            self.state.tab_kinds().count()
        }

        fn tab_title(&self, tab: usize) -> &str {
            match self.state.tab_kinds().nth(tab) {
                Some(SecKind::Movie) => "Movies",
                Some(SecKind::Show) => "TV Shows",
                None => "",
            }
        }

        fn tab_of_section(&self, section: usize) -> Option<usize> {
            self.state.section_kind(section).and_then(|kind| self.state.tab_of_kind(kind))
        }

        fn loading_initial(&self) -> bool {
            self.state.cur_state().is_some_and(|state| {
                state.total < 0 && matches!(state.fetch, SecFetch::Loading)
            })
        }

        fn fetch_state(&self) -> SecFetch {
            self.state.cur_state().map_or(SecFetch::Loading, |state| state.fetch)
        }

        fn first_run_asks(&self) -> bool {
            let session = crate::plex::session::peek();
            crate::plex::pins::asks(
                self.state.sources.len(),
                session.pins_for(&crate::plex::session::current_profile_key()),
            )
        }
    }

    struct RegisteredCleanup;

    #[test]
    fn addressed_discovery_retry_rejects_retired_tables_and_other_sources() {
        let _guard = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        seed_sources_for_owner_test(&mut browse.state, 2, false);
        browse.state.source_mut(0).unwrap().retry_cd = 9;
        browse.state.source_mut(1).unwrap().retry_cd = 13;
        let sid = browse.state.sources()[1].sid;
        let epoch = browse.state.table_epoch();
        assert!(browse.state.retry_source(epoch, sid));
        assert_eq!(browse.state.sources()[0].retry_cd, 9);
        assert_eq!(browse.state.sources()[1].retry_cd, 0);
        browse.state.source_mut(1).unwrap().retry_cd = 17;
        assert!(!browse.state.retry_source(epoch.wrapping_add(1), sid));
        assert!(!browse.state.retry_source(epoch, ServerId::from_raw(99)));
        assert_eq!(browse.state.sources()[1].retry_cd, 17);
    }

    impl Drop for RegisteredCleanup {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }

    fn registered_source(
    ) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "old", "cid");
        assert!(crate::plex::set_current(sid));
        let mut source = a_source("original", "", true);
        source.sid = sid;
        source.sections_done = false;
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![source]);
        (
            RegisteredCleanup,
            browse,
            sid,
            crate::plex::client_for(sid).unwrap(),
        )
    }

    fn registered_page_source(
    ) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
        let (cleanup, mut browse, sid, client) = registered_source();
        if let Some(source) = browse.state.source_mut(0) {
            source.sections_done = true;
            source.counts_done = true;
        }
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        (cleanup, browse, sid, client)
    }

    fn registered_resident_page_source(
    ) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client) {
        let (cleanup, mut browse, sid, client) = registered_page_source();
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 1;
        state.items = SecItems::from_vec(vec![Some(PmsMovie::default())]);
        (cleanup, browse, sid, client)
    }

    #[test]
    fn a_settled_query_change_with_unknown_total_is_still_page_work() {
        let _g = crate::testlock::serial();
        let (_cleanup, browse, _, _) = registered_resident_page_source();
        let mut state = browse.state;
        let adapter = BrowseAdapter::default();
        let _ = state.sync_roster_owned();
        for source in &mut state.sources {
            source.sections_done = true;
            source.counts_done = true;
        }
        adapter.src_fetching.store(true, Ordering::SeqCst);
        assert!(!state.pump_needs_work(&adapter), "the resident query starts settled");

        state.set_unwatched(true);

        assert_eq!(state.states[state.cur()].total, -1);
        assert!(state.pump_needs_work(&adapter),
            "unknown total means page zero is owed even before a wanted window is published");
    }

    fn registered_directory_source(
    ) -> (RegisteredCleanup, TestBrowse, ServerId, &'static crate::plex::Client)
    {
        let (cleanup, mut browse, sid, client) = registered_page_source();
        let state = browse.state.state_mut(0).unwrap();
        state.genres = Arc::new(vec![GenreEntry {
            id: "new".into(),
            title: "New Genre".into(),
        }]);
        state.letters = Arc::new(vec![("N".into(), 7)]);
        state.genres_done = false;
        state.letters_done = false;
        (cleanup, browse, sid, client)
    }

    fn queue_success_from(
        browse: &mut TestBrowse,
        client: &'static crate::plex::Client,
        token_gen: u32,
    ) {
        let landing = SrcLanding {
            client,
            token_gen,
            name: "stale-name".into(),
            what: SrcWhat::Sections(Some(vec![(99, "Stale Library".into(), SecKind::Movie)])),
        };
        *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((browse.state.table_epoch(), 0, landing));
        let _ = browse.state.land_discovery_owned(&browse.adapter);
    }

    fn queue_page_from(
        browse: &mut TestBrowse,
        client: &'static crate::plex::Client,
        token_gen: u32,
    ) {
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(PageResult {
            client,
            token_gen,
            gen: browse.state.query_gen(),
            sec: 0,
            start: 0,
            items: Vec::new(),
            total: 0,
            sorts: None,
        });
        browse.adapter.fetching.store(true, Ordering::SeqCst);
        let _outcome = browse.pump();
    }

    fn queue_directories_from(
        browse: &mut TestBrowse,
        client: &'static crate::plex::Client,
        token_gen: u32,
    ) {
        let epoch = browse.state.table_epoch();
        *browse.adapter.genre_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(DirectoryResult {
            epoch,
            sec: 0,
            client,
            token_gen,
            list: vec![GenreEntry {
                id: "stale".into(),
                title: "Stale Genre".into(),
            }],
        });
        *browse.adapter.letter_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(DirectoryResult {
            epoch,
            sec: 0,
            client,
            token_gen,
            list: vec![("S".into(), 99)],
        });
        browse.adapter.genre_fetching.store(true, Ordering::SeqCst);
        browse.adapter.letters_fetching.store(true, Ordering::SeqCst);
        browse.state.land_directory_owned(
            &browse.adapter.genre_fetching, &browse.adapter.genre_result, |st, list| {
            st.genres_done = true;
            st.genres = Arc::new(list);
        });
        browse.state.land_directory_owned(
            &browse.adapter.letters_fetching, &browse.adapter.letter_result, |st, list| {
            st.letters_done = true;
            st.letters = Arc::new(list);
        });
    }

    fn assert_new_directories_survive(browse: &TestBrowse) {
        let state = browse.state.states().first().unwrap();
        assert_eq!(state.genres.len(), 1);
        assert_eq!(state.genres[0].id, "new");
        assert_eq!(state.genres[0].title, "New Genre");
        assert_eq!(*state.letters, vec![("N".into(), 7)]);
        assert!(!state.genres_done);
        assert!(!state.letters_done);
    }

    #[test]
    fn an_equal_size_profile_roster_replaces_the_inactive_source_instead_of_appending() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let mut browse = TestBrowse::default();
        let a = crate::plex::register_for_test("browse-a", "127.0.0.1", 1, "a", "cid");
        let b = crate::plex::register_for_test("browse-b", "127.0.0.1", 2, "b", "cid");
        browse.sync_roster();
        assert_eq!(browse.state.sources().iter().map(|s| s.sid).collect::<Vec<_>>(), [a, b]);

        crate::plex::revoke_for_profile_switch();
        let c = crate::plex::register_for_test("browse-c", "127.0.0.1", 3, "c", "cid");
        assert_eq!(
            crate::plex::server_count(),
            2,
            "the replacement deliberately preserves count"
        );
        browse.sync_roster();
        assert_eq!(browse.state.sources().iter().map(|s| s.sid).collect::<Vec<_>>(), [a, c]);

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn filling_a_source_name_refreshes_the_retained_directory() {
        let _g = crate::testlock::serial();
        let _cleanup = RegisteredCleanup;
        crate::plex::reset_servers_for_test();
        let mut browse = TestBrowse::default();
        let sid = crate::plex::register_for_test("browse-name-fill", "127.0.0.1", 1, "t", "cid");
        crate::plex::describe_server(sid, "", "", true);
        browse.sync_roster();
        let mut directory = view::DirectorySnapshot::default();
        directory.capture_from(&mut browse.state);
        let old = directory.clone();
        let generation = browse.state.source_list_gen();
        crate::plex::describe_server_name(sid, "Learned");
        browse.sync_roster();
        assert_ne!(browse.state.source_list_gen(), generation);
        directory.capture_from(&mut browse.state);
        assert_eq!(directory.view().sources()[0].1.name, "Learned");
        assert_eq!(old.view().sources()[0].1.name, "");
        let generation = browse.state.source_list_gen();
        browse.sync_roster();
        assert_eq!(
            browse.state.source_list_gen(),
            generation,
            "a steady name does not republish"
        );
    }

    /// **A corrected credit reaches the Library panel.** `plex::servers::owner_credit` is the one
    /// rule, but this table is a per-source CACHE in front of it, and it used to fill the handle
    /// only while its own copy was empty — so a source that had once been captioned could never
    /// lose or change that caption, whatever the registry later learned.
    ///
    /// That is the reported bug's last hop. A session written before the rule existed publishes the
    /// account holder's own handle against the household's server; the roster refresh re-grades it
    /// and `describe` takes it off the registry — and the Sources panel and the Library read-out
    /// went on saying "Shared by …" regardless.
    ///
    /// The machine NAME keeps its fill-only behaviour in the same block, deliberately: it is the
    /// one field here that a rename would churn under an open panel.
    #[test]
    fn a_source_follows_a_corrected_credit_but_not_a_renamed_machine() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let mut browse = TestBrowse::default();
        let sid = crate::plex::register_for_test("browse-credit", "127.0.0.1", 1, "t", "cid");

        // what a build without the rule left in the registry at boot
        crate::plex::describe_server(sid, "Mac mini", "admin", false);
        browse.sync_roster();
        assert_eq!(
            browse.state.sources().first().map(|s| (s.handle.as_str(), s.owned)),
            Some(("admin", false))
        );

        // the roster refresh lands, re-graded: nobody is credited for the household's own server
        crate::plex::describe_server(sid, "Mac mini", "", false);
        browse.sync_roster();
        assert_eq!(
            browse.state.sources().first().map(|s| s.handle.as_str()),
            Some(""),
            "the panel follows the registry off a credit, not only onto one"
        );

        // …and a rename still does not travel
        crate::plex::describe_server(sid, "nas-loft", "", false);
        browse.sync_roster();
        assert_eq!(browse.state.sources().first().map(|s| s.name.as_str()), Some("Mac mini"));

        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn a_discovery_landing_from_before_a_same_slot_repoint_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

        queue_success_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
        assert_eq!(browse.state.sources()[0].name, "original");
        assert!(
            browse.state.sections().is_empty(),
            "old-origin rows must not enter the new lifecycle"
        );
    }

    #[test]
    fn endpoint_outcomes_follow_only_current_failed_discovery_through_both_pumps() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, client) = registered_source();
        with_refused_discovery_for_test(|| {
            queue_discovery_for_owner_test(
                &mut browse.state, &browse.adapter, client, client.token_gen(), false);
            let requests = browse.discover_pump();
            assert_eq!(requests.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), [sid]);
            assert!(
                !crate::plex::write_held_for_test(),
                "intent returned after lifecycle lock release"
            );
            queue_discovery_for_owner_test(
                &mut browse.state, &browse.adapter, client, client.token_gen(), false);
            assert_eq!(browse.pump().endpoints.iter().count(), 1);
            queue_discovery_for_owner_test(
                &mut browse.state, &browse.adapter, client, client.token_gen(), true);
            assert_eq!(browse.discover_pump().endpoints.iter().count(), 0);
            let old_gen = client.token_gen();
            client.set_token("new-synthetic");
            queue_discovery_for_owner_test(
                &mut browse.state, &browse.adapter, client, old_gen, false);
            assert_eq!(browse.pump().endpoints.iter().count(), 0);
        });
    }

    #[test]
    fn controlled_discovery_replays_refusal_retry_and_success_with_exact_identity() {
        let _serial = crate::testlock::serial();
        let (_cleanup, browse, sid, client) = registered_source();
        // Two independent executions start from this pre-request store value. No worker is
        // running: only the OS executor is substituted, below the real discovery policy.
        let initial_epoch = browse.state.table_epoch();
        let stores = crate::stores::Stores::default();
        let mt = unsafe { crate::task::MainThread::assume() };
        let mut transcript: Vec<std::collections::VecDeque<serde_json::Value>> = Vec::new();
        let mut states = Vec::new();
        let mut successful_result = None;
        let mut parsed: Option<crate::ui::rec::Recording> = None;
        for replay in [false, true] {
            // seed_sources is a reset fixture; restore the same pre-execution epoch on each
            // independent run, never to cancel or bypass a guard during either history.
            stores.browse.borrow_mut().prepare_discovery_replay_for_test(sid, initial_epoch);
            let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
            let mut io = crate::app::HomeIo {
                replay,
                preferences: Default::default(),
                requests: Vec::new(),
                admissions: Default::default(),
                failure: None,
                profile: publisher.snapshot(),
            };
            let mut attempts = 0;
            let sink = crate::ui::rec::MemSink::default();
            let bytes = sink.segments.clone();
            let header = crate::ui::rec::Header::new(17, &crate::ui::press::Press::new());
            let mut writer = crate::ui::rec::Writer::open(Box::new(sink), &header, 0).unwrap();
            for frame in 0..=602 {
                if replay {
                    io.admissions = parsed.as_ref().unwrap().frames[frame]
                        .effects
                        .iter()
                        .map(|effect| effect["payload"].clone())
                        .collect();
                }
                io.discovery_owned_with(&stores, &mut |request| {
                    assert!(!replay, "replay must never execute live admission");
                    attempts += 1;
                    if attempts == 1 { return false; }
                    let descriptor = request.descriptor();
                    successful_result = Some(serde_json::json!({"kind":"discovery","version":1,
                        "epoch":descriptor["epoch"],"source":descriptor["source"],"sid":descriptor["sid"],
                        "client":descriptor["client"],"token_gen":descriptor["token_gen"],
                        "name":"s00000001","what":{"Sections":[]}}));
                    true
                });
                if frame == 600 {
                    let result =
                        record::decode(successful_result.clone().expect("retry admitted"), |id| {
                            (id == client.instance_gen()).then_some(client)
                        })
                        .unwrap();
                    assert!(stores.browse.borrow_mut()
                        .apply_discovery(&result, &io.preferences)
                        .endpoints
                        .iter()
                        .next()
                        .is_none());
                }
                let requests: std::collections::VecDeque<_> =
                    std::mem::take(&mut io.requests).into();
                let state = stores.browse.borrow().discovery_policy_for_test();
                assert!(
                    io.failure.is_none() && io.admissions.is_empty(),
                    "frame={frame} replay={replay} failure={:?} pending={}",
                    io.failure,
                    io.admissions.len()
                );
                if replay {
                    assert_eq!(
                        requests, transcript[frame],
                        "request/answer identity and frame must match"
                    );
                    assert_eq!(
                        state, states[frame],
                        "refusal/retry policy must match normal execution"
                    );
                } else {
                    writer.tick(
                        frame as u64,
                        crate::ui::machine::Tick {
                            ms: frame as u32 * 16,
                            dt_us: 16_000,
                        },
                    );
                    for request in &requests {
                        writer.effect_payload(frame as u64, "Cache", "Request", request.clone());
                    }
                    writer.flush_frame().unwrap();
                    transcript.push(requests);
                    states.push(state);
                }
            }
            assert!(
                stores.browse.borrow().discovery_policy_for_test().2,
                "admitted retry delivers its real result"
            );
            assert_eq!(attempts, if replay { 0 } else { 2 });
            writer.finish().unwrap();
            if !replay {
                let bytes = bytes.borrow();
                parsed = Some(
                    crate::ui::rec::Recording::parse(
                        &header.to_json().to_string(),
                        &bytes.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                        17,
                    )
                    .unwrap(),
                );
            }
        }
        assert_eq!(transcript[0][0]["admitted"], serde_json::json!(false));
        assert!(transcript[1..600]
            .iter()
            .all(std::collections::VecDeque::is_empty));
        assert_eq!(transcript[600][0]["admitted"], serde_json::json!(true));
    }

    #[test]
    fn a_same_slot_repoint_rearms_section_discovery_without_erasing_known_rows() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, _) = registered_page_source();
        assert!(browse.state.sources()[0].sections_done);
        assert_eq!(browse.section_count(), 1);
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );

        browse.sync_roster();

        assert!(
            !browse.state.sources()[0].sections_done,
            "the new lifecycle must enumerate again"
        );
        assert_eq!(
            browse.section_count(),
            1,
            "known rows stay visible until a fresh answer lands"
        );
    }

    #[test]
    fn a_discovery_landing_from_before_an_in_place_retoken_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
            sid
        );
        assert!(
            std::ptr::eq(old, crate::plex::client_for(sid).unwrap()),
            "retoken stays in place"
        );
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

        queue_success_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
        assert_eq!(browse.state.sources()[0].name, "original");
        assert!(browse.state.sections().is_empty());
    }

    #[test]
    fn a_discovery_landing_from_before_a_profile_reset_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_source();
        let old_gen = old.token_gen();
        crate::plex::revoke_for_profile_switch();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
            sid
        );
        crate::plex::finish_profile_switch(&[sid]);
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);

        queue_success_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unreachable)
        );
        assert_eq!(browse.state.sources()[0].name, "original");
        assert!(browse.state.sections().is_empty());
    }

    #[test]
    fn page_failure_and_recovery_republish_directory_reachability() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, client) = registered_resident_page_source();
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Reachable);
        // A fully discovered, current lifecycle: neither directory discovery nor the next-page
        // scheduler has work. Only the injected page results below can change the source.
        browse.sync_roster();
        let source = browse.state.source_mut(0).unwrap();
        source.client_addr = client as *const _ as usize;
        source.token_gen = client.token_gen();
        source.sections_done = true;
        source.counts_done = true;
        let mut directory = view::DirectorySnapshot::default();
        directory.capture_from(&mut browse.state);
        let original = directory.clone();
        for (total, expected, fetch) in [
            (-1, SourceState::Unreachable, SecFetch::Failed),
            (-1, SourceState::Unreachable, SecFetch::Failed),
            (1, SourceState::Reachable, SecFetch::Ready),
        ] {
            let changed = browse.state.sources()[0].state != expected;
            let generation = browse.state.source_list_gen();
            *browse.adapter.page_result.lock().unwrap() = Some(PageResult {
                client,
                token_gen: client.token_gen(),
                gen: browse.state.query_gen(),
                sec: 0,
                start: 0,
                items: if total < 0 {
                    vec![]
                } else {
                    vec![PmsMovie {
                        sid,
                        ..Default::default()
                    }]
                },
                total,
                sorts: None,
            });
            browse.adapter.fetching.store(true, Ordering::SeqCst);
            let _outcome = browse.pump();
            assert_eq!(browse.state.sources()[0].state, expected);
            assert_eq!(
                browse.state.source_list_gen(),
                generation.wrapping_add(u32::from(changed))
            );
            // A later roster sync cannot be relied on to supply a missing notification: its
            // registry and local states already match after the page's atomic commit.
            browse.sync_roster();
            directory.capture_from(&mut browse.state);
            assert_eq!(directory.view().sources()[0].1.state, expected);
            assert_eq!(directory.view().source_fetch(), fetch);
            assert_eq!(original.view().sources()[0].1.state, SourceState::Reachable);
        }
    }

    #[test]
    fn a_page_landing_from_before_a_same_slot_repoint_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

        queue_page_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
        assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
        assert_eq!(
            browse.state.states()[0].total,
            1,
            "old-origin page must not replace the new lifecycle's rows"
        );
        assert_eq!(browse.state.states()[0].items.len(), 1);
    }

    #[test]
    fn a_page_landing_from_before_an_in_place_retoken_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
            sid
        );
        assert!(std::ptr::eq(old, crate::plex::client_for(sid).unwrap()));
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

        queue_page_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
        assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
        assert_eq!(browse.state.states()[0].total, 1);
        assert_eq!(browse.state.states()[0].items.len(), 1);
    }

    #[test]
    fn a_page_landing_from_before_a_profile_reset_is_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
        let old_gen = old.token_gen();
        crate::plex::revoke_for_profile_switch();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
            sid
        );
        crate::plex::finish_profile_switch(&[sid]);
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);

        queue_page_from(&mut browse, old, old_gen);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unreachable)
        );
        assert_eq!(browse.state.sources()[0].state, SourceState::Unreachable);
        assert_eq!(browse.state.states()[0].total, 1);
        assert_eq!(browse.state.states()[0].items.len(), 1);
    }

    #[test]
    fn a_repoint_requested_after_validation_waits_for_the_local_page_commit() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, client) = registered_resident_page_source();
        let token_gen = client.token_gen();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let committed = std::sync::Arc::new(AtomicBool::new(false));
        let seen = committed.clone();
        let repoint = std::thread::spawn(move || {
            // This worker races the registry's own `WRITE` mutex against the main thread's
            // `commit_reachability_if_current` closure below — that IS the property under test —
            // so it is not a bystander of some other module's test; it still writes the same
            // crate-global registry `crate::testlock::serial()` protects, and it joins back into
            // the outer test (below) strictly before that guard drops. See
            // `crate::testlock::adopt_current_thread`'s doc for the exact contract.
            crate::testlock::adopt_current_thread();
            start_rx.recv().unwrap();
            attempt_tx.send(()).unwrap();
            assert_eq!(
                crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
                sid
            );
            assert!(
                seen.load(Ordering::SeqCst),
                "repoint returned before the browse commit released WRITE"
            );
        });

        let applied = crate::plex::commit_reachability_if_current(
            sid,
            client,
            token_gen,
            true,
            None,
            |outcome| {
                assert!(
                    crate::plex::write_held_for_test(),
                    "local page mutation must execute under WRITE"
                );
                start_tx.send(()).unwrap();
                attempt_rx.recv().unwrap(); // the other thread is now about to take WRITE
                assert!(browse.state.apply_source_outcome(0, client, outcome));
                browse.state.state_mut(0).unwrap().total = 2;
                committed.store(true, Ordering::SeqCst);
                true
            },
        );
        assert_eq!(applied, Some(true));
        repoint.join().unwrap();
        assert_eq!(browse.state.states()[0].total, 2);
        assert!(!std::ptr::eq(client, crate::plex::client_for(sid).unwrap()));
    }

    #[test]
    fn directory_landings_from_before_a_same_slot_repoint_are_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_directory_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );
        queue_directories_from(&mut browse, old, old_gen);
        assert_new_directories_survive(&browse);
    }

    #[test]
    fn directory_landings_from_before_an_in_place_retoken_are_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_directory_source();
        let old_gen = old.token_gen();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
            sid
        );
        queue_directories_from(&mut browse, old, old_gen);
        assert_new_directories_survive(&browse);
    }

    #[test]
    fn directory_landings_from_before_a_profile_reset_are_inert() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_directory_source();
        let old_gen = old.token_gen();
        crate::plex::revoke_for_profile_switch();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
            sid
        );
        crate::plex::finish_profile_switch(&[sid]);
        queue_directories_from(&mut browse, old, old_gen);
        assert_new_directories_survive(&browse);
    }

    #[test]
    fn directory_landings_for_the_current_lifecycle_commit_both_menus() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, client) = registered_directory_source();
        queue_directories_from(&mut browse, client, client.token_gen());
        let state = browse.state.states().first().unwrap();
        assert_eq!(state.genres[0].id, "stale");
        assert_eq!(*state.letters, vec![("S".into(), 99)]);
        assert!(state.genres_done);
        assert!(state.letters_done);
    }

    #[test]
    fn blocking_section_discovery_discards_a_same_slot_repoint_during_the_request() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_source();
        let count = ensure_sections_with(&mut browse.state, |client| {
            assert!(std::ptr::eq(client, old));
            assert_eq!(
                crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
                sid
            );
            Some(vec![(1, "Stale Movies".into(), SecKind::Movie)])
        });

        assert_eq!(count, 0);
        assert!(
            browse.state.sections().is_empty(),
            "the old origin's section table must not land"
        );
        assert_eq!(
            crate::plex::server_probe_result(sid),
            None,
            "the replacement lifecycle stays unprobed"
        );
    }

    #[test]
    fn blocking_section_discovery_discards_an_in_place_retoken_during_the_request() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, old) = registered_source();
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
        let old_gen = old.token_gen();
        let count = ensure_sections_with(&mut browse.state, |client| {
            assert!(std::ptr::eq(client, old));
            assert_eq!(
                crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
                sid
            );
            Some(vec![(1, "Stale Movies".into(), SecKind::Movie)])
        });

        assert_ne!(old.token_gen(), old_gen);
        assert_eq!(count, 0);
        assert!(browse.state.sections().is_empty());
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
    }

    #[test]
    fn blocking_section_failure_preserves_an_auth_401_published_during_the_request() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, sid, _) = registered_source();
        let count = ensure_sections_with(&mut browse.state, |_| {
            crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
            None
        });

        assert_eq!(count, 0);
        assert_eq!(
            crate::plex::server_probe_result(sid),
            Some(crate::plex::probe::Outcome::Unauthorized)
        );
        assert_eq!(
            browse.state.sources()[0].state,
            SourceState::Unauthorized,
            "browse mirrors the canonical merged answer"
        );
    }

    /// Regression: `reset()` dropped the three result mailboxes but left the single-flight
    /// flags set, and those are cleared ONLY inside a successful mailbox take. Sequence:
    /// scroll Library so a page fetch spawns → BACK to Home (pump stops running) → the worker
    /// lands its result → switch profile → `install_pms` calls `reset()` and nulls the mailbox
    /// → the flag is now true with nothing left that can ever clear it. `maybe_spawn` returns
    /// early forever and the Library is a spinner until the app is killed.
    #[test]
    fn reset_clears_the_single_flight_flags_with_the_mailboxes() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.adapter.fetching.store(true, Ordering::SeqCst);
        browse.adapter.genre_fetching.store(true, Ordering::SeqCst);
        browse.adapter.letters_fetching.store(true, Ordering::SeqCst);
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = None;

        browse.reset();

        assert!(
            !browse.adapter.fetching.load(Ordering::SeqCst),
            "page fetch stayed latched — Library wedges"
        );
        assert!(
            !browse.adapter.genre_fetching.load(Ordering::SeqCst),
            "genre fetch stayed latched"
        );
        assert!(
            !browse.adapter.letters_fetching.load(Ordering::SeqCst),
            "letters fetch stayed latched"
        );
    }

    /// `reset()` must also drop the retry backoff, or a profile switch inherits the previous
    /// user's cooldown and stalls the first page fetch for up to ~2s.
    #[test]
    fn reset_clears_the_retry_backoff() {
        // Takes the crate lock for the same reason the fetch-machine tests below do — see the note
        // there. `reset()` is the most destructive call in this module, and a test that makes it
        // without the lock is not testing concurrently, it is CORRUPTING whoever is.
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.state.retry_cd = 120;
        browse.reset();
        assert_eq!(browse.state.retry_cd, 0);
    }

    // ---- the three-state fetch machine ---------------------------------------------------------
    //
    // These drive the same owned pump core that reports to `ui::idle`'s process-global flag — the
    // exact obligation `ui/xfade.rs` inherited when its `tick` started doing the same — so they
    // take the CRATE-wide serial lock, not a module-local one. Their local state has one default
    // row and no sections; `maybe_spawn` returns before it can reach the network, so nothing here
    // spawns a worker.

    /// One default state with no section table, used by store-only tests that never land a page.
    fn seed_one_section(browse: &mut TestBrowse) {
        browse.reset();
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = None;
        browse.state.states = vec![SecState::default()];
    }
    /// Land what a worker would post for the CURRENT query: `total < 0` is the failure sentinel.
    fn land_page(browse: &mut TestBrowse, total: i64, items: usize) {
        let client = crate::plex::client();
        let r = PageResult {
            client,
            token_gen: client.token_gen(),
            gen: browse.state.query_gen(),
            sec: 0,
            start: 0,
            items: (0..items).map(|_| PmsMovie::default()).collect(),
            total,
            sorts: None,
        };
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        let _outcome = browse.pump();
    }

    /// THE bug: a failed first page armed the retry cooldown and nothing else, so `total` stayed
    /// -1, `loading_initial()` stayed true and the Library grid spun with no way out — for the
    /// rest of the session, on the user's own server. The failure must now be a STATE the screen
    /// can see, and the spinner must stop.
    #[test]
    fn a_failed_first_page_leaves_the_section_failed_and_not_loading() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, _) = registered_page_source();
        land_page(&mut browse, -1, 0);
        assert_eq!(browse.fetch_state(), SecFetch::Failed);
        assert!(
            !browse.loading_initial(),
            "the grid must stop spinning on a failure"
        );
        assert_eq!(
            browse.state.cur_state().unwrap().total,
            -1,
            "…with nothing to show, which is what makes it the SCREEN's failure too"
        );
    }

    /// A served page is Ready, and stays the plain "here are your items" state.
    #[test]
    fn a_served_page_leaves_the_section_ready() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, _) = registered_page_source();
        land_page(&mut browse, 3, 3);
        assert_eq!(browse.fetch_state(), SecFetch::Ready);
        assert!(!browse.loading_initial());
        assert_eq!(browse.state.cur_state().unwrap().total, 3);
    }

    /// An EMPTY answer is an answer — `Ready`, never `Failed`. The library really does hold
    /// nothing (an unwatched filter that matches none, a section still being scanned), and the
    /// grid's own "Nothing here matches" line is the right read-out. This is `StatusKind::Empty`'s
    /// rule, stated in the state machine so a screen cannot get it wrong.
    #[test]
    fn an_empty_but_successful_listing_is_ready_not_failed() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, _) = registered_page_source();
        land_page(&mut browse, 0, 0);
        assert_eq!(browse.fetch_state(), SecFetch::Ready);
        assert_eq!(browse.state.cur_state().unwrap().total, 0,
            "an empty library is an answer, not a fault");
        assert!(!browse.loading_initial());
    }

    /// A failure belongs to the query it was fetched for. Re-query (a sort/filter/section change
    /// wipes the store) and the section is Loading again, not stuck wearing the old failure —
    /// otherwise the read-out would blame a listing the user has already replaced.
    #[test]
    fn a_requery_clears_a_previous_failure() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, _) = registered_page_source();
        land_page(&mut browse, -1, 0);
        assert_eq!(browse.fetch_state(), SecFetch::Failed);
        browse.state.requery();
        assert_eq!(browse.fetch_state(), SecFetch::Loading);
        assert!(browse.loading_initial());
    }

    // ---- the SOURCE's own state, one layer up ---------------------------------------------------
    //
    // Same three states, one layer up, and graded through the per-source flags rather than through
    // `ensure_sections`: the fetch half needs a server, and a host test that reached for one would
    // be dialling whatever address another module's test had just registered.
    //
    // `cur_source_state` is a PROJECTION of `reachable`/`sections_done` — there is no fourth field
    // to set, which is the point of resolving it that way: the flags the Sources list already dims
    // a group by are the flags the read-out reads.

    /// Seed one source in a chosen phase and make it the CURRENT server, so the empty-table
    /// fallback in [`cur_source_state`] resolves to it rather than to whatever the registry was
    /// left holding. Registration dials nothing — it publishes a slot.
    fn seed_one_source(
        browse: &mut TestBrowse,
        reachable: bool,
        sections_done: bool,
    ) -> usize {
        // The CURRENT server's id, registered or not — see `seed_sources_for_test` for why nothing
        // is registered here. It makes `cur_source_idx`'s empty-table fallback resolve to this row.
        browse.seed_sources(vec![BrowseSource {
            sid: crate::plex::current_server(),
            client_addr: 0,
            token_gen: 0,
            machine_id: "mach-0".into(),
            owned: true,
            name: "nas-home".into(),
            handle: "friend".into(),
            state: if reachable {
                SourceState::Reachable
            } else {
                SourceState::Unreachable
            },
            tier: None,
            sections_done,
            counts_done: true,
            retry_cd: 0,
        }]);
        0
    }

    /// THE bug, one layer up: `ensure_sections` folded every failure into an empty table, so the
    /// screen saw exactly what it sees before the first request — no section, no state, and
    /// `fetch_state()` answering `Loading` out of its `unwrap_or` — and spun forever with no way
    /// out. A source that did not answer must be a state the screen can SEE.
    #[test]
    fn a_source_that_did_not_answer_is_observable_rather_than_an_eternal_spinner() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        seed_one_source(&mut browse, true, false);
        assert_eq!(
            browse.state.cur_source_state(),
            SecFetch::Loading,
            "nobody has asked it anything yet"
        );
        browse.state.source_mut(0).unwrap().set_reachable(false);
        assert_eq!(
            browse.state.cur_source_state(),
            SecFetch::Failed,
            "the screen must be able to see this"
        );
    }

    /// An account with nothing we browse ANSWERED. `Ready` with no sections, never `Failed` — the
    /// same reason an empty listing is (`StatusKind::Empty`), and the case that lands in the very
    /// same two `unwrap_or` defaults as a failure and so used to spin identically.
    #[test]
    fn a_source_with_no_browsable_library_answered_and_did_not_fail() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        seed_one_source(&mut browse, true, true);
        assert_eq!(
            browse.state.cur_source_state(),
            SecFetch::Ready,
            "an empty answer is an answer"
        );
        assert_eq!(browse.section_count(), 0);
    }

    /// A served table clears a previous failure and seeds one state per section, and from then on
    /// the state is read off the SECTION's source rather than off the current server.
    #[test]
    fn a_served_table_clears_the_failure_and_seeds_its_states() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        seed_one_source(&mut browse, false, false);
        assert_eq!(browse.state.cur_source_state(), SecFetch::Failed);
        {
            let s = browse.state.source_mut(0).unwrap();
            s.set_reachable(true);
            s.sections_done = true;
        }
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "Film Club".into(), SecKind::Movie),
            ],
        );
        assert_eq!(browse.state.cur_source_state(), SecFetch::Ready);
        assert_eq!(browse.section_count(), 2);
        assert!(browse.loading_initial(), "a fresh section has not answered yet");
    }

    // ---- the (source, section) table ------------------------------------------------------------
    //
    // These seed SOURCES directly and mark every phase done, so `maybe_discover` picks nothing and
    // no worker is spawned — the same discipline as the fetch-machine tests above, one layer up.
    // Their `sid` is `UNSET`, which resolves to no client, so even a spawn could reach no socket.

    pub(super) fn a_source(name: &str, handle: &str, reachable: bool) -> BrowseSource {
        BrowseSource {
            sid: ServerId::UNSET,
            client_addr: 0,
            token_gen: 0,
            // the machine id doubles as the fixture's identity, and OWNERSHIP follows the handle
            // here (a fixture, not the product rule — `sync_roster` takes `owned` from the roster,
            // because a share whose `sourceTitle` plex.tv did not send is still a share)
            machine_id: name.to_string(),
            owned: handle.is_empty(),
            name: name.into(),
            handle: handle.into(),
            state: if reachable {
                SourceState::Reachable
            } else {
                SourceState::Unreachable
            },
            tier: None,
            sections_done: true,
            counts_done: true,
            retry_cd: 0,
        }
    }
    /// THE reason the table gained a source dimension. Measured against the real share on
    /// 2026-08-11: both servers have a section `1`, and they are different libraries. A bare key
    /// names two things, so every row carries its source and the two rows coexist.
    #[test]
    fn two_servers_both_have_a_section_one_and_the_table_tells_them_apart() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);

        assert_eq!(browse.section_count(), 3);
        let ours = &browse.state.sections()[0];
        let theirs = &browse.state.sections()[2];
        assert_eq!((ours.key, ours.src), (1, 0), "our section 1, on source 0");
        assert_eq!(
            (theirs.key, theirs.src),
            (1, 1),
            "THEIR section 1 — same key, different source"
        );
        assert_eq!(
            (browse.section_title(0), browse.section_title(2)),
            ("Movies", "Film Club")
        );
        // and the chip's annotation follows the section being browsed, not the account
        assert_eq!(browse.state.sources()[theirs.src].handle, "friend");
        assert_eq!(browse.state.sources()[ours.src].handle, "",
            "your own libraries carry no owner at all");
    }

    /// A source discovered LATE must never move an existing index. `PageResult.sec` is a section
    /// index, so a table that reshuffled under an in-flight fetch would splice one library's items
    /// into another's store — the soundness the old `ensure_sections` early-return provided and
    /// APPEND-ONLY now provides for every source rather than only for the second call.
    #[test]
    fn a_source_arriving_late_appends_and_moves_no_existing_index() {
        let _g = crate::testlock::serial();
        // A landing re-derives every row's favourite and, since 2026-09-05, REPOINTS `cur`
        // when that takes the current section's away — so this test's own subject (an index
        // holding still) is only well-defined against a known favourite set. Without a
        // scratch session it grades whatever `auth.json` a neighbouring test left behind.
        let _t = TempPins::new("late-append");
        _t.watching("u-late-append");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        browse.state.set_cur(1);
        let before = (
            browse.state.cur(),
            browse.section_title(1).to_string(),
            browse.state.states().len(),
        );

        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_eq!(
            browse.state.cur(),
            before.0,
            "the library being browsed is still the one at that index"
        );
        assert_eq!(browse.section_title(1), before.1);
        assert_eq!(
            browse.state.states().len(),
            browse.section_count(),
            "states stay in lockstep with the table"
        );
        assert_eq!(browse.state.states().len(), before.2 + 1);

        // A RE-discovery ("Check for new shares", or a server that came back) re-offers the same
        // list: every row is already there, so nothing is duplicated and nothing moves…
        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_eq!(browse.section_count(), 3);
        assert_eq!(browse.state.cur(), before.0);
        // …while a library the owner has CREATED since is appended, at the end, where it cannot
        // disturb an index anything is already holding.
        browse.append_sections(
            1,
            vec![
                (1, "Film Club".into(), SecKind::Movie),
                (4, "Club Shows".into(), SecKind::Show),
            ],
        );
        assert_eq!(browse.section_count(), 4);
        assert_eq!(browse.section_title(3), "Club Shows");
        assert_eq!(
            browse.section_title(1),
            before.1,
            "and the row we were browsing is untouched"
        );
    }

    // ---- the Home selection: defaults, persistence, and one answer per PROFILE ------------------
    //
    // The RULES are `plex::pins` and are graded there, pure. What is graded here is the plumbing
    // around them, which is where the failures actually live: does an answer reach the disk, does
    // it come back, and does it come back to the person who gave it.

    /// The session-isolation guard, under the name this module's tests have always called it.
    /// It is `plex::session::TempSession` now — one guard rather than the three near-identical
    /// copies that had grown here, in `ui::onboard` and in `auth`; the local alias is kept only so
    /// the dozens of call sites below still read as pinning THIS module's per-profile answer.
    use crate::plex::session::TempSession as TempPins;

    /// One account, two servers — seeded and discovered exactly as a boot does it.
    fn seed_two_servers(browse: &mut TestBrowse) {
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
    }

    /// **The first-run defaults, and the one rule the control has.**
    ///
    /// This asserted `(true, true, true)` — every granted library on — for as long as deliverable F
    /// had nowhere to ask the question: defaulting a share OFF with no screen to say so means it is
    /// granted, discovered, browsable and silently absent from Home with no control anywhere to
    /// turn it on. The screen exists now, so the design's own default is back, and it is the state
    /// that screen SHOWS before anybody touches it.
    #[test]
    fn your_own_libraries_start_on_home_and_a_friends_does_not() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("defaults");
        t.watching("u-owner");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
            (true, true, false),
            "yours On, a friend's Off"
        );
        assert_eq!(browse.state.pinned_count(), 2);

        assert!(
            browse.state.toggle_pin(2),
            "…and a friend's can be turned on, which is what makes it a decision"
        );
        assert_eq!(browse.state.pinned_count(), 3);
        assert!(
            browse.state.toggle_pin(0) && browse.state.toggle_pin(1),
            "your own can be unpinned — a preference, not a mistake"
        );
        assert_eq!(browse.state.pinned_count(), 1);

        assert!(browse.pinned(2) && browse.state.pinned_count() == 1);
        assert!(!browse.state.toggle_pin(2), "the last pinned library is refused");
        assert!(browse.pinned(2), "…and refused means UNCHANGED, not toggled twice");
        assert_eq!(browse.state.pinned_count(), 1);
    }

    /// **The SHARED fixture's shape is its own, not the disk's.**
    ///
    /// [`seed_two_source_table_for_test`] is used by three dozen tests in a dozen modules and its
    /// doc promises one thing — four libraries projecting to two library-type pills. That promise
    /// is about the pins as much as about the table, because [`append_sections`] ends in
    /// [`resolve_pins`]. This plants exactly the record that broke it: an answer for the CURRENT
    /// profile naming this fixture's own machines, with Movies switched off.
    ///
    /// It is the regression artifact for a failure that was NOT a race. A record of this shape,
    /// for the empty profile key, was sitting in one checkout's `target/debug/deps/auth.json` —
    /// which is where `paths::in_app_dir` resolves for a TEST BINARY — and it made
    /// `app::bridge::library_publishes_the_actual_container_strip` and
    /// `app::chrome::four_libraries_on_two_servers_publish_two_type_destinations` fail alone,
    /// single-threaded, in that checkout only. Run against the fixture as it was, this test is red
    /// with the Movies pill missing, in exactly the way those two were.
    #[test]
    fn the_shared_fixture_resolves_the_defaults_over_a_recorded_answer() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("fixture-owns-its-pins");
        t.watching("u-fixture-owns-its-pins");
        let user = crate::plex::session::current_profile_key();
        let lib = |machine: &str, key| crate::plex::session::PinnedLib {
            machine_id: machine.into(),
            key,
        };
        assert!(
            crate::plex::session::update(|s| {
                let mut next = s.clone();
                next.set_pins_for(
                    &user,
                    crate::plex::session::HomePins {
                        user: user.clone(),
                        asked: true,
                        on: vec![lib("mac-mini", 2)],
                        off: vec![lib("mac-mini", 1), lib("nas-home", 1), lib("nas-home", 2)],
                    },
                );
                Some(next)
            }),
            "the answer really is on disk, or this test grades nothing"
        );

        let mut browse = TestBrowse::default();
        seed_two_source_table_for_owner_test(&mut browse.state);

        assert_eq!(
            (browse.state.tab_kind(0), browse.state.tab_kind(1), browse.tab_count()),
            (Some(SecKind::Movie), Some(SecKind::Show), 2),
            "the fixture's own two pills, whatever anybody recorded for this profile"
        );
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2), browse.pinned(3)),
            (true, true, false, false),
            "…and they are the OWNERSHIP defaults: yours On, a friend's Off"
        );
        assert!(
            crate::plex::session::peek().pins_for(&user).is_none(),
            "the record was forgotten rather than worked around, so a later resolve agrees"
        );
    }

    /// **The editor's commit is one write for the whole batch, not one per toggle.** [`toggle_pin`]
    /// above records on every call because it has no draft standing between the press and the
    /// store; [`apply_pins`] is what a caller with one (`screens::onboard`) reaches for instead —
    /// every
    /// edit lands in the SAME `record_pins` call, so an editing session that flips three rows costs
    /// this app one write and one generation bump, exactly as it costs one press of Done.
    #[test]
    fn apply_pins_writes_the_whole_batch_in_one_record() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("apply-pins");
        t.watching("u-owner");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, true, false));

        browse.state.apply_pins(&[(2, true), (1, false)]);
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
            (true, false, true),
            "every edit in the batch landed"
        );

        let sess = crate::plex::session::peek();
        let rec = sess.pins_for(&crate::plex::session::current_profile_key());
        assert!(
            rec.is_some_and(|r| r.asked),
            "one commit is still a recorded answer"
        );
    }

    /// **A selection outlives the run.** Every flip was in-memory until 2026-08-21, so the answer
    /// was gone by the next boot and the ownership default came back — which reads as the switch
    /// not working rather than as nothing having been written down.
    #[test]
    fn a_selection_survives_the_table_being_rebuilt() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("persist");
        t.watching("u-owner");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        assert!(browse.state.toggle_pin(2) && browse.state.toggle_pin(1)); // the share On, one of ours Off
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));

        // …and now the table is wiped and re-discovered, which is what a profile switch, a
        // sign-in and a `reset` all do
        seed_two_servers(&mut browse);
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
            (true, false, true),
            "the answer came back"
        );
    }

    /// **The answer reaches Home before that server's libraries have been ENUMERATED.**
    ///
    /// Every catalog screen drives discovery, but the share's Home hubs may land before its section
    /// worker. In that interval the share is in the roster with no row in the section table — and
    /// `pms::feeds_home`'s "a library nobody has discovered is undecided, not unpinned" rule then
    /// put a friend's shelves back on the front door of somebody who had turned them off the night
    /// before. `library_pins` is the join, and the recorded answer is the other half of it.
    #[test]
    fn a_recorded_answer_reaches_home_before_that_servers_sections_do() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("unenumerated");
        t.watching("u-owner");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        browse.state.record_pins(true); // `Start watching` on the defaults: ours On, the friend's Off

        // the next boot, before the share's section worker has landed
        let boot = |browse: &mut TestBrowse| {
            browse.seed_sources(vec![
                a_source("mac-mini", "", true),
                a_source("nas-home", "friend", true),
            ]);
            browse.append_sections(
                0,
                vec![
                    (1, "Movies".into(), SecKind::Movie),
                    (2, "TV Shows".into(), SecKind::Show),
                ],
            );
        };
        boot(&mut browse);
        assert_eq!(
            browse.state.sections().len(),
            2,
            "the share has not answered — it contributes no rows"
        );
        let pins = browse.state.library_pins();
        assert!(
            pins.contains(&(1, 1, false)),
            "the friend's recorded Off is reported anyway, or Home reads it as undecided: {pins:?}"
        );
        assert_eq!(
            pins.len(),
            3,
            "…and nothing else is invented: two enumerated rows plus the one record"
        );

        // the other direction, so this is a JOIN and not a blanket "a share is off"
        t.watching("u-owner");
        seed_two_servers(&mut browse);
        assert!(browse.state.toggle_pin(2));
        browse.state.record_pins(true);
        boot(&mut browse);
        assert!(
            browse.state.library_pins().contains(&(1, 1, true)),
            "a recorded On reaches Home the same way"
        );

        // and a source the record cannot NAME is left undecided rather than joined by accident
        browse.seed_sources(vec![a_source("mac-mini", "", true), {
            let mut s = a_source("nas-home", "friend", true);
            s.machine_id = String::new();
            s
        }]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        assert!(
            browse.state.library_pins().iter().all(|&(si, _, _)| si == 0),
            "a nameless machine joins nothing"
        );
    }

    /// **A flip made while a friend's server is asleep does not withdraw the answer about it.**
    ///
    /// `record_pins` writes the section TABLE, which holds only what has answered — and
    /// `set_pins_for` replaces a profile's record wholesale. So without the merge, one switch
    /// flipped on a boot the share missed erased the share's recorded answer, and the ownership
    /// default came back for a library the user had already decided about.
    #[test]
    fn a_flip_made_while_a_share_is_absent_does_not_erase_its_answer() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("merge");
        t.watching("u-owner");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        assert!(browse.state.toggle_pin(2), "the friend's library goes on Home");
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, true, true));

        // a boot the share missed entirely, on which one of our own is turned off
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        assert!(browse.state.toggle_pin(1));
        assert!(
            browse.state.library_pins().contains(&(1, 1, true)),
            "the absent share is still recorded On"
        );

        // …and the next boot on which it DOES answer finds both answers intact
        seed_two_servers(&mut browse);
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));
    }

    /// **THE requirement: the selection is per PROFILE.** It hung off the `Session` — one per
    /// install — so a household could hold exactly one opinion about a friend's films, and
    /// switching profile left the previous person's shelves on the front door.
    #[test]
    fn two_profiles_keep_their_own_home_selections_across_a_switch() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("profiles");
        let mut browse = TestBrowse::default();

        // Dad wants the friend's films on Home and does not want his own TV shows there.
        t.watching("u-dad");
        seed_two_servers(&mut browse);
        assert!(browse.state.toggle_pin(2) && browse.state.toggle_pin(1));
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (true, false, true));

        // The kid switches in. Never asked, so the defaults — NOT dad's answer.
        t.watching("u-kid");
        seed_two_servers(&mut browse);
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
            (true, true, false),
            "a switch switches the shelves"
        );
        assert!(browse.state.toggle_pin(0), "…and the kid answers for themselves");
        assert_eq!((browse.pinned(0), browse.pinned(1), browse.pinned(2)), (false, true, false));

        // …and back, with dad's answer intact rather than overwritten by the kid's.
        t.watching("u-dad");
        seed_two_servers(&mut browse);
        assert_eq!(
            (browse.pinned(0), browse.pinned(1), browse.pinned(2)),
            (true, false, true),
            "one file, two answers"
        );
    }

    /// The route's own gate, end to end: two sources and an unanswered profile, then never again
    /// for that profile — while the person beside them is still owed the question.
    #[test]
    fn the_first_run_question_is_asked_once_per_profile() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("gate");
        t.watching("u-dad");
        let mut browse = TestBrowse::default();
        seed_two_servers(&mut browse);
        assert!(
            browse.first_run_asks(),
            "two sources, and nobody has asked this profile"
        );

        browse.state.record_pins(true); // what `Start watching` — and BACK, which commits the same thing — does
        assert!(!browse.first_run_asks(), "asked once, never again");
        t.watching("u-kid");
        assert!(
            browse.first_run_asks(),
            "…and the answer belongs to the person who gave it"
        );

        // A single-server install is not a question at all, whoever is watching.
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        assert!(!browse.first_run_asks());
    }

    /// **The switch governs the strip, and a strip POSITION is not a name.**
    ///
    /// Both halves in one run, because they are the same fact seen twice. Switching off the last
    /// favourite of a type removes its pill — the design's "a type left with no ON library draws
    /// no pill" — and the moment that can happen, *TV Shows* stops being pill 1 and becomes pill
    /// 0. Anything that stored the integer is now pointing at the wrong destination, which is why
    /// `ui::widgets::Pill::Section` carries a `SecKind`.
    ///
    /// The pin state is SET rather than resolved, deliberately: `resolve_pins` consults this
    /// machine's own signed-in session (`session::peek`), so a test that let it decide would be
    /// asserting against whatever `home_pins` the developer happens to have recorded — green here
    /// and red on a clean checkout, the shape `[[make-check-hides-host-assumptions]]` describes.
    /// That hazard is older than this test and is not this landing's to fix; stating the intent is.
    /// **Issue #68, at the layer that can answer it.** Two TV libraries on one server sit behind
    /// ONE *TV Shows* pill — `tab_section` opens exactly one of them — so the head of the Library
    /// is the only place that can say the other exists. This is the fact it says it from.
    ///
    /// The three assertions are the three ways the head can be wrong: silence where there is a
    /// choice, a `1 of 1` where there is not, and a position that disagrees with the panel the
    /// same head opens.
    #[test]
    fn two_libraries_behind_one_pill_have_a_position_and_a_lone_one_has_none() {
        let _g = crate::testlock::serial();
        let _t = TempPins::new("kind-position");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
                (3, "Animes".into(), SecKind::Show),
            ],
        );
        assert_eq!(
            browse.tab_count(),
            2,
            "still two pills — the second TV library folds onto the one it shares a type with, \
             which is the whole shape of the report"
        );
        assert_eq!(
            browse.kind_position(1),
            Some((1, 2)),
            "the TV library the pill opens on is the FIRST of two"
        );
        assert_eq!(
            browse.kind_position(2),
            Some((2, 2)),
            "…and the reported one the second"
        );
        assert_eq!(
            browse.kind_position(0),
            None,
            "the lone film library has no position: `1 of 1` would advertise a choice that does \
             not exist"
        );

        // …and the count is a promise about the list the head OPENS, so it follows the same
        // favourite filter that list does rather than the grant.
        set_pinned_for_owner_test(&mut browse.state, 2, false);
        assert_eq!(
            browse.kind_position(1),
            None,
            "with its sibling switched off there is nowhere else to go, and nothing to count"
        );
        assert_eq!(
            browse.source_rows().len(),
            1,
            "the panel agrees — one row, so a `1 of 2` beside it would have been a lie"
        );
    }

    /// **The scope both the head's row and the panel it opens now share.** Two surfaces read this:
    /// the owned Library row and its Sources menu behind `+N`. Neither may use
    /// [`source_rows`], which is scoped through `cur_kind()` and therefore lags a tab press by the
    /// length of the page fade — the row drew the MOVIE libraries under a *TV Shows* tab and kept
    /// them (its cache key is the viewed section, which does not move again at the commit), and the
    /// popover listed the other type's libraries under a row naming this one.
    ///
    /// It is graded here rather than at either call site because both of those go through text
    /// measurement — `crate::text` is SDL2_ttf, which the host test build does not link — so this
    /// is the layer at which the shared decision is reachable at all.
    #[test]
    fn the_rows_a_head_offers_follow_the_viewed_librarys_type_not_the_current_one() {
        let _g = crate::testlock::serial();
        let _t = TempPins::new("rows-for-section");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "Films".into(), SecKind::Movie),
                (3, "TV Shows".into(), SecKind::Show),
                (4, "Animes".into(), SecKind::Show),
            ],
        );
        // browsing a FILM library, asking about a SHOW one — the mid-fade shape exactly
        browse.state.set_cur(0);
        let rows = browse.state.source_rows_for(3);
        let shows: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            shows,
            vec!["TV Shows", "Animes"],
            "the rows must follow the section asked about, not `cur()`"
        );
        assert_eq!(
            browse.source_rows().len(),
            2,
            "…while `source_rows` answers for the films, which is what made it the wrong call"
        );

        // …and it is the FAVOURITE filter too, so the count on a head can never promise a row the
        // panel behind it will not draw.
        set_pinned_for_owner_test(&mut browse.state, 3, false); // "Animes"
        assert_eq!(
            browse.state.source_rows_for(3).len(),
            1,
            "a switched-off library draws no row here either"
        );
        assert_eq!(
            browse.kind_position(3),
            None,
            "…and so there is nothing left to count"
        );
    }

    /// The position is read of the VIEWED library, which during a page fade is not `cur()` — so it
    /// may not be derived from the current section's kind the way [`source_rows`] is.
    #[test]
    fn a_position_is_scoped_to_the_type_of_the_library_it_is_asked_about() {
        let _g = crate::testlock::serial();
        let _t = TempPins::new("kind-position-scope");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "Films".into(), SecKind::Movie),
                (3, "TV Shows".into(), SecKind::Show),
            ],
        );
        browse.state.set_cur(2); // browsing the SHOW library…
        assert_eq!(
            browse.kind_position(0),
            Some((1, 2)),
            "…and a film library still counts against the films, not against what is on screen"
        );
        assert_eq!(
            browse.kind_position(2),
            None,
            "the lone show library, from the same call"
        );
    }

    /// **Issue #68's second half, reported by the person who hit it.** Two TV libraries on one
    /// server, the pill opening the one they did not want — so they did the obviously right thing
    /// and switched the other OFF in *Favorite libraries*. Nothing changed: "even if I disable my
    /// Anime library in the settings, only my Animes library is populated under TV Shows".
    ///
    /// They were not wrong about the control. In the shipped build the favourite switch governed
    /// HOME alone and `tab_section` filtered on `s.kind` and nothing else, so a pill resolved to
    /// the first library of its type whether or not the user had just told the app to stop showing
    /// it. That is worse than the missing switcher beside it: the one workaround the UI offered was
    /// correct, and the app ignored it.
    #[test]
    fn switching_a_librarys_favourite_off_repoints_its_tab_at_the_one_that_is_left() {
        let _g = crate::testlock::serial();
        let _t = TempPins::new("tab-follows-favourite");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        // The reporter's shape, in their order: the pill lands on the library they were trying to
        // get away from, because table order is the server's and nothing else.
        browse.append_sections(
            0,
            vec![
                (1, "Animes".into(), SecKind::Show),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        let shows = browse.state.tab_of_kind(SecKind::Show).expect("the type has a pill");
        assert_eq!(
            browse.state.tab_section(shows),
            Some(0),
            "the pill opens the first of the two — which is the complaint, not the bug"
        );

        // …and now the switch they actually reached for.
        set_pinned_for_owner_test(&mut browse.state, 0, false);

        assert_eq!(
            browse.state.tab_section(shows),
            Some(1),
            "with Animes switched off the TV Shows pill must open the library that is left"
        );
        assert!(
            browse.state.tab_has_favorite(SecKind::Show),
            "…and it keeps its pill: one of the two is still on"
        );
    }

    #[test]
    fn switching_off_a_types_last_favourite_takes_its_pill_and_renumbers_the_rest() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-reshape");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        assert_eq!(browse.tab_count(), 2, "both types have a favourite to start with");
        assert_eq!(browse.state.tab_of_kind(SecKind::Show), Some(1));

        // …and this profile has since switched its film library off.
        set_pinned_for_owner_test(&mut browse.state, 0, false);

        assert_eq!(
            browse.tab_count(),
            1,
            "the type with no favourite left draws no pill"
        );
        assert_eq!(browse.tab_title(0), "TV Shows");
        assert_eq!(
            browse.state.tab_of_kind(SecKind::Movie),
            None,
            "a switched-off type has no position at all — not position 0"
        );
        assert_eq!(
            browse.state.tab_of_kind(SecKind::Show),
            Some(0),
            "…and the type that remains has MOVED, which is the whole hazard"
        );
        assert_eq!(
            browse.state.tab_section(0),
            Some(1),
            "the surviving pill opens the surviving library"
        );
    }

    /// **A pill is a TYPE, never a person.** The strip names your own libraries; a friend's film
    /// library gets no pill of its own (the toolbar chip under it says whose), but a type only they
    /// have does — otherwise that content is unreachable from the strip at all. And the selection
    /// capsule for a borrowed library rests on its TYPE's pill, so nothing is ever homeless.
    #[test]
    fn the_tab_strip_grows_by_types_and_never_by_people() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-types");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        browse.append_sections(
            1,
            vec![
                (1, "Film Club".into(), SecKind::Movie),
                (2, "Their Shows".into(), SecKind::Show),
            ],
        );

        assert_eq!(
            browse.tab_count(),
            2,
            "your Movies, plus the shows nobody of yours provides"
        );
        assert_eq!((browse.tab_title(0), browse.tab_title(1)), ("Movies", "TV Shows"));
        assert_eq!(browse.state.tab_section(1), Some(2));
        assert_eq!(
            browse.tab_of_section(1),
            Some(0),
            "their films ride YOUR Movies pill — same type, one level"
        );
        assert_eq!(
            browse.tab_of_section(2),
            Some(1),
            "their shows have a pill of their own"
        );
    }

    /// The case a BOOLEAN type could not express, and the reason [`SecKind`] exists: "does an owned
    /// library have this kind" has to be asked of a real type, or a friend's library of a type you
    /// do not own rides one of your pills and nothing in it is reachable from the strip.
    ///
    /// Stated here with an owner who has ONLY films and a friend who also shares shows. It used to
    /// be stated with a friend's MUSIC library, which read better — the two servers differed by a
    /// type neither could be confused for — but music is no longer a type this product has a level
    /// for, and a test may not be the last place a deleted feature survives.
    #[test]
    fn a_friends_library_of_a_type_you_do_not_own_gets_its_own_pill() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-missing-type");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]); // we own films and nothing else
        browse.append_sections(
            1,
            vec![
                (1, "Film Club".into(), SecKind::Movie),
                (2, "Their Shows".into(), SecKind::Show),
            ],
        );

        assert_eq!(
            browse.tab_count(),
            2,
            "your films, plus the shows nobody of yours provides"
        );
        assert_eq!(browse.tab_title(1), "TV Shows");
        assert_eq!(
            browse.tab_of_section(2),
            Some(1),
            "their shows are their own pill, NOT your Movies one"
        );
        assert_eq!(
            browse.tab_of_section(1),
            Some(0),
            "…while their films still ride yours"
        );
        // the wire types this product has a level for — and the ones it deliberately does not, which
        // is what keeps an unplayable library out of the strip, the Sources panel and the grid at once
        assert_eq!(SecKind::from_wire("movie"), Some(SecKind::Movie));
        assert_eq!(SecKind::from_wire("show"), Some(SecKind::Show));
        assert_eq!(
            SecKind::from_wire("artist"),
            None,
            "music has no level below the grid: no pill"
        );
        assert_eq!(SecKind::from_wire("photo"), None);
        assert_eq!(
            SecKind::from_wire("mixed"),
            None,
            "a type with no level is still refused"
        );
    }

    /// **The deliverable, as an assertion**: the strip does not grow by PEOPLE. Its pill list —
    /// and therefore its width, which is a pure function of the labels — does not move as the
    /// roster grows from one server to three, because every borrowed library folds onto the pill of
    /// a type you already have. Only a type gaining its FIRST favourite library may widen it, which
    /// is the second half below.
    ///
    /// The shape the design rejected is the control: a pill per section reaches eleven pills here,
    /// which is what measured 2133px against a 1540px track at three friends.
    #[test]
    fn the_strip_is_the_same_row_at_one_friend_and_at_three() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-width");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
            a_source("nas-home", "friend", true),
            a_source("nas-home", "friend", true),
        ]);
        // OWNED, deliberately: `tab_title` hands back a `&'static str` borrowed out of the section
        // table's own `String`s, and `append_sections` can reallocate that Vec — so a row captured
        // as borrows and compared after the next source lands is reading freed memory. Every
        // caller in the app consumes these inside one frame with no append in between, which is
        // what makes the signature sound in the product and unsound in a test that spans landings.
        let row = |browse: &TestBrowse| {
            (0..browse.tab_count())
                .map(|tab| browse.tab_title(tab).to_string())
                .collect::<Vec<_>>()
        };
        // We own FILMS and nothing else. The owner used to hold both types here, which made the
        // second half of this test need a third type (music) to have anything left over; with the
        // product's list down to two, the un-owned type has to be one of them.
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        let alone = row(&browse);
        assert_eq!(
            alone,
            vec!["Movies"],
            "a type with no favourite library draws no pill: we hold no shows yet"
        );

        for src in 1..=3 {
            browse.append_sections(
                src,
                vec![
                    (1, "Film Club".into(), SecKind::Movie),
                    (3, "Film Club".into(), SecKind::Movie),
                ],
            );
            assert_eq!(
                row(&browse),
                alone,
                "source {src} added a pill — the strip must not grow by people"
            );
        }
        assert_eq!(browse.section_count(), 7, "seven libraries…");
        assert_eq!(browse.tab_count(), 1, "…and still the one pill they all fold onto");

        // …and a type NOBODY owns grows the row by exactly one however many people share it. Every
        // fixture above is a type we own, which is why this half needs saying separately: it is the
        // only branch of the projection that can admit a borrowed library at all — a shared library
        // of a type you have none of defaults ON (`pins::default_on`), so it really does arrive as
        // a new pill rather than as a switched-off one nobody can reach.
        for src in 1..=3 {
            browse.append_sections(src, vec![(9, "Their Shows".into(), SecKind::Show)]);
        }
        assert_eq!(
            browse.tab_count(),
            2,
            "three friends sharing shows are ONE TV Shows pill"
        );
        assert_eq!(row(&browse).len(), 2);
    }

    /// A profile switch must not leave the previous account's pills on screen — and now that the
    /// strip is a projection of the FAVOURITE set rather than a permanent two, that is a statement
    /// about the pills themselves and not only about a cache. `reset()` empties the table, so the
    /// row it projects is EMPTY until the new account's own libraries are discovered; the pills
    /// then come back one type at a time as they land.
    ///
    /// The generation is what the tab row's label cache keys on, so it has to move across the
    /// reset too — otherwise `draw_tab_row` (which iterates the cache, not the live table) would go
    /// on drawing and hit-testing libraries the new user cannot open until some later landing
    /// happened to change the row.
    #[test]
    fn a_profile_switch_re_measures_the_strip_instead_of_keeping_the_last_accounts_pills() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-profile");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        assert_eq!(browse.tab_count(), 2);
        let before = browse.state.tabs_gen();

        browse.reset(); // install_pms: a different account signs in
        assert_eq!(
            browse.tab_count(),
            0,
            "the previous account's pills are gone, not inherited"
        );
        assert_ne!(
            browse.state.tabs_gen(),
            before,
            "…and the row's generation moved, so the label cache cannot serve them"
        );

        // the new account's own libraries land and the row is rebuilt from THEM
        browse.seed_sources(vec![a_source("nas-home", "", true)]);
        browse.append_sections(0, vec![(4, "Films".into(), SecKind::Movie)]);
        assert_eq!((browse.tab_count(), browse.tab_title(0)), (1, "Movies"));
    }

    /// The strip's own generation moves when the ROW changes and not when the TABLE does — which,
    /// once a table is appended to one source at a time, are different questions. Every borrowed
    /// library that folds onto a pill you already have bumps the table's generation and changes
    /// nothing in the row, so keying the label + width cache on the table re-measured every pill in
    /// the strip once per source, on Home's hot path, for a strip that had not moved.
    #[test]
    fn only_a_changed_row_costs_the_tab_cache_a_re_measure() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-cache");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        let (g0, table0) = (browse.state.tabs_gen(), browse.state.sections_gen());

        // two friends' film libraries land: both fold onto your Movies pill
        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        browse.append_sections(2, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_ne!(
            browse.state.sections_gen(),
            table0,
            "the TABLE's generation moved, twice"
        );
        assert_eq!(
            browse.state.tabs_gen(),
            g0,
            "…and the row did not, so it must not re-measure"
        );

        // …and the other direction, which is the half that makes the generation worth having: a
        // type gaining its FIRST favourite library really does reshape the row — a new pill — so
        // this landing MUST cost a re-measure where the three above must not.
        browse.append_sections(2, vec![(9, "Their Shows".into(), SecKind::Show)]);
        assert_ne!(
            browse.state.tabs_gen(),
            g0,
            "a new pill appeared: the cached labels and widths are stale"
        );
        let g1 = browse.state.tabs_gen();
        browse.append_sections(2, vec![(10, "More Shows".into(), SecKind::Show)]);
        assert_eq!(
            browse.state.tabs_gen(),
            g1,
            "…and the next one folds onto it again, costing nothing"
        );
    }

    /// **The Source chip cannot switch tabs, because it is scoped to the tab's own TYPE.**
    ///
    /// Owner-reported on the device build: picking a library in the Sources panel could land on one
    /// of a different type, which moves the selected section — and the tab is derived from the
    /// section's kind, so a toolbar control silently navigated the row above it.
    ///
    /// The scope is the fix, not a guard on the press: every row the panel offers is of the browsed
    /// type, so no reachable press can change the tab. Both servers' films appear together; neither
    /// server's shows do.
    #[test]
    fn the_sources_panel_offers_only_libraries_of_the_tab_being_browsed() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("picker-scope");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        browse.append_sections(
            1,
            vec![
                (1, "Film Club".into(), SecKind::Movie),
                (2, "Their Shows".into(), SecKind::Show),
            ],
        );
        // The friend's two libraries are types we own, so `pins::default_on` starts them OFF and
        // the picker — which is favourite-scoped now — would not offer them at all. Favourite them
        // explicitly: what is under test here is the TYPE scope, and it has to be graded on a
        // roster where both servers have something to contribute to each tab.
        set_pinned_for_owner_test(&mut browse.state, 2, true);
        set_pinned_for_owner_test(&mut browse.state, 3, true);

        // browsing a FILM library: both servers' film libraries, and no show library from either
        browse.state.set_cur(0);
        let films: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
        assert_eq!(
            films,
            vec!["Movies", "Film Club"],
            "both servers' films, nothing else: {films:?}"
        );

        // …and the same panel on the shows tab is the other list entirely
        browse.state.set_cur(1);
        let shows: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
        assert_eq!(
            shows,
            vec!["TV Shows", "Their Shows"],
            "both servers' shows: {shows:?}"
        );

        // …and the picker is FAVOURITE-scoped as well as type-scoped: switching one off takes it
        // out of the list, which is how a non-favourite library stops being reachable from here.
        // Settings' own editor (`all_source_rows`) is the unscoped list that brings it back.
        set_pinned_for_owner_test(&mut browse.state, 3, false);
        let shows: Vec<String> = browse.source_rows().iter().map(|r| r.title.clone()).collect();
        assert_eq!(shows, vec!["TV Shows"], "a non-favourite is not offered");
        assert!(
            browse.state.all_source_rows().iter().any(|r| r.title == "Their Shows"),
            "…but Settings still lists it, or it could never come back"
        );
        set_pinned_for_owner_test(&mut browse.state, 3, true);

        // the decisive property: every row the panel can activate keeps the browsed TYPE, so the
        // tab derived from it cannot move. Stated over the section each row opens, not its title.
        for r in browse.source_rows() {
            assert_eq!(
                browse.state.sections()[r.section].kind,
                SecKind::Show,
                "a row of another type is reachable"
            );
        }
    }

    /// **Reachability is a fact about NOW, and a page fetch is the only evidence that keeps
    /// arriving.** `sections_done` latches on success, so the discovery worker never asks that
    /// server anything again — without this a source that went offline an hour into the session
    /// could never stop reading as reachable, and its group would never dim. It moves in both
    /// directions, because a server that came back must stop being dimmed too.
    #[test]
    fn a_page_fetch_is_what_keeps_reachability_honest_after_discovery() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert!(
            browse.state.sources()[1].sections_done,
            "discovery is done — it will never re-ask by itself"
        );

        browse.state.source_mut(1).unwrap().set_reachable(false); // a page for THEIR library did not come back
        assert!(!browse.state.sources()[1].reachable(), "their group dims");
        assert!(
            browse.state.sources()[0].reachable(),
            "…and ours is untouched — it answered"
        );

        browse.state.source_mut(1).unwrap().set_reachable(true); // …and it comes back
        assert!(browse.state.sources()[1].reachable());
        assert_eq!(
            browse.state.sources()[1].retry_cd,
            0,
            "a server that answered is worth re-asking at once"
        );

        browse.state.source_mut(1).unwrap().state = SourceState::Unauthorized;
        browse.state.source_mut(1).unwrap().set_reachable(true);
        assert_eq!(
            browse.state.sources()[1].state,
            SourceState::Reachable,
            "a successful page clears a known 401"
        );
    }

    /// **The widening contract, pinned.** [`SourceState`] replaced a `bool`, and the whole claim of
    /// that commit is that it changed nothing — so the mapping is a test rather than a paragraph.
    ///
    /// The asymmetry is the part worth pinning: **`NotProbed` reads as reachable**. That looks
    /// wrong until you recall what it replaced — a source was seeded `reachable: true` at
    /// registration precisely so its group would not open dimmed before anything had been dialled.
    /// A future reader who "fixes" this to `false` will dim every group for the frames between
    /// registration and the first probe, which is a visible flicker on every boot.
    #[test]
    fn not_probed_reads_as_reachable_and_only_a_failed_dial_dims_a_group() {
        let _g = crate::testlock::serial();
        let mut s = a_source("nas-home", "friend", true);

        s.state = SourceState::NotProbed;
        assert!(
            s.reachable(),
            "nobody has dialled it — the group must not open dimmed"
        );
        assert_eq!(s.tier, None, "and nothing has told us which tier won");

        s.set_reachable(true);
        assert_eq!(s.state, SourceState::Reachable);
        assert!(s.reachable());

        s.set_reachable(false);
        assert_eq!(s.state, SourceState::Unreachable);
        assert!(!s.reachable(), "the ONLY state that dims a group");

        // Answered-but-refused is a token problem, not a network failure. The legacy reachability
        // question therefore remains true, while `ui::source_list` matches Unauthorized directly
        // and dims/disables the group because there is nothing browsable behind that credential.
        s.state = SourceState::Unauthorized;
        s.set_reachable(false);
        assert_eq!(
            s.state,
            SourceState::Unauthorized,
            "a status-folded request cannot erase a known 401"
        );
        assert!(
            s.reachable(),
            "it answered; this old bool projection is only the network question"
        );
    }

    #[test]
    fn registry_probe_state_and_tier_seed_and_update_the_browse_source() {
        let _g = crate::testlock::serial();
        struct Cleanup;
        impl Drop for Cleanup {
            fn drop(&mut self) {
                crate::plex::reset_servers_for_test();
            }
        }
        crate::plex::reset_servers_for_test();
        let _cleanup = Cleanup;
        let mut browse = TestBrowse::default();

        let sid = crate::plex::register_for_test("mach-A", "10.0.0.1", 32400, "tok", "cid");
        crate::plex::client_for(sid)
            .unwrap()
            .set_link(crate::plex::probe::Location::Remote);
        browse.sync_roster();
        assert_eq!(
            browse.state.sources()[0].state,
            SourceState::NotProbed,
            "a restored tier is not a current probe answer"
        );
        assert_eq!(
            browse.state.sources()[0].tier,
            Some(crate::plex::probe::Location::Remote)
        );

        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
        browse.sync_roster();
        assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
        assert_eq!(
            browse.state.sources()[0].tier,
            Some(crate::plex::probe::Location::Remote),
            "cached route metadata is retained"
        );

        crate::plex::client_for(sid)
            .unwrap()
            .set_link(crate::plex::probe::Location::Relay);
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Reachable);
        browse.sync_roster();
        assert_eq!(browse.state.sources()[0].state, SourceState::Reachable);
        assert_eq!(browse.state.sources()[0].tier, Some(crate::plex::probe::Location::Relay));

        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);
        browse.sync_roster();
        assert_eq!(browse.state.sources()[0].state, SourceState::Unreachable);
        assert_eq!(
            browse.state.sources()[0].tier,
            Some(crate::plex::probe::Location::Relay),
            "offline does not erase the last route"
        );
    }

    /// The same mapping on the projection the renderer actually sees, because `SrcGroup` carries
    /// its own copy of the question and two copies are how a widening drifts.
    #[test]
    fn the_source_group_projection_answers_reachability_the_same_way() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        // Set the SOURCE directly. `mark_source_reachable` takes a *section* index and resolves the
        // source through it (`sections()[i].src`), so with no sections seeded it early-returns —
        // which cost this test one failing run before the name gave it away.
        browse.state.source_mut(1).unwrap().set_reachable(false);
        let g = browse.state.source_groups();
        assert_eq!(g[0].state, SourceState::Reachable);
        assert!(g[0].reachable());
        assert_eq!(g[1].state, SourceState::Unreachable);
        assert!(!g[1].reachable(), "the dim the Sources list draws");
        assert_eq!(g[1].tier, None);
    }

    /// A REFUSED discovery spawn must back off, not retry at 60 Hz.
    ///
    /// `maybe_discover` runs once a frame, so releasing the single flight alone re-picks the same
    /// source on the very next frame — and `task::spawn` logs every refusal, so the app would write
    /// ~60 `task: spawn 'sources' REFUSED` lines a second into the one file on-device triage reads,
    /// exactly when the machine is under enough thread pressure to be worth reading about.
    ///
    /// The other half is what it must NOT do: nothing was asked of the server, so the source stays
    /// reachable and its discovery stays un-done. A refusal is ours, not theirs.
    ///
    /// Drives `discovery_spawn_refused` rather than `maybe_discover`, because there is no way to
    /// make the OS refuse a thread on demand — and then runs a real second of frames through
    /// `maybe_discover` to show the backoff actually holds the picker off. That call is safe here
    /// precisely BECAUSE the backoff is armed: every source is un-ready, so it decrements the
    /// counters and returns without dialling anything.
    #[test]
    fn a_refused_discovery_spawn_backs_off_instead_of_flooding_the_log() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
        ]);
        for i in 0..2 {
            let s = browse.state.source_mut(i).unwrap();
            s.sections_done = false; // both still want discovery
            s.counts_done = true;
        }
        browse.adapter.src_fetching.store(true, Ordering::SeqCst); // as `maybe_discover` armed it before spawning

        browse.state.discovery_spawn_refused_owned(&browse.adapter, 1);

        assert!(
            !browse.adapter.src_fetching.load(Ordering::SeqCst),
            "the single flight must be released"
        );
        assert_eq!(
            browse.state.sources()[1].retry_cd,
            SRC_RETRY_CD,
            "…and the next attempt is ~10s out, not 1 frame"
        );
        assert!(
            browse.state.sources()[1].reachable(),
            "a refused THREAD says nothing about their server"
        );
        assert!(
            !browse.state.sources()[1].sections_done,
            "…and it is still a source waiting to be discovered"
        );
        assert_eq!(browse.state.sources()[0].retry_cd, 0, "the other source is untouched");

        // one second of frames: the picker must not come back to it
        browse.state.source_mut(0).unwrap().retry_cd = SRC_RETRY_CD; // so nothing in this table is dialable
        for _ in 0..60 {
            browse.state.maybe_discover_owned(&browse.adapter, &mut execute_discovery);
        }
        assert!(
            !browse.adapter.src_fetching.load(Ordering::SeqCst),
            "no attempt was made in a whole second"
        );
        assert!(
            browse.state.sources()[1].retry_cd > 0,
            "…and the backoff still has most of its cooldown left"
        );
    }

    /// An EMPTY count landing is a failure, not an answer: the worker pushes one entry per request
    /// that succeeded. Latching `counts_done` on it would leave those rows reading their type word
    /// instead of their size for the rest of the session, with nothing able to fix it —
    /// `maybe_discover` skips a done source, and this is the bug class the module has now hit twice
    /// (the single-flight flags were the first).
    #[test]
    fn an_empty_count_landing_does_not_latch_the_probe_off() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, client) = registered_source();
        browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        if let Some(s) = browse.state.source_mut(0) {
            s.counts_done = false;
        }
        let epoch = browse.state.table_epoch();

        // nothing came back
        *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            epoch,
            0,
            SrcLanding {
                client,
                token_gen: client.token_gen(),
                name: String::new(),
                what: SrcWhat::Counts(Vec::new()),
            },
        ));
        let _ = browse.state.land_discovery_owned(&browse.adapter);
        assert!(
            !browse.state.sources()[0].counts_done,
            "an empty answer must leave the probe armed"
        );
        assert_eq!(browse.state.sections()[0].count, -1);

        // …and the real one does land, and does latch
        *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((
            epoch,
            0,
            SrcLanding {
                client,
                token_gen: client.token_gen(),
                name: String::new(),
                what: SrcWhat::Counts(vec![(1, 185)]),
            },
        ));
        let _ = browse.state.land_discovery_owned(&browse.adapter);
        assert!(browse.state.sources()[0].counts_done);
        assert_eq!(
            browse.state.sections()[0].count,
            185,
            "the row can say \"185 films\" now"
        );
    }

    /// With one source providing the canonical Movie and Show libraries, the two permanent type
    /// destinations resolve directly to those rows — and the Source chip is absent, not empty.
    #[test]
    fn one_source_resolves_both_permanent_type_destinations() {
        let _g = crate::testlock::serial();
        // The strip reads the favourite set, and the favourite set is resolved against the
        // RECORDED per-profile answer — so this test needs a session of its own, or it
        // grades whatever the host machine happens to have on disk.
        let _t = TempPins::new("strip-one-source");
        let mut browse = TestBrowse::default();
        browse.seed_sources(vec![a_source("mac-mini", "", true)]);
        browse.append_sections(
            0,
            vec![
                (1, "Movies".into(), SecKind::Movie),
                (2, "TV Shows".into(), SecKind::Show),
            ],
        );
        assert_eq!(browse.tab_count(), browse.section_count());
        for i in 0..browse.section_count() {
            assert_eq!(browse.state.tab_section(i), Some(i));
            assert_eq!(browse.tab_of_section(i), Some(i));
            assert_eq!(browse.tab_title(i), browse.section_title(i));
        }
        assert_eq!(
            browse.state.sources().len(),
            1,
            "…and the Source chip's own condition is false"
        );
    }

    /// A failure landing from a SUPERSEDED query must not blame the current one: the user has
    /// already changed sort/filter/section, a fresh fetch is on its way, and marking the new query
    /// Failed would show a failure read-out over a listing that is still perfectly healthy.
    #[test]
    fn a_stale_failure_landing_does_not_blame_the_current_query() {
        let _g = crate::testlock::serial();
        let (_cleanup, mut browse, _, client) = registered_page_source();
        let stale = browse.state.query_gen();
        browse.state.bump_gen(); // the query moved on under the in-flight fetch
        let r = PageResult {
            client,
            token_gen: client.token_gen(),
            gen: stale,
            sec: 0,
            start: 0,
            items: Vec::new(),
            total: -1,
            sorts: None,
        };
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        let _outcome = browse.pump();
        assert_eq!(
            browse.fetch_state(),
            SecFetch::Loading,
            "the current query has not answered yet — it has not failed"
        );
    }

    /// The optimistic edit behind the browse grid's context menu — three properties in one, because
    /// they are one press: the mark flips on the frame it is pressed, it flips in EVERY section's
    /// store rather than only the one on screen, and a row on another SERVER with the same key is
    /// untouched.
    ///
    /// Without this the write went out correctly and nothing on screen changed until a refetch, so
    /// the row read as having done nothing — the exact gap `pms::edit_item`'s doc records ("the item
    /// is on no shelf (a Library-grid or Related item): nothing to redraw").
    #[test]
    fn a_watched_edit_reaches_every_section_and_only_the_right_server() {
        let _g = crate::testlock::serial();
        let mut browse = TestBrowse::default();
        seed_one_section(&mut browse);
        let sid = crate::plex::ServerId::UNSET;
        let other = crate::plex::ServerId::from_raw(1);
        let row = |sid, rk: &str, resume: i64| {
            let mut m = PmsMovie::default();
            m.sid = sid;
            m.rk = rk.to_string();
            m.unwatched = true;
            m.resume_ms = resume;
            Some(m)
        };
        {
            let st = browse.state.states_mut();
            *st = vec![SecState::default(), SecState::default()];
            // the section on screen, the one browsed a minute ago (which keeps its items), and a
            // FRIEND's row carrying the same key — both servers number their items from 1
            st[0].items = SecItems::from_vec(vec![row(sid, "7", 90_000), row(other, "7", 0)]);
            st[1].items = SecItems::from_vec(vec![row(sid, "7", 0), row(sid, "9", 0)]);
        }

        assert!(
            browse.state.set_watched_local(sid, "7", true),
            "the item is in the store, so the edit lands"
        );
        let watched = |browse: &TestBrowse, sec: usize, i: usize| {
            let st = browse.state.states();
            let m = st[sec].items.get(i).unwrap();
            (m.watched, m.unwatched, m.resume_ms)
        };
        assert_eq!(
            watched(&browse, 0, 0),
            (true, false, 0),
            "…tick on, and the resume bar retires with it"
        );
        assert_eq!(
            watched(&browse, 1, 0),
            (true, false, 0),
            "…in a section that is not the one being browsed"
        );
        assert_eq!(
            watched(&browse, 0, 1),
            (false, true, 0),
            "…and never on the friend's item with the same key"
        );
        assert_eq!(
            watched(&browse, 1, 1),
            (false, true, 0),
            "…nor on an item that was not asked about"
        );

        assert!(
            !browse.state.set_watched_local(sid, "404", true),
            "an item in no section reports a miss"
        );
    }
}
