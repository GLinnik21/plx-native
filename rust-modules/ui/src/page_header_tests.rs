use super::*;
use std::ffi::CStr;

struct Fixed;
impl Measure for Fixed {
    fn width(&self, s: &CStr, _: i32, bold: bool) -> f32 { s.to_bytes().len() as f32 * if bold { 10.0 } else { 8.0 } }
    fn cap_h(&self, _: i32) -> f32 { 20.0 }
    fn line_h(&self, _: i32) -> f32 { 36.0 }
}

fn item(label: &str, value: Option<&str>) -> HeaderItem {
    HeaderItem { label: label.into(), value: value.map(Into::into), focusable: true, ..Default::default() }
}

fn header(value: &str) -> PageHeader {
    let spec = PageHeaderSpec {
        left: vec![item("Films", None), item("Shows", None)],
        overflow: 2,
        right: vec![item("Collections", Some("12")), item("Recommended", None), HeaderItem { fold_glyph: true, ..item("Sort", Some(value)) }],
    };
    let widest = [item("Collections", Some("9,999")), item("Recommended", None), HeaderItem { fold_glyph: true, ..item("Sort", Some("Date added")) }];
    PageHeader::new(spec, &widest, &Fixed)
}

#[test]
fn text_edges_are_96_and_1771_and_plates_overhang_20() {
    let l = header("Date added").layout(1920.0, &Fixed);
    let first = l.items.first().unwrap();
    let last = l.items.last().unwrap();
    assert_eq!(first.text.x, 96.0);
    assert_eq!(first.plate.x, 76.0);
    assert!((last.text.x + last.text.w - 1771.0).abs() < 0.01, "{:?}", last.text);
    assert!((last.plate.x + last.plate.w - 1791.0).abs() < 0.01);
    assert_eq!((first.text.y, first.text.h), (148.0, 56.0));
}

#[test]
fn right_cluster_width_does_not_change_with_the_value() {
    let a = header("Title").layout(1920.0, &Fixed);
    let b = header("Date added").layout(1920.0, &Fixed);
    assert_eq!(a.cluster_w, b.cluster_w);
    assert_eq!(a.left_budget, b.left_budget);
}

#[test]
fn overflow_is_an_item_between_the_names_and_the_controls_and_read_outs_take_no_focus() {
    let h = header("Title");
    let l = h.layout(1920.0, &Fixed);
    assert_eq!(l.items[2].slot, Slot::Overflow);
    assert_eq!(l.items[3].slot, Slot::Right(0));
    let mut spec = h.spec().clone();
    spec.right.insert(0, HeaderItem { focusable: false, ..item("312 of 1,284", None) });
    let l = PageHeader::new(spec, &[], &Fixed).layout(1920.0, &Fixed);
    assert!(PageHeader::item_rects(&l).iter().all(|(i, _)| l.items[*i].slot != Slot::Right(0)));
}

#[test]
fn names_that_do_not_fit_fold_into_plus_n_and_all_fit_when_there_is_room() {
    let widths = [100.0, 100.0, 100.0, 100.0];
    assert_eq!(fit_left(&widths, 10_000.0, 40.0), 4);
    // 4 names = 400 + 3*GAP; a budget of 300 leaves room for 1 name once "+N" (40) is reserved.
    let n = fit_left(&widths, 300.0, 40.0);
    assert!(n < 4 && n >= 1, "{n}");
    let used: f32 = widths[..n].iter().sum::<f32>() + GAP * (n - 1) as f32;
    assert!(used + 40.0 <= 300.0);
}

#[test]
fn focus_pill_grows_by_the_control_scale() {
    let l = header("Title").layout(1920.0, &Fixed);
    let plate = l.items[0].plate;
    let pill = plate.scaled(crate::widgets::CTRL_FOCUS_SCALE);
    assert!((pill.w / plate.w - 1.07).abs() < 0.001);
}

#[test]
fn neighbouring_text_runs_stand_48_apart() {
    assert_eq!(GAP, 48.0, "the board's 53px between visible text edges, less glyph side bearings");
    let l = header("Title").layout(1920.0, &Fixed);
    let (a, b) = (l.items[0].text, l.items[1].text);
    assert_eq!(b.x - (a.x + a.w), GAP);
}

#[test]
fn values_are_regular_weight_white_and_labels_are_65_percent() {
    let _g = plx_base::testlock::serial();
    let h = header("Title");
    let l = h.layout(1920.0, &Fixed);
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, None));
    let value = runs.iter().find(|r| r.text == "Title").expect("the sort value is drawn");
    assert!(!value.bold, "no bold anywhere in the row");
    assert_eq!(value.ink, [1.0, 1.0, 1.0, 1.0], "values at 100% white");
    let label = runs.iter().find(|r| r.text == "Collections").expect("the label is drawn");
    assert!(!label.bold);
    assert_eq!(label.ink, [1.0, 1.0, 1.0, 0.65], "labels at 65% white");
    assert!(runs.iter().all(|r| !r.bold && r.size == theme::size::BODY), "one size, one weight");
}

#[test]
fn the_filtered_readout_is_half_white_and_64_left_of_collections() {
    let _g = plx_base::testlock::serial();
    let mut spec = header("Title").spec().clone();
    spec.right.insert(0, HeaderItem { readout: true, ..item("24 of 27", None) });
    let h = PageHeader::new(spec, &[], &Fixed);
    let l = h.layout(1920.0, &Fixed);
    let (readout, collections) = (l.items[3].text, l.items[4].text);
    assert_eq!(collections.x - (readout.x + readout.w), READOUT_GAP);
    assert_eq!(READOUT_GAP, 64.0);
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, None));
    let run = runs.iter().find(|r| r.text == "24 of 27").unwrap();
    assert_eq!(run.ink, theme::COUNT_FILTERED_INK);
    assert_eq!(run.ink[3], 0.5);
    let last = l.items.last().unwrap();
    assert!((last.text.x + last.text.w - 1771.0).abs() < 0.01, "the cluster still ends at the right text edge");
}
