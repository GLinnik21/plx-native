//! The live-backdrop walk's tests that build their scene through `ui::Painter` and
//! `ui::widgets::Glass`: the walk itself moved to `gfx::backdrop` (module-layers step L5), and a
//! test of the `gfx` layer may not name `ui`, so these live with the layer that owns all of their
//! parts. The tests that need nothing above `gfx` stayed in `gfx/backdrop.rs`.
use plx_gfx::gfx::backdrop::*;
use plx_gfx::gfx::Rect;
use std::{cell::RefCell, rc::Rc};

fn rect(x: f32) -> Rect {
    Rect::new(x, 0.0, 10.0, 10.0)
}

fn declare_glass(p: crate::Painter, r: Rect) {
    assert!(crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
        p,
        r,
        0.0,
        2.0,
        [1.0; 4],
        plx_gfx::gfx::GlassRim::Standing,
        plx_gfx::gfx::GlassFace::NONE,
        crate::theme::Material::UltraThin
    ));
}

#[test]
fn still_commands_track_crop_texture_revision_and_scrim_under_cached_glass() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let tex = 9081;
    plx_gfx::gfx::tex_ledger::specified(tex, 720, 480);
    // Each changed input repeats once: its first frame invalidates; its settled twin reuses.
    for step in 0..14 {
        let state = step / 2;
        if step == 4 { plx_gfx::gfx::tex_ledger::specified(tex, 720, 480); }
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            let uv = if state >= 1 { [0.0, 0.1, 1.0, 0.8] } else { plx_gfx::gfx::UV_FULL };
            let rad = if state >= 3 { 18.0 } else { 14.0 };
            let focus = if state >= 4 { 0.5 } else { 0.0 };
            let band = if state >= 5 { 90.0 } else { 80.0 };
            let scrim = crate::theme::scrim(if state >= 6 { 0.8 } else { 0.7 });
            // `declare()` returns true here for a reason unrelated to fusion eligibility:
            // `discover(sources.clone())` keeps this whole block inside a DISCOVERY walk, where
            // `Painter::declare` always short-circuits true without ever reaching
            // `gfx::draw_tex_carded_still` (see `declare`'s own `!frame::backdrop::discovering()`
            // guard) — so this assertion never actually exercises the `f > 0.0` fusion refusal
            // added alongside `FOCUS_IMAGE`; it is purely a discovery-tracking smoke test.
            assert!(p.tex_carded_still(tex, uv, rect(0.0), rad, focus, band, scrim));
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        assert_eq!(!sources.borrow().entries[&Z(1)].valid, step % 2 == 0, "step {step}");
        commit(&sources);
    }
    plx_gfx::gfx::tex_ledger::deleted(tex);
}

#[test]
fn standalone_still_scrims_invalidate_only_when_their_picture_changes() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for step in 0..8 {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            assert!(p.art_scrim(rect(0.0), if step >= 2 { 18.0 } else { 14.0 },
                if step >= 4 { 90.0 } else { 80.0 },
                crate::theme::scrim(if step >= 6 { 0.8 } else { 0.7 })));
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        assert_eq!(!sources.borrow().entries[&Z(1)].valid, step % 2 == 0, "step {step}");
        commit(&sources);
    }
}

#[test]
fn the_real_painter_ignores_foreground_and_outside_motion_but_captures_settle() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for (i, x) in [0.0, 1.0, 2.0, 2.0].into_iter().enumerate() {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            p.rect(Rect::new(x, 0.0, 20.0, 20.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
            p.rect(
                Rect::new(900.0 + i as f32, 900.0, 20.0, 20.0),
                0.0,
                [0.0; 4],
                [0.0; 4],
                0.0,
            );
            declare_glass(p, rect(0.0));
            p.rect(rect(0.0), 0.0, [i as f32; 4], [0.0; 4], 0.0);
        }
        sources.borrow_mut().resolve();
        assert_eq!(!sources.borrow().entries[&Z(1)].valid, i < 3, "frame {i}");
        commit(&sources);
    }
}

#[test]
fn one_capture_per_band_covers_this_frames_union_even_on_activation() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let _band = layer(Z::CHROME, true);
        declare_glass(crate::Painter::root(), rect(0.0));
        declare_glass(crate::Painter::root(), rect(1000.0));
    }
    sources.borrow_mut().resolve();
    let jobs = sources.borrow().jobs();
    assert_eq!(jobs.len(), 1);
    assert!(covers(jobs[0].1, rect(0.0)) && covers(jobs[0].1, rect(1000.0)));
}

#[test]
fn overlapping_bands_retain_separate_underlays_and_capture_the_lower_composite() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
        declare_glass(p, rect(0.0));
        declare_glass(p, rect(0.0));
    }
    sources.borrow_mut().resolve();
    let sources = sources.borrow();
    assert!(sources.entries[&Z(1)]
        .underlay
        .iter()
        .all(|p| p.glass.is_none()));
    assert!(sources.entries[&Z(2)]
        .underlay
        .iter()
        .any(|p| p.glass == Some(Z(1))));
    assert!(sources.entries[&Z(2)]
        .underlay
        .iter()
        .all(|p| p.glass != Some(Z(2))));
    assert_eq!(
        sources.jobs().iter().map(|(z, _)| *z).collect::<Vec<_>>(),
        vec![Z(1)],
        "the upper band captures in visible order, with the lower glass's exact sharp rim"
    );
}

#[test]
fn a_frozen_replacement_removes_covered_lower_glass_from_upper_capture_dependencies() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));

    // Seed the lower retained glass and its overlapping upper neighbour.
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        declare_glass(p, rect(0.0));
        let _upper = layer(Z(3), false);
        declare_glass(p, rect(0.0));
    }
    sources.borrow_mut().resolve();
    commit(&sources);

    for dim_revision in [1.0f32, 2.0] {
        sources.borrow_mut().begin(vec![Layer {
            z: Z(2),
            rect: canvas(),
            blocks: true,
            revision: 9,
            composite_alpha: None,
        }]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            let _above_replacement = layer(Z(3), false);
            p.rect(rect(0.0), 0.0, [dim_revision; 4], [0.0; 4], 0.0);
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        let jobs = sources.borrow().jobs();
        assert_eq!(
            jobs.iter().map(|(z, _)| *z).collect::<Vec<_>>(),
            vec![Z(4)],
            "replay the frozen composite plus current dim directly; never refresh its covered lower glass"
        );
        commit(&sources);
    }
}

#[test]
fn adding_lower_glass_outside_the_sampled_region_does_not_refresh_shared_chrome() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for frame in 0..2 {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            if frame == 1 {
                declare_glass(p, rect(1000.0));
            }
            p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
            let _band = layer(Z::CHROME, true);
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        if frame == 1 {
            assert!(sources.borrow().entries[&Z::CHROME].valid);
        }
        commit(&sources);
    }
}

#[test]
fn a_moving_lower_glass_outside_the_sampled_region_does_not_refresh_it() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for frame in 0..2 {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            declare_glass(p, rect(1000.0 + frame as f32));
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        if frame == 1 {
            assert!(sources.borrow().entries[&Z(2)].valid);
        }
        commit(&sources);
    }
}

#[test]
fn recording_painters_do_not_declare_live_glass_or_underlay_damage() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::recording();
        p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
        assert!(!crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
            p,
            rect(0.0),
            0.0,
            2.0,
            [1.0; 4],
            plx_gfx::gfx::GlassRim::Standing,
            plx_gfx::gfx::GlassFace::NONE,
            crate::theme::Material::UltraThin
        ));
    }
    sources.borrow_mut().resolve();
    assert!(sources.borrow().entries.is_empty());
    assert!(sources.borrow().paints.is_empty());
}

#[test]
fn content_at_or_above_the_surfaces_band_is_never_recorded() {
    // The `nav.modals.surfaces` draw loop (Settings/AccountMenu/ItemMenu/About's own content,
    // `dispatch.rs::draw_with`) is not gated by `host_render`/`PagePlan`, so it walks fully on
    // every discovered frame — but nothing reads a `Paint` recorded at or above `Z::surface(0)`
    // (the one glass there, a menu's own, reads the prefix BELOW it), so recording what it
    // declares was pure cost for data nothing reads. This is the modal-100 /
    // push-100 stress-bench regression's second cause (see docs/backdrop-blur-profiling.md).
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
        assert_eq!(sources.borrow().paints.len(), 1, "below the surfaces band records");
        {
            let _layer = layer(Z::surface(0), false);
            p.rect(rect(0.0), 0.0, [1.0; 4], [0.0; 4], 0.0);
            p.rect(rect(0.0), 0.0, [2.0; 4], [0.0; 4], 0.0);
        }
        assert_eq!(
            sources.borrow().paints.len(),
            1,
            "content declared at/above the surfaces band must not be recorded"
        );
        {
            let _layer = layer(Z::PAGE, false);
            p.rect(rect(0.0), 0.0, [3.0; 4], [0.0; 4], 0.0);
        }
        assert_eq!(sources.borrow().paints.len(), 2, "layer unwinds back below the band");
    }
}

#[test]
fn a_glass_command_in_the_surfaces_band_is_dropped_but_its_entry_resolves() {
    // A glass declared inside a surface's layer (a menu's was, until it moved onto frozen ground,
    // `ui::popover_glass`). `Painter::declare`'s pre-check drops its command word, as it drops
    // everything at/above the band (nothing there is ever read), but the backdrop entry it
    // requested is real and is captured. This was a "must assert" pin while no glass lived in a
    // surface; once menus became glass it panicked every menu frame.
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
        let _layer = layer(Z::surface(0), false);
        declare_glass(p, rect(0.0));
    }
    let mut s = sources.borrow_mut();
    assert!(s.paints.iter().all(|p| p.glass.is_none()), "no glass command is recorded in the band");
    s.resolve();
    let z = *s.entries.keys().next().expect("the glass's entry exists");
    assert!(z >= Z::surface(0));
    assert!(s.jobs().iter().any(|&(jz, _)| jz == z), "and it is captured");
}

#[test]
fn content_below_a_frozen_host_boundary_is_never_recorded() {
    // A Cached host (Settings/AccountMenu over Home) still walks its full page+chrome tree on
    // every discovered frame; `held_ceiling()` publishes that freeze as a canvas-covering
    // blocking `Layer` (`dispatch.rs::backdrop_layers`). No live glass ever reads through it —
    // a glass above it retains the layer's own synthetic, revision-keyed `Paint` instead
    // (`Sources::begin`) — so recording the real primitives underneath is exactly the dead
    // work the surfaces-band fix already excludes above `Z::surface(0)`, just bounded by a
    // per-frame layer instead of a fixed ceiling. This is the modal-100 stress-bench
    // regression's open hypothesis (docs/backdrop-blur-profiling.md's last dated addendum).
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![Layer {
        z: Z(5),
        rect: canvas(),
        blocks: true,
        revision: 1,
        composite_alpha: None,
    }]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        assert_eq!(
            sources.borrow().paints.len(),
            1,
            "begin() already recorded the blocking layer's own synthetic paint"
        );
        {
            let _layer = layer(Z(1), false);
            p.rect(rect(0.0), 0.0, [1.0; 4], [0.0; 4], 0.0);
        }
        assert_eq!(
            sources.borrow().paints.len(),
            1,
            "content strictly below a canvas-wide blocking layer must not be recorded"
        );
        {
            let _layer = layer(Z(6), false);
            p.rect(rect(0.0), 0.0, [2.0; 4], [0.0; 4], 0.0);
        }
        assert_eq!(
            sources.borrow().paints.len(),
            2,
            "content at/above the frozen boundary is still live and must still record"
        );
    }
}

#[test]
fn glass_growing_into_unchanged_captured_pixels_reuses_its_source() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for frame in 0..2 {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            p.rect(rect(500.0), 0.0, [1.0; 4], [1.0; 4], 0.0);
            let _band = layer(Z::CHROME, true);
            declare_glass(
                p,
                Rect::new(0.0, 0.0, if frame == 0 { 10.0 } else { 700.0 }, 10.0),
            );
            declare_glass(p, rect(1000.0));
        }
        sources.borrow_mut().resolve();
        if frame == 1 {
            assert!(
                sources.borrow().entries[&Z::CHROME].valid,
                "unfurling chrome is not page damage"
            );
        }
        commit(&sources);
    }
}

#[test]
fn a_video_plane_does_not_declare_a_framebuffer_glass_source() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let _walk = discover(sources.clone());
    let old = plx_gfx::gfx::set_video_plane_frame(true);
    let handled = crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
        crate::Painter::root(),
        rect(0.0),
        0.0,
        2.0,
        [1.0; 4],
        plx_gfx::gfx::GlassRim::Standing,
        plx_gfx::gfx::GlassFace::NONE,
        crate::theme::Material::UltraThin,
    );
    plx_gfx::gfx::set_video_plane_frame(old);
    assert!(!handled);
    assert!(sources.borrow().entries.is_empty());
}

#[test]
fn shared_chrome_splits_when_its_glasses_overlap() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let _band = layer(Z::CHROME, true);
        declare_glass(crate::Painter::root(), rect(0.0));
        declare_glass(crate::Painter::root(), rect(0.0));
        declare_glass(crate::Painter::root(), rect(1000.0));
    }
    sources.borrow_mut().resolve();
    assert_eq!(
        sources.borrow().entries.keys().copied().collect::<Vec<_>>(),
        vec![Z::CHROME, Z(Z::CHROME.0 + 2), Z(Z::CHROME.0 + 3)]
    );
    assert_eq!(
        sources.borrow().jobs().len(),
        2,
        "the second band uses a visible prefix; the independent third stays above it"
    );
}

/// The top bar's chip (the profile circle) and its centred track: `BAND_AIR`-clear of each other
/// at every strip width, so they never overlap, but a track of 1432 px or more starts inside the
/// chip's sampling footprint (its rect grown by `BLUR_REACH`). Footprint contact alone must not
/// split the shared band: that is a second source job and a second page pass for glass that
/// samples the page, not each other.
#[test]
fn shared_chrome_neighbours_whose_footprints_touch_but_rects_do_not_keep_one_source() {
    let _guard = plx_base::testlock::serial();
    let chip = Rect::new(96.0, 54.0, 76.0, 76.0);
    let track = Rect::new(188.0, 54.0, 1544.0, 76.0);
    for (name, order) in [("chip then track", [chip, track]), ("track then chip", [track, chip])] {
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let _band = layer(Z::CHROME, true);
            for r in order {
                declare_glass(crate::Painter::root(), r);
            }
        }
        sources.borrow_mut().resolve();
        assert_eq!(
            sources.borrow().entries.keys().copied().collect::<Vec<_>>(),
            vec![Z::CHROME],
            "{name}: one source in the shared band"
        );
        let jobs = sources.borrow().jobs();
        assert_eq!(jobs.len(), 1, "{name}: one capture");
        assert!(covers(jobs[0].1, chip) && covers(jobs[0].1, track), "{name}: it covers both");
    }
}

#[test]
fn a_frozen_replacement_hides_lower_damage_but_its_new_image_invalidates() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for (frame, revision) in [7, 7, 8].into_iter().enumerate() {
        sources.borrow_mut().begin(vec![Layer {
            z: Z(2),
            rect: canvas(),
            blocks: true,
            revision,
            composite_alpha: None,
        }]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            p.rect(rect(0.0), 0.0, [frame as f32; 4], [0.0; 4], 0.0);
            let _upper = layer(Z(3), false);
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        assert_eq!(!sources.borrow().entries[&Z(4)].valid, frame != 1);
        commit(&sources);
    }
}

#[test]
fn held_image_alpha_reuses_the_filter_but_content_revision_invalidates_it() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for (frame, (revision, alpha)) in [(7, 0.2), (7, 0.8), (8, 0.8)].into_iter().enumerate() {
        sources.borrow_mut().begin(vec![Layer {
            z: Z(2),
            rect: canvas(),
            blocks: true,
            revision,
            composite_alpha: Some(alpha),
        }]);
        {
            let _walk = discover(sources.clone());
            let _upper = layer(Z(3), false);
            declare_glass(crate::Painter::root(), rect(0.0));
        }
        sources.borrow_mut().resolve();
        let jobs = sources.borrow().jobs();
        match frame {
            0 => assert_eq!(jobs.len(), 1, "the first filtered source is produced"),
            1 => assert!(jobs.is_empty(), "alpha-only change is composed from the cached filter"),
            2 => assert_eq!(jobs.len(), 1, "new held-image content invalidates the filter"),
            _ => unreachable!(),
        }
        commit(&sources);
    }
}

#[test]
fn clipped_out_glass_neither_captures_nor_draws() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        p.clip(Rect::new(0.0, 0.0, 5.0, 5.0));
        declare_glass(p, rect(100.0));
    }
    sources.borrow_mut().resolve();
    assert!(sources.borrow().entries.is_empty());
    let _walk = enter(sources, Z::ALL);
    assert!(!surface(Rect::new(-100.0, -100.0, 10.0, 10.0)).unwrap().draw);
}

/// A `ClipScope` (what `TableView::draw` opens) must reach the discovery walk's clip exactly as
/// `Painter::clip` does, and hand it back on drop.
#[test]
fn a_clip_scope_clips_the_discovery_walk_and_restores_it() {
    let _guard = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    for scoped in [true, false] {
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::Painter::root();
            if scoped {
                let _clip = crate::screen::ClipScope::open_in(p, Rect::new(0.0, 0.0, 5.0, 5.0));
                declare_glass(p, rect(100.0));
            } else {
                declare_glass(p, rect(100.0));
            }
        }
        sources.borrow_mut().resolve();
        assert_eq!(sources.borrow().entries.is_empty(), scoped, "scoped={scoped}");
        commit(&sources);
    }
}

/// A draw in the band between `BLUR_REACH` and `BLUR_MARGIN` of a glass that never rises cannot
/// reach a pixel of that glass, so it must not invalidate it; a glass that rises keeps the wider
/// grab. Home's chrome glass ends at y=130 and the focused card's top edge is at y=211: 130 + 68
/// is 198 (clear), 130 + 88 is 218 (inside).
#[test]
fn a_draw_in_the_rise_margin_band_invalidates_only_a_glass_that_can_rise() {
    let _guard = plx_base::testlock::serial();
    let glass = Rect::new(96.0, 54.0, 1130.0, 76.0);
    for rises in [false, true] {
        let sources = Rc::new(RefCell::new(Sources::default()));
        let mut verdicts = Vec::new();
        for (step, (x, y)) in [(300.0, 213.0), (340.0, 213.0), (380.0, 213.0), (380.0, 190.0)]
            .into_iter()
            .enumerate()
        {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = crate::Painter::root();
                p.rect(Rect::new(x, y, 270.0, 400.0), 0.0, [0.3; 4], [0.3; 4], 0.0);
                let drew = if rises {
                    crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop_rising(
                        p, glass, 0.0, 2.0, [1.0; 4], plx_gfx::gfx::GlassRim::Standing,
                        plx_gfx::gfx::GlassFace::NONE, crate::theme::Material::UltraThin)
                } else {
                    crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
                        p, glass, 0.0, 2.0, [1.0; 4], plx_gfx::gfx::GlassRim::Standing,
                        plx_gfx::gfx::GlassFace::NONE, crate::theme::Material::UltraThin)
                };
                assert!(drew);
            }
            sources.borrow_mut().resolve();
            if step > 0 {
                verdicts.push(!sources.borrow().entries[&Z(1)].valid);
            }
            commit(&sources);
        }
        // steps 1,2: the card slides at y=213; step 3: it moves up to y=190, inside the reach
        let want = if rises { vec![true, true, true] } else { vec![false, false, true] };
        assert_eq!(verdicts, want, "rises={rises}");
    }
}

/// A glass in the first surface's band, with a menu's rim and face: what a menu's panel was until
/// it moved onto frozen ground (`ui::popover_glass`). The walk still serves such a glass — a dim
/// it applies at composite — so the tests below keep that
/// machinery honest with the glass stated directly.
fn surface_glass(p: crate::Painter, r: Rect) {
    assert!(crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
        p,
        r,
        0.0,
        24.0,
        [1.0; 4],
        plx_gfx::gfx::GlassRim::Menu,
        crate::popover_glass::FACE,
        crate::theme::PANEL_MATERIAL
    ));
}

/// One frame of a surface glass over a page and its modal dim, the order `dispatch.rs::draw_with` walks
/// them in: the page, the dim (`ModalStack::draw_scrims` → `UnderlayField::draw` as
/// `Role::Dim`) and, when `lift` is set, an opener lift drawn ABOVE the dim under the menu's
/// footprint, then the glass in its surface band. Returns whether the menu's source
/// owes a capture this frame (a job), and marks it captured, as a successful GPU job does.
fn menu_frame(
    sources: &std::rc::Rc<std::cell::RefCell<crate::frame::backdrop::Sources>>,
    field: &crate::underlay::UnderlayField,
    alpha: f32,
    lift: bool,
) -> bool {
    use crate::frame::backdrop::{commit, discover, layer, Z};
    let r = Rect::new(1200.0, 220.0, 320.0, 600.0);
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        p.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, [0.2, 0.3, 0.4, 1.0], [0.2, 0.3, 0.4, 1.0], 0.0);
        {
            let _dim = layer(Z::DIM, false);
            field.draw(p, crate::Rect::FULL, crate::underlay::Role::Dim { weight: crate::theme::underlay::TINT }, alpha);
            if lift {
                p.rect(Rect::new(1180.0, 200.0, 80.0, 80.0), 8.0, [0.9; 4], [0.9; 4], 0.0);
            }
        }
        let _surface = layer(Z::surface(0), false);
        surface_glass(p, r);
    }
    let owed = {
        let mut s = sources.borrow_mut();
        s.resolve();
        s.jobs().iter().any(|&(z, _)| z >= Z::surface(0))
    };
    commit(sources);
    owed
}

/// **A menu's dim ramping in front of a still page re-blurs nothing.** The dim is linear in its
/// alpha (`mix(page, ink, a)`) and its ink is the page's own smooth light, so the menu's glass
/// filters the page ONCE and applies this frame's dim at composite — the PageDip precedent
/// (`Layer::composite_alpha`), with the dim's ink as the ground. Red on the build that keyed
/// the source on the dim's alpha: every frame of an appear (and of a close) re-ran the whole
/// direct source pass, `blur.scene` replaying the page plus both taps — the Sort & Filter and
/// account menus' GLASSDIFF on the television named `field` arg 15 (the tint's alpha) on 18
/// of SFopen's 24 frames.
#[test]
fn a_ramping_dim_under_a_menu_captures_its_source_once() {
    let _g = plx_base::testlock::serial();
    let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
    let flat = crate::underlay::UnderlayField::new();
    let mut composite = Vec::new();
    let owed: Vec<bool> = (0..12)
        .map(|i| {
            let a = 0.025 * i as f32;
            let o = menu_frame(&sources, &flat, a, false);
            let s = sources.borrow();
            let z = *s.entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
            composite.push((a, s.composite_dim(z).map(|d| d.tint[3])));
            o
        })
        .collect();
    assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole ramp: {owed:?}");
    assert!(owed[0], "and it is the first frame's");
    // …and what the composite applies is THIS frame's dim, the scrim ink at this alpha.
    for (a, applied) in composite {
        assert_eq!(applied, Some(a), "the composite takes the frame's own alpha");
    }
    // The band's direct replay skips the dim it applies; the visible walk always draws it.
    let z = *sources.borrow().entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
    {
        let _job = crate::frame::backdrop::enter(sources.clone(), z);
        assert!(crate::frame::backdrop::dim_deferred(), "the job's replay leaves the dim out");
    }
    {
        let _visible = crate::frame::backdrop::enter(sources.clone(), crate::frame::backdrop::Z::ALL);
        assert!(!crate::frame::backdrop::dim_deferred(), "the visible frame draws it");
    }
}

/// **Nor does the dim's FIELD being re-latched** under a standing menu. Every host re-capture
/// re-reads the page's light into the field (`ModalUnderlay::sync` on a new `page_epoch`),
/// which re-specifies its texture; the composite reads the field live, so the source does not
/// have to be re-taken for it. Red on the build that recorded the texture's revision in the
/// dim's arguments: the tab pulse after "Add to Watchlist" re-captured the host on each of its
/// frames, and each one re-ran the menu's source pass (GLASSDIFF `field` arg 10, HWCNT 19.4M a
/// frame for the pulse's 0.55 s against 9.2M after it).
#[test]
fn a_relatched_dim_field_under_a_menu_captures_its_source_once() {
    let _g = plx_base::testlock::serial();
    let tex = 9_731;
    plx_gfx::gfx::tex_ledger::specified(tex, 60, 32);
    let field = crate::underlay::UnderlayField::latched_for_test(tex);
    let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
    let owed: Vec<bool> = (0..8)
        .map(|_| {
            let o = menu_frame(&sources, &field, crate::theme::underlay::DIM_COMPACT, false);
            plx_gfx::gfx::tex_ledger::specified(tex, 60, 32); // the next frame's re-latch
            o
        })
        .collect();
    plx_gfx::gfx::tex_ledger::deleted(tex);
    assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture: {owed:?}");
}

/// **The guard: anything drawn ABOVE the dim under the menu keeps the dim in the source.** A
/// composite-time dim would darken that content too (an opener lift is drawn over the dim so
/// that it does NOT recede), so such a menu keys on the dim's real arguments, as before, and
/// re-captures when its alpha moves.
#[test]
fn content_above_the_dim_keeps_the_dim_in_the_menus_source() {
    let _g = plx_base::testlock::serial();
    let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
    let flat = crate::underlay::UnderlayField::new();
    let owed: Vec<bool> = (0..6).map(|i| menu_frame(&sources, &flat, 0.05 * i as f32, true)).collect();
    assert_eq!(owed, vec![true; 6], "every alpha step is a new source");
    let s = sources.borrow();
    let z = *s.entries.keys().find(|&&z| z >= crate::frame::backdrop::Z::surface(0)).unwrap();
    assert_eq!(s.composite_dim(z), None, "and the composite applies no second dim");
}

// ---- The page wash under the chrome band --------------------------------------------------

/// A wash `k` (tl, tr, br, bl) whose top corners sit `step` codes along an ease. The bottom corners
/// stay put, as the Library's do (`PageGround::CARD_W` leans the wash at the top).
fn wash_at(step: f32) -> [[f32; 3]; 4] {
    let c = |code: f32| code / 255.0;
    [
        [c(39.0 + step), c(35.0 + step), c(33.0 + step)],
        [c(38.0 + step), c(36.0 + step), c(40.0 + step)],
        [c(30.0), c(30.0), c(31.0)],
        [c(29.0), c(30.0), c(30.0)],
    ]
}

/// One discovered frame of a page under the chrome band, in the order `dispatch.rs::draw_with`
/// walks it: **the frame clear** (every page opens with `gfx::frame_clear`, which a discovery walk
/// records as a full-canvas draw of its own, the Library's `lb.clear`), the page's ground, a card,
/// then the tab bar's glass in `Z::CHROME`. `moving` is what the update phase reported (a spring in
/// flight). Returns whether the band owes a source job.
///
/// The clear is NOT optional here: this list once began at the wash, which no real page does, and
/// every wash test was green while the feature never ran on a device or in the simulator (the clear
/// was the wash's predecessor in the glass's prefix, so "the wash is the first draw" never held).
fn wash_frame(
    sources: &Rc<RefCell<Sources>>,
    ground: impl Fn(crate::Painter),
    card_x: f32,
    moving: bool,
) -> bool {
    wash_frame_with(sources, ground, card_x, moving, |p| {
        let _chrome = layer(Z::CHROME, true);
        declare_glass(p, Rect::new(28.0, 20.0, 1312.0, 100.0));
    })
}

/// [`wash_frame`] with the glass left to the caller, declared after the page's own draws.
fn wash_frame_with(
    sources: &Rc<RefCell<Sources>>,
    ground: impl Fn(crate::Painter),
    card_x: f32,
    moving: bool,
    glass: impl Fn(crate::Painter),
) -> bool {
    plx_machine::idle::frame_begin(1.0 / 60.0);
    if moving {
        plx_machine::idle::note_motion();
    }
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        let p = crate::Painter::root();
        plx_gfx::gfx::frame_clear(crate::theme::CLEAR_RGB.0, crate::theme::CLEAR_RGB.1, crate::theme::CLEAR_RGB.2);
        ground(p);
        p.rect(Rect::new(card_x, 60.0, 200.0, 100.0), 8.0, [0.5; 4], [0.5; 4], 0.0);
        glass(p);
    }
    let owed = {
        let mut s = sources.borrow_mut();
        s.resolve();
        !s.jobs().is_empty()
    };
    commit(sources);
    owed
}

/// A hand-fed wash whose springs are still easing (`resting` false): the frames of an ease.
fn plain_wash(step: f32) -> impl Fn(crate::Painter) {
    move |p| p.ambient(Rect::FULL, 1.0, wash_at(step), false)
}

/// The same wash with every spring landed (`resting` true), as `AmbientWash::draw` declares it
/// from the frame its ease finishes.
fn resting_wash(step: f32) -> impl Fn(crate::Painter) {
    move |p| p.ambient(Rect::FULL, 1.0, wash_at(step), true)
}

/// **A wash easing under the chrome re-sources nothing.** `AmbientWash` dissolves each corner
/// channel on its own spring, so a library switch (or a focus move onto art-less content) moves the
/// wash's exact 8-bit codes by 2-6 EVERY frame; the ambient draw is full-screen, so it sits in the
/// prefix of every live glass, and the exact-argument compare re-sourced the tab bar's band on
/// each of those frames (GLASSDIFF `kind=ambient`, 77-83% of a switch's refreshing frames, ~0.8M
/// Mali cycles and 2.7-7.1 ms of render thread each). Nothing else changes here.
#[test]
fn a_ramping_wash_under_the_chrome_captures_its_source_once() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let steps = [0.0, 2.0, 5.0, 9.0, 13.0, 18.0, 24.0, 27.0, 30.0, 31.0, 32.0, 33.0];
    let owed: Vec<bool> = steps
        .iter()
        .map(|&s| wash_frame(&sources, plain_wash(s), 300.0, true))
        .collect();
    assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole ease: {owed:?}");
    assert!(owed[0], "and it is the activation frame's");
}

/// **The settle frame refreshes once**, so the picture that stays on the panel is exact. The idle
/// gate presents the first still frame after motion (`idle::should_present`), and that is the frame
/// the band re-takes its source on: the wash it holds is then the one the page rests on.
#[test]
fn the_settle_frame_refreshes_once() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let mut owed = Vec::new();
    for s in [0.0, 3.0, 8.0, 14.0, 21.0, 25.0] {
        owed.push(wash_frame(&sources, plain_wash(s), 300.0, true));
    }
    for _ in 0..4 {
        owed.push(wash_frame(&sources, resting_wash(25.0), 300.0, false));
    }
    let frames: Vec<usize> = owed.iter().enumerate().filter(|(_, &o)| o).map(|(i, _)| i).collect();
    assert_eq!(frames, vec![0, 6], "activation, then the first still frame: {owed:?}");
}

/// **A code the tail holds for a frame is not rest.** A critically damped spring's last stretch
/// changes its 8-bit code less than once a frame, so the drawn wash repeats for a frame or two
/// while the springs behind it still move. The wash says so itself (`resting` false), and the
/// source is re-taken once, on the frame it says it has landed.
#[test]
fn a_code_the_tail_holds_for_a_frame_is_not_rest() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let mut owed = Vec::new();
    for s in [0.0, 3.0, 8.0, 14.0, 21.0, 21.0, 24.0, 25.0, 25.0, 26.0] {
        owed.push(wash_frame(&sources, plain_wash(s), 300.0, true));
    }
    for _ in 0..4 {
        owed.push(wash_frame(&sources, resting_wash(26.0), 300.0, false));
    }
    let frames: Vec<usize> = owed.iter().enumerate().filter(|(_, &o)| o).map(|(i, _)| i).collect();
    assert_eq!(frames, vec![0, 10], "activation, then the frame the wash landed: {owed:?}");
}

/// **The wash's rest is its own**: a card, a spinner or a scroll elsewhere on the page can keep the
/// page moving for as long as the wash has been still, and the picture that stays on the panel is
/// still owed the wash it rests on.
#[test]
fn a_wash_at_rest_is_re_taken_while_something_else_still_moves() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let mut owed: Vec<bool> = [0.0, 4.0, 9.0, 15.0]
        .iter()
        .map(|&s| wash_frame(&sources, plain_wash(s), 300.0, true))
        .collect();
    owed.extend((0..4).map(|_| wash_frame(&sources, resting_wash(18.0), 300.0, true)));
    let frames: Vec<usize> = owed.iter().enumerate().filter(|(_, &o)| o).map(|(i, _)| i).collect();
    assert_eq!(frames, vec![0, 4], "activation, then the wash's own rest: {owed:?}");
}

/// **A page dip is not rest.** `ui::nav` fades a page toward the app ground through the painter's
/// alpha, which mixes the wash's corners toward `SURFACE_APP` and moves the drawn corners every
/// frame with every spring at rest. Rest means the drawn corners have stopped, so the cascade must
/// be the identity for it to count; the glass keeps taking the dip as a composite term.
#[test]
fn a_wash_at_rest_under_a_page_dip_owes_nothing() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let dip = |a: f32| move |p: crate::Painter| p.alpha(a).ambient(Rect::FULL, 1.0, wash_at(10.0), true);
    let owed: Vec<bool> = [1.0, 0.9, 0.8, 0.7, 0.6, 0.5].iter().map(|&a| wash_frame(&sources, dip(a), 300.0, true)).collect();
    assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole dip: {owed:?}");
}

/// **A real ease re-takes the source once, at the wash's own rest, however its tail holds a code.**
/// `AmbientWash` is twelve critically damped springs, and the codes it draws are those springs
/// rounded to 8 bits: in the tail one channel moves less than a code a frame, so whole frames draw
/// the previous frame's codes while the ease is still running (the simulator's library switch drew
/// 7-8 such frames per ease). A held code is not rest; the wash's own springs are. This steps a
/// REAL wash through two eases (the first from the flat app ground) with the page's update/draw
/// order and holds the owed frames to: the activation frame, then ONE frame per ease, the first on
/// which the wash itself says it has landed (`AmbientWash::at_rest`). It proves its own premise by
/// counting the held codes, and keeps stepping long after the rest so a code that flipped late
/// would show.
#[test]
fn a_real_ease_re_takes_the_source_once_at_the_washes_own_rest() {
    use crate::widgets::AmbientWash;
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let wash = RefCell::new(AmbientWash::flat(crate::theme::SURFACE_APP));
    let drawn = RefCell::new(Vec::<[[f32; 3]; 4]>::new());
    let moving = RefCell::new(Vec::<bool>::new());
    let eases = [
        AmbientWash::keyed([[0.9, 0.4, 0.2], [0.2, 0.5, 0.9], [0.3, 0.8, 0.3], [0.7, 0.2, 0.6]], [AmbientWash::GROUND_W; 4]),
        AmbientWash::keyed([[0.1, 0.7, 0.5], [0.8, 0.2, 0.3], [0.5, 0.5, 0.9], [0.9, 0.9, 0.2]], [AmbientWash::GROUND_W; 4]),
    ];
    let frame = |target: Option<[[f32; 4]; 4]>| {
        wash_frame(
            &sources,
            |p| {
                let mut w = wash.borrow_mut();
                if let Some(t) = target {
                    w.step(t, AmbientWash::K, 1.0 / 60.0);
                }
                moving.borrow_mut().push(!w.at_rest());
                drawn.borrow_mut().push(w.drawn_corners());
                w.draw(p, Rect::FULL);
            },
            300.0,
            false,
        )
    };
    let mut owed = vec![frame(None)];
    let mut ease_at = Vec::new();
    for target in eases {
        ease_at.push(owed.len());
        for _ in 0..120 {
            owed.push(frame(Some(target)));
        }
    }
    let (moving, drawn) = (moving.into_inner(), drawn.into_inner());
    let mut expected = vec![0];
    let mut held = 0;
    for (n, &start) in ease_at.iter().enumerate() {
        let end = ease_at.get(n + 1).copied().unwrap_or(owed.len());
        let rest = (start..end).find(|&i| !moving[i]).expect("the ease comes to rest");
        assert!(rest > start + 10, "ease {n} ran for a while before it rested: {rest}");
        assert!((rest..end).all(|i| !moving[i]), "and stayed at rest");
        held += (start + 1..rest).filter(|&i| drawn[i] == drawn[i - 1]).count();
        expected.push(rest);
    }
    assert!(held >= 2, "the premise: the tails hold a drawn code while still moving ({held} frames)");
    let frames: Vec<usize> = owed.iter().enumerate().filter(|(_, &o)| o).map(|(i, _)| i).collect();
    assert_eq!(frames, expected, "activation, then one re-take per ease at its rest: {owed:?}");
}

/// **The composite's wash term makes the wash the source holds the wash that is being drawn**, at
/// every point the glass can sample: the source keeps frame 0's wash and the term owed on frame `n`
/// is `drawn(n) - drawn(0)` there. The points are the whole chrome band and the margin the lens can
/// push a sample out to (the shader reads `srcRGB` at the REFRACTED uv, up to the 38 px bevel off
/// the fragment), clipped to the canvas.
///
/// The tolerance is the algebra's own: both sides are the same bilinear field, so the residue is
/// f32 rounding, `1e-5` (a thousandth of an 8-bit code). What the GPU adds on top is bounded by
/// construction, not measured here: the field mesh interpolates a bilinear wash linearly over 16x16
/// cells, which twists it by at most `|tl - tr + br - bl| / 1024` (under half a code for any 8-bit
/// corners), and `fs_glass.frag` evaluates the term in fp16 (`2^-11` relative to values under 0.25).
#[test]
fn the_composite_wash_term_is_each_frames_drawn_wash_less_the_captured_one_at_the_refracted_uv() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let bilinear = |k: [[f32; 3]; 4], x: f32, y: f32| -> [f32; 3] {
        let (u, v) = (x / 1920.0, y / 1080.0);
        std::array::from_fn(|i| {
            let top = k[0][i] + (k[1][i] - k[0][i]) * u;
            let bot = k[3][i] + (k[2][i] - k[3][i]) * u;
            top + (bot - top) * v
        })
    };
    assert!(wash_frame(&sources, plain_wash(0.0), 300.0, true), "frame 0 takes the source");
    let baked = wash_at(0.0);
    let (mut step, mut worst, mut largest) = (0.0f32, 0.0f32, 0.0f32);
    for frame in 1..=12 {
        step += 2.0 + (frame % 5) as f32; // 3, 4, 5, 6, 2, 3 ... codes: the ease's 2-6 per frame
        assert!(!wash_frame(&sources, plain_wash(step), 300.0, true), "frame {frame} owes no source");
        let term = sources
            .borrow()
            .composite_ground(Z::CHROME)
            .unwrap_or_else(|| panic!("frame {frame}: the band owes a composite term"));
        for y in (0..=190).step_by(10) {
            for x in (0..=1920).step_by(60) {
                let (x, y) = (x as f32, y as f32);
                let (was, now, add) = (bilinear(baked, x, y), bilinear(wash_at(step), x, y), term.at(x, y));
                for i in 0..3 {
                    worst = worst.max((was[i] + add[i] - now[i]).abs());
                    largest = largest.max(add[i].abs());
                }
            }
        }
    }
    assert!(worst < 1e-5, "baked + term is the drawn wash to the algebra's rounding: {worst}");
    assert!(largest > 40.0 / 255.0, "and the term is not vacuously small: {largest}");
}

/// **A wash that has not moved owes no term**, and one under an ink ramp is let through by what the
/// ramp lets through: `weight = 1 - alpha` at the rect's top and bottom edge, linear between.
#[test]
fn the_wash_term_follows_the_ink_ramp_laid_over_the_wash() {
    use crate::frame::backdrop::Ground;
    // Every corner moved by `step` codes, so the difference is uniform and the weight alone shapes it.
    let g = |step: f32, ink_a: [f32; 2]| Ground {
        corners: wash_at(0.0).map(|c| c.map(|v| v + step / 255.0)),
        rect: [0.0, 100.0, 1920.0, 400.0],
        ink: [0.0, 0.0, 0.0],
        ink_a,
    };
    assert!(g(0.0, [0.0; 2]).delta(&g(0.0, [0.0; 2])).is_none(), "same wash, no term");
    assert!(g(3.0, [0.25; 2]).delta(&g(0.0, [0.0; 2])).is_none(), "another ink is another source");
    let t = g(10.0, [0.5, 0.0]).delta(&g(0.0, [0.5, 0.0])).expect("a moved wash owes a term");
    let top = t.at(960.0, 100.0)[0];
    let bottom = t.at(960.0, 500.0)[0];
    let moved = 10.0 / 255.0;
    assert!((top - 0.5 * moved).abs() < 1e-6, "half let through at the top: {top}");
    assert!((bottom - moved).abs() < 1e-6, "all of it at the foot: {bottom}");
    assert_eq!(t.at(960.0, -400.0), t.at(960.0, 100.0), "clamped above the rect");
}

/// **The guard: the wash stepping while a card moves under the band owes every frame.** The delta
/// at composite is the wash's alone; a card under the region changes the filtered picture itself.
#[test]
fn a_wash_easing_with_a_card_moving_under_the_chrome_owes_every_frame() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let owed: Vec<bool> = (0..12)
        .map(|i| wash_frame(&sources, plain_wash(3.0 * i as f32), 300.0 + 8.0 * i as f32, true))
        .collect();
    assert_eq!(owed, vec![true; 12]);
}

/// **An INKED wash strip is deferred too, and its ramp keys the source.** Home (and Detail) lay their
/// atmospheric ramp over the wash in the same pass (`ambient_inked`, cut at the ramp's knees): the
/// strip over the chrome band is a smooth field like the plain wash, so an ease under it re-sources
/// nothing; the ramp's own alphas are the SOURCE's (a hero fading in is real change).
#[test]
fn an_inked_wash_strip_is_deferred_and_its_ramp_keys_the_source() {
    let _g = plx_base::testlock::serial();
    let strips = |step: f32, foot: f32| {
        move |p: crate::Painter| {
            let ink = [0.02, 0.02, 0.03, 1.0];
            p.ambient_inked(Rect::new(0.0, 0.0, 1920.0, 400.0), wash_at(step), (ink, [0.0, foot]), false);
            p.ambient_inked(Rect::new(0.0, 400.0, 1920.0, 680.0), wash_at(step), (ink, [foot, 0.6]), false);
        }
    };
    let sources = Rc::new(RefCell::new(Sources::default()));
    let eased: Vec<bool> = (0..12).map(|i| wash_frame(&sources, strips(3.0 * i as f32, 0.2), 300.0, true)).collect();
    assert_eq!(eased.iter().filter(|&&o| o).count(), 1, "one capture for the whole ease: {eased:?}");
    assert!(wash_frame(&sources, strips(36.0, 0.3), 300.0, true), "a ramp that moved is a new source");
}

/// **The hold hint's live glass takes the wash at composite too.** It stands on the page band, at
/// the foot of the page, over the same ground; its region is the whole footprint of its rise.
#[test]
fn a_ramping_wash_under_the_hold_hint_captures_its_source_once() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let hint = |p: crate::Painter| {
        assert!(crate::widgets::Glass::DYNAMIC_BACKDROP.backdrop_rising(
            p,
            Rect::new(720.0, 940.0, 480.0, 72.0),
            0.0,
            36.0,
            [1.0; 4],
            plx_gfx::gfx::GlassRim::Standing,
            plx_gfx::gfx::GlassFace::NONE,
            crate::theme::Material::UltraThin,
        ));
    };
    let owed: Vec<bool> = (0..12)
        .map(|i| wash_frame_with(&sources, plain_wash(3.0 * i as f32), 300.0, true, &hint))
        .collect();
    assert_eq!(owed.iter().filter(|&&o| o).count(), 1, "one capture for the whole ease: {owed:?}");
}

/// **A ground with art in it is never deferred.** `ambient_art` (tag 22, which `tex_carded_still`
/// shares) paints a photograph into the wash: not a smooth field, and its picture is the source.
#[test]
fn an_art_ground_under_the_chrome_is_never_deferred() {
    let _g = plx_base::testlock::serial();
    let tex = 9_447;
    plx_gfx::gfx::tex_ledger::specified(tex, 720, 480);
    let sources = Rc::new(RefCell::new(Sources::default()));
    let art = |step: f32| {
        move |p: crate::Painter| {
            p.ambient_art(
                Rect::FULL,
                wash_at(step),
                tex,
                Rect::new(0.0, 0.0, 1920.0, 800.0),
                plx_gfx::gfx::UV_FULL,
                [1.0; 4],
                ([0.0; 4], [0.0; 2]),
            )
        }
    };
    let owed: Vec<bool> = (0..8).map(|i| wash_frame(&sources, art(3.0 * i as f32), 300.0, true)).collect();
    plx_gfx::gfx::tex_ledger::deleted(tex);
    assert_eq!(owed, vec![true; 8], "the art ground keeps the wash in the source");
}

/// **Two grounds under the region keep the wash in the source.** A ramp knee inside the glass's
/// footprint cuts the wash into two bands (`AmbientWash::draw_ground`); the delta is one wash's.
#[test]
fn two_grounds_under_the_region_keep_the_wash_in_the_source() {
    let _g = plx_base::testlock::serial();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let bands = |step: f32| {
        move |p: crate::Painter| {
            p.ambient(Rect::new(0.0, 0.0, 1920.0, 100.0), 1.0, wash_at(step), false);
            p.ambient(Rect::new(0.0, 100.0, 1920.0, 980.0), 1.0, wash_at(step), false);
        }
    };
    let owed: Vec<bool> = (0..8).map(|i| wash_frame(&sources, bands(3.0 * i as f32), 300.0, true)).collect();
    assert_eq!(owed, vec![true; 8]);
}
