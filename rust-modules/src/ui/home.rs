//! The home screen as a retui tree: Backdrop + Hero + Grid([CardRow; MAX_HUBS]).
//! fr/fc/snapTarget are the focus source of truth (private module state); the tree
//! reads them live each frame via Env and writes back through nav. plex_run drives it
//! through home_init/update/draw/move_focus/pointer_focus/wheel (crate path) and the
//! row/col/snap_target accessors.
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::consts::*;
use crate::ui::icons::Icon;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::hero_logo::{self, HeroLogo, LogoRung};
use crate::ui::label::{Label, VAlign};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{
    AmbientWash, Art, Button, CircleButton, PageDots, HERO_BASE_SCRIM_Y0,
};
// `guard` was a private copy of this barrier living here; it is now the shared `ui::guard` (its
// doc comment carries the FFI-unwind rationale + the GL-scissor repair the local copy was missing).
use crate::ui::{guard, hero_alpha, on_axis, Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- focus state: private main-thread module state. Home internals read/write these
// directly; app.rs (the only outside reader) reaches them through the accessors below. ----
static mut fr: c_int = 0;
static mut fc: c_int = 0;
static mut snapTarget: f32 = 0.0;
// rotating hero: current index into pms::hero_pool + a flip debounce (so a held LEFT/RIGHT, via the
// key-repeat path, can't machine-gun the carousel), the slide transition (outgoing index + a 0→1
// progress spring + direction), an idle auto-flip countdown, the action-row focus, and the
// on-screen action-button rects for pointer hover/clicks.
static mut hero_idx: c_int = 0;
static mut hero_flip_cd: f32 = 0.0;
static mut hero_prev: c_int = -1; // outgoing pool index while sliding (-1 = idle)
static mut hero_slide: Spring = Spring::at(1.0); // slide progress 0→1
static mut hero_dir: f32 = 1.0; // +1 = next (content slides left), -1 = prev
static mut hero_auto: f32 = HERO_AUTO_S; // countdown to the next automatic flip
static mut hero_fc: c_int = 0; // hero focus: -1 = profile chip, 0 = pill, 1 = info
static mut hero_btns: [Rect; HERO_NBTN] = [Rect::new(0.0, 0.0, 0.0, 0.0); HERO_NBTN]; // pill / info
const HERO_FLIP_CD: f32 = 0.35;
const HERO_AUTO_S: f32 = 8.0; // idle seconds between automatic hero flips
/// How many CONTROLS the hero action row holds — the Play/Continue pill and the info disc, and
/// that is the whole of it. The pager chevron beside them is an INDICATOR, not a third button
/// (see [`hero_actions`]): it is drawn, it is never focused and it has no hit rect, so it is not
/// counted here. This number is the row's focus clamp ([`set_hero_focus`]), the edge test that
/// decides when RIGHT pages instead of moving ([`home_hero_key`]) and the length of the pointer
/// hit-target array — one fact, so removing the chevron from the row was one edit.
const HERO_NBTN: usize = 2;
/// The sliding text column's bottom-anchor line. The stack is a FIXED height whatever artwork
/// lands: the reserved title band (`hero_logo::band_h` = 120) + kicker + 3-line synopsis tops out at
/// y≈387 for every item ([`hero_stack_top`]). A logo taller than the band (up to
/// `theme::logo::HERO_H_MAX` = 268) spills upward as PAINT, reaching y≈239 at worst — clear of the
/// top-bar track's bottom edge (`widgets::TOP_BAR_BOTTOM` = 112). The action row and page dots hang
/// at fixed offsets below this line, so the chrome never jumps between hero items.
///
/// (Both numbers were 21px lower — 408 and 260 — until the synopsis joined detail's on the shared
/// `ui::hero_synopsis` block, `LABEL`/36 instead of `MICRO`/29. A bottom-anchored stack pays for a
/// taller block by growing UPWARD, which is the whole cost of that change and is why the two host
/// contracts grading this column read the block's height rather than quoting it.)
pub(crate) const HERO_TEXT_BOTTOM: f32 = 692.0;
/// The hero text column's width — the wrap budget for the title/kicker/synopsis and the column a
/// clearLogo is contained to, so its RIGHT END (`MARGIN_X + HERO_COL_W` = 750) is the worst point
/// the hero scrim's legibility table has to hold.
pub(crate) const HERO_COL_W: f32 = 660.0;
/// The action row's top edge and control diameter. Module-level because the row SLIDES with its
/// item while the page dots below it do NOT — two draw sites, one geometry, so they cannot drift.
const HERO_ROW_Y: f32 = HERO_TEXT_BOTTOM + theme::space::MD;
/// The action row's control size IS the shared one — the same height the status read-out's own
/// pill takes ([`StatusOverlay::CTRL_H`](crate::ui::widgets::StatusOverlay::CTRL_H)), which is not a
/// coincidence to be restated as a second 60: this screen draws both, and the status screen's Retry
/// stands in for this very row (see [`draw_status`]).
const HERO_CTRL_D: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;
/// The air between one element of the action row and the next, edge to edge. Hoisted out of
/// [`hero_actions`] because the pager mark's padding is derived from it — the row has ONE rhythm,
/// and a mark that sat on a second number would read as belonging to something else.
///
/// The rhythm is the SHARED one, for the same reason [`HERO_CTRL_D`] is: the detail page draws this
/// row too, so an alias here and a second literal there would be two rhythms in one app. Hoisting
/// the literal named the fork; pointing it at [`widgets::CTRL_GAP`](crate::ui::widgets::CTRL_GAP)
/// removes it.
const HERO_CTRL_GAP: f32 = crate::ui::widgets::CTRL_GAP;
/// The pager mark's own size — the chevron drawn at exactly the box every DISC control in this row
/// carries its glyph in (`HERO_CTRL_D` × [`widgets::DISC_ICON_RATIO`](crate::ui::widgets::DISC_ICON_RATIO)
/// = 32.4, rounded to 32 at the draw by the same `.round()` `CircleButton` applies to its own glyph
/// box, so the two land on the same whole number), which is what makes it read as a peer of the
/// controls without being one. It is the size the chevron has always been drawn at; what the
/// correction removed is the 60px control BOX that used to sit around it, not the mark.
const HERO_PAGER_D: f32 = HERO_CTRL_D * crate::ui::widgets::DISC_ICON_RATIO;
/// The air beside the mark, applied to its BOX on the left (where it becomes the visible stand-off
/// from the info disc's plate) and again on the right as the row's trailing extent, which only
/// [`hero_actions`]'s `on_axis` cull can see because nothing is drawn past it. Both halves resolve
/// to the row's own [`HERO_CTRL_GAP`] *measured to the ink* — on the right the arithmetic works out
/// on its own, since box-right + `PAD` is ink-right + bearing + (`GAP` − bearing).
///
/// **Measured to the INK, not to the box**, and the distinction is the whole of this constant.
/// A glyph is centred and INSET in its box: `chevron.svg` strokes the middle ⅜ of its viewBox
/// (`M9 6l6 6-6 6` at stroke-width 3 spans x 7.5..16.5 of 24), so at 32 the ink is 12px wide with
/// [`HERO_PAGER_BEARING`] of empty box each side. Padding the BOX by the row's gap therefore puts
/// the *visible* stand-off at 30 where the pill-to-disc one is 20, and on the panel the mark reads
/// adrift of the row rather than part of it — measured at 3× on the dev set, 2026-08-21.
///
/// The pill and the disc are both FILLED, so their visual edge is their own edge and the row's
/// rhythm is a gap between inks. A bare mark has no plate, so the only way it can join that rhythm
/// is to have its INK sit one gap from the disc — which means starting its box a bearing earlier.
/// That is what the owner's "equal padding" asks for: equal to the eye, not equal in the arithmetic.
/// (This was shipped box-measured first and corrected after looking at the television.)
const HERO_PAGER_PAD: f32 = HERO_CTRL_GAP - HERO_PAGER_BEARING;
/// The empty box each side of the chevron's ink — `chevron.svg`'s stroke starts 7.5/24 into its
/// viewBox, so the bearing is that fraction of the mark's box. Named because [`HERO_PAGER_PAD`]
/// subtracts it and the `on_axis` cull adds it back.
const HERO_PAGER_BEARING: f32 = HERO_PAGER_D * (7.5 / 24.0);
const K_SLIDE: f32 = 130.0; // slide spring — a touch softer than the grid springs, reads cinematic
/// The slide is over once its remaining travel is **sub-pixel**. Threshold in PIXELS, not in
/// spring units: the old `pos > 0.995` cut retired the transition while the incoming layer still
/// sat `0.005 * SCR_W ≈ 10px` short of home, so every flip ended on a visible teleport. The
/// off-screen layers are culled per-frame, so carrying the spring to a true rest costs nothing.
const HERO_SLIDE_REST_PX: f32 = 0.5;
/// How many pool pages either side of the one on screen get their artwork warmed. ONE, and the
/// number is a budget decision:
///  * the carousel can only move one step at a time — LEFT/RIGHT at the row's ends and the
///    8-second auto-flip all call [`hero_flip`]`(±1)` — so ±1 is COMPLETE coverage of "what can be on screen
///    next"; depth 2 warms a page reachable only after another 8 s of sitting still, by which time
///    depth 1 has already warmed it;
///  * each page costs TWO of the store's 64 slots (the 1280×720 backdrop and the 600×240 clearLogo,
///    which is claimed even for an item that has none — the 404 lands `P_FAILED` and holds the
///    slot). Depth 1 = 4 slots on top of Home's ~29-slot peak; the whole `HERO_MAX` = 8 pool would
///    be 16, for pages nobody is about to look at.
///
/// **Raising this is the one knob here that can hurt**, and not through eviction: `P_WANT`/
/// `P_LOADING`/`P_DECODED` slots are never victims ([`crate::posters::store_idle`]'s doc and
/// `posters::victim`), so enough in-flight warms make a `poster_get` for a poster the user is
/// LOOKING AT fall through to "all visible: skip" and return 0 — a tile that is never even
/// requested, which is worse than one that arrives late. The same paragraph is why
/// [`prefetch_hero_neighbours`] issues at most one key per frame from an idle store; drop either
/// guard and the depth stops being safe.
const HERO_PREFETCH: usize = 1;
/// Ambient corner weights (tl, tr, br, bl — [`Painter::ambient`]'s order): how far each corner of
/// the billboard's ground leans from the app surface toward the item's UltraBlur colour. The top
/// pair carries the old uniform `dim = 0.55`; the bottom pair is a step weaker because the hero text
/// scrim already sits at 0.5–0.94 alpha down there and colour spent under it is colour thrown away.
///
/// Deliberately stronger than [`AmbientWash::GROUND_W`] (0.26) and that is not a drift: detail's and
/// person's washes are a GROUND UNDER BODY TEXT, this one is a **stand-in for a missing photograph**
/// filling the billboard. Both go through [`AmbientWash::keyed`], so both are held under the same
/// luminance cap; only the strength differs, which is the part that is per-screen.
const HERO_WASH_W: [f32; 4] = [0.55, 0.55, 0.40, 0.40];
/// The snap past which the hero's backdrop ART is neither drawn nor RESOLVED. Past it the layer is
/// faded to under 0.004 by `backdrop_art`'s own `1 - sp` tint and slid nearly a panel-height off the
/// top, so there is nothing to see — and resolving it anyway cost a key build plus a store probe
/// (mutex + up to 64 key compares) on every frame of the whole grid scene, and stamped the slot
/// `Touch::Draw`, which kept the hero's backdrop permanently evict-protected for as long as Home was
/// up. Named because [`Backdrop`]'s update and draw must read the SAME number: one skips the probe,
/// the other the blit, and a drift between them is a layer the springs believe in and nothing paints.
const HERO_ART_CULL: f32 = 0.996;
// (the top-left profile chip's rect, hit test and unfurl spring all live in `widgets` now — it is
// the SHARED bar's control, not this screen's, and Home owning them is what left it activatable on
// Home alone. What Home still owns is where its own focus is: [`top_focus`].)

/// Focus accessors clamp INTO THE LIVE HUB BOUNDS at read time (mirroring Home::env's per-frame
/// clamp): a hub refetch can shrink the shelves underneath the raw statics, and the OK dispatch
/// reads these — an unclamped stale index made OK silently no-op on a visibly focused card.
pub(crate) fn row() -> c_int {
    let nh = n_hubs() as c_int;
    unsafe { addr_of!(fr).read() }.clamp(0, (nh - 1).max(0))
}
pub(crate) fn col() -> c_int {
    let nc = crate::pms::hub_len(row().max(0) as usize) as c_int;
    unsafe { addr_of!(fc).read() }.clamp(0, (nc - 1).max(0))
}
pub(crate) fn snap_target() -> f32 {
    unsafe { addr_of!(snapTarget).read() }
}
/// The snap spring's live POSITION (0 = hero fully shown, 1 = grid) — unlike [`snap_target`] this
/// lags through the transition, so it reflects what is actually on screen. The home OK handler gates
/// on it so a quick DOWN→OK launches the hero item still visible, not the grid's first card. Falls
/// back to the target before the scene is built.
pub(crate) fn snap_pos() -> f32 {
    unsafe { (*addr_of!(SCENE)).as_ref().map(|h| h.snap.pos).unwrap_or_else(|| addr_of!(snapTarget).read()) }
}
pub(crate) fn set_row(v: c_int) {
    unsafe { addr_of_mut!(fr).write(v) }
}
/// The snap the screen may actually hold, given `nrows` addressable shelves: **no shelves means no
/// grid**, so it collapses to the hero. ONE pure rule, applied in two places, because either alone
/// is a bug: `set_snap_target` refuses a new dive (a DOWN press would otherwise slide the
/// billboard — or the loading/empty/error read-out's hero band — away and land on a bare screen),
/// and `home_update` re-applies it every frame because a catalog can empty UNDER a snap already
/// committed. Left at 1.0 with nothing to show, app.rs's pointer gate (`snap_pos() < 0.5`) routes
/// clicks to the empty grid's hit-test instead of the hero rects, and the status screen's Retry
/// control — the one way back — is silently unclickable.
fn pinned_snap(v: f32, nrows: usize) -> f32 {
    if nrows == 0 {
        0.0
    } else {
        v
    }
}

pub(crate) fn set_snap_target(v: f32) {
    unsafe { addr_of_mut!(snapTarget).write(pinned_snap(v, n_hubs())) }
}

#[inline]
fn g_fr() -> c_int {
    unsafe { addr_of!(fr).read() }
}
#[inline]
fn g_fc() -> c_int {
    unsafe { addr_of!(fc).read() }
}

// Home grid is now N hub shelves of varying length (not the old fixed ROWS×COLS).
// The Grid's CardRow array + each row's cell springs are sized to these maxima; the
// *actual* counts come from pms::hub_count()/hub_len().
// Continue Watching, On Deck, Recently Added, collections… — and, with a second server, whatever
// the shares contribute. The number lives in `pms`, which BUDGETS against it: the shelf allowance
// is divided between the sources there, so the two must be one constant or a share can be truncated
// away by a cap the data layer never heard of.
const MAX_HUBS: usize = crate::pms::MAX_SHELVES;
// Cards per shelf. In `pms` for the same reason MAX_HUBS is: it is where the merged Continue
// Watching shelf is TRUNCATED to it, and this file clamps the drawn focus to it while `col()` — the
// index OK acts on — clamps only to the shelf's real length. One constant, so the ring and the
// press can never disagree about which card is focused.
const MAX_ITEMS: usize = crate::pms::MAX_SHELF_ITEMS;

/// Rows the grid can actually address: the server's hub count clamped to the fixed `shelves`
/// array. **Every `shelves[]` index site must go through this** — a server with 3-4 libraries
/// returns well over `MAX_HUBS` hubs (/hubs promotes several rows per library on top of
/// continue/ondeck/recentlyAdded), and `pms::hub_count()` is uncapped.
fn n_hubs_of(server_hubs: usize) -> usize {
    server_hubs.min(MAX_HUBS)
}
fn n_hubs() -> usize {
    n_hubs_of(crate::pms::hub_count())
}

/// Step the focus row by `dir`, clamped into the addressable rows. Used by every vertical move:
/// the raw `fr + dir` it replaces could walk past the end of `shelves` and panic on the keypress
/// (`Home::env` clamps the value it *returns*, but never writes the clamp back to the global).
fn step_row(cur: c_int, dir: c_int, nrows: usize) -> c_int {
    if nrows == 0 {
        return 0;
    }
    (cur + dir).clamp(0, nrows as c_int - 1)
}

/// the item at (hub row, column) in the home hub grid, or None
pub(crate) fn movie_at(r: c_int, c: c_int) -> Option<&'static PmsMovie> {
    if r < 0 || c < 0 {
        return None;
    }
    crate::pms::hub_item(r as usize, c as usize)
}

/// The currently-shown rotating-hero item (curated pool: Continue Watching then Recently Added),
/// falling back to the first catalog item when the pool is empty. Backdrop, Hero and the home OK
/// handler all read the hero through this so they never disagree on which item is featured.
pub(crate) fn hero_item() -> Option<&'static PmsMovie> {
    let n = crate::pms::hero_pool_len();
    if n == 0 {
        return movie_at(0, 0);
    }
    let i = unsafe { addr_of!(hero_idx).read() }.clamp(0, n as c_int - 1);
    crate::pms::hero_pool_item(i as usize)
}

/// dev/test hook: jump the hero to a specific pool index (the `plxnative-heroidx` trigger) so a flipped
/// state can be captured headlessly. Clamped on read via [`hero_index`]/[`hero_item`].
pub(crate) fn set_hero_idx(i: c_int) {
    unsafe { addr_of_mut!(hero_idx).write(i.max(0)) };
}

/// current hero index, clamped into the live pool (0 when empty) — drives the page indicator.
fn hero_index() -> usize {
    let n = crate::pms::hero_pool_len();
    if n == 0 {
        return 0;
    }
    unsafe { addr_of!(hero_idx).read() }.clamp(0, n as c_int - 1) as usize
}

/// Advance the hero to the next (`dir=+1`) / previous (`dir=-1`) pooled item, wrapping, and start
/// the slide transition (outgoing item + direction + progress spring reset). Debounced by
/// `hero_flip_cd` so a held edge-key (via the repeat path) flips a few times a second, not every
/// frame — the same machine-gun guard the season tabs needed. Any flip (manual or automatic)
/// restarts the idle auto-flip countdown.
///
/// Private again: every way the billboard pages is now this screen's own — the row's two edge keys
/// ([`home_hero_key`]) and the idle countdown in [`home_update`]. It was `pub(crate)` for
/// `app.rs`'s two chevron activations, the OK and the click-hold, and both are gone with the
/// button.
fn hero_flip(dir: c_int) {
    let n = crate::pms::hero_pool_len() as c_int;
    if n <= 1 {
        return;
    }
    unsafe {
        if addr_of!(hero_flip_cd).read() > 0.0 {
            return;
        }
        let cur = addr_of!(hero_idx).read().clamp(0, n - 1);
        addr_of_mut!(hero_prev).write(cur);
        addr_of_mut!(hero_dir).write(dir as f32);
        addr_of_mut!(hero_slide).write(Spring::at(0.0));
        addr_of_mut!(hero_idx).write((cur + dir).rem_euclid(n));
        addr_of_mut!(hero_flip_cd).write(HERO_FLIP_CD);
        addr_of_mut!(hero_auto).write(HERO_AUTO_S);
    }
}

/// The slide transition, or None when idle: (outgoing pool index, outgoing x offset, incoming x
/// offset). Backdrop and Hero both read it so art and text move as one phase.
fn hero_slide_state() -> Option<(c_int, f32, f32)> {
    unsafe {
        let prev = addr_of!(hero_prev).read();
        if prev < 0 {
            return None;
        }
        let t = addr_of!(hero_slide).read().pos;
        let dir = addr_of!(hero_dir).read();
        Some((prev, -dir * t * SCR_W, dir * (1.0 - t) * SCR_W))
    }
}

/// The pooled hero item at index `i`, or None (negative, or out of a shrunken pool —
/// hero_pool_item bounds-checks the upper end).
fn hero_item_at(i: c_int) -> Option<&'static PmsMovie> {
    if i < 0 {
        return None;
    }
    crate::pms::hero_pool_item(i as usize)
}

/// Whose server hero page `i` came from, as the owner's handle — **empty for our own**, which is
/// every page on a single-server install. [`hero_item_at`]'s twin, and it has to be one: the
/// billboard is nobody's focused tile, so the shelf heading below it is describing something else
/// entirely and the run on the meta line is the only thing on the screen that says whose the film is.
fn hero_source_at(i: c_int) -> &'static str {
    or_dev_source(if i < 0 { "" } else { crate::pms::hero_pool_source(i as usize) })
}

/// Whose server the CURRENT hero came from — [`hero_item`]'s twin, and it follows that function's
/// empty-pool fallback rather than reporting "ours" for it: with nothing pooled the hero is the
/// first card of the first shelf, so its source is that shelf's.
fn hero_source() -> &'static str {
    if crate::pms::hero_pool_len() == 0 {
        return or_dev_source(crate::pms::hub_source(0));
    }
    hero_source_at(hero_index() as c_int)
}

/// [`dev_source`]'s stand-in when it is armed, else the real answer. **Both hero source accessors
/// go through it**, and that is the point: a flip draws the outgoing page through `hero_source_at`
/// and the incoming one through `hero_source`, so a stand-in applied to only one of them would pop
/// the run into existence halfway through the slide — at exactly the moment the trigger exists to
/// let someone judge it.
fn or_dev_source(real: &'static str) -> &'static str {
    dev_source().unwrap_or(real)
}

/// `/tmp/plxnative-shared=<handle>` — the SAME trigger the detail page's own source run reads
/// (`metadata::fetch_detail`) and the Search results' owner annotation
/// (`ui::search::results::owner_handle`), deliberately not a third one: one file arming every
/// surface is the only way to see them agree.
///
/// **The trigger WINS WHEN ARMED**, which is the precedence every `/tmp/plxnative-*` read in this
/// app has (`crate::dev`'s module doc: `plxnative-token` beats the signed-in session) and the
/// phrase to grep for across the three sites. This one used to stand in only where the real answer
/// was empty, which is the opposite rule to `metadata.rs`'s — so on a television with a real share
/// installed, arming the trigger changed the detail page and left the hero saying something else,
/// and the one thing this stand-in exists for is watching the surfaces agree. `Some("")` (an armed
/// but EMPTY file) forces the absence of a handle rather than doing nothing, for the same reason.
///
/// Read once: the trigger surface is boot state, and a per-frame `stat` inside the hero's draw is
/// not. Compiled out with the `devtriggers` feature, like every other trigger (`crate::dev`), and
/// it is needed because the two things the design states about this run — that it sits inside the
/// corner wedge, and that it ends well short of the last fifth of the frame — are things only a
/// panel can settle.
fn dev_source() -> Option<&'static str> {
    static SEEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| crate::dev::read("shared")).as_deref()
}

/// Hero action-row focus: -1 = the profile chip, 0 = Play/Continue pill, 1 = info. (The pager
/// chevron beside them is an indicator and takes no focus — see [`hero_actions`].)
/// Below -1 sit the CENTERED tab pills (the top band, left→right: chip, then pill i as `-(i+2)`):
/// -2 = **Home**, -3 = the first section (Movies), -4 = the second (TV Shows), …
///
/// Home is pill 0 and a real focus stop like any other — it used to be packed as `-(i+1)`, which
/// aliased it onto the chip's -1, so the top band walked chip → Movies and the Home pill could
/// never take the focused (white) treatment on its own screen.
pub(crate) fn hero_focus() -> c_int {
    unsafe { addr_of!(hero_fc).read() }
}
pub(crate) fn set_hero_focus(v: c_int) {
    unsafe { addr_of_mut!(hero_fc).write(v.clamp(hero_focus_lo(), HERO_NBTN as c_int - 1)) }
}

/// The lowest (most negative) hero-focus value: the LAST tab pill. Every discovered library
/// section has one — the row scrolls to reach the pills past the screen width, so the clamp is
/// the section count, no longer a hard cap that hid a fifth library from the whole UI.
/// ([`crate::ui::widgets::tab_count`] is the one source of that count; it is never 0, so the
/// subtraction can't wrap.)
fn hero_focus_lo() -> c_int {
    hero_focus_lo_for(crate::ui::widgets::tab_count())
}
/// The pure half of [`hero_focus_lo`] — split out so the "focus reaches the LAST section however
/// many there are" invariant is host-testable without a live section table.
fn hero_focus_lo_for(npill: usize) -> c_int {
    hero_focus_for_pill(npill.max(1) - 1)
}

/// Decode a top-band focus value to its tab-pill index (0 = Home, 1.. = sections), or None
/// when the focus isn't on a pill — the ONE home of the `-(i+2)` packing's sign math (inverse:
/// [`hero_focus_for_pill`]); it was previously inlined at four sites.
///
/// `c_int::MIN` is app.rs's "focus is a grid card, not the hero band" sentinel and is NOT a
/// pill: it satisfies the `<= -2` sign test but negates to itself (overflow), so an ungated
/// decode used to send EVERY grid-card OK into the last library section. Rejecting it here —
/// in the one place that owns the packing — keeps every caller safe by construction.
pub(crate) fn hero_pill_index(f: c_int) -> Option<usize> {
    (f <= -2 && f != c_int::MIN).then(|| (-f - 2) as usize)
}
/// Encode: the hero-focus value that puts the top band on tab pill `i` (0 = Home).
pub(crate) fn hero_focus_for_pill(i: usize) -> c_int {
    -(i as c_int) - 2
}

/// The focused thing is a pressable grid card — the same cross-screen predicate `detail` and
/// `library` expose, so app.rs asks one uniform per-route question (Home's grid always holds
/// cards, so "the grid is showing" is the whole answer).
pub(crate) fn focus_is_card() -> bool {
    snap_pos() > 0.5
}

/// Hero pointer hit-test against the action-row rects recorded at draw: returns the button index
/// (0 pill / 1 info) or -2 for a miss. `hover` moves the hero focus without acting. The pager
/// mark has no entry here: it is not selectable, so it is not clickable either.
pub(crate) fn hero_button_at(mx: f32, my: f32) -> c_int {
    let btns = unsafe { addr_of!(hero_btns).read() };
    btns.iter().position(|r| r.contains(mx, my)).map(|i| i as c_int).unwrap_or(-2)
}
pub(crate) fn hero_pointer_focus(mx: f32, my: f32) {
    let b = hero_button_at(mx, my);
    if b >= 0 {
        set_hero_focus(b);
    }
}

// (the resume-bar fraction rule lives on PmsMovie::resume_frac — shared with the Library grid)

// ---- Backdrop: the page GROUND (one dissolving ambient wash) + the hero ART layers (which slide
// with their item) + the text scrim. Still explicit per-element alphas, NOT the cascade — each
// fades on its own curve, and a wash cannot be alpha-faded at all (see `AmbientWash`).
struct Backdrop {
    /// The page's ground: ONE wash for the whole panel, keyed to the CURRENT hero and dissolving
    /// between pages on its own springs. It deliberately does NOT slide with a flip — two washes
    /// sliding past each other put a hard vertical seam between two atmospheres across the panel,
    /// and atmosphere is the one thing that should cross-fade rather than travel.
    ///
    /// It is HERO-SCOPED: [`wash_corners`]'s lean falls to zero as the snap dives, so in the grid it
    /// resolves to exactly [`theme::SURFACE_APP`] and `is_flat` skips the pass entirely. The shelves
    /// keep sitting on the flat clear the card shadows are tuned against — this is a billboard
    /// ground, not a page tint.
    wash: AmbientWash,
    /// Art reveal 0→1 for the CURRENT layer and, during a flip, the OUTGOING one. THIS is the
    /// cross-fade that covers a prefetch miss: a layer whose backdrop has not decoded yet sits at 0
    /// and shows the wash, then dissolves the photograph in when it lands, replacing the hard
    /// `if tex != 0` cut. A prefetch HIT skips the fade entirely — the re-seat in `update` starts the
    /// incoming layer at 1.0 when its texture is already there, so the common case is a page that
    /// slides in complete, not one that slides in and then fades up.
    ///
    /// Within one keying the reveal only ever runs UP. A texture evicted while Home is off screen
    /// (the player and the library both push the store hard) must not make the billboard fade its
    /// own photograph out and back in on the frame you return to it.
    art_a: Spring,
    art_out_a: Spring,
    /// The two layers' resolved textures AND their decoded pixel sizes (`(tex, tw, th)` from
    /// [`crate::ui::widgets::resolve_tex_wh`]), taken in `update` so `draw` cannot disagree with the
    /// springs about whether there is anything to reveal — and so the request is issued in the
    /// UPDATE phase, one step earlier than `backdrop_art`'s own cull used to allow. The size rides
    /// along free (one store probe) and is what lets the layer `Rect::cover` its frame instead of
    /// stretching a 16:9 source across whatever the panel happens to be.
    tex: (u32, f32, f32),
    tex_out: (u32, f32, f32),
    /// The pool index the springs are keyed to. `update` compares it against the live one and
    /// re-seats on a change, so the flip does not have to reach in from [`hero_flip`] (a free fn with
    /// no scene handle) — and the `plxnative-heroidx` dev jump, which changes the index with no slide
    /// at all, is covered by the same rule for free.
    ///
    /// This is the ONLY thing sequencing the springs to the page: an edit that moves the hero index
    /// after `Backdrop::update` in the frame would dissolve the wash from the wrong page. `home_update`
    /// runs the auto-flip (and the slide step) before it calls `bg.update` — keep that order.
    keyed: c_int,
}
impl Backdrop {
    fn new() -> Self {
        // Mounts flat on the app's own ground; `update` JUMPS it to the first hero's colours
        // (keyed < 0) so Home does not fade up from grey on every boot.
        Backdrop {
            wash: AmbientWash::flat(theme::SURFACE_APP),
            art_a: Spring::at(0.0),
            art_out_a: Spring::at(0.0),
            tex: (0, 0.0, 0.0),
            tex_out: (0, 0.0, 0.0),
            keyed: -1,
        }
    }
}
impl View for Backdrop {
    fn update(&mut self, env: &Env) {
        // Resolved only while the hero band can still show them ([`HERO_ART_CULL`]) — the cull
        // `backdrop_art`'s own `sp < HERO_ART_CULL` gate used to perform when the resolve lived in
        // `draw`. Past the dive both layers read as "no texture", which is exactly what the rest of
        // this component already means by a texture that has not arrived: `reveal` reports 0, the
        // ground's skip test believes the texture rather than the spring, and `draw` paints nothing
        // — so the struct's promise that draw cannot disagree with the springs still holds.
        // `_on(m.sid, …)`: with Home merged the hero rotation carries BORROWED items, whose `art`
        // path is a key on their own server. The bare form resolves against whichever server is
        // current and would draw the friend's billboard as a blank page.
        let art_of = |m: Option<&PmsMovie>| {
            m.map(|m| crate::ui::widgets::resolve_tex_wh_on(m.sid, &m.art, 1280, 720, 0))
                .unwrap_or((0, 0.0, 0.0))
        };
        let live = env.sp < HERO_ART_CULL;
        self.tex = if live { art_of(hero_item()) } else { (0, 0.0, 0.0) };
        self.tex_out =
            if live { art_of(hero_slide_state().and_then(|(prev, _, _)| hero_item_at(prev))) } else { (0, 0.0, 0.0) };

        let cur = hero_index() as c_int;
        if cur != self.keyed {
            self.art_out_a.jump(self.art_a.pos); // the leaving layer keeps the reveal it had
            // prefetch HIT ⇒ no fade at all. Past the dive `tex` is the cull's 0 rather than an
            // answer about the store, so a re-key down there (only a hub refetch can move the index
            // while the grid is up — the auto-flip is gated on `snap.pos < 0.05` and a manual flip
            // needs hero focus) starts the layer at 0 and DISSOLVES it in on the way back up. That
            // is this component's own "the backdrop was not ready" behaviour, not a new one.
            self.art_a.jump(if self.tex.0 != 0 { 1.0 } else { 0.0 });
            if self.keyed < 0 {
                self.wash.jump(wash_corners(hero_item(), env.sp)); // mount: no dissolve from grey
            }
            self.keyed = cur;
        }
        // the reveals run UP only (see the field doc); the re-seat above is the only thing that
        // lowers one, and it does so exactly when the page it belonged to changed.
        if self.tex.0 != 0 {
            self.art_a.step(1.0, AmbientWash::K, env.dt);
        }
        if self.tex_out.0 != 0 {
            self.art_out_a.step(1.0, AmbientWash::K, env.dt);
        }
        self.wash.step(wash_corners(hero_item(), env.sp), AmbientWash::K, env.dt);
    }
    fn draw(&self, env: &Env, p: Painter) {
        let sp = env.sp;
        let slide = hero_slide_state();
        let (a_in, a_out) = (reveal(self.tex.0, &self.art_a), reveal(self.tex_out.0, &self.art_out_a));
        // The GROUND, standing in for the flat dark-gray base `home_draw`'s frame_clear(CLEAR_RGB)
        // already laid down (CLEAR_RGB == SURFACE_APP #2C2C2E — which is why the redundant
        // full-screen SURFACE_APP rect that used to sit here was deleted for fill-rate). It is drawn
        // only when something can actually see it: skipped when opaque art covers the panel, and
        // skipped when it has resolved to the clear colour anyway. BOTH skips matter — without the
        // first the hero view pays an extra ~2M-fragment pass it does not need, without the second
        // the whole GRID does.
        if !wash_hidden(sp, a_in, slide.map(|_| a_out))
            && !self.wash.is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS)
        {
            self.wash.draw(p, Rect::FULL);
        }
        // hero backdrop ART over it — confined to the hero view, fading out as the grid rises so the
        // shelf area stays flat gray. During a flip the outgoing and incoming items' art slide
        // side-by-side (same phase as the text), each at its own reveal.
        if sp < HERO_ART_CULL {
            if let Some((_, dx_out, dx_in)) = slide {
                backdrop_art(p, self.tex_out, sp, dx_out, a_out);
                backdrop_art(p, self.tex, sp, dx_in, a_in);
            } else {
                backdrop_art(p, self.tex, sp, 0.0, a_in);
            }
        }
        if env.hero_a > 0.01 {
            // The hero TEXT SCRIM CONTRACT, in two parts, because one axis cannot do both jobs.
            //
            // PART ONE, here: the frame-wide atmospheric ramp ([`base_scrim_a`], two stacked
            // stops). It sets the picture's mood and gives the shelf line its depth. It is NOT
            // what makes the copy readable, and an earlier version of this comment claiming the
            // text band was "y≈550–700" is what sent a tuning pass at the wrong band: the column
            // is bottom-anchored on `HERO_TEXT_BOTTOM` 692 and grows UP, so the title band's top
            // is at y≈387 (`hero_stack_top`) and the HERO-72 fallback's cap top — baselined on the
            // band's bottom — at y≈455, where this ramp supplies only ~0.14, under 2:1 over
            // bright art. It cannot be raised to fix that, because at any given y it is uniform
            // across all 1920px: enough alpha up there to rescue the title would put most of a
            // black frame over the whole picture on the title's line. (The clearLogo the band
            // usually holds paints higher still — to y≈239 for a square — where this ramp is
            // nothing at all and only the wedge below is working.)
            let sa = base_scrim_bottom_a(env.hero_a);
            let mid = sa * BASE_SCRIM_MID_K;
            // the two bands share ONE seam expression, so the pair is watertight and the paint
            // cannot drift from `base_scrim_a`'s idea of where the stops are
            p.rect(Rect::new(0.0, HERO_BASE_SCRIM_Y0, SCR_W, BASE_SCRIM_Y1 - HERO_BASE_SCRIM_Y0), 0.0,
                theme::scrim(0.0), theme::scrim(mid), 0.0);
            p.rect(Rect::new(0.0, BASE_SCRIM_Y1, SCR_W, SCR_H - BASE_SCRIM_Y1), 0.0,
                theme::scrim(mid), theme::scrim(sa), 0.0);
            // PART TWO: the corner the text is actually IN, darkened along the axis the ramp has
            // nothing to say about. `right: false` — home's hero is one left-aligned column.
            crate::ui::widgets::hero_scrim(p, env.hero_a, false);
        }
    }
}

/// This ramp's own stop, as absolute y: it reaches [`BASE_SCRIM_MID_K`] of its full weight here (the
/// two bands' shared seam) and runs to full at the foot of the panel. Named because `Backdrop::draw`
/// and [`base_scrim_a`] must read the SAME numbers — the whole reason that function exists.
///
/// Where it STARTS is [`HERO_BASE_SCRIM_Y0`], shared with the detail page: the two screens declared
/// that line verbatim under the same name in two files. Only the origin is shared — the CURVE below
/// it is deliberately not (see that const).
const BASE_SCRIM_Y1: f32 = 0.65 * SCR_H; // 702
/// The midpoint weight, as a fraction of the foot's.
const BASE_SCRIM_MID_K: f32 = 0.55;

/// The atmospheric ramp's alpha at the FOOT of the panel, for a hero at `hero_a`. The 0.30 floor is
/// pre-existing and slightly odd — `Backdrop::draw` gates the whole block at `hero_a > 0.01`, where
/// this still reads 0.306, so there is a 0.3-alpha cliff at the frame's foot as the snap crosses
/// the gate. Flagged rather than fixed: retuning the atmospheric floor is a different change from
/// fixing the text corner, and only the panel can judge it.
pub(crate) fn base_scrim_bottom_a(hero_a: f32) -> f32 {
    0.30 + 0.64 * hero_a.clamp(0.0, 1.0)
}

/// The frame-wide atmospheric ramp's alpha at `y` — the two stops [`Backdrop`] paints, as one pure
/// function so the paint and the legibility contract cannot read different numbers. It is the base
/// the hero wedge composites over; `widgets`' anchor table grades
/// `1 − (1 − base_scrim_a) · (1 − hero_scrim_a)` against the real text ys.
pub(crate) fn base_scrim_a(y: f32, hero_a: f32) -> f32 {
    let sa = base_scrim_bottom_a(hero_a);
    let mid = sa * BASE_SCRIM_MID_K;
    if y <= HERO_BASE_SCRIM_Y0 {
        0.0
    } else if y < BASE_SCRIM_Y1 {
        mid * (y - HERO_BASE_SCRIM_Y0) / (BASE_SCRIM_Y1 - HERO_BASE_SCRIM_Y0)
    } else {
        mid + (sa - mid) * ((y - BASE_SCRIM_Y1) / (SCR_H - BASE_SCRIM_Y1)).clamp(0.0, 1.0)
    }
}

/// The wash's target corners: the current hero's UltraBlur envelope, each corner leaned from the
/// app's own ground toward it by [`HERO_WASH_W`], the whole lean falling to zero as the snap dives
/// into the grid.
///
/// Mixing FROM [`theme::SURFACE_APP`] (through [`AmbientWash::keyed`], which also holds an artwork
/// corner under the shared luminance cap) — instead of scaling the corners with `Painter::ambient`'s
/// `dim`, which is what this replaces — fixes two black screens at once. `dim` fades toward BLACK:
/// at snap 0.9 the old code painted the whole panel opaque near-black BEHIND the grid and then
/// snapped it to `#2C2C2E` at 0.996, taking the medium-grey base the card drop-shadows are tuned
/// against with it. And an item with NO `UltraBlurColors` drew nothing at all, leaving the hero
/// scrim (up to 0.94 alpha) sitting on the bare clear. With a floor at the app surface, "no artwork"
/// IS the app's flat ground with no special case, and the grid end of the snap lands on exactly the
/// colour `home_draw`'s `frame_clear` already laid down — which is also what keeps this wash
/// hero-scoped rather than a tint over the shelves.
fn wash_corners(m: Option<&PmsMovie>, sp: f32) -> [[f32; 4]; 4] {
    let lean = (1.0 - sp).clamp(0.0, 1.0);
    match m.filter(|m| m.has_blur) {
        Some(m) => AmbientWash::keyed(m.blur, HERO_WASH_W.map(|w| w * lean)),
        None => [theme::SURFACE_APP; 4],
    }
}

/// Would every pixel of the ground be painted over anyway? Pure, and split out because the
/// full-screen fill it skips is this component's whole fill-rate cost and is invisible on the host.
/// Three ways something sees through: the snap has begun (which both fades the art by `1 - sp` and
/// slides it up off the bottom of the panel by `sp * (SCR_H - 120)`), the current layer's reveal is
/// unfinished, or a flip has a second layer on the panel that is unfinished. `art_out` is `None`
/// when no flip is running.
fn wash_hidden(sp: f32, art: f32, art_out: Option<f32>) -> bool {
    sp <= 0.005 && art > 0.995 && art_out.map(|a| a > 0.995).unwrap_or(true)
}

/// A layer's EFFECTIVE reveal: its spring, or 0 when there is no texture behind it. The spring is
/// monotone within one keying (see [`Backdrop::art_a`]), so it can be high while the texture is
/// briefly gone — and the ground's skip test must believe the TEXTURE, not the spring, or the wash
/// would be skipped over a layer drawing nothing.
fn reveal(tex: u32, s: &Spring) -> f32 {
    if tex != 0 {
        s.pos
    } else {
        0.0
    }
}

/// One hero page's backdrop ART layer at slide offset `dx` and reveal `a` (0 = the ground shows
/// through, 1 = the photograph), from the `(tex, tw, th)` the [`Backdrop`] resolved this frame. Only
/// the ART lives here now — the wash is ONE full-screen [`AmbientWash`] drawn under both layers by
/// `Backdrop::draw`. A layer the slide has already carried off the panel is culled — the outgoing
/// art is gone for most of the transition, and skipping it keeps the two-layer flip from doubling a
/// full-screen fill.
///
/// The frame is `Rect::cover`ed onto the decoded source: the transcode preserves the source aspect
/// inside the requested 1280×720 box, so an art asset that is not 16:9 would otherwise be SQUASHED
/// across the full panel (`Painter::tex` maps UV 0..1 across the rect).
fn backdrop_art(p: Painter, tex: (u32, f32, f32), sp: f32, dx: f32, a: f32) {
    let (t, tw, th) = tex;
    if t == 0 || a <= 0.01 || !on_axis(dx, SCR_W, SCR_W, 0.0) {
        return;
    }
    p.tex(t, Rect::new(dx, -sp * (SCR_H - 120.0), SCR_W, SCR_H).cover(tw, th), 0.0,
        theme::with_a(theme::TINT_WHITE, a * (1.0 - sp)));
}

/// The ratingKey whose clearLogo headlines a hero page: for an EPISODE the SHOW's (the hero
/// headlines the series — [`hero_content`]'s rule), else the item's own. Its own fn because the
/// prefetch must warm the same key the draw asks for; a warm on the episode's rk is a different slot
/// and buys nothing.
fn hero_logo_rk(m: &PmsMovie) -> &str {
    if m.kind == 3 && !m.show_rk.is_empty() {
        &m.show_rk
    } else {
        &m.rk
    }
}

/// The pool pages a prefetch should warm around `cur`, nearest-and-FORWARD first (+1, −1, +2, −2 …),
/// wrapped into a pool of `n`, with `cur` itself and duplicates dropped (a two-page pool makes +1
/// and −1 the same page). Forward first because RIGHT at the end of the row and the idle auto-flip
/// both page forward, so with one key of budget per frame that is the page most likely to be seen next. Writes
/// into `out`, returns how many it wrote — alloc-free on a per-frame path, and pure so the wrap and
/// the de-dup are host-testable without a live pool.
fn prefetch_order(cur: c_int, n: c_int, out: &mut [c_int; 2 * HERO_PREFETCH]) -> usize {
    if n <= 1 {
        return 0;
    }
    let cur = cur.rem_euclid(n);
    let mut k = 0;
    for d in 1..=HERO_PREFETCH as c_int {
        for s in [d, -d] {
            let i = (cur + s).rem_euclid(n);
            if i != cur && !out[..k].contains(&i) {
                out[k] = i;
                k += 1;
            }
        }
    }
    k
}

/// The prefetch gate's SCREEN half, pure: warm only while the billboard is actually on screen, and
/// not while a flip is running (the layer coming IN is the thing being waited on — the page after it
/// can wait 400 ms). Split from [`prefetch_hero_neighbours`] so the rule is testable without a
/// texture store or a live pool (the `status_takes` idiom).
///
/// The store's own "am I idle" half is deliberately NOT a parameter: Rust evaluates arguments
/// eagerly, so passing it in meant `posters::store_idle` — a store-mutex lock and a 64-slot scan —
/// ran on every Home frame including the whole grid scene, where `sp` alone had already settled the
/// answer. The caller `&&`s the two, so the cheap test short-circuits the expensive one.
fn prefetch_armed(sp: f32, sliding: bool) -> bool {
    sp < 0.05 && !sliding
}

/// Warm the artwork of the hero pages either side of the one on screen, so a flip has its backdrop
/// (and clearLogo) already decoded instead of starting the fetch on the first frame of the slide —
/// which is what put a bare wash on the panel for the length of an image-transcode round trip every
/// time the billboard turned over.
///
/// It issues at most ONE key per frame, from an otherwise idle store. Both halves are load-bearing:
///  * the idle gate is the only way to stay off the critical path, because `poster_worker` has no
///    priority — it claims the first `P_WANT` slot BY SLOT INDEX, and indices are handed out
///    arbitrarily (see [`crate::posters::store_idle`]);
///  * one key per frame caps the race that remains (a tile that misses the frame after a warm went
///    out): the two workers hold at most one prefetch, so the worst a visible poster can queue behind
///    is a single fetch, never a batch of four.
///
/// A depth-1 prefetch is therefore fully enqueued over ~4 idle frames — orders of magnitude inside
/// the 8-second auto-flip. On a server slow enough that the store is never idle the prefetch simply
/// never fires and the screen degrades to exactly the old behaviour, which is the right degradation.
fn prefetch_hero_neighbours() {
    use crate::posters::Warm;
    // the screen's own gate FIRST, so the store probe behind `store_idle` (a lock plus a 64-slot
    // scan) is only paid on the frames where the answer could still be yes
    if !(prefetch_armed(snap_pos(), hero_slide_state().is_some()) && crate::posters::store_idle()) {
        return;
    }
    let n = crate::pms::hero_pool_len() as c_int;
    let mut order = [0 as c_int; 2 * HERO_PREFETCH];
    let k = prefetch_order(hero_index() as c_int, n, &mut order);
    for &i in &order[..k] {
        let Some(m) = hero_item_at(i) else { continue };
        // the backdrop first: it is the full-screen layer, and the one whose absence reads as a blank
        // page. A missing logo only costs the title band a text→logotype swap.
        // the item's own server, matching the resolve the draw will make — a warm that named a
        // different server than the draw would fetch the picture twice and use neither
        if crate::ui::widgets::warm_tex_on(m.sid, &m.art, 1280, 720, 0) == Warm::Claimed {
            return;
        }
        // the same server the draw will resolve against (`hero_logo`), or the warm names a
        // different slot and buys nothing
        if crate::posters::logo_warm(m.sid, hero_logo_rk(m)) == Warm::Claimed {
            return;
        }
    }
}

// ---- Hero: low-left content composite. Drawn under p.alpha(hero_a) so the whole
// group fades as one; widgets carry base alphas that the cascade scales.
struct Hero;
impl Hero {
    fn new() -> Self {
        Hero
    }
}
/// The top of the hero's bottom-anchored text stack: the title band, the `space::MD` under it, the
/// kicker and the synopsis block (`syn_h` already carries its own `space::SM` lead-in), all stacked
/// UP from [`HERO_TEXT_BOTTOM`]. Pure and shared with the tests, because the one genuinely new
/// behaviour on this page is that a clearLogo may paint ABOVE this line — up to
/// `theme::logo::HERO_H_MAX - hero_logo::band_h` px of it — and the failure mode (a 268px logo
/// colliding with the tab pills) is otherwise only observable by waking a television.
///
/// `pub(crate)` for one outside reader: `widgets`' hero-scrim anchor table, which grades this page's
/// title where the draw actually puts it rather than re-typing the stack.
pub(crate) fn hero_stack_top(title_h: f32, meta_h: f32, syn_h: f32) -> f32 {
    HERO_TEXT_BOTTOM - (title_h + theme::space::MD + meta_h + syn_h)
}

/// **The hero meta line's right bound**, and the only width budget this line has ever had — three
/// fifths of the frame.
///
/// The line is bottom-left copy over a full-bleed photograph, so it is legible only inside the
/// corner wedge, which is gone by `widgets::HERO_SCRIM_W` (four fifths, 1536) and leaves the last
/// fifth of the picture completely alone. Ending the line at three fifths keeps a whole fifth of the
/// panel between it and that edge — "well short of the last fifth", the design's own words — so the
/// run always sits where the wedge is still carrying weight, and never wanders into the side of the
/// frame the composition is usually about.
///
/// It bounds the SOURCE run only. The line's own facts keep wrapping to [`HERO_COL_W`] exactly as
/// they always have, which is what makes the annotation free for a single-server install and is why
/// the handle is what truncates when the two together are too long: the run is the last thing on the
/// line and a fact about where the item came from, so it is the right thing to lose first — and
/// losing it is a truncation, never a second line. The hero column is a FOUR-line composition
/// (title band, this line, three of synopsis) anchored on [`HERO_TEXT_BOTTOM`]; a fifth line would
/// push the pinned action row into the peeking shelf.
const HERO_META_R: f32 = 0.60 * SCR_W; // 1152
/// [`HERO_META_R`] in the meta flow's own coordinates, whose origin is the text column's left edge.
const META_FLOW_W: f32 = HERO_META_R - MARGIN_X;

/// The words the run is made of. "Shared by friend" — the PERSON, not the machine: the same handle
/// the shelf headings carry, never a server's own name.
/// The hero's own caller has already returned for an empty handle, so the flattened form is safe
/// here — the words live in [`crate::ui::fmt::shared_by`], which is where the detail page's facts
/// row and the Library read-out get them too.
fn shared_by(source: &str) -> String {
    crate::ui::fmt::shared_by(source).unwrap_or_default()
}

/// The hero meta line's SOURCE annotation, flowed onto the end of a line whose own facts have
/// already been laid out to `base_w`: `· Shared by friend`, after a middot at `TEXT_SEPARATOR`'s
/// .45, both runs on the line's own **BODY** rung and one step of ink under it.
///
/// **With no source there is no flow at all** — no pad, no dot, no run, no draw call, and the meta
/// line is left byte for byte the one it has always been. That is the design's "with one source,
/// none of this is drawn" as absence rather than as a branch that draws nothing visible.
///
/// The rung is the part that does NOT travel from the detail page's version of this run. There it is
/// `CAPTION`, because that line sits under a page of copy; here it is `BODY`, because this line is
/// read from across the room. A rung is a role — what transfers is the step of INK
/// (`TEXT_SECONDARY` → `TEXT_TERTIARY`), which is what makes the annotation quieter than the facts
/// it joins on every screen that carries it.
///
/// `run(text, dx, budget, sz, bold, ink) -> advance` is the seam, as in [`heading_flow`]: the draw
/// passes a closure that elides to `budget` and paints, the host tests pass one that measures. So
/// the drawn flow and the graded one are one expression. Each run's `budget` is the distance from
/// where it starts to [`META_FLOW_W`], which is what makes the line TRUNCATE instead of ever
/// becoming two: there is no wrap in this flow to reach, only a right bound to elide against.
/// Returns the whole line's advance, source run included.
fn meta_source_flow(base_w: f32, source: &str, mut run: impl FnMut(&str, f32, f32, c_int, c_int, [f32; 4]) -> f32) -> f32 {
    if source.is_empty() {
        return base_w; // one source: the meta line is the item's facts and nothing else
    }
    let mut dx = base_w + SOURCE_PAD;
    dx += run("\u{b7}", dx, META_FLOW_W - dx, theme::size::BODY, 0, theme::TEXT_SEPARATOR);
    dx += SOURCE_PAD;
    dx += run(&shared_by(source), dx, META_FLOW_W - dx, theme::size::BODY, 0, theme::TEXT_TERTIARY);
    dx
}

/// Draw [`meta_source_flow`] with its runs' cap tops on `y` — the meta line's own, since every run
/// shares that line's rung and weight and therefore its cap band. `base_w` is how wide the facts
/// ahead of it actually drew ([`meta_drawn_w`]); the runs ride the painter handed in, so the hero's
/// slide carries the annotation with the line it belongs to.
fn draw_meta_source(p: Painter, source: &str, x: f32, y: f32, base_w: f32) {
    meta_source_flow(base_w, source, |s, dx, budget, sz, bold, ink| {
        // the CString must outlive the draw call, not the closure (`ui/CLAUDE.md`'s first gotcha)
        match CString::new(crate::text::elide(s, budget, sz, bold, false)) {
            Ok(cs) => {
                let mut lab = Label::new(cs.as_ptr(), sz, ink).v(VAlign::CapTop);
                if bold == 1 {
                    lab = lab.bold();
                }
                lab.draw(p, Rect::new(x + dx, y, budget, 0.0))
            }
            Err(_) => 0.0,
        }
    });
}

/// How wide the meta line's own facts DREW, so the source run can be flowed onto their end rather
/// than onto a guess. The line is a one-line [`TextView`], whose wrap rebuilds it as its words
/// joined by single spaces — which is the string itself, since every separator on it is authored as
/// one space — so a line that fits measures exactly. A line that did NOT fit was ellipsized to fill
/// the column, and reports the column: the run then starts at the column's edge, one pad past the
/// ellipsis. `truncates` shares the draw's memoized wrap, so asking costs nothing.
///
/// The clamp is not belt-and-braces. `truncates` answers "were WORDS left unplaced", and a single
/// token too wide for the column leaves none — `TextView` ellipsizes that case in its own safety
/// pass instead, so the measured string can be wider than anything that was drawn. Unclamped, the
/// run would then start in mid-air past the ellipsis, and a truly absurd one could hand the flow a
/// negative budget, which `text::elide` reads as "no bound at all" and would paint straight past
/// the frame.
fn meta_drawn_w(tv: &TextView<'_>, meta: &str, col_w: f32) -> f32 {
    if tv.truncates(col_w) {
        return col_w;
    }
    CString::new(meta)
        .ok()
        .map(|c| crate::text::text_width(c.as_ptr(), theme::size::BODY, 0))
        .unwrap_or(0.0)
        .min(col_w)
}

/// One hero item's sliding content column: title band (clearLogo or text) → small meta/kicker →
/// synopsis. For an EPISODE the show is the star: the show's clearLogo/title in the title band and
/// a "S1 E4 · Episode title" kicker — the episode's own name never headlines. The column is
/// **bottom-anchored** on `HERO_TEXT_BOTTOM`: heights are measured first and the stack grows UP,
/// so the synopsis' last line — and with it the pinned action row + page dots below — sits at the
/// same y for every item (top-down flow made the chrome jump on every flip). Every gap is a
/// `theme::space` rung and every step advances by the *measured* height of the element just drawn —
/// except the title band, whose height is the RESERVED `hero_logo::band_h` rather than the drawn
/// logo's, so the stack cannot lurch the moment a texture lands (see `ui::hero_logo`).
///
/// `source` is whose server this item came from, empty for our own ([`hero_source_at`]). It rides
/// the meta line as that line's LAST run and changes nothing else about the column — including its
/// height, which is measured from the facts alone: the annotation is bounded to one line by
/// construction ([`meta_source_flow`]), so it can never reflow the stack it sits in.
fn hero_content(hero: &PmsMovie, source: &str, p: Painter, dx: f32) {
    let tx = MARGIN_X;
    let col_w = HERO_COL_W;
    // a column the slide has carried off the panel costs a full text layout + glyph draws for
    // nothing (same rule as the backdrop layer's cull). The box is the column's — EXCEPT when a
    // source run is on the meta line, which is the one thing here that draws outside the column
    // (out to [`META_FLOW_W`]): culled on the column's width, a slide would take the whole block
    // away while ~400px of that line was still on the panel, and the tail of the meta line would
    // blink out ahead of the item it belongs to.
    if !on_axis(tx + dx, if source.is_empty() { col_w } else { META_FLOW_W }, SCR_W, 0.0) {
        return;
    }
    let d_a = theme::TEXT_SECONDARY;

    let is_ep = hero.kind == 3;
    let title: &str = if is_ep && !hero.show_title.is_empty() { &hero.show_title } else { &hero.title };
    // The title band is a FIXED layout height whatever the logo turns out to be
    // ([`hero_logo::band_h`]): a squarer logo spills upward as paint, so nothing below moves when a
    // texture lands and two pool pages with different logo aspects stay level through a flip.
    let title_h = hero_logo::band_h(LogoRung::Hero);

    // meta/kicker line — episodes: "S1 E4 · Episode title"; else "Movie/Show · YEAR · RATING"
    let meta = if is_ep {
        let ep_title = &hero.title;
        let mut s = String::new();
        if hero.season_index > 0 {
            s.push_str(&format!("S{} ", hero.season_index));
        }
        if hero.ep_index > 0 {
            s.push_str(&format!("E{}", hero.ep_index));
        }
        if !s.is_empty() && !ep_title.is_empty() {
            s.push_str(" \u{b7} ");
        }
        s.push_str(ep_title);
        s
    } else {
        let rating = &hero.rating;
        let noun = if hero.kind == 1 { "Show" } else { "Movie" };
        format!("{} \u{b7} {} \u{b7} {}", noun, hero.year, if rating.is_empty() { "NR" } else { rating })
    };
    let meta_tv = TextView::new(&meta, theme::size::BODY, d_a).max_lines(1);
    let meta_h = meta_tv.measure_h(col_w);

    // synopsis — the SHARED hero blurb ([`crate::ui::hero_synopsis`]: `size::LABEL` 26 on a 36px
    // leading in `TEXT_READING`, capped at 3 lines), pixel-wrapped to the hero column. It is
    // shared because the detail page's hero draws the same role, and for one release the two
    // disagreed about the rung, the leading AND the ink; that function carries the argument for
    // the values. Home passes no lead run — an episode PREFIX is the detail hero's, whose blurb is
    // the episode's own summary; this line is whatever the pooled item says about itself.
    //
    // The 3-line cap is also this screen's own ceiling: a 4th line would push the pinned action
    // row's clearance into the peeking shelf. Raising the rung DID move the whole column up (the
    // stack is bottom-anchored on `HERO_TEXT_BOTTOM`), by `3 * (36 - 29)` = 21px, which is why the
    // two layout contracts that quote this block's height read it from `ui::hero_syn_h` now.
    let summary = &hero.summary;
    let syn = (!summary.is_empty()).then(|| crate::ui::hero_synopsis(summary, ""));
    let syn_h = syn.as_ref().map(|tv| theme::space::SM + tv.measure_h(col_w)).unwrap_or(0.0);

    // stack the measured blocks up from the anchor, then draw top-down
    let mut y = hero_stack_top(title_h, meta_h, syn_h);
    HeroLogo::new(hero.sid, hero_logo_rk(hero), title, LogoRung::Hero).draw(p, Rect::new(tx, y, col_w, title_h));
    y += title_h + theme::space::MD;
    meta_tv.draw(p, Rect::new(tx, y, col_w, 0.0));
    // Guarded, because the ARGUMENT is the expensive part: `meta_drawn_w` builds a `CString` and
    // runs an uncached `TTF_SizeUTF8` over the whole meta line, and Rust evaluates it before the
    // call can decide there is nothing to draw. A single-server install has no source run and is
    // documented as paying nothing for the feature — it was paying a measure per hero frame, twice
    // per frame through a slide.
    if !source.is_empty() {
        draw_meta_source(p, source, tx, y, meta_drawn_w(&meta_tv, &meta, col_w));
    }
    y += meta_h;
    if let Some(tv) = syn {
        tv.draw(p, Rect::new(tx, y + theme::space::SM, col_w, 0.0));
    }
}

/// One hero item's action row — Play/Continue pill, info circle, and the pager MARK — at horizontal
/// slide offset `dx`. It rides WITH its item, in the same phase as the text column and the backdrop
/// art: the pill's label *is* the item's ("Continue" when the press would actually resume it, else
/// "Play"), and so is its width, so a row that stayed put would have to relabel and resize itself
/// mid-flip — the one thing on screen contradicting the motion.
///
/// The row sits at a FIXED y (the text column above is bottom-anchored, so the button-to-text air
/// is one MD for every item). The two CONTROLS are the focus row (hero_fc), so LEFT/RIGHT walk
/// them instead of paging. The pill launches playback directly (the info circle is the road to the
/// detail page). MD, not LG: the synopsis' leading box already carries ~7px of descender slack, and
/// the bigger rung read as the button drifting from its text.
///
/// `live` marks the INCOMING (real) row, the one whose rects become the pointer hit targets. They
/// are recorded at the row's DRAWN position — the same draw-and-hit-must-agree rule the grid's
/// `card_x` keeps — so a click mid-flip lands on the button the eye sees, and the outgoing ghost
/// is never clickable.
fn hero_actions(hero: &PmsMovie, env: &Env, p: Painter, dx: f32, live: bool) {
    let tx = MARGIN_X;
    let pill_y = HERO_ROW_Y;
    let hf = hero_focus();
    let (cd, cgap) = (HERO_CTRL_D, HERO_CTRL_GAP); // control diameter + inter-control gap
    // The label asks the function the PRESS applies — `app.rs`'s `play_item_now` starts this item
    // at `metadata::resume_ns`, which refuses an offset of 10s or less, or one past 95%. Keyed on
    // a raw `resume_ms > 0`, a four-second offset (or one the server left past the end) labelled
    // the pill "Continue" for a play that then began at 0. The detail page's own pill takes the
    // same route for the same reason — the word has to promise what the press delivers.
    let resumes = crate::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
    let plabel = if resumes { c"Continue" } else { c"Play" };
    let pw = Button::pill_w(plabel.as_ptr(), theme::size::BODY, true); // measured from the label it carries
    // local (painter-relative) frames, and the screen-space rects that mirror them
    let pill = Rect::new(tx, pill_y, pw, cd);
    let info = Rect::new(tx + pw + cgap, pill_y, cd, cd);
    // The pager MARK, vertically centred on the control row: whole pixels, because an icon mask is
    // 1:1 texel content and a fractional box would soften the stroke (`icons::draw` snaps the
    // ORIGIN; the size is ours to keep whole).
    let pd = HERO_PAGER_D.round();
    let mark = Rect::new(info.x + cd + HERO_PAGER_PAD, pill_y + (cd - pd) * 0.5, pd, pd);
    if live {
        // recorded BEFORE the cull: a live row carried off the panel must take its hit targets
        // with it, or a click would still land on a button that is no longer there.
        let screen = [pill, info].map(|r| Rect::new(r.x + dx, r.y, r.w, r.h));
        unsafe { addr_of_mut!(hero_btns).write(screen) };
    }
    // measured to the mark's PADDED right edge — the row's real extent, so a paging row is culled
    // at the moment the last thing drawn in it leaves the panel and not a control-box early
    if !on_axis(tx + dx, mark.x + pd + HERO_PAGER_PAD - tx, SCR_W, 0.0) {
        return;
    }
    Button::new(plabel.as_ptr(), theme::size::BODY, pill).icon(Icon::Play).focused(hf == 0).draw(env, p);
    CircleButton::new(c"".as_ptr()).icon(Icon::Info).at(info.x, info.y).focused(hf == 1).draw(env, p);
    // The pager is an **INDICATOR, not a control**: it says the billboard pages, and that is all it
    // does. It takes no focus (`HERO_NBTN` counts the two controls above it), it records no hit
    // rect, and nothing in the app can activate it — pressing it was never the point, since RIGHT
    // at the end of the row, LEFT at its start and the idle auto-flip already page the carousel.
    //
    // So it wears no control geometry either. It is drawn at its NATURAL size (`HERO_PAGER_D` — the
    // same box the info disc carries its glyph in), stood off the disc by the row's own gap
    // (`HERO_PAGER_PAD`, measured to the box — see its doc), rather than as a glyph floating inside
    // a 60px button box whose empty half pushed it 14px further right than the rhythm wanted. What
    // ended here before was a disc-backed `CircleButton`, then a bare-styled one; both were still a
    // button, which is the thing this is not.
    crate::ui::icons::draw(p, Icon::Chevron, mark, theme::TEXT_SECONDARY);
}

impl View for Hero {
    fn draw(&self, env: &Env, p: Painter) {
        let Some(hero) = hero_item() else {
            return;
        };

        // per-item content + its action row, sliding during a flip (same phase/direction as the
        // backdrop art) — everything that belongs to the ITEM travels together.
        if let Some((prev, dx_out, dx_in)) = hero_slide_state() {
            if let Some(ph) = hero_item_at(prev) {
                let po = p.translate(dx_out, 0.0);
                hero_content(ph, hero_source_at(prev), po, dx_out);
                hero_actions(ph, env, po, dx_out, false);
            }
            let pi = p.translate(dx_in, 0.0);
            hero_content(hero, hero_source(), pi, dx_in);
            hero_actions(hero, env, pi, dx_in, true);
        } else {
            hero_content(hero, hero_source(), p, 0.0);
            hero_actions(hero, env, p, 0.0, true);
        }

        // Page indicator: one dot per pooled hero item, the current one lit — CENTRED on the panel,
        // because it paces the whole billboard rather than the action row it happens to sit under
        // (left-aligned it read as a fourth control in that row). SM keeps it on the action row's
        // baseline band — at MD it hovered midway to the peeking shelf and read as stuck to the
        // poster row instead.
        let pool_n = crate::pms::hero_pool_len();
        if pool_n > 1 {
            PageDots::new(pool_n)
                .active(hero_index())
                .centered_at(SCR_W * 0.5, HERO_ROW_Y + HERO_CTRL_D + theme::space::SM)
                .draw(env, p);
        }
    }
}

// ---- Grid: the collection view. Holds one CardRow per hub (the shared animated shelf component) +
// the vertical scroll spring; drives nav/hit-test/wheel, draws all non-focused cells then the single
// focused card LAST (cross-row z-order, invariant #3). CardRow owns the per-cell scale springs + the
// scroll spring + the tile rendering — the same component detail's Related row uses.
struct Grid {
    shelves: [CardRow; MAX_HUBS],
    scroll_y: Spring,
}
impl View for Grid {
    fn update(&mut self, env: &Env) {
        // delegate each row's per-cell scale springs + scroll spring to the shared CardRow (only the
        // focused row's scroll animates — focused=None freezes it, matching the old Shelf::update).
        //
        // The pop belongs to the GRID: in hero view the shelf only peeks and focus is on the
        // billboard, so a magnified tile down there read as "already selected" — and, being scaled
        // about its centre, it also broke the peek row's even margin/gap rhythm. No cell is focused
        // until the snap has committed to the grid, on the SAME 0.5 threshold `draw`'s focused-last
        // pass and [`focus_is_card`] use, so the pop, the label and the z-order all arrive together.
        let grid = env.sp > 0.5;
        for r in 0..MAX_HUBS {
            let focused = (grid && env.fr as usize == r).then_some(env.fc as usize);
            self.shelves[r].update(crate::pms::hub_len(r), focused, &RowStyle::HOME, env.dt);
        }
        let nh = n_hubs().max(1);
        let max_y = (nh as f32 * ROW_PITCH - (SCR_H - CONTENT_Y) + 60.0).max(0.0);
        // minimal scroll-into-view (the vertical twin of CardRow's rule, same reveal core): only
        // move the page when the focused row's block — title band above, card + focused label
        // below — would clip the viewport; a fully visible row never re-seats the page.
        let top = env.fr as f32 * ROW_PITCH;
        let lo = top + GRID_TOP_Y + CARD_DY + CARD_H + 96.0 - (SCR_H - 24.0); // card + title/caption metadata visible
        let hi = top + GRID_TOP_Y - 66.0 - 96.0; // hub title band clear below the chip row
        self.scroll_y.step(card_row::reveal(self.scroll_y.pos, lo, hi, max_y), K_SCROLL, env.dt);
    }
    fn layout(&mut self, _frame: Rect, env: &Env) {
        // full-screen root view: positions absolutely from PEEK_Y/GRID_TOP_Y by env.sp, so the
        // trait's frame rect (env.screen) is unused.
        let shelf_top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * env.sp; // 828 -> 150
        for r in 0..MAX_HUBS {
            self.shelves[r].base_y = shelf_top + r as f32 * ROW_PITCH - self.scroll_y.pos * env.sp;
        }
    }
    fn draw(&self, env: &Env, p: Painter) {
        let nh = n_hubs();
        // PASS 1 — every shelf's non-focused cells (the globally-focused cell is skipped in grid mode,
        // drawn LAST below so it overlaps neighbouring rows: cross-row z-order, invariant #3).
        for r in 0..nh {
            let row_y = self.shelves[r].base_y;
            if !on_axis(row_y, CARD_H, SCR_H, 0.0) {
                continue;
            }
            // hub title above the row — held clear of the magnified card so the focus glow never
            // washes over it (the shared CardRow heading-clearance rule: it rises once when a card
            // moves under it and STAYS up until none is, rather than tracking the pop)
            if env.sp > 0.02 {
                let lift = self.shelves[r].lift();
                // the snap fade rides the painter's cascade alpha, not the ink: `with_a` REPLACES a
                // token's alpha rather than multiplying it, and the separator token carries .45 of
                // its own — baking `env.sp` into the colour would throw that away.
                draw_heading(
                    p.alpha(env.sp),
                    crate::pms::hub_title(r),
                    crate::pms::hub_source(r),
                    MARGIN_X,
                    row_y - TITLE_DY - lift,
                );
            }
            for c in 0..crate::pms::hub_len(r) {
                if r == env.fr as usize && c == env.fc as usize && env.sp > 0.5 {
                    continue; // focused card drawn last (grid z-order)
                }
                let m = movie_at(r as c_int, c as c_int);
                let Some(mm) = m else { continue };
                let x = card_x(c, self.eff_scroll(r, env.sp));
                if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) {
                    continue;
                }
                let s = self.shelves[r].scale(c);
                let rect = Rect::new(x, row_y + CARD_DY, CARD_W, CARD_H).scaled(s);
                let resume = mm.resume_frac();
                card_row::draw_tile(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume);
            }
        }
        // PASS 2 — the single focused card + ring + metadata (title / "S1 • E8" / year), drawn
        // LAST for cross-row z-order (grid mode).
        self.draw_focused_cell(env, p);
    }
}

impl Grid {
    /// The focused cell alone — the grid's PASS 2, as its own call.
    ///
    /// A method rather than the tail of [`View::draw`] because it is drawn TWICE while a context
    /// menu is up: once in its own z-order, and once more over the modal scrim
    /// ([`redraw_focused_card`]), so the card the popover is about does not recede with the page
    /// behind it. One body, so the lifted copy is the drawn card and not a second expression of it.
    fn draw_focused_cell(&self, env: &Env, p: Painter) {
        if env.sp <= 0.5 {
            return;
        }
        let (r, c) = (env.fr as usize, env.fc as usize);
        if r >= n_hubs() {
            return;
        }
        // The focused card is the only one that can be pressed; fold the ui::press dip/bounce
        // factor into its scale (1.0 when idle, so a no-op unless a click is in flight).
        let s = self.shelves[r].scale(c.min(MAX_ITEMS - 1)) * crate::ui::press::scale();
        let x = card_x(c, self.eff_scroll(r, env.sp));
        let rect = Rect::new(x, self.shelves[r].base_y + CARD_DY, CARD_W, CARD_H).scaled(s);
        let m = movie_at(r as c_int, c as c_int);
        let cw = crate::pms::hub_is_continue(r); // Continue Watching: amber ▶ + "show · X min left"
        // keep the CStrings alive through the draw
        let label = m
            .map(|mm| {
                let mut l = if cw {
                    card_row::TileLabel::played(&mm.title)
                } else {
                    card_row::TileLabel::title(&mm.title)
                };
                l.caption = if cw { cw_caption(mm) } else { focused_caption(mm) };
                l
            })
            .unwrap_or_default();
        let resume = m.and_then(PmsMovie::resume_frac);
        card_row::draw_focused(p, Art::Poster(m), rect, s, &RowStyle::HOME, resume, &label);
    }
}

/// Pad either side of the `·` that introduces a SOURCE annotation, on the shared gap scale — the
/// same "hairline gap between two things that belong to one line" rung `detail::dotted_run` spends
/// on its own dots. One value, because this screen states a source in two places — the shelf
/// heading ([`heading_flow`]) and the hero's meta line ([`meta_source_flow`]) — and they are one
/// annotation in two positions, not two spacings.
const SOURCE_PAD: f32 = theme::space::XS;

/// The shelf heading, flowed left→right from the heading origin: the hub's title, and — only when
/// the row came from ANOTHER server — a quiet `· handle` naming that source ("Recently Added in
/// Film Club · friend").
///
/// **With an empty source the annotation is ABSENT, not empty**: no gap, no dot, no second run, no
/// draw call. That is the design's "with one source, none of this is drawn" implemented as absence
/// rather than as a branch that draws nothing visible — so the ANNOTATION costs a single-server
/// library nothing at all, in geometry, in draw calls and in ink.
///
/// The heading is NOT unchanged overall, and the difference is deliberate: its ink moved from
/// `TEXT_PRIMARY` (`#f7fafc`) to `TEXT_HEADING` (`#ebf0f7`) in the same pass. Every shelf heading on
/// Home is ~4% darker as a result, today, with no source string anywhere. That is a harmonization,
/// not a side effect — `TEXT_HEADING` is the shared section-heading ink that `detail.rs` and
/// `person.rs` already use, and Home was the one screen still inking its headings as body text.
///
/// Two runs and not one string because they are two SIZES, two weights and three inks, and
/// `Painter::text` can express exactly one of each per call (`theme::TEXT_SEPARATOR`'s doc: a dot
/// baked into a joined string is one run at one colour by construction). They are BASELINE-aligned
/// via `text::baseline_y` — a `BODY` run top-aligned against a `HEADLINE` one reads as a
/// superscript.
///
/// `run(text, dx, sz, bold, ink) -> advance` is the seam: the draw passes a closure that paints and
/// returns `Painter::text`'s advance, the host tests pass one that measures. The drawn flow and the
/// graded one are therefore ONE expression, not `dotted_run`/`dotted_run_w`'s two that have to be
/// kept agreeing. Returns the total advance.
///
/// The flow is PURE — it takes no font metric, which is also why the host suite can drive it: the
/// per-run baseline drop is resolved in [`draw_heading`], from the very `sz`/`bold` handed out here.
fn heading_flow(title: &str, source: &str, mut run: impl FnMut(&str, f32, c_int, c_int, [f32; 4]) -> f32) -> f32 {
    let mut dx = run(title, 0.0, theme::size::HEADLINE, 1, theme::TEXT_HEADING);
    if source.is_empty() {
        return dx; // one source: the heading is the title and nothing else
    }
    dx += SOURCE_PAD;
    dx += run("\u{b7}", dx, theme::size::BODY, 0, theme::TEXT_SEPARATOR);
    dx += SOURCE_PAD;
    dx += run(source, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY);
    dx
}

/// Draw [`heading_flow`] with its title's cap top at `(x, y)`. Every run rides the painter handed
/// in, so the snap fade and the row's heading `lift` move the whole heading together — an
/// annotation that detached from its title mid-scroll would read as two objects.
///
/// Each run drops onto the TITLE's baseline (`text::baseline_y`, a no-op for the title itself,
/// which is measured against its own tokens): the annotation is a rung down, and top-aligning a
/// `BODY` run against a `HEADLINE` one would read as a superscript. That one line is the only part
/// of this heading the host suite cannot grade — it opens no SDL_ttf, so there are no cap bands to
/// measure (the same boundary `widgets`' anchor table works within); what the tests DO pin is the
/// tokens it resolves from.
fn draw_heading(p: Painter, title: &str, source: &str, x: f32, y: f32) {
    heading_flow(title, source, |s, dx, sz, bold, ink| {
        // the CString must outlive the draw call, not the closure (`ui/CLAUDE.md`'s first gotcha)
        match CString::new(s) {
            Ok(cs) => p.text(cs.as_ptr(), x + dx, crate::text::baseline_y(sz, bold, theme::size::HEADLINE, 1, y), sz, ink, 0, bold),
            Err(_) => 0.0,
        }
    });
}

/// Metadata caption under the FOCUSED poster: episodes read "S1 • E8", movies their year — the
/// selected item's info lives with the selected poster instead of floating between the rows.
fn focused_caption(m: &PmsMovie) -> Option<CString> {
    let s = if m.kind == 3 && m.ep_index > 0 {
        if m.season_index > 0 {
            format!("S{} \u{2022} E{}", m.season_index, m.ep_index)
        } else {
            format!("E{}", m.ep_index)
        }
    } else if m.kind == 0 && m.year > 0 {
        m.year.to_string()
    } else {
        return None;
    };
    CString::new(s).ok()
}

/// The focused Continue-Watching card's secondary line (Home Screen.dc): an in-progress item reads
/// "<show> · 8 min left" (episodes) or just the time-remaining (a resumed movie); a next-up episode
/// (no resume point yet) reads "<show> · New episode". `title` above it carries the episode name.
///
/// "In progress" is [`PmsMovie::resume_frac`] — THE resume rule, and the same call `Grid::draw`
/// makes for the BAR in the pass that asks for this caption. A hand-written
/// `resume_ms > 0 && dur_ns > 0` here dropped that rule's end-guard, so a finished item whose
/// server never cleared `viewOffset` drew NO bar (`resume_frac`'s answer) under a caption still
/// promising "1 min left" — `fmt::time_left` floors at a minute, so it could not even read zero.
/// One item, described two ways in one draw. Whether that tile also wore the watched TICK is a
/// separate question with a separate answer: `widgets::poster_mark` reads `PmsMovie::watched`, so
/// a stale past-end `viewOffset` on an item the server has not marked watched drew no mark at all.
fn cw_caption(m: &PmsMovie) -> Option<CString> {
    let show = m.show_title.as_str();
    let s = if m.resume_frac().is_some() {
        let left = crate::ui::fmt::time_left(m.dur_ns / 1_000_000 - m.resume_ms);
        if m.kind == 3 && !show.is_empty() {
            format!("{show} \u{00b7} {left}")
        } else {
            left // a resumed movie: time-remaining alone
        }
    } else if m.kind == 3 {
        // next-up episode: no resume point, so no bar and no time — just the "New episode" cue
        if show.is_empty() {
            "New episode".to_string()
        } else {
            format!("{show} \u{00b7} New episode")
        }
    } else {
        return None;
    };
    CString::new(s).ok()
}
/// The drawn left edge of grid column `c` in a row whose EFFECTIVE horizontal scroll is `es`.
/// The one x formula the draw, the pointer hit-test and the vertical column-keeper all share —
/// hit_at/vert used to re-derive it WITHOUT the `* sp` snap fold the draw applies, so the hover
/// target disagreed with the drawn frame mid-snap (hero→grid).
#[inline]
fn card_x(c: usize, es: f32) -> f32 {
    MARGIN_X + c as f32 * (CARD_W + GAP) - es
}
/// Inverse of [`card_x`]: the column of `count` whose drawn span contains `mx` (hit_at's scan).
#[inline]
fn col_at(mx: f32, es: f32, count: usize) -> Option<usize> {
    (0..count).find(|&c| {
        let x = card_x(c, es);
        mx >= x && mx <= x + CARD_W
    })
}
impl Grid {
    fn new() -> Self {
        Grid { shelves: [CardRow::new(); MAX_HUBS], scroll_y: Spring::at(0.0) }
    }
    /// Row `r`'s DRAWN horizontal scroll: the shelf spring folded by the hero→grid snap `sp`
    /// (the hero view shows rows unscrolled; the fold is how draw has always applied it).
    #[inline]
    fn eff_scroll(&self, r: usize, sp: f32) -> f32 {
        self.shelves[r].scroll_x() * sp
    }
    // ---- navigation: writes the fr/fc globals (never caches focus) ----
    fn nav(&self, sym: c_uint, sp: f32) {
        unsafe {
            let nh = n_hubs() as c_int;
            let nc = crate::pms::hub_len(fr.max(0) as usize) as c_int;
            if sym == SDLK_LEFT && fc > 0 {
                fc -= 1;
            } else if sym == SDLK_RIGHT && fc < nc - 1 {
                fc += 1;
            } else if sym == SDLK_UP && fr > 0 {
                self.vert(-1, sp);
            } else if sym == SDLK_DOWN && fr < nh - 1 {
                self.vert(1, sp);
            }
        }
    }
    /// vertical move keeping VISUAL column alignment across rows' animated scroll
    unsafe fn vert(&self, dir: c_int, sp: f32) {
        // Both indices must be clamped to the addressable rows: `fr` is a raw global that a
        // stale write (or a server with >MAX_HUBS hubs) can push past the end of `shelves`,
        // and this runs on the KEYPRESS — it panicked before `draw` was ever reached.
        let n = n_hubs();
        let cur = step_row(g_fr(), 0, n);
        let ncur = step_row(g_fr(), dir, n);
        let cx = card_x(g_fc().max(0) as usize, self.eff_scroll(cur as usize, sp)) + CARD_W * 0.5;
        let mut nc =
            ((cx - MARGIN_X - CARD_W * 0.5 + self.eff_scroll(ncur as usize, sp)) / (CARD_W + GAP) + 0.5) as c_int;
        let ncount = crate::pms::hub_len(ncur as usize) as c_int;
        nc = nc.clamp(0, (ncount - 1).max(0));
        fr = ncur;
        fc = nc;
    }
    /// Card under the pointer, or None. Vertical fly-away guard: a row that is only PARTIALLY on
    /// screen is not hoverable unless it is already the focused row — hovering it would move `fr`
    /// and the page spring would chase it (vertical auto-scroll), which is exactly the "pointer
    /// flies away" the pointer rules ban; horizontal scroll-into-view within a row is kept.
    fn hit_at(&self, mx: f32, my: f32, sp: f32) -> Option<(usize, usize)> {
        for r in 0..n_hubs() {
            let row_y = self.shelves[r].base_y + CARD_DY;
            if my < row_y || my > row_y + CARD_H {
                continue;
            }
            let fully_visible = row_y >= 40.0 && row_y + CARD_H <= SCR_H - 20.0;
            if !fully_visible && r != g_fr() as usize {
                continue;
            }
            if let Some(c) = col_at(mx, self.eff_scroll(r, sp), crate::pms::hub_len(r)) {
                return Some((r, c));
            }
        }
        None
    }
    /// hover/click focus write: focus the card under the pointer; reports whether one was hit
    fn hit_test(&self, mx: f32, my: f32, sp: f32) -> bool {
        if let Some((r, c)) = self.hit_at(mx, my, sp) {
            unsafe {
                fr = r as c_int;
                fc = c as c_int;
            }
            return true;
        }
        false
    }
    fn wheel(&self, dy: c_int) {
        unsafe {
            let nh = n_hubs() as c_int;
            if dy < 0 && fr < nh - 1 {
                fr += 1;
            } else if dy > 0 && fr > 0 {
                fr -= 1;
            }
            let nc = crate::pms::hub_len(fr.max(0) as usize) as c_int;
            if fc >= nc {
                fc = (nc - 1).max(0);
            }
        }
    }
}

// ---- Home root + the C ABI ----
struct Home {
    snap: Spring,
    bg: Backdrop,
    hero: Hero,
    grid: Grid,
}
impl Home {
    fn new() -> Self {
        Home { snap: Spring::at(0.0), bg: Backdrop::new(), hero: Hero::new(), grid: Grid::new() }
    }
    fn env(&self, dt: f32) -> Env {
        let sp = self.snap.pos;
        // clamp the focus into the current hub bounds so a stray write degrades to a
        // valid shelves[fr]/cards[fc] index rather than reading out of range.
        let nh = n_hubs().max(1) as c_int;
        let cfr = g_fr().clamp(0, nh - 1);
        let ncols = crate::pms::hub_len(cfr as usize).max(1) as c_int;
        let cfc = g_fc().clamp(0, (ncols - 1).min(MAX_ITEMS as c_int - 1));
        Env { dt, screen: Rect::FULL, fr: cfr, fc: cfc, sp, hero_a: hero_alpha(sp, 0.55) }
    }
}
impl View for Home {
    // Compose the tree: flat backdrop, the hero group faded as one under p.alpha(hero_a), then the
    // grid. The grid's layout (base_y, &mut) is done by the home_draw wrapper just before this, and
    // snap-step + grid.update happen in home_update — the wrapper owns those because building `env`
    // depends on the freshly-stepped snap, which View::update/draw can't sequence.
    fn draw(&self, env: &Env, p: Painter) {
        use crate::ui::profile::phase;
        phase("hm.backdrop", || self.bg.draw(env, p));
        if env.hero_a > 0.01 {
            phase("hm.hero", || self.hero.draw(env, p.alpha(env.hero_a)));
        }
        phase("hm.grid", || self.grid.draw(env, p));
    }
}

static mut SCENE: Option<Home> = None;
#[inline]
fn scene() -> &'static mut Home {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().expect("home_init not called") }
}

pub(crate) fn home_init() {
    guard(|| unsafe { *addr_of_mut!(SCENE) = Some(Home::new()) });
}

pub(crate) fn home_update(dt: f32) {
    guard(|| {
        let h = scene();
        // The hub-fetch state machine: land an off-thread refetch and count down to the next
        // automatic retry. It ticks HERE because Home is the only screen that shows the result —
        // so nothing spawns a background fetch behind the player, and a Home that came up empty
        // heals itself the moment the server answers again.
        crate::pms::pump(dt);
        // The LIVE half of [`pinned_snap`]: a catalog can empty UNDER a committed snap (a profile
        // switch whose fetch fails does exactly that), and a filter on new writes cannot undo one
        // already made. Re-applied every frame, so every snap reader falls into line for free.
        if n_hubs() == 0 {
            unsafe { addr_of_mut!(snapTarget).write(0.0) };
            h.snap.jump(0.0); // no slide: there is nothing on either side of it to animate
        }
        // spinner phase, wrapped on a whole Spinner period so it stays exact (a bare f32
        // accumulator loses sub-frame resolution after a few days parked on this screen — and a
        // screen that sits here for days is precisely the failure this read-out is for)
        unsafe {
            let ms = addr_of!(status_ms).read() + dt * 1000.0;
            addr_of_mut!(status_ms).write(ms % (crate::ui::widgets::Spinner::PERIOD_MS as f32 * 1000.0));
        };
        unsafe {
            let cd = addr_of!(hero_flip_cd).read();
            if cd > 0.0 {
                addr_of_mut!(hero_flip_cd).write((cd - dt).max(0.0));
            }
            // slide transition: step the progress spring, drop the outgoing layer once the travel
            // left is sub-pixel (see HERO_SLIDE_REST_PX) — and land the spring exactly on 1.0 so
            // the retiring frame and the first idle frame draw the same pixels.
            if addr_of!(hero_prev).read() >= 0 {
                let sl = &mut *addr_of_mut!(hero_slide);
                sl.step(1.0, K_SLIDE, dt);
                if (1.0 - sl.pos).abs() * SCR_W < HERO_SLIDE_REST_PX {
                    sl.jump(1.0);
                    addr_of_mut!(hero_prev).write(-1);
                }
            }
            // idle auto-flip: only while the billboard is actually showing; any flip (manual or
            // this one) resets the countdown inside hero_flip.
            if h.snap.pos < 0.05 && crate::pms::hero_pool_len() > 1 {
                let a = addr_of!(hero_auto).read() - dt;
                addr_of_mut!(hero_auto).write(a);
                if a <= 0.0 {
                    hero_flip(1);
                }
            } else {
                addr_of_mut!(hero_auto).write(HERO_AUTO_S);
            }
        }
        let target = unsafe { addr_of!(snapTarget).read() };
        h.snap.step(target, K_SNAP, dt);
        // the shared top bar's own motion — the strip's horizontal scroll (a server with more
        // libraries than fit the row reaches the rest by scrolling the focused pill into view),
        // its travelling capsules, and the profile chip's unfurl. Home is always this screen's
        // selected tab, so off the band the strip returns to the start.
        crate::ui::widgets::tab_row_update(0, top_focus(), dt);
        let env = h.env(dt);
        // The backdrop's own springs (the wash dissolve + the two art reveals) — and, inside it, the
        // frame's ONE resolve of each hero layer's texture. It runs AFTER the auto-flip and the slide
        // step above, because its re-seat keys off the hero index having already moved.
        h.bg.update(&env);
        h.grid.update(&env);
        // Last, deliberately: the prefetch gate wants a store this frame has already finished
        // disturbing, so it never issues a warm ahead of a texture something on screen is waiting on.
        prefetch_hero_neighbours();
    });
}

pub(crate) fn home_draw() {
    guard(|| {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let h = scene();
        let env = h.env(0.0);
        h.grid.layout(env.screen, &env); // &mut layout before the &self composite draw
        // THE page transition, as ONE cascade-alpha push at the root of the tree: the backdrop,
        // hero, grid and read-out are what a route change REPLACES, so they dip to the app ground
        // and back (`ui::nav`). Nothing per-element is needed — `Painter::alpha` is multiplicative
        // and folded into every primitive by `Painter::c`, `Painter::ambient` included since it
        // learned to mix toward `SURFACE_APP`, which is the same colour `frame_clear` just laid
        // down. The shared top band below is deliberately NOT on it.
        let pc = Painter::root().alpha(crate::ui::nav::page_alpha());
        h.draw(&env, pc);
        // loading / empty / error, only when there are no shelves. In the CONTENT layer (it stands
        // in for the hero and grid above it), so the top chrome below still paints over it.
        draw_status(&env, pc);
        // …and, when armed, the SYNTHETIC ground replaces all of it: drawn here so it is what the
        // bar samples and what the backdrop blur sources, and on the page's own alpha so a route
        // change still dips it. `testpat` is a no-op unless a spec is set.
        crate::ui::testpat::underlay(pc);
        crate::ui::testpat::draw(pc);
        // the centered library tab pills (shared with the Library screen): Home is the selected
        // tab here and a pill holds focus when the hero top band is on it, both of which the row's
        // own travelling capsules already carry — `home_update` hands them down every frame.
        //
        // CONTINUOUS CHROME: this band is the SAME control the Library draws, so it holds still
        // while the pages swap under it (`nav::chrome_alpha`). Fading it with the page would make a
        // tab press read as the whole screen blinking — and the capsule travelling across a bar
        // that stays put IS the transition the tab bar is supposed to show.
        let pk = Painter::root().alpha(crate::ui::nav::chrome_alpha());
        crate::ui::widgets::draw_tab_row(pk);
        // the chip goes on TOP of the tab track, not under it. Its focused capsule unfurls the
        // profile name to its right, and with enough libraries the (centered) track now reaches
        // far enough left to meet it — under the track, the name was painted over by the track's
        // own scrim. Both are the same dark material, so an overlap reads as one band.
        crate::ui::widgets::profile_chip(pk);
    });
}

/// **Where this screen's focus is on the SHARED top bar** — the one question `widgets` asks of every
/// screen that wears it, so the chip's unfurl and the strip's capsules are animated in one place
/// (`widgets::tab_row_update`) rather than three.
///
/// Gated on the SNAP as a whole: the top band is only reachable while the billboard is showing, and
/// diving to the grid always passes back through the action row (`app.rs`'s DOWN arm sends a
/// negative `hero_fc` to 0 rather than to the grid), so `hero_fc` is never a band value down there.
/// Stating the gate once here is what makes that an invariant rather than something the pill half
/// happens to satisfy — it used to hold for the chip and be left implicit for the pills.
pub(crate) fn top_focus() -> crate::ui::widgets::TopFocus {
    use crate::ui::widgets::TopFocus;
    if snap_pos() >= 0.5 {
        return TopFocus::Away;
    }
    match hero_focus() {
        -1 => TopFocus::Chip,
        f => hero_pill_index(f).map(TopFocus::Pill).unwrap_or(TopFocus::Away),
    }
}

// ---- the loading / empty / error read-out ----------------------------------------------------
// Home used to draw the SAME bare dark screen whether the hub fetch was still running, the server
// was unreachable, or the library really was empty — and nothing ever tried again, so a wifi
// hiccup at boot meant a dead screen until the app was restarted. The three states are now told
// apart (`pms::HubState`) and the screen is always recoverable: an automatic retry on a backoff,
// plus a control for the person who doesn't want to wait for it.

/// ms clock for the status spinner. The screen owns its own phase the way `library.rs` does —
/// `home_draw` runs at dt=0, so only `home_update` can advance one.
static mut status_ms: f32 = 0.0;

/// Home's "there are no shelves" read-out: caption, treatment, and the label of the control under
/// it (`None` = no control — while a fetch is in flight the spinner IS the state, and the fetch a
/// Retry would kick is the one already running).
///
/// `None` overall = there IS content, so nothing is drawn over it. A background refresh that fails
/// behind a populated Home stays silent and retries underneath rather than throwing an error over
/// shelves the user can still use — the same rule `browse.rs` keeps for its paged grid.
fn status_read() -> Option<(&'static std::ffi::CStr, crate::ui::widgets::StatusKind, Option<&'static std::ffi::CStr>)> {
    use crate::ui::widgets::StatusKind;
    if crate::pms::hub_count() > 0 {
        return None;
    }
    Some(match crate::pms::hub_state() {
        crate::pms::HubState::Loading => (c"Loading your library\u{2026}", StatusKind::Working, None),
        crate::pms::HubState::Failed => (c"Can't reach your Plex server", StatusKind::Failed, Some(c"Try Again")),
        crate::pms::HubState::Ready => (c"Nothing on this server yet", StatusKind::Empty, Some(c"Refresh")),
    })
}

/// Draw the read-out over an EMPTY Home: the shared `StatusOverlay` (spinner + caption, or a
/// tinted caption alone) with its one control below. Deliberately independent of `hero_a` and of
/// the snap — with no shelves there is nothing for the grid to show, so the message must not fade
/// out as the snap drifts.
///
/// The Retry control is the COMPONENT's now, not this screen's. It was hand-built here — a
/// `Button::pill_w` and a y measured from the panel centre — while the caption above it came from
/// the shared read-out, so the two halves of one block were laid out by two different pieces of
/// code and only agreed by inspection. The Library's section read-out draws the same pair, and two
/// hand-rolled copies of it is exactly the drift `ui/CLAUDE.md`'s fourth rule exists to stop.
fn draw_status(env: &Env, p: Painter) {
    let Some((caption, kind, action)) = status_read() else {
        return; // there is content: the hero owns the screen, and its own hit targets
    };
    let mut ov = crate::ui::widgets::StatusOverlay::new(Rect::FULL, caption, kind)
        .phase(unsafe { addr_of!(status_ms).read() } as u32)
        // Focus is `>= 0` rather than `== 0` because this row has exactly one button — LEFT/RIGHT
        // must not be able to park focus on nothing.
        .focused(hero_focus() >= 0);
    if let Some(label) = action {
        ov = ov.action(label);
    }
    ov.draw(env, p);
    // The status screen OWNS the hero action row's focus slot AND its pointer hit targets: the two
    // can never be on screen together (a status state means an empty catalog, which means
    // `hero_item()` is None and `Hero::draw` returns before recording anything), so the existing
    // click path drives Retry with no second hit-test. The rect comes from the component that drew
    // the pill, so a click can never land somewhere the button isn't.
    // parked OFF the panel rather than at the origin: `Rect::contains` is inclusive, so a
    // zero-size rect at (0,0) would "contain" a click at exactly (0,0)
    let none = Rect::new(-1.0, -1.0, 0.0, 0.0);
    let btn = ov.action_frame().unwrap_or(none);
    unsafe { addr_of_mut!(hero_btns).write([btn, none]) };
}

/// Does the status screen own this activation? Retry is Home's ONLY control while a read-out is
/// up, so it takes everything except the top band — the profile chip and the library tab pills,
/// the two escapes that still work with no shelves. Split from [`status_activate`] so the rule is
/// testable without kicking a real fetch.
///
/// The chip clause is belt-and-braces since the chip became the shared bar's control: `app.rs`
/// answers `TopFocus::Chip` before this screen's activation is reached at all. It stays because it
/// is the RULE — "the top band is not the read-out's" — and the read-out is what the rule is about.
fn status_takes(hf: c_int) -> bool {
    status_read().is_some() && hf != -1 && hero_pill_index(hf).is_none()
}

/// OK (or a click) while a status read-out is up: kick the fetch again. Returns whether it handled
/// the press; app.rs's ONE home activation asks this first.
pub(crate) fn status_activate(hf: c_int) -> bool {
    if !status_takes(hf) {
        return false;
    }
    crate::pms::request_retry();
    true
}

pub(crate) fn home_move_focus(sym: c_uint) {
    guard(|| {
        let s = scene();
        let sp = s.snap.pos; // read once, passed down — hit/vert must see the DRAWN scroll
        s.grid.nav(sym, sp);
    });
}

/// Hero-view horizontal key: LEFT/RIGHT walk the action-row focus (pill → info); RIGHT on the LAST
/// control pages the billboard forward and LEFT on the pill (the row's left end) pages it BACK — so
/// holding either edge key keeps paging (the debounced `hero_flip` throttles it). **This is the
/// whole of paging by hand**, now that the chevron is an indicator rather than a button: the row's
/// two ends are the pager, and `hero_auto` is the third way the billboard flips. app.rs calls this
/// only while the snap is in hero view; non-arrow keys are no-ops. The chip (focus -1) has no
/// horizontal neighbours.
pub(crate) fn home_hero_key(sym: c_uint) {
    let f = hero_focus();
    match sym {
        SDLK_RIGHT if f >= 0 => {
            if f < HERO_NBTN as c_int - 1 {
                set_hero_focus(f + 1);
            } else {
                hero_flip(1);
            }
        }
        // top band (chip + library pills): RIGHT walks chip → Movies → TV Shows (more negative =
        // further right per the -(i+1) encoding); LEFT walks back to the chip.
        SDLK_RIGHT if f < 0 => set_hero_focus(f - 1), // set_hero_focus clamps at the last pill
        SDLK_LEFT if f < -1 => set_hero_focus(f + 1),
        SDLK_LEFT if f > 0 => set_hero_focus(f - 1),
        SDLK_LEFT if f == 0 => hero_flip(-1),
        _ => {}
    }
}

pub(crate) fn home_pointer_focus(mx: f32, my: f32) {
    guard(|| {
        let s = scene();
        let sp = s.snap.pos;
        s.grid.hit_test(mx, my, sp);
    });
}

/// Pointer click on a grid card: focus it and report the hit, so app.rs can run the SAME
/// activation as OK (play / open detail). Uses hit_at's visibility rules — a click on a
/// half-visible row is ignored rather than scrolling the page.
pub(crate) fn home_card_click(mx: f32, my: f32) -> bool {
    let mut hit = false;
    guard(|| {
        let s = scene();
        let sp = s.snap.pos;
        hit = s.grid.hit_test(mx, my, sp);
    });
    hit
}

pub(crate) fn home_wheel(dy: c_int) {
    guard(|| scene().grid.wheel(dy));
}

/// The focused grid card's rect at its **focus magnification** (screen coords), or None when the
/// scene has not been built or the focus points outside the live hubs. The item-menu popover anchors
/// beside this, off the SAME `card_x`/`eff_scroll`/`base_y`/`scale(c)` the draw and the pointer
/// hit-test use — a panel placed off a re-derived geometry would drift off its card mid-scroll,
/// which is the bug `card_x`'s doc comment already records for the hit-test.
///
/// It deliberately **omits `press::scale()`**, the one factor the draw also multiplies in
/// (`Grid::draw`). The only caller opens on a press that is at full dip at that instant and is
/// cancelled in the same breath, so folding the dip in would anchor the panel off a transient ~8%
/// shrink and then leave it there while the card springs back out from under it. The rest-size
/// magnified rect is where the card actually settles.
///
/// Note it is the GRID rect: the hero view has no card, so the caller (a press-and-hold, which only
/// arms on a grid card) is what keeps this meaningful.
pub(crate) fn focused_card_rect() -> Option<Rect> {
    let h = unsafe { (*addr_of!(SCENE)).as_ref()? };
    let r = row().max(0) as usize;
    let c = col().max(0) as usize;
    if r >= n_hubs() || c >= crate::pms::hub_len(r) {
        return None;
    }
    let sp = h.snap.pos;
    let base = Rect::new(card_x(c, h.grid.eff_scroll(r, sp)), h.grid.shelves[r].base_y + CARD_DY, CARD_W, CARD_H);
    Some(base.scaled(h.grid.shelves[r].scale(c.min(MAX_ITEMS - 1))))
}

/// Re-draw the focused grid card ON TOP of a modal scrim — this screen's half of
/// [`crate::ui::popover::Opener`], handed to the item context menu when it opens over Home.
///
/// It runs the SAME `Grid::draw_focused_cell` the page just ran, on a fresh root painter carrying
/// the same page alpha, so the lifted copy is the drawn card rather than a second expression of it.
/// No layout: `home_draw` already stepped `base_y` for this frame, and re-running it here would want
/// a `&mut` the draw phase must not take.
pub(crate) fn redraw_focused_card() {
    guard(|| {
        let h = scene();
        let env = h.env(0.0);
        let p = Painter::root().alpha(crate::ui::nav::page_alpha());
        // **Cut at the chrome.** In the page's own order the grid is drawn BEFORE the top band, so a
        // card scrolling up through it slides UNDER the tab row; a lift drawn after everything would
        // invert that for the one card the menu is about. The focused row is kept well below this
        // line by the reveal rule, so today the scissor cuts nothing — it is what keeps the z-order
        // rule true rather than merely unbroken. Set and cleared in one breath, as `Painter::clip`'s
        // global GL state requires.
        let cut = crate::ui::widgets::TOP_BAR_Y + crate::ui::widgets::TAB_PILL_H;
        p.clip(Rect::new(0.0, cut, SCR_W, SCR_H - cut));
        h.grid.draw_focused_cell(&env, p);
        p.clip_clear();
    });
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The home screen's focus lives in `static mut fr`/`fc`, so tests that drive it must not
    /// run concurrently. (This is the cost the audit flagged: module-singleton state makes host
    /// tests order-dependent. One lock is cheaper than reshaping the screen right now.)
    static FOCUS: Mutex<()> = Mutex::new(());

    #[test]
    fn n_hubs_clamps_the_server_count_to_the_shelf_array() {
        assert_eq!(n_hubs_of(0), 0);
        assert_eq!(n_hubs_of(3), 3);
        assert_eq!(n_hubs_of(MAX_HUBS), MAX_HUBS);
        // A server with 3-4 libraries returns well over 16 hubs (/hubs promotes several rows
        // per library on top of continue/ondeck/recentlyAdded). `shelves` is a fixed [_; 16].
        assert_eq!(n_hubs_of(MAX_HUBS + 1), MAX_HUBS);
        assert_eq!(n_hubs_of(200), MAX_HUBS);
    }

    /// Regression: the top band packed pill `i` as `-(i+1)`, so Home (pill 0) landed on -1 — the
    /// profile chip's value. `hero_pill_index` rejects anything above -2, so the Home pill was
    /// unreachable by D-pad and could never wear the focused (white) treatment on its own screen.
    /// The packing now round-trips for EVERY pill the row can draw, Home included — and the row
    /// is no longer capped at 5, so the sweep runs well past the old bound.
    #[test]
    fn every_tab_pill_round_trips_through_the_focus_packing() {
        for i in 0..32usize {
            let f = hero_focus_for_pill(i);
            assert_ne!(f, -1, "pill {i} must not alias the profile chip");
            assert_eq!(hero_pill_index(f), Some(i), "pill {i} must decode back to itself");
        }
        assert_eq!(hero_pill_index(-1), None, "the profile chip is not a pill");
        assert_eq!(hero_pill_index(0), None, "the hero Play pill is not a tab pill");
        assert_eq!(hero_pill_index(c_int::MIN), None, "the grid-card sentinel is not a pill");
    }

    /// Regression (unit 10): the top band clamped focus at `MAX_TABS - 1` = 4 sections, so a
    /// server with a fifth `movie`/`show` library had no way at all to reach it — `browse`
    /// discovered it, the grid browsed it, and RIGHT simply stopped. Walking RIGHT from the Home
    /// pill must now land on the LAST pill for any section count, and never past it.
    #[test]
    fn top_band_focus_walks_to_the_last_section_whatever_the_count() {
        for npill in 1..=8usize {
            let lo = hero_focus_lo_for(npill);
            // RIGHT in the top band is `set_hero_focus(f - 1)` (more negative = further right)
            let mut f = hero_focus_for_pill(0);
            for _ in 0..npill + 4 {
                f = (f - 1).clamp(lo, HERO_NBTN as c_int - 1);
                assert!(
                    hero_pill_index(f).map(|i| i < npill).unwrap_or(false),
                    "walking RIGHT with {npill} pills left focus on {f}, which is not a drawable pill"
                );
            }
            assert_eq!(
                hero_pill_index(f),
                Some(npill - 1),
                "RIGHT must reach the last of {npill} pills"
            );
            // and LEFT walks back through every pill to Home, then off the pills onto the chip
            for _ in 0..npill - 1 {
                f = (f + 1).clamp(lo, HERO_NBTN as c_int - 1);
            }
            assert_eq!(hero_pill_index(f), Some(0), "LEFT must return to the Home pill");
            f = (f + 1).clamp(lo, HERO_NBTN as c_int - 1);
            assert_eq!(f, -1, "one more LEFT leaves the pills for the profile chip");
        }
    }

    /// …and the clamp is actually WIRED to the pill count. The walk above drives the pure helper;
    /// this drives the real setter, which is the line the cap used to live on. The host has no
    /// section table (`browse::reset` — the only writer any test reaches — always leaves it
    /// empty), so the row here is Home and Search: the two pills that exist whatever the account
    /// turns out to hold. The band's last pill is therefore SEARCH, and this asserts the clamp
    /// lands on it rather than on the last library — which on a fresh boot there is none of.
    #[test]
    fn set_hero_focus_clamps_onto_the_last_drawable_pill() {
        let _s = crate::testlock::serial(); // the count comes from `browse`'s table, a crate global
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let saved = hero_focus();
        crate::browse::reset();
        assert_eq!(crate::ui::widgets::tab_count(), 2, "no sections discovered: Home and Search");
        set_hero_focus(c_int::MIN / 2);
        let last = crate::ui::widgets::search_pill();
        assert_eq!(hero_focus(), hero_focus_for_pill(last), "past the last pill clamps onto it");
        assert_eq!(hero_pill_index(hero_focus()), Some(last));
        set_hero_focus(999);
        assert_eq!(hero_focus(), HERO_NBTN as c_int - 1, "past the action row clamps to its end");
        set_hero_focus(saved);
    }

    /// **The pager chevron is an INDICATOR: not a focus stop, and the row's END is what pages.**
    ///
    /// The owner's correction (2026-08-21): "Chevron should not be even selectable. It just shows
    /// that the page flips." It used to be `hero_fc == 2` — a third stop in this row, activatable
    /// by OK and by a click, first as a disc and then as a bare mark. The correction is about the
    /// CONTROL, not the drawing, and its two halves are invisible from either side alone: a focus
    /// that still reached a third index would put the ACCENT face on a thing nothing can do, while
    /// a RIGHT that merely stopped at the last control would leave the billboard with no forward
    /// pager at all on the D-pad — the chevron *was* the forward one.
    ///
    /// So both are pinned here, against the real key path: no number of RIGHT presses reaches an
    /// index past the info disc, and the press that would have gone there flips the billboard
    /// instead while focus stays put. (LEFT off the pill is the same statement at the other end.)
    ///
    /// **The COUNT is pinned as a literal, and that is not belt-and-braces.** "The row's last
    /// control pages" is true of a row of ANY length, so a version of this test phrased entirely in
    /// `HERO_NBTN` grades the key path and nothing about the correction: with `HERO_NBTN` back at 3
    /// and the chevron recording a hit rect again, the walk simply rests on 2, the edge key still
    /// pages, and every assertion below still passes — the whole suite stayed green under exactly
    /// that patch, which is how this test was first written. A test that moves WITH the number it
    /// is about cannot see the number.
    #[test]
    fn the_pager_is_not_a_focus_stop_and_the_rows_end_pages_instead() {
        let _s = crate::testlock::serial(); // pms's hero pool + browse's pill table
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let saved = hero_focus();
        crate::pms::seed_for_test(3, crate::pms::HubState::Ready);
        assert!(crate::pms::hero_pool_len() > 1, "a pool with somewhere to page to");

        // (0) **the COUNT, as literals.** Everything below is about "the row's last control", which
        // is true of a row of any length — phrased in `HERO_NBTN` the whole test passes unchanged
        // with the chevron back as stop 2 and a hit rect of its own (measured, suite green under
        // exactly that patch). Two is the correction, so two is written down. `hero_btns` is
        // `[Rect; HERO_NBTN]`, so this pins the pointer half as well — there is no third slot for a
        // hit target to be recorded into, whatever a future `hero_actions` tries to write.
        const LAST_CTRL: c_int = 1; // the info disc — the row's right-hand end
        assert_eq!(HERO_NBTN, 2, "the hero row holds the Play pill and the info disc, and nothing else");
        assert_eq!(unsafe { addr_of!(hero_btns).read() }.len(), 2, "…and so does the hit-target array");

        // (1) the chevron is unreachable: RIGHT walks pill → info and then stops walking
        set_hero_focus(0);
        for press in 1..=8 {
            home_hero_key(SDLK_RIGHT);
            assert!(
                hero_focus() <= LAST_CTRL,
                "RIGHT #{press} left focus on {} — past the row's last CONTROL",
                hero_focus()
            );
        }
        assert_eq!(hero_focus(), LAST_CTRL, "…and it rests on the info disc");

        // (2) …because that press pages the billboard instead. The cooldown is cleared first
        // because the walk above SPENT it: press #2 already landed on the row's end and flipped
        // once, arming `hero_flip`'s debounce, and presses #3-8 were swallowed by it. Without this
        // line the assertion below would be grading the cooldown rather than the edge key.
        unsafe { addr_of_mut!(hero_flip_cd).write(0.0) };
        let before = hero_index();
        home_hero_key(SDLK_RIGHT);
        assert_ne!(hero_index(), before, "RIGHT at the row's end must page the billboard");
        assert_eq!(hero_focus(), LAST_CTRL, "…and must not move focus doing it");

        // (3) the other end, unchanged: LEFT on the pill pages back
        set_hero_focus(0);
        unsafe { addr_of_mut!(hero_flip_cd).write(0.0) };
        let before = hero_index();
        home_hero_key(SDLK_LEFT);
        assert_ne!(hero_index(), before, "LEFT on the pill pages back");
        assert_eq!(hero_focus(), 0, "…and stays on the pill");

        set_hero_focus(saved);
        unsafe { addr_of_mut!(hero_flip_cd).write(0.0) };
        crate::pms::reset();
    }

    /// **What Home reports to the SHARED top bar** — the one value `widgets::tab_row_update`
    /// animates the chip's unfurl and the strip's capsules from, and the one `app.rs` routes OK on.
    ///
    /// Two things it must get right, and they used to be two different rules on one screen. The
    /// chip is `hero_fc == -1` and the pills are the packed negatives, so a single `match` covers
    /// the band; and the whole band is gated on the SNAP, because the top band is only reachable
    /// while the billboard is showing. The gate used to be spelled only for the chip
    /// (`chip_focused`) and left implicit for the pills — true by construction, but true because
    /// nothing happened to violate it rather than because anything said so.
    #[test]
    fn the_top_band_reports_the_chip_and_the_pills_as_one_answer() {
        use crate::ui::widgets::TopFocus;
        let _s = crate::testlock::serial(); // `hero_pill_index`'s clamp reads `browse`'s table
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let (saved, snap0) = (hero_focus(), snap_target());
        set_snap_target(0.0); // the billboard is showing

        set_hero_focus(-1);
        assert_eq!(top_focus(), TopFocus::Chip, "-1 IS the profile chip");
        set_hero_focus(hero_focus_for_pill(0));
        assert_eq!(top_focus(), TopFocus::Pill(0), "…and the packed negatives are the pills");
        // LEFT off the Home pill is the chip: the same walk the Library and Search adopted, and
        // the reason it is the same walk is that it is the only one the geometry admits.
        home_hero_key(SDLK_LEFT);
        assert_eq!(top_focus(), TopFocus::Chip, "◀ off the first pill reaches the chip");
        home_hero_key(SDLK_RIGHT);
        assert_eq!(top_focus(), TopFocus::Pill(0), "▶ walks back onto the pill it left");

        for f in 0..HERO_NBTN as c_int {
            set_hero_focus(f);
            assert_eq!(top_focus(), TopFocus::Away, "the action row is not the bar");
        }
        set_hero_focus(saved);
        set_snap_target(snap0);
    }

    /// The top band walks the PILLS the strip's projection produces, not the section table — with
    /// several sources those are different lengths. FOUR libraries across two servers project to
    /// two pills (both servers provide both types), so RIGHT stops past them and never on a
    /// library-shaped pill that nothing would draw. The gap between the two counts is the whole
    /// point; it used to be 5→3 because the fixture's fifth library was a music one, and music is
    /// no longer a type this product has a level for.
    ///
    /// The row is Home + those two + Search, and the last stop is the SEARCH pill — which is the
    /// other half of why the two counts must not be conflated: one end of the row is not a library
    /// either.
    #[test]
    fn the_top_band_walks_the_projected_pills_not_the_section_table() {
        let _s = crate::testlock::serial();
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let saved = hero_focus();
        crate::browse::seed_two_source_table_for_test();
        assert_eq!(crate::browse::section_count(), 4, "four libraries…");
        assert_eq!(crate::ui::widgets::tab_count(), 4, "…and Home + two pills + Search");

        set_hero_focus(c_int::MIN / 2); // walk RIGHT past the end of the row
        assert_eq!(hero_pill_index(hero_focus()), Some(3), "the last pill is the last PILL, not the last library");
        assert!(crate::ui::widgets::is_search_pill(3), "…and that pill is Search, not a section");
        set_hero_focus(saved);
        crate::browse::reset();
    }

    #[test]
    fn step_row_stays_inside_the_addressable_rows() {
        assert_eq!(step_row(0, 1, 5), 1);
        assert_eq!(step_row(4, 1, 5), 4, "DOWN on the last row stays put");
        assert_eq!(step_row(0, -1, 5), 0, "UP on the first row stays put");
        assert_eq!(step_row(9, 1, 5), 4, "a stale focus is pulled back into range");
        assert_eq!(step_row(0, 1, 0), 0, "an empty catalog has no row to move to");
    }

    /// Regression: with more hubs than the fixed `shelves: [CardRow; 16]` array, a DOWN press on
    /// row 15 computed `g_fr() + dir == 16` and indexed the array out of bounds — panicking on
    /// the KEYPRESS, before `draw` was ever reached. `Home::env` clamps the value it *returns*
    /// but never writes it back, so the raw global kept climbing.
    #[test]
    fn vert_cannot_walk_past_the_shelf_array() {
        // Two locks, and both are load-bearing: FOCUS for `static mut fr`/`fc`, and the crate-wide
        // serial lock because this READS `pms`'s catalog statics (`n_hubs`/`hub_len`) which other
        // modules' tests now WRITE — `commit` frees the old `HUBS` buffer, so an unsynchronized
        // read here is a use-after-free, not a flaky assertion. Every writer takes `serial` only,
        // so this pair can never be acquired in the opposite order.
        let _s = crate::testlock::serial();
        let _g = FOCUS.lock().unwrap_or_else(|e| e.into_inner());
        let grid = Grid::new();
        set_row(MAX_HUBS as c_int - 1); // 15 — the last shelf the array can hold
        unsafe { grid.vert(1, 1.0) };
        assert!(
            row() < MAX_HUBS as c_int,
            "vert() walked to row {} with only {MAX_HUBS} shelves",
            row()
        );
        set_row(0);
    }

    /// The whole point of the status read-out: the three "no shelves" cases must be TOLD APART.
    /// One bare dark screen used to stand in for all of them — still fetching, server unreachable,
    /// and library genuinely empty — and content must silence it entirely, because a refresh that
    /// fails behind populated shelves is not worth covering them with.
    ///
    /// Takes the crate-wide serial lock (not this module's `FOCUS`): the catalog it drives is
    /// `pms`'s, read from other modules' tests too.
    #[test]
    fn the_status_readout_tells_loading_empty_and_failed_apart() {
        use crate::pms::HubState;
        use crate::ui::widgets::StatusKind;
        let _g = crate::testlock::serial();

        crate::pms::seed_for_test(0, HubState::Loading);
        let (_, kind, action) = status_read().expect("an empty Home must say something");
        assert_eq!(kind, StatusKind::Working);
        assert!(action.is_none(), "a fetch in flight offers no Retry — it IS the retry");

        crate::pms::seed_for_test(0, HubState::Failed);
        let (_, kind, action) = status_read().unwrap();
        assert_eq!(kind, StatusKind::Failed);
        assert!(action.is_some(), "an unreachable server must offer a way back");

        crate::pms::seed_for_test(0, HubState::Ready);
        let (_, kind, action) = status_read().unwrap();
        assert_eq!(kind, StatusKind::Empty, "an empty library is an answer, not an error");
        assert!(action.is_some());

        crate::pms::seed_for_test(3, HubState::Ready);
        assert!(status_read().is_none(), "shelves silence the read-out");
        crate::pms::seed_for_test(3, HubState::Failed);
        assert!(status_read().is_none(), "…including when a background refresh just failed");
        crate::pms::reset();
    }

    /// No shelves means no grid — the rule `set_snap_target` refuses new dives with and
    /// `home_update` re-applies to the live spring every frame. Both halves matter: a catalog can
    /// empty UNDER a snap already committed (a profile switch whose fetch fails does exactly
    /// that), and left at 1.0 with nothing to show, app.rs's pointer gate routes clicks to the
    /// empty grid's hit-test instead of the hero rects — leaving the status screen's Retry
    /// control, the one way back, silently unclickable.
    ///
    /// Only the pure rule is host-testable: driving `home_update` would drag the panic barrier's
    /// `gfx::clip_clear` — and with it `glDisable` — into a binary that has no GL to link against.
    #[test]
    fn no_shelves_means_no_grid_snap() {
        assert_eq!(pinned_snap(1.0, 0), 0.0, "an empty catalog has no grid to dive into");
        assert_eq!(pinned_snap(0.0, 0), 0.0, "…and the hero is where it stays");
        assert_eq!(pinned_snap(1.0, 1), 1.0, "one shelf is a grid");
        assert_eq!(pinned_snap(1.0, MAX_HUBS), 1.0);
        assert_eq!(pinned_snap(0.0, 3), 0.0, "a move back TO the hero is never filtered");
    }

    /// The escapes from a dead Home must survive it: the profile chip and the library tab pills
    /// keep their own activations, so only the Retry control takes the press.
    #[test]
    fn the_status_screen_takes_ok_but_never_the_top_band() {
        use crate::pms::HubState;
        let _g = crate::testlock::serial();

        crate::pms::seed_for_test(0, HubState::Failed);
        assert!(status_takes(0), "the Retry control takes the press");
        assert!(status_takes(c_int::MIN), "…and so does an OK that thinks it is on a grid card");
        assert!(!status_takes(-1), "the profile chip still opens the account menu");
        // the tab strip is uncapped since unit 10 (it scrolls, so every discovered section is
        // focusable) — walk a generous range rather than a constant that no longer exists
        for i in 0..8usize {
            assert!(!status_takes(hero_focus_for_pill(i)), "tab pill {i} still enters its library");
        }

        crate::pms::seed_for_test(3, HubState::Failed);
        assert!(!status_takes(0), "with shelves up, OK belongs to the hero row again");
        crate::pms::reset();
    }

    /// Regression: hit_at re-derived the card x WITHOUT the `* sp` snap fold the draw applies
    /// (`- scroll_x()` vs `- scroll_x() * env.sp`), so mid-snap the hover target and the drawn
    /// card disagreed by `scroll_x * (1 - sp)`. Both now go through card_x/col_at with ONE
    /// effective scroll — this pins the round-trip at every snap phase.
    #[test]
    fn pointer_hit_column_matches_the_drawn_card_at_every_snap_phase() {
        for &scroll in &[0.0f32, 415.0, 830.0] {
            for &sp in &[0.0f32, 0.37, 1.0] {
                let es = scroll * sp; // eff_scroll's fold, applied to both draw and hit
                for c in 0..24usize {
                    let center = card_x(c, es) + CARD_W * 0.5; // the drawn card's center
                    assert_eq!(
                        col_at(center, es, 24),
                        Some(c),
                        "clicking the center of drawn card {c} (scroll={scroll}, sp={sp}) must hit it"
                    );
                }
            }
        }
    }

    /// The item-menu popover anchors off `focused_card_rect`, so it has to agree with the DRAWN
    /// card at every scroll/snap phase or the panel floats away from the tile it belongs to. This
    /// pins the geometry the accessor composes — `card_x(c, eff_scroll) × Rect::scaled(focus)` —
    /// against the same formula `Grid::draw` uses, over the same phase grid as the hit-test test
    /// above. (The accessor itself needs a live `SCENE` + hubs, neither of which exists on the
    /// host; what is testable, and what actually broke in the hit-test's own history, is the
    /// formula.)
    #[test]
    fn the_card_anchor_rect_tracks_the_drawn_card_through_scroll_and_snap() {
        for &scroll in &[0.0f32, 415.0, 830.0] {
            for &sp in &[0.0f32, 0.37, 1.0] {
                let es = scroll * sp;
                for c in 0..8usize {
                    for &focus in &[1.0f32, RowStyle::HOME.focus_scale] {
                        let base = Rect::new(card_x(c, es), 176.0 + CARD_DY, CARD_W, CARD_H);
                        let r = base.scaled(focus);
                        // magnification is about the card's CENTRE — the anchor must not slide
                        assert!(
                            (r.cx() - base.cx()).abs() < 0.001 && (r.cy() - base.cy()).abs() < 0.001,
                            "card {c} (scroll={scroll}, sp={sp}, focus={focus}) moved its centre"
                        );
                        // …and the rect the panel is placed beside is the one the pointer would hit
                        assert_eq!(
                            col_at(r.cx(), es, 8),
                            Some(c),
                            "anchor centre for card {c} (scroll={scroll}, sp={sp}) is not over that card"
                        );
                        assert!(r.w >= CARD_W && r.h >= CARD_H, "focus must not shrink the card");
                    }
                }
            }
        }
    }

    // ── the hero backdrop: the ground, the reveal skip, and the prefetch policy ───────────────
    //
    // NONE of the five below takes the module's `FOCUS` mutex, and that is deliberate rather than
    // an omission: the lock exists for `static mut fr`/`fc`, and every function under test here is
    // pure — it takes its item, its snap and its pool size as ARGUMENTS. Do not add the lock "for
    // safety"; it would serialize five tests against the focus suite for nothing.

    /// A hero item carrying an UltraBlur envelope, and nothing else the wash looks at.
    fn blurred(blur: [[f32; 3]; 4]) -> PmsMovie {
        PmsMovie { has_blur: true, blur, ..Default::default() }
    }

    /// The black-frame regression, pinned at both ends of the snap.
    ///
    /// The old ground was `Painter::ambient`'s `dim` scaling the corners toward BLACK: at snap 0.9
    /// that is `dim = 0.055` and at 0.99 `dim = 0.0055` — a full-screen OPAQUE near-black rect
    /// behind the rising grid, which then snapped to `#2C2C2E` at 0.996. And an item with no
    /// envelope drew nothing at all, leaving the hero scrim on the bare clear. Flooring the lean on
    /// [`theme::SURFACE_APP`] fixes both, and this is the test that stops a later edit from
    /// re-deriving the lean as a scale.
    #[test]
    fn the_wash_floors_on_the_app_surface_and_never_dims_toward_black() {
        let flat = [theme::SURFACE_APP; 4];
        // "no artwork" IS the app's own ground, with no special case
        assert_eq!(wash_corners(None, 0.0), flat, "an empty pool is the app's ground");
        assert_eq!(wash_corners(Some(&PmsMovie::default()), 0.0), flat, "…and so is an item with no envelope");
        // the grid end of the snap lands on exactly the colour `frame_clear` already laid down —
        // where the old `dim` ramp landed on black
        let bright = blurred([[0.95, 0.93, 0.90], [1.0, 1.0, 1.0], [0.8, 0.75, 0.2], [0.1, 0.1, 0.12]]);
        assert_eq!(wash_corners(Some(&bright), 1.0), flat, "the dive resolves to the app ground, not to black");
        assert_eq!(wash_corners(Some(&bright), 1.5), flat, "…and an overshooting snap cannot push past it");

        // The tail of the dive — the exact frames that used to be near-black — for a DARK envelope
        // as much as a bright one. The bound is the one invariant that separates a WEIGHT scale
        // from a brightness ramp: a corner can only travel as far from the app surface as the lean
        // weight STILL IN PLAY allows it to, so the deviation is capped by that corner's own
        // remaining weight times the furthest a mix could take it (toward white above the surface,
        // toward black below it). Do not loosen this to a flat percentage — the whole point is that
        // it tightens to nothing as `sp → 1`, which is what pins the grid end of the snap.
        //
        // For scale: at sp=0.9 this allows ±0.046 on the two top corners, and the old `dim` ramp
        // put them at 0.010 — a deviation of 0.163, off by three and a half times the budget.
        for m in [&bright, &blurred([[0.0; 3]; 4])] {
            for &sp in &[0.9f32, 0.99, 0.996] {
                for (c, w) in wash_corners(Some(m), sp).iter().zip(HERO_WASH_W) {
                    let lean = w * (1.0 - sp);
                    for (ch, s) in c.iter().zip(theme::SURFACE_APP) {
                        let budget = lean * s.max(1.0 - s);
                        assert!(
                            (ch - s).abs() <= budget + 1e-6,
                            "at sp={sp} the ground is {ch} against a {s} surface — that is {} off a \
                             {budget} budget, so the old dim ramp is back",
                            (ch - s).abs()
                        );
                    }
                    assert_eq!(c[3], 1.0, "a wash corner is opaque");
                }
            }
        }

        // the lean is a WEIGHT SCALE on the shared mix, not a brightness scale over it. Re-deriving
        // it as `dim(keyed(blur, W), lean)` would darken the surface itself and fails here.
        assert_eq!(
            wash_corners(Some(&bright), 0.5),
            AmbientWash::keyed(bright.blur, HERO_WASH_W.map(|w| w * 0.5)),
            "half-dived is half the lean toward the artwork, not half the brightness"
        );
        // …and the lean does go TOWARD the artwork: a bright envelope lifts the panel off the
        // surface rather than sinking it.
        let lit = wash_corners(Some(&bright), 0.0);
        assert!(lit[1][0] > theme::SURFACE_APP[0], "a white corner must brighten the ground it keys");
        assert!(lit[1][0] < 1.0, "…but never all the way: it is a wash, not the photograph");
    }

    /// The prefetch's index math: complete coverage of "what can be on screen next", never the page
    /// already on it, and forward first — which one-key-per-frame makes load-bearing.
    #[test]
    fn prefetch_order_wraps_and_never_warms_the_page_on_screen() {
        let mut out = [0 as c_int; 2 * HERO_PREFETCH];
        assert_eq!(prefetch_order(0, 0, &mut out), 0, "an empty pool has no neighbours");
        assert_eq!(prefetch_order(0, 1, &mut out), 0, "…and neither does a one-page pool");
        assert_eq!(prefetch_order(0, 2, &mut out), 1, "in a two-page pool +1 and -1 are the same page");
        assert_eq!(out[0], 1);

        for n in 2..=8 as c_int {
            for cur in 0..n {
                let k = prefetch_order(cur, n, &mut out);
                assert!(k <= 2 * HERO_PREFETCH, "wrote {k} entries into a {}-slot buffer", 2 * HERO_PREFETCH);
                assert_eq!(out[0], (cur + 1).rem_euclid(n), "forward first (n={n}, cur={cur})");
                for i in 0..k {
                    assert!((0..n).contains(&out[i]), "page {} is outside a {n}-page pool", out[i]);
                    assert_ne!(out[i], cur, "warmed the page already on screen (n={n}, cur={cur})");
                    assert!(!out[..i].contains(&out[i]), "warmed page {} twice", out[i]);
                }
            }
        }
    }

    /// The gate's screen half. Warming while a flip is running would put a key in front of the very
    /// layer sliding on. (The store half — `posters::store_idle`, which keeps a warm from competing
    /// with a texture the user is waiting on, since the poster workers claim by slot index and have
    /// no priority to lose — is the caller's `&&`, deliberately outside this function so it is not
    /// evaluated on frames this half already refused.)
    #[test]
    fn the_prefetch_is_armed_only_from_a_settled_hero() {
        assert!(prefetch_armed(0.0, false), "billboard up, nothing moving");
        assert!(!prefetch_armed(0.0, true), "mid-flip, the incoming layer IS the thing being waited on");
        for &sp in &[0.05f32, 0.2, 1.0] {
            assert!(!prefetch_armed(sp, false), "at sp={sp} the billboard is on its way out");
        }
    }

    /// The ground's skip test — a full-screen fill whose failure is invisible on the host and is
    /// either a blank panel or a fill-rate regression on the TV.
    #[test]
    fn the_ground_is_skipped_only_when_opaque_art_covers_it() {
        assert!(wash_hidden(0.0, 1.0, None), "one fully revealed layer at rest covers the panel");
        assert!(wash_hidden(0.0, 1.0, Some(1.0)), "…and so do two, mid-flip");
        assert!(!wash_hidden(0.0, 0.7, None), "a layer still dissolving in shows the ground through");
        assert!(!wash_hidden(0.0, 1.0, Some(0.7)), "…and so does the OTHER layer of a flip");
        assert!(!wash_hidden(0.0, 0.0, None), "no art at all is exactly what the ground is for");
        // the snap both fades the art by (1 - sp) and slides it up off the bottom of the panel, so
        // full reveals do not mean coverage once the dive has begun
        assert!(!wash_hidden(0.2, 1.0, Some(1.0)), "a diving hero uncovers the panel however revealed its art is");
        assert!(!wash_hidden(1.0, 1.0, None), "the grid shows the ground (which by then is the flat surface)");
    }

    /// **The upward spill, bounded.** A hero clearLogo now overflows the TOP of its reserved band as
    /// paint (`hero_logo`'s layout ≠ paint rule), which is the one genuinely new behaviour on this
    /// page — and its failure mode, a 268px square emblem colliding with the tab pills, is otherwise
    /// only observable by waking a television. The two text heights are the device font's, quoted
    /// rather than measured (the host suite opens no SDL_ttf) — the same boundary `widgets`' anchor
    /// table works within. Touches no focus state, so it deliberately does NOT take the `FOCUS`
    /// mutex; do not add one.
    #[test]
    fn the_home_hero_logo_never_reaches_the_top_bar() {
        const META_H: f32 = 28.0 * 1.32; // one line of `size::BODY` at TextView's leading
        // the shared hero blurb at its cap — READ from `ui::hero_syn_h`, never quoted, so a change
        // to the rung or the leading moves this contract with it instead of silently invalidating
        // it (it was the literal `3.0 * 29.0` while home's blurb was MICRO/29)
        let syn_h = crate::ui::hero_syn_h(crate::ui::HERO_SYN_MAXLINES);
        let band = hero_logo::band_h(LogoRung::Hero);
        // the worst case is the TALLEST text stack — it puts the band, and so the spill above it,
        // highest on screen (a shorter summary only pushes the whole column back down)
        let top = hero_stack_top(band, META_H, theme::space::SM + syn_h);
        let spill = theme::logo::HERO_H_MAX - band;
        assert!(
            top - spill > crate::ui::widgets::TOP_BAR_BOTTOM,
            "a full-height logo tops out at {} — inside the top chrome ({})",
            top - spill,
            crate::ui::widgets::TOP_BAR_BOTTOM
        );
    }

    /// The prefetch and the draw must name ONE clearLogo key, and this rule is the only place they
    /// could drift: an episode's hero headlines the SERIES, so it is the show's logo, not its own.
    #[test]
    fn the_hero_logo_key_is_the_shows_for_an_episode() {
        let ep = PmsMovie { kind: 3, rk: "42".into(), show_rk: "7".into(), ..Default::default() };
        assert_eq!(hero_logo_rk(&ep), "7", "an episode's hero wears the show's logotype");
        let orphan = PmsMovie { kind: 3, rk: "42".into(), ..Default::default() };
        assert_eq!(hero_logo_rk(&orphan), "42", "…falling back to its own when the server sent no parent");
        let movie = PmsMovie { kind: 0, rk: "42".into(), show_rk: "7".into(), ..Default::default() };
        assert_eq!(hero_logo_rk(&movie), "42", "a movie is never keyed to a stray parent");
    }

    /// The caption and the resume bar beside it are ONE claim about an item — `Grid::draw` takes
    /// both in the same pass — so the caption's in-progress arm has to be exactly
    /// [`PmsMovie::resume_frac`]'s answer. Re-derived here as `resume_ms > 0 && dur_ns > 0` it was
    /// not: that pair has no end-guard, so a finished item whose server never cleared `viewOffset`
    /// wore the watched tick with no bar and still read "1 min left". Pure — takes its item as an
    /// argument, so no `FOCUS` mutex (the same call as the backdrop tests above).
    #[test]
    fn the_continue_watching_caption_promises_time_left_only_when_the_bar_is_drawn() {
        let ep = |resume_ms: i64| PmsMovie {
            kind: 3,
            dur_ns: 45 * 60 * 1_000_000_000,
            resume_ms,
            show_title: "Laura".into(),
            ..Default::default()
        };
        // never started, stopped exactly at the end, and a stale offset PAST it
        for m in [ep(0), ep(45 * 60_000), ep(60 * 60_000)] {
            assert!(m.resume_frac().is_none(), "offset {} is not in progress", m.resume_ms);
            let cap = cw_caption(&m).expect("a Continue Watching episode always captions");
            assert!(
                !cap.to_str().unwrap().contains("left"),
                "offset {}: no bar, so the caption must not promise time remaining ({cap:?})",
                m.resume_ms
            );
        }
        let mid = ep(20 * 60_000);
        assert!(mid.resume_frac().is_some(), "20 minutes into 45 IS in progress");
        assert_eq!(cw_caption(&mid).unwrap().to_str().unwrap(), "Laura \u{00b7} 25 min left");
    }

    // ---- the shelf heading's source annotation (Shared Sources, deliverable C) ----------------
    //
    // Pure flow arithmetic: no focus state, so these deliberately do NOT take the `FOCUS` mutex
    // (the same call as `the_home_hero_logo_never_reaches_the_top_bar`). The host suite opens no
    // SDL_ttf — `text_width` answers 0 for every string — so the flow is graded through a
    // SYNTHETIC metric whose width depends on the size and weight a run was measured at. That
    // dependence is the point: it is what makes "the annotation is measured at its own tokens, not
    // at the title's" an assertion rather than an intention.

    /// One run the flow asked for, as recorded by the measuring closure.
    struct Run {
        text: String,
        dx: f32,
        sz: c_int,
        bold: c_int,
        ink: [f32; 4],
    }

    /// Drive [`heading_flow`] with the synthetic metric; returns its total advance + every run.
    fn flow(title: &str, source: &str) -> (f32, Vec<Run>) {
        let mut runs: Vec<Run> = Vec::new();
        let w = heading_flow(title, source, |s, dx, sz, bold, ink| {
            runs.push(Run { text: s.to_string(), dx, sz, bold, ink });
            width_of(s, sz, bold)
        });
        (w, runs)
    }
    /// A stand-in for `Painter::text`'s advance — monotone in length, size and weight.
    fn width_of(s: &str, sz: c_int, bold: c_int) -> f32 {
        s.chars().count() as f32 * (sz as f32 + 6.0 * bold as f32)
    }

    /// (a) **The acceptance criterion for the whole feature.** With no source the heading is ONE
    /// run — the title, at the title's own tokens — and its width is the title's own advance with
    /// nothing added. No dot, no pad, no empty second run: absence, not an empty annotation.
    #[test]
    fn a_shelf_with_no_source_draws_exactly_the_title_and_nothing_else() {
        let (w, runs) = flow("Recently Added", "");
        assert_eq!(runs.len(), 1, "an empty source must produce no further runs at all");
        assert_eq!(runs[0].text, "Recently Added");
        assert_eq!(runs[0].dx, 0.0, "the title starts at the heading origin");
        assert_eq!(w, width_of("Recently Added", theme::size::HEADLINE, 1), "the title's own advance, exactly");
    }

    /// (b) A source annotation can only ADD to the heading — the title in front of it is
    /// untouched, and the flow grows by the dot, the handle and the two pads between them.
    #[test]
    fn a_shared_source_extends_the_heading_past_the_title() {
        let (bare, _) = flow("Recently Added in Film Club", "");
        let (annotated, runs) = flow("Recently Added in Film Club", "friend");
        assert!(annotated > bare, "the annotation must extend the heading ({annotated} vs {bare})");
        assert_eq!(runs.len(), 3, "title, separator, handle");
        assert_eq!(runs[0].dx, 0.0, "the title still starts at the origin");
        assert_eq!(
            annotated,
            bare + 2.0 * SOURCE_PAD
                + width_of("\u{b7}", theme::size::BODY, 0)
                + width_of("friend", theme::size::BODY, 0),
            "the growth is exactly the dot, the handle and one pad either side of the dot"
        );
        assert_eq!(runs[1].dx, bare + SOURCE_PAD, "the dot is one pad past the title");
        assert_eq!(
            runs[2].dx,
            runs[1].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
            "the handle is one pad past the dot"
        );
    }

    /// (c) Each run is measured and inked at the tokens the design names for IT — the annotation is
    /// a rung down, regular against the title's bold, tertiary ink after a `.45` separator. Those
    /// tokens are also what `draw_heading` resolves each run's baseline drop from, so pinning them
    /// pins the alignment the host cannot measure.
    #[test]
    fn each_heading_run_carries_its_own_size_weight_and_ink() {
        let (_, runs) = flow("Recently Added in Film Club", "friend");
        assert_eq!((runs[0].sz, runs[0].bold), (theme::size::HEADLINE, 1), "the title is HEADLINE bold");
        assert_eq!(runs[0].ink, theme::TEXT_HEADING, "…in the shared section-heading ink");
        assert_eq!(runs[1].text, "\u{b7}");
        assert_eq!((runs[1].sz, runs[1].bold), (theme::size::BODY, 0), "the separator is measured at BODY regular");
        assert_eq!(runs[1].ink, theme::TEXT_SEPARATOR, "…at the separator token's own .45");
        assert_eq!(runs[2].text, "friend");
        assert_eq!((runs[2].sz, runs[2].bold), (theme::size::BODY, 0), "the handle is BODY regular, not the title's");
        assert_eq!(runs[2].ink, theme::TEXT_TERTIARY);
        assert!(
            (runs[1].sz, runs[1].bold) == (runs[2].sz, runs[2].bold),
            "the dot and the handle are one annotation: same rung, same weight, so they drop onto the title's baseline together"
        );
        assert!(runs[1].sz < runs[0].sz, "the annotation is a rung DOWN from the title — the drop is why it must be baseline-aligned");
    }

    // ---- the HERO's own source run (Shared Sources, deliverable C) ----------------------------
    //
    // The hero is not the focused tile of the shelf beneath it — it is an independent rotating
    // billboard — so the heading below it attributes something else and this run is the only thing
    // on the screen that says whose the film is. Same synthetic metric as the heading tests above,
    // and the same reason for it. Pure: no focus state, no `FOCUS` mutex.

    /// One meta run, plus the BUDGET it was given — which is the half the heading flow has no
    /// equivalent of, and the half the truncation rule lives in.
    struct MetaRun {
        text: String,
        dx: f32,
        budget: f32,
        sz: c_int,
        bold: c_int,
        ink: [f32; 4],
    }

    /// Drive [`meta_source_flow`] with the synthetic metric, eliding to the budget the way
    /// `draw_meta_source`'s closure does (`text::elide` never returns a run wider than its budget).
    /// Returns the whole line's advance + every run the flow asked for.
    fn meta_flow(base_w: f32, source: &str) -> (f32, Vec<MetaRun>) {
        let mut runs: Vec<MetaRun> = Vec::new();
        let w = meta_source_flow(base_w, source, |s, dx, budget, sz, bold, ink| {
            runs.push(MetaRun { text: s.to_string(), dx, budget, sz, bold, ink });
            width_of(s, sz, bold).min(budget.max(0.0))
        });
        (w, runs)
    }

    /// **The one-source acceptance criterion.** With no source the hero meta line is exactly the
    /// line it has always been: the flow asks for nothing, adds nothing to its advance, and issues
    /// no draw call. Absence, not an empty annotation — which is what makes the run free for the
    /// installs that will never see one.
    #[test]
    fn a_hero_from_our_own_server_draws_no_source_run_at_all() {
        let (w, runs) = meta_flow(420.0, "");
        assert!(runs.is_empty(), "an empty source must produce no runs, no pad and no dot");
        assert_eq!(w, 420.0, "…and must not move the line's own end by a pixel");
    }

    /// A borrowed hero states whose it is, as the LAST run on its own meta line — after a middot,
    /// one pad either side, in the design's own words ("the person, not the machine").
    #[test]
    fn a_borrowed_hero_states_its_owner_as_the_last_run_on_the_line() {
        let base = 420.0;
        let (w, runs) = meta_flow(base, "friend");
        assert_eq!(runs.len(), 2, "the separator and the run, and nothing else");
        assert_eq!(runs[0].text, "\u{b7}");
        assert_eq!(runs[0].dx, base + SOURCE_PAD, "the dot is one pad past the facts");
        assert_eq!(runs[1].text, "Shared by friend", "the person, not the machine");
        assert_eq!(
            runs[1].dx,
            runs[0].dx + width_of("\u{b7}", theme::size::BODY, 0) + SOURCE_PAD,
            "the run is one pad past the dot"
        );
        assert_eq!(
            w,
            base + 2.0 * SOURCE_PAD
                + width_of("\u{b7}", theme::size::BODY, 0)
                + width_of("Shared by friend", theme::size::BODY, 0),
            "the line grows by exactly the dot, the run and one pad either side of the dot"
        );
    }

    /// **The rung stays, the ink steps down.** The run keeps the line's own `BODY` — it is read
    /// from across the room — and takes one step of ink, `TEXT_SECONDARY` → `TEXT_TERTIARY`. It is
    /// deliberately NOT the detail page's `CAPTION`, which is that line's own rung and not a
    /// property of the run: a rung is a role, and only the ink step travels between the two screens.
    #[test]
    fn the_hero_source_run_keeps_the_lines_rung_and_takes_one_step_of_ink() {
        let (_, runs) = meta_flow(420.0, "friend");
        for r in &runs {
            assert_eq!((r.sz, r.bold), (theme::size::BODY, 0), "'{}' left the meta line's own rung", r.text);
            assert_ne!(r.sz, theme::size::CAPTION, "the hero line is BODY — it is not E's line and must not shrink to it");
        }
        assert_eq!(runs[0].ink, theme::TEXT_SEPARATOR, "the middot carries the separator token's own .45");
        assert_eq!(runs[1].ink, theme::TEXT_TERTIARY, "one step under the line's TEXT_SECONDARY, never level with it");
        assert_ne!(runs[1].ink, theme::TEXT_SECONDARY);
    }

    /// **The measurement rule: the line never becomes two.** Every run is handed a finite budget
    /// running to [`HERO_META_R`], so an over-long handle ELIDES where a wrapped block would have
    /// spilled onto a second line — and the whole line still ends at the bound. The worst case is a
    /// meta line whose own facts already filled the hero column: even then the run must have real
    /// room, or the annotation would arrive as a bare ellipsis.
    #[test]
    fn an_over_long_handle_truncates_rather_than_wrapping() {
        let ridiculous = "a-very-long-plex-account-handle-that-nobody-would-ever-choose";
        for base in [0.0f32, 420.0, HERO_COL_W] {
            let (w, runs) = meta_flow(base, ridiculous);
            assert_eq!(runs.len(), 2, "base {base}: a long handle is still ONE run — there is no second line to go to");
            for r in &runs {
                assert!(r.budget > 0.0, "base {base}: '{}' was given no room at all ({})", r.text, r.budget);
                assert!(r.dx + r.budget <= META_FLOW_W + 1e-3, "base {base}: '{}' may elide past the bound", r.text);
            }
            assert!(w <= META_FLOW_W + 1e-3, "base {base}: the line ran to {w}, past its {META_FLOW_W} bound");
        }
        // …and the room left at that worst case is a real annotation's worth rather than a stub —
        // a QUARTER of the whole line, which is the guard that a future widening of the hero column
        // (or a tightening of the bound) cannot quietly starve the run into a bare ellipsis. It is
        // stated as a share of the line and not in pixels of text because the synthetic metric here
        // is roughly twice the shipped font's advance; the share is a claim about the geometry,
        // which is the part the host can actually speak for.
        let (_, worst) = meta_flow(HERO_COL_W, "friend");
        assert!(
            worst[1].budget >= 0.25 * META_FLOW_W,
            "a meta line whose facts fill the column leaves the run only {} of {META_FLOW_W}",
            worst[1].budget
        );
    }

    /// Where the bound IS: inside the corner wedge, and a full fifth of the panel short of the last
    /// fifth of the frame — the side of the picture the composition is usually about, which the
    /// wedge leaves alone entirely. Both ends are read from `widgets::hero_scrim_a` (the same closed
    /// form the hero's legibility contract is graded on) rather than restated here, so a retune of
    /// the wedge fails this instead of silently stranding the run outside it.
    #[test]
    fn the_meta_lines_bound_keeps_the_run_inside_the_hero_wedge() {
        use crate::ui::widgets::hero_scrim_a;
        assert!(hero_scrim_a(HERO_META_R, 1.0) > 0.0, "the line ends where the wedge has already given up");
        let wedge_end = (0..=SCR_W as i32)
            .map(|x| x as f32)
            .find(|&x| hero_scrim_a(x, 1.0) <= 0.0)
            .expect("the wedge must end inside the frame");
        assert!(
            HERO_META_R <= wedge_end - 0.2 * SCR_W,
            "the bound ({HERO_META_R}) must stay a fifth of the panel short of the wedge's end ({wedge_end})"
        );
        assert!(HERO_META_R > MARGIN_X + HERO_COL_W, "…and past the text column, or the run has nowhere to go");
    }
}
