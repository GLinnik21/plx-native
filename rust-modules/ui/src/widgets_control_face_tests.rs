//! Control face treatment: ambient-keyed vs. unkeyed grounds, focus pop (`CtlPop`) motion, the danger face, and the blended pill/capsule outline + focus cast shadow.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// A keyed control carries the HERO's colour without borrowing its darkness. Blue artwork and
/// amber artwork must therefore produce different hues at the SAME authored face lightness,
/// while the body stays the same hue one lightness step below the lit top. This is the part of
/// `components/core/control-focus.card.html` the old static `ACCENT` face could not express.
#[test]
fn an_ambient_key_tints_both_levels_of_the_focused_face() {
    let blue = ControlPalette::ambient([
        0x0a as f32 / 255.0,
        0x63 as f32 / 255.0,
        0xb4 as f32 / 255.0,
    ]);
    let amber = ControlPalette::ambient([
        0xb8 as f32 / 255.0,
        0x72 as f32 / 255.0,
        0x1c as f32 / 255.0,
    ]);
    let (blue_top, blue_body) = blue.focus_face();
    let (amber_top, amber_body) = amber.focus_face();

    let bt = srgb_to_oklab(blue_top);
    let bb = srgb_to_oklab(blue_body);
    let at = srgb_to_oklab(amber_top);
    let ab = srgb_to_oklab(amber_body);
    assert!((bt[0] - theme::CONTROL_FOCUS_FACE_L).abs() < 0.002);
    assert!((at[0] - theme::CONTROL_FOCUS_FACE_L).abs() < 0.002);
    assert!((bt[0] - bb[0] - theme::CONTROL_FOCUS_BODY_STEP).abs() < 0.003);
    assert!((at[0] - ab[0] - theme::CONTROL_FOCUS_BODY_STEP).abs() < 0.003);
    assert!(
        bt[2] < 0.0 && at[2] > 0.0,
        "blue and amber keys must remain opposite hues"
    );
}

/// Video is the explicit exception: its pixels live on another plane, so even a supplied key
/// is ignored. The focused face stays flat ACCENT and the idle face stays the HUD's light film.
#[test]
fn an_unkeyed_control_never_samples_the_ambient_palette() {
    let palette = ControlPalette::ambient([0.04, 0.39, 0.71]);
    let focus = ControlStyle::Accent.face(true, ControlGround::Unkeyed, palette);
    let idle = ControlStyle::Accent.face(false, ControlGround::Unkeyed, palette);
    assert_eq!(
        (focus.top, focus.body, focus.ink),
        (theme::ACCENT, theme::ACCENT, theme::ACCENT_INK)
    );
    assert_eq!(
        (idle.top, idle.body),
        (
            theme::CONTROL_IDLE_FILL_UNKEYED,
            theme::CONTROL_IDLE_FILL_UNKEYED
        )
    );
}

/// **A control row's pop animates BOTH ways**, which is the whole reason it is an array of
/// springs and not one global scalar. Walking focus from control 0 to control 1 must leave 0
/// still shrinking while 1 grows — a single spring could only snap the outgoing face to rest,
/// which on a 60px disc is a visible 4px jump at the moment the eye is already on that control.
#[test]
fn a_control_leaving_focus_shrinks_while_its_neighbour_grows() {
    let mut pop: CtlPop<2> = CtlPop::new();
    for _ in 0..40 {
        pop.step(Some(0), 1.0 / 60.0);
    }
    let settled = pop.scale(0);
    assert!(
        (settled - CTRL_FOCUS_SCALE).abs() < 0.005,
        "a held focus settles ON the pop, not near it: {settled}"
    );
    assert_eq!(pop.scale(1), 1.0, "and its neighbour is at rest");

    // focus moves — one frame later BOTH are in flight, neither at an endpoint
    pop.step(Some(1), 1.0 / 60.0);
    let (leaving, arriving) = (pop.scale(0), pop.scale(1));
    assert!(
        leaving < settled && leaving > 1.0,
        "the leaving face is still shrinking: {leaving}"
    );
    assert!(
        arriving > 1.0,
        "…while the arriving one has already started: {arriving}"
    );
}

/// **The SEASON STRIP's pop, driven exactly as `detail` drives it**: one control, open while
/// section 1 holds focus. The half that matters is the PRESS — the design system says a plated
/// pill takes "a Button's press, dip on the way down, ring on release"
/// (`components/chrome/TabStrip.jsx`), and this row had neither the pop nor the dip until
/// 2026-08-22 while `CTRL_FOCUS_SCALE`'s own doc claimed it had the pop.
///
/// It also pins the other half, which is what stops the fix from over-reaching: once focus has
/// left the row for the episodes below, a press belongs to whatever is focused THERE and must
/// not reach this capsule. `CtlPop` already owns that rule; the test is that the season strip is
/// wired through it rather than multiplying `press::scale()` in by hand at the draw.
///
/// Takes `testlock::serial()` for the pop's own statics; the press is the test's own `Press`.
#[test]
fn the_season_strips_pop_takes_the_press_dip_only_while_the_row_holds_focus() {
    let mut p = crate::press::Press::new();
    let _g = plx_base::testlock::serial();
    let mut pop: CtlPop<1> = CtlPop::new();
    for _ in 0..60 {
        pop.step(Some(0), 1.0 / 60.0);
    }
    let focused = pop.scale(0);
    assert!(
        (focused - CTRL_FOCUS_SCALE).abs() < 0.005,
        "a focused season pill settles on the control pop: {focused}"
    );

    // OK goes down on the tab. The strip keeps a CARD's press — a hold here marks the whole
    // season watched — so this is `begin`, not `begin_ctl`; the DIP is identical either way.
    p.begin(1000);
    let mut now = 1000u32;
    for _ in 0..8 {
        now = now.wrapping_add(16);
        p.tick(now, 1.0 / 60.0);
        pop.step(Some(0), 1.0 / 60.0);
    }
    let pressed = pop.scale(0);
    assert!(
        pressed < focused - 0.02,
        "the press must dip the focused pill inward: {pressed} vs {focused}"
    );

    // …and the same press, with focus moved off the row, leaves this capsule alone.
    let mut away: CtlPop<1> = CtlPop::new();
    for _ in 0..60 {
        away.step(None, 1.0 / 60.0);
    }
    assert_eq!(
        away.scale(0),
        1.0,
        "a row without focus is at rest, press or no press"
    );

    p.cancel();
    for _ in 0..200 {
        now = now.wrapping_add(16);
        p.tick(now, 1.0 / 60.0);
    }
    assert!(
        !p.is_active(),
        "leave the global at rest for the next test"
    );
}

/// **The focus pop does NOT bounce**, and that is the whole assertion: it grows to its target and
/// stops, exactly as a poster's does. The design system says so twice — `tokens/motion.css`'s
/// opening line ("focus ARRIVING is a calm grow, the CLICK is what rings") and the `--ease-bounce`
/// token's own "never a focus pop" — and `Button.jsx` reaches for `--ease-bounce` only while
/// `releasing`.
///
/// It asserted the OPPOSITE until 2026-08-22, when the owner reported the bounce on screen. A
/// test can pin a mistake as firmly as it pins a rule, and this one did: the pop rang on arrival,
/// the doc beside it said the design system asked for that, and the test agreed with the doc.
/// None of the three had been checked against the design system itself.
#[test]
fn the_pop_grows_to_focus_without_ringing() {
    let mut pop: CtlPop<1> = CtlPop::new();
    let mut peak = 1.0f32;
    for _ in 0..40 {
        pop.step(Some(0), 1.0 / 60.0);
        peak = peak.max(pop.scale(0));
    }
    assert!(
        peak <= CTRL_FOCUS_SCALE + 0.0005,
        "a focus arrival must not pass its target — that is the click's alone: peaked at {peak}"
    );
    assert!(
        (peak - CTRL_FOCUS_SCALE).abs() < 0.005,
        "…and it does REACH it — a spring that never arrives is not calm, it is slow: {peak}"
    );
}

/// Nothing focused closes every pop, and `reset` gets there with no motion at all — the two
/// halves a page teardown needs (`detail::reset_view_state`), so a re-mounted page never opens
/// with a control standing proud of its row.
#[test]
fn a_row_with_no_focus_settles_flat() {
    let mut pop: CtlPop<3> = CtlPop::new();
    for _ in 0..40 {
        pop.step(Some(2), 1.0 / 60.0);
    }
    for _ in 0..60 {
        pop.step(None, 1.0 / 60.0);
    }
    for i in 0..3 {
        assert!(
            (pop.scale(i) - 1.0).abs() < 0.005,
            "control {i} eased back to rest"
        );
    }
    pop.step(Some(1), 1.0 / 60.0);
    pop.reset();
    for i in 0..3 {
        assert_eq!(pop.scale(i), 1.0, "…and reset is exact, not nearly");
    }
}

/// An out-of-range index answers 1.0 rather than panicking. `detail::hero_ctls` builds a row
/// whose LENGTH depends on the item (a resume point comes and goes, a second source comes and
/// goes), so a focus index can outrun the array for the frame between a set changing and
/// `hero_col` re-seating the focus on it.
#[test]
fn an_index_past_the_row_is_a_resting_control() {
    let mut pop: CtlPop<2> = CtlPop::new();
    pop.step(Some(7), 1.0 / 60.0);
    assert_eq!(pop.scale(7), 1.0);
    assert_eq!(
        pop.scale(0),
        1.0,
        "and an out-of-range FOCUS pops nothing in range either"
    );
}

/// The unfurl's three formulas are ONE formula asked three ways, and each way is load-bearing
/// somewhere the others are not: [`CircleButton::cap_w`] places the capsule, `cap_label_w`
/// recovers what it was sized for so the label's fade can be geometric rather than guessed, and
/// [`CircleButton::label_budget`] is what a row with a hard right edge asks before allowing any
/// of it (`detail::watch_cap_at`). They agree or the control paints a word past its own frame.
#[test]
fn the_unfurl_geometry_round_trips() {
    let d = StatusOverlay::CTRL_H;
    for label_w in [40.0f32, 120.0, 233.0, 267.0, 400.0] {
        assert_eq!(
            CircleButton::cap_w(d, 0.0, label_w),
            d,
            "shut is the bare disc"
        );
        let open = CircleButton::cap_w(d, 1.0, label_w);
        assert!(
            open > d + label_w,
            "an open capsule holds its label and then some"
        );

        for e in [0.05f32, 0.25, 0.5, 0.9, 1.0] {
            let w = CircleButton::cap_w(d, e, label_w);
            assert!(
                w >= d && w <= open,
                "the capsule stays between its two ends at e={e}"
            );
            let back = CircleButton::cap_label_w(d, e, w).expect("open enough to invert");
            assert!(
                (back - label_w).abs() < 0.01,
                "e={e}: recovered {back}, measured {label_w}"
            );
        }
        // …and the budget is the same equation solved for the label: spending exactly it must
        // land the open capsule exactly `room` past the bare disc.
        let room = open - d;
        let budget = CircleButton::label_budget(d, room);
        assert!(
            (budget - label_w).abs() < 0.01,
            "budget {budget} for a {label_w} label"
        );
    }
    // a shut (or nearly shut) capsule carries no recoverable label — the range where the ramp
    // has nothing to fade in anyway
    assert_eq!(CircleButton::cap_label_w(d, 0.0, d), None);
    // "no label" collapses whatever the unfurl says, so a caller cannot animate a bare disc wide
    for e in [0.0f32, 0.5, 1.0] {
        assert_eq!(CircleButton::cap_w(d, e, 0.0), d);
        assert_eq!(CircleButton::cap_w(d, e, -5.0), d);
    }
}

/// The **destructive** face, both states, as the one sentence it is meant to say: *the colour
/// belongs to the ACTION, the fill belongs to FOCUS.*
///
/// Both halves are graded, because each fails invisibly on its own. An idle plate that is not
/// tinted (a `Danger` arm that fell through to `Accent`'s neutral) still lights up correctly on
/// focus, so a device capture of the focused pill looks right. A focused face that is not the
/// whole hue reads as an ordinary control the moment the ring lands on it, which is the frame
/// nobody screenshots. The comparisons are against the tokens rather than against colour codes,
/// so a palette retune moves this test with the design instead of breaking it.
#[test]
fn the_danger_face_tints_when_idle_and_takes_the_whole_hue_when_focused() {
    let palette = ControlPalette::default();
    let idle = ControlStyle::Danger.face(false, ControlGround::Keyed, palette);
    let foc = ControlStyle::Danger.face(true, ControlGround::Keyed, palette);
    let (idle_fill, idle_ink) = (idle.top, idle.ink);
    let (foc_fill, foc_ink) = (foc.top, foc.ink);

    // FOCUSED: the hue is the face, under primary ink — the same substitution `Accent` makes
    // with its near-white, so focus reads identically across the whole control family.
    assert_eq!(foc_fill, theme::DANGER);
    assert_eq!(foc_ink, theme::TEXT_PRIMARY);

    // IDLE: the neutral plate with the hue leaned into it, and the label in the hue itself —
    // the action is NAMED before the remote reaches it.
    assert_eq!(
        idle_fill,
        theme::mix(palette.idle, theme::DANGER, theme::DANGER_IDLE_TINT)
    );
    assert_eq!(idle_ink, theme::DANGER);
    let neutral = ControlStyle::Accent
        .face(false, ControlGround::Keyed, palette)
        .top;
    assert_ne!(
        idle_fill, neutral,
        "an idle destructive plate is not the neutral one"
    );
    assert_eq!(
        idle_fill[3], neutral[3],
        "…but it is the same MATERIAL — `mix` keeps the neutral's alpha, so the hue is the \
         only difference between the two idle plates"
    );
    // …and it is a TINT, not the fill: 16% of the way over, so the plate is nearer the
    // neutral it is derived from than the hue it is announcing.
    for c in 0..3 {
        let leaned = (idle_fill[c] - neutral[c]).abs();
        let whole = (theme::DANGER[c] - neutral[c]).abs();
        assert!(
            leaned <= whole * 0.5,
            "channel {c}: an idle plate that is half the hue is a fill, not a tint"
        );
    }
    assert_ne!(
        idle_fill, foc_fill,
        "the FILL is what focus owns, on this style as on every other"
    );
}

/// **No keyline on the danger face.** The knockout stroke is `Keyline`'s alone — a danger rim
/// would be a third signal saying what the plate and the label already say, and the perimeter
/// sheen every control wears (`control_rim`) is a card CONSTANT rather than a state.
///
/// `Button::plate` decides this with a `matches!` on one variant, so the property is not
/// something the type system defends: a later `| ControlStyle::Danger` added to that test
/// compiles and looks plausible. This is what refuses it.
#[test]
fn the_danger_face_wears_no_keyline() {
    let styled = |s: ControlStyle, focused: bool| {
        let b = Button::new(
            c"x".as_ptr(),
            theme::size::BODY,
            Rect::new(0.0, 0.0, 260.0, 60.0),
        )
        .style(s)
        .focused(focused);
        matches!(b.style, ControlStyle::Keyline) && !b.focused
    };
    assert!(
        !styled(ControlStyle::Danger, false),
        "an idle danger pill draws no stroke"
    );
    assert!(!styled(ControlStyle::Danger, true));
    assert!(
        styled(ControlStyle::Keyline, false),
        "the control case: Keyline idle still does"
    );
}

/// The unkeyed scope drops the ambient contract WHOLE. The player cannot sample its video
/// plane, so idle becomes the HUD's film and focus becomes flat ACCENT; a keyed page uses the
/// same focus semantics (bright face, dark ink, pop and cast) but lets both face levels answer
/// to the page hue. That is one control system with two knowability contracts, not two ranks.
#[test]
fn the_unkeyed_ground_drops_the_ambient_contract_whole() {
    let palette = ControlPalette::ambient([0.04, 0.39, 0.71]);
    let keyed_idle = ControlStyle::Accent.face(false, ControlGround::Keyed, palette);
    let video_idle = ControlStyle::Accent.face(false, ControlGround::Unkeyed, palette);
    assert_eq!(keyed_idle.top, palette.idle);
    assert_eq!(video_idle.top, theme::CONTROL_IDLE_FILL_UNKEYED);
    assert_ne!(keyed_idle.top, video_idle.top);
    assert_eq!(keyed_idle.ink, video_idle.ink, "one `--control-idle-ink`");

    let keyed_focus = ControlStyle::Accent.face(true, ControlGround::Keyed, palette);
    let video_focus = ControlStyle::Accent.face(true, ControlGround::Unkeyed, palette);
    assert_eq!((keyed_focus.top, keyed_focus.body), palette.focus_face());
    assert_ne!(
        keyed_focus.top, keyed_focus.body,
        "a keyed focus has a lit top and shaded body"
    );
    assert_eq!(
        (video_focus.top, video_focus.body),
        (theme::ACCENT, theme::ACCENT)
    );
    assert_eq!(
        keyed_focus.ink, video_focus.ink,
        "focus ink keeps the same meaning on both grounds"
    );
}

/// **Why the unkeyed idle face is a light FILM and not a darker plate** — the polarity argument,
/// as arithmetic rather than as a paragraph.
///
/// A control over video stands on the HUD's own black ramp, so its ground is `frame` darkened
/// by that ramp — dark, but by an amount that depends on a picture nobody can read. The film
/// composites LIGHTER than that ground whatever the frame is doing, which is what lets it read
/// as an object catching light; the keyed dark plate FLIPS — lighter than a night frame, and a
/// hole punched in a snow one. Composited in the same non-linear space the blender works in,
/// which is the space the decision was made in.
#[test]
fn the_unkeyed_idle_face_is_the_one_whose_polarity_cannot_flip() {
    // one channel is enough: both fills are neutral to within a 2/255 blue lean
    let over = |fill: [f32; 4], ground: f32| fill[0] * fill[3] + ground * (1.0 - fill[3]);
    // the ramp under the control row is nowhere near opaque, so the frame still reaches it
    let ramp = 0.45;
    let mut plate_flipped = false;
    for i in 0..=10 {
        let frame = i as f32 / 10.0;
        let ground = frame * (1.0 - ramp); // the frame, under the HUD's ramp
        let film = over(theme::CONTROL_IDLE_FILL_UNKEYED, ground);
        let plate = over(theme::CONTROL_IDLE_FILL, ground);
        assert!(
            film > ground,
            "frame {frame}: the film has to be the lighter of the two on EVERY frame"
        );
        plate_flipped |= plate < ground;
    }
    assert!(
        plate_flipped,
        "the keyed plate is supposed to fail on a bright frame — if it no longer does, the \
         whole reason for a second ground has gone and this pair should collapse back to one"
    );
}

/// The video film needs an edge of its own: applying the keyed card constant unchanged leaves
/// only a 0.12-alpha step over the film, which disappeared on the panel. The idle unkeyed rim
/// must therefore be brighter and wider than the keyed constant, while remaining visibly below
/// the pure-white focused edge so focus still has somewhere to go.
#[test]
fn an_idle_control_over_video_keeps_a_visible_edge_below_focus() {
    let (idle, idle_top, idle_w) = control_rim_spec(false, ControlGround::Unkeyed);
    let (focus, focus_top, focus_w) = control_rim_spec(true, ControlGround::Unkeyed);
    assert!(
        idle[3] > theme::CARD_SHEEN[3],
        "the film needs more edge than a keyed dark plate"
    );
    assert!(idle[3] < focus[3], "idle remains quieter than focus");
    assert!(
        idle_w > theme::CARD_SHEEN_W,
        "device rasterization needs the full unkeyed width"
    );
    assert_eq!(
        idle_w, focus_w,
        "ground owns one stroke geometry; state owns its brightness"
    );
    assert!(idle_top > 0.0, "idle keeps the shared lamp on its crown");
    assert_eq!(
        focus_top, 0.0,
        "nothing is brighter than the focused white edge"
    );

    assert_eq!(focus, theme::CONTROL_RIM_FOCUS_UNKEYED);
    let constant = (
        theme::CARD_SHEEN,
        theme::GLASS_RIM_LIGHT[3] - theme::CARD_SHEEN[3],
        theme::CARD_SHEEN_W,
    );
    assert_eq!(
        control_rim_spec(true, ControlGround::Keyed),
        constant,
        "focused on a page"
    );
    assert_eq!(control_rim_spec(false, ControlGround::Keyed), constant);
}

/// **Every control face is KEYED until its caller says otherwise** — including
/// [`TransportButton`], whose only caller today is the player HUD. A widget that defaulted to
/// its one caller's ground would be the one control in the app whose look came from where it
/// happened to be used rather than from what it was told, and the next screen to reach for it
/// would inherit a video treatment silently.
#[test]
fn a_control_face_is_keyed_until_it_is_told_otherwise() {
    let f = Rect::new(0.0, 0.0, 260.0, 60.0);
    assert_eq!(
        Button::new(c"x".as_ptr(), theme::size::BODY, f).ground,
        ControlGround::Keyed
    );
    assert_eq!(CircleButton::new(c"".as_ptr()).ground, ControlGround::Keyed);
    assert_eq!(TransportButton::new(0, f).ground, ControlGround::Keyed);
    assert_eq!(
        TabPill::new(c"Info".as_ptr(), theme::size::BODY, f).ground,
        ControlGround::Keyed
    );
    assert_eq!(
        Button::new(c"x".as_ptr(), theme::size::BODY, f)
            .ground(ControlGround::Unkeyed)
            .ground,
        ControlGround::Unkeyed,
    );
}

/// **The HUD's standalone pill is a control FACE, so the ground reaches it too** — the owner's
/// rule for the player is one material, and the Info/Chapters pill sits in the same band as the
/// transport discs. `TabPill::face` is a second implementation of the same choice `ControlStyle`
/// makes, which is exactly why it is graded rather than assumed.
///
/// A SEGMENT is the other half and the one worth pinning: it is inked by its strip's travelling
/// capsules and stands on that row's own track, never on video, so it must ignore the ground
/// outright rather than quietly resolving one.
#[test]
fn the_ground_reaches_the_standalone_pill_and_stops_at_a_segment() {
    let f = Rect::new(0.0, 0.0, 180.0, 64.0);
    let pill = |g: ControlGround, focused: bool| {
        TabPill::new(c"Info".as_ptr(), theme::size::BODY, f)
            .ground(g)
            .focused(focused)
            .face()
    };
    assert_eq!(
        pill(ControlGround::Keyed, false).0,
        Some(theme::CONTROL_IDLE_FILL)
    );
    assert_eq!(
        pill(ControlGround::Unkeyed, false).0,
        Some(theme::CONTROL_IDLE_FILL_UNKEYED)
    );
    assert_eq!(
        pill(ControlGround::Keyed, true),
        pill(ControlGround::Unkeyed, true),
        "this standalone tab has no ambient palette input, so only its idle ground treatment moves"
    );

    let seg = |g: ControlGround, selected: bool| {
        TabPill::new(c"Season 1".as_ptr(), theme::size::BODY, f)
            .ground(g)
            .segment(selected)
            .face()
    };
    for selected in [false, true] {
        assert_eq!(
            seg(ControlGround::Keyed, selected),
            seg(ControlGround::Unkeyed, selected),
            "a segment stands on its strip's track, so it has no ground of its own to answer to"
        );
    }
}

/// **A capsule is not a stadium, and a disc is still a circle.** The outline is chosen from the
/// SHAPE rather than from the widget, which is what makes an unfurled [`CircleButton`] come out
/// right without a flag: a circle takes none of it, and the same control opening into a capsule
/// takes the same outline a [`Button`] does.
#[test]
fn the_capsule_takes_the_blended_outline_and_the_disc_stays_a_circle() {
    assert!(
        face_outline(Rect::new(0.0, 0.0, 260.0, 60.0)).is_some(),
        "a pill"
    );
    assert!(
        face_outline(Rect::new(0.0, 0.0, 60.0, 60.0)).is_none(),
        "a disc is a circle"
    );
    assert!(
        face_outline(Rect::new(0.0, 0.0, 60.4, 60.0)).is_none(),
        "…and so is one a hair wide"
    );
    // the unfurl: the same control, once it has opened far enough to be a capsule
    assert!(face_outline(Rect::new(0.0, 0.0, 190.0, 60.0)).is_some());
}

/// **The drawn BOX is not the laid-out frame, and that is what keeps a capsule and a disc the
/// same object.** The end circle is under half its box, so a box the size of the frame would
/// draw a capsule's ends smaller than the disc beside it. The box carries the deficit; the
/// frame — every layout number in the app — does not move.
#[test]
fn a_capsules_box_carries_the_end_deficit_and_its_frame_does_not_move() {
    let f = Rect::new(100.0, 200.0, 260.0, 60.0);
    let b = face_box(f);
    assert!(b.h > f.h, "the box is taller: {} vs {}", b.h, f.h);
    assert_eq!(
        (b.x, b.w),
        (f.x, f.w),
        "and no wider, and it has not moved sideways"
    );
    assert!(
        (b.cy() - f.cy()).abs() < 0.001,
        "it grows about the frame's own centre"
    );
    let ends = face_outline(b)
        .expect("the grown box carries the outline")
        .end
        * 2.0;
    assert!(
        (ends - f.h).abs() < 0.01,
        "the drawn ends ARE the frame's height: {ends}"
    );
    // a disc is handed back untouched — growing one would make it an ellipse
    let d = Rect::new(0.0, 0.0, 60.0, 60.0);
    assert_eq!((face_box(d).w, face_box(d).h), (d.w, d.h));
}

/// **The focus cast is two stops and the CSS numbers are doubled on the way in.** A
/// `drop-shadow`'s third value is a standard deviation; this renderer's blur is a box-shadow's
/// diameter. Transcribing the design's 16 and 3 literally would draw a shadow half the size it
/// asks for — the kind of error that looks like taste from a couch, so it is pinned here.
#[test]
fn the_focus_cast_is_a_contact_and_a_pool_at_twice_the_css_blur() {
    let [pool, contact] = theme::CONTROL_CAST_FOCUS;
    assert_eq!(pool.1, 32.0, "drop-shadow 16px σ → 32px of box-shadow blur");
    assert_eq!(contact.1, 6.0, "…and 3 → 6");
    assert!(
        pool.0 > contact.0 && pool.1 > contact.1,
        "the pool falls further and spreads wider"
    );
    assert!(pool.2 > contact.2, "and carries more ink than the contact");
    for (_, _, a) in theme::CONTROL_CAST_FOCUS {
        assert!(a > 0.0 && a < 0.5, "a lift, not a hole: {a}");
    }
}

#[test]
fn a_selected_player_tab_keeps_a_quiet_face_until_it_owns_focus() {
    let pill = || TabPill::new(c"Chapters".as_ptr(), theme::size::BODY, Rect::new(0.0, 0.0, 200.0, 64.0))
        .ground(ControlGround::Unkeyed).selected(true);
    let selected = pill().face();
    assert_eq!(selected, (Some(theme::TAB_PLATE_SELECTED), theme::TEXT_PRIMARY, 1.0));
    assert_ne!(selected.0, Some(crate::ACCENT));
    let focused = pill().focused(true).face();
    assert_eq!(focused, (Some(crate::ACCENT), crate::ACCENT_INK, 1.0));
}

// ---- a press abandoned by a direction key --------------------------------------------------

/// OK goes down on control 0 and is held 350 ms, then a direction key moves focus to control 1 and
/// abandons the press (`Press::cancel`). The spring-back must play on control 0, the one pressed,
/// while control 1 takes the ordinary focus pop: no control's scale may change by more than the
/// ordinary per-frame step (3 %; the bug was -8 % / +9 % in ONE frame) and each ends where the focus
/// look puts it. Real `Press`, real `CtlPop`, one 16 ms frame at a time.
#[test]
fn an_abandoned_press_springs_back_on_the_control_that_was_pressed() {
    let mut pop: CtlPop<2> = CtlPop::new();
    let mut press = crate::press::Press::new();
    let mut focus = Some(0);
    for _ in 0..60 { pop.step(focus, 0.016); }
    let mut ms = 0;
    press.begin(ms);
    let mut prev = [pop.scale_with(0, press.scale()), pop.scale_with(1, press.scale())];
    let mut worst = 0.0f32;
    let mut dipped_after_move = 0.0f32;
    for n in 1..=60 {
        if n == 22 {
            press.cancel();
            focus = Some(1);
        }
        ms += 16;
        press.tick(ms, 0.016);
        pop.step(focus, 0.016);
        let now = [pop.scale_with(0, press.scale()), pop.scale_with(1, press.scale())];
        for c in 0..2 {
            worst = worst.max((now[c] / prev[c] - 1.0).abs());
        }
        if n == 22 { dipped_after_move = now[0]; }
        prev = now;
    }
    assert!(worst < 0.03, "a control's scale jumped {:.1} % in one frame", worst * 100.0);
    assert!(dipped_after_move < CTRL_FOCUS_SCALE * 0.95, "the pressed control is still dipped on the frame focus moved: {dipped_after_move}");
    assert!((prev[0] - 1.0).abs() < 0.01, "the pressed control settles at rest: {}", prev[0]);
    assert!((prev[1] - CTRL_FOCUS_SCALE).abs() < 0.01, "the control focus moved to settles at the focus pop: {}", prev[1]);
}

/// The avatar picker's call shape: `focused` is per call, the latch holds the pressed avatar.
#[test]
fn the_dip_latch_follows_the_pressed_index_not_focus() {
    let l = DipLatch::default();
    assert_eq!(l.factor(0, Some(0), 1.0), 1.0, "at rest nothing dips");
    assert_eq!(l.factor(1, None, 0.9), 1.0, "an unfocused avatar painted first does not latch");
    assert_eq!(l.factor(0, Some(0), 0.9), 0.9, "the focused avatar latches");
    assert_eq!(l.factor(0, None, 0.92), 0.92, "focus moved on: the pressed avatar keeps the dip");
    assert_eq!(l.factor(1, Some(1), 0.92), 1.0, "and the new focus does not take it");
    assert_eq!(l.factor(0, None, 1.0), 1.0, "back at rest releases");
    assert_eq!(l.factor(1, Some(1), 0.95), 0.95, "the next press latches afresh");
}

/// Frames of a control row under a real press: `reads` says whether the row is drawn (scale read)
/// on a given frame. Returns the final scales.
fn row_frames(pop: &mut CtlPop<2>, press: &mut crate::press::Press, ms: &mut u32, focus: Option<usize>, n: usize, draws: bool) -> [f32; 2] {
    let mut out = [1.0; 2];
    for _ in 0..n {
        *ms += 16;
        press.tick(*ms, 0.016);
        pop.step(focus, 0.016);
        if draws { out = [pop.scale_with(0, press.scale()), pop.scale_with(1, press.scale())]; }
    }
    out
}

/// A press that commits a page or popover leaves the row undrawn while its spring settles. The
/// latch must not survive into the next press: pressing control 1 dips control 1, not control 0.
#[test]
fn a_press_after_an_undrawn_spring_dips_the_control_pressed_now() {
    let mut pop: CtlPop<2> = CtlPop::new();
    let mut press = crate::press::Press::new();
    let mut ms = 0;
    for _ in 0..60 { pop.step(Some(0), 0.016); }
    press.begin_ctl(ms);
    row_frames(&mut pop, &mut press, &mut ms, Some(0), 10, true);
    press.release(ms);
    // the row stops drawing (a pushed page) until the spring has settled
    row_frames(&mut pop, &mut press, &mut ms, Some(0), 80, false);
    assert!(!press.is_active(), "the spring settled while the row was undrawn");
    for _ in 0..60 { pop.step(Some(1), 0.016); }
    press.begin_ctl(ms);
    let s = row_frames(&mut pop, &mut press, &mut ms, Some(1), 10, true);
    assert!(s[1] < CTRL_FOCUS_SCALE * 0.97, "control 1 dips: {}", s[1]);
    assert!((s[0] - 1.0).abs() < 0.01, "control 0 does not: {}", s[0]);
}

/// OK on 0, Right at 350 ms, OK on 1 inside the spring-back: from the frame the new press arms the
/// dip belongs to control 1, and nothing jumps by more than the residual of the old spring-back.
#[test]
fn a_press_armed_inside_the_previous_springback_dips_the_new_control() {
    let mut pop: CtlPop<2> = CtlPop::new();
    let mut press = crate::press::Press::new();
    let mut ms = 0;
    for _ in 0..60 { pop.step(Some(0), 0.016); }
    press.begin_ctl(ms);
    row_frames(&mut pop, &mut press, &mut ms, Some(0), 21, true);
    press.cancel();
    let mut prev = row_frames(&mut pop, &mut press, &mut ms, Some(1), 3, true);
    assert!(press.is_active() && prev[0] < CTRL_FOCUS_SCALE * 0.99, "control 0 is springing back: {prev:?}");
    let residual = (1.0 - press.scale()).abs();
    press.begin_ctl(ms);
    for n in 0..20 {
        let now = row_frames(&mut pop, &mut press, &mut ms, Some(1), 1, true);
        for c in 0..2 {
            let step = (now[c] / prev[c] - 1.0).abs();
            assert!(step < 0.03 + residual, "frame {n}: control {c} jumped {:.1} %", step * 100.0);
        }
        if n >= 4 {
            assert!(now[1] < CTRL_FOCUS_SCALE * 0.98, "frame {n}: the dip belongs to control 1: {now:?}");
            assert!(now[0] > 0.995, "frame {n}: control 0 only unpops, it no longer dips: {now:?}");
        }
        prev = now;
    }
}

/// **The focus fill is a cross-fade on the lift spring, not a step beside it.** Frame by frame, as
/// the hero row does it (`CtlPop::step`, then each control drawn at `CtlPop::focus`): focus moves
/// from control 0 to control 1 and the plate of BOTH changes by no more than the lift spring itself
/// moved, out = 1 - in, and the two land exactly on the rest faces (the INK is a choice between the
/// rest inks, not a cross-fade: `the_ink_over_a_focus_ramp_is_one_of_the_two_rest_inks_and_flips_once`).
/// Before the rule the
/// plate flipped in the first frame (18,738 px on Home, 14,151 on Detail) while the lift took
/// twenty.
#[test]
fn the_focus_fill_crossfades_on_the_lift_spring_out_equals_one_minus_in() {
    let palette = ControlPalette::ambient([0.7, 0.3, 0.2]);
    let idle = ControlStyle::Accent.face(false, ControlGround::Keyed, palette);
    let lit = ControlStyle::Accent.face(true, ControlGround::Keyed, palette);
    let mut pop = CtlPop::<2>::new();
    for _ in 0..200 {
        pop.step(Some(0), 1.0 / 60.0);
    }
    let draw = |pop: &CtlPop<2>, i: usize| {
        let b = Button::new(c"Resume".as_ptr(), theme::size::BODY, Rect::new(0.0, 0.0, 200.0, 60.0)).palette(palette).focus_mix(pop.focus(i));
        let c = CircleButton::new(c"".as_ptr()).palette(palette).focus_mix(pop.focus(i));
        (b.face_now(), c.face_now())
    };
    let step = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    let span = step(idle.top, lit.top);
    assert!(span > 0.2 && step(idle.ink, lit.ink) > 0.2, "the two faces must differ for the test to see a step");
    let (mut prev0, mut prev1) = (pop.focus(0), pop.focus(1));
    let (mut f0, mut f1) = (draw(&pop, 0).0, draw(&pop, 1).0);
    let near = |a: [f32; 4], b: [f32; 4]| step(a, b) < 1e-3;
    assert!(near(f0.top, lit.top) && near(f0.ink, lit.ink) && near(f1.top, idle.top) && near(f1.ink, idle.ink));
    let mut frames_between = 0;
    for _ in 0..60 {
        pop.step(Some(1), 1.0 / 60.0);
        let (t0, t1) = (pop.focus(0), pop.focus(1));
        let (n0, n1) = (draw(&pop, 0), draw(&pop, 1));
        // the plate moves by exactly the spring's step, never more
        assert!(step(n0.0.top, f0.top) <= span * (prev0 - t0).abs() + 1e-4, "control 0's plate stepped past its spring");
        assert!(step(n1.0.top, f1.top) <= span * (t1 - prev1).abs() + 1e-4, "control 1's plate stepped past its spring");
        // out = 1 - in: the two controls' progress shares one spring and the disc agrees with the pill
        assert!((t0 + t1 - 1.0).abs() < 1e-3, "out and in must sum to one: {t0} + {t1}");
        assert_eq!(n0.0.top, n0.1.top);
        if t0 > 0.05 && t0 < 0.95 {
            frames_between += 1;
        }
        (prev0, prev1, f0, f1) = (t0, t1, n0.0, n1.0);
    }
    assert!(frames_between >= 4, "the fade must take several frames, took {frames_between}");
    let (end0, end1) = (draw(&pop, 0).0, draw(&pop, 1).0);
    assert!(near(end0.top, idle.top) && near(end0.ink, idle.ink) && near(end1.top, lit.top) && near(end1.ink, lit.ink));
}

/// The worst contrast a face's ink gets against its own plate: the label is read over the gradient,
/// so the lower of the two stops, each laid over `INK_GROUND` by its alpha, the ink over each by its.
fn worst_ink_contrast(face: &ControlFace) -> f32 {
    [face.top, face.body]
        .map(|c| over(INK_GROUND, c, c[3]))
        .into_iter()
        .map(|fill| contrast(over(fill, face.ink, face.ink[3]), fill))
        .fold(f32::MAX, f32::min)
}

/// **A control's label is readable on EVERY tick of its focus ramp.** The hero's Resume pill went
/// from a dark idle plate with white ink to a near-white plate with dark ink on the lift spring, and
/// the ink was cross-faded on that same spring: half way, a mid-grey ink on a mid-grey plate (1.10:1
/// to 2.92:1 in the Detail frames, 1.34:1 on Home). The ink is ONE choice per frame now: whichever of
/// the two endpoint inks reads best against the plate composited at that tick (the flip sits where
/// white and the dark ink have equal contrast, plate luma ~0.18), the same max-contrast rule the nav
/// strip's pills wear, never a mix of the two.
#[test]
fn a_controls_ink_reads_at_three_to_one_on_every_tick_of_its_focus_ramp() {
    let ambient: [[f32; 3]; 4] = [[0.7, 0.3, 0.2], [0.16, 0.22, 0.30], [0.95, 0.9, 0.8], [0.1, 0.5, 0.2]];
    let mut worst = (f32::MAX, 0.0);
    for ground in [ControlGround::Keyed, ControlGround::Unkeyed] {
        let palettes = ambient.map(ControlPalette::ambient).into_iter().chain([ControlPalette::default()]);
        for palette in palettes {
            for (name, style) in [("Accent", ControlStyle::Accent), ("Keyline", ControlStyle::Keyline)] {
                for tick in 0..=240 {
                    let t = tick as f32 / 240.0;
                    let c = worst_ink_contrast(&style.face_at(t, ground, palette));
                    if c < worst.0 {
                        worst = (c, t);
                    }
                    assert!(c >= 3.0, "{name} on {ground:?} at focus {t:.3}: the ink reads at {c:.2}:1");
                }
            }
        }
    }
    // 3.92:1 at t=0.554 measured: the flip is where white and the dark ink are equally readable.
    assert!(worst.0 >= 3.5, "worst ink contrast over the ramp: {:.2}:1 at t={:.3}", worst.0, worst.1);
}

/// The ink is a CHOICE between the two endpoint inks, so what a frame draws is always one of the
/// two inks the face already owns (never a mix), it flips exactly once on the way in, and both rests
/// are exactly the two faces `face` already drew.
#[test]
fn the_ink_over_a_focus_ramp_is_one_of_the_two_rest_inks_and_flips_once() {
    let palette = ControlPalette::ambient([0.7, 0.3, 0.2]);
    for ground in [ControlGround::Keyed, ControlGround::Unkeyed] {
        let (idle, lit) = (ControlStyle::Accent.face(false, ground, palette), ControlStyle::Accent.face(true, ground, palette));
        assert_ne!(idle.ink, lit.ink);
        let mut flips = 0;
        let mut prev = idle.ink;
        for tick in 0..=480 {
            let ink = ControlStyle::Accent.face_at(tick as f32 / 480.0, ground, palette).ink;
            assert!(ink == idle.ink || ink == lit.ink, "{ground:?}: a mixed ink {ink:?} at tick {tick}");
            flips += usize::from(ink != prev);
            prev = ink;
        }
        assert_eq!(flips, 1, "{ground:?}: the ink flips once");
        assert_eq!(ControlStyle::Accent.face_at(0.0, ground, palette).ink, idle.ink);
        assert_eq!(ControlStyle::Accent.face_at(1.0, ground, palette).ink, lit.ink);
    }
}

/// **A disc's glyph is drawn from ONE mask for the whole of its pop.** The glyph's raster size used
/// to be rounded off the POPPED height, so `icons::draw` (which keys its cache on the box it is
/// handed) swapped masks each time the lift spring crossed a whole pixel, and the last swap landed on
/// the frame the spring settled: the filled hero bookmark re-rastered (122-225 px) with nothing
/// moving. `icons::draw` returns early for a recording painter, so the choice is graded where it is
/// made: `CircleButton::glyph_size`.
#[test]
fn a_discs_glyph_keeps_one_raster_across_its_whole_pop() {
    let base_h = StatusOverlay::CTRL_H;
    // The old arithmetic, to show the swap the new one removes: the mask the popped box asked for.
    let old = |scale: f32| crate::icons::icon_raster_px((base_h * scale * DISC_ICON_RATIO).round() as i32);
    assert_ne!(old(1.0), old(CTRL_FOCUS_SCALE), "the pop crossed a whole pixel of the old raster");
    let (rest_side, rest_raster) = CircleButton::glyph_size(base_h, 1.0);
    assert_eq!(rest_side, rest_raster as f32, "at rest the mask is drawn 1:1");
    assert_eq!(rest_raster, crate::icons::icon_raster_px(rest_raster));
    // Sampled finely across the dip (0.9x) through the pop (1.07x) and the settle exactly on it.
    for tick in 0..=170 {
        let scale = 0.9 + tick as f32 * 0.001;
        let (side, raster) = CircleButton::glyph_size(base_h, scale);
        assert_eq!(raster, rest_raster, "scale {scale}: the glyph changed mask");
        assert!((side - rest_side * scale).abs() < 1e-3, "scale {scale}: the box follows the spring continuously");
    }
}
