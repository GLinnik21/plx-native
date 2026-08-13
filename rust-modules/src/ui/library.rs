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
use crate::ui::label::Label;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use crate::ui::widgets::{Art, Spinner};
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
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Menu {
    None,
    /// The **Sources list** — every library on every granted server, in two levels (see [`Level`]).
    Source,
    Sort,
    Filter,
    Genre,
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
/// Chip rects for pointer hit-testing, in [`chips`] order. The row is two or three long, so the
/// unused slot stays a zero rect — and every scan tests `w > 0.5` first, because a zero rect at the
/// origin `contains` the origin.
static mut TOOL_RECTS: [Rect; 3] = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
/// Which level of the Sources panel is showing. Browse opens first, EVERY time (picking is the
/// frequent act; pinning is a thing you do once per friend), so this is not remembered across opens.
static mut SRC_LEVEL: Level = Level::Browse;
/// Focus is on the level pills rather than in the list. UP from the first row reaches them,
/// LEFT/RIGHT swaps the level under them, DOWN/OK returns to the rows.
static mut SRC_ON_PILLS: bool = false;
/// One action per global row index of the open Sources panel (see [`SrcAction`]).
static mut SRC_ACTIONS: Vec<SrcAction> = Vec::new();
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
const CHIPS_ONE: [Chip; 2] = [Chip::Sort, Chip::Filter];
const CHIPS_MANY: [Chip; 3] = [Chip::Source, Chip::Sort, Chip::Filter];
/// The chips on the toolbar, in drawn order — Source FIRST when it exists.
fn chips() -> &'static [Chip] {
    if source_chip_on() {
        &CHIPS_MANY
    } else {
        &CHIPS_ONE
    }
}
/// The chip holding toolbar focus. `TOOL_F` is clamped here rather than everywhere it is read: the
/// row's length changes the moment a second source is granted, and a stale index must resolve to a
/// chip that exists, never to one that does not.
fn focused_chip() -> Chip {
    let c = chips();
    c[unsafe { addr_of!(TOOL_F).read() }.min(c.len() - 1)]
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
#[cfg(test)]
fn chip_menu(c: Chip) -> Menu {
    match c {
        Chip::Source => Menu::Source,
        Chip::Sort => Menu::Sort,
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
// letter rail: focused letter + the per-letter rects recorded at draw (pointer hit-testing)
const MAX_LETTERS: usize = 34; // A–Z + digits/# buckets
static mut RAIL_F: usize = 0;
static mut RAIL_RECTS: [Rect; MAX_LETTERS] = [Rect::new(0.0, 0.0, 0.0, 0.0); MAX_LETTERS];
static mut RAIL_N: usize = 0;

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
    if n == 0 {
        return;
    }
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
    unsafe {
        AREA = Area::Grid;
        MENU = Menu::None;
        TOOL_F = 0;
        PENDING = Pending::None; // whatever was queued before we left has just been flushed
        EPOCH = crate::browse::query_gen(); // AFTER `set_cur` above, so our own bump isn't foreign
    }
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
    let ready = !crate::browse::loading_initial(); // the SAME `st.total` the draw loops read
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
    crate::ui::widgets::tab_row_update(crate::browse::tab_of_section(view_section()) as c_int + 1, tab_focus(), dt);
    if menu_open() {
        pop().update(dt);
        table().update(dt, table_rect().h - crate::ui::table::PAD_V);
        // async menu data landing while the popover is open — rebuild it in place (a Sort
        // menu stuck on "Loading…" whose OK secretly toggled the direction was a
        // review-confirmed bug)
        let built = unsafe { addr_of!(MENU_BUILT).read() };
        match menu() {
            Menu::Sort if built != crate::browse::sorts().len() => build_sort_menu(true),
            Menu::Genre if built != crate::browse::genres().len() => build_genre_menu(true),
            // a source's libraries landing while the list is open (its discovery is a worker) —
            // rebuilt in place, same rule as a menu's value list arriving
            Menu::Source if built != crate::browse::section_count() => build_source_menu(true),
            _ => {}
        }
    }
}

// ---- input: D-pad ---------------------------------------------------------------------------

pub(crate) fn move_focus(sym: c_uint) {
    if menu() == Menu::Source {
        // The level pills are a focus stop ABOVE the list: UP off the first row reaches them,
        // LEFT/RIGHT swaps the level under them, DOWN comes back into the rows. One control per
        // press — there is no accessory inside a row to reach sideways for.
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
                    AREA = Area::Toolbar;
                }
            }
            Area::Toolbar => {
                if sym == SDLK_LEFT && TOOL_F > 0 {
                    TOOL_F -= 1;
                } else if sym == SDLK_RIGHT && TOOL_F + 1 < chips().len() {
                    TOOL_F += 1;
                } else if sym == SDLK_UP {
                    AREA = Area::Tabs;
                    // the pill the chrome is showing, not the store's — and the pill that
                    // REPRESENTS it, which for a borrowed library is its type's
                    TAB_F = crate::browse::tab_of_section(view_section()) + 1;
                } else if sym == SDLK_DOWN && total() > 0 {
                    AREA = Area::Grid;
                }
            }
            Area::Grid => {
                let (r, c) = (GR, GC);
                if sym == SDLK_LEFT && GC > 0 {
                    GC -= 1;
                } else if sym == SDLK_RIGHT {
                    if GC + 1 < cols_in_row(GR) {
                        GC += 1;
                    } else if rail_on() {
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
    if menu() == Menu::Source && unsafe { addr_of!(SRC_ON_PILLS).read() } {
        unsafe { SRC_ON_PILLS = false }; // OK on the level you are already showing = into its list
        return Action::None;
    }
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
            switch_tab(f - 1);
            Action::None
        }
        Area::Toolbar => {
            // matched on the CHIP, never on its index: the row is two chips long with one source
            // and three with two, so position is not identity
            open_chip_menu(focused_chip());
            Action::None
        }
        Area::Grid => Action::Card,
        Area::Rail => {
            unsafe { AREA = Area::Grid }; // OK on a letter lands in the grid at that title
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
        // the Sources panel has no drill-in level to walk back through: its two levels are peers,
        // reached sideways, so BACK dismisses the panel from either one
        Menu::Source | Menu::Sort | Menu::Filter => {
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
            TAB_F = crate::browse::tab_of_section(crate::browse::cur()) + 1;
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
const SRC_LEVEL_H: f32 = SRC_PILL_H + 2.0 * theme::space::SM;

fn panel_rect() -> Rect {
    let (w, band) = if menu() == Menu::Source { (SRC_W, SRC_LEVEL_H) } else { (MENU_W, 0.0) };
    let ph = (table().measured_height() + band).clamp(120.0, SCR_H - 280.0);
    Rect::new(MARGIN_X, TOOL_Y + TOOL_H + 18.0, w, ph)
}
/// The LIST's frame inside the panel — under the level pills when the Sources panel is open, the
/// whole panel otherwise. Every scroll, hit-test and draw reads this one function, so the three
/// cannot disagree about where the rows are.
fn table_rect() -> Rect {
    let r = panel_rect();
    if menu() == Menu::Source {
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
fn close_menu() {
    unsafe { MENU = Menu::None };
    pop().close();
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
    let groups = crate::browse::source_groups();
    let rows = crate::browse::source_rows();
    let (secs, acts) = source_sections(level, &groups, &rows);
    let sel = source_sel(&rows, keep.then(|| table().sel));
    unsafe {
        *addr_of_mut!(SRC_ACTIONS) = acts;
        MENU_BUILT = rows.len();
        MENU = Menu::Source;
    }
    table().compact = false; // two-line rows: HEADLINE titles over CAPTION sub-lines
    // `slide` = `keep`, and that is the level swap's whole promise kept in one argument: a rebuild
    // GLIDES (so the panel's scroll and highlight hold still while the words under them change),
    // an OPEN snaps to the current library with the list scrolled to the top.
    table().set_sections(secs, sel, keep);
    if !keep {
        unsafe { SRC_ON_PILLS = false };
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

fn src_action(sel: i32) -> SrcAction {
    let acts = unsafe { &*addr_of!(SRC_ACTIONS) };
    usize::try_from(sel).ok().and_then(|i| acts.get(i)).copied().unwrap_or(SrcAction::None)
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
        Menu::Source => match src_action(sel) {
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
            let (unwatched_row, genre_row) = filter_rows();
            if sel == unwatched_row {
                request(Pending::Unwatched(!view_unwatched()));
                open_filter_menu(true); // rebuilt from `view_unwatched()` — the check flips at once
                table().sel = unwatched_row;
            } else if sel == genre_row {
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
        if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
            AREA = Area::Toolbar;
            TOOL_F = i;
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
        if i == 0 {
            return Action::GoHome;
        }
        switch_tab(i - 1);
        return Action::None;
    }
    let tools = unsafe { addr_of!(TOOL_RECTS).read() };
    if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
        // resolved through the CHIP the click landed on, exactly as the key path does. It used to
        // defer to `on_ok`, which reads the FOCUSED chip — `pointer_focus` above has just moved
        // focus there, so the two agreed by side effect rather than by construction.
        open_chip_menu(chips()[i.min(chips().len() - 1)]);
        return Action::None;
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

    // ---- grid: pass 1 — every non-focused visible cell (row-culled; resolve only on-screen) --
    let t = total();
    if crate::browse::loading_initial() {
        Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
            .phase(unsafe { addr_of!(PHASE_MS).read() } as u32)
            .draw(&Env::inert(), p);
    } else if crate::browse::failed_initial() {
        // A failed first page now STOPS the spinner (`browse::SecFetch`), so this line is what the
        // user sees instead of a grid that spins forever. It is a placeholder for the real failure
        // read-out — caption, reason, Try again — which lands with the `StatusOverlay` treatment;
        // it draws on `p`, not `pg`, for the reason the spinner does: a status must stay legible
        // while the content band is dark. `browse` keeps retrying underneath it.
        Label::new(c"Couldn't load this library".as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY)
            .h(crate::ui::label::HAlign::Center)
            .draw(p, Rect::new(0.0, SCR_H * 0.5 - 20.0, SCR_W, 40.0));
    } else if t == 0 {
        Label::new(c"Nothing here matches".as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY)
            .h(crate::ui::label::HAlign::Center)
            .draw(pg, Rect::new(0.0, SCR_H * 0.5 - 20.0, SCR_W, 40.0));
    }
    // visible row RANGE by arithmetic, not an O(totalSize) walk (a 10k-item section must not
    // cost 1.7k iterations per frame); rows fully under the opaque top-chrome scrim are culled
    // too — drawing the full card composite beneath it was pure fill-rate waste.
    // Deliberately NO `if pg.alpha > 0` early-out around these loops: at the floor they already
    // cost nothing (`n_rows()` is 0 while `total < 0`), and skipping them would stop `resolve_tex`
    // being called for the incoming section's visible tiles until the fade-IN starts — delaying
    // every poster fetch by the length of the fade. The 64-slot LRU must see the on-screen
    // requests exactly as early as it does today.
    let r_lo = (((sc - CARD_H - GLOW_PAD) / PITCH).floor().max(0.0)) as usize; // loose; per-row cull is exact
    let r_hi = ((((sc + SCR_H - GRID_TOP) / PITCH).ceil()).max(0.0) as usize + 1).min(n_rows());
    for r in r_lo..r_hi {
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
    if focused_pass && t > 0 {
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

    let tool_focus = |i: usize| area() == Area::Toolbar && !menu_open() && unsafe { addr_of!(TOOL_F).read() } == i;
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

    // item count, right-aligned on the toolbar line (cached — changes only on re-query)
    let tot = crate::browse::total();
    if tot >= 0 {
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
        let mp = pop().painter(0.45, 20.0);
        let r = panel_rect();
        mp.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        if menu() == Menu::Source {
            draw_level_pills(mp, r);
        }
        table().draw(mp, table_rect());
    }
}

/// The Sources panel's level switch: two segmented pills at the panel top, the shared
/// [`TabPill`](crate::ui::widgets::TabPill) in its boolean `segment` model — a selected segment is
/// a subtle pill, an unselected one is dim text with no pill, and the focused one is the bright
/// accent capsule. That model exists for a segmented control that is NOT in a strip, which is
/// exactly this: two fixed segments with nothing to travel between.
fn draw_level_pills(p: Painter, panel: Rect) {
    use crate::ui::widgets::TabPill;
    let rects = src_pill_rects(panel);
    unsafe { *addr_of_mut!(SRC_PILL_RECTS) = rects };
    let level = unsafe { addr_of!(SRC_LEVEL).read() };
    let on_pills = unsafe { addr_of!(SRC_ON_PILLS).read() };
    for (i, (label, lv)) in [(c"Browse", Level::Browse), (c"On Home", Level::OnHome)].into_iter().enumerate() {
        TabPill::new(label.as_ptr(), theme::size::BODY, rects[i])
            .segment(level == lv)
            .focused(on_pills && level == lv)
            .draw(&Env::inert(), p);
    }
}

/// The level pill under the pointer, or None (rects recorded at draw; stale while closed, which is
/// why every caller is already inside a `menu_open()` branch).
fn src_pill_at(mx: f32, my: f32) -> Option<usize> {
    if menu() != Menu::Source {
        return None;
    }
    let rects = unsafe { addr_of!(SRC_PILL_RECTS).read() };
    rects.iter().position(|r| r.w > 0.5 && r.contains(mx, my))
}

/// The A–Z rail: only the letters that EXIST (the server's `/firstCharacter` index), vertically
/// centered in the grid band on the right edge. The letter holding rail focus is a snow disc;
/// the letter containing the current grid focus reads primary; the rest tertiary.
fn draw_rail(p: Painter) {
    if !rail_on() || menu_open() {
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

        for a in [Area::Grid, Area::Toolbar, Area::Rail] {
            unsafe { AREA = a };
            assert_eq!(focused_pill(), None, "focus is not on the tab row");
        }

        unsafe { AREA = area0; MENU = menu0; TAB_F = tab0 };
        clear();
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
            vec![group("mac-mini", "", true), group("<peer-name-2>", "<peer-owner-1>", true)],
            vec![
                lib(0, 0, "Movies", true, false, true),
                lib(0, 1, "TV Shows", true, false, false),
                lib(1, 2, "LDN Films", false, false, false),
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
        assert_eq!((secs[1].header.as_str(), secs[1].accessory.as_str()), ("<peer-name-2>", "<peer-owner-1>"));
        assert!(!secs[0].dim && !secs[1].dim);

        g[1].reachable = false;
        let mut r = r;
        r[2].pinned = true; // pinned AND unreachable: the state the design calls out by name
        let (secs, _) = source_sections(Level::OnHome, &g, &r);
        assert!(secs[1].dim, "the whole group dims, header included");
        assert!(!secs[0].dim, "…and only that group");
        assert_eq!(secs[1].rows[0].toggle, Some(true), "it still reads On — nothing was turned off");
        // the state leads the accessory, so the run that gives way under elision is the handle
        assert_eq!(secs[1].accessory, "Not reachable \u{b7} <peer-owner-1>");

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
    #[test]
    fn every_chip_opens_its_own_menu_by_identity_and_never_by_position() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        let tool0 = unsafe { addr_of!(TOOL_F).read() };

        // chip → menu, one row per chip. A shifted row cannot satisfy this: it is keyed on the
        // value, not on where the value sits.
        assert_eq!(chip_menu(Chip::Source), Menu::Source);
        assert_eq!(chip_menu(Chip::Sort), Menu::Sort);
        assert_eq!(chip_menu(Chip::Filter), Menu::Filter);

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
            assert_eq!(focused_chip(), want);
        }
        // a `TOOL_F` left over from the longer row resolves to a chip that EXISTS rather than
        // panicking or naming one that does not
        unsafe { TOOL_F = 2 };
        assert_eq!(focused_chip(), Chip::Filter);

        unsafe { TOOL_F = tool0 };
    }
}
