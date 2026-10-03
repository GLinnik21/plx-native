//! **The clock reason on a failed Home or Library read-out** (issue #378's second half).
//!
//! A television has no battery clock: cold-booted offline its date is wrong and the server's
//! certificate fails its date check. `net::keypin` retries with the remembered key when it holds
//! one; when it CANNOT, the viewer lands on a failed Home or Library whose generic verdict does not
//! say why. [`reason_for`] gives that read-out a reason line and the clock glyph, and
//! [`ClockWatch`] is how a screen holds the fact so a frame's draw and its hit rects see ONE value.
//!
//! Not a screen: shared plumbing, named through `super::` like `plaintext_question`, so Home and
//! the Library share it without reaching each other.

use std::ffi::CStr;

use crate::net::keypin::Blocked;
use crate::ui::icons::Icon;

/// The reason line and glyph for why key mode cannot help, or `None` when nothing says it cannot.
pub(crate) fn reason_for(blocked: Option<Blocked>) -> Option<(&'static CStr, Icon)> {
    let reason = match blocked? {
        Blocked::NoKey => plx_platform::i18n::msg::browse_clock_no_key_c(),
        Blocked::KeyChanged => plx_platform::i18n::msg::browse_clock_key_changed_c(),
    };
    Some((reason, Icon::ClockBadgeAlert))
}

/// What a screen holds of `net::keypin::blocked_for` — the fact about the ONE server its read-out
/// speaks about — re-read only when `keypin::revision` moved or that server changed
/// (`OfferWatch`'s shape: the revision is global, the answer is per machine).
#[derive(Clone, Debug, Default)]
pub(crate) struct ClockWatch {
    seen: Option<(u64, Option<String>)>,
    held: Option<Blocked>,
}

impl ClockWatch {
    /// Re-read the fact about `machine` (`None`: the read-out speaks about no known server, so no
    /// server's fact applies); `true` when what the read-out shows changed.
    pub(crate) fn refresh(&mut self, machine: Option<&str>) -> bool {
        let rev = crate::net::keypin::revision();
        if matches!(&self.seen, Some((r, m)) if *r == rev && m.as_deref() == machine) {
            return false;
        }
        self.seen = Some((rev, machine.map(str::to_owned)));
        // The empty id names no server: only a dev-planted fact answers it.
        let next = crate::net::keypin::blocked_for(machine.unwrap_or_default());
        std::mem::replace(&mut self.held, next) != next
    }

    /// The fact as of the last [`refresh`](Self::refresh).
    #[cfg(test)]
    pub(crate) fn blocked(&self) -> Option<Blocked> {
        self.held
    }

    /// Its reason line and glyph ([`reason_for`]).
    pub(crate) fn reason(&self) -> Option<(&'static CStr, Icon)> {
        reason_for(self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;
    use crate::ui::fit::HEADROOM;
    use plx_platform::i18n::{language_on_this_thread_for_test, msg};
    use crate::net::keypin;
    use crate::ui::widgets::StatusOverlay;

    #[test]
    fn each_cause_names_its_reason_and_the_clock_glyph_and_nothing_names_nothing() {
        assert_eq!(reason_for(None), None);
        assert_eq!(
            reason_for(Some(Blocked::NoKey)),
            Some((msg::browse_clock_no_key_c(), Icon::ClockBadgeAlert))
        );
        assert_eq!(
            reason_for(Some(Blocked::KeyChanged)),
            Some((msg::browse_clock_key_changed_c(), Icon::ClockBadgeAlert))
        );
        assert_eq!(
            msg::browse_clock_no_key(),
            "This TV’s clock looks wrong. Connect the TV to the internet once, then try again."
        );
        assert_eq!(
            msg::browse_clock_key_changed(),
            "Your server’s key has changed. Connect the TV to the internet once so the app can check it again."
        );
    }

    /// The watch follows `keypin::revision` AND the machine it is asked about: it re-reads when a
    /// fact moved or the read-out changed server and says so, and a second refresh over the same
    /// pair changes nothing and reports nothing. Another server's fact never shows.
    #[test]
    fn the_watch_re_reads_when_the_revision_or_the_machine_moves_and_not_otherwise() {
        let _serial = plx_base::testlock::serial();
        let key = keypin::key_of("clock-watch.invalid", 32400);
        let _scoped = keypin::Scoped::watch_machine("clock-watch-machine", &key);
        let here = Some("clock-watch-machine");
        let mut watch = ClockWatch::default();
        watch.refresh(here);
        assert_eq!(watch.blocked(), None, "no fact stands for this machine");
        let before = watch.blocked();
        keypin::strict_failure(&key, 60, Some(10));
        assert_eq!(watch.blocked(), before, "a held value does not change under the screen mid-frame");
        assert!(watch.refresh(here), "the revision moved");
        assert_eq!(watch.blocked(), Some(Blocked::NoKey));
        assert!(!watch.refresh(here), "nothing moved since");

        assert!(watch.refresh(Some("clock-watch-other")), "another server: the fact is not its own");
        assert_eq!(watch.blocked(), None);
        assert!(!watch.refresh(None), "no server at all: no fact, so nothing the read-out shows moved");
        assert!(watch.refresh(here), "back to the server the fact is about");
        assert_eq!(watch.blocked(), Some(Blocked::NoKey));
    }

    /// **Both reasons fit the two-line slot in every shipped language**: nothing else fit-tests
    /// Home's or a Library source's reason line. A reason that truncates loses its remedy.
    #[test]
    fn both_reasons_fit_the_two_line_slot_in_every_shipped_language() {
        let mut out = Vec::new();
        for language in plx_platform::i18n::SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            for cause in [Blocked::NoKey, Blocked::KeyChanged] {
                let (reason, _) = reason_for(Some(cause)).expect("a cause has a reason");
                if StatusOverlay::failed_reason_truncates(reason, &ShippedMeasure, HEADROOM) {
                    out.push(format!("{} {cause:?}: {reason:?}", language.tag()));
                }
            }
        }
        assert!(out.is_empty(), "reasons the read-out would end in an ellipsis:\n  {}", out.join("\n  "));
    }
}
