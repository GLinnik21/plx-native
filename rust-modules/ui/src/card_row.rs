//! `CardRow` — the shared animated poster-shelf component.
//!
//! The spring-and-tile primitive under [`cards::Shelf`](crate::cards::Shelf), `pub(crate)` L0: a
//! focus-scale [`Spring`] per cell and a horizontal scroll spring, with the tile rendering (poster,
//! big glow focus ring, resume bar, centered title). [`RowStyle::HOME`] is the single source of
//! the shelf's motion and geometry.
//!
//! The x/`base_y` positioning loop, the cull and the stops are the Shelf's; `CardRow` owns spring state and
//! the leaf tile draws and never draws a whole row itself. A screen never names it: it implements
//! `CardSource` and hands the content to `cards::Shelf` / `cards::Grid`. It is decoupled from any
//! focus globals / catalog: focus is a plain `Option<usize>`, the item art is a [`Art`] the caller
//! supplies.
use crate::consts::*;
use crate::hstack::{Flow, Item};
use crate::vstack::{Anchor, VStack};
use crate::theme;
use crate::tile::{TileFacts, TileKind};
use crate::widgets::{card, pop_factor, Art};
use crate::{Painter, Rect, Spring};
use std::os::raw::c_char;

pub const MAX_ROW_ITEMS: usize = 24; // == home MAX_ITEMS

/// A card row's motion + geometry. [`RowStyle::HOME`] is the single value both the home grid and the
/// detail Related row pass, so the two rows are indistinguishable in look and animation.
#[derive(Clone, Copy)]
pub struct RowStyle {
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    pub margin_x: f32,
    pub radius: f32,
    pub focus_scale: f32,
    pub k_scale: f32,
    pub k_scroll: f32,
    /// Circular tiles (cast headshots, who's-watching avatars) vs rounded-rect posters. A circle is
    /// just a tile drawn at `radius = width/2`; the shared springs/scroll/ring are identical.
    pub circular: bool,
    /// **How much of the panel's RIGHT edge is spoken for by something that is not this row** —
    /// zero everywhere but the Library grid, whose A–Z rail stands in that band.
    ///
    /// It exists because a focused tile's label is deliberately WIDER than its tile
    /// ([`under_budget`] reserves two tiles and two gaps so a long title has somewhere to go) and
    /// was clamped against the PANEL. On every screen but one those two edges are the same line, so
    /// the shared rule quietly assumed it — and on the Library the last column's caption ran
    /// straight through the rail's letters and out to the panel edge. Photographed on the
    /// television 2026-09-05.
    ///
    /// A reserve from the physical edge, intersected with the shared safe area;
    /// [`RowStyle::with_right_reserve`] is how a screen declares one.
    pub right_reserve: f32,
}
impl RowStyle {
    /// The furthest a row of `n` cells scrolls: the content past the viewport, 0 for a row that
    /// fits whole.
    pub fn max_scroll(&self, n: usize) -> f32 {
        let viewport = SCR_W - 2.0 * self.margin_x;
        (n as f32 * (self.w + self.gap) - self.gap - viewport).max(0.0)
    }

    /// This style with `r` px of the panel's right edge reserved — see [`RowStyle::right_reserve`].
    /// A `const fn` so a screen can name the result once, beside the constant it derives it from,
    /// rather than rebuilding it per frame.
    pub const fn with_right_reserve(self, r: f32) -> RowStyle {
        RowStyle {
            right_reserve: r,
            ..self
        }
    }

    /// The home shelf's portrait-poster row: 1.09 focus pop (matches `Home Screen.dc.html`), the big
    /// glow ring, animated scroll. The `ui::press` click dips a focused card from here back toward
    /// `scale(1.0)`, so the pop needs to be large enough for that press-in to read.
    pub const HOME: RowStyle = RowStyle {
        w: CARD_W,
        h: CARD_H,
        gap: GAP,
        margin_x: MARGIN_X,
        radius: theme::CARD_RING_RAD,
        focus_scale: 1.09,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: false,
        right_reserve: 0.0,
    };
    /// **A shelf of EPISODES: landscape stills.** Same motion as HOME — the spring magnification,
    /// the scroll, the glow ring and the heading lift are all one vocabulary — and a different
    /// aspect, because an episode is not a poster.
    ///
    /// 420x236 is the detail page's own episode strip (`ui::detail`'s `EP_W`/`EP_H`), taken rather
    /// than re-chosen: the two are the same object on two screens, and a second 16:9 size would be
    /// the kind of near-miss nobody notices until they are side by side. The gap is `EP_GAP`'s 28
    /// for the same reason.
    ///
    /// The pop is HOME's 1.09 rather than a landscape-specific number: `slot - w * focus_scale` is
    /// 448 - 457.8, which is NEGATIVE — a popped still overlaps its neighbour's slot by ~10px, and
    /// that is correct here and not a bug. A card row draws the focused tile LAST, so it magnifies
    /// OVER its neighbours; the headroom rule quoted on `CAST` is a CIRCLE's rule, where the tile
    /// is drawn in place and an overlap would read as two objects colliding.
    pub const EPISODE: RowStyle = RowStyle {
        w: 420.0,
        h: 236.0,
        gap: 28.0,
        margin_x: MARGIN_X,
        radius: theme::CARD_RING_RAD,
        focus_scale: 1.09,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: false,
        right_reserve: 0.0,
    };
    /// Detail "Cast & Crew": circular headshots, the tight strip ring. Same motion as HOME (spring
    /// magnification + scroll), so cast animates like the poster shelves.
    ///
    /// **1.13, not the original 1.06** — reported as too weak to read as a focus change at all
    /// (owner punch list). At `w`/`gap` = 190/40 (slot 230) a popped circle's diameter is
    /// `190 × 1.13` = 214.7px, still 15.3px short of the neighbouring slot's centre-to-centre pitch
    /// with air on both sides — raise this further only after re-checking that headroom (`slot −
    /// w × focus_scale` must stay positive). `detail::cast_label` derives its focused-label drop
    /// from this same field, so the two can never drift apart.
    pub const CAST: RowStyle = RowStyle {
        w: 190.0, // = detail CAST_D
        h: 190.0,
        gap: 40.0, // w+gap = 230 = detail CAST_SLOT (per-member pitch)
        margin_x: MARGIN_X,
        radius: 95.0, // = w/2 (circle); draw_* recomputes per-rect anyway when `circular`
        focus_scale: 1.13,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
        right_reserve: 0.0,
    };
    /// "Who's watching" profile pictures: big circular avatars with a clear pop. Centered by the
    /// caller (a short roster), so the scroll spring stays put unless the row overflows.
    ///
    /// The pop is much larger than the shelves' 1.09 **on purpose**: a poster shelf marks its focus
    /// against close, hard-edged neighbours, but a lone row of widely-spaced circles has no such
    /// reference and the same 9% read as "nothing is selected". 1.18 still leaves 28px between
    /// adjacent tiles at full pop (slot 292 − 264), and `profiles::NAME_DY` derives the name band's
    /// drop from this number so the label can never collide with the popped circle.
    pub const PROFILES: RowStyle = RowStyle {
        w: 220.0,
        h: 220.0,
        gap: 72.0,
        margin_x: MARGIN_X,
        radius: 110.0,
        focus_scale: 1.18,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
        right_reserve: 0.0,
    };
    /// Corner radius for a tile of size `rect` at radius-scale `s`: half-width for a circle, else the
    /// style's fixed radius scaled with the focus pop.
    #[inline]
    pub fn tile_radius(&self, rect: Rect, s: f32) -> f32 {
        if self.circular {
            rect.w * 0.5
        } else {
            self.radius * s
        }
    }
}

/// Per-row animation state: a focus-scale spring per cell + the horizontal scroll spring. `Copy` so
/// a grid can build `vec![CardRow::new(); n]` and carry a surviving row over by value.
#[derive(Clone, Copy)]
pub struct CardRow {
    scale: [Spring; MAX_ROW_ITEMS],
    /// The ONE spring every cell PAST the array shares — see [`CardRow::scale`]. A cast row is
    /// routinely 60 names long, so "the tiles past the 24th simply don't animate" is not a corner
    /// case; it is most of the Cast & Crew shelf, and the crew now lives at its far end.
    overflow: Spring,
    /// Which cell holds focus (`-1` none), so an overflow cell can claim `overflow` while its
    /// neighbours — which read the same spring — stay at rest.
    focus: i32,
    scroll_x: Spring,
    lift: Spring,
    /// 0 collapsed / 1 expanded — how much of the focused label block this shelf is currently
    /// reserving. See [`label_band`]; the screens turn it into a row pitch.
    band: Spring,
    pub base_y: f32,
    /// What the focused caption band shows, when the shelf follows the Under posters setting; `None`
    /// keeps the legacy two-line band ([`LEGACY_BAND_H`]), as [`Grid`](crate::cards::Grid) does.
    /// A pref input, not motion: it is not in the canon.
    caption_policy: Option<crate::caption_policy::CaptionPolicy>,
    /// **The row is where the pointer's sideways glide put it** ([`glide`](Self::glide)): `update`
    /// stops steering the scroll to the focused cell until [`unfree`](Self::unfree) (a key's move
    /// along the row, a click on one of its cards). A hand-off from the pointer, like the pages'
    /// own `free`: what it decides, `scroll_x`, is in the canon.
    free: bool,
}
impl CardRow {
    /// Follow (`Some`) or stop following (`None`, the legacy band) the Under posters policy.
    pub fn set_caption_policy(&mut self, policy: Option<crate::caption_policy::CaptionPolicy>) {
        self.caption_policy = policy;
    }
    pub fn caption_policy(&self) -> Option<crate::caption_policy::CaptionPolicy> {
        self.caption_policy
    }

    /// Motion read by placement/reveal is behavioral state for an owned, replayable screen.
    /// Destructure exhaustively so adding a field requires revisiting the census.
    pub fn write_motion(&self, c: &mut plx_machine::machine::Canon) {
        let Self { scale, overflow, focus, scroll_x, lift, band, base_y, caption_policy: _, free: _ } = self;
        c.seq(scale.len());
        for spring in scale { c.f32(spring.pos).f32(spring.vel); }
        for spring in [overflow, scroll_x, lift, band] { c.f32(spring.pos).f32(spring.vel); }
        c.u32(*focus as u32).f32(*base_y);
    }

    pub const fn new() -> Self {
        CardRow {
            scale: [Spring::at(1.0); MAX_ROW_ITEMS],
            overflow: Spring::at(1.0),
            focus: -1,
            scroll_x: Spring::at(0.0),
            lift: Spring::at(0.0),
            band: Spring::at(0.0),
            base_y: 0.0,
            free: false,
            caption_policy: None,
        }
    }
    /// Whether every spring sits bit-exactly at its target with no velocity and nothing holds
    /// focus. An unfocused [`update`](Self::update) of such a row writes back the same bits and
    /// reports nothing to the idle gate, so a caller may skip the call (Home does, per row).
    pub fn at_exact_rest(&self) -> bool {
        let at = |sp: &Spring, rest: f32| sp.pos == rest && sp.vel == 0.0;
        self.focus == -1
            && self.scale.iter().all(|sp| at(sp, 1.0))
            && at(&self.overflow, 1.0)
            && at(&self.lift, 0.0)
            && at(&self.band, 0.0)
    }
    /// Snap an unfocused row whose every spring is already [`settled`](plx_machine::idle::settled)
    /// onto its rest target, so [`at_exact_rest`](Self::at_exact_rest) holds and a caller may stop
    /// stepping it. `step` never lands a released spring exactly: `band` decays into denormals and a
    /// cell's `scale` stalls one ulp above 1.0 ([`update`](Self::update) lands those cells itself,
    /// [`Spring::land`], so a row that keeps focus elsewhere has none), so without this a row that
    /// was ever focused is stepped every frame forever. Inert by construction: it fires only inside the
    /// threshold the idle gate already treats as "motion finished". `scroll_x` is a retained
    /// viewport, not a rest value, and is left alone. Returns whether the row is now at exact rest.
    pub fn park(&mut self) -> bool {
        if self.focus != -1 {
            return false;
        }
        let rest = |sp: &Spring, to: f32| plx_machine::idle::settled(sp.pos, to, sp.vel);
        if !(self.scale.iter().all(|sp| rest(sp, 1.0))
            && rest(&self.overflow, 1.0)
            && rest(&self.lift, 0.0)
            && rest(&self.band, 0.0))
        {
            return false;
        }
        for sp in self.scale.iter_mut().chain([&mut self.overflow]) {
            sp.jump(1.0);
        }
        self.lift.jump(0.0);
        self.band.jump(0.0);
        true
    }
    /// Let go of focus and put every spring that is a rest value (each cell's scale, the heading's
    /// lift, the label band) on its rest target AT ONCE, whatever it was doing: a row that is
    /// [`at_exact_rest`](Self::at_exact_rest) afterwards. [`park`](Self::park) lands only a row whose
    /// springs have already settled; this is the cut for a row nobody can see move (the page is a held
    /// transition image). `scroll_x` is a retained viewport, not a rest value, and is left alone.
    pub fn snap_to_rest(&mut self) {
        self.focus = -1;
        for sp in self.scale.iter_mut().chain([&mut self.overflow]) {
            sp.jump(1.0);
        }
        self.lift.jump(0.0);
        self.band.jump(0.0);
    }
    /// Step every cell's scale spring (all `MAX_ROW_ITEMS` every call — invariant #10; Home skips the
    /// call for a row at [`at_exact_rest`](Self::at_exact_rest), a fixed point of this method) toward
    /// `focus_scale` for the focused cell else 1.0; then, ONLY when this row is focused, glide the
    /// scroll spring *minimally*: scroll just far enough that the focused cell (plus one gap of
    /// breathing room) sits fully inside the viewport, and not at all when it already does — no
    /// slot pinning, so entering a row never shuffles tiles that were already visible.
    /// `focused == None` freezes the scroll (a non-focused row holds its position — exactly the
    /// home shelf's behavior).
    pub fn update(&mut self, n: usize, focused: Option<usize>, sty: &RowStyle, dt: f32) {
        self.focus = focused.map(|f| f as i32).unwrap_or(-1);
        for (i, sp) in self.scale.iter_mut().enumerate() {
            let target = if focused == Some(i) {
                sty.focus_scale
            } else {
                1.0
            };
            sp.step(target, sty.k_scale, dt);
            // A cell the focus has left comes to EXACT rest. `step` stalls it one ulp above 1.0 and
            // `park` never runs while the row holds focus on another cell, so a row walked by a held
            // key left every visited card at an `f` of 1e-6: the focus program, never the resting split.
            if focused != Some(i) {
                sp.land(1.0, dt);
            }
        }
        // the shared overflow spring pops only while focus is actually out past the array
        let past = focused.is_some_and(|f| f >= MAX_ROW_ITEMS);
        let ot = if past { sty.focus_scale } else { 1.0 };
        self.overflow.step(ot, sty.k_scale, dt);
        if !past {
            self.overflow.land(1.0, dt);
        }
        if let Some(fc) = focused {
            if n > 0 && !self.free {
                self.scroll_x
                    .step(self.scroll_target(fc, n, sty), sty.k_scroll, dt);
            }
        }
        self.lift.step(
            heading_clearance(focused, sty, self.scroll_x.pos),
            sty.k_scale,
            dt,
        );
        // The label band opens only for the shelf that is actually showing a label block, and it
        // travels at the SCROLL rate rather than the pop's: it moves the column, so it has to
        // arrive with the scroll that is revealing the row, not with the tile that is growing.
        self.band
            .step(focused.is_some() as i32 as f32, sty.k_scroll, dt);
    }
    /// Adopt cell `i` as focused AT FULL pop, with no motion: the focus arrived without a deliberate
    /// move (a restore, a reconcile, a landing), so the tile must be drawn as it was left rather than
    /// growing from rest. The cell past the spring array shares `overflow`, as in [`CardRow::scale`].
    pub fn adopt(&mut self, i: usize, sty: &RowStyle) {
        if i < MAX_ROW_ITEMS { self.scale[i].jump(sty.focus_scale) } else { self.overflow.jump(sty.focus_scale) }
    }
    /// Put the springs that settle around a focused card where focus on cell `i` of `n` rests them,
    /// with no motion: the scroll that brings the card into view (a glide the page froze when it was
    /// covered ends here), the label band wide open and the heading raised by its clearance at that
    /// scroll. The twin of [`adopt`](Self::adopt) for a seat that comes back to a row the page had
    /// shut (a shelf put to rest under a transition image), where adopting the pop alone would leave
    /// the glide, the band and the heading to play as layout motion under the shelves around it.
    pub fn settle_seat(&mut self, i: usize, n: usize, sty: &RowStyle) {
        if n > 0 {
            self.scroll_x.jump(self.scroll_target(i, n, sty));
        }
        self.band.jump(1.0);
        self.lift.jump(heading_clearance(Some(i), sty, self.scroll_x.pos));
    }
    /// Cell `i`'s spring, the shared overflow one for a cell past the array.
    fn cell_mut(&mut self, i: usize) -> &mut Spring {
        if i < MAX_ROW_ITEMS { &mut self.scale[i] } else { &mut self.overflow }
    }
    /// Snap cell `i` to rest (1.0, no velocity): the tile it held lost focus without a deliberate
    /// move, so nothing should let it go.
    pub fn rest(&mut self, i: usize) {
        self.cell_mut(i).jump(1.0);
    }
    /// The focused element moved from cell `from` to cell `to` because the CONTENT changed (a
    /// landing), not because focus did: its pop spring, velocity and all, goes with it, the cell it
    /// leaves snaps to rest, and the scroll shifts by `dx` so the tile stays where it was on screen.
    pub fn relocate(&mut self, from: usize, to: usize, dx: f32) {
        // two cells past the array share one spring: it already is the moved element's
        if from != to && (from < MAX_ROW_ITEMS || to < MAX_ROW_ITEMS) {
            let state = *self.cell_mut(from);
            *self.cell_mut(to) = state;
            self.rest(from);
        }
        self.scroll_x.pos += dx;
    }
    /// The cell recorded as focused is now `i`: the element it held was moved there by a landing
    /// the caller has already carried, in a tick that also moves focus on from it.
    pub(crate) fn refocus(&mut self, i: usize) {
        self.focus = i as i32;
    }
    /// Cell `i`'s own pop spring; `None` for a cell past the array (those share one spring).
    pub(crate) fn cell_spring(&self, i: usize) -> Option<Spring> {
        self.scale.get(i).copied()
    }
    /// Give cell `i` the pop spring `sp` (state and velocity); a cell past the array is ignored.
    pub(crate) fn put_cell_spring(&mut self, i: usize, sp: Spring) {
        if let Some(cell) = self.scale.get_mut(i) {
            *cell = sp;
        }
    }
    /// The scroll half of a [`relocate`](Self::relocate): shift by `dx` so a tile whose pop was
    /// carried some other way stays where it was on screen.
    pub(crate) fn shift_scroll(&mut self, dx: f32) {
        self.scroll_x.pos += dx;
    }
    /// **How far the row still has to scroll for `focused` to be where it will rest** — live
    /// scroll minus [`scroll_into_view`]'s target, so a tile's settled screen x is its live x plus
    /// this. `0` once the glide has landed, and `0` for a focus that needs no scroll at all.
    /// Derived from the spring and the focus, never stored, so it is not part of the row's
    /// replayable motion. A focused label hands it to [`place_label`] through
    /// [`TileLabel::settling`], so its alignment is judged where the tile is GOING.
    pub fn settle_lag(&self, n: usize, focused: usize, sty: &RowStyle) -> f32 {
        if n == 0 {
            return 0.0;
        }
        self.scroll_x.pos - self.scroll_target(focused, n, sty)
    }
    /// The scroll offset [`scroll_into_view`] wants for `focused` — what `update` steers the
    /// spring to and `settle_lag` measures the live offset against.
    fn scroll_target(&self, focused: usize, n: usize, sty: &RowStyle) -> f32 {
        let vw = SCR_W - 2.0 * sty.margin_x;
        scroll_into_view(self.scroll_x.pos, focused, n, sty.w, sty.gap, vw)
    }
    /// Cell `i`'s live focus-pop scale. Cells inside the spring array own a spring each; every cell
    /// PAST it shares `overflow`, and only the focused one reads it — otherwise the whole tail of a
    /// long row would pop together. The cost of sharing is that stepping between two overflow cells
    /// swaps the pop instantly instead of crossfading; the alternative (this used to clamp the index,
    /// so an overflow cell read cell 23's spring) was a focused tile with NO pop, NO lifted shadow
    /// and NO ring — the entire focus treatment is derived from this number.
    #[inline]
    pub fn scale(&self, i: usize) -> f32 {
        if i < MAX_ROW_ITEMS {
            self.scale[i].pos
        } else if self.focus == i as i32 {
            self.overflow.pos
        } else {
            1.0
        }
    }
    #[inline]
    pub fn scroll_x(&self) -> f32 {
        self.scroll_x.pos
    }
    /// Read-only witness for the finite hero/grid device scene. A retained offset
    /// must have a stationary shelf spring while the dive runs.
    #[cfg(feature = "devtriggers")]
    pub fn scroll_velocity(&self) -> f32 { self.scroll_x.vel }

    /// **The pointer's sideways glide** (`InputKind::Pan`): move the row by `dx` px now, held to
    /// `[0, max]` for `n` cells, and leave it there: the scroll stops following the focused cell
    /// until [`unfree`](Self::unfree). Moved directly, frame by frame, so a steady glide is a
    /// steady motion (a spring stepped toward each frame's target would trail it).
    pub fn glide(&mut self, dx: f32, n: usize, sty: &RowStyle) {
        let max = sty.max_scroll(n);
        self.scroll_x.jump((self.scroll_x.pos + dx).clamp(0.0, max));
        self.free = true;
    }
    /// The row follows its focused cell again (a key's move along it, a click on one of its cards).
    pub fn unfree(&mut self) {
        self.free = false;
    }
    /// Whether a glide holds the row ([`glide`](Self::glide)).
    pub fn is_free(&self) -> bool {
        self.free
    }
    /// Restore a saved viewport without animating from the constructor's origin. Clamp against
    /// current content because the row may have shrunk while its screen was covered or evicted.
    pub fn restore_scroll(&mut self, scroll: f32, n: usize, sty: &RowStyle) {
        let max = sty.max_scroll(n);
        self.scroll_x.jump(scroll.clamp(0.0, max));
    }
    /// Pull the scroll back into `[0, max]` for `n` cells when it lies outside (a [`relocate`](Self::relocate)
    /// shift the row cannot honour: a tile near either end, or a row that fits whole). The scroll
    /// target is derived from the scroll, so this moves both; a scroll already inside is left
    /// alone, so a clamp that does not bite adds no spring.
    pub fn clamp_scroll(&mut self, n: usize, sty: &RowStyle) {
        let max = sty.max_scroll(n);
        let at = self.scroll_x.pos.clamp(0.0, max);
        if at != self.scroll_x.pos {
            self.scroll_x.jump(at);
        }
    }
    /// Which cell holds focus (`-1` none), as `update` last recorded it.
    #[inline]
    pub fn focus(&self) -> i32 {
        self.focus
    }
    /// How far this row's heading must rise, live — see [`heading_clearance`] for the rule and
    /// [`CardRow::update`] for the spring that holds it there.
    #[inline]
    pub fn lift(&self) -> f32 {
        self.lift.pos
    }
    /// This shelf's live label-band expansion, 0 collapsed → 1 focused. Feed it to [`under_band`];
    /// a screen wanting the SETTLED layout (a scroll target must be computed from that, never from
    /// the value mid-flight) calls `under_band` with a bare `0.0`/`1.0` instead.
    #[inline]
    pub fn band_expand(&self) -> f32 {
        self.band.pos
    }
    /// The band this shelf is reserving right now — [`under_band`] of [`CardRow::band_expand`].
    #[inline]
    pub fn under_band(&self) -> f32 {
        under_band_for(self.band_h(), self.band.pos)
    }
    /// The focused band's whole height under this row's [`CaptionPolicy`](crate::caption_policy::CaptionPolicy)
    /// ([`crate::caption_policy::band_h`]), or the legacy [`LEGACY_BAND_H`] for a row that follows none.
    pub fn band_h(&self) -> f32 {
        self.caption_policy.as_ref().map_or(LEGACY_BAND_H, crate::caption_policy::band_h)
    }
    /// This shelf's focused label block, revealed as far as its band has opened — [`TileLabel::revealed`]
    /// with this row's own band height and live expansion, and `rise` how far what the layout owner
    /// puts directly below the row (the next shelf's heading) is drawn above its place
    /// ([`crate::cards::SectionFrame::below_rise`]). Chain it onto the `TileLabel` of the focused card.
    #[inline]
    pub fn reveal(&self, label: TileLabel, rise: f32) -> TileLabel {
        label.revealed(self.band_h(), self.band.pos, rise)
    }
}

/// **The tile in a row whose drawn centre is nearest a screen x** — the shared rule for VERTICAL
/// focus movement between two rows that scroll independently.
///
/// Directional navigation follows where the user SEES a tile, not where it lives in the row's
/// array. Two shelves hold different horizontal scrolls, so carrying the index across
/// (`dst = src`) lands focus on a tile that can be most of a screen away from the one that was
/// focused — and the destination row then scrolls hard to reveal it, which reads as the page
/// lurching sideways for no reason the user can see. Pressing DOWN with a tile at the right of the
/// screen should land on the tile that is also at the right of the screen.
///
/// `x` is the SOURCE tile's centre ([`tile_centre_x`]); the four row parameters are the
/// DESTINATION's. Rounding to nearest is what makes UP↔DOWN↔UP spatially stable: both rows sit on
/// a uniform lattice, so the return trip rounds by the same fraction the outward trip did and lands
/// back where it started — unless the clamp moved it, which is the one case where the destination
/// genuinely has no tile under the cursor.
///
/// It is also what keeps the destination row STILL: [`scroll_into_view`] scrolls only for a tile
/// that would clip the viewport, and the tile nearest the source's centre is on screen whenever the
/// source was. Scrolling is then a consequence of the geometry rather than a thing to suppress.
///
/// Lifted out of the home grid's private `Grid::vert`, which has done this since that grid was
/// written and was the only screen that did. Every shelf-based screen shares it now.
pub fn column_near_x(
    x: f32,
    origin: f32,
    adv: f32,
    w: f32,
    scroll: f32,
    n: usize,
    from: usize,
) -> usize {
    if n == 0 || adv <= 0.0 {
        return 0;
    }
    // where the source centre falls on the destination's lattice, in tiles
    let t = (x - origin - w * 0.5 + scroll) / adv;
    let lo = t.floor();
    let (dlo, dhi) = (t - lo, lo + 1.0 - t);
    // **`from` is the tie-break, and it is what makes the round trip stable.** Two rows offset by
    // exactly half a tile put the cursor equidistant between two destination tiles, and ANY rule
    // that decides a tie by direction alone — round half up, half down, half to even — ratchets:
    // going out picks the right-hand tile, coming back picks the right-hand tile again, and
    // bouncing UP/DOWN walks focus along the row one tile per press while the user presses nothing
    // horizontal. Measured by `bouncing_between_two_rows_is_stable`, which failed
    // `[7, 8, 8, 9, 9, 10, 10, 11]` on a plain round-to-nearest. Keeping the index you already had
    // is the only tie-break that is symmetric under swapping the two rows, and it is also the
    // answer a viewer expects: an exact tie is not a reason to move sideways.
    let k = if (dlo - dhi).abs() < 1.0e-3 {
        let (a, b) = (lo, lo + 1.0);
        let f = from as f32;
        if (a - f).abs() <= (b - f).abs() {
            a
        } else {
            b
        }
    } else if dlo < dhi {
        lo
    } else {
        lo + 1.0
    };
    // `floor`-based rather than a cast: a cast truncates TOWARD ZERO, so a source tile left of the
    // destination row's origin rounded the wrong way before the clamp hid it.
    (k.max(0.0) as usize).min(n - 1)
}

/// Where tile `i` of a row is drawn, centre x. The other half of [`column_near_x`], so a caller
/// never spells the same lattice out twice with the chance of disagreeing about the half-tile.
#[cfg(test)]
#[inline]
pub fn tile_centre_x(i: usize, origin: f32, adv: f32, w: f32, scroll: f32) -> f32 {
    origin + i as f32 * adv - scroll + w * 0.5
}

/// The SETTLED rect of tile `i` — the one formula `cards::Shelf` draws by and `ui::geom::Shelf::place`
/// answers with (spec §7.1: draw calls place). The focus pop is applied on top by the caller
/// (`Rect::scaled` by the cell's spring), never here.
#[inline]
pub fn tile_rect(i: usize, origin: f32, pitch: f32, scroll: f32, row_y: f32, size: (f32, f32)) -> Rect {
    Rect::new(origin + i as f32 * pitch - scroll, row_y, size.0, size.1)
}

/// THE clamp-into-view core every scroller shares — uniform slots ([`scroll_into_view`]),
/// variable-width season tabs, and the home grid's vertical spring: the nearest scroll in
/// `[0, max]` at which the target band is fully revealed. `lo`/`hi` are the scrolls where the
/// item's trailing/leading edge (plus breathing room) just fits; `cur` already inside ⇒ returned
/// unchanged (no pinning).
pub fn reveal(cur: f32, lo: f32, hi: f32, max: f32) -> f32 {
    cur.clamp(lo.min(hi), hi).clamp(0.0, max)
}

/// The minimal scroll-into-view rule for uniform-slot strips (home shelves, detail
/// Related/Cast/episodes, the profiles roster): scroll only when the focused item — padded by one
/// `gap` of breathing room (which also covers the focus pop) — would clip the viewport `vw`,
/// clamped to the content range so short rows never over-scroll. (Season tabs and the vertical
/// grid have non-uniform geometry and feed their own edges straight into [`reveal`].)
pub fn scroll_into_view(cur: f32, fc: usize, n: usize, w: f32, gap: f32, vw: f32) -> f32 {
    let slot = w + gap;
    let max_sx = (n as f32 * slot - gap - vw).max(0.0);
    let lo = fc as f32 * slot + w + gap - vw; // right edge (+gap) on screen
    let hi = fc as f32 * slot - gap; // left edge (−gap) on screen
    reveal(cur, lo, hi, max_sx)
}

/// **The FULL height a `sty` heading ever rises by** — half the focus pop's growth, which is what a
/// tile magnified about its own centre lifts its top edge by.
///
/// [`heading_clearance`] is this number tapered by a proximity ramp, so it is the CEILING of that
/// function and never a value it merely happens to reach. It is named and public because a screen
/// has to RESERVE room for it: the home grid rests the focused shelf with its heading at
/// `GRID_TOP_Y − TITLE_DY` and `GRID_TOP_Y` clears the chip by this lift (a row travelling to that
/// destination still crosses the shared top band, under chrome drawn after it, exactly as its cards
/// do, and its heading fades there by where it stands: `scroll_chrome::clearance_under_shown_chrome`),
/// and it can only do that arithmetic if the lift is a fact it can ask for rather than one that
/// lives inside a draw. Written out by hand at the call site it would be the same three terms in two
/// places, and a change to `focus_scale` would move the drawn heading while leaving the layout that
/// makes room for it behind.
#[inline]
pub fn heading_lift_max(sty: &RowStyle) -> f32 {
    sty.h * (sty.focus_scale - 1.0) * 0.5
}

/// How far a row's heading must rise to stay clear of the focused tile — the ONE rule home hub
/// titles and the detail strip headings ("Related", "Cast & Crew") share. A popped tile's focus
/// glow spills past its top edge and washes over the heading above it, but only while that tile is
/// near the row's left edge (under the heading), so this is the FULL pop height tapered by a
/// proximity ramp. The first TWO slots need the whole clearance — a heading plus the glow's spill
/// reaches past slot 1, and a partial lift there let the glow climb into the title's descender
/// line; the ramp tapers from slot 2 out.
///
/// It is a **destination, not a readout**: the height comes from the style's `focus_scale`, never
/// from a live scale spring. Multiplying by the pop's current magnitude tied the heading to the
/// magnification, so it rode every selection change down and back up — and worse, the frame focus
/// moved the incoming cell's spring was still at rest, so the heading dropped its whole lift in
/// one frame and sprang back. [`CardRow`] holds the result on its own spring, so the heading
/// simply STAYS up for as long as something is under it and settles once when nothing is.
pub fn heading_clearance(focused: Option<usize>, sty: &RowStyle, scroll: f32) -> f32 {
    let Some(c) = focused else {
        return 0.0;
    };
    let x = sty.margin_x + c as f32 * (sty.w + sty.gap) - scroll;
    let near = ((sty.margin_x + sty.w * 2.5 + sty.gap - x) / sty.w).clamp(0.0, 1.0);
    heading_lift_max(sty) * near
}
// ---- the shelf HEADING, shared by every screen that draws a shelf --------------------------
//
// It lived in `home.rs` as a private pair for as long as Home was the only screen with shelves.
// The Library grew its own on 2026-09-05 (the server owner's `Library Recommended` rows), and a
// second copy would have drifted on exactly the details that make a heading readable: which run is
// the title's rung, how far the annotation sits from it, and which ink names a source.
/// The gap either side of the heading's separator dot.
pub const HEADING_SOURCE_PAD: f32 = theme::space::XS;


/// The shelf heading, flowed left→right from the heading origin: the hub's title, and — only when
/// the row came from ANOTHER server — a quiet `· handle` naming that source ("Recently Added in
/// Film Club · friend").
///
/// **With an empty source the annotation is ABSENT, not empty**: no gap, no dot, no second run, no
/// draw call. That is the design's "with one source, none of this is drawn" implemented as absence
/// rather than as a branch that draws nothing visible — so the ANNOTATION costs a single-server
/// library nothing at all, in geometry, in draw calls and in ink.
///
/// The heading is NOT unchanged overall, and the difference is deliberate: its ink moved from
/// `TEXT_PRIMARY` (`#f7fafc`) to `TEXT_HEADING` (`#ebf0f7`) in the same pass. Every shelf heading on
/// Home is ~4% darker as a result, today, with no source string anywhere. That is a harmonization,
/// not a side effect — `TEXT_HEADING` is the shared section-heading ink that `detail.rs` and
/// `person.rs` already use, and Home was the one screen still inking its headings as body text.
///
/// Two runs and not one string because they are two SIZES, two weights and three inks, and
/// `Painter::text` can express exactly one of each per call (`theme::TEXT_SEPARATOR`'s doc: a dot
/// baked into a joined string is one run at one colour by construction). They are BASELINE-aligned
/// via `text::baseline_y` — a `BODY` run top-aligned against a `HEADLINE` one reads as a
/// superscript.
///
/// `run(text, dx, sz, bold, ink) -> advance` is the seam: the draw passes a closure that paints and
/// returns `Painter::text`'s advance, the host tests pass one that measures. The drawn flow and the
/// graded one are therefore ONE expression, not `dotted_run`/`dotted_run_w`'s two that have to be
/// kept agreeing. Returns the total advance.
///
/// The flow is PURE — it takes no font metric, which is also why the host suite can drive it: the
/// per-run baseline drop is resolved in [`draw_heading`], from the very `sz`/`bold` handed out here.
///
/// Not `ui::fit::two_runs`: the runs are a sequential server-text flow, each placed after the last,
/// not a primary/secondary pair competing for one span.
pub fn heading_flow(
    title: &str,
    source: &str,
    mut run: impl FnMut(&str, f32, std::os::raw::c_int, std::os::raw::c_int, [f32; 4]) -> f32,
) -> f32 {
    heading_parts(title, "", source, |s, dx, sz, bold, ink, _| run(s, dx, sz, bold, ink))
}

/// Which note of a heading a run belongs to: the title, the count after it, or the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadingPart {
    Title,
    Count,
    Source,
}

/// The text a heading flows over: `str` for the elided path, `CStr` for the callers whose strings
/// are already NUL-terminated and so draw with no per-frame copy.
pub trait HeadingText {
    fn blank(&self) -> bool;
    fn dot() -> &'static Self;
}

impl HeadingText for str {
    fn blank(&self) -> bool {
        self.is_empty()
    }
    fn dot() -> &'static Self {
        "\u{b7}"
    }
}

impl HeadingText for std::ffi::CStr {
    fn blank(&self) -> bool {
        self.to_bytes().is_empty()
    }
    fn dot() -> &'static Self {
        c"\u{b7}"
    }
}

/// THE heading + count idiom: the title, then each quiet note that is present (the member count,
/// then the source) as `HEADING_SOURCE_PAD`, a separator dot, `HEADING_SOURCE_PAD`, the note in
/// the `BODY` rung and `TEXT_TERTIARY`. Search's result rows, a Person's shelves, a linked
/// collection heading and an annotated shelf all flow through it, so a count can never sit at a
/// different gap, size or ink on one screen than on the next. An empty note is ABSENT, as the
/// source always was. `run` also learns which [`HeadingPart`] it is painting (Search fades the
/// source when a row's owner appears).
pub fn heading_parts<S: HeadingText + ?Sized + 'static>(
    title: &S,
    count: &S,
    source: &S,
    mut run: impl FnMut(&S, f32, std::os::raw::c_int, std::os::raw::c_int, [f32; 4], HeadingPart) -> f32,
) -> f32 {
    // One flow: title, then per note a dot and the note, each `HEADING_SOURCE_PAD` from the last.
    // Each run is painted (and so measured) as it is placed, hence `place`, not a slice.
    let mut flow = Flow::new(0.0, HEADING_SOURCE_PAD);
    flow.place(|x| Item::new(run(title, x, theme::size::HEADLINE, 1, theme::TEXT_HEADING, HeadingPart::Title)));
    for (note, part) in [(count, HeadingPart::Count), (source, HeadingPart::Source)] {
        if note.blank() {
            continue; // absent, not empty: no gap, no dot, no run
        }
        flow.place(|x| Item::new(run(S::dot(), x, theme::size::BODY, 0, theme::TEXT_SEPARATOR, part)));
        flow.place(|x| Item::new(run(note, x, theme::size::BODY, 0, theme::TEXT_TERTIARY, part)));
    }
    flow.end()
}

/// [`heading_parts`] painted with its title's cap top at `(x, y)`: every run on the TITLE's
/// baseline, `source_alpha` multiplying the source's runs alone. The one painter of a
/// NUL-terminated heading (Search's rows, a Person's shelves); [`draw_heading`] is the `&str` one.
pub fn draw_heading_c(
    p: Painter,
    title: &std::ffi::CStr,
    count: &std::ffi::CStr,
    source: &std::ffi::CStr,
    x: f32,
    y: f32,
    source_alpha: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    let cap = measure.cap_h(theme::size::HEADLINE);
    heading_parts(title, count, source, |s, dx, sz, bold, ink, part| {
        let mut label = crate::label::Label::new(s.as_ptr(), sz, ink).v(crate::label::VAlign::Baseline);
        if bold != 0 {
            label = label.bold();
        }
        let p = if part == HeadingPart::Source { p.alpha(source_alpha.clamp(0.0, 1.0)) } else { p };
        label.draw(p, Rect::new(x + dx, y, 0.0, cap));
        measure.width(s, sz, bold != 0)
    });
}

/// Draw [`heading_flow`] with its title's cap top at `(x, y)`. Every run rides the painter handed
/// in, so the snap fade and the row's heading `lift` move the whole heading together — an
/// annotation that detached from its title mid-scroll would read as two objects.
///
/// Each run drops onto the TITLE's baseline (`text::baseline_y`, a no-op for the title itself,
/// which is measured against its own tokens): the annotation is a rung down, and top-aligning a
/// `BODY` run against a `HEADLINE` one would read as a superscript. That one line is the only part
/// of this heading the host suite cannot grade — it opens no SDL_ttf, so there are no cap bands to
/// measure (the same boundary `widgets`' anchor table works within); what the tests DO pin is the
/// tokens it resolves from.
pub fn draw_heading(
    p: Painter,
    title: &str,
    source: &str,
    x: f32,
    y: f32,
    max_w: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    bounded_heading_flow(title, "", source, max_w, measure, |s, dx, sz, bold, ink| {
        // the CString must outlive the draw call, not the closure (`ui/CLAUDE.md`'s first gotcha)
        match std::ffi::CString::new(s) {
            Ok(cs) => p.text(
                cs.as_ptr(),
                x + dx,
                plx_gfx::text::baseline_y(sz, bold, theme::size::HEADLINE, 1, y),
                sz,
                ink,
                0,
                bold,
            ),
            Err(_) => 0.0,
        }
    });
}

/// [`heading_flow`] against a right boundary: the one elision rule every shelf heading shares,
/// [`draw_heading`]'s and `ui::linked_heading`'s. `run` receives each run already elided and
/// returns its advance, so the measured and the drawn flow stay one expression.
///
/// **`max_w` is a RIGHT BOUNDARY, and a run that would cross it is elided rather than clipped** —
/// through `text::elide`, the single truncation impl in the app, so a bounded heading reads like
/// every other bounded string here. It exists because the Library's A–Z rail occupies the right
/// of the content region: a heading drawn to the panel edge runs UNDER the letters, which looks
/// broken and is not something the rail can fix from its own side. Every other caller passes
/// `INFINITY` and pays nothing — the elide is skipped outright, so the memoised binary search is
/// never entered for a heading nobody bounded.
pub fn bounded_heading_flow(
    title: &str,
    count: &str,
    source: &str,
    max_w: f32,
    measure: &dyn plx_machine::machine::Measure,
    mut run: impl FnMut(&str, f32, std::os::raw::c_int, std::os::raw::c_int, [f32; 4]) -> f32,
) -> f32 {
    // A note's dot is drawn only with its note: when the room the note WOULD have after the dot is
    // under `MIN_HEADING_RUN` the dot is dropped, and so is the note that follows it.
    let mut dot_dropped = false;
    heading_parts(title, count, source, |s, dx, sz, bold, ink, _| {
        let room = max_w - dx;
        if max_w.is_finite() && room < MIN_HEADING_RUN {
            return 0.0; // no room left for this run at all — drop it rather than draw a stub
        }
        if max_w.is_finite() {
            if s == <str as HeadingText>::dot() {
                let note_room = room - measure.width_str(s, sz, bold != 0) - HEADING_SOURCE_PAD;
                dot_dropped = note_room < MIN_HEADING_RUN;
                if dot_dropped {
                    return 0.0;
                }
            } else if std::mem::take(&mut dot_dropped) {
                return 0.0; // the note of a dropped dot
            }
            let owned =
                plx_gfx::text::elide_by(s, room, false, |t| measure.width_str(t, sz, bold != 0));
            run(&owned, dx, sz, bold, ink)
        } else {
            run(s, dx, sz, bold, ink)
        }
    })
}

/// The narrowest run [`draw_heading`] will draw against a bound. Below it an elide has nothing left
/// but its own ellipsis, and a heading ending in a bare `…` beside another bare `…` reads as
/// damage rather than as truncation.
const MIN_HEADING_RUN: f32 = 48.0;


/// Conservative paint bounds for a complete card, including its shadow and focused label.
/// The label anchors to the unscaled bottom even during a press, just as `draw_focused` does.
/// `pad` is inflated uniformly by both the shadow's blur AND its downward offset (rather than only
/// the bottom edge) — the same conservative, symmetric inflation `Painter::tex_carded`'s own quad
/// uses, so a shared bound can stay one `inset` call.
fn tile_paint_bounds(rect: Rect, scale: f32, labelled: bool) -> Rect {
    let pad = theme::CARD_SHADOW_BLUR + theme::CARD_SHADOW_DY + 1.0;
    let mut bounds = rect.inset(-pad);
    if labelled {
        let label_bottom = unscaled_bottom(rect, scale) + UNDER_LABEL_H;
        bounds.h = bounds.h.max(label_bottom - bounds.y);
    }
    bounds
}

/// Reject a whole card before preparing artwork or fitting overlay text. The blur source is a
/// crop of the page; its primitive-level rejection comes too late to avoid that CPU work.
pub fn paint_visible(p: Painter, rect: Rect, scale: f32, labelled: bool) -> bool {
    // Recording follows the buffered composition's text queries, including its offscreen rows.
    if p.is_recording() { return true; }
    let label_h = if labelled { UNDER_LABEL_H } else { 0.0 };
    paint_visible_with_label_h(p, rect, scale, label_h)
}

/// A card whose caller declares how far below it something is drawn (`label_h`): a grid of value
/// lines passes [`SORT_VALUE_REACH`].
pub fn paint_visible_with_label_h(p: Painter, rect: Rect, scale: f32, label_h: f32) -> bool {
    if p.is_recording() { return true; }
    let mut bounds = tile_paint_bounds(rect, scale, label_h > 0.0);
    if label_h > 0.0 { bounds.h = bounds.h.max(unscaled_bottom(rect, scale) + label_h - bounds.y); }
    bounds.x += p.dx();
    bounds.y += p.dy();
    let visible = bounds.intersect(Rect::FULL);
    visible.w > 0.0 && visible.h > 0.0
        && (crate::frame::backdrop::discovering()
            || !plx_gfx::gfx::culled(bounds.x, bounds.y, bounds.w, bounds.h))
}

/// A non-focused cell body: the art tile + an optional resume bar. `rect` is the caller's
/// already-scaled rect; `s` scales the corner radius. (The home grid's non-focused cell, verbatim.)
pub fn draw_tile(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
) {
    let rad = sty.tile_radius(rect, s);
    // every tile carries a resting shadow + 1px perimeter stroke, both FOLDED into card()'s single
    // composite pass; a near-neighbour mid-pop (s slightly >1) lifts smoothly toward the focused
    // shadow via `f`.
    let f = pop_factor(s, sty.focus_scale);
    // `card` takes the RESTING rect and the scale, not the popped rect: whatever it sets from the
    // tile's size (a fan's live name) must not re-layout on every frame of a pop.
    card(p, rect.scaled(1.0 / s), art, rad, true, s, f);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
}

/// The metadata block under a FOCUSED tile: a centred title, optionally a second caption line
/// ("S1 · E8", a year, a character name), optionally led by the Continue-Watching play glyph. Only
/// the focused tile carries it, so a row builds exactly one of these per frame.
///
/// It is a type rather than three parameters because the block's height is a fact the SCREEN needs:
/// every shelf must reserve room for it in its own flow, and `reveal`/`scroll_into_view` only keep
/// air under the block a screen *declares*. Before this, five call sites hand-authored that number
/// from constants they could not see — [`TileLabel::height`] is the answer they were restating.
#[derive(Default, Clone)]
pub struct TileLabel {
    pub title: Option<std::ffi::CString>,
    pub caption: Option<std::ffi::CString>,
    /// The sort value line of a library grid sorted by something other than Title or Random: the
    /// ONE line such a grid draws under every poster, in place of the title and details block
    /// ([`draw_sort_value`]). `None` for every other label.
    pub value: Option<SortValue>,
    /// lead the FIRST line drawn with the amber play triangle (Continue Watching's tile has no play
    /// disc): the title when the block has one, else the details line. The mark hangs to the left of
    /// the line, which is centred as if it were not there. Set only while a press on the tile plays
    /// — see `widgets::still_line`'s `press_plays`
    pub glyph: bool,
    /// **How far the band this block hangs in has opened**, the band's height, its expansion and
    /// the rise of what stands below it ([`TileLabel::revealed`]); each LINE of the block is on
    /// screen only once the room under the card holds its box ([`line_reveal_for`]). `None` for every caller whose band does not
    /// animate (the profile picker's avatars, the settings sample row): the block is whole, which is
    /// why `Default` gives it that. Every `cards::Shelf` and `cards::Grid` sets it.
    pub reveal: Option<BandOpen>,
    /// **How far the tile's row still has to scroll before this tile rests** (see
    /// [`CardRow::settle_lag`]); `0` for a tile that is not mid-glide. [`place_label`] judges the
    /// block's alignment and width at the tile's SETTLED screen position, so a glide does not
    /// re-decide them on every frame.
    pub settle_lag: f32,
    /// Set the lines with the Under-posters text shadow (`0 1px 8px` black 35%,
    /// [`crate::caption_policy::CAPTION_SHADOW`]): the caption is then set on the page itself, not
    /// on a scrim.
    pub shadowed: bool,
    /// Hold the block to the LIFTED tile it names ([`TileLabel::within_tile`]): its window is the
    /// tile's width instead of the shared reach into the neighbours' room, and a line the window
    /// cannot hold glides on the shared marquee and dissolves at BOTH of the window's edges
    /// ([`LABEL_EDGE_FADE`]) instead of being cut there.
    pub within_tile: bool,
}

/// **The sort value under one poster** of a library grid sorted by something other than Title or
/// Random (spec "Under posters", item 5): the words, an optional "+1" for a second version, the
/// rating provider's mark. The words are built once per item and cached by the screen; the grid
/// only borrows them ([`draw_sort_value`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SortValue {
    /// "98%", "2h 14m", "4K", "27 Oct", "Oct 2024": numbers and their units joined by a no-break
    /// space. When the value is empty: the en dash for data the server does not have, "Never"
    /// under Date viewed and Plays for something nobody played.
    pub text: String,
    /// A second version exists ("4K +1"): the first thing dropped when the line does not fit.
    pub plus: bool,
    /// The rating provider's mark, hung to the LEFT of the number so the number stays on the
    /// poster's axis; dropped, after the "+1", when the line does not fit. Empty for every sort
    /// that is not a score.
    pub mark: &'static [crate::widgets::MarkLayer],
    /// The value is empty: the dash for data the server does not have, or the word "Never" for
    /// something never played. Either is drawn at [`theme::SORT_VALUE_MISSING_INK`] and never bold,
    /// because an empty value stays quiet even under the focused poster.
    pub missing: bool,
}

/// The "+1" a second version adds, after a no-break space.
pub const SORT_VALUE_PLUS: &str = "\u{a0}+1";

/// A label band part-way through opening or closing: its whole height, its expansion and how far
/// what stands below it is drawn above its place ([`TileLabel::revealed`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandOpen {
    /// the band's whole height when open ([`crate::caption_policy::band_h`]).
    pub band_h: f32,
    /// its expansion, 0 collapsed → 1 open.
    pub e: f32,
    /// px the tile (a grid) or heading (a shelf) directly below the band is DRAWN above where the
    /// layout rests it: its pop, its press, its heading's lift. It takes that much of the room.
    pub rise: f32,
}

/// One line of a [`TileLabel`] block: its box under the card's unscaled bottom edge and its alpha
/// ([`TileLabel::lines`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineBox {
    /// the line box's top, px below the card's bottom edge.
    pub top: f32,
    /// the line box's bottom, px below the card's bottom edge.
    pub bottom: f32,
    /// how much of the line is drawn, 0..1.
    pub alpha: f32,
}

impl TileLabel {
    /// **A line of the block is the label BAND's content, so it may not be on screen before the
    /// room for it is.** `band_h` is the band's whole height ([`CardRow::band_h`],
    /// `Grid::band_h`), `e` its live expansion, 0 collapsed → 1 open, and `rise` how far the tile
    /// (grid) or heading (shelf) directly below the band is DRAWN above where the layout rests it
    /// (0 when it rests: a caller that passes 0 says what is below is not lifted); see
    /// [`line_reveal_for`] for the rule. The ONE rule for a shelf and a grid alike.
    pub fn revealed(mut self, band_h: f32, e: f32, rise: f32) -> Self {
        self.reveal = Some(BandOpen { band_h, e, rise });
        self
    }
    /// Chain this off a shelf's [`CardRow::settle_lag`]: the label is placed for where the tile
    /// will REST, and only translates with it on the way there.
    pub fn settling(mut self, lag: f32) -> Self {
        self.settle_lag = lag;
        self
    }
    /// **Hold this block to its tile.** For a tile whose caption carries a name that is wider than
    /// the tile (the Collections page: the face cuts the name, the caption tells it whole): the
    /// block's window is the tile, so the name never runs under the next column, and what the
    /// window cannot hold glides on the shared label marquee, dissolving at both edges
    /// ([`LABEL_EDGE_FADE`]) rather than being cut mid-letter. Every other label keeps the shared
    /// reach into its neighbours' room ([`under_budget`]).
    pub fn within_tile(mut self) -> Self {
        self.within_tile = true;
        self
    }
    /// A block from [`crate::caption_policy::caption_lines`]: the title line (if the policy shows
    /// one) over the details line, centred and shadowed. A line the policy leaves out is absent.
    pub fn from_lines(lines: [Option<String>; 2]) -> Self {
        let [title, details] = lines;
        TileLabel {
            title: title.and_then(|t| std::ffi::CString::new(t).ok()),
            caption: details.and_then(|d| std::ffi::CString::new(d).ok()),
            shadowed: true,
            ..Default::default()
        }
    }
    /// Title only — the plain poster shelf.
    pub fn title(t: &str) -> Self {
        TileLabel {
            title: std::ffi::CString::new(t).ok(),
            ..Default::default()
        }
    }
    /// Title + a caption line; an empty caption is simply absent (never a blank row).
    pub fn titled(t: &str, caption: &str) -> Self {
        TileLabel {
            caption: std::ffi::CString::new(caption)
                .ok()
                .filter(|_| !caption.is_empty()),
            ..Self::title(t)
        }
    }
    /// **The lines the block puts on screen, as boxes under the card.** `[title, details]`, each
    /// the line's box measured down from the card's UNSCALED bottom edge (the anchor
    /// [`draw_label`] places the block from) and the alpha it is drawn with; a line the block does
    /// not have is `None`. The one place the block's vertical metrics are read as boxes, so what is
    /// drawn, what a test reads and what the reveal rule is solved against cannot disagree.
    pub fn lines(&self) -> [Option<LineBox>; 2] {
        let items = Self::items(self.title.is_some(), self.caption.is_some());
        let stack = VStack::new(&items, UNDER_LINE_GAP, Anchor::Top(UNDER_DROP));
        let line = |i: usize| stack.span(i).map(|(y, h)| self.line(y, y + h));
        [line(0), line(1)]
    }
    /// The block's two lines as one stack: the title line (index 0) and the details line (index 1),
    /// each present or absent. Both [`TileLabel::lines`] and [`TileLabel::height`] read this, so the
    /// drawn lines and the reserved height are the same stack.
    const fn items(title: bool, caption: bool) -> [Item; 2] {
        [
            if title { Item::new(UNDER_LINE_H) } else { Item::absent() },
            if caption { Item::new(UNDER_CAPTION_H) } else { Item::absent() },
        ]
    }
    /// The line whose box is `top..bottom` under the card, at the alpha this block's band gives it.
    fn line(&self, top: f32, bottom: f32) -> LineBox {
        let alpha = self.reveal.map_or(1.0, |b| line_reveal_for(b.band_h, b.e, b.rise, bottom));
        LineBox { top, bottom, alpha }
    }
    /// THE height a screen must reserve under the poster row for this block. Derived from the same
    /// constants [`draw_focused`] lays it out with, so a screen's declared band and the block on
    /// screen cannot drift. The title is always exactly one line now — a title too wide for its
    /// budget marquees in place rather than growing the block, so this no longer varies with the
    /// row style at all.
    pub fn height(caption: bool) -> f32 {
        Self::block_h(caption)
    }
    /// [`Self::height`] as a `const fn`, so [`UNDER_LABEL_H`] IS this stack's full case rather than
    /// a hand-summed copy of it. The title is always a line, so the block's height is the stack's
    /// height from its top.
    const fn block_h(caption: bool) -> f32 {
        let items = Self::items(true, caption);
        UNDER_DROP + VStack::new(&items, UNDER_LINE_GAP, Anchor::Top(0.0)).height()
    }
    /// [`TileLabel::height`] for a block that has whatever THIS one has.
    #[cfg(test)]
    fn own_height(&self) -> f32 {
        Self::height(self.caption.is_some())
    }
}

/// The focused cell body — the caller draws this LAST for its z-order: the art tile, the big glow
/// ring, an optional resume bar, then its [`TileLabel`]. (The home grid's focused-last cell.)
///
/// **`s` IS THE SCALE `rect` WAS BUILT FROM — pop AND press, not the pop alone.** It is not a
/// second, decorative knob: this function divides by it twice, and both readings are wrong the
/// moment the two disagree. `f = pop_factor(s, focus_scale)` is the drop-shadow and perimeter sheen, so
/// a press that reaches the rect but not `s` leaves the tile wearing a full focus shadow while it
/// shrinks — the click reads as a jitter instead of the tile pressing INTO the page. And `ty`
/// anchors the label block to the UNSCALED card bottom as `rect.h / s`, which is the resting
/// height only while `s` is the whole scale, so the same mismatch slides the caption up and back
/// on every click, against the design's "a pop/press never moves it". Every caller in the app
/// passes the press-inclusive scale (Home, the Library's shelves AND its grid, Search, Detail's
/// Related, Person, the profile picker); the grid was the one exception, for the length of
/// phase 8, and `screens/library/parts.rs::treatment_scale` carries that account.
pub fn draw_focused(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
    label: &TileLabel,
    measure: &dyn plx_machine::machine::Measure,
) {
    draw_focused_led(p, art, rect, s, sty, resume, label, measure, None);
}

/// Ask for the focused tile's art ahead of its draw: what [`draw_focused_led`] takes as `led`.
/// The focused tile paints LAST (over its neighbours) and so, drawn in order, asks for its poster
/// last; the source serves the newest frame's requests in the order they came, and the tile the
/// reader is looking at waited for every other one (CONFIRM A1). A grid calls this before it
/// paints the others and passes the answer to the draw, so the tile still resolves ONCE.
/// `rect` and `s` are the draw's own.
pub fn lead_focused(p: Painter, art: &Art<'_>, rect: Rect, s: f32) -> crate::widgets::ArtImage {
    crate::widgets::lead_card_art(p, rect.scaled(1.0 / s), art, true, s)
}

/// [`draw_focused`] for a tile whose art was asked for ahead of its draw ([`lead_focused`]).
/// `led` is that answer; `None` resolves in the draw.
#[allow(clippy::too_many_arguments)]
pub fn draw_focused_led(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
    label: &TileLabel,
    measure: &dyn plx_machine::machine::Measure,
    led: Option<crate::widgets::ArtImage>,
) {
    draw_focused_tile_led(p, art, rect, s, sty, resume, led);
    draw_label(p, rect, s, sty, label, measure);
}

/// [`draw_focused_led`] without the label block: the lifted tile, its sheen and its resume bar.
/// For a caller that paints the label on its own turn (the grid paints every caption beneath
/// every tile, [`draw_label`] first).
pub fn draw_focused_tile_led(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
    led: Option<crate::widgets::ArtImage>,
) {
    let rad = sty.tile_radius(rect, s);
    // Home Screen focus treatment: soft drop-shadow + 1px perimeter sheen, both FOLDED into card()'s
    // single composite pass and ramped in with the pop `f` (the same 0→1 pop scalar the ring used).
    let f = pop_factor(s, sty.focus_scale);
    // `card` takes the RESTING rect and the scale, not the popped rect: whatever it sets from the
    // tile's size (a fan's live name) must not re-layout on every frame of a pop.
    crate::widgets::card_named_led(p, rect.scaled(1.0 / s), art, None, rad, true, s, f, led);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
}

/// The bottom edge of the card drawn at `rect` (scale `s`) at its UNSCALED size, about the same
/// centre: the line a label block hangs from, which a pop or a press never moves.
pub fn unscaled_bottom(rect: Rect, s: f32) -> f32 {
    rect.y + rect.h * 0.5 + (rect.h / s) * 0.5
}

/// The label block under a tile drawn at `rect` (scale `s`) alone, without the tile: what
/// [`draw_focused`] ends with, and what a landing's ghost of the focused card keeps drawing while
/// the grid waits for the landing to step.
pub fn draw_label(
    p: Painter,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    label: &TileLabel,
    measure: &dyn plx_machine::machine::Measure,
) {
    // The label block anchors to the UNSCALED card bottom (rect is scaled about its center, so
    // base bottom = cy + (h/s)/2). In the mock the label is normal-flow BELOW the transformed
    // poster — a pop/press never moves it; the pop eats the poster→label gap instead of shoving
    // the label into the next shelf's title.
    let bottom = unscaled_bottom(rect, s);
    // …and each LINE of the block rides the band's reveal ([`line_reveal_for`]). The card, its ring
    // and its resume bar do NOT: they are the tile, which is on screen either way; the alpha is
    // only about whether the room under it holds the line yet.
    let lines = label.lines();
    if lines.iter().flatten().all(|l| l.alpha <= 0.0) {
        return;
    }
    draw_label_block(p, rect, sty, label, lines, bottom, measure);
}

/// The sort value line's box: CAPTION 24 on a 24 px line.
pub const SORT_VALUE_H: f32 = 24.0;
/// How far under the poster's bottom edge the value line's box starts at rest.
pub const SORT_VALUE_GAP: f32 = 12.0;
/// The same, measured from the LIFTED poster's bottom edge when the card is focused.
pub const SORT_VALUE_GAP_LIFTED: f32 = 14.0;
/// A row's collapsed gap under its posters while values show (40 otherwise): the air above a value
/// is at most half the air below it, so it reads as its own poster's.
pub const SORT_VALUE_ROW_GAP: f32 = 60.0;
/// How far the FOCUSED row opens past the collapsed one while values show (the board's `openBy`).
pub const SORT_VALUE_OPEN: f32 = 26.0;
/// The Under posters band height ([`crate::caption_policy::band_h`]'s unit: the whole open gap)
/// that opens a row [`SORT_VALUE_OPEN`] past the 40 px collapsed gap the band machinery counts from.
pub const SORT_VALUE_BAND_H: f32 = COLLAPSED_GAP + SORT_VALUE_OPEN;
/// Room under an UNSCALED poster bottom a value line can reach (the lifted card's shift, the focused
/// gap and the box), what the grid culls and reveals with.
pub const SORT_VALUE_REACH: f32 = 56.0;
/// The rating mark beside a value: a 20 px box, the board's size, hung [`SORT_VALUE_MARK_GAP`] to the
/// left of the number. A tomato is drawn past its box by `icons::mark_zoom` so its body is the board's
/// 15 px disc (its viewBox leaves room for the stem), centred in the same box.
const SORT_VALUE_MARK_D: f32 = 20.0;
/// A budget no line reaches: asking the memoised fit for it yields the run's natural width.
const SORT_VALUE_NATURAL: f32 = 1.0e5;
const SORT_VALUE_MARK_GAP: f32 = 6.0;

/// **The two textures of a sort value at lift factor `t`** (0 at rest, 1 fully lifted), as the
/// `(regular, bold)` alphas: the weight change is a crossfade on the lift's own spring, so it is a
/// function of the spring's value and nothing else (a snapped lift shows exactly one texture), and
/// the LEAVING texture fades faster than the arriving one (the card taking focus is `rising`: its
/// regular texture leaves; a card giving it up leaves with its bold). Both ends are exact.
pub fn sort_value_weights(t: f32, rising: bool) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    if rising { ((1.0 - t) * (1.0 - t), t) } else { (1.0 - t, t * t) }
}

/// **A value line, drawn under one poster of a grid** (`rect` is the poster as drawn, scale `s`):
/// CAPTION 24, tabular figures (the face's digits), centred on the poster's axis, 12 px under the
/// poster at rest at 80% white and regular, 14 px under the LIFTED poster at 100% white and bold
/// once the card is focused; between the two the weight is a crossfade of two textures on the
/// lift ([`sort_value_weights`]). Never smaller type and never an ellipsis: a line wider than the
/// tile drops its "+1", then its mark. The mark hangs to the left of the number, which stays on
/// the axis, so the line is judged against the tile as symmetric about it.
pub(crate) fn draw_sort_value(
    p: Painter, rect: Rect, s: f32, sty: &RowStyle, value: &SortValue,
    measure: &dyn plx_machine::machine::Measure, focused: bool,
) {
    use crate::caption_policy::{CAPTION_SHADOW, CAPTION_SHADOW_DY};
    let size = theme::size::CAPTION;
    let t = crate::widgets::pop_factor(s, sty.focus_scale);
    let top = rect.y + rect.h + SORT_VALUE_GAP + (SORT_VALUE_GAP_LIFTED - SORT_VALUE_GAP) * t;
    let cy = top + SORT_VALUE_H * 0.5;
    let hang = 2.0 * (SORT_VALUE_MARK_D + SORT_VALUE_MARK_GAP);
    // Judged on the bold face, the widest the line gets, so the choice is the same whichever
    // texture shows; the memoised fitted run answers it, so a warm frame measures nothing.
    let width_of = |text: &str, mark: bool| measure.fit_line_measured(text, SORT_VALUE_NATURAL, size, true).1
        + if mark { hang } else { 0.0 };
    let has_mark = !value.mark.is_empty();
    let with_plus = value.plus.then(|| format!("{}{SORT_VALUE_PLUS}", value.text));
    // A line that fits at its natural width is not given a budget it could be cut to: `natural` is
    // that verdict, taken from the measure the choice already made (a dropped mark is measured bare).
    let (text, mark, natural) = match &with_plus {
        Some(plus) if width_of(plus, has_mark) <= sty.w => (plus.as_str(), value.mark, true),
        _ if width_of(&value.text, has_mark) <= sty.w => (value.text.as_str(), value.mark, true),
        _ => (value.text.as_str(), &[][..], has_mark && width_of(&value.text, false) <= sty.w),
    };
    let budget = if natural { SORT_VALUE_NATURAL } else { sty.w };
    let (regular, bold) = if value.missing { (1.0, 0.0) } else { sort_value_weights(t, focused) };
    let mut width = 0.0;
    for (is_bold, alpha, ink) in [(false, regular, if value.missing { theme::SORT_VALUE_MISSING_INK } else { theme::SORT_VALUE_INK }),
        (true, bold, theme::TEXT_PRIMARY)] {
        if alpha <= 0.0 { continue; }
        let (run, w) = measure.fit_line_measured(text, budget, size, is_bold);
        width = f32::max(width, w);
        let y = plx_gfx::text::text_vcenter_y(size, i32::from(is_bold), cy);
        let layer = p.alpha(alpha);
        layer.text(run.as_ptr(), rect.cx(), y + CAPTION_SHADOW_DY, size, CAPTION_SHADOW, 1, i32::from(is_bold));
        layer.text(run.as_ptr(), rect.cx(), y, size, ink, 1, i32::from(is_bold));
    }
    if mark.is_empty() { return; }
    let mark_rect = Rect::new(rect.cx() - width * 0.5 - SORT_VALUE_MARK_GAP - SORT_VALUE_MARK_D, cy - SORT_VALUE_MARK_D * 0.5,
        SORT_VALUE_MARK_D, SORT_VALUE_MARK_D);
    // The art's own zoom puts a tomato's body at the board's 15 px in the 20 px box (`icons::mark_zoom`).
    for (mask, tint) in mark { crate::icons::draw(p, *mask, mark_rect.scaled(crate::icons::mark_zoom(*mask)), *tint); }
}

/// **The state line under a focused Continue Watching tile** (`registry::captions::shelf_label`: the
/// caption rung of the deck's state, replacing the details line, and always shown on the deck (the
/// owner's 2026-10-22 call), Details Nothing included (`captions::deck_policy`); a
/// plain poster's details line is worded by `caption_policy::caption_lines`, the one rule). An
/// in-progress episode reads "S3 · E4 · 8m left" and a next-up episode (no resume point yet)
/// "S2 · E1 · New episode"; a resumed movie reads just the time remaining. The episode's ADDRESS
/// leads because the deck's poster is the show's own art and already names the show (the owner's
/// call, 2026-10-22, replacing the Home Screen.dc "<show> · 8m left" wording); `title` above it
/// carries the episode's own name, so the address never repeats it. An unnumbered episode (the
/// server sent no index) still names its show: "Show · 8m left", "Show · New episode".
///
/// "In progress" is `PmsMovie::resume_frac` — THE resume rule, which the caller applies when it
/// fills [`TileFacts::resume`], and the same call `Grid::draw` makes for the BAR in the pass that
/// asks for this caption. A hand-written
/// `resume_ms > 0 && dur_ns > 0` here dropped that rule's end-guard, so a finished item whose
/// server never cleared `viewOffset` drew NO bar (`resume_frac`'s answer) under a caption still
/// promising "1m left" — `fmt::time_left` floors at a minute, so it could not even read zero.
/// One item, described two ways in one draw. Whether that tile also wore the watched TICK is a
/// separate question with a separate answer: `widgets::poster_mark` reads `PmsMovie::watched`, so
/// a stale past-end `viewOffset` on an item the server has not marked watched drew no mark at all.
pub fn cw_caption(m: &TileFacts<'_>) -> Option<std::ffi::CString> {
    use crate::caption_policy::join;
    use plx_platform::i18n::msg;
    let show = m.show_title;
    let is_episode = m.kind == TileKind::Episode;
    let address = (is_episode && m.ep_index > 0)
        .then(|| crate::fmt::episode_address(i64::from(m.season_index), i64::from(m.ep_index)));
    let s = if let Some(resume) = m.resume {
        let left = crate::fmt::time_left(resume.left_ms);
        match address {
            Some(a) => join(vec![a, left]),
            None if is_episode && !show.is_empty() => join(vec![show.to_string(), left]),
            None => Some(left), // a resumed movie: time-remaining alone
        }
    } else if is_episode {
        // next-up episode: no resume point, so no bar and no time — just the "New episode" cue
        match address {
            Some(a) => join(vec![a, msg::widgets_card_new_episode().to_string()]),
            None if show.is_empty() => Some(msg::widgets_card_new_episode().to_string()),
            None => Some(msg::widgets_card_show_new_episode(show)),
        }
    } else {
        return None;
    };
    std::ffi::CString::new(s?).ok()
}

// The under-tile metadata block's four metrics — the ONLY authority on its geometry, which
// [`TileLabel::height`] hands back to the screens that must reserve room for it.
/// poster bottom → the title block's first line.
const UNDER_DROP: f32 = 30.0;
/// one title line's pitch (`Person Screen.dc.html`'s `line-height: 30px`).
const UNDER_LINE_H: f32 = 30.0;
/// title block → caption (the mock's `margin-top: 4px`).
const UNDER_LINE_GAP: f32 = 4.0;
/// the caption's own line box. `UNDER_DROP + UNDER_LINE_H + UNDER_LINE_GAP + this` = 92, which is
/// exactly the band `detail.rs` hand-authored for its one-line-title-plus-role cast tiles — the
/// coincidence that confirms the decomposition.
const UNDER_CAPTION_H: f32 = 28.0;
/// The focused label block's total height with a caption line (item 11) — the same 92 the doc
/// comment above derives, exposed so a SCREEN's row pitch can be built from the one number that
/// also drives [`TileLabel::height`], instead of a screen re-deriving or (worse) hand-copying it.
/// `consts::UNDER_LABEL_AIR` is this constant's other half: the two together are what unifies
/// Home's and Library's row spacing.
pub const UNDER_LABEL_H: f32 = TileLabel::block_h(true);

/// **What an UNFOCUSED shelf reserves where the focused one reserves [`UNDER_LABEL_H`]** — the
/// collapsed half of the band, and the reason it is a subtraction rather than a taste number.
///
/// A shelf's label block only exists while that shelf holds focus. Reserving all 92px of it on
/// every shelf all the time — which is what `consts::ROW_PITCH` did until 2026-09-05 — spends most
/// of a 1080 panel's vertical budget on emptiness: on a three-shelf screen it is 184px of nothing,
/// two thirds of a poster. The owner reported it as "the gap from the previous shelf feels a bit
/// too large".
///
/// So the band OPENS on focus and closes again behind it ([`CardRow::under_band`]), and what stays
/// behind is the only thing an unfocused shelf actually owes its neighbour: the air between a
/// poster's bottom edge and the next section's heading. That distance is a REGION gap in the design
/// system's own vocabulary — one titled block of content to the next — so it is `space::LG`, and
/// this constant is whatever is left of it once `consts::UNDER_LABEL_AIR` (already inside the pitch,
/// below the block) is taken out. Authoring the 18 directly would be a number nobody could check;
/// authoring the rung makes the claim falsifiable, and the test beside it does.
pub const LABEL_BAND_COLLAPSED: f32 =
    crate::theme::space::LG - crate::consts::UNDER_LABEL_AIR;

/// The label band a shelf reserves at expansion `e` (0 collapsed → 1 focused) — **the ONE
/// expression every screen's row pitch is built from**, so Home, the Library, Search and the person
/// page cannot collapse by three different amounts.
///
/// A screen's pitch is its own fixed part plus this: for a poster shelf that fixed part is
/// `consts::ROW_PITCH - UNDER_LABEL_H`, and `under_band(1.0)` is `UNDER_LABEL_H` exactly, so a
/// focused shelf is byte-identical to the geometry that existed before the band could close.
pub fn under_band(e: f32) -> f32 {
    under_band_for(LEGACY_BAND_H, e)
}

/// The whole gap under a focused poster before the next row (the label block and the air below
/// it) with the legacy 92 px caption block: what the tiles that are not posters (a landscape still row, a
/// headshot row) open to under every setting.
/// The Libraries' "Under posters" setting replaces it with [`crate::caption_policy::band_h`].
pub const LEGACY_BAND_H: f32 = UNDER_LABEL_H + crate::consts::UNDER_LABEL_AIR;

/// The gap a collapsed row keeps under its posters: [`LABEL_BAND_COLLAPSED`] plus the air, the
/// design system's `space::LG` (40 px).
const COLLAPSED_GAP: f32 = LABEL_BAND_COLLAPSED + crate::consts::UNDER_LABEL_AIR;

/// How far a focused row's band opens past the collapsed one for a band of `band_h`: what the row
/// below is carried down by ([`BAND_OPEN`] for the legacy band, 40 / 72 / 0 px for the Under
/// posters setting's 80 / 112 / 0).
pub fn band_open_for(band_h: f32) -> f32 {
    under_band_for(band_h, 1.0) - LABEL_BAND_COLLAPSED
}

/// The gap a FOCUSED row opens to for a band of `band_h` ([`crate::caption_policy::band_h`]): the
/// band itself, never less than the collapsed gap, so a policy showing nothing opens nothing.
pub fn open_gap(band_h: f32) -> f32 {
    band_h.max(COLLAPSED_GAP)
}

/// [`under_band`] for a band of `band_h`: the label band at expansion `e` (0 collapsed → 1 open).
/// Only the focused row opens it; non-focused rows stay [`COLLAPSED_GAP`] apart.
pub fn under_band_for(band_h: f32, e: f32) -> f32 {
    LABEL_BAND_COLLAPSED + (open_gap(band_h) - COLLAPSED_GAP) * e.clamp(0.0, 1.0)
}

/// How far past the moment the room holds a line's box the line takes to come in fully, in px of
/// room: a hand-over over a few px, so a line is neither cut in whole nor drawn over the row under
/// it while it fades. Not a count of anything: a distance in the band's own units.
const LINE_REVEAL_RAMP: f32 = 8.0;

/// The room under a card's bottom edge at band expansion `e`, to the DRAWN top of what stands below
/// it: the label band and the air below it (how far down the NEXT row's tiles (a grid) or heading
/// (a shelf) rest from this card's bottom) less `rise`, how far that thing is drawn above its rest
/// (a tile popping to its focus scale lifts its top edge by up to [`heading_lift_max`]; a heading
/// rises by its lift). A tile or heading is drawn over a caption, so the room that matters is to its
/// drawn top, not its cell's.
pub fn room_for(band_h: f32, e: f32, rise: f32) -> f32 {
    crate::consts::UNDER_LABEL_AIR + under_band_for(band_h, e) - rise.max(0.0)
}

/// **How much of a caption line is on screen: only once the room under its card holds its box.**
/// `bottom` is the line box's bottom edge below the card's bottom ([`LineBox::bottom`]), `band_h`
/// the band's whole height, `e` its live expansion and `rise` how far what stands directly below it
/// is drawn above its place ([`room_for`]).
///
/// The block hangs at a FIXED offset under its card the instant its row takes focus, while the
/// room for it opens over the band's whole travel and the next row's tiles (a grid) or heading (a
/// shelf) are carried down on that same spring, and the tile that takes the focus pops up into the
/// room (or the one that loses it lets go of it). A line drawn before the room holds it is drawn
/// OVER what is rising to its place (a half-faint title through the tiles of the row below, the
/// departing block sliding under the row that rises into its place, or under the card that pops
/// into it). So a line shows from the moment `room_for(band_h, e, rise)` reaches its `bottom`,
/// smoothstepped over [`LINE_REVEAL_RAMP`] px of further room, and never more than the band itself
/// has opened (`e`), so a line is still its band's content after the room holds it. The title (the
/// block's first line) therefore comes in before the details line under it, and goes out after it;
/// and on the way out the departing line is gone as soon as the room that held it is. A shelf and a
/// grid use this one rule.
///
/// The ramp starts AT `bottom`, never before it: the first visible frame of a line already has the
/// whole box clear of what is under it.
pub fn line_reveal_for(band_h: f32, e: f32, rise: f32, bottom: f32) -> f32 {
    let t = ((room_for(band_h, e, rise) - bottom) / LINE_REVEAL_RAMP).clamp(0.0, 1.0);
    (t * t * (3.0 - 2.0 * t)).min(e.clamp(0.0, 1.0))
}

/// The fixed half of a poster shelf's pitch — everything `consts::ROW_PITCH` is made of EXCEPT the
/// label band. Add [`under_band`] to it.
pub const ROW_PITCH_FIXED: f32 = crate::consts::ROW_PITCH - UNDER_LABEL_H;

/// How much a shelf GROWS when it takes focus — the travel of [`under_band`], and what one focus
/// move costs (or reclaims) in document height.
pub const BAND_OPEN: f32 = UNDER_LABEL_H - LABEL_BAND_COLLAPSED;

/// **The SETTLED top of shelf `r`** in a column of uniform shelves whose fixed pitch is
/// `pitch_fixed`, with `focus` (if any) holding the open band and every other shelf collapsed.
///
/// Scroll targets, content height and every clearance test must be computed from THIS and never
/// from the live [`CardRow::under_band`], and that is the whole correctness argument of the
/// collapsing band: the reveal target and the band are two springs travelling at one rate to two
/// destinations. Derive the reveal target from the band's CURRENT value and the target moves every
/// frame while the band chases it — the row arrives, then drifts. Derive it from where the band is
/// GOING and both springs run one fixed distance, so their difference (which is what the eye sees:
/// the row's screen y) is itself a single critically-damped move.
pub fn settled_top(r: usize, focus: Option<usize>, pitch_fixed: f32) -> f32 {
    settled_top_for(r, focus, pitch_fixed, BAND_OPEN)
}

/// [`settled_top`] for a focused row that opens by `band_open` (40 px for a one-line band of 80,
/// 72 for two lines of 112, 0 for none: `under_band_for(band_h, 1.0) - LABEL_BAND_COLLAPSED`).
pub fn settled_top_for(r: usize, focus: Option<usize>, pitch_fixed: f32, band_open: f32) -> f32 {
    r as f32 * (pitch_fixed + LABEL_BAND_COLLAPSED)
        + if focus.is_some_and(|f| f < r) {
            band_open
        } else {
            0.0
        }
}

// ---- The focused single-line title's marquee (item 10) ---------------------------------------
//
// Widening the budget ([`under_budget`]) is enough for most real titles; a handful still overflow
// it ("Everything Everywhere All at Once" under a 250px card at LABEL-bold), and eliding those to
// "Everything Ev…" is exactly the defect item 10 reports. So the single-line title falls through
// to a looping marquee whenever it still does not fit: a beat of rest so the couch can start
// reading, a slow glide left, a gap, then the same run re-entering from the right, forever while
// this tile holds focus.

// The CAPTION under the title is the same story one size down (a year, a character, an episode's
// "Show · S1 · E4", a server handle): elided it hid the part that tells two credits apart, so it
// marquees in the same block when it still does not fit. Title and caption are ONE block with ONE
// clock and ONE cycle ([`marquee::Marquee::glide_in`]) — they leave their rest beat together and
// loop together; a line that fits stays plain and still.
//
// The timing (rest beat, glide, loop) is `crate::marquee`'s, shared with the menu rows; this
// shelf's focused label block owns its own clock there.
use crate::marquee::{self, TITLE as MARQUEE};

thread_local! {
    /// The focused tile's label-block marquee clock (title AND caption, keyed by the block).
    /// Only one tile holds focus app-wide, so one clock; a popover menu's focused row has its own
    /// (`table`), so the two never restart each other.
    static TITLE_CLOCK: marquee::Clock = const { marquee::Clock::new() };
}

/// Air above and below a marquee's clip window, so ascenders and descenders glide inside the clip
/// instead of being sliced by it.
const MARQUEE_AIR: f32 = 6.0;


/// Air between Continue-Watching's play glyph and the name that follows it.
const PLAY_ICON_GAP: f32 = 10.0;

/// The Continue-Watching glyph's pixel size at `sz` — 72% of the title's own size, rounded (Home
/// Screen.dc's play-triangle proportion). Pure so both branches of [`title_marquee`] read the
/// same number rather than each rounding it themselves.
#[inline]
fn play_icon_size(sz: std::os::raw::c_int) -> f32 {
    (sz as f32 * 0.72).round()
}

/// How much of the marquee's window a glyph-led title gives up to the icon + its gap — `0.0` for a
/// plain title. Pulled out to its own pure function so a host test can pin the arithmetic
/// [`title_marquee`]'s overflow and fitting branches both depend on,
/// without a `Painter` to draw through.
#[inline]
fn glyph_lead(sz: std::os::raw::c_int, glyph: bool) -> f32 {
    if glyph {
        play_icon_size(sz) + PLAY_ICON_GAP
    } else {
        0.0
    }
}

/// **The amber play mark that leads a label line**, `isz` ([`play_icon_size`]) square at `x`, centred
/// on the cap band of the line of size `sz` / weight `bold` whose cap top is `y`. The one drawing of
/// it, whichever line (title or details) the block leads with.
fn draw_play_mark(p: Painter, x: f32, y: f32, sz: std::os::raw::c_int, bold: std::os::raw::c_int) {
    let isz = play_icon_size(sz);
    let (ct, cb) = plx_gfx::text::text_cap_band(sz, bold);
    let icy = y + (ct + cb) * 0.5;
    let mark = Rect::new(x, icy - isz * 0.5, isz, isz);
    #[cfg(any(test, feature = "test-support"))]
    if p.is_recording() || crate::frame::backdrop::discovering() {
        crate::draw_census::note_play_mark(p.place(mark));
    }
    crate::icons::draw(p, crate::icons::Icon::Play, mark, theme::RESUME_FILL);
}

/// How the lines of a label block sit in its window — the one alignment every line shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelAlign {
    /// Every line is centred on the card (`x + w / 2`).
    Centre,
    /// Every line starts at `x`: the card's own leading (left) edge.
    Leading,
    /// Every line ends at `x + w`: the card's own trailing (right) edge. The window reaches LEFT
    /// from the card, over the room its neighbours leave, instead of off the card's right.
    Trailing,
}

/// Where a focused tile's label block lands and how it aligns — the ONE placement every line
/// under the tile (title, Continue-Watching glyph + name, caption) is drawn through, so the lines
/// always read as one unit: all centred on the card, all sharing its leading edge, or all sharing
/// its trailing edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelPlace {
    /// The block's left edge, in the painter's own space.
    pub x: f32,
    /// The width the block may use; a run that does not fit it marquees (title and caption alike).
    pub w: f32,
    /// How every line sits in `x..x + w`.
    pub align: LabelAlign,
}

impl LabelPlace {
    /// The left edge of a `run`-wide line inside the block. A run wider than the block starts at
    /// its left edge (it glides from there), whatever the alignment.
    fn run_x(&self, run: f32) -> f32 {
        match self.align {
            LabelAlign::Centre => self.x + (self.w - run) * 0.5,
            LabelAlign::Leading => self.x,
            LabelAlign::Trailing => self.x + (self.w - run).max(0.0),
        }
    }
}

/// How far a label line dissolves at each edge of the window it glides inside, when the block is
/// held to its tile ([`TileLabel::within_tile`]): the run is faded out over its last this-many px
/// before the window's right edge, and faded in over the window's first this-many px once its head
/// has glided past the left edge ([`lead_band`]), instead of being cut hard mid-letter at either.
pub const LABEL_EDGE_FADE: f32 = 24.0;

/// A run whose head is no further than this past its window's left edge is AT REST there: it takes
/// no leading dissolve. Float noise, not travel: a line parked at the end of its glide sits on the
/// edge to within an ulp of `-off + run_w + GAP`, and that is not a run to dim.
const LEAD_REST_EPS: f32 = 0.01;

/// Air kept between a label block and the PANEL's edge (or a band the screen reserves there).
/// A label belongs to its card, not to the safe area: it is never moved toward the safe frame.
const EDGE_PAD: f32 = 16.0;

/// The horizontal room a label block may use, in the painter's space — see [`anchor_label`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LabelRoom {
    /// The panel's left edge less [`EDGE_PAD`]: a CENTRED block must sit inside `lo..hi`.
    pub lo: f32,
    /// The panel's right edge less [`EDGE_PAD`] and any screen reserve ([`RowStyle::right_reserve`]).
    pub hi: f32,
    /// The row's left content margin ([`RowStyle::margin_x`]): the furthest left a block that
    /// reaches left of its card ([`LabelAlign::Trailing`]) may start.
    pub floor: f32,
}

/// **The pure placement decision** for a `w`-wide block under a card that spans `lead..trail`
/// (unscaled), given the room `room`. `want` is the block's UNCAPPED width (`w` is the widest run
/// clamped to [`under_budget`], the reach into a neighbour's room); only a block that reaches
/// LEFT of its card may use more than `w`.
///
/// 1. the block centred on the card sits inside `lo..hi` → [`LabelAlign::Centre`], `w` wide (the
///    design's normal case; nothing is clipped, nothing changes);
/// 2. else it starts at the card's leading edge ([`LabelAlign::Leading`]) and keeps the room to
///    `hi`, up to `w` — whenever that shows the WHOLE block (a card with room to its right);
/// 3. else the block overflows its room on the right. If the room LEFT of the card's trailing edge
///    (down to `room.floor`) is larger than the room right of its leading edge, it is
///    [`LabelAlign::Trailing`]: the window ends on the card's own right edge and reaches left, up
///    to `want` wide, so the card at the end of a row keeps its caption under it instead of
///    running into the margin;
/// 4. else it stays leading and glides in the room to `hi`.
///
/// A block that still does not fit the room it was given glides inside it (the caller's marquee).
/// The trailing edge is the card's, but never past `hi`: a card whose right edge is inside a
/// reserved band (the Library's rail) ends its block where the band starts.
pub fn anchor_label(lead: f32, trail: f32, w: f32, want: f32, room: LabelRoom) -> LabelPlace {
    let cx = (lead + trail) * 0.5;
    let x = cx - w * 0.5;
    if x >= room.lo && x + w <= room.hi {
        return LabelPlace { x, w, align: LabelAlign::Centre };
    }
    // Whole pixels: the room left before the panel edge is what the runs marquee against, and a
    // gliding shelf must not re-key the elide cache with every sub-pixel of travel.
    let right = (room.hi - lead).max(0.0);
    let trail = trail.min(room.hi);
    let left = (trail - room.floor).max(0.0);
    if w > right && left > right {
        let w = want.max(w).min(left).floor();
        return LabelPlace { x: trail - w, w, align: LabelAlign::Trailing };
    }
    LabelPlace { x: lead, w: right.min(w).floor(), align: LabelAlign::Leading }
}

/// **The window of a block held to its tile** ([`TileLabel::within_tile`]): `tile_w` wide from the
/// card's leading edge `lead`, never wider. A `run` that fits is centred on the card like any
/// block; one that does not leads from the window's edge — the lines of the block share that edge,
/// as they do in [`anchor_label`]'s leading case — and glides inside it (the caller's marquee).
/// The tile is the UNSCALED card, so the pop never re-decides which lines overflow.
fn tile_window(lead: f32, tile_w: f32, run: f32) -> LabelPlace {
    if run <= tile_w {
        LabelPlace { x: lead + (tile_w - run) * 0.5, w: run, align: LabelAlign::Centre }
    } else {
        LabelPlace { x: lead, w: tile_w, align: LabelAlign::Leading }
    }
}

/// **The leading dissolve of a run whose head is `left - x` px past its window's left edge**, as
/// `(from, to)` px from the string's own left edge: alpha 0 at the window's edge, 1 a
/// [`LABEL_EDGE_FADE`] later. `None` for a run at rest (starting at the edge, or not yet past it —
/// [`LEAD_REST_EPS`]): it draws crisp.
///
/// The band is as wide as the run has travelled, up to the full [`LABEL_EDGE_FADE`]. The marquee
/// moves 40 px/s, so its first glide frame is a fraction of a pixel past the edge; a full-width
/// band from the first frame would drop the head of the line (still full alpha at rest, 24 px of
/// it) to most of its alpha in that one frame. Grown with the travel, the picture is the rest
/// picture at zero travel and the full dissolve from `LABEL_EDGE_FADE` px on.
fn lead_band(x: f32, left: f32) -> Option<(f32, f32)> {
    let past = left - x;
    (past > LEAD_REST_EPS).then(|| (past, past + past.min(LABEL_EDGE_FADE)))
}

/// **The mirror of [`lead_band`] for a right-to-left run**, whose head is its RIGHT end: the
/// dissolve over the part of the run that is `x + run_w - right` px past the window's right edge,
/// as `(from, to)` px from the string's own left edge, glyph alpha 1 at `from` and 0 at the
/// window's edge. `None` at rest, where the run's first words sit on that edge and draw crisp.
/// It grows with the travel for the same reason the leading band does.
fn head_band_right(x: f32, run_w: f32, right: f32) -> Option<(f32, f32)> {
    let past = x + run_w - right;
    (past > LEAD_REST_EPS).then(|| (right - x - past.min(LABEL_EDGE_FADE), right - x))
}

/// One line of a label block at `x`, plain, or — given the window it glides inside, `(left, right)`
/// — dissolving over the window's first [`LABEL_EDGE_FADE`] px for the part of a run that has
/// glided past the left edge ([`lead_band`]) and over its last [`LABEL_EDGE_FADE`] px for the part
/// that reaches the right. `run_w` is the line's drawn width; a run clear of both edges takes the
/// plain program. A right-to-left run (`rtl`) is the mirror image: its head is at the right, so
/// the band that grows with the travel is the right one ([`head_band_right`]), and its cut tail
/// dissolves over the window's first [`LABEL_EDGE_FADE`] px at the left.
fn block_text(
    p: Painter,
    text: *const c_char,
    x: f32,
    y: f32,
    sz: std::os::raw::c_int,
    ink: [f32; 4],
    bold: std::os::raw::c_int,
    run_w: f32,
    window: Option<(f32, f32)>,
    rtl: bool,
) {
    let (lead, trail) = window.map_or((None, None), |(left, right)| {
        if rtl {
            (
                (x < left + LABEL_EDGE_FADE).then_some((left - x, left + LABEL_EDGE_FADE - x)),
                head_band_right(x, run_w, right),
            )
        } else {
            (
                lead_band(x, left),
                (x + run_w > right - LABEL_EDGE_FADE).then_some((right - LABEL_EDGE_FADE - x, right - x)),
            )
        }
    });
    if lead.is_none() && trail.is_none() {
        p.text(text, x, y, sz, ink, 0, bold);
    } else {
        p.text_fade_edges(text, x, y, sz, ink, bold, lead, trail);
    }
}

/// **Place a `w`-wide label block under the tile `rect`** — [`place_label_wide`] for a block with
/// no wider wish. A block is centred on its card, starts at the card's leading edge, or (at the
/// end of a row, where the room on the right is not enough) ends at its trailing edge; it is
/// never shifted by some other amount toward a screen edge.
pub fn place_label(p: Painter, rect: Rect, sty: &RowStyle, w: f32, lag: f32) -> LabelPlace {
    place_label_wide(p, rect, sty, w, w, lag)
}

/// **[`anchor_label`] with the card's edges and the screen's room.** `w` is the block's width
/// clamped to [`under_budget`], `want` its uncapped one.
///
/// **The panel test is a screen fact and `rect` usually is not.** a shelf is drawn through
/// `translate(-scroll_x, 0)`, so every rect below it is a CONTENT coordinate; `p.dx()` converts,
/// so the comparison happens in screen space and the answer comes back in the painter's. An
/// untranslated caller (`home`, `library`, `profiles`) has `dx == 0`.
///
/// **Where the tile is going, not where it is.** `lag` is [`CardRow::settle_lag`]: the
/// alignment and the block's width are decided at the tile's SETTLED screen position, while `x`
/// still follows the live card. A tile that glides in from the right used to cross the panel-edge
/// threshold part-way through, so the block flipped alignment and re-sized (re-deciding which
/// lines overflow, flipping the title into its marquee) on the way — the label visibly jumped.
///
/// The right bound is the panel's edge less whatever the SCREEN has reserved there
/// (`RowStyle::right_reserve` — the Library's A–Z rail); the left one the row's content margin
/// ([`RowStyle::margin_x`]). The card's edges are the UNSCALED card's, so a focus pop never moves
/// the block or re-keys the width-keyed wrap/elide caches.
pub fn place_label_wide(p: Painter, rect: Rect, sty: &RowStyle, w: f32, want: f32, lag: f32) -> LabelPlace {
    let settled_dx = p.dx() + lag;
    let room = LabelRoom {
        lo: EDGE_PAD - settled_dx,
        hi: SCR_W - sty.right_reserve - EDGE_PAD - settled_dx,
        floor: sty.margin_x - settled_dx,
    };
    let lead = rect.cx() - sty.w * 0.5;
    anchor_label(lead, lead + sty.w, w, want, room)
}

/// The focused tile's single-line title, drawn in the block `at`: plain whenever the run fits the
/// block, else a looping [`crate::marquee`] glide inside it (in step with the caption's, through
/// `block`) — see the section doc above for why. It never touches the clock when it fits: the
/// caller releases it once, when NO line of the block overflows. Reports to
/// [`plx_machine::idle`] only on a frame the marquee is actually gliding, so a screen full of short
/// (or resting) titles costs the present gate nothing.
///
/// `glyph` leads the line with Continue-Watching's amber play triangle — the SAME clock and the
/// SAME budget arithmetic as the plain title, just with the icon's width plus its gap subtracted
/// from the window before anything is measured against it, so a played title that overflows
/// marquees exactly like any other. This is ONE implementation with a glyph parameter rather than a
/// fourth title path: before it, the glyph line was a dead end that never fell through to a
/// marquee at all, so a long Continue-Watching title elided instead of animating like every other
/// shelf's focused label.
///
/// `fade` (a block held to its tile, [`TileLabel::within_tile`]) dissolves the gliding run at both
/// edges of its window instead of cutting it there ([`block_text`]).
fn title_marquee(
    p: Painter,
    at: LabelPlace,
    text: *const c_char,
    y: f32,
    glyph: bool,
    w: f32,
    block: Option<&marquee::Block>,
    shadowed: bool,
    fade: bool,
) {
    let (sz, bold) = (theme::size::LABEL, 1);
    let lead = glyph_lead(sz, glyph);
    let budget = (at.w - lead).max(0.0);
    if w <= budget {
        // The text is placed as if the mark were not there (centred under the card, like the line
        // below it); the mark hangs to its left, kept inside the block's window.
        let tx = at.run_x(w).max(at.x + lead);
        if glyph {
            draw_play_mark(p, tx - lead, y, sz, bold);
        }
        if shadowed {
            p.text(text, tx, y + crate::caption_policy::CAPTION_SHADOW_DY, sz, crate::caption_policy::CAPTION_SHADOW, 0, bold);
        }
        p.text(text, tx, y, sz, theme::TEXT_PRIMARY, 0, bold);
        return;
    }
    let Some(block) = block else { return };
    // An overflowing run fills the whole block, so the window IS the block; the glyph, when
    // present, sits fixed at its left edge and only the text window after it scrolls. An
    // overflowing focused title never lets the screen rest (`Marquee::report`) — which is what
    // "marquee while focused" means, and why a fitting title must never reach this branch.
    if glyph {
        draw_play_mark(p, at.x, y, sz, bold);
    }
    let text_x0 = at.x + lead;
    // an Arabic or Hebrew title glides mirrored, from its first words at the window's right edge
    let rtl = !text.is_null() && plx_gfx::text::is_rtl_paragraph(&unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy());
    TITLE_CLOCK.with(|clock| {
        MARQUEE.glide_in(
            clock,
            p,
            &block.key,
            w,
            block.cycle_w,
            Rect::new(text_x0, y - MARQUEE_AIR, budget, UNDER_LINE_H + 2.0 * MARQUEE_AIR),
            rtl,
            |dx| {
                let window = fade.then_some((text_x0, text_x0 + budget));
                if shadowed {
                    block_text(p, text, text_x0 + dx, y + crate::caption_policy::CAPTION_SHADOW_DY, sz,
                        crate::caption_policy::CAPTION_SHADOW, bold, w, window, rtl);
                }
                block_text(p, text, text_x0 + dx, y, sz, theme::TEXT_PRIMARY, bold, w, window, rtl);
            },
        )
    });
}

/// The drawn width of a focused title run (LABEL, bold); `0` for a null pointer.
fn title_w(text: *const c_char, measure: &dyn plx_machine::machine::Measure) -> f32 {
    if text.is_null() {
        0.0
    } else {
        measure.width(unsafe { std::ffi::CStr::from_ptr(text) }, theme::size::LABEL, true)
    }
}

/// The under-tile text budget: the tile, a gap either side, PLUS half of each neighbouring tile's
/// width (item 10) — a focused label may bleed into the gap and up to the midpoint of the next
/// tile without visually colliding with it, which is what makes a real Plex title ("Wallace &
/// Gromit: The Curse of the Were-Rabbit") legible instead of "Wallace & Gr…". Two neighbours
/// contributing half their width each is one extra tile-width overall, hence `+ sty.w` once more
/// on top of the plain tile-plus-gaps budget. [`place_label`] only ever narrows it where the panel
/// ends — widening the budget never lets a block run off the panel, it only lets it reach further
/// into open space before [`title_marquee`] takes over for whatever still does not fit.
///
/// **Unscaled is load-bearing, not tidiness.** `rect` is the focus-popped tile, so a budget taken
/// from it sweeps ~310→332px across a pop — which re-keys `TextView`'s and `elide`'s width-keyed
/// caches on *every frame of the animation*, and both clear wholesale at 512 entries, so one walk
/// of a 24-tile shelf evicts every other screen's wrapped text. It also reflowed the title's line
/// breaks mid-pop. The block's y anchor is already unscaled two lines up, for the same reason.
#[inline]
fn under_budget(sty: &RowStyle) -> f32 {
    2.0 * sty.w + 2.0 * sty.gap
}

/// **Where a focused tile's label block actually lands on the panel** — its left edge and its
/// width, in `p`'s own space. The WIDEST case on purpose — the block an overflowing title fills —
/// so a screen with a constraint on it can grade the block that is DRAWN; every line under the
/// tile occupies a sub-rect of this.
pub fn label_band(p: Painter, rect: Rect, sty: &RowStyle) -> (f32, f32) {
    let at = place_label(p, rect, sty, under_budget(sty), 0.0);
    (at.x, at.w)
}

/// The focused tile's whole label block — title (with its optional Continue-Watching glyph) and
/// caption — placed ONCE by [`place_label_wide`] from the wider of its two runs (plus the uncapped
/// `want`), so both lines share one alignment: centred together, starting together at the card's
/// leading edge, or ending together at its trailing edge when the room to the right is too small.
fn draw_label_block(
    p: Painter,
    rect: Rect,
    sty: &RowStyle,
    label: &TileLabel,
    lines: [Option<LineBox>; 2],
    bottom: f32,
    measure: &dyn plx_machine::machine::Measure,
) {
    // Every run of the block is a label block's to a recording census, whatever its ink.
    #[cfg(any(test, feature = "test-support"))]
    let _label_scope = crate::draw_census::label_scope();
    let within_tile = label.within_tile;
    let full = under_budget(sty);
    let csz = theme::size::CAPTION;
    // Measured once and threaded through: the title's drawn width feeds both how wide the block
    // is asked to be and (unchanged) how title_marquee decides plain vs. looping.
    let title_w_val = label.title.as_ref().map(|t| measure.fit_line_measured(
        &t.to_string_lossy(), f32::INFINITY, theme::size::LABEL, true).1);
    // The play mark leads the FIRST line drawn: the title, or the details line of a block with none.
    let title_mark = label.glyph && label.title.is_some();
    let caption_mark = label.glyph && label.title.is_none() && label.caption.is_some();
    let lead = glyph_lead(theme::size::LABEL, title_mark);
    let caption_lead = glyph_lead(csz, caption_mark);
    // The mark HANGS left of a line that is centred as if it were not there, so the window keeps its
    // room on both sides of the text (`2 * lead`): the line stays on the block's centre.
    let title_run = title_w_val.map_or(0.0, |w| (w + 2.0 * lead).min(full));
    let caption_str = label.caption.as_ref().map(|c| c.to_string_lossy());
    let caption_w = caption_str.as_ref().map_or(0.0, |s| measure.fit_line_measured(s, f32::INFINITY, csz, false).1);
    let caption_run = caption_w + 2.0 * caption_lead;
    // …and the UNCAPPED widest run, which only a block that reaches left of its card may use
    // ([`anchor_label`]).
    let want = title_w_val.map_or(0.0, |w| w + 2.0 * lead).max(caption_run);
    let at = if within_tile {
        tile_window(rect.cx() - sty.w * 0.5, sty.w, want)
    } else {
        place_label_wide(p, rect, sty, title_run.max(caption_run.min(full)), want, label.settle_lag)
    };
    #[cfg(feature = "devtriggers")]
    {
        let placed = p.to_screen(Rect::new(at.x, 0.0, at.w, 1.0)).0;
        crate::card_probe::caption(placed.x, placed.w);
    }

    // Which lines of the block do not fit it? They glide together on ONE clock and ONE cycle (the
    // longest's); when none does, the clock is released so an overflowing block focused again
    // later starts from its rest beat rather than resuming mid-glide.
    let title_over = title_w_val.is_some_and(|w| w > (at.w - lead).max(0.0));
    let caption_over = caption_w > (at.w - caption_lead).max(0.0);
    let title_text = label.title.as_ref().map_or(std::borrow::Cow::Borrowed(""), |t| t.to_string_lossy());
    let block = marquee::Block::of(
        (&title_text, title_over.then_some(title_w_val.unwrap_or(0.0))),
        (caption_str.as_deref().unwrap_or(""), caption_over.then_some(caption_w)),
    );
    if block.is_none() {
        TITLE_CLOCK.with(|c| c.release());
    }
    // Each line is drawn where its box is and at the alpha that box earns from the band
    // ([`TileLabel::lines`], the one place the block's vertical metrics live); a line the band has
    // not made room for yet is not drawn at all.
    let [title_line, caption_line] = lines.map(|line| line.filter(|l| l.alpha > 0.0));
    if let (Some(t), Some(line)) = (&label.title, title_line) {
        title_marquee(p.alpha(line.alpha), at, t.as_ptr(), bottom + line.top, title_mark, title_w_val.unwrap_or(0.0),
            block.as_ref(), label.shadowed, within_tile);
    }
    if let (Some(tc), Some(line)) = (&label.caption, caption_line) {
        let (p, y) = (p.alpha(line.alpha), bottom + line.top);
        // The mark hangs left of the text; a gliding line keeps it fixed at the window's left edge
        // and glides inside the window after it (the title's rule, `title_marquee`).
        if let Some(block) = block.as_ref().filter(|_| caption_over) {
            if caption_mark {
                draw_play_mark(p, at.x, y, csz, 0);
            }
            let x0 = at.x + caption_lead;
            let caption_rtl = plx_gfx::text::is_rtl_paragraph(&tc.to_string_lossy());
            TITLE_CLOCK.with(|clock| {
                MARQUEE.glide_in(
                    clock,
                    p,
                    &block.key,
                    caption_w,
                    block.cycle_w,
                    Rect::new(x0, y - MARQUEE_AIR, (at.w - caption_lead).max(0.0), UNDER_CAPTION_H + 2.0 * MARQUEE_AIR),
                    caption_rtl,
                    |dx| {
                        let window = within_tile.then_some((x0, at.x + at.w));
                        if label.shadowed {
                            block_text(p, tc.as_ptr(), x0 + dx, y + crate::caption_policy::CAPTION_SHADOW_DY, csz,
                                crate::caption_policy::CAPTION_SHADOW, 0, caption_w, window, caption_rtl);
                        }
                        block_text(p, tc.as_ptr(), x0 + dx, y, csz, theme::CAPTION_DETAIL_INK, 0, caption_w, window,
                            caption_rtl);
                    },
                )
            });
        } else {
            let tx = at.run_x(caption_w).max(at.x + caption_lead);
            if caption_mark {
                draw_play_mark(p, tx - caption_lead, y, csz, 0);
            }
            if label.shadowed {
                p.text(tc.as_ptr(), tx, y + crate::caption_policy::CAPTION_SHADOW_DY, csz,
                    crate::caption_policy::CAPTION_SHADOW, 0, 0);
            }
            p.text(tc.as_ptr(), tx, y, csz, theme::CAPTION_DETAIL_INK, 0, 0);
        }
    }
}

/// The line box of a credit label's role line: the caption size plus a hair of air.
pub const CREDIT_ROLE_LEADING: f32 = theme::size::CAPTION as f32 + theme::space::XS;

/// Where a credit label's role line starts below its name line's cap top — the name's cap height
/// and a gap. One number for the label in every state, so the two baselines never move.
pub fn credit_role_dy(measure: &dyn plx_machine::machine::Measure) -> f32 {
    measure.cap_h(theme::size::LABEL) + theme::space::XS
}

/// **A persistent two-line label under a headshot** — the name, then the role / job, drawn for
/// EVERY tile of a shelf, focused or not (a poster shelf draws only the focused tile's block, see
/// [`draw_label_block`]; a cast shelf names everyone). `at` is the label's box: its left edge and
/// width (the text budget) and the cap top of the name line.
///
/// **One shape in every state.** The name is ONE line and the role is ONE line, always, on the same
/// two baselines — a block that wraps the role to two lines unfocused and shows one clipped, gliding
/// line focused changes height and jumps when focus moves. What differs is only what a line that
/// does not fit does:
/// - unfocused: it ends in an ellipsis (`…`), still;
/// - focused: it is never elided — it glides through its budget ([`marquee`]'s title timing, drawn
///   as a run and its follower inside a clip window the budget wide), the name and the role on ONE
///   [`marquee::Block`] (one clock, one cycle) so they leave their rest beat and loop together,
///   exactly like a poster's title and caption. A focused line that fits stays centred and still.
///
/// `clock` is the caller's: one headshot holds focus app-wide, so one clock per shelf; it is
/// released here whenever no focused line overflows, so the next overflowing label starts from its
/// rest beat. A line is centred when plain and starts at `at.x` while gliding.
pub fn draw_credit_label(
    p: Painter,
    clock: &marquee::Clock,
    at: Rect,
    name: &str,
    role: &str,
    focused: bool,
    measure: &dyn plx_machine::machine::Measure,
) {
    use crate::label::{HAlign, Label, VAlign};
    if at.w <= 0.0 {
        return;
    }
    let (nsz, rsz) = (theme::size::LABEL, theme::size::CAPTION);
    let name_w = measure.width_str(name, nsz, focused);
    let role_w = measure.width_str(role, rsz, false);
    let over = |w: f32, present: bool| (focused && present && w > at.w).then_some(w);
    let block = marquee::Block::of(
        (name, over(name_w, !name.is_empty())),
        (role, over(role_w, !role.is_empty())),
    );
    if focused && block.is_none() {
        clock.release();
    }
    let (name_y, role_y) = (at.y, at.y + credit_role_dy(measure));
    // (text, size, bold, colour, cap top, line box height, full run width when it overflows)
    let lines = [
        (name, nsz, focused, if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY },
            name_y, nsz as f32 + theme::space::XS, over(name_w, !name.is_empty())),
        (role, rsz, false, theme::TEXT_TERTIARY, role_y, CREDIT_ROLE_LEADING, over(role_w, !role.is_empty())),
    ];
    for (text, sz, bold, col, y, h, overflow) in lines {
        if text.is_empty() {
            continue;
        }
        let frame = Rect::new(at.x, y, at.w, h);
        let style = |l: Label| if bold { l.bold() } else { l };
        if let (Some(run_w), Some(block)) = (overflow, block.as_ref()) {
            let Ok(c) = std::ffi::CString::new(text) else { continue };
            let label = style(Label::new(c.as_ptr(), sz, col)).h(HAlign::Left).v(VAlign::CapTop);
            marquee::TITLE.glide_in(
                clock,
                p,
                &block.key,
                run_w,
                block.cycle_w,
                Rect::new(frame.x, frame.y - MARQUEE_AIR, frame.w, frame.h + 2.0 * MARQUEE_AIR),
                plx_gfx::text::is_rtl_paragraph(text),
                |dx| {
                    label.draw(p, Rect::new(frame.x + dx, frame.y, frame.w, frame.h));
                },
            );
        } else {
            let shown = plx_gfx::text::elide_by(text, frame.w, false, |t| measure.width_str(t, sz, bold));
            let Ok(c) = std::ffi::CString::new(shown) else { continue };
            style(Label::new(c.as_ptr(), sz, col)).h(HAlign::Center).v(VAlign::CapTop).draw(p, frame);
        }
    }
}

/// Full-bleed resume bar: the bottom band of the card itself (Continue Watching). Delegates to
/// [`crate::widgets::progress_bar`], which is the ONE bar the whole app draws — a poster shelf and
/// the detail page's episode filmstrip now make the identical call, and its doc carries the two
/// pixel-level rules (snapped band, disjoint track/fill) that a hand-rolled copy here kept getting
/// subtly wrong. Only the HEIGHT is local, because it is poster-relative.
pub fn resume_bar(p: Painter, r: Rect, frac: f32, rad: f32) {
    // 5px on a resting poster (and it rides the focus pop): the band's bottom ~1.5px are the card
    // edge's own SDF AA ramp, so a 4px band reads ~2.5px — 5px leaves the mock's ~4px of solid color.
    let bh = (r.h * 5.0 / 375.0).max(4.0);
    crate::widgets::progress_bar(p, r, rad, bh, frac);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    /// A card draw takes its press from the frame: an idle `PressRead` (scale 0.0 before any press
    /// machine read) dips by 1.0, not to nothing.
    #[test]
    fn an_idle_press_read_dips_by_one() {
        assert_eq!(plx_machine::machine::PressRead::<()>::default().dip(), 1.0);
        let moving = plx_machine::machine::PressRead::<()> { scale: 0.92, ..Default::default() };
        assert_eq!(moving.dip(), 0.92);
    }

    /// A bound that leaves a note less than `MIN_HEADING_RUN` drops the note AND its dot: a lone
    /// separator dot trailing the title reads as damage.
    #[test]
    fn a_bound_that_drops_a_note_drops_its_dot_too() {
        struct Fixed;
        impl plx_machine::machine::Measure for Fixed {
            fn width(&self, s: &std::ffi::CStr, sz: i32, _: bool) -> f32 {
                s.to_bytes().len() as f32 * sz as f32 * 0.5
            }
            fn cap_h(&self, sz: i32) -> f32 { sz as f32 }
            fn line_h(&self, sz: i32) -> f32 { sz as f32 }
        }
        let flow = |max_w: f32| {
            let mut texts = Vec::new();
            bounded_heading_flow("T", "12 results", "", max_w, &Fixed, |s, _, sz, _, _| {
                texts.push(s.to_string());
                s.len() as f32 * sz as f32 * 0.5
            });
            texts
        };
        let title_w = theme::size::HEADLINE as f32 * 0.5;
        let dot_x = title_w + HEADING_SOURCE_PAD;
        // the dot has room (>= MIN), the note after it does not
        let tight = flow(dot_x + MIN_HEADING_RUN + 1.0);
        assert_eq!(tight, ["T"], "no dot without its note: {tight:?}");
        // ample room keeps all three
        assert_eq!(flow(f32::INFINITY), ["T", "\u{b7}", "12 results"]);
        assert_eq!(flow(10_000.0).len(), 3);
    }

    /// The runs `draw_heading_c` PAINTS for a title, a count and a source, left to right.
    fn painted_heading(title: &std::ffi::CStr, count: &std::ffi::CStr, source: &std::ffi::CStr) -> Vec<crate::draw_census::Run> {
        let mut runs = crate::draw_census::capture_runs(|| {
            draw_heading_c(Painter::recording(), title, count, source, 100.0, 50.0, 1.0, &crate::widgets::LegacyMeasure);
        });
        runs.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
        runs
    }

    /// Q4, painted: title -> dot -> count stand `HEADING_SOURCE_PAD` apart edge to edge, the count is
    /// the quiet BODY note, and an absent count draws no dot, no gap and no run (Search and Person
    /// both paint through this call).
    #[test]
    fn a_painted_heading_keeps_the_pad_between_title_dot_and_count() {
        let _g = plx_base::testlock::serial();
        let runs = painted_heading(c"Movies", c"12", c"");
        let texts: Vec<_> = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["Movies", "\u{b7}", "12"]);
        assert_eq!(runs[0].at.x, 100.0);
        for pair in runs.windows(2) {
            let gap = pair[1].at.x - (pair[0].at.x + pair[0].at.w);
            assert!((gap - HEADING_SOURCE_PAD).abs() < 0.5, "{} -> {}: {gap}", pair[0].text, pair[1].text);
        }
        assert_eq!((runs[2].size, runs[2].ink), (theme::size::BODY, theme::TEXT_TERTIARY));
        assert_eq!((runs[1].size, runs[1].ink), (theme::size::BODY, theme::TEXT_SEPARATOR));
        // the source rides the same pad behind the count
        let both = painted_heading(c"Movies", c"12", c"friend");
        assert_eq!(both.len(), 5);
        let gap = both[3].at.x - (both[2].at.x + both[2].at.w);
        assert!((gap - HEADING_SOURCE_PAD).abs() < 0.5, "count -> second dot: {gap}");
        // hidden count: the title alone
        let bare = painted_heading(c"Movies", c"", c"");
        assert_eq!(bare.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Movies"]);
    }

    #[test]
    fn discovery_and_text_recording_keep_visible_cards_when_gl_is_suppressed() {
        use crate::frame::backdrop::{self, Sources};
        use std::{cell::RefCell, rc::Rc};
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let _walk = backdrop::discover(sources);
        let card = super::Rect::new(100.0, 200.0, 400.0, 220.0);
        assert!(plx_gfx::gfx::culled(card.x, card.y, card.w, card.h));
        assert!(super::paint_visible(super::Painter::root(), card, 1.0, true));
        assert!(super::paint_visible(super::Painter::recording(), card, 1.0, true));
        let offscreen = super::Rect::new(100.0, -500.0, 400.0, 220.0);
        assert!(!super::paint_visible(super::Painter::root(), offscreen, 1.0, true));
        assert!(super::paint_visible(super::Painter::recording(), offscreen, 1.0, true));
    }

    #[test]
    fn card_paint_culling_keeps_shadow_and_pressed_caption_at_viewport_edges() {
        let _guard = plx_base::testlock::serial();
        let p = super::Painter::root();
        let just_above = super::Rect::new(100.0, -250.0, 400.0, 220.0);
        assert!(super::paint_visible(p, just_above, 1.0, false), "the shadow reaches the panel");
        let label_only = super::Rect::new(100.0, -300.0, 400.0, 220.0);
        assert!(!super::paint_visible(p, label_only, 1.0, false));
        assert!(super::paint_visible(p, label_only, 0.94, true), "a pressed card's label remains visible");
        let far_above = super::Rect::new(100.0, -500.0, 400.0, 220.0);
        assert!(!super::paint_visible(p, far_above, 0.94, true));
        assert!(super::paint_visible(p.translate(0.0, 400.0), far_above, 0.94, true));
        // Past `theme::CARD_SHADOW_BLUR + theme::CARD_SHADOW_DY + 1.0`'s pad (the risen shadow's own
        // worst case), so this stays a genuine off-screen rect rather than one the bigger pad pulls
        // back into view.
        let below = super::Rect::new(100.0, super::SCR_H + 200.0, 400.0, 220.0);
        assert!(!super::paint_visible(p, below, 1.0, true));
    }

    use super::*;

    fn motion_hash(row: &CardRow) -> u64 {
        let mut canon = plx_machine::machine::Canon::new();
        row.write_motion(&mut canon);
        canon.finish()
    }

    #[test]
    fn a_fresh_row_is_at_exact_rest_and_update_is_a_fixed_point() {
        let mut row = CardRow::new();
        assert!(row.at_exact_rest());
        let before = motion_hash(&row);
        row.update(24, None, &RowStyle::HOME, 1.0 / 60.0);
        assert_eq!(motion_hash(&row), before);
        row.update(0, None, &RowStyle::HOME, 1.0 / 60.0);
        assert_eq!(motion_hash(&row), before);
    }

    /// A row that held focus used to stall one ulp from rest (`scale` at 1.0000001, `band` at
    /// denormals) and never reach exact rest; `park` snaps it once every spring is settled.
    #[test]
    fn a_released_row_parks_within_120_frames() {
        let dt = 1.0 / 60.0;
        let mut row = CardRow::new();
        for _ in 0..30 {
            row.update(24, Some(3), &RowStyle::HOME, dt);
        }
        assert!(!row.at_exact_rest());
        assert!(!row.park(), "a focused row never parks");
        let mut parked_at = None;
        for f in 0..120 {
            row.update(24, None, &RowStyle::HOME, dt);
            if row.park() {
                parked_at = Some(f);
                break;
            }
        }
        assert!(parked_at.is_some(), "not parked within 120 frames");
        assert!(row.at_exact_rest());
        let before = motion_hash(&row);
        row.update(24, None, &RowStyle::HOME, dt);
        assert_eq!(motion_hash(&row), before, "update of a parked row is a fixed point");
    }

    /// What the card compositor does with a tile drawn at `scale`: the pop scalar the painter passes
    /// (`draw_tile`), the shadow lift `Painter::tex_carded` derives from it, and whether that sends the
    /// card to the focus program (and out of the resting split) rather than the plain one.
    fn card_path(sty: &RowStyle, scale: f32) -> (f32, bool) {
        let f = pop_factor(scale, sty.focus_scale);
        let (_, _, _, off_l) = crate::card_shadow_params(sty.h, f, theme::CARD_SHADOW[3]);
        let dy = f.clamp(0.0, 1.0) * off_l;
        (f, plx_gfx::gfx::wants_focus_program(f, dy))
    }

    /// **A held key walks the focus along a row, and the row never lets go of focus: it moves on to
    /// the next cell.** `park` refuses a focused row, so every cell the walk leaves behind used to
    /// stall one ulp above 1.0 (`step` never lands a released spring), which is `f` of about 1e-6,
    /// which sends the card to the focus program (gloss, glow band, shadow branch) and out of the
    /// resting split, for as long as the walk lasts. They must come to EXACT rest, the same
    /// `scale == 1.0` / `f == 0.0` a card never visited has, within a bounded number of frames.
    #[test]
    fn a_cell_the_walk_leaves_lands_exactly_on_rest_while_the_row_stays_focused() {
        let dt = 1.0 / 60.0;
        for (name, sty) in [("HOME", RowStyle::HOME), ("EPISODE", RowStyle::EPISODE), ("CAST", RowStyle::CAST)] {
            // an ordinary cell, and one past the spring array (those share the one `overflow` spring)
            for (from, to) in [(3, 4), (MAX_ROW_ITEMS + 5, MAX_ROW_ITEMS + 6), (MAX_ROW_ITEMS + 5, 2)] {
                let mut row = CardRow::new();
                for _ in 0..30 {
                    row.update(80, Some(from), &sty, dt);
                }
                assert!(row.scale(from) > 1.0, "{name}: the cell {from} was popped");
                let mut landed = None;
                for frame in 0..90 {
                    row.update(80, Some(to), &sty, dt);
                    if row.scale(from) == 1.0 {
                        landed = Some(frame);
                        break;
                    }
                }
                assert!(landed.is_some(), "{name}: cell {from} still at {} 90 frames after focus moved to {to}", row.scale(from));
                assert_eq!(card_path(&sty, row.scale(from)), (0.0, false), "{name}: a settled cell draws the plain program");
                // and it stays there: the row is still focused and still stepping every frame
                for _ in 0..30 {
                    row.update(80, Some(to), &sty, dt);
                    assert_eq!(row.scale(from), 1.0, "{name}: cell {from} left rest again");
                }
                // the focused cell is not touched: it is on its way up to the pop (or at it)
                assert!(row.scale(to) > 1.0, "{name}: the new focus cell {to} pops");
            }
        }
    }

    #[test]
    fn park_leaves_a_moving_row_alone() {
        let mut row = CardRow::new();
        row.update(24, Some(3), &RowStyle::HOME, 1.0 / 60.0);
        row.update(24, None, &RowStyle::HOME, 1.0 / 60.0);
        let before = motion_hash(&row);
        assert!(!row.park());
        assert_eq!(motion_hash(&row), before);
    }

    /// A row focused past the spring array pops the shared `overflow` spring; it must park too.
    #[test]
    fn a_row_released_from_an_overflow_cell_parks() {
        let dt = 1.0 / 60.0;
        let mut row = CardRow::new();
        for _ in 0..30 {
            row.update(60, Some(MAX_ROW_ITEMS + 5), &RowStyle::HOME, dt);
        }
        for _ in 0..120 {
            row.update(60, None, &RowStyle::HOME, dt);
            if row.park() {
                break;
            }
        }
        assert!(row.at_exact_rest());
    }

    /// Parking must not change the row's reserved label band relative to a never-focused row.
    #[test]
    fn a_parked_row_reserves_exactly_the_collapsed_band() {
        let dt = 1.0 / 60.0;
        let mut visited = CardRow::new();
        for _ in 0..30 {
            visited.update(24, Some(3), &RowStyle::HOME, dt);
        }
        for _ in 0..120 {
            visited.update(24, None, &RowStyle::HOME, dt);
            if visited.park() {
                break;
            }
        }
        let fresh = CardRow::new();
        assert_eq!(visited.band.pos, fresh.band.pos);
        assert_eq!(visited.lift.pos, fresh.lift.pos);
        assert_eq!(visited.under_band(), fresh.under_band());
        assert_eq!(visited.reveal(TileLabel::title("x"), 0.0).reveal, fresh.reveal(TileLabel::title("x"), 0.0).reveal);
    }

    /// **The pointer's sideways glide moves the row and leaves it there.** A glided row does not
    /// steer back to its focused cell (the cell a pointer rests on as the row slides, or the one a
    /// key last left), until `unfree` (a key's move along it, a click on one of its cards); the
    /// glide never leaves `[0, max]`.
    #[test]
    fn a_glided_row_stays_where_the_glide_put_it_until_it_is_released() {
        let dt = 1.0 / 60.0;
        let sty = &RowStyle::HOME;
        let mut row = CardRow::new();
        for _ in 0..60 { row.update(24, Some(0), sty, dt); }
        assert_eq!(row.scroll_x(), 0.0, "premise: the first cell needs no scroll");
        for _ in 0..30 { row.glide(20.0, 24, sty); }
        assert_eq!(row.scroll_x(), 600.0, "the glide moves the row by what it is handed");
        for _ in 0..120 { row.update(24, Some(0), sty, dt); }
        assert_eq!(row.scroll_x(), 600.0, "a glided row does not steer back to its focused cell");
        row.unfree();
        for _ in 0..240 { row.update(24, Some(0), sty, dt); }
        assert!(row.scroll_x() < 1.0, "released, it brings its focused cell back into view: {}", row.scroll_x());
        row.glide(1.0e6, 24, sty);
        assert_eq!(row.scroll_x(), sty.max_scroll(24), "never past the row's end");
        row.glide(-1.0e6, 24, sty);
        assert_eq!(row.scroll_x(), 0.0, "nor before its start");
    }

    #[test]
    fn restored_scroll_is_retained_and_clamped_to_current_content() {
        let mut row = CardRow::new();
        row.restore_scroll(900.0, 24, &RowStyle::HOME);
        assert_eq!(row.scroll_x(), 900.0);
        assert_eq!(row.scroll_x.vel, 0.0);
        row.restore_scroll(900.0, 2, &RowStyle::HOME);
        assert_eq!(row.scroll_x(), 0.0);
        row.restore_scroll(-50.0, 24, &RowStyle::HOME);
        assert_eq!(row.scroll_x(), 0.0);
    }

    /// Every placement is card-anchored: a block is centred on its card, starts at the card's own
    /// leading edge, or ends at its trailing edge, whatever the scroll, the rail reserve or the
    /// block's width — and never runs past the panel (or the rail) on the right, nor left of the
    /// row's content margin when it reaches left.
    #[test]
    fn focused_label_blocks_are_centred_on_or_anchored_to_their_card() {
        for dx in [0.0, -290.0, -2900.0, 240.0] {
            let p = Painter::root().translate(dx, 0.0);
            for reserve in [0.0, 112.0] {
                let sty = RowStyle::HOME.with_right_reserve(reserve);
                for screen_x in [-120.0, MARGIN_X, 700.0, SCR_W - MARGIN_X - CARD_W, SCR_W - CARD_W] {
                    let rect = Rect::new(screen_x - dx, 300.0, sty.w, sty.h);
                    for w in [120.0, 280.0, under_budget(&sty)] {
                        let at = place_label(p, rect, &sty, w, 0.0);
                        match at.align {
                            LabelAlign::Centre => {
                                assert_eq!(at.x + at.w * 0.5, rect.cx(), "a centred block shares the card's centre");
                                assert_eq!(at.w, w);
                            }
                            LabelAlign::Leading => {
                                assert_eq!(at.x, rect.x, "a leading block starts at the card's leading edge");
                                assert!(at.w <= w);
                                assert!(at.x + dx + at.w <= SCR_W - reserve - EDGE_PAD);
                                assert_eq!(at.run_x(at.w), at.x);
                            }
                            LabelAlign::Trailing => {
                                let edge = (rect.x + sty.w).min(SCR_W - reserve - EDGE_PAD - dx);
                                assert_eq!(at.x + at.w, edge, "a trailing block ends at the card's right edge (or the band's)");
                                assert!(at.w <= w);
                                assert!(at.x + dx >= sty.margin_x - 0.01, "and never starts left of the content margin");
                                assert_eq!(at.run_x(at.w), at.x);
                                assert_eq!(at.run_x(60.0) + 60.0, at.x + at.w, "a short run ends on the block's edge");
                            }
                        }
                    }
                }
            }
        }
    }

    // ---- the pure anchor decision ------------------------------------------------------------

    /// A 1920 panel with the usual air at both edges and the 96px content margin.
    const ROOM: LabelRoom = LabelRoom { lo: EDGE_PAD, hi: SCR_W - EDGE_PAD, floor: MARGIN_X };

    /// A block that fits centred under its card is exactly where it always was: nothing is
    /// clipped, so nothing moves.
    #[test]
    fn a_block_that_fits_centred_is_unchanged() {
        let at = anchor_label(700.0, 950.0, 400.0, 400.0, ROOM);
        assert_eq!(at, LabelPlace { x: 625.0, w: 400.0, align: LabelAlign::Centre });
        // even beside the right margin, when the centred block still sits on the panel
        let at = anchor_label(1500.0, 1750.0, 500.0, 500.0, ROOM);
        assert_eq!(at.align, LabelAlign::Centre);
    }

    /// A block held to its tile ([`TileLabel::within_tile`]) never has a window wider than the
    /// tile: a run that fits is centred like any block, one that does not leads from the tile's
    /// edge inside a tile-wide window.
    #[test]
    fn a_block_held_to_its_tile_is_never_wider_than_the_tile() {
        assert_eq!(tile_window(100.0, 250.0, 100.0), LabelPlace { x: 175.0, w: 100.0, align: LabelAlign::Centre });
        assert_eq!(tile_window(100.0, 250.0, 250.0), LabelPlace { x: 100.0, w: 250.0, align: LabelAlign::Centre });
        assert_eq!(tile_window(100.0, 250.0, 900.0), LabelPlace { x: 100.0, w: 250.0, align: LabelAlign::Leading });
    }

    /// A card with room on its right keeps today's leading-edge window: the whole block shows.
    #[test]
    fn an_overflowing_block_with_room_on_the_right_stays_leading() {
        let at = anchor_label(100.0, 350.0, 600.0, 600.0, ROOM);
        assert_eq!(at, LabelPlace { x: 100.0, w: 600.0, align: LabelAlign::Leading });
    }

    /// At the end of a row the room on the right is a sliver: the window ends on the card's own
    /// right edge and reaches left, and the lines are right-aligned to that edge.
    #[test]
    fn an_overflowing_block_at_the_right_margin_anchors_to_the_cards_right_edge() {
        let at = anchor_label(1574.0, 1824.0, 700.0, 700.0, ROOM);
        assert_eq!(at, LabelPlace { x: 1124.0, w: 700.0, align: LabelAlign::Trailing });
        assert_eq!(at.run_x(300.0), 1524.0, "a shorter line ends on the same edge");
        assert_eq!(at.run_x(700.0), 1124.0);
    }

    /// A block that reaches left of its card may be wider than the shared reach (`w`, clamped to
    /// the budget): it takes what it wants of the room, so a title that would have glided shows
    /// whole. A centred or leading block never does.
    #[test]
    fn a_trailing_block_takes_the_width_it_wants_and_the_others_do_not() {
        let at = anchor_label(1574.0, 1824.0, 560.0, 720.0, ROOM);
        assert_eq!(at, LabelPlace { x: 1104.0, w: 720.0, align: LabelAlign::Trailing });
        assert_eq!(anchor_label(700.0, 950.0, 400.0, 900.0, ROOM).w, 400.0);
        assert_eq!(anchor_label(100.0, 350.0, 600.0, 900.0, ROOM).w, 600.0);
        // the card's edge inside a reserved band: the block ends where the band starts
        let railed = LabelRoom { hi: 1700.0, ..ROOM };
        let at = anchor_label(1574.0, 1824.0, 560.0, 560.0, railed);
        assert_eq!((at.x + at.w, at.align), (1700.0, LabelAlign::Trailing));
    }

    /// Too long even for the room to the left: the window is clamped to the content margin and
    /// the run glides inside it (the caller's marquee), never past either bound.
    #[test]
    fn a_block_too_long_both_ways_clamps_to_the_content_margin() {
        let at = anchor_label(1574.0, 1824.0, 2400.0, 2400.0, ROOM);
        assert_eq!(at, LabelPlace { x: MARGIN_X, w: 1824.0 - MARGIN_X, align: LabelAlign::Trailing });
        // …and a card in the middle of a narrow panel whose left room is the larger one
        let narrow = LabelRoom { lo: 16.0, hi: 700.0, floor: 96.0 };
        let at = anchor_label(400.0, 650.0, 900.0, 900.0, narrow);
        assert_eq!(at, LabelPlace { x: 96.0, w: 554.0, align: LabelAlign::Trailing });
        // the other way round the window is the larger room to the right, and glides in it
        let at = anchor_label(100.0, 350.0, 2400.0, 2400.0, ROOM);
        assert_eq!(at, LabelPlace { x: 100.0, w: 1804.0, align: LabelAlign::Leading });
    }

    /// Title and caption are one unit: at the right edge both end at the card's right edge, so a
    /// short caption under a long title ends where the title does.
    #[test]
    fn title_and_caption_share_the_trailing_edge_at_the_end_of_a_row() {
        let sty = RowStyle::HOME;
        let rect = Rect::new(SCR_W - MARGIN_X - sty.w, 300.0, sty.w, sty.h);
        let at = place_label(Painter::root(), rect, &sty, under_budget(&sty), 0.0);
        assert_eq!(at.align, LabelAlign::Trailing);
        assert_eq!(at.run_x(60.0) + 60.0, rect.x + sty.w);
        assert_eq!(at.run_x(at.w) + at.w, rect.x + sty.w);
        let mid = Rect::new(700.0, 300.0, sty.w, sty.h);
        let centred = place_label(Painter::root(), mid, &sty, 200.0, 0.0);
        assert_eq!(centred.align, LabelAlign::Centre, "a block with room stays centred on its card");
        assert_eq!(centred.run_x(60.0) + 30.0, mid.cx());
    }

    /// The same card at the same place, wherever the shelf has scrolled: the decision is made in
    /// screen space, so a shelf scrolled by thousands of pixels anchors its last card the same way.
    #[test]
    fn the_anchor_is_judged_in_screen_space() {
        let sty = RowStyle::HOME;
        for dx in [0.0, -2900.0] {
            let p = Painter::root().translate(dx, 0.0);
            let rect = Rect::new(SCR_W - MARGIN_X - sty.w - dx, 300.0, sty.w, sty.h);
            let (x, width) = label_band(p, rect, &sty);
            assert_eq!(x + width, rect.x + sty.w, "the widest label ends on its card's right edge");
            assert!(width > sty.w, "and reaches left over the room the neighbours leave");
            assert!(x + dx >= sty.margin_x - 0.01, "never past the content margin");
        }
    }

    /// The text runs of one focused tile at the end of a shelf: the x of every run on its title
    /// line and on its caption line.
    fn end_of_row_runs(title: &str, caption: &str) -> (Vec<f32>, Vec<f32>, Rect) {
        let sty = RowStyle::HOME;
        let rect = Rect::new(SCR_W - MARGIN_X - sty.w, 300.0, sty.w, sty.h);
        let ty = rect.y + rect.h + UNDER_DROP;
        let label = TileLabel::titled(title, caption);
        let log = crate::draw_census::capture(|| {
            draw_focused(Painter::recording(), Art::Poster(None), rect, 1.0, &sty, None, &label,
                &crate::fixture::FixtureMeasure);
        });
        let (mut t, mut c) = (Vec::new(), Vec::new());
        for (_, r) in log.into_iter().filter(|(tag, r)| *tag == 100 && r.y >= ty - 8.0) {
            if r.y < ty + UNDER_LINE_H { t.push(r.x) } else { c.push(r.x) }
        }
        (t, c, rect)
    }

    /// Drawn: the last card of a shelf with a title wider than its card and than the room on its
    /// right but narrower than the room on its left — the title and caption each draw ONE still
    /// run, ending on the card's right edge and inside the content bounds.
    #[test]
    fn a_right_anchored_caption_draws_inside_the_content_bounds_and_ends_at_the_cards_edge() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let title = "Wallace & Gromit: Vengeance Most Fowl and the Curse of the Were-Rabbit";
        let caption = "A caption wider than the card";
        let (t, c, rect) = end_of_row_runs(title, caption);
        let edge = rect.x + RowStyle::HOME.w;
        let title_w = measure_w(title, theme::size::LABEL);
        let caption_w = measure_w(caption, theme::size::CAPTION);
        assert!(title_w > under_budget(&RowStyle::HOME), "the title is wider than the shared reach");
        assert!(title_w > SCR_W - EDGE_PAD - rect.x, "the title overflows the room on the card's right");
        assert!(title_w < edge - MARGIN_X, "and fits the room on its left");
        assert_eq!((t.len(), c.len()), (1, 1), "both lines fit the window: one still run each ({t:?} {c:?})");
        assert!((t[0] + title_w - edge).abs() < 0.5, "the title ends on the card's right edge: {t:?}");
        assert!((c[0] + caption_w - edge).abs() < 0.5, "and so does the caption: {c:?}");
        assert!(t[0] >= MARGIN_X && c[0] >= MARGIN_X, "inside the content margin: {t:?} {c:?}");
        assert!(edge <= SCR_W - MARGIN_X, "and the right margin");
    }

    /// And a title too long even for the room to the left glides inside the clamped window — a run
    /// and its follower, the glide starting at the window's left edge (the content margin), never
    /// at the card — while a short caption under it still ends on the card's edge.
    #[test]
    fn a_title_too_long_for_both_sides_glides_inside_the_clamped_window() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let (t, c, rect) = end_of_row_runs(LONG_TITLE, "1979");
        assert_eq!(t.len(), 2, "a run and its follower: {t:?}");
        assert!((leftmost(&t) - MARGIN_X).abs() < 0.5, "the glide starts at the content margin: {t:?}");
        assert_eq!(c.len(), 1);
        assert!((c[0] + measure_w("1979", theme::size::CAPTION) - (rect.x + RowStyle::HOME.w)).abs() < 0.5,
            "the short caption still ends on the card's edge: {c:?}");
    }

    #[test]
    fn an_oversized_label_block_leads_from_its_card_and_stops_at_the_rail() {
        let sty = RowStyle { w: SCR_W, ..RowStyle::HOME.with_right_reserve(112.0) };
        let (x, width) = label_band(Painter::root(), Rect::new(0.0, 0.0, sty.w, sty.h), &sty);
        assert_eq!(x, 0.0);
        assert_eq!(width, SCR_W - sty.right_reserve - EDGE_PAD);
    }

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn a_label_band_reveal_defers_art_and_then_settles() {
        use crate::card_motion::{History, Identity, Verdict};
        let mut row = CardRow::new();
        let mut history = History::default();
        let mut saw_fast = false;
        let mut last = Verdict::Unknown;
        for frame in 0..600 {
            let before = row.under_band();
            row.update(3, Some(0), &RowStyle::HOME, DT);
            assert_eq!(row.scroll_x(), 0.0);
            history.begin();
            last = history.observe(Identity { owner: 1, asset: 1 },
                Rect::new(0.0, ROW_PITCH_FIXED + row.under_band(), 250.0, 375.0), frame * 16);
            if frame > 0 && (row.under_band() - before).abs() / DT > 120.0 {
                assert_eq!(last, Verdict::Moving, "band-only displacement classifies as Moving");
                saw_fast = true;
            }
        }
        assert!(saw_fast, "the fixture must exercise a fast band reveal");
        assert_eq!(last, Verdict::Settled);
    }

    /// The full clearance a HOME shelf's heading needs over a popped tile — asked of
    /// [`heading_lift_max`] rather than restating its three terms, because a screen now RESERVES
    /// this same number in its layout (the home grid's `GRID_TOP_Y` clearance) and a test that re-derived it
    /// would keep passing while the two drifted.
    fn full() -> f32 {
        heading_lift_max(&RowStyle::HOME)
    }

    // The lattice every shelf in the app is drawn on: 250-wide tiles, 96px left margin.
    const ADV: f32 = CARD_W + 40.0;

    /// **Vertical focus follows the SCREEN, not the array index.** Two shelves hold independent
    /// horizontal scrolls, so carrying the index across lands focus on a tile that can be most of a
    /// screen away — and the destination row then scrolls hard to reveal it.
    #[test]
    fn a_vertical_step_lands_under_the_tile_you_were_looking_at() {
        // the source row is scrolled four tiles along and focus is on its tile 6, which is
        // therefore drawn at column 2 on screen
        let src_x = tile_centre_x(6, MARGIN_X, ADV, CARD_W, 4.0 * ADV);
        assert_eq!(src_x, tile_centre_x(2, MARGIN_X, ADV, CARD_W, 0.0));

        // an UNSCROLLED destination row: the tile under the cursor is its tile 2, not its tile 6
        assert_eq!(column_near_x(src_x, MARGIN_X, ADV, CARD_W, 0.0, 20, 6), 2);
        // …and a destination scrolled the SAME amount keeps the index, which is the case the old
        // index-carrying rule got right and is why it survived so long
        assert_eq!(column_near_x(src_x, MARGIN_X, ADV, CARD_W, 4.0 * ADV, 20, 6), 6);

        // a destination scrolled FURTHER than the source: the visual column is preserved again
        assert_eq!(column_near_x(src_x, MARGIN_X, ADV, CARD_W, 9.0 * ADV, 20, 6), 11);
    }

    /// UP → DOWN → UP must land back where it started, or the page walks sideways under a user
    /// doing nothing but bouncing between two rows.
    #[test]
    fn bouncing_between_two_rows_is_stable() {
        // a half-tile offset between the rows, the worst case for a rounding rule
        let (a, b) = (3.0 * ADV, 3.0 * ADV + ADV * 0.5);
        let mut i = 7usize;
        let first =
            column_near_x(tile_centre_x(i, MARGIN_X, ADV, CARD_W, a), MARGIN_X, ADV, CARD_W, b, 20, i);
        let mut seen = vec![i, first];
        for step in 0..6 {
            let (from_s, to_s) = if step % 2 == 0 { (b, a) } else { (a, b) };
            let cur = *seen.last().unwrap();
            let x = tile_centre_x(cur, MARGIN_X, ADV, CARD_W, from_s);
            i = column_near_x(x, MARGIN_X, ADV, CARD_W, to_s, 20, cur);
            seen.push(i);
        }
        // it settles into a two-cycle rather than drifting: every even entry is the same index and
        // so is every odd one
        assert!(seen.iter().step_by(2).all(|v| *v == seen[0]), "drifted: {seen:?}");
        assert!(seen[1..].iter().step_by(2).all(|v| *v == seen[1]), "drifted: {seen:?}");
    }

    /// A SHORTER destination row clamps to its last tile — the one case where the destination
    /// genuinely has nothing under the cursor and must scroll.
    #[test]
    fn a_shorter_row_clamps_to_its_last_tile() {
        let x = tile_centre_x(9, MARGIN_X, ADV, CARD_W, 0.0);
        assert_eq!(column_near_x(x, MARGIN_X, ADV, CARD_W, 0.0, 4, 9), 3);
        assert_eq!(column_near_x(x, MARGIN_X, ADV, CARD_W, 0.0, 1, 9), 0);
        // an EMPTY row answers 0 rather than panicking on `n - 1`
        assert_eq!(column_near_x(x, MARGIN_X, ADV, CARD_W, 0.0, 0, 9), 0);
    }

    /// A source tile drawn LEFT of the destination row's origin rounds to 0 rather than wrapping.
    /// `floor` after the +0.5 is what makes that true: a cast truncates toward zero, so -0.8 + 0.5
    /// became 0 by accident on one side of the axis and would have become -1 on the other.
    #[test]
    fn a_tile_left_of_the_origin_lands_on_the_first() {
        let x = tile_centre_x(0, MARGIN_X, ADV, CARD_W, 2.0 * ADV); // off the left edge
        assert_eq!(column_near_x(x, MARGIN_X, ADV, CARD_W, 0.0, 20, 0), 0);
    }

    fn run(row: &mut CardRow, frames: usize, focused: Option<usize>, sty: &RowStyle) {
        for _ in 0..frames {
            row.update(8, focused, sty, DT);
        }
    }

    /// Regression, two defects in one: the heading clearance used to be `live_scale - 1`, i.e. a
    /// readout of the focus-pop spring. So (a) the frame focus moved, the incoming cell's spring
    /// was still at rest and the heading dropped its ENTIRE lift in one frame before springing
    /// back, and (b) between those it rode the pop up and down. It is a held destination now:
    /// walking slots 0 → 1 must leave the heading exactly where it is, all the way through.
    #[test]
    fn the_heading_holds_still_while_focus_walks_the_slots_beneath_it() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        let held = row.lift();
        assert!(
            (held - full()).abs() < 0.1,
            "slot 0 must hold the full clearance ({held} vs {})",
            full()
        );

        // slot 1 is also under the heading, so nothing should move — not by a pixel, not for a frame
        for f in 0..180 {
            row.update(8, Some(1), &sty, DT);
            assert!(
                (row.lift() - held).abs() < 0.1,
                "heading moved {:.2}px at frame {f} while focus stayed under it",
                row.lift() - held
            );
        }
    }

    /// And when the popped tile really does leave, the heading settles back — once, smoothly, with
    /// no frame moving it more than a spring step (the defect moved ~16px in a single frame).
    #[test]
    fn the_heading_settles_back_smoothly_once_nothing_is_under_it() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        let mut prev = row.lift();
        for _ in 0..240 {
            row.update(8, Some(5), &sty, DT); // far down the row — no clearance needed
            let l = row.lift();
            assert!(
                (l - prev).abs() < 3.0,
                "heading jumped {:.1}px in one frame",
                l - prev
            );
            prev = l;
        }
        assert!(
            prev < 0.5,
            "the heading must come all the way back down (got {prev})"
        );
    }

    /// A row that owns no focus rests flat — including a row that just LOST focus, which used to
    /// snap its heading back the same way.
    #[test]
    fn an_unfocused_row_lifts_its_heading_by_nothing() {
        let sty = RowStyle::HOME;
        let mut row = CardRow::new();
        run(&mut row, 180, Some(0), &sty);
        run(&mut row, 240, None, &sty);
        assert!(row.lift() < 0.5);
    }

    /// The clearance tapers with distance from the heading: slots 0 and 1 sit under it and get the
    /// whole thing, slot 2 is on the ramp, and a tile far down the row leaves it alone.
    #[test]
    fn the_clearance_tapers_as_the_popped_tile_moves_away_from_the_heading() {
        let sty = RowStyle::HOME;
        let at = |c: usize| heading_clearance(Some(c), &sty, 0.0);
        assert_eq!(at(0), at(1), "the first two slots share the full clearance");
        assert!((at(0) - full()).abs() < 0.001);
        assert!(
            at(2) > 0.5 && at(2) < at(1),
            "slot 2 is on the taper ({} vs {})",
            at(2),
            at(1)
        );
        assert_eq!(at(4), 0.0, "a tile far from the heading must not move it");
        assert_eq!(heading_clearance(None, &sty, 0.0), 0.0);
    }

    /// A row LONGER than the spring array. Every focus treatment a tile gets — the pop, the lifted
    /// shadow, the ring — is derived from `scale(i)`, and the index used to be clamped into the
    /// array, so focusing tile 30 of a 60-name cast row popped nothing at all (it read cell 23's
    /// resting spring). The shared overflow spring must pop for the focused tile, and for it alone:
    /// its neighbours read the same spring.
    #[test]
    fn a_focused_tile_past_the_spring_array_still_pops_and_its_neighbours_do_not() {
        let sty = RowStyle::CAST;
        let mut row = CardRow::new();
        let far = MAX_ROW_ITEMS + 6; // a crew credit at the end of a long cast list
        for _ in 0..180 {
            row.update(far + 2, Some(far), &sty, DT);
        }
        assert!(
            (row.scale(far) - sty.focus_scale).abs() < 0.01,
            "the focused overflow tile must reach the focus scale (got {})",
            row.scale(far)
        );
        assert_eq!(
            row.scale(far + 1),
            1.0,
            "the tile after it shares the spring, not the focus"
        );
        assert_eq!(row.scale(far - 1), 1.0, "nor the one before it");
        assert!(
            (row.scale(MAX_ROW_ITEMS - 1) - 1.0).abs() < 0.01,
            "and the last in-array cell rests"
        );

        // focus coming back inside the array releases the overflow pop
        for _ in 0..180 {
            row.update(far + 2, Some(0), &sty, DT);
        }
        assert_eq!(
            row.scale(far),
            1.0,
            "focus left the tail, so no overflow tile reads the spring"
        );
        assert!(
            (row.scale(0) - sty.focus_scale).abs() < 0.01,
            "and the in-array cell pops as ever"
        );
    }

    // ---- item 10: the focused title marquee -------------------------------------------------

    /// A run that already fits the budget never moves and never reports — the case that must cost
    /// the idle present gate nothing.
    #[test]
    fn a_title_that_fits_the_budget_never_moves() {
        assert_eq!(MARQUEE.x(0.0, 100.0, 300.0), 0.0);
        assert_eq!(MARQUEE.x(50_000.0, 100.0, 300.0), 0.0);
        assert!(!MARQUEE.moving(0.0, 100.0, 300.0));
        assert!(!MARQUEE.moving(50_000.0, 100.0, 300.0));
    }

    /// An over-wide run sits at rest for the whole hold beat: `t_ms < MARQUEE.hold_ms` must read
    /// zero offset and report no motion, so a title lands and holds still long enough to start
    /// reading before anything moves.
    #[test]
    fn an_overflowing_title_rests_before_it_glides() {
        let (w, budget) = (500.0, 300.0);
        assert_eq!(MARQUEE.x(0.0, w, budget), 0.0);
        assert_eq!(MARQUEE.x(MARQUEE.hold_ms - 1.0, w, budget), 0.0);
        assert!(!MARQUEE.moving(0.0, w, budget));
        assert!(!MARQUEE.moving(MARQUEE.hold_ms - 1.0, w, budget));
    }

    /// Past the hold beat it glides at the documented speed and reports motion — and the offset is
    /// monotone through the glide (no snap-back mid-cycle).
    #[test]
    fn an_overflowing_title_glides_at_the_documented_speed_and_reports_motion() {
        let (w, budget) = (500.0, 300.0);
        assert!(MARQUEE.moving(MARQUEE.hold_ms + 1.0, w, budget));
        let a = MARQUEE.x(MARQUEE.hold_ms + 100.0, w, budget);
        let b = MARQUEE.x(MARQUEE.hold_ms + 600.0, w, budget);
        assert!(b > a, "the run must keep moving left through the glide");
        // 500ms of glide at 40px/s = 20px
        assert!((b - a - 20.0).abs() < 0.01, "got {} vs expected 20", b - a);
    }

    /// The cycle LOOPS: a full period (hold + the whole glide) must land back at the rest offset,
    /// exactly like the first cycle did — a marquee that drifted cycle to cycle would slowly
    /// desync its two drawn copies from the clip window.
    #[test]
    fn the_marquee_loops_cleanly() {
        let (w, budget) = (500.0, 300.0);
        let travel = w + marquee::GAP;
        let glide_ms = travel / marquee::SPEED * 1000.0;
        let period = MARQUEE.hold_ms + glide_ms;
        assert_eq!(MARQUEE.x(period, w, budget), MARQUEE.x(0.0, w, budget));
        assert_eq!(
            MARQUEE.x(period + 50.0, w, budget),
            MARQUEE.x(50.0, w, budget)
        );
        // just before the wrap the run has travelled (almost) the full distance
        let just_before = MARQUEE.x(period - 0.001, w, budget);
        assert!(
            (just_before - travel).abs() < 0.1,
            "got {just_before} vs travel {travel}"
        );
    }

    /// The title's [`marquee::Clock`] restarts at zero the instant the focused text changes, and keeps
    /// advancing by real elapsed time (read off [`plx_machine::idle::now_ms`]) while it stays the
    /// same. `frame_begin` stands in for the real frame loop's own per-frame call, advancing the
    /// same clock `title_marquee` reads in production — nothing about `marquee_clock` itself is
    /// untestable now that it reads an absolute snapshot instead of summing a `dt` of its own.
    #[test]
    fn the_marquee_clock_restarts_when_the_focused_text_changes() {
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(0.0);
        assert_eq!(TITLE_CLOCK.with(|c| c.read("Alpha")), 0.0, "first sight of a title starts at 0");
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let t1 = TITLE_CLOCK.with(|c| c.read("Alpha"));
        assert!(t1 > 0.0, "the clock must advance while the title holds focus");
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let t2 = TITLE_CLOCK.with(|c| c.read("Alpha"));
        assert!(t2 > t1, "and keep advancing frame over frame");
        assert_eq!(
            TITLE_CLOCK.with(|c| c.read("Beta")),
            0.0,
            "a different focused title restarts the clock at 0"
        );
    }

    // ---- the glyph variant shares the marquee's clock and budget arithmetic -----------------

    /// A plain title gives up nothing to a glyph it does not have.
    #[test]
    fn a_plain_title_has_no_glyph_lead() {
        assert_eq!(glyph_lead(theme::size::LABEL, false), 0.0);
    }

    /// A glyph-led title's window is narrower by exactly the icon's size plus its gap — the same
    /// number [`title_marquee`]'s fitting and overflow branches both
    /// read, pinned once here instead of through a draw.
    #[test]
    fn a_glyph_led_title_gives_up_the_icon_plus_its_gap() {
        let sz = theme::size::LABEL;
        let lead = glyph_lead(sz, true);
        assert!(lead > 0.0, "a glyph must cost the window something");
        assert_eq!(lead, play_icon_size(sz) + PLAY_ICON_GAP);
    }

    /// The whole point of item 10 §b: a title that fits the PLAIN budget can still overflow the
    /// GLYPH-reduced one, so a Continue-Watching name that is merely long enough to have needed
    /// eliding before now marquees instead — the defect the owner reported ("the text does not
    /// animate" on a long Continue Watching title).
    #[test]
    fn a_title_that_fits_the_plain_budget_can_overflow_the_glyph_budget() {
        let sz = theme::size::LABEL;
        let lead = glyph_lead(sz, true);
        let full_budget = 300.0;
        let glyph_budget = full_budget - lead;
        let w = full_budget - lead * 0.5; // fits the plain window, not the glyph-reduced one
        assert!(w <= full_budget, "sanity: fits the plain budget");
        assert!(
            w > glyph_budget,
            "sanity: must overflow the glyph-reduced budget for this test to mean anything"
        );
        assert!(!MARQUEE.moving(0.0, w, full_budget), "plain: still resting, not overflowing");
        assert!(
            MARQUEE.moving(MARQUEE.hold_ms + 1.0, w, glyph_budget),
            "glyph: the same run overflows the narrower window and must glide"
        );
    }

    // ---- word-wrap is gone: the focused title is always exactly one line ---------------------

    /// [`TileLabel::height`] no longer varies with the row style at all — a shelf of long titles
    /// marquees in place rather than growing its label band, so every row in the app reserves the
    /// same fixed height for the same `caption` choice.
    #[test]
    fn the_label_height_is_fixed_regardless_of_row_style() {
        assert_eq!(TileLabel::height(true), UNDER_LABEL_H);
        // derived from the stack, and still the 92 px band detail.rs and every row pitch were built on
        assert_eq!(UNDER_LABEL_H, 92.0);
        assert!(TileLabel::height(false) < TileLabel::height(true));
    }

    /// The two lines are one stack: the details line stands [`UNDER_LINE_GAP`] under the title line,
    /// an absent caption draws no line, and the block's height is the stack's.
    #[test]
    fn the_label_lines_are_one_stack_with_the_reserved_height() {
        let both = TileLabel::titled("Title", "Caption");
        let [title, details] = both.lines();
        let (title, details) = (title.unwrap(), details.unwrap());
        assert_eq!(title.top, UNDER_DROP);
        assert_eq!(title.bottom, UNDER_DROP + UNDER_LINE_H);
        assert_eq!(details.top - title.bottom, UNDER_LINE_GAP);
        assert_eq!(details.bottom - details.top, UNDER_CAPTION_H);
        assert_eq!(details.bottom, TileLabel::height(true));
        let bare = TileLabel::title("Title");
        let [title, details] = bare.lines();
        assert!(details.is_none());
        assert_eq!(title.unwrap().bottom, TileLabel::height(false));
    }

    /// [`TileLabel::own_height`] agrees with the free function for a label that carries whatever
    /// it carries.
    #[test]
    fn a_label_s_own_height_matches_its_caption() {
        let with_caption = TileLabel::titled("Title", "Caption");
        let without = TileLabel::title("Title");
        assert_eq!(with_caption.own_height(), TileLabel::height(true));
        assert_eq!(without.own_height(), TileLabel::height(false));
    }

    // ---- The collapsing label band -------------------------------------------------------------

    /// The two endpoints and the rung the collapsed one is derived from. `under_band(1)` has to be
    /// `UNDER_LABEL_H` exactly or a focused shelf is not the geometry every clearance test in the
    /// app was written against; `under_band(0)` has to leave the design system's REGION gap between
    /// a poster's bottom edge and the next section's heading, which is the claim
    /// [`LABEL_BAND_COLLAPSED`] makes and the only reason it is a subtraction.
    #[test]
    fn a_collapsed_shelf_still_leaves_the_region_gap_before_the_next_heading() {
        assert_eq!(under_band(1.0), UNDER_LABEL_H);
        assert_eq!(under_band(0.0), LABEL_BAND_COLLAPSED);
        // clamped, so a spring's overshoot (there is none here — it is critically damped — or a
        // caller's stray value) can never make a shelf taller than its focused self
        assert_eq!(under_band(1.4), UNDER_LABEL_H);
        assert_eq!(under_band(-0.2), LABEL_BAND_COLLAPSED);

        // a collapsed shelf: card bottom → the next section's heading cap top
        use crate::consts::{CARD_DY, CARD_H, TITLE_DY, UNDER_LABEL_AIR};
        let pitch = ROW_PITCH_FIXED + under_band(0.0);
        let card_bottom = CARD_DY + CARD_H;
        let next_heading = pitch - TITLE_DY;
        assert_eq!(
            next_heading - card_bottom,
            crate::theme::space::LG,
            "an unfocused shelf owes its neighbour one region gap and nothing more"
        );
        // …and that gap is exactly what the block's own trailing air grew into
        assert_eq!(
            LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR,
            crate::theme::space::LG
        );
    }

    /// The document is 74px shorter per unfocused shelf, and [`settled_top`] is the closed form of
    /// the running sum `Grid::layout` and `library::Layout` accumulate. One shelf open, always —
    /// so the whole column shifts by exactly [`BAND_OPEN`] at the shelves BELOW the focus and by
    /// nothing at all at the ones above it.
    #[test]
    fn only_the_shelves_below_the_focus_move_when_focus_walks_the_column() {
        let fixed = ROW_PITCH_FIXED;
        let collapsed = fixed + LABEL_BAND_COLLAPSED;
        assert_eq!(BAND_OPEN, UNDER_LABEL_H - LABEL_BAND_COLLAPSED);

        // focus on shelf 1 of 4
        for r in 0..=1 {
            assert_eq!(settled_top(r, Some(1), fixed), r as f32 * collapsed);
        }
        for r in 2..=4 {
            assert_eq!(
                settled_top(r, Some(1), fixed),
                r as f32 * collapsed + BAND_OPEN
            );
        }
        // walking 1 → 2 moves shelf 2 by nothing and shelf 3 by nothing: the open band simply
        // changes owner, and only the shelf BETWEEN the two ends moves
        assert_eq!(settled_top(2, Some(2), fixed), 2.0 * collapsed);
        assert_eq!(
            settled_top(3, Some(1), fixed),
            settled_top(3, Some(2), fixed)
        );
        // and with nothing focused at all the column is uniformly collapsed
        assert_eq!(settled_top(3, None, fixed), 3.0 * collapsed);
        // the old geometry is the degenerate case: every shelf open is `ROW_PITCH` apart
        assert_eq!(fixed + under_band(1.0), crate::consts::ROW_PITCH);
    }

    /// **The animator obligation** (`ui/CLAUDE.md`): the band must report to the frame gate while it
    /// is travelling and stop reporting once it has arrived. An over-reporting one costs the whole
    /// present-gate saving with every `fps_floor` in the suite still green, so the REST half is the
    /// one that is only ever caught here.
    #[test]
    fn the_label_band_reports_while_it_opens_and_goes_quiet_once_it_has() {
        let _g = plx_base::testlock::serial();
        let mut row = CardRow::new();
        let sty = RowStyle::HOME;

        // opening: the very first step has to be seen by the gate
        let (_, moved) = plx_machine::idle::scoped_motion(|| {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        });
        assert!(moved, "an opening band must wake the present gate");
        assert!(row.band_expand() > 0.0);

        // …and it arrives
        for _ in 0..240 {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        }
        assert!((row.under_band() - UNDER_LABEL_H).abs() < 0.5);
        let (_, still) = plx_machine::idle::scoped_motion(|| {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        });
        assert!(!still, "a settled open band must not keep the panel awake");

        // and the same on the way back down
        for _ in 0..240 {
            row.update(6, None, &sty, 1.0 / 60.0);
        }
        assert!((row.under_band() - LABEL_BAND_COLLAPSED).abs() < 0.5);
        let (_, still) = plx_machine::idle::scoped_motion(|| {
            row.update(6, None, &sty, 1.0 / 60.0);
        });
        assert!(!still, "a settled collapsed band must not keep the panel awake");
    }
    /// The focused label's alignment and width are decided ONCE per focus move, not re-decided on
    /// every frame of the scroll glide. Moving right onto a tile that needs the row to scroll, the
    /// tile starts off to the right and glides left; judged against its LIVE screen position the
    /// block flipped between "centred" and "leading edge" (and re-sized, re-deciding which lines
    /// overflow and flipping the title into its marquee) part-way through, which read as the label jumping.
    #[test]
    fn a_label_block_keeps_its_placement_through_the_scroll_glide() {
        for sty in [RowStyle::HOME, RowStyle::EPISODE] {
            let n = 14;
            let pitch = sty.w + sty.gap;
            for from in 0..n - 1 {
                let mut row = CardRow::new();
                for _ in 0..600 {
                    row.update(n, Some(from), &sty, 1.0 / 60.0);
                }
                let to = from + 1;
                let rect = Rect::new(sty.margin_x + to as f32 * pitch, 300.0, sty.w, sty.h);
                let mut seen = Vec::new();
                for _ in 0..120 {
                    row.update(n, Some(to), &sty, 1.0 / 60.0);
                    let p = Painter::root().translate(-row.scroll_x(), 0.0);
                    let at = place_label(p, rect, &sty, under_budget(&sty), row.settle_lag(n, to, &sty));
                    seen.push((at.align, at.w));
                }
                let settled = *seen.last().unwrap();
                assert!(
                    seen.iter().all(|s| *s == settled),
                    "{}x{} → col {to}: placement changed mid-glide: {seen:?}",
                    sty.w, sty.h,
                );
            }
        }
    }

    // ---- the focused caption marquees with its title ----------------------------------------

    const LONG_CAPTION: &str = "Dr. Alexandra Wolkowicz-Harrington Smythe, chief consulting physician to the \
        royal household";
    const LONG_TITLE: &str = "The Extraordinarily Long and Winding Adventures of Alexandra Wolkowicz-\
        Harrington Smythe and Her Many Consulting Physicians Across the Whole Wide Royal Household";

    /// Draw one focused tile in the middle of the panel and return the x of every text run on its
    /// title line and on its caption line (a marquee is a run plus its follower).
    fn label_runs(title: &str, caption: &str) -> (Vec<f32>, Vec<f32>) {
        let sty = RowStyle::HOME;
        let rect = Rect::new(700.0, 300.0, sty.w, sty.h);
        let ty = rect.y + rect.h + UNDER_DROP;
        let label = TileLabel::titled(title, caption);
        let log = crate::draw_census::capture(|| {
            draw_focused(Painter::recording(), Art::Poster(None), rect, 1.0, &sty, None, &label,
                &crate::fixture::FixtureMeasure);
        });
        let (mut t, mut c) = (Vec::new(), Vec::new());
        for (_, r) in log.into_iter().filter(|(tag, r)| *tag == 100 && r.y >= ty - 8.0) {
            if r.y < ty + UNDER_LINE_H { t.push(r.x) } else { c.push(r.x) }
        }
        (t, c)
    }

    fn leftmost(xs: &[f32]) -> f32 {
        xs.iter().copied().fold(f32::MAX, f32::min)
    }

    /// One focused tile's block as drawn: the text runs and the amber play marks, top to bottom.
    fn block_of(label: &TileLabel) -> (Vec<crate::draw_census::Run>, Vec<Rect>) {
        let sty = RowStyle::HOME;
        let rect = Rect::new(700.0, 300.0, sty.w, sty.h);
        let mut runs = Vec::new();
        let marks = crate::draw_census::capture_play_marks(|| {
            runs = crate::draw_census::capture_runs(|| {
                draw_focused(Painter::recording(), Art::Poster(None), rect, 1.0, &sty, None, label,
                    &crate::fixture::FixtureMeasure);
            });
        });
        runs.retain(|run| run.at.y >= rect.y + rect.h);
        runs.sort_by(|a, b| a.at.y.total_cmp(&b.at.y));
        (runs, marks)
    }

    /// **The amber mark leads the FIRST line drawn and hangs left of it**: with a title it leads the
    /// title, and the title is centred as if the mark were not there, on the same centre as the line
    /// below it (a group centred WITH the mark sat half a mark off the line below it).
    /// **The census reads a focused block's lines whatever their ink**: a shadowed Under posters
    /// block, a stray UNSHADOWED legacy block (it would hide from a style whitelist), a line that
    /// reads like its neighbour (two lines, not one) and a gliding line (its follower is the same
    /// line) each come out as the lines a person reads.
    #[test]
    fn the_label_census_reads_every_block_line_whatever_its_ink() {
        let lines_of = |label: &TileLabel| {
            let runs = crate::draw_census::capture_runs(|| {
                draw_focused(Painter::recording(), Art::Poster(None), Rect::new(700.0, 300.0, RowStyle::HOME.w, RowStyle::HOME.h),
                    1.0, &RowStyle::HOME, None, label, &crate::fixture::FixtureMeasure);
            });
            crate::draw_census::label_lines(&runs)
        };
        let shadowed = TileLabel::from_lines([Some("Heat".into()), Some("Crime".into())]);
        assert_eq!(lines_of(&shadowed), ["Heat", "Crime"]);
        let stray = TileLabel::titled("Stray", "legacy line");
        assert!(!stray.shadowed, "premise: the legacy block carries no shadow");
        assert_eq!(lines_of(&stray), ["Stray", "legacy line"], "an unshadowed block is seen too");
        let twins = TileLabel::from_lines([Some("Season 3".into()), Some("Season 3".into())]);
        assert_eq!(lines_of(&twins), ["Season 3", "Season 3"], "two lines that read alike stay two");
        let gliding = TileLabel::from_lines([None, Some("A details line far too long for the card it sits under to hold in one go".into())]).within_tile();
        assert_eq!(lines_of(&gliding).len(), 1, "a gliding line and its follower are one line");
        assert!(lines_of(&TileLabel::default()).is_empty(), "no block, no line");
    }

    #[test]
    fn the_play_mark_hangs_left_of_a_title_centred_with_the_line_below() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        let label = TileLabel { glyph: true, ..TileLabel::titled("Sintel", "8m left") };
        let (runs, marks) = block_of(&label);
        assert_eq!(marks.len(), 1, "one mark: {marks:?}");
        let (title, caption) = (&runs[0], &runs[1]);
        assert_eq!((title.text.as_str(), caption.text.as_str()), ("Sintel", "8m left"));
        let centre = |run: &crate::draw_census::Run| run.at.x + run.at.w * 0.5;
        assert!((centre(title) - centre(caption)).abs() < 0.5,
            "the title is centred like the line below it: {} vs {}", centre(title), centre(caption));
        let mark = marks[0];
        assert!((mark.x + mark.w + PLAY_ICON_GAP - title.at.x).abs() < 0.5, "the mark hangs left of the title: {mark:?} {:?}", title.at);
        assert!(mark.y < title.at.y + UNDER_LINE_H && mark.y + mark.h > title.at.y, "on the title's line: {mark:?} {:?}", title.at);
    }

    /// With no title the mark leads the details line, centred the same way; with no line at all
    /// (a label with no lines) there is nothing to lead and no mark.
    #[test]
    fn the_play_mark_leads_the_details_line_when_there_is_no_title() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        let label = TileLabel { glyph: true, caption: std::ffi::CString::new("8m left").ok(), ..TileLabel::default() };
        let (runs, marks) = block_of(&label);
        assert_eq!((runs.len(), marks.len()), (1, 1), "{runs:?} {marks:?}");
        let (line, mark) = (&runs[0], marks[0]);
        assert!((line.at.x + line.at.w * 0.5 - (700.0 + RowStyle::HOME.w * 0.5)).abs() < 0.5, "centred on the card: {:?}", line.at);
        assert!((mark.x + mark.w + PLAY_ICON_GAP - line.at.x).abs() < 0.5, "the mark hangs left of the line: {mark:?} {:?}", line.at);
        assert!(mark.y < line.at.y + UNDER_CAPTION_H && mark.y + mark.h > line.at.y, "on the line: {mark:?} {:?}", line.at);
        let (runs, marks) = block_of(&TileLabel { glyph: true, ..TileLabel::default() });
        assert!(runs.is_empty() && marks.is_empty(), "a block with no line draws no mark: {runs:?} {marks:?}");
        let (_, marks) = block_of(&TileLabel { glyph: false, ..TileLabel::titled("Sintel", "8m left") });
        assert!(marks.is_empty(), "a press that does not play draws none");
    }

    /// A details line the block cannot hold glides after a mark that stays at the window's left edge.
    #[test]
    fn a_gliding_details_line_keeps_the_play_mark_at_the_window_edge() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let label = TileLabel { glyph: true, caption: std::ffi::CString::new(LONG_CAPTION).ok(), ..TileLabel::default() };
        let (rest, rest_marks) = block_of(&label);
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 500.0) / 1000.0);
        let (gliding, marks) = block_of(&label);
        assert_eq!((rest_marks.len(), marks.len()), (1, 1));
        assert_eq!(rest_marks[0].x, marks[0].x, "the mark does not glide");
        let leftmost_run = |runs: &[crate::draw_census::Run]| runs.iter().map(|r| r.at.x).fold(f32::MAX, f32::min);
        assert!(leftmost_run(&gliding) < leftmost_run(&rest), "the line has moved left of its rest");
        assert!((marks[0].x + marks[0].w + PLAY_ICON_GAP - leftmost_run(&rest)).abs() < 0.5, "at rest the text starts after the mark");
    }

    /// A focused tile's caption — a year, a character, an episode address — is read like its
    /// title: one that does not fit the block rests, glides through, and reports damage while it
    /// moves, instead of ending in an ellipsis that hides the part that tells two credits apart.
    #[test]
    fn a_focused_caption_that_does_not_fit_marquees() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = plx_machine::idle::take_local_damage();
        let (title, rest) = label_runs("Alien", LONG_CAPTION);
        assert_eq!(title.len(), 1, "a fitting title stays one still run: {title:?}");
        assert_eq!(rest.len(), 2, "the caption is a run and its follower: {rest:?}");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "resting is not damage");
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 500.0) / 1000.0);
        let (_, gliding) = label_runs("Alien", LONG_CAPTION);
        assert!(leftmost(&gliding) < leftmost(&rest), "the caption has moved left: {rest:?} -> {gliding:?}");
        assert!(plx_machine::idle::take_local_damage() > 0, "a gliding frame reports");
    }

    /// And nothing else moves: a caption that fits is one still run, centred like it always was,
    /// and a title that fits leaves the shared clock alone — a settled shelf stays quiet.
    #[test]
    fn a_fitting_caption_stays_one_still_run_and_reports_nothing() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        for _ in 0..3 {
            plx_machine::idle::frame_begin(1.5);
            let _ = plx_machine::idle::take_local_damage();
            let (title, caption) = label_runs("Alien", "1979");
            assert_eq!((title.len(), caption.len()), (1, 1));
            assert_eq!(plx_machine::idle::take_local_damage(), 0);
        }
    }

    fn measure_w(s: &str, sz: i32) -> f32 {
        use plx_machine::machine::Measure as _;
        crate::fixture::FixtureMeasure.width_str(s, sz, false)
    }

    /// **The title and its caption are one block, so they start, glide and loop together.** Each
    /// looping on its OWN period, one would restart mid-way through the other's glide — a block
    /// that never rests at once. The block's cycle is its longest run's: the shorter run finishes
    /// its glide early (seamlessly back at its rest position) and waits for the longer to finish.
    #[test]
    fn a_title_and_its_caption_glide_in_lockstep() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let (t0, c0) = label_runs(LONG_TITLE, LONG_CAPTION);
        let (title_x, cap_x) = (leftmost(&t0), leftmost(&c0));
        assert_eq!((t0.len(), c0.len()), (2, 2), "both overflow: {t0:?} {c0:?}");
        let mut elapsed = 0.0_f32;
        let mut at = |ms: f32| {
            plx_machine::idle::frame_begin((ms - elapsed) / 1000.0);
            elapsed = ms;
            label_runs(LONG_TITLE, LONG_CAPTION)
        };
        // both leave the rest beat together
        let (t, c) = at(MARQUEE.hold_ms + 400.0);
        assert!(leftmost(&t) < title_x && leftmost(&c) < cap_x, "both glide: {t:?} {c:?}");
        // the caption's own period ends first; it waits at rest while the title is still gliding
        let own = MARQUEE.period(measure_w(LONG_CAPTION, theme::size::CAPTION));
        let (t, c) = at(own + 500.0);
        assert!(leftmost(&t) < title_x, "the longer title is still gliding: {t:?}");
        assert!(c.iter().any(|x| (x - cap_x).abs() < 0.5), "the caption is back at rest, not looping: {c:?}");
        // and both restart together when the longer one loops
        let cycle = MARQUEE.period(measure_w(LONG_TITLE, theme::size::LABEL));
        let (t, c) = at(cycle + 300.0);
        assert!((leftmost(&t) - title_x).abs() < 0.5 && (leftmost(&c) - cap_x).abs() < 0.5,
            "both restart together: {t:?} {c:?}");
    }

    // ---- a line gliding in a window held to its tile dissolves at BOTH of the window's edges ----

    /// Every run of `caption` one focused tile held to its tile ([`TileLabel::within_tile`])
    /// draws, leftmost first (the run, then its follower), with the census's `clip`, `fade` and
    /// `lead` so a test can read where the window is and how each run dissolves into it.
    fn within_tile_caption_runs(title: &str, caption: &str) -> Vec<crate::draw_census::Run> {
        let sty = RowStyle::HOME;
        let rect = Rect::new(700.0, 300.0, sty.w, sty.h);
        let label = TileLabel::titled(title, caption).within_tile();
        let mut runs = crate::draw_census::capture_runs(|| {
            draw_focused(Painter::recording(), Art::Poster(None), rect, 1.0, &sty, None, &label,
                &crate::fixture::FixtureMeasure);
        });
        runs.retain(|r| r.text == caption);
        runs.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
        runs
    }

    /// The window a gliding run is clipped to, as `(left, right)` in screen x.
    fn window_of(run: &crate::draw_census::Run) -> (f32, f32) {
        let w = run.clip.expect("a gliding run is clipped to its window");
        (w.x, w.x + w.w)
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    /// **A gliding line dissolves in at the window's LEADING edge too (FRAMES-5 item 4).** Only
    /// the trailing edge dissolved, so the line's head was cut hard mid-letter at the window's
    /// left edge ("Th" sliced). Once the run has travelled past [`LABEL_EDGE_FADE`] the first
    /// that-many px of the window ramp 0 -> 1; the trailing dissolve is untouched.
    #[test]
    fn a_gliding_within_tile_line_dissolves_in_at_the_windows_leading_edge() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = within_tile_caption_runs("Alien", LONG_CAPTION);
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 2000.0) / 1000.0);
        let runs = within_tile_caption_runs("Alien", LONG_CAPTION);
        assert_eq!(runs.len(), 2, "a run and its follower: {runs:?}");
        let (left, right) = window_of(&runs[0]);
        assert!(runs[0].at.x < left - LABEL_EDGE_FADE, "the run has glided out past the left edge: {:?}", runs[0]);
        let (from, to) = runs[0].lead.expect("a gliding run dissolves in over the window's first edge-fade px");
        assert!(near(from, left) && near(to, left + LABEL_EDGE_FADE),
            "the leading band covers [{left}, {}], got {from}..{to}", left + LABEL_EDGE_FADE);
        let (tf, tt) = runs[0].fade.expect("the trailing dissolve is still there");
        assert!(near(tf, right - LABEL_EDGE_FADE) && near(tt, right), "trailing band unchanged: {tf}..{tt}");
        assert!(runs[1].at.x > left, "the follower is still inside the window's right: {:?}", runs[1]);
        assert!(runs[1].lead.is_none(), "a run that starts inside the window has no leading band: {:?}", runs[1]);
    }

    /// And AT REST (the rest beat before the glide) the run starts exactly at the window's left
    /// edge and draws crisp there: the leading band applies only while the run's left is past it.
    #[test]
    fn a_within_tile_line_at_rest_draws_crisp_at_the_windows_leading_edge() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let runs = within_tile_caption_runs("Alien", LONG_CAPTION);
        assert_eq!(runs.len(), 2, "a run and its follower: {runs:?}");
        let (left, right) = window_of(&runs[0]);
        assert!(near(runs[0].at.x, left), "at rest the run starts on the window's left edge: {:?}", runs[0]);
        assert!(runs.iter().all(|r| r.lead.is_none()), "no leading band at rest: {runs:?}");
        let (tf, tt) = runs[0].fade.expect("the run still dissolves into the window's right edge");
        assert!(near(tf, right - LABEL_EDGE_FADE) && near(tt, right), "trailing band unchanged: {tf}..{tt}");
    }

    /// **The leading band grows in with the travel instead of popping.** The first glide frame
    /// moves the run well under a pixel; a full-width band there would dim the head of the line by
    /// most of its alpha in that one frame. The band is as wide as the run has travelled, up to
    /// the full [`LABEL_EDGE_FADE`] — zero width at rest, so the two are the same picture.
    #[test]
    fn the_leading_band_grows_in_with_the_travel_instead_of_popping() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = within_tile_caption_runs("Alien", LONG_CAPTION);
        // a quarter second into the glide: 10 px travelled at 40 px/s
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 250.0) / 1000.0);
        let runs = within_tile_caption_runs("Alien", LONG_CAPTION);
        let (left, _) = window_of(&runs[0]);
        let travelled = left - runs[0].at.x;
        assert!(travelled > 5.0 && travelled < LABEL_EDGE_FADE, "10 px into the glide: {travelled}");
        let (from, to) = runs[0].lead.expect("a run past the left edge has a leading band");
        assert!(near(from, left) && near(to, left + travelled),
            "the band is as wide as the travel ({travelled} px from {left}): {from}..{to}");
    }

    /// A shorter line of a block parks at the end of its travel while the longer one still glides,
    /// its follower sitting on the window's left edge up to float noise: that is a run AT REST and
    /// must not pick up a leading band from an ulp of `-off + run_w + GAP`.
    #[test]
    fn a_line_parked_at_the_end_of_its_travel_draws_crisp() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = within_tile_caption_runs(LONG_TITLE, LONG_CAPTION);
        let own = MARQUEE.period(measure_w(LONG_CAPTION, theme::size::CAPTION));
        plx_machine::idle::frame_begin((own + 500.0) / 1000.0);
        let runs = within_tile_caption_runs(LONG_TITLE, LONG_CAPTION);
        let (left, _) = window_of(&runs[0]);
        let parked: Vec<_> = runs.iter().filter(|r| (r.at.x - left).abs() < 0.5).collect();
        assert_eq!(parked.len(), 1, "the follower has arrived at the window's left edge: {runs:?}");
        assert!(parked[0].lead.is_none(), "and it is at rest, so crisp: {:?}", parked[0]);
    }

    /// The leading band as a pure function of how far the run's head is past the window's edge:
    /// none at rest or within float noise of it, as wide as the travel while it is short of the
    /// full dissolve, and the full [`LABEL_EDGE_FADE`] beyond it — always starting at the edge
    /// (`from` is the travel itself, in px from the string's own left).
    #[test]
    fn the_leading_band_is_none_at_rest_grows_with_travel_and_caps_at_the_edge_fade() {
        assert_eq!(lead_band(100.0, 100.0), None, "at rest");
        assert_eq!(lead_band(100.0 + 1e-4, 100.0), None, "inside the window by an ulp: still at rest");
        assert_eq!(lead_band(100.0 - 1e-4, 100.0), None, "past the edge by an ulp: still at rest");
        assert_eq!(lead_band(90.0, 100.0), Some((10.0, 20.0)));
        assert_eq!(lead_band(100.0 - LABEL_EDGE_FADE, 100.0), Some((24.0, 48.0)));
        assert_eq!(lead_band(0.0, 100.0), Some((100.0, 100.0 + LABEL_EDGE_FADE)));
    }

    // ---- a credit label is one name line and one role line, in every state -------------------

    thread_local! {
        static CREDIT_CLOCK: marquee::Clock = const { marquee::Clock::new() };
    }

    /// Every text run one credit label draws, as `(x, y)`.
    fn credit_runs(name: &str, role: &str, focused: bool) -> Vec<(f32, f32)> {
        let at = Rect::new(500.0, 100.0, 222.0, 60.0);
        crate::draw_census::capture(|| {
            CREDIT_CLOCK.with(|c| draw_credit_label(Painter::recording(), c, at, name, role, focused,
                &crate::fixture::FixtureMeasure));
        })
        .into_iter()
        .filter(|(tag, _)| *tag == 100)
        .map(|(_, r)| (r.x, r.y))
        .collect()
    }

    /// The distinct baselines (rounded) a set of runs sits on, top to bottom.
    fn baselines(runs: &[(f32, f32)]) -> Vec<i32> {
        let mut ys: Vec<i32> = runs.iter().map(|r| r.1.round() as i32).collect();
        ys.sort_unstable();
        ys.dedup();
        ys
    }

    const CREDIT_CASES: [(&str, &str); 4] = [
        ("Ana", "Director"),
        ("Sergio Hasselbaink", "Barley, the lumberjack"),
        ("Rogier Schippers", "Captain, the long-suffering hero of the northern wastes"),
        ("Alexandra Wolkowicz-Harrington Smythe", LONG_CAPTION),
    ];

    /// **One shape, focused or not.** A headshot's label is a name line and a role line, ALWAYS:
    /// the same two baselines for a short role, a medium one and one that is far too long, focused
    /// or resting, mid-glide or not. A role that wrapped to two lines when unfocused and became
    /// one clipped line on focus made the block change height and jump when focus moved.
    #[test]
    fn a_credit_label_is_one_name_line_and_one_role_line_in_every_state() {
        let _serial = plx_base::testlock::serial();
        plx_machine::idle::frame_begin(1.0 / 60.0);
        CREDIT_CLOCK.with(|c| c.release());
        let reference = baselines(&credit_runs("Ana", "Director", false));
        assert_eq!(reference.len(), 2, "a name line and a role line: {reference:?}");
        for (name, role) in CREDIT_CASES {
            for focused in [false, true] {
                CREDIT_CLOCK.with(|c| c.release());
                for ms in [0.0_f32, MARQUEE.hold_ms + 700.0, 4000.0] {
                    plx_machine::idle::frame_begin(ms / 1000.0);
                    let runs = credit_runs(name, role, focused);
                    assert_eq!(baselines(&runs), reference,
                        "{name:?}/{role:?} focused={focused} at {ms} ms: {runs:?}");
                }
            }
        }
    }

    /// Unfocused, a name or role that does not fit is ONE elided run — it neither wraps nor
    /// glides, so a resting shelf of credits is still and every label is a single line.
    #[test]
    fn an_unfocused_credit_label_that_overflows_is_one_elided_run_per_line() {
        let _serial = plx_base::testlock::serial();
        for (name, role) in CREDIT_CASES {
            for ms in [0.0_f32, 5000.0] {
                plx_machine::idle::frame_begin(ms / 1000.0);
                let _ = plx_machine::idle::take_local_damage();
                assert_eq!(credit_runs(name, role, false).len(), 2, "{name:?}/{role:?}");
                assert_eq!(plx_machine::idle::take_local_damage(), 0, "an unfocused label never animates");
            }
        }
    }

    /// Focused and too long, name and role glide on one clock and one cycle, like a poster's
    /// title and caption, and a fitting focused label draws two still runs and reports nothing.
    #[test]
    fn a_focused_credit_label_glides_only_the_lines_that_do_not_fit() {
        let _serial = plx_base::testlock::serial();
        CREDIT_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = plx_machine::idle::take_local_damage();
        let (long_name, long_role) = CREDIT_CASES[3];
        let rest = credit_runs("Ana", long_role, true);
        assert_eq!(rest.len(), 3, "a still name, plus the role and its follower: {rest:?}");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "resting is not damage");
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 500.0) / 1000.0);
        let moved = credit_runs("Ana", long_role, true);
        assert!(moved.iter().map(|r| r.0).fold(f32::MAX, f32::min)
            < rest.iter().map(|r| r.0).fold(f32::MAX, f32::min), "the role glides: {rest:?} -> {moved:?}");
        assert!(plx_machine::idle::take_local_damage() > 0);
        CREDIT_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.5);
        let _ = plx_machine::idle::take_local_damage();
        assert_eq!(credit_runs("Ana", "Director", true).len(), 2);
        assert_eq!(credit_runs(long_name, "Director", true).len(), 3, "the long name and its follower");
        CREDIT_CLOCK.with(|c| c.release());
        let _ = plx_machine::idle::take_local_damage();
        assert_eq!(credit_runs("Ana", "Director", true).len(), 2);
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "a fitting label never reports");
    }

    // ---- right-to-left lines glide mirrored ---------------------------------------------------

    const LONG_ARABIC_TITLE: &str = "المغامرات الطويلة جدا لألكسندرا وولكوفيتش هارينغتون سميث وأطبائها الكثيرين في البيت الملكي";
    const LONG_ARABIC_CAPTION: &str = "الدكتورة ألكسندرا وولكوفيتش هارينغتون سميث، كبيرة الأطباء في البيت الملكي";

    /// `text`'s run and its follower in `runs`, as (follower, run), and the clip they glide in.
    fn glide_pair(runs: &[crate::draw_census::Run], text: &str) -> (f32, f32, Rect) {
        let mut xs: Vec<&crate::draw_census::Run> = runs.iter().filter(|r| r.text == text).collect();
        assert_eq!(xs.len(), 2, "{text:?} glides as a run and its follower: {runs:?}");
        xs.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
        (xs[0].at.x, xs[1].at.x, xs[1].clip.expect("a gliding run is clipped to its window"))
    }

    /// The mirror image, checked line by line: at rest the run's RIGHT edge is on its window's (the
    /// first words show), its follower waits one gap to its LEFT; 500 ms into the glide it has
    /// moved 20 px RIGHT (40 px/s).
    fn assert_glides_mirrored(rest: &[crate::draw_census::Run], moved: &[crate::draw_census::Run], text: &str, sz: i32) {
        let w = measure_w(text, sz);
        let (follower, run, clip) = glide_pair(rest, text);
        assert!((run + w - (clip.x + clip.w)).abs() < 0.01, "{text:?} rests flush right: {run} + {w} vs {clip:?}");
        assert!((follower + w + marquee::GAP - run).abs() < 0.01, "the follower one gap to the left");
        let (_, later, _) = glide_pair(moved, text);
        assert!((later - run - 20.0).abs() < 0.01, "{text:?} glides right: {run} -> {later}");
    }

    /// **An Arabic title reads from its right edge**, so a focused one that overflows glides as the
    /// mirror image of a Latin one, and its caption with it.
    #[test]
    fn a_right_to_left_title_and_caption_rest_on_their_first_words_and_glide_right() {
        let _serial = plx_base::testlock::serial();
        let sty = RowStyle::HOME;
        let rect = Rect::new(700.0, 300.0, sty.w, sty.h);
        let label = TileLabel::titled(LONG_ARABIC_TITLE, LONG_ARABIC_CAPTION);
        let draw = || crate::draw_census::capture_runs(|| {
            draw_focused(Painter::recording(), Art::Poster(None), rect, 1.0, &sty, None, &label,
                &crate::fixture::FixtureMeasure);
        });
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let rest = draw();
        plx_machine::idle::frame_begin(MARQUEE.hold_ms / 1000.0);
        let _ = draw();
        plx_machine::idle::frame_begin(0.5);
        let moved = draw();
        assert_glides_mirrored(&rest, &moved, LONG_ARABIC_TITLE, theme::size::LABEL);
        assert_glides_mirrored(&rest, &moved, LONG_ARABIC_CAPTION, theme::size::CAPTION);
        TITLE_CLOCK.with(|c| c.release());
    }

    /// And a headshot's Arabic name and role.
    #[test]
    fn a_right_to_left_credit_label_glides_mirrored() {
        let _serial = plx_base::testlock::serial();
        let at = Rect::new(500.0, 100.0, 222.0, 60.0);
        let draw = || crate::draw_census::capture_runs(|| {
            CREDIT_CLOCK.with(|c| draw_credit_label(Painter::recording(), c, at, LONG_ARABIC_TITLE,
                LONG_ARABIC_CAPTION, true, &crate::fixture::FixtureMeasure));
        });
        CREDIT_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let rest = draw();
        plx_machine::idle::frame_begin(MARQUEE.hold_ms / 1000.0);
        let _ = draw();
        plx_machine::idle::frame_begin(0.5);
        let moved = draw();
        assert_glides_mirrored(&rest, &moved, LONG_ARABIC_TITLE, theme::size::LABEL);
        assert_glides_mirrored(&rest, &moved, LONG_ARABIC_CAPTION, theme::size::CAPTION);
        CREDIT_CLOCK.with(|c| c.release());
    }

    /// **A right-to-left line dissolves where it is cut, the mirror of a Latin one.** At rest an
    /// overflowing Arabic line sits flush right on its first words, so it draws crisp at the
    /// window's right edge and dissolves into the LEFT one, where its last words are cut.
    #[test]
    fn a_right_to_left_within_tile_line_at_rest_is_crisp_at_its_head_and_dissolves_at_its_left() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let runs = within_tile_caption_runs("Alien", LONG_ARABIC_CAPTION);
        assert_eq!(runs.len(), 2, "a run and its follower: {runs:?}");
        let run = &runs[1];
        let (left, right) = window_of(run);
        let w = measure_w(LONG_ARABIC_CAPTION, theme::size::CAPTION);
        assert!(near(run.at.x + w, right), "at rest the first words sit on the window's right edge: {run:?}");
        assert!(run.fade.is_none(), "the head draws crisp at the window's right edge at rest: {run:?}");
        let (from, to) = run.lead.expect("the cut tail dissolves into the window's left edge");
        assert!(near(from, left) && near(to, left + LABEL_EDGE_FADE),
            "the left band covers [{left}, {}], got {from}..{to}", left + LABEL_EDGE_FADE);
        TITLE_CLOCK.with(|c| c.release());
    }

    /// Once it glides right, the head leaves through the window's right edge, and that band grows
    /// with the travel up to the full [`LABEL_EDGE_FADE`] (the leading band's rule, mirrored), so
    /// the first glide frame is the rest picture.
    #[test]
    fn a_gliding_right_to_left_line_dissolves_out_at_the_right_edge_growing_with_the_travel() {
        let _serial = plx_base::testlock::serial();
        TITLE_CLOCK.with(|c| c.release());
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = within_tile_caption_runs("Alien", LONG_ARABIC_CAPTION);
        let w = measure_w(LONG_ARABIC_CAPTION, theme::size::CAPTION);
        // a quarter second into the glide: 10 px travelled at 40 px/s
        plx_machine::idle::frame_begin((MARQUEE.hold_ms + 250.0) / 1000.0);
        let runs = within_tile_caption_runs("Alien", LONG_ARABIC_CAPTION);
        let run = &runs[1];
        let (_, right) = window_of(run);
        let travelled = run.at.x + w - right;
        assert!(travelled > 5.0 && travelled < LABEL_EDGE_FADE, "10 px into the glide: {travelled}");
        let (from, to) = run.fade.expect("a head past the right edge dissolves out there");
        assert!(near(from, right - travelled) && near(to, right),
            "the band is as wide as the travel ({travelled} px to {right}): {from}..{to}");
        // two seconds on, past the full band
        plx_machine::idle::frame_begin(1.75);
        let runs = within_tile_caption_runs("Alien", LONG_ARABIC_CAPTION);
        let run = &runs[1];
        let (_, right) = window_of(run);
        assert!(run.at.x + w - right > LABEL_EDGE_FADE, "well into the glide: {run:?}");
        let (from, to) = run.fade.expect("the head still dissolves out at the right edge");
        assert!(near(from, right - LABEL_EDGE_FADE) && near(to, right), "the full band: {from}..{to}");
        TITLE_CLOCK.with(|c| c.release());
    }
}

#[cfg(test)]
mod sort_caption_review_tests {
    use super::*;
    use plx_machine::machine::Measure;
    use std::{cell::{Cell, RefCell}, ffi::CStr, rc::Rc};

    #[derive(Default)]
    struct CachedMeasure {
        fits: RefCell<plx_gfx::text::FittedLines>,
        widths: Cell<usize>,
    }
    impl Measure for CachedMeasure {
        fn fit_line(&self, s: &str, budget: f32, size: i32, bold: bool) -> Rc<CStr> {
            self.fits.borrow_mut().fit(self, s, budget, size, bold)
        }
        fn fit_line_measured(&self, s: &str, budget: f32, size: i32, bold: bool) -> (Rc<CStr>, f32) {
            self.fits.borrow_mut().fit_measured(self, s, budget, size, bold)
        }
        fn width(&self, s: &CStr, size: i32, _: bool) -> f32 {
            self.widths.set(self.widths.get() + 1);
            s.to_bytes().len() as f32 * size as f32 * 0.5
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 }
        fn line_h(&self, size: i32) -> f32 { size as f32 }
    }

    #[test]
    fn a_warm_sort_value_line_measures_nothing_and_a_focused_one_adds_only_its_second_texture() {
        let _guard = plx_base::testlock::serial();
        let measure = CachedMeasure::default();
        let value = SortValue { text: "98%".into(), mark: &[(crate::icons::Icon::Crowd, theme::RATING_AUDIENCE)], ..Default::default() };
        let rect = Rect::new(100.0, 100.0, 250.0, 375.0);
        let style = RowStyle { w: 250.0, ..crate::poster_grid::STYLE };
        let draw = |s: f32, focused: bool| draw_sort_value(Painter::recording(), rect, s, &style, &value, &measure, focused);
        draw(1.0, false);
        let cold = measure.widths.get();
        assert!(cold > 0);
        draw(1.0, false);
        assert_eq!(measure.widths.get(), cold, "a warm value line must not measure again");
        draw(style.focus_scale, true);
        let focused_cold = measure.widths.get();
        draw(style.focus_scale, true);
        assert_eq!(measure.widths.get(), focused_cold);
    }

    /// The ink a value line is drawn with at lift factor `t` (the card's scale on the way from rest to
    /// `focus_scale`): the `(regular, bold)` effective alphas of the runs, the shadow's runs left out,
    /// and the bold run's colour.
    fn value_inks(t: f32, focused: bool, value: &SortValue) -> (f32, f32, Option<[f32; 4]>) {
        let style = RowStyle { w: 250.0, ..crate::poster_grid::STYLE };
        let rect = Rect::new(100.0, 100.0, 250.0, 375.0);
        // a settled lift IS `focus_scale` (the spring lands on it), not 1 + 1 * (focus_scale - 1)
        let s = if t >= 1.0 { style.focus_scale } else { 1.0 + t * (style.focus_scale - 1.0) };
        let runs = crate::draw_census::capture_runs(|| draw_sort_value(
            Painter::recording(), rect, s, &style, value, &crate::fixture::FixtureMeasure, focused));
        // each texture is a shadow run (black) then an ink run; the ink is the run whose colour is not black
        let ink = |bold: bool| runs.iter().filter(|run| run.bold == bold && run.ink[0] > 0.0).map(|run| run.ink).next();
        (ink(false).map_or(0.0, |c| c[3]), ink(true).map_or(0.0, |c| c[3]), ink(true))
    }

    #[test]
    fn a_focused_settled_value_is_drawn_at_the_full_ink_of_text_primary() {
        let _guard = plx_base::testlock::serial();
        let value = SortValue { text: "98%".into(), ..Default::default() };
        // Settled and focused: the bold texture alone, at TEXT_PRIMARY's colour and an alpha of exactly 1.
        let (regular, bold, colour) = value_inks(1.0, true, &value);
        assert_eq!(regular, 0.0, "no regular texture left under the bold one");
        assert_eq!(colour, Some(theme::TEXT_PRIMARY), "the peak channel IS #F7FAFC");
        assert_eq!(bold, 1.0);
        // Rest: the regular texture alone at 80% white.
        let (regular, bold, _) = value_inks(0.0, false, &value);
        assert!((regular - 0.8).abs() < 1e-6 && bold == 0.0, "rest is 80% white regular: {regular} {bold}");
        // A missing value stays a regular 50% white dash, lifted or not (absence does not brighten).
        let dash = SortValue { text: "\u{2013}".into(), missing: true, ..Default::default() };
        for (t, focused) in [(0.0, false), (1.0, true)] {
            let (regular, bold, _) = value_inks(t, focused, &dash);
            assert!((regular - 0.5).abs() < 1e-6 && bold == 0.0, "dash t={t}: {regular} {bold}");
        }
    }

    #[test]
    fn a_tomato_mark_is_drawn_at_the_boards_15_px_body_hung_6_px_left_of_the_number() {
        let _guard = plx_base::testlock::serial();
        use crate::icons::Icon;
        let mark: &'static [crate::widgets::MarkLayer] =
            &[(Icon::Tomato, theme::RATING_FRESH), (Icon::TomatoCalyx, theme::RATING_LEAF)];
        let value = SortValue { text: "98%".into(), mark, ..Default::default() };
        let style = RowStyle { w: 250.0, ..crate::poster_grid::STYLE };
        let rect = Rect::new(100.0, 100.0, 250.0, 375.0);
        let (runs, icons) = {
            let mut icons = Vec::new();
            let runs = crate::draw_census::capture_runs(|| {
                icons = crate::draw_census::capture_icons(|| draw_sort_value(
                    Painter::recording(), rect, 1.0, &style, &value, &crate::fixture::FixtureMeasure, false));
            });
            (runs, icons)
        };
        assert!(runs.iter().any(|run| run.text == "98%"), "the number");
        let number_left = rect.cx() - crate::fixture::FixtureMeasure.width_str("98%", theme::size::CAPTION, false) * 0.5;
        assert_eq!(icons.len(), 2, "both layers of the mark");
        let line_cy = rect.y + rect.h + SORT_VALUE_GAP + SORT_VALUE_H * 0.5;
        for (id, at) in &icons {
            // every layer shares one box, centred on the line and the board's 20 px box
            let zoom = crate::icons::mark_zoom(*id);
            assert!((at.w - SORT_VALUE_MARK_D * zoom).abs() < 0.01 && (at.cy() - line_cy).abs() < 0.01, "{id:?} {at:?}");
            assert!((at.cx() + SORT_VALUE_MARK_D * 0.5 - (number_left - SORT_VALUE_MARK_GAP)).abs() < 0.01,
                "the 20 px box hangs 6 px left of the number: {at:?} vs {number_left}");
        }
        let body = icons[0].1.w * 15.2 / 26.0;
        assert!((body - 15.0).abs() < 0.05, "the body is the board's 15 px disc, not {body}");
    }

    #[test]
    fn the_lift_crossfade_is_monotone_in_both_directions() {
        let _guard = plx_base::testlock::serial();
        let value = SortValue { text: "98%".into(), ..Default::default() };
        for focused in [true, false] {
            let steps: Vec<(f32, f32)> = (0..=20).map(|i| {
                let (regular, bold, _) = value_inks(i as f32 / 20.0, focused, &value);
                (regular, bold)
            }).collect();
            assert!(steps.windows(2).all(|w| w[1].1 >= w[0].1), "the bold ink only rises with the lift: {steps:?}");
            assert!(steps.windows(2).all(|w| w[1].0 <= w[0].0), "the regular ink only falls with the lift: {steps:?}");
            assert_eq!((steps[0], steps[20]), ((0.8, 0.0), (0.0, 1.0)), "exact ends (focused={focused})");
            // the value never goes dimmer than the dimmer end part-way: the two textures together keep it readable
            assert!(steps.iter().all(|&(regular, bold)| regular + bold >= 0.5), "{steps:?}");
        }
    }

    #[test]
    fn the_weight_crossfade_has_exact_ends_and_the_leaving_texture_fades_faster() {
        for rising in [true, false] {
            assert_eq!(sort_value_weights(0.0, rising), (1.0, 0.0), "at rest only the regular texture");
            assert_eq!(sort_value_weights(1.0, rising), (0.0, 1.0), "lifted only the bold texture");
            assert_eq!(sort_value_weights(-0.5, rising), (1.0, 0.0));
            assert_eq!(sort_value_weights(2.0, rising), (0.0, 1.0));
        }
        for step in 1..10 {
            let t = step as f32 / 10.0;
            let (regular, bold) = sort_value_weights(t, true);
            assert!(regular < 1.0 - t && bold == t, "the card taking focus loses its regular texture first: {t}");
            let (regular, bold) = sort_value_weights(t, false);
            assert!(bold < t && regular == 1.0 - t, "the card giving focus up loses its bold texture first: {t}");
        }
    }
}
