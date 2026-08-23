//! The person / actor page — reached by pressing OK on a detail page's cast headshot.
//!
//! **The reference is `Person Screen v2.dc.html`** (the owner's design project), whose shape is
//! Apple TV's, not Plex's field list. One scroll flow, two states of one header:
//!
//! * an **asymmetric editorial band** across the top — a circular portrait on the left with a text
//!   column beside it (name at HERO, a roles kicker, a Born/Died line, a 3-line bio that dissolves
//!   into a right-pinned `MORE`), the two **centred on each other** — followed by plain **`Movies`
//!   / `Shows` shelves** of ordinary poster cards on the shared [`CardRow`] strip, each heading
//!   carrying its count.
//! * the band **CONDENSES once focus drops into the shelves** (portrait 320→160, the name steps
//!   HERO→TITLE, every other header line fades out) and expands again when focus returns. The
//!   condense is editorial, not load-bearing: v2's side-by-side packing already fits the first
//!   shelf under the EXPANDED band (v1's stacked column did not — that is why the header became a
//!   focus row in the first place), so shrinking it is about handing the room to the content the
//!   user is now looking at, exactly as the mockup stages it.
//!
//! **The band centres its two halves rather than top-aligning them** (owner call 2026-07-30, over
//! the mock's `align-items: flex-start` and its `padding-top: 14px`). That pad was a CSS artifact
//! anyway — a 72px line box carries leading above its caps, which our cap-band placement does not
//! have (`ui::label`'s layout ≠ paint rule) — but the alignment itself is the deliberate change:
//! against a 3-line bio, a top-aligned circle reads as floating beside a ragged column.
//!
//! Two rendering notes on the condense. The name cannot animate its POINT SIZE — every
//! intermediate size would rasterize a fresh glyph run into the cache — so it is drawn as a
//! crossfade between its two rung sizes while the geometry (portrait diameter, column x) rides
//! the one `cond` spring. And the whole page sits on an **ambient wash** ([`Painter::ambient`])
//! that is keyed to the focused poster's `UltraBlurColors` — the same corner data home's backdrop
//! and detail's below-hero wash draw — fading to a faint warm `ACCENT` tint while the header holds
//! focus; twelve small springs chase the corner channels so a focus change is a dissolve, not a cut.
//!
//! **Every header line below the name is optional, and the block reads as finished without them.**
//! The name + portrait arrive with the page (the cast row already held them); the roles/dates/bio
//! come from plex.tv (`crate::person`, `plex/discover.rs`) and may never come at all. Each line is
//! drawn only when it has content and the measured flow centres what is present, so the degenerate
//! page — portrait, name, shelves — is composed, not broken. No placeholder, no header spinner.
//!
//! Structure mirrors `detail.rs`: the page is a [`ScrollColumn`] whose children are **the header**
//! and the PRESENT shelves. The header is still a **focus row** (child 0): reachability no longer
//! forces it (see above), but it is what UP from the first shelf lands on, it is the page's top
//! scroll position, and it is the state the condense keys off. Perf discipline for the A53 + Mali:
//! shelves draw through [`card_row::strip`] (`on_axis`-culled, so only VISIBLE tiles touch
//! `resolve_tex`), the header flow is measured once per store change (never per frame), and
//! `crate::person` caps each shelf at a `CardRow`'s spring count.
#![allow(non_upper_case_globals)]
use crate::person::Person;
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{AmbientWash, Art, Spinner, StatusKind, StatusOverlay};
use crate::ui::{card_row::reveal, Column, Env, Painter, Rect, ScrollColumn, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry (a measured flow; every Y below is derived, none is authored) -------------------

/// Shelves, in flow order. Kind 0 = Movies, 1 = Shows — re-exported from the store, so an index
/// means one thing across the store, this focus model, the headings and the hit-test.
use crate::person::NSHELF;
/// Headings as C-string LITERALS, not `&str` — they are drawn every frame, and `CString::new`
/// per shelf per frame is a heap allocation the draw path does not need (the same reason
/// `detail.rs` writes `c"Related"` / `c"Cast & Crew"`).
const SHELF_TITLE: [&std::ffi::CStr; NSHELF] = [c"Movies", c"Shows"];

/// Portrait diameter, EXPANDED (the header holds focus) and CONDENSED (focus is in the shelves) —
/// the v2 band's two states; the `cond` spring interpolates between them.
const PORTRAIT_EXP: f32 = 320.0;
const PORTRAIT_CON: f32 = 160.0;
/// ...and expanded beside a header that is ONLY a name — the degenerate page (no internet, or a
/// person plex.tv has never heard of). Smaller on purpose; see [`header_flow`].
const PORTRAIT_BARE: f32 = 220.0;
/// The condensed band is exactly the condensed portrait: nothing but the name shares it.
const H_CON: f32 = PORTRAIT_CON;
/// Requested from the image transcoder at 300×300 — the SAME size the detail page's cast circles
/// ask for, so arriving here from a headshot is a poster-cache HIT, not a fresh round trip through
/// `/photo/:/transcode` (a 320px draw of a 300px photo is invisible; a second cache slot is not).
const PORTRAIT_RES: (c_int, c_int) = (300, 300);
/// First child's pre-scroll top — the page's top air above the band (`Person Screen v2.dc.html`'s
/// `margin-top: 96px`). It is also [`TOP_MARGIN`], as it is in the mock: the same number the page
/// rests its top content at is the highest a focused shelf may lift to.
const HEADER_TOP: f32 = 96.0;
/// portrait → the text column: the band's one internal major gap (the mock's `gap: 64px`).
const BAND_GAP: f32 = theme::space::XL;
/// name → the roles kicker (the mock's `margin-top: 24px`).
const META_GAP: f32 = theme::space::MD;
/// the kicker → the Born/Died line. One meta stack, so the tight rung.
const LIFE_GAP: f32 = theme::space::SM;
/// the meta stack → bio: a whole block break, and the mock widened it to 40 with the count column
/// gone — the bio is the band's bottom half now, not a footnote under the dates.
const BIO_GAP: f32 = theme::space::LG;
/// a shelf heading → its item count, inline.
const SHELF_COUNT_GAP: f32 = theme::space::SM;
/// Bio column width — the mock's `max-width: 1180px`, which is what the text column narrows to now
/// that nothing sits to its right. The name and the two meta lines take the same budget, so the
/// whole text column has ONE measure and a long birthplace cannot outrun the prose under it.
const BIO_W: f32 = 1180.0;
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
/// The selectable-block mark's inset around the bio prose — `detail.rs`'s About columns spend the
/// same 26 on x, so a block that can be opened is padded identically on both pages. The vertical
/// pad is even (the About columns' 36/24 pair is asymmetric because their 36 clears a HEADING's cap
/// band; this block has no heading, so an even box is what hugs it).
const HL_PAD_X: f32 = 26.0;
const HL_PAD_Y: f32 = 24.0;
/// The bio's truncation mark, as a C literal — it is drawn every frame the bio is cut off, and the
/// SAME pointer the reserved zone is measured from, so the gap and the mark can never end up being
/// two different strings (`detail.rs`'s About card writes `c"MORE"` twice for want of one).
const MORE: &std::ffi::CStr = c"MORE";
/// Air between the point the bio's dissolving last line has vanished and the left edge of [`MORE`].
/// **Re-derived for this rung, not copied.** detail's About card reserves 36 px beside a
/// `size::CAPTION` synopsis — 1.5× its own type size — and the bio is a rung larger
/// (`size::BODY`), where the same proportion is 42; `theme::space::LG` is the ladder rung nearest
/// that. An inline gap comes off the space ladder here exactly as [`SHELF_COUNT_GAP`] does, never
/// off a literal tuned against another screen's type size.
const BIO_MORE_GAP: f32 = theme::space::LG;
/// Block gap between the header and a shelf, and between shelves — detail.rs's `SECTION_GAP`.
const SHELF_GAP: f32 = theme::space::XL;
/// Shelf heading → poster row. Matches detail's Related row, so a shelf is paced identically on
/// both pages. (The band BELOW the row is not a constant here — see [`shelf_block_h`].)
const SHELF_LABEL_H: f32 = 46.0;
/// The shelves' own row style: [`RowStyle::HOME`]'s motion and geometry verbatim — one source, so a
/// poster here animates exactly as it does on Home — plus the **two-line** title a shelf of real
/// Plex names needs (one elided line loses two thirds of "Wallace & Gromit: The Curse of the
/// Were-Rabbit").
const SHELF_STYLE: RowStyle = RowStyle { title_lines: 2, ..RowStyle::HOME };
/// A focused shelf lifts no higher than this from the screen top — [`HEADER_TOP`]'s twin.
const TOP_MARGIN: f32 = HEADER_TOP;
/// Air kept under the lowest visible content when a shelf is revealed by scrolling.
///
/// The OVERSCAN keep-out rather than a `space` rung (it was `LG` 40): what this bounds is a focused
/// tile's own caption against the bottom edge of the panel, so the number that belongs here is the
/// safe area's, not a gap from the spacing ladder.
const BOTTOM_PAD: f32 = crate::ui::consts::MARGIN_Y;

/// The condense spring's rate — the scroll rate, because the band's resize IS a scroll-scale
/// motion (the mock gives both the same `.42s` curve).
const K_COND: f32 = K_SCROLL;
/// Ambient corner weights (tl, tr, br, bl — [`Painter::ambient`]'s order), i.e. how far each
/// corner leans from the app surface toward the source colour. Header state: a faint warm
/// `ACCENT` wash, strongest top-left (the mock's `radial at 18% 0%`). Card state: the focused
/// poster's UltraBlur corners, strong across the top and nearly gone by the bottom (the mock's
/// `.30 → .07` stops, trimmed a step on-device because our wash is opaque, not additive) — the
/// top pair is [`AmbientWash::GROUND_W`], the one strength every item-keyed wash in the app leans
/// by, spelled as the shared constant rather than as its value so the two cannot drift apart.
const AMB_HEADER_W: [f32; 4] = [0.10, 0.06, 0.02, 0.03];
const AMB_CARD_W: [f32; 4] = [AmbientWash::GROUND_W, AmbientWash::GROUND_W, 0.08, 0.08];

// ---- screen state ----------------------------------------------------------------------------

struct Scene {
    /// **The header holds focus** (flow child 0) rather than any shelf. It is the page's top
    /// position, what UP from the first shelf returns to, and the state the band's condense keys
    /// off — see [`move_focus`].
    on_header: bool,
    /// which shelf holds focus (a KIND, not a position — a shelf that empties must not silently
    /// hand its focus to the other one's contents). Meaningless while [`Scene::on_header`].
    focus_kind: usize,
    /// per-shelf focus memory: leaving a shelf and coming back restores the tile you were on
    col: [c_int; NSHELF],
    shelves: [CardRow; NSHELF],
    column: ScrollColumn,
    /// The band's state spring: 0 = expanded (header focused), 1 = condensed. ONE spring drives
    /// the portrait diameter, the band height, the column x and every crossfade, so no two of
    /// them can ever be mid-transition in different places.
    cond: Spring,
    /// The ambient wash, dissolving toward [`amb_target`] — what makes a focus change wash over the
    /// page instead of cutting. The springs and the "a wash cannot be alpha-faded" reasoning live in
    /// the shared [`AmbientWash`], not here.
    amb: AmbientWash,
    spin_ms: f32,
    /// a cast headshot was just activated; app.rs takes this and routes (see [`take_request`])
    requested: bool,
    /// The header's text runs, NUL-terminated ONCE per store change (see [`refresh_header`]) —
    /// never per frame. `Label` holds a non-owning pointer, so these must outlive the draw: a
    /// field does, a temporary would not. This keeps the HEADER's draw allocation-free; the strip
    /// beneath it allocates exactly the focused tile's two lines per frame, which is the shared
    /// `card_row::strip` contract every shelf in the app draws under.
    name_c: CString,
    roles_c: CString,
    life_c: CString,
    /// the per-shelf heading counts — digits change only when a landing does, so they are baked
    /// with the rest of the runs rather than formatted per frame
    shelf_count_c: [CString; NSHELF],
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
            cond: Spring::at(0.0),
            amb: AmbientWash::flat(theme::SURFACE_APP),
            spin_ms: 0.0,
            requested: false,
            name_c: CString::default(),
            roles_c: CString::default(),
            life_c: CString::default(),
            shelf_count_c: [CString::default(), CString::default()],
            header: HeaderFlow::default(),
            header_dirty: true,
        }
    }
    /// The band's CURRENT height — the expanded measured height eased toward [`H_CON`] by the
    /// condense spring. This is `Column::height(0)`, so the shelf tops, the scroll targets and the
    /// pointer hit-test all follow the resize for free.
    fn band_h(&self) -> f32 {
        let c = self.cond01();
        self.header.exp_h + (H_CON - self.header.exp_h) * c
    }
    /// The condense scalar, clamped — 0 expanded, 1 condensed. Read through this, never off the
    /// spring directly, so the band height and every crossfade provably share one number.
    fn cond01(&self) -> f32 {
        self.cond.pos.clamp(0.0, 1.0)
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

/// The tile holding focus, WITHOUT touching the scene singleton — for callers that already hold
/// `&Scene` (minting a second `&'static mut` through [`scene`] while one is live is aliasing UB;
/// `update` carries the same note).
fn focused_of<'a>(p: &'a Person, sc: &Scene) -> Option<&'a PmsMovie> {
    if sc.on_header {
        return None;
    }
    p.shelf(sc.focus_kind).get(sc.col[sc.focus_kind].max(0) as usize)
}

// ---- the scroll flow (ScrollColumn children: 0 = header, 1.. = present shelves) ---------------

impl Column for Scene {
    fn len(&self) -> usize {
        crate::person::current().map(|p| 1 + present(p).1).unwrap_or(1)
    }
    fn height(&self, i: usize) -> f32 {
        if i == 0 {
            self.band_h()
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

/// Where each header line sits inside the EXPANDED band, and how tall the band is — the ONE place
/// the header flow is computed, so the measure and [`draw_header`] cannot disagree about where a
/// line goes. Every text entry is `None` unless its content exists: a person with no kicker has no
/// gap where one would have been, which is what makes a sparse header read as composed rather than
/// as fields that failed to load. The portrait and the text stack are EACH vertically centred in
/// the band (`exp_h` = whichever is taller), which is also what centres the degenerate
/// portrait-plus-name page with no special case.
///
/// **Computed once per store change, not per frame** ([`Scene::header`]). It is the only thing on
/// this page that measures text, and it is reached from `Column::height(0)` — which `child_top`,
/// `content_h`, `scroll_target` and the pointer hit-test all call, several times a frame each.
#[derive(Clone, Copy, Default)]
struct HeaderFlow {
    /// the expanded band height — `max(exp_d, text stack)`
    exp_h: f32,
    /// the EXPANDED portrait diameter: [`PORTRAIT_EXP`] normally, the smaller [`PORTRAIT_BARE`] when
    /// the header is nothing but a name. Recorded here rather than re-derived in the draw, so the
    /// measured band height and the drawn circle can never disagree about which one it is.
    exp_d: f32,
    /// y offsets FROM THE BAND TOP of each present line's cap-top (expanded state)
    name_y: f32,
    meta_y: Option<f32>,
    life_y: Option<f32>,
    bio_y: Option<f32>,
}

fn header_flow(sc: &Scene) -> HeaderFlow {
    let mut f = HeaderFlow::default();
    let mut y = crate::text::cap_h(theme::size::HERO, 1); // the name; everything below stacks from its bottom
    if !sc.roles_c.as_bytes().is_empty() {
        y += META_GAP;
        f.meta_y = Some(y);
        y += crate::text::cap_h(theme::size::LABEL, 0);
    }
    if !sc.life_c.as_bytes().is_empty() {
        // the life line hugs the kicker above it, but takes the kicker's own gap when it is the
        // first meta line — the gap belongs to the stack's top edge, not to a particular line
        y += if f.meta_y.is_some() { LIFE_GAP } else { META_GAP };
        f.life_y = Some(y);
        y += crate::text::cap_h(theme::size::LABEL, 0);
    }
    let bio = crate::person::current().map(|p| p.bio.as_str()).unwrap_or("");
    if !bio.is_empty() {
        y += BIO_GAP;
        f.bio_y = Some(y);
        y += bio_view(bio, 1.0).measure_h(BIO_W);
    }
    // A header that is nothing BUT a name wears the smaller portrait: 320px of headshot beside one
    // line reads as a portrait with a caption.
    let bare = f.meta_y.is_none() && f.life_y.is_none() && f.bio_y.is_none();
    f.exp_d = if bare { PORTRAIT_BARE } else { PORTRAIT_EXP };
    f.exp_h = y.max(f.exp_d);
    // **The text block is CENTRED on the portrait** — owner call, 2026-07-30, over the mock's
    // `align-items: flex-start`. Centring BOTH in the band is what makes their centres coincide,
    // whichever is taller: the text block for a normal header (the 320 circle wins), the circle for
    // a header with an unusually long bio. Top-alignment only looks deliberate when the text block
    // ends near the portrait's bottom too, and a 3-line bio does not — it left the circle floating
    // against a ragged column.
    let ty = (f.exp_h - y) * 0.5;
    f.name_y = ty;
    for v in [&mut f.meta_y, &mut f.life_y, &mut f.bio_y].into_iter().flatten() {
        *v += ty;
    }
    f
}

/// The bio block, in ONE place so its measure and its draw cannot disagree: 3 lines of body text
/// that **dissolve into a right-pinned [`MORE`]** when the cap actually hid words. The affordance
/// is a truncation MARK, not a control — nothing on this page expands text in place. `a` fades it
/// with the rest of the expanded header.
///
/// **The reserve is all this view owns of the affordance; the mark itself is drawn by
/// [`draw_header`].** That is `fade_last`'s shape, and it is the About card's — the last line is
/// painted through the fade shader (`shaders/fs_text_fade.frag`) so it vanishes *before* the
/// column's right edge, and the word is then pinned to that edge on the same cap band. It replaced
/// an inline `.trailing("MORE", …)`, which hard-elided the last line with an ellipsis and floated
/// the word at the text's own ragged end: one page of this app cut its prose off with `…`, the
/// other let it fade, for the same reason and one rung apart.
fn bio_view(bio: &str, a: f32) -> TextView<'_> {
    TextView::new(bio, theme::size::BODY, theme::with_a(theme::TEXT_SECONDARY, a))
        .leading(BIO_LEAD)
        .max_lines(BIO_LINES)
        .fade_last(more_w() + BIO_MORE_GAP)
}

/// [`MORE`]'s drawn width at the bio's own rung and weight — `size::BODY` bold, NOT the About
/// card's `size::CAPTION`. Only the reserve reads it: the mark is drawn right-ALIGNED on the
/// column edge, so its own draw needs no width at all.
fn more_w() -> f32 {
    crate::text::text_width(MORE.as_ptr(), theme::size::BODY, 1)
}

/// Rebuild the header's cached text runs from the store — from [`update`], on the frame a store
/// change lands. They are `format!`s over store fields, and doing that 60 times a second for
/// strings that change twice in a page's life is exactly the per-frame allocation the shelf tiles
/// avoid.
///
/// **The life line is a stack of independent facts, joined only where both exist**: a living person
/// simply has no Died clause, a person with only a death date reads "Died 2 Jun 2017", and someone
/// plex.tv knows nothing about produces empty strings, which the flow then gives no room to at all.
/// That is why this builds a LIST and joins it rather than filling one template with holes.
fn remeasure_header(sc: &mut Scene) {
    refresh_runs(sc);
    // the flow MEASURES the runs above, so it can only be computed after them — which is why the
    // two live behind one entry point instead of two calls a caller must order correctly
    sc.header = header_flow(sc);
    sc.header_dirty = false;
}

fn refresh_runs(sc: &mut Scene) {
    let Some(p) = crate::person::current() else {
        sc.roles_c = CString::default();
        sc.life_c = CString::default();
        sc.shelf_count_c = [CString::default(), CString::default()];
        return;
    };
    // The name is rebuilt (and budgeted) HERE, not at `open` — it is the one header run every page
    // has, and the only one that would otherwise draw unmeasured: a HERO-size "Daniel Michael Blake
    // Day-Lewis" runs past 1300px. Same column budget as everything under it.
    sc.name_c = cstr_elide(&p.name, BIO_W, theme::size::HERO, 1);
    sc.roles_c = cstr_elide(&p.roles, BIO_W, theme::size::LABEL, 0);

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
    sc.life_c = cstr_elide(&life.join(" \u{b7} "), BIO_W, theme::size::LABEL, 0);

    // Each heading's count prints the RESPONSE total, not the tile list's length: the shelves cap at
    // a `CardRow`'s spring count, and "24" on a person with 60 movies would be the cap posing as a
    // fact about the person.
    for k in 0..NSHELF {
        sc.shelf_count_c[k] = match p.total(k) {
            0 => CString::default(),
            n => CString::new(n.to_string()).unwrap_or_default(),
        };
    }
}

/// A header meta line, NUL-terminated and clipped to `w`. `Painter` has no clip, so an over-long
/// line (a birthplace with four commas in it) would otherwise paint out past the bio beneath it;
/// `text::elide` is the shared measurer that cuts it at the same width. `cont` is FALSE — that
/// flag is `TextView`'s continued-line mode and marks the run with an ellipsis even when nothing
/// was cut, which put a stray `…` on every kicker and every Born/Died line that simply fit.
fn cstr_elide(s: &str, w: f32, sz: c_int, bold: c_int) -> CString {
    if s.is_empty() {
        return CString::default();
    }
    CString::new(crate::text::elide(s, w, sz, bold, false)).unwrap_or_default()
}

/// Total flowed content height (last child's bottom + the bottom air).
fn content_h(sc: &Scene) -> f32 {
    let col = sc.column;
    let last = sc.len().saturating_sub(1);
    col.child_top(sc, last) + sc.height(last) + BOTTOM_PAD
}

/// A shelf block's flowed height: heading + poster row + the focused tile's label band.
///
/// The band is ASKED FOR, not restated — `card_row` lays that block out and
/// [`card_row::TileLabel::height`] is its only authority on how tall it is. Getting it wrong is not
/// cosmetic: [`reveal_block`] guarantees [`BOTTOM_PAD`] under the block a screen DECLARES, so a
/// short declaration runs the character caption off the bottom of the panel — which is exactly what
/// a hand-authored 112 did here, because it assumed the mock's 24px poster→title drop where the
/// shared code uses 30.
fn shelf_block_h() -> f32 {
    SHELF_LABEL_H + CARD_H + card_row::TileLabel::height(&SHELF_STYLE, true)
}

/// The MINIMAL scroll that reveals the block `[top, top+h)` inside `content` px of flow — the
/// shared [`reveal`] rule the shelves and the Library grid use, NOT a lift-to-margin pin: a block
/// scrolls only as far as its own extent needs, and not at all when it already fits.
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

// ---- the ambient wash ------------------------------------------------------------------------

/// The wash's target corners (tl, tr, br, bl), each mixed from the app surface toward a source
/// colour: the focused poster's UltraBlur corners while a card holds focus, else the faint warm
/// header tint. A tile whose art carried no blur envelope keeps the header tint too — a wash
/// invented from nothing would flash grey on one poster in a row of colour.
///
/// Mixing FROM [`theme::SURFACE_APP`] is what makes "no artwork" the app's own flat ground with no
/// special case, and it is why the corner weights below can be read as "how much of this poster
/// shows through". That mix is [`AmbientWash::target`]'s contract now, not a loop written here.
///
/// The card corners go through [`AmbientWash::keyed`], so a white poster cannot outshine the role
/// captions sitting on the ground under it — see `widgets::GROUND_LUMA`. The header tint is a
/// palette token we chose, so it needs no cap.
fn amb_target(sc: &Scene) -> [[f32; 4]; 4] {
    match crate::person::current().and_then(|p| focused_of(p, sc)).filter(|m| m.has_blur) {
        Some(m) => AmbientWash::keyed(m.blur, AMB_CARD_W),
        None => AmbientWash::target([theme::WASH_WARM; 4], AMB_HEADER_W),
    }
}

// ---- entry / exit ----------------------------------------------------------------------------

/// Mount the page for a cast member — everything `Role[]` already carries (`key` = the local
/// personId, `guid` = the `tagKey` plex.tv answers to). Loads the store's header immediately and
/// resets this screen's focus / scroll / band state, WITHOUT raising the routing latch.
///
/// The shared half of [`open`] and of the BACK trail's re-entry (`app.rs`'s `enter_node`), which
/// must not raise the latch: it is already doing the routing, and a latch left set there would be
/// drained on the same frame and push back the very node the pop just took off.
///
/// **The page opens on its HEADER** — expanded, at scroll 0 — because the header is the page's
/// subject until the user asks for the shelves. There is nothing focusable in it (it is a scroll
/// POSITION, see [`move_focus`]), and nothing to focus on the shelves yet either: they land a
/// moment later, and DOWN goes to them.
pub(crate) fn reopen(sid: crate::plex::ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    crate::person::open(sid, key, guid, name, thumb);
    let sc = scene();
    // A WHOLESALE reset, not a field-by-field one: every default this page mounts with — focus on
    // the header, scroll and condense at 0, shelves at their left edge, EMPTY text runs — is already
    // spelled in `Scene::new`, and a field added there must not need remembering here too. The runs
    // in particular must start empty so the previous person's dates cannot survive into this one for
    // the length of a fetch; `header_dirty` then has `remeasure_header` build them on the next
    // `update`, which keeps the page's only text measure off `detail::on_ok`'s input path.
    *sc = Scene::new();
    // the wash starts AT the header tint — the previous person's poster colours must not dissolve
    // across the new page's mount
    let k = amb_target(sc);
    sc.amb.jump(k);
}

/// [`reopen`] plus the flag app.rs routes on — the INTERACTIVE entry, called from `detail.rs`'s
/// cast-row OK arm.
pub(crate) fn open(sid: crate::plex::ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    reopen(sid, key, guid, name, thumb);
    scene().requested = true;
}

/// app.rs polls this right after a detail-page OK: true exactly once per [`open`]. Keeping the
/// request here (rather than a second return channel out of `detail::on_ok`) is what lets the
/// cast arm stay four lines in a file eight other changes are landing in.
pub(crate) fn take_request() -> bool {
    let sc = scene();
    std::mem::replace(&mut sc.requested, false)
}

/// BACK: leave the page. The page UNDERNEATH is put back by `app.rs`'s BACK trail (`ui::trail`),
/// which knows the whole history — this used to re-open a single remembered `from_rk`, which was
/// right for exactly one level and lost person → detail → person entirely (the second BACK went to
/// Home rather than to the first actor).
pub(crate) fn leave() {
    // drop any un-consumed request: `requested` is a LATCH, and one left set here would fire on
    // some unrelated OK several screens later
    scene().requested = false;
    // the bio panel is THIS page's, so it dies with it — a panel left open would come back up over
    // whatever the trail puts here next, with the previous person's biography still in it
    crate::ui::person_bio::close();
    crate::person::close();
}

/// A panel this SCREEN has open takes the BACK press, and the page stays — `detail::back()`'s shape
/// one screen over, and for the reason that one records: a panel is part of the screen, so leaving
/// the screen must not be how you close it. `false` means there was nothing of ours to close and
/// `app.rs` should pop the BACK trail.
pub(crate) fn back() -> bool {
    if crate::ui::person_bio::is_open() {
        crate::ui::person_bio::close();
        return true;
    }
    false
}

/// **Is there more biography than the header shows?** — the ONE gate on the bio panel, and it is
/// deliberately the same call the truncation MARK is drawn from ([`draw_header`]), sharing the same
/// memoised wrap.
///
/// Written as one predicate rather than repeated at the two call sites because the mark and the
/// panel must never disagree: a panel reachable on a bio that fits would open on the words the user
/// has just finished reading, and a `MORE` with nothing behind it is worse still.
pub(crate) fn bio_is_truncated() -> bool {
    crate::person::current()
        .map(|p| !p.bio.is_empty() && bio_view(&p.bio, 1.0).truncates(BIO_W))
        .unwrap_or(false)
}

/// OK while the HEADER holds focus. Returns whether the press was spent.
///
/// **This is the bio panel's entry point, and it is why the header's OK is no longer inert.** The
/// header is already a focus row (flow child 0) but contains no CONTROL — nothing in it draws a
/// focus ring, `focus_is_card` is false there, and `app.rs` therefore arms no tvOS press on it. So
/// this acts on the key DOWN rather than on a press spring-back: there is no card to dip, and
/// waiting ~150ms to animate a dip nobody can see would only add latency.
///
/// The alternative — making the bio block a real focusable inside the band — was rejected against
/// the page as built, and still is. It would put a second focus stop inside a row this module
/// documents four times over as a scroll POSITION rather than a selection, and give the condense a
/// state it has no mock for. Here the whole header band is the target: focus is already at the top
/// of the page, and OK there reads the rest.
///
/// **What that rejection used to include, and no longer does, is the MARK.** The argument ran that
/// a focus mark on the prose would turn `MORE` into a control — but the band was then the only
/// focusable thing in the app that drew nothing at all for holding focus, so the block OK acts on
/// was indistinguishable from prose that ends in a word. [`draw_header`] now draws the shared
/// `widgets::text_block_highlight` under the bio on exactly this function's predicate. `MORE` is
/// unchanged and is still a mark; the lift is the page saying which block the press will reach.
pub(crate) fn header_ok() -> bool {
    if !scene().on_header || !bio_is_truncated() {
        return false;
    }
    crate::ui::person_bio::open();
    true
}

/// The focused shelf card (None while the shelves are still out, and None while the HEADER holds
/// focus — it contains no control) — app.rs opens its detail page.
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    let sc = scene();
    let p = crate::person::current()?;
    focused_of(p, sc)
}

/// The only focusABLE thing on this page is a poster card, so OK always takes the tvOS press (dip
/// on key-down, activate on the spring-back). Named to match the other screens' predicate so
/// app.rs's press arm reads the same for all of them.
///
/// False on the header row: it carries no card to dip. OK there is not inert any more — it opens
/// the bio panel ([`header_ok`]) — but that is an overlay rather than an activation, so it takes no
/// press. And false while the bio panel is UP, for `detail::focus_is_card`'s reason: without it an
/// OK held over the panel would begin a tvOS press on a tile nobody can see and commit it on
/// release, opening a detail page from behind a modal.
pub(crate) fn focus_is_card() -> bool {
    !crate::ui::person_bio::is_open() && focused_item().is_some()
}

pub(crate) fn on_ok() -> Action {
    // nothing behind an open panel is activatable — the panel's own OK is inert (it is a reader,
    // not a chooser), so the press is simply swallowed here
    if crate::ui::person_bio::is_open() {
        return Action::None;
    }
    if focused_item().is_some() {
        Action::Card
    } else {
        Action::None
    }
}

// ---- update ----------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    // The focused card's IDENTITY, taken BEFORE the pump. The shelves are a merge across every
    // source now (`person::merge_shelves`), and a landing re-divides the row's budget between them
    // — so a bare column index silently comes to mean a different film the moment a slow share
    // answers. Before this, a share landing two seconds in slid the card out from under the user's
    // focus and the next OK opened something they were not looking at.
    let was = focused_id();
    // `clamp_focus` and the dirty flag below both reach `scene()` themselves, so they run BEFORE
    // this function takes its own `&'static mut` — holding one across a call that mints a second is
    // aliasing UB, not a lint. (`detail.rs::update` carries the same note for the same reason.)
    if crate::person::pump() {
        reseat(was);
        clamp_focus();
        scene().header_dirty = true; // a landing changes the header runs AND the flow
    }
    let sc = scene();
    // Remeasure BEFORE anything reads the flow this frame: `scroll_target` below, and every draw
    // after it, go through `Column::height(0)`, which is now a plain field read.
    if sc.header_dirty {
        remeasure_header(sc);
    }
    sc.spin_ms += dt * 1000.0;
    // the band: expanded while the header holds focus, condensed the moment it leaves
    sc.cond.step(if sc.on_header { 0.0 } else { 1.0 }, K_COND, dt);
    // the wash: dissolve toward whatever the page is about now
    let k = amb_target(sc);
    sc.amb.step(k, AmbientWash::K, dt);
    let n_items = |k: usize| crate::person::current().map(|p| p.shelf(k).len()).unwrap_or(0);
    for k in 0..NSHELF {
        // a shelf that does not hold focus freezes its scroll (CardRow's `None` contract) — the
        // same behaviour a non-focused home shelf has. The header holding focus freezes them ALL,
        // which is what a page scrolled back to its top should do.
        let n = n_items(k);
        let focus = (!sc.on_header && k == sc.focus_kind && n > 0).then(|| sc.col[k].max(0) as usize);
        sc.shelves[k].update(n, focus, &SHELF_STYLE, dt);
    }
    let want = scroll_target(sc);
    sc.column.scroll.step(want, K_SCROLL, dt);
    // the bio panel's own appear + scroll springs. Unconditional, like every other popover's
    // `update` — a closed one steps nothing.
    crate::ui::person_bio::update(dt);
}

/// The focused card's IDENTITY — the `(server, ratingKey)` PAIR, never the bare key. The shelves
/// merge two sources into one row and both servers number their items from 1, so a bare key names a
/// card on neither of them in particular (`plex::same_item`). None while the header holds focus,
/// which is a state [`reseat`] then correctly leaves alone.
fn focused_id() -> Option<(crate::plex::ServerId, String)> {
    focused_item().map(|m| (m.sid, m.rk.clone()))
}

/// Put focus back on the card it was on, wherever a landing has since moved it.
///
/// [`clamp_focus`] cannot do this: it range-clamps an INDEX, and the index is exactly what stops
/// meaning the same thing when `person::merge_shelves` re-divides the row between the sources. A
/// no-op when the card is gone (its source dropped out, or the cap pushed it off the row) — the
/// clamp that follows then does what it always did.
fn reseat(was: Option<(crate::plex::ServerId, String)>) {
    let Some((sid, rk)) = was else { return };
    let Some(p) = crate::person::current() else { return };
    for kind in 0..NSHELF {
        let found = p.shelf(kind).iter().position(|m| crate::plex::same_item((m.sid, &m.rk), (sid, &rk)));
        if let Some(i) = found {
            let sc = scene();
            sc.focus_kind = kind;
            sc.col[kind] = i as c_int;
            return;
        }
    }
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
/// `detail.rs` uses (its section 0). The header row predates the v2 band (it was what made the
/// portrait reachable at all when the v1 header outgrew the fold) and the condense gives it its
/// second job: it is the state the band expands for. UP into it is a scroll + expand, not a
/// selection — nothing draws a focus ring, and DOWN returns to the shelf that had it.
pub(crate) fn move_focus(sym: c_uint) {
    // The bio panel takes the nav keys while it is up — focus is TRAPPED in it, as it is in every
    // other popover this app opens (`detail::move_focus` carries the same three lines for the
    // "Also available" sheet). Without this the shelves would walk under the sheet and the page
    // would scroll behind it.
    if crate::ui::person_bio::is_open() {
        crate::ui::person_bio::move_focus(sym);
        return;
    }
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
/// **None while the bio panel is up**, which gates hover AND click in one place rather than in the
/// two callers: a modal owns the frame, so a cursor wandering over the page behind it must neither
/// light a card up nor activate one. (Its own hit test is the panel's; today it has none — the
/// panel is read with the D-pad and dismissed with BACK.)
///
/// O(VISIBLE), deliberately — the same discipline `library.rs::cell_at` documents. Two things
/// make it that: the per-shelf `row_y` is computed ONCE outside the column loop (calling it per
/// tile walks the whole `child_top` flow ~48 times per pointer motion), and the column is derived
/// arithmetically from x instead of being searched for. The flow walk is cheap in itself —
/// `Column::height(0)` reads the cached [`HeaderFlow`] through [`Scene::band_h`] rather than
/// measuring text — but the hoist stays: it is the part that keeps this O(VISIBLE).
fn tile_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    if crate::ui::person_bio::is_open() {
        return None;
    }
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
    // THE page transition, as ONE cascade-alpha push at the root — the same line `detail::draw` and
    // `home_draw` carry, and for the same reasons (see `ui::nav`). Like the detail page, this screen
    // has no continuous chrome, so the whole tree rides the page alpha; the wash included, since
    // `Painter::ambient` mixes toward `theme::SURFACE_APP` under the cascade and that is the colour
    // the clear above just laid down.
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let sc = scene();
    let env = Env::inert();
    // Forget last frame's focused tile before anything draws, and before the early return below:
    // focus on the header, or a page with no shelves yet, must leave [`FOCUS_TILE`] empty rather
    // than naming a tile this frame does not draw.
    unsafe { *addr_of_mut!(FOCUS_TILE) = None };
    if crate::person::current().is_none() {
        return;
    }
    // the ambient wash under everything — opaque, so it stands in for the clear
    sc.amb.draw(p, Rect::FULL);
    let col = sc.column;
    col.draw(sc, &env, p);
    draw_shelf_state(p, &env, sc);
    // ---- the bio panel, over everything the page just drew ------------------------------------
    // The SCRIM is the page's and is drawn HERE, before the panel — `Popover::scrim`'s rule, and on
    // this page it is also what protects the panel's fine print: the person's own `size::HERO` name
    // sits directly behind the sheet's top-left corner, and a 72px headline read through a 72%
    // frost lifts the ground under a `size::CAPTION` line past its graded contrast. Dimming the
    // page before the frost samples it is the fix; see `person_bio`'s module doc.
    crate::ui::person_bio::scrim();
    crate::ui::person_bio::draw();
}

/// The header band: the circular portrait and, centred on it, the text column (name, the roles
/// kicker, Born/Died, the 3-line bio). Drawn from the child's local
/// origin (the `ScrollColumn` has already translated to the block top) and from the SAME
/// [`header_flow`] that measured it, so a line can never draw where the flow reserved no room.
///
/// The condense: geometry (portrait diameter, band height, column x) reads the `cond` spring
/// directly; the name crossfades between its HERO and TITLE runs (a point size cannot animate —
/// every intermediate size would rasterize a new glyph run); every other expanded line simply
/// fades with `1 - cond`, exactly the mock's `bioOpacity`.
fn draw_header(p: Painter, person: &Person, sc: &Scene) {
    let flow = sc.header;
    let c = sc.cond01();
    let ea = 1.0 - c; // the expanded content's alpha
    let band_h = sc.band_h();
    let d = flow.exp_d + (PORTRAIT_CON - flow.exp_d) * c;
    // `Art::Person`, not `Thumb` — the SAME art case the credits shelf this page was opened from
    // draws, so a person with no headshot wears the person glyph here exactly as their circle did
    // there; and it draws that glyph only for an EMPTY key, never for a texture still resolving.
    // NEVER pixel-snapped: this is scaled art, and snapping it would fight the transcoder's
    // resampling exactly the way snapping a poster does.
    // centred in the band, which is what puts it on the text block's centre line (see `header_flow`);
    // at full condense the band IS the portrait, so this is 0 there.
    let portrait = Rect::new(MARGIN_X, (band_h - d) * 0.5, d, d);
    crate::ui::widgets::card(
        p,
        portrait,
        Art::Person { sid: person.sid, key: &person.thumb, res: PORTRAIT_RES },
        d * 0.5,
        false,
        1.0,
        0.0,
    );

    let col_x = MARGIN_X + d + BAND_GAP;
    // The NAME, as the two rungs of a crossfade — a point size cannot be tweened (see theme.rs's
    // rasterization contract), so the expanded HERO run in the measured flow and the condensed TITLE
    // run centred on the small portrait each fade as the other rises. A table, so the pair cannot
    // drift into two different labels.
    let ny = (band_h - crate::text::cap_h(theme::size::TITLE, 1)) * 0.5;
    for (sz, y, a) in [(theme::size::HERO, flow.name_y, ea), (theme::size::TITLE, ny, c)] {
        if a > 0.01 {
            Label::new(sc.name_c.as_ptr(), sz, theme::with_a(theme::TEXT_PRIMARY, a))
                .bold()
                .v(VAlign::CapTop)
                .draw(p, Rect::new(col_x, y, 0.0, 0.0));
        }
    }
    if ea <= 0.01 {
        return; // fully condensed: nothing below the name exists
    }
    // The two meta lines: the roles kicker reads as content (SECONDARY), the Born/Died line as a
    // footnote to it (TERTIARY). Both are pre-built + pre-elided runs — see `refresh_header`; the
    // mock dropped the years run from the kicker, so the life line carries every date.
    for (y, run, col) in [
        (flow.meta_y, &sc.roles_c, theme::TEXT_SECONDARY),
        (flow.life_y, &sc.life_c, theme::TEXT_TERTIARY),
    ] {
        if let Some(y) = y {
            Label::new(run.as_ptr(), theme::size::LABEL, theme::with_a(col, ea))
                .v(VAlign::CapTop)
                .draw(p, Rect::new(col_x, y, 0.0, 0.0));
        }
    }
    if let Some(by) = flow.bio_y {
        // `BIO_W` flat, NOT a width derived from the live column x: the wrap must be the one
        // `header_flow` measured (same cache entry), and a width that moved with the condense would
        // re-wrap ~3 KB of prose on every frame of the animation. The column has room for it at
        // every portrait diameter — see `BIO_W`.
        let bio = bio_view(&person.bio, ea);
        // **The block that OK opens wears the page's selectable-block mark** — the shared
        // `text_block_highlight`, i.e. the same lift the detail page's About card and its three
        // columns take. Drawn UNDER the prose, so it is a ground and not a frame around it.
        //
        // Owner call, and it narrows a decision `header_ok` records rather than reversing it: what
        // was rejected there was making the bio a SECOND FOCUS STOP inside the band, and it still
        // is not one — the whole band is one stop, and this draws no ring, no scale and no dip.
        // What it fixes is that the band was the only focusable thing in the app that showed
        // nothing for holding focus, so the one block OK acts on looked like prose that happened to
        // end in a word. `MORE` stays a mark; the lift is what says the mark can be pressed.
        //
        // Gated on `header_ok`'s condition, in its two halves: focus is here, and there is more to
        // read. A lift under a two-line bio would promise a panel `person_bio::open` is never going
        // to be asked for. The truncation half is asked of THIS view rather than through
        // `bio_is_truncated`, so the lift, the `MORE` below it and the panel behind them are one
        // expression on one memoised wrap and cannot disagree by a frame. On `sc.on_header` rather
        // than on the condense spring, because focus leaves on a keypress and the spring is still
        // at ~0 for several frames after it — a mark that faded out with the band would be
        // describing focus that had already gone.
        //
        // The box hugs the prose's own INK: `last_line_cap_y` is where the view put the last line's
        // cap band, so the height is measured to the text rather than to the flow, which ends in a
        // line of leading nothing is drawn in.
        let bh = bio.measure_h(BIO_W);
        let truncated = bio.truncates(BIO_W);
        if sc.on_header && truncated {
            let ink = bio.last_line_cap_y(by, bh) - by + crate::text::cap_h(theme::size::BODY, 0);
            crate::ui::widgets::text_block_highlight(
                p,
                Rect::new(col_x - HL_PAD_X, by - HL_PAD_Y, BIO_W + 2.0 * HL_PAD_X, ink + 2.0 * HL_PAD_Y),
            );
        }
        bio.draw(p, Rect::new(col_x, by, BIO_W, 0.0));
        // MORE — quiet grey (the About card's ink; this is a mark, not a control, and the loudest
        // thing in the band must stay the name), pinned to the bio COLUMN's right edge and sitting
        // on the last line's own cap band, which is where the line just dissolved to meet it. Both
        // halves of that placement are ASKED OF THE VIEW THAT DREW THE TEXT (`last_line_cap_y`, and
        // `Label`'s own cap-band conversion) rather than restated here — the leading and the cap
        // band are `bio`'s, not this block's. `ea` because the mark belongs to the expanded band and
        // must condense out with it.
        if truncated {
            Label::new(MORE.as_ptr(), theme::size::BODY, theme::with_a(theme::TEXT_TERTIARY, ea))
                .bold()
                .h(HAlign::Right)
                .v(VAlign::CapTop)
                .draw(p, Rect::new(col_x, bio.last_line_cap_y(by, bh), BIO_W, 0.0));
        }
    }
}


/// One `Movies` / `Shows` shelf: the heading with its item count, then the shared strip of poster
/// cards. Identical composition to detail's Related row — same `RowStyle::HOME` geometry, same
/// springs, same progress language on the tiles — plus the focused tile's SECOND line: the
/// character this person played in it ([`Person::role`]), the caption `card_row::strip` now
/// threads through.
fn draw_shelf(p: Painter, person: &Person, kind: usize, sc: &Scene) {
    let items = person.shelf(kind);
    let row = &sc.shelves[kind];
    // `-1` while the HEADER holds focus, not just while another shelf does: a shelf whose kind
    // happens to match still owns no focus then, and `strip` captions whatever column it is given.
    // Without the `on_header` term the page mounted with tile 0 already wearing its title and
    // character line — a card that reads as selected while the portrait is what the user is on.
    // (`update` already freezes these rows' springs in that state, so the pop was correctly absent,
    // which is exactly what made the stray labels look deliberate.)
    let focus_col = if !sc.on_header && sc.focus_kind == kind { sc.col[kind] } else { -1 };
    // Where this shelf's tile row lands on the PANEL — the `ScrollColumn` has already translated `p`
    // to the block top, so the number is not otherwise recoverable from inside the strip. Only
    // needed for the focused shelf, and only to record the focused tile's frame (see [`FOCUS_TILE`]).
    let screen_top = (focus_col >= 0)
        .then(|| focus_pos(person, sc).map(|pos| sc.column.child_top(sc, pos + 1) - sc.column.scroll.pos))
        .flatten();
    let hy = -row.lift();
    #[allow(unused_variables)] // `tw` is used only when the count run exists
    let tw = Label::new(SHELF_TITLE[kind].as_ptr(), theme::size::HEADLINE, theme::TEXT_HEADING)
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(MARGIN_X, hy, SCR_W, 0.0));
    if !sc.shelf_count_c[kind].as_bytes().is_empty() {
        // the count sits ON THE HEADING'S BASELINE — two sizes cap-top-aligned would leave the
        // smaller run floating above the line the eye reads
        let tw = crate::text::text_width(SHELF_TITLE[kind].as_ptr(), theme::size::HEADLINE, 1);
        Label::new(sc.shelf_count_c[kind].as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .v(VAlign::Baseline)
            .draw(p, Rect::new(MARGIN_X + tw + SHELF_COUNT_GAP, hy, 0.0, crate::text::cap_h(theme::size::HEADLINE, 1)));
    }
    card_row::strip(
        p,
        row,
        items.len(),
        focus_col,
        SHELF_LABEL_H,
        (CARD_W, CARD_H),
        CARD_W + GAP,
        &SHELF_STYLE,
        SCR_W,
        |i| Art::Poster(items.get(i)),
        |i| items.get(i).and_then(|m| m.resume_frac()),
        // the focused tile's two lines: the title (wrapped to two, per SHELF_STYLE) over the
        // character this person played in it
        |i| match items.get(i) {
            Some(m) => card_row::TileLabel::titled(&m.title, person.role(kind, i)),
            None => card_row::TileLabel::default(),
        },
        // The focused tile's frame, in screen space and UNSCALED — what the press-and-hold context
        // menu is anchored beside and re-drawn into over its scrim. Recorded rather than derived,
        // for `search::HIT_R`'s reason: the horizontal offset is the scroll spring inside this
        // shelf's own `CardRow`, which the caller cannot see.
        |_, i, x, focused| {
            if let (true, Some(top)) = (focused, screen_top) {
                let r = Rect::new(x - row.scroll_x(), top + SHELF_LABEL_H, CARD_W, CARD_H);
                unsafe { *addr_of_mut!(FOCUS_TILE) = Some((kind, i, r)) };
            }
        },
    );
}

/// The focused shelf tile's kind, column and UNSCALED screen frame, recorded by [`draw_shelf`].
///
/// Cleared at the top of every [`draw`], so a frame drawn with focus on the HEADER — or with no
/// shelves at all — leaves `None` rather than the last tile that held it.
static mut FOCUS_TILE: Option<(usize, usize, Rect)> = None;

/// The focused tile's rect at its focus magnification — what the item context menu anchors beside.
/// `press::scale()` is left out for `home::focused_card_rect`'s reason.
pub(crate) fn focused_tile_rect() -> Option<Rect> {
    let (kind, i, base) = unsafe { *addr_of!(FOCUS_TILE) }?;
    Some(base.scaled(scene().shelves[kind].scale(i)))
}

/// Re-draw the focused tile ON TOP of a modal scrim — this screen's half of
/// [`crate::ui::popover::Opener`].
///
/// The page has no clip of its own (the shelves are `on_axis`-culled, not scissored) and no chrome
/// over them, so unlike Search this needs no bound: whatever the strip drew, this draws again in
/// the same place.
pub(crate) fn redraw_focused_tile() {
    crate::ui::guard(|| {
        let Some((kind, i, base)) = (unsafe { *addr_of!(FOCUS_TILE) }) else { return };
        let Some(person) = crate::person::current() else { return };
        let sc = scene();
        let Some(m) = person.shelf(kind).get(i) else { return };
        let s = sc.shelves[kind].scale(i) * crate::ui::press::scale();
        let label = card_row::TileLabel::titled(&m.title, person.role(kind, i));
        let p = Painter::root().alpha(crate::ui::nav::page_alpha());
        card_row::draw_focused(
            p,
            Art::Poster(Some(m)),
            base.scaled(s),
            s,
            &SHELF_STYLE,
            m.resume_frac(),
            &label,
        );
    });
}

/// What sits where the shelves go while there are none: a spinner until the one `/media` request
/// lands, then — only if the person really has nothing in this library — a plain line saying so.
/// Anchored on the SAME flow the shelves would occupy, so nothing jumps when they arrive.
fn draw_shelf_state(p: Painter, env: &Env, sc: &Scene) {
    if crate::person::current().map(|pp| present(pp).1 > 0).unwrap_or(true) {
        return;
    }
    // the SAME flow the first shelf would occupy — child_top(1), not a restated sum, so nothing
    // jumps when the shelves arrive and a flow change cannot leave this anchor behind
    let y = sc.column.child_top(sc, 1) - sc.column.scroll.pos;
    let band = Rect::new(0.0, y, SCR_W, CARD_H);
    if crate::person::loading() {
        // A BARE spinner, not `StatusKind::Working` — that treatment stacks a caption under the
        // dots, and there is nothing honest to write here: the header above is already complete and
        // the shelves are the only thing outstanding. Deliberate, per `StatusOverlay`'s own note
        // that the caller owns the words.
        Spinner::new(band.cx(), band.cy(), 26.0)
            .phase(sc.spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(env, p);
        return;
    }
    // ...but the settled answer IS the shared Empty read-out — a library with nothing in it is an
    // answer, not a fault, which is the distinction `StatusKind::Empty` exists to keep.
    StatusOverlay::new(band, c"Nothing from this person is in your libraries", StatusKind::Empty).draw(env, p);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn item(rk: &str) -> PmsMovie {
        PmsMovie { rk: rk.to_string(), ..Default::default() }
    }

    /// Seed the store the way a landing does, without a server — and step off the header, because
    /// the page MOUNTS there (see [`open`]) and every shelf-focus assertion below starts from
    /// the first shelf.
    fn seed(movies: usize, shows: usize) {
        seed_at_header(movies, shows);
        move_focus(SDLK_DOWN);
    }

    /// [`seed`] without the opening DOWN — for the tests that are about the header row itself.
    ///
    /// Mounts through [`super::reopen`], not `crate::person::open` directly, because `reopen` is
    /// where the SCENE singleton is reset. The data-layer call alone left the previous test's
    /// `col[]`/focus in place — `clamp_focus` clamps but never zeroes — so which column a fresh
    /// page "opened" on depended on which test the serial lock ran before this one.
    fn seed_at_header(movies: usize, shows: usize) {
        super::reopen(
            crate::plex::ServerId::UNSET,
            "161",
            "5d77682aeb5d26001f1de4b0",
            "Idina Menzel",
            "",
        );
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

    /// UP from the FIRST shelf must reach the header — the page's top row and the state the band
    /// expands for — while UP from a LOWER shelf still walks one shelf at a time and never
    /// teleports past them.
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
        assert!(scene().on_header, "UP from the first shelf must reach the header");
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

    /// **The card under focus must stay the same CARD when a source lands**, not the same index.
    /// The shelves are a merge, and `person::merge_shelves` re-divides the row's budget every time a
    /// server answers — so the film at column 3 a moment ago can be a different film now. Before
    /// [`reseat`], a share landing two seconds into the page slid the selection out from under the
    /// user and the next OK opened something they were not looking at.
    #[test]
    fn a_landing_that_re_divides_the_shelf_keeps_focus_on_the_same_card() {
        let _serial = crate::testlock::serial();
        seed(4, 0);
        move_focus(SDLK_RIGHT);
        move_focus(SDLK_RIGHT); // on "m2"
        let was = focused_id();
        assert_eq!(was.as_ref().map(|(_, rk)| rk.as_str()), Some("m2"));

        // the row is rebuilt with two rows inserted ahead of it — what a second source landing does
        let rebuilt: Vec<PmsMovie> =
            ["x0", "x1", "m0", "m1", "m2", "m3"].iter().map(|rk| item(rk)).collect();
        crate::person::install_for_test(rebuilt, Vec::new());
        reseat(was);
        clamp_focus();
        assert_eq!(scene().col[0], 4, "the index followed the card, instead of the card following the index");
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("m2".to_string()));

        // a card that is gone entirely leaves the clamp to do what it always did — never a panic
        let was = focused_id();
        crate::person::install_for_test(vec![item("z0")], Vec::new());
        reseat(was);
        clamp_focus();
        assert_eq!(focused_item().map(|m| m.rk.clone()), Some("z0".to_string()));
        crate::person::close();
    }

    /// `leave()` must drop the un-consumed request. `requested` is a LATCH — `detail::on_ok`'s cast
    /// arm raises it and only app.rs's per-frame drain lowers it — so one left set here fires on an
    /// unrelated OK several screens later and teleports the user onto an actor page they never
    /// asked for.
    #[test]
    fn leaving_drops_the_un_consumed_request() {
        let _serial = crate::testlock::serial();
        seed(2, 0);
        scene().requested = true;

        leave();

        assert!(!take_request(), "the request latched past the page it belonged to");
        assert!(crate::person::current().is_none());
    }

    /// The invariant `app.rs`'s `enter_node` depends on: putting a person page back through a BACK
    /// pop must NOT raise the routing latch. `open` raises it (it is the interactive entry, and
    /// app.rs's drain is what routes); `reopen` must not, because the drain would then push the very
    /// node the pop just took off — and the next BACK would appear to do nothing.
    #[test]
    fn reopen_mounts_the_page_without_asking_to_be_routed_to() {
        let _serial = crate::testlock::serial();
        reopen(crate::plex::ServerId::UNSET, "77", "plex://person/77", "Peter Sallis", "/t.jpg");
        assert!(!take_request(), "a trail re-entry must not ask app.rs to route again");
        assert_eq!(crate::person::current().map(|p| p.key.clone()), Some("77".to_string()));
        assert!(scene().on_header, "…and it is still a full mount: the page opens on its header");

        open(crate::plex::ServerId::UNSET, "78", "plex://person/78", "Nick Park", "/n.jpg");
        assert!(take_request(), "the interactive entry is the one that raises the latch");
        crate::person::close();
    }

    // The band's real heights need the text stack (the name's cap band, the bio's pixel wrap),
    // which the host suite cannot link — so the geometry tests below feed `reveal_block`, which is
    // pure, with BOUNDS on those heights instead of measuring them.
    //
    /// The FULL text stack, bounded above by giving every line its whole point size (the real flow
    /// measures cap bands, which are shorter) — 72+24+26+16+26+40+120 = 324.
    const STACK_BOUND: f32 = theme::size::HERO as f32
        + META_GAP
        + theme::size::LABEL as f32
        + LIFE_GAP
        + theme::size::LABEL as f32
        + BIO_GAP
        + BIO_LINES as f32 * BIO_LEAD;
    /// The expanded band, bounded above: whichever of the portrait and that stack is taller. `f32::max`
    /// is not const, hence the comparison. This is v2's packing dividend in one number — the text rides
    /// BESIDE the portrait now, so a header with a full biography is ~324px instead of the ~580 v1's
    /// stacked column reached.
    const EXP_HEADER_H: f32 = if STACK_BOUND > PORTRAIT_EXP { STACK_BOUND } else { PORTRAIT_EXP };

    /// **The v2 packing dividend, as geometry.** The first shelf fits on screen with NO scroll
    /// under BOTH band states — even the EXPANDED one, and even against the pessimistic point-size
    /// bound above. (Under v1's stacked header a full biography pushed the first shelf below the
    /// fold, which is the bug that made the header a focus row; the row stays, but for navigation
    /// and the condense, not reachability.) So this is the test that fails if a future band, gap or
    /// under-title band grows past the room the page actually has.
    #[test]
    fn the_first_shelf_fits_at_rest_under_either_band_state() {
        let h = shelf_block_h();
        let scroll = |band: f32| {
            let top = HEADER_TOP + band + SHELF_GAP;
            reveal_block(0.0, top, h, top + h + BOTTOM_PAD)
        };
        // The two bands the page can actually BE in: condensed, and the portrait-bound expanded one.
        for band in [H_CON, PORTRAIT_EXP] {
            assert_eq!(
                scroll(band),
                0.0,
                "band {band}: focusing the first shelf scrolled the page (block bottom {}, floor {})",
                HEADER_TOP + band + SHELF_GAP + h,
                SCR_H - BOTTOM_PAD
            );
        }
        // [`EXP_HEADER_H`] is a MODELLING bound, not a band — it substitutes point sizes for cap
        // heights, so it stands 4px above anything the text stack can produce. Since `BOTTOM_PAD`
        // became the overscan keep-out (54, from 40) the expanded band clears the floor by 3px, so
        // those 4 phantom pixels buy one pixel of scroll. Asserted as a BOUND rather than dropped:
        // what this test is for is the packing dividend, and a bound that grows past its own
        // pessimism is the regression to catch.
        assert!(
            scroll(EXP_HEADER_H) <= EXP_HEADER_H - PORTRAIT_EXP,
            "the pessimistic bound costs {}px of scroll, more than the {}px it is pessimistic by",
            scroll(EXP_HEADER_H),
            EXP_HEADER_H - PORTRAIT_EXP,
        );
    }

    /// ...and a SECOND shelf, which cannot fit alongside the first, does scroll — by exactly
    /// enough to put its own block bottom on screen, never further. Graded at the CONDENSED band
    /// height, which is the state the page is actually in once focus is down at shelf two.
    #[test]
    fn reaching_the_second_shelf_scrolls_it_fully_into_view_and_no_further() {
        let h = shelf_block_h();
        let top = HEADER_TOP + H_CON + SHELF_GAP + h + SHELF_GAP;
        let content = top + h + BOTTOM_PAD;
        let want = reveal_block(0.0, top, h, content);
        assert!(want > 0.0, "the second shelf must scroll into view");
        assert!(top + h - want <= SCR_H, "its block bottom is still off screen");
        assert!(top - want >= TOP_MARGIN, "it scrolled past the minimum — the shelf overshot upward");
    }

    /// Focusing the header — flow child 0, at [`HEADER_TOP`] — rests the page at 0 for ANY band
    /// height the page can produce, from wherever the scroll happens to be: returning from deep in
    /// the shelves lands at the top (where the band then expands), never part-way.
    #[test]
    fn focusing_the_header_rests_the_page_at_the_top_from_any_scroll() {
        for h in [H_CON, EXP_HEADER_H, EXP_HEADER_H * 1.5] {
            let content = HEADER_TOP + h + SHELF_GAP + 2.0 * (shelf_block_h() + SHELF_GAP) + BOTTOM_PAD;
            for cur in [0.0, 200.0, content] {
                assert_eq!(
                    reveal_block(cur, HEADER_TOP, h, content),
                    0.0,
                    "band h={h} did not rest the page at the top from scroll {cur}"
                );
            }
        }
    }
}
