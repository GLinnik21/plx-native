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
//!   so that column opens §1B rather than a panel of its own: the shared
//!   [`crate::screens::registry::AppArg::TracksPanel`] route.
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
//! One glass ground and a fixed vertical ladder of runs — no list, no focus, no control, and no
//! state of its own at all. The only key it answers is BACK (and OK, see [`AboutPanelScreen`]),
//! because §1E states the family rule: *"only 1D carries a control — the read-only panels close on
//! BACK."* So the closing `Press [BACK] to return` line is not a hint, it is the whole affordance,
//! and it is the shared [`crate::ui::widgets::KeyHint`].
//!
//! **A `Style::Alert` surface on the container tree** since restructure phase 10 (§6.2) — the
//! shape it always had, stated to the container instead of implied, and the FIRST conversion §0's
//! criterion 5 is proven on. Its `static mut POP` is gone: the container owns the phase, the appear
//! spring (`DrawFrame::page_alpha` IS that spring for a surface) and the dim
//! (`ModalStack::draw_scrims`, off [`AboutPanelScreen::scrim`]). Nothing on screen changes.
//!
//! **It is the panel with the least state in the app, and that is worth stating rather than
//! reading as an omission**: no cursor, no scroll, no selection — the sheet is one measured
//! ladder over `metadata::current()`. So its `LogicalState` writes nothing and its whole record in
//! a recording is the container's: which argument is present, in which phase. There is no UP/DOWN
//! here for a replay to be blind to, which is exactly what `tracks_panel`'s `page:i32` had to be in
//! its shape for.
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
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets;
use crate::ui::widgets::KeyHint;
use crate::ui::{Painter, Rect};

// ---- the frame -------------------------------------------------------------------------------

/// The mock's panel width. Wide enough that a synopsis wraps at a comfortable measure rather than
/// at a poster's width, and narrow enough to leave the page visible either side of it.
const PANEL_W: f32 = 1120.0;
/// The mock's padding, uniform on all four sides.
const PAD: f32 = theme::alert::PAD;
/// The panel's inner content column — every run wraps to this.
pub(crate) const CONTENT_W: f32 = PANEL_W - 2.0 * PAD;
/// The design system's `--glass-edge-clear`: the panel never comes closer than this to any screen
/// edge. It is a property of the MATERIAL, not taste — a backdrop blur samples a window around its
/// own rect, and a panel flush to an edge has nothing on one side to sample.
const EDGE_CLEAR: f32 = 68.0;
/// The modal scrim's peak alpha — the mock's `scrimStill: 0.46`, shared by all four alerts.
const SCRIM_A: f32 = theme::alert::SCRIM_A;

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
    y = s.rule + widgets::HAIRLINE_H;

    s.footer = y + G_FOOTER;
    y = s.footer + KeyHint::height();

    // …and the footer closes on ITS OWN pad, not the panel's: `KeyHint::pad_below` is `G_FOOTER`'s
    // twin, so the line sits centred in the band the rule opens. `PAD` here made the panel exactly
    // symmetric against its own frame and 24-over/48-under against the hairline, which is the edge
    // this line is actually read from — the argument, with the ink numbers, is on `pad_below`.
    s.h = y + KeyHint::pad_below();
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
    let one = stack(Blocks {
        synopsis: SYN_LEAD,
        ..b
    })
    .h;
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
    Rect::new(
        (SCR_W - PANEL_W) * 0.5,
        ((SCR_H - h) * 0.5).max(EDGE_CLEAR),
        PANEL_W,
        h,
    )
}

// ---- the surface -------------------------------------------------------------------------------

/// The fields [`AboutPanelScreen`] canonicalises, for the recorder's shape pin (§5.4). It is EMPTY
/// and that is the honest answer: this sheet has no cursor, no scroll and no selection, so its
/// whole record in a recording is the container's — which argument is present, in which phase
/// (`Navigation::write`). A shape with a field in it would be claiming state the panel does not
/// have, and the pin would then move for a change nothing could observe.
pub(crate) const SHAPE: &str = "AboutPanelScreen{}";

/// **How far the sheet rises as it appears, in px** — `Popover::RISE`, the one number the whole
/// panel family shares so that two surfaces leaving together read as one movement. The container
/// owns the spring; this is only the distance it drives.
const RISE: f32 = crate::ui::popover::Popover::RISE;

/// The About footer's card, read in full. Presented on the Detail page's own `ModalStack`
/// (`registry::ContentPanel::About`), dismissed by BACK or OK.
pub(crate) struct AboutPanelScreen {
    entry: crate::ui::machine::EntryId,
    /// The frosted ground's render state — a resource, never logical state, which is why it is not
    /// in [`SHAPE`].
    glass: crate::ui::widgets::GlassState,
}

impl AboutPanelScreen {
    pub(crate) fn new(entry: crate::ui::machine::EntryId) -> Self {
        Self {
            entry,
            glass: crate::ui::widgets::GlassState::new(),
        }
    }

    /// The whole panel, at this frame's appear fraction.
    ///
    /// `Painter::root()` rather than a painter handed down the tree, and `alpha`/`translate` rather
    /// than `Popover::content_painter`: the container draws a surface after the page, so the fade
    /// and the slide are this draw's own, and the scrim is already down (see [`Self::scrim`]).
    fn paint(
        &mut self,
        d: &metadata::Detail,
        appear: f32,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        let slide = RISE * (1.0 - appear);
        let p = Painter::root().alpha(appear).translate(0.0, slide);

        // ---- measure ----
        // The tagline first, because the synopsis's line budget is what is LEFT after it.
        let mut b = Blocks {
            synopsis: 0.0,
            tagline: if d.tagline.is_empty() { 0.0 } else { FINE_LEAD },
        };
        let syn = TextView::new(&d.summary, theme::size::BODY, theme::TEXT_READING)
            .leading(SYN_LEAD)
            .max_lines(syn_lines(b));
        b.synopsis = if d.summary.is_empty() {
            0.0
        } else {
            syn.measure_h(CONTENT_W)
        };
        let s = stack(b);
        let r = panel_rect(s.h);

        // ---- ground ----
        crate::ui::widgets::Glass::CACHED.panel(p, r, slide, theme::ALERT_PANEL_RAD);

        // ---- content ----
        let cx = r.x + PAD;
        let run = |text: &str, y: f32, sz, lead: f32, col, bold| {
            let mut v = TextView::new(text, sz, col).leading(lead).max_lines(1);
            if bold {
                v = v.bold();
            }
            v.draw(p, Rect::new(cx, r.y + y, CONTENT_W, 0.0));
        };

        run(
            "ABOUT",
            s.eyebrow,
            theme::size::CAPTION,
            EYEBROW_LEAD,
            theme::TEXT_TERTIARY,
            true,
        );
        if s.synopsis > 0.0 {
            syn.draw(p, Rect::new(cx, r.y + s.synopsis, CONTENT_W, 0.0));
        }
        if s.tagline > 0.0 {
            run(
                &d.tagline,
                s.tagline,
                theme::size::CAPTION,
                FINE_LEAD,
                theme::TEXT_TERTIARY,
                false,
            );
        }
        rule(p, r, s.rule);

        let hint = KeyHint::new(c"Press", c"BACK", c"to return");
        // RIGHT-aligned on the padding edge, as §1B and §1C are and as the design draws all three
        // (§1A's footer row is `justify-content:flex-end`). It was centred for one revision, on the
        // theory that a lone hint with no left-hand partner should not sit at a margin; the owner's
        // answer was to keep it right, so the family has ONE footer alignment and this panel is not the
        // exception to it. Do not re-derive the centred form — `KeyHint`'s own doc offers the
        // arithmetic, and no caller wants it.
        hint.draw(
            p,
            r.x + r.w - PAD - hint.width(measure),
            r.y + s.footer + KeyHint::height() * 0.5,
            measure,
        );
    }
}

impl<H: crate::screens::registry::AppLike> crate::ui::machine::Machine<H> for AboutPanelScreen {
    type Ev = crate::ui::screen::ScreenEvent<H>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        _cx: &crate::ui::machine::Cx<'_, H>,
        fx: &mut crate::ui::machine::Effects<'_, H>,
    ) -> crate::ui::machine::Handled {
        use crate::ui::machine::{Edge, Fx, Handled, InputKind, Key, NavOp};
        use crate::ui::screen::ScreenEvent;
        match ev {
            ScreenEvent::Input(input) => match input.kind {
                // **BACK and OK both close, and OK is the deliberate half.** The design says only
                // BACK, and the panel carries no control for OK to commit — but on a remote OK is
                // the primary button, and a modal that answers it with nothing is the one dead key
                // on the screen. Closing is the superset: nothing else can be reached from here for
                // OK to mean instead.
                InputKind::Key {
                    key: Key::Back | Key::Ok,
                    edge: Edge::Down,
                    ..
                } => {
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    Handled::Yes
                }
                // Every other key is SWALLOWED rather than passed down: the sheet is modal, and a
                // D-pad press that walked the page's own ladder under it is the trap the legacy
                // `is_open()` guard in `detail::key` existed to prevent.
                InputKind::Key { edge: Edge::Down | Edge::Repeat, .. } => Handled::Yes,
                // A click does nothing at all — `Style::Alert`'s own `on_miss`, stated here
                // because the container hands this screen the pointer whatever the miss policy
                // says (`hit_source` being `Engine` changes nothing: the panel's hit map is
                // empty, `draw` records no `Stop`s, so a click resolves to no hit either way).
                // This is the one thing about the panel that CHANGED when it left `ui::popover`:
                // the legacy module closed on a click anywhere ("the panel holds nothing to
                // hit"), which is a `Compact` popover's rule, and this sheet is an Alert — the
                // same call `tracks_panel` made when it converted, so the family answers a stray
                // click one way rather than two.
                InputKind::Click { .. } | InputKind::Pointer { .. } => Handled::Yes,
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

/// **No focusable element at all — the panel holds no control.** That used to be the reason this
/// screen answered `FocusSource::Legacy`/`HitSource::Legacy` (restructure phase 12's D2 converts
/// every remaining Legacy answerer to the uniform `Engine` contract, `tracks_panel`/`alt_sources`/
/// the player among them). It is still the honest description of what this `Focusable` impl
/// declares — zero groups, `group_of`/`place` answering `None` — and an EMPTY declaration is a
/// legitimate one: the engine and the hit map ask this impl exactly what a populated screen's
/// would be asked, get nothing back, and fall through to `Outcome::Nothing` at every step
/// (`enter`, `reconcile`, `move_dir`) rather than doing anything — see this module's own
/// `engine_paths_are_inert_on_a_panel_with_no_focusable_element` test, which proves it against
/// the same `FocusEngine` entry points `ui/dispatch.rs` calls. The panel's own `step` still
/// swallows every key and click itself (BACK/OK dismiss, everything else is eaten), unchanged by
/// which source the container reads.
impl<H: crate::screens::registry::AppLike> crate::ui::screen::Focusable<H> for AboutPanelScreen {
    fn groups(&self, _cx: &crate::ui::machine::Cx<'_, H>, _out: &mut Vec<crate::ui::screen::GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &crate::ui::machine::Cx<'_, H>) -> Option<crate::ui::machine::GroupId> {
        None
    }
    fn neighbour(
        &self,
        _key: crate::ui::machine::FocusKey<u32>,
        _dir: crate::ui::screen::Dir,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::screen::Step<u32> {
        crate::ui::screen::Step::Edge
    }
    fn place(
        &self,
        _key: &u32,
        _cx: &crate::ui::machine::Cx<'_, H>,
        _at: crate::ui::screen::At,
    ) -> Option<crate::ui::screen::Placed> {
        None
    }
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        want
    }
    fn seat(
        &self,
        _g: crate::ui::machine::GroupId,
        _from: crate::ui::screen::Placed,
        _cx: &crate::ui::machine::Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::machine::FocusKey {
            entry: self.entry,
            elem: 0,
        }
    }
}

impl crate::ui::machine::LogicalState for AboutPanelScreen {
    fn write(&self, _c: &mut crate::ui::machine::Canon) {}
    fn probe(&self, out: &mut String) {
        out.push_str("about");
    }
}

impl<H: crate::screens::registry::AppLike> crate::ui::screen::Screen<H> for AboutPanelScreen {
    fn name(&self) -> &'static str {
        "about"
    }
    fn state(&self) -> &dyn crate::ui::machine::LogicalState {
        self
    }
    fn crumb(&self, _cx: &crate::ui::machine::Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut crate::ui::frame::Budget, _cx: &crate::ui::machine::Cx<'_, H>) {
        crate::ui::widgets::Glass::CACHED.prepare(&mut self.glass, false);
    }
    /// The modal dim, asked for rather than drawn.
    ///
    /// **Nothing is LIFTED back out of it.** `Scrim::lifting` exists for a panel that is ABOUT an
    /// element still on screen; this one is about the card it was opened from and says everything
    /// that card says, at length — lifting it would put a truncated copy of this panel's own first
    /// three runs alongside it. The mock lifts nothing either.
    ///
    /// **The ordering this replaces was load-bearing and is now the container's** (§16.3): a
    /// `Glass::CACHED` ground samples the framebuffer as it stands, so the dim has to be down
    /// before the sheet's backdrop is taken, or the frosted ground reads brighter than the dimmed
    /// screen around it. `ModalStack::draw_scrims` draws it at the end of the PAGE pass — strictly
    /// earlier than the surface pass this `draw` runs in — and multiplies by the appear spring and
    /// by `nav::page_alpha`, which is `Popover::scrim`'s own arithmetic and one factor more than
    /// the page-drawn version could reach.
    fn scrim(&self) -> crate::ui::screen::Scrim {
        crate::ui::screen::Scrim::dim(SCRIM_A)
    }
    fn draw(&mut self, f: &mut crate::ui::screen::DrawFrame<'_, '_, H>) {
        // **The item is the one that LANDED, not the page's.** The panel is presented over exactly
        // one page and dismissed with it, so in practice they are the same item; reading
        // `metadata::current()` keeps this module's dependency at the store it always had rather
        // than adding a copy of the page's identity to an argument that carries nothing.
        let Some(d) = metadata::current() else { return };
        // The container owns the appear spring; `DrawFrame::page_alpha` IS `Surface::motion.appear`
        // for a surface, which is what this panel's own `Popover` used to hold.
        let appear = f.page_alpha;
        let measure = f.measure;
        // Named for `/tmp/plxnative-cpuprof` beside the page's own phases, so a slow frame while
        // this sheet is up can be read as the PANEL or as the host under it rather than as one
        // `main.ui` total.
        crate::ui::profile::phase("dt.about", || self.paint(&d, appear, measure));
    }
    fn render(&self) -> crate::ui::screen::RenderStrategy {
        crate::ui::screen::RenderStrategy::Page
    }
    fn focus_source(&self) -> crate::ui::screen::FocusSource {
        crate::ui::screen::FocusSource::Engine
    }
    fn hit_source(&self) -> crate::ui::screen::HitSource {
        crate::ui::screen::HitSource::Engine
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// One full-width hairline across the content column.
fn rule(p: Painter, r: Rect, y: f32) {
    widgets::hairline(p, r.x + PAD, r.y + y, CONTENT_W);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder in the mock's own order, with every block present: each block sits below the one
    /// above it by exactly its gap, and the panel's height closes with the bottom padding.
    #[test]
    fn the_stack_lays_the_mocks_ladder_out_in_order() {
        let b = Blocks {
            synopsis: 3.0 * SYN_LEAD,
            tagline: FINE_LEAD,
        };
        let s = stack(b);
        assert_eq!(s.eyebrow, PAD, "the first block starts at the padding");
        assert_eq!(s.synopsis, s.eyebrow + EYEBROW_LEAD + G_SYNOPSIS);
        assert_eq!(s.tagline, s.synopsis + b.synopsis + G_TAGLINE);
        assert_eq!(s.rule, s.tagline + b.tagline + G_RULE);
        assert_eq!(s.footer, s.rule + widgets::HAIRLINE_H + G_FOOTER);
        assert_eq!(
            s.h,
            s.footer + KeyHint::height() + KeyHint::pad_below(),
            "…and the footer closes on its OWN pad, the one that centres it under the rule"
        );
        assert_eq!(
            s.h - (s.footer + KeyHint::height()),
            s.footer - (s.rule + widgets::HAIRLINE_H),
            "which is the same air the rule opens above it — the hint is centred in that band"
        );
        // the whole point of the mock's shape: it fits, with room to spare
        assert!(
            s.h < max_panel_h(),
            "the reference item's panel is {} against a {} ceiling",
            s.h,
            max_panel_h()
        );
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
        let b = Blocks {
            synopsis: 3.0 * SYN_LEAD,
            tagline: FINE_LEAD,
        };
        let s = stack(b);
        assert_eq!(
            s.synopsis - s.eyebrow,
            EYEBROW_LEAD + G_SYNOPSIS,
            "the eyebrow labels the PROSE — nothing is placed between them"
        );
        assert_eq!(
            s.h,
            PAD + EYEBROW_LEAD
                + G_SYNOPSIS
                + b.synopsis
                + G_TAGLINE
                + b.tagline
                + G_RULE
                + widgets::HAIRLINE_H
                + G_FOOTER
                + KeyHint::height()
                + KeyHint::pad_below(),
            "the panel is exactly eyebrow · prose · tagline · rule · footer"
        );
    }

    /// An ABSENT block costs neither its height nor the gap above it — a film with no tagline must
    /// not sit over a reserved hole, and the block after it keeps its own gap. This is the whole
    /// reason each gap belongs to the block below it.
    #[test]
    fn an_absent_block_takes_its_gap_with_it() {
        let full = Blocks {
            synopsis: SYN_LEAD,
            tagline: FINE_LEAD,
        };

        let no_tag = stack(Blocks {
            tagline: 0.0,
            ..full
        });
        assert_eq!(no_tag.tagline, 0.0, "an absent block is not placed");
        assert_eq!(
            stack(full).h - no_tag.h,
            FINE_LEAD + G_TAGLINE,
            "dropping the tagline drops its own height AND its gap"
        );
        assert_eq!(
            no_tag.rule,
            no_tag.synopsis + SYN_LEAD + G_RULE,
            "the rule keeps its own gap"
        );

        // …and the degenerate item — no summary AND no tagline — still closes with a rule over its
        // footer rather than a keycap hanging under the eyebrow.
        let bare = stack(Blocks::default());
        assert_eq!((bare.synopsis, bare.tagline), (0.0, 0.0));
        assert_eq!(
            bare.rule,
            PAD + EYEBROW_LEAD + G_RULE,
            "the footer's rule is unconditional"
        );
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
        let base = Blocks {
            synopsis: 0.0,
            tagline: FINE_LEAD,
        };
        let n = syn_lines(base);
        assert!(
            n >= 3,
            "the reference item's ladder leaves room for a real paragraph, got {n}"
        );
        // the budget is exactly what fits
        assert!(
            stack(Blocks {
                synopsis: n as f32 * SYN_LEAD,
                ..base
            })
            .h <= max_panel_h()
        );
        assert!(
            stack(Blocks {
                synopsis: (n + 1) as f32 * SYN_LEAD,
                ..base
            })
            .h > max_panel_h()
        );

        // a taller neighbouring block buys the synopsis fewer lines, monotonically
        let taller = syn_lines(Blocks {
            tagline: FINE_LEAD + 4.0 * SYN_LEAD,
            ..base
        });
        assert!(
            taller < n,
            "growing another block must cost the synopsis lines: {taller} vs {n}"
        );

        // and the floor holds even when the arithmetic says there is no room at all
        assert_eq!(
            syn_lines(Blocks {
                tagline: 10_000.0,
                ..base
            }),
            1
        );
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
        let b = Blocks {
            synopsis: 3.0 * SYN_LEAD,
            tagline: FINE_LEAD,
        };
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
        assert!(
            (r.x - 400.0).abs() < 0.001,
            "the mock's own left:400 falls out of centring, got {}",
            r.x
        );
        assert!(
            (r.y - 280.0).abs() < 0.001,
            "…and a 520 sheet's own top, got {}",
            r.y
        );

        // a tall panel is clamped rather than allowed to run off the frame
        let tall = panel_rect(5_000.0);
        assert_eq!(tall.h, max_panel_h());
        assert!(tall.y >= EDGE_CLEAR - 0.001, "top edge clear: {}", tall.y);
        assert!(
            tall.y + tall.h <= SCR_H - EDGE_CLEAR + 0.001,
            "bottom edge clear"
        );
        assert!(
            tall.x >= EDGE_CLEAR && tall.x + tall.w <= SCR_W - EDGE_CLEAR,
            "side edge clear"
        );
    }

    // ---- the surface, driven with no SDL (§15.1 `a_new_screen_is_unit_tested_with_no_sdl`) -----
    //
    // A host of its own, three lines of it, rather than the application's: this panel is generic
    // over `AppLike` exactly so it can be stepped without one, and borrowing a sibling's test host
    // is the sibling dependency the layer gate exists to refuse.

    use crate::screens::registry::{AppFx, AppMsg};
    use crate::ui::machine::{
        Canon, Chrome, Cx, Edge, Effects, EntryId, FocusRead, Fx, Handled, Host, InputEvent,
        InputKind, InputOwner, Key, LogicalState, Machine, NavOp, PressRead, ScreenId,
        Source, Stamped, Tick,
    };
    use crate::ui::present::Present;
    use crate::ui::screen::{ScreenArg, ScreenEvent};

    #[derive(Clone, PartialEq, Eq)]
    struct TestArg;
    impl LogicalState for TestArg {
        fn write(&self, c: &mut Canon) {
            c.u32(0);
        }
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for TestArg {
        fn chrome(&self) -> Chrome {
            Chrome::None
        }
        fn id(&self) -> ScreenId {
            ScreenId(701)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, other: &Self) -> bool {
            self == other
        }
    }

    #[derive(Clone, Default, Debug)]
    struct TestInit;
    impl LogicalState for TestInit {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }

    struct TestHost;
    impl Host for TestHost {
        type Arg = TestArg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = TestInit;
        type Memory = TestInit;
    }

    const ENTRY: EntryId = EntryId(7);

    fn cx(measure: &crate::ui::fixture::FixtureMeasure) -> Cx<'_, TestHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// What one input does to a fresh panel: the effects it emitted, and whether it was consumed.
    fn press(kind: InputKind<u32>) -> (Vec<Stamped<TestHost>>, Handled) {
        let measure = crate::ui::fixture::FixtureMeasure;
        let cx = cx(&measure);
        let (mut out, mut present) = (Vec::new(), Present::new());
        let mut fx = Effects::new(&mut out, crate::ui::machine::MachineId::Nav, &mut present);
        let mut panel = AboutPanelScreen::new(ENTRY);
        let handled = panel.step(
            &ScreenEvent::Input(InputEvent {
                kind,
                at: Tick::default(),
                source: Source::Sdl,
            }),
            &cx,
            &mut fx,
        );
        (out, handled)
    }

    fn key(k: Key) -> InputKind<u32> {
        InputKind::Key {
            key: k,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        }
    }

    fn dismissed(out: &[Stamped<TestHost>]) -> bool {
        out.iter()
            .any(|s| matches!(&s.fx, Fx::Nav(NavOp::Dismiss(id)) if *id == ENTRY))
    }

    /// **BACK and OK both leave, and every other key is EATEN.**
    ///
    /// The second half is the one that had a bug's worth of wiring behind it: while this was a
    /// `Popover`, the page's own `panel_input` had to test `about_panel::is_open()` before its key
    /// ladder ran, or a D-pad press walked the detail page's sections under the open sheet. That
    /// guard is deleted with the popover — the container gives input to the topmost surface and the
    /// page is never asked — so what stops the ladder now is this screen answering `Handled::Yes`,
    /// and nothing else does.
    #[test]
    fn back_and_ok_dismiss_the_sheet_and_every_other_key_is_swallowed() {
        for k in [Key::Back, Key::Ok] {
            let (out, handled) = press(key(k));
            assert_eq!(handled, Handled::Yes, "{k:?} is the panel's own");
            assert!(dismissed(&out), "{k:?} dismisses this entry");
        }
        for k in [Key::Up, Key::Down, Key::Left, Key::Right] {
            let (out, handled) = press(key(k));
            assert_eq!(handled, Handled::Yes, "{k:?} must not reach the page under the sheet");
            assert!(!dismissed(&out), "{k:?} is not an exit");
            assert!(out.is_empty(), "{k:?} does nothing at all");
        }
    }

    /// A click does NOTHING — not even close, which is the one thing about this panel that changed
    /// when it became a surface. `Style::Alert` answers a miss with nothing (§6.2), and the
    /// container hands this screen the pointer whatever the miss policy says, so the refusal has
    /// to be here. `tracks_panel` made the same call when it converted; the legacy module closed
    /// on a click anywhere, which is a `Compact` popover's rule.
    #[test]
    fn a_click_neither_dismisses_the_sheet_nor_reaches_the_page() {
        let (out, handled) = press(InputKind::Click { x: 10.0, y: 10.0, hit: None });
        assert_eq!(handled, Handled::Yes);
        assert!(out.is_empty(), "an Alert answers a click with nothing");
    }

    /// **The `FocusSource::Engine`/`HitSource::Engine` conversion (restructure phase 12, D2)
    /// changes nothing observable — this is the proof.** Before the conversion, `ui/dispatch.rs`
    /// never asked the engine about this screen at all (`engine_page()`/`hit_page()` read
    /// `Legacy` and short-circuited). After it, the dispatcher calls exactly the entry points
    /// exercised here — `FocusEngine::enter` on mount (`Enter::Fresh`, `restored: None`, the
    /// container's actual call shape in `ui/dispatch.rs`'s `after_step`) and `FocusEngine::reconcile`
    /// before every draw — and both must still do nothing, because the `Focusable` impl above
    /// declares zero groups. Graded directly against `FocusEngine`, the same type the dispatcher
    /// holds, rather than against the dispatcher itself (a sibling dependency the layer gate
    /// forbids this module from taking).
    #[test]
    fn engine_paths_are_inert_on_a_panel_with_no_focusable_element() {
        use crate::ui::focus::{FocusEngine, Outcome};
        use crate::ui::machine::GroupId;
        use crate::ui::screen::FocusTarget;

        let measure = crate::ui::fixture::FixtureMeasure;
        let cx = cx(&measure);
        let panel = AboutPanelScreen::new(ENTRY);
        let owner = InputOwner::Entry(ENTRY);
        let mut engine: FocusEngine<u32> = FocusEngine::new();

        // The dispatcher's mount-time call: a fresh Enter, target the container group, nothing
        // restored (`ScreenEvent::Enter(Enter::Fresh { .. })` never carries a restore).
        let outcome = engine.enter(owner, &panel, FocusTarget::ContainerGroup(GroupId(0)), None, &cx);
        assert!(
            matches!(outcome, Outcome::Nothing),
            "no groups means nothing to enter, exactly as under Legacy nothing was ever asked"
        );

        // With nothing entered, the dispatcher's own pre-draw reconcile step also has nothing to
        // do — `FocusEngine::current` answers `None` for this owner.
        let outcome = engine.reconcile(owner, &panel, &cx);
        assert!(matches!(outcome, Outcome::Nothing));

        use crate::ui::screen::Screen;
        assert_eq!(
            (
                Screen::<TestHost>::focus_source(&panel),
                Screen::<TestHost>::hit_source(&panel),
            ),
            (crate::ui::screen::FocusSource::Engine, crate::ui::screen::HitSource::Engine),
            "the conversion this test guards"
        );
    }

}
