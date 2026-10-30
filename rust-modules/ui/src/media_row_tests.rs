//! The media row's geometry and ink: where the poster, the text and the chip stand, what focus
//! changes, that an empty line takes no room, that a long title stays inside its column, and that
//! the progress band is drawn only while there is progress to show.
use super::*;
use crate::fixture::FixtureMeasure;

const ROW: Rect = Rect { x: 96.0, y: 300.0, w: 1728.0, h: ROW_H };

fn row<'a>() -> MediaRow<'a> {
    MediaRow {
        title: "Title",
        lines: ["Movie · 2024", "Requested by Alex · 28 Oct", ""],
        chip: "Approved",
        ..Default::default()
    }
}

fn runs(r: MediaRow<'_>) -> Vec<crate::draw_census::Run> {
    crate::draw_census::capture_runs(|| r.draw(Painter::recording(), ROW, &FixtureMeasure))
}

#[test]
fn the_poster_leads_the_text_follows_it_and_the_chip_ends_the_row() {
    let r = row();
    let poster = MediaRow::poster_rect(ROW);
    assert_eq!((poster.x, poster.w, poster.h), (ROW.x + PAD, POSTER_W, POSTER_H));
    assert_eq!(poster.y + poster.h * 0.5, ROW.y + ROW.h * 0.5, "centred on the row");
    let (x, w) = r.text_span(ROW, &FixtureMeasure);
    assert_eq!(x, poster.x + poster.w + GUTTER);
    let chip_x = ROW.x + ROW.w - PAD - r.chip_w(&FixtureMeasure);
    assert_eq!(x + w + GUTTER, chip_x, "the text stops a gutter before the chip");
    let bare = MediaRow { chip: "", ..row() };
    assert_eq!(bare.text_span(ROW, &FixtureMeasure).1, ROW.x + ROW.w - PAD - x, "no chip: the text runs to the pad");
}

#[test]
fn an_empty_line_takes_no_room_and_the_block_stays_centred() {
    let bands = row().bands(ROW, &FixtureMeasure);
    assert!(bands[0].is_some() && bands[1].is_some() && bands[2].is_some() && bands[3].is_none());
    let (top, _) = bands[0].unwrap();
    let (y, h) = bands[2].unwrap();
    assert!(((top + y + h) * 0.5 - (ROW.y + ROW.h * 0.5)).abs() < 0.01, "the drawn block is centred");
    let alone = MediaRow { lines: ["", "", ""], ..row() }.bands(ROW, &FixtureMeasure);
    let (y, h) = alone[0].unwrap();
    assert!((y + h * 0.5 - (ROW.y + ROW.h * 0.5)).abs() < 0.01, "a title alone stands in the middle");
}

#[test]
fn focus_inks_the_row_as_a_table_rows_focus_does() {
    let _g = plx_base::testlock::serial();
    let ink_of = |runs: &[crate::draw_census::Run], text: &str| runs.iter().find(|r| r.text == text).map(|r| r.ink);
    let rest = runs(row());
    assert_eq!(ink_of(&rest, "Title"), Some(theme::TEXT_PRIMARY));
    assert_eq!(ink_of(&rest, "Movie · 2024"), Some(theme::TEXT_SECONDARY));
    let focused = runs(MediaRow { focused: true, ..row() });
    assert_eq!(ink_of(&focused, "Title"), Some(theme::ACCENT_INK));
    assert_eq!(ink_of(&focused, "Requested by Alex · 28 Oct"), Some(theme::ROW_VALUE_INK_ON));
    assert!(focused.iter().any(|r| r.text == "Approved"), "the chip is drawn");
    assert!(focused.iter().find(|r| r.text == "Title").is_some_and(|r| r.bold), "the title is bold");
}

#[test]
fn a_long_title_is_cut_to_its_column() {
    let _g = plx_base::testlock::serial();
    let long = "W".repeat(400);
    let r = MediaRow { title: &long, ..row() };
    let (_, w) = r.text_span(ROW, &FixtureMeasure);
    let drawn = runs(r).into_iter().find(|run| run.text.starts_with('W')).expect("the title is drawn");
    assert!(drawn.text.ends_with('…'), "cut with an ellipsis");
    assert!(FixtureMeasure.width_str(&drawn.text, theme::size::BODY, true) <= w);
}

#[test]
fn the_band_is_drawn_only_while_there_is_progress() {
    let _g = plx_base::testlock::serial();
    let fills = |r: MediaRow<'_>| crate::draw_census::capture(|| r.draw(Painter::recording(), ROW, &FixtureMeasure)).len();
    let still = fills(row());
    assert_eq!(fills(MediaRow { progress: Some(0.5), ..row() }), still + 2, "the amber fill and its track");
    assert_eq!(fills(MediaRow { progress: Some(0.0), ..row() }), still + 1, "nothing done: the track alone");
}
