//! Settings > Services > Seerr, stepped directly with a hand-built context whose views carry a real
//! (never-run) Seerr store: the rows, the keyboard and the effects the page emits.
use super::*;
use plx_data::stores::seerr::SeerrStore;
use plx_machine::machine::{FocusRead, InputOwner, PressRead, Source, Stamped};
use plx_machine::present::Present;

fn store(address: &str) -> SeerrStore {
    let mut store = SeerrStore::default();
    store.set_address(address);
    store
}

fn cx(store: &SeerrStore) -> Cx<'_, InnerHost> {
    Cx {
        views: crate::registry::InnerViews {
            directory: plx_data::stores::browse::DirectoryView::empty_for_test(),
            seerr: Some(store),
        },
        tick: Tick::default(),
        measure: &super::super::test_support::MEASURE,
        press: PressRead::default(),
        focus: FocusRead::default(),
        owner: InputOwner::Entry(EntryId(0)),
    }
}

fn step(page: &mut SeerrPage, store: &SeerrStore, ev: ScreenEvent<InnerHost>) -> Vec<Stamped<InnerHost>> {
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let _ = <SeerrPage as Machine<InnerHost>>::step(page, &ev, &cx(store), &mut fx);
    out
}

fn input(kind: InputKind<u32>) -> ScreenEvent<InnerHost> {
    ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::Script, kind })
}

fn key(key: Key) -> ScreenEvent<InnerHost> {
    input(InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false })
}

fn keyboard(out: &[Stamped<InnerHost>]) -> Option<bool> {
    out.iter().find_map(|s| match &s.fx {
        Fx::Deliver(_, Delivery::Keyboard { up }) => Some(*up),
        _ => None,
    })
}

fn written(out: &[Stamped<InnerHost>]) -> Option<String> {
    out.iter().find_map(|s| match &s.fx {
        Fx::App(AppFx::Preferences(crate::registry::PreferenceCmd::SeerrAddress { address, .. })) => Some(address.clone()),
        _ => None,
    })
}

fn connects(out: &[Stamped<InnerHost>]) -> bool {
    out.iter().any(|s| matches!(&s.fx, Fx::App(AppFx::Preferences(crate::registry::PreferenceCmd::SeerrConnect))))
}

fn row_value(page: &SeerrPage, id: SeerrId) -> Option<String> {
    let i = page.form.index_of_key(id.key())?;
    page.form.table.sections.iter().flat_map(|s| &s.rows).nth(i).and_then(|row| row.value.clone())
}

#[test]
fn the_root_row_says_whether_an_address_is_set() {
    assert_eq!(summary(""), msg::settings_seerr_value_off());
    assert_eq!(summary("  "), msg::settings_seerr_value_off());
    assert_eq!(summary("http://192.0.2.10:5055"), msg::settings_seerr_value_on());
}

#[test]
fn the_connect_line_states_the_connection() {
    assert_eq!(status_text("", Some(&Connection::Connecting)), msg::settings_seerr_status_off(), "no address says so first");
    for (connection, want) in [
        (Connection::Idle, msg::settings_seerr_status_idle().to_string()),
        (Connection::Connecting, msg::settings_seerr_status_connecting().to_string()),
        (Connection::Connected { name: "s00000004".into() }, msg::settings_seerr_status_connected("s00000004")),
        (Connection::NotAllowed, msg::settings_seerr_status_not_allowed().to_string()),
        (Connection::Unreachable, msg::settings_seerr_status_unreachable().to_string()),
        (Connection::BadAddress, msg::settings_seerr_status_bad_address().to_string()),
    ] {
        assert_eq!(status_text("h", Some(&connection)), want, "{connection:?}");
    }
}

#[test]
fn with_no_address_connect_is_dim_and_remove_is_not_offered() {
    let empty = store("");
    let page = SeerrPage::new(EntryId(0), &cx(&empty));
    assert_eq!(row_value(&page, SeerrId::Address).as_deref(), Some(msg::settings_seerr_address_unset()));
    assert!(page.form.index_of_key(SeerrId::Remove.key()).is_none());
    let set = store("http://192.0.2.10:5055");
    let page = SeerrPage::new(EntryId(0), &cx(&set));
    assert_eq!(row_value(&page, SeerrId::Address).as_deref(), Some("http://192.0.2.10:5055"));
    assert!(page.form.index_of_key(SeerrId::Remove.key()).is_some());
}

#[test]
fn ok_on_the_address_types_it_and_ok_again_writes_it() {
    let s = store("");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    let out = step(&mut page, &s, ScreenEvent::Activate(SeerrId::Address.key().0));
    assert_eq!(keyboard(&out), Some(true), "the television's keyboard comes up");
    assert!(page.state.editing);
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit("192.0.2.10:5055".into()))));
    step(&mut page, &s, input(InputKind::Key { key: Key::Other, sym: plx_ui::consts::SDLK_BACKSPACE, wcode: 0,
        edge: Edge::Down, at_edge: false }));
    // One key, one commit: a multi-character commit at the end is the panel's PREDICTION and
    // replaces the last word (`TextBuffer::commit`), which is not what a typed digit does.
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit("6".into()))));
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit(" ".into()))));
    let out = step(&mut page, &s, key(Key::Ok));
    assert_eq!(keyboard(&out), Some(false));
    assert_eq!(written(&out).as_deref(), Some("192.0.2.10:5056"), "trimmed, and the edit keys edited");
    assert!(!page.state.editing);
    assert_eq!(row_value(&page, SeerrId::Address).as_deref(), Some("192.0.2.10:5056"), "shown while the write runs");
}

#[test]
fn back_while_typing_puts_the_address_back_and_stays_on_the_page() {
    let s = store("http://192.0.2.10:5055");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    step(&mut page, &s, ScreenEvent::Activate(SeerrId::Address.key().0));
    step(&mut page, &s, input(InputKind::Text(TextEdit::Clear)));
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let handled = <SeerrPage as Machine<InnerHost>>::step(&mut page, &key(Key::Back), &cx(&s), &mut fx);
    assert_eq!(handled, Handled::Yes, "BACK ends the typing, it does not leave the page");
    assert_eq!((keyboard(&out), written(&out)), (Some(false), None));
    assert_eq!(page.state.address, "http://192.0.2.10:5055");
}

#[test]
fn the_panel_closing_by_its_own_hand_keeps_what_was_typed() {
    let s = store("");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    step(&mut page, &s, ScreenEvent::Activate(SeerrId::Address.key().0));
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit("seerr.lan:5055".into()))));
    let out = step(&mut page, &s, input(InputKind::SystemKeyboard(false)));
    assert_eq!(written(&out).as_deref(), Some("seerr.lan:5055"));
    assert_eq!(keyboard(&out), None, "the panel is already down: nothing to lower");
}

#[test]
fn connect_signs_in_only_with_an_address_and_remove_forgets_it() {
    let empty = store("");
    let mut page = SeerrPage::new(EntryId(0), &cx(&empty));
    assert!(!connects(&step(&mut page, &empty, ScreenEvent::Activate(SeerrId::Connect.key().0))));
    let set = store("http://192.0.2.10:5055");
    let mut page = SeerrPage::new(EntryId(0), &cx(&set));
    assert!(connects(&step(&mut page, &set, ScreenEvent::Activate(SeerrId::Connect.key().0))));
    let out = step(&mut page, &set, ScreenEvent::Activate(SeerrId::Remove.key().0));
    assert_eq!(written(&out).as_deref(), Some(""));
}

#[test]
fn leaving_while_typing_lowers_the_keyboard_and_writes_nothing() {
    let s = store("");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    step(&mut page, &s, ScreenEvent::Activate(SeerrId::Address.key().0));
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit("half".into()))));
    let out = step(&mut page, &s, ScreenEvent::WillLeave(plx_machine::machine::Leave::ForGood));
    assert_eq!((keyboard(&out), written(&out)), (Some(false), None));
}

#[test]
fn the_rows_follow_the_stored_address_and_the_connection() {
    let mut s = store("");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    s.set_address("http://192.0.2.10:5055");
    step(&mut page, &s, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 }));
    assert_eq!(row_value(&page, SeerrId::Address).as_deref(), Some("http://192.0.2.10:5055"));
    let connect = page.form.index_of_key(SeerrId::Connect.key()).unwrap();
    let row = page.form.table.sections.iter().flat_map(|s| &s.rows).nth(connect).unwrap();
    assert_eq!(row.detail, msg::settings_seerr_status_idle());
}

#[test]
fn the_address_field_stands_above_the_raised_keyboard() {
    let keyboard = plx_ui::text_field::SYSTEM_KEYBOARD_TOP;
    assert!(panel().y + panel().h <= keyboard, "the panel ends at {} and the keyboard starts at {keyboard}",
        panel().y + panel().h);
    assert!(field_rect().y + field_rect().h <= keyboard, "the field is typed into while the keyboard is up");
}

#[test]
fn the_copy_ends_above_the_address_panel_in_every_language() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::text_view::TextView;
    let _g = plx_base::testlock::serial();
    let layout = RouteLayout::screen();
    let mut out = Vec::new();
    for language in plx_platform::i18n::SHIPPED {
        let _lang = plx_platform::i18n::language_on_this_thread_for_test(language);
        let tag = language.tag();
        let frame = layout.narrative_copy_frame(true, msg::settings_seerr_title(), layout.action.y, &ShippedMeasure);
        let copy_h = TextView::new(msg::settings_seerr_copy(), theme::size::LABEL, theme::TEXT_READING)
            .leading(theme::size::LABEL as f32 + theme::space::XS)
            .max_lines(12)
            .with_measure(&ShippedMeasure)
            .measure_h(layout.narrative.w);
        if frame.y + copy_h + theme::space::MD > panel().y {
            out.push(format!("{tag}: the copy ends at {:.0}, under the address panel at {:.0}", frame.y + copy_h, panel().y));
        }
    }
    assert!(out.is_empty(), "{out:#?}");
}
