//! The person / actor page — reached by pressing OK on a detail page's cast headshot.
//!
//! **The reference is Apple TV, not Plex.** Plex's own person page is a field list (left portrait,
//! "Actor, Producer, Director", Born + age, social handles, a count-badged "Movies & Shows in
//! Media Libraries", "Known For", "Featured Videos") — that is the feature checklist, not the
//! mockup. This is the tvOS shape, one centred column: a **circular portrait**, the **name**, the
//! **roles** line, a **Born / Died** line, a **3-line bio truncated with an inline `MORE`**, then
//! plain **`Movies`** and **`Shows`** shelves of ordinary poster cards — the same [`CardRow`] tiles
//! the home shelves, the Library grid and detail's Related row draw, so a poster looks and animates
//! identically wherever it appears.
//!
//! **Every header line is optional, and the page is composed so that missing ones read as
//! finished rather than broken.** The name + portrait arrive with the page (the cast row already
//! held them); the roles/dates/bio come from plex.tv (`crate::person`, `plex/discover.rs`) and may
//! never come at all — a person the provider has no record of, or a TV on a LAN with no internet.
//! There is therefore **no placeholder and no header spinner**: each line is drawn only when it has
//! content, the block's height is MEASURED from what is present, and the shelves reflow under it.
//! The degenerate case is exactly the shape this page shipped as: portrait, name, shelves.
//!
//! **Social handles are deliberately not drawn.** The provider sends `External[]` (facebook /
//! instagram / twitter), and Plex's web page shows them, but on a TV they are dead ink: there is no
//! browser to open them in, no keyboard to type them into and no way to hand one to a phone. They
//! would cost a row of the one screen area this design spends on the portrait.
//!
//! Structure mirrors `detail.rs`: the page is a [`ScrollColumn`] whose children are **the header**
//! and the PRESENT shelves, so the block flow, the off-screen cull and the scroll spring are the
//! shared ones. The header is also a **focus row** — child 0, exactly as detail's hero is its
//! section 0 — which is what makes the portrait reachable again once focus has gone down into the
//! shelves (see [`move_focus`]). Perf discipline for the A53 + Mali: shelves are drawn through
//! [`card_row::strip`], which culls with `on_axis` so only VISIBLE tiles ever call `resolve_tex`
//! (the 64-slot poster LRU must not see off-screen requests), and `crate::person` caps each shelf
//! at a `CardRow`'s spring count so the live spring population is bounded.
#![allow(non_upper_case_globals)]
use crate::person::Person;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{Art, Spinner};
use crate::ui::{card_row::reveal, Column, Env, Painter, Rect, ScrollColumn, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

// ---- geometry (a measured flow; every Y below is derived, none is authored) -------------------

/// Shelves, in flow order. Kind 0 = Movies, 1 = Shows — the same order `crate::person`'s store
/// uses, so an index means one thing across the store, the focus model and the hit-test.
const NSHELF: usize = 2;
/// Headings as C-string LITERALS, not `&str` — they are drawn every frame, and `CString::new`
/// per shelf per frame is a heap allocation the draw path does not need (the same reason
/// `detail.rs` writes `c"Related"` / `c"Cast & Crew"`).
const SHELF_TITLE: [&std::ffi::CStr; NSHELF] = [c"Movies", c"Shows"];

/// Portrait diameter. Requested from the image transcoder at 300×300 — the SAME size the detail
/// page's cast circles ask for, so arriving here from a headshot is a poster-cache HIT, not a
/// fresh round trip through `/photo/:/transcode`.
const PORTRAIT_D: f32 = 260.0;
const PORTRAIT_RES: (c_int, c_int) = (300, 300);
/// First child's pre-scroll top — the page's top air above the portrait.
const HEADER_TOP: f32 = 96.0;
/// portrait → name: a major gap, because it separates two whole regions of the header block.
const NAME_GAP: f32 = theme::space::XL;
/// name → the roles kicker: the tight gap of a title and its subtitle, not a block break.
const ROLES_GAP: f32 = theme::space::SM;
/// roles → the life dates. The two are one meta stack, so this is the smallest rung.
const LIFE_GAP: f32 = theme::space::XS;
/// the meta stack → bio (a heading block and its body text).
const BIO_GAP: f32 = theme::space::MD;
/// Bio column width. Narrower than the shelves on purpose: a 3-line block set to the full
/// 1740px content width would run ~14 words a line and read as a paragraph of legal text.
const BIO_W: f32 = 1120.0;
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
/// The meta lines (roles, Born/Died) elide at the bio column's width rather than running the full
/// panel: they belong to the same centred column the bio does, and a birthplace like
/// "Twickenham, England, UK" can otherwise push the line wider than the text under it.
const META_W: f32 = BIO_W;
/// Block gap between the header and a shelf, and between shelves — detail.rs's `SECTION_GAP`.
const SHELF_GAP: f32 = theme::space::XL;
/// Shelf heading → poster row, and poster row → block bottom (the focused tile's under-title
/// band). Same two as detail's Related row, so a shelf is paced identically on both pages.
const SHELF_LABEL_H: f32 = 46.0;
const SHELF_UNDER_H: f32 = 54.0;
/// A focused shelf lifts no higher than this from the screen top.
const TOP_MARGIN: f32 = 120.0;
/// Air kept under the lowest visible content when a shelf is revealed by scrolling.
const BOTTOM_PAD: f32 = theme::space::LG;

/// Cap-band height of a run — the layout height of one line of text at `sz`. Layout ≠ paint:
/// descenders hang below this and are never measured (see `ui::label`'s module docs).
fn cap_h(sz: c_int, bold: c_int) -> f32 {
    let (top, base) = crate::text::text_cap_band(sz, bold);
    base - top
}

// ---- screen state ----------------------------------------------------------------------------

struct Scene {
    /// **The header holds focus** (flow child 0) rather than any shelf. It is the page's top
    /// position, and it is what makes the portrait reachable: see [`move_focus`].
    on_header: bool,
    /// which shelf holds focus (a KIND, not a position — a shelf that empties must not silently
    /// hand its focus to the other one's contents). Meaningless while [`Scene::on_header`].
    focus_kind: usize,
    /// per-shelf focus memory: leaving a shelf and coming back restores the tile you were on
    col: [c_int; NSHELF],
    shelves: [CardRow; NSHELF],
    column: ScrollColumn,
    spin_ms: f32,
    /// the detail page this page was opened from — BACK's target (see [`leave`])
    from_rk: String,
    /// a cast headshot was just activated; app.rs takes this and routes (see [`take_request`])
    requested: bool,
    /// The header's text runs, NUL-terminated ONCE — at [`open`] for the name, and on every store
    /// change for the two plex.tv lines (see [`refresh_header`]) — rather than per frame. `Label`
    /// holds a non-owning pointer, so these must outlive the draw: a field does, a temporary would
    /// not. Building them here also keeps the draw path allocation-free, which is the rule the
    /// shelf tiles already follow.
    name_c: CString,
    roles_c: CString,
    life_c: CString,
    /// The measured header layout, and the flag that says it needs remeasuring. Rebuilt in
    /// [`update`] on the frame a store change lands, never in the draw — see [`HeaderFlow`].
    header: HeaderFlow,
    header_dirty: bool,
}

impl Scene {
    fn new() -> Self {
        Scene {
            on_header: true,
            focus_kind: 0,
            col: [0; NSHELF],
            shelves: [CardRow::new(); NSHELF],
            column: ScrollColumn::new(HEADER_TOP, TOP_MARGIN),
            spin_ms: 0.0,
            from_rk: String::new(),
            requested: false,
            name_c: CString::default(),
            roles_c: CString::default(),
            life_c: CString::default(),
            header: HeaderFlow::default(),
            header_dirty: true,
        }
    }
}

static mut SCENE: Option<Scene> = None;
fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).get_or_insert_with(Scene::new) }
}

/// What an OK / click on this page means for app.rs (everything else is consumed internally).
pub(crate) enum Action {
    None,
    /// A shelf card was activated — open its detail page (app.rs owns routing).
    Card,
}

// ---- present shelves -------------------------------------------------------------------------

/// The shelf kinds that have content, in flow order, and how many. A person with no shows has no
/// "Shows" heading at all — an empty labelled shelf reads as a broken fetch.
fn present(p: &Person) -> ([usize; NSHELF], usize) {
    let mut v = [0usize; NSHELF];
    let mut n = 0;
    for k in 0..NSHELF {
        if !p.shelf(k).is_empty() {
            v[n] = k;
            n += 1;
        }
    }
    (v, n)
}

/// Flow position of the focused shelf among the present ones (0-based), or None when the page has
/// no shelves yet.
fn focus_pos(p: &Person, sc: &Scene) -> Option<usize> {
    let (kinds, n) = present(p);
    kinds[..n].iter().position(|&k| k == sc.focus_kind)
}

// ---- the scroll flow (ScrollColumn children: 0 = header, 1.. = present shelves) ---------------

impl Column for Scene {
    fn len(&self) -> usize {
        crate::person::current().map(|p| 1 + present(p).1).unwrap_or(1)
    }
    fn height(&self, i: usize) -> f32 {
        if i == 0 {
            self.header.total
        } else {
            shelf_block_h()
        }
    }
    fn gap_before(&self, _i: usize) -> f32 {
        SHELF_GAP
    }
    fn focus_child(&self) -> Option<usize> {
        if self.on_header {
            return Some(0);
        }
        crate::person::current().and_then(|p| focus_pos(p, self)).map(|pos| pos + 1)
    }
    fn draw_child(&self, i: usize, _env: &Env, p: Painter) {
        let Some(person) = crate::person::current() else { return };
        if i == 0 {
            draw_header(p, person, self);
            return;
        }
        let (kinds, n) = present(person);
        if let Some(&kind) = kinds[..n].get(i - 1) {
            draw_shelf(p, person, kind, self);
        }
    }
}

/// Where each header line sits inside the header block, and how tall the block is — the ONE place
/// the header flow is computed, so the measure and [`draw_header`] cannot disagree about where a
/// line goes. Every entry after the portrait is `None` unless its content exists: a person with no
/// roles line has no gap where one would have been, which is what makes a sparse header read as
/// composed rather than as fields that failed to load.
///
/// **Computed once per store change, not per frame** ([`Scene::header`]). It is the only thing on
/// this page that measures text, and it is reached from `Column::height(0)` — which `child_top`,
/// `content_h`, `scroll_target` and the pointer hit-test all call, several times a frame each. A
/// per-frame `HeaderFlow` would re-hash the whole biography (~3 KB of prose) on every one of those,
/// which is exactly the cost `tile_at` already hoists its `row_y` out of the tile loop to avoid.
#[derive(Clone, Copy, Default)]
struct HeaderFlow {
    name_y: f32,
    roles_y: Option<f32>,
    life_y: Option<f32>,
    bio_y: Option<f32>,
    total: f32,
}

fn header_flow(sc: &Scene) -> HeaderFlow {
    let name_y = PORTRAIT_D + NAME_GAP;
    let mut y = name_y + cap_h(theme::size::HERO, 1);
    let mut f = HeaderFlow { name_y, ..HeaderFlow::default() };
    if !sc.roles_c.as_bytes().is_empty() {
        y += ROLES_GAP;
        f.roles_y = Some(y);
        y += cap_h(theme::size::LABEL, 0);
    }
    if !sc.life_c.as_bytes().is_empty() {
        // the life line hugs the roles line above it, but takes the roles GAP when it is the first
        // meta line — the gap belongs to the stack's top edge, not to a particular line
        y += if f.roles_y.is_some() { LIFE_GAP } else { ROLES_GAP };
        f.life_y = Some(y);
        y += cap_h(theme::size::LABEL, 0);
    }
    let bio = crate::person::current().map(|p| p.bio.as_str()).unwrap_or("");
    if !bio.is_empty() {
        y += BIO_GAP;
        f.bio_y = Some(y);
        y += bio_view(bio).measure_h(BIO_W);
    }
    f.total = y;
    f
}

/// The bio block, in ONE place so its measure and its draw cannot disagree: 3 lines of body text
/// with the classic inline `… MORE` affordance, which `TextView` only paints when the cap actually
/// hid words. The affordance is a truncation MARK, not a control — the same treatment detail's
/// About card gives its synopsis; nothing on this page expands text in place.
fn bio_view(bio: &str) -> TextView<'_> {
    TextView::new(bio, theme::size::BODY, theme::TEXT_SECONDARY)
        .leading(BIO_LEAD)
        .max_lines(BIO_LINES)
        .trailing("MORE", theme::TEXT_PRIMARY)
}

/// Rebuild the header's cached text runs from the store — from [`update`], on the frame a store
/// change lands. The two plex.tv lines are composed HERE, once, rather than in the draw: they are
/// `format!`s over store fields, and doing that 60 times a second for a string that changes twice
/// in a page's life is exactly the per-frame allocation the shelf tiles avoid.
///
/// **The life line is a stack of independent facts, joined only where both exist.** Born, the
/// birthplace and Died each appear on their own terms — a living person simply has no Died clause,
/// a person with a death date but no birth date reads "Died 2 Jun 2017", and someone plex.tv knows
/// nothing about produces the empty string, which the flow then gives no room to at all. That is
/// why this builds a LIST and joins it, rather than formatting one template with holes in it.
fn refresh_header(sc: &mut Scene) {
    let Some(p) = crate::person::current() else {
        sc.roles_c = CString::default();
        sc.life_c = CString::default();
        return;
    };
    sc.roles_c = cstr_elide(&p.roles, theme::size::LABEL);

    let mut life: Vec<String> = Vec::new();
    let born = crate::ui::fmt::pretty_date(&p.born, 0);
    if !born.is_empty() {
        life.push(match p.birthplace.is_empty() {
            true => format!("Born {born}"),
            false => format!("Born {born}, {}", p.birthplace),
        });
    }
    let died = crate::ui::fmt::pretty_date(&p.died, 0);
    if !died.is_empty() {
        life.push(format!("Died {died}"));
    }
    sc.life_c = cstr_elide(&life.join(" \u{b7} "), theme::size::LABEL);
}

/// A header meta line, NUL-terminated and clipped to the centred column. `Painter` has no clip, so
/// an over-long line (a birthplace with four commas in it) would otherwise paint out past the bio
/// beneath it; `text::elide` is the shared measurer that cuts it at the same width.
fn cstr_elide(s: &str, sz: c_int) -> CString {
    if s.is_empty() {
        return CString::default();
    }
    CString::new(crate::text::elide(s, META_W, sz, 0, true)).unwrap_or_default()
}

/// Total flowed content height (last child's bottom + the bottom air).
fn content_h(sc: &Scene) -> f32 {
    let col = sc.column;
    let last = sc.len().saturating_sub(1);
    col.child_top(sc, last) + sc.height(last) + BOTTOM_PAD
}

/// A shelf block's flowed height (heading + poster row + the focused tile's under-title band).
fn shelf_block_h() -> f32 {
    SHELF_LABEL_H + CARD_H + SHELF_UNDER_H
}

/// The MINIMAL scroll that reveals the block `[top, top+h)` inside `content` px of flow — the
/// shared [`reveal`] rule the shelves and the Library grid use, NOT a lift-to-margin pin: a block
/// scrolls only as far as its own extent needs, and not at all when it already fits.
///
/// It is minimal in BOTH directions, which is the half that matters here. Coming back up to the
/// first shelf it stops at that shelf's own top margin — short of the page top — so it can never
/// bring the header back on its own. That is not a defect to patch out of this rule (a lift-to-
/// margin pin would be worse: it would scroll the portrait away the instant the page mounted); it
/// is why the header is a focus row of its own, whose reveal costs no scroll at all.
///
/// Pure, and kept separate from [`scroll_target`] deliberately — the flow that feeds it is measured
/// through the text stack, which the host suite cannot link, so this is the layer the geometry
/// tests can actually reach.
fn reveal_block(cur: f32, top: f32, h: f32, content: f32) -> f32 {
    let lo = top + h - (SCR_H - BOTTOM_PAD);
    let hi = top - TOP_MARGIN;
    reveal(cur, lo, hi, (content - SCR_H).max(0.0))
}

fn scroll_target(sc: &Scene) -> f32 {
    let col = sc.column;
    let Some(fi) = sc.focus_child() else { return 0.0 };
    reveal_block(col.scroll.pos, col.child_top(sc, fi), sc.height(fi), content_h(sc))
}

// ---- entry / exit ----------------------------------------------------------------------------

/// Open the page for a cast member — called from `detail.rs`'s cast-row OK arm with everything
/// `Role[]` already carries (`key` = the local personId, `guid` = the `tagKey` plex.tv answers to).
/// Loads the store's header immediately, resets this screen's focus and scroll, remembers the
/// detail page to come back to, and raises the flag app.rs routes on.
///
/// **The page opens on its HEADER, not on the first shelf**, which is the whole reason this shape
/// works at all once there is a biography: the header is taller than the room a focused first shelf
/// leaves, so a page that mounted with the shelf focused would arrive already scrolled with the top
/// of the portrait cut off — the exact complaint [`move_focus`] fixes for the return trip. There is
/// nothing focusable in the header (it is a scroll POSITION, see [`move_focus`]), and nothing to
/// focus on the shelves yet either: they land a moment later, and DOWN goes to them.
pub(crate) fn open(key: &str, guid: &str, name: &str, thumb: &str) {
    crate::person::open(key, guid, name, thumb);
    let sc = scene();
    sc.on_header = true;
    sc.focus_kind = 0;
    sc.col = [0; NSHELF];
    sc.shelves = [CardRow::new(); NSHELF]; // a fresh person starts its shelves at their left edge
    sc.column.scroll.jump(0.0);
    sc.spin_ms = 0.0;
    sc.from_rk = crate::ui::detail::mounted_rk();
    sc.name_c = CString::new(name).unwrap_or_default();
    // The plex.tv lines start EMPTY and are built by `refresh_header` on the next `update` —
    // there is nothing to compose from yet, and clearing them here is what stops the previous
    // person's dates surviving into this one for the length of a fetch. The MEASURE is deferred to
    // that same `update` rather than done here, because `open` is on the detail page's OK path and
    // measuring text is the one thing on this page that is not free.
    sc.roles_c = CString::default();
    sc.life_c = CString::default();
    sc.header_dirty = true;
    sc.requested = true;
}

/// app.rs polls this right after a detail-page OK: true exactly once per [`open`]. Keeping the
/// request here (rather than a second return channel out of `detail::on_ok`) is what lets the
/// cast arm stay four lines in a file eight other changes are landing in.
pub(crate) fn take_request() -> bool {
    let sc = scene();
    std::mem::replace(&mut sc.requested, false)
}

/// BACK: leave the page and put the detail page it came from back on screen. `current()` is still
/// that item unless this page opened a DIFFERENT one in the meantime, in which case it is
/// re-opened by rk (async — the page mounts on the catalog row this frame).
pub(crate) fn leave() {
    let from = std::mem::take(&mut scene().from_rk);
    // and drop any un-consumed request with it: `requested` is a LATCH, and one left set here
    // would fire on some unrelated OK several screens later
    scene().requested = false;
    let same = crate::metadata::current().map(|d| d.rk == from).unwrap_or(false);
    if !same && !from.is_empty() {
        crate::ui::detail::open_rk(&from);
    }
    crate::person::close();
}

/// The focused shelf card (None while the shelves are still out, and None while the HEADER holds
/// focus — it contains no control) — app.rs opens its detail page.
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    let sc = scene();
    if sc.on_header {
        return None;
    }
    let p = crate::person::current()?;
    p.shelf(sc.focus_kind).get(sc.col[sc.focus_kind].max(0) as usize)
}

/// The only focusABLE thing on this page is a poster card, so OK always takes the tvOS press (dip
/// on key-down, activate on the spring-back). Named to match the other screens' predicate so
/// app.rs's press arm reads the same for all of them.
///
/// False on the header row, which is what keeps app.rs unchanged by the header becoming a row: it
/// arms no press and dispatches no activation, so OK there is simply inert — correct, because the
/// header carries no control.
pub(crate) fn focus_is_card() -> bool {
    focused_item().is_some()
}

pub(crate) fn on_ok() -> Action {
    if focused_item().is_some() {
        Action::Card
    } else {
        Action::None
    }
}

// ---- update ----------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    // `clamp_focus` and the dirty flag below both reach `scene()` themselves, so they run BEFORE
    // this function takes its own `&'static mut` — holding one across a call that mints a second is
    // aliasing UB, not a lint. (`detail.rs::update` carries the same note for the same reason.)
    if crate::person::pump() {
        clamp_focus();
        scene().header_dirty = true; // a profile landing changes the meta runs AND the flow
    }
    let sc = scene();
    // Remeasure BEFORE anything reads the flow this frame: `scroll_target` below, and every draw
    // after it, go through `Column::height(0)`, which is now a plain field read.
    if sc.header_dirty {
        refresh_header(sc);
        sc.header = header_flow(sc);
        sc.header_dirty = false;
    }
    sc.spin_ms += dt * 1000.0;
    let n_items = |k: usize| crate::person::current().map(|p| p.shelf(k).len()).unwrap_or(0);
    for k in 0..NSHELF {
        // a shelf that does not hold focus freezes its scroll (CardRow's `None` contract) — the
        // same behaviour a non-focused home shelf has. The header holding focus freezes them ALL,
        // which is what a page scrolled back to its top should do.
        let focus =
            (!sc.on_header && k == sc.focus_kind && n_items(k) > 0).then(|| sc.col[k].max(0) as usize);
        sc.shelves[k].update(n_items(k), focus, &RowStyle::HOME, dt);
    }
    let want = scroll_target(sc);
    sc.column.scroll.step(want, K_SCROLL, dt);
}

/// Keep the focus inside whatever the store currently holds — after a landing, and after every
/// navigation. A shelf can appear (the fetch lands) or be absent entirely, so the focused KIND is
/// re-seated onto the first present shelf rather than left pointing at nothing.
fn clamp_focus() {
    let sc = scene();
    let Some(p) = crate::person::current() else {
        sc.on_header = true;
        sc.focus_kind = 0;
        sc.col = [0; NSHELF];
        return;
    };
    let (kinds, n) = present(p);
    if n == 0 {
        // nothing below the header to hold focus — a page still fetching, or a person with
        // nothing in these libraries. Either way the header is the only row there is.
        sc.on_header = true;
    } else if !kinds[..n].contains(&sc.focus_kind) {
        sc.focus_kind = kinds[0];
    }
    for k in 0..NSHELF {
        let last = p.shelf(k).len() as c_int - 1;
        sc.col[k] = sc.col[k].clamp(0, last.max(0));
    }
}

// ---- input -----------------------------------------------------------------------------------

/// D-pad. Rows are **the header, then the present shelves** — the same "hero is a row" model
/// `detail.rs` uses (its section 0), and the reason for it here is a bug: with focus locked to the
/// shelves, the page could never scroll back up far enough to show the portrait again. The minimal
/// [`reveal`] rule pins a focused first shelf at [`TOP_MARGIN`] and stops, so once the header grew
/// past the room that leaves — which is exactly what a biography does to it — everything above that
/// pin was unreachable for the rest of the page's life. It is not a scroll bug: there was no row to
/// scroll TO. The header is that row, and [`scroll_target`] then puts the page back at 0 because
/// revealing child 0 costs no scroll at all.
///
/// The header holds no control (see the module docs), so UP into it is a scroll, not a selection:
/// nothing draws a focus ring, and DOWN returns to the shelf that had it.
pub(crate) fn move_focus(sym: c_uint) {
    let sc = scene();
    let Some(p) = crate::person::current() else { return };
    let (kinds, n) = present(p);
    if sc.on_header {
        // LEFT/RIGHT do nothing on a row with one item, and UP is already at the top
        if sym == SDLK_DOWN && n > 0 {
            sc.on_header = false;
        }
        clamp_focus();
        return;
    }
    if n == 0 {
        return;
    }
    let pos = focus_pos(p, sc).unwrap_or(0);
    let k = sc.focus_kind;
    match sym {
        SDLK_LEFT => sc.col[k] = (sc.col[k] - 1).max(0),
        SDLK_RIGHT => {
            let last = p.shelf(k).len() as c_int - 1;
            sc.col[k] = (sc.col[k] + 1).min(last.max(0));
        }
        SDLK_UP if pos > 0 => sc.focus_kind = kinds[pos - 1],
        SDLK_UP => sc.on_header = true, // ...and from the FIRST shelf, back to the portrait
        SDLK_DOWN if pos + 1 < n => sc.focus_kind = kinds[pos + 1],
        _ => {}
    }
    clamp_focus();
}

/// Screen-space y of shelf `kind`'s poster row, given its flow position among the present
/// shelves. The ONE vertical geometry the draw and the pointer hit-test share.
fn shelf_row_y(sc: &Scene, pos: usize) -> f32 {
    sc.column.child_top(sc, pos + 1) - sc.column.scroll.pos + SHELF_LABEL_H
}

/// The shelf tile under the pointer, or None in the gaps.
///
/// O(VISIBLE), deliberately — the same discipline `library.rs::cell_at` documents. Two things
/// make it that: the per-shelf `row_y` is computed ONCE outside the column loop (calling it per
/// tile walks the whole `child_top` flow ~48 times per pointer motion), and the column is derived
/// arithmetically from x instead of being searched for. The flow walk is now cheap in itself —
/// `Column::height(0)` reads the cached [`HeaderFlow`] rather than measuring text — but the hoist
/// stays: it is the part that keeps this O(VISIBLE) instead of O(tiles).
fn tile_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    let sc = scene();
    let p = crate::person::current()?;
    let (kinds, n) = present(p);
    for (pos, &kind) in kinds[..n].iter().enumerate() {
        let row_y = shelf_row_y(sc, pos);
        if my < row_y || my > row_y + CARD_H {
            continue; // not in this shelf's band
        }
        let slot = CARD_W + GAP;
        let x = mx - (MARGIN_X - sc.shelves[kind].scroll_x());
        if x < 0.0 {
            return None;
        }
        let i = (x / slot) as usize;
        // reject the inter-card gap: only the card's own width is a hit
        return (i < p.shelf(kind).len() && x - i as f32 * slot <= CARD_W).then_some((kind, i));
    }
    None
}

/// Pointer hover: focus follows the cursor onto a shelf card. Landing on a tile also LEAVES the
/// header — otherwise the hovered card would light up while the page still held itself at the top,
/// and the next OK would act on a card the scroll had already moved.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if let Some((kind, i)) = tile_at(mx, my) {
        focus_tile(kind, i);
    }
}

/// Pointer click: same activation as OK on the card under the cursor.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    match tile_at(mx, my) {
        Some((kind, i)) => {
            focus_tile(kind, i);
            Action::Card
        }
        None => Action::None,
    }
}

/// Move focus onto one shelf tile — the ONE place hover and click agree on what "focused" means.
fn focus_tile(kind: usize, i: usize) {
    let sc = scene();
    sc.on_header = false;
    sc.focus_kind = kind;
    sc.col[kind] = i as c_int;
}

// ---- draw ------------------------------------------------------------------------------------

pub(crate) fn draw() {
    // The clear IS the app surface (see `profiles::draw` for why no SURFACE_APP rect follows it).
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    let sc = scene();
    let env = Env::inert();
    if crate::person::current().is_none() {
        return;
    }
    let col = sc.column;
    col.draw(sc, &env, p);
    draw_shelf_state(p, &env, sc);
}

/// The header: centred circular portrait, the name, and whichever of the plex.tv lines exist —
/// roles, Born/Died, the 3-line bio. Drawn from the child's local origin (the `ScrollColumn` has
/// already translated to the block top) and from the SAME [`header_flow`] that measured it, so a
/// line can never draw where the flow did not reserve room for it.
fn draw_header(p: Painter, person: &Person, sc: &Scene) {
    let flow = sc.header;
    let portrait = Rect::new((SCR_W - PORTRAIT_D) * 0.5, 0.0, PORTRAIT_D, PORTRAIT_D);
    // `Art::Person`, not `Thumb` — the SAME art case the credits shelf this page was opened from
    // draws, so a person with no headshot wears the person glyph here exactly as their circle did
    // there. It also draws that glyph only for an EMPTY key, never for a texture still resolving,
    // which is the distinction a hand-rolled `thumb.is_empty()` branch here got wrong: it would
    // have flashed "no photo" on every portrait for the length of its fetch.
    // NEVER pixel-snapped: this is scaled art, and snapping it would fight the transcoder's
    // resampling exactly the way snapping a poster does.
    crate::ui::widgets::card(
        p,
        portrait,
        Art::Person { key: &person.thumb, res: PORTRAIT_RES },
        PORTRAIT_D * 0.5,
        false,
        1.0,
        0.0,
    );

    Label::new(sc.name_c.as_ptr(), theme::size::HERO, theme::TEXT_PRIMARY)
        .bold()
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(0.0, flow.name_y, SCR_W, 0.0));
    // The two meta lines: the roles kicker reads as content (SECONDARY), the life dates as a
    // footnote to it (TERTIARY). Both are pre-built + pre-elided runs — see `refresh_header`.
    let meta = [(flow.roles_y, &sc.roles_c, theme::TEXT_SECONDARY), (flow.life_y, &sc.life_c, theme::TEXT_TERTIARY)];
    for (y, run, col) in meta {
        if let Some(y) = y {
            Label::new(run.as_ptr(), theme::size::LABEL, col)
                .h(HAlign::Center)
                .v(VAlign::CapTop)
                .draw(p, Rect::new(0.0, y, SCR_W, 0.0));
        }
    }
    if let Some(by) = flow.bio_y {
        bio_view(&person.bio).draw(p, Rect::new((SCR_W - BIO_W) * 0.5, by, BIO_W, 0.0));
    }
}

/// One `Movies` / `Shows` shelf: the heading, then the shared strip of poster cards. Identical
/// composition to detail's Related row — same `RowStyle::HOME` geometry, same springs, same
/// progress language on the tiles (the unwatched corner angle rides `Art::Poster`, the amber
/// resume bar rides `PmsMovie::resume_frac`).
fn draw_shelf(p: Painter, person: &Person, kind: usize, sc: &Scene) {
    let items = person.shelf(kind);
    let row = &sc.shelves[kind];
    let focus_col = if sc.focus_kind == kind { sc.col[kind] } else { -1 };
    Label::new(SHELF_TITLE[kind].as_ptr(), theme::size::HEADLINE, theme::TEXT_HEADING)
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(MARGIN_X, -row.lift(), SCR_W, 0.0));
    card_row::strip(
        p,
        row,
        items.len(),
        focus_col,
        SHELF_LABEL_H,
        (CARD_W, CARD_H),
        CARD_W + GAP,
        &RowStyle::HOME,
        SCR_W,
        |i| Art::Poster(items.get(i)),
        |i| items.get(i).and_then(|m| m.resume_frac()),
        |i| items.get(i).and_then(|m| CString::new(m.title.clone()).ok()),
        |_, _, _, _| {},
    );
}

/// What sits where the shelves go while there are none: a spinner until the one `/media` request
/// lands, then — only if the person really has nothing in this library — a plain line saying so.
/// Anchored on the SAME flow the shelves would occupy, so nothing jumps when they arrive.
fn draw_shelf_state(p: Painter, env: &Env, sc: &Scene) {
    if crate::person::current().map(|pp| present(pp).1 > 0).unwrap_or(true) {
        return;
    }
    let y = HEADER_TOP + sc.header.total + SHELF_GAP - sc.column.scroll.pos;
    if crate::person::loading() {
        Spinner::new(SCR_W * 0.5, y + CARD_H * 0.5, 26.0)
            .phase(sc.spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(env, p);
        return;
    }
    Label::new(c"Nothing from this person is in your libraries".as_ptr(), theme::size::BODY, theme::TEXT_TERTIARY)
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(0.0, y + CARD_H * 0.5, SCR_W, 0.0));
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn item(rk: &str) -> PmsMovie {
        PmsMovie { rk: rk.to_string(), ..Default::default() }
    }

    /// Seed the store the way a landing does, without a server — and step off the header, because
    /// the page now MOUNTS there (see [`open`]) and every shelf-focus assertion below starts from
    /// the first shelf.
    fn seed(movies: usize, shows: usize) {
        seed_at_header(movies, shows);
        move_focus(SDLK_DOWN);
    }

    /// [`seed`] without the opening DOWN — for the tests that are about the header row itself.
    fn seed_at_header(movies: usize, shows: usize) {
        crate::person::open("161", "5d77682aeb5d26001f1de4b0", "Idina Menzel", "");
        crate::person::install_for_test(
            (0..movies).map(|i| item(&format!("m{i}"))).collect(),
            (0..shows).map(|i| item(&format!("s{i}"))).collect(),
        );
        clamp_focus();
    }

    /// A person with only movies has ONE shelf, and DOWN must not walk focus off the end of it —
    /// the focus model indexes the PRESENT shelves, not the two possible ones.
    #[test]
    fn focus_walks_only_the_shelves_that_exist() {
        let _serial = crate::testlock::serial();
        seed(3, 0);
        assert_eq!(scene().focus_kind, 0);
        move_focus(SDLK_DOWN);
        assert_eq!(scene().focus_kind, 0, "there is no Shows shelf to move to");
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT); // past the end
        assert_eq!(scene().col[0], 2, "column focus clamps to the last card");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m2".to_string()));

        seed(2, 2);
        move_focus(SDLK_DOWN);
        assert_eq!(scene().focus_kind, 1, "with both shelves present DOWN reaches Shows");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("s0".to_string()));
        crate::person::close();
    }

    /// **The reported bug**: once focus was down in the shelves the page could never get back to
    /// the portrait. UP from the FIRST shelf must reach the header — and from there the page's
    /// resting scroll is 0, i.e. the whole header on screen — while UP from a LOWER shelf still
    /// walks one shelf at a time and never teleports past them.
    #[test]
    fn up_from_the_first_shelf_returns_to_the_header_and_scrolls_the_portrait_back() {
        let _serial = crate::testlock::serial();
        seed(2, 2);
        move_focus(SDLK_DOWN); // → Shows, the second shelf
        assert_eq!(scene().focus_kind, 1);

        move_focus(SDLK_UP);
        assert!(!scene().on_header, "UP from the SECOND shelf must land on the first, not the header");
        assert_eq!(scene().focus_kind, 0);

        move_focus(SDLK_UP);
        assert!(scene().on_header, "UP from the first shelf left the portrait unreachable");
        assert!(focused_item().is_none(), "the header holds no card — OK there must do nothing");
        assert_eq!(Column::focus_child(scene()), Some(0), "the header is flow child 0");

        move_focus(SDLK_DOWN);
        assert!(!scene().on_header, "DOWN must go back into the shelves");
        assert_eq!(scene().focus_kind, 0);
        crate::person::close();
    }

    /// The header is the ONLY row while the shelves are still out (or when a person has nothing in
    /// these libraries) — UP/DOWN there must not focus a shelf that does not exist, and the page
    /// must not scroll away from a header that is all there is.
    #[test]
    fn a_person_with_no_shelves_stays_on_the_header() {
        let _serial = crate::testlock::serial();
        seed_at_header(0, 0);
        assert!(scene().on_header);
        move_focus(SDLK_DOWN);
        assert!(scene().on_header, "DOWN found a shelf that is not there");
        move_focus(SDLK_UP);
        assert!(scene().on_header);
        assert_eq!(Column::focus_child(scene()), Some(0));
        crate::person::close();
    }

    /// A shelf that disappears under the focus (a re-open landing with a different split) must
    /// re-seat it, not leave `focused_item` reading an empty list — and the per-shelf column
    /// memory must clamp with it.
    #[test]
    fn a_landing_that_drops_a_shelf_re_seats_the_focus() {
        let _serial = crate::testlock::serial();
        seed(2, 3);
        move_focus(SDLK_DOWN);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT);
        assert_eq!(scene().focus_kind, 1);
        assert_eq!(scene().col[1], 2);

        crate::person::install_for_test(vec![item("m0")], Vec::new()); // shows vanished
        clamp_focus();
        assert_eq!(scene().focus_kind, 0, "focus must fall back to the one present shelf");
        assert_eq!(scene().col[1], 0, "the vanished shelf's remembered column clamps too");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m0".to_string()));
        crate::person::close();
    }

    /// `leave()` must drop the un-consumed request as well as the return rk. `requested` is a
    /// LATCH — `detail::on_ok`'s cast arm raises it and only app.rs's per-frame drain lowers it —
    /// so one left set here fires on an unrelated OK several screens later and teleports the user
    /// onto an actor page they never asked for.
    #[test]
    fn leaving_drops_the_un_consumed_request_and_the_return_rk() {
        let _serial = crate::testlock::serial();
        seed(2, 0);
        scene().requested = true;
        scene().from_rk = "2005".to_string();
        // the detail page we came from is still loaded, which is `leave`'s common case AND keeps
        // this test from spawning a re-open worker it has no use for (a stray background thread
        // perturbs `stream.rs`'s process-wide fd count — see the note in that test)
        crate::metadata::set_current_for_test(Some(crate::metadata::Detail {
            rk: "2005".to_string(),
            ..Default::default()
        }));

        leave();

        assert!(!take_request(), "the request latched past the page it belonged to");
        assert!(scene().from_rk.is_empty(), "a stale return rk survived the page");
        assert!(crate::person::current().is_none());
    }

    // The header's own height needs the text stack (the name's cap band, the bio's pixel wrap),
    // which the host suite cannot link — so the geometry tests below feed `reveal_block`, which is
    // pure, with BOUNDS on that height instead of measuring it.
    //
    /// A bio-less header: portrait + name, the name bounded by its full point size.
    const BARE_HEADER_H: f32 = PORTRAIT_D + NAME_GAP + theme::size::HERO as f32;
    const FIRST_SHELF_TOP: f32 = HEADER_TOP + BARE_HEADER_H + SHELF_GAP;
    /// A FULL header — roles + Born/Died + a 3-line bio, each line bounded by its point size. This
    /// is the shape the plex.tv fetch produces, i.e. the normal one, and the reason the header had
    /// to become a focus row: it no longer fits above a focused first shelf.
    const FULL_HEADER_H: f32 = BARE_HEADER_H
        + ROLES_GAP
        + theme::size::LABEL as f32
        + LIFE_GAP
        + theme::size::LABEL as f32
        + BIO_GAP
        + BIO_LINES as f32 * BIO_LEAD;

    /// A bio-less page opens showing its WHOLE header: revealing the first shelf costs no scroll at
    /// all. This is why the target is the shared minimal `reveal` and not a lift-to-margin pin — a
    /// pin would slide the portrait off the top the moment the page mounted.
    #[test]
    fn a_bare_header_needs_no_scroll_to_reveal_the_first_shelf() {
        let h = shelf_block_h();
        let content = FIRST_SHELF_TOP + h + BOTTOM_PAD;
        assert_eq!(
            reveal_block(0.0, FIRST_SHELF_TOP, h, content),
            0.0,
            "focusing the first shelf scrolled the header away (block bottom {})",
            FIRST_SHELF_TOP + h
        );
    }

    /// ...and a SECOND shelf, which cannot fit alongside the first, does scroll — by exactly
    /// enough to put its own block bottom on screen, never further.
    #[test]
    fn reaching_the_second_shelf_scrolls_it_fully_into_view_and_no_further() {
        let h = shelf_block_h();
        let top = FIRST_SHELF_TOP + h + SHELF_GAP;
        let content = top + h + BOTTOM_PAD;
        let want = reveal_block(0.0, top, h, content);
        assert!(want > 0.0, "the second shelf must scroll into view");
        assert!(top + h - want <= SCR_H, "its block bottom is still off screen");
        assert!(top - want >= TOP_MARGIN, "it scrolled past the minimum — the shelf overshot upward");
    }

    /// **The bug, as geometry.** With a real header the first shelf CANNOT be revealed without
    /// scrolling, and `reveal` is MINIMAL in both directions — arriving from below it stops the
    /// moment the shelf's own block is on screen, which is still well short of the top. So while
    /// the shelves were the only focusable rows, that resting offset was a floor the page could
    /// never rise above and the portrait was gone for good. This test states the precondition, so
    /// that if the header ever shrinks back under the fold the next reader can tell that the header
    /// row stopped being load-bearing.
    #[test]
    fn a_full_header_cannot_share_the_screen_with_a_focused_first_shelf() {
        let h = shelf_block_h();
        let top = HEADER_TOP + FULL_HEADER_H + SHELF_GAP;
        // both content lengths matter: with ONE shelf the flow's own end clamps the scroll, with
        // TWO it is the shelf's lift-to-margin edge — the header stays off screen either way
        for content in [top + h + BOTTOM_PAD, top + 2.0 * h + SHELF_GAP + BOTTOM_PAD] {
            let want = reveal_block(0.0, top, h, content);
            assert!(want > 0.0, "the first shelf now fits under a full header — see the doc comment");
            assert!(HEADER_TOP - want < 0.0, "the header's top is off screen once the shelf is revealed");
            // and coming back from further down does not undo it — `reveal` is minimal from that
            // side too, so it stops at the shelf's lift-to-margin edge, still short of the top
            let back = reveal_block(content, top, h, content);
            assert!(back > 0.0, "focusing the first shelf from below reached the top by itself");
            assert!(HEADER_TOP - back < 0.0, "the header would have been on screen without a header row");
        }
    }

    /// **The fix.** Focusing the header — flow child 0, at [`HEADER_TOP`] — rests the page at 0 for
    /// ANY header height the page can produce, which is what puts the portrait back on screen. It
    /// holds from wherever the scroll happens to be, so returning from deep in the shelves lands at
    /// the top rather than part-way.
    #[test]
    fn focusing_the_header_rests_the_page_at_the_top_from_any_scroll() {
        for h in [BARE_HEADER_H, FULL_HEADER_H, FULL_HEADER_H * 1.5] {
            let content = HEADER_TOP + h + SHELF_GAP + 2.0 * (shelf_block_h() + SHELF_GAP) + BOTTOM_PAD;
            for cur in [0.0, 200.0, content] {
                assert_eq!(
                    reveal_block(cur, HEADER_TOP, h, content),
                    0.0,
                    "header h={h} did not rest the page at the top from scroll {cur}"
                );
            }
        }
    }
}
