//! **Also available** — the same film held by more than one source, as a list you can walk into.
//!
//! Deliverable E of the Shared Sources design (`docs/shared-servers.md` §6): when a second pinned
//! source also holds the item on screen, the detail page's actions row grows an *Also available*
//! pill with a trailing chevron, and it opens this panel — the app's usual [`Popover`] +
//! [`TableView`], the same object the Library's Sort and Filter chips open one page over.
//!
//! Its rows use all FOUR of that cell's content places — the leading mark, the label over its
//! sub-line, the trailing read-out, and the right-aligned badge:
//!
//! | mark | label | sub-line | read-out | badge |
//! |---|---|---|---|---|
//! | ✓ | `Movies`    | `This account` | `1 hr 57 min` | `1080p` |
//! |   | `Film Club` | `friend`       | `1 hr 57 min` | `4K`    |
//!
//! # The four calls this module implements, and why each is not the obvious thing
//!
//! **A CONTROL in the actions row, not a section of the page.** An item on several servers is a
//! rare fact; a permanent band would spend a whole row on nothing on every other page in the
//! library. So the list is behind a button, and the button itself is drawn ONLY when a second
//! pinned source holds this item ([`is_available`]) — the same "with one source, none of this is
//! drawn" rule the shelf heading's annotation implements as absence rather than as a branch that
//! draws nothing visible.
//!
//! **The tick marks the copy you are on now**, and the badge is the RESOLUTION class — the
//! vocabulary the hero's badges row already speaks (`ui::fmt::resolution`, one spelling of "4K" in
//! the product). A row is `[library] [owner · runtime] [class]`: the label answers *which library*,
//! the sub-line *whose*, the badge *what you would get*.
//!
//! **OK NAVIGATES.** It opens that server's own page for the film; it does not swap the copy under
//! you. That is also what settles the per-server progress problem: **a RESUME POSITION does not
//! travel between servers** — it is about a file you are streaming from one host — so a design that
//! switched the copy in place would have had to explain a resume position that jumped. Navigating
//! makes it self-evident: you are on a different page, and it says where you are in *that* copy.
//! Like every other menu here this module only REPORTS an [`Action`]; the detail page performs it
//! (through its own `take_open_request` latch, the same one its Related shelf and episode filmstrip
//! raise).
//!
//! This paragraph said "watch state does not travel between servers" flat, and half of that stopped
//! being true when [`crate::viewstate`] began fanning a Mark as Watched out over every source
//! holding the guid. The two halves are not the same claim and the split is the whole point: the
//! **watched flag follows the TITLE** (having seen a film is not a fact about a host), while the
//! **offset stays with the copy**. Nothing here changed — this panel still navigates rather than
//! swaps, for the reason above, which is about the offset.
//!
//! **Ordering is the copy that PLAYS first**, then what a viewer would prefer: higher quality
//! first, and at equal quality yours before a friend's, because yours cannot go offline mid-film.
//! The design's own example pins the first clause against the second — a 1080p copy you are
//! standing on sorts above a friend's 4K one.
//!
//! Explicitly rejected, and not to be re-introduced: merging the copies invisibly and playing the
//! best one (it is what makes a film disappear when a friend goes offline, with no way to see why);
//! a permanent section on the page; switching the copy silently on OK.
//!
//! # Where the copies come from
//!
//! **The identity is the `guid` (`plex://movie/…`), never the `ratingKey`.** Every item-shaped
//! integer in Plex is server-local and starts at 1, so both servers in this household have a
//! `ratingKey` 4 and a section 1 (`docs/shared-servers.md` §1). Matching on one would confidently
//! offer a different film — which is why the STORE itself is keyed on the `(server, ratingKey)`
//! PAIR and not on the key alone (see [`FOR_SID`]): a resolve outliving the page that asked for it
//! is the normal case, not the exotic one.
//! [`install`] is the seam the multi-server data layer feeds once it can
//! ask each pinned source for its copy of a guid; until then the store is EMPTY at every mount, so
//! [`is_available`] is false, the button is not drawn, and a one-server install pays nothing —
//! not a query, not a control, not a row of layout.
//!
//! For headless verification there is a draw-only stand-in built on the SAME
//! `/tmp/plxnative-shared=<handle>` trigger the hero's own "Shared by …" run is judged through
//! (see [`dev_stand_in`]) — one trigger for one deliverable — which is how this panel is captured
//! on the TV before that layer exists.
#![allow(non_upper_case_globals)]
use crate::plex::ServerId;
use crate::ui::consts::*;
use crate::ui::popover::Popover;
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::{theme, Rect};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// One copy of the item on ONE source — everything a row needs, and nothing about layout.
///
/// Built by whoever resolved the item's `guid` across the pinned sources; this module only orders,
/// marks and draws them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AltCopy {
    /// The registry slot the copy lives on ([`crate::plex::servers`]). It is the row's IDENTITY —
    /// the gate counts distinct values of it, and OK resolves the destination client through it —
    /// so a copy whose source is not registered carries [`ServerId::UNSET`] and can be listed but
    /// never navigated to.
    pub(crate) sid: ServerId,
    /// The LIBRARY this copy is in, on that server ("Movies", "Film Club") — the row's label.
    /// Libraries are what a person browses; the machine name (`nas-home`) belongs to the Sources
    /// list and to a failure read-out, and appears nowhere else in the product.
    pub(crate) library: String,
    /// **The CREDIT for that server** — `plex::servers::owner_credit`'s answer, the same one the
    /// shelf headings and the Sources list draw, and NOT the raw `sourceTitle` it used to be.
    /// `Some(handle)` for a person outside this household; `None` when there is nobody to credit,
    /// which is our own server, the household's own server whichever Plex Home profile is watching,
    /// and a share plex.tv never named.
    ///
    /// `None` is the ABSENCE of an owner, not an empty one: it is what the row spells
    /// [`OWN_ACCOUNT`], and it is the ordering's own-before-a-friend's tiebreak. That read-out is
    /// exactly right for the first two cases and a slight over-claim for the third — see
    /// `docs/shared-servers.md` §13, which records why an unnamed external share is not
    /// distinguishable here today.
    ///
    /// Stamped when the resolve lands and RE-stamped by [`restamp_owners`] whenever the registry
    /// re-describes a source, because this is a copy of a fact that can be corrected under an open
    /// page.
    pub(crate) owner: Option<String>,
    /// That server's ratingKey for the item — per-server, and the whole reason the identity above
    /// it is a guid. Where OK navigates.
    pub(crate) rk: String,
    /// Runtime in ms, for the row's sub-line. 0 = unknown, and then the sub-line is the owner
    /// alone rather than "0 min".
    pub(crate) dur_ms: i64,
    /// `Media.videoResolution` ("4k" / "1080" / "sd"), plus the stored frame size as its fallback —
    /// exactly the trio `ui::fmt::resolution` takes, so the badge here and the hero's media chip
    /// are one function and cannot spell the same file two ways.
    pub(crate) res: String,
    pub(crate) width: i64,
    pub(crate) height: i64,
}

/// What OK on the highlighted row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    /// Open `rk` on server `sid` — that copy's own page. Never a swap of the copy in place.
    Open {
        sid: ServerId,
        rk: String,
    },
}

/// The panel's width — the design's `altPanelW`. Wide because the row fills all four of the cell's
/// content places (library, owner, runtime, class) and the two on the right are fixed runs: the
/// label block gets whatever is left, and it is the library name that must not elide.
const PANEL_W: f32 = 700.0;
/// The pinned ~20px corner radius, the item menu's.
const PANEL_RAD: f32 = 20.0;
/// Air between the button that opened the panel and the panel itself — one `space` rung.
const BTN_GAP: f32 = theme::space::MD;
/// Keep-out from the screen edges, so a panel opened low on the page is still whole.
///
/// Per AXIS, for `item_menu::EDGE`'s reason: `space::XL` 64 clears `MARGIN_Y` vertically and misses
/// `MARGIN_X` horizontally by 32px.
const EDGE: f32 = theme::space::XL;
const EDGE_X: f32 = crate::ui::consts::MARGIN_X;
/// Scrim peak alpha — the LIBRARY toolbar chip menu's (`ui::library`), deliberately, because this
/// is the same object one page over: a chip-shaped control on a live page opening a list over it.
/// The page recedes; it is not blanked, and the hero behind stays readable.
const SCRIM_A: f32 = 0.45;
/// How far the panel rises into place, matching those same chip menus.
const RISE: f32 = Popover::RISE;

/// The item the store describes, as the page's own mounted ratingKey. It does two jobs: with
/// [`FOR_SID`] it is the mailbox key [`install`] matches a landing against (a guid resolve that
/// outlived the page it was asked for must not land on the next one), and on its own it is the `rk`
/// half of "which copy am I on now", whose other half is [`here_sid`].
static mut FOR_RK: String = String::new();
/// The SERVER that rk belongs to — stamped by [`reset`] from the row that opened the page, and the
/// other half of the mailbox key.
///
/// Not decoration: every `ratingKey` Plex issues is a server-local integer dense from 1, so this
/// household's two servers both have a film 4 (docs/shared-servers.md §1). A resolve is one round
/// trip PER SOURCE and a dead share costs a full `connect(2)` timeout, so leaving our film 4 and
/// opening the share's film 4 while one is out is ordinary — and the rk alone says they are the
/// same page. The generation guard in `metadata::pump_alt_sources` cannot see it either: it only
/// moves when a DETAIL lands, and the new page's has not. The panel would list the other machine's
/// copies, its tick would be missing or on the wrong row, and OK would open a different film.
static mut FOR_SID: ServerId = ServerId::UNSET;
/// Every copy of that item, in the order the producer found them (this module orders them itself).
static mut COPIES: Vec<AltCopy> = Vec::new();

/// Shared open/appear choreography — and `caching_host`, because the detail page under this
/// anchored menu is standing still while it is up (see [`crate::ui::popover::host`]).
static mut POP: Popover = Popover::new().caching_host();
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The destination per global row index — parallel to the rows the panel was BUILT from, so a
/// selection can never be resolved against a differently-ordered list (`more_menu`'s rule: the row
/// list IS the mapping).
static mut DESTS: Vec<(ServerId, String)> = Vec::new();
/// The button's drawn rect at open time, in screen coords — what the panel anchors to.
static mut ANCHOR: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the key
/// ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes nothing
/// else, so without this the fingerprint records the panel opening and closing and nothing between.
/// Through `addr_of!` rather than the module's own `table()`, which hands out a `&'static mut`.
pub(crate) fn sel() -> i32 {
    unsafe { (*addr_of!(TABLE)).sel }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn dests() -> &'static mut Vec<(ServerId, String)> {
    unsafe { &mut *addr_of_mut!(DESTS) }
}
fn copies() -> &'static [AltCopy] {
    unsafe { &*addr_of!(COPIES) }
}

// ---- the store -------------------------------------------------------------------------------

/// Point the store at a freshly mounted item and drop whatever the last one had.
///
/// Called by the detail page from every mount, BEFORE anything can be installed for the new item.
/// It also closes the panel: a popover that survived the page under it would be listing another
/// film's copies over this one's hero.
///
/// `item_sid` is the ROW's server, never `plex::current_server()` — a merged Home shelf holds rows
/// from more than one machine, and the row is the only thing that knows which. An empty `item_rk`
/// (the page closing, or a mount with no row yet) pairs with [`ServerId::UNSET`]: nothing can then
/// land, which is the point.
pub(crate) fn reset(item_sid: ServerId, item_rk: &str) {
    hide();
    // ASSIGNMENT, not `ptr::write`: these are owning heap values, and `write` overwrites without
    // running the old one's destructor — which here would leak a `String` and a `Vec<AltCopy>` on
    // every detail mount. (The `.write()` idiom this file's neighbours use is sound where they use
    // it, on `Copy` types and `&'static [T]`.)
    unsafe {
        *addr_of_mut!(FOR_RK) = item_rk.to_string();
        *addr_of_mut!(COPIES) = Vec::new();
        *addr_of_mut!(STAND_RK) = String::new();
        addr_of_mut!(STAND_OWNS).write(false);
    }
    unsafe { addr_of_mut!(FOR_SID).write(item_sid) }; // `Copy`, so `write` is the right idiom here
}

/// Install the copies resolved for `(item_sid, item_rk)` — the seam the multi-server data layer
/// feeds.
///
/// **A landing only applies while it is still the item being awaited**, the invariant
/// `metadata::pump_detail` exists for: a guid resolve is a per-server round trip, so one asked for
/// the page you just left can land on the page you just opened, and a wrong list here would offer
/// to navigate to a different film.
///
/// The test is the PAIR, through `plex::same_item` — the one place that rule lives. An rk-only test
/// passes for the SHARE's film 4 while our film 4's resolve is landing, which is not an edge case:
/// both servers number their items from 1 (see [`FOR_SID`]). Repaints, so it invalidates the frame
/// gate — a landing that grows the actions row must be visible without waiting for a keypress.
pub(crate) fn install(item_sid: ServerId, item_rk: &str, list: Vec<AltCopy>) {
    let here = (unsafe { addr_of!(FOR_SID).read() }, unsafe {
        (*addr_of!(FOR_RK)).as_str()
    });
    if !crate::plex::same_item((item_sid, item_rk), here) {
        return;
    }
    // A real resolve replaces whatever was there, stand-in included — so it is no longer the
    // stand-in's list and `regrade` below must grade it.
    unsafe { addr_of_mut!(STAND_OWNS).write(false) };
    let mut list = list;
    // **The worker's stamp is not trusted.** `resolve_alt_sources` reads the credit on a background
    // thread, and a resolve dispatched before a roster refresh re-grades that credit lands after
    // it — past `pump_alt_sources`' facts epoch, which it has already consumed. Regrading here is
    // the only point that sees both the list and the current answer.
    regrade(&mut list);
    unsafe { *addr_of_mut!(COPIES) = list }; // assignment — see [`reset`]
    rebuild_if_showing();
    crate::ui::idle::invalidate();
}

/// **Re-read every retained copy's CREDIT from the registry.** Called when the facts epoch moves —
/// `plex::servers::facts_gen`, i.e. a source has been re-described — which is a different event
/// from the roster changing and must not be answered the same way.
///
/// The copies themselves are still correct: a re-grade of who is credited for a server does not
/// change which servers hold the item, so discarding the list (or the resolve in flight for it)
/// would throw away good work and leave the panel's control absent until the page was remounted.
/// What IS stale is this struct's copy of `owner`, taken at resolve time — and it is the sixth and
/// last surface that draws the "Shared by …" decision, so leaving it saying the old thing puts the
/// account holder's name back on the household's own library after every other surface has dropped
/// it.
///
/// **MAIN THREAD**, like every other reader and writer of `COPIES` — `metadata::pump_alt_sources`
/// is its only caller and runs on the SDL loop. The workers in this chain touch the mutex-protected
/// result slot and never this store.
pub(crate) fn restamp_owners() {
    let changed = unsafe { regrade(&mut *addr_of_mut!(COPIES)) };
    if changed {
        rebuild_if_showing();
        crate::ui::idle::invalidate();
    }
}

/// Re-materialise the panel's table whenever it is ON SCREEN — which is **not** [`is_open`].
///
/// The rows are a SNAPSHOT: `open` builds `TABLE` and `DESTS` once and `draw` renders that, so a
/// correction reaches `COPIES` and stops there. Rebuilding is what carries it the last step, and
/// the order moves with the text — `owner` is the own-before-a-friend's tiebreak in [`rows`], so a
/// corrected panel can legitimately swap two of its rows.
///
/// `visible()` and not `is_open()`, because [`close`] runs the appear choreography BACKWARDS: it
/// clears "open" immediately and keeps drawing for the length of the fade. A panel dismissed a
/// frame before the correction would otherwise spend that fade still naming the wrong person, which
/// is a small window and exactly the kind that ships.
fn rebuild_if_showing() {
    if pop().visible() {
        rebuild_table(Sel::Keep);
    }
}

/// **Re-read a copy list's CREDITS from the registry**, `true` when any of them moved.
///
/// The ONE place `AltCopy::owner` is decided, applied at both boundaries where a list can be
/// wrong: on the way IN ([`install`], because a resolve dispatched before a correction lands after
/// it and no epoch downstream can see that) and on an already-installed list ([`restamp_owners`],
/// because a roster refresh re-grades the credit under a mounted page). Stamping it on the worker
/// as well would be a third opinion; the worker's value is simply overwritten here.
///
/// **Skipped entirely while the headless stand-in owns the list** — see [`dev_stand_in`], whose
/// whole purpose is to FABRICATE a borrowed copy on a slot the registry describes as ours. The
/// trigger outranks the real answer everywhere else in this app for the same reason.
///
/// The guard is [`STAND_OWNS`], an explicit "the current `COPIES` came from the stand-in", and not
/// "the trigger is armed" — which is what it was for one round, and is a different statement.
/// A real resolve can land over a stand-in list (`install` takes it and `STAND_RK` stops the
/// stand-in rebuilding), and an armed-but-EMPTY trigger builds no stand-in at all; on both, "armed"
/// would have gone on suppressing the regrade for a list the stand-in does not own.
///
/// **MAIN THREAD** — see [`restamp_owners`].
fn regrade(list: &mut [AltCopy]) -> bool {
    if unsafe { addr_of!(STAND_OWNS).read() } {
        return false;
    }
    let mut moved = false;
    for c in list.iter_mut() {
        let credit = crate::plex::server_facts(c.sid)
            .map(|f| f.handle.clone())
            .unwrap_or_default();
        // `None` is the absence of an owner and must not become `Some("")` — the same guard
        // `metadata::alt_copies` applies when it stamps this field in the first place.
        let next = (!credit.is_empty()).then_some(credit);
        if c.owner != next {
            c.owner = next;
            moved = true;
        }
    }
    moved
}

/// Does the headless stand-in own the CURRENT `COPIES`? Set where [`dev_stand_in`] writes them,
/// cleared by [`install`] (a real resolve has replaced them) and by [`reset`] (a new page). Always
/// `false` in a release build, where the stand-in is compiled out entirely.
static mut STAND_OWNS: bool = false;

/// Drop copies whose grant left the live registry while this detail page remained mounted.
/// Called only when the registry generation moves, so the ordinary per-frame path pays nothing.
pub(crate) fn prune_inactive() {
    let list = unsafe { &mut *addr_of_mut!(COPIES) };
    let before = list.len();
    list.retain(|c| crate::plex::client_for(c.sid).is_some());
    if list.len() != before {
        close();
        crate::ui::idle::invalidate();
    }
}

/// How many distinct SOURCES hold this item. The gate is stated in sources rather than in copies
/// because that is the design's own wording — two copies in two libraries of ONE server is not
/// "also available *elsewhere*", and the row that would name the other one has nowhere to send you
/// that you are not already.
fn source_count(list: &[AltCopy]) -> usize {
    let mut n = 0;
    for (i, c) in list.iter().enumerate() {
        if !list[..i].iter().any(|p| p.sid == c.sid) {
            n += 1;
        }
    }
    n
}

/// **The gate**: is a second pinned source holding this item? The actions row asks before it draws
/// the control, so with one source there is no button, no layout for it and no draw call.
pub(crate) fn is_available() -> bool {
    source_count(copies()) >= 2
}

// ---- the model (pure) ------------------------------------------------------------------------

/// A copy's quality, as SCAN LINES — the ordering's second key.
///
/// It reads the same three fields `ui::fmt::resolution` badges the row with, in the same
/// precedence (the server's own class first, the stored frame size only as its fallback), so the
/// ladder the eye sees on the badges and the order the rows are in cannot disagree. Saturating,
/// because these are wire values through the lenient `de_i64` and a garbage 19-digit height must
/// not overflow into a top-of-list sort key.
fn scan_lines(c: &AltCopy) -> i64 {
    let r = c.res.trim().to_ascii_lowercase();
    match r.as_str() {
        "" => {}
        "8k" => return 4320,
        "4k" => return 2160,
        "sd" => return 480,
        _ if r.chars().all(|ch| ch.is_ascii_digit()) => return r.parse::<i64>().unwrap_or(0),
        _ => {}
    }
    if c.height > 0 {
        c.height
    } else {
        c.width.saturating_mul(9) / 16
    }
}

/// One drawn row, resolved: what it says and where OK goes. The panel builds its `TableView` from
/// these and nothing else, so the ordering, the tick and the destination map are ONE list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AltRow {
    /// the library, on that source
    pub(crate) label: String,
    /// whose copy it is: `"This account"` / `"friend"`
    pub(crate) detail: String,
    /// the runtime, for the row's trailing read-out — `None` for a copy the server sent no
    /// duration for, which then says nothing rather than "0 min"
    pub(crate) value: Option<String>,
    /// the resolution class, or `None` for a copy the server described no video for
    pub(crate) badge: Option<String>,
    /// the copy the page is on NOW — the picker's tick
    pub(crate) checked: bool,
    pub(crate) sid: ServerId,
    pub(crate) rk: String,
}

/// What the sub-line calls a source with no owner: the signed-in account's own server. A PERSON in
/// every case, which is the design's rule for every browsing surface — the machine name never
/// appears outside the Sources list.
const OWN_ACCOUNT: &str = "This account";

/// Build the panel's rows from `list`, given the copy the page is standing on (`here_sid` +
/// `here_rk`). PURE — every ordering and marking decision in this module is here, and the host
/// suite drives it directly.
///
/// **Exactly one row is ticked**, by construction: the mark is placed on the FIRST copy matching
/// the page's own (server, ratingKey), so a producer that listed the same copy twice still yields
/// one "you are here" rather than two. A list with no such copy is ticked nowhere at all — which
/// is the honest answer while a landing is being reconciled, and never two.
///
/// The order is the design's: the copy that plays first, then higher quality, then yours before a
/// friend's, then by library name so the list cannot reshuffle between two frames that resolved
/// the same facts.
pub(crate) fn rows(list: &[AltCopy], here_sid: ServerId, here_rk: &str) -> Vec<AltRow> {
    // `same_item`, not a hand-rolled `&&`: it is the one place the pair rule lives, and its own doc
    // promises every stored-item lookup routes through it. These two sites were the last that did
    // not — and they are the ones that decide which row wears the tick and which press does nothing.
    let here = list
        .iter()
        .position(|c| crate::plex::same_item((c.sid, &c.rk), (here_sid, here_rk)));
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&list[a], &list[b]);
        // `false` sorts before `true`, so each key is written as "the one that loses"
        (
            here != Some(a),
            std::cmp::Reverse(scan_lines(ca)),
            ca.owner.is_some(),
            &ca.library,
        )
            .cmp(&(
                here != Some(b),
                std::cmp::Reverse(scan_lines(cb)),
                cb.owner.is_some(),
                &cb.library,
            ))
    });
    idx.into_iter()
        .map(|i| {
            let c = &list[i];
            let who = c.owner.as_deref().unwrap_or(OWN_ACCOUNT);
            AltRow {
                label: c.library.clone(),
                detail: who.to_string(),
                // a copy the server sent no duration for leaves the read-out slot EMPTY rather
                // than claiming "0 min" — the same rule the sub-line's dangling separator followed
                // while the runtime was part of it
                value: (c.dur_ms > 0).then(|| crate::ui::fmt::dur_long(c.dur_ms)),
                badge: crate::ui::fmt::resolution(&c.res, c.width, c.height),
                checked: here == Some(i),
                sid: c.sid,
                rk: c.rk.clone(),
            }
        })
        .collect()
}

/// The rows as the panel would build them right now: the store, marked against **the server the
/// open PAGE belongs to** and the item the store was filled for.
///
/// The page's server, not `plex::current_server()`. Those were the same thing only while browsing a
/// shared library also re-pointed `current`; once that stopped, opening a borrowed film left
/// `current` on our own server, no copy matched the pair, and the panel drew **no tick at all** —
/// owner-reported. The tick answers "which of these am I looking at", and only the page knows.
fn live_rows() -> Vec<AltRow> {
    rows(copies(), here_sid(), unsafe {
        (*addr_of!(FOR_RK)).as_str()
    })
}

/// The server the open PAGE is on — the app-wide answer, asked in ONE place.
///
/// This module had two spellings of it, and they disagreed: `live_rows` read the loaded `Detail`
/// while `action_at`'s caller passed `plex::current_server()`, so the row that wore the tick and
/// the row a press treated as "you are already here" could be different rows. Both also fell
/// through to the current server during the mount window, which is exactly when `metadata::current`
/// is empty by design (`open_rk` clears it for the whole fetch) — and after browsing stopped
/// re-pointing `current`, that fallback is a DIFFERENT machine rather than a harmless synonym.
///
/// `detail::mounted_sid` is the one derivation: the loaded item's server, else the page's own
/// mounted sid, which was stamped from the row that opened it.
///
/// [`FOR_SID`] is not a second spelling of this and must not become one: it is the MAILBOX key,
/// stamped by [`reset`] from the very same value on the very same call as `mounted_sid` (see
/// `detail::mount_rk`), and read only by [`install`]. They agree by construction — which is also
/// what makes the tick and the landing gate answer about one page.
fn here_sid() -> ServerId {
    crate::ui::detail::mounted_sid()
}

// ---- the panel -------------------------------------------------------------------------------

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// Open the list, anchored to the control that opened it (the *Also available* pill's drawn rect).
/// Selection starts on the copy you are on — the row the tick is against — because that is where
/// the eye already is, and one DOWN from it is the alternative.
pub(crate) fn open(anchor: Rect) {
    unsafe { addr_of_mut!(ANCHOR).write(anchor) };
    rebuild_table(Sel::OnTheCopyYouAreOn);
    pop().open();
}

/// Where the selection lands when [`rebuild_table`] runs.
enum Sel {
    /// Opening: on the copy you are on — the row the tick is against — because that is where the
    /// eye already is, and one DOWN from it is the alternative.
    OnTheCopyYouAreOn,
    /// Rebuilding under an OPEN panel: leave the cursor where the user put it. The rows can
    /// re-order beneath it (a credit correction moves the own-before-a-friend's tiebreak), which
    /// is a smaller surprise than the cursor jumping back to the tick mid-interaction.
    Keep,
}

/// Materialise `COPIES` into the popover's table and destination list. Called on [`open`] and
/// again whenever the rows' content changes under an open panel ([`restamp_owners`]) — the table
/// is a snapshot, not a view, so nothing else makes a correction visible.
fn rebuild_table(sel_mode: Sel) {
    let rs = live_rows();
    let mut sec = Section::new("");
    let mut sel = match sel_mode {
        Sel::Keep => table().sel.max(0),
        Sel::OnTheCopyYouAreOn => 0,
    };
    dests().clear();
    for (i, r) in rs.iter().enumerate() {
        // all four of the cell's content places, which is what the row has to say: the tick is
        // WHICH copy you are on, the label WHICH LIBRARY, the sub-line WHOSE, the read-out HOW LONG
        // and the badge WHAT YOU GET. The runtime used to be a dotted second half of the sub-line,
        // which left the read-out slot empty and made the one fact that lines up down the panel the
        // one buried mid-string.
        let mut row = Row::new(r.label.clone())
            .detail(r.detail.clone())
            .checked(r.checked);
        if let Some(v) = &r.value {
            row = row.value(v.clone());
        }
        if let Some(b) = &r.badge {
            row = row.badge(Badge::Text(b.clone()));
        }
        sec = sec.row(row);
        dests().push((r.sid, r.rk.clone()));
        if r.checked && matches!(sel_mode, Sel::OnTheCopyYouAreOn) {
            sel = i as i32;
        }
    }
    sel = sel.min((rs.len() as i32 - 1).max(0));
    table().compact = false; // rows carry a sub-line — the track menu's size class, not the account popover's
    table().set_sections(vec![sec], sel, false);
    debug_assert_eq!(dests().len() as i32, table().n_rows());
}

pub(crate) fn close() {
    pop().dismiss();
}
/// The INSTANT hide, for page teardown — the item or page this panel is about is being replaced
/// under it, so there is nothing for a fade to fade over. Interactive exits use [`close`], which
/// runs the appear choreography backwards (`Popover::dismiss`); a teardown that used it would
/// leave `visible()` true with the old rows in the sheet, drawn over the incoming page until the
/// spring ran out (Codex review, 2026-09-02).
pub(crate) fn hide() {
    pop().close();
}

pub(crate) fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    } else {
        return;
    }
    // This menu moved; the page behind it did not — see `popover::note_own_damage`.
    crate::ui::popover::note_own_damage();
}

/// Commit the highlighted row and close. The row you are ALREADY on reports nothing — there is
/// nowhere to navigate to, and the tick has already answered the question the press was asking.
pub(crate) fn on_ok() -> Action {
    let sel = table().sel;
    let act = action_at(dests(), sel, here_sid(), unsafe {
        (*addr_of!(FOR_RK)).as_str()
    });
    // A navigation replaces the page this menu stands on, so the menu goes at once; a press on the
    // row you are already on is a plain dismissal and fades like BACK would.
    if matches!(act, Action::None) {
        close();
    } else {
        hide();
    }
    act
}

/// The index→destination mapping, pure so the "the row you are on is not a destination" rule is
/// host-testable. A selection outside the list is [`Action::None`] rather than whatever happens to
/// sit at that index in some other row set (`sel` survives a rebuild).
fn action_at(list: &[(ServerId, String)], sel: i32, here_sid: ServerId, here_rk: &str) -> Action {
    let Some((sid, rk)) = usize::try_from(sel).ok().and_then(|i| list.get(i)) else {
        return Action::None;
    };
    if crate::plex::same_item((*sid, rk), (here_sid, here_rk)) || rk.is_empty() {
        return Action::None;
    }
    Action::Open {
        sid: *sid,
        rk: rk.clone(),
    }
}

/// Pointer hover: focus follows the cursor over the rows.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if !is_open() {
        return;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
    }
}

/// Pointer click: commit the row under the cursor (same as OK); a click anywhere else dismisses,
/// which is what a click outside a popover means everywhere in this app.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if !is_open() {
        return Action::None;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
        return on_ok();
    }
    close();
    Action::None
}

/// Under the button when there is room, above it when there is not — never over it, so the control
/// that opened the panel stays readable beside its own list. Horizontally it hangs off the
/// button's left edge (the list and the label it came from share a margin), pulled inside the
/// screen's keep-out. Pure (anchor + measured height in, rect out), so the placement rules are
/// host-testable without the module's statics.
fn panel_at(a: Rect, content_h: f32) -> Rect {
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE);
    let below = a.y + a.h + BTN_GAP;
    let y = if below + h <= SCR_H - EDGE {
        below
    } else {
        (a.y - BTN_GAP - h).max(EDGE)
    };
    let x = a.x.clamp(EDGE_X, (SCR_W - EDGE_X - PANEL_W).max(EDGE_X));
    Rect::new(x, y, PANEL_W, h)
}

fn panel_rect() -> Rect {
    panel_at(
        unsafe { addr_of!(ANCHOR).read() },
        table().measured_height(),
    )
}

pub(crate) fn update(dt: f32) {
    if !pop().visible() {
        return;
    }
    // This menu's own springs, kept out of the host page's motion — `popover::own_motion`.
    let _own = crate::ui::popover::own_motion();
    pop().update(dt);
    // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
    table().update(dt, panel_rect().h);
}

pub(crate) fn draw() {
    if !pop().visible() {
        return;
    }
    // Live over the frozen host, and — since `painter` draws the scrim as its first act — the
    // snapshot this guard takes is still of the UNDIMMED page. See `popover::host::live`.
    let _live = crate::ui::popover::host::live();
    let p = pop().painter(SCRIM_A, RISE);
    let r = panel_rect();
    pop().panel(p, r, PANEL_RAD);
    table().draw(p, r);
}

// ---- the headless stand-in ---------------------------------------------------------------------
//
// Reached through `dev::read`, so the whole of it is absent from a `RELEASE=1` build at compile
// time along with the rest of the `/tmp` surface. The trigger literal is
// `/tmp/plxnative-shared`, spelled here for the catalog grep in `docs/agent-reference.md`.

/// Which item the stand-in has been built for, so it is built ONCE per page rather than every
/// frame. Cleared by [`reset`] with the rest of the store.
static mut STAND_RK: String = String::new();

/// Build the headless stand-in once the item has LANDED — called every frame by `detail::update`,
/// and doing nothing at all in the ordinary case (two string compares, no allocation).
///
/// It cannot be built in [`reset`], where the rest of the store is: a mount deliberately clears
/// `metadata::current()` for the whole 2-5 round-trip fetch window (`detail::open_rk`), so at that
/// moment there is no runtime, no resolution class and no title to build a copy of the item FROM.
pub(crate) fn pump() {
    let for_rk = unsafe { (*addr_of!(FOR_RK)).as_str() };
    if for_rk.is_empty() || for_rk == unsafe { (*addr_of!(STAND_RK)).as_str() } {
        return; // no page, or this one has already had its chance — two string compares
    }
    // Wait for the item, and do NOT mark the attempt until it is here: a mount keeps
    // `metadata::current()` empty for the whole fetch, and spending the one chance during that
    // window would mean no stand-in at all on the page it was armed for.
    if crate::metadata::current()
        .filter(|d| d.rk == for_rk)
        .is_none()
    {
        return;
    }
    // Marked whatever the outcome, so the ordinary build — where the trigger is not armed at all —
    // opens the `/tmp` file ONCE per page rather than on every frame of it.
    unsafe { *addr_of_mut!(STAND_RK) = for_rk.to_string() };
    if let Some(list) = dev_stand_in(for_rk) {
        unsafe {
            *addr_of_mut!(COPIES) = list;
            addr_of_mut!(STAND_OWNS).write(true);
        }
        crate::ui::idle::invalidate();
    }
}

/// A DRAW-ONLY copy list for `/tmp/plxnative-shared=<handle>`, so this panel and the control that
/// opens it can be judged on a television before the multi-server data layer exists.
///
/// **It reuses the trigger the hero's own "Shared by …" run is judged through** rather than adding
/// a second one for the same deliverable: that trigger already means "pretend this item came from
/// `<handle>`", and a second copy of the film on `<handle>`'s source is the other half of the same
/// pretence.
///
/// What it stands in for, exactly:
///
/// * **your copy** is the real one — the page's own ratingKey, on the current server, with the
///   loaded item's own runtime and resolution class, in the library the browse store says you are
///   in. Nothing about it is invented, which is what makes the TICK meaningful.
/// * **their copy** is that same film on a SECOND REGISTRY SLOT ([`stand_in_slot`]). It is a real
///   slot with a real client, so OK on it performs the real source switch and opens a real page —
///   the one behaviour of this panel that cannot be judged from a still.
/// * its resolution is **one class better than yours**, and that is the stand-in's ONE invention.
///   It exists because the ordering rule is otherwise unobservable: the design's own example is a
///   1080p copy you are standing on sorting above a friend's 4K one, which is the case that
///   separates "the copy that plays" from "the best copy". With two equal badges the panel cannot
///   show that it got the rule right.
///
/// `None` (and no stand-in) when the trigger is not armed or the item has not landed yet.
///
/// NB this is **not** one of the three "WINS WHEN ARMED" precedence sites (`metadata::fetch_detail`,
/// `ui::home`'s `dev_source`, `ui::search::results`' `dev_source`), which pick between the trigger
/// and a real answer. There is no real answer here to outrank: an unarmed trigger means no panel
/// content at all, and an armed-but-EMPTY file means the same, because a copy list has to be
/// attributed to somebody.
#[cfg(not(test))]
fn dev_stand_in(item_rk: &str) -> Option<Vec<AltCopy>> {
    let handle = crate::dev::read("shared").filter(|h| !h.is_empty())?;
    let d = crate::metadata::current().filter(|d| d.rk == item_rk)?;
    let library = crate::browse::section_title(crate::browse::cur()).to_string();
    let here = crate::plex::current_server();
    let theirs = stand_in_slot()?;
    let v = stand_in(
        &handle,
        &library,
        item_rk,
        &d.video_resolution,
        d.dur_ms,
        here,
        theirs,
    );
    crate::log(&format!(
        "altsources: stand-in for rk={item_rk} on slot {} (dev)",
        theirs.raw()
    ));
    Some(v)
}
#[cfg(test)]
fn dev_stand_in(_item_rk: &str) -> Option<Vec<AltCopy>> {
    None // the host suite must not depend on what this dev Mac happens to have under /tmp
}

/// The registry slot the stand-in's borrowed copy lives on — a REAL one, because the point of the
/// stand-in is to exercise the switch.
///
/// A genuinely different server if one is registered. Otherwise the current server registered a
/// SECOND time under a synthetic machine id: from everything this feature does — counting sources,
/// resolving a client, switching, refetching — that is a second source, and because it is the same
/// machine the ratingKey it carries is honestly the same film. What it cannot stand in for is a
/// server going offline, or a library and a resolution that differ for real.
///
/// The token comes from the harness's own `/tmp/plxnative-token`, which is the only place a session
/// token is available to a dev path; with no token there is nothing to build a working client from,
/// so there is no stand-in at all rather than one that 401s.
#[cfg(not(test))]
fn stand_in_slot() -> Option<ServerId> {
    let here = crate::plex::current_server();
    // `server_ids`, never `0..server_count()`: slot numbers are permanent and a sign-out retires
    // the ones below the registry's floor, so the live roster is a window and not a prefix.
    let other =
        crate::plex::server_ids().find(|&id| id != here && crate::plex::client_for(id).is_some());
    if let Some(id) = other {
        return Some(id); // a real second server is already registered — use it
    }
    let c = crate::plex::client_opt()?;
    let token = crate::dev::read("token").filter(|t| !t.is_empty())?;
    // …and a registry with no room left answers `UNSET`, which is no stand-in at all rather than
    // one that resolves to whatever happens to be current.
    Some(crate::plex::register(
        &format!("standin-{}", c.machine_id()),
        c.host(),
        c.port(),
        &token,
    ))
    .filter(|id| id.is_set())
}

/// [`dev_stand_in`]'s pure half — the two copies it describes, so the shape of what a device
/// capture is looking at is itself host-graded.
fn stand_in(
    handle: &str,
    library: &str,
    rk: &str,
    res: &str,
    dur_ms: i64,
    here: ServerId,
    theirs: ServerId,
) -> Vec<AltCopy> {
    let mine = AltCopy {
        sid: here,
        library: library.to_string(),
        owner: None,
        rk: rk.to_string(),
        dur_ms,
        res: res.to_string(),
        width: 0,
        height: 0,
    };
    let borrowed = AltCopy {
        sid: theirs,
        owner: Some(handle.to_string()),
        res: one_class_better(res),
        ..mine.clone()
    };
    vec![mine, borrowed]
}

/// The resolution class one rung above `res`, in [`scan_lines`]' own vocabulary — the stand-in's
/// single invention (see [`dev_stand_in`]). Anything already at the top, or unrecognised, is
/// returned unchanged rather than promoted into a class that does not exist.
fn one_class_better(res: &str) -> String {
    match res.trim().to_ascii_lowercase().as_str() {
        "sd" | "480" | "576" => "720".into(),
        "720" => "1080".into(),
        "1080" | "1440" => "4k".into(),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------------------------------------
// The pure half — the ordering, the gate and the row model. `open`/`draw` own `static mut`
// TABLE/POP/COPIES and are deliberately not `Sync`; everything a test can reach takes its inputs
// as arguments.
#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u16) -> ServerId {
        ServerId::from_raw(n)
    }
    /// A copy on server `s`, in `library`, owned by `owner` (`""` = this account), at class `res`.
    fn copy(s: u16, library: &str, owner: &str, rk: &str, res: &str) -> AltCopy {
        AltCopy {
            sid: sid(s),
            library: library.into(),
            owner: (!owner.is_empty()).then(|| owner.to_string()),
            rk: rk.into(),
            dur_ms: 7_020_000, // 1 hr 57 min, the design's own runtime
            res: res.into(),
            width: 0,
            height: 0,
        }
    }
    fn labels(rs: &[AltRow]) -> Vec<&str> {
        rs.iter().map(|r| r.label.as_str()).collect()
    }

    /// **The gate.** The control is drawn only when a SECOND pinned source holds the item — so one
    /// server, or two copies inside one server, draw nothing at all. The middle case is the one
    /// worth pinning: two rows would look like a working feature while the second row led back to
    /// the machine you are already on.
    #[test]
    fn the_button_appears_only_when_a_second_source_holds_the_item() {
        assert_eq!(source_count(&[]), 0, "nothing resolved yet");
        assert_eq!(
            source_count(&[copy(0, "Movies", "", "4", "1080")]),
            1,
            "one source is the 90% install"
        );
        assert_eq!(
            source_count(&[
                copy(0, "Movies", "", "4", "1080"),
                copy(0, "4K Movies", "", "9", "4k")
            ]),
            1,
            "two copies on ONE server are still one source"
        );
        assert_eq!(
            source_count(&[
                copy(0, "Movies", "", "4", "1080"),
                copy(1, "Film Club", "friend", "318", "4k")
            ]),
            2
        );
        // and a third source counts once however many copies it contributes
        assert_eq!(
            source_count(&[
                copy(0, "Movies", "", "4", "1080"),
                copy(1, "Film Club", "friend", "318", "4k"),
                copy(1, "Films", "friend", "319", "1080"),
            ]),
            2
        );
    }

    /// **The ordering rule, exactly as the design states it**: the copy that plays first, then what
    /// a viewer would prefer. The design's own example is the case that separates the two clauses —
    /// a 1080p copy you are standing on sorts above a friend's 4K one — and it is the row order
    /// the canvas draws.
    #[test]
    fn the_copy_that_plays_comes_first_and_the_rest_rank_by_preference() {
        let list = [
            copy(1, "Film Club", "friend", "318", "4k"),
            copy(0, "Movies", "", "4", "1080"),
        ];
        let rs = rows(&list, sid(0), "4");
        assert_eq!(
            labels(&rs),
            ["Movies", "Film Club"],
            "the copy that plays leads, whatever its class"
        );
        // the sub-line answers WHOSE and only that; the runtime is the trailing read-out, so it
        // lines up down the panel instead of sitting mid-string at a different x on every row
        assert_eq!(rs[0].detail, "This account");
        assert_eq!(rs[1].detail, "friend");
        assert_eq!(rs[0].value.as_deref(), Some("1 hr 57 min"));
        assert_eq!(rs[1].value.as_deref(), Some("1 hr 57 min"));
        assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
        assert_eq!(
            rs[1].badge.as_deref(),
            Some("4K"),
            "the badge is the hero's own resolution vocabulary"
        );

        // …and standing on the OTHER copy inverts only the first key, not the rest
        assert_eq!(labels(&rows(&list, sid(1), "318")), ["Film Club", "Movies"]);
    }

    /// Below the copy that plays, quality decides — and only at EQUAL quality does "yours cannot go
    /// offline mid-film" break the tie. Both halves are asserted against the same fixture, because
    /// getting the keys in the other order would pass a test for either one alone.
    #[test]
    fn quality_outranks_ownership_and_ownership_breaks_the_tie() {
        let here = copy(9, "On now", "", "1", "720");
        let mine = copy(0, "Movies", "", "4", "1080");
        let theirs_4k = copy(1, "Film Club", "friend", "318", "4k");
        let theirs_hd = copy(2, "Kino", "carol", "77", "1080");

        let rs = rows(
            &[
                here.clone(),
                mine.clone(),
                theirs_4k.clone(),
                theirs_hd.clone(),
            ],
            sid(9),
            "1",
        );
        assert_eq!(
            labels(&rs),
            ["On now", "Film Club", "Movies", "Kino"],
            "playing copy, then 4K, then the two 1080p with mine in front"
        );

        // the input order must not decide anything — the same set shuffled sorts identically
        let shuffled = rows(&[theirs_hd, theirs_4k, mine, here], sid(9), "1");
        assert_eq!(labels(&shuffled), labels(&rs));
    }

    /// The badge and the sort key are read off the SAME fields in the same precedence, so a list
    /// can never be ordered against a ladder the badges contradict. (The server's class wins over
    /// the stored frame size: a 2.35:1 1080p film is 1918x802, which a height rule would sort as
    /// 720p while its badge said 1080p.)
    #[test]
    fn the_sort_key_agrees_with_the_badge_it_is_drawn_beside() {
        let with = |res: &str, w: i64, h: i64| AltCopy {
            res: res.into(),
            width: w,
            height: h,
            ..Default::default()
        };
        let ladder = ["8k", "4k", "1080", "720", "576", "sd"];
        for pair in ladder.windows(2) {
            let (a, b) = (with(pair[0], 0, 0), with(pair[1], 0, 0));
            assert!(
                scan_lines(&a) > scan_lines(&b),
                "{} must outrank {}",
                pair[0],
                pair[1]
            );
        }
        // the class beats the frame size, exactly as `fmt::resolution` badges it
        let scope = with("1080", 1918, 802);
        assert_eq!(scan_lines(&scope), 1080);
        assert_eq!(
            crate::ui::fmt::resolution(&scope.res, scope.width, scope.height).as_deref(),
            Some("1080p")
        );
        // …and with no class at all both fall back to the frame, and still agree
        let noclass = with("", 3840, 2160);
        assert!(scan_lines(&noclass) > scan_lines(&with("", 1920, 1080)));
        assert_eq!(
            crate::ui::fmt::resolution(&noclass.res, noclass.width, noclass.height).as_deref(),
            Some("4K")
        );
        // a garbage height must not overflow into a top-of-list key
        assert!(scan_lines(&with("", i64::MAX, i64::MAX)) > 0);
    }

    /// **Exactly one copy is marked current** — the row model's whole claim about the tick. Marked
    /// once even when a producer lists the same copy twice, and marked NOWHERE (never twice, never
    /// on a guess) when the page's own copy is not in the list at all.
    #[test]
    fn exactly_one_row_is_ever_ticked() {
        let ticked = |rs: &[AltRow]| rs.iter().filter(|r| r.checked).count();

        let list = [
            copy(0, "Movies", "", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "4k"),
        ];
        let rs = rows(&list, sid(0), "4");
        assert_eq!(ticked(&rs), 1);
        assert!(
            rs[0].checked,
            "the tick is on the copy the page is standing on"
        );

        // a duplicated copy: one identity, one tick
        let dup = [
            copy(0, "Movies", "", "4", "1080"),
            copy(0, "Movies", "", "4", "1080"),
            copy(1, "LDN", "b", "318", "4k"),
        ];
        assert_eq!(
            ticked(&rows(&dup, sid(0), "4")),
            1,
            "one identity cannot be two 'you are here's"
        );

        // the same ratingKey on the OTHER server is a different copy — rk alone must not tick it
        assert_eq!(
            ticked(&rows(
                &[
                    copy(0, "Movies", "", "4", "1080"),
                    copy(1, "LDN", "b", "4", "4k")
                ],
                sid(0),
                "4"
            )),
            1
        );
        // …and nothing is ticked when the page's copy is not in the list yet
        assert_eq!(
            ticked(&rows(&list, sid(7), "4")),
            0,
            "no guess, no second tick"
        );
    }

    /// A copy the server sent no runtime for leaves the read-out slot EMPTY and still says whose it
    /// is — never "0 min", the dangling-clause rule the hero's facts row follows.
    #[test]
    fn a_copy_with_no_runtime_states_only_its_owner() {
        let mut c = copy(1, "Film Club", "friend", "318", "4k");
        c.dur_ms = 0;
        let rs = rows(&[c], sid(0), "4");
        assert_eq!(rs[0].detail, "friend");
        assert_eq!(rs[0].value, None, "an unknown runtime is absent, not zero");
        // and one with no video at all carries no badge rather than an empty chip
        let mut n = copy(0, "Movies", "", "4", "");
        n.dur_ms = 0;
        assert_eq!(rows(&[n], sid(0), "4")[0].badge, None);
    }

    /// OK NAVIGATES — and the row you are already on is not a destination: it reports nothing, so
    /// the panel simply dismisses rather than re-mounting the page under itself. An out-of-range
    /// selection is `None` too, never a neighbouring row's server.
    #[test]
    fn ok_navigates_to_another_copy_and_never_to_the_one_you_are_on() {
        let list = [
            (sid(0), "4".to_string()),
            (sid(1), "318".to_string()),
            (sid(2), String::new()),
        ];
        assert_eq!(
            action_at(&list, 0, sid(0), "4"),
            Action::None,
            "the copy you are on goes nowhere"
        );
        assert_eq!(
            action_at(&list, 1, sid(0), "4"),
            Action::Open {
                sid: sid(1),
                rk: "318".into()
            }
        );
        assert_eq!(
            action_at(&list, 2, sid(0), "4"),
            Action::None,
            "a copy with no ratingKey is not a destination"
        );
        assert_eq!(action_at(&list, 3, sid(0), "4"), Action::None);
        assert_eq!(action_at(&list, -1, sid(0), "4"), Action::None);
        assert_eq!(action_at(&[], 0, sid(0), "4"), Action::None);
    }

    /// The headless stand-in describes what a device capture is looking at, so its SHAPE is graded
    /// here: your real copy plus the same film on a second slot, one class better — which is the
    /// design's own example, and the only arrangement in which a still can show that the ordering
    /// rule put the copy that PLAYS above the better one.
    #[test]
    fn the_headless_stand_in_shows_the_case_the_ordering_rule_turns_on() {
        let v = stand_in("friend", "Movies", "4", "1080", 7_020_000, sid(0), sid(1));
        assert_eq!(
            source_count(&v),
            2,
            "…or the gate would refuse the very panel it exists to show"
        );
        assert_eq!(
            v[0].owner, None,
            "your copy is the real one, on the current server"
        );
        assert_eq!((v[0].rk.as_str(), v[0].dur_ms), ("4", 7_020_000));
        assert_eq!(v[1].owner.as_deref(), Some("friend"));
        assert_eq!(v[1].rk, v[0].rk, "the same film — the slot is what differs");

        let rs = rows(&v, sid(0), "4");
        assert!(rs[0].checked, "the tick is on yours…");
        assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
        assert_eq!(
            rs[1].badge.as_deref(),
            Some("4K"),
            "…and the better copy is the one BELOW it"
        );

        // the one invention is bounded: the top of the ladder is not promoted past itself, and an
        // unrecognised class is left exactly as the server spelled it
        assert_eq!(one_class_better("4k"), "4k");
        assert_eq!(one_class_better("sd"), "720");
        assert_eq!(one_class_better(""), "");
        assert_eq!(one_class_better("weird"), "weird");
    }

    /// **The store is a MAILBOX keyed on the pair, and the pair is what the landing is matched
    /// against.** A cross-source resolve is one round trip PER SOURCE and a share that has gone
    /// away costs a whole `connect(2)` timeout, so one asked for the page you just left routinely
    /// lands seconds after you have opened another — and with two servers registered "another
    /// page with the same ratingKey" is the ordinary case, since both number their items from 1.
    ///
    /// Nothing upstream can catch it: `metadata::pump_alt_sources`' generation guard only moves
    /// when a DETAIL lands, and the newly opened page's has not. So an rk-only test here put the
    /// OTHER machine's copies on this hero, where the tick would be missing (no listed copy matches
    /// the page) and OK would open a different film on a server you were not looking at.
    ///
    /// Touches the module's `static mut` store, so it holds `testlock::serial()` and empties the
    /// store on the way OUT as well as in — `ui::detail`'s hero-row tests count the controls this
    /// store produces, and a leaked list is a count they never touched.
    /// **A corrected credit re-stamps rows already on an OPEN page**, which is the sixth and last
    /// surface that draws the "Shared by …" decision (`plex::servers::owner_credit`,
    /// `docs/shared-servers.md` §13).
    ///
    /// `AltCopy::owner` is a COPY of the registry's credit, taken when the cross-source resolve
    /// landed. When a roster refresh re-grades that credit — the household's own server ceasing to
    /// be captioned with the account holder's handle — every other surface follows and this one
    /// could not: `metadata::pump_alt_sources` answered the change by invalidating the resolve and
    /// pruning, which retains the installed copies untouched and restarts nothing, so the row went
    /// on naming the person watching until the page was remounted.
    ///
    /// The row ORDER is asserted with it, because `owner` is the own-before-a-friend's tiebreak in
    /// [`rows`] and a restamp that did not reach the ordering would put the household's copy below
    /// a friend's on a page that had just decided they were equals.
    #[test]
    fn a_re_described_source_restamps_the_credit_on_an_open_page() {
        struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Fresh {
            fn drop(&mut self) {
                reset(ServerId::UNSET, "");
                crate::plex::reset_servers_for_test();
            }
        }
        let _g = Fresh(crate::testlock::serial());
        crate::plex::reset_servers_for_test();
        let house = crate::plex::register_for_test("alt-house", "127.0.0.1", 1, "t", "cid");
        let friend = crate::plex::register_for_test("alt-friend", "127.0.0.1", 2, "t", "cid");
        assert_eq!((house, friend), (sid(0), sid(1)), "slots 0 and 1");

        // what a build without the rule published: the household's own server wearing the account
        // holder's handle, and the panel's rows stamped from it
        crate::plex::describe_server(house, "Mac mini", "admin", false);
        crate::plex::describe_server(friend, "nas-home", "friend", false);
        reset(house, "4");
        install(
            house,
            "4",
            vec![
                copy(0, "Movies", "admin", "4", "1080"),
                copy(1, "Film Club", "friend", "318", "1080"),
            ],
        );
        assert_eq!(
            rows(copies(), house, "4")
                .iter()
                .map(|r| r.detail.clone())
                .collect::<Vec<_>>(),
            ["admin", "friend"]
        );

        // the roster refresh re-grades the household's own server, with NO new resolve
        crate::plex::describe_server(house, "Mac mini", "", false);
        restamp_owners();

        let after = rows(copies(), house, "4");
        assert_eq!(
            after.iter().map(|r| r.detail.clone()).collect::<Vec<_>>(),
            [OWN_ACCOUNT, "friend"],
            "the row follows the registry off a credit without waiting for a re-resolve"
        );
        assert_eq!(
            copies()[0].owner,
            None,
            "and absence is spelled `None`, never `Some(\"\")`"
        );

        // the friend is untouched — a restamp is not a blanket clear
        assert_eq!(copies()[1].owner.as_deref(), Some("friend"));

        // **An OPEN panel is a materialised table, not a view of `COPIES`.** Without the rebuild
        // it keeps both its old text and its old ORDER — and the order is not cosmetic, `owner` is
        // the own-before-a-friend's tiebreak — until the user closes and reopens it.
        crate::plex::describe_server(house, "Mac mini", "admin", false);
        restamp_owners();
        reset(house, "4");
        install(
            house,
            "4",
            vec![
                copy(0, "Movies", "admin", "4", "1080"),
                copy(1, "Film Club", "friend", "318", "1080"),
            ],
        );
        open(crate::ui::Rect::new(0.0, 0.0, 100.0, 40.0));
        // read the MATERIALISED table, not `rows(copies())` — the pure path would pass without the
        // rebuild and prove nothing about what is on screen
        let drawn = || {
            table().sections[0]
                .rows
                .iter()
                .map(|r| r.detail.clone())
                .collect::<Vec<_>>()
        };
        // no detail page is mounted in a host test, so no row wears the tick and the ordering is
        // decided by the two keys under it — equal quality, then OWNER, then library name. Both
        // copies are credited here, so it falls through to the name: "Film Club" before "Movies".
        assert_eq!(drawn(), ["friend", "admin"]);

        crate::plex::describe_server(house, "Mac mini", "", false);
        restamp_owners();
        assert_eq!(
            table().n_rows(),
            2,
            "the open panel is rebuilt, not emptied or duplicated"
        );
        // The text AND the order, which is the whole reason the table has to be rebuilt rather than
        // patched: with nobody credited for the house's copy, `owner.is_some()` breaks the tie one
        // key EARLIER than the library name and the two rows swap.
        assert_eq!(
            drawn(),
            [OWN_ACCOUNT, "friend"],
            "an OPEN panel follows the correction; it is a snapshot, not a view of COPIES"
        );

        // **And a DISMISSED but still-drawing panel too.** `close` runs the appear choreography
        // backwards: `is_open()` goes false at once while `visible()` stays true and `draw` keeps
        // rendering the table for the length of the fade. Gating the rebuild on `is_open` left that
        // window naming the wrong person — small, and exactly the kind that ships.
        crate::plex::describe_server(house, "Mac mini", "admin", false);
        restamp_owners();
        assert_eq!(drawn(), ["friend", "admin"]);
        close();
        assert!(
            !is_open() && pop().visible(),
            "dismissed, and still on screen for the fade"
        );
        crate::plex::describe_server(house, "Mac mini", "", false);
        restamp_owners();
        assert_eq!(
            drawn(),
            [OWN_ACCOUNT, "friend"],
            "the fade draws the corrected rows, not the ones it was dismissed with"
        );
        hide();
    }

    /// **A resolve dispatched BEFORE the correction, landing AFTER it.** The worker reads the
    /// credit off the registry on a background thread; by the time its list reaches the main
    /// thread the roster refresh can have re-graded that credit AND `pump_alt_sources` can have
    /// consumed the facts epoch it moved. Nothing downstream would ever look again, so the stale
    /// stamp would have outlived every correction — which is why [`install`] regrades rather than
    /// trusting what the worker carried.
    #[test]
    fn a_resolve_that_landed_after_the_correction_is_regraded_on_the_way_in() {
        struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Fresh {
            fn drop(&mut self) {
                reset(ServerId::UNSET, "");
                crate::plex::reset_servers_for_test();
            }
        }
        let _g = Fresh(crate::testlock::serial());
        crate::plex::reset_servers_for_test();
        let house = crate::plex::register_for_test("alt-late-house", "127.0.0.1", 1, "t", "cid");
        let friend = crate::plex::register_for_test("alt-late-friend", "127.0.0.1", 2, "t", "cid");
        crate::plex::describe_server(house, "Mac mini", "admin", false);
        crate::plex::describe_server(friend, "nas-home", "friend", false);

        // the worker's list, stamped while the old credit was still published
        let in_flight = vec![
            copy(0, "Movies", "admin", "4", "1080"),
            copy(1, "Film Club", "friend", "318", "1080"),
        ];

        // …then the correction lands, and the epoch that saw it is already spent
        crate::plex::describe_server(house, "Mac mini", "", false);
        restamp_owners();

        // …and only now does the resolve arrive
        reset(house, "4");
        install(house, "4", in_flight);

        assert_eq!(
            copies()[0].owner,
            None,
            "the list is graded against the registry as it is NOW, not as the worker found it"
        );
        assert_eq!(copies()[1].owner.as_deref(), Some("friend"));
    }

    #[test]
    fn a_landing_for_another_servers_copy_with_the_same_key_is_refused() {
        struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Fresh {
            fn drop(&mut self) {
                reset(ServerId::UNSET, "");
            }
        }
        let _g = Fresh(crate::testlock::serial());
        let two_sources = || {
            vec![
                copy(0, "Movies", "", "4", "1080"),
                copy(1, "Film Club", "friend", "318", "4k"),
            ]
        };

        // OUR film 4 is the page, and its resolve is out
        reset(sid(0), "4");
        assert!(!is_available(), "a fresh mount holds no copies");
        // …and while it is out the user opens the SHARE's film 4 — the same key, another machine
        reset(sid(1), "4");
        install(sid(0), "4", two_sources());
        assert!(
            !is_available(),
            "our copies are not news about the share's film"
        );

        // the control: the very same landing DOES install when the page is still the one that
        // asked, so the refusal above is about the SERVER and not about the mechanism
        reset(sid(0), "4");
        install(sid(0), "4", two_sources());
        assert!(is_available(), "the awaited landing installs");

        // and the pre-existing rule is untouched: the same server, a different item
        reset(sid(0), "4");
        install(sid(0), "318", two_sources());
        assert!(
            !is_available(),
            "a landing for another item on this server is still refused"
        );

        // a closed page (UNSET, empty rk) is a mailbox nothing can reach
        reset(ServerId::UNSET, "");
        install(sid(0), "4", two_sources());
        assert!(!is_available(), "nothing lands on a page that is gone");
    }

    /// The panel hangs off the control that opened it, and is never over it or off the screen.
    #[test]
    fn the_panel_hangs_off_its_button_and_stays_on_screen() {
        let btn = Rect::new(crate::ui::consts::MARGIN_X, 300.0, 300.0, 60.0);
        let r = panel_at(btn, 224.0);
        assert_eq!(r.w, PANEL_W);
        assert_eq!(
            r.y,
            btn.y + btn.h + BTN_GAP,
            "under the button when there is room"
        );
        assert_eq!(r.x, btn.x, "and aligned to its left edge");

        // a button low on the page flips the panel ABOVE it rather than off the bottom
        let low = Rect::new(crate::ui::consts::MARGIN_X, 900.0, 300.0, 60.0);
        let r = panel_at(low, 224.0);
        assert!(
            r.y + r.h <= low.y - BTN_GAP + 0.01,
            "flipped above the button"
        );
        assert!(r.y >= EDGE);

        // a button near the right edge pulls the panel back inside the keep-out
        let right = Rect::new(SCR_W - 200.0, 300.0, 180.0, 60.0);
        let r = panel_at(right, 224.0);
        assert!(
            r.x + r.w <= SCR_W - EDGE_X + 0.01,
            "a panel must not run off the panel"
        );
        // …and a list taller than the screen is clamped rather than drawn past both edges
        let tall = panel_at(btn, 4000.0);
        assert!(tall.y >= EDGE && tall.y + tall.h <= SCR_H - EDGE + 0.01);

        // Every one of those worst cases is inside the overscan frame — the keep-out is per AXIS
        // precisely so that the horizontal clamp is `MARGIN_X` and not the `space::XL` the vertical
        // one uses. `ui::consts::SAFE` is the frame; this is that predicate on this panel's own
        // extremes, since a panel placed against an ANCHOR has no fixed rect a table could carry.
        for (what, p) in [
            ("under", r),
            ("tall", tall),
            ("right-edge", panel_at(right, 224.0)),
        ] {
            assert!(
                crate::ui::consts::inside_safe(p),
                "the {what} panel leaves the safe area: ({}, {}) {}x{}",
                p.x,
                p.y,
                p.w,
                p.h
            );
        }
    }

    /// **A page teardown hides the menu at ONCE; only an interactive exit fades.** Codex review,
    /// 2026-09-02: `reset` called the fading `close`, so a mount that followed a navigation from
    /// this menu found `dismiss` a no-op (already not open), left `visible()` true, and drew the
    /// previous server's rows over the incoming detail page until the spring ran out.
    #[test]
    fn a_reset_hides_the_menu_at_once_while_back_fades_it() {
        let _g = crate::testlock::serial();
        pop().open();
        assert!(pop().visible());
        reset(ServerId::UNSET, "");
        assert!(!pop().visible(), "teardown: gone on the frame it is called");
        assert!(!is_open());

        pop().open();
        close();
        assert!(!is_open(), "BACK: input ends on the press frame");
        assert!(pop().visible(), "…but the sheet is still fading");
        hide();
        assert!(!pop().visible());
    }
}
