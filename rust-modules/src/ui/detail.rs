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
use crate::ui::widgets::{resolve_tex, Art, Button, CircleButton, ControlStyle, TabPill};
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
// Play pill MINIMUM width. The drawn width is `Button::pill_w`, measured from the label, and for
// "Play" that lands ~2px above this — so the detail Play pill is now pixel-identical to home's
// rather than approximating it with a constant. The floor only guards a pathologically short label.
const PW: f32 = 168.0;
const CGAP: f32 = 20.0;
const CD: f32 = 60.0; // circle button diameter
/// The hero text column's width — the synopsis wrap AND every fine-print line under it (the
/// date/runtime pair, the "Directed by" credit) share it, so nothing in the block can grow past
/// the right-aligned "Starring" line.
const HERO_TEXT_W: f32 = 900.0;

// ---- the hero text column's y-chain (`hero_chain`), pitch by pitch, from `Details Screen.dc.html`.
// These are the GAPS between line tops, not absolute ys: two of the bands are conditional and the
// synopsis is measured, so only the pitches are constant. ----
/// The title block's baseline band — its bottom edge, which the clearLogo art and the text title
/// both sit on, and the anchor the whole chain hangs from.
const TITLE_BOTTOM: f32 = 566.0;
const META_DY: f32 = 30.0; // title bottom → "TV Show · Drama · TV-MA"
const RATINGS_DY: f32 = 50.0; // meta line → the review-score row
const SYN_DY: f32 = 54.0; // last line above → synopsis
const FACTS_DY: f32 = theme::space::MD; // synopsis bottom → date/counts line
const BTN_DY: f32 = 50.0; // last fine-print line → the action row
/// Synopsis line pitch, and the floor its measured height is clamped up to so a one-line blurb (or a
/// missing one) still leaves the facts line where a two-line blurb puts it.
const SYN_LEAD: f32 = 36.0;
const SYN_MIN_H: f32 = 34.0;
/// Lines of blurb before it elides — the mock's own band. Past this the flow walks the Play pill down.
const SYN_MAXLINES: usize = 3;
// ---- the "Starring" block: a right-aligned wrapping caption in its own column beside the action row.
/// Its column. Narrow on purpose — it must not reach the hero text column on its left, and wrapping to
/// two lines inside 560px is what keeps three long names off the Play pill.
const STARRING_W: f32 = 560.0;
const STARRING_LEAD: f32 = 32.0;
/// Its cap top relative to the action row's own top — a few px down, so the two read as level rather
/// than as one hanging off the other.
const STARRING_DY: f32 = 6.0;
/// Share of the hero text column the blurb's bold `S2, E3 · Laura:` prefix may claim before the
/// episode title inside it starts eliding. It shares line 1 with the start of the blurb, so it cannot
/// have the whole width — see `hero_blurb`.
const LEAD_BUDGET: f32 = 0.55;
/// Gap between two review-score badges. Deliberately its own value rather than a `space` rung: `MD`
/// (24) let four marks in four different silhouettes crowd into one texture, and `LG` (40) broke the
/// row into four unrelated islands. 32 is the mock's, and it reads as one row of four things.
const RATING_ROW_GAP: f32 = 32.0;

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
const HERO_FADE: f32 = 400.0; // px of scroll over which the hero fades out (draw + pointer share it)
const HERO_HIT_MIN_A: f32 = 0.5; // a hero control must be at least this opaque to be CLICKABLE

// Season tabs (header for the episode row)
const TAB_ROW_H: f32 = CD; // tab pills stand as tall as the hero buttons (one control height)
/// Season-tab pill padding either side of its label (`Details Screen.dc.html`'s `padding: 0 26px`,
/// up from 18). A 60px-tall pill wrapped on 18 read as a tall thin lozenge; at 26 the pill is the
/// capsule the mock draws, and it matches the Play pill's own label inset beside it.
const TAB_PAD: f32 = 26.0;
/// Air BETWEEN two tab pills.
const TAB_GAP: f32 = theme::space::SM;
/// Per-tab horizontal advance past the label width — derived from the two above rather than spelled,
/// so a change to the padding can't silently change the gap (which is what 52 vs 18 used to hide).
const TAB_ADVANCE: f32 = 2.0 * TAB_PAD + TAB_GAP;
const SEASON_SETTLE: f32 = 0.2; // hold a season tab this long (s) before its episodes are fetched
// Episodes: landscape stills + under-card metadata
const EP_ANIM_MAX: usize = 40; // per-episode pop-spring count (episodes past this don't animate the pop)
const EP_W: f32 = 420.0;
const EP_H: f32 = 236.0; // 16:9-ish still
const EP_GAP: f32 = 28.0;
// ---- marks ON the still (`Details Screen.dc.html`). Every one of these is CONSTANT through the
// focus pop — they ride the scaled rect without resizing — because they are 1:1-texel content and a
// size on the scale spring would rasterize + upload a fresh mask every animating frame.
//
// The still carries exactly TWO things: a bottom scrim with one state line on it, and a full-bleed
// progress bar. There is no capsule behind the line and no badge in any corner; see `ep_state`.
/// Left inset of the state line, and the margin every mark on a still shares.
const EP_MARK_INSET: f32 = 16.0;
/// The progress bar's height. It is **full-bleed at the very bottom edge** — the same bar, in the
/// same place, that a Continue Watching card wears (`card_row::resume_bar`), which is the whole point:
/// this strip used to draw an inset rounded capsule 16px up, so "how far in am I" was two different
/// objects on two screens of one app.
const EP_BAR_H: f32 = 5.0;
/// The bottom scrim under the state line: its height and its darkest alpha at the card's edge.
/// Without it a white label sits on an arbitrary video frame and its legibility is a coin flip.
const EP_SCRIM_H: f32 = 88.0;
const EP_SCRIM_A: f32 = 0.78;
/// The state line's BASELINE inset from the still's bottom edge. The mock spells this as a CSS
/// `bottom:11px` on the line's flex box, which is its box bottom, not its baseline — with a 24px font
/// on `line-height:normal` the descender band under that baseline is ~6px, so the equivalent baseline
/// inset is ~17. Reading the 11 literally sat the label 6px lower than the design and that much closer
/// to the bar.
const EP_LINE_BOT: f32 = 17.0;
/// The state glyph's box, and its gap to the label. ONE box for both glyphs (the play triangle and
/// the watched disc) so the label starts at the same x whichever state a tile is in — the mock sizes
/// them 18 and 20 and lets the label shift, which makes a row of stills read as ragged.
const EP_GLYPH_D: f32 = 20.0;
const EP_LINE_GAP: f32 = 10.0;
// Under-still metadata: kicker → title → summary → date row. The (fine-print) air date sits at the
// BOTTOM of the block, under the summary; offsets are from the meta top (EP_H + EP_META_TOP below
// the still) and the block height is derived from the tallest episode summary (see `ep_meta_h`) so
// the dynamic below-hero flow reflows to fit.
//
// The mock RESERVES the blurb's band instead (`min-height:102px`, which would make this whole block a
// fixed height and retire `ep_meta_h`'s measure sweep). Deliberately not adopted — owner call: the
// block stays content-derived, so a short blurb takes short space. Only the mock's TYPE changes land
// here: the title steps up a rung, and both leadings open to 34.
// The three gaps below are all measured last-line INK (baseline) → next block's cap top, and the mock
// spells the same rhythm as CSS margins between 34px line boxes (10/12/12). Converted, its optical
// gaps come out near-UNIFORM at ~23/27/26; ours used to TIGHTEN down the block (20/18/16), which read
// as the blurb crowding the date. One rung — `space::MD` — for the two that carry prose.
const EP_META_TOP: f32 = 30.0; // still bottom → kicker
const EP_TITLE_DY: f32 = 46.0; // kicker BASELINE → title cap top (≈24 of ink gap)
const EP_TITLE_LEAD: f32 = 34.0; // title line pitch (title wraps to at most 2 lines)
const EP_TITLE_SUMMARY_GAP: f32 = theme::space::MD; // title last-line baseline → summary cap top
const EP_SUMMARY_DATE_GAP: f32 = theme::space::MD; // summary last-line baseline → date cap top
const EP_SUMMARY_LEAD: f32 = 34.0;
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

// ---- one geometry source per focusable region -------------------------------------------------
// Each of these is called by the DRAW and by the pointer hit-test (`hit_at`), so a control can never
// be drawn in one place and clicked in another. Same immediate-mode contract as `home::card_x` /
// `col_at`: derive, don't record — the rects fall out of the same constants the painter uses.

/// The Play pill's label — the word the press will actually perform, so it decides the pill's WIDTH
/// as well as its text. One source for the painter and for [`hero_pill_w`].
fn hero_pill_label() -> &'static std::ffi::CStr {
    if has_restart() {
        c"Resume"
    } else {
        c"Play"
    }
}
/// …and the pill's drawn width: measured from that label through the one hero-pill formula
/// (`widgets::pill_w`), floored at `PW` so a pathologically short label still gets a pill.
fn hero_pill_w() -> f32 {
    // `icon: true` — the hero pill carries the Play glyph, so its frame reserves the icon box
    Button::pill_w(hero_pill_label().as_ptr(), theme::size::BODY, true).max(PW)
}

/// The hero action-row control rect for index `i` at row top `y`.
///
/// The row is DYNAMIC — the ↺ restart disc exists only while [`has_restart`] — and the pill's width
/// follows its label, so positions are ACCUMULATED rather than per-control constants: the pill sits
/// at the margin, and every control after it is a `CD` disc one `CD + CGAP` further along than the
/// last. Indices stay contiguous (`hero_col` shifts focus with the set, `btn_watched` is always the
/// end), so "disc slot" is simply `i - BTN_PLAY - 1` and the watched toggle slides left by exactly
/// one disc when the restart control is absent. Walked by `draw_buttons` AND by the pointer
/// hit-test — anything else would put the click a control out of step whenever a resume point
/// exists, which is precisely the state a pointer user is most likely to be in.
fn hero_btn_rect(i: c_int, y: f32) -> Rect {
    hero_btn_rect_w(i, y, hero_pill_w())
}
/// [`hero_btn_rect`]'s pure core — the accumulation itself, for a pill of measured width `pw`.
///
/// Split from the measurement on purpose: `hero_pill_w` reaches SDL_ttf, and a host test that calls
/// into the TV's text stack fails to LINK on the dev Mac (the crate's Tier-1 limit). Parameterising
/// the width keeps the row's geometry host-testable at BOTH pill widths — "Play" and the wider
/// "Resume" — which is the case the fontless host could otherwise never see.
fn hero_btn_rect_w(i: c_int, y: f32, pw: f32) -> Rect {
    if i <= BTN_PLAY {
        return Rect::new(MARGIN_X, y, pw, CD);
    }
    let slot = (i - BTN_PLAY - 1) as f32;
    Rect::new(MARGIN_X + pw + CGAP + slot * (CD + CGAP), y, CD, CD)
}

/// A season tab's pill: padded `TAB_PAD` either side of the tab's CONTENT (its label plus whatever
/// trailing note it carries — `TabLay::w` already measures both), standing the full row height so
/// tabs and hero buttons read as one control family. `top` is the row's local y for the draw and its
/// screen y for the hit-test; the horizontal scroll is applied by the caller, which is why this
/// takes the layout entry rather than reaching for one.
fn tab_pill_rect(lay: &TabLay, top: f32) -> Rect {
    Rect::new(lay.x - TAB_PAD, top, lay.w + 2.0 * TAB_PAD, TAB_ROW_H)
}

/// The content-space left edge of strip index `i` at horizontal pitch `pitch` — the ONE x formula
/// the episode filmstrip and the Related/Cast shelves (`card_row::strip`) advance by.
///
/// It MUST stay the formula `card_row::strip` uses (`sty.margin_x + i * pitch`); the hit-test
/// `strip_at` is its inverse, so a divergence puts every shelf click on the wrong tile. Every
/// `RowStyle` sets `margin_x: MARGIN_X` today, which is what makes the two agree — the test
/// `strip_x_matches_the_shared_shelf_formula` pins that rather than leaving it a coincidence.
#[inline]
fn strip_x(i: usize, pitch: f32) -> f32 {
    MARGIN_X + i as f32 * pitch
}

/// Inverse of [`strip_x`]: which of `n` tiles of width `w` has screen x `mx` inside its DRAWN span,
/// given the row's live horizontal scroll `sx`. (home's `col_at`, for detail's three strips.)
#[inline]
fn strip_at(mx: f32, sx: f32, n: usize, pitch: f32, w: f32) -> Option<usize> {
    (0..n).find(|&i| {
        let x = strip_x(i, pitch) - sx;
        mx >= x && mx <= x + w
    })
}

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    let idx = view().selected;
    if idx < 0 {
        return None;
    }
    crate::pms::movie(idx as usize)
}

/// The resume position (ns) the hero's Play control would ACTUALLY apply — i.e. exactly what
/// `on_ok`'s play arm hands `app.rs`. A movie resumes from its own `viewOffset`; a show resumes its
/// ON-DECK episode (see [`hero_episode`]), so it reads that leaf's — and reads nothing at all on a show
/// the hero is presenting as a series, where Play starts from 0.
///
/// Both go through `metadata::resume_ns`, the same Plex rule the action applies, deliberately: keyed
/// on a raw `resume_ms > 0` instead (as the home hero is), a 4-second offset or one past the 95%
/// end-of-item mark would label the pill "Resume" for a play that starts at 0, and hang a restart
/// disc beside it that does nothing. The label and the control set have to promise what the press
/// delivers.
/// Has this show been started AT ALL? The server puts `S1E1 off=0` on deck for a show nobody has
/// touched (verified across six shows, 2026-07-30), so "there is something on deck" is NOT the same
/// question as "this show is underway", and only the second one decides whether the hero is about an
/// episode.
fn show_started(d: &metadata::Detail) -> bool {
    d.on_deck.as_ref().map(|e| e.resume_ms > 0).unwrap_or(false)
        || d.seasons.iter().any(|s| s.viewed_leaf_count > 0)
}

/// The episode a show's hero is ABOUT — its backdrop still, its blurb, its facts line, its Resume
/// label and the episode Play starts. `None` means the hero presents the SERIES instead.
///
/// It is the SERVER's on-deck episode (`Detail::on_deck`), not a search of the loaded season, and that
/// is the entire point: the client holds one season at a time, so the previous client-side search made
/// the hero's subject change every time you browsed to a different season tab. On this library the
/// server answers S2E2 for The Morning Show while season 1 is the tab you are looking at.
///
/// Two states have no episode to be about, and the server only covers one of them:
/// * **finished** — PMS returns no `OnDeck` at all, so `?` here handles it for free.
/// * **unplayed** — PMS *does* offer `S1E1`, so [`show_started`] is what rejects it. A show you have
///   never opened leads with its own key art and premise, which is what a first visit needs.
fn hero_episode() -> Option<&'static metadata::Episode> {
    let d = metadata::current()?;
    let ep = d.on_deck.as_ref()?;
    show_started(d).then_some(ep)
}

fn hero_resume_ns() -> i64 {
    let Some(d) = metadata::current() else { return 0 };
    if d.is_show {
        hero_episode().map(|e| metadata::resume_ns(e.resume_ms, e.dur_ms)).unwrap_or(0)
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
    let title_h = TextView::new(title, theme::size::BODY, theme::TEXT_PRIMARY)
        .bold()
        .leading(EP_TITLE_LEAD)
        .max_lines(2)
        .measure_h(EP_W);
    let (lt, lb) = crate::text::text_cap_band(theme::size::BODY, 1); // title cap-top/baseline offsets
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
    scroll_target_for(view().section)
}
/// [`scroll_target`] for an ARBITRARY section — the page's resting scroll while `sec` holds focus.
/// Parameterized so the pointer can ask "would focusing this move the page?" without moving it
/// (`hover_allows`); the focused-section call above is the same function with today's section.
fn scroll_target_for(sec: c_int) -> f32 {
    if sec == 0 {
        return 0.0;
    }
    // episodes anchor on the season tabs above them, so the tabs stay visible while browsing episodes
    let anchor = if sec == 2 { 1 } else { sec };
    let (s, n) = sections();
    // the anchor's non-hero ScrollColumn index; lift its top to margin (== the old section_y math)
    match s[..n].iter().position(|&x| x == anchor) {
        Some(pos) if pos >= 1 => {
            let v = view();
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
    // a keep-focus latch belongs to the item that armed it — a new page must not inherit one
    unsafe { *addr_of_mut!(KEEP_EP) = None };
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
/// (label plus whatever trailing note it carries) and the label CString.
struct TabLay {
    i: usize,
    x: f32,
    w: f32,
    label: CString,
}

/// ONE source of truth for the season-tab strip's layout — the draw pass and the focus/scroll
/// geometry both walk this, so their x-advance can't drift. A label that can't be a CString
/// (interior NUL — never in practice) is skipped entirely (not drawn, no advance).
///
/// **A tab is its name and nothing else.** It carried an episode COUNT until 2026-07-29 and a
/// finished-season TICK until 2026-07-30, both removed by owner call and both for the same reason: a
/// tab strip is navigation, and hanging status off it widened every pill in the row for a fact the
/// content underneath already shows. The episode tiles carry their own watched marks (`ep_state`), so
/// a finished season is visible by scanning it — the tab does not need to summarise that.
///
/// COST: this is uncached and runs on the draw path (and again per frame while the tab row holds
/// focus, through `tab_focus_geom`), so it is one `CString` + one `text_width` per season per call.
/// Cheap now that `text::text_width` is a `TTF_SizeUTF8` metrics walk rather than a
/// rasterize+upload, but a many-season strip (Top Gear) is the shape that would want the
/// fingerprinted cache `ep_meta_h` uses, and no FPS scene covers it: `fps:detail-transition` is a
/// MOVIE, which returns from `draw_tabs` before reaching here.
fn tabs_layout(d: &crate::metadata::Detail) -> Vec<TabLay> {
    let mut x = MARGIN_X;
    let mut out = Vec::with_capacity(d.seasons.len());
    for (i, s) in d.seasons.iter().enumerate() {
        let label = if s.title.is_empty() { format!("Season {}", s.index) } else { s.title.clone() };
        if let Ok(lc) = CString::new(label) {
            let mut lay = TabLay { i, x, w: 0.0, label: lc };
            lay.w = crate::text::text_width(lay.label.as_ptr(), theme::size::BODY, 1);
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
///
/// `LABEL`/36, not `MICRO`/29: the blurb is the longest run of prose on the page and it sits over the
/// backdrop scrim, where the fine-print rung read as a caption someone had shrunk by mistake
/// (`Details Screen.dc.html`, and its `github.md` note — *"lifted to LABEL 26/36, it read too small
/// on-device at MICRO 22"*). [`theme::TEXT_READING`] goes with the size for the same reason. The
/// 3-line cap is the mock's own band; past that the flow would walk the Play pill down the screen.
fn hero_synopsis<'a>(summary: &'a str, lead: &'a str) -> TextView<'a> {
    let v = TextView::new(summary, theme::size::LABEL, theme::TEXT_READING)
        .leading(SYN_LEAD)
        .max_lines(SYN_MAXLINES);
    if lead.is_empty() {
        v
    } else {
        v.lead(lead, theme::TEXT_PRIMARY)
    }
}

/// The hero's blurb, as `(lead, body)`.
///
/// For a **show** this is the [`hero_episode`]'s OWN summary behind a bold `S2, E3 · Laura:` prefix —
/// not the series premise. The premise is static and you read it once; what the page owes you on
/// every later visit is what happens next, and the prefix is what stops that from reading as the
/// show's description. For a movie — or a show whose episodes have not landed yet, since the season
/// fetch is async and the hero must render meanwhile — it is the item's own summary and no prefix.
///
/// Owned rather than borrowed because the prefix is formatted: two short allocations a frame (this
/// runs once in the flow measure and once in the paint), against the 40-a-frame the episode strip
/// used to do before its lines were cached.
fn hero_blurb(m: Option<&PmsMovie>) -> (String, String) {
    let d = metadata::current();
    if let Some(ep) = hero_episode() {
        if !ep.summary.is_empty() {
            // The prefix shares line 1 with the start of the blurb, so it gets a BUDGET and the title
            // inside it elides to fit. Without this a long episode title ("In the Dark Night of the
            // Soul It's Always 3:30 in the Morning") consumed the whole column, leaving the blurb's
            // first word a few px to wrap into — which came out as a line reading `A…`. The title is
            // on its own tile in the filmstrip below, so eliding it here loses nothing.
            let title = if ep.title.is_empty() {
                String::new()
            } else {
                let head = format!("S{}, E{} \u{b7} ", ep.season, ep.index);
                let room = HERO_TEXT_W * LEAD_BUDGET
                    - crate::text::text_width(
                        CString::new(head.as_str()).unwrap_or_default().as_ptr(),
                        theme::size::LABEL,
                        1,
                    )
                    - crate::text::text_width(c":".as_ptr(), theme::size::LABEL, 1);
                format!(" \u{b7} {}", crate::text::elide(&ep.title, room.max(0.0), theme::size::LABEL, 1, false))
            };
            return (format!("S{}, E{}{}:", ep.season, ep.index, title), ep.summary.clone());
        }
    }
    let own = d.map(|d| d.summary.clone()).unwrap_or_else(|| m.map(|m| m.summary.clone()).unwrap_or_default());
    (String::new(), own)
}

/// The loaded item's director credits, or None when the server sent no `Director[]` — the ONE
/// rule for whether the hero carries a "Directed by …" line. The flow (`hero_layout`) reserves the
/// line's band on it and the paint (`draw_hero`) formats it, so the two cannot disagree. Borrowed
/// rather than formatted: `hero_layout` runs twice a frame and has no business allocating.
fn directors() -> Option<&'static [String]> {
    metadata::current().map(|d| d.directors.as_slice()).filter(|v| !v.is_empty())
}

/// Whether the loaded item has any review score to badge — the ONE rule for whether the hero
/// reserves the ratings band. Paired with [`directors`]: both are conditional bands, and both are
/// answered from the same place the paint reads so the flow and the pixels agree.
fn has_ratings() -> bool {
    metadata::current().map(|d| !d.ratings.is_empty()).unwrap_or(false)
}

/// The hero text column's y-chain, computed ONCE per frame for BOTH the painter ([`draw_hero`]) and
/// the below-hero scroll flow ([`update`]'s `column.top`), so the pixels and the scroll math cannot
/// desync. A 4-line synopsis used to push the Play pill to within 2px of the season tabs; the flow
/// keeps one standardized gap ([`BTN_CONTENT_GAP`]) under the buttons for every title.
///
/// A struct rather than the tuple this used to be: it grew a sixth element (the ratings row) and two
/// of the six are CONDITIONAL, which is exactly the shape where positional access starts naming the
/// wrong band silently.
struct HeroChain {
    /// "TV Show · Drama · TV-MA".
    meta_y: f32,
    /// The review-score row. Only meaningful, and only given a band, when [`has_ratings`].
    ratings_y: f32,
    syn_y: f32,
    /// The one fine-print line: air date · extent-or-runtime · media chips · the "Directed by" credit,
    /// all of it — plus, right-aligned on the same line, the "Starring" credit.
    facts_y: f32,
    btn_y: f32,
}

fn hero_layout(m: Option<&PmsMovie>) -> HeroChain {
    let (lead, body) = hero_blurb(m);
    let syn_h = if body.is_empty() { 0.0 } else { hero_synopsis(&body, &lead).measure_h(HERO_TEXT_W) };
    hero_chain(syn_h, has_ratings())
}

/// The pure arithmetic half of [`hero_layout`] — everything except the synopsis measurement, which
/// is the one part that needs a font. Split out so the flow-vs-paint contract this chain exists to
/// hold (above all the two CONDITIONAL bands) is host-testable; measuring is not, since the host
/// suite opens no SDL_ttf.
///
/// The pitches reproduce `Details Screen.dc.html`'s chain on a two-line blurb — title bottom 566 →
/// meta 596 → ratings 646 → synopsis 700 → facts ~800 → buttons ~850 — while staying MEASURED rather
/// than fixed, because a real library's synopses are one to three lines and two of these bands come
/// and go with the metadata.
fn hero_chain(syn_h: f32, has_ratings: bool) -> HeroChain {
    let meta_y = TITLE_BOTTOM + META_DY;
    let ratings_y = meta_y + RATINGS_DY;
    // the synopsis hangs off whichever line is actually above it, so an item with no scores closes
    // the gap instead of leaving the band empty
    let syn_y = if has_ratings { ratings_y } else { meta_y } + SYN_DY;
    let facts_y = syn_y + syn_h.max(SYN_MIN_H) + FACTS_DY;
    // The "Directed by" credit used to claim a BAND of its own here, one air rung under the facts line
    // and reserved only when the item had directors. It is now the tail of the facts line itself
    // (`Details Screen.dc.html` runs date · extent · chips · credit as ONE row), so the chain has one
    // fewer conditional band and the button row hangs off the facts line for every item.
    HeroChain { meta_y, ratings_y, syn_y, facts_y, btn_y: facts_y + BTN_DY }
}

pub(crate) fn update(dt: f32) {
    // Apply a landed async season fetch: the episode row starts over on the new list (focus +
    // scroll to episode 0 — the remembered position belonged to the previous season). The ONE
    // exception is a refresh of the season already on screen (`KEEP_EP`), which lands the same list
    // with one field changed and must therefore hold the user's place rather than take it away.
    if metadata::pump_season() {
        let keep = take_kept_episode(); // reads metadata, so resolved before `view()` is borrowed
        let v = view();
        v.saved_col[2] = keep.unwrap_or(0);
        if keep.is_none() {
            v.ep_hscroll.jump(0.0); // a kept row is already scrolled where it belongs
        }
        if v.section == 2 {
            v.col = v.saved_col[2];
            v.card_scale.jump(1.0);
        }
    }
    pump_reveal(); // after pump_season: a landing season resets the episode focus this restores
    view().spin_ms += dt * 1000.0;
    // the flow top rides the measured hero: buttons bottom + one standardized gap (hero_layout)
    view().column.top = hero_layout(selected()).btn_y + CD + BTN_CONTENT_GAP;
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
    let hero_a = hero_alpha(scroll, HERO_FADE);
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
        // A SHOW's backdrop is the NEXT EPISODE's still, not the series art (`Details Screen.dc.html`,
        // and Apple's own show page). It is the frame belonging to the thing Play would start and the
        // blurb underneath describes, so the whole hero is about one episode; it also means the page
        // visibly changes as you work through a season, where series art never does. Falls back to the
        // series art whenever there is no episode yet (the season fetch is async) or it has no thumb.
        hero_episode()
            .map(|e| e.thumb.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| m.filter(|m| !m.art.is_empty()).map(|m| m.art.clone()))
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
    let hero_vis = hero_alpha(scroll, HERO_FADE);
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
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let mut drew_logo = false;
    if let Some((lt, ww, hh)) = crate::posters::logo_tex(rk, 680.0, 120.0) {
        p.tex(lt, Rect::new(tx, TITLE_BOTTOM - hh, ww, hh), 0.0, w_a);
        drew_logo = true;
    }
    if !drew_logo {
        let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
        let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
        if let Ok(t) = CString::new(title) {
            p.text(t.as_ptr(), tx, TITLE_BOTTOM - 68.0, theme::size::HERO, w_a, 0, 1);
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
    let ch = hero_layout(m);
    if let Ok(mc) = CString::new(parts.join("   \u{b7}   ")) {
        p.text(mc.as_ptr(), tx, ch.meta_y, theme::size::BODY, d_a, 0, 0);
    }

    // ---- review scores, on their own line under the meta row (see `draw_ratings`) ----
    draw_ratings(p, tx, ch.ratings_y);

    // ---- blurb: for a show the NEXT EPISODE's summary behind its bold `S2, E3 · Laura:` prefix (see
    // `hero_blurb`); its measured height is already folded into facts_y/btn_y by hero_layout ----
    let (lead, body) = hero_blurb(m);
    if !body.is_empty() {
        hero_synopsis(&body, &lead).draw(p, Rect::new(tx, ch.syn_y, HERO_TEXT_W, 0.0));
    }

    // ---- air date · season/episode counts (a show) or runtime (a movie) · media chips ----
    if let Some(d) = d {
        let (mut info, extent) = hero_facts(d);
        if let Some(extent) = extent {
            if !info.is_empty() {
                info.push_str("    \u{b7}    ");
            }
            info.push_str(&extent);
        }
        let mut bx = tx;
        if let Ok(ic) = CString::new(info) {
            let w = p.text(ic.as_ptr(), tx, ch.facts_y, theme::size::CAPTION, dim, 0, 0);
            if w > 0.0 {
                bx += w + theme::space::MD;
            }
        }
        bx += draw_media_badges(p, d, bx, ch.facts_y);
        // …and the crew credit closes the row. Inline rather than on a line of its own: it is the same
        // KIND of fine print as everything left of it, and a second row of it under the first read as a
        // stray sentence. Elided to whatever the right-aligned Starring column leaves.
        if let Some(names) = directors() {
            let budget = SCR_W - MARGIN_X - STARRING_W - theme::space::LG - bx;
            let line = crate::text::elide(
                &format!("    \u{b7}    Directed by {}", names.join(", ")),
                budget.max(0.0),
                theme::size::CAPTION,
                0,
                false,
            );
            if let Ok(dc) = CString::new(line) {
                p.text(dc.as_ptr(), bx, ch.facts_y, theme::size::CAPTION, dim, 0, 0);
            }
        }
    }

    // ---- buttons (btn_y from hero_layout) ----
    draw_buttons(p, env, ch.btn_y);

    // ---- "Starring …" — a right-aligned WRAPPING block in its own column, level with the action row.
    // It reads as a caption to the hero rather than a control because the word "Starring" is a rung
    // dimmer than the names it introduces, which is the mock's own device for keeping a full-size line
    // of type from competing with the Play pill beside it. ----
    if let Some(d) = d {
        if !d.cast.is_empty() {
            let names: Vec<String> = d.cast.iter().take(3).map(|c| c.tag.clone()).collect();
            // One colour for the whole run: the mock sets the word "Starring" a rung dimmer than the
            // names, which needs a RIGHT-aligned lead run, and `TextView::lead` is left-aligned only
            // (it anchors at the column's left edge, which on a right-aligned block would strand the
            // word far from the names it introduces).
            let line = format!("Starring {}", names.join(", "));
            TextView::new(&line, theme::size::CAPTION, theme::TEXT_TERTIARY)
                .leading(STARRING_LEAD)
                .max_lines(2)
                .h(crate::ui::label::HAlign::Right)
                .draw(p, Rect::new(SCR_W - MARGIN_X - STARRING_W, ch.btn_y + STARRING_DY, STARRING_W, 0.0));
        }
    }
}

/// The hero facts line's `(date, middle clause)` — and it describes **whatever the hero is about**,
/// which is the same switch [`hero_episode`] makes for the backdrop and the blurb.
///
/// * Hero is about an EPISODE (a started show) → *that episode's* air date and runtime.
/// * Hero is about the SERIES (an unplayed show) → the series' first-aired date and its extent,
///   `2 seasons, 20 episodes`.
/// * A movie → its own date and runtime.
///
/// One subject per hero state, top to bottom. The series extent used to sit here unconditionally, so
/// on a started show the block changed subject twice in three lines — an episode blurb, then a series
/// fact, then a button acting on the episode again. A show's extent is series information and belongs
/// with the series; the season tabs directly below already say how many there are.
///
/// A show record's own `duration` is deliberately never used: PMS reports one EPISODE's runtime there
/// (when it sends it at all), which on a series reads as though the whole show ran 53 minutes. The
/// extent instead comes from the season list we already hold, so this needs no extra request — and an
/// episode total of 0 means no season carried a `leafCount` (PMS omits the key rather than sending 0),
/// so the clause degrades to the season count rather than claiming "0 episodes".
fn hero_facts(d: &metadata::Detail) -> (String, Option<String>) {
    if let Some(ep) = hero_episode() {
        let runtime = (ep.dur_ms >= 60_000).then(|| crate::ui::fmt::dur_long(ep.dur_ms));
        return (pretty_date(&ep.aired, 0), runtime);
    }
    let date = pretty_date(&d.aired, d.year);
    if d.is_show {
        let seasons = d.seasons.len();
        if seasons == 0 {
            return (date, None);
        }
        let eps: i64 = d.seasons.iter().map(|s| s.leaf_count).sum();
        let s_word = if seasons == 1 { "season" } else { "seasons" };
        let extent = if eps > 0 {
            let e_word = if eps == 1 { "episode" } else { "episodes" };
            format!("{seasons} {s_word}, {eps} {e_word}")
        } else {
            format!("{seasons} {s_word}")
        };
        return (date, Some(extent));
    }
    (date, (d.dur_ms >= 60_000).then(|| crate::ui::fmt::dur_long(d.dur_ms)))
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
    // cap-band centre of the line drawn at `text_y` (the inverse of text::text_vcenter_y), computed
    // once — every chip below sits on it
    let (cap_top, baseline) = crate::text::text_cap_band(theme::size::CAPTION, 0);
    let cy = text_y + (cap_top + baseline) * 0.5;
    // the FILLED chip: what the picture IS (its resolution class)
    for text in [crate::ui::fmt::resolution(&d.video_resolution, d.width, d.height)].into_iter().flatten() {
        // the gap separates chips, so it is never left dangling after the last one
        if bx > x {
            bx += theme::space::XS;
        }
        bx += crate::ui::widgets::badge(p, bx, cy, &text, None, crate::ui::widgets::BadgeStyle::Filled);
    }
    // …then the ACCESSIBILITY group, as keyline chips (`Details Screen.dc.html`): what the item
    // OFFERS, which is a different kind of claim from what it is, and the two chip styles are how the
    // row says so without a separator. Derived from the streams we already parsed. For a SHOW these
    // come from the episode `Detail` borrows its media from (see the struct's note), i.e. they describe
    // an episode's file rather than the series.
    //
    // **No `CC` chip** (owner call): the mock has one, but "has subtitles at all" is true of very
    // nearly every item in a library, so the chip was on constantly and told a viewer nothing. `SDH`
    // (a track flagged hearing-impaired) and `AD` (audio description) are rare and are the ones that
    // actually answer "is this watchable for me".
    let sdh = d.subs.iter().any(|s| s.sdh);
    let ad = d.audio.iter().any(|s| s.ad);
    for (on, label) in [(sdh, "SDH"), (ad, "AD")] {
        if !on {
            continue;
        }
        if bx > x {
            bx += theme::space::XS;
        }
        bx += crate::ui::widgets::keyline_chip(p, bx, cy, label, theme::TEXT_SECONDARY, theme::SURFACE_APP);
    }
    bx - x
}

/// `RatingArt` → the mask and brand tint that draw it. The mapping lives here, in the UI layer, so
/// `metadata::RatingArt` (which is parsed out of the server's `Rating.image` string) stays free of
/// `Icon`/colour — and so the *state* the server named is what picks the art, end to end.
fn rating_mark(art: metadata::RatingArt) -> crate::ui::widgets::RatingMark {
    use crate::ui::icons::Icon;
    use crate::ui::widgets::{MarkLayer, RatingMark, WORDMARK_IMDB, WORDMARK_TMDB};
    use metadata::RatingArt as A;

    // Each mark's colour layers, BACK TO FRONT — the order is the drawing, since they overlap.
    // `static` so the slices outlive the call; see `widgets::RatingMark::Glyph` for the one-path-per-
    // layer contract these obey.
    /// Ripe: red body, green calyx over it.
    static FRESH: &[MarkLayer] =
        &[(Icon::Tomato, theme::RATING_FRESH), (Icon::TomatoCalyx, theme::RATING_LEAF)];
    /// Certified Fresh: RT's SEAL — gold disc, red box, then the green calyx and banner. Not a
    /// variant of the fruit, which is why it shares no layer with [`FRESH`].
    static CERTIFIED: &[MarkLayer] = &[
        (Icon::TomatoCertifiedSeal, theme::RATING_SEAL),
        (Icon::TomatoCertifiedBox, theme::RATING_FRESH),
        (Icon::TomatoCertifiedGreen, theme::RATING_LEAF),
    ];
    /// Rotten: one layer. A splat is one substance, and a second hue on it reads as two splats.
    static ROTTEN: &[MarkLayer] = &[(Icon::TomatoRotten, theme::RATING_ROTTEN)];
    /// Upright tub: the audience score's POSITIVE art, so the tub shares the fresh red; gold corn.
    static UPRIGHT: &[MarkLayer] =
        &[(Icon::Popcorn, theme::RATING_FRESH), (Icon::PopcornKernels, theme::RATING_KERNEL)];
    /// Spilled tub: the rotten green, gold corn thrown clear of it.
    static SPILLED: &[MarkLayer] = &[
        (Icon::PopcornSpilled, theme::RATING_ROTTEN),
        (Icon::PopcornSpilledKernels, theme::RATING_KERNEL),
    ];

    match art {
        A::TomatoFresh => RatingMark::Glyph(FRESH),
        A::TomatoCertified => RatingMark::Glyph(CERTIFIED),
        A::TomatoRotten => RatingMark::Glyph(ROTTEN),
        A::PopcornUpright => RatingMark::Glyph(UPRIGHT),
        A::PopcornSpilled => RatingMark::Glyph(SPILLED),
        // …and the two logotype brands, whose marks are their NAMES (see `widgets::RatingMark`)
        A::Imdb => RatingMark::Wordmark(&WORDMARK_IMDB),
        A::Tmdb => RatingMark::Wordmark(&WORDMARK_TMDB),
    }
}

/// The review-score row — **its own line under the meta line**, flowed left→right from the text
/// margin and centred on the band a [`theme::size::LABEL`] score occupies (so the row sits on a real
/// text line rather than at a guessed y). A badge that would cross the right margin is dropped and
/// the flow stops there.
///
/// It used to trail the genre line inline, which put a tomato at a different x on every item and
/// gave the row whatever width the genres left over — on a three-genre show the fourth score fell off
/// the panel. `Details Screen.dc.html` gives it a line, and a line is what it needs: four marks in
/// four different shapes are a block to scan, not a clause. The band is only reserved when there is
/// at least one score (see [`hero_chain`]), so a title the agents found no reviews for closes the gap
/// instead of leaving a hole.
fn draw_ratings(p: Painter, x: f32, row_y: f32) {
    let ratings = match metadata::current() {
        Some(d) if !d.ratings.is_empty() => &d.ratings,
        _ => return,
    };
    let (cap_top, baseline) = crate::text::text_cap_band(theme::size::LABEL, 1);
    let cy = row_y + (cap_top + baseline) * 0.5;
    let mut bx = x;
    for r in ratings.iter() {
        let mark = rating_mark(r.art);
        let score = crate::ui::fmt::rating_score(r.art, r.value);
        let w = crate::ui::widgets::rating_badge_w(&mark, &score);
        if w <= 0.0 || bx + w > SCR_W - MARGIN_X {
            break;
        }
        crate::ui::widgets::rating_badge(p, bx, cy, &mark, &score);
        bx += w + RATING_ROW_GAP;
    }
}

fn draw_buttons(p: Painter, env: &Env, y: f32) {
    // One shared control family (widgets::Button/CircleButton, default ControlStyle::Accent — the
    // same as the info card): the focused control fills warm ACCENT with dark ink, the rest are
    // solid dark discs. Play reuses the pill Button; the ↺ disc restarts a resumable item from
    // 00:00; the check disc is the watched toggle (its glyph lights RESUME amber while the item is
    // marked watched). The discs are placed by an ACCUMULATED x, not per-control constants, because
    // the middle one comes and goes — see `hero_btns`. That accumulation IS `hero_btn_rect`, which
    // the pointer hit-test walks too, so no control can be drawn in one place and clicked in another.
    let focus = focus();
    let restart = has_restart();

    // The pill says what the press will DO. Home's hero says "Continue" — that row belongs to the
    // Continue Watching shelf, and the word is the shelf's. On an item page the control has a
    // sibling: "Resume" and the ↺ disc beside it are a PAIR, read together, the way every reference
    // client words it. The pill's width follows its label (`Button::pill_w`, the ONE pill width
    // formula, reached through `hero_pill_w` so the pointer measures the very same frame) — the
    // longer word gets its own air instead of being crammed into "Play"'s.
    let plabel = hero_pill_label();

    Button::new(plabel.as_ptr(), theme::size::BODY, hero_btn_rect(BTN_PLAY, y))
        .icon(crate::ui::icons::Icon::Play)
        .focused(focus == BTN_PLAY)
        .draw(env, p);
    if restart {
        let r = hero_btn_rect(BTN_RESTART, y);
        CircleButton::new(c"".as_ptr())
            .icon(crate::ui::icons::Icon::Restart)
            .at(r.x, r.y)
            .focused(focus == BTN_RESTART)
            .draw(env, p);
    }
    let watched = metadata::current().map(|d| d.watched).unwrap_or(false);
    let wf = focus == btn_watched();
    let wstyle = if watched && !wf {
        ControlStyle::Custom { fill: theme::CONTROL_IDLE_FILL, ink: theme::RESUME_FILL }
    } else {
        ControlStyle::Accent
    };
    let wr = hero_btn_rect(btn_watched(), y);
    CircleButton::new(c"".as_ptr())
        .icon(crate::ui::icons::Icon::Check)
        .style(wstyle)
        .at(wr.x, wr.y)
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
        // ±TAB_PAD, tabs advance by that content width + TAB_ADVANCE (see tabs_layout). The pill
        // fills the block height (TAB_ROW_H == the hero-button CD), so tabs and buttons read as one
        // control family. `tab_pill_rect` is that frame, shared with the pointer hit-test — a tab
        // widened by its watched tick must stay clickable across the whole pill it draws.
        let pill = tab_pill_rect(&lay, tab_y);
        if on_axis(pill.x - sx, pill.w, SCR_W, 0.0) {
            TabPill::new(lay.label.as_ptr(), theme::size::BODY, pill)
                .segment(selected)
                .plated() // no tab-bar track under these — the pills are their own ground
                .focused(focused)
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
        let x = strip_x(i, EP_W + EP_GAP);
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
        // Marks ON the still: the scrim, ONE state line on it, and the full-bleed progress bar. All
        // three track the SCALED card while it is popped.
        let cr = if popped { card.scaled(sc) } else { card };
        let st = ep_state(ep);
        // the scrim first, so the line reads over any frame; then the line; then the bar, which is the
        // one mark that belongs to the card's own bottom edge rather than to the scrim above it
        crate::ui::widgets::art_scrim(pe, cr, 12.0, EP_SCRIM_H, EP_SCRIM_A);
        ep_state_line(pe, cr, &st);
        if let Some(frac) = st.progress {
            // the SHARED bar — literally the same call a Continue Watching card makes
            crate::ui::widgets::progress_bar(pe, cr, 12.0, EP_BAR_H, frac);
        }
        // under-card metadata: kicker → title → full summary → air date + content rating. The summary
        // flows off the ACTUAL title height (1 or 2 lines) — nothing is reserved, so a 1-line title
        // doesn't leave a gap — and the fine-print date closes the block at the bottom. The block
        // height tracks the tallest episode via ep_meta_h.
        let ty = ep_y + EP_H + EP_META_TOP;
        let titc = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        // the blurb steps a rung brighter on the focused tile, as the title does — the mock moves both
        let sumc = if focused { theme::TEXT_SECONDARY } else { dimc };
        let date = pretty_date(&ep.aired, 0); // formatted once — feeds both the layout and the draw
        let (date_y, summary_y, _) = ep_meta_layout(&ep.title, !date.is_empty(), &ep.summary);
        if let Ok(ec) = CString::new(format!("EPISODE {}", ep.index)) {
            pe.text(ec.as_ptr(), x, ty, theme::size::CAPTION, dimc, 0, 1);
        }
        // title wraps to at most 2 lines (elided past that) — long titles used to run into the next card
        TextView::new(&ep.title, theme::size::BODY, titc)
            .bold()
            .leading(EP_TITLE_LEAD)
            .max_lines(2)
            .draw(pe, Rect::new(x, ty + EP_TITLE_DY, EP_W, 0.0));
        if !ep.summary.is_empty() {
            TextView::new(&ep.summary, theme::size::CAPTION, sumc)
                .leading(EP_SUMMARY_LEAD)
                .max_lines(EP_SUMMARY_MAXLINES)
                .draw(pe, Rect::new(x, ty + summary_y, EP_W, 0.0));
        }
        if !date.is_empty() {
            if let Ok(dc) = CString::new(date) {
                let w = pe.text(dc.as_ptr(), x, ty + date_y, theme::size::MICRO, dimc, 0, 0);
                // the episode's own content rating, as a fine-print keyline chip after the date — the
                // mock's `18+`. `keyline_chip`, not `badge`: this has to be the dimmest thing on the
                // tile, and at badge's band and weight it outweighed the episode title above it. `bg`
                // is the surface actually behind it, since the outline is a knockout.
                if !ep.rating.is_empty() {
                    let (ct, cb) = crate::text::text_cap_band(theme::size::MICRO, 0);
                    crate::ui::widgets::keyline_chip(
                        pe,
                        x + w + theme::space::SM,
                        ty + date_y + (ct + cb) * 0.5,
                        &ep.rating,
                        dimc,
                        theme::SURFACE_APP,
                    );
                }
            }
        }
    }
}

/// The glyph at the head of an episode still's state line — or none at all.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
enum EpGlyph {
    /// Never started: the amber play triangle. OK will start it from 00:00.
    Play,
    /// Finished: the amber watched disc. OK will play it again from 00:00.
    Watched,
    /// In progress: NO glyph. The bar underneath already says both where you are and that OK resumes,
    /// and the mock leaves the slot empty rather than adding a third mark that repeats it.
    None,
}

/// Everything an episode still says about its own playback state: one glyph, one label, and the
/// progress fraction for the bar (`None` = draw no bar).
struct EpState {
    glyph: EpGlyph,
    /// Runtime (`48 min`) normally; what is LEFT (`12 min left`) while in progress. Empty when the
    /// server sent no runtime — the line then draws its glyph alone rather than "0 min".
    label: String,
    progress: Option<f32>,
}

/// Resolve an episode's three playback states into ONE mark — the pure half of the still's state
/// line, split out because the *choice* is the behaviour while drawing it needs a font and a GL
/// context.
///
/// This is the whole progress vocabulary for a still, and it replaced two overlapping ones: an amber
/// bar or a corner check disc for STATE, plus a neutral duration capsule for the ACTION, which meant a
/// finished episode wore an amber check in one corner and `▶ 48 min` in the other and a Continue
/// Watching card wore neither of those things. `Details Screen.dc.html` folds all of it into one line:
///
/// | state | glyph | label | bar |
/// |---|---|---|---|
/// | never started | `▶` | runtime | — |
/// | in progress | — | time left | yes |
/// | watched | `✓` | runtime | — |
///
/// **In progress wins over watched**, and the precedence matters because PMS reports both: it keeps a
/// resume point on an episode that was finished and then re-started, so `watched` and a live
/// `viewOffset` coexist on the wire. Being part-way through a re-watch is what the viewer is actually
/// doing, so that is what the tile says. `resume_ms >= dur_ms` is NOT in progress — that is a finished
/// episode whose offset was never cleared, and treating it as in-progress drew a 100%-full bar that
/// looked like a rendering bug.
///
/// Both strings come from the same `ui::fmt` FAMILY — `dur_long`'s "48 min" and `time_left`'s "12 min
/// left" spell their units identically, where `dur_short`'s "48m" beside a neighbouring "12 min left"
/// would put two spellings of a minute in one filmstrip.
fn ep_state(ep: &metadata::Episode) -> EpState {
    let has_dur = ep.dur_ms > 0;
    let in_progress = has_dur && ep.resume_ms > 0 && ep.resume_ms < ep.dur_ms;
    if in_progress {
        return EpState {
            glyph: EpGlyph::None,
            label: crate::ui::fmt::time_left(ep.dur_ms - ep.resume_ms),
            progress: Some((ep.resume_ms as f32 / ep.dur_ms as f32).clamp(0.0, 1.0)),
        };
    }
    EpState {
        glyph: if ep.watched { EpGlyph::Watched } else { EpGlyph::Play },
        label: if has_dur { crate::ui::fmt::dur_long(ep.dur_ms) } else { String::new() },
        progress: None,
    }
}

/// Draw [`ep_state`]'s glyph-and-label line at the still's bottom-left, directly on the artwork —
/// **no capsule behind it**; `widgets::art_scrim` (drawn first by the caller) is what makes that safe.
/// `card` is the rect actually drawn, i.e. the SCALED one while the tile is popped, so the line rides
/// the focus pop without resizing.
fn ep_state_line(p: Painter, card: Rect, st: &EpState) {
    let sz = theme::size::CAPTION;
    // the label's cap band sits on the line's baseline inset, so the glyph can centre on it
    let (ct, cb) = crate::text::text_cap_band(sz, 0);
    let ty = card.y + card.h - EP_LINE_BOT - cb;
    let mut lx = card.x + EP_MARK_INSET;
    let icon = match st.glyph {
        EpGlyph::Play => Some(crate::ui::icons::Icon::Play),
        EpGlyph::Watched => None, // a filled disc, not a mask — drawn below
        EpGlyph::None => None,
    };
    let cy = ty + (ct + cb) * 0.5;
    match st.glyph {
        EpGlyph::Watched => {
            crate::ui::widgets::watched_disc(
                p,
                Rect::new(lx, cy - EP_GLYPH_D * 0.5, EP_GLYPH_D, EP_GLYPH_D),
            );
            lx += EP_GLYPH_D + EP_LINE_GAP;
        }
        _ => {
            if let Some(icon) = icon {
                crate::ui::icons::draw(
                    p,
                    icon,
                    Rect::new(lx, cy - EP_GLYPH_D * 0.5, EP_GLYPH_D, EP_GLYPH_D),
                    theme::RESUME_FILL,
                );
                lx += EP_GLYPH_D + EP_LINE_GAP;
            }
        }
    }
    if st.label.is_empty() {
        return;
    }
    // a watched episode's runtime steps back a rung: the disc beside it is the statement, and at full
    // primary the two competed
    let col = if st.glyph == EpGlyph::Watched { theme::TEXT_SECONDARY } else { theme::TEXT_PRIMARY };
    if let Ok(lc) = CString::new(st.label.as_str()) {
        p.text(lc.as_ptr(), lx, ty, sz, col, 0, 0);
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
    card_row::strip(
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
        |_| None,
        |i| card_row::TileLabel::title(&d.related[i].title),
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
    card_row::strip(
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
        |_| None, // resume: a person is not an item with a playhead
        // every credit is captioned by `extra`, focused or not — so no TileLabel
        |_| card_row::TileLabel::default(),
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
                if let Some((rk, watched)) = metadata::current().map(|d| (d.rk.clone(), d.watched)) {
                    if watched {
                        crate::plex::client().unscrobble(&rk);
                    } else {
                        crate::plex::client().scrobble(&rk);
                    }
                    // the page re-reads itself, season and all. No keep-focus target: the press was
                    // on the hero, so there is no filmstrip position to protect.
                    refresh_view_state("");
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
                // The episode the HERO is about — BY VALUE, not by index: it comes from the server's
                // on-deck hub and may live in a season the strip has not loaded, so there is no index
                // into `d.episodes` that names it. A show presenting itself (unplayed/finished) has
                // none, and there Play starts the loaded season's first episode.
                match hero_episode() {
                    Some(ep) => metadata::current().is_some_and(|d| play_episode(d, ep)),
                    None => play_episode_at(0),
                }
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
        4 => {
            // Credits: OK opens that person's page — **cast AND crew alike**. The row indexes the
            // combined credit space (`credit(i)` walks every actor then every crew member), so
            // this MUST resolve through `credit`, not `cast[i]`: a director's tile is at an index
            // past the actors, and reading `cast[i]` there would either open the wrong actor or
            // find nothing. A director is a person with a `tagKey` like any other, `crew_credits`
            // carries the id/guid through, and `/library/people/{id}/media` answers for them the
            // same way — so the tile that shows a director opens the director.
            //
            // The page needs nothing this row doesn't already hold: the tag carries the name, the
            // headshot AND the personId, so it mounts on that header and only the rest is fetched.
            // A row PMS sent with neither id form is simply not actionable (`person_key` empty)
            // and raises no request, so app.rs leaves the route here.
            //
            // BOTH ids go through, because they address different services: `person_key` is the
            // LOCAL id for `/library/people/{id}/media`, while `tag_key` (the guid) is the only
            // one plex.tv's biography provider answers to. Passing one for the other 404s.
            if let Some(c) = metadata::current().and_then(|d| d.credit(col.max(0) as usize)) {
                let key = c.person_key();
                if !key.is_empty() {
                    crate::ui::person::open(&key, &c.tag_key, &c.tag, &c.thumb);
                }
            }
            false
        }
        _ => false,
    }
}

// ---- the episode filmstrip's press-and-hold context menu (ui/item_menu.rs) -------------------
// Three seams, because the menu is screen-agnostic by design: it only ever REPORTS an Action, and
// `app.rs` performs it. detail.rs supplies the item, the rect to anchor beside, and the two
// operations an Action can turn into here.

/// The focused episode's identity for the context menu — its ratingKey and whether it is watched
/// (`viewCount >= 1`, which is EXACT for a leaf, so the row can be a true toggle). `None` unless
/// the filmstrip actually holds focus.
///
/// Also `None` while a season fetch is in flight: the row is still drawing the PREVIOUS season's
/// episodes (dimmed, under a spinner) while `cur_season` already names the new one, so a menu built
/// from it would offer to mark the wrong episode watched — the same trap `play_episode_at` refuses
/// the press for.
pub(crate) fn focused_episode() -> Option<(String, bool)> {
    if metadata::season_loading() {
        return None;
    }
    let v = view();
    if v.section != 2 {
        return None;
    }
    let ep = metadata::current()?.episodes.get(v.col.max(0) as usize)?;
    Some((ep.rk.clone(), ep.watched))
}

/// The focused episode still's DRAWN rect, in screen space — what the popover anchors BESIDE.
///
/// Derived from the very formulas `draw_episodes` walks (`strip_x` for the column, the live
/// h-scroll, `section_screen_top` for the band, the cell's own pop spring for the scale), never
/// recorded, so the panel hangs off the tile the user is looking at at the size it is really drawn.
/// The press dip is deliberately NOT folded in: opening the menu cancels the press, so the cell is
/// on its way back to the resting popped scale by the frame the anchor is captured.
pub(crate) fn focused_episode_rect() -> Option<Rect> {
    if view().section != 2 {
        return None;
    }
    let i = view().col.max(0) as usize;
    if i >= n_items(2).max(0) as usize {
        return None;
    }
    // read one field at a time (as `draw_episodes` does) rather than holding a `&mut DetailView`
    // across `section_screen_top`, which reaches for the same static
    let top = section_screen_top(2)?;
    let x = strip_x(i, EP_W + EP_GAP) - view().ep_hscroll.pos;
    // episodes past the spring cap are drawn at the focus scale without animating — same fallback
    let sc = view().ep_scale.get(i).map(|s| s.pos).unwrap_or(crate::ui::widgets::CARD_FOCUS_SCALE);
    Some(Rect::new(x, top, EP_W, EP_H).scaled(sc))
}

/// Re-read the mounted item from the server after something changed its view state, KEEPING the
/// season the user was browsing.
///
/// Shared by the hero's watched toggle and by the filmstrip's context menu, which mark different
/// things watched — the whole show versus one of its episodes — but both need this page to be
/// showing the truth afterwards (the tab ticks, the episode checks, the resume bars and the hero's
/// own toggle all read the refetched item).
/// `keep_ep` is the episode this refresh is ABOUT, if any (the filmstrip menu passes the one it
/// just toggled; the hero passes nothing) — see [`KEEP_EP`] for why the row would otherwise scroll
/// itself away from the tile the user just acted on.
pub(crate) fn refresh_view_state(keep_ep: &str) {
    let Some((rk, season)) = metadata::current().map(|d| (d.rk.clone(), d.cur_season)) else {
        return;
    };
    // BLOCKING: the season restore below reads the refreshed item, and a deferred landing would
    // install cur_season 0 on top of it — clobbering the season the user was browsing.
    metadata::load_detail_now(&rk);
    if season != 0 {
        // Armed BEFORE the request, so a landing can never beat the latch into place. Season 0
        // needs neither: `load_detail_now` already leaves the page on it, so no fetch lands and
        // nothing resets the row.
        if !keep_ep.is_empty() {
            unsafe { *addr_of_mut!(KEEP_EP) = Some((keep_ep.to_string(), season)) };
        }
        metadata::load_season(season); // keep the season the user was browsing
    }
}

/// The episode the filmstrip must land back on when the next season fetch lands, and the season it
/// belongs to.
///
/// Armed by [`refresh_view_state`] alone, because that refetch is the one case that re-requests the
/// season already on screen. `update`'s "a landed season starts the row over" reset is right for a
/// genuine tab switch — it is a different list — but here the SAME list lands with one field
/// changed, and starting over would scroll the filmstrip off the very tile whose state the user
/// just changed, which reads as the page losing their place as a *result* of their action.
///
/// One-shot, and only honoured for the season it names (see [`take_kept_episode`]): a tab switch
/// that overtakes the refresh still starts its own season over.
static mut KEEP_EP: Option<(String, usize)> = None;

/// Consume a pending keep-focus request, resolved to an episode INDEX in the list that just landed.
/// `None` when there is none, when the landed season is not the one it was armed for, or when the
/// episode is no longer in the list — in every one of those the row falls back to starting over.
///
/// Deliberately consuming even when it does not resolve: a latch that survived a landing it could
/// not be applied to would sit there steering some later, unrelated season switch.
fn take_kept_episode() -> Option<c_int> {
    let (rk, season) = unsafe { (*addr_of_mut!(KEEP_EP)).take()? };
    let d = metadata::current()?;
    if d.cur_season != season {
        return None;
    }
    d.episodes.iter().position(|e| e.rk == rk).map(|i| i as c_int)
}

/// Play the loaded season's episode `rk` ignoring its resume point — the filmstrip menu's "Play
/// from Start". Reports whether playback should start, exactly like [`on_ok`], and leaves the
/// resume in [`last_resume_ns`] for `app.rs` to read (which [`commit_resume`] has just zeroed).
///
/// Resolved by rk rather than by the focus index: the menu captured its target when it opened, and
/// a pointer hover walking the popover's rows must not be able to retarget the press.
pub(crate) fn play_episode_rk_from_start(rk: &str) -> bool {
    let Some(i) = metadata::current().and_then(|d| d.episodes.iter().position(|e| e.rk == rk)) else {
        return false;
    };
    let started = play_episode_at(i as c_int);
    commit_resume(true);
    started
}

/// The rk the person page must come BACK to — the detail item currently mounted here. Read by
/// app.rs when it routes into the person page, so the page owns its own return target instead of
/// app.rs having to reconstruct one after an intervening navigation.
pub(crate) fn mounted_rk() -> String {
    metadata::current().map(|d| d.rk.clone()).unwrap_or_else(|| view().mounted_rk.clone())
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
    play_episode(d, ep)
}

/// Start `ep` — the by-value half of [`play_episode_at`], so an episode that is NOT in the loaded
/// season's list (the hero's on-deck one) can still be played.
fn play_episode(d: &metadata::Detail, ep: &metadata::Episode) -> bool {
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
            // the shared chip leaf (filled style) — cy = chip top + half its height
            let cy = ay + crate::ui::widgets::BADGE_H * 0.5;
            crate::ui::widgets::badge(p, ax, cy, label, None, crate::ui::widgets::BadgeStyle::Filled);
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
    /// A credit with a real `personId` — crew rows carry one on the wire exactly like actors, and
    /// it is what makes the tile open a person page.
    fn credit(name: &str, job: &str) -> metadata::Cast {
        credit_id(name, job, 0)
    }
    fn credit_id(name: &str, job: &str, id: i64) -> metadata::Cast {
        metadata::Cast {
            tag: name.to_string(),
            role: job.to_string(),
            thumb: String::new(),
            id,
            tag_key: String::new(),
        }
    }

    /// The crew credit takes **no vertical space**: it is the tail of the facts line, not a band of its
    /// own, so the action row hangs off the facts line for every item whether or not PMS sent
    /// `Director[]`. It used to claim a reserved 40px band, which is what put "Directed by" on a line
    /// by itself on the movie page — the mock runs date · extent · chips · credit as ONE row.
    #[test]
    fn the_crew_credit_costs_the_chain_no_vertical_space() {
        for syn_h in [0.0f32, 108.0] {
            for ratings in [false, true] {
                let ch = hero_chain(syn_h, ratings);
                assert_eq!(
                    ch.btn_y,
                    ch.facts_y + BTN_DY,
                    "the action row hangs off the facts line, credit or no credit"
                );
            }
        }
    }

    /// What a show's hero is ABOUT, which is one decision every part of it reads — backdrop still,
    /// blurb, facts line, Resume label, Play target. It comes from the SERVER's on-deck episode rather
    /// than a search of the loaded season, and that is the bug this replaced: the client holds one
    /// season at a time, so a client-side "next episode" changed subject every time you browsed to a
    /// different season TAB.
    ///
    /// Two states have no next episode, and the server only covers one of them — verified live across
    /// six shows (2026-07-30): a FINISHED show gets no `OnDeck` at all, but an UNPLAYED one is offered
    /// `S1E1 off=0`, so "something is on deck" is not the same question as "this show is underway".
    #[test]
    fn a_shows_hero_is_about_the_servers_on_deck_episode_or_the_series() {
        let _serial = crate::testlock::serial();
        let season = |viewed: i64| metadata::Season {
            rk: String::new(),
            index: 1,
            title: String::new(),
            leaf_count: 10,
            viewed_leaf_count: viewed,
        };
        let ep = |season_no: i64, index: i64, resume_ms: i64| metadata::Episode {
            index,
            season: season_no,
            resume_ms,
            dur_ms: 60 * 60_000,
            summary: format!("S{season_no}E{index} blurb"),
            thumb: format!("/thumb/s{season_no}e{index}"),
            ..Default::default()
        };
        let show = |seasons: Vec<metadata::Season>, on_deck, loaded: Vec<metadata::Episode>| {
            let mut d = item(&[]);
            d.is_show = true;
            d.summary = "the series premise".to_string();
            d.seasons = seasons;
            d.on_deck = on_deck;
            d.episodes = loaded;
            d
        };

        // The case that drove the change: on deck is S2E2, and the LOADED season is 1 — the hero must
        // still be about S2E2, because browsing tabs is navigation, not a change of subject.
        metadata::install_for_test(Some(show(
            vec![season(10), season(1)],
            Some(ep(2, 2, 10 * 60_000)),
            vec![ep(1, 1, 0), ep(1, 2, 0)],
        )));
        let shown = hero_episode().expect("a started show is about its on-deck episode");
        assert_eq!(shown.season, 2, "the on-deck season, NOT the tab being browsed");
        assert_eq!(shown.index, 2);
        assert_eq!(shown.thumb, "/thumb/s2e2", "…which is also the backdrop the hero draws");
        assert_eq!(hero_resume_ns(), metadata::resume_ns(10 * 60_000, 60 * 60_000), "and Resume reads ITS offset");

        // FINISHED: the server sends no OnDeck, so the hero presents the series for free
        metadata::install_for_test(Some(show(vec![season(10)], None, vec![ep(1, 1, 0)])));
        assert!(hero_episode().is_none(), "a finished show presents itself");
        assert_eq!(hero_resume_ns(), 0, "…and its Play starts from 0, so no restart disc");

        // UNPLAYED: the server DOES offer S1E1, and `show_started` is what rejects it
        metadata::install_for_test(Some(show(vec![season(0)], Some(ep(1, 1, 0)), vec![ep(1, 1, 0)])));
        assert!(hero_episode().is_none(), "an unplayed show presents itself, on-deck offer notwithstanding");

        // …and a live offset alone makes it underway, with no watched leaf anywhere
        metadata::install_for_test(Some(show(vec![season(0)], Some(ep(1, 1, 90_000)), vec![ep(1, 1, 90_000)])));
        assert!(hero_episode().is_some(), "a part-watched episode means the show is started");

        metadata::install_for_test(None);
    }

    /// The ratings row is the chain's OTHER conditional band, and it has the same failure modes as
    /// the credit line in the opposite direction: unreserved, four brand marks would print over the
    /// synopsis; always reserved, every title the metadata agents found no reviews for wears a 50px
    /// hole between its genres and its blurb. Everything BELOW it moves as one, and everything above
    /// it stays put.
    #[test]
    fn the_ratings_band_is_reserved_when_there_are_scores_and_never_otherwise() {
        for syn_h in [0.0f32, 108.0] {
            let none = hero_chain(syn_h, false);
            let some = hero_chain(syn_h, true);

            assert_eq!(some.meta_y, none.meta_y, "the meta line is above the band; it cannot move");
            assert_eq!(
                none.syn_y,
                none.meta_y + SYN_DY,
                "no scores: the synopsis hangs straight off the meta line"
            );
            assert_eq!(
                some.syn_y,
                some.ratings_y + SYN_DY,
                "with scores it hangs off the ratings row instead"
            );
            // the whole lower block travels together, by exactly the band's height
            let shift = some.syn_y - none.syn_y;
            assert_eq!(shift, RATINGS_DY, "the band is worth one RATINGS_DY and nothing else");
            assert_eq!(some.facts_y - none.facts_y, shift, "the facts line travels with it");
            assert_eq!(some.btn_y - none.btn_y, shift, "so does the action row");
        }
    }

    /// The chain reproduces the mock's absolute ys on the blurb length the mock was drawn at (two
    /// lines at `SYN_LEAD`). Pinned as ABSOLUTES on purpose: every pitch above is individually
    /// asserted elsewhere, but only their sum says whether the page still looks like the design, and
    /// that sum is the thing a later "just nudge one gap" quietly breaks.
    #[test]
    fn a_two_line_blurb_lands_the_chain_on_the_mockups_own_ys() {
        let ch = hero_chain(2.0 * SYN_LEAD, true);
        assert_eq!(ch.meta_y, 596.0, "meta line");
        assert_eq!(ch.ratings_y, 646.0, "review scores");
        assert_eq!(ch.syn_y, 700.0, "synopsis");
        assert_eq!(ch.facts_y, 796.0, "date/counts line (the mock's 800, on our measured blurb)");
        assert_eq!(ch.btn_y, 846.0, "action row (the mock's 850)");
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

    /// OK on the credits shelf must resolve through the COMBINED index space, not `cast[i]`.
    ///
    /// This is a rebase hazard, not a hypothetical: the person-page arm was written against a
    /// shelf that held actors only, and the crew fold later made `credit(i)` run every actor and
    /// THEN every crew member. Indexing `cast[i]` at a crew tile silently opens the wrong actor
    /// (or, past the end, nobody) while the tile under the focus ring shows a director. Crew rows
    /// carry a `personId` like any other tag, so the correct behaviour is that a director's tile
    /// opens the director.
    #[test]
    fn ok_on_a_crew_tile_opens_that_crew_members_page_not_an_actor_at_the_same_index() {
        let _serial = crate::testlock::serial();
        let mut d = item(&[]);
        d.cast = vec![credit_id("Anne Actor", "Herself", 11), credit_id("Ben Actor", "Himself", 12)];
        d.crew = vec![credit_id("Dana Director", "Director", 99)];
        mount(Some(d), 0);

        // index 2 is PAST both actors — it is the crew member
        let v = view();
        v.section = 4;
        v.col = 2;
        assert!(!on_ok(), "a credit tile never starts playback");
        let p = crate::person::current().expect("the person store opened");
        assert_eq!(p.key, "99", "the crew tile opened the actor at cast[2] (or nothing) instead");
        assert_eq!(p.name, "Dana Director");

        // and an ordinary actor index still resolves to that actor
        view().col = 1;
        assert!(!on_ok());
        assert_eq!(crate::person::current().unwrap().key, "12");

        crate::person::close();
        mount(None, 0);
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
    /// A show whose ON-DECK episode carries `ep_resume_ms`. The hero's resume comes from the server's
    /// on-deck hub, not from the loaded season's first episode (see `hero_episode`), so a fixture that
    /// only fills `episodes` describes a show the hero would present as a SERIES.
    fn show(ep_resume_ms: i64, dur_ms: i64) -> Detail {
        let ep = || Episode { rk: "e1".into(), dur_ms, resume_ms: ep_resume_ms, ..Default::default() };
        Detail {
            rk: "s1".into(),
            is_show: true,
            episodes: vec![ep()],
            on_deck: Some(ep()),
            // any watched leaf makes it a STARTED show, which is what lets the hero be about an
            // episode at all — an untouched show presents itself however deep its on-deck offset
            seasons: vec![metadata::Season {
                rk: String::new(),
                index: 1,
                title: String::new(),
                leaf_count: 10,
                viewed_leaf_count: 1,
            }],
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

        // a show page's Play starts its ON-DECK episode, so the row reads THAT leaf
        mount(Some(show(0, 2_700_000)), 0);
        assert_eq!(hero_btns(), 2, "show whose on-deck episode has not been started");
        mount(Some(show(600_000, 2_700_000)), 0);
        assert_eq!(hero_btns(), 3, "show whose on-deck episode is part-watched");
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

    // ---- pointer hit-tests: the rects the pointer answers with must be the ones the painter drew.
    // Detail's twins of home's "the pointer hit column matches the drawn card at every snap phase".

    /// The pointer's inverse must land on the tile the painter drew — for detail's three horizontal
    /// `strip_x` and the shared `card_row::strip` compute a tile's left edge independently —
    /// detail's off `MARGIN_X`, the component's off `sty.margin_x`. They agree only because every
    /// `RowStyle` sets `margin_x: MARGIN_X`; if one ever stops, the shelves would DRAW at one x and
    /// HIT-TEST at another, which is silent and looks like a focus bug rather than a layout one.
    #[test]
    fn strip_x_matches_the_shared_shelf_formula() {
        for sty in [&RowStyle::HOME, &RowStyle::CAST] {
            assert_eq!(sty.margin_x, MARGIN_X, "a RowStyle margin drifted from detail's strip_x");
        }
    }

    /// strips. Both sides go through `strip_x`, so this pins the round trip at every horizontal
    /// scroll phase, including the gutter, which belongs to no tile at all.
    #[test]
    fn pointer_hit_index_matches_the_drawn_tile_in_every_strip() {
        // (pitch, tile width) for the episode filmstrip, the Related shelf and the Cast shelf
        for &(pitch, w) in &[(EP_W + EP_GAP, EP_W), (REL_W + REL_GAP, REL_W), (CAST_SLOT, CAST_D)] {
            assert!(pitch > w + 1.0, "a strip's pitch must leave a real gutter ({pitch} vs {w})");
            for &sx in &[0.0f32, 137.0, 1024.0] {
                for i in 0..12usize {
                    let x = strip_x(i, pitch) - sx; // the DRAWN left edge at this scroll
                    for probe in [x + 1.0, x + w * 0.5, x + w - 1.0] {
                        assert_eq!(
                            strip_at(probe, sx, 12, pitch, w),
                            Some(i),
                            "clicking drawn tile {i} (pitch={pitch}, scroll={sx}) must hit it"
                        );
                    }
                }
                // the gutter between two tiles is a miss, not the neighbour
                assert_eq!(strip_at(strip_x(1, pitch) - sx - 1.0, sx, 12, pitch, w), None);
            }
        }
    }

    /// The hero action row, on a row whose control SET changes under the pointer. With a resume
    /// point the ↺ disc takes index 1 and the watched toggle slides to 2, so a hit-test built on
    /// per-control constants would answer one control out of step for exactly the items a pointer
    /// user is most likely to open. The invariant is therefore the ACCUMULATION `hero_btn_rect`
    /// performs — the pill at the margin, every later control one `CD + CGAP` past the last —
    /// asserted whatever the pill measures (the host has no fonts, so `pill_w` floors at `PW`
    /// there and carries real metrics on the TV; the walk holds either way).
    #[test]
    fn hero_action_row_hit_matches_the_drawn_controls_at_every_set_size() {
        let _serial = crate::testlock::serial();
        let y = 812.0; // any row top; the live one comes from hero_chain

        // (item, control count, the pill width its label measures to). The widths stand in for
        // `hero_pill_w` — "Resume" is the wider word, and the row must accumulate off whichever it
        // is rather than off a constant.
        for (d, want, pw) in
            [(movie(0, 7_200_000), 2, PW), (movie(1_800_000, 7_200_000), 3, PW + 62.0)]
        {
            mount(Some(d), 0);
            assert_eq!(hero_btns(), want, "the control set this case means to exercise");

            let pill = hero_btn_rect_w(BTN_PLAY, y, pw);
            assert_eq!(pill.x, MARGIN_X, "the pill starts at the margin");
            assert_eq!((pill.w, pill.h), (pw, CD), "the pill IS the measured frame");
            assert!(!pill.contains(MARGIN_X - 1.0, y + CD * 0.5), "the left margin is not the pill");

            let mut prev = pill;
            for i in (BTN_PLAY + 1)..hero_btns() {
                let r = hero_btn_rect_w(i, y, pw);
                assert_eq!((r.w, r.h), (CD, CD), "control {i} is a CD disc");
                assert_eq!(r.x, prev.x + prev.w + CGAP, "control {i} accumulates past its neighbour");
                assert!(
                    !prev.contains(r.cx(), r.cy()) && !r.contains(prev.cx(), prev.cy()),
                    "controls {} and {i} must each answer only for themselves",
                    i - 1
                );
                // the gutter between two controls belongs to neither
                let gutter = prev.x + prev.w + CGAP * 0.5;
                assert!(!prev.contains(gutter, y + CD * 0.5) && !r.contains(gutter, y + CD * 0.5));
                prev = r;
            }
            // the watched toggle is always the row's LAST control — it shifts, it is not displaced
            assert_eq!(hero_btn_rect_w(btn_watched(), y, pw).x, prev.x, "the toggle ends the row");
        }

        mount(None, 0);
    }

    /// Marking an episode watched from the filmstrip's context menu refetches the page, which
    /// re-requests the season already on screen — and `update`'s landing handler starts the episode
    /// row over, which is right for a tab switch and wrong here. The latch is what tells the two
    /// apart, and every way it can be wrong ends in the SAME fallback (start over), never in a
    /// stale latch steering a later, unrelated season switch.
    #[test]
    fn a_watched_toggle_holds_the_filmstrips_place_and_a_stale_latch_never_steers_a_tab_switch() {
        let _serial = crate::testlock::serial();
        let season2 = |cur_season| Detail {
            rk: "s1".into(),
            is_show: true,
            cur_season,
            episodes: vec![
                Episode { rk: "e1".into(), ..Default::default() },
                Episode { rk: "e2".into(), ..Default::default() },
                Episode { rk: "e3".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let arm = |rk: &str, season| unsafe { *addr_of_mut!(KEEP_EP) = Some((rk.to_string(), season)) };

        // the ordinary case: the same season lands back, and the row holds the episode it acted on
        metadata::set_current_for_test(Some(season2(2)));
        arm("e3", 2);
        assert_eq!(take_kept_episode(), Some(2), "the row must land back on the tile that changed");
        assert_eq!(take_kept_episode(), None, "…and the latch is one-shot");

        // a tab switch overtook the refresh: a DIFFERENT season landed, so it starts over
        metadata::set_current_for_test(Some(season2(3)));
        arm("e3", 2);
        assert_eq!(take_kept_episode(), None, "a latch never applies to a season it wasn't armed for");
        assert_eq!(unsafe { (*addr_of!(KEEP_EP)).is_none() }, true, "and is consumed all the same");

        // the episode left the list (an unwatched-only filter, a server-side edit): start over
        metadata::set_current_for_test(Some(season2(2)));
        arm("gone", 2);
        assert_eq!(take_kept_episode(), None);

        // opening another page drops a latch the previous item armed
        arm("e3", 2);
        reset_view_state(view());
        assert_eq!(take_kept_episode(), None, "a new page must not inherit the old page's latch");

        mount(None, 0);
    }

    /// `ep_state` is the still's WHOLE progress vocabulary — one glyph, one label, one optional bar
    /// per episode — and it replaced two overlapping ones. Every row of the table in its doc comment
    /// is pinned here, because each is a different way to lie about the same episode: a resuming tile
    /// showing the full runtime misstates what is left, a finished one showing ▶ claims it is new, and
    /// an episode the server sent no duration for must not say "0 min".
    #[test]
    fn an_episode_still_resolves_its_three_states_into_one_mark() {
        let ep = |dur_ms, resume_ms| Episode { dur_ms, resume_ms, ..Default::default() };

        // never started: the play glyph, the whole runtime, no bar
        let st = ep_state(&ep(48 * 60_000, 0));
        assert_eq!(st.glyph, EpGlyph::Play, "an unstarted episode leads with ▶");
        assert_eq!(st.label, "48 min");
        assert!(st.progress.is_none(), "nothing to show progress of");

        // in progress: NO glyph (the bar says it), what is LEFT rather than the runtime, and the bar
        let st = ep_state(&ep(60 * 60_000, 15 * 60_000));
        assert_eq!(st.glyph, EpGlyph::None, "the bar is the mark; a glyph would repeat it");
        assert_eq!(st.label, "45 min left", "the remainder, not the runtime");
        assert_eq!(st.progress, Some(0.25), "a quarter in");

        // the two labels must spell a minute the SAME way — they sit on neighbouring tiles of one
        // filmstrip, and `dur_short`'s "48m" beside `time_left`'s "45 min left" is the exact drift
        // `ui::fmt` exists to prevent
        assert!(st.label.contains(" min"), "{}", st.label);

        // watched: the disc, the runtime (it is startable again), no bar
        let mut done = ep(48 * 60_000, 0);
        done.watched = true;
        let st = ep_state(&done);
        assert_eq!(st.glyph, EpGlyph::Watched, "a finished episode leads with the check disc");
        assert_eq!(st.label, "48 min", "and still states how long it runs");
        assert!(st.progress.is_none());

        // PRECEDENCE: PMS keeps a resume point on an episode that was finished and re-started, so the
        // wire says watched AND in-progress. Being part-way through the re-watch is what the viewer is
        // doing, so in-progress wins.
        let mut rewatch = ep(60 * 60_000, 6 * 60_000);
        rewatch.watched = true;
        let st = ep_state(&rewatch);
        assert_eq!(st.glyph, EpGlyph::None, "a re-started watched episode reads as in progress");
        assert_eq!(st.progress, Some(0.1));

        // …but an offset AT or PAST the runtime is a finished episode whose viewOffset was never
        // cleared, not a 100%-complete in-progress one (which drew a full bar that looked like a bug)
        let mut stale = ep(60 * 60_000, 60 * 60_000);
        stale.watched = true;
        assert_eq!(ep_state(&stale).glyph, EpGlyph::Watched, "resume == duration is finished");
        assert!(ep_state(&stale).progress.is_none(), "and draws no bar");

        // no runtime on the wire: the glyph still states the state, the label says nothing
        let st = ep_state(&ep(0, 0));
        assert_eq!(st.glyph, EpGlyph::Play);
        assert!(st.label.is_empty(), "no runtime, no claim about one");
        assert!(st.progress.is_none(), "and no bar to divide by a zero duration");
    }

    /// The state line and the progress bar are the two things pinned to a still's BOTTOM edge, and
    /// they must not touch at any phase of the focus pop (both ride the scaled rect). The bar is now
    /// full-bleed against that edge, so the line's clearance is the whole guard.
    #[test]
    fn the_state_line_clears_the_full_bleed_bar_at_every_pop_phase() {
        assert!(
            EP_LINE_BOT > EP_BAR_H,
            "the line's baseline inset ({EP_LINE_BOT}) must clear the bar's band ({EP_BAR_H})"
        );
        for sc in [1.0f32, 1.03, crate::ui::widgets::CARD_FOCUS_SCALE] {
            let cr = Rect::new(300.0, 480.0, EP_W, EP_H).scaled(sc);
            // the bar spans the card's full width — the point of the change, and what makes it the
            // same bar a Continue Watching card wears
            let bar = Rect::new(cr.x, cr.y + cr.h - EP_BAR_H, cr.w, EP_BAR_H);
            assert_eq!(bar.x, cr.x, "full-bleed: no side inset");
            assert_eq!(bar.w, cr.w, "full-bleed: the whole card width");
            assert!(bar.y + bar.h <= cr.y + cr.h + 0.01, "and it sits ON the bottom edge");
            // the scrim the line reads over covers the line's band and stays inside the still
            assert!(EP_SCRIM_H < cr.h, "the scrim must not swallow the whole still");
            assert!(EP_SCRIM_H > EP_LINE_BOT + EP_GLYPH_D, "…but must cover the line it exists for");
        }
    }


    /// A season tab's pill covers its whole CONTENT — the label plus the trailing tick a finished
    /// season wears — and never overlaps its neighbour at the layout's own advance. `TabLay::w`
    /// measures label + note, so a ticked tab is wider than its label alone: a pill built from the
    /// label would leave the tick outside the click target.
    #[test]
    fn season_tab_pills_cover_their_note_and_never_overlap() {
        let top = 120.0;
        let mid = top + TAB_ROW_H * 0.5;
        let mut x = MARGIN_X;
        let mut prev: Option<Rect> = None;
        // (label width, note width): a bare tab, a ticked one, and a long title wearing a tick
        for (lw, nw) in [(86.0f32, 0.0f32), (140.0, 34.0), (210.0, 26.0)] {
            let lay = TabLay {
                i: 0,
                x,
                w: lw + nw, // exactly what tabs_layout stores: label width + TabPill::note_w
                label: CString::default(),
            };
            let r = tab_pill_rect(&lay, top);
            assert!(r.contains(x, mid), "the pill must cover its label's left edge");
            assert!(r.contains(x + lw + nw, mid), "…and the far edge of its trailing note");
            assert_eq!(r.h, TAB_ROW_H, "the pill stands one control tall");
            if let Some(p) = prev {
                assert!(r.x > p.x + p.w, "neighbouring season pills must not overlap");
            }
            prev = Some(r);
            x += lay.w + TAB_ADVANCE; // tabs_layout's advance
        }
    }
}

// ---- pointer (the Magic Remote's primary input) ----------------------------------------------
// The remote's pointer arrives as ordinary SDL mouse events, and `app.rs`'s remote FIFO synthesizes
// clicks as `ck:X,Y` in the SAME authored 1920x1080 space — so hit-testing in authored coords serves
// both by construction. Every rect below is re-derived from the geometry helpers the painter uses
// (`hero_btn_rect`, `tab_pill_rect`, `strip_x`, the `ScrollColumn` flow), never recorded, so a
// sibling change to a section's layout moves the draw and the click target together.

/// Screen-space top of section `sec` right now: its place in the below-hero flow (`child_top` — the
/// same y the `ScrollColumn` pre-translates that block's painter to) minus the live scroll. `None`
/// for a section that isn't present, and for the hero (0), which is pinned rather than a child.
fn section_screen_top(sec: c_int) -> Option<f32> {
    let (s, n) = sections();
    let pos = s[..n].iter().position(|&x| x == sec)?;
    if pos == 0 {
        return None;
    }
    let v = view();
    let col = v.column;
    Some(col.child_top(v, pos - 1) - col.scroll.pos)
}

/// The (section, item) under the pointer, or `None` for a miss. Checked in draw order — the pinned
/// hero first, then each present below-hero block against its own strip pitch. The About footer (5)
/// is deliberately absent: its four "items" are a read-only card and three text columns whose
/// activation is a no-op, so a click there is a miss rather than a focus jump into a dead end.
fn hit_at(mx: f32, my: f32) -> Option<(c_int, c_int)> {
    let scroll = view().column.scroll.pos;
    // hero action row: it scrolls with the hero, and it stops answering while the hero fades out.
    // The threshold is FIRMER than the paint's `> 0.01` on purpose — a control faded to a percent
    // of a percent is not something the user can see, and clicking apparently-empty space must not
    // start playback. Only reachable mid-transition anyway (the scroll rests at 0 or well past the
    // fade), which is exactly when a stray click is least intended.
    if hero_alpha(scroll, HERO_FADE) > HERO_HIT_MIN_A {
        let y = hero_layout(selected()).btn_y - scroll;
        // the LIVE control set, not a constant: `hero_btns()` is 3 only while the restart disc is
        // up, and `hero_btn_rect` slides the watched toggle to match
        for b in 0..hero_btns() {
            if hero_btn_rect(b, y).contains(mx, my) {
                return Some((0, b));
            }
        }
    }
    let d = metadata::current()?; // nothing below the hero is drawn until the item lands
    // season tabs: the strip scrolls horizontally, so the pill is placed at its DRAWN label x. The
    // row band is tested FIRST because `tabs_layout` measures every season's label through an
    // unmemoised TTF call — this runs on every motion event, so it must not pay that per pixel of
    // pointer travel across the rest of the page.
    if let Some(top) = section_screen_top(1).filter(|t| my >= *t && my <= t + TAB_ROW_H) {
        let sx = view().tab_hscroll.pos;
        for lay in tabs_layout(d) {
            // `tab_pill_rect` is the frame the painter drew, in the strip's CONTENT space (the
            // painter applies `-sx` as a translate), so the pointer is converted into that space
            // rather than the rect out of it. A tab widened by its watched tick is therefore
            // clickable across the note too — that width is `TabLay::w`, which measures both.
            if tab_pill_rect(&lay, top).contains(mx + sx, my) {
                return Some((1, lay.i as c_int));
            }
        }
    }
    // episode filmstrip: the whole block (still + its under-card metadata) belongs to that episode,
    // so clicking the title or the blurb picks the same one as clicking the still.
    if let Some(top) = section_screen_top(2) {
        if my >= top && my <= top + block_h(2) {
            let n = n_items(2).max(0) as usize;
            if let Some(i) = strip_at(mx, view().ep_hscroll.pos, n, EP_W + EP_GAP, EP_W) {
                return Some((2, i as c_int));
            }
        }
    }
    // Related / Cast: the two shared `CardRow` strips, each below its heading
    if let Some(top) = section_screen_top(3) {
        let row_y = top + REL_LABEL_H;
        if my >= row_y && my <= row_y + REL_H {
            let n = n_items(3).max(0) as usize;
            if let Some(i) = strip_at(mx, view().related.scroll_x(), n, REL_W + REL_GAP, REL_W) {
                return Some((3, i as c_int));
            }
        }
    }
    if let Some(top) = section_screen_top(4) {
        let row_y = top + CAST_LABEL_H;
        if my >= row_y && my <= row_y + CAST_D {
            let n = n_items(4).max(0) as usize;
            if let Some(i) = strip_at(mx, view().cast.scroll_x(), n, CAST_SLOT, CAST_D) {
                return Some((4, i as c_int));
            }
        }
    }
    None
}

/// Land focus on (`sec`, `col`) exactly the way [`move_focus`] would: remember the strip we are
/// leaving (so returning to it restores the item), re-pop the newly focused card, and queue a
/// debounced season load when the focus lands on a tab. Reports whether focus actually MOVED — the
/// caller aborts an armed click on a move, since the press belongs to the control it started on.
fn focus_to(sec: c_int, col: c_int) -> bool {
    let v = view();
    if v.section == sec && v.col == col {
        return false; // already there — don't restart the pop, or re-arm the season debounce
    }
    if v.section != sec && matches!(v.section, 2 | 3 | 4) {
        v.saved_col[v.section as usize] = v.col;
    }
    if v.section == 1 && sec != 1 {
        // Focus is LEAVING the tabs before the debounce fired, so the season it queued was never
        // dwelled on — drop it. The pointer makes this the common case rather than a rarity: a
        // hover brushing up across the tab strip and back down onto the episodes would otherwise
        // commit a season switch 200ms later, and `pump_season` resets the episode row under the
        // hand that was reaching for it.
        v.pending_season = -1;
    }
    v.section = sec;
    v.col = col;
    v.card_scale.jump(1.0);
    if sec == 1 {
        v.pending_season = col;
        v.season_settle = 0.0;
    }
    true
}

/// May a HOVER move focus to `sec`? Only when doing so would not scroll the page.
///
/// This is the detail page's form of home's vertical fly-away guard. There the rule is "a partially
/// visible row is not hoverable"; here focus DRIVES the scroll (`scroll_target_for` lifts the focused
/// block to `TOP_MARGIN`), so a hover that changes section would yank hundreds of pixels of content
/// out from under a stationary pointer — and whatever slid under it would then claim focus in turn.
/// So hover walks the items of the block that already holds focus (its own row scrolls horizontally
/// into view, exactly as home allows), plus any block that shares its anchor — the season tabs and
/// their episode row, which is the pair the user is actually browsing. Everything else needs a
/// deliberate CLICK, which is allowed to scroll because the user aimed at it.
fn hover_allows(sec: c_int) -> bool {
    (scroll_target_for(sec) - scroll_target_for(view().section)).abs() < 0.5
}

// Both entry points run under `ui::guard`, as home's do: they are reached from the `extern "C"`
// event loop, which the draw-dispatch barrier does not cover, so an arithmetic panic here would
// take the process rather than a frame. `hit_at` is the most computed of the pointer paths and it
// runs on every motion event.

/// Pointer motion: focus what the pointer is over, where that is free of page movement. Reports
/// whether focus MOVED, so `app.rs` can abort a click armed on the control the pointer just left.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    let mut moved = false;
    crate::ui::guard(|| {
        if let Some((sec, col)) = hit_at(mx, my) {
            if hover_allows(sec) {
                moved = focus_to(sec, col);
            }
        }
    });
    moved
}

/// Pointer click: focus what was clicked and report the hit, so `app.rs` can run the SAME activation
/// as OK — the tvOS press dip for a card, immediate for the Play pill / watched disc / season tab.
pub(crate) fn click(mx: f32, my: f32) -> bool {
    let mut hit = false;
    crate::ui::guard(|| {
        if let Some((sec, col)) = hit_at(mx, my) {
            focus_to(sec, col);
            hit = true;
        }
    });
    hit
}

/// "YYYY-MM-DD" -> "D Mon YYYY"; falls back to the year, then empty. Moved to [`crate::ui::fmt`]
/// when the person page's Born/Died line needed the same spelling — `fmt.rs` is the ONE home for a
/// display formatter, and two screens printing the same date two ways is exactly the drift it
/// exists to stop.
use crate::ui::fmt::pretty_date;
