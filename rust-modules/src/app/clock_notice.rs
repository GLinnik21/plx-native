//! Tell the viewer, once per app run, that the television's clock is wrong and the app is serving
//! the server by its remembered key (issue #378's key mode).
//!
//! `net::keypin` publishes the fact ([`keypin::engaged`], and a [`keypin::revision`] that moves
//! when any fact changes); this is the one consumer that acts on it, through the television's own
//! toast ([`crate::webos::toast`]). It is polled from the frame loop, so it works on every route:
//! at an offline cold boot key mode first engages during the startup connect, long before any
//! screen could carry a read-out of its own.
//!
//! **Once per run, and never retried.** The flag is set when the attempt is MADE, not when it is
//! accepted: a television that refuses the toast would otherwise be asked again on every
//! revision, and a person who did not see the first one is not helped by the fortieth. The latch
//! lapsing and re-engaging (every ten minutes of key mode) changes nothing here, because
//! `engaged` records the first engagement and the flag is already down.
//!
//! The toast call blocks for an LS2 round trip, so the message is built here, on the frame thread
//! (the locale is read from it), and the call runs on a small worker.

use crate::net::keypin;

/// What the frame loop owns between polls.
pub(crate) struct ClockNotice {
    /// The [`keypin::revision`] last looked at; `None` until the first poll, so a fact published
    /// before the loop's first frame is still seen.
    seen: Option<u64>,
    /// An attempt was made. Never reset.
    told: bool,
}

impl ClockNotice {
    pub(crate) fn new() -> Self {
        Self { seen: None, told: false }
    }

    /// One frame's look: an atomic load unless a fact moved.
    pub(crate) fn poll(&mut self) {
        if let Some(message) = self.step(keypin::revision(), keypin::engaged()) {
            send(message);
        }
    }

    /// The decision, free of the bus and the process-wide facts: the message to show now, if this
    /// is the one time it is due. `engaged` is [`keypin::engaged`] as read at `revision`.
    fn step(&mut self, revision: u64, engaged: Option<Option<i64>>) -> Option<String> {
        if self.told || self.seen == Some(revision) {
            return None;
        }
        self.seen = Some(revision);
        let year = engaged?;
        self.told = true;
        Some(message(year))
    }
}

/// The toast's text: the year the device believed when key mode first engaged, when it is known.
/// The year is passed as text: a catalog number argument is locale-formatted, and a year is not a
/// quantity ("2,020").
fn message(year: Option<i64>) -> String {
    use crate::i18n::msg;
    match year {
        Some(y) => msg::browse_clock_notice_year(&y.to_string()),
        None => msg::browse_clock_notice().to_owned(),
    }
}

/// Raise `message` off the frame thread and log what became of it, once.
fn send(message: String) {
    crate::task::spawn_small("clock notice", move || {
        let outcome = crate::webos::toast::toast(&message);
        crate::log(&format!("clock notice: toast {outcome:?}"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_fires_exactly_once_however_often_the_facts_move() {
        let mut notice = ClockNotice::new();
        assert_eq!(notice.step(1, None), None, "a revision with nothing engaged tells nobody");
        assert_eq!(notice.step(1, None), None);
        let first = notice.step(2, Some(Some(2020)));
        assert!(first.is_some_and(|m| m.contains("2020")), "the first engagement is told");
        // Re-engagements after a latch lapse, a blocked fact appearing or clearing, and the same
        // revision read again: none of them tell it twice.
        assert_eq!(notice.step(2, Some(Some(2020))), None);
        assert_eq!(notice.step(3, Some(Some(2020))), None);
        assert_eq!(notice.step(9, Some(Some(2031))), None);
        assert_eq!(notice.step(10, Some(None)), None);
    }

    #[test]
    fn a_fact_published_before_the_first_poll_is_still_seen() {
        let mut notice = ClockNotice::new();
        assert!(notice.step(0, Some(Some(2020))).is_some(), "revision 0 is a revision like another");
    }

    #[test]
    fn it_does_not_burn_its_one_chance_on_an_unengaged_revision() {
        let mut notice = ClockNotice::new();
        for revision in 1..5 {
            assert_eq!(notice.step(revision, None), None);
        }
        assert!(notice.step(5, Some(None)).is_some());
    }

    #[test]
    fn the_message_names_the_year_only_when_it_is_known() {
        let _en = crate::i18n::language_on_this_thread_for_test(crate::i18n::Preference::En);
        assert_eq!(message(Some(2020)), "TV clock looks wrong (2020). Connected by the remembered key.");
        assert_eq!(message(None), "TV clock looks wrong. Connected by the remembered key.");
        // Not locale-grouped: a year is not a quantity.
        assert!(!message(Some(2020)).contains("2,020"));
    }

    /// The system toast wraps at roughly 35 characters a line and two lines are proven to show.
    #[test]
    fn every_shipped_language_fits_the_toast() {
        const LIMIT: usize = 70;
        const LINE: usize = 35;
        for language in crate::i18n::SHIPPED {
            let _guard = crate::i18n::language_on_this_thread_for_test(language);
            for (what, text) in [("with a year", message(Some(2020))), ("no year", message(None))] {
                let tag = language.tag();
                assert!(!text.trim().is_empty(), "{tag} {what}: empty");
                let length = text.chars().count();
                assert!(length <= LIMIT, "{tag} {what}: {length} characters, over {LIMIT}: {text}");
                let mut lines = 1;
                let mut width = 0;
                for word in text.split_whitespace() {
                    let w = word.chars().count();
                    if width != 0 && width + 1 + w > LINE {
                        lines += 1;
                        width = w;
                    } else {
                        width += if width == 0 { w } else { 1 + w };
                    }
                }
                assert!(lines <= 2, "{tag} {what}: wraps to {lines} lines at {LINE}: {text}");
            }
            assert!(message(Some(2020)).contains("2020"), "{}: the year is shown", language.tag());
        }
    }
}
