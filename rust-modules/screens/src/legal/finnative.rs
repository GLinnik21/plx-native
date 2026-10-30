//! FinNative's legal text: the contact it prints (no mailbox, so no `@`), the PlxNative it is built
//! on, and its privacy policy (one Jellyfin server, no reporting).
/// FinNative has no mailbox: its one contact is the project's issue tracker, so no document may
/// print an `@` at all.
#[cfg(feature = "finnative")]
pub const CONTACT_EMAIL: &str = "github.com/tiper/fin-native/issues";

/// The PlxNative this tree is built on, as PlxNative's own build of the merged upstream
/// commit names itself: `X.Y.Z` when the merge is at a release tag, the next minor with `-dev`
/// after it. About prints it under the build line. Set it at each upstream merge, beside keeping
/// FinNative's own number in the version files (the `cut-release` skill, "FinNative").
pub const PLXNATIVE_BASE: &str = "0.10.0-dev";

/// FinNative's policy: one Jellyfin server and no reporting, so none of the Plex services, the
/// unencrypted-connection answers or the optional reports PlxNative's policy describes.
pub(super) static PRIVACY: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| [
    plx_platform::i18n::msg::settings_legal_privacy_fin_responsible(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_responsible_body(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_jellyfin(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_jellyfin_body(),
    plx_platform::i18n::msg::settings_legal_privacy_local(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_local_body(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_reports(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_reports_body(),
    plx_platform::i18n::msg::settings_legal_privacy_retention(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_retention_body(),
    plx_platform::i18n::msg::settings_legal_privacy_uninstall(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_uninstall_body(),
    plx_platform::i18n::msg::settings_legal_privacy_contact(),
    plx_platform::i18n::msg::settings_legal_privacy_fin_contact_body()
].join("\n\n"));

#[cfg(test)]
mod tests {
    use super::super::{Page, ABOUT};
    use super::*;

    /// FinNative: About names the PlxNative it is built on, on a line of its own under the build,
    /// in the form PlxNative's own build reports (`X.Y.Z`, or the next minor `X.Y.0-dev`).
    #[test]
    fn about_names_the_plxnative_it_is_built_on() {
        assert!(
            ABOUT.contains(&format!("\nPlxNative {PLXNATIVE_BASE}\n")),
            "About should name PlxNative {PLXNATIVE_BASE}, says: {ABOUT:?}"
        );
        let (number, dev) = match PLXNATIVE_BASE.strip_suffix("-dev") {
            Some(n) => (n, true),
            None => (PLXNATIVE_BASE, false),
        };
        let parts: Vec<u32> = number.split('.').map(|c| c.parse().expect("a number")).collect();
        assert_eq!(parts.len(), 3, "{PLXNATIVE_BASE} is not X.Y.Z");
        assert!(!dev || parts[2] == 0, "a -dev base is the next minor, X.Y.0-dev: {PLXNATIVE_BASE}");
    }

    /// FinNative's policy: its one Jellyfin server, nothing reported anywhere, and none of the
    /// Plex services or reporting processors PlxNative's policy names.
    #[test]
    #[cfg(feature = "finnative")]
    fn the_finnative_policy_describes_a_jellyfin_client_that_reports_nothing() {
        let p = Page::Privacy.body();
        for claim in ["Jellyfin server", "never your password", "sends no crash reports", "recent searches", "RETENTION", "UNINSTALLING", "github.com/tiper"] {
            assert!(p.contains(claim), "the policy never mentions {claim:?}");
        }
        for stale in ["Plex", "Sentry", "PostHog", "Analytics ID", "Crash report ID", "PIN", "@"] {
            assert!(!p.contains(stale), "the policy still describes PlxNative: {stale:?}");
        }
    }
}
