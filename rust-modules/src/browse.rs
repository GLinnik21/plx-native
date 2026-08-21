//! browse — the Library screen's per-section paged catalog.
//!
//! Sibling of `pms.rs`'s hub catalog, which stays hub-only (256-cap, rebuilt wholesale by
//! `pms_fetch_hubs`); this store pages arbitrarily large sections without ever blocking the
//! main loop. Data model: one sparse `Vec<Option<PmsMovie>>` per section, sized to the
//! listing's `totalSize`, filled page-by-page (`PAGE` items) by ONE background fetch at a
//! time using the season-switch idiom from `metadata.rs` — [`crate::task::spawn_small`] + a
//! `Mutex` mailbox + generation atomics (a re-query supersedes in-flight landings), applied
//! on the main thread by [`pump`] once a frame while the Library screen is up.
//!
//! The sort/filter MENUS are server-driven: the first page of a section is requested with
//! `includeMeta=1` and the response's `Meta.Type[]` supplies the Sort entries; the genre
//! value list is fetched lazily (`kick_genres`) when the filter menu first opens. Nothing
//! menu-shaped is hardcoded — a music section would bring its own sorts.
//!
//! All statics are main-thread-only (same discipline as `pms.rs`); the worker threads touch
//! only the mailboxes + atomics and the `&'static` plex client.
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
//! `ensure_sections` early-return was the only thing preventing that; appending is the property
//! that replaces it, and it holds for every source that lands later rather than only for the
//! second call.
use crate::plex::{SectionQuery, ServerId};
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

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

/// One SOURCE the table is addressed by — a server this account has been granted. Comes from the
/// [server registry](crate::plex::server_ids), which is the granted roster: a server is registered
/// only once plex.tv (or the `plxnative-servers` dev trigger) handed us a token for it.
pub(crate) struct BrowseSource {
    /// The registry slot every fetch for this source's sections is issued through.
    pub(crate) sid: ServerId,
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
    /// Did it ANSWER? One of the design's three orthogonal states — *granted* (the roster's
    /// answer), *pinned* (the only control), *reachable* (a fact about now). A source that has
    /// stopped answering keeps every section it had learned and every pin on them: its group dims
    /// whole and still reads `On`, because nothing was unpinned. Hiding it would read as a
    /// revoked share.
    pub(crate) reachable: bool,
    /// its `/library/sections` has landed — sections are appended exactly once per source
    sections_done: bool,
    /// its per-library item counts have landed (the row sub-line's "185 films")
    counts_done: bool,
    /// frames before the next discovery attempt after a failure (main-thread; [`pump`] counts down)
    retry_cd: u32,
}

// ---- section table (discovered per source) ---------------------------------------------------

/// One browsable library section (movie or show), from one source's `GET /library/sections`.
pub(crate) struct BrowseSection {
    /// index into [`SOURCES`] — the server half of this row's address. A bare `key` names two
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
    /// Does this library feed **Home**? The design's one control. It governs Home and nothing
    /// else: tabs, grid, sort and the A–Z rail all come from the GRANT, which is not a setting.
    /// Your own libraries start pinned and a friend's start unpinned, which is the first-run state
    /// the design specifies; the last pinned library cannot be turned off, or Home has nothing.
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
/// state and the store TOGETHER (`ui::library`'s `readout_of`, where that whole decision lives as
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

/// Per-section browse state: the current query, the server-driven menus, the sparse item
/// store, and the remembered view (focus/scroll survive leaving the screen — state amnesia
/// is the official app's loudest complaint).
struct SecState {
    // query
    sort_idx: usize,
    sort_desc: bool,
    unwatched: bool,
    genre: Option<GenreEntry>,
    // menus (kept across re-queries)
    sorts: Vec<SortEntry>,
    genres: Vec<GenreEntry>,
    genres_done: bool, // a genre fetch LANDED (even empty) — kick_genres won't re-spawn
    /// per-letter (label, count) in titleSort order, from `/firstCharacter` — the letter rail.
    /// Counts describe the UNFILTERED title listing, so the rail only shows in that state.
    letters: Vec<(String, i64)>,
    letters_done: bool,
    // data
    fetch: SecFetch, // what the last page fetch for this section did
    total: i64,      // -1 = unknown (first fetch of this query still out)
    items: Vec<Option<PmsMovie>>,
    // remembered view
    focus: usize,
    scroll: f32,
}

impl Default for SecState {
    fn default() -> Self {
        SecState {
            sort_idx: 0,
            sort_desc: false,
            unwatched: false,
            genre: None,
            sorts: Vec::new(),
            genres: Vec::new(),
            genres_done: false,
            letters: Vec::new(),
            letters_done: false,
            fetch: SecFetch::Loading,
            total: -1,
            items: Vec::new(),
            focus: 0,
            scroll: 0.0,
        }
    }
}

static mut SOURCES: Vec<BrowseSource> = Vec::new();
static mut SECTIONS: Vec<BrowseSection> = Vec::new();
static mut STATES: Vec<SecState> = Vec::new();
static mut CUR: usize = 0;
/// Wanted item-index range (inclusive lo, exclusive hi) — set by the grid each frame from its
/// visible rows + lookahead; [`pump`] fetches the first missing page inside it.
static mut WANT: (usize, usize) = (0, 0);

fn sections() -> &'static Vec<BrowseSection> {
    unsafe { &*addr_of!(SECTIONS) }
}
/// The granted roster, in registration order — the session's own server first.
pub(crate) fn sources() -> &'static [BrowseSource] {
    unsafe { &*addr_of!(SOURCES) }
}
fn source_mut(i: usize) -> Option<&'static mut BrowseSource> {
    unsafe { (&mut *addr_of_mut!(SOURCES)).get_mut(i) }
}
fn states() -> &'static Vec<SecState> {
    unsafe { &*addr_of!(STATES) }
}
fn state_mut(i: usize) -> Option<&'static mut SecState> {
    unsafe { (&mut *addr_of_mut!(STATES)).get_mut(i) }
}
fn cur_state() -> Option<&'static SecState> {
    states().get(cur())
}

// ---- fetch plumbing (generation + single-flight + mailboxes) --------------------------------

static GEN: AtomicU32 = AtomicU32::new(0);
/// Bumped whenever the section table's SHAPE changes — a source's sections appended, or the whole
/// table wiped by [`reset`]. Label/measurement caches keyed on the table (the tab strip's pill
/// widths, the rail's letters) invalidate on it. Because the table only ever GROWS, a cache keyed
/// on this is complete: no existing entry can have changed under it.
static SECTIONS_GEN: AtomicU32 = AtomicU32::new(0);
/// The table's IDENTITY epoch — bumped by [`reset`] and by nothing else, i.e. exactly when the
/// signed-in account changes and every index in the table stops meaning what it meant.
///
/// Landings blamed on a section INDEX gate on this rather than on [`SECTIONS_GEN`]: an APPEND from
/// one source must not discard a landing in flight for another, and it cannot invalidate one
/// either, because appending never moves an existing index.
static EPOCH: AtomicU32 = AtomicU32::new(0);
static FETCHING: AtomicBool = AtomicBool::new(false);
static GENRE_FETCHING: AtomicBool = AtomicBool::new(false);
static LETTERS_FETCHING: AtomicBool = AtomicBool::new(false);
static SRC_FETCHING: AtomicBool = AtomicBool::new(false);
/// Every single-flight flag, in one place. These are cleared ONLY inside a successful mailbox
/// take, so [`reset`] — which drops the mailboxes — must clear them too or the fetch stays
/// latched forever and the screen wedges on a spinner. **Add a new flag here, not just above**,
/// and `reset` picks it up for free.
const IN_FLIGHT: [&AtomicBool; 4] = [&FETCHING, &GENRE_FETCHING, &LETTERS_FETCHING, &SRC_FETCHING];
/// Frames left before another page fetch may spawn after a FAILED one (main-thread; pump
/// decrements). Stops a fast-failing network from spawning a worker per frame.
static mut RETRY_CD: u32 = 0;

struct PageResult {
    gen: u32,
    sec: usize,
    start: usize,
    items: Vec<PmsMovie>,
    /// totalSize of the listing; **negative = the fetch FAILED** — pump must not touch the
    /// store (a transient network error once wiped a whole populated section to "empty").
    total: i64,
    sorts: Option<Vec<SortEntry>>, // Some when the fetch carried includeMeta=1
}
static PAGE_RESULT: Mutex<Option<PageResult>> = Mutex::new(None);
// menu-data landings carry the table EPOCH so a landing spawned before a [`reset`] (profile
// switch) can never populate the NEW user's state at the same index
static GENRE_RESULT: Mutex<Option<(u32, usize, Vec<GenreEntry>)>> = Mutex::new(None);
static LETTER_RESULT: Mutex<Option<(u32, usize, Vec<(String, i64)>)>> = Mutex::new(None);

/// What a source-discovery worker brings back, per SOURCE — named by its index, which appending
/// can never move.
///
/// `name` rides EVERY landing rather than only the section one, because the current server's
/// sections are discovered on the main thread ([`ensure_sections`]) and so never reach a worker at
/// that phase; without this its group header would be the one blank line in the panel.
struct SrcLanding {
    /// `GET /`'s `friendlyName`, or "" when it was already known or the server did not answer
    name: String,
    what: SrcWhat,
}
enum SrcWhat {
    /// `GET /library/sections`. `None` is the FAILURE sentinel — the source is marked unreachable
    /// and whatever sections it had already contributed are left exactly where they are.
    Sections(Option<Vec<(i64, String, SecKind)>>),
    /// The unfiltered item count per library, **by section KEY** rather than by index: the table
    /// may have grown between the spawn and the landing, and a key is stable inside one source.
    Counts(Vec<(i64, i64)>),
}
static SRC_RESULT: Mutex<Option<(u32, usize, SrcLanding)>> = Mutex::new(None);

/// Supersede everything in flight for the CURRENT query (sort/filter/section change): a late
/// landing with an older generation is discarded by [`pump`].
fn bump_gen() -> u32 {
    GEN.fetch_add(1, Ordering::SeqCst) + 1
}

/// Reset the current section's item store for a changed query (menus + remembered view keep).
fn requery() {
    bump_gen();
    if let Some(st) = state_mut(cur()) {
        // a replaced query has no answer yet, whatever the last one produced — including a
        // failure, which must not outlive the query it belonged to
        st.fetch = SecFetch::Loading;
        st.total = -1;
        st.items.clear();
        st.focus = 0;
        st.scroll = 0.0;
    }
}

// ---- public surface: sections ---------------------------------------------------------------

/// Wipe the whole store (sections, states, caches) and supersede everything in flight.
/// Called on every `install_pms` — a profile/account switch must never show the previous
/// user's cached grid, watched-state angles, or section tabs (pms.rs's hub catalog is
/// rebuilt wholesale on the same event; this is the browse twin).
pub(crate) fn reset() {
    bump_gen();
    SECTIONS_GEN.fetch_add(1, Ordering::SeqCst);
    EPOCH.fetch_add(1, Ordering::SeqCst);
    *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *GENRE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *LETTER_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // Dropping a mailbox without clearing its flag latches the fetch forever (the flag is only
    // cleared on a successful take), so the two must move together.
    for f in IN_FLIGHT {
        f.store(false, Ordering::SeqCst);
    }
    unsafe {
        *addr_of_mut!(SOURCES) = Vec::new();
        *addr_of_mut!(SECTIONS) = Vec::new();
        *addr_of_mut!(STATES) = Vec::new();
        // The Home selection belongs to ONE profile, and this runs on every `install_pms` — i.e.
        // on the switch. Carrying it would let the previous person's answer govern the next
        // person's Home for the frames before their own record is read (see [`RECORDED`]).
        *addr_of_mut!(RECORDED) = None;
        // TABS is deliberately NOT cleared here, only invalidated. It is a memo, and the ONE thing
        // that decides whether the strip re-measures is [`tabs`] comparing the old row against the
        // new one — so emptying it in advance makes that comparison `[] != []`, which is false, and
        // the label cache keeps the PREVIOUS ACCOUNT's pills: `draw_tab_row` iterates the cache, so
        // after a profile switch the strip would go on drawing and hit-testing libraries the new
        // user cannot open, until some later landing happened to change the row.
        TABS_GEN = u32::MAX;
        CUR = 0;
        RETRY_CD = 0;
    }
}

/// The section-table SHAPE generation — bumped by [`reset`] and by every append; the tab-row label
/// cache and the rail's letter cache key on it.
pub(crate) fn sections_gen() -> u32 {
    SECTIONS_GEN.load(Ordering::SeqCst)
}

/// Bumped when a source FACT the Sources list states changes without the table's shape moving: a
/// machine name learned off the server itself, a library's item count landing, a source flipping
/// reachable. Deliberately not [`SECTIONS_GEN`], whose documented meaning is the SHAPE and whose
/// readers (the tab-strip pill widths, the A–Z rail's letters) rely on "the table only ever grows,
/// so a cache keyed on this is complete".
static SRC_FACTS_GEN: AtomicU32 = AtomicU32::new(0);

/// **The generation of everything the Sources list DRAWS** — the table's shape plus the facts its
/// rows state. The number both surfaces that draw that list watch (`ui::library`'s panel and
/// `ui::onboard`'s route), because a surface keyed on the SHAPE alone goes on saying "Films" long
/// after the count arrived and heads an unnamed group with no header at all — which is precisely
/// what both did.
///
/// A SUM of two monotone counters, so it is itself strictly monotone: any bump to either moves it
/// by one, and there is no pair of states that can alias.
pub(crate) fn source_list_gen() -> u32 {
    SECTIONS_GEN.load(Ordering::SeqCst).wrapping_add(SRC_FACTS_GEN.load(Ordering::SeqCst))
}

/// The QUERY generation — the content epoch. [`bump_gen`] moves it from exactly three places
/// ([`requery`], [`reset`], [`set_cur`]), i.e. precisely when the item set is REPLACED, and never
/// on a scroll / letter jump / focus move / page landing. The Library screen watches it so a store
/// wiped from underneath it (a profile switch calling [`reset`]) is cross-faded like any other
/// reload instead of cut.
pub(crate) fn query_gen() -> u32 {
    GEN.load(Ordering::SeqCst)
}

/// Adopt every server the registry holds that the table does not — the granted roster, appended
/// in registration order so the session's own server is source 0. Cheap enough for every Library
/// entry: it touches only the slots it has not seen.
///
/// Facts are re-read on every call rather than copied once: the roster ingest and the server's own
/// `friendlyName` land at different times, in either order, and a source whose header was blank
/// when it was adopted must be able to fill in later.
fn sync_roster() {
    let known = sources().len();
    for sid in crate::plex::server_ids() {
        // matched on the SLOT, not on position: the roster and this table happen to be appended in
        // the same order today, and that is an invariant nobody outside this loop would know to
        // keep. The list is a handful of servers, so the scan costs nothing.
        let at = sources().iter().position(|s| s.sid == sid);
        match at.and_then(source_mut) {
            // Steady state — every frame the Library is up — allocates NOTHING: a field is only
            // read (and cloned) while it is still unknown. A roster that later RENAMES a server we
            // already have a name for is deliberately not followed: a machine name changing under
            // an open panel is churn, not news.
            Some(s) => {
                if s.name.is_empty() || s.handle.is_empty() {
                    if let Some(f) = crate::plex::server_facts(sid) {
                        if s.name.is_empty() {
                            s.name = f.name.clone();
                        }
                        if s.handle.is_empty() {
                            s.handle = f.handle.clone();
                        }
                        // `owned` follows the roster's answer whenever one lands, unlike the two
                        // names above: it is not a display string that would churn under an open
                        // panel but the input to which libraries feed Home, and a source adopted
                        // before plex.tv described it is optimistically OURS (see the `None` arm).
                        s.owned = f.owned;
                    }
                }
                // The machine id is learned LATE on the one path that matters: a stored session
                // registers the primary by address (`plex::install`) before the roster names it.
                // Once known it never changes — the registry keys on it — so this is a fill, not a
                // follow.
                if s.machine_id.is_empty() {
                    s.machine_id = machine_of(sid);
                }
            }
            None => unsafe {
                let f = crate::plex::server_facts(sid);
                // Optimistically OURS, for [`BrowseSource::reachable`]'s reason one field down: a
                // source plex.tv has not described yet is not a source known to be somebody
                // else's, and defaulting it to a share would take your own libraries off Home for
                // the frames between registration and the roster landing.
                let owned = f.map(|f| f.owned).unwrap_or(true);
                let (name, handle) = f.map(|f| (f.name.clone(), f.handle.clone())).unwrap_or_default();
                (*addr_of_mut!(SOURCES)).push(BrowseSource {
                    sid,
                    machine_id: machine_of(sid),
                    owned,
                    name,
                    handle,
                    // Optimistic until proven otherwise: a source nobody has dialled yet is not a
                    // source that failed, and the whole group would otherwise open dimmed.
                    reachable: true,
                    sections_done: false,
                    counts_done: false,
                    retry_cd: 0,
                });
            },
        }
    }
    if sources().len() != known {
        crate::log(&format!("browse: roster now {} source(s)", sources().len()));
    }
}

/// Discover the CURRENT server's movie/show sections (once; blocking — one small GET, the same
/// boot-fetch budget as `pms_fetch_hubs`). Safe to call every Library entry; later calls are free.
/// Returns the table's size.
///
/// **Only the current server is fetched here, and that is the point.** This runs on the main
/// thread at boot and on every Library entry, so fanning out over the roster would park the SDL
/// loop for one `connect(2)` timeout per unreachable share — seconds of frozen boot for a friend
/// who switched their server off. Every OTHER source is discovered by [`pump`] on a worker
/// ([`maybe_discover`]), which costs the main thread nothing and lets a dead share simply arrive
/// as `reachable: false`.
pub(crate) fn ensure_sections() -> usize {
    sync_roster();
    let cur_sid = crate::plex::current_server();
    let Some(si) = sources().iter().position(|s| s.sid == cur_sid) else {
        return sections().len();
    };
    if sources()[si].sections_done {
        return sections().len();
    }
    let found = catch_unwind(|| {
        crate::plex::client_for(cur_sid).and_then(|c| c.sections()).map(|mc| project_sections(&mc))
    })
    .unwrap_or(None);
    let ok = found.is_some();
    append_sections(si, found.unwrap_or_default());
    if let Some(s) = source_mut(si) {
        s.sections_done = ok;
        s.reachable = ok;
        s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
    }
    sections().len()
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
            d.key.parse::<i64>().ok().map(|k| (k, d.title.clone(), kind))
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

/// APPEND one source's sections to the table, with their per-section states in lockstep.
///
/// Append, never rebuild — see this module's header. Existing indices (and therefore `CUR`, every
/// remembered view, and every in-flight page landing's `sec`) survive untouched, which is what
/// makes a source arriving late safe at all. A source is only ever appended once, so a repeat call
/// for it is a no-op rather than a duplicated library.
fn append_sections(src: usize, list: Vec<(i64, String, SecKind)>) {
    // Only what this source does not already have. A re-discovery ("Check for new shares", or a
    // server that came back) therefore ADDS a library the owner has since created and leaves every
    // existing row — and every index — exactly where it was.
    let fresh: Vec<(i64, String, SecKind)> = list
        .into_iter()
        .filter(|(k, _, _)| !sections().iter().any(|s| s.src == src && s.key == *k))
        .collect();
    if fresh.is_empty() {
        return;
    }
    // Pushed UNSET, then resolved with the whole table below. **The default lives in
    // `plex::pins`, not here**, and it is per-PROFILE: this line was a bare `let pinned = true`
    // for as long as deliverable F (the first-run route) had nowhere to ask the question — a
    // deliberate stand-in, because "your own On, a friend's Off" without a screen to say so is a
    // share that is granted, discovered, browsable and silently absent from Home with no visible
    // control anywhere to turn it on. That screen exists now, so the real rule applies.
    unsafe {
        let secs = &mut *addr_of_mut!(SECTIONS);
        let states = &mut *addr_of_mut!(STATES);
        for (key, title, kind) in fresh {
            secs.push(BrowseSection { src, key, title, kind, count: -1, pinned: false });
            states.push(SecState::default());
        }
    }
    resolve_pins();
    SECTIONS_GEN.fetch_add(1, Ordering::SeqCst);
    crate::ui::idle::invalidate(); // a new tab pill / Sources row appears under a settled screen
}

pub(crate) fn section_count() -> usize {
    sections().len()
}
/// The LIBRARY titles this source contributed, in table order — what a source is called in
/// CONTENT, as opposed to [`BrowseSource::name`], which is its machine and belongs in the Sources
/// list. A friend's server is experienced as the library it gave you ("Film Club"), never as a
/// hostname; `Search Screen.dc.html`'s scope line names it that way, and the app's own rule is
/// "people in content, machines in settings".
///
/// Empty while the section table is still landing, which the caller must read as "not yet known"
/// rather than "none" — the same trap `Session::pinned` documents.
pub(crate) fn library_titles(sid: ServerId) -> Vec<&'static str> {
    let srcs = sources();
    sections()
        .iter()
        .filter(|s| srcs.get(s.src).map(|c| c.sid) == Some(sid))
        .map(|s| s.title.as_str())
        .collect()
}
pub(crate) fn section_title(i: usize) -> &'static str {
    sections().get(i).map(|s| s.title.as_str()).unwrap_or("")
}
/// The TYPE of section `i` — `None` for an index the table does not hold.
pub(crate) fn section_kind(i: usize) -> Option<SecKind> {
    sections().get(i).map(|s| s.kind)
}
/// The registry slot section `i` is browsed through. **Read this at the SPAWN SITE**; a worker
/// that calls `client()` instead dials whichever server happens to be current when it runs.
fn section_sid(i: usize) -> Option<ServerId> {
    let s = sections().get(i)?;
    sources().get(s.src).map(|src| src.sid)
}
pub(crate) fn cur() -> usize {
    unsafe { CUR }.min(sections().len().saturating_sub(1))
}
/// The handle of section `i`'s owner — `""` on your own libraries, where the annotation is absent
/// rather than empty. The Source chip's dim trailing run.
///
/// Takes the section rather than reading `cur()`, because the chip must relabel on the PRESS frame:
/// its name comes from the queued section (`view_section`) and a handle resolved from the committed
/// one would pop in 70 ms later, changing the chip's measured width mid-fade.
pub(crate) fn handle_of(i: usize) -> &'static str {
    sections().get(i).and_then(|s| sources().get(s.src)).map(|s| s.handle.as_str()).unwrap_or("")
}
/// Switch section tab. Keeps the target section's remembered query/view; only re-fetches
/// when its store is empty.
pub(crate) fn set_cur(i: usize) {
    if i >= sections().len() || i == cur() {
        return;
    }
    unsafe { CUR = i };
    bump_gen(); // discard any in-flight page for the previous section
    activate_source_of(i);
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
/// Record what a request to section `i`'s server just proved about it. See the call in [`pump`].
fn mark_source_reachable(i: usize, ok: bool) {
    let Some(src) = sections().get(i).map(|s| s.src) else { return };
    let Some(s) = source_mut(src) else { return };
    if s.reachable == ok {
        return;
    }
    s.reachable = ok;
    // A server that has come back is worth re-asking properly (its library list may have moved on);
    // one that has gone means the Sources list must dim its group NOW rather than at the next press.
    if ok {
        s.retry_cd = 0;
    }
    crate::ui::idle::invalidate();
}

// ---- the TAB projection: which sections get a pill in the shared top strip -------------------
//
// **A pill is a TYPE, never a person** (the design's deliverable B). Source lives in the toolbar
// chip one line below, so the strip names your own libraries and nothing else — with one exception
// that costs nothing: a friend sharing a type you do not own (they have shows, you don't) DOES add
// a pill, because otherwise that content is unreachable from the strip at all.
//
// The consequence is the property B was written for: the strip is a constant width at one friend
// or at ten. Put source in the strip instead and three friends measure 2133px against a 1540 track.
//
// With one source this is the identity map, so a single-server install draws exactly the strip it
// always did.

/// tab index → section index, rebuilt when the table's shape moves.
static mut TABS: Vec<usize> = Vec::new();
static mut TABS_GEN: u32 = u32::MAX;
/// …and how many times the projection above actually CHANGED, which is a different question from
/// how many times it was re-derived. See [`tabs_gen`].
static mut TABS_SHAPE_GEN: u32 = 0;

fn tabs() -> &'static Vec<usize> {
    let g = sections_gen();
    if unsafe { addr_of!(TABS_GEN).read() } != g {
        // compared by real TYPE, not by `is_show`: with a boolean, "does an owned library have this
        // kind" answers YES for a friend's MUSIC library on the strength of your films, so it would
        // get no pill and nothing in it could be reached from the strip at all
        let owned_kinds: Vec<SecKind> = sections()
            .iter()
            .filter(|s| sources().get(s.src).map(|x| x.handle.is_empty()).unwrap_or(true))
            .map(|s| s.kind)
            .collect();
        // A missing type earns ONE pill, not one per borrowed library of it. Two friends who both
        // share Music are two libraries of a type you do not own, and admitting both puts two
        // identically-titled *Music* pills in the row with nothing to tell them apart — which is
        // the strip growing by PEOPLE, the one property this projection exists to prevent. The
        // first one carries the type; the rest are reachable through the toolbar's Source chip,
        // exactly as a borrowed library of a type you DO own already is.
        let mut borrowed_kinds: Vec<SecKind> = Vec::new();
        let v: Vec<usize> = sections()
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let owned = sources().get(s.src).map(|x| x.handle.is_empty()).unwrap_or(true);
                if owned {
                    return true;
                }
                if owned_kinds.contains(&s.kind) || borrowed_kinds.contains(&s.kind) {
                    return false;
                }
                borrowed_kinds.push(s.kind);
                true
            })
            .map(|(i, _)| i)
            .collect();
        // The strip's own generation, bumped only when the projected PILL LIST changes. The section
        // table's generation moves for things the strip cannot see — a source's counts landing, a
        // second friend's libraries arriving that all fold onto pills already drawn — and the label
        // cache downstream re-measures every pill in the row when it does. That is Home's hot path,
        // and with sources landing one at a time it now happens several times a boot.
        // BORROW, never `.read()`: that copies the `Vec` bitwise, and dropping the copy frees the
        // buffer the static still points at — a double free that aborts the process, not a warning.
        let changed = unsafe { (*addr_of!(TABS)).as_slice() != v.as_slice() };
        unsafe {
            *addr_of_mut!(TABS) = v;
            TABS_GEN = g;
            if changed {
                TABS_SHAPE_GEN += 1;
            }
        }
    }
    unsafe { &*addr_of!(TABS) }
}

/// The generation of the strip's own SHAPE — moves only when the projected pill list actually
/// changes, which is what the tab row's label + width cache keys on
/// ([`widgets::with_tab_metrics`](crate::ui::widgets)).
pub(crate) fn tabs_gen() -> u32 {
    tabs(); // re-project first: a stale answer would name the generation of a row nobody has
    unsafe { addr_of!(TABS_SHAPE_GEN).read() }
}

/// Pills in the strip (excluding Home, which the strip prepends itself).
pub(crate) fn tab_count() -> usize {
    tabs().len()
}
pub(crate) fn tab_title(t: usize) -> &'static str {
    tabs().get(t).map(|&i| section_title(i)).unwrap_or("")
}
/// The section a pill opens.
pub(crate) fn tab_section(t: usize) -> usize {
    tabs().get(t).copied().unwrap_or(0)
}
/// The pill that represents section `s`: its own when it has one, else the pill of the same TYPE —
/// so browsing a friend's film library keeps the *Movies* pill lit, which is what "a pill is a
/// type" means for the selection capsule. Falls back to 0, never out of range.
pub(crate) fn tab_of_section(s: usize) -> usize {
    let t = tabs();
    if let Some(p) = t.iter().position(|&i| i == s) {
        return p;
    }
    let Some(kind) = section_kind(s) else { return 0 };
    t.iter().position(|&i| section_kind(i) == Some(kind)).unwrap_or(0)
}

// ---- pinning: the ONE control, it governs Home only, and it is PER PROFILE -------------------
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
/// The table only ever holds the sources that have ANSWERED, and Home is the one screen whose boot
/// enumerates a single server: `ensure_sections` fetches the CURRENT server's sections and no
/// others. So on every boot after the first, a share sits in the roster with no row here at
/// all — and `pms::feeds_home`'s "a library nobody has discovered is undecided, not unpinned" rule
/// then puts a friend's shelves on the front door of somebody who turned them off last night.
/// Their answer is on disk keyed by machine, which is exactly the join that settles it.
///
/// `None` means "nothing has been read yet", never "nothing was recorded" — the same distinction
/// [`library_pins`] and `Session::home_pins` both turn on. Written by [`resolve_pins`] and
/// [`record_pins`], cleared by [`reset`]; never read from the file per frame.
static mut RECORDED: Option<crate::plex::session::HomePins> = None;

/// One source's `machineIdentifier` as the registry knows it, `""` while nobody has learned it.
fn machine_of(sid: ServerId) -> String {
    crate::plex::client_for(sid).map(|c| c.machine_id().to_string()).unwrap_or_default()
}

/// The section table as the pin rules see it, in table order.
fn lib_refs() -> Vec<crate::plex::pins::LibRef<'static>> {
    let srcs = sources();
    sections()
        .iter()
        .map(|s| {
            let (machine_id, owned) =
                srcs.get(s.src).map(|c| (c.machine_id.as_str(), c.owned)).unwrap_or(("", true));
            crate::plex::pins::LibRef { machine_id, key: s.key, owned }
        })
        .collect()
}

/// Re-derive every row's pin from the CURRENT profile's persisted answer.
///
/// Called on every append rather than only on the fresh rows, because the never-empty floor is a
/// question about the whole table (`plex::pins::resolve`). Idempotent over rows that already have
/// an answer: every flip is recorded, so a resolved row and its record agree.
///
/// It reads the session file, which is a lock + an `fs::read` — acceptable only because this runs
/// once per source's sections landing and never per frame. `session.rs`'s own doc forbids the
/// latter, and this is the reason the read is here rather than inside [`pinned`].
fn resolve_pins() {
    let libs = lib_refs();
    let sess = crate::plex::session::peek();
    let rec = sess.pins_for(&crate::plex::session::current_profile_key());
    let want = crate::plex::pins::resolve(&libs, rec);
    unsafe {
        // …and keep the record itself, because [`library_pins`] needs it for the sources this
        // table does NOT hold and cannot afford to read the file (it runs off Home's per-frame
        // pump). Cleared by [`reset`], so it can never outlive the profile that gave it.
        *addr_of_mut!(RECORDED) = rec.cloned();
        let secs = &mut *addr_of_mut!(SECTIONS);
        for (i, on) in want.into_iter().enumerate() {
            if let Some(s) = secs.get_mut(i) {
                s.pinned = on;
            }
        }
    }
}

/// Write the table's current state down as this profile's answer.
///
/// `asked` is what the first-run route passes `true` for; a toggle made later from the Library's
/// Sources panel also records `true`, because a recorded decision is a recorded decision — the
/// defaults must never come back and overrule one, and a profile that has flipped a switch has
/// plainly been asked.
pub(crate) fn record_pins(asked: bool) {
    let libs = lib_refs();
    let on: Vec<bool> = sections().iter().map(|s| s.pinned).collect();
    let user = crate::plex::session::current_profile_key();
    let fresh = crate::plex::pins::record(&user, asked, &libs, &on);
    let mut written: Option<crate::plex::session::HomePins> = None;
    // Through `update`, never `save`: this owns ONE field of a file four other writers touch (the
    // roster worker, the profile switch, the sign-in save, the search-recents flush), and a
    // whole-file replace from a stale snapshot is how a lost update signs the device out.
    crate::plex::session::update(|s| {
        // …and MERGE rather than replace, because this table is what has answered, not what
        // exists: a friend's server asleep at the moment a switch is flipped must not have its
        // recorded answer overwritten with silence (`plex::pins::carry_forward`).
        let rec = crate::plex::pins::carry_forward(fresh.clone(), s.pins_for(&user), &libs);
        let mut next = s.clone();
        next.set_pins_for(&user, rec.clone());
        written = Some(rec);
        Some(next)
    });
    // Only what actually reached the file: `update` is a no-op on a session with no `client_id`,
    // and adopting a record that was never written would make the snapshot disagree with disk.
    //
    // No `SECTIONS_GEN` bump here, deliberately — a caller that CHANGED something owes one
    // ([`toggle_pin`] makes it, one line after this call). Writing alone cannot change what
    // [`library_pins`] answers: the fresh record is the table's own state, and `carry_forward` can
    // only put back entries [`RECORDED`] already held. A future caller that flips a row without
    // going through `toggle_pin` owes the bump too, or Home does not re-merge.
    if let Some(rec) = written {
        unsafe { *addr_of_mut!(RECORDED) = Some(rec) };
    }
}

/// **Has the first-run question been put to the profile now watching?** — the route's own gate,
/// answered from the granted roster and this profile's record. See `plex::pins::asks`.
pub(crate) fn first_run_asks() -> bool {
    let sess = crate::plex::session::peek();
    crate::plex::pins::asks(sources().len(), sess.pins_for(&crate::plex::session::current_profile_key()))
}

pub(crate) fn pinned(i: usize) -> bool {
    sections().get(i).map(|s| s.pinned).unwrap_or(false)
}
pub(crate) fn pinned_count() -> usize {
    sections().iter().filter(|s| s.pinned).count()
}
/// Is this the last library feeding Home? Its row draws its value dimmed and states the rule; it
/// is NOT dimmed whole, because dim means unavailable and this is the library that works.
pub(crate) fn is_last_pinned(i: usize) -> bool {
    pinned(i) && pinned_count() == 1
}
/// Flip a library's pin. Returns false — changing nothing — for the last pinned one: unpinning it
/// would leave Home with nothing to draw, which is the only real failure this control has.
pub(crate) fn toggle_pin(i: usize) -> bool {
    if is_last_pinned(i) {
        return false;
    }
    let Some(s) = (unsafe { (&mut *addr_of_mut!(SECTIONS)).get_mut(i) }) else { return false };
    s.pinned = !s.pinned;
    // …and it OUTLIVES the run. Every flip was in-memory until 2026-08-21, so a selection made on
    // the Sources panel was gone by the next boot and the ownership default came back — which
    // reads as the switch not working rather than as nothing having been written down.
    record_pins(true);
    // The pin is an input to Home's merge (`pms::feeds_home`), and the merge re-runs off this
    // generation — without the bump the switch said `On`, the store agreed, and Home did not
    // change until something else happened to land. Reported on the device exactly that way:
    // "Film Club is enabled On Home but not on home".
    SECTIONS_GEN.fetch_add(1, Ordering::SeqCst);
    crate::ui::idle::invalidate();
    true
}
/// Every library this profile has an ANSWER for as `(source index, section key, pinned)` — the
/// READ side of the pin store, and the ONE projection of it this module exports.
///
/// It replaced two narrower ones (`pinned_libraries`, `discovered_sources`) that `pms` folded into
/// server sets separately — three loops resolving a source index to a registry slot, so a change to
/// that mapping had to land in three places. Both are columns of this table.
///
/// **Two sources of answer, and the second one is not an optimisation.** The section table for
/// everything that has been enumerated, plus [`RECORDED`] for every roster source that has NOT —
/// because Home never enumerates. Boot fetches the CURRENT server's sections and no others, and
/// `browse::pump`/[`discover_pump`] run from the Library and Search screens; so on the second and
/// every later boot a share sat here with no rows at all, `pms::feeds_home` read that as
/// "undecided", and a friend's shelves went back on the front door of somebody who had turned them
/// off. The recorded answer is keyed by machine, which is precisely the join that was missing.
///
/// NB an EMPTY result STILL means "nothing has been discovered or recorded yet", NOT "nothing is
/// pinned": [`is_last_pinned`] forbids unpinning the last library, so the pinned set is never
/// legitimately empty. `pms::feeds_home` is written around exactly that distinction.
///
/// The section KEY is what an item carries (`librarySectionID`), so this is the join Home needs to
/// honour a per-library pin: `/hubs` is a whole-SERVER request and answers with rows from every
/// library on it, pinned or not. Without the key the finest gate available is "does this server
/// feed Home at all", which is why unpinning one library of a two-library server changed nothing.
pub(crate) fn library_pins() -> Vec<(usize, i64, bool)> {
    let mut out: Vec<(usize, i64, bool)> = sections().iter().map(|s| (s.src, s.key, s.pinned)).collect();
    let Some(rec) = (unsafe { (*addr_of!(RECORDED)).as_ref() }) else { return out };
    for (si, src) in sources().iter().enumerate() {
        // A source with rows here has been enumerated and the table is the truth for it — including
        // a library it has since lost. A nameless machine cannot be joined against a record at all.
        if src.machine_id.is_empty() || sections().iter().any(|s| s.src == si) {
            continue;
        }
        for (lib, on) in rec.on.iter().map(|l| (l, true)).chain(rec.off.iter().map(|l| (l, false))) {
            if lib.machine_id == src.machine_id {
                out.push((si, lib.key, on));
            }
        }
    }
    out
}

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
    /// false dims the WHOLE group, header included, and states it there
    pub(crate) reachable: bool,
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

pub(crate) fn source_groups() -> Vec<SrcGroup> {
    sources()
        .iter()
        .map(|s| SrcGroup { name: s.name.clone(), handle: s.handle.clone(), reachable: s.reachable })
        .collect()
}

/// The TYPE the Source panel is scoped to: the kind of the library currently being browsed, which
/// is also the kind of the selected TAB. `None` only before any section exists.
fn cur_kind() -> Option<SecKind> {
    sections().get(cur()).map(|s| s.kind)
}

/// The Sources list's rows — **only the libraries of the CURRENT TAB'S TYPE**, across every source.
///
/// The scoping is the whole point and it is owner-directed (2026-08-14): "the chip should not switch
/// tabs — the Movies tab should show only movie shared libraries, and shows shows." Picking a row
/// used to be able to land on a library of another type, which moved the selected SECTION and
/// therefore the selected TAB, so a control in the toolbar silently navigated the row above it.
///
/// Filtering here rather than guarding the activation is deliberate: a guard would leave rows on
/// screen that refuse to do anything when pressed, and the panel would have to explain why. With the
/// type as the scope there is nothing to explain — the tab bar is how you reach the other type, and
/// a source with no library of this type drops out of the panel entirely (`source_sections` already
/// omits a group whose rows are all gone, so no empty header is drawn).
///
/// Both LEVELS are scoped, not just Browse. The panel is one row set shown two ways, and a Browse
/// level that listed four libraries while On Home listed six would read as two different lists. The
/// cost is that pinning a TV library is done from the TV Shows tab; the tab bar is one press away,
/// and `pinned`/`last_pinned` stay whole-roster facts so the "Home needs one library" refusal still
/// counts every pin, not just the visible ones.
pub(crate) fn source_rows() -> Vec<SrcRow> {
    let Some(kind) = cur_kind() else { return Vec::new() };
    rows_where(|s| s.kind == kind)
}

/// **Every library, unscoped** — the first-run route's list (`Shared Sources.dc.html` §F).
///
/// The panel's [`source_rows`] is scoped to the browsed TYPE because a chip in the toolbar must
/// not silently navigate the tab above it. The route has no tab bar and no current library: it
/// asks about the whole roster at once, which is the only shape in which "what goes on your Home?"
/// is one question. Same rows, same projection, same row model on the other side — see
/// `ui::source_list`.
pub(crate) fn all_source_rows() -> Vec<SrcRow> {
    rows_where(|_| true)
}

/// The projection both row sets share. Split out so the two can never disagree about what a row
/// SAYS while disagreeing about which rows there are — the failure a second hand-written builder
/// invites, and the reason `source_rows` grew a twin rather than the route growing its own.
fn rows_where(keep: impl Fn(&BrowseSection) -> bool) -> Vec<SrcRow> {
    // Whole-roster facts, both of them, even when the caller is showing a subset: the "Home needs
    // one library" refusal counts every pin, not just the visible ones.
    let last = pinned_count() == 1;
    let cur = cur();
    sections()
        .iter()
        .enumerate()
        .filter(|(_, s)| keep(s))
        .map(|(i, s)| SrcRow {
            src: s.src,
            section: i,
            title: s.title.clone(),
            count_line: count_line(s.count, s.kind),
            pinned: s.pinned,
            last_pinned: last && s.pinned,
            current: i == cur,
        })
        .collect()
}

/// A library's sub-line: its size once the count has landed, else its TYPE. Never absent — a row
/// that loses its second line changes height, and the panel is not allowed to resize under a level
/// switch or a landing.
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
pub(crate) fn recheck_shares() {
    sync_roster();
    // EVERY source, not only the ones already known to be down. Asking again is the whole content
    // of this row: a server that has since gone offline learns it (its group dims), one that came
    // back learns that, and a library the owner has created since appears — `append_sections` adds
    // only what is new, so nothing already on screen moves.
    for i in 0..sources().len() {
        if let Some(s) = source_mut(i) {
            s.retry_cd = 0;
            s.sections_done = false;
            s.counts_done = false;
        }
    }
    crate::ui::idle::invalidate();
}

// ---- public surface: items ------------------------------------------------------------------

/// totalSize of the current query, or -1 while the first page is still out.
pub(crate) fn total() -> i64 {
    cur_state().map(|s| s.total).unwrap_or(-1)
}
/// Item at absolute index `i` — None = not yet fetched (draw a skeleton). The reference is
/// valid until the next [`pump`]/re-query (main-thread only, same lifetime rule as
/// `pms::movie`).
pub(crate) fn item(i: usize) -> Option<&'static PmsMovie> {
    cur_state().and_then(|s| s.items.get(i)).and_then(|o| o.as_ref())
}
/// Flip `(sid, rk)`'s watched state in every section's item store — the optimistic half of a
/// view-state write, for the browse grid.
///
/// `pms::edit_item`'s twin, and it exists because that one only ever touched the HOME hubs: its own
/// doc says "the item is on no shelf (a Library-grid or Related item): nothing to redraw", which
/// was exactly right and exactly the gap. Mark a film watched from the grid's context menu and the
/// tick did not appear until a refetch — the press read as having done nothing.
///
/// **Every section, not the current one.** A section the user browsed a minute ago keeps its items
/// and its scroll position (that is the whole point of the per-section store), so leaving it stale
/// would put the tick back the wrong way round the moment they tab back to it.
///
/// The row is edited, never REMOVED: an "unwatched only" listing genuinely no longer contains an
/// item just marked watched, but a card vanishing from under the cursor on a press that was about
/// state and not about membership is worse than one that is briefly in a list it no longer matches.
/// The refetch the write's landing kicks is what reconciles that, as it does everywhere else here.
///
/// Returns whether anything matched, matching `pms::edit_item`'s shape. **MAIN THREAD.**
pub(crate) fn set_watched_local(sid: crate::plex::ServerId, rk: &str, on: bool) -> bool {
    let states = unsafe { &mut *addr_of_mut!(STATES) };
    let mut hit = false;
    for m in states.iter_mut().flat_map(|s| s.items.iter_mut()).flatten() {
        if crate::plex::same_item((m.sid, &m.rk), (sid, rk)) {
            crate::pms::set_watched(m, on);
            hit = true;
        }
    }
    hit
}

/// The grid's desired index window (visible + lookahead) — drives which page fetches next.
pub(crate) fn want(lo: usize, hi: usize) {
    unsafe { WANT = (lo, hi) };
}
/// What the current section's last page fetch did — the Library screen's read-out is a projection
/// of this and the store, never of the store alone.
pub(crate) fn fetch_state() -> SecFetch {
    cur_state().map(|s| s.fetch).unwrap_or(SecFetch::Loading)
}

/// True while the current query has no data yet (first page in flight) — the grid's full-screen
/// spinner state.
///
/// Reads the STATE, not `total < 0`: those two agreed for every path that ends in an answer, and
/// disagreed for the one that doesn't. A failed first page leaves `total` at -1 forever, so this
/// used to spin forever with it.
///
/// **Only the suite calls it now** — the screen asks `ui::library::readout()`, which folds this
/// state together with the section TABLE's. Kept, and marked, because the three tests that assert
/// it are asserting exactly the distinction above; `rustc`'s dead-code warning does not count test
/// callers, so this reads as removable and is not.
#[allow(dead_code)]
pub(crate) fn loading_initial() -> bool {
    fetch_state() == SecFetch::Loading
}

/// The same three states for the SOURCE behind what the screen is showing — the layer above a
/// page, and the other half of the Library's read-out ([`crate::ui::library`]'s `readout_of`).
///
/// It is a PROJECTION of [`BrowseSource`]'s flags, not a fourth field: `reachable` and
/// `sections_done` already carry the whole answer, per source, which is strictly more than the one
/// global this replaced could say. (That global existed on a branch written before the table had a
/// source dimension at all; keeping both would have been one rule in two places, and this file's
/// history is mostly that mistake.)
///
/// It asks about the source of the section at [`cur`], falling back to the current SERVER when the
/// table has no section to ask about — which is exactly the case the read-out one layer up exists
/// for, a server that could not be reached to discover anything.
pub(crate) fn cur_source_state() -> SecFetch {
    let Some(s) = cur_source_idx().and_then(|i| sources().get(i)) else { return SecFetch::Loading };
    if !s.reachable {
        SecFetch::Failed
    } else if s.sections_done {
        SecFetch::Ready
    } else {
        SecFetch::Loading
    }
}
/// Seed the roster with `n` sources in a chosen reachability, for a host test on the SCREEN side —
/// `ui::library`'s read-out is a projection of these flags, and its tests cannot reach this
/// module's private ones. The real transition needs a server that refuses to answer, which no host
/// tier has. Compiled out of every shipped build.
#[cfg(test)]
pub(crate) fn seed_sources_for_test(n: usize, reachable: bool) {
    reset();
    // Source 0 takes whatever `plex::current` already is, and NOTHING is registered. Registering
    // here would leave slots in a crate-global registry that `pump`'s `sync_roster` re-adopts, and
    // `maybe_discover` would then park a worker in `connect(2)` against a fixture address on behalf
    // of some later test — the hazard `plex::servers`' own `Fresh` guard exists for. Matching the
    // current server instead makes the empty-table fallback in `cur_source_idx` resolve to source 0
    // by construction, with no global touched but this module's own.
    let cur = crate::plex::current_server();
    let v: Vec<BrowseSource> = (0..n)
        .map(|k| BrowseSource {
            sid: if k == 0 { cur } else { ServerId::from_raw(k as u16) },
            machine_id: format!("mach-{k}"),
            owned: k == 0,
            name: if k == 0 { "nas-home".into() } else { "film-club".into() },
            handle: if k == 0 { String::new() } else { "friend".into() },
            reachable,
            sections_done: reachable,
            counts_done: true,
            retry_cd: 0,
        })
        .collect();
    unsafe { *addr_of_mut!(SOURCES) = v };
}

/// Re-kick the source behind what the screen is showing — the read-out's *Try again*, and the ONE
/// place the two layers are told apart.
///
/// Both back-offs go: the SOURCE's, so an undiscovered table is re-attempted on the next `pump`
/// rather than in ~10 s ([`SRC_RETRY_CD`]), and the page's, so a section that has its table refetches
/// at once rather than in ~2 s. Clearing both is right whichever layer failed — the one that already
/// has its answer has nothing to re-attempt — and it means the read-out's control needs to know
/// nothing about which layer it is fixing.
///
/// **Nothing here blocks.** The first version called [`ensure_sections`] for the undiscovered case,
/// on the reasoning that it is the same call route entry makes — but route entry makes it for a
/// server nobody has dialled yet, while this button is only reachable for one that has just been
/// PROVEN not to answer, so it would park the SDL loop for the full `connect(2)` timeout every
/// press. That trade did not exist on the branch this came from, where there was no worker to
/// discover a source; there is now, and `maybe_discover` picks up exactly the pair this sets
/// (`retry_cd == 0 && !sections_done`) on the very next [`pump`], off the main thread. The button's
/// whole job at both layers is therefore the same one: skip the wait.
pub(crate) fn retry_cur_source() {
    unsafe { RETRY_CD = 0 };
    if let Some(i) = cur_source_idx() {
        if let Some(s) = source_mut(i) {
            s.retry_cd = 0;
        }
    }
}

/// The MACHINE name and the OWNER's handle of the source behind what the screen is showing.
///
/// These are the only two identifying strings the Library's failure read-out is allowed to say
/// (`ui::library`'s `dead_strs` — no address, no path, no machineIdentifier: `ui::stats`' rule, for
/// its reason). Either can be `""` and each means something different by it: an unknown machine has
/// not named itself yet, while an empty HANDLE means the source is your OWN server and there is no
/// owner to name — drawn as the absence of a line, never as an empty one.
pub(crate) fn cur_source_labels() -> (&'static str, &'static str) {
    cur_source_idx()
        .and_then(|i| sources().get(i))
        .map(|s| (s.name.as_str(), s.handle.as_str()))
        .unwrap_or(("", ""))
}

/// The source behind what the screen is showing: the section at [`cur`], else the current server —
/// an empty table has no section to ask about, and that is exactly the case the read-out one layer
/// up exists for. Shared by [`cur_source_state`] and [`retry_cur_source`] so the state that draws
/// the read-out and the retry that answers it can never mean two different servers.
fn cur_source_idx() -> Option<usize> {
    sections()
        .get(cur())
        .map(|s| s.src)
        .or_else(|| {
            let sid = crate::plex::current_server();
            sources().iter().position(|s| s.sid == sid)
        })
        .filter(|&i| i < sources().len())
}

// ---- public surface: sort menu --------------------------------------------------------------

pub(crate) fn sorts() -> &'static [SortEntry] {
    cur_state().map(|s| s.sorts.as_slice()).unwrap_or(&[])
}
pub(crate) fn sort_idx() -> usize {
    cur_state().map(|s| s.sort_idx).unwrap_or(0)
}
pub(crate) fn sort_desc() -> bool {
    cur_state().map(|s| s.sort_desc).unwrap_or(false)
}
/// Current sort's display title for the toolbar chip ("Title" until the menus land).
pub(crate) fn sort_label() -> &'static str {
    let st = match cur_state() {
        Some(s) => s,
        None => return "",
    };
    st.sorts.get(st.sort_idx).map(|s| s.title.as_str()).unwrap_or("Title")
}
/// Apply a sort-menu pick: a NEW entry switches to it at its default direction; re-picking
/// the ACTIVE entry toggles direction. Re-queries the listing either way.
pub(crate) fn set_sort(idx: usize) {
    let c = cur();
    let Some(st) = state_mut(c) else { return };
    if idx >= st.sorts.len() {
        return;
    }
    if idx == st.sort_idx {
        st.sort_desc = !st.sort_desc;
    } else {
        st.sort_idx = idx;
        st.sort_desc = st.sorts[idx].default_desc;
    }
    requery();
}

// ---- public surface: filters ----------------------------------------------------------------

pub(crate) fn unwatched() -> bool {
    cur_state().map(|s| s.unwatched).unwrap_or(false)
}
pub(crate) fn toggle_unwatched() {
    let c = cur();
    if let Some(st) = state_mut(c) {
        st.unwatched = !st.unwatched;
    }
    requery();
}
pub(crate) fn genres() -> &'static [GenreEntry] {
    cur_state().map(|s| s.genres.as_slice()).unwrap_or(&[])
}
pub(crate) fn genre_sel() -> Option<&'static GenreEntry> {
    cur_state().and_then(|s| s.genre.as_ref())
}
/// Toolbar chip text: the active genre's name, else "All".
pub(crate) fn filter_label() -> &'static str {
    genre_sel().map(|g| g.title.as_str()).unwrap_or("All")
}
/// Apply a genre pick (None = All). Re-queries.
pub(crate) fn set_genre(idx: Option<usize>) {
    let c = cur();
    let Some(st) = state_mut(c) else { return };
    st.genre = idx.and_then(|i| st.genres.get(i).cloned());
    requery();
}
/// The ONE query-independent directory-fetch idiom (genres, letters): `done`-gated single
/// flight → spawn → `section_directory` → project rows → land in the mailbox tagged with the
/// section-table generation. `done` (not emptiness) gates the spawn: a landed-empty list is an
/// answer, and the FETCHING single-flight flags are GLOBAL — callers re-kick each frame while
/// waiting, so a fetch busy on ANOTHER section can't permanently starve this one.
fn kick_directory<T: Send + 'static>(
    done: bool,
    flag: &'static AtomicBool,
    mail: &'static Mutex<Option<(u32, usize, Vec<T>)>>,
    dir: &'static str,
    project: fn(&crate::plex::LibrarySection) -> Option<T>,
) {
    let c = cur();
    if states().get(c).is_none() || done {
        return;
    }
    // the SERVER this section lives on, captured on the main thread (`browse.rs`'s standing rule:
    // never resolve the current server inside a worker)
    let Some(sid) = section_sid(c) else { return };
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let key = sections()[c].key;
    let sgen = EPOCH.load(Ordering::SeqCst);
    let spawned = crate::task::spawn_small("directory", move || {
        let list = catch_unwind(|| {
            let mut v = Vec::new();
            if let Some(client) = crate::plex::client_for(sid) {
                if let Some(mc) = client.section_directory(key, dir) {
                    v.extend(mc.directory.iter().filter_map(project));
                }
            }
            v
        })
        .unwrap_or_default();
        // mailbox filled outside the guard so a panicking fetch still lands (empty)
        *mail.lock().unwrap_or_else(|e| e.into_inner()) = Some((sgen, c, list));
    });
    if !spawned {
        // `land_directory` clears the single-flight when it takes the mailbox, and nothing is
        // ever going to fill it — so release the flag here or this directory never fetches again
        // for the rest of the session. Callers re-kick every frame, so this retries by itself.
        flag.store(false, Ordering::SeqCst);
    }
}

/// The landing half of [`kick_directory`]: take the mailbox, clear the single-flight, and
/// apply to the section's state iff the table's IDENTITY (its [`EPOCH`]) still holds. Not its
/// shape: a section appended by another source since the spawn cannot have moved this index.
fn land_directory<T>(
    flag: &'static AtomicBool,
    mail: &'static Mutex<Option<(u32, usize, Vec<T>)>>,
    apply: impl FnOnce(&'static mut SecState, Vec<T>),
) {
    if let Some((sgen, sec, list)) = mail.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // a menu's value list arriving repopulates an open Sort/Filter popover (`ui::idle`)
        crate::ui::idle::invalidate();
        flag.store(false, Ordering::SeqCst);
        if sgen == EPOCH.load(Ordering::SeqCst) {
            if let Some(st) = state_mut(sec) {
                apply(st, list);
            }
        }
    }
}

/// Lazily fetch the genre value list the first time the filter menu opens (off-thread; lands
/// via [`pump`]).
pub(crate) fn kick_genres() {
    let done = cur_state().map(|s| s.genres_done).unwrap_or(true);
    kick_directory(done, &GENRE_FETCHING, &GENRE_RESULT, "genre", |d| {
        (!d.key.is_empty() && !d.title.is_empty())
            .then(|| GenreEntry { id: d.key.clone(), title: d.title.clone() })
    });
}

// ---- letter rail (firstCharacter index) -----------------------------------------------------

/// Per-letter (label, count) of the current section, or empty until [`kick_letters`] lands.
pub(crate) fn letters() -> &'static [(String, i64)] {
    cur_state().map(|s| s.letters.as_slice()).unwrap_or(&[])
}
/// Absolute item index of the first title under letter `i` — the prefix sum of the counts
/// before it (jump = focus/scroll move, never a filter: Emby semantics).
pub(crate) fn letter_start(i: usize) -> usize {
    letters().iter().take(i).map(|(_, n)| *n as usize).sum()
}
/// The rail is only truthful on the unfiltered ascending title listing: the letter counts
/// describe exactly that ordering. Menus not landed yet ⇒ the server default (titleSort asc)
/// is in effect, so the rail may show.
pub(crate) fn rail_available() -> bool {
    let Some(st) = cur_state() else { return false };
    let title_asc = match st.sorts.get(st.sort_idx) {
        Some(s) => s.key == "titleSort" && !st.sort_desc,
        None => true, // server default IS titleSort asc
    };
    title_asc && !st.unwatched && st.genre.is_none() && st.letters.len() > 1
}
/// Lazily fetch the `/firstCharacter` index for the current section (off-thread; lands via
/// [`pump`]). Letter counts are query-independent (always the unfiltered title listing).
pub(crate) fn kick_letters() {
    let done = cur_state().map(|s| s.letters_done).unwrap_or(true);
    kick_directory(done, &LETTERS_FETCHING, &LETTER_RESULT, "firstCharacter", |d| {
        (!d.key.is_empty() && d.size > 0).then(|| (d.title.clone(), d.size))
    });
}

// ---- remembered view ------------------------------------------------------------------------

pub(crate) fn saved_view() -> (usize, f32) {
    cur_state().map(|s| (s.focus, s.scroll)).unwrap_or((0, 0.0))
}
pub(crate) fn save_view(focus: usize, scroll: f32) {
    let c = cur();
    if let Some(st) = state_mut(c) {
        st.focus = focus;
        st.scroll = scroll;
    }
}

// ---- source discovery, off the main thread ---------------------------------------------------

/// What a discovery worker is being asked for. Two phases per source, one worker at a time
/// process-wide: the roster is a handful of servers and none of it is on a user's critical path.
enum SrcJob {
    /// its section list
    Sections,
    /// the unfiltered item count of each of its libraries, by section key
    Counts(Vec<i64>),
}

/// Pick and spawn the next source-discovery fetch, if any. Called once a frame by [`pump`].
fn maybe_discover() {
    // per-source failure backoff (the roster is short; this loop is cheaper than a heap of timers)
    for i in 0..sources().len() {
        if let Some(s) = source_mut(i) {
            s.retry_cd = s.retry_cd.saturating_sub(1);
        }
    }
    if SRC_FETCHING.load(Ordering::SeqCst) {
        return;
    }
    // SECTIONS for every source before the COUNTS of any: the section list is what puts a library
    // on screen (a tab pill, a Sources row), the count is a sub-line refining one that is already
    // drawn. Doing them source-by-source instead would put our own libraries' counts — three LAN
    // requests — ahead of a friend's libraries existing at all.
    let mut pick: Option<(usize, ServerId, SrcJob, bool)> = None;
    let ready = |i: usize| sources().get(i).map(|s| s.retry_cd == 0).unwrap_or(false);
    let no_name = |i: usize| sources().get(i).map(|s| s.name.is_empty()).unwrap_or(false);
    for i in 0..sources().len() {
        if ready(i) && !sources()[i].sections_done {
            pick = Some((i, sources()[i].sid, SrcJob::Sections, no_name(i)));
            break;
        }
    }
    if pick.is_none() {
        for i in 0..sources().len() {
            if !ready(i) || sources()[i].counts_done {
                continue;
            }
            let keys: Vec<i64> = sections().iter().filter(|x| x.src == i).map(|x| x.key).collect();
            if keys.is_empty() {
                // a source with nothing browsable (music/photo only) has no counts to fetch, and
                // must not be picked again for the rest of the session
                if let Some(s) = source_mut(i) {
                    s.counts_done = true;
                }
                continue;
            }
            pick = Some((i, sources()[i].sid, SrcJob::Counts(keys), no_name(i)));
            break;
        }
    }
    let Some((si, sid, job, want_name)) = pick else { return };

    let epoch = EPOCH.load(Ordering::SeqCst);
    let is_sections = matches!(job, SrcJob::Sections);
    SRC_FETCHING.store(true, Ordering::SeqCst);
    // `sid` is captured HERE, on the main thread. The worker resolves it through `client_for`
    // and never reads `client()` — a worker that asked for "the current server" would dial
    // whichever one the user happened to be browsing by the time it got scheduled.
    let spawned = crate::task::spawn_small("sources", move || {
        let landing = catch_unwind(|| {
            // the server naming ITSELF, so a roster that never reached plex.tv still heads its
            // group with a machine name. One request, once, per source.
            let name = if want_name {
                crate::plex::client_for(sid).and_then(|c| c.friendly_name()).unwrap_or_default()
            } else {
                String::new()
            };
            let what = match job {
                SrcJob::Sections => SrcWhat::Sections(
                    crate::plex::client_for(sid).and_then(|c| c.sections()).map(|mc| project_sections(&mc)),
                ),
                SrcJob::Counts(keys) => {
                    let mut out = Vec::new();
                    if let Some(c) = crate::plex::client_for(sid) {
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
                            if let Some(mc) = c.section_items_query(&q) {
                                out.push((k, mc.total_size));
                            }
                        }
                    }
                    SrcWhat::Counts(out)
                }
            };
            SrcLanding { name, what }
        })
        .unwrap_or_else(|_| {
            // a panicking fetch is a FAILURE of the job it was doing, never a success of another:
            // reporting a panicked count probe as a failed section list would drop the source's
            // whole library list on the floor.
            let what = if is_sections { SrcWhat::Sections(None) } else { SrcWhat::Counts(Vec::new()) };
            SrcLanding { name: String::new(), what }
        });
        *SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some((epoch, si, landing));
    });
    if !spawned {
        discovery_spawn_refused(si);
    }
}

/// The OS refused a discovery worker ([`crate::task::spawn_small`] returned `false`) — release the
/// single flight and back this source off.
///
/// Both halves are load-bearing and neither is enough alone.
///
/// * **Release**: `SRC_FETCHING` is cleared only inside a successful mailbox take, and nothing is
///   ever going to fill that mailbox — the same latch
///   `reset_clears_the_single_flight_flags_with_the_mailboxes` guards.
/// * **Back off**, by the same [`SRC_RETRY_CD`] a failed landing arms. Releasing alone leaves
///   [`maybe_discover`] re-picking this source on the very NEXT frame, and `task::spawn` logs every
///   refusal (`task: spawn 'sources' REFUSED`) — so a machine under enough thread pressure to
///   refuse a thread would write ~60 lines a second into the one file on-device triage reads. A
///   quieter log is the wrong fix: retrying at 60 Hz cannot succeed either, because nothing about
///   the refusal changes within a frame.
///
/// What it deliberately does NOT do is mark the source unreachable. Nothing was asked of the
/// server, so the Sources list must not dim its group and the Library must not say it failed; only
/// the next ATTEMPT moves. [`retry_cur_source`] clears this exactly as it clears a real failure, so
/// the read-out's *Try again* still skips the wait.
fn discovery_spawn_refused(si: usize) {
    SRC_FETCHING.store(false, Ordering::SeqCst);
    if let Some(s) = source_mut(si) {
        s.retry_cd = SRC_RETRY_CD;
    }
}

/// Apply a discovery landing. Gated on the table EPOCH, not on its shape generation: an append
/// from one source must not throw away another's answer.
fn land_discovery() {
    let Some((epoch, si, landing)) = SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    SRC_FETCHING.store(false, Ordering::SeqCst);
    crate::ui::idle::invalidate(); // a Sources row, a tab pill or a count appears
    if epoch != EPOCH.load(Ordering::SeqCst) {
        return; // the account changed under it — every index means something else now
    }
    let SrcLanding { name, what } = landing;
    if !name.is_empty() {
        if let Some(s) = source_mut(si) {
            s.name = name.clone();
        }
        SRC_FACTS_GEN.fetch_add(1, Ordering::SeqCst); // the group's header exists now
        // …and back into the registry, so anything else that asks about this server gets the same
        // answer without a second `GET /`. `owned` is carried through unchanged: this describer
        // learned a name, not a grant — which is what `describe_server_name` is FOR. It used to
        // recompute it here as `handle.is_empty()`, three lines under a comment claiming the
        // opposite, and that derivation is the one `ServerFacts::owned` documents as wrong: a share
        // whose `sourceTitle` plex.tv did not send has no handle and is still a share, so every one
        // of those flipped to "ours" the instant its friendly name landed.
        if let Some(s) = sources().get(si) {
            crate::plex::describe_server_name(s.sid, &name);
        }
    }
    match what {
        SrcWhat::Sections(list) => {
            let ok = list.is_some();
            append_sections(si, list.unwrap_or_default());
            if let Some(s) = source_mut(si) {
                let was = s.reachable;
                s.sections_done = ok;
                s.reachable = ok;
                s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
                if was != ok {
                    SRC_FACTS_GEN.fetch_add(1, Ordering::SeqCst); // the group dims, or comes back
                }
            }
            if !ok {
                // The machine name, never a token or an address — this line is what a user sends us.
                let who = sources().get(si).map(|s| s.name.clone()).unwrap_or_default();
                crate::log(&format!("browse: source {si} ({who}) did not answer — its group reads unreachable"));
            }
        }
        SrcWhat::Counts(counts) => {
            // EMPTY is a failure, not an answer: the worker pushes one entry per request that
            // succeeded, so a server that stopped answering mid-probe yields nothing. Latching
            // `counts_done` on that would leave those rows reading "Films" for the rest of the
            // session with no way to fix it — `maybe_discover` skips a done source and
            // `recheck_shares` only re-arms unreachable ones.
            let ok = !counts.is_empty();
            unsafe {
                for s in (*addr_of_mut!(SECTIONS)).iter_mut().filter(|s| s.src == si) {
                    if let Some((_, n)) = counts.iter().find(|(k, _)| *k == s.key) {
                        s.count = *n;
                    }
                }
            }
            if ok {
                SRC_FACTS_GEN.fetch_add(1, Ordering::SeqCst); // "Films" becomes "185 films"
            }
            if let Some(s) = source_mut(si) {
                s.counts_done = ok;
                s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
            }
        }
    }
}

// ---- pump: mailbox apply + next-fetch scheduling (main thread, once a frame) ----------------

/// Returns true when new items just landed (the grid re-clamps focus on it).
/// The ROSTER half of [`pump`], and nothing else: adopt newly registered servers, land a
/// discovery, schedule the next one. No paging, no menu fetches — so a screen that needs to know
/// what libraries exist can say so without pulling a grid's worth of items it will never draw.
///
/// It exists because the section table is only populated by [`pump`], which runs from the Library
/// screen alone. A boot straight into Search therefore knew the roster ("2 sources") and not one
/// library NAME, and the scope line beside the field — whose whole job is naming them — fell back
/// to "a shared server" forever. Cheap and idempotent: the same single-flight and per-source
/// backoff [`pump`] relies on, so calling it every frame from a second screen costs one comparison
/// once the answers are in.
pub(crate) fn discover_pump() {
    sync_roster();
    land_discovery();
    maybe_discover();
}

pub(crate) fn pump() -> bool {
    let mut changed = false;
    unsafe {
        if RETRY_CD > 0 {
            RETRY_CD -= 1;
        }
    }
    // the roster: adopt anything newly registered, land a discovery, schedule the next one. Cheap
    // and idempotent, and it is what lets a friend's libraries arrive without the main thread ever
    // waiting on their server.
    sync_roster();
    land_discovery();
    maybe_discover();
    // menu-data landings (query-independent; epoch-gated inside land_directory so a pre-reset
    // fetch can't populate a new user's state at the same index)
    land_directory(&GENRE_FETCHING, &GENRE_RESULT, |st, list| {
        st.genres_done = true;
        if st.genres.is_empty() {
            st.genres = list;
        }
    });
    land_directory(&LETTERS_FETCHING, &LETTER_RESULT, |st, list| {
        st.letters_done = true;
        if st.letters.is_empty() {
            st.letters = list;
        }
    });
    // page landing
    if let Some(r) = PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // a page landing fills the grid under a screen that may have gone idle waiting for it;
        // the FAILED branch repaints too, since the retry back-off changes what the grid shows
        crate::ui::idle::invalidate();
        FETCHING.store(false, Ordering::SeqCst);
        // A page fetch is also EVIDENCE ABOUT THE SERVER, and it is the only evidence that keeps
        // arriving after discovery: `sections_done` latches on success, so without this a source
        // that went offline an hour into the session could never stop reading as reachable. It is
        // a fact about NOW in both directions — a served page says the server is answering, a
        // failed one says it is not — and it is deliberately not gated on the query generation:
        // whether the machine replied does not depend on which listing was asked for.
        mark_source_reachable(r.sec, r.total >= 0);
        if r.total < 0 {
            // the fetch FAILED (network/parse) — leave the store exactly as it was and back
            // off before retrying (a wiped-to-"empty" store here was a review-confirmed bug:
            // one wifi hiccup blanked a populated grid permanently)
            unsafe { RETRY_CD = 120 }; // ~2s at 60fps
            // …but SAY SO. The store staying put is why nothing else in this landing can record
            // the failure, and for a first page that meant `total` stayed -1 and the grid spun
            // forever. Blamed on the same generation gate as a success: a landing from a query the
            // user has already replaced describes a listing nobody is looking at any more.
            if r.gen == GEN.load(Ordering::SeqCst) {
                if let Some(st) = state_mut(r.sec) {
                    st.fetch = SecFetch::Failed;
                }
            }
        } else if r.gen == GEN.load(Ordering::SeqCst) {
            if let Some(st) = state_mut(r.sec) {
                // the server answered: Ready even at totalSize 0 — an empty library is an answer,
                // never a fault (`StatusKind::Empty`'s rule, and `SecFetch`'s)
                st.fetch = SecFetch::Ready;
                if let Some(sorts) = r.sorts {
                    if st.sorts.is_empty() {
                        st.sorts = sorts;
                    }
                }
                if st.total != r.total {
                    st.total = r.total;
                    st.items.resize_with(st.total as usize, || None);
                }
                for (k, m) in r.items.into_iter().enumerate() {
                    if let Some(slot) = st.items.get_mut(r.start + k) {
                        *slot = Some(m);
                    }
                }
                changed = true;
            }
        }
    }
    maybe_spawn();
    changed
}

/// One fetch in flight at a time: pick the first missing page inside the wanted window
/// (or page 0 when the query has no data yet) and spawn it.
fn maybe_spawn() {
    if FETCHING.load(Ordering::SeqCst) || unsafe { RETRY_CD > 0 } {
        return;
    }
    let c = cur();
    let Some(st) = states().get(c) else { return };
    let Some(sec) = sections().get(c) else { return };

    let start = if st.total < 0 {
        0 // first page of this query
    } else {
        let (lo, hi) = unsafe { WANT };
        let hi = hi.min(st.total as usize);
        let mut found: Option<usize> = None;
        let mut p = (lo / PAGE) * PAGE;
        while p < hi {
            let end = (p + PAGE).min(st.total as usize);
            if st.items[p..end].iter().any(|o| o.is_none()) {
                found = Some(p);
                break;
            }
            p += PAGE;
        }
        match found {
            Some(p) => p,
            None => return, // wanted window fully resident
        }
    };

    let include_meta = st.sorts.is_empty();
    // sort param: empty until the menus land (server default = titleSort asc — the same
    // listing the menus arrive with, so the first page is never re-sorted under the user)
    let sort = match st.sorts.get(st.sort_idx) {
        Some(s) => format!("{}:{}", s.key, if st.sort_desc { "desc" } else { "asc" }),
        None => String::new(),
    };
    let mut filters: Vec<(String, String)> = Vec::new();
    // The unwatched filter is a MOVIE/SHOW question, and those are now the only two types that
    // reach here at all: shows advertise `unwatchedLeaves` (any unwatched episode), movies take a
    // plain `unwatched=1`, and the comment this replaced already recorded that the plain form has
    // odd semantics off type=1 (verified live 2026-07-19). The music/photo arm this match used to
    // carry went with `SecKind`'s variants — see its doc; an unplayable type is refused at
    // `from_wire` now, so there is no listing of one to send a filter to.
    if st.unwatched {
        match sec.kind {
            SecKind::Show => filters.push(("unwatchedLeaves".to_string(), "1".to_string())),
            SecKind::Movie => filters.push(("unwatched".to_string(), "1".to_string())),
        }
    }
    if let Some(g) = &st.genre {
        filters.push(("genre".to_string(), g.id.clone()));
    }

    let gen = GEN.load(Ordering::SeqCst);
    let key = sec.key;
    let sec_idx = c; // captured on the main thread; the worker must not read the statics
    // …and so is the SERVER — the section's OWN one, not whatever is current. `key` alone is
    // ambiguous across sources (both servers have a section `1`), `client()` inside the worker
    // would answer with whatever is current by then, and the sid is stamped onto every row this
    // parses, so a row is only ever addressable as `(sid, rk)` — see `pms::PmsMovie::sid`.
    let Some(sid) = section_sid(c) else { return };
    FETCHING.store(true, Ordering::SeqCst);
    let spawned = crate::task::spawn_small("page", move || {
        let result = catch_unwind(|| {
            let q = SectionQuery {
                section_key: key,
                sort: &sort,
                filters: &filters,
                start: start as i64,
                size: PAGE as i64,
                include_meta,
            };
            let mc = crate::plex::client_for(sid).and_then(|cl| cl.section_items_query(&q));
            let Some(mc) = mc else {
                return (Vec::new(), -1i64, None); // FAILURE sentinel — pump leaves the store alone
            };
            let items: Vec<PmsMovie> = mc.metadata.iter().map(|m| parse_item(m, sid)).collect();
            // keep the item count authoritative even when PMS omits totalSize on an
            // unpaged-shaped response (paged queries carry it; belt and braces)
            let total = if mc.total_size > 0 { mc.total_size } else { start as i64 + items.len() as i64 };
            let sorts = mc.meta.as_ref().and_then(|m| {
                m.types.iter().find(|t| t.active != 0).or_else(|| m.types.first()).map(|t| {
                    t.sort
                        .iter()
                        .filter(|s| !s.key.is_empty())
                        .map(|s| SortEntry {
                            key: s.key.clone(),
                            title: if s.title.is_empty() { s.key.clone() } else { s.title.clone() },
                            default_desc: s.default_direction == "desc",
                        })
                        .collect::<Vec<_>>()
                })
            });
            (items, total, sorts)
        })
        .unwrap_or((Vec::new(), -1, None)); // a panicking fetch is a failure, not an empty library
        // mailbox filled outside the guard so a panicking fetch still lands; single-flight
        // (FETCHING) means no monotone race — pump clears the flag when it takes this
        let (items, total, sorts) = result;
        *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(PageResult { gen, sec: sec_idx, start, items, total, sorts });
    });
    if !spawned {
        // the flag is cleared ONLY inside a successful mailbox take, and nothing will fill that
        // mailbox — the same latch `reset_clears_the_single_flight_flags_with_the_mailboxes`
        // guards. `maybe_spawn` runs every frame, so releasing it here retries by itself.
        FETCHING.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------------------
    /// A two-source table for tests OUTSIDE this module (`ui::home`'s focus walk): your Movies and
    /// TV Shows, a friend's films and shows that fold onto them, and a friend's music that does not
    /// — five libraries projecting to three pills. Writes crate globals, so the caller holds
    /// [`crate::testlock::serial`] and `reset()`s afterwards.
    #[cfg(test)]
    pub(crate) fn seed_two_source_table_for_test() {
        tests::seed_sources(vec![
            tests::a_source("mac-mini", "", true),
            tests::a_source("nas-home", "friend", true),
        ]);
        // Our own server has BOTH types; the share has one library of each. The third row here
        // used to be a Music library, standing in for "a type only a friend has" — that type is
        // gone (see `SecKind`), so the growth case is expressed by the OWNED side instead, in the
        // tests that need it: an owner with only Movies, a friend who also has Shows.
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        append_sections(
            1,
            vec![(1, "Film Club".into(), SecKind::Movie), (2, "Film Club".into(), SecKind::Show)],
        );
    }

    /// One more library landing on source `src`, for tests OUTSIDE this module — the discovery
    /// worker's answer arriving late, which the Library screen's open Sources panel has to rebuild
    /// for. Goes through the real [`append_sections`], so it moves [`sections_gen`] exactly as the
    /// landing does; a test that wrote `SECTIONS` directly would prove nothing about that bump.
    /// Same contract as [`seed_two_source_table_for_test`]: crate globals, so hold
    /// [`crate::testlock::serial`] and `reset()` afterwards.
    #[cfg(test)]
    pub(crate) fn append_section_for_test(src: usize, key: i64, title: &str, kind: SecKind) {
        append_sections(src, vec![(key, title.into(), kind)]);
    }

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `reset()` dropped the three result mailboxes but left the single-flight
    /// flags set, and those are cleared ONLY inside a successful mailbox take. Sequence:
    /// scroll Library so a page fetch spawns → BACK to Home (pump stops running) → the worker
    /// lands its result → switch profile → `install_pms` calls `reset()` and nulls the mailbox
    /// → the flag is now true with nothing left that can ever clear it. `maybe_spawn` returns
    /// early forever and the Library is a spinner until the app is killed.
    #[test]
    fn reset_clears_the_single_flight_flags_with_the_mailboxes() {
        let _g = crate::testlock::serial();
        FETCHING.store(true, Ordering::SeqCst);
        GENRE_FETCHING.store(true, Ordering::SeqCst);
        LETTERS_FETCHING.store(true, Ordering::SeqCst);
        *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;

        reset();

        assert!(!FETCHING.load(Ordering::SeqCst), "page fetch stayed latched — Library wedges");
        assert!(!GENRE_FETCHING.load(Ordering::SeqCst), "genre fetch stayed latched");
        assert!(!LETTERS_FETCHING.load(Ordering::SeqCst), "letters fetch stayed latched");
    }

    /// `reset()` must also drop the retry backoff, or a profile switch inherits the previous
    /// user's cooldown and stalls the first page fetch for up to ~2s.
    #[test]
    fn reset_clears_the_retry_backoff() {
        // Takes the crate lock for the same reason the fetch-machine tests below do — see the note
        // there. `reset()` is the most destructive call in this module, and a test that makes it
        // without the lock is not testing concurrently, it is CORRUPTING whoever is.
        let _g = crate::testlock::serial();
        unsafe { RETRY_CD = 120 };
        reset();
        assert_eq!(unsafe { RETRY_CD }, 0);
    }

    // ---- the three-state fetch machine ---------------------------------------------------------
    //
    // These drive `pump`, which reports to `ui::idle`'s process-global flag — the exact obligation
    // `ui/xfade.rs` inherited when its `tick` started doing the same — so they take the CRATE-wide
    // serial lock, not a module-local one. They also leave no section table behind them: with
    // `STATES` seeded and `SECTIONS` empty, `maybe_spawn` returns before it can reach the network,
    // so nothing here spawns a worker.

    /// One default section state, no section table (see above), and the mailbox emptied.
    fn seed_one_section() {
        reset();
        *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        unsafe { *addr_of_mut!(STATES) = vec![SecState::default()] };
    }
    /// Land what a worker would post for the CURRENT query: `total < 0` is the failure sentinel.
    fn land_page(total: i64, items: usize) {
        let r = PageResult {
            gen: GEN.load(Ordering::SeqCst),
            sec: 0,
            start: 0,
            items: (0..items).map(|_| PmsMovie::default()).collect(),
            total,
            sorts: None,
        };
        *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        pump();
    }

    /// THE bug: a failed first page armed the retry cooldown and nothing else, so `total` stayed
    /// -1, `loading_initial()` stayed true and the Library grid spun with no way out — for the
    /// rest of the session, on the user's own server. The failure must now be a STATE the screen
    /// can see, and the spinner must stop.
    #[test]
    fn a_failed_first_page_leaves_the_section_failed_and_not_loading() {
        let _g = crate::testlock::serial();
        seed_one_section();
        land_page(-1, 0);
        assert_eq!(fetch_state(), SecFetch::Failed);
        assert!(!loading_initial(), "the grid must stop spinning on a failure");
        assert_eq!(total(), -1, "…with nothing to show, which is what makes it the SCREEN's failure too");
        reset();
    }

    /// A served page is Ready, and stays the plain "here are your items" state.
    #[test]
    fn a_served_page_leaves_the_section_ready() {
        let _g = crate::testlock::serial();
        seed_one_section();
        land_page(3, 3);
        assert_eq!(fetch_state(), SecFetch::Ready);
        assert!(!loading_initial());
        assert_eq!(total(), 3);
        reset();
    }

    /// An EMPTY answer is an answer — `Ready`, never `Failed`. The library really does hold
    /// nothing (an unwatched filter that matches none, a section still being scanned), and the
    /// grid's own "Nothing here matches" line is the right read-out. This is `StatusKind::Empty`'s
    /// rule, stated in the state machine so a screen cannot get it wrong.
    #[test]
    fn an_empty_but_successful_listing_is_ready_not_failed() {
        let _g = crate::testlock::serial();
        seed_one_section();
        land_page(0, 0);
        assert_eq!(fetch_state(), SecFetch::Ready);
        assert_eq!(total(), 0, "an empty library is an answer, not a fault");
        assert!(!loading_initial());
        reset();
    }

    /// A failure belongs to the query it was fetched for. Re-query (a sort/filter/section change
    /// wipes the store) and the section is Loading again, not stuck wearing the old failure —
    /// otherwise the read-out would blame a listing the user has already replaced.
    #[test]
    fn a_requery_clears_a_previous_failure() {
        let _g = crate::testlock::serial();
        seed_one_section();
        land_page(-1, 0);
        assert_eq!(fetch_state(), SecFetch::Failed);
        requery();
        assert_eq!(fetch_state(), SecFetch::Loading);
        assert!(loading_initial());
        reset();
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
    fn seed_one_source(reachable: bool, sections_done: bool) -> usize {
        // The CURRENT server's id, registered or not — see `seed_sources_for_test` for why nothing
        // is registered here. It makes `cur_source_idx`'s empty-table fallback resolve to this row.
        seed_sources(vec![BrowseSource {
            sid: crate::plex::current_server(),
            machine_id: "mach-0".into(),
            owned: true,
            name: "nas-home".into(),
            handle: "friend".into(),
            reachable,
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
        seed_one_source(true, false);
        assert_eq!(cur_source_state(), SecFetch::Loading, "nobody has asked it anything yet");
        source_mut(0).unwrap().reachable = false;
        assert_eq!(cur_source_state(), SecFetch::Failed, "the screen must be able to see this");
        reset();
    }

    /// An account with nothing we browse ANSWERED. `Ready` with no sections, never `Failed` — the
    /// same reason an empty listing is (`StatusKind::Empty`), and the case that lands in the very
    /// same two `unwrap_or` defaults as a failure and so used to spin identically.
    #[test]
    fn a_source_with_no_browsable_library_answered_and_did_not_fail() {
        let _g = crate::testlock::serial();
        seed_one_source(true, true);
        assert_eq!(cur_source_state(), SecFetch::Ready, "an empty answer is an answer");
        assert_eq!(section_count(), 0);
        reset();
    }

    /// A served table clears a previous failure and seeds one state per section, and from then on
    /// the state is read off the SECTION's source rather than off the current server.
    #[test]
    fn a_served_table_clears_the_failure_and_seeds_its_states() {
        let _g = crate::testlock::serial();
        seed_one_source(false, false);
        assert_eq!(cur_source_state(), SecFetch::Failed);
        {
            let s = source_mut(0).unwrap();
            s.reachable = true;
            s.sections_done = true;
        }
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "Film Club".into(), SecKind::Movie)]);
        assert_eq!(cur_source_state(), SecFetch::Ready);
        assert_eq!(section_count(), 2);
        assert!(loading_initial(), "a fresh section has not answered yet");
        reset();
    }

    // ---- the (source, section) table ------------------------------------------------------------
    //
    // These seed SOURCES directly and mark every phase done, so `maybe_discover` picks nothing and
    // no worker is spawned — the same discipline as the fetch-machine tests above, one layer up.
    // Their `sid` is `UNSET`, which resolves to no client, so even a spawn could reach no socket.

    pub(super) fn a_source(name: &str, handle: &str, reachable: bool) -> BrowseSource {
        BrowseSource {
            sid: ServerId::UNSET,
            // the machine id doubles as the fixture's identity, and OWNERSHIP follows the handle
            // here (a fixture, not the product rule — `sync_roster` takes `owned` from the roster,
            // because a share whose `sourceTitle` plex.tv did not send is still a share)
            machine_id: name.to_string(),
            owned: handle.is_empty(),
            name: name.into(),
            handle: handle.into(),
            reachable,
            sections_done: true,
            counts_done: true,
            retry_cd: 0,
        }
    }
    pub(super) fn seed_sources(srcs: Vec<BrowseSource>) {
        reset();
        unsafe { *addr_of_mut!(SOURCES) = srcs };
    }

    /// THE reason the table gained a source dimension. Measured against the real share on
    /// 2026-08-11: both servers have a section `1`, and they are different libraries. A bare key
    /// names two things, so every row carries its source and the two rows coexist.
    #[test]
    fn two_servers_both_have_a_section_one_and_the_table_tells_them_apart() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);

        assert_eq!(section_count(), 3);
        let ours = &sections()[0];
        let theirs = &sections()[2];
        assert_eq!((ours.key, ours.src), (1, 0), "our section 1, on source 0");
        assert_eq!((theirs.key, theirs.src), (1, 1), "THEIR section 1 — same key, different source");
        assert_eq!((section_title(0), section_title(2)), ("Movies", "Film Club"));
        // and the chip's annotation follows the section being browsed, not the account
        assert_eq!(handle_of(2), "friend");
        assert_eq!(handle_of(0), "", "your own libraries carry no owner at all");
        reset();
    }

    /// A source discovered LATE must never move an existing index. `PageResult.sec` is a section
    /// index, so a table that reshuffled under an in-flight fetch would splice one library's items
    /// into another's store — the soundness the old `ensure_sections` early-return provided and
    /// APPEND-ONLY now provides for every source rather than only for the second call.
    #[test]
    fn a_source_arriving_late_appends_and_moves_no_existing_index() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        set_cur(1);
        let before = (cur(), section_title(1).to_string(), states().len());

        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_eq!(cur(), before.0, "the library being browsed is still the one at that index");
        assert_eq!(section_title(1), before.1);
        assert_eq!(states().len(), section_count(), "states stay in lockstep with the table");
        assert_eq!(states().len(), before.2 + 1);

        // A RE-discovery ("Check for new shares", or a server that came back) re-offers the same
        // list: every row is already there, so nothing is duplicated and nothing moves…
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_eq!(section_count(), 3);
        assert_eq!(cur(), before.0);
        // …while a library the owner has CREATED since is appended, at the end, where it cannot
        // disturb an index anything is already holding.
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie), (4, "Club Shows".into(), SecKind::Show)]);
        assert_eq!(section_count(), 4);
        assert_eq!(section_title(3), "Club Shows");
        assert_eq!(section_title(1), before.1, "and the row we were browsing is untouched");
        reset();
    }

    // ---- the Home selection: defaults, persistence, and one answer per PROFILE ------------------
    //
    // The RULES are `plex::pins` and are graded there, pure. What is graded here is the plumbing
    // around them, which is where the failures actually live: does an answer reach the disk, does
    // it come back, and does it come back to the person who gave it.

    /// Redirect the session file at a directory of this test's own and seed a signed-in session
    /// (an empty `client_id` makes `session::update` a no-op by design), taking both back on drop.
    /// The caller must hold [`crate::testlock::serial`] for its whole body — see
    /// `session::redirect_for_test`.
    struct TempPins {
        dir: std::path::PathBuf,
    }
    impl TempPins {
        fn new(tag: &str) -> TempPins {
            let dir = std::env::temp_dir().join(format!("plxnative-pins-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            crate::plex::session::redirect_for_test(Some(dir.join("auth.json")));
            crate::plex::session::save(&crate::plex::session::Session {
                client_id: "cid-test".into(),
                ..Default::default()
            });
            TempPins { dir }
        }
        /// Become `uuid` — the same call the profile switch makes, and what every read and write of
        /// the selection keys on (`session::current_profile_key`).
        fn watching(&self, uuid: &str) {
            crate::plex::session::set_current(Some(crate::plex::session::UserRef {
                uuid: uuid.into(),
                ..Default::default()
            }));
        }
    }
    impl Drop for TempPins {
        fn drop(&mut self) {
            crate::plex::session::set_current(None);
            crate::plex::session::redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// One account, two servers — seeded and discovered exactly as a boot does it.
    fn seed_two_servers() {
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
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
        seed_two_servers();
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, true, false), "yours On, a friend's Off");
        assert_eq!(pinned_count(), 2);

        assert!(toggle_pin(2), "…and a friend's can be turned on, which is what makes it a decision");
        assert_eq!(pinned_count(), 3);
        assert!(toggle_pin(0) && toggle_pin(1), "your own can be unpinned — a preference, not a mistake");
        assert_eq!(pinned_count(), 1);

        assert!(is_last_pinned(2));
        assert!(!toggle_pin(2), "the last pinned library is refused");
        assert!(pinned(2), "…and refused means UNCHANGED, not toggled twice");
        assert_eq!(pinned_count(), 1);
        reset();
    }

    /// **A selection outlives the run.** Every flip was in-memory until 2026-08-21, so the answer
    /// was gone by the next boot and the ownership default came back — which reads as the switch
    /// not working rather than as nothing having been written down.
    #[test]
    fn a_selection_survives_the_table_being_rebuilt() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("persist");
        t.watching("u-owner");
        seed_two_servers();
        assert!(toggle_pin(2) && toggle_pin(1)); // the share On, one of ours Off
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, false, true));

        // …and now the table is wiped and re-discovered, which is what a profile switch, a
        // sign-in and a `reset` all do
        seed_two_servers();
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, false, true), "the answer came back");
        reset();
    }

    /// **The answer reaches Home before that server's libraries have been ENUMERATED.**
    ///
    /// Home is the one screen that never enumerates: boot fetches the CURRENT server's sections and
    /// no others, and the discovery pump runs from the Library and Search screens. So on the second
    /// and every later boot the share is in the roster with no row in the section table — and
    /// `pms::feeds_home`'s "a library nobody has discovered is undecided, not unpinned" rule then
    /// put a friend's shelves back on the front door of somebody who had turned them off the night
    /// before. `library_pins` is the join, and the recorded answer is the other half of it.
    #[test]
    fn a_recorded_answer_reaches_home_before_that_servers_sections_do() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("unenumerated");
        t.watching("u-owner");
        seed_two_servers();
        record_pins(true); // `Start watching` on the defaults: ours On, the friend's Off

        // the next boot: the roster is restored, our own sections are fetched, the share's are not
        let boot = || {
            seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
            append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        };
        boot();
        assert_eq!(sections().len(), 2, "the share has not answered — it contributes no rows");
        let pins = library_pins();
        assert!(
            pins.contains(&(1, 1, false)),
            "the friend's recorded Off is reported anyway, or Home reads it as undecided: {pins:?}"
        );
        assert_eq!(pins.len(), 3, "…and nothing else is invented: two enumerated rows plus the one record");

        // the other direction, so this is a JOIN and not a blanket "a share is off"
        t.watching("u-owner");
        seed_two_servers();
        assert!(toggle_pin(2));
        record_pins(true);
        boot();
        assert!(library_pins().contains(&(1, 1, true)), "a recorded On reaches Home the same way");

        // and a source the record cannot NAME is left undecided rather than joined by accident
        seed_sources(vec![a_source("mac-mini", "", true), {
            let mut s = a_source("nas-home", "friend", true);
            s.machine_id = String::new();
            s
        }]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        assert!(library_pins().iter().all(|&(si, _, _)| si == 0), "a nameless machine joins nothing");
        reset();
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
        seed_two_servers();
        assert!(toggle_pin(2), "the friend's library goes on Home");
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, true, true));

        // a boot the share missed entirely, on which one of our own is turned off
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        assert!(toggle_pin(1));
        assert!(library_pins().contains(&(1, 1, true)), "the absent share is still recorded On");

        // …and the next boot on which it DOES answer finds both answers intact
        seed_two_servers();
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, false, true));
        reset();
    }

    /// **THE requirement: the selection is per PROFILE.** It hung off the `Session` — one per
    /// install — so a household could hold exactly one opinion about a friend's films, and
    /// switching profile left the previous person's shelves on the front door.
    #[test]
    fn two_profiles_keep_their_own_home_selections_across_a_switch() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("profiles");

        // Dad wants the friend's films on Home and does not want his own TV shows there.
        t.watching("u-dad");
        seed_two_servers();
        assert!(toggle_pin(2) && toggle_pin(1));
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, false, true));

        // The kid switches in. Never asked, so the defaults — NOT dad's answer.
        t.watching("u-kid");
        seed_two_servers();
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, true, false), "a switch switches the shelves");
        assert!(toggle_pin(0), "…and the kid answers for themselves");
        assert_eq!((pinned(0), pinned(1), pinned(2)), (false, true, false));

        // …and back, with dad's answer intact rather than overwritten by the kid's.
        t.watching("u-dad");
        seed_two_servers();
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, false, true), "one file, two answers");
        reset();
    }

    /// The route's own gate, end to end: two sources and an unanswered profile, then never again
    /// for that profile — while the person beside them is still owed the question.
    #[test]
    fn the_first_run_question_is_asked_once_per_profile() {
        let _g = crate::testlock::serial();
        let t = TempPins::new("gate");
        t.watching("u-dad");
        seed_two_servers();
        assert!(first_run_asks(), "two sources, and nobody has asked this profile");

        record_pins(true); // what `Start watching` — and BACK, which commits the same thing — does
        assert!(!first_run_asks(), "asked once, never again");
        t.watching("u-kid");
        assert!(first_run_asks(), "…and the answer belongs to the person who gave it");

        // A single-server install is not a question at all, whoever is watching.
        seed_sources(vec![a_source("mac-mini", "", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        assert!(!first_run_asks());
        reset();
    }

    /// **A pill is a TYPE, never a person.** The strip names your own libraries; a friend's film
    /// library gets no pill of its own (the toolbar chip under it says whose), but a type only they
    /// have does — otherwise that content is unreachable from the strip at all. And the selection
    /// capsule for a borrowed library rests on its TYPE's pill, so nothing is ever homeless.
    #[test]
    fn the_tab_strip_grows_by_types_and_never_by_people() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie), (2, "Their Shows".into(), SecKind::Show)]);

        assert_eq!(tab_count(), 2, "your Movies, plus the shows nobody of yours provides");
        assert_eq!((tab_title(0), tab_title(1)), ("Movies", "Their Shows"));
        assert_eq!(tab_section(1), 2);
        assert_eq!(tab_of_section(1), 0, "their films ride YOUR Movies pill — same type, one level");
        assert_eq!(tab_of_section(2), 1, "their shows have a pill of their own");
        reset();
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
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]); // we own films and nothing else
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie), (2, "Their Shows".into(), SecKind::Show)]);

        assert_eq!(tab_count(), 2, "your films, plus the shows nobody of yours provides");
        assert_eq!(tab_title(1), "Their Shows");
        assert_eq!(tab_of_section(2), 1, "their shows are their own pill, NOT your Movies one");
        assert_eq!(tab_of_section(1), 0, "…while their films still ride yours");
        // the wire types this product has a level for — and the ones it deliberately does not, which
        // is what keeps an unplayable library out of the strip, the Sources panel and the grid at once
        assert_eq!(SecKind::from_wire("movie"), Some(SecKind::Movie));
        assert_eq!(SecKind::from_wire("show"), Some(SecKind::Show));
        assert_eq!(SecKind::from_wire("artist"), None, "music has no level below the grid: no pill");
        assert_eq!(SecKind::from_wire("photo"), None);
        assert_eq!(SecKind::from_wire("mixed"), None, "a type with no level is still refused");
        reset();
    }

    /// **The deliverable, as an assertion**: the strip is a constant row however many friends
    /// arrive. Its pill list — and therefore its width, which is a pure function of the labels —
    /// does not move as the roster grows from one server to three, because every borrowed library
    /// folds onto the pill of a type you already have. Only a MISSING type may widen it.
    ///
    /// The shape the design rejected is the control: a pill per section reaches eleven pills here,
    /// which is what measured 2133px against a 1540px track at three friends.
    #[test]
    fn the_strip_is_the_same_row_at_one_friend_and_at_three() {
        let _g = crate::testlock::serial();
        seed_sources(vec![
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
        let row = || (0..tab_count()).map(|t| tab_title(t).to_string()).collect::<Vec<_>>();
        // We own FILMS and nothing else. The owner used to hold both types here, which made the
        // second half of this test need a third type (music) to have anything left over; with the
        // product's list down to two, the un-owned type has to be one of them.
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        let alone = row();
        assert_eq!(alone, vec!["Movies"], "your own library, and nothing else");

        for src in 1..=3 {
            append_sections(
                src,
                vec![(1, "Film Club".into(), SecKind::Movie), (3, "Film Club".into(), SecKind::Movie)],
            );
            assert_eq!(row(), alone, "source {src} added a pill — the strip must not grow by people");
        }
        assert_eq!(section_count(), 7, "seven libraries…");
        assert_eq!(tab_count(), 1, "…and the one pill it started with");

        // …and a type NOBODY owns grows the row by exactly one however many people share it. Every
        // fixture above is a type we own, which is why this half needs saying separately: it is the
        // only branch of the projection that can admit a borrowed library at all.
        for src in 1..=3 {
            append_sections(src, vec![(9, "Their Shows".into(), SecKind::Show)]);
        }
        assert_eq!(tab_count(), 2, "three friends sharing shows are ONE TV Shows pill");
        assert_eq!(row().len(), 2);
        reset();
    }

    /// A profile switch must not leave the previous account's pills on screen. `reset()` empties
    /// the table, and the strip's generation is what the tab row's label cache keys on — so if the
    /// projection's own memo were CLEARED here rather than merely invalidated, the comparison that
    /// decides "did the row change" would be `[] != []`, i.e. false, and `draw_tab_row` (which
    /// iterates the cache, not the live table) would go on drawing and hit-testing libraries the
    /// new user cannot open until some later landing happened to change the row.
    #[test]
    fn a_profile_switch_re_measures_the_strip_instead_of_keeping_the_last_accounts_pills() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        let g0 = tabs_gen();
        assert_eq!(tab_count(), 2);

        reset(); // install_pms: a different account signs in
        assert_eq!(tab_count(), 0, "the row is empty…");
        assert_ne!(tabs_gen(), g0, "…and the strip MUST re-measure rather than keep the old pills");
    }

    /// The strip's own generation moves when the ROW changes and not when the TABLE does — which,
    /// once a table is appended to one source at a time, are different questions. Every borrowed
    /// library that folds onto a pill you already have bumps the table's generation and changes
    /// nothing in the row, so keying the label + width cache on the table re-measured every pill in
    /// the strip once per source, on Home's hot path, for a strip that had not moved.
    #[test]
    fn only_a_changed_row_costs_the_tab_cache_a_re_measure() {
        let _g = crate::testlock::serial();
        seed_sources(vec![
            a_source("mac-mini", "", true),
            a_source("nas-home", "friend", true),
            a_source("nas-home", "friend", true),
        ]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        let (g0, table0) = (tabs_gen(), sections_gen());

        // two friends' film libraries land: both fold onto your Movies pill
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        append_sections(2, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert_ne!(sections_gen(), table0, "the TABLE's generation moved, twice");
        assert_eq!(tabs_gen(), g0, "…and the row did not, so it must not re-measure");

        // …while a MISSING type is exactly what must invalidate it (we own films only, so their
        // shows are the type nobody of ours provides)
        append_sections(2, vec![(9, "Their Shows".into(), SecKind::Show)]);
        assert_ne!(tabs_gen(), g0, "a gained pill MUST re-measure the row");
        reset();
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
        reset();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie), (2, "Their Shows".into(), SecKind::Show)]);

        // browsing a FILM library: both servers' film libraries, and no show library from either
        set_cur(0);
        let films: Vec<String> = source_rows().iter().map(|r| r.title.clone()).collect();
        assert_eq!(films, vec!["Movies", "Film Club"], "both servers' films, nothing else: {films:?}");

        // …and the same panel on the shows tab is the other list entirely
        set_cur(1);
        let shows: Vec<String> = source_rows().iter().map(|r| r.title.clone()).collect();
        assert_eq!(shows, vec!["TV Shows", "Their Shows"], "both servers' shows: {shows:?}");

        // the decisive property: every row the panel can activate keeps the browsed TYPE, so the
        // tab derived from it cannot move. Stated over the section each row opens, not its title.
        for r in source_rows() {
            assert_eq!(sections()[r.section].kind, SecKind::Show, "a row of another type is reachable");
        }
        reset();
    }

    /// **Reachability is a fact about NOW, and a page fetch is the only evidence that keeps
    /// arriving.** `sections_done` latches on success, so the discovery worker never asks that
    /// server anything again — without this a source that went offline an hour into the session
    /// could never stop reading as reachable, and its group would never dim. It moves in both
    /// directions, because a server that came back must stop being dimmed too.
    #[test]
    fn a_page_fetch_is_what_keeps_reachability_honest_after_discovery() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
        assert!(sources()[1].sections_done, "discovery is done — it will never re-ask by itself");

        mark_source_reachable(1, false); // a page for THEIR library did not come back
        assert!(!sources()[1].reachable, "their group dims");
        assert!(sources()[0].reachable, "…and ours is untouched — it answered");

        mark_source_reachable(1, true); // …and it comes back
        assert!(sources()[1].reachable);
        assert_eq!(sources()[1].retry_cd, 0, "a server that answered is worth re-asking at once");
        reset();
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
        seed_sources(vec![a_source("mac-mini", "", true), a_source("nas-home", "friend", true)]);
        for i in 0..2 {
            let s = source_mut(i).unwrap();
            s.sections_done = false; // both still want discovery
            s.counts_done = true;
        }
        SRC_FETCHING.store(true, Ordering::SeqCst); // as `maybe_discover` armed it before spawning

        discovery_spawn_refused(1);

        assert!(!SRC_FETCHING.load(Ordering::SeqCst), "the single flight must be released");
        assert_eq!(sources()[1].retry_cd, SRC_RETRY_CD, "…and the next attempt is ~10s out, not 1 frame");
        assert!(sources()[1].reachable, "a refused THREAD says nothing about their server");
        assert!(!sources()[1].sections_done, "…and it is still a source waiting to be discovered");
        assert_eq!(sources()[0].retry_cd, 0, "the other source is untouched");

        // one second of frames: the picker must not come back to it
        source_mut(0).unwrap().retry_cd = SRC_RETRY_CD; // so nothing in this table is dialable
        for _ in 0..60 {
            maybe_discover();
        }
        assert!(!SRC_FETCHING.load(Ordering::SeqCst), "no attempt was made in a whole second");
        assert!(sources()[1].retry_cd > 0, "…and the backoff still has most of its cooldown left");
        reset();
    }

    /// An EMPTY count landing is a failure, not an answer: the worker pushes one entry per request
    /// that succeeded. Latching `counts_done` on it would leave those rows reading their type word
    /// instead of their size for the rest of the session, with nothing able to fix it —
    /// `maybe_discover` skips a done source, and this is the bug class the module has now hit twice
    /// (the single-flight flags were the first).
    #[test]
    fn an_empty_count_landing_does_not_latch_the_probe_off() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
        if let Some(s) = source_mut(0) {
            s.counts_done = false;
        }
        let epoch = EPOCH.load(Ordering::SeqCst);

        // nothing came back
        *SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((epoch, 0, SrcLanding { name: String::new(), what: SrcWhat::Counts(Vec::new()) }));
        land_discovery();
        assert!(!sources()[0].counts_done, "an empty answer must leave the probe armed");
        assert_eq!(sections()[0].count, -1);

        // …and the real one does land, and does latch
        *SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((epoch, 0, SrcLanding { name: String::new(), what: SrcWhat::Counts(vec![(1, 185)]) }));
        land_discovery();
        assert!(sources()[0].counts_done);
        assert_eq!(sections()[0].count, 185, "the row can say \"185 films\" now");
        reset();
    }

    /// With one source the projection is the identity map, which is what makes a single-server
    /// install draw exactly the strip it always did — and the Source chip absent, not empty.
    #[test]
    fn one_source_leaves_the_strip_exactly_as_it_was() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true)]);
        append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
        assert_eq!(tab_count(), section_count());
        for i in 0..section_count() {
            assert_eq!(tab_section(i), i);
            assert_eq!(tab_of_section(i), i);
            assert_eq!(tab_title(i), section_title(i));
        }
        assert_eq!(sources().len(), 1, "…and the Source chip's own condition is false");
        reset();
    }

    /// A failure landing from a SUPERSEDED query must not blame the current one: the user has
    /// already changed sort/filter/section, a fresh fetch is on its way, and marking the new query
    /// Failed would show a failure read-out over a listing that is still perfectly healthy.
    #[test]
    fn a_stale_failure_landing_does_not_blame_the_current_query() {
        let _g = crate::testlock::serial();
        seed_one_section();
        let stale = GEN.load(Ordering::SeqCst);
        bump_gen(); // the query moved on under the in-flight fetch
        let r = PageResult { gen: stale, sec: 0, start: 0, items: Vec::new(), total: -1, sorts: None };
        *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        pump();
        assert_eq!(fetch_state(), SecFetch::Loading, "the current query has not answered yet — it has not failed");
        reset();
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
        seed_one_section();
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
        unsafe {
            let st = &mut *addr_of_mut!(STATES);
            *st = vec![SecState::default(), SecState::default()];
            // the section on screen, the one browsed a minute ago (which keeps its items), and a
            // FRIEND's row carrying the same key — both servers number their items from 1
            st[0].items = vec![row(sid, "7", 90_000), row(other, "7", 0)];
            st[1].items = vec![row(sid, "7", 0), row(sid, "9", 0)];
        }

        assert!(set_watched_local(sid, "7", true), "the item is in the store, so the edit lands");
        let watched = |sec: usize, i: usize| {
            let st = unsafe { &*addr_of!(STATES) };
            let m = st[sec].items[i].as_ref().unwrap();
            (m.watched, m.unwatched, m.resume_ms)
        };
        assert_eq!(watched(0, 0), (true, false, 0), "…tick on, and the resume bar retires with it");
        assert_eq!(watched(1, 0), (true, false, 0), "…in a section that is not the one being browsed");
        assert_eq!(watched(0, 1), (false, true, 0), "…and never on the friend's item with the same key");
        assert_eq!(watched(1, 1), (false, true, 0), "…nor on an item that was not asked about");

        assert!(!set_watched_local(sid, "404", true), "an item in no section reports a miss");
        reset();
    }
}
