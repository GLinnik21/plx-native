//! `CardRow` — the shared animated poster-shelf component.
//!
//! Extracts the home shelf's per-row animation state (a focus-scale [`Spring`] per cell + a
//! horizontal scroll spring) and its tile rendering (poster + big glow focus ring + resume bar +
//! centered title) into ONE reusable widget, so the home `Grid` and the detail Related row are
//! literally the same component — [`RowStyle::HOME`] is the single source of the shelf's motion and
//! geometry that both screens read.
//!
//! What stays per-caller is only the x/`base_y` positioning loop, because home threads `env.sp` and
//! splits the loop across the `Grid`'s two-pass **cross-row** focused-last z-order (invariant #3 in
//! `ui/CLAUDE.md`) — which a lone detail row doesn't have. So `CardRow` deliberately owns spring
//! state + the leaf tile draws, and NEVER draws a whole row or applies an in-row focused-last pass
//! on a caller's behalf. It's decoupled from any focus globals / catalog: focus is a plain
//! `Option<usize>`, the item art is a [`Art`] the caller supplies.
use crate::ui::consts::*;
use crate::ui::theme;
use crate::ui::widgets::{card, Art};
use crate::ui::{Painter, Rect, Spring};
use std::os::raw::c_char;

pub(crate) const MAX_ROW_ITEMS: usize = 24; // == home MAX_ITEMS

/// A card row's motion + geometry. [`RowStyle::HOME`] is the single value both the home grid and the
/// detail Related row pass, so the two rows are indistinguishable in look and animation.
#[derive(Clone, Copy)]
pub(crate) struct RowStyle {
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
    /// A RESERVE rather than a right edge, so the default is 0 and every existing style is
    /// unchanged to the pixel; [`RowStyle::with_right_reserve`] is how a screen declares one.
    pub right_reserve: f32,
}
impl RowStyle {
    /// This style with `r` px of the panel's right edge reserved — see [`RowStyle::right_reserve`].
    /// A `const fn` so a screen can name the result once, beside the constant it derives it from,
    /// rather than rebuilding it per frame.
    pub(crate) const fn with_right_reserve(self, r: f32) -> RowStyle {
        RowStyle {
            right_reserve: r,
            ..self
        }
    }

    /// The home shelf's portrait-poster row: 1.09 focus pop (matches `Home Screen.dc.html`), the big
    /// glow ring, animated scroll. The `ui::press` click dips a focused card from here back toward
    /// `scale(1.0)`, so the pop needs to be large enough for that press-in to read.
    pub(crate) const HOME: RowStyle = RowStyle {
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
    pub(crate) const EPISODE: RowStyle = RowStyle {
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
    pub(crate) const CAST: RowStyle = RowStyle {
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
    pub(crate) const PROFILES: RowStyle = RowStyle {
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
    /// ring focus scalar denominator — `(s-1)/ring_denom` maps scale∈[1, focus_scale] to [0, 1].
    #[inline]
    fn ring_denom(&self) -> f32 {
        self.focus_scale - 1.0
    }
    /// Corner radius for a tile of size `rect` at radius-scale `s`: half-width for a circle, else the
    /// style's fixed radius scaled with the focus pop.
    #[inline]
    pub(crate) fn tile_radius(&self, rect: Rect, s: f32) -> f32 {
        if self.circular {
            rect.w * 0.5
        } else {
            self.radius * s
        }
    }
}

/// Per-row animation state: a focus-scale spring per cell + the horizontal scroll spring. `Copy` so
/// a grid can hold `[CardRow; N]` and copy-init it.
#[derive(Clone, Copy)]
pub(crate) struct CardRow {
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
}
impl CardRow {
    pub(crate) const fn new() -> Self {
        CardRow {
            scale: [Spring::at(1.0); MAX_ROW_ITEMS],
            overflow: Spring::at(1.0),
            focus: -1,
            scroll_x: Spring::at(0.0),
            lift: Spring::at(0.0),
            band: Spring::at(0.0),
            base_y: 0.0,
        }
    }
    /// Step every cell's scale spring (all `MAX_ROW_ITEMS` every frame — invariant #10) toward
    /// `focus_scale` for the focused cell else 1.0; then, ONLY when this row is focused, glide the
    /// scroll spring *minimally*: scroll just far enough that the focused cell (plus one gap of
    /// breathing room) sits fully inside the viewport, and not at all when it already does — no
    /// slot pinning, so entering a row never shuffles tiles that were already visible.
    /// `focused == None` freezes the scroll (a non-focused row holds its position — exactly the
    /// home shelf's behavior).
    pub(crate) fn update(&mut self, n: usize, focused: Option<usize>, sty: &RowStyle, dt: f32) {
        self.focus = focused.map(|f| f as i32).unwrap_or(-1);
        for (i, sp) in self.scale.iter_mut().enumerate() {
            let target = if focused == Some(i) {
                sty.focus_scale
            } else {
                1.0
            };
            sp.step(target, sty.k_scale, dt);
        }
        // the shared overflow spring pops only while focus is actually out past the array
        let ot = if focused.is_some_and(|f| f >= MAX_ROW_ITEMS) {
            sty.focus_scale
        } else {
            1.0
        };
        self.overflow.step(ot, sty.k_scale, dt);
        if let Some(fc) = focused {
            if n > 0 {
                let want = scroll_into_view(
                    self.scroll_x.pos,
                    fc,
                    n,
                    sty.w,
                    sty.gap,
                    SCR_W - 2.0 * sty.margin_x,
                );
                self.scroll_x.step(want, sty.k_scroll, dt);
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
    /// Cell `i`'s live focus-pop scale. Cells inside the spring array own a spring each; every cell
    /// PAST it shares `overflow`, and only the focused one reads it — otherwise the whole tail of a
    /// long row would pop together. The cost of sharing is that stepping between two overflow cells
    /// swaps the pop instantly instead of crossfading; the alternative (this used to clamp the index,
    /// so an overflow cell read cell 23's spring) was a focused tile with NO pop, NO lifted shadow
    /// and NO ring — the entire focus treatment is derived from this number.
    #[inline]
    pub(crate) fn scale(&self, i: usize) -> f32 {
        if i < MAX_ROW_ITEMS {
            self.scale[i].pos
        } else if self.focus == i as i32 {
            self.overflow.pos
        } else {
            1.0
        }
    }
    #[inline]
    pub(crate) fn scroll_x(&self) -> f32 {
        self.scroll_x.pos
    }
    /// How far this row's heading must rise, live — see [`heading_clearance`] for the rule and
    /// [`CardRow::update`] for the spring that holds it there.
    #[inline]
    pub(crate) fn lift(&self) -> f32 {
        self.lift.pos
    }
    /// This shelf's live label-band expansion, 0 collapsed → 1 focused. Feed it to [`under_band`];
    /// a screen wanting the SETTLED layout (a scroll target must be computed from that, never from
    /// the value mid-flight) calls `under_band` with a bare `0.0`/`1.0` instead.
    #[inline]
    pub(crate) fn band_expand(&self) -> f32 {
        self.band.pos
    }
    /// The band this shelf is reserving right now — [`under_band`] of [`CardRow::band_expand`].
    #[inline]
    pub(crate) fn under_band(&self) -> f32 {
        under_band(self.band.pos)
    }
    /// How much of the focused label block this shelf may show right now — [`band_reveal`] of
    /// [`CardRow::band_expand`]. Chain it onto the `TileLabel` with [`TileLabel::revealed`].
    #[inline]
    pub(crate) fn band_reveal(&self) -> f32 {
        band_reveal(self.band.pos)
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
/// Lifted out of `ui::home`'s private `Grid::vert`, which has done this since the home grid was
/// written and was the only screen that did. Every shelf-based screen shares it now.
pub(crate) fn column_near_x(
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
#[inline]
pub(crate) fn tile_centre_x(i: usize, origin: f32, adv: f32, w: f32, scroll: f32) -> f32 {
    origin + i as f32 * adv - scroll + w * 0.5
}

/// THE clamp-into-view core every scroller shares — uniform slots ([`scroll_into_view`]),
/// variable-width season tabs, and the home grid's vertical spring: the nearest scroll in
/// `[0, max]` at which the target band is fully revealed. `lo`/`hi` are the scrolls where the
/// item's trailing/leading edge (plus breathing room) just fits; `cur` already inside ⇒ returned
/// unchanged (no pinning).
pub(crate) fn reveal(cur: f32, lo: f32, hi: f32, max: f32) -> f32 {
    cur.clamp(lo.min(hi), hi).clamp(0.0, max)
}

/// The minimal scroll-into-view rule for uniform-slot strips (home shelves, detail
/// Related/Cast/episodes, the profiles roster): scroll only when the focused item — padded by one
/// `gap` of breathing room (which also covers the focus pop) — would clip the viewport `vw`,
/// clamped to the content range so short rows never over-scroll. (Season tabs and the vertical
/// grid have non-uniform geometry and feed their own edges straight into [`reveal`].)
pub(crate) fn scroll_into_view(cur: f32, fc: usize, n: usize, w: f32, gap: f32, vw: f32) -> f32 {
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
/// has to RESERVE room for it: the home grid's vertical reveal bounds the scroll so a raised
/// heading cannot SETTLE inside the shared top band ([`crate::ui::home`]'s `row_reveal_band` — a
/// row travelling to that destination still crosses the band, under chrome drawn after it, exactly
/// as its cards do), and it can only do that arithmetic if the lift is a fact it can ask for rather
/// than one that lives inside a draw. Written out by hand at the call site it would be the same three terms in two
/// places, and a change to `focus_scale` would move the drawn heading while leaving the layout that
/// makes room for it behind.
#[inline]
pub(crate) fn heading_lift_max(sty: &RowStyle) -> f32 {
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
pub(crate) fn heading_clearance(focused: Option<usize>, sty: &RowStyle, scroll: f32) -> f32 {
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
pub(crate) const HEADING_SOURCE_PAD: f32 = theme::space::XS;


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
pub(crate) fn heading_flow(
    title: &str,
    source: &str,
    mut run: impl FnMut(&str, f32, std::os::raw::c_int, std::os::raw::c_int, [f32; 4]) -> f32,
) -> f32 {
    let mut dx = run(title, 0.0, theme::size::HEADLINE, 1, theme::TEXT_HEADING);
    if source.is_empty() {
        return dx; // one source: the heading is the title and nothing else
    }
    dx += HEADING_SOURCE_PAD;
    dx += run("\u{b7}", dx, theme::size::BODY, 0, theme::TEXT_SEPARATOR);
    dx += HEADING_SOURCE_PAD;
    dx += run(source, dx, theme::size::BODY, 0, theme::TEXT_TERTIARY);
    dx
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
pub(crate) fn draw_heading(p: Painter, title: &str, source: &str, x: f32, y: f32, max_w: f32) {
    heading_flow(title, source, |s, dx, sz, bold, ink| {
        // **`max_w` is a RIGHT BOUNDARY, and a run that would cross it is elided rather than
        // clipped** — through `text::elide`, the single truncation impl in the app, so a bounded
        // heading reads like every other bounded string here.
        //
        // It exists because the Library's A–Z rail occupies the right of the content region: a
        // heading drawn to the panel edge runs UNDER the letters, which looks broken and is not
        // something the rail can fix from its own side. Every other caller passes `INFINITY` and
        // pays nothing — the elide is skipped outright, so the memoised binary search is never
        // entered for a heading nobody bounded.
        let room = max_w - dx;
        if max_w.is_finite() && room < MIN_HEADING_RUN {
            return 0.0; // no room left for this run at all — drop it rather than draw a stub
        }
        let owned;
        let s = if max_w.is_finite() {
            owned = crate::text::elide(s, room, sz, bold, false);
            owned.as_str()
        } else {
            s
        };
        // the CString must outlive the draw call, not the closure (`ui/CLAUDE.md`'s first gotcha)
        match std::ffi::CString::new(s) {
            Ok(cs) => p.text(
                cs.as_ptr(),
                x + dx,
                crate::text::baseline_y(sz, bold, theme::size::HEADLINE, 1, y),
                sz,
                ink,
                0,
                bold,
            ),
            Err(_) => 0.0,
        }
    });
}

/// The narrowest run [`draw_heading`] will draw against a bound. Below it an elide has nothing left
/// but its own ellipsis, and a heading ending in a bare `…` beside another bare `…` reads as
/// damage rather than as truncation.
const MIN_HEADING_RUN: f32 = 48.0;


/// A non-focused cell body: the art tile + an optional resume bar. `rect` is the caller's
/// already-scaled rect; `s` scales the corner radius. (The home grid's non-focused cell, verbatim.)
pub(crate) fn draw_tile(
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
    let f = ((s - 1.0) / sty.ring_denom()).clamp(0.0, 1.0);
    card(p, rect, art, rad, false, 1.0, f);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
}

/// The metadata block under a FOCUSED tile: a centred title, optionally a second caption line
/// ("S1 • E8", a year, a character name), optionally led by the Continue-Watching play glyph. Only
/// the focused tile carries it, so a row builds exactly one of these per frame.
///
/// It is a type rather than three parameters because the block's height is a fact the SCREEN needs:
/// every shelf must reserve room for it in its own flow, and `reveal`/`scroll_into_view` only keep
/// air under the block a screen *declares*. Before this, five call sites hand-authored that number
/// from constants they could not see — [`TileLabel::height`] is the answer they were restating.
pub(crate) struct TileLabel {
    pub title: Option<std::ffi::CString>,
    pub caption: Option<std::ffi::CString>,
    /// lead the title with the amber play triangle (Continue Watching's tile has no play disc)
    pub glyph: bool,
    /// **How much of this block is on screen, 0..1** — [`band_reveal`] of the row's label band.
    /// 1 for every caller whose band does not animate (the Library grid, the profile picker, the
    /// detail Related shelf), which is why `Default` gives it that.
    pub reveal: f32,
}

/// **Hand-written, not derived, for one field**: `reveal` defaults to 1 (fully on screen), where a
/// derive would give 0 and every tile in the app whose band does not animate would draw no label at
/// all. The three text fields keep the derive's answer.
impl Default for TileLabel {
    fn default() -> Self {
        TileLabel {
            title: None,
            caption: None,
            glyph: false,
            reveal: 1.0,
        }
    }
}

impl TileLabel {
    /// **The block is the label BAND's content, so it may not be on screen before the room for it
    /// is.** Chain this off a shelf's [`CardRow::band_reveal`]; see that function for the geometry
    /// the curve is solved from.
    pub(crate) fn revealed(mut self, a: f32) -> Self {
        self.reveal = a;
        self
    }
    /// Title only — the plain poster shelf.
    pub(crate) fn title(t: &str) -> Self {
        TileLabel {
            title: std::ffi::CString::new(t).ok(),
            ..Default::default()
        }
    }
    /// Title + a caption line; an empty caption is simply absent (never a blank row).
    pub(crate) fn titled(t: &str, caption: &str) -> Self {
        TileLabel {
            caption: std::ffi::CString::new(caption)
                .ok()
                .filter(|_| !caption.is_empty()),
            ..Self::title(t)
        }
    }
    /// Title led by the amber play triangle — Continue Watching's focused primary line.
    pub(crate) fn played(t: &str) -> Self {
        TileLabel {
            glyph: true,
            ..Self::title(t)
        }
    }
    /// THE height a screen must reserve under the poster row for this block. Derived from the same
    /// constants [`draw_focused`] lays it out with, so a screen's declared band and the block on
    /// screen cannot drift. The title is always exactly one line now — a title too wide for its
    /// budget marquees in place rather than growing the block, so this no longer varies with the
    /// row style at all.
    pub(crate) fn height(caption: bool) -> f32 {
        UNDER_DROP
            + UNDER_LINE_H
            + if caption {
                UNDER_LINE_GAP + UNDER_CAPTION_H
            } else {
                0.0
            }
    }
    /// [`TileLabel::height`] for a block that has whatever THIS one has.
    fn own_height(&self) -> f32 {
        Self::height(self.caption.is_some())
    }
}

/// The focused cell body — the caller draws this LAST for its z-order: the art tile, the big glow
/// ring, an optional resume bar, then its [`TileLabel`]. (The home grid's focused-last cell.)
pub(crate) fn draw_focused(
    p: Painter,
    art: Art,
    rect: Rect,
    s: f32,
    sty: &RowStyle,
    resume: Option<f32>,
    label: &TileLabel,
) {
    let rad = sty.tile_radius(rect, s);
    // Home Screen focus treatment: soft drop-shadow + 1px perimeter sheen, both FOLDED into card()'s
    // single composite pass and ramped in with the pop `f` (the same 0→1 pop scalar the ring used).
    let f = ((s - 1.0) / sty.ring_denom()).clamp(0.0, 1.0);
    card(p, rect, art, rad, false, 1.0, f);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac, rad);
    }
    // The label block anchors to the UNSCALED card bottom (rect is scaled about its center, so
    // base bottom = cy + (h/s)/2). In the mock the label is normal-flow BELOW the transformed
    // poster — a pop/press never moves it; the pop eats the poster→label gap instead of shoving
    // the label into the next shelf's title.
    let mut ty = rect.y + rect.h * 0.5 + (rect.h / s) * 0.5 + UNDER_DROP;
    // …and the block itself rides the band's reveal. The card, its ring and its resume bar do NOT:
    // they are the tile, which is on screen either way; this alpha is only about whether the room
    // under it has opened yet ([`CardRow::band_reveal`]).
    if label.reveal <= 0.0 {
        return;
    }
    let p = p.alpha(label.reveal);
    if let Some(t) = &label.title {
        // ONE title path for every focused tile in the app: a single line, elided/centred when it
        // fits and looping under a marquee when it does not — Continue-Watching's amber play glyph
        // is a parameter of the same function, not a fourth path, so a glyph-led title marquees
        // exactly like a plain one.
        title_marquee(p, rect, sty, t.as_ptr(), ty, label.glyph);
        ty += UNDER_LINE_H + UNDER_LINE_GAP;
    }
    if let Some(c) = &label.caption {
        under_label(
            p,
            rect,
            sty,
            c.as_ptr(),
            ty,
            theme::size::CAPTION,
            0,
            theme::TEXT_SECONDARY,
        );
    }
}

/// THE one-row strip loop: draw a whole horizontal shelf, non-focused tiles first (off-axis
/// culled) and the focused tile LAST for its z-order — the in-row focused-last rule, which is all
/// a lone strip needs (home's `Grid` keeps its own CROSS-row pass, which this deliberately does
/// not try to own).
///
/// This is the loop the detail page's Related and Cast rows and the person page's Movies/Shows
/// shelves all run; it lives here rather than in a screen so the culling discipline travels with
/// it. **Only tiles that pass `on_axis` are drawn, and therefore only they call `resolve_tex`** —
/// the 64-slot poster LRU must never see an off-screen request, which is exactly what a
/// hand-rolled per-screen copy of this loop keeps getting wrong.
///
/// The caller supplies the per-index content as closures: `art` the tile's [`Art`], `resume` its
/// amber progress fraction (`None` = not in progress), `label` the FOCUSED tile's [`TileLabel`]
/// (`TileLabel::default()` for a row that captions every tile through `extra` instead), and `extra`
/// any per-tile caption drawn for EVERY tile (cast names/roles). `label` is invoked for the focused
/// index only. `axis_span` widens the cull band where captions should survive slightly off-screen.
#[allow(clippy::too_many_arguments)]
pub(crate) fn strip<'a>(
    p: Painter,
    row: &CardRow,
    n: usize,
    focus_col: std::os::raw::c_int,
    row_y: f32,
    size: (f32, f32),
    pitch: f32,
    sty: &RowStyle,
    axis_span: f32,
    art: impl Fn(usize) -> Art<'a>,
    resume: impl Fn(usize) -> Option<f32>,
    label: impl Fn(usize) -> TileLabel,
    extra: impl Fn(Painter, usize, f32, bool),
) {
    let sx = row.scroll_x();
    let pr = p.translate(-sx, 0.0);
    for i in 0..n {
        if i as std::os::raw::c_int == focus_col {
            continue; // focused tile drawn last
        }
        let x = sty.margin_x + i as f32 * pitch;
        if !crate::ui::on_axis(x - sx, size.0, axis_span, 0.0) {
            continue;
        }
        let s = row.scale(i);
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        draw_tile(pr, art(i), rect, s, sty, resume(i));
        extra(pr, i, x, false);
    }
    if focus_col >= 0 && (focus_col as usize) < n {
        let i = focus_col as usize;
        let x = sty.margin_x + i as f32 * pitch;
        // fold the ui::press click dip into the focused tile's scale (1.0 when idle) — same as home
        let s = row.scale(i) * crate::ui::press::scale();
        let rect = Rect::new(x, row_y, size.0, size.1).scaled(s);
        draw_focused(
            pr,
            art(i),
            rect,
            s,
            sty,
            resume(i),
            &label(i).revealed(row.band_reveal()),
        );
        extra(pr, i, x, true);
    }
}

/// **The one trailing FACT under a focused tile** — the caption rung, for every poster shelf in the
/// app and for the Library grid.
///
/// It was `home.rs`'s, private, in two halves (`focused_caption` and `cw_caption`), and the Library
/// simply had neither: its shelves passed a bare title, so every focused poster there reserved a
/// two-rung block and drew one rung into it. The owner reported that as ragged rows
/// (2026-09-05, "some rows have only one text label without detail label"), and the fix is the one
/// this system asks for — share the rule, not shrink the box: `ROW_PITCH` reserves
/// [`UNDER_LABEL_H`] for every shelf on both screens, so shrinking per shelf would make two rows of
/// the same object different heights and leave the Library's grid disagreeing with the shelves
/// above it.
///
/// **`is_continue` is the SHELF's fact, not the item's.** A deck says how much is left because that
/// is what you came for; everywhere else the item identifies itself.
///
/// **The year is the fallback for ANY kind, which is the one behaviour change in the promotion.**
/// Home's version restricted it to `kind == 0`, so a SHOW poster — the whole of a TV library's
/// "Recently Added" — captioned nothing on either screen. The Library's own grid has always
/// captioned a show with its year, so this makes the two agree rather than inventing a rule: on
/// one screen, one object, one answer.
///
/// `None` is still possible and still means one rung: an item the server dated to nothing at all.
pub(crate) fn focused_caption(m: &crate::pms::PmsMovie, is_continue: bool) -> Option<std::ffi::CString> {
    if is_continue {
        return cw_caption(m);
    }
    let s = if m.kind == 3 && m.ep_index > 0 {
        if m.season_index > 0 {
            format!("S{} \u{2022} E{}", m.season_index, m.ep_index)
        } else {
            format!("E{}", m.ep_index)
        }
    } else if m.year > 0 {
        m.year.to_string()
    } else {
        return None;
    };
    std::ffi::CString::new(s).ok()
}

/// The focused Continue-Watching card's secondary line (Home Screen.dc): an in-progress item reads
/// "<show> · 8 min left" (episodes) or just the time-remaining (a resumed movie); a next-up episode
/// (no resume point yet) reads "<show> · New episode". `title` above it carries the episode name.
///
/// "In progress" is [`PmsMovie::resume_frac`] — THE resume rule, and the same call `Grid::draw`
/// makes for the BAR in the pass that asks for this caption. A hand-written
/// `resume_ms > 0 && dur_ns > 0` here dropped that rule's end-guard, so a finished item whose
/// server never cleared `viewOffset` drew NO bar (`resume_frac`'s answer) under a caption still
/// promising "1 min left" — `fmt::time_left` floors at a minute, so it could not even read zero.
/// One item, described two ways in one draw. Whether that tile also wore the watched TICK is a
/// separate question with a separate answer: `widgets::poster_mark` reads `PmsMovie::watched`, so
/// a stale past-end `viewOffset` on an item the server has not marked watched drew no mark at all.
fn cw_caption(m: &crate::pms::PmsMovie) -> Option<std::ffi::CString> {
    let show = m.show_title.as_str();
    let s = if m.resume_frac().is_some() {
        let left = crate::ui::fmt::time_left(m.dur_ns / 1_000_000 - m.resume_ms);
        if m.kind == 3 && !show.is_empty() {
            format!("{show} \u{00b7} {left}")
        } else {
            left // a resumed movie: time-remaining alone
        }
    } else if m.kind == 3 {
        // next-up episode: no resume point, so no bar and no time — just the "New episode" cue
        if show.is_empty() {
            "New episode".to_string()
        } else {
            format!("{show} \u{00b7} New episode")
        }
    } else {
        return None;
    };
    std::ffi::CString::new(s).ok()
}

// The under-tile metadata block's four metrics — the ONLY authority on its geometry, which
// [`TileLabel::height`] hands back to the screens that must reserve room for it.
/// poster bottom → the title block's first line.
const UNDER_DROP: f32 = 30.0;
/// one title line's pitch (`Person Screen v2.dc.html`'s `line-height: 30px`).
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
pub(crate) const UNDER_LABEL_H: f32 = UNDER_DROP + UNDER_LINE_H + UNDER_LINE_GAP + UNDER_CAPTION_H;

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
pub(crate) const LABEL_BAND_COLLAPSED: f32 =
    crate::ui::theme::space::LG - crate::ui::consts::UNDER_LABEL_AIR;

/// The label band a shelf reserves at expansion `e` (0 collapsed → 1 focused) — **the ONE
/// expression every screen's row pitch is built from**, so Home, the Library, Search and the person
/// page cannot collapse by three different amounts.
///
/// A screen's pitch is its own fixed part plus this: for a poster shelf that fixed part is
/// `consts::ROW_PITCH - UNDER_LABEL_H`, and `under_band(1.0)` is `UNDER_LABEL_H` exactly, so a
/// focused shelf is byte-identical to the geometry that existed before the band could close.
pub(crate) fn under_band(e: f32) -> f32 {
    LABEL_BAND_COLLAPSED + (UNDER_LABEL_H - LABEL_BAND_COLLAPSED) * e.clamp(0.0, 1.0)
}

/// **How much of the focused label block may be on screen at expansion `e`** — and this is solved
/// from the collision it exists to prevent rather than picked by eye.
///
/// The block is drawn at a FIXED offset under its card the instant its shelf takes focus, while the
/// room for it opens over the band's whole travel. So for the first part of every focus move the
/// caption sits on top of the NEXT shelf's heading — which is exactly what the first simulator pass
/// showed, "Recently Added Movies" printed through "WeCrashed · 35 min left".
///
/// The two distances, both measured down from this shelf's card bottom: the next heading's cap top
/// is at `UNDER_LABEL_AIR + under_band(e)`, and the block's own caption ends at [`UNDER_LABEL_H`].
/// So the room HOLDS the block from `under_band(e) >= UNDER_LABEL_H - UNDER_LABEL_AIR` onward, and
/// the reveal is 0 until then and smoothsteps over what is left of the travel. There is therefore
/// no expansion at which any part of the block is drawn over the heading below it — which a bare
/// "fade it in" would not have given, only made fainter.
///
/// The shape it produces is the reference product's own: a beat while the room opens, then the
/// title fading up. On the way out it is the reverse and it is FAST — the block is gone inside the
/// first third of the collapse, well before the shelf below arrives where it was.
pub(crate) fn band_reveal(e: f32) -> f32 {
    let room = crate::ui::consts::UNDER_LABEL_AIR + under_band(e);
    let holds = UNDER_LABEL_H; // the room at which the block is fully clear of the next heading
    let settled = crate::ui::consts::UNDER_LABEL_AIR + UNDER_LABEL_H;
    let t = ((room - holds) / (settled - holds)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The fixed half of a poster shelf's pitch — everything `consts::ROW_PITCH` is made of EXCEPT the
/// label band. Add [`under_band`] to it.
pub(crate) const ROW_PITCH_FIXED: f32 = crate::ui::consts::ROW_PITCH - UNDER_LABEL_H;

/// How much a shelf GROWS when it takes focus — the travel of [`under_band`], and what one focus
/// move costs (or reclaims) in document height.
pub(crate) const BAND_OPEN: f32 = UNDER_LABEL_H - LABEL_BAND_COLLAPSED;

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
pub(crate) fn settled_top(r: usize, focus: Option<usize>, pitch_fixed: f32) -> f32 {
    r as f32 * (pitch_fixed + LABEL_BAND_COLLAPSED)
        + if focus.is_some_and(|f| f < r) {
            BAND_OPEN
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

/// How long the run rests before it starts gliding — long enough to read the opening words of an
/// ordinary title before it moves.
const MARQUEE_HOLD_MS: f32 = 1000.0;
/// Glide speed, in px/s. Chosen to be readable from a couch rather than merely legible paused — a
/// scrolling news-ticker speed blurs on a TV's own motion smoothing.
const MARQUEE_SPEED: f32 = 40.0;
/// Air between the outgoing run's tail and its follower's head — enough that the two never read as
/// one run with a repeated word running into itself.
const MARQUEE_GAP: f32 = 60.0;

/// The marquee's horizontal offset at `t_ms` since this run became (or stayed) the focused title,
/// for a run `text_w` px wide inside a `budget` px window.
///
/// A pure function of elapsed time, which is what makes it trivially unit-testable and trivially
/// resettable — the caller just starts `t_ms` back at zero. `0` for the first [`MARQUEE_HOLD_MS`]
/// (the run sits at rest, left edge in the window), then glides at [`MARQUEE_SPEED`] until the run
/// plus [`MARQUEE_GAP`] of air has fully passed, then loops. A run that already fits (`text_w <=
/// budget`) is inert at every `t_ms` — the caller should not even reach here for one, but this
/// stays harmless if it does.
///
/// Draw TWO copies at `x - offset` and `x - offset + text_w + MARQUEE_GAP` (the follower), both
/// clipped to `budget` — the follower is what makes the wrap seamless instead of a visible pop back
/// to the start: by the time `offset` reaches the full travel distance the follower has arrived
/// exactly where the primary run started, so the loop boundary is invisible.
fn marquee_x(t_ms: f32, text_w: f32, budget: f32) -> f32 {
    if text_w <= budget {
        return 0.0;
    }
    let period = marquee_period(text_w, budget);
    let t = t_ms.max(0.0) % period;
    if t < MARQUEE_HOLD_MS {
        0.0
    } else {
        (t - MARQUEE_HOLD_MS) / 1000.0 * MARQUEE_SPEED
    }
}

/// Is the marquee actually GLIDING at `t_ms` — i.e. is this the frame that must keep
/// [`crate::ui::idle`] awake? `false` during the rest beat and whenever the run fits, so a settled
/// screen full of short (or currently-resting) titles still meets the idle present gate's fps
/// ceiling — see [`title_marquee`]'s doc for why this has to be a separate question from
/// [`marquee_x`] rather than "moved since last frame": the rest beat's `offset == 0` is not motion,
/// but it is also not the screen being settled in the sense the gate cares about — nothing is
/// drawn differently between two resting frames, which is exactly what should NOT report.
fn marquee_moving(t_ms: f32, text_w: f32, budget: f32) -> bool {
    if text_w <= budget {
        return false;
    }
    t_ms.max(0.0) % marquee_period(text_w, budget) >= MARQUEE_HOLD_MS
}

/// One full marquee cycle in ms — the rest beat plus the glide that carries the run and its
/// [`MARQUEE_GAP`] of air fully past. The ONE place the period is spelled; [`marquee_x`],
/// [`marquee_moving`] and [`marquee_phase`] all fold on it.
fn marquee_period(text_w: f32, budget: f32) -> f32 {
    let _ = budget; // a fitting run never reaches here; the period is a property of the run alone
    let travel = text_w + MARQUEE_GAP; // one full cycle's glide distance
    MARQUEE_HOLD_MS + travel / MARQUEE_SPEED * 1000.0
}

/// Fold the `f64` clock onto one period BEFORE it becomes an `f32`. The accumulator is `f64` so
/// it never stops advancing, but an `f32` of a six-day-old clock (~2^29 ms) still only moves in
/// 64 ms steps — which at [`MARQUEE_SPEED`] is a 2.5 px judder at 15 Hz rather than a glide
/// (Codex review, 2026-09-02). A phase is never larger than one period, so it is exact in `f32`.
fn marquee_phase(t_ms: f64, text_w: f32, budget: f32) -> f32 {
    if text_w <= budget {
        return 0.0;
    }
    (t_ms.max(0.0) % marquee_period(text_w, budget) as f64) as f32
}

thread_local! {
    /// Which title currently owns the marquee clock below, and since when. Only one tile can hold
    /// focus app-wide, so ONE clock is enough — keyed by the drawn TEXT rather than a catalog id,
    /// because `draw_focused` is called from five sites across three lanes' files (home, library,
    /// detail, person, profiles) with no uniform notion of "item identity" to key on. A coincidental
    /// identical title on a genuinely different item simply keeps the marquee running rather than
    /// resetting it, which is invisible — the two runs read the same either way.
    static MARQUEE_KEY: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());
    /// Elapsed ms since `MARQUEE_KEY` last changed, advanced from [`crate::ui::idle::dt`] because
    /// this runs inside `draw`, which — unlike [`CardRow::update`] — gets no `dt` of its own.
    /// `f64`: an `f32` accumulator stops moving at ~2^29 ms (six days on one title), where a
    /// 16 ms frame is below its precision floor.
    static MARQUEE_MS: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
}

/// Advance (or restart) the marquee clock for `text`, returning its value in ms. Called once per
/// frame from [`title_marquee`], which is called at most once per frame (the focused tile is drawn
/// exactly once) — so this cannot double-advance within a frame the way a naively-shared clock read
/// from two draws in the same pass would.
fn marquee_clock(text: &str) -> f64 {
    let changed = MARQUEE_KEY.with(|k| {
        let mut k = k.borrow_mut();
        if k.as_str() == text {
            false
        } else {
            *k = text.to_string();
            true
        }
    });
    if changed {
        MARQUEE_MS.with(|m| m.set(0.0));
        0.0
    } else {
        MARQUEE_MS.with(|m| {
            let v = m.get() + crate::ui::idle::dt() as f64 * 1000.0;
            m.set(v);
            v
        })
    }
}

/// Air between Continue-Watching's play glyph and the name that follows it.
const PLAY_ICON_GAP: f32 = 10.0;

/// The Continue-Watching glyph's pixel size at `sz` — 72% of the title's own size, rounded (Home
/// Screen.dc's play-triangle proportion). Pure so [`title_marquee`] and [`play_label_fit`] read the
/// same number rather than each rounding it themselves.
#[inline]
fn play_icon_size(sz: std::os::raw::c_int) -> f32 {
    (sz as f32 * 0.72).round()
}

/// How much of the marquee's window a glyph-led title gives up to the icon + its gap — `0.0` for a
/// plain title. Pulled out to its own pure function so a host test can pin the arithmetic
/// [`title_marquee`]'s overflow branch and [`play_label_fit`]'s fitting branch both depend on,
/// without a `Painter` to draw through.
#[inline]
fn glyph_lead(sz: std::os::raw::c_int, glyph: bool) -> f32 {
    if glyph {
        play_icon_size(sz) + PLAY_ICON_GAP
    } else {
        0.0
    }
}

/// The focused tile's single-line title: the plain elided [`under_label`] whenever the run fits the
/// widened [`under_budget`], else a looping [`marquee_x`] — see the section doc above for why.
/// Reports to [`crate::ui::idle`] only on a frame the marquee is actually gliding, so a screen full
/// of short (or resting) titles costs the present gate nothing.
///
/// `glyph` leads the line with Continue-Watching's amber play triangle — the SAME clock and the
/// SAME budget arithmetic as the plain title, just with the icon's width plus its gap subtracted
/// from the window before anything is measured against it, so a played title that overflows
/// marquees exactly like any other. This is ONE implementation with a glyph parameter rather than a
/// fourth title path: before it, the glyph line was a dead end that never fell through to a
/// marquee at all, so a long Continue-Watching title elided instead of animating like every other
/// shelf's focused label.
fn title_marquee(p: Painter, rect: Rect, sty: &RowStyle, text: *const c_char, y: f32, glyph: bool) {
    let (sz, bold) = (theme::size::LABEL, 1);
    let isz = play_icon_size(sz);
    let lead = glyph_lead(sz, glyph);
    let full = under_budget(sty);
    let budget = full - lead;
    let w = crate::text::text_width(text, sz, bold);
    if w <= budget {
        // a fitting title RELEASES the clock, so an overflowing one focused again later starts
        // from its rest beat rather than resuming mid-glide
        MARQUEE_KEY.with(|k| k.borrow_mut().clear());
        if glyph {
            play_label_fit(p, rect, sty, text, y, isz, sz, bold);
        } else {
            under_label(p, rect, sty, text, y, sz, bold, theme::TEXT_PRIMARY);
        }
        return;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let t_ms = marquee_phase(marquee_clock(&s), w, budget);
    // The clock above advances by `idle::dt` ON DRAWN FRAMES ONLY, and a drawn frame is one the
    // present gate let through. So the rest beat has to buy its own frames, or it never ends: a
    // focused overflowing title was reproduced sitting clipped and motionless at 4 s and again at
    // 7 s of focus (sim, 2026-09-02) — the screen settled inside the hold, presents stopped, the
    // clock froze at a few hundred ms and only the 2 s keepalive ever nudged it, one 1/60 s tick
    // at a time. `wake` is the right half of the pair for the hold (a frame, with no claim that
    // pixels changed); `invalidate` is for the glide, where they do. An overflowing focused title
    // therefore never lets the screen rest — which is what "marquee while focused" means, and why
    // a fitting title must never reach this branch.
    if marquee_moving(t_ms, w, budget) {
        crate::ui::idle::invalidate();
    } else {
        crate::ui::idle::wake();
    }
    let off = marquee_x(t_ms, w, budget);
    let travel = w + MARQUEE_GAP;
    // The [glyph? + text-window] group is centred as ONE block, the same screen-space clamp the
    // other blocks use — see [`edge_clamp`]. The glyph, when present, sits fixed at the group's left
    // edge; only the text window inside it scrolls.
    let (x0, _) = label_band(p, rect, sty);
    let text_x0 = x0 + lead;
    if glyph {
        let (ct, cb) = crate::text::text_cap_band(sz, bold);
        let icy = y + (ct + cb) * 0.5; // centre the glyph on the name's cap band
        crate::ui::icons::draw(
            p,
            crate::ui::icons::Icon::Play,
            Rect::new(x0, icy - isz * 0.5, isz, isz),
            theme::RESUME_FILL,
        );
    }
    p.clip(Rect::new(text_x0, y - 6.0, budget, UNDER_LINE_H + 12.0));
    p.text(text, text_x0 - off, y, sz, theme::TEXT_PRIMARY, 0, bold);
    p.text(text, text_x0 - off + travel, y, sz, theme::TEXT_PRIMARY, 0, bold);
    p.clip_clear();
}

/// The under-tile text budget: the tile, a gap either side, PLUS half of each neighbouring tile's
/// width (item 10) — a focused label may bleed into the gap and up to the midpoint of the next
/// tile without visually colliding with it, which is what makes a real Plex title ("Wallace &
/// Gromit: The Curse of the Were-Rabbit") legible instead of "Wallace & Gr…". Two neighbours
/// contributing half their width each is one extra tile-width overall, hence `+ sty.w` once more
/// on top of the plain tile-plus-gaps budget. Still edge-clamped by [`edge_clamp`] — widening the
/// budget never lets a block run off the panel, it only lets it reach further into open space
/// before [`title_marquee`] takes over for whatever still does not fit.
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
/// width, in `p`'s own space. The ONE expression for that extent, so a screen with a constraint on
/// it can grade the block that is DRAWN rather than re-deriving the centring and the clamp.
///
/// It is the WIDEST case on purpose: [`under_budget`], which is what [`title_marquee`] reserves
/// before it knows whether the name fits. An elided caption occupies a sub-rect of this, so a bound
/// this block satisfies is one every line under the tile satisfies.
pub(crate) fn label_band(p: Painter, rect: Rect, sty: &RowStyle) -> (f32, f32) {
    let full = under_budget(sty);
    (edge_clamp(p, rect.cx() - full * 0.5, full, sty), full)
}
/// Air kept between an edge tile's label block and the panel edge.
const EDGE_PAD: f32 = 16.0;

/// Keep a `w`-wide block whose left edge is `x` inside the PANEL, and answer in `p`'s own space.
///
/// **The clamp is a screen fact and `x` usually is not.** [`strip`] draws a shelf through
/// `translate(-scroll_x, 0)`, so every rect below it is a CONTENT coordinate; comparing one against
/// `SCR_W` is comparing two different spaces, and the moment a row had scrolled about a panel's
/// width the clamp bound bit on every column at once — the focused tile's title and caption froze
/// at a fixed screen x (each pinned at its own bound, since the bound depends on the run's width)
/// while the words inside them still changed with focus. Device-observed on Search's episode shelf
/// 2026-08-15, and latent in every scrolling shelf in the app: `detail`'s Related and Cast rows and
/// `person`'s two shelves all run `strip` and all pass through here.
///
/// `p.dx()` converts, so the comparison happens in screen space and the result comes back in the
/// painter's. An untranslated caller (`home`, `library`, `profiles` draw their focused tile
/// directly) has `dx == 0` and is unchanged to the pixel.
fn edge_clamp(p: Painter, x: f32, w: f32, sty: &RowStyle) -> f32 {
    let lo = EDGE_PAD - p.dx();
    // The right bound is the panel's edge less whatever the SCREEN has reserved there
    // (`RowStyle::right_reserve` — the Library's A–Z rail, and nothing else today). The left has no
    // twin: no screen reserves a left band, and inventing a symmetric knob nobody sets would be a
    // second way to be wrong about a rule that has exactly one exception.
    //
    // A block WIDER than the band has no position that satisfies both edges; `lo` wins, which
    // pins its left edge and lets the overflow run off the right — the same end `elide` and the
    // wrap budget are already cutting.
    let hi = SCR_W - sty.right_reserve - w - EDGE_PAD - p.dx();
    (x).clamp(lo, hi.max(lo))
}

/// One centered metadata line under a focused tile: elided to the tile-plus-gaps budget and kept
/// inside the screen edges — a long episode title under an edge tile used to run off the panel.
fn under_label(
    p: Painter,
    rect: Rect,
    sty: &RowStyle,
    text: *const c_char,
    y: f32,
    sz: std::os::raw::c_int,
    bold: std::os::raw::c_int,
    col: [f32; 4],
) {
    let budget = under_budget(sty);
    let s = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let short = crate::text::elide(&s, budget, sz, bold, false);
    if let Ok(tc) = std::ffi::CString::new(short) {
        let w = crate::text::text_width(tc.as_ptr(), sz, bold);
        // Same screen-space clamp as the wrapped title's — see [`edge_clamp`]. Expressed on the
        // run's LEFT edge and converted back, so one function owns the rule for both blocks.
        let cx = edge_clamp(p, rect.cx() - w * 0.5, w, sty) + w * 0.5;
        p.text(tc.as_ptr(), cx, y, sz, col, 1, bold);
    }
}

/// The focused Continue-Watching card's primary line when the name FITS without a marquee: an amber
/// play triangle followed by the episode/movie name, the [icon + gap + name] group centred under the
/// tile (Home Screen.dc — the play affordance lives here, not as a disc on the poster). `budget` is
/// already the text-only window [`title_marquee`] measured against (the icon and its gap already
/// subtracted), so this only re-elides defensively — the caller already knows the run fits.
fn play_label_fit(
    p: Painter,
    rect: Rect,
    sty: &RowStyle,
    text: *const c_char,
    y: f32,
    isz: f32,
    sz: std::os::raw::c_int,
    bold: std::os::raw::c_int,
) {
    // The run already fits its window (the caller checked), so it is drawn verbatim — no elide,
    // no copy.
    let tw = crate::text::text_width(text, sz, bold);
    let gw = isz + PLAY_ICON_GAP + tw;
    // The [glyph + gap + name] group as ONE block, clamped in screen space like the other two —
    // see [`edge_clamp`].
    let gl = edge_clamp(p, rect.cx() - gw * 0.5, gw, sty);
    let (ct, cb) = crate::text::text_cap_band(sz, bold);
    let icy = y + (ct + cb) * 0.5; // centre the glyph on the name's cap band
    crate::ui::icons::draw(
        p,
        crate::ui::icons::Icon::Play,
        Rect::new(gl, icy - isz * 0.5, isz, isz),
        theme::RESUME_FILL,
    );
    p.text(text, gl + isz + PLAY_ICON_GAP, y, sz, theme::TEXT_PRIMARY, 0, bold);
}

/// Full-bleed resume bar: the bottom band of the card itself (Continue Watching). Delegates to
/// [`crate::ui::widgets::progress_bar`], which is the ONE bar the whole app draws — a poster shelf and
/// the detail page's episode filmstrip now make the identical call, and its doc carries the two
/// pixel-level rules (snapped band, disjoint track/fill) that a hand-rolled copy here kept getting
/// subtly wrong. Only the HEIGHT is local, because it is poster-relative.
pub(crate) fn resume_bar(p: Painter, r: Rect, frac: f32, rad: f32) {
    // 5px on a resting poster (and it rides the focus pop): the band's bottom ~1.5px are the card
    // edge's own SDF AA ramp, so a 4px band reads ~2.5px — 5px leaves the mock's ~4px of solid color.
    let bh = (r.h * 5.0 / 375.0).max(4.0);
    crate::ui::widgets::progress_bar(p, r, rad, bh, frac);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    /// The full clearance a HOME shelf's heading needs over a popped tile — asked of
    /// [`heading_lift_max`] rather than restating its three terms, because a screen now RESERVES
    /// this same number in its layout (`ui::home`'s `row_reveal_band`) and a test that re-derived it
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
        assert_eq!(marquee_x(0.0, 100.0, 300.0), 0.0);
        assert_eq!(marquee_x(50_000.0, 100.0, 300.0), 0.0);
        assert!(!marquee_moving(0.0, 100.0, 300.0));
        assert!(!marquee_moving(50_000.0, 100.0, 300.0));
    }

    /// An over-wide run sits at rest for the whole hold beat: `t_ms < MARQUEE_HOLD_MS` must read
    /// zero offset and report no motion, so a title lands and holds still long enough to start
    /// reading before anything moves.
    #[test]
    fn an_overflowing_title_rests_before_it_glides() {
        let (w, budget) = (500.0, 300.0);
        assert_eq!(marquee_x(0.0, w, budget), 0.0);
        assert_eq!(marquee_x(MARQUEE_HOLD_MS - 1.0, w, budget), 0.0);
        assert!(!marquee_moving(0.0, w, budget));
        assert!(!marquee_moving(MARQUEE_HOLD_MS - 1.0, w, budget));
    }

    /// Past the hold beat it glides at the documented speed and reports motion — and the offset is
    /// monotone through the glide (no snap-back mid-cycle).
    #[test]
    fn an_overflowing_title_glides_at_the_documented_speed_and_reports_motion() {
        let (w, budget) = (500.0, 300.0);
        assert!(marquee_moving(MARQUEE_HOLD_MS + 1.0, w, budget));
        let a = marquee_x(MARQUEE_HOLD_MS + 100.0, w, budget);
        let b = marquee_x(MARQUEE_HOLD_MS + 600.0, w, budget);
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
        let travel = w + MARQUEE_GAP;
        let glide_ms = travel / MARQUEE_SPEED * 1000.0;
        let period = MARQUEE_HOLD_MS + glide_ms;
        assert_eq!(marquee_x(period, w, budget), marquee_x(0.0, w, budget));
        assert_eq!(
            marquee_x(period + 50.0, w, budget),
            marquee_x(50.0, w, budget)
        );
        // just before the wrap the run has travelled (almost) the full distance
        let just_before = marquee_x(period - 0.001, w, budget);
        assert!(
            (just_before - travel).abs() < 0.1,
            "got {just_before} vs travel {travel}"
        );
    }

    /// [`marquee_clock`] restarts at zero the instant the focused text changes, and keeps
    /// advancing by real elapsed time while it stays the same — the pure half of "the phase clock
    /// resets when focus moves to another tile" (the impure half, reading `idle::dt`, is not
    /// unit-testable and is exercised by [`title_marquee`] instead).
    #[test]
    fn the_marquee_clock_restarts_when_the_focused_text_changes() {
        MARQUEE_KEY.with(|k| k.borrow_mut().clear());
        MARQUEE_MS.with(|m| m.set(0.0));
        assert_eq!(marquee_clock("Alpha"), 0.0, "first sight of a title starts at 0");
        // idle::dt() defaults to 1/60s on a thread that never called frame_begin — advancing while
        // the key is unchanged must add real, nonzero time.
        let t1 = marquee_clock("Alpha");
        assert!(t1 > 0.0, "the clock must advance while the title holds focus");
        let t2 = marquee_clock("Alpha");
        assert!(t2 > t1, "and keep advancing frame over frame");
        assert_eq!(
            marquee_clock("Beta"),
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
    /// number [`play_label_fit`]'s fitting branch and [`title_marquee`]'s overflow branch both
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
        assert!(!marquee_moving(0.0, w, full_budget), "plain: still resting, not overflowing");
        assert!(
            marquee_moving(MARQUEE_HOLD_MS + 1.0, w, glyph_budget),
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
        assert!(TileLabel::height(false) < TileLabel::height(true));
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
        use crate::ui::consts::{CARD_DY, CARD_H, TITLE_DY, UNDER_LABEL_AIR};
        let pitch = ROW_PITCH_FIXED + under_band(0.0);
        let card_bottom = CARD_DY + CARD_H;
        let next_heading = pitch - TITLE_DY;
        assert_eq!(
            next_heading - card_bottom,
            crate::ui::theme::space::LG,
            "an unfocused shelf owes its neighbour one region gap and nothing more"
        );
        // …and that gap is exactly what the block's own trailing air grew into
        assert_eq!(
            LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR,
            crate::ui::theme::space::LG
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
        assert_eq!(fixed + under_band(1.0), crate::ui::consts::ROW_PITCH);
    }

    /// **The animator obligation** (`ui/CLAUDE.md`): the band must report to the frame gate while it
    /// is travelling and stop reporting once it has arrived. An over-reporting one costs the whole
    /// present-gate saving with every `fps_floor` in the suite still green, so the REST half is the
    /// one that is only ever caught here.
    #[test]
    fn the_label_band_reports_while_it_opens_and_goes_quiet_once_it_has() {
        let _g = crate::testlock::serial();
        let mut row = CardRow::new();
        let sty = RowStyle::HOME;

        // opening: the very first step has to be seen by the gate
        let (_, moved) = crate::ui::idle::scoped_motion(|| {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        });
        assert!(moved, "an opening band must wake the present gate");
        assert!(row.band_expand() > 0.0);

        // …and it arrives
        for _ in 0..240 {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        }
        assert!((row.under_band() - UNDER_LABEL_H).abs() < 0.5);
        let (_, still) = crate::ui::idle::scoped_motion(|| {
            row.update(6, Some(0), &sty, 1.0 / 60.0);
        });
        assert!(!still, "a settled open band must not keep the panel awake");

        // and the same on the way back down
        for _ in 0..240 {
            row.update(6, None, &sty, 1.0 / 60.0);
        }
        assert!((row.under_band() - LABEL_BAND_COLLAPSED).abs() < 0.5);
        let (_, still) = crate::ui::idle::scoped_motion(|| {
            row.update(6, None, &sty, 1.0 / 60.0);
        });
        assert!(!still, "a settled collapsed band must not keep the panel awake");
    }
}
