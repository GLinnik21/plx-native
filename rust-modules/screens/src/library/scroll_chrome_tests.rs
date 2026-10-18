use super::scroll_chrome::*;
use plx_ui::scroll_capsule;

const PITCH: f32 = 480.0;

#[test]
fn the_tuck_is_zero_at_rest_and_one_once_the_first_row_has_gone() {
    assert_eq!(tuck(0.0, PITCH), 0.0);
    assert_eq!(tuck(PITCH - TUCK_RAMP, PITCH), 0.0, "the ramp starts exactly one ramp before");
    assert_eq!(tuck(PITCH, PITCH), 1.0);
    assert_eq!(tuck(PITCH * 5.0, PITCH), 1.0);
    let mid = tuck(PITCH - TUCK_RAMP * 0.5, PITCH);
    assert!(mid > 0.0 && mid < 1.0);
    let mut last = 0.0;
    for s in 0..=PITCH as i32 {
        let t = tuck(s as f32, PITCH);
        assert!(t >= last, "monotonic in scroll");
        last = t;
    }
}

#[test]
fn a_short_pitch_still_leaves_scroll_zero_untucked() {
    assert_eq!(tuck(0.0, 40.0), 0.0);
    assert_eq!(tuck(0.0, 0.0), 0.0);
}

#[test]
fn a_resting_page_has_a_constant_tuck_so_it_asks_for_no_frames() {
    let (a, moving) = plx_machine::idle::scoped_motion(|| tuck(PITCH * 3.0, PITCH));
    assert_eq!(a, 1.0);
    assert!(!moving, "the tuck is derived from the scroll spring; it animates nothing itself");
}

#[test]
fn rows_settle_at_120_or_lower_and_the_clamp_never_pushes_down() {
    assert_eq!(settle_scroll(500.0, 130.0), 500.0, "already clear");
    assert_eq!(settle_scroll(500.0, 120.0), 500.0);
    assert_eq!(settle_scroll(500.0, 100.0), 480.0, "pulled back by the 20 px deficit");
    assert_eq!(settle_scroll(10.0, -50.0), 0.0, "never below the page top");
    assert!(SETTLE_MIN_Y >= scroll_capsule::Y + scroll_capsule::H, "clear of the capsule");
}

#[test]
fn the_capsule_label_is_name_then_count_and_shortens_the_name_not_the_count() {
    let w = |s: &str| s.chars().count() as f32 * 10.0;
    let t = capsule_text("Films", 1284, 320.0, w);
    let t = t.to_str().unwrap();
    assert!(t.starts_with("Films ") && t.ends_with("1,284") || t.ends_with("1\u{a0}284") || t.contains("1284"), "{t}");
    let long = capsule_text("Documentaries and Concerts of the Century", 1284, 200.0, w);
    let long = long.to_str().unwrap();
    assert!(w(long) <= 200.0, "{long}");
    assert!(long.contains('\u{2026}'), "{long}");
    assert!(long.contains("1"), "the count survives: {long}");
}

#[test]
fn the_capsule_label_fits_every_shipped_language_for_a_long_name() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_machine::machine::Measure;
    let _guard = plx_base::testlock::serial();
    let m = ShippedMeasure;
    let w = |s: &str| m.width_str(s, plx_ui::theme::size::BODY, true);
    let budget = scroll_capsule::label_budget() / plx_ui::fit::HEADROOM;
    for language in plx_platform::i18n::SHIPPED {
        let _lang = plx_platform::i18n::language_on_this_thread_for_test(language);
        for name in ["Films", "Television Documentaries and Concerts", "Нарматыўныя дакументальныя фільмы"] {
            let t = capsule_text(name, 12_345, budget, w);
            assert!(w(t.to_str().unwrap()) <= budget, "{language:?} {name}: {t:?}");
        }
    }
}

#[test]
fn the_right_edge_is_the_last_column_of_whatever_the_grid_has() {
    assert!(!on_last_column(None, 6));
    assert!(!on_last_column(Some(0), 6));
    assert!(on_last_column(Some(5), 6));
    assert!(on_last_column(Some(11), 6));
    assert!(on_last_column(Some(3), 4), "the episode grid has four columns");
    assert!(!on_last_column(Some(3), 0));
}

#[test]
fn every_grid_row_rests_clear_of_the_capsule() {
    use super::layout::Layout;
    let lay = Layout::new(false, 400);
    for row in 0..lay.rows {
        let rest = lay.row_y(row, lay.row_reveal(row));
        assert!(rest >= SETTLE_MIN_Y - 0.01 || lay.row_reveal(row) == 0.0, "row {row} rests at {rest}");
    }
}

#[test]
fn the_capsule_field_latches_only_when_tucked() {
    let mut screen = crate::library::LibraryScreen::new(
        crate::library::EntryId(1),
        crate::library::InstanceId(1),
        plx_data::stores::browse::SecKind::Movie,
    );
    assert!(!screen.capsule_field.is_latched());
    assert!(screen.capsule_glass_field().is_none(), "unlatched: the capsule draws its flat form");
    screen.latch_capsule_field(0.0);
    assert!(!screen.capsule_field.is_latched(), "an untucked page latches nothing");
    screen.latch_capsule_field(1.0);
    assert!(screen.capsule_field.is_latched(), "a tucked page stands the capsule on its own frozen ground");
    let handed = screen.capsule_glass_field().expect("latched: draw hands the capsule its own field");
    assert!(std::ptr::eq(handed, &screen.capsule_field), "the capsule stands on the page's own latched field");
    screen.capsule_field.reset();
    assert!(screen.capsule_glass_field().is_none(), "a reset field is handed on no longer");
}

/// The paints one descriptive draw of `page` declares (`frame::backdrop::discover`: primitives are
/// recorded, GL is never reached), as `popover_glass::tests::declared_all` records a menu's.
fn declared_by_a_draw(fixture: &super::tests::Fixture, page: &mut crate::library::LibraryScreen)
    -> Vec<std::rc::Rc<plx_ui::frame::backdrop::Paint>>
{
    use plx_ui::screen::Screen;
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let cx = fixture.cx(None);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints = sources.borrow().paints.clone();
    paints
}

/// **The draw itself hands the capsule the page's frozen ground.** A page scrolled one grid pitch
/// (tuck 1) latches its field on that frame and declares exactly one `glass_frozen` — the capsule
/// on the latched field — and never a `backdrop` (no live blur: owner policy). Back at the top the
/// capsule is gone and the field is re-armed. The `capsule_glass_field` seam is graded above; this
/// is the call site in `draw_page`: hand it `None` and the tucked page stays on the flat tint.
///
/// The host cannot upload the latch (no GL context), so `stand_in_upload_for_test` plays the
/// texture name the device's `upload_rgba` would mint; everything else is the production path.
#[test]
fn a_tucked_page_draws_the_capsule_on_its_own_frozen_ground() {
    let _g = plx_base::testlock::serial();
    let _upload = plx_ui::underlay::stand_in_upload_for_test(9_761);
    let fixture = super::tests::Fixture::new();
    let mut page = fixture.screen();
    let pitch = page.layout.grid_pitch();
    assert_eq!(tuck(pitch, pitch), 1.0);
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    assert!(!page.capsule_field.is_latched(), "nothing latched before the first tucked draw");

    let tucked = declared_by_a_draw(&fixture, &mut page);
    assert!(page.capsule_field.is_latched(), "the tucked draw latches the page's own ground");
    assert_eq!(tucked.iter().filter(|p| p.kind() == "glass_frozen").count(), 1,
        "one frozen glass: the capsule, on the field `draw` hands it");
    assert!(tucked.iter().all(|p| p.kind() != "backdrop"), "never a live blur source");

    page.scroll.jump(0.0);
    page.scroll_target = 0.0;
    let at_rest = declared_by_a_draw(&fixture, &mut page);
    assert!(at_rest.iter().all(|p| p.kind() != "glass_frozen" && p.kind() != "backdrop"),
        "at the top there is no capsule, so no glass");
    assert!(!page.capsule_field.is_latched(), "back at the top the next scrolled stay latches afresh");
}
