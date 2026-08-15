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
use crate::ui::widgets::{Art, Pill, Spinner, StatusKind};
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
// The top-chrome scrim's own numbers used to live here — a knee at 188, its .40 alpha, and a 56px
// scroll-linked appear. They are `widgets::nav_scrim`'s now, DERIVED from `GRID_TOP` rather than
// spelled: the design system tokenises this treatment for every full-screen route that scrolls a
// column under the bar (`--scrim-in`, `--scrim-knee-a`, a per-route content line), and both of its
// routes agree on the shape these literals happened to be. Search draws the identical element.
//
// What that shape buys, kept here because it is this screen's own measurement: the knee keeps the
// toolbar chips (bottom y≈186) solidly backed while the tail crosses the RESTING popped card's top
// (`GRID_TOP - CARD_H*0.045 ≈ 197`) at ≤~26% — a soft entering-the-shadow veil, not a wash. One
// short linear ramp instead of the knee read as a harsh bar shadow with a visible bottom edge.
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
/// **The SOURCE outranks the section, and both of its terminal answers do — while there is nothing
/// to show.** With no table there is no section to page, so `fetch` is the `unwrap_or(Loading)` of a
/// state that does not exist and `total` is its `unwrap_or(-1)` — i.e. the spinner, from two
/// defaults, which is the eternal spin this whole change is about. It is not enough to catch the
/// FAILED source: one that answered with nothing we browse (a music-only account) is `Ready` with
/// `n_sections == 0` and lands in the same two defaults one case over. That one is `Empty` — it
/// answered — and the read-out says so.
///
/// The `total < 0` gate on both is the ONE thing this rule gained from the multi-source table, and
/// it is a real decision rather than a mechanical one: a source that has stopped answering keeps
/// the sections it had learned and can still have a page store full of items, so "this source is
/// gone" and "there is nothing of it on screen" stopped being the same fact. The read-out wins only
/// when the second is true as well. What that costs is a grid you can still scroll but cannot play
/// from — the posters are cached, the server is not there — and the alternative costs a library
/// blanked to report a server the Sources list is already dimming. The page rule below has made the
/// same trade since before this branch; making the two layers disagree about it would be worse than
/// either answer.
///
/// Two per-section rules are the ones that keep being got wrong. A `Failed` fetch on a section that
/// already HAS items is `Grid`: a mid-scroll page failure must not blank a working screen — the
/// items are still there, the back-off is already counting, and `browse::pump`'s failure branch
/// exists precisely so the store survives. And a `Ready` empty listing is `Empty`, never `Failed`:
/// an empty library is an answer (`StatusKind::Empty`'s rule, stated in `browse::SecFetch` too).
fn readout_of(table: crate::browse::SecFetch, n_sections: usize, fetch: crate::browse::SecFetch, total: i64) -> Readout {
    use crate::browse::SecFetch;
    // Both of the TABLE's terminal answers, and both gated on there being NOTHING TO SHOW — the
    // same sentence the page rule below is gated on, and the merge is what made that matter. On the
    // branch this came from, a failed table meant no sections at all, so "failed" and "nothing to
    // show" were the same fact. They are not any more: a source that stops answering KEEPS every
    // section it had learned (`BrowseSource::reachable`'s rule, the one the Sources list dims a
    // group by), and its page store can still hold the items you were just looking at. Blanking
    // those to say "can't reach" is the bug the page rule exists to prevent, one layer up.
    if total < 0 {
        match table {
            SecFetch::Failed => return Readout::Failed,
            SecFetch::Ready if n_sections == 0 => return Readout::Empty,
            _ => {}
        }
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
        crate::browse::cur_source_state(),
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
    /// (empty file, or `dead`) a BORROWED source that did not answer — the design's case, with the
    /// reason line drawn. The owner is the REAL one whenever the browsed source has a handle; the
    /// stand-in is only the fallback for a one-server account, where the three-block read-out has
    /// no way to reach a panel at all and a capture is the only thing that grades its layout.
    Dead,
    /// `own` — the same failure on YOUR OWN server: verdict and action, no reason line, because
    /// there is no owner to name.
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
/// Which menu is open — **and, inside the variant, the key that menu was built from.**
///
/// The key belongs here because it means nothing anywhere else: it describes ONE panel, the one on
/// screen. [`close_menu`] assigns `Menu::None`, which drops the key with the panel it described, so
/// there is no state left over for a reader to pick up and no build-time record that can outlive
/// the rows it was recorded from. A menu that has no landing to wait for carries no key at all —
/// see [`Filter`](Menu::Filter).
///
/// Not `Copy` (the Sources panel's action list is a `Vec`), so [`menu`] hands out a borrow; the
/// discipline that comes with that is written there.
#[derive(Debug)]
enum Menu {
    None,
    /// The **Sources list** — every library on every granted server, in two levels (see [`Level`]).
    Source {
        /// The section-table generation ([`crate::browse::sections_gen`]) this panel was built
        /// from — its staleness key, and deliberately NOT a row count.
        ///
        /// Sort and Genre can key on a count because the list they are built from is the list they
        /// show. The Sources panel's rows are `browse::source_rows()`, which is **kind-filtered** —
        /// only the libraries of the tab's own type — while the obvious count beside it,
        /// `section_count()`, counts every kind. Those two are equal only on an account with a
        /// single kind of library, so on any ordinary one (Movies *and* TV Shows) the guard never
        /// converged and the panel rebuilt itself — `source_groups()` + `source_rows()` String
        /// clones and a full `set_sections()` — on EVERY frame it was open, which is exactly the
        /// state this screen's own instrumentation measured as fill-rate-bound (`menu.panel`
        /// 16–42 ms). A generation cannot be compared against the wrong quantity, because it is not
        /// a count of anything: `browse::append_sections` bumps it when a source's libraries land,
        /// and that landing is the whole reason this rebuild exists.
        gen: u32,
        /// One action per global row index (see [`SrcAction`]) — including the separator, which
        /// does nothing and can never be focused, but still occupies an index.
        acts: Vec<SrcAction>,
    },
    /// Sort by — the value list `browse::sorts()`.
    Sort {
        /// Row count this menu was built from (0 = the "Loading…" row). [`menu_stale`] rebuilds it
        /// in place when the live list stops matching, and [`menu_commit`] gates OK on it so a
        /// press on "Loading…" cannot resolve to a value that is not there.
        built: usize,
    },
    /// Filter — the unwatched switch and the way in to Genre.
    ///
    /// **No key**, because its rows are static: they are built from the `view_*` resolvers rather
    /// than from a server list, so there is no landing they could be stale against and nothing to
    /// rebuild in place ([`menu_stale`] says the same in its `false` arm).
    Filter,
    /// The Genre value list, drilled into from Filter.
    Genre {
        /// Row count this menu was built from — `Sort`'s key, over `browse::genres()`.
        built: usize,
    },
}

/// The toolbar's chips, as a TYPED list rather than positions.
///
/// The Source chip is drawn only when there is more than one source, so a chip's index is not its
/// identity: with one server the row is `[Sort, Filter]` and with two it is `[Source, Sort,
/// Filter]`. Every activation matches on the CHIP. A `_` catch-all here is the specific bug this
/// enum exists to prevent — inserting a chip at index 0 silently made Source open the Sort menu,
/// because "everything that isn't 0" was Filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chip {
    Source,
    Sort,
    Filter,
}

/// The two levels of the Sources panel, swapped by the pills at its top — the same swap the
/// player's track menu makes between Audio and Subtitles.
///
/// They differ in MEDIUM as well as in position, and that is the design's rule: a **mark** says
/// where you are, a **word** says what is set, and no row ever says both. So Browse draws one tick
/// and no words, On Home draws every row's word and no ticks — neither level mirrors the other's
/// marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    /// a picker: one tick, on the library you are looking at; OK closes the panel
    Browse,
    /// toggles: `On`/`Off` at the trailing edge; OK flips and the panel stays open
    OnHome,
}

/// What a row of the Sources panel does. Built beside the rows, indexed by the same global row
/// index the `TableView` reports — including the separator, which does nothing and can never be
/// focused, but still occupies an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SrcAction {
    None,
    /// browse (Browse level) or pin (On Home level) this section
    Library(usize),
    Recheck,
}
/// What an OK / click means for app.rs (everything else is consumed internally).
pub(crate) enum Action {
    None,
    /// The Home tab was picked — route back to Home.
    GoHome,
    /// The Search pill was picked. A separate arm rather than a `GoHome`-with-a-pill, because the
    /// strip's last pill is not a library and `switch_tab` would resolve it to a section that does
    /// not exist — silently, since `tab_section` clamps.
    GoSearch,
    /// A grid card was activated — open its detail page (app.rs owns routing).
    Card,
}

static mut AREA: Area = Area::Grid;
static mut TAB_F: usize = 1; // focused pill while AREA==Tabs (0 = Home)
/// Where the toolbar cursor was last WALKED to. Never read directly — the row it indexes changes
/// length without input, so every reader goes through [`tool_f`], which clamps it to the row that
/// is drawn. (It is not a chip identity either: the row is two chips long with one source and
/// three with two — see [`Chip`].)
static mut TOOL_F: usize = 0;
static mut GR: usize = 0; // grid focus row
static mut GC: usize = 0; // grid focus col
static mut SCROLL: Spring = Spring::at(0.0);
static mut FOCUS_S: Spring = Spring::at(1.0); // focused cell pop
static mut PREV_S: Spring = Spring::at(1.0); // previously-focused cell shrinking back
static mut PREV_IDX: i64 = -1;
/// The open menu, **and its build-time key** — the two are one value, see [`Menu`].
static mut MENU: Menu = Menu::None;
static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
static mut PHASE_MS: f32 = 0.0; // spinner phase accumulator
/// The failure read-out's Try again pill, recorded at draw for the pointer (the `TOOL_RECTS` /
/// `RAIL_RECTS` idiom). Parked OFF the panel rather than at the origin, because `Rect::contains`
/// is inclusive and a zero-size rect at (0,0) would "contain" a click at exactly (0,0).
static mut STATUS_BTN: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);
/// Chip rects for pointer hit-testing, in [`chips`] order. The row is two or three long, so the
/// unused slot stays a zero rect — and every scan tests `w > 0.5` first, because a zero rect at the
/// origin `contains` the origin.
static mut TOOL_RECTS: [Rect; 3] = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
/// Which level of the Sources panel is showing. Browse opens first, EVERY time (picking is the
/// frequent act; pinning is a thing you do once per friend), so this is not remembered across opens.
static mut SRC_LEVEL: Level = Level::Browse;
/// The Sources panel's level switch, as the shared travelling-capsule strip. A static for the same
/// reason the top row's is: the springs must survive the frame, and there is exactly one of this
/// control on screen.
static mut SRC_STRIP: crate::ui::widgets::TabStrip = crate::ui::widgets::TabStrip::new();
/// Focus is on the level pills rather than in the list. UP from the first row reaches them,
/// LEFT/RIGHT swaps the level under them, DOWN returns to the rows. While it is true the table
/// draws NO selection pill ([`TableView::list_focused`]), so the panel shows exactly one focus.
static mut SRC_ON_PILLS: bool = false;
/// The two level pills' rects, for the pointer.
static mut SRC_PILL_RECTS: [Rect; 2] = [Rect::new(0.0, 0.0, 0.0, 0.0); 2];

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
// ---- the toolbar's chip row -------------------------------------------------------------------

/// Does the toolbar carry a Source chip? **With one source none of this is drawn** — not a bare
/// suffix, not an empty slot, not a disabled chip: the toolbar is the two chips it has always been
/// and the Sources list does not exist. Absence, not a branch that draws nothing visible.
fn source_chip_on() -> bool {
    crate::browse::sources().len() > 1
}
const CHIPS_NONE: [Chip; 0] = [];
const CHIPS_SOURCE_ONLY: [Chip; 1] = [Chip::Source];
const CHIPS_ONE: [Chip; 2] = [Chip::Sort, Chip::Filter];
const CHIPS_MANY: [Chip; 3] = [Chip::Source, Chip::Sort, Chip::Filter];
/// The chips DRAWN, in drawn order — Source FIRST when it exists. The ONE predicate behind the
/// draw, the pointer scan and the vertical walk, so a chip can never be invisible-and-clickable or
/// drawn-and-unreachable.
///
/// **The failure read-out takes the grid's controls away and keeps navigation.** Sort and Filter
/// act on a grid that has no items, so neither is drawn (nor is the count, nor the A–Z rail); the
/// Source chip is how you LEAVE a dead source, which is exactly why the read-out spends no line
/// telling you the way out (`Shared Sources.dc.html` D). With one source there is no Source chip to
/// keep, so that row empties completely and the tab strip above is the way out.
///
/// An OPEN menu is the exception: it is anchored under its own chip and would otherwise be a panel
/// hanging off nothing — a listing can fail while its Filter menu is up, and a chip must outlive
/// the panel it opened.
fn chips() -> &'static [Chip] {
    match (source_chip_on(), readout() == Readout::Failed && !menu_open()) {
        (true, false) => &CHIPS_MANY,
        (true, true) => &CHIPS_SOURCE_ONLY,
        (false, false) => &CHIPS_ONE,
        (false, true) => &CHIPS_NONE,
    }
}
/// **The toolbar's focused chip INDEX — the ONE authority, read by the draw, by the walk and by
/// the activation alike.** Nothing else may read `TOOL_F`.
///
/// The raw static is where the user last walked to, and the row's LENGTH moves under it without
/// any input at all: a second source is granted, or a listing fails and `chips()` collapses
/// `CHIPS_MANY` → `CHIPS_SOURCE_ONLY`, or a profile switch is absorbed by the `EPOCH` watchdog with
/// no `enter()` to zero it. So the index is clamped to the row that is actually DRAWN, at the one
/// point everything reads, because the failure of clamping it in only some of them is silent and
/// asymmetric: `focused_chip` clamped and the draw did not, so the toolbar drew NO focused chip
/// while OK still activated the clamped last one and RIGHT was dead against a length the ring was
/// not standing in.
///
/// Clamping rather than resetting is deliberate: the raw value survives, so a row that grows back
/// (the source answered again) returns focus to the chip the user actually walked to instead of
/// dumping it at 0.
fn tool_f() -> usize {
    let n = chips().len();
    unsafe { addr_of!(TOOL_F).read() }.min(n.saturating_sub(1))
}

/// The chip holding toolbar focus, or `None` when the row is EMPTY — which it is on a one-source
/// failure, where every chip this toolbar has is a grid operation.
fn focused_chip() -> Option<Chip> {
    chips().get(tool_f()).copied()
}

/// THE chip → menu map, and the ONE place a chip is turned into an action. Both activation paths
/// (OK on the focused chip, a pointer click on one) call it, so the remote and the pointer cannot
/// disagree about what a chip does — the pointer arm used to route through `on_ok`, which meant it
/// inherited whatever `on_ok` got wrong.
///
/// **Exhaustive on purpose.** A `_` arm here is the bug this whole enum exists to prevent: the
/// toolbar used to match on the chip's INDEX with a catch-all, so the day Source was inserted at 0
/// it opened the Sort menu and both Sort and Filter opened Filter — silently, since every index
/// still resolved to *something*. Add a chip and this stops compiling, which is the point.
fn open_chip_menu(c: Chip) {
    match c {
        Chip::Source => open_source_menu(),
        Chip::Sort => open_sort_menu(),
        // the unwatched switch lives inside this one
        Chip::Filter => open_filter_menu(false),
    }
}

/// Which menu a chip opens, as a value — the testable half of [`open_chip_menu`], which is the
/// half that performs it. They are written as one `match` each over the same enum, so a new chip
/// is a compile error in both.
///
/// The keys are empty, and that is the honest answer here: a key describes a panel that was BUILT
/// from a row set, and this function answers which menu a chip leads to. Its caller matches on the
/// variant, never on the payload.
#[cfg(test)]
fn chip_menu(c: Chip) -> Menu {
    match c {
        Chip::Source => Menu::Source { gen: 0, acts: Vec::new() },
        Chip::Sort => Menu::Sort { built: 0 },
        Chip::Filter => Menu::Filter,
    }
}

/// A chip's two runs: the dim static NAME and the bright VALUE that carries the information.
fn chip_value(c: Chip) -> (&'static str, String) {
    match c {
        // the chip names the LIBRARY (the owner rides beside it as a dim micro run — see
        // `chip_strs`), which is the one piece of source annotation the design keeps and the reason
        // the tab strip above needs none
        Chip::Source => ("Library", crate::browse::section_title(view_section()).to_string()),
        Chip::Sort => ("Sort", view_sort_label().to_string()),
        Chip::Filter => ("Filter", view_filter_value()),
    }
}

// ---- per-frame draw caches (rebuilt only when their source state changes; building CStrings
// + re-measuring text every frame was a review-confirmed waste — same rationale as TAB_CACHE) --
struct ChipStrs {
    name: CString,
    val: CString,
    /// The Source chip's owner annotation — the handle, a rung down and at 62% of the label's ink.
    /// `None` on your own libraries and on every other chip, and None means NO RUN: no gap, no
    /// separator, no draw call.
    note: Option<CString>,
    w: f32,
}
/// (cache key, chips). The key is every string the row draws — **including the Source chip's own
/// value and its handle**, without which the chip keeps a stale label right across a source switch,
/// which is the one moment its label is the only thing on screen that changed.
static mut CHIP_CACHE: Option<(String, Vec<ChipStrs>)> = None;
static mut RAIL_LABELS: Option<(u32, usize, Vec<CString>)> = None; // (sections_gen, section, labels)
// (total, the section TYPE's noun, the rendered string) — keyed on the noun rather than on a
// movie/show boolean, because a section is one of four types now (`browse::SecKind`)
static mut COUNT_C: Option<(i64, &'static str, CString)> = None;
/// The failure read-out's two runtime lines, keyed on the RAW source labels they were built from
/// (`(name, handle)`) — see [`dead_strs`] for why that rather than a generation, and why the key is
/// the raw pair rather than the resolved one.
static mut DEAD_C: Option<(String, String, CString, Option<CString>)> = None;
/// The Empty read-out's caption, cached per (section table, section, filtered) — the three inputs
/// that can change what it says.
static mut EMPTY_C: Option<(u32, usize, bool, CString)> = None;
// letter rail: focused letter + the per-letter rects recorded at draw (pointer hit-testing)
/// Every bucket `/firstCharacter` can return, not one alphabet's worth. `#` + Latin is 27, which
/// is where the old 34 came from — but a Cyrillic section adds 32 more, and the cap TRUNCATED in
/// silence: the rail simply stopped mid-alphabet and every title past it became unreachable by
/// jump, with nothing on screen saying anything had been dropped. 64 covers `#` + Latin + Cyrillic
/// (59) with room, and now that the rail SCROLLS a large `n` costs no layout.
const MAX_LETTERS: usize = 64;
/// The rail's letter pitch — a CONSTANT, not the band divided by `n`. It was the latter (capped at
/// this value), which made the spacing a function of how many letters a section happens to have: 9
/// letters drew at 34 px, a Cyrillic 30 fell to 27.5 against a CAPTION cap height of ~17, closing
/// the gap to under half a letter and reading as a solid column of type rather than an index. Past
/// what fits at this pitch the rail scrolls ([`RAIL_SCROLL`]) — so every section's rail has the
/// same spacing, for the same reason it is top-anchored rather than centred.
const RAIL_PITCH: f32 = 34.0;
static mut RAIL_F: usize = 0;
static mut RAIL_RECTS: [Rect; MAX_LETTERS] = [Rect::new(0.0, 0.0, 0.0, 0.0); MAX_LETTERS];
static mut RAIL_N: usize = 0;
/// Scroll offset (px) for a section with more letters than fit. Driven by the rail's own focus
/// while it holds focus, otherwise by the letter the GRID is in — so scrolling the grid carries
/// the rail along and it never shows a stretch of alphabet the grid left behind.
static mut RAIL_SCROLL: Spring = Spring::at(0.0);

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

/// How many toolbar chips are DRAWN — [`chips`]'s length, and nothing else. The whole decision
/// (which chips a failure keeps, and that an open menu keeps its anchor) lives in `chips`, so the
/// walk and the draw cannot answer it differently.
///
/// One cost is recorded here so it is not rediscovered as a surprise: a query the SERVER rejects
/// outright (a sort key it 400s on) fails the same way a dead server does, and the chips that could
/// have changed it are not drawn — so *Try again* re-runs the same losing query, and the way out is
/// the Source chip, another section, or Home. Drawing Sort and Filter over the failure is in the
/// design's rejected column ("they act on a grid that has no items") and clearing the user's filter
/// behind their back is worse than either, so this stands.
fn toolbar_n() -> usize {
    chips().len()
}

/// The failure read-out's verdict + reason — the machine that failed, and the person who shared it.
///
/// **The owner is real now.** On the branch this came from it was hard-wired `None` with a note
/// that nothing yet mapped a section to the source it came from; the multi-source table is exactly
/// that mapping, so `browse::cur_source_labels` answers both halves and the reason line stops being
/// a stand-in. An empty handle still means YOUR OWN server, and the line is then ABSENT rather than
/// empty — our own server failing is a complete statement and there is nobody to name.
///
/// Cached, and keyed on the STRINGS rather than on a generation, because that is what actually
/// changes: a machine names itself through `friendlyName`, which lands on a worker long after the
/// table generation last moved, and a cache keyed on the generation would hold "Can't reach "
/// forever. Two `&str` comparisons a frame, and only while the read-out is on screen.
fn dead_strs() -> &'static (String, String, CString, Option<CString>) {
    // Keyed on the RAW labels, which are two `&'static str` reads off this module's own table — and
    // the staleness check runs BEFORE anything resolves them. It used to resolve first and compare
    // after, which meant `source_labels`' session fallback ran every drawn frame the read-out was
    // up: a file read and a JSON parse, in the one state where that fallback is guaranteed to fire
    // (a source that never answered can never have supplied a `friendlyName`).
    let (name, handle) = crate::browse::cur_source_labels();
    let cache = unsafe { &mut *addr_of_mut!(DEAD_C) };
    let stale = cache.as_ref().map(|(n, h, _, _)| n != name || h != handle).unwrap_or(true);
    if stale {
        let (machine, owner) = source_labels();
        let verdict = CString::new(format!("Can't reach {machine}")).unwrap_or_default();
        let reason = owner
            .as_ref()
            .map(|o| CString::new(format!("Shared by {o} \u{b7} your own server is fine.")).unwrap_or_default());
        *cache = Some((name.to_string(), handle.to_string(), verdict, reason));
    }
    cache.as_ref().unwrap()
}

/// The two names the read-out may speak, resolved for the source the screen is showing.
///
/// The MACHINE falls back to the session's own server name and then to a WORD — never to the
/// address, though it is right there in the client. This read-out is photographable (it is what a
/// user sends when reporting "my friend's library stopped working") and `ui::stats` already settled
/// the rule for anything a camera can reach: no URL, no path, no address, no machineIdentifier. On
/// a BORROWED source that reasoning is stronger, because the machine is a third party's.
///
/// The OWNER is `None` for your own server. [`FailTest::Dead`] can put a stand-in there, and only
/// there: with one server and nothing shared, the design's three-block read-out has no way to
/// appear on a real panel at all, and a capture of it is the only thing that grades the layout.
fn source_labels() -> (String, Option<String>) {
    let (name, handle) = crate::browse::cur_source_labels();
    let machine = if !name.is_empty() {
        name.to_string()
    } else {
        let session = crate::plex::session::peek().server.name;
        if session.is_empty() {
            "this server".to_string()
        } else {
            session
        }
    };
    let owner = if handle.is_empty() {
        (fail_test() == Some(FailTest::Dead)).then(|| STANDIN_OWNER.to_string())
    } else {
        Some(handle.to_string())
    };
    (machine, owner)
}

/// The stand-in handle [`FailTest::Dead`] falls back to. A DOCUMENTATION placeholder, deliberately
/// not a plausible one: it is drawn on a real screen and may be photographed, and a real person's
/// plex.tv handle is not ours to put on a television.
const STANDIN_OWNER: &str = "friend";

/// The Empty read-out's caption. A library the server ANSWERED for and that holds nothing is
/// named — "No films in Film Club" — because the name is what makes it a statement rather than a
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
            // the TYPE's own noun — a section is one of four kinds now, so "films" is a guess
            // rather than an answer for three of them
            let noun = crate::browse::section_kind(sec).map(|k| k.noun()).unwrap_or("items");
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
    // `browse` owns WHICH layer failed — it holds the per-source flags — and clears the back-off on
    // both, which is the right answer either way. Nothing blocks and nothing re-enters the screen:
    // a table that lands arrives through `pump`'s worker like any other source's, and the read-out
    // clears itself on the frame `readout()` stops saying Failed.
    crate::browse::retry_cur_source();
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
/// The open menu and its key, as a borrow — [`Menu`] owns a `Vec` and so cannot be read out by
/// copy.
///
/// **Take what you need out of it before anything rebuilds.** Every builder ASSIGNS `MENU`, which
/// drops the key this borrow points into: [`src_action`] copies its answer out, [`menu_stale`]
/// compares and returns a `bool`, and an arm that calls a builder either binds nothing out of the
/// variant or reads its key into a local first ([`menu_commit`]'s Sort arm).
fn menu() -> &'static Menu {
    unsafe { &*addr_of!(MENU) }
}
/// Install the open menu together with its key, dropping whatever the last one held.
fn set_menu(m: Menu) {
    unsafe { *addr_of_mut!(MENU) = m };
}
/// Is the SOURCES panel the open menu? Asked by the panel geometry (it is wider and carries a level
/// band above its list), by both input ladders and by the draw, so it is one predicate rather than
/// a `matches!` re-spelled at each of them.
fn source_menu_open() -> bool {
    matches!(menu(), Menu::Source { .. })
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

/// The pill focus LANDS on when it walks up into the tab row.
fn own_pill() -> usize {
    // The pill that REPRESENTS this section, which for a borrowed library is its type's — and then
    // clamped into the pills that EXIST. `last_section_pill` is that ceiling and owns the "with no
    // section table there is only Home" fallback: an index past the last pill is a highlight on
    // nothing, reachable now that a failed discovery keeps the user on this screen instead of
    // bailing out of `enter` before it had a table. It used to be spelled `search_pill() - 1` here,
    // which meant "the last pill that is a library" only because Search happens to sit last — a
    // fact about the strip that now lives in the strip.
    let p = crate::ui::widgets::pill_of(Pill::Section(crate::browse::tab_of_section(view_section())));
    p.min(crate::ui::widgets::last_section_pill())
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
    !matches!(menu(), Menu::None)
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

/// Enter the Library on tab PILL `tab` (0-based, Home excluded). Discovers the current server's
/// sections on first use (blocking, one small GET) and restores that library's remembered
/// focus/scroll.
///
/// Everything here TELEPORTS something the user can see — the store swap, the restored grid scroll,
/// the focus band — so on the `Faded` arrival it is called from the page fade's floor, at alpha 0,
/// rather than on the press frame.
pub(crate) fn enter(tab: usize, arrival: Arrival) {
    flush(); // a reload queued on the way out is applied, not dropped (see [`flush`])
    let n = crate::browse::ensure_sections();
    // A table that could not be FETCHED used to return here, before a single line of screen state
    // was set — which left the focus band wherever the last visit had parked it, on a screen that
    // now has no grid and no chips. The discovery failing is a state this screen draws (the read-out
    // one layer up), so it is entered like any other: everything below runs, and only the parts
    // that index a section are skipped.
    if n > 0 {
        // `tab` is a PILL index (`app.rs`'s `Nav::Library`, which derives it from the pill pressed),
        // and a pill is a type rather than a library — so it is resolved through the projection here,
        // at the ONE boundary where the strip's index space enters this screen.
        crate::browse::set_cur(crate::browse::tab_section(tab.min(crate::browse::tab_count().saturating_sub(1))));
        crate::browse::kick_letters();
        restore_view();
        // with more libraries than fit the row, a section past the visible pills would open with its
        // own tab off screen (the `/tmp/plxnative-library=N` boot, or a restored far-right section).
        // Put it on screen once, here, rather than dragging the row every frame — but only on a CUT;
        // see `Arrival::Faded` for why a jump is both wrong and redundant under a travelling capsule.
        if matches!(arrival, Arrival::Cut) {
            crate::ui::widgets::tab_row_reveal(crate::browse::tab_of_section(crate::browse::cur()) + 1);
        }
    }
    set_menu(Menu::None);
    unsafe {
        AREA = Area::Grid;
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

/// Queue a switch to the library a TAB PILL opens. The STORE moves on the fade floor
/// ([`apply_section`]); the PILL moves now (every chrome reader goes through [`view_section`]).
///
/// A pill is a TYPE, not a library, so the strip's index space is not the table's — see
/// `browse::tab_section`. Everything downstream of here speaks SECTION indices.
fn switch_tab(t: usize) {
    if t >= crate::browse::tab_count() {
        return;
    }
    switch_to_section(crate::browse::tab_section(t));
}

/// Queue a switch to a section by its ABSOLUTE index in the table — what the Sources list picks,
/// where a row can name a library on any server and on no pill at all.
fn switch_to_section(i: usize) {
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
/// The rail letter whose range contains item `idx`. Clamped to the drawn set, since it also
/// indexes [`RAIL_RECTS`] by way of [`RAIL_F`].
fn letter_of(idx: usize) -> usize {
    let mut sum = 0usize;
    for (i, (_, n)) in crate::browse::letters().iter().enumerate().take(MAX_LETTERS) {
        sum += *n as usize;
        if idx < sum {
            return i;
        }
    }
    crate::browse::letters().len().min(MAX_LETTERS).saturating_sub(1)
}
/// The rail's geometry for `n` letters: `(y0, cx, win_h, max_scroll)`. ONE answer, because the
/// scroll step in [`update`] and [`draw_rail`] must agree about the window — a reveal rule aimed
/// at a window that is not the one on screen parks the focused letter off the end of it.
fn rail_geom(n: usize) -> (f32, f32, f32, f32) {
    // the band the fit-to-fill divisor used, so a rail that DID fit sits exactly where it did
    let vis = ((SCR_H - GRID_TOP - 40.0) / RAIL_PITCH).floor().max(1.0);
    let win_h = vis.min(n as f32) * RAIL_PITCH;
    (GRID_TOP + 8.0, SCR_W - 64.0, win_h, (n as f32 * RAIL_PITCH - win_h).max(0.0))
}
/// Where the rail's scroll wants to be with letter `drive` in hand: the grid's own reveal rule in
/// letter units, keeping one whole letter clear on each side so the driver never sits in the edge
/// fade. Pure, so the window invariant is assertable off-device.
fn rail_scroll_target(cur: f32, drive: usize, n: usize) -> f32 {
    let (_, _, win_h, max) = rail_geom(n);
    let d = drive.min(n.saturating_sub(1)) as f32;
    card_row::reveal(cur, (d + 2.0) * RAIL_PITCH - win_h, (d - 1.0) * RAIL_PITCH, max)
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

    // The Sources panel's level capsules travel, so they are STEPPED here like every other spring
    // and only DRAWN in `draw_level_pills`. Stepped whether or not the panel is open — the springs
    // then settle while it is closed and a re-open starts from rest rather than sliding in from
    // wherever the last visit left them.
    {
        let rects = src_pill_rects(panel_rect());
        let sel_i = if unsafe { addr_of!(SRC_LEVEL).read() } == Level::Browse { 0 } else { 1 };
        let foc_i = if unsafe { addr_of!(SRC_ON_PILLS).read() } { sel_i } else { -1 };
        unsafe {
            (*addr_of_mut!(SRC_STRIP)).update(sel_i, foc_i, |i| rects.get(i).map(|r| (r.x, r.w)), dt);
        }
    }

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
    if matches!(menu(), Menu::Genre { .. }) && crate::browse::genres().is_empty() {
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

        // the letter rail's own scroll, for the sections whose alphabet does not fit at
        // `RAIL_PITCH`. Same reveal rule as the grid above, in letter units: keep the driving
        // letter one whole letter clear of each end, so it never sits in the edge fade.
        let drive = if area() == Area::Rail { RAIL_F } else { letter_of(focus_idx()) };
        let ln = crate::browse::letters().len().min(MAX_LETTERS);
        let want_rs = rail_scroll_target((*addr_of!(RAIL_SCROLL)).pos, drive, ln);
        (*addr_of_mut!(RAIL_SCROLL)).step(want_rs, K_SCROLL, dt);

        // remember the view for re-entry (state amnesia is the official app's #2 complaint)
        crate::browse::save_view(focus_idx(), (*addr_of!(SCROLL)).pos);
    }
    // the shared tab strip's horizontal scroll — with more libraries than fit the row, this is
    // what reaches the far pills. Drawn from `.pos`; the draw pass runs at dt=0. Off the tab row
    // it tracks THIS section's pill, which `enter` already put on screen, so it holds still.
    // the VIEW section, so the strip travels to the pressed pill on the press frame while the grid
    // dissolves under it
    crate::ui::widgets::tab_row_update(crate::browse::tab_of_section(view_section()) as c_int + 1, tab_focus(), dt);
    if menu_open() {
        pop().update(dt);
        table().update(dt, table_rect().h - crate::ui::table::PAD_V);
        // async menu data landing while the popover is open — rebuild it in place (a Sort
        // menu stuck on "Loading…" whose OK secretly toggled the direction was a
        // review-confirmed bug). Two matches over one enum, the `chip_menu`/`open_chip_menu`
        // idiom: one DECIDES and is host-gradeable, one PERFORMS and measures text. A new menu
        // stops both compiling.
        //
        // Nothing is bound out of the variants here, deliberately: each of these rebuilds REPLACES
        // `MENU` — key and all — so a field borrowed from the scrutinee would be dropped underneath
        // the arm that is still running (see [`menu`]).
        if menu_stale() {
            match menu() {
                Menu::Sort { .. } => build_sort_menu(true),
                Menu::Genre { .. } => build_genre_menu(true),
                // a source's libraries landing while the list is open (its discovery is a worker) —
                // rebuilt in place, same rule as a menu's value list arriving
                Menu::Source { .. } => build_source_menu(true),
                Menu::Filter | Menu::None => {}
            }
        }
    }
}

// ---- input: D-pad ---------------------------------------------------------------------------

pub(crate) fn move_focus(sym: c_uint) {
    // FIRST, and the order is the panel's whole level layer: `menu_open()` below SUBSUMES the
    // Sources panel (it is a menu), so a ladder that asked it first would swallow LEFT/RIGHT and
    // leave the level pills unreachable. The test that fails if the two are ever folded together is
    // `the_sources_panel_owns_left_right_for_its_levels_and_a_plain_popover_does_not`.
    if source_menu_open() {
        // The level pills ARE a focus stop above the list: UP off the first row reaches them,
        // LEFT/RIGHT swaps the level under them, DOWN comes back into the rows.
        //
        // **Exactly one of the two wears focus at a time**, which is the part that was wrong: the
        // pill row took the bright accent capsule while the list went on painting its selected row,
        // because a table always paints its selection. `TableView::list_focused` is the "nothing
        // selected" mode that fixes it — the row is REMEMBERED (you come back to where you left)
        // and simply not drawn as focused while the pills have it.
        let on_pills = unsafe { addr_of!(SRC_ON_PILLS).read() };
        if on_pills {
            match sym {
                SDLK_LEFT => set_source_level(Level::Browse),
                SDLK_RIGHT => set_source_level(Level::OnHome),
                SDLK_DOWN => unsafe { SRC_ON_PILLS = false },
                _ => {}
            }
        } else if sym == SDLK_UP {
            if table().sel == 0 {
                unsafe { SRC_ON_PILLS = true };
            } else {
                table().move_sel(-1);
            }
        } else if sym == SDLK_DOWN {
            table().move_sel(1);
        }
        return;
    }
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
                // walk from the DRAWN index (`tool_f`), never from the raw static: a step taken
                // from a stale one moves the ring by more than one chip, or by none at all
                let f = tool_f();
                if sym == SDLK_LEFT && f > 0 {
                    TOOL_F = f - 1;
                } else if sym == SDLK_RIGHT && f + 1 < chips().len() {
                    TOOL_F = f + 1;
                } else if sym == SDLK_UP {
                    AREA = Area::Tabs;
                    TAB_F = own_pill();
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

/// What a press on strip pill `i` means for this screen. ONE function for the OK key and the
/// pointer click, which spelled the same three-way ladder out separately and identically — the
/// shape every other paired key/pointer path on this screen already avoids (`click` resolves the
/// toolbar through the same `open_chip_menu` the key does, and for the same reason).
fn tab_pill_action(i: usize) -> Action {
    match crate::ui::widgets::pill_at(i) {
        // the screen the strip leads with; leaving for it is `app.rs`'s call, not ours
        Pill::Home => Action::GoHome,
        Pill::Search => Action::GoSearch,
        // a TAB, not a section: `switch_tab` resolves it through `browse::tabs`, because several
        // libraries can share one pill
        Pill::Section(tab) => {
            switch_tab(tab);
            Action::None
        }
    }
}

pub(crate) fn on_ok() -> Action {
    // Before the `menu_open()` rung below, which subsumes this one — see [`move_focus`]'s note on
    // the same ordering. Reversed, OK on the pills would fall straight into `menu_commit`.
    if source_menu_open() && unsafe { addr_of!(SRC_ON_PILLS).read() } {
        // OK on the level you are already showing means "into its list" — the level itself is
        // switched by LEFT/RIGHT, so there is nothing here for OK to toggle. Without this it fell
        // through and committed whichever row the list had remembered.
        unsafe { SRC_ON_PILLS = false };
        return Action::None;
    }
    if menu_open() {
        menu_commit(table().sel);
        return Action::None;
    }
    match area() {
        Area::Tabs => tab_pill_action(unsafe { addr_of!(TAB_F).read() }),
        Area::Toolbar => {
            // matched on the CHIP, never on its index: the row is two chips long with one source
            // and three with two, so position is not identity
            // `None` only when the row is empty, which is the one-source failure — and then the
            // toolbar is not a band focus can be in at all (`sync_readout_focus`)
            if let Some(c) = focused_chip() {
                open_chip_menu(c);
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
        Menu::Genre { .. } => {
            open_filter_menu(true);
            return true;
        }
        // the Sources panel has no drill-in level to walk back through: its two levels are peers,
        // reached sideways, so BACK dismisses the panel from either one
        Menu::Source { .. } | Menu::Sort { .. } | Menu::Filter => {
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

/// Sort/Filter panel width — one-line rows.
const MENU_W: f32 = 470.0;
/// The Sources panel is wider **because its rows are two-line**: a library's name over its size,
/// under a machine-name header carrying an owner. 470 elides all three.
const SRC_W: f32 = 640.0;
/// The level pills are the same control height as every other pill in the app (the 60px
/// circle-button family `widgets::CHIP_D` names).
const SRC_PILL_H: f32 = crate::ui::widgets::CHIP_D;
/// The Sources panel's level-switch band: the pill row with a `space` rung of air above and below
/// it. The band is a CONSTANT, and both levels list the same libraries and end with the same
/// separator + recheck row, so **the panel does not resize when the level swaps**.
/// The level band: the pill row plus the air above and below it.
///
/// **`space::SM` above, and below it exactly enough that the pill row and the first list row sit one
/// `space::MD` rung apart.** The list brings its own `TableView::PAD_V/2` top padding, so a band
/// that also spent a full rung underneath double-padded the seam — pill bottom to first row measured
/// 16 + 20 = 36px, which is not a rung at all and read as a gap nobody chose. The subtraction is the
/// opposite of a hand-tuned offset: it is what makes the SEAM land on the scale, and it stays right
/// if either constant moves.
const SRC_LEVEL_H: f32 =
    SRC_PILL_H + theme::space::SM + (theme::space::MD - crate::ui::table::TOP_PAD);

fn panel_rect() -> Rect {
    let (w, band) = if source_menu_open() { (SRC_W, SRC_LEVEL_H) } else { (MENU_W, 0.0) };
    let ph = (table().measured_height() + band).clamp(120.0, SCR_H - 280.0);
    Rect::new(MARGIN_X, TOOL_Y + TOOL_H + 18.0, w, ph)
}
/// The LIST's frame inside the panel — under the level pills when the Sources panel is open, the
/// whole panel otherwise. Every scroll, hit-test and draw reads this one function, so the three
/// cannot disagree about where the rows are.
fn table_rect() -> Rect {
    let r = panel_rect();
    if source_menu_open() {
        Rect::new(r.x, r.y + SRC_LEVEL_H, r.w, r.h - SRC_LEVEL_H)
    } else {
        r
    }
}
/// The two level pills' frames, left to right, inside `panel`. Laid out from the labels so the
/// pills fit their words, and started on the same line the rows' content starts on.
fn src_pill_rects(panel: Rect) -> [Rect; 2] {
    let sz = theme::size::BODY;
    let x0 = panel.x + crate::ui::table::CONTENT_X;
    let w0 = crate::ui::widgets::TabPill::width("Browse".len(), sz);
    let w1 = crate::ui::widgets::TabPill::width("On Home".len(), sz);
    let y = panel.y + theme::space::SM;
    [
        Rect::new(x0, y, w0, SRC_PILL_H),
        Rect::new(x0 + w0 + theme::space::SM, y, w1, SRC_PILL_H),
    ]
}
/// Dismiss the open menu. **The key goes with it** — it lives in the [`Menu`] variant, so
/// `Menu::None` is not a flag sitting beside a row count and an action list that describe a panel
/// nobody can see any more; it is the absence of them.
fn close_menu() {
    set_menu(Menu::None);
    pop().close();
}

/// **Has the data under the OPEN menu moved since it was built?** The rebuild-on-landing test,
/// split out of [`update`] as a value so it can be graded on the host — the rebuild itself goes
/// through `TableView`, which measures text.
///
/// Every arm compares LIKE WITH LIKE, which is the whole content of this function: a menu's key is
/// whatever it recorded at build time and nothing else. Sort and Genre record the length of the
/// list their rows are; the Sources panel records the section-table GENERATION, because its rows
/// are a kind-filtered projection whose count matches no other count in the app (see
/// [`Menu::Source`]'s `gen` for the frame-per-frame rebuild that cost). `Filter` is static rows
/// built from `view_*`, so it has no landing to wait for, carries no key and is never stale.
///
/// Reading the key off the OPEN variant is what makes "like with like" structural: there is no way
/// to reach Sort's count while the Sources panel is up, because while it is up that count does not
/// exist.
fn menu_stale() -> bool {
    match menu() {
        Menu::Sort { built } => *built != crate::browse::sorts().len(),
        Menu::Genre { built } => *built != crate::browse::genres().len(),
        Menu::Source { gen, .. } => *gen != crate::browse::sections_gen(),
        Menu::Filter | Menu::None => false,
    }
}

// ---- the Sources list ------------------------------------------------------------------------

/// THE ROW MODEL of the two-level Sources panel, pure over its inputs so it is host-testable
/// without a live section table (which is also why `browse` hands out owned [`SrcGroup`]/[`SrcRow`]
/// projections rather than borrows of its statics).
///
/// The levels never mirror each other's marks. **Browse** is a picker: one tick, on the library you
/// are looking at, and no words. **On Home** is a set of switches: every row states its value as
/// the word `On`/`Off` at the trailing edge, and no ticks. A mark says where you are, a word says
/// what is set, and no row is allowed to say both.
///
/// Returns the sections beside one [`SrcAction`] per global row index — the separator included,
/// which does nothing but still occupies an index.
fn source_sections(
    level: Level,
    groups: &[crate::browse::SrcGroup],
    rows: &[crate::browse::SrcRow],
) -> (Vec<Section>, Vec<SrcAction>) {
    let mut out: Vec<Section> = Vec::new();
    let mut acts: Vec<SrcAction> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let mine = rows.iter().filter(|r| r.src == gi);
        // a server whose libraries we have never learned contributes no group at all — a header
        // over nothing reads as broken, and D's answer for a source that cannot be reached on a
        // FIRST run is absence
        if mine.clone().next().is_none() {
            continue;
        }
        // the header is the MACHINE, its accessory the PERSON — the one place in the app a machine
        // is named, and the reason every other surface can say only the handle. An unreachable
        // group also SAYS so there, and says it FIRST: the accessory is elided from the right, so
        // leading with the state means a long handle gives way rather than the fact that the server
        // is not answering. It is a state in the same register as the rows' own `On`/`Off`.
        let accessory = match (g.reachable, g.handle.is_empty()) {
            (true, _) => g.handle.clone(),
            (false, true) => "Not reachable".to_string(),
            (false, false) => format!("Not reachable \u{b7} {}", g.handle),
        };
        let mut sec = Section::new(g.name.clone()).accessory(accessory).dim(!g.reachable);
        for r in mine {
            let mut row = Row::new(r.title.clone());
            row = match level {
                Level::Browse => row.checked(r.current).detail(r.count_line.clone()),
                Level::OnHome => row
                    .toggle(r.pinned)
                    // the LAST pinned library: the value dims, the label keeps live ink, and the
                    // sub-line states the rule. Not the whole row — dim means unavailable, and this
                    // is the library that works.
                    .value_dim(r.last_pinned)
                    .detail(if r.last_pinned {
                        "Home needs one library".to_string()
                    } else {
                        r.count_line.clone()
                    }),
            };
            acts.push(SrcAction::Library(r.section));
            sec = sec.row(row);
        }
        out.push(sec);
    }
    // …and the one row that is not a library, last, under a separator. It rides BOTH levels, which
    // is what keeps their row counts — and therefore the panel's height — identical.
    if let Some(last) = out.last_mut() {
        last.rows.push(Row::separator());
        acts.push(SrcAction::None);
        // no leading glyph, deliberately: on the Browse level that column carries the picker's
        // tick, and an action mark in it would be a second grammar for one column
        last.rows.push(Row::new("Check for new shares"));
        acts.push(SrcAction::Recheck);
    }
    (out, acts)
}

/// Where the cursor sits. On OPEN it lands on the library you are browsing; on a rebuild — a level
/// swap, a pin flip, a source's libraries landing — it HOLDS, because the two levels list the same
/// rows in the same order and the design's whole claim about the swap is that nothing moves.
fn source_sel(rows: &[crate::browse::SrcRow], keep: Option<i32>) -> i32 {
    keep.unwrap_or_else(|| rows.iter().position(|r| r.current).map(|i| i as i32).unwrap_or(0))
}

/// Build (or rebuild in place) the Sources panel at the current level.
fn build_source_menu(keep: bool) {
    let level = unsafe { addr_of!(SRC_LEVEL).read() };
    // READ THE GENERATION FIRST, before the projections it stamps. `source_groups`/`source_rows`
    // only read, so nothing can move between here and the build today — but recording it after
    // would silently adopt a landing this panel was not built from, and that is a rebuild lost
    // rather than one too many.
    let table_gen = crate::browse::sections_gen();
    let groups = crate::browse::source_groups();
    let rows = crate::browse::source_rows();
    let (secs, acts) = source_sections(level, &groups, &rows);
    let sel = source_sel(&rows, keep.then(|| table().sel));
    // the panel IS its key: the generation these rows were projected from and one action per row,
    // installed together and dropped together by [`close_menu`]
    set_menu(Menu::Source { gen: table_gen, acts });
    table().compact = false; // two-line rows: HEADLINE titles over CAPTION sub-lines
    // `slide` = `keep`, and that is the level swap's whole promise kept in one argument: a rebuild
    // GLIDES (so the panel's scroll and highlight hold still while the words under them change),
    // an OPEN snaps to the current library with the list scrolled to the top.
    table().set_sections(secs, sel, keep);
    // the list holds focus for as long as the pills do not; an OPEN always starts in the list
    table().list_focused = unsafe { !addr_of!(SRC_ON_PILLS).read() };
    if !keep {
        unsafe { SRC_ON_PILLS = false };
        table().list_focused = true;
        pop().open();
    }
}

/// Open the Sources list. **Browse opens first, every time** — picking is the frequent act and
/// pinning is a thing you do once per friend, so it is one press further in and never in the way.
fn open_source_menu() {
    unsafe { SRC_LEVEL = Level::Browse };
    build_source_menu(false);
}

/// Swap the level under the pills. A no-op for the level already showing, so holding LEFT does not
/// rebuild the list 60 times a second.
fn set_source_level(l: Level) {
    if unsafe { addr_of!(SRC_LEVEL).read() } == l {
        return;
    }
    unsafe { SRC_LEVEL = l };
    build_source_menu(true);
}

/// What the row at global index `sel` of the OPEN Sources panel does.
///
/// The list is read out of the open variant, so this can only ever answer for the panel that is on
/// screen — with any other menu up there is no list to index, rather than a leftover one that
/// happens to be indexable. The answer is COPIED out because every rebuild assigns `MENU` and drops
/// the list it was read from ([`build_source_menu`]).
fn src_action(sel: i32) -> SrcAction {
    match menu() {
        Menu::Source { acts, .. } => {
            usize::try_from(sel).ok().and_then(|i| acts.get(i)).copied().unwrap_or(SrcAction::None)
        }
        _ => SrcAction::None,
    }
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
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], crate::browse::sort_idx() as i32, keep);
    set_menu(Menu::Sort { built: sorts.len() });
    if !keep {
        pop().open();
    }
}

/// `keep` = re-entered from the Genre level via BACK (slide the pill, don't restart the fade).
/// Does the section being browsed answer "unwatched"? Only movies and shows have that state, so on
/// a music or photo library the switch is not offered rather than offered and inert — `browse`
/// refuses to put the filter on such a query, and a control that visibly changes nothing is a worse
/// answer than one that is not there. It also shifts the Genre row up, which is why every index in
/// this menu is resolved through [`filter_rows`] rather than written as a literal.
fn has_unwatched() -> bool {
    matches!(
        crate::browse::section_kind(crate::browse::cur()),
        Some(crate::browse::SecKind::Movie) | Some(crate::browse::SecKind::Show)
    )
}

/// The Filter menu's row map: which row index means what, for the section being browsed.
fn filter_rows() -> (i32, i32) {
    if has_unwatched() {
        (0, 1) // unwatched, genre
    } else {
        (-1, 0) // no switch; genre leads
    }
}

fn open_filter_menu(keep: bool) {
    let mut sec = Section::new("Filter");
    if has_unwatched() {
        // A SWITCH, not one option among several: it says what it is set to in words at the trailing
        // edge, and carries no mark at all in the leading column.
        sec = sec.row(Row::new("Unwatched only").toggle(view_unwatched()));
    }
    let sec = sec.row(
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
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    set_menu(Menu::Filter); // static rows, so no key: there is no landing to be stale against
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
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    set_menu(Menu::Genre { built: genres.len() });
    if !keep {
        pop().open();
    }
}

fn menu_commit(sel: i32) {
    match menu() {
        Menu::Source { .. } => match src_action(sel) {
            SrcAction::Library(s) => match unsafe { addr_of!(SRC_LEVEL).read() } {
                // a pick, so the panel closes and the grid dissolves under it — exactly as a Sort
                // pick does. It moves the TAB with the library when the two disagree, which is what
                // makes one list the whole map.
                Level::Browse => {
                    switch_to_section(s);
                    close_menu();
                }
                // a toggle, so the panel STAYS OPEN: the word flips under the focus fill and
                // nothing moves. A refused flip (the last pinned library) changes nothing at all —
                // no flash, no toast, and the row keeps its focus.
                Level::OnHome => {
                    if crate::browse::toggle_pin(s) {
                        build_source_menu(true);
                    }
                }
            },
            SrcAction::Recheck => {
                crate::browse::recheck_shares();
                build_source_menu(true);
            }
            SrcAction::None => {}
        },
        Menu::Sort { built } => {
            // gate on the row set the TABLE was built with, not the live sorts() — OK on the
            // "Loading…" row must be a no-op, never a hidden direction toggle. Copied out of the
            // variant first: `close_menu` below drops the menu this key belongs to.
            let built = *built;
            if built > 0 && sel >= 0 && (sel as usize) < built {
                request(Pending::Sort(sel as usize));
            }
            close_menu(); // the panel closes NOW; the grid dissolves under it
        }
        Menu::Filter => {
            let (unwatched_row, genre_row) = filter_rows();
            if sel == unwatched_row {
                request(Pending::Unwatched(!view_unwatched()));
                open_filter_menu(true); // rebuilt from `view_unwatched()` — the check flips at once
                table().sel = unwatched_row;
            } else if sel == genre_row {
                build_genre_menu(false);
            }
        }
        Menu::Genre { .. } => {
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
        if src_pill_at(mx, my).is_some() {
            // hover moves FOCUS to the pill row and nothing more. Switching the level here would
            // rebuild the list under the pointer on a mouse move — the same class of bug as the
            // tab-pill hover that used to park app focus on a pill (`tools/stream-screen.py`).
            unsafe { SRC_ON_PILLS = true };
            return;
        }
        if let Some(gi) = table().hit_row(table_rect(), mx, my) {
            table().sel = gi;
            unsafe { SRC_ON_PILLS = false };
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
        // `w > 0.5` rather than a slice by the row's length: a chip that is not drawn parks its
        // rect, so the geometry itself says what is hittable and the scan cannot outlive a shorter
        // row (the failure read-out drops Sort and Filter — see `chips`).
        if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
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
        if let Some(i) = src_pill_at(mx, my) {
            unsafe { SRC_ON_PILLS = true };
            set_source_level(if i == 0 { Level::Browse } else { Level::OnHome });
        } else if let Some(gi) = table().hit_row(table_rect(), mx, my) {
            table().sel = gi;
            unsafe { SRC_ON_PILLS = false };
            menu_commit(gi);
        } else if !panel_rect().contains(mx, my) {
            // inside the panel but on no row (the level band's dead space) is not a dismissal
            close_menu();
        }
        return Action::None;
    }
    pointer_focus(mx, my);
    if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
        return tab_pill_action(i);
    }
    let tools = unsafe { addr_of!(TOOL_RECTS).read() };
    if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
        // resolved through the CHIP the click landed on, exactly as the key path does. It used to
        // defer to `on_ok`, which reads the FOCUSED chip — `pointer_focus` above has just moved
        // focus there, so the two agreed by side effect rather than by construction.
        // `get`, not an index: the row can be EMPTY now (a one-source failure drops every chip it
        // has), and `saturating_sub(1)` would then index an empty slice. Unreachable today because
        // an undrawn chip parks its rect and the scan above tests `w > 0.5` — which is exactly the
        // kind of "safe by a fact somewhere else" that stops being true when the row changes again.
        if let Some(&c) = chips().get(i) {
            open_chip_menu(c);
        }
        return Action::None;
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
/// The Source chip's owner annotation sits one rung down, on `MICRO` — the scale's kicker rung,
/// which exists for exactly this: a one-line de-emphasised run beside a value, never for content.
const NOTE_SZ: c_int = theme::size::MICRO;
/// …at 62% of the label's OWN ink. A weight, not a second colour role, so the annotation reads the
/// same step down whether the chip is idle (light ink on a dark capsule) or focused (dark ink on
/// the accent), where a fixed grey would go muddy.
const NOTE_INK_A: f32 = 0.62;

/// The toolbar chips' strings + measured widths, cached until anything they draw changes.
fn chip_strs() -> &'static [ChipStrs] {
    // the VIEW labels, not the committed ones: a chip must relabel on the press frame, not 70 ms
    // later. The key is every drawn run, so it invalidates on the QUEUE and then again on the
    // commit only if the resolved label actually differs (by construction it doesn't).
    let row = chips();
    // the QUEUED section's owner, not the committed one — the chip's name already comes from
    // `view_section()`, and resolving the handle from `cur()` would draw a friend's library name
    // with no owner beside it for the 70 ms of the fade and then pop the run in, changing the
    // chip's measured width mid-transition
    let handle = crate::browse::handle_of(view_section());
    let key = row.iter().fold(String::new(), |mut k, &c| {
        let (n, v) = chip_value(c);
        k.push_str(n);
        k.push('\u{1}');
        k.push_str(&v);
        k.push('\u{1}');
        k
    }) + handle;
    let cache = unsafe { &mut *addr_of_mut!(CHIP_CACHE) };
    if cache.as_ref().map(|(k, _)| *k != key).unwrap_or(true) {
        let built = row
            .iter()
            .map(|&c| {
                let (name, value) = chip_value(c);
                let sz = theme::size::LABEL;
                let name_c = CString::new(name).unwrap_or_default();
                let val_c = CString::new(format!(" \u{b7} {value}")).unwrap_or_default();
                // The owner annotation rides ONLY the Source chip, and only for someone else's
                // library. `handle` is "" on your own, so this is `None` there — absent, not empty.
                let note = (c == Chip::Source && !handle.is_empty())
                    .then(|| CString::new(format!("  {handle}")).unwrap_or_default());
                let nw = crate::text::text_width(name_c.as_ptr(), sz, 0);
                let vw = crate::text::text_width(val_c.as_ptr(), sz, 1);
                let hw = note.as_ref().map(|h| crate::text::text_width(h.as_ptr(), NOTE_SZ, 0)).unwrap_or(0.0);
                let w = CHIP_PAD + nw + vw + hw + 10.0 + CHIP_ISZ + CHIP_PAD;
                ChipStrs { name: name_c, val: val_c, note, w }
            })
            .collect();
        *cache = Some((key, built));
    }
    &cache.as_ref().unwrap().1
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
    // the owner annotation, on the VALUE's baseline so the two runs sit on one line despite the
    // rung change (layout ≠ paint). Absent entirely on your own libraries.
    if let Some(h) = &cs.note {
        let hy = crate::text::baseline_y(NOTE_SZ, 0, sz, 1, ty);
        tx += p.text(h.as_ptr(), tx, hy, NOTE_SZ, theme::with_a(bright_ink, NOTE_INK_A), 0, 0);
    }
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
            let (_, _, verdict, reason) = dead_strs();
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
    // The SHARED navigation-bar scrim — this screen's own three bands, hoisted into
    // `widgets::nav_scrim` so Search draws the identical treatment instead of a second copy of it.
    // The numbers are unchanged: that function derives `170 / 188` from `GRID_TOP` by the design
    // system's own shape (`top − 44` and `top − 26`), which is what these literals always were.
    // The fade starts BELOW the toolbar, not 44px above the grid: at the old fixed length it began
    // at 170, sixteen pixels above the chips' own bottom edge, so a chip's lower half sat on ground
    // that was already fading out from under it — visible on the panel. `TOOL_Y + TOOL_H` is the
    // lowest thing this bar draws, which is exactly what the shared element wants.
    crate::ui::widgets::nav_scrim(p, TOOL_Y + TOOL_H, GRID_TOP, sc);
    // the Library screen has no chip focus stop (its top-band focus is the tab row), so the chip
    // never unfurls here — expand 0.
    let cd = crate::ui::widgets::CHIP_D;
    crate::ui::widgets::profile_chip(pk, Rect::new(MARGIN_X, TOP_BAR_Y, cd, cd), 0.0);
    crate::ui::widgets::draw_tab_row(pk);

    // the focused index comes from `tool_f` — the same clamped authority `focused_chip` reads, so
    // the chip wearing the ring and the chip OK activates are one chip
    let tool_i = tool_f();
    let tool_focus = |i: usize| area() == Area::Toolbar && !menu_open() && tool_i == i;
    let strs = chip_strs();
    let mut x = MARGIN_X;
    let mut rects = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
    for (i, cs) in strs.iter().enumerate() {
        // every chip on this toolbar OPENS A MENU, which is why the trailing glyph is always a
        // chevron — including Source's
        let r = toolbar_chip(p, x, cs, Icon::ChevronDown, tool_focus(i));
        rects[i] = r;
        x = r.x + r.w + 20.0;
    }
    unsafe { *addr_of_mut!(TOOL_RECTS) = rects };

    // Item count, right-aligned on the toolbar line (cached — changes only on re-query). It counts
    // what the GRID holds, so it goes with the grid: absent on the failure (the design's rule —
    // there is nothing to count) and absent beside the empty read-out too, which already says the
    // same thing in words and does not need "0 films" agreeing with it.
    let tot = crate::browse::total();
    if tot >= 0 && read == Readout::Grid {
        let noun = crate::browse::section_kind(crate::browse::cur()).map(|k| k.noun()).unwrap_or("items");
        let cache = unsafe { &mut *addr_of_mut!(COUNT_C) };
        if cache.as_ref().map(|(ct, cn, _)| *ct != tot || *cn != noun).unwrap_or(true) {
            *cache = Some((tot, noun, CString::new(format!("{tot} {noun}")).unwrap_or_default()));
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
        // Split into TWO phases on purpose (`/tmp/plxnative-profile`): an open menu costs this
        // screen ~2× its draw — measured `draw` 16 → 33-42 ms with the Sort panel up, which is
        // the whole of the frame (`pump=0.0 swap=0.4 up=0`) and lands it either side of the vsync
        // boundary, so frames alternate 16/33 and the rate reads 23-29. The two candidates are a
        // full-screen alpha quad (`Popover::painter`'s scrim: 2.07M fragments over an
        // already-drawn grid) and the panel's own list, and they are separable only by measuring
        // them apart. Zero-overhead with the trigger off.
        use crate::ui::profile::phase;
        let mp = phase("menu.scrim", || pop().painter(0.45, 20.0));
        phase("menu.panel", || {
            let r = panel_rect();
            mp.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
            if source_menu_open() {
                draw_level_pills(mp, r);
            }
            // ONE focus in the panel: the list draws its selection pill only while the level pills
            // do not hold focus. Every other menu has no control outside its list, so this is
            // always true for them.
            table().list_focused = !source_menu_open() || unsafe { !addr_of!(SRC_ON_PILLS).read() };
            table().draw(mp, table_rect());
        });
    }
}

/// The Sources panel's level switch: two segmented pills at the panel top, the shared
/// [`TabPill`](crate::ui::widgets::TabPill) in its boolean `segment` model — a selected segment is
/// a subtle pill and an unselected one is dim text with no pill. That model exists for a segmented
/// control that is NOT in a strip, which is exactly this: two fixed segments with nothing to travel
/// between.
///
/// **Exactly one focus in the panel.** The pills are a focus stop, and while they hold it the table
/// draws no selection pill at all ([`TableView::list_focused`]) — that pairing is the fix for the
/// owner-reported "two items are at focus", where the pill row took the bright capsule while the
/// table went on painting its selection, which a table always did.
fn draw_level_pills(p: Painter, panel: Rect) {
    use crate::ui::widgets::TabPill;
    let rects = src_pill_rects(panel);
    unsafe { *addr_of_mut!(SRC_PILL_RECTS) = rects };
    let level = unsafe { addr_of!(SRC_LEVEL).read() };
    let on_pills = unsafe { addr_of!(SRC_ON_PILLS).read() };
    // **The shared strip, so these capsules TRAVEL like every other tab row in the app.** They were
    // per-pill `segment`/`focused` fills, which is the standalone model — the capsules that mark
    // selection and focus were repainted onto a new pill instead of sliding to it, so this was the
    // one tab row in the product that did not animate (owner-reported). `TabStrip` owns both
    // capsules and their springs; the pills keep only their label and ink, which is exactly the
    // division `TabPill::mix` exists for.
    //
    // `span` reads the SAME `rects` the pills are drawn from, which is the strip's own requirement:
    // a capsule can then never land off a pill.
    unsafe { (*addr_of!(SRC_STRIP)).draw(p, rects[0].y, SRC_PILL_H, false) };
    for (i, (label, lv)) in [(c"Browse", Level::Browse), (c"On Home", Level::OnHome)].into_iter().enumerate() {
        let selected = if level == lv { 1.0 } else { 0.0 };
        TabPill::new(label.as_ptr(), theme::size::BODY, rects[i])
            .mix(if on_pills { selected } else { 0.0 }, selected)
            .draw(&Env::inert(), p);
    }
}

/// The level pill under the pointer, or None (rects recorded at draw; stale while closed, which is
/// why every caller is already inside a `menu_open()` branch).
fn src_pill_at(mx: f32, my: f32) -> Option<usize> {
    if !source_menu_open() {
        return None;
    }
    let rects = unsafe { addr_of!(SRC_PILL_RECTS).read() };
    rects.iter().position(|r| r.w > 0.5 && r.contains(mx, my))
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
    // pulled in from the panel edge. More letters than fit that band SCROLL — the pitch is never
    // divided down to make them fit, see [`RAIL_PITCH`].
    let (y0, cx, win_h, max_scroll) = rail_geom(n);
    let scroll = unsafe { (*addr_of!(RAIL_SCROLL)).pos };
    // a slim capsule track behind the letters — the tab-track family, minimized: makes the
    // rail read as a CONTROL to discover, not stray typography
    let track = Rect::new(cx - 22.0, y0 - 10.0, 44.0, win_h + 20.0);
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
        // a different section's rail opens at the top rather than sliding back from wherever the
        // last one was left (change-guarded inside `jump`, so this costs no frames)
        unsafe { (*addr_of_mut!(RAIL_SCROLL)).jump(0.0) };
    }
    let labels = &lcache.as_ref().unwrap().2;
    let scrolls = max_scroll > 0.5;
    if scrolls {
        // belt and braces under the edge fade: a glyph reaches α0 before it reaches the window
        // edge, so this only catches the rounding — but without it a letter could paint over the
        // capsule's rounded cap while the spring is mid-flight.
        p.clip(Rect::new(cx - 22.0, y0, 44.0, win_h));
    }
    for (i, label) in labels.iter().enumerate() {
        let cy = y0 + (i as f32 + 0.5) * RAIL_PITCH - scroll;
        // one letter of fade at each end that has more beyond it: the rail says "there is more
        // alphabet this way" without a scrollbar, and letters leave rather than pop.
        let rel = cy - y0;
        let mut a = 1.0f32;
        if scroll > 0.5 {
            a = a.min(rel / RAIL_PITCH);
        }
        if scroll < max_scroll - 0.5 {
            a = a.min((win_h - rel) / RAIL_PITCH);
        }
        let a = a.clamp(0.0, 1.0);
        // The pointer target is the SLOT, not the disc: a 38 px box at a 34 px pitch overlapped
        // its neighbour, and `rail_at` takes the FIRST match, so the top of every letter clicked
        // the letter above it. Scrolled-away letters keep an empty rect — `Rect::contains` is
        // false on it — so a click can only reach a letter that is actually legible.
        unsafe {
            (*addr_of_mut!(RAIL_RECTS))[i] = if a > 0.5 {
                Rect::new(cx - 22.0, cy - RAIL_PITCH * 0.5, 44.0, RAIL_PITCH)
            } else {
                Rect::new(0.0, 0.0, 0.0, 0.0)
            }
        };
        if a <= 0.0 {
            continue; // off the window entirely — the cull, since `n` can be twice what fits
        }
        let q = p.alpha(a);
        let d = 38.0f32;
        let r = Rect::new(cx - d * 0.5, cy - d * 0.5, d, d);
        let focused = in_rail && i == unsafe { addr_of!(RAIL_F).read() };
        // idle letters at SECONDARY (the review put TERTIARY below the couch floor here);
        // the current-position letter reads PRIMARY, the focused one is a snow disc
        let ink = if focused {
            q.rect(r, d * 0.5, crate::ui::ACCENT, crate::ui::ACCENT, 0.0);
            crate::ui::ACCENT_INK
        } else if i == cur_letter {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
        q.text(label.as_ptr(), cx, ty, theme::size::CAPTION, ink, 1, 1);
    }
    if scrolls {
        p.clip_clear();
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
            if crate::browse::tab_count() > 1 {
                switch_tab(1);
            }
        }
        1 => switch_tab(0),
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

    /// Take the open menu OUT, leaving `Menu::None` — the save half of a save/restore around a test
    /// that installs one. [`Menu`] owns a `Vec`, so it cannot be read out by copy the way the
    /// screen's other statics can: a bitwise read would hand back a second owner of the same
    /// allocation and both would free it.
    fn take_menu() -> Menu {
        std::mem::replace(unsafe { &mut *addr_of_mut!(MENU) }, Menu::None)
    }
    fn level() -> Level {
        unsafe { addr_of!(SRC_LEVEL).read() }
    }
    fn on_pills() -> bool {
        unsafe { addr_of!(SRC_ON_PILLS).read() }
    }
    /// How many rows the OPEN Sources panel is holding an action for — 0 for every other state,
    /// since the list lives in the variant.
    fn src_acts_len() -> usize {
        match menu() {
            Menu::Source { acts, .. } => acts.len(),
            _ => 0,
        }
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
        let (area0, tab0) = unsafe { (addr_of!(AREA).read(), addr_of!(TAB_F).read()) };
        let menu0 = take_menu();

        unsafe { AREA = Area::Tabs; TAB_F = 3 };
        set_menu(Menu::None);
        assert_eq!(focused_pill(), Some(3), "the row holds focus — that pill crosses to Home");

        set_menu(Menu::Sort { built: 0 });
        assert_eq!(focused_pill(), None, "a menu owns the frame; the row underneath is not focused");
        set_menu(Menu::None);

        for a in [Area::Grid, Area::Toolbar, Area::Rail, Area::Status] {
            unsafe { AREA = a };
            assert_eq!(focused_pill(), None, "focus is not on the tab row");
        }

        unsafe { AREA = area0; TAB_F = tab0 };
        set_menu(menu0);
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
        // and it does not depend on which `unwrap_or` default the absent section hands back
        for f in [SecFetch::Loading, SecFetch::Ready, SecFetch::Failed] {
            assert_eq!(readout_of(SecFetch::Failed, 0, f, -1), Readout::Failed);
        }
    }

    /// The decision the multi-source table forced, stated as a test because it is a trade rather
    /// than a mechanism. A source that stops answering KEEPS the sections it had learned and can
    /// still have a page store full of items — so "this source is gone" stopped implying "there is
    /// nothing of it on screen". The read-out takes the region only when BOTH are true; with items
    /// resident the grid stands, exactly as it does for a failed page one layer down.
    #[test]
    fn an_unreachable_source_with_items_still_resident_keeps_its_grid() {
        assert_eq!(readout_of(SecFetch::Failed, 3, SecFetch::Failed, 185), Readout::Grid);
        assert_eq!(readout_of(SecFetch::Failed, 3, SecFetch::Failed, -1), Readout::Failed, "…and takes it when there is nothing");
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

    /// The read-out is drawn beside NO GRID CONTROL — Sort, Filter, the count and the A–Z rail all
    /// act on a grid that has no items. The Source chip is the exception, and it is the design's
    /// own: it is NAVIGATION, it is how you leave a dead source, and it is why the read-out spends
    /// no line telling you the way out. `chips()` is the ONE predicate behind the draw, the pointer
    /// scan and the vertical walk, so this pins all three at once.
    #[test]
    fn the_failure_read_out_keeps_navigation_and_drops_every_grid_control() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        // ONE source: an ordinary listing carries Sort + Filter and no Source chip at all
        crate::browse::seed_sources_for_test(1, true);
        assert_eq!(chips(), &CHIPS_ONE[..], "one source draws no Source chip");
        crate::browse::seed_sources_for_test(1, false);
        assert_eq!(readout(), Readout::Failed);
        assert!(chips().is_empty(), "there is nothing to sort, to filter or to count");

        // TWO sources: the Source chip leads the row, and it OUTLIVES the failure
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!(chips(), &CHIPS_MANY[..], "two sources put Source at the head of the row");
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(readout(), Readout::Failed);
        assert_eq!(chips(), &CHIPS_SOURCE_ONLY[..], "the way OUT of a dead source stays drawn");
        crate::browse::reset();
    }

    // ---- the Sources chip + its two-level panel -------------------------------------------------

    fn group(name: &str, handle: &str, reachable: bool) -> crate::browse::SrcGroup {
        crate::browse::SrcGroup { name: name.into(), handle: handle.into(), reachable }
    }
    fn lib(src: usize, section: usize, title: &str, pinned: bool, last: bool, current: bool) -> crate::browse::SrcRow {
        crate::browse::SrcRow {
            src,
            section,
            title: title.into(),
            count_line: "26 films".into(),
            pinned,
            last_pinned: last,
            current,
        }
    }
    /// Our own server with two libraries and a friend's with one — the canvas's own shape.
    fn two_sources() -> (Vec<crate::browse::SrcGroup>, Vec<crate::browse::SrcRow>) {
        (
            vec![group("mac-mini", "", true), group("nas-home", "friend", true)],
            vec![
                lib(0, 0, "Movies", true, false, true),
                lib(0, 1, "TV Shows", true, false, false),
                lib(1, 2, "Film Club", false, false, false),
            ],
        )
    }
    /// The LIBRARY rows, in order, as (has a tick, its trailing word) — `n` of them, which drops
    /// the trailing "Check for new shares" action from the marks under test.
    fn marks(secs: &[Section], n: usize) -> Vec<(bool, Option<String>)> {
        secs.iter()
            .flat_map(|s| s.rows.iter())
            .filter(|r| !r.sep)
            .take(n)
            .map(|r| (r.checked, r.value.clone().or_else(|| r.toggle.map(|o| if o { "On" } else { "Off" }.to_string()))))
            .collect()
    }

    /// THE row model, and the rule that makes the two levels readable as one panel: **a mark says
    /// where you are, a word says what is set, and no row says both.** Browse draws one tick and no
    /// words; On Home draws every row's word and no ticks. Neither level mirrors the other's marks.
    #[test]
    fn the_two_levels_never_mirror_each_others_marks() {
        let (g, r) = two_sources();

        let (browse, _) = source_sections(Level::Browse, &g, &r);
        let m = marks(&browse, r.len());
        assert_eq!(m.iter().filter(|(t, _)| *t).count(), 1, "exactly one tick — the library you are on");
        assert!(m[0].0, "…and it is on the current library");
        assert!(m.iter().all(|(_, w)| w.is_none()), "Browse states no values: no pins are drawn here");

        let (home, _) = source_sections(Level::OnHome, &g, &r);
        let m = marks(&home, r.len());
        assert!(m.iter().all(|(t, _)| !t), "On Home draws no ticks — it is not a picker");
        assert_eq!(
            m.iter().map(|(_, w)| w.as_deref().unwrap_or("")).collect::<Vec<_>>(),
            vec!["On", "On", "Off"],
            "every row reads its value as a word at the trailing edge"
        );
    }

    /// A server is a GROUP — the machine name as its header, the person as the header's accessory,
    /// which is the one place in the app a machine is named. An unreachable one dims WHOLE (header
    /// included) and its rows still read `On`, because nothing was unpinned; hiding it would read
    /// as a revoked share.
    #[test]
    fn a_server_is_a_group_and_an_unreachable_one_dims_whole_while_still_reading_on() {
        let (mut g, r) = two_sources();
        let (secs, _) = source_sections(Level::OnHome, &g, &r);
        assert_eq!(secs.len(), 2);
        assert_eq!((secs[0].header.as_str(), secs[0].accessory.as_str()), ("mac-mini", ""));
        assert_eq!((secs[1].header.as_str(), secs[1].accessory.as_str()), ("nas-home", "friend"));
        assert!(!secs[0].dim && !secs[1].dim);

        g[1].reachable = false;
        let mut r = r;
        r[2].pinned = true; // pinned AND unreachable: the state the design calls out by name
        let (secs, _) = source_sections(Level::OnHome, &g, &r);
        assert!(secs[1].dim, "the whole group dims, header included");
        assert!(!secs[0].dim, "…and only that group");
        assert_eq!(secs[1].rows[0].toggle, Some(true), "it still reads On — nothing was turned off");
        // the state leads the accessory, so the run that gives way under elision is the handle
        assert_eq!(secs[1].accessory, "Not reachable \u{b7} friend");

        // a source whose libraries we never learned contributes no group at all: a header over
        // nothing reads as broken, and absence is the answer for a share that has never answered
        let (secs, _) = source_sections(Level::Browse, &g, &r[..2]);
        assert_eq!(secs.len(), 1);
    }

    /// The last pinned library: the VALUE dims and the sub-line states the rule, while the label
    /// keeps live ink and the row keeps its focus. Not the whole row — dim means unavailable, and
    /// this is the library that works.
    #[test]
    fn the_last_pinned_library_dims_its_value_and_states_the_rule() {
        let g = vec![group("mac-mini", "", true)];
        let r = vec![lib(0, 0, "Movies", true, true, true), lib(0, 1, "TV Shows", false, false, false)];
        let (secs, _) = source_sections(Level::OnHome, &g, &r);
        let last = &secs[0].rows[0];
        assert!(last.value_dim, "the value dims");
        assert!(!last.dim, "the label keeps live ink — the row is not unavailable");
        assert_eq!(last.toggle, Some(true));
        assert_eq!(last.detail, "Home needs one library", "the sub-line says why");
        assert_eq!(secs[0].rows[1].detail, "26 films", "every other row keeps its size");
    }

    /// `Check for new shares` is the only row that is not a library: last, under a separator, and
    /// on BOTH levels — which is also what keeps their row counts, and so the panel's height,
    /// identical across the swap. Its action must line up with the row index the table reports,
    /// separator included.
    #[test]
    fn the_recheck_row_sits_last_under_a_separator_on_both_levels() {
        let (g, r) = two_sources();
        for level in [Level::Browse, Level::OnHome] {
            let (secs, acts) = source_sections(level, &g, &r);
            let rows: Vec<&Row> = secs.iter().flat_map(|s| s.rows.iter()).collect();
            assert_eq!(rows.len(), acts.len(), "one action per row index, separator included");
            assert_eq!(rows.len(), r.len() + 2, "three libraries, a separator and the recheck row");
            assert!(rows[3].sep, "the separator divides the libraries from the action");
            assert_eq!(acts[3], SrcAction::None, "…and it does nothing");
            assert_eq!(rows[4].label, "Check for new shares");
            assert_eq!(acts[4], SrcAction::Recheck);
            for (i, row) in r.iter().enumerate() {
                assert_eq!(acts[i], SrcAction::Library(row.section));
            }
        }
    }

    /// **The single most load-bearing test in this unit.** The toolbar used to match on a chip's
    /// INDEX with a `_` catch-all, so the day a chip was inserted at position 0 the whole row
    /// shifted its meaning by one — Source opened Sort, Sort and Filter both opened Filter — with
    /// nothing failing, because every index still resolved to *something*.
    ///
    /// So the mapping is asserted by IDENTITY, chip → menu, and separately that the ROW is the
    /// right chips in the right order. Insert a fourth chip at index 0 and the row assertion fails;
    /// mis-wire a chip to another's menu and the identity assertion fails; add a chip to the enum
    /// without wiring it and `open_chip_menu`'s exhaustive match stops compiling. Position is
    /// asserted nowhere, which is the property under test.
    ///
    /// Takes `testlock::serial` because the one-source half reads `source_chip_on()`, which asks
    /// `browse`'s ROSTER — a crate global another module's test seeds. Without the lock this passed
    /// only while no sibling happened to be holding a two-source table at that instant, which is a
    /// race that reports as "the Source chip is drawn on a single-server install".
    #[test]
    fn every_chip_opens_its_own_menu_by_identity_and_never_by_position() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        // Observe the single-source shape DELIBERATELY. The lock above serialises this test against
        // its siblings but does not undo them, and a sibling that seeds a two-source table leaves
        // the roster behind — so `source_chip_on()` below was reading whatever ran last. That
        // reports as "the Source chip is drawn on a single-server install", which is a real bug's
        // symptom and was not one.
        crate::browse::reset();
        let tool0 = unsafe { addr_of!(TOOL_F).read() };

        // chip → menu, one row per chip. A shifted row cannot satisfy this: it is keyed on the
        // value, not on where the value sits. Matched on the VARIANT — a menu's payload is its
        // build-time key and says nothing about which chip opens it.
        assert!(matches!(chip_menu(Chip::Source), Menu::Source { .. }));
        assert!(matches!(chip_menu(Chip::Sort), Menu::Sort { .. }));
        assert!(matches!(chip_menu(Chip::Filter), Menu::Filter));

        // the row, in drawn order, in both shapes. **Source leads when it exists** — this is the
        // assertion that fails if anyone inserts another chip at index 0.
        assert_eq!(CHIPS_MANY, [Chip::Source, Chip::Sort, Chip::Filter]);
        assert_eq!(CHIPS_ONE, [Chip::Sort, Chip::Filter]);
        // …and the same INDEX means different chips in the two shapes, which is precisely why
        // nothing is allowed to route on one
        assert_ne!(CHIPS_MANY[1], CHIPS_ONE[1]);

        // one source (the host's own state: `browse` has no roster) — Source is ABSENT, not an
        // empty slot and not a disabled chip
        assert!(!source_chip_on());
        assert_eq!(chips(), &CHIPS_ONE);
        for (i, want) in [(0, Chip::Sort), (1, Chip::Filter)] {
            unsafe { TOOL_F = i };
            assert_eq!(focused_chip(), Some(want));
        }
        // a `TOOL_F` left over from the longer row resolves to a chip that EXISTS rather than
        // panicking or naming one that does not
        unsafe { TOOL_F = 2 };
        assert_eq!(focused_chip(), Some(Chip::Filter));

        // ---- and the row SHRINKING under the focus index ------------------------------------
        //
        // The chip the toolbar DRAWS the ring on and the chip OK activates must be one chip. They
        // were not: `focused_chip` clamped and the draw's `tool_focus` compared the raw `TOOL_F`,
        // so a row that lost its grid chips drew no focused chip at all while OK still fired the
        // clamped last one — and RIGHT was dead, because it too measured a stale index against the
        // shorter row. Both now read [`tool_f`], which is why this asserts through it.
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!(chips(), &CHIPS_MANY[..]);
        unsafe { TOOL_F = 2 };
        assert_eq!(tool_f(), 2, "the user walked to the end of the long row");
        assert_eq!(focused_chip(), Some(Chip::Filter));

        // the listing fails and `chips()` collapses under the cursor, with NO input in between
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(chips(), &CHIPS_SOURCE_ONLY[..]);
        let i = tool_f();
        assert!(i < chips().len(), "the focused index is inside the row that is drawn");
        assert_eq!(chips().get(i).copied(), focused_chip(), "the ring and the press name one chip");
        assert_eq!(focused_chip(), Some(Chip::Source));
        // …and the walk measures the same row: there is nothing to the RIGHT of the only chip,
        // which is a statement about the row on screen rather than about one the ring left behind
        assert_eq!(i + 1, chips().len(), "RIGHT is at the end because the ring is on the last chip");

        // the source answers again: the row grows back and focus returns to the chip the user
        // actually walked to, rather than having been dumped at 0 while it was away
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!((tool_f(), focused_chip()), (2, Some(Chip::Filter)));

        crate::browse::reset();
        unsafe { TOOL_F = tool0 };
    }

    /// **The rebuild-on-landing guard has to CONVERGE**, and with two kinds of library it did not.
    ///
    /// The Sources panel records a key at build time and `update` rebuilds it in place when the
    /// live one differs. Its key was `source_rows().len()` — KIND-FILTERED, only the libraries of
    /// the tab's own type — compared against `browse::section_count()`, which counts every kind.
    /// On any account with both Movies and TV Shows those two are never equal, so the panel rebuilt
    /// itself (`source_groups()` + `source_rows()` String clones, then a whole `set_sections()`) on
    /// every frame it was open — in the one state this screen's instrumentation measured as
    /// fill-rate-bound. Keyed on the section-table GENERATION it converges, because a generation is
    /// not a count of anything and cannot be compared against the wrong one.
    ///
    /// It must still catch the landing it exists for, so the second half appends a library the way
    /// a discovery worker does and asserts both the rebuild and the row it brought.
    #[test]
    fn the_sources_panel_stops_rebuilding_once_the_table_stops_moving() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        let menu0 = take_menu();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();

        // THE SHAPE THAT BROKE IT: the panel lists one kind, the section table holds two.
        assert_eq!(crate::browse::section_count(), 4, "two sources, a Movies and a Shows library each");
        assert_eq!(crate::browse::source_rows().len(), 2, "…of which the panel lists the tab's kind");
        assert_ne!(
            crate::browse::source_rows().len(),
            crate::browse::section_count(),
            "the two quantities the old guard compared can never meet here"
        );

        build_source_menu(false);
        assert!(matches!(menu(), Menu::Source { .. }));
        let rows0 = src_acts_len();
        // …and it settles: every frame the panel stays open is a frame it does NOT rebuild
        for frame in 0..3 {
            assert!(!menu_stale(), "frame {frame} rebuilt a panel nothing had changed under");
        }

        // a source's libraries landing IS what this rebuild exists for, and still fires
        crate::browse::append_section_for_test(1, 5, "Film Club Extra", crate::browse::SecKind::Movie);
        assert!(menu_stale(), "a library landing under the open panel must rebuild it");
        build_source_menu(true);
        assert!(!menu_stale(), "…once, and then settle again");
        assert_eq!(
            src_acts_len(),
            rows0 + 1,
            "the rebuild is real — the new library is a row, not just a quieted guard"
        );

        // …and the key is the panel's, not the screen's: closing takes the action list with it, so
        // nothing is left behind for the next open to inherit
        close_menu();
        assert_eq!(src_acts_len(), 0, "the closed panel holds no rows to act on");
        crate::browse::reset();
        set_menu(menu0);
    }

    // ---- the input ladders, with a panel open ---------------------------------------------------
    //
    // Both ladders ([`move_focus`], [`on_ok`]) ask about the SOURCES panel before they ask whether
    // any menu is open, and the order is load-bearing rather than stylistic: `menu_open()` is
    // `!matches!(menu(), Menu::None)`, so it SUBSUMES `Menu::Source`. Tidy the two rungs into one
    // arm and the panel's whole level layer goes away without a compile error and without a changed
    // pixel until someone presses the key — LEFT/RIGHT stop reaching the pills, UP stops hopping
    // onto them, and OK on them commits a row the user is not looking at. Both ladders are called
    // from inside the SDL event loop (`app.rs`), where nothing can grade them; the order is pinned
    // here, at the functions themselves.

    /// **With the Sources panel open, LEFT/RIGHT belong to the level pills while UP/DOWN walk the
    /// rows; with a plain popover open, UP/DOWN are the whole ladder.** Asserted as one test
    /// because it is the same key at the same cursor position answering differently — which is
    /// exactly what a merged arm would flatten.
    #[test]
    fn the_sources_panel_owns_left_right_for_its_levels_and_a_plain_popover_does_not() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let menu0 = take_menu();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();

        // the panel lists the libraries of the browsed KIND across both sources (`source_rows` is
        // kind-filtered), then the separator and the recheck row
        open_source_menu();
        assert!(matches!(menu(), Menu::Source { .. }));
        assert_eq!(level(), Level::Browse, "Browse opens first, every time");
        assert!(!on_pills(), "an open lands in the LIST");
        assert_eq!(table().sel, 0, "…on the library being browsed");

        // DOWN/UP walk the rows while the list holds focus
        move_focus(SDLK_DOWN);
        assert_eq!(table().sel, 1, "DOWN steps a row");
        move_focus(SDLK_UP);
        assert_eq!((table().sel, on_pills()), (0, false), "UP steps back, still in the list");

        // UP off the FIRST row is the hop onto the level pills
        move_focus(SDLK_UP);
        assert!(on_pills(), "UP off row 0 reaches the pills");
        assert_eq!(table().sel, 0, "…and the list REMEMBERS the row it left");

        // …and up there LEFT/RIGHT are the level switch
        move_focus(SDLK_RIGHT);
        assert_eq!(level(), Level::OnHome, "RIGHT swaps the level under the pills");
        move_focus(SDLK_RIGHT);
        assert_eq!(level(), Level::OnHome, "…and holds: there is no third level to walk to");
        move_focus(SDLK_LEFT);
        assert_eq!(level(), Level::Browse);
        assert_eq!(table().sel, 0, "the swap holds the cursor — nothing under the pills moves");
        move_focus(SDLK_DOWN);
        assert!(!on_pills(), "DOWN returns to the rows");

        // ---- the same keys, against a plain popover ---------------------------------------------
        open_filter_menu(false);
        assert!(matches!(menu(), Menu::Filter));
        assert_eq!(table().n_rows(), 2, "Unwatched only + Genre, this being a movie library");
        for k in [SDLK_LEFT, SDLK_RIGHT] {
            move_focus(k);
            assert_eq!((table().sel, on_pills()), (0, false), "there is nothing sideways to walk to");
        }
        move_focus(SDLK_DOWN);
        assert_eq!(table().sel, 1, "UP/DOWN are its whole ladder");
        move_focus(SDLK_UP);
        move_focus(SDLK_UP); // back at the first row, with no pill row above it
        assert_eq!((table().sel, on_pills()), (0, false), "no other arm of this ladder hops onto pills");

        close_menu();
        crate::browse::reset();
        set_menu(menu0);
        clear();
    }

    /// **OK on the level pills goes INTO the list; it commits nothing.** The pills are switched by
    /// LEFT/RIGHT, so there is nothing there for OK to toggle — and because `menu_open()` subsumes
    /// this state, a press that fell through to the rung below would pick whichever row the list
    /// had remembered and close the panel, which is a library switch the user never asked for.
    #[test]
    fn ok_on_the_level_pills_enters_the_list_instead_of_committing_its_row() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let menu0 = take_menu();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();

        open_source_menu();
        // stand the list on a row that WOULD do something: the other source's library, which is not
        // the one being browsed, so a commit is a visible queued switch rather than a no-op
        table().sel = 1;
        assert_eq!(src_action(1), SrcAction::Library(2));
        unsafe { SRC_ON_PILLS = true };

        assert!(matches!(on_ok(), Action::None));
        assert!(!on_pills(), "OK on the level already showing means INTO its list");
        assert!(matches!(menu(), Menu::Source { .. }), "the panel is still up — nothing was picked");
        assert_eq!(pending(), Pending::None, "…and nothing was queued");
        assert_eq!(table().sel, 1, "the row the list remembered is still the row it remembers");

        // the very next press, now that the list holds focus, IS the commit — so the guard above is
        // specific to the pills rather than a menu-wide swallow
        assert!(matches!(on_ok(), Action::None));
        assert_eq!(pending(), Pending::Section(2), "OK in the list picks that library");
        assert!(matches!(menu(), Menu::None), "…and a Browse pick closes the panel");

        crate::browse::reset();
        set_menu(menu0);
        clear();
    }

    /// The whole point of the constant pitch: a Latin section and a Cyrillic one space their
    /// letters identically, and the long one scrolls instead of compressing. Both rail statics
    /// are untouched here — [`rail_geom`] and [`rail_scroll_target`] are pure, which is why they
    /// were split out of the draw.
    #[test]
    fn a_long_alphabet_scrolls_rather_than_squeezing_its_letters() {
        let (y_short, x_short, win_short, max_short) = rail_geom(9); // Movies, Latin
        let (y_long, x_long, win_long, max_long) = rail_geom(30); // a Cyrillic section

        // same origin, so the rail does not move between sections…
        assert_eq!((y_short, x_short), (y_long, x_long));
        // …the short one is exactly as tall as its letters and never scrolls…
        assert_eq!(win_short, 9.0 * RAIL_PITCH);
        assert_eq!(max_short, 0.0);
        // …and the long one fills the band and scrolls the remainder, at the SAME pitch
        assert!(max_long > 0.0, "30 letters must not fit the band at {RAIL_PITCH} px");
        assert_eq!(win_long + max_long, 30.0 * RAIL_PITCH);
        assert!(win_long <= SCR_H - GRID_TOP - 40.0);
    }

    /// The window invariant, at every letter of a section that does not fit: whatever the scroll
    /// was, one step of the reveal rule leaves the driving letter fully on screen — and inside the
    /// one-letter margin the edge fade occupies, so the letter you are on is never the faded one.
    #[test]
    fn the_reveal_rule_keeps_the_driving_letter_clear_of_both_fades() {
        const N: usize = 30;
        let (_, _, win_h, max) = rail_geom(N);
        for drive in 0..N {
            // from the top, from the bottom, and from where the previous letter left it
            for from in [0.0, max, drive as f32 * RAIL_PITCH] {
                let s = rail_scroll_target(from, drive, N);
                assert!((0.0..=max).contains(&s), "scroll {s} escaped 0..={max} at letter {drive}");
                let top = drive as f32 * RAIL_PITCH - s; // the letter's slot, window-relative
                let margin = if drive == 0 || drive + 1 == N { 0.0 } else { RAIL_PITCH };
                assert!(
                    top >= margin - 0.01 && top + RAIL_PITCH <= win_h - margin + 0.01,
                    "letter {drive} sits at {top}..{} in a {win_h} window (scroll {s} from {from})",
                    top + RAIL_PITCH,
                );
            }
        }
    }

    /// A section short enough to fit never scrolls, whichever letter drives — otherwise the rail
    /// would drift on a screen whose alphabet is entirely visible.
    #[test]
    fn a_rail_that_fits_never_scrolls() {
        for drive in 0..9 {
            assert_eq!(rail_scroll_target(0.0, drive, 9), 0.0);
        }
    }
}
