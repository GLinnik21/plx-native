//! The Library browse screen — a full-section poster wall with server-driven sort/filter.
//!
//! Layout (per the approved mock): the shared top bar (profile chip leading at the margin +
//! CENTERED tab pills Home | \<sections\>, the tvOS tab-bar idiom — [`draw_tab_row`] is shared
//! with Home), a toolbar of two menu chips (Sort · X / Filter · X, the latter naming every active filter) + item
//! count), and a 6-across vertical grid of [`card_row`] leaf tiles fed by the `browse` paged
//! store. Menus are Popover + TableView (the track-menu components), their contents entirely
//! server-driven (`browse::sorts()` from `includeMeta=1`; genres from the value list).
//!
//! Focus model: three bands — Tabs, Toolbar, Grid — plus a modal menu state. BACK walks
//! grid/toolbar → tab bar → (app.rs) Home: the Netflix-2025 escape rule, so the nav is always
//! one press away at any scroll depth. Focus/scroll per section persist in the browse store.
//!
//! Reload choreography: every path that REPLACES the item set (tab, sort, unwatched, genre, and a
//! profile switch wiping the store from underneath) is deferred through [`Xfade`] — fade the
//! outgoing wall out, swap at the floor, fade the new one in — because `browse::requery` empties
//! the store synchronously on the key press, so there is nothing left to fade unless the animation
//! SCHEDULES the swap. The queued action is the typed [`Pending`]; the top chrome reads it through
//! the `view_*` resolvers so a chip relabels on the press frame while the grid is still dissolving.
//!
//! Perf discipline (32-bit A53 + Mali): the grid steps TWO scale springs total (focused +
//! shrinking previous), culls rows with `on_axis`, and only visible cells call `resolve_tex`
//! (the 64-slot poster LRU must never see off-screen requests).
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use crate::ui::widgets::{Art, Spinner, StatusKind};
use crate::ui::xfade::Xfade;
use crate::ui::{on_axis, Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry -------------------------------------------------------------------------------
const COLS: usize = 6;
/// 6×250 + 5×LGAP fills the 1740px between the margins exactly.
const LGAP: f32 = (SCR_W - 2.0 * MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);
const TOOL_Y: f32 = 134.0;
const TOOL_H: f32 = 52.0;
const GRID_TOP: f32 = 214.0;
/// Top-chrome scrim fade, two piecewise-linear segments ≈ ease-out: a steep drop to
/// [`SCRIM_KNEE_A`] by [`SCRIM_KNEE`], then a long gentle tail to 0 at `GRID_TOP`. One short
/// linear ramp here read as a harsh bar shadow with a visible bottom edge; the knee keeps the
/// toolbar chips (bottom y≈186) solidly backed while the tail crosses the RESTING popped card's
/// top (`GRID_TOP - CARD_H*0.045 ≈ 197`) at ≤~26% — a soft entering-the-shadow veil, not a wash.
const SCRIM_KNEE: f32 = 188.0;
const SCRIM_KNEE_A: f32 = 0.40;
/// The scrim FADES IN over the first px of scroll (alpha = `sc / SCRIM_IN`, clamped) instead of
/// popping on at a 1px threshold. Scroll is spring-driven, so the appear inherits the spring's
/// easing — scroll-linked, stateless, cannot desync. Fully established by 56px, before a
/// scrolling poster meaningfully overlaps the chip row (tops reach the chips at ~28px, mid-sweep,
/// where motion masks the 50% backing for the 2-3 frames it lasts).
const SCRIM_IN: f32 = 56.0;
/// Row pitch: card + the focused under-label band (title + caption) before the next row.
const PITCH: f32 = CARD_H + 96.0;
use crate::ui::widgets::TOP_BAR_Y; // the shared top-bar chrome lives in widgets

// ---- screen state (main-thread statics, same discipline as home.rs) -------------------------
#[derive(Clone, Copy, PartialEq, Eq)]
enum Area {
    Tabs,
    Toolbar,
    Grid,
    /// The A–Z letter rail on the right edge (RIGHT from the grid's last column). Jump, never
    /// filter: picking a letter moves focus/scroll to its first title (Emby semantics). Only
    /// reachable while [`rail_on`] — the unfiltered ascending-title listing.
    Rail,
    /// The failure read-out's **Try again**, which stands where the grid would be. Exactly one
    /// control, so it is a band rather than a cursor; [`sync_readout_focus`] owns when it is
    /// entered and left, because the failure can arrive (and clear) long after [`enter`].
    Status,
}

/// What the Library's CONTENT REGION shows in place of a grid — the projection the screen draws
/// from, and the whole read-out decision in one pure function ([`readout_of`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Readout {
    /// nothing: the grid speaks for itself (including a grid that is merely missing a later page)
    Grid,
    /// the first answer for this query is still out — the spinner
    Loading,
    /// this source did not answer and there is nothing to show for it: verdict, reason, Try again
    Failed,
    /// the source answered, and the answer is nothing
    Empty,
}

/// The read-out, from BOTH fetch states and the store. Pure, because it is the only part of this
/// screen a host can grade and because three of its four answers were previously spread across
/// three `else if`s in the middle of the draw.
///
/// **The table outranks the section, and both of its terminal answers do.** With no table there is
/// no section to page, so `fetch` is the `unwrap_or(Loading)` of a state that does not exist and
/// `total` is its `unwrap_or(-1)` — i.e. the spinner, from two defaults, which is the eternal spin
/// this whole change is about. It is not enough to catch the FAILED table: a server that answered
/// with nothing we browse (a music-only account) is `Ready` with `n_sections == 0` and lands in the
/// same two defaults one case over. That one is `Empty` — it answered — and the read-out says so.
///
/// Two per-section rules are the ones that keep being got wrong. A `Failed` fetch on a section that
/// already HAS items is `Grid`: a mid-scroll page failure must not blank a working screen — the
/// items are still there, the back-off is already counting, and `browse::pump`'s failure branch
/// exists precisely so the store survives. And a `Ready` empty listing is `Empty`, never `Failed`:
/// an empty library is an answer (`StatusKind::Empty`'s rule, stated in `browse::SecFetch` too).
fn readout_of(table: crate::browse::SecFetch, n_sections: usize, fetch: crate::browse::SecFetch, total: i64) -> Readout {
    use crate::browse::SecFetch;
    match table {
        SecFetch::Failed => return Readout::Failed,
        SecFetch::Ready if n_sections == 0 => return Readout::Empty,
        _ => {}
    }
    match fetch {
        SecFetch::Loading => Readout::Loading,
        SecFetch::Failed if total < 0 => Readout::Failed,
        SecFetch::Failed => Readout::Grid,
        SecFetch::Ready if total == 0 => Readout::Empty,
        SecFetch::Ready => Readout::Grid,
    }
}

/// [`readout_of`] against the live store — or, on an automated boot, against [`FailTest`].
fn readout() -> Readout {
    match fail_test() {
        Some(FailTest::Dead) | Some(FailTest::Own) | Some(FailTest::Table) => return Readout::Failed,
        Some(FailTest::Empty) => return Readout::Empty,
        None => {}
    }
    readout_of(
        crate::browse::sections_state(),
        crate::browse::section_count(),
        crate::browse::fetch_state(),
        crate::browse::total(),
    )
}

/// `/tmp/plxnative-libfail` — force one read-out state, for the device pass.
///
/// This screen's failure is the one state in the app that **cannot be reached on purpose**: it
/// needs a server that refuses to answer. The honest way is `tools/netcond.py` scoped to the page
/// request, and that stays the real test — but it is three moving parts (a proxy, a recompiled
/// `PMS_PORT`, and a macOS firewall prompt that silently swallows the TV's connections until it is
/// allowed once), and every one of them fails looking exactly like "the app is fine". This makes
/// the READ-OUT reachable with a file, so a capture grades the drawing rather than the plumbing.
///
/// It forces the projection only. Nothing downstream is faked: `browse` keeps whatever state it
/// really has, Try again still re-kicks a real fetch, and the moment the file is gone the screen is
/// back on [`readout_of`]. Behind `devtriggers` like every other trigger, so it does not exist in a
/// `RELEASE=1` build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailTest {
    /// (empty file, or `dead`) a BORROWED source that did not answer — the design's case, with a
    /// stand-in owner so the reason line is drawn. It is the one variant that puts words on screen
    /// the product cannot yet produce, because nothing maps a section to a source yet; that is the
    /// point of it, and `dead_strs` is the single place it applies.
    Dead,
    /// `own` — the same failure on OUR OWN server: verdict and action, no reason line, which is
    /// what every install actually draws today.
    Own,
    /// `table` — the failure one layer up, a section list that could not be fetched.
    Table,
    /// `empty` — reachable and empty: a statement, no Retry, no danger tint.
    Empty,
}

fn fail_test() -> Option<FailTest> {
    // Read ONCE. `readout()` is asked several times a frame, and a `stat` per call on a path in the
    // shared `/tmp` is not something to put in a draw loop.
    static ONCE: std::sync::OnceLock<Option<FailTest>> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        crate::dev::read("libfail").map(|v| match v.as_str() {
            "own" => FailTest::Own,
            "table" => FailTest::Table,
            "empty" => FailTest::Empty,
            _ => FailTest::Dead,
        })
    })
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    None,
    Sort,
    Filter,
    Genre,
}
/// What an OK / click means for app.rs (everything else is consumed internally).
pub(crate) enum Action {
    None,
    /// The Home tab was picked — route back to Home.
    GoHome,
    /// A grid card was activated — open its detail page (app.rs owns routing).
    Card,
}

static mut AREA: Area = Area::Grid;
static mut TAB_F: usize = 1; // focused pill while AREA==Tabs (0 = Home)
static mut TOOL_F: usize = 0; // focused toolbar chip (0 sort / 1 filter — both open a menu)
static mut GR: usize = 0; // grid focus row
static mut GC: usize = 0; // grid focus col
static mut SCROLL: Spring = Spring::at(0.0);
static mut FOCUS_S: Spring = Spring::at(1.0); // focused cell pop
static mut PREV_S: Spring = Spring::at(1.0); // previously-focused cell shrinking back
static mut PREV_IDX: i64 = -1;
static mut MENU: Menu = Menu::None;
static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
/// Source-row count the OPEN menu was built from (only one menu is ever open; 0 = "Loading…").
/// update() rebuilds the menu in place when the live server list outgrows this.
static mut MENU_BUILT: usize = 0;
static mut PHASE_MS: f32 = 0.0; // spinner phase accumulator
/// Sort and Filter. Both open a menu, and both act on the GRID — which is why the failure read-out
/// draws neither (see [`toolbar_n`], the one place that count is decided).
const N_TOOL_CHIPS: usize = 2;
static mut TOOL_RECTS: [Rect; N_TOOL_CHIPS] = [Rect::new(0.0, 0.0, 0.0, 0.0); N_TOOL_CHIPS];
/// The failure read-out's Try again pill, recorded at draw for the pointer (the `TOOL_RECTS` /
/// `RAIL_RECTS` idiom). Parked OFF the panel rather than at the origin, because `Rect::contains`
/// is inclusive and a zero-size rect at (0,0) would "contain" a click at exactly (0,0).
static mut STATUS_BTN: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);

// ---- the grid's content cross-fade + its deferred reload -------------------------------------
/// A grid reload the user has asked for but which has NOT been applied to the store yet: the
/// fade-out is running and [`Xfade::tick`] will hand it back on the floor frame. Typed and `Copy`
/// rather than a boxed closure — one value, zero alloc, and the newest request simply overwrites
/// the one before it (a fast double tab-switch must commit ONCE, to the last section pressed;
/// mixing two DIFFERENT switches inside one 70 ms fade drops the earlier one, which is the honest
/// reading of "the user changed their mind mid-press").
///
/// Every variant that can be pressed twice while its menu stays open carries its TARGET, not a
/// verb: [`Pending::Unwatched`]'s two presses inside one fade must cancel out, and a bare "toggle"
/// marker cannot express that — the chip would relabel on the first press and then refuse to
/// relabel back. `Sort` and `Genre` close their menu on the press, so they cannot be re-pressed
/// inside the window and stay plain picks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    None,
    /// switch to section index i
    Section(usize),
    /// pick sort entry i (re-picking the ACTIVE one toggles direction — `browse::set_sort`)
    Sort(usize),
    /// set the unwatched filter to this value
    Unwatched(bool),
    /// pick genre index (None = All Genres)
    Genre(Option<usize>),
}
static mut PENDING: Pending = Pending::None;
/// The grid's content cross-fade. The grid, its focused label, the empty line, the item count and
/// the A–Z rail all draw under its alpha; the top chrome (scrim, chip, pills, toolbar, spinner,
/// menus) does NOT — it is what persists across the swap.
static mut XF: Xfade = Xfade::new();
/// Last `browse::query_gen()` we have accounted for — the watchdog that catches a store replaced
/// by something OTHER than this screen (`browse::reset()` on a profile switch), so that is faded
/// rather than cut and the fader can never be left parked at 0 by a swap it never scheduled.
static mut EPOCH: u32 = 0;

fn xf() -> &'static mut Xfade {
    unsafe { &mut *addr_of_mut!(XF) }
}
fn pending() -> Pending {
    unsafe { addr_of!(PENDING).read() }
}

/// Queue a reload and start the fade-out. The newest request WINS — see [`Pending`].
fn request(p: Pending) {
    unsafe { PENDING = p };
    xf().reload();
}

/// Apply the queued reload. Called on exactly one frame — [`Xfade::tick`]'s commit frame — and by
/// [`flush`]. Every scroll/focus teleport ([`grid_reset`], [`restore_view`]) happens HERE, at
/// alpha 0, so the jump is never on screen.
fn apply_pending() {
    let p = pending();
    unsafe { PENDING = Pending::None };
    match p {
        Pending::None => {}
        Pending::Section(i) => apply_section(i),
        Pending::Sort(i) => {
            crate::browse::set_sort(i);
            grid_reset();
        }
        Pending::Unwatched(v) => {
            // the target, re-checked against the store: two presses inside one fade collapse to a
            // no-op, and a no-op must NOT re-query (nor reset the scroll — nothing changed)
            if crate::browse::unwatched() != v {
                crate::browse::toggle_unwatched();
                grid_reset();
            }
        }
        Pending::Genre(g) => {
            crate::browse::set_genre(g);
            grid_reset();
        }
    }
}

/// Apply a queued reload NOW, without waiting for the fade — for the paths that LEAVE the screen
/// mid-fade. Without this, a sort picked and then BACKed out of within 70 ms is silently dropped
/// while the toolbar chip — which reads the queued intent, see [`view_sort_label`] — has already
/// relabelled itself. The chrome lying about the listing is worse than the original no-fade cut.
/// Re-mounts the fader so the (now different) content dissolves in rather than cutting.
fn flush() {
    if pending() != Pending::None {
        apply_pending();
        xf().mount();
    }
}

// ---- what the CHROME shows while a reload is queued -------------------------------------------
// The grid's data swap is deferred to the fade floor, but a CONTROL must acknowledge a press on
// the frame it happens. So the pills/chips resolve through the queued [`Pending`] when there is
// one and through the committed store otherwise. There is exactly ONE pending value, so the chip,
// the pill and the menu checkmark can never disagree with each other — and none of them touches
// `browse`'s query state, so the store stays coherent for the whole fade.
//
// Do NOT "fix" the 70 ms by committing the query state early and deferring only the store wipe:
// `browse::maybe_spawn` scans the OLD listing for holes and `pump` splices the reply in at
// `st.items[start + k]`, so flipping `st.unwatched` without clearing the store opens a window in
// which a FILTERED page lands at UNFILTERED offsets.

/// The section the chrome should read as current — the queued one while a tab switch is in flight.
fn view_section() -> usize {
    match pending() {
        Pending::Section(i) => i,
        _ => crate::browse::cur(),
    }
}
/// The unwatched-filter state the Filter menu's checkmark and the Filter chip's value should show.
fn view_unwatched() -> bool {
    match pending() {
        Pending::Unwatched(v) => v,
        _ => crate::browse::unwatched(),
    }
}
/// The Sort chip's value token.
fn view_sort_label() -> &'static str {
    match pending() {
        Pending::Sort(i) => crate::browse::sorts().get(i).map(|s| s.title.as_str()).unwrap_or("Title"),
        _ => crate::browse::sort_label(),
    }
}
/// The Filter chip's value token.
fn view_filter_label() -> &'static str {
    match pending() {
        Pending::Genre(None) => "All",
        Pending::Genre(Some(i)) => crate::browse::genres().get(i).map(|g| g.title.as_str()).unwrap_or("All"),
        _ => crate::browse::filter_label(),
    }
}

/// What the **Filter chip** reads — every active filter, not just the genre: `All`, `Comedy`,
/// `Unwatched`, or `Comedy · Unwatched`.
///
/// It folds both because the Filter MENU now owns both switches. The unwatched filter used to have
/// its own capsule on this toolbar, so the chip beside it only ever had to name the genre; with that
/// capsule gone the chip is the only thing on screen that can say the grid is filtered at all, and a
/// filter with no visible sign of itself is a library that has silently lost half its titles.
/// Middle dots separate the active ones, the same way every metadata run in the app does.
fn view_filter_value() -> String {
    let genre = view_filter_label();
    match (genre, view_unwatched()) {
        ("All", false) => "All".to_string(),
        ("All", true) => "Unwatched".to_string(),
        (g, false) => g.to_string(),
        (g, true) => format!("{g} \u{b7} Unwatched"),
    }
}
// ---- per-frame draw caches (rebuilt only when their source state changes; building CStrings
// + re-measuring text every frame was a review-confirmed waste — same rationale as TAB_CACHE) --
struct ChipStrs {
    name: CString,
    val: CString,
    w: f32,
}
static mut CHIP_CACHE: Option<(String, String, usize, [ChipStrs; 2])> = None;
static mut RAIL_LABELS: Option<(u32, usize, Vec<CString>)> = None; // (sections_gen, section, labels)
static mut COUNT_C: Option<(i64, bool, CString)> = None;
/// The failure read-out's two runtime lines, cached per section TABLE — a profile or server switch
/// calls `browse::reset()`, which bumps `sections_gen`, and that is precisely when the source
/// behind this screen can change. Rebuilt at most once per switch, never per frame; the verdict
/// reads a persisted session file to name the machine.
static mut DEAD_C: Option<(u32, CString, Option<CString>)> = None;
/// The Empty read-out's caption, cached per (section table, section, filtered) — the three inputs
/// that can change what it says.
static mut EMPTY_C: Option<(u32, usize, bool, CString)> = None;
// letter rail: focused letter + the per-letter rects recorded at draw (pointer hit-testing)
const MAX_LETTERS: usize = 34; // A–Z + digits/# buckets
static mut RAIL_F: usize = 0;
static mut RAIL_RECTS: [Rect; MAX_LETTERS] = [Rect::new(0.0, 0.0, 0.0, 0.0); MAX_LETTERS];
static mut RAIL_N: usize = 0;

// ---- the section read-out: what it says, where it sits, what its one control does ------------
//
// `Shared Sources.dc.html` deliverable D. A borrowed server that does not answer is stated in two
// places, neither of them Home — the Sources list, and here. The rules that are easy to lose:
//
//   * The failure owns the GRID, not the screen. It is anchored in the content region and the
//     chrome above it — profile chip, tab strip, the Source chip — is untouched and still live.
//     That difference is the whole message: a section failed, the app did not.
//   * The verdict names the MACHINE ("Can't reach <machine>") and the reason names the PERSON
//     ("Shared by <handle> · your own server is fine."). This is the one place outside the Sources
//     list that speaks a machine name, because the machine is what failed; and the reason does two
//     jobs at once — whose it is, and that nothing else is down.
//   * Those are the only two identifying strings this screen renders, and the list is CLOSED. No
//     address, no path, no URL, no machineIdentifier, no token — `ui::stats`' redaction rule, and
//     for its reason: a read-out is something a user PHOTOGRAPHS for a bug report, and on a
//     borrowed source the machine is not even theirs to publish.
//   * NO alert glyph (that mark is the player's), no BACK hint (BACK is universal, and the chip
//     and tabs above are the visible way out), no sort/filter chips, no count, no rail — all four
//     of those act on a grid that has no items.

/// The band the grid occupies — what the read-out is ABOUT, and where it is anchored.
///
/// Deliberately NOT `Rect::FULL`, which is the read-out's canonical position everywhere else: a
/// block centred on the panel says the APP failed. Centring it on the content region says this
/// section did, and leaves the live chrome above it saying so.
fn content_region() -> Rect {
    Rect::new(MARGIN_X, GRID_TOP, SCR_W - 2.0 * MARGIN_X, SCR_H - GRID_TOP - theme::space::LG)
}

/// How many toolbar chips are DRAWN — the ONE predicate behind the draw, the pointer hit test and
/// the vertical walk, so a chip can never be invisible and clickable, or drawn and unreachable.
///
/// Zero while the failure read-out is up: Sort and Filter act on a grid with no items. The
/// exception is an OPEN menu, which is anchored under its own chip and would otherwise be a panel
/// hanging off nothing — a listing can fail while its Filter menu is up, and the chip must outlive
/// the panel it opened. The walk reading this rather than a constant is also what makes the Source
/// chip (navigation, and the way back out of a dead source) reachable from the read-out the moment
/// it exists — it is drawn in this state, so UP will find it with no change here.
///
/// The one cost, recorded so it is not rediscovered as a surprise: a query the SERVER rejects
/// outright (a sort key it 400s on) fails the same way a dead server does, and the chips that could
/// have changed it are not drawn — so *Try again* re-runs the same losing query, and the way out is
/// another section or Home rather than an un-filter. Drawing the chips over the failure is in the
/// design's rejected column ("they act on a grid that has no items") and clearing the user's filter
/// behind their back is worse than either, so this stands.
fn toolbar_n() -> usize {
    if readout() == Readout::Failed && !menu_open() {
        0
    } else {
        N_TOOL_CHIPS
    }
}

/// The failure read-out's verdict + reason, built once per section table (see [`DEAD_C`]).
///
/// The owner is `None` on every install today, which is the honest state of the tree rather than an
/// omission: nothing has shared a server with us yet, because nothing yet maps a section to the
/// source it came from. That mapping is `plex::servers`' registry plus the account roster's
/// `sourceTitle`, and THIS is the one seam it has to reach — exactly the shape `HubRow::source`
/// took for the shelf heading. With no owner the reason line is ABSENT, not empty: our own server
/// failing is a complete statement on its own, and there is nobody to name.
fn dead_strs() -> &'static (u32, CString, Option<CString>) {
    let gen = crate::browse::sections_gen();
    let cache = unsafe { &mut *addr_of_mut!(DEAD_C) };
    if cache.as_ref().map(|(g, _, _)| *g != gen).unwrap_or(true) {
        let machine = source_machine();
        // `None` on every install today — see this function's doc. `FailTest::Dead` is the one
        // thing that can put a name here, and only on an automated boot, because the design's
        // three-block read-out is otherwise unphotographable.
        let owner: Option<String> = (fail_test() == Some(FailTest::Dead)).then(|| STANDIN_OWNER.to_string());
        *cache = Some((
            gen,
            CString::new(format!("Can't reach {machine}")).unwrap_or_default(),
            owner.map(|o| CString::new(format!("Shared by {o} \u{b7} your own server is fine.")).unwrap_or_default()),
        ));
    }
    cache.as_ref().unwrap()
}

/// The stand-in handle [`FailTest::Dead`] names. A DOCUMENTATION placeholder, deliberately not a
/// plausible one: it is drawn on a real screen and may be photographed, and a real person's plex.tv
/// handle is not ours to put on a television.
const STANDIN_OWNER: &str = "a-friend";

/// The name of the machine behind the current section — what the verdict blames.
///
/// The signed-in session carries the server's own `name`, which is the string a person recognises
/// and is what the server chose to call itself. When there is no session (an automated boot injects
/// a bare token) there is no name, and the fallback is a WORD rather than the address.
///
/// **The address is deliberately not a fallback**, though it is right there in the client. This
/// read-out is photographable — it is what a user sends when reporting "my friend's library
/// stopped working" — and `ui::stats` already settled the rule for anything a camera can reach: no
/// URL, no path, no address, no machineIdentifier. On a BORROWED source that reasoning is stronger
/// still, because the machine is a third party's. "Can't reach this server" says everything the
/// user can act on; an IP says one thing more, to whoever else is in the room.
fn source_machine() -> String {
    let name = crate::plex::session::peek().server.name;
    if name.is_empty() {
        return "this server".to_string();
    }
    name
}

/// The Empty read-out's caption. A library the server ANSWERED for and that holds nothing is
/// named — "No films in LDN Films" — because the name is what makes it a statement rather than a
/// shrug. A listing emptied by a FILTER is not the library being empty and must not claim to be.
fn empty_caption() -> &'static CString {
    let sec = crate::browse::cur();
    let gen = crate::browse::sections_gen();
    let filtered = crate::browse::unwatched() || crate::browse::genre_sel().is_some();
    let cache = unsafe { &mut *addr_of_mut!(EMPTY_C) };
    let stale = cache.as_ref().map(|(g, s, f, _)| *g != gen || *s != sec || *f != filtered).unwrap_or(true);
    if stale {
        let s = if crate::browse::section_count() == 0 {
            // the table itself answered with nothing we browse (a music-only account) — there is no
            // section to name, and naming one would be inventing it
            "No libraries on this server".to_string()
        } else if filtered {
            "Nothing here matches".to_string()
        } else {
            let noun = if crate::browse::section_is_show(sec) { "shows" } else { "films" };
            format!("No {noun} in {}", crate::browse::section_title(sec))
        };
        *cache = Some((gen, sec, filtered, CString::new(s).unwrap_or_default()));
    }
    &cache.as_ref().unwrap().3
}

/// The read-out's one action: re-kick THIS source, and nothing else in the app.
///
/// Two layers can have failed and the fix differs. A failed section TABLE is re-fetched here and
/// now — blocking, one small GET, the same call [`enter`] makes on every route entry, so a dead
/// server costs the connect timeout exactly as leaving and coming back does. A failed PAGE only
/// needs its back-off cleared: `browse` is already counting down to the next automatic attempt, and
/// the button's job is to skip the wait rather than to start a second fetch.
fn retry_source() {
    if crate::browse::sections_failed() {
        // a table landing brings pills, a grid and a focus band with it — that is `enter`'s whole
        // job, and it is a CUT because the chrome it lands is not on screen to be continuous with
        enter(view_section(), Arrival::Cut);
        return;
    }
    crate::browse::retry();
}

/// Keep the focus band and the read-out in agreement, every frame. The failure can arrive long
/// after [`enter`] (a first page that fails two seconds in) and can clear without a keypress (the
/// automatic retry succeeding), and the grid and the read-out can never both hold focus — one of
/// them is not on screen. Focus LANDS on Try again rather than beside it, because a server that did
/// not answer often answers a second later and that press is the reason the control exists.
fn sync_readout_focus() {
    let failed = readout() == Readout::Failed;
    unsafe {
        // the rail goes first: it is taken away by EVERY read-out, not only the failure, and a
        // focus band left on it is one that only LEFT can escape
        if AREA == Area::Rail && !rail_drawn() {
            AREA = Area::Grid;
        }
        // The bands the failure takes away: the grid and its rail always, the toolbar only when it
        // has nothing left to hold. A control that is not drawn must not hold focus — the same rule
        // that stops it being clickable.
        let undrawn = matches!(AREA, Area::Grid | Area::Rail) || (AREA == Area::Toolbar && toolbar_n() == 0);
        if failed && undrawn {
            AREA = Area::Status;
        } else if !failed && AREA == Area::Status {
            AREA = Area::Grid;
        }
    }
}

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn menu() -> Menu {
    unsafe { addr_of!(MENU).read() }
}
fn area() -> Area {
    unsafe { addr_of!(AREA).read() }
}
/// The tab pill holding remote focus, or -1. ONE source for it: the draw pass and the strip's
/// scroll step must agree on which pill to keep on screen, or the row chases a pill it isn't
/// highlighting.
fn tab_focus() -> c_int {
    if area() == Area::Tabs && !menu_open() {
        unsafe { addr_of!(TAB_F).read() as c_int }
    } else {
        -1
    }
}

/// The pill focus LANDS on when it walks up into the tab row: this section's own, clamped into the
/// pills that exist. With no section table there is only Home, and an index past the last pill is a
/// highlight on nothing — reachable now that a failed discovery keeps the user on this screen
/// instead of bailing out of [`enter`] before it had one.
fn own_pill() -> usize {
    (view_section() + 1).min(crate::ui::widgets::tab_count().saturating_sub(1))
}

/// The tab pill holding focus, as a plain index — what a route change LEAVING this screen carries
/// to Home, so the pill the user is standing on is still the pill under focus on the other side.
/// Without it the SELECTION capsule travels to Home while the FOCUS capsule snaps back to wherever
/// Home was last left, which reads as two objects rather than one bar crossing a boundary. `None`
/// whenever the tab row is not what holds focus (a menu, the grid, the toolbar, the rail).
pub(crate) fn focused_pill() -> Option<usize> {
    (tab_focus() >= 0).then(|| unsafe { addr_of!(TAB_F).read() })
}

// ---- derived grid facts ---------------------------------------------------------------------
fn total() -> usize {
    crate::browse::total().max(0) as usize
}
fn n_rows() -> usize {
    total().div_ceil(COLS)
}
fn cols_in_row(r: usize) -> usize {
    let t = total();
    if (r + 1) * COLS <= t {
        COLS
    } else {
        t.saturating_sub(r * COLS)
    }
}
fn focus_idx() -> usize {
    unsafe { addr_of!(GR).read() * COLS + addr_of!(GC).read() }
}
/// Clamp the grid focus into the live item count (a re-query / late page can shrink it).
fn clamp_focus() {
    unsafe {
        let t = total();
        if t == 0 {
            GR = 0;
            GC = 0;
            return;
        }
        if GR >= n_rows() {
            GR = n_rows() - 1;
        }
        let nc = cols_in_row(GR);
        if GC >= nc {
            GC = nc.saturating_sub(1);
        }
    }
}
/// The focused grid item (None while its page is still loading).
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    crate::browse::item(focus_idx())
}
pub(crate) fn menu_open() -> bool {
    menu() != Menu::None
}

// ---- enter / view persistence ---------------------------------------------------------------

/// How the screen is being ARRIVED at — the one thing [`enter`] cannot infer, and the only
/// difference between its two kinds of caller.
pub(crate) enum Arrival {
    /// A hard cut: the `/tmp/plxnative-library` boot, where there is nothing on screen to be
    /// continuous with, so the shared strip LANDS on this section's pill instead of sliding to it.
    Cut,
    /// The page cross-fade (`ui::nav`). The tab bar is continuous chrome with a capsule crossing it
    /// right now, and [`widgets::tab_row_reveal`](crate::ui::widgets::tab_row_reveal) JUMPS the
    /// strip scroll — a jump under a travelling capsule teleports the pills out from under it. It
    /// is also unnecessary here: `tab_row_update` has been targeting the pending pill since the
    /// press frame (`nav::view_tab`), so the strip is already springing there, and a pressed pill
    /// was visible by construction anyway (`tab_pill_at` matches CLIPPED rects, and D-pad focus
    /// scrolls a pill into view before OK can reach it).
    Faded,
}

/// Enter the Library on section `sec` (0-based). Discovers the sections on first use
/// (blocking, one small GET) and restores the section's remembered focus/scroll.
///
/// Everything here TELEPORTS something the user can see — the store swap, the restored grid scroll,
/// the focus band — so on the `Faded` arrival it is called from the page fade's floor, at alpha 0,
/// rather than on the press frame.
pub(crate) fn enter(sec: usize, arrival: Arrival) {
    flush(); // a reload queued on the way out is applied, not dropped (see [`flush`])
    let n = crate::browse::ensure_sections();
    // A table that could not be FETCHED used to return here, before a single line of screen state
    // was set — which left the focus band wherever the last visit had parked it, on a screen that
    // now has no grid and no chips. The discovery failing is a state this screen draws (the read-out
    // one layer up), so it is entered like any other: everything below runs, and only the parts
    // that index a section are skipped.
    if n > 0 {
        crate::browse::set_cur(sec.min(n - 1));
        crate::browse::kick_letters();
        restore_view();
        // with more libraries than fit the row, a section past the visible pills would open with its
        // own tab off screen (the `/tmp/plxnative-library=N` boot, or a restored far-right section).
        // Put it on screen once, here, rather than dragging the row every frame — but only on a CUT;
        // see `Arrival::Faded` for why a jump is both wrong and redundant under a travelling capsule.
        if matches!(arrival, Arrival::Cut) {
            crate::ui::widgets::tab_row_reveal(crate::browse::cur() + 1);
        }
    }
    unsafe {
        AREA = Area::Grid;
        MENU = Menu::None;
        TOOL_F = 0;
        PENDING = Pending::None; // whatever was queued before we left has just been flushed
        EPOCH = crate::browse::query_gen(); // AFTER `set_cur` above, so our own bump isn't foreign
    }
    // ...and then the read-out takes the band if it owns the content region, so an entry onto a
    // dead source opens ON Try again rather than on the frame after it
    sync_readout_focus();
    // Route entry has NO outgoing content: park at 0 and fade in once the listing exists (the very
    // next frame for a section whose store is still resident). This is also the ONE recovery for a
    // fader left mid-phase by leaving the screen — only this route steps it.
    xf().mount();
}

fn restore_view() {
    let (focus, scroll) = crate::browse::saved_view();
    unsafe {
        GR = focus / COLS;
        GC = focus % COLS;
        (*addr_of_mut!(SCROLL)).jump(scroll);
        FOCUS_S = Spring::at(1.0);
        PREV_IDX = -1;
    }
    clamp_focus();
}

/// Reset the grid to the top — every re-query (sort/filter change) starts a fresh listing.
/// Deliberately does NOT touch the focus band: toggling the unwatched filter must leave focus
/// on that chip, not warp it into the grid.
fn grid_reset() {
    unsafe {
        GR = 0;
        GC = 0;
        (*addr_of_mut!(SCROLL)).jump(0.0);
        PREV_IDX = -1;
    }
}

/// Queue a tab switch. The STORE moves on the fade floor ([`apply_section`]); the PILL moves now
/// (every chrome reader goes through [`view_section`]).
fn switch_section(i: usize) {
    if i >= crate::browse::section_count() || i == view_section() {
        return;
    }
    request(Pending::Section(i));
}

/// The committed half of [`switch_section`] — runs at alpha 0. The guard is re-checked against
/// the store because `cur()` may have moved between the request and the commit.
fn apply_section(i: usize) {
    if i >= crate::browse::section_count() || i == crate::browse::cur() {
        return;
    }
    crate::browse::set_cur(i);
    crate::browse::kick_letters();
    restore_view();
}

// ---- letter rail helpers --------------------------------------------------------------------

fn rail_on() -> bool {
    crate::browse::rail_available()
}
/// Is the A–Z rail actually on screen? The ONE answer, read by the draw, by the RIGHT key that
/// enters it and by [`sync_readout_focus`] — the `toolbar_n` rule applied to the other control
/// this screen can take away.
///
/// It INDEXES a grid, so any read-out standing in for one takes it with it. Its letters do not:
/// they are query-independent and stay resident from the listing that worked before this one
/// failed, so `rail_on` alone would happily draw a rail into an empty region — and, worse, let
/// RIGHT park focus on it, since with no items `cols_in_row` is 0 and RIGHT always falls through.
fn rail_drawn() -> bool {
    rail_on() && !menu_open() && readout() == Readout::Grid
}
/// The rail letter whose range contains item `idx`.
fn letter_of(idx: usize) -> usize {
    let mut sum = 0usize;
    for (i, (_, n)) in crate::browse::letters().iter().enumerate() {
        sum += *n as usize;
        if idx < sum {
            return i;
        }
    }
    crate::browse::letters().len().saturating_sub(1)
}
/// Jump the grid focus to letter `i`'s first title (the prefix-sum index) — focus + scroll
/// move, the listing itself never changes.
fn rail_jump(i: usize) {
    let letters = crate::browse::letters();
    if letters.is_empty() {
        return;
    }
    let i = i.min(letters.len().min(MAX_LETTERS) - 1);
    unsafe { RAIL_F = i };
    let idx = crate::browse::letter_start(i);
    let old = focus_idx();
    unsafe {
        GR = idx / COLS;
        GC = idx % COLS;
    }
    clamp_focus();
    if focus_idx() != old {
        grid_focus_moved(old);
    }
}

// ---- update ---------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    unsafe { PHASE_MS += dt * 1000.0 };

    // ---- content cross-fade. FIRST, because the commit frame teleports SCROLL/GR/GC and the
    // wanted window below must be computed from the POST-swap scroll (the same reason that window
    // is computed before `pump`).
    // "There is something to show" — which is a READ-OUT as much as a grid. This used to ask
    // `!browse::loading_initial()`, i.e. "did a section answer", and a table that answered with
    // NOTHING BROWSABLE has no section to answer: `fetch_state()` falls through its `unwrap_or`
    // forever, the fader never left its hold, and the empty read-out was drawn at alpha 0 on a
    // screen that looked simply blank. `readout()` is the one question with the right shape.
    let ready = readout() != Readout::Loading;
    if xf().tick(dt, ready) {
        apply_pending();
    }
    // Watchdog: a store replaced by something other than this screen (`browse::reset()` on a
    // profile switch) still gets a fade rather than a cut. Read AFTER the commit, so our own
    // `bump_gen` is never mistaken for a foreign one.
    let g = crate::browse::query_gen();
    if g != unsafe { addr_of!(EPOCH).read() } {
        unsafe { EPOCH = g };
        if !xf().is_swapping() {
            xf().mount();
        }
    }

    unsafe {
        // wanted window BEFORE pump (its maybe_spawn reads it — after a tab switch the first
        // fetch must target THIS section's restored scroll, not the previous frame's)
        let sc = (*addr_of!(SCROLL)).pos;
        let lo_row = (((sc - CARD_H) / PITCH).floor().max(0.0)) as usize;
        let hi_row = (((sc + SCR_H - GRID_TOP) / PITCH).ceil().max(0.0)) as usize + 1;
        crate::browse::want(lo_row.saturating_sub(1) * COLS, (hi_row + 1) * COLS);
    }
    if crate::browse::pump() {
        clamp_focus();
    }
    // AFTER the pump: a landing is exactly what turns the grid into a read-out (or back), and the
    // focus band must not spend a frame on the one that is no longer drawn.
    sync_readout_focus();
    // waiting menu data re-kicks each frame (self-guarded): the single-flight fetch flags are
    // GLOBAL, so a fetch busy on another section must not permanently starve this one
    if crate::browse::letters().is_empty() {
        crate::browse::kick_letters();
    }
    if menu() == Menu::Genre && crate::browse::genres().is_empty() {
        crate::browse::kick_genres();
    }
    unsafe {

        // springs: focused pop + the one shrinking previous cell (NOT a spring per cell — a
        // paged grid has unbounded rows; two springs give the same read on the A53 budget)
        (*addr_of_mut!(FOCUS_S)).step(RowStyle::HOME.focus_scale, K_SCALE, dt);
        (*addr_of_mut!(PREV_S)).step(1.0, K_SCALE, dt);
        if (*addr_of!(PREV_S)).pos < 1.003 {
            PREV_IDX = -1;
        }

        // vertical scroll: reveal the focused row (label band included), never above its slot
        let row_top = GR as f32 * PITCH;
        let max_y = (n_rows() as f32 * PITCH - (SCR_H - GRID_TOP) + 20.0).max(0.0);
        let lo = row_top + CARD_H + 96.0 - (SCR_H - GRID_TOP - 16.0);
        let hi = row_top;
        let want = if matches!(area(), Area::Grid | Area::Rail) {
            card_row::reveal((*addr_of!(SCROLL)).pos, lo, hi, max_y) // rail jumps scroll the grid too
        } else {
            (*addr_of!(SCROLL)).pos
        };
        (*addr_of_mut!(SCROLL)).step(want, K_SCROLL, dt);

        // remember the view for re-entry (state amnesia is the official app's #2 complaint)
        crate::browse::save_view(focus_idx(), (*addr_of!(SCROLL)).pos);
    }
    // the shared tab strip's horizontal scroll — with more libraries than fit the row, this is
    // what reaches the far pills. Drawn from `.pos`; the draw pass runs at dt=0. Off the tab row
    // it tracks THIS section's pill, which `enter` already put on screen, so it holds still.
    // the VIEW section, so the strip travels to the pressed pill on the press frame while the grid
    // dissolves under it
    crate::ui::widgets::tab_row_update(view_section() as c_int + 1, tab_focus(), dt);
    if menu_open() {
        pop().update(dt);
        let ph = panel_rect().h;
        table().update(dt, ph - 40.0);
        // async menu data landing while the popover is open — rebuild it in place (a Sort
        // menu stuck on "Loading…" whose OK secretly toggled the direction was a
        // review-confirmed bug)
        let built = unsafe { addr_of!(MENU_BUILT).read() };
        match menu() {
            Menu::Sort if built != crate::browse::sorts().len() => build_sort_menu(true),
            Menu::Genre if built != crate::browse::genres().len() => build_genre_menu(true),
            _ => {}
        }
    }
}

// ---- input: D-pad ---------------------------------------------------------------------------

pub(crate) fn move_focus(sym: c_uint) {
    if menu_open() {
        if sym == SDLK_UP {
            table().move_sel(-1);
        } else if sym == SDLK_DOWN {
            table().move_sel(1);
        }
        return;
    }
    unsafe {
        match area() {
            Area::Tabs => {
                // Home + every section: the row scrolls the far pills into view, so focus may
                // walk to the last one (it used to stop at a hard cap of 4 sections)
                let n = crate::ui::widgets::tab_count();
                if sym == SDLK_LEFT && TAB_F > 0 {
                    TAB_F -= 1;
                } else if sym == SDLK_RIGHT && TAB_F + 1 < n {
                    TAB_F += 1;
                } else if sym == SDLK_DOWN {
                    // DOWN goes to the first band that is DRAWN — the toolbar normally, the failure
                    // read-out when the chips it would land on are not there
                    AREA = if toolbar_n() > 0 { Area::Toolbar } else { Area::Status };
                }
            }
            Area::Toolbar => {
                if sym == SDLK_LEFT && TOOL_F > 0 {
                    TOOL_F -= 1;
                } else if sym == SDLK_RIGHT && TOOL_F + 1 < toolbar_n() {
                    TOOL_F += 1;
                } else if sym == SDLK_UP {
                    AREA = Area::Tabs;
                    TAB_F = own_pill(); // the pill the chrome is showing, not the store's
                } else if sym == SDLK_DOWN {
                    if readout() == Readout::Failed {
                        AREA = Area::Status;
                    } else if total() > 0 {
                        AREA = Area::Grid;
                    }
                }
            }
            Area::Grid => {
                let (r, c) = (GR, GC);
                if sym == SDLK_LEFT && GC > 0 {
                    GC -= 1;
                } else if sym == SDLK_RIGHT {
                    if GC + 1 < cols_in_row(GR) {
                        GC += 1;
                    } else if rail_drawn() {
                        // RIGHT off the last column enters the letter rail at the current letter
                        AREA = Area::Rail;
                        RAIL_F = letter_of(focus_idx());
                        return;
                    }
                } else if sym == SDLK_UP {
                    if GR > 0 {
                        GR -= 1;
                    } else {
                        AREA = Area::Toolbar;
                        return;
                    }
                } else if sym == SDLK_DOWN && GR + 1 < n_rows() {
                    GR += 1;
                }
                clamp_focus();
                if (GR, GC) != (r, c) {
                    grid_focus_moved(r * COLS + c);
                }
            }
            Area::Rail => {
                let n = crate::browse::letters().len().min(MAX_LETTERS); // never beyond the drawn rects
                if sym == SDLK_UP && RAIL_F > 0 {
                    rail_jump(RAIL_F - 1); // live jump: the grid follows the letter
                } else if sym == SDLK_DOWN && RAIL_F + 1 < n {
                    rail_jump(RAIL_F + 1);
                } else if sym == SDLK_LEFT {
                    AREA = Area::Grid; // back into the grid at the jumped position
                }
            }
            Area::Status => {
                // ONE control, so there is nowhere to walk sideways or down to. UP leaves the
                // read-out for the chrome above it, which never went down — the toolbar when it has
                // a chip to hold, else the tab strip. The read-out therefore spends no line telling
                // the user how to get out: every exit is a control they can already see.
                if sym == SDLK_UP {
                    if toolbar_n() > 0 {
                        AREA = Area::Toolbar;
                        TOOL_F = 0;
                    } else {
                        AREA = Area::Tabs;
                        TAB_F = own_pill();
                    }
                }
            }
        }
    }
}

/// CH▲/CH▼ page jump: one screenful of rows per press.
pub(crate) fn page(dir: i32) {
    if menu_open() || area() != Area::Grid {
        return;
    }
    unsafe {
        let old = focus_idx();
        // a visible screen holds ~1.84 rows — round, don't truncate (a truncated "page" of 1
        // row was identical to plain UP/DOWN)
        let step = (((SCR_H - GRID_TOP) / PITCH).round().max(1.0)) as usize;
        if dir > 0 {
            GR = (GR + step).min(n_rows().saturating_sub(1));
        } else {
            GR = GR.saturating_sub(step);
        }
        clamp_focus();
        if focus_idx() != old {
            grid_focus_moved(old);
        }
    }
}

/// Hand the pop spring to the newly-focused cell; the old cell shrinks via PREV.
fn grid_focus_moved(old_idx: usize) {
    unsafe {
        PREV_IDX = old_idx as i64;
        PREV_S = addr_of!(FOCUS_S).read();
        FOCUS_S = Spring::at(1.0);
    }
}

// ---- input: OK / BACK -----------------------------------------------------------------------

pub(crate) fn focus_is_card() -> bool {
    !menu_open() && area() == Area::Grid && focused_item().is_some()
}

pub(crate) fn on_ok() -> Action {
    if menu_open() {
        menu_commit(table().sel);
        return Action::None;
    }
    match area() {
        Area::Tabs => {
            let f = unsafe { addr_of!(TAB_F).read() };
            if f == 0 {
                return Action::GoHome;
            }
            switch_section(f - 1);
            Action::None
        }
        Area::Toolbar => {
            match unsafe { addr_of!(TOOL_F).read() } {
                0 => open_sort_menu(),
                // both chips open a menu now — the unwatched switch lives inside this one
                _ => open_filter_menu(false),
            }
            Action::None
        }
        Area::Grid => Action::Card,
        Area::Rail => {
            unsafe { AREA = Area::Grid }; // OK on a letter lands in the grid at that title
            Action::None
        }
        Area::Status => {
            retry_source();
            Action::None
        }
    }
}

/// BACK inside the screen. Returns false when the press should leave to Home (tab bar was
/// already focused) — the Netflix-2025 escape rule: first BACK reaches the tab bar.
pub(crate) fn back() -> bool {
    match menu() {
        Menu::Genre => {
            open_filter_menu(true);
            return true;
        }
        Menu::Sort | Menu::Filter => {
            close_menu();
            return true;
        }
        Menu::None => {}
    }
    // No menu is being dismissed, so this BACK either walks out of the screen or up to the tab bar
    // — either way the queued reload must land now rather than be dropped by a route change (and
    // the tab-bar walk below reads `cur()`, which [`flush`] has just made agree with the chrome).
    flush();
    if area() == Area::Rail {
        unsafe { AREA = Area::Grid };
        return true;
    }
    if area() != Area::Tabs {
        unsafe {
            AREA = Area::Tabs;
            TAB_F = own_pill(); // `flush` above has made the chrome and the store agree
        }
        return true;
    }
    false
}

// ---- menus (Popover + TableView, all server-driven) -----------------------------------------

fn panel_rect() -> Rect {
    let ph = table().measured_height().clamp(120.0, SCR_H - 280.0);
    Rect::new(MARGIN_X, TOOL_Y + TOOL_H + 18.0, 470.0, ph)
}
fn close_menu() {
    unsafe { MENU = Menu::None };
    pop().close();
}

fn open_sort_menu() {
    build_sort_menu(false);
}

/// `keep` = rebuilding the already-open menu in place (the server sorts landed) — don't
/// restart the popover's appear animation.
fn build_sort_menu(keep: bool) {
    let sorts = crate::browse::sorts();
    let mut sec = Section::new("Sort by");
    if sorts.is_empty() {
        sec = sec.row(Row::new("Loading\u{2026}").dim(true));
    }
    for (i, s) in sorts.iter().enumerate() {
        let active = i == crate::browse::sort_idx();
        let mut row = Row::new(s.title.clone()).checked(active);
        if active {
            // direction as the chevron ASSET (per the icon rule), up = asc; OK again flips it
            row = row.ticon(if crate::browse::sort_desc() { Icon::ChevronDown } else { Icon::ChevronUp });
        }
        sec = sec.row(row);
    }
    unsafe { MENU_BUILT = sorts.len() };
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], crate::browse::sort_idx() as i32, keep);
    unsafe { MENU = Menu::Sort };
    if !keep {
        pop().open();
    }
}

/// `keep` = re-entered from the Genre level via BACK (slide the pill, don't restart the fade).
fn open_filter_menu(keep: bool) {
    let sec = Section::new("Filter")
        // A SWITCH, not one option among several: it says what it is set to in words at the trailing
        // edge, and carries no mark at all in the leading column.
        .row(Row::new("Unwatched only").toggle(view_unwatched()))
        .row(
            // "All" / "Comedy" is a READ-OUT — what this row is set to — so it belongs in the same
            // trailing slot as the switch above it, not on a sub-line. It was a sub-line until the
            // 2026-08-13 sync, which put the two rows of this one menu in the odd position of
            // answering "what is set" in two different places; a sub-line is for a DESCRIPTION
            // (the track menu's "EAC3 · 5.1"), and it also costs the row 32px of height it was
            // spending to say one word.
            Row::new("Genre")
                .value(crate::browse::genre_sel().map(|g| g.title.clone()).unwrap_or_else(|| "All".into()))
                .chevron(true),
        );
    let sel = if keep { 1 } else { 0 };
    unsafe { MENU_BUILT = 0 }; // filter L1 is static rows — no rebuild-on-landing path
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    unsafe { MENU = Menu::Filter };
    if !keep {
        pop().open();
    }
}

fn build_genre_menu(keep: bool) {
    crate::browse::kick_genres();
    let genres = crate::browse::genres();
    let selected = crate::browse::genre_sel().map(|g| g.id.clone());
    let mut sec = Section::new("Genre").row(Row::new("All Genres").checked(selected.is_none()));
    if genres.is_empty() {
        sec = sec.row(Row::new("Loading\u{2026}").dim(true));
    }
    let mut sel = 0i32;
    for (i, g) in genres.iter().enumerate() {
        let active = selected.as_deref() == Some(g.id.as_str());
        if active {
            sel = i as i32 + 1;
        }
        sec = sec.row(Row::new(g.title.clone()).checked(active));
    }
    unsafe { MENU_BUILT = genres.len() };
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    unsafe { MENU = Menu::Genre };
    if !keep {
        pop().open();
    }
}

fn menu_commit(sel: i32) {
    match menu() {
        Menu::Sort => {
            // gate on the row set the TABLE was built with, not the live sorts() — OK on the
            // "Loading…" row must be a no-op, never a hidden direction toggle
            let built = unsafe { addr_of!(MENU_BUILT).read() };
            if built > 0 && sel >= 0 && (sel as usize) < built {
                request(Pending::Sort(sel as usize));
            }
            close_menu(); // the panel closes NOW; the grid dissolves under it
        }
        Menu::Filter => {
            if sel == 0 {
                request(Pending::Unwatched(!view_unwatched()));
                open_filter_menu(true); // rebuilt from `view_unwatched()` — the check flips at once
                table().sel = 0;
            } else if sel == 1 {
                build_genre_menu(false);
            }
        }
        Menu::Genre => {
            let genres_n = crate::browse::genres().len();
            if sel == 0 {
                request(Pending::Genre(None));
                close_menu();
            } else if !crate::browse::genres().is_empty() && (sel as usize) <= genres_n {
                request(Pending::Genre(Some(sel as usize - 1)));
                close_menu();
            }
        }
        Menu::None => {}
    }
}

// ---- pointer --------------------------------------------------------------------------------

pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if menu_open() {
        if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
            table().sel = gi;
        }
        return;
    }
    unsafe {
        if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
            AREA = Area::Tabs;
            TAB_F = i;
            return;
        }
        let tools = addr_of!(TOOL_RECTS).read();
        if let Some(i) = tools[..toolbar_n()].iter().position(|r| r.contains(mx, my)) {
            AREA = Area::Toolbar;
            TOOL_F = i;
            return;
        }
        if status_btn_at(mx, my) {
            AREA = Area::Status;
            return;
        }
        if let Some(i) = rail_at(mx, my) {
            AREA = Area::Rail;
            RAIL_F = i; // hover focuses the letter; the click jumps
            return;
        }
        if let Some((r, c)) = cell_at(mx, my) {
            let old = focus_idx();
            AREA = Area::Grid;
            GR = r;
            GC = c;
            if focus_idx() != old {
                grid_focus_moved(old);
            }
        }
    }
}

/// The grid cell under the pointer, with home's fly-away guard: a partially-visible row is not
/// hoverable unless it is already the focused row (hovering it would scroll the page under the
/// pointer).
fn cell_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    // O(visible): only the two rows that can straddle the pointer's y, not the whole section
    let r_lo = (((sc + my - GRID_TOP - CARD_H) / PITCH).floor().max(0.0)) as usize;
    let r_hi = ((((sc + my - GRID_TOP) / PITCH).floor()).max(0.0) as usize + 1).min(n_rows());
    for r in r_lo..r_hi.min(n_rows()) {
        let y = GRID_TOP + r as f32 * PITCH - sc;
        if my < y || my > y + CARD_H {
            continue;
        }
        let fully_visible = y >= GRID_TOP - 8.0 && y + CARD_H <= SCR_H - 20.0;
        if !fully_visible && r != unsafe { addr_of!(GR).read() } {
            continue;
        }
        for c in 0..cols_in_row(r) {
            let x = MARGIN_X + c as f32 * (CARD_W + LGAP);
            if mx >= x && mx <= x + CARD_W {
                return Some((r, c));
            }
        }
    }
    None
}

/// Pointer click — same activation map as OK.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if menu_open() {
        if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
            table().sel = gi;
            menu_commit(gi);
        } else {
            close_menu();
        }
        return Action::None;
    }
    pointer_focus(mx, my);
    if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
        if i == 0 {
            return Action::GoHome;
        }
        switch_section(i - 1);
        return Action::None;
    }
    let tools = unsafe { addr_of!(TOOL_RECTS).read() };
    if tools[..toolbar_n()].iter().any(|r| r.contains(mx, my)) {
        return on_ok();
    }
    if status_btn_at(mx, my) {
        return on_ok(); // `pointer_focus` above has already parked the band on the pill
    }
    if let Some(i) = rail_at(mx, my) {
        rail_jump(i); // pointer click on a letter = the jump
        return Action::None;
    }
    if cell_at(mx, my).is_some() {
        return Action::Card;
    }
    Action::None
}

/// Is the pointer on the failure read-out's Try again pill? Rect recorded at draw and parked off
/// the panel whenever the read-out is not up, so a stale one cannot be clicked (see [`STATUS_BTN`]).
fn status_btn_at(mx: f32, my: f32) -> bool {
    unsafe { addr_of!(STATUS_BTN).read() }.contains(mx, my)
}

/// The rail letter under the pointer, or None (rects recorded at draw; empty when hidden).
fn rail_at(mx: f32, my: f32) -> Option<usize> {
    let n = unsafe { addr_of!(RAIL_N).read() };
    let rects = unsafe { addr_of!(RAIL_RECTS).read() };
    rects[..n].iter().position(|r| r.contains(mx, my))
}

pub(crate) fn wheel(dy: c_int) {
    if menu_open() {
        table().move_sel(if dy < 0 { 1 } else { -1 });
        return;
    }
    move_focus(if dy < 0 { SDLK_DOWN } else { SDLK_UP });
}

// ---- draw -----------------------------------------------------------------------------------

const CHIP_PAD: f32 = 24.0;
const CHIP_ISZ: f32 = 22.0; // trailing-icon box

/// The two toolbar chips' strings + measured widths, cached until the labels/section change.
fn chip_strs() -> &'static [ChipStrs; 2] {
    // the VIEW labels, not the committed ones: a chip must relabel on the press frame, not 70 ms
    // later. The cache key is these same three values, so it invalidates on the QUEUE and then
    // again on the commit only if the resolved label actually differs (by construction it doesn't).
    let sort = view_sort_label();
    let filt = view_filter_value();
    let sec = view_section();
    let cache = unsafe { &mut *addr_of_mut!(CHIP_CACHE) };
    let stale = cache.as_ref().map(|(s, f, c, _)| s != sort || *f != filt || *c != sec).unwrap_or(true);
    if stale {
        let build = |name: &str, value: &str| {
            let sz = theme::size::LABEL;
            let name_c = CString::new(name).unwrap_or_default();
            let val_c = CString::new(format!(" \u{b7} {value}")).unwrap_or_default();
            let nw = crate::text::text_width(name_c.as_ptr(), sz, 0);
            let vw = crate::text::text_width(val_c.as_ptr(), sz, 1);
            let w = CHIP_PAD + nw + vw + 10.0 + CHIP_ISZ + CHIP_PAD;
            ChipStrs { name: name_c, val: val_c, w }
        };
        let chips = [build("Sort", sort), build("Filter", &filt)];
        *cache = Some((sort.to_string(), filt, sec, chips));
    }
    &cache.as_ref().unwrap().3
}

/// One toolbar chip from its cached strings. Hierarchy per the design review: the VALUE
/// ("Title"/"All") is the information-bearing token — bright + bold; the static name prefix is
/// dim + regular. Every chip on this toolbar OPENS A MENU, which is why the trailing glyph is
/// always a chevron: the third chip — a value-less capsule that WAS its own toggle, wearing an
/// amber disc when on — is gone (2026-08-13), because the Filter menu it sat beside already
/// carries "Unwatched only" as a checkable row, and one state deserves one control.
fn toolbar_chip(p: Painter, x: f32, cs: &ChipStrs, icon: Icon, focused: bool) -> Rect {
    let sz = theme::size::LABEL;
    let r = Rect::new(x, TOOL_Y, cs.w, TOOL_H);
    let (bg, bright_ink, dim_ink) = if focused {
        (crate::ui::ACCENT, crate::ui::ACCENT_INK, theme::with_a(crate::ui::ACCENT_INK, 0.62))
    } else {
        (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK, theme::TEXT_SECONDARY)
    };
    p.rrect(r, TOOL_H * 0.5, TOOL_H * 0.5, bg);
    let ty = crate::text::text_vcenter_y(sz, 1, r.y + r.h * 0.5);
    let mut tx = r.x + CHIP_PAD;
    tx += p.text(cs.name.as_ptr(), tx, ty, sz, dim_ink, 0, 0);
    tx += p.text(cs.val.as_ptr(), tx, ty, sz, bright_ink, 0, 1);
    // trailing icon — an SVG asset, never a font glyph
    let ir = Rect::new(tx + 10.0, r.y + (r.h - CHIP_ISZ) * 0.5, CHIP_ISZ, CHIP_ISZ);
    let tint = if focused { crate::ui::ACCENT_INK } else { theme::TEXT_SECONDARY };
    crate::ui::icons::draw(p, icon, ir, tint);
    r
}

pub(crate) fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    // TWO nested fades, and the nesting is the whole design.
    //
    // `p` is the PAGE — everything this screen owns that Home does not: the grid, the top scrim,
    // the toolbar chips, the count, the rail, the menus. It dips to the app ground and back when
    // the ROUTE changes (`ui::nav`), which is what makes Home↔Library a transition rather than a
    // cut. `pk` is the CONTINUOUS CHROME — the profile chip and the shared tab row, the same
    // control Home draws — which holds still while the pages swap under it.
    //
    // `pg` adds the grid's OWN content cross-fade on top of `p`, multiplicatively and for free.
    // Everything that is a FUNCTION OF THE LISTING draws through it: the tiles, the focused tile's
    // label block, the empty line, the item count and the A–Z rail (which indexes the grid and is
    // meaningless without it). Everything that PERSISTS ACROSS a re-query stays on `p`: the
    // top-chrome scrim, the three toolbar chips, the loading spinner and the menus.
    // `Painter::alpha` is multiplicative and folded into every primitive by `Painter::c` —
    // including `tex_carded`'s tint and its folded drop shadow, `sheen_rim`, `Painter::text` and
    // `icons::draw` — so no per-tile work is needed and nothing can escape either fade.
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let pk = Painter::root().alpha(crate::ui::nav::chrome_alpha());
    let pg = p.alpha(xf().alpha());
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    let focused_i = focus_idx();
    // the focused card draws LAST (pass 2) while the grid OR the rail owns focus — a rail
    // jump keeps the landed card visibly anchored
    let focused_pass = matches!(area(), Area::Grid | Area::Rail) && !menu_open();

    // ---- the content region: a grid, or the ONE read-out that stands in for it ----------------
    // The spinner and the failure draw on `p`, not `pg`: a status must stay legible while the
    // content band is dark (`Xfade`'s rule — never fade a read-out with the content it replaces).
    // The empty line rides `pg` because it IS a function of the listing, and dissolves with it.
    let t = total();
    let read = readout();
    let mut status_btn = Rect::new(-1.0, -1.0, 0.0, 0.0);
    match read {
        Readout::Loading => {
            Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
                .phase(unsafe { addr_of!(PHASE_MS).read() } as u32)
                .draw(&Env::inert(), p);
        }
        Readout::Failed => {
            // Anchored in the CONTENT REGION, not on the panel: the chrome above is still live, and
            // that is the difference between a section failing and the app failing. No spinner
            // (`browse` retries underneath, and a spinner for a source that isn't coming would hold
            // the panel presenting — `ui::idle`), no alert glyph, one action.
            let (_, verdict, reason) = dead_strs();
            let mut ov = crate::ui::widgets::StatusOverlay::new(content_region(), verdict, StatusKind::Failed)
                .action(c"Try again")
                .focused(area() == Area::Status && !menu_open());
            if let Some(r) = reason {
                ov = ov.reason(r);
            }
            ov.draw(&Env::inert(), p);
            status_btn = ov.action_frame().unwrap_or(status_btn);
        }
        Readout::Empty => {
            // NOT `Failed`, and so no danger tint and no Retry: the server answered, and its answer
            // was nothing. There is nothing here to try again.
            crate::ui::widgets::StatusOverlay::new(content_region(), empty_caption(), StatusKind::Empty)
                .draw(&Env::inert(), pg);
        }
        Readout::Grid => {}
    }
    unsafe { *addr_of_mut!(STATUS_BTN) = status_btn };
    // visible row RANGE by arithmetic, not an O(totalSize) walk (a 10k-item section must not
    // cost 1.7k iterations per frame); rows fully under the opaque top-chrome scrim are culled
    // too — drawing the full card composite beneath it was pure fill-rate waste.
    // Deliberately NO `if pg.alpha > 0` early-out around these loops: at the floor they already
    // cost nothing (`n_rows()` is 0 while `total < 0`), and skipping them would stop `resolve_tex`
    // being called for the incoming section's visible tiles until the fade-IN starts — delaying
    // every poster fetch by the length of the fade. The 64-slot LRU must see the on-screen
    // requests exactly as early as it does today.
    //
    // The read-out STANDS IN PLACE OF the grid, so a state that draws one draws no tiles. In the
    // product that is already true by arithmetic — every non-`Grid` read-out has `total <= 0` and
    // `n_rows()` is 0 — but it is true by ARITHMETIC, not by construction, which is exactly the
    // kind of agreement that stops holding the moment something else decides the state (the
    // `libfail` boot). It costs no poster fetch: there are no tiles to resolve in those states.
    let r_lo = (((sc - CARD_H - GLOW_PAD) / PITCH).floor().max(0.0)) as usize; // loose; per-row cull is exact
    let r_hi = ((((sc + SCR_H - GRID_TOP) / PITCH).ceil()).max(0.0) as usize + 1).min(n_rows());
    for r in r_lo..r_hi {
        if read != Readout::Grid {
            break;
        }
        let y = GRID_TOP + r as f32 * PITCH - sc;
        if !on_axis(y, CARD_H, SCR_H, GLOW_PAD) || y + CARD_H < 150.0 {
            continue;
        }
        for c in 0..cols_in_row(r) {
            let idx = r * COLS + c;
            if focused_pass && idx == focused_i {
                continue; // drawn last (z-order over neighbours)
            }
            let m = crate::browse::item(idx);
            let x = MARGIN_X + c as f32 * (CARD_W + LGAP);
            let s = if idx as i64 == unsafe { addr_of!(PREV_IDX).read() } {
                unsafe { addr_of!(PREV_S).read() }.pos
            } else {
                1.0
            };
            let rect = Rect::new(x, y, CARD_W, CARD_H).scaled(s);
            let resume = m.and_then(PmsMovie::resume_frac);
            card_row::draw_tile(pg, Art::Poster(m), rect, s, &RowStyle::HOME, resume);
        }
    }

    // ---- grid: pass 2 — the focused card + under-label, over its NEIGHBOURS but UNDER the top
    // chrome: the toolbar/pills win, so a focused card scrolling through the top band slides
    // beneath the Sort/Filter chips instead of popping over them (Home's convention; this used to
    // draw after the chrome and inverted it).
    if focused_pass && t > 0 && read == Readout::Grid {
        let (r, c) = unsafe { (addr_of!(GR).read(), addr_of!(GC).read()) };
        let y = GRID_TOP + r as f32 * PITCH - sc;
        let x = MARGIN_X + c as f32 * (CARD_W + LGAP);
        // mid-transit of a page/letter jump the focused row can be off-screen for a few
        // frames — skip its resolve+draw until the scroll spring brings it on
        if on_axis(y, CARD_H, SCR_H, GLOW_PAD) {
            let s = unsafe { addr_of!(FOCUS_S).read() }.pos * crate::ui::press::scale();
            let rect = Rect::new(x, y, CARD_W, CARD_H).scaled(s);
            let m = crate::browse::item(focused_i);
            let label = m
                .map(|mm| match mm.year {
                    y if y > 0 => card_row::TileLabel::titled(&mm.title, &y.to_string()),
                    _ => card_row::TileLabel::title(&mm.title),
                })
                .unwrap_or_default();
            let resume = m.and_then(PmsMovie::resume_frac);
            card_row::draw_focused(pg, Art::Poster(m), rect, s, &RowStyle::HOME, resume, &label);
        }
    }

    // ---- top chrome: scrim under it once the grid has scrolled, then chip + pills + toolbar --
    // (drawn AFTER the focused card, so the whole band — scrim included — covers it)
    let ca = (sc / SCRIM_IN).clamp(0.0, 1.0);
    if ca > 0.004 {
        let base = theme::SURFACE_APP;
        let ps = p.alpha(ca); // the appear fade rides the Painter cascade — all three bands together
        ps.rect(Rect::new(0.0, 0.0, SCR_W, 170.0), 0.0, base, base, 0.0);
        ps.rect(Rect::new(0.0, 170.0, SCR_W, SCRIM_KNEE - 170.0), 0.0, base, theme::with_a(base, SCRIM_KNEE_A), 0.0);
        ps.rect(
            Rect::new(0.0, SCRIM_KNEE, SCR_W, GRID_TOP - SCRIM_KNEE),
            0.0,
            theme::with_a(base, SCRIM_KNEE_A),
            theme::with_a(base, 0.0),
            0.0,
        );
    }
    // the Library screen has no chip focus stop (its top-band focus is the tab row), so the chip
    // never unfurls here — expand 0.
    let cd = crate::ui::widgets::CHIP_D;
    crate::ui::widgets::profile_chip(pk, Rect::new(MARGIN_X, TOP_BAR_Y, cd, cd), 0.0);
    crate::ui::widgets::draw_tab_row(pk);

    // Sort and Filter are GRID operations, and the count counts what the grid holds — so on the
    // failure read-out none of the three is drawn: there is nothing to sort, nothing to filter and
    // nothing to count. Navigation is the exception and outlives the failure (the Source chip, when
    // it lands, belongs above this line): it is how you leave a dead source, which is why the
    // read-out itself carries no way-out hint. Their rects are parked with them — a control that is
    // not drawn must not be clickable (`ControlSlot::UpNext`'s rule).
    let none = Rect::new(-1.0, -1.0, 0.0, 0.0);
    if toolbar_n() == 0 {
        unsafe { *addr_of_mut!(TOOL_RECTS) = [none; N_TOOL_CHIPS] };
    } else {
        let tool_focus = |i: usize| area() == Area::Toolbar && !menu_open() && unsafe { addr_of!(TOOL_F).read() } == i;
        let chips = chip_strs();
        let mut x = MARGIN_X;
        let r0 = toolbar_chip(p, x, &chips[0], Icon::ChevronDown, tool_focus(0));
        x = r0.x + r0.w + 20.0;
        let r1 = toolbar_chip(p, x, &chips[1], Icon::ChevronDown, tool_focus(1));
        unsafe { *addr_of_mut!(TOOL_RECTS) = [r0, r1] };
    }

    // Item count, right-aligned on the toolbar line (cached — changes only on re-query). It counts
    // what the GRID holds, so it goes with the grid: absent on the failure (the design's rule —
    // there is nothing to count) and absent beside the empty read-out too, which already says the
    // same thing in words and does not need "0 films" agreeing with it.
    let tot = crate::browse::total();
    if tot >= 0 && read == Readout::Grid {
        let is_show = crate::browse::section_is_show(crate::browse::cur());
        let cache = unsafe { &mut *addr_of_mut!(COUNT_C) };
        if cache.as_ref().map(|(ct, cshow, _)| *ct != tot || *cshow != is_show).unwrap_or(true) {
            let noun = if is_show { "shows" } else { "films" };
            *cache = Some((tot, is_show, CString::new(format!("{tot} {noun}")).unwrap_or_default()));
        }
        let cs = &cache.as_ref().unwrap().2;
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 0, TOOL_Y + TOOL_H * 0.5);
        pg.text(cs.as_ptr(), SCR_W - MARGIN_X, ty, theme::size::CAPTION, theme::TEXT_TERTIARY, 2, 0);
    }

    // ---- letter rail (right edge; only on the unfiltered ascending-title listing) ------------
    // faded WITH the grid it indexes. Its `RAIL_RECTS` stay live at α≈0 for the ~210 ms of a
    // transition, so a pointer click there still jumps focus — judged harmless (the rail is
    // conceptually still present), and the one hit-test/visibility divergence this fade has.
    draw_rail(pg);

    // ---- menus over everything ---------------------------------------------------------------
    // These build their own root painter (`Popover::painter` — its scrim must not compound with
    // its own content fade), which used to mean an open menu sat at full strength over a dipping
    // page. It doesn't any more: `Popover::painter` multiplies BOTH of its roots by
    // `nav::page_alpha`, because a panel is drawn ON a page and a route change replaces the page.
    // The path was already unreachable here — every input that could start a route change while a
    // menu is up dismisses the menu instead, and `enter` clears `MENU` at the floor regardless —
    // but the popovers Home and the detail page carry (the item context menu, the profile menu) are
    // NOT, so the rule belongs in the shared component rather than in a note per screen.
    if menu_open() {
        let mp = pop().painter(0.45, 20.0);
        let r = panel_rect();
        mp.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        table().draw(mp, r);
    }
}

/// The A–Z rail: only the letters that EXIST (the server's `/firstCharacter` index), vertically
/// centered in the grid band on the right edge. The letter holding rail focus is a snow disc;
/// the letter containing the current grid focus reads primary; the rest tertiary.
fn draw_rail(p: Painter) {
    if !rail_drawn() {
        unsafe { RAIL_N = 0 };
        return;
    }
    let letters = crate::browse::letters();
    let n = letters.len().min(MAX_LETTERS);
    // TOP-ANCHORED at a fixed pitch (design review): the rail must sit in the same place for
    // every section (a centered block shifted between Movies' 9 letters and Shows' 7), and
    // pulled in from the panel edge. Capped so a full A–Z+digits set still fits the band.
    let pitch = 34.0f32.min((SCR_H - GRID_TOP - 40.0) / n as f32);
    let y0 = GRID_TOP + 8.0;
    let cx = SCR_W - 64.0;
    // a slim capsule track behind the letters — the tab-track family, minimized: makes the
    // rail read as a CONTROL to discover, not stray typography
    let track = Rect::new(cx - 22.0, y0 - 10.0, 44.0, pitch * n as f32 + 20.0);
    p.rect_sheened(track, 22.0, theme::scrim_black(0.30), theme::scrim_black(0.40));
    let in_rail = area() == Area::Rail;
    let cur_letter = letter_of(focus_idx());
    unsafe { RAIL_N = n };
    // label CStrings cached per (section table, section) — the letters are immutable once
    // landed, and this loop runs every frame in the default browse state
    let lcache = unsafe { &mut *addr_of_mut!(RAIL_LABELS) };
    let key = (crate::browse::sections_gen(), crate::browse::cur());
    let stale = lcache.as_ref().map(|(g, s, v)| *g != key.0 || *s != key.1 || v.len() != n).unwrap_or(true);
    if stale {
        *lcache = Some((
            key.0,
            key.1,
            letters.iter().take(n).map(|(l, _)| CString::new(l.as_str()).unwrap_or_default()).collect(),
        ));
    }
    let labels = &lcache.as_ref().unwrap().2;
    for (i, label) in labels.iter().enumerate() {
        let cy = y0 + (i as f32 + 0.5) * pitch;
        let d = 38.0f32;
        let r = Rect::new(cx - d * 0.5, cy - d * 0.5, d, d);
        unsafe { (*addr_of_mut!(RAIL_RECTS))[i] = r };
        let focused = in_rail && i == unsafe { addr_of!(RAIL_F).read() };
        // idle letters at SECONDARY (the review put TERTIARY below the couch floor here);
        // the current-position letter reads PRIMARY, the focused one is a snow disc
        let ink = if focused {
            p.rect(r, d * 0.5, crate::ui::ACCENT, crate::ui::ACCENT, 0.0);
            crate::ui::ACCENT_INK
        } else if i == cur_letter {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
        p.text(label.as_ptr(), cx, ty, theme::size::CAPTION, ink, 1, 1);
    }
}

// ---- headless switch-exercise hook (the FPS "all switches" scene) ---------------------------

/// One scripted switch action per call, cycling EVERY switch: tab → tab → sort
/// open/move/close → unwatched on/off → filter open → drill into Genre (kicks the value-list
/// fetch) → back out → letter-rail jump to the last letter and back. Drives the
/// `library_switch` FPS scene.
///
/// The unwatched step drives the same `Pending::Unwatched` REQUEST the Filter menu's row does, so it
/// still exercises the reload path now that the toolbar chip that used to own that switch is gone.
pub(crate) fn switch_step(k: u32) {
    match k % 14 {
        0 => {
            if crate::browse::section_count() > 1 {
                switch_section(1);
            }
        }
        1 => switch_section(0),
        2 => open_sort_menu(),
        3 => table().move_sel(1),
        4 => {
            let _ = back();
        }
        // the scene fires every 1400 ms — far longer than the ~210 ms transition — so every
        // switch still commits, and the scene now FPS-gates the cross-fade too
        5 | 6 => request(Pending::Unwatched(!view_unwatched())),
        7 => open_filter_menu(false),
        8 => table().move_sel(1),
        9 => menu_commit(1), // → the Genre value list (fetches it the first time)
        10 | 11 => {
            let _ = back(); // genre → filter → closed
        }
        12 => rail_jump(crate::browse::letters().len().saturating_sub(1)), // A→Z jump
        13 => rail_jump(0),                                               // and back to the top
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The deferred-reload queue. `PENDING`/`XF` are screen-level `static mut`s, so every test
    //! here holds the module's own [`PEND`] mutex for its whole body — the `home.rs` `FOCUS`
    //! precedent, and the cost of screen-level singleton state; a test that works around it races
    //! its siblings rather than the code.
    //!
    //! What is NOT here, deliberately: `apply_pending`'s effect on the STORE. `set_sort`/`set_genre`
    //! index into `st.sorts`/`st.genres`, which only a live `includeMeta=1` response fills, and
    //! nothing on this host draws a pixel — whether the dissolve READS right is a device capture.
    use super::*;
    use std::sync::Mutex;

    static PEND: Mutex<()> = Mutex::new(());

    /// Put the screen's queue + fader back the way a fresh boot has them, so ordering between
    /// tests cannot matter.
    fn clear() {
        unsafe { PENDING = Pending::None };
        *xf() = Xfade::new();
    }

    /// A fast double switch must commit ONCE, to the last thing pressed — the queue is one
    /// overwritten value, not a backlog. Deliberately kept off `browse`'s statics: with an empty
    /// section table `apply_section` early-returns, so this needs the module lock only.
    #[test]
    fn the_newest_queued_reload_supersedes_the_one_before_it() {
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        request(Pending::Sort(3));
        request(Pending::Section(2));
        assert_eq!(pending(), Pending::Section(2), "the newer request must replace the older one");
        assert!(xf().is_swapping(), "a request arms the fade-out");
        apply_pending();
        assert_eq!(pending(), Pending::None, "the commit drains the queue — one press, one swap");
        clear();
    }

    /// The whole point of the typed queue: the store is 70 ms behind, but a CONTROL must
    /// acknowledge its press on the frame it happens, or the fade reads as dropped input. Reads
    /// `browse::cur()`/`unwatched()`, so it takes `testlock::serial()` too — `browse`'s own tests
    /// call `reset()`, which nulls `SECTIONS`/`STATES` underneath these.
    #[test]
    fn the_chrome_reads_the_queued_reload_not_the_committed_store() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let committed = crate::browse::unwatched();
        request(Pending::Unwatched(!committed));
        assert_eq!(view_unwatched(), !committed, "the chrome flips on the press, not on the commit");
        // a second press inside the same fade cancels the first: the chip must be able to say so
        request(Pending::Unwatched(committed));
        assert_eq!(view_unwatched(), committed);

        request(Pending::Section(2));
        assert_eq!(view_section(), 2, "the pill travels to the pressed tab immediately");
        assert_eq!(view_unwatched(), crate::browse::unwatched(), "an unrelated request falls back to the store");
        clear();
        assert_eq!(view_section(), crate::browse::cur(), "with nothing queued the chrome IS the store");
    }

    /// [`focused_pill`] feeds a focus assignment ACROSS a route boundary (`app.rs` hands it to
    /// `home::set_hero_focus` at the page fade's floor), so a wrong answer parks Home's focus
    /// somewhere the user never was. It must report the pill only while the tab row is genuinely
    /// what holds focus — not the grid, not the toolbar, not the rail, and not under a menu, where
    /// `TAB_F` is merely the value the row was last left on.
    #[test]
    fn the_pill_the_user_is_holding_is_what_leaves_the_screen() {
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let (area0, menu0, tab0) = unsafe { (addr_of!(AREA).read(), addr_of!(MENU).read(), addr_of!(TAB_F).read()) };

        unsafe { AREA = Area::Tabs; MENU = Menu::None; TAB_F = 3 };
        assert_eq!(focused_pill(), Some(3), "the row holds focus — that pill crosses to Home");

        unsafe { MENU = Menu::Sort };
        assert_eq!(focused_pill(), None, "a menu owns the frame; the row underneath is not focused");
        unsafe { MENU = Menu::None };

        for a in [Area::Grid, Area::Toolbar, Area::Rail, Area::Status] {
            unsafe { AREA = a };
            assert_eq!(focused_pill(), None, "focus is not on the tab row");
        }

        unsafe { AREA = area0; MENU = menu0; TAB_F = tab0 };
        clear();
    }

    // ---- the section read-out ------------------------------------------------------------------
    //
    // [`readout_of`] is PURE — no statics, so no lock and no ordering — which is the whole reason
    // the decision was pulled out of the middle of `draw` in the first place: it is the only part
    // of a failure state a host can grade at all, and every one of its four answers used to be an
    // `else if` nothing could reach without a television.

    use crate::browse::SecFetch;

    /// The failure the screen exists for: this source did not answer, and there is nothing of it to
    /// show. Anything less than the full read-out here is the eternal spinner coming back.
    #[test]
    fn a_failed_fetch_with_nothing_to_show_is_the_failure_read_out() {
        assert_eq!(readout_of(SecFetch::Ready, 2, SecFetch::Failed, -1), Readout::Failed);
    }

    /// …and the mirror of it. A page failing mid-scroll on a section that already HAS items must
    /// not blank a working grid: the items are on screen, the back-off is counting, and a read-out
    /// over them would throw away a library the user can still browse to report a missing page.
    #[test]
    fn a_failed_page_over_a_populated_grid_leaves_the_grid_alone() {
        assert_eq!(readout_of(SecFetch::Ready, 2, SecFetch::Failed, 185), Readout::Grid);
    }

    /// A server that answered with nothing is `Empty` — never `Failed`, so no danger tint and no
    /// Retry. An empty library is an answer, and the read-out states it rather than blaming it.
    #[test]
    fn a_reachable_but_empty_library_is_an_answer_not_a_fault() {
        assert_eq!(readout_of(SecFetch::Ready, 2, SecFetch::Ready, 0), Readout::Empty);
        assert_ne!(readout_of(SecFetch::Ready, 2, SecFetch::Ready, 0), Readout::Failed);
        assert_eq!(readout_of(SecFetch::Ready, 2, SecFetch::Ready, 1), Readout::Grid, "a listing with items is just the grid");
    }

    /// Only a first fetch that has not answered yet is the spinner — the state that must be
    /// reachable exactly once per query, or it is the bug all over again.
    #[test]
    fn only_an_unanswered_first_fetch_spins() {
        assert_eq!(readout_of(SecFetch::Ready, 2, SecFetch::Loading, -1), Readout::Loading);
    }

    /// The eternal spinner one case OVER from the failed table: an account with nothing we browse
    /// (music and photos only) ANSWERED, so there is no section, no state, and both of the section
    /// accessors fall through to their defaults — `Loading` and `-1`, i.e. the spinner again. It is
    /// `Empty`, because the server told us; catching only the FAILED table would have left this
    /// spinning exactly as before.
    #[test]
    fn a_table_that_answered_with_no_browsable_library_is_empty_not_a_spinner() {
        assert_eq!(readout_of(SecFetch::Ready, 0, SecFetch::Loading, -1), Readout::Empty);
        assert_ne!(readout_of(SecFetch::Ready, 0, SecFetch::Loading, -1), Readout::Loading);
    }

    /// …and a table nobody has asked for yet is still the spinner, which is the one case where a
    /// spinner is the truth: the boot fetch is out.
    #[test]
    fn a_table_still_being_discovered_spins() {
        assert_eq!(readout_of(SecFetch::Loading, 0, SecFetch::Loading, -1), Readout::Loading);
    }

    /// The layer above: a failed section TABLE. There is no section, so `fetch_state()` answers
    /// `Loading` from its `unwrap_or` and the screen used to spin forever on it — one symptom, one
    /// screen, and now one read-out. The table's verdict OUTRANKS the section state precisely
    /// because that state is a default rather than an observation.
    #[test]
    fn a_failed_sections_list_is_the_same_read_out_and_not_a_spinner() {
        assert_eq!(readout_of(SecFetch::Failed, 0, SecFetch::Loading, -1), Readout::Failed);
        assert_ne!(readout_of(SecFetch::Failed, 0, SecFetch::Loading, -1), Readout::Loading);
        // and it does not depend on what the (absent) section's `unwrap_or` defaults claim
        for f in [SecFetch::Loading, SecFetch::Ready, SecFetch::Failed] {
            assert_eq!(readout_of(SecFetch::Failed, 0, f, 0), Readout::Failed);
        }
    }

    /// The read-out sits in the CONTENT REGION, under the live chrome — not centred on the panel,
    /// which is what the app-wide read-out means. `GRID_TOP` is the grid's own top edge, so the
    /// block centres on the band whose content is missing and the toolbar line stays clear.
    #[test]
    fn the_failure_is_anchored_in_the_content_region_not_on_the_panel() {
        let r = content_region();
        assert!(r.y >= GRID_TOP, "it starts at the grid, below the live toolbar line");
        assert!(r.cy() > Rect::FULL.cy(), "so its centre sits BELOW the panel centre");
        assert!(r.x >= MARGIN_X && r.x + r.w <= SCR_W - MARGIN_X, "and inside the screen margins");
    }

    /// Sort, Filter, the count and the rail all act on a grid that has no items, so the failure
    /// read-out is drawn beside none of them. `toolbar_n` is what the vertical walk reads, so this
    /// also pins UP-from-the-read-out on the chrome that is actually drawn.
    #[test]
    fn the_failure_read_out_is_not_drawn_beside_grid_controls() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        crate::browse::reset(); // no table, no section: `readout()` is Failed only once it FAILED
        assert!(toolbar_n() > 0, "an ordinary listing keeps its Sort and Filter chips");
        crate::browse::mark_sections_failed_for_test();
        assert_eq!(readout(), Readout::Failed);
        assert_eq!(toolbar_n(), 0, "there is nothing to sort, filter or count");
        crate::browse::reset();
    }
}
