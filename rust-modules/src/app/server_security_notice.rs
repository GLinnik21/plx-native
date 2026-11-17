//! Warn about the Plex Media Server builds affected by Plex's 2026 security advisory.
//!
//! A version is only a reason to advise an update, never proof that a server is clean. Unknown or
//! incomplete versions stay unknown. Each registered server is warned at most once per app run;
//! separate notices are spaced so simultaneous discoveries do not overwrite one another.

use std::collections::BTreeSet;
use plx_plex::plex;

#[derive(Debug, PartialEq, Eq)]
enum Status { Affected, Fixed, Unknown }

fn status(version: &str) -> Status {
    let numbers = version.split_once('-').map_or(version, |(numbers, _)| numbers);
    let mut fields = numbers.split('.');
    let mut parts = [0u32; 4];
    for part in &mut parts {
        let Some(field) = fields.next() else { return Status::Unknown };
        if field.is_empty() || !field.bytes().all(|c| c.is_ascii_digit()) { return Status::Unknown; }
        let Ok(number) = field.parse() else { return Status::Unknown };
        *part = number;
    }
    if fields.next().is_some() { return Status::Unknown; }
    if parts < [1, 43, 3, 10861] { Status::Affected } else { Status::Fixed }
}

#[derive(Default)]
pub(crate) struct ServerSecurityNotice {
    told: BTreeSet<u16>,
    last_poll: Option<u32>,
    last_notice: Option<u32>,
}

impl ServerSecurityNotice {
    pub(crate) fn poll(&mut self, now: u32, playing: bool) {
        let notice = self.poll_with(now, playing, || {
            plex::server_ids().map(|id| {
                let name = plex::server_facts(id).map(|f| f.name.clone()).unwrap_or_default();
                (id.raw(), name, plex::serverinfo::version_of(id))
            }).collect()
        });
        if let Some(message) = notice {
            plx_base::task::spawn_small("server security notice", move || {
                let outcome = plx_platform::tv::toast::toast(&message);
                plx_base::eventlog::log(&format!("server security notice: toast {outcome:?}"));
            });
        }
    }

    /// Snapshot the current roster only when a notice could be due. A failed or unknown version
    /// is left untold, so a later successful fetch can still trigger its notice.
    fn poll_with(&mut self, now: u32, playing: bool,
        servers: impl FnOnce() -> Vec<(u16, String, String)>) -> Option<String> {
        if playing || self.last_poll.is_some_and(|t| now.wrapping_sub(t) < 2000)
            || self.last_notice.is_some_and(|t| now.wrapping_sub(t) < 15_000) { return None; }
        self.last_poll = Some(now);
        for (id, name, version) in servers() {
            if self.told.contains(&id) || status(&version) != Status::Affected { continue; }
            self.told.insert(id);
            self.last_notice = Some(now);
            return Some(message(&name));
        }
        None
    }
}

fn message(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(24).collect();
    let name = if name.trim().is_empty() { "Plex" } else { name.trim() };
    plx_platform::i18n::msg::browse_server_security_notice(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_full_build_and_treats_unparseable_versions_as_unknown() {
        for version in ["1.43.2.99999-abc", "1.43.3.10860-abc", "1.42.9.99999-x"] {
            assert_eq!(status(version), Status::Affected, "{version}");
        }
        for version in ["1.43.3.10861-abc", "1.43.3.10896-abc", "1.43.4.1-x", "2.0.0.0-x"] {
            assert_eq!(status(version), Status::Fixed, "{version}");
        }
        for version in ["", "1.43.3", "1.43.3.x", "1.43.3.10861.2", "1.43.3.10861junk", " 1.43.3.10861"] {
            assert_eq!(status(version), Status::Unknown, "{version}");
        }
    }
    #[test]
    fn warnings_wait_for_known_versions_and_playback_then_space_each_server_once() {
        let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let servers = |a: &str| vec![(1, "Living room".into(), a.into()),
            (2, "Family".into(), "1.43.2.9000-hash".into())];
        let mut notice = ServerSecurityNotice::default();
        assert!(notice.poll_with(0, true, || servers("1.43.2.1-x")).is_none());
        assert!(notice.poll_with(1, false, || vec![(1, "Living room".into(), "".into())]).is_none());
        assert!(notice.poll_with(2001, false, || servers("1.43.2.1-x")).unwrap().contains("Living room"));
        assert!(notice.poll_with(17000, false, || servers("1.43.2.1-x")).is_none());
        assert!(notice.poll_with(17001, false, || servers("1.43.2.1-x")).unwrap().contains("Family"));
        assert!(notice.poll_with(40000, false, || servers("1.43.2.1-x")).is_none());
    }
    #[test]
    fn interval_survives_wrapping_clock() {
        let mut notice = ServerSecurityNotice::default();
        let servers = || vec![(1, "Server".into(), "1.43.2.1-x".into())];
        assert!(notice.poll_with(u32::MAX - 1000, false, servers).is_some());
        assert!(notice.poll_with(1000, false, || vec![(2, "Other".into(), "1.43.2.1-x".into())]).is_none());
        assert!(notice.poll_with(14000, false, || vec![(2, "Other".into(), "1.43.2.1-x".into())]).is_some());
    }
    #[test]
    fn notices_fit_the_television_toast_and_sanitize_server_names() {
        for language in plx_platform::i18n::SHIPPED {
            let _guard = plx_platform::i18n::language_on_this_thread_for_test(language);
            let text = message("ABCDEFGHIJKLMNOPQRSTUVWX\nignored");
            assert!(text.chars().count() <= 120, "{}: {text}", language.tag());
            assert!(!text.contains('\n'));
            assert!(text.contains("ABCDEFGHIJKLMNOPQRSTUVWX"));
            let (mut lines, mut width) = (1, 0);
            for word in text.split_whitespace() {
                let length = word.chars().count();
                if width != 0 && width + 1 + length > 40 {
                    lines += 1;
                    width = length;
                } else {
                    width += if width == 0 { length } else { length + 1 };
                }
            }
            assert!(lines <= 3, "{}: {text}", language.tag());
        }
    }
}
