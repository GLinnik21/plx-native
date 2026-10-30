#[test]
fn a_hold_doubles_its_scrub_speed_every_step_up_to_the_cap() {
    use super::{scrub_speed, SCRUB_BASE, SCRUB_MAX, SCRUB_STEP_MS};
    assert_eq!(scrub_speed(0), SCRUB_BASE);
    assert_eq!(scrub_speed(SCRUB_STEP_MS - 1), SCRUB_BASE, "a step holds until its dwell is up");
    assert_eq!(scrub_speed(SCRUB_STEP_MS), 2.0 * SCRUB_BASE);
    assert_eq!(scrub_speed(2 * SCRUB_STEP_MS), 4.0 * SCRUB_BASE);
    assert_eq!(scrub_speed(3 * SCRUB_STEP_MS), SCRUB_MAX);
    assert_eq!(SCRUB_MAX, 8.0 * SCRUB_BASE, "the cap is a whole step of the ladder");
    assert_eq!(scrub_speed(u32::MAX), SCRUB_MAX, "a very long hold stays at the cap");
    let mut last = 0.0;
    for ms in (0..20_000).step_by(100) {
        let v = scrub_speed(ms);
        assert!(v >= last && v <= SCRUB_MAX, "{ms} ms -> {v}");
        last = v;
    }
}

/// #585: a cue of authored line breaks that each wrap again ran past three lines and the tail
/// was silently dropped. Every line of an ordinary long cue must survive.
#[test]
fn a_long_cue_keeps_every_line_past_three() {
    let cue = "First authored line that is long enough to wrap onto a second row\n\
               Second authored line that is long enough to wrap onto a second row\n\
               Third authored line that is long enough to wrap onto a second row";
    let lines = caption_wrap(cue, 1.0, &FM);
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines.last().unwrap().ends_with("second row"), "{lines:?}");
}

#[test]
fn chapter_panel_retains_selection_without_borrowing_hud_focus() {
    assert_eq!(super::classic::hud_tab_state(false, GROUP_TABS, 1, 1), (false, true));
    assert_eq!(super::classic::hud_tab_state(false, GROUP_TABS, 1, 0), (false, false));
    assert_eq!(super::classic::hud_tab_state(true, GROUP_TABS, 1, 1), (true, false));
    assert_eq!(super::classic::hud_tab_state(true, GROUP_SCRUB, 1, 1), (false, false));
    // The same ownership rule covers Info, without conflating it with Chapters.
    assert_eq!(super::classic::hud_tab_state(false, GROUP_TABS, 0, 0), (false, true));
}

use super::*;
use plx_ui::widgets::{StatusKind, StatusOverlay};
use super::classic::*;
use super::geometry::*;
use super::motion::*;

/// The title at the default look (the cell classic and the trailer draw it in), `right` the cell's right edge.
fn draw_player_title(
    p: Painter,
    kicker: Kicker,
    title: *const std::os::raw::c_char,
    right: f32,
    measure: &dyn plx_machine::machine::Measure,
    paused: bool,
    clock: &plx_ui::marquee::Clock,
) {
    draw_player_title_at(p, TitleLook::default(), kicker, title, right, measure, paused, clock);
}

/// **The right edge of the title's cell in the compiled classic** for a frame's slot: where the compile's
/// `title.main` text ends (`w: fill` up to the control row's occupant). The value the hand-written
/// `title_right` reported, frozen in `classic-title-right.golden.tsv`.
fn compiled_title_right(compiled: &crate::hud_format::compile::Compiled, slot: ControlSlot) -> f32 {
    use crate::hud_format::compile::DrawOp;
    let (state, skip) = frame_state(compiled, slot, true, false).expect("classic has a state for every slot");
    state
        .skip_variant(skip)
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Text { bind, rect, .. } if bind == "title.main" => Some(rect.x + rect.w),
            _ => None,
        })
        .expect("the compact states carry the title")
}

#[test]
fn long_hud_titles_fit_before_every_control_slot() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let measure = &ShippedMeasure;
    let long = CString::new("An extraordinarily long episode — Über den Wolken — ".repeat(20)).unwrap();
    let clock = plx_ui::marquee::Clock::new();
    for language in plx_platform::i18n::SHIPPED {
        let _language = plx_platform::i18n::language_on_this_thread_for_test(language);
        let compiled = super::loaded::load_classic("test", plx_platform::i18n::current(), measure).compiled;
        let slots = [
            ControlSlot::Discs(Controls::NONE),
            ControlSlot::Discs(Controls { previous: true, next: true, restart: true }),
            slot_for(Some(marker(MarkerKind::Intro, false)), false, NextEpisodeMode::Countdown),
            slot_for(Some(marker(MarkerKind::Credits, true)), true, NextEpisodeMode::Countdown),
        ];
        for slot in slots {
            let right = compiled_title_right(&compiled, slot);
            let items = geometry(&compiled, slot, true, false).items;
            let mut row = HudMotion::new();
            for focused in 0..slot.items() {
                for _ in 0..30 {
                    row.pop.step(Some(focused as usize), 1.0 / 60.0);
                    for i in 0..slot.items() {
                        let Some(rect) = items[i as usize] else { continue };
                        assert!(right + theme::space::MD <= rect.scaled(row.scale(i)).x + 0.001);
                    }
                    for kicker in [Kicker::Episode(long.as_ptr()), Kicker::Context(long.as_ptr())] {
                        let runs = plx_ui::draw_census::capture_runs(|| {
                            draw_player_title(Painter::recording(), kicker, long.as_ptr(),
                                right, measure, false, &clock);
                        });
                        assert_eq!(runs.len(), 2);
                        for run in runs {
                            assert!(run.text.ends_with('…'), "{}: expected ellipsis: {run:?}", language.tag());
                            assert_eq!(run.at.x, SB_X);
                            assert!(measure.width_str(&run.text, run.size, run.bold) <= right - SB_X + 1.0,
                                "{}: fitted title exceeds the control boundary: {run:?}", language.tag());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn short_hud_titles_keep_the_original_position_typography_and_ink() {
    let clock = plx_ui::marquee::Clock::new();
    for kicker in [Kicker::Episode(c"S1 · E1 · Pilot".as_ptr()), Kicker::Context(c"Trailer".as_ptr())] {
        let original = plx_ui::draw_census::capture_runs(|| {
            draw_title(Painter::recording(), kicker, c"Short title".as_ptr());
        });
        for paused in [false, true] {
            let bounded = plx_ui::draw_census::capture_runs(|| {
                draw_player_title(Painter::recording(), kicker, c"Short title".as_ptr(),
                    compiled_title_right(&super::loaded::classic_for_test(), ControlSlot::Discs(Controls::NONE)), &FM, paused, &clock);
            });
            assert_eq!(original, bounded);
        }
    }
}

#[test]
fn paused_title_reveals_once_and_resume_restores_ellipsis() {
    let clock = plx_ui::marquee::Clock::new();
    let long = c"A very long movie title that should slowly reveal its final words after pausing";
    let right = compiled_title_right(&super::loaded::classic_for_test(), ControlSlot::Discs(Controls::NONE));
    let paint = |paused| plx_ui::draw_census::capture_runs(|| {
        draw_player_title(Painter::recording(), Kicker::Context(c"Movie".as_ptr()),
            long.as_ptr(), right, &FM, paused, &clock);
    });
    let start = paint(true);
    assert!(start[1].text.ends_with('…'));
    plx_machine::idle::frame_begin(2.0);
    let moving = paint(true);
    assert!(moving[1].at.x < SB_X);
    plx_machine::idle::frame_begin(30.0);
    let ended = paint(true);
    assert_eq!(ended[1].at.x, SB_X - (FM.width(long, HUD_TITLE_SZ, true) - (right - SB_X)));
    assert_eq!(paint(true)[1].at.x, ended[1].at.x, "the title must stay at the end");
    assert!(paint(false)[1].text.ends_with('…'), "resume restores a static ellipsis");
    assert!(paint(true)[1].text.ends_with('…'), "a later pause starts from the beginning");
}

/// An Arabic title's end is on its LEFT: the paused reveal moves it right, from its first words
/// on the window's right edge to its last words on the window's left edge, then rests there.
#[test]
fn a_paused_right_to_left_title_reveals_its_end_to_the_left() {
    let _g = plx_base::testlock::serial();
    let clock = plx_ui::marquee::Clock::new();
    let long = c"فيلم طويل جدا يجب أن يكشف كلماته الأخيرة ببطء بعد الإيقاف المؤقت للتشغيل";
    let right = compiled_title_right(&super::loaded::classic_for_test(), ControlSlot::Discs(Controls::NONE));
    let (full, window) = (FM.width(long, HUD_TITLE_SZ, true), right - SB_X);
    assert!(full > window, "the fixture title overflows");
    let paint = |paused| plx_ui::draw_census::capture_runs(|| {
        draw_player_title(Painter::recording(), Kicker::Context(c"Movie".as_ptr()),
            long.as_ptr(), right, &FM, paused, &clock);
    });
    let _ = paint(true);
    plx_machine::idle::frame_begin(2.0);
    let moving = paint(true);
    let elapsed = 2000.0 - plx_ui::marquee::TITLE.hold_ms;
    let off = elapsed / 1000.0 * plx_ui::marquee::SPEED;
    assert_eq!(moving[1].at.x, SB_X + window - full + off, "first words flush right, moving right");
    plx_machine::idle::frame_begin(60.0);
    assert_eq!(paint(true)[1].at.x, SB_X, "the last words rest on the window's left edge");
    assert_eq!(paint(true)[1].at.x, SB_X, "and stay");
}

/// **The overscan audit grades the compiled classic's own rects**, and the report carries what the page
/// places from: the occupant's items (an enabled disc has a rect, an absent one none) and the bar's
/// x-to-fraction map. (Was `frame_geometry_is_the_classic_layouts_own_rects` and
/// `geometry_fills_the_items_and_tabs_it_was_asked_for`, whose subjects were the hand-written
/// `frame_geometry`/`geometry`; the values they held are `classic-geometry.golden.tsv`'s.)
#[test]
fn the_overscan_audit_grades_the_compiled_classic_rects() {
    let g = audit_geometry();
    assert_eq!(g.playbar, Rect::new(SB_X, SB_Y, sb_w(), SB_H));
    assert_eq!(g.row, ctrl_row_hit_rect());
    assert_eq!(g.row_anchor, RowAnchor { right: CTRL_RIGHT, top: CTRL_Y, h: CTRL_H });
    let mut audited = Vec::new();
    overscan_rects(&mut audited);
    assert_eq!(
        audited,
        vec![
            ("player scrubber", g.playbar),
            ("player control row", g.row),
            ("player bottom tab row", g.tab_row),
            ("player title / elapsed clock", g.title),
        ]
    );
    assert!(g.items[BTN_SUBTITLES as usize].is_some(), "an enabled disc has a rect");
    assert!(g.items[BTN_PREVIOUS as usize].is_none(), "an absent disc has none");
    // the bar maps an x to a fraction of ITS width, clamped
    assert_eq!(g.scrub_frac(g.playbar.x - 50.0), 0.0);
    assert_eq!(g.scrub_frac(g.playbar.x + g.playbar.w * 0.5), 0.5);
    assert_eq!(g.scrub_frac(g.playbar.x + g.playbar.w + 50.0), 1.0);
}

/// The host tests' text capability (half an em per byte).
const FM: plx_ui::fixture::FixtureMeasure = plx_ui::fixture::FixtureMeasure;

/// **The transport's clocks are resident before the draw, this second's and the next one's.**
/// On the TV the draw met each new clock string cold AFTER the back-buffer wait (`clear:12.4 …
/// textx2:1.1`, once a second under an open Tracks/More popover), which is what pushed those
/// frames past 20 ms. `ClockWarm::queue` is what `PlayerScreen::prepare` calls; the drain is
/// the presenting side's (`app::run::prepare_window`), ahead of the clear.
#[test]
fn the_transport_clocks_are_rasterised_before_the_draw_and_one_second_ahead() {
    let _g = plx_base::testlock::serial();
    plx_gfx::text::reset_prewarm_for_test();
    let sz = theme::size::CAPTION;
    let mut warm = ClockWarm::default();
    // 1:23.4 into a 2:00 item: the draw shows "1:23" (bold) and "-0:36" (regular).
    let (pos, dur) = (83_400_000_000, 120_000_000_000);
    assert_eq!((fmt_time(pos, false), fmt_time(dur - pos, true)), ("1:23".into(), "-0:36".into()));
    let at = |pos_ns: i64| HudModel { playpos_ns: pos_ns, duration_ns: dur, ..HudModel::default() };
    warm.queue(&at(pos));
    // `prepare` may run on a frame the loop then does not present: it records, uploads nothing.
    assert!(plx_gfx::text::prewarm_pending(), "prepare queued the clocks");
    assert!(!plx_gfx::text::prewarm_resident_for_test(b"1:23", sz, 1), "prepare itself uploaded");
    // Six strings against a three-string frame budget: the second presented frame finishes them.
    while plx_gfx::text::prewarm_pending() {
        plx_ui::panel_motion::PanelMotion::drain_queued_text_for_test();
    }
    for (text, bold) in [("1:23", 1), ("-0:36", 0), ("1:24", 1), ("-0:35", 0)] {
        assert!(
            plx_gfx::text::prewarm_resident_for_test(text.as_bytes(), sz, bold),
            "{text:?} (bold={bold}) was left for the draw to rasterise after the back-buffer wait"
        );
    }
    // …and the elapsed clock at the size it GROWS to during a scrub with no card
    // (`scrub_preview::clock_faces`): a cold rung there is a font open plus a rasterisation in
    // the middle of a hold.
    let grown = crate::scrub_preview::CLOCK_GROWN_SZ;
    for text in ["1:23", "1:24"] {
        assert!(
            plx_gfx::text::prewarm_resident_for_test(text.as_bytes(), grown, 1),
            "{text:?} at the grown size ({grown}) was left cold"
        );
    }

    // The draw empties the queue every frame, so the offer is repeated every prepared frame —
    // and an undrained offer must come back rather than be remembered as done.
    plx_gfx::text::reset_prewarm_for_test();
    warm.queue(&at(pos + 16_000_000));
    assert!(plx_gfx::text::prewarm_pending(), "the same second was not offered again");

    // The end of the item: nothing below zero, and no string the clock cannot show.
    plx_gfx::text::reset_prewarm_for_test();
    warm.queue(&at(dur + 5_000_000_000));
    plx_ui::panel_motion::PanelMotion::drain_queued_text_for_test();
    assert!(plx_gfx::text::prewarm_resident_for_test(b"-0:00", sz, 0));
    assert!(plx_gfx::text::prewarm_resident_for_test(b"2:05", sz, 1));
    plx_gfx::text::reset_prewarm_for_test();
}

use plx_data::metadata::{Marker, MarkerKind};
use plx_media::route::NextEpisodeMode;
use crate::skip_pill::SkipAction;

/// **The image-subtitle display set is a render, and it says how much of one** (§8.3 rule (c)):
/// one texture per rect, the SOURCE pixels behind them, and nothing left claimed once the set
/// is released. The bytes cannot be derived from the screen rects the cache keeps — those are
/// where each bitmap lands, scaled into the video rect — which is why the count is carried.
#[test]
fn the_image_subtitle_set_reports_its_own_textures_and_bytes() {
    let mut subs = SubtitleBitmaps::stub(&[(720, 120), (300, 80)]);
    let r = subs.render_report();
    assert_eq!(r.textures, 2, "a two-rect display set is two textures");
    assert_eq!(r.bytes, (720 * 120 + 300 * 80) * 4);
    subs.release();
    assert_eq!(
        subs.render_report(),
        plx_ui::frame::RenderReport::NONE,
        "a released set holds nothing and must stop claiming bytes"
    );
}

fn marker(kind: MarkerKind, final_seg: bool) -> Marker {
    Marker {
        kind,
        start_ms: 1_000,
        end_ms: 2_000,
        final_seg,
    }
}

fn seg(start_ms: i64, end_ms: i64, final_seg: bool) -> Marker {
    Marker {
        kind: MarkerKind::Credits,
        start_ms,
        end_ms,
        final_seg,
    }
}

// A rail 1000px wide over a 1000ms item: 1px per ms, so an offset reads straight off the x.
const SX: f32 = 100.0;
const SW: f32 = 1000.0;
const DUR: i64 = 1_000;

/// The classic discs' rects, as the compile reports them (what the page places and hit-tests from).
fn disc_rects() -> [Option<Rect>; MAX_ROW_ITEMS] {
    geometry(&super::loaded::classic_for_test(), ControlSlot::Discs(Controls::NONE), false, false).items
}

/// **Q5: the discs stand `CONTROL_GAP` (20) apart, not the old 22**, and the row's width is the three discs
/// and two of those gaps.
#[test]
fn the_control_row_gap_is_the_shared_control_gap() {
    let discs = disc_rects();
    let at = |i: i32| discs[i as usize].expect("a present disc");
    assert_eq!(plx_ui::widgets::CONTROL_GAP, 20.0);
    assert_eq!(at(BTN_AUDIO).x - (at(BTN_SUBTITLES).x + BTN_S), 20.0);
    assert_eq!(ctrl_row_w(), 3.0 * BTN_S + 2.0 * 20.0);
}

/// **The bottom tabs are one run `space::SM` apart**, read off the compile's tab pills; with no chapters
/// there is one pill, at the same place, and no second.
#[test]
fn the_bottom_tabs_stand_one_sm_apart_and_are_drawn_where_they_are_hit() {
    let compiled = super::loaded::classic_for_test();
    let two = geometry(&compiled, ControlSlot::Discs(Controls::NONE), true, false).tabs;
    let (info, chapters) = (two[0].expect("Info"), two[1].expect("Chapters"));
    assert_eq!(info.x, SB_X);
    assert_eq!(chapters.x - (info.x + info.w), theme::space::SM);
    let one = geometry(&compiled, ControlSlot::Discs(Controls::NONE), false, false).tabs;
    assert_eq!(one[0], Some(info), "with no chapters there is one pill, at the same place");
    assert_eq!(one[1], None, "…and no second");
}

/// The row is right-anchored and does not overlap itself: the LAST disc ends exactly at
/// `CTRL_RIGHT`, each disc sits a full `CONTROL_GAP` clear of its neighbour, and the whole span is
/// `ctrl_row_w`.
#[test]
fn the_disc_row_is_right_anchored_and_spans_ctrl_row_w() {
    let discs = disc_rects();
    let at = |i: i32| discs[i as usize].expect("a present disc");
    assert_eq!(
        at(BTN_N - 1).x + BTN_S,
        CTRL_RIGHT,
        "last disc must end at the row's edge"
    );
    for i in (BTN_SUBTITLES + 1)..BTN_N {
        assert_eq!(
            at(i).x - (at(i - 1).x + BTN_S),
            plx_ui::widgets::CONTROL_GAP,
            "gap before disc {i}"
        );
    }
    assert_eq!(
        CTRL_RIGHT - at(BTN_SUBTITLES).x,
        ctrl_row_w(),
        "drawn span vs the row's floor width"
    );
}

/// The control row's precedence, which used to live in the ORDER of five separate if-chains
/// across `player_hud` and `app.rs` — the draw, the OK handler, the pointer handler, the
/// LEFT/RIGHT clamp and `icon_hit` — with nothing keeping them in step and nothing to test.
#[test]
fn the_control_row_picks_the_most_specific_occupant() {
    // no segment under the playhead → the ordinary Subtitles + Audio pair
    assert!(slot_for(None, false, NextEpisodeMode::Countdown).is_discs());
    assert!(
        slot_for(None, true, NextEpisodeMode::Countdown).is_discs(),
        "a queued successor alone changes nothing"
    );
    assert_eq!(slot_for(None, true, NextEpisodeMode::Countdown).items(), BTN_N);
    assert_eq!(
        slot_for(None, true, NextEpisodeMode::Countdown).primary_btn(),
        BTN_SUBTITLES,
        "the discs open on Subtitles"
    );

    // an intro is always Skip, successor or not — "what's next" is an end-of-episode idea
    for has_next in [false, true] {
        let slot = slot_for(Some(marker(MarkerKind::Intro, false)), has_next, NextEpisodeMode::Countdown);
        assert!(matches!(slot, ControlSlot::Skip(p) if p.kind == MarkerKind::Intro));
        assert_eq!(slot.items(), 1, "a Skip pill is the row's only item");
        assert_eq!(
            slot.primary_btn(),
            0,
            "…so it is also the one focus lands on"
        );
    }

    // credits WITH somewhere to go → Up Next outranks Skip Credits…
    let up = slot_for(Some(marker(MarkerKind::Credits, true)), true, NextEpisodeMode::Countdown);
    assert!(matches!(up, ControlSlot::UpNext(_)));
    // …and it is the ONE stand-in with a pair, whose primary is the RIGHT-hand item. Asserted
    // together because they are one fact: an `items()` of 2 with a `primary_btn()` of 0 would
    // park the ring on Watch Credits, and `up_next::countdown_may_run` would then disarm the
    // countdown on the frame after it armed — a timer that visibly never runs.
    assert_eq!(up.items(), 2, "Watch Credits + Next Episode");
    assert_eq!(up.primary_btn(), crate::up_next::BTN_NEXT);
    // …and WITHOUT (a show's last episode) it stays Skip Credits, which is the whole reason
    // both still exist
    assert!(matches!(
        slot_for(Some(marker(MarkerKind::Credits, true)), false, NextEpisodeMode::Countdown),
        ControlSlot::Skip(p) if p.kind == MarkerKind::Credits
    ));
}

/// Up Next is only offered for a credits marker that runs to the end of the item (`final`).
/// A mid-item credits segment with a successor queued (a post-credits scene follows) is a
/// plain Skip Credits pill that seeks past it, not a countdown into the next episode.
#[test]
fn up_next_needs_a_final_credits_marker() {
    let mid = slot_for(Some(marker(MarkerKind::Credits, false)), true, NextEpisodeMode::Countdown);
    assert!(
        matches!(mid, ControlSlot::Skip(p) if p.kind == MarkerKind::Credits
            && p.action == SkipAction::Seek(2_000 * 1_000_000)),
        "a non-final credits marker offers Skip Credits even with a successor queued"
    );
    assert!(matches!(slot_for(Some(marker(MarkerKind::Credits, true)), true, NextEpisodeMode::Countdown), ControlSlot::UpNext(_)));
}

/// **Next episode: After credits / Off.** During the credits of an episode with a successor the
/// control row offers nothing — no Up Next tile (so no countdown and no HUD raise, both of
/// which key off the slot), and no Skip Credits pill. Everything else keeps its occupant.
#[test]
fn after_credits_and_off_offer_nothing_during_the_credits() {
    for mode in [NextEpisodeMode::AfterCredits, NextEpisodeMode::Off] {
        assert!(slot_for(Some(marker(MarkerKind::Credits, true)), true, mode).is_discs(), "{mode:?}");
        // a show's last episode and a movie keep the Skip Credits pill, which finishes the item
        assert!(matches!(
            slot_for(Some(marker(MarkerKind::Credits, true)), false, mode),
            ControlSlot::Skip(p) if p.action == SkipAction::Finish
        ));
        // a mid-item credits segment and an intro are skippable as ever
        assert!(matches!(
            slot_for(Some(marker(MarkerKind::Credits, false)), true, mode),
            ControlSlot::Skip(p) if p.action == SkipAction::Seek(2_000 * 1_000_000)
        ));
        assert!(matches!(slot_for(Some(marker(MarkerKind::Intro, false)), true, mode), ControlSlot::Skip(_)));
    }
    assert!(matches!(
        slot_for(Some(marker(MarkerKind::Credits, true)), true, NextEpisodeMode::Countdown),
        ControlSlot::UpNext(_)
    ));
}

/// A `final` credits segment runs to the end of the item, so skipping it FINISHES rather than
/// seeks — seeking to its stated end would race the decoder against its own last frames.
#[test]
fn only_a_final_credits_segment_finishes_the_item() {
    let fin = match slot_for(Some(marker(MarkerKind::Credits, true)), false, NextEpisodeMode::Countdown) {
        ControlSlot::Skip(p) => p.action,
        _ => unreachable!(),
    };
    assert_eq!(fin, SkipAction::Finish);

    // a mid-item credits segment (a post-credits scene follows) seeks past it and plays on
    let mid = match slot_for(Some(marker(MarkerKind::Credits, false)), false, NextEpisodeMode::Countdown) {
        ControlSlot::Skip(p) => p.action,
        _ => unreachable!(),
    };
    assert_eq!(mid, SkipAction::Seek(2_000 * 1_000_000));
    // …as does an intro, `final` flag or not (PMS only sets it on credits)
    let intro = match slot_for(Some(marker(MarkerKind::Intro, true)), false, NextEpisodeMode::Countdown) {
        ControlSlot::Skip(p) => p.action,
        _ => unreachable!(),
    };
    assert_eq!(intro, SkipAction::Seek(2_000 * 1_000_000));
}

/// The ring only goes back to the scrubber on the EDGE where a stand-in vanished under it.
/// As a steady state (`is_discs() && focused`, which is how it shipped) it fired on every
/// frame the user had walked UP to the Subtitles/Audio discs, so focus could not rest there
/// and OK on a disc never reached the track menu — invisible to the pointer path, and so to
/// the on-device suite.
#[test]
fn only_a_vanishing_standin_takes_the_focus_ring_back() {
    let discs = slot_for(None, false, NextEpisodeMode::Countdown);
    let skip = slot_for(Some(marker(MarkerKind::Intro, false)), false, NextEpisodeMode::Countdown);

    // the edge it exists for: the pill was there last frame, the discs are back, ring on the row
    assert!(standin_left_the_ring(true, discs, true));

    // the steady state it must NOT fire on: the discs were already there, so nothing vanished
    assert!(
        !standin_left_the_ring(false, discs, true),
        "walking UP to the discs is not a lost stand-in"
    );

    // nothing to take back if the ring is elsewhere, or if a stand-in still owns the row
    assert!(!standin_left_the_ring(true, discs, false));
    assert!(!standin_left_the_ring(true, skip, true));
}
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}
fn assert_rect(r: Rect, x: f32, y: f32, w: f32, h: f32) {
    assert!(
        close(r.x, x) && close(r.y, y) && close(r.w, w) && close(r.h, h),
        "got ({}, {}, {}, {}), want ({x}, {y}, {w}, {h})",
        r.x,
        r.y,
        r.w,
        r.h
    );
}

/// **Every tone has an ink, the first is the white every earlier build drew, and each rung
/// under it is strictly darker** — the ladder's whole promise is "down is dimmer", and the
/// table is indexed by position, so a rung added to `SubtitleTone::LADDER` without an ink
/// here would silently draw white. Opaque and achromatic throughout: a tone gives up light,
/// never coverage (the outline depends on it) and never hue (it tints image subtitles).
#[test]
fn the_subtitle_tones_are_a_strictly_darkening_opaque_grey_ladder_from_white() {
    use plx_plex::plex::session::SubtitleTone;
    assert_eq!(theme::SUBTITLE_INKS.len(), SubtitleTone::LADDER.len());
    assert_eq!(subtitle_ink_for(SubtitleTone::White), [1.0, 1.0, 1.0, 1.0]);
    let mut prev = f32::MAX;
    for tone in SubtitleTone::LADDER {
        let ink = subtitle_ink_for(tone);
        assert!(ink[0] == ink[1] && ink[1] == ink[2], "{tone:?} has a hue");
        assert_eq!(ink[3], 1.0, "{tone:?} is not opaque");
        assert!(ink[0] < prev, "{tone:?} is not darker than the rung above it");
        prev = ink[0];
    }
    // the darkest rung still has to be READ over the 0.85 black outline it is drawn on
    assert!(prev > 0.25, "the darkest tone has sunk into its own outline");
}

/// A 1080p-authored PGS rect must land EXACTLY where it always did — the scale is identity,
/// not merely close to it, or every Blu-ray subtitle in the library moves a pixel.
#[test]
fn image_sub_1080p_canvas_is_identity() {
    assert_rect(
        sub_screen_rect((300, 900, 1320, 96), 1920, 1080),
        300.0,
        900.0,
        1320.0,
        96.0,
    );
}

/// The bug this unit exists for: a DVD VobSub rip is authored on a 720×480 canvas, so drawn
/// 1:1 it is a postage stamp covering the top-left third of the panel. Scaled from its own
/// canvas it spans the picture and sits at the bottom, exactly like the PGS case above.
#[test]
fn image_sub_dvd_canvas_fills_the_panel() {
    // a full-width, near-bottom VobSub line
    assert_rect(
        sub_screen_rect((0, 400, 720, 60), 720, 480),
        0.0,
        900.0,
        1920.0,
        135.0,
    );
    // and an inset one keeps its position within the picture
    let r = sub_screen_rect((180, 240, 360, 48), 720, 480);
    assert_rect(r, 480.0, 540.0, 960.0, 108.0);
    assert!(
        close(r.x + r.w * 0.5, SCR_W * 0.5),
        "a canvas-centred rect stays centred"
    );
}

/// PAL (720×576) and 4K PGS (3840×2160) are the other two canvases in the wild; the 4K one
/// scales DOWN, which the old 1:1 path drew at double size and half off-screen.
#[test]
fn image_sub_pal_and_4k_canvases() {
    assert_rect(
        sub_screen_rect((0, 480, 720, 72), 720, 576),
        0.0,
        900.0,
        1920.0,
        135.0,
    );
    assert_rect(
        sub_screen_rect((600, 1800, 2640, 192), 3840, 2160),
        300.0,
        900.0,
        1320.0,
        96.0,
    );
}

/// A decoder that never declares a canvas (0) must fall back to 1:1 — the behaviour of the
/// whole path before this change — rather than divide by zero or blank the cue.
#[test]
fn image_sub_unknown_canvas_stays_1to1() {
    assert_rect(
        sub_screen_rect((300, 900, 1320, 96), 0, 0),
        300.0,
        900.0,
        1320.0,
        96.0,
    );
    assert_rect(
        sub_screen_rect((300, 900, 1320, 96), -1, 1080),
        300.0,
        900.0,
        1320.0,
        96.0,
    );
}

/// The HUD lift: image cues rise just enough to clear the transport, and only then.
#[test]
fn image_sub_lifts_only_over_the_transport() {
    let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0); // bottom 1035, overhangs
                                                           // transport down: nothing moves, ever
    assert_eq!(hud_lift([dialogue].into_iter(), false, HudBand::CLASSIC), 0.0);
    // transport up: a bottom-anchored cue rises exactly its overhang, landing ON the ceiling
    let lift = hud_lift([dialogue].into_iter(), true, HudBand::CLASSIC);
    assert!(
        close(1035.0 - lift, HudBand::CLASSIC.ceil_y),
        "lifted bottom should sit at the ceiling"
    );
    // a sign at the top of frame already clears it and must stay put
    let sign = Rect::new(100.0, 100.0, 200.0, 100.0);
    assert_eq!(hud_lift([sign].into_iter(), true, HudBand::CLASSIC), 0.0);
    assert!(!overhangs(sign, HudBand::CLASSIC) && overhangs(dialogue, HudBand::CLASSIC));
    // a set too tall to lift is clamped instead of being pushed off the top of the screen
    assert_eq!(
        hud_lift([Rect::new(0.0, 0.0, 1920.0, SCR_H)].into_iter(), true, HudBand::CLASSIC),
        0.0
    );
    assert_eq!(
        hud_lift(
            [Rect::new(0.0, 50.0, 1920.0, SCR_H - 50.0)].into_iter(),
            true,
            HudBand::CLASSIC
        ),
        50.0
    );
}

/// The multi-rect case the lift has to get right, and the reason it is measured over the
/// OVERHANGING rects rather than the whole set: a sign plus dialogue must lift the dialogue
/// clear WITHOUT dragging the sign (whose top would otherwise clamp the whole shift), and
/// two-line dialogue must move as one unit or the two lines collapse onto each other.
#[test]
fn image_sub_lift_spans_only_the_rects_that_overhang() {
    let sign = Rect::new(100.0, 100.0, 200.0, 100.0);
    let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0);
    let lift = hud_lift([sign, dialogue].into_iter(), true, HudBand::CLASSIC);
    assert!(
        close(lift, 1035.0 - HudBand::CLASSIC.ceil_y),
        "the sign's top must not clamp the lift"
    );
    assert!(
        close(dialogue.y + dialogue.h - lift, HudBand::CLASSIC.ceil_y),
        "dialogue clears the transport"
    );
    assert!(!overhangs(sign, HudBand::CLASSIC), "and the sign is not shifted at all");

    // two-line dialogue: different overhangs, ONE shift, so the gap between them survives
    let l1 = Rect::new(300.0, 900.0, 1320.0, 50.0); // bottom 950
    let l2 = Rect::new(300.0, 960.0, 1320.0, 50.0); // bottom 1010
    let two = hud_lift([l1, l2].into_iter(), true, HudBand::CLASSIC);
    assert!(
        close(two, 1010.0 - HudBand::CLASSIC.ceil_y),
        "measured over the group, not per rect"
    );
    assert!(overhangs(l1, HudBand::CLASSIC) && overhangs(l2, HudBand::CLASSIC), "both take the same shift");
    assert!(
        close((l2.y - two) - (l1.y + l1.h - two), 10.0),
        "their 10px gap is preserved"
    );
}

// ---- who owns the "the pipeline is working" signal -----------------------------------------

use plx_media::player::PlaybackState as S;

/// Every state the enum has, so a state added later cannot quietly escape the table below.
const ALL_STATES: [S; 8] = [
    S::Idle,
    S::Resolving,
    S::Connecting,
    S::Buffering,
    S::Seeking,
    S::Playing,
    S::Error,
    S::Paused,
];

/// The whole point of routing both surfaces through one function: for any state, and either
/// answer to "has this session shown a picture", there is exactly ONE indicator — never two
/// (the reported bug) and never none while the pipeline is working.
///
/// Driven off `is_busy()` itself rather than a hand-copied list, so a FUTURE busy state cannot
/// silently lose its indicator; the membership guard underneath pins what `is_busy()` means, so
/// the coverage claim can't be satisfied by quietly narrowing it.
#[test]
fn exactly_one_surface_owns_the_busy_signal() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    for st in ALL_STATES {
        for seen in [false, true] {
            let b = busy_surface(&ps, st, seen);
            if st.is_busy() || st == S::Error {
                assert_ne!(b, Busy::None, "{st:?}/seen={seen} lost its indicator");
            } else {
                assert_eq!(b, Busy::None, "{st:?}/seen={seen} must show nothing");
            }
        }
    }
    for st in ALL_STATES {
        let want = matches!(st, S::Resolving | S::Connecting | S::Buffering | S::Seeking);
        assert_eq!(st.is_busy(), want, "{st:?} changed sides of is_busy()");
    }
}

/// The regression, named after the bug. The shipped guard was `is_busy() && frames() == 0`, and
/// `pump` zeroes `frames` as part of APPLYING a seek — so the centred read-out fired on every
/// seek, on top of the transport's own spinner. Keyed on the SESSION bit instead, a seek over a
/// live picture is the transport's alone.
#[test]
fn a_seek_over_a_live_picture_belongs_to_the_transport() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    assert_eq!(busy_surface(&ps, S::Seeking, true), Busy::Transport);
    assert!(!matches!(busy_surface(&ps, S::Seeking, true), Busy::Readout(..)));
}

/// The 1-3 frame window between `engine`'s prime→Play clear of `seeking` and the first
/// presented frame, which the pump publishes as `Buffering`. Keyed on the state alone this
/// flashes the centred block at the END of every seek; it is precisely why the rule takes
/// `seen_frame`, and without the assertion someone will "simplify" it back.
#[test]
fn the_post_seek_buffering_tail_does_not_flash_the_centre_readout() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    assert_eq!(busy_surface(&ps, S::Buffering, true), Busy::Transport);
    assert_eq!(
        busy_surface(&ps, S::Resolving, true),
        Busy::Transport,
        "auto-advance over a live picture"
    );
    assert_eq!(busy_surface(&ps, S::Connecting, true), Busy::Transport);
}

/// **An audio-track pick's reload says "Switching audio…"; every other wait keeps its caption.**
/// Only `Buffering` is renamed (the state a rebuild's load tail reads); a cold start's
/// Preparing/Connecting and a seek stay as they were.
#[test]
fn an_audio_pick_rebuild_reads_switching_audio_and_nothing_else_does() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    assert_eq!(
        busy_surface_with(&ps, S::Buffering, false, true),
        Busy::Readout(StatusKind::Working, c"Switching audio\u{2026}")
    );
    assert_eq!(
        busy_surface_with(&ps, S::Buffering, false, false),
        Busy::Readout(StatusKind::Working, c"Buffering\u{2026}")
    );
    assert_eq!(
        busy_surface_with(&ps, S::Connecting, false, true),
        Busy::Readout(StatusKind::Working, c"Connecting\u{2026}")
    );
    assert_eq!(busy_surface_with(&ps, S::Buffering, true, true), Busy::Transport);
}

/// A paused session that is doing no work, driven through the state the HUD really reads
/// (`player::state` over the published `pb_state`, which `pump::paused_and_idle` sets to
/// `Paused`): no centred read-out, no transport spinner, and the paused transport (not
/// "playing").
#[test]
fn a_paused_idle_session_with_no_picture_draws_the_paused_transport_not_a_busy_surface() {
    let _g = plx_base::testlock::serial();
    let ps = plx_media::route::PlaybackSession::IDLE;
    let was = plx_media::player::swap_state_for_test(S::Paused);
    let st = plx_media::player::state(&ps);
    let busy = busy_surface_with(&ps, st, false, false);
    let playing = plx_media::player::is_playing(&ps);
    plx_media::player::restore_state_for_test(was);
    assert_eq!(st, S::Paused);
    assert_eq!(busy, Busy::None);
    assert!(!playing, "the transport shows its paused glyph");
}

/// No picture yet → the wait is about the whole panel. With the captions, which locks the
/// kind↔caption pairing the enum carries (both are chosen once, by this function).
#[test]
fn a_cold_start_and_a_reload_both_own_the_whole_panel() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    assert_eq!(
        busy_surface(&ps, S::Resolving, false),
        Busy::Readout(StatusKind::Working, c"Preparing\u{2026}")
    );
    assert_eq!(
        busy_surface(&ps, S::Connecting, false),
        Busy::Readout(StatusKind::Working, c"Connecting\u{2026}")
    );
    assert_eq!(
        busy_surface_with(&ps, S::Buffering, false, false),
        Busy::Readout(StatusKind::Working, c"Buffering\u{2026}")
    );
    // tapping RIGHT during pre-roll (or `/tmp/plxnative-autoseek`): no picture to mark up
    assert_eq!(
        busy_surface(&ps, S::Seeking, false),
        Busy::Readout(StatusKind::Working, c"Seeking\u{2026}")
    );
}

/// A dead producer has no picture and no position, only a message — so it is the centred
/// read-out either way. And it is a FAULT, never `Empty`, which is the "this is an answer"
/// treatment.
#[test]
fn a_dead_producer_reads_out_whether_or_not_a_picture_was_up() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    for seen in [false, true] {
        assert_eq!(
            busy_surface(&ps, S::Error, seen),
            Busy::Readout(StatusKind::Failed, c"Playback failed")
        );
    }
}

fn readout_of(e: &plx_media::player::ErrorShape) -> FailureReadout {
    let cx = plx_media::player::FailureContext { forced: true, can_retry: true, repair_idle: true };
    FailureReadout::of(e, plx_media::player::failure_actions(e.kind, cx), "0.7.0 · webOS 4.5 · code".into())
}

/// **Say it once.** Under Force the reason IS the route's verdict (what happened and why), no
/// note repeats it, the row is *Switch to Auto and play* · *Back* (no *Details*), and the
/// support facts are the dim footer — which the draw really paints, below the row.
#[test]
fn the_forced_readout_says_what_and_why_once_and_draws_the_diagnostics_as_the_footer() {
    use plx_media::player::FailureAction as A;
    let verdict = plx_media::route::PlayVerdict::Forced(plx_media::route::ForcedFailure::Video).text().into_owned();
    let e = plx_media::player::failtest_policy_shape_for_test(&verdict);
    let r = readout_of(&e);
    assert_eq!(r.reason(), verdict.as_str());
    assert_eq!(r.note(), None, "the verdict already names the cause");
    assert_eq!(r.actions(), &[A::PlayAutomatically, A::Back]);
    assert_eq!(r.footer(), "0.7.0 · webOS 4.5 · code");
    let log = plx_ui::draw_census::capture(|| {
        r.draw(plx_ui::Painter::recording(), 0, &plx_ui::fixture::FixtureMeasure);
    });
    let texts: Vec<Rect> = log.iter().filter(|(tag, _)| *tag == 100).map(|(_, r)| *r).collect();
    assert!(texts.iter().any(|t| (t.y - FOOTER_TOP).abs() < theme::size::CAPTION as f32 * 2.0),
        "the footer line is drawn at the foot of the screen: {texts:?}");
}

/// The read-out's row, note and footer stack top to bottom inside the safe band, and the
/// footer never meets the row.
#[test]
fn the_diagnostics_footer_sits_below_the_row_inside_the_safe_bottom_band() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let verdict = plx_media::route::PlayVerdict::Forced(plx_media::route::ForcedFailure::Video).text().into_owned();
    let e = plx_media::player::failtest_policy_shape_for_test(&verdict);
    let r = readout_of(&e);
    let row_bottom = r.frames(&ShippedMeasure).iter().flatten().map(|f| f.y + f.h).fold(0.0, f32::max);
    assert!(row_bottom > 0.0, "the row was laid out");
    // a two-line CAPTION note sits between them, with room to spare
    let note_box = theme::size::CAPTION as f32 * 1.5 * 2.0;
    assert!(FOOTER_TOP > row_bottom + note_box + theme::space::MD,
        "footer top {FOOTER_TOP} must clear the row ({row_bottom}) and its note");
    let line_box = theme::size::CAPTION as f32 * 1.5;
    assert!(FOOTER_TOP + line_box < SCR_H - theme::space::XL + 1.0);
}

/// **Every row fits the screen, in every shipped language**: the longest row each kind can
/// draw, measured with the device's advances, stays inside the read-out's width.
#[test]
fn every_failure_row_fits_the_screen_in_every_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::fit::HEADROOM;
    use plx_platform::i18n::{language_on_this_thread_for_test};
    use plx_media::player::FailureAction as A;
    let rows: [&[A]; 3] = [
        &[A::PlayAutomatically, A::TryAgain, A::Back],
        &[A::TryAgain, A::ChangeQuality, A::Back],
        &[A::Repair, A::Back],
    ];
    let mut out = Vec::new();
    for language in plx_platform::i18n::SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        for row in rows {
            let labels: Vec<&'static std::ffi::CStr> = row.iter().map(|a| failure_action_label(*a)).collect();
            let o = StatusOverlay::new(Rect::FULL, c"", StatusKind::Failed).page(plx_ui::icons::Icon::Alert).reason(c"x").row(&labels);
            let frames = o.row_frames_measured(&ShippedMeasure);
            let lo = frames.iter().flatten().map(|f| f.x).fold(f32::MAX, f32::min);
            let hi = frames.iter().flatten().map(|f| f.x + f.w).fold(0.0, f32::max);
            let limit = SCR_W - 2.0 * plx_ui::consts::MARGIN_X;
            if (hi - lo) > limit * HEADROOM || lo < plx_ui::consts::MARGIN_X {
                out.push(format!("{}: {row:?} spans {:.0}px of {limit:.0}", language.tag(), hi - lo));
            }
        }
    }
    assert!(out.is_empty(), "rows wider than the screen:\n  {}", out.join("\n  "));
}

/// The sandbox failure's help sentence ends in a URL that cannot break, and it is drawn as the
/// read-out's note — centred CAPTION, two lines of [`StatusOverlay::REASON_W`]. It must fit
/// that in every shipped language, or the pointer to the fix is the part an ellipsis eats.
#[test]
fn the_jail_help_note_fits_the_note_slot_in_every_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::fit::HEADROOM;
    use plx_ui::text_view::TextView;
    use plx_platform::i18n::{language_on_this_thread_for_test, msg};
    let mut out = Vec::new();
    for language in plx_platform::i18n::SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let help = msg::widgets_reason_jail_help();
        let fits = !TextView::new(help, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .h(plx_ui::label::HAlign::Center)
            .max_lines(2)
            .with_measure(&ShippedMeasure)
            .truncates(StatusOverlay::REASON_W * HEADROOM);
        if !fits {
            out.push(format!("{}: {help}", language.tag()));
        }
    }
    assert!(out.is_empty(), "jail help cut short:\n  {}", out.join("\n  "));
}

/// Every Force verdict fits the read-out's two-line reason slot, in every shipped language.
#[test]
fn every_forced_verdict_fits_the_reason_slot_in_every_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::fit::HEADROOM;
    use plx_platform::i18n::{language_on_this_thread_for_test, msg};
    let mut out = Vec::new();
    for language in plx_platform::i18n::SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        for s in [msg::widgets_verdict_forced_no_original(), msg::widgets_verdict_forced_container(),
            msg::widgets_verdict_forced_video(), msg::widgets_verdict_forced_audio(),
            msg::widgets_verdict_forced_unauthorized(), msg::widgets_verdict_forced_open(),
            msg::widgets_verdict_forced_audio_conversion(), msg::widgets_reason_forced_playback_help()] {
            let c = std::ffi::CString::new(s).unwrap();
            if StatusOverlay::failed_reason_truncates(&c, &ShippedMeasure, HEADROOM) {
                out.push(format!("{}: {s}", language.tag()));
            }
        }
    }
    assert!(out.is_empty(), "verdicts cut short:\n  {}", out.join("\n  "));
}

/// …and when it does, it owns the WHOLE frame: `draw_hud` draws nothing — no scrim, no
/// scrubber, no clocks, no bottom tabs (`Player Screen.dc.html` sets `hudDisplay:none` and
/// `tabsDisplay:none` on the failed variant, because a live-looking scrubber at 0:00 over a
/// black panel is the bug the read-out exists to end).
///
/// The other half is the one that would be easy to break by simplifying this to "a read-out is
/// up": every WORKING read-out must leave the transport alone, or the HUD blanks through every
/// cold start, every reconnect and every pre-roll seek — states where the position is real and
/// the transport is what the user reads the instant the first frame lands.
#[test]
fn only_a_failure_takes_the_frame_away_from_the_transport() {
    let ps = plx_media::route::PlaybackSession::IDLE;
    for seen in [false, true] {
        assert!(
            readout_owns_frame(busy_surface(&ps, S::Error, seen)),
            "a failure hides the transport"
        );
    }
    for st in [S::Resolving, S::Connecting, S::Buffering, S::Seeking] {
        let b = busy_surface(&ps, st, false);
        assert!(
            matches!(b, Busy::Readout(StatusKind::Working, _)),
            "{st:?} is a read-out"
        );
        assert!(
            !readout_owns_frame(b),
            "{st:?} is mid-flight — the transport stays up under it"
        );
    }
    // and the two non-read-out surfaces are never a reason to blank the frame
    assert!(!readout_owns_frame(busy_surface(&ps, S::Playing, true)));
    assert!(
        !readout_owns_frame(busy_surface(&ps, S::Seeking, true)),
        "a seek over a live picture"
    );
}

/// The geometry regression, guarding against a new `OVERLAY_BOTTOM`: the read-out centres on
/// the PANEL, and doing so still clears the transport — the constraint the carve-out claimed to
/// be enforcing and was not.
///
/// The spinner half of the block is pure geometry (the caption half needs `text_height`, which
/// the host suite cannot link) and is the LARGER half, so `above()` is a conservative stand-in
/// for the whole block. Numerically 540 + 58.16 = 598.16 < 610: deliberately tight, so it fails
/// if someone bumps `R_PAGE`, thickens the scrim, or re-carves the frame.
#[test]
fn the_readout_is_centred_and_still_clears_the_transport() {
    let f = readout_frame();
    assert_eq!(
        f.cy(),
        SCR_H * 0.5,
        "the wait is about the whole picture, so it centres on it"
    );
    assert_eq!(f.cx(), SCR_W * 0.5);
    assert!(
        f.cy() + StatusOverlay::above() < SCR_H - SCRIM_H,
        "the read-out must not reach into the transport scrim"
    );
    assert!(
        f.cy() - StatusOverlay::above() > 0.0,
        "nor off the top of the panel"
    );
}

// ---- the transport state read-out ---------------------------------------------------------
//
// `transport_mark` is the whole rule, factored out of the draw so it can be graded here: the
// draw itself is one `match` over the returned value, and the slot's geometry is unchanged.
// 1 s in ns, so a position reads as a second.
const S: i64 = 1_000_000_000;

/// The common case, and the one the design is FOR: frames on the panel, nothing pressed
/// recently, and the slot is empty. A mark that is always up says nothing.
#[test]
fn playing_steadily_shows_nothing() {
    assert_eq!(
        transport_mark(false, Busy::None, false, 42 * S, 42 * S, None),
        TransportMark::None
    );
    // …and the resume mark has expired rather than never existed
    assert_eq!(
        transport_mark(false, Busy::None, false, 42 * S, 42 * S, Some(PLAY_MARK_MS)),
        TransportMark::None
    );
}

/// Paused is UNCHANGED from the behaviour that shipped before the read-out grew its other two
/// states, and it outranks the resume mark: a user who resumed and re-paused inside two seconds
/// is paused, whatever the clock still says.
#[test]
fn paused_shows_pause_and_outranks_a_live_resume_clock() {
    assert_eq!(
        transport_mark(true, Busy::None, false, 42 * S, 42 * S, None),
        TransportMark::Pause
    );
    assert_eq!(
        transport_mark(true, Busy::None, false, 42 * S, 42 * S, Some(200)),
        TransportMark::Pause
    );
}

/// Direction comes from the POSITIONS, not from a keycode — which is what makes one expression
/// cover the three ways the playhead travels. A live scrub is not busy (the pipeline is still
/// playing the old position), so `scrubbing` has to be its own input.
#[test]
fn travel_direction_is_read_off_the_positions() {
    // LEFT/RIGHT scrub preview, while playing: not busy at all
    assert_eq!(
        transport_mark(false, Busy::None, true, 52 * S, 42 * S, None),
        TransportMark::FastForward
    );
    assert_eq!(
        transport_mark(false, Busy::None, true, 32 * S, 42 * S, None),
        TransportMark::Rewind
    );
    // a chapter/marker hop or a rapid-seek burst: scrub cleared, the seek in flight, the frozen
    // playhead sitting at the target while the published position is still where we left
    assert_eq!(
        transport_mark(false, Busy::Transport, false, 600 * S, 42 * S, None),
        TransportMark::FastForward
    );
    assert_eq!(
        transport_mark(false, Busy::Transport, false, 5 * S, 42 * S, None),
        TransportMark::Rewind
    );
}

/// Travel outranks paused — a paused scrub is still a scrub, and that is also the precedence
/// the slot had when its two states were spinner-over-pause.
#[test]
fn a_paused_scrub_still_reads_as_travel() {
    assert_eq!(
        transport_mark(true, Busy::None, true, 32 * S, 42 * S, None),
        TransportMark::Rewind
    );
    assert_eq!(
        transport_mark(true, Busy::Transport, false, 90 * S, 42 * S, None),
        TransportMark::FastForward
    );
}

/// What the spinner was narrowed TO. `Busy::Transport` is two facts — the seek the user asked
/// for, and a re-buffer nobody asked for — and the direction glyph can only speak for the
/// first. So the spinner keeps every busy frame with no direction in it, and gives up the ones
/// that have one.
#[test]
fn the_spinner_keeps_only_the_busy_frames_with_no_direction() {
    // a mid-play re-buffer: busy, but the playhead is not going anywhere
    assert_eq!(
        transport_mark(false, Busy::Transport, false, 42 * S, 42 * S, None),
        TransportMark::Working
    );
    // the prime→first-frame tail of a landed seek, once the published position has caught up
    assert_eq!(
        transport_mark(false, Busy::Transport, true, 42 * S, 42 * S, None),
        TransportMark::Working
    );
    // and the centred read-out's own states never reach this slot
    assert_eq!(
        transport_mark(
            false,
            Busy::Readout(StatusKind::Working, c"Buffering…"),
            false,
            0,
            0,
            None
        ),
        TransportMark::None
    );
}

/// "Play sign show only for a couple of seconds when we press play, do not show it all the play
/// time." The clock is ms since the paused→playing edge; `None` means this session has never
/// been resumed, which is a fresh start rather than a resume and draws nothing.
#[test]
fn the_play_mark_expires_and_a_never_resumed_session_has_none() {
    assert_eq!(
        transport_mark(false, Busy::None, false, 0, 0, Some(0)),
        TransportMark::Play
    );
    assert_eq!(
        transport_mark(false, Busy::None, false, 0, 0, Some(PLAY_MARK_MS - 1)),
        TransportMark::Play
    );
    assert_eq!(
        transport_mark(false, Busy::None, false, 0, 0, Some(PLAY_MARK_MS)),
        TransportMark::None,
        "the couple of seconds is a boundary, not a suggestion"
    );
    assert_eq!(
        transport_mark(false, Busy::None, false, 0, 0, None),
        TransportMark::None
    );
}

/// A resume the user asked for while the playhead is also travelling: the travel is the newer
/// fact and the slot is one glyph, so it wins. (This is the frame after a paused scrub commits
/// — `commit_seek` drops `paused` to let the pipeline prime the new position.)
#[test]
fn travel_outranks_the_resume_mark() {
    assert_eq!(
        transport_mark(false, Busy::Transport, false, 90 * S, 42 * S, Some(0)),
        TransportMark::FastForward
    );
}

/// **A cue is never cut at a line count**: every wrapped line of every segment survives the
/// wrap, however many there are.
#[test]
fn a_six_line_cue_wraps_to_six_lines() {
    let text = "one\ntwo\nthree\nfour\nfive\nsix";
    assert_eq!(caption_wrap(text, 1.0, &FM), ["one", "two", "three", "four", "five", "six"]);
    let long = "word ".repeat(120);
    let wrapped = caption_wrap(long.trim(), 1.0, &FM);
    assert!(wrapped.len() > 3, "a long cue wraps past the anchor count, got {}", wrapped.len());
    assert_eq!(wrapped.join(" ").split_whitespace().count(), 120, "no word is lost");
}

/// **A larger size widens the line, it does not add lines.** An ordinary two-line cue at Extra
/// large wrapped at 28 characters, so it became four lines and stepped down to ~47 px (smaller
/// than the viewer asked for).
/// It must stay two lines, drawn at the full Extra large size.
#[test]
fn a_two_line_cue_at_extra_large_stays_two_lines_at_full_size() {
    let xl = size_scale(plx_plex::plex::session::SubtitleSize::ExtraLarge);
    let cue = "I don't know why you came here tonight,\nbut you had better leave right now.";
    let lines = caption_wrap(cue, xl, &FM);
    assert_eq!(lines, ["I don't know why you came here tonight,", "but you had better leave right now."]);
    let lay = caption_layout(lines.len(), xl, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!(lay.face, 54, "full Extra large size");
}

/// **Huge, the rung above Extra large, keeps the same two lines too**, at its full 65 px (the
/// official client's largest caption reads about that size on the TV).
#[test]
fn a_two_line_cue_at_huge_stays_two_lines_at_full_size() {
    let huge = size_scale(plx_plex::plex::session::SubtitleSize::Huge);
    let cue = "I don't know why you came here tonight,\nbut you had better leave right now.";
    let lines = caption_wrap(cue, huge, &FM);
    assert_eq!(lines, ["I don't know why you came here tonight,", "but you had better leave right now."]);
    let lay = caption_layout(lines.len(), huge, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!(lay.face, 65, "full Huge size");
}

/// **No wrapped line is wider than the wrap width, and no size's width passes three quarters
/// of the screen**; only a single word wider than the width keeps a line of its own.
#[test]
fn caption_lines_stay_inside_the_wrap_width() {
    use plx_plex::plex::session::SubtitleSize;
    let widths: Vec<f32> = SubtitleSize::LADDER.iter().map(|&s| wrap_width(size_scale(s))).collect();
    assert_eq!(widths, [760.0 * 0.78, 760.0, 950.0, 1140.0, 1368.0]);
    assert!(widths.iter().all(|&w| w <= 1440.0), "{widths:?}");
    assert_eq!(wrap_width(10.0), 1440.0, "any size is capped at 75% of 1920");
    let measure = |s: &str| s.chars().count() as f32 * 10.0;
    let lines = wrap_px("aa bbb cccc dd eeeeeeeeeeee f", 80.0, &measure);
    assert_eq!(lines, ["aa bbb", "cccc dd", "eeeeeeeeeeee", "f"]);
    assert!(lines.iter().all(|l| measure(l) <= 80.0 || !l.contains(' ')), "{lines:?}");
}

/// `text_style::today_baseline`/`today_layout` are a port of `caption_geometry`/`caption_layout`
/// with hard-coded constants (the synthesized ASS header is built from them); this pins that the
/// port and the code it replaces cannot drift: 3 positions x 5 sizes x HUD up/down, 1-3 lines.
#[test]
fn text_style_baseline_ties_to_caption_geometry_and_layout() {
    use plx_media::player::text_style::{today_baseline, today_layout};
    use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
    assert_eq!(CAPTION_FLOOR_Y, SCR_H - plx_ui::consts::MARGIN_Y, "media hard-codes the screen margin");
    assert_eq!((plx_media::player::text_style::SCR_W, plx_media::player::text_style::SCR_H), (SCR_W, SCR_H));
    for band in [HudBand::CLASSIC, HudBand::from_controls_top(700.0)] {
        let mut n = 0;
        for position in SubtitlePosition::LADDER {
            for size in SubtitleSize::LADDER {
                for hud_up in [false, true] {
                    let (scale, baseline, floor) = caption_geometry_for(size, position, hud_up, band);
                    let t = today_baseline(position, size_scale(size), hud_up, band);
                    assert_eq!((t.scale, t.baseline, t.floor), (scale, baseline, floor), "{position:?} {size:?} up={hud_up}");
                    for lines in 1..=CAPTION_ANCHOR_LINES {
                        let lay = caption_layout(lines, scale, baseline, floor);
                        let tl = today_layout(lines, scale, baseline);
                        assert_eq!((tl.face, tl.pitch, tl.top), (lay.face, lay.pitch, lay.top), "{lines} lines");
                    }
                    n += 1;
                }
            }
        }
        assert_eq!(n, 30);
    }
}

/// **The caption draw and the band's difference**: a band that is not classic's moves the lifted
/// baseline and floor by exactly the difference of the two bands, and the HUD-down placement not at
/// all.
#[test]
fn a_non_classic_band_moves_the_lifted_caption_by_its_difference() {
    use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
    let band = HudBand::from_controls_top(700.0);
    let shift = HudBand::CLASSIC.controls_top_y - band.controls_top_y;
    assert_eq!(shift, 92.0);
    for position in SubtitlePosition::LADDER {
        for size in SubtitleSize::LADDER {
            let (_, cb, cf) = caption_geometry_for(size, position, true, HudBand::CLASSIC);
            let (_, nb, nf) = caption_geometry_for(size, position, true, band);
            assert_eq!((cb - nb, cf - nf), (shift, shift), "{position:?} {size:?}");
            assert_eq!(
                caption_geometry_for(size, position, false, band),
                caption_geometry_for(size, position, false, HudBand::CLASSIC),
                "the HUD down ignores the band"
            );
        }
    }
}

/// The image-subtitle lift lands the overhanging group on the band's ceiling, so a higher band
/// lifts the same cue by exactly the difference.
#[test]
fn image_sub_lift_follows_the_band() {
    let band = HudBand::from_controls_top(700.0);
    let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0);
    let classic = hud_lift([dialogue].into_iter(), true, HudBand::CLASSIC);
    let lift = hud_lift([dialogue].into_iter(), true, band);
    assert!(close(lift - classic, 92.0), "{lift} vs {classic}");
    assert!(close(1035.0 - lift, band.ceil_y), "the lifted bottom sits on the band's ceiling");
    // A cue that clears classic's ceiling but not the higher band's is lifted only by the latter.
    let above = Rect::new(300.0, 650.0, 1320.0, 120.0); // bottom 770
    assert!(!overhangs(above, HudBand::CLASSIC) && overhangs(above, band));
    assert_eq!(hud_lift([above].into_iter(), true, HudBand::CLASSIC), 0.0);
    assert!(close(hud_lift([above].into_iter(), true, band), 770.0 - band.ceil_y));
}

/// **`min_controls_top_y`**: the highest row top for which the largest block (three lines at the
/// largest size, the highest position) is still inside `SAFE`. The number is computed, not
/// estimated: 54 (SAFE top) + 3 x 48 x 1.8 (block) + 320 (position) + 12 (clearance) = 645.2, up
/// to the whole pixel 646. A band exactly there keeps the block on screen through
/// `caption_geometry_for` and `hud_lift`; one px higher does not.
#[test]
fn min_controls_top_y_is_where_the_largest_block_just_fits() {
    use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
    let bound = min_controls_top_y();
    assert_eq!(bound, 646.0);
    assert!(bound <= HudBand::CLASSIC.controls_top_y, "classic's own row passes");
    let (size, position) = (SubtitleSize::Huge, SubtitlePosition::High);
    let top_of_block = |band: HudBand| {
        let (scale, baseline, floor) = caption_geometry_for(size, position, true, band);
        caption_layout(CAPTION_ANCHOR_LINES, scale, baseline, floor).top
    };
    assert!(top_of_block(HudBand::from_controls_top(bound)) >= SAFE.y);
    assert!(top_of_block(HudBand::from_controls_top(bound - 1.0)) < SAFE.y, "one px higher does not");
    // Every other pick of size and position fits at the bound too: it is the binding one.
    for p in SubtitlePosition::LADDER {
        for z in SubtitleSize::LADDER {
            let (scale, baseline, floor) = caption_geometry_for(z, p, true, HudBand::from_controls_top(bound));
            assert!(caption_layout(CAPTION_ANCHOR_LINES, scale, baseline, floor).top >= SAFE.y, "{p:?} {z:?}");
        }
    }
    // The image path: the same block authored on the highest position's baseline. `hud_lift` lands
    // its bottom on the BAND's ceiling (classic's 780 would not move it at all, so this is what
    // ties the reader to the band) and, being a lift, leaves the top inside `SAFE`.
    let height = CAPTION_ANCHOR_LINES as f32 * CAPTION_LINE_PITCH * size_scale(size);
    let bottom = SUB_BASE_Y - position_lift(position);
    let block = Rect::new(300.0, bottom - height, 1320.0, height);
    for band in [HudBand::from_controls_top(bound), HudBand::from_controls_top(bound - 1.0)] {
        let lift = hud_lift([block].into_iter(), true, band);
        assert!(close(block.y + block.h - lift, band.ceil_y), "{band:?}: bottom on the ceiling");
        assert!(block.y - lift >= SAFE.y, "{band:?}: lifted top {}", block.y - lift);
    }
    // The clamp, not the bound: a band far past it cannot push the block off the screen.
    let far = HudBand::from_controls_top(200.0);
    let lift = hud_lift([block].into_iter(), true, far);
    assert!(block.y - lift >= 0.0, "a band far past the bound clamps on screen");
    assert!(block.y + block.h - lift > far.ceil_y, "…which is the clamp, short of the ceiling");
}

/// **`HudGeometry::band` is the compact row's top**, in every slot: classic's layout reports
/// `HudBand::CLASSIC`, from the slot-independent half and from the whole geometry alike.
#[test]
fn the_layouts_band_is_classics_in_every_slot() {
    assert_eq!(audit_geometry().band(), HudBand::CLASSIC);
    let compiled = super::loaded::classic_for_test();
    let slots = [
        ControlSlot::Discs(Controls::NONE),
        ControlSlot::Discs(Controls { previous: true, next: true, restart: true }),
        slot_for(Some(marker(MarkerKind::Intro, false)), false, NextEpisodeMode::Countdown),
        slot_for(Some(marker(MarkerKind::Credits, true)), true, NextEpisodeMode::Countdown),
    ];
    for slot in slots {
        assert_eq!(geometry(&compiled, slot, true, false).band(), HudBand::CLASSIC);
    }
    assert_eq!(
        HudBand::CLASSIC,
        HudBand::from_controls_top(CTRL_Y),
        "the band is the row's own top: the one number classic draws its row at"
    );
}

/// **A 1-3 line caption lays out exactly as it did before the fit rule**: 36 px at the
/// viewer's default size, a 48 px pitch, and the block's bottom on the baseline.
#[test]
fn a_three_line_caption_keeps_its_size_and_position() {
    let lay = caption_layout(3, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!(lay.face, 36);
    assert_eq!(lay.pitch, 48.0);
    assert_eq!(lay.top, 836.0);
    assert_eq!(lay.line_top(0), 836.0);
    assert_eq!(lay.line_top(2), 932.0);
    let one = caption_layout(1, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!((one.face, one.top), (36, 932.0), "one line sits on the baseline");
    let two = caption_layout(2, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!((two.face, two.top), (36, 884.0));
}

/// **A six-line block at the default size is too tall for the area below the capsule, so it
/// steps down, and every one of its lines is still laid out inside the floor.**
#[test]
fn a_six_line_block_steps_down_and_keeps_every_line() {
    let lay = caption_layout(6, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert!(lay.face < 36, "the face steps down, got {}", lay.face);
    assert_eq!(lay.lines, 6);
    let bottom = lay.line_top(5) + lay.pitch;
    assert!(bottom <= CAPTION_FLOOR_Y, "the block ends at {bottom}, past the floor");
}

/// **A block that cannot fit even at the smallest step is still drawn whole.** Lines are never
/// dropped to make room.
#[test]
fn a_block_too_tall_for_any_step_keeps_all_its_lines() {
    let lay = caption_layout(12, 1.5, SUB_BASE_Y, CAPTION_FLOOR_Y);
    assert_eq!(lay.lines, 12);
    assert_eq!(lay.face, (36.0 * 1.5 * CAPTION_FIT_STEPS[3]).round() as i32, "smallest step");
}

/// **The Timing capsule's y is the same for a 1, 3 or 6-line caption.** Its bottom edge is
/// [`CAPSULE_BOTTOM_Y`], and a taller block grows down from that edge, never up into the
/// capsule.
#[test]
fn the_capsule_edge_is_fixed_and_a_taller_block_grows_away_from_it() {
    assert_eq!(CAPSULE_BOTTOM_Y, 836.0);
    for n in [1usize, 3, 6] {
        let lay = caption_layout(n, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y);
        assert!(
            lay.top >= CAPSULE_BOTTOM_Y,
            "{n} lines: block top {} rises into the capsule (bottom {CAPSULE_BOTTOM_Y})",
            lay.top
        );
    }
    assert_eq!(caption_layout(6, 1.0, SUB_BASE_Y, CAPTION_FLOOR_Y).top, CAPSULE_BOTTOM_Y);
}

mod playback_navigation_tests {
    use super::*;
    use plx_ui::icons::Icon;

    #[test]
    fn episode_pair_uses_custom_gap_without_moving_existing_discs() {
        let compiled = super::super::loaded::classic_for_test();
        let pair = Controls::for_item(true, false, true, false);
        let rects = |c: Controls| geometry(&compiled, ControlSlot::Discs(c), false, false).items;
        let (pair_rects, none_rects) = (rects(pair), rects(Controls::NONE));
        for i in BTN_SUBTITLES..=BTN_MORE {
            assert_eq!(pair_rects[i as usize], none_rects[i as usize]);
            assert!(pair_rects[i as usize].is_some());
        }
        let previous = pair_rects[BTN_PREVIOUS as usize].unwrap();
        let next = pair_rects[BTN_NEXT as usize].unwrap();
        let subtitles = pair_rects[BTN_SUBTITLES as usize].unwrap();
        assert_eq!(next.x - previous.x - previous.w, plx_ui::widgets::CONTROL_GAP);
        assert_eq!(subtitles.x - next.x - next.w, 2.0 * plx_ui::widgets::CONTROL_GAP);
        assert!(none_rects[BTN_PREVIOUS as usize].is_none());
        assert!(none_rects[BTN_NEXT as usize].is_none());
        assert_eq!(pair.primary(), BTN_NEXT);
        assert_eq!(pair.action(BTN_PREVIOUS), None);
        assert_eq!(pair.icon(BTN_PREVIOUS), Icon::Previous);
        let restarting = Controls::for_item(true, false, true, true);
        assert_eq!(restarting.icon(BTN_PREVIOUS), Icon::Restart);
        assert_eq!(restarting.action(BTN_PREVIOUS), Some(Action::Previous));
    }

    #[test]
    fn episode_pair_is_absent_for_movies_extras_and_single_items() {
        for controls in [Controls::for_item(false, true, true, true),
            Controls::for_item(true, false, false, true)] {
            assert_eq!(controls, Controls::NONE);
            assert_eq!(controls.primary(), BTN_SUBTITLES);
        }
        let last = Controls::for_item(true, true, false, false);
        assert_eq!(last.primary(), BTN_PREVIOUS);
        assert_eq!(last.action(BTN_NEXT), None);
    }
}

/// **Under `builtin:classic`, the row resolved through the compile is the row resolved through `Controls`.**
/// The page now asks the package's control at an index what a press performs and whether it has a stop;
/// for classic's five discs that must be exactly classic's table, for every valuation of the pair and of
/// Previous's restart target, and the cursor parks where `Controls::primary` parks it.
#[test]
fn classic_resolved_through_the_compile_is_classics_table() {
    use super::command::HudCommand;
    use super::model::HudModel;
    use plx_base::fontcov::advances::ShippedMeasure;
    let hud = super::loaded::load_classic("test", plx_platform::i18n::english(), &ShippedMeasure);
    let compiled = &*hud.compiled;
    let model = HudModel::default();
    for episode in [false, true] {
        for previous in [false, true] {
            for next in [false, true] {
                for restart in [false, true] {
                    let controls = Controls::for_item(episode, previous, next, restart);
                    let slot = ControlSlot::Discs(controls);
                    for i in -1..=BTN_N + 2 {
                        let want: Option<HudCommand> = controls.command(i);
                        assert_eq!(row_command(compiled, slot, &model, i), want, "{controls:?} item {i}");
                        assert_eq!(row_enabled(compiled, slot, &model, i), controls.enabled(i), "{controls:?} item {i}");
                    }
                    assert_eq!(row_primary(compiled, slot, &model), controls.primary(), "{controls:?}");
                }
            }
        }
    }
}

/// The playbar's package options (`knob`, `clocks`, `mark`): extended in place, with classic's values
/// the default, so the trailer's bar and the classic HUD's bar are the frames they were.
mod playbar_options {
    use super::*;
    use crate::hud_format::compile::{ClockMode, KnobMode, MarkMode};

    fn bar(knob: Knob, mark: TransportMark) -> Playbar {
        Playbar { pos_ns: 600_000_000_000, dur_ns: 1_200_000_000_000, knob, mark, now: 0, clock_grow: 0.0 }
    }
    fn census(b: Playbar, look: PlaybarLook) -> Vec<(u64, Rect)> {
        plx_ui::draw_census::capture(|| draw_playbar_look(Painter::recording(), b, look, &FM))
    }
    fn runs(b: Playbar, look: PlaybarLook) -> Vec<plx_ui::draw_census::Run> {
        plx_ui::draw_census::capture_runs(|| draw_playbar_look(Painter::recording(), b, look, &FM))
    }

    #[test]
    fn the_default_look_is_classics() {
        let look = PlaybarLook::default();
        assert_eq!((look.knob, look.clocks, look.mark), (KnobMode::Tick, ClockMode::FollowHead, MarkMode::AfterElapsed));
        for (knob, mark) in [(Knob::Tick, TransportMark::Pause), (Knob::Focused, TransportMark::Play), (Knob::Held, TransportMark::None)] {
            let b = bar(knob, mark);
            let classic = plx_ui::draw_census::capture(|| draw_playbar(Painter::recording(), b, &FM));
            assert_eq!(classic, census(b, look), "{knob:?}");
        }
    }

    #[test]
    fn an_always_knob_replaces_the_tick_at_rest() {
        let tick = census(bar(Knob::Tick, TransportMark::None), PlaybarLook::default());
        let always = census(bar(Knob::Tick, TransportMark::None), PlaybarLook { knob: KnobMode::Always, ..PlaybarLook::default() });
        let thin = |v: &[(u64, Rect)]| v.iter().any(|(_, r)| (r.w - 3.0).abs() < 0.01);
        let round = |v: &[(u64, Rect)]| v.iter().any(|(_, r)| (r.w - 18.0).abs() < 0.01 && (r.h - 18.0).abs() < 0.01);
        assert!(thin(&tick) && !round(&tick), "classic rests as a tick: {tick:?}");
        assert!(round(&always) && !thin(&always), "`always` rests as a knob: {always:?}");
        // a knob already shown (focused or held) is unchanged by the option
        for knob in [Knob::Focused, Knob::Held] {
            let b = bar(knob, TransportMark::None);
            assert_eq!(census(b, PlaybarLook::default()), census(b, PlaybarLook { knob: KnobMode::Always, ..PlaybarLook::default() }));
        }
    }

    #[test]
    fn ends_clocks_sit_at_the_bars_two_ends() {
        let look = PlaybarLook { clocks: ClockMode::Ends, ..PlaybarLook::default() };
        let b = bar(Knob::Tick, TransportMark::None);
        let elapsed = |look| runs(b, look).into_iter().find(|r| r.align == 1).expect("the elapsed clock");
        let follow = elapsed(PlaybarLook::default());
        let ends = elapsed(look);
        let l = look.track.x;
        assert!((follow.at.x - (l + look.track.w * 0.5)).abs() < 1.0, "classic: under the playhead, {:?}", follow.at);
        assert!((ends.at.x - ends.at.w * 0.5 - l).abs() < 1.0, "`ends`: the clock's left edge is the bar's start, {:?}", ends.at);
        let rem = runs(b, look).into_iter().find(|r| r.align == 2).expect("the remaining clock");
        assert!((rem.at.x - (l + look.track.w)).abs() < 0.01, "remaining ends at the bar's end");
        // near the end of the item the classic remaining clock gives way; `ends` keeps both apart
        let late = Playbar { pos_ns: 1_199_000_000_000, ..b };
        assert!(runs(late, PlaybarLook::default()).iter().all(|r| r.align != 2), "classic hides it");
        assert!(runs(late, look).iter().any(|r| r.align == 2), "`ends` never meets it");
    }

    #[test]
    fn a_busy_only_mark_draws_the_spinner_and_nothing_else() {
        let glyphs = |b: Playbar, look| plx_ui::draw_census::capture_icons(|| draw_playbar_look(Painter::recording(), b, look, &FM));
        let busy_only = PlaybarLook { mark: MarkMode::BusyOnly, ..PlaybarLook::default() };
        for mark in [TransportMark::Pause, TransportMark::Play, TransportMark::Rewind, TransportMark::FastForward] {
            let b = bar(Knob::Tick, mark);
            assert_eq!(glyphs(b, PlaybarLook::default()).len(), 1, "classic draws the {mark:?} glyph");
            assert!(glyphs(b, busy_only).is_empty(), "`busy_only` suppresses it: {mark:?}");
        }
        let working = bar(Knob::Tick, TransportMark::Working);
        assert_eq!(census(working, busy_only), census(working, PlaybarLook::default()), "the spinner stays");
    }
}
