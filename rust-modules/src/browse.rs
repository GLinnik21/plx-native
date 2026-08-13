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
use crate::plex::SectionQuery;
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// Page size for section listings. Two grid screens' worth (10 rows × 6) — big enough that a
/// full-screen scroll rarely waits, small enough that a page parse stays invisible on-frame.
const PAGE: usize = 60;

// ---- section table (discovered once) -------------------------------------------------------

/// One browsable library section (movie or show), from `GET /library/sections`.
pub(crate) struct BrowseSection {
    pub(crate) key: i64,
    pub(crate) title: String,
    pub(crate) is_show: bool,
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

static mut SECTIONS: Vec<BrowseSection> = Vec::new();
static mut STATES: Vec<SecState> = Vec::new();
static mut CUR: usize = 0;
/// Wanted item-index range (inclusive lo, exclusive hi) — set by the grid each frame from its
/// visible rows + lookahead; [`pump`] fetches the first missing page inside it.
static mut WANT: (usize, usize) = (0, 0);

fn sections() -> &'static Vec<BrowseSection> {
    unsafe { &*addr_of!(SECTIONS) }
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
/// Bumped whenever the section TABLE is rebuilt ([`ensure_sections`]/[`reset`]) — landings
/// keyed by section INDEX are only applied when their table generation still matches, and
/// the tab-row label cache invalidates on it.
static SECTIONS_GEN: AtomicU32 = AtomicU32::new(0);
static FETCHING: AtomicBool = AtomicBool::new(false);
static GENRE_FETCHING: AtomicBool = AtomicBool::new(false);
static LETTERS_FETCHING: AtomicBool = AtomicBool::new(false);
/// Every single-flight flag, in one place. These are cleared ONLY inside a successful mailbox
/// take, so [`reset`] — which drops the mailboxes — must clear them too or the fetch stays
/// latched forever and the screen wedges on a spinner. **Add a new flag here, not just above**,
/// and `reset` picks it up for free.
const IN_FLIGHT: [&AtomicBool; 3] = [&FETCHING, &GENRE_FETCHING, &LETTERS_FETCHING];
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
// menu-data landings carry the section-table generation so a landing spawned before a
// [`reset`] (profile switch) can never populate the NEW user's state at the same index
static GENRE_RESULT: Mutex<Option<(u32, usize, Vec<GenreEntry>)>> = Mutex::new(None);
static LETTER_RESULT: Mutex<Option<(u32, usize, Vec<(String, i64)>)>> = Mutex::new(None);

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
    *PAGE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *GENRE_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *LETTER_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // Dropping a mailbox without clearing its flag latches the fetch forever (the flag is only
    // cleared on a successful take), so the two must move together.
    for f in IN_FLIGHT {
        f.store(false, Ordering::SeqCst);
    }
    unsafe {
        *addr_of_mut!(SECTIONS) = Vec::new();
        *addr_of_mut!(STATES) = Vec::new();
        CUR = 0;
        RETRY_CD = 0;
    }
}

/// The section-table generation — bumped by [`reset`]/[`ensure_sections`]; the tab-row label
/// cache keys on it.
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

/// Discover the movie/show sections (once; blocking — one small GET, same boot-fetch budget as
/// `pms_fetch_hubs`). Safe to call every Library entry; later calls are free. Returns count.
pub(crate) fn ensure_sections() -> usize {
    if !sections().is_empty() {
        return sections().len();
    }
    let found = catch_unwind(|| {
        let mut v: Vec<BrowseSection> = Vec::new();
        if let Some(client) = crate::plex::client_opt() {
            if let Some(mc) = client.sections() {
                for d in &mc.directory {
                    let is_show = match d.kind.as_str() {
                        "movie" => false,
                        "show" => true,
                        _ => continue, // music/photo: not browsable here
                    };
                    if let Ok(key) = d.key.parse::<i64>() {
                        v.push(BrowseSection { key, title: d.title.clone(), is_show });
                    }
                }
            }
        }
        v
    })
    .unwrap_or_default();
    let states: Vec<SecState> = found.iter().map(|_| SecState::default()).collect();
    SECTIONS_GEN.fetch_add(1, Ordering::SeqCst);
    unsafe {
        *addr_of_mut!(SECTIONS) = found;
        *addr_of_mut!(STATES) = states;
        CUR = 0;
    }
    sections().len()
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
pub(crate) fn cur() -> usize {
    unsafe { CUR }.min(sections().len().saturating_sub(1))
}
/// Switch section tab. Keeps the target section's remembered query/view; only re-fetches
/// when its store is empty.
pub(crate) fn set_cur(i: usize) {
    if i >= sections().len() || i == cur() {
        return;
    }
    unsafe { CUR = i };
    bump_gen(); // discard any in-flight page for the previous section
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
    if states().get(c).is_none() || done || flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let key = sections()[c].key;
    let sgen = sections_gen();
    let spawned = crate::task::spawn_small("directory", move || {
        let list = catch_unwind(|| {
            let mut v = Vec::new();
            if let Some(client) = crate::plex::client_opt() {
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
/// apply to the section's state iff the section table hasn't been rebuilt underneath it.
fn land_directory<T>(
    flag: &'static AtomicBool,
    mail: &'static Mutex<Option<(u32, usize, Vec<T>)>>,
    apply: impl FnOnce(&'static mut SecState, Vec<T>),
) {
    if let Some((sgen, sec, list)) = mail.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // a menu's value list arriving repopulates an open Sort/Filter popover (`ui::idle`)
        crate::ui::idle::invalidate();
        flag.store(false, Ordering::SeqCst);
        if sgen == sections_gen() {
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

// ---- pump: mailbox apply + next-fetch scheduling (main thread, once a frame) ----------------

/// Returns true when new items just landed (the grid re-clamps focus on it).
pub(crate) fn pump() -> bool {
    let mut changed = false;
    unsafe {
        if RETRY_CD > 0 {
            RETRY_CD -= 1;
        }
    }
    // menu-data landings (query-independent; sgen-gated inside land_directory so a pre-reset
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
            let mc = crate::plex::client_opt().and_then(|cl| cl.section_items_query(&q));
            let Some(mc) = mc else {
                return (Vec::new(), -1i64, None); // FAILURE sentinel — pump leaves the store alone
            };
            let items: Vec<PmsMovie> = mc.metadata.iter().map(parse_item).collect();
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
