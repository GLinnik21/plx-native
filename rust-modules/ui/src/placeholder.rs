//! **Placeholder accounting: every placeholder a frame SHOWS is counted, and in the sentinel build
//! painted in one colour nothing else uses.** (S4 of the demo-video pipeline.)
//!
//! A placeholder is the app saying "this is not here yet": a skeleton where a poster has not
//! arrived, a ground where a still has not, a spinner, a "Loading" read-out, a hero title typed out
//! because the clearLogo has not landed, an episode strip dimmed while its season reloads. The
//! demo video must contain none of them, and two independent oracles check that:
//!
//! * **The counter (this module).** [`note`] is called at the place a placeholder is *drawn*, and
//!   [`Frame`] reports what a frame drew: that there was one, why ([`Reason`]) and for which key.
//!   A page that is shown as a held IMAGE draws nothing, so the debt the image carries is re-noted
//!   on every frame it is painted (see "A held page image"). It fails when a site nobody hooked
//!   draws one.
//! * **The pixel sentinel (feature `placeholder-sentinel`).** The tokens [`theme::SKELETON_TOP`],
//!   [`theme::SKELETON_BOT`] and [`theme::CARD_PLACEHOLDER`] become [`theme::PLACEHOLDER_SENTINEL`]
//!   (#FF00FE), and every placeholder paint — through [`painter`] — ignores the painter's alpha
//!   and RGB-dim cascade (`STALE_ALPHA`, a route fade) and the sheen, so it lands as exactly that
//!   value. It fails when a placeholder is drawn in a colour nobody overrode, which is the one
//!   thing the counter cannot see. Blending stays ON (the renderer has one program), and is exact
//!   because the source alpha is 1: the compile-time `assert!` on [`sentinel_alpha_is_one`]
//!   and the test `the_sentinel_is_opaque_and_matches_its_byte_code` hold that line. The feature is
//!   enabled by the dump build alone and is absent from every shipping and television build
//!   (`make check` compiles it, code and tests, so it keeps building).
//!
//! **Where the call goes — the one rule.** At the DRAW, never at a resolve. `HomeScreen::prefetch`
//! resolves the neighbouring heroes' backdrops and draws none of them; a count there would hold
//! debt on art that is not on screen (`Backdrop::bind` only records `pending_art` for the hero it
//! draws, and `Backdrop::draw` does the counting). A recording pass (`Painter::is_recording`, a text
//! prewarm or a `record_walk`) draws nothing, so [`note`] counts nothing in one.
//!
//! **Zero or non-zero, never "how many".** The page closure runs once as a discovery walk, once per
//! blur source job and once visibly (`src/app/run.rs`), and [`note`] ticks in each, so one skeleton
//! reads 2 or more and [`ENTRY_CAP`] fills that much sooner. Only `count == 0` against `count > 0`
//! is meaningful; print [`Entry`] reasons deduplicated.
//!
//! **Absence is not a wait.** A person with no headshot, a collection with no artwork of its own,
//! a hero whose clearLogo 404ed: nothing will ever arrive, the tile is correct, and it is drawn in
//! [`theme::CARD_ABSENT`] (or, for text, as plain ink) — neither counted nor painted in the
//! sentinel. A hero logo that SETTLED as a miss is reported in [`Frame::absent`]
//! ([`Reason::HeroLogoAbsent`], [`note_absent`]) so a storyboard that needs the logo can fail fast on
//! it; the count only moves while the logo is still on its way. A key of `""` never resolves at all
//! (`Art::Thumb { key: "" }`, an empty `still_key`, an empty `thumb` on a non-collection poster): it
//! IS counted, because it is a card with no picture, and it will never clear — a driver must fail
//! fast on an entry whose key is empty instead of holding for it.
//!
//! **A held page image.** During a `PageDip` route push the destination is drawn live ONCE into a
//! snapshot and then painted as that image — nothing walks the page — on every frame until it is
//! quiescent (up to `PAGE_QUIESCENCE_HOLD_MAX_MS` after the dip), when one replacement capture draws
//! it live again. The dispatcher records what the capture drew ([`mark`], [`since`] → [`Debt`]) and
//! re-notes it ([`renote`]) on every frame the image stands in for the page, so a spinner, a
//! "Loading" title or a skeleton captured into the image keeps the count above zero for as long as
//! it is on screen. `Dispatcher::held_page_image` says whether the page shown is an image and at
//! what alpha. The sentinel does NOT survive an image: it is blended at the dip alpha, and it never
//! carried a caption, so the pixel oracle cannot see a placeholder inside an image and the counter
//! alone vouches for those frames.
//!
//! **What the dump driver (S5b) must do.** Call [`arm`] once at start; then for each frame
//! [`reset`] BEFORE the frame's first walk, draw it, and [`take`] after the last walk, on the UI
//! thread (the counter is per thread). That is the contract, in full:
//!
//! 1. **Force a live draw on every written frame.** A frame the app did not draw reads count 0.
//! 2. **A held page image is a fact, not debt.** The image carries its capture's placeholders,
//!    re-noted by [`renote`], so a spinner captured into it still holds the frame through the
//!    counter. The driver does not hold on `Dispatcher::held_page_image` itself: it re-captures on
//!    each repeat until the count is 0 (`plx_gfx::dump::held_repeat`), then writes the image
//!    frames, so a push films as the product plays it. Keep a negative test that delays
//!    `/library/metadata/<rk>` by 3 s across a push and asserts no written frame contains the
//!    spinner or the "Loading" title.
//! 3. **Zero or non-zero only** (see above); a frame with `count > 0` is a failure naming
//!    `entries`, deduplicated.
//! 4. **Reveal springs start after arrival.** Home's art spring starts at 0 when the texture lands,
//!    and `Xfade` In ramps and the dip alpha ramp likewise: the first frame after a hold shows the
//!    bare wash with count 0. Wait for them to settle before writing, or accept them as animation
//!    and say so.
//! 5. **Content that arrives late with no placeholder drawn** — Home shelves and Continue Watching,
//!    Detail's related, cast, extras and ratings, Library shelves and the grid header, `Xfade` Hold
//!    over a search's results — is invisible to both oracles. The landing predicate
//!    (`landgate`) and `expect.complete` must cover them.
//! 6. **Logos and empty keys**: every hero title the storyboard shows must have a clearLogo, and the
//!    driver fails on [`Frame::absent`] and on an entry with an empty key rather than holding.
//! 7. **The scanner counts connected components, not scanline runs**, and compares written RGB to
//!    [`theme::PLACEHOLDER_SENTINEL_RGB8`] before any YUV conversion, to exactly that value. The
//!    spinner is counter-only for practical purposes: its dots are 6.16 px (page) or 3.36 px
//!    (inline) radius with antialiased edges, so a page dot is an ~84-pixel blob and never a 64-pixel
//!    scanline run; only a component scan sees them. COUNTER-ONLY reasons, which no pixel scan can
//!    see and which the run report must list as such: [`Reason::HomeBackdrop`],
//!    [`Reason::DetailBackdrop`], [`Reason::WorkingReadout`] and every other caption,
//!    [`Reason::HeroLogoText`] (thin strokes), anything under a scrim or glass, and a held image.
//! 8. **Build the sentinel in CI** (the `site-video-sim` target) and run the plan's negative tests
//!    against both oracles.
//! 9. **Keep `pin_hero`**: an outgoing hero backdrop with no texture is not counted.
//!
//! **Cost when unarmed (every shipping and television build).** Each hook ([`note`], [`mark`],
//! [`since`], [`renote`], [`note_absent`]) is one relaxed atomic load of a process-wide armed-thread
//! count and a branch, before any thread-local access; [`note`] and [`renote`] are `#[inline]` and
//! keep the recording half out of line. [`painter`], [`sentinel_fill`] and [`stale_cover`] compile
//! to nothing outside the sentinel build, and [`ink`], [`skeleton`], [`ground`], [`flat`] and
//! [`face`] draw the very same primitive with the very same arguments.
//!
//! **What the sentinel cannot see**, so the counter owns it alone: a hub or detail BACKDROP that
//! has not arrived (the bare ambient wash under a translucent scrim ramp cannot be an exact
//! colour), a "Loading" caption and a hero-logo fallback (text in its own ink), a spinner under a
//! naive scanline run, and a held image.
//!
//! The call sites are closed by a rule, not by memory: `ci/check-placeholders.py` greps every
//! reference to `SKELETON_TOP`, `SKELETON_BOT`, `CARD_PLACEHOLDER`, `CARD_ABSENT`, `STALE_ALPHA`,
//! `Spinner::new`/`leading`/`Spinner { .. }`, the `SKEL_*` alphas and the `*_loading` messages and
//! requires a call into this module INSIDE THE SAME FUNCTION — or a `placeholder-exempt:` comment
//! that says why not — and checks that every [`Reason`] is still named by production code. Its
//! docstring lists the sites it cannot see (a new spinner type, a wait caption not named
//! `*_loading`, an uncounted wait that draws nothing).

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::theme;
use crate::{Painter, Rect};

/// Why a placeholder was drawn. One variant per distinct site family; the driver prints
/// [`Reason::name`] with the key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// A poster, still or landscape tile with no texture: the `SKELETON_TOP`/`SKELETON_BOT` sheet
    /// (`widgets::card_named`, the `Poster` and `Still` arms). Includes `Art::Poster(None)`,
    /// `Art::Still(None)` and an empty path, which all resolve to texture 0.
    CardSkeleton,
    /// A `Thumb` or `Person` tile with no texture: the flat `CARD_PLACEHOLDER` ground.
    CardGround,
    /// The playing item's still on the player's info panel (`appkit::info_panel`).
    InfoPanelStill,
    /// The profile chip's avatar whose picture has not arrived (`widgets::profile_chip`).
    ProfileChip,
    /// One line of text that has not arrived (`widgets::skeleton_bar`, the Person page header).
    SkeletonBar,
    /// Artwork that has not arrived, as a swept sheet (`widgets::skeleton_sheet`).
    SkeletonSheet,
    /// The Detail page's spinner while its metadata loads.
    DetailSpinner,
    /// The Detail page's spinner while a season's episodes reload.
    SeasonSpinner,
    /// The Library's standalone loading spinner.
    LibrarySpinner,
    /// A page-sized spinner of a screen outside the content pages (onboarding, sign-in, the profile
    /// picker); the key names which. An inline (`R_INLINE`) spinner marking one line or control is
    /// a busy mark, not a stand-in, and is exempt (`placeholder-exempt:`).
    PageSpinner,
    /// A `StatusKind::Working` read-out (`widgets::StatusOverlay`): Home's hub load, a collection's
    /// load, the Library's, a sign-in wait.
    WorkingReadout,
    /// The Detail hero's title is the "Loading" message because no item has landed yet.
    DetailTitle,
    /// The episode strip drawn dimmed at `STALE_ALPHA` while its season reloads.
    StaleEpisodes,
    /// The hero's title typed out as text while its clearLogo is still on its way
    /// (`hero_logo::HeroLogo`; a logo that has settled as a miss is [`Reason::HeroLogoAbsent`]).
    HeroLogoText,
    /// The hero's title typed out as text because its clearLogo SETTLED as a miss (the 404 of an
    /// item with no logo, or an undecodable body). Never counted: nothing will arrive, so it is
    /// not a wait. Reported in [`Frame::absent`] only, so a driver whose storyboard promises a
    /// logo can fail fast on it instead of reading the frame as clean.
    HeroLogoAbsent,
    /// The Home hero's backdrop art (path non-empty) has no texture yet.
    HomeBackdrop,
    /// The Detail page's backdrop art (path non-empty) has no texture yet.
    DetailBackdrop,
}

impl Reason {
    /// A stable lowercase name for logs and the driver's failure line.
    pub fn name(self) -> &'static str {
        match self {
            Reason::CardSkeleton => "card-skeleton",
            Reason::CardGround => "card-ground",
            Reason::InfoPanelStill => "info-panel-still",
            Reason::ProfileChip => "profile-chip",
            Reason::SkeletonBar => "skeleton-bar",
            Reason::SkeletonSheet => "skeleton-sheet",
            Reason::DetailSpinner => "detail-spinner",
            Reason::SeasonSpinner => "season-spinner",
            Reason::LibrarySpinner => "library-spinner",
            Reason::PageSpinner => "page-spinner",
            Reason::WorkingReadout => "working-readout",
            Reason::DetailTitle => "detail-title",
            Reason::StaleEpisodes => "stale-episodes",
            Reason::HeroLogoText => "hero-logo-text",
            Reason::HeroLogoAbsent => "hero-logo-absent",
            Reason::HomeBackdrop => "home-backdrop",
            Reason::DetailBackdrop => "detail-backdrop",
        }
    }
}

/// One counted placeholder draw.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub reason: Reason,
    /// What it stood in for: an image path, a ratingKey, a caption — whatever the site has. Empty
    /// when the site has nothing better (a `None` tile, a spinner).
    pub key: String,
}

/// What one frame drew.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Frame {
    /// Every placeholder drawn, including those past [`ENTRY_CAP`].
    pub count: u32,
    /// The first [`ENTRY_CAP`] of them, in draw order.
    pub entries: Vec<Entry>,
    /// Draws that show the form of something that will NEVER arrive ([`note_absent`]): not
    /// counted, not a wait. A driver whose storyboard needs the thing fails fast on a non-empty
    /// list; it must not hold for it. Capped at [`ENTRY_CAP`].
    pub absent: Vec<Entry>,
}

impl Frame {
    /// How many of this frame's placeholders were drawn for `reason`.
    pub fn of(&self, reason: Reason) -> usize {
        self.entries.iter().filter(|e| e.reason == reason).count()
    }
}

/// The entries one frame keeps. A frame that hits it is already a failure; the cap bounds the
/// memory of a screen that draws a hundred skeletons.
pub const ENTRY_CAP: usize = 64;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static FRAME: RefCell<Frame> = RefCell::new(Frame::default());
}

/// How many threads are armed. The unarmed fast path of [`note`], [`mark`] and [`renote`] is ONE
/// relaxed load of this and a branch, before any thread-local access: shipping builds never arm,
/// so on the television no placeholder hook costs more than that.
static ARMED_THREADS: AtomicU32 = AtomicU32::new(0);

/// Arm or disarm this thread, keeping [`ARMED_THREADS`] equal to the number of armed threads.
/// Returns the prior state.
fn set_armed(on: bool) -> bool {
    let was = ARMED.with(|a| a.replace(on));
    match armed_delta(was, on) {
        1 => {
            ARMED_THREADS.fetch_add(1, Ordering::Relaxed);
        }
        -1 => {
            ARMED_THREADS.fetch_sub(1, Ordering::Relaxed);
        }
        _ => {}
    }
    was
}

/// How [`ARMED_THREADS`] moves when a thread goes from `was` to `now`.
const fn armed_delta(was: bool, now: bool) -> i32 {
    (now as i32) - (was as i32)
}

/// True when this thread records. Cheap on every thread that is not armed.
#[inline]
fn armed() -> bool {
    ARMED_THREADS.load(Ordering::Relaxed) != 0 && ARMED.with(Cell::get)
}

/// Start recording on this thread. The dump driver calls it once; [`capture`] does it for a test.
pub fn arm() {
    set_armed(true);
}

/// Stop recording on this thread (and drop what was recorded).
pub fn disarm() {
    set_armed(false);
    reset();
}

/// Clear this frame's count. The driver calls it before drawing each frame.
pub fn reset() {
    FRAME.with(|f| *f.borrow_mut() = Frame::default());
}

/// This frame's placeholder count so far (0 when not armed).
pub fn count() -> u32 {
    FRAME.with(|f| f.borrow().count)
}

/// Take this frame's [`Frame`] and clear it. The driver calls it after drawing each frame.
pub fn take() -> Frame {
    FRAME.with(|f| std::mem::take(&mut *f.borrow_mut()))
}

/// **Where a live draw began**, for [`since`]. Free when unarmed.
#[derive(Clone, Copy, Default, Debug)]
pub struct Mark {
    count: u32,
    entries: usize,
    absent: usize,
}

/// Note where the frame's counter stands now.
#[inline]
pub fn mark() -> Mark {
    if !armed() {
        return Mark::default();
    }
    FRAME.with(|f| {
        let f = f.borrow();
        Mark { count: f.count, entries: f.entries.len(), absent: f.absent.len() }
    })
}

/// **The placeholders a captured page image carries.** A page image is drawn live ONCE, into its
/// snapshot, and then painted unchanged on every frame until the page is quiescent
/// (`containers::transition::PageImage`); nothing walks the page in those frames, so the debt the
/// image holds is recorded at the capture ([`since`]) and re-noted on every frame the image is
/// drawn ([`renote`]). A frame that shows a snapshot containing a spinner, a "Loading" title or a
/// skeleton therefore counts it, exactly as if the page had drawn it live.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Debt {
    count: u32,
    entries: Vec<Entry>,
    absent: Vec<Entry>,
}

impl Debt {
    /// True when the captured page held no placeholder and no settled absence.
    pub fn is_empty(&self) -> bool {
        self.count == 0 && self.absent.is_empty()
    }
}

/// Everything noted since `m`: the debt of the draw between [`mark`] and now.
pub fn since(m: Mark) -> Debt {
    if !armed() {
        return Debt::default();
    }
    FRAME.with(|f| {
        let f = f.borrow();
        if f.count < m.count {
            // the frame was reset in between: all of it belongs to this draw
            return Debt { count: f.count, entries: f.entries.clone(), absent: f.absent.clone() };
        }
        let from = m.entries.min(f.entries.len());
        let from_absent = m.absent.min(f.absent.len());
        Debt { count: f.count - m.count, entries: f.entries[from..].to_vec(), absent: f.absent[from_absent..].to_vec() }
    })
}

/// Count `debt` again: a frame that paints a snapshot that holds these placeholders.
#[inline]
pub fn renote(debt: &Debt) {
    if debt.is_empty() || !armed() {
        return;
    }
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        f.count += debt.count;
        for e in &debt.entries {
            if f.entries.len() >= ENTRY_CAP {
                break;
            }
            f.entries.push(e.clone());
        }
        for e in &debt.absent {
            if f.absent.len() >= ENTRY_CAP {
                break;
            }
            f.absent.push(e.clone());
        }
    });
}

/// Run `f` armed and return its result with what it drew; restores the prior armed state. The seam
/// a host test uses to ask "did this draw count a placeholder".
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Frame) {
    let was = set_armed(true);
    reset();
    let r = f();
    let frame = take();
    set_armed(was);
    (r, frame)
}

/// [`capture`] for a host test that DRAWS: `f` runs inside a backdrop discovery walk, in which every
/// `Painter` primitive declares itself instead of calling GL (the host has no GL context), so the
/// real draw code runs end to end and the counter sees what it would on a frame.
#[cfg(any(test, feature = "test-support"))]
pub fn capture_declared<R>(f: impl FnOnce() -> R) -> (R, Frame) {
    use crate::frame::backdrop::{self, Sources, Z};
    use std::{cell::RefCell, rc::Rc};
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let _walk = backdrop::discover(sources);
    let _page = backdrop::layer(Z::page(1), false);
    capture(f)
}

/// **Count one placeholder DRAW.** Call it where the placeholder is painted, with the painter that
/// paints it. A recording pass paints nothing and so counts nothing.
#[inline]
pub fn note(p: Painter, reason: Reason, key: &str) {
    if ARMED_THREADS.load(Ordering::Relaxed) == 0 || p.is_recording() || !ARMED.with(Cell::get) {
        return;
    }
    record(reason, key);
}

/// **Report one draw of an absence**: the form something takes when it will never arrive (a hero
/// title typed out because its clearLogo 404ed). Not a wait, so [`Frame::count`] does not move;
/// it lands in [`Frame::absent`].
#[inline]
pub fn note_absent(p: Painter, reason: Reason, key: &str) {
    if ARMED_THREADS.load(Ordering::Relaxed) == 0 || p.is_recording() || !ARMED.with(Cell::get) {
        return;
    }
    record_absent(reason, key);
}

#[cold]
#[inline(never)]
fn record_absent(reason: Reason, key: &str) {
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        if f.absent.len() < ENTRY_CAP {
            f.absent.push(Entry { reason, key: key.to_owned() });
        }
    });
}

/// The armed half of [`note`], kept out of line so the unarmed call stays a load and a branch.
#[cold]
#[inline(never)]
fn record(reason: Reason, key: &str) {
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        f.count += 1;
        if f.entries.len() < ENTRY_CAP {
            f.entries.push(Entry { reason, key: key.to_owned() });
        }
    });
}

/// The painter a placeholder paints through: in the sentinel build the alpha and RGB-dim cascade
/// are dropped (translate, clip, zoom and pop stay), so the colour lands exactly; otherwise `p`.
#[inline]
pub fn painter(p: Painter) -> Painter {
    #[cfg(feature = "placeholder-sentinel")]
    {
        Painter { a: 1.0, rgb: 1.0, ..p }
    }
    #[cfg(not(feature = "placeholder-sentinel"))]
    {
        p
    }
}

/// True in a build whose placeholders are sentinel-painted.
pub const SENTINEL: bool = cfg!(feature = "placeholder-sentinel");

/// The sentinel's alpha is 1: the guarantee that blending leaves the exact value.
pub const fn sentinel_alpha_is_one() -> bool {
    theme::PLACEHOLDER_SENTINEL[3] == 1.0
}
const _: () = assert!(sentinel_alpha_is_one());

/// **A skeleton sheet** (`SKELETON_TOP` → `SKELETON_BOT`) at `r`, counted. The sheened gradient
/// every missing poster and still wears; the sentinel build paints the flat sentinel instead,
/// unfaded and without the rim.
pub fn skeleton(p: Painter, reason: Reason, key: &str, r: Rect, rad: f32) {
    note(p, reason, key);
    #[cfg(not(feature = "placeholder-sentinel"))]
    p.rect_sheened(r, rad, theme::SKELETON_TOP, theme::SKELETON_BOT);
    #[cfg(feature = "placeholder-sentinel")]
    painter(p).rect(r, rad, theme::SKELETON_TOP, theme::SKELETON_BOT, 0.0);
}

/// **A sheened face with `top`→`bot` fills**, counted: a placeholder that is not a skeleton token
/// (the profile chip's disc). The sentinel build paints the flat sentinel in its place.
pub fn face(p: Painter, reason: Reason, key: &str, r: Rect, rad: f32, top: [f32; 4], bot: [f32; 4]) {
    note(p, reason, key);
    #[cfg(not(feature = "placeholder-sentinel"))]
    p.rect_sheened(r, rad, top, bot);
    #[cfg(feature = "placeholder-sentinel")]
    {
        let _ = (top, bot);
        painter(p).rect(r, rad, theme::PLACEHOLDER_SENTINEL, theme::PLACEHOLDER_SENTINEL, 0.0);
    }
}

/// **The sheened flat ground** (`CARD_PLACEHOLDER`) at `r`, counted — a `Thumb` or `Person` tile
/// whose picture has not arrived.
pub fn ground(p: Painter, reason: Reason, key: &str, r: Rect, rad: f32) {
    note(p, reason, key);
    #[cfg(not(feature = "placeholder-sentinel"))]
    p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
    #[cfg(feature = "placeholder-sentinel")]
    painter(p).rrect(r, rad, rad, theme::CARD_PLACEHOLDER);
}

/// **The unsheened flat ground** (`CARD_PLACEHOLDER`) at `r`, counted — the info panel's still.
pub fn flat(p: Painter, reason: Reason, key: &str, r: Rect, rad: f32) {
    note(p, reason, key);
    painter(p).rrect(r, rad, rad, theme::CARD_PLACEHOLDER);
}

/// **A text-line bar or swept sheet in the sentinel**: under the feature, paint `r` flat sentinel
/// and return `true` (the caller skips its own paint and its sheen); in every other build return
/// `false` and draw nothing. `skeleton_bar` and `skeleton_sheet` call it after [`note`].
#[inline]
pub fn sentinel_fill(p: Painter, r: Rect, rad: f32) -> bool {
    #[cfg(feature = "placeholder-sentinel")]
    {
        painter(p).rect(r, rad, theme::PLACEHOLDER_SENTINEL, theme::PLACEHOLDER_SENTINEL, 0.0);
        true
    }
    #[cfg(not(feature = "placeholder-sentinel"))]
    {
        let _ = (p, r, rad);
        false
    }
}

/// **The painter and ink a placeholder's TEXT is drawn with**, counted: `(p, col)` unchanged in
/// every build but the sentinel's, where it is the unfaded painter and [`theme::PLACEHOLDER_SENTINEL`].
pub fn ink(p: Painter, reason: Reason, key: &str, col: [f32; 4]) -> (Painter, [f32; 4]) {
    note(p, reason, key);
    #[cfg(feature = "placeholder-sentinel")]
    {
        let _ = col;
        (painter(p), theme::PLACEHOLDER_SENTINEL)
    }
    #[cfg(not(feature = "placeholder-sentinel"))]
    {
        (p, col)
    }
}

/// **The stale strip's cover**: in the sentinel build a flat sentinel rect over `r` (the strip the
/// cascade dimmed to `STALE_ALPHA`, whose real art cannot be an exact colour); nothing otherwise.
#[inline]
pub fn stale_cover(p: Painter, r: Rect, rad: f32) {
    let _ = sentinel_fill(p, r, rad);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 150.0)
    }

    #[test]
    fn nothing_is_counted_until_armed() {
        reset();
        note(Painter::root(), Reason::CardSkeleton, "a");
        assert_eq!(count(), 0);
        assert_eq!(take(), Frame::default());
    }

    #[test]
    fn a_draw_ticks_the_counter_with_its_reason_and_key() {
        let (_, f) = capture_declared(|| {
            skeleton(Painter::root(), Reason::CardSkeleton, "/library/metadata/1/thumb", rect(), 8.0);
            ground(Painter::root(), Reason::CardGround, "/p", rect(), 8.0);
            flat(Painter::root(), Reason::InfoPanelStill, "", rect(), 8.0);
            face(Painter::root(), Reason::ProfileChip, "/a", rect(), 8.0, theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_FILL);
        });
        assert_eq!(f.count, 4);
        assert_eq!(
            f.entries.iter().map(|e| (e.reason, e.key.as_str())).collect::<Vec<_>>(),
            vec![
                (Reason::CardSkeleton, "/library/metadata/1/thumb"),
                (Reason::CardGround, "/p"),
                (Reason::InfoPanelStill, ""),
                (Reason::ProfileChip, "/a"),
            ]
        );
    }

    #[test]
    fn a_recording_pass_counts_nothing() {
        let (_, f) = capture_declared(|| {
            skeleton(Painter::recording(), Reason::CardSkeleton, "a", rect(), 8.0);
            crate::record_walk(|| ground(Painter::root(), Reason::CardGround, "b", rect(), 8.0));
        });
        assert_eq!(f, Frame::default());
    }

    #[test]
    fn reset_and_take_are_the_drivers_frame_boundary() {
        arm();
        note(Painter::root(), Reason::DetailSpinner, "");
        assert_eq!(count(), 1);
        reset();
        assert_eq!(count(), 0);
        note(Painter::root(), Reason::LibrarySpinner, "");
        let f = take();
        assert_eq!((f.count, f.of(Reason::LibrarySpinner)), (1, 1));
        assert_eq!(count(), 0, "take clears the frame");
        disarm();
    }

    #[test]
    fn the_entry_list_is_capped_but_the_count_is_not() {
        let (_, f) = capture(|| {
            for _ in 0..ENTRY_CAP + 5 {
                note(Painter::root(), Reason::SkeletonBar, "");
            }
        });
        assert_eq!(f.count as usize, ENTRY_CAP + 5);
        assert_eq!(f.entries.len(), ENTRY_CAP);
    }

    #[test]
    fn a_debt_recorded_at_a_draw_is_noted_again_on_a_frame_that_only_shows_it() {
        let (debt, _) = capture(|| {
            note(Painter::root(), Reason::CardSkeleton, "before");
            let m = mark();
            note(Painter::root(), Reason::DetailSpinner, "");
            note(Painter::root(), Reason::DetailTitle, "rk");
            note_absent(Painter::root(), Reason::HeroLogoAbsent, "rk");
            since(m)
        });
        assert!(!debt.is_empty());
        let ((), frame) = capture(|| renote(&debt));
        assert_eq!(frame.count, 2, "only what was drawn after the mark: {frame:?}");
        assert_eq!(frame.entries.iter().map(|e| e.reason).collect::<Vec<_>>(), [Reason::DetailSpinner, Reason::DetailTitle]);
        assert_eq!(frame.absent.len(), 1, "an absence is carried too: {frame:?}");
    }

    #[test]
    fn nothing_is_recorded_noted_or_carried_until_armed() {
        disarm();
        let m = mark();
        note(Painter::root(), Reason::CardSkeleton, "a");
        note_absent(Painter::root(), Reason::HeroLogoAbsent, "a");
        assert!(since(m).is_empty());
        renote(&Debt { count: 3, entries: vec![], absent: vec![] });
        assert_eq!(take(), Frame::default());
    }

    #[test]
    fn a_recording_pass_reports_no_absence() {
        let (_, f) = capture_declared(|| note_absent(Painter::recording(), Reason::HeroLogoAbsent, "a"));
        assert_eq!(f, Frame::default());
    }

    #[test]
    fn an_absence_is_listed_and_never_counted() {
        let (_, f) = capture(|| note_absent(Painter::root(), Reason::HeroLogoAbsent, "42"));
        assert_eq!(f.count, 0);
        assert_eq!(f.absent, vec![Entry { reason: Reason::HeroLogoAbsent, key: "42".into() }]);
    }

    #[test]
    fn a_draw_after_a_reset_belongs_to_the_debt_whole() {
        arm();
        reset();
        let m = Mark { count: 5, entries: 5, absent: 0 };
        note(Painter::root(), Reason::CardGround, "k");
        let d = since(m);
        disarm();
        assert_eq!(d.count, 1);
    }

    /// `ARMED_THREADS` is process-wide, so a test cannot read it while others arm: the bookkeeping is
    /// a pure function of (was, now) and this pins it.
    #[test]
    fn the_armed_thread_count_moves_only_on_a_change_of_state() {
        assert_eq!(armed_delta(false, true), 1);
        assert_eq!(armed_delta(true, false), -1);
        assert_eq!(armed_delta(true, true), 0);
        assert_eq!(armed_delta(false, false), 0);
    }

    #[test]
    fn arming_and_disarming_a_thread_is_observable_on_that_thread() {
        arm();
        arm();
        assert!(armed() && ARMED_THREADS.load(Ordering::Relaxed) >= 1);
        let _ = capture(|| ());
        assert!(armed(), "capture restores the prior state");
        disarm();
        assert!(!armed());
        let _ = capture(|| assert!(armed()));
        assert!(!armed(), "a capture from unarmed returns to unarmed");
    }

    /// The default paint output is unchanged: in a build without the sentinel every token is the
    /// stop it always was, so `placeholder.rs` added accounting and not a single pixel.
    #[cfg(not(feature = "placeholder-sentinel"))]
    #[test]
    fn the_non_sentinel_colour_constants_are_unchanged() {
        use plx_gfx::gfx::tokens::rgb8;
        assert_eq!(theme::CARD_PLACEHOLDER, rgb8(0x1f, 0x21, 0x29));
        assert_eq!(theme::SKELETON_TOP, rgb8(0x1f, 0x21, 0x29));
        assert_eq!(theme::SKELETON_BOT, rgb8(0x14, 0x17, 0x1c));
        assert_eq!(theme::CARD_ABSENT, theme::CARD_PLACEHOLDER);
        assert!(!SENTINEL);
    }

    #[cfg(feature = "placeholder-sentinel")]
    #[test]
    fn the_sentinel_build_paints_one_exact_colour() {
        assert!(SENTINEL);
        for c in [theme::CARD_PLACEHOLDER, theme::SKELETON_TOP, theme::SKELETON_BOT] {
            assert_eq!(c, theme::PLACEHOLDER_SENTINEL);
        }
        assert_eq!(theme::CARD_ABSENT, plx_gfx::gfx::tokens::rgb8(0x1f, 0x21, 0x29), "absence is never sentinel");
        let faded = Painter::root().alpha(0.35).rgb(0.5);
        assert_eq!((painter(faded).opacity(), painter(faded).scale()), (1.0, 1.0));
    }

    #[test]
    fn the_sentinel_is_opaque_and_matches_its_byte_code() {
        assert_eq!(theme::PLACEHOLDER_SENTINEL[3], 1.0);
        let [r, g, b] = theme::PLACEHOLDER_SENTINEL_RGB8;
        for (c, byte) in theme::PLACEHOLDER_SENTINEL.iter().zip([r, g, b]) {
            assert_eq!((c * 255.0).round() as u8, byte);
        }
    }

    /// #FF00FE is in no palette stop: the whole of `theme.rs` names it exactly once (its own
    /// definition), so a frame pixel of that value is a placeholder and nothing else.
    #[test]
    fn the_sentinel_is_in_no_palette_stop() {
        let src = include_str!("theme.rs").to_ascii_lowercase();
        let spaced = src.matches("rgb8(0xff, 0x00, 0xfe)").count();
        let packed = src.matches("ff00fe").count();
        assert_eq!(spaced, 1, "only the sentinel's own definition may spell #FF00FE");
        assert_eq!(packed, 1, "only the sentinel's own doc line may spell #FF00FE");
    }
}
