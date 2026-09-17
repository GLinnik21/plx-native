//! The top bar's glass blur-region budget (track + unfurled chip as one priced pair) and the dynamic glass refresh cadence.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **[`GLASS_TRACK_MAX`] is the budget, solved for width** — asserted rather than asserted-in-a-
/// comment, because the two numbers live in different files and the one that moves is the
/// budget. A glass surface is charged for the blurred RECTANGLE: itself grown
/// [`crate::gfx::BLUR_MARGIN`] on every side. At the limit width that must still fit
/// [`crate::gfx::GLASS_REGION_BUDGET`], which is what a MOVING host carries at 60 fps — and this
/// bar's host is always moving, since the page under it is what the user is scrolling.
///
/// **It is priced as the PAIR now**, and that is the change worth reading twice. This asserted
/// the track alone against the budget, which was the whole story while the track was the only
/// glass in the band; [`profile_chip_with`]'s capsule is a second surface of the same material, and
/// two glass surfaces in one frame converge on ONE grab (`gfx::blur_region_union`) that has to
/// span both. A track that passes on its own and fails as a pair is exactly the regression this
/// exists to catch, so the counterexample moved with it: the old one was a 1050-wide track,
/// which is far outside either limit, where 940 — the width the TRACK alone could afford — is
/// inside its own budget and outside the band's. That is the boundary the second surface moved.
/// [`BAND_REGION_BUDGET`] is a restatement of a `cfg(test)` constant, so it can only be kept
/// honest by an equality. Both halves of [`GLASS_TRACK_MAX`] are also re-derived here through
/// `gfx::blur_region` itself, which is the check that `BAND_REGION_H`'s clamp assumption still
/// holds — the constant would silently over-state the region if the bar ever dropped past
/// `BLUR_MARGIN`.
#[test]
fn the_band_budget_restated_here_is_the_measured_one() {
    assert_eq!(BAND_REGION_BUDGET, crate::gfx::GLASS_REGION_BUDGET);
    let (h, y) = (TAB_PILL_H + 2.0 * TAB_TRACK_PAD, TOP_BAR_Y - TAB_TRACK_PAD);
    let reg = crate::gfx::blur_region(0.0, y, crate::ui::consts::SCR_W, h);
    assert_eq!(reg[3], BAND_REGION_H, "the band's real region height");
}

#[test]
fn the_whole_bands_glass_fits_one_region_budget() {
    assert!(
        band_region(GLASS_TRACK_MAX) <= crate::gfx::GLASS_REGION_BUDGET,
        "the band at the limit costs {:.0} px^2, past the {:.0} a moving host carries",
        band_region(GLASS_TRACK_MAX),
        crate::gfx::GLASS_REGION_BUDGET,
    );
    assert!(
        band_region(940.0) > crate::gfx::GLASS_REGION_BUDGET,
        "940 was the TRACK's own limit and must be outside the BAND's: {:.0} px^2",
        band_region(940.0),
    );
}

/// **The unfurled chip and the glass track can never touch** — the one thing about this band
/// that no shader can fix, so it is arithmetic and it is asserted.
///
/// There is ONE blur cache. A second glass surface drawn over the first samples that same
/// snapshot, so an overlap gets no second blur — only a second scrim and a second rim
/// composited over material that already carries both, which is the doubling `draw_tab_row`'s
/// source-pass note measures from the other side (a face reading (52,60,38) came out at
/// (33,38,26) once it contained itself). The capsule is bounded by [`CHIP_NAME_MAX`] and the
/// track is centred, so the clearance is two constants and needs no font.
///
/// **Be clear about what each half can catch, because as long as [`GLASS_TRACK_MAX`] is SOLVED
/// for this both are identities.** That is the design working — the constant is the clearance,
/// so nothing is left to check — and it means the assertions only bite under an EDIT. The first
/// fires the moment the limit goes back to being a literal (it was `940.0` until 2026-08-21,
/// which leaves the widest capsule 42px inside the widest track). The second is the opposite
/// guard, and it is NOT "a wider track would be overlapped" — a track past the limit wears no
/// glass at all, so it can never be overlapped as glass. It pins the clearance TIGHT: within a
/// pixel of [`BAND_AIR`] and not an arbitrary gulf, so nobody buys safety here by quietly
/// spending the strip.
///
/// The capsule side is the rect [`profile_chip_with`] actually draws ([`chip_cap`]), which is the
/// half that could have gone wrong on its own: [`CHIP_CAP_MAX_R`] hand-restated those seven
/// terms until it was made to ask for them.
#[test]
fn the_unfurled_chip_never_reaches_the_glass_track() {
    let track_x = |w: f32| (crate::ui::consts::SCR_W - w) * 0.5;
    let cap = chip_cap(1.0, CHIP_NAME_MAX);
    assert_eq!(
        cap.x + cap.w,
        CHIP_CAP_MAX_R,
        "priced and drawn are one expression"
    );
    assert!(
        cap.x + cap.w + BAND_AIR <= track_x(GLASS_TRACK_MAX) + 0.01,
        "the widest capsule ends at {} and the widest glass track starts at {} — they must \
         clear each other by {}",
        cap.x + cap.w,
        track_x(GLASS_TRACK_MAX),
        BAND_AIR,
    );
    // **The cap is TIGHT against whichever constraint binds** — a limit that gives away more
    // strip than either rule needs is a cost nobody decided to pay. This used to be pinned
    // within a pixel of `BAND_AIR`, which said the same thing while the touch rule was the one
    // that bound; since the bar dropped to clear the overscan frame the BUDGET binds instead,
    // so the clearance above is legitimately wider and the tightness claim has to be made
    // against the widest track the budget admits.
    //
    // Both bounds are RE-DERIVED here rather than read back: the touch one off the rect
    // `chip_cap` draws, the budget one by searching `band_region` — which asks
    // `gfx::blur_region_union` itself. Comparing `GLASS_TRACK_MAX` against the two module
    // constants it is literally the `min` of would be an identity that only NaN could fail, and
    // it would have missed exactly the error this catches: `GLASS_TRACK_BUDGET_MAX`'s closed
    // form assumed a union left edge of 0, which stopped being true when the chip moved to
    // `MARGIN_X`.
    let touch = crate::ui::consts::SCR_W - 2.0 * (cap.x + cap.w + BAND_AIR);
    let budget = {
        let (mut lo, mut hi) = (0.0f32, crate::ui::consts::SCR_W);
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if band_region(mid) <= crate::gfx::GLASS_REGION_BUDGET {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    let want = touch.min(budget);
    assert!(
        (GLASS_TRACK_MAX - want).abs() <= 1.0,
        "the cap is {GLASS_TRACK_MAX} where the binding constraint allows {want} \
         (touch {touch}, budget {budget})",
    );
    assert!(
        budget < touch,
        "the BUDGET is what binds today — if that flips, so does the note above"
    );
    // the capsule only ever grows RIGHTWARD off a fixed edge, which is what lets one number
    // stand for the band at every point of the unfurl (see `band_region`).
    for e in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let c = chip_cap(e, CHIP_NAME_MAX);
        assert_eq!(
            (c.x, c.y, c.h),
            (cap.x, cap.y, cap.h),
            "only the width moves"
        );
        assert!(c.w <= cap.w + 0.01, "the rest capsule is the widest");
    }
}

/// The unfurl fades the whole FACE, and the two rim weights are the half that matters.
///
/// `fs_glass.frag` writes `max(u_tint.a * cov, rimw)`: the rim is deliberately allowed to exceed
/// its own surface's coverage, so that a 1px line survives its antialiased edge. Fade only the
/// tint and the consequence is a bright empty hairline capsule snapping on around the avatar in
/// the first milliseconds of focus — visible, and not something the tint can walk back.
///
/// And at rest it is the track's face to the bit, which is the claim the whole arrangement
/// rests on: one solve, one material, two surfaces.
#[test]
fn the_chips_capsule_fades_its_rim_with_its_scrim_and_rests_on_the_tracks_own_face() {
    let face = crate::gfx::GlassFace {
        scrim_top: [0.0, 0.0, 0.0, 0.40],
        scrim_bot: [0.0, 0.0, 0.0, 0.52],
        rim: [1.0, 1.0, 1.0, 0.14],
        rim_lit: [1.0, 1.0, 1.0, 0.28],
        rim_w: 1.0,
    };
    let rest = chip_face(face, 1.0);
    for (a, b) in [
        (rest.scrim_top, face.scrim_top),
        (rest.scrim_bot, face.scrim_bot),
        (rest.rim, face.rim),
        (rest.rim_lit, face.rim_lit),
    ] {
        assert_eq!(a, b, "at rest the chip wears the track's face unchanged");
    }
    let half = chip_face(face, 0.5);
    assert_eq!(half.scrim_top[3], 0.20, "the scrim fades with the unfurl");
    assert_eq!(half.rim[3], 0.07, "…and so does the perimeter");
    assert_eq!(
        half.rim_lit[3], 0.14,
        "…and the lit edge, which the tint alone cannot reach"
    );
    assert_eq!(
        half.rim[..3],
        face.rim[..3],
        "the lamp's COLOUR does not move; its weight does"
    );
    assert_eq!(
        half.rim_w, face.rim_w,
        "one design-system pixel, at every point of the unfurl"
    );
}

/// The chip is a contained control before it is focused: the resting surround is a circle in
/// the same inset band as the tab track, and focus only WIDENS it rightward for the name —
/// `profile_chip` draws `chip_cap(e, …)` at every `e`, at full face.
#[test]
fn the_profile_chip_has_a_round_surround_at_rest_and_only_widens_on_focus() {
    let closed = chip_cap(0.0, CHIP_NAME_MAX);
    let open = chip_cap(1.0, CHIP_NAME_MAX);
    assert_eq!(closed.w, closed.h, "the resting surround is circular");
    assert_eq!((closed.x, closed.y, closed.h), (open.x, open.y, open.h));
    assert!(
        open.w > closed.w,
        "focus reveals the name by widening rightward"
    );
}

/// The width rule is the only refusal a SECTION TABLE can trip on its own, so it has to hold
/// for the strip the product actually draws and give way for one the server could produce.
#[test]
fn a_normal_strip_keeps_the_material_and_a_long_one_loses_it() {
    // Home + three libraries + the square Search mark, at the widths `with_tab_metrics`
    // measures: label + 2 x TAB_PILL_PAD.
    let normal = [126.0, 146.0, 176.0, TAB_ICON_PILL_W];
    assert!(
        tab_track_w(&normal) <= GLASS_TRACK_MAX,
        "the product's own strip is {} wide and must keep the material",
        tab_track_w(&normal),
    );
    let many = [
        126.0,
        146.0,
        176.0,
        150.0,
        160.0,
        140.0,
        170.0,
        TAB_ICON_PILL_W,
    ];
    assert!(
        tab_track_w(&many) > GLASS_TRACK_MAX,
        "eight pills is {} wide and must drop to flat",
        tab_track_w(&many),
    );
}

/// The shipped default, asserted in ONE place so moving it is a one-line decision with a
/// measurement behind it rather than a cascade through the cadence tests.
#[test]
fn the_shipped_cadence_is_every_changed_present() {
    assert_eq!(
        DEFAULT_DYNAMIC_PERIOD, 1,
        "measured on the direct source path: one-in-one costs +0.07% of the frame against \
         one-in-three, and 60.0 fps either way"
    );
}

/// The one-pass hero ground's WEDGE field must be the four-quad wedge, not a lookalike. Graded
/// at every corner of both quads [`hero_scrim_quads`] builds, because a bilinear quad IS its
/// corners — reproduce those and the interiors follow, since both expressions are the same
/// product of two linear factors. This is the test that would have caught a swapped feather
/// range or a peak taken at the wrong x.
#[test]
fn the_one_pass_ground_reproduces_the_wedge_at_every_corner_of_both_quads() {
    for strength in [0.0f32, 0.35, 1.0] {
        let wedge = hero_ground_wedge(strength);
        let (q, n) = hero_scrim_quads(strength, false);
        assert_eq!(n, 2, "home's hero has no right wedge");
        for (r, k) in q.iter().take(n) {
            // (tl, tr, br, bl) — the order `Painter::grad4` and `fs_ambient.frag` agree on
            for (corner, (x, y)) in [
                (k[0], (r.x, r.y)),
                (k[1], (r.x + r.w, r.y)),
                (k[2], (r.x + r.w, r.y + r.h)),
                (k[3], (r.x, r.y + r.h)),
            ] {
                let one = hero_ground_wedge_a(wedge, x, y);
                assert!(
                    (one - corner[3]).abs() < 1e-6,
                    "strength {strength} at ({x}, {y}): one-pass {one} vs quad {}",
                    corner[3]
                );
            }
        }
    }
}

/// …and the ATMOSPHERIC ramp must be the SCREEN's own curve, sampled where it bends. Home's is
/// a two-stop ramp with a midpoint knee and detail's a single linear stop, so the parameters
/// are the caller's — this pins the packing (`y0, knee, a_knee, a_foot`) against the curve the
/// legibility table is graded on, at both stops and either side of each.
#[test]
fn the_one_pass_ground_reproduces_the_screens_atmospheric_ramp() {
    for hero_a in [0.2f32, 0.6, 1.0] {
        let ramp = crate::ui::landing_hero::base_scrim_ramp(hero_a);
        for y in [
            0.0f32,
            200.0,
            HERO_BASE_SCRIM_Y0,
            400.0,
            600.0,
            702.0,
            900.0,
            crate::ui::consts::SCR_H,
        ] {
            let one = hero_ground_ramp_a(ramp, y);
            let quads = crate::ui::landing_hero::base_scrim_a(y, hero_a);
            assert!(
                (one - quads).abs() < 1e-6,
                "hero_a {hero_a} at y {y}: one-pass {one} vs screen {quads}"
            );
        }
    }
}

/// The whole reason one pass can stand in for three: two straight-alpha layers of ONE ink
/// compose as `a1 + a2 - a1*a2`, and the art under them folds into the same single blend. This
/// is `fs_hero.frag`'s algebra, executed — if it is wrong the panel shows a differently-lit
/// hero and nothing else in the suite would notice.
#[test]
fn one_blend_lands_where_three_stacked_ones_do() {
    let over = |dst: f32, src: f32, a: f32| dst * (1.0 - a) + src * a;
    for dst in [0.0f32, 0.31, 1.0] {
        for art in [0.0f32, 0.62, 1.0] {
            for aa in [0.0f32, 0.4, 1.0] {
                for a1 in [0.0f32, 0.25, 0.72] {
                    for a2 in [0.0f32, 0.5, 0.72] {
                        const INK: f32 = 0.04;
                        let three = over(over(over(dst, art, aa), INK, a1), INK, a2);
                        let b = a1 + a2 - a1 * a2;
                        let s = 1.0 - (1.0 - aa) * (1.0 - b);
                        let src = (art * aa * (1.0 - b) + INK * b) / s.max(1.0 / 4096.0);
                        let one = over(dst, src, s);
                        assert!(
                            (one - three).abs() < 1e-5,
                            "dst {dst} art {art} A {aa} a1 {a1} a2 {a2}: {one} vs {three}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn dynamic_glass_refreshes_on_every_third_successful_present() {
    let mut clock = DynamicClock::new();
    clock.cover_now(41);
    assert_eq!(clock.step(42, true, P), DynamicStep::Wait);
    assert_eq!(
        clock.step(43, false, P),
        DynamicStep::Wait,
        "pending damage survives"
    );
    assert_eq!(clock.step(44, false, P), DynamicStep::Refresh);
    assert_eq!(
        clock.step(44, true, P),
        DynamicStep::None,
        "same-present owners are covered"
    );
    assert_eq!(
        clock.step(45, false, P),
        DynamicStep::None,
        "a clean keepalive does nothing"
    );
}

#[test]
fn dynamic_glass_cadence_survives_the_present_counter_wrapping() {
    let mut clock = DynamicClock::new();
    clock.cover_now(u32::MAX - 1);
    assert_eq!(clock.step(u32::MAX, true, P), DynamicStep::Wait);
    assert_eq!(clock.step(0, false, P), DynamicStep::Wait);
    assert_eq!(clock.step(1, false, P), DynamicStep::Refresh);
}

#[test]
fn staggered_dynamic_owners_share_one_global_cadence() {
    let mut clock = DynamicClock::new();
    clock.cover_now(10); // owner A opens
    assert_eq!(clock.step(11, true, P), DynamicStep::Wait);
    clock.cover_now(12); // owner B opens: its immediate shared capture covers A too
    assert_eq!(clock.step(12, true, P), DynamicStep::None);
    assert_eq!(clock.step(13, true, P), DynamicStep::Wait);
    assert_eq!(clock.step(14, true, P), DynamicStep::Wait);
    assert_eq!(clock.step(15, true, P), DynamicStep::Refresh);
    assert_eq!(
        clock.step(15, true, P),
        DynamicStep::None,
        "B cannot schedule a second capture"
    );
}

/// A period of one refreshes on EVERY present with a dirty underlay — the 60 Hz end of the
/// cadence sweep. It must still refuse a second capture on a present already covered, or two
/// glass owners would run the chain twice in one frame and the cost curve would measure that.
#[test]
fn a_period_of_one_refreshes_every_dirty_present_but_only_once_each() {
    let mut clock = DynamicClock::new();
    clock.cover_now(70);
    assert_eq!(clock.step(71, true, 1), DynamicStep::Refresh);
    assert_eq!(
        clock.step(71, true, 1),
        DynamicStep::None,
        "already covered this present"
    );
    assert_eq!(clock.step(72, true, 1), DynamicStep::Refresh);
    assert_eq!(
        clock.step(73, false, 1),
        DynamicStep::None,
        "a clean present still refreshes nothing"
    );
}

/// A longer period waits longer and no more than that: the wait count is `period - 1`.
#[test]
fn a_longer_period_waits_exactly_that_many_presents() {
    for period in 2..=8u32 {
        let mut clock = DynamicClock::new();
        clock.cover_now(0);
        for present in 1..period {
            assert_eq!(
                clock.step(present, true, period),
                DynamicStep::Wait,
                "period {period} refreshed early at present {present}"
            );
        }
        assert_eq!(
            clock.step(period, true, period),
            DynamicStep::Refresh,
            "period {period}"
        );
    }
}

/// `/tmp/plxnative-glasshz` writes this static and nothing else does. Zero would be a refresh
/// every zero frames, so it clamps rather than dividing the cadence by nothing.
#[test]
fn the_cadence_override_clamps_and_reports_what_it_installed() {
    // A crate-wide static, so this takes the shared globals lock rather than a local one.
    let _g = crate::testlock::serial();
    let restore = dynamic_period();
    assert_eq!(
        set_dynamic_period(0),
        1,
        "zero presents per refresh is not a cadence"
    );
    assert_eq!(dynamic_period(), 1);
    assert_eq!(set_dynamic_period(4), 4);
    assert_eq!(set_dynamic_period(99), 8, "past 8 is clamped, not refused");
    set_dynamic_period(restore);
    assert_eq!(
        dynamic_period(),
        DEFAULT_DYNAMIC_PERIOD,
        "the default is what ships"
    );
}

#[test]
fn glass_state_reactivates_after_deactivation_or_a_route_gap() {
    let mut state = GlassState::new();
    assert!(state.needs_activation(20));
    state.active = true;
    state.last_seen = 20;
    assert!(
        !state.needs_activation(21),
        "idle time has no successful presents"
    );
    assert!(
        state.needs_activation(22),
        "another route presented in between"
    );
    state.deactivate();
    assert!(state.needs_activation(21));
}

/// **Every glass policy COMPOSITES**, and the axis that let one not to is gone.
///
/// This test used to assert the source-dim arithmetic — `Glass::DYNAMIC.source_rgb(0.5) == 0.75`
/// and so on — for a preset nothing outside this module ever constructed. The mechanism was
/// measured against the item menu over one checker ground and rejected (`e75b5e49`): dimming the
/// page going INTO the backdrop destroys the modulation the frost is layered over, so the panel
/// arrives flat. What is worth pinning now is that no policy can reintroduce it by accident —
/// the two presets differ in REFRESH and in nothing else.
#[test]
fn every_glass_policy_composites_and_differs_only_in_refresh() {
    assert_ne!(
        Glass::CACHED,
        Glass::DYNAMIC_BACKDROP,
        "the two presets are not the same policy"
    );
    // **The policy carries NOTHING beside its refresh**, which is the property the deleted axis
    // violated — and the only form of it that can actually fail. Comparing a preset against a
    // literal of its own one field cannot: that is `Self{refresh} == Self{refresh}`, true by
    // construction, which is what the first version of this test asserted twice.
    assert_eq!(
        std::mem::size_of::<Glass>(),
        std::mem::size_of::<GlassRefresh>(),
        "a second axis on Glass would show up here first"
    );
}
