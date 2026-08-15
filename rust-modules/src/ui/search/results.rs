//! The typed result shelves.
//!
//! ## The design
//!
//! Shelves in a **fixed** order — `crate::search::KINDS` — and an empty type draws nothing at all.
//! Ranking is honoured inside a shelf and never across them: the server reorders its hubs per
//! query (measured), and reordering rows per keystroke moves the row under the user's focus while
//! they are still typing.
//!
//! Tile geometry is per type: posters 250×375, episode stills 420×236, headshots a 250 circle.
//! Collections are posters. An item with no artwork mounts no slot — the tile's own skeleton face
//! is the resting state for art the server has not given us, which for a collection is always
//! (`Directory` entries carry no thumb).
//!
//! The heading is HEADLINE bold, the count BODY/tertiary beside it. Every search shelf merges
//! across sources, so the heading **cannot claim an owner** — the owner annotation follows FOCUS,
//! exactly as Continue Watching's does on Home, and rides on the focused tile's own caption.
//!
//! One caption on screen at a time, as every other shelf in this app does it: the label belongs to
//! the focused tile and nothing else carries type under its artwork. The band is reserved either
//! way ([`super::LABEL_BLOCK`]) so nothing reflows as focus travels.
//!
//! ## What it is built on
//!
//! [`card_row::strip`] — the one-row loop — with [`card_row::TileLabel::height`] reserving the
//! label band, the way `ui/person.rs`'s `draw_shelf` already stacks several shelves over one scroll
//! flow. Every tile treatment a poster wears elsewhere in the app therefore travels here for free:
//! the focus pop and its lifted shadow, the amber resume bar, the watched tick.
//!
//! ## Five things that are not obvious from the picture
//!
//! **1. The flow is authored; the lift is paint.** A shelf's block is `HEAD_TO_ROW` + the tile +
//! [`super::LABEL_BLOCK`], and [`stack_top`] stacks them — the ONE vertical geometry this screen
//! has. The draw reads it and so does the scroll rule ([`reveal_shelf`], which `super`'s
//! `scroll_target` is now a single line over); the pointer reads rects RECORDED at draw, which is
//! the same geometry observed rather than a second derivation of it.
//!
//! It claimed that before and it was false: `super` carried its own `block_h`/`shelf_top`/
//! `content_h` and its own scroll policy, disagreeing by [`BOTTOM_PAD`], each half green under its
//! own test. **The policy that survived is the minimal reveal below, not the pin** the mock spells
//! as `shift() = min(blockTop − CONTENT_TOP, max)`. Three reasons, in order of weight. Every other
//! vertical shelf flow in this app is [`card_row::reveal`] — `home.rs`'s grid, `person.rs`'s
//! `reveal_block`, the Library grid — so pinning would make Search the one screen that jumps. The
//! pin's stated justification in `super` ("nothing clips the flow") went stale the day [`draw`]
//! gained a real scissor: a shelf above the focused one is now CUT at the field, where before it
//! would have painted up through the chrome. And the shipped pin was not the mock's anyway — the
//! mock clamps to `end − SCR_H + 40`, and that 40 is [`BOTTOM_PAD`], which `super::content_h`
//! omitted, so the last shelf's caption band sat flush on the panel edge with no air under it.
//!
//! What a user sees for it: stepping from shelf 0 to shelf 1 moves the page 306px instead of 549,
//! so the row above stays half on screen rather than leaving entirely, and the bottom of the flow
//! keeps 40px under the last caption.
//!
//! The focused shelf's heading *rises* over its magnified tile, and that rise is `CardRow::lift()`:
//! the shared clearance rule, `tile height × (focus_scale − 1) ÷ 2` (16.9px on a poster row — the
//! design's 17), tapered by how near the popped tile is to the heading. It moves nothing below it.
//!
//! **2. The owner annotation is a FADE with its swap at the floor.** A heading here cannot claim an
//! owner, so the run follows focus — and focus can step straight from one borrowed source to
//! another. A point of ink cannot cross-fade between two different words, so [`Owner`] holds the
//! run it is currently naming, drives its alpha to zero whenever the target differs, adopts the new
//! one while it is invisible and rises again: the same Out/Hold/In shape `ui::xfade` has one
//! altitude up. With no source there is no run, no dot, no pad and no draw call at all, which is
//! how a single-server library pays nothing for the feature.
//!
//! **3. Only VISIBLE tiles may resolve a texture.** `card_row::strip` culls off-axis columns for
//! us; the vertical half is this module's, so a shelf whose whole block is off screen is skipped
//! before `strip` is entered. The poster LRU is 64 slots and one query answers with five shelves.
//!
//! **4. A new result set starts at its left edge.** The store clears its shelves on the keystroke
//! and refills them when the answer lands, so a row whose item COUNT changed is re-seated rather
//! than left holding the scroll offset of a different query's results.
//!
//! **5. The springs are stepped from [`update`], which `super::update` calls once a frame** — the
//! ordinary shape, and the reason this module owes the frame gate no clock of its own. Both spring
//! integrators report to `ui::idle::note_spring` from the inside, and the update phase runs before
//! `should_present`, so the shelves' motion is seen for free.
//!
//! Worth spelling out because it was not always so. A region's whole contract is
//! `draw(Painter, &View)` — no dt — so this module used to advance from its own monotonic clock
//! inside [`draw`], and then had to report the DISCRETE way (`ui::idle::invalidate`) against an
//! exact per-row motion signature, because **`note_spring` cannot speak for a spring stepped in a
//! draw**: `app.rs` runs `idle::frame_begin` → update → `should_present` → draw, and `frame_begin`
//! clears the motion flag at the top of the next iteration, so a report raised during frame N's
//! draw is wiped before anything reads it. All of it existed only because the one-line caller did
//! not, and all of it is gone with the caller in place.
//!
//! [`step_owner`]'s `invalidate` is the one report that stays, and for a different reason: the
//! adopt frame changes the run's WORDS with the spring at rest on the floor, and no integrator can
//! speak for a change that carries no motion.
//!
//! ## Why there is no `#![allow(dead_code)]` here any more
//!
//! There was one, on this file and on `super`, and it is what let ~130 lines of rival flow —
//! geometry, scroll rule, hit-test, all of it tested and green — sit here with no caller anywhere
//! in the crate while `super` ran its own copy. Nothing but the compiler was ever going to notice
//! that. Do not put a blanket back to quiet a seam that is still being built: give the seam a
//! caller, or say in its doc that it has none and why.

use crate::search::{Item, Kind, Shelf};
use crate::ui::card_row::{self, CardRow, RowStyle, TileLabel};
use crate::ui::consts::*;
use crate::ui::label::{Label, VAlign};
use crate::ui::search::{View, Zone, CONTENT_TOP, HEAD_TO_ROW, LABEL_BLOCK};
use crate::ui::theme;
use crate::ui::widgets::Art;
use crate::ui::{on_axis, Painter, Rect, Spring};
use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::ptr::addr_of_mut;

/// How many shelves there can ever be. The store emits [`crate::search::KINDS`] with the empty
/// types omitted, so this is the ceiling and never the count.
const NSHELF: usize = crate::search::KINDS.len();

/// A shelf's title to its count read-out — the design's 16px, spelled as the `space` rung it is.
const COUNT_GAP: f32 = theme::space::SM;
/// Pad either side of the `·` that introduces the SOURCE annotation. The same rung `home.rs` spends
/// on the same annotation (its `SOURCE_PAD`): this is that annotation in a third position, not a
/// third spacing.
const SOURCE_PAD: f32 = theme::space::XS;
/// Air kept under the lowest visible content when a shelf is revealed by scrolling.
const BOTTOM_PAD: f32 = theme::space::LG;
/// Headroom between the field's bottom edge and where scrolled content is cut. It is not spacing —
/// nothing is drawn in it at rest — it is the allowance for the two things that paint above a
/// shelf's own block top: the focused heading's `CardRow::lift()` and a magnified tile's glow.
const BAND_CLEAR: f32 = theme::space::MD;

/// Art is asked for at the size the REST OF THE APP asks for it, never at this shelf's own tile
/// size: the store key is `(server, path, w, h, png)`, so a film already on a Home shelf is a
/// poster-cache HIT here rather than a second trip through `/photo/:/transcode` into a second slot
/// of a 64-slot LRU. `detail.rs`'s filmstrip requests a still at 640×360 and `person.rs` a portrait
/// at 300×300; these are those numbers, spelled here so the reason travels with them.
///
/// A POSTER has no constant, because `Art::Poster` resolves at its own fixed 250×375 inside
/// `widgets::card` — the one below is only ever reached by a `Directory` hit that turned out to
/// carry a thumb after all.
const TAG_POSTER_RES: (c_int, c_int) = (250, 375);
const STILL_RES: (c_int, c_int) = (640, 360);
const HEAD_RES: (c_int, c_int) = (300, 300);

/// Tile size for a shelf of this kind: `(w, h, circular)`.
fn tile_size(kind: Kind) -> (f32, f32, bool) {
    match kind {
        Kind::Episode => (420.0, 236.0, false),
        Kind::Person => (250.0, 250.0, true),
        _ => (CARD_W, CARD_H, false),
    }
}

/// A shelf's row style: [`RowStyle::HOME`]'s motion verbatim — one source, so a poster here
/// animates exactly as it does on Home — with only the tile's own size and shape per kind.
///
/// `title_lines` stays at HOME's **one**, and this is the one place the search shelves cannot copy
/// the person page (which wraps to two). [`super::LABEL_BLOCK`] is the band this screen reserves
/// and it is fixed at 114 by the keyboard clearance the whole layout is built around; a two-line
/// title makes [`card_row::TileLabel::height`] 122, which would put the caption 8px inside the next
/// shelf's heading. The host test below grades exactly that.
fn style(kind: Kind) -> RowStyle {
    let (w, h, circular) = tile_size(kind);
    RowStyle { w, h, circular, ..RowStyle::HOME }
}

// ---- geometry (pure: the draw and the reveal rule both read these) ------------------------------

/// The vertical extent shelf `i` occupies in the flow: its heading, the row, and the caption band
/// reserved under it whether or not a caption is drawn. There is no gap term because the reserved
/// band IS the air — a poster shelf comes out at `HEAD_TO_ROW + 375 + LABEL_BLOCK` = 549, which is
/// `consts::ROW_PITCH` exactly, so a search shelf and a home shelf stack at the same rhythm (the
/// test below asserts that equality rather than restating the number).
///
/// **[`super::LABEL_BLOCK`] must stay at least [`card_row::TileLabel::height`] for the [`style`]
/// this module draws with** — that function is `ui/CLAUDE.md`'s named single authority on the band,
/// and this expression is what has to reserve it. At `title_lines: 1` it is 92, so the 114 declared
/// there holds it with 22 to spare (the same 22 home's `ROW_PITCH` carries); at 2 it is **122 and
/// the constant is 8 short**, which is the cross-file cost of raising `title_lines` and is graded
/// by `the_reserved_caption_band_holds_the_block_the_shared_component_draws` below.
fn block_h(kind: Kind) -> f32 {
    HEAD_TO_ROW + tile_size(kind).1 + LABEL_BLOCK
}

/// Flow y of shelf `i`'s HEADING top, given the kinds on screen — the ONE vertical geometry this
/// screen has. Shelves stack from [`super::CONTENT_TOP`] with no gap between blocks, because
/// [`super::LABEL_BLOCK`] already carries the air under a row (the caption block itself is 92 of
/// its 114).
///
/// Pure, and kept so deliberately: it is the layer the host suite can drive, and [`reveal_shelf`]
/// — the only impure thing built on it — is one line on top. (`person.rs`'s `reveal_block` splits
/// for exactly this reason.)
fn stack_top(kinds: &[Kind], i: usize) -> f32 {
    CONTENT_TOP + kinds[..i.min(kinds.len())].iter().map(|&k| block_h(k)).sum::<f32>()
}

/// Total flowed height — the last block's bottom plus the bottom air.
fn stack_content_h(kinds: &[Kind]) -> f32 {
    match kinds.last() {
        Some(&k) => stack_top(kinds, kinds.len() - 1) + block_h(k) + BOTTOM_PAD,
        None => 0.0,
    }
}

/// The MINIMAL vertical scroll that reveals shelf `i` — the shared [`card_row::reveal`] rule the
/// home grid, the person page and the Library grid use, not a lift-to-margin pin: a block scrolls
/// only as far as its own extent needs, and not at all when it already fits. The module doc's
/// point 1 has why this is the rule that won and what it costs the mock.
///
/// It answers for the flow alone. **When the flow is frozen** — the keyboard is up, or the shelves
/// do not hold focus — the answer is not 0 from here but never asked for: `super::scroll_frozen` is
/// the one place either freeze is stated, because both are facts about the state machine's ZONE and
/// not about geometry.
fn reveal_of(cur: f32, kinds: &[Kind], i: usize) -> f32 {
    if i >= kinds.len() {
        return 0.0;
    }
    let top = stack_top(kinds, i);
    let h = block_h(kinds[i]);
    let lo = top + h - (SCR_H - BOTTOM_PAD);
    let hi = top - CONTENT_TOP;
    card_row::reveal(cur, lo, hi, (stack_content_h(kinds) - SCR_H).max(0.0))
}

/// The kinds on screen, in flow order, and how many — the store's own order, which is already
/// [`crate::search::KINDS`] with the empty types omitted.
///
/// A fixed array rather than a `Vec`: this is asked twice a frame — once by [`draw`] and once by
/// [`reveal_shelf`] out of `super::update` — and a five-element heap allocation on the draw path is
/// one it does not need. (`person.rs::present` is the same shape for the same reason.)
fn present() -> ([Kind; NSHELF], usize) {
    let mut v = [Kind::Movie; NSHELF];
    let mut n = 0;
    for s in crate::search::shelves().iter().take(NSHELF) {
        v[n] = s.kind;
        n += 1;
    }
    (v, n)
}

/// [`reveal_of`] against the store — the value `View::shift` springs toward, and the ONLY geometry
/// this module hands out. `super` owns the flow's freezes and nothing else about its shape.
pub(crate) fn reveal_shelf(cur: f32, i: usize) -> f32 {
    let (k, n) = present();
    reveal_of(cur, &k[..n], i)
}

/// The rect a DRAWN tile is recorded at for the pointer, cut to the band the user can actually
/// see — or `None` when the whole tile is above it.
///
/// **What is painted and what is REACHABLE are not the same rect on this screen**, and that is the
/// whole reason this exists. [`draw`]'s scissor cuts at `BAND_CLEAR` above the field, and the
/// navigation scrim over everything above [`super::CHROME_BOTTOM`] is OPAQUE — so a shelf scrolled
/// up under the chrome goes on being drawn (partly into the scissor's dead zone, partly behind a
/// flat band) while its recorded rects went on claiming it was there to be pressed. The pointer
/// then found tiles in blank top chrome: a hover re-seated focus, which yanks the flow to reveal a
/// shelf nobody pointed at, and a click opened a poster that was not on screen.
///
/// It is this module's own cull rule at the other end. `on_axis` skips a shelf that is off the
/// PANEL, before `strip` is entered; this trims the tile that is behind the CHROME, after `strip`
/// has drawn it — and it FLOORS rather than dropping, so a tile crossing the line stays addressable
/// on exactly the half of it that can be seen. The `> 0.5` is `super::find_rect`'s own rule about
/// zero-extent rects, applied where the rect is made instead of only where it is scanned:
/// `Rect::contains` is inclusive, so a rect floored to nothing would still answer on its own edge.
fn hit_band(r: Rect, floor: f32) -> Option<Rect> {
    let top = r.y.max(floor);
    let bottom = r.y + r.h;
    (bottom - top > 0.5).then(|| Rect::new(r.x, top, r.w, bottom - top))
}

// ---- screen state ------------------------------------------------------------------------------

/// The owner annotation's own little state machine — see the module doc's point 2.
struct Owner {
    /// 0 = absent, 1 = fully stated. It rides the painter's cascade alpha and never
    /// `theme::with_a`: [`theme::TEXT_SEPARATOR`] carries .45 of its own, and `with_a` REPLACES an
    /// alpha rather than multiplying it.
    a: Spring,
    /// The handle currently being named, and the shelf it rides. Empty = nothing to say.
    shown: String,
    row: usize,
    /// `shown`, NUL-terminated once per change rather than once per frame.
    shown_c: CString,
}

impl Owner {
    fn new() -> Self {
        Owner { a: Spring::at(0.0), shown: String::new(), row: 0, shown_c: CString::default() }
    }
    /// This shelf's annotation alpha — 0 for every shelf but the one the run is riding.
    fn alpha(&self, row: usize) -> f32 {
        if self.row == row {
            self.a.pos.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

struct Shelves {
    rows: [CardRow; NSHELF],
    owner: Owner,
    /// The count read-outs, baked when a shelf's ANSWER changes — a `format!` per shelf per frame
    /// is exactly the allocation the tile loop avoids.
    count_c: [CString; NSHELF],
    /// What each `count_c` was baked FOR: the kind and the item count together, never the count
    /// alone. A shelf POSITION is not a kind — a query whose Movies hub comes back empty puts Cast
    /// & Crew at index 0 — so a bare count leaves "Cast & Crew  5 results" on screen the moment the
    /// two answers happen to be the same size. It is also the re-seat key (see [`update`]).
    count_key: [Option<(Kind, usize)>; NSHELF],
    /// The shelf TITLES, NUL-terminated once for the life of the app: they are `&'static str` in
    /// the store and would otherwise be a `CString::new` per shelf per frame. Indexed by the kind's
    /// position in [`crate::search::KINDS`], not by shelf position, because an absent type shifts
    /// every shelf below it. (`person.rs` writes its two as `c""` literals; these live in the store
    /// as the headings, and one copy is better than two that can drift.)
    title_c: [CString; NSHELF],
}

impl Shelves {
    fn new() -> Self {
        Shelves {
            rows: [CardRow::new(); NSHELF],
            owner: Owner::new(),
            count_c: std::array::from_fn(|_| CString::default()),
            count_key: [None; NSHELF],
            title_c: std::array::from_fn(|i| CString::new(crate::search::KINDS[i].title()).unwrap_or_default()),
        }
    }
    /// This kind's baked heading run.
    fn title(&self, kind: Kind) -> &CStr {
        &self.title_c[crate::search::KINDS.iter().position(|&k| k == kind).unwrap_or(0)]
    }
}

static mut STATE: Option<Shelves> = None;
fn state() -> &'static mut Shelves {
    unsafe { (*addr_of_mut!(STATE)).get_or_insert_with(Shelves::new) }
}

/// Which column shelf `i` draws as focused, or `None` for every shelf that does not hold the remote
/// — including ALL of them while focus is in the field or the strip.
///
/// This is what makes "one caption on screen at a time" true by construction: `card_row::strip`
/// captions the column it is given and nothing else, so a shelf handed `None` draws a full row of
/// bare artwork. It is also the whole implementation of the freeze — a row scrolls only while a
/// tile inside it holds focus, so withholding focus while `v.editing` IS "nothing scrolls at all
/// while the keyboard is up".
fn focus_col(v: &View, i: usize, n: usize) -> Option<usize> {
    (v.zone == Zone::Results && !v.editing && v.row == i && n > 0).then(|| v.col.min(n - 1))
}

/// The item holding focus, or None when nothing in the shelves does.
fn focused_item(v: &View) -> Option<&'static Item> {
    let s = crate::search::shelves().get(v.row)?;
    focus_col(v, v.row, s.items.len()).and_then(|c| s.items.get(c))
}

/// Whose server the focused item came from, as the owner's handle — **empty for our own**, which is
/// every item on a single-server install.
///
/// The dev stand-in is the SAME `/tmp/plxnative-shared=<handle>` file the hero's "Shared by …" run
/// and the detail page's facts row read, deliberately not a second trigger: one file arming every
/// surface is the only way to see them agree, and this annotation is otherwise unreachable on a
/// machine with one server — which is every machine that can photograph it.
fn owner_handle(v: &View) -> &'static str {
    let Some(it) = focused_item(v) else { return "" };
    // **The trigger WINS WHEN ARMED** — see [`dev_source`].
    dev_source().unwrap_or_else(|| crate::plex::server_facts(it.sid()).map(|f| f.handle.as_str()).unwrap_or(""))
}

/// The stand-in handle, read ONCE — the trigger surface is boot state, and a `stat` per frame
/// inside a draw is not. Compiled out with the `devtriggers` feature, like every other trigger.
///
/// **The trigger WINS WHEN ARMED**, the precedence every `/tmp/plxnative-*` read in this app has
/// (`crate::dev`'s module doc: `plxnative-token` beats the signed-in session) and the phrase to
/// grep for — the same file is read by `metadata::fetch_detail` and `ui::home`'s `dev_source`, and
/// this site used to stand in only where the real answer was empty, the opposite rule. On a set
/// with a real share installed that made one screen answer with the stand-in and another with the
/// truth, which is precisely what the stand-in exists to compare. An armed but EMPTY file forces
/// the absence of a handle rather than doing nothing, for the same reason.
fn dev_source() -> Option<&'static str> {
    static SEEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| crate::dev::read("shared")).as_deref()
}

// ---- update ------------------------------------------------------------------------------------

/// Step the shelves — one call from `super::update`, in the update phase, so both spring
/// integrators' own `note_spring` reports reach the frame gate before it decides (module doc,
/// point 5).
pub(crate) fn update(dt: f32, v: &View) {
    let shelves = crate::search::shelves();
    let handle = owner_handle(v);
    let st = state();
    for (i, row) in st.rows.iter_mut().enumerate() {
        // A shelf's ANSWER — its kind and its extent. The store empties its shelves on the
        // keystroke and refills them when the reply lands, so this is `None` for a frame or two in
        // between, and both edges of that are changes.
        let key = shelves.get(i).map(|s| (s.kind, s.items.len()));
        if st.count_key[i] != key {
            st.count_key[i] = key;
            st.count_c[i] = match key {
                Some((k, n)) => CString::new(format!("{n} {}", k.count_word(n))).unwrap_or_default(),
                None => CString::default(),
            };
            // A DIFFERENT result set: re-seat the row rather than leave it parked in the scroll
            // offset of a query whose answers are gone. Keyed on the answer rather than on the
            // count alone, so two broad queries that both fill a shelf still re-seat it.
            *row = CardRow::new();
        }
        match shelves.get(i) {
            // a shelf that is not there holds no focus and needs no style of its own
            None => row.update(0, None, &RowStyle::HOME, dt),
            Some(s) => {
                let n = s.items.len();
                row.update(n, focus_col(v, i, n), &style(s.kind), dt);
            }
        }
    }
    step_owner(&mut st.owner, v.row, handle, dt);
}

/// Drive the annotation toward the focused item's source: out whenever the run on screen is not the
/// one wanted, adopt while invisible, in again. Pure but for the one spring, so the swap rule is
/// host-gradeable — see the tests.
///
/// The adopt calls [`crate::ui::idle::invalidate`] because it is a DISCRETE change with no motion
/// in it: the spring is already at rest on the floor, so `note_spring` sees nothing, and the frame
/// that changes the run's words would otherwise be the one frame the present gate is free to skip.
fn step_owner(o: &mut Owner, want_row: usize, want: &str, dt: f32) {
    let settled = o.row == want_row && o.shown == want;
    o.a.step(if settled && !o.shown.is_empty() { 1.0 } else { 0.0 }, K_SCALE, dt);
    if !settled && o.a.pos < OWNER_FLOOR {
        o.shown.clear();
        o.shown.push_str(want);
        o.shown_c = CString::new(want).unwrap_or_default();
        o.row = want_row;
        crate::ui::idle::invalidate();
    }
}

/// The alpha below which the annotation is invisible, and so the alpha at which its words may be
/// swapped. It is also the draw's skip test: under it there is no run, no dot, no pad and no draw
/// call.
const OWNER_FLOOR: f32 = 0.02;

// ---- draw --------------------------------------------------------------------------------------

pub(crate) fn draw(p: Painter, v: &View) {
    let shelves = crate::search::shelves();
    if shelves.is_empty() {
        return;
    }
    // `shift` is a SCROLL, as every other flow in this app spells it: content is drawn one shift
    // higher, so the whole region rides ONE translate and every y below stays a flow coordinate.
    // Content scrolls UP, and has to be CUT above rather than painted over the chrome. The shelves
    // ride one translate and nothing else bounds them, so a scrolled row rose behind the tab strip
    // and the query capsule and was drawn ON TOP of both — device-observed, and invisible until
    // something was actually scrolled.
    //
    // **The cut is now ABOVE the field, and it is a bound rather than a treatment.** It sat just
    // BELOW the field, which made it the only thing standing between a scrolling poster and the
    // chrome — and a scissor is a razor: a tile crossing the capsule lost its top half to a hard
    // horizontal line, which is what got photographed. `super::draw_scroll_scrim` does that job
    // properly now (the Library's veil, and the draw order that lets it be opaque), so this only
    // has to stop a tile reaching the tab strip, and it cuts inside the veil's own opaque band
    // where the cut cannot be seen. `BAND_CLEAR` is still the headroom for the two things that
    // legitimately paint above a shelf's block top — the focused heading's lift and a magnified
    // tile's glow.
    //
    // Paired with `clip_clear` below, and deliberately AFTER the early return above: this is global
    // GL scissor state, so a return between the two would leave the rest of the frame scissored.
    let cut = super::FIELD.y - BAND_CLEAR;
    p.clip(Rect::new(0.0, cut, SCR_W, SCR_H - cut));
    let pf = p.translate(0.0, -v.shift);
    let st: &Shelves = state();
    let (ks, n) = present();
    for (i, s) in shelves.iter().take(n).enumerate() {
        let top = stack_top(&ks[..n], i);
        // The VERTICAL cull, over the whole block: a shelf off the panel must not reach `strip` at
        // all, because that is where `resolve_tex` is called — the poster LRU is 64 slots against
        // five shelves of answers.
        if on_axis(top - v.shift, block_h(s.kind), SCR_H, 0.0) {
            draw_shelf(pf, st, s, i, v, top);
        }
    }
    p.clip_clear();
}

fn draw_shelf(p: Painter, st: &Shelves, s: &Shelf, i: usize, v: &View, top: f32) {
    let sty = style(s.kind);
    let n = s.items.len();
    let row = &st.rows[i];
    // the heading rises over the magnified tile and takes nothing with it: the flow below is
    // authored from `top`, never from the lifted line
    draw_heading(p, st, s, i, top - row.lift());

    // The caption states the FOCUSED ITEM's own source, not the heading's settled one. The two are
    // the same string except during the annotation's fade, and there the caption must be the one
    // that is right: it is attached to a particular poster, at full opacity, and the heading's run
    // is the one wearing the alpha that says it is mid-change.
    let handle = owner_handle(v);
    card_row::strip(
        p,
        row,
        n,
        focus_col(v, i, n).map(|c| c as c_int).unwrap_or(-1),
        top + HEAD_TO_ROW,
        (sty.w, sty.h),
        sty.w + sty.gap,
        &sty,
        SCR_W,
        |k| tile_art(s.kind, &s.items[k]),
        |k| match &s.items[k] {
            Item::Media(m) => m.resume_frac(),
            Item::Tag(_) => None,
        },
        |k| TileLabel::titled(s.items[k].title(), &subtitle(s.kind, &s.items[k], handle)),
        // Record what was DRAWN, in screen space, so the pointer can reach it. Without this
        // `super::HIT_R` stayed empty and every result on the screen was unreachable by pointer —
        // the whole content area answered to the d-pad alone, silently, because `mod.rs::hit`
        // scanned a store nothing ever filled while a complete `results::tile_at` (which DERIVED
        // the same rects, and is deleted) sat with no caller. Both files carried
        // `#![allow(dead_code)]`, which is why neither half warned.
        //
        // Recording is chosen over deriving on purpose, and this is the reason: the rect below is
        // the tile as PAINTED, this frame's horizontal scroll and focus pop included, where a
        // derivation is a second expression that has to be kept in step with the draw by hand.
        //
        // `strip` hands over the tile's layout **x** — not a scale, which an earlier version of
        // this closure assumed and passed straight to `Rect::scaled`, inflating every rect ~90×.
        // `mod.rs::hit` takes the FIRST rect containing the point and non-focused tiles are
        // recorded first, so that resolved every click anywhere in the results region to shelf 0,
        // column 0. `detail.rs`'s cast row uses the same argument as an x and is the reference.
        //
        // That x is in CONTENT space (`strip` draws through a `-scroll_x` translate), and the y is
        // the flow line less this frame's scroll — hit-testing happens in the untranslated screen
        // the user is actually pointing at. Scaled by the cell's own spring so the target matches
        // the tile as DRAWN, magnified or not.
        //
        // …and CUT to what is visible before it is recorded ([`hit_band`]): the paint is scissored
        // and over-painted above the field, so a rect taken straight from the layout claims ground
        // the tile does not hold.
        |_, k, x, _| {
            let r = Rect::new(x - row.scroll_x(), top + HEAD_TO_ROW - v.shift, sty.w, sty.h)
                .scaled(row.scale(k));
            if let Some(r) = hit_band(r, super::CHROME_BOTTOM) {
                super::note_tile_rect(i, k, r);
            }
        },
    );
}

/// The heading, flowed left→right from the shelf's own origin: the type's title, its count, and —
/// only while the focused item is BORROWED — a quiet `· handle` naming that source.
///
/// The annotation's runs ride a faded PAINTER rather than a faded ink, because
/// [`theme::TEXT_SEPARATOR`] carries .45 of its own alpha and `theme::with_a` would replace it.
///
/// Every run sits on the TITLE's baseline (a `BODY` run cap-top-aligned against a `HEADLINE` one
/// reads as a superscript), which is what the reference cap band below is for — the same
/// `VAlign::Baseline` construction `person.rs` uses for its shelf counts.
fn draw_heading(p: Painter, st: &Shelves, s: &Shelf, i: usize, y: f32) {
    let a = st.owner.alpha(i);
    let pa = p.alpha(a);
    let cap = crate::text::cap_h(theme::size::HEADLINE, 1);
    heading_flow(
        st.title(s.kind),
        &st.count_c[i],
        if a > OWNER_FLOOR { st.owner.shown_c.as_c_str() } else { c"" },
        |run, dx, sz, bold, ink, faded| {
            // Every run is a run this module already NUL-terminated — the titles for the life of
            // the app, the count and the handle when they last changed. Nothing here allocates:
            // a `CString::new` per run per shelf per frame is what the baking exists to avoid, and
            // it is also the `ui/CLAUDE.md` lifetime gotcha (a temporary would be freed under the
            // pointer `Label` holds).
            let mut lab = Label::new(run.as_ptr(), sz, ink).v(VAlign::Baseline);
            if bold == 1 {
                lab = lab.bold();
            }
            lab.draw(if faded { pa } else { p }, Rect::new(MARGIN_X + dx, y, 0.0, cap))
        },
    );
}

/// The heading's flow, as ONE expression the draw and the host tests share: the draw passes a
/// closure that paints and returns `Label`'s advance, the tests pass one that measures. Returns the
/// whole line's advance.
///
/// **With no source there is no annotation at all** — no pad, no dot, no run, no draw call. That is
/// the design's "with one source, none of this is drawn" implemented as absence rather than as a
/// branch that draws nothing visible, which is what makes the feature free for a single-server
/// library in geometry, in ink and in draw calls alike.
fn heading_flow(
    title: &CStr,
    count: &CStr,
    source: &CStr,
    mut run: impl FnMut(&CStr, f32, c_int, c_int, [f32; 4], bool) -> f32,
) -> f32 {
    let mut dx = run(title, 0.0, theme::size::HEADLINE, 1, theme::TEXT_HEADING, false);
    if !count.is_empty() {
        dx += COUNT_GAP;
        dx += run(count, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY, false);
    }
    if source.is_empty() {
        return dx;
    }
    dx += SOURCE_PAD;
    dx += run(c"\u{b7}", dx, theme::size::BODY, 0, theme::TEXT_SEPARATOR, true);
    dx += SOURCE_PAD;
    dx += run(source, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY, true);
    dx
}

/// One tile's artwork.
///
/// A COLLECTION resolves nothing — `Directory` entries carry no thumb, so this is not the loading
/// state but the resting one — and it takes `Art::Poster(None)` for that rather than an empty
/// `Art::Thumb`, because the two draw DIFFERENT blank faces (`SKELETON_TOP`/`BOT` versus
/// `CARD_PLACEHOLDER`) and a whole shelf of collections sits directly under a shelf of posters
/// whose unloaded tiles wear the first one. Same blank, one vocabulary.
///
/// **An EPISODE takes `art`, and that is a known compromise, not the right answer.**
/// `pms::parse_item` deliberately overwrites an episode row's `thumb` with the SHOW's portrait
/// poster (`grandparentThumb`), so a landscape still cannot fill a poster card on Home — and the
/// episode's own still is then not on the DTO at all. `art` is the show's landscape fanart: right
/// shape, wrong specificity, so five episodes of one show draw one picture distinguished only by
/// their captions. The fix belongs in the STORE, which parses these hits and can keep the still
/// (`detail.rs` draws its filmstrip from the episode's own thumb at exactly [`STILL_RES`], so the
/// cache slot is already the right one); this shelf needs no change when it does.
fn tile_art<'a>(kind: Kind, it: &'a Item) -> Art<'a> {
    match (kind, it) {
        // The episode's OWN still, falling back to the show's fanart only when the server sent
        // none. `m.art` alone drew the same picture on every episode of one show — the fanart IS
        // the show's, so a row of six results for one series was six identical tiles.
        (Kind::Episode, Item::Media(m)) => {
            // still → thumb → art, and the middle step is the one that matters. `parse_item` fills
            // `still` only when it SUBSTITUTED the show poster into `thumb`; when the show has no
            // `grandparentThumb` there is nothing to substitute, so `thumb` IS the episode's own
            // 16:9 still and `still` is empty. Falling straight through to `art` there is the show
            // fanart again — the identical-tiles defect, surviving for exactly the shows the fix
            // did not cover.
            let key = [&m.still, &m.thumb, &m.art].into_iter().find(|k| !k.is_empty()).unwrap_or(&m.art);
            Art::Thumb { sid: m.sid, key, res: STILL_RES }
        }
        (_, Item::Media(m)) => Art::Poster(Some(m)),
        (Kind::Person, Item::Tag(t)) => Art::Person { sid: t.sid, key: &t.thumb, res: HEAD_RES },
        (_, Item::Tag(t)) if t.thumb.is_empty() => Art::Poster(None),
        (_, Item::Tag(t)) => Art::Thumb { sid: t.sid, key: &t.thumb, res: TAG_POSTER_RES },
    }
}

/// The focused tile's SECOND line — what this result is, in the fewest words that identify it: an
/// episode's show and address, a film's year, a collection's extent. A person's name is the whole
/// fact, so their tile carries no second line rather than an invented one.
///
/// A BORROWED item states its source here, as the line's LAST fact. Joined into the one run rather
/// than drawn as its own: a dot baked into a joined string is one run at one colour by
/// construction, and this line is already the quiet one — the heading's annotation is where the
/// separator earns its own ink. With no other fact the handle stands alone, because a caption that
/// OPENS with a middot reads as a bullet rather than as an annotation.
fn subtitle(kind: Kind, it: &Item, handle: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    match it {
        Item::Media(m) if kind == Kind::Episode => {
            let ord = crate::ui::fmt::episode_ordinal(m.season_index as i64, m.ep_index as i64);
            parts.push(match m.show_title.is_empty() {
                true => ord,
                false => format!("{} \u{b7} {ord}", m.show_title),
            });
        }
        Item::Media(m) if m.year > 0 => parts.push(m.year.to_string()),
        Item::Tag(t) if kind == Kind::Collection && t.count > 0 => {
            parts.push(format!("{} item{}", t.count, if t.count == 1 { "" } else { "s" }));
        }
        _ => {}
    }
    if !handle.is_empty() {
        parts.push(handle.to_string());
    }
    parts.join(" \u{b7} ")
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The geometry here is pure and is graded directly. What it CANNOT say is whether the shelves
    //! look right — nothing below opens a GL context or an SDL_ttf font, so the heading flow is
    //! measured through a synthetic advance and the picture is a device (or simulator) capture.
    use super::*;

    /// One 60 Hz frame.
    const DT: f32 = 1.0 / 60.0;
    const ALL: [Kind; 5] = crate::search::KINDS;

    fn cs(s: &str) -> CString {
        CString::new(s).expect("test literal")
    }

    /// **The band this screen RESERVES must hold the block `card_row` actually draws.**
    /// [`super::super::LABEL_BLOCK`] is fixed by the keyboard clearance the whole layout is built
    /// around, so it is the tile label that has to fit inside it — and it only does at one title
    /// line. This is the test that fails the day someone raises `title_lines` for a shelf of long
    /// Plex titles: the caption would land 8px inside the next shelf's heading.
    #[test]
    fn the_reserved_caption_band_holds_the_block_the_shared_component_draws() {
        for k in ALL {
            let drawn = TileLabel::height(&style(k), true);
            assert!(drawn <= LABEL_BLOCK, "{k:?}: the label block draws {drawn}px into a band of {LABEL_BLOCK}");
        }
        assert!(
            TileLabel::height(&RowStyle { title_lines: 2, ..style(Kind::Movie) }, true) > LABEL_BLOCK,
            "a two-line title is what this band cannot hold — if that stops being true, say so here"
        );
    }

    /// The flow: each shelf sits exactly one block below the one before it, the first at
    /// `CONTENT_TOP`, and a block is the shelf's own heading + tile + reserved caption band. None
    /// of it may depend on which kinds are present or on how many items they hold.
    #[test]
    fn shelves_stack_by_their_own_block_heights_from_the_content_top() {
        assert_eq!(stack_top(&ALL, 0), CONTENT_TOP);
        for i in 1..ALL.len() {
            assert_eq!(
                stack_top(&ALL, i) - stack_top(&ALL, i - 1),
                block_h(ALL[i - 1]),
                "shelf {i} did not start one block below shelf {}",
                i - 1
            );
        }
        // A poster shelf is the app's ONE shelf rhythm, stated as the equality rather than as 549:
        // a tile-size change here must not silently re-space this flow against every other one.
        assert_eq!(block_h(Kind::Movie), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Show), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Collection), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Episode), HEAD_TO_ROW + 236.0 + LABEL_BLOCK);
        assert_eq!(block_h(Kind::Person), HEAD_TO_ROW + 250.0 + LABEL_BLOCK);
        // an episode shelf is shorter than a poster shelf by exactly the tile difference, so which
        // types are present must never change any other shelf's own height
        assert_eq!(block_h(Kind::Movie) - block_h(Kind::Episode), CARD_H - 236.0);
        // …and the same shelf laid out among a DIFFERENT set keeps its block
        assert_eq!(stack_top(&[Kind::Episode, Kind::Movie], 1), CONTENT_TOP + block_h(Kind::Episode));
        assert_eq!(stack_content_h(&[]), 0.0, "no shelves is no content, not one block of air");
        // an index past the end cannot panic: the store re-lands under a focus that has not been
        // re-clamped yet on exactly one frame
        assert_eq!(stack_top(&ALL, 99), stack_top(&ALL, ALL.len()));
    }

    /// **The keyboard clearance the whole screen is sized for.** With the panel up, the first
    /// shelf's heading and its ENTIRE row must finish above the keyboard's top edge — that is what
    /// `CONTENT_TOP`/`HEAD_TO_ROW` are for — and it must hold for the tallest tile a first shelf
    /// can have, since any of the five types can be the first one present.
    #[test]
    fn the_first_shelfs_whole_row_clears_the_raised_keyboard() {
        let floor = SCR_H - crate::ui::search::KEYBOARD_H;
        for k in ALL {
            let bottom = stack_top(&[k], 0) + HEAD_TO_ROW + tile_size(k).1;
            assert!(bottom <= floor, "{k:?}: the first row ends at {bottom}, under the keyboard at {floor}");
        }
        // The ACTUAL numbers `super`'s layout note quotes, so a doc claim nobody can reproduce
        // cannot come back: a poster shelf ends at 683 against a MEASURED panel edge of 756.
        assert_eq!(stack_top(&[Kind::Movie], 0) + HEAD_TO_ROW + CARD_H, 683.0);
        assert_eq!(floor, 756.0);
    }

    /// The scroll rule is `card_row::reveal`'s, **not the mock's pin** — the module doc's point 1
    /// is the decision and this is its executable half: a shelf already fully on screen must not
    /// move the page, and one below the fold scrolls exactly far enough to show its own block and
    /// no further.
    #[test]
    fn a_shelf_scrolls_only_as_far_as_its_own_block_needs() {
        assert_eq!(reveal_of(0.0, &ALL, 0), 0.0, "the first shelf is already on screen");

        let want = reveal_of(0.0, &ALL, 2);
        assert!(want > 0.0, "the third shelf is below the fold and must be revealed");
        let top = stack_top(&ALL, 2);
        assert!(top + block_h(ALL[2]) - want <= SCR_H, "its block bottom is still off screen");
        assert!(top - want >= CONTENT_TOP, "it scrolled past the minimum — the shelf overshot upward");
        // already revealed ⇒ no re-seating (the `reveal` contract: no slot pinning)
        assert_eq!(reveal_of(want, &ALL, 2), want);

        // The difference from the retired pin, stated as a number so the decision cannot be
        // reversed by accident: shelf 1 comes into view for 306px of scroll where pinning its
        // block top to `CONTENT_TOP` demanded 549 — the whole of shelf 0, which stays half on
        // screen instead of leaving.
        let one = reveal_of(0.0, &ALL, 1);
        assert!(one < stack_top(&ALL, 1) - CONTENT_TOP, "the reveal must undercut the pin, or it IS the pin");
        assert_eq!(one, stack_top(&ALL, 1) + block_h(ALL[1]) - (SCR_H - BOTTOM_PAD));

        // …and the end of the flow keeps `BOTTOM_PAD` of air, which the pin's own clamp dropped.
        let last = ALL.len() - 1;
        let end = reveal_of(0.0, &ALL, last);
        assert_eq!(stack_content_h(&ALL) - end, SCR_H, "the last block rests one panel above the flow's end");
        assert_eq!(stack_top(&ALL, last) + block_h(ALL[last]) - end, SCR_H - BOTTOM_PAD);

        assert_eq!(reveal_of(0.0, &ALL, 99), 0.0, "a shelf that is not there cannot scroll the page");
    }

    /// **What is drawn and what can be PRESSED are not the same rect above the field.** The paint
    /// is scissored at `BAND_CLEAR` above it and the navigation scrim over everything higher is
    /// opaque, so a shelf scrolled up under the chrome goes on being drawn while its recorded rects
    /// went on claiming it was there. The pointer found tiles in blank top chrome: a hover
    /// re-seated focus, which yanks the flow back to reveal a shelf nobody pointed at, and a click
    /// opened a poster that was not on screen.
    #[test]
    fn a_tile_scrolled_under_the_chrome_is_not_a_pointer_target() {
        let floor = crate::ui::search::CHROME_BOTTOM;
        let tile = |y: f32| Rect::new(400.0, y, CARD_W, CARD_H);

        // At rest the first shelf's row is well clear of the chrome, and its rect is recorded
        // exactly as it was drawn — the common case must cost nothing.
        let at_rest = tile(stack_top(&ALL, 0) + HEAD_TO_ROW);
        let r = hit_band(at_rest, floor).expect("an on-screen tile is a target");
        assert_eq!((r.y, r.h), (at_rest.y, at_rest.h));

        // Crossing the line: only the visible part answers, and the column is untouched — this is
        // a floor, not a cull.
        let cross = tile(floor - 100.0);
        let r = hit_band(cross, floor).expect("the part below the chrome is still a target");
        assert_eq!((r.y, r.h), (floor, CARD_H - 100.0));
        assert_eq!((r.x, r.w), (cross.x, cross.w));
        assert!(!r.contains(r.x + 1.0, floor - 1.0), "…and nothing above the line is inside it any more");

        // Fully above it: no rect at all, so `super::find_rect` never sees one. The exact-touch
        // case is a real target and not a nicety — `Rect::contains` is inclusive, so a rect floored
        // to nothing is clickable along its own edge (`library::STATUS_BTN`'s lesson, which
        // `find_rect`'s `w > 0.5` states for the horizontal).
        assert!(hit_band(tile(floor - CARD_H - 1.0), floor).is_none());
        assert!(hit_band(tile(floor - CARD_H), floor).is_none(), "a tile ending ON the line is not one");
        assert!(hit_band(tile(floor - CARD_H + 0.4), floor).is_none(), "nor is a sliver under half a pixel");

        // …and the flow really does reach that state: revealing the last shelf — where a ▼ walk
        // down the shelves ends — puts the first one's whole row above the chrome.
        let deep = reveal_of(0.0, &ALL, ALL.len() - 1);
        let row0 = tile(stack_top(&ALL, 0) + HEAD_TO_ROW - deep);
        assert!(row0.y + row0.h < floor, "the first row is behind the chrome at {deep}px of scroll");
        assert!(hit_band(row0, floor).is_none(), "…so nothing in it may answer the pointer");
    }

    /// The heading flow: title, one 16px gap, the count — and the source annotation ABSENT rather
    /// than empty when the focused item is ours, so a single-server library pays nothing for it.
    /// Measured through the very expression the draw paints, with a synthetic per-character advance
    /// (the host suite opens no SDL_ttf, so there are no real glyph widths to have).
    #[test]
    fn the_heading_states_its_count_and_annotates_only_a_borrowed_source() {
        let w = |s: &str, sz: c_int| s.chars().count() as f32 * sz as f32 * 0.5;
        type Run = (String, f32, c_int, c_int, [f32; 4], bool);
        let runs = |title: &str, count: &str, src: &str| {
            let (t, c, s) = (cs(title), cs(count), cs(src));
            let mut out: Vec<Run> = Vec::new();
            heading_flow(&t, &c, &s, |run, dx, sz, bold, ink, faded| {
                let txt = run.to_str().unwrap_or_default().to_string();
                let adv = w(&txt, sz);
                out.push((txt, dx, sz, bold, ink, faded));
                adv
            });
            out
        };

        let bare = runs("Movies", "12 results", "");
        assert_eq!(bare.len(), 2, "the title and its count, and nothing else for one source");
        assert_eq!((bare[0].2, bare[0].3), (theme::size::HEADLINE, 1), "the title is HEADLINE bold");
        assert_eq!(bare[0].4, theme::TEXT_HEADING, "…in the shared section-heading ink");
        assert_eq!((bare[1].2, bare[1].3), (theme::size::BODY, 0), "the count is BODY regular");
        assert_eq!(bare[1].4, theme::TEXT_TERTIARY);
        assert_eq!(bare[1].1, w("Movies", theme::size::HEADLINE) + COUNT_GAP, "the count is one gap past the title");

        let shared = runs("Movies", "12 results", "friend");
        assert_eq!(shared.len(), 4, "title, count, separator, handle");
        assert_eq!(shared[..2], bare[..2], "the annotation must not disturb the runs ahead of it");
        assert_eq!(shared[2].0, "\u{b7}");
        assert_eq!(shared[2].4, theme::TEXT_SEPARATOR, "the middot is the tertiary ink at .45");
        assert_eq!(shared[2].1, bare[1].1 + w("12 results", theme::size::BODY) + SOURCE_PAD);
        assert_eq!(shared[3].0, "friend");
        assert_eq!(shared[3].4, theme::TEXT_TERTIARY);
        assert_eq!((shared[3].2, shared[3].3), (theme::size::BODY, 0), "the handle takes the count's rung");
        assert!(shared[2].5 && shared[3].5, "both annotation runs fade");
        assert!(!shared[0].5 && !shared[1].5, "…and the title and count never do");

        // a shelf whose count has not been baked yet is a bare title, with no stray gap
        assert_eq!(runs("Collections", "", "").len(), 1);
    }

    /// The annotation cannot cross-fade between two different handles, so it must go out, swap
    /// while it is invisible, and come back — never change its words while they can be read.
    #[test]
    fn the_owner_annotation_swaps_its_words_only_while_it_is_invisible() {
        // it reports to the frame gate's process-global flag, so it is serial by obligation and
        // not by precaution (`ui/CLAUDE.md`'s xfade note)
        let _g = crate::testlock::serial();
        let mut o = Owner::new();
        for _ in 0..180 {
            step_owner(&mut o, 0, "friend", DT);
        }
        assert_eq!(o.shown, "friend");
        assert!(o.a.pos > 0.98, "a borrowed item states its source (got {})", o.a.pos);

        // focus steps to another borrowed source
        let mut swapped = false;
        for f in 0..180 {
            step_owner(&mut o, 1, "otherfriend", DT);
            if o.shown != "friend" {
                assert!(o.a.pos <= OWNER_FLOOR, "frame {f}: it swapped its words at alpha {}", o.a.pos);
                swapped = true;
                break;
            }
        }
        assert!(swapped, "the new source never landed");
        assert_eq!(o.shown, "otherfriend");
        for _ in 0..180 {
            step_owner(&mut o, 1, "otherfriend", DT);
        }
        assert!(o.a.pos > 0.98, "…and then rises again");

        // back onto one of ours: it fades out and STAYS out, with nothing to say
        for _ in 0..240 {
            step_owner(&mut o, 1, "", DT);
        }
        assert!(o.a.pos < OWNER_FLOOR, "an owned item must state nothing (got {})", o.a.pos);
        assert_eq!(o.shown, "");
        assert_eq!(o.alpha(0), 0.0, "and no OTHER shelf ever wears the run");
    }

    /// A settled annotation must not ask for a frame — the present gate's other half, and the
    /// obligation `ui/CLAUDE.md` puts on anything that animates. An over-reporting animator costs
    /// the whole idle saving while every `floor` in the suite still passes.
    #[test]
    fn a_settled_annotation_goes_quiet_and_a_moving_one_does_not() {
        let _g = crate::testlock::serial();
        let asked = || {
            crate::ui::idle::frame_begin(DT);
            crate::ui::idle::note_present(0);
            crate::ui::idle::should_present(0)
        };
        asked(); // drain whatever the previous test left on the shared flag

        // the ADOPT frame carries no motion at all — the spring is at rest on the floor — so it is
        // the discrete `invalidate` that has to speak for it
        let mut o = Owner::new();
        crate::ui::idle::frame_begin(DT);
        crate::ui::idle::note_present(0);
        step_owner(&mut o, 0, "friend", DT);
        assert_eq!(o.shown, "friend");
        assert!(crate::ui::idle::should_present(0), "the frame that changed the run's words must repaint");

        // …and every frame of the rise after it reports as ordinary spring motion
        for f in 0..4 {
            crate::ui::idle::frame_begin(DT);
            crate::ui::idle::note_present(0);
            step_owner(&mut o, 0, "friend", DT);
            assert!(crate::ui::idle::should_present(0), "frame {f} of the rise must repaint");
        }

        for _ in 0..600 {
            step_owner(&mut o, 0, "friend", DT);
        }
        for f in 0..10 {
            crate::ui::idle::frame_begin(DT);
            crate::ui::idle::note_present(0);
            step_owner(&mut o, 0, "friend", DT);
            assert!(!crate::ui::idle::should_present(0), "frame {f}: a settled annotation asked for a repaint");
        }
    }

    /// The caption is the fewest words that identify a result — and a borrowed one names its source
    /// as the LAST fact on that line, never as a line that OPENS with a middot.
    #[test]
    fn a_caption_identifies_the_result_and_names_a_borrowed_source_last() {
        use crate::pms::PmsMovie;
        use crate::search::TagHit;

        let film = Item::Media(PmsMovie { title: "Wallace & Gromit".into(), year: 2005, ..Default::default() });
        assert_eq!(subtitle(Kind::Movie, &film, ""), "2005");
        assert_eq!(subtitle(Kind::Movie, &film, "friend"), "2005 \u{b7} friend");

        let ep = Item::Media(PmsMovie {
            kind: 3,
            title: "A Grand Day Out".into(),
            show_title: "Wallace & Gromit".into(),
            season_index: 1,
            ep_index: 3,
            ..Default::default()
        });
        assert_eq!(subtitle(Kind::Episode, &ep, ""), "Wallace & Gromit \u{b7} S1, E3", "an episode says where it lives");

        // no year, no show, nothing to say — and then a handle stands alone rather than opening the
        // line with a stray dot
        let bare = Item::Media(PmsMovie { title: "Untitled".into(), ..Default::default() });
        assert_eq!(subtitle(Kind::Movie, &bare, ""), "");
        assert_eq!(subtitle(Kind::Movie, &bare, "friend"), "friend");

        let person = Item::Tag(TagHit { name: "Peter Sallis".into(), count: 9, ..Default::default() });
        assert_eq!(subtitle(Kind::Person, &person, ""), "", "a person's name is the whole fact");
        assert_eq!(subtitle(Kind::Collection, &Item::Tag(TagHit { count: 4, ..Default::default() }), ""), "4 items");
        assert_eq!(subtitle(Kind::Collection, &Item::Tag(TagHit { count: 1, ..Default::default() }), ""), "1 item");
    }
}
