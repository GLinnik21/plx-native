//! Detail screen: a full-screen item page — the reused hero (no page dots) over the
//! item backdrop, with a vertical-scroll flow underneath for the episode/related/
//! cast rows and About footer (added in later increments). Reads the loaded item
//! from crate::metadata and the selected catalog row (backdrop art + blur) from the
//! browse catalog. Mirrors the home screen's C-shaped entry points (open/update/
//! draw/move_focus) driven by app.rs.
#![allow(dead_code)]
use crate::metadata;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{resolve_tex, Art, Button, CircleButton, ControlStyle, TabNote, TabPill};
use crate::ui::{hero_alpha, on_axis, Column, Env, Painter, Rect, ScrollColumn, Spring, View}; // View: Button/CircleButton::draw
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

// ---- retained view-tree migration (step 6.2): detail's mutable state lives in DetailView, reached
// through the lazy view() accessor. The frozen pub(crate) fns (open/update/draw/move_focus/…) keep
// their identical signatures and read/write view() fields directly. LAZY init (unlike home's eager
// scene()) because detail has no detail_init C-ABI — open/open_rk are usually the first calls, but a
// draw before open must not panic, so view() builds a default DetailView on first touch.
struct DetailView {
    selected: c_int,
    /// The ratingKey the page is pointed at, kept so `reselect` can re-resolve `selected` while the
    /// fetch is still in flight. `selected` is an INDEX into a catalog that a hub refetch rebuilds
    /// wholesale (Continue Watching re-sorts by `lastViewedAt`, so every row before the just-played
    /// item shifts) — the index is only meaningful next to the identity that produced it, and
    /// `metadata::current()` is deliberately None for the whole 2-5 round-trip window (see
    /// `open_rk`). Without this, a refetch landing inside that window left `selected` naming a
    /// different movie for the rest of the visit.
    mounted_rk: String,
    section: c_int, // 0=hero buttons, 1=season tabs, 2=episodes, 3=related, 4=cast, 5=about
    col: c_int,     // focused item within the section
    // the below-hero sections are a shared ScrollColumn: it owns the vertical scroll spring, the
    // section flow (child_top == the old section_y), and the off-screen band cull (via on_axis).
    column: ScrollColumn,
    card_scale: Spring, // focused card-row item pop (springs on selection change)
    // per-episode focus-pop springs — like a CardRow's per-cell scale springs, so a deselected episode
    // still animates its pop (and thus its lifted shadow) all the way back down instead of snapping.
    // Fixed cap; episodes past it (very long seasons) simply don't animate the pop.
    ep_scale: [Spring; EP_ANIM_MAX],
    ep_hscroll: Spring, // episode row horizontal scroll — glides instead of snapping
    tab_hscroll: Spring, // season-tab row horizontal scroll (many-season shows overflow the width)
    // season-tab load is DEBOUNCED: focusing a tab records the target here + resets the settle timer;
    // update() loads it only once focus has been stable (SEASON_SETTLE). Otherwise a held LEFT/RIGHT
    // through the tabs would fire a blocking PMS `/children` fetch every repeat tick.
    pending_season: c_int, // tab focused but not yet loaded (-1 = none / already loaded)
    season_settle: f32,    // seconds the pending season has been stable
    related: CardRow,   // the Related row = the SAME animated shelf component the home grid uses
    cast: CardRow,      // Cast & Crew row — the same component, circular RowStyle::CAST
    last_resume_ns: i64, // resume position (ns) on_ok just started (0 = from start); app.rs seeks here
    // Whether the hero row carried a restart disc the last time its focus was resolved. The row's
    // control SET is derived from the loaded item, which arrives (and changes) asynchronously — so
    // the index the focus holds has to be carried across a set change or it silently comes to mean a
    // different button. See `hero_col`.
    hero_had_restart: bool,
    // per-section focus memory (episodes/related/cast): leaving a row and coming back restores the
    // item you were on — paired with the frozen h-scrolls, a row "stays where it was" instead of
    // snapping to its start whenever focus moves elsewhere. Indexed by section id.
    saved_col: [c_int; 6],
    spin_ms: f32, // spinner phase for the episode row's season-loading state
}
impl DetailView {
    fn new() -> Self {
        Self {
            selected: -1,
            mounted_rk: String::new(),
            section: 0,
            col: 0,
            column: ScrollColumn::new(CONTENT_TOP, TOP_MARGIN), // top re-derived per frame (update)
            card_scale: Spring::at(1.0),
            ep_scale: [Spring::at(1.0); EP_ANIM_MAX],
            ep_hscroll: Spring::at(0.0),
            tab_hscroll: Spring::at(0.0),
            pending_season: -1,
            season_settle: 0.0,
            related: CardRow::new(),
            cast: CardRow::new(),
            last_resume_ns: 0,
            hero_had_restart: false,
            saved_col: [0; 6],
            spin_ms: 0.0,
        }
    }
}
static mut VIEW: Option<DetailView> = None;
fn view() -> &'static mut DetailView {
    unsafe { (*addr_of_mut!(VIEW)).get_or_insert_with(DetailView::new) }
}

// ---- the hero action row ----
// Play/Resume pill (0), the restart disc (1, present ONLY while there is a resume point to ignore),
// then the watched toggle. An info disc would be a no-op on its own page, so the row stops there.
// The set is DYNAMIC, so nothing may hard-code the watched index: see `hero_btns`/`btn_watched`.
const BTN_PLAY: c_int = 0;
const BTN_RESTART: c_int = 1;
// Play pill MINIMUM width. The drawn width is `widgets::pill_w`, measured from the label, and for
// "Play" that lands ~2px above this — so the detail Play pill is now pixel-identical to home's
// rather than approximating it with a constant. The floor only guards a pathologically short label.
const PW: f32 = 168.0;
const CGAP: f32 = 20.0;
const CD: f32 = 60.0; // circle button diameter
/// The hero text column's width — the synopsis wrap AND every fine-print line under it (the
/// date/runtime pair, the "Directed by" credit) share it, so nothing in the block can grow past
/// the right-aligned "Starring" line.
const HERO_TEXT_W: f32 = 900.0;

// Below-the-hero content is ONE vertical scroll of stacked blocks, driven by the shared retui
// `ScrollColumn` (`impl Column for DetailView`): its `child_top` COMPUTES each block's pre-scroll top
// by stacking the *present* blocks' heights (`block_h`, via `Column::height`) from CONTENT_TOP with a
// single SECTION_GAP between them (season tabs → episodes hug with TAB_EP_GAP) — no hard-coded
// per-section Y to keep in sync. A block's height is content-derived (e.g. the Related block tracks
// REL_H → the shared home poster size), so resizing one block reflows everything below it. The
// container's scroll lifts the focused block's top to TOP_MARGIN under the compact title.
const TOP_MARGIN: f32 = 120.0;
const CONTENT_TOP: f32 = 920.0; // nominal flow top; update() re-derives it from the hero buttons
const SECTION_GAP: f32 = theme::space::XL; // vertical padding between top-level blocks (64)
const TAB_EP_GAP: f32 = theme::space::MD; // season tabs → their episode row (they read as one unit)
const SCROLLED: f32 = CONTENT_TOP - TOP_MARGIN; // backdrop-dim saturation reference (= 800)
const BTN_CONTENT_GAP: f32 = theme::space::XL; // hero button row → first below-hero block (64)

// Season tabs (header for the episode row)
const TAB_ROW_H: f32 = CD; // tab pills stand as tall as the hero buttons (one control height)
const TAB_ADVANCE: f32 = 52.0; // per-tab horizontal advance past the label width
const SEASON_SETTLE: f32 = 0.2; // hold a season tab this long (s) before its episodes are fetched
// Episodes: landscape stills + under-card metadata
const EP_ANIM_MAX: usize = 40; // per-episode pop-spring count (episodes past this don't animate the pop)
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
// Under-still metadata: kicker → title → summary → date. The (fine-print) air date sits at the
// BOTTOM of the block, under the summary; offsets are from the meta top (EP_H + EP_META_TOP below
// the still) and the block height is derived from the tallest episode summary (see `ep_meta_h`)
// so the dynamic below-hero flow reflows to fit.
const EP_META_TOP: f32 = 30.0; // still bottom → kicker
const EP_TITLE_DY: f32 = 42.0; // kicker → title
const EP_TITLE_LEAD: f32 = 30.0; // title line pitch (title wraps to at most 2 lines)
const EP_TITLE_SUMMARY_GAP: f32 = 18.0; // title last-line BASELINE → summary cap top (cap-band derived)
const EP_SUMMARY_DATE_GAP: f32 = 16.0; // summary last-line baseline → date cap top
const EP_SUMMARY_LEAD: f32 = 30.0;
const EP_SUMMARY_MAXLINES: usize = 8; // bounds the pathological case; ordinary episode blurbs fit fully
const EP_META_BOT_PAD: f32 = 24.0;
// Related row (portrait posters) reuses the home shelf poster geometry (consts::CARD_*) so a poster
// is one size app-wide (its texture request was already 250×375 → now drawn 1:1 sharp).
const REL_W: f32 = CARD_W; // 250
const REL_H: f32 = CARD_H; // 375
const REL_GAP: f32 = GAP; // 30
const REL_LABEL_H: f32 = 46.0; // "Related" heading → poster row
const REL_UNDER_H: f32 = 54.0; // poster row → tile title → block bottom
// Cast & Crew row (circular headshots)
const CAST_D: f32 = 190.0; // headshot diameter
const CAST_SLOT: f32 = 230.0; // per-member horizontal pitch (room for the name)
const CAST_LABEL_H: f32 = 60.0; // "Cast & Crew" heading → headshot row
const CAST_UNDER_H: f32 = 92.0; // headshot → name/role → block bottom (name LABEL + role CAPTION)

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    let idx = view().selected;
    if idx < 0 {
        return None;
    }
    crate::pms::movie(idx as usize)
}

/// The resume position (ns) the hero's Play control would ACTUALLY apply — i.e. exactly what
/// `on_ok`'s play arm hands `app.rs`. A movie resumes from its own `viewOffset`; a show page's Play
/// starts `episodes[0]` of the selected season (see `play_episode_at`), so it reads that leaf's.
///
/// Both go through `metadata::resume_ns`, the same Plex rule the action applies, deliberately: keyed
/// on a raw `resume_ms > 0` instead (as the home hero is), a 4-second offset or one past the 95%
/// end-of-item mark would label the pill "Resume" for a play that starts at 0, and hang a restart
/// disc beside it that does nothing. The label and the control set have to promise what the press
/// delivers.
fn hero_resume_ns() -> i64 {
    let Some(d) = metadata::current() else { return 0 };
    if d.is_show {
        d.episodes.first().map(|e| metadata::resume_ns(e.resume_ms, e.dur_ms)).unwrap_or(0)
    } else {
        metadata::resume_ns(d.resume_ms, d.dur_ms)
    }
}
/// is there a resume point for the restart disc to ignore? (no resume point → Play already starts
/// at 0, so a restart control would be a second button doing the first one's job)
fn has_restart() -> bool {
    hero_resume_ns() > 0
}
/// how many controls the hero row is showing right now
fn hero_btns() -> c_int {
    if has_restart() {
        3
    } else {
        2
    }
}
/// index of the watched toggle — always last, so it shifts with the restart disc
fn btn_watched() -> c_int {
    hero_btns() - 1
}
/// The hero row's focused index, resolved against the LIVE control set and written back — so the
/// correction IS the state rather than something each reader has to remember.
///
/// The set is derived from an item that arrives, and changes, asynchronously, so an index alone is
/// not a stable way to name a control. Both directions bite, and both are reachable by hand:
///
/// - It **grows**. `open_rk` deliberately clears `current()` for the whole 2-5 round-trip fetch, so
///   during that window the row is two buttons and index 1 is the watched toggle. Press RIGHT there,
///   let a part-watched item land, and index 1 has quietly become the restart disc — the next OK
///   would start playback instead of toggling watched.
/// - It **shrinks**. The watched toggle is the control that can delete the restart disc from under
///   the focus, because scrobbling clears the server's `viewOffset`. Focus left at index 2 of a
///   two-button row draws nothing focused and makes every later OK inert.
///
/// So the focus follows its CONTROL, not its index: the disc is inserted at (and removed from)
/// `BTN_RESTART`, so everything at or after it shifts by one when the set flips. The clamp then
/// catches anything else. NB `move_focus` reads `col` raw, which is sound only because this runs
/// every drawn frame (`env_of`) and the event loop polls keys BEFORE `update` lands a fetch — a key
/// press therefore always sees a `col` already resolved against the set that press is acting on.
fn hero_col() -> c_int {
    let v = view();
    if v.section == 0 {
        let now = has_restart();
        if now != v.hero_had_restart {
            if v.col >= BTN_RESTART {
                v.col += if now { 1 } else { -1 };
            }
            v.hero_had_restart = now;
        }
        v.col = v.col.clamp(0, hero_btns() - 1);
    }
    v.col
}

/// What an OK on hero control `col` means. The row's indices MOVE with its control set, so the
/// mapping lives in one pure function instead of a literal compared at each site — and it is the
/// half of the press that is host-testable, the actions themselves being PMS round trips.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeroAction {
    Play,
    /// the same play, with the resume rule dropped
    Restart,
    Watched,
    None,
}
fn hero_action(col: c_int) -> HeroAction {
    if col == btn_watched() {
        HeroAction::Watched
    } else if col == BTN_PLAY {
        HeroAction::Play
    } else if has_restart() && col == BTN_RESTART {
        HeroAction::Restart
    } else {
        HeroAction::None
    }
}

/// The focused hero button (0=Play), or -1 when the hero section isn't focused.
///
/// **This WRITES** (through `hero_col`), unlike the read-only accessor it used to be. Harmless from
/// where it is called today — `env_of` and `draw_buttons`, both outside the `col.draw(view(), …)`
/// window in `draw()` — but do not reach for it from inside a `Column::draw_child`, which holds a
/// `&DetailView` reborrowed from that same `&'static mut`.
pub(crate) fn focus() -> c_int {
    if view().section == 0 {
        hero_col()
    } else {
        -1
    }
}

/// available sections for the loaded item (hero always; tabs/episodes only for shows;
/// related/cast for both when present). Section ids: 0 hero, 1 tabs, 2 episodes,
/// 3 related, 4 cast.
fn sections() -> ([c_int; 6], usize) {
    // fixed stack array (max ids 0..=5) + a live count — no per-frame heap alloc on the draw path.
    let mut v = [0 as c_int; 6];
    let mut n = 1; // hero (0) always present; v[0] is already 0
    if let Some(d) = metadata::current() {
        if d.is_show && !d.seasons.is_empty() {
            v[n] = 1;
            n += 1;
        }
        if d.is_show && !d.episodes.is_empty() {
            v[n] = 2;
            n += 1;
        }
        // Cast & Crew (credits) before Related — the flow order is the push order here (block_h /
        // draw_child / move_focus all key off the section id, not its position, so this is the one
        // place ordering lives).
        if d.credits_len() > 0 {
            v[n] = 4;
            n += 1;
        }
        if !d.related.is_empty() {
            v[n] = 3;
            n += 1;
        }
        v[n] = 5; // About footer — always present when an item is loaded
        n += 1;
    }
    (v, n)
}
fn n_items(section: c_int) -> c_int {
    match section {
        0 => hero_btns(),
        1 => metadata::current().map(|d| d.seasons.len()).unwrap_or(0) as c_int,
        2 => metadata::current().map(|d| d.episodes.len()).unwrap_or(0) as c_int,
        3 => metadata::current().map(|d| d.related.len()).unwrap_or(0) as c_int,
        4 => metadata::current().map(|d| d.credits_len()).unwrap_or(0) as c_int, // actors + crew
        5 => 4, // About: card + Information + Languages + Accessibility (selection moves between them)
        _ => 0,
    }
}
/// content height of a scrollable block — how far the next block is pushed down. Content-derived, so
/// changing a size (poster, still) reflows the stack. About (5) is last, so its height pushes nothing.
fn block_h(section: c_int) -> f32 {
    match section {
        1 => TAB_ROW_H,
        2 => EP_H + ep_meta_h(),
        3 => REL_LABEL_H + REL_H + REL_UNDER_H,
        4 => CAST_LABEL_H + CAST_D + CAST_UNDER_H,
        _ => 0.0,
    }
}
/// The under-still meta layout for ONE episode, measured from the meta top (`ty`): the summary hugs
/// the ACTUAL title height (1 or 2 lines) and the (fine-print, MICRO) air date hangs at the block's
/// BOTTOM under the summary — nothing is reserved, so a 1-line title doesn't leave a 2-line gap.
/// Returns `(date_y, summary_y, total_h)`, all px below ty. Cap-band-derived pacing: each gap is
/// measured last-line-INK (baseline) → next block's cap top, not leading-box to raw texture — the
/// leading slack used to make the gaps read visibly unequal. (`date_y` stays a raw Painter::text
/// draw-y; the cap offsets translate ink-space gaps into it.) measure_h is wrap-cached, so this is
/// cheap after the first frame.
fn ep_meta_layout(title: &str, has_date: bool, summary: &str) -> (f32, f32, f32) {
    let title_h = TextView::new(title, theme::size::LABEL, theme::TEXT_PRIMARY)
        .bold()
        .leading(EP_TITLE_LEAD)
        .max_lines(2)
        .measure_h(EP_W);
    let (lt, lb) = crate::text::text_cap_band(theme::size::LABEL, 1); // title cap-top/baseline offsets
    let title_base = EP_TITLE_DY + (title_h - EP_TITLE_LEAD) + (lb - lt); // last-line baseline below ty
    // summary: a TextView anchors its cap band at the draw-y, so the ink gap needs no correction
    let summary_y = title_base + EP_TITLE_SUMMARY_GAP;
    let (st, sb) = crate::text::text_cap_band(theme::size::CAPTION, 0); // summary line offsets
    let summary_h = if summary.is_empty() {
        0.0
    } else {
        TextView::new(summary, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .leading(EP_SUMMARY_LEAD)
            .max_lines(EP_SUMMARY_MAXLINES)
            .measure_h(EP_W)
    };
    // last ink baseline above the date: the summary's last line, or the title's when there is none
    let base = if summary.is_empty() { title_base } else { summary_y + (summary_h - EP_SUMMARY_LEAD) + (sb - st) };
    let (mt, mb) = crate::text::text_cap_band(theme::size::MICRO, 0); // date offsets
    let date_y = base + EP_SUMMARY_DATE_GAP - mt; // raw Painter::text draw-y for the MICRO date line
    let total = if has_date { date_y + mb } else { base };
    (date_y, summary_y, total)
}
/// dynamic under-still metadata height for the episode row: the TALLEST episode's flowed meta (title
/// 1–2 lines + date + full summary). Content-derived, so the below-hero flow reflows to fit.
/// CACHED per episode set — the O(episodes) wrap-measure sweep only changes on a detail/season
/// load, but `Column::height` re-asks 2-4× per frame (scroll target + per-child draw).
fn ep_meta_h() -> f32 {
    let d = match metadata::current() {
        Some(d) => d,
        None => return EP_META_TOP + EP_TITLE_DY + EP_META_BOT_PAD,
    };
    // fingerprint the episode set (item + season + count + first leaf); recompute only when it
    // changes. The first episode's rk matters now that season loads are async: cur_season flips
    // before the episodes swap, and two seasons can share a length.
    static mut CACHE: (u64, f32) = (0, 0.0);
    let fp = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.rk.hash(&mut h);
        d.cur_season.hash(&mut h);
        d.episodes.len().hash(&mut h);
        d.episodes.first().map(|e| e.rk.as_str()).unwrap_or("").hash(&mut h);
        h.finish().max(1) // 0 is the "empty" sentinel
    };
    unsafe {
        if (*addr_of_mut!(CACHE)).0 == fp {
            return (*addr_of_mut!(CACHE)).1;
        }
    }
    let mut maxh = 0.0f32;
    for ep in &d.episodes {
        let has_date = !pretty_date(&ep.aired, 0).is_empty();
        let (_, _, total) = ep_meta_layout(&ep.title, has_date, &ep.summary);
        maxh = maxh.max(total);
    }
    let h = EP_META_TOP + maxh + EP_META_BOT_PAD;
    unsafe { *addr_of_mut!(CACHE) = (fp, h) }
    h
}
/// scroll offset that lifts the focused section's top to TOP_MARGIN
fn scroll_target() -> f32 {
    let v = view();
    if v.section == 0 {
        return 0.0;
    }
    // episodes anchor on the season tabs above them, so the tabs stay visible while browsing episodes
    let anchor = if v.section == 2 { 1 } else { v.section };
    let (s, n) = sections();
    // the anchor's non-hero ScrollColumn index; lift its top to margin (== the old section_y math)
    match s[..n].iter().position(|&x| x == anchor) {
        Some(pos) if pos >= 1 => {
            let col = v.column;
            col.lift_target(v, pos - 1)
        }
        _ => 0.0,
    }
}
/// is the loaded item a TV show?
pub(crate) fn is_show() -> bool {
    metadata::current().map(|d| d.is_show).unwrap_or(false)
}

/// Open the detail page for catalog row `idx`: load its full detail (blocking) and
/// reset focus/scroll.
/// Reset focus + all scroll springs to the top of a freshly-opened page — shared by open/open_rk
/// so the two entry points can't drift (the retained-scroll fields must ALL be zeroed on a new item).
fn reset_view_state(v: &mut DetailView) {
    v.section = 0;
    v.col = 0;
    // re-derived by the next `hero_col`; listed here because it is retained hero-row state, even
    // though col is 0 at this point so no shift could fire off a stale value anyway
    v.hero_had_restart = false;
    v.pending_season = -1;
    v.saved_col = [0; 6];
    v.column.scroll.jump(0.0);
    v.ep_hscroll.jump(0.0);
    v.tab_hscroll.jump(0.0);
    // Related and Cast are `CardRow`s, and their scroll offset is retained state exactly like the
    // two h-scrolls above — it just isn't a field we can `jump`, because a CardRow's scroll/lift/
    // per-cell pop springs are private to card_row.rs. Left alone, opening a second item inherited
    // the previous one's horizontal position: a Related row scrolled six posters deep for the last
    // movie opened the next one parked mid-poster, with the first few items off-screen left.
    // Re-seating the whole component is the reset — `CardRow::new()` is the same const value the
    // `DetailView::new()` constructor uses, and its one non-spring field (`base_y`) is written only
    // by home's grid, never by detail (detail's rows sit at the ScrollColumn's local origin).
    v.related = CardRow::new();
    v.cast = CardRow::new();
}

/// Open the detail page for a catalog row. BLOCKING: its only caller is the headless
/// `plxnative-detail` trigger, which replays `move_focus`/`on_ok` in the same frame (and the
/// `detail-transition` FPS scene needs the sections present before `detailosc` starts swinging
/// the scroll — an async load there would measure a static hero and pass vacuously).
pub(crate) fn open(idx: c_int) {
    let v = view();
    v.selected = idx;
    reset_view_state(v);
    if idx >= 0 {
        if let Some(m) = crate::pms::movie(idx as usize) {
            v.mounted_rk = m.rk.clone(); // keep the index reconcilable — see `reselect`
            if !m.rk.is_empty() {
                metadata::load_detail_now(&m.rk);
            }
        }
    }
}

/// Leave the detail page (drop the loaded item).
pub(crate) fn close() {
    metadata::clear();
    let v = view();
    v.selected = -1;
    v.mounted_rk.clear(); // a closed page must not be reconciled by a late refetch
}

pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    let v = view();
    let sec = v.section;
    let col = v.col;
    if sym == SDLK_LEFT || sym == SDLK_RIGHT {
        // About (5) is a 2D block: the card (col 0) has no horizontal neighbours; the three columns
        // (Information/Languages/Accessibility = cols 1..=3) move among themselves.
        if sec == 5 {
            if col >= 1 {
                let nc = if sym == SDLK_LEFT { (col - 1).max(1) } else { (col + 1).min(3) };
                if nc != col {
                    v.col = nc;
                    v.card_scale.jump(1.0);
                }
            }
            return;
        }
        let n = n_items(sec);
        if n <= 0 {
            return;
        }
        let nc = if sym == SDLK_LEFT { (col - 1).max(0) } else { (col + 1).min(n - 1) };
        if nc != col {
            v.col = nc;
            v.card_scale.jump(1.0); // re-pop the newly-focused card
            // focusing a season tab queues that season; update() fetches it once focus settles, so a
            // held LEFT/RIGHT scans the tabs without a blocking fetch on every repeat tick.
            if sec == 1 {
                v.pending_season = nc;
                v.season_settle = 0.0;
            }
        }
    } else if sym == SDLK_UP || sym == SDLK_DOWN {
        // About (5) is a 2D block: DOWN descends from the card (col 0) into the columns row (col 1);
        // UP climbs a column back to the card. Only UP *from the card* falls through to the generic
        // section change (leaving About upward). This makes DOWN — not RIGHT — enter the columns.
        if sec == 5 {
            if sym == SDLK_DOWN {
                if col == 0 {
                    v.col = 1;
                    v.card_scale.jump(1.0);
                }
                return; // columns row is the bottom of the last section
            } else if col >= 1 {
                v.col = 0;
                v.card_scale.jump(1.0);
                return;
            }
            // UP from the card → fall through to leave the section
        }
        let (secs, n) = sections();
        let avail = &secs[..n];
        let pos = avail.iter().position(|&s| s == sec).unwrap_or(0);
        let np = if sym == SDLK_UP { pos.saturating_sub(1) } else { (pos + 1).min(avail.len().saturating_sub(1)) };
        let ns = avail[np];
        if ns != sec {
            // remember where this row was left — only the horizontally-scrolling strips
            // (episodes/related/cast) are ever restored; hero/tabs/About slots stay unused
            if matches!(sec, 2 | 3 | 4) {
                v.saved_col[sec as usize] = col;
            }
            v.section = ns;
            v.card_scale.jump(1.0); // pop the card in the newly-entered row
            // land on the active season when entering the tabs; restore the remembered item for
            // the episode/related/cast rows (their h-scroll held, so the row truly stays put);
            // else the first item
            let start = match ns {
                1 => metadata::current().map(|d| d.cur_season as c_int).unwrap_or(0),
                2 | 3 | 4 => v.saved_col[ns as usize].clamp(0, (n_items(ns) - 1).max(0)),
                _ => 0,
            };
            v.col = start;
        }
    }
}

/// ONE season tab's resolved layout: its index, content-space label x, the tab's CONTENT width
/// (label plus whatever trailing note it carries), the label CString and the note's own pieces.
struct TabLay {
    i: usize,
    x: f32,
    w: f32,
    label: CString,
    /// the season's episode count, pre-rendered; empty when the tab shows no count
    count: CString,
    /// every episode of the season is watched → the note is a tick, not a count
    done: bool,
}
impl TabLay {
    /// This tab's trailing note. Borrows `count`, so it must not outlive the layout entry.
    fn note(&self) -> TabNote {
        if self.done {
            TabNote::Done
        } else if self.count.as_bytes().is_empty() {
            TabNote::None
        } else {
            TabNote::Count(self.count.as_ptr())
        }
    }
}

/// ONE source of truth for the season-tab strip's layout — the draw pass and the focus/scroll
/// geometry both walk this, so their x-advance can't drift. A label that can't be a CString
/// (interior NUL — never in practice) is skipped entirely (not drawn, no advance).
///
/// Each tab carries a quiet second fact past its name: how many episodes the season holds, or a
/// tick once they are all watched (`metadata::Season::watched` is the ONE rule for that, shared
/// with the item-level watched state). A season the server sent no `leafCount` for shows neither.
///
/// COST: this is uncached and runs on the draw path (and again per frame while the tab row holds
/// focus, through `tab_focus_geom`), so it is one `CString` + one `text_width` per season per call
/// — and the count adds a second of each. Both are cheap now that `text::text_width` is a
/// `TTF_SizeUTF8` metrics walk rather than a rasterize+upload, but a many-season strip (Top Gear)
/// is the shape that would want the fingerprinted cache `ep_meta_h` uses, and no FPS scene covers
/// it: `fps:detail-transition` is a MOVIE, which returns from `draw_tabs` before reaching here.
fn tabs_layout(d: &crate::metadata::Detail) -> Vec<TabLay> {
    let mut x = MARGIN_X;
    let mut out = Vec::with_capacity(d.seasons.len());
    for (i, s) in d.seasons.iter().enumerate() {
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        if let Ok(lc) = CString::new(label) {
            let count = if s.watched() || s.leaf_count <= 0 {
                CString::default()
            } else {
                CString::new(s.leaf_count.to_string()).unwrap_or_default()
            };
            let mut lay = TabLay { i, x, w: 0.0, label: lc, count, done: s.watched() };
            lay.w = crate::text::text_width(lay.label.as_ptr(), theme::size::BODY, 1) + TabPill::note_w(lay.note());
            x += lay.w + TAB_ADVANCE;
            out.push(lay);
        }
    }
    out
}

/// content-space geometry (label x, label width) of the focused season tab.
fn tab_focus_geom() -> (f32, f32) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return (MARGIN_X, 0.0),
    };
    let col = view().col.max(0) as usize;
    let mut x_end = MARGIN_X;
    for lay in tabs_layout(d) {
        if lay.i == col {
            return (lay.x, lay.w);
        }
        x_end = lay.x + lay.w + TAB_ADVANCE;
    }
    (x_end, 0.0)
}
/// season-tab-row horizontal scroll target: minimal scroll-into-view (the CardRow rule) — move
/// only when the focused tab's pill (± one tab-gap of context) would clip the viewport; a
/// non-focused row HOLDS its scroll (gliding back to the start read as the row "jumping away"
/// whenever focus left it).
fn tab_hscroll_target() -> f32 {
    let v = view();
    if v.section != 1 {
        return v.tab_hscroll.pos;
    }
    let (x, w) = tab_focus_geom();
    let lo = x + w + 18.0 + TAB_ADVANCE - (SCR_W - MARGIN_X); // pill right edge (+context) on screen
    let hi = x - 18.0 - TAB_ADVANCE - MARGIN_X; // pill left edge (−context) on screen
    card_row::reveal(v.tab_hscroll.pos, lo, hi, f32::MAX) // no content-max: hi already bounds it
}

/// episode-row horizontal scroll target: minimal scroll-into-view via the shared CardRow rule;
/// a non-focused row HOLDS its scroll (it snaps to the start only on a season change — see the
/// `pump_season` reset in update()).
fn ep_hscroll_target() -> f32 {
    let v = view();
    if v.section != 2 {
        return v.ep_hscroll.pos;
    }
    let n = n_items(2).max(0) as usize;
    card_row::scroll_into_view(v.ep_hscroll.pos, v.col.max(0) as usize, n, EP_W, EP_GAP, SCR_W - 2.0 * MARGIN_X)
}

/// The ONE hero-synopsis TextView (rung / leading / line cap live here once) — shared by the
/// flow measure (hero_layout) and the paint (draw_hero) so they cannot diverge.
fn hero_synopsis(summary: &str) -> TextView<'_> {
    TextView::new(summary, theme::size::MICRO, theme::TEXT_SECONDARY).leading(29.0).max_lines(4)
}

/// The loaded item's director credits, or None when the server sent no `Director[]` — the ONE
/// rule for whether the hero carries a "Directed by …" line. The flow (`hero_layout`) reserves the
/// line's band on it and the paint (`draw_hero`) formats it, so the two cannot disagree. Borrowed
/// rather than formatted: `hero_layout` runs twice a frame and has no business allocating.
fn directors() -> Option<&'static [String]> {
    metadata::current().map(|d| d.directors.as_slice()).filter(|v| !v.is_empty())
}

/// The hero text column's y-chain — title bottom (566) → meta (+36) → synopsis (+46, MEASURED) →
/// date (+20) → "Directed by" (+`space::LG`, only when there is one) → buttons (+46) — computed
/// ONCE for both the painter (draw_hero) and the flow (update()'s column.top), so the pixels and
/// the scroll math cannot desync. A 4-line synopsis used to push the Play pill to within 2px of
/// the season tabs; the flow now keeps one standardized gap (`BTN_CONTENT_GAP`) under the buttons
/// for every title.
/// Returns (meta_y, syn_y, date_y, btn_y, dir_y) — `dir_y` is APPENDED rather than slotted into
/// flow order so the existing element indices keep their meaning (`update()` reads `.3` for the
/// button row), and it is only meaningful when [`directors`] is Some.
fn hero_layout(m: Option<&PmsMovie>) -> (f32, f32, f32, f32, f32) {
    let d = metadata::current();
    let owned = if d.is_none() { m.map(|m| m.summary.clone()).unwrap_or_default() } else { String::new() };
    let summary: &str = d.map(|d| d.summary.as_str()).unwrap_or(&owned);
    let syn_h = if summary.is_empty() { 0.0 } else { hero_synopsis(summary).measure_h(HERO_TEXT_W) };
    hero_chain(syn_h, directors().is_some())
}

/// The pure arithmetic half of [`hero_layout`] — everything except the synopsis measurement, which
/// is the one part that needs a font. Split out so the flow-vs-paint contract this chain exists to
/// hold (above all the CONDITIONAL "Directed by" band) is host-testable; measuring is not, since
/// the host suite opens no SDL_ttf.
fn hero_chain(syn_h: f32, has_director: bool) -> (f32, f32, f32, f32, f32) {
    let meta_y = 566.0 + 36.0;
    let syn_y = meta_y + 46.0;
    let date_y = syn_y + syn_h.max(34.0) + 20.0;
    // The credit line sits one air rung under the date/runtime line — a PITCH like its siblings
    // above (these ys are line tops, so a `space::LG` step leaves ~10px of ink gap at CAPTION,
    // which is the "two separate fine-print statements" reading, not a paragraph). It takes NO
    // space at all on an item without directors — the button row keeps its old y there.
    let dir_y = date_y + theme::space::LG;
    let last_y = if has_director { dir_y } else { date_y };
    (meta_y, syn_y, date_y, last_y + 46.0, dir_y)
}

pub(crate) fn update(dt: f32) {
    // apply a landed async season fetch: the episode row starts over on the new list (focus +
    // scroll to episode 0 — the remembered position belonged to the previous season)
    if metadata::pump_season() {
        let v = view();
        v.ep_hscroll.jump(0.0);
        v.saved_col[2] = 0;
        if v.section == 2 {
            v.col = 0;
            v.card_scale.jump(1.0);
        }
    }
    pump_reveal(); // after pump_season: a landing season resets the episode focus this restores
    view().spin_ms += dt * 1000.0;
    // the flow top rides the measured hero: buttons bottom + one standardized gap (hero_layout)
    view().column.top = hero_layout(selected()).3 + CD + BTN_CONTENT_GAP;
    // targets read view() internally — compute them before borrowing v for the springs
    let sct = scroll_target();
    let hst = ep_hscroll_target();
    let tst = tab_hscroll_target();
    let v = view();
    v.column.scroll.step(sct, K_SCROLL, dt);
    crate::ui::anim::probe("detail.scroll", v.column.scroll.pos, v.column.scroll.vel, sct, dt);
    v.card_scale.step(crate::ui::widgets::CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("detail.card", v.card_scale.pos, v.card_scale.vel, crate::ui::widgets::CARD_FOCUS_SCALE, dt);
    // per-episode pop springs: focused cell → CARD_FOCUS_SCALE, all others ease back to 1.0 (so a
    // just-deselected episode animates its pop + lifted shadow out instead of snapping). Step every
    // spring each frame (cheap), same discipline as CardRow.
    let ecol = if v.section == 2 { v.col.max(0) as usize } else { usize::MAX };
    let en = n_items(2).max(0) as usize;
    for (i, sp) in v.ep_scale.iter_mut().enumerate() {
        let t = if i == ecol && i < en { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 };
        sp.step(t, 300.0, dt);
    }
    v.ep_hscroll.step(hst, 240.0, dt);
    crate::ui::anim::probe("detail.epscroll", v.ep_hscroll.pos, v.ep_hscroll.vel, hst, dt);
    v.tab_hscroll.step(tst, 240.0, dt);
    crate::ui::anim::probe("detail.tabscroll", v.tab_hscroll.pos, v.tab_hscroll.vel, tst, dt);
    // Related is the shared home-shelf component now: step its per-card scale springs + scroll spring
    // (focused only when the Related section holds focus, else the scales ease back and scroll freezes).
    let rfoc = (v.section == 3).then_some(v.col.max(0) as usize);
    v.related.update(n_items(3) as usize, rfoc, &RowStyle::HOME, dt);
    // Cast is the same shared shelf component (circular RowStyle::CAST): spring magnification + scroll.
    let cfoc = (v.section == 4).then_some(v.col.max(0) as usize);
    v.cast.update(n_items(4) as usize, cfoc, &RowStyle::CAST, dt);
    // debounced season fetch: load the queued season only after its tab has held focus for a beat, so
    // scanning tabs (a hold, or fast taps) coalesces into ONE blocking `/children` fetch, not one per step.
    if v.pending_season >= 0 {
        v.season_settle += dt;
        if v.season_settle >= SEASON_SETTLE {
            let ps = v.pending_season;
            v.pending_season = -1;
            let cur = metadata::current().map(|d| d.cur_season as c_int).unwrap_or(-1);
            if ps != cur {
                metadata::load_season(ps.max(0) as usize);
            }
        }
    }
}

fn env_of(dt: f32) -> Env {
    Env { dt, screen: Rect::FULL, fr: focus(), fc: 0, sp: 1.0, hero_a: 1.0 }
}

pub(crate) fn draw() {
    let p = Painter::root();
    // clear to the app's base gray (the backdrop art/ambient draw over it; below-hero rows reveal it)
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let env = env_of(0.0);
    let m = selected();
    let scroll = view().column.scroll.pos;
    use crate::ui::profile::phase;
    phase("dt.backdrop", || draw_backdrop(p, m, scroll));
    let hero_a = hero_alpha(scroll, 400.0);
    let ps = p.translate(0.0, -scroll);
    // hero fades out as the page scrolls down into the rows (pinned but scrolled)
    if hero_a > 0.01 {
        phase("dt.hero", || draw_hero(ps.alpha(hero_a), &env, m));
    }
    // compact centered title fades in at the top of the scrolled view (pinned, not scrolled) —
    // but only while the scroll sits in the season/episode band; diving into cast/related/about
    // fades it back out (the logo identifies the show while season-browsing, then gets out of the
    // way — it doesn't fit the tightened section gap anyway).
    if hero_a < 0.99 {
        let la = (1.0 - hero_a) * compact_title_vis(scroll);
        if la > 0.01 {
            phase("dt.ctitle", || draw_compact_title(p.alpha(la), m));
        }
    }
    // Below-hero sections through the shared ScrollColumn: it owns the flow (child_top == the old
    // section_y), the scroll, and the off-screen cull (on_axis) — Painter has no clip/scissor, so a
    // section fully below the fold (~35ms/frame of wasted strings+draws) is SKIPPED, not clipped. Each
    // present section draws local-coord under a painter pre-translated to its child origin (see the
    // `impl Column for DetailView` dispatch). The focused section is never culled.
    let col = view().column;
    col.draw(view(), &env, p);
    // A fetch is in flight with nothing loaded (`open_rk` clears CURRENT so the page never acts
    // on or paints a stale item), so `sections()` is hero-only and everything below the buttons
    // is empty — mark it, the same way the episode row marks a season switch. The hero itself
    // still reads: with `current()` None, `draw_hero`/`hero_layout` fall back to the catalog row
    // for the art, title and summary, so only the content area is genuinely blank.
    //
    // The `current().is_none()` half is what keeps this off the blocking paths, where the item is
    // installed before the next draw and a spinner would flash for one frame.
    if metadata::detail_loading() && metadata::current().is_none() {
        // centred in the empty content area — derived from the flow (column.top is the measured
        // hero bottom), so it follows a taller hero down instead of drifting under it
        let cy = (view().column.top + SCR_H) * 0.5;
        crate::ui::widgets::Spinner::new(SCR_W * 0.5, cy, 26.0)
            .phase(view().spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(&env, ps);
    }
}

// The below-hero sections ARE the ScrollColumn's children (the hero at section id 0 is pinned, drawn
// separately, and excluded here). Column index i maps to the (i+1)-th present section id, so child 0
// is the first non-hero section sitting at CONTENT_TOP.
impl Column for DetailView {
    fn len(&self) -> usize {
        let (_, n) = sections();
        n - 1 // exclude the pinned hero (section id 0 at index 0)
    }
    fn height(&self, i: usize) -> f32 {
        let (s, _) = sections();
        block_h(s[i + 1])
    }
    fn gap_before(&self, i: usize) -> f32 {
        // gap between the previous child (section s[i]) and this one (s[i+1]); tabs→episodes hug
        let (s, _) = sections();
        if s[i] == 1 && s[i + 1] == 2 {
            TAB_EP_GAP
        } else {
            SECTION_GAP
        }
    }
    fn focus_child(&self) -> Option<usize> {
        if self.section == 0 {
            return None; // hero focused → no scrolling child is focused
        }
        let (s, n) = sections();
        s[..n].iter().position(|&x| x == self.section).map(|p| p - 1)
    }
    fn draw_child(&self, i: usize, env: &Env, p: Painter) {
        use crate::ui::profile::phase;
        let (s, _) = sections();
        match s[i + 1] {
            1 => phase("dt.tabs", || draw_tabs(p)),
            // while a season fetch is in flight the row shows the PREVIOUS season's episodes,
            // dimmed, with a spinner — the switch is async so rapid tab hops never freeze the UI
            2 => phase("dt.eps", || {
                if metadata::season_loading() {
                    draw_episodes(p.alpha(0.35));
                    crate::ui::widgets::Spinner::new(SCR_W * 0.5, EP_H * 0.5, 26.0)
                        .phase(self.spin_ms as u32)
                        .tint(theme::TEXT_PRIMARY)
                        .draw(env, p);
                } else {
                    draw_episodes(p);
                }
            }),
            3 => phase("dt.related", || draw_related(p)),
            4 => phase("dt.cast", || draw_cast(p)),
            5 => phase("dt.about", || draw_about(p)),
            _ => {}
        }
    }
}

fn draw_backdrop(p: Painter, m: Option<&PmsMovie>, scroll: f32) {
    // 0 at the hero, 1 when scrolled down into the rows
    let sf = (scroll / SCROLLED).clamp(0.0, 1.0);
    // Overall scroll-darkening for row-text legibility. Folded into the wash + art tints below
    // instead of a separate full-screen dim pass — one fewer full-screen blit per scrolled frame.
    let d = 1.0 - sf * 0.55;
    // Backdrop art: prefer the catalog row's art, else the loaded detail's art. Fades out as the page
    // scrolls into the rows so the episode/row text reads over a dark bg. Resolved FIRST (the texture
    // is cached — this is a cheap lookup) so the ambient wash below can be skipped whenever the opaque
    // full-screen art already covers it — drawing both is a wasted full-screen pass on the weak GPU
    // (the hero's fill-rate cost; mirrors home's Backdrop which draws art OR ambient, never both).
    let art_a = 1.0 - sf;
    let art_tex = if art_a > 0.01 {
        m.filter(|m| !m.art.is_empty())
            .map(|m| m.art.clone())
            .or_else(|| metadata::current().map(|d| d.art.clone()).filter(|s| !s.is_empty()))
            .map(|art| resolve_tex(&art, 1920, 1080, 0))
            .unwrap_or(0)
    } else {
        0
    };
    // ambient wash from the item's UltraBlur corners, dimmed by the scroll (`d`), drawn ONLY when it
    // will actually show: no art texture yet, or the art has faded on scroll enough to reveal it.
    // Because black-over-at-alpha is a linear multiply, dimming each source layer by `d` is
    // pixel-identical to the old separate full-screen dim overlay, one fewer full-screen pass.
    if let Some(m) = m {
        if m.has_blur && (art_tex == 0 || art_a < 0.99) {
            p.ambient(Rect::FULL, 0.55 * d, m.blur);
        }
    }
    if art_tex != 0 {
        p.tex(art_tex, Rect::FULL, 0.0, theme::with_a(theme::dim(theme::TINT_WHITE, d), art_a)); // white dimmed by scroll `d`
    }
    // bottom scrim for the hero's lower-left content — tied to the hero's own fade (it serves that
    // text), so it clears once the hero is gone rather than lingering the whole scroll (a wasted
    // full-screen-width pass through the back half of the transition).
    let hero_vis = hero_alpha(scroll, 400.0);
    if hero_vis > 0.01 {
        p.rect(
            Rect::new(0.0, SCR_H * 0.34, SCR_W, SCR_H * 0.66),
            0.0,
            theme::scrim(0.0),
            theme::scrim(0.95 * hero_vis),
            0.0,
        );
    }
    // the shelf gray fades in as the page scrolls off the hero, so the below-hero rows sit on the
    // app's base gray (their Related/Cast card shadows read) regardless of the item's backdrop art or
    // UltraBlur wash. Fully transparent in the hero view, so no wasted fill there.
    if sf > 0.001 {
        let g = theme::with_a(theme::SURFACE_APP, sf);
        p.rect(Rect::FULL, 0.0, g, g, 0.0);
    }
}

fn draw_hero(p: Painter, env: &Env, m: Option<&PmsMovie>) {
    let tx = MARGIN_X;
    let w_a = theme::TEXT_PRIMARY;
    let d_a = theme::TEXT_SECONDARY;
    let dim = theme::TEXT_TERTIARY;
    let d = metadata::current();

    // ---- title: clearLogo (transparent PNG) if loaded, else bold text. Borrow from the loaded
    // Detail (the common case) — cloning rk/title/summary every frame was pure churn; the catalog
    // fallback (`m`, pre-load frames only) clones from the catalog row. ----
    let title_bottom = 566.0f32;
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let mut drew_logo = false;
    if let Some((lt, ww, hh)) = crate::posters::logo_tex(rk, 680.0, 120.0) {
        p.tex(lt, Rect::new(tx, title_bottom - hh, ww, hh), 0.0, w_a);
        drew_logo = true;
    }
    if !drew_logo {
        let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
        let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
        if let Ok(t) = CString::new(title) {
            p.text(t.as_ptr(), tx, title_bottom - 68.0, theme::size::HERO, w_a, 0, 1);
        }
    }

    // ---- meta line: "TV Show · Sci-Fi · Adventure · 18+" ----
    let mut parts: Vec<&str> = Vec::new();
    if let Some(d) = d {
        parts.push(if d.is_show { "TV Show" } else { "Movie" });
        for g in d.genres.iter().take(2) {
            parts.push(g);
        }
        if !d.rating.is_empty() {
            parts.push(&d.rating);
        }
    }
    // the whole y-chain below comes from the ONE shared layout (also feeds the scroll flow)
    let (meta_y, syn_y, date_y, btn_y, dir_y) = hero_layout(m);
    if let Ok(mc) = CString::new(parts.join("   \u{b7}   ")) {
        p.text(mc.as_ptr(), tx, meta_y, theme::size::BODY, d_a, 0, 0);
    }

    // ---- synopsis: the shared fine-print TextView (hero_synopsis — one size app-wide, matches
    // the home hero); its measured height is already folded into date_y/btn_y by hero_layout ----
    let summary_owned = if d.is_none() { m.map(|m| m.summary.clone()).unwrap_or_default() } else { String::new() };
    let summary: &str = d.map(|d| d.summary.as_str()).unwrap_or(&summary_owned);
    if !summary.is_empty() {
        hero_synopsis(summary).draw(p, Rect::new(tx, syn_y, HERO_TEXT_W, 0.0));
    }

    // ---- date · runtime · media badges ----
    if let Some(d) = d {
        let mut info = pretty_date(&d.aired, d.year);
        if d.dur_ms >= 60_000 {
            if !info.is_empty() {
                info.push_str("    \u{b7}    ");
            }
            info.push_str(&crate::ui::fmt::dur_long(d.dur_ms));
        }
        let mut bx = tx;
        if let Ok(ic) = CString::new(info) {
            let w = p.text(ic.as_ptr(), tx, date_y, theme::size::CAPTION, dim, 0, 0);
            if w > 0.0 {
                bx += w + theme::space::MD;
            }
        }
        draw_media_badges(p, d, bx, date_y);
    }

    // ---- "Directed by …" — the crew credit the reference puts in the metadata block. A step
    // brighter than the date/runtime fine print above it (these are names, not filing data), and
    // elided to the hero text column so a five-director title can't run under the Starring line.
    // Its band is reserved by hero_layout, so an item without directors moves nothing. ----
    if let Some(names) = directors() {
        let line = crate::text::elide(
            &format!("Directed by {}", names.join(", ")),
            HERO_TEXT_W,
            theme::size::CAPTION,
            0,
            false,
        );
        if let Ok(dc) = CString::new(line) {
            p.text(dc.as_ptr(), tx, dir_y, theme::size::CAPTION, d_a, 0, 0);
        }
    }

    // ---- buttons (btn_y from hero_layout) ----
    draw_buttons(p, env, btn_y);

    // ---- "Starring …" right-aligned near the bottom-right ----
    if let Some(d) = d {
        if !d.cast.is_empty() {
            let names: Vec<String> = d.cast.iter().take(3).map(|c| c.tag.clone()).collect();
            if let Ok(sc) = CString::new(format!("Starring {}", names.join(", "))) {
                // right-aligned against the right margin (measured directly, no invisible fake-draw)
                let w = crate::text::text_width(sc.as_ptr(), theme::size::LABEL, 1);
                p.text(sc.as_ptr(), SCR_W - MARGIN_X - w, btn_y + 16.0, theme::size::LABEL, d_a, 0, 1);
            }
        }
    }
}

/// The hero's media chips, flowed left→right from `x` and cap-band-centred on the text line drawn
/// at `text_y` (so they sit ON the date/runtime line, not near it). Returns the width consumed.
///
/// They describe the item's PRIMARY version — `Media[0]`, see `plex::Metadata::primary_media` — so
/// a multi-version item (the dev library has episodes with both a 4k and a 1080 version) is badged
/// by whichever one PMS lists first, until there is a version picker to choose with. Today the row
/// is the resolution alone; the audio/subtitle chips the reference client shows beside it are the
/// same `badge` leaf and belong here when they land.
fn draw_media_badges(p: Painter, d: &metadata::Detail, x: f32, text_y: f32) -> f32 {
    let mut bx = x;
    // one Option per chip — the audio/subtitle chips the reference client shows beside the
    // resolution join this list (and nothing else changes) when they land
    let chips = [crate::ui::fmt::resolution(&d.video_resolution, d.width, d.height)];
    for text in chips.into_iter().flatten() {
        // the gap separates chips, so it is never left dangling after the last one
        if bx > x {
            bx += theme::space::XS;
        }
        // cap-band centre of the line drawn at `text_y` (the inverse of text::text_vcenter_y),
        // computed here rather than up front so an empty row touches no glyph cache
        let (cap_top, baseline) = crate::text::text_cap_band(theme::size::CAPTION, 0);
        let cy = text_y + (cap_top + baseline) * 0.5;
        bx += crate::ui::widgets::badge(p, bx, cy, &text, crate::ui::widgets::BadgeStyle::Filled);
    }
    bx - x
}

fn draw_buttons(p: Painter, env: &Env, y: f32) {
    // One shared control family (widgets::Button/CircleButton, default ControlStyle::Accent — the
    // same as the info card): the focused control fills warm ACCENT with dark ink, the rest are
    // solid dark discs. Play reuses the pill Button; the ↺ disc restarts a resumable item from
    // 00:00; the check disc is the watched toggle (its glyph lights RESUME amber while the item is
    // marked watched). The discs are placed by an ACCUMULATED x, not per-control constants, because
    // the middle one comes and goes — see `hero_btns`.
    let tx = MARGIN_X;
    let focus = focus();
    let restart = has_restart();

    // The pill says what the press will DO. Home's hero says "Continue" — that row belongs to the
    // Continue Watching shelf, and the word is the shelf's. On an item page the control has a
    // sibling: "Resume" and the ↺ disc beside it are a PAIR, read together, the way every reference
    // client words it. The pill's width follows its label (widgets::pill_w, the one hero-pill
    // formula) so the longer word gets its own air instead of being crammed into "Play"'s frame.
    let plabel = if restart { c"Resume" } else { c"Play" };
    let pw = crate::ui::widgets::pill_w(plabel.as_ptr(), theme::size::BODY).max(PW);
    let mut cx = tx + pw + CGAP;

    Button::new(plabel.as_ptr(), theme::size::BODY, Rect::new(tx, y, pw, CD))
        .icon(crate::ui::icons::Icon::Play)
        .focused(focus == BTN_PLAY)
        .draw(env, p);
    if restart {
        CircleButton::new(c"".as_ptr())
            .icon(crate::ui::icons::Icon::Restart)
            .at(cx, y)
            .focused(focus == BTN_RESTART)
            .draw(env, p);
        cx += CD + CGAP;
    }
    let watched = metadata::current().map(|d| d.watched).unwrap_or(false);
    let wf = focus == btn_watched();
    let wstyle = if watched && !wf {
        ControlStyle::Custom { fill: theme::CONTROL_IDLE_FILL, ink: theme::RESUME_FILL }
    } else {
        ControlStyle::Accent
    };
    CircleButton::new(c"".as_ptr())
        .icon(crate::ui::icons::Icon::Check)
        .style(wstyle)
        .at(cx, y)
        .focused(wf)
        .draw(env, p);
}

/// Visibility (0..1) of the pinned compact title at scroll depth `scroll`: 1 while the first
/// below-hero sections (season tabs + episodes) hold the top, fading to 0 across the 300px before
/// the first DEEP section (cast/related/about) lifts to the top margin.
fn compact_title_vis(scroll: f32) -> f32 {
    let v = view();
    let (s, n) = sections();
    let hide_at = s[..n]
        .iter()
        .position(|&x| x >= 3) // first deep section id (cast=4 / related=3 / about=5)
        .filter(|&pos| pos >= 1)
        .map(|pos| {
            let col = v.column;
            col.lift_target(v, pos - 1)
        })
        .unwrap_or(f32::MAX);
    ((hide_at - scroll) / 300.0).clamp(0.0, 1.0)
}

/// small centered clearLogo/title shown at the top once the page is scrolled
fn draw_compact_title(p: Painter, m: Option<&PmsMovie>) {
    let d = metadata::current();
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let cx = SCR_W * 0.5;
    if let Some((lt, ww, hh)) = crate::posters::logo_tex(rk, f32::MAX, 54.0) {
        p.tex(lt, Rect::new(cx - ww * 0.5, 40.0, ww, hh), 0.0, theme::TEXT_PRIMARY);
        return;
    }
    let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
    let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
    if let Ok(t) = CString::new(title) {
        p.text(t.as_ptr(), cx, 54.0, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
    }
}

/// season tab row: active season bright/bold, focused tab gets a highlight pill
fn draw_tabs(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.seasons.is_empty() {
        return;
    }
    let sec = view().section;
    let col = view().col;
    let tab_y = 0.0; // local origin — the ScrollColumn pre-translates the painter to this section's top
    // many-season shows (Top Gear) overflow the row width, so the whole strip scrolls horizontally to
    // keep the focused tab visible (spring target in `tab_hscroll_target`); off-screen tabs are culled.
    let sx = view().tab_hscroll.pos;
    let pt = p.translate(-sx, 0.0);
    // segmented control: the *selected* season carries a subtle pill (bright ACCENT while the tab row
    // is focused); non-selected seasons are plain dim text. (TabPill handles the state → look.)
    let e = Env::inert();
    for lay in tabs_layout(d) {
        let selected = lay.i == d.cur_season;
        let focused = sec == 1 && col == lay.i as c_int;
        // pill sized to the (bold) label plus its trailing note — content sits at x, pill padded
        // ±18, tabs advance by that content width + TAB_ADVANCE (see tabs_layout). The pill fills
        // the block height (TAB_ROW_H == the hero-button CD), so tabs and buttons read as one
        // control family.
        if on_axis(lay.x - 18.0 - sx, lay.w + 36.0, SCR_W, 0.0) {
            TabPill::new(lay.label.as_ptr(), theme::size::BODY, Rect::new(lay.x - 18.0, tab_y, lay.w + 36.0, TAB_ROW_H))
                .segment(selected)
                .focused(focused)
                .note(lay.note())
                .draw(&e, pt);
        }
    }
}

/// horizontal row of landscape episode cards with under-card metadata
fn draw_episodes(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.episodes.is_empty() {
        return;
    }
    let sec = view().section;
    let col = view().col;
    let focus_col = if sec == 2 { col } else { -1 };
    // the ui::press click dip folds into the focused episode's per-cell pop below
    let press = crate::ui::press::scale();
    // keep the focused card on-screen (spring-scrolled so it glides to the 2nd slot instead of
    // snapping — matches the chapters strip; fixes the "scatter" on LEFT/RIGHT)
    let sx = view().ep_hscroll.pos;
    let pe = p.translate(-sx, 0.0);
    let dimc = theme::TEXT_TERTIARY;
    let ep_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    for (i, ep) in d.episodes.iter().enumerate() {
        let x = MARGIN_X + i as f32 * (EP_W + EP_GAP);
        if !on_axis(x - sx, EP_W, SCR_W, 0.0) {
            continue; // off-screen
        }
        let focused = i as c_int == focus_col;
        // per-cell pop: the focused episode springs up (press dip folded in); a just-deselected one
        // springs back down, so its scale + lifted shadow animate out instead of snapping. Episodes
        // past the spring cap snap to the focus scale (no per-cell animation) rather than losing the
        // focus cue entirely.
        let base = view()
            .ep_scale
            .get(i)
            .map(|s| s.pos)
            .unwrap_or(if focused { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 });
        let sc = if focused { base * press } else { base };
        // the focused cell is always "popped" so a press dip (sc < 1) still scales it (the dip would
        // otherwise be clamped away); a deselecting cell stays popped until its spring settles back.
        let popped = focused || sc > 1.001;
        let card = Rect::new(x, ep_y, EP_W, EP_H);
        // episode still + focus ring + scale-pop (shared with the chapters strip)
        crate::ui::widgets::draw_card(pe, card, &ep.thumb, (640, 360), 12.0, popped, sc);
        // Playback state on the still — ONE mark at a time, resolved in the same order the posters
        // resolve theirs in `widgets::card`: a resume point wins (a part-watched re-run is what the
        // viewer is in the middle of), otherwise a finished episode carries the watched check.
        // Without this, a fully-watched episode looked exactly like one never started. Both marks
        // track the scaled card while it's popped.
        let cr = if popped { card.scaled(sc) } else { card };
        if ep.resume_ms > 0 && ep.dur_ms > 0 {
            let frac = (ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0);
            let bar = Rect::new(cr.x + 12.0, cr.y + cr.h - 16.0, cr.w - 24.0, 5.0);
            // the CARD-BOTTOM resume pair, which is what these two tokens are documented for —
            // not the player scrubber's `RAIL_*`, which this strip used to borrow. Amber is the
            // app's one watched-state hue (the poster shelves' resume bar and unwatched angle, the
            // watched toggle's check, the badge above), so a white bar here left this tile the only
            // place in the product speaking a second one.
            pe.rrect(bar, 2.5, 2.5, theme::RESUME_TRACK);
            pe.rrect(Rect::new(bar.x, bar.y, bar.w * frac, bar.h), 2.5, 2.5, theme::RESUME_FILL);
        } else if ep.watched {
            crate::ui::widgets::watched_badge(pe, cr);
        }
        // under-card metadata: kicker → title → full summary → air date. The summary flows off the
        // ACTUAL title height (1 or 2 lines) — nothing is reserved, so a 1-line title doesn't leave
        // a gap — and the fine-print date (MICRO) closes the block at the bottom. The block height
        // tracks the tallest episode via ep_meta_h.
        let ty = ep_y + EP_H + EP_META_TOP;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let date = pretty_date(&ep.aired, 0); // formatted once — feeds both the layout and the draw
        let (date_y, summary_y, _) = ep_meta_layout(&ep.title, !date.is_empty(), &ep.summary);
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, theme::size::CAPTION, dimc, 0, 1);
        }
        // title wraps to at most 2 lines (elided past that) — long titles used to run into the next card
        TextView::new(&ep.title, theme::size::LABEL, titc)
            .bold()
            .leading(EP_TITLE_LEAD)
            .max_lines(2)
            .draw(pe, Rect::new(x, ty + EP_TITLE_DY, EP_W, 0.0));
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, theme::size::CAPTION, dimc)
                .leading(EP_SUMMARY_LEAD)
                .max_lines(EP_SUMMARY_MAXLINES)
                .draw(pe, Rect::new(x, ty + summary_y, EP_W, 0.0));
        }
        if !date.is_empty() {
            if let Ok(dc) = CString::new(date) {
                pe.text(dc.as_ptr(), x, ty + date_y, theme::size::MICRO, dimc, 0, 0);
            }
        }
    }
}

/// The ONE two-pass strip loop for detail's CardRow shelves (Related, Cast): non-focused tiles
/// first (off-axis culled), the focused tile LAST for its z-order (the in-row focused-last rule —
/// a lone strip has no cross-row neighbour). `art` supplies each tile's Art; `title` the focused
/// tile's under-title (Related; None for cast); `extra` any per-tile caption drawn for EVERY tile
/// (cast names/roles; a no-op for Related). `axis_span` widens the cull band where captions
/// should survive slightly off-screen.
#[allow(clippy::too_many_arguments)]
fn draw_strip<'a>(
    p: Painter,
    row: &CardRow,
    n: usize,
    focus_col: c_int,
    row_y: f32,
    size: (f32, f32),
    pitch: f32,
    sty: &RowStyle,
    axis_span: f32,
    art: impl Fn(usize) -> Art<'a>,
    title: impl Fn(usize) -> Option<CString>,
    extra: impl Fn(Painter, usize, f32, bool),
) {
    let sx = row.scroll_x();
    let pr = p.translate(-sx, 0.0);
    for i in 0..n {
        if i as c_int == focus_col {
            continue; // focused tile drawn last
        }
        let x = MARGIN_X + i as f32 * pitch;
        if !on_axis(x - sx, size.0, axis_span, 0.0) {
            continue;
        }
        let s = row.scale(i);
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        card_row::draw_tile(pr, art(i), rect, s, sty, None);
        extra(pr, i, x, false);
    }
    if focus_col >= 0 && (focus_col as usize) < n {
        let i = focus_col as usize;
        let x = MARGIN_X + i as f32 * pitch;
        // fold the ui::press click dip into the focused tile's scale (1.0 when idle) — same as home
        let s = row.scale(i) * crate::ui::press::scale();
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        let tc = title(i); // kept alive across the draw call
        let tp = tc.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
        card_row::draw_focused(pr, art(i), rect, s, sty, None, tp, std::ptr::null(), false);
        extra(pr, i, x, true);
    }
}

/// "Related" — a horizontal row of portrait poster cards from the related hub. A real instance of
/// the home shelf: view().related owns the animated per-card scale springs + scroll spring
/// (RowStyle::HOME), so it pops + glides + rings + titles exactly like a home shelf.
fn draw_related(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.related.is_empty() {
        return;
    }
    let related_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let focus_col = if view().section == 3 { view().col } else { -1 };
    let lift = view().related.lift();
    p.text(c"Related".as_ptr(), MARGIN_X, related_y - lift, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    draw_strip(
        p,
        &view().related,
        d.related.len(),
        focus_col,
        related_y + REL_LABEL_H,
        (REL_W, REL_H),
        REL_W + REL_GAP,
        &RowStyle::HOME,
        SCR_W,
        |i| Art::Thumb { key: &d.related[i].thumb, res: (250, 375) },
        |i| CString::new(d.related[i].title.clone()).ok(),
        |_, _, _, _| {},
    );
}

/// "Cast & Crew" — circular headshots with names, on the shared [`CardRow`] (circular
/// `RowStyle::CAST`): spring focus-magnification + animated scroll + glow ring, exactly like the
/// poster/Related shelves. Cast shows every member's name/role (a poster row titles only the
/// focused tile), so the labels ride the `extra` hook.
///
/// The row is the item's whole CREDIT list — actors first, then the crew the server sent
/// (`metadata::Detail::credit`), which is what finally makes the heading true. The sub-caption is
/// the character for an actor and the job ("Director", "Writer") for a crew member, because crew
/// rows carry no `role` on the wire; both ride the same `cast_label`.
fn draw_cast(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let n = d.credits_len();
    if n == 0 {
        return;
    }
    let cast_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let focus_col = if view().section == 4 { view().col } else { -1 };
    let lift = view().cast.lift();
    p.text(c"Cast & Crew".as_ptr(), MARGIN_X, cast_y - lift, theme::size::HEADLINE, theme::TEXT_HEADING, 0, 1);
    let row_y = cast_y + CAST_LABEL_H;
    draw_strip(
        p,
        &view().cast,
        n,
        focus_col,
        row_y,
        (CAST_D, CAST_D),
        CAST_SLOT,
        &RowStyle::CAST,
        SCR_W + CAST_D,
        // Art::Person, not Thumb: plenty of crew (and some actors) have no headshot on the
        // server, and the person glyph says "no photo" where a bare placeholder disc said
        // "broken image". The absolute metadata-static.plex.tv URL rides the SAME server-side
        // /photo/:/transcode fetch the actor headshots already use.
        |i| Art::Person { key: d.credit(i).map_or("", |c| c.thumb.as_str()), res: (300, 300) },
        |_| None,
        |pc, i, x, focused| {
            if let Some(c) = d.credit(i) {
                cast_label(pc, &c.tag, &c.role, x + CAST_D * 0.5, row_y, focused);
            }
        },
    );
}

/// A credit's name + sub-caption (an actor's character, or a crew member's job), centred under the
/// headshot and elided to the per-member slot (long names like "Benedict Cumberbatch" would
/// otherwise run into the neighbour at couch-legible sizes).
fn cast_label(p: Painter, name: &str, role: &str, cx: f32, row_y: f32, focused: bool) {
    let name_c = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let budget = CAST_SLOT - 12.0;
    if let Ok(nc) = CString::new(crate::text::elide(name, budget, theme::size::LABEL, 1, false)) {
        p.text(nc.as_ptr(), cx, row_y + CAST_D + 26.0, theme::size::LABEL, name_c, 1, if focused { 1 } else { 0 });
    }
    if !role.is_empty() {
        // measured at the weight it is DRAWN (regular): measuring bold cut it a word early, which
        // the longer crew captions ("Director, Writer") hit far more often than a character name
        if let Ok(rc) = CString::new(crate::text::elide(role, budget, theme::size::CAPTION, 0, false)) {
            p.text(rc.as_ptr(), cx, row_y + CAST_D + 58.0, theme::size::CAPTION, theme::TEXT_TERTIARY, 1, 0);
        }
    }
}

/// resume position (ns) for the last on_ok play case (0 = from the beginning). app.rs
/// captures this after start_bufferfeed and seeks there once the pipeline is ready.
pub(crate) fn last_resume_ns() -> i64 {
    view().last_resume_ns
}
/// apply Plex's resume rule (metadata::resume_ns) and stash the position for app.rs.
fn set_resume(resume_ms: i64, dur_ms: i64) {
    view().last_resume_ns = metadata::resume_ns(resume_ms, dur_ms);
}
/// The resume `on_ok` finally leaves for `app.rs`. Both play arms bake `metadata::resume_ns` into
/// `last_resume_ns` — the movie one via `set_resume`, the episode one inside `play_episode_at` — so
/// Restart drops it AFTER the fact and covers them both with one statement, rather than threading a
/// from-start flag through two call sites that would then have to be kept in step forever.
fn commit_resume(from_start: bool) {
    if from_start {
        view().last_resume_ns = 0;
    }
}

/// OK/SELECT on the detail page: returns true if playback should start (the route
/// URL/HUD have already been set). Section 0 = hero Play, 1 = season tab, 2 = episode.
/// Is the focused element a CARD (episode / Related / Cast tile) rather than the Play pill, a season
/// tab, or an About row? Card focus gets the tvOS press — the OK dips it and the activation commits on
/// release (app.rs); the non-card controls activate immediately.
pub(crate) fn focus_is_card() -> bool {
    matches!(view().section, 2 | 3 | 4)
}

pub(crate) fn on_ok() -> bool {
    view().last_resume_ns = 0; // default: no resume (set below for plays)
    let sec = view().section;
    let col = view().col;
    match sec {
        0 => {
            // the hero row's indices move with its control set, so resolve the press through the
            // one pure mapping (`hero_col` also corrects a focus the set shrank under)
            let action = hero_action(hero_col());
            if action == HeroAction::Watched {
                // watched toggle: scrobble/unscrobble, then re-fetch so the button state, the
                // resume bars and the Continue Watching shelf all reflect the new view state.
                if let Some((rk, watched, season)) =
                    metadata::current().map(|d| (d.rk.clone(), d.watched, d.cur_season))
                {
                    if watched {
                        crate::plex::client().unscrobble(&rk);
                    } else {
                        crate::plex::client().scrobble(&rk);
                    }
                    // BLOCKING: the season restore below reads the refreshed item, and a
                    // deferred landing would install cur_season 0 on top of it — clobbering the
                    // season the user was browsing, which is exactly what this arm prevents.
                    metadata::load_detail_now(&rk);
                    if season != 0 {
                        metadata::load_season(season); // keep the season the user was browsing
                    }
                    crate::pms::refetch_hubs_reconcile(); // CW shelf changes with the view state
                }
                return false;
            }
            // Restart (↺) is the SAME play the pill performs, with the resume rule dropped —
            // applied by `commit_resume` once the arm has run (app.rs reads `last_resume_ns` only
            // after `on_ok` returns, which is what makes the after-the-fact override sound).
            let from_start = action == HeroAction::Restart;
            if !from_start && action != HeroAction::Play {
                return false; // watched handled above; everything else inert
            }
            let started = if is_show() {
                play_episode_at(0)
            } else {
                if let Some(m) = selected() {
                    crate::route::request_play_movie(m);
                } else if let Some(d) = metadata::current() {
                    // a hub refetch can orphan the page's catalog row (the item left the hubs) —
                    // the loaded Detail carries everything the string-based route entry needs
                    crate::route::request_play(&d.rk, &d.part, &d.vcodec, &d.acodec, &d.title, "");
                } else {
                    return false;
                }
                set_resume(
                    metadata::current().map(|d| d.resume_ms).unwrap_or(0),
                    metadata::current().map(|d| d.dur_ms).unwrap_or(0),
                );
                true
            };
            commit_resume(from_start);
            started
        }
        1 => {
            // only a season that isn't already selected loads — OK on the highlighted tab used to
            // fire a redundant refetch whose pump-apply then reset the retained episode position
            let cur = metadata::current().map(|d| d.cur_season as c_int).unwrap_or(-1);
            if col != cur {
                metadata::load_season(col.max(0) as usize);
            }
            view().pending_season = -1; // explicit selection → cancel the debounced load
            false
        }
        2 => play_episode_at(col),
        3 => {
            // Related: open that item's detail page in place
            let rk = metadata::current().and_then(|d| d.related.get(col.max(0) as usize)).map(|r| r.rk.clone());
            if let Some(rk) = rk {
                open_rk(&rk);
            }
            false
        }
        _ => false, // cast (4): headshots are not actionable
    }
}

/// Re-resolve the selected catalog row after a hub refetch rebuilt the catalog underneath an open
/// detail page (indices move; the rk is the stable identity).
pub(crate) fn reselect() {
    // Fall back to the mounted rk when the fetch is still in flight: `open_rk` clears CURRENT for
    // that whole window on purpose, and a refetch landing inside it would otherwise leave
    // `selected` pointing into the OLD catalog's ordering. Re-pointing an index only ever needed
    // the item's identity, never its loaded detail.
    let rk = metadata::current()
        .map(|d| d.rk.clone())
        .unwrap_or_else(|| view().mounted_rk.clone());
    if !rk.is_empty() {
        view().selected = crate::pms::index_of_rk(&rk);
    }
}

/// Point the page at `rk` — catalog row (for the backdrop art/blur) + fresh focus/scroll. The
/// two `open_rk*` twins share it so they cannot drift; only the fetch differs.
fn mount_rk(rk: &str) {
    let idx = crate::pms::index_of_rk(rk);
    let v = view();
    v.selected = idx;
    v.mounted_rk = rk.to_string();
    reset_view_state(v);
}

/// Re-open the detail page for an arbitrary ratingKey (e.g. a Related item). Uses the
/// catalog row for the backdrop art/blur when the item is in the browse catalog, else
/// falls back to the loaded detail's own art (no blur).
///
/// ASYNC: the page mounts this frame on the catalog row and the sections fill in when
/// `metadata::pump_detail` lands the fetch. Callers that must read `metadata::current()` before
/// the next statement want [`open_rk_now`] instead.
pub(crate) fn open_rk(rk: &str) {
    mount_rk(rk);
    // DROP THE OLD ITEM FIRST. The page is now pointed at `rk`, but the fetch takes 2-5 round
    // trips — and for that whole window every `current()` reader would otherwise still see the
    // PREVIOUS item while the page claims to be this one. That is not cosmetic: `on_ok`'s Play
    // arm would play the old item (or the new one at the old one's resume offset), the watched
    // toggle would scrobble it on the server, `draw_hero` prefers `current()` over the catalog
    // row so the whole hero would paint it, and `reselect()` would repoint `selected` back at it.
    // With CURRENT cleared, every one of those reads None and correctly does nothing until the
    // landing — the same "refuse the press while the data is in flight" rule `play_episode_at`
    // already applies during a season switch.
    //
    // ORDER MATTERS: `clear()` supersedes the detail generation, so it must run BEFORE the
    // request that establishes the new one, or it would cancel the fetch it is preparing for.
    metadata::clear();
    metadata::request_detail(rk);
}

/// [`open_rk`] but BLOCKING — for the callers that act on the loaded item in the SAME frame:
/// [`open_rk_season`] (its `load_season_now` indexes `d.seasons`), home_activate's play-a-show
/// arm (gated on `current().rk == expect`), and the headless `plxnative-detail` trigger (which
/// replays move_focus/on_ok immediately). Each of those is a deliberate freeze.
pub(crate) fn open_rk_now(rk: &str) {
    mount_rk(rk);
    metadata::load_detail_now(rk);
}

/// Open the SHOW detail page for `show_rk` with the season numbered `season_num` selected
/// (a season entry point routes to the show page, not a standalone season page).
/// A show page opened FROM playback, landing on the episode that was playing.
///
/// The seasons and the episode list arrive from separate fetches, neither of which has landed when
/// the player exits, so the request is RECORDED and applied by [`pump_reveal`] as the data shows up.
/// The alternative — the blocking `open_rk_season` the home path uses — would put 2-5 PMS round
/// trips on the BACK keypress, on the same frame that is already tearing the pipeline down.
static mut REVEAL: Option<(String, i64, String)> = None; // show rk, season index, episode rk

pub(crate) fn open_show_at_episode(show_rk: &str, season: i64, ep_rk: &str) {
    open_rk(show_rk);
    unsafe { *addr_of_mut!(REVEAL) = Some((show_rk.to_string(), season, ep_rk.to_string())) }
}

/// Apply a pending reveal once its data lands. Two stages, because the season tab and the episode
/// row come from different fetches; runs AFTER `pump_season` so it wins the focus reset that a
/// landing season performs.
fn pump_reveal() {
    let want = match unsafe { (*addr_of!(REVEAL)).clone() } {
        Some(w) => w,
        None => return,
    };
    let (show_rk, season, ep_rk) = want;
    // Everything is read out of `current()` BEFORE `load_season` is called: that write goes through
    // the same static this borrow came from.
    let state = metadata::current().map(|d| {
        (
            d.rk.clone(),
            d.seasons.iter().position(|s| s.index == season),
            d.seasons.is_empty(),
            d.cur_season,
            d.episodes.iter().position(|e| e.rk == ep_rk),
            d.episodes.is_empty(),
        )
    });
    let Some((rk, season_pos, no_seasons, cur_season, ep_pos, no_eps)) = state else { return };
    if rk != show_rk {
        unsafe { *addr_of_mut!(REVEAL) = None }; // the page moved on — not ours to steer
        return;
    }
    if no_seasons {
        return; // the show fetch is still in flight
    }
    if let Some(pos) = season_pos {
        if cur_season != pos {
            if !metadata::season_loading() {
                metadata::load_season(pos);
            }
            return; // that season's episodes aren't here yet
        }
    }
    if let Some(i) = ep_pos {
        let v = view();
        v.section = 2; // the episode row
        v.col = i as c_int;
        v.saved_col[2] = i as c_int;
        v.card_scale.jump(1.0);
        unsafe { *addr_of_mut!(REVEAL) = None };
    } else if !no_eps {
        // the season landed and it isn't in there — stop trying rather than spin
        unsafe { *addr_of_mut!(REVEAL) = None };
    }
}

pub(crate) fn open_rk_season(show_rk: &str, season_num: c_int) {
    open_rk_now(show_rk); // BLOCKING: the season lookup below indexes the loaded item's seasons
    if let Some(d) = metadata::current() {
        if let Some(pos) = d.seasons.iter().position(|s| s.index as c_int == season_num) {
            // BLOCKING on purpose: home_activate's season arm plays episodes[0] right after this
            // returns — the async tab-switch path would still hold the previous season's list
            metadata::load_season_now(pos);
        }
    }
}

fn play_episode_at(i: c_int) -> bool {
    // a season fetch is in flight: d.episodes is still the PREVIOUS season's list (drawn dimmed
    // under a spinner) while cur_season/the tab highlight already show the new one — launching
    // from it would play the wrong season's episode. Ignore the press; the row lands in a beat.
    if metadata::season_loading() {
        return false;
    }
    let d = match metadata::current() {
        Some(d) => d,
        None => return false,
    };
    let ep = match d.episodes.get(i.max(0) as usize) {
        Some(e) => e,
        None => return false,
    };
    let show = d.title.clone();
    let hud_title = if ep.title.is_empty() { show.clone() } else { ep.title.clone() };
    let hud_ctx = format!("{}  \u{b7}  S{} E{}", show, ep.season, ep.index);
    // describe the playing episode for the in-player Info card (current() stays on the show here)
    let ep_year = ep.aired.get(0..4).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    metadata::set_now_playing(Some(metadata::NowPlaying {
        is_episode: true,
        title: show.clone(),
        ep_title: ep.title.clone(),
        season: ep.season,
        index: ep.index,
        summary: ep.summary.clone(),
        year: ep_year,
        dur_ms: ep.dur_ms,
        rating: ep.rating.clone(),
        thumb: ep.thumb.clone(),
        detail_rk: d.rk.clone(),
    }));
    set_resume(ep.resume_ms, ep.dur_ms);
    crate::route::request_play(&ep.rk, &ep.part, &ep.vcodec, &ep.acodec, &hud_title, &hud_ctx);
    true
}

// ---- About footer (section 5): heading + card + Information/Languages/Accessibility ----

fn text_at(p: Painter, x: f32, y: f32, sz: c_int, col: [f32; 4], bold: c_int, s: &str) -> f32 {
    match CString::new(s) {
        Ok(t) => p.text(t.as_ptr(), x, y, sz, col, 0, bold),
        Err(_) => 0.0,
    }
}

/// a dim label over one/two pixel-wrapped value lines; returns the vertical advance
fn draw_pair(p: Painter, x: f32, y: f32, label: &str, value: &str, lbl: [f32; 4], val: [f32; 4]) -> f32 {
    text_at(p, x, y, theme::size::CAPTION, lbl, 0, label);
    let h = TextView::new(value, theme::size::LABEL, val).bold().leading(30.0).max_lines(2).draw(p, Rect::new(x, y + 34.0, 520.0, 0.0));
    34.0 + h.max(30.0) + 22.0
}

/// the vertical advance [`draw_pair`] will consume for `value` — same formula, measured not painted,
/// so a column's focus highlight can be sized to wrap its content before the rows are drawn.
fn pair_h(value: &str) -> f32 {
    let h = TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING).bold().leading(30.0).max_lines(2).measure_h(520.0);
    34.0 + h.max(30.0) + 22.0
}

/// The About block's derived strings, rebuilt only when the loaded item changes — the block used
/// to re-format ~20 strings (info pairs, the audio list, the accessibility rows) every frame it
/// was on screen.
struct AboutRows {
    rk: String,
    info: Vec<(&'static str, String)>,
    orig_audio: Option<String>,
    audio_list: String,
    access: Vec<(&'static str, &'static str)>,
}
static mut ABOUT: Option<AboutRows> = None;
fn about_rows(d: &metadata::Detail) -> &'static AboutRows {
    let slot = unsafe { &mut *addr_of_mut!(ABOUT) };
    if slot.as_ref().map(|c| c.rk != d.rk).unwrap_or(true) {
        // Information: (label, value) pairs
        let mut info: Vec<(&'static str, String)> = Vec::new();
        let released = pretty_date(&d.aired, d.year);
        if !released.is_empty() {
            info.push(("Released", released));
        }
        let dur = if d.dur_ms > 0 { d.dur_ms } else { d.episodes.first().map(|e| e.dur_ms).unwrap_or(0) };
        if dur > 0 {
            info.push(("Run Time", crate::ui::fmt::dur_long(dur)));
        }
        info.push(("Rated", if d.rating.is_empty() { "NR".to_string() } else { d.rating.clone() }));
        if !d.countries.is_empty() {
            info.push(("Regions of Origin", d.countries.join(", ")));
        }
        // Languages: an "Original Audio" pair + a wrapped "Audio" list
        let orig_audio =
            d.audio.first().map(|a| if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() });
        let audio_list = d
            .audio
            .iter()
            .take(8)
            .map(|a| {
                let lang = if a.lang.is_empty() { "Unknown".to_string() } else { a.lang.clone() };
                format!("{} ({})", lang, a.codec.to_uppercase())
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Accessibility: the present capability descriptions
        let cc = !d.subs.is_empty();
        let sdh = d.subs.iter().any(|s| s.sdh);
        let ad = d.audio.iter().any(|a| a.ad);
        let acc_all: [(bool, &'static str, &'static str); 3] = [
            (cc, "CC", "Closed captions refer to subtitles in available languages with the addition of relevant non-dialogue information."),
            (sdh, "SDH", "Subtitles for the deaf and hard of hearing (SDH) refer to subtitles in the original language with the addition of relevant non-dialogue information."),
            (ad, "AD", "Audio descriptions (AD) refer to a narration track describing what is happening on screen, to provide context for those who are blind or have low vision."),
        ];
        let access = acc_all.iter().filter(|(pr, _, _)| *pr).map(|(_, l, ds)| (*l, *ds)).collect();
        *slot = Some(AboutRows { rk: d.rk.clone(), info, orig_audio, audio_list, access });
    }
    slot.as_ref().unwrap()
}

fn draw_about(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    let tx = MARGIN_X;
    let about_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    let hd = theme::TEXT_PRIMARY; // headings (brighter)
    let val = theme::TEXT_HEADING; // values (a step below headings)
    let lbl = theme::TEXT_TERTIARY; // dim labels
    let dim = theme::TEXT_TERTIARY;

    text_at(p, tx, about_y, theme::size::HEADLINE, hd, 1, "About");

    // card + column geometry — the card's height HUGS its measured content (title + genres +
    // wrapped synopsis + padding); a fixed 330px box used to leave up to ~180px of dead space
    // inside the focus highlight for a short blurb.
    let (cw, cy, pad) = (640.0f32, about_y + 50.0, 30.0f32);
    let syn_w0 = cw - 2.0 * pad;
    let syn_h = TextView::new(&d.summary, theme::size::CAPTION, theme::TEXT_HEADING)
        .leading(30.0)
        .max_lines(5)
        .measure_h(syn_w0);
    let ch = pad + 100.0 + syn_h + pad;
    let col_y = about_y + 430.0;
    let lx = 760.0f32; // Languages column x
    let ax = 1360.0f32; // Accessibility column x

    // The column rows drive BOTH the measured focus highlight (so it wraps the content — a
    // fixed-height panel used to let a long audio list spill past the selection) and the paint
    // below. Cached per item (about_rows); the measures below are wrap-memoised in TextView.
    let rows = about_rows(d);
    let (info, orig_audio, audio_list, access) = (&rows.info, &rows.orig_audio, &rows.audio_list, &rows.access);

    // ---- measured content extent (px below col_y) of each column, mirroring the paint advances ----
    let info_off = 68.0 + info.iter().map(|(_, v)| pair_h(v)).sum::<f32>();
    let lang_off = if orig_audio.is_none() {
        30.0
    } else {
        let list_h = TextView::new(audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).measure_h(500.0);
        68.0 + orig_audio.as_ref().map(|o| pair_h(o)).unwrap_or(0.0) + 34.0 + list_h
    };
    let access_off = if access.is_empty() {
        102.0
    } else {
        64.0 + access
            .iter()
            .map(|(_, ds)| 52.0 + TextView::new(ds, theme::size::CAPTION, val).leading(30.0).max_lines(4).measure_h(500.0) + 26.0)
            .sum::<f32>()
    };

    // ---- focus highlight: a translucent rounded panel under the SELECTED block that WRAPS its
    // content — the card is a fixed box; the three columns size to their measured height. ----
    if view().section == 5 {
        const HL_TOP: f32 = 36.0; // pad above the column heading
        const HL_BOT: f32 = 24.0; // pad below the last content line
        // the Information/Accessibility extents end in a per-row trailing gap (22 / 26px) that is
        // flow spacing, not content — trim it so the highlight hugs the last line's ink evenly.
        let hl = match view().col {
            0 => Rect::new(tx, cy, cw, ch),
            1 => Rect::new(tx - 26.0, col_y - HL_TOP, 600.0, info_off - 22.0 + HL_TOP + HL_BOT),
            2 => Rect::new(lx - 26.0, col_y - HL_TOP, 560.0, lang_off + HL_TOP + HL_BOT),
            _ => Rect::new(ax - 26.0, col_y - HL_TOP, 560.0, access_off - 26.0 + HL_TOP + HL_BOT),
        };
        p.rrect(hl, 18.0, 18.0, theme::OVERLAY_FOCUS_SOFT);
    }

    // ---- card: title, genres, summary — every run hard-bounded to the card's inner width (a
    // long title or genre list used to paint past the card frame) ----
    let ix = tx + pad;
    text_at(p, ix, cy + pad, theme::size::HEADLINE, hd, 1,
        &crate::text::elide(&d.title, syn_w0, theme::size::HEADLINE, 1, false));
    if !d.genres.is_empty() {
        text_at(p, ix, cy + pad + 44.0, theme::size::CAPTION, dim, 0,
            &crate::text::elide(&d.genres.join(", "), syn_w0, theme::size::CAPTION, 0, false));
    }
    let sy = cy + pad + 100.0;
    let syn_w = cw - 2.0 * pad;
    let mw = crate::text::text_width(c"MORE".as_ptr(), theme::size::CAPTION, 1);
    // CAPTION (not BODY): in the dense About card the body-size synopsis read oversized next to the
    // compact columns — match it to the Accessibility descriptions below (same CAPTION rung). When
    // the blurb is cut off, the last line dissolves (fade_last) into the MORE zone instead of
    // colliding with the label.
    let syn =
        TextView::new(&d.summary, theme::size::CAPTION, val).leading(30.0).max_lines(5).fade_last(mw + 36.0);
    let syn_hh = syn.draw(p, Rect::new(ix, sy, syn_w, 0.0));
    // "MORE" — quiet grey, pinned to the card's right padding edge ON the last synopsis line's cap
    // band (it used to sit bright in the bottom padding, visually detached from the text block).
    if syn.truncates(syn_w) {
        let (mt, _) = crate::text::text_cap_band(theme::size::CAPTION, 1);
        p.text(c"MORE".as_ptr(), tx + cw - pad, sy + syn_hh - 30.0 - mt, theme::size::CAPTION,
            theme::TEXT_TERTIARY, 2, 1);
    }

    // ---- Information ----
    text_at(p, tx, col_y, theme::size::HEADLINE, hd, 1, "Information");
    let mut yy = col_y + 68.0;
    for (label, value) in info {
        yy += draw_pair(p, tx, yy, label, value, lbl, val);
    }

    // ---- Languages ----
    text_at(p, lx, col_y, theme::size::HEADLINE, hd, 1, "Languages");
    let mut ly = col_y + 68.0;
    if let Some(orig) = orig_audio {
        ly += draw_pair(p, lx, ly, "Original Audio", orig, lbl, val);
    }
    if !audio_list.is_empty() {
        text_at(p, lx, ly, theme::size::CAPTION, lbl, 0, "Audio");
        TextView::new(audio_list, theme::size::LABEL, val).leading(32.0).max_lines(6).draw(p, Rect::new(lx, ly + 34.0, 500.0, 0.0));
    }

    // ---- Accessibility ----
    text_at(p, ax, col_y, theme::size::HEADLINE, hd, 1, "Accessibility");
    if access.is_empty() {
        text_at(p, ax, col_y + 68.0, theme::size::CAPTION, dim, 0, "\u{2014}");
    } else {
        let mut ay = col_y + 64.0;
        for (label, desc) in access {
            // the shared chip leaf (filled style) — cy = chip top + half its 34px height
            crate::ui::widgets::badge(p, ax, ay + 17.0, label, crate::ui::widgets::BadgeStyle::Filled);
            let h = TextView::new(desc, theme::size::CAPTION, val).leading(30.0).max_lines(4).draw(p, Rect::new(ax, ay + 52.0, 500.0, 0.0));
            ay += 52.0 + h + 26.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(directors: &[&str]) -> metadata::Detail {
        metadata::Detail {
            rk: "rk-1".to_string(),
            title: "An Item".to_string(),
            directors: directors.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }
    fn credit(name: &str, job: &str) -> metadata::Cast {
        metadata::Cast { tag: name.to_string(), role: job.to_string(), thumb: String::new() }
    }

    /// `hero_chain` is the ONE y-chain the hero's paint and the below-hero scroll flow share
    /// (`update()` reads `.3` for the button row). The "Directed by" line therefore has to reserve
    /// its band in the chain itself: drawing it without doing so would put a credit line under the
    /// Play pill, and reserving it unconditionally would leave a 40px hole on every item PMS sent
    /// no `Director[]` for — which is most TV shows.
    #[test]
    fn the_directed_by_band_is_reserved_when_there_is_a_line_and_never_otherwise() {
        // driven at both a short and a tall synopsis: the band must be the ONLY difference
        for syn_h in [0.0f32, 116.0] {
            let (_, _, date_y, btn_y, _) = hero_chain(syn_h, false);
            assert_eq!(btn_y, date_y + 46.0, "no directors: the button row keeps the y it always had");

            let (_, _, date2, btn2, dir_y) = hero_chain(syn_h, true);
            assert_eq!(date2, date_y, "nothing ABOVE the credit line moves");
            assert_eq!(dir_y, date_y + theme::space::LG, "the credit sits one air rung under the date line");
            assert_eq!(btn2, dir_y + 46.0, "and the buttons clear the line that is now there");
            assert!(btn2 > btn_y, "the reserved band is real space, not an overlap");
        }
    }

    /// The shelf is the whole credit list, so an item with crew but no actors still gets one — the
    /// section gate and the item count must agree on that or the page offers a focus stop it draws
    /// nothing into.
    #[test]
    fn a_crew_only_item_still_gets_the_cast_and_crew_shelf() {
        let _serial = crate::testlock::serial();
        let mut d = item(&["Jane Doe"]);
        d.crew = vec![credit("Jane Doe", "Director, Writer")];
        metadata::install_for_test(Some(d));

        let (secs, n) = sections();
        assert!(secs[..n].contains(&4), "section 4 is present for a crew-only item");
        assert_eq!(n_items(4), 1, "and it holds the one credit");

        metadata::install_for_test(None);
        assert!(!sections().0[..sections().1].contains(&4), "with nothing loaded there is no shelf");
    }

    use crate::metadata::{Detail, Episode};

    /// Install `d` as the loaded item and park hero focus on `col`. Both statics are crate-wide
    /// (`metadata::CURRENT` and detail's own `VIEW`), so every caller holds `testlock::serial()`.
    /// `hero_had_restart` is seeded from the item so a mount is a settled starting state — a test
    /// that means to exercise a set CHANGE makes it happen explicitly, after the mount.
    fn mount(d: Option<Detail>, col: c_int) {
        metadata::set_current_for_test(d);
        let restart = has_restart();
        let v = view();
        v.section = 0;
        v.col = col;
        v.last_resume_ns = 0;
        v.hero_had_restart = restart;
    }
    fn movie(resume_ms: i64, dur_ms: i64) -> Detail {
        Detail { rk: "m1".into(), dur_ms, resume_ms, ..Default::default() }
    }
    fn show(ep_resume_ms: i64, dur_ms: i64) -> Detail {
        Detail {
            rk: "s1".into(),
            is_show: true,
            episodes: vec![Episode { rk: "e1".into(), dur_ms, resume_ms: ep_resume_ms, ..Default::default() }],
            ..Default::default()
        }
    }

    /// The restart disc exists exactly when the play beside it would resume — the pill's label, the
    /// control count and the watched index all hang off that one predicate, and it is Plex's resume
    /// RULE (`metadata::resume_ns`), not a raw `viewOffset > 0`.
    #[test]
    fn restart_disc_tracks_the_resume_the_play_would_actually_apply() {
        let _serial = crate::testlock::serial();

        mount(None, 0);
        assert_eq!(hero_btns(), 2, "no item loaded: nothing to restart");
        assert_eq!(btn_watched(), 1);

        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(hero_btns(), 2, "unwatched movie: Play already starts at 0");

        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(hero_btns(), 3, "part-watched movie: Play resumes, so restart is reachable");
        assert_eq!(btn_watched(), 2, "the watched toggle stays LAST — it shifts, it is not displaced");

        // the two rule edges: a few seconds in is not a resume point, and neither is the 95% tail
        // (both resume_ns to 0, where "Resume" would be a lie and ↺ a second Play)
        mount(Some(movie(4_000, 7_200_000)), 0);
        assert_eq!(hero_btns(), 2, "a 4s offset is below Plex's resume floor");
        mount(Some(movie(7_100_000, 7_200_000)), 0);
        assert_eq!(hero_btns(), 2, "past the 95% mark the item plays from the start");

        // a show page's Play starts episodes[0] of the selected season, so the row reads THAT leaf
        mount(Some(show(0, 2_700_000)), 0);
        assert_eq!(hero_btns(), 2, "show whose first episode has not been started");
        mount(Some(show(600_000, 2_700_000)), 0);
        assert_eq!(hero_btns(), 3, "show whose first episode is part-watched");
        view().section = 1;
        assert_eq!(focus(), -1, "focus() still reports -1 off the hero section");

        mount(None, 0);
    }

    /// The index→action mapping, which is the whole reason the row can grow a control: with a
    /// resume point index 1 is the restart disc and the toggle has moved to 2; without one, index 1
    /// is the toggle and there is no restart to reach.
    #[test]
    fn hero_indices_mean_different_actions_in_the_two_control_sets() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(hero_action(0), HeroAction::Play);
        assert_eq!(hero_action(1), HeroAction::Watched, "no restart disc: index 1 is the toggle");
        assert_eq!(hero_action(2), HeroAction::None, "and there is no index 2");

        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(hero_action(0), HeroAction::Play);
        assert_eq!(hero_action(1), HeroAction::Restart);
        assert_eq!(hero_action(2), HeroAction::Watched, "the toggle moved, and OK must follow it");

        mount(None, 0);
    }

    /// The watched toggle can delete the control the focus is sitting on: scrobbling clears the
    /// server's viewOffset, so the row goes 3 → 2 under a focus parked at index 2. The clamp must
    /// bring it back to the toggle rather than leave it past the end — where nothing draws focused
    /// and, since `hero_action` would answer `None`, every later OK is inert.
    #[test]
    fn hero_focus_is_clamped_when_the_control_set_shrinks_under_it() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(1_800_000, 7_200_000)), 2);
        assert_eq!(focus(), 2, "3-button row: focus sits on the watched toggle");

        // the scrobble landed: watched, and no resume point any more, so the restart disc is gone
        metadata::set_current_for_test(Some(Detail { watched: true, ..movie(0, 7_200_000) }));
        assert_eq!(hero_btns(), 2);
        assert_eq!(focus(), 1, "focus lands back on the watched toggle, not past the end");
        assert_eq!(view().col, 1, "and the clamp is written back — it IS the state");
        assert_eq!(hero_action(view().col), HeroAction::Watched);

        mount(None, 0);
    }

    /// `commit_resume` is the whole of the restart action — it runs over whatever the play arm baked
    /// in, so one statement covers the movie path and the episode path. `on_ok` itself is a PMS
    /// round trip; the field it leaves behind is all app.rs reads, and that is host-checkable.
    #[test]
    fn restart_drops_the_resume_both_play_paths_bake_in() {
        let _serial = crate::testlock::serial();

        // the movie arm: set_resume applies the rule, then the press decides whether it survives
        mount(Some(movie(1_800_000, 7_200_000)), 0);
        set_resume(1_800_000, 7_200_000);
        commit_resume(false);
        assert_eq!(last_resume_ns(), 1_800_000_000_000, "Play keeps the resume: 30 minutes in");
        commit_resume(true);
        assert_eq!(last_resume_ns(), 0, "Restart drops it: 00:00");

        // the episode arm bakes its resume in at play_episode_at's tail — same override, same result
        mount(Some(show(600_000, 2_700_000)), BTN_RESTART);
        set_resume(600_000, 2_700_000);
        commit_resume(false);
        assert_eq!(last_resume_ns(), 600_000_000_000, "the episode Play resumes 10 minutes in");
        assert_eq!(hero_action(hero_col()), HeroAction::Restart, "and the disc is what index 1 means here");
        commit_resume(true);
        assert_eq!(last_resume_ns(), 0);

        mount(None, 0);
    }

    /// The set can also GROW under a parked focus: `open_rk` clears `current()` for the whole fetch,
    /// so a RIGHT pressed in that window lands on the watched toggle of a TWO-button row — and if
    /// the item then arrives part-watched, index 1 has become the restart disc. Focus must travel
    /// with its control, or that press plays the item instead of marking it watched.
    #[test]
    fn hero_focus_follows_its_control_when_the_set_grows_under_it() {
        let _serial = crate::testlock::serial();

        // mid-fetch: nothing loaded, so the row is Play + watched and RIGHT parks on the toggle
        mount(None, 0);
        assert_eq!(focus(), 0);
        view().col = 1;
        assert_eq!(hero_action(focus()), HeroAction::Watched, "index 1 is the toggle while the row is short");

        // the fetch lands, part-watched: the row grows a restart disc AT index 1
        metadata::set_current_for_test(Some(movie(1_800_000, 7_200_000)));
        assert_eq!(focus(), 2, "the focus moved with the toggle rather than staying on the index");
        assert_eq!(hero_action(focus()), HeroAction::Watched, "so OK still means what the user aimed at");

        // Play never shifts — it is before the insertion point
        view().col = 0;
        metadata::set_current_for_test(Some(movie(0, 7_200_000)));
        assert_eq!(focus(), 0, "the pill holds index 0 through a set change");

        mount(None, 0);
    }
}

/// "YYYY-MM-DD" -> "D Mon YYYY"; falls back to the year, then empty
fn pretty_date(iso: &str, year: i64) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(mo), Ok(da)) =
            (parts[0].parse::<i64>(), parts[1].parse::<usize>(), parts[2].parse::<i64>())
        {
            const MON: [&str; 12] =
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            if (1..=12).contains(&mo) {
                return format!("{da} {} {y}", MON[mo - 1]);
            }
        }
    }
    if year > 0 {
        year.to_string()
    } else {
        String::new()
    }
}
