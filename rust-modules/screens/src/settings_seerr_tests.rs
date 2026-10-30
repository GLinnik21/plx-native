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

fn method(out: &[Stamped<InnerHost>]) -> Option<Option<String>> {
    out.iter().find_map(|s| match &s.fx {
        Fx::App(AppFx::Preferences(crate::registry::PreferenceCmd::SeerrMethod { email, .. })) => Some(email.clone()),
        _ => None,
    })
}

fn signs_in(out: &[Stamped<InnerHost>]) -> Option<(String, String)> {
    out.iter().find_map(|s| match &s.fx {
        Fx::App(AppFx::Preferences(crate::registry::PreferenceCmd::SeerrSignIn { email, password })) =>
            Some((email.clone(), password.clone())),
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
fn the_address_is_typed_in_its_own_row_above_the_raised_keyboard() {
    for address in ["", "http://192.168.1.20:5055"] {
        let store = store(address);
        let page = SeerrPage::new(EntryId(0), &cx(&store));
        let field = field_rect(&page.form, &SeerrId::Address).expect("the Address row is on screen");
        let i = page.form.index_of(&SeerrId::Address).unwrap() as i32;
        let row = page.form.table.row_rect(RouteLayout::screen().sectioned_table(), i).unwrap();
        assert!(field.y >= row.y && field.y + field.h <= row.y + row.h, "the field is drawn in the row");
        let keyboard = plx_ui::text_field::SYSTEM_KEYBOARD_TOP;
        assert!(field.y + field.h <= keyboard, "the field ends at {} and the keyboard starts at {keyboard}",
            field.y + field.h);
        assert!(field.h >= theme::size::TITLE as f32, "the row is tall enough for the field's run");
    }
}

fn with_account(address: &str, email: &str) -> SeerrStore {
    let mut store = store(address);
    store.set_account(SeerrAccount::Account { email: email.into(), kept: None });
    store
}

fn type_into(page: &mut SeerrPage, store: &SeerrStore, id: SeerrId, text: &str) -> Vec<Stamped<InnerHost>> {
    step(page, store, ScreenEvent::Activate(id.key().0));
    step(page, store, input(InputKind::Text(TextEdit::Clear)));
    for c in text.chars() {
        step(page, store, input(InputKind::Text(TextEdit::Commit(c.to_string().into()))));
    }
    step(page, store, key(Key::Ok))
}

#[test]
fn sign_in_with_switches_between_plex_and_a_seerr_account() {
    let s = store("http://192.0.2.10:5055");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    assert_eq!(row_value(&page, SeerrId::Method).as_deref(), Some(msg::settings_seerr_method_plex()));
    assert!(page.form.index_of_key(SeerrId::Email.key()).is_none(), "Plex asks for no email");
    let out = step(&mut page, &s, ScreenEvent::Activate(SeerrId::Method.key().0));
    assert_eq!(method(&out), Some(Some(String::new())), "a Seerr account, its email not yet typed");
    assert_eq!(row_value(&page, SeerrId::Method).as_deref(), Some(msg::settings_seerr_method_account()), "shown at once");
    assert!(page.form.index_of_key(SeerrId::Email.key()).is_some() && page.form.index_of_key(SeerrId::Password.key()).is_some());
    let out = step(&mut page, &s, ScreenEvent::Activate(SeerrId::Method.key().0));
    assert_eq!(method(&out), Some(None), "back to Plex");
}

#[test]
fn the_email_is_written_and_the_password_only_goes_with_sign_in() {
    let s = with_account("http://192.0.2.10:5055", "");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    let out = type_into(&mut page, &s, SeerrId::Email, "viewer@example.invalid");
    assert_eq!(method(&out), Some(Some("viewer@example.invalid".into())));
    assert!(signs_in(&step(&mut page, &s, ScreenEvent::Activate(SeerrId::Connect.key().0))).is_none(),
        "no password typed: Sign in does nothing");
    let out = type_into(&mut page, &s, SeerrId::Password, "pass word");
    assert_eq!((method(&out), written(&out)), (None, None), "a typed password is written nowhere");
    assert_eq!(row_value(&page, SeerrId::Password).as_deref(), Some(msg::settings_seerr_password_set()));
    let out = step(&mut page, &s, ScreenEvent::Activate(SeerrId::Connect.key().0));
    assert_eq!(signs_in(&out), Some(("viewer@example.invalid".into(), "pass word".into())), "kept as typed");
    assert!(page.password.is_none(), "Sign in carries the password away");
    assert_eq!(row_value(&page, SeerrId::Password).as_deref(), Some(msg::settings_seerr_password_unset()));
}

#[test]
fn the_password_is_drawn_masked_and_dropped_when_the_page_goes() {
    let s = with_account("http://192.0.2.10:5055", "viewer@example.invalid");
    let mut page = SeerrPage::new(EntryId(0), &cx(&s));
    step(&mut page, &s, ScreenEvent::Activate(SeerrId::Password.key().0));
    step(&mut page, &s, input(InputKind::Text(TextEdit::Commit("pw".into()))));
    let mut budget = Budget::default();
    page.prepare(&mut budget, &cx(&s));
    assert_eq!(page.field.text(), "**");
    step(&mut page, &s, key(Key::Ok));
    assert!(page.password.is_some());
    step(&mut page, &s, ScreenEvent::WillLeave(plx_machine::machine::Leave::ForGood));
    assert!(page.password.is_none());
}

#[test]
fn every_typed_row_is_typed_in_above_the_raised_keyboard() {
    let s = with_account("http://192.0.2.10:5055", "viewer@example.invalid");
    let page = SeerrPage::new(EntryId(0), &cx(&s));
    for id in [SeerrId::Address, SeerrId::Email, SeerrId::Password] {
        let field = field_rect(&page.form, &id).expect("on screen");
        assert!(field.y + field.h <= plx_ui::text_field::SYSTEM_KEYBOARD_TOP, "{id:?} ends at {}", field.y + field.h);
    }
}
