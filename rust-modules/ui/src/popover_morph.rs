//! The **morph** open style: a glass opener (a pill) grows into its panel instead of fading and
//! sliding in on [`crate::popover::K_APPEAR`]. Selected per popover (see
//! [`crate::popover::Popover::morphing`], the ONE selector; open it with
//! [`crate::popover::Popover::open_from`] and the opener's rect, and `Popover::panel` lays the glass
//! at the morphing rect and radius on the one fill ramp below). The item hold menu is not a morph: it keeps the opener lift and the shared appear.
//!
//! **Every header menu (the Library's, the Watchlist's Sort & Filter, the profile menu) morphs the
//! GLASS, and the opener is CONSUMED by it** (owner decision, 2026-11-17, which REVERSES design
//! review S11/S12's "the opener is never part of the morph"). The host's opener lift still paints
//! the opener above the scrim and the panel, but at [`Frame::opener_alpha`]: it fades out as the
//! glass detaches from it on open and fades back in as the droplet merges into it on close. The
//! morph draws no opener face of its own; it hands the glass a MERGE shape ([`Frame::merge`], the
//! opener's own rect shrinking to nothing) that the glass shader joins to the panel with a smooth
//! minimum, so the panel looks pinched off the button and drained back into it.
//!
//! Source: design board `Motion`, card "Sort & Filter" (end state `WatchlistOptions`): the label
//! pill becomes a panel that fits its rows while the label dissolves into them. The board states
//! the springs this module names; where it disagrees with the brief the brief wins (recorded on
//! the constants below).
//!
//! **The open is UNDERDAMPED, fitted to a measurement.** The owner's verdict on the first critically
//! damped morph was "too fast" (it was still after ~150 ms, with its rows done by ~80), and the
//! owner's screen recording of an iOS 26 glass menu (the Claude app's "..." menu, 60 fps, measured
//! frame by frame against the static page, 2026-11-17; this SUPERSEDES the earlier Photos-menu
//! reference that saw no overshoot) shows why: the glass's height overshoots by about 9% of its
//! travel (peaking ~150 ms), its width lags the height by about two frames and overshoots by about
//! 4% (peaking ~220 ms), and both settle slowly to rest by ~400 ms. The width and height are
//! therefore separate UNDERDAMPED springs ([`K_HEIGHT`]/[`ZETA_HEIGHT`], [`K_WIDTH`]/[`ZETA_WIDTH`]
//! after a [`WIDTH_LAG`]), fitted to that table (minimising the largest per-frame error, t = 0 on the
//! first frame the glass draws; a single second-order spring cannot trace the slow tail, so the fit
//! keeps the overshoot a little under the measured one: 6% and 4% against 9% and 4%, within 0.06 of
//! the table on every frame, `the_open_follows_the_ios_reference_curve`). The rect is larger than
//! the panel while it rings.
//!
//! **The glass is a round LENS, and the rows are magnified by it** (same recording, 2026-11-17,
//! `series.md` "Iteration 2"). While it is small the corner radius is half its smaller side, a
//! capsule, not the panel's radius; at [`SHAPE_AT`] (~150 ms) a spring ([`K_SHAPE`]) relaxes it to the
//! caller's radius by ~250 ms, landing on it exactly ([`Frame::radius`]; a close rounds it back as it
//! shrinks). The ROWS have their own underdamped spring, [`K_LENS`]/[`ZETA_LENS`] (~0.2 when they first
//! show, on the third glass frame, 0.65 at 67 ms, a 12% overshoot at ~167 ms, 1 by ~360, within 0.02 of
//! the table), not the glass's height ratio ([`Frame::rows_scale`]); on a close they shrink with the slab. The rows are cut by the
//! lens itself, its rect and radius, through the shared painter's rounded clip
//! (`ClipScope::open_rounded_in`: a coverage mask on the rect, text and icon programs), so a letter
//! the round edge crosses is cut by the arc as on iOS 26; settled, nothing is cut. A stiff spring
//! ([`K_OUT`]) carries the CLOSE's first phase, the rows leaving; the glass keeps its frost to the
//! hand-back. The old rows spring ([`K_ROWS`]) remains for the fade+scale fallback only.
//!
//! **One surface, two clocks.** The pill and the panel are ONE surface whose rect, radius and fill
//! animate. The glass's cross-in is a function of the HEIGHT alone (it leads, so the glass draws from
//! the first step: iOS shows it on frame 0, a thin drop extending below the button); everything else
//! but the close's `out` is a function of [`Frame::progress`], the slower of the two rect springs
//! (clamped to 0..1: 0 = the opener's rect, 1 = the panel, which the rect passes while it rings):
//!
//! - the glass and its SHADOW cross in together over the first [`FILL_AT`] of the HEIGHT, a
//!   smoothstep ([`Frame::ground_alpha`]): the ONE ramp, so a rect that has left the opener is
//!   never opaque (an opaque first frame hung a slab ~25 px below the pill, N-b);
//! - the OPENER fades out over the first [`OPENER_OUT_AT`] ([`Frame::opener_alpha`]) and the glass
//!   is JOINED to a shrinking copy of the opener's rect ([`Frame::merge`]) until [`MERGE_END_AT`],
//!   so the glass reads as detaching from the button rather than appearing over it;
//! - the glass is **never clear** (owner decision, 2026-11-18, from the same recording: the droplet
//!   is not transparent while it moves, the page behind it is blurred and washed from its first
//!   visible frame): its body is the menu's FROST under the dark face (`popover_glass`) on every
//!   frame of the open and of the close, until the close's hand-back, and the morph hands the glass
//!   no clarity at all ([`Frame::glass_morph`]). The rim and its refraction are the settled glass's
//!   and the shape morph is untouched. Where the frost does not stand yet (the first frame, before the
//!   field has latched, and any frame the build is held back) `popover_glass` wears the field under
//!   the heavier face, never the sharp page;
//! - the ROWS ([`Frame::content_alpha`]) start at [`ROWS_FROM_AT`], on the third glass frame
//!   (~33 ms; iOS shows them at 67-100 and the owner asked for them earlier), and are opaque by
//!   [`ROWS_OPAQUE_AT`] (~120 ms), never more opaque than the glass they stand on. They are scaled by
//!   the lens spring ([`Frame::rows_scale`]) and cut by the surface's round lens
//!   (`Popover::morph_rows`), so no row ever draws outside the rounded glass;
//! - the modal DIM ([`Frame::dim`], [`dim_at`]) follows the progress too (in by ~150 ms), through
//!   `Popover::dim` and `Scrim::following`, instead of the container's own appear spring.
//!
//! **A close is NOT the open reversed**; it is a droplet that drains back into the opener, retimed to
//! the same recording's close (60 fps; the hand-back is at ~233 ms: iOS has the button whole at ~200 and
//! the droplet gone by ~250). The width and the height are separate springs: the width ([`K_CLOSE_W`]) reaches the
//! opener's width by ~133 ms, the height ([`K_CLOSE_H`]) bulges ~7% of its travel at ~33 ms
//! ([`BULGE_KICK_H`]) and then drains over ~250 ms (`the_close_follows_the_ios_reference_curve`):
//! 1. the slab BULGES outward, then shrinks; the rows leave over about 50 ms
//!    ([`Frame::content_alpha`] rides the `out` spring, [`K_OUT`], gone by ~67 ms), shrinking WITH
//!    the slab ([`Frame::rows_scale`]), leaving the frosted glass, which stays frosted (dark) all the
//!    way into the opener, while the corner rounds back to a capsule ([`K_SHAPE`]);
//! 2. the frosted slab A springs from the panel toward the DRAIN rect ([`drain_rect`]): just BELOW the
//!    opener, centred on it, its height and width ([`DRAIN_WIDTH`]), a capsule, [`DRAIN_GAP`] under
//!    it (closer than [`MERGE_K`], so the glass can bridge it). The opener re-forms as its OWN blob B,
//!    growing up from the opener's bottom edge as A closes in ([`BLOB_BORN`] to [`REFORM_FROM`]), the
//!    same [`Frame::merge`] shape the open shrinks;
//! 3. the fold ([`K_FOLD`], released in the last fifth of the height's travel, [`FOLD_AT`], ~175 ms: A is
//!    still travelling and the drawn height follows the table's tail): B grows to the opener's full
//!    rect while A narrows to nothing, and drains its height, toward its own top-centre, draining UP into B through the smooth-minimum neck, and the opener's own face fades
//!    back in over the last of it ([`OPENER_BACK_FROM`]). **B whitens as it re-forms**: its face blends
//!    from the glass toward the opener's FOCUSED fill ([`Merge::tint`], [`blob_tint`]: 0 when it
//!    is born, [`REFORM_TINT`] when the slab arrives, all of it by [`TINT_WHOLE_AT`]), so the pill the host's lift
//!    paints when A is gone ([`A_GONE_AT`]) is the colour B already wore and nothing pops; the
//!    opener's face (its label) never leads that tint ([`Frame::opener_alpha`] is at most the
//!    tint on every frame). The modal dim follows the HEIGHT out and is gone with A. When A is
//!    gone the frame IS the closed frame, the opener at rest, exactly; it draws no glass.
//!
//! The open is NOT the close either: the opener's rect grows into the panel's (the panel now
//! COVERS its opener, `popover::cover_opener`), height leading, frosted throughout, the opener's
//! face consumed, ringing past the panel before it rests. Its merge shape is never whitened: frame 0 draws no glass (the face alone), and
//! the face's fall and the glass's rise run on one progress, a cross-fade with no frame at which
//! white is swapped for dark.
//!
//! **No measured opener, no morph.** [`Frame`] with `from == None` is the fade+scale FALLBACK:
//! the panel scales about its centre from [`FALLBACK_SCALE_FROM`] to 1 and its glass fades in on
//! the same fill curve.

use crate::{Rect, Spring, Zoom};

/// The board's panel corner radius, `border-radius: 22px` — what the design board's morph ends on.
/// The shipped morph does NOT end here: shipped panels state their own radius (20/24/26) and the
/// morph's end radius IS the caller's ([`Morph::frame`]'s `panel_radius`), so the handover to the
/// settled panel and the hand-back at the start of a close do not snap. This is the stand-in for
/// a caller with no panel of its own (the rows' painter, which reads no radius).
pub const PANEL_RADIUS: f32 = 22.0;
/// The share of the rect springs' travel (the smaller of the two) at which the round LENS relaxes
/// toward the caller's panel radius on an open (~150 ms, as the glass reaches its size; iOS eases it
/// to the panel's by ~250). Until then the corner radius is half the glass's smaller side.
pub const SHAPE_AT: f32 = 0.94;
/// The stiffness of the spring that carries the corner between the lens (0) and the panel's radius
/// (1): critically damped, ~100 ms to within 4%. A spring of its own, because the rect springs
/// approach rest in two frames (a clock read off them cannot carry a 100 ms ease), and it lands
/// exactly ([`Spring::land_if_still`]) so the radius IS the panel's when the open settles.
pub const K_SHAPE: f32 = 1200.0;
/// Width spring stiffness while opening: the underdamped fit to the iOS 26 recording (2026-11-17,
/// see the module doc), `ZETA_WIDTH` 0.7 after [`WIDTH_LAG`]. Largest error against the measured
/// table 0.035 (frame 3), width overshoot 4.6% at ~220 ms against the measured ~4% at ~220.
pub const K_WIDTH: f32 = 450.0;
/// The width spring's damping ratio ([`K_WIDTH`]): ringing 4.6% past the panel, back to rest by ~0.4 s.
pub const ZETA_WIDTH: f32 = 0.7;
/// How long the width waits before it starts to open, in seconds: the recording's width does not
/// move for the first two frames (0.00, -0.02, 0.07) while its height is already 0.08 -> 0.37, so
/// the glass first extends DOWN from the button and then widens (about 1.6 frames).
pub const WIDTH_LAG: f32 = 0.026;
/// Height spring stiffness while opening: the underdamped fit to the same recording, `ZETA_HEIGHT`
/// 0.66, from the first glass frame with no delay. Largest error against the table 0.053 (frames 1
/// and 10): a second-order spring cannot hold the measured 9% peak AND the slow tail, so the fit
/// peaks at 6.3% (at ~120 ms; measured: 9% at ~150) and decays a little faster than the table.
pub const K_HEIGHT: f32 = 920.0;
/// The height spring's damping ratio ([`K_HEIGHT`]): ringing 6.3% past the panel, back to rest by ~0.35 s.
pub const ZETA_HEIGHT: f32 = 0.66;
/// The close's WIDTH spring (critically damped, driven past the drain to [`CLOSE_W_TARGET`] so it
/// does not slow to a crawl at it): fitted to iOS 26's measured close (2026-11-17; `series.md`
/// "Iteration 2", the slab reaches the opener's width by ~133 ms), largest error 0.03 of the travel.
/// It used to be one `K_CLOSE` 2000 for both axes, a slab at the drain by ~60 ms: twice as fast as iOS.
pub const K_CLOSE_W: f32 = 460.0;
/// Where the close's width spring is driven (0 = the drain's width; the rect clamps at it).
pub const CLOSE_W_TARGET: f32 = -0.37;
/// The close's HEIGHT spring: the slower clock of the two (the height drains over ~250 ms while the
/// width is done by ~133), largest error 0.05 against the measured table.
pub const K_CLOSE_H: f32 = 120.0;
/// Where the close's height spring is driven (below the drain, so the drain is passed at speed).
pub const CLOSE_H_TARGET: f32 = -0.67;
/// The outward velocity (progress units per second) the WIDTH spring is given on the first frame of a
/// close from a settled panel: a hair of a bulge (iOS: width 0.98 at 17 ms).
pub const BULGE_KICK_W: f32 = 5.3;
/// The same for the HEIGHT: the slab BULGES ~7% of its travel at ~33 ms (iOS: 1.027 at 17 ms, 1.07 at
/// 33) before it drains; with [`K_CLOSE_H`] pulling in, this peaks at 1.06 at ~25 ms.
pub const BULGE_KICK_H: f32 = 5.5;
/// The drain rect's width as a share of the opener's ([`drain_rect`]; never wider than the opener):
/// the slab reaches the opener's own width, as iOS's does (the table's width 0 is the button's), and
/// the fold then narrows it as it drains.
pub const DRAIN_WIDTH: f32 = 1.0;
/// The drain rect's height as a share of the opener's (never taller than the drain is wide, so a
/// round opener drains as a round drop).
pub const DRAIN_HEIGHT: f32 = 1.0;
/// The air between the opener's bottom edge and the drain rect's top, in px: smaller than
/// [`MERGE_K`], so the smooth minimum bridges the two and the neck shows.
pub const DRAIN_GAP: f32 = 8.0;
/// The height progress (the slab's travel, 1 = the panel, 0 = the drain) under which a close's FOLD is
/// released: the slab is in the last fifth of its way (~175 ms) and starts to drain up into the opener.
/// The recording's bbox drains through ~200 ms and is gone by ~250, so the fold takes only the TAIL (a
/// fold released at half the travel collapsed the drawn slab by a third of the panel in one frame, the
/// owner's film at +133 ms); the hand-back is at ~233 ms. The drawn series is fitted, not the springs
/// (`the_close_follows_the_ios_reference_curve`).
pub const FOLD_AT: f32 = 0.2;
/// The fold spring's stiffness (0 = the slab whole under the opener, 1 = drained into it):
/// critically damped, the slab is within 4% of gone in about 50 ms (three frames).
pub const K_FOLD: f32 = 5800.0;
/// The opener's re-formed blob B ([`Frame::merge`]) as a share of the opener's own size once the
/// slab has arrived at the drain: it grows from [`BLOB_BORN`] to this over the slab's approach (the
/// open's merge curve, [`merge_scale_at`]), so it is about half size when the fold begins. The fold
/// grows it to 1.
pub const REFORM_FROM: f32 = 0.7;
/// The opener's re-formed blob B's size on its FIRST frame, as a share of the opener's own size.
/// It has to be a shape the smooth minimum can join the slab to, not a point: a bead a few px across is a point to it, which turned
/// B into a one-frame spike on the slab's top edge, and the next frame into a capsule floating
/// above the slab with the neck broken. It is born on the opener's BOTTOM edge, the one nearest the
/// slab, and grows UP from it ([`Frame::merge`]), so the gap between B's bottom and the slab's top
/// never exceeds [`DRAIN_GAP`], inside the neck's reach (half of [`MERGE_K`]).
pub const BLOB_BORN: f32 = 0.3;
/// The share of the opener's focused fill B wears once the slab has arrived at the drain, before
/// the fold begins ([`blob_tint`]): B is part-whitened over the re-form (it was dark glass on every
/// frame of it) and takes the rest as A drains into it.
pub const REFORM_TINT: f32 = 0.4;
/// The fold at which the opener's own face starts to fade back in ([`Frame::opener_alpha`]): the
/// face returns over the last of the drain, on the blob.
pub const OPENER_BACK_FROM: f32 = 0.55;
/// The fold at which the blob B wears ALL of the opener's fill ([`blob_tint`]): a little before A is
/// gone ([`A_GONE_AT`]), so the last glass frame is whole whatever step the fold takes on it.
pub const TINT_WHOLE_AT: f32 = 0.9;
/// The slab's scale (1 = the drain rect) at which it is gone: the close is over and the frame is
/// the closed one. A few pixels of glass, already inside the blob's neck.
pub const A_GONE_AT: f32 = 0.04;
/// The stiffness of the `out` spring that carries a close's first phase, the rows leaving (0 =
/// content in, 1 = content out; the glass keeps its frost). Critically damped, so ≈50 ms to within 10% of rest and
/// ≈67 ms to within 4%: the content leaves over about three frames while the slab shrinks (the
/// reference's rows are gone by 50-67 ms; the old 9000 had them gone in two frames, over a rect that held).
pub const K_OUT: f32 = 6000.0;
/// The rows' LENS spring: they start at 0 when the glass first draws and ring past 1 to a ~12% peak at
/// ~167 ms, as iOS's lens magnifies them (measured 2026-11-17, `series.md` "ROWS": 0.65 at 67 ms,
/// 1.12 at 167, 1.0 by ~360), largest error against the table 0.016. Not the glass's height ratio.
pub const K_LENS: f32 = 395.0;
/// The lens spring's damping ratio ([`K_LENS`]).
pub const ZETA_LENS: f32 = 0.57;
/// The rows' scale spring stiffness (brief: k ~ 720). Only the fade+scale FALLBACK uses the rows'
/// own spring now; with an opener an open's rows are scaled by the lens spring ([`K_LENS`]) and a
/// close's by the shrinking slab ([`Frame::rows_scale`]).
pub const K_ROWS: f32 = 720.0;
/// The rows' damping ratio (brief and board: 0.6 — underdamped, one small overshoot).
pub const ZETA_ROWS: f32 = 0.6;
/// The rows' scale when the morph starts (the board's `scale(.82)`).
pub const ROWS_FROM: f32 = 0.82;
/// The HEIGHT at which the glass is fully in: the growing shape is all glass before it gets big.
pub const FILL_AT: f32 = 0.18;
/// Progress (the slower of the two rect springs) at which the rows begin to appear: the width's
/// first move, on the third glass frame (~33 ms after the first), tiny inside the small lens
/// ([`K_LENS`]: scale ~0.2 then, ~0.45 a frame later). The recording shows them at 67-100 ms and
/// the owner asked for the content EARLIER (2026-11-18: the droplet is already a menu while it
/// moves), so the start follows the width's lag, the first frame the glass is wider than the opener.
pub const ROWS_FROM_AT: f32 = 0.05;
/// Progress at which the rows are fully opaque: ~120 ms (the width at 0.8), as the glass nears its
/// size, rather than at the first peak (0.96, ~150 ms) they were opaque at before the content came earlier.
pub const ROWS_OPAQUE_AT: f32 = 0.8;
/// Progress at which the opener's own face is fully gone on an open (and starts to return on a
/// close): it fades out over `[0, OPENER_OUT_AT]` as the glass detaches from it
/// ([`Frame::opener_alpha`]; the host's opener lift paints the opener at that alpha).
pub const OPENER_OUT_AT: f32 = 0.15;
/// Progress at which the merge shape has shrunk to nothing on an open (and begins to re-form on a
/// close): the glass is joined to a copy of the opener's rect that scales to 0 over
/// `[0, MERGE_END_AT]` ([`Frame::merge`]).
pub const MERGE_END_AT: f32 = 0.35;
/// The smooth-minimum reach of the merge, in px: shapes closer than this are joined by a neck.
/// The glass is also inflated by up to a quarter of it where the two shapes coincide.
pub const MERGE_K: f32 = 28.0;
/// Fallback when the opener has no measured rect to morph from: a fade+scale of the panel about
/// its centre, starting at this scale. The board's `menuout` keyframes leave a menu at 0.92, so an
/// entrance from the same scale is the matching motion.
pub const FALLBACK_SCALE_FROM: f32 = 0.92;

/// Everything a draw needs from the morph at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The surface's current rect.
    pub rect: Rect,
    /// Its current corner radius.
    pub radius: f32,
    /// The morph's progress, 0 = the opener's rect, 1 = the panel: the slower rect spring, clamped to
    /// 0..1 (the rect itself passes the panel while the springs ring).
    pub progress: f32,
    /// The glass's share of the fill (0 = none, 1 = all glass): the alpha of the glass, with an
    /// opener also its shadow's (the one ramp); for the fallback the glass's alpha alone.
    pub ground_alpha: f32,
    /// Alpha of the panel's rows: 0 until [`ROWS_FROM_AT`] and never above [`ground_alpha`](Self::ground_alpha),
    /// so a row is only ever drawn on a surface that is at least as much glass as it is row.
    pub content_alpha: f32,
    /// Scale of the rows about [`rows_anchor`]: with an opener the LENS spring ([`K_LENS`]: ~0.2 when
    /// the rows first show, ~0.65 at 67 ms, a 12% overshoot at ~167 ms, 1 at rest), shrinking with the slab on a close
    /// (the glass's height over the panel's); for the fade+scale fallback its own rows spring (0.82 -> 1).
    pub rows_scale: f32,
    /// How much of the OPENER's own face is left (1 = whole, 0 = consumed): fades out over the
    /// first [`OPENER_OUT_AT`] of the progress on an open and back in over the last of a close's
    /// fold ([`OPENER_BACK_FROM`]). 1 for the
    /// fallback, which has no opener to consume. The host's opener lift paints at this alpha
    /// (`Screen::opener_alpha`).
    pub opener_alpha: f32,
    /// The shape the glass is JOINED to, or `None` once there is nothing to join (the settled
    /// panel, the fallback). On an open: the opener's rect scaled about its own centre by
    /// `1 - smoothstep(0, MERGE_END_AT, progress)`. On a close: the opener's re-formed blob B, the
    /// same rect scaled about the centre of its BOTTOM edge (the edge nearest the slab, so it is
    /// joined to the slab from its first frame) from [`BLOB_BORN`] to [`REFORM_FROM`] as the slab
    /// arrives and by the fold up to 1 as it drains in, wearing more of the opener's fill as it
    /// grows ([`Merge::tint`]).
    pub merge: Option<Merge>,
    /// How much of the modal dim has come in, 0..1 ([`dim_at`] of the progress): the share of the
    /// surface's peak dim (`Popover::dim` hands it to the container as `Scrim::following`).
    pub dim: f32,
}

impl Frame {
    /// What the glass wears at this instant: the join, as the gfx layer draws it (`Popover::ground`
    /// hands it to `popover_glass::draw_shadowed`). The clarity is always 0: the body is the frost on
    /// every frame of an open and of a close (module doc), so the morph has nothing to say about it.
    ///
    /// `opener_fill` is the colour the opener's own focused face is drawn with (the caller knows
    /// its opener; [`crate::popover::Popover::opener_fill`]): the join whitens toward it by
    /// [`Merge::tint`]. `None` (an opener that is itself glass, like the profile chip) leaves the
    /// join's face as the glass's own.
    pub fn glass_morph(&self, opener_fill: Option<[f32; 4]>) -> plx_gfx::gfx::GlassMorph {
        plx_gfx::gfx::GlassMorph {
            merge: self.merge.map(|m| plx_gfx::gfx::GlassMerge {
                rect: m.rect,
                radius: m.radius,
                k: m.k,
                tint: opener_fill.map_or([0.0; 4], |c| [c[0], c[1], c[2], m.tint * c[3]]),
            }),
            clear: 0.0,
        }
    }
}

/// The second shape of a merging glass: where it is, how round, and how far its neck reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Merge {
    /// The shape's rect (the opener's, scaled about its own centre).
    pub rect: Rect,
    /// Its corner radius (the opener's pill radius, scaled with it).
    pub radius: f32,
    /// The smooth-minimum reach in px ([`MERGE_K`]).
    pub k: f32,
    /// How much of the OPENER'S FOCUSED FILL the shape's face wears, 0 = the glass's own face, 1 =
    /// the flat fill (the colour is the caller's: [`Frame::glass_morph`]). 0 on an open. On a close
    /// it is [`blob_tint`]: 0 when B is born, rising over the re-form and the fold, so it arrives
    /// whole on the pill the host's lift then paints.
    pub tint: f32,
}

/// `x` mapped from `[a, b]` to `[0, 1]`, clamped.
fn ramp(x: f32, a: f32, b: f32) -> f32 {
    ((x - a) / (b - a)).clamp(0.0, 1.0)
}

/// The smoothstep of `x` over `[a, b]`.
fn smooth(x: f32, a: f32, b: f32) -> f32 {
    let u = ramp(x, a, b);
    u * u * (3.0 - 2.0 * u)
}

/// The fill crossover at progress `t`: a smoothstep over `[0, FILL_AT]`.
pub fn fill_at(t: f32) -> f32 {
    smooth(t, 0.0, FILL_AT)
}

/// The opener's own alpha at progress `t`: `1 - smoothstep(0, OPENER_OUT_AT, t)`.
pub fn opener_alpha_at(t: f32) -> f32 {
    1.0 - smooth(t, 0.0, OPENER_OUT_AT)
}

/// **The modal dim's share at progress `t`**: a smoothstep over the whole progress, so the page dims
/// as the panel grows (in by ~150 ms, when the glass first reaches its size) and clears as the slab
/// shrinks to the drain on a close (gone with the slab on the hand-back frame,
/// `the_dim_follows_the_morph_for_a_morphing_menu_and_is_the_containers_for_any_other`), the
/// container's own appear spring (k 300: 73% at 150 ms, still 14% at 200 ms on the way out) no
/// longer deciding it for a morphing menu.
pub fn dim_at(t: f32) -> f32 {
    smooth(t, 0.0, 1.0)
}

/// The merge shape's scale at progress `t`: `1 - smoothstep(0, MERGE_END_AT, t)`.
pub fn merge_scale_at(t: f32) -> f32 {
    1.0 - smooth(t, 0.0, MERGE_END_AT)
}

/// **How much of the opener's focused fill the re-formed blob B wears** on a close, from the
/// re-form's `reform` (0 when B is born, 1 once the slab has arrived at the drain, the open's merge
/// curve [`merge_scale_at`]) and the `fold` (0 = the slab whole under the opener, 1 = drained in):
/// [`REFORM_TINT`] of it over the re-form, then the rest over the fold, the whole of it by
/// [`TINT_WHOLE_AT`], before A is gone ([`A_GONE_AT`], the frame the close hands back). B is born as glass, whitens as it
/// re-forms and arrives whole, so the pill the host's lift paints in its place is the colour B
/// already wears (owner film, 2026-11-17: B was dark glass on every frame and popped white on the
/// hand-back). It rides the same two clocks as B's size, which are eased already (a smoothstep and
/// a critically damped spring), so it needs no curve of its own, and the re-form's whitening is
/// kept to a share so the fast arrival of the slab (four frames) cannot make it step by more than
/// about a fifth of the range.
pub fn blob_tint(reform: f32, fold: f32) -> f32 {
    let gone = (fold / TINT_WHOLE_AT).clamp(0.0, 1.0);
    let reform = reform.clamp(0.0, 1.0);
    REFORM_TINT * reform * reform * (1.0 - gone) + gone
}

/// **The drain rect** a close's slab travels to ([`Morph::frame`]): centred on the `opener`
/// horizontally, its top [`DRAIN_GAP`] under the opener's bottom edge, [`DRAIN_WIDTH`] of its width
/// (so never wider) and about its height ([`DRAIN_HEIGHT`], never taller than wide): a small drop
/// of glass hanging just below the control, close enough to be joined to it by the neck.
pub fn drain_rect(opener: Rect) -> Rect {
    let w = opener.w * DRAIN_WIDTH;
    let h = (opener.h * DRAIN_HEIGHT).min(w);
    Rect::new(opener.cx() - w * 0.5, opener.y + opener.h + DRAIN_GAP, w, h)
}

/// `a` to `b` by `t`.
fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `a` to `b` by `t`, per edge.
fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Rect::new(l(a.x, b.x), l(a.y, b.y), l(a.w, b.w), l(a.h, b.h))
}

/// `r` scaled by `sw` wide and `sh` tall about its own top-centre: a slab that drains UP toward the
/// opener above it.
fn shrink_to_top_centre(r: Rect, sw: f32, sh: f32) -> Rect {
    Rect::new(r.cx() - r.w * sw * 0.5, r.y, r.w * sw, r.h * sh)
}

/// The panel's rect when the width spring is at `pw` and the height spring at `ph` (0 = the
/// opener's pill rect, 1 = the final panel). `from` is `None` when the opener has no rect: the
/// panel scales about its centre ([`FALLBACK_SCALE_FROM`] -> 1, driven by `pw`).
pub fn rect_at_xy(from: Option<Rect>, panel: Rect, pw: f32, ph: f32) -> Rect {
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    match from {
        Some(f) => Rect::new(
            lerp(f.x, panel.x, pw),
            lerp(f.y, panel.y, ph),
            lerp(f.w, panel.w, pw),
            lerp(f.h, panel.h, ph),
        ),
        None => {
            let s = lerp(FALLBACK_SCALE_FROM, 1.0, pw);
            Rect::new(
                panel.cx() - panel.w * s * 0.5,
                panel.cy() - panel.h * s * 0.5,
                panel.w * s,
                panel.h * s,
            )
        }
    }
}

/// The panel's rect at one morph progress `t` for both axes; see [`rect_at_xy`].
pub fn rect_at(from: Option<Rect>, panel: Rect, t: f32) -> Rect {
    rect_at_xy(from, panel, t, t)
}

/// The corner radius for the round lens: `lens` (half the glass's smaller side) while `settle` is 0,
/// the caller's `end_radius` once it is 1 (exactly, no float residue), never more than a capsule.
pub fn radius_at(lens: f32, end_radius: f32, settle: f32) -> f32 {
    if settle >= 1.0 {
        return end_radius.min(lens);
    }
    (lens + (end_radius - lens) * settle.max(0.0)).min(lens)
}

/// `fs_glass.frag`'s `sdBox`: the signed distance from `p` to the rect `r` with corner `radius`
/// (held to the rect's half extent).
pub fn sd_box(p: (f32, f32), r: Rect, radius: f32) -> f32 {
    let radius = radius.min(r.w * 0.5).min(r.h * 0.5);
    let q = ((p.0 - r.cx()).abs() - r.w * 0.5 + radius, (p.1 - r.cy()).abs() - r.h * 0.5 + radius);
    let (ox, oy) = (q.0.max(0.0), q.1.max(0.0));
    (ox * ox + oy * oy).sqrt() + q.0.max(q.1).min(0.0) - radius
}

/// The point the rows scale about while the morph runs: the point of the final `panel` nearest
/// the opener's centre (the edge of the panel the pill sits on), or the panel's centre when the
/// opener has no rect. Rows grow out of the pill instead of out of the middle of the glass.
pub fn rows_anchor(from: Option<Rect>, panel: Rect) -> (f32, f32) {
    match from {
        Some(f) => (f.cx().clamp(panel.x, panel.x + panel.w), f.cy().clamp(panel.y, panel.y + panel.h)),
        None => (panel.cx(), panel.cy()),
    }
}

/// The scale the rows are drawn under at one instant: `scale` about [`rows_anchor`].
pub fn rows_zoom(from: Option<Rect>, panel: Rect, scale: f32) -> Zoom {
    let (ox, oy) = rows_anchor(from, panel);
    Zoom { s: scale, ox, oy }
}

/// The morph's motion state. `const`-constructible so a `static Popover` can hold one.
#[derive(Clone, Copy)]
pub struct Morph {
    w: Spring,
    h: Spring,
    /// The fade+scale fallback's rows spring (0.82 -> 1); an opener's rows use `lens` instead
    /// (`rows_scale = lerp(lens, rect.h / panel.h, out)`: the lens spring on an open, the slab's
    /// height on a close).
    rows: Spring,
    /// A close's content phase, the rows leaving: 0 = in, 1 = out ([`K_OUT`]); the glass stays frosted.
    out: Spring,
    /// The corner: 0 = the round lens (half the smaller side), 1 = the caller's panel radius
    /// ([`K_SHAPE`], released at [`SHAPE_AT`] on an open, back to 0 on a close).
    shape: Spring,
    /// The rows' lens magnification on an open ([`K_LENS`]): 0 when the glass first draws, 1 at rest.
    lens: Spring,
    /// A close's fold, the slab draining into the re-formed opener: 0 = whole under it, 1 = gone
    /// ([`K_FOLD`]). Released once the slab is within [`FOLD_AT`] of the drain rect.
    fold: Spring,
    /// What is left of the width's [`WIDTH_LAG`], in seconds.
    lag: f32,
    /// A close has begun (and its [`BULGE_KICK_W`]/[`BULGE_KICK_H`] have been given); an open ends it.
    closing: bool,
}

impl Morph {
    pub const fn new() -> Self {
        Morph {
            w: Spring::at(0.0),
            h: Spring::at(0.0),
            rows: Spring::at(ROWS_FROM),
            out: Spring::at(0.0),
            lens: Spring::at(0.0),
            shape: Spring::at(0.0),
            fold: Spring::at(0.0),
            lag: WIDTH_LAG,
            closing: false,
        }
    }

    /// Back to the pill, with no motion in between.
    pub fn reset(&mut self) {
        *self = Morph::new();
    }

    /// Advance one frame toward the open (`true`) or closed (`false`) end.
    ///
    /// An open is the two UNDERDAMPED rect springs ([`K_HEIGHT`], and [`K_WIDTH`] after its
    /// [`WIDTH_LAG`]); `out` and the `fold` return to 0 if a close had raised them. EVERY spring the
    /// morph owns is put exactly on its target when the frame loop's rest test stops calling it moving
    /// ([`Spring::land_if_still`]): an underdamped spring only ever decays toward its target,
    /// [`settled`](Self::settled) must not fire at the first crossing while it rings, and the loop must
    /// not be woken for a sub-pixel tail after it has slept.
    ///
    /// A close is NOT that reversed: the rect springs are released on the first frame with an
    /// outward [`BULGE_KICK_H`] (a settled panel bulges before it shrinks), `out` rises with them (the
    /// rows leave while the slab shrinks; the glass stays frosted), and the fold is released once the
    /// slab is within [`FOLD_AT`] of the drain rect.
    pub fn step(&mut self, open: bool, dt: f32) {
        // Every spring below is put EXACTLY on its target the moment the frame loop's own rest test (the one
        // its integrators report `Motion` by) stops calling it moving at this step's `dt`: the instant the loop
        // would stop asking for frames. The tight threshold `Spring::land` uses lets an underdamped spring
        // ring on under the loop's quarter-pixel test after it fell asleep, and each idle wake, which steps
        // by up to `FRAME_DT_CLAMP`, then reports motion again and presents one more sub-pixel frame; a
        // spring that is landed when the loop sleeps cannot be woken by any later step.
        let still = dt;
        if open {
            self.closing = false;
            self.out.step(0.0, K_OUT, dt);
            self.out.land_if_still(0.0, still);
            self.fold.step(0.0, K_FOLD, dt);
            self.fold.land_if_still(0.0, still);
            let mut dt_w = dt;
            if self.lag > 0.0 {
                let waited = self.lag.min(dt);
                self.lag -= waited;
                dt_w = dt - waited;
            }
            self.h.step_zeta(1.0, K_HEIGHT, ZETA_HEIGHT, dt);
            if dt_w > 0.0 {
                self.w.step_zeta(1.0, K_WIDTH, ZETA_WIDTH, dt_w);
            }
            self.h.land_if_still(1.0, still);
            self.w.land_if_still(1.0, still);
            self.lens.step_zeta(1.0, K_LENS, ZETA_LENS, dt);
            self.lens.land_if_still(1.0, still);
            let panel_corner = if self.w.pos.min(self.h.pos) >= SHAPE_AT { 1.0 } else { 0.0 };
            self.shape.step(panel_corner, K_SHAPE, dt);
            self.shape.land_if_still(panel_corner, still);
            self.rows.step_zeta(1.0, K_ROWS, ZETA_ROWS, dt);
            self.rows.land_if_still(1.0, still);
            return;
        }
        self.lag = 0.0;
        if !self.closing {
            self.closing = true;
            if self.w.pos >= 0.9 && self.h.pos >= 0.9 {
                self.w.vel += BULGE_KICK_W;
                self.h.vel += BULGE_KICK_H;
            }
        }
        self.out.step(1.0, K_OUT, dt);
        self.out.land_if_still(1.0, still);
        self.w.step(CLOSE_W_TARGET, K_CLOSE_W, dt);
        self.w.land_if_still(CLOSE_W_TARGET, still);
        self.h.step(CLOSE_H_TARGET, K_CLOSE_H, dt);
        self.h.land_if_still(CLOSE_H_TARGET, still);
        self.lens.step_zeta(1.0, K_LENS, ZETA_LENS, dt);
        self.lens.land_if_still(1.0, still);
        self.shape.step(0.0, K_SHAPE, dt);
        self.shape.land_if_still(0.0, still);
        if self.h.pos <= FOLD_AT {
            self.fold.step(1.0, K_FOLD, dt);
            self.fold.land_if_still(1.0, still);
        } else {
            self.fold.vel = 0.0;
        }
        self.rows.step_zeta(ROWS_FROM, K_ROWS, ZETA_ROWS, dt);
        self.rows.land_if_still(ROWS_FROM, still);
    }

    /// Has the close finished for an opener the slab drains into: the slab gone ([`A_GONE_AT`])?
    /// (`Popover::update` resets the morph, `out` included, when it lands.) A close with no opener
    /// rect has no droplet: [`closed_for`](Self::closed_for).
    pub fn closed(&self) -> bool {
        self.fold.pos >= 1.0 - A_GONE_AT
    }

    /// [`closed`](Self::closed) for a morph with an opener (`has_opener`), or for the fade+scale
    /// fallback, which has no droplet to wait for: the rect back at its start and the rows too.
    pub fn closed_for(&self, has_opener: bool) -> bool {
        if has_opener {
            return self.closed();
        }
        self.w.pos <= 0.001 && self.h.pos <= 0.001 && self.rows.pos <= ROWS_FROM + 0.001
    }

    /// Has the open arrived? EVERY spring LANDED (not merely crossed its target: the rect, the lens
    /// and the rows ring past it first), so no later step, however long an idle wake makes it, can move
    /// anything. A close that has begun ([`step`](Self::step) with `false`) raises `out` on its first
    /// frame, so the settled panel stops being settled with it.
    pub fn settled(&self) -> bool {
        [&self.w, &self.h, &self.rows, &self.lens, &self.shape].iter().all(|s| s.pos == 1.0 && s.vel == 0.0)
            && [&self.out, &self.fold].iter().all(|s| s.pos == 0.0 && s.vel == 0.0)
    }

    /// The draw state now, for the opener rect `from` (`None` = the fade+scale fallback) and the
    /// final `panel` rect.
    ///
    /// `panel_radius` is the caller's own panel corner radius: the morph ENDS on it (and a close
    /// starts from it), so the settled ground and the morphing one are the same shape.
    pub fn frame(&self, from: Option<Rect>, panel: Rect, panel_radius: f32) -> Frame {
        // The springs ring past 1 (and a close's bulge too): the rect extrapolates with them, every
        // clock below reads them clamped to 0..1. The glass's cross-in reads the HEIGHT alone (it
        // leads, so the glass draws from the first step while the width still waits its lag).
        let (pw_raw, ph_raw) = (self.w.pos.max(0.0), self.h.pos.max(0.0));
        let (pw, ph) = (pw_raw.min(1.0), ph_raw.min(1.0));
        let out = self.out.pos.clamp(0.0, 1.0);
        let fold = self.fold.pos.clamp(0.0, 1.0);
        let progress = pw.min(ph);
        // The rows stand on the glass, so they are never more opaque than it (the rows start while the
        // glass's own cross-in is still short of whole, and a row must never be the surface's only
        // content): `ground_alpha` is the glass's alpha, each branch below states its own.
        let rows = ramp(progress, ROWS_FROM_AT, ROWS_OPAQUE_AT) * (1.0 - out);
        // A close's dim follows the (slower) HEIGHT and is out when the slab is gone
        let dim = dim_at(lerp_f(progress, ph, smooth(out, 0.0, 1.0))) * (1.0 - fold);
        let Some(f) = from else {
            return Frame {
                rect: rect_at_xy(None, panel, pw, ph),
                radius: panel_radius,
                progress,
                ground_alpha: fill_at(progress),
                content_alpha: rows.min(fill_at(progress)),
                rows_scale: self.rows.pos,
                opener_alpha: 1.0,
                merge: None,
                dim,
            };
        };
        // The slab is gone: the close is over and the frame is the closed one, exactly (the same
        // frame a fresh morph gives), so the hand-back to the page's own opener has no step.
        if fold >= 1.0 - A_GONE_AT {
            return Morph::new().frame(from, panel, panel_radius);
        }
        // A CLOSE is the `out` spring's story (c = 1), an open the other (c = 0): every part that
        // differs between them is blended by it, so a close that interrupts an open (or the other
        // way round) is continuous rather than snapping from one path to the other.
        let c = out;
        let lerp = lerp_f;
        let pill_radius = f.w.min(f.h) * 0.5;
        let drain = drain_rect(f);
        // where the slab's travel starts from: the opener's rect on an open, the drain rect on a close
        let start = lerp_rect(f, drain, c);
        let travelled = rect_at_xy(Some(start), panel, pw_raw, ph_raw);
        let sa = 1.0 - fold;
        // The merge shape B: the opener's rect shrinking about its own centre on an open; on a close
        // the opener re-forming, a bead of [`BLOB_BORN`] the frame the slab comes within the merge's
        // reach, growing as it arrives and the whole opener as it drains in. A close's B grows UP
        // from the opener's bottom edge, the one nearest the slab, so that edge never leaves the
        // slab's top by more than the drain gap and the neck holds from B's first frame.
        let reform = merge_scale_at(progress);
        let closed_b = if reform > 0.0 { lerp(lerp(BLOB_BORN, REFORM_FROM, reform), 1.0, fold) } else { 0.0 };
        let sb = lerp(reform, closed_b, c);
        let merge = (sb > 0.0).then(|| {
            let (w, h) = (f.w * sb, f.h * sb);
            // the centre-anchored top on an open, the bottom-anchored top on a close (bit-exact at c = 0)
            let top = lerp(f.cy() - h * 0.5, f.y + f.h - h, c);
            // whitened only on a close: an open's shape sits INSIDE the growing glass and is gone by
            // `MERGE_END_AT`, and its face fades as the opener's does (a cross-fade, no hand-over)
            Merge { rect: Rect::new(f.cx() - w * 0.5, top, w, h), radius: pill_radius * sb, k: MERGE_K, tint: blob_tint(reform, fold) * c }
        });
        // the fold narrows A linearly and drains its height with the SQUARE of the fold, so the slab
        // keeps its height while the fold takes hold and the drawn series follows the table's tail
        let rect = shrink_to_top_centre(travelled, sa, 1.0 - fold * fold);
        // The round LENS: half the smaller side while the glass is small (and again as a close shrinks
        // it), easing to the caller's radius as it nears rest (the shape spring)
        let settle = self.shape.pos.clamp(0.0, 1.0);
        let lens = rect.w.min(rect.h) * 0.5;
        let radius = radius_at(lens, panel_radius, settle);
        // a close's slab is glass all the way into the opener; an open's is the one fill ramp
        let ground_alpha = lerp(fill_at(ph), 1.0, c);
        Frame {
            rect,
            radius,
            progress,
            ground_alpha,
            content_alpha: rows.min(ground_alpha),
            // the rows are laid out in the glass: they are as big as it is, over the settled panel's
            rows_scale: if panel.h > 0.0 { lerp(self.lens.pos, rect.h / panel.h, c) } else { 1.0 },
            // the open's fade hands over to the close's return by `out`, steeply: the blend's residual
            // while `out` is still closing on 1 must not read as the face flickering back
            opener_alpha: lerp(opener_alpha_at(progress), smooth(fold, OPENER_BACK_FROM, 1.0), smooth(c, 0.0, 0.9)),
            merge,
            dim,
        }
    }
}

impl Default for Morph {
    fn default() -> Self {
        Morph::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    const PILL: Rect = Rect::new(1500.0, 40.0, 196.0, 38.0);
    const PANEL: Rect = Rect::new(1380.0, 40.0, 316.0, 400.0);
    /// A caller's own panel radius, deliberately not the board's 22.
    const RAD: f32 = 26.0;

    fn open_frames() -> Vec<(Morph, Frame)> {
        let mut m = Morph::new();
        let mut out = vec![(m, m.frame(Some(PILL), PANEL, RAD))];
        for _ in 0..120 {
            m.step(true, DT);
            out.push((m, m.frame(Some(PILL), PANEL, RAD)));
        }
        out
    }

    #[test]
    fn starts_as_the_pill_and_settles_on_the_panel() {
        let f = open_frames();
        assert_eq!(f[0].1.rect, PILL);
        assert_eq!(f[0].1.radius, PILL.h * 0.5);
        assert_eq!((f[0].1.ground_alpha, f[0].1.content_alpha), (0.0, 0.0));
        let (m, last) = f.last().copied().unwrap();
        assert!(m.settled());
        assert!((last.rect.w - PANEL.w).abs() < 0.5 && (last.rect.h - PANEL.h).abs() < 0.5);
        assert!((last.rect.x - PANEL.x).abs() < 0.5 && (last.rect.y - PANEL.y).abs() < 0.5);
        assert_eq!((last.ground_alpha, last.content_alpha), (1.0, 1.0));
        assert!((last.rows_scale - 1.0).abs() < 0.002);
    }

    /// iOS 26's glass menu, measured frame by frame (the owner's screen recording of the Claude iOS
    /// app's "..." menu, 2026-11-17, 60 fps, per-pixel diff against the static page; t = 0 is the
    /// first frame any glass shows). Normalized progress of the glass's HEIGHT and WIDTH, 0 = the
    /// button, 1 = the settled panel, > 1 = overshoot, as `(frame, height, width)`.
    const IOS_OPEN: [(usize, f32, f32); 20] = [
        (0, 0.08, 0.0), (1, 0.37, -0.02), (2, 0.59, 0.07), (3, 0.76, 0.25), (4, 0.86, 0.44), (5, 0.95, 0.58),
        (6, 1.015, 0.70), (7, 1.06, 0.78), (8, 1.085, 0.88), (9, 1.09, 0.95), (10, 1.085, 0.997), (11, 1.07, 1.023),
        (12, 1.06, 1.033), (13, 1.05, 1.04), (14, 1.04, 1.04), (16, 1.023, 1.037), (18, 1.009, 1.027),
        (20, 1.002, 1.017), (23, 1.0, 1.003), (25, 1.0, 1.0),
    ];

    /// The open follows the iOS recording's curve: height and width progress per frame, from the
    /// first frame the glass DRAWS (`ground_alpha > 0`, which is the table's frame 0) through frame
    /// 25, within 0.06 of the table. A critically damped pair (the old k 620 / k 950) is finished
    /// by 150 ms and has no overshoot: the owner's "your animations are too fast".
    #[test]
    fn the_open_follows_the_ios_reference_curve() {
        let mut m = Morph::new();
        let mut frames = Vec::new();
        for _ in 0..40 {
            m.step(true, DT);
            frames.push(m.frame(Some(PILL), PANEL, RAD));
        }
        let first = frames.iter().position(|f| f.ground_alpha > 0.0).expect("the glass draws");
        assert_eq!(first, 0, "the glass draws on the first step: the height leads");
        let mut worst = 0.0_f32;
        for &(n, want_h, want_w) in &IOS_OPEN {
            let r = frames[first + n].rect;
            let (h, w) = ((r.h - PILL.h) / (PANEL.h - PILL.h), (r.w - PILL.w) / (PANEL.w - PILL.w));
            assert!((h - want_h).abs() <= 0.06, "frame {n}: height {h} vs {want_h}");
            assert!((w - want_w).abs() <= 0.06, "frame {n}: width {w} vs {want_w}");
            worst = worst.max((h - want_h).abs()).max((w - want_w).abs());
        }
        assert!(worst > 0.0);
        let peak = |f: fn(&Rect) -> f32, lo: f32, hi: f32| frames.iter().map(|fr| (f(&fr.rect) - lo) / hi).fold(0.0_f32, f32::max);
        assert!(peak(|r| r.h, PILL.h, PANEL.h - PILL.h) > 1.04, "the height overshoots (iOS: ~9% of the travel)");
        assert!(peak(|r| r.w, PILL.w, PANEL.w - PILL.w) > 1.02, "the width overshoots (iOS: ~4%)");
    }

    /// An underdamped open still LANDS: it ends exactly on the panel with no velocity, and reports
    /// settled only then, not at the first crossing of 1 while it rings.
    #[test]
    fn the_open_ends_exactly_at_rest_and_is_not_settled_while_it_rings() {
        let mut m = Morph::new();
        let mut crossed = None;
        let mut settled_at = None;
        for n in 1..=90 {
            m.step(true, DT);
            let r = m.frame(Some(PILL), PANEL, RAD).rect;
            if crossed.is_none() && r.h >= PANEL.h {
                crossed = Some(n);
                assert!(!m.settled(), "the first crossing of the panel's height is not rest");
            }
            if settled_at.is_none() && m.settled() {
                settled_at = Some(n);
            }
        }
        let (crossed, settled_at) = (crossed.expect("crosses"), settled_at.expect("lands"));
        assert!(settled_at >= crossed + 6, "it rings before it lands ({crossed} -> {settled_at})");
        assert!(settled_at as f32 * DT <= 0.6, "and lands within 0.6 s ({settled_at} frames)");
        let r = m.frame(Some(PILL), PANEL, RAD).rect;
        assert_eq!(r, PANEL, "exactly the panel at rest, not within a tolerance");
        let before = m.frame(Some(PILL), PANEL, RAD);
        m.step(true, DT);
        assert_eq!(m.frame(Some(PILL), PANEL, RAD), before, "no motion once landed");
    }

    /// One step as the frame loop reads it: the motion its springs report by the rest test, and a landing
    /// (a jump, which presents a frame of its own).
    fn judged_step(m: &mut Morph, open: bool, dt: f32) -> plx_machine::motion::report::FrameReport {
        plx_machine::motion::report::frame_begin(dt);
        m.step(open, dt);
        plx_machine::motion::report::read()
    }

    /// Step at 16 ms until the frame loop's own test calls the morph quiet (no spring moving, nothing landed this
    /// frame), then hand it ONE step at `idle_dt` as an idle wake does: the loop sleeps once it is quiet, and a
    /// wake after a long sleep steps by up to [`FRAME_DT_CLAMP`], so that step must report nothing and draw nothing.
    fn assert_quiet_stays_quiet_across_an_idle_wake(open: bool, idle_dt: f32) {
        let mut m = Morph::new();
        if !open {
            for _ in 0..120 {
                m.step(true, DT);
            }
        }
        let quiet_at = (1..=400).find(|_| {
            let r = judged_step(&mut m, open, DT);
            !r.moving && !r.jumped
        });
        assert!(quiet_at.is_some(), "the {} goes quiet", if open { "open" } else { "close" });
        let before = m.frame(Some(PILL), PANEL, RAD);
        let r = judged_step(&mut m, open, idle_dt);
        assert!(
            !r.moving && !r.jumped,
            "{} quiet at 16 ms (frame {quiet_at:?}) still reports motion when an idle wake steps it by {idle_dt} s: {r:?}",
            if open { "open" } else { "close" },
        );
        assert_eq!(m.frame(Some(PILL), PANEL, RAD), before, "and the frame it draws does not change");
    }

    /// The open's two late frames on the TV (~55 and ~110 ms after the run ends): `rows` rang on and `lens` landed
    /// only within 6e-5, so a spring the 16 ms rest test called still reported motion when the idle wake handed it
    /// 50 ms, and one sub-pixel frame presented before the loop slept again.
    #[test]
    fn an_open_that_is_quiet_does_not_wake_the_loop_when_an_idle_step_comes_late() {
        for idle_dt in [0.02, 0.033, plx_machine::motion::FRAME_DT_CLAMP] {
            assert_quiet_stays_quiet_across_an_idle_wake(true, idle_dt);
        }
    }

    #[test]
    fn a_close_that_is_quiet_does_not_wake_the_loop_when_an_idle_step_comes_late() {
        for idle_dt in [0.02, 0.033, plx_machine::motion::FRAME_DT_CLAMP] {
            assert_quiet_stays_quiet_across_an_idle_wake(false, idle_dt);
        }
    }

    #[test]
    fn height_leads_width_by_the_stiffness_ratio() {
        let mut m = Morph::new();
        for _ in 0..6 {
            m.step(true, DT);
        }
        let fr = m.frame(Some(PILL), PANEL, RAD);
        let (pw, ph) = ((fr.rect.w - PILL.w) / (PANEL.w - PILL.w), (fr.rect.h - PILL.h) / (PANEL.h - PILL.h));
        assert!(ph > pw, "the stiffer height spring is ahead of the width ({ph} vs {pw})");
    }

    #[test]
    fn open_and_close_durations_follow_from_the_springs() {
        let mut m = Morph::new();
        for _ in 0..(0.25 / DT).round() as usize {
            m.step(true, DT);
        }
        assert!(!m.settled(), "the open is still ringing at 250 ms");
        let fr = m.frame(Some(PILL), PANEL, RAD);
        assert!((fr.rect.w - PANEL.w).abs() < 0.07 * (PANEL.w - PILL.w), "but within 7% of the panel's width: {fr:?}");
        for _ in 0..60 {
            m.step(true, DT);
        }
        assert!(m.settled());
        // A close is the slab's travel to the drain and the fold into the opener: at most 250 ms.
        let mut frames = 0;
        while !m.closed() {
            m.step(false, DT);
            frames += 1;
            assert!(frames <= 15, "the closed frame by 250 ms");
        }
        assert!(frames >= 10, "and not a snap: {frames} frames");
    }

    #[test]
    fn radius_is_the_lens_and_ends_at_the_callers_panel_radius() {
        let f = open_frames();
        let pill_r = PILL.h * 0.5;
        assert_eq!(f[0].1.radius, pill_r);
        let peak = f.iter().map(|(_, fr)| fr.radius).fold(0.0_f32, f32::max);
        assert!(peak > RAD + 8.0, "mid-open the corner is the round lens, past the panel's ({peak})");
        assert!((f.last().unwrap().1.radius - RAD).abs() < 0.05, "the morph ends on the caller's radius, not 22");
        assert_eq!(radius_at(pill_r, RAD, 0.0), pill_r);
        assert_eq!(radius_at(80.0, RAD, 1.0), RAD);
        assert_eq!(radius_at(10.0, RAD, 1.0), 10.0, "never past a capsule");
    }

    /// The handover frames carry no radius snap: from the first frame `settled()` reports, to the
    /// caller's own radius, the step is a sub-pixel; and the first close frame is the same value.
    #[test]
    fn the_radius_is_continuous_into_and_out_of_the_settled_panel() {
        let mut m = Morph::new();
        let mut prev = m.frame(Some(PILL), PANEL, RAD).radius;
        let mut settled_at = None;
        for _ in 0..240 {
            m.step(true, DT);
            let r = m.frame(Some(PILL), PANEL, RAD).radius;
            if settled_at.is_none() && m.settled() {
                settled_at = Some(r);
                assert!((r - RAD).abs() < 0.05, "settling hands the caller's radius {RAD} a {r}");
                assert!((r - prev).abs() < 1.0, "no snap at settle ({prev} -> {r})");
            }
            prev = r;
        }
        assert!(settled_at.is_some());
        let at_rest = m.frame(Some(PILL), PANEL, RAD).radius;
        assert!((at_rest - RAD).abs() < 0.05, "the close starts on the caller's radius ({at_rest})");
    }

    /// The rows start on the THIRD glass frame (~33 ms, the owner's 2026-11-18 direction: the droplet is
    /// already a menu while it moves; iOS shows them at 67-100) and are opaque by ~120 ms; their SCALE is
    /// the lens spring's (`the_rows_follow_the_ios_lens_magnification`), not the glass's height ratio,
    /// 1 at rest.
    #[test]
    fn the_rows_show_from_the_third_glass_frame() {
        let mut m = Morph::new();
        let mut frames = vec![m.frame(Some(PILL), PANEL, RAD)];
        for _ in 0..60 {
            m.step(true, DT);
            frames.push(m.frame(Some(PILL), PANEL, RAD));
        }
        // `frames[0]` is the unstepped pill: the first glass frame is `frames[1]`, the table's frame 0
        let at = |ms: f32| frames[(ms / (DT * 1000.0)).round() as usize + 1];
        assert!((frames.last().unwrap().rows_scale - 1.0).abs() < 1e-3, "1 at rest");
        assert_eq!(at(0.0).content_alpha, 0.0, "none on the first glass frame");
        let first = frames.iter().position(|f| f.content_alpha > 0.0).expect("the rows show") - 1;
        assert!(first <= 3, "the rows' first visible frame is at or before glass frame 3 (frame {first}, ~{} ms)", first as f32 * DT * 1000.0);
        assert_eq!(first, 2, "the rows start on the third glass frame (table frame 2, ~33 ms), as the module doc says");
        assert!(at(50.0).content_alpha > 0.1 && at(50.0).content_alpha < 0.6, "arriving by ~50 ms: {:?}", at(50.0));
        assert!(at(83.0).content_alpha > 0.5 && at(83.0).content_alpha < 1.0, "still arriving at 83 ms: {:?}", at(83.0));
        assert!(at(133.0).content_alpha > 0.99, "opaque by ~133 ms: {:?}", at(133.0));
        assert!(frames.windows(2).take(12).all(|w| w[1].content_alpha >= w[0].content_alpha - 1e-6), "they only arrive on an open");
        assert!(frames.iter().all(|f| f.content_alpha <= f.ground_alpha), "never more opaque than the glass they stand on");
    }

    /// **The glass is never clear: no open frame and no close frame asks the glass for the sharp page**
    /// (owner, 2026-11-18, from the iOS 26 recording: the droplet is frosted from its first visible frame
    /// until the close hands back to the opener, never a transparent bead). Every frame of an open, a
    /// close, a close that interrupts an open (at every step of it) and an open that interrupts a close,
    /// with and without an opener fill to whiten the join toward, and for the fallback.
    #[test]
    fn no_open_or_close_frame_draws_a_clear_body() {
        let fill = Some([0.9, 0.9, 0.9, 0.9]);
        let check = |what: &str, f: &Frame| {
            for fill in [None, fill] {
                assert_eq!(f.glass_morph(fill).clear, 0.0, "{what}: a clear body: {f:?}");
            }
        };
        for from in [Some(PILL), None] {
            let (open, close) = open_then_close(from);
            let drawn = open.iter().chain(&close).filter(|f| f.ground_alpha > 0.0).count();
            assert!(drawn > 30, "the frames that draw glass were checked ({drawn})");
            open.iter().for_each(|f| check("open", f));
            close.iter().for_each(|f| check("close", f));
        }
        // a close that interrupts the open after `n` frames, and the open that interrupts it in turn
        for n in 0..14 {
            let mut m = Morph::new();
            for _ in 0..n {
                m.step(true, DT);
            }
            for _ in 0..40 {
                m.step(false, DT);
                check(&format!("close after {n} open frames"), &m.frame(Some(PILL), PANEL, RAD));
            }
            let mut m = Morph::new();
            for _ in 0..90 {
                m.step(true, DT);
            }
            for _ in 0..n {
                m.step(false, DT);
            }
            for _ in 0..40 {
                m.step(true, DT);
                check(&format!("re-open after {n} close frames"), &m.frame(Some(PILL), PANEL, RAD));
            }
        }
    }

    /// An open and the close after it, every frame.
    fn open_then_close(from: Option<Rect>) -> (Vec<Frame>, Vec<Frame>) {
        let mut m = Morph::new();
        let mut open = vec![m.frame(from, PANEL, RAD)];
        for _ in 0..120 {
            m.step(true, DT);
            open.push(m.frame(from, PANEL, RAD));
        }
        let mut close = Vec::new();
        for _ in 0..120 {
            m.step(false, DT);
            close.push(m.frame(from, PANEL, RAD));
        }
        (open, close)
    }

    /// Spec (d): the glass's share crosses in over the first [`FILL_AT`] of the growth and a close
    /// lands ON the pill's rect with none of it left.
    #[test]
    fn the_glass_crosses_in_over_the_first_of_the_growth_and_a_close_lands_on_the_pill() {
        let (open, close) = open_then_close(Some(PILL));
        for fr in open.iter().chain(&close) {
            if fr.progress >= FILL_AT {
                assert_eq!(fr.ground_alpha, 1.0, "all glass from {FILL_AT}: {fr:?}");
            }
        }
        assert!(open.iter().any(|fr| fr.ground_alpha > 0.0 && fr.ground_alpha < 1.0), "the fill crosses over frames");
        let last = close.last().unwrap();
        assert_eq!((last.rect, last.ground_alpha), (PILL, 0.0), "a close lands ON the pill");
    }

    /// Spec (c): no row on a surface that is less glass than row, none before the glass has begun to
    /// widen, and none left by the time the rect is back near the pill.
    #[test]
    fn rows_only_draw_on_a_surface_that_is_all_glass() {
        let (open, close) = open_then_close(Some(PILL));
        for fr in open.iter().chain(&close) {
            assert!(fr.content_alpha <= fr.ground_alpha, "a row more opaque than the glass under it: {fr:?}");
            if fr.content_alpha > 0.0 {
                assert!(fr.progress > ROWS_FROM_AT, "a row before the glass has begun to widen: {fr:?}");
            }
        }
        assert!(open.iter().take_while(|fr| fr.progress <= ROWS_FROM_AT).count() >= 2, "the rows wait for the glass");
        // The rows leave over ~50 ms on a close, shrinking with the slab (their scale is the glass's
        // height over the panel's), and are gone by ~67 ms (four frames).
        assert!(close[3].content_alpha < 0.05, "gone by ~67 ms: {:?}", close[3]);
        assert!(close[3].rows_scale < close[0].rows_scale, "and shrinking with the slab: {:?}", close[3]);
        assert_eq!(open.last().unwrap().content_alpha, 1.0);
    }

    /// The growing shape is all glass before it gets big.
    #[test]
    fn the_glass_is_whole_by_a_fifth_of_the_growth() {
        assert!(FILL_AT <= 0.18 + 1e-6);
        let (open, _) = open_then_close(Some(PILL));
        let big = open.iter().find(|fr| fr.rect.w > PILL.w + 0.2 * (PANEL.w - PILL.w)).expect("it grows");
        assert_eq!(big.ground_alpha, 1.0, "all glass by the time it is 20% grown: {big:?}");
        // the rows start while the glass is still crossing in (`ROWS_FROM_AT` is under `FILL_AT`), and are
        // held under its alpha (`content_alpha <= ground_alpha`, `rows_only_draw_on_a_surface_that_is_all_glass`)
        assert!(ROWS_FROM_AT < FILL_AT);
    }

    #[test]
    fn without_an_opener_rect_the_panel_fades_and_scales_about_its_centre() {
        let m = Morph::new();
        let fr = m.frame(None, PANEL, RAD);
        assert!((fr.rect.w - PANEL.w * FALLBACK_SCALE_FROM).abs() < 1e-3);
        assert!((fr.rect.cx() - PANEL.cx()).abs() < 1e-3 && (fr.rect.cy() - PANEL.cy()).abs() < 1e-3);
        assert_eq!(rect_at(None, PANEL, 1.0), PANEL);
        let (open, close) = open_then_close(None);
        assert!(open.iter().chain(&close).all(|fr| fr.rect.w <= PANEL.w + 1e-3), "no opener: never past the panel");
        assert_eq!(rect_at(Some(PILL), PANEL, 0.0), PILL);
        assert_eq!(rect_at(Some(PILL), PANEL, 1.0), PANEL);
        // and with no opener there is nothing to consume or join
        for fr in open.iter().chain(&close) {
            assert_eq!((fr.opener_alpha, fr.merge), (1.0, None), "the fallback has no droplet: {fr:?}");
        }
    }

    /// A settled open panel draws exactly as it did before the droplet: no merge, whole.
    #[test]
    fn a_settled_panel_has_no_merge_and_a_consumed_opener() {
        let (open, _) = open_then_close(Some(PILL));
        let last = open.last().unwrap();
        assert!((last.rect.x - PANEL.x).abs() + (last.rect.y - PANEL.y).abs() < 0.01, "on the panel");
        assert!((last.rect.w - PANEL.w).abs() + (last.rect.h - PANEL.h).abs() < 0.01, "on the panel");
        assert_eq!(last.glass_morph(None), plx_gfx::gfx::GlassMorph::NONE, "the settled glass is the plain one");
        assert_eq!((last.ground_alpha, last.content_alpha), (1.0, 1.0));
        assert_eq!(last.opener_alpha, 0.0, "the opener is consumed by the open panel");
    }

    /// OPEN: the glass is joined to the whole opener at the start, and the join leaves on its own threshold.
    /// (It is frosted all the way, `no_open_or_close_frame_draws_a_clear_body`.)
    #[test]
    fn the_open_starts_as_a_droplet_joined_to_the_opener_and_leaves_it_as_it_grows() {
        let (open, _) = open_then_close(Some(PILL));
        let first = open[0];
        let m = first.merge.expect("a merge at progress 0");
        assert_eq!((m.rect, m.k), (PILL, MERGE_K));
        assert_eq!(m.radius, PILL.h * 0.5);
        assert_eq!(first.opener_alpha, 1.0);
        for fr in &open {
            if fr.progress >= MERGE_END_AT {
                assert_eq!(fr.merge, None, "no merge once the shape is gone: {fr:?}");
            }
            if let Some(m) = fr.merge {
                assert!((m.rect.cx() - PILL.cx()).abs() < 1e-3 && (m.rect.cy() - PILL.cy()).abs() < 1e-3, "about its own centre");
                assert!(m.rect.w <= PILL.w + 1e-3 && m.radius <= PILL.h * 0.5 + 1e-3);
            }
        }
        for pair in open.windows(2) {
            let s = |f: &Frame| f.merge.map_or(0.0, |m| m.rect.w);
            assert!(s(&pair[1]) <= s(&pair[0]) + 1e-3, "the merge only shrinks on an open");
            assert!(pair[1].opener_alpha <= pair[0].opener_alpha + 1e-3, "the opener only fades on an open");
        }
        assert!(open.iter().any(|f| f.opener_alpha > 0.0 && f.opener_alpha < 1.0), "the opener fades over frames");
        assert!(open.iter().all(|f| f.progress < OPENER_OUT_AT || f.opener_alpha == 0.0), "consumed by {OPENER_OUT_AT}");
    }

    /// CLOSE, phase 1: the slab BULGES outward on the first frame (iOS: the bottom edge +17 px of
    /// ~750, about 2% of the height) before it shrinks, with the rows leaving over ~50 ms WITH it, not
    /// over a rect that holds (the frost stays, `no_open_or_close_frame_draws_a_clear_body`).
    #[test]
    fn a_close_bulges_first_and_the_rows_leave_with_the_slab() {
        let (open, close) = open_then_close(Some(PILL));
        let rest = open.last().unwrap().rect;
        let c1 = close[0];
        assert!(c1.rect.h > rest.h + 0.01 * rest.h && c1.rect.h < rest.h + 0.07 * rest.h, "the first frame bulges (iOS: 2.7% of the travel at 17 ms, ~7% at 33): {c1:?}");
        assert!(c1.content_alpha > 0.3 && c1.content_alpha < 0.95, "the content is leaving, not gone: {c1:?}");
        assert!(close[1].rect.h > rest.h, "the bulge holds to ~33 ms (iOS: 1.07 of the travel there): {:?}", close[1]);
        assert!(close[3].rect.h < rest.h, "and the slab is shrinking by ~67 ms: {:?}", close[3]);
        assert!(close[3].content_alpha < 0.05, "rows gone by ~67 ms: {:?}", close[3]);
        assert!(close.iter().skip(5).all(|f| f.content_alpha < 0.02), "no rows on the way back");
        for pair in close.windows(2).take(6) {
            assert!(pair[1].content_alpha <= pair[0].content_alpha + 1e-4);
        }
    }

    /// The close, stepped at 60 fps until the morph says it is closed (the frames the droplet draws).
    fn close_frames() -> Vec<Frame> {
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        let mut out = Vec::new();
        for _ in 0..120 {
            m.step(false, DT);
            out.push(m.frame(Some(PILL), PANEL, RAD));
            if m.closed() {
                break;
            }
        }
        assert!(m.closed(), "the close lands");
        out
    }

    /// CLOSE, phase 2: after the content has left, the frosted slab A lies BELOW the opener (in the
    /// drain band, closer than the neck reach) and is smaller than it: the drain rect is
    /// [`DRAIN_WIDTH`] of the opener's width, its height, a capsule, centred on it.
    #[test]
    fn a_close_sends_the_slab_to_a_drain_just_below_the_opener() {
        let drain = drain_rect(PILL);
        assert_eq!(drain.y, PILL.y + PILL.h + DRAIN_GAP, "its top is a few px under the opener's bottom");
        assert!(DRAIN_GAP < MERGE_K, "closer than the neck reach, so the glass can bridge it");
        assert!((drain.cx() - PILL.cx()).abs() < 1e-3, "centred on the opener");
        assert!((drain.w - PILL.w * DRAIN_WIDTH).abs() < 1e-3 && drain.w <= PILL.w, "{DRAIN_WIDTH} of its width, never wider");
        assert!(drain.h <= PILL.h + 1e-3 && drain.h > 0.9 * PILL.h, "about its height");
        let round = drain_rect(Rect::new(96.0, 54.0, 76.0, 76.0));
        assert!(round.h <= round.w + 1e-3, "a round opener drains as a round drop, never a tall one");
        let close = close_frames();
        // the slab's top only moves down toward the drain (the fold drains it before it arrives)
        let tops: Vec<f32> = close.iter().map(|f| f.rect.y).collect();
        assert!(tops[2..tops.len() - 1].windows(2).all(|w| w[1] >= w[0] - 1e-3), "the slab only moves down after its bulge: {tops:?}");
        let arrived = close[close.len() - 2];
        assert!(arrived.rect.y > PILL.y + 0.3 * PILL.h, "A hangs under the opener's top: {arrived:?}");
        assert!(arrived.rect.w * arrived.rect.h < PILL.w * PILL.h, "and is smaller than it (the fold has begun to drain it): {arrived:?}");
        // the slab only moves toward the drain (its rect shrinks) from the first frame the rect is released
        let released = close.iter().position(|f| f.rect != close[0].rect).expect("the rect leaves");
        for pair in close[released..close.len() - 1].windows(2) {
            assert!(pair[1].rect.h <= pair[0].rect.h + 1e-3, "the slab only shrinks on a close: {pair:?}");
        }
    }

    /// CLOSE, phase 3: the opener re-forms as its own blob B at its own spot (growing from its
    /// bottom edge, never past its own rect) and the slab drains up into it through the neck: A and B are both
    /// there for several frames with the merge active, A reaches nothing, the opener's face only
    /// returns, and the last frame is the closed frame exactly.
    #[test]
    fn a_close_drains_the_slab_into_the_reforming_opener_through_a_neck() {
        let close = close_frames();
        for f in &close {
            if f.progress >= MERGE_END_AT {
                assert_eq!(f.merge, None, "no blob while the slab is far: {f:?}");
            }
            if f.progress >= OPENER_OUT_AT {
                assert_eq!(f.opener_alpha, 0.0, "the opener stays consumed until the slab is near: {f:?}");
            }
            if let Some(b) = f.merge {
                assert!((b.rect.cx() - PILL.cx()).abs() < 1e-3, "B is centred across the opener");
                assert!((b.rect.y + b.rect.h - (PILL.y + PILL.h)).abs() < 0.5, "B grows from the opener's bottom edge, the one nearest the slab: {b:?}");
                assert!(b.rect.w <= PILL.w + 1e-3 && b.rect.h <= PILL.h + 1e-3 && b.k == MERGE_K, "B never exceeds the opener: {b:?}");
            }
        }
        let neck = close.iter().filter(|f| f.merge.is_some_and(|b| b.rect.w > 0.0) && f.rect.w > 0.0 && f.rect.h > 0.0 && f.ground_alpha > 0.0).count();
        assert!(neck >= 3, "A and B both have size, merged, on {neck} frames");
        let fold = close.iter().find(|f| f.rect.w < drain_rect(PILL).w).expect("the fold starts");
        let b = fold.merge.expect("B is there as A drains");
        assert!(b.rect.w > 0.3 * PILL.w && b.rect.w < PILL.w, "B is about half size as the fold begins: {b:?}");
        // B only grows, A only shrinks, and the opener's face only returns, once the fold is under way
        let from = close.iter().position(|f| f.progress <= FOLD_AT).expect("the fold");
        for pair in close[from..close.len() - 1].windows(2) {
            let (a, b) = (|f: &Frame| f.rect.w, |f: &Frame| f.merge.map_or(0.0, |m| m.rect.w));
            assert!(a(&pair[1]) <= a(&pair[0]) + 1e-3, "A drains: {pair:?}");
            assert!(b(&pair[1]) >= b(&pair[0]) - 0.05, "B grows (the open spring's residue aside): {pair:?}");
        }
        let alphas: Vec<f32> = close.iter().map(|f| f.opener_alpha).collect();
        assert!(alphas.windows(2).all(|w| w[1] >= w[0] - 1e-6), "the opener only returns: {alphas:?}");
        assert!(alphas.iter().any(|a| *a > 0.0 && *a < 1.0), "its face fades back in over frames: {alphas:?}");
        let last = close.last().unwrap();
        let tiny = close[close.len() - 2];
        assert!(tiny.rect.w < 0.2 * drain_rect(PILL).w, "A has all but gone a frame before the end: {tiny:?}");
        assert_eq!(*last, Morph::new().frame(Some(PILL), PANEL, RAD), "the last frame IS the closed frame");
        assert_eq!((last.rect, last.opener_alpha, last.ground_alpha), (PILL, 1.0, 0.0));
        let m = last.merge.expect("the opener's own blob, whole");
        assert_eq!((m.rect, m.radius), (PILL, PILL.h * 0.5));
        assert!((11..=17).contains(&close.len()), "about 14 frames (230 ms) at 60 fps, got {}", close.len());
    }

    /// The openers a morph closes into: the header pill the suite uses, the profile chip (a 60 px
    /// circle) and a tall, wide control. A taller opener has its blob's bottom edge further from the
    /// slab's top while the blob is small, which is where the neck used to break.
    const OPENERS: [Rect; 3] = [PILL, Rect::new(76.0, 40.0, 60.0, 60.0), Rect::new(1400.0, 40.0, 240.0, 56.0)];
    /// How far inside the glass the union must be at the middle of the gap between the blob and the
    /// slab, in px. Below it the neck is a hair from breaking and reads as a gap once it is
    /// antialiased.
    const NECK_MARGIN: f32 = 1.0;
    /// The smallest the blob may be on its first frame, as a share of the opener's height: below it
    /// the smooth minimum sees a point, not a shape, and turns it into a one-frame cusp.
    const BLOB_MIN_SIDE: f32 = 0.25;

    /// The close for `pill`, under a panel that covers it (its top edge is the opener's, as
    /// `popover::cover_opener` hangs every morphing menu), stepped at 60 fps until the morph says it
    /// is closed.
    fn close_frames_for(pill: Rect) -> Vec<Frame> {
        close_frames_for_panel(pill, covering_panel(pill))
    }

    /// The panel `popover::cover_opener` hangs over `pill`: its top edge is the opener's.
    fn covering_panel(pill: Rect) -> Rect {
        Rect::new(pill.x + pill.w - PANEL.w.max(pill.w), pill.y, PANEL.w.max(pill.w), PANEL.h)
    }

    /// [`close_frames_for`] under an explicit `panel`.
    fn close_frames_for_panel(pill: Rect, panel: Rect) -> Vec<Frame> {
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        let mut out = Vec::new();
        for _ in 0..120 {
            m.step(false, DT);
            out.push(m.frame(Some(pill), panel, RAD));
            if m.closed() {
                break;
            }
        }
        assert!(m.closed(), "the close lands for {pill:?}");
        out
    }

    /// The glass's own union at `p`: the slab A (`a`, its rect and corner radius) joined to the blob
    /// B (`b`) by `fs_glass.frag`'s polynomial smooth minimum, `h = clamp(0.5 + 0.5 (db - da) / k, 0,
    /// 1)`, `d = mix(db, da, h) - k h (1 - h)`. Inside the glass where it is negative. Halfway
    /// between two flat edges `g` apart both distances are `g / 2` and `h` is a half, so the union is
    /// `g / 2 - k / 4`: the neck bridges a gap up to `k / 2`, [`MERGE_K`] / 2 = 14 px.
    fn union_sdf(p: (f32, f32), a: (Rect, f32), b: &Merge) -> f32 {
        let (da, db) = (sd_box(p, a.0, a.1), sd_box(p, b.rect, b.radius));
        let h = (0.5 + 0.5 * (db - da) / b.k).clamp(0.0, 1.0);
        db + (da - db) * h - b.k * h * (1.0 - h)
    }

    /// CLOSE, the neck: from the first frame B exists until A is gone, the two are JOINED, on every
    /// frame and at every opener (owner film, 2026-11-17: on B's second frame it was a small capsule
    /// floating above the slab with a gap, and a frame later it was joined by a neck). The union of
    /// the two, evaluated at B's centre across, halfway between B's bottom edge and A's top, is
    /// inside the glass by [`NECK_MARGIN`].
    ///
    /// Red on the tree where B grew about the opener's centre from nothing: a taller opener's
    /// bottom edge is further above the slab's top than the neck reaches while B is small.
    #[test]
    fn a_close_keeps_the_blob_joined_to_the_slab_on_every_frame_both_exist() {
        for pill in OPENERS {
            let mut both = 0;
            for (n, f) in close_frames_for(pill).iter().enumerate() {
                let Some(b) = f.merge.filter(|b| b.rect.w > 0.0 && b.rect.h > 0.0) else { continue };
                if f.rect.w <= 0.0 || f.rect.h <= 0.0 {
                    continue;
                }
                both += 1;
                let mid = (b.rect.y + b.rect.h + f.rect.y) * 0.5;
                let d = union_sdf((b.rect.cx(), mid), (f.rect, f.radius), &b);
                assert!(
                    d < -NECK_MARGIN,
                    "opener {pill:?}, close frame {n}: B and A come apart (union {d} px at the middle of a {} px gap): {f:?}",
                    f.rect.y - (b.rect.y + b.rect.h)
                );
            }
            assert!(both >= 6, "A and B both stand for several frames at {pill:?}: {both}");
        }
    }

    /// CLOSE, the birth: B is a bead with a size of its own on its first frame, not a point (owner
    /// film: a one-frame spike on the slab's top edge). Its smaller side is at least
    /// [`BLOB_MIN_SIDE`] of the opener's height.
    #[test]
    fn the_blob_is_born_big_enough_to_join_the_slab_without_a_cusp() {
        for pill in OPENERS {
            let first = close_frames_for(pill).iter().find_map(|f| f.merge).expect("the opener re-forms");
            assert!(
                first.rect.w.min(first.rect.h) >= BLOB_MIN_SIDE * pill.h,
                "opener {pill:?}: B starts {} x {} px, under {BLOB_MIN_SIDE} of its height",
                first.rect.w, first.rect.h
            );
        }
    }

    /// The B tint of a frame: how much of the opener's focused fill the blob wears (0 with no blob).
    fn tint_of(f: &Frame) -> f32 {
        f.merge.map_or(0.0, |m| m.tint)
    }

    /// CLOSE, the colour of B (owner film, 2026-11-17: B was dark glass on every frame of its
    /// re-forming and popped to the white focused pill on the hand-back frame): B's face blends
    /// from the glass toward the opener's focused fill, 0 when it is born, rising over the re-form
    /// and the fold, and the last frame that draws glass wears all of it (the hand-back frame
    /// itself, the closed one, draws NO glass: the host's lift paints the pill). No frame steps by
    /// more than a third of the range at 60 fps, at every opener.
    #[test]
    fn a_close_whitens_the_blob_toward_the_openers_fill_without_a_step() {
        let mut worst = 0.0_f32;
        for pill in OPENERS {
            let close = close_frames_for(pill);
            let (hand_back, drawn) = close.split_last().expect("frames");
            assert_eq!(hand_back.ground_alpha, 0.0, "the closed frame draws no glass, so its tint is not a pixel: {hand_back:?}");
            let born = drawn.iter().position(|f| f.merge.is_some_and(|b| b.rect.w > 0.0)).expect("B is born");
            assert!(drawn[..born].iter().all(|f| tint_of(f) == 0.0), "no blob, no tint");
            let tints: Vec<f32> = drawn[born..].iter().map(tint_of).collect();
            assert!(tints[0] < 0.05, "opener {pill:?}: B is born as glass, not as the pill ({})", tints[0]);
            for w in tints.windows(2) {
                assert!(w[1] >= w[0] - 1e-6, "opener {pill:?}: the tint only rises: {tints:?}");
                worst = worst.max(w[1] - w[0]);
            }
            let last = *tints.last().unwrap();
            assert!(last >= 0.97, "opener {pill:?}: the last glass frame wears the opener's fill ({last}): {tints:?}");
            assert!(tints.len() >= 6, "opener {pill:?}: it takes several frames, not a switch: {tints:?}");
        }
        assert!(worst <= 1.0 / 3.0, "no per-frame step past a third of the range at 60 fps ({worst})");
    }

    /// CLOSE: the opener's own face (the host's lift, [`Frame::opener_alpha`]) never leads the
    /// tint: a face at some alpha over a dark blob is a grey pill, over a white one it only brings
    /// its label in. Both are whole on the closed frame.
    #[test]
    fn the_openers_face_never_outruns_the_blobs_tint_on_a_close() {
        for pill in OPENERS {
            let close = close_frames_for(pill);
            let (hand_back, drawn) = close.split_last().expect("frames");
            for (n, f) in drawn.iter().enumerate() {
                assert!(
                    f.opener_alpha <= tint_of(f) + 1e-3,
                    "opener {pill:?}, close frame {n}: a grey half-alpha pill over dark glass (face {} over tint {})",
                    f.opener_alpha, tint_of(f)
                );
            }
            assert_eq!(hand_back.opener_alpha, 1.0, "the face is whole when the glass is gone");
        }
    }

    /// OPEN: the opener's face and the glass CROSS-FADE on one progress (frame 0 draws no glass
    /// at all; from then the face falls as the glass rises) and the merge shape wears no tint: the
    /// open has no hand-over frame at which white is replaced by dark, so the merge is left as it was.
    #[test]
    fn the_open_cross_fades_the_face_into_the_glass_and_wears_no_tint() {
        let (open, _) = open_then_close(Some(PILL));
        assert_eq!((open[0].ground_alpha, open[0].opener_alpha), (0.0, 1.0), "frame 0 draws no glass: the face alone");
        let pairs: Vec<(f32, f32)> = open.iter().take(4).map(|f| (f.opener_alpha, f.ground_alpha)).collect();
        assert!(
            pairs.iter().any(|&(face, glass)| face > 0.0 && face < 1.0 && glass > 0.0),
            "the face and the glass are both there on an in-between frame: {pairs:?}"
        );
        assert!(open.iter().all(|f| tint_of(f) == 0.0), "an open's merge shape is never whitened");
    }

    /// The fill the caller names reaches the glass as a colour with the frame's tint as its alpha:
    /// the colour is the caller's (never a literal here), the alpha is `Merge::tint`; with no fill
    /// the join is not whitened at all.
    #[test]
    fn the_glass_morph_carries_the_callers_fill_and_the_frames_tint() {
        let close = close_frames_for(PILL);
        let f = close[close.len() - 2];
        let fill = crate::theme::ACCENT;
        let tint = f.glass_morph(Some(fill)).merge.expect("B joins").tint;
        assert_eq!((tint[0], tint[1], tint[2]), (fill[0], fill[1], fill[2]), "the caller's colour");
        assert!((tint[3] - tint_of(&f) * fill[3]).abs() < 1e-6 && tint[3] > 0.9, "the frame's amount: {tint:?}");
        assert_eq!(f.glass_morph(None).merge.expect("B joins").tint[3], 0.0, "no fill, no whitening");
        assert_eq!(Morph::new().frame(None, PANEL, RAD).glass_morph(Some(fill)), plx_gfx::gfx::GlassMorph::NONE);
    }

    /// OPEN: now that the panel covers its opener, the first frame IS the opener's live rect (and its
    /// round corner), and the open is continuous: the rect only grows from there, with no jump.
    #[test]
    fn the_open_starts_exactly_on_the_openers_rect_over_a_panel_that_covers_it() {
        let panel = Rect::new(PILL.x + PILL.w - 316.0, PILL.y, 316.0, 400.0);
        assert!(panel.x <= PILL.x && panel.y == PILL.y && panel.w >= PILL.w && panel.h >= PILL.h, "premise: the panel covers the opener");
        let mut m = Morph::new();
        let first = m.frame(Some(PILL), panel, RAD);
        assert_eq!((first.rect, first.radius), (PILL, PILL.h * 0.5), "the first frame is the opener's rect and corner");
        let mut prev = first.rect;
        // until the width's first peak (~215 ms): then it rings back
        for _ in 0..12 {
            m.step(true, DT);
            let r = m.frame(Some(PILL), panel, RAD).rect;
            assert!(r.y == PILL.y && r.x <= prev.x + 1e-3 && r.x + r.w >= prev.x + prev.w - 1e-3, "grows out of itself, never jumping: {prev:?} -> {r:?}");
            prev = r;
        }
    }

    /// The `out` spring is stiff (k 9000: ω·dt = 1.6 at 60 fps), but it is the closed form, so it
    /// is stable at any frame time: in range and monotone at 60, 30 and 15 fps.
    #[test]
    fn the_content_phase_is_monotone_and_in_range_at_any_frame_time() {
        for dt in [DT, 2.0 * DT, 4.0 * DT] {
            let mut m = Morph::new();
            for _ in 0..120 {
                m.step(true, dt);
            }
            assert!(m.settled());
            let mut prev = m.frame(Some(PILL), PANEL, RAD);
            for _ in 0..40 {
                m.step(false, dt);
                let f = m.frame(Some(PILL), PANEL, RAD);
                for v in [f.content_alpha, f.opener_alpha, f.ground_alpha] {
                    assert!((0.0..=1.0).contains(&v), "in range at dt {dt}: {f:?}");
                }
                assert!(f.content_alpha <= prev.content_alpha + 1e-4, "content only leaves at dt {dt}");
                prev = f;
            }
            assert!(m.closed());
        }
    }

    /// A re-open during a close brings the content back: `out` springs home and the panel settles.
    #[test]
    fn a_reopen_during_a_close_brings_the_content_back_and_settles() {
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        for _ in 0..4 {
            m.step(false, DT);
        }
        assert!(!m.settled() && m.frame(Some(PILL), PANEL, RAD).content_alpha < 0.2);
        for _ in 0..120 {
            m.step(true, DT);
        }
        let f = m.frame(Some(PILL), PANEL, RAD);
        assert!(m.settled(), "the re-open settles");
        assert!(f.content_alpha > 0.999 && f.merge.is_none() && f.glass_morph(None).clear == 0.0, "{f:?}");
    }

    /// The modal dim of a MORPHING menu follows its morph (`Popover::dim`, the shared place every
    /// header menu's `Screen::scrim` goes through): in by ~150 ms and out by ~200 ms, where the
    /// container's own appear spring (k 300) is 73% at 150 ms and still 14% at 200 ms on the way out.
    /// A popover that does not morph asks for the container's spring, unchanged.
    #[test]
    fn the_dim_follows_the_morph_for_a_morphing_menu_and_is_the_containers_for_any_other() {
        let _g = plx_base::testlock::serial();
        let mut pop = crate::popover::Popover::new().morphing();
        pop.open_from(Some(PILL));
        let dim = |p: &crate::popover::Popover| p.dim(0.30).ramp.expect("a morphing menu's dim has its own ramp");
        assert_eq!(dim(&pop), 0.0, "no dim before the first step");
        let mut coming = Vec::new();
        for _ in 0..40 {
            pop.update(DT);
            coming.push(dim(&pop));
        }
        assert!(coming[9] >= 0.98, "in by ~150 ms: {}", coming[9]);
        assert!(coming[2] < 0.5, "and not in two frames: {}", coming[2]);
        assert!(coming.windows(2).all(|w| w[1] >= w[0] - 1e-3), "it only comes in: {coming:?}");
        assert_eq!(*coming.last().unwrap(), 1.0);
        pop.dismiss();
        let mut going = Vec::new();
        for _ in 0..30 {
            pop.update(DT);
            going.push(dim(&pop));
        }
        assert!(going[0] > 0.9, "a close has not dimmed out on its first frame: {}", going[0]);
        assert!(going[12] <= 0.05, "out by ~217 ms: {}", going[12]);
        assert_eq!(going[13], 0.0, "and gone with the slab, on the hand-back frame (233 ms)");
        assert!(going.windows(2).all(|w| w[1] <= w[0] + 3e-3), "it only goes out: {going:?}");
        let plain = crate::popover::Popover::new();
        let scrim = plain.dim(0.30);
        assert_eq!((scrim.alpha, scrim.ramp), (0.30, None), "a popover that does not morph keeps the container's spring");
    }

    /// iOS 26's glass menu CLOSE, measured frame by frame (the owner's recording, 2026-11-17, 60 fps,
    /// per-pixel diff against the static page, t = 0 the press frame; `series.md`, "Iteration 2"):
    /// the slab's normalized width and height, 0 = the button, 1 = the panel at rest, as
    /// `(frame, width, height)` for 17 ms .. 250 ms. The width reaches the button's by ~133 ms (the
    /// table's flat 0.00 after it is the bbox floor, the button's own width: the slab keeps narrowing
    /// under it), the height BULGES ~7% at 33 ms and drains over ~250 ms.
    const IOS_CLOSE: [(usize, f32, f32); 15] = [
        (1, 0.98, 1.027), (2, 0.88, 1.07), (3, 0.68, 1.05), (4, 0.54, 0.97), (5, 0.40, 0.87), (6, 0.26, 0.74),
        (7, 0.12, 0.61), (8, 0.0, 0.50), (9, 0.0, 0.39), (10, 0.0, 0.29), (11, 0.0, 0.20), (12, 0.0, 0.13),
        (13, 0.0, 0.08), (14, 0.0, 0.04), (15, 0.0, 0.01),
    ];

    /// The close follows the iOS recording ON WHAT IS DRAWN: the bbox of the slab A (after the fold's
    /// shrink) and the blob B, as the glass draw receives them (`Frame::rect`, `Frame::merge`),
    /// normalized against the opener as the table is, per frame within 0.08. The hand-back frame draws
    /// no glass: the opener, `(0, 0)`, as does every frame after it. (The first version of this fitted
    /// the springs' travel BEFORE the fold shrank it, and the drawn slab collapsed by a third of the
    /// panel in one frame at +133 ms.)
    #[test]
    fn the_close_follows_the_ios_reference_curve() {
        let drawn = drawn_close_series(PILL, PANEL);
        let mut worst = 0.0_f32;
        for &(n, want_w, want_h) in &IOS_CLOSE {
            let (w, h) = drawn.get(n - 1).copied().unwrap_or((0.0, 0.0));
            worst = worst.max((w - want_w).abs()).max((h - want_h).abs());
            assert!((w - want_w).abs() <= 0.08, "close frame {n}: drawn width {w} vs {want_w}: {drawn:?}");
            assert!((h - want_h).abs() <= 0.08, "close frame {n}: drawn height {h} vs {want_h}: {drawn:?}");
        }
        assert!(worst > 0.0);
    }

    /// The drawn height FALLS smoothly through the table's tail: from the bulge's peak on it never rises,
    /// and no frame (the hand-back included) drops more than 12.5% of the panel's height, at every
    /// opener. The hand-back is at 200-250 ms (iOS: whole at ~200, the droplet gone by ~250).
    #[test]
    fn the_drawn_close_drains_smoothly_to_the_hand_back() {
        for pill in OPENERS {
            let series = drawn_close_series(pill, covering_panel(pill));
            // heights as a share of the PANEL's height (the series is normalized against panel - opener)
            let to_panel = (covering_panel(pill).h - pill.h) / covering_panel(pill).h;
            let heights: Vec<f32> = series.iter().map(|s| s.1 * to_panel).chain([0.0]).collect();
            for (n, w) in heights.windows(2).enumerate().skip(2) {
                assert!(w[1] <= w[0] + 1e-3, "opener {pill:?}: the drawn height rises at frame {n}: {heights:?}");
                assert!(w[0] - w[1] <= 0.125, "opener {pill:?}: the drawn slab drops {} of the panel at frame {n}: {heights:?}", w[0] - w[1]);
            }
            let hand_back_ms = series.len() as f32 * DT * 1000.0;
            assert!((200.0..=250.0).contains(&hand_back_ms), "opener {pill:?}: the hand-back at {hand_back_ms} ms");
        }
    }

    /// The fold merges the slab into the opener WHILE IT TRAVELS (the table: the bbox drains through
    /// ~200 ms): on the frame it begins the slab is already in the last fifth of its way to the drain,
    /// its top within a quarter of the way up from the drain band, under the opener's top.
    #[test]
    fn the_fold_begins_when_the_slab_is_nearly_at_the_drain() {
        let mut m = Morph::new();
        for _ in 0..120 {
            m.step(true, DT);
        }
        let drain = drain_rect(PILL);
        let begun = (0..40)
            .find_map(|_| {
                m.step(false, DT);
                (m.fold.pos > 0.0).then(|| m.frame(Some(PILL), PANEL, RAD))
            })
            .expect("the fold begins");
        assert!(m.h.pos <= FOLD_AT + 0.1, "the slab has all but arrived when the fold begins ({})", m.h.pos);
        assert!(begun.rect.y >= drain.y - 0.25 * (drain.y - PANEL.y), "A is in the drain band, at or under the opener: {begun:?}");
        assert!(begun.rect.y + begun.rect.h > PILL.y + PILL.h, "and still reaches below it: {begun:?}");
    }

    /// The rows' lens magnification, measured (same recording; `series.md` "ROWS", t = 0 the first glass
    /// frame): content scale as `(ms, scale)`. Not the glass's height ratio: smaller inside a small lens,
    /// overshooting more (12% against the height's 9%), settled by ~360 ms.
    const IOS_ROWS: [(f32, f32); 11] = [
        (67.0, 0.65), (150.0, 1.11), (167.0, 1.12), (183.0, 1.117), (200.0, 1.10), (217.0, 1.073), (233.0, 1.054),
        (267.0, 1.036), (300.0, 1.016), (333.0, 1.007), (367.0, 1.0),
    ];

    #[test]
    fn the_rows_follow_the_ios_lens_magnification() {
        let mut m = Morph::new();
        let mut frames = vec![m.frame(Some(PILL), PANEL, RAD)];
        for _ in 0..40 {
            m.step(true, DT);
            frames.push(m.frame(Some(PILL), PANEL, RAD));
        }
        let at = |ms: f32| frames[(ms / (DT * 1000.0)).round() as usize + 1];
        for &(ms, want) in IOS_ROWS.iter().filter(|(ms, _)| *ms >= 150.0) {
            assert!((at(ms).rows_scale - want).abs() <= 0.06, "{ms} ms: {} vs {want}", at(ms).rows_scale);
        }
        let first = frames.iter().find(|f| f.content_alpha > 0.0).expect("the rows show");
        assert!(first.rows_scale < 1.0, "small inside a small lens when they first show: {first:?}");
        assert!(frames.iter().map(|f| f.rows_scale).fold(0.0_f32, f32::max) > 1.09, "the rows overshoot more than the glass");
    }

    /// While small (and while it drains) the glass is a round lens: its corner radius is half its smaller
    /// side; it lands on the panel radius exactly. Per frame the radius may move by no more than the
    /// lens side's own change, plus 30% of its distance from the panel radius (the ease), plus 2 px.
    /// (The literal "15% of the panel radius" cannot hold: the lens side itself steps 18 px on frame 1.)
    #[test]
    fn the_glass_is_a_round_lens_while_small_and_lands_on_the_panel_radius() {
        let (open, close) = open_then_close(Some(PILL));
        for ms in [50.0, 100.0] {
            let f = open[(ms / (DT * 1000.0)).round() as usize + 1];
            let lens = f.rect.w.min(f.rect.h) * 0.5;
            assert!((f.radius - lens).abs() <= 0.1 * lens, "{ms} ms: radius {} vs half the smaller side {lens}", f.radius);
        }
        assert_eq!(open.last().unwrap().radius, RAD, "exactly the panel radius once settled");
        let side = |f: &Frame| f.rect.w.min(f.rect.h) * 0.5;
        let all: Vec<Frame> = open.iter().chain(close.iter()).copied().collect();
        for (n, w) in all.windows(2).enumerate() {
            let jump = (w[1].radius - w[0].radius).abs();
            let gap = (w[0].radius - RAD).abs().max((w[1].radius - RAD).abs());
            // The open (and its settle) meets the strict bound. A close's radius DEPARTS the panel's:
            // its first step is the whole of its own distance from it, so no spring can keep that to
            // 30% of the gap; there the bound is half the lens-to-panel span (a snap is all of it).
            let allowed = if n + 1 < open.len() {
                (side(&w[1]) - side(&w[0])).abs() + 0.3 * gap + 2.0
            } else {
                (side(&w[1]) - side(&w[0])).abs() + 0.5 * (side(&w[1]) - RAD).abs() + 2.0
            };
            assert!(jump <= allowed, "frame {n}: radius {} -> {} (allowed {allowed})", w[0].radius, w[1].radius);
        }
        let last_drawn = close[close.len() - 2];
        assert!(last_drawn.radius <= side(&last_drawn) + 1e-3, "a close ends round: {last_drawn:?}");
    }

    /// **The settle hands over without a pop.** The rows are cut by the lens (its rect and radius)
    /// while the morph runs and by nothing once it has settled, so on the last frames before the
    /// settle the rows' own box (the panel inset by a typical row margin, zoomed as the rows are)
    /// must already lie inside the lens's rounded corners: dropping the cut then crops nothing. A
    /// corner that was outside would be a row corner appearing at the settle.
    #[test]
    fn the_rows_lie_inside_the_lens_on_the_last_frames_before_the_settle() {
        const ROW_MARGIN: f32 = 12.0;
        let mut m = Morph::new();
        let mut unsettled = Vec::new();
        for _ in 0..240 {
            m.step(true, DT);
            if m.settled() {
                break;
            }
            unsettled.push(m.frame(Some(PILL), PANEL, RAD));
        }
        assert!(unsettled.len() > 20, "the open ran {} unsettled frames", unsettled.len());
        for f in &unsettled[unsettled.len() - 4..] {
            let z = rows_zoom(Some(PILL), PANEL, f.rows_scale);
            let b = PANEL.inset(ROW_MARGIN);
            let placed = Rect::new(z.ox + (b.x - z.ox) * z.s, z.oy + (b.y - z.oy) * z.s, b.w * z.s, b.h * z.s);
            for c in [(placed.x, placed.y), (placed.x + placed.w, placed.y), (placed.x, placed.y + placed.h), (placed.x + placed.w, placed.y + placed.h)] {
                assert!(sd_box(c, f.rect, f.radius) <= -0.5, "a row corner {c:?} is cut at the settle: {f:?}");
            }
        }
    }

    /// The glass the painter receives on a close frame: the slab A (`Frame::rect`) joined to the opener's
    /// blob B (`Frame::merge`), as the bbox of both, which is what the recording's table measures.
    fn drawn_union(f: &Frame) -> Rect {
        let mut u = f.rect;
        if let Some(b) = f.merge.filter(|b| b.rect.w > 0.0 && b.rect.h > 0.0) {
            let (x0, y0) = (u.x.min(b.rect.x), u.y.min(b.rect.y));
            let (x1, y1) = ((u.x + u.w).max(b.rect.x + b.rect.w), (u.y + u.h).max(b.rect.y + b.rect.h));
            u = Rect::new(x0, y0, x1 - x0, y1 - y0);
        }
        u
    }

    /// The close's normalized drawn size per frame: the bbox of A and B against the opener, as the table
    /// is (0 = the opener, 1 = the panel), width clamped at the table's flat 0.00 (the button's own width).
    /// The hand-back frame, which draws no glass, is the opener: `(0, 0)`.
    fn drawn_close_series(pill: Rect, panel: Rect) -> Vec<(f32, f32)> {
        close_frames_for_panel(pill, panel)
            .iter()
            .map(|f| {
                if f.ground_alpha <= 0.0 {
                    return (0.0, 0.0);
                }
                let u = drawn_union(f);
                (((u.w - pill.w) / (panel.w - pill.w)).max(0.0), (u.h - pill.h) / (panel.h - pill.h))
            })
            .collect()
    }
}
