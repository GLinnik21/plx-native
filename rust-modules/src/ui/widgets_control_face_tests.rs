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
    let mut p = crate::ui::press::Press::new();
    let _g = crate::testlock::serial();
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
