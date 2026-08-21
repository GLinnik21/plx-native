//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie()/hub_item()/hero_pool_item(), plus urlenc_str (shared by posters/route).
//! The fetch + JSON parse go through the typed `crate::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
use crate::plex::ServerId;
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// Catalog rows Home holds at most, across EVERY source. A hard ceiling on the store the whole
/// screen indexes into, not a per-server one — see [`allot`] for how the sources divide it.
const PMS_MAX_MOVIES: usize = 256;

/// Shelves Home holds at most, across every source — the number of rows the grid can actually
/// address (`ui::home::MAX_HUBS` is this constant, so the budget can never quietly stop matching
/// the array it is budgeting for).
///
/// It matters far more with two servers than it ever did with one. The truncation is in SHELF
/// ORDER, so a single greedy source — `/hubs` promotes several rows per library, and four
/// libraries already overrun this — used to be able to spend the entire allowance before the next
/// source got a row. That is starvation, and it is what [`allot`] exists to prevent.
pub(crate) const MAX_SHELVES: usize = 16;

/// Cards one shelf holds at most — the number the grid can address (`ui::home::MAX_ITEMS` is this
/// constant, for the same reason [`MAX_SHELVES`] is).
///
/// It was unreachable with one server, because `/hubs?count=12` bounds every shelf at 12. The
/// MERGED deck is what reaches it: three sources' Continue Watching is up to 36 cards, and
/// `ui::home`'s focus ring and its OK dispatch clamp differently past this number — the ring stops
/// at the last addressable card while the press opens whatever column the raw index names. Cap the
/// data and the two can never disagree.
pub(crate) const MAX_SHELF_ITEMS: usize = 24;

/// Items asked of each hub endpoint, per source. `/hubs?count=` is items-per-hub, so this bounds
/// a shelf, never the number of shelves.
const HUB_FETCH_COUNT: i64 = 12;

/// A catalog row — owned strings (the old C-ABI fixed `[u8; N]` buffers are gone; no C
/// consumer remains). Fields pub(crate) so the UI / route / player read them directly.
#[derive(Default, Clone)]
pub struct PmsMovie {
    /// WHICH SERVER this row came from. Every other identity on it — `rk`, `show_rk`, `part` — is a
    /// server-local key that a second server reuses from 1 (docs/shared-servers.md §2 measured the
    /// collision), so the row is only addressable as the PAIR `(sid, rk)`; see
    /// [`crate::plex::same_item`]. Stamped by [`parse_item`] from a value the SPAWNING thread
    /// captured, never from `plex::current_server()` inside the worker — `parse_item` runs on the
    /// hub, page and person workers, and by the time one of them parses, "the current server" may
    /// already be a different machine than the one whose bytes it is holding.
    pub(crate) sid: ServerId,
    /// The LIBRARY on `sid` this row came from (`librarySectionID`), 0 when the server sent none.
    /// The pin's grain: a whole-server `/hubs` answers with rows from every library, so this is the
    /// only thing that can keep an UNPINNED library's items off Home without a per-library fetch.
    pub(crate) sec: i64,
    pub(crate) title: String,
    pub(crate) year: c_int,
    pub(crate) rating: String,
    pub(crate) dur_ns: i64,
    pub(crate) part: String,
    pub(crate) thumb: String,
    /// The item's OWN thumb where [`PmsMovie::thumb`] holds a substitute — i.e. an episode's 16:9
    /// still, empty on everything else. A landscape tile draws this; a portrait card draws `thumb`.
    pub(crate) still: String,
    pub(crate) art: String,
    pub(crate) summary: String,
    pub(crate) rk: String,
    pub(crate) vcodec: String,
    pub(crate) acodec: String,
    pub(crate) blur: [[f32; 3]; 4],
    pub(crate) has_blur: bool,
    pub(crate) kind: c_int,    // 0 = movie, 1 = show, 2 = season, 3 = episode
    pub(crate) resume_ms: i64, // viewOffset — drives the Continue Watching resume bar
    pub(crate) show_rk: String, // parent show rk (episode: grandparent; season: parent)
    pub(crate) season_index: c_int, // season number (episode: parentIndex; season: index)
    pub(crate) show_title: String, // episode only: grandparentTitle (the hero headlines the SHOW)
    pub(crate) ep_index: c_int,    // episode only: episode number within the season
    /// Fully unwatched (movie/episode: no viewCount; show/season: zero viewed leaves).
    pub(crate) unwatched: bool,
    /// Fully **watched** — and deliberately NOT `!unwatched`, which is the trap this field exists to
    /// close. For a movie or episode the two are the same thing, but for a SHOW or SEASON
    /// `!unwatched` only means "at least one episode has been played", so a series you are three
    /// episodes into satisfies it. The tile mark is a claim of DONE (`ui::widgets::poster_mark`), so
    /// it needs `viewedLeafCount >= leafCount` instead: partly-watched sits with never-started under
    /// "no mark", because the honest statement about a show mid-run is the resume state of its next
    /// episode, which a poster in a grid does not have. Caught by a device capture — a library
    /// filtered to `unwatchedLeaves=1` had five tiles wearing a watched disc.
    ///
    /// The comparison is the house rule, not a new one: it is `metadata::Season::watched`'s, and the
    /// same one `fetch_detail` applies to a show — including the load-bearing `leaf_count > 0` half,
    /// without which a container the server sent no counts for is `0 >= 0` and reads as watched.
    pub(crate) watched: bool,
}

impl PmsMovie {
    /// Played fraction for the amber resume bar, or None when not in progress — THE one
    /// resume-bar rule, shared by the home shelves and the Library grid (it was copy-pasted
    /// into both screens before), and the definition of `PosterMark::InProgress`.
    ///
    /// **A resume point at or past the end is NOT in progress.** That is a finished item whose
    /// `viewOffset` the server never cleared, and counting it as in-progress drew a 100%-full bar
    /// that read as a rendering bug — and, once the poster's mark became the watched disc
    /// (2026-08-13), also suppressed the disc that item should be wearing, so a finished movie could
    /// end up with a full bar and no check. `ui::detail::ep_state` has always applied this rule to
    /// an episode still; now a poster and the filmstrip beside it cannot describe one item two ways.
    pub(crate) fn resume_frac(&self) -> Option<f32> {
        (self.resume_ms > 0 && self.dur_ns > 0 && self.resume_ms * 1_000_000 < self.dur_ns)
            .then(|| (self.resume_ms as f32 * 1_000_000.0 / self.dur_ns as f32).clamp(0.0, 1.0))
    }
}

// The catalog (private; the UI reads it through movie()/hub_item()/hero_pool_item()).
// Main-thread only, like every UI static; rebuilt wholesale by pms_fetch_hubs.
static mut CATALOG: Vec<PmsMovie> = Vec::new();

fn catalog() -> &'static Vec<PmsMovie> {
    unsafe { &*std::ptr::addr_of!(CATALOG) }
}

/// catalog row `i`, or None. The reference stays valid until the next refetch (main-thread
/// only — the same lifetime discipline the old raw `movie_ptr` had, now bounds-checked;
/// [`commit`] is the one mutation and re-resolves the open surfaces itself).
pub(crate) fn movie(i: usize) -> Option<&'static PmsMovie> {
    catalog().get(i)
}
/// Catalog index of the row `(sid, rk)` names, or -1.
///
/// **Server-scoped, and that is the whole point.** This used to scan `m.rk == rk` over one flat
/// catalog, which is unambiguous only while every row comes from one machine. On a Continue
/// Watching shelf merged across servers it is not: a friend's episode and one of ours can carry the
/// same ratingKey, so a bare-key scan returns whichever row is EARLIER — and the caller that
/// exposed it was `detail::mount_rk` (which then mounts the wrong backdrop, blur envelope and
/// selection). -1 stays "not in the hub catalog", which every caller already handles as
/// "off-catalog".
///
/// The item menu's Play-from-Start was the other caller and is not one any more: it carries the row
/// it was opened on (`ui::item_menu::ITEM`), because a Library, Search or person-page tile is in no
/// hub at all and this answered -1 for every one of them.
pub(crate) fn index_of_rk(sid: ServerId, rk: &str) -> c_int {
    catalog()
        .iter()
        .position(|m| crate::plex::same_item((m.sid, &m.rk), (sid, rk)))
        .map(|i| i as c_int)
        .unwrap_or(-1)
}

// ---- helpers ----
/// owned copy of a metadata string with newlines flattened to spaces (single-line UI fields)
fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

/// percent-encode into a String (Rust callers, e.g. posters::poster_key)
pub(crate) fn urlenc_str(src: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(src.len());
    for &ch in src.as_bytes() {
        if ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.' | b'~') {
            out.push(ch as char);
        } else {
            out.push('%');
            out.push(HEX[(ch >> 4) as usize] as char);
            out.push(HEX[(ch & 15) as usize] as char);
        }
    }
    out
}

/// Parse one Plex `Metadata` item (from a section listing OR a hub) into a catalog row.
/// pub(crate): the Library browse store (`browse.rs`) and the person page (`person.rs`) map their
/// listings with it too.
///
/// `sid` is the server the response came from and is **passed in, never looked up**: all three
/// callers run this on a worker thread, and the house rule (`browse.rs`'s spawn site states it
/// outright) is that a worker reads no statics. It is also the only correct answer — the current
/// server can change while a page fetch is in flight, and the rows in hand belong to the machine
/// that was asked, not to whichever one is current when they finish parsing.
pub(crate) fn parse_item(it: &crate::plex::Metadata, sid: ServerId) -> PmsMovie {
    let mut m = PmsMovie { sid, sec: it.library_section_id, ..Default::default() };
    m.kind = match it.kind.as_str() {
        "show" => 1,
        "season" => 2,
        "episode" => 3,
        _ => 0,
    };
    match m.kind {
        3 => {
            // episode: parent show = grandparent, season number = parentIndex
            m.show_rk = clean(&it.grandparent_rating_key);
            m.season_index = it.parent_index as c_int;
            m.show_title = clean(&it.grandparent_title);
            m.ep_index = it.index as c_int;
        }
        2 => {
            // season: parent show = parent, season number = index
            m.show_rk = clean(&it.parent_rating_key);
            m.season_index = it.index as c_int;
        }
        _ => {}
    }
    // shows/seasons count leaves (a show with any watched episode is no longer "unwatched");
    // movies/episodes key on viewCount absence (docs/pms-api.md §2)
    m.unwatched = match m.kind {
        1 | 2 => it.viewed_leaf_count == 0 && it.leaf_count > 0,
        _ => it.view_count == 0,
    };
    // …and DONE is its own question, not the negation of that one: for a container it takes ALL the
    // leaves, so a show three episodes in is neither (see the `watched` field's doc).
    m.watched = match m.kind {
        1 | 2 => it.leaf_count > 0 && it.viewed_leaf_count >= it.leaf_count,
        _ => it.view_count > 0,
    };
    m.title = clean(&it.title);
    m.year = it.year as c_int;
    m.rating = clean(&it.content_rating);
    m.dur_ns = if it.duration > 0 { it.duration * 1_000_000 } else { 0 };
    m.resume_ms = it.view_offset;
    // poster: prefer the show poster for episodes (grandparentThumb) so a landscape
    // episode still doesn't fill a portrait card
    let thumb = if it.grandparent_thumb.is_empty() { &it.thumb } else { &it.grandparent_thumb };
    m.thumb = clean(thumb);
    // …and the item's OWN thumb, unsubstituted. The line above is right for a POSTER shelf and
    // wrong for a landscape one, and both exist: an episode's own thumb is a 16:9 still, so Home's
    // portrait cards want the show poster, while a 420x236 tile wants the still — with the
    // substitution applied, a search for a show drew the same fanart on every episode in the row.
    //
    // Kept as a second field rather than resolved per caller because `parse_item` runs on a worker
    // and cannot know which shelf will draw the row. Empty on a movie, where `thumb` already IS
    // the item's own.
    m.still = if it.grandparent_thumb.is_empty() { String::new() } else { clean(&it.thumb) };
    m.art = clean(&it.art);
    m.summary = clean(&it.summary);
    m.rk = clean(&it.rating_key);
    // Media[0]: codecs + Part[0].key (movies/episodes; a show container has none)
    if let Some(md) = it.media.first() {
        m.vcodec = clean(&md.video_codec);
        m.acodec = clean(&md.audio_codec);
        if let Some(p0) = md.part.first() {
            m.part = clean(&p0.key);
        }
    }
    // UltraBlurColors -> the ambient gradient. `UltraBlurColors::corners` owns the corner ORDER and
    // the all-black-envelope guard (shared with the detail store, which keys the same wash off the
    // LOADED item); `de_ultrablur` already accepted both the array and object shapes PMS returns
    // (D-1), so blur populates where the old object-only read left it blank.
    if let Some(blur) = it.ultra_blur_colors.and_then(|u| u.corners()) {
        m.blur = blur;
        m.has_blur = true;
    }
    m
}

// The full-library browse path lives in `crate::browse` (the Library screen's per-section
// PAGED catalog — sparse store + off-thread page fetches via `section_items_query`). This
// module stays hub-only; `browse` reuses `parse_item` above for its listings.

// ---- home hubs: each hub is a titled slice of the catalog ----
struct HubRow {
    title: String,
    hub_id: String, // locale-independent hubIdentifier ("home.continue", "home.movies.recent", …)
    /// Which SERVER this shelf's items came from, as the owner's handle ("friend") — empty
    /// whenever the row came from the signed-in user's own server, which is every row today.
    /// Empty is the ABSENCE of an annotation, not an empty one: the home shelf heading draws no
    /// separator and no second run at all for it (`ui::home::heading_flow`), so the annotation costs
    /// a single-server library nothing — no gap, no dot, no draw call. (The heading's INK changed in
    /// the same pass, which is a separate, deliberate harmonization; `heading_flow`'s doc has it.)
    /// Populated by the multi-server data layer when it lands.
    source: String,
    start: usize,
    len: usize,
}
static mut HUBS: Vec<HubRow> = Vec::new();

fn hubs() -> &'static Vec<HubRow> {
    unsafe { &*std::ptr::addr_of!(HUBS) }
}

// ---- rotating hero pool: curated catalog indices (Continue Watching then Recently Added) ----
const HERO_MAX: usize = 8;

/// One page of the rotating billboard: a catalog index, plus the handle of the SERVER the shelf it
/// was drawn from came from ("friend") — empty for the signed-in user's own, exactly as
/// [`HubRow::source`] means it.
///
/// The handle is carried on the SLOT rather than looked up from the item's shelf at draw time, and
/// that is the point of the type existing at all: the pool is the one place in the app that lifts
/// items OUT of their shelf order ([`own_items_first`] promotes an owned page to the front), so a
/// pool entry that only knew its catalog index would have to find its way back to a hub through a
/// range scan to answer "whose is this" — for a fact the build already had in its hand.
struct HeroSlot {
    idx: usize,
    source: String,
}
static mut HERO_POOL: Vec<HeroSlot> = Vec::new();

fn pool() -> &'static Vec<HeroSlot> {
    unsafe { &*std::ptr::addr_of!(HERO_POOL) }
}

/// number of items in the rotating hero pool
pub(crate) fn hero_pool_len() -> usize {
    pool().len()
}
/// hero-pool item `i`, or None
pub(crate) fn hero_pool_item(i: usize) -> Option<&'static PmsMovie> {
    movie(pool().get(i)?.idx)
}
/// Handle of the server hero-pool page `i` came from ("friend"), or **empty** for the signed-in
/// user's own — see [`HeroSlot`]. The hero's meta line draws no run at all for the empty case
/// (`ui::home::meta_source_flow`), so a single-server library pays nothing for this. Borrowed on the
/// same terms as [`hub_title`]: main-thread only, valid until the next hub commit.
pub(crate) fn hero_pool_source(i: usize) -> &'static str {
    pool().get(i).map(|s| s.source.as_str()).unwrap_or("")
}

/// **Own items first — an ORDERING, not a filter** (Shared Sources, deliverable C).
///
/// A borrowed item may not hold the FIRST rotation while the owner contributes at least one, so the
/// app's front door opens on your own library and a friend's film arrives one 8-second flip in,
/// attributed. Everything else about the pool is untouched: it stays merged, in the order the
/// shelves produced it, and the promoted page is lifted out and re-inserted rather than sorted, so
/// `[B1, B2, O1, O2, B3]` becomes `[O1, B1, B2, O2, B3]` — one page moves, nothing is dropped and
/// nothing else is reordered.
///
/// Filtering instead would leave a borrowed-only account with **no hero at all**, and would overrule
/// a pin the user made; that is why a pool with nothing of our own in it is left exactly as it is and
/// opens on a borrowed page. This is also why the rule needs no switch of its own: the pool is built
/// from included sources only, so a borrowed hero is always the consequence of a pin.
fn own_items_first(pool: &mut Vec<HeroSlot>) {
    if pool.first().map(|s| s.source.is_empty()).unwrap_or(true) {
        return; // nothing pooled, or one of ours already opens the door
    }
    if let Some(k) = pool.iter().position(|s| s.source.is_empty()) {
        let own = pool.remove(k);
        pool.insert(0, own);
    }
}

/// number of home hubs
pub(crate) fn hub_count() -> usize {
    hubs().len()
}
/// title of hub `i` (e.g. "Continue Watching") — borrowed from the main-thread hub table (the
/// per-frame shelf-title draw shouldn't clone a String per row; HUBS only changes on a re-fetch).
pub(crate) fn hub_title(i: usize) -> &'static str {
    hubs().get(i).map(|h| h.title.as_str()).unwrap_or("")
}
/// Handle of the server hub `i` came from ("friend"), or **empty** for the signed-in user's own
/// server — see [`HubRow::source`]. Borrowed on the same terms as [`hub_title`]: main-thread only,
/// valid until the next hub commit.
pub(crate) fn hub_source(i: usize) -> &'static str {
    hubs().get(i).map(|h| h.source.as_str()).unwrap_or("")
}
/// item count in hub `i`
pub(crate) fn hub_len(i: usize) -> usize {
    hubs().get(i).map(|h| h.len).unwrap_or(0)
}
/// whether hub `i` is the merged Continue Watching shelf (its tiles play directly on OK, so the
/// home grid stamps the play-hint badge on them). Matched on the locale-independent hubIdentifier.
pub(crate) fn hub_is_continue(i: usize) -> bool {
    hubs().get(i).map(|h| h.hub_id == "home.continue").unwrap_or(false)
}
/// item `col` of hub `hub`, or None
pub(crate) fn hub_item(hub: usize, col: usize) -> Option<&'static PmsMovie> {
    let h = hubs().get(hub)?;
    if col < h.len {
        movie(h.start + col)
    } else {
        None
    }
}

/// Refetch the home hubs — the player exit's entry point. A pass-through to [`pms_fetch_hubs`]
/// now that reconciling the surfaces that index into the rebuilt catalog is [`commit`]'s own job.
///
/// **BLOCKING on the owned server**, because [`pms_fetch_hubs`] is. That is affordable on the
/// player-exit path, which is already a teardown, and it is NOT affordable off a keypress — see
/// [`request_refetch_hubs`], which is what the view-state writes use.
pub(crate) fn refetch_hubs_reconcile() -> c_int {
    pms_fetch_hubs()
}

/// Refetch the home hubs OFF the main thread — every source on a worker, the owned one included,
/// landing through [`pump`] like any other fetch. **MAIN THREAD, NON-BLOCKING.**
///
/// The twin of [`refetch_hubs_reconcile`], and the difference is only which sources are inline.
/// `pms_fetch_hubs` keeps the owned server synchronous because it is the BOOT fetch and Home cannot
/// open without it; a refetch has a populated Home already on screen, so there is nothing to wait
/// for and every reason not to — this is reached from a keypress (`viewstate`'s Mark as Watched /
/// Remove from Deck), where a WAN server's `/hubs` pair is seconds of parked frame loop.
///
/// No reconcile call here, and none is owed anywhere: [`commit`] performs the re-selection and the
/// repaint itself, at the only moment the catalog those surfaces index into actually moves.
pub(crate) fn request_refetch_hubs() {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // supersede every retry already in flight
    sync_roster();
    let mut srcs = lock_srcs();
    // A superseded worker's landing is dropped on the generation above, so releasing the
    // single-flight latches here cannot double-apply anything — and without it a source whose
    // worker was in flight across this call would stay latched and never fetch again. Same clause
    // `pms_fetch_hubs` opens with, for the same reason.
    for s in srcs.iter_mut() {
        s.fetching = false;
        retry_now(s); // from the bottom of the ladder: the user asked for this, in effect
    }
}

/// A local, **optimistic** edit to what the shelves say about one item — applied before the write
/// that justifies it has left the machine, so a press lands on the panel at once however far away
/// the item's server is. See [`edit_item`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LocalEdit {
    /// The item is now watched (`true`) or unwatched (`false`), everywhere it appears.
    Watched(bool),
    /// The item has been hidden from Continue Watching (`removeFromContinueWatching`) — it leaves
    /// the deck and NOTHING else about it changes, which is exactly what that endpoint does.
    LeftTheDeck,
}

/// Apply `edit` to every shelf row naming `(sid, rk)` and re-commit Home. Returns whether anything
/// matched. **MAIN THREAD** — it rebuilds the catalog the UI holds `&'static` rows out of.
///
/// It edits each source's own last PROJECTION and re-runs the pure [`merge`], rather than splicing
/// the committed catalog: `HubRow` addresses its cards as a `start`/`len` window into one flat
/// `Vec`, and the hero pool holds indices into the same, so removing a row by hand means fixing up
/// every window behind it and every pool slot — three chances to leave the three statics disagreeing,
/// which is the exact class `commit`'s doc says they move together to avoid. Re-merging is arithmetic
/// the module already trusts, and it also lets a shelf that lost a card refill from the budget.
///
/// This is one half of a pair and is useless alone: it is what the user SEES, and the refetch the
/// write's landing kicks is what the server SAYS. Where they disagree the refetch wins, silently.
pub(crate) fn edit_item(sid: ServerId, rk: &str, edit: LocalEdit) -> bool {
    let mut srcs = lock_srcs();
    let mut hit = false;
    for s in srcs.iter_mut() {
        if let Some(b) = s.last.as_mut() {
            hit |= apply_edit(b, sid, rk, edit);
        }
    }
    if !hit {
        return false; // the item is on no shelf (a Library-grid or Related item): nothing to redraw
    }
    let build = merge(&srcs);
    drop(srcs); // before calling out — `detail::reselect` walks the catalog this replaces
    commit(build);
    true
}

/// [`edit_item`] on ONE source's projection. Pure — no statics, no I/O — so the rule is graded on
/// the host rather than inferred from a screenshot.
fn apply_edit(b: &mut SourceBuild, sid: ServerId, rk: &str, edit: LocalEdit) -> bool {
    let mine = |m: &PmsMovie| crate::plex::same_item((m.sid, &m.rk), (sid, rk));
    match edit {
        LocalEdit::Watched(on) => {
            let mut hit = false;
            for c in b.cw.iter_mut() {
                if mine(&c.m) {
                    set_watched(&mut c.m, on);
                    hit = true;
                }
            }
            for m in b.shelves.iter_mut().flat_map(|s| s.items.iter_mut()) {
                if mine(m) {
                    set_watched(m, on);
                    hit = true;
                }
            }
            hit
        }
        LocalEdit::LeftTheDeck => {
            let before = b.cw.len();
            b.cw.retain(|c| !mine(&c.m));
            b.cw.len() != before
        }
    }
}

/// The three fields one row's watch state is spread over, moved together.
///
/// `resume_ms` goes with them, and it is the half that is easy to miss: [`PmsMovie::resume_frac`]
/// takes PRECEDENCE over the watched flag at the mark (`ui::widgets::poster_mark` — a re-watch in
/// flight outranks a finished item), so a row left holding its old `viewOffset` would wear the
/// progress bar it had before and show no tick at all — the press would read as having done
/// nothing. An unscrobble genuinely clears `viewOffset` server-side, and a scrobbled item leaves
/// the deck; where the server disagrees, its own refetch is a moment behind this and wins.
///
/// `pub(crate)` because the hub catalog stopped being the only store an optimistic edit reaches:
/// `browse`, `search` and `person` each hold their own rows and each flips them the same way, and
/// three copies of "which three fields" is three chances for one of them to leave the resume bar on.
pub(crate) fn set_watched(m: &mut PmsMovie, on: bool) {
    m.watched = on;
    m.unwatched = !on;
    m.resume_ms = 0;
}

/// Fetch Home — BLOCKING for the owned server, off-thread for every other source. The boot /
/// install path's fetch; returns the committed catalog's row count.
///
/// **Only the FIRST source is fetched inline.** Home cannot open without the signed-in user's own
/// server, and this call's return value is what `install_pms` logs as `nmovies=`, so that one stays
/// synchronous exactly as it always was. Every shared source is kicked onto a worker instead:
/// a share that has gone away costs a full connect timeout (8 s, measured — `docs/shared-servers.md`
/// §2), and serialising N of those on the SDL thread would freeze boot for as long as the friend's
/// router stays quiet. They land in [`pump`] whenever they land.
///
/// A failure does not commit empty. It used to — "blanking the home consistently" — which made a
/// dead server indistinguishable from a server with nothing on it, and left Home a bare dark screen
/// with no explanation and nothing that would ever try again. Now the verdict is PER SOURCE: the
/// failing one keeps whatever it last answered with, the others commit regardless, and [`pump`]
/// retries that source alone on its own backoff.
pub(crate) fn pms_fetch_hubs() -> c_int {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // supersede every retry already in flight
    sync_roster();
    let mut srcs = lock_srcs();
    // A superseded worker's landing is dropped on the generation above, so releasing the
    // single-flight latches here cannot double-apply anything — and without it a source whose
    // worker was in flight across this call would stay latched and never fetch again.
    for s in srcs.iter_mut() {
        s.fetching = false;
    }
    if let Some(own) = srcs.first_mut() {
        // MAIN THREAD: the client this source's shelves will describe, captured here and carried
        // into the parse — the same capture [`kick`] makes at its spawn site (see `parse_item`).
        match crate::plex::client_for(own.sid).and_then(|c| {
            let sid = own.sid;
            catch_unwind(move || fetch_source(c, sid)).ok().flatten()
        }) {
            Some(b) => landed_ok(own, b),
            None => landed_fail(own),
        }
    }
    for s in srcs.iter_mut().skip(1) {
        kick(s);
    }
    let build = merge(&srcs);
    drop(srcs);
    commit(build)
}

/// The catalog/hubs/pool triple the merge produces, before it is committed.
type HubBuild = (Vec<PmsMovie>, Vec<HubRow>, Vec<HeroSlot>);

// ---- one source's contribution -----------------------------------------------------------------

/// A Continue Watching entry, carrying the sort key the MERGE needs. `lastViewedAt` used to be read
/// off the wire DTO and thrown away at parse time, because one server's hub arrived already in the
/// right order; across sources the order has to be re-established after the fact, so the key has to
/// survive the projection.
struct CwItem {
    last_viewed_at: i64,
    m: PmsMovie,
}

/// One shelf as a source projected it: rows already parsed, filtered and stamped with the server
/// they came from, so the merge is pure arithmetic over owned data and never touches a wire DTO.
struct Shelf {
    title: String,
    hub_id: String,
    items: Vec<PmsMovie>,
}

/// ONE source's whole contribution to Home — its Continue Watching items (merged with everyone
/// else's into a single shelf) and its own shelves (kept whole, annotated with its owner's handle).
///
/// Owned data only, so a worker can build it and hand it over through the mailbox, and a source can
/// KEEP the last one it answered with across a failure.
#[derive(Default)]
struct SourceBuild {
    cw: Vec<CwItem>,
    shelves: Vec<Shelf>,
}

/// GET one source's hubs and project them. `None` = the request failed (transport, HTTP, or
/// parse), which is NOT the same as a server with nothing on it (`Some` with an empty build) —
/// that distinction is the whole reason an empty library reads as Ready and not as an error.
///
/// The client AND its `sid` are passed IN, captured at the spawn site: a worker must never ask
/// which server is current (`browse.rs` states the rule, and a server switch mid-fetch would
/// otherwise stamp these rows with the other machine's id — the one thing every `(sid, rk)`
/// comparison downstream then trusts). A `&'static Client` also pins the exact address this fetch
/// was aimed at even if the registry re-points that slot mid-request.
fn fetch_source(c: &crate::plex::Client, sid: ServerId) -> Option<SourceBuild> {
    let mc = c.home_hubs(HUB_FETCH_COUNT)?;
    // The Continue Watching shelf comes from the DEDICATED hub (see `project`). Its failure fails
    // THIS SOURCE (`?`) — nothing of it commits and it retries on its own backoff. Losing the most
    // important shelf to a transient error would be worse than briefly showing the previous one.
    let cw = c.continue_watching(HUB_FETCH_COUNT)?;
    Some(project(&mc, &cw, sid))
}

/// Project one source's `/hubs` + `/hubs/continueWatching` responses into its [`SourceBuild`].
/// Pure — no statics, no I/O, no knowledge of any other source; `sid` is the server the two
/// containers came from, stamped onto every row it builds.
fn project(mc: &crate::plex::MediaContainer, cw: &crate::plex::MediaContainer, sid: ServerId) -> SourceBuild {
    const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];
    // need a poster to show it in a shelf
    let keep = |it: &crate::plex::Metadata| {
        let m = parse_item(it, sid);
        (!m.title.is_empty() && !m.thumb.is_empty()).then_some(m)
    };
    let mut out = SourceBuild::default();

    // Continue Watching comes from the **dedicated** `/hubs/continueWatching` hub, and `/hubs`'s own
    // `home.continue` / `home.ondeck` pair is skipped entirely. It used to merge that pair by hand
    // (in-progress items unified with next-up episodes, deduped by ratingKey) to reproduce the
    // official-app row — the dedicated hub already IS that row, so the merge was reimplementing a
    // server-side answer.
    //
    // The reason it has to be the dedicated one, though, is `removeFromContinueWatching`: measured on
    // PMS 1.43.3, that action hides an item from `/hubs/continueWatching` and from `home.ondeck` but
    // **NOT** from `home.continue` (see `plex::Client::remove_from_continue_watching`). Built from the
    // pair, this shelf would keep drawing a card the server had been told to hide, and the context
    // menu's Remove row would look broken while the server had done exactly as asked.
    for hub in cw.hub.iter() {
        out.cw = hub
            .metadata
            .iter()
            .filter(|m| !SKIP.contains(&m.kind.as_str()))
            .filter_map(|it| keep(it).map(|m| CwItem { last_viewed_at: it.last_viewed_at, m }))
            .collect();
        if !out.cw.is_empty() {
            break; // the first hub that has anything in it IS the deck
        }
    }

    for hub in &mc.hub {
        if SKIP.contains(&hub.kind.as_str()) {
            continue;
        }
        if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
            continue; // superseded by the dedicated hub above
        }
        let items: Vec<PmsMovie> = hub.metadata.iter().filter_map(&keep).collect();
        if items.is_empty() {
            continue;
        }
        out.shelves.push(Shelf { title: hub.title.clone(), hub_id: hub.hub_identifier.clone(), items });
    }
    out
}

// ---- the merge: every source, one Home ----------------------------------------------------------

/// Divide `budget` between sources that each want `want[i]`, so that **no source can starve
/// another**.
///
/// This is the whole of the multi-server budget rule. Handing the budget out first-come (which is
/// what a single running total does, and what this module did while there was only ever one server)
/// means the first source spends it: `/hubs` promotes several rows per library, so a four-library
/// server alone overruns [`MAX_SHELVES`] and a share behind it draws nothing at all.
///
/// It is water-filling, not a flat `budget / n`: the smallest demand is served first and what it
/// does not use is RE-DIVIDED among the rest, so a modest source costs nobody anything and two
/// greedy ones still split what is left evenly. A leftover pass in source order instead would hand
/// every unclaimed row to whoever came first, which is the starvation this exists to prevent
/// wearing a fairer name. Pure, so the rule is graded on the host rather than inferred from a
/// screenshot.
///
/// `pub(crate)` for `crate::person`, whose Movies/Shows shelves are the same merge one level down:
/// a prolific actor's films on the server you arrived through would otherwise fill the row and
/// leave the share behind it nothing — which is the bug that store exists to fix, re-created inside
/// it. One water-filling rule, not two.
pub(crate) fn allot(budget: usize, want: &[usize]) -> Vec<usize> {
    let n = want.len();
    let mut out = vec![0usize; n];
    let mut left = budget;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| want[i]); // stable: equal demands keep source order, our own first
    for (k, &i) in order.iter().enumerate() {
        // `.max(1)` so a budget smaller than the source count reaches as many sources as it can
        // rather than nobody; `.min(left)` keeps that honest once it runs out.
        let share = (left / (n - k)).max(1).min(left);
        out[i] = want[i].min(share);
        left -= out[i];
    }
    out
}

/// Merge every source's last good projection into the one catalog/hubs/pool triple the UI reads.
/// PURE — no statics, no I/O — which is what makes the ordering, the annotation and the budget
/// gradeable on the host.
///
/// The shape of Home, in order:
/// 1. **Continue Watching**, merged across every source and sorted by `lastViewedAt` descending, so
///    a borrowed item holds first position exactly when the owner watched it last. It carries NO
///    annotation (see [`HubRow::source`]). This is the official client's own shape: the owner's
///    screenshots show a friend's two films sitting BETWEEN their own three, in one row.
/// 2. **Every other shelf**, source by source in roster order — the owned server's first, then each
///    shared server's, contiguously, because adjacency is the grouping device.
///
/// A source that has never answered contributes nothing at all: no heading, no empty shelf, no
/// placeholder row. One that answered and has since failed keeps the shelves it last had, which is
/// the other half of the same rule — a transient failure must not blank a populated Home, and a
/// source that is really gone leaves the ROSTER, which is what drops its shelves.
fn merge(srcs: &[Src]) -> HubBuild {
    let pins = library_pins_by_server();
    let live: Vec<(&str, &SourceBuild)> =
        srcs.iter().filter_map(|s| s.last.as_ref().map(|b| (s.handle.as_str(), b))).collect();

    let mut new_cat: Vec<PmsMovie> = Vec::new();
    let mut new_hubs: Vec<HubRow> = Vec::new();
    // Parallel to `new_cat`: the HANDLE of the source each row came from. The hero pool is the only
    // reader, and it needs the fact per ITEM rather than per shelf — the merged deck's own `source`
    // is empty by design, so a slot that took its handle from its shelf would attribute every
    // borrowed film in Continue Watching to nobody, and `own_items_first` would think our own
    // library already opened the door.
    let mut row_handle: Vec<&str> = Vec::new();

    // ---- 1. the merged deck ----
    let mut cw: Vec<(&str, &CwItem)> = live
        .iter()
        .flat_map(|(h, b)| b.cw.iter().map(move |c| (*h, c)))
        .filter(|(_, c)| item_pinned(&pins, &c.m))
        .collect();
    // stable: equal timestamps keep source order, so the owned server wins a tie
    cw.sort_by(|a, b| b.1.last_viewed_at.cmp(&a.1.last_viewed_at));
    cw.truncate(MAX_SHELF_ITEMS);
    for (h, c) in &cw {
        new_cat.push(c.m.clone());
        row_handle.push(h);
    }
    if !new_cat.is_empty() {
        new_hubs.push(HubRow {
            // the hub id the rest of the module matches on (hero-pool eligibility,
            // `hub_is_continue`), rather than the dedicated hub's own "continueWatching"
            title: "Continue Watching".to_string(),
            hub_id: "home.continue".to_string(),
            source: String::new(),
            start: 0,
            len: new_cat.len(),
        });
    }

    // ---- 2. every other shelf, grouped by source ----
    let shelf_want: Vec<usize> = live.iter().map(|(_, b)| b.shelves.len()).collect();
    let row_want: Vec<usize> = live.iter().map(|(_, b)| b.shelves.iter().map(|s| s.items.len()).sum()).collect();
    let shelves_for = allot(MAX_SHELVES - new_hubs.len(), &shelf_want);
    let rows_for = allot(PMS_MAX_MOVIES - new_cat.len(), &row_want);

    for (i, (handle, b)) in live.iter().enumerate() {
        let mut rows_left = rows_for[i];
        for sh in b.shelves.iter().take(shelves_for[i]) {
            let start = new_cat.len();
            // Filtered BEFORE the take, so an unpinned library cannot spend a pinned one's row
            // budget — and a shelf left with nothing contributes no `HubRow` below, which is how an
            // unpinned library's whole shelf disappears rather than becoming an empty heading.
            for m in sh.items.iter().filter(|m| item_pinned(&pins, m)).take(rows_left.min(MAX_SHELF_ITEMS)) {
                new_cat.push(m.clone());
                row_handle.push(handle);
            }
            rows_left -= new_cat.len() - start;
            if new_cat.len() > start {
                new_hubs.push(HubRow {
                    title: sh.title.clone(),
                    hub_id: sh.hub_id.clone(),
                    source: (*handle).to_string(),
                    start,
                    len: new_cat.len() - start,
                });
            }
        }
    }

    // ---- 3. the rotating hero pool ----
    // Continue Watching items first, then Recently Added, deduped by the item's IDENTITY. Require
    // landscape `art` (the hero draws a full-bleed backdrop) and skip seasons (a bare "Season 1"
    // makes a poor billboard). Capped at HERO_MAX.
    let mut new_pool: Vec<HeroSlot> = Vec::new();
    for hub in &new_hubs {
        // Match on the locale-independent hubIdentifier, not the localized display title:
        // "home.continue" plus every Recently Added variant (home.movies.recent,
        // home.television.recent, promoted <type>.recentlyadded.<id>) all carry "recent".
        let eligible = hub.hub_id == "home.continue" || hub.hub_id.contains("recent");
        if !eligible {
            continue;
        }
        for idx in hub.start..hub.start + hub.len {
            if new_pool.len() >= HERO_MAX {
                break;
            }
            let m = &new_cat[idx];
            if m.art.is_empty() || m.kind == 2 {
                continue; // need landscape art; skip seasons
            }
            // dedup by the item's IDENTITY, not by its bare key: two shelves merged from two
            // servers can each contribute a different film numbered 1, and a bare-key dedup would
            // silently drop the second from the hero rotation.
            //
            // NB the pool holds `HeroSlot`s, so the index is `s.idx` — unit 12's hero-ordering work
            // and unit 3's identity work landed in this same expression from opposite directions.
            if new_pool
                .iter()
                .any(|s| crate::plex::same_item((new_cat[s.idx].sid, &new_cat[s.idx].rk), (m.sid, &m.rk)))
            {
                continue;
            }
            new_pool.push(HeroSlot { idx, source: row_handle[idx].to_string() });
        }
    }
    // …and only now is the order decided: the pool is assembled shelf by shelf, so which server
    // opens the door is not knowable until every shelf has contributed.
    own_items_first(&mut new_pool);
    (new_cat, new_hubs, new_pool)
}

// ---- fetch state machine: PER SOURCE loading / ready / failed + the automatic-retry backoff ----
//
// Modelled on `browse.rs`'s page store, deliberately, because it already learned two of the three
// lessons this needed: a FAILED fetch must never overwrite a populated store (one wifi hiccup used
// to blank a whole grid permanently), and a fast-failing network must be held off by a countdown
// rather than re-spawning a worker every frame.
//
// The third only appears with a second server, and every piece of it was process-global before:
// **a verdict belongs to a SOURCE, not to Home.** One `?` chain meant a dead share aborted the whole
// build and nothing committed — on a cold boot, a whole-screen "Can't reach your Plex server" about
// a library that was answering perfectly well. One in-flight latch meant a share that takes eight
// seconds to time out held the owned server's retry behind it. One backoff meant the ladder a dead
// share had climbed to 30 s was the ladder every other source then waited on.

/// What a source's last fetch produced. Home's loading / empty / error read-out is a projection of
/// these ([`hub_state`] folds them): an empty catalog is only an empty *screen* when a fetch
/// actually succeeded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HubState {
    /// a fetch is in flight, or the first one hasn't run yet — nothing to show is not an answer
    Loading,
    /// the server answered; the catalog is whatever it says it is (possibly legitimately empty)
    Ready,
    /// the fetch failed — whatever this source last answered with is untouched and [`pump`] is
    /// counting down to the next automatic attempt for it alone
    Failed,
}

/// A source Home is built from, plus everything the fetch state machine knows about it.
/// Main-thread only, behind [`SRCS`].
struct Src {
    /// The registry slot every fetch for this source is issued through — and the id stamped onto
    /// every row it parses.
    sid: ServerId,
    /// The owner's plex.tv handle ("friend") for a BORROWED server; **empty** for one of our own.
    /// Read from the registry at [`sync_roster`] time, never inside a worker.
    handle: String,
    state: HubState,
    /// Single flight, PER SOURCE. Cleared only where a landing is taken, where the spawn was
    /// refused, or where an authoritative fetch has invalidated everything in flight — drop one of
    /// those and this source never fetches again for the rest of the session.
    fetching: bool,
    /// Bumped by every [`kick`]. A landing whose seq is not the latest is a superseded worker's and
    /// is dropped: [`HUB_GEN`] says "a different account or server set", this says "an older attempt
    /// at the same one", and only the pair rules out both double-applies.
    seq: u32,
    retry_s: f32,
    retry_n: u32,
    /// The projection this source last ANSWERED with, kept across a failure. This is what makes a
    /// failure never blank a populated Home; `None` (never answered) is what makes a dead source
    /// contribute nothing at all — no heading, no empty shelf, no spinner row.
    last: Option<SourceBuild>,
}

impl Src {
    fn new(sid: ServerId, handle: String) -> Src {
        Src { sid, handle, state: HubState::Loading, fetching: false, seq: 0, retry_s: 0.0, retry_n: 0, last: None }
    }
}

/// The source table, in display order: our own servers first, then each shared one. Rebuilt from
/// the roster by [`sync_roster`]; every other access is main-thread, so the lock is never contended
/// and exists to keep the borrow checker (and any future worker) honest rather than to arbitrate.
static SRCS: Mutex<Vec<Src>> = Mutex::new(Vec::new());
fn lock_srcs() -> std::sync::MutexGuard<'static, Vec<Src>> {
    SRCS.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the source table was last built from — the registry's size and the pinned-library set.
/// Either moving rebuilds it, which is how a share the roster layer has just registered, or a
/// library the user has just pinned, reaches Home without anyone having to call in.
static SEEN: AtomicU64 = AtomicU64::new(u64::MAX);

/// One source's finished (or failed) off-thread fetch. `build: None` deliberately carries no data,
/// so a failure can never be mistaken for "the server returned nothing".
struct Landing {
    gen: u32,
    seq: u32,
    sid: ServerId,
    build: Option<SourceBuild>,
}
static RESULTS: Mutex<Vec<Landing>> = Mutex::new(Vec::new());

/// Bumped by every authoritative fetch ([`reset`] on an identity change, and the blocking install
/// fetch): a worker spawned before it lands with a stale tag and is DROPPED. Without this, a retry
/// in flight across a profile switch could commit the previous account's hubs on top of the new
/// one's — the same late-landing hazard `browse.rs` keys its `GEN` on.
static HUB_GEN: AtomicU32 = AtomicU32::new(0);
/// `browse::sections_gen()` as of the last merge. The section table is where the pinned set comes
/// from, so a change to it can change WHICH sources Home is built from without any hub landing —
/// see the read in [`pump`]. Starts at 0, which is also the table's own starting generation, so a
/// boot that discovers nothing does not merge twice for nothing.
static LAST_SECTIONS_GEN: AtomicU32 = AtomicU32::new(0);

/// The backoff ladder's ends. A TV parked on a sleeping server must keep trying — that IS the
/// feature — without ever becoming a request loop, so the wait doubles from `MIN` to a `MAX`
/// that still recovers within half a minute of the server coming back.
const RETRY_MIN_S: f32 = 2.0;
const RETRY_MAX_S: f32 = 30.0;

/// Wait before attempt `fails + 1`: 2s, 4s, 8s, 16s, then 30s forever. Pure — host-tested.
fn backoff_secs(fails: u32) -> f32 {
    let steps = fails.saturating_sub(1).min(6); // 1<<6 already exceeds the cap; guards the shift
    (RETRY_MIN_S * (1u32 << steps) as f32).min(RETRY_MAX_S)
}

/// Home's fetch state, folded from every source — what the loading / empty / error read-out reads.
///
/// **Any source answering makes Home answered**, and the total-failure read-out is reserved for the
/// case where every one of them failed. That is the whole point of the fold: "Can't reach your Plex
/// server", drawn because a friend's machine is asleep, is a lie about the library that is working.
/// No source at all (before the first install) is Loading — nothing to show is not an answer.
pub(crate) fn hub_state() -> HubState {
    let s = lock_srcs();
    if s.iter().any(|x| x.state == HubState::Ready) {
        HubState::Ready
    } else if !s.is_empty() && s.iter().all(|x| x.state == HubState::Failed) {
        HubState::Failed
    } else {
        HubState::Loading
    }
}

/// Does this server feed **Home**? The design's one control, and the seam the pin store fills.
///
/// `pinned` is every pinned library's server, from `browse::pinned_libraries`. The rule is *not*
/// "is this server in that list": an EMPTY list means the pin store knows nothing yet, not that
/// nothing is pinned. `/library/sections` lands asynchronously and always after `pms_fetch_hubs`,
/// and `browse::is_last_pinned` forbids unpinning the last library — so "empty" can only mean "no
/// section has been discovered anywhere", and treating it as "nothing is pinned" would leave Home
/// with no sources at all on the frame it boots.
///
/// Pure, so the bootstrap rule is graded rather than observed on a television.
fn feeds_home(sid: ServerId, pinned: &[ServerId], known: &[ServerId]) -> bool {
    // **A server whose libraries we have not enumerated yet is UNDECIDED, not unpinned.**
    //
    // The pin is a decision about libraries; you cannot have decided against one nobody has
    // discovered. Section discovery for a source other than the current one runs on a worker off
    // `browse::pump`, which only runs while the LIBRARY screen is up — so on a fresh boot the share
    // is in the roster, has answered with shelves, and has no known sections at all. Testing
    // `pinned.contains` there excluded it from Home until you happened to visit the Library, which
    // is exactly how the owner found it: "it appeared on the home screen only after I watched the
    // library."
    //
    // The whole-set emptiness check below is the same rule one level up (nothing discovered
    // anywhere yet) and is kept for the boot frame before any source has answered.
    pinned.is_empty() || pinned.contains(&sid) || !known.contains(&sid)
}

/// The pin table as `(server, section key, pinned)` — `browse`'s rows with their source index
/// resolved to a registry slot, which is the form [`item_pinned`] can join an item against.
///
/// **The one projection.** There were three: this, plus a `servers_with_known_sections` and a
/// `pinned_servers` that were the same index→slot fold with a different `browse` call feeding them
/// — and each pulled its own `pub(crate)` out of `browse`, so a change to how a source index maps
/// to a registry slot had to land in three places. Both are columns of this table:
/// `known` is its `sid`s deduped, `pinned` is the same after `filter(pinned)`.
fn library_pins_by_server() -> Vec<(ServerId, i64, bool)> {
    let srcs = crate::browse::sources();
    crate::browse::library_pins()
        .into_iter()
        .filter_map(|(si, key, pinned)| srcs.get(si).map(|s| (s.sid, key, pinned)))
        .collect()
}

/// May this row appear on Home? **Per LIBRARY, which is the grain the switch offers.**
///
/// `/hubs` is a whole-SERVER request and answers with rows from every library on that server, so
/// without this the finest gate available was "does this server feed Home at all" — and unpinning
/// one library of a two-library server changed nothing at all on screen. Owner-reported.
///
/// Unknown is ALLOWED, in both directions: a row whose server sent no `librarySectionID`, and a
/// library the section table has not enumerated yet, both pass. The pin is a decision about
/// libraries we know about, and the alternative — hiding what we cannot classify — empties Home on
/// the frame it boots, which is the same mistake [`feeds_home`] documents one level up.
fn item_pinned(pins: &[(ServerId, i64, bool)], m: &PmsMovie) -> bool {
    if m.sec == 0 {
        return true; // the server said nothing about this row's library
    }
    match pins.iter().find(|(sid, key, _)| *sid == m.sid && *key == m.sec) {
        Some((_, _, pinned)) => *pinned,
        None => true, // not enumerated yet
    }
}

/// The two server sets [`feeds_home`] takes, folded out of [`library_pins_by_server`] in one pass:
/// `(pinned, known)`. Separated from the rule so `feeds_home` stays pure and host-gradeable.
fn home_server_sets(pins: &[(ServerId, i64, bool)]) -> (Vec<ServerId>, Vec<ServerId>) {
    let (mut pinned, mut known) = (Vec::new(), Vec::new());
    for &(sid, _, is_pinned) in pins {
        if !known.contains(&sid) {
            known.push(sid);
        }
        if is_pinned && !pinned.contains(&sid) {
            pinned.push(sid);
        }
    }
    (pinned, known)
}

/// The sources Home is built from, in display order: our own servers first, then each share, each
/// group keeping registration order. The merge appends shelves in exactly this order and adjacency
/// is the grouping device, so "own first, then each shared server's, contiguously" is true by
/// construction rather than by convention.
///
/// The registry IS the granted roster — a server is in it only once plex.tv (or the
/// `plxnative-servers` dev trigger) handed us a token for it — and [`feeds_home`] is what narrows
/// the grant to a pin. The handle comes from the same place (`ServerFacts`), so nothing here has an
/// opinion about who a server belongs to that the Sources list does not share.
fn roster() -> Vec<(ServerId, String)> {
    let (pinned, known) = home_server_sets(&library_pins_by_server());
    let mut own: Vec<(ServerId, String)> = Vec::new();
    let mut shared: Vec<(ServerId, String)> = Vec::new();
    for sid in crate::plex::server_ids() {
        if crate::plex::client_for(sid).is_none() || !feeds_home(sid, &pinned, &known) {
            continue;
        }
        let handle = crate::plex::server_facts(sid).map(|f| f.handle.clone()).unwrap_or_default();
        if handle.is_empty() {
            own.push((sid, handle));
        } else {
            shared.push((sid, handle));
        }
    }
    own.append(&mut shared);
    own
}

/// A cheap fingerprint of what [`roster`] would return, so [`sync_roster`] can skip the rebuild on
/// the frames — almost all of them — where nothing has changed.
///
/// **Two atomic loads and no allocation.** The inputs are the registry's size and the section
/// table, and the pinned set is a projection of the second — `toggle_pin`, `append_sections` and
/// `reset` all bump `SECTIONS_GEN`, so that counter already moves whenever the pinned set can.
/// Folding the pinned SERVERS in by hand meant walking `browse`'s table and building two `Vec`s
/// here, on a path `pump` runs every loop iteration — ~60×/s including on a settled Home, which is
/// the screen `ui::idle` was tuned down to ~1% of a core on.
fn roster_key() -> u64 {
    ((crate::plex::server_count() as u64) << 32) | crate::browse::sections_gen() as u64
}

/// Bring the source table in line with the roster: a surviving source keeps everything it has
/// (its state, its backoff, and the build it last answered with), a new one arrives Loading and is
/// picked up by the next [`pump`], and one that has left takes its shelves with it.
fn sync_roster() {
    let k = roster_key();
    if SEEN.swap(k, Ordering::Relaxed) == k {
        return;
    }
    let want = roster();
    let mut srcs = lock_srcs();
    let mut out: Vec<Src> = Vec::with_capacity(want.len());
    for (sid, handle) in want {
        // One slot, one source. The way a roster came to name a slot twice was `plex::register`
        // answering with `current()` when the table was full; that now answers `ServerId::UNSET`,
        // which resolves to nothing and never reaches this list. The guard stays because a second
        // `Src` sharing a sid is worse than the mistake that produced it: every landing resolves to
        // the first of them, so the other never un-latches its single flight and silently stops
        // fetching for good.
        if out.iter().any(|x| x.sid == sid) {
            continue;
        }
        match srcs.iter().position(|x| x.sid == sid) {
            Some(i) => {
                let mut keep = srcs.remove(i);
                keep.handle = handle;
                out.push(keep);
            }
            None => out.push(Src::new(sid, handle)),
        }
    }
    // Whatever is left in `srcs` has left the roster — un-pinned, or a share plex.tv no longer
    // grants. A worker still out for one of them posts a landing for a sid this table no longer
    // holds, which `pump` drops.
    let dropped = srcs.iter().any(|x| x.last.is_some());
    *srcs = out;
    if dropped {
        let build = merge(&srcs);
        drop(srcs); // before calling out — `detail::reselect` walks the catalog this replaces
        commit(build);
    }
}

/// Install a finished merge — the whole post-mutation ritual, not just the stores.
///
/// All three statics move together (they always have — a half-applied catalog once left a stale
/// hero pool floating over emptied shelves), and so do the two surfaces that index INTO them:
/// `detail::reselect` re-resolves an open page's selected row against the rebuilt catalog (indices
/// move, the rk is the stable identity; home's focus self-clamps at its read accessors), and
/// `idle::invalidate` repaints a screen that may have settled — a shelf gaining or losing a card
/// has no spring behind it, so nothing else would report the change to the frame gate.
///
/// Those two used to be the CALLER's to remember, and the five commit sites did not agree: three
/// performed the pair, one ([`pms_fetch_hubs`]) reconciled only through a wrapper its other caller
/// bypassed, and [`reset`] did neither. What kept that last omission from being visible is that
/// `reset`'s one production caller routes away from the detail page first — not any property of
/// `reset`. A ritual every caller must repeat is the defect class, so it lives here, where a new
/// commit site cannot forget it and no site has to be checked against the others.
///
/// MAIN THREAD, and callers release the [`SRCS`] guard first: the re-selection re-enters this
/// module ([`index_of_rk`]) to walk the catalog just replaced.
fn commit(build: HubBuild) -> c_int {
    let (new_cat, new_hubs, new_pool) = build;
    let n = new_cat.len();
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = new_cat;
        *std::ptr::addr_of_mut!(HUBS) = new_hubs;
        *std::ptr::addr_of_mut!(HERO_POOL) = new_pool;
    }
    crate::ui::detail::reselect();
    crate::ui::idle::invalidate();
    n as c_int
}

/// Record one source's success: it answers with this build from now on, and the backoff retires so
/// its next failure starts at the bottom of the ladder instead of inheriting a 30 s wait.
fn landed_ok(s: &mut Src, b: SourceBuild) {
    // The success twin of `landed_fail`'s line. Without it a source that fetched and answered was
    // indistinguishable in the log from one still in flight — "hubs: source 1 fetching" with
    // nothing after it says only that the worker started. The SLOT, never the handle (a plex.tv
    // username is the friend's, and the event log is what users send us).
    crate::log(&format!(
        "hubs: source {} ok — {} shelves, {} in CW",
        s.sid.raw(),
        b.shelves.len(),
        b.cw.len()
    ));
    s.last = Some(b);
    s.state = HubState::Ready;
    s.retry_n = 0;
    s.retry_s = 0.0;
}

/// Record one source's failure: keep whatever it last answered with and arm ITS next attempt.
fn landed_fail(s: &mut Src) {
    s.retry_n = s.retry_n.saturating_add(1);
    s.retry_s = backoff_secs(s.retry_n);
    s.state = HubState::Failed;
    // the ONE line that says a dead source is dead ON PURPOSE and is coming back — without it the
    // whole recovery is invisible in the event log. The SLOT, never the handle: a plex.tv username
    // is the friend's, and the event log is what users send us.
    crate::log(&format!(
        "hubs: source {} FAILED (attempt {}) — retrying in {:.0}s",
        s.sid.raw(),
        s.retry_n,
        s.retry_s
    ));
}

/// Step one source's retry countdown by `dt` seconds; true when its next attempt is due. Split out
/// so the ladder is testable without spawning a worker or touching a socket.
fn retry_due(s: &mut Src, dt: f32) -> bool {
    let left = s.retry_s - dt;
    s.retry_s = left.max(0.0);
    left <= 0.0
}

/// Spawn an off-thread fetch for ONE source (single flight); [`pump`] lands it. Only the owned
/// server's boot fetch is blocking, and that is exactly what lets Home show a live loading state at
/// all: a blocking fetch on the SDL loop draws no frames while it runs, so the spinner it is
/// supposed to be spinning would never reach the panel.
fn kick(s: &mut Src) {
    if s.fetching {
        return; // one in flight already — its spinner is the honest answer
    }
    // CAPTURE AT THE SPAWN SITE. The worker is handed this server's own `&'static Client` and its
    // slot id; it never asks which server is current, and a slot re-pointed mid-request cannot
    // redirect a fetch that is already out (`plex::servers` leaks each client precisely so that
    // reference stays live).
    let Some(c) = crate::plex::client_for(s.sid) else {
        landed_fail(s); // a source whose slot holds no client has nothing to contribute
        return;
    };
    s.fetching = true;
    s.state = HubState::Loading;
    s.retry_s = 0.0;
    s.seq = s.seq.wrapping_add(1);
    let (gen, seq, sid) = (HUB_GEN.load(Ordering::SeqCst), s.seq, s.sid);
    let spawned = crate::task::spawn_small("hubs", move || {
        let build = catch_unwind(move || fetch_source(c, sid)).ok().flatten();
        // pushed OUTSIDE the guard so a panicking fetch still lands (as a failure) rather than
        // latching this source's single flight forever
        RESULTS.lock().unwrap_or_else(|e| e.into_inner()).push(Landing { gen, seq, sid, build });
    });
    if !spawned {
        // nothing will ever fill the mailbox (the thread limit refused us), so release the latch
        // here and back off — `pump` will try again on the ladder.
        s.fetching = false;
        landed_fail(s);
    } else {
        crate::log(&format!("hubs: source {} fetching (off-thread)", sid.raw()));
    }
}

/// Try this source again NOW, from the bottom of the ladder.
fn retry_now(s: &mut Src) {
    s.retry_n = 0;
    s.retry_s = 0.0;
    kick(s);
}

/// The Retry control's kick: try every source again NOW, from the bottom of the ladder — a person
/// who asks for it should never be made to sit out a 30-second automatic wait. A no-op for any
/// source whose fetch is already in flight.
pub(crate) fn request_retry() {
    for s in lock_srcs().iter_mut() {
        retry_now(s);
    }
}

/// Once-a-frame main-thread tick: land finished fetches, then count each source down to its next
/// automatic attempt. Driven by `ui::home::home_update`, so it runs exactly while Home is the
/// screen that cares (and never spawns a background fetch behind the player).
pub(crate) fn pump(dt: f32) {
    sync_roster();
    // taken into a `let` FIRST: an `if let`/`for` scrutinee holds its temporary guard for the whole
    // body under edition 2021, which would run the merge with the mailbox still locked — and the
    // worker that fills it must never wait on a main-thread frame
    let landed = std::mem::take(&mut *RESULTS.lock().unwrap_or_else(|e| e.into_inner()));
    let any_landed = !landed.is_empty();
    let cur = HUB_GEN.load(Ordering::SeqCst);
    let mut srcs = lock_srcs();
    let mut dirty = false;
    for l in landed {
        // A landing from before the last authoritative fetch describes a server (or an account) we
        // have since moved off, and one whose seq has been superseded describes an attempt this
        // source has already replaced. Either is dropped whole — neither committed nor blamed.
        let Some(s) = srcs.iter_mut().find(|s| s.sid == l.sid) else {
            continue; // its source left the roster while it was out
        };
        if l.gen != cur || l.seq != s.seq {
            continue;
        }
        s.fetching = false;
        match l.build {
            Some(b) => {
                landed_ok(s, b);
                dirty = true;
            }
            None => landed_fail(s),
        }
    }
    // Anything but Ready with nothing in flight is a state only a fetch can leave: Failed (with a
    // backoff owed) or a Loading whose worker landed stale and was dropped — the latter owes
    // nothing, so it re-kicks on the spot rather than wedging that source on a spinner forever.
    for s in srcs.iter_mut() {
        if s.state != HubState::Ready && !s.fetching && retry_due(s, dt) {
            kick(s);
        }
    }
    // …and re-merge when the SECTION TABLE moves, not only when a build lands. `feeds_home` reads
    // the pinned set, which is derived from that table — so both of the ways the set changes were
    // invisible to Home before this:
    //
    //   * a share's sections are discovered on a WORKER (`browse::maybe_discover`), strictly after
    //     the boot fetch. Until they land the share is in the roster but has no pinned library, so
    //     the merge that ran on its hub landing excluded it — and nothing re-ran.
    //   * a pin toggled by hand (now bumping the same generation).
    //
    // `merge` is pure over the builds each source already answered with: no request, no allocation
    // beyond the rebuilt catalog. Cheap enough to run on a generation change rather than to try to
    // predict which changes matter.
    let sgen = crate::browse::sections_gen();
    let sections_moved = LAST_SECTIONS_GEN.swap(sgen, Ordering::SeqCst) != sgen;
    let build = (dirty || sections_moved).then(|| merge(&srcs));
    drop(srcs); // before calling out: `detail::reselect` walks the catalog this is about to replace
    if any_landed {
        // A landing that COMMITS repaints from inside `commit`; this is the one that does not —
        // a failure rewrites no shelf but does change the status caption, under a Home screen that
        // may have gone idle with nothing else on it to move.
        crate::ui::idle::invalidate();
    }
    if let Some(build) = build {
        let n = commit(build);
        crate::log(&format!("hubs: landed — {n} items, {} shelves", hub_count()));
    }
}

/// A source that has answered with `n` placeholder rows in one shelf (test fixture). Only the SHAPE
/// is real — `project` would have dropped these rows for having no title/poster; what the tests
/// using it assert is the landing/merge bookkeeping, which never looks inside a row.
///
/// Two fields ARE filled, and both because the hero pool reads them: `art`, since `merge` skips a
/// row with no landscape artwork (it would make a blank billboard), and a distinct `rk` per row,
/// since the pool dedups by item IDENTITY and n rows sharing the empty key are ONE film to it. A
/// fixture of bare defaults therefore committed shelves with an EMPTY pool — a Home that has
/// content but cannot page — and `ui::home`'s pager test needs somewhere to page to. A fixture
/// that cannot express the app's ordinary state quietly limits what can be tested through it.
#[cfg(test)]
fn build_test(n: usize) -> SourceBuild {
    SourceBuild {
        cw: Vec::new(),
        shelves: vec![Shelf {
            title: "Continue Watching".into(),
            hub_id: "home.continue".into(),
            items: (0..n)
                .map(|i| PmsMovie { rk: (i + 1).to_string(), art: "/art".into(), ..PmsMovie::default() })
                .collect(),
        }],
    }
}

/// Test hook: put the store in a known place — one source in `state`, having answered with `items`
/// rows in one shelf. Home's read-out is a pure projection of that pair, and the states a host test
/// cannot reach for real (a live server answering, or refusing) are exactly the ones worth pinning.
#[cfg(test)]
pub(crate) fn seed_for_test(items: usize, state: HubState) {
    reset();
    let mut s = Src::new(ServerId::UNSET, String::new());
    s.state = state;
    if items > 0 {
        s.last = Some(build_test(items));
    }
    let srcs = vec![s];
    let build = merge(&srcs);
    *lock_srcs() = srcs;
    // leave `sync_roster` believing it is up to date — otherwise the next `pump` would replace this
    // synthetic source with whatever the (empty, in a host test) registry holds
    SEEN.store(roster_key(), Ordering::Relaxed);
    commit(build);
}

/// Drop everything and re-arm the fetch — the identity-change twin of [`crate::browse::reset`],
/// called from the same place (`install_pms`). Now that a failed fetch KEEPS the previous build,
/// a profile switch whose fetch fails would otherwise leave the previous user's shelves on screen;
/// this is the one place that must still wipe them.
pub(crate) fn reset() {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // a worker still running belongs to the old identity
    *RESULTS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
    *lock_srcs() = Vec::new();
    SEEN.store(u64::MAX, Ordering::Relaxed);
    // Adopt the section table's generation with the empty commit below: a bump from BEFORE this
    // reset is already reflected in "nothing", so it is not owed a re-merge. Left unadopted, the
    // next `pump` "caught up" on a generation some other era had moved and re-committed — freeing
    // the HUBS strings out from under a `hub_title` borrow held across that pump, which is how the
    // test suite read freed memory whenever another module's `browse::reset` ran in between.
    LAST_SECTIONS_GEN.store(crate::browse::sections_gen(), Ordering::SeqCst);
    commit((Vec::new(), Vec::new(), Vec::new()));
}
// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // NB every test that touches the catalog statics holds the crate-wide serial lock: they are
    // read from other modules' tests too (`ui::home` walks `hub_len`), which a module-local mutex
    // cannot see. `reset()` doubles as the teardown.
    //
    // No test here may leave a source in a state `pump` would KICK (not Ready, not fetching, no
    // backoff owed): a kick reaches for `plex::client_for`, and a slot another module's test
    // registered would send a real worker at a real address.

    fn sid(slot: u16) -> ServerId {
        ServerId::from_raw(slot)
    }

    /// One source of the table, seeded directly. `handle` empty = a server of our own.
    fn src(slot: u16, handle: &str, state: HubState, last: Option<SourceBuild>) -> Src {
        let mut s = Src::new(sid(slot), handle.into());
        s.state = state;
        s.last = last;
        s
    }

    /// Install a source table and the merge of it — the state a run of landings would have reached.
    /// Bypasses the roster, because a host test has no server registry to derive one from.
    fn seed(srcs: Vec<Src>) {
        let build = merge(&srcs);
        *lock_srcs() = srcs;
        SEEN.store(roster_key(), Ordering::Relaxed); // leave `sync_roster` idle
        commit(build);
    }

    /// A landing from the worker this source has out right now (current generation and seq) — what
    /// a real fetch would post. `None` is a failure.
    fn land(slot: u16, build: Option<SourceBuild>) {
        let s = sid(slot);
        let seq = lock_srcs().iter().find(|x| x.sid == s).map(|x| x.seq).unwrap_or(0);
        RESULTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Landing { gen: HUB_GEN.load(Ordering::SeqCst), seq, sid: s, build });
    }

    /// A drawable catalog row of one server. `thumb` is what `project` requires of a row, `art`
    /// what the hero pool requires of one.
    fn row(slot: u16, rk: &str) -> PmsMovie {
        PmsMovie {
            sid: sid(slot),
            rk: rk.into(),
            title: rk.into(),
            thumb: "/t.jpg".into(),
            art: "/a.jpg".into(),
            ..Default::default()
        }
    }
    fn shelf(slot: u16, title: &str, hub_id: &str, rks: &[&str]) -> Shelf {
        Shelf { title: title.into(), hub_id: hub_id.into(), items: rks.iter().map(|r| row(slot, r)).collect() }
    }
    /// A source's projection: `(lastViewedAt, rk)` deck entries plus whole shelves.
    fn built(slot: u16, cw: &[(i64, &str)], shelves: Vec<Shelf>) -> SourceBuild {
        SourceBuild {
            cw: cw.iter().map(|&(t, r)| CwItem { last_viewed_at: t, m: row(slot, r) }).collect(),
            shelves,
        }
    }
    /// The ratingKeys of shelf `h`, in drawn order.
    fn rks(h: usize) -> Vec<String> {
        (0..hub_len(h)).filter_map(|c| hub_item(h, c)).map(|m| m.rk.clone()).collect()
    }

    #[test]
    fn the_backoff_doubles_then_holds_at_the_ceiling() {
        assert_eq!(backoff_secs(1), RETRY_MIN_S, "the first retry is the shortest wait");
        assert_eq!(backoff_secs(2), 4.0);
        assert_eq!(backoff_secs(3), 8.0);
        assert_eq!(backoff_secs(4), 16.0);
        assert_eq!(backoff_secs(5), RETRY_MAX_S, "32s is past the ceiling");
        assert_eq!(backoff_secs(99), RETRY_MAX_S, "and it never grows past it (nor overflows)");
    }

    /// The bug the fetch state machine exists for: a failed fetch used to commit an EMPTY catalog,
    /// so one unreachable moment blanked a populated Home for good. A failure must leave every one
    /// of the three statics exactly as it found them.
    #[test]
    fn a_failed_landing_never_blanks_a_populated_home() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Ready, Some(build_test(3)))]);
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), 3);

        land(0, None);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Failed, "the failure must be distinguishable");
        assert_eq!(hub_count(), 1, "the shelves survive a failed refetch");
        assert_eq!(hub_len(0), 3);
        assert!(lock_srcs()[0].retry_s > 0.0, "and the next attempt is armed");
        reset();
    }

    /// A landing that carries a build commits it, and a success retires that source's backoff so
    /// its next failure starts at the bottom of the ladder instead of inheriting a 30s wait.
    #[test]
    fn a_successful_landing_commits_and_retires_the_backoff() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None)]);
        {
            let mut s = lock_srcs();
            landed_fail(&mut s[0]);
            landed_fail(&mut s[0]);
        }
        assert_eq!(hub_state(), HubState::Failed);

        land(0, Some(build_test(2)));
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_len(0), 2);
        assert_eq!(lock_srcs()[0].retry_n, 0);
        assert_eq!(lock_srcs()[0].retry_s, 0.0);
        reset();
    }

    /// An answer of "nothing" is an ANSWER: it must land as Ready (Home's empty state), not as a
    /// failure, or the screen would apologise for a server that is simply empty — and retry it
    /// forever.
    #[test]
    fn a_server_with_no_hubs_is_ready_and_empty_not_failed() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None)]);
        land(0, Some(SourceBuild::default()));
        pump(0.0);
        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_count(), 0);
        reset();
    }

    /// The countdown is real time (seconds of `dt`), not frames like `browse.rs`'s — a device
    /// that drops to 30fps must still retry on the same wall clock. NB `retry_due` keeps
    /// reporting due once it is spent; what makes an attempt happen only once is `kick`
    /// re-latching that source's single flight, not this.
    #[test]
    fn the_retry_countdown_fires_when_the_backoff_elapses() {
        let mut s = src(0, "", HubState::Loading, None);
        landed_fail(&mut s); // arms RETRY_MIN_S
        for _ in 0..3 {
            assert!(!retry_due(&mut s, 0.5), "0.5s at a time must not fire before the 2s wait is spent");
        }
        assert!(retry_due(&mut s, 0.5), "the fourth half-second spends it");
    }

    /// A retry still in flight when the account changes must not commit the PREVIOUS identity's
    /// hubs over the new one's: its landing carries the old generation and is dropped whole —
    /// neither committed nor blamed on the current fetch. The same drop catches a SUPERSEDED
    /// attempt at the same source within one generation, which is what the per-source seq is for:
    /// an authoritative fetch releases every single-flight latch, so without it the worker that
    /// call abandoned could still land on top of the one that replaced it.
    #[test]
    fn a_stale_or_superseded_landing_is_dropped_whole() {
        let _g = crate::testlock::serial();
        reset();
        let stale = HUB_GEN.load(Ordering::SeqCst);
        reset(); // the identity change
        seed(vec![src(0, "", HubState::Ready, Some(build_test(2)))]); // …and the new identity's catalog
        let s0 = sid(0);
        let cur = HUB_GEN.load(Ordering::SeqCst);
        {
            let mut r = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
            r.push(Landing { gen: stale, seq: 0, sid: s0, build: Some(build_test(9)) });
            r.push(Landing { gen: cur, seq: 99, sid: s0, build: Some(build_test(7)) });
        }
        pump(0.0);
        assert_eq!(hub_len(0), 2, "neither may replace the current catalog");
        assert_eq!(hub_state(), HubState::Ready, "nor be counted as a failure of the current one");
        reset();
    }

    // ---- the OPTIMISTIC local edit ---------------------------------------------------------
    //
    // The half of a view-state write the user actually sees (`crate::viewstate`): the shelves
    // change on the frame of the press, and the refetch the write's landing kicks is what
    // reconciles them. What is graded here is that the edit reaches every row the item occupies
    // and that the re-merge leaves the three statics addressing each other correctly — the reason
    // it edits each source's PROJECTION rather than splicing the committed catalog.

    /// A row that is part-way through, i.e. the shape a Continue Watching card really has.
    fn started(slot: u16, rk: &str) -> PmsMovie {
        PmsMovie { dur_ns: 90 * 60 * 1_000_000_000, resume_ms: 30 * 60_000, unwatched: false, ..row(slot, rk) }
    }

    /// Every appearance of the item flips, deck and shelves alike — a home screen that marked one
    /// of them and not the other would be two answers about one film on one screen. And the resume
    /// point goes with the flag: `poster_mark` reads progress AHEAD of watched, so a row keeping its
    /// old `viewOffset` wears its old bar and no tick, which reads as the press having done nothing.
    #[test]
    fn marking_an_item_watched_flips_every_row_that_names_it_and_retires_its_resume_bar() {
        let _g = crate::testlock::serial();
        reset();
        let mut b = SourceBuild {
            cw: vec![CwItem { last_viewed_at: 9, m: started(0, "7") }],
            shelves: vec![shelf(0, "Recently Added", "home.movies.recent", &["7", "8"])],
        };

        assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(true)));

        assert!(b.cw[0].m.watched && !b.cw[0].m.unwatched, "the deck card");
        assert_eq!(b.cw[0].m.resume_ms, 0, "…and its bar retires with the flag");
        assert!(b.shelves[0].items[0].watched, "the same film on another shelf");
        assert!(!b.shelves[0].items[1].watched, "and nothing else on it");

        // …and the reverse toggle is the exact inverse, on a container as much as on a leaf
        assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(false)));
        assert!(!b.cw[0].m.watched && b.cw[0].m.unwatched);
        reset();
    }

    /// An item on another server that happens to share the ratingKey is a DIFFERENT item — the rule
    /// `plex::same_item` exists for, applied to the one edit that writes to a row rather than
    /// reading one. A bare-key match here would tick a friend's film because you finished yours.
    #[test]
    fn an_edit_never_reaches_the_same_rating_key_on_another_server() {
        let _g = crate::testlock::serial();
        reset();
        let mut b = SourceBuild { cw: Vec::new(), shelves: vec![shelf(1, "Theirs", "h", &["7"])] };
        assert!(!apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(true)), "nothing matched");
        assert!(!b.shelves[0].items[0].watched);
        reset();
    }

    /// Remove from Continue Watching is a HIDE and nothing else: the card leaves the deck, keeps its
    /// resume point, and its appearances on every OTHER shelf are untouched — which is exactly what
    /// the server does (`plex::Client::remove_from_continue_watching`). Marking it watched instead
    /// would throw the position away, which is the mistake that endpoint exists to avoid.
    #[test]
    fn a_deck_removal_leaves_the_deck_only_and_keeps_the_resume_point() {
        let _g = crate::testlock::serial();
        reset();
        let mut b = SourceBuild {
            cw: vec![CwItem { last_viewed_at: 9, m: started(0, "7") }, CwItem { last_viewed_at: 8, m: started(0, "8") }],
            shelves: vec![Shelf {
                title: "Recently Added".into(),
                hub_id: "home.movies.recent".into(),
                items: vec![started(0, "7")],
            }],
        };

        assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::LeftTheDeck));

        assert_eq!(b.cw.len(), 1, "only the one asked for leaves");
        assert_eq!(b.cw[0].m.rk, "8");
        assert_eq!(b.shelves[0].items[0].rk, "7", "its other shelf keeps it");
        assert_eq!(b.shelves[0].items[0].resume_ms, 30 * 60_000, "…with the position it had");
        assert!(!b.shelves[0].items[0].watched, "…and its watch state untouched: this is a HIDE");
        reset();
    }

    /// The reason the edit goes through the projection and the pure `merge` rather than splicing
    /// `CATALOG`: a `HubRow` is a `start`/`len` WINDOW into one flat vec and the hero pool holds
    /// indices into the same, so a row removed by hand means fixing every window behind it. Here the
    /// deck loses a card and the shelf behind it must still draw exactly its own items.
    #[test]
    fn a_removed_deck_card_leaves_the_shelves_behind_it_correctly_addressed() {
        let _g = crate::testlock::serial();
        reset();
        let build = SourceBuild {
            cw: vec![CwItem { last_viewed_at: 9, m: started(0, "7") }, CwItem { last_viewed_at: 8, m: started(0, "8") }],
            shelves: vec![shelf(0, "Recently Added", "home.movies.recent", &["a", "b"])],
        };
        seed(vec![src(0, "", HubState::Ready, Some(build))]);
        assert_eq!(rks(0), vec!["7", "8"], "the deck as it stands");
        assert_eq!(rks(1), vec!["a", "b"]);

        assert!(edit_item(sid(0), "7", LocalEdit::LeftTheDeck));

        assert_eq!(rks(0), vec!["8"], "the card is gone from the deck");
        assert_eq!(rks(1), vec!["a", "b"], "and the shelf behind it still names its own items");
        assert_eq!(hub_count(), 2, "no shelf appeared or vanished");
        reset();
    }

    /// An item on no shelf at all — a Library-grid or Related page press — must not re-commit Home
    /// for nothing: the return value is what tells `viewstate` whether anything on screen moved, and
    /// a commit here would free the catalog strings out from under a live `hub_title` borrow for no
    /// reason at all.
    #[test]
    fn an_item_on_no_shelf_reports_no_edit_and_recommits_nothing() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
        assert!(!edit_item(sid(0), "not-on-home", LocalEdit::Watched(true)));
        assert_eq!(hub_len(0), 2, "Home is exactly as it was");
        reset();
    }

    // ---- the hero pool's ordering ----------------------------------------------------------
    //
    // Pure list arithmetic over `HeroSlot`, so these touch no static and take no lock. The pool is
    // written as the sequence of SOURCE handles its pages carry, "" being one of ours, which is
    // exactly what `merge` hands `own_items_first` after every shelf has contributed.

    fn pool_of(sources: &[&str]) -> Vec<HeroSlot> {
        sources.iter().enumerate().map(|(i, s)| HeroSlot { idx: i, source: s.to_string() }).collect()
    }
    fn sources_of(pool: &[HeroSlot]) -> Vec<&str> {
        pool.iter().map(|s| s.source.as_str()).collect()
    }

    /// The rule: one page moves to the front, nothing is dropped, and nothing else is reordered.
    #[test]
    fn a_borrowed_page_never_opens_the_door_while_we_have_one_of_our_own() {
        let mut p = pool_of(&["friend", "friend", "", "", "friend"]);
        own_items_first(&mut p);
        assert_eq!(sources_of(&p), ["", "friend", "friend", "", "friend"]);
        assert_eq!(p.iter().map(|s| s.idx).collect::<Vec<_>>(), [2, 0, 1, 3, 4], "the pages themselves survive");
    }

    #[test]
    fn a_pool_that_already_opens_on_one_of_ours_is_left_exactly_alone() {
        for start in [
            vec!["", "friend", "", "friend"],
            vec!["", "", ""],
            vec!["", "friend"],
        ] {
            let mut p = pool_of(&start);
            own_items_first(&mut p);
            assert_eq!(sources_of(&p), start, "an ordering that already holds must not be re-derived");
        }
    }

    /// Filtering instead of ordering would leave this account with NO hero at all.
    #[test]
    fn a_borrowed_only_account_still_gets_a_hero_and_it_holds_the_first_rotation() {
        let mut p = pool_of(&["friend", "friend2"]);
        own_items_first(&mut p);
        assert_eq!(sources_of(&p), ["friend", "friend2"], "nothing of ours to promote, so nothing moves");
    }

    #[test]
    fn the_ordering_holds_at_the_empty_and_single_page_ends() {
        let mut empty: Vec<HeroSlot> = Vec::new();
        own_items_first(&mut empty);
        assert!(empty.is_empty());
        for one in [vec![""], vec!["friend"]] {
            let mut p = pool_of(&one);
            own_items_first(&mut p);
            assert_eq!(sources_of(&p), one);
        }
    }

    // ---- the identity every merged row carries ----------------------------------------------

    /// The STAMPING contract, and the linchpin under every `(sid, rk)` test in the crate: if
    /// `project` did not stamp the server it was asked of onto every row, two servers' item `1`
    /// would be one item to the whole app.
    #[test]
    fn every_row_a_source_projects_is_stamped_with_the_server_it_was_asked_of() {
        let body = |rk: &str, title: &str| {
            format!(
                r#"{{"MediaContainer":{{"Hub":[{{"type":"movie","hubIdentifier":"home.movies.recent",
                   "title":"Recently Added","Metadata":[{{"ratingKey":"{rk}","type":"movie","title":"{title}",
                   "thumb":"/t.jpg","art":"/a.jpg"}}]}}]}}}}"#
            )
        };
        let parse = |s: String| {
            serde_json::from_str::<crate::plex::Envelope>(&s).expect("a PMS body parses").media_container
        };
        let empty = crate::plex::MediaContainer::default();
        let ours = sid(3);

        let b = project(&parse(body("1", "Ours")), &empty, ours);
        assert_eq!(b.shelves.len(), 1, "the shelf survived the title/poster filter");
        assert_eq!(b.shelves[0].items.len(), 1);
        assert_eq!(
            (b.shelves[0].items[0].sid, b.shelves[0].items[0].rk.as_str()),
            (ours, "1"),
            "the row names the server it came from"
        );

        // the same wire body parsed for ANOTHER server must not produce rows that compare equal to
        // the first server's — this is the whole reason the field exists, and with a MERGED Home
        // the two now sit in one catalog rather than in two runs of the app
        let theirs = sid(4);
        let b2 = project(&parse(body("1", "Theirs")), &empty, theirs);
        let (m1, m2) = (&b.shelves[0].items[0], &b2.shelves[0].items[0]);
        assert!(
            !crate::plex::same_item((m1.sid, &m1.rk), (m2.sid, &m2.rk)),
            "one ratingKey from two servers must never alias"
        );

        // …and merged, both survive into the catalog and into the hero pool
        let (cat, hubs, pool) = merge(&[
            src(3, "", HubState::Ready, Some(b)),
            src(4, "friend", HubState::Ready, Some(b2)),
        ]);
        assert_eq!(cat.len(), 2);
        assert_eq!(hubs.len(), 2, "one shelf each, neither folded into the other");
        assert_eq!(pool.len(), 2, "…and so does the hero pool, by construction");
    }

    /// **A ratingKey alone does not name an item once a second server exists.** Both servers
    /// number from 1, so the merged catalog below holds two different films called `"1"` — and the
    /// bare-key scan this replaced returned the FIRST of them to every caller, which is a play of
    /// the wrong film from the item menu and the wrong backdrop on the detail page.
    #[test]
    fn a_catalog_row_is_found_by_its_server_and_key_never_by_the_key_alone() {
        let _g = crate::testlock::serial();
        reset();
        let (a, b) = (sid(0), sid(1));
        let mk = |s: ServerId, rk: &str, title: &str| PmsMovie {
            sid: s,
            rk: rk.to_string(),
            title: title.to_string(),
            ..Default::default()
        };
        // ours first, so a bare-key scan would always answer with it
        let cat = vec![mk(a, "1", "ours"), mk(a, "2", "ours too"), mk(b, "1", "the friend's")];
        let hubs = vec![HubRow {
            title: "Continue Watching".into(),
            hub_id: "home.continue".into(),
            source: String::new(),
            start: 0,
            len: 3,
        }];
        commit((cat, hubs, Vec::new()));

        assert_eq!(index_of_rk(a, "1"), 0);
        assert_eq!(index_of_rk(b, "1"), 2, "the SHARE's item 1, not ours");
        assert_eq!(movie(index_of_rk(b, "1") as usize).map(|m| m.title.as_str()), Some("the friend's"));
        assert_eq!(index_of_rk(a, "2"), 1);
        assert_eq!(index_of_rk(b, "2"), -1, "a key our server has and the share does not is a MISS");
        assert_eq!(index_of_rk(ServerId::UNSET, "1"), -1, "and an unscoped lookup answers for neither");
        reset();
    }

    /// `reset` is the profile-switch wipe: the previous user's shelves must not survive it, and
    /// the state machine must come back as a fresh boot's (Loading, no source, no backoff owed).
    #[test]
    fn reset_wipes_the_catalog_and_re_arms_the_fetch() {
        let _g = crate::testlock::serial();
        seed(vec![src(0, "", HubState::Ready, Some(build_test(4)))]);
        reset();
        assert_eq!(hub_count(), 0);
        assert_eq!(catalog().len(), 0);
        assert_eq!(hero_pool_len(), 0);
        assert_eq!(hub_state(), HubState::Loading);
        assert!(lock_srcs().is_empty());
    }

    // ---- what a SECOND source changes -----------------------------------------------------------

    /// The failure this unit exists for. Home used to be one `?` chain over one server, so a single
    /// dead share aborted the whole build: nothing committed, and on a cold boot the user got a
    /// whole-screen "Can't reach your Plex server" about their own working library. A source's
    /// verdict is now its own — the one that answered commits, the one that failed contributes
    /// nothing and backs off alone.
    #[test]
    fn one_failing_source_still_commits_the_other() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Loading, None), src(1, "friend", HubState::Loading, None)]);

        land(0, Some(built(0, &[], vec![shelf(0, "Recently Added", "home.movies.recent", &["a1", "a2"])])));
        land(1, None);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready, "one server answering is an answered Home");
        assert_eq!(hub_count(), 1, "the dead share contributes NOTHING — no heading, no empty shelf");
        assert_eq!(hub_len(0), 2);
        assert_eq!(hub_source(0), "", "and the shelf that did arrive is our own, so it is unannotated");
        let s = lock_srcs();
        assert_eq!(s[0].state, HubState::Ready);
        assert_eq!(s[1].state, HubState::Failed);
        assert!(s[1].retry_s > 0.0, "the share retries on its own ladder…");
        assert_eq!(s[0].retry_s, 0.0, "…and the working server owes nothing");
        drop(s);
        reset();
    }

    /// …and the same rule once BOTH sources are populated, which is the state a user is actually
    /// sitting in front of when a friend's server goes to sleep. The failing source keeps the
    /// shelves it last answered with, the working source is not touched at all, and neither the
    /// catalog nor the deck loses a row.
    #[test]
    fn a_failing_source_leaves_a_populated_home_completely_intact() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[(9, "own-cw")], vec![shelf(0, "Films", "a.recent", &["a1", "a2"])]))),
            src(1, "friend", HubState::Ready, Some(built(1, &[(5, "their-cw")], vec![shelf(1, "Film Club", "b.recent", &["b1"])]))),
        ]);
        let before: Vec<(&str, &str, usize)> =
            (0..hub_count()).map(|i| (hub_title(i), hub_source(i), hub_len(i))).collect();
        assert_eq!(before.len(), 3, "deck + one shelf each");
        let rows = catalog().len();

        land(1, None); // the share stops answering
        pump(0.0);

        let after: Vec<(&str, &str, usize)> =
            (0..hub_count()).map(|i| (hub_title(i), hub_source(i), hub_len(i))).collect();
        assert_eq!(after, before, "not one shelf, heading or row moved");
        assert_eq!(catalog().len(), rows);
        assert_eq!(rks(0), ["own-cw", "their-cw"], "and the merged deck kept both servers' items");
        assert_eq!(hub_state(), HubState::Ready, "Home is not failed while a source is answering");
        assert_eq!(lock_srcs()[0].retry_s, 0.0, "the working source owes no backoff");
        reset();
    }

    /// A PARTIAL landing repaints. A settled Home stops presenting entirely (`ui::idle`), so a
    /// source arriving seconds after the owned server did — which is the normal shape of this
    /// feature, not an edge case — would otherwise draw its shelves invisibly until the next
    /// keypress. The failure half matters just as much: a retry that fails changes the status
    /// caption under an empty Home.
    #[test]
    fn a_source_landing_repaints_a_settled_home() {
        let _g = crate::testlock::serial();
        reset();
        crate::ui::idle::set_enabled(true);
        seed(vec![src(0, "", HubState::Ready, Some(build_test(1))), src(1, "friend", HubState::Loading, None)]);

        for (what, build) in [("a share arriving", Some(build_test(2))), ("a share failing", None)] {
            crate::ui::idle::should_present(0); // takes-and-clears whatever was already pending
            assert!(!crate::ui::idle::should_present(0), "the panel is settled with nothing happening");
            land(1, build);
            pump(0.0);
            assert!(crate::ui::idle::should_present(0), "{what} must invalidate the frame");
        }
        reset();
    }

    /// The total-failure read-out is reserved for a total failure. Any source answering makes Home
    /// answered; a mix of failed and still-loading is still loading.
    #[test]
    fn only_every_source_failing_reads_as_a_failed_home() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend", HubState::Loading, None)]);
        assert_eq!(hub_state(), HubState::Loading, "one source still trying is not a dead Home");

        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend", HubState::Ready, None)]);
        assert_eq!(hub_state(), HubState::Ready, "a share being down says nothing about our own");

        seed(vec![src(0, "", HubState::Failed, None), src(1, "friend", HubState::Failed, None)]);
        assert_eq!(hub_state(), HubState::Failed, "everything down IS the whole-screen case");
        reset();
    }

    /// Continue Watching is ONE shelf across every source, ordered by when the owner last watched —
    /// so a borrowed item legitimately holds first position, and the heading therefore cannot claim
    /// an owner. It carries no annotation at all. This is the official client's own shape: the
    /// owner's screenshots show a friend's films sitting BETWEEN their own, in one row.
    #[test]
    fn continue_watching_merges_across_sources_by_last_viewed() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[(300, "own-old"), (100, "own-oldest")], vec![]))),
            src(1, "friend", HubState::Ready, Some(built(1, &[(900, "their-new"), (200, "their-mid")], vec![]))),
        ]);

        assert_eq!(hub_count(), 1, "one deck, not one per server");
        assert!(hub_is_continue(0));
        assert_eq!(hub_source(0), "", "a shelf drawn from two servers cannot be named by one of them");
        // The timestamps INTERLEAVE on purpose: a per-source concatenation, which is what the
        // obvious implementation of "merge" does, would give own-old, own-oldest, their-new,
        // their-mid and pass any test written with one server's deck in front of the other's.
        assert_eq!(rks(0), ["their-new", "own-old", "their-mid", "own-oldest"]);
        reset();
    }

    /// Every OTHER shelf keeps its source: the owner's handle for a borrowed server, empty for our
    /// own. And the groups stay contiguous in roster order — adjacency is the grouping device, so a
    /// source's shelves may never be interleaved with another's.
    #[test]
    fn every_other_shelf_carries_its_source_and_the_groups_stay_contiguous() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[(1, "cw")], vec![shelf(0, "Films", "a.recent", &["a"])]))),
            src(1, "friend", HubState::Ready, Some(built(1, &[], vec![shelf(1, "Film Club", "b.recent", &["b"]), shelf(1, "Club TV", "b.tv", &["b2"])]))),
            src(2, "friend2", HubState::Ready, Some(built(2, &[], vec![shelf(2, "Docs", "c.recent", &["c"])]))),
        ]);

        let by_row: Vec<(&str, &str)> = (0..hub_count()).map(|i| (hub_title(i), hub_source(i))).collect();
        assert_eq!(
            by_row,
            [
                ("Continue Watching", ""),
                ("Films", ""),
                ("Film Club", "friend"),
                ("Club TV", "friend"),
                ("Docs", "friend2")
            ],
            "deck first, then our own, then each share whole"
        );
        reset();
    }

    /// A source that has never answered contributes nothing — no heading, no empty shelf, no
    /// spinner row (which would also hold the panel presenting for a source that isn't coming).
    /// One that HAS answered and has since failed keeps what it last had: a transient failure must
    /// not reflow the shelves under the focus ring.
    #[test]
    fn a_source_that_never_answered_draws_nothing_at_all() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[], vec![shelf(0, "Films", "a.recent", &["a"])]))),
            src(1, "friend", HubState::Failed, None),
            src(2, "friend2", HubState::Failed, Some(built(2, &[], vec![shelf(2, "Docs", "c.recent", &["c"])]))),
        ]);
        let by_row: Vec<(&str, &str)> = (0..hub_count()).map(|i| (hub_title(i), hub_source(i))).collect();
        assert_eq!(by_row, [("Films", ""), ("Docs", "friend2")]);
        reset();
    }

    /// A source that leaves the roster takes its shelves with it at the next read — the one thing
    /// that DOES remove a live source's rows, because "gone" (un-pinned, or a revoked share) is a
    /// fact about the grant rather than about a fetch that happened to fail.
    #[test]
    fn a_source_that_leaves_the_roster_stops_contributing() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[], vec![shelf(0, "Films", "a.recent", &["a"])]))),
            src(1, "friend", HubState::Ready, Some(built(1, &[], vec![shelf(1, "Film Club", "b.recent", &["b"])]))),
        ]);
        assert_eq!(hub_count(), 2);

        // the registry a host test has is empty, so the roster this recomputes to is empty too
        SEEN.store(u64::MAX, Ordering::Relaxed);
        sync_roster();

        assert_eq!(hub_count(), 0, "both un-rostered sources' shelves are gone");
        assert!(lock_srcs().is_empty());
        reset();
    }

    /// **The pin's grain is a LIBRARY, and `/hubs` is a whole-SERVER request.** So the server-level
    /// gate cannot be the only one: unpinning one library of a two-library server left every one of
    /// its items on Home, which is what the owner hit ("I disabled local server lib from home but
    /// it persisted"). Each row carries its own `librarySectionID`, and that is the join.
    ///
    /// Unknown passes in both directions — a row whose server sent no id, and a library the section
    /// table has not enumerated — because hiding what cannot be classified empties Home on the
    /// frame it boots.
    #[test]
    fn an_unpinned_library_keeps_its_items_off_home_even_when_its_server_feeds_it() {
        let (a, b) = (sid(0), sid(1));
        // server A has two libraries: 1 pinned, 2 NOT. Server B has one, pinned.
        let pins = vec![(a, 1, true), (a, 2, false), (b, 1, true)];
        let row = |s: ServerId, sec: i64| PmsMovie { sid: s, sec, ..Default::default() };

        assert!(item_pinned(&pins, &row(a, 1)), "a pinned library of a server that feeds Home");
        assert!(!item_pinned(&pins, &row(a, 2)), "…and its UNPINNED sibling, on the same server");
        assert!(item_pinned(&pins, &row(b, 1)));

        // the same section KEY on another server is a different library — both servers number from 1
        let pins2 = vec![(a, 1, false), (b, 1, true)];
        assert!(!item_pinned(&pins2, &row(a, 1)));
        assert!(item_pinned(&pins2, &row(b, 1)), "keys collide across servers; the pair does not");

        // unknown passes, both ways
        assert!(item_pinned(&pins, &row(a, 0)), "the server sent no librarySectionID");
        assert!(item_pinned(&pins, &row(a, 9)), "a library the section table has not enumerated");
        assert!(item_pinned(&[], &row(a, 1)), "nothing discovered yet");
    }

    /// **The pin store is the seam, and "no pinned library" only means something for a server whose
    /// libraries have been ENUMERATED.** `/library/sections` lands asynchronously and always after
    /// `pms_fetch_hubs`, and for a source other than the current one it lands only while the LIBRARY
    /// screen is up — so a booted app knows the roster and none of its shares' libraries.
    ///
    /// Reading that state as "not pinned" is what produced the owner's report that a share
    /// "appeared on the home screen only after I watched the library": the pin said On the whole
    /// time, and Home was asking a question the section table could not yet answer.
    #[test]
    fn a_server_whose_libraries_are_unknown_is_undecided_not_unpinned() {
        let (a, b) = (sid(0), sid(1));
        // nothing discovered anywhere: every granted server feeds Home
        assert!(feeds_home(a, &[], &[]));
        assert!(feeds_home(b, &[], &[]));

        // **the regression**: `a` is discovered and pinned, `b` is in the roster and has not been
        // enumerated. `b` is UNDECIDED and must still feed Home.
        assert!(feeds_home(b, &[a], &[a]), "a share nobody has enumerated is not a share turned off");

        // …and once `b`'s libraries ARE known, the pin is a real answer in both directions
        assert!(!feeds_home(b, &[a], &[a, b]), "enumerated, granted, browsable — and not pinned");
        assert!(feeds_home(b, &[a, b], &[a, b]), "pinned");
        assert!(feeds_home(a, &[a], &[a, b]));
    }

    /// The budget is split, not raced for. Whoever is asked first used to spend it — and `/hubs`
    /// promotes several rows per library, so one four-library server already overruns
    /// [`MAX_SHELVES`] and the share behind it drew nothing.
    #[test]
    fn the_budget_is_shared_so_neither_source_starves_the_other() {
        assert_eq!(allot(10, &[4, 4]), [4, 4], "a budget nobody exhausts is not rationed");
        assert_eq!(allot(10, &[99, 99]), [5, 5], "two greedy sources split it evenly");
        assert_eq!(allot(10, &[2, 99]), [2, 8], "what one does not want is passed on, not wasted");
        assert_eq!(allot(10, &[99, 0, 99]), [5, 0, 5], "…and RE-DIVIDED, not given to whoever is first");
        assert_eq!(allot(1, &[9, 9, 9]), [1, 0, 0], "a budget below one each reaches as many as it can");
        assert_eq!(allot(10, &[]), Vec::<usize>::new());

        let _g = crate::testlock::serial();
        reset();
        let many = |slot: u16, tag: &str| {
            (0..30).map(|i| shelf(slot, &format!("{tag}{i}"), "x", &["r"])).collect::<Vec<_>>()
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[], many(0, "a")))),
            src(1, "friend", HubState::Ready, Some(built(1, &[], many(1, "b")))),
        ]);
        assert_eq!(hub_count(), MAX_SHELVES, "the total cap still holds");
        let theirs = (0..hub_count()).filter(|&i| hub_source(i) == "friend").count();
        assert_eq!(theirs, MAX_SHELVES / 2, "and the share gets its half rather than the leftovers");
        reset();
    }

    /// The same split over catalog ROWS, which is the cap the shelves' items come out of.
    #[test]
    fn the_row_budget_is_shared_too() {
        let _g = crate::testlock::serial();
        reset();
        // enough shelves, each already at the per-shelf ceiling, that the ROW cap is what binds
        let fat = |slot: u16, tag: &str| {
            (0..20)
                .map(|s| {
                    let keys: Vec<String> = (0..MAX_SHELF_ITEMS).map(|i| format!("{tag}-{s}-{i}")).collect();
                    shelf(slot, &format!("{tag}{s}"), "x.recent", &keys.iter().map(|r| r.as_str()).collect::<Vec<_>>())
                })
                .collect::<Vec<_>>()
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[], fat(0, "a")))),
            src(1, "friend", HubState::Ready, Some(built(1, &[], fat(1, "b")))),
        ]);
        assert_eq!(catalog().len(), PMS_MAX_MOVIES, "the total cap still holds");
        let theirs: usize = (0..hub_count()).filter(|&i| hub_source(i) == "friend").map(hub_len).sum();
        assert_eq!(theirs, PMS_MAX_MOVIES / 2, "and the share gets half the rows, not the leftovers");
        reset();
    }

    /// The merged deck is capped at what the grid can ADDRESS. Three sources' Continue Watching is
    /// up to 36 cards, and past `MAX_SHELF_ITEMS` `ui::home`'s focus ring and its OK dispatch clamp
    /// differently — the ring stops at the last addressable card while the press opens whatever
    /// column the raw index names. Unreachable with one server, which is why the cap lives here now.
    #[test]
    fn the_merged_deck_is_capped_at_what_the_grid_can_address() {
        let _g = crate::testlock::serial();
        reset();
        let deck = |slot: u16, tag: &str| {
            let v: Vec<(i64, String)> = (0..12).map(|i| (i, format!("{tag}{i}"))).collect();
            built(slot, &v.iter().map(|(t, r)| (*t, r.as_str())).collect::<Vec<_>>(), vec![])
        };
        seed(vec![
            src(0, "", HubState::Ready, Some(deck(0, "a"))),
            src(1, "friend", HubState::Ready, Some(deck(1, "b"))),
            src(2, "friend2", HubState::Ready, Some(deck(2, "c"))),
        ]);
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), MAX_SHELF_ITEMS, "36 cards merged, 24 drawable");
        reset();
    }

    /// The hero pool, on a MERGED deck. Two things it can only get right by knowing which source
    /// each ROW came from: our own item opens the rotation, and two servers' identical ratingKeys
    /// are two different films. The deck's own `source` is empty by design, so a slot that read its
    /// handle from its shelf would attribute every borrowed film to nobody — and `own_items_first`
    /// would think one of ours had already opened the door.
    #[test]
    fn the_hero_pool_opens_on_our_own_item_and_never_dedups_across_servers() {
        let _g = crate::testlock::serial();
        reset();
        seed(vec![
            src(0, "", HubState::Ready, Some(built(0, &[(100, "1")], vec![]))),
            src(1, "friend", HubState::Ready, Some(built(1, &[(900, "1")], vec![]))),
        ]);
        assert_eq!(rks(0), ["1", "1"], "the deck orders them by recency: theirs first");
        assert_eq!(hero_pool_len(), 2, "one ratingKey, two servers, two films");
        assert_eq!(hero_pool_source(0), "", "the door opens on our own library");
        assert_eq!(hero_pool_source(1), "friend", "…and the borrowed film rotates in behind it, attributed");
        assert!(
            std::ptr::eq(hero_pool_item(0).unwrap(), movie(1).unwrap()),
            "catalog row 1 is ours (row 0 is theirs, watched later)"
        );
        reset();
    }
}