//! **About** — the detail page's About footer, read in full, as a glass alert panel.
//!
//! `Alert Views.dc.html` §1A. The About footer (detail section 5) is four columns; only the FIRST
//! one, the card, opens anything. That is the design's rule and it is stated as a rule rather than
//! as a layout accident: *"a block earns an alert only when the page truncates something worth
//! reading in full."* The card truncates — its synopsis is capped at five `CAPTION` lines with a
//! right-pinned MORE, and its title and genres are elided to the card frame. The other three do
//! not, and each is refused for its own reason:
//!
//! * **Accessibility** is three fixed definitions of CC/SDH/AD that never change and are already
//!   printed in full on the page.
//! * **Information** is four facts the hero line already carries — so rather than get a panel of
//!   their own they RIDE ALONG here, under the synopsis, which is why this panel has a facts grid
//!   at all.
//! * **Languages** — the audio list — belongs beside the bitrates in Track information (§1B), not
//!   in a panel of its own.
//!
//! Do not add an alert to columns 1–3 from this note. The absence is the design.
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
//! **The height is CONTENT-DRIVEN.** The mock hints 1120×520; 520 is a canvas placeholder and the
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
/// The panel's inner content column — every run and the facts grid wrap to this.
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
// rhythm on a page; a panel's internal rhythm is finer than a page's and lands between rungs at the
// top of it — 14px from an eyebrow to its title, 12px from the title to its subtitle — where no
// rung sits and rounding to one would change the design rather than tokenise it. They are named
// here, in one block, for the reason the rung ladder exists: one value per role, not a literal per
// call site.
//
// Each gap belongs to the block BELOW it (CSS `margin-top`), which is what makes an absent block
// cost nothing: drop the run and its gap goes with it, and the block after it keeps its own.

/// `line-height: 1` on the eyebrow — a caps run has no descenders to clear.
const EYEBROW_LEAD: f32 = 24.0; // size::CAPTION × 1
/// `1.1` — tight, because the title is one line by construction.
const TITLE_LEAD: f32 = 44.0; // size::TITLE × 1.1
/// `1.32` on the two tertiary one-liners (genres, tagline).
const FINE_LEAD: f32 = 32.0; // size::CAPTION × 1.32
/// `1.5` — reading leading, and the loosest in the panel because the synopsis is the only run here
/// anyone reads a paragraph of.
const SYN_LEAD: f32 = 42.0; // size::BODY × 1.5
/// `1.25` on both halves of a fact.
const FACT_LABEL_LEAD: f32 = 30.0; // size::CAPTION × 1.25
const FACT_VALUE_LEAD: f32 = 32.0; // size::LABEL × 1.25 (32.5, held to a whole pixel)

const G_TITLE: f32 = 14.0;
const G_GENRES: f32 = 12.0;
const G_RULE_A: f32 = 28.0;
const G_SYNOPSIS: f32 = 28.0;
const G_TAGLINE: f32 = 20.0;
const G_RULE_B: f32 = 32.0;
const G_FACTS: f32 = 32.0;
const G_RULE_C: f32 = 32.0;
const G_FOOTER: f32 = 24.0;

/// A hairline is one physical pixel of [`theme::HAIRLINE`] and occupies one pixel of flow.
const RULE_H: f32 = 1.0;
/// Between a fact's label and its value.
const FACT_GAP_Y: f32 = 6.0;
/// Between fact columns.
const FACT_GAP_X: f32 = 32.0;

/// The facts grid is **always four columns**, whatever the server sent.
///
/// `about_rows` yields between one and four facts — Released and Regions of Origin are both
/// conditional, Rated is always present — and the design draws four. Sizing the grid to the count
/// would make the same panel a different shape per item, so the columns are fixed and a short list
/// simply leaves the right-hand ones empty. It also keeps the values in the same place on every
/// page, which is the point of a grid.
pub(crate) const FACT_COLS: usize = 4;

/// One fact column's width.
pub(crate) const fn fact_col_w() -> f32 {
    (CONTENT_W - (FACT_COLS as f32 - 1.0) * FACT_GAP_X) / FACT_COLS as f32
}

/// Column `i`'s x offset from the content column's left edge.
pub(crate) const fn fact_col_x(i: usize) -> f32 {
    i as f32 * (fact_col_w() + FACT_GAP_X)
}

// ---- the pure layout -------------------------------------------------------------------------

/// The measured extents of the four blocks whose height depends on the ITEM — everything that needs
/// a font open, resolved by the caller so [`stack`] is arithmetic the host suite can grade without
/// one.
///
/// **0.0 means the block is ABSENT, not empty**, and the two are different: an absent block costs
/// neither its own height nor the gap above it, so a film with no tagline does not sit over a
/// reserved hole. Every field here is conditional in real data — episodes carry no tagline, a
/// hand-added item can carry no genres, and a `/children`-borrowed show can carry no summary.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct Blocks {
    pub(crate) genres: f32,
    pub(crate) synopsis: f32,
    pub(crate) tagline: f32,
    /// The TALLEST of the four fact columns — the grid's rows stretch, so one two-line value
    /// ("United States of America" wraps at this column width) sets the whole band.
    pub(crate) facts: f32,
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
    pub(crate) title: f32,
    pub(crate) genres: f32,
    pub(crate) rule_a: f32,
    pub(crate) synopsis: f32,
    pub(crate) tagline: f32,
    pub(crate) rule_b: f32,
    pub(crate) facts: f32,
    pub(crate) rule_c: f32,
    pub(crate) footer: f32,
    pub(crate) h: f32,
}

/// Resolve the ladder for one item's measured blocks.
///
/// The two hairlines that BRACKET the facts grid come and go WITH it. With no facts at all the
/// grid's leading rule would be a second hairline immediately under the closing one of the prose
/// block — two rules with nothing between them, which reads as a mistake rather than as a section
/// that happens to be empty. The trailing rule stays, because it is the footer's rule as much as
/// the grid's.
pub(crate) fn stack(b: Blocks) -> Stack {
    let mut s = Stack::default();
    let mut y = PAD;

    s.eyebrow = y;
    y += EYEBROW_LEAD;

    s.title = y + G_TITLE;
    y = s.title + TITLE_LEAD;

    if b.genres > 0.0 {
        s.genres = y + G_GENRES;
        y = s.genres + b.genres;
    }

    s.rule_a = y + G_RULE_A;
    y = s.rule_a + RULE_H;

    s.synopsis = y + G_SYNOPSIS;
    y = s.synopsis + b.synopsis;

    if b.tagline > 0.0 {
        s.tagline = y + G_TAGLINE;
        y = s.tagline + b.tagline;
    }

    if b.facts > 0.0 {
        s.rule_b = y + G_RULE_B;
        y = s.rule_b + RULE_H;
        s.facts = y + G_FACTS;
        y = s.facts + b.facts;
    }

    s.rule_c = y + G_RULE_C;
    y = s.rule_c + RULE_H;

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
/// eventually draw, with the other three already measured.
pub(crate) fn syn_lines(b: Blocks) -> usize {
    let fixed = stack(Blocks { synopsis: 0.0, ..b }).h;
    let avail = max_panel_h() - fixed;
    let n = (avail / SYN_LEAD).floor();
    if n.is_finite() && n >= 1.0 {
        n as usize
    } else {
        1
    }
}

/// The panel's rect on screen: CENTRED, both ways, and never inside the glass keep-out.
///
/// The mock places it at `left:400 top:280` for a 1120×520 sheet on a 1920×1080 frame, which is
/// dead centre on both axes — so centring is the rule and those two numbers are one instance of it.
/// A content-driven height means the top is solved rather than authored.
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
    // The four facts come from the page's own per-item cache — the same strings the Information
    // column draws, never a second derivation of them.
    let info = crate::ui::detail::about_info(d);

    // Every one-line run is a `max_lines(1)` `TextView`, which word-wraps and then ellipsizes what
    // it could not place — so the ellipsis is the view's, not a second `text::elide` pass over the
    // same string with the same budget. Only the eyebrow is safe by construction.
    let genres = d.genres.join(", ");

    // ---- measure ----
    // Everything except the synopsis first, because the synopsis's line budget is what is LEFT.
    let facts_h = facts_height(info);
    let mut b = Blocks {
        genres: if genres.is_empty() { 0.0 } else { FINE_LEAD },
        synopsis: 0.0,
        tagline: if d.tagline.is_empty() { 0.0 } else { FINE_LEAD },
        facts: facts_h,
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
    run(&d.title, s.title, theme::size::TITLE, TITLE_LEAD, theme::TEXT_PRIMARY, true);
    if s.genres > 0.0 {
        run(&genres, s.genres, theme::size::CAPTION, FINE_LEAD, theme::TEXT_TERTIARY, false);
    }
    rule(p, r, s.rule_a);
    if b.synopsis > 0.0 {
        syn.draw(p, Rect::new(cx, r.y + s.synopsis, CONTENT_W, 0.0));
    }
    if s.tagline > 0.0 {
        run(&d.tagline, s.tagline, theme::size::CAPTION, FINE_LEAD, theme::TEXT_TERTIARY, false);
    }
    if s.facts > 0.0 {
        rule(p, r, s.rule_b);
        draw_facts(p, cx, r.y + s.facts, info);
    }
    rule(p, r, s.rule_c);

    let hint = KeyHint::new(c"Press", c"BACK", c"to return");
    hint.draw(p, r.x + r.w - PAD - hint.width(), r.y + s.footer + KeyHint::height() * 0.5);
}

/// One full-width hairline across the content column.
fn rule(p: Painter, r: Rect, y: f32) {
    p.rect(Rect::new(r.x + PAD, r.y + y, CONTENT_W, RULE_H), 0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
}

/// The grid's band height — the tallest column, since the rows stretch.
fn facts_height(info: &[(&'static str, String)]) -> f32 {
    let mut h: f32 = 0.0;
    for (_, value) in info.iter().take(FACT_COLS) {
        h = h.max(FACT_LABEL_LEAD + FACT_GAP_Y + fact_value(value).measure_h(fact_col_w()));
    }
    h
}

/// A fact's VALUE as its view — built in one place so the measure and the draw cannot wrap it two
/// different ways.
fn fact_value(value: &str) -> TextView<'_> {
    TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING).bold().leading(FACT_VALUE_LEAD)
}

fn draw_facts(p: Painter, cx: f32, top: f32, info: &[(&'static str, String)]) {
    for (i, (label, value)) in info.iter().take(FACT_COLS).enumerate() {
        let x = cx + fact_col_x(i);
        TextView::new(label, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .leading(FACT_LABEL_LEAD)
            .max_lines(1)
            .draw(p, Rect::new(x, top, fact_col_w(), 0.0));
        fact_value(value).draw(p, Rect::new(x, top + FACT_LABEL_LEAD + FACT_GAP_Y, fact_col_w(), 0.0));
    }
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

    /// The four fact columns are EQUAL, gapped, and exactly fill the content column — the property
    /// that makes a short fact list leave a hole on the right rather than a wider column on the
    /// left. Graded on the arithmetic rather than on a screenshot, because a rounding error here is
    /// a two-pixel drift nobody sees on a device and a ragged column the moment the panel width
    /// moves.
    #[test]
    fn the_facts_grid_splits_the_content_column_into_four_equal_gapped_columns() {
        let w = fact_col_w();
        assert!(w > 0.0);
        // every column is the same width, and consecutive columns are exactly FACT_GAP_X apart
        for i in 1..FACT_COLS {
            let gap = fact_col_x(i) - (fact_col_x(i - 1) + w);
            assert!((gap - FACT_GAP_X).abs() < 0.001, "column {i} gap {gap} != {FACT_GAP_X}");
        }
        assert!(fact_col_x(0).abs() < 0.001, "column 0 starts at the content column's left edge");
        let right = fact_col_x(FACT_COLS - 1) + w;
        assert!((right - CONTENT_W).abs() < 0.001, "the last column ends flush at {CONTENT_W}, got {right}");
    }

    /// The ladder in the mock's own order, with every block present: each block sits below the one
    /// above it by exactly its gap, and the panel's height closes with the bottom padding.
    #[test]
    fn the_stack_lays_the_mocks_ladder_out_in_order() {
        let b = Blocks { genres: FINE_LEAD, synopsis: 3.0 * SYN_LEAD, tagline: FINE_LEAD, facts: 101.0 };
        let s = stack(b);
        assert_eq!(s.eyebrow, PAD, "the first block starts at the padding");
        assert_eq!(s.title, s.eyebrow + EYEBROW_LEAD + G_TITLE);
        assert_eq!(s.genres, s.title + TITLE_LEAD + G_GENRES);
        assert_eq!(s.rule_a, s.genres + b.genres + G_RULE_A);
        assert_eq!(s.synopsis, s.rule_a + RULE_H + G_SYNOPSIS);
        assert_eq!(s.tagline, s.synopsis + b.synopsis + G_TAGLINE);
        assert_eq!(s.rule_b, s.tagline + b.tagline + G_RULE_B);
        assert_eq!(s.facts, s.rule_b + RULE_H + G_FACTS);
        assert_eq!(s.rule_c, s.facts + b.facts + G_RULE_C);
        assert_eq!(s.footer, s.rule_c + RULE_H + G_FOOTER);
        assert_eq!(s.h, s.footer + KeyHint::height() + PAD);
        // the whole point of the mock's shape: it fits, with room to spare
        assert!(s.h < max_panel_h(), "the reference item's panel is {} against a {} ceiling", s.h, max_panel_h());
    }

    /// An ABSENT block costs neither its height nor the gap above it — a film with no tagline must
    /// not sit over a reserved hole, and the block after it keeps its own gap. This is the whole
    /// reason each gap belongs to the block below it.
    #[test]
    fn an_absent_block_takes_its_gap_with_it() {
        let full = Blocks { genres: FINE_LEAD, synopsis: SYN_LEAD, tagline: FINE_LEAD, facts: 60.0 };

        let no_tag = stack(Blocks { tagline: 0.0, ..full });
        assert_eq!(no_tag.tagline, 0.0, "an absent block is not placed");
        assert_eq!(
            stack(full).h - no_tag.h,
            FINE_LEAD + G_TAGLINE,
            "dropping the tagline drops its own height AND its gap"
        );
        assert_eq!(no_tag.rule_b, no_tag.synopsis + SYN_LEAD + G_RULE_B, "the rule keeps its own gap");

        let no_genres = stack(Blocks { genres: 0.0, ..full });
        assert_eq!(no_genres.genres, 0.0);
        assert_eq!(no_genres.rule_a, no_genres.title + TITLE_LEAD + G_RULE_A);
        assert_eq!(stack(full).h - no_genres.h, FINE_LEAD + G_GENRES);
    }

    /// With no facts at all, BOTH the grid and the hairline that introduces it go — leaving one
    /// rule before the footer instead of two with nothing between them. (`about_rows` always pushes
    /// `Rated`, so this is the defensive case rather than the common one; it is graded because the
    /// alternative failure is two abutting hairlines, which reads as a bug rather than as an empty
    /// section.)
    #[test]
    fn no_facts_takes_the_grids_own_hairline_with_it() {
        let s = stack(Blocks { genres: FINE_LEAD, synopsis: SYN_LEAD, tagline: 0.0, facts: 0.0 });
        assert_eq!(s.facts, 0.0);
        assert_eq!(s.rule_b, 0.0, "the grid's leading rule goes with the grid");
        assert_eq!(s.rule_c, s.synopsis + SYN_LEAD + G_RULE_C, "one rule is left, and it closes the prose");
        assert!(s.rule_c > 0.0 && s.footer > s.rule_c);
    }

    /// The synopsis budget spends what is LEFT and is never zero.
    ///
    /// Three cases, and the middle one is the regression that matters: a panel whose other blocks
    /// have grown must give the synopsis FEWER lines, never a negative or a panic. The floor case
    /// pins the "one clipped line beats none" rule — a panel whose entire subject is the synopsis
    /// cannot show none of it.
    #[test]
    fn the_synopsis_is_given_whatever_room_is_left_and_never_none() {
        let base = Blocks { genres: FINE_LEAD, synopsis: 0.0, tagline: FINE_LEAD, facts: 101.0 };
        let n = syn_lines(base);
        assert!(n >= 3, "the reference item's ladder leaves room for a real paragraph, got {n}");
        // the budget is exactly what fits
        assert!(stack(Blocks { synopsis: n as f32 * SYN_LEAD, ..base }).h <= max_panel_h());
        assert!(stack(Blocks { synopsis: (n + 1) as f32 * SYN_LEAD, ..base }).h > max_panel_h());

        // a taller facts band buys the synopsis fewer lines, monotonically
        let taller = syn_lines(Blocks { facts: 101.0 + 4.0 * SYN_LEAD, ..base });
        assert!(taller < n, "growing another block must cost the synopsis lines: {taller} vs {n}");

        // and the floor holds even when the arithmetic says there is no room at all
        assert_eq!(syn_lines(Blocks { facts: 10_000.0, ..base }), 1);
    }

    /// The panel is centred both ways and never enters the glass keep-out — including when its
    /// content is taller than the screen, which is the case the `min` exists for.
    #[test]
    fn the_panel_is_centred_and_clears_the_glass_keep_out() {
        let r = panel_rect(520.0);
        assert_eq!(r.w, PANEL_W);
        assert_eq!(r.h, 520.0);
        assert!((r.x - 400.0).abs() < 0.001, "the mock's own left:400 falls out of centring, got {}", r.x);
        assert!((r.y - 280.0).abs() < 0.001, "…and its top:280, got {}", r.y);

        // a tall panel is clamped rather than allowed to run off the frame
        let tall = panel_rect(5_000.0);
        assert_eq!(tall.h, max_panel_h());
        assert!(tall.y >= EDGE_CLEAR - 0.001, "top edge clear: {}", tall.y);
        assert!(tall.y + tall.h <= SCR_H - EDGE_CLEAR + 0.001, "bottom edge clear");
        assert!(tall.x >= EDGE_CLEAR && tall.x + tall.w <= SCR_W - EDGE_CLEAR, "side edge clear");
    }
}
