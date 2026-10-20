//! The Collections count the library header shows beside "Collections" (`LibraryLike::collections_count`).
//!
//! One `type=18` `Size=0` probe per library section (`plx_plex` `Client::collections_count`), run
//! on a small worker so the frame thread never waits on the server, and kept for the session:
//! a refusal (HTTP 401/403) is final, a count is re-read after [`REFRESH_MS`], and a probe that learned
//! nothing (transport failure, 404) is retried after [`RETRY_MS`] while the header keeps the
//! Collections control off the row (it is drawn only with a known count). The header reads this
//! every frame for the library it names, so a lookup is a short lock over a handful of entries and
//! starts at most one worker per section at a time.
//!
//! **Every section is warmed when the directory lands** ([`warm`], called once a frame by the
//! bridge): each distinct `(server, section key)` is asked through the same lookup and the same
//! cache, one `Size=0` read each, so a library switch usually finds its answer already there and
//! the control is on the row in the first frame of the switch. A section the warm pass could not
//! ask (no client yet) is asked when the header first names it.
//!
//! A controlled run (record / replay) never probes: the worker's landing time is not on the tape,
//! so the header would differ between a recording and its replay.
use plx_plex::plex::collections::CollectionsCount;
use plx_screens::registry::Forbidden;
use std::sync::Mutex;

/// How long a known count is trusted before it is read again (collections made on another device).
const REFRESH_MS: u32 = 300_000;
/// How long to wait before asking again after an answer that said nothing.
const RETRY_MS: u32 = 20_000;

/// `(server, client instance, section key)`: a sign-out or a new client starts from nothing.
type Key = (u16, u32, i64);

#[derive(Clone, Copy)]
struct Entry {
    /// What the last probe that said something said.
    answer: Option<CollectionsCount>,
    /// The frame clock (`Tick::ms`) when the probe that produced `answer` (or the last empty one)
    /// was first seen to have returned. Stamped by the next lookup, never by the worker: the app
    /// reads no wall clock.
    at: Option<u32>,
    /// A worker returned and the next lookup owes `at` its stamp.
    settled: bool,
    probing: bool,
}

static CACHE: Mutex<Vec<(Key, Entry)>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<(Key, Entry)>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Should `entry` be probed now? Pure, so the schedule is testable without a clock or a server.
fn wants_probe(entry: &Entry, now: u32) -> bool {
    if entry.probing || entry.settled { return false; }
    let age = entry.at.map(|at| now.wrapping_sub(at));
    match (entry.answer, age) {
        (_, None) => true,
        (Some(CollectionsCount::Forbidden), _) => false,
        (Some(CollectionsCount::Count(_)), Some(age)) => age >= REFRESH_MS,
        (Some(CollectionsCount::Unknown) | None, Some(age)) => age >= RETRY_MS,
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

/// Record what a finished probe learned, then ask for a frame: a resting page runs no ticks, so
/// without the wake the header would keep showing "Collections" with no number (or a control the
/// answer says to hide) until the next input.
fn land(key: Key, learned: CollectionsCount) {
    {
        let mut cache = lock();
        if let Some((_, entry)) = cache.iter_mut().find(|(seen, _)| *seen == key) {
            // An answer that said nothing never replaces one that did.
            if learned != CollectionsCount::Unknown || entry.answer.is_none() { entry.answer = Some(learned); }
            entry.settled = true;
            entry.probing = false;
        }
    }
    plx_machine::idle::invalidate();
}

/// The count for the directory's section `section` (an index into its table, which names the
/// server as well as the key), starting its probe the first time it is asked for. `now` is the
/// frame clock in milliseconds. `None` while it is not known.
pub(crate) fn count(
    directory: plx_data::stores::browse::DirectoryView<'_>,
    section: usize,
    now: u32,
) -> Option<Result<u32, Forbidden>> {
    // A controlled run (replay, record) and a unit test never reach the network: the probe would
    // be a request the run's own script did not make.
    if cfg!(test) || plx_data::stores::tape::active() { return None; }
    let section = directory.sections().get(section)?;
    ask(section.sid?, section.key, now)
}

/// What one landing of the directory is: its epoch and the table's generation and size. The same
/// stamp means the same sections, so the warm pass has nothing new to ask.
type Landing = (u32, u32, usize);

/// The sections a landed directory asks about: each distinct `(server, section key)` once, in
/// table order. A section with no server (a fixture's) has nothing to ask.
fn targets(directory: plx_data::stores::browse::DirectoryView<'_>) -> Vec<(plx_plex::plex::ServerId, i64)> {
    let mut seen: Vec<(plx_plex::plex::ServerId, i64)> = Vec::new();
    for section in directory.sections() {
        if let Some(sid) = section.sid {
            if !seen.contains(&(sid, section.key)) { seen.push((sid, section.key)); }
        }
    }
    seen
}

/// The decision behind [`warm`], with the asking passed in so it is testable without a server:
/// when `directory` is a landing `warmed` has not seen, ask `each` for every target once and
/// remember the landing. The same directory again asks nothing.
fn warm_with(
    directory: plx_data::stores::browse::DirectoryView<'_>,
    warmed: &mut Option<Landing>,
    mut each: impl FnMut(plx_plex::plex::ServerId, i64),
) {
    let Some(epoch) = directory.epoch() else { return };
    let landing = (epoch, directory.sections_gen(), directory.sections().len());
    if *warmed == Some(landing) { return; }
    *warmed = Some(landing);
    for (sid, key) in targets(directory) { each(sid, key); }
}

/// Warm the probe for every section of `directory`, once per landing (see the module doc). Called
/// every frame by the bridge; a frame that is not a new landing costs a comparison.
pub(crate) fn warm(directory: plx_data::stores::browse::DirectoryView<'_>, now: u32) {
    if cfg!(test) || plx_data::stores::tape::active() { return; }
    static WARMED: Mutex<Option<Landing>> = Mutex::new(None);
    let mut warmed = WARMED.lock().unwrap_or_else(|e| e.into_inner());
    warm_with(directory, &mut warmed, |sid, key| { let _ = ask(sid, key, now); });
}

/// The answer for `(sid, key)` from the cache, starting its probe when the schedule says so.
fn ask(sid: plx_plex::plex::ServerId, section: i64, now: u32) -> Option<Result<u32, Forbidden>> {
    let client = plx_plex::plex::client_for(sid)?;
    let key = (sid.raw(), client.instance_gen(), section);
    let (answer, launch) = {
        let mut cache = lock();
        let index = match cache.iter().position(|(seen, _)| *seen == key) {
            Some(index) => index,
            None => {
                cache.push((key, Entry { answer: None, at: None, settled: false, probing: false }));
                cache.len() - 1
            }
        };
        let entry = &mut cache[index].1;
        if entry.settled { entry.settled = false; entry.at = Some(now); }
        let launch = wants_probe(entry, now);
        if launch { entry.probing = true; }
        (entry.answer, launch)
    };
    if launch {
        let spawned = plx_base::task::spawn_small("collections-count", move || {
            let learned = std::panic::catch_unwind(|| client.collections_count(key.2))
                .unwrap_or(CollectionsCount::Unknown);
            land(key, learned);
        });
        if !spawned {
            if let Some((_, entry)) = lock().iter_mut().find(|(seen, _)| *seen == key) {
                entry.probing = false;
                entry.settled = true;
            }
        }
    }
    shown(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(answer: Option<CollectionsCount>, age_s: u32, probing: bool, now: u32) -> Entry {
        Entry { answer, at: Some(now - age_s * 1000), settled: false, probing }
    }

    #[test]
    fn a_section_is_probed_once_until_its_answer_goes_stale() {
        let now = 100_000_000;
        let fresh = Entry { answer: None, at: None, settled: false, probing: false };
        assert!(wants_probe(&fresh, now), "never asked: ask");
        assert!(!wants_probe(&Entry { probing: true, ..fresh }, now), "one worker per section");
        assert!(!wants_probe(&Entry { settled: true, ..fresh }, now), "a returned worker is stamped first");
        let count = Some(CollectionsCount::Count(3));
        assert!(!wants_probe(&entry(count, 10, false, now), now), "a count is kept");
        assert!(wants_probe(&entry(count, 301, false, now), now), "…and re-read when old");
        assert!(!wants_probe(&entry(Some(CollectionsCount::Forbidden), 9_000, false, now), now), "a refusal is final");
        assert!(!wants_probe(&entry(None, 5, false, now), now), "an empty answer waits before a retry");
        assert!(wants_probe(&entry(None, 21, false, now), now));
    }

    fn table(sections: &[(Option<u16>, i64)]) -> plx_data::stores::browse::DirectorySnapshot {
        use plx_data::stores::browse::{DirectorySnapshot, SectionView, SrcRow};
        DirectorySnapshot::fixture(3, 0, sections.iter().enumerate().map(|(i, &(sid, key))| SectionView {
            sid: sid.map(plx_plex::plex::ServerId::from_raw),
            key,
            kind: plx_data::browse::SecKind::Movie,
            row: SrcRow { section: i, title: format!("Library {i}"), pinned: true, ..Default::default() },
        }).collect())
    }

    /// **A landed directory asks about EVERY section**, not only the current one: one read per
    /// distinct `(server, key)`, in table order (the same key on two servers is two sections), none
    /// for a section with no server, and nothing again for the same landing. A directory that is
    /// replaced (a new epoch) is a new landing and is asked again, the cache keeping the answers
    /// the schedule still trusts.
    #[test]
    fn a_landed_directory_warms_every_section_once() {
        let snapshot = table(&[(Some(1), 5), (Some(1), 6), (Some(2), 5), (None, 9), (Some(1), 5)]);
        let mut warmed = None;
        let mut asked = Vec::new();
        warm_with(snapshot.view(), &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked, [(1, 5), (1, 6), (2, 5)], "every distinct section of every server, once");
        warm_with(snapshot.view(), &mut warmed, |sid, key| asked.push((sid.raw(), key)));
        assert_eq!(asked.len(), 3, "the same landing asks nothing again");
        let later = table(&[(Some(1), 5), (Some(1), 6), (Some(2), 5), (Some(2), 7)]);
        let mut again = Vec::new();
        warm_with(later.view(), &mut warmed, |sid, key| again.push((sid.raw(), key)));
        assert_eq!(again, [(1, 5), (1, 6), (2, 5), (2, 7)], "a table of another size is a new landing");
        let mut none = Vec::new();
        warm_with(plx_data::stores::browse::DirectoryView::empty_for_test(), &mut warmed, |sid, key| none.push((sid.raw(), key)));
        assert!(none.is_empty(), "no directory yet, nothing to ask");
    }

    #[test]
    fn a_landed_answer_wakes_a_frame() {
        let key: Key = (u16::MAX, u32::MAX, -7);
        lock().push((key, Entry { answer: None, at: None, settled: false, probing: true }));
        let _ = plx_machine::idle::take_local_damage();
        land(key, CollectionsCount::Count(4));
        assert!(plx_machine::idle::take_local_damage() > 0, "the landing must request a frame");
        let mut cache = lock();
        let at = cache.iter().position(|(k, _)| *k == key).unwrap();
        let entry = cache.remove(at).1;
        assert_eq!(entry.answer, Some(CollectionsCount::Count(4)));
        assert!(entry.settled && !entry.probing);
    }

    #[test]
    fn the_header_sees_a_count_a_refusal_or_nothing() {
        assert_eq!(shown(None), None);
        assert_eq!(shown(Some(CollectionsCount::Unknown)), None);
        assert_eq!(shown(Some(CollectionsCount::Count(0))), Some(Ok(0)));
        assert!(matches!(shown(Some(CollectionsCount::Forbidden)), Some(Err(Forbidden))));
    }
}
