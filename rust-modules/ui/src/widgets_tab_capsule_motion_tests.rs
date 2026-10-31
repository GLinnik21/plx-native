//! The tab strip's travelling selection/focus capsules: canonical motion hashing, arrival, ink and the glass material they draw.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---- the travelling capsules --------------------------------------------------------------
#[test]
fn tab_strip_canonical_motion_covers_both_capsules_and_each_spring() {
    let hash = |strip: &TabStrip| {
        let mut c = plx_machine::machine::Canon::new();
        strip.write_motion(&mut c);
        c.finish()
    };
    let initial = hash(&TabStrip::new());
    for focused in [false, true] {
        for field in 0..7 {
            let mut strip = TabStrip::new();
            let capsule = if focused { &mut strip.foc } else { &mut strip.sel };
            match field {
                0 => capsule.x.pos = 1.0,
                1 => capsule.x.vel = 1.0,
                2 => capsule.w.pos = 1.0,
                3 => capsule.w.vel = 1.0,
                4 => capsule.a.pos = 1.0,
                5 => capsule.a.vel = 1.0,
                _ => capsule.at = 0,
            }
            assert_ne!(initial, hash(&strip), "capsule={focused} field={field}");
        }
    }
}

/// The one rule both the fill and the ink read, at every edge it has.
#[test]
fn cap_cover_is_the_pills_own_share_of_what_is_over_it() {
    let pill = (100.0, 200.0);
    assert_eq!(
        cap_cover(pill, pill),
        1.0,
        "a capsule sitting exactly on the pill covers it"
    );
    assert_eq!(
        cap_cover(pill, (0.0, 100.0)),
        0.0,
        "abutting on the left is not covering"
    );
    assert_eq!(cap_cover(pill, (300.0, 100.0)), 0.0, "…nor on the right");
    assert_eq!(
        cap_cover(pill, (0.0, 200.0)),
        0.5,
        "half over the left edge is half the pill"
    );
    assert_eq!(
        cap_cover(pill, (200.0, 400.0)),
        0.5,
        "…and half over the right edge likewise"
    );
    assert_eq!(
        cap_cover(pill, (-500.0, 5000.0)),
        1.0,
        "a capsule wider than the pill still covers 1, not more"
    );
    let z = cap_cover((100.0, 0.0), pill);
    assert_eq!(z, 0.0, "a zero-width pill covers nothing");
    assert!(
        z.is_finite(),
        "…and does not divide by zero into a NaN that would poison every ink"
    );
}

/// A fixture in the shape the two tests below need: seeded, in one material, with the OTHER

/// The contrast floor is a floor, so the two rates are not one rate: the bar darkens at the
/// page's own pace and clears at roughly half it. Symmetric rates would spend half of every
/// flicker under `TRACK_INK_CONTRAST`, which is the one thing the adaptive weight exists to hold.
#[test]
fn the_track_darkens_faster_than_it_clears() {
    let _g = serial_for_motion();
    assert!(
        density_k(0.40, 0.60) > density_k(0.60, 0.40),
        "getting darker protects the labels; getting lighter is cosmetic"
    );
    let dt = 1.0 / 60.0;
    let mut attack = crate::Spring::at(0.40);
    attack.step(0.60, density_k(0.40, 0.60), dt);
    let mut release = crate::Spring::at(0.60);
    release.step(0.40, density_k(0.60, 0.40), dt);
    assert!(
        (attack.pos - 0.40).abs() > (release.pos - 0.60).abs(),
        "over the same distance and one frame, attack covered {:.4} and release {:.4}",
        (attack.pos - 0.40).abs(),
        (release.pos - 0.60).abs(),
    );
}

/// The regression that would otherwise draw a bright capsule sweeping the whole strip on the
/// first Library frame: an UNPLACED capsule lands on its pill, it does not fly to it. Alpha still
/// fades in, because arriving in a row is a fade — the two are deliberately different motions.
#[test]
fn a_capsule_lands_on_its_first_pill_instead_of_flying_in_from_the_origin() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let p7 = span_of(&w, 7);
    let mut c = Capsule::new();
    c.step(Some((7, p7)), false, DT);
    assert_eq!(
        cap_cover(p7, c.span()),
        1.0,
        "the first frame must already be ON pill 7"
    );
    assert!(
        c.alpha() > 0.0 && c.alpha() < 1.0,
        "…while its alpha is still ramping up ({})",
        c.alpha()
    );
}

/// A capsule mid-travel must always be sitting on SOMETHING. The label ink is derived from
/// coverage, so a frame where neither neighbour is covered is a frame where both labels read as
/// plain while a bright capsule floats in the gutter between them.
#[test]
fn a_travelling_capsule_is_never_sitting_on_nothing() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let (p0, p1) = (span_of(&w, 0), span_of(&w, 1));
    let mut c = Capsule::new();
    for _ in 0..60 {
        c.step(Some((0, p0)), false, DT);
    }
    assert!(
        cap_cover(p0, c.span()) > 0.99,
        "the fixture must start settled on pill 0"
    );

    c.step(Some((1, p1)), false, DT);
    assert!(
        cap_cover(p0, c.span()) > 0.5,
        "one frame in it must still be mostly on pill 0, not teleported"
    );
    for f in 0..60 {
        c.step(Some((1, p1)), false, DT);
        let on = cap_cover(p0, c.span()).max(cap_cover(p1, c.span()));
        assert!(
            on > 0.0,
            "frame {f}: the capsule is on neither pill (span {:?})",
            c.span()
        );
    }
    assert!(
        cap_cover(p1, c.span()) > 0.99,
        "it must arrive fully on pill 1"
    );
    assert!(cap_cover(p0, c.span()) < 0.01, "…and leave pill 0 behind");
}

/// Focus leaving the row fades the capsule where it stands rather than parking it at the origin,
/// and coming back LANDS — the "capsule streaks across the row when you come back from the grid"
/// failure, which is the whole reason [`CAP_LAND_A`] exists.
#[test]
fn focus_leaving_the_row_fades_in_place_and_coming_back_lands() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let (p2, p9) = (span_of(&w, 2), span_of(&w, 9));
    let mut c = Capsule::new();
    for _ in 0..60 {
        c.step(Some((2, p2)), false, DT);
    }
    let held = c.span();
    for _ in 0..40 {
        c.step(None, false, DT);
    }
    assert!(
        c.alpha() < CAP_LAND_A,
        "focus off the row must fade the capsule out ({})",
        c.alpha()
    );
    assert_eq!(
        c.span(),
        held,
        "…in place: an invisible capsule must not also drift"
    );

    c.step(Some((9, p9)), false, DT);
    assert_eq!(
        cap_cover(p9, c.span()),
        1.0,
        "returning focus to a FAR pill lands on it, never glides"
    );
}

/// The two-capsule model, locked against a regression to one: on the Library screen the selected
/// tab is the section you are browsing while focus walks the row, so a pill can be wearing either,
/// both, or neither — and the mixes it is inked with are exactly that answer.
#[test]
fn the_ink_a_pill_wears_is_exactly_the_capsule_over_it() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let settle = |sel: c_int, foc: c_int| {
        let mut s = TabStrip::new();
        for _ in 0..90 {
            s.update(
                sel,
                foc,
                |i| (i < w.len()).then(|| span_of(&w, i)),
                SelMark::Travels,
                DT,
            );
        }
        s
    };
    let s = settle(2, 5);
    let near =
        |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
    assert!(
        near(s.mixes(span_of(&w, 2)), (0.0, 1.0)),
        "the selected pill wears only the selection"
    );
    assert!(
        near(s.mixes(span_of(&w, 5)), (1.0, 0.0)),
        "the focused pill wears only the focus"
    );
    assert!(
        near(s.mixes(span_of(&w, 0)), (0.0, 0.0)),
        "an idle pill wears neither"
    );
    let both = settle(3, 3);
    assert!(
        near(both.mixes(span_of(&w, 3)), (1.0, 1.0)),
        "one pill can wear both at once"
    );
    // …and nothing is marked at all when there is nothing to mark (an emptied season strip, or
    // focus off the row on a page that has no selection either).
    let none = settle(-1, -1);
    assert!(
        near(none.mixes(span_of(&w, 0)), (0.0, 0.0)),
        "no target means no ink anywhere"
    );
}

/// Coverage CONSERVATION across a travel: the two capsules only ever have one pill's worth of
/// coverage between them, so no frame can have two pills' labels both mostly under the focus
/// capsule. (What a label is inked with is `ink_spans`', split at the capsule's edge; this grades
/// the coverage those stretches are cut from.)
#[test]
fn a_travel_never_inks_more_than_one_pills_worth_of_label() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let mut s = TabStrip::new();
    let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
    for _ in 0..90 {
        s.update(-1, 4, span, SelMark::Travels, DT);
    }
    for f in 0..90 {
        s.update(-1, 5, span, SelMark::Travels, DT);
        let total: f32 = (0..w.len()).map(|i| s.mixes(span_of(&w, i)).0).sum();
        assert!(
            total <= 1.0 + 1e-3,
            "frame {f}: {total} pills' worth of focus ink is lit at once"
        );
        assert!(
            total > 0.0,
            "frame {f}: the focus ink went out entirely mid-travel"
        );
    }
}

/// **The season strip's and the page header row's selection LANDS; the top bar's travels.** Both
/// halves, because the rule is a difference between two callers and a regression would be one call site copied onto
/// the other.
///
/// The owner's report (2026-08-22) was that a grey pill sliding across the season row "does not
/// fit". It is graded on the capsule's POSITION one frame after the selection moves: a travelling
/// capsule is still near where it started, a landing one is already there.
#[test]
fn the_selection_mark_lands_on_a_season_strip_and_travels_on_a_tab_bar() {
    const DT: f32 = 1.0 / 60.0;
    let w = [140.0f32, 160.0, 150.0, 170.0];
    let span_of =
        |i: usize| -> (f32, f32) { (w.iter().take(i).map(|v| v + 20.0).sum::<f32>(), w[i]) };
    let span = |i: usize| (i < w.len()).then(|| span_of(i));
    let settle = |s: &mut TabStrip, sel: c_int, mark: SelMark| {
        for _ in 0..120 {
            s.update(sel, -1, span, mark, DT);
        }
    };
    let (far, near) = (span_of(3).0, span_of(0).0);

    // LANDS — one frame after the selection moves it is already at the new pill
    let mut season = TabStrip::new();
    settle(&mut season, 0, SelMark::Lands);
    assert!(
        (season.sel.span().0 - near).abs() < 0.5,
        "settled on pill 0"
    );
    season.update(3, -1, span, SelMark::Lands, DT);
    assert!(
        (season.sel.span().0 - far).abs() < 0.5,
        "a landing mark is AT the new pill on the very next frame, not on its way: {}",
        season.sel.span().0
    );

    // TRAVELS — one frame later it has barely left, which is the whole point of the other strip
    let mut bar = TabStrip::new();
    settle(&mut bar, 0, SelMark::Travels);
    bar.update(3, -1, span, SelMark::Travels, DT);
    let moved = bar.sel.span().0;
    assert!(
        moved > near && moved < near + (far - near) * 0.5,
        "a travelling mark is between the two after one frame: {moved}"
    );
}

/// Every RESTING state a strip-driven pill can be in must be the look it replaced, to the bit —
/// this is the whole reason the mix lerps between the same ink roles the boolean arms pick from
/// rather than inventing a ramp of its own.
///
/// **The IDLE role is the one deliberate exception, and it is a fact about grounds rather than
/// about pills.** The strip-driven row is the standing tab track: it sits on glass over
/// uncontrolled artwork, and its scrim is solved so this ink clears [`TRACK_INK_CONTRAST`] — a
/// muted `TEXT_TERTIARY` there costs .61 of black over a bright hero, which is a band laid across
/// the picture rather than a material. The boolean arms are the detail page's season tabs, drawn
/// bare on a controlled dark ground where nothing is solved and the muted ink is right. Two
/// grounds, two answers; the roles they SHARE still have to agree to the bit.
#[test]
fn a_settled_mixed_pill_is_the_boolean_look_it_replaced() {
    // "to the bit" means to the PANEL's bit: a lerp that lands on its endpoint still carries a
    // ~3e-8 float residue, and the framebuffer is 8 bits per channel.
    let is = |got: [f32; 4], want: [f32; 4], what: &str| {
        for i in 0..4 {
            assert!(
                (got[i] - want[i]).abs() < 0.5 / 255.0,
                "{what}: channel {i} is {} not {} — over half a display code out",
                got[i],
                want[i]
            );
        }
    };
    is(
        TabPill::mixed_ink(0.0, 0.0),
        theme::TEXT_READING,
        "plain segment on glass",
    );
    assert_ne!(
        theme::TEXT_READING,
        theme::TEXT_TERTIARY,
        "the fixture is meaningless if the two idle inks have converged"
    );
    is(
        TabPill::mixed_ink(0.0, 1.0),
        theme::TEXT_PRIMARY,
        "selected, focus elsewhere",
    );
    is(
        TabPill::mixed_ink(1.0, 0.0),
        crate::ACCENT_INK,
        "focused",
    );
    is(
        TabPill::mixed_ink(1.0, 1.0),
        crate::ACCENT_INK,
        "focus outranks selection",
    );
    // out-of-range mixes clamp rather than extrapolating past the tokens
    is(
        TabPill::mixed_ink(-1.0, 4.0),
        theme::TEXT_PRIMARY,
        "an over-range selection clamps",
    );
    is(
        TabPill::mixed_ink(9.0, -9.0),
        crate::ACCENT_INK,
        "an over-range focus clamps",
    );
}

/// **The bottom stop may not run past the ceiling.** The solve sizes the scrim so the labels
/// clear their floor against the TOP stop, which is the lightest of the pair, and the bottom was
/// free to be `a + spread` however heavy `a` got. Past [`theme::TAB_TRACK_A_TOP`] there is
/// nothing left to see through, so a bar drawn there has stopped being glass at its lower edge —
/// which is the flat capsule this material replaced, reintroduced under every heavy bar. Found by
/// a judging panel doing the arithmetic, not by looking.
#[test]
fn the_pair_never_draws_past_the_weight_where_glass_stops_being_glass() {
    let _g = serial_for_motion();
    let mut prev = 0.0f32;
    for i in 0..=40 {
        // a neutral ground swept the whole way up, so the solve sweeps its whole range
        let v = i as f32 / 40.0;
        // A fresh `TrackDensity` per iteration, on the stack — `track_density` used to be a
        // process-wide static a test had to save and restore; it is a plain `&mut` argument
        // now (spec phase 12, PX-WIDGETS), so there is nothing left to leak into the next test.
        let mut density = TrackDensity::new();
        let (top, bot) = tab_glass_stops([v, v, v], &mut density);
        assert!(
            bot[3] <= theme::TAB_TRACK_A_TOP + 1e-6,
            "ground {v:.2}: bottom stop {:.3} is past the ceiling {:.3}",
            bot[3],
            theme::TAB_TRACK_A_TOP
        );
        assert!(bot[3] >= top[3] - 1e-6, "ground {v:.2}: the pair inverted");
        assert!(
            top[3] >= prev - 1e-6,
            "ground {v:.2}: the top stop went backwards"
        );
        prev = top[3];
    }
}

/// A dark ground keeps the authored pair to the bit — that is the case the tokens were drawn
/// for, and the taper must not disturb it.
#[test]
fn a_dark_ground_draws_the_authored_pair_exactly() {
    let _g = serial_for_motion();
    let mut density = TrackDensity::new();
    let (top, bot) = tab_glass_stops([0.0, 0.0, 0.0], &mut density);
    assert!(
        (top[3] - theme::TAB_GLASS_TOP[3]).abs() < 1e-6,
        "top {:.4}",
        top[3]
    );
    assert!(
        (bot[3] - theme::TAB_GLASS_BOT[3]).abs() < 1e-6,
        "bot {:.4}",
        bot[3]
    );
}

/// **The plated composite**, which is arithmetic and therefore has no business being graded by
/// eye. A season tab's ground is now TWO layers — the pill's own idle plate and the strip's
/// travelling selection capsule over it — where it used to be one flat token. Same-colour
/// source-over is `a + b − ab`, so the pair must land back on [`theme::TAB_PLATE_SELECTED`]; and
/// the plate must retire exactly as the OPAQUE focus capsule arrives, or a focused season would
/// wear a 0.08 white film its unplated twin in the top row does not.
#[test]
fn a_travelling_plate_lands_back_on_the_plate_it_replaced() {
    let idle = theme::TAB_PLATE_IDLE[3];
    let over = theme::TAB_PLATE_SELECTED_OVER[3];
    let composite = over + idle * (1.0 - over);
    assert!(
        (composite - theme::TAB_PLATE_SELECTED[3]).abs() < 1.0 / 255.0,
        "capsule .{over} over plate .{idle} composites to {composite}, not TAB_PLATE_SELECTED's {}",
        theme::TAB_PLATE_SELECTED[3]
    );
    // order-independence is what lets the pill keep painting its plate AFTER the strip drew the
    // capsule under it — the draw order this component actually uses
    assert!(
        ((idle + over * (1.0 - idle)) - composite).abs() < 1e-6,
        "same-colour source-over must be order-independent, or the draw order becomes load-bearing"
    );
    // all three plate tokens are the same white; only their weight differs
    for i in 0..3 {
        assert_eq!(
            theme::TAB_PLATE_SELECTED_OVER[i],
            theme::TAB_PLATE_IDLE[i],
            "channel {i}"
        );
        assert_eq!(
            theme::TAB_PLATE_SELECTED_OVER[i],
            theme::TAB_PLATE_SELECTED[i],
            "channel {i}"
        );
    }
    // …and the split survives the `Painter` cascade, which scales BOTH layers: two layers under
    // a cascade are not the same function as one, so the drift is bounded here rather than
    // assumed. (Nothing draws a tab strip at α<1 today; this is the guard for the day one does.)
    for &a in &[1.0f32, 0.75, 0.5, 0.25] {
        let one = a * theme::TAB_PLATE_SELECTED[3];
        let two = a * over + a * idle * (1.0 - a * over);
        assert!(
            (one - two).abs() < 1.0 / 255.0,
            "at cascade α={a} the two-layer plate is {two} against the old {one} — over a display code apart"
        );
    }
}

/// **The container has to be visible on a page that is not.** A black scrim can only subtract,
/// so before the lift existed the bar's face over an L\*0 page measured the page exactly and
/// the whole object was one pixel of rim. This is the invariant that says it never happens
/// again — stated as a face lighter than its ground, which is what a person sees, rather than
/// as the constant that currently produces it.
#[test]
fn the_glass_never_goes_darker_than_its_floor() {
    let floor = theme::TAB_GLASS_LIFT_FLOOR;
    for i in 0..=100 {
        let lum = i as f32 / 100.0;
        let ground = [lum, lum, lum];
        let a = track_alpha_for(ground);
        let g = track_lift(ground, a);
        let face = lum * (1.0 - a) + g * a;
        assert!(
            face >= floor.min(lum * (1.0 - a)).min(floor) - 1e-4,
            "over a page at {lum} the face is {face}, under the floor {floor}"
        );
    }
    // the case the floor exists for: a page that shows nothing at all
    let a = track_alpha_for([0.0, 0.0, 0.0]);
    let face = track_lift([0.0, 0.0, 0.0], a) * a;
    assert!(
        face >= floor - 1e-4,
        "over a page at zero the face is {face}, short of the floor {floor}"
    );
}

/// …and it costs a bright bar NOTHING. The lift is spent out of the density solve's slack, so
/// the moment the solve leaves its floor there is none: a heavy bar is the same black scrim it
/// was, to the bit. Without this the fix for the dark end quietly lightens the light end, which
/// is where the labels are hardest to hold.
#[test]
fn a_bright_ground_gets_no_lift_at_all() {
    for &lum in &[0.45f32, 0.6, 0.8, 1.0] {
        let ground = [lum, lum, lum];
        let a = track_alpha_for(ground);
        assert!(
            track_lift(ground, a) == 0.0,
            "ground {lum} solved to α={a} and still took a lift"
        );
    }
}

/// The clamp is the whole licence for the rule: the solve above sized the scrim against a BLACK
/// tint, and a lighter face spends the contrast it bought. Whatever the lift does, the idle
/// label still clears its floor.
#[test]
fn the_lift_never_spends_the_labels_contrast() {
    for i in 0..=40 {
        let lum = i as f32 / 40.0;
        let ground = [lum, lum, lum];
        let a = track_alpha_for(ground);
        let g = track_lift(ground, a);
        let face = [
            ground[0] * (1.0 - a) + g * a,
            ground[1] * (1.0 - a) + g * a,
            ground[2] * (1.0 - a) + g * a,
            1.0,
        ];
        let c = contrast(theme::TEXT_READING, face);
        assert!(
            c >= TRACK_INK_CONTRAST - 0.01,
            "ground {lum}: α={a} lift={g} leaves the idle label at {c:.2}:1"
        );
    }
}

/// **The floor above is the TRACK's, over the track's own ground — and the band's other surface
/// has no equivalent.** A PINNED EXPOSURE, not a bug assertion: [`BarMaterial`]'s note is where
/// the decision and the three candidate fixes live.
///
/// [`profile_chip_with`]'s capsule wears the face [`track_alpha_for`] solved against pixels ~800px
/// away, and the unfurled capsule's whole content is a name in [`theme::TEXT_PRIMARY`]. Where
/// the two grounds agree the shared face is exactly right, which is the arrangement's whole
/// argument; where they do not, the chip carries a promise nobody made about it. Nothing
/// darkens the top band before this — [`HERO_BASE_SCRIM_Y0`] is 367 and [`HERO_SCRIM_TOP`]
/// 162 — so both grounds are raw backdrop, and `gfx::sample_ground`'s census puts the MEDIAN
/// span at 26.8 L* across the track alone.
///
/// Written as a test so the number cannot drift silently in EITHER direction: tightening the
/// exposure, or closing it, fails here and sends the next reader to the note.
#[test]
fn the_bands_face_carries_no_promise_about_the_chips_own_ground() {
    // a neutral at a CIE lightness — the axis every measurement in this material is quoted in
    let grey_at = |l: f32| {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..40 {
            let m = 0.5 * (lo + hi);
            if lstar([m, m, m, 1.0]) < l {
                lo = m
            } else {
                hi = m
            }
        }
        0.5 * (lo + hi)
    };
    // the drawn face, exactly as the test above builds it
    let face_over = |g: f32, a: f32| {
        let v = g * (1.0 - a) + track_lift([g, g, g], a) * a;
        [v, v, v, 1.0]
    };
    let dark = grey_at(20.0);
    let a = track_alpha_for([dark, dark, dark]);
    assert_eq!(
        a,
        theme::TAB_GLASS_TOP[3],
        "a dark track rests the solve on its floor"
    );
    assert!(
        contrast(theme::TEXT_READING, face_over(dark, a)) >= TRACK_INK_CONTRAST,
        "the track's own ink is served — that is the contract this one is measured against"
    );
    // …and the same face, under the chip, over a bright corner of the same hero.
    let bright = grey_at(92.0);
    let chip = contrast(theme::TEXT_PRIMARY, face_over(bright, a));
    assert!(
        (1.7..2.1).contains(&chip),
        "the chip's name over L*92 art with the track solved dark reads {chip:.2}:1 — if this \
         moved, in either direction, re-read `BarMaterial`"
    );
    let was = {
        let v = bright * (1.0 - theme::TAB_TRACK_A_TOP);
        contrast(theme::TEXT_PRIMARY, [v, v, v, 1.0])
    };
    assert!(
        was > 9.0,
        "the flat capsule this replaced cleared every ground: {was:.2}:1"
    );
}

/// **The play indicator is a biconditional, and this grades every cell of it.**
///
/// The owner's rule, 2026-09-05: *"visual play indicators mean immediate playback, progress
/// bars mean viewing progress, and cards without a play indicator navigate to details."* Two of
/// those three clauses are about what is ABSENT, which is why an exhaustive table is the only
/// honest test — the defect this was written for was a cell nobody looked at (in-progress on a
/// shelf that plays), and it was invisible to every screen test because it is a missing draw
/// call rather than a wrong one.
#[test]
fn the_play_glyph_appears_exactly_where_the_press_plays() {
    use crate::icons::Icon;
    let g = |m, plays| still_glyph(m, plays).map(|(i, _)| i);

    // navigating shelves: never a play indicator, whatever the watch state
    assert_eq!(g(PosterMark::None, false), None);
    assert_eq!(g(PosterMark::InProgress, false), None);
    assert_eq!(
        g(PosterMark::Watched, false),
        Some(Icon::Check),
        "a tick is a statement about the past, not a promise about the press",
    );

    // shelves that play: the action takes the slot, IN PROGRESS INCLUDED
    assert_eq!(g(PosterMark::None, true), Some(Icon::Play));
    assert_eq!(
        g(PosterMark::InProgress, true),
        Some(Icon::Play),
        "the Continue Watching deck is almost entirely in-progress tiles — this is the cell \
         that made the whole deck announce nothing and then play",
    );
    assert_eq!(
        g(PosterMark::Watched, true),
        Some(Icon::Check),
        "the bounded exception: the tick is the only carrier of watched-ness, and the one \
         surface it is reachable on draws no navigating card beside it",
    );

    // the amber is the action's and nothing else's
    assert_eq!(still_glyph(PosterMark::None, true).map(|(_, c)| c), Some(theme::RESUME_FILL));
    assert_eq!(
        still_glyph(PosterMark::Watched, false).map(|(_, c)| c),
        Some(theme::TEXT_SECONDARY),
    );
}

// ---- ink by the capsule's drawn edge ---------------------------------------------------------

/// The pill height the ink tests draw capsules at.
const INK_H: f32 = 60.0;

/// Every strip ground, with the focus pop a header or a season strip draws at.
fn ink_grounds() -> [TabGround; 3] {
    [TabGround::Tracked { glass: false }, TabGround::Plated { pop: CTRL_FOCUS_SCALE }, TabGround::Bare { pop: CTRL_FOCUS_SCALE, focus: true }]
}

/// Each strip ground with the ink its labels wear: the tab bar's and season strip's, and the page
/// header's (a bare ground).
fn ink_cases() -> Vec<(TabGround, fn(f32, f32) -> [f32; 4])> {
    let mut cases: Vec<_> = ink_grounds().into_iter().map(|ground| (ground, TabPill::mixed_ink as fn(f32, f32) -> [f32; 4])).collect();
    cases.push((TabGround::Bare { pop: CTRL_FOCUS_SCALE, focus: true }, header_ink));
    cases
}

/// The tab bar's ink and the page header's: the same regions, two palettes (a header label rests
/// at white 65% and a tab's at `TEXT_READING`).
fn header_ink(focus: f32, selected: f32) -> [f32; 4] {
    theme::cross(theme::cross(theme::CONTROL_TEXT_REST, theme::CONTROL_IDLE_INK, selected), crate::ACCENT_INK, focus)
}

/// What the pixel `(x, y)` of a strip whose pills stand `INK_H` tall from y 0 stands on, as DRAWN:
/// like [`backing_at`] but each capsule is the ROUNDED rect `TabStrip::draw` paints (its rect grown
/// about its centre, corner radius half its height), so a pixel above or below the centre row that
/// is outside the arc is on the slate. A hard decision per pixel centre (`d <= 0`): the antialiased
/// rim pixel itself is a blend and is not graded.
fn backing_at_xy(strip: &TabStrip, ground: TabGround, x: f32, y: f32) -> [f32; 4] {
    let plate = match ground {
        TabGround::Tracked { .. } => theme::OVERLAY_FOCUS_PILL,
        TabGround::Plated { .. } => theme::TAB_PLATE_SELECTED_OVER,
        TabGround::Bare { .. } => theme::TAB_PLATE_SELECTED,
    };
    let covers = |c: &Capsule, grow: Grow| {
        let (cx, cw) = c.span();
        let r = grow.of(Rect::new(cx, 0.0, cw, INK_H));
        let rad = (r.h * 0.5).min(r.w * 0.5);
        let q = ((x - (r.x + r.w * 0.5)).abs() - (r.w * 0.5 - rad), (y - (r.y + r.h * 0.5)).abs() - (r.h * 0.5 - rad));
        let d = q.0.max(0.0).hypot(q.1.max(0.0)) + q.0.max(q.1).min(0.0) - rad;
        (c.alpha() > 0.004 && cw > 0.5 && d <= 0.0).then(|| c.alpha())
    };
    let over = |dst: [f32; 4], src: [f32; 4], a: f32| theme::cross(dst, [src[0], src[1], src[2], 1.0], a);
    let mut c = INK_GROUND;
    if let Some(a) = covers(&strip.sel, Grow::Scale(1.0)) { c = over(c, plate, plate[3] * a); }
    if let Some(a) = covers(&strip.foc, Grow::focus(ground)) { c = over(c, crate::ACCENT, a); }
    c
}

/// The lowest contrast between the ink the pixels of a pill wear and what is under them, over the
/// pill's GLYPH BOX: the 34 rows a label's ascenders and descenders stand on (centre +-17) and every
/// pixel column from 12 px in from either end (labels and marks stand 18+ in), as `(ratio, x, dy)`:
/// the 2-D grade the column-only ones could not give.
fn worst_contrast_2d(strip: &TabStrip, ground: TabGround, pill: (f32, f32), ink_of: fn(f32, f32) -> [f32; 4]) -> (f32, f32, f32) {
    let rect = Rect::new(pill.0, 0.0, pill.1, INK_H);
    let mut worst = (f32::MAX, pill.0, 0.0);
    for column in (pill.0.floor() as i32)..((pill.0 + pill.1).ceil() as i32) {
        let x = column as f32 + 0.5;
        if x < pill.0 + 12.0 || x >= pill.0 + pill.1 - 12.0 { continue; }
        for k in -17..17 {
            let y = INK_H * 0.5 + k as f32 + 0.5;
            let (focus, selected) = strip.ink_at(rect, ground, x, y);
            let backing = backing_at_xy(strip, ground, x, y);
            let ink = ink_of(focus, selected);
            let ratio = contrast(theme::cross(backing, [ink[0], ink[1], ink[2], 1.0], ink[3]), backing);
            if ratio < worst.0 { worst = (ratio, x, y - INK_H * 0.5); }
        }
    }
    worst
}

/// **A capsule's ends are ROUND, and the ink follows them**: a glyph row above or below the centre
/// row is inside the capsule only where the arc is, 2-4 px further in than the capsule's x-extreme
/// (the rows an ascender or a descender stand on). The ink the pixel wears is decided by the DRAWN
/// shape per pixel, so on every glyph row, across a travel and across the fade-in, every pixel of
/// every pill keeps 3:1 against what is under it, on every strip ground.
#[test]
fn a_label_a_capsule_is_crossing_stays_readable_on_every_glyph_row_not_only_the_centre_one() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
    for (ground, ink_of) in ink_cases() {
        let mut s = TabStrip::new();
        for _ in 0..90 { s.update(2, 4, span, SelMark::Travels, DT); }
        let mut worst = (f32::MAX, 0, 0.0, 0.0);
        for frame in 0..90 {
            s.update(2, 5, span, SelMark::Travels, DT);
            for pill in [4, 5] {
                let (ratio, x, dy) = worst_contrast_2d(&s, ground, span_of(&w, pill), ink_of);
                if ratio < worst.0 { worst = (ratio, frame, x, dy); }
            }
        }
        assert!(worst.0 >= 3.0, "travel: worst {:.2}:1 at frame {} x {} row {:+} from the centre (3:1 wanted)", worst.0, worst.1, worst.2, worst.3);
        for (from, to) in [(-1, 3), (3, -1)] {
            let mut s = TabStrip::new();
            for _ in 0..90 { s.update(3, from, span, SelMark::Lands, DT); }
            let mut worst = (f32::MAX, 0, 0.0, 0.0);
            for frame in 0..60 {
                s.update(3, to, span, SelMark::Lands, DT);
                let (ratio, x, dy) = worst_contrast_2d(&s, ground, span_of(&w, 3), ink_of);
                if ratio < worst.0 { worst = (ratio, frame, x, dy); }
            }
            assert!(worst.0 >= 3.0, "{from} -> {to}: worst {:.2}:1 at frame {} x {} row {:+} from the centre (3:1 wanted)", worst.0, worst.1, worst.2, worst.3);
        }
    }
}

/// **The ink regions follow the capsules' DRAWN shapes**: a pill no capsule is across is one plain
/// region; a capsule across it splits it along the capsule's popped rounded shape (the capsule grows
/// about its centre, `Rect::scaled`, and its end is an arc), so on a glyph row above the centre the
/// ink flips further in than on the centre row; a lifted control (`focus: false`) has no focus
/// capsule to cut at or to ink for.
#[test]
fn the_ink_regions_follow_the_capsules_drawn_shapes() {
    let _g = serial_for_motion();
    let w = widths_for(12);
    let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
    let rect = |i: usize| Rect::new(span_of(&w, i).0, 0.0, w[i], INK_H);
    let ground = TabGround::Bare { pop: CTRL_FOCUS_SCALE, focus: true };
    let mut s = TabStrip::new();
    for _ in 0..90 { s.update(-1, 4, span, SelMark::Travels, DT); }
    let at_rest = s.pill_ink(rect(4), ground);
    assert_eq!(at_rest.regions().len(), 1, "a capsule at rest on a pill is over all of its glyphs");
    assert!(at_rest.regions()[0].focus > 0.5 && at_rest.clip_of(&at_rest.regions()[0], Painter::root()).is_none(), "painted once, plain, in the capsule's ink");
    let far = s.pill_ink(rect(9), ground);
    assert_eq!(far.regions(), [InkRegion { focus: 0.0, selected: 0.0, sel: None, foc: None }], "a pill the capsule is not over is plain resting ink");
    let mut crossed = false;
    for _ in 0..40 {
        s.update(-1, 5, span, SelMark::Travels, DT);
        let pill = rect(4);
        let ink = s.pill_ink(pill, ground);
        if ink.regions().len() < 2 { continue; }
        crossed = true;
        let (cx, cw) = s.foc.span();
        let drawn = Grow::focus(ground).of(Rect::new(cx, 0.0, cw, INK_H));
        // The capsule is leaving to the right: its left end is an arc across the pill's glyph rows.
        let (mid, off) = (INK_H * 0.5 + 0.5, INK_H * 0.5 - 15.5);
        let first_inside = |y: f32| (0..400).map(|k| drawn.x + k as f32 * 0.25).find(|&x| ink.at(x, y).0 > 0.5);
        let (centre, high) = (first_inside(mid), first_inside(off));
        if let (Some(centre), Some(high)) = (centre, high) {
            assert!(high > centre + 1.5, "on a row 15 px above the centre the ink flips further in ({high} vs {centre}): the arc, not the extreme");
        }
        assert!(ink.regions().iter().any(|r| r.focus > 0.5 && r.foc == Some(Keep::Inside)) && ink.regions().iter().any(|r| r.focus == 0.0 && r.foc == Some(Keep::Outside)),
            "the focus ink is inside the capsule, the resting ink outside it: {:?}", ink.regions());
    }
    assert!(crossed, "the capsule left pill 4 across the shape of it");
    let lifted = TabGround::Bare { pop: CTRL_FOCUS_SCALE, focus: false };
    for pill in [4, 5] {
        let ink = s.pill_ink(rect(pill), lifted);
        assert!(ink.regions().iter().all(|r| r.focus == 0.0), "a lifted control wears no focus ink");
    }
}

// ---- one label, one glyph geometry ------------------------------------------------------------

/// The top bar's pills as the device measures them (a bold label plus `TAB_PILL_PAD` either side,
/// the icon faces square): Home, Movies, TV Shows, the Watchlist mark and Search.
fn nav_widths() -> [f32; 5] {
    [120.0 + 36.0, 160.0 + 36.0, 150.0 + 36.0, TAB_ICON_PILL_W, TAB_ICON_PILL_W]
}

/// How the label of a pill is drawn in one ink region: the builder chain `draw_tab_row` and
/// `draw_strip` run per region of [`paint_inked`], graded through the pill's own `face()`.
fn region_label(ink: &PillInk, region: &InkRegion) -> TabPill {
    TabPill::new(std::ptr::null(), theme::size::BODY, Rect::new(0.0, 0.0, 100.0, INK_H)).inked(ink, region.focus, region.selected)
}

/// **A label's glyphs are ONE geometry whichever ink a region paints them in.** The regions of a
/// pill are cut along a capsule's drawn edge and each paints the WHOLE label, clipped to its side,
/// so the clips tile the glyph only if every region rasterizes the same glyph. A weight taken from
/// each region's own mixes (regular outside the capsule, bold inside) painted the outside half of
/// "TV Shows" in the regular face and the inside half in the bold one, whose strokes and advance
/// differ: the "s" the capsule's rounded end was crossing was painted by neither (frames 9-ink,
/// n=128-130: white on the white capsule). The header row never drew bold, which is why it only
/// ever lost a rim. Graded here over every hop of the real bar, both ways, on the strip's own
/// regions: each region draws the label at the same weight.
#[test]
fn every_ink_region_of_a_label_is_drawn_at_the_same_weight() {
    let _g = serial_for_motion();
    let w = nav_widths();
    let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
    let ground = TabGround::Tracked { glass: true };
    let mut crossed = 0;
    for (sel, from, to) in [(1, 1, 2), (1, 2, 3), (1, 3, 2), (1, 2, 1), (1, 3, 4), (1, 4, 0)] {
        let mut s = TabStrip::new();
        for _ in 0..90 { s.update(sel, from, span, SelMark::Travels, DT); }
        for frame in 0..80 {
            s.update(sel, to, span, SelMark::Travels, DT);
            for pill in 0..w.len() {
                let ink = s.pill_ink(Rect::new(span_of(&w, pill).0, 0.0, w[pill], INK_H), ground);
                if ink.regions().len() < 2 { continue; }
                crossed += 1;
                let weights: Vec<f32> = ink.regions().iter().map(|r| region_label(&ink, r).face().2).collect();
                assert!(
                    weights.iter().all(|&x| (x - weights[0]).abs() < 1e-4),
                    "{from} -> {to} frame {frame} pill {pill}: the regions draw the label at weights {weights:?}, so the capsule's arc crosses two different glyph sets"
                );
            }
        }
    }
    assert!(crossed > 20, "the hops cross labels ({crossed} region splits seen)");
}

/// **No glyph pixel of the top bar is ever lost on the capsule**, over every hop of the real bar's
/// pills (labels and the square icon faces) in both directions: the ink every pixel of every pill
/// wears keeps 3:1 against the composited fill under it, decided per pixel centre from the drawn
/// rounded shape (`backing_at_xy`; the antialiased rim pixel itself is a blend and is not graded,
/// which is the "within 1 px of the drawn edge" skip). The pill a capsule's rounded end is crossing
/// is the case: the label there is two inks cut along the arc and must stay one glyph set
/// ([`every_ink_region_of_a_label_is_drawn_at_the_same_weight`]).
#[test]
fn the_top_bar_keeps_three_to_one_on_every_glyph_pixel_of_every_hop() {
    let _g = serial_for_motion();
    let w = nav_widths();
    let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
    for ground in [TabGround::Tracked { glass: true }, TabGround::Tracked { glass: false }] {
        for (sel, from, to) in [(1, 1, 2), (1, 2, 3), (1, 3, 4), (1, 4, 3), (1, 3, 2), (1, 2, 0), (1, 0, 3)] {
            let mut s = TabStrip::new();
            for _ in 0..90 { s.update(sel, from, span, SelMark::Travels, DT); }
            for frame in 0..90 {
                s.update(sel, to, span, SelMark::Travels, DT);
                for pill in 0..w.len() {
                    let (ratio, x, dy) = worst_contrast_2d(&s, ground, span_of(&w, pill), TabPill::mixed_ink);
                    assert!(ratio >= 3.0, "{from} -> {to} frame {frame} pill {pill}: {ratio:.2}:1 at x {x} row {dy:+} (3:1 wanted)");
                }
            }
        }
    }
}

/// **What the top bar DRAWS in a label, not what the widget says it will.** The test above reads
/// `region_label(..).face().2`, the widget's own weight number, which is why it passed while the bar
/// crossfaded a regular and a bold raster at alpha `1 - w` and `w`: where both faces cover a pixel the
/// ink reaches `1 - w(1 - w)` of full and where one covers it is `w` or `1 - w` alone, so 111
/// glyph-body pixels sat under 3:1 in 13 frames of the Home -> Movies -> TV Shows hops (worst 2.02:1,
/// design review judgment 4: "never an ink lerp"). This one records what [`StripRender::draw`] hands
/// a painter, frame by frame through the same hops: every label run carries EXACTLY the ink one of
/// its pill's regions names (no alpha cascaded onto it, so none of it is a lerp of two rasters), and
/// all the runs of a label in a frame are in ONE weight, which STEPS (once per rise or fall of the
/// capsule over the pill) rather than easing.
#[test]
fn the_top_bar_draws_every_label_at_full_ink_in_one_weight_per_frame() {
    let _g = serial_for_motion();
    // A recording painter paints no pixels, and the bar's draw calls `gfx::sample_ground`, which
    // reads a band image and so needs a walk; with none it answers the last reading and copies
    // nothing (`gfx`'s `a_bar_with_no_band_image_keeps_its_last_reading_and_copies_nothing`).
    let faces = [
        TabFace::Label("Home".into()),
        TabFace::Label("Movies".into()),
        TabFace::Label("TV Shows".into()),
        TabFace::Icon(crate::icons::Icon::Bookmark),
        TabFace::Icon(crate::icons::Icon::Search),
    ];
    let labels = TabLabels { generation: 1, faces: &faces };
    let words = ["Home", "Movies", "TV Shows"];
    let mut mid_weight_frames = 0;
    let mut stepped = 0;
    for (sel, from, to) in [(1, 1, 2), (1, 2, 3), (1, 3, 2), (1, 2, 1), (1, 1, 0), (1, 0, 2)] {
        let mut strip = StripRender::new();
        let mut band = TabBand::new();
        for _ in 0..90 {
            strip.update(labels, sel, TopFocus::Pill(from), DT);
        }
        // Per label: the weight drawn on the previous frame, and how often it changed over the hop.
        let mut last: [Option<bool>; 3] = [None; 3];
        let mut flips = [0usize; 3];
        for frame in 0..80 {
            strip.update(labels, sel, TopFocus::Pill(to), DT);
            let runs = crate::draw_census::capture_runs(|| strip.draw(labels, Painter::recording(), &mut band));
            let widths = with_tab_metrics_for(labels, |_, _, w| w.to_vec());
            let glass_on = tab_glass_on();
            for (i, word) in words.iter().enumerate() {
                let mine: Vec<_> = runs.iter().filter(|r| r.text == *word).collect();
                assert!(!mine.is_empty(), "{from} -> {to} frame {frame}: \"{word}\" is not drawn");
                let ink = strip.top_strip.pill_ink(Rect::new(tab_pill_x(&widths, i), TOP_BAR_Y, widths[i], TAB_PILL_H), TabGround::Tracked { glass: glass_on });
                if ink.weight() > 0.02 && ink.weight() < 0.98 {
                    mid_weight_frames += 1;
                }
                let inks: Vec<[f32; 4]> = ink.regions().iter().map(|r| TabPill::mixed_ink(r.focus, r.selected)).collect();
                for run in &mine {
                    assert!(
                        inks.contains(&run.ink),
                        "{from} -> {to} frame {frame}: \"{word}\" is drawn in {:?}, which is no region's ink ({inks:?}): an alpha was cascaded onto the label",
                        run.ink
                    );
                }
                let bold = mine[0].bold;
                assert!(
                    mine.iter().all(|r| r.bold == bold),
                    "{from} -> {to} frame {frame}: \"{word}\" is drawn in both weights at once ({:?})",
                    mine.iter().map(|r| r.bold).collect::<Vec<_>>()
                );
                assert_eq!(bold, ink.weight() >= 0.5, "{from} -> {to} frame {frame}: \"{word}\" is bold iff its weight {} is past one half", ink.weight());
                if last[i].is_some_and(|b| b != bold) {
                    flips[i] += 1;
                }
                last[i] = Some(bold);
            }
        }
        for (i, f) in flips.iter().enumerate() {
            assert!(*f <= 2, "{from} -> {to}: \"{}\" changed weight {f} times over one hop", words[i]);
            stepped += f;
        }
    }
    assert!(mid_weight_frames > 20, "the hops cross labels mid-weight ({mid_weight_frames} frames)");
    assert!(stepped > 4, "the weight steps as a capsule crosses a label ({stepped} steps)");
}

/// **The painted gaps (lane hs-ui2).** Reads the label runs a recording painter was handed and
/// checks the AIR between neighbouring pill frames as drawn: [`TAB_GAP`] in the top bar, the row's
/// `gap` in a strip, a skipped label leaving no double gap. A label is centred in its frame, so a
/// frame is its run's measured width plus the pad each side.
#[test]
fn painted_pill_frames_stand_one_gap_apart_in_the_top_bar_and_in_a_strip() {
    let _g = serial_for_motion();
    // a label's frame (x, w) from the first run that says it
    fn frame(runs: &[crate::draw_census::Run], word: &str, pad: f32) -> (f32, f32) {
        let run = runs.iter().find(|r| r.text == word).unwrap_or_else(|| panic!("\"{word}\" is not drawn"));
        assert_eq!(run.align, 1, "\"{word}\" is centred in its frame");
        let w = run.at.w + 2.0 * pad;
        (run.at.x - w * 0.5, w)
    }
    // ---- the top bar ----
    let faces = [
        TabFace::Label("Home".into()),
        TabFace::Label("Movies".into()),
        TabFace::Label("TV Shows".into()),
        TabFace::Icon(crate::icons::Icon::Search),
    ];
    let labels = TabLabels { generation: 1, faces: &faces };
    let mut strip = StripRender::new();
    let mut band = TabBand::new();
    for _ in 0..90 {
        strip.update(labels, 0, TopFocus::Pill(0), DT);
    }
    let runs = crate::draw_census::capture_runs(|| strip.draw(labels, Painter::recording(), &mut band));
    let bar: Vec<(f32, f32)> = ["Home", "Movies", "TV Shows"].iter().map(|w| frame(&runs, w, TAB_PILL_PAD)).collect();
    for pair in bar.windows(2) {
        let air = pair[1].0 - (pair[0].0 + pair[0].1);
        assert!((air - TAB_GAP).abs() <= 0.5, "top-bar pill frames stand {air} apart, not {TAB_GAP}");
    }
    // the Search icon pill's frame is the geometry's (an icon has no run to read): TAB_ICON_PILL_W
    // wide, and one TAB_GAP after the last label's painted frame
    let search = with_tab_metrics_for(labels, |_, _, widths| tab_geometry(widths, 0.0).pill(3));
    assert_eq!(search.w, TAB_ICON_PILL_W, "the Search pill is the icon pill's width");
    let air = search.x - (bar[2].0 + bar[2].1);
    assert!((air - TAB_GAP).abs() <= 0.5, "the Search pill's frame stands {air} after TV Shows, not {TAB_GAP}");
    // ---- a strip, with a label that cannot be drawn between two that can ----
    let measure = crate::fixture::FixtureMeasure;
    let gap = 37.0;
    let lays = strip_layout_measured(
        ["Alpha".into(), "bad\0label".into(), "Beta".into(), "Gamma".into()].into_iter(),
        96.0, theme::size::BODY, gap, &measure,
    );
    assert_eq!(lays.len(), 3, "the unusable label is skipped");
    assert_eq!(lays.iter().map(|l| l.i).collect::<Vec<_>>(), [0, 2, 3], "indices survive the skip");
    let tabs = TabStrip::new();
    let runs = crate::draw_census::capture_runs(|| draw_strip(Painter::recording(), &tabs, &lays, 10.0, 60.0, 0.0, TabGround::Plated { pop: 0.0 }));
    let frames: Vec<(f32, f32)> = ["Alpha", "Beta", "Gamma"].iter().map(|w| frame(&runs, w, STRIP_PAD)).collect();
    assert!((frames[0].0 - (96.0 - STRIP_PAD)).abs() <= 0.5, "the first frame starts one pad before x0");
    for pair in frames.windows(2) {
        let air = pair[1].0 - (pair[0].0 + pair[0].1);
        assert!((air - gap).abs() <= 0.5, "strip pill frames stand {air} apart, not {gap}");
    }
}

/// The focus cast rides the capsule's own fade (`control_cast_at`'s doc): on a plated strip the
/// shadow used to be drawn at full strength while the fill faded out and then vanish in the one
/// frame the fill dropped under the draw cutoff — and appear whole on the way back.
#[test]
fn a_plated_focus_capsules_cast_fades_with_it_both_ways() {
    let _g = serial_for_motion();
    let measure = crate::fixture::FixtureMeasure;
    let lays = strip_layout_measured(
        ["Alpha".into(), "Beta".into(), "Gamma".into()].into_iter(),
        96.0, theme::size::BODY, 37.0, &measure,
    );
    let (top, h) = (10.0, 60.0);
    let span = |i: usize| lays.iter().find(|l| l.i == i).map(|l| {
        let r = strip_pill_rect(l, top, h);
        (r.x, r.w)
    });
    let full: f32 = theme::CONTROL_CAST_FOCUS.iter().map(|(_, _, a)| a).sum();
    let mut strip = TabStrip::new();
    // (cast strength as a share of the full lift, the focus fill's effective alpha, plates drawn)
    let frame = |strip: &mut TabStrip, focused: c_int| {
        strip.update(0, focused, span, SelMark::Lands, DT);
        let inks = crate::draw_census::capture_inks(|| {
            draw_strip(Painter::recording(), strip, &lays, top, h, 0.0, TabGround::Plated { pop: 1.0 });
        });
        // The cast must be the OUTSIDE-only shadow (tag 5): the interior-painting one (tag 4) shows
        // through a translucent capsule as it fades.
        assert!(inks.iter().all(|(t, _, _)| *t != 4), "a capsule's cast is drawn outside its outline only");
        let sum = |tag: u64| inks.iter().filter(|(t, _, _)| *t == tag).map(|(_, _, a)| *a).sum::<f32>();
        let fill = inks.iter().filter(|(t, _, _)| *t == 8).map(|(_, _, a)| *a).fold(0.0, f32::max);
        (sum(5) / full, fill, inks.iter().any(|(t, _, _)| *t == 2))
    };
    let mut trace = Vec::new();
    for _ in 0..60 { trace.push(frame(&mut strip, 0)); }
    let held = *trace.last().unwrap();
    assert!((held.0 - 1.0).abs() < 1e-3, "a focused plated pill wears the full cast ({})", held.0);
    for _ in 0..60 { trace.push(frame(&mut strip, -1)); }
    let out = *trace.last().unwrap();
    for _ in 0..60 { trace.push(frame(&mut strip, 0)); }
    let back = *trace.last().unwrap();

    assert_eq!(out.0, 0.0, "after focus leaves the strip there is no cast");
    assert_eq!(out.1, 0.0, "…and no focus capsule");
    assert!(out.2, "…only the selection plate remains");
    assert!((back.0 - 1.0).abs() < 1e-3, "after focus returns the cast is whole again ({})", back.0);
    assert!(back.1 > 0.99, "…and so is the fill ({})", back.1);
    // the cast leads the fill out: <=10% of full once the capsule is at half alpha, whole at rest
    for (n, t) in trace.iter().enumerate() {
        if t.1 <= 0.5 { assert!(t.0 <= 0.1, "frame {n}: the cast ({}) outlives the fill ({})", t.0, t.1); }
        if t.1 > 0.999 { assert!((t.0 - 1.0).abs() < 1e-3, "frame {n}: a settled capsule wears the whole cast ({})", t.0); }
    }
    // the cast is a smoothstep of the fill (slope up to 3x over [0.5, 1]): it may move 3x as fast per frame, never a step
    for (n, pair) in trace.windows(2).enumerate() {
        let (dc, df) = ((pair[1].0 - pair[0].0).abs(), (pair[1].1 - pair[0].1).abs());
        assert!(dc <= 0.35, "frame {}: the cast jumped {dc} in one frame ({} -> {})", n + 1, pair[0].0, pair[1].0);
        assert!(df <= 0.12, "frame {}: the fill jumped {df} in one frame ({} -> {})", n + 1, pair[0].1, pair[1].1);
    }
}
