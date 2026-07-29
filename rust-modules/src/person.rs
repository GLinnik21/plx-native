//! person — the actor/person page's data layer (the store `ui/person.rs` draws).
//!
//! Sibling of `metadata.rs` (the detail page's item store) and `browse.rs` (the Library's paged
//! catalog), and built on the same three pieces: [`crate::task::spawn_small`] + a `Mutex` mailbox
//! + a generation atomic, applied on the MAIN thread by [`pump`] once a frame while the page is
//! up. The worker never touches a static; [`current`] hands out a `&'static Person` that the draw
//! reads all frame, so the main thread stays its only writer (the same soundness rule
//! `metadata.rs`'s `CURRENT` carries).
//!
//! **Two requests per page, to two different services**, both spawned by [`pump`] and landing
//! through their own mailbox:
//!
//! * **the shelves**, from the LOCAL server — `GET /library/people/{personId}/media` returns
//!   everything the person appears in across EVERY library section at once, no per-section
//!   `?actor=<id>` sweep. The rows are split into the Movies / Shows shelves by **each row's own
//!   `type`**; the container's `viewGroup` is a trap (it read `"movie"` on a response whose only
//!   row was a `show`).
//! * **the biography**, from plex.tv — `GET discover.provider.plex.tv/library/people/{tagKey}`
//!   over the TLS+DNS `net.rs` path (see `plex/discover.rs` for the wire facts). This is the fetch
//!   that fills a header the local server cannot: the roles line, the born/died dates and the bio.
//!
//! **The header still MOUNTS on what it was handed.** `Role[]` already carries the name and the
//! headshot (`plex::Tag`), so the page draws instantly and the plex.tv fields fade in under the
//! name when they land. That ordering is the whole degrade strategy: the biography request can
//! fail, or the person can be one plex.tv has never heard of, and the page must then read as
//! *finished* at portrait + name — never as a row of fields that failed to load. Nothing in the
//! header is a placeholder; each line is drawn only when it has content.
//!
//! **The two id spaces are not interchangeable.** [`Person::key`] is the LOCAL `personId` (the
//! numeric `Tag::id`, or its `tagKey` when the server omitted the number) and addresses `/media`;
//! [`Person::guid`] is the `tagKey` and is the ONLY thing plex.tv answers to — the numeric id
//! 404s there (`"Invalid value provided for metadataId!"`). Keep both; do not collapse them.
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

/// How many departments the roles line names before it stops. Plex prints every one; on a couch
/// that turns a one-line kicker into "Actor, Writer, Producer, Composer, Costume Makeup" for a
/// jobbing actor (Peter Sallis has five). The wire order is most-credits-first, so the first three
/// ARE the ones worth naming.
const MAX_ROLES: usize = 3;

/// One person's page: the header handed in by the cast row, the plex.tv biography fields, and the
/// two shelves fetched from `/library/people/{key}/media`.
pub(crate) struct Person {
    /// the LOCAL `personId` this record was opened for (numeric tag id, or the guid when the
    /// server sent no number) — addresses `/library/people/{key}/media`
    pub(crate) key: String,
    /// the `tagKey` guid — the ONLY id `discover.provider.plex.tv` answers to (module docs).
    /// Empty when the credit row carried none, which simply means no biography is fetched.
    pub(crate) guid: String,
    pub(crate) name: String,
    pub(crate) thumb: String,
    /// Biography, from plex.tv. Empty until the profile fetch lands — and STAYS empty for a person
    /// plex.tv has no record of. Drawn only when non-empty.
    pub(crate) bio: String,
    /// The departments, already shortened + prettified for display: `"Actor, Producer"`. See
    /// [`roles_line`].
    pub(crate) roles: String,
    /// ISO `YYYY-MM-DD` birth / death dates and the birthplace, verbatim from plex.tv — the SCREEN
    /// formats them (`ui::fmt::pretty_date`), because how a date reads is a display decision.
    /// `died` is empty for someone living, which is why the page tests "non-empty", never "unknown".
    pub(crate) born: String,
    pub(crate) died: String,
    pub(crate) birthplace: String,
    pub(crate) movies: Vec<PmsMovie>,
    pub(crate) shows: Vec<PmsMovie>,
    /// the `/media` fetch has landed successfully at least once. The `browse.rs` `total < 0`
    /// sentinel in bool form: it is what separates "still loading" from "this person genuinely
    /// has nothing here", and what stops [`maybe_spawn`] re-fetching forever.
    pub(crate) landed: bool,
    /// the plex.tv profile fetch has ANSWERED at least once — including the answer "no such
    /// person", which arrives as an all-empty profile. Its only job is to stop the retry; the page
    /// never renders a "loading" state for the header, because a header that is complete without a
    /// biography must not flicker a spinner into a space it will never fill.
    pub(crate) profiled: bool,
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

/// The page's two fetches, in ONE index space — the single-flight flags, the retry countdowns and
/// the mailboxes are all arrays keyed by these, so adding a third is a constant plus one arm in
/// [`maybe_spawn`]/[`apply`], and [`supersede`] picks it up with no edit at all.
const F_MEDIA: usize = 0;
const F_PROFILE: usize = 1;
const NFETCH: usize = 2;

/// The claim that fetch `i` is out. Cleared ONLY by a mailbox take, so anything that drops the
/// mailbox ([`supersede`]) must clear it too — otherwise the fetch stays latched and the page
/// spins forever. Same latch `browse.rs` documents on its `IN_FLIGHT` array.
///
/// It bounds spawns per *pump*, which is what matters; it is NOT a hard one-worker-at-a-time
/// interlock, and claiming otherwise would be wrong. Two ways a second worker can briefly exist:
/// [`supersede`] releases the claim while the old worker is still running, and a take releases it
/// before the generation check (so a stale landing can free a NEWER fetch's claim, costing one
/// duplicate request). Neither can wedge or corrupt — [`land`] is monotone on the generation and
/// [`pump`] discards anything stale — and `browse.rs` has the identical shape.
static IN_FLIGHT: [AtomicBool; NFETCH] = [AtomicBool::new(false), AtomicBool::new(false)];
/// Frames left before fetch `i` may spawn again after a FAILED attempt (main-thread; [`pump`]
/// decrements). Stops a fast-failing network from spawning a worker every frame. PER-FETCH on
/// purpose: a plex.tv biography that cannot be reached (the TV is on a LAN with no internet — the
/// case this app is built to keep working) must not hold the LOCAL shelves off for two seconds a
/// go, and vice versa.
static mut RETRY_CD: [u32; NFETCH] = [0; NFETCH];
/// ~2s at 60fps — the same backoff `browse.rs` uses for a failed page.
const RETRY_FRAMES: u32 = 120;

/// What a finished fetch delivers. Both arms carry `Option`, and in BOTH the `None` means the same
/// thing: the fetch FAILED (transport, parse, or a panicking worker) and must be retried. It has to
/// stay distinguishable from a successful answer that happens to be empty — installing empty
/// shelves on a failure is the "one wifi hiccup blanked a populated grid" bug `browse.rs` carries
/// its `total < 0` sentinel for, and the profile has the same trap in a subtler form (plex.tv
/// answers 200 with an empty container for a person it has never heard of, which is an ANSWER).
enum Landing {
    Media(Option<(Vec<PmsMovie>, Vec<PmsMovie>)>),
    Profile(Option<crate::plex::discover::PersonProfile>),
}

struct Mail {
    gen: u32,
    what: Landing,
}
/// One mailbox per fetch, same index space as [`IN_FLIGHT`].
static SLOT: [Mutex<Option<Mail>>; NFETCH] = [Mutex::new(None), Mutex::new(None)];

/// Post a finished fetch to its mailbox. MONOTONE: an older fetch landing late must never clobber
/// a newer result the pump has not consumed yet. Named (not inlined in the worker closure) for the
/// same reason as `metadata::land_detail` — the guard is the one piece of this machinery a test
/// cannot reach through [`open`], because reaching it needs two overlapping real fetches.
fn land(i: usize, gen: u32, what: Landing) {
    let mut slot = SLOT[i].lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().map(|r| r.gen < gen).unwrap_or(true) {
        *slot = Some(Mail { gen, what });
    }
}

/// Invalidate everything in flight: bump the generation (a late landing is discarded), drop every
/// mailbox, release the single-flight flags with them, and clear the retry backoffs. The ONE place
/// those four move together.
fn supersede() {
    GEN.fetch_add(1, Ordering::SeqCst);
    for i in 0..NFETCH {
        *SLOT[i].lock().unwrap_or_else(|e| e.into_inner()) = None;
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
        unsafe { RETRY_CD[i] = 0 };
    }
}

// ---- public surface --------------------------------------------------------------------------

/// Open the page for `key` (a local `personId`) / `guid` (the `tagKey`) with the header the caller
/// already has — the cast row's name + headshot. MAIN THREAD, NON-BLOCKING: nothing is fetched
/// here, [`pump`] spawns both requests on the next frame, so the page mounts on its header
/// immediately and fills in around it.
pub(crate) fn open(key: &str, guid: &str, name: &str, thumb: &str) {
    supersede();
    unsafe {
        *addr_of_mut!(CURRENT) = Some(Person {
            key: key.to_string(),
            guid: guid.to_string(),
            name: name.to_string(),
            thumb: thumb.to_string(),
            bio: String::new(),
            roles: String::new(),
            born: String::new(),
            died: String::new(),
            birthplace: String::new(),
            movies: Vec::new(),
            shows: Vec::new(),
            landed: false,
            profiled: false,
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

/// MAIN THREAD, once a frame while the page is up: apply every landed fetch and schedule the next.
/// Returns true when the store just changed — the screen re-clamps its focus and rebuilds its
/// cached header strings on it.
pub(crate) fn pump() -> bool {
    let mut changed = false;
    for i in 0..NFETCH {
        unsafe {
            if RETRY_CD[i] > 0 {
                RETRY_CD[i] -= 1;
            }
        }
        let taken = SLOT[i].lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = taken {
            // the take ALWAYS releases the single-flight, whatever the landing turns out to be —
            // dropping a stale one without this is how the flag latches forever
            IN_FLIGHT[i].store(false, Ordering::SeqCst);
            if r.gen == GEN.load(Ordering::SeqCst) {
                changed |= apply(i, r.what);
            }
            // else superseded: a different person is open now, so this landing is news about
            // NEITHER of them. The failure arm inside `apply` is skipped with it on purpose — a
            // stale failure that armed the backoff would delay the new person's first fetch by
            // ~2s for a network error that was never about them.
        }
        maybe_spawn(i);
    }
    changed
}

/// Install ONE landing on the open person. Returns whether anything actually changed.
fn apply(i: usize, what: Landing) -> bool {
    let Some(p) = (unsafe { (*addr_of_mut!(CURRENT)).as_mut() }) else { return false };
    match what {
        // FAILED: leave the store exactly as it was and back off before retrying
        Landing::Media(None) | Landing::Profile(None) => {
            unsafe { RETRY_CD[i] = RETRY_FRAMES };
            false
        }
        Landing::Media(Some((movies, shows))) => {
            crate::log(&format!(
                "person: key={} '{}' movies={} shows={}",
                p.key, p.name, movies.len(), shows.len()
            ));
            p.movies = movies;
            p.shows = shows;
            p.landed = true;
            true
        }
        Landing::Profile(Some(prof)) => {
            // The name is NOT overwritten from here: the local `Role[]` tag is what the credits
            // shelf showed a moment ago, and swapping it for plex.tv's spelling mid-fetch would
            // rename the person under the user's eyes. Same for the headshot.
            p.roles = roles_line(&prof);
            p.bio = prof.summary;
            p.born = prof.born_at;
            p.died = prof.died_at;
            p.birthplace = prof.birth_place;
            p.profiled = true;
            crate::log(&format!(
                "person: profile guid={} roles='{}' born={} died={} bio={}B",
                p.guid, p.roles, !p.born.is_empty(), !p.died.is_empty(), p.bio.len()
            ));
            true
        }
    }
}

/// The roles line: the person's departments, most-credited first, prettified and capped at
/// [`MAX_ROLES`]. Pure, so the wire→display mapping is host-testable.
///
/// `CreditType.title` is the display name — **except** when the provider has none, where it repeats
/// the raw slug (`"costume-makeup"`, live on Peter Sallis). A leading lower-case letter is what
/// gives that away, so those are un-slugged and title-cased rather than printed as typed.
pub(crate) fn roles_line(prof: &crate::plex::discover::PersonProfile) -> String {
    prof.credit_types
        .iter()
        .filter_map(|c| {
            let raw = if c.title.is_empty() { &c.kind } else { &c.title };
            let pretty: String = raw
                .split(['-', '_', ' '])
                .filter(|w| !w.is_empty())
                .map(|w| {
                    let mut ch = w.chars();
                    match ch.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!pretty.is_empty()).then_some(pretty)
        })
        .take(MAX_ROLES)
        .collect::<Vec<_>>()
        .join(", ")
}

/// One fetch of kind `i` at a time, and only while the open person still wants it. Re-entered every
/// frame by [`pump`], which is what makes the failure path self-healing: a refused `spawn_small`
/// (the device's thread ceiling) or a transient network error simply retries after the backoff
/// instead of latching the page on a spinner forever.
fn maybe_spawn(i: usize) {
    if IN_FLIGHT[i].load(Ordering::SeqCst) || unsafe { RETRY_CD[i] > 0 } {
        return;
    }
    let Some(p) = current() else { return };
    // What each fetch needs to exist at all, and what says it is already done. A person with no
    // `tagKey` simply never asks plex.tv anything — that is the "not actionable" case, not a
    // failure, so it must not spin a worker every frame either.
    let (want, key) = match i {
        F_MEDIA => (!p.landed && !p.key.is_empty(), p.key.clone()),
        F_PROFILE => (!p.profiled && !p.guid.is_empty(), p.guid.clone()),
        _ => return, // `pump` only ever passes 0..NFETCH
    };
    if !want {
        return;
    }
    let gen = GEN.load(Ordering::SeqCst);
    IN_FLIGHT[i].store(true, Ordering::SeqCst);
    let spawned = crate::task::spawn_small("person", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an empty filmography / an empty biography
        let what = if i == F_MEDIA {
            Landing::Media(
                catch_unwind(|| {
                    let mc = crate::plex::client_opt()?.person_media(&key)?;
                    Some(split_by_type(&mc))
                })
                .unwrap_or(None),
            )
        } else {
            Landing::Profile(catch_unwind(|| fetch_profile(&key)).unwrap_or(None))
        };
        land(i, gen, what);
    });
    if !spawned {
        // nothing will ever fill the mailbox, and the flag is cleared only by a take — release it
        // here or this person never fetches again. `maybe_spawn` runs every frame, so this retries
        // by itself.
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
    }
}

/// WORKER THREAD: the blocking plex.tv biography request. The identity it presents is the persisted
/// login session's — the same `client_id` (+ account token when signed in) every other plex.tv call
/// in the app carries, via the same [`AccountClient`](crate::plex::account::AccountClient). Reading
/// the session here rather than passing it in keeps this off the main thread's critical path; it is
/// one small file read per person page.
#[cfg(not(test))]
fn fetch_profile(guid: &str) -> Option<crate::plex::discover::PersonProfile> {
    let s = crate::plex::session::load();
    let tok = (!s.account_token.is_empty()).then_some(s.account_token.as_str());
    crate::plex::account::AccountClient::new(&s.client_id, tok).person_profile(guid)
}

/// HOST SUITE: this is the crate's TLS seam, and the dev Mac has no libcurl to satisfy it — a test
/// that merely *reaches* this function fails to **link**, not to assert (the boundary the root
/// `CLAUDE.md` calls structural limit #1, and the reason `ff.rs` cfg-gates its `#[link]`s). `pump`
/// is called by tests, so the reference has to be cut here rather than avoided by discipline.
/// Everything downstream of the request — the mailbox, the generation guard, the unknown-vs-failed
/// distinction and the roles mapping — is exercised directly through [`land`]/[`pump`]/[`roles_line`],
/// which is where the logic worth testing actually lives.
#[cfg(test)]
fn fetch_profile(_guid: &str) -> Option<crate::plex::discover::PersonProfile> {
    None
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

    /// Park BOTH fetches, so a take's RELEASE of a single-flight flag is observable rather than
    /// immediately masked by the next fetch claiming it — and, more importantly, so the host suite
    /// spawns NO worker: one would reach for a PMS client that isn't installed and for a plex.tv
    /// these tests must never touch, and a stray background thread also perturbs the process-wide
    /// fd count `stream.rs`'s tests assert on. Call it before every `pump()`.
    fn hold_off() {
        unsafe { RETRY_CD = [RETRY_FRAMES; NFETCH] };
    }

    fn profile(bio: &str, born: &str, died: &str) -> crate::plex::discover::PersonProfile {
        crate::plex::discover::PersonProfile {
            summary: bio.to_string(),
            born_at: born.to_string(),
            died_at: died.to_string(),
            ..Default::default()
        }
    }

    /// A late landing for the actor you already left must not repopulate the one you are looking
    /// at. `open` bumps the generation; `pump` drops anything older — and it must still clear the
    /// single-flight flag while doing so, or the NEW person can never fetch.
    #[test]
    fn a_landing_from_the_previous_person_is_discarded_but_still_releases_the_fetch() {
        let _serial = crate::testlock::serial();
        open("161", "5d776", "Idina Menzel", "");
        let stale = GEN.load(Ordering::SeqCst);
        open("465", "5d777", "Cynthia Erivo", ""); // supersedes: the fetch above is now obsolete

        IN_FLIGHT[F_MEDIA].store(true, Ordering::SeqCst);
        hold_off();
        land(F_MEDIA, stale, Landing::Media(Some((vec![PmsMovie::default()], Vec::new()))));
        assert!(!pump(), "a superseded landing must not publish");

        let p = current().expect("the new person stays open");
        assert_eq!(p.key, "465");
        assert!(p.movies.is_empty(), "the previous actor's filmography leaked in");
        assert!(!p.landed, "a discarded landing must not settle the spinner");
        assert!(!IN_FLIGHT[F_MEDIA].load(Ordering::SeqCst), "the take must release the single-flight even for a landing it drops");
        close();
    }

    /// A FAILED fetch (None) must leave a populated page alone and schedule a retry — the
    /// "one wifi hiccup blanked a populated grid" regression, in this store's shape.
    #[test]
    fn a_failed_fetch_keeps_the_shelves_and_backs_off_instead_of_publishing_empty() {
        let _serial = crate::testlock::serial();
        open("161", "5d776", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);
        // seed a populated, landed page the honest way (through the pump)
        land(F_MEDIA, gen, Landing::Media(Some((vec![PmsMovie::default()], Vec::new()))));
        hold_off();
        assert!(pump());
        assert_eq!(current().unwrap().movies.len(), 1);

        land(F_MEDIA, gen, Landing::Media(None)); // the retry fails
        hold_off();
        assert!(!pump(), "a failure publishes nothing");
        assert_eq!(current().unwrap().movies.len(), 1, "the failure wiped a populated shelf");
        assert_eq!(unsafe { RETRY_CD[F_MEDIA] }, RETRY_FRAMES, "a failure must back off before retrying");
        close();
    }

    /// `close`/`reset` drop the mailboxes, and a single-flight flag is cleared ONLY by a successful
    /// take — so they must clear EVERY one (and the backoffs) themselves or the next person opened
    /// never fetches. The `browse.rs` latch, one store over, now once per fetch kind.
    #[test]
    fn close_clears_every_single_flight_flag_and_retry_backoff() {
        let _serial = crate::testlock::serial();
        open("161", "5d776", "Idina Menzel", "");
        for i in 0..NFETCH {
            IN_FLIGHT[i].store(true, Ordering::SeqCst);
            unsafe { RETRY_CD[i] = RETRY_FRAMES };
        }

        reset();

        for i in 0..NFETCH {
            assert!(!IN_FLIGHT[i].load(Ordering::SeqCst), "fetch {i} stayed latched — the page wedges");
            assert_eq!(unsafe { RETRY_CD[i] }, 0);
        }
        assert!(current().is_none());
    }

    /// The plex.tv profile lands into the HEADER fields and settles the fetch — and it must not
    /// disturb the shelves, which come from a different service on a different mailbox.
    #[test]
    fn a_profile_landing_fills_the_header_without_touching_the_shelves() {
        let _serial = crate::testlock::serial();
        open("6059", "5d7768268718ba001e311be6", "Peter Sallis", "");
        let gen = GEN.load(Ordering::SeqCst);
        land(F_MEDIA, gen, Landing::Media(Some((vec![PmsMovie::default()], Vec::new()))));
        hold_off();
        assert!(pump());

        land(F_PROFILE, gen, Landing::Profile(Some(profile("An English actor.", "1921-02-01", "2017-06-02"))));
        hold_off();
        assert!(pump(), "the profile landing is a change the screen must see");
        let p = current().unwrap();
        assert_eq!(p.bio, "An English actor.");
        assert_eq!(p.born, "1921-02-01");
        assert_eq!(p.died, "2017-06-02", "a deceased person's Died line has to survive the landing");
        assert!(p.profiled);
        assert_eq!(p.movies.len(), 1, "the biography landing wiped the shelves");
        close();
    }

    /// plex.tv answers **200 with an empty container** for a person it has never heard of, which
    /// `person_profile` turns into a DEFAULT profile. That is an answer, not a failure: it must
    /// settle `profiled` (so the page stops asking) and arm NO backoff — while a real failure does
    /// the opposite. Getting this backwards is a page that re-requests a biography forever.
    #[test]
    fn an_unknown_person_settles_the_profile_while_a_failure_backs_off() {
        let _serial = crate::testlock::serial();
        open("6059", "0000000000000000000000ff", "Nobody", "");
        let gen = GEN.load(Ordering::SeqCst);
        hold_off();

        land(F_PROFILE, gen, Landing::Profile(Some(crate::plex::discover::PersonProfile::default())));
        assert!(pump());
        let p = current().unwrap();
        assert!(p.profiled, "an 'unknown person' answer must settle, not retry forever");
        assert!(p.bio.is_empty() && p.roles.is_empty(), "nothing may be invented for an unknown person");
        assert!(unsafe { RETRY_CD[F_PROFILE] } < RETRY_FRAMES, "an ANSWER must not arm the failure backoff");

        land(F_PROFILE, gen, Landing::Profile(None)); // now a real transport failure
        hold_off();
        assert!(!pump());
        assert_eq!(unsafe { RETRY_CD[F_PROFILE] }, RETRY_FRAMES, "a failure must back off before retrying");
        close();
    }

    /// The roles line is Plex's "Actor, Producer" kicker: display titles, most-credited first,
    /// capped — and the provider's own un-named departments (`title == the raw slug`, live on Peter
    /// Sallis's `costume-makeup`) are un-slugged rather than printed as typed.
    #[test]
    fn the_roles_line_prettifies_slugs_and_caps_the_list() {
        let ct = |kind: &str, title: &str| crate::plex::discover::CreditType {
            kind: kind.to_string(),
            title: title.to_string(),
        };
        let mut prof = crate::plex::discover::PersonProfile::default();
        prof.credit_types = vec![
            ct("actor", "Actor"),
            ct("writer", "Writer"),
            ct("producer", "Producer"),
            ct("music", "Composer"), // past the cap
        ];
        assert_eq!(roles_line(&prof), "Actor, Writer, Producer");

        prof.credit_types = vec![ct("costume-makeup", "costume-makeup"), ct("art", "")];
        assert_eq!(roles_line(&prof), "Costume Makeup, Art", "a raw slug reached the screen");

        assert_eq!(roles_line(&crate::plex::discover::PersonProfile::default()), "");
    }
}
