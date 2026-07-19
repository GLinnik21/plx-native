//! The Library browse screen — a full-section poster wall with server-driven sort/filter.
//!
//! Layout (per the approved mock): the shared top bar (profile chip leading at the margin +
//! CENTERED tab pills Home | \<sections\>, the tvOS tab-bar idiom — [`draw_tab_row`] is shared
//! with Home), a toolbar of state-carrying chips (Sort · X / Filter · X / Unwatched + item
//! count), and a 6-across vertical grid of [`card_row`] leaf tiles fed by the `browse` paged
//! store. Menus are Popover + TableView (the track-menu components), their contents entirely
//! server-driven (`browse::sorts()` from `includeMeta=1`; genres from the value list).
//!
//! Focus model: three bands — Tabs, Toolbar, Grid — plus a modal menu state. BACK walks
//! grid/toolbar → tab bar → (app.rs) Home: the Netflix-2025 escape rule, so the nav is always
//! one press away at any scroll depth. Focus/scroll per section persist in the browse store.
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
/// Row pitch: card + the focused under-label band (title + caption) before the next row.
const PITCH: f32 = CARD_H + 96.0;
use crate::ui::widgets::{MAX_TABS, TOP_BAR_Y}; // the shared top-bar chrome lives in widgets

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
static mut TOOL_F: usize = 0; // focused toolbar chip (0 sort / 1 filter / 2 unwatched)
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
static mut TOOL_RECTS: [Rect; 3] = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
// ---- per-frame draw caches (rebuilt only when their source state changes; building CStrings
// + re-measuring text every frame was a review-confirmed waste — same rationale as TAB_CACHE) --
struct ChipStrs {
    name: CString,
    val: CString,
    has_val: bool,
    w: f32,
}
static mut CHIP_CACHE: Option<(String, String, usize, [ChipStrs; 3])> = None;
static mut RAIL_LABELS: Option<(u32, usize, Vec<CString>)> = None; // (sections_gen, section, labels)
static mut COUNT_C: Option<(i64, bool, CString)> = None;
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

/// Enter the Library on section `sec` (0-based). Discovers the sections on first use
/// (blocking, one small GET) and restores the section's remembered focus/scroll.
pub(crate) fn enter(sec: usize) {
    let n = crate::browse::ensure_sections();
    if n == 0 {
        return;
    }
    crate::browse::set_cur(sec.min(n - 1));
    crate::browse::kick_letters();
    restore_view();
    unsafe {
        AREA = Area::Grid;
        MENU = Menu::None;
        TOOL_F = 0;
    }
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
/// Deliberately does NOT touch the focus band: toggling the Unwatched chip must leave focus
/// on that chip, not warp it into the grid.
fn grid_reset() {
    unsafe {
        GR = 0;
        GC = 0;
        (*addr_of_mut!(SCROLL)).jump(0.0);
        PREV_IDX = -1;
    }
}

fn switch_section(i: usize) {
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
                // clamp to what the row can DRAW — focus must never walk onto a truncated pill
                let n = (1 + crate::browse::section_count()).min(MAX_TABS);
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
                } else if sym == SDLK_RIGHT && TOOL_F < 2 {
                    TOOL_F += 1;
                } else if sym == SDLK_UP {
                    AREA = Area::Tabs;
                    TAB_F = crate::browse::cur() + 1;
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
                1 => open_filter_menu(false),
                _ => {
                    crate::browse::toggle_unwatched();
                    grid_reset();
                }
            }
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
        Menu::Sort | Menu::Filter => {
            close_menu();
            return true;
        }
        Menu::None => {}
    }
    if area() == Area::Rail {
        unsafe { AREA = Area::Grid };
        return true;
    }
    if area() != Area::Tabs {
        unsafe {
            AREA = Area::Tabs;
            TAB_F = crate::browse::cur() + 1;
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
        .row(Row::new("Unwatched only").checked(crate::browse::unwatched()))
        .row(
            Row::new("Genre")
                .detail(crate::browse::genre_sel().map(|g| g.title.clone()).unwrap_or_else(|| "All".into()))
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
                crate::browse::set_sort(sel as usize);
                grid_reset();
            }
            close_menu();
        }
        Menu::Filter => {
            if sel == 0 {
                crate::browse::toggle_unwatched();
                grid_reset();
                open_filter_menu(true); // refresh the check, keep the menu up
                table().sel = 0;
            } else if sel == 1 {
                build_genre_menu(false);
            }
        }
        Menu::Genre => {
            let genres_n = crate::browse::genres().len();
            if sel == 0 {
                crate::browse::set_genre(None);
                grid_reset();
                close_menu();
            } else if !crate::browse::genres().is_empty() && (sel as usize) <= genres_n {
                crate::browse::set_genre(Some(sel as usize - 1));
                grid_reset();
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
        if let Some(i) = tools.iter().position(|r| r.contains(mx, my)) {
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
    if tools.iter().any(|r| r.contains(mx, my)) {
        return on_ok();
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

/// The three toolbar chips' strings + measured widths, cached until the labels/section change.
fn chip_strs() -> &'static [ChipStrs; 3] {
    let sort = crate::browse::sort_label();
    let filt = crate::browse::filter_label();
    let sec = crate::browse::cur();
    let cache = unsafe { &mut *addr_of_mut!(CHIP_CACHE) };
    let stale = cache.as_ref().map(|(s, f, c, _)| s != sort || f != filt || *c != sec).unwrap_or(true);
    if stale {
        let build = |name: &str, value: &str| {
            let sz = theme::size::LABEL;
            let name_c = CString::new(name).unwrap_or_default();
            let val_c = CString::new(if value.is_empty() { String::new() } else { format!(" \u{b7} {value}") })
                .unwrap_or_default();
            let name_bold = if value.is_empty() { 1 } else { 0 };
            let nw = crate::text::text_width(name_c.as_ptr(), sz, name_bold);
            let vw = if value.is_empty() { 0.0 } else { crate::text::text_width(val_c.as_ptr(), sz, 1) };
            let w = CHIP_PAD + nw + vw + 10.0 + CHIP_ISZ + CHIP_PAD;
            ChipStrs { name: name_c, val: val_c, has_val: !value.is_empty(), w }
        };
        let chips = [build("Sort", sort), build("Filter", filt), build("Unwatched", "")];
        *cache = Some((sort.to_string(), filt.to_string(), sec, chips));
    }
    &cache.as_ref().unwrap().3
}

/// One toolbar chip from its cached strings. Hierarchy per the design review: the VALUE
/// ("Title"/"All") is the information-bearing token — bright + bold; the static name prefix is
/// dim + regular. A value-less chip IS the toggle (its label is the value), and `toggled`
/// renders the unmistakable amber disc + ink check.
fn toolbar_chip(p: Painter, x: f32, cs: &ChipStrs, icon: Icon, focused: bool, toggled: bool) -> Rect {
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
    let (name_ink, name_bold) = if cs.has_val { (dim_ink, 0) } else { (bright_ink, 1) };
    tx += p.text(cs.name.as_ptr(), tx, ty, sz, name_ink, 0, name_bold);
    if cs.has_val {
        tx += p.text(cs.val.as_ptr(), tx, ty, sz, bright_ink, 0, 1);
    }
    // trailing icon — an SVG asset, never a font glyph
    let ir = Rect::new(tx + 10.0, r.y + (r.h - CHIP_ISZ) * 0.5, CHIP_ISZ, CHIP_ISZ);
    if toggled && !cs.has_val {
        let dr = ir.inset(-4.0);
        p.rect(dr, dr.w * 0.5, theme::RESUME_FILL, theme::RESUME_FILL, 0.0);
        crate::ui::icons::draw(p, icon, ir, crate::ui::ACCENT_INK);
    } else {
        let tint = if focused { crate::ui::ACCENT_INK } else { theme::TEXT_SECONDARY };
        crate::ui::icons::draw(p, icon, ir, tint);
    }
    r
}

pub(crate) fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
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
    } else if t == 0 {
        Label::new(c"Nothing here matches".as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY)
            .h(crate::ui::label::HAlign::Center)
            .draw(p, Rect::new(0.0, SCR_H * 0.5 - 20.0, SCR_W, 40.0));
    }
    // visible row RANGE by arithmetic, not an O(totalSize) walk (a 10k-item section must not
    // cost 1.7k iterations per frame); rows fully under the opaque top-chrome scrim are culled
    // too — drawing the full card composite beneath it was pure fill-rate waste
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
            card_row::draw_tile(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume);
        }
    }

    // ---- top chrome: scrim under it once the grid has scrolled, then chip + pills + toolbar --
    if sc > 1.0 {
        let base = theme::SURFACE_APP;
        p.rect(Rect::new(0.0, 0.0, SCR_W, 170.0), 0.0, base, base, 0.0);
        p.rect(Rect::new(0.0, 170.0, SCR_W, GRID_TOP - 170.0), 0.0, base, theme::with_a(base, 0.0), 0.0);
    }
    crate::ui::widgets::profile_chip(p, Rect::new(MARGIN_X, TOP_BAR_Y, 64.0, 64.0), false);
    let tab_focus = if area() == Area::Tabs && !menu_open() { (unsafe { addr_of!(TAB_F).read() }) as c_int } else { -1 };
    crate::ui::widgets::draw_tab_row(p, crate::browse::cur() as c_int + 1, tab_focus);

    let tool_focus = |i: usize| area() == Area::Toolbar && !menu_open() && unsafe { addr_of!(TOOL_F).read() } == i;
    let unw = crate::browse::unwatched();
    let chips = chip_strs();
    let mut x = MARGIN_X;
    let r0 = toolbar_chip(p, x, &chips[0], Icon::ChevronDown, tool_focus(0), false);
    x = r0.x + r0.w + 20.0;
    let r1 = toolbar_chip(p, x, &chips[1], Icon::ChevronDown, tool_focus(1), false);
    x = r1.x + r1.w + 20.0;
    let r2 = toolbar_chip(p, x, &chips[2], if unw { Icon::Check } else { Icon::Ring }, tool_focus(2), unw);
    unsafe { *addr_of_mut!(TOOL_RECTS) = [r0, r1, r2] };

    // item count, right-aligned on the toolbar line (cached — changes only on re-query)
    let tot = crate::browse::total();
    if tot >= 0 {
        let is_show = crate::browse::section_is_show(crate::browse::cur());
        let cache = unsafe { &mut *addr_of_mut!(COUNT_C) };
        if cache.as_ref().map(|(ct, cshow, _)| *ct != tot || *cshow != is_show).unwrap_or(true) {
            let noun = if is_show { "shows" } else { "films" };
            *cache = Some((tot, is_show, CString::new(format!("{tot} {noun}")).unwrap_or_default()));
        }
        let cs = &cache.as_ref().unwrap().2;
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 0, TOOL_Y + TOOL_H * 0.5);
        p.text(cs.as_ptr(), SCR_W - MARGIN_X, ty, theme::size::CAPTION, theme::TEXT_TERTIARY, 2, 0);
    }

    // ---- grid: pass 2 — the focused card + under-label, over the chrome scrim ----------------
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
            let title_c = m.and_then(|mm| CString::new(mm.title.as_str()).ok());
            let title = title_c.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let caption = m.and_then(|mm| (mm.year > 0).then(|| CString::new(mm.year.to_string()).ok()).flatten());
            let cap_ptr = caption.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
            let resume = m.and_then(PmsMovie::resume_frac);
            card_row::draw_focused(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume, title, cap_ptr, false);
        }
    }

    // ---- letter rail (right edge; only on the unfiltered ascending-title listing) ------------
    draw_rail(p);

    // ---- menus over everything ---------------------------------------------------------------
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
        5 | 6 => {
            crate::browse::toggle_unwatched();
            grid_reset();
        }
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
