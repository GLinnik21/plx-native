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
