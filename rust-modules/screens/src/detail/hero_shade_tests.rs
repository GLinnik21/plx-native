//! Detail's continuous shade at rest, during trailer preview and as the hero fades out.

use super::*;

fn titled(sid: ServerId) -> Detail {
    let mut d = detail(sid, "show");
    d.title = "A Test Title".into();
    d
}

#[test]
fn detail_shade_regression_credits_do_not_create_a_second_patch() {
    let sid = ServerId::UNSET;
    let mut d = titled(sid);
    d.cast.push(plx_data::metadata::Cast {
        tag: "An Actor".into(), role: String::new(), thumb: String::new(), id: 0, tag_key: String::new(),
    });
    let guard = install(d);
    let screen = bare(&guard, sid, "show");
    let meta = test_store().view();
    let measure = &plx_ui::fixture::FixtureMeasure;
    let y = screen.hero_chain(measure, meta).btn_y - theme::space::LG;
    let shade = screen.hero_shade(1.0);
    let mut previous = shade.alpha(0.0, y);
    for x in (20..=1920).step_by(20) {
        let alpha = shade.alpha(x as f32, y);
        assert!(alpha <= previous + 0.001, "the shade darkened at x={x}: {previous} -> {alpha}");
        previous = alpha;
    }
    assert!(previous > 0.5, "the lower shade also protects the credits");
    clear();
}

#[test]
fn the_edge_shade_rises_with_the_preview_and_clears_for_full_trailer() {
    let sid = ServerId::UNSET;
    let guard = install(titled(sid));
    let mut screen = bare(&guard, sid, "show");
    let x = plx_ui::detail_layout::PREVIEW_LOGO_X;
    let y = plx_ui::detail_layout::PREVIEW_LOGO_Y;
    let mut previous = screen.hero_shade(1.0).alpha(x, y);
    assert_eq!(previous, 0.0);
    for t in [0.25, 0.5, 0.75, 1.0] {
        screen.preview_logo = Spring::at(t);
        let alpha = screen.hero_shade(1.0).alpha(x, y);
        assert!(alpha >= previous, "the fade should follow the logo at {t}");
        previous = alpha;
    }
    assert!(previous > 0.4);
    screen.preview_field = 0.0;
    screen.preview_base_scrim = 0.0;
    let shade = screen.hero_shade(1.0);
    assert_eq!(shade.alpha(x, y), 0.0);
    assert_eq!(shade.alpha(x, 1000.0), 0.0);
    clear();
}
