//! Detail screen: a full-screen item page — the reused hero (no page dots) over the
//! item backdrop, with a vertical-scroll flow underneath for the episode/related/
//! cast rows and About footer (added in later increments). Reads the loaded item
//! from crate::metadata and the selected catalog row (backdrop art + blur) from the
//! browse catalog. Mirrors the home screen's C-shaped entry points (open/update/
//! draw/move_focus) driven by app.rs.
//!
//! The whole page sits on an ambient GROUND (`DetailView::amb`, keyed by `amb_blur`/`amb_target`):
//! the item's own `UltraBlurColors`, mixed toward `theme::SURFACE_APP` and held under the wash's
//! luminance ceiling, so the below-hero rows sit on the item's colours instead of the flat app grey.
//! Its envelope comes from the LOADED item first and the catalog row only as a stand-in, because
//! three of the four ways into this page (Library grid, Related tile, person page) are not in the
//! home hub catalog at all.
//!
//! The page serves EPISODES too, not only movies and shows — the home shelf's context menu ("Go to
//! Episode") and this page's own filmstrip both open `Route::Detail` on a leaf — so the backdrop
//! ladder ([`hero_art_path`]) has a rung for a leaf's own still, and every full-bleed blit `COVER`s
//! the panel rather than stretching to it (a still is a video frame whose aspect we do not control).
//! The episode filmstrip is correspondingly TWO focus rows deep ([`EpRow`]): the still, which plays,
//! and the metadata block under it, which opens that episode's page.
#![allow(dead_code)]
use crate::metadata;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::hero_logo::{self, HeroLogo, LogoRung};
use crate::ui::label::HAlign;
use crate::ui::text_view::TextView;
use crate::ui::theme;
// `dotted_run` lives in `widgets` now — the facts row and the bio panel's identity line are one
// idiom two screens apart, and this file is where it was written first, not where it belongs.
use crate::ui::widgets::{
    dotted_run, resolve_tex_wh_on, AmbientWash, Art, Button, CircleButton, PosterMark, TabPill,
    TabStrip, HERO_BASE_SCRIM_Y0,
};
use crate::ui::{hero_alpha, on_axis, Column, Env, Painter, Rect, ScrollColumn, Spring, View}; // View: Button/CircleButton::draw
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// Which of the episode filmstrip's TWO focus rows holds focus. Section 2 is one horizontal strip of
/// cells, but a cell is two distinct targets stacked: the still, and the metadata block under it.
///
/// It is a sub-row rather than a second section id because the two share EVERYTHING that makes the
/// strip a strip — the same `col`, the same `ep_hscroll`, the same per-cell pop springs, the same
/// scroll anchor — and a second section would have had to mirror all of it (and would have had its own
/// `saved_col`, so LEFT/RIGHT would silently mean a different episode on each row).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum EpRow {
    /// The landscape still. OK plays the episode; a press-and-HOLD opens its context menu.
    #[default]
    Still,
    /// The kicker / title / summary / air-date block beneath it. OK opens that episode's OWN page —
    /// the same navigation the context menu's "Go to Episode" row performs.
    Text,
}

/// Where a detail page was standing — enough to put it back when BACK returns to it, and nothing
/// more. Carried by `ui::trail`'s `Node::Detail`, snapshotted by [`spot`] and applied by
/// [`open_rk_at`].
///
/// A `Spot` names FOCUS, not pixels: the vertical scroll is derived from the focused section
/// (`scroll_target`) and both h-scrolls from `col`, so recording them too would be two sources for
/// one fact — and the second would be wrong the moment the item came back with a different-length
/// list. `Default` is a page that has been entered and not left yet: hero, item 0.
///
/// `season` is the season NUMBER, not `cur_season`'s POSITION: a `/children` refetch can reorder or
/// re-title the list, and the number is what the user actually saw on the tab. `None` for a movie —
/// and that `None` is load-bearing, because it is what stops a movie's restore waiting for a season
/// list that will never arrive (see [`spot_season_gate`]).
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct Spot {
    /// section id (0 hero, 1 tabs, 2 episodes, 3 related, 4 cast, 5 about)
    pub(crate) section: c_int,
    /// focused item within that section
    pub(crate) col: c_int,
    /// the filmstrip's sub-row ([`EpRow`]) — a bool because `EpRow` is private to this module and a
    /// trail node has no business naming it
    pub(crate) ep_text: bool,
    /// the per-section focus memory, so LEFT/RIGHT in a row the user never returned to still comes
    /// back where they left it
    pub(crate) saved_col: [c_int; 6],
    /// the selected season's NUMBER, or `None` for an item with no seasons
    pub(crate) season: Option<i64>,
}

/// Snapshot the page as it stands. Called at the moment a navigation off it is RAISED, not when the
/// navigation is performed, so nothing that runs in between can move the focus under the snapshot.
pub(crate) fn spot() -> Spot {
    let season = metadata::current().and_then(|d| d.seasons.get(d.cur_season).map(|s| s.index));
    let v = view();
    Spot {
        section: v.section,
        col: v.col,
        ep_text: v.ep_row == EpRow::Text,
        saved_col: v.saved_col,
        season,
    }
}

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
    /// The SERVER `mounted_rk` belongs to — the other half of that identity. A ratingKey is
    /// server-local and both servers number from 1, so `pms::index_of_rk` takes the pair; mounting
    /// on the key alone matched a colliding row on the other machine and gave the page its
    /// backdrop, blur envelope and selection.
    mounted_sid: crate::plex::ServerId,
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
    /// Which sub-row of the episode filmstrip holds focus (see [`EpRow`]). Only meaningful while
    /// `section == 2`; every path that lands focus ON section 2 sets it explicitly, from the DIRECTION
    /// of the move, so UP and DOWN stay exact inverses of each other across the block. Deliberately
    /// NOT remembered in `saved_col`: coming back into the strip from below should land on the block
    /// nearest the pointer/focus that left, which is a fact about the move, not about the strip.
    ep_row: EpRow,
    tab_hscroll: Spring, // season-tab row horizontal scroll (many-season shows overflow the width)
    /// The season strip's travelling capsules — the SAME component the shared top tab row uses
    /// ([`TabStrip`]), because the owner directive recorded above `widgets::TAB_PILL_H` makes the two
    /// rows one control, and one control has one motion. A value rather than a static: the top row
    /// carries its capsule ACROSS a route flip on purpose, whereas a season strip belongs to its item
    /// and must start clean on the next one (`reset_view_state`).
    tabs: TabStrip,
    // season-tab load is DEBOUNCED: focusing a tab records the target here + resets the settle timer;
    // update() loads it only once focus has been stable (SEASON_SETTLE). Otherwise a held LEFT/RIGHT
    // through the tabs would fire a blocking PMS `/children` fetch every repeat tick.
    pending_season: c_int, // tab focused but not yet loaded (-1 = none / already loaded)
    season_settle: f32,    // seconds the pending season has been stable
    related: CardRow,   // the Related row = the SAME animated shelf component the home grid uses
    cast: CardRow,      // Cast & Crew row — the same component, circular RowStyle::CAST
    last_resume_ns: i64, // resume position (ns) on_ok just started (0 = from start); app.rs seeks here
    // Which conditional controls the hero row carried the last time its focus was resolved. The
    // row's control SET is derived from the loaded item and from the resolved copy list, both of
    // which arrive (and change) asynchronously — so the index the focus holds has to be carried
    // across a set change or it silently comes to mean a different button. See `hero_col`.
    hero_set: HeroSet,
    /// How far each of the row's two DISCS is unfurled into its labelled capsule (0..1), indexed by
    /// [`disc_verb`]'s slot: 0 the restart disc, 1 the watched toggle. Two springs and not one
    /// because they stand side by side and the ring moving between them is one capsule closing while
    /// the other opens — a single spring would make that a jump-cut through a shared value. See
    /// [`disc_verb`] for why the labels exist at all, and `widgets::DISC_LABEL_LEAD` for the
    /// geometry they drive.
    disc_unfurl: [Spring; 2],
    /// The action row's FOCUS POP — one spring per control, indexed the way [`hero_ctls`] orders
    /// them. Four, the widest set the page can build (`Play, ▶|, Also available, ✓`).
    ctl_pop: crate::ui::widgets::CtlPop<4>,
    // per-section focus memory (episodes/related/cast): leaving a row and coming back restores the
    // item you were on — paired with the frozen h-scrolls, a row "stays where it was" instead of
    // snapping to its start whenever focus moves elsewhere. Indexed by section id.
    saved_col: [c_int; 6],
    spin_ms: f32, // spinner phase for the episode row's season-loading state
    /// The page's ambient GROUND — the item's UltraBlur corners mixed toward the app surface,
    /// dissolving when the page changes subject. Opaque (see [`AmbientWash`]), so it stands IN for
    /// the clear rather than riding over it: it is what the below-hero rows sit on, which used to be
    /// flat `SURFACE_APP` at every scroll position past the hero — the page paid for the wash and
    /// then buried it under a grey rect.
    amb: AmbientWash,
}
impl DetailView {
    fn new() -> Self {
        Self {
            selected: -1,
            mounted_rk: String::new(),
            mounted_sid: crate::plex::ServerId::UNSET,
            section: 0,
            col: 0,
            column: ScrollColumn::new(CONTENT_TOP, TOP_MARGIN), // top re-derived per frame (update)
            card_scale: Spring::at(1.0),
            ep_scale: [Spring::at(1.0); EP_ANIM_MAX],
            ep_hscroll: Spring::at(0.0),
            ep_row: EpRow::Still,
            tab_hscroll: Spring::at(0.0),
            tabs: TabStrip::new(),
            pending_season: -1,
            season_settle: 0.0,
            related: CardRow::new(),
            cast: CardRow::new(),
            last_resume_ns: 0,
            hero_set: HeroSet::default(),
            disc_unfurl: [Spring::at(0.0); 2],
            ctl_pop: crate::ui::widgets::CtlPop::new(),
            saved_col: [0; 6],
            spin_ms: 0.0,
            // the app's own flat ground until an item keys it — byte-identical to the grey this page
            // showed before it had a ground
            amb: AmbientWash::flat(theme::SURFACE_APP),
        }
    }
}
static mut VIEW: Option<DetailView> = None;
fn view() -> &'static mut DetailView {
    unsafe { (*addr_of_mut!(VIEW)).get_or_insert_with(DetailView::new) }
}

// ---- the hero action row ----
// Play/Resume pill (0), the restart disc (1, present ONLY while there is a resume point to ignore),
// the "Also available" pill (present ONLY while a second pinned source holds this item), then the
// WATCH-STATE discs. An info disc would be a no-op on its own page, so the row stops there.
// The set is DYNAMIC — everything after the pill comes and goes, and the tail is one disc or TWO —
// so nothing may hard-code an index: see `HeroCtl`/`hero_set`/`hero_btns`/`hero_watch_state`.
const BTN_PLAY: c_int = 0;
const BTN_RESTART: c_int = 1;
// Play pill MINIMUM width. The drawn width is `Button::pill_w`, measured from the label, and for
// "Play" that lands ~2px above this — so the detail Play pill is now pixel-identical to home's
// rather than approximating it with a constant. The floor only guards a pathologically short label.
const PW: f32 = 168.0;
// The hero row's inter-control air and its disc diameter, both the SHARED control-family numbers
// rather than a second copy of them. `home.rs` draws this same row and already aliased the
// diameter, so the two literals that used to sit here were the one place the two heroes could
// silently drift apart — and `StatusOverlay::CTRL_H`'s own doc already claimed "the hero rowS'
// pill/disc diameter, which `home.rs` aliases rather than restating", which a `60.0` here made
// false.
const CGAP: f32 = crate::ui::widgets::CTRL_GAP;
const CD: f32 = crate::ui::widgets::StatusOverlay::CTRL_H; // circle button diameter
/// The hero text column's width — the TITLE BAND, the synopsis wrap AND every fine-print line under
/// it (the date/runtime pair, the "Directed by" credit) share it, so nothing in the block can grow
/// past the right-aligned "Starring" line. The title band used to have its own narrower literal
/// (680), which only ever bound a wordmark past ~7.5:1 and cost it the rung's height floor; one
/// column for the whole block is the same rule the rest of the page already followed.
/// 900 -> 943 with the Arial->Inter swap (2026-08-01). Inter sets ~4.75% wider than Arial after
/// kerning, and the synopsis is clamped to 3 lines, so the extra width came out of CONTENT rather
/// than out of the wrap: the movie hero's blurb lost about five words and stopped mid-idiom
/// ("...So, it's all for..."). The cause is horizontal, so the fix is horizontal — widening the
/// column restores the tuned character count while keeping the block at 3 lines, which keeps its
/// measured height and therefore leaves the facts row, the Play pill and the rest of the y-chain
/// exactly where they were. (A 4th line would have walked the Play pill down every detail page.)
/// Re-derive this if the font changes again: it is 900 x (Inter run width / Arial run width).
pub(crate) const HERO_TEXT_W: f32 = 943.0;

// ---- the hero text column's y-chain (`hero_chain`), pitch by pitch, from `Details Screen.dc.html`.
// These are the GAPS between line tops, not absolute ys: two of the bands are conditional and the
// synopsis is measured, so only the pitches are constant. ----
/// The title block's baseline band — its bottom edge, which the clearLogo art's BOTTOM EDGE and the
/// text title's BASELINE both sit on, and the anchor the whole chain hangs from. The band's height
/// is `hero_logo::band_h(Hero)` (120), so its top is at 446; a squarer clearLogo spills upward out
/// of it as paint (to y=298 for a 1:1 mark) without moving anything below.
pub(crate) const TITLE_BOTTOM: f32 = 566.0;
const META_DY: f32 = 30.0; // title bottom → "TV Show · Drama · TV-MA"
const RATINGS_DY: f32 = 50.0; // meta line → the review-score row
const SYN_DY: f32 = 54.0; // last line above → synopsis
const FACTS_DY: f32 = theme::space::MD; // synopsis bottom → date/counts line
const BTN_DY: f32 = 50.0; // last fine-print line → the action row
/// Synopsis line pitch — the SHARED one ([`crate::ui::HERO_SYN_LEAD`]), aliased so this page's own
/// chain arithmetic below still reads in its own vocabulary. The value moved out when the two heroes
/// were made to agree about the blurb; the LINE CAP moved with it.
const SYN_LEAD: f32 = crate::ui::HERO_SYN_LEAD;
/// The floor the measured synopsis height is clamped up to, so a one-line blurb (or a missing one)
/// still leaves the facts line where a two-line blurb puts it. This page's own, not the shared
/// block's: it is a fact about THIS chain, which has two conditional bands under the blurb.
const SYN_MIN_H: f32 = 34.0;
// ---- the PEOPLE column: the crew credit over the cast, right-aligned beside the action row.
/// Its column. Narrow on purpose — it must not reach the hero text column on its left, and wrapping to
/// two lines inside 560px is what keeps three long names off the Play pill.
pub(crate) const PEOPLE_W: f32 = 560.0;
/// Line pitch inside the column (`line-height: 32px`) — the block is measured in whole lines of it,
/// which is what lets [`people_top`] speak for a block of any height.
pub(crate) const PEOPLE_LEAD: f32 = 32.0;
/// Lines ONE of the column's blocks may wrap to before it elides. Two, for both: three long cast
/// names do not fit 560px on one line, and a multi-director credit is the same shape of problem.
const PEOPLE_MAXLINES: usize = 2;
/// Cast members named. Three is the mock's list, and it is a BILLING order, not a sample: PMS sends
/// `Role[]` in credit order, so the first three are the top-billed ones.
const PEOPLE_CAST: usize = 3;
/// The tallest the whole column can get, in lines — both blocks wrapped to their own cap. The
/// legibility table grades the top line of THIS block, because that is the worst case: the right
/// wedge feathers in from its top, so the highest line of the column is the least darkened one.
pub(crate) const PEOPLE_MAX_LINES: usize = 2 * PEOPLE_MAXLINES;
/// The people column's ink — and the one thing on this page taken from the legibility contract
/// rather than from `Details Screen.dc.html`, which sets the column `--text-tertiary`.
///
/// Bottom-anchoring the column moved its worst case (the top line of a fully wrapped credit over a
/// fully wrapped cast list) 74px UP the frame, into a band where the ground is much weaker: the
/// right wedge feathers in from y=702, so the composite scrim there falls from ≈0.71 to ≈0.59.
/// At `TEXT_TERTIARY` that is 2.37:1 over bright artwork — **under the design's own stated 3:1
/// floor**, and the honest reading is that the port took the mock's ink without the mock's ground
/// (the mock's bottom ramp reaches .72 by a third of the way up the frame, where this page's single
/// linear stop is at .55). One step brighter clears it at 3.67:1 with the ground unchanged, which is
/// the cheap half of that trade: retuning `base_scrim_a` is a whole-frame decision only a panel can
/// judge, and reaching 3:1 at TERTIARY would need the right wedge at .80 from y=600 — 60% stronger
/// over a band the component's own doc flags as its banding risk.
///
/// The mock's INTENT — the column is quiet fine print, a caption and not a control — is carried by
/// its size (`CAPTION`, two rungs under the identity line beside it) rather than by an ink that
/// cannot be read over a bright still. `widgets`' hero-scrim anchor table grades this row and holds
/// it to the ordinary 3.0/2.5 floors again.
pub(crate) const PEOPLE_INK: [f32; 4] = theme::TEXT_SECONDARY;
/// The LABEL word's ink ("Starring", "Directed by") — one rung under the names beside it, which is
/// the mock's own relationship (#7B818A against #949AA3, a factor of ~0.83) transposed onto the
/// brighter ink [`PEOPLE_INK`] had to take. The label is scaffolding and the name is the content,
/// so the name wins the contrast; dimming by a whole rung rather than by a bespoke value keeps the
/// column on the ladder. Its own floor is the row's LOWER one — it is a predictable word read by
/// shape, not a string anyone has to decipher.
pub(crate) const PEOPLE_LABEL_INK: [f32; 4] = theme::TEXT_TERTIARY;
/// The facts row's hard RIGHT bound — the people column's own left edge, less a rung of air.
///
/// The two are LEVEL: the facts line sits one `BTN_DY` above the button row and the column bottoms
/// out on the same row and grows upward, so at three lines the column's top line and the facts
/// line share a band to within a few pixels. Nothing stops them meeting except this, and the row
/// has no natural ceiling — the worst case (an air date, `3 seasons, 30 episodes`, and the "hardware
/// conversion needs \[PLEX PASS\]" fragment) measures past 1360 on the shipped font, ~95px INSIDE
/// the column. The deleted "Directed by" tail carried exactly this guard; the fragment that replaced
/// it inherited none, which is the regression this const is.
const FACTS_R: f32 = SCR_W - MARGIN_X - PEOPLE_W - theme::space::SM;
/// Share of the hero text column the blurb's bold `S2, E3 · Laura:` prefix may claim before the
/// episode title inside it starts eliding. It shares line 1 with the start of the blurb, so it cannot
/// have the whole width — see `hero_blurb`.
const LEAD_BUDGET: f32 = 0.55;
/// Gap between two review-score badges. Deliberately its own value rather than a `space` rung: `MD`
/// (24) let four marks in four different silhouettes crowd into one texture, and `LG` (40) broke the
/// row into four unrelated islands. 32 is the mock's, and it reads as one row of four things.
const RATING_ROW_GAP: f32 = 32.0;
/// Air either side of a separator `\u{b7}` on the identity line and on the facts line. It used to
/// be spelled as literal spaces inside the joined string ("   \u{b7}   "), which is what forced the
/// dot to share the words' ink; [`dotted_run`] draws it as its own run, so the air is a number.
/// Both take the `SM` rung: the mock spells 16px on the identity line and 20px on the facts line,
/// and 4px at `CAPTION` is under the threshold of that difference being visible — one rung for
/// both keeps the row on the spacing ladder rather than carrying a bespoke number to express
/// something the eye cannot see.
const META_SEP_PAD: f32 = theme::space::SM;
const FACTS_SEP_PAD: f32 = theme::space::SM;

// Below-the-hero content is ONE vertical scroll of stacked blocks, driven by the shared retui
// `ScrollColumn` (`impl Column for DetailView`): its `child_top` COMPUTES each block's pre-scroll top
// by stacking the *present* blocks' heights (`block_h`, via `Column::height`) from CONTENT_TOP with a
// single SECTION_GAP between them (season tabs → episodes hug with TAB_EP_GAP) — no hard-coded
// per-section Y to keep in sync. A block's height is content-derived (e.g. the Related block tracks
// REL_H → the shared home poster size), so resizing one block reflows everything below it. The
// container's scroll lifts the focused block's top to TOP_MARGIN under the compact title.
const TOP_MARGIN: f32 = 120.0;
/// The pinned compact title's top edge — the literal the old top-anchored logo draw used, now named.
const COMPACT_TITLE_TOP: f32 = 40.0;
/// …and its anchor line: the compact logo's BOTTOM EDGE and the text fallback's BASELINE both sit
/// here. [`TOP_MARGIN`] (120) is where the first scrolled section lifts to, so this leaves 26px of
/// air under it — and `theme::logo::COMPACT_H_MAX` (72) is set by exactly that clearance rather than
/// by the area rule.
const COMPACT_TITLE_BOT: f32 = COMPACT_TITLE_TOP + theme::logo::COMPACT_H_MIN; // 94
const CONTENT_TOP: f32 = 920.0; // nominal flow top; update() re-derives it from the hero buttons
const SECTION_GAP: f32 = theme::space::XL; // vertical padding between top-level blocks (64)
const TAB_EP_GAP: f32 = theme::space::MD; // season tabs → their episode row (they read as one unit)
const SCROLLED: f32 = CONTENT_TOP - TOP_MARGIN; // backdrop-dim saturation reference (= 800)
const BTN_CONTENT_GAP: f32 = theme::space::XL; // hero button row → first below-hero block (64)
const HERO_FADE: f32 = 400.0; // px of scroll over which the hero fades out (draw + pointer share it)
const HERO_HIT_MIN_A: f32 = 0.5; // a hero control must be at least this opaque to be CLICKABLE
/// The box the hero backdrop is transcoded into — the SAME for an episode still and a show/movie
/// backdrop, so a page whose subject changes never re-fetches the same picture at a second size (a
/// second size is a second slot in a 64-entry store).
///
/// Deliberately no `upscale=1`: without it PMS returns a source smaller than the box at its NATIVE
/// size, so a 640×360 episode still costs a 640×360 texture whether we ask for 1280 or 1920 — while
/// asking small would throw detail away on the shows whose agent did supply a 1920 still. The
/// magnification a small still needs is free on the GPU (`Rect::cover` + `GL_LINEAR`); server-side it
/// would be ~9× the bytes over the wire and ~9× the VRAM, at exactly the same softness.
const HERO_ART: (c_int, c_int) = (1920, 1080);

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
// ---- the metadata block as a FOCUS TARGET ([`EpRow::Text`]): a translucent rounded panel under the
// selected block, MEASURED to wrap that episode's own content. Modelled on the About footer's column
// highlight, which is this app's existing idiom for a focusable block of text — same token, same
// radius, same "hug the content rather than a fixed box" rule (a fixed panel would leave a 1-line
// blurb sitting in a hole sized for an 8-line one, which is the defect About's own highlight was
// rebuilt to fix).
/// Air either side of the text column inside the panel. The narrowest rung, and the gutter is what
/// picks it: two cells are `EP_GAP` (28) apart, so the pad is shared between two neighbouring panels
/// and anything from 14 up would have them MEET. At `XS` they stay 12px apart — which is also why the
/// horizontal pad is a rung tighter than the vertical one, rather than the About footer's roomier
/// 26px inset. (The lit panel is never lit next to another one, but the geometry has to be honest
/// whichever cell is focused.)
const EP_TEXT_HL_PAD_X: f32 = theme::space::XS;
/// Air above the kicker's line box and below the last line of ink. `SM`, and the vertical direction
/// has the room for it: `EP_META_TOP` (30) is the gap from the still's bottom edge to that line box,
/// so the panel starts 14px clear of the still (clear of the focus pop too, which grows it ~8px), and
/// `EP_META_BOT_PAD` (24) at the foot leaves 8px under the panel of the TALLEST episode — so
/// `block_h` needs NO allowance for the highlight and nothing in the flow below moves when one
/// appears. Asserted by `the_episode_text_highlight_fits_the_block_the_flow_already_reserves`.
///
/// It also buys the corner back: 16px down a `widgets::TEXT_BLOCK_HL_RAD` arc has retreated to
/// within 0.2px of the panel's edge, so the rounding cannot bite into the kicker's first glyph the
/// way it would have at the horizontal pad alone.
const EP_TEXT_HL_PAD_Y: f32 = theme::space::SM;
/// The filmstrip's alpha while a SEASON FETCH is in flight: the row keeps showing the previous
/// season's episodes, dimmed, with a spinner over them — the switch is async, so a rapid tab hop
/// never freezes the UI and never blanks the strip. Named because two places draw through it: the
/// strip itself, and the lift of one cell over a context menu's scrim
/// ([`redraw_focused_episode`]), which must not restore to full strength a tile that is dim
/// precisely because it is stale.
const EP_STALE_A: f32 = 0.35;
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

/// The *Also available* control's label — the design's own words, and a fixed string: it names a
/// FACT about the item rather than an action, so unlike the Play pill beside it there is no state
/// for it to relabel from.
const ALT_LABEL: &std::ffi::CStr = c"Also available";
/// …and its drawn width, through the same one pill formula, counting the trailing chevron. No
/// floor: the label is long enough to size itself, and `PW` is the Play pill's guard against a
/// pathologically SHORT one.
fn alt_pill_w() -> f32 {
    // no leading icon, one trailing accessory — the disclosure chevron
    Button::pill_w_full(ALT_LABEL.as_ptr(), theme::size::BODY, false, true)
}

/// The verbs the hero's DISCS unfurl to say while they hold focus.
///
/// A disc is the one kind of control in this row that cannot state its own job: the pill next to it
/// carries "Resume", the *Also available* control carries its whole sentence, and a bare ✓ or ▶| is
/// a shape nobody can read from a sofa. So on focus — the only moment a television can tell anybody
/// anything — a disc opens into a capsule and says it (`widgets::DISC_LABEL_LEAD`). **Both** discs
/// do; there is nothing special about the watch one.
///
/// **The verb NAMES ITS SUBJECT when the row has two of them.** On a started show the whole hero is
/// about the ON-DECK EPISODE — the backdrop still, the blurb, and what Play and ▶| act on
/// ([`hero_episode`]) — while the watched toggle writes the item the page is MOUNTED on, which is
/// the show ([`on_ok`] takes `metadata::current()`, never the leaf). Two subjects on one row, and
/// nothing on screen said so: the press that reads as "I have seen this episode" marks four hundred
/// of them. So the word *Show* appears on the ODD ONE OUT, exactly when that gap is real. Everything
/// else in the row acts on the subject the blurb has just named, which is why *Play from Start*
/// needs no noun — it starts the very thing "Resume" beside it would resume.
///
/// **These are `item_menu`'s words, verbatim, and that is the point**: all three actions are also
/// offered on the press-and-hold menus, and a control that said "Watched" here while a row said
/// "Mark as Watched" would read as two verbs for one write. The agreement is asserted rather than
/// trusted — see `the_unfurled_verbs_are_the_menus_own`. Each states the OUTCOME its press
/// produces, not the state the item is in.
const MARK_WATCHED_LABEL: &std::ffi::CStr = c"Mark as Watched";
const MARK_UNWATCHED_LABEL: &std::ffi::CStr = c"Mark as Unwatched";
const MARK_SHOW_WATCHED_LABEL: &std::ffi::CStr = c"Mark Show as Watched";
const MARK_SHOW_UNWATCHED_LABEL: &std::ffi::CStr = c"Mark Show as Unwatched";
const PLAY_FROM_START_LABEL: &std::ffi::CStr = c"Play from Start";

/// A disc's slot in [`DetailView::disc_unfurl`] and the verb it unfurls to — `None` for the two
/// PILLS, which say their word at rest and have nothing to reveal.
///
/// Slot order is the row's own: the restart disc always precedes the watched toggle, so a pair of
/// springs indexed this way animates in the direction the eye travels. PURE, so the whole mapping
/// is host-testable without a loaded item.
fn disc_verb(ctl: HeroCtl, name_show: bool) -> Option<(usize, &'static std::ffi::CStr)> {
    match (ctl, name_show) {
        (HeroCtl::Restart, _) => Some((0, PLAY_FROM_START_LABEL)),
        (HeroCtl::MarkWatched, false) => Some((1, MARK_WATCHED_LABEL)),
        (HeroCtl::MarkWatched, true) => Some((1, MARK_SHOW_WATCHED_LABEL)),
        (HeroCtl::MarkUnwatched, false) => Some((1, MARK_UNWATCHED_LABEL)),
        (HeroCtl::MarkUnwatched, true) => Some((1, MARK_SHOW_UNWATCHED_LABEL)),
        _ => None,
    }
}

/// Where the watched toggle sits in `set` — it is always present, so this is `Some` for every set
/// the row can be in. Keyed on the control's JOB rather than on either of its two faces, which is
/// what lets [`hero_col`] carry the focus across a press that flips it.
fn watch_index(set: HeroSet) -> Option<c_int> {
    let (v, n) = hero_ctls(set);
    v[..n].iter().position(|&c| is_watch_ctl(c)).map(|i| i as c_int)
}

/// Is `ctl` the watched toggle, wearing either face?
fn is_watch_ctl(ctl: HeroCtl) -> bool {
    matches!(ctl, HeroCtl::MarkWatched | HeroCtl::MarkUnwatched)
}

/// Does the toggle's subject differ from the one the rest of the hero is about? True exactly while
/// the hero is showing an on-deck EPISODE — see [`disc_verb`] for why that is the whole question.
fn watch_names_show() -> bool {
    hero_episode().is_some()
}

/// Which unfurl slot the ring is on, if any — `None` whenever focus is on a pill or off the hero row
/// entirely (`focus()` answers -1 below section 0), which is what closes both capsules when the page
/// is scrolled away from under them.
fn focused_disc_slot() -> Option<usize> {
    disc_verb(ctl_at(hero_set(), focus())?, false).map(|(slot, _)| slot)
}

/// Both discs' drawn widths right now: the shared unfurl formula
/// ([`widgets::CircleButton::cap_w`]) over each verb and its own spring, so the row's accumulation,
/// the painter and the pointer all advance by the very same numbers at every phase of the animation.
fn disc_caps() -> [f32; 2] {
    let set = hero_set();
    let (v, n) = hero_ctls(set);
    let named = watch_names_show();
    let mut lw = [0.0f32; 2];
    for &c in &v[..n] {
        if let Some((slot, label)) = disc_verb(c, named) {
            lw[slot] = crate::text::text_width(label.as_ptr(), theme::size::BODY, 1);
        }
    }
    disc_caps_at(set, hero_pill_w(), alt_pill_w(), view().disc_unfurl.map(|s| s.pos), lw)
}

/// …and the rule itself, PURE (the split every measured thing in this row takes — see
/// [`hero_btn_rect_at`]): both discs' drawn widths, given the row's set, both pills, each disc's
/// unfurl `e` and each verb's measured width. A slot the set does not contain passes `label_w = 0`,
/// which collapses to the bare disc and costs the budget nothing.
///
/// **The unfurl is BUDGETED, and a verb that does not fit is not shown at all.** This row shares
/// its band with the right-hand people column and must clear [`FACTS_R`]
/// (`the_widest_action_row_clears_the_people_column`) — a bound the closed row has hundreds of
/// pixels of room inside, and that the widest row the page can build (a resume point AND a second
/// source, so four controls) spends much of. So a capsule may only open into what the CLOSED row
/// leaves unspent, and when the whole verb will not fit there the disc stays a disc.
///
/// Dropping it whole rather than eliding it is the facts line's own ladder one block up
/// ([`facts_fit`]), for the same reason: the point of the label is to teach a mark nobody can read,
/// and *"Mark Show as Unwat…"* teaches less than the tick already did. Half a word is worth less
/// than the shape it was meant to explain. The refusal is a state the WIDGET also has to survive —
/// it keeps being handed a climbing focus spring — which is why `CircleButton` re-derives the
/// unfurl from the frame rather than trusting that `e` (`CircleButton::cap_label_w`).
///
/// **The budget belongs to the PAIR, and the widest verb is what has to fit in it.** The two discs
/// stand side by side and the ring moving between them has one capsule closing while the other
/// opens, so for a stretch of that hand-off the row is paying for two. They are complementary
/// drives on springs of one stiffness, so their sum never exceeds a single full one (a linear
/// spring's states add: 1→0 alongside 0→1 keeps the total at 1, and a pair starting shut only ever
/// climbs to it). The most the row can therefore owe at any instant is the WIDER verb's own extra —
/// so that, and not each verb alone, is what the room must cover, and why both discs are refused
/// together rather than one at a time.
fn disc_caps_at(set: HeroSet, pill: f32, alt: f32, e: [f32; 2], label_w: [f32; 2]) -> [f32; 2] {
    let (_, n) = hero_ctls(set);
    let closed = HeroWidths { pill, alt, disc: [CD; 2] };
    let last = hero_btn_rect_at(set, n as c_int - 1, 0.0, closed);
    let budget = CircleButton::label_budget(CD, FACTS_R - (last.x + last.w));
    if label_w[0].max(label_w[1]) > budget {
        return [CD; 2];
    }
    [CircleButton::cap_w(CD, e[0], label_w[0]), CircleButton::cap_w(CD, e[1], label_w[1])]
}

/// The hero action-row control rect for index `i` at row top `y`.
///
/// The row is DYNAMIC — the ↺ restart disc exists only while [`has_restart`], the *Also available*
/// pill only while a second source holds the item, and the watch-state tail is one disc or two
/// ([`hero_ctls`]) — and two of its five controls are pills whose width follows their label, so
/// positions are ACCUMULATED rather than per-control constants: the
/// row starts at the margin and every control is one `CGAP` past the last one's right edge. Walked
/// by `draw_buttons` AND by the pointer hit-test — anything else would put the click a control out
/// of step whenever a resume point exists, which is precisely the state a pointer user is most
/// likely to be in.
fn hero_btn_rect(i: c_int, y: f32) -> Rect {
    hero_btn_rect_at(hero_set(), i, y, hero_widths())
}

/// Every measured width the row's accumulation needs, as one value.
///
/// It is a struct and not four arguments because THREE of the five controls are now variable —
/// both pills follow their label, and each watch disc follows its own unfurl spring — and a walk
/// taking them positionally is one transposed pair away from drawing the row at widths the pointer
/// does not share.
#[derive(Clone, Copy, Debug)]
struct HeroWidths {
    /// the Play/Resume pill, measured from the word it is currently carrying
    pill: f32,
    /// the *Also available* pill, including its disclosure chevron
    alt: f32,
    /// the two discs, in [`disc_verb`]'s slot order, each already unfurled ([`disc_caps`])
    disc: [f32; 2],
}

/// The live widths. Separated from [`hero_btn_rect_at`] for the reason that function's doc gives:
/// every one of these reaches SDL_ttf, and the host suite links none.
fn hero_widths() -> HeroWidths {
    HeroWidths { pill: hero_pill_w(), alt: alt_pill_w(), disc: disc_caps() }
}
/// The accumulation itself, PURE: the drawn frame of control `i` in `set`, given both pills'
/// measured widths. One walk, so a variable-width control in the MIDDLE of the row (which the alt
/// pill is) cannot put everything after it a fixed disc-pitch out of step.
///
/// The widths are ARGUMENTS on purpose: measuring one reaches SDL_ttf, and a host test that calls
/// into the TV's text stack fails to LINK on the dev Mac (the crate's Tier-1 limit). Parameterising
/// them keeps the row's geometry host-testable at every pill width — "Play" and the wider "Resume",
/// with and without the alt control, and at every phase of a watch disc's unfurl — which is the
/// case the fontless host could otherwise never see.
fn hero_btn_rect_at(set: HeroSet, i: c_int, y: f32, cw: HeroWidths) -> Rect {
    let (v, n) = hero_ctls(set);
    let mut x = MARGIN_X;
    let mut w = cw.pill;
    for (k, c) in v[..n].iter().enumerate() {
        w = match c {
            HeroCtl::Play => cw.pill,
            HeroCtl::Alt => cw.alt,
            HeroCtl::Restart => cw.disc[0],
            HeroCtl::MarkWatched | HeroCtl::MarkUnwatched => cw.disc[1],
        };
        if k as c_int >= i {
            break;
        }
        x += w + CGAP;
    }
    // Height is always the DISC: an unfurled control is a capsule that grew sideways, so only `w`
    // ever leaves `CD` behind (`widgets::CircleButton::cap_w`).
    Rect::new(x, y, w, CD)
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

/// Catalog row `idx`, or `None` for the -1 sentinel. Split from [`selected`] because it touches NO
/// static state of this screen: `reset_view_state` runs while holding a `&mut DetailView` and must
/// not reach through `view()` for a second one (minting a second `&'static mut` inside that borrow
/// is aliasing UB — the same warning `person.rs`'s `update` carries).
fn row_at(idx: c_int) -> Option<&'static PmsMovie> {
    (idx >= 0).then(|| crate::pms::movie(idx as usize)).flatten()
}

/// the selected catalog row (backdrop art/blur), if any
fn selected() -> Option<&'static PmsMovie> {
    row_at(view().selected)
}

/// The resume position (ns) the hero's Play control would ACTUALLY apply — i.e. exactly what
/// `on_ok`'s play arm hands `app.rs`. A movie resumes from its own `viewOffset`; a show resumes its
/// ON-DECK episode (see [`hero_episode`]), so it reads that leaf's — and reads nothing at all on a show
/// the hero is presenting as a series, where Play starts from 0.
///
/// Both go through `metadata::resume_ns`, the same Plex rule the action applies, deliberately: keyed
/// on a raw `resume_ms > 0` instead, a 4-second offset or one past the 95%
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
/// server answers S2E2 of a show while season 1 is the tab you are looking at.
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

/// Is the loaded item a LEAF of a show — i.e. is this page an EPISODE's own page?
///
/// `Detail::kind` is the item's own PMS `type`, so this is not `!is_show`: a movie is not an episode
/// either. It exists because an episode page is a real destination (the home shelf's context menu
/// offers "Go to Episode", which routes `Route::Detail` at the leaf's ratingKey) and it inherits
/// almost everything from its show — including, on the wire, `art`. What it does NOT inherit is
/// `thumb`, which for a leaf is its OWN still: see [`hero_art_path`].
fn is_episode(d: &metadata::Detail) -> bool {
    d.kind == "episode"
}

/// Does the people column carry its "Starring" line? The server sent a `Role[]`, or it did not.
fn has_starring(d: &metadata::Detail) -> bool {
    !d.cast.is_empty()
}

/// The hero's crew credit as `(label, names)` — the people column's FIRST line, and `None` when the
/// server credited nobody.
///
/// The label follows what the item is, because the two credits are not the same claim:
/// * a **movie** (or an episode's own page) is *Directed by* its `Director[]` tags — what PMS
///   populates on a leaf, and what this hero has always drawn;
/// * a **series** is *Created by*, and "created by" is a WRITING credit, not a directing one. So it
///   is read off the show container's own `Writer[]`, which arrives here inside
///   [`metadata::Detail::crew`] (that vec folds `Director[]` + `Writer[]` and carries each person's
///   job in `role`). Deliberately NOT the show's `Director[]`: where an agent sends one at all it
///   names a director, and labelling a director "Created by" states a credit the server never gave.
///
/// A series whose agent supplied no writer simply has no credit line — the column degrades one line
/// at a time, exactly as an item with no cast drops its "Starring" line.
fn hero_credit(d: &metadata::Detail) -> Option<(&'static str, Vec<&str>)> {
    if d.is_show {
        let names: Vec<&str> =
            d.crew.iter().filter(|c| c.role.contains("Writer")).map(|c| c.tag.as_str()).collect();
        return (!names.is_empty()).then_some(("Created by", names));
    }
    let names: Vec<&str> = d.directors.iter().map(|s| s.as_str()).collect();
    (!names.is_empty()).then_some(("Directed by", names))
}

/// Does the hero carry its right-aligned people column at all? ONE test, because two places have to
/// agree about it: [`draw_people`] draws nothing for an item with neither credit nor cast, and
/// [`draw_backdrop`] asks `widgets::hero_scrim` for its mirrored RIGHT wedge only when there is copy
/// in that corner to darken for — that wedge exists for this column and for nothing else
/// (`HERO_SCRIM_R_*`'s own doc says so). Unconditional it was 700×378 = 264,600 blended fragments a
/// frame shading an empty corner.
fn has_people(d: &metadata::Detail) -> bool {
    has_starring(d) || hero_credit(d).is_some()
}

/// The people column's BOTTOM edge: the action row's own bottom. `Details Screen.dc.html` pins the
/// block at `bottom: 10px` inside a 920-tall hero whose buttons run 850..910 — i.e. level with the
/// buttons, which is the relationship worth keeping when the chain above is MEASURED rather than
/// fixed. Bottom-anchored, so the block grows UPWARD: a top-anchored one would walk down into the
/// section flow every time the cast list took a second line.
pub(crate) fn people_bottom(btn_y: f32) -> f32 {
    btn_y + CD
}

/// The cap top of the column's TOPMOST line for a block `lines` lines tall — the y `widgets`'
/// hero-scrim legibility table grades, since the right wedge feathers in from its top and the
/// column's highest line is therefore its least-darkened one.
pub(crate) fn people_top(btn_y: f32, lines: usize) -> f32 {
    people_bottom(btn_y) - lines as f32 * PEOPLE_LEAD
}

/// The PMS image path the hero backdrop is keyed on — the ladder [`draw_backdrop`] resolves into a
/// texture, split out of the draw so the CHOICE (which is the behaviour) is host-testable while the
/// blit is not.
///
/// Rungs, in order:
/// 1. **The episode a SHOW's hero is about** ([`hero_episode`]) — its own still, not the series art
///    (`Details Screen.dc.html`, and Apple's own show page): the frame belonging to the thing Play
///    would start and the blurb underneath describes, so the whole hero is about one episode. It also
///    means the page visibly changes as you work through a season, where series art never does.
/// 2. **An EPISODE page's own still** (`Detail::thumb`). A leaf has no `OnDeck`, so rung 1 is `None`
///    here and the ladder used to fall straight through to `art` — which for an episode is the SHOW's
///    inherited backdrop. That is the owner-reported defect: the episode page painted the series'
///    picture. A leaf's `thumb` is its own still, which is exactly what this page is about.
/// 3. …else the item's landscape ART: the catalog row's first (it is loaded before the fetch lands,
///    which is what keeps the hero from flashing empty), then the loaded item's.
///
/// **Rung 2 has no catalog-row twin, deliberately.** A catalog row's `thumb` for an episode is the
/// SHOW's PORTRAIT poster and not the still — `pms::parse_item` prefers `grandparentThumb` on purpose,
/// so a landscape still does not fill a portrait card — so there is no episode still in the catalog to
/// prefer, and covering a 2:3 poster across a 16:9 panel would crop it to an unrecognisable middle
/// band. The row's `art` (rung 3) is the honest answer for the handful of frames before the leaf's own
/// metadata lands, and rung 2 takes over the frame it does.
fn hero_art_path(m: Option<&PmsMovie>) -> String {
    let d = metadata::current();
    hero_episode()
        .map(|e| e.thumb.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.filter(|d| is_episode(d)).map(|d| d.thumb.clone()).filter(|s| !s.is_empty()))
        .or_else(|| m.filter(|m| !m.art.is_empty()).map(|m| m.art.clone()))
        .or_else(|| d.map(|d| d.art.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

/// The server [`hero_art_path`]'s picture is a key on. Every rung of that ladder — the hero
/// episode, the loaded `Detail`, the catalog row — belongs to the page's own server, because the
/// episodes were fetched from the `Detail` and the row is the same item; so this is one answer, not
/// one per rung. The loaded page wins over the catalog row: a page can outlive the row that opened
/// it (a hub refetch can orphan it), and it is the page whose art is on screen.
fn hero_art_sid(m: Option<&PmsMovie>) -> crate::plex::ServerId {
    metadata::current()
        .map(|d| d.sid)
        .filter(|s| s.is_set())
        .or_else(|| m.map(|m| m.sid))
        .unwrap_or_default()
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

/// The loaded item's watch state, in the app's ONE watch-state vocabulary ([`PosterMark`]) — what
/// decides which FACE the hero's watched toggle wears, ✓ or −.
///
/// The row folds two of the three states together ([`hero_ctls`]: only `Watched` reads −), and the
/// distinction it keeps is still load-bearing rather than vestigial: `InProgress` is what makes a
/// finished-then-restarted item read ✓ instead of −, because the viewer is part-way through a
/// re-watch and "not watched" is the honest answer to give a toggle. All three states survive here
/// because the tile context menu, which offers BOTH verbs at once, resolves the same question.
///
/// It is the same three states a poster wears and deliberately the same enum, but it is a second
/// resolver rather than a call to [`crate::ui::widgets::poster_mark`]. The structural reason is
/// that the poster's reads a `PmsMovie` CATALOG ROW, and three of the four ways onto this page (the
/// Library grid, a Related tile, the person page) never put one in the home catalog — the page
/// would silently answer `None` for every item reached that way. It then answers differently in
/// two places, and both are the two functions' questions genuinely differing:
///
/// * **A CONTAINER mid-run.** A poster in a grid says nothing about a show three episodes into ten,
///   because "where its next episode stands" is not something a tile can state. Here the question
///   is which write verbs are reachable, and such a show can honestly be sent to either end — so it
///   IS in the middle. (What the middle then GETS differs by surface: the menu lists both verbs, the
///   hero's toggle reads ✓, because a list may name two destinations and a toggle is one thing.)
/// * **The edges of the resume rule.** `resume` is [`hero_resume_ns`]'s answer, i.e. the resume the
///   Play pill beside this toggle would ACTUALLY apply, so a 4-second offset or a 95% tail is no
///   more "in the middle" here than it is a "Resume" up there — while a poster's bar is drawn from
///   a raw `viewOffset` and does mark them. This row promises what its presses deliver (the same
///   rule the ↺ disc and the pill's label already follow); a mark on artwork only describes.
///
/// **`InProgress` outranks the watched flag**, exactly as the poster's does: PMS reports both on a
/// finished-then-restarted item, and being part-way through the re-watch is what the viewer is
/// doing. Pure, so the control set it produces is host-gradeable.
///
/// **The tile context menu asks the same question and must give the same answer** — its state rows
/// are this vocabulary as words (`ui::item_menu::state_rows`), so a change to what counts as "in
/// the middle" here belongs in `widgets::row_watch_state` too, which is that menu's resolver over a
/// catalog row and departs from `poster_mark` on exactly the container case this one does.
///
/// **KNOWN, on CONTAINERS only: the optimistic flip does not reach the evidence this reads**, so a
/// show's tail settles a round trip late instead of on the press frame. `metadata::set_watched_local`
/// edits the matched item's own `watched`/`resume_ms` — which is the whole answer for a leaf — but a
/// show's progress lives in `on_deck` and the seasons' `viewed_leaf_count`, and those keep the
/// server's pre-press values until `refresh_view_state`'s re-read lands. Marking a show watched
/// therefore changes nothing on screen for that window, and un-watching a finished one shows the
/// ✓ face on it for a beat before settling to −. Self-correcting, and no verb is wrong while it
/// lasts — the toggle is offering the write the server still believes is available.
/// Fixing it means teaching that function to carry a container's evidence too
/// (clear `on_deck`, drive `viewed_leaf_count` to the end the write names) — which is a data-layer
/// change three other surfaces on this page read, so it is deliberately not made from here.
fn hero_watch_state(d: &metadata::Detail, resume: i64) -> PosterMark {
    // A container has no resume point of its own — a show mid-run is one whose leaves have been
    // started and not finished, which is `show_started` minus the `watched` end of the range.
    // (`show_started` is TRUE for a finished show too: it asks "has this been touched at all".)
    let mid_run = d.is_show && show_started(d) && !d.watched;
    if resume > 0 || mid_run {
        PosterMark::InProgress
    } else if d.watched {
        PosterMark::Watched
    } else {
        PosterMark::None
    }
}

/// [`hero_watch_state`] for the item actually loaded — `None` (nothing started) while the page is
/// still fetching, which is the state that draws the single "mark as watched" disc, i.e. the row
/// the page has always shown during that window.
fn hero_mark() -> PosterMark {
    metadata::current().map(|d| hero_watch_state(d, hero_resume_ns())).unwrap_or_default()
}

/// A control in the hero action row, named rather than numbered.
///
/// The row's INDICES move — four of these five are conditional — so an index is only meaningful
/// next to the set that produced it. Everything that has to survive a set change (the focus, the
/// press mapping, the accumulation the pointer walks) is expressed in terms of this instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeroCtl {
    /// the Play/Resume pill — always present, always first
    Play,
    /// the ↺ disc, present only while there is a resume point for it to ignore
    Restart,
    /// the *Also available* pill, present only while a second pinned source holds this item
    Alt,
    /// the ✓ face of the WATCHED TOGGLE — "mark as watched". The toggle is always present and
    /// always last; this is the face it wears while the item is not watched, part-watched included.
    MarkWatched,
    /// the − face of that same toggle — "mark as unwatched", worn once the item IS watched.
    MarkUnwatched,
}

/// Which of the conditional controls the row is showing: the two independent bits, plus the
/// item's watch state, which is what decides which FACE the row's watched toggle wears.
/// The row's whole shape is this struct, which is what lets a set CHANGE be compared (`hero_col`)
/// rather than inferred.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct HeroSet {
    restart: bool,
    alt: bool,
    mark: PosterMark,
}

/// The live set, read from the loaded item and the resolved copy list.
fn hero_set() -> HeroSet {
    HeroSet {
        restart: has_restart(),
        alt: crate::ui::alt_sources::is_available(),
        mark: hero_mark(),
    }
}

/// The row's controls, in drawn order, for a given set. A fixed stack array plus a live count, like
/// [`sections`] — no per-frame allocation on the draw path.
///
/// **Order is the design's** (`Shared Sources.dc.html`, deliverable E): the play group, then *Also
/// available*, then the watched toggle last. The alt pill sits with the things you might do to
/// the copy rather than with the things you do to your own view state.
///
/// **There is no ⓘ *Track information* disc here, and its absence is a decision.** It shipped in
/// this middle group on the reasoning that inspecting the copy and choosing between copies are the
/// same kind of question — which is true, and is not enough: `Alert Views.dc.html` does not draw
/// this row at all, so the disc was OUR placement, and it put a control for a panel about the audio
/// and subtitle tracks 900px above the About footer's **Languages** column, which lists exactly
/// those tracks and is what the design says §1B *"belongs beside"*. Owner call, 2026-08-21: the
/// panel opens from the block it is about ([`on_ok`]'s section-5 arm), and the hero keeps only the
/// controls that DO something to the item. Do not re-add it here — a second door into one panel is
/// how two surfaces come to disagree about when it is available.
///
/// **The tail is ONE control, always** — the watched TOGGLE (`Details Screen.dc.html`). Its glyph
/// states the outcome its press produces: ✓ while the item is not watched, − once it is, and the
/// press turns each into the other. The second action is therefore never missing, only one press
/// away.
///
/// **This row briefly drew the two faces side by side for a PART-WATCHED item, and that was wrong.**
/// The reasoning was that such an item is at neither end of the range so neither verb alone
/// describes it — but the toggle does not describe the item, it owns the WATCHED FLAG, and that
/// flag has exactly two states however far through the item you are. A part-watched item is *not
/// watched*, so the toggle reads ✓. What being part-way through actually gives you is somewhere to
/// start over from, and that is a different fact with its own control standing right beside this
/// one — the ↺ disc, which is present under exactly the same condition (`has_restart`) and
/// disappears when the item is watched because there is then no resume point left. The pair the row
/// used to grow was that fact stated twice, once in the wrong vocabulary.
///
/// **The tile context menu still offers BOTH rows and is right to** (`item_menu::state_rows`). A
/// menu is a list of available actions with no state of its own, so it can name both destinations
/// at once; a toggle is one control that must read as the thing it currently is. Same
/// [`hero_watch_state`] vocabulary underneath, two presentations of it — which is why the menu was
/// deliberately left alone when this changed.
///
/// One further consequence, and it is what keeps the row honest: the toggle is the control that can
/// DELETE the ↺ disc from under itself (marking watched clears the resume point), so the row can
/// shrink under the very press that acted on it. [`hero_col`] is what re-lands the focus on it.
fn hero_ctls(set: HeroSet) -> ([HeroCtl; 6], usize) {
    let mut v = [HeroCtl::Play; 6];
    let mut n = 1;
    if set.restart {
        v[n] = HeroCtl::Restart;
        n += 1;
    }
    if set.alt {
        v[n] = HeroCtl::Alt;
        n += 1;
    }
    // the watched toggle, wearing the face of the write it would perform
    v[n] = if set.mark == PosterMark::Watched { HeroCtl::MarkUnwatched } else { HeroCtl::MarkWatched };
    n += 1;
    (v, n)
}

/// The control at index `col` in `set`, or `None` for an index the set does not have.
fn ctl_at(set: HeroSet, col: c_int) -> Option<HeroCtl> {
    let (v, n) = hero_ctls(set);
    usize::try_from(col).ok().filter(|&i| i < n).map(|i| v[i])
}

/// Where `ctl` sits in `set`, or `None` when the set does not include it.
fn index_of(set: HeroSet, ctl: HeroCtl) -> Option<c_int> {
    let (v, n) = hero_ctls(set);
    v[..n].iter().position(|&c| c == ctl).map(|i| i as c_int)
}

/// how many controls the hero row is showing right now
fn hero_btns() -> c_int {
    hero_ctls(hero_set()).1 as c_int
}
/// Screen index of hero control `ctl` right now, or `None` while the row is not showing it — the
/// live-set form of [`index_of`], which is what every draw and press site must use instead of a
/// literal. (It replaced a `btn_watched()` that answered `hero_btns() - 1`: with the part-watched
/// pair the row's last control is *mark unwatched*, so "last" stopped naming one control at all.)
fn hero_index(ctl: HeroCtl) -> Option<c_int> {
    index_of(hero_set(), ctl)
}
/// The hero row's focused index, resolved against the LIVE control set and written back — so the
/// correction IS the state rather than something each reader has to remember.
///
/// The set is derived from an item that arrives, and changes, asynchronously, so an index alone is
/// not a stable way to name a control. Both directions bite, and both are reachable by hand:
///
/// - It **grows**. `open_rk` deliberately clears `current()` for the whole 2-5 round-trip fetch, so
///   during that window the row is two buttons and index 1 is the *mark watched* disc. Press RIGHT
///   there, let a part-watched item land, and index 1 has quietly become the restart disc — the
///   next OK would start playback instead of writing view state.
/// - It **shrinks**. A watch-state disc is the control that can delete the restart disc from under
///   the focus, because scrobbling clears the server's `viewOffset` — and it deletes ITSELF at the
///   same time, since a part-watched item that becomes watched drops from two tail discs to one.
///   Focus left at index 3 of a two-button row draws nothing focused and makes every later OK inert.
///
/// So the focus follows its CONTROL, not its index: the index is resolved through the set it was
/// taken in ([`ctl_at`]) and looked up again in the new one ([`index_of`]). The clamp then catches
/// what is left — a control that VANISHED under the focus has no new index, and the row's last
/// control is the honest place to land. NB `move_focus` reads `col` raw, which is sound only
/// because this runs every drawn frame (`env_of`) and the event loop polls keys BEFORE `update`
/// lands a fetch — a key press therefore always sees a `col` already resolved against the set that
/// press is acting on.
///
/// It was a `+= 1` / `-= 1` around one insertion point while the restart disc was the only
/// conditional control. *Also available* is a second, so the shift is no longer a single offset:
/// two controls can appear in one frame (a page mounting part-watched with a second source), and
/// which one moved the focus depends on where it was sitting.
fn hero_col() -> c_int {
    let v = view();
    if v.section == 0 {
        let now = hero_set();
        if now != v.hero_set {
            let was = ctl_at(v.hero_set, v.col);
            v.hero_set = now;
            // …and the WATCHED TOGGLE is carried by its job, not by its face. It is the one control
            // whose identity changes under its own press — ✓ becomes − — and the same press can
            // delete the ↺ disc beside it (marking watched clears the resume point), so looking for
            // `MarkWatched` in a set that now holds `MarkUnwatched` at a different index finds
            // nothing and the focus falls to the clamp. That happens to land right today only
            // because the toggle is last; saying it outright is what keeps it right if anything is
            // ever appended after it.
            let landed = was
                .and_then(|c| index_of(now, c))
                .or_else(|| was.filter(|c| is_watch_ctl(*c)).and_then(|_| watch_index(now)));
            if let Some(i) = landed {
                v.col = i;
            }
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
    /// `/:/scrobble` — the ✓ disc
    MarkWatched,
    /// `/:/unscrobble` — the − disc
    MarkUnwatched,
    /// open the list of the OTHER sources holding this item (`ui::alt_sources`)
    Alt,
    None,
}

impl HeroAction {
    /// The view-state write this action performs, if it is one of the two that do.
    ///
    /// **The verb comes from the CONTROL, never from the item's state**, which is the substantive
    /// half of splitting the old single toggle in two: the press used to re-read `d.watched` and
    /// derive `if watched { Unwatched } else { Watched }`, so what a press did depended on a fact
    /// that could have changed since the frame the user looked at — and on a part-watched item
    /// there is no single "other end" for it to derive at all. Now the glyph the user aimed at IS
    /// the verb.
    fn watch_write(self) -> Option<crate::viewstate::Write> {
        match self {
            HeroAction::MarkWatched => Some(crate::viewstate::Write::Watched),
            HeroAction::MarkUnwatched => Some(crate::viewstate::Write::Unwatched),
            _ => None,
        }
    }
}

fn hero_action(col: c_int) -> HeroAction {
    match ctl_at(hero_set(), col) {
        Some(HeroCtl::Play) => HeroAction::Play,
        Some(HeroCtl::Restart) => HeroAction::Restart,
        Some(HeroCtl::Alt) => HeroAction::Alt,
        Some(HeroCtl::MarkWatched) => HeroAction::MarkWatched,
        Some(HeroCtl::MarkUnwatched) => HeroAction::MarkUnwatched,
        None => HeroAction::None,
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
/// The focus panel behind [`EpRow::Text`] for the cell whose content-space left edge is `x`, drawn
/// from the section's local origin `ep_y`, wrapping a metadata block measured `total_h` tall —
/// `ep_meta_layout`'s third element, i.e. THAT episode's own height rather than the row's tallest, so
/// a one-line blurb is not lit inside a hole sized for an eight-line one.
///
/// Pure, and the ONE place the panel's frame lives: the paint calls it, the pointer's split reads the
/// same `EP_H` boundary it hangs off, and the test that grades it against `block_h(2)` calls it too.
#[inline]
fn ep_text_hl(x: f32, ep_y: f32, total_h: f32) -> Rect {
    let ty = ep_y + EP_H + EP_META_TOP; // the kicker's cap top — where the block's ink starts
    Rect::new(
        x - EP_TEXT_HL_PAD_X,
        ty - EP_TEXT_HL_PAD_Y,
        EP_W + 2.0 * EP_TEXT_HL_PAD_X,
        total_h + 2.0 * EP_TEXT_HL_PAD_Y,
    )
}

/// Which of a filmstrip cell's two focus rows the point `dy` px below the section's top is in — the
/// ONE split, shared by the pointer ([`hit_at`]) and asserted against what [`ep_text_hl`] draws.
///
/// The boundary is the still's own bottom edge, so the dead air between the still and the metadata
/// block belongs to the still above it: there is no band the pointer can rest in that answers for
/// neither row. (The focus pop makes a still ~8px taller than `EP_H`; the hit-test has always ignored
/// the pop, since a target that grows when you reach it is worse than one that does not.)
#[inline]
fn ep_row_at(dy: f32) -> EpRow {
    if dy <= EP_H {
        EpRow::Still
    } else {
        EpRow::Text
    }
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
    v.hero_set = HeroSet::default();
    v.pending_season = -1;
    v.saved_col = [0; 6];
    // a keep-focus latch belongs to the item that armed it — a new page must not inherit one, nor
    // the season restore ([`REFRESH`]) that arms it. Both self-retire (each tests the item it was
    // armed for), and both are cleared here for the same reason `PENDING` is: a latch whose fetch
    // never lands would otherwise sit armed for a page the user might come back to much later.
    unsafe {
        *addr_of_mut!(KEEP_EP) = None;
        *addr_of_mut!(REFRESH) = None;
    }
    // …and so do both navigation latches. `OPEN_REQ` is drained once per frame by app.rs, so one
    // that survives its page fires on an unrelated OK several screens later (the incident
    // `person.rs`'s `requested` latch records). `PENDING` normally retires itself — `pump_pending`
    // drops one whose rk is not the loaded item — but only once SOMETHING loads, so a mount whose
    // fetch never lands would leave a placement armed for a page the user might come back to much
    // later. Clearing here is safe because both arms set `PENDING` immediately AFTER the mount they
    // belong to, so this can never eat the one just asked for.
    unsafe {
        *addr_of_mut!(OPEN_REQ) = None;
        *addr_of_mut!(PENDING) = None;
        *addr_of_mut!(ALT_REQ) = false; // …and both panel latches, for exactly the same reason
        *addr_of_mut!(ALT_OPEN) = None;
    }
    v.ep_row = EpRow::Still; // a fresh page's filmstrip starts on its stills, like a fresh arrival
    v.column.scroll.jump(0.0);
    v.ep_hscroll.jump(0.0);
    v.tab_hscroll.jump(0.0);
    // …and the watched toggle's unfurl, which is retained MOTION state exactly like the three
    // above. Focus lands back on the Play pill here (`col = 0`), so leaving a page with the capsule
    // open and opening another from it — a Related card, a cast member's film — would mount the new
    // one with a full-width "Mark as Watched" standing in a row that has nothing focused on it, and
    // then animate it shut. A jump, not a step: the capsule should never have been there to close.
    v.disc_unfurl = [Spring::at(0.0); 2];
    // …and the pop beside it, for that same reason: a page must not mount with a control already
    // standing 7% proud of the row.
    v.ctl_pop.reset();
    // Retained MOTION state, exactly like the scrolls above: a fresh strip's capsules must not glide
    // in from the season the previous item was parked on. Re-seating the whole component is the reset
    // (`TabStrip::new()` is the same const value the constructor uses), and the capsules' own landing
    // rule then places them on the first frame instead of flying them in.
    v.tabs = TabStrip::new();
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
    // The ground JUMPS on a mount, never dissolves: the item we just left must not wash across a
    // page that has already changed subject (`person.rs::open` states the same rule for the same
    // reason). From the CATALOG ROW only — see [`row_blur`]; the loaded envelope is picked up by
    // `update`'s dissolve the frame it lands, which is what makes a Library-opened page (no catalog
    // row at all) fade INTO its colours out of the flat grey instead of snapping to them.
    v.amb.jump(amb_target(row_blur(row_at(v.selected))));
}

/// Open the detail page for a catalog row. BLOCKING: its only caller is the headless
/// `plxnative-detail` trigger, which replays `move_focus`/`on_ok` in the same frame (and the
/// `detail-transition` FPS scene needs the sections present before `detailosc` starts swinging
/// the scroll — an async load there would measure a static hero and pass vacuously).
pub(crate) fn open(idx: c_int) {
    let v = view();
    v.selected = idx;
    reset_view_state(v);
    // …until the row below names the item this page is about
    crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
    if idx >= 0 {
        if let Some(m) = crate::pms::movie(idx as usize) {
            // the row's OWN server, never the current one — a merged shelf holds rows from more
            // than one, and the row is the only thing that knows which
            v.mounted_sid = m.sid;
            v.mounted_rk = m.rk.clone(); // keep the index reconcilable — see `reselect`
            crate::ui::alt_sources::reset(m.sid, &m.rk);
            if !m.rk.is_empty() {
                metadata::load_detail_now(m.sid, &m.rk);
            }
        }
    }
}

/// BACK on the detail page, handled INSIDE the screen: dismiss whichever of the page's own panels
/// is up, and report whether the press was spent. `false` means the page has nothing of its own to
/// close and `app.rs` should pop the BACK trail — the shape `library::back()` established, for the
/// same reason (a panel is part of the screen, so leaving the screen must not be how you close it).
///
/// **Three panels now**, and they are mutually exclusive in practice — each is opened by a control
/// the others cover — but all three are tested, in the order they sit in z: a defensive close of a
/// panel that is not up is free, and spending the press twice is not possible because each arm
/// returns. For the two ALERTS, BACK is not merely *a* way out but the only affordance either one
/// draws, which is why both footers name the key.
pub(crate) fn back() -> bool {
    if crate::ui::tracks_panel::is_open() {
        crate::ui::tracks_panel::close();
        return true;
    }
    if crate::ui::alt_sources::is_open() {
        crate::ui::alt_sources::close();
        return true;
    }
    if crate::ui::about_panel::is_open() {
        crate::ui::about_panel::close();
        return true;
    }
    false
}

/// Leave the detail page (drop the loaded item).
pub(crate) fn close() {
    // the panels are this page's, so they die with it — and with an unset server and an empty rk
    // nothing can land in the "Also available" store for a page that is gone
    crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
    // …and so do both ALERTS. The About panel draws `metadata::current()`, which the next line
    // clears, so a survivor would print the last item's synopsis over the next one's hero — or,
    // past `clear()`, draw nothing while still eating every key the page sends it. Track
    // information is the same argument about the same item.
    crate::ui::about_panel::close();
    crate::ui::tracks_panel::close();
    metadata::clear();
    // Every latch dies with the page — `reset_view_state`'s rule restated for the exit that mounts
    // nothing.
    unsafe {
        *addr_of_mut!(OPEN_REQ) = None;
        *addr_of_mut!(PENDING) = None;
        *addr_of_mut!(KEEP_EP) = None;
        *addr_of_mut!(REFRESH) = None;
        *addr_of_mut!(ALT_REQ) = false;
        *addr_of_mut!(ALT_OPEN) = None;
    }
    let v = view();
    v.selected = -1;
    v.mounted_rk.clear(); // a closed page must not be reconciled by a late refetch
    v.mounted_sid = crate::plex::ServerId::UNSET; // …and its server goes with it
}

pub(crate) fn move_focus(sym: c_int) {
    // An open panel takes the nav keys — focus is trapped inside it, as it is in every other
    // popover this app opens. Track information has no focusable row, so UP/DOWN page its body
    // and LEFT/RIGHT are swallowed rather than reaching the hero row behind it (the same rule
    // `key_more_menu` keeps for a one-column menu: a control that is not visible is not drivable).
    if crate::ui::tracks_panel::is_open() {
        crate::ui::tracks_panel::move_focus(sym);
        return;
    }
    if crate::ui::alt_sources::is_open() {
        crate::ui::alt_sources::move_focus(sym);
        return;
    }
    // The About alert SWALLOWS them. Same rule, opposite reason: it has no list and no control, so
    // there is nothing for a D-pad press to move — but the page behind a modal must not take keys
    // either, or the focus a BACK returns to is somewhere the user never steered it.
    if crate::ui::about_panel::is_open() {
        return;
    }
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
        // Episodes (2) is a 2D block for the same reason About is: a cell is a still with its own
        // focusable metadata block under it ([`EpRow`]). DOWN descends from the still onto that block,
        // UP climbs back — and ONLY DOWN-from-the-text and UP-from-the-still fall through to the
        // generic section change below, which is what makes the two keys exact inverses across the
        // whole block. No spring is jumped: the still's pop is the per-cell `ep_scale`, whose target
        // follows `ep_row` (see `update`), so leaving the still animates the pop OUT instead of
        // dropping it.
        //
        // Gated on the strip actually HAVING episodes, because `section` can outlive them: `open_rk`
        // clears `current()` for the whole fetch, and a DOWN in that window must fall straight through
        // to the generic escape rather than consume itself moving between two rows of nothing.
        if sec == 2 && n_items(2) > 0 {
            let want = if sym == SDLK_DOWN { EpRow::Text } else { EpRow::Still };
            if v.ep_row != want {
                v.ep_row = want;
                return;
            }
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
            // …and the filmstrip's sub-row follows the DIRECTION of the arrival: coming DOWN from the
            // tabs lands on the still, coming UP from the shelf below lands on the metadata block that
            // is nearest — the geometric rule, and the other half of the UP/DOWN inverse above.
            if ns == 2 {
                v.ep_row = if sym == SDLK_UP { EpRow::Text } else { EpRow::Still };
            }
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
/// CACHED per season set, the way `ep_meta_h` is, and for a sharper reason than it: this is asked
/// for THREE times a frame now (the scroll target through `tab_focus_geom`, the capsule spans in
/// `update`, and the draw), and each answer costs one `format!` + one `CString` + one `text_width`
/// per season. Cheap per season — `text::text_width` is a `TTF_SizeUTF8` metrics walk, not a
/// rasterize+upload — but a many-season strip multiplies it by 20-odd, and **no FPS scene
/// covers this row**: `fps:detail-transition` is a MOVIE, which returns from `draw_tabs` before
/// reaching here. So the cost cannot be gated on device and must not be paid per frame. The
/// fingerprint is the item + the season titles themselves, because a `/children` load can rewrite a
/// title in place without changing the count.
///
/// Returns a `&'static` slice into the cache: the CStrings live as long as it does, which is what
/// lets `draw_tabs` hand `lay.label.as_ptr()` to a `TabPill`. **Do not call this from inside a loop
/// that is still reading a previous result** — a rebuild frees those CStrings (the same hazard
/// `widgets::with_tab_metrics` scopes away with a closure; here the fingerprint cannot move
/// mid-frame, since it is derived from `metadata::current()`, which only the pumps replace).
fn tabs_layout(d: &crate::metadata::Detail) -> &'static [TabLay] {
    static mut CACHE: (u64, Vec<TabLay>) = (0, Vec::new());
    let fp = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.rk.hash(&mut h);
        d.seasons.len().hash(&mut h);
        for s in &d.seasons {
            s.title.hash(&mut h);
            s.index.hash(&mut h);
        }
        h.finish().max(1) // 0 is the "never built" sentinel
    };
    unsafe {
        let cache = &mut *addr_of_mut!(CACHE);
        if cache.0 == fp {
            return &cache.1;
        }
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
        *cache = (fp, out);
        &cache.1
    }
}

/// Content-space `(x, w)` of season tab `i`'s PILL — the frame [`tab_pill_rect`] draws, not the
/// label inside it. The one span function [`TabStrip`] is placed from, so a capsule can only ever
/// come to rest exactly on a pill.
fn tab_span(lays: &[TabLay], i: usize) -> Option<(f32, f32)> {
    lays.get(i).map(|l| {
        let r = tab_pill_rect(l, 0.0);
        (r.x, r.w)
    })
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

/// The hero-synopsis TextView — now [`crate::ui::hero_synopsis`], the block BOTH heroes build, so
/// the home billboard and this page can never again disagree about the rung, the leading or the ink.
/// Kept as a one-line alias because this module reads it in four places and the name is the local
/// vocabulary; the argument for the values it carries is on the shared function.
///
/// It is still shared by this page's flow measure (`hero_layout`) and its paint (`draw_hero`), which
/// was the original reason it existed and is unchanged.
fn hero_synopsis<'a>(summary: &'a str, lead: &'a str) -> TextView<'a> {
    crate::ui::hero_synopsis(summary, lead)
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

/// Whether the loaded item has any review score to badge — the ONE rule for whether the hero
/// reserves the ratings band. Answered from the same place the paint reads, so the flow and the
/// pixels agree about a band that comes and goes with the metadata.
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
///
/// `pub(crate)` for one reader outside this page: `widgets`' hero-scrim legibility table grades the
/// contrast of each hero line at the y this chain puts it, rather than re-typing 596/646/700/800 —
/// so moving a pitch here updates the contract instead of silently invalidating it.
pub(crate) struct HeroChain {
    /// The IDENTITY line: "TV Show · Drama · TV-MA" and, riding its end, the media chips.
    pub(crate) meta_y: f32,
    /// The review-score row. Only meaningful, and only given a band, when [`has_ratings`].
    pub(crate) ratings_y: f32,
    pub(crate) syn_y: f32,
    /// The one fine-print line: air date · extent-or-runtime · how it plays. The right-aligned
    /// PEOPLE column is not on it — it is bottom-anchored on the button row below ([`people_bottom`]).
    pub(crate) facts_y: f32,
    pub(crate) btn_y: f32,
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
pub(crate) fn hero_chain(syn_h: f32, has_ratings: bool) -> HeroChain {
    let meta_y = TITLE_BOTTOM + META_DY;
    let ratings_y = meta_y + RATINGS_DY;
    // the synopsis hangs off whichever line is actually above it, so an item with no scores closes
    // the gap instead of leaving the band empty
    let syn_y = if has_ratings { ratings_y } else { meta_y } + SYN_DY;
    let facts_y = syn_y + syn_h.max(SYN_MIN_H) + FACTS_DY;
    // The crew credit claims no band here at all. It was a conditional line of its own, then the tail
    // of the facts row; `Details Screen.dc.html` now puts it in the right-hand PEOPLE column beside
    // the buttons, where it is BOTTOM-anchored ([`people_bottom`]) and so costs the chain nothing —
    // the button row hangs off the facts line for every item, whatever the credits say.
    HeroChain { meta_y, ratings_y, syn_y, facts_y, btn_y: facts_y + BTN_DY }
}

pub(crate) fn update(dt: f32) {
    // The flow top rides the measured hero: buttons bottom + one standardized gap (hero_layout).
    // HOISTED ABOVE the pumps because a landed `Pending::Spot` JUMPS the scroll to `scroll_target`,
    // which measures the flow down from this value — against last frame's hero it would land the
    // restore at the wrong offset. Nothing the pumps write feeds `hero_layout` (it reads `selected`
    // and `current()`), so for every other frame this is the same number in a different place.
    view().column.top = hero_layout(selected()).btn_y + CD + BTN_CONTENT_GAP;
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
    // AFTER pump_season, for the same reason `pump_pending` is: this is what ASKS for the season
    // whose landing the block above then places, so running it first would have the two chasing each
    // other by a frame. (`metadata::pump_detail` runs later in the frame than this whole function,
    // in `app.rs` — which is why the latch is polled every frame rather than driven by the landing.)
    pump_refresh();
    pump_pending(); // after pump_season: a landing season resets the episode focus this restores
    view().spin_ms += dt * 1000.0;
    // targets read view() internally — compute them before borrowing v for the springs
    let sct = scroll_target();
    let hst = ep_hscroll_target();
    let tst = tab_hscroll_target();
    let amb = amb_target(amb_blur()); // reaches view() via `selected` — same rule
    // The season strip's pill spans, resolved BEFORE `view()` is borrowed (same rule as the targets
    // above; `tabs_layout` is cached per season set, so this is a pointer, not a layout pass).
    let tab_lays: &[TabLay] = metadata::current()
        .filter(|d| !d.seasons.is_empty())
        .map(|d| tabs_layout(d))
        .unwrap_or(&[]);
    let tab_sel = metadata::current().map(|d| d.cur_season as c_int).unwrap_or(-1);
    // Which disc the ring is on, resolved BEFORE `view()` is borrowed — same rule as the targets
    // above, since `focus`/`hero_set` both reach `view()` themselves.
    let disc_foc = focused_disc_slot();
    // …and the whole row's, by control index rather than by disc slot: the pop is every control's,
    // the unfurl only the discs'. `focus()` answers -1 off the hero row, which `try_from` turns into
    // "nothing focused" and closes every pop.
    let ctl_foc = usize::try_from(focus()).ok();
    let v = view();
    v.column.scroll.step(sct, K_SCROLL, dt);
    // the page ground dissolves toward the item's colours (the loaded envelope lands mid-visit, so
    // this is the only path that picks it up); `AmbientWash::K` is the one rate every page wash shares
    v.amb.step(amb, AmbientWash::K, dt);
    crate::ui::anim::probe("detail.scroll", v.column.scroll.pos, v.column.scroll.vel, sct, dt);
    v.card_scale.step(crate::ui::widgets::CARD_FOCUS_SCALE, 300.0, dt);
    crate::ui::anim::probe("detail.card", v.card_scale.pos, v.card_scale.vel, crate::ui::widgets::CARD_FOCUS_SCALE, dt);
    // per-episode pop springs: focused cell → CARD_FOCUS_SCALE, all others ease back to 1.0 (so a
    // just-deselected episode animates its pop + lifted shadow out instead of snapping). Step every
    // spring each frame (cheap), same discipline as CardRow.
    // …and only while the STILL sub-row holds focus: with focus on the metadata block the panel under
    // it is the focus mark, so the still eases back down rather than wearing a second one.
    let ecol = if v.section == 2 && v.ep_row == EpRow::Still { v.col.max(0) as usize } else { usize::MAX };
    let en = n_items(2).max(0) as usize;
    for (i, sp) in v.ep_scale.iter_mut().enumerate() {
        let t = if i == ecol && i < en { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 };
        sp.step(t, 300.0, dt);
    }
    // The discs' unfurl: the focused one opens, the other (and both, once the ring leaves the hero
    // row entirely — `focus()` answers -1 off section 0) eases shut, so a disc losing focus animates
    // its capsule closed instead of snapping back to a circle. Stepped every frame like the episode
    // pops above; a `Spring` reports its own motion to `ui::idle`, so the page keeps presenting for
    // exactly as long as one is moving.
    for (slot, sp) in v.disc_unfurl.iter_mut().enumerate() {
        let t = if disc_foc == Some(slot) { 1.0 } else { 0.0 };
        sp.step(t, crate::ui::widgets::K_DISC_UNFURL, dt);
    }
    // The FOCUS POP the same way, and for the same reason the unfurl gets a spring each: the control
    // being left has to keep animating after it has stopped being the focused one.
    v.ctl_pop.step(ctl_foc, dt);
    v.ep_hscroll.step(hst, 240.0, dt);
    crate::ui::anim::probe("detail.epscroll", v.ep_hscroll.pos, v.ep_hscroll.vel, hst, dt);
    v.tab_hscroll.step(tst, 240.0, dt);
    crate::ui::anim::probe("detail.tabscroll", v.tab_hscroll.pos, v.tab_hscroll.vel, tst, dt);
    // …and the capsules that ride inside that scroll, on the shared component's own stiffness (also
    // 240 — the strip and the highlight in it must settle as one object). A season strip with no
    // seasons hands both capsules an empty span table, i.e. "nothing to mark", so they fade out in
    // place rather than holding a stale pill through a `/children` load that emptied the row.
    let tab_foc = if v.section == 1 { v.col } else { -1 };
    v.tabs.update(tab_sel, tab_foc, |i| tab_span(tab_lays, i), dt);
    // Related is the shared home-shelf component now: step its per-card scale springs + scroll spring
    // (focused only when the Related section holds focus, else the scales ease back and scroll freezes).
    let rfoc = (v.section == 3).then_some(v.col.max(0) as usize);
    v.related.update(n_items(3) as usize, rfoc, &RowStyle::HOME, dt);
    // Cast is the same shared shelf component (circular RowStyle::CAST): spring magnification + scroll.
    let cfoc = (v.section == 4).then_some(v.col.max(0) as usize);
    v.cast.update(n_items(4) as usize, cfoc, &RowStyle::CAST, dt);
    // the "Also available" panel's own appear spring + list scroll (no-op while it is closed), and
    // its headless stand-in, which can only be built once the item has LANDED (a mount deliberately
    // keeps `current()` empty for the whole fetch, so at `reset` time there is nothing to copy)
    crate::ui::alt_sources::pump();
    crate::ui::alt_sources::update(dt);
    // the two alerts' appear springs (+ Track information's body scroll) — no-ops while closed,
    // like their neighbour above
    crate::ui::about_panel::update(dt);
    crate::ui::tracks_panel::update(dt);
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
    // THE page transition, as ONE cascade-alpha push at the root of the tree (`ui::nav`, and
    // `home_draw` for the same line one screen over). This page has NO continuous chrome — the
    // shared top tab bar is not on it — so every pixel it draws is content the route change
    // replaces, and there is no `chrome_alpha()` half to write here. The ambient ground rides it
    // too: `Painter::ambient` mixes its corners toward `theme::SURFACE_APP` under a cascade alpha,
    // which is the very colour `frame_clear` lays down below, so the whole page dips to the app
    // ground with no seam and at no extra pass.
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    // Clear to the app's base gray. It is no longer what the page SHOWS below the hero — the ambient
    // ground is (`draw_backdrop`) — but it stays for three reasons: a tiler's clear is a tile init,
    // not a fill; it defines the one frame-shaped hole the ground deliberately skips (the hero, where
    // opaque art covers every pixel); and it is the graceful floor if the ambient program ever fails
    // to link (`gfx.rs` then binds program 0 and draws nothing), in which case the page falls back to
    // looking exactly as it did before it had a ground.
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let env = env_of(0.0);
    let m = selected();
    let scroll = view().column.scroll.pos;
    let amb = view().amb; // Copy, like `let col = view().column` below
    use crate::ui::profile::phase;
    phase("dt.backdrop", || draw_backdrop(p, m, scroll, amb));
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
    // LAST, over everything: this page's modals build their OWN root painter (scrim included)
    // rather than riding the scrolled one — a popover is anchored to where its button was DRAWN,
    // or to the frame, and never to the page's content space. Both are no-ops while closed.
    crate::ui::alt_sources::draw();
    // The two ALERTS are the same kind of modal. About is split in two on purpose: its DIM is the
    // page's (drawn here, at the end of the page) and its PANEL is its own. `ui/CLAUDE.md`'s rule —
    // the scrim is part of what a glass panel looks through, so it must be on the frame before the
    // backdrop is sampled, and `Popover::scrim` + `content_painter` is the pairing that puts it
    // there without compounding with the panel's own fade. All of these are no-ops while closed.
    crate::ui::about_panel::draw_scrim();
    crate::ui::about_panel::draw();
    crate::ui::tracks_panel::draw();
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
                    draw_episodes(p.alpha(EP_STALE_A));
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

/// The catalog row's UltraBlur envelope, if it has one. Split out because MOUNT may use only this
/// one: `open_rk` calls `mount_rk` (and so `reset_view_state`) one statement BEFORE
/// `metadata::clear()`, so for that statement `current()` still names the item we are LEAVING —
/// jumping the ground to it would paint the previous page's colours across this one.
fn row_blur(m: Option<&PmsMovie>) -> Option<[[f32; 3]; 4]> {
    m.filter(|m| m.has_blur).map(|m| m.blur)
}

/// The envelope the ground is keyed to, in preference order: the LOADED item's own (authoritative,
/// and the ONLY source for a page opened from the Library grid, a Related tile or the person page —
/// none of those items are in the home hub catalog `pms::index_of_rk` searches), else the catalog
/// row the page mounted on (which covers the 2-5 round trips `open_rk` deliberately spends with
/// `current()` cleared). Detail FIRST so the ground cannot flicker back to grey when a hub refetch
/// drops the row out from under `selected` — `reselect` re-points by rk, but a row that left the
/// hub is gone.
///
/// A SHOW keys off the show, not its on-deck episode: the backdrop ART is the episode's still
/// ([`hero_episode`]) because the hero is about that episode, but the GROUND is what the whole page
/// is about, and it must not change colour when you browse to another season tab.
fn amb_blur() -> Option<[[f32; 3]; 4]> {
    metadata::current().filter(|d| d.has_blur).map(|d| d.blur).or_else(|| row_blur(selected()))
}

/// The ground's target corners. Pure — the host suite's hook into this whole feature.
///
/// Flat weights, deliberately: the detail page IS one item, so the ground should be the artwork's
/// own corner arrangement rather than a second gradient laid over it (the person page tapers its
/// bottom corners because its band is top-anchored and its shelves live below; nothing here is), and
/// one weight means ONE legibility ceiling for the whole page instead of four.
fn amb_target(blur: Option<[[f32; 3]; 4]>) -> [[f32; 4]; 4] {
    match blur {
        Some(b) => AmbientWash::keyed(b, [AmbientWash::GROUND_W; 4]),
        // No artwork colours → the app's own flat ground, byte-identical to the grey this page
        // showed before it had one. `AmbientWash::target` at w=0 is exactly `SURFACE_APP`, so this
        // needs no branch in the draw.
        None => [theme::SURFACE_APP; 4],
    }
}

/// The hero's atmospheric ramp starts at the shared [`HERO_BASE_SCRIM_Y0`] and runs, in ONE linear
/// stop, to [`BASE_SCRIM_FOOT_A`] at the foot of the panel — home's is a two-stop curve within 0.05
/// alpha of it everywhere, and unifying the two CURVES is deliberately not done: retuning the
/// atmospheric floor is a different decision from fixing the text corner, and only the panel can
/// judge either. Only the origin is shared.
///
/// Its weight at the foot of the panel, for a fully visible hero.
const BASE_SCRIM_FOOT_A: f32 = 0.95;

/// The frame-wide atmospheric ramp's alpha at `y`, for a hero at `hero_vis` — the stop
/// [`draw_backdrop`] paints, as one pure function so the paint and the legibility contract cannot
/// read different numbers. It is the base the hero wedge composites over; `widgets`' anchor table
/// grades `1 − (1 − base_scrim_a) · (1 − hero_scrim_a)` at this page's real text ys, which it reads
/// from [`hero_chain`].
pub(crate) fn base_scrim_a(y: f32, hero_vis: f32) -> f32 {
    BASE_SCRIM_FOOT_A * hero_vis.clamp(0.0, 1.0) * ((y - HERO_BASE_SCRIM_Y0).max(0.0) / (SCR_H - HERO_BASE_SCRIM_Y0)).min(1.0)
}

fn draw_backdrop(p: Painter, m: Option<&PmsMovie>, scroll: f32, amb: AmbientWash) {
    // 0 at the hero, 1 when scrolled down into the rows
    let sf = (scroll / SCROLLED).clamp(0.0, 1.0);
    // Scroll-darkening for the BACKDROP ART's tail, so the row text reads over the last of it.
    // Folded into the art tint rather than a separate full-screen dim pass — one fewer full-screen
    // blit per scrolled frame. The GROUND below needs none of it: it is mixed FROM `SURFACE_APP`
    // under a luminance ceiling, so it is legible at every scroll position by construction — which
    // is exactly what lets it BE the ground instead of a hero-only nicety that has to be hidden
    // under grey once the rows arrive.
    let d = 1.0 - sf * 0.55;
    // Backdrop art. Fades out as the page scrolls into the rows so the episode/row text reads over a
    // dark bg. Resolved FIRST (the texture is cached — this is a cheap lookup) so the ambient wash
    // below can be skipped whenever the opaque full-screen art already covers it — drawing both is a
    // wasted full-screen pass on the weak GPU (the hero's fill-rate cost; mirrors home's Backdrop
    // which draws art OR ambient, never both).
    let art_a = 1.0 - sf;
    // Which picture (`hero_art_path` owns the ladder and its rationale), and at what DECODED size —
    // the size is what lets the blit COVER the panel instead of stretching to it.
    let (art_tex, art_w, art_h) = if art_a > 0.01 {
        resolve_tex_wh_on(hero_art_sid(m), &hero_art_path(m), HERO_ART.0, HERO_ART.1, 0)
    } else {
        (0, 0.0, 0.0)
    };
    // THE PAGE GROUND: the item's UltraBlur corners, opaque, standing in for the flat clear — what
    // every below-hero row sits on, at every scroll position, instead of the app gray. No `has_blur`
    // gate any more: with no envelope the target IS `SURFACE_APP` (`amb_target`), so the "no
    // artwork" page is the old grey with no special case.
    //
    // TWO skips, the same pair `home::Backdrop::draw` applies, and both matter. Skipped while the
    // opaque full-screen art covers every pixel of it — the one-full-screen-pass rule the wash was
    // always drawn under, and one pass CHEAPER than before (scrolled down this used to draw the wash
    // and then bury it under the grey fade-in that lived at the end of this function). And skipped
    // once the wash has RESOLVED to the clear colour, which for an item PMS sent no UltraBlur
    // envelope for is its resting state: `amb_target(None)` is exactly `[SURFACE_APP; 4]`, the
    // colour `frame_clear` laid down one line earlier, so without this test such a page pays a
    // ~2.07M-fragment opaque gradient every frame to write the pixels that are already there.
    if (art_tex == 0 || art_a < 0.99) && !amb.is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS) {
        amb.draw(p, Rect::FULL);
    }
    if art_tex != 0 {
        // COVER, never stretch. `Painter::tex` maps UV 0..1 across the rect, so a source that is not
        // the panel's 16:9 is SQUASHED — and now that an episode's own still can key this backdrop
        // (`hero_art_path` rung 2) that is the routine case, not the exotic one: a still is a video
        // frame whose aspect and resolution we do not control, and a squashed face is worse than a
        // cropped one. `Rect::cover` is a no-op on a 16:9 source, so an ordinary show/movie backdrop
        // is pixel-identical to before, and it returns the frame unchanged while the size is still 0
        // (the pre-decode window). Not snapped: scaled content never is (root `CLAUDE.md`'s
        // rasterization contract) and the cover rect is fractional by construction.
        let frame = Rect::FULL.cover(art_w, art_h);
        p.tex(art_tex, frame, 0.0, theme::with_a(theme::dim(theme::TINT_WHITE, d), art_a)); // white dimmed by scroll `d`
    }
    // The hero's TEXT SCRIM, in two parts — tied to the hero's own fade (they serve that text), so
    // both clear once the hero is gone rather than lingering the whole scroll (a wasted
    // full-screen pass through the back half of the transition).
    //
    // PART ONE: the frame-wide atmospheric ramp ([`base_scrim_a`]). Mood and depth, not legibility:
    // the hero's title band tops out at y=446 and its HERO-72 text fallback's cap top — baselined on
    // `TITLE_BOTTOM` — at y≈514, where this supplies only ~0.20, around 2:1 over bright artwork, and
    // it cannot be raised there without flattening the picture across all 1920px on the same line.
    // (A squarer clearLogo paints higher still, to y=298, where this ramp is nothing at all.)
    let hero_vis = hero_alpha(scroll, HERO_FADE);
    if hero_vis > 0.01 {
        p.rect(
            Rect::new(0.0, HERO_BASE_SCRIM_Y0, SCR_W, SCR_H - HERO_BASE_SCRIM_Y0),
            0.0,
            theme::scrim(0.0),
            theme::scrim(base_scrim_a(SCR_H, hero_vis)),
            0.0,
        );
        // PART TWO: the corners the text is actually IN. `right` — this hero has TWO text corners,
        // the column at `MARGIN_X` and the right-aligned PEOPLE column at x 1270..1830
        // (`draw_hero`'s last block), which the left wedge by definition cannot reach. Asked for on
        // exactly the condition that column is DRAWN on ([`has_people`]): the mirrored wedge exists
        // for it alone, and an item PMS returned no cast and no crew for was paying a quarter of a
        // million blended fragments a frame to darken an empty corner.
        crate::ui::widgets::hero_scrim(p, hero_vis, metadata::current().map(has_people).unwrap_or(false));
    }
    // (Nothing follows. The shelf gray that used to fade in here — so the below-hero rows sat on the
    // app's base gray whatever the item's artwork was — is the GROUND's job now: the wash above is
    // opaque and already legible, and `AmbientWash::GROUND_W` bounds its darkest corner to ≈33/255,
    // clear of the near-black 25/255 that `theme::SURFACE_APP`'s own doc rejects as too dark for a
    // Related/Cast card shadow to read against.)
}

fn draw_hero(p: Painter, env: &Env, m: Option<&PmsMovie>) {
    let tx = MARGIN_X;
    let d_a = theme::TEXT_SECONDARY;
    // (the facts row's own fine-print ink lives in `facts_flow`, which resolves it from the token
    // itself — that is what makes the rung and the ink of every run on that line host-gradeable)
    let d = metadata::current();

    // ---- title: clearLogo (transparent PNG) if loaded, else bold text — one shared band, sized by
    // the constant-AREA rule and contained to the same `HERO_TEXT_W` column the synopsis and every
    // fine-print line below already wrap to. Borrow from the loaded Detail (the common case) —
    // cloning rk/title/summary every frame was pure churn; the catalog fallback (`m`, pre-load
    // frames only) clones from the catalog row. ----
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
    let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
    let band = hero_logo::band_h(LogoRung::Hero);
    HeroLogo::new(hero_art_sid(m), rk, title, LogoRung::Hero).draw(p, Rect::new(tx, TITLE_BOTTOM - band, HERO_TEXT_W, band));

    // ---- meta line: "TV Show · Sci-Fi · Adventure · 18+", or for an EPISODE's own page
    // "<show name> · S1, E1 · TV-MA". ----
    //
    // The leading part answers "what am I looking at", and for an episode the honest answer is the
    // SHOW it belongs to, not its kind: the title band above already says the episode's name, so a
    // bare noun would be the one line on the page that never names the series. It used to read
    // "Movie" outright — `is_show` is false for an episode, and until the shelf context menu's
    // "Go to Episode" made this page a real destination, nothing navigated here to notice.
    //
    // The season/episode ordinal is `fmt::episode_ordinal` — the shared spelling `fmt::episode_kicker`
    // is built on, so an episode's address reads the same here, in the player HUD and in the Up Next
    // caption. Only the title is dropped, because it IS the title band directly above this line.
    let se = d
        .filter(|d| is_episode(d) && d.season > 0 && d.index > 0)
        .map(|d| crate::ui::fmt::episode_ordinal(d.season, d.index))
        .unwrap_or_default();
    let mut parts: Vec<&str> = Vec::new();
    if let Some(d) = d {
        if is_episode(d) {
            // an episode leads with its show, then where in it — genres are the SERIES' and say
            // nothing this line does not already carry
            if !d.show_title.is_empty() {
                parts.push(&d.show_title);
            }
            if !se.is_empty() {
                parts.push(&se);
            }
        } else {
            parts.push(if d.is_show { "TV Show" } else { "Movie" });
            for g in d.genres.iter().take(2) {
                parts.push(g);
            }
        }
        if !d.rating.is_empty() {
            parts.push(&d.rating);
        }
    }
    // the whole y-chain below comes from the ONE shared layout (also feeds the scroll flow)
    let ch = hero_layout(m);
    // The media chips ride the END of this line rather than the fine print below it. That is the
    // mock's own rule, and its reason: **type, genre, rating, resolution and accessibility all answer
    // "what IS this"** — dates, counts and how it plays are a different question, answered one line
    // down. One question per line, so neither line is a list of unrelated facts.
    let mut mx = tx;
    {
        mx += dotted_run(p, &parts, tx, ch.meta_y, theme::size::BODY, d_a, META_SEP_PAD);
    }
    if let Some(d) = d {
        if mx > tx {
            mx += theme::space::SM; // the words → chips gap (the mock's 16px flex gap)
        }
        // the chips centre on THIS line's cap band, so they must be told which type size they ride
        draw_media_badges(p, d, mx, ch.meta_y, theme::size::BODY);
    }

    // ---- review scores, on their own line under the meta row (see `draw_ratings`) ----
    draw_ratings(p, tx, ch.ratings_y);

    // ---- blurb: for a show the NEXT EPISODE's summary behind its bold `S2, E3 · Laura:` prefix (see
    // `hero_blurb`); its measured height is already folded into facts_y/btn_y by hero_layout ----
    let (lead, body) = hero_blurb(m);
    if !body.is_empty() {
        hero_synopsis(&body, &lead).draw(p, Rect::new(tx, ch.syn_y, HERO_TEXT_W, 0.0));
    }

    // ---- air date · season/episode counts (a show) or runtime (a movie) · how it plays · whose
    // copy this is ----
    // Facts of ONE kind: what you get if you press Play, and — for a borrowed item — where the copy
    // came from. The media chips moved up to the identity line and the crew credit moved to the
    // people column, which is what leaves this row a sentence rather than a pile.
    if let Some(d) = d {
        let (date, extent) = hero_facts(d);
        let ext = extent.as_deref().unwrap_or("");
        // The item's SOURCE, last on the line — empty (and so absent entirely) on our own server.
        let credit = shared_by(&d.source);
        // Every candidate is measured through the VERY flow that draws it, so a reserved width and
        // a painted one cannot come to differ. (The `dotted_run_w` this replaced was a second width
        // expression that had to be kept agreeing with the first — `home::heading_flow`'s doc names
        // that pair as the pattern to stop repeating.)
        let width = |f: FactsFit| {
            facts_flow(
                &facts_parts(&date, ext, f),
                fit_credit(&credit, f),
                |s, _, sz, _| run_w(s, sz),
                |_, after| play_mode_w(d, after),
            )
        };
        let fit = facts_fit(!credit.is_empty(), FACTS_R - tx, width);
        // The ladder's last rung: a date that does not fit even alone is ellipsised into whatever
        // the fragment leaves it. Nothing else is on the row by then — `facts_fit` gave both the
        // credit and the extent up before reaching here.
        let elided: String;
        let date_run: &str = if fit.elide {
            let budget = (FACTS_R - tx - play_mode_w(d, true)).max(0.0);
            elided = crate::text::elide(&date, budget, theme::size::CAPTION, 0, false);
            &elided
        } else {
            &date
        };
        facts_flow(
            &facts_parts(date_run, ext, fit),
            fit_credit(&credit, fit),
            |s, dx, sz, ink| match CString::new(s) {
                Ok(c) => p.text(c.as_ptr(), tx + dx, ch.facts_y, sz, ink, 0, 0),
                Err(_) => 0.0,
            },
            |dx, after| draw_play_mode(p, d, tx + dx, ch.facts_y, after),
        );
    }

    // ---- buttons (btn_y from hero_layout) ----
    draw_buttons(p, env, ch.btn_y);

    // ---- the people column, bottom-anchored on the button row ----
    if let Some(d) = d {
        draw_people(p, d, ch.btn_y);
    }
}

/// The trailing SOURCE credit's words — `Shared by friend`, or **empty** for an item that lives on
/// the signed-in user's own server.
///
/// **The person, never the machine.** `friend` is the same handle the shelf headings and the
/// Sources list say; the server's own name (`nas-home`) appears nowhere outside that list and the
/// failure read-out, because "whose copy is this" is answered by a person and "what could not be
/// reached" by a machine.
///
/// An empty handle produces an empty string, and [`facts_flow`] draws nothing at all for one — no
/// separator, no run, no draw call. That is the design's *"on your own server there is no run,
/// not an empty one"* implemented as ABSENCE rather than as a branch that paints nothing visible,
/// which is what makes the attribution free for the single-server install.
/// The facts row wants the empty string for "no owner" (it measures and flows runs by width, and a
/// zero-width run costs nothing), so this is [`crate::ui::fmt::shared_by`] flattened — the words
/// themselves live there, once, for every surface that says them.
fn shared_by(source: &str) -> String {
    crate::ui::fmt::shared_by(source).unwrap_or_default()
}

/// Which members of the facts row survive its width bound — [`facts_fit`]'s answer, and the input
/// that both the measure and the paint flow from, so the row that was graded is the row that is
/// drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FactsFit {
    /// draw the middle clause: a movie's runtime, or a show's `2 seasons, 20 episodes`
    extent: bool,
    /// draw the trailing `\u{b7} Shared by <handle>`
    credit: bool,
    /// the date does not fit even alone, and is ellipsised into what the fragment leaves it
    elide: bool,
}

/// The facts row's YIELD LADDER, pure: what the row gives up when it would cross [`FACTS_R`].
///
/// `w(fit)` measures one candidate through the same [`facts_flow`] that draws it, which is what
/// lets this decide in the row's real geometry while staying a pure function of widths — and what
/// lets the host suite drive it, since no font is open there and every real measurement is 0.
///
/// The rule is this row's own, from the credit tail the fragment replaced: **a member goes entirely
/// before it goes garbled.** The order is
///
/// 1. the TRAILING CREDIT first — it is the one run on the line that is not a fact about the item,
///    so it yields ahead of the facts it follows;
/// 2. then the EXTENT, entirely — series information the season tabs directly below already carry;
/// 3. and only then is a date that cannot fit alone ellipsised.
///
/// The "how this plays" fragment never yields — it is measured first and the clause bends around it
/// (see [`FACTS_R`]). The ladder is MONOTONE: a member given up is never reconsidered, so the row
/// cannot win the credit back by dropping the extent. That is deliberate — the item's own facts
/// outrank the attribution at every rung, not only at the first.
fn facts_fit(has_credit: bool, budget: f32, mut w: impl FnMut(FactsFit) -> f32) -> FactsFit {
    let mut fit = FactsFit { extent: true, credit: has_credit, elide: false };
    if w(fit) <= budget {
        return fit;
    }
    if fit.credit {
        fit.credit = false;
        if w(fit) <= budget {
            return fit;
        }
    }
    fit.extent = false;
    fit.elide = w(fit) > budget;
    fit
}

/// The row's text members for one fit — the date, and the middle clause while it survives.
/// [`facts_flow`] skips an empty member, so a dropped extent takes its separator with it.
fn facts_parts<'a>(date: &'a str, ext: &'a str, fit: FactsFit) -> [&'a str; 2] {
    [date, if fit.extent { ext } else { "" }]
}

/// The credit for one fit, under the same absence rule: given up means empty, and empty means no
/// run rather than an empty one.
fn fit_credit<'a>(credit: &'a str, fit: FactsFit) -> &'a str {
    if fit.credit {
        credit
    } else {
        ""
    }
}

/// The hero's FACTS row, flowed left→right from the text margin: the date/extent clause joined by
/// the row's dimmed `\u{b7}`, then the "how this plays" fragment, then — only for a BORROWED item —
/// the trailing `\u{b7} Shared by friend`.
///
/// **The credit is LAST, after the facts about the item**, because it is a fact about where the
/// item came from; and it is on this line at all, rather than in the badges row (those state what
/// the item IS — its resolution and accessibility class) or on a line of its own (a fifth line in
/// the one column already fighting for height), because it is read once.
///
/// It joins at the line's own rung and the line's own ink — `CAPTION`/[`theme::TEXT_TERTIARY`],
/// regular, like every other word here. It is dim because the LINE is dim, which is the lowest rung
/// in the ladder; taking it lower would put it under the couch legibility floor.
///
/// `run(text, dx, sz, ink) -> advance` is the seam ([`crate::ui::home`]'s `heading_flow` pattern):
/// the draw passes a closure that paints and returns `Painter::text`'s advance, the host tests one
/// that measures, and `mode(dx, after) -> advance` is the same seam for the fragment, which is not
/// text. The drawn row and the graded one are therefore ONE expression — and so is the row the
/// ladder measures, which is the whole reason [`facts_fit`] takes a measure rather than owning one.
///
/// `after` reaches the fragment as "something has actually been emitted", not as "the item has an
/// air date": an item PMS sent neither a date nor an extent for would otherwise open the line with
/// an orphan dot, and so would a credit standing alone on it.
fn facts_flow(
    parts: &[&str],
    credit: &str,
    mut run: impl FnMut(&str, f32, c_int, [f32; 4]) -> f32,
    mode: impl FnOnce(f32, bool) -> f32,
) -> f32 {
    /// The row's separator, drawn as its OWN run at [`theme::TEXT_SEPARATOR`] — a dot sharing the
    /// ink of the words either side JOINS them instead of punctuating them (see [`dotted_run`],
    /// whose join this tail-and-fragment flow cannot be expressed as).
    fn sep(run: &mut impl FnMut(&str, f32, c_int, [f32; 4]) -> f32, dx: f32) -> f32 {
        let sz = theme::size::CAPTION;
        2.0 * FACTS_SEP_PAD + run("\u{b7}", dx + FACTS_SEP_PAD, sz, theme::TEXT_SEPARATOR)
    }
    let sz = theme::size::CAPTION;
    let ink = theme::TEXT_TERTIARY;
    let mut dx = 0.0;
    let mut any = false;
    for part in parts.iter().filter(|s| !s.is_empty()) {
        if any {
            dx += sep(&mut run, dx);
        }
        dx += run(part, dx, sz, ink);
        any = true;
    }
    // The fragment reports what it consumed, and a non-zero advance is what "it drew" means here —
    // the same test `draw_play_mode`'s caller used to make one level up. It matters for the credit
    // behind it: an item with neither a date nor an extent still gets `Direct Play · Shared by …`
    // rather than the two running together.
    let mode_w = mode(dx, any);
    dx += mode_w;
    any |= mode_w > 0.0;
    if !credit.is_empty() {
        if any {
            dx += sep(&mut run, dx);
        }
        dx += run(credit, dx, sz, ink);
    }
    dx
}

/// The hero's **people column** — the crew credit over the cast, right-aligned in its own column
/// beside the action row (`Details Screen.dc.html`).
///
/// The two lines are one block because they are one KIND of fact. That is also the mock's stated
/// reason for the move that created this function: a bare "Converts on server" parked above these
/// two read as a third credit, so the playback state went down to the facts line and the crew credit
/// came up here off it.
///
/// **Bottom-anchored, growing UPWARD** ([`people_bottom`]): the last line sits level with the
/// buttons for every item, and a cast list that takes a second line grows into the air above instead
/// of walking down toward the section flow.
///
/// The lead word is a step dimmer than the names ([`PEOPLE_LABEL_INK`]), the mock's own hierarchy:
/// the label is scaffolding, the name is the content. That needed a RIGHT-aligned lead run, which
/// `TextView::lead` could not do — it anchored at the column's left edge, which on a right-aligned
/// block strands the word across the line's whole slack — so `TextView` learned to place a lead
/// beside line 0's text instead, and to draw it at the block's own weight (`lead_quiet`).
///
/// **[`PEOPLE_INK`], not the mock's `--text-tertiary`** — the one place this column departs from
/// the design, and it is the ground, not the ink, that forces it. See that const.
fn draw_people(p: Painter, d: &metadata::Detail, btn_y: f32) {
    let x = SCR_W - MARGIN_X - PEOPLE_W;
    let mut bottom = people_bottom(btn_y);
    // the cast is the column's LAST line, so it is placed first and everything else stacks above it
    if has_starring(d) {
        let names: Vec<&str> = d.cast.iter().take(PEOPLE_CAST).map(|c| c.tag.as_str()).collect();
        bottom -= people_line(p, "Starring", &names, x, bottom);
    }
    if let Some((label, names)) = hero_credit(d) {
        people_line(p, label, &names, x, bottom);
    }
}

/// One block of the people column, placed by its BOTTOM edge; returns the height it consumed so the
/// caller can stack the next block above it. Wrapping is what makes the height vary, which is
/// exactly why the column is measured from the bottom rather than laid out from a top.
fn people_line(p: Painter, label: &str, names: &[&str], x: f32, bottom: f32) -> f32 {
    // Each PERSON is one wrap atom: their name parts are joined with a no-break space, so a line
    // can break between people but never inside one ("Jennifer" stranded above "Aniston" is the
    // defect this exists to stop — owner-reported). `TextView`'s tokeniser honours the glue.
    let names = names.iter().map(|n| n.replace(' ', "\u{a0}")).collect::<Vec<_>>().join(", ");
    let v = TextView::new(&names, theme::size::CAPTION, PEOPLE_INK)
        .leading(PEOPLE_LEAD)
        .max_lines(PEOPLE_MAXLINES)
        .h(HAlign::Right)
        .lead_quiet(label, PEOPLE_LABEL_INK);
    let h = v.measure_h(PEOPLE_W);
    v.draw(p, Rect::new(x, bottom - h, PEOPLE_W, 0.0));
    h
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
/// at `text_y` in `line_sz` (so they sit ON that line, not near it). Returns the width consumed.
///
/// They ride the IDENTITY line — "TV Show · Drama · TV-MA  \[4K\] \[CC\]\[SDH\]\[AD\]" — because
/// resolution and accessibility answer the same question the words before them do. That is why
/// `line_sz` is a parameter rather than a constant: the chips sat on the CAPTION fine print until
/// 2026-08-12 and now sit on a BODY run, and a chip centred on the wrong band sits visibly high.
///
/// They describe the item's PRIMARY version — `Media[0]`, see `plex::Metadata::primary_media` — so
/// a multi-version item (the dev library has episodes with both a 4k and a 1080 version) is badged
/// by whichever one PMS lists first, until there is a version picker to choose with.
fn draw_media_badges(p: Painter, d: &metadata::Detail, x: f32, text_y: f32, line_sz: c_int) -> f32 {
    let mut bx = x;
    // cap-band centre of the line drawn at `text_y` (the inverse of text::text_vcenter_y), computed
    // once — every chip below sits on it
    let (cap_top, baseline) = crate::text::text_cap_band(line_sz, 0);
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
    // **`CC` is back** (2026-08-12, the design's own revision, and flagged for re-decision rather
    // than silently re-dropped): it had been cut here on an owner call — "has subtitles at all" is
    // true of very nearly every item in a library, so the chip was on constantly and told a viewer
    // nothing, while `SDH` (a track flagged hearing-impaired) and `AD` (audio description) are rare
    // and are the ones that answer "is this watchable for me". The mock reinstates all three, and the
    // design is the visual baseline.
    let cc = !d.subs.is_empty();
    let sdh = d.subs.iter().any(|s| s.sdh);
    let ad = d.audio.iter().any(|s| s.ad);
    for (on, label) in [(cc, "CC"), (sdh, "SDH"), (ad, "AD")] {
        if !on {
            continue;
        }
        if bx > x {
            bx += theme::space::XS;
        }
        bx += crate::ui::widgets::keyline_chip(p, bx, cy, label, theme::TEXT_SECONDARY);
    }
    bx - x
}

/// Which "how this plays" sentence the facts row carries (`Details Screen.dc.html`; the states are
/// derived from Plex's own Pass documentation, per the owner's standing directive — the design is
/// the visual baseline, the LOGIC follows the docs).
///
/// The whole point of resolving it as a value is that the row shows exactly ONE of these, and the
/// precedence between the two Pass-gated ones is a judgement worth stating in one place instead of
/// re-deriving at a paint site: **a wrong picture outranks a slower one.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlayNote {
    /// The plain fact, in the row's own fine print: "Direct Play" / "Direct Stream" / "Converts on
    /// server". Every server reaches it, because h264 SOFTWARE encoding is subscription-free (the
    /// design revision that assumed "a Pass-less server cannot convert at all" is not what Plex's
    /// docs say, and the issue-#22 target-chain fix depends on the opposite). It is also what an
    /// Unknown subscription gets, always: an absence the app cannot prove must not be named — and
    /// what a container-only REMUX gets on ANY subscription, because no encoder runs.
    Quiet,
    /// "Converts on server · hardware conversion needs \[PLEX PASS\]" — **no alert glyph, quiet
    /// register.** It will convert and the server is PROVEN Pass-less, so hardware conversion is
    /// unavailable: certain, and a SERVER fact rather than a prediction about this item (which track
    /// converts, and whether the server would reach for a hardware encoder, are both unknown before
    /// Play). A slower conversion is not a wrong picture, which is why it gets no severity mark and
    /// the capsule is the only gold on the line.
    Soft,
    /// "⚠ HDR → SDR · tone-mapping needs \[PLEX PASS\]" — the one warning the docs make real. **HDR
    /// tone mapping is a Plex Pass server feature** (support.plex.tv, Transcoder), so an HDR item
    /// that must convert on a server *known* to have no Pass arrives washed-out and nothing
    /// client-side can fix it; the only useful moment to say so is before Play. Same sentence shape
    /// as [`PlayNote::Soft`] — `fact · requirement needs [capsule]` — and the ONE difference is the
    /// alert glyph: severity by mark, never by hue or by a second shape.
    Warn,
}

/// The three-state gate, pure — the truth table the tests exercise and the paint obeys.
///
/// `Warn` needs all three facts proven (a real conversion, an HDR source, and a *proven* missing
/// subscription); `Soft` is the same minus the HDR, because hardware conversion is Pass-gated
/// whatever the source is; everything else is [`PlayNote::Quiet`]. `Unknown` and `Yes` are silent
/// about the subscription by construction — asserting an unproven absence is issue #22's bug with
/// the polarity flipped.
///
/// **`converts` is the RE-ENCODE, not "not direct play".** Both Pass-gated states are claims about
/// an ENCODER — hardware conversion is one, HDR tone mapping is a step inside one — so a
/// [`crate::route::Preview::Remux`] earns neither: the server copies the codecs into MKV, the
/// picture arrives 4K/HDR10 intact, and no server-side encode happens for a subscription to gate.
/// Reading the gate as "anything that is not direct play" put the flagship HDR warning on a 4K HDR
/// HEVC file in a `.mov`, and the soft note on every mkv whose only fault was an audio track that
/// had to be converted — pointing the user at a purchase that would fix nothing, which is the one
/// thing `player::error_shape`'s own rule forbids.
fn play_note(
    pv: crate::route::Preview,
    hdr: bool,
    sub: crate::plex::serverinfo::Subscription,
) -> PlayNote {
    let converts = pv == crate::route::Preview::Converts;
    let no_pass = sub == crate::plex::serverinfo::Subscription::No;
    if converts && no_pass && hdr {
        PlayNote::Warn // a wrong picture outranks a slower one
    } else if converts && no_pass {
        PlayNote::Soft
    } else {
        PlayNote::Quiet
    }
}

/// The Plex Pass claim that applies to THIS item: the subscription of the server it came from.
///
/// **Never `serverinfo::subscription()`**, which answers for whichever server is *current* — and
/// `current` stays pinned to the primary while a shared library is browsed (`plex::servers`' rule),
/// so a borrowed film's note was drawn from OUR server's answer. Both polarities are silent and
/// wrong, which is why this is a named function rather than an inline call: with our own server
/// Pass'd, a friend's free server converting a film says nothing at all where the design asks it to
/// say "hardware conversion needs [PLEX PASS]"; with ours free, every borrowed film wears a warning
/// about a subscription that is not the one doing the work. `subscription_of` exists for exactly
/// this, and `Detail::sid` is the item's own server, captured at fetch time.
///
/// An item with no server on it ([`crate::plex::ServerId::UNSET`] — a host fixture, or a row parsed
/// before any server was registered) reads as `Unknown`, which [`play_note`] is silent about.
fn item_subscription(d: &metadata::Detail) -> crate::plex::serverinfo::Subscription {
    crate::plex::serverinfo::subscription_of(d.sid)
}

/// The alert glyph's box on the facts row — the line's own type size, which is the 24-unit svg the
/// mock inlines on a CAPTION line.
const FACTS_GLYPH_D: f32 = theme::size::CAPTION as f32;

/// One element of the "how this plays" fragment.
///
/// The fragment is **measured and drawn from the same list**, which is the whole reason it is a
/// list rather than a run of `bx +=` statements: the facts clause to its left has to reserve room
/// for it (see [`FACTS_R`]), and a second hand-written width expression is exactly how a reserved
/// width and a painted width come to differ.
#[derive(Clone, Copy)]
enum Bit {
    /// a run of the row's fine print — `(text, ink, bold)`
    Word(&'static core::ffi::CStr, [f32; 4], c_int),
    /// the `·` separator with `gap` px of air either side
    Sep(f32),
    /// the bare alert triangle, at the line's own type size
    Glyph,
    /// plain air
    Air(f32),
    /// the non-interactive PLEX PASS capsule
    Capsule,
}

/// The longest fragment [`play_mode_bits`] builds is the warning's seven; eight leaves one spare.
const FACTS_BITS: usize = 8;

/// The fragment's content, as the list both the measure and the paint walk. `after` says whether
/// anything precedes it on the row — a separator introduces the fragment, and an item PMS sent no
/// air date and no extent for would otherwise open the line with an orphan `·`.
fn play_mode_bits(d: &metadata::Detail, after: bool) -> ([Bit; FACTS_BITS], usize) {
    let mut bits = [Bit::Air(0.0); FACTS_BITS];
    let mut n = 0;
    let Some(pv) = crate::route::playback_preview(d) else { return (bits, 0) };
    let mut push = |b: Bit| {
        bits[n] = b;
        n += 1;
    };
    let dim = theme::TEXT_TERTIARY;
    let sec = theme::TEXT_SECONDARY;
    if after {
        // the row-level separator, at the same air the date/extent pair either side of it uses
        push(Bit::Sep(theme::space::MD));
    }
    match play_note(pv, d.hdr, item_subscription(d)) {
        PlayNote::Quiet => push(Bit::Word(
            match pv {
                crate::route::Preview::DirectPlay => c"Direct Play",
                // Plex's own name for a container-only remux, and the same word
                // `info_panel::playback_now` reports for the LIVE session — the picture is copied,
                // so calling it a conversion would state that the server touched the pixels.
                crate::route::Preview::Remux => c"Direct Stream",
                crate::route::Preview::Converts => c"Converts on server",
            },
            dim,
            0,
        )),
        PlayNote::Soft => {
            push(Bit::Word(c"Converts on server", dim, 0));
            // inside the fragment the air closes one rung: these clauses are one sentence, where the
            // row-level dots separate three independent facts
            push(Bit::Sep(theme::space::SM));
            push(Bit::Word(c"hardware conversion needs", sec, 0));
            push(Bit::Air(theme::space::SM));
            push(Bit::Capsule);
        }
        PlayNote::Warn => {
            // The glyph stands BARE beside the words, not inside a keyline chip: the mock's warning
            // row is a plain run, and boxing the severity mark made it read as one more metadata
            // chip in a row that already has several.
            push(Bit::Glyph);
            push(Bit::Air(theme::space::SM));
            push(Bit::Word(c"HDR \u{2192} SDR", sec, 1));
            push(Bit::Sep(theme::space::SM));
            push(Bit::Word(c"tone-mapping needs", sec, 0));
            push(Bit::Air(theme::space::SM));
            push(Bit::Capsule);
        }
    }
    (bits, n)
}

/// Drawn width of one fine-print run — the facts row's own measure, taking the `sz` [`facts_flow`]
/// hands out rather than assuming one, so the measured row is the flowed row (0 for a run the text
/// layer cannot measure, which is the same "unmeasurable → 0" contract `text_width` keeps).
fn run_w(s: &str, sz: c_int) -> f32 {
    CString::new(s).ok().map(|c| crate::text::text_width(c.as_ptr(), sz, 0)).unwrap_or(0.0)
}

fn bit_w(b: Bit) -> f32 {
    match b {
        Bit::Word(c, _, bold) => crate::text::text_width(c.as_ptr(), theme::size::CAPTION, bold),
        Bit::Sep(gap) => 2.0 * gap + crate::text::text_width(c"\u{b7}".as_ptr(), theme::size::CAPTION, 0),
        Bit::Glyph => FACTS_GLYPH_D,
        Bit::Air(g) => g,
        Bit::Capsule => crate::ui::widgets::pass_capsule_w(),
    }
}

/// The width "how this plays" will occupy — the layout companion the facts clause measures against.
fn play_mode_w(d: &metadata::Detail, after: bool) -> f32 {
    let (bits, n) = play_mode_bits(d, after);
    bits[..n].iter().copied().map(bit_w).sum()
}

/// "How this plays", flowed after the date/extent clause at `x`; returns the width consumed.
fn draw_play_mode(p: Painter, d: &metadata::Detail, x: f32, text_y: f32, after: bool) -> f32 {
    let (cap_top, baseline) = crate::text::text_cap_band(theme::size::CAPTION, 0);
    let cy = text_y + (cap_top + baseline) * 0.5;
    let (bits, n) = play_mode_bits(d, after);
    let mut bx = x;
    for b in &bits[..n] {
        match *b {
            Bit::Word(c, col, bold) => {
                bx += p.text(c.as_ptr(), bx, text_y, theme::size::CAPTION, col, 0, bold);
            }
            Bit::Sep(gap) => {
                // dimmed against the words either side (`theme::TEXT_SEPARATOR`) — both mocks set
                // every separator dot to .45, so it punctuates the row instead of joining it
                p.text(c"\u{b7}".as_ptr(), bx + gap, text_y, theme::size::CAPTION, theme::TEXT_SEPARATOR, 0, 0);
                bx += bit_w(*b);
            }
            Bit::Glyph => {
                crate::ui::icons::draw(
                    p,
                    crate::ui::icons::Icon::Alert,
                    Rect::new(bx, cy - FACTS_GLYPH_D * 0.5, FACTS_GLYPH_D, FACTS_GLYPH_D),
                    theme::TEXT_SECONDARY,
                );
                bx += FACTS_GLYPH_D;
            }
            Bit::Air(g) => bx += g,
            Bit::Capsule => bx += crate::ui::widgets::pass_capsule(p, bx, cy, false),
        }
    }
    bx - x
}

/// `RatingArt` → the mask layers that draw its VERDICT. The mapping lives here, in the UI layer,
/// so `metadata::RatingArt` (parsed out of the server's `Rating.image` string) stays free of
/// `Icon`/colour — and so the *state* the server named is what picks the art, end to end.
///
/// An empty slice means "this provider has no verdict to draw". IMDb and TMDB publish a number and
/// nothing else; they used to get a logotype chip in their brand colours, and before that a generic
/// star, and both were answering "whose score is this?" — which the group's caption now answers in
/// words. A mark here has to mean something, or it should not be drawn.
fn rating_mark(art: metadata::RatingArt) -> &'static [crate::ui::widgets::MarkLayer] {
    use crate::ui::icons::Icon;
    use crate::ui::widgets::MarkLayer;
    use metadata::RatingArt as A;

    // Each mark's colour layers, BACK TO FRONT — the order is the drawing, since they overlap.
    // `static` so the slices outlive the call.
    /// Ripe: red body, green calyx over it.
    static FRESH: &[MarkLayer] =
        &[(Icon::Tomato, theme::RATING_FRESH), (Icon::TomatoCalyx, theme::RATING_LEAF)];
    /// Certified: the SAME fruit struck in gold. A rarer bar reads as a richer version of the
    /// thing — and the retired art's alternative, RT's own Certified Fresh seal, was the one shape
    /// in the set that was unmistakably somebody's award rather than a piece of fruit.
    static CERTIFIED: &[MarkLayer] =
        &[(Icon::Tomato, theme::RATING_CERTIFIED), (Icon::TomatoCalyx, theme::RATING_LEAF)];
    /// Rotten: the same fruit drained to an outline, calyx included. Negation by losing the colour,
    /// not by acquiring a second device.
    static ROTTEN: &[MarkLayer] =
        &[(Icon::TomatoHollow, theme::RATING_MUTED), (Icon::TomatoCalyx, theme::RATING_MUTED)];
    /// A crowd that liked it.
    static CROWD_UP: &[MarkLayer] = &[(Icon::Crowd, theme::RATING_AUDIENCE)];
    /// The same crowd, drained. One layer, so it cannot go hollow the way the fruit does — see
    /// `Icon::Crowd`.
    static CROWD_DOWN: &[MarkLayer] = &[(Icon::Crowd, theme::RATING_MUTED)];

    match art {
        A::TomatoFresh => FRESH,
        A::TomatoCertified => CERTIFIED,
        A::TomatoRotten => ROTTEN,
        A::PopcornUpright => CROWD_UP,
        A::PopcornSpilled => CROWD_DOWN,
        A::Imdb | A::Tmdb => &[],
    }
}

/// The review-score row — **its own line under the meta line**, flowed left→right from the text
/// margin and centred on the band a [`theme::size::LABEL`] score occupies (so the row sits on a real
/// text line rather than at a guessed y). A group that would cross the right margin is dropped and
/// the flow stops there.
///
/// It used to trail the genre line inline, which put a tomato at a different x on every item and
/// gave the row whatever width the genres left over — on a three-genre show the fourth score fell off
/// the panel. `Details Screen.dc.html` gives it a line, and a line is what it needs. The band is only
/// reserved when there is at least one score (see [`hero_chain`]), so a title the agents found no
/// reviews for closes the gap instead of leaving a hole.
///
/// The unit of flow is the PROVIDER, not the score: `ratings` arrives sorted by
/// `RatingArt::rank`, which already puts Rotten Tomatoes' two states adjacent, so this is a
/// run-length pass over equal [`metadata::RatingArt::provider`] names.
fn draw_ratings(p: Painter, x: f32, row_y: f32) {
    let ratings = match metadata::current() {
        Some(d) if !d.ratings.is_empty() => &d.ratings,
        _ => return,
    };
    let (cap_top, baseline) = crate::text::text_cap_band(theme::size::LABEL, 1);
    let cy = row_y + (cap_top + baseline) * 0.5;
    let mut bx = x;
    let mut i = 0;
    while i < ratings.len() {
        let caption = ratings[i].art.provider();
        let end = ratings[i..].partition_point(|r| r.art.provider() == caption) + i;
        // Borrowed by `RatingCell`, so they have to outlive the draw — hence the two vectors
        // rather than formatting inside the map.
        let scores: Vec<String> = ratings[i..end]
            .iter()
            .map(|r| crate::ui::fmt::rating_score(r.art, r.value))
            .collect();
        let cells: Vec<crate::ui::widgets::RatingCell> = ratings[i..end]
            .iter()
            .zip(scores.iter())
            .map(|(r, s)| crate::ui::widgets::RatingCell {
                mark: rating_mark(r.art),
                value: s,
                suffix: crate::ui::fmt::rating_suffix(r.art),
            })
            .collect();
        let w = crate::ui::widgets::rating_group_w(caption, &cells);
        if w <= 0.0 || bx + w > SCR_W - MARGIN_X {
            break;
        }
        crate::ui::widgets::rating_group(p, bx, cy, caption, &cells);
        bx += w + RATING_ROW_GAP;
        i = end;
    }
}

fn draw_buttons(p: Painter, env: &Env, y: f32) {
    // One shared control family (widgets::Button/CircleButton, all on the default
    // ControlStyle::Accent — the same as the info card): the focused control fills warm ACCENT with
    // dark ink, the rest are solid dark discs. Play reuses the pill Button; the ▶| disc restarts a
    // resumable item from 00:00; the ✓/− toggle writes watch state. Both discs UNFURL their verb on
    // focus (`disc_verb`), which is the only way either says what it does. They are placed by an
    // ACCUMULATED x, not per-control constants, because what sits between them comes and goes — see
    // `hero_btns`. That accumulation IS `hero_btn_rect`, which the pointer hit-test walks too, so no
    // control can be drawn in one place and clicked in another.
    //
    // The set and every measured width are resolved ONCE for the whole pass and walked from there
    // (the discipline `tabs`/`amb` take in `draw`): each width is a `TTF_SizeUTF8`, and asking
    // `hero_btn_rect` per control re-measured the entire row once per control — five controls
    // paying for twenty-five. `hit_at` hoists the same pair for the same reason, and it is the one
    // that matters most: it runs on every pointer MOTION event.
    let focus = focus();
    let restart = has_restart();
    let set = hero_set();
    let cw = hero_widths();
    let rect = |i: c_int| hero_btn_rect_at(set, i, y, cw);
    // The FOCUS POP, per control (`widgets::CtlPop`). It is a factor on the frame, applied by the
    // widget about the control's centre, so it does NOT feed `hero_btn_rect_at` — the row's
    // accumulation stays the resting one and the controls beside a popped one do not shuffle. The
    // hit-test reads that same resting rect, which is the one thing to know here: the popped face
    // strictly CONTAINS its resting rect, so every pixel that was clickable still is and only the
    // ~2px halo the pop adds is not. That is the trade every popped tile in this app already makes
    // (`home::focused_card_rect` leaves `press::scale()` out for the same reason).
    let pop = |i: c_int| view().ctl_pop.scale(i.max(0) as usize);

    // The pill says what the press will DO. Home's hero says "Continue" — that row belongs to the
    // Continue Watching shelf, and the word is the shelf's. On an item page the control has a
    // sibling: "Resume" and the ▶| disc beside it are a PAIR, read together, the way every reference
    // client words it — and the disc now spells out its half ("Play from Start") when focused. The pill's width follows its label (`Button::pill_w`, the ONE pill width
    // formula, reached through `hero_pill_w` so the pointer measures the very same frame) — the
    // longer word gets its own air instead of being crammed into "Play"'s.
    let plabel = hero_pill_label();

    Button::new(plabel.as_ptr(), theme::size::BODY, rect(BTN_PLAY))
        .icon(crate::ui::icons::Icon::Play)
        .focused(focus == BTN_PLAY)
        .scale(pop(BTN_PLAY))
        .draw(env, p);
    if restart {
        // **`Icon::Restart`, the `↺`, and NOT the menu row's `Icon::PlayStart`** — the one place in
        // the app where those two marks for one action are drawn differently, and it is deliberate.
        // This disc stands ~20px from the Play/Resume pill and is icon-ONLY at rest, so its glyph
        // has to carry the whole "this is not that button" by itself; `play-start` is a play
        // triangle with a leading bar, which against the pill's own triangle is a 3px stem seen
        // from three metres. The menu row has the words *Play from Start* beside its glyph and
        // nothing resembling a play mark near it, so it keeps the better icon. `icons::Restart`
        // argues the split; it was reconciled the other way on 2026-08-21 and that was wrong.
        //
        // What the arrow does NOT say is WHAT it restarts — on a show page that is the on-deck
        // episode, not the series — and that is answered where it always was, by the unfurled verb,
        // in the words the menu already uses.
        draw_disc(p, env, crate::ui::icons::Icon::Restart, HeroCtl::Restart, rect(BTN_RESTART), focus == BTN_RESTART, pop(BTN_RESTART));
    }
    // "Also available" — drawn ONLY while a second pinned source holds this item (`hero_ctls`), so
    // the 90% single-server library never pays a control for it. The trailing chevron is what says
    // the press opens a list rather than acting: it is the same disclosure mark a drill-in row
    // carries, and without it the pill reads as a fourth thing to DO to the film.
    if let Some(i) = index_of(set, HeroCtl::Alt) {
        Button::new(ALT_LABEL.as_ptr(), theme::size::BODY, rect(i))
            .trailing_icon(crate::ui::icons::Icon::ChevronDown)
            .focused(focus == i)
            .scale(pop(i))
            .draw(env, p);
    }
    // The watched TOGGLE — always exactly one (`hero_ctls`). It **states the OUTCOME its press
    // produces** rather than the state the item is in — the design system's rule for a control that
    // is already a circle (`CircleButton.d.ts`): the BARE check / minus, never the filled
    // check-circle / minus-circle a menu ROW carries, because the button is the circle and a disc
    // inside a disc would be two circles saying one thing.
    //
    // Both are on the ordinary idle/accent style. There used to be an amber `Custom` arm that inked
    // a watched-and-unfocused check `RESUME_FILL`, from when ONE disc had to carry both states; the
    // glyph carries the state now, and amber has since become the resume BAR's alone (`theme.rs`)
    // precisely so the two cannot be confused at a glance.
    //
    // …and it UNFURLS while it holds focus, into a capsule carrying the verb its press performs
    // (`disc_verb`). The mark alone is the one thing in this row nobody can read: a pill says its
    // word and a chevron says "this opens", but a bare ✓ over a backdrop is a shape.
    for (ctl, icon) in [
        (HeroCtl::MarkWatched, crate::ui::icons::Icon::Check),
        (HeroCtl::MarkUnwatched, crate::ui::icons::Icon::Minus),
    ] {
        let Some(i) = index_of(set, ctl) else { continue }; // one of the two faces, never both
        draw_disc(p, env, icon, ctl, rect(i), focus == i, pop(i));
    }
}

/// One of the hero row's DISCS: the glyph, and the verb it unfurls to while focused.
///
/// Shared by the restart disc and the watched toggle because the two differ in exactly their icon
/// and their verb, and in nothing else — the frame, the unfurl spring, the style and the focus rule
/// are one behaviour. `frame`, never `at`: the row has already reserved this control's unfurled
/// width, and a disc that kept its own 60 would draw its capsule clipped back to a circle
/// (`CircleButton::frame`).
fn draw_disc(
    p: Painter,
    env: &Env,
    icon: crate::ui::icons::Icon,
    ctl: HeroCtl,
    r: Rect,
    focused: bool,
    scale: f32,
) {
    let mut b = CircleButton::new(c"".as_ptr()).icon(icon).frame(r).focused(focused).scale(scale);
    if let Some((slot, label)) = disc_verb(ctl, watch_names_show()) {
        b = b.label(label.as_ptr(), view().disc_unfurl[slot].pos);
    }
    b.draw(env, p);
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

/// small centered clearLogo/title shown at the top once the page is scrolled. The band spans the
/// full content width rather than the old `f32::MAX` "no column at all", so a long show title elides
/// instead of running off the panel; its centre is still exactly `SCR_W * 0.5`.
fn draw_compact_title(p: Painter, m: Option<&PmsMovie>) {
    let d = metadata::current();
    let rk_owned = if d.is_none() { m.map(|m| m.rk.clone()).unwrap_or_default() } else { String::new() };
    let rk: &str = d.map(|d| d.rk.as_str()).unwrap_or(&rk_owned);
    let title_owned = if d.is_none() { m.map(|m| m.title.clone()).unwrap_or_default() } else { String::new() };
    let title: &str = d.map(|d| d.title.as_str()).unwrap_or(&title_owned);
    let band = hero_logo::band_h(LogoRung::Compact);
    HeroLogo::new(hero_art_sid(m), rk, title, LogoRung::Compact)
        .align(HAlign::Center)
        .draw(p, Rect::new(MARGIN_X, COMPACT_TITLE_BOT - band, SCR_W - 2.0 * MARGIN_X, band));
}

/// Season tab row: a plate under every season, a travelling capsule on the ACTIVE one and a brighter
/// travelling capsule on the FOCUSED one, with each label's ink derived from how covered it is.
fn draw_tabs(p: Painter) {
    let d = match metadata::current() {
        Some(d) => d,
        None => return,
    };
    if d.seasons.is_empty() {
        return;
    }
    let tab_y = 0.0; // local origin — the ScrollColumn pre-translates the painter to this section's top
    // many-season shows (20-odd seasons) overflow the row width, so the whole strip scrolls horizontally to
    // keep the focused tab visible (spring target in `tab_hscroll_target`); off-screen tabs are culled.
    let sx = view().tab_hscroll.pos;
    let pt = p.translate(-sx, 0.0);
    // Segmented control, one motion: the selected season and the focused one are TRAVELLING capsules
    // the strip owns (`TabStrip`, the same component and the same springs as the top tab row — the
    // owner directive that keeps the two rows indistinguishable covers their motion). Drawn FIRST,
    // because they are the pills' ground; each pill then paints only its own idle plate and its
    // label, and takes its ink from how covered it is.
    let tabs = view().tabs; // Copy, resolved once — like `amb` in `draw`
    tabs.draw(pt, tab_y, TAB_ROW_H, true, false);
    let e = Env::inert();
    for lay in tabs_layout(d) {
        // pill sized to the (bold) label plus its trailing note — content sits at x, pill padded
        // ±TAB_PAD, tabs advance by that content width + TAB_ADVANCE (see tabs_layout). The pill
        // fills the block height (TAB_ROW_H == the hero-button CD), so tabs and buttons read as one
        // control family. `tab_pill_rect` is that frame, shared with the pointer hit-test — a tab
        // widened by its watched tick must stay clickable across the whole pill it draws.
        let pill = tab_pill_rect(lay, tab_y);
        if on_axis(pill.x - sx, pill.w, SCR_W, 0.0) {
            let (fm, sm) = tabs.mixes((pill.x, pill.w));
            TabPill::new(lay.label.as_ptr(), theme::size::BODY, pill)
                .plated() // no tab-bar track under these — the pills are their own ground
                .mix(fm, sm)
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
    // Which of the cell's two focus rows is lit ([`EpRow`]): the still wears the pop + ring, the
    // metadata block wears a panel behind it. Exactly one of them, ever.
    let erow = view().ep_row;
    // the ui::press click dip folds into the focused episode's per-cell pop below
    let press = crate::ui::press::scale();
    // keep the focused card on-screen (spring-scrolled so it glides to the 2nd slot instead of
    // snapping — matches the chapters strip; fixes the "scatter" on LEFT/RIGHT)
    let sx = view().ep_hscroll.pos;
    let pe = p.translate(-sx, 0.0);
    let ep_y = 0.0; // local origin (ScrollColumn pre-translates to this section's top)
    for (i, ep) in d.episodes.iter().enumerate() {
        let x = strip_x(i, EP_W + EP_GAP);
        if !on_axis(x - sx, EP_W, SCR_W, 0.0) {
            continue; // off-screen
        }
        draw_episode_cell(pe, d, i, ep, focus_col, erow, press, ep_y);
    }
}

/// ONE episode cell of the filmstrip: the still with its marks, and the metadata block under it.
///
/// A function rather than the body of [`draw_episodes`]' loop because the FOCUSED cell is drawn
/// twice while its context menu is up — once in the strip, and once more over the modal scrim
/// ([`redraw_focused_episode`]), so the tile the popover is anchored beside does not recede with the
/// page behind it. One body, so the lifted copy is the drawn cell and not a second expression of it.
///
/// `pe` is the strip painter — already translated by `-ep_hscroll`, so `x` here is a CONTENT
/// coordinate and `ep_y` the section-local origin, exactly as the loop had them.
#[allow(clippy::too_many_arguments)]
fn draw_episode_cell(
    pe: Painter,
    d: &metadata::Detail,
    i: usize,
    ep: &metadata::Episode,
    focus_col: c_int,
    erow: EpRow,
    press: f32,
    ep_y: f32,
) {
    let dimc = theme::TEXT_TERTIARY;
    let x = strip_x(i, EP_W + EP_GAP);
    let focused = i as c_int == focus_col;
    // …and whether it is the STILL that is focused. The metadata block below has its own mark, so
    // the still keeps neither the pop nor the ring while focus is down there.
    let still_focused = focused && erow == EpRow::Still;
    // per-cell pop: the focused episode springs up (press dip folded in); a just-deselected one
    // springs back down, so its scale + lifted shadow animate out instead of snapping. Episodes
    // past the spring cap snap to the focus scale (no per-cell animation) rather than losing the
    // focus cue entirely.
    let base = view()
        .ep_scale
        .get(i)
        .map(|s| s.pos)
        .unwrap_or(if still_focused { crate::ui::widgets::CARD_FOCUS_SCALE } else { 1.0 });
    let sc = if still_focused { base * press } else { base };
    // the focused cell is always "popped" so a press dip (sc < 1) still scales it (the dip would
    // otherwise be clamped away); a deselecting cell stays popped until its spring settles back.
    let popped = still_focused || sc > 1.001;
    let card = Rect::new(x, ep_y, EP_W, EP_H);
    // episode still + focus ring + scale-pop (shared with the chapters strip)
    crate::ui::widgets::draw_card(pe, card, d.sid, &ep.thumb, (640, 360), 12.0, popped, sc);
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
    let (date_y, summary_y, meta_h) = ep_meta_layout(&ep.title, !date.is_empty(), &ep.summary);
    // …and when it is the METADATA BLOCK that holds focus, its own translucent panel goes down
    // first, wrapping this episode's measured content (`ep_text_hl`). The panel itself is the
    // SHARED `widgets::text_block_highlight` — literally the same call the About footer's column
    // highlight makes, so "this block of text is the thing you are on" is one object with one
    // wash, one radius and one shadow. This screen supplies only the frame it hugs.
    if focused && erow == EpRow::Text {
        crate::ui::widgets::text_block_highlight(pe, ep_text_hl(x, ep_y, meta_h));
    }
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
            // tile, and at badge's band and weight it outweighed the episode title above it.
            if !ep.rating.is_empty() {
                let (ct, cb) = crate::text::text_cap_band(theme::size::MICRO, 0);
                crate::ui::widgets::keyline_chip(
                    pe,
                    x + w + theme::space::SM,
                    ty + date_y + (ct + cb) * 0.5,
                    &ep.rating,
                    dimc,
                );
            }
        }
    }
}

/// Re-draw the focused episode cell ON TOP of a modal scrim — this page's half of
/// [`crate::ui::popover::Opener`], handed to the item context menu when it opens over the
/// filmstrip.
///
/// It rebuilds the two translates [`draw_episodes`] gets for free from the `ScrollColumn`
/// (the section's screen top) and from the strip (`-ep_hscroll`), then runs the same
/// [`draw_episode_cell`]. `section_screen_top` returning `None` — the filmstrip scrolled off, or no
/// season loaded — means there is nothing to lift, and the scrim simply covers everything, which is
/// the behaviour before this existed.
///
/// The whole CELL, not the still alone: the design's unit here is the tile plus its metadata block
/// (`EpRow` makes them two focus rows of one thing), and lifting half of it would say the popover is
/// about a picture rather than about an episode.
///
/// It carries the SEASON-SWITCH DIM too ([`EP_STALE_A`], the `p.alpha(0.35)` the strip is drawn
/// through while a season fetch is in flight). A hold on the previous season's tiles is a reachable
/// press — the row keeps drawing and keeps focus through the switch — and lifting one of them at
/// full strength out of a strip that is dimmed precisely because it is STALE would make the one
/// stale tile the brightest thing on the screen.
pub(crate) fn redraw_focused_episode() {
    crate::ui::guard(|| {
        let Some(d) = metadata::current() else { return };
        if view().section != 2 {
            return;
        }
        let i = view().col.max(0) as usize;
        let Some(ep) = d.episodes.get(i) else { return };
        let Some(top) = section_screen_top(2) else { return };
        let sx = view().ep_hscroll.pos;
        let stale = if metadata::season_loading() { EP_STALE_A } else { 1.0 };
        let pe = Painter::root()
            .alpha(crate::ui::nav::page_alpha() * stale)
            .translate(-sx, top);
        draw_episode_cell(pe, d, i, ep, i as c_int, view().ep_row, crate::ui::press::scale(), 0.0);
    });
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

/// One episode's watch state in the app's shared vocabulary ([`PosterMark`]) — what the filmstrip's
/// context menu builds its rows from (`item_menu::state_rows`).
///
/// A **projection of [`ep_state`]**, not a second predicate, which is the whole point: the still's
/// state line and the menu opened on that still answer the same question about the same episode, and
/// the resume-point edge this page already keeps (a `viewOffset` at or past the end is a finished
/// item, not one in progress) is written down once. `EpGlyph::None` IS the in-progress case — the bar
/// says it, so the glyph slot stays empty — which is why the progress field is what this reads.
fn ep_watch_state(ep: &metadata::Episode) -> PosterMark {
    let st = ep_state(ep);
    if st.progress.is_some() {
        PosterMark::InProgress
    } else if st.glyph == EpGlyph::Watched {
        PosterMark::Watched
    } else {
        PosterMark::None
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
    // Both glyphs are plain masks in ONE box now: the watched tick used to be a filled amber disc,
    // and the 2026-08-13 evening sync took the disc off the poster too — amber belongs to the resume
    // bar alone. It also takes the LINE's own ink rather than a hue, because a watched line is a
    // statement about the past (`✓ 48 min`) while an unstarted one is an invitation (`▶ 48 min`),
    // and only the invitation earns the amber.
    let icon = match st.glyph {
        EpGlyph::Play => Some((crate::ui::icons::Icon::Play, theme::RESUME_FILL)),
        EpGlyph::Watched => Some((crate::ui::icons::Icon::Check, theme::TEXT_SECONDARY)),
        EpGlyph::None => None,
    };
    let cy = ty + (ct + cb) * 0.5;
    if let Some((icon, tint)) = icon {
        crate::ui::icons::draw(p, icon, Rect::new(lx, cy - EP_GLYPH_D * 0.5, EP_GLYPH_D, EP_GLYPH_D), tint);
        lx += EP_GLYPH_D + EP_LINE_GAP;
    }
    if st.label.is_empty() {
        return;
    }
    // a watched episode's runtime steps back a rung: the tick beside it is the statement, and at full
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
        // `Art::Poster`, not `Art::Thumb` — and that ONE word is what puts the watched tick on this
        // shelf. The disc is drawn inside `widgets::card`'s Poster arm, off the row's own
        // `poster_mark`, so a `Thumb` (which carries a path and nothing else) can never wear it.
        // Related was the only poster shelf in the app passing `Thumb`, because until the row
        // became a real catalog row there was no `PmsMovie` behind it to ask.
        |i| Art::Poster(d.related.get(i)),
        // …and the resume BAR, the other half of the same three-state language. It was a hard-coded
        // `None` for exactly as long as the data was absent. `resume_frac` (not a bare `viewOffset`)
        // because it owns the "at or past the end is FINISHED, not in progress" rule — without which
        // a completed item whose resume point the server never cleared draws a 100%-full bar AND
        // loses the tick it should be wearing.
        |i| d.related.get(i).and_then(|m| m.resume_frac()),
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
        |i| Art::Person { sid: d.sid, key: d.credit(i).map_or("", |c| c.thumb.as_str()), res: (300, 300) },
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
    // Nothing behind an open panel is pressable, so nothing behind it is a card either: the press
    // belongs to the panel, and `on_ok` routes it there. Without this an OK held over the panel
    // would begin a tvOS press on a tile nobody can see and commit it on release. True of ALL
    // THREE of this page's panels — neither alert has a list to route the press to, and About
    // dismisses on it.
    if crate::ui::alt_sources::is_open()
        || crate::ui::about_panel::is_open()
        || crate::ui::tracks_panel::is_open()
    {
        return false;
    }
    // The episode filmstrip's METADATA block is not a card: it is a link into another page, so it
    // commits at once like the Play pill, a season tab or an About row. Two consequences, both wanted.
    // It gets no press dip — there is nothing to dip, the panel behind the text is the whole mark, and
    // the page changing under the press is the feedback. And because the press-and-HOLD context menu is
    // armed only where a press begins (`app.rs`), a hold on the text can never open the still's menu.
    if view().section == 2 {
        return view().ep_row == EpRow::Still;
    }
    // The SEASON STRIP is a press surface, and it is the odd one in this list: everything else here
    // is a tile. It is here for one reason — the hold is armed only where a press BEGINS (`app.rs`),
    // so a tab that activated on the key-DOWN could never grow the context menu that marks a whole
    // season watched (`open_season_menu`). Making it a press moves the tab's own activation to the
    // RELEASE, which is what a tap should do anyway, and lets `press::tick`'s long latch cancel that
    // activation so a HOLD opens the menu instead of also switching the season under it.
    //
    // It gets no press DIP, unlike every tile above: the strip's focus mark is a travelling capsule
    // the pills share as their ground (`TabStrip`), and scaling one pill off a capsule that stays
    // put would draw a ring around it rather than a press. The capsule arriving IS the tab's
    // feedback; the popover's own appear spring is the hold's.
    matches!(view().section, 1 | 3 | 4)
}

pub(crate) fn on_ok() -> bool {
    // The "Also available" panel owns the press while it is up — it is a modal list ON this page,
    // so an OK belongs to its highlighted row and never to the control set behind it. Reported
    // rather than performed there (`alt_sources`'s rule); [`ALT_OPEN`] is where it becomes a
    // navigation request, for the same reason the Related shelf's OK does not call `open_rk` itself.
    // Track information is READ-ONLY: it has no row to activate, so OK is swallowed rather than
    // reaching the hero control behind it. BACK is the only key that dismisses it, which is what
    // the panel's own footer says.
    if crate::ui::tracks_panel::is_open() {
        return false;
    }
    if crate::ui::alt_sources::is_open() {
        request_alt_open(crate::ui::alt_sources::on_ok());
        return false;
    }
    // The About alert owns the press too, and spends it dismissing — see `about_panel::on_ok` for
    // why a read-only panel answers OK at all when the design names only BACK.
    if crate::ui::about_panel::is_open() {
        crate::ui::about_panel::on_ok();
        return false;
    }
    view().last_resume_ns = 0; // default: no resume (set below for plays)
    let sec = view().section;
    let col = view().col;
    match sec {
        0 => {
            // the hero row's indices move with its control set, so resolve the press through the
            // one pure mapping (`hero_col` also corrects a focus the set shrank under)
            let action = hero_action(hero_col());
            if let Some(w) = action.watch_write() {
                // A watch-state disc. **The verb is the CONTROL's** (`HeroAction::watch_write`) —
                // this arm used to read `d.watched` back and derive the opposite, which a
                // part-watched item has no single answer for.
                //
                // **The ITEM's server, not the current one** — `rk` is a key on
                // `d.sid` and on no other machine, and browsing a shared library no longer re-points
                // `current`, so resolved from `client()` this posted a BORROWED item's ratingKey to
                // OUR server. Both servers number from 1, so that is not a no-op: the friend's film
                // stays unwatched and an unrelated film of ours is marked instead.
                //
                // The scrobble, the page re-read and the hub refetch all used to run right here, on
                // the SDL thread — three blocking round-trip chains off one keypress. `viewstate`
                // owns all three now: it flips this page and the shelves optimistically, writes on a
                // worker, and calls `refresh_view_state` when the write lands. `Some("")` is
                // "refresh the detail page, with no filmstrip position to protect" — the press was
                // on the hero. NB the optimistic half reaches a LEAF's discs on this frame and a
                // CONTAINER's only when the re-read lands — `hero_watch_state`'s doc says why.
                //
                // The `guid` rides along because this page HAS it, and it is the guid OF `d.rk` —
                // the same pair the page is mounted on. It is what makes the write follow the
                // TITLE: a film both sources hold ends up watched on both (`crate::viewstate`).
                // NB the verb stays the CONTROL's `w` from `watch_write` above and is NOT
                // re-derived from `d.watched` here: `d` can have moved since the frame the user
                // aimed at, and the FACE they aimed at is the promise that must be kept.
                if let Some((sid, rk, guid)) =
                    metadata::current().map(|d| (d.sid, d.rk.clone(), d.guid.clone()))
                {
                    crate::viewstate::request(sid, &rk, w, Some(String::new()), &guid);
                }
                return false;
            }
            if action == HeroAction::Alt {
                // ask for the list of the OTHER sources holding this film. Nothing is chosen yet —
                // and nothing is PRESENTED here either; see [`ALT_REQ`].
                unsafe { *addr_of_mut!(ALT_REQ) = true };
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
                    // the loaded Detail carries everything the string-based route entry needs,
                    // INCLUDING the server it was loaded from: `d.rk` is a key on `d.sid` and on no
                    // other machine, so resolving it against the browsed surface is how a borrowed
                    // film came to be played from our own server (or not at all).
                    crate::route::request_play(
                        crate::route::item_sid(d.sid),
                        &d.rk,
                        &d.part,
                        &d.vcodec,
                        &d.acodec,
                        &d.title,
                        "",
                    );
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
        2 => match view().ep_row {
            EpRow::Still => play_episode_at(col),
            // The metadata block is a LINK to that episode's own page — the same navigation the
            // context menu's "Go to Episode" row performs, so there is one way into an item page and
            // not two. It is also literally what the Related tile beside it does, which is why both
            // raise the SAME request.
            EpRow::Text => {
                if let Some(rk) = ep_page_rk() {
                    request_open(rk);
                }
                false
            }
        },
        3 => {
            // Related: open that item's detail page ON TOP of this one
            let rk = metadata::current().and_then(|d| d.related.get(col.max(0) as usize)).map(|r| r.rk.clone());
            if let Some(rk) = rk.filter(|r| !r.is_empty()) {
                request_open(rk);
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
            // …and the LOCAL id is only meaningful against the server that issued it, so the
            // page's own `sid` rides with it (`person::Person::sid`). The guid does not need it.
            if let Some((sid, c)) =
                metadata::current().and_then(|d| Some((d.sid, d.credit(col.max(0) as usize)?)))
            {
                let key = c.person_key();
                if !key.is_empty() {
                    crate::ui::person::open(sid, &key, &c.tag_key, &c.tag, &c.thumb);
                }
            }
            false
        }
        // About (5). **TWO of the four columns open a panel, and each opens the panel that is
        // ABOUT IT** — `Alert Views.dc.html`'s opening paragraph, taken at its word in both
        // directions. The CARD (col 0) opens §1A, because it is the column that truncates something
        // worth reading in full. **LANGUAGES (col 2) opens §1B**, Track information, because the
        // design's own sentence for refusing that column a panel of its own is that the audio list
        // *"belongs beside the bitrates in Track information"* — which is a statement about where
        // the tracks are READ, and so about where the press belongs, not only about what not to
        // build. (It was a ⓘ disc in the hero until 2026-08-21; `hero_ctls` records why it is not.)
        //
        // Information (1) and Accessibility (3) still open nothing and fall through to `false`:
        // four facts the page prints in full, and three fixed definitions that never change. That
        // is the same "with nothing to open, nothing is drawn and nothing happens" shape the
        // actions row's absent controls take.
        //
        // Both arms are GATED on their subject existing — `metadata::current()` for the card,
        // `tracks_panel::is_available()` for the tracks, which is false for a show container that
        // has no file of its own. A modal over an item that is not loaded traps every key the page
        // sends it while showing nothing.
        5 => {
            if col == 0 && metadata::current().is_some() {
                crate::ui::about_panel::open();
            } else if col == 2 && crate::ui::tracks_panel::is_available() {
                // Opened straight from here, unlike `Alt`'s deferred request: that panel is
                // ANCHORED beside its control's drawn rect, so it needs a frame `app.rs` can hand
                // it one from. This one is a fixed, centred frame and needs nothing from the draw
                // — so a request latch would be a hop with no payload.
                crate::ui::tracks_panel::open();
            }
            false
        }
        _ => false,
    }
}

/// The ratingKey of a page this one wants opened ON TOP of it — raised by the filmstrip's text row
/// and by the Related shelf, drained ONCE per frame by `app.rs`, which owns routing and the BACK
/// trail.
///
/// These two arms used to call [`open_rk`] themselves, on the reasoning that "the route is already
/// `Route::Detail`, so nothing above has to be told". That is exactly why BACK from an episode page
/// landed on Home: the page had been replaced and the trail still described the one that was gone.
/// The same shape as `person::take_request` for the same reason — a latch drained in one place
/// cannot be forgotten by one of the four paths that reach `on_ok`.
///
/// **An rk and nothing else.** Where this page was standing is captured by `app.rs`'s own
/// `leaving_spot`, from this module's [`spot`], on the frame the navigation is raised — and it has
/// to be, because five other navigations off a detail page (BACK, a cast headshot, a context-menu
/// row, the top tab bar…) never come through here at all. The latch used to carry a second copy,
/// taken from the same function on the same frame, that the drain destructured away unread.
///
/// The rk is carried WITH its server. Both arms that raise one (a Related tile, the filmstrip's
/// metadata row) name an item on the page's OWN server — a related hub and a season are answered by
/// the machine that was asked — and a bare key handed to `app.rs` would be re-resolved against
/// whichever server is current, which on a share is a different film with the same number.
static mut OPEN_REQ: Option<(crate::plex::ServerId, String)> = None;

/// Raise [`OPEN_REQ`] for `rk` on the loaded item's server.
fn request_open(rk: String) {
    let sid = mounted_sid();
    unsafe { *addr_of_mut!(OPEN_REQ) = Some((sid, rk)) }
}

/// Drain the request — `None` unless one is pending. One-shot, like `person::take_request`.
pub(crate) fn take_open_request() -> Option<(crate::plex::ServerId, String)> {
    unsafe { (*addr_of_mut!(OPEN_REQ)).take() }
}

// ---- "Also available": the same film on a second pinned source (ui/alt_sources.rs) -------------
// Three seams, the same three the episode filmstrip's context menu has: this page supplies the
// control's rect to anchor the panel to, takes the panel's press, and turns the chosen copy into
// the one navigation it knows how to raise.

/// An OK on the *Also available* control, waiting to be presented. Raised by [`on_ok`] and drained
/// once a frame by `app.rs`, which opens the panel with the anchor [`alt_btn_rect`] measures — the
/// same division `item_menu` keeps, where the SCREEN says what was pressed and the caller presents
/// the popover beside the rect it hands in.
///
/// It carries a BOOL and not the rect for a mechanical reason worth stating: measuring this row's
/// pills reaches SDL_ttf, the host suite links none (the crate's Tier-1 limit), and several host
/// tests drive `on_ok` — so a latch that measured its own anchor would drag every one of them into
/// the TV's text stack and fail to LINK on the dev Mac.
static mut ALT_REQ: bool = false;

/// Drain the request. One-shot, like [`take_open_request`].
pub(crate) fn take_alt_request() -> bool {
    unsafe { std::mem::replace(&mut *addr_of_mut!(ALT_REQ), false) }
}

/// The *Also available* control's rect in SCREEN coordinates, or `None` when the row is not showing
/// one. The row scrolls with the hero, so this is the drawn frame — the panel must anchor to where
/// the button IS, not to where it would be at the top of the page.
pub(crate) fn alt_btn_rect() -> Option<Rect> {
    let i = hero_index(HeroCtl::Alt)?;
    let y = hero_layout(selected()).btn_y - view().column.scroll.pos;
    Some(hero_btn_rect(i, y))
}

/// The copy the panel chose, waiting to be OPENED — its server and that server's ratingKey.
///
/// **It is a navigation, never a swap of the copy in place** — the design's call, and the one that
/// settles the per-server resume position: the destination page arrives with its own badges, its
/// own actions and its own progress, so nothing has to explain a position that jumped.
///
/// It is a REQUEST rather than an act, and the reason is worth stating because the first version
/// did act. Opening another server's page means re-pointing `plex::client()`, and that one call
/// invalidates far more than this page: the hub catalog, the browse grid, an open person page and
/// the poster cache are all keyed to whichever server was current when they were filled, and the
/// BACK trail's nodes are ratingKeys with no server at all. Every one of those is `app.rs`'s to
/// reset — it is where the identical wipe already lives for a profile switch (`activate_server`) —
/// so a screen doing the switch itself would leave server A's ratingKeys being fetched from server
/// B, with only the page in front of you correct.
static mut ALT_OPEN: Option<(crate::plex::ServerId, String)> = None;

/// Take the panel's chosen [`Action`](crate::ui::alt_sources::Action) as a request. A copy with no
/// destination (the row you are already on) raises nothing.
fn request_alt_open(act: crate::ui::alt_sources::Action) {
    if let crate::ui::alt_sources::Action::Open { sid, rk } = act {
        unsafe { *addr_of_mut!(ALT_OPEN) = Some((sid, rk)) };
    }
}

/// Drain it — `None` unless a copy was chosen. One-shot, like [`take_open_request`].
pub(crate) fn take_alt_open() -> Option<(crate::plex::ServerId, String)> {
    unsafe { (*addr_of_mut!(ALT_OPEN)).take() }
}

// ---- the episode filmstrip's press-and-hold context menu (ui/item_menu.rs) -------------------
// Three seams, because the menu is screen-agnostic by design: it only ever REPORTS an Action, and
// `app.rs` performs it. detail.rs supplies the item, the rect to anchor beside, and the two
// operations an Action can turn into here.

/// The focused episode's identity for the context menu — its ratingKey and its watch STATE, which is
/// three-valued and not two ([`ep_watch_state`]): a part-watched episode is at neither end of the
/// range, so the menu offers it both write rows rather than guessing an end. `None` unless the
/// filmstrip actually holds focus.
///
/// Also `None` while a season fetch is in flight: the row is still drawing the PREVIOUS season's
/// episodes (dimmed, under a spinner) while `cur_season` already names the new one, so a menu built
/// from it would offer to mark the wrong episode watched — the same trap `play_episode_at` refuses
/// the press for.
pub(crate) fn focused_episode() -> Option<(String, PosterMark)> {
    if metadata::season_loading() {
        return None;
    }
    let v = view();
    if v.section != 2 {
        return None;
    }
    // The menu belongs to the STILL. A hold on the metadata block cannot reach here anyway — that row
    // is not `focus_is_card`, so no press is ever begun on it — but the menu's own precondition is
    // stated here rather than left to a fact about another module.
    if v.ep_row != EpRow::Still {
        return None;
    }
    let ep = metadata::current()?.episodes.get(v.col.max(0) as usize)?;
    Some((ep.rk.clone(), ep_watch_state(ep)))
}

/// The episode whose OWN page the filmstrip's [`EpRow::Text`] row opens — `None` unless that row
/// actually holds focus, and `None` while a season fetch is in flight for the same reason
/// [`focused_episode`] refuses then: the row is still drawing the PREVIOUS season's episodes under a
/// spinner while `cur_season` already names the new one, so an index into it names the wrong episode.
///
/// Split out of [`on_ok`] so the navigation TARGET is host-testable — the act itself ([`open_rk`]) is
/// a PMS round trip.
fn ep_page_rk() -> Option<String> {
    if metadata::season_loading() {
        return None;
    }
    let v = view();
    if v.section != 2 || v.ep_row != EpRow::Text {
        return None;
    }
    let ep = metadata::current()?.episodes.get(v.col.max(0) as usize)?;
    (!ep.rk.is_empty()).then(|| ep.rk.clone())
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


// ---- the season strip's context-menu seam ----------------------------------------------------
// The filmstrip's three functions above, for the tab row. A season is the one level of this show
// with no marking surface of its own: an episode has its still's long press, the show has the
// hero's toggle (and its poster's menu on every card screen), and the middle — "I have seen all of
// season 3" — had nowhere to be said. It is also the level a user most often means: a show is 24
// seasons of commitment to mark in one press, an episode is one of 400.

/// The focused season tab's `(ratingKey, watch state)` — what the long-press menu is built from.
///
/// `None` unless the tab strip actually holds focus, and `None` while a season fetch is in flight,
/// for [`focused_episode`]'s reason turned one row up: the strip keeps drawing and keeps focus
/// through a switch, so an index into it during the fetch can name a season the user has already
/// left.
///
/// The state is the SEASON's own counts, not the show's and not the loaded episodes': a season the
/// user has never opened still knows how many of its leaves are viewed, because `includeMeta`
/// brings `leafCount`/`viewedLeafCount` down with the season list itself.
pub(crate) fn focused_season() -> Option<(String, PosterMark)> {
    if metadata::season_loading() {
        return None;
    }
    let v = view();
    if v.section != 1 {
        return None;
    }
    let s = metadata::current()?.seasons.get(v.col.max(0) as usize)?;
    (!s.rk.is_empty()).then(|| (s.rk.clone(), season_watch_state(s)))
}

/// A season's watch state, in the app's ONE vocabulary ([`PosterMark`]).
///
/// PURE, and the season-scope twin of `hero_watch_state`/`ep_watch_state`. A container has no
/// resume point of its own, so the three states are read off the leaf counts alone —
/// [`metadata::Season::watched`] is the "all of them" end, and its `leaf_count > 0` guard is what
/// stops a season the server sent no counts for reading as finished (`0 >= 0`).
fn season_watch_state(s: &metadata::Season) -> PosterMark {
    if s.watched() {
        PosterMark::Watched
    } else if s.viewed_leaf_count > 0 {
        PosterMark::InProgress
    } else {
        PosterMark::None
    }
}

/// The focused season pill's DRAWN rect, in screen space — what the popover anchors BESIDE.
///
/// Derived from the formulas `draw_tabs` walks (`tabs_layout` for the pill, the live h-scroll,
/// `section_screen_top` for the band), never recorded, so the panel hangs off the pill the user is
/// looking at. Unlike the filmstrip's, there is no pop spring to fold in: a tab marks focus with a
/// travelling capsule, not a scale.
pub(crate) fn focused_season_rect() -> Option<Rect> {
    let v = view();
    if v.section != 1 {
        return None;
    }
    let d = metadata::current()?;
    let i = v.col.max(0) as usize;
    let lay = tabs_layout(d).get(i)?;
    let top = section_screen_top(1)?;
    let sx = view().tab_hscroll.pos;
    let r = tab_pill_rect(lay, top);
    Some(Rect::new(r.x - sx, r.y, r.w, r.h))
}

/// Re-draw the focused season pill ON TOP of the popover's dim — the cut-out
/// [`crate::ui::popover::Opener`] takes, so the one thing the panel is ABOUT stays lit.
///
/// The whole STRIP is re-drawn rather than the one pill, for the filmstrip's reason: the pills
/// share a travelling capsule (`TabStrip`), which is their ground, and lifting a pill out from
/// over it would leave the highlight behind on the dimmed row.
pub(crate) fn redraw_focused_season() {
    crate::ui::guard(|| {
        if view().section != 1 || metadata::current().is_none() {
            return;
        }
        let Some(top) = section_screen_top(1) else { return };
        draw_tabs(Painter::root().alpha(crate::ui::nav::page_alpha()).translate(0.0, top));
    });
}

// ---- the Related shelf's context-menu seam --------------------------------------------------
// The filmstrip's three functions above, for the shelf below it. They exist because a hold on a
// Related tile used to do nothing at all: the shelf ARMED the same press every other card surface
// does (`press::begin` + `ok_armed`), dipped the card, latched long — and then fell through to the
// spring-back, because `focused_episode` declines every section but 2 and nothing else answered.
//
// The reason given at the time was that a Related row carried no `(ratingKey, watched)` pair to
// build menu rows from, which was true of the STRUCT and never of the data (see
// [`metadata::Related`]). With the row a real `PmsMovie` the whole card-surface path applies
// unchanged — `item_menu::open`, not `open_episode`: this is a card row like Home's or the Library
// grid's, not a leaf of the season this page has loaded.

/// The focused Related row — `None` unless that shelf actually holds focus.
///
/// Handed to `app.rs`'s `open_tile_menu`, the same shared opener the Library grid, Search and the
/// person page use, so the menu's row set, its choreography and its dispatch are theirs rather than
/// a fourth copy.
///
/// **The row carries its own `sid`, and that is the whole server story.** A related item is a key
/// on the server this page is mounted on; `fetch_related` stamped it (see [`metadata::Related`]),
/// `item_menu::open` reads `m.sid` into its `SID`, and `apply_item_action` scrobbles against that.
/// No call site here reaches for `plex::current_server()` — the documented way this exact class of
/// bug happens is a borrowed ratingKey posted to OUR server, which marks an unrelated film watched
/// because both servers number from 1.
pub(crate) fn focused_related() -> Option<&'static PmsMovie> {
    metadata::current()?.related.get(focused_related_col()?)
}

/// The focused Related poster's DRAWN rect, in screen space — what the popover anchors BESIDE.
///
/// DERIVED, never recorded, which is this file's rule for every focusable region (see the
/// "one geometry source per focusable region" note above [`hero_pill_label`]): the same `strip_x`
/// the painter advances by, the same live `CardRow` scroll, the same `section_screen_top` band, and
/// the row's own per-cell scale spring. The person page RECORDS its equivalent, and says why — its
/// shelf's scroll spring lives inside a `CardRow` the caller cannot see. Here `view().related` IS
/// that `CardRow`, so there is nothing to record.
///
/// `press::scale()` is deliberately left out, exactly as [`focused_episode_rect`] leaves it out:
/// opening the menu cancels the press, so by the frame the anchor is captured the tile is already
/// springing back to its resting popped scale.
pub(crate) fn focused_related_rect() -> Option<Rect> {
    let i = focused_related_col()?;
    Some(related_tile_rect(i)?.scaled(view().related.scale(i)))
}

/// The focused Related column, bounds-checked against the shelf that is actually loaded — `None`
/// when the shelf does not hold focus, and `None` for a column past its end (a page can be
/// re-mounted under a retained focus column, which is the same hazard `focused_episode` guards).
fn focused_related_col() -> Option<usize> {
    if view().section != 3 {
        return None;
    }
    let i = view().col.max(0) as usize;
    (i < n_items(3).max(0) as usize).then_some(i)
}

/// Related tile `i`'s UNSCALED screen rect — the ONE geometry source the anchor and the scrim lift
/// both derive from, so the panel cannot hang off one rect while the lifted tile draws at another.
///
/// Unscaled deliberately: `card_row::draw_focused` takes the SCALED rect *and* the scale it was
/// scaled by, and recovers the label's anchor as `rect.h / s`. Handing it an already-scaled rect
/// with a different `s` (the press dip is in one and not the other) would draw the poster at the
/// wrong size and drop its title at a y derived from an unscaled height that never existed.
fn related_tile_rect(i: usize) -> Option<Rect> {
    Some(related_tile_rect_at(i, section_screen_top(3)?, view().related.scroll_x()))
}

/// …and its ARITHMETIC, with the two live values passed in — **pure**, so the agreement between
/// this, `card_row::strip`'s draw and `strip_at`'s hit-test is host-testable.
///
/// The split is not cosmetic. `section_screen_top` walks the `ScrollColumn` flow, which sums
/// `block_h` over the present blocks, and `block_h(2)` measures an episode's title through
/// `TextView` — i.e. through SDL_ttf. That is a native library the host suite has no link for, so a
/// test that called the wrapper would not fail an assertion, it would fail to LINK (`CLAUDE.md`'s
/// structural limit #1). Everything worth grading here is in this function; the wrapper is two
/// lookups.
fn related_tile_rect_at(i: usize, top: f32, scroll_x: f32) -> Rect {
    Rect::new(strip_x(i, REL_W + REL_GAP) - scroll_x, top + REL_LABEL_H, REL_W, REL_H)
}

/// Re-draw the focused Related poster ON TOP of the modal scrim — this shelf's half of
/// [`crate::ui::popover::Opener`], so the tile the panel hangs off does not recede with the page.
///
/// It runs `card_row::draw_focused` with the same art, the same resume fraction and the same
/// `TileLabel` [`draw_related`] builds, at the rect [`focused_related_rect`] derives — the tile the
/// shelf drew, not a second expression of it. The one difference from the strip's own pass is that
/// `press::scale()` IS folded in here: this draws every frame the menu is open, and the dip is on
/// its way out, so following it keeps the lifted copy identical to the cell underneath it.
pub(crate) fn redraw_focused_related() {
    crate::ui::guard(|| {
        let Some(d) = metadata::current() else { return };
        let Some(i) = focused_related_col() else { return };
        let Some(base) = related_tile_rect(i) else { return };
        let Some(m) = d.related.get(i) else { return };
        let s = view().related.scale(i) * crate::ui::press::scale();
        let label = card_row::TileLabel::title(&m.title);
        let p = Painter::root().alpha(crate::ui::nav::page_alpha());
        card_row::draw_focused(p, Art::Poster(Some(m)), base.scaled(s), s, &RowStyle::HOME, m.resume_frac(), &label);
    });
}

/// Re-read the mounted item from the server after something changed its view state, KEEPING the
/// season the user was browsing.
///
/// Shared by the hero's watched toggle and by the filmstrip's context menu, which mark different
/// things watched — the whole show versus one of its episodes — but both need this page to be
/// showing the truth afterwards (the tab ticks, the episode checks, the resume bars and the hero's
/// own discs all read the refetched item).
/// `keep_ep` is the episode this refresh is ABOUT, if any (the filmstrip menu passes the one it
/// just toggled; the hero passes nothing) — see [`KEEP_EP`] for why the row would otherwise scroll
/// itself away from the tile the user just acted on.
///
/// **ASYNC.** This used to be a blocking `load_detail_now` — 2 round trips for a movie, 5 for a
/// show, straight off a keypress — with the season restore written on the next line because the
/// refreshed item was already installed by then. That is what a WAN or sleeping share turns into
/// seconds of frozen UI, so the fetch now goes through the ordinary detail mailbox and the restore
/// is a LATCH ([`REFRESH`]) that [`pump_refresh`] applies when the landing arrives. The page is not
/// cleared and no spinner appears: `CURRENT` keeps the item (optimistically edited by
/// `metadata::set_watched_local`) for the whole window, so what the user sees is their press,
/// corrected a beat later if the server disagrees.
pub(crate) fn refresh_view_state(keep_ep: &str) {
    let Some((sid, rk, season)) = metadata::current().map(|d| (d.sid, d.rk.clone(), d.cur_season)) else {
        return;
    };
    // Armed BEFORE the request, so a landing can never beat the latch into place — the same
    // ordering the blocking version needed for `KEEP_EP`, one level out.
    unsafe {
        *addr_of_mut!(REFRESH) = Some(Refresh { sid, rk: rk.clone(), season, keep_ep: keep_ep.to_string() })
    };
    metadata::request_detail(sid, &rk);
}

/// The season restore a [`refresh_view_state`] owes once its re-read lands.
///
/// It exists because a fresh detail fetch always comes back on season 0 (`fetch_full` fills
/// `episodes` from `seasons[0]`), while the user is browsing whichever season they opened. The
/// blocking version could put that back on the next statement; the async one has to wait for the
/// landing, and this is what remembers what to put back.
#[derive(Clone)]
struct Refresh {
    /// The item the refresh was asked for — matched as the (server, key) PAIR when it lands, so a
    /// page walked to another machine's item with the same rk is not steered by this latch.
    sid: crate::plex::ServerId,
    rk: String,
    /// The season INDEX the page was on (an index into `d.seasons`, which is what `cur_season` is).
    season: usize,
    /// The episode [`KEEP_EP`] must land the filmstrip back on; empty for the hero's own toggle.
    keep_ep: String,
}
static mut REFRESH: Option<Refresh> = None;

/// Apply a landed [`refresh_view_state`]. MAIN THREAD, once a frame from [`update`].
///
/// One-shot on the first frame the re-read is no longer in flight, whatever it produced —
/// `metadata::pump_detail` catches `DETAIL_DONE` up on a FAILURE too, so a server that refused
/// settles this rather than leaving the latch waiting for a landing that will never come. Every way
/// it can fail to apply ends in the same place: the page keeps the item and the season it already
/// had, which is what a failed refresh should look like.
fn pump_refresh() {
    if metadata::detail_loading() {
        return; // the re-read is still out
    }
    let Some(r) = (unsafe { (*addr_of_mut!(REFRESH)).take() }) else { return };
    let Some(d) = metadata::current() else { return }; // page closed under it
    if !crate::plex::same_item((d.sid, &d.rk), (r.sid, &r.rk)) {
        return; // the user walked to another item — not ours to steer
    }
    if r.season == 0 || d.cur_season == r.season || r.season >= d.seasons.len() {
        // Nothing to restore: season 0 is where a landing already leaves the page, an unchanged
        // `cur_season` means the fetch failed and the browsed season is still listed, and a season
        // that is no longer in the show is one the server has removed.
        return;
    }
    if !r.keep_ep.is_empty() {
        unsafe { *addr_of_mut!(KEEP_EP) = Some((r.keep_ep, r.season)) };
    }
    metadata::load_season(r.season); // back to the season the user was browsing
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

/// The item this page is currently pointed at — the LOADED item's rk while one is loaded, else the
/// rk the page was mounted on (the whole 2-5 round-trip window `open_rk` deliberately keeps
/// `current()` empty for).
///
/// Two consumers, both about the BACK trail (`ui::trail`): `Trail::ensure_detail`, which makes the
/// trail agree with a page reached WITHOUT navigating (a player exit, the Info card's jump), and
/// `app.rs`'s `enter_node`, whose "is the page behind me still the one loaded?" test is what keeps a
/// trail pop free in the common case.
pub(crate) fn mounted_rk() -> String {
    metadata::current().map(|d| d.rk.clone()).unwrap_or_else(|| view().mounted_rk.clone())
}

/// The SERVER that rk belongs to — read from exactly the same two places, in the same order, so the
/// pair can never come from two different items. Callers ask both together and compare the pair
/// (`Node::same_page`, `Trail::ensure_detail`): a bare rk names a page on neither server once a
/// share is registered.
pub(crate) fn mounted_sid() -> crate::plex::ServerId {
    metadata::current().map(|d| d.sid).unwrap_or_else(|| view().mounted_sid)
}

/// Re-resolve the selected catalog row after a hub refetch rebuilt the catalog underneath an open
/// detail page (indices move; the rk is the stable identity).
pub(crate) fn reselect() {
    // Fall back to the mounted rk when the fetch is still in flight: `open_rk` clears CURRENT for
    // that whole window on purpose, and a refetch landing inside it would otherwise leave
    // `selected` pointing into the OLD catalog's ordering. Re-pointing an index only ever needed
    // the item's identity, never its loaded detail.
    let (sid, rk) = (mounted_sid(), mounted_rk());
    if !rk.is_empty() {
        view().selected = crate::pms::index_of_rk(sid, &rk);
    }
}

/// Point the page at `(sid, rk)` — catalog row (for the backdrop art/blur) + fresh focus/scroll.
/// The `open_rk*` twins share it so they cannot drift; only the fetch differs.
fn mount_rk(sid: crate::plex::ServerId, rk: &str) {
    let idx = crate::pms::index_of_rk(sid, rk);
    let v = view();
    v.selected = idx;
    v.mounted_rk = rk.to_string();
    v.mounted_sid = sid;
    reset_view_state(v);
    // point the copy list at the new item — a store still describing the previous film would draw
    // its control on this hero and offer to navigate to its copies. BOTH halves of the identity:
    // the other server's film with this rk is a different film, and its resolve may be in flight.
    crate::ui::alt_sources::reset(sid, rk);
    // The About alert dies with the item it was opened for, exactly as the copy list does. It draws
    // `metadata::current()` and the page is about to clear that for the whole fetch window, so a
    // surviving panel would be a modal over an item that is no longer loaded — trapping every key
    // the page sends it while showing nothing.
    crate::ui::about_panel::close();
}

/// Re-open the detail page for an arbitrary ratingKey (e.g. a Related item). Uses the
/// catalog row for the backdrop art/blur when the item is in the browse catalog, else
/// falls back to the loaded detail's own art (no blur).
///
/// ASYNC: the page mounts this frame on the catalog row and the sections fill in when
/// `metadata::pump_detail` lands the fetch. Callers that must read `metadata::current()` before
/// the next statement want [`open_rk_now`] instead.
pub(crate) fn open_rk(sid: crate::plex::ServerId, rk: &str) {
    mount_rk(sid, rk);
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
    metadata::request_detail(sid, rk);
}

/// [`open_rk`] but BLOCKING — for the callers that act on the loaded item in the SAME frame:
/// [`open_rk_season`] (its `load_season_now` indexes `d.seasons`), home_activate's play-a-show
/// arm (gated on `current().rk == expect`), and the headless `plxnative-detail` trigger (which
/// replays move_focus/on_ok immediately). Each of those is a deliberate freeze.
pub(crate) fn open_rk_now(sid: crate::plex::ServerId, rk: &str) {
    mount_rk(sid, rk);
    metadata::load_detail_now(sid, rk);
}

/// A focus placement waiting for its data. Both variants are the same two-stage wait — the item,
/// then the right season — and differ only in what they place when it lands and in how they got
/// here.
///
/// The wait exists because the seasons and the episode list arrive from separate fetches, neither
/// of which has landed at the moment the placement is asked for. The alternative — the blocking
/// `open_rk_season` the home path uses — would put 2-5 PMS round trips on a keypress, and both of
/// these are asked for on a frame that has other work (a player teardown, a BACK pop).
#[derive(Clone)]
enum Pending {
    /// A show page opened FROM playback, landing on the episode that played
    /// ([`open_show_at_episode`]). Show rk, season NUMBER, episode rk.
    Episode { show_rk: String, season: i64, ep_rk: String },
    /// A page BACK returned to, put back exactly as it was left ([`open_rk_at`]).
    Spot { rk: String, spot: Spot },
}
static mut PENDING: Option<Pending> = None;

/// Open the SHOW detail page for `show_rk` with the episode `ep_rk` (in the season numbered
/// `season`) revealed once the data lands — a season entry point routes to the show page, not to a
/// standalone season page.
pub(crate) fn open_show_at_episode(sid: crate::plex::ServerId, show_rk: &str, season: i64, ep_rk: &str) {
    open_rk(sid, show_rk);
    unsafe {
        *addr_of_mut!(PENDING) =
            Some(Pending::Episode { show_rk: show_rk.to_string(), season, ep_rk: ep_rk.to_string() })
    }
}

/// [`open_rk`] plus a restore: mount the page and put the focus, season and scroll back where `spot`
/// says once the fetch lands. The BACK twin of [`open_show_at_episode`], on the same pump — this is
/// what makes a trail pop feel like a return rather than a fresh arrival.
pub(crate) fn open_rk_at(sid: crate::plex::ServerId, rk: &str, spot: &Spot) {
    open_rk(sid, rk);
    unsafe { *addr_of_mut!(PENDING) = Some(Pending::Spot { rk: rk.to_string(), spot: spot.clone() }) }
}

/// Does a [`Spot`] restore still have to wait on a season? `Some(pos)` = that season is not the one
/// loaded, so ask for it and wait; `None` = place now.
///
/// The `Episode` variant's own gate cannot be shared, and this is the difference: it returns early
/// while `d.seasons` is empty (the show fetch is still out), which for a MOVIE would spin forever.
/// A spot with no season is a spot on an item that has none, and it places immediately.
fn spot_season_gate(d: &metadata::Detail, spot: &Spot) -> Option<usize> {
    let want = spot.season?;
    let pos = d.seasons.iter().position(|s| s.index == want)?;
    (pos != d.cur_season).then_some(pos)
}

/// Where a [`Spot`] actually lands on the item as it is NOW — `(section, col, ep_row)`.
///
/// Clamped, because the item may have come back shorter than it was left: a season with fewer
/// episodes, a Related shelf the server no longer sends. A section that is not in the flow at all
/// falls back to the hero rather than focusing a block nothing draws — and drops the column with it,
/// because a column only means anything next to the section that produced it.
fn spot_landing(spot: &Spot) -> (c_int, c_int, EpRow) {
    let (secs, n) = sections();
    let (sec, col) = if secs[..n].contains(&spot.section) {
        (spot.section, spot.col.clamp(0, (n_items(spot.section) - 1).max(0)))
    } else {
        (0, 0)
    };
    let row = if spot.ep_text { EpRow::Text } else { EpRow::Still };
    (sec, col, row)
}

/// Apply a pending placement once its data lands. Two stages, because the season tab and the episode
/// row come from different fetches; runs AFTER `pump_season` so it wins the focus reset that a
/// landing season performs.
fn pump_pending() {
    let want = match unsafe { (*addr_of!(PENDING)).clone() } {
        Some(w) => w,
        None => return,
    };
    let Some(d) = metadata::current() else { return };
    match want {
        Pending::Episode { show_rk, season, ep_rk } => {
            // Everything is read out of `current()` BEFORE `load_season` is called: that write goes
            // through the same static this borrow came from.
            let (rk, season_pos, no_seasons, cur_season, ep_pos, no_eps) = (
                d.rk.clone(),
                d.seasons.iter().position(|s| s.index == season),
                d.seasons.is_empty(),
                d.cur_season,
                d.episodes.iter().position(|e| e.rk == ep_rk),
                d.episodes.is_empty(),
            );
            if rk != show_rk {
                unsafe { *addr_of_mut!(PENDING) = None }; // the page moved on — not ours to steer
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
                v.ep_row = EpRow::Still; // …on the STILL: the reveal points at the episode that played
                v.col = i as c_int;
                v.saved_col[2] = i as c_int;
                v.card_scale.jump(1.0);
                unsafe { *addr_of_mut!(PENDING) = None };
            } else if !no_eps {
                // the season landed and it isn't in there — stop trying rather than spin
                unsafe { *addr_of_mut!(PENDING) = None };
            }
        }
        Pending::Spot { rk, spot } => {
            // Same rule as above: resolve every read off `current()` before anything writes it.
            let (same, gate) = (d.rk == rk, spot_season_gate(d, &spot));
            if !same {
                unsafe { *addr_of_mut!(PENDING) = None };
                return;
            }
            if let Some(pos) = gate {
                if !metadata::season_loading() {
                    metadata::load_season(pos);
                }
                return; // that season's episodes aren't here yet
            }
            let (sec, col, row) = spot_landing(&spot);
            // The scroll JUMPS, and only on this variant: coming back to a page you left must read
            // as a restoration, whereas the `Episode` reveal is an ARRIVAL on a page you were not on
            // and its glide down to the filmstrip is the wanted behaviour. Resolved before `view()`
            // is borrowed, like every other target in this file.
            let sct = scroll_target_for(sec);
            unsafe { *addr_of_mut!(PENDING) = None };
            let v = view();
            v.saved_col = spot.saved_col;
            v.section = sec;
            v.col = col;
            v.ep_row = row;
            v.card_scale.jump(1.0);
            v.column.scroll.jump(sct);
        }
    }
}

pub(crate) fn open_rk_season(sid: crate::plex::ServerId, show_rk: &str, season_num: c_int) {
    open_rk_now(sid, show_rk); // BLOCKING: the season lookup below indexes the loaded item's seasons
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
    // An episode belongs to its SHOW's server — `d.sid` — because that is the page these rows were
    // fetched from. Same failure as the movie path if it is taken from the browsed surface instead.
    crate::route::request_play(
        crate::route::item_sid(d.sid),
        &ep.rk,
        &ep.part,
        &ep.vcodec,
        &ep.acodec,
        &hud_title,
        &hud_ctx,
    );
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

    // ---- focus highlight: the shared `widgets::text_block_highlight` under the SELECTED block,
    // wrapping its content — the card is a fixed box; the three columns size to their measured
    // height. Same call the episode filmstrip's metadata row makes; only the frame differs. ----
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
        crate::ui::widgets::text_block_highlight(p, hl);
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
    // The cap-top comes from `syn` itself: written out here it was `sy + syn_hh - 30.0`, a SECOND
    // copy of the leading three lines above, which is exactly how a mark drifts half a line off its
    // prose when someone retunes the block. Same call the person page's bio makes.
    if syn.truncates(syn_w) {
        let (mt, _) = crate::text::text_cap_band(theme::size::CAPTION, 1);
        p.text(c"MORE".as_ptr(), tx + cw - pad, syn.last_line_cap_y(sy, syn_hh) - mt,
            theme::size::CAPTION, theme::TEXT_TERTIARY, 2, 1);
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
    /// The docs-derived truth table for the facts row's "how this plays" state, in FULL: every one
    /// of the 2×2×3 input combinations, so the table is a specification rather than a spot check.
    ///
    /// Two Pass-gated states, and both are docs-derived: hardware conversion and HDR tone mapping
    /// are Plex Pass server features, so a PROVEN Pass-less server that will convert gets the soft
    /// note, and an HDR source on top of that gets the warning instead (a wrong picture outranks a
    /// slower one). Everything else is quiet — including every `Unknown` row, because an unproven
    /// missing subscription must not be asserted, and every `Yes` row, because there is nothing to
    /// say. Direct play is quiet whatever the source and the server are: the pixels arrive untouched.
    #[test]
    fn how_it_plays_resolves_the_full_docs_truth_table() {
        use super::PlayNote::{Quiet, Soft, Warn};
        use crate::plex::serverinfo::Subscription as Sub;
        use crate::route::Preview as Pv;
        for (pv, hdr, sub, want) in [
            // a RE-ENCODE on a server PROVEN to have no Pass — the only two non-quiet rows
            (Pv::Converts, true, Sub::No, Warn),
            (Pv::Converts, false, Sub::No, Soft),
            // …the same conversion where the subscription is known-good or simply unknown
            (Pv::Converts, true, Sub::Yes, Quiet),
            (Pv::Converts, false, Sub::Yes, Quiet),
            (Pv::Converts, true, Sub::Unknown, Quiet),
            (Pv::Converts, false, Sub::Unknown, Quiet),
            // A container-only REMUX is the row this table could not see while `Preview` had two
            // values, and it is the one that made the claims FALSE: the server copies the codecs
            // into MKV, so the picture arrives 4K/HDR10 intact and no encoder runs. Both Pass-gated
            // states are properties of an encode, so a proven-Pass-less server changes nothing here
            // — "HDR → SDR · tone-mapping needs [PLEX PASS]" on a 4K HDR HEVC file in a .mov was a
            // factually wrong claim, and the soft note named an encoder that never starts.
            (Pv::Remux, true, Sub::No, Quiet),
            (Pv::Remux, false, Sub::No, Quiet),
            (Pv::Remux, true, Sub::Yes, Quiet),
            (Pv::Remux, false, Sub::Yes, Quiet),
            (Pv::Remux, true, Sub::Unknown, Quiet),
            (Pv::Remux, false, Sub::Unknown, Quiet),
            // direct play: nothing is re-encoded, so no server feature is in play at all
            (Pv::DirectPlay, true, Sub::No, Quiet),
            (Pv::DirectPlay, false, Sub::No, Quiet),
            (Pv::DirectPlay, true, Sub::Yes, Quiet),
            (Pv::DirectPlay, false, Sub::Yes, Quiet),
            (Pv::DirectPlay, true, Sub::Unknown, Quiet),
            (Pv::DirectPlay, false, Sub::Unknown, Quiet),
        ] {
            assert_eq!(super::play_note(pv, hdr, sub), want, "{pv:?} hdr={hdr} {sub:?}");
        }
    }

    /// **Which server the truth table above is asked ABOUT** — the half it cannot see, because it
    /// takes the tristate as an argument.
    ///
    /// The note is a claim about the machine that would run the encoder, i.e. the ITEM's server.
    /// This read `serverinfo::subscription()`, the CURRENT server's — and browsing a shared library
    /// deliberately leaves `current` on the primary, so the two differ for every borrowed film.
    /// Both polarities are silent: our Pass'd server hid a free share's soft note and its HDR
    /// warning entirely (the two states the design added this row for), while a free primary put
    /// "hardware conversion needs [PLEX PASS]" on a friend's Pass'd server's conversion, which is
    /// the confident wrong answer pointing at a purchase that would fix nothing.
    ///
    /// The registry and the subscription slots are crate globals, so this holds `testlock::serial()`
    /// and empties the registry on the way OUT as well as in.
    #[test]
    fn the_pass_note_judges_the_items_own_server_not_the_browsed_one() {
        use crate::plex::serverinfo::{store_for_test, Subscription as Sub};
        use crate::plex::ServerId;
        struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Fresh {
            fn drop(&mut self) {
                crate::plex::reset_servers_for_test();
            }
        }
        let _g = Fresh(crate::testlock::serial());
        crate::plex::reset_servers_for_test();
        let reg = |m: &str, host: &str| crate::plex::register_for_test(m, host, 32400, "tok", "cid");
        let (ours, theirs) = (reg("mach-A", "10.0.0.1"), reg("mach-B", "10.0.0.2"));
        // the slot arrays outlive `reset_servers_for_test` — start from the boot state explicitly
        store_for_test(ours, Sub::Unknown, "");
        store_for_test(theirs, Sub::Unknown, "");
        store_for_test(ours, Sub::Yes, "1.43.3.10861-cd85035e7");
        store_for_test(theirs, Sub::No, "1.32.0.6918-free");
        // browsing the share does NOT re-point `current`, which is what made this look right
        assert!(crate::plex::set_current(ours));

        let on = |sid| Detail { sid, rk: "m1".into(), hdr: true, ..Default::default() };
        assert_eq!(super::item_subscription(&on(theirs)), Sub::No, "a borrowed film asks the share");
        assert_eq!(super::item_subscription(&on(ours)), Sub::Yes, "…and ours asks ours");
        // which is the whole point: the same conversion, the same source, two different notes
        assert_eq!(
            super::play_note(crate::route::Preview::Converts, true, super::item_subscription(&on(theirs))),
            super::PlayNote::Warn,
            "the free share's HDR re-encode is the warning the design asks for"
        );
        assert_eq!(
            super::play_note(crate::route::Preview::Converts, true, super::item_subscription(&on(ours))),
            super::PlayNote::Quiet,
            "and our Pass'd server's identical conversion says nothing"
        );
        // an item with no server on it is "we have not heard", never slot 0's answer
        assert_eq!(super::item_subscription(&on(ServerId::UNSET)), Sub::Unknown);
    }

    // ---- the facts row's source credit (Shared Sources, deliverable E) ------------------------
    //
    // The whole unit is width arithmetic over a row of text, which is exactly the shape the host
    // suite can grade: `facts_flow` hands its runs to a closure (the draw paints them, these
    // measure them), so the tokens, the order and the yield ladder are all readable here. No font
    // is open on the dev Mac — `text_width` answers 0 — which is why every width below is the
    // test's own and why the flow takes a measure rather than calling one.

    /// One thing the row emitted, in order — a text run with the tokens it was handed, or the
    /// "how this plays" fragment, which is not text and so gets its own event.
    #[derive(Clone, PartialEq, Debug)]
    enum Ev {
        Run(String, f32, c_int, [f32; 4]),
        Mode(f32, bool),
    }

    /// Per-character width of the synthetic font these tests flow through.
    const CW: f32 = 10.0;

    /// Flow the facts row with synthetic widths, recording what it emitted. Returns the total
    /// advance and the event list.
    fn flow(parts: &[&str], credit: &str, mode_w: f32) -> (f32, Vec<Ev>) {
        // one RefCell because both seams record into the same list, and two closures cannot hold
        // one `&mut` between them
        let evs = std::cell::RefCell::new(Vec::new());
        let w = super::facts_flow(
            parts,
            credit,
            |s, dx, sz, ink| {
                evs.borrow_mut().push(Ev::Run(s.to_string(), dx, sz, ink));
                CW * s.chars().count() as f32
            },
            |dx, after| {
                evs.borrow_mut().push(Ev::Mode(dx, after));
                mode_w
            },
        );
        (w, evs.into_inner())
    }

    const DATE: &str = "13 Jun 2024";
    const EXT: &str = "1 hr 57 min";
    const HANDLE: &str = "friend";

    /// **The acceptance criterion for this unit.** On your own server the line is exactly what it
    /// is today — not an empty run, not a dangling separator, not a draw call that paints nothing.
    /// The borrowed row is the same row with the credit ADDED to the end of it, which is what the
    /// event lists being prefix-equal says.
    #[test]
    fn an_item_on_your_own_server_gets_no_source_run_at_all() {
        assert_eq!(super::shared_by(""), "", "no owner to credit → no words, so no run");

        let (own_w, own) = flow(&[DATE, EXT], "", 90.0);
        assert_eq!(own.len(), 4, "date, separator, extent, fragment — and nothing else");
        assert!(
            !own.iter().any(|e| matches!(e, Ev::Run(s, ..) if s.is_empty())),
            "absence is no run, never an empty one"
        );

        let credit = super::shared_by(HANDLE);
        let (shared_w, shared) = flow(&[DATE, EXT], &credit, 90.0);
        assert_eq!(&shared[..4], &own[..], "the credit may only ADD — every run before it is untouched");
        assert_eq!(
            shared_w - own_w,
            2.0 * super::FACTS_SEP_PAD + CW + CW * credit.chars().count() as f32,
            "the growth is exactly the dot, the handle's words, and one pad either side of the dot"
        );
    }

    /// Where the credit goes and what it is drawn as: LAST on the line — after the facts about the
    /// item and after the "how this plays" fragment — introduced by the row's own `·`, and on the
    /// line's own rung and ink, because it is dim for the reason the line is dim.
    #[test]
    fn the_source_credit_is_the_last_run_on_the_line_after_the_play_mode_fragment() {
        let credit = super::shared_by(HANDLE);
        assert_eq!(credit, "Shared by friend", "the person, never the machine");

        let (_, evs) = flow(&[DATE, EXT], &credit, 90.0);
        let Some(Ev::Mode(mode_dx, after)) = evs.iter().find(|e| matches!(e, Ev::Mode(..))).cloned() else {
            panic!("the fragment must be flowed");
        };
        assert!(after, "the fragment is introduced by a separator when facts precede it");
        assert_eq!(evs.len(), 6, "date, ·, extent, fragment, ·, credit");

        // the separator that introduces the credit, and then the credit itself
        let (Ev::Run(dot, dot_dx, dot_sz, dot_ink), Ev::Run(words, words_dx, words_sz, words_ink)) =
            (evs[4].clone(), evs[5].clone())
        else {
            panic!("the credit is a separator and a run, in that order");
        };
        assert_eq!(dot, "\u{b7}");
        assert_eq!(dot_ink, theme::TEXT_SEPARATOR, "a dot in the words' own ink joins them instead of punctuating");
        assert_eq!(dot_dx, mode_dx + 90.0 + super::FACTS_SEP_PAD, "one pad past the fragment");
        assert_eq!(words, credit);
        assert_eq!(words_dx, dot_dx + CW + super::FACTS_SEP_PAD, "one pad past the dot");

        // …and it joins the line rather than sitting on a rung of its own
        let Ev::Run(_, _, date_sz, date_ink) = evs[0].clone() else { panic!("the date leads the row") };
        assert_eq!((words_sz, words_ink), (date_sz, date_ink), "same rung, same ink as the facts it follows");
        assert_eq!((dot_sz, words_sz), (theme::size::CAPTION, theme::size::CAPTION), "the line's own rung");
        assert_eq!(words_ink, theme::TEXT_TERTIARY, "the lowest rung of ink — the line is dim, so the run is");
    }

    /// A separator introduces the credit only when something is actually in front of it. An item
    /// PMS sent no air date and no runtime for would otherwise open its facts line with an orphan
    /// dot — and the fragment counts as "in front of it" even though it is not text.
    #[test]
    fn a_credit_with_nothing_in_front_of_it_opens_the_line_bare() {
        let credit = super::shared_by(HANDLE);

        let (_, alone) = flow(&["", ""], &credit, 0.0);
        assert_eq!(alone.len(), 2, "the fragment reported nothing, so the credit is the only text");
        assert!(matches!(&alone[1], Ev::Run(s, dx, ..) if s == &credit && *dx == 0.0), "no dot, and flush left");
        assert!(matches!(alone[0], Ev::Mode(0.0, false)), "nothing precedes the fragment either");

        let (_, after_mode) = flow(&["", ""], &credit, 90.0);
        assert_eq!(after_mode.len(), 3, "fragment, ·, credit");
        assert!(matches!(&after_mode[1], Ev::Run(s, ..) if s == "\u{b7}"), "a fragment that drew IS something in front");
    }

    /// The yield ladder, in the order the row gives things up: **the trailing credit first** (it is
    /// the one run on the line that is not a fact about the item), then the extent entirely, and
    /// only then a date elided into what is left. Monotone, so a row that had to drop its extent
    /// can never win the credit back.
    #[test]
    fn the_trailing_credit_is_the_first_member_the_row_gives_up() {
        use super::FactsFit;
        const DATE_W: f32 = 110.0;
        const EXT_W: f32 = 110.0;
        const CREDIT_W: f32 = 160.0;
        const MODE_W: f32 = 90.0;
        const SEP: f32 = 2.0 * super::FACTS_SEP_PAD + CW;
        // the same shape `facts_flow` measures: members joined by the row's separator, the
        // fragment always present between the clause and the credit
        let w = |f: FactsFit| {
            DATE_W + if f.extent { SEP + EXT_W } else { 0.0 } + MODE_W + if f.credit { SEP + CREDIT_W } else { 0.0 }
        };
        let full = w(FactsFit { extent: true, credit: true, elide: false });
        let no_credit = w(FactsFit { extent: true, credit: false, elide: false });
        let bare = w(FactsFit { extent: false, credit: false, elide: false });

        assert_eq!(
            super::facts_fit(true, full, w),
            FactsFit { extent: true, credit: true, elide: false },
            "a row that fits keeps everything"
        );
        assert_eq!(
            super::facts_fit(true, full - 1.0, w),
            FactsFit { extent: true, credit: false, elide: false },
            "one pixel short: the credit goes, and the item's own facts stay whole"
        );
        assert_eq!(
            super::facts_fit(true, no_credit - 1.0, w),
            FactsFit { extent: false, credit: false, elide: false },
            "then the extent goes entirely — a member goes whole before it goes garbled"
        );
        assert_eq!(
            super::facts_fit(true, bare - 1.0, w),
            FactsFit { extent: false, credit: false, elide: true },
            "and only a date that cannot fit alone is ellipsised"
        );

        // Two invariants across the whole budget range, not just at the four corners.
        for i in 0..=((full as i32) + 40) {
            let b = i as f32;
            let fit = super::facts_fit(true, b, w);
            assert!(!(fit.credit && !fit.extent), "the credit can never outlive the extent (budget {b})");
            // …and an item on our OWN server decides exactly as it does today: the credit term is
            // absent from the row entirely, so only the extent and the elide are ever in question.
            let own = super::facts_fit(false, b, w);
            let want_ext = no_credit <= b;
            assert_eq!(
                (own.extent, own.credit, own.elide),
                (want_ext, false, !want_ext && bare > b),
                "no source: the ladder is the two-rung one it has always been (budget {b})"
            );
        }
    }

    /// The hero's crew credit: a series is *Created by* its writers, a movie *Directed by* its
    /// directors, and an item the server credited nobody for carries no line at all rather than a
    /// label with nothing after it. The show arm deliberately ignores `Director[]` — labelling a
    /// director "Created by" would state a credit the server never gave.
    #[test]
    fn the_crew_credit_names_the_right_job_for_the_kind_of_item() {
        let _l = crate::testlock::serial();
        let mut movie = item(&["Jane Doe", "Ann Other"]);
        movie.is_show = false;
        assert_eq!(
            super::hero_credit(&movie),
            Some(("Directed by", vec!["Jane Doe", "Ann Other"]))
        );

        let mut show = item(&["Pilot Director"]);
        show.is_show = true;
        show.crew = vec![credit("Pilot Director", "Director"), credit("Kerry Ehrin", "Writer")];
        assert_eq!(super::hero_credit(&show), Some(("Created by", vec!["Kerry Ehrin"])));

        // …and a show whose agent sent no writer has no creator to name
        show.crew = vec![credit("Pilot Director", "Director")];
        assert_eq!(super::hero_credit(&show), None);
        assert!(!super::has_people(&show), "no credit and no cast → no right-hand column at all");
        show.cast = vec![credit("Anne Actor", "Herself")];
        assert!(super::has_people(&show), "a cast alone still earns the column (and its wedge)");
    }

    /// The facts row and the people column are LEVEL, so the thing that keeps them apart is a
    /// width bound and nothing else.
    ///
    /// Bottom-anchoring the column let it grow up into the facts line's own cap band at three
    /// lines — the ordinary case, a one-line credit over a cast list that wraps — and the right end
    /// of that facts line is where the PLEX PASS capsule sits. Vertically they are allowed to
    /// share the band (the mock's own hero does exactly that); what must never happen is the two
    /// runs of ink MEETING, and the only thing that can prevent it is the row stopping short of
    /// the column's left edge. `draw_play_mode` carried no budget at all, and the worst case
    /// measures ~95px inside the column on the shipped font.
    #[test]
    fn the_facts_row_stops_short_of_the_people_column() {
        let _l = crate::testlock::serial();
        use crate::ui::consts::{MARGIN_X, SCR_W};
        assert!(
            super::FACTS_R < SCR_W - MARGIN_X - super::PEOPLE_W,
            "the facts row's bound must sit LEFT of the column it is bounded against"
        );
        // …and the reason the bound is load-bearing: at three lines the column's top line is inside
        // the facts line's band (a CAPTION cap band is ~17px on the shipped font).
        let ch = super::hero_chain(76.0, true);
        assert!(
            super::people_top(ch.btn_y, 3) < ch.facts_y + 17.0,
            "three lines puts the column's top line level with the facts row — that is the case the \
             width bound exists for"
        );
    }

    /// The people column is anchored by its BOTTOM, so its last line sits on the action row's
    /// bottom edge however many lines the block takes — and each extra line grows UPWARD, into the
    /// hero's air, never down into the section flow below.
    #[test]
    fn the_people_column_grows_upward_off_the_button_row() {
        let _l = crate::testlock::serial();
        let btn_y = 846.0;
        let bottom = super::people_bottom(btn_y);
        assert_eq!(bottom, btn_y + super::CD);
        let one = super::people_top(btn_y, 1);
        let four = super::people_top(btn_y, super::PEOPLE_MAX_LINES);
        assert_eq!(one, bottom - super::PEOPLE_LEAD);
        assert!(four < one, "a taller block starts higher, not lower");
        // The below-hero flow is measured from this same edge (`update`: btn_y + CD +
        // BTN_CONTENT_GAP), so however tall the column grows it can never push the sections down —
        // which is the whole reason it hangs off the buttons rather than off a top.
        assert!(bottom <= btn_y + super::CD + super::BTN_CONTENT_GAP);
        assert!(four < bottom, "the block is measured upward from its bottom edge");
    }

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

    // ── the page GROUND (`amb_target`) ───────────────────────────────────────────────────────────
    // Both are pure arithmetic over `amb_target`, but they take the module's serial lock like every
    // other test here: `crate::testlock::serial()` is this module's convention because most of its
    // fns reach `static mut VIEW`, and a reader must not have to work out which two are exempt.

    /// The "no visible change where there is no data" guarantee: on an item PMS sent no
    /// `UltraBlurColors` for — and on a server that sends none at all — the page is byte-identical
    /// to the flat app grey it showed before it had a ground. That is what lets `draw_backdrop`
    /// drop the old `has_blur` gate and draw the wash unconditionally.
    #[test]
    fn an_item_with_no_ultrablur_keeps_the_flat_app_ground() {
        let _serial = crate::testlock::serial();
        assert_eq!(amb_target(None), [theme::SURFACE_APP; 4], "no envelope is the app's own ground");
    }

    /// The ground is the item's OWN corner arrangement, at ONE weight on all four corners — unlike
    /// the person page, which tapers its bottom pair toward its shelves. Pins the flat-weight
    /// decision (and the tl/tr/br/bl ring order surviving the mix) so a later "let's taper it like
    /// person" is a deliberate edit rather than a drift.
    ///
    /// The four sources are deliberately DIM (every one is under `widgets::GROUND_LUMA`), so the
    /// wash's luminance ceiling is the identity here and what is left is the weight alone —
    /// `keyed`'s cap has its own test next to the constant it enforces.
    #[test]
    fn the_page_ground_is_the_items_own_corner_arrangement() {
        let _serial = crate::testlock::serial();
        let b = [[0.30, 0.05, 0.05], [0.05, 0.30, 0.05], [0.05, 0.05, 0.30], [0.25, 0.25, 0.05]];
        let g = amb_target(Some(b));
        for (i, corner) in g.iter().enumerate() {
            for ch in 0..3 {
                let s = theme::SURFACE_APP[ch];
                let want = s + (b[i][ch] - s) * AmbientWash::GROUND_W;
                assert!(
                    (corner[ch] - want).abs() < 1e-6,
                    "corner {i} channel {ch}: {} is not SURFACE_APP leaned GROUND_W toward the source ({want})",
                    corner[ch]
                );
            }
            assert_eq!(corner[3], 1.0, "the ground is opaque — it stands in for the clear");
        }
        // four distinguishable sources stay four distinguishable corners, in the order they came in
        for i in 0..4 {
            for j in (i + 1)..4 {
                assert_ne!(g[i], g[j], "corners {i} and {j} collapsed — the ring order is lost");
            }
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

    use crate::metadata::{Detail, Episode, Season};

    /// Install `d` as the loaded item and park hero focus on `col`. Both statics are crate-wide
    /// (`metadata::CURRENT` and detail's own `VIEW`), so every caller holds `testlock::serial()`.
    /// `hero_set` is seeded from the item so a mount is a settled starting state — a test that
    /// means to exercise a set CHANGE makes it happen explicitly, after the mount.
    fn mount(d: Option<Detail>, col: c_int) {
        metadata::set_current_for_test(d);
        // The copy list is per-ITEM state exactly like `metadata::CURRENT`, and it is a crate
        // global: without this an alt test would leave a third control on the hero row and the
        // NEXT test to run would fail on a count it never touched. Tests that want the control
        // call `alt_arm` after mounting.
        crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
        let set = hero_set();
        let v = view();
        v.section = 0;
        v.col = col;
        v.ep_row = EpRow::Still; // the filmstrip sub-row is retained state; no test may inherit one
        v.tabs = TabStrip::new(); // …and so are the season strip's capsules
        // …and so is the per-section column memory, which is the one that got away. `move_focus`
        // seeds `col` from `saved_col[ns]` when it ENTERS the episode/related/cast row, so a test
        // that walked one of those strips left the next test's first DOWN landing on ITS column
        // rather than on the one this mount just asked for. Order-dependent, therefore invisible
        // until the set of tests changes: it surfaced when route.rs gained a case that holds
        // `testlock` across real socket I/O, which re-ordered who runs beside whom.
        // `reset_view_state` — the production reset this helper stands in for — has always cleared
        // it; the fixture simply did not.
        v.saved_col = [0; 6];
        v.last_resume_ns = 0;
        v.hero_set = set;
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

    /// The restart disc exists exactly when the play beside it would resume — the pill's label and
    /// every index after it hang off that one predicate, and it is Plex's resume RULE
    /// (`metadata::resume_ns`), not a raw `viewOffset > 0`.
    ///
    /// Asserted through [`hero_index`] rather than through a control COUNT, deliberately: the count
    /// also moves with the watched toggle (whose face changes but whose place does not, not
    /// one), so a bare number here would be graded by two independent rules and would have to be
    /// re-derived whenever either changed. `restart_and_the_watch_tail_are_independent` is where
    /// the two are checked together.
    #[test]
    fn restart_disc_tracks_the_resume_the_play_would_actually_apply() {
        let _serial = crate::testlock::serial();
        // is the ↺ disc up, and at the one index it can occupy?
        let restart = || hero_index(HeroCtl::Restart);

        mount(None, 0);
        assert_eq!(restart(), None, "no item loaded: nothing to restart");
        assert_eq!(hero_index(HeroCtl::MarkWatched), Some(1), "…and the row is Play + one disc");

        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(restart(), None, "unwatched movie: Play already starts at 0");

        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(restart(), Some(BTN_RESTART), "part-watched movie: Play resumes, so ↺ is reachable");

        // the two rule edges: a few seconds in is not a resume point, and neither is the 95% tail
        // (both resume_ns to 0, where "Resume" would be a lie and ↺ a second Play)
        mount(Some(movie(4_000, 7_200_000)), 0);
        assert_eq!(restart(), None, "a 4s offset is below Plex's resume floor");
        mount(Some(movie(7_100_000, 7_200_000)), 0);
        assert_eq!(restart(), None, "past the 95% mark the item plays from the start");

        // a show page's Play starts its ON-DECK episode, so the row reads THAT leaf
        mount(Some(show(0, 2_700_000)), 0);
        assert_eq!(restart(), None, "show whose on-deck episode has not been started");
        mount(Some(show(600_000, 2_700_000)), 0);
        assert_eq!(restart(), Some(BTN_RESTART), "show whose on-deck episode is part-watched");
        view().section = 1;
        assert_eq!(focus(), -1, "focus() still reports -1 off the hero section");

        mount(None, 0);
    }

    /// The row's two conditional groups are independent and answer DIFFERENT questions, which is
    /// the whole reason a part-watched item does not grow a second watch control: the ↺ disc asks
    /// "would Play resume" and the toggle asks "is the watched flag set". A part-watched item
    /// answers yes to the first and *no* to the second — so it gets ↺ **and a ✓**, three controls,
    /// the widest an ordinary single-source page gets.
    #[test]
    fn restart_and_the_watch_tail_are_independent() {
        let _serial = crate::testlock::serial();

        // never started: no resume to ignore, one disc to offer
        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(hero_btns(), 2, "Play + ✓");
        assert_eq!(hero_action(1), HeroAction::MarkWatched);

        // watched, with no resume point: still two controls, but the tail disc is the other one
        mount(Some(Detail { watched: true, ..movie(0, 7_200_000) }), 0);
        assert_eq!(hero_btns(), 2, "Play + −");
        assert_eq!(hero_action(1), HeroAction::MarkUnwatched, "a finished item offers only the way back");

        // part-watched: ↺ for the resume point, and the toggle still reads ✓ because the item is
        // NOT watched. The two facts, each with its own control — the widest ordinary row.
        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(hero_btns(), 3, "Play + ↺ + ✓");
        assert_eq!(hero_action(1), HeroAction::Restart);
        assert_eq!(hero_action(2), HeroAction::MarkWatched);

        // a show mid-run has no resume point of its own, so it is the toggle alone — and it reads
        // ✓, because the question is whether the SERIES is watched and it is not
        mount(Some(show(0, 2_700_000)), 0);
        assert_eq!(hero_btns(), 2, "Play + ✓, with no ↺");
        assert_eq!(hero_action(1), HeroAction::MarkWatched);

        mount(None, 0);
    }

    /// **The optimistic flip settles a LEAF's tail on the press frame and a CONTAINER's a round
    /// trip late** — pinned because it is the one window in which this row is behind the press,
    /// and because the reason lives in `metadata::set_watched_local` rather than in the resolver.
    ///
    /// That function edits the matched item's own `watched` and `resume_ms`, which is the whole of
    /// a leaf's evidence: a part-watched movie marked watched loses its resume point and its ↺ disc
    /// in the same instant, and the toggle flips to −. A SHOW's progress is not on the show — it is
    /// `on_deck` and the seasons' `viewed_leaf_count`, which keep the server's pre-press values —
    /// so the row keeps reading `InProgress` until `refresh_view_state`'s re-read replaces the
    /// item. The toggle is not WRONG meanwhile, it is merely stale: it still offers ✓, which is
    /// what the server still believes. Teach the data layer to carry a container's evidence and
    /// THIS test is what should fail.
    #[test]
    fn the_optimistic_flip_settles_a_leaf_at_once_and_a_container_a_round_trip_late() {
        let _serial = crate::testlock::serial();
        use crate::plex::ServerId;

        // LEAF: the press frame IS the settled frame
        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(hero_btns(), 3, "Play + ↺ + ✓");
        assert!(metadata::set_watched_local(ServerId::UNSET, "m1", true), "the flip found the item");
        assert_eq!(hero_btns(), 2, "the resume point goes, and the toggle flips, at once");
        assert_eq!(hero_action(1), HeroAction::MarkUnwatched);

        // CONTAINER: the same call, and the row does not move
        mount(Some(show(600_000, 2_700_000)), 0);
        assert_eq!(hero_btns(), 3, "Play + ↺ + ✓");
        assert!(metadata::set_watched_local(ServerId::UNSET, "s1", true), "the flip found the show");
        assert_eq!(hero_btns(), 3, "the evidence the row reads is the server's, and it has not moved");

        // …and the re-read is what settles it: no on-deck episode, no viewed leaves, watched
        metadata::set_current_for_test(Some(Detail {
            watched: true,
            seasons: Vec::new(),
            on_deck: None,
            ..show(0, 2_700_000)
        }));
        assert_eq!(hero_btns(), 2, "Play + −");
        assert_eq!(hero_action(1), HeroAction::MarkUnwatched);

        mount(None, 0);
    }

    /// **The watch-state resolver's truth table** — the pure function the whole tail hangs off, and
    /// the one place the leaf rule and the container rule are stated side by side. A leaf reads its
    /// own resume position under Plex's rule; a container has none of its own, so it reads whether
    /// its leaves have been started and not finished. Everything else in this lane (which discs are
    /// drawn, which indices they take, which verb a press writes) is a consequence of this answer.
    #[test]
    fn the_watch_state_resolver_answers_leaf_and_container_by_their_own_rules() {
        // ---- LEAVES: the resume the Play pill would apply, then the flag
        let mark = |d: &Detail, resume| hero_watch_state(d, resume);
        assert_eq!(mark(&movie(0, 7_200_000), 0), PosterMark::None, "never started");
        assert_eq!(mark(&movie(1_800_000, 7_200_000), 1_800_000_000_000), PosterMark::InProgress);
        assert_eq!(mark(&Detail { watched: true, ..movie(0, 7_200_000) }, 0), PosterMark::Watched);
        // …and the precedence, on the one item PMS reports as BOTH: a finished film someone has
        // started again is in the middle of that re-watch, which is what the viewer is doing
        assert_eq!(
            mark(&Detail { watched: true, ..movie(1_800_000, 7_200_000) }, 1_800_000_000_000),
            PosterMark::InProgress,
            "a re-watch in flight outranks the watched flag"
        );
        // the RULE's edges, which is why `resume` is `hero_resume_ns`'s answer and not a raw
        // viewOffset: neither of these puts a "Resume" on the pill, so neither is in the middle
        assert_eq!(mark(&movie(4_000, 7_200_000), 0), PosterMark::None, "4s in is not started");
        assert_eq!(mark(&movie(7_100_000, 7_200_000), 0), PosterMark::None, "past 95% is finished");

        // ---- CONTAINERS: no resume of their own, so `show_started` minus the watched end
        let untouched = Detail { seasons: Vec::new(), on_deck: None, ..show(0, 2_700_000) };
        assert_eq!(mark(&untouched, 0), PosterMark::None, "a show nobody has opened");
        assert_eq!(
            mark(&show(0, 2_700_000), 0),
            PosterMark::InProgress,
            "one watched leaf and the series is mid-run, with no resume point anywhere"
        );
        // `show_started` is TRUE for a FINISHED show too — it asks "touched at all" — so the
        // watched flag is what has to reject it, or a finished series would never offer − alone
        assert_eq!(
            mark(&Detail { watched: true, ..show(0, 2_700_000) }, 0),
            PosterMark::Watched,
            "every leaf seen: finished, not mid-run"
        );
        assert_eq!(
            mark(&Detail { watched: true, ..show(600_000, 2_700_000) }, 600_000_000_000),
            PosterMark::InProgress,
            "…until an episode of it is started again"
        );
    }

    /// **What a press WRITES comes from the disc, not from the item.** The verb used to be derived
    /// (`if watched { Unwatched } else { Watched }`), which a part-watched item has no single answer
    /// for — and which meant a press acted on a fact that could have moved since the frame the user
    /// was looking at. The toggle's FACE now names the verb, and the face is a fact about the item
    /// that cannot change between the frame drawn and the press dispatched.
    #[test]
    fn each_watch_disc_writes_its_own_verb() {
        let _serial = crate::testlock::serial();
        use crate::viewstate::Write;

        assert_eq!(HeroAction::MarkWatched.watch_write(), Some(Write::Watched));
        assert_eq!(HeroAction::MarkUnwatched.watch_write(), Some(Write::Unwatched));
        for a in [HeroAction::Play, HeroAction::Restart, HeroAction::Alt, HeroAction::None] {
            assert_eq!(a.watch_write(), None, "{a:?} is not a view-state write");
        }

        // …and through the row, where the toggle wears ONE face at a time and its write is that
        // face's. A part-watched item is not watched, so the reachable verb is `Watched` — the
        // other end is one press away, not one control away.
        let verb = |ctl| hero_index(ctl).map(|i| hero_action(i).watch_write());
        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(verb(HeroCtl::MarkWatched), Some(Some(Write::Watched)));
        assert_eq!(verb(HeroCtl::MarkUnwatched), None, "part-watched is NOT watched: no − face");
        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(verb(HeroCtl::MarkWatched), Some(Some(Write::Watched)));
        assert_eq!(verb(HeroCtl::MarkUnwatched), None, "an unstarted item has nothing to un-watch");
        mount(Some(Detail { watched: true, ..movie(0, 7_200_000) }), 0);
        assert_eq!(verb(HeroCtl::MarkWatched), None, "a finished item is already there");
        assert_eq!(verb(HeroCtl::MarkUnwatched), Some(Some(Write::Unwatched)));

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
        assert_eq!(hero_action(1), HeroAction::MarkWatched, "no restart disc: index 1 is the ✓ disc");
        assert_eq!(hero_action(2), HeroAction::None, "and there is no index 2");

        mount(Some(movie(1_800_000, 7_200_000)), 0);
        assert_eq!(hero_action(0), HeroAction::Play);
        assert_eq!(hero_action(1), HeroAction::Restart);
        assert_eq!(hero_action(2), HeroAction::MarkWatched, "the toggle moved, and OK must follow it");
        assert_eq!(hero_action(3), HeroAction::None, "and there is no index 3");

        mount(None, 0);
    }

    /// **The toggle rewrites the row under its own press, and the focus must follow it.** Marking
    /// a part-watched item watched clears the server's `viewOffset`, so the ↺ disc beside it goes;
    /// and the toggle itself changes FACE, ✓ becoming −. The row goes 3 → 2 under a focus parked at
    /// index 2, and the control under it is not merely displaced — the `HeroCtl` it was is not in
    /// the new set at all, which is the case `index_of` cannot answer and [`watch_index`] must.
    /// Anything else leaves focus past the end, where nothing draws focused and, since
    /// `hero_action` would answer `None`, every later OK is inert.
    #[test]
    fn hero_focus_is_clamped_when_the_control_set_shrinks_under_it() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(1_800_000, 7_200_000)), 2);
        assert_eq!(focus(), 2, "3-button row: focus sits on the toggle, wearing ✓");
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched);

        // the scrobble landed: watched, and no resume point any more — so the ↺ disc is gone and
        // the toggle has flipped to −, one index to the left of where the focus was parked
        metadata::set_current_for_test(Some(Detail { watched: true, ..movie(0, 7_200_000) }));
        assert_eq!(hero_btns(), 2);
        assert_eq!(focus(), 1, "focus follows the toggle, and does not sit past the end");
        assert_eq!(view().col, 1, "and it is written back — it IS the state");
        assert_eq!(
            hero_action(view().col),
            HeroAction::MarkUnwatched,
            "still the toggle, now offering the honest undo of the press that got here"
        );

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
    /// so a RIGHT pressed in that window lands on the ✓ disc of a TWO-button row — and if the item
    /// then arrives part-watched, index 1 has become the restart disc. Focus must travel with its
    /// control, or that press plays the item instead of writing view state.
    #[test]
    fn hero_focus_follows_its_control_when_the_set_grows_under_it() {
        let _serial = crate::testlock::serial();

        // mid-fetch: nothing loaded, so the row is Play + ✓ and RIGHT parks on the disc
        mount(None, 0);
        assert_eq!(focus(), 0);
        view().col = 1;
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched, "index 1 is the ✓ disc while the row is short");

        // the fetch lands, part-watched: the row grows a restart disc AT index 1 and a − disc after
        metadata::set_current_for_test(Some(movie(1_800_000, 7_200_000)));
        assert_eq!(focus(), 2, "the focus moved with the disc rather than staying on the index");
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched, "so OK still means what the user aimed at");

        // Play never shifts — it is before the insertion point
        view().col = 0;
        metadata::set_current_for_test(Some(movie(0, 7_200_000)));
        assert_eq!(focus(), 0, "the pill holds index 0 through a set change");

        mount(None, 0);
    }

    // ---- "Also available": the same film on a second pinned source (Shared Sources, deliverable E)
    //
    // The ROW MODEL — the ordering, the gate, the tick — is `ui::alt_sources`' own; what belongs
    // here is the ACTIONS ROW around it: that the control exists only when the panel has something
    // to show, where it sits, what an OK on it means, and that the row's geometry survives a
    // variable-width control appearing in its middle.

    /// Arm the copy store with the design's own pair (your 1080p `Movies` copy and a friend's 4K
    /// `Film Club` one). The store is a crate global like `metadata::CURRENT`, so callers hold
    /// `testlock::serial()` — and must [`alt_clear`] before they end, or the next test's hero row
    /// inherits a control it never asked for.
    ///
    /// The friend's slot is derived from the current server rather than written down: `plex`'s own
    /// tests leave `CURRENT` wherever they last set it (they reset on entry, not on exit), so a
    /// literal `from_raw(1)` collapsed both copies onto one source — and the alt control silently
    /// failed to appear — whenever one of those ran first under the same serial lock.
    fn alt_arm(rk: &str) {
        use crate::ui::alt_sources::{install, reset, AltCopy};
        let here = crate::plex::current_server();
        let theirs = crate::plex::ServerId::from_raw(here.raw().wrapping_add(1));
        // `reset` and `install` are the two ends of a MAILBOX keyed on `(server, rk)` — arming the
        // store means naming the same page at both, exactly as a real mount and its landing do.
        reset(here, rk);
        install(
            here,
            rk,
            vec![
                AltCopy {
                    sid: here,
                    library: "Movies".into(),
                    rk: rk.into(),
                    dur_ms: 7_020_000,
                    res: "1080".into(),
                    ..Default::default()
                },
                AltCopy {
                    sid: theirs,
                    library: "Film Club".into(),
                    owner: Some("friend".into()),
                    rk: "318".into(),
                    dur_ms: 7_020_000,
                    res: "4k".into(),
                    ..Default::default()
                },
            ],
        );
    }
    fn alt_clear() {
        crate::ui::alt_sources::reset(crate::plex::ServerId::UNSET, "");
    }

    /// **The gate, as the actions row sees it.** One source and the row is what it always was — no
    /// control, no index, no layout. A second source holding the film adds exactly one control, and
    /// it lands BEFORE the watched toggle, which stays at the end.
    #[test]
    fn the_actions_row_grows_an_also_available_control_only_for_a_second_source() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(0, 7_200_000)), 0);
        assert_eq!(hero_btns(), 2, "one source: Play + the ✓ disc, exactly as before");
        assert_eq!(hero_action(1), HeroAction::MarkWatched);

        alt_arm("m1");
        assert_eq!(hero_btns(), 3, "a second source is one more control");
        assert_eq!(hero_action(0), HeroAction::Play);
        assert_eq!(hero_action(1), HeroAction::Alt, "…and it sits with the play group, not after the discs");
        assert_eq!(hero_action(2), HeroAction::MarkWatched, "the watch disc still ends the row");
        assert_eq!(hero_index(HeroCtl::MarkWatched), Some(2));

        // EVERY conditional control at once, which is the widest the row ever gets: the resume
        // disc, the alt pill, then the watched toggle
        mount(Some(movie(1_800_000, 7_200_000)), 0);
        alt_arm("m1");
        assert_eq!(hero_btns(), 4);
        assert_eq!(hero_action(1), HeroAction::Restart);
        assert_eq!(hero_action(2), HeroAction::Alt);
        assert_eq!(hero_action(3), HeroAction::MarkWatched);
        assert_eq!(hero_action(4), HeroAction::None, "and there is no index 4");

        alt_clear();
        assert_eq!(hero_btns(), 3, "the control leaves with the copies that justified it");

        // …and the NEGATIVE the gate is really about: a copy list that names only the source you
        // are already on adds nothing. That is also how "not pinned" reads from here — the store
        // holds the copies on PINNED sources and nothing else, so a source nobody pinned cannot
        // put a control on this row however many copies of the film it has.
        mount(Some(movie(0, 7_200_000)), 0);
        use crate::ui::alt_sources::{install, reset, AltCopy};
        let here = crate::plex::current_server();
        reset(here, "m1");
        install(
            here,
            "m1",
            vec![
                AltCopy { sid: here, library: "Movies".into(), rk: "m1".into(), ..Default::default() },
                AltCopy { sid: here, library: "4K Movies".into(), rk: "9".into(), ..Default::default() },
            ],
        );
        assert_eq!(hero_btns(), 2, "two copies on ONE source are not 'also available elsewhere'");

        alt_clear();
        mount(None, 0);
    }

    /// **The decisive property, proven through the real press path**: OK on another copy asks to
    /// open THAT SERVER's page for the film, and leaves the page you are on completely alone.
    ///
    /// This is the call the design is built around, and the one a device capture could most easily
    /// appear to confirm without it being true — a route change is a route change whoever caused
    /// it. So it is pinned here instead, at both ends: the request names the OTHER server and that
    /// server's own ratingKey, and NOTHING about the current page moves — no in-place swap, no
    /// second navigation request, no change of mounted item. (The switch and the mount are
    /// `app.rs`'s, deliberately: re-pointing `client()` invalidates every per-server store.)
    #[test]
    fn ok_on_another_copy_asks_for_that_servers_page_and_leaves_this_one_alone() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(0, 7_200_000)), 0);
        alt_arm("m1");
        focus(); // spend the set change, so parking on the control below is deliberate

        // press OK on the control: it asks for the panel and commits nothing
        let alt = hero_index(HeroCtl::Alt).expect("the control is up");
        view().col = alt;
        assert_eq!(hero_action(focus()), HeroAction::Alt);
        assert!(!on_ok(), "opening a list never starts playback");
        assert!(take_alt_request(), "the press asked app.rs to present the panel");
        assert!(take_alt_open().is_none(), "…and chose no copy yet");

        // app.rs presents it beside the control's drawn rect (any rect here — the host measures no
        // pills, which is why `alt_sources::open` takes the anchor rather than finding it)
        crate::ui::alt_sources::open(Rect::new(300.0, 812.0, 300.0, 60.0));
        assert!(crate::ui::alt_sources::is_open());
        assert!(!focus_is_card(), "nothing behind an open panel is pressable");

        // it opens on the copy you are ON, so one DOWN reaches the other one
        crate::ui::alt_sources::move_focus(SDLK_DOWN as c_int);
        assert!(!on_ok(), "committing a row never starts playback either");
        assert!(!crate::ui::alt_sources::is_open(), "…and the panel closed behind it");

        let (sid, rk) = take_alt_open().expect("the press chose the other copy");
        assert_ne!(sid, crate::plex::current_server(), "the destination is the OTHER server");
        assert_eq!(rk, "318", "…addressed by ITS ratingKey, not this server's");

        // and the page you are standing on is untouched: the copy was not swapped under you, and
        // no ordinary open request was raised alongside (which would be a second navigation)
        assert!(take_open_request().is_none(), "the copy must not be switched in place");
        assert_eq!(mounted_rk(), "m1", "this page stays this page until app.rs navigates");
        assert_eq!(hero_btns(), 3, "…control set and all");

        alt_clear();
        mount(None, 0);
    }

    /// The other half of the same press: OK on the copy you are ALREADY on asks for nothing. It is
    /// a picker whose tick already answered the question, so the panel simply dismisses rather than
    /// re-mounting the page under itself.
    #[test]
    fn ok_on_the_copy_you_are_on_dismisses_and_navigates_nowhere() {
        let _serial = crate::testlock::serial();

        mount(Some(movie(0, 7_200_000)), 0);
        alt_arm("m1");
        crate::ui::alt_sources::open(Rect::new(300.0, 812.0, 300.0, 60.0));
        assert!(!on_ok());
        assert!(take_alt_open().is_none(), "the row you are on is not a destination");
        assert!(take_open_request().is_none());
        assert!(!crate::ui::alt_sources::is_open(), "…but the press is still spent on dismissing");

        // BACK dismisses it too, and is reported as SPENT so app.rs does not also pop the trail
        crate::ui::alt_sources::open(Rect::new(300.0, 812.0, 300.0, 60.0));
        assert!(back(), "a panel the page has open takes the BACK press");
        assert!(!crate::ui::alt_sources::is_open());
        assert!(!back(), "…and with nothing open the page hands BACK back to app.rs");

        alt_clear();
        mount(None, 0);
    }

    /// The alt control appears in the MIDDLE of the row, from an async landing — the case the old
    /// single-offset shift could not express. Focus must travel with the control it is on, in both
    /// directions, or the OK that follows means a different button than the one the user aimed at.
    #[test]
    fn hero_focus_survives_a_control_appearing_in_the_middle_of_the_row() {
        let _serial = crate::testlock::serial();
        alt_clear();

        // parked on the ✓ disc of a two-control row
        mount(Some(movie(0, 7_200_000)), 1);
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched);

        // the copy list lands: the alt pill is inserted at index 1, under the focus
        alt_arm("m1");
        assert_eq!(focus(), 2, "the focus followed the disc rather than staying on index 1");
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched, "so OK still means what it did a frame ago");

        // …and it survives the list going away again
        alt_clear();
        assert_eq!(focus(), 1);
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched);

        // a focus sitting ON the vanished control has no new index to travel to — the clamp puts it
        // on the row's last control rather than past the end, where every OK would be inert.
        // (`focus()` first, so the set change is already spent and parking on index 1 is a
        // deliberate landing on the alt pill rather than the shift carrying the disc there.)
        alt_arm("m1");
        focus();
        view().col = 1;
        assert_eq!(hero_action(focus()), HeroAction::Alt);
        alt_clear();
        assert_eq!(focus(), 1);
        assert_eq!(hero_action(focus()), HeroAction::MarkWatched, "never an index the row does not have");

        mount(None, 0);
    }

    /// **The hero row is the things you DO to the item, and Track information is not one of them.**
    ///
    /// It shipped here as a ⓘ disc between *Also available* and the watch tail, and it opens from
    /// the About footer's Languages column now (`on_ok`'s section-5 arm) — the block the panel is
    /// about. The row is graded whole, in the set where every conditional control is present,
    /// because "the disc is gone" is only a true statement about a row that would otherwise have
    /// had one: this is a movie with a file, a resume point and a second copy.
    #[test]
    fn the_hero_row_carries_no_track_information_disc() {
        let full = HeroSet { restart: true, alt: true, mark: PosterMark::InProgress };
        let (v, n) = hero_ctls(full);
        assert_eq!(
            &v[..n],
            &[
                HeroCtl::Play,
                HeroCtl::Restart,
                HeroCtl::Alt,
                HeroCtl::MarkWatched,
            ]
        );
        // …and the ordinary set: one server, nothing started. Play, then the one watch disc.
        let plain = HeroSet { restart: false, alt: false, mark: PosterMark::None };
        let (v, n) = hero_ctls(plain);
        assert_eq!(&v[..n], &[HeroCtl::Play, HeroCtl::MarkWatched]);
    }

    /// The row is ACCUMULATED, and with the alt pill it has a variable-width control in the middle
    /// — the exact case a fixed disc pitch gets wrong. Pure: both pill widths are passed in, since
    /// the host links no SDL_ttf (the split `hero_btn_rect_at` exists for).
    ///
    /// Every set the row can be in, as the PRODUCT of its three independent facts — the ↺ disc, the
    /// alt pill and the watch-state tail — rather than a hand-listed sample. The tail is the one
    /// that adds a SECOND control at the far end, so a walk that was right for a one-disc tail can
    /// still be wrong for another, and a sample list would not have said so.
    /// Every `HeroSet` the row can be in, as the product of its three independent facts.
    fn every_hero_set() -> impl Iterator<Item = HeroSet> {
        [false, true].into_iter().flat_map(|restart| {
            [false, true].into_iter().flat_map(move |alt| {
                [PosterMark::None, PosterMark::InProgress, PosterMark::Watched]
                    .into_iter()
                    .map(move |mark| HeroSet { restart, alt, mark })
            })
        })
    }

    /// The row's widths with the watched toggle CLOSED — what every geometry assertion that is not
    /// about the unfurl wants, and what the row looked like before the unfurl existed.
    fn closed_widths(pw: f32, alt_w: f32) -> HeroWidths {
        HeroWidths { pill: pw, alt: alt_w, disc: [CD; 2] }
    }

    #[test]
    fn the_actions_row_accumulates_around_a_variable_width_control() {
        let (y, pw, alt_w) = (812.0f32, PW + 62.0, 340.0f32);
        // Swept at THREE unfurl phases, not just at rest: a watch disc is a variable-width control
        // too now, and mid-animation it is a width no constant in this file mentions. `0.37` is an
        // arbitrary in-between — the point is that nothing downstream may assume the two endpoints.
        for e in [0.0f32, 0.37, 1.0] {
            let cw = HeroWidths {
                pill: pw,
                alt: alt_w,
                disc: [CircleButton::cap_w(CD, e, 120.0), CircleButton::cap_w(CD, e, 160.0)],
            };
            for set in every_hero_set() {
                let (_, n) = hero_ctls(set);
                let mut prev = hero_btn_rect_at(set, 0, y, cw);
                assert_eq!((prev.x, prev.w), (MARGIN_X, pw), "the play pill starts the row at its own width");
                for i in 1..n as c_int {
                    let r = hero_btn_rect_at(set, i, y, cw);
                    assert_eq!(r.x, prev.x + prev.w + CGAP, "control {i} accumulates past its neighbour");
                    assert_eq!(r.h, CD, "every control in the row stands the same height");
                    assert!(!prev.contains(r.cx(), r.cy()) && !r.contains(prev.cx(), prev.cy()));
                    prev = r;
                }
                // the alt control is the pill's width, not a disc's — the whole reason the walk exists
                if let Some(i) = index_of(set, HeroCtl::Alt) {
                    assert_eq!(hero_btn_rect_at(set, i, y, cw).w, alt_w);
                }
                // …and the watched toggle is the width its unfurl asked for, not a disc pitch —
                // whichever of its two faces this set is wearing.
                assert_eq!(
                    hero_btn_rect_at(set, watch_index(set).unwrap(), y, cw).w,
                    cw.disc[1],
                    "the toggle at e={e} on {set:?}"
                );
                if let Some(i) = index_of(set, HeroCtl::Restart) {
                    assert_eq!(hero_btn_rect_at(set, i, y, cw).w, cw.disc[0], "the ▶| disc at e={e}");
                }
                // …and a watch-state disc still ends the row, whichever tail this set has
                assert_eq!(hero_btn_rect_at(set, n as c_int - 1, y, cw).x, prev.x);
                let last = ctl_at(set, n as c_int - 1);
                assert!(
                    matches!(last, Some(HeroCtl::MarkWatched) | Some(HeroCtl::MarkUnwatched)),
                    "the row ends on a watch disc, not on {last:?}"
                );
            }
        }
    }

    /// **The unfurl can never push the row past the people column.** [`disc_caps_at`] is what
    /// guarantees it, and this is the guarantee: over every set, every unfurl phase and a label
    /// sweep that runs from trivially short to absurd, the row's right edge stays inside
    /// [`FACTS_R`]. A disc that cannot afford its verb must report `CD` and stay a disc.
    ///
    /// The widths here are the same generous stand-ins the sibling tests use (the host links no
    /// SDL_ttf), which is why the sweep matters more than any single number: the real question is
    /// whether the RULE holds, not whether today's five strings happen to fit.
    ///
    /// The phases swept are the ones the springs can actually be in. They are complementary drives
    /// of one stiffness, so their sum never exceeds one full capsule — see [`disc_caps_at`]. That
    /// bound is the rule's premise, so this honours it rather than testing states the pair cannot
    /// reach: the hand-off `(e, 1-e)` in both directions, and each disc alone.
    #[test]
    fn an_unfurling_disc_never_crosses_the_people_column() {
        let (pw, alt_w) = (PW + 62.0, 340.0f32);
        let mut ever_opened = false;
        for set in every_hero_set() {
            let (_, n) = hero_ctls(set);
            // trivially short, the real verbs paired as the row really pairs them, and two absurd
            for lw in [[10.0f32, 12.0], [201.0, 316.0], [201.0, 233.0], [400.0, 400.0], [900.0, 40.0]] {
                for e in every_unfurl_phase() {
                    let disc = disc_caps_at(set, pw, alt_w, e, lw);
                    let cw = HeroWidths { pill: pw, alt: alt_w, disc };
                    let last = hero_btn_rect_at(set, n as c_int - 1, 0.0, cw);
                    assert!(
                        last.x + last.w <= FACTS_R,
                        "{set:?} at e={e:?} labels={lw:?} ends at {} past {FACTS_R}",
                        last.x + last.w
                    );
                    for w in disc {
                        assert!(w >= CD, "a disc never draws narrower than itself");
                        ever_opened |= w > CD;
                    }
                }
            }
        }
        assert!(ever_opened, "the sweep must actually exercise an OPEN capsule, not only refusals");
    }

    /// The unfurl phases the PAIR can actually be in — see [`disc_caps_at`]'s complementary-spring
    /// argument, which is this sweep's premise.
    fn every_unfurl_phase() -> impl Iterator<Item = [f32; 2]> {
        [0.0f32, 0.25, 0.5, 0.75, 1.0]
            .into_iter()
            .flat_map(|e| [[e, 1.0 - e], [1.0 - e, e], [e, 0.0], [0.0, e]])
    }

    /// **Every verb this row can actually carry fits the widest row it can actually build.** The
    /// sweep above proves the RULE (nothing crosses `FACTS_R`); this proves the rule is not
    /// achieving that by refusing everything, which it would also pass. Real metrics are
    /// unavailable here — the host links no SDL_ttf — so the verbs are stand-ins measured from the
    /// shipped `appfont-bold` at `size::BODY`, and the pills are the generous ones the sibling
    /// tests use. A verb that grows past this has to be re-checked on the device, not here.
    #[test]
    fn the_real_verbs_all_fit_the_widest_row() {
        let (pw, alt_w) = (PW + 62.0, 340.0f32);
        // measured 2026-08-21 from pkg/appfont-bold.ttf at 28px
        const VERBS: [(&str, f32); 5] = [
            ("Play from Start", 201.0),
            ("Mark as Watched", 233.0),
            ("Mark as Unwatched", 267.0),
            ("Mark Show as Watched", 316.0),
            ("Mark Show as Unwatched", 350.0),
        ];
        for (verb, lw) in VERBS {
            for set in every_hero_set() {
                // paired with the widest verb the OTHER slot can carry, since the budget is shared
                let widest = VERBS.iter().map(|v| v.1).fold(0.0f32, f32::max);
                let got = disc_caps_at(set, pw, alt_w, [1.0, 0.0], [lw, widest]);
                assert!(got[0] > CD, "{verb:?} ({lw}px) must open on {set:?}, not be refused");
            }
        }
    }

    /// The budget refuses whole verbs, never partial ones — the facts row's own ladder rule. A
    /// label a few pixels too wide for the room collapses BOTH discs all the way back (the pair
    /// shares one budget, so they are refused together — [`disc_caps_at`]).
    #[test]
    fn a_verb_that_does_not_fit_is_dropped_whole() {
        let (pw, alt_w) = (PW + 62.0, 340.0f32);
        let widest = HeroSet { restart: true, alt: true, mark: PosterMark::InProgress };
        let (_, n) = hero_ctls(widest);
        let closed = hero_btn_rect_at(widest, n as c_int - 1, 0.0, closed_widths(pw, alt_w));
        let budget = CircleButton::label_budget(CD, FACTS_R - (closed.x + closed.w));
        assert!(budget > 0.0, "the closed widest row must leave SOME room, else the rule is untestable");

        let open = disc_caps_at(widest, pw, alt_w, [0.0, 1.0], [budget; 2]);
        assert!(open[1] > CD, "a verb that exactly spends the budget opens");
        let last =
            hero_btn_rect_at(widest, n as c_int - 1, 0.0, HeroWidths { pill: pw, alt: alt_w, disc: open });
        assert!(last.x + last.w <= FACTS_R, "…and spending the whole budget still clears the column");

        assert_eq!(
            disc_caps_at(widest, pw, alt_w, [0.0, 1.0], [budget + 3.0; 2]),
            [CD; 2],
            "a verb that does not fit leaves BOTH controls plain discs"
        );
    }

    /// The unfurled verbs ARE the press-and-hold menu's — one vocabulary for one set of actions.
    /// `item_menu` builds its rows from these same strings, and a control that said one thing while
    /// a row beside it said another would read as two different actions.
    ///
    /// …and the SUBJECT-naming pair are those same words with one noun inserted, never a reworded
    /// second vocabulary: the toggle names what it writes to, it does not rephrase the write.
    #[test]
    fn the_unfurled_verbs_are_the_menus_own() {
        // The C strings the painter needs cannot BE the shared `&str` (a `CString` per frame is an
        // allocation on the draw path), so the agreement is asserted instead of shared.
        assert_eq!(MARK_WATCHED_LABEL.to_str().unwrap(), crate::ui::widgets::MARK_WATCHED_VERB);
        assert_eq!(MARK_UNWATCHED_LABEL.to_str().unwrap(), crate::ui::widgets::MARK_UNWATCHED_VERB);
        assert_eq!(PLAY_FROM_START_LABEL.to_str().unwrap(), crate::ui::widgets::PLAY_FROM_START_VERB);
        for (plain, named) in [
            (MARK_WATCHED_LABEL, MARK_SHOW_WATCHED_LABEL),
            (MARK_UNWATCHED_LABEL, MARK_SHOW_UNWATCHED_LABEL),
        ] {
            let (p, w) = (plain.to_str().unwrap(), named.to_str().unwrap());
            assert_eq!(w, p.replacen("Mark ", "Mark Show ", 1), "{w:?} is {p:?} naming its subject");
        }
    }

    /// **Both discs carry a verb; neither pill does; the two never share a slot.** The slots index
    /// the spring pair, so a collision would animate one control off the other's focus — and a
    /// missing one would leave a disc mute, which is the whole thing this lane exists to fix.
    ///
    /// Plus the row-level rule the design turns on: **exactly one** watched toggle, always last, in
    /// every set the page can build.
    #[test]
    fn the_row_offers_exactly_one_watched_toggle() {
        assert_eq!(disc_verb(HeroCtl::Restart, false), Some((0, PLAY_FROM_START_LABEL)));
        assert_eq!(disc_verb(HeroCtl::Restart, true), Some((0, PLAY_FROM_START_LABEL)),
            "the restart disc acts on the subject the blurb just named, so it never takes a noun");
        assert_eq!(disc_verb(HeroCtl::MarkWatched, false), Some((1, MARK_WATCHED_LABEL)));
        assert_eq!(disc_verb(HeroCtl::MarkUnwatched, false), Some((1, MARK_UNWATCHED_LABEL)));
        assert_eq!(disc_verb(HeroCtl::MarkWatched, true), Some((1, MARK_SHOW_WATCHED_LABEL)));
        assert_eq!(disc_verb(HeroCtl::MarkUnwatched, true), Some((1, MARK_SHOW_UNWATCHED_LABEL)));
        for c in [HeroCtl::Play, HeroCtl::Alt] {
            for named in [false, true] {
                assert_eq!(disc_verb(c, named), None, "{c:?} is a pill — it says its word at rest");
            }
        }
        // no two controls of one set may share a spring slot
        for set in every_hero_set() {
            let (v, n) = hero_ctls(set);
            let mut seen = [false; 2];
            for &c in &v[..n] {
                if let Some((slot, _)) = disc_verb(c, false) {
                    assert!(!seen[slot], "{set:?}: two controls claim unfurl slot {slot}");
                    seen[slot] = true;
                }
            }
            let verbs = v[..n].iter().filter(|c| is_watch_ctl(**c)).count();
            assert_eq!(verbs, 1, "{set:?} must offer exactly one watched toggle, not {verbs}");
            assert_eq!(watch_index(set), Some(n as c_int - 1), "{set:?}: and it is the row's last");
            // …wearing the face of the write it would perform. A PART-WATCHED item is not watched,
            // so it reads ✓ — the case the row used to answer with both faces at once.
            let want = if set.mark == PosterMark::Watched { HeroCtl::MarkUnwatched } else { HeroCtl::MarkWatched };
            assert_eq!(ctl_at(set, n as c_int - 1), Some(want), "{set:?}");
        }
    }

    /// **The widest row the page can produce still clears the people column.** The part-watched
    /// tail added a FIFTH control to a row that had four, and this row shares its band with the
    /// right-hand people column — which is bottom-anchored on it (`people_bottom`) and grows
    /// upward, so an overrun would not be a clipped button but two blocks of the hero written over
    /// each other. `FACTS_R` is the left edge the facts line above already yields at, and the
    /// action row is now measured against the same bound.
    ///
    /// The pill widths are stand-ins (the host links no SDL_ttf), and generous ones: "Resume" at
    /// `PW + 62` is the wider of the two words the play pill ever carries, and 340 is well past
    /// what *Also available* measures at `size::BODY`.
    ///
    /// This is the CLOSED row. The unfurl's own half of the bound — that a disc opening into a
    /// labelled capsule cannot cross the same line — is
    /// `an_unfurling_watch_toggle_never_crosses_the_people_column`, and it is a separate test
    /// because it is a separate rule: this one says the row FITS, that one says the animation may
    /// only spend what this one leaves over.
    #[test]
    fn the_widest_action_row_clears_the_people_column() {
        let (y, pw, alt_w) = (812.0f32, PW + 62.0, 340.0f32);
        let widest = HeroSet { restart: true, alt: true, mark: PosterMark::InProgress };
        let (_, n) = hero_ctls(widest);
        assert_eq!(n, 4, "every conditional control at once");

        let last = hero_btn_rect_at(widest, n as c_int - 1, y, closed_widths(pw, alt_w));
        assert!(
            last.x + last.w < FACTS_R,
            "the action row ends at {} and the people column starts at {FACTS_R}",
            last.x + last.w
        );
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

    /// **The Related menu's anchor is the tile the pointer would hit, and the tile the strip drew.**
    ///
    /// Three formulas have to agree for the popover to hang off the right poster: `card_row::strip`
    /// draws at `margin_x + i*pitch - scroll`, `strip_at` inverts it for the pointer, and
    /// `related_tile_rect` re-derives it for the [`Opener`](crate::ui::popover::Opener). The first
    /// two were already pinned against each other; this adds the third, which is the one a menu
    /// that opens beside the WRONG card would come from — and it is silent, because the panel still
    /// appears and the tile it lifts still looks like a tile.
    ///
    /// It also grades the two refusals, which matter more than they look: a menu opened on a shelf
    /// that does not hold focus, or on a column past the end of the shelf that is actually loaded,
    /// would anchor to a poster nobody is looking at. A page can be re-mounted under a retained
    /// focus column (a Related jump reloads the page beneath it), so the out-of-range case is
    /// reachable rather than theoretical.
    #[test]
    fn the_related_menus_anchor_is_the_tile_the_shelf_drew() {
        // The strip draws tile `i` at `sty.margin_x + i*pitch`, translated by `-scroll_x`, in a band
        // one heading below the section top — and the anchor must land on exactly that poster.
        for &top in &[0.0f32, 240.0, 806.0] {
            for &sx in &[0.0f32, 137.0, 900.0] {
                for i in 0..6usize {
                    let r = related_tile_rect_at(i, top, sx);
                    // the same left edge `card_row::strip` advances by (`strip_x_matches_the_shared
                    // _shelf_formula` above pins `strip_x` to the component's own formula)
                    assert!((r.x - (strip_x(i, REL_W + REL_GAP) - sx)).abs() < 0.01, "the anchor's x is the drawn x");
                    assert!((r.y - (top + REL_LABEL_H)).abs() < 0.01, "…in the band below the heading");
                    assert_eq!((r.w, r.h), (REL_W, REL_H), "…at the poster's own size, UNSCALED");
                    // …and the pointer's inverse agrees that a click in the middle of it is tile `i`,
                    // so the menu, the focus and the click can never mean three different posters
                    assert_eq!(strip_at(r.x + REL_W * 0.5, sx, 6, REL_W + REL_GAP, REL_W), Some(i));
                }
            }
        }

        // ---- and the two refusals, which decide whether a menu opens on a poster at all ----
        let _serial = crate::testlock::serial();
        let rel = |rk: &str| crate::metadata::Related { rk: rk.into(), ..Default::default() };
        mount(
            Some(Detail {
                rk: "m1".into(),
                related: vec![rel("r0"), rel("r1"), rel("r2")],
                ..Default::default()
            }),
            0,
        );

        // focus elsewhere on the page is not focus on this shelf, whatever the column says — the
        // filmstrip's own menu answers for section 2, and both must not answer at once
        view().section = 2;
        view().col = 1;
        assert!(focused_related().is_none(), "the filmstrip's focus is not the Related shelf's");

        view().section = 3;
        for i in 0..3usize {
            view().col = i as c_int;
            assert_eq!(focused_related().map(|m| m.rk.as_str()), Some(["r0", "r1", "r2"][i]));
        }

        // A column past the loaded shelf opens nothing rather than anchoring off its end. Reachable
        // rather than theoretical: the column is retained per section, and a Related jump reloads
        // the page underneath it — so the shelf can get shorter while `col` stays where it was.
        view().col = 7;
        assert!(focused_related().is_none(), "no tile 7 to open a menu on");

        mount(None, 0);
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

    /// **An unfurled capsule is clickable across the word it grew to show.** The whole point of
    /// `hero_btn_rect` taking the animated width is that the pointer grades against the shape on
    /// screen: a hit-test still walking bare discs would leave the label a decoration you can read
    /// and cannot press, and would answer NOTHING for most of the capsule the user is aiming at.
    ///
    /// Swept across the unfurl, not just at the ends: the mid-animation widths are where a second,
    /// stale derivation of the row would diverge without ever being wrong at rest.
    #[test]
    fn a_pointer_lands_on_the_capsule_the_unfurl_drew() {
        let (y, pw) = (812.0f32, PW + 62.0);
        // a part-watched item, so the ↺ disc stands immediately BEFORE the toggle that opens
        let set = HeroSet { restart: true, alt: false, mark: PosterMark::InProgress };
        let i = watch_index(set).expect("every set has a toggle");
        assert_eq!(ctl_at(set, i - 1), Some(HeroCtl::Restart), "the neighbour this test is about");
        for e in [0.2f32, 0.6, 1.0] {
            // the toggle opening while the ▶| disc beside it is shut — the phase a pointer meets
            let disc = disc_caps_at(set, pw, 0.0, [0.0, e], [201.0, 267.0]);
            let cw = HeroWidths { pill: pw, alt: 0.0, disc };
            let (prev, cap) = (hero_btn_rect_at(set, i - 1, y, cw), hero_btn_rect_at(set, i, y, cw));
            let cy = y + CD * 0.5;

            assert!(cap.w > CD, "e={e}: the toggle really is a capsule");
            assert!(cap.contains(cap.x + cap.w - 1.0, cy), "e={e}: its far edge answers for itself");
            assert!(cap.contains(cap.x + 1.0, cy), "e={e}: …and so does the disc it grew out of");
            assert!(!prev.contains(cap.x + 1.0, cy), "e={e}: the ↺ disc does not answer for it");
            assert_eq!(cap.x, prev.x + prev.w + CGAP, "e={e}: it opens rightward off a fixed edge");
            // the gutter between them belongs to neither control
            let gutter = prev.x + prev.w + CGAP * 0.5;
            assert!(!prev.contains(gutter, cy) && !cap.contains(gutter, cy), "e={e}");
        }
    }

    /// The hero action row, on a row whose control SET changes under the pointer. With a resume
    /// point the ↺ disc takes index 1 and the toggle slides past it, so a hit-test built on
    /// per-control constants would answer one control out of step for exactly the items a pointer
    /// user is most likely to open. The invariant is therefore the ACCUMULATION `hero_btn_rect`
    /// performs — the pill at the margin, every later control one `CD + CGAP` past the last —
    /// asserted whatever the pill measures (the host has no fonts, so `pill_w` floors at `PW`
    /// there and carries real metrics on the TV; the walk holds either way).
    ///
    /// It drives `hero_btn_rect_at`, which is what `hero_btn_rect` resolves both pill widths INTO,
    /// because measuring one reaches SDL_ttf and the host links none (`hero_btn_rect_at`'s doc).
    #[test]
    fn hero_action_row_hit_matches_the_drawn_controls_at_every_set_size() {
        let _serial = crate::testlock::serial();
        let y = 812.0; // any row top; the live one comes from hero_chain
        alt_clear(); // this case is about the play group; the alt control has its own

        // (item, control count, the pill width its label measures to). The widths stand in for
        // `hero_pill_w` — "Resume" is the wider word, and the row must accumulate off whichever it
        // is rather than off a constant. All three watch states are here: the part-watched one is
        // the row that grows the ↺ disc, and the finished one is the row whose toggle wears the
        // OTHER glyph — same walk, two different lengths.
        for (d, want, pw) in [
            (movie(0, 7_200_000), 2, PW),
            (Detail { watched: true, ..movie(0, 7_200_000) }, 2, PW),
            (movie(1_800_000, 7_200_000), 3, PW + 62.0),
        ] {
            mount(Some(d), 0);
            assert_eq!(hero_btns(), want, "the control set this case means to exercise");
            let set = hero_set();
            let rect = |i: c_int| hero_btn_rect_at(set, i, y, closed_widths(pw, 0.0));

            let pill = rect(BTN_PLAY);
            assert_eq!(pill.x, MARGIN_X, "the pill starts at the margin");
            assert_eq!((pill.w, pill.h), (pw, CD), "the pill IS the measured frame");
            assert!(!pill.contains(MARGIN_X - 1.0, y + CD * 0.5), "the left margin is not the pill");

            let mut prev = pill;
            for i in (BTN_PLAY + 1)..hero_btns() {
                let r = rect(i);
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
            // a watch-state disc is always the row's LAST control — the tail shifts, it is never
            // displaced by what appears before it
            assert_eq!(rect(hero_btns() - 1).x, prev.x, "a watch disc ends the row");
            assert!(matches!(
                hero_action(hero_btns() - 1),
                HeroAction::MarkWatched | HeroAction::MarkUnwatched
            ));
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

    /// …and the OTHER half of that refresh, which the blocking version never needed: a fresh detail
    /// fetch always comes back on season 0, so the season the user was browsing has to be put back
    /// once the re-read LANDS. [`REFRESH`] is what remembers it, [`pump_refresh`] is what applies it,
    /// and every way it can fail to apply must end in the page keeping what it already had.
    ///
    /// NB the successful arm below spawns a `/children` worker (`metadata::load_season`) that finds
    /// no client and returns at once; the season mailbox's monotone guard is what keeps its landing
    /// from reaching any later test, and `load_season_now` settles the spinner before this returns.
    #[test]
    fn a_landed_view_state_refresh_puts_the_browsed_season_back_and_never_steers_another_page() {
        let _serial = crate::testlock::serial();
        let show = |cur_season| Detail {
            sid: crate::plex::ServerId::UNSET,
            rk: "s1".into(),
            is_show: true,
            cur_season,
            seasons: vec![
                Season { rk: "sk1".into(), index: 1, title: "S1".into(), leaf_count: 0, viewed_leaf_count: 0 },
                Season { rk: "sk2".into(), index: 2, title: "S2".into(), leaf_count: 0, viewed_leaf_count: 0 },
            ],
            episodes: vec![Episode { rk: "e1".into(), ..Default::default() }],
            ..Default::default()
        };
        let arm = |rk: &str, season, keep: &str| unsafe {
            *addr_of_mut!(REFRESH) = Some(Refresh {
                sid: crate::plex::ServerId::UNSET,
                rk: rk.to_string(),
                season,
                keep_ep: keep.to_string(),
            })
        };

        // a re-read that landed leaves the page on season 0 — the latch puts it back on season 1,
        // and arms the filmstrip to land on the episode whose state actually changed
        metadata::clear(); // settles `detail_loading()`: the latch only fires once the re-read is in
        mount(Some(show(0)), 0);
        arm("s1", 1, "e1");
        pump_refresh();
        assert_eq!(metadata::current().unwrap().cur_season, 1, "back on the season the user was browsing");
        assert_eq!(unsafe { (*addr_of!(KEEP_EP)).clone() }, Some(("e1".to_string(), 1)));
        assert!(unsafe { (*addr_of!(REFRESH)).is_none() }, "…and the latch is one-shot");

        // a FAILED re-read keeps the item AND its season, so there is nothing to put back and no
        // second `/children` to spend on saying so
        metadata::load_season_now(0); // settle the season mailbox the arm above kicked
        unsafe { *addr_of_mut!(KEEP_EP) = None };
        mount(Some(show(1)), 0);
        arm("s1", 1, "e1");
        pump_refresh();
        assert_eq!(metadata::current().unwrap().cur_season, 1);
        assert!(unsafe { (*addr_of!(KEEP_EP)).is_none() }, "no keep-focus latch is armed for a fetch nobody made");

        // the user walked to ANOTHER item while the write was out: the latch is consumed and steers
        // nothing — its season index would otherwise name a season of a show it knows nothing about
        mount(Some(show(0)), 0);
        arm("other-show", 1, "e1");
        pump_refresh();
        assert_eq!(metadata::current().unwrap().cur_season, 0, "the page on screen is left alone");
        assert!(unsafe { (*addr_of!(REFRESH)).is_none() }, "and the latch is consumed all the same");

        // …and a page that closed under the refresh is not a page to steer either
        mount(None, 0);
        arm("s1", 1, "e1");
        pump_refresh();
        assert!(unsafe { (*addr_of!(REFRESH)).is_none() });

        unsafe { *addr_of_mut!(KEEP_EP) = None };
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

    /// …and the same episode asked as the CONTEXT MENU's question. `ep_watch_state` is the only
    /// input to `item_menu::build_episode`'s row set — one write row at either end of the range,
    /// BOTH in the middle — so what it answers is literally which rows a hold on this still puts on
    /// screen. Untested, the projection is exactly the kind of precedence that regresses silently:
    /// it reads `progress` FIRST, so a finished-then-restarted episode reads as in the middle, and
    /// an offset the server never cleared does not.
    ///
    /// It is a projection of [`ep_state`] and graded against it here rather than against a second
    /// predicate, because the whole reason it exists is that the still's own state line and the menu
    /// opened on that still cannot describe one episode two ways.
    #[test]
    fn an_episode_resolves_the_same_three_states_for_the_menu_opened_on_it() {
        let ep = |dur_ms, resume_ms| Episode { dur_ms, resume_ms, ..Default::default() };
        let watched = |mut e: Episode| {
            e.watched = true;
            e
        };

        // the three ordinary states, each the state the still's own glyph/bar already says
        assert_eq!(ep_watch_state(&ep(48 * 60_000, 0)), PosterMark::None, "▶ = one row, the way forward");
        assert_eq!(ep_watch_state(&watched(ep(48 * 60_000, 0))), PosterMark::Watched, "✓ = one row, the way back");
        assert_eq!(
            ep_watch_state(&ep(60 * 60_000, 15 * 60_000)),
            PosterMark::InProgress,
            "the bar = neither end, so the menu offers BOTH"
        );

        // PRECEDENCE, the same one the glyph keeps: PMS reports watched AND a live offset on a
        // re-watch, and being part-way through it is what the viewer is doing — so it is the pair,
        // not the single "Mark as Unwatched" the old `ep.watched` bool would have given.
        assert_eq!(ep_watch_state(&watched(ep(60 * 60_000, 6 * 60_000))), PosterMark::InProgress);

        // …but an offset AT or PAST the runtime is a finished episode, not one in the middle — the
        // resume-point edge this page keeps once, inherited rather than restated
        assert_eq!(ep_watch_state(&watched(ep(60 * 60_000, 60 * 60_000))), PosterMark::Watched);
        assert_eq!(ep_watch_state(&ep(0, 30 * 60_000)), PosterMark::None, "no runtime, no middle");

        // and every answer is the projection of the line the tile is drawing, on every one of them
        for e in [
            ep(48 * 60_000, 0),
            watched(ep(48 * 60_000, 0)),
            ep(60 * 60_000, 15 * 60_000),
            watched(ep(60 * 60_000, 6 * 60_000)),
            watched(ep(60 * 60_000, 60 * 60_000)),
            ep(0, 30 * 60_000),
        ] {
            let st = ep_state(&e);
            let want = if st.progress.is_some() {
                PosterMark::InProgress
            } else if st.glyph == EpGlyph::Watched {
                PosterMark::Watched
            } else {
                PosterMark::None
            };
            assert_eq!(ep_watch_state(&e), want, "the menu must not answer a second time");
        }
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

    // ── the episode page's hero art, and the filmstrip's second focus row ───────────────────────

    /// An EPISODE has a page of its own — the home shelf's context menu offers "Go to Episode", and
    /// the filmstrip's metadata block opens the same one — and that page must lead with the EPISODE's
    /// still, not the show's backdrop.
    ///
    /// It painted the show's for one structural reason: an episode is a LEAF, so it has no `OnDeck`,
    /// so the show rung is `None` and the ladder fell straight through to `art` — which PMS fills on an
    /// episode with the SERIES' backdrop. The `thumb` beside it is the leaf's own still, and that is
    /// the rung this pins.
    #[test]
    fn an_episode_page_leads_with_the_episodes_own_still() {
        let _serial = crate::testlock::serial();
        let episode_page = || Detail {
            rk: "1859".into(),
            kind: "episode".into(),
            thumb: "/library/metadata/1859/thumb/178".into(), // the episode's own still
            art: "/library/metadata/1801/art/177".into(),     // …inherited from its SHOW
            ..Default::default()
        };

        mount(Some(episode_page()), 0);
        assert_eq!(
            hero_art_path(None),
            "/library/metadata/1859/thumb/178",
            "the episode page must lead with its own still, not the series backdrop it inherits"
        );

        // A CATALOG ROW does not override it, and could not help if it did: a row's `thumb` for an
        // episode is the SHOW's portrait poster (`pms::parse_item` prefers `grandparentThumb` so a
        // landscape still does not fill a portrait card), and its `art` is the show's backdrop.
        let row = crate::pms::PmsMovie {
            kind: 3,
            art: "/row/show-art".into(),
            thumb: "/row/show-poster".into(),
            ..Default::default()
        };
        assert_eq!(hero_art_path(Some(&row)), "/library/metadata/1859/thumb/178");

        // …but before the leaf's own metadata lands there is no still to prefer, and the row's ART is
        // the honest stand-in — never its portrait poster.
        mount(None, 0);
        assert_eq!(hero_art_path(Some(&row)), "/row/show-art", "the row's ART, never its show poster");

        // an episode whose agent supplied no still falls back the same way rather than drawing nothing
        let mut no_still = episode_page();
        no_still.thumb.clear();
        mount(Some(no_still), 0);
        assert_eq!(hero_art_path(None), "/library/metadata/1801/art/177");

        // a MOVIE is not an episode: its `thumb` is a portrait poster, so the ladder must not reach it
        let mut film = episode_page();
        film.kind = "movie".into();
        mount(Some(film), 0);
        assert_eq!(hero_art_path(None), "/library/metadata/1801/art/177", "a movie leads with its art");

        // and nothing at all is the EMPTY key, so `draw_backdrop` skips the layer instead of building
        // a transcode request for ""
        mount(Some(Detail { rk: "x".into(), ..Default::default() }), 0);
        assert!(hero_art_path(None).is_empty());

        mount(None, 0);
    }

    /// A SHOW's hero still outranks everything: it is about ONE episode (`hero_episode`), and the
    /// episode rung added for the leaf page must not have quietly displaced it.
    #[test]
    fn a_shows_hero_still_outranks_every_other_art() {
        let _serial = crate::testlock::serial();
        let mut d = show(600_000, 2_700_000); // started: its on-deck episode is part-watched
        d.art = "/show/art".into();
        d.thumb = "/show/poster".into();
        d.on_deck = Some(Episode { rk: "e1".into(), thumb: "/ep/still".into(), resume_ms: 600_000, ..Default::default() });
        mount(Some(d), 0);
        assert_eq!(hero_art_path(None), "/ep/still", "the show hero draws the episode it is about");
        mount(None, 0);
    }

    /// A filmstrip cell is TWO focus stops — the still and the metadata block under it — and the pair
    /// must be exact inverses under UP/DOWN, in both directions, or the strip becomes a place you can
    /// walk into and not walk back out of the same way.
    ///
    /// The horizontal keys are the other half of the contract: LEFT/RIGHT stay on the row you are on
    /// (they move `col`, which BOTH rows share, so the h-scroll follows focus for free) and never step
    /// outside the episode array.
    #[test]
    fn the_filmstrips_still_and_its_text_pair_up_and_down() {
        let _serial = crate::testlock::serial();
        let ep = |n: i64| Episode { rk: format!("e{n}"), index: n, ..Default::default() };
        let mut d = Detail {
            rk: "s1".into(),
            is_show: true,
            episodes: vec![ep(1), ep(2), ep(3)],
            ..Default::default()
        };
        d.seasons = vec![metadata::Season { rk: String::new(), index: 1, title: String::new(), leaf_count: 3, viewed_leaf_count: 0 }];
        mount(Some(d), 0);
        // the block below the filmstrip that DOWN must eventually reach (About is always present)
        let (s, n) = sections();
        assert_eq!(&s[..n], &[0, 1, 2, 5], "the fixture this test means to walk");

        let at = || (view().section, view().col, view().ep_row);
        view().section = 1; // the season tabs, one press above the strip

        crate::ui::detail::move_focus(SDLK_DOWN as c_int);
        assert_eq!(at(), (2, 0, EpRow::Still), "DOWN from the tabs lands on the still");
        crate::ui::detail::move_focus(SDLK_DOWN as c_int);
        assert_eq!(at(), (2, 0, EpRow::Text), "…and DOWN again on ITS text, not on the next section");
        crate::ui::detail::move_focus(SDLK_DOWN as c_int);
        assert_eq!(view().section, 5, "only DOWN from the text leaves the strip");

        crate::ui::detail::move_focus(SDLK_UP as c_int);
        assert_eq!(at(), (2, 0, EpRow::Text), "coming back UP lands on the block nearest — the text");
        crate::ui::detail::move_focus(SDLK_UP as c_int);
        assert_eq!(at(), (2, 0, EpRow::Still), "…then its still");
        crate::ui::detail::move_focus(SDLK_UP as c_int);
        assert_eq!(view().section, 1, "…then out to the tabs, which is where we came in");

        // LEFT/RIGHT keep the sub-row and stay inside the array, from EITHER row
        for row in [EpRow::Still, EpRow::Text] {
            let v = view();
            v.section = 2;
            v.col = 0;
            v.ep_row = row;
            for _ in 0..6 {
                crate::ui::detail::move_focus(SDLK_RIGHT as c_int);
            }
            assert_eq!(at(), (2, 2, row), "RIGHT stops at the last episode and holds the row it is on");
            for _ in 0..6 {
                crate::ui::detail::move_focus(SDLK_LEFT as c_int);
            }
            assert_eq!(at(), (2, 0, row), "…and LEFT stops at the first");
        }

        mount(None, 0);
    }

    /// What OK means on each of the two rows: the still PLAYS, the metadata block NAVIGATES to that
    /// episode's own page. The navigation half is graded on the rk it resolves — the act itself
    /// (`open_rk`) is a PMS round trip — and on `focus_is_card`, which is what routes the press: a card
    /// dips and commits on release AND can be held open into the context menu, while the text row
    /// commits at once and can never reach that menu.
    #[test]
    fn the_filmstrips_text_row_opens_that_episodes_own_page() {
        let _serial = crate::testlock::serial();
        let ep = |n: i64| Episode { rk: format!("e{n}"), index: n, ..Default::default() };
        mount(
            Some(Detail {
                rk: "s1".into(),
                is_show: true,
                episodes: vec![ep(1), ep(2), ep(3)],
                ..Default::default()
            }),
            0,
        );

        let v = view();
        v.section = 2;
        v.col = 2;
        v.ep_row = EpRow::Still;
        assert_eq!(ep_page_rk(), None, "the still plays; it does not navigate");
        assert!(focus_is_card(), "…and it is a card: press-dip, commit on release, hold for the menu");
        assert!(focused_episode().is_some(), "which is exactly the row the context menu belongs to");

        view().ep_row = EpRow::Text;
        assert_eq!(ep_page_rk().as_deref(), Some("e3"), "the text row opens the FOCUSED episode's page");
        assert!(!focus_is_card(), "…as a link: it commits at once, like a season tab or an About row");
        assert_eq!(focused_episode(), None, "and a hold on it must not open the still's context menu");

        // …and OK on it REPORTS rather than navigating. That is the fix for the owner's bug: the arm
        // used to call `open_rk` itself, so `app.rs` never learned a page had been entered and BACK
        // fell past the show page onto Home.
        assert!(!on_ok(), "a link opens a page; it does not start playback");
        assert_eq!(
            take_open_request().map(|(_, rk)| rk).as_deref(),
            Some("e3"),
            "the text row must raise an open request for the focused episode"
        );
        assert!(take_open_request().is_none(), "the latch is one-shot");
        assert_eq!(
            metadata::current().map(|d| d.rk.clone()),
            Some("s1".to_string()),
            "the page must still be the show until app.rs performs the navigation"
        );

        // it belongs to the filmstrip alone — no other section can be mistaken for it
        for sec in [0, 1, 3, 4, 5] {
            view().section = sec;
            assert_eq!(ep_page_rk(), None, "section {sec} has no episode text row");
        }
        // an episode PMS sent no ratingKey for is not a destination (`open_rk("")` would fetch nothing
        // and land on a blank page — the same belt `app.rs`'s item-menu dispatch wears)
        let v = view();
        v.section = 2;
        v.ep_row = EpRow::Text;
        v.col = 0;
        metadata::set_current_for_test(Some(Detail {
            rk: "s1".into(),
            is_show: true,
            episodes: vec![Episode::default()],
            ..Default::default()
        }));
        assert_eq!(ep_page_rk(), None);

        take_open_request(); // nothing above raised one; drain anyway so no test inherits a latch
        mount(None, 0);
    }

    /// The Related shelf is the OTHER way a detail page opens a detail page, and it must go through
    /// the SAME latch — one navigation into an item page, not two. (This is also the arm that makes
    /// the trail more than one deep in practice: PMS's related hubs are near-symmetric, so A→B→A is
    /// two presses.)
    #[test]
    fn the_related_shelf_raises_the_same_open_request() {
        let _serial = crate::testlock::serial();
        let rel =
            |rk: &str| crate::metadata::Related { rk: rk.into(), ..Default::default() };
        mount(
            Some(Detail { rk: "m1".into(), related: vec![rel("r0"), rel("r1")], ..Default::default() }),
            0,
        );
        let v = view();
        v.section = 3;
        v.col = 1;

        assert!(!on_ok());
        assert_eq!(
            take_open_request().map(|(_, rk)| rk).as_deref(),
            Some("r1"),
            "Related must raise an open request"
        );
        assert!(take_open_request().is_none(), "one press, one request");

        // a Related row PMS sent no ratingKey for is not a destination — the same belt the
        // filmstrip's text row wears
        metadata::set_current_for_test(Some(Detail {
            rk: "m1".into(),
            related: vec![rel("")],
            ..Default::default()
        }));
        view().col = 0;
        assert!(!on_ok());
        assert!(take_open_request().is_none(), "an empty rk would fetch nothing and draw a blank page");

        mount(None, 0);
    }

    /// A raised request belongs to the page that raised it. `app.rs` drains `OPEN_REQ` once a frame
    /// whatever the route, so one that survives its page fires on an unrelated OK several screens
    /// later — the incident `person.rs`'s `requested` latch records, ported to this one.
    #[test]
    fn an_open_request_does_not_outlive_its_page() {
        let _serial = crate::testlock::serial();
        let rel =
            |rk: &str| crate::metadata::Related { rk: rk.into(), ..Default::default() };
        mount(Some(Detail { rk: "m1".into(), related: vec![rel("r0")], ..Default::default() }), 0);

        let v = view();
        v.section = 3;
        v.col = 0;
        assert!(!on_ok());
        close();
        assert!(take_open_request().is_none(), "leaving the page must drop its un-consumed request");

        // …and so must mounting a different one (`open_rk`'s reset), which is the path an ACCEPTED
        // request itself takes a beat later
        metadata::set_current_for_test(Some(Detail {
            rk: "m1".into(),
            related: vec![rel("r0")],
            ..Default::default()
        }));
        let v = view();
        v.section = 3;
        v.col = 0;
        assert!(!on_ok());
        reset_view_state(view());
        assert!(take_open_request().is_none(), "a new page must not inherit the old one's request");

        mount(None, 0);
    }

    /// A `Spot` must round-trip through the page it describes: snapshot it, move the focus somewhere
    /// else, and the landing puts it back — for every section the flow can hold and both filmstrip
    /// sub-rows. Graded on `spot`/`spot_landing`, the pure halves; the scroll jump `pump_pending`
    /// performs measures the flow through SDL_ttf, which the host suite cannot link.
    #[test]
    fn a_spot_round_trips_through_the_page_it_describes() {
        let _serial = crate::testlock::serial();
        let ep = |n: i64| Episode { rk: format!("e{n}"), index: n, ..Default::default() };
        let rel =
            |rk: &str| crate::metadata::Related { rk: rk.into(), ..Default::default() };
        let season = |i: i64| crate::metadata::Season {
            rk: format!("s{i}"),
            index: i,
            title: format!("Season {i}"),
            leaf_count: 3,
            viewed_leaf_count: 0,
        };
        let show = || Detail {
            rk: "s1".into(),
            is_show: true,
            seasons: vec![season(1), season(2)],
            cur_season: 1,
            episodes: vec![ep(1), ep(2), ep(3)],
            related: vec![rel("r0"), rel("r1")],
            ..Default::default()
        };
        mount(Some(show()), 0);

        for (sec, col) in [(0, 0), (1, 1), (2, 2), (3, 1), (5, 2)] {
            for ep_text in [false, true] {
                let v = view();
                v.section = sec;
                v.col = col;
                v.ep_row = if ep_text { EpRow::Text } else { EpRow::Still };
                v.saved_col = [0, 1, col, 1, 0, 2];
                let s = spot();
                assert_eq!(s.season, Some(2), "the season NUMBER, not `cur_season`'s position");
                // The snapshot IS the page as it stands — the fact `app.rs`'s `leaving_spot` reads
                // on the PRESS frame to fill a navigation's `NavReq::spot`. It used to be asserted
                // from the filmstrip's and Related's own OK tests, which read a second copy the
                // `OPEN_REQ` latch carried and production threw away; the latch is an rk alone now,
                // so this is the one place the fact is graded, for every section rather than two.
                assert_eq!(
                    (s.section, s.col, s.ep_text),
                    (sec, col, ep_text),
                    "the snapshot must name the section, item and sub-row the page is on"
                );

                // move somewhere else entirely, then land the spot back
                let v = view();
                v.section = 0;
                v.col = 0;
                v.ep_row = EpRow::Still;
                v.saved_col = [0; 6];
                let (lsec, lcol, lrow) = spot_landing(&s);
                let v = view();
                v.saved_col = s.saved_col;
                v.section = lsec;
                v.col = lcol;
                v.ep_row = lrow;
                assert_eq!((v.section, v.col), (sec, col), "section {sec} col {col} must come back");
                assert_eq!(v.ep_row == EpRow::Text, ep_text, "…and so must the filmstrip's sub-row");
                assert_eq!(v.saved_col, [0, 1, col, 1, 0, 2], "…and the per-section focus memory");
            }
        }
        mount(None, 0);
    }

    /// The item can come back SHORTER than it was left — a season with fewer episodes, a Related
    /// shelf the server no longer sends. A restore must clamp onto what is there rather than focus a
    /// slot nothing draws, and a section that has left the flow entirely falls back to the hero.
    #[test]
    fn a_restored_spot_clamps_onto_an_item_whose_lists_shrank() {
        let _serial = crate::testlock::serial();
        let rel =
            |rk: &str| crate::metadata::Related { rk: rk.into(), ..Default::default() };
        let s = Spot { section: 3, col: 5, ..Default::default() };

        metadata::set_current_for_test(Some(Detail {
            rk: "m1".into(),
            related: vec![rel("r0"), rel("r1")],
            ..Default::default()
        }));
        assert_eq!(spot_landing(&s), (3, 1, EpRow::Still), "col clamps to the last item present");

        metadata::set_current_for_test(Some(Detail { rk: "m1".into(), ..Default::default() }));
        assert_eq!(
            spot_landing(&s),
            (0, 0, EpRow::Still),
            "with no Related shelf the section is not in the flow at all — fall back to the hero"
        );

        // and the degenerate case: nothing loaded at all, so the flow is the bare hero. The landing
        // must still be a focus the page can hold — never negative, never past the row.
        metadata::set_current_for_test(None);
        let (sec, col, _) = spot_landing(&Spot { section: 0, col: 4, ..Default::default() });
        assert_eq!(sec, 0);
        assert!((0..n_items(0).max(1)).contains(&col), "a landing must be a focus the page can hold");
        mount(None, 0);
    }

    /// The trap `Pending::Episode`'s `if seasons.is_empty() { return }` gate would spring if it were
    /// shared: a MOVIE has no seasons and never will, so a restore keyed off that gate would wait
    /// forever and the page would sit at the hero. A spot with no season places immediately; one
    /// whose season is already loaded places immediately; one whose is not asks for it and waits.
    #[test]
    fn a_movie_spot_does_not_wait_for_a_season_that_will_never_land() {
        let _serial = crate::testlock::serial();
        let season = |i: i64| crate::metadata::Season {
            rk: format!("s{i}"),
            index: i,
            title: String::new(),
            leaf_count: 0,
            viewed_leaf_count: 0,
        };
        let movie = Detail { rk: "m1".into(), ..Default::default() };
        assert_eq!(
            spot_season_gate(&movie, &Spot { section: 3, col: 0, ..Default::default() }),
            None,
            "a movie's restore must never wait on a season list"
        );

        let show = Detail {
            rk: "s1".into(),
            is_show: true,
            seasons: vec![season(1), season(2), season(3)],
            cur_season: 2,
            ..Default::default()
        };
        assert_eq!(spot_season_gate(&show, &Spot { season: Some(3), ..Default::default() }), None, "already loaded");
        assert_eq!(
            spot_season_gate(&show, &Spot { season: Some(1), ..Default::default() }),
            Some(0),
            "a different season is asked for by POSITION, resolved from the number the user saw"
        );
        assert_eq!(
            spot_season_gate(&show, &Spot { season: Some(9), ..Default::default() }),
            None,
            "a season the item no longer has must place, not spin"
        );
        mount(None, 0);
    }

    /// The text row's focus panel must fit INSIDE the band the flow already reserves for the block, or
    /// `block_h(2)` would have to grow and every section below the filmstrip would move down when a
    /// highlight appeared. It also must not collide with the still above it at any pop phase, and two
    /// neighbouring cells' panels must not touch.
    ///
    /// Pure arithmetic on the constants: `block_h(2)`'s own measure sweep (`ep_meta_h`) wraps text
    /// through SDL_ttf, which the host suite cannot link — so the block height is reconstructed here
    /// from the same formula rather than called.
    #[test]
    fn the_episode_text_highlight_fits_the_block_the_flow_already_reserves() {
        let ep_y = 0.0;
        // the tallest episode's measured meta, and a short one sharing the block with it
        for &maxh in &[120.0f32, 402.0] {
            let block_h = EP_H + EP_META_TOP + maxh + EP_META_BOT_PAD; // == block_h(2)
            for &total in &[40.0f32, maxh * 0.5, maxh] {
                let hl = ep_text_hl(300.0, ep_y, total);
                assert!(hl.y > ep_y + EP_H, "the panel must start clear of the still ({} vs {EP_H})", hl.y);
                assert!(
                    hl.y + hl.h <= ep_y + block_h + 0.01,
                    "the panel ({}) must fit the block the flow reserves ({block_h}) — nothing below may move",
                    hl.y + hl.h
                );
                // the still at full focus pop is still above it (it grows about its centre)
                let popped = Rect::new(300.0, ep_y, EP_W, EP_H).scaled(crate::ui::widgets::CARD_FOCUS_SCALE);
                assert!(popped.y + popped.h < hl.y, "a popped still must not reach into the panel");
                // and the panel wraps THIS episode's ink, not the row's tallest
                assert_eq!(hl.h, total + 2.0 * EP_TEXT_HL_PAD_Y, "it hugs the content it is lit for");
            }
            // neighbouring cells: one pitch apart, panels must not meet (the pad is SHARED between
            // them, so the gutter buys `EP_GAP - 2 * pad` and anything from 14 up would collide)
            let a = ep_text_hl(strip_x(0, EP_W + EP_GAP), ep_y, maxh);
            let b = ep_text_hl(strip_x(1, EP_W + EP_GAP), ep_y, maxh);
            assert!(a.x + a.w < b.x, "two lit panels would overlap at this horizontal pad");
            assert!(a.x < strip_x(0, EP_W + EP_GAP), "…and the panel does wrap its text, not abut it");
        }
    }

    /// The pointer's sub-row split must name the row the painter DREW at that y — the detail page's
    /// standing rule that a control is never clicked somewhere other than where it is drawn.
    #[test]
    fn the_pointer_lands_on_the_filmstrip_row_that_is_drawn_at_that_y() {
        // everything on the still, including its very bottom edge, is the still
        for dy in [0.0f32, 1.0, EP_H * 0.5, EP_H] {
            assert_eq!(ep_row_at(dy), EpRow::Still, "y={dy} is drawn inside the still");
        }
        // …and every y inside the panel the text row lights is the text row, at every content height
        for &total in &[40.0f32, 402.0] {
            let hl = ep_text_hl(0.0, 0.0, total);
            for dy in [hl.y, hl.y + hl.h * 0.5, hl.y + hl.h] {
                assert_eq!(ep_row_at(dy), EpRow::Text, "y={dy} is drawn inside the text panel");
            }
        }
        // the dead air between them belongs to the still above it — no band answers for neither row
        assert_eq!(ep_row_at(EP_H + 1.0), EpRow::Text);
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

/// The (section, item, filmstrip sub-row) under the pointer, or `None` for a miss. Checked in draw
/// order — the pinned hero first, then each present below-hero block against its own strip pitch. The
/// About footer (5) is deliberately absent: its four "items" are a read-only card and three text
/// columns whose activation is a no-op, so a click there is a miss rather than a focus jump into a
/// dead end.
///
/// The third element is only meaningful for section 2 (see [`EpRow`]); every other section answers
/// [`EpRow::Still`], the value [`focus_to`] would leave the field at anyway.
fn hit_at(mx: f32, my: f32) -> Option<(c_int, c_int, EpRow)> {
    let scroll = view().column.scroll.pos;
    // hero action row: it scrolls with the hero, and it stops answering while the hero fades out.
    // The threshold is FIRMER than the paint's `> 0.01` on purpose — a control faded to a percent
    // of a percent is not something the user can see, and clicking apparently-empty space must not
    // start playback. Only reachable mid-transition anyway (the scroll rests at 0 or well past the
    // fade), which is exactly when a stray click is least intended.
    if hero_alpha(scroll, HERO_FADE) > HERO_HIT_MIN_A {
        let y = hero_layout(selected()).btn_y - scroll;
        // the LIVE control set, not a constant: the row runs from TWO controls to five, and this
        // walks the same accumulation the painter did — off the same once-resolved set and widths
        // (`draw_buttons`) — so the watched toggle at the end is clicked wherever this set
        // happens to put them, and an UNFURLED one is clicked across the whole capsule it drew.
        let set = hero_set();
        let cw = hero_widths();
        for b in 0..(hero_ctls(set).1 as c_int) {
            if hero_btn_rect_at(set, b, y, cw).contains(mx, my) {
                return Some((0, b, EpRow::Still));
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
                return Some((1, lay.i as c_int, EpRow::Still));
            }
        }
    }
    // Episode filmstrip: the whole block (still + its under-card metadata) belongs to that episode, so
    // clicking the title or the blurb picks the same one as clicking the still — but WHICH of the cell's
    // two focus rows it lands on now follows the y. The split is the still's own bottom edge, so the
    // dead air between the still and the text block belongs to the still above it and there is no band
    // the pointer can sit in that answers for neither. The horizontal span stays `EP_W` for both rows:
    // that is the text COLUMN the metadata wraps to, and the focus panel's 16px bleed either side is
    // air, not content (a click there would be a click on the neighbouring gutter).
    if let Some(top) = section_screen_top(2) {
        if my >= top && my <= top + block_h(2) {
            let n = n_items(2).max(0) as usize;
            if let Some(i) = strip_at(mx, view().ep_hscroll.pos, n, EP_W + EP_GAP, EP_W) {
                return Some((2, i as c_int, ep_row_at(my - top)));
            }
        }
    }
    // Related / Cast: the two shared `CardRow` strips, each below its heading
    if let Some(top) = section_screen_top(3) {
        let row_y = top + REL_LABEL_H;
        if my >= row_y && my <= row_y + REL_H {
            let n = n_items(3).max(0) as usize;
            if let Some(i) = strip_at(mx, view().related.scroll_x(), n, REL_W + REL_GAP, REL_W) {
                return Some((3, i as c_int, EpRow::Still));
            }
        }
    }
    if let Some(top) = section_screen_top(4) {
        let row_y = top + CAST_LABEL_H;
        if my >= row_y && my <= row_y + CAST_D {
            let n = n_items(4).max(0) as usize;
            if let Some(i) = strip_at(mx, view().cast.scroll_x(), n, CAST_SLOT, CAST_D) {
                return Some((4, i as c_int, EpRow::Still));
            }
        }
    }
    None
}

/// Land focus on (`sec`, `col`, `row`) exactly the way [`move_focus`] would: remember the strip we are
/// leaving (so returning to it restores the item), re-pop the newly focused card, and queue a
/// debounced season load when the focus lands on a tab. Reports whether focus actually MOVED — the
/// caller aborts an armed click on a move, since the press belongs to the control it started on.
///
/// `row` is the filmstrip sub-row [`hit_at`] resolved from the pointer's y; it is written
/// unconditionally, so leaving section 2 leaves the field on the [`EpRow::Still`] every other section
/// answers with, and the state is never a stale sub-row of a strip that no longer holds focus.
fn focus_to(sec: c_int, col: c_int, row: EpRow) -> bool {
    let v = view();
    if v.section == sec && v.col == col && v.ep_row == row {
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
    v.ep_row = row;
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
    // an open panel owns the pointer too: hover walks ITS rows, and the page underneath must not
    // re-focus (or scroll) under a modal the user is reading
    if crate::ui::alt_sources::is_open() {
        crate::ui::guard(|| crate::ui::alt_sources::pointer_focus(mx, my));
        return false;
    }
    // Neither ALERT has rows for a hover to walk, so both only take the pointer AWAY from the
    // page — the same reason their `move_focus` arms swallow or page the D-pad. (Track information
    // shipped without this half: its lane guarded the key paths and left `pointer_focus`/`click`
    // falling through to `hit_at`, so a click behind the open sheet re-focused — and launched — a
    // card under a modal. The About lane guarded both, which is what made the gap visible when the
    // two were merged.)
    if crate::ui::about_panel::is_open() || crate::ui::tracks_panel::is_open() {
        return false;
    }
    let mut moved = false;
    crate::ui::guard(|| {
        if let Some((sec, col, row)) = hit_at(mx, my) {
            if hover_allows(sec) {
                moved = focus_to(sec, col, row);
            }
        }
    });
    moved
}

/// Pointer click: focus what was clicked and report the hit, so `app.rs` can run the SAME activation
/// as OK — the tvOS press dip for a card, immediate for the Play pill / watched disc / season tab.
pub(crate) fn click(mx: f32, my: f32) -> bool {
    // A click while the panel is up commits its row or dismisses it, and is spent EITHER WAY:
    // reporting a hit would send `app.rs` on to `on_ok`, which would act a second time.
    if crate::ui::alt_sources::is_open() {
        crate::ui::guard(|| request_alt_open(crate::ui::alt_sources::click(mx, my)));
        return false;
    }
    // Same rule for both ALERTS, which have nothing to commit — and each answers the pointer the
    // way it answers OK, so the remote and the mouse cannot disagree about one panel. About
    // dismisses (its `on_ok` does); Track information SWALLOWS, because BACK is the only dismissal
    // its footer names and its `on_ok` returns without closing. Either way the press is spent here
    // rather than falling through to the page's own hit test.
    if crate::ui::about_panel::is_open() {
        crate::ui::guard(|| crate::ui::about_panel::click(mx, my));
        return false;
    }
    if crate::ui::tracks_panel::is_open() {
        return false;
    }
    let mut hit = false;
    crate::ui::guard(|| {
        if let Some((sec, col, row)) = hit_at(mx, my) {
            focus_to(sec, col, row);
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
