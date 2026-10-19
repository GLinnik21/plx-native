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
        ..Default::default()
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
fn neighbouring_text_runs_stand_a_gap_apart_that_covers_the_pill_growth() {
    assert_eq!(GAP, 48.0, "the floor: the board's 53px between visible text edges, less glyph side bearings");
    let l = header("Title").layout(1920.0, &Fixed);
    let (a, b) = (l.items[0].text, l.items[1].text);
    assert_eq!(b.x - (a.x + a.w), gap_between(a.w, b.w));
    assert!(gap_between(a.w, b.w) >= GAP);
}

#[test]
fn values_are_regular_weight_white_and_labels_are_65_percent() {
    let _g = plx_base::testlock::serial();
    let h = header("Title");
    let l = h.layout(1920.0, &Fixed);
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, &TabStrip::new(), 1.0, false));
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
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, &TabStrip::new(), 1.0, false));
    let run = runs.iter().find(|r| r.text == "24 of 27").unwrap();
    assert_eq!(run.ink, theme::COUNT_FILTERED_INK);
    assert_eq!(run.ink[3], 0.5);
    let last = l.items.last().unwrap();
    assert!((last.text.x + last.text.w - 1771.0).abs() < 0.01, "the cluster still ends at the right text edge");
}

/// Does a focused, scaled pill on item `a` touch the plate of item `b` (or the reverse)? Items are
/// adjacent on one row, so the x ranges decide.
fn touches(a: Rect, b: Rect) -> bool {
    let (sa, sb) = (a.scaled(crate::widgets::CTRL_FOCUS_SCALE), b.scaled(crate::widgets::CTRL_FOCUS_SCALE));
    let hit = |p: Rect, q: Rect| p.x < q.x + q.w && q.x < p.x + p.w;
    hit(sa, b) || hit(sb, a)
}

fn real(label: &str, selected: bool) -> HeaderItem {
    HeaderItem { selected_plate: selected, ..item(label, None) }
}

#[test]
fn a_focused_scaled_pill_never_touches_its_neighbours_plate() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let m = ShippedMeasure;
    let spec = PageHeaderSpec {
        left: vec![real("Movies", true), real("LDN Films bamx23", false), real("A Very Long Library Name Indeed", false)],
        overflow: 3,
        right: vec![item("Collections", Some("12")), item("Recommended", None),
            HeaderItem { fold_glyph: true, ..item("Sort & Filter", Some("Date added")) }],
        ..Default::default()
    };
    let l = PageHeader::new(spec, &[], &m).layout(1920.0, &m);
    for pair in l.items.windows(2) {
        let group = |s: Slot| matches!(s, Slot::Right(_));
        // The two groups are kept apart by CLUSTER_GAP, checked below; this is each group's own row.
        if !(pair[0].focusable && pair[1].focusable) || group(pair[0].slot) != group(pair[1].slot) { continue; }
        assert!(!touches(pair[0].plate, pair[1].plate),
            "{:?} and {:?}: a focused pill grown 1.07 reaches its neighbour", pair[0].slot, pair[1].slot);
    }
}

#[test]
fn the_gap_between_two_items_grows_with_the_wider_plate() {
    let narrow = gap_between(60.0, 60.0);
    let wide = gap_between(60.0, 400.0);
    assert!(wide > narrow, "{narrow} {wide}");
    assert_eq!(gap_between(60.0, 400.0), gap_between(400.0, 60.0));
    assert!(narrow >= GAP);
}

#[test]
fn fit_left_and_layout_agree_on_the_gap() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let m = ShippedMeasure;
    let names = ["Movies", "LDN Films bamx23"];
    let spec = PageHeaderSpec { left: names.iter().map(|n| real(n, false)).collect(), ..Default::default() };
    let l = PageHeader::new(spec, &[], &m).layout(1920.0, &m);
    let widths: Vec<f32> = l.items.iter().map(|i| i.text.w).collect();
    let used = widths[0] + gap_between(widths[0], widths[1]) + widths[1];
    assert_eq!(fit_left(&widths, used, 0.0), 2, "exactly fits");
    assert_eq!(fit_left(&widths, used - 1.0, 0.0), 1, "one px short");
    assert_eq!(l.items[1].text.x, l.items[0].text.x + widths[0] + gap_between(widths[0], widths[1]));
}

// ── the row's motion, its crumb configuration and its focus group (T3) ──────────────────────────

const DT: f32 = 1.0 / 60.0;

/// The rounded rects one recording draw of the row paints, in paint order. The selection plate is
/// exactly [`HEIGHT`] tall (it never scales); the focus capsule wears the pop, so it is taller.
fn rrects(page: &PageHeader, layout: &HeaderLayout, capsules: &TabStrip, pop: &CtlPop<1>, lifted: bool) -> Vec<Rect> {
    crate::draw_census::capture(|| page.draw(Painter::recording(), layout, capsules, pop.scale_with(0, 0.0), lifted))
        .into_iter().filter(|(tag, _)| *tag == 2).map(|(_, rect)| rect).collect()
}

fn focus_capsule(rects: &[Rect]) -> Option<Rect> { rects.iter().copied().find(|r| r.h > HEIGHT + 0.5) }
fn selection_plate(rects: &[Rect]) -> Option<Rect> { rects.iter().copied().find(|r| (r.h - HEIGHT).abs() < 0.01) }

/// A row at rest with its focus on item `focused` and its selection on `selected`.
fn settled(layout: &HeaderLayout, selected: i32, focused: i32) -> (TabStrip, CtlPop<1>) {
    let (mut capsules, mut pop) = (TabStrip::new(), CtlPop::<1>::new());
    for _ in 0..180 { PageHeader::step(layout, selected, focused, &mut capsules, &mut pop, DT); }
    (capsules, pop)
}

/// **The focus capsule travels** (owner: "pills have no transitions, look raw"). Focus moving from
/// one item to the next draws the capsule between the two on the way, one step per frame and never
/// backwards, and it comes to rest on the new item's plate.
#[test]
fn the_focus_capsule_travels_between_items_and_never_backwards() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (mut capsules, mut pop) = settled(&layout, -1, 0);
    let (from, to) = (layout.items[0].plate.cx(), layout.items[1].plate.cx());
    let at_rest = focus_capsule(&rrects(&page, &layout, &capsules, &pop, false)).expect("a focused item wears the capsule");
    assert!((at_rest.cx() - from).abs() < 0.5, "settled on item 0");
    assert!((at_rest.h / HEIGHT - crate::widgets::CTRL_FOCUS_SCALE).abs() < 0.001, "the capsule wears the control pop");

    let mut path = Vec::new();
    for _ in 0..90 {
        PageHeader::step(&layout, -1, 1, &mut capsules, &mut pop, DT);
        path.push(focus_capsule(&rrects(&page, &layout, &capsules, &pop, false)).expect("stays drawn").cx());
    }
    assert!(path.windows(2).all(|w| w[1] >= w[0] - 0.01), "never backwards: {path:?}");
    assert!(path.iter().any(|x| *x > from + 4.0 && *x < to - 4.0), "a frame between the two items: {path:?}");
    assert!((path.last().unwrap() - to).abs() < 0.5, "rests on item 1: {path:?}");
    assert!(path[0] < to - 4.0, "the first frame has not arrived: {path:?}");
}

/// **The selection plate lands under the focus capsule** ([`SelMark::Lands`]): pressing a library
/// name selects it with the capsule already on its pill, so the plate is on the new item the
/// first frame instead of crawling out from behind the accent one. A `Travels` strip, as the top
/// bar's, would still be on its way.
#[test]
fn the_selection_plate_lands_on_the_new_item_the_first_frame() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (mut capsules, mut pop) = settled(&layout, 0, 0);
    let (old, new) = (layout.items[0].plate, layout.items[1].plate);
    let rects = rrects(&page, &layout, &capsules, &pop, false);
    assert_eq!(selection_plate(&rects).map(|r| r.x), Some(old.x), "the plate marks item 0");

    let mut travelling = capsules;
    PageHeader::step(&layout, 1, 1, &mut capsules, &mut pop, DT);
    let plate = selection_plate(&rrects(&page, &layout, &capsules, &pop, false)).expect("the plate stays drawn");
    assert_eq!((plate.x, plate.w), (new.x, new.w), "landed on item 1 in one frame");

    travelling.update(1, 1, |i| layout.items.get(i).map(|r| (r.plate.x, r.plate.w)), SelMark::Travels, DT);
    let glide = selection_plate(&rrects(&page, &layout, &travelling, &pop, false)).expect("drawn");
    assert!(glide.x < new.x - 1.0, "the contrast: a Travels plate is still on its way ({} < {})", glide.x, new.x);
}

/// A `lifted` control (a menu has taken it out of the page) wears no focus capsule, while the
/// selection plate stays; and a read-out is never inked by a capsule that crosses it.
#[test]
fn a_lifted_control_wears_no_focus_capsule_but_the_selection_plate_stays() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (capsules, pop) = settled(&layout, 0, 3);
    let on = rrects(&page, &layout, &capsules, &pop, false);
    let off = rrects(&page, &layout, &capsules, &pop, true);
    assert!(focus_capsule(&on).is_some() && focus_capsule(&off).is_none());
    assert_eq!(selection_plate(&on), selection_plate(&off));
}

/// **A breadcrumb is a configuration of the row**: the placement is [`Crumb::layout`]'s, so the
/// pieces stand where the mock has them, only the root is a stop, the page name wears the
/// selected plate, and the right cluster is the same cluster the row would have had.
#[test]
fn a_crumb_lays_out_where_crumb_layout_says_and_only_its_root_takes_focus() {
    let crumb = Crumb::new("Films", "Collections");
    let want = crumb.layout(&Fixed);
    let sort = HeaderItem { fold_glyph: true, ..item("Sort", Some("Title")) };
    let spec = PageHeaderSpec { crumb: Some(crumb), right: vec![sort.clone()], ..Default::default() };
    let page = PageHeader::new(spec, std::slice::from_ref(&sort), &Fixed);
    let layout = page.layout(1920.0, &Fixed);
    let find = |slot: Slot| layout.items.iter().position(|r| r.slot == slot).map(|i| (i, layout.items[i]));

    let (_, root) = find(Slot::CrumbRoot).expect("the root link");
    assert_eq!((root.plate, root.text, root.focusable), (want.root_plate, want.root_text, true));
    let (page_at, name) = find(Slot::CrumbPage).expect("the page name");
    assert_eq!((name.plate, name.text, name.focusable), (want.page_plate, want.page_text, false));
    assert_eq!(layout.glyph, Some(want.glyph), "the chevron stands where the crumb puts it");

    let stops: Vec<Slot> = PageHeader::item_rects(&layout).iter().map(|(i, _)| layout.items[*i].slot).collect();
    assert_eq!(stops, [Slot::CrumbRoot, Slot::Right(0)], "the root and Sort, not the title");
    assert_eq!(page.selected(&layout), page_at as i32, "the page name is the selected plate");
    let alone = PageHeader::new(PageHeaderSpec { right: vec![sort.clone()], ..Default::default() }, std::slice::from_ref(&sort), &Fixed)
        .layout(1920.0, &Fixed);
    assert_eq!(find(Slot::Right(0)).unwrap().1.plate, alone.items[0].plate, "the right cluster does not move for a crumb");
}

fn row_of(page: PageHeader, layout: HeaderLayout, stops: &[(u32, usize)]) -> HeaderRow {
    HeaderRow::new(page, layout, stops.to_vec())
}

fn from_x(x: f32) -> Placed {
    let rect = Rect::new(x - 10.0, 700.0, 20.0, 20.0);
    Placed { rect, rest_rect: rect, clip: Rect::new(0.0, 0.0, 1920.0, 1080.0), index: None }
}

/// **`HeaderFocus::seat`**: coming into the row, the stop seated is the one wearing the selected
/// plate (Up from the grid lands on the library on view), and only a row with no selected stop
/// falls back to the stop nearest the key that came up from below. One rule for every page.
#[test]
fn the_header_focus_seats_the_selected_stop_then_the_nearest_one() {
    const ENTRY: EntryId = EntryId(7);
    let selected_at = |selected: Option<usize>| {
        let mut spec = header("Title").spec().clone();
        spec.left[0].selected_plate = selected == Some(0);
        spec.left[1].selected_plate = selected == Some(1);
        let widest = [item("Collections", Some("9,999")), item("Recommended", None), HeaderItem { fold_glyph: true, ..item("Sort", Some("Date added")) }];
        let page = PageHeader::new(spec, &widest, &Fixed);
        let layout = page.layout(1920.0, &Fixed);
        // Films, Shows, then Collections / Recommended / Sort: elements 10.. in focus order.
        row_of(page, layout, &[(10, 0), (11, 1), (12, 3), (13, 4), (14, 5)])
    };
    let clip = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    let seat = |row: &HeaderRow, x: f32| row.focus(ENTRY, GroupId(3), (0.0, 0.0), clip).seat(from_x(x)).elem;

    let row = selected_at(Some(1));
    assert_eq!(row.selected_stop(), Some(11));
    assert_eq!(seat(&row, 1700.0), 11, "the selected name wins over the control nearest the key");
    assert_eq!(seat(&row, 90.0), 11, "...and over the name nearest it");

    let row = selected_at(None);
    assert_eq!(row.selected_stop(), None);
    assert_eq!(seat(&row, 1760.0), 14, "no selection: the nearest stop by x, Sort on the right");
    assert_eq!(seat(&row, 100.0), 10, "...Films on the left");
}

#[test]
fn the_header_focus_walks_its_stops_without_wrapping_and_reconciles_to_a_stop() {
    const ENTRY: EntryId = EntryId(7);
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let row = row_of(page, layout, &[(10, 0), (11, 1), (12, 3)]);
    let focus = row.focus(ENTRY, GroupId(3), (0.0, 20.0), Rect::new(0.0, 0.0, 1920.0, 1080.0));
    let key = |elem| FocusKey { entry: ENTRY, elem };
    assert!(matches!(focus.neighbour(key(10), Dir::Right), Step::Move(to) if to == key(11)));
    assert!(matches!(focus.neighbour(key(11), Dir::Left), Step::Move(to) if to == key(10)));
    assert!(matches!(focus.neighbour(key(10), Dir::Left), Step::Edge), "no wrap at the left end");
    assert!(matches!(focus.neighbour(key(12), Dir::Right), Step::Edge), "no wrap at the right end");
    assert!(matches!(focus.neighbour(key(11), Dir::Down), Step::Edge), "Up and Down are the page's links");

    let spec = focus.group_spec().expect("a row with stops declares a group");
    assert_eq!((spec.len, spec.id), (3, GroupId(3)));
    assert_eq!(focus.reconcile(key(11)), Some(key(11)));
    assert_eq!(focus.reconcile(key(99)), None, "an element that is no stop falls back to the page's seat");
    assert_eq!(focus.reconcile(FocusKey { entry: EntryId(8), elem: 11 }), None, "another page's key is not ours");
    let drawn = focus.place(11, At::Drawn).unwrap().rect;
    let target = focus.place(11, At::SpringTarget).unwrap().rect;
    assert_eq!(drawn.y - target.y, 20.0, "the row rides the page's scroll: drawn 0, target 20 is 20 px apart");
}
