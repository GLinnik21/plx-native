//! The Player HUD page: its rows from a published list, the prompt before an unreviewed install, the
//! commands each row sends, and that every row fits its column in every shipped language.
use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::hudpacks::{Meta, Source};
use plx_machine::machine::{FocusRead, InputOwner, PressId, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_ui::fixture::FixtureMeasure;

fn context(focus: u32) -> Cx<'static, InnerHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: plx_data::stores::browse::DirectoryView::empty_for_test(),
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead { current: Some(FocusKey { entry: EntryId(0), elem: focus }), ..Default::default() },
        owner: InputOwner::Entry(EntryId(0)),
    }
}

fn drive(page: &mut HudPage, ev: ScreenEvent<InnerHost>, focus: u32) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    page.step(&ev, &context(focus), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    out
}

fn tick() -> ScreenEvent<InnerHost> {
    ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 })
}

fn huds(emitted: &[Stamped<InnerHost>]) -> Vec<&HudCmd> {
    emitted.iter().filter_map(|e| match &e.fx { Fx::App(AppFx::Huds(c)) => Some(c), _ => None }).collect()
}

fn meta(id: &str, name: &str) -> Meta {
    Meta { id: id.into(), name: name.into(), version: "1.0.0".into(), format: "1.0".into() }
}

fn builtin(selected: bool) -> Entry {
    Entry { id: BUILTIN_CLASSIC.into(), meta: None, sha: None, source: None, trust: Trust::BuiltIn, loadable: Ok(()), until_restart: false, selected }
}

fn pack(id: &str, name: &str, byte: u8, selected: bool) -> Entry {
    Entry {
        id: id.into(),
        meta: Some(meta(id, name)),
        sha: Some(Sha::of(&[byte])),
        source: Some(Source::Drop),
        trust: Trust::Unreviewed,
        loadable: Ok(()),
        until_restart: false,
        selected,
    }
}

/// A drop file as the store lists it (`hudpacks::drops`): a parsed file has its hash and meta, whatever its status; a file
/// over the size bound was never parsed, so it has neither, and the refusal alone.
fn drop_file(name: &str, byte: u8, status: Result<(), Refusal>) -> DropFile {
    if status == Err(Refusal::TooLarge) {
        return too_large_drop(name);
    }
    DropFile { name: name.into(), sha: Some(Sha::of(&[byte])), meta: Some(meta("com.example.dropped", "Dropped")), status }
}

/// What the store lists for a file over the size bound: no hash, no meta, and the refusal.
fn too_large_drop(name: &str) -> DropFile {
    DropFile { name: name.into(), sha: None, meta: None, status: Err(Refusal::TooLarge) }
}

/// The page's rows as (id, label, detail, checked), read from its own form.
fn shown(page: &HudPage) -> Vec<(HudId, String, String, bool)> {
    (0..page.form.table.n_rows() as usize)
        .filter_map(|i| {
            let id = page.form.id_at(i)?.clone();
            let row = page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i)?;
            Some((id, row.label.clone(), row.detail.clone(), row.checked))
        })
        .collect()
}

fn list() -> HudList {
    HudList {
        entries: vec![builtin(false), pack("com.example.a", "Alpha", 1, true)],
        drops: vec![drop_file("dropped.plxhud.json", 9, Ok(()))],
        unfit: vec![],
        language: "English".into(),
        notice: None,
    }
}

/// A page that has been sent `list`: the Open command is taken off its first event and the rows follow the next tick.
fn page_with(list: HudList) -> (HudPage, mpsc::Sender<HudList>) {
    let mut page = HudPage::new(EntryId(0));
    let emitted = drive(&mut page, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GroupId(0)) }), 0);
    let tx = match huds(&emitted).as_slice() {
        [HudCmd::Open(tx)] => tx.clone(),
        other => panic!("the first event opens the page's channel, once: {} commands", other.len()),
    };
    tx.send(list).unwrap();
    drive(&mut page, tick(), 0);
    (page, tx)
}

fn key_of(page: &HudPage, id: &HudId) -> u32 {
    page.form.key_at(page.form.index_of(id).expect("row present")).unwrap().0
}

#[test]
fn the_page_opens_its_channel_once_and_lists_what_it_is_sent() {
    let (mut page, _tx) = page_with(list());
    let again = drive(&mut page, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::FirstInGroup(GroupId(0)) }), 0);
    assert!(huds(&again).is_empty(), "a second event asks for nothing");
    let ids: Vec<HudId> = rows_for_test(&page.list).into_iter().map(|(_, id)| id).collect();
    assert_eq!(
        ids,
        [
            HudId::Pack(BUILTIN_CLASSIC.into()),
            HudId::Pack("com.example.a".into()),
            HudId::Remove("com.example.a".into()),
            HudId::Drop(Sha::of(&[9]))
        ]
    );
    assert_eq!(page.state.rows, 4);
}

#[test]
fn the_builtin_has_no_removal_row_and_every_key_is_unique_and_below_the_band() {
    let rows = rows_for_test(&list());
    assert!(!rows.iter().any(|(_, id)| *id == HudId::Remove(BUILTIN_CLASSIC.into())));
    let mut keys: Vec<u32> = rows.iter().map(|(k, _)| *k).collect();
    assert!(keys.iter().all(|k| *k < BAND));
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), rows.len());
}

#[test]
fn ok_on_a_package_picks_it_and_ok_on_remove_removes_it() {
    let (mut page, _tx) = page_with(list());
    let key = key_of(&page, &HudId::Pack("com.example.a".into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(matches!(huds(&emitted).as_slice(), [HudCmd::Select(id)] if id == "com.example.a"));
    let key = key_of(&page, &HudId::Pack(BUILTIN_CLASSIC.into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(matches!(huds(&emitted).as_slice(), [HudCmd::Select(id)] if id == BUILTIN_CLASSIC), "the built-in stays selectable");
    let key = key_of(&page, &HudId::Remove("com.example.a".into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(matches!(huds(&emitted).as_slice(), [HudCmd::Remove(id)] if id == "com.example.a"));
}

#[test]
fn a_package_the_app_cannot_run_is_listed_but_cannot_be_picked() {
    let mut l = list();
    l.entries[1].loadable = Err(Unloadable::NewerFormat);
    let (mut page, _tx) = page_with(l);
    let key = key_of(&page, &HudId::Pack("com.example.a".into()));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty(), "OK on an unloadable row does nothing");
}

/// An unreviewed file is installed only after the viewer says yes to the prompt; BACK or Cancel installs nothing.
#[test]
fn a_drop_file_installs_only_after_the_prompt_is_confirmed() {
    let (mut page, _tx) = page_with(list());
    let key = key_of(&page, &HudId::Drop(Sha::of(&[9])));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty(), "OK only asks");
    assert!(page.state.confirming && page.prompt.is_open());
    assert_eq!(page.asking, Some(Sha::of(&[9])));

    let affirm = ALERT + 1;
    let emitted = drive(&mut page, ScreenEvent::PressCommit(PressId(0)), affirm);
    assert!(matches!(huds(&emitted).as_slice(), [HudCmd::InstallDrop(sha)] if *sha == Sha::of(&[9])), "yes installs the file that was asked about");
    assert!(!page.state.confirming);

    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty());
    let cancel = ALERT;
    let emitted = drive(&mut page, ScreenEvent::PressCommit(PressId(0)), cancel);
    assert!(huds(&emitted).is_empty(), "no installs nothing");
    assert!(page.asking.is_none());
}

#[test]
fn a_file_that_cannot_be_installed_is_listed_and_inert() {
    let mut l = list();
    l.drops = vec![drop_file("imposter.plxhud.json", 3, Err(Refusal::BuiltinId))];
    let (mut page, _tx) = page_with(l);
    let key = key_of(&page, &HudId::Drop(Sha::of(&[3])));
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty() && !page.prompt.is_open(), "a refused file offers no install");
}

#[test]
fn the_unfit_reason_names_the_language_and_the_selection_is_still_shown_checked() {
    let mut l = list();
    l.unfit = vec![Sha::of(&[1])];
    l.language = "Deutsch".into();
    let facts = facts_of(&l.entries[1], &l);
    assert!(facts.iter().any(|f| *f == plx_platform::i18n::msg::settings_hud_unfit_language("Deutsch")), "{facts:?}");
    let (page, _tx) = page_with(l);
    let rows = shown(&page);
    let alpha = rows.iter().find(|r| r.0 == HudId::Pack("com.example.a".into())).expect("the package's row");
    assert!(alpha.3, "the unfit package is still the checked row: the selection is kept");
    assert!(alpha.2.contains(&plx_platform::i18n::msg::settings_hud_unfit_language("Deutsch")), "{alpha:?}");
    let classic = rows.iter().find(|r| r.0 == HudId::Pack(BUILTIN_CLASSIC.into())).expect("the built-in's row");
    assert!(!classic.3, "and the built-in is not");
}

/// The checkmark follows the selection: exactly one row is checked, and it is the selected entry's.
#[test]
fn exactly_the_selected_package_is_checked() {
    for (builtin_selected, expect) in [(true, BUILTIN_CLASSIC), (false, "com.example.a")] {
        let mut l = list();
        l.entries[0].selected = builtin_selected;
        l.entries[1].selected = !builtin_selected;
        let (page, _tx) = page_with(l);
        let checked: Vec<HudId> = shown(&page).into_iter().filter(|r| r.3).map(|r| r.0).collect();
        assert_eq!(checked, [HudId::Pack(expect.into())]);
    }
}

/// A file over the size bound has no hash, and still appears with its reason; it cannot be installed.
#[test]
fn a_too_large_drop_is_listed_with_its_reason_and_cannot_be_installed() {
    let mut l = list();
    l.drops = vec![too_large_drop("huge.plxhud.json")];
    let (mut page, _tx) = page_with(l);
    let rows = shown(&page);
    let huge = rows.iter().find(|r| r.1 == "huge.plxhud.json").expect("the file is on the page");
    assert_eq!(huge.2, plx_platform::i18n::msg::settings_hud_cant_install());
    let key = page.form.key_at(page.form.index_of(&huge.0).unwrap()).unwrap().0;
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty() && !page.prompt.is_open(), "nothing is installable from it");
}

#[test]
fn the_notice_replaces_the_line_under_the_title_only_while_there_is_one() {
    let mut l = list();
    assert_eq!(copy_of(&l), plx_platform::i18n::msg::settings_hud_copy());
    l.notice = Some(Notice::Refused { sha: Sha::of(&[9]), reason: Refusal::BuiltinId });
    assert_eq!(copy_of(&l), plx_platform::i18n::msg::settings_hud_cant_install());
    l.notice = Some(Notice::NotWritten { sha: Sha::of(&[9]) });
    assert_eq!(copy_of(&l), plx_platform::i18n::msg::settings_hud_not_saved());
}

/// **Every row fits its column in every shipped language**, whatever it says: the built-in, a package with
/// every fact it can carry, an unloadable one, an unfit one in the longest native language name, and
/// every refusal a drop file can show. The package names are the author's text and are not judged.
#[test]
fn every_row_fits_its_column_in_every_language() {
    let all_refusals = [
        Refusal::BuiltinId,
        Refusal::Invalid,
        Refusal::Unloadable(Unloadable::NewerFormat),
        Refusal::Unloadable(Unloadable::OlderFormat),
        Refusal::Unloadable(Unloadable::AppRange),
        Refusal::AlreadyInstalled,
        Refusal::NotNewer,
        Refusal::TooLarge,
        Refusal::Changed,
    ];
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let mut every = list();
        let mut volatile = pack("com.example.b", "Beta", 2, false);
        volatile.until_restart = true;
        let mut unfit = pack("com.example.c", "Gamma", 3, false);
        unfit.until_restart = true;
        let mut newer = pack("com.example.d", "Delta", 4, false);
        newer.loadable = Err(Unloadable::NewerFormat);
        every.entries.extend([volatile, unfit, newer]);
        every.unfit = vec![Sha::of(&[3])];
        every.language = SHIPPED.iter().map(|l| plx_platform::i18n::Preference::SHIPPED.into_iter().find(|p| p.tag() == l.tag()).map_or("", |p| p.native_name())).max_by_key(|n| n.chars().count()).unwrap_or("").to_owned();
        every.drops = all_refusals.iter().enumerate().map(|(i, r)| drop_file(&format!("f{i}.plxhud.json"), 20 + i as u8, Err(*r))).collect();
        every.drops.push(drop_file("ok.plxhud.json", 40, Ok(())));
        let mut form: FormTable<HudId, HudAction, SettingsPage> = FormTable::new(BAND);
        form.table.compact = false;
        form.set(hud_form(&every), None);
        for e in form.table.app_fit_failures(RouteLayout::screen().sectioned_table().w, language.tag()) {
            out.push(e);
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

/// **The line under the title fits the column complete in every language**, whichever text it carries (the page's
/// own copy, a refused file, a store that could not write): never cut at the line cap, never taller than the room
/// between the title and the action band, and short enough to read as a note.
#[test]
fn the_line_under_the_title_fits_the_column_in_every_language() {
    const MAX_COPY_LINES: usize = 5;
    let layout = RouteLayout::screen();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let mut l = list();
        for (name, notice) in [
            ("no notice", None),
            ("refused", Some(Notice::Refused { sha: Sha::of(&[9]), reason: Refusal::Invalid })),
            ("not written", Some(Notice::NotWritten { sha: Sha::of(&[9]) })),
        ] {
            l.notice = notice;
            let copy = copy_of(&l);
            let size = theme::size::LABEL;
            let view = plx_ui::text_view::TextView::new(&copy, size, theme::TEXT_READING)
                .leading(size as f32 + theme::space::XS)
                .max_lines(12)
                .with_measure(&ShippedMeasure);
            let top = layout.narrative_copy_frame(true, plx_platform::i18n::msg::settings_hud_title(), layout.action.y, &ShippedMeasure).y;
            let room = layout.action.y - theme::space::XL - top;
            let (lines, h) = (view.line_count(layout.narrative.w), view.measure_h(layout.narrative.w));
            if view.truncates(layout.narrative.w) {
                out.push(format!("{tag} {name}: the copy is cut at the line cap"));
            }
            if h > room {
                out.push(format!("{tag} {name}: the copy is {h}px tall, the column has {room}px"));
            }
            if lines > MAX_COPY_LINES {
                out.push(format!("{tag} {name}: {lines} lines, more than {MAX_COPY_LINES}"));
            }
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}
