//! The Collections count the library header shows beside "Collections" (`LibraryLike::collections_count`).
//!
//! One `type=18` `Size=0` probe per library section (`plx_plex` `Client::collections_count`), run
//! on a small worker so the frame thread never waits on the server, and kept for the session:
//! a refusal (HTTP 401/403) is final, a count is re-read after [`REFRESH`], and a probe that learned
//! nothing (transport failure, 404) is retried after [`RETRY`] while the header keeps showing
//! "Collections" without a number. The header reads this every frame, so a lookup is a short lock
//! over a handful of entries and starts at most one worker per section at a time.
//!
//! A controlled run (record / replay) never probes: the worker's landing time is not on the tape,
//! so the header would differ between a recording and its replay.
use plx_plex::plex::collections::CollectionsCount;
use plx_screens::registry::Forbidden;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a known count is trusted before it is read again (collections made on another device).
const REFRESH: Duration = Duration::from_secs(300);
/// How long to wait before asking again after an answer that said nothing.
const RETRY: Duration = Duration::from_secs(20);

/// `(server, client instance, section key)`: a sign-out or a new client starts from nothing.
type Key = (u16, u32, i64);

#[derive(Clone, Copy)]
struct Entry {
    /// What the last probe that said something said.
    answer: Option<CollectionsCount>,
    /// When the probe that produced `answer` (or the last empty one) returned.
    at: Option<Instant>,
    probing: bool,
}

static CACHE: Mutex<Vec<(Key, Entry)>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<(Key, Entry)>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Should `entry` be probed now? Pure, so the schedule is testable without a clock or a server.
fn wants_probe(entry: &Entry, now: Instant) -> bool {
    if entry.probing { return false; }
    let age = entry.at.map(|at| now.saturating_duration_since(at));
    match (entry.answer, age) {
        (_, None) => true,
        (Some(CollectionsCount::Forbidden), _) => false,
        (Some(CollectionsCount::Count(_)), Some(age)) => age >= REFRESH,
        (Some(CollectionsCount::Unknown) | None, Some(age)) => age >= RETRY,
    }
}

/// What the header may show for `answer`: `None` = not known, `Some(Err)` = refused.
fn shown(answer: Option<CollectionsCount>) -> Option<Result<u32, Forbidden>> {
    match answer? {
        CollectionsCount::Count(n) => Some(Ok(n)),
        CollectionsCount::Forbidden => Some(Err(Forbidden)),
        CollectionsCount::Unknown => None,
    }
}

/// The count for library section `section` of the directory's current section, starting its
/// probe the first time it is asked for. `None` for any other section: only the page's own
/// section is ever asked about, and a section key alone does not name a server.
pub(crate) fn count(
    directory: plx_data::stores::browse::DirectoryView<'_>,
    section: u32,
) -> Option<Result<u32, Forbidden>> {
    // A controlled run (replay, record) and a unit test never reach the network: the probe would
    // be a request the run's own script did not make.
    if cfg!(test) || plx_data::stores::tape::active() { return None; }
    let current = directory.sections().get(directory.current()?)?;
    if current.key != i64::from(section) { return None; }
    let sid = current.sid?;
    let client = plx_plex::plex::client_for(sid)?;
    let key = (sid.raw(), client.instance_gen(), current.key);
    let now = Instant::now();
    let (answer, launch) = {
        let mut cache = lock();
        let index = match cache.iter().position(|(seen, _)| *seen == key) {
            Some(index) => index,
            None => {
                cache.push((key, Entry { answer: None, at: None, probing: false }));
                cache.len() - 1
            }
        };
        let entry = &mut cache[index].1;
        let launch = wants_probe(entry, now);
        if launch { entry.probing = true; }
        (entry.answer, launch)
    };
    if launch {
        let spawned = plx_base::task::spawn_small("collections-count", move || {
            let learned = std::panic::catch_unwind(|| client.collections_count(key.2))
                .unwrap_or(CollectionsCount::Unknown);
            let mut cache = lock();
            if let Some((_, entry)) = cache.iter_mut().find(|(seen, _)| *seen == key) {
                // An answer that said nothing never replaces one that did.
                if learned != CollectionsCount::Unknown || entry.answer.is_none() { entry.answer = Some(learned); }
                entry.at = Some(Instant::now());
                entry.probing = false;
            }
        });
        if !spawned {
            if let Some((_, entry)) = lock().iter_mut().find(|(seen, _)| *seen == key) {
                entry.probing = false;
                entry.at = Some(now);
            }
        }
    }
    shown(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(answer: Option<CollectionsCount>, age_s: u64, probing: bool, now: Instant) -> Entry {
        Entry { answer, at: now.checked_sub(Duration::from_secs(age_s)), probing }
    }

    #[test]
    fn a_section_is_probed_once_until_its_answer_goes_stale() {
        let now = Instant::now() + Duration::from_secs(1_000);
        let fresh = Entry { answer: None, at: None, probing: false };
        assert!(wants_probe(&fresh, now), "never asked: ask");
        assert!(!wants_probe(&Entry { probing: true, ..fresh }, now), "one worker per section");
        let count = Some(CollectionsCount::Count(3));
        assert!(!wants_probe(&entry(count, 10, false, now), now), "a count is kept");
        assert!(wants_probe(&entry(count, 301, false, now), now), "…and re-read when old");
        assert!(!wants_probe(&entry(Some(CollectionsCount::Forbidden), 9_000, false, now), now), "a refusal is final");
        assert!(!wants_probe(&entry(None, 5, false, now), now), "an empty answer waits before a retry");
        assert!(wants_probe(&entry(None, 21, false, now), now));
    }

    #[test]
    fn the_header_sees_a_count_a_refusal_or_nothing() {
        assert_eq!(shown(None), None);
        assert_eq!(shown(Some(CollectionsCount::Unknown)), None);
        assert_eq!(shown(Some(CollectionsCount::Count(0))), Some(Ok(0)));
        assert!(matches!(shown(Some(CollectionsCount::Forbidden)), Some(Err(Forbidden))));
    }
}
