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
        if let Some(message) = self.poll_with(keypin::revision, keypin::engaged) {
            send(message);
        }
    }

    /// The decision, with the two process-wide reads injected so a test can count them. Evaluated
    /// as late as it can be: `engaged` takes the keypin lock and scans its table, which the frame
    /// thread must not do every frame, so it is only read for a revision not yet seen.
    fn poll_with(
        &mut self,
        revision: impl FnOnce() -> u64,
        engaged: impl FnOnce() -> Option<Option<i64>>,
    ) -> Option<String> {
        if self.told {
            return None;
        }
        let revision = revision();
        if self.seen == Some(revision) {
            return None;
        }
        self.seen = Some(revision);
        let year = engaged()?;
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
        crate::eventlog::log(&format!("clock notice: toast {outcome:?}"));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One poll over the given reads.
    fn step(notice: &mut ClockNotice, revision: u64, engaged: Option<Option<i64>>) -> Option<String> {
        notice.poll_with(|| revision, || engaged)
    }

    #[test]
    fn it_fires_exactly_once_however_often_the_facts_move() {
        let mut notice = ClockNotice::new();
        assert_eq!(step(&mut notice, 1, None), None, "a revision with nothing engaged tells nobody");
        assert_eq!(step(&mut notice, 1, None), None);
        let first = step(&mut notice, 2, Some(Some(2020)));
        assert!(first.is_some_and(|m| m.contains("2020")), "the first engagement is told");
        // Re-engagements after a latch lapse, a blocked fact appearing or clearing, and the same
        // revision read again: none of them tell it twice.
        assert_eq!(step(&mut notice, 2, Some(Some(2020))), None);
        assert_eq!(step(&mut notice, 3, Some(Some(2020))), None);
        assert_eq!(step(&mut notice, 9, Some(Some(2031))), None);
        assert_eq!(step(&mut notice, 10, Some(None)), None);
    }

    #[test]
    fn a_fact_published_before_the_first_poll_is_still_seen() {
        let mut notice = ClockNotice::new();
        assert!(step(&mut notice, 0, Some(Some(2020))).is_some(), "revision 0 is a revision like another");
    }

    /// The frame loop polls forever: `engaged` locks the keypin mutex, so once told it is never read
    /// and while the revision stands still it is not read either; a moved revision reads it once.
    #[test]
    fn the_locking_read_happens_only_for_a_revision_not_yet_seen_and_never_once_told() {
        let engaged_reads = std::cell::Cell::new(0);
        let poll = |notice: &mut ClockNotice, revision: u64, engaged: Option<Option<i64>>| {
            notice.poll_with(|| revision, || {
                engaged_reads.set(engaged_reads.get() + 1);
                engaged
            })
        };
        let mut notice = ClockNotice::new();
        assert_eq!(poll(&mut notice, 4, None), None);
        assert_eq!(engaged_reads.get(), 1, "a first sight of a revision reads the facts");
        for _ in 0..1000 {
            assert_eq!(poll(&mut notice, 4, None), None);
        }
        assert_eq!(engaged_reads.get(), 1, "an unchanged revision costs no lock");
        assert!(poll(&mut notice, 5, Some(Some(2020))).is_some());
        assert_eq!(engaged_reads.get(), 2);
        for revision in 6..1000 {
            assert_eq!(poll(&mut notice, revision, Some(Some(2020))), None);
        }
        assert_eq!(engaged_reads.get(), 2, "once told, nothing is read at all");
    }

    #[test]
    fn it_does_not_burn_its_one_chance_on_an_unengaged_revision() {
        let mut notice = ClockNotice::new();
        for revision in 1..5 {
            assert_eq!(step(&mut notice, revision, None), None);
        }
        assert!(step(&mut notice, 5, Some(None)).is_some());
    }

    #[test]
    fn the_message_names_the_year_only_when_it_is_known() {
        let _en = crate::i18n::language_on_this_thread_for_test(crate::i18n::Preference::En);
        assert_eq!(message(Some(2020)), "TV clock looks wrong (2020). Connected by the remembered key.");
        assert_eq!(message(None), "TV clock looks wrong. Connected by the remembered key.");
        // Not locale-grouped: a year is not a quantity.
        assert!(!message(Some(2020)).contains("2,020"));
    }

    /// The system toast does not truncate at 80 or 120 characters. **Measured on the television**
    /// (`noaction`, no arrow): a 140-character Latin message showed in full on three lines, about
    /// 45 Latin characters per line ("TV clock looks wrong (2020). Connected to your" held 45).
    /// 120 is the cap we hold ourselves to; a line is budgeted at 40, which leaves a margin for
    /// the wider Cyrillic glyphs, and the text at three such lines.
    #[test]
    fn every_shipped_language_fits_the_toast() {
        const LIMIT: usize = 120;
        const LINE: usize = 40;
        const LINES: usize = 3;
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
                assert!(lines <= LINES, "{tag} {what}: wraps to {lines} lines at {LINE}: {text}");
            }
            assert!(message(Some(2020)).contains("2020"), "{}: the year is shown", language.tag());
        }
    }
}
