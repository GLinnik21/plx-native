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
    /// The MACHINE name ("bx23-ldn") — the Sources list's group header, and the only place in the
    /// app a machine is named. Learned from the roster, else from the server naming itself
    /// (`Client::friendly_name`); `""` until one of those lands.
    pub(crate) name: String,
    /// The owner's plex.tv handle ("bamx23"); **empty on your own server**, where the absence of an
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
    pub(crate) is_show: bool,
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
/// section is Failed with items still on screen, which is why the screen's read-out projects the
/// pair (see [`failed_initial`]) rather than the state alone — the same rule `pms::HubState` and
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
    Sections(Option<Vec<(i64, String, bool)>>),
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
        *addr_of_mut!(TABS) = Vec::new();
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
                    }
                }
            }
            None => unsafe {
                let f = crate::plex::server_facts(sid);
                let (name, handle) = f.map(|f| (f.name.clone(), f.handle.clone())).unwrap_or_default();
                (*addr_of_mut!(SOURCES)).push(BrowseSource {
                    sid,
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

/// `MediaContainer.Directory[]` → the (key, title, is_show) rows this app can browse. The ONE
/// projection, shared by the blocking discovery above and the worker below, so the two can never
/// disagree about which sections exist.
fn project_sections(mc: &crate::plex::MediaContainer) -> Vec<(i64, String, bool)> {
    mc.directory
        .iter()
        .filter_map(|d| {
            let is_show = match d.kind.as_str() {
                "movie" => false,
                "show" => true,
                _ => return None, // music/photo: not browsable here
            };
            d.key.parse::<i64>().ok().map(|k| (k, d.title.clone(), is_show))
        })
        .collect()
}

/// APPEND one source's sections to the table, with their per-section states in lockstep.
///
/// Append, never rebuild — see this module's header. Existing indices (and therefore `CUR`, every
/// remembered view, and every in-flight page landing's `sec`) survive untouched, which is what
/// makes a source arriving late safe at all. A source is only ever appended once, so a repeat call
/// for it is a no-op rather than a duplicated library.
fn append_sections(src: usize, list: Vec<(i64, String, bool)>) {
    // Only what this source does not already have. A re-discovery ("Check for new shares", or a
    // server that came back) therefore ADDS a library the owner has since created and leaves every
    // existing row — and every index — exactly where it was.
    let fresh: Vec<(i64, String, bool)> = list
        .into_iter()
        .filter(|(k, _, _)| !sections().iter().any(|s| s.src == src && s.key == *k))
        .collect();
    if fresh.is_empty() {
        return;
    }
    // Your own libraries feed Home; a friend's do not until you say so (the design's first-run
    // state). `handle` is the roster's own answer to "is this someone else's".
    let pinned = sources().get(src).map(|s| s.handle.is_empty()).unwrap_or(true);
    unsafe {
        let secs = &mut *addr_of_mut!(SECTIONS);
        let states = &mut *addr_of_mut!(STATES);
        for (key, title, is_show) in fresh {
            secs.push(BrowseSection { src, key, title, is_show, count: -1, pinned });
            states.push(SecState::default());
        }
    }
    SECTIONS_GEN.fetch_add(1, Ordering::SeqCst);
    crate::ui::idle::invalidate(); // a new tab pill / Sources row appears under a settled screen
}

pub(crate) fn section_count() -> usize {
    sections().len()
}
pub(crate) fn section_title(i: usize) -> &'static str {
    sections().get(i).map(|s| s.title.as_str()).unwrap_or("")
}
pub(crate) fn section_is_show(i: usize) -> bool {
    sections().get(i).map(|s| s.is_show).unwrap_or(false)
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
fn activate_source_of(i: usize) {
    let Some(sid) = section_sid(i) else { return };
    if sid == crate::plex::current_server() || !crate::plex::set_current(sid) {
        return;
    }
    crate::log(&format!("browse: current server is now source {}", section_src(i)));
    crate::pms::reset(); // Home's catalog is the OLD server's — drop it and re-arm the fetch
    crate::person::reset();
    crate::route::forget_server_identity(); // the PlayQueue's machineIdentifier was the old one
}
/// The source index of section `i` — the server half of its address.
fn section_src(i: usize) -> usize {
    sections().get(i).map(|s| s.src).unwrap_or(0)
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

fn tabs() -> &'static Vec<usize> {
    let g = sections_gen();
    if unsafe { addr_of!(TABS_GEN).read() } != g {
        let owned_kinds: Vec<bool> = sections()
            .iter()
            .filter(|s| sources().get(s.src).map(|x| x.handle.is_empty()).unwrap_or(true))
            .map(|s| s.is_show)
            .collect();
        let v: Vec<usize> = sections()
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let owned = sources().get(s.src).map(|x| x.handle.is_empty()).unwrap_or(true);
                owned || !owned_kinds.contains(&s.is_show)
            })
            .map(|(i, _)| i)
            .collect();
        unsafe {
            *addr_of_mut!(TABS) = v;
            TABS_GEN = g;
        }
    }
    unsafe { &*addr_of!(TABS) }
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
    let kind = section_is_show(s);
    t.iter().position(|&i| section_is_show(i) == kind).unwrap_or(0)
}

// ---- pinning: the ONE control, and it governs Home only --------------------------------------

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
    crate::ui::idle::invalidate();
    true
}
/// Every library that feeds Home, as (source index, section index).
///
/// No caller yet: this is the READ side of the pin store, and Home's multi-source shelf assembly
/// (the design's deliverable C) is the one that wants it. Kept here rather than added later
/// because the pin is written here and a setting with no reader is easier to spot than one whose
/// reader disagrees about where it lives.
/// Browsing ignores it entirely, because browsing is governed by the grant.
#[allow(dead_code)]
pub(crate) fn pinned_libraries() -> Vec<(usize, usize)> {
    sections().iter().enumerate().filter(|(_, s)| s.pinned).map(|(i, s)| (s.src, i)).collect()
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

pub(crate) fn source_rows() -> Vec<SrcRow> {
    let last = pinned_count() == 1;
    let cur = cur();
    sections()
        .iter()
        .enumerate()
        .map(|(i, s)| SrcRow {
            src: s.src,
            section: i,
            title: s.title.clone(),
            count_line: count_line(s.count, s.is_show),
            pinned: s.pinned,
            last_pinned: last && s.pinned,
            current: i == cur,
        })
        .collect()
}

/// A library's sub-line: its size once the count has landed, else its TYPE. Never absent — a row
/// that loses its second line changes height, and the panel is not allowed to resize under a level
/// switch or a landing.
fn count_line(count: i64, is_show: bool) -> String {
    if count >= 0 {
        format!("{count} {}", if is_show { "shows" } else { "films" })
    } else {
        (if is_show { "TV shows" } else { "Films" }).to_string()
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
/// The grid's desired index window (visible + lookahead) — drives which page fetches next.
pub(crate) fn want(lo: usize, hi: usize) {
    unsafe { WANT = (lo, hi) };
}
/// What the current section's last page fetch did — the Library screen's read-out is a projection
/// of this and the store, never of the store alone.
pub(crate) fn fetch_state() -> SecFetch {
    cur_state().map(|s| s.fetch).unwrap_or(SecFetch::Loading)
}
/// True while the current query has no data yet (first page in flight) — the grid's
/// full-screen spinner state.
///
/// Reads the STATE, not `total < 0`: those two agreed for every path that ends in an answer, and
/// disagreed for the one that doesn't. A failed first page leaves `total` at -1 forever, so this
/// used to spin forever with it.
pub(crate) fn loading_initial() -> bool {
    fetch_state() == SecFetch::Loading
}
/// The failure the SCREEN can see: the last fetch failed **and** there is nothing to show for this
/// query. A mid-scroll page failure on a populated section is deliberately not this — the grid
/// still has its items, `RETRY_CD` is already counting, and blanking a working screen over one
/// missing page is the bug the failure branch of [`pump`] exists to avoid.
pub(crate) fn failed_initial() -> bool {
    cur_state().map(|s| s.fetch == SecFetch::Failed && s.total < 0).unwrap_or(false)
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
        // the flag is cleared only inside a successful mailbox take, and nothing will fill that
        // mailbox — the `reset_clears_the_single_flight_flags_with_the_mailboxes` latch again
        SRC_FETCHING.store(false, Ordering::SeqCst);
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
        // …and back into the registry, so anything else that asks about this server gets the same
        // answer without a second `GET /`. `owned` is carried through unchanged: this describer
        // learned a name, not a grant.
        if let Some(s) = sources().get(si) {
            let owned = s.handle.is_empty();
            crate::plex::describe_server(s.sid, &name, "", owned);
        }
    }
    match what {
        SrcWhat::Sections(list) => {
            let ok = list.is_some();
            append_sections(si, list.unwrap_or_default());
            if let Some(s) = source_mut(si) {
                s.sections_done = ok;
                s.reachable = ok;
                s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
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
            if let Some(s) = source_mut(si) {
                s.counts_done = ok;
                s.retry_cd = if ok { 0 } else { SRC_RETRY_CD };
            }
        }
    }
}

// ---- pump: mailbox apply + next-fetch scheduling (main thread, once a frame) ----------------

/// Returns true when new items just landed (the grid re-clamps focus on it).
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
    if st.unwatched {
        // shows advertise unwatchedLeaves (any unwatched episode); plain unwatched=1 has odd
        // semantics on type=2 (verified live 2026-07-19)
        let k = if sec.is_show { "unwatchedLeaves" } else { "unwatched" };
        filters.push((k.to_string(), "1".to_string()));
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
        assert!(failed_initial(), "…and the screen must be able to see that it failed");
        reset();
    }

    /// A served page is Ready, and stays the plain "here are your items" state.
    #[test]
    fn a_served_page_leaves_the_section_ready() {
        let _g = crate::testlock::serial();
        seed_one_section();
        land_page(3, 3);
        assert_eq!(fetch_state(), SecFetch::Ready);
        assert!(!loading_initial() && !failed_initial());
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
        assert!(!failed_initial(), "an empty library is an answer, not a fault");
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
        assert!(loading_initial() && !failed_initial());
        reset();
    }

    // ---- the (source, section) table ------------------------------------------------------------
    //
    // These seed SOURCES directly and mark every phase done, so `maybe_discover` picks nothing and
    // no worker is spawned — the same discipline as the fetch-machine tests above, one layer up.
    // Their `sid` is `UNSET`, which resolves to no client, so even a spawn could reach no socket.

    fn a_source(name: &str, handle: &str, reachable: bool) -> BrowseSource {
        BrowseSource {
            sid: ServerId::UNSET,
            name: name.into(),
            handle: handle.into(),
            reachable,
            sections_done: true,
            counts_done: true,
            retry_cd: 0,
        }
    }
    fn seed_sources(srcs: Vec<BrowseSource>) {
        reset();
        unsafe { *addr_of_mut!(SOURCES) = srcs };
    }

    /// THE reason the table gained a source dimension. Measured against the real share on
    /// 2026-08-11: both servers have a section `1`, and they are different libraries. A bare key
    /// names two things, so every row carries its source and the two rows coexist.
    #[test]
    fn two_servers_both_have_a_section_one_and_the_table_tells_them_apart() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("bx23-ldn", "bamx23", true)]);
        append_sections(0, vec![(1, "Movies".into(), false), (2, "TV Shows".into(), true)]);
        append_sections(1, vec![(1, "LDN Films".into(), false)]);

        assert_eq!(section_count(), 3);
        let ours = &sections()[0];
        let theirs = &sections()[2];
        assert_eq!((ours.key, ours.src), (1, 0), "our section 1, on source 0");
        assert_eq!((theirs.key, theirs.src), (1, 1), "THEIR section 1 — same key, different source");
        assert_eq!((section_title(0), section_title(2)), ("Movies", "LDN Films"));
        // and the chip's annotation follows the section being browsed, not the account
        assert_eq!(handle_of(2), "bamx23");
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
        seed_sources(vec![a_source("mac-mini", "", true), a_source("bx23-ldn", "bamx23", true)]);
        append_sections(0, vec![(1, "Movies".into(), false), (2, "TV Shows".into(), true)]);
        set_cur(1);
        let before = (cur(), section_title(1).to_string(), states().len());

        append_sections(1, vec![(1, "LDN Films".into(), false)]);
        assert_eq!(cur(), before.0, "the library being browsed is still the one at that index");
        assert_eq!(section_title(1), before.1);
        assert_eq!(states().len(), section_count(), "states stay in lockstep with the table");
        assert_eq!(states().len(), before.2 + 1);

        // A RE-discovery ("Check for new shares", or a server that came back) re-offers the same
        // list: every row is already there, so nothing is duplicated and nothing moves…
        append_sections(1, vec![(1, "LDN Films".into(), false)]);
        assert_eq!(section_count(), 3);
        assert_eq!(cur(), before.0);
        // …while a library the owner has CREATED since is appended, at the end, where it cannot
        // disturb an index anything is already holding.
        append_sections(1, vec![(1, "LDN Films".into(), false), (4, "LDN Shows".into(), true)]);
        assert_eq!(section_count(), 4);
        assert_eq!(section_title(3), "LDN Shows");
        assert_eq!(section_title(1), before.1, "and the row we were browsing is untouched");
        reset();
    }

    /// The pin defaults and the one rule the control has. Your own libraries feed Home, a friend's
    /// do not until you say so — and the LAST pinned library cannot be turned off, because Home
    /// with nothing on it is the only real failure this setting has.
    #[test]
    fn a_friends_libraries_start_unpinned_and_the_last_pin_cannot_be_turned_off() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("bx23-ldn", "bamx23", true)]);
        append_sections(0, vec![(1, "Movies".into(), false), (2, "TV Shows".into(), true)]);
        append_sections(1, vec![(1, "LDN Films".into(), false)]);
        assert_eq!((pinned(0), pinned(1), pinned(2)), (true, true, false));
        assert_eq!(pinned_count(), 2);

        assert!(toggle_pin(2), "a friend's library can be pinned");
        assert_eq!(pinned_count(), 3);
        assert!(toggle_pin(0) && toggle_pin(1), "your own can be unpinned — a preference, not a mistake");
        assert_eq!(pinned_count(), 1);

        assert!(is_last_pinned(2));
        assert!(!toggle_pin(2), "the last pinned library is refused");
        assert!(pinned(2), "…and refused means UNCHANGED, not toggled twice");
        assert_eq!(pinned_count(), 1);
        reset();
    }

    /// **A pill is a TYPE, never a person.** The strip names your own libraries; a friend's film
    /// library gets no pill of its own (the toolbar chip under it says whose), but a type only they
    /// have does — otherwise that content is unreachable from the strip at all. And the selection
    /// capsule for a borrowed library rests on its TYPE's pill, so nothing is ever homeless.
    #[test]
    fn the_tab_strip_grows_by_types_and_never_by_people() {
        let _g = crate::testlock::serial();
        seed_sources(vec![a_source("mac-mini", "", true), a_source("bx23-ldn", "bamx23", true)]);
        append_sections(0, vec![(1, "Movies".into(), false)]);
        append_sections(1, vec![(1, "LDN Films".into(), false), (2, "Their Shows".into(), true)]);

        assert_eq!(tab_count(), 2, "your Movies, plus the shows nobody of yours provides");
        assert_eq!((tab_title(0), tab_title(1)), ("Movies", "Their Shows"));
        assert_eq!(tab_section(1), 2);
        assert_eq!(tab_of_section(1), 0, "their films ride YOUR Movies pill — same type, one level");
        assert_eq!(tab_of_section(2), 1, "their shows have a pill of their own");
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
        seed_sources(vec![a_source("mac-mini", "", true), a_source("bx23-ldn", "bamx23", true)]);
        append_sections(0, vec![(1, "Movies".into(), false)]);
        append_sections(1, vec![(1, "LDN Films".into(), false)]);
        assert!(sources()[1].sections_done, "discovery is done — it will never re-ask by itself");

        mark_source_reachable(1, false); // a page for THEIR library did not come back
        assert!(!sources()[1].reachable, "their group dims");
        assert!(sources()[0].reachable, "…and ours is untouched — it answered");

        mark_source_reachable(1, true); // …and it comes back
        assert!(sources()[1].reachable);
        assert_eq!(sources()[1].retry_cd, 0, "a server that answered is worth re-asking at once");
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
        append_sections(0, vec![(1, "Movies".into(), false)]);
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
        append_sections(0, vec![(1, "Movies".into(), false), (2, "TV Shows".into(), true)]);
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
}
