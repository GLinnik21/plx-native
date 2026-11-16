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

use plx_net::net::keypin::Blocked;
use plx_ui::icons::Icon;

/// The reason line and glyph for why key mode cannot help, or `None` when nothing says it cannot.
pub fn reason_for(blocked: Option<Blocked>) -> Option<(&'static CStr, Icon)> {
    let reason = match blocked? {
        Blocked::NoKey => plx_platform::i18n::msg::browse_clock_no_key_c(),
        Blocked::KeyChanged => plx_platform::i18n::msg::browse_clock_key_changed_c(),
    };
    Some((reason, Icon::ClockBadgeAlert))
}

/// The captured reason held for the one server this read-out describes. The immutable
/// publication may be replaced independently of its revision, so compare the reason itself.
#[derive(Clone, Debug, Default)]
pub struct ClockWatch {
    held: Option<Blocked>,
}

impl ClockWatch {
    /// Read the captured fact for `machine`; `None` selects the explicit empty-id fact.
    /// Return `true` only when the held reason changed.
    pub fn refresh(&mut self, facts: Option<&plx_plex::plex::ClockFacts>, machine: Option<&str>) -> bool {
        let next = facts.and_then(|facts| facts.blocked_for(machine));
        std::mem::replace(&mut self.held, next) != next
    }

    /// The fact as of the last [`refresh`](Self::refresh).
    #[cfg(test)]
    pub fn blocked(&self) -> Option<Blocked> {
        self.held
    }

    /// Its reason line and glyph ([`reason_for`]).
    pub fn reason(&self) -> Option<(&'static CStr, Icon)> {
        reason_for(self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::fit::HEADROOM;
    use plx_platform::i18n::{language_on_this_thread_for_test, msg};

    use plx_ui::widgets::StatusOverlay;

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
        use plx_plex::plex::{ClockFacts, ServerId};
        let make = |reason| ClockFacts::scripted(7, ServerId::UNSET, None,
            [(ServerId::UNSET, "machine".to_owned(), reason)]);
        let no_key = make(Some(Blocked::NoKey));
        let changed = make(Some(Blocked::KeyChanged));
        let mut watch = ClockWatch::default();
        assert!(watch.refresh(Some(&no_key), Some("machine")));
        assert_eq!(watch.blocked(), Some(Blocked::NoKey));
        assert!(!watch.refresh(Some(&no_key), Some("machine")));
        assert!(watch.refresh(Some(&changed), Some("machine")), "equal revisions may carry different facts");
        assert_eq!(watch.blocked(), Some(Blocked::KeyChanged));
        assert!(watch.refresh(Some(&changed), Some("other")));
        assert!(!watch.refresh(Some(&changed), None));
        assert!(watch.refresh(Some(&no_key), Some("machine")));
        assert!(watch.refresh(None, Some("machine")));
    }

    /// **Both reasons fit the two-line slot in every shipped language**: nothing else fit-tests
    /// Home's or a Library source's reason line. A reason that truncates loses its remedy.
    #[test]
    fn clock_consumer_sites_do_not_read_ambient_sources() {
        let cases = [
            (include_str!("clock_readout.rs"), "pub fn refresh(", "pub fn blocked("),
            (include_str!("home/mod.rs"), "fn tick<H: HomeLike>", "self.plaintext_alert.update(dt)"),
            (include_str!("library/status.rs"), "pub(super) fn watch_readout", "pub(super) fn status_rect"),
        ];
        for (source, start, end) in cases {
            let body = source.split_once(start).expect("consumer start").1.split_once(end).expect("consumer end").0;
            for forbidden in [concat!("keypin::", "revision"), concat!("keypin::", "blocked_for"),
                concat!("plex::", "client_for"), concat!("plex::", "current_server")] {
                assert!(!body.contains(forbidden), "ambient read: {forbidden}");
            }
        }
    }
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
