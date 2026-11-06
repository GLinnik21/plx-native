//! The Player HUD page: its rows from a published list, the prompt before an unreviewed install, the
//! commands each row sends, and that every row fits its column in every shipped language.
use super::*;
use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::hudpacks::{Meta, Source};
use plx_machine::machine::{FocusRead, PressId, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
use plx_ui::fixture::FixtureMeasure;

fn context(focus: u32) -> Cx<'static, InnerHost> {
    static MEASURE: FixtureMeasure = FixtureMeasure;
    Cx {
        views: plx_data::stores::browse::DirectoryView::empty_for_test().into(),
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
    Meta { id: id.into(), name: name.into(), version: "1.0.0".into(), package_format: "1.0".into(), hud_format: "1.0".into() }
}

fn builtin(selected: bool) -> Entry {
    Entry { id: BUILTIN_CLASSIC.into(), meta: None, sha: None, source: None, trust: Trust::BuiltIn, loadable: Ok(()), until_restart: false, selected, dev_source: None }
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
        dev_source: None,
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

fn row_of<'a>(page: &'a HudPage, id: &HudId) -> &'a Row {
    let i = page.form.index_of(id).expect("row present");
    page.form.table.sections.iter().flat_map(|s| s.rows.iter()).nth(i).expect("row in the table")
}

/// The trailing read-out of the row `id` (a reason, for a row that cannot be used).
fn value_of(page: &HudPage, id: &HudId) -> Option<String> {
    row_of(page, id).value.clone()
}

fn list() -> HudList {
    HudList {
        entries: vec![builtin(false), pack("com.example.a", "Alpha", 1, true)],
        drops: vec![drop_file("dropped.plxhud", 9, Ok(()))],
        unfit: vec![],
        language: "English".into(),
        notice: None,
        offer: None,
        fetching: false,
        ..HudList::default()
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

/// **The crumb over the title names the page this one is pushed from**, Video & playback (the Playback page's
/// title), the way a picker on that page does (`PickerKind::parent_title`) and not the Settings root two pages up.
#[test]
fn the_crumb_names_the_page_it_is_pushed_from() {
    let page = HudPage::new(EntryId(0));
    let parent = plx_platform::i18n::msg::settings_playback_title();
    assert_ne!(parent, plx_platform::i18n::msg::settings_title(), "premise: the two titles differ");
    assert_eq!(page.view().header.crumb, Some(parent), "the header's crumb");
    assert_eq!(page.crumb(&context(0)).as_deref(), Some(parent), "the screen's own crumb says the same");
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
            HudId::Browse,
            HudId::Remove("com.example.a".into()),
            HudId::Address,
            HudId::Drop(Sha::of(&[9]))
        ]
    );
    assert_eq!(page.state.rows, 6);
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
    assert_eq!(page.asking, Some(Asking::Drop(Sha::of(&[9]))));

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
    l.drops = vec![drop_file("imposter.plxhud", 3, Err(Refusal::BuiltinId))];
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
    assert!(facts.iter().all(|f| *f != plx_platform::i18n::msg::settings_hud_unfit_language("Deutsch")), "the reason is the row's value, not a sub-line: {facts:?}");
    let (page, _tx) = page_with(l);
    let rows = shown(&page);
    let alpha = rows.iter().find(|r| r.0 == HudId::Pack("com.example.a".into())).expect("the package's row");
    assert!(alpha.3, "the unfit package is still the checked row: the selection is kept");
    assert_eq!(value_of(&page, &alpha.0), Some(plx_platform::i18n::msg::settings_hud_unfit_language("Deutsch")), "{alpha:?}");
    assert!(row_of(&page, &alpha.0).disabled, "and it wears the ghost state");
    let classic = rows.iter().find(|r| r.0 == HudId::Pack(BUILTIN_CLASSIC.into())).expect("the built-in's row");
    assert!(!classic.3, "and the built-in is not");
}

/// **The app's own HUD is named "Built-in", and its row says no trust fact under it.** The row's name is the one
/// word the owner gave it, `settings_hud_builtin()`, in every shipped language (it was "Classic" over a "Built-in"
/// sub-line, which printed the word twice once it was the name); a built-in with no other fact to give has no detail
/// line at all. A sideloaded package still says "Unreviewed" and a registry package says no tier, so only the built-in
/// row changed.
#[test]
fn the_builtin_row_is_named_built_in_and_says_no_trust_fact_under_it() {
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let built_in = plx_platform::i18n::msg::settings_hud_builtin();
        for selected in [false, true] {
            let mut l = list();
            l.entries = vec![builtin(selected), pack("com.example.a", "Alpha", 1, !selected)];
            let (page, _tx) = page_with(l);
            let rows = shown(&page);
            let row = rows.iter().find(|r| r.0 == HudId::Pack(BUILTIN_CLASSIC.into())).expect("the built-in's row");
            assert_eq!(row.1, built_in, "{tag}: the built-in's row is named Built-in");
            assert_eq!(row.2, "", "{tag}: and has no detail line: it is not repeated as a trust fact under its own name");
            assert_eq!(row.3, selected, "{tag}: the checkmark still follows the selection");
            let alpha = rows.iter().find(|r| r.0 == HudId::Pack("com.example.a".into())).expect("the package's row");
            assert_eq!(alpha.1, "Alpha", "{tag}: a package keeps the author's name");
            assert_eq!(alpha.2, plx_platform::i18n::msg::settings_hud_unreviewed(), "{tag}: and a sideload its trust tier");
        }
        let mut registry = pack("com.example.r", "Registry", 5, false);
        registry.trust = Trust::Registry;
        assert!(facts_of(&registry, &list()).is_empty(), "{tag}: a registry package shows no tier");
    }
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
    l.drops = vec![too_large_drop("huge.plxhud")];
    let (mut page, _tx) = page_with(l);
    let rows = shown(&page);
    let huge = rows.iter().find(|r| r.1 == "huge.plxhud").expect("the file is on the page");
    assert_eq!(huge.2, "", "no sub-line: the reason is the value");
    assert_eq!(value_of(&page, &huge.0).as_deref(), Some(plx_platform::i18n::msg::settings_hud_too_large()));
    assert!(row_of(&page, &huge.0).disabled);
    let key = page.form.key_at(page.form.index_of(&huge.0).unwrap()).unwrap().0;
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(huds(&emitted).is_empty() && !page.prompt.is_open(), "nothing is installable from it");
}

#[test]
fn the_notice_replaces_the_line_under_the_title_only_while_there_is_one() {
    let mut l = list();
    assert_eq!(copy_of(&l, None), plx_platform::i18n::msg::settings_hud_copy());
    l.notice = Some(Notice::Refused { sha: Sha::of(&[9]), reason: Refusal::BuiltinId });
    assert_eq!(copy_of(&l, None), plx_platform::i18n::msg::settings_hud_cant_install());
    l.notice = Some(Notice::Refused { sha: Sha::of(&[9]), reason: Refusal::Invalid });
    assert_eq!(copy_of(&l, None), plx_platform::i18n::msg::settings_hud_not_a_hud());
    l.notice = Some(Notice::NotWritten { sha: Some(Sha::of(&[9])) });
    assert_eq!(copy_of(&l, None), plx_platform::i18n::msg::settings_hud_not_saved());
    for (reason, say) in [
        (FetchError::NotFound, plx_platform::i18n::msg::settings_hud_fetch_not_found()),
        (FetchError::TooLarge, plx_platform::i18n::msg::settings_hud_too_large()),
        (FetchError::Unreachable, plx_platform::i18n::msg::settings_hud_fetch_unreachable()),
        (FetchError::Certificate, plx_platform::i18n::msg::settings_hud_fetch_certificate()),
    ] {
        l.notice = Some(Notice::FetchFailed { reason });
        assert_eq!(copy_of(&l, None), say, "{reason:?}");
    }
    assert!(plx_platform::i18n::msg::settings_hud_fetch_certificate().len() > 10, "the certificate refusal is named, not folded into 'could not connect'");
    l.notice = Some(Notice::NotWritten { sha: None });
    assert_eq!(copy_of(&l, Some(AddressError::Form)), plx_platform::i18n::msg::settings_hud_address_form(), "a refused address outranks a notice");
}

/// Every refusal the store can give a file, for the tests that must cover each: the match has no wildcard, so a
/// refusal added to the store does not compile here until it is listed (and so is measured).
fn every_refusal() -> [Refusal; 10] {
    fn listed(refusal: Refusal) -> bool {
        match refusal {
            Refusal::BuiltinId
            | Refusal::Invalid
            | Refusal::Unloadable(Unloadable::NewerFormat | Unloadable::OlderFormat | Unloadable::AppRange | Unloadable::Revoked)
            | Refusal::AlreadyInstalled
            | Refusal::NotNewer
            | Refusal::TooLarge
            | Refusal::Changed => true,
        }
    }
    let all = [
        Refusal::BuiltinId,
        Refusal::Unloadable(Unloadable::Revoked),
        Refusal::Invalid,
        Refusal::Unloadable(Unloadable::NewerFormat),
        Refusal::Unloadable(Unloadable::OlderFormat),
        Refusal::Unloadable(Unloadable::AppRange),
        Refusal::AlreadyInstalled,
        Refusal::NotNewer,
        Refusal::TooLarge,
        Refusal::Changed,
    ];
    assert!(all.into_iter().all(listed));
    all
}

/// **Every row fits its column in every shipped language**, whatever it says: the built-in, a package with
/// every fact it can carry, an unloadable one, an unfit one in the longest native language name, and
/// every refusal a drop file can show. The package names are the author's text and are not judged.
#[test]
fn every_row_fits_its_column_in_every_language() {
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let mut every = list();
        let mut volatile = pack("com.example.b", "Beta", 2, false);
        volatile.until_restart = true;
        let mut unfit = pack("com.example.c", "Gamma", 3, false);
        unfit.until_restart = true;
        every.entries.extend([volatile, unfit]);
        // a package for each reason the app will not run it, whose reason is the row's value
        for (i, why) in [Unloadable::NewerFormat, Unloadable::OlderFormat, Unloadable::Revoked, Unloadable::AppRange].into_iter().enumerate() {
            let mut gone = pack(&format!("com.example.u{i}"), "Delta", 60 + i as u8, false);
            gone.loadable = Err(why);
            gone.until_restart = true;
            every.entries.push(gone);
        }
        every.unfit = vec![Sha::of(&[3])];
        every.language = SHIPPED.iter().map(|l| plx_platform::i18n::Preference::SHIPPED.into_iter().find(|p| p.tag() == l.tag()).map_or("", |p| p.native_name())).max_by_key(|n| n.chars().count()).unwrap_or("").to_owned();
        every.drops = every_refusal().iter().enumerate().map(|(i, r)| drop_file(&format!("f{i}.plxhud"), 20 + i as u8, Err(*r))).collect();
        every.drops.push(drop_file("ok.plxhud", 40, Ok(())));
        let mut form: FormTable<HudId, HudAction, SettingsPage> = FormTable::new(BAND);
        form.table.compact = false;
        form.table.board_metrics(true); // as `hud_settings.rs` draws it
        form.set(hud_form(&every, None).0, None);
        for e in form.table.app_fit_failures(RouteLayout::settings_page().sectioned_table().w, language.tag()) {
            out.push(e);
        }
        // The address row in each state it has: waiting, with a long address typed (the author's text, and the
        // detail is cut to fit the column), and downloading.
        for (fetching, draft) in [(false, None), (false, Some("https://example.com/a/very/long/path/hud.plxhud")), (true, None)] {
            let mut busy = list();
            busy.fetching = fetching;
            let mut form: FormTable<HudId, HudAction, SettingsPage> = FormTable::new(BAND);
            form.table.compact = false;
            form.table.board_metrics(true);
            let draft = draft.map(|text| TextBuffer::new(text.to_owned(), text.len()));
            form.set(hud_form(&busy, draft.as_ref()).0, None);
            out.extend(form.table.app_fit_failures(RouteLayout::settings_page().sectioned_table().w, language.tag()));
        }
    }
    plx_ui::table::assert_no_fit_failures(&out);
}

/// **The line under the title fits the column complete in every language**, whichever text it carries (the page's
/// own copy, each of the nine refusals a file can be given, a store that could not write): never cut at the line cap,
/// never taller than the room between the title and the action band, and short enough to read as a note.
#[test]
fn the_line_under_the_title_fits_the_column_in_every_language() {
    use plx_platform::i18n::msg;
    const MAX_COPY_LINES: usize = 5;
    let layout = RouteLayout::settings_page();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let check = |name: &str, copy: &str, out: &mut Vec<String>| {
            let size = theme::size::LABEL;
            let view = plx_ui::text_view::TextView::new(copy, size, theme::TEXT_READING)
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
        };
        let mut l = list();
        let notices = std::iter::once(("no notice".to_owned(), None))
            .chain(every_refusal().into_iter().map(|reason| (format!("refused {reason:?}"), Some(Notice::Refused { sha: Sha::of(&[9]), reason }))))
            .chain(std::iter::once(("not written".to_owned(), Some(Notice::NotWritten { sha: Some(Sha::of(&[9])) }))))
            .chain(
                [FetchError::NotFound, FetchError::TooLarge, FetchError::Unreachable, FetchError::Certificate]
                    .into_iter()
                    .map(|reason| (format!("fetch {reason:?}"), Some(Notice::FetchFailed { reason }))),
            );
        let refused = [None, Some(AddressError::Form), Some(AddressError::HttpNotPrivate)];
        for (name, notice, refused) in notices.flat_map(|(n, notice)| refused.map(move |r| (format!("{n} / {r:?}"), notice.clone(), r))) {
            l.notice = notice;
            check(&name, &copy_of(&l, refused), &mut out);
        }
        // the package page's own state sentences (a delisted package, a withdrawal, a catalog that could not refresh)
        for (name, copy) in [
            ("delisted", msg::settings_hud_unavailable().to_owned()),
            ("withdrawn", format!("{} {}", msg::settings_hud_revoked(), "Withdrawn for a reason of the catalog's own, one short sentence long.")),
            ("catalog cached", msg::settings_hud_catalog_cached().to_owned()),
        ] {
            check(name, &copy, &mut out);
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

// ---- install from an address ----

fn input(kind: InputKind<u32>) -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent { at: Tick::default(), source: plx_machine::machine::Source::Script, kind })
}

fn press(key: Key) -> ScreenEvent<InnerHost> {
    input(InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false })
}

fn typed(text: &str) -> ScreenEvent<InnerHost> {
    input(InputKind::Text(TextEdit::Commit(text.into())))
}

/// The keyboard raises (`true`) and lowers (`false`) the page asked for, in order.
fn keyboards(emitted: &[Stamped<InnerHost>]) -> Vec<bool> {
    emitted.iter().filter_map(|e| match &e.fx { Fx::Deliver(_, Delivery::Keyboard { up }) => Some(*up), _ => None }).collect()
}

fn address_row(page: &HudPage) -> (String, String, bool) {
    let rows = page.form.table.sections.iter().flat_map(|s| s.rows.iter());
    let row = rows.into_iter().find(|r| r.label == plx_platform::i18n::msg::settings_hud_address()).expect("the address row");
    (row.label.clone(), row.detail.clone(), row.dim)
}

/// **The row raises the keyboard, shows what is typed, and OK on the keyboard sends the parsed address**; nothing else
/// leaves the page, and the keyboard is lowered again.
#[test]
fn the_address_row_types_into_its_detail_line_and_ok_sends_the_parsed_address() {
    let (mut page, _tx) = page_with(list());
    assert_eq!(address_row(&page).1, plx_platform::i18n::msg::settings_hud_address_hint());
    let key = key_of(&page, &HudId::Address);
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert_eq!(keyboards(&emitted), [true], "OK on the row raises the keyboard");
    assert!(huds(&emitted).is_empty() && page.state.editing);

    drive(&mut page, typed("gh:owner/repo@v1.2"), key);
    assert_eq!(address_row(&page).1, "gh:owner/repo@v1.2|", "the detail line shows the draft");
    drive(&mut page, press(Key::Other), key);
    let emitted = drive(&mut page, input(InputKind::Key { key: Key::Other, sym: plx_ui::consts::SDLK_BACKSPACE, wcode: 0, edge: Edge::Down, at_edge: false }), key);
    assert!(huds(&emitted).is_empty());
    assert_eq!(address_row(&page).1, "gh:owner/repo@v1.|", "Backspace edits the draft");
    drive(&mut page, typed("2"), key);

    let emitted = drive(&mut page, press(Key::Ok), key);
    assert_eq!(keyboards(&emitted), [false], "OK on the keyboard lowers it");
    match huds(&emitted).as_slice() {
        [HudCmd::Fetch(address)] => assert_eq!(address.url(), "https://github.com/owner/repo/releases/download/v1.2/hud.plxhud"),
        other => panic!("one fetch, got {}", other.len()),
    }
    assert!(!page.state.editing);
    assert_eq!(address_row(&page).1, plx_platform::i18n::msg::settings_hud_address_hint());
}

/// A refused address sends nothing and says why under the title (the forms, or the LAN rule for a public `http://`);
/// the next edit clears it; BACK lowers the keyboard and sends nothing; a keyboard the television took down closes the field.
#[test]
fn a_refused_address_is_said_under_the_title_and_back_sends_nothing() {
    let (mut page, _tx) = page_with(list());
    let key = key_of(&page, &HudId::Address);
    for (text, say) in [
        ("ftp://example.com/x", plx_platform::i18n::msg::settings_hud_address_form()),
        ("http://example.com/x", plx_platform::i18n::msg::settings_hud_address_lan()),
        ("http://127.0.0.1/x", plx_platform::i18n::msg::settings_hud_address_lan()),
    ] {
        drive(&mut page, ScreenEvent::Activate(key), key);
        drive(&mut page, input(InputKind::Text(TextEdit::Clear)), key);
        drive(&mut page, typed(text), key);
        let emitted = drive(&mut page, press(Key::Ok), key);
        assert!(huds(&emitted).is_empty(), "{text}: a refused address sends nothing");
        assert_eq!(keyboards(&emitted), [false], "{text}");
        assert_eq!(page.copy, say, "{text}");
    }
    drive(&mut page, ScreenEvent::Activate(key), key);
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_hud_copy(), "raising the field again clears the refusal");
    let emitted = drive(&mut page, press(Key::Back), key);
    assert!(huds(&emitted).is_empty() && keyboards(&emitted) == [false] && !page.state.editing, "BACK lowers the keyboard and sends nothing");

    drive(&mut page, ScreenEvent::Activate(key), key);
    let emitted = drive(&mut page, input(InputKind::SystemKeyboard(false)), key);
    assert!(huds(&emitted).is_empty() && keyboards(&emitted).is_empty() && !page.state.editing, "the television's own close is not echoed back");
}

/// While a fetch is in flight the row is dimmed, says so, and OK on it does nothing: no second request, no keyboard.
#[test]
fn the_address_row_is_busy_while_a_fetch_runs() {
    let mut l = list();
    l.fetching = true;
    let (mut page, _tx) = page_with(l);
    assert_eq!(address_row(&page).1, plx_platform::i18n::msg::settings_hud_fetching());
    assert!(address_row(&page).2, "dimmed");
    let key = key_of(&page, &HudId::Address);
    let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
    assert!(keyboards(&emitted).is_empty() && huds(&emitted).is_empty() && !page.state.editing);
}

fn offered() -> OfferView {
    OfferView {
        sha: Sha::of(&[7]),
        meta: meta("com.example.fetched", "Fetched"),
        source: Source::Address,
        from: Some(address::parse("http://192.168.1.20:8080/hud.plxhud").unwrap()),
    }
}

/// **A fetched package is offered through the same prompt a drop file gets**, Cancel focused; yes installs the offer
/// that was shown, no and BACK decline it, and a listing that still carries the answered offer does not reopen it.
#[test]
fn an_offer_opens_the_prompt_with_cancel_focused_and_each_answer_names_the_offer() {
    for (answer, back) in [(ALERT + 1, false), (ALERT, false), (0, true)] {
        let mut l = list();
        l.offer = Some(offered());
        let (mut page, tx) = page_with(l.clone());
        assert!(page.prompt.is_open() && page.state.confirming, "the listing carrying an offer opens the prompt");
        assert_eq!(page.asking, Some(Asking::Offer(Sha::of(&[7]))));
        assert!(!page.prompt.choice(), "Cancel is the answer focused");
        assert_eq!(page.prompt.seat(ALERT_GROUP, EntryId(0)), Some(FocusKey { entry: EntryId(0), elem: ALERT }), "the engine seats Cancel");

        let emitted = if back {
            drive(&mut page, press(Key::Back), ALERT)
        } else {
            drive(&mut page, ScreenEvent::PressCommit(PressId(0)), answer)
        };
        match (huds(&emitted).as_slice(), answer == ALERT + 1) {
            ([HudCmd::InstallOffer(sha)], true) | ([HudCmd::DeclineOffer(sha)], false) => assert_eq!(*sha, Sha::of(&[7])),
            (other, _) => panic!("answer {answer} back {back}: {} commands", other.len()),
        }
        assert!(!page.state.confirming && page.asking.is_none());

        // The store has not answered yet: the same listing arrives again and the prompt stays down.
        l.fetching = false;
        tx.send(l.clone()).unwrap();
        l.language = "Deutsch".into();
        tx.send(l).unwrap();
        drive(&mut page, tick(), 0);
        assert!(!page.prompt.is_open(), "an answered offer is not asked again");
    }
}

/// The prompt names the package, where it came from, that a plain-http source is a development one, and that it is unreviewed.
#[test]
fn the_offer_prompt_says_what_it_is_about() {
    let mut l = list();
    l.offer = Some(offered());
    let (page, _tx) = page_with(l);
    let text = page.prompt.body_for_test();
    assert_eq!(text[0], "Fetched 1.0.0");
    assert_eq!(text[1], join(&[plx_platform::i18n::msg::settings_hud_offer_from("192.168.1.20"), plx_platform::i18n::msg::settings_hud_offer_dev().to_owned(), plx_platform::i18n::msg::settings_hud_unreviewed().to_owned()]));
    assert_eq!(text[2], plx_platform::i18n::msg::settings_hud_offer_body());
}

#[test]
fn the_address_caret_follows_an_interior_correction() {
    let (mut page, _tx) = page_with(list());
    let key = key_of(&page, &HudId::Address);
    drive(&mut page, ScreenEvent::Activate(key), key);
    drive(&mut page, typed("gh:owner/repo"), key);
    drive(&mut page, press(Key::Left), key);
    assert_eq!(address_row(&page).1, "gh:owner/rep|o");
    drive(&mut page, input(InputKind::Text(TextEdit::Backspace)), key);
    assert_eq!(address_row(&page).1, "gh:owner/re|o");
    drive(&mut page, typed("p"), key);
    assert_eq!(address_row(&page).1, "gh:owner/rep|o");
    let emitted = drive(&mut page, press(Key::Ok), key);
    assert!(matches!(huds(&emitted).as_slice(), [HudCmd::Fetch(address)] if address.url() == "https://github.com/owner/repo/releases/latest/download/hud.plxhud"));
}

// ---- fetch a dev source again (PR15) ----

const DEV_A: &str = "http://192.168.1.20:8123/hud.plxhud";
const DEV_B: &str = "http://10.0.0.7:9000/b.plxhud";

/// The Play key as the remote's ▶ reaches a page: an `Other` key whose `wcode` the shared classifier names.
fn play_key(edge: Edge) -> ScreenEvent<InnerHost> {
    input(InputKind::Key { key: Key::Other, sym: 0, wcode: plx_ui::consts::WCODE_PLAY, edge, at_edge: false })
}

/// An installed package that came from the development source `from`.
fn dev_pack(id: &str, byte: u8, from: &str) -> Entry {
    Entry { source: Some(Source::Address), dev_source: Some(address::parse(from).unwrap()), ..pack(id, id, byte, false) }
}

/// `drive`, with the answer too: whether the page took the key.
fn drive_handled(page: &mut HudPage, ev: ScreenEvent<InnerHost>, focus: u32) -> (Handled, Vec<Stamped<InnerHost>>) {
    let mut out = Vec::new();
    let mut present = Present::new();
    let handled = page.step(&ev, &context(focus), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present));
    (handled, out)
}

fn dev_page() -> HudPage {
    let mut l = list();
    l.entries.push(dev_pack("com.example.dev.a", 3, DEV_A));
    l.entries.push(dev_pack("com.example.dev.b", 4, DEV_B));
    page_with(l).0
}

/// **Play on the row of a package from a development source fetches the address it came from**: the same `HudCmd::Fetch`
/// "Install from address" sends, so the store's offer, the prompt and its confirmation follow and nothing is installed
/// without the viewer's OK. Each row asks for its own address.
#[test]
fn play_on_a_dev_source_row_fetches_its_recorded_address() {
    let mut page = dev_page();
    for (id, url) in [("com.example.dev.a", DEV_A), ("com.example.dev.b", DEV_B)] {
        let key = key_of(&page, &HudId::Pack(id.into()));
        let (handled, emitted) = drive_handled(&mut page, play_key(Edge::Down), key);
        assert_eq!(handled, Handled::Yes, "{id}");
        match huds(&emitted).as_slice() {
            [HudCmd::Fetch(address)] => assert_eq!((address.url(), address.dev_source()), (url, true), "{id}"),
            other => panic!("{id}: one fetch, got {} commands", other.len()),
        }
        assert!(!page.state.editing && !page.prompt.is_open(), "nothing else opens: the offer arrives with the next listing");
    }
}

/// The remote's other spellings of ▶ are the same key (`consts::classify_input` is the one reading).
#[test]
fn every_spelling_of_play_asks_the_same() {
    let mut page = dev_page();
    let key = key_of(&page, &HudId::Pack("com.example.dev.a".into()));
    for (sym, wcode) in [(0, plx_ui::consts::WCODE_PLAY), (plx_ui::consts::WCODE_PLAY_ALT_A, 0), (0, plx_ui::consts::WCODE_PLAY_ALT_B)] {
        let ev = input(InputKind::Key { key: Key::Other, sym, wcode, edge: Edge::Down, at_edge: false });
        let emitted = drive(&mut page, ev, key);
        assert!(matches!(huds(&emitted).as_slice(), [HudCmd::Fetch(a)] if a.url() == DEV_A), "sym {sym} wcode {wcode}");
    }
}

/// Every row that is not a dev source's, and every other key, is left as it was: Classic, a drop file's package, an
/// address install from a published source, the address row, a removal row and a drop file. The key goes unhandled.
#[test]
fn play_elsewhere_sends_nothing() {
    let mut l = list();
    l.entries.push(Entry { source: Some(Source::Address), ..pack("com.example.published", "Published", 5, false) });
    l.entries.push(dev_pack("com.example.dev", 6, DEV_A));
    let (mut page, _tx) = page_with(l);
    let ids = [
        HudId::Pack(BUILTIN_CLASSIC.into()),
        HudId::Pack("com.example.a".into()),
        HudId::Pack("com.example.published".into()),
        HudId::Address,
        HudId::Remove("com.example.a".into()),
        HudId::Remove("com.example.dev".into()),
        HudId::Drop(Sha::of(&[9])),
    ];
    for id in ids {
        let key = key_of(&page, &id);
        let (handled, emitted) = drive_handled(&mut page, play_key(Edge::Down), key);
        assert!(emitted.iter().all(|e| !matches!(e.fx, Fx::App(_))), "{id:?}: {} effects", emitted.len());
        assert_eq!(handled, Handled::No, "{id:?}");
    }
    let key = key_of(&page, &HudId::Pack("com.example.dev".into()));
    for other in [press(Key::Ok), press(Key::Left), input(InputKind::Key { key: Key::Other, sym: 0, wcode: plx_ui::consts::WCODE_PAUSE, edge: Edge::Down, at_edge: false })] {
        assert!(huds(&drive(&mut page, other, key)).is_empty(), "only Play fetches again");
    }
    // A press is one fetch: its auto-repeat and its release are not.
    for edge in [Edge::Repeat, Edge::Up] {
        assert!(huds(&drive(&mut page, play_key(edge), key)).is_empty(), "{edge:?}");
    }
}

/// The key waits for the page to be free: a fetch or another package operation in flight, the keyboard up for the
/// address, or the prompt open (its answers are the prompt's).
#[test]
fn play_waits_for_a_free_page() {
    for busy in ["fetching", "package_busy", "editing", "prompt"] {
        let mut l = list();
        l.entries.push(dev_pack("com.example.dev", 6, DEV_A));
        match busy {
            "fetching" => l.fetching = true,
            "package_busy" => l.package_busy = true,
            "prompt" => l.offer = Some(offered()),
            _ => (),
        }
        let (mut page, _tx) = page_with(l);
        if busy == "editing" {
            let address = key_of(&page, &HudId::Address);
            drive(&mut page, ScreenEvent::Activate(address), address);
            assert!(page.state.editing);
        }
        assert_eq!(page.prompt.is_open(), busy == "prompt");
        // The dev row keeps the focus in every case, so the prompt's own hold on the key (not an empty focus) is what stops it.
        let key = key_of(&page, &HudId::Pack("com.example.dev".into()));
        let emitted = drive(&mut page, play_key(Edge::Down), key);
        assert!(huds(&emitted).is_empty(), "{busy}: {} commands", huds(&emitted).len());
    }
}

fn gallery_list() -> HudList {
    let index=plx_data::hudpacks::catalog::parse(include_bytes!("../../../tests/fixtures/hud-gallery/index.json")).unwrap();
    HudList {catalog:plx_data::hudpacks::catalog::Snapshot {index:Some(std::sync::Arc::new(index)),checked_at:Some(10),..Default::default()},
        app_version:"0.10.0".into(),language_tag:"en".into(),entries:vec![builtin(true)],..HudList::default()}
}

fn gallery_page(page: SettingsPage) -> HudPage {
    let mut out=HudPage::at(EntryId(0),page);out.list=gallery_list();out.rebuild(None);out
}

#[test]
fn gallery_is_shared_navigation_and_hides_incompatible_or_unfit_packages() {
    let mut root=gallery_page(SettingsPage::PlayerHud);
    let browse=root.form.key_at(root.form.index_of(&HudId::Browse).unwrap()).unwrap().0;
    let fx=drive(&mut root,ScreenEvent::Activate(browse),browse);
    assert!(fx.iter().any(|e| matches!(e.fx,Fx::Nav(NavOp::Push(SettingsPage::HudGallery)))));
    let mut page=gallery_page(SettingsPage::HudGallery);
    assert_eq!(page.form.table.n_rows(),2);
    let mut index=page.list.catalog.index.as_deref().unwrap().clone();
    index.entries[0].package_format="2.0".into();
    index.entries[1].unfit=vec![plx_data::hudpacks::catalog::FitChange {from:"0.9.0".into(),langs:vec!["en".into()]}];
    page.list.catalog.index=Some(std::sync::Arc::new(index));page.rebuild(None);
    assert_eq!(page.form.table.n_rows(),0);
    assert_eq!(page.copy,plx_platform::i18n::msg::settings_hud_empty());
}

#[test]
fn gallery_install_and_use_are_separate_and_about_owns_package_metadata() {
    let list=gallery_list();let package=&list.catalog.index.as_ref().unwrap().entries[0];let sha=package.sha();
    let mut page=gallery_page(SettingsPage::HudDetail(sha));
    drive(&mut page,ScreenEvent::Enter(Enter::Fresh {focus:FocusTarget::FirstInGroup(GroupId(0))}),0);
    let fx=drive(&mut page,ScreenEvent::Activate(1),1);
    assert!(matches!(huds(&fx).as_slice(),[HudCmd::FetchRegistry(s)] if *s == sha));
    assert!(page.form.index_of(&HudId::Use).is_none());
    let mut entry=pack(&package.id,&package.name,1,false);entry.sha=Some(sha);
    page.list.entries.push(entry);page.list.notice=Some(Notice::Installed {id:package.id.clone(),version:package.version.clone(),sha,until_restart:false});
    page.rebuild(Some(HudId::Use));
    assert_eq!(page.copy, plx_platform::i18n::msg::settings_hud_copy(), "Use changes the choice; only installation preserves it");
    let fx=drive(&mut page,ScreenEvent::Activate(1),1);
    assert!(matches!(huds(&fx).as_slice(),[HudCmd::Select(id)] if *id == package.id));
    assert!(fx.iter().any(|e|matches!(e.fx,Fx::Nav(NavOp::PopToArg(SettingsPage::PlayerHud)))));
    let about=gallery_page(SettingsPage::HudAbout(sha));
    assert!(about.form.index_of(&HudId::Fact(0)).is_some());
    assert!(page.form.index_of(&HudId::Fact(0)).is_none(),"metadata is secondary");
}

#[test]
fn preview_focus_and_back_never_choose_a_different_scene() {
    let list=gallery_list();let package=list.catalog.index.as_ref().unwrap().entries.iter().find(|p|p.name=="Reference").unwrap();
    let mut page=gallery_page(SettingsPage::HudPreview(package.sha()));
    drive(&mut page,tick(),1);assert_eq!(page.scenes().len(),3);
    drive(&mut page,ScreenEvent::Activate(2),2);assert_eq!(page.state.scene,1);
    let controls=page.controls.as_ref().unwrap();
    assert!(matches!(controls.neighbour(FocusKey{entry:EntryId(0),elem:0},Dir::Left),Step::Edge));
    assert!(matches!(controls.neighbour(FocusKey{entry:EntryId(0),elem:0},Dir::Right),Step::Move(FocusKey{elem:1,..})));
    assert_eq!(page.state.scene,1,"moving focus from Back does not select Compact");
    let fx=drive(&mut page,ScreenEvent::Activate(0),0);
    assert!(fx.iter().any(|e|matches!(e.fx,Fx::Nav(NavOp::Pop))));
    assert_eq!(page.state.scene,1);
}

#[test]
fn preview_press_commits_choose_scenes_and_activate_back() {
    let list = gallery_list();
    let package = list.catalog.index.as_ref().unwrap().entries.iter()
        .find(|package| package.name == "Reference").unwrap();
    let mut page = gallery_page(SettingsPage::HudPreview(package.sha()));
    drive(&mut page, tick(), 0);

    // Control groups and pointer stops use the shared press protocol: OK-up and
    // pointer release deliver PressCommit rather than the direct Activate shortcut.
    drive(&mut page, ScreenEvent::PressCommit(PressId(1)), 2);
    assert_eq!(page.state.scene, 1, "committing Chapters & info selects its preview");
    drive(&mut page, ScreenEvent::PressCommit(PressId(2)), 3);
    assert_eq!(page.state.scene, 2, "committing Up Next selects its preview");
    for current in [None, Some(FocusKey { entry: EntryId(44), elem: 2 })] {
        let mut cx = context(2);
        cx.focus.current = current;
        let mut effects = Vec::new();
        let mut present = Present::new();
        let handled = page.step(&ScreenEvent::PressCommit(PressId(9)), &cx,
            &mut Effects::new(&mut effects, MachineId::Instance(InstanceId(0)), &mut present));
        assert!(matches!(handled, Handled::No));
        assert!(effects.is_empty(), "a commit without this page's focus cannot act");
        assert_eq!(page.state.scene, 2);
    }
    let effects = drive(&mut page, ScreenEvent::PressCommit(PressId(3)), 0);
    assert!(effects.iter().any(|effect| matches!(effect.fx, Fx::Nav(NavOp::Pop))));
    assert_eq!(page.state.scene, 2, "Back does not change the chosen scene");
}

#[test]
fn gallery_forms_and_preview_controls_fit_every_shipped_language() {
    let mut failures=Vec::new();
    for language in SHIPPED {
        let _guard=language_on_this_thread_for_test(language);
        let list=gallery_list();let sha=list.catalog.index.as_ref().unwrap().entries[1].sha();
        for dest in [SettingsPage::PlayerHud,SettingsPage::HudGallery,SettingsPage::HudDetail(sha),SettingsPage::HudAbout(sha)] {
            let page=gallery_page(dest);
            failures.extend(page.form.table.app_fit_failures(RouteLayout::settings_page().sectioned_table().w,language.tag()));
        }
        // a delisted package and one already installed: their disabled Install rows' values fit the column
        let mut delisted=gallery_page(SettingsPage::HudDetail(sha));
        delisted.list.catalog.previous=delisted.list.catalog.index.clone();
        delisted.list.catalog.index=None;delisted.rebuild(None);
        let mut same=gallery_page(SettingsPage::HudDetail(sha));
        let package=same.list.catalog.package(sha).unwrap().clone();
        let mut installed=pack(&package.id,&package.name,9,false);
        installed.meta.as_mut().unwrap().version=package.version.clone();
        same.list.entries.push(installed);same.rebuild(None);
        for page in [&delisted,&same] {
            failures.extend(page.form.table.app_fit_failures(RouteLayout::settings_page().sectioned_table().w,language.tag()));
        }
        let page=gallery_page(SettingsPage::HudPreview(sha));
        let controls=plx_ui::preview_controls::PreviewControls::new(page.scenes().iter().map(|(label,_)|(*label).into()),&ShippedMeasure);
        for elem in 0..controls.len() as u32 {
            let r=controls.place(elem).unwrap().rect;
            assert!(plx_ui::consts::inside_safe(r),"{} preview control {elem}: {r:?}",language.tag());
        }
    }
    plx_ui::table::assert_no_fit_failures(&failures);
}

#[test]
fn gallery_preview_never_uses_a_plex_server_slot() {
    let page=gallery_page(SettingsPage::HudGallery);
    match page.picture().and_then(|picture| picture.art).expect("the first catalog appearance has a preview") {
        plx_ui::widgets::Art::Thumb {sid,..} => assert_eq!(sid,plx_plex::plex::ServerId::UNSET.raw()),
        _ => panic!("the native preview must use the public image source"),
    }
}

#[test]
fn a_failed_index_refresh_does_not_disable_an_explicit_cached_package_download() {
    let list=gallery_list();let sha=list.catalog.index.as_ref().unwrap().entries[0].sha();
    let mut page=gallery_page(SettingsPage::HudDetail(sha));
    page.list.catalog.refresh_failed=true;page.rebuild(None);
    let fx=drive(&mut page,ScreenEvent::Activate(1),1);
    assert!(matches!(huds(&fx).as_slice(),[HudCmd::FetchRegistry(s)] if *s==sha));
    for notice in [Notice::Refused {sha,reason:Refusal::Changed},Notice::NotWritten {sha:Some(sha)}] {
        page.list.notice=Some(notice);page.rebuild(None);
        assert_eq!(page.copy,copy_of(&page.list,None),"the initiating detail page must explain the failure");
    }
    let mut fresh=gallery_page(SettingsPage::HudDetail(sha));
    fresh.list.notice=page.list.notice.clone();fresh.rebuild(None);
    assert_ne!(fresh.copy,page.copy,"another view does not inherit an unrelated install failure");
}

#[test]
fn the_first_hud_listing_seats_the_current_choice_until_the_reader_moves() {
    let (page,_)=page_with(list());
    assert_eq!(page.form.selected_id(),Some(&HudId::Pack("com.example.a".into())));
    let mut page=HudPage::new(EntryId(0));
    let emitted=drive(&mut page,ScreenEvent::Enter(Enter::Fresh{focus:FocusTarget::FirstInGroup(GroupId(0))}),ADDRESS_KEY);
    let tx=match huds(&emitted).as_slice(){[HudCmd::Open(tx)]=>tx.clone(),_=>panic!("open")};
    drive(&mut page,ScreenEvent::FocusMoved {from:None,to:FocusKey{entry:EntryId(0),elem:ADDRESS_KEY},by:plx_machine::machine::By::Dir},ADDRESS_KEY);
    tx.send(list()).unwrap();drive(&mut page,tick(),ADDRESS_KEY);
    assert_eq!(page.form.selected_id(),Some(&HudId::Address),"an intentional cursor is preserved across the listing");
}

#[test]
fn gallery_keeps_packages_visible_on_development_app_releases() {
    let mut page = gallery_page(SettingsPage::HudGallery);
    page.list.app_version = "0.10.0-dev".into();
    page.rebuild(None);
    assert_eq!(page.form.table.n_rows(), 2);
}

#[test]
fn about_cannot_remove_an_installed_hud_while_its_update_is_pending() {
    let list = gallery_list();
    let package = list.catalog.index.as_ref().unwrap().entries[0].clone();
    let mut page = gallery_page(SettingsPage::HudAbout(package.sha()));
    page.list.entries.push(pack(&package.id, &package.name, 1, false));
    for fetching in [true, false] {
        // Fetching and held-offer/confirmed-write phases share the complete operation flag.
        page.list.fetching = fetching;
        page.list.package_busy = true;
        page.rebuild(None);
        let key = key_of(&page, &HudId::Remove(package.id.clone()));
        let emitted = drive(&mut page, ScreenEvent::Activate(key), key);
        assert!(huds(&emitted).is_empty(), "a busy store cannot accept a removal");
        assert!(!emitted.iter().any(|e| matches!(e.fx, Fx::Nav(_))));
    }
}


#[test]
fn preview_backgrounds_stay_in_the_selected_scene_and_resolved_language() {
    let mut list = gallery_list();
    let mut index = list.catalog.index.as_deref().unwrap().clone();
    let package = index.entries.iter_mut().find(|p| p.name == "Reference").unwrap();
    let sha = package.sha();
    let primary = package.previews.iter().find(|p| p.state == "sections-3-movie-paused" && p.lang == "en").unwrap().clone();
    let mut alternate = primary.clone();
    alternate.url = "https://example.invalid/second.png".into();
    package.previews.push(alternate);
    let mut foreign = primary.clone();
    foreign.lang = "de".into();
    foreign.url = "https://example.invalid/german.png".into();
    package.previews.push(foreign);
    list.catalog.index = Some(std::sync::Arc::new(index));
    list.language_tag = "fr".into(); // Missing translation resolves once to English.
    let (mut page, _tx) = page_with(list);
    page.page = SettingsPage::HudPreview(sha);
    page.rebuild(None);
    page.state.scene = 1;
    assert_eq!(page.preview_frames(), vec![primary.url.as_str(), "https://example.invalid/second.png"]);
    for _ in 0..600 { drive(&mut page, tick(), 2); }
    assert_eq!(page.state.scene, 1, "the timer must never change the selected tab");
    assert_eq!(page.preview_frames().len(), 2);
    page.state.scene = 0;
    assert!(!page.preview_frames().contains(&"https://example.invalid/second.png"));
}


#[test]
fn preview_backgrounds_resume_only_for_the_visible_settings_page() {
    let (mut page, _tx) = page_with(gallery_list());
    assert!(page.preview_active());
    drive(&mut page, ScreenEvent::Suspend, 0);
    assert!(!page.preview_active());
    drive(&mut page, ScreenEvent::Resume, 0);
    assert!(page.preview_active(), "returning to the app resumes the same visible preview");
    drive(&mut page, ScreenEvent::Cover, 0);
    assert!(!page.preview_active());
    drive(&mut page, ScreenEvent::Suspend, 0);
    drive(&mut page, ScreenEvent::Resume, 0);
    assert!(!page.preview_active(), "resuming every inner body must not wake a covered page");
    drive(&mut page, ScreenEvent::Enter(Enter::Restored), 0);
    assert!(page.preview_active());
}

// ---- Settings v2: the sentence that follows focus, the ghost state, the picture slot ----

/// Move focus to the row `id` the way the engine reports it.
fn focus_on(page: &mut HudPage, id: &HudId) {
    let key = key_of(page, id);
    drive(page, ScreenEvent::FocusMoved { from: None, to: FocusKey { entry: EntryId(0), elem: key }, by: plx_machine::machine::By::Dir }, key);
}

fn withdrawn_list() -> HudList {
    let mut l = list();
    l.entries[1].loadable = Err(Unloadable::Revoked);
    l
}

/// **The sentence follows the focused row**, a pure function of the row and the listing: the page's own while a row has
/// none (the built-in, Browse, Install from address), the row's when it has (why a HUD cannot be used and what to do;
/// what Remove does), and the fade is heading for whatever it says.
#[test]
fn the_sentence_follows_the_focused_row_and_a_row_with_none_shows_the_pages() {
    use plx_platform::i18n::msg;
    let (mut page, _tx) = page_with(withdrawn_list());
    let alpha = HudId::Pack("com.example.a".into());
    let page_copy = msg::settings_hud_copy();
    for (id, says) in [
        (HudId::Pack(BUILTIN_CLASSIC.into()), page_copy),
        (alpha.clone(), msg::settings_hud_withdrawn_detail()),
        (HudId::Remove("com.example.a".into()), msg::settings_hud_remove_detail()),
        (HudId::Address, page_copy),
        (HudId::Browse, page_copy),
    ] {
        focus_on(&mut page, &id);
        assert_eq!(page.sentence(), says, "{id:?}");
        assert_eq!(page.follow.text(), says, "{id:?}: the fade is heading for it");
    }
}

/// A refusal or a failed fetch in force is the sentence whichever row has focus; the row's own comes back once it clears.
#[test]
fn a_notice_in_force_outranks_every_rows_sentence() {
    use plx_platform::i18n::msg;
    let mut l = withdrawn_list();
    l.notice = Some(Notice::FetchFailed { reason: FetchError::Unreachable });
    let (mut page, _tx) = page_with(l);
    focus_on(&mut page, &HudId::Pack("com.example.a".into()));
    assert_eq!(page.sentence(), msg::settings_hud_fetch_unreachable());
    page.list.notice = None;
    page.rebuild(None);
    focus_on(&mut page, &HudId::Pack("com.example.a".into()));
    assert_eq!(page.sentence(), msg::settings_hud_withdrawn_detail());
}

/// **A withdrawn HUD is listed as a ghost row** (board 4.4): the reason is its value (the old sub-line), the row cannot
/// be picked, the removal row names it ("Remove Alpha") with no sub-line, and Install from address keeps its field hint.
#[test]
fn a_withdrawn_hud_is_a_ghost_row_with_its_reason_as_the_value() {
    use plx_platform::i18n::msg;
    let (mut page, _tx) = page_with(withdrawn_list());
    let alpha = HudId::Pack("com.example.a".into());
    let row = row_of(&page, &alpha);
    assert!(row.disabled && row.detail == msg::settings_hud_unreviewed(), "a ghost row keeps its status sub-line");
    assert_eq!(row.value.as_deref(), Some(msg::settings_hud_revoked()), "This HUD was withdrawn. is the value");
    let key = key_of(&page, &alpha);
    assert!(huds(&drive(&mut page, ScreenEvent::Activate(key), key)).is_empty(), "and OK does nothing");
    let remove = row_of(&page, &HudId::Remove("com.example.a".into()));
    assert_eq!(remove.label, msg::settings_hud_remove_named("Alpha"));
    assert_eq!(remove.detail, "", "the old sub-line Remove is the label now");
    assert!(!remove.disabled);
    let address = row_of(&page, &HudId::Address);
    assert_eq!(address.detail, msg::settings_hud_address_hint(), "a field hint keeps its sub-line");
    let builtin = row_of(&page, &HudId::Pack(BUILTIN_CLASSIC.into()));
    assert!(!builtin.disabled && builtin.value.is_none());
}

/// **A HUD that does not fit the language** (board 4.5) is the same DISABLED row, its value "Doesn't fit in {language}":
/// focus lands on it and reads why, and OK (or a click, which is the same `Activate`) does nothing.
#[test]
fn a_hud_that_does_not_fit_the_language_is_a_disabled_row_and_ok_is_inert() {
    use plx_platform::i18n::msg;
    let mut l = list();
    l.unfit = vec![Sha::of(&[1])];
    l.language = "Français".into();
    let (mut page, _tx) = page_with(l);
    let alpha = HudId::Pack("com.example.a".into());
    assert_eq!(row_of(&page, &alpha).value.as_deref(), Some(msg::settings_hud_unfit_language("Français").as_str()));
    assert!(row_of(&page, &alpha).disabled);
    focus_on(&mut page, &alpha);
    assert_eq!(page.sentence(), msg::settings_hud_unfit_detail("Français"));
    let key = key_of(&page, &alpha);
    assert!(huds(&drive(&mut page, ScreenEvent::Activate(key), key)).is_empty(), "OK on a disabled row selects nothing");
    // a row that fits is unchanged: OK still selects it
    let classic = HudId::Pack(BUILTIN_CLASSIC.into());
    let key = key_of(&page, &classic);
    assert!(matches!(huds(&drive(&mut page, ScreenEvent::Activate(key), key)).as_slice(), [HudCmd::Select(id)] if id == BUILTIN_CLASSIC), "a usable row selects");
}

/// A file the store refused says why as its value and what to do beside the table; one it can install keeps its sub-line.
#[test]
fn a_refused_file_is_a_ghost_row_and_an_installable_one_keeps_its_sub_line() {
    use plx_platform::i18n::msg;
    let mut l = list();
    l.drops = vec![drop_file("ok.plxhud", 9, Ok(())), drop_file("bad.plxhud", 3, Err(Refusal::Invalid)), drop_file("new.plxhud", 4, Err(Refusal::Unloadable(Unloadable::NewerFormat)))];
    let (mut page, _tx) = page_with(l);
    let ok = row_of(&page, &HudId::Drop(Sha::of(&[9])));
    assert!(!ok.disabled && ok.value.is_none() && ok.detail.contains(msg::settings_hud_install()));
    let bad = row_of(&page, &HudId::Drop(Sha::of(&[3])));
    assert!(bad.disabled && bad.detail.is_empty());
    assert_eq!(bad.value.as_deref(), Some(msg::settings_hud_not_a_hud()));
    focus_on(&mut page, &HudId::Drop(Sha::of(&[3])));
    assert_eq!(page.sentence(), msg::settings_hud_file_detail());
    focus_on(&mut page, &HudId::Drop(Sha::of(&[4])));
    assert_eq!(page.sentence(), msg::settings_hud_newer_detail(), "a format the app cannot read says the format's sentence");
}

fn with_index(page: &mut HudPage, edit: impl FnOnce(&mut plx_data::hudpacks::catalog::Index)) {
    let mut index = page.list.catalog.index.as_deref().unwrap().clone();
    edit(&mut index);
    page.list.catalog.index = Some(std::sync::Arc::new(index));
    page.rebuild(None);
}

/// **Package page, update waiting** (board 4.2): focus on Update, the sentence describes Update; move focus and it changes.
#[test]
fn the_package_pages_sentence_follows_update_and_the_rows() {
    use plx_platform::i18n::msg;
    let list = gallery_list();
    let package = list.catalog.index.as_ref().unwrap().entries.iter().find(|p| p.name == "Reference").unwrap().clone();
    let sha = package.sha();
    let mut page = gallery_page(SettingsPage::HudDetail(sha));
    let mut installed = pack(&package.id, &package.name, 7, false);
    installed.meta.as_mut().unwrap().version = "0.5.0".into();
    page.list.entries.push(installed);
    page.rebuild(None);
    let update = HudId::InstallRegistry(sha);
    assert_eq!(row_of(&page, &update).label, msg::settings_hud_update());
    focus_on(&mut page, &update);
    assert_eq!(page.sentence(), msg::settings_hud_update_detail());
    focus_on(&mut page, &HudId::About);
    assert_eq!(page.sentence(), msg::settings_hud_about_detail());
    focus_on(&mut page, &HudId::Preview);
    assert_eq!(page.sentence(), msg::settings_hud_preview_detail());
    assert!(page.picture().is_some(), "the package's preview is in the picture slot");
    // not installed at all: Install, with its own sentence
    let mut fresh = gallery_page(SettingsPage::HudDetail(sha));
    focus_on(&mut fresh, &update);
    assert_eq!(row_of(&fresh, &update).label, msg::settings_hud_install());
    assert_eq!(fresh.sentence(), msg::settings_hud_install_detail());
}

/// A withdrawn package in the catalog, or one that does not fit the language, cannot be installed: Install is a ghost
/// row with the reason as its value; a withdrawal's sentence is the page's own state (the withdrawal and the catalog's reason), a
/// language's says what to do.
#[test]
fn an_uninstallable_package_page_row_is_a_ghost_with_its_reason() {
    use plx_platform::i18n::msg;
    let list = gallery_list();
    let sha = list.catalog.index.as_ref().unwrap().entries[0].sha();
    let mut page = gallery_page(SettingsPage::HudDetail(sha));
    with_index(&mut page, |index| index.revoked.push(plx_data::hudpacks::catalog::Revocation { sha256: sha.hex(), reason: "Unsafe.".into() }));
    let install = HudId::InstallRegistry(sha);
    assert!(row_of(&page, &install).disabled);
    assert_eq!(row_of(&page, &install).value.as_deref(), Some(msg::settings_hud_revoked()));
    focus_on(&mut page, &install);
    assert_eq!(page.sentence(), format!("{} Unsafe.", msg::settings_hud_revoked()), "the page's state (the withdrawal and the catalog's reason) outranks the row's sentence");
    let mut page = gallery_page(SettingsPage::HudDetail(sha));
    page.list.unfit = vec![sha];
    page.list.language = "Deutsch".into();
    page.rebuild(None);
    assert_eq!(row_of(&page, &install).value.as_deref(), Some(msg::settings_hud_unfit_language("Deutsch").as_str()));
    focus_on(&mut page, &install);
    assert_eq!(page.sentence(), msg::settings_hud_unfit_detail("Deutsch"));
}

/// **A package page opens on its state, and a disabled Install always says why.** The page's state (a delisted package,
/// a withdrawal with the catalog's reason) is the sentence whichever row has focus, as a notice is on the Player HUD page;
/// a package an equal or newer version of which is installed (the same version under another hash) says so as the value.
#[test]
fn a_package_pages_state_outranks_its_rows_and_a_disabled_install_says_why() {
    use plx_platform::i18n::msg;
    let list = gallery_list();
    let package = list.catalog.index.as_ref().unwrap().entries.iter().find(|p| p.name == "Reference").unwrap().clone();
    let sha = package.sha();
    let install = HudId::InstallRegistry(sha);
    let every_row = [HudId::Preview, install.clone(), HudId::About];

    // withdrawn: opens on the withdrawal and the catalog's reason, on every row
    let mut withdrawn = gallery_page(SettingsPage::HudDetail(sha));
    with_index(&mut withdrawn, |index| index.revoked.push(plx_data::hudpacks::catalog::Revocation { sha256: sha.hex(), reason: "Unsafe.".into() }));
    assert_eq!(withdrawn.sentence(), format!("{} Unsafe.", msg::settings_hud_revoked()), "the page opens on the reason");
    for id in &every_row {
        focus_on(&mut withdrawn, id);
        assert_eq!(withdrawn.sentence(), format!("{} Unsafe.", msg::settings_hud_revoked()), "{id:?}");
    }
    assert!(row_of(&withdrawn, &install).disabled);
    assert_eq!(row_of(&withdrawn, &install).value.as_deref(), Some(msg::settings_hud_revoked()));

    // delisted (a refresh removed it; the previous index still describes it): the unavailable sentence, and the value
    let mut delisted = gallery_page(SettingsPage::HudDetail(sha));
    let old = delisted.list.catalog.index.clone();
    with_index(&mut delisted, |index| index.entries.retain(|p| p.sha() != sha));
    delisted.list.catalog.previous = old;
    delisted.rebuild(None);
    assert!(delisted.package().is_some(), "the premise: the page still describes it from the previous index");
    assert!(row_of(&delisted, &install).disabled);
    assert_eq!(row_of(&delisted, &install).value.as_deref(), Some(msg::settings_hud_unavailable_value()));
    for id in &every_row {
        focus_on(&mut delisted, id);
        assert_eq!(delisted.sentence(), msg::settings_hud_unavailable(), "{id:?}");
    }

    // the same version installed under another hash: not newer, so Install is disabled and says "Already installed"
    let mut same = gallery_page(SettingsPage::HudDetail(sha));
    let mut installed = pack(&package.id, &package.name, 9, false);
    installed.meta.as_mut().unwrap().version = package.version.clone();
    assert_ne!(installed.sha, Some(sha), "the premise: another hash");
    same.list.entries.push(installed);
    same.rebuild(None);
    assert!(row_of(&same, &install).disabled);
    assert_eq!(row_of(&same, &install).value.as_deref(), Some(msg::settings_hud_already()));
    focus_on(&mut same, &install);
    assert_eq!(same.sentence(), msg::settings_hud_copy(), "no page state: the page's own sentence, the value says why");
    // and a plain installable package keeps its Install sentence with no reason
    let mut fresh = gallery_page(SettingsPage::HudDetail(sha));
    focus_on(&mut fresh, &install);
    assert!(!row_of(&fresh, &install).disabled && row_of(&fresh, &install).value.is_none());
    assert_eq!(fresh.sentence(), msg::settings_hud_install_detail());
}

/// **The picture follows the focused HUD** (board 4.1): the gallery's picture slot holds the preview of the HUD under
/// focus, frames and all, and nothing else; About has none.
#[test]
fn the_gallery_picture_follows_the_focused_hud() {
    let mut page = gallery_page(SettingsPage::HudGallery);
    let list = gallery_list();
    let index = list.catalog.index.as_ref().unwrap();
    let first = HudId::Catalog(index.entries[0].sha());
    let second = HudId::Catalog(index.entries[1].sha());
    focus_on(&mut page, &first);
    drive(&mut page, tick(), 0);
    let a: Vec<String> = page.preview_frames().into_iter().map(str::to_owned).collect();
    assert!(!a.is_empty() && page.picture().is_some());
    focus_on(&mut page, &second);
    drive(&mut page, tick(), 0);
    let b: Vec<String> = page.preview_frames().into_iter().map(str::to_owned).collect();
    assert!(!b.is_empty());
    assert_ne!(a, b, "another HUD, another picture");
    assert!(b.iter().all(|url| index.entries[1].previews.iter().any(|p| p.url == *url)), "the second HUD's own frames");
    assert_eq!(page.sentence(), plx_platform::i18n::msg::settings_hud_gallery_copy(), "and the sentence is the page's own");
    let about = gallery_page(SettingsPage::HudAbout(index.entries[0].sha()));
    assert!(about.picture().is_none());
}

/// **Empty gallery** (board 4.3): no rows and no picture; the description carries the whole message.
#[test]
fn an_empty_gallery_has_no_rows_no_picture_and_its_message_as_the_sentence() {
    use plx_platform::i18n::msg;
    let mut page = gallery_page(SettingsPage::HudGallery);
    with_index(&mut page, |index| index.entries.clear());
    assert_eq!(page.form.table.n_rows(), 0);
    assert!(page.picture().is_none());
    assert_eq!(page.sentence(), msg::settings_hud_empty());
}

/// **Offline gallery** (board 4.1's variant): the description is "Couldn't refresh HUDs. Showing the saved catalog." with
/// the catalog date, and the picture stays.
#[test]
fn an_offline_gallery_says_the_saved_catalog_with_its_date_and_keeps_the_picture() {
    use plx_platform::i18n::msg;
    let mut page = gallery_page(SettingsPage::HudGallery);
    page.list.catalog.refresh_failed = true;
    page.list.catalog_date = "3 Nov".into();
    page.rebuild(None);
    let want = format!("{}\n\n{}", msg::settings_hud_catalog_cached(), msg::settings_hud_cached("3 Nov"));
    assert_eq!(page.sentence(), want);
    let first = page.list.catalog.index.as_ref().unwrap().entries[0].sha();
    focus_on(&mut page, &HudId::Catalog(first));
    drive(&mut page, tick(), 0);
    assert!(page.picture().is_some());
    assert_eq!(page.sentence(), want, "a row with no sentence of its own keeps it");
}

/// **Every sentence fits the left column in every language**: at most five lines at the reading size (the followed
/// sentence's cap), whatever page and state says it - a row's reason and what to do, a row's action, the page's own
/// (empty, loading, offline with the date), the notices. The language name is the longest native one.
#[test]
fn every_sentence_beside_the_table_fits_five_lines_in_every_language() {
    let layout = RouteLayout::settings_page();
    let mut out = Vec::new();
    for language in SHIPPED {
        let _guard = language_on_this_thread_for_test(language);
        let tag = language.tag();
        let mut every = list();
        every.language = SHIPPED.iter().map(|l| plx_platform::i18n::Preference::SHIPPED.into_iter().find(|p| p.tag() == l.tag()).map_or("", |p| p.native_name())).max_by_key(|n| n.chars().count()).unwrap_or("").to_owned();
        every.unfit = vec![Sha::of(&[3])];
        let mut unfit = pack("com.example.c", "Gamma", 3, false);
        unfit.until_restart = true;
        every.entries.push(unfit);
        for (i, why) in [Unloadable::NewerFormat, Unloadable::OlderFormat, Unloadable::Revoked, Unloadable::AppRange].into_iter().enumerate() {
            let mut gone = pack(&format!("com.example.u{i}"), "Delta", 60 + i as u8, false);
            gone.loadable = Err(why);
            every.entries.push(gone);
        }
        every.drops = every_refusal().iter().enumerate().map(|(i, r)| drop_file(&format!("f{i}.plxhud"), 20 + i as u8, Err(*r))).collect();
        let mut sentences: Vec<(String, String)> = hud_form(&every, None).1.into_iter().map(|(id, says)| (format!("{id:?}"), says)).collect();
        // the package and gallery pages: each state of Install, the offline and the empty gallery
        let gallery = gallery_list();
        let sha = gallery.catalog.index.as_ref().unwrap().entries[0].sha();
        for (state, edit) in [("fresh", 0u8), ("withdrawn", 1), ("unfit", 2), ("update", 3), ("offline", 4), ("loading", 5)] {
            let mut page = gallery_page(SettingsPage::HudDetail(sha));
            page.list.language = every.language.clone();
            match edit {
                1 => with_index(&mut page, |i| i.revoked.push(plx_data::hudpacks::catalog::Revocation { sha256: sha.hex(), reason: "A long reason the catalog gave for the withdrawal of this HUD.".into() })),
                2 => { page.list.unfit = vec![sha]; page.rebuild(None); }
                3 => { let mut e = pack("com.plxnative.examples.minimal", "Minimal", 9, false); e.meta.as_mut().unwrap().version = "0.1.0".into(); page.list.entries.push(e); page.rebuild(None); }
                4 => { page.list.catalog.refresh_failed = true; page.list.catalog_date = "30 September 2026".into(); page.rebuild(None); }
                _ => {}
            }
            sentences.extend(page.says.iter().map(|(id, says)| (format!("{state} {id:?}"), says.clone())));
            sentences.push((format!("{state} page"), page.copy.clone()));
            let mut gal = gallery_page(SettingsPage::HudGallery);
            gal.list.catalog.refresh_failed = edit == 4;
            gal.list.catalog_date = "30 September 2026".into();
            gal.list.catalog.fetching = edit == 5;
            gal.rebuild(None);
            sentences.push((format!("{state} gallery"), gal.copy.clone()));
        }
        let mut empty = gallery_page(SettingsPage::HudGallery);
        with_index(&mut empty, |i| i.entries.clear());
        sentences.push(("empty gallery".into(), empty.copy.clone()));
        for (name, text) in sentences {
            let size = theme::size::LABEL;
            let view = plx_ui::text_view::TextView::new(&text, size, theme::TEXT_READING)
                .leading(size as f32 + theme::space::XS)
                .max_lines(plx_ui::follow_copy::FOLLOW_LINES)
                .with_measure(&ShippedMeasure);
            if view.truncates(layout.narrative.w) {
                out.push(format!("{tag} {name}: over {} lines: {text}", plx_ui::follow_copy::FOLLOW_LINES));
            }
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// The ghost state reaches the pixels: the withdrawn HUD's row, with focus, is under the ghost pill and the solid one
/// stays under the usable row (the shared table's own test judges the inks; this judges the page wired them).
#[test]
fn the_pill_under_a_withdrawn_hud_is_the_ghost_and_under_the_built_in_the_solid() {
    let (mut page, _tx) = page_with(withdrawn_list());
    let solid_on = |page: &HudPage| page.form.table.ghost_cover_for_test();
    focus_on(&mut page, &HudId::Pack(BUILTIN_CLASSIC.into()));
    for _ in 0..240 { page.form.table.update(1.0 / 60.0, 600.0); }
    assert_eq!(solid_on(&page), 0.0);
    focus_on(&mut page, &HudId::Pack("com.example.a".into()));
    for _ in 0..240 { page.form.table.update(1.0 / 60.0, 600.0); }
    assert_eq!(solid_on(&page), 1.0);
}
