use super::*;
use std::ffi::CStr;
use crate::widgets::focus_pill;

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

/// The focusable items' slots in focus order: the stops [`HeaderFocus`] seats, read off the layout
/// (a read-out or a lone title carries no stop).
fn stops(layout: &HeaderLayout) -> Vec<Slot> {
    layout.items.iter().filter(|r| r.focusable).map(|r| r.slot).collect()
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
    assert!(stops(&l).iter().all(|slot| *slot != Slot::Right(0)));
}

/// **S4.** A header pill grows by a CONSTANT outset past its plate, whatever its label: the pill
/// of "Movies" and of a long library name stand out by the same px (a scale made it 22 px a side
/// for one and 45 for the other), and the first pill never reaches further left than the
/// outset past the first plate.
#[test]
fn the_focus_pill_stands_a_constant_outset_past_its_plate_whatever_the_label_is() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let m = ShippedMeasure;
    let spec = PageHeaderSpec {
        left: vec![real("Movies", true), real("A Very Long Library Name Indeed Of Many Words", false)],
        ..Default::default()
    };
    let page = PageHeader::new(spec, &[], &m);
    let layout = page.layout(1920.0, &m);
    assert!(layout.items[1].plate.w > 3.0 * layout.items[0].plate.w, "the two plates are very different widths");
    for focused in 0..2 {
        let (capsules, pop) = settled(&layout, -1, focused);
        let cap = focus_capsule(&rrects(&page, &layout, &capsules, &pop, None)).expect("a focused item wears the capsule");
        let plate = layout.items[focused as usize].plate;
        assert!((plate.x - cap.x - PILL_FOCUS_OUTSET).abs() < 0.05, "item {focused}: left outset {}", plate.x - cap.x);
        assert!((cap.x + cap.w - (plate.x + plate.w) - PILL_FOCUS_OUTSET).abs() < 0.05, "item {focused}: right outset");
        assert!((plate.y - cap.y - PILL_FOCUS_OUTSET).abs() < 0.05 && (cap.h - plate.h - 2.0 * PILL_FOCUS_OUTSET).abs() < 0.05,
            "item {focused}: the same outset above and below");
        let pill = focus_pill(plate);
        assert!((pill.x - cap.x).abs() < 0.05 && (pill.w - cap.w).abs() < 0.05, "item {focused}: focus_pill is the drawn extent");
    }
    let (capsules, pop) = settled(&layout, -1, 0);
    let cap = focus_capsule(&rrects(&page, &layout, &capsules, &pop, None)).unwrap();
    assert!(cap.x >= TEXT_X0 - OVERHANG - PILL_FOCUS_OUTSET - 0.05, "no pill reaches past the first plate's outset: {}", cap.x);
}

/// **S9.** Neighbouring text runs stand `GAP` apart, and `GAP` is the focused pill's extent plus
/// [`theme::space::SM`] of air before the neighbour's plate, for any widths.
#[test]
fn neighbouring_text_runs_stand_a_gap_apart_that_leaves_sm_of_air_past_the_focused_pill() {
    assert_eq!(GAP, 2.0 * OVERHANG + PILL_FOCUS_OUTSET + theme::space::SM, "plate, outset, air, plate");
    let l = header("Title").layout(1920.0, &Fixed);
    let (a, b) = (l.items[0].text, l.items[1].text);
    assert_eq!(b.x - (a.x + a.w), GAP);
}

fn real(label: &str, selected: bool) -> HeaderItem {
    HeaderItem { selected_plate: selected, ..item(label, None) }
}

/// The air between item `a`'s focused pill and the plate of the item `b` after it, in px, and the
/// reverse (`b`'s pill and `a`'s plate): both are `SM`, whatever the names are.
#[test]
fn a_focused_pill_leaves_sm_of_air_before_its_neighbours_plate() {
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
        let (a, b) = (focus_pill(pair[0].plate), focus_pill(pair[1].plate));
        let air = |pill: Rect, plate: Rect| (plate.x - (pill.x + pill.w)).max(pill.x - (plate.x + plate.w));
        for (air, who) in [(air(a, pair[1].plate), "after"), (air(b, pair[0].plate), "before")] {
            assert!((air - theme::space::SM).abs() < 0.01,
                "{:?} and {:?}: {air} px of air {who} the focused pill", pair[0].slot, pair[1].slot);
        }
    }
}

#[test]
fn values_are_regular_weight_white_and_labels_are_65_percent() {
    let _g = plx_base::testlock::serial();
    let h = header("Title");
    let l = h.layout(1920.0, &Fixed);
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, &TabStrip::new(), 1.0, None));
    let value = runs.iter().find(|r| r.text == "Title").expect("the sort value is drawn");
    assert!(!value.bold, "no bold anywhere in the row");
    assert_eq!(value.ink, [1.0, 1.0, 1.0, 1.0], "values at 100% white");
    let label = runs.iter().find(|r| r.text == "Collections").expect("the label is drawn");
    assert!(!label.bold);
    assert_eq!(label.ink, [1.0, 1.0, 1.0, 0.65], "labels at 65% white");
    assert!(runs.iter().all(|r| !r.bold && r.size == theme::size::BODY), "one size, one weight");
}

#[test]
fn the_filtered_readout_is_half_white_and_one_gap_left_of_collections() {
    let _g = plx_base::testlock::serial();
    let mut spec = header("Title").spec().clone();
    spec.right.insert(0, HeaderItem { readout: true, ..item("24 of 27", None) });
    let h = PageHeader::new(spec, &[], &Fixed);
    let l = h.layout(1920.0, &Fixed);
    let (readout, collections) = (l.items[3].text, l.items[4].text);
    assert_eq!(collections.x - (readout.x + readout.w), GAP, "a read-out stands the row's one gap before the control after it");
    let runs = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, &TabStrip::new(), 1.0, None));
    let run = runs.iter().find(|r| r.text == "24 of 27").unwrap();
    assert_eq!(run.ink, theme::COUNT_FILTERED_INK);
    assert_eq!(run.ink[3], 0.5);
    let last = l.items.last().unwrap();
    assert!((last.text.x + last.text.w - 1771.0).abs() < 0.01, "the cluster still ends at the right text edge");
}

/// The layout stands names one `GAP` apart, the gap the Library's `pack_names` budgets with
/// (`library::header::pack_names`, the rule the product packs by).
#[test]
fn names_stand_one_gap_apart() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let m = ShippedMeasure;
    let names = ["Movies", "LDN Films bamx23"];
    let spec = PageHeaderSpec { left: names.iter().map(|n| real(n, false)).collect(), ..Default::default() };
    let l = PageHeader::new(spec, &[], &m).layout(1920.0, &m);
    let widths: Vec<f32> = l.items.iter().map(|i| i.text.w).collect();
    assert_eq!(l.items[1].text.x, l.items[0].text.x + widths[0] + GAP);
}

// ── the row's motion, its crumb configuration and its focus group (T3) ──────────────────────────

const DT: f32 = 1.0 / 60.0;

/// The rounded rects one recording draw of the row paints, in paint order. The selection plate is
/// exactly [`HEIGHT`] tall (it never scales); the focus capsule wears the pop, so it is taller.
fn rrects(page: &PageHeader, layout: &HeaderLayout, capsules: &TabStrip, pop: &CtlPop<1>, lifted: Option<usize>) -> Vec<Rect> {
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
    let at_rest = focus_capsule(&rrects(&page, &layout, &capsules, &pop, None)).expect("a focused item wears the capsule");
    assert!((at_rest.cx() - from).abs() < 0.5, "settled on item 0");
    assert!((at_rest.h - HEIGHT - 2.0 * PILL_FOCUS_OUTSET).abs() < 0.01, "the capsule wears the focus outset");

    let mut path = Vec::new();
    for _ in 0..90 {
        PageHeader::step(&layout, -1, 1, &mut capsules, &mut pop, DT);
        path.push(focus_capsule(&rrects(&page, &layout, &capsules, &pop, None)).expect("stays drawn").cx());
    }
    assert!(path.windows(2).all(|w| w[1] >= w[0] - 0.01), "never backwards: {path:?}");
    assert!(path.iter().any(|x| *x > from + 4.0 && *x < to - 4.0), "a frame between the two items: {path:?}");
    assert!((path.last().unwrap() - to).abs() < 0.5, "rests on item 1: {path:?}");
    assert!(path[0] < to - 4.0, "the first frame has not arrived: {path:?}");
}

/// **The selection plate travels to the newly selected item, under the focus capsule** (design
/// review S5: "OK moves the plate"): pressing a library name selects it with the capsule already
/// on its pill, and the plate follows on the same plate spring instead of being CUT onto it in
/// one frame. Every frame between is drawn: it never goes backwards, no frame covers more than a
/// fraction of the way, it is on its way on the first frame and rests on the new plate.
#[test]
fn the_selection_plate_travels_to_the_new_item_and_is_never_cut() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (mut capsules, mut pop) = settled(&layout, 0, 0);
    let (old, new) = (layout.items[0].plate, layout.items[1].plate);
    let rects = rrects(&page, &layout, &capsules, &pop, None);
    assert_eq!(selection_plate(&rects).map(|r| r.x), Some(old.x), "the plate marks item 0");

    let mut path = Vec::new();
    for _ in 0..90 {
        PageHeader::step(&layout, 1, 1, &mut capsules, &mut pop, DT);
        path.push(selection_plate(&rrects(&page, &layout, &capsules, &pop, None)).expect("the plate stays drawn").cx());
    }
    let (from, to) = (old.cx(), new.cx());
    let hop = to - from;
    assert!(path[0] < to - 0.5 * hop, "the first frame is on its way, not landed: {path:?}");
    assert!(path.windows(2).all(|w| w[1] >= w[0] - 0.01), "never backwards: {path:?}");
    assert!(path.windows(2).all(|w| w[1] - w[0] < 0.6 * hop), "no frame is a cut: {path:?}");
    assert!(path.iter().any(|x| *x > from + 4.0 && *x < to - 4.0), "a frame between the two items: {path:?}");
    assert!((path.last().unwrap() - to).abs() < 0.5, "rests on item 1: {path:?}");
}

/// A `lifted` control (a menu has taken it out of the page) wears no focus capsule, while the
/// selection plate stays; and a read-out is never inked by a capsule that crosses it.
#[test]
fn a_lifted_control_wears_no_focus_capsule_but_the_selection_plate_stays() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (capsules, pop) = settled(&layout, 0, 3);
    let on = rrects(&page, &layout, &capsules, &pop, None);
    let off = rrects(&page, &layout, &capsules, &pop, Some(3));
    assert!(focus_capsule(&on).is_some() && focus_capsule(&off).is_none());
    assert_eq!(selection_plate(&on), selection_plate(&off));
}

/// A menu leaves a lifted control NOTHING on the page (one rule for every opener, design review
/// S12): no capsule and no label, because [`PageHeader::draw_opener`] is its only copy. A label left
/// behind sat dimmed under the scrim next to the lit one.
#[test]
fn a_lifted_control_leaves_no_label_on_the_page() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (capsules, pop) = settled(&layout, 0, 3);
    let words = |lifted: Option<usize>| crate::draw_census::capture_text(|| {
        page.draw(Painter::recording(), &layout, &capsules, pop.scale_with(0, 0.0), lifted)
    }).into_iter().map(|(text, _)| text).collect::<Vec<_>>();
    let kept = words(None);
    assert!(kept.iter().any(|t| t == "Collections"), "{kept:?}");
    let away = words(Some(3));
    assert!(!away.iter().any(|t| t == "Collections"), "the opener lift draws it: {away:?}");
    assert_eq!(away.len() + 3, kept.len(), "only that item's label, separator and value went");
}

/// **S10: a title is plain text.** A [`HeaderItem::title`] wears no selection plate (the row has no
/// selected item), takes no focus, and stands at full white, not a resting control's 65%; it takes
/// the room a plate-wearing label would, so swapping the one for the other moves nothing.
#[test]
fn a_title_is_full_white_plain_text_with_no_plate_and_no_stop() {
    let _g = plx_base::testlock::serial();
    let sort = HeaderItem { fold_glyph: true, focusable: true, ..item("Sort", Some("Title")) };
    let spec = |left: HeaderItem| PageHeaderSpec { left: vec![left], right: vec![sort.clone()], ..Default::default() };
    let titled = PageHeader::new(spec(HeaderItem { title: true, ..item("Watchlist", None) }), std::slice::from_ref(&sort), &Fixed);
    let plated = PageHeader::new(spec(real("Watchlist", true)), std::slice::from_ref(&sort), &Fixed);
    let layout = titled.layout(1920.0, &Fixed);
    let boxes = |l: &HeaderLayout| l.items.iter().map(|r| (r.text, r.plate)).collect::<Vec<_>>();
    assert_eq!(boxes(&layout), boxes(&plated.layout(1920.0, &Fixed)), "the title takes the room a plated label takes");
    assert_eq!(titled.selected(&layout), -1, "no item wears the plate");
    assert_eq!(stops(&layout), [Slot::Right(0)], "the title is not a stop");
    let (capsules, pop) = settled(&layout, titled.selected(&layout), 1);
    let rects = rrects(&titled, &layout, &capsules, &pop, None);
    assert!(selection_plate(&rects).is_none(), "no plate under the title: {rects:?}");
    let runs = crate::draw_census::capture_runs(|| titled.draw(Painter::recording(), &layout, &capsules, 1.0, None));
    assert_eq!(runs.iter().find(|r| r.text == "Watchlist").expect("the title is drawn").ink, [1.0; 4]);
    let plated_runs = crate::draw_census::capture_runs(|| plated.draw(Painter::recording(), &layout, &TabStrip::new(), 1.0, None));
    assert_eq!(plated_runs.iter().find(|r| r.text == "Watchlist").unwrap().ink, [1.0, 1.0, 1.0, 0.65], "a plain control label stays at 65%");
}

/// **The opener, lit** (S12): `draw_opener` paints the control as the focused one: the accent pill
/// stood out by the focus outset over its plate, and every run of its face in the focused ink.
#[test]
fn the_opener_is_painted_as_the_focused_control() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (_, pop) = settled(&layout, 0, 3);
    let plate = layout.items[3].plate;
    let scale = pop.scale_with(0, 0.0);
    let rects = crate::draw_census::capture(|| page.draw_opener(Painter::recording(), &layout, 3, scale))
        .into_iter().filter(|(tag, _)| *tag == 2).map(|(_, r)| r).collect::<Vec<_>>();
    let want = focus_pill(plate);
    assert!(rects.len() == 1 && (rects[0].x - want.x).abs() < 0.01 && (rects[0].y - want.y).abs() < 0.01
        && (rects[0].w - want.w).abs() < 0.01 && (rects[0].h - want.h).abs() < 0.01,
        "one accent pill, at the focused pill's extent: {rects:?} vs {want:?}");
    let runs = crate::draw_census::capture_runs(|| page.draw_opener(Painter::recording(), &layout, 3, scale));
    assert!(runs.iter().any(|run| run.text == "Collections"), "{runs:?}");
    assert!(runs.iter().all(|run| run.ink.iter().zip(crate::ACCENT_INK).all(|(a, b)| (a - b).abs() < 0.01)), "the focused ink on every run: {runs:?}");
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

    assert_eq!(stops(&layout), [Slot::CrumbRoot, Slot::Right(0)], "the root and Sort, not the title");
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
    assert_eq!((focus.reveal(11), focus.reveal(99)), (Some(0.0), None), "reaching a stop scrolls the page to its top");
    let drawn = focus.place(11, At::Drawn).unwrap().rect;
    let target = focus.place(11, At::SpringTarget).unwrap().rect;
    assert_eq!(drawn.y - target.y, 20.0, "the row rides the page's scroll: drawn 0, target 20 is 20 px apart");
}

/// **A surface above the page holds the row's motion.** While a Sort & Filter menu is up the
/// engine's focus is on the menu's own rows (another entry), and the control is lifted out of the
/// page. The row's capsule must neither fade nor close its pop meanwhile, or the menu's closing
/// morph would hand over to a capsule that has to fade back in; focus leaving the row for the
/// page's own content (or nowhere) still closes it.
#[test]
fn a_surface_above_the_page_holds_the_rows_motion_but_focus_leaving_it_closes_it() {
    const ENTRY: EntryId = EntryId(7);
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let row = row_of(header("Title"), layout.clone(), &[(10, 0), (11, 1)]);
    let alpha_after = |current: Option<FocusKey<u32>>| {
        let (mut capsules, mut pop) = settled(&layout, -1, 1);
        for _ in 0..60 { row.step(ENTRY, current, &mut capsules, &mut pop, DT); }
        focus_capsule(&rrects(&page, &layout, &capsules, &pop, None)).map(|r| r.h)
    };
    let held = alpha_after(Some(FocusKey { entry: EntryId(8), elem: 0 }));
    assert_eq!(held.map(|h| (h * 1000.0).round()), Some(((HEIGHT + 2.0 * PILL_FOCUS_OUTSET) * 1000.0).round()),
        "the menu's rows hold focus: the capsule stays up, at its pop");
    assert_eq!(alpha_after(Some(FocusKey { entry: ENTRY, elem: 11 })).map(|h| (h * 1000.0).round()), held.map(|h| (h * 1000.0).round()),
        "focus on the row's own stop is the same picture");
    assert_eq!(alpha_after(Some(FocusKey { entry: ENTRY, elem: 99 })), None, "focus on the page's content closes the capsule");
    assert_eq!(alpha_after(None), None, "focus nowhere closes it too");
}

/// **The row's motion is decoration, not layout.** A page-freeze holds a pushed page's snapshot
/// until its LAYOUT stops moving so the first posters can settle behind it; the header's capsules
/// and pop move nothing but themselves, so they wake the present gate (`page_moving`) and never
/// hold that snapshot (`page_layout_moving`): a warm Library push read as held for its whole
/// length when the selection plate's landing counted.
#[test]
fn the_rows_motion_wakes_the_gate_but_never_holds_a_page_freeze() {
    let page = header("Title");
    let layout = page.layout(1920.0, &Fixed);
    let (mut capsules, mut pop) = (TabStrip::new(), CtlPop::<1>::new());
    let mut frames = 0;
    let mut layout_moved = false;
    let mut page_moved = false;
    for _ in 0..90 {
        plx_machine::idle::frame_begin(DT);
        PageHeader::step(&layout, 0, 1, &mut capsules, &mut pop, DT);
        layout_moved |= plx_machine::idle::page_layout_moving();
        page_moved |= plx_machine::idle::page_moving();
        frames += 1;
    }
    assert_eq!(frames, 90);
    assert!(page_moved, "the capsule's travel is visible motion: it wakes the present gate");
    assert!(!layout_moved, "and it is decoration: it never counts toward the page's layout motion");
}

/// **A header row is built once per key, whoever holds it.** The rule every page holds its row by
/// (the Library, its Collections and Recommended pages, the Watchlist): the row stays while the key
/// stands, a new key builds exactly one, and going back to an old key is a new key, not a restore.
#[test]
fn a_row_cache_builds_when_its_key_changes_and_never_otherwise() {
    let mut cache = RowCache::<u32>::new();
    assert_eq!((cache.row(), cache.key(), cache.builds()), (None, None, 0), "nothing built before the first ask");
    assert!(cache.refresh(7, || 70), "the first ask builds");
    for _ in 0..50 {
        assert!(!cache.refresh(7, || unreachable!("the key stood: the row is not built again")));
        assert!(cache.is_current(7));
    }
    assert_eq!((cache.row(), cache.key(), cache.builds()), (Some(&70), Some(7), 1));
    assert!(cache.refresh(8, || 80), "a changed key builds");
    assert!(cache.refresh(7, || 71), "the old key is not remembered: it builds again");
    assert_eq!((cache.row(), cache.builds()), (Some(&71), 3));
    // `install` is the split form for a builder that needs the page mutably (the Library's).
    assert!(!cache.is_current(9));
    cache.install(9, 90);
    assert!(cache.is_current(9));
    assert_eq!(cache.builds(), 4);
}

#[test]
fn a_shared_row_hands_one_row_to_every_reader_until_the_key_changes() {
    let row = SharedRow::<String>::new();
    let first = row.get(1, || "one".to_owned());
    for _ in 0..50 {
        let again = row.get(1, || unreachable!("the key stood"));
        assert!(std::rc::Rc::ptr_eq(&first, &again), "every reader gets the row that was built");
    }
    assert_eq!(row.builds(), 1);
    let next = row.get(2, || "two".to_owned());
    assert_eq!((next.as_str(), first.as_str(), row.builds()), ("two", "one", 2), "a reader that holds the old row keeps it");
}

/// The right cluster with a slotted Collections control holding `value`, and its widest case.
fn slotted(value: Option<&str>, readout: bool) -> (PageHeaderSpec, Vec<HeaderItem>) {
    let collections = |value: Option<&str>| HeaderItem { value_slot: Some("9,999".into()), ..item("Collections", value) };
    let mut right = vec![HeaderItem { value: value.map(Into::into), ..collections(None) }, item("Recommended", None)];
    let mut widest = vec![collections(Some("9,999")), item("Recommended", None)];
    if readout {
        right.insert(0, HeaderItem { readout: true, focusable: false, ..item("24 of 27", None) });
        widest.insert(0, right[0].clone());
    }
    (PageHeaderSpec { right, ..Default::default() }, widest)
}

fn row_with(value: Option<&str>) -> HeaderRow {
    let (spec, widest) = slotted(value, false);
    let page = PageHeader::new(spec, &widest, &Fixed);
    let layout = page.layout(1920.0, &Fixed);
    row_of(page, layout, &[(1, 0), (2, 1)])
}

/// **A slotted control is placed by what it DRAWS, so the row has one gap** (the owner's photo: a
/// wide gap before "Recommended" and a narrow one after it). The slot is the widest the count ever
/// is and sizes the cluster's BUDGET (`cluster_w`, `left_budget`: the room the names may use never
/// changes with the count); it is no air inside the row. Whatever the control holds, the next item
/// stands exactly one [`GAP`] from the ink, the cluster ends at [`TEXT_X1`], the items after the
/// control do not move (the cluster is anchored at its right edge), and the plate hugs the text.
/// The accepted cost: a count of another digit count moves the control's own label by that digit.
/// (This replaces `a_slotted_control_keeps_its_label_still_whatever_it_holds`, the old rule: the
/// label stood at the slot's leading edge and the slot's unused air sat before the next item.)
#[test]
fn a_slotted_control_is_placed_by_what_it_draws_so_the_row_has_one_gap() {
    let rows: Vec<HeaderRow> = [Some("4"), Some("12"), Some("9,999")].into_iter().map(row_with).collect();
    for row in &rows {
        let (collections, recommended) = (row.layout.items[0], row.layout.items[1]);
        assert_eq!(recommended.text.x - (collections.text.x + collections.text.w), GAP, "one gap after the ink, not after the slot");
        assert_eq!(recommended.text, rows[0].layout.items[1].text, "everything after the control stands where it stood");
        assert_eq!(collections.plate.w, collections.text.w + 2.0 * OVERHANG, "the plate hugs the text drawn");
        assert!((recommended.text.x + recommended.text.w - TEXT_X1).abs() < 0.01, "and the cluster still ends at the right text edge");
        assert_eq!((row.layout.cluster_w, row.layout.left_budget), (rows[0].layout.cluster_w, rows[0].layout.left_budget),
            "the budget the names pack into is the widest case's, whatever the count is");
    }
    let x = |at: usize| rows[at].layout.items[0].text.x;
    assert_eq!(x(0) - x(1), 8.0, "one more digit moves the label one digit's width left (Fixed: 8 px a glyph)");
    assert!(x(1) > x(2), "and so on up to the widest the slot holds");
}

/// **A read-out and a slotted control after it both keep the row's one gap**: the read-out stands
/// one [`GAP`] before the control, and the control's ink one [`GAP`] from the item after it, count
/// or no count.
#[test]
fn a_slotted_control_after_a_readout_still_stands_one_gap_before_the_next_item() {
    for value in ["4", "12", "9,999"] {
        let (spec, widest) = slotted(Some(value), true);
        let l = PageHeader::new(spec, &widest, &Fixed).layout(1920.0, &Fixed);
        let (readout, collections, recommended) = (l.items[0].text, l.items[1].text, l.items[2].text);
        assert_eq!(recommended.x - (collections.x + collections.w), GAP, "count {value}: one gap after the ink");
        assert_eq!(collections.x - (readout.x + readout.w), GAP, "count {value}: the read-out is one gap clear");
    }
}

/// **The read-out is unpinned**: it stands one [`GAP`] before the control after it, so it moves with
/// the live cluster's width (the cluster is right-anchored, the read-out is its leading item) and
/// the control after it keeps its place when the read-out's own text changes.
#[test]
fn the_readout_stands_one_gap_before_the_live_cluster_and_moves_with_it() {
    let (with, widest) = slotted(Some("4"), true);
    let held = PageHeader::new(with, &widest, &Fixed).layout(1920.0, &Fixed);
    let gap_of = |l: &HeaderLayout| l.items[1].text.x - (l.items[0].text.x + l.items[0].text.w);
    assert_eq!(gap_of(&held), GAP);
    assert!(held.items[0].text.x > TEXT_X1 - held.cluster_w, "narrower than the budget: the read-out is NOT at the budget's edge");
    let (mut without, _) = slotted(None, true);
    without.right.remove(1);
    let gone = PageHeader::new(without, &widest, &Fixed).layout(1920.0, &Fixed);
    assert_eq!(gone.items[1].text.x - (gone.items[0].text.x + gone.items[0].text.w), GAP, "still one gap when the control is gone");
    assert!(gone.items[0].text.x > held.items[0].text.x, "the shorter cluster leaves the read-out further right");
    assert_eq!(gone.cluster_w, held.cluster_w, "the budget is unchanged");
}

/// Every text run of `row` drawn at swap `swap`, as `(text, x, alpha)`.
fn swap_runs(row: &HeaderRow, swap: &HeaderSwap) -> Vec<(String, f32, f32)> {
    crate::draw_census::capture_runs(|| row.draw_with(Painter::recording(), 0.0, &TabStrip::new(), 1.0, None, swap))
        .into_iter().map(|run| (run.text, run.at.x, run.ink[3])).collect()
}

/// **A changed count of the same width cross-fades in place**: the old number leaves and the new
/// one arrives at the same x (the label does not move, the control's width is the same), their
/// opacities sum to one on every frame, the label is drawn once at one x, and the swap ends by
/// itself on the new row alone. A count of another digit count moves the label, which is the next
/// test's dissolve.
#[test]
fn a_changed_count_cross_fades_in_place_on_the_content_swap_clock() {
    let _g = plx_base::testlock::serial();
    let (old, new) = (row_with(Some("4")), row_with(Some("7")));
    let mut swap = HeaderSwap::new();
    swap.begin(&old, &new);
    assert!(swap.is_active());
    let label_x = swap_runs(&new, &swap).iter().find(|(text, ..)| text == "Collections").unwrap().1;
    let mut crossed = false;
    for _ in 0..30 {
        let runs = swap_runs(&new, &swap);
        let at = |text: &str| runs.iter().find(|(t, ..)| t == text).map(|(_, x, alpha)| (*x, *alpha));
        let labels: Vec<_> = runs.iter().filter(|(text, ..)| text == "Collections").collect();
        assert_eq!(labels.len(), 1, "one label, never two: {runs:?}");
        assert_eq!(labels[0].1, label_x, "and it does not move");
        if let (Some(from), Some(to)) = (at("4"), at("7")) {
            assert_eq!(from.0, to.0, "the numbers share an x");
            assert!((from.1 + to.1 - 1.0).abs() < 1e-4, "and their opacities sum to one: {from:?} {to:?}");
            crossed = true;
        }
        swap.step(1.0 / 60.0);
    }
    assert!(crossed, "the two numbers shared frames");
    assert!(!swap.is_active(), "the clock ran out");
    let runs = swap_runs(&new, &swap);
    assert!(runs.iter().all(|(text, ..)| text != "4") && runs.iter().any(|(text, _, alpha)| text == "7" && *alpha == 1.0));
}

/// **A count of another width dissolves the control at each x**: the width is the control's own, so
/// its label stands at another x on the new row (a digit's width on the Fixed measure). The swap
/// carries that as the dissolve it already plays for a control that comes or goes: the old face
/// (label and old number) fades out where it stood while the new face fades in where it now stands,
/// their opacities summing to one on every frame; nothing glides and the label is never drawn
/// twice at the same x.
#[test]
fn a_count_of_another_width_leaves_and_arrives_at_each_x() {
    let _g = plx_base::testlock::serial();
    let (old, new) = (row_with(Some("4")), row_with(Some("12")));
    let (old_x, new_x) = (old.layout.items[0].text.x, new.layout.items[0].text.x);
    assert_eq!(old_x - new_x, 8.0, "premise: the wider count moves the label one digit left");
    let mut swap = HeaderSwap::new();
    swap.begin(&old, &new);
    assert!(swap.is_active());
    let mut crossed = 0;
    for _ in 0..30 {
        let runs = swap_runs(&new, &swap);
        let label = |x: f32| runs.iter().find(|(text, rx, _)| text == "Collections" && *rx == x).map(|(_, _, alpha)| *alpha);
        let number = |text: &str| runs.iter().find(|(t, ..)| t == text).map(|(_, x, alpha)| (*x, *alpha));
        if let (Some(out), Some(arriving)) = (label(old_x), label(new_x)) {
            assert!((out + arriving - theme::CONTROL_TEXT_REST[3]).abs() < 1e-4, "the labels' opacities sum to the resting one: {out} {arriving}");
            let (from, to) = (number("4").expect("the old number leaves with its label"), number("12").expect("the new one arrives with its"));
            assert_eq!(from.0 - old_x, to.0 - new_x, "each number stands at its own label's offset");
            assert!((from.1 + to.1 - 1.0).abs() < 1e-4, "and the numbers' opacities sum to one: {from:?} {to:?}");
            crossed += 1;
        }
        assert!(runs.iter().filter(|(text, ..)| text == "Collections").count() <= 2, "two faces at most: {runs:?}");
        swap.step(1.0 / 60.0);
    }
    assert!(crossed >= 6, "several frames of dissolve, not a cut: {crossed}");
    assert!(!swap.is_active(), "the clock ran out");
    let runs = swap_runs(&new, &swap);
    assert!(runs.iter().all(|(text, ..)| text != "4") && runs.iter().filter(|(text, ..)| text == "Collections").count() == 1,
        "and the new face stands alone: {runs:?}");
}

/// A row with the Recommended control alone: the library whose probe found no collections.
fn row_without_the_slotted_control() -> HeaderRow {
    let (spec, widest) = slotted(None, false);
    let page = PageHeader::new(PageHeaderSpec { right: spec.right[1..].to_vec(), ..spec }, &widest, &Fixed);
    let layout = page.layout(1920.0, &Fixed);
    row_of(page, layout, &[(2, 0)])
}

/// **A control the row before did not have fades in where it stands** (a count that lands after the
/// row was built, or a library with collections after one without): the swap is on from the first
/// frame, the face rises from nothing on the swap clock and never pops. This is the way a count the
/// header did not know yet reaches the row, now that an unknown count is not drawn.
#[test]
fn a_control_the_row_before_lacked_fades_in_where_it_stands() {
    let _g = plx_base::testlock::serial();
    let (old, new) = (row_without_the_slotted_control(), row_with(Some("4")));
    let mut swap = HeaderSwap::new();
    swap.begin(&old, &new);
    assert!(swap.is_active(), "the swap runs although the row before carried no slotted control");
    let label_x = new.layout.items[0].text.x;
    let mut alphas = Vec::new();
    for _ in 0..30 {
        let runs = swap_runs(&new, &swap);
        if let Some((_, x, alpha)) = runs.iter().find(|(text, ..)| text == "Collections") {
            assert_eq!(*x, label_x, "it fades in where it stands");
            alphas.push(*alpha);
        }
        swap.step(1.0 / 60.0);
    }
    assert!(alphas.len() >= 6, "several frames of fade, not a pop: {alphas:?}");
    assert!(alphas[0] < theme::CONTROL_TEXT_REST[3] - 0.05, "the first frame is not fully there: {alphas:?}");
    assert!(alphas.windows(2).all(|pair| pair[1] >= pair[0]), "rising: {alphas:?}");
    assert!((alphas.last().unwrap() - theme::CONTROL_TEXT_REST[3]).abs() < 1e-4, "to fully there: {alphas:?}");
    assert!(!swap.is_active(), "and the swap ends on the new row alone");
}

/// A row whose read-out says `text`, with the Collections control holding `collections` (none:
/// the control is not on the row).
fn row_with_readout(text: &str, collections: Option<&str>) -> HeaderRow {
    let (mut spec, widest) = slotted(collections, true);
    spec.right[0].label = text.into();
    if collections.is_none() { spec.right.remove(1); }
    let page = PageHeader::new(spec, &widest, &Fixed);
    let layout = page.layout(1920.0, &Fixed);
    row_of(page, layout, &[])
}

/// **The read-out rides the content swap** (the owner dislikes an un-animated jump): the unpinned
/// filter read-out stands one gap before the cluster's next item, so it moves when Collections
/// arrives or leaves, when its count changes digit count, and when its own text changes. Each is a
/// leave-and-arrive at the two xs on the swap clock (the same dissolve a slotted control that moves
/// plays), never a cut: opacities sum to the read-out's resting one on every frame.
#[test]
fn a_readout_that_moves_or_changes_dissolves_at_each_x() {
    let _g = plx_base::testlock::serial();
    let rest = theme::COUNT_FILTERED_INK[3];
    // Collections arrives (the read-out moves left), then leaves, then the text changes in place.
    for (old, new) in [
        (row_with_readout("24 of 27", None), row_with_readout("24 of 27", Some("4"))),
        (row_with_readout("24 of 27", Some("4")), row_with_readout("24 of 27", None)),
        (row_with_readout("24 of 27", Some("4")), row_with_readout("8 of 27", Some("4"))),
    ] {
        let (old_text, new_text) = (old.page.spec().right[0].label.clone(), new.page.spec().right[0].label.clone());
        let (old_x, new_x) = (old.layout.items[0].text.x, new.layout.items[0].text.x);
        let mut swap = HeaderSwap::new();
        swap.begin(&old, &new);
        assert!(swap.is_active(), "{old_text} to {new_text}: the swap runs although no slotted value changed");
        let mut dissolving = 0;
        for _ in 0..30 {
            let runs = swap_runs(&new, &swap);
            let at = |text: &str, x: f32| runs.iter().find(|(t, rx, _)| t == text && *rx == x).map(|(_, _, alpha)| *alpha);
            if let (Some(out), Some(arriving)) = (at(&old_text, old_x), at(&new_text, new_x)) {
                assert!((out + arriving - rest).abs() < 1e-4, "the two faces' opacities sum to the resting one: {out} {arriving}");
                if out > 0.01 && arriving > 0.01 { dissolving += 1; }
            }
            swap.step(1.0 / 60.0);
        }
        assert!(dissolving >= 6, "{old_text} to {new_text}: several frames of dissolve, not a cut: {dissolving}");
        assert!(!swap.is_active());
        let runs = swap_runs(&new, &swap);
        assert_eq!(runs.iter().filter(|(t, ..)| *t == new_text).count(), 1, "the new face stands alone: {runs:?}");
        assert!(runs.iter().find(|(t, ..)| *t == new_text).is_some_and(|r| r.1 == new_x && (r.2 - rest).abs() < 1e-4));
        assert!(old_text == new_text || runs.iter().all(|(t, ..)| *t != old_text));
    }
}

/// A read-out both libraries have dissolves on a switch like a control both rows have; one only the
/// new library has is simply there, and one only the old library had is not drawn (`Edge::Cut`).
#[test]
fn a_switch_cuts_a_readout_only_one_row_has() {
    let _g = plx_base::testlock::serial();
    let with = row_with_readout("24 of 27", Some("4"));
    let without = row_with(Some("4"));
    let mut swap = HeaderSwap::new();
    swap.begin_switch(&with, &without);
    assert!(swap_runs(&without, &swap).iter().all(|(t, ..)| t != "24 of 27"), "the library left's read-out is not drawn");
    swap.begin_switch(&without, &with);
    let runs = swap_runs(&with, &swap);
    let readout = runs.iter().find(|(t, ..)| t == "24 of 27").expect("the read-out is on the row");
    assert!((readout.2 - theme::COUNT_FILTERED_INK[3]).abs() < 1e-4, "and not fading in: {readout:?}");
}

/// **A control the next row does not have fades out where it stood** (a library with no
/// collections): its face is painted at the old x with a falling opacity, never cut, and the swap
/// leaves the row exactly as the new row draws it.
#[test]
fn a_control_the_next_row_lacks_fades_out_in_place() {
    let _g = plx_base::testlock::serial();
    let old = row_with(Some("4"));
    let (spec, widest) = slotted(None, false);
    let page = PageHeader::new(PageHeaderSpec { right: spec.right[1..].to_vec(), ..spec }, &widest, &Fixed);
    let layout = page.layout(1920.0, &Fixed);
    let new = row_of(page, layout, &[(2, 0)]);
    let old_x = old.layout.items[0].text.x;
    let mut swap = HeaderSwap::new();
    swap.begin(&old, &new);
    let mut alphas = Vec::new();
    for _ in 0..30 {
        let runs = swap_runs(&new, &swap);
        if let Some((_, x, alpha)) = runs.iter().find(|(text, ..)| text == "Collections") {
            assert_eq!(*x, old_x, "it fades where it stood");
            alphas.push(*alpha);
        }
        swap.step(1.0 / 60.0);
    }
    assert!(alphas.len() >= 6, "several frames of fade, not a cut: {alphas:?}");
    assert!(alphas.windows(2).all(|pair| pair[1] <= pair[0]), "falling: {alphas:?}");
    assert!((alphas[0] - theme::CONTROL_TEXT_REST[3]).abs() < 1e-4, "from fully there: {alphas:?}");
    assert!(swap_runs(&new, &swap).iter().all(|(text, ..)| text != "Collections"), "and gone once the clock ran out");
}

/// **A switch to another library's row cuts a control only one of the rows has** (`begin_switch`):
/// the old library's control is not drawn on the new one (not even fading out, which for a library
/// with no collections shows its label for several frames), and a control the old row lacked is
/// simply there at its resting opacity. A control both rows have still dissolves, number to number.
#[test]
fn a_switch_cuts_the_control_only_one_row_has_and_dissolves_the_one_both_have() {
    let _g = plx_base::testlock::serial();
    let with = |value: &str| row_with(Some(value));
    // The library without: the control is gone from the first frame, and no swap is left running.
    let mut swap = HeaderSwap::new();
    swap.begin_switch(&with("4"), &row_without_the_slotted_control());
    let runs = swap_runs(&row_without_the_slotted_control(), &swap);
    assert!(runs.iter().all(|(text, ..)| text != "Collections" && text != "4"), "the left library's control is drawn: {runs:?}");
    // The library with, from one without: there from the first frame, at its resting opacity.
    let new = with("4");
    swap.begin_switch(&row_without_the_slotted_control(), &new);
    let runs = swap_runs(&new, &swap);
    let label = runs.iter().find(|(text, ..)| text == "Collections").expect("the control is on the row");
    assert!((label.2 - theme::CONTROL_TEXT_REST[3]).abs() < 1e-4, "and not fading in: {label:?}");
    // Both have it: the number cross-fades in place, as it does for one row changing.
    swap.end();
    let new = with("7");
    swap.begin_switch(&with("4"), &new);
    assert!(swap.is_active(), "a count that differs between the two libraries dissolves");
    for _ in 0..8 { swap.step(1.0 / 60.0); }
    let runs = swap_runs(&new, &swap);
    assert!(runs.iter().any(|(text, ..)| text == "4") && runs.iter().any(|(text, ..)| text == "7"), "number to number: {runs:?}");
    // Whereas the same page's own row losing the control (begin) fades it out where it stood.
    swap.end();
    swap.begin(&with("4"), &row_without_the_slotted_control());
    assert!(swap_runs(&row_without_the_slotted_control(), &swap).iter().any(|(text, ..)| text == "Collections"));
}

/// **A switch ends a fade of the row it leaves, whatever the switch itself carries** (`begin_switch`
/// while a `begin` is still running): the same library's count changed (4 to 7, fading), and the
/// press on another library arrives before the clock ran out. The new library's row has no slotted
/// control, so the switch changes nothing a swap would carry and used to return early with the
/// running fade still holding the OLD library's face: `leaving()` then drew it, fading out, on the
/// page of a library that has none. The fade is the left library's and ends with the switch.
#[test]
fn a_switch_arriving_during_a_fade_ends_it_and_draws_nothing_of_the_library_left() {
    let _g = plx_base::testlock::serial();
    let (before, after) = (row_with(Some("4")), row_with(Some("7")));
    let other = row_without_the_slotted_control();
    let mut swap = HeaderSwap::new();
    swap.begin(&before, &after);
    for _ in 0..3 { swap.step(1.0 / 60.0); }
    assert!(swap.is_active(), "premise: the count's fade is running");
    swap.begin_switch(&after, &other);
    assert!(!swap.is_active(), "the switch ends the left library's fade");
    let runs = swap_runs(&other, &swap);
    assert!(runs.iter().all(|(text, ..)| text != "Collections" && text != "4" && text != "7"),
        "no frame of the new page draws the control of the library left: {runs:?}");
    // And the same when the new library has the control with the same count: the fade between two
    // numbers of the library left is not the new page's to show.
    let mut swap = HeaderSwap::new();
    swap.begin(&before, &after);
    for _ in 0..3 { swap.step(1.0 / 60.0); }
    swap.begin_switch(&after, &row_with(Some("7")));
    let runs = swap_runs(&row_with(Some("7")), &swap);
    assert!(!swap.is_active() && runs.iter().all(|(text, ..)| text != "4"), "the old number is not drawn: {runs:?}");
}

/// **A label a capsule's shape crosses is painted once per ink region, each in its own ink**: the
/// part under the white focus capsule in the dark accent ink and the rest in the resting white,
/// split along the capsule's drawn shape (the regions are `TabStrip::pill_ink`'s, the per-fragment
/// cut each is painted through is the ink program's).
#[test]
fn a_label_the_focus_capsule_is_half_across_is_two_inks() {
    let _g = plx_base::testlock::serial();
    let h = header("Title");
    let l = h.layout(1920.0, &Fixed);
    let (mut capsules, mut pop) = settled(&l, -1, 0);
    // Travel to the next item until the capsule's edge stands inside the first one's plate.
    let plate = l.items[0].plate;
    let mut frame = None;
    for _ in 0..60 {
        PageHeader::step(&l, -1, 1, &mut capsules, &mut pop, DT);
        let spans = capsules.pill_ink(plate, TabGround::Bare { pop: 1.0, focus: true });
        if spans.regions().len() > 1 { frame = Some(spans); break; }
    }
    let spans = frame.expect("the capsule's shape crosses the first plate on its way");
    let label = &h.spec().left.first().map_or_else(|| h.spec().right[0].label.clone(), |item| item.label.clone());
    let runs: Vec<_> = crate::draw_census::capture_runs(|| h.draw(Painter::recording(), &l, &capsules, 1.0, None))
        .into_iter().filter(|run| &run.text == label).collect();
    assert_eq!(runs.len(), spans.regions().len(), "one paint of the label per region");
    let same = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-4);
    for (run, region) in runs.iter().zip(spans.regions()) {
        let want = if region.focus > 0.5 { crate::ACCENT_INK } else { theme::CONTROL_TEXT_REST };
        assert!(same(run.ink, want), "the region {region:?} wears its own ink: {:?} (wanted {want:?})", run.ink);
    }
    assert!(runs.iter().any(|run| same(run.ink, crate::ACCENT_INK)) && runs.iter().any(|run| same(run.ink, theme::CONTROL_TEXT_REST)),
        "both inks are on the label: the capsule is half across it");
}

/// **The gaps the eye reads are the gaps the stack says.** A Library header row is laid out on
/// `LegacyMeasure`, the measure the recording painter draws with, and DRAWN through a recording
/// painter; each item's drawn extent is read back off the runs it painted (its label's x to the end
/// of its last run, a fold glyph's box included), and the air between neighbours is [`GAP`] within
/// half a pixel: in the left names, between the last name and the "+N", and across the right
/// cluster. Collections wears "4" in a "9,999" slot (and `widest` holds "9,999"), so the live
/// cluster is narrower than its budget: the slot is not air (the air after the drawn "4" is [`GAP`],
/// whatever the slot holds, #875), and the read-out stands one [`GAP`] before Collections like the
/// rest.
#[test]
fn the_drawn_runs_of_a_library_header_stand_one_gap_apart() {
    let _g = plx_base::testlock::serial();
    let m = crate::widgets::LegacyMeasure;
    let collections = |value: &str| HeaderItem { value_slot: Some("9,999".into()), ..item("Collections", Some(value)) };
    let right = vec![
        HeaderItem { readout: true, focusable: false, ..item("24 of 27 films", None) },
        collections("4"),
        item("Recommended", None),
        HeaderItem { fold_glyph: true, ..item("Sort & Filter", Some("Title")) },
    ];
    let mut widest = right.clone();
    widest[1] = collections("9,999");
    let spec = PageHeaderSpec {
        left: vec![real("Movies", true), real("Documentaries", false)],
        overflow: 2,
        right: right.clone(),
        ..Default::default()
    };
    let page = PageHeader::new(spec, &widest, &m);
    let layout = page.layout(1920.0, &m);
    let runs = crate::draw_census::capture_runs(|| page.draw(Painter::recording(), &layout, &TabStrip::new(), 1.0, None));
    // One item's drawn extent: its label's x to the end of the last thing it paints.
    let extent = |label: &str, value: Option<&str>, fold: bool| -> (f32, f32) {
        let drawn = |text: &str| runs.iter().find(|run| run.text == text).unwrap_or_else(|| panic!("{text:?} was not drawn: {runs:?}"));
        let first = drawn(label).at;
        let last = value.map_or(first, |value| drawn(value).at);
        (first.x, last.x + last.w + if fold { FOLD_GAP + FOLD_SIZE } else { 0.0 })
    };
    let left = [extent("Movies", None, false), extent("Documentaries", None, false), extent("+2", None, false)];
    let cluster = [
        extent("24 of 27 films", None, false),
        extent("Collections", Some("4"), false),
        extent("Recommended", None, false),
        extent("Sort & Filter", Some("Title"), true),
    ];
    let live_w = cluster[3].1 - cluster[0].0;
    let slack = layout.cluster_w - live_w;
    let air = |row: &[(f32, f32)]| row.windows(2).map(|pair| pair[1].0 - pair[0].1).collect::<Vec<f32>>();
    for (gap, want, who) in air(&left).into_iter().zip([GAP; 2]).map(|(gap, want)| (gap, want, "names"))
        .chain(air(&cluster).into_iter().zip([GAP; 3]).map(|(gap, want)| (gap, want, "cluster")))
    {
        assert!((gap - want).abs() <= 0.5, "{who}: {gap} px drawn between neighbours, the layout says {want}");
    }
    assert!(slack > 20.0, "the fixture leaves the budget well clear of the live cluster, or the slot is not seen: {slack} px");
    assert!((cluster[3].1 - TEXT_X1).abs() <= 0.5, "the cluster ends at the right text edge, drawn: {}", cluster[3].1);
    assert!((left[0].0 - TEXT_X0).abs() <= 0.5, "the names start at the left text edge");
}

/// The fold glyph stands `FOLD_GAP` past the end of the value the face wears, and the item's run
/// (`run_w`) ends `FOLD_SIZE` after that: the draw and the width read one fold item.
#[test]
fn the_fold_glyph_stands_fold_gap_past_the_value() {
    let _g = plx_base::testlock::serial();
    // The census paints with `LegacyMeasure`, so the row is laid out with it too: draw advances
    // and layout widths are then the same numbers, as on the device.
    let m = crate::widgets::LegacyMeasure;
    let sort_item = HeaderItem { fold_glyph: true, ..item("Sort", Some("Date added")) };
    let page = PageHeader::new(
        PageHeaderSpec { right: vec![sort_item.clone()], ..Default::default() }, &[sort_item], &m);
    let layout = page.layout(1920.0, &m);
    let sort = layout.items.last().unwrap();
    let runs = crate::draw_census::capture_runs(|| page.draw(Painter::recording(), &layout, &TabStrip::new(), 1.0, None));
    let value = runs.iter().filter(|r| r.at.x >= sort.text.x).max_by(|a, b| a.at.x.total_cmp(&b.at.x)).expect("Sort's value is drawn");
    assert_eq!(value.text, "Date added");
    // The glyph (not in the census) stands FOLD_GAP past the value and the item's width ends
    // FOLD_SIZE on, so the painted value's end plus the fold item is the item's text edge.
    assert_eq!(value.at.x + value.at.w + FOLD_GAP + FOLD_SIZE, sort.text.x + sort.text.w);
}
