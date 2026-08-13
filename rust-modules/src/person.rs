//! person — the actor/person page's data layer (the store `ui/person.rs` draws).
//!
//! Sibling of `metadata.rs` (the detail page's item store) and `browse.rs` (the Library's paged
//! catalog), and built on the same three pieces: [`crate::task::spawn_small`] + a `Mutex` mailbox
//! + a generation atomic, applied on the MAIN thread by [`pump`] once a frame while the page is
//! up. The worker never touches a static; [`current`] hands out a `&'static Person` that the draw
//! reads all frame, so the main thread stays its only writer (the same soundness rule
//! `metadata.rs`'s `CURRENT` carries).
//!
//! **Three requests per page, to two different services**, each spawned by [`pump`] and landing
//! through its own mailbox:
//!
//! * **the shelves**, from the LOCAL server — `GET /library/people/{personId}/media` returns
//!   everything the person appears in across EVERY library section at once, no per-section
//!   `?actor=<id>` sweep. The rows are split into the Movies / Shows shelves by **each row's own
//!   `type`**; the container's `viewGroup` is a trap (it read `"movie"` on a response whose only
//!   row was a `show`).
//! * **the biography**, from plex.tv — `GET discover.provider.plex.tv/library/people/{tagKey}`
//!   over the TLS+DNS `net.rs` path (see `plex/discover.rs` for the wire facts). This is the fetch
//!   that fills a header the local server cannot: the roles line, the born/died dates and the bio.
//! * **the character names**, from the LOCAL server — one batched
//!   `GET /library/metadata/{every shelf key}` ([`crate::plex::Client::metadata_many`]). It exists
//!   because the shelf listing above does NOT carry them: its rows have a `Role[]` whose entries
//!   hold only `tag`, so "which part did they play in this one" is a second read. It is therefore
//!   the ONE fetch here that DEPENDS on another: it can only be addressed once the shelves have
//!   landed, and [`maybe_spawn`] expresses that by keying it on the shelf keys, which are empty
//!   until then.
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

/// One shelf of a person's page. The three fields move together and must never be updated apart:
/// the captions describe THESE items by index, and the total is the number to print rather than
/// `items.len()`.
#[derive(Default)]
pub(crate) struct Shelf {
    /// the tiles, capped at [`SHELF_MAX`]
    pub(crate) items: Vec<PmsMovie>,
    /// How many items of this kind the `/media` response REALLY held. Distinct from `items.len()`,
    /// which is capped at [`SHELF_MAX`] (a `CardRow` spring-array limit): a prolific actor has 60
    /// movies in the library, and a heading that read "24" would be the cap masquerading as a fact
    /// about the person.
    pub(crate) total: usize,
    /// The character each tile is this person's credit for (`"Wallace (voice)"`), PARALLEL to
    /// `items` — index `i` captions tile `i`, `""` where the server named no part. A parallel vector
    /// rather than a field on `PmsMovie`, because a character name is a fact about a *person's
    /// credit in* an item, not about the item: the same movie on a home shelf has no role.
    pub(crate) roles: Vec<String>,
}

/// The shelves a person's page has, in flow order. `ui/person.rs` indexes everything by this.
pub(crate) const NSHELF: usize = 2;

/// One person's page: the header handed in by the cast row, the plex.tv biography fields, and the
/// two shelves fetched from `/library/people/{key}/media`.
pub(crate) struct Person {
    /// WHICH SERVER [`Person::key`] is a `personId` ON. A personId is server-local exactly like a
    /// ratingKey (docs/shared-servers.md §1), so `key` alone names a person on no machine in
    /// particular once a share is registered — the pair is the identity, compared through
    /// [`crate::plex::same_item`]. `guid` needs no such scoping: it is plex.tv's, and global.
    pub(crate) sid: crate::plex::ServerId,
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
    /// The two shelves, indexed by KIND (0 = Movies, 1 = Shows) — the same index the screen's focus
    /// model, headings and hit-test use, so "which shelf" is spelled one way everywhere.
    pub(crate) shelves: [Shelf; NSHELF],
    /// the `/media` fetch has landed successfully at least once. The `browse.rs` `total < 0`
    /// sentinel in bool form: it is what separates "still loading" from "this person genuinely
    /// has nothing here", and what stops [`maybe_spawn`] re-fetching forever.
    pub(crate) landed: bool,
    /// the plex.tv profile fetch has ANSWERED at least once — including the answer "no such
    /// person", which arrives as an all-empty profile. Its only job is to stop the retry; the page
    /// never renders a "loading" state for the header, because a header that is complete without a
    /// biography must not flicker a spinner into a space it will never fill.
    pub(crate) profiled: bool,
    /// the batched character-name read has ANSWERED for the CURRENT shelves. Cleared by every media
    /// landing (the keys it was addressed to are gone), which is what re-asks for the new list.
    pub(crate) roled: bool,
}

impl Person {
    /// The tiles of shelf `kind`.
    pub(crate) fn shelf(&self, kind: usize) -> &[PmsMovie] {
        &self.shelves[kind].items
    }
    /// The character tile `i` of shelf `kind` credits this person as — `""` until the batched read
    /// lands, and `""` forever for a credit the server names no part for (every CREW credit, since
    /// a director appears in the item's `Director[]`, not its `Role[]`). Bounds-checked rather than
    /// indexed: the roles vector is filled a frame or more after the shelf it captions.
    pub(crate) fn role(&self, kind: usize, i: usize) -> &str {
        self.shelves[kind].roles.get(i).map(String::as_str).unwrap_or("")
    }
    /// How many items of kind `kind` the server really had — what a heading prints (see
    /// [`Shelf::total`]).
    pub(crate) fn total(&self, kind: usize) -> usize {
        self.shelves[kind].total
    }
    /// Every shelf key, in flow order — and the roles fetch's cache key: empty until the shelves
    /// land (so nothing is asked too early) and it CHANGES with them (so a re-landing re-asks). At
    /// most `NSHELF * SHELF_MAX` keys.
    fn shelf_keys(&self) -> Vec<&str> {
        self.shelves
            .iter()
            .flat_map(|s| s.items.iter())
            .map(|m| m.rk.as_str())
            .filter(|rk| !rk.is_empty())
            .collect()
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

/// The page's three fetches, in ONE index space — the single-flight flags, the retry countdowns and
/// the mailboxes are all arrays keyed by these, so adding one is a constant plus one arm in
/// [`maybe_spawn`]/[`apply`], and [`supersede`] picks it up with no edit at all (the roles fetch
/// was exactly that).
const F_MEDIA: usize = 0;
const F_PROFILE: usize = 1;
const F_ROLES: usize = 2;
const NFETCH: usize = 3;

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
static IN_FLIGHT: [AtomicBool; NFETCH] =
    [AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)];
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
    Media(Option<[Shelf; NSHELF]>),
    Profile(Option<crate::plex::discover::PersonProfile>),
    Roles(Option<RolesLanding>),
}

/// A finished character-name batch. `keys` is the shelf-key list it was ADDRESSED to, which rides
/// along because [`apply`] must refuse a landing for a shelf list that has since been replaced
/// (see there). `pairs` is already filtered to THIS person's credit per item — the worker walks the
/// batched response so ~30 KB of tag arrays never crosses the mailbox.
pub(crate) struct RolesLanding {
    keys: Vec<String>,
    pairs: Vec<(String, String)>,
}

struct Mail {
    gen: u32,
    what: Landing,
}
/// One mailbox per fetch, same index space as [`IN_FLIGHT`].
static SLOT: [Mutex<Option<Mail>>; NFETCH] = [Mutex::new(None), Mutex::new(None), Mutex::new(None)];

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
///
/// `sid` is the server whose `/library/people/{key}/media` this page will read — the server the
/// credit row came from, captured by the caller. The two ids are already documented as
/// non-interchangeable (module doc); this is the third fact about `key`: it is only meaningful
/// against one machine.
pub(crate) fn open(sid: crate::plex::ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    supersede();
    unsafe {
        *addr_of_mut!(CURRENT) = Some(Person {
            sid,
            key: key.to_string(),
            guid: guid.to_string(),
            name: name.to_string(),
            thumb: thumb.to_string(),
            bio: String::new(),
            roles: String::new(),
            born: String::new(),
            died: String::new(),
            birthplace: String::new(),
            shelves: Default::default(),
            landed: false,
            profiled: false,
            roled: false,
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
        Landing::Media(None) | Landing::Profile(None) | Landing::Roles(None) => {
            unsafe { RETRY_CD[i] = RETRY_FRAMES };
            false
        }
        Landing::Media(Some(shelves)) => {
            crate::log(&format!(
                "person: key={} '{}' movies={}/{} shows={}/{}",
                p.key, p.name,
                shelves[0].items.len(), shelves[0].total,
                shelves[1].items.len(), shelves[1].total
            ));
            // Assigning the whole array is what carries the invariant: the captions described the OLD
            // items, so they go WITH them (a `Shelf` lands with `roles` empty) and `roled` re-asks.
            // As three loose fields this was three things to remember.
            p.shelves = shelves;
            p.landed = true;
            p.roled = false;
            true
        }
        Landing::Roles(Some(RolesLanding { keys, pairs })) => {
            // A landing addressed to a shelf list that has since been REPLACED must not settle the
            // current one: the IN_FLIGHT doc allows a brief duplicate same-generation media worker,
            // so a second media landing can swap the shelves while a roles batch for the first list
            // is in flight. Refusing it (without arming the failure backoff — nothing failed) leaves
            // `roled` false, and `maybe_spawn` simply re-asks with the keys that are now true.
            if keys != p.shelf_keys() {
                return false;
            }
            // match by ratingKey, never by index: the pairs arrive in the batched response's order,
            // which the server does keep, but nothing about THIS store should depend on it
            for sh in p.shelves.iter_mut() {
                sh.roles = sh
                    .items
                    .iter()
                    .map(|m| {
                        pairs
                            .iter()
                            .find(|(rk, _)| *rk == m.rk)
                            .map(|(_, role)| role.clone())
                            .unwrap_or_default()
                    })
                    .collect();
            }
            p.roled = true;
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

/// What fetch `i` should be addressed to right now, or `None` when it wants nothing — the ONE place
/// each fetch's precondition and its key live together, so a fetch cannot be gated on one thing and
/// keyed off another.
///
/// `None` covers both "already answered" and **"not actionable"**: a person with no `tagKey` never
/// asks plex.tv anything, which is not a failure and must not spin a worker every frame. The roles
/// fetch keys on the SHELF list — empty until the media landing — which is what sequences it after
/// `F_MEDIA` with no explicit ordering machinery, and re-asks when the shelves change.
///
/// Called every frame for the page's whole life, so it allocates only when it will actually spawn.
fn address(i: usize, p: &Person) -> Option<Vec<String>> {
    let one = |s: &String| (!s.is_empty()).then(|| vec![s.clone()]);
    match i {
        F_MEDIA if !p.landed => one(&p.key),
        F_PROFILE if !p.profiled => one(&p.guid),
        F_ROLES if p.landed && !p.roled => {
            let keys = p.shelf_keys();
            (!keys.is_empty()).then(|| keys.into_iter().map(str::to_string).collect())
        }
        _ => None,
    }
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
    let Some(key) = address(i, p) else { return };
    // the person's two ids ride along for the roles worker, which filters the batched response down
    // to their own credit per row — either id can be the one the response's tags actually carry
    let (id, guid) = (p.key.clone(), p.guid.clone());
    // the page's server, captured here on the MAIN thread — the worker must neither read the
    // current server nor stamp its rows with it (see `pms::parse_item`)
    let sid = p.sid;
    let gen = GEN.load(Ordering::SeqCst);
    IN_FLIGHT[i].store(true, Ordering::SeqCst);
    let spawned = crate::task::spawn_small("person", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an empty filmography / an empty biography / a page with no captions
        let what = match i {
            F_MEDIA => Landing::Media(
                catch_unwind(|| {
                    let mc = crate::plex::client_for(sid)?.person_media(&key[0])?;
                    Some(split_by_type(&mc, sid))
                })
                .unwrap_or(None),
            ),
            F_PROFILE => Landing::Profile(catch_unwind(|| fetch_profile(&key[0])).unwrap_or(None)),
            F_ROLES => Landing::Roles(
                catch_unwind(|| {
                    let keys: Vec<&str> = key.iter().map(String::as_str).collect();
                    let mc = crate::plex::client_for(sid)?.metadata_many(&keys)?;
                    let pairs = roles_from(&mc, &id, &guid);
                    Some(RolesLanding { keys: key.clone(), pairs })
                })
                .unwrap_or(None),
            ),
            _ => return, // `pump` only ever passes 0..NFETCH
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

/// `(ratingKey, character)` for every row of a batched `/library/metadata/{csv}` response, keeping
/// only THIS person's credit in each — the full record's `Role[]` names every cast member, and the
/// page wants one line, "what did *this* person play in it".
///
/// Which tag IS this person is [`crate::plex::Tag::is_person`]'s job — both id spaces, because
/// [`Person::key`] is the `tagKey` guid whenever the credit row carried no number. Rows with **no
/// part for them** (every CREW credit — a director appears in `Director[]`, not `Role[]`) are simply
/// absent from the result rather than carried as empty pairs, and `apply` reads a missing key as
/// `""`. Naming their department instead would need the crew arrays this batch excludes, for a line
/// the mockup does not ask for.
pub(crate) fn roles_from(
    mc: &crate::plex::MediaContainer,
    id: &str,
    guid: &str,
) -> Vec<(String, String)> {
    mc.metadata
        .iter()
        .filter_map(|it| {
            let r = it.role.iter().find(|r| r.is_person(id, guid))?;
            (!r.role.is_empty()).then(|| (it.rating_key.clone(), r.role.clone()))
        })
        .collect()
}

/// Split a `/library/people/{id}/media` container into the Movies and Shows shelves **by each
/// row's own `type`**, counting the REAL totals past the [`SHELF_MAX`] tile cap.
///
/// The container's `viewGroup` cannot be used for this and is the whole reason this function is
/// named and tested: verified live 2026-07-29, person 6059's response carries `viewGroup:"movie"`
/// over five movies AND one show. Anything that is neither a `movie` nor a `show` is dropped —
/// the page has exactly two shelves and they are labelled, so silently filing an episode under
/// "Shows" would put a landscape still in a portrait poster slot.
pub(crate) fn split_by_type(mc: &crate::plex::MediaContainer, sid: crate::plex::ServerId) -> [Shelf; NSHELF] {
    let mut out: [Shelf; NSHELF] = Default::default();
    for it in &mc.metadata {
        let sh = match it.kind.as_str() {
            "movie" => &mut out[0],
            "show" => &mut out[1],
            _ => continue,
        };
        sh.total += 1;
        if sh.items.len() < SHELF_MAX {
            sh.items.push(parse_item(it, sid));
        }
    }
    out
}

/// TEST ONLY: publish shelves onto the open person exactly as a successful landing would, so the
/// screen's focus/flow tests need neither a server nor the mailbox.
#[cfg(test)]
pub(crate) fn install_for_test(movies: Vec<PmsMovie>, shows: Vec<PmsMovie>) {
    if let Some(p) = unsafe { (*addr_of_mut!(CURRENT)).as_mut() } {
        p.shelves = [movies, shows].map(|items| Shelf { total: items.len(), items, roles: Vec::new() });
        p.landed = true;
        p.roled = false;
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::{MediaContainer, Metadata};


    /// A [`Landing::Media`] payload the way a worker builds one — totals = lens, i.e. an uncapped
    /// response.
    fn media(movies: Vec<PmsMovie>, shows: Vec<PmsMovie>) -> Landing {
        Landing::Media(Some(
            [movies, shows].map(|items| Shelf { total: items.len(), items, roles: Vec::new() }),
        ))
    }

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
            row("movie", "1", "A Movie"),
            row("show", "1975", "A Show"),
            row("movie", "2", "Another Movie"),
        ];
        let s = split_by_type(&mc, crate::plex::ServerId::UNSET);
        assert_eq!(s[0].items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(s[1].items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1975"]);
        assert_eq!((s[0].total, s[1].total), (2, 1));
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
        let s = split_by_type(&mc, crate::plex::ServerId::UNSET);
        assert_eq!(s[0].items.len(), SHELF_MAX);
        assert_eq!(s[0].total, SHELF_MAX + 5, "the count is the RESPONSE's total, not the tile cap");
        assert!(s[1].items.is_empty(), "an episode/season is not a Show shelf tile");
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
        open(crate::plex::ServerId::UNSET, "161", "5d776", "Idina Menzel", "");
        let stale = GEN.load(Ordering::SeqCst);
        open(crate::plex::ServerId::UNSET, "465", "5d777", "Cynthia Erivo", ""); // supersedes: the fetch above is now obsolete

        IN_FLIGHT[F_MEDIA].store(true, Ordering::SeqCst);
        hold_off();
        land(F_MEDIA, stale, media(vec![PmsMovie::default()], Vec::new()));
        assert!(!pump(), "a superseded landing must not publish");

        let p = current().expect("the new person stays open");
        assert_eq!(p.key, "465");
        assert!(p.shelf(0).is_empty(), "the previous actor's filmography leaked in");
        assert!(!p.landed, "a discarded landing must not settle the spinner");
        assert!(!IN_FLIGHT[F_MEDIA].load(Ordering::SeqCst), "the take must release the single-flight even for a landing it drops");
        close();
    }

    /// A FAILED fetch (None) must leave a populated page alone and schedule a retry — the
    /// "one wifi hiccup blanked a populated grid" regression, in this store's shape.
    #[test]
    fn a_failed_fetch_keeps_the_shelves_and_backs_off_instead_of_publishing_empty() {
        let _serial = crate::testlock::serial();
        open(crate::plex::ServerId::UNSET, "161", "5d776", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);
        // seed a populated, landed page the honest way (through the pump)
        land(F_MEDIA, gen, media(vec![PmsMovie::default()], Vec::new()));
        hold_off();
        assert!(pump());
        assert_eq!(current().unwrap().shelf(0).len(), 1);

        land(F_MEDIA, gen, Landing::Media(None)); // the retry fails
        hold_off();
        assert!(!pump(), "a failure publishes nothing");
        assert_eq!(current().unwrap().shelf(0).len(), 1, "the failure wiped a populated shelf");
        assert_eq!(unsafe { RETRY_CD[F_MEDIA] }, RETRY_FRAMES, "a failure must back off before retrying");
        close();
    }

    /// `close`/`reset` drop the mailboxes, and a single-flight flag is cleared ONLY by a successful
    /// take — so they must clear EVERY one (and the backoffs) themselves or the next person opened
    /// never fetches. The `browse.rs` latch, one store over, now once per fetch kind.
    #[test]
    fn close_clears_every_single_flight_flag_and_retry_backoff() {
        let _serial = crate::testlock::serial();
        open(crate::plex::ServerId::UNSET, "161", "5d776", "Idina Menzel", "");
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
        open(crate::plex::ServerId::UNSET, "6059", "5d7768268718ba001e311be6", "Peter Sallis", "");
        let gen = GEN.load(Ordering::SeqCst);
        land(F_MEDIA, gen, media(vec![PmsMovie::default()], Vec::new()));
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
        assert_eq!(p.shelf(0).len(), 1, "the biography landing wiped the shelves");
        close();
    }

    /// plex.tv answers **200 with an empty container** for a person it has never heard of, which
    /// `person_profile` turns into a DEFAULT profile. That is an answer, not a failure: it must
    /// settle `profiled` (so the page stops asking) and arm NO backoff — while a real failure does
    /// the opposite. Getting this backwards is a page that re-requests a biography forever.
    #[test]
    fn an_unknown_person_settles_the_profile_while_a_failure_backs_off() {
        let _serial = crate::testlock::serial();
        open(crate::plex::ServerId::UNSET, "6059", "0000000000000000000000ff", "Nobody", "");
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

    /// `roles_from` keeps exactly THIS person's credit per row — matched by the tag's numeric id
    /// OR its `tagKey` guid, because [`Person::key`] is the guid whenever the credit row carried no
    /// number, and an id-only match then blanked every caption while still paying for the batch. A
    /// row where the person is crew only (no `Role[]` entry of theirs) yields `""`, never a
    /// neighbour's character.
    #[test]
    fn roles_from_matches_by_id_or_tag_key_and_leaves_crew_rows_blank() {
        let tag = |name: &str, role: &str, id: i64, guid: &str| crate::plex::Tag {
            tag: name.into(),
            role: role.into(),
            id,
            tag_key: guid.into(),
            ..Default::default()
        };
        let mut mc = MediaContainer::default();
        let mut with_cast = row("movie", "1971", "A Close Shave");
        with_cast.role = vec![
            tag("Peter Sallis", "Wallace (voice)", 6059, "5d7768268718ba001e311be6"),
            tag("Anne Reid", "Wendolene (voice)", 7001, "5d776828a091de001f2e63e6"),
        ];
        let mut crew_only = row("movie", "2005", "The Curse of the Were-Rabbit");
        crew_only.role = vec![tag("Helena Bonham Carter", "Lady Tottington", 7002, "")];
        mc.metadata = vec![with_cast, crew_only];

        // ONLY the row this person has a part in — the crew-only row is absent rather than carried as
        // an empty pair, and `apply` reads a missing key as "" anyway
        let want = vec![("1971".to_string(), "Wallace (voice)".to_string())];
        assert_eq!(roles_from(&mc, "6059", "5d7768268718ba001e311be6"), want);
        // a person OPENED BY GUID (no numeric id on the credit row) must match through the tagKey —
        // this is the case an id-only match silently reduced to a wasted fetch and blank captions
        assert_eq!(roles_from(&mc, "5d7768268718ba001e311be6", "5d7768268718ba001e311be6"), want);
        // an id of 0 means "the server sent none" — it must never match a page opened for key "0",
        // and an EMPTY guid must never match a tag whose tagKey is also empty
        assert!(roles_from(&mc, "0", "").is_empty());
    }

    /// A roles landing captions the shelves BY KEY (order-independent), and the NEXT media landing
    /// clears both vectors with the shelves they described — a caption must never outlive the list
    /// it was addressed to, and the cleared `roled` is what re-asks for the new one.
    #[test]
    fn a_roles_landing_captions_by_key_and_a_media_landing_resets_it() {
        let _serial = crate::testlock::serial();
        open(crate::plex::ServerId::UNSET, "6059", "5d7768268718ba001e311be6", "Peter Sallis", "");
        let gen = GEN.load(Ordering::SeqCst);
        let movie = |rk: &str| PmsMovie { rk: rk.to_string(), ..Default::default() };
        land(F_MEDIA, gen, media(vec![movie("1971"), movie("2005")], vec![movie("1975")]));
        hold_off();
        assert!(pump());
        assert!(!current().unwrap().roled, "captions cannot predate the read that fetches them");

        // a landing addressed to keys the store no longer holds must be REFUSED — and without the
        // failure backoff: it is stale, not broken, and the re-ask must be free to go at once. The
        // sentinel countdown still parks the spawn (see `hold_off`), but is small enough that a
        // wrongly-armed RETRY_FRAMES would overwrite it visibly.
        let stale = RolesLanding { keys: vec!["9999".to_string()], pairs: vec![("9999".to_string(), "Nobody".to_string())] };
        land(F_ROLES, gen, Landing::Roles(Some(stale)));
        hold_off();
        unsafe { RETRY_CD[F_ROLES] = 5 };
        assert!(!pump(), "a mis-addressed caption landing published");
        assert!(!current().unwrap().roled, "a stale landing settled the CURRENT list's captions");
        assert_eq!(unsafe { RETRY_CD[F_ROLES] }, 4, "staleness is not a failure — no backoff armed (the sentinel just ticked)");

        // pairs arrive REVERSED relative to the shelves — the match is by rk, so it must not matter
        let keys = current().unwrap().shelf_keys().iter().map(|k| k.to_string()).collect();
        let pairs = vec![
            ("1975".to_string(), "Wallace".to_string()),
            ("2005".to_string(), "Wallace / Hutch (voice)".to_string()),
            ("1971".to_string(), "Wallace (voice)".to_string()),
        ];
        land(F_ROLES, gen, Landing::Roles(Some(RolesLanding { keys, pairs })));
        hold_off();
        assert!(pump(), "a caption landing is a change the screen must see");
        let p = current().unwrap();
        assert_eq!(p.role(0, 0), "Wallace (voice)");
        assert_eq!(p.role(0, 1), "Wallace / Hutch (voice)");
        assert_eq!(p.role(1, 0), "Wallace");
        assert_eq!(p.role(0, 99), "", "past-the-end reads are blank, not a panic");
        assert!(p.roled);

        // a fresh media landing (same person) replaces the shelves — the captions go WITH them
        land(F_MEDIA, gen, media(vec![movie("2005")], Vec::new()));
        hold_off();
        assert!(pump());
        let p = current().unwrap();
        assert_eq!(p.role(0, 0), "", "a caption survived the list it was addressed to");
        assert!(!p.roled, "the reset is what re-asks for the new list's captions");
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
