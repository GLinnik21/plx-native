//! The top bar's glass at any strip width (the chip's name budget keeps the unfurled capsule off the track) and the dynamic glass refresh cadence.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The unfurled chip and the glass track can never touch, at ANY strip width** — the one thing
/// about this band that no shader can fix, so it is arithmetic and it is asserted.
///
/// There is ONE blur cache. A second glass surface drawn over the first samples that same
/// snapshot, so an overlap gets no second blur — only a second scrim and a second rim
/// composited over material that already carries both, which is the doubling `draw_tab_row`'s
/// source-pass note measures from the other side (a face reading (52,60,38) came out at
/// (33,38,26) once it contained itself).
///
/// The track is centred and its width is the strip's, which is data (libraries, languages), so the
/// clearance cannot be bought by capping the track, as it was until 2026-10-30 (the retired
/// `GLASS_TRACK_MAX` dropped the owner's 679 px Belarusian strip to the flat capsule). It is the
/// chip's to keep: [`chip_name_budget`] is the name width the bar leaves room for, the owner elides
/// to it (`profile_chip_text`), and the capsule drawn is [`chip_cap`] of the elided name. So this
/// sweeps the TRACK from a lone pill to the widest the strip draws and asks the three questions
/// that chain can get wrong, through the same expressions the product draws with and a font-free
/// measure: the capsule's right edge plus [`BAND_AIR`] stays left of the track's left edge; a bar
/// that leaves no room draws the closed circle (no name, nothing unfurled); and the clearance is
/// TIGHT, within one glyph of the room, so nobody buys it by quietly spending the name.
#[test]
fn the_unfurled_chip_never_reaches_the_glass_track_at_any_strip_width() {
    let measure = crate::fixture::FixtureMeasure;
    // one ASCII glyph of `FixtureMeasure` at the chip's rung: the finest the elision can land
    let glyph = theme::size::BODY as f32 * 0.5;
    // no spaces: `elide_by` trims a cut that lands on one, which would cost the tightness check
    // a second glyph for a reason that is not the budget's
    let long = "Abcdefghij".repeat(8);
    let long = long.as_str();
    let circle = chip_cap(0.0, 0.0);
    let widest = TAB_VIEW_MAX + 2.0 * TAB_TRACK_PAD;
    let (mut no_name, mut named, mut clipped) = (0, 0, 0);
    let mut w = TAB_ICON_PILL_W + 2.0 * TAB_TRACK_PAD;
    while w <= widest + 0.01 {
        let track_left = (crate::consts::SCR_W - w) * 0.5;
        let budget = chip_name_budget(w);
        assert!(
            budget == 0.0 || (CHIP_NAME_MIN..=CHIP_NAME_MAX).contains(&budget),
            "track {w}: a budget of {budget} is neither no name nor a usable one"
        );
        for label in [long, "Guest", ""] {
            let (_, name, name_w) = profile_chip_text(label, "G", budget, &measure);
            let cap = chip_cap(1.0, name_w);
            assert!(
                name_w <= budget + 0.01,
                "track {w}: \"{label}\" came out {name_w} wide against a budget of {budget}"
            );
            assert!(
                cap.x + cap.w + BAND_AIR <= track_left + 0.01,
                "track {w}: the unfurled capsule ends at {} and the track starts at {track_left} \
                 — they must clear each other by {BAND_AIR}",
                cap.x + cap.w,
            );
            if budget == 0.0 || label.is_empty() {
                assert!(name.as_bytes().is_empty(), "track {w}: no room, no name (\"{label}\")");
                assert_eq!((name_w, cap), (0.0, circle), "track {w}: a nameless chip is the closed circle");
            }
        }
        // the capsule only ever grows RIGHTWARD off a fixed edge and only to the name's width
        let (_, _, long_w) = profile_chip_text(long, "G", budget, &measure);
        for e in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = chip_cap(e, long_w);
            assert_eq!((c.x, c.y, c.h), (circle.x, circle.y, circle.h), "only the width moves");
            assert!(c.w >= circle.w - 0.01 && c.w <= chip_cap(1.0, long_w).w + 0.01);
        }
        if budget == 0.0 {
            no_name += 1;
        } else {
            named += 1;
            // TIGHT: while the bar is what limits the name (not the ceiling), the slack left
            // between the capsule and the AIR is under one glyph
            let cap = chip_cap(1.0, long_w);
            if budget < CHIP_NAME_MAX {
                clipped += 1;
                let slack = track_left - BAND_AIR - (cap.x + cap.w);
                assert!(
                    slack < glyph + 0.01,
                    "track {w}: {slack} px of room unspent (a glyph is {glyph})"
                );
            }
        }
        w += 1.0;
    }
    assert!(no_name > 0, "the widest strip must leave the chip no name, or the sweep never met that case");
    assert!(clipped > 0, "some strips must be what limits the name, or the sweep never met that case");
    assert!(named > 0, "some strips must leave the chip a name");
}

/// The ceiling is the shipped bar's budget: the Belarusian strip (679 px, the widest of the shipped
/// locales: Russian, the next, is 666 px, 13 px narrower, by the shipped font's advances) and every
/// narrower one keep the whole [`CHIP_NAME_MAX`], so the room arithmetic only ever bites on a strip
/// far past anything shipped. Pinned so a change to the chip's gap, tail or frame that quietly
/// shrinks every profile name on the TV is a test failure and not a surprise.
#[test]
fn the_shipped_strips_leave_the_chip_its_whole_name_budget() {
    let belarusian = [168.0, 144.0, 167.0, TAB_ICON_PILL_W, TAB_ICON_PILL_W];
    let english = [126.0, 146.0, 176.0, TAB_ICON_PILL_W, TAB_ICON_PILL_W];
    for widths in [&belarusian[..], &english[..]] {
        assert_eq!(chip_name_budget(tab_track_w(widths)), CHIP_NAME_MAX);
    }
    assert_eq!(chip_name_budget(0.0), CHIP_NAME_MAX, "no strip at all constrains nothing");
}

/// A nameless chip has nothing to unfurl for, so every unfurl of [`chip_cap`] is the closed circle,
/// and a named chip still grows rightward.
#[test]
fn a_chip_with_no_name_stays_the_closed_circle_and_a_named_one_unfurls() {
    let circle = chip_cap(0.0, 0.0);
    assert_eq!(circle.w, circle.h, "the resting surround is circular");
    for e in [0.0, 0.5, 1.0] {
        assert_eq!(chip_cap(e, 0.0), circle, "no name, no unfurl (e = {e})");
    }
    assert!(chip_cap(1.0, 40.0).w > circle.w, "a name unfurls the capsule");
    assert_eq!(chip_cap(1.0, CHIP_NAME_MAX), CHIP_CAP_MAX);
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
    let face = plx_gfx::gfx::GlassFace {
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

/// What [`drawn_strip`] read off one draw of the shared strip.
struct DrawnStrip {
    /// the band published the glass material (the flat capsule publishes none)
    glass: bool,
    /// the pills plus the air between them
    content: f32,
    /// the track the draw stood on ([`tab_track_w`])
    track: f32,
}

/// **DRAW the shared strip for `faces`** (a recording painter, the ground read answered by the
/// last reading) and report the material the draw published on the band, with the widths it stood
/// on. [`tab_glass_on`] takes no width, so asking it says nothing about any strip: the band's
/// published material is what the chip, the account menu's lift and the planner read, and it is
/// the answer that changes if the draw ever decides the material by width again.
fn drawn_strip(generation: u32, faces: &[TabFace]) -> DrawnStrip {
    let _g = serial_for_motion();
    // the draw calls `gfx::sample_ground`, which reads a band image and so needs a walk; with none
    // it answers the last reading and copies nothing
    let labels = TabLabels { generation, faces };
    let mut strip = StripRender::new();
    let mut band = TabBand::new();
    strip.update(labels, 0, TopFocus::Away, DT);
    strip.draw(labels, Painter::recording(), &mut band);
    let (content, track) = with_tab_metrics_for(labels, |_, _, w| (tab_content_w(w), tab_track_w(w)));
    DrawnStrip { glass: band.face().is_some(), content, track }
}

/// **The Belarusian strip keeps the material** (the regression this file exists for since
/// 2026-10-30). "Галоўная", "Фільмы", "Серыялы" and the Watchlist and Search marks measure
/// 132 / 108 / 131 px of bold BODY label on the television, i.e. pills of 168 / 144 / 167 plus two
/// 60 px marks: a 679 px track, 7 px past the 672 the retired `GLASS_TRACK_MAX` allowed, so the
/// whole bar fell to the flat capsule on the owner's television the moment the Watchlist pill was
/// added. (The app's own layout rect for the track reads `620,54,679,76` there; the 676 px a
/// capture shows is the scrim INSIDE the capsule's 1 px rims, not the track.)
///
/// The test DRAWS a strip past that cap ([`drawn_strip`]). The host has no font and measures half
/// an em a byte, so those television widths cannot be reproduced word for word, and the television's
/// width cannot be reproduced at all: a host pill is 14 px a byte plus 36, a mark is 60 and the air
/// is 16, every term even, so no strip the host draws is an odd number of pixels wide and 679 is
/// odd. The strip drawn has the television's shape (three labels, then the Watchlist and Search
/// marks) with 27 bytes of label, which is 686 px: 14 px past the retired cap and 7 px past the
/// television's strip, and the test pins both tracks.
#[test]
fn the_belarusian_five_pill_strip_keeps_the_glass() {
    /// what the retired `GLASS_TRACK_MAX` was at the end
    const RETIRED_GLASS_TRACK_MAX: f32 = 672.0;
    let widths = [168.0, 144.0, 167.0, TAB_ICON_PILL_W, TAB_ICON_PILL_W];
    assert_eq!(tab_track_w(&widths), 679.0, "the strip the owner's television draws");
    let faces = [
        TabFace::Label("Libraries".into()),
        TabFace::Label("Galleries".into()),
        TabFace::Label("Playlists".into()),
        TabFace::Icon(crate::icons::Icon::Bookmark),
        TabFace::Icon(crate::icons::Icon::Search),
    ];
    let drawn = drawn_strip(3, &faces);
    assert_eq!(drawn.track, 686.0, "the host's strip of the television's shape, 27 bytes of label");
    assert!(
        drawn.track > RETIRED_GLASS_TRACK_MAX,
        "the strip drawn ({} px) must be past the cap that dropped the television's",
        drawn.track
    );
    assert!(drawn.glass, "a {} px track dropped to the flat capsule", drawn.track);
}

/// **No strip width takes the material away** — a section table can add libraries and a language
/// can lengthen every label, and the bar must still be glass. This DRAWS the bar ([`drawn_strip`])
/// for the shipped strip and for one far past [`TAB_VIEW_MAX`], which scrolls inside a track that
/// stops growing ([`tab_view_w`]), and asks the band what material the draw published: glass for
/// both. The only refusal left is the `flattabs` experiment.
#[test]
fn any_strip_keeps_the_material_including_one_that_scrolls() {
    let normal = vec![
        TabFace::Label("Home".into()),
        TabFace::Label("Movies".into()),
        TabFace::Label("TV Shows".into()),
        TabFace::Icon(crate::icons::Icon::Bookmark),
        TabFace::Icon(crate::icons::Icon::Search),
    ];
    // two dozen libraries: far more than the viewport holds, whatever the font's advances
    let many: Vec<TabFace> = (0..24)
        .map(|i| TabFace::Label(format!("Library number {i}")))
        .chain([TabFace::Icon(crate::icons::Icon::Bookmark), TabFace::Icon(crate::icons::Icon::Search)])
        .collect();
    let mut widest = 0.0f32;
    for (generation, faces) in [(1, normal), (2, many)] {
        let drawn = drawn_strip(generation, &faces);
        widest = widest.max(drawn.content);
        assert!(
            drawn.glass,
            "a {} px track (content {}) lost the material",
            drawn.track, drawn.content
        );
    }
    assert!(
        widest > TAB_VIEW_MAX,
        "the second strip must outgrow the viewport ({widest} against {TAB_VIEW_MAX}) to mean what it says"
    );
}

/// The shipped default, asserted in ONE place so moving it is a one-line decision with a
/// measurement behind it rather than a cascade through the cadence tests.
#[test]
fn the_shipped_cadence_is_every_changed_present() {
    use crate::frame::backdrop::{decide,Request,Damage,Z,canvas};
    assert!(decide(Request {z:Z::CHROME,rect:canvas(),valid:true}, &[],
        &[Damage {z:Z::PAGE,rect:canvas()}]).refresh);
    assert!(!decide(Request {z:Z::CHROME,rect:canvas(),valid:true}, &[], &[]).refresh);
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
        let ramp = crate::landing_hero::base_scrim_ramp(hero_a);
        for y in [
            0.0f32,
            200.0,
            HERO_BASE_SCRIM_Y0,
            400.0,
            600.0,
            702.0,
            900.0,
            crate::consts::SCR_H,
        ] {
            let one = hero_ground_ramp_a(ramp, y);
            let quads = crate::landing_hero::base_scrim_a(y, hero_a);
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

/// **Every glass policy COMPOSITES**, and the axis that let one not to is gone.
///
/// This test used to assert the source-dim arithmetic — `Glass::DYNAMIC.source_rgb(0.5) == 0.75`
/// and so on — for a preset nothing outside this module ever constructed. The mechanism was
/// measured against the item menu over one checker ground and rejected (`e75b5e49`): dimming the
/// page going INTO the backdrop destroys the modulation the frost is layered over, so the panel
/// arrives flat. What is worth pinning now is that no policy can reintroduce it by accident —
/// a material carries no private scheduling policy.
#[test]
fn live_glass_carries_no_private_source_policy() {
    // Scheduling is owned by the layer walk, never an independently phased widget clock.
    assert_eq!(
        std::mem::size_of::<Glass>(),
        0,
        "a second axis on Glass would show up here first"
    );
}

/// **Glass over the blur chain is chrome only; no popover or screen names the blur path.** Every
/// panel's ground goes through `panel_ground`, which draws `popover_glass` (DS6) for a
/// `Ground::Menu` ONLY — glass on the frozen ground, which reads no framebuffer — and stands a
/// `Ground::Sheet` on the latched underlay field (directly, or through `Popover::panel`), as a menu
/// does too with no latched field. The blur chain's users are the top bar's standing track, the profile chip's capsule, the dev tile
/// band — all in `widgets.rs` — the standing hold hint's capsule (`hold_hint.rs`: a non-interactive
/// standing note, the track's own material, budgeted by
/// `hold_hint::tests::the_capsules_own_blur_region_fits_the_budget_and_never_meets_the_top_band`),
/// the library's scroll capsule (`scroll_capsule.rs`: the same standing container over the page
/// the chrome tucked away from, budgeted by
/// `scroll_capsule::tests::a_long_names_capsule_stays_inside_the_glass_region_budget`),
/// plus the frame mechanism and the dev load dial.
///
/// A source grep, because the failure it guards is a call that COMPILES: `Glass` and
/// `Painter::backdrop_blur` stay reachable for the chrome, so a panel that reached for them again
/// would draw, look plausible on a still page, and put the ~11% of a frame's GPU the migration
/// removed straight back.
#[test]
fn no_popover_panel_uses_the_blur_path_and_every_one_stands_on_the_field() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // The blur path's legitimate users: chrome, the frame plan, the dev dial, the API itself, and
    // a fixture that names it in a string.
    const CHROME: &[&str] = &[
        "ui/src/widgets.rs",
        // the standing hold hint: one `Glass::DYNAMIC_BACKDROP.backdrop` call, region-budgeted
        "ui/src/hold_hint.rs",
        // the library's scroll capsule: the nav pill's live glass over the page it tucked away
        // from, one `Glass::DYNAMIC_BACKDROP.backdrop` call, region-budgeted
        "ui/src/scroll_capsule.rs",
        "ui/src/frame/glass.rs",
        "ui/src/frame/backdrop_tests.rs",
        "ui/src/glassload.rs",
        "ui/src/lib.rs",
        "ui/src/fixture.rs",
        "ui/src/widgets_glass_budget_tests.rs",
    ];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read src dir") {
            let p = e.expect("dir entry").path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("ui/src"), &mut files);
    walk(&root.join("appkit/src"), &mut files);
    walk(&root.join("screens/src"), &mut files);
    let mut offenders = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if CHROME.contains(&rel.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(f).expect("read source");
        for (n, line) in src.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if ["Glass::", "GlassState", "backdrop_blur(", ".backdrop("]
                .iter()
                .any(|pat| code.contains(pat))
            {
                offenders.push(format!("{rel}:{}: {code}", n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the blur path is chrome-only — a panel's ground is `widgets::panel_ground`:\n{}",
        offenders.join("\n")
    );

    // …and the glass scope is exactly the MENUS (owner decision, wave 2 review): Sort & Filter, the
    // account menu and the item hold menu name `Ground::Menu`; every other panel (alerts, About,
    // Tracks, the person bio, the Also-available sheet, the decision alert) names `Ground::Sheet`.
    // The player's "…" menu stands over the video plane and draws no `panel_ground` at all.
    let ground_of = |rel: &str| {
        let src = std::fs::read_to_string(root.join(rel)).expect("read panel source");
        assert!(
            src.contains("panel_ground(") || src.contains("pop.panel(") || src.contains("popover.panel("),
            "{rel} must draw its ground through `panel_ground` (or `Popover::panel`)"
        );
        let code: String = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        (code.contains("Ground::Menu"), code.contains("Ground::Sheet"))
    };
    for rel in ["screens/src/item_menu.rs", "screens/src/account_menu.rs", "screens/src/library/menu.rs"] {
        assert_eq!(ground_of(rel), (true, false), "{rel} is a MENU: `Ground::Menu` and nothing else");
    }
    for rel in [
        "screens/src/alt_sources.rs",
        "screens/src/about_panel.rs",
        "screens/src/tracks_panel.rs",
        "screens/src/person_bio.rs",
        "ui/src/decision_alert.rs",
    ] {
        assert_eq!(ground_of(rel), (false, true), "{rel} is a SHEET: `Ground::Sheet` and nothing else");
    }
}
