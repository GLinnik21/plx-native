//! The person / actor page — reached by pressing OK on a detail page's cast headshot.
//!
//! **The reference is Apple TV, not Plex.** Plex's own person page is a field list (left portrait,
//! "Actor, Producer, Director", Born + age, social handles, a count-badged "Movies & Shows in
//! Media Libraries", "Known For", "Featured Videos") — that is the feature checklist, not the
//! mockup. This is the tvOS shape: a **centred circular portrait**, the **name**, a **3-line bio
//! truncated with an inline `MORE`**, then plain **`Movies`** and **`Shows`** shelves of ordinary
//! poster cards — the same [`CardRow`] tiles the home shelves, the Library grid and detail's
//! Related row draw, so a poster looks and animates identically wherever it appears.
//!
//! **The no-bio degrade is the normal case, and it is deliberate.** PMS has no biography to give
//! (see `crate::person`'s module docs), so the header is name + portrait and is COMPOSED to read
//! as finished that way: the two elements are centred as one block and the shelves follow at the
//! standard block gap, so nothing looks like a field that failed to load. When a bio does arrive
//! (a later plex.tv unit), it slots in under the name and the flow reflows around it — the whole
//! layout is measured, never a table of fixed Ys.
//!
//! Structure mirrors `detail.rs`: the page is a [`ScrollColumn`] whose children are the header
//! and the PRESENT shelves, so the block flow, the off-screen cull and the scroll spring are the
//! shared ones. Perf discipline for the A53 + Mali: shelves are drawn through
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
/// name → bio (a heading and its body text).
const BIO_GAP: f32 = theme::space::MD;
/// Bio column width. Narrower than the shelves on purpose: a 3-line block set to the full
/// 1740px content width would run ~14 words a line and read as a paragraph of legal text.
const BIO_W: f32 = 1120.0;
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
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
    /// which shelf holds focus (a KIND, not a position — a shelf that empties must not silently
    /// hand its focus to the other one's contents)
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
    /// The name, NUL-terminated once at [`open`] rather than per frame. `Label` holds a
    /// non-owning pointer, so this must outlive the draw — a field does; a temporary would not.
    name_c: CString,
}

impl Scene {
    fn new() -> Self {
        Scene {
            focus_kind: 0,
            col: [0; NSHELF],
            shelves: [CardRow::new(); NSHELF],
            column: ScrollColumn::new(HEADER_TOP, TOP_MARGIN),
            spin_ms: 0.0,
            from_rk: String::new(),
            requested: false,
            name_c: CString::default(),
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
            header_h()
        } else {
            shelf_block_h()
        }
    }
    fn gap_before(&self, _i: usize) -> f32 {
        SHELF_GAP
    }
    fn focus_child(&self) -> Option<usize> {
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

/// Measured header height: portrait + name, plus the bio block when there is one. Content-derived,
/// so the shelves below reflow the day a biography actually lands (today it never does).
fn header_h() -> f32 {
    let mut h = PORTRAIT_D + NAME_GAP + cap_h(theme::size::HERO, 1);
    if let Some(p) = crate::person::current() {
        if !p.bio.is_empty() {
            h += BIO_GAP + bio_view(&p.bio).measure_h(BIO_W);
        }
    }
    h
}

/// The bio block, in ONE place so its measure and its draw cannot disagree: 3 lines of body text
/// with the classic inline `… MORE` affordance, which `TextView` only paints when the cap actually
/// hid words.
fn bio_view(bio: &str) -> TextView<'_> {
    TextView::new(bio, theme::size::BODY, theme::TEXT_SECONDARY)
        .leading(BIO_LEAD)
        .max_lines(BIO_LINES)
        .trailing("MORE", theme::TEXT_PRIMARY)
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
/// shared [`reveal`] rule the shelves and the Library grid use, NOT a lift-to-margin pin. That
/// distinction is what keeps the page opening on its header: focusing the FIRST shelf scrolls
/// only as far as that shelf's own block needs, which on a bio-less header is not at all, whereas
/// pinning it to [`TOP_MARGIN`] would slide the portrait off the top the instant the page mounted.
///
/// Pure, and kept separate from [`scroll_target`] for exactly that reason — the flow it is given
/// is measured through the text stack, which the host suite cannot link.
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
/// `Role[]` already carries. Loads the store's header immediately, resets this screen's focus and
/// scroll, remembers the detail page to come back to, and raises the flag app.rs routes on.
pub(crate) fn open(key: &str, name: &str, thumb: &str) {
    crate::person::open(key, name, thumb);
    let sc = scene();
    sc.focus_kind = 0;
    sc.col = [0; NSHELF];
    sc.shelves = [CardRow::new(); NSHELF]; // a fresh person starts its shelves at their left edge
    sc.column.scroll.jump(0.0);
    sc.spin_ms = 0.0;
    sc.from_rk = crate::ui::detail::mounted_rk();
    sc.name_c = CString::new(name).unwrap_or_default();
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

/// The focused shelf card (None while the shelves are still out) — app.rs opens its detail page.
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    let sc = scene();
    let p = crate::person::current()?;
    p.shelf(sc.focus_kind).get(sc.col[sc.focus_kind].max(0) as usize)
}

/// Every focusable thing on this page is a poster card, so OK always takes the tvOS press (dip on
/// key-down, activate on the spring-back). Named to match the other screens' predicate so app.rs's
/// press arm reads the same for all of them.
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
    // BOTH of these reach `scene()` themselves, so they run BEFORE this function takes its own
    // `&'static mut` — holding one across a call that mints a second is aliasing UB, not a lint.
    // (`detail.rs::update` carries the same note for the same reason.)
    if crate::person::pump() {
        clamp_focus();
    }
    let sc = scene();
    sc.spin_ms += dt * 1000.0;
    let n_items = |k: usize| crate::person::current().map(|p| p.shelf(k).len()).unwrap_or(0);
    for k in 0..NSHELF {
        // a shelf that does not hold focus freezes its scroll (CardRow's `None` contract) — the
        // same behaviour a non-focused home shelf has
        let focus = (k == sc.focus_kind && n_items(k) > 0).then(|| sc.col[k].max(0) as usize);
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
        sc.focus_kind = 0;
        sc.col = [0; NSHELF];
        return;
    };
    let (kinds, n) = present(p);
    if n > 0 && !kinds[..n].contains(&sc.focus_kind) {
        sc.focus_kind = kinds[0];
    }
    for k in 0..NSHELF {
        let last = p.shelf(k).len() as c_int - 1;
        sc.col[k] = sc.col[k].clamp(0, last.max(0));
    }
}

// ---- input -----------------------------------------------------------------------------------

pub(crate) fn move_focus(sym: c_uint) {
    let sc = scene();
    let Some(p) = crate::person::current() else { return };
    let (kinds, n) = present(p);
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
/// make it that: the per-shelf `row_y` is computed ONCE outside the column loop (it goes through
/// `child_top` → `header_h` → the text cap band, which is a cache scan, so calling it per tile
/// re-measured the whole flow ~48 times per pointer motion), and the column is derived
/// arithmetically from x instead of being searched for.
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

/// Pointer hover: focus follows the cursor onto a shelf card.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if let Some((kind, i)) = tile_at(mx, my) {
        let sc = scene();
        sc.focus_kind = kind;
        sc.col[kind] = i as c_int;
    }
}

/// Pointer click: same activation as OK on the card under the cursor.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    match tile_at(mx, my) {
        Some((kind, i)) => {
            let sc = scene();
            sc.focus_kind = kind;
            sc.col[kind] = i as c_int;
            Action::Card
        }
        None => Action::None,
    }
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

/// The header: centred circular portrait, the name, and (when one exists) the 3-line bio. Drawn
/// from the child's local origin — the `ScrollColumn` has already translated to the block top.
fn draw_header(p: Painter, person: &Person, sc: &Scene) {
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

    let name_y = PORTRAIT_D + NAME_GAP;
    Label::new(sc.name_c.as_ptr(), theme::size::HERO, theme::TEXT_PRIMARY)
        .bold()
        .h(HAlign::Center)
        .v(VAlign::CapTop)
        .draw(p, Rect::new(0.0, name_y, SCR_W, 0.0));
    if !person.bio.is_empty() {
        let by = name_y + cap_h(theme::size::HERO, 1) + BIO_GAP;
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
    let y = HEADER_TOP + header_h() + SHELF_GAP - sc.column.scroll.pos;
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

    /// Seed the store the way a landing does, without a server.
    fn seed(movies: usize, shows: usize) {
        crate::person::open("161", "Idina Menzel", "");
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

    /// The header's own height needs the text stack (the name's cap band), which the host suite
    /// cannot link — so bound it instead: the name occupies at most its full point size. Every
    /// assertion below therefore holds for the WORST case, and a fortiori for the real one.
    const MAX_HEADER_H: f32 = PORTRAIT_D + NAME_GAP + theme::size::HERO as f32;
    const FIRST_SHELF_TOP: f32 = HEADER_TOP + MAX_HEADER_H + SHELF_GAP;

    /// The page opens showing its WHOLE header: with no bio, revealing the first shelf must cost
    /// no scroll at all. This is why the target is the shared minimal `reveal` and not a
    /// lift-to-margin pin — a pin would slide the portrait off the top the moment the page
    /// mounted, which is the one thing the Apple TV layout cannot survive.
    #[test]
    fn opening_the_page_never_scrolls_the_portrait_off_the_top() {
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
}
