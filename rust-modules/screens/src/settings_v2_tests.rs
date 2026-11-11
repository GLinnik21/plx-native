//! Settings v2, stage 1: the root's anatomy (groups, rows without sub-lines, the sentence that
//! follows focus, the picture slot's rows) and the Diagnostics page the trace rows moved to.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, SHIPPED};
use plx_platform::playback_trace::{Command, Status};

fn inputs(signed_in: bool) -> RootInputs {
    RootInputs {
        trace: Status::Off,
        signed_in, multi_user: false, library_count: 3, auto_sign_in: false, trailer_autoplay: true,
        resolution: UiResolution::Auto, resolution_offered: false,
        language: plx_platform::i18n::Preference::System,
        under_posters: plx_plex::plex::session::UnderPosters::DEFAULT, plaintext: Vec::new(),
    }
}

fn ids(inputs: &RootInputs) -> Vec<RootId> {
    let mut form = FormTable::<RootId, Action, SettingsPage>::new(super::super::registry::BAND);
    form.set(root_form(inputs), None);
    (0..form.table.n_rows() as usize).filter_map(|i| form.id_at(i).cloned()).collect()
}

/// The board's root (Sv2-1): four groups in order, eleven rows for a signed-in multi-profile
/// set — and no row carries a sub-line, because the sentence lives beside the table.
#[test]
fn the_root_is_four_groups_of_rows_without_sub_lines() {
    let inputs = RootInputs { multi_user: true, ..inputs(true) };
    assert_eq!(ids(&inputs), vec![
        RootId::Favourites, RootId::UnderPosters,
        RootId::Playback, RootId::AudioSubtitles,
        RootId::Language, RootId::AutoSignIn, RootId::TrailerAutoplay,
        RootId::Privacy, RootId::Diagnostics, RootId::Legal, RootId::About,
    ]);
    let mut form = FormTable::<RootId, Action, SettingsPage>::new(super::super::registry::BAND);
    form.set(root_form(&inputs), None);
    for section in &form.table.sections {
        for row in &section.rows {
            assert!(row.detail.is_empty(), "{:?} still carries a sub-line", row.label);
        }
    }
}

/// Sv2s-R1: signed out, the root is Playback's Video & playback, Language, Privacy and Support —
/// no Libraries group, no Audio & subtitles, no trailer switch, and Automatically Sign In needs
/// more than one profile.
#[test]
fn the_signed_out_root_drops_every_signed_in_row() {
    assert_eq!(ids(&inputs(false)), vec![
        RootId::Playback, RootId::Language,
        RootId::Privacy, RootId::Diagnostics, RootId::Legal, RootId::About,
    ]);
    assert!(!ids(&inputs(true)).contains(&RootId::AutoSignIn), "one profile skips the picker already");
}

/// How far the longest root scrolls to seat About, MEASURED on the table the root draws (board row
/// metrics, in the Settings layout's own table frame), and how far it may drift before this test
/// says the row heights or the section break moved. It is a pin of our geometry, not a number
/// read off the board.
const ALL_ROWS_SCROLL: f32 = 244.0;
const ALL_ROWS_SCROLL_TOLERANCE: f32 = 6.0;

/// Sv2s-R2: every conditional row at once is the board's longest root, and it scrolls.
#[test]
fn the_root_with_every_conditional_row_scrolls_with_focus() {
    let all = RootInputs {
        multi_user: true, resolution_offered: true,
        plaintext: vec![PlaintextRowInput { machine: ServerMachineId("m".into()), name: "Server".into(), named: true, on: false, connected: false }],
        ..inputs(true)
    };
    let scroll_to_last = |inputs: &RootInputs| {
        let mut form = FormTable::<RootId, Action, SettingsPage>::new(super::super::registry::BAND);
        form.table.compact = false;
        form.table.board_metrics(true); // the table the root draws, in the root's own layout
        form.set(root_form(inputs), Some(&RootId::About));
        for _ in 0..600 {
            form.table.update(1.0 / 60.0, RouteLayout::settings_page().sectioned_table().h);
        }
        form.table.scroll_pos()
    };
    let full = scroll_to_last(&all);
    let short = scroll_to_last(&inputs(false));
    assert!((full - ALL_ROWS_SCROLL).abs() < ALL_ROWS_SCROLL_TOLERANCE,
        "the longest root scrolls {ALL_ROWS_SCROLL}px (+-{ALL_ROWS_SCROLL_TOLERANCE}) to reach About: {full}");
    assert_eq!(short, 0.0, "the signed-out root fits without scrolling");
    assert!(full > scroll_to_last(&RootInputs { multi_user: true, ..inputs(true) }), "more rows, more scroll");
}

/// The sentence is a pure function of the focused row: its own, else the page's.
#[test]
fn the_sentence_follows_the_focused_row_and_falls_back_to_the_pages() {
    assert_eq!(sentence_for(Some(&RootId::Language)), msg::settings_language_detail());
    assert_eq!(sentence_for(Some(&RootId::TrailerAutoplay)), msg::settings_trailers_detail());
    assert_eq!(sentence_for(Some(&RootId::Diagnostics)), msg::settings_diagnostics_detail());
    assert_eq!(sentence_for(None), msg::settings_root_copy(), "no focused row, the page sentence");
    assert_eq!(sentence_for(Some(&RootId::Plaintext(ServerMachineId("m".into())))), msg::settings_root_copy(),
        "a server's switch states its own status in the table and has no sentence of its own");
    let libraries = sentence_for(Some(&RootId::Favourites));
    assert!(libraries.starts_with(msg::settings_libraries_detail()) && libraries.ends_with(msg::settings_libraries_more()));
}

/// A page that opens shows its first row's sentence at once: the page's own sentence is never
/// faded into it.
#[test]
fn the_root_opens_on_its_first_rows_sentence_with_nothing_fading() {
    let _serial = plx_base::testlock::serial();
    let root = RootPage::new(EntryId(0), cx(None).views.directory, &crate::registry::capture_fixture());
    let first = root.form.selected_id().cloned();
    assert!(first.is_some(), "the root seats a row on mount");
    assert_eq!(root.follow.text(), sentence_for(first.as_ref()).as_ref());
    assert_eq!(root.follow.arrival(), 1.0, "nothing is fading right after mount");
}

#[test]
fn moving_focus_moves_the_followed_sentence() {
    let _serial = plx_base::testlock::serial();
    let mut root = RootPage::new(EntryId(0), cx(None).views.directory, &crate::registry::capture_fixture());
    select_root(&mut root, RootId::Language);
    assert_eq!(root.follow.text(), msg::settings_language_detail());
    assert!(root.follow.arrival() < 1.0, "a new sentence fades in");
    select_root(&mut root, RootId::About);
    assert_eq!(root.follow.text(), msg::settings_about_detail());
}

/// **Every sentence fits the five lines the left column gives it, in every shipped language**
/// (the board's rule: "Descriptions must fit five lines in German, French and Belarusian"),
/// measured with the shipped faces at the narrative column's width.
#[test]
fn every_followed_sentence_fits_five_lines_in_every_language() {
    use plx_ui::text_view::TextView;
    let layout = RouteLayout::settings_page();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let frame = layout.narrative_copy_frame(false, msg::settings_title(), layout.action.y, &ShippedMeasure);
        // `Header::paint` hangs the sentence its CSS box's cap drop (~7px) below the frame's top
        let drop = layout.copy_cap_drop(theme::size::LABEL, &ShippedMeasure);
        let mut all: Vec<RootId> = vec![RootId::Favourites, RootId::UnderPosters, RootId::Playback, RootId::AudioSubtitles,
            RootId::Language, RootId::AutoSignIn, RootId::TrailerAutoplay, RootId::Resolution, RootId::Privacy,
            RootId::Diagnostics, RootId::Legal, RootId::About];
        all.push(RootId::Plaintext(ServerMachineId("m".into())));
        for id in all {
            let text = sentence_for(Some(&id));
            let view = TextView::new(&text, theme::size::LABEL, theme::TEXT_READING)
                .leading(theme::size::LABEL as f32 + theme::space::XS)
                .max_lines(plx_ui::follow_copy::FOLLOW_LINES + 1)
                .with_measure(&ShippedMeasure);
            let lines = (view.measure_h(layout.narrative.w) / (theme::size::LABEL as f32 + theme::space::XS)).round() as usize;
            if lines > plx_ui::follow_copy::FOLLOW_LINES {
                out.push(format!("{tag} {id:?}: {lines} lines"));
            }
            if frame.y + drop + lines as f32 * (theme::size::LABEL as f32 + theme::space::XS) > layout.picture_slot().y {
                out.push(format!("{tag} {id:?}: runs into the picture slot"));
            }
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

// ---- the Diagnostics page ------------------------------------------------------------------

fn diag_key(id: DiagIdProbe) -> u32 {
    match id { DiagIdProbe::Trace => 1, DiagIdProbe::Export => 2, DiagIdProbe::Delete => 3 }
}
enum DiagIdProbe { Trace, Export, Delete }

#[test]
fn the_diagnostics_buttons_are_effects_and_the_answered_status_drives_the_switch() {
    let _serial = plx_base::testlock::serial();
    let mut page = diagnostics::DiagnosticsPage::new(EntryId(0));
    let mut present = plx_machine::present::Present::new();
    let mut out = Vec::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.step(&ScreenEvent::Activate(diag_key(DiagIdProbe::Trace)), &cx(None), &mut fx);
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Start)))));
    // a press must not claim a recording before the runner answers: pressed again, it still starts
    out.clear();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.step(&ScreenEvent::Activate(diag_key(DiagIdProbe::Trace)), &cx(None), &mut fx);
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Start)))));
    assert!(!out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Stop)))),
        "a press must not claim a recording before the runner answers");
    out.clear();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.step(&ScreenEvent::App(super::super::registry::AppMsg::PlaybackTrace(Status::Recording)), &cx(None), &mut fx);
    page.step(&ScreenEvent::Activate(diag_key(DiagIdProbe::Trace)), &cx(None), &mut fx);
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Stop)))));
    out.clear();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    page.step(&ScreenEvent::Activate(diag_key(DiagIdProbe::Export)), &cx(None), &mut fx);
    page.step(&ScreenEvent::Activate(diag_key(DiagIdProbe::Delete)), &cx(None), &mut fx);
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Export)))));
    assert!(out.iter().any(|s| matches!(s.fx, Fx::App(AppFx::PlaybackTrace(Command::Delete)))));
}

/// The one sub-line that stays in the Diagnostics table is the switch's consequence; Delete is
/// last and alone in its section, so the table's hairline stands before it.
#[test]
fn diagnostics_keeps_the_consequence_sub_line_and_delete_last() {
    let form = diagnostics::diagnostics_form(Status::Off);
    let mut table = FormTable::new(super::super::registry::BAND);
    table.set(form, None);
    let rows: Vec<&plx_ui::table::Row> = table.table.sections.iter().flat_map(|s| s.rows.iter()).collect();
    assert!(!rows[0].detail.is_empty(), "the switch states its consequence");
    assert!(rows[1].detail.is_empty(), "the sentence under the title took Save diagnostic log's");
    assert!(rows[2].detail.is_empty());
    assert_eq!(table.table.n_rows(), 3);
    assert_eq!(table.table.sections.len(), 2, "Delete stands in a section of its own");
    assert_eq!(table.table.sections[1].rows.len(), 1);
    assert_eq!(table.id_at(2).map(|id| id.key().0), Some(3), "Delete is the last row");
}

fn cached_poster(title: &str) -> plx_data::pms::PmsMovie {
    plx_data::pms::PmsMovie { title: title.into(), thumb: format!("/library/metadata/{title}/thumb"), ..Default::default() }
}

/// **The Favorite libraries fan has art on entry.** Settings is entered from Home, which lists
/// shelves, not a library: the current library's listing is empty, yet the store holds posters
/// (the very ones the Under posters picture draws). The fan used to read the listing alone, so it
/// stayed empty until focus went down to Under posters, whose ask loaded the shelves, and back up.
#[test]
fn the_favorite_libraries_fan_reads_the_cache_the_picture_reads_not_the_current_listing_alone() {
    let _guard = plx_base::testlock::serial();
    let shelves = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new())
        .with_shelf_sample(vec![cached_poster("Alpha"), cached_poster("Bravo")]);
    let titles: Vec<&str> = shelves.view().sample(PICTURE_POSTERS).map(|m| m.title.as_str()).collect();
    assert_eq!(titles, vec!["Alpha", "Bravo"], "a poster held only in a shelf is in the fan");
    let listing = plx_data::stores::browse::DirectorySnapshot::fixture(0, 0, Vec::new())
        .with_sample(vec![cached_poster("One"), cached_poster("Two"), cached_poster("Three"), cached_poster("Four"), cached_poster("Five")]);
    assert_eq!(listing.view().sample(PICTURE_POSTERS).count(), PICTURE_POSTERS, "a full fan is capped");
}

/// A root entered with nothing cached asks for the posters while Favorite libraries has focus, so
/// the fan fills on its own, and stops asking once it has one.
#[test]
fn the_root_asks_for_posters_while_favorite_libraries_has_focus_and_the_fan_is_empty() {
    use plx_data::browse::SecKind;
    use plx_data::stores::browse::{DirectorySnapshot, SectionView, SrcRow};
    use plx_machine::present::Present;
    let _g = plx_base::testlock::serial();
    let _sess = multi_user_session("root-fan-ask");
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let section = |key, kind| SectionView { sid: Some(sid), key, kind, row: SrcRow { pinned: true, ..Default::default() } };
    let empty = DirectorySnapshot::fixture(7, 0, vec![section(1, SecKind::Movie), section(2, SecKind::Show)]);
    let mut root = RootPage::new(EntryId(0), empty.view(), &crate::registry::capture_fixture());
    assert_eq!(root.form.selected_id(), Some(&RootId::Favourites), "premise: the root opens on Favorite libraries");
    let focus = Some(FocusKey { entry: EntryId(0), elem: root.state.sel.0 });
    let tick = |root: &mut RootPage, snapshot: &DirectorySnapshot| {
        let mut out = Vec::new();
        let mut present = Present::new();
        let c = cx_with(focus, snapshot.view());
        root.step(&ScreenEvent::Tick(Tick::default()), &c, &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
        out
    };
    let asked = |effects: Vec<Stamped<InnerHost>>| effects.iter().filter(|e| matches!(&e.fx, Fx::App(AppFx::Store(
        plx_data::stores::StoreId::Browse, plx_data::stores::StoreCmd::Browse(plx_data::stores::browse::BrowseCmd::Addressed {
            work: plx_data::stores::browse::LibraryWork::Hubs { .. }, .. }))))).count();
    assert_eq!(asked(tick(&mut root, &empty)), 2, "both kinds' shelves are asked for while the fan is empty");
    let filled = DirectorySnapshot::fixture(7, 0, vec![section(1, SecKind::Movie), section(2, SecKind::Show)])
        .with_shelf_sample(vec![cached_poster("Alpha")]);
    assert_eq!(asked(tick(&mut root, &filled)), 0, "a fan with a poster asks for nothing");
}
