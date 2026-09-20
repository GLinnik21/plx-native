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
//! menu-shaped is hardcoded, with one measured exception: PMS 1.43.4 never advertises a
//! play-count sort in `Meta.Type[].Sort` on any section type, yet it DOES honour
//! `sort=viewCount:desc`/`:asc` on movie and show sections (an unrecognised key 500s the whole
//! listing, so this is not something to guess at for a section type that hasn't been proven).
//! [`with_plays_sort`] appends that one client-side entry where [`SecKind`] proves it works,
//! and only once, in case a future server starts advertising the key itself (issue #146).
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
    /// Answered, verified — the right machine — but only over a transport this build can never
    /// put a credential on (issue #95, plan §4). Not [`Self::Reachable`]: nothing is browsable
    /// behind it. Not [`Self::Unreachable`] either: the server is alive, and saying it is not
    /// sends the user to look at a router for nothing.
    InsecureOnly,
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
    /// share whose `sourceTitle` plex.tv did not send is still a share.
    ///
    /// It stays the RAW wire flag, and it is **no longer what the first-run default or the tab
    /// destination read**: both ask [`BrowseSource::household`], because a Plex Home managed
    /// profile is told `owned:false` about its own family server. This remains the answer to
    /// "does this ACCOUNT own it", which is a real question several other readers still have, and
    /// the two are kept apart on purpose.
    pub(crate) owned: bool,
    /// plex.tv's `home` on the grant, carried verbatim from the registry's [`ServerFacts`].
    /// Evidence for [`household`](Self::household); never read on its own.
    pub(crate) home: bool,
    /// plex.tv's `ownerId` on the grant, carried verbatim. Evidence for
    /// [`household`](Self::household); `0` means plex.tv named nobody and never matches a
    /// household member.
    pub(crate) owner_id: i64,
    /// **Is this OUR HOUSEHOLD'S server?** — [`crate::plex::is_household`]'s verdict on the three
    /// carried evidence fields above plus the Plex Home roster
    /// ([`crate::plex::session::Session::household_ids`]).
    ///
    /// A cached DERIVATION, not a fact from the wire, and it lives here rather than on
    /// `SourceRef`/`ServerFacts` for one reason: those carry evidence, which is durable, while
    /// this depends on a roster that arrives separately and can change under it. Keeping the
    /// verdict where the roster is already re-read is what lets `BrowseState`'s own readers stay
    /// self-contained instead of each re-deriving it from a session they would have to fetch.
    ///
    /// **Recomputed by the source-fact sync** ([`BrowseState::sync_roster_owned`]), beside the
    /// `owned` it sits next to — so it follows a roster ingest, a re-describe and a newly
    /// appearing source, which is every path that changes the evidence.
    ///
    /// It also follows a **Home-roster arrival that changes no source fact**, which needed its own
    /// trigger: [`BrowseState::discovery_needs_pump`] read the registry and never the session, so
    /// `/api/v2/home/users` landing alone — the one event that can reclassify every source at once
    /// while every `ServerFacts` stays byte-identical — never reached the sync at all. That gate
    /// now compares this cached verdict against a freshly graded one, and the sync answers a
    /// change by re-resolving the WHOLE pin table.
    ///
    /// **Who reads it**: the first-run/Settings pin default (`BrowseState::lib_refs` →
    /// `plex::pins`) and the tab destination's tiebreak ([`BrowseState::section_of_kind`]).
    pub(crate) household: bool,
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
        !matches!(self.state, SourceState::Unreachable | SourceState::InsecureOnly)
    }
    /// Record a generic PMS request that answered or did not. A failed request cannot distinguish
    /// HTTP status from transport/parse failure, so it preserves an Unauthorized or InsecureOnly
    /// result supplied by the identity prober; a successful request is enough evidence to clear
    /// any failure.
    #[cfg(test)]
    pub(crate) fn set_reachable(&mut self, ok: bool) {
        // A generic PMS request folds status/transport/parse errors into one `None`, so it cannot
        // disprove the more specific 401/InsecureOnly result the identity prober already
        // observed. Only a successful request clears either; the auth coordinator can explicitly
        // replace it with a later aggregate Unreachable result through the registry.
        if ok {
            self.state = SourceState::Reachable;
        } else if !matches!(self.state, SourceState::Unauthorized | SourceState::InsecureOnly) {
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
        Some(crate::plex::probe::Outcome::InsecureOnly) => SourceState::InsecureOnly,
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

/// [`BrowseSource::household`]'s one derivation: the carried grant evidence, graded against the
/// Plex Home roster by the rule that owns the question.
///
/// It is a free function rather than a method so that it reads the evidence and NOTHING else —
/// in particular not the cached verdict it is about to overwrite.
fn household_verdict(source: &BrowseSource, household: &[i64]) -> bool {
    crate::plex::is_household(
        crate::plex::GrantEvidence {
            owned: source.owned,
            home: source.home,
            owner_id: source.owner_id,
        }
        .grant(),
        household,
    )
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
    /// Your HOUSEHOLD's libraries start favourite and a friend's start favourite only where the
    /// household has no library of that type ([`crate::plex::pins::default_on`]); the last
    /// favourite cannot be turned off, or the app has nothing. The household and not the account
    /// — see [`BrowseSource::household`] and [`BrowseState::lib_refs`] for why plex.tv's raw
    /// `owned` cannot answer this for a Plex Home managed profile.
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
        // **The Plex Home roster is the one input to this gate that is not in the registry.**
        // `/api/v2/home/users` lands on its own schedule and changes no `ServerFacts` at all, so
        // every other test below is blind to it — and it is the answer that decides whether a
        // managed profile's family server is the household's or a stranger's. Without this the
        // sync it guards was simply never reached on a roster arrival, and the source stayed
        // misgraded until something else happened to move a fact. `peek()` is a lock and an `Arc`
        // clone over a live cache (`plex/CLAUDE.md`), not a file read, which is what makes it
        // affordable on a per-frame gate.
        let household = crate::plex::session::peek().household_ids();
        for source in &self.sources {
            if source.household != household_verdict(source, &household) {
                return true;
            }
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
                    || source.handle != facts.handle || source.owned != facts.owned
                    || source.home != facts.home || source.owner_id != facts.owner_id {
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
                        extensions: Default::default(),
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
        let mut touched = vec![false; self.sections.len()];
        touched[index] = true;
        self.record_pins(true, &touched);
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
    /// **Where a tab press lands for a content type**, when the profile has not already chosen.
    ///
    /// [`remembered_section`](Self::remembered_section) wins first and unconditionally — a person
    /// who picked a library from the Sources panel gets that library back, and no rule here may
    /// second-guess it. What follows is only the tiebreak among the *pinned* libraries of the
    /// type, and it prefers the HOUSEHOLD's over an outsider's.
    ///
    /// The tiebreak is issue #68's mechanism, one household wider. 0.6.x's `tab_section` sorted on
    /// raw `!owned` with no `pinned` filter at all, so two equally-graded libraries tied and the
    /// section table's arrival order decided permanently — *"I have two TV Shows libraries … only
    /// my Animes are being displayed"*. 0.7's `pinned` filter and the remembered choice closed that
    /// for an owner. They did not close it for a Plex Home managed profile, because plex.tv grades
    /// that profile's own family server `owned:false` exactly like a friend's share: NOTHING was
    /// owned, so everything tied again and the tab could land on a stranger's shelf. Grading on
    /// the household is what makes the tiebreak able to separate them.
    fn section_of_kind(&self, kind: SecKind) -> Option<usize> {
        if let Some(section) = self.remembered_section(kind) {
            return Some(section);
        }
        self.sections.iter().enumerate()
            .filter(|(_, section)| section.kind == kind && section.pinned)
            .min_by_key(|(_, section)| {
                !self.sources.get(section.src).map(|source| source.household).unwrap_or(false)
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
    /// The pin rules' view of the section table.
    ///
    /// **Both bits are the HOUSEHOLD's, not this account's.** `plex::pins` is a pure leaf that
    /// takes bools, so the whole of `is_household` — the grant, plex.tv's `home` flag and the Plex
    /// Home roster — is resolved on this side and handed down already decided
    /// ([`BrowseSource::household`]). Reading `owned` here is what gave a managed profile's own
    /// family server a stranger's defaults: plex.tv answers such a profile `owned:false` on it,
    /// and `owns_type` then found nothing of the household's either, so EVERY library defaulted On
    /// — a genuine friend's share included.
    fn lib_refs(&self) -> Vec<crate::plex::pins::LibRef<'_>> {
        let household_type = |kind: SecKind| self.sections.iter().any(|section| {
            section.kind == kind
                && self.sources.get(section.src).map(|source| source.household).unwrap_or(true)
        });
        self.sections.iter().map(|section| {
            let (machine_id, household) = self.sources.get(section.src)
                .map(|source| (source.machine_id.as_str(), source.household)).unwrap_or(("", true));
            crate::plex::pins::LibRef {
                machine_id, key: section.key, household,
                household_type: household_type(section.kind),
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
    fn resolve_pins_from(&mut self, session: &crate::plex::session::Session, user: &str) -> bool {
        self.load_remembered(session, user);
        let record = session.pins_for(user).cloned();
        self.resolve_pins_with(record)
    }
    /// Re-derive every row from `record` and the never-empty floor, and report whether the table
    /// actually MOVED — `pins::resolve` is a whole-table function (its own doc says why the floor
    /// cannot be decided per row), so this is the only shape a re-resolve comes in.
    fn resolve_pins_with(&mut self, record: Option<crate::plex::session::HomePins>) -> bool {
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
        moved
    }
    /// **The whole-table reconcile, plus the publication a moved row owes** — the one path for
    /// "something happened that can invalidate an already-resolved table".
    ///
    /// Two callers reach it, and they are the two ways that can happen: a Plex Home roster landing
    /// reclassifies a SOURCE (`sync_roster_owned`), and the Home editor's commit replaces this
    /// profile's RECORD (`apply_pins`). Both end in the same question — what does
    /// `pins::resolve` say about every row now — so they ask it the same way rather than each
    /// keeping a version of the answer. A row that moves without `bump_sections_gen` +
    /// `ui::idle::invalidate` is a table nothing republishes, which is a pill strip and a set of
    /// shelves still drawing the previous resolve.
    fn reconcile_pins(&mut self, record: Option<crate::plex::session::HomePins>) -> bool {
        let moved = self.resolve_pins_with(record);
        if moved {
            self.bump_sections_gen();
            crate::ui::idle::invalidate();
        }
        moved
    }
    /// [`reconcile_pins`](Self::reconcile_pins) against the record a session holds.
    fn reconcile_pins_from(
        &mut self,
        session: &crate::plex::session::Session,
        user: &str,
    ) -> bool {
        self.load_remembered(session, user);
        self.reconcile_pins(session.pins_for(user).cloned())
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
    /// Write this profile's answer down. `touched` is indexed like the section table and marks
    /// the rows the VIEWER answered — as the editor reported them, never as a comparison against
    /// the live pins; everything else keeps whatever it had already answered and is otherwise
    /// left unrecorded, to go on re-deriving (`plex::pins::answers`).
    ///
    /// **The derivation of `answers` happens INSIDE `update`**, against the record that write is
    /// about to replace — the same reason `carry_forward` is already computed there. Reading the
    /// previous answer off `self.recorded` would read a copy taken at the last resolve, and a
    /// concurrent writer between those two points would have its answer dropped rather than
    /// merged, which is precisely the lost update `session::update` exists to prevent.
    fn record_pins(&mut self, asked: bool, touched: &[bool]) {
        let libraries = self.lib_refs();
        let on: Vec<bool> = self.sections.iter().map(|section| section.pinned).collect();
        let user = crate::plex::session::current_profile_key();
        let mut written = None;
        crate::plex::session::update(|session| {
            let previous = session.pins_for(&user);
            let answers = crate::plex::pins::answers(&libraries, &on, touched, previous);
            let fresh = crate::plex::pins::record(&user, asked, &libraries, &answers);
            let record = crate::plex::pins::carry_forward(fresh, previous, &libraries);
            let mut next = session.clone();
            next.set_pins_for(&user, record.clone());
            written = Some(record);
            Some(next)
        });
        if let Some(record) = written {
            self.recorded = Some(record);
        }
    }
    /// The editor's one commit: apply the rows it ANSWERED, and record exactly those.
    ///
    /// Recording the rest would freeze defaults nobody chose; `plex::pins::answers` has the
    /// argument.
    ///
    /// **The provenance arrives with the command; it is not reconstructed here.** This used to
    /// take the whole visible draft and infer "the viewer moved this row" from "it disagrees with
    /// the live pin" — which cannot see the one case that matters: a row the viewer toggled to a
    /// value a roster correction then moved the LIVE pin to as well, before the commit, arrives
    /// agreeing with the table and reads as untouched. Left unrecorded it goes on re-deriving, so
    /// the day the default moves again (the household loses its last library of that type) an
    /// explicit Off comes back On with nothing to appeal to. `screens::onboard` already computes
    /// `draft != entry_pins` for `dirty`, and that IS the provenance, so it sends it.
    ///
    /// An answer that agrees with the live pin therefore still moves nothing on screen and is
    /// still written down; an empty batch is still a commit (`asked`, no answers).
    ///
    /// **And the table is reconciled with the record afterwards**, through the same
    /// [`reconcile_pins`](Self::reconcile_pins) a reclassification uses. Recording only the
    /// answered rows means the unanswered ones keep re-deriving — including one the never-empty
    /// floor had RAISED, which would otherwise keep its raised value on screen while the record
    /// says otherwise, and go back down at the next resolve with no user action behind it.
    fn apply_pins(&mut self, answers: &[(usize, bool)]) {
        let mut touched = vec![false; self.sections.len()];
        let mut changed = false;
        for &(index, on) in answers {
            let Some(section) = self.sections.get_mut(index) else {
                continue;
            };
            if section.pinned != on {
                section.pinned = on;
                changed = true;
            }
            if let Some(slot) = touched.get_mut(index) {
                *slot = true;
            }
        }
        self.record_pins(true, &touched);
        if changed {
            self.repoint_cur();
            self.bump_sections_gen();
            crate::ui::idle::invalidate();
        }
        // The record `record_pins` just wrote, not a re-read of the session: it is the authority
        // that write produced (merged under `session::update`'s own lock), and a re-read could
        // answer from before it on a write the keymanager refused.
        let record = self.recorded.clone();
        self.reconcile_pins(record);
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
        // The Plex Home roster, read ONCE for the whole sync: `peek()` is a write-through cache
        // over the persisted session (`plex/CLAUDE.md`), so this is a lock and an `Arc` clone
        // rather than a file read — but it is still per sync and not per source.
        let session = crate::plex::session::peek();
        let household = session.household_ids();
        let retire_adapter = self.sources.iter().any(|source| !live.contains(&source.sid));
        let mut changed = retire_adapter;
        // Did any source change SIDE of the household line in this pass? That, and not a changed
        // `ServerFacts`, is what the pin defaults and the tab destination are derived from.
        let mut reclassified = false;
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
                        if source.handle != facts.handle || source.owned != facts.owned
                            || source.home != facts.home || source.owner_id != facts.owner_id {
                            source.handle = facts.handle.clone();
                            source.owned = facts.owned;
                            source.home = facts.home;
                            source.owner_id = facts.owner_id;
                            changes += 1;
                        }
                    }
                    // Derived AFTER the evidence above lands, and unconditionally: the roster this
                    // is graded against arrives on its own schedule, so an unchanged `ServerFacts`
                    // does not mean an unchanged verdict.
                    let household_now = household_verdict(source, &household);
                    if source.household != household_now {
                        source.household = household_now;
                        reclassified = true;
                        changes += 1;
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
                    // An undescribed slot is the session's own server (`servers::describe_name`
                    // says why that is not a guess), so it carries no third-party evidence.
                    let home = facts.is_some_and(|facts| facts.home);
                    let owner_id = facts.map_or(0, |facts| facts.owner_id);
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
                        owned, home, owner_id,
                        household: crate::plex::is_household(
                            crate::plex::GrantEvidence { owned, home, owner_id }.grant(),
                            &household,
                        ),
                        name, handle, state, tier,
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
        if reclassified {
            // **A source changed sides, so the WHOLE pin table is re-resolved** — `pins::resolve`
            // is a whole-table function on purpose (its own doc: the never-empty floor is a
            // question about the table, not about a row), and one source's reclassification moves
            // `household_type` for every library of its kind on every other source too.
            //
            // Only the rows nobody has answered about actually move: a recorded answer beats the
            // default in both directions, which is exactly what makes a late roster able to
            // correct a default it arrived too late to inform and unable to overrule a decision.
            // `reconcile_pins_from`'s `bump_sections_gen` republishes through the same generation
            // a pin toggle does, and carries the tab strip's pill mask with it; the tab
            // DESTINATION is derived live by `section_of_kind`, so it follows the same bump
            // without a cache of its own. It bumps only when a row actually MOVED — a
            // reclassification that moves none publishes nothing new to the section table, and
            // the source facts it did move have their own generation, bumped above.
            self.reconcile_pins_from(&session, &crate::plex::session::current_profile_key());
            changed = true;
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
                            let kind = self.section_kind(result.sec);
                            if let Some(state) = self.state_mut(result.sec) {
                                state.fetch = SecFetch::Ready;
                                if let Some(sorts) = result.sorts {
                                    if state.sorts.is_empty() {
                                        let sorts = match kind {
                                            Some(kind) => with_plays_sort(sorts, kind),
                                            None => sorts,
                                        };
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

/// PMS's own sort key for play count — never advertised by `includeMeta=1` (measured against
/// PMS 1.43.4, on movie and show sections alike) but honoured by the server on both when sent
/// directly, sorting movies by their view count and shows by their own. An unrecognised key
/// 500s the whole listing, which is why [`with_plays_sort`] never sends this for a kind that
/// hasn't been proven to accept it.
const PLAYS_SORT_KEY: &str = "viewCount";

/// Whether the server is proven to honour [`PLAYS_SORT_KEY`] for this section kind. An
/// exhaustive match rather than a wildcard default: [`SecKind`]'s own doc says a third variant
/// (`Artist`/`Photo`) belongs in "the commit that builds its level", and when that lands this
/// match must fail to compile until someone decides whether the new kind belongs here too,
/// rather than silently inheriting `true`.
fn kind_offers_plays_sort(kind: SecKind) -> bool {
    match kind {
        SecKind::Movie | SecKind::Show => true,
    }
}

/// Append the client-side "Plays" sort where the server is proven to honour it, unless it is
/// already in the advertised list (a future PMS that starts advertising `viewCount` itself
/// must not produce two rows). Descending by default — most-played first, the direction the
/// feature exists for.
fn with_plays_sort(mut sorts: Vec<SortEntry>, kind: SecKind) -> Vec<SortEntry> {
    if kind_offers_plays_sort(kind) && !sorts.iter().any(|sort| sort.key == PLAYS_SORT_KEY) {
        sorts.push(SortEntry {
            key: PLAYS_SORT_KEY.into(),
            title: "Plays".into(),
            default_desc: true,
        });
    }
    sorts
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
// Source rides inside the library pill's own label instead, so adding people never reshapes this strip.
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
        !matches!(self.state, SourceState::Unreachable | SourceState::InsecureOnly)
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
            home: false,
            owner_id: 0,
            household: index == 0,
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
        home: false,
        owner_id: 0,
        household: true,
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
        test_support::a_source("mac-mini", "", true),
        test_support::a_source("nas-home", "friend", true),
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
        crate::plex::describe_server(sid, &source.name, &source.handle,
            crate::plex::GrantEvidence {
                owned: source.owned, home: source.home, owner_id: source.owner_id,
            });
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
#[path = "browse_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "browse_table_tests.rs"]
mod table_tests;

#[cfg(test)]
#[path = "browse_discovery_lifecycle_tests.rs"]
mod discovery_lifecycle_tests;

#[cfg(test)]
#[path = "browse_fetch_state_tests.rs"]
mod fetch_state_tests;

#[cfg(test)]
#[path = "browse_home_and_tabs_tests.rs"]
mod home_and_tabs_tests;

#[cfg(test)]
#[path = "browse_reachability_tests.rs"]
mod reachability_tests;
