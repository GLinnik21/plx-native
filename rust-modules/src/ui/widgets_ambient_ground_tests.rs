//! `AmbientWash`'s legibility contract and the `PageGround` policy built on it.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ── AmbientWash: the page GROUND's legibility contract ───────────────────────────────────
//
// All pure math over `theme` tokens — no GL, no globals, so these are ordinary parallel tests.
// `Spring::step` (used by the dissolve test) is `gfx::spring`, closed-form arithmetic.

/// One sRGB channel, linearized (WCAG 2.x). Local to the tests on purpose: the app never needs
/// this — it is the yardstick the design decision was made with, kept here so the decision stays
/// checkable rather than remembered.
/// The corner sources a ground has to survive: the blown-out extreme, each primary and secondary
/// at full saturation, a mid grey, and the brightest thing in the palette.
fn hostile_sources() -> Vec<[f32; 3]> {
    vec![
        [1.0, 1.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 1.0],
        [1.0, 0.0, 1.0],
        [0.5, 0.5, 0.5],
        [
            theme::WASH_WARM[0],
            theme::WASH_WARM[1],
            theme::WASH_WARM[2],
        ],
        [
            theme::TEXT_PRIMARY[0],
            theme::TEXT_PRIMARY[1],
            theme::TEXT_PRIMARY[2],
        ],
    ]
}

/// **The legibility contract, executable.** Every corner of an artwork-keyed ground at
/// [`AmbientWash::GROUND_W`] must clear 3:1 against [`theme::TEXT_TERTIARY`] — the dimmest ink
/// the app puts on a ground (cast roles, unfocused episode summaries, the About column's labels,
/// all at `size::CAPTION`) — and 7:1 against [`theme::TEXT_PRIMARY`]. This is the test that would
/// have caught the UNCAPPED version (a white corner lands tertiary at 1.98:1), and it is the one
/// that fails the day someone raises `GROUND_W` or `GROUND_LUMA` past what a page can carry. It
/// guards the person page and the detail page at once, because both go through `keyed`.
#[test]
fn a_ground_never_outshines_the_fine_print_that_sits_on_it() {
    for src in hostile_sources() {
        let g = AmbientWash::keyed([src; 4], [AmbientWash::GROUND_W; 4]);
        for (i, corner) in g.iter().enumerate() {
            let t = contrast(theme::TEXT_TERTIARY, *corner);
            assert!(t >= 3.0, "corner {i} of {src:?}: TEXT_TERTIARY at {t:.2}:1, under the 3:1 large-text floor");
            let p = contrast(theme::TEXT_PRIMARY, *corner);
            assert!(p >= 7.0, "corner {i} of {src:?}: TEXT_PRIMARY at {p:.2}:1");
        }
    }
}

/// The other end: a ground keyed to near-black key art must not sink below the value a card's
/// drop shadow needs to read against. `SURFACE_APP`'s own doc rejects the old near-black
/// (25,25,29)/255 for exactly that reason; `GROUND_W ≤ 0.26` is what keeps the floor above it,
/// with no floor constant anywhere.
#[test]
fn a_ground_stays_light_enough_for_a_card_shadow() {
    let floor = AmbientWash::keyed([[0.0; 3]; 4], [AmbientWash::GROUND_W; 4]);
    let rejected = [25.0 / 255.0, 25.0 / 255.0, 29.0 / 255.0, 1.0];
    for corner in floor {
        for ch in 0..3 {
            let want = theme::SURFACE_APP[ch] * (1.0 - AmbientWash::GROUND_W);
            assert!(
                (corner[ch] - want).abs() < 1e-6,
                "the darkest ground is the surface scaled by 1-GROUND_W"
            );
            assert!(
                corner[ch] > rejected[ch],
                "channel {ch} sank to {} — into the near-black the palette rejects",
                corner[ch]
            );
        }
    }
}

/// "No artwork is the app's own flat ground" — stated in the type's docs since the person page
/// shipped, pinned here. At weight 0 the mix must be EXACTLY the surface, so a screen needs no
/// has-envelope branch in its draw.
#[test]
fn no_artwork_is_the_apps_own_ground() {
    for src in hostile_sources() {
        let quad = [[src[0], src[1], src[2], 1.0]; 4];
        assert_eq!(
            AmbientWash::target(quad, [0.0; 4]),
            [theme::SURFACE_APP; 4],
            "src {src:?}"
        );
    }
}

/// The cap is a scalar multiply, not a per-channel clamp: a bright saturated corner comes back
/// at exactly [`GROUND_LUMA`] with its channel RATIOS untouched (a clamp would desaturate it
/// toward white), and a corner already under the ceiling comes back bit-identical.
#[test]
fn the_luma_cap_spends_brightness_and_nothing_else() {
    let bright = [0.9, 0.7, 0.2];
    let c = ground_capped(bright);
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    assert!(
        (y - GROUND_LUMA).abs() < 1e-4,
        "capped luma {y} != {GROUND_LUMA}"
    );
    let k = c[0] / bright[0];
    for ch in 0..3 {
        assert!(
            (c[ch] / bright[ch] - k).abs() < 1e-5,
            "channel {ch} was scaled by a different factor — that is a clamp, not a cap"
        );
    }
    assert_eq!(c[3], 1.0, "a ground corner is opaque");

    let dark = [0.10, 0.20, 0.05];
    assert_eq!(
        ground_capped(dark),
        [dark[0], dark[1], dark[2], 1.0],
        "a corner under the ceiling is untouched"
    );
}

/// A tl/tr/br/bl transposition is invisible on a near-symmetric gradient in a screenshot and
/// wrong on every asymmetric one — the exact hazard `UltraBlurColors::corners`' doc warns about
/// (the JSON reading order is not the ring order). Both constructors must be index-preserving.
#[test]
fn the_corners_keep_their_ring_order() {
    let src = [
        [0.8, 0.1, 0.1],
        [0.1, 0.8, 0.1],
        [0.1, 0.1, 0.8],
        [0.7, 0.7, 0.1],
    ];
    let w = [AmbientWash::GROUND_W; 4];
    let keyed = AmbientWash::keyed(src, w);
    for i in 0..4 {
        let alone = AmbientWash::keyed([src[i]; 4], w);
        assert_eq!(keyed[i], alone[0], "corner {i} did not stay at index {i}");
    }
    let quad: [[f32; 4]; 4] = std::array::from_fn(|i| [src[i][0], src[i][1], src[i][2], 1.0]);
    let plain = AmbientWash::target(quad, w);
    for i in 0..4 {
        assert_eq!(
            plain[i],
            theme::mix(theme::SURFACE_APP, quad[i], w[i]),
            "target moved corner {i}"
        );
    }
}

/// The skip test a screen uses to avoid a full-screen fill that changes nothing: flat on the
/// ground it is drawn over → skippable; dissolving toward a different colour → not; settled back
/// → skippable again. Drives the real corner springs, so it also pins that a dissolve actually
/// converges within the ~0.5 s the rate promises.
#[test]
fn a_wash_that_has_resolved_to_the_ground_is_flat() {
    const EPS: f32 = AmbientWash::FLAT_EPS;
    let mut w = AmbientWash::flat(theme::SURFACE_APP);
    assert!(
        w.is_flat(theme::SURFACE_APP, EPS),
        "a wash mounted on the ground is already flat"
    );

    let away = [[0.9, 0.2, 0.1, 1.0]; 4];
    for _ in 0..6 {
        w.step(away, AmbientWash::K, 1.0 / 60.0);
    }
    assert!(
        !w.is_flat(theme::SURFACE_APP, EPS),
        "a wash on its way to a colour is not the ground"
    );

    for _ in 0..60 {
        w.step([theme::SURFACE_APP; 4], AmbientWash::K, 1.0 / 60.0);
    }
    assert!(
        w.is_flat(theme::SURFACE_APP, EPS),
        "a second of dissolve must land back on the ground"
    );
}

#[test]
fn page_ground_canonical_state_covers_every_held_target_and_spring_component() {
    let base = ground_hash(&PageGround::new());
    for corner in 0..4 {
        for component in 0..4 {
            let mut changed = PageGround::new();
            changed.target[corner][component] += 0.125;
            assert_ne!(ground_hash(&changed), base, "held target {corner}/{component}");
        }
        for component in 0..3 {
            let mut changed = PageGround::new();
            changed.wash.corners[corner][component].pos += 0.125;
            assert_ne!(ground_hash(&changed), base, "position {corner}/{component}");
            let mut changed = PageGround::new();
            changed.wash.corners[corner][component].vel = 0.125;
            assert_ne!(ground_hash(&changed), base, "velocity {corner}/{component}");
        }
    }
}

#[test]
fn page_ground_canonical_state_is_repeatable_and_read_only_through_a_held_dissolve() {
    let _guard = crate::testlock::serial();
    let (mut left, mut right) = (PageGround::new(), PageGround::new());
    for frame in 0..20 {
        let source = (frame == 0).then_some([[0.8, 0.1, 0.2]; 4]);
        left.key(source, PageGround::CARD_W, 1.0 / 60.0);
        right.key(source, PageGround::CARD_W, 1.0 / 60.0);
        let hash = ground_hash(&left);
        assert_eq!(hash, ground_hash(&left), "hashing must not step motion");
        assert_eq!(hash, ground_hash(&right), "equal inputs must have equal canonical state");
    }
}

/// **The rule the type exists for**: an item with no `UltraBlurColors` envelope must leave the
/// ground where it is, not clear it. Without this, one artless poster between two coloured ones
/// flashes the whole page grey and back on the way past — and the failure is invisible on a
/// library whose artwork all carries envelopes, which is most of them.
#[test]
fn an_item_with_no_envelope_holds_the_ground_instead_of_clearing_it() {
    let dt = 1.0 / 60.0;
    let red = [[0.9, 0.1, 0.1]; 4];
    let (mut held, mut control) = (PageGround::new(), PageGround::new());
    assert!(held.is_flat(), "a ground mounts on the app's own surface");

    for _ in 0..30 {
        held.key(Some(red), PageGround::CARD_W, dt);
        control.key(Some(red), PageGround::CARD_W, dt);
    }
    assert!(!held.is_flat(), "half a second on a coloured tile keys the ground");

    // …and now the ring walks across artless tiles for a second, while the control's tile keeps
    // its envelope. The two must be indistinguishable: `None` is "nothing new to say", not
    // "there is nothing here". Graded against a still-converging control rather than against a
    // frozen snapshot, because the spring is *supposed* to keep closing on the held target.
    for _ in 0..60 {
        held.key(None, PageGround::CARD_W, dt);
        control.key(Some(red), PageGround::CARD_W, dt);
    }
    for (a, b) in held.corners().iter().flatten().zip(control.corners().iter().flatten()) {
        assert!(
            (a - b).abs() <= 1e-6,
            "an artless tile moved the ground to {a}, where holding gives {b}"
        );
    }
    assert!(!held.is_flat(), "…and it is certainly not back on the flat clear");
}

/// A ground that has resolved to the app's own clear colour must report itself flat, because
/// `draw` skips it there and that skip IS this component's fill-rate story: a full-screen
/// ambient pass is ~2M fragments for a gradient identical to the `frame_clear` under it.
#[test]
fn a_ground_keyed_to_nothing_is_a_pass_the_screen_can_skip() {
    let mut g = PageGround::new();
    assert!(g.is_flat(), "nothing keyed, nothing to draw");
    // an explicit target of the surface itself resolves back to skippable
    let blue = AmbientWash::keyed([[0.1, 0.2, 0.9]; 4], PageGround::CARD_W);
    for _ in 0..60 {
        g.key_target(blue, 1.0 / 60.0);
    }
    assert!(!g.is_flat(), "a keyed ground is drawn");
    for _ in 0..60 {
        g.key_target([theme::SURFACE_APP; 4], 1.0 / 60.0);
    }
    assert!(g.is_flat(), "a second of dissolve back to the surface is skippable again");
}

/// [`PageGround::jump_target`] is for a page that has changed SUBJECT: the previous subject's
/// colours must be gone on the frame the new one mounts, not dissolving across it.
#[test]
fn a_page_changing_subject_jumps_rather_than_washing_the_old_one_across_it() {
    let mut g = PageGround::new();
    let red = AmbientWash::keyed([[0.9, 0.1, 0.1]; 4], PageGround::CARD_W);
    let green = AmbientWash::keyed([[0.1, 0.9, 0.1]; 4], PageGround::CARD_W);
    g.jump_target(red);
    g.jump_target(green);
    for (c, t) in g.corners().iter().zip(green) {
        for (a, b) in c.iter().zip(t) {
            assert_eq!(*a, b, "a jump lands ON the target, with nothing in between");
        }
    }
}

/// The shared arrangement is the shared STRENGTH across the top — one number, so two browsing
/// screens cannot lean their grounds by two different amounts and read as two products.
#[test]
fn the_shared_card_arrangement_leans_by_the_shared_ground_strength() {
    assert_eq!(PageGround::CARD_W[0], AmbientWash::GROUND_W);
    assert_eq!(PageGround::CARD_W[1], AmbientWash::GROUND_W);
    assert!(
        PageGround::CARD_W[2] < PageGround::CARD_W[0]
            && PageGround::CARD_W[3] < PageGround::CARD_W[1],
        "…and fades toward the bottom, where the content rows are"
    );
}

/// **[`AmbientWash::sample`] is the shader's own bilinear, on the CPU** — the whole point of it
/// is that a caller can paint the EXACT colour the ground already puts at a point, and an
/// approximation would show as a rectangle rather than a dissolve.
///
/// Graded against `fs_ambient.frag`'s two mixes (`mix(mix(tl,tr,u), mix(bl,br,u), v)`) at the
/// four corners and at the centre, on a wash whose corners are four distinguishable colours —
/// which is also what pins the CORNER ORDER (tl, tr, br, bl, the order `Painter::ambient` and
/// `Painter::grad4` both take): swapping any pair still passes a test written on a flat wash.
#[test]
fn the_ambient_wash_reports_the_colour_the_shader_puts_at_a_point() {
    let tl = [1.0, 0.0, 0.0];
    let tr = [0.0, 1.0, 0.0];
    let br = [0.0, 0.0, 1.0];
    let bl = [1.0, 1.0, 0.0];
    let mut w = AmbientWash::flat(theme::SURFACE_APP);
    w.jump([
        [tl[0], tl[1], tl[2], 1.0],
        [tr[0], tr[1], tr[2], 1.0],
        [br[0], br[1], br[2], 1.0],
        [bl[0], bl[1], bl[2], 1.0],
    ]);
    let r = Rect::new(100.0, 50.0, 400.0, 200.0);
    let close = |a: [f32; 3], b: [f32; 3], what: &str| {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-4, "{what}: {a:?} against {b:?}");
        }
    };
    close(w.sample(r, r.x, r.y), tl, "top left");
    close(w.sample(r, r.x + r.w, r.y), tr, "top right");
    close(w.sample(r, r.x + r.w, r.y + r.h), br, "bottom right");
    close(w.sample(r, r.x, r.y + r.h), bl, "bottom left");
    close(
        w.sample(r, r.x + r.w * 0.5, r.y + r.h * 0.5),
        [0.5, 0.5, 0.25],
        "centre",
    );
    // …and a point outside the rect clamps rather than extrapolating: a fade band drawn at the
    // very edge of the screen must not ask for a colour the gradient never had.
    close(w.sample(r, r.x - 500.0, r.y - 500.0), tl, "clamped past the top left");
}
