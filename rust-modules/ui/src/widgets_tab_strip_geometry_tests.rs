//! Top tab bar / season strip pill geometry, label measuring + caching, document rail, and the profile chip's hit test and viewport scrolling.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn keyed_tab_geometry_uses_the_same_centered_lattice_for_draw_and_target() {
    use super::*;
    let widths = [140.0, 220.0, TAB_ICON_PILL_W];
    let keys = [7, 2, 9];
    let mut members = Vec::new();
    tab_members_at(&widths, &keys, 17.0, 43.0, &mut members);
    assert_eq!(members.iter().map(|m| m.elem).collect::<Vec<_>>(), keys);
    let content = widths.iter().sum::<f32>() + 2.0 * TAB_GAP;
    let left = (crate::consts::SCR_W - content) * 0.5;
    assert_eq!(members[0].drawn.x, left - 17.0);
    assert_eq!(members[0].target.x, left - 43.0);
    for (i, member) in members.iter().enumerate() {
        assert_eq!(member.drawn.w, widths[i]);
        assert_eq!(member.target.w, widths[i]);
        assert_eq!(member.clip.x, left);
        assert_eq!(member.clip.w, content);
        assert_eq!(member.drawn.x - member.target.x, 26.0);
    }
    assert_eq!(members[1].drawn.x - members[0].drawn.x, widths[0] + TAB_GAP);
}

/// Paint and container input consume the SAME geometry object. The clipped visible rect is
/// derived from the member's drawn rect and clip, never recorded into a parallel pill model.
#[test]
fn tab_geometry_is_shared_by_paint_members_and_clipping() {
    let widths = [900.0, 700.0, TAB_ICON_PILL_W];
    let keys = [7, 2, 9];
    let scroll = 411.0;
    let geometry = tab_geometry(&widths, scroll);
    let mut members = Vec::new();
    tab_members_at(&widths, &keys, scroll, scroll, &mut members);
    assert_eq!(widths.len(), members.len());
    let same = |a: Rect, b: Rect| (a.x, a.y, a.w, a.h) == (b.x, b.y, b.w, b.h);
    for (i, member) in members.iter().enumerate() {
        let pill = geometry.pill(i);
        assert!(same(pill, member.drawn));
        assert!(same(geometry.clip, member.clip));
        assert!(same(pill.intersect(geometry.clip), member.drawn.intersect(member.clip)));
    }
    assert_eq!(geometry.track.w, geometry.clip.w + 2.0 * TAB_TRACK_PAD);
}

#[test]
fn captured_tab_labels_measure_words_and_keep_the_search_mark_square() {
    use super::*;
    let faces = vec![TabFace::Label("Home".into()), TabFace::Label("Фильмы".into()),
        TabFace::Icon(crate::icons::Icon::Search)];
    let (words, widths) = tab_metrics_from(&faces, |s| s.to_bytes().len() as f32 * 7.0);
    assert_eq!(words.len(), faces.len());
    assert_eq!(widths[0], 4.0 * 7.0 + 2.0 * TAB_PILL_PAD);
    assert_eq!(widths[1], "Фильмы".len() as f32 * 7.0 + 2.0 * TAB_PILL_PAD);
    assert_eq!(widths[2], TAB_ICON_PILL_W);
    assert_eq!(words[1].to_bytes(), "Фильмы".as_bytes());
}

/// DS5: an icon face is an icon wherever it sits, not only last. Two icon faces after two labels
/// (Home, Movies, Bookmark, Search) both get the square pill, and the strip does not scroll.
/// Red against a `i + 1 == len` rule: the Bookmark would be measured as an empty label.
#[test]
fn an_icon_face_is_square_wherever_it_sits_in_the_strip() {
    let faces = vec![TabFace::Label("Home".into()), TabFace::Label("Movies".into()),
        TabFace::Icon(crate::icons::Icon::Bookmark), TabFace::Icon(crate::icons::Icon::Search)];
    let (words, widths) = tab_metrics_from(&faces, |s| s.to_bytes().len() as f32 * 7.0);
    assert_eq!(widths[2], TAB_ICON_PILL_W);
    assert_eq!(widths[3], TAB_ICON_PILL_W);
    assert_ne!(widths[2], 2.0 * TAB_PILL_PAD, "an icon is not an empty label");
    assert!(words[2].to_bytes().is_empty() && words[3].to_bytes().is_empty());
    assert_eq!(widths[0], 4.0 * 7.0 + 2.0 * TAB_PILL_PAD);
    assert!(tab_content_w(&widths) <= TAB_VIEW_MAX, "four stops fit");
    for i in 0..widths.len() {
        for &start in &[0.0f32, 400.0, 9000.0] {
            assert_eq!(tab_scroll_target(&widths, i, start), 0.0, "stop {i} must not scroll");
        }
    }
}

#[test]
fn tab_metrics_cache_rejects_different_captured_labels_even_at_the_same_generation() {
    use super::*;
    let faces = vec![TabFace::Label("Home".into()), TabFace::Label("Movies".into()),
        TabFace::Icon(crate::icons::Icon::Search)];
    let (words, widths) = tab_metrics_from(&faces, |_| 42.0);
    let cache = (7, faces.clone(), words, widths);
    assert!(tab_cache_matches(&cache, TabLabels { generation: 7, faces: &faces }));
    assert!(!tab_cache_matches(&cache, TabLabels { generation: 8, faces: &faces }));
    let changed = vec![TabFace::Label("Home".into()), TabFace::Label("TV Shows".into()),
        TabFace::Icon(crate::icons::Icon::Search)];
    assert!(!tab_cache_matches(&cache, TabLabels { generation: 7, faces: &changed }));
    assert!(!tab_cache_matches(&cache, TabLabels { generation: 7, faces: &faces[..2] }));
    let mark_as_word = vec![TabFace::Label("Home".into()), TabFace::Label("Movies".into()),
        TabFace::Label(String::new())];
    assert!(!tab_cache_matches(&cache, TabLabels { generation: 7, faces: &mark_as_word }),
        "an empty word is not the Search mark: the face kind is part of the captured vocabulary");
}

#[test]
fn measured_strip_reuses_shared_padding_gap_and_source_indices() {
    let measure = crate::fixture::FixtureMeasure;
    let size = theme::size::BODY;
    let gap = 37.0;
    let lays = strip_layout_measured(
        ["A".into(), "bad\0label".into(), "Longer".into()].into_iter(),
        96.0, size, gap, &measure,
    );
    assert_eq!(lays.len(), 2);
    assert_eq!((lays[0].i, lays[1].i), (0, 2));
    assert_eq!(lays[0].x, 96.0);
    for lay in &lays {
        assert_eq!(lay.w, plx_machine::machine::Measure::width(&measure, &lay.label, size, true));
    }
    let first = strip_pill_rect(&lays[0], 10.0, 60.0);
    let second = strip_pill_rect(&lays[1], 10.0, 60.0);
    assert_eq!(second.x, first.x + first.w + gap);
}

#[test]
fn continuous_document_rail_tracks_both_ends_and_disappears_when_content_fits() {
    assert_eq!(
        continuous_rail_geom(0.0, 900.0, 300.0),
        Some((0.0, 1.0 / 3.0))
    );
    let (top, h) = continuous_rail_geom(600.0, 900.0, 300.0).unwrap();
    assert!((top + h - 1.0).abs() < 0.0001);
    assert_eq!(continuous_rail_geom(0.0, 300.0, 300.0), None);
}

/// **The chip's frame and its hit test are one rect, and it really does sit LEFT of the pills.**
///
/// Both halves matter and neither is visible in a diff. The hit test is what makes the chip
/// clickable on all three screens that draw it (it was Home's alone, recorded at Home's draw),
/// so it must address exactly the rect the draw uses. And the D-pad rule every one of those
/// screens now adopts — ◀ off the FIRST pill reaches the chip, ▶ walks back — is only sane
/// while the chip is genuinely the bar's leftmost thing: [`TAB_SIDE_CLEAR`] is the margin that
/// keeps the CENTRED strip off it, and a strip that grew far enough left to overlap would make
/// the walk cross two controls occupying one place.
#[test]
fn the_profile_chip_is_hit_where_it_is_drawn_and_sits_left_of_the_pills() {
    let r = CHIP_FRAME;
    assert!(
        profile_chip_at(r.cx(), r.cy()),
        "the middle of the avatar is the chip"
    );
    assert!(
        !profile_chip_at(r.x - 1.0, r.cy()),
        "…and a pixel outside it is not"
    );
    assert!(!profile_chip_at(r.cx(), r.y + r.h + 1.0), "…on either axis");
    // it shares the bar's line with the pills: one control height, one top edge
    assert_eq!(r.y, TOP_BAR_Y);
    // one control height with the pills, which is also what makes the focused chip's capsule
    // the tab track's own band — the two read as one strip of chrome rather than as two
    // objects at different heights (see `CHIP_D`)
    assert_eq!(r.h, TAB_PILL_H);

    // the widest strip the row will ever lay out, centred: its left edge must clear the chip
    for n in 1..=16usize {
        let w = widths_for(n);
        let track_l = (crate::consts::SCR_W - tab_view_w(&w)) * 0.5 - TAB_TRACK_PAD;
        assert!(
            track_l > r.x + r.w,
            "n={n}: the tab track reaches x={track_l}, over a chip ending at {}",
            r.x + r.w
        );
    }
}

/// The unit-10 invariant, stated the way the [`tab_count`] doc states it: EVERY pill can be
/// drawn, because the strip scrolls to it. For any section count, and from any starting
/// scroll, the target for pill `i` puts that whole pill inside the visible strip — so no
/// focusable index can lack a drawn rect, which is what the old `MAX_TABS` cap used to buy
/// by refusing to focus the pills it could not reach.
#[test]
fn every_pill_scrolls_fully_into_the_strips_viewport() {
    for n in 1..=16usize {
        let w = widths_for(n);
        let view_w = tab_view_w(&w);
        let max = (tab_content_w(&w) - view_w).max(0.0);
        for &start in &[0.0f32, 400.0, -900.0, 9000.0] {
            for i in 0..n {
                let t = tab_scroll_target(&w, i, start);
                assert!(
                    t >= -0.01 && t <= max + 0.01,
                    "n={n} i={i}: scroll {t} outside 0..={max}"
                );
                let x = tab_pill_x(&w, i) - t; // the pill's left edge in viewport space
                assert!(
                    x >= -0.01 && x + w[i] <= view_w + 0.01,
                    "n={n} i={i} (start {start}): pill spans {x}..{} of a {view_w}-wide strip",
                    x + w[i]
                );
            }
        }
    }
}

/// A row that fits must keep its centered, unscrolled tvOS look — the track IS the content and
/// there is nowhere to scroll to. Past that the scroll is minimal, not eager: asking for a
/// pill that is already on screen must leave the strip exactly where it is, wherever that is.
#[test]
fn the_strip_scrolls_only_once_it_outgrows_the_row_and_only_as_far_as_it_must() {
    let fits = widths_for(4);
    assert!(
        tab_content_w(&fits) <= TAB_VIEW_MAX,
        "4 pills of this size must still fit"
    );
    assert_eq!(
        tab_view_w(&fits),
        tab_content_w(&fits),
        "the track is the content"
    );
    assert_eq!(
        tab_content_w(&fits) - tab_view_w(&fits),
        0.0,
        "…so there is no scroll range"
    );

    let over = widths_for(12);
    assert!(
        tab_content_w(&over) > TAB_VIEW_MAX,
        "12 pills of this size must overflow"
    );
    assert_eq!(
        tab_view_w(&over),
        TAB_VIEW_MAX,
        "the track caps at the viewport"
    );
    assert!(
        tab_scroll_target(&over, 11, 0.0) > 0.0,
        "the last pill must be scrolled to"
    );
    assert_eq!(
        tab_scroll_target(&over, 0, 0.0),
        0.0,
        "the first pill is already at the start"
    );
    assert_eq!(
        tab_scroll_target(&over, 1, 0.0),
        0.0,
        "a pill in view does not move the strip"
    );
    // and the same rule part-way along: pill 5 sits inside the viewport at scroll 800, so the
    // strip HOLDS there rather than re-centering on it
    assert_eq!(
        tab_scroll_target(&over, 5, 800.0),
        800.0,
        "a mid-strip pill in view holds"
    );
}

/// A focus index left over from a bigger section table (the table is refetched on sign-in or
/// a server switch) must clamp, not index out of bounds — the strip is drawn from these same
/// widths every frame, so a panic here would be a panic in the frame loop. Graded on an
/// OVERFLOWING row so the clamp has a non-zero range to be wrong about.
#[test]
fn a_stale_pill_index_clamps_instead_of_panicking() {
    let w = widths_for(12);
    let max = tab_content_w(&w) - tab_view_w(&w);
    assert!(
        max > 0.0,
        "the fixture must actually have somewhere to scroll"
    );
    assert_eq!(
        tab_scroll_target(&w, 99, 500.0),
        500.0,
        "an in-range scroll is left alone"
    );
    assert_eq!(
        tab_scroll_target(&w, 99, 9e3),
        max,
        "an over-scroll is pulled back to the end"
    );
    assert_eq!(
        tab_scroll_target(&w, 99, -5.0),
        0.0,
        "a negative scroll is pulled to the start"
    );
    assert_eq!(
        tab_pill_x(&w, 99),
        tab_pill_x(&w, 12),
        "past the end reads as the strip's end"
    );
    assert_eq!(
        tab_content_w(&[]),
        0.0,
        "an empty strip has no width and no gaps"
    );
    assert_eq!(tab_scroll_target(&[], 0, 12.0), 0.0);
}

/// The tab pulse rises from rest to its peak and comes back to exactly rest; it ends on its own.
#[test]
fn the_tab_pulse_envelope_is_zero_at_both_ends_and_one_at_the_peak() {
    assert_eq!(tab_pulse_envelope(0.0), 0.0);
    assert!((tab_pulse_envelope(TAB_PULSE_S * 0.5) - 1.0).abs() < 1e-6, "the peak");
    assert_eq!(tab_pulse_envelope(TAB_PULSE_S), 0.0, "settled");
    assert_eq!(tab_pulse_envelope(-1.0), 0.0);
    assert_eq!(tab_pulse_envelope(TAB_PULSE_S * 3.0), 0.0);
    assert!(tab_pulse_envelope(TAB_PULSE_S * 0.25) < tab_pulse_envelope(TAB_PULSE_S * 0.5), "rising");
}

/// A pulse runs for [`TAB_PULSE_S`] and is then gone, and while it runs the page is never held for
/// it: it is decorative, so it adds no layout motion.
#[test]
fn a_tab_pulse_runs_its_beat_then_is_gone_without_holding_the_page() {
    let faces = [TabFace::Label("Home".into()), TabFace::Icon(crate::icons::Icon::Bookmark)];
    let labels = TabLabels { generation: 1, faces: &faces };
    let mut strip = StripRender::new();
    // The strip's own springs settle first, so what is measured below is the pulse alone.
    for _ in 0..600 {
        strip.update(labels, 0, TopFocus::Away, 1.0 / 60.0);
    }
    assert_eq!(strip.pulse_read(), None);
    strip.pulse(1);
    assert_eq!(strip.pulse_read(), Some((1, 0.0)));
    plx_machine::idle::reset_for_test();
    strip.update(labels, 0, TopFocus::Away, TAB_PULSE_S * 0.5);
    let (at, env) = strip.pulse_read().expect("still running at the peak");
    assert_eq!(at, 1);
    assert!(env > 0.99, "peak envelope {env}");
    assert!(!plx_machine::idle::page_layout_moving(), "a pulse is not layout motion");
    strip.update(labels, 0, TopFocus::Away, TAB_PULSE_S);
    assert_eq!(strip.pulse_read(), None, "ended on its own");
}

/// **A running pulse is MOTION, never page damage.** It is a time-driven decoration on the chrome,
/// and "Add to Watchlist" starts it on the frame the item menu is dismissed, so it runs under a
/// FADING menu. There `popover::host::host_refresh` draws a moving page live, without capturing it
/// (`CAPTURE_POINTLESS`); page DAMAGE instead re-captures the host every frame, which bumps
/// `page_epoch`, which re-latches the modal dim's field (a reduction of the whole page and a
/// readback) on each of the pulse's frames. Red on the build whose pulse called
/// `idle::invalidate`: HWCNT on the television held 19.4M a frame for the pulse's 0.55 s after Add,
/// against 9.2M the frame it ended.
#[test]
fn a_running_tab_pulse_reports_motion_not_page_damage() {
    let faces = [TabFace::Label("Home".into()), TabFace::Icon(crate::icons::Icon::Bookmark)];
    let labels = TabLabels { generation: 1, faces: &faces };
    let mut strip = StripRender::new();
    for _ in 0..600 {
        strip.update(labels, 0, TopFocus::Away, 1.0 / 60.0);
    }
    strip.pulse(1);
    plx_machine::idle::reset_for_test();
    let _ = plx_machine::idle::take_local_damage();
    let mut frames = 0;
    while strip.pulse_read().is_some() {
        plx_machine::idle::frame_begin(1.0 / 60.0);
        strip.update(labels, 0, TopFocus::Away, 1.0 / 60.0);
        if strip.pulse_read().is_none() {
            break;
        }
        frames += 1;
        assert!(plx_machine::idle::present_moving(), "frame {frames}: the pulse keeps the gate awake");
        assert!(plx_machine::idle::page_moving(), "frame {frames}: as the page's own motion");
        assert!(!plx_machine::idle::page_layout_moving(), "frame {frames}: decorative, not layout");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "frame {frames}: and raises no damage");
    }
    // A 0.4 s beat at 60 Hz: 24 frames, less the frame that crosses the end.
    assert!(frames >= 22, "the pulse ran its beat ({frames} frames)");
    // What that buys under the fading menu: the page is redrawn live, and an OPEN menu's frozen
    // host pauses it, as every decoration on a page the user cannot reach is paused.
    assert!(crate::popover::host_refresh(true, false, true));
    assert!(!crate::popover::host_refresh(false, false, true));
}

/// Every draw `f` declares in a backdrop discovery pass, in order.
fn declared(f: impl FnOnce()) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
    use crate::frame::backdrop::{discover, Sources};
    let sources = std::rc::Rc::new(std::cell::RefCell::new(Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = discover(sources.clone());
        f();
    }
    let paints = sources.borrow().paints.clone();
    paints
}

/// **The pulse is the GLYPH, not a plate.** The mark grows about its own centre, 1 → 1.35 → 1,
/// with a brief soft white glow around it, and nothing is filled behind it: a disc there is the
/// strip's selected plate and reads as "Watchlist selected" (the design session's frame dump of
/// Add from the Home hold menu, n372 on). Red on the build that drew a `TAB_PULSE_GLOW_A` accent
/// rrect the pill's size behind the mark.
#[test]
fn the_tab_pulse_grows_the_glyph_with_a_white_glow_and_draws_no_plate() {
    let _g = plx_base::testlock::serial();
    let ir = Rect::new(900.0, 30.0, TAB_ICON_D, TAB_ICON_D);
    let (cx, cy) = (ir.x + ir.w * 0.5, ir.y + ir.h * 0.5);
    // At rest, and at both ends of the beat: the glyph at its own rect and nothing else.
    for env in [0.0, tab_pulse_envelope(0.0), tab_pulse_envelope(TAB_PULSE_S)] {
        let look = tab_pulse_look(ir, env);
        assert_eq!(look.glyph, ir);
        assert!(look.glow.is_none());
    }
    let peak = tab_pulse_look(ir, tab_pulse_envelope(TAB_PULSE_S * 0.5));
    assert!((peak.glyph.w / ir.w - TAB_PULSE_SCALE).abs() < 1e-3, "the glyph itself grows 1.35x: {:?}", peak.glyph);
    assert!((peak.glyph.x + peak.glyph.w * 0.5 - cx).abs() < 1e-3 && (peak.glyph.y + peak.glyph.h * 0.5 - cy).abs() < 1e-3, "about its centre");
    let glow = peak.glow.expect("a glow at the peak");
    assert_eq!(&glow.col[..3], &[1.0, 1.0, 1.0], "white, not the accent");
    // What is declared besides the glyph (`draw_tab_mark` draws the icon after this; a test has no
    // rasteriser for it): the glow is an outside-only penumbra, never a filled shape.
    let paints = declared(|| draw_tab_glow(Painter::root(), tab_pulse_look(ir, 1.0)));
    assert!(
        paints.iter().all(|p| !matches!(p.kind().as_str(), "rect" | "rrect" | "rrect_sheened" | "rect_rimmed")),
        "no plate behind the mark: {:?}",
        paints.iter().map(|p| p.kind()).collect::<Vec<_>>()
    );
    assert!(paints.iter().any(|p| p.kind() == "shadow_outside"), "the glow is drawn");
    assert!(declared(|| draw_tab_glow(Painter::root(), tab_pulse_look(ir, 0.0))).iter().all(|p| p.kind() != "shadow_outside"), "and nothing extra at rest");
}

/// The beat is about 400 ms, up and back.
#[test]
fn the_tab_pulse_is_a_400_ms_beat() {
    assert!((TAB_PULSE_S - 0.4).abs() < 1e-6);
}
