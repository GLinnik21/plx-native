//! Plex library fetch/parse into the private catalog (was src/pms.c), read by the UI
//! via movie()/hub_item()/hero_pool_item(), plus urlenc_str (shared by posters/route).
//! The fetch + JSON parse go through the typed `crate::plex` client (serde DTOs) — no
//! hand-built paths or `Value` scraping here.
use std::os::raw::c_int;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

const PMS_MAX_MOVIES: usize = 256;

/// A catalog row — owned strings (the old C-ABI fixed `[u8; N]` buffers are gone; no C
/// consumer remains). Fields pub(crate) so the UI / route / player read them directly.
#[derive(Default)]
pub struct PmsMovie {
    pub(crate) title: String,
    pub(crate) year: c_int,
    pub(crate) rating: String,
    pub(crate) dur_ns: i64,
    pub(crate) part: String,
    pub(crate) thumb: String,
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
/// `refetch_hubs_reconcile` is the one mutation ritual and re-resolves the open surfaces).
pub(crate) fn movie(i: usize) -> Option<&'static PmsMovie> {
    catalog().get(i)
}
/// catalog index whose ratingKey == `rk` (for the /tmp/plxnative-detail probe), or -1
pub(crate) fn index_of_rk(rk: &str) -> c_int {
    catalog().iter().position(|m| m.rk == rk).map(|i| i as c_int).unwrap_or(-1)
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
/// pub(crate): the Library browse store (`browse.rs`) maps its paged listings with it too.
pub(crate) fn parse_item(it: &crate::plex::Metadata) -> PmsMovie {
    let mut m = PmsMovie::default();
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

// ---- pinning: which libraries reach Home ----------------------------------------------------

// The presses that drive this — the Sources list's `On Home` level and the first-run route —
// land in sibling units; the model goes in first so both are written against ONE answer instead
// of each inventing its own. Until they arrive, most of the surface below is reachable only from
// the tests, which is what this allow is for.
#[allow(dead_code)]
pub(crate) mod pin {
    //! **Which libraries feed Home** — the one user-facing control shared servers introduce, and
    //! the only thing in the app that a granted library can be turned off from.
    //!
    //! Three states, orthogonal, and conflating any two of them is the bug this module exists to
    //! prevent:
    //!
    //! - **granted** — plex.tv's answer. Not a setting. The app has no server on/off at all; a
    //!   share you were given is browsable whether or not it is pinned.
    //! - **pinned** — the only control there is. A pinned library feeds Home; an unpinned one
    //!   does not. *Nothing else changes.*
    //! - **reachable** — a fact about right now. A pinned server that does not answer still reads
    //!   `On`, because nothing was turned off (hiding it would read as a revoked share).
    //!
    //! **Home consults the pin, and nothing else does.** The section table, the tab strip, the
    //! browse grid's paging, the sort menu and the A–Z rail all come from the GRANT. That is why
    //! unpinning your own server is legitimate and gets no warning: a friend's films on your front
    //! door, your own library still one press away on its own tab. The single call site is
    //! [`super::build_hubs`], which is what makes the claim checkable — grep `feeds_home`.
    //!
    //! ## The rules, and where each is enforced
    //!
    //! 1. **Defaults** ([`resolve`]): your own libraries pinned, a friend's not. The point of
    //!    asking is not to put a stranger's shelves on your front door unannounced.
    //! 2. **The last pinned library cannot be unpinned** ([`apply`]). An empty Home is the only
    //!    real failure this control can produce, so the refusal lives in the data layer and no UI
    //!    can route around it. (The list still draws the row live — only its *value* dims. Dim
    //!    means unavailable, and that library is the one that works.)
    //! 3. **Unknown is not none.** An empty persisted table means nobody has answered yet, so the
    //!    defaults apply; an empty ROSTER means we have not been told what exists yet, so the
    //!    gate passes everything ([`PinSet::feeds_home`]). Both readings exist because the
    //!    opposite one shows an empty Home on a first boot after an upgrade.
    //! 4. **Continue Watching's merge is conditional on pinning** — a borrowed item joins the
    //!    merged shelf only once its library is pinned. It falls out of rule 3's single gate: the
    //!    shelf is built from items, and every item goes through it.
    //!
    //! ## Threading
    //!
    //! The resolved set is main-thread state like every other static in this module. The hub
    //! fetch runs on a worker, so it takes a [`snapshot`] **captured at the spawn site** and
    //! carries it in — nothing here is read from a worker thread.
    use crate::plex::session::{self, LibraryPin};
    use std::ptr::{addr_of, addr_of_mut};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A library's identity across servers: `machineIdentifier` + its section key. A section key
    /// is only unique within one server (every PMS numbers its first library `1`).
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub(crate) struct LibKey {
        pub(crate) machine_id: String,
        pub(crate) section: i64,
    }

    impl LibKey {
        pub(crate) fn new(machine_id: &str, section: i64) -> Self {
            LibKey { machine_id: machine_id.to_string(), section }
        }
    }

    /// One granted library, as the pin model needs to see it. Everything else about a library —
    /// its title, its type, the handle of the friend who shared it, whether it answered — belongs
    /// to the roster that owns the grant, not here.
    ///
    /// It maps straight off what the roster already holds: `machine_id` is the registry key
    /// (`plex::account::Resource::client_identifier`, i.e. `Client::machine_id`), `section` is a
    /// `LibrarySection::key` from that server's `/library/sections`, and `owned` is
    /// `Resource::owned` — plex.tv's own word for it, not a guess derived from `source_title`.
    #[derive(Clone, Debug)]
    pub(crate) struct Granted {
        pub(crate) key: LibKey,
        /// the account's own server, as opposed to a share someone gave it
        pub(crate) owned: bool,
    }

    /// One resolved library: is it on Home, and is it ours. `owned` rides along because both
    /// readers need it — the rescue in [`resolve`] prefers your own libraries, and the Sources
    /// list groups by server.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub(crate) struct Row {
        pub(crate) key: LibKey,
        pub(crate) owned: bool,
        pub(crate) on: bool,
    }

    /// The resolved answer for every granted library: what the persisted table says, or the
    /// default where it says nothing. One value, read by Home AND by the list that governs it —
    /// they must never be able to disagree, which is why the rescue in [`resolve`] is here and
    /// not a floor Home applies on its own.
    #[derive(Clone, Default, PartialEq, Eq, Debug)]
    pub(crate) struct PinSet {
        rows: Vec<Row>,
    }

    /// What [`set`] did — the last-pinned refusal is a value, not a silent no-op, so the UI can
    /// tell "you turned it off" from "that one is holding Home up".
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) enum Set {
        /// the pin moved (and was persisted)
        Applied,
        /// already in that state — nothing to do
        Unchanged,
        /// refused: it is the last pinned library, and Home may not be emptied
        LastPinned,
        /// no such library in the granted roster (a stale row, or the roster has not landed)
        NotGranted,
    }

    /// Resolve the roster against the persisted decisions. **Pure** — the whole model is here.
    ///
    /// Per library: the recorded answer if there is one, else the default (own → on, shared →
    /// off). Then the rescue below, for a non-empty roster that resolves to NOTHING pinned.
    ///
    /// The rescue is not a floor over the user's choice — the last-pinned rule means they cannot
    /// reach an empty set by pressing OK. It catches the two ways the WORLD can produce one: an
    /// account with **no libraries of its own** (a borrowed-only install is not a mode — it draws
    /// the same Home as anyone else), and a grant disappearing out from under the only pin the
    /// user had.
    ///
    /// A rescued value is a REAL value, not a private correction: the Sources list is this same
    /// function, so the user sees those rows reading `On`, and the next [`record`] writes them
    /// down as the answers they are. That is deliberate. The alternative — keeping the rescue
    /// ephemeral so the file still says `off` — makes the file disagree with the screen, and Home
    /// then changes on its own the day the missing grant comes back, which is the harder bug to
    /// explain. Nothing here can write something the user was not shown.
    ///
    /// **Which libraries it pins is the whole subtlety**, because the alternative to an empty Home
    /// must not be a stranger's shelves on the front door — the thing rule 1 exists to prevent. It
    /// takes the first tier below that matches anything, and takes ONE row from a tier that
    /// contradicts an answer, all of a tier that contradicts nothing. So it overrules a recorded
    /// "off" only where the alternative is an empty Home, overrules YOUR OWN first (an owned
    /// library on your own Home is never a surprise), and takes back exactly one when it does.
    pub(crate) fn resolve(roster: &[Granted], persisted: &[LibraryPin]) -> PinSet {
        let recorded: Vec<Option<bool>> = roster
            .iter()
            .map(|g| {
                persisted
                    .iter()
                    .find(|p| p.machine_id == g.key.machine_id && p.section == g.key.section)
                    .map(|p| p.pinned)
            })
            .collect();
        let mut rows: Vec<Row> = roster
            .iter()
            .zip(&recorded)
            .map(|(g, rec)| Row { key: g.key.clone(), owned: g.owned, on: rec.unwrap_or(g.owned) })
            .collect();
        if !rows.is_empty() && !rows.iter().any(|r| r.on) {
            // Tiers, in order, and how much of each to take back. Note what is NOT here: "your own,
            // unanswered". Such a row defaults ON, so it cannot be present when nothing is on —
            // the rescue only ever meets libraries that are off for a REASON.
            type Tier = fn(&Row, bool) -> bool;
            const TIERS: [(Tier, bool); 3] = [
                // yours, all of which must be answered "off" to be here — so take back ONE, the
                // fewest answers that can un-empty Home, and yours before anyone else's.
                (|r, _| r.owned, true),
                // shares nobody has ruled on: no answer to contradict, so all of them. This is the
                // borrowed-only account, which is not a mode — it gets the Home an owner gets.
                (|_, answered| !answered, false),
                // everything is answered off and none of it is yours: one, so the door opens.
                (|_, _| true, true),
            ];
            let hits = |t: Tier| -> Vec<bool> {
                rows.iter().zip(&recorded).map(|(r, rec)| t(r, rec.is_some())).collect()
            };
            if let Some((h, one)) = TIERS.iter().map(|&(t, one)| (hits(t), one)).find(|(h, _)| h.contains(&true)) {
                for (r, _) in rows.iter_mut().zip(h).filter(|(_, hit)| *hit) {
                    r.on = true;
                    if one {
                        break;
                    }
                }
            }
        }
        PinSet { rows }
    }

    /// Flip one library, enforcing the last-pinned rule. **Pure** — [`set`] is this plus the
    /// statics and the write, so the rule is testable without a session file.
    pub(crate) fn apply(rows: &mut [Row], key: &LibKey, on: bool) -> Set {
        let Some(i) = rows.iter().position(|r| &r.key == key) else { return Set::NotGranted };
        if rows[i].on == on {
            return Set::Unchanged;
        }
        if !on && rows.iter().filter(|r| r.on).count() <= 1 {
            return Set::LastPinned; // the only real failure this control can produce
        }
        rows[i].on = on;
        Set::Applied
    }

    impl PinSet {
        /// Does an item from this library reach Home?
        ///
        /// **Unknown passes.** An empty set means no roster has been installed yet (boot, or a
        /// build with no roster at all) and a key we have never heard of is equally unanswered —
        /// in both cases the honest answer is the item, not a blank Home. The gate can only ever
        /// subtract from what the user has explicitly answered.
        ///
        /// Compares the fields rather than building a `LibKey` to look one up: this runs once per
        /// item of every hub of both responses, and the temporary key would be a heap allocation
        /// per item — a few hundred per fetch, on an ARMv7 set, to answer a string comparison.
        pub(crate) fn feeds_home(&self, machine_id: &str, section: i64) -> bool {
            self.rows
                .iter()
                .find(|r| r.key.section == section && r.key.machine_id == machine_id)
                .map(|r| r.on)
                .unwrap_or(true)
        }
        /// One library's row, or None when the roster does not name it. **Everything that asks a
        /// question about a SPECIFIC library goes through here**, because [`feeds_home`]'s
        /// "unknown passes" answer is right for an item reaching Home and wrong for every other
        /// question: an unknown key is not "the last pinned library", and it is not a row the
        /// Sources list can draw.
        ///
        /// [`feeds_home`]: PinSet::feeds_home
        pub(crate) fn row(&self, key: &LibKey) -> Option<&Row> {
            self.rows.iter().find(|r| &r.key == key)
        }
        /// The libraries Home should fetch. (A multi-source Home fetches only these; a source
        /// with none of its libraries pinned is not contacted for Home at all.)
        pub(crate) fn pinned(&self) -> impl Iterator<Item = &LibKey> {
            self.rows.iter().filter(|r| r.on).map(|r| &r.key)
        }
        /// How many libraries feed Home.
        pub(crate) fn pinned_count(&self) -> usize {
            self.rows.iter().filter(|r| r.on).count()
        }
        /// This library is pinned and is the only one — the row whose value the list draws dimmed,
        /// and the one press [`apply`] refuses.
        pub(crate) fn is_last_pinned(&self, key: &LibKey) -> bool {
            self.pinned_count() == 1 && self.row(key).is_some_and(|r| r.on)
        }
        /// A roster has been resolved (as opposed to "we have not been told yet").
        pub(crate) fn is_known(&self) -> bool {
            !self.rows.is_empty()
        }
        /// The whole table, for the Sources list: one row per granted library, in roster order.
        /// Borrowed from THIS set — take a [`snapshot`] and read it from that, never from the
        /// process-wide one, which [`set`] mutates and [`install_roster`] replaces wholesale.
        pub(crate) fn rows(&self) -> &[Row] {
            &self.rows
        }
        /// The libraries Home must leave out — the only part of this set Home can observe, since
        /// an unknown key passes. Two sets with the same exclusions produce the same Home, which
        /// is what [`install_roster`] compares to decide whether a refetch is owed at all.
        /// SORTED, because roster order is plex.tv's to choose and nothing promises it is stable:
        /// compared in arrival order, the same libraries coming back shuffled would read as a
        /// change and refetch Home for nothing — the very thing the comparison is here to avoid.
        fn exclusions(&self) -> Vec<&LibKey> {
            let mut out: Vec<&LibKey> = self.rows.iter().filter(|r| !r.on).map(|r| &r.key).collect();
            out.sort_by(|a, b| (&a.machine_id, a.section).cmp(&(&b.machine_id, b.section)));
            out
        }
    }

    // ---- process state (main thread only) ----------------------------------------------------

    /// The resolved answer. Empty until a roster lands — which the gate reads as "unknown".
    ///
    /// The roster ITSELF is deliberately not kept beside it. There is no re-resolve path — [`set`]
    /// moves a row in place and [`record`] writes it through — so a stored copy would be state
    /// [`reset`] has to keep in sync for nothing, and it would suggest a "re-resolve from what we
    /// last knew" that does not exist. The roster's owner already holds it; it is handed here to be
    /// resolved, not to be stored.
    static mut PINS: PinSet = PinSet { rows: Vec::new() };
    /// Bumped whenever the answer changes. Home's [`super::pump`] watches it and refetches, so a
    /// pin toggled in the Library toolbar reaches the shelves without the UI having to know that
    /// Home is even a thing — and several toggles in one visit coalesce into one fetch.
    static PIN_GEN: AtomicU32 = AtomicU32::new(0);

    fn pins() -> &'static PinSet {
        unsafe { &*addr_of!(PINS) }
    }

    /// Install the granted roster (call on every roster landing — boot, a refresh, "Check for new
    /// shares"). Re-resolves against the persisted decisions; does NOT write anything, because
    /// resolving is not answering: writing here would spend the first-run question the moment a
    /// roster arrived.
    pub(crate) fn install_roster(roster: &[Granted]) {
        let resolved = resolve(roster, &session::peek().pins);
        let (n, on) = (resolved.rows.len(), resolved.pinned_count());
        // Only an EXCLUSION is observable on Home (an unknown key passes), so a roster landing
        // that changes none of them owes no refetch. That is the ordinary boot: the roster arrives
        // moments after the blocking hub fetch, and without this every single boot would refetch
        // both hub endpoints and drop a populated Home back to Loading to arrive at the same
        // shelves.
        let same = resolved.exclusions() == pins().exclusions();
        unsafe { *addr_of_mut!(PINS) = resolved };
        if !same {
            PIN_GEN.fetch_add(1, Ordering::SeqCst);
        }
        crate::log(&format!("pins: {on} of {n} libraries feed Home"));
    }

    /// The resolved set, as owned data — **capture this at the spawn site**, never inside a
    /// worker (`static mut` is main-thread-only here, like every other static in this module).
    pub(crate) fn snapshot() -> PinSet {
        pins().clone()
    }

    /// Does this library feed Home? (main-thread convenience over [`snapshot`])
    pub(crate) fn feeds_home(machine_id: &str, section: i64) -> bool {
        pins().feeds_home(machine_id, section)
    }

    /// This library is the last pinned one — the list draws its value dimmed and OK does nothing.
    pub(crate) fn is_last_pinned(key: &LibKey) -> bool {
        pins().is_last_pinned(key)
    }

    // There is deliberately NO borrowing accessor for the whole table. One would hand out a
    // `&'static` into `PINS`, and the obvious use of it — walk the rows, press OK on one — would
    // then hold a shared borrow of the very rows `set` takes `&mut` to, with `install_roster` free
    // to drop the Vec underneath it. The Sources list walks a `snapshot()` instead: it is a dozen
    // rows built once per panel, and it cannot be invalidated by the press it exists to make.

    /// The pin generation (see [`PIN_GEN`]).
    pub(crate) fn gen() -> u32 {
        PIN_GEN.load(Ordering::SeqCst)
    }

    /// Pin or unpin one library, persist the answer, and tell Home. The last-pinned refusal is
    /// returned rather than swallowed — see [`Set`].
    ///
    /// A failed WRITE is not in the return value, and that is a decision rather than an oversight:
    /// there is no such row in the design and no words for one on screen. It is logged instead, so
    /// the fact survives where it can be read — a support log — rather than becoming a fourth case
    /// every call site has to pretend to render. What an unsaved pin does NOT do is hold forever:
    /// [`install_roster`] re-resolves from the file, so the next roster landing — a refresh,
    /// "Check for new shares" — puts the library back the way disk says.
    pub(crate) fn set(key: &LibKey, on: bool) -> Set {
        let r = apply(unsafe { &mut (*addr_of_mut!(PINS)).rows }, key, on);
        if r == Set::Applied {
            record();
            PIN_GEN.fetch_add(1, Ordering::SeqCst);
            crate::ui::idle::invalidate(); // the row's value flips under a settled panel
        }
        r
    }

    /// Write the current answers down — including the ones that are still defaults. The first-run
    /// screen calls this for BOTH of its exits (`Start watching` and the skip): the defaults it
    /// showed were an answer, and recording them is what stops that screen coming back. A share
    /// that arrives afterwards is unanswered on its own and lands unpinned, in the Sources list,
    /// where "Check for new shares" already lives.
    ///
    /// **False means nothing was written** — no roster has resolved (so there is nothing to
    /// answer), or the session file is unreadable. It is returned rather than swallowed because
    /// the caller's whole reason for calling is that this screen must not come back, and a silent
    /// no-op here is a first-run question asked again on every boot. A route that shows a LIST of
    /// libraries can only be up when [`PinSet::is_known`], so in practice this is a tripwire.
    pub(crate) fn acknowledge() -> bool {
        record()
    }

    /// Has anyone ever answered? (Empty means unknown — see [`session::Session::pins`].) The
    /// first-run route's gate; a boot-path read, not a per-frame one — it opens the session file.
    pub(crate) fn asked() -> bool {
        !session::peek().pins.is_empty()
    }

    /// Persist the resolved table into the session, keeping decisions about libraries outside the
    /// current roster (a server that is merely not in this roster read — a stale grant, or a
    /// roster that has not landed yet — must not lose the answer the user gave it).
    fn record() -> bool {
        let rows = pins().rows();
        if rows.is_empty() {
            crate::log("pins: nothing resolved to record (no roster yet)");
            return false;
        }
        // A row whose server has no `machineIdentifier` is UNKEYED, and the reader drops exactly
        // those (`session::de_pins`) — a half-blank key names no library, and two servers with
        // blank ids would be the same one. Writing them would round-trip to nothing: the pin would
        // read as saved, then be gone at the next boot, taking the first-run question's answer with
        // it. So they are skipped here, loudly, rather than written into a hole.
        let (keyed, unkeyed): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| !r.key.machine_id.is_empty());
        if !unkeyed.is_empty() {
            crate::log(&format!(
                "pins: {} librar{} on a server with no machineIdentifier — held for this run, not saved",
                unkeyed.len(),
                if unkeyed.len() == 1 { "y" } else { "ies" }
            ));
        }
        if keyed.is_empty() {
            return false;
        }
        let mut s = session::peek();
        s.pins.retain(|p| !keyed.iter().any(|r| r.key.machine_id == p.machine_id && r.key.section == p.section));
        s.pins.extend(keyed.iter().map(|r| LibraryPin {
            machine_id: r.key.machine_id.clone(),
            section: r.key.section,
            pinned: r.on,
        }));
        // …through the field's one writer, which also reports whether anything reached disk. A
        // `save` here would be the stale-snapshot bug this model spent a finding learning about.
        session::set_pins(s.pins)
    }

    /// Drop the roster and the resolved set — the identity-change wipe, driven by
    /// [`super::reset`]. The next account's pins come from ITS session file, and until its roster
    /// lands the gate is back to passing everything.
    pub(crate) fn reset() {
        unsafe { *addr_of_mut!(PINS) = PinSet::default() };
        PIN_GEN.fetch_add(1, Ordering::SeqCst);
    }

    /// Test hook: install a resolved set directly, without a session file or a roster fetch.
    #[cfg(test)]
    pub(crate) fn seed_for_test(rows: Vec<Row>) {
        unsafe { *addr_of_mut!(PINS) = PinSet { rows } };
    }
}

// ---- home hubs: each hub is a titled slice of the catalog ----
struct HubRow {
    title: String,
    hub_id: String, // locale-independent hubIdentifier ("home.continue", "home.movies.recent", …)
    /// Which SERVER this shelf's items came from, as the owner's handle ("bamx23") — empty
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
static mut HERO_POOL: Vec<usize> = Vec::new();

/// number of items in the rotating hero pool
pub(crate) fn hero_pool_len() -> usize {
    unsafe { std::ptr::addr_of!(HERO_POOL).as_ref().map(|v| v.len()).unwrap_or(0) }
}
/// hero-pool item `i`, or None
pub(crate) fn hero_pool_item(i: usize) -> Option<&'static PmsMovie> {
    let idx = unsafe { std::ptr::addr_of!(HERO_POOL).as_ref() }.and_then(|v| v.get(i)).copied()?;
    movie(idx)
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
/// Handle of the server hub `i` came from ("bamx23"), or **empty** for the signed-in user's own
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

/// Refetch the home hubs and reconcile the surfaces that index into the rebuilt catalog: an open
/// detail page re-resolves its selected row (home's focus self-clamps at its read accessors).
/// The ONE post-mutation refresh ritual — player exit and the watched toggle both call this.
pub(crate) fn refetch_hubs_reconcile() -> c_int {
    let n = pms_fetch_hubs();
    crate::ui::detail::reselect();
    n
}

/// Fetch the home hubs (Continue Watching, On Deck, Recently Added, collections) into
/// the catalog + the HUBS grouping — BLOCKING, and the boot/install path's fetch. Skips
/// music/photo/playlist hubs + empty ones. Builds the new catalog/hubs/pool locally, then
/// commits all three statics at once ([`commit`]).
///
/// A failure no longer commits empty. It used to — "blanking the home consistently" — which
/// made a dead server indistinguishable from a server with nothing on it, and left Home a bare
/// dark screen with no explanation and nothing that would ever try again. Now it lands in
/// [`fail`]: the previous (consistent) three statics stay exactly as they were, the state
/// machine goes [`HubState::Failed`], and [`pump`] retries on a backoff. The consistency the
/// old comment was defending is intact — all three statics still only ever move together.
pub(crate) fn pms_fetch_hubs() -> c_int {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // supersede any retry already in flight
    dev_roster();
    // Read on the MAIN THREAD and passed in, exactly as `kick_refetch` does it for the worker —
    // one rule for both callers, so the off-thread path can never be the only one that is safe.
    let src = source_id();
    let pins = pin::snapshot();
    unsafe { std::ptr::addr_of_mut!(PIN_APPLIED).write(pin::gen()) };
    match catch_unwind(|| fetch_build(&src, &pins)) {
        Ok(Some(build)) => commit(build),
        _ => {
            fail();
            catalog().len() as c_int
        }
    }
}

/// dev: **`/tmp/plxnative-shared=<csv of section keys>`** — the pin model's only on-device
/// handle until the Sources list lands.
///
/// It installs a granted roster made of THIS server's own sections, marking the listed ones as
/// though a friend had shared them — so they arrive **unpinned by default** and their shelves
/// leave Home, while the tab strip, the browse grid, its sort menu and its A–Z rail (all of which
/// read the grant, never the pin) stay exactly as they were. That pair of boots is the whole
/// visible claim of this unit, and there is no other way to photograph it before the roster
/// transport and the toolbar chip exist. An empty value (`touch` the file) installs the roster
/// with everything owned, which is the "nothing is excluded" control.
///
/// **List all but one.** Listing EVERY section is the rescue case, not the demo: nothing would
/// resolve pinned, so `resolve` un-empties Home and the screen is unchanged — correct behaviour
/// that looks exactly like a broken feature. The boot log settles it either way; it prints
/// `pins: <on> of <n> libraries feed Home` right after the roster line.
///
/// Costs one small `GET /library/sections` per hub fetch while it is armed, and nothing at all
/// otherwise: `dev::read` is a compile-time `None` in a `RELEASE=1` build, so the fetch below is
/// unreachable and folds away with it. Re-runs until a roster is installed, so it survives a
/// profile switch (which resets the pins) without a latch of its own.
fn dev_roster() {
    if pin::snapshot().is_known() {
        return;
    }
    let Some(spec) = crate::dev::read("shared") else { return };
    let shared: Vec<i64> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let mid = source_id();
    let Some(mc) = crate::plex::client_opt().and_then(|c| c.sections()) else { return };
    let roster: Vec<pin::Granted> = mc
        .directory
        .iter()
        .filter_map(|d| d.key.trim().parse::<i64>().ok())
        .map(|k| pin::Granted { key: pin::LibKey::new(&mid, k), owned: !shared.contains(&k) })
        .collect();
    crate::log(&format!("pins: dev roster — {} libraries, {:?} treated as shared", roster.len(), shared));
    pin::install_roster(&roster);
}

/// The catalog/hubs/pool triple a fetch produces, before it is committed. Owned data only, so
/// the off-thread refetch can build it on a worker and hand it over through a mailbox.
type HubBuild = (Vec<PmsMovie>, Vec<HubRow>, Vec<usize>);

/// GET /hubs and project it — the whole fetch, minus the commit. `None` = the request failed
/// (transport, HTTP, or parse), which is NOT the same as a server with no hubs (`Some` with an
/// empty build). Shared verbatim by the blocking boot fetch and the off-thread retry.
///
/// `pins` is the pin gate, carried IN rather than read here: this runs on a worker for every
/// retry, and the resolved set is main-thread state.
fn fetch_build(src: &str, pins: &pin::PinSet) -> Option<HubBuild> {
    let c = crate::plex::client_opt()?;
    let mc = c.home_hubs(12)?;
    // The Continue Watching shelf comes from the DEDICATED hub, not from `/hubs`'s
    // `home.continue` + `home.ondeck` pair — see `build_hubs`. Its failure fails the whole fetch
    // (`?`), which is the module's existing contract: nothing commits, the three statics keep their
    // last consistent values, and `pump` retries on a backoff. Losing the most important shelf to a
    // transient error would be worse than briefly showing the previous one.
    let cw = c.continue_watching(12)?;
    Some(build_hubs(&mc, &cw, src, pins))
}

/// The `machineIdentifier` these hubs' items should be keyed on — the other half of a
/// [`pin::LibKey`], since a section key is only unique within one server. **Main thread only**
/// (it reads the session file), so the fetch path captures it at the spawn site like the pin set.
///
/// Two sources, in this order, and the second is not a nicety. The registry keys on the id and
/// hands each server's `Client` its own — but the legacy `plex::install(host, port, token)`, which
/// is still how EVERY boot on this build reaches its server, registers with an empty one
/// (`plex/servers.rs`: the caller has a stored address and no id). An empty id matches no row, so
/// the whole gate would quietly pass everything while looking like it worked. The stored session
/// has the real id — `auth` writes `ServerRef::machine_id` from the same `clientIdentifier` the
/// roster is keyed by — so it is the fallback until a registration supplies one.
fn source_id() -> String {
    let live = crate::plex::client_opt().map(|c| c.machine_id().to_string()).unwrap_or_default();
    if !live.is_empty() {
        return live;
    }
    unsafe {
        if (*std::ptr::addr_of!(SOURCE)).is_none() {
            *std::ptr::addr_of_mut!(SOURCE) = Some(crate::plex::session::peek().server.machine_id);
        }
        (*std::ptr::addr_of!(SOURCE)).clone().unwrap_or_default()
    }
}
/// Cache for [`source_id`]'s session fallback — cleared by [`reset`], i.e. exactly when
/// `install_pms` can have changed which server we are on.
static mut SOURCE: Option<String> = None;

/// Project a /hubs response into the catalog/hubs/pool triple. Pure — no statics, no I/O.
///
/// **This is the one place Home consults the pin** (`pins.feeds_home`), and it consults it per
/// ITEM rather than per shelf, because the merged Continue Watching row draws from every source
/// at once: a borrowed film joins it only once its library is pinned, and a shelf left empty by
/// the filter is dropped like any other empty one. The hero pool is built from the shelves below,
/// so it inherits the same gate for free — a borrowed hero is always the consequence of a pin.
fn build_hubs(
    mc: &crate::plex::MediaContainer,
    cw: &crate::plex::MediaContainer,
    src: &str,
    pins: &pin::PinSet,
) -> HubBuild {
    let mut new_cat: Vec<PmsMovie> = Vec::new();
    let mut new_hubs: Vec<HubRow> = Vec::new();
    let mut new_pool: Vec<usize> = Vec::new();
    const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];

    // Build the display shelves as ordered lists of item refs.
    //
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
    let mut shelves: Vec<(String, String, Vec<&crate::plex::Metadata>)> = Vec::new(); // (title, hubIdentifier, items)
    let mut cw_idx: Option<usize> = None; // shelves slot of Continue Watching
    for hub in cw.hub.iter() {
        let items: Vec<&crate::plex::Metadata> = hub
            .metadata
            .iter()
            .filter(|m| !SKIP.contains(&m.kind.as_str()) && pins.feeds_home(src, m.library_section_id))
            .collect();
        if items.is_empty() {
            continue;
        }
        // keep the hub id the rest of the module already matches on (hero-pool eligibility,
        // `hub_plays_directly`), rather than the dedicated hub's own "continueWatching"
        shelves.push(("Continue Watching".to_string(), "home.continue".to_string(), items));
        cw_idx = Some(shelves.len() - 1);
        break;
    }
    for hub in &mc.hub {
        if SKIP.contains(&hub.kind.as_str()) || hub.metadata.is_empty() {
            continue;
        }
        if hub.hub_identifier == "home.continue" || hub.hub_identifier == "home.ondeck" {
            continue; // superseded by the dedicated hub above
        }
        let items: Vec<&crate::plex::Metadata> =
            hub.metadata.iter().filter(|m| pins.feeds_home(src, m.library_section_id)).collect();
        if items.is_empty() {
            continue; // every item belonged to an unpinned library — the shelf is not Home's
        }
        shelves.push((hub.title.clone(), hub.hub_identifier.clone(), items));
    }
    if let Some(si) = cw_idx {
        // stable sort: most recently played first; equal timestamps keep hub order.
        shelves[si].2.sort_by(|a, b| b.last_viewed_at.cmp(&a.last_viewed_at));
    }

    for (title, hub_id, items) in &shelves {
        let start = new_cat.len();
        for item in items {
            if new_cat.len() >= PMS_MAX_MOVIES {
                break;
            }
            let m = parse_item(item);
            if !m.title.is_empty() && !m.thumb.is_empty() {
                new_cat.push(m); // need a poster to show it in a shelf
            }
        }
        if new_cat.len() > start {
            new_hubs.push(HubRow {
                title: title.clone(),
                hub_id: hub_id.clone(),
                // one server today: `/hubs` is asked of the machine we are signed in to, so every
                // row it returns is that machine's. A shared source names itself only once the
                // multi-server layer builds rows from more than one client.
                source: String::new(),
                start,
                len: new_cat.len() - start,
            });
        }
    }

    // Build the rotating hero pool: Continue Watching items first, then Recently Added,
    // deduped by ratingKey. Require landscape `art` (the hero draws a full-bleed backdrop)
    // and skip seasons (a bare "Season 1" makes a poor billboard). Capped at HERO_MAX.
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
            if new_pool.iter().any(|&j| new_cat[j].rk == m.rk) {
                continue; // dedup by ratingKey
            }
            new_pool.push(idx);
        }
    }
    (new_cat, new_hubs, new_pool)
}

// ---- fetch state machine: loading / ready / failed + the automatic-retry backoff -------------
//
// Modelled on `browse.rs`'s page store, deliberately, because it already learned both lessons
// this needed: a FAILED fetch must never overwrite a populated store (one wifi hiccup used to
// blank a whole grid permanently), and a fast-failing network must be held off by a countdown
// rather than re-spawning a worker every frame.

/// What the last hub fetch produced. Home's loading / empty / error read-out is a projection of
/// this: an empty catalog is only an empty *screen* when the fetch actually succeeded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HubState {
    /// a fetch is in flight, or the first one hasn't run yet — nothing to show is not an answer
    Loading,
    /// the server answered; the catalog is whatever it says it is (possibly legitimately empty)
    Ready,
    /// the fetch failed — the previous catalog (if any) is untouched and [`pump`] is counting
    /// down to the next automatic attempt
    Failed,
}
static mut HUB_STATE: HubState = HubState::Loading;

/// Off-thread refetch landing, tagged with the generation it was spawned in. `Built` commits;
/// `Failed` deliberately carries no data, so a failure cannot be mistaken for "the server
/// returned nothing".
enum HubLanding {
    Built(u32, HubBuild),
    Failed(u32),
}
static HUB_RESULT: Mutex<Option<HubLanding>> = Mutex::new(None);
/// Bumped by every authoritative fetch ([`reset`] on an identity change, and the blocking
/// install fetch): a worker spawned before it lands with a stale tag and is DROPPED. Without
/// this, a retry in flight across a profile switch could commit the previous account's hubs on
/// top of the new one's — the same late-landing hazard `browse.rs` keys its `GEN` on.
static HUB_GEN: AtomicU32 = AtomicU32::new(0);
/// Single flight, cleared ONLY where the mailbox is taken (or where the spawn is refused) — the
/// same latch discipline as `browse::IN_FLIGHT`: drop one without the other and the hubs never
/// fetch again for the rest of the session.
static HUBS_FETCHING: AtomicBool = AtomicBool::new(false);

/// The [`pin`] generation the committed shelves were built against. [`pump`] refetches when it
/// falls behind, which is how a pin toggled in the Library toolbar reaches Home: the UI changes
/// one bit and says nothing to this module, several toggles in one visit coalesce into one fetch,
/// and no keypress ever blocks the loop on a network round trip. Main-thread only, like the rest.
static mut PIN_APPLIED: u32 = 0;

/// Seconds until the next automatic attempt, and the consecutive-failure count that sizes it.
/// Main-thread only (stepped by [`pump`], which the Home update drives).
static mut RETRY_S: f32 = 0.0;
static mut RETRY_N: u32 = 0;
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

/// The hub-fetch state — Home's loading/empty/error projection reads it.
pub(crate) fn hub_state() -> HubState {
    unsafe { std::ptr::addr_of!(HUB_STATE).read() }
}

/// Install a finished build: all three statics move together (they always have — a half-applied
/// catalog once left a stale hero pool floating over emptied shelves), and a success retires the
/// backoff so the next failure starts at the bottom of the ladder again.
fn commit(build: HubBuild) -> c_int {
    let (new_cat, new_hubs, new_pool) = build;
    let n = new_cat.len();
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = new_cat;
        *std::ptr::addr_of_mut!(HUBS) = new_hubs;
        *std::ptr::addr_of_mut!(HERO_POOL) = new_pool;
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Ready);
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    n as c_int
}

/// Record a failed fetch: keep whatever catalog is already committed and arm the next attempt.
fn fail() {
    unsafe {
        let n = std::ptr::addr_of!(RETRY_N).read().saturating_add(1);
        std::ptr::addr_of_mut!(RETRY_N).write(n);
        std::ptr::addr_of_mut!(RETRY_S).write(backoff_secs(n));
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Failed);
        // the ONE line that says a dead Home is dead ON PURPOSE and is coming back — without it
        // the whole recovery is invisible in the event log
        crate::log(&format!("hubs: fetch FAILED (attempt {n}) — retrying in {:.0}s", backoff_secs(n)));
    }
}

/// Step the automatic-retry countdown by `dt` seconds; true when the next attempt is due. Split
/// out from [`pump`] so the ladder is testable without spawning a worker or touching a socket.
fn retry_due(dt: f32) -> bool {
    unsafe {
        let left = std::ptr::addr_of!(RETRY_S).read() - dt;
        std::ptr::addr_of_mut!(RETRY_S).write(left.max(0.0));
        left <= 0.0
    }
}

/// Spawn an off-thread hub refetch (single flight); [`pump`] lands it. The boot/install fetch
/// stays blocking, but every RETRY goes through here — and that is exactly what lets Home show a
/// live loading state at all: a blocking fetch on the SDL loop draws no frames while it runs, so
/// the spinner it is supposed to be spinning would never reach the panel.
fn kick_refetch() {
    if HUBS_FETCHING.swap(true, Ordering::SeqCst) {
        return; // one in flight already — its spinner is the honest answer
    }
    unsafe {
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Loading);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    let gen = HUB_GEN.load(Ordering::SeqCst);
    // CAPTURE AT THE SPAWN SITE: the resolved pin set and the source id are main-thread state
    // (the latter reads the session file), so both are read here and MOVED into the worker.
    let src = source_id();
    let pins = pin::snapshot();
    let pin_gen = pin::gen();
    let spawned = crate::task::spawn_small("hubs", move || {
        let built = catch_unwind(|| fetch_build(&src, &pins)).ok().flatten();
        // filled OUTSIDE the guard so a panicking fetch still lands (as a failure) rather than
        // latching the single flight forever
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(built.map(|b| HubLanding::Built(gen, b)).unwrap_or(HubLanding::Failed(gen)));
    });
    if !spawned {
        // nothing will ever fill the mailbox (the thread limit refused us), so release the latch
        // here and back off — `pump` will try again on the ladder.
        HUBS_FETCHING.store(false, Ordering::SeqCst);
        fail();
    } else {
        // Only now: `PIN_APPLIED` claims the committed shelves were built against these pins, and
        // a refused spawn builds nothing. Setting it before the spawn would retire the pin-change
        // trigger for a fetch that never ran, leaving an unpinned library's shelves on Home until
        // some unrelated retry happened to come round.
        unsafe { std::ptr::addr_of_mut!(PIN_APPLIED).write(pin_gen) };
        crate::log("hubs: retrying (off-thread)");
    }
}

/// The Retry control's kick: try again NOW, from the bottom of the ladder — a person who asks
/// for it should never be made to sit out a 30-second automatic wait. A no-op while a fetch is
/// already in flight.
pub(crate) fn request_retry() {
    unsafe {
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
    }
    kick_refetch();
}

/// Once-a-frame main-thread tick: land a finished refetch, then count down to the next automatic
/// attempt. Driven by `ui::home::home_update`, so it runs exactly while Home is the screen that
/// cares (and never spawns a background fetch behind the player).
pub(crate) fn pump(dt: f32) {
    // taken into a `let` FIRST: an `if let` scrutinee holds its temporary guard for the whole
    // body under edition 2021, which would run the commit + `detail::reselect()` with the mailbox
    // still locked (and changes meaning again under 2024)
    let landed = HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(landing) = landed {
        // A refetch landing rewrites the shelves under a Home screen that may have gone idle
        // (the failure state repaints too — a retry that fails changes the status caption).
        crate::ui::idle::invalidate();
        HUBS_FETCHING.store(false, Ordering::SeqCst);
        let cur = HUB_GEN.load(Ordering::SeqCst);
        match landing {
            // a landing from before the last authoritative fetch describes a server (or an
            // account) we have since moved off — drop it, neither commit nor blame
            HubLanding::Built(gen, _) | HubLanding::Failed(gen) if gen != cur => {}
            HubLanding::Built(_, build) => {
                let n = commit(build);
                crate::log(&format!("hubs: retry landed — {n} items, {} shelves", hub_count()));
                // the same post-mutation ritual `refetch_hubs_reconcile` performs: an open
                // detail page re-resolves its selected row against the rebuilt catalog
                crate::ui::detail::reselect();
            }
            HubLanding::Failed(_) => fail(),
        }
    }
    // A pin moved since these shelves were built, so they describe a Home the user has since
    // changed. Refetch rather than filter what is committed: an unpinned library's items are gone
    // from the catalog the shelves index into, and a newly pinned one was never fetched at all.
    //
    // Only from Ready, which is what keeps this off the ladder's toes: any other state already
    // owes a fetch, and that fetch takes a fresh snapshot anyway. Without the guard, a refused
    // spawn (`fail()` → Failed, PIN_APPLIED left behind) would come straight back here on the next
    // frame and try again — a worker spawn attempt per frame, the exact thing the backoff exists
    // to prevent.
    let pin_stale = unsafe { std::ptr::addr_of!(PIN_APPLIED).read() } != pin::gen();
    if pin_stale && hub_state() == HubState::Ready && !HUBS_FETCHING.load(Ordering::SeqCst) {
        kick_refetch(); // sets PIN_APPLIED once the worker is actually running
        return;
    }
    // Anything but Ready with nothing in flight is a state only a fetch can leave: Failed (with a
    // backoff owed) or a Loading whose worker landed stale and was dropped — the latter owes
    // nothing, so it re-kicks on the spot rather than wedging Home on a spinner forever.
    if hub_state() != HubState::Ready && !HUBS_FETCHING.load(Ordering::SeqCst) && retry_due(dt) {
        kick_refetch();
    }
}

/// A committable build of `n` placeholder rows in one shelf (test fixture). Only the SHAPE is
/// real — `build_hubs` would have dropped these rows for having no title/poster; what the tests
/// here assert is the commit/landing bookkeeping, which never looks inside a row.
#[cfg(test)]
fn build_test(n: usize) -> HubBuild {
    let cat: Vec<PmsMovie> = (0..n).map(|_| PmsMovie::default()).collect();
    let hubs =
        vec![HubRow { title: "Continue Watching".into(), hub_id: "home.continue".into(), source: String::new(), start: 0, len: n }];
    (cat, hubs, Vec::new())
}

/// Test hook: put the store in a known place — `items` rows in one shelf, plus a fetch state.
/// Home's read-out is a pure projection of this pair, and the states a host test cannot reach for
/// real (a live server answering, or refusing) are exactly the ones worth pinning.
#[cfg(test)]
pub(crate) fn seed_for_test(items: usize, state: HubState) {
    reset();
    if items > 0 {
        commit(build_test(items));
    }
    unsafe { std::ptr::addr_of_mut!(HUB_STATE).write(state) };
}

/// Drop the catalog and re-arm the fetch — the identity-change twin of [`crate::browse::reset`],
/// called from the same place (`install_pms`). Now that a failed fetch KEEPS the previous
/// catalog, a profile switch whose fetch fails would otherwise leave the previous user's shelves
/// on screen; this is the one place that must still wipe them.
pub(crate) fn reset() {
    HUB_GEN.fetch_add(1, Ordering::SeqCst); // a worker still running belongs to the old identity
    *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    HUBS_FETCHING.store(false, Ordering::SeqCst);
    // The pins belong to the account too: the next one's answers live in ITS session file, and
    // its roster has not landed yet. Both statics move with the catalog, and `PIN_APPLIED` is
    // squared with the (now bumped) generation so the boot fetch below isn't immediately redone.
    pin::reset();
    unsafe {
        *std::ptr::addr_of_mut!(CATALOG) = Vec::new();
        *std::ptr::addr_of_mut!(HUBS) = Vec::new();
        *std::ptr::addr_of_mut!(HERO_POOL) = Vec::new();
        *std::ptr::addr_of_mut!(SOURCE) = None; // the new identity's server may not be this one
        std::ptr::addr_of_mut!(HUB_STATE).write(HubState::Loading);
        std::ptr::addr_of_mut!(RETRY_N).write(0);
        std::ptr::addr_of_mut!(RETRY_S).write(0.0);
        std::ptr::addr_of_mut!(PIN_APPLIED).write(pin::gen());
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // NB every test that touches the catalog statics holds the crate-wide serial lock: they are
    // read from other modules' tests too (`ui::home` walks `hub_len`), which a module-local mutex
    // cannot see. `reset()` doubles as the teardown.

    /// A landing tagged with the CURRENT generation — what a worker spawned right now would post.
    fn land(l: impl FnOnce(u32) -> HubLanding) {
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(l(HUB_GEN.load(Ordering::SeqCst)));
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

    /// The bug this whole unit exists for: a failed fetch used to commit an EMPTY catalog, so one
    /// unreachable moment blanked a populated Home for good. A failure must leave every one of
    /// the three statics exactly as it found them.
    #[test]
    fn a_failed_landing_never_blanks_a_populated_home() {
        let _g = crate::testlock::serial();
        reset();
        commit(build_test(3));
        assert_eq!(hub_count(), 1);
        assert_eq!(hub_len(0), 3);

        land(HubLanding::Failed);
        pump(0.0);

        assert_eq!(hub_state(), HubState::Failed, "the failure must be distinguishable");
        assert_eq!(hub_count(), 1, "the shelves survive a failed refetch");
        assert_eq!(hub_len(0), 3);
        assert!(unsafe { std::ptr::addr_of!(RETRY_S).read() } > 0.0, "and the next attempt is armed");
        reset();
    }

    /// A landing that carries a build commits it, and a success retires the backoff so the next
    /// failure starts at the bottom of the ladder instead of inheriting a 30s wait.
    #[test]
    fn a_successful_landing_commits_and_retires_the_backoff() {
        let _g = crate::testlock::serial();
        reset();
        fail();
        fail();
        assert_eq!(hub_state(), HubState::Failed);

        land(|g| HubLanding::Built(g, build_test(2)));
        pump(0.0);

        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_len(0), 2);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_N).read() }, 0);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_S).read() }, 0.0);
        reset();
    }

    /// An answer of "nothing" is an ANSWER: it must land as Ready (Home's empty state), not as a
    /// failure, or the screen would apologise for a server that is simply empty — and retry it
    /// forever.
    #[test]
    fn a_server_with_no_hubs_is_ready_and_empty_not_failed() {
        let _g = crate::testlock::serial();
        reset();
        land(|g| HubLanding::Built(g, (Vec::new(), Vec::new(), Vec::new())));
        pump(0.0);
        assert_eq!(hub_state(), HubState::Ready);
        assert_eq!(hub_count(), 0);
        reset();
    }

    /// The countdown is real time (seconds of `dt`), not frames like `browse.rs`'s — a device
    /// that drops to 30fps must still retry on the same wall clock. NB `retry_due` keeps
    /// reporting due once it is spent; what makes an attempt happen only once is `kick_refetch`
    /// re-latching the single flight, not this.
    #[test]
    fn the_retry_countdown_fires_when_the_backoff_elapses() {
        let _g = crate::testlock::serial();
        reset();
        fail(); // arms RETRY_MIN_S
        for _ in 0..3 {
            assert!(!retry_due(0.5), "0.5s at a time must not fire before the 2s wait is spent");
        }
        assert!(retry_due(0.5), "the fourth half-second spends it");
        reset();
    }

    /// A retry still in flight when the account changes must not commit the PREVIOUS identity's
    /// hubs over the new one's: its landing carries the old generation and is dropped whole —
    /// neither committed nor blamed on the current fetch.
    #[test]
    fn a_landing_from_before_a_reset_is_dropped_whole() {
        let _g = crate::testlock::serial();
        reset();
        let stale = HUB_GEN.load(Ordering::SeqCst);
        reset(); // the identity change
        commit(build_test(2)); // …and the new identity's catalog
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(HubLanding::Built(stale, build_test(9)));
        pump(0.0);
        assert_eq!(hub_len(0), 2, "the stale build must not replace the current catalog");
        assert_eq!(hub_state(), HubState::Ready, "nor be counted as a failure of the current one");
        reset();
    }

    // ---- pinning: which libraries reach Home ------------------------------------------------
    //
    // All pure: `resolve` and `apply` are the whole model, and `build_hubs` is a projection. None
    // of these touch the session file or a socket.

    use pin::{Granted, LibKey, Row};

    fn own(section: i64) -> Granted {
        Granted { key: LibKey::new("aaaabbbb1111", section), owned: true }
    }
    fn theirs(section: i64) -> Granted {
        Granted { key: LibKey::new("ccccdddd2222", section), owned: false }
    }
    fn recorded(machine: &str, section: i64, pinned: bool) -> crate::plex::session::LibraryPin {
        crate::plex::session::LibraryPin { machine_id: machine.into(), section, pinned }
    }
    fn row(machine: &str, section: i64, on: bool) -> Row {
        Row { key: LibKey::new(machine, section), owned: machine == "aaaabbbb1111", on }
    }

    /// The defaults, on a roster nobody has answered for: your own libraries feed Home, a
    /// friend's do not. The point of asking is not to put a stranger's shelves on your front door
    /// unannounced — and asking is a screen, not a silent default.
    #[test]
    fn a_fresh_roster_pins_your_own_libraries_and_not_a_friends() {
        let roster = [own(1), own(2), theirs(1)];
        let p = pin::resolve(&roster, &[]);
        assert!(p.feeds_home("aaaabbbb1111", 1));
        assert!(p.feeds_home("aaaabbbb1111", 2));
        assert!(!p.feeds_home("ccccdddd2222", 1), "a share must not reach Home unannounced");
        assert_eq!(p.pinned_count(), 2);
    }

    /// A section key is only unique WITHIN a server — both of these are library 1 — so the
    /// machine id is load-bearing in the key, not decoration.
    #[test]
    fn a_recorded_answer_beats_the_default_and_is_keyed_per_server() {
        let roster = [own(1), theirs(1)];
        let table = [recorded("aaaabbbb1111", 1, false), recorded("ccccdddd2222", 1, true)];
        let p = pin::resolve(&roster, &table);
        assert!(!p.feeds_home("aaaabbbb1111", 1), "unpinning your own server is legitimate");
        assert!(p.feeds_home("ccccdddd2222", 1), "and a friend's library reaches Home once pinned");
    }

    /// The one real failure this control can produce is an empty Home, so the last pinned library
    /// refuses to be unpinned — in the DATA LAYER, where no UI can route around it. It is not
    /// unavailable, though: pinning something else frees it immediately.
    #[test]
    fn the_last_pinned_library_cannot_be_unpinned() {
        let mut rows = vec![row("aaaabbbb1111", 1, true), row("ccccdddd2222", 1, false)];
        assert_eq!(pin::apply(&mut rows, &LibKey::new("aaaabbbb1111", 1), false), pin::Set::LastPinned);
        assert!(rows[0].on, "and the refusal must not have moved it half-way");

        assert_eq!(pin::apply(&mut rows, &LibKey::new("ccccdddd2222", 1), true), pin::Set::Applied);
        assert_eq!(
            pin::apply(&mut rows, &LibKey::new("aaaabbbb1111", 1), false),
            pin::Set::Applied,
            "with a second library pinned, the first is ordinary again"
        );
        assert_eq!(
            pin::apply(&mut rows, &LibKey::new("ccccdddd2222", 1), false),
            pin::Set::LastPinned,
            "…and now the friend's is the one holding Home up"
        );
    }

    /// The rule is about the LAST one, not about your own: a set holding one pinned library is
    /// what the list draws dimmed, whoever owns it.
    ///
    /// The unknown key is the trap. `feeds_home` answers "yes" for a library the roster does not
    /// name — right for an item reaching Home, wrong here: with one library pinned it would make
    /// EVERY unknown key read as "the one holding Home up", so the list would draw an unrelated
    /// row's value dimmed while `apply` cheerfully answered `NotGranted` for it.
    #[test]
    fn the_last_pinned_row_is_the_one_the_list_dims() {
        let roster = [own(1), theirs(1)];
        let p = pin::resolve(&roster, &[]);
        assert!(p.is_last_pinned(&LibKey::new("aaaabbbb1111", 1)));
        assert!(!p.is_last_pinned(&LibKey::new("ccccdddd2222", 1)), "an unpinned row is not 'the last pinned'");
        assert!(!p.is_last_pinned(&LibKey::new("eeeeffff3333", 3)), "and neither is a library nobody has heard of");
    }

    /// An account with no libraries of its own is not a mode — it draws the same Home as anyone
    /// else. The defaults alone would leave it empty, so a roster that resolves to nothing pinned
    /// is rescued whole. (The same rescue covers the only other way to reach an empty set: the
    /// grant behind your one pinned library disappearing.)
    #[test]
    fn a_borrowed_only_account_still_gets_a_home() {
        let roster = [theirs(1), theirs(2)];
        let p = pin::resolve(&roster, &[]);
        assert_eq!(p.pinned_count(), 2, "nothing pinned would be an empty Home");
    }

    /// …but the rescue is a fallback, not a floor: the moment ANY library resolves pinned, the
    /// rest keep the answer they were given.
    #[test]
    fn the_rescue_never_overrides_a_real_answer() {
        let roster = [theirs(1), theirs(2)];
        let p = pin::resolve(&roster, &[recorded("ccccdddd2222", 1, true), recorded("ccccdddd2222", 2, false)]);
        assert!(p.feeds_home("ccccdddd2222", 1));
        assert!(!p.feeds_home("ccccdddd2222", 2));
    }

    /// And when it does have to fire, WHICH libraries it pins is the whole point: never a share
    /// nobody has ruled on while a library of your own is standing there.
    ///
    /// The case that made this a rule: you unpin your own library while a friend's is pinned
    /// (legal — the last-pinned rule was satisfied), then that friend revokes the share. Pinning
    /// "everything left" would put two never-answered strangers on your front door — the exact
    /// thing the defaults exist to prevent — so your own library comes back instead, even though
    /// it is the one row that carries a recorded "off".
    #[test]
    fn the_rescue_reaches_for_your_own_library_before_a_strangers() {
        let roster = [own(1), theirs(2), theirs(3)];
        let p = pin::resolve(&roster, &[recorded("aaaabbbb1111", 1, false)]);
        assert!(p.feeds_home("aaaabbbb1111", 1), "your own library is the one that comes back");
        assert!(!p.feeds_home("ccccdddd2222", 2), "not a share nobody has ruled on");
        assert!(!p.feeds_home("ccccdddd2222", 3));

        // …and an unanswered library of your own is preferred to an answered one, so the rescue
        // takes back as little as it can.
        let p = pin::resolve(&[own(1), own(2)], &[recorded("aaaabbbb1111", 1, false)]);
        assert!(!p.feeds_home("aaaabbbb1111", 1), "the recorded 'off' survives while anything else can serve");
        assert!(p.feeds_home("aaaabbbb1111", 2));
    }

    /// When the rescue has to contradict an answer, it contradicts exactly ONE. Unpinning both of
    /// your own libraries is legal while a friend's is pinned; if that share is then revoked, one
    /// library must come back — not the pair, because the second one buys nothing that the first
    /// has not already bought, and the next press writes whatever this returns down as the user's
    /// own answer.
    ///
    /// The tier that takes only one is the tier that overrules something. The tier that takes all
    /// of them ("shares nobody has ruled on") contradicts nothing, and a borrowed-only account
    /// gets the whole Home an owner would.
    #[test]
    fn the_rescue_takes_back_one_answer_not_every_answer() {
        let both_off = [recorded("aaaabbbb1111", 1, false), recorded("aaaabbbb1111", 2, false)];
        let p = pin::resolve(&[own(1), own(2)], &both_off);
        assert_eq!(p.pinned_count(), 1, "one library un-empties Home; the second answer stands");
        assert!(p.feeds_home("aaaabbbb1111", 1), "and it is the first, not an arbitrary one");

        let borrowed = pin::resolve(&[theirs(1), theirs(2), theirs(3)], &[]);
        assert_eq!(borrowed.pinned_count(), 3, "nothing was answered, so nothing is being overruled");
    }

    /// **Empty means unknown, not none.** No roster installed (boot, or a build that never
    /// installs one) and a library the roster does not name both pass the gate — the alternative
    /// is a Home that blanks itself on a first boot after an upgrade, which is the failure this
    /// whole model is shaped to avoid.
    #[test]
    fn an_unresolved_roster_and_an_unknown_library_both_feed_home() {
        let unknown = pin::PinSet::default();
        assert!(!unknown.is_known());
        assert!(unknown.feeds_home("aaaabbbb1111", 1), "no roster yet is not 'nothing is pinned'");
        assert!(unknown.feeds_home("", 0), "…including an item that names no library at all");

        let p = pin::resolve(&[own(1)], &[]);
        assert!(p.feeds_home("some-other-server", 7), "a library nobody has ruled on reaches Home");
    }

    /// Continue Watching's merge is conditional on pinning: a borrowed item joins the merged
    /// shelf only once its library is pinned. Graded through `build_hubs`, because that is the
    /// ONE place Home consults the pin — and it filters per ITEM, since the merged row is the one
    /// shelf whose items come from several libraries at once.
    #[test]
    fn continue_watching_merges_only_the_libraries_that_are_pinned() {
        let cw: crate::plex::MediaContainer = serde_json::from_str(
            r#"{"Hub":[{"hubIdentifier":"continueWatching","title":"Continue Watching","Metadata":[
                 {"type":"movie","ratingKey":"1","title":"Ours","thumb":"/t1","librarySectionID":1},
                 {"type":"movie","ratingKey":"2","title":"Theirs","thumb":"/t2","librarySectionID":"4"}
               ]}]}"#,
        )
        .expect("fixture parses");
        let empty: crate::plex::MediaContainer = serde_json::from_str("{}").unwrap();
        assert_eq!(cw.hub[0].metadata[1].library_section_id, 4, "PMS string-encodes ids");

        // both pinned: the shelf merges, most-recently-played order
        let both = pin::resolve(&[own(1), own(4)], &[]);
        let (cat, hubs, _) = build_hubs(&empty, &cw, "aaaabbbb1111", &both);
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].len, 2);
        assert_eq!(cat.len(), 2);

        // section 4 unpinned: its item leaves the merged shelf, and nothing else moves
        let one = pin::resolve(&[own(1), own(4)], &[recorded("aaaabbbb1111", 4, false)]);
        let (cat, hubs, _) = build_hubs(&empty, &cw, "aaaabbbb1111", &one);
        assert_eq!(hubs[0].len, 1, "the merged shelf shortens rather than showing a dead card");
        assert_eq!(cat[0].title, "Ours");

        // the same item on a DIFFERENT server is a different library — an unpinned section 4 on
        // ours must not filter a friend's section 4
        let (_, hubs, _) = build_hubs(&empty, &cw, "ccccdddd2222", &one);
        assert_eq!(hubs[0].len, 2);
    }

    /// **The defining rule, and it is a negative one: Home consults the pin and nothing else does.**
    /// Tabs, grid, sort menu and A–Z rail all come from the GRANT, so unpinning a library must take
    /// it off Home while leaving it exactly as browsable as it was.
    ///
    /// Two halves, because the rule has two. The first is behavioural: one `/library/sections`
    /// answer and one hub answer, and the SAME section is gone from Home while still standing in
    /// the directory the section table is built from (`browse::ensure_sections` maps that array
    /// 1:1). The second is structural, and it is the half that will actually catch the regression:
    /// `browse.rs` — which owns the section table, the paged grid, the server-driven sort menu and
    /// the letter rail — must not so much as NAME the pin model. A source-text assertion is a blunt
    /// instrument, but the thing being defended is a design rule about which module may ask a
    /// question, and that is precisely what a call-site check can state and a behavioural test
    /// cannot. (`ui/library.rs` is deliberately NOT probed: the Sources list lives in that toolbar
    /// by design, and it is the one screen that must ask.)
    #[test]
    fn home_consults_the_pin_and_nothing_else_does() {
        let sections: crate::plex::MediaContainer = serde_json::from_str(
            r#"{"Directory":[{"key":"1","type":"movie","title":"Movies"},
                             {"key":"2","type":"show","title":"TV Shows"}]}"#,
        )
        .expect("fixture parses");
        let hubs_json: crate::plex::MediaContainer = serde_json::from_str(
            r#"{"Hub":[{"hubIdentifier":"home.movies.recent","title":"Recently Added Movies","type":"movie","Metadata":[
                 {"type":"movie","ratingKey":"9","title":"A Film","thumb":"/t","librarySectionID":1}]},
               {"hubIdentifier":"home.television.recent","title":"Recently Added TV","type":"show","Metadata":[
                 {"type":"show","ratingKey":"7","title":"A Show","thumb":"/t2","librarySectionID":2}]}]}"#,
        )
        .expect("fixture parses");
        let empty: crate::plex::MediaContainer = serde_json::from_str("{}").unwrap();

        let off = pin::resolve(&[own(1), own(2)], &[recorded("aaaabbbb1111", 1, false)]);
        let (cat, shelves, _) = build_hubs(&hubs_json, &empty, "aaaabbbb1111", &off);
        assert_eq!(shelves.len(), 1, "Home loses the unpinned library's shelf");
        assert_eq!(cat[0].title, "A Show");
        assert!(
            sections.directory.iter().any(|d| d.key == "1"),
            "…and the section table it is browsed from is untouched: the tab, the grid, its sort \
             menu and its A–Z rail all come from the grant"
        );

        const BROWSE: &str = include_str!("browse.rs");
        for probe in ["pms::pin", "feeds_home", "PinSet", "is_last_pinned"] {
            assert!(
                !BROWSE.contains(probe),
                "browse.rs names `{probe}` — the section table, the grid's paging, the sort menu \
                 and the letter rail must come from the GRANT. Pinning governs Home only."
            );
        }
    }

    /// What the model WRITES has to be what it reads back. The two halves live in different
    /// modules — `pin::record` builds the rows, `session::de_pins` parses them — and the hole they
    /// left between them was silent: a library whose server has no `machineIdentifier` was written
    /// happily and dropped on the way back in, so a pin read as saved and was gone by the next
    /// boot, taking the first-run question's answer with it. Written rows are keyed rows only.
    #[test]
    fn every_recorded_row_round_trips_through_the_session() {
        use crate::plex::session::{LibraryPin, Session};
        let resolved = [row("aaaabbbb1111", 1, true), row("aaaabbbb1111", 2, false), row("", 3, true)];
        let written: Vec<LibraryPin> = resolved
            .iter()
            .filter(|r| !r.key.machine_id.is_empty()) // `record`'s rule
            .map(|r| LibraryPin { machine_id: r.key.machine_id.clone(), section: r.key.section, pinned: r.on })
            .collect();
        let json = serde_json::to_string(&Session { pins: written, ..Session::default() }).unwrap();
        let back: Session = serde_json::from_str(&json).expect("a written session parses");
        assert_eq!(back.pins.len(), 2, "the unkeyed row is not written, because it cannot be read");
        assert_eq!(back.pin_of("aaaabbbb1111", 1), Some(true));
        assert_eq!(back.pin_of("aaaabbbb1111", 2), Some(false), "and a 'no' comes back a 'no'");
    }

    /// A friend can revoke a share between two boots, and the answer the user gave about it is
    /// still on file. The roster is what exists; the table is only what was said about it — so a
    /// row naming a server that is no longer granted is simply not in the resolved set, and
    /// nothing indexes one table by the other's length.
    #[test]
    fn a_pin_for_a_revoked_share_is_dropped_not_a_panic() {
        let table = [recorded("ccccdddd2222", 1, true), recorded("eeeeffff3333", 4, false), recorded("aaaabbbb1111", 1, true)];
        let p = pin::resolve(&[own(1)], &table);
        assert_eq!(p.rows().len(), 1, "the resolved set is the ROSTER, not the file");
        assert!(p.feeds_home("aaaabbbb1111", 1));
        assert!(p.is_last_pinned(&LibKey::new("aaaabbbb1111", 1)), "and it is now the last one standing");
        // the revoked rows are neither resolved nor reachable — asking about one is a question
        // about a library that does not exist, and the honest answer is the permissive one
        assert!(p.feeds_home("ccccdddd2222", 1));
        assert_eq!(pin::apply(&mut p.rows().to_vec(), &LibKey::new("eeeeffff3333", 4), true), pin::Set::NotGranted);
    }

    /// A shelf whose every item belonged to an unpinned library is not drawn empty — it is not
    /// drawn at all. (Absence on Home is the design's answer; a shelf that cannot be scrolled
    /// into is worse than a shelf that isn't there.) The hero pool is built from the shelves, so
    /// it empties with them — a borrowed hero is always the consequence of a pin.
    #[test]
    fn an_unpinned_librarys_shelf_is_absent_from_home_hero_included() {
        let hubs_json: crate::plex::MediaContainer = serde_json::from_str(
            r#"{"Hub":[{"hubIdentifier":"home.movies.recent","title":"Recently Added","type":"movie","Metadata":[
                 {"type":"movie","ratingKey":"9","title":"Theirs","thumb":"/t","art":"/a","librarySectionID":4}
               ]}]}"#,
        )
        .expect("fixture parses");
        let empty: crate::plex::MediaContainer = serde_json::from_str("{}").unwrap();

        let (_, shelves, pool) = build_hubs(&hubs_json, &empty, "aaaabbbb1111", &pin::resolve(&[own(4)], &[]));
        assert_eq!(shelves.len(), 1, "pinned: the shelf is there");
        assert_eq!(pool.len(), 1, "…and it can supply the rotating hero");

        let off = pin::resolve(&[own(4), own(1)], &[recorded("aaaabbbb1111", 4, false)]);
        let (cat, shelves, pool) = build_hubs(&hubs_json, &empty, "aaaabbbb1111", &off);
        assert!(shelves.is_empty(), "no heading, no row, no placeholder");
        assert!(cat.is_empty());
        assert!(pool.is_empty(), "and no hero the user cannot open");
    }

    /// `reset` is the profile-switch wipe: the previous user's shelves must not survive it, and
    /// the state machine must come back as a fresh boot's (Loading, no backoff owed).
    #[test]
    fn reset_wipes_the_catalog_and_re_arms_the_fetch() {
        let _g = crate::testlock::serial();
        commit(build_test(4));
        fail();
        reset();
        assert_eq!(hub_count(), 0);
        assert_eq!(catalog().len(), 0);
        assert_eq!(hero_pool_len(), 0);
        assert_eq!(hub_state(), HubState::Loading);
        assert_eq!(unsafe { std::ptr::addr_of!(RETRY_N).read() }, 0);
    }
}
