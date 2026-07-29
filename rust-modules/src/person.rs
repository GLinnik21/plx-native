//! person — the actor/person page's data layer (the store `ui/person.rs` draws).
//!
//! Sibling of `metadata.rs` (the detail page's item store) and `browse.rs` (the Library's paged
//! catalog), and built on the same three pieces: [`crate::task::spawn_small`] + a `Mutex` mailbox
//! + a generation atomic, applied on the MAIN thread by [`pump`] once a frame while the page is
//! up. The worker never touches a static; [`current`] hands out a `&'static Person` that the draw
//! reads all frame, so the main thread stays its only writer (the same soundness rule
//! `metadata.rs`'s `CURRENT` carries).
//!
//! **One request per page**: `GET /library/people/{personId}/media` returns everything the person
//! appears in across EVERY library section at once — no per-section `?actor=<id>` sweep. The rows
//! are split into the Movies / Shows shelves by **each row's own `type`**; the container's
//! `viewGroup` is a trap (it read `"movie"` on a response whose only row was a `show`).
//!
//! **The header is handed in, not fetched.** `Role[]` already carries the name and the headshot
//! (`plex::Tag`), and `GET /library/people/{id}` adds nothing to them — verified live on
//! 2026-07-29, the record PMS returns is `{id, filter, tag, tagType, tagKey, thumb}` with **no
//! summary and no birth date**. So the page opens on the header it was given and only waits on
//! the shelves. [`Person::bio`] exists and is drawn when non-empty, but nothing populates it
//! today: the biography lives on plex.tv, behind the TLS+DNS `net.rs` does for the account layer,
//! and that call is deliberately out of scope here — the layout is designed to read as complete
//! without it (name + portrait), not as a field that failed to load.
use crate::pms::{parse_item, PmsMovie};
use std::panic::catch_unwind;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// Per-shelf item cap. A `CardRow` owns exactly [`crate::ui::card_row::MAX_ROW_ITEMS`] focus-scale
/// springs and `scale(i)` clamps past the end, so an item beyond the cap would draw with the last
/// cell's pop and — worse — never pop at all when focused (`update`'s loop can't reach its index).
/// It is also the perf ceiling the A53 budget wants: a shelf is a horizontal strip, not a grid.
const SHELF_MAX: usize = crate::ui::card_row::MAX_ROW_ITEMS;

/// One person's page: the header handed in by the cast row, plus the two shelves fetched from
/// `/library/people/{key}/media`.
pub(crate) struct Person {
    /// the `personId` this record was opened for (numeric tag id, or the guid)
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) thumb: String,
    /// Biography. Always empty today — see the module docs; drawn only when non-empty.
    pub(crate) bio: String,
    pub(crate) movies: Vec<PmsMovie>,
    pub(crate) shows: Vec<PmsMovie>,
    /// the `/media` fetch has landed successfully at least once. The `browse.rs` `total < 0`
    /// sentinel in bool form: it is what separates "still loading" from "this person genuinely
    /// has nothing here", and what stops [`maybe_spawn`] re-fetching forever.
    pub(crate) landed: bool,
}

impl Person {
    /// The shelf for `kind` (0 = Movies, 1 = Shows) — the ONE place the shelf index maps to a
    /// list, so the focus model, the draw and the hit-test cannot disagree.
    pub(crate) fn shelf(&self, kind: usize) -> &[PmsMovie] {
        match kind {
            0 => &self.movies,
            _ => &self.shows,
        }
    }
}

static mut CURRENT: Option<Person> = None;

/// The open person, or None. Main-thread only; the reference is valid until the next
/// [`pump`]/[`open`]/[`close`] (same lifetime rule as `metadata::current`).
pub(crate) fn current() -> Option<&'static Person> {
    unsafe { (*addr_of!(CURRENT)).as_ref() }
}

// ---- fetch plumbing (generation + single-flight + mailbox + retry backoff) -------------------

/// Bumped by every [`open`]/[`close`]/[`reset`]: a landing whose generation no longer matches is
/// discarded by [`pump`], so a slow fetch for the actor you just left can never repopulate the
/// one you are looking at now.
static GEN: AtomicU32 = AtomicU32::new(0);
/// The claim that a `/media` fetch is out. Cleared ONLY by a mailbox take, so anything that drops
/// the mailbox ([`supersede`]) must clear it too — otherwise the fetch stays latched and the page
/// spins forever. Same latch `browse.rs` documents on its `IN_FLIGHT` array.
///
/// It bounds spawns per *pump*, which is what matters; it is NOT a hard one-worker-at-a-time
/// interlock, and claiming otherwise would be wrong. Two ways a second worker can briefly exist:
/// [`supersede`] releases the claim while the old worker is still running, and a take releases it
/// before the generation check (so a stale landing can free a NEWER fetch's claim, costing one
/// duplicate request). Neither can wedge or corrupt — [`land`] is monotone on the generation and
/// [`pump`] discards anything stale — and `browse.rs` has the identical shape.
static FETCHING: AtomicBool = AtomicBool::new(false);
/// Every single-flight flag, in one place — add a new one HERE and [`supersede`] picks it up.
const IN_FLIGHT: [&AtomicBool; 1] = [&FETCHING];
/// Frames left before another fetch may spawn after a FAILED one (main-thread; [`pump`]
/// decrements). Stops a fast-failing network from spawning a worker every frame.
static mut RETRY_CD: u32 = 0;
/// ~2s at 60fps — the same backoff `browse.rs` uses for a failed page.
const RETRY_FRAMES: u32 = 120;

struct MediaResult {
    gen: u32,
    /// `None` = the fetch FAILED (transport, parse, or a panicking worker). It must stay
    /// distinguishable from a person who genuinely has nothing: installing empty shelves on a
    /// failure is the "one wifi hiccup blanked a populated grid" bug `browse.rs` carries its
    /// `total < 0` sentinel for.
    items: Option<(Vec<PmsMovie>, Vec<PmsMovie>)>,
}
static SLOT: Mutex<Option<MediaResult>> = Mutex::new(None);

/// Post a finished fetch to the mailbox. MONOTONE: an older fetch landing late must never clobber
/// a newer result the pump has not consumed yet. Named (not inlined in the worker closure) for the
/// same reason as `metadata::land_detail` — the guard is the one piece of this machinery a test
/// cannot reach through [`open`], because reaching it needs two overlapping real fetches.
fn land(gen: u32, items: Option<(Vec<PmsMovie>, Vec<PmsMovie>)>) {
    let mut slot = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(MediaResult { gen, items });
    }
}

/// Invalidate everything in flight: bump the generation (a late landing is discarded), drop the
/// mailbox, release the single-flight flags with it, and clear the retry backoff. The ONE place
/// those four move together.
fn supersede() {
    GEN.fetch_add(1, Ordering::SeqCst);
    *SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    for f in IN_FLIGHT {
        f.store(false, Ordering::SeqCst);
    }
    unsafe { RETRY_CD = 0 };
}

// ---- public surface --------------------------------------------------------------------------

/// Open the page for `key` (a `personId`) with the header the caller already has — the cast row's
/// name + headshot. MAIN THREAD, NON-BLOCKING: nothing is fetched here, [`pump`] spawns the one
/// `/media` request on the next frame, so the page mounts on its header immediately.
pub(crate) fn open(key: &str, name: &str, thumb: &str) {
    supersede();
    unsafe {
        *addr_of_mut!(CURRENT) = Some(Person {
            key: key.to_string(),
            name: name.to_string(),
            thumb: thumb.to_string(),
            bio: String::new(),
            movies: Vec::new(),
            shows: Vec::new(),
            landed: false,
        });
    }
}

/// Drop the open person and supersede any fetch for it — on leaving the page. Without the
/// supersede, a landing arriving after the page closed would repopulate `CURRENT` behind whatever
/// screen is now mounted (the bug `metadata::clear` carries the same guard for).
pub(crate) fn close() {
    supersede();
    unsafe { *addr_of_mut!(CURRENT) = None };
}

/// Wipe the store on a profile/account switch — the browse/pms twin of `install_pms`'s reset. A
/// new user must never inherit the previous one's page, and the flags must move with the mailbox.
pub(crate) fn reset() {
    close();
}

/// True while the open person's shelves have not landed yet — the page's spinner state. A failed
/// fetch stays "loading" on purpose: [`maybe_spawn`] retries after the backoff, so the spinner is
/// telling the truth about what the page is doing.
pub(crate) fn loading() -> bool {
    current().map(|p| !p.landed).unwrap_or(false)
}

/// MAIN THREAD, once a frame while the page is up: apply a landed fetch and schedule the next.
/// Returns true when shelves just changed (the screen re-clamps its focus on it).
pub(crate) fn pump() -> bool {
    let mut changed = false;
    unsafe {
        if RETRY_CD > 0 {
            RETRY_CD -= 1;
        }
    }
    let taken = SLOT.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(r) = taken {
        // the take ALWAYS releases the single-flight, whatever the landing turns out to be —
        // dropping a stale one without this is how the flag latches forever
        FETCHING.store(false, Ordering::SeqCst);
        if r.gen != GEN.load(Ordering::SeqCst) {
            // Superseded: a different person is open now, so this landing is news about NEITHER
            // of them. Note the failure arm below is inside this gate on purpose — a stale
            // failure that armed the backoff would delay the new person's first fetch by ~2s
            // for a network error that was never about them.
        } else {
            match r.items {
                // FAILED: leave the store exactly as it was and back off before retrying
                None => unsafe { RETRY_CD = RETRY_FRAMES },
                Some((movies, shows)) => {
                    if let Some(p) = unsafe { (*addr_of_mut!(CURRENT)).as_mut() } {
                        crate::log(&format!(
                            "person: key={} '{}' movies={} shows={}",
                            p.key, p.name, movies.len(), shows.len()
                        ));
                        p.movies = movies;
                        p.shows = shows;
                        p.landed = true;
                        changed = true;
                    }
                }
            }
        }
    }
    maybe_spawn();
    changed
}

/// One fetch at a time, and only while there is an unfetched person open. Re-entered every frame
/// by [`pump`], which is what makes the failure path self-healing: a refused `spawn_small` (the
/// device's thread ceiling) or a transient network error simply retries after the backoff instead
/// of latching the page on a spinner forever.
fn maybe_spawn() {
    if FETCHING.load(Ordering::SeqCst) || unsafe { RETRY_CD > 0 } {
        return;
    }
    let Some(p) = current() else { return };
    if p.landed || p.key.is_empty() {
        return;
    }
    let key = p.key.clone();
    let gen = GEN.load(Ordering::SeqCst);
    FETCHING.store(true, Ordering::SeqCst);
    let spawned = crate::task::spawn_small("person", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an empty filmography
        let items = catch_unwind(|| {
            let mc = crate::plex::client_opt()?.person_media(&key)?;
            Some(split_by_type(&mc))
        })
        .unwrap_or(None);
        land(gen, items);
    });
    if !spawned {
        // nothing will ever fill the mailbox, and the flag is cleared only by a take — release it
        // here or this person never fetches again. `maybe_spawn` runs every frame, so this retries
        // by itself.
        FETCHING.store(false, Ordering::SeqCst);
    }
}

/// Split a `/library/people/{id}/media` container into the Movies and Shows shelves **by each
/// row's own `type`**.
///
/// The container's `viewGroup` cannot be used for this and is the whole reason this function is
/// named and tested: verified live 2026-07-29, person 6059's response carries `viewGroup:"movie"`
/// over five movies AND one show. Anything that is neither a `movie` nor a `show` is dropped —
/// the page has exactly two shelves and they are labelled, so silently filing an episode under
/// "Shows" would put a landscape still in a portrait poster slot.
pub(crate) fn split_by_type(mc: &crate::plex::MediaContainer) -> (Vec<PmsMovie>, Vec<PmsMovie>) {
    let (mut movies, mut shows) = (Vec::new(), Vec::new());
    for it in &mc.metadata {
        let dst = match it.kind.as_str() {
            "movie" => &mut movies,
            "show" => &mut shows,
            _ => continue,
        };
        if dst.len() < SHELF_MAX {
            dst.push(parse_item(it));
        }
    }
    (movies, shows)
}

/// TEST ONLY: publish shelves onto the open person exactly as a successful landing would, so the
/// screen's focus/flow tests need neither a server nor the mailbox.
#[cfg(test)]
pub(crate) fn install_for_test(movies: Vec<PmsMovie>, shows: Vec<PmsMovie>) {
    if let Some(p) = unsafe { (*addr_of_mut!(CURRENT)).as_mut() } {
        p.movies = movies;
        p.shows = shows;
        p.landed = true;
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::{MediaContainer, Metadata};

    fn row(kind: &str, rk: &str, title: &str) -> Metadata {
        Metadata {
            kind: kind.to_string(),
            rating_key: rk.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    /// The shelves are filled from each ROW's `type`, never from the container's `viewGroup` —
    /// verified live against person 6059, whose response is `viewGroup:"movie"` over five movies
    /// and one show. Reading the container would have filed that show under Movies.
    #[test]
    fn media_rows_are_shelved_by_their_own_type_not_the_containers_view_group() {
        let mut mc = MediaContainer::default();
        mc.metadata = vec![
            row("movie", "1", "Frozen"),
            row("show", "1975", "Cracking Contraptions"),
            row("movie", "2", "Frozen II"),
        ];
        let (movies, shows) = split_by_type(&mc);
        assert_eq!(movies.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(shows.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1975"]);
    }

    /// Neither shelf may exceed a `CardRow`'s spring count: past it `scale(i)` clamps to the last
    /// cell, so an over-cap tile would wear its neighbour's pop and never animate its own.
    /// Unknown types (season/episode/clip) are dropped rather than filed under a wrong label.
    #[test]
    fn shelves_are_capped_at_the_card_rows_spring_count_and_drop_unknown_types() {
        let mut mc = MediaContainer::default();
        for i in 0..(SHELF_MAX + 5) {
            mc.metadata.push(row("movie", &i.to_string(), "m"));
        }
        mc.metadata.push(row("episode", "e1", "an episode"));
        mc.metadata.push(row("season", "s1", "a season"));
        let (movies, shows) = split_by_type(&mc);
        assert_eq!(movies.len(), SHELF_MAX);
        assert!(shows.is_empty(), "an episode/season is not a Show shelf tile");
    }

    /// A late landing for the actor you already left must not repopulate the one you are looking
    /// at. `open` bumps the generation; `pump` drops anything older — and it must still clear the
    /// single-flight flag while doing so, or the NEW person can never fetch.
    #[test]
    fn a_landing_from_the_previous_person_is_discarded_but_still_releases_the_fetch() {
        let _serial = crate::testlock::serial();
        open("161", "Idina Menzel", "");
        let stale = GEN.load(Ordering::SeqCst);
        open("465", "Cynthia Erivo", ""); // supersedes: the fetch above is now obsolete

        FETCHING.store(true, Ordering::SeqCst);
        // hold the re-kick off for this frame, so the take's RELEASE of the single-flight is
        // observable rather than immediately masked by the next fetch claiming it (and so the
        // host suite spawns no worker for a client that isn't installed)
        unsafe { RETRY_CD = 2 };
        land(stale, Some((vec![PmsMovie::default()], Vec::new())));
        assert!(!pump(), "a superseded landing must not publish");

        let p = current().expect("the new person stays open");
        assert_eq!(p.key, "465");
        assert!(p.movies.is_empty(), "the previous actor's filmography leaked in");
        assert!(!p.landed, "a discarded landing must not settle the spinner");
        assert!(!FETCHING.load(Ordering::SeqCst), "the take must release the single-flight even for a landing it drops");
        close();
    }

    /// A FAILED fetch (None) must leave a populated page alone and schedule a retry — the
    /// "one wifi hiccup blanked a populated grid" regression, in this store's shape.
    #[test]
    fn a_failed_fetch_keeps_the_shelves_and_backs_off_instead_of_publishing_empty() {
        let _serial = crate::testlock::serial();
        open("161", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);
        // seed a populated, landed page the honest way (through the pump)
        land(gen, Some((vec![PmsMovie::default()], Vec::new())));
        assert!(pump());
        assert_eq!(current().unwrap().movies.len(), 1);

        land(gen, None); // the retry fails
        assert!(!pump(), "a failure publishes nothing");
        assert_eq!(current().unwrap().movies.len(), 1, "the failure wiped a populated shelf");
        assert!(unsafe { RETRY_CD } > 0, "a failure must back off before retrying");
        close();
    }

    /// `close`/`reset` drop the mailbox, and the single-flight flag is cleared ONLY by a
    /// successful take — so they must clear it (and the backoff) themselves or the next person
    /// opened never fetches. The `browse.rs` latch, one store over.
    #[test]
    fn close_clears_the_single_flight_flag_and_the_retry_backoff() {
        let _serial = crate::testlock::serial();
        open("161", "Idina Menzel", "");
        FETCHING.store(true, Ordering::SeqCst);
        unsafe { RETRY_CD = RETRY_FRAMES };
        *SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;

        reset();

        assert!(!FETCHING.load(Ordering::SeqCst), "the fetch stayed latched — the page wedges");
        assert_eq!(unsafe { RETRY_CD }, 0);
        assert!(current().is_none());
    }
}
