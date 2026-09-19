//! Shared fixtures and helpers for the `ui::widgets` test modules split out below.

use super::*;

/// The band's blurred region, in authored px^2, for a track `w` wide with the chip unfurled
/// beside it — through **`gfx`'s own `blur_region` and `blur_region_union`**, not a copy of them.
///
/// The copy is the thing to avoid here and there is a measurement to prove it: this test's
/// predecessor modelled the region inline and left the screen CLAMP out, which priced the tab
/// track at `(940+176) x 252` = 281k px^2 where the real region is `1116 x 200` = 223k — a 26%
/// over-estimate that sat under a passing assertion for as long as the limit existed, and the
/// reason the old limit read as if it had only ~7% of headroom when it had far more. Both
/// surfaces are grown `gfx::BLUR_MARGIN` a side and then clamped to the screen, so the capsule
/// at x=96 puts the union's left edge at 8 (`BAND_REGION_X0`), the band at y=36 puts its top on 0
/// and makes it 200 tall
/// rather than 252. That is `blur_region`'s business, and it is now asked rather than restated.
///
/// The rects are the DRAWN ones: `chip_cap` at rest with a name at its budget, and the centred
/// track `draw_tab_row` builds. Only the chip's left edge reaches the union — the capsule grows
/// rightward and the clearance keeps its right edge inside the track's — so the pair prices the
/// same at every point of the unfurl, which is why one number can stand for the band.
pub(super) fn band_region(w: f32) -> f32 {
    let (h, y) = (TAB_PILL_H + 2.0 * TAB_TRACK_PAD, TOP_BAR_Y - TAB_TRACK_PAD);
    let track = crate::gfx::blur_region((crate::ui::consts::SCR_W - w) * 0.5, y, w, h);
    let cap = chip_cap(1.0, CHIP_NAME_MAX);
    let chip = crate::gfx::blur_region(cap.x, cap.y, cap.w, cap.h);
    let u = crate::gfx::blur_region_union(track, chip);
    u[2] * u[3]
}



// ── The poster's state mark: one vocabulary, one mark at a time ───────────────────────────
//
// `card` needs a GL context, so the CHOICE is what a host test can reach — and the choice is
// where this has been wrong before: the mark used to say "unwatched" in amber, which is the
// opposite claim, and the bar/disc precedence is only observable on an item PMS reports as both.

/// A MOVIE row at a given watched state. `dur_ns` is 100 min, so `resume_ms` reads as a
/// percentage of the way in. For a LEAF the two flags really are each other's negation — which
/// is exactly what stops being true for a container, hence the show cases below.
pub(super) fn row(watched: bool, resume_ms: i64) -> PmsMovie {
    let mut m = PmsMovie::default();
    m.dur_ns = 100 * 60 * 1000 * 1_000_000;
    m.watched = watched;
    m.unwatched = !watched;
    m.resume_ms = resume_ms;
    m
}

// ── PageGround: the policy every browsing screen shares ────────────────────────────────────
//
// Pure over `theme` tokens and the real corner springs, as the block above is.

pub(super) fn ground_hash(ground: &PageGround) -> u64 {
    let mut c = crate::ui::machine::Canon::new();
    ground.write_motion(&mut c);
    c.finish()
}

pub(super) struct StatusMetrics;

impl crate::ui::machine::Measure for StatusMetrics {
    fn width(&self, _: &core::ffi::CStr, size: i32, bold: bool) -> f32 {
        // the status note wraps in the reason rung's regular face; everything else measured is
        // the action pill
        if size == STATUS_REASON_SZ && !bold {
            return 96.0;
        }
        assert_eq!(size, STATUS_CAP_SZ);
        assert!(bold, "the action uses the Button's bold face");
        96.0
    }
    fn cap_h(&self, _: i32) -> f32 { panic!("status layout uses line boxes") }
    fn line_h(&self, size: i32) -> f32 {
        match size {
            STATUS_FAILED_VERDICT_SZ => 52.0,
            STATUS_CAP_SZ => 40.0,
            STATUS_REASON_SZ => 28.0,
            _ => panic!("no status rung is {size}"),
        }
    }
}

// ---- the legibility contract itself -------------------------------------------------------

/// One sRGB channel, linearized (WCAG 2.x) — see the `AmbientWash` block above for why this
/// yardstick lives in the tests rather than in the app.
pub(super) fn srgb_lin(c: f32) -> f32 {
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The contrast ratio an ink gets over `art` (a flat encoded grey standing in for a backdrop)
/// once a scrim of total alpha `a` is composited over it. The panel is plain 888 with no sRGB
/// framebuffer anywhere in the tree, so token values ARE sRGB codes and GL's blend is a
/// straight lerp in that space: `c = art·(1−a) + SCRIM_INK·a`.
pub(super) fn contrast_over_art(ink: [f32; 4], art: f32, a: f32) -> f32 {
    let comp: Vec<f32> = (0..3)
        .map(|i| art * (1.0 - a) + theme::SCRIM_INK[i] * a)
        .collect();
    let l1 = 0.2126 * srgb_lin(ink[0]) + 0.7152 * srgb_lin(ink[1]) + 0.0722 * srgb_lin(ink[2]);
    let l2 =
        0.2126 * srgb_lin(comp[0]) + 0.7152 * srgb_lin(comp[1]) + 0.0722 * srgb_lin(comp[2]);
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

/// One HERO-72 bold cap band, the device font's — quoted, not measured, because the host suite
/// opens no SDL_ttf (the same boundary that keeps `detail::hero_chain` pure).
pub(super) const HERO_CAP_H: f32 = 52.0;

/// The height of home's synopsis block at its cap — the SHARED hero blurb
/// ([`crate::ui::hero_synopsis`]), so this table follows the rung and the leading instead of
/// quoting them. It was the literal `87.0` with `// three size::MICRO lines at the hero's 29px
/// leading` beside it, which is exactly the comment that goes stale in silence: the block is
/// `LABEL`/36 now, and 87 would have gone on grading the title 21px lower than it is drawn.
pub(super) fn home_syn_h() -> f32 {
    crate::ui::hero_syn_h(crate::ui::HERO_SYN_MAXLINES)
}

/// The TOP of home's title band, from the bottom-anchored stack the hero draws (`hero_content`):
/// the reserved logo band + `space::MD` + a one-line BODY kicker + `space::SM` + the synopsis
/// block, stacked UP from `HERO_TEXT_BOTTOM` through the screen's own `hero_stack_top`, so the
/// contract reads the arithmetic the draw uses. Only the KICKER's height is still quoted like
/// [`HERO_CAP_H`], because it is one line of a rung and the host opens no SDL_ttf.
pub(super) fn home_title_band_top() -> f32 {
    const META_H: f32 = 34.0; // one line of `size::BODY`
    crate::ui::landing_hero::stack_top(
        crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero),
        META_H,
        theme::space::SM + home_syn_h(),
    )
}

/// …and the cap top of the TEXT it falls back to, which is what the legibility table grades.
/// The fallback is BASELINED on the band's bottom edge (`hero_logo::place`'s optical rule), so
/// the ink sits a cap band above that line, ~70px below the band's own top. A clearLogo occupies
/// the band instead and reaches higher — that is art, graded by eye on a capture, not here.
pub(super) fn home_title_cap_top() -> f32 {
    home_title_band_top() + crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero)
        - HERO_CAP_H
}

/// The same line on the detail page, where the band's anchor is `TITLE_BOTTOM` outright.
pub(super) fn detail_title_cap_top() -> f32 {
    crate::ui::detail_layout::TITLE_BOTTOM - HERO_CAP_H
}

/// A spread of pill widths. Deliberately wider than the device's (a real "TV" pill measures
/// ≈67px at `size::BODY`, "Kids & Family Movies" ≈350) so a modest pill count crosses
/// [`TAB_VIEW_MAX`] and the overflow arithmetic is exercised without inventing 30 libraries —
/// the thresholds below are therefore about the geometry, not about a particular server.
pub(super) fn widths_for(n: usize) -> Vec<f32> {
    (0..n).map(|i| 140.0 + (i % 5) as f32 * 60.0).collect()
}

// `Spring::step` is `gfx::spring`, pure and already driven frame-by-frame by `card_row.rs`'s
// tests, so capsule MOTION is fully host-testable. What is not: anything through
// `with_tab_metrics` (it measures with SDL2_ttf), which is why these drive the pure
// `Capsule`/`TabStrip` against `widths_for` spans instead of the real row.

/// One frame at the app's own cadence, the value `card_row.rs`'s tests step with.
pub(super) const DT: f32 = 1.0 / 60.0;

/// Pill `i`'s content-space `(x, w)` in the fixture strip — the same pair
/// [`StripRender::update`] hands the strip, built from the same two functions.
pub(super) fn span_of(w: &[f32], i: usize) -> (f32, f32) {
    (tab_pill_x(w, i), w[i])
}

/// The capsule/strip tests drive `Capsule::step`, whose landing frame `Spring::jump`s — and
/// `Spring::jump` reports to `ui::idle`'s process-global dirty flag. So they are serial by
/// obligation, not precaution (`xfade.rs`'s rule): under parallel libtest they intermittently
/// failed OTHER modules' "a settled screen asks for nothing" assertions.
pub(super) fn serial_for_motion() -> crate::testlock::Serial {
    crate::testlock::serial()
}
