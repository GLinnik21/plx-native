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

fn drop_file(name: &str, byte: u8, status: Result<(), Refusal>) -> DropFile {
    DropFile { name: name.into(), sha: Some(Sha::of(&[byte])), meta: Some(meta("com.example.dropped", "Dropped")), status }
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
    assert!(l.entries[1].selected);
    let (page, _tx) = page_with(l);
    assert_eq!(page.form.table.n_rows(), 4);
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
        for notice in [None, Some(Notice::Refused { sha: Sha::of(&[9]), reason: Refusal::Invalid }), Some(Notice::NotWritten { sha: Sha::of(&[9]) })] {
            every.notice = notice;
            let mut form: FormTable<HudId, HudAction, SettingsPage> = FormTable::new(BAND);
            form.table.compact = false;
            form.set(hud_form(&every), None);
            for e in form.table.app_fit_failures(RouteLayout::screen().sectioned_table().w, language.tag()) {
                out.push(e);
            }
        }
        let _ = ShippedMeasure;
    }
    plx_ui::table::assert_no_fit_failures(&out);
}
