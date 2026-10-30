//! FinNative's Privacy & data differences: a build with no reporting configured (no Sentry DSN, no
//! PostHog key, which is every FinNative build) can send nothing, so there is nothing to ask about
//! and nothing to switch.
use super::*;

/// Can this build send a report at all? Always in PlxNative; in FinNative only when a Sentry DSN
/// or a PostHog key was compiled in, which no FinNative build does. Without it there is nothing to
/// switch: Privacy & data keeps only the policy and the delete. The page's own tests exercise the
/// switches, so a test build answers yes unless [`reporting_off_for_test`] says otherwise.
pub(super) fn reporting_configured() -> bool {
    #[cfg(test)]
    return !REPORTING_OFF.with(|off| off.get());
    #[cfg(all(not(test), feature = "finnative"))]
    return plx_telemetry::telemetry::sender::configured();
    #[cfg(all(not(test), not(feature = "finnative")))]
    true
}

/// The first-run question has nothing to ask when no report can be sent.
#[cfg(feature = "finnative")]
pub(super) fn can_report() -> bool {
    plx_telemetry::telemetry::sender::configured()
}

#[cfg(test)]
thread_local! {
    static REPORTING_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build this thread's pages as a build that can send nothing does.
#[cfg(test)]
pub(crate) fn reporting_off_for_test(off: bool) {
    REPORTING_OFF.with(|c| c.set(off));
}

/// The page of a build that can send nothing: the policy, and Delete all local data.
pub(super) fn policy_and_delete_form() -> Form<RowId, Act, SettingsPage> {
    Form::new()
            .section(
                FormSection::new(plx_platform::i18n::msg::settings_consent_information())
                    .item(RowId::Policy, RowKind::Nav(SettingsPage::Preview(
                        PreviewKind::ALL.iter().position(|k| *k == PreviewKind::Policy).unwrap_or(0) as u8,
                    )), Act::Open,
                        Row::new(plx_platform::i18n::msg::settings_consent_policy()).detail(plx_platform::i18n::msg::settings_consent_preview_policy()).chevron(true)),
            )
            .section(
                FormSection::new(plx_platform::i18n::msg::settings_consent_on_tv()).item(
                    RowId::Delete, RowKind::Button, Act::Delete,
                    Row::new(plx_platform::i18n::msg::settings_consent_delete()).detail(plx_platform::i18n::msg::settings_consent_delete_detail()).chevron(true),
                ),
            )
}

#[cfg(test)]
#[test]
fn a_build_that_cannot_report_offers_only_the_policy_and_the_delete() {
    reporting_off_for_test(true);
    let mut form: FormTable<RowId, Act, SettingsPage> = FormTable::new(crate::registry::BAND);
    form.set(consent_form(Mode::Settings, (false, false)), None);
    let rows: Vec<RowId> = (0..form.table.n_rows() as usize).filter_map(|i| form.id_at(i).copied()).collect();
    reporting_off_for_test(false);
    assert_eq!(rows, [RowId::Policy, RowId::Delete]);
}

