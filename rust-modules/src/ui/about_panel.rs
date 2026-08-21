//! **About** — the detail page's About footer, read in full, as a glass alert panel.
//!
//! `Alert Views.dc.html` §1A. The About footer (detail section 5) is four columns; only the FIRST
//! one, the card, opens THIS panel. That is the design's rule and it is stated as a rule rather
//! than as a layout accident: *"a block earns an alert only when the page truncates something
//! worth reading in full."* The card truncates — its synopsis is capped at five `CAPTION` lines
//! with a right-pinned MORE. The other three do not, and each is settled by name:
//!
//! * **Accessibility** is three fixed definitions of CC/SDH/AD that never change and are already
//!   printed in full on the page — no alert.
//! * **Information** is *"four facts the detail screen already prints in full behind the panel, so
//!   they stay there"* — no alert, and (see below) no copy of them in this one either.
//! * **Languages** — the audio list — *"belongs beside the bitrates in Track information"* (§1B),
//!   so that column opens §1B rather than a panel of its own. `crate::ui::tracks_panel`.
//!
//! Do not give columns 1 or 3 an alert from this note. The absence is the design.
//!
//! # It holds the PROSE and nothing else, and that is a correction
//!
//! This panel shipped with a title, a genre line and a four-column facts grid under the synopsis,
//! on the earlier spec's reasoning that the Information facts *"ride along"* here rather than earn
//! a panel. **Both halves were wrong on screen and the design now says so.** The facts sat in the
//! panel while the page printed the same four, in the same order, in the Information column
//! *directly behind it* — visible around the panel's own edge, which is how the duplication was
//! spotted. The title and genres repeated the card the press came from, on the frame that card is
//! still showing them.
//!
//! So §1A is now: eyebrow, synopsis, tagline, hairline, footer. Everything the reader came for and
//! nothing they are already looking at. [`Blocks`] and [`Stack`] carry exactly those.
//!
//! # What it is made of
//!
//! One [`Popover`] on the shared open/appear choreography, one glass ground, and a fixed vertical
//! ladder of runs — no list, no focus, no control. The only key it answers is BACK (and OK, see
//! [`on_ok`]), because §1E states the family rule: *"only 1D carries a control — the read-only
//! panels close on BACK."* So the closing `Press [BACK] to return` line is not a hint, it is the
//! whole affordance, and it is the shared [`crate::ui::widgets::KeyHint`].
//!
//! # Two things about it that are decisions rather than defaults
//!
//! **The height is CONTENT-DRIVEN.** The mock hints 1120×360; that is a canvas placeholder and the
//! content div under it carries no height at all. A real PMS synopsis runs well past three lines,
//! and the whole reason this panel exists is that the card CUT one — so a panel that cut it again
//! at a different width would be pointless. The panel grows, and [`syn_lines`] is the last resort:
//! it spends whatever is left inside the screen's glass keep-out and never less than one line.
//!
//! **It is the only glass on its frame, and it charges past the region budget.** Stated rather than
//! hidden, in the design's own words: every one of these panels does, once grown 88px a side — and
//! that is accepted because [`crate::gfx::GLASS_REGION_BUDGET`] prices a MOVING host, while the page
//! under an open alert is standing still. The detail page behind this one is not scrolling, its
//! shelves are not springing and its hero is not advancing: nothing under the panel changes, so the
//! one cached snapshot [`crate::ui::widgets::Glass::CACHED`] takes on open stays true for as long as
//! the panel is up. A panel over a moving page would owe `Glass::DYNAMIC_BACKDROP` and a real
//! per-frame cost; this one owes one capture. It also clears the design's `--glass-edge-clear` 68 on
//! all four sides ([`EDGE_CLEAR`]), which is what keeps the blur's own sample window on the page
//! rather than off the edge of it.

use crate::metadata;
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::KeyHint;
use crate::ui::{Painter, Rect};
use std::ptr::{addr_of, addr_of_mut};

// ---- the frame -------------------------------------------------------------------------------

/// The mock's panel width. Wide enough that a synopsis wraps at a comfortable measure rather than
/// at a poster's width, and narrow enough to leave the page visible either side of it.
const PANEL_W: f32 = 1120.0;
/// The mock's padding, uniform on all four sides.
const PAD: f32 = 48.0;
/// The panel's inner content column — every run wraps to this.
pub(crate) const CONTENT_W: f32 = PANEL_W - 2.0 * PAD;
/// The design system's `--glass-edge-clear`: the panel never comes closer than this to any screen
/// edge. It is a property of the MATERIAL, not taste — a backdrop blur samples a window around its
/// own rect, and a panel flush to an edge has nothing on one side to sample.
const EDGE_CLEAR: f32 = 68.0;
/// The modal scrim's peak alpha — the mock's `scrimStill: 0.46`, shared by all four alerts.
const SCRIM_A: f32 = 0.46;
/// How far below its resting place the panel starts its entry slide.
const RISE: f32 = Popover::RISE;

// ---- the ladder ------------------------------------------------------------------------------
//
// These are the mock's OWN margins and line heights, verbatim, and they are deliberately not
// `theme::space` rungs. That ladder (XS 8 / SM 16 / MD 24 / LG 40 / XL 64) is the app's INTER-BLOCK
// rhythm on a page; a panel's internal rhythm is finer than a page's and lands between rungs — the
// tagline's 20 and the family's 14 and 12 (`theme::alert`) all sit where no rung does, and rounding
// them to one would change the design rather than tokenise it. They are named here, in one block,
// for the reason the rung ladder exists: one value per role, not a literal per call site. The two
// this panel shares with §1B and §1C are named in `theme::alert` instead, because a value two
// panels spend is a family token and drifted the moment it was not.
//
// Each gap belongs to the block BELOW it (CSS `margin-top`), which is what makes an absent block
// cost nothing: drop the run and its gap goes with it, and the block after it keeps its own.

/// `line-height: 1` on the eyebrow — a caps run has no descenders to clear.
const EYEBROW_LEAD: f32 = theme::alert::EYEBROW_LEAD;
/// `1.32` on the tertiary one-liner under the prose (the tagline).
const FINE_LEAD: f32 = 32.0; // size::CAPTION × 1.32
/// `1.5` — reading leading, and the loosest in the panel because the synopsis is the only run here
/// anyone reads a paragraph of.
const SYN_LEAD: f32 = 42.0; // size::BODY × 1.5

/// The eyebrow labels the prose directly now that no title stands between them, so it spends the
/// spec's `margin-top:24` rather than the family's tighter eyebrow→TITLE step
/// ([`theme::alert::GAP_EYEBROW_TITLE`], which §1B and §1C still take).
const G_SYNOPSIS: f32 = 24.0;
const G_TAGLINE: f32 = 20.0;
const G_RULE: f32 = 32.0;
const G_FOOTER: f32 = 24.0;

/// A hairline is one physical pixel of [`theme::HAIRLINE`] and occupies one pixel of flow.
const RULE_H: f32 = 1.0;

// ---- the pure layout -------------------------------------------------------------------------

/// The measured extents of the two blocks whose height depends on the ITEM — everything that needs
/// a font open, resolved by the caller so [`stack`] is arithmetic the host suite can grade without
/// one.
///
/// **0.0 means the block is ABSENT, not empty**, and the two are different: an absent block costs
/// neither its own height nor the gap above it, so a film with no tagline does not sit over a
/// reserved hole. Both fields are conditional in real data — episodes carry no tagline, and a
/// `/children`-borrowed show can carry no summary.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct Blocks {
    pub(crate) synopsis: f32,
    pub(crate) tagline: f32,
}

/// Where every block lands, panel-LOCAL (y = 0 is the panel's top edge), plus the panel's own outer
/// height. Each y is the block's TOP: for a text run that is its first line's cap band, which is
/// what `TextView`/`Label` take; for the footer it is the top of the key cap's band.
///
/// A y of 0.0 means the block is not drawn — no block can legitimately land there, since the first
/// one starts at [`PAD`].
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct Stack {
    pub(crate) eyebrow: f32,
    pub(crate) synopsis: f32,
    pub(crate) tagline: f32,
    pub(crate) rule: f32,
    pub(crate) footer: f32,
    pub(crate) h: f32,
}

/// Resolve the ladder for one item's measured blocks.
///
/// **The one hairline is the FOOTER's**, and it is unconditional. It closes the prose whatever the
/// prose turned out to be — an item with no summary and no tagline still gets a rule over its
/// footer rather than a keycap floating under an eyebrow. (The panel used to carry three rules
/// bracketing a facts grid, with the grid's own two coming and going with it; with the grid gone
/// there is one block to close and one rule to close it.)
pub(crate) fn stack(b: Blocks) -> Stack {
    let mut s = Stack::default();
    let mut y = PAD;

    s.eyebrow = y;
    y += EYEBROW_LEAD;

    if b.synopsis > 0.0 {
        s.synopsis = y + G_SYNOPSIS;
        y = s.synopsis + b.synopsis;
    }

    if b.tagline > 0.0 {
        s.tagline = y + G_TAGLINE;
        y = s.tagline + b.tagline;
    }

    s.rule = y + G_RULE;
    y = s.rule + RULE_H;

    s.footer = y + G_FOOTER;
    y = s.footer + KeyHint::height();

    s.h = y + PAD;
    s
}

/// The tallest the panel is allowed to be — the screen less the glass keep-out on both edges.
pub(crate) const fn max_panel_h() -> f32 {
    SCR_H - 2.0 * EDGE_CLEAR
}

/// How many lines of synopsis this item can be given before the panel would outgrow the screen.
///
/// **The panel grows to fit the prose; this is only the floor under that.** The card on the page
/// already caps the synopsis at five lines, and a panel that capped it again would have no reason
/// to exist — so the budget is "whatever is left", computed by laying the ladder out with NO
/// synopsis and spending the remainder. It never returns 0: a panel whose whole subject is the
/// synopsis must show a line of it even if the arithmetic says there is no room, and one clipped
/// line is a better failure than none.
///
/// `b.synopsis` is ignored (that is the number being solved for); pass the block set you will
/// eventually draw, with the tagline already measured.
///
/// Solved from a ONE-LINE panel rather than from a synopsis-less one, because `synopsis: 0.0` means
/// ABSENT to [`stack`] — it would take [`G_SYNOPSIS`] out of the ladder with it and hand the budget
/// a gap that the drawn panel is going to spend. The floor falls out of the same expression: the
/// first line is already in `one`, so a negative remainder still leaves it.
pub(crate) fn syn_lines(b: Blocks) -> usize {
    let one = stack(Blocks { synopsis: SYN_LEAD, ..b }).h;
    let extra = ((max_panel_h() - one) / SYN_LEAD).floor();
    if extra.is_finite() && extra > 0.0 {
        1 + extra as usize
    } else {
        1
    }
}

/// The panel's rect on screen: CENTRED, both ways, and never inside the glass keep-out.
///
/// The mock places it at `left:400` on a 1920 frame, which is dead centre for a 1120 sheet — so
/// centring is the rule and the mock's own left edge is one instance of it. Its TOP is not read
/// off the canvas at all: the hinted height is a placeholder over a content div that carries none,
/// and a content-driven height means the top is solved rather than authored. (The test below still
/// checks the pair against a 520 sheet, because 400/280 is the arithmetic, not the artefact.)
pub(crate) fn panel_rect(content_h: f32) -> Rect {
    let h = content_h.min(max_panel_h());
    Rect::new((SCR_W - PANEL_W) * 0.5, ((SCR_H - h) * 0.5).max(EDGE_CLEAR), PANEL_W, h)
}

// ---- state -----------------------------------------------------------------------------------

static mut POP: Popover = Popover::new();

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// Open it over the page. Repaints — a modal appearing is exactly the discrete change
/// [`crate::ui::idle`] cannot see from a spring alone on its first frame.
pub(crate) fn open() {
    pop().open();
    crate::ui::idle::invalidate();
}

pub(crate) fn close() {
    if is_open() {
        pop().close();
        crate::ui::idle::invalidate();
    }
}

/// OK while the panel is up.
///
/// **It closes.** The design says only BACK, and the panel carries no control for OK to commit — but
/// on a remote OK is the primary button, and a modal that answers it with nothing is the one dead
/// key on the screen. Closing is the superset: BACK still closes, and nothing else can be reached
/// from here for OK to mean instead. Reported rather than performed, so the page's key ladder keeps
/// deciding whether the press was spent.
pub(crate) fn on_ok() {
    close();
}

pub(crate) fn update(dt: f32) {
    pop().update(dt);
}

// ---- draw ------------------------------------------------------------------------------------

/// The modal dim, drawn by the HOST PAGE rather than by the panel.
///
/// Pairs with [`draw`], which uses `Popover::content_painter` and so draws no scrim of its own —
/// calling `Popover::painter` instead would dim twice. The split is the rule `ui/CLAUDE.md` states
/// and `account_menu` is the worked example of: the scrim sits between the page and the panel, so it
/// is part of what the glass looks through, and a dim drawn WITH the panel reaches the visible frame
/// but not the blur's source. This panel's policy is `CACHED`, which grabs framebuffer 0 and would
/// survive either placement — it is written the right way round anyway, because the placement is a
/// property of where a scrim BELONGS and not of which capture path today's policy happens to take.
///
/// Nothing is LIFTED back out of the dim. `Popover::scrim_lifting` exists for a panel that is ABOUT
/// an element still on screen; this one is about the card it was opened from and says everything the
/// card says, at length — lifting it would put a truncated copy of this panel's own first three runs
/// alongside it. The mock lifts nothing either.
pub(crate) fn draw_scrim() {
    if is_open() {
        pop().scrim(SCRIM_A);
    }
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let Some(d) = metadata::current() else {
        return;
    };

    // ---- measure ----
    // The tagline first, because the synopsis's line budget is what is LEFT after it.
    let mut b = Blocks {
        synopsis: 0.0,
        tagline: if d.tagline.is_empty() { 0.0 } else { FINE_LEAD },
    };
    let syn = TextView::new(&d.summary, theme::size::BODY, theme::TEXT_READING)
        .leading(SYN_LEAD)
        .max_lines(syn_lines(b));
    b.synopsis = if d.summary.is_empty() { 0.0 } else { syn.measure_h(CONTENT_W) };
    let s = stack(b);
    let r = panel_rect(s.h);

    // ---- ground ----
    let p = pop().content_painter(RISE);
    pop().panel(p, r, theme::ALERT_PANEL_RAD);

    // ---- content ----
    let cx = r.x + PAD;
    let run = |text: &str, y: f32, sz, lead: f32, col, bold| {
        let mut v = TextView::new(text, sz, col).leading(lead).max_lines(1);
        if bold {
            v = v.bold();
        }
        v.draw(p, Rect::new(cx, r.y + y, CONTENT_W, 0.0));
    };

    run("ABOUT", s.eyebrow, theme::size::CAPTION, EYEBROW_LEAD, theme::TEXT_TERTIARY, true);
    if s.synopsis > 0.0 {
        syn.draw(p, Rect::new(cx, r.y + s.synopsis, CONTENT_W, 0.0));
    }
    if s.tagline > 0.0 {
        run(&d.tagline, s.tagline, theme::size::CAPTION, FINE_LEAD, theme::TEXT_TERTIARY, false);
    }
    rule(p, r, s.rule);

    let hint = KeyHint::new(c"Press", c"BACK", c"to return");
    hint.draw(p, r.x + r.w - PAD - hint.width(), r.y + s.footer + KeyHint::height() * 0.5);
}

/// One full-width hairline across the content column.
fn rule(p: Painter, r: Rect, y: f32) {
    p.rect(Rect::new(r.x + PAD, r.y + y, CONTENT_W, RULE_H), 0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
}

// ---- pointer ---------------------------------------------------------------------------------

/// A click anywhere dismisses, which is what a click outside a popover means everywhere in this
/// app — and here it is what a click INSIDE one means too, since the panel holds nothing to hit.
pub(crate) fn click(_mx: f32, _my: f32) {
    close();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder in the mock's own order, with every block present: each block sits below the one
    /// above it by exactly its gap, and the panel's height closes with the bottom padding.
    #[test]
    fn the_stack_lays_the_mocks_ladder_out_in_order() {
        let b = Blocks { synopsis: 3.0 * SYN_LEAD, tagline: FINE_LEAD };
        let s = stack(b);
        assert_eq!(s.eyebrow, PAD, "the first block starts at the padding");
        assert_eq!(s.synopsis, s.eyebrow + EYEBROW_LEAD + G_SYNOPSIS);
        assert_eq!(s.tagline, s.synopsis + b.synopsis + G_TAGLINE);
        assert_eq!(s.rule, s.tagline + b.tagline + G_RULE);
        assert_eq!(s.footer, s.rule + RULE_H + G_FOOTER);
        assert_eq!(s.h, s.footer + KeyHint::height() + PAD);
        // the whole point of the mock's shape: it fits, with room to spare
        assert!(s.h < max_panel_h(), "the reference item's panel is {} against a {} ceiling", s.h, max_panel_h());
    }

    /// **Nothing the page is already showing behind the panel is in it.** The panel is the prose,
    /// so its ladder has one text block above the tagline and no second one — no title, no genre
    /// line, and above all no copy of the four Information facts, which the detail page prints in
    /// full in the column directly behind this sheet.
    ///
    /// Graded on the ladder's own arithmetic, which is the only place a re-added block could hide:
    /// the eyebrow's next neighbour is the synopsis, and the panel's height is exactly what its
    /// five steps add up to. A block spliced back in would have to move one of the two.
    #[test]
    fn the_panel_holds_the_prose_and_nothing_the_page_already_prints() {
        let b = Blocks { synopsis: 3.0 * SYN_LEAD, tagline: FINE_LEAD };
        let s = stack(b);
        assert_eq!(
            s.synopsis - s.eyebrow,
            EYEBROW_LEAD + G_SYNOPSIS,
            "the eyebrow labels the PROSE — nothing is placed between them"
        );
        assert_eq!(
            s.h,
            PAD + EYEBROW_LEAD + G_SYNOPSIS + b.synopsis + G_TAGLINE + b.tagline + G_RULE + RULE_H
                + G_FOOTER + KeyHint::height() + PAD,
            "the panel is exactly eyebrow · prose · tagline · rule · footer"
        );
    }

    /// An ABSENT block costs neither its height nor the gap above it — a film with no tagline must
    /// not sit over a reserved hole, and the block after it keeps its own gap. This is the whole
    /// reason each gap belongs to the block below it.
    #[test]
    fn an_absent_block_takes_its_gap_with_it() {
        let full = Blocks { synopsis: SYN_LEAD, tagline: FINE_LEAD };

        let no_tag = stack(Blocks { tagline: 0.0, ..full });
        assert_eq!(no_tag.tagline, 0.0, "an absent block is not placed");
        assert_eq!(
            stack(full).h - no_tag.h,
            FINE_LEAD + G_TAGLINE,
            "dropping the tagline drops its own height AND its gap"
        );
        assert_eq!(no_tag.rule, no_tag.synopsis + SYN_LEAD + G_RULE, "the rule keeps its own gap");

        // …and the degenerate item — no summary AND no tagline — still closes with a rule over its
        // footer rather than a keycap hanging under the eyebrow.
        let bare = stack(Blocks::default());
        assert_eq!((bare.synopsis, bare.tagline), (0.0, 0.0));
        assert_eq!(bare.rule, PAD + EYEBROW_LEAD + G_RULE, "the footer's rule is unconditional");
        assert!(bare.footer > bare.rule);
    }

    /// The synopsis budget spends what is LEFT and is never zero.
    ///
    /// Three cases, and the middle one is the regression that matters: a panel whose other blocks
    /// have grown must give the synopsis FEWER lines, never a negative or a panic. The floor case
    /// pins the "one clipped line beats none" rule — a panel whose entire subject is the synopsis
    /// cannot show none of it.
    #[test]
    fn the_synopsis_is_given_whatever_room_is_left_and_never_none() {
        let base = Blocks { synopsis: 0.0, tagline: FINE_LEAD };
        let n = syn_lines(base);
        assert!(n >= 3, "the reference item's ladder leaves room for a real paragraph, got {n}");
        // the budget is exactly what fits
        assert!(stack(Blocks { synopsis: n as f32 * SYN_LEAD, ..base }).h <= max_panel_h());
        assert!(stack(Blocks { synopsis: (n + 1) as f32 * SYN_LEAD, ..base }).h > max_panel_h());

        // a taller neighbouring block buys the synopsis fewer lines, monotonically
        let taller = syn_lines(Blocks { tagline: FINE_LEAD + 4.0 * SYN_LEAD, ..base });
        assert!(taller < n, "growing another block must cost the synopsis lines: {taller} vs {n}");

        // and the floor holds even when the arithmetic says there is no room at all
        assert_eq!(syn_lines(Blocks { tagline: 10_000.0, ..base }), 1);
    }

    /// **What the panel costs the blur chain, pinned — because dropping the repeated blocks was
    /// also the cheapest thing available for the frame-rate complaint that came with them.**
    ///
    /// A glass surface's price is its snapshot REGION: its own rect grown by `gfx::BLUR_MARGIN` on
    /// every side, which the chain then reduces twice and up-filters. Removing the title, the genre
    /// line, a hairline and the four-column facts grid took **300px** off the reference item's
    /// panel (715 → 415), and because the region grows in one dimension only — the width is fixed
    /// at [`PANEL_W`] — the whole of that comes off the region: **1.15M authored px² → 0.77M, a
    /// third less.**
    ///
    /// Graded here rather than described, with the OLD number written down, because the failure
    /// mode is silent and specific: a block spliced back into [`stack`] re-inflates the region with
    /// nothing on screen to say so, and the cost lands on a television nobody here owns. The
    /// ceiling is deliberately not `gfx::GLASS_REGION_BUDGET` (300k) — every alert in the family
    /// charges past that, in the design's own words, because the budget prices a MOVING host and
    /// the page under a modal is standing still. This grades the direction of travel instead.
    ///
    /// It says nothing about frames per second, and cannot: the region is authored geometry, the
    /// frame rate is the SM9000's Mali, and the two are joined only by a measurement on the set.
    #[test]
    fn the_prose_only_panel_costs_the_blur_chain_a_third_less_region() {
        // the reference item: a 3-line synopsis and a tagline
        let b = Blocks { synopsis: 3.0 * SYN_LEAD, tagline: FINE_LEAD };
        let r = panel_rect(stack(b).h);
        let reg = crate::gfx::blur_region(r.x, r.y, r.w, r.h);
        let area = reg[2] * reg[3];

        // what the same item cost while the panel also carried a title, genres, a second hairline
        // and the facts grid — 300px of ladder, all of it height, and still centred
        let was = crate::gfx::blur_region(r.x, r.y - 150.0, r.w, r.h + 300.0);
        let was_area = was[2] * was[3];

        assert!(
            area < was_area * 0.72,
            "the prose-only panel should cost a third less region: {area} against {was_area}"
        );
        // …and the panel it is measuring is the real one, not an arithmetic slip: a 1120-wide sheet
        // grown by the margin either side, still inside the frame.
        assert_eq!(reg[2], PANEL_W + 2.0 * crate::gfx::BLUR_MARGIN);
        assert!(reg[1] >= 0.0 && reg[1] + reg[3] <= crate::ui::consts::SCR_H);
    }

    /// The panel is centred both ways and never enters the glass keep-out — including when its
    /// content is taller than the screen, which is the case the `min` exists for.
    #[test]
    fn the_panel_is_centred_and_clears_the_glass_keep_out() {
        let r = panel_rect(520.0);
        assert_eq!(r.w, PANEL_W);
        assert_eq!(r.h, 520.0);
        assert!((r.x - 400.0).abs() < 0.001, "the mock's own left:400 falls out of centring, got {}", r.x);
        assert!((r.y - 280.0).abs() < 0.001, "…and a 520 sheet's own top, got {}", r.y);

        // a tall panel is clamped rather than allowed to run off the frame
        let tall = panel_rect(5_000.0);
        assert_eq!(tall.h, max_panel_h());
        assert!(tall.y >= EDGE_CLEAR - 0.001, "top edge clear: {}", tall.y);
        assert!(tall.y + tall.h <= SCR_H - EDGE_CLEAR + 0.001, "bottom edge clear");
        assert!(tall.x >= EDGE_CLEAR && tall.x + tall.w <= SCR_W - EDGE_CLEAR, "side edge clear");
    }
}
