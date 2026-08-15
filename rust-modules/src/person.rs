//! person — the actor/person page's data layer (the store `ui/person.rs` draws).
//!
//! Sibling of `metadata.rs` (the detail page's item store) and `browse.rs` (the Library's paged
//! catalog), and built on the same three pieces: [`crate::task::spawn_small`] + a `Mutex` mailbox
//! + a generation atomic, applied on the MAIN thread by [`pump`] once a frame while the page is
//! up. The worker never touches a static; [`current`] hands out a `&'static Person` that the draw
//! reads all frame, so the main thread stays its only writer (the same soundness rule
//! `metadata.rs`'s `CURRENT` carries).
//!
//! ## A filmography is a fact about a PERSON, not about a server
//!
//! **Every registered server is asked, and the shelves are the merge.** This page used to fetch
//! from exactly one — the machine whose credit row opened it — and present the result as the
//! person's filmography. With a share registered that is a lie in both directions: a person opened
//! from your own library hid everything of theirs on a friend's server, and (once a search screen
//! can surface the same actor from a borrowed source) *which server you happened to arrive
//! through* decided what their page showed. It is the same page either way now.
//!
//! **The join key is the `tagKey`, and only the `tagKey`** ([`resolve_local`]). [`Person::guid`] is
//! plex.tv's global person id; the numeric `personId` in [`Person::key`] is server-local exactly
//! like a `ratingKey` (docs/shared-servers.md §1) and addresses a *different person* on the next
//! machine. So every server but the origin costs one extra round trip — a `/hubs/search` for the
//! name, whose `actor` hub is joined back on the guid — before its filmography can be asked for at
//! all.
//!
//! ## The fetches, and their index space
//!
//! Three requests **per source**, plus one that is not per-source at all:
//!
//! * **[`K_RESOLVE`]** — `GET /hubs/search?query=<name>` on that server, joined on the `tagKey` to
//!   its own local `personId`. Skipped entirely for the ORIGIN server, which handed us that id in
//!   the credit row. A server that has never heard of them answers here, and that is an ANSWER: it
//!   contributes nothing, fails nobody, and shows no error.
//! * **[`K_MEDIA`]** — the shelves: `GET /library/people/{personId}/media` returns everything the
//!   person appears in across EVERY library section at once, no per-section `?actor=<id>` sweep.
//!   The rows are split into the Movies / Shows shelves by **each row's own `type`**; the
//!   container's `viewGroup` is a trap (it read `"movie"` on a response whose only row was a
//!   `show`). Each row is stamped with the source's `ServerId`, which is what makes a card open on
//!   the machine it came from.
//! * **[`K_ROLES`]** — the character names, one batched `GET /library/metadata/{that source's shelf
//!   keys}` ([`crate::plex::Client::metadata_many`]). It exists because the shelf listing does NOT
//!   carry them: its rows have a `Role[]` whose entries hold only `tag`. It is the ONE fetch here
//!   that DEPENDS on another — it can only be addressed once that source's shelves have landed, and
//!   [`address`] expresses that by keying it on that source's shelf keys, which are empty until
//!   then. Per source, because a `ratingKey` csv means nothing on another machine, and filtered on
//!   the SOURCE's own local id, not the origin's.
//! * **[`F_PROFILE`]** — the biography, from plex.tv: `GET
//!   discover.provider.plex.tv/library/people/{tagKey}` over the TLS+DNS `net.rs` path (see
//!   `plex/discover.rs` for the wire facts). **Deliberately still single.** It is the one fetch that
//!   is already global — plex.tv answers about the person, not about anybody's library — so fanning
//!   it out would be the same request N times for one answer.
//!
//! The mailboxes, single-flight flags and retry countdowns are all arrays over ONE flat index
//! ([`fx`]/[`un_fx`]), keyed on the registry **slot** rather than on a position in [`Person::srcs`]:
//! a slot is stable for the life of the process, so a landing can never be applied to a different
//! server because the source list moved under it.
//!
//! **The header still MOUNTS on what it was handed.** `Role[]` already carries the name and the
//! headshot (`plex::Tag`), so the page draws instantly and the plex.tv fields fade in under the
//! name when they land. That ordering is the whole degrade strategy: the biography request can
//! fail, or the person can be one plex.tv has never heard of, and the page must then read as
//! *finished* at portrait + name — never as a row of fields that failed to load. Nothing in the
//! header is a placeholder; each line is drawn only when it has content.
//!
//! **The three id spaces are not interchangeable.** [`Person::key`] is the ORIGIN server's local
//! `personId` (the numeric `Tag::id`, or its `tagKey` when that server omitted the number) and
//! addresses `/media` **on `Person::sid` only**; [`Src::local`] is the same thing for every other
//! source, resolved rather than given; [`Person::guid`] is the `tagKey` and is the ONLY thing
//! plex.tv answers to — the numeric id 404s there (`"Invalid value provided for metadataId!"`).
//! Keep all three; do not collapse them.
use crate::plex::{ServerId, Tag};
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

/// Items asked of each hub in the cross-server resolve. `/hubs/search`'s `limit` is per-hub and
/// defaults to **3** (`docs/plex-openapi.json`), which is too few to rely on: a search for a
/// surname returns every actor who shares it, and the one we are joining on has to be among them.
/// It is not a display list — nothing is drawn from this response but one id.
const RESOLVE_LIMIT: i64 = 12;

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
    /// about the person. On the MERGED shelf it is the sum across every source, for the same
    /// reason — the count is a fact about the person, not about the row that fitted.
    pub(crate) total: usize,
    /// The character each tile is this person's credit for (`"Wallace (voice)"`), PARALLEL to
    /// `items` — index `i` captions tile `i`, `""` where the server named no part. A parallel vector
    /// rather than a field on `PmsMovie`, because a character name is a fact about a *person's
    /// credit in* an item, not about the item: the same movie on a home shelf has no role.
    pub(crate) roles: Vec<String>,
}

/// The shelves a person's page has, in flow order. `ui/person.rs` indexes everything by this.
pub(crate) const NSHELF: usize = 2;

/// One SOURCE of the person's filmography — a registered server, its own local id for this person,
/// what it answered with, and the three per-server flags [`address`] reads. Main-thread only, like
/// the `Person` that owns it.
///
/// A source is *settled* ([`Src::settled`]) once it has finished contributing, whichever way: its
/// media landed, or its resolve came back "never heard of them". Both are answers.
struct Src {
    /// the registry slot every fetch for this source is issued through, and the id stamped onto
    /// every row it parses
    sid: ServerId,
    /// **This server's own `personId`** for this person. `None` after a settled resolve means it
    /// has no record of them, which is why the media fetch is gated on `resolved` as well: `None`
    /// before the resolve answers means "not known yet", and the two must not be confused.
    local: Option<String>,
    /// the resolve has ANSWERED — found or not. The ORIGIN starts `true` (the credit row handed us
    /// its id) and so does a source that cannot be asked at all (see [`sources`]).
    resolved: bool,
    /// this source's own contribution, before the merge
    shelves: [Shelf; NSHELF],
    /// its `/media` fetch has landed successfully at least once
    landed: bool,
    /// its batched character-name read has ANSWERED for ITS CURRENT shelves. Cleared by every media
    /// landing (the keys it was addressed to are gone), which is what re-asks for the new list.
    roled: bool,
}

impl Src {
    /// Every key this source's shelves hold, in flow order — and its roles fetch's cache key: empty
    /// until its media lands (so nothing is asked too early) and it CHANGES with them (so a
    /// re-landing re-asks). At most `NSHELF * SHELF_MAX` keys.
    ///
    /// PER SOURCE, because the batch it addresses is a `/library/metadata/{csv}` of server-local
    /// `ratingKey`s: hand another machine this list and it answers about different items entirely.
    fn shelf_keys(&self) -> Vec<&str> {
        self.shelves
            .iter()
            .flat_map(|s| s.items.iter())
            .map(|m| m.rk.as_str())
            .filter(|rk| !rk.is_empty())
            .collect()
    }

    /// Has this source finished contributing? Either its filmography landed, or it answered that it
    /// has never heard of them. Distinct from `landed` because "nothing from this server" is an
    /// answer the page is allowed to stop waiting on — see [`resettle`].
    fn settled(&self) -> bool {
        self.landed || (self.resolved && self.local.is_none())
    }

    /// Did this source actually put a tile on the page?
    ///
    /// **Not the same question as `landed`,** and the difference is a visible flash. A `/media`
    /// response can succeed and still yield nothing: [`split_by_type`] keeps only `movie` and `show`
    /// rows, so a person credited on this server in episodes alone lands *successfully* with two
    /// empty shelves. Reading that as "the page has something to show" draws the empty read-out over
    /// a share that is still resolving, and its films then pop in on top of the words.
    fn has_content(&self) -> bool {
        self.shelves.iter().any(|sh| !sh.items.is_empty())
    }
}

/// One person's page: the header handed in by the cast row, the plex.tv biography fields, and the
/// MERGED Movies / Shows shelves every source contributed to.
pub(crate) struct Person {
    /// The ORIGIN server — the one whose credit row opened this page, and the only one
    /// [`Person::key`] means anything on. A personId is server-local exactly like a ratingKey
    /// (docs/shared-servers.md §1), so `key` alone names a person on no machine in particular once
    /// a share is registered; the pair is the identity, compared through
    /// [`crate::plex::same_item`]. `guid` needs no such scoping: it is plex.tv's, and global.
    ///
    /// It is NOT "the server this page reads" any more — that is every entry of [`Person::srcs`].
    /// What it still decides is the header (`Art::Person` fetches the headshot from it) and the
    /// trail's person identity.
    pub(crate) sid: ServerId,
    /// the ORIGIN server's local `personId` (numeric tag id, or the guid when that server sent no
    /// number) — addresses `/library/people/{key}/media` on [`Person::sid`] and nowhere else
    pub(crate) key: String,
    /// the `tagKey` guid — plex.tv's global person id. The ONLY thing
    /// `discover.provider.plex.tv` answers to (module docs), and the ONLY key two SERVERS can
    /// agree on, which is what makes [`resolve_local`] possible. Empty when the credit row carried
    /// none, which costs both the biography and every cross-server join.
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
    ///
    /// The MERGE of every source's own shelves ([`merge_shelves`]), rebuilt by [`resettle`] on each
    /// landing. Never written directly by a landing: a source owns its rows, and this is a
    /// projection of them.
    pub(crate) shelves: [Shelf; NSHELF],
    /// Every server this page asks, in [`sources`] order. Read once at [`open`].
    srcs: Vec<Src>,
    /// Something is worth showing, so the spinner can stop. The `browse.rs` `total < 0` sentinel in
    /// bool form, folded across the sources by [`resettle`], and the fold is **content, then
    /// consensus**:
    ///
    /// * ANY source having put a TILE on the page settles it — a share whose machine is asleep must
    ///   not hold shelves we already have off the screen;
    /// * with nothing anywhere, it takes EVERY source having answered.
    ///
    /// Both halves stop the same flash from a different side: a server that says "never heard of
    /// them" instantly, and one that answers with a successful but empty filmography ([`Src::
    /// has_content`] — an episodes-only credit lands exactly like that), must neither of them draw
    /// "nothing in your libraries" over a fetch still out on the server that does have them.
    pub(crate) landed: bool,
    /// the plex.tv profile fetch has ANSWERED at least once — including the answer "no such
    /// person", which arrives as an all-empty profile. Its only job is to stop the retry; the page
    /// never renders a "loading" state for the header, because a header that is complete without a
    /// biography must not flicker a spinner into a space it will never fill.
    pub(crate) profiled: bool,
}

impl Person {
    /// The tiles of shelf `kind`.
    pub(crate) fn shelf(&self, kind: usize) -> &[PmsMovie] {
        &self.shelves[kind].items
    }
    /// The character tile `i` of shelf `kind` credits this person as — `""` until the batched read
    /// lands, and `""` forever for a credit the server names no part for (every CREW credit, since
    /// a director appears in the item's `Director[]`, not `Role[]`). Bounds-checked rather than
    /// indexed: the roles vector is filled a frame or more after the shelf it captions.
    pub(crate) fn role(&self, kind: usize, i: usize) -> &str {
        self.shelves[kind].roles.get(i).map(String::as_str).unwrap_or("")
    }
    /// How many items of kind `kind` every source really had — what a heading prints (see
    /// [`Shelf::total`]).
    pub(crate) fn total(&self, kind: usize) -> usize {
        self.shelves[kind].total
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

/// The three requests this page makes OF ONE SERVER. Adding one is a constant, one arm in
/// [`address`] and one in [`maybe_spawn`]; [`supersede`] picks it up with no edit at all.
const K_RESOLVE: usize = 0;
const K_MEDIA: usize = 1;
const K_ROLES: usize = 2;
const NKIND: usize = 3;

/// The ONE fetch that is not per-server: plex.tv's biography, which is already global (module doc).
/// It sits past every per-source mailbox, which is what keeps [`un_fx`] total.
const F_PROFILE: usize = crate::plex::MAX_SERVERS * NKIND;
/// Mailboxes, single-flight flags and retry countdowns, all over this one index space.
const NFETCH: usize = F_PROFILE + 1;

/// Mailbox index of source `sid`'s fetch of kind `k`, `None` for a `ServerId` that names no slot.
///
/// **Keyed on the registry SLOT, never on a position in [`Person::srcs`].** A slot is stable for
/// the life of the process, so a worker's landing is applied to the server it asked even if the
/// source list is rebuilt under it; an index into a `Vec` would silently start meaning a different
/// machine. [`crate::plex::MAX_SERVERS`] is the registry's own ceiling, imported rather than
/// restated — a second 16 here would go out of bounds the day that one moves.
fn fx(sid: ServerId, k: usize) -> Option<usize> {
    let slot = sid.is_set().then(|| sid.raw() as usize).filter(|&s| s < crate::plex::MAX_SERVERS)?;
    Some(slot * NKIND + k)
}

/// …and back: which source and which kind mailbox `i` belongs to. `None` for [`F_PROFILE`], which
/// belongs to no server.
fn un_fx(i: usize) -> Option<(ServerId, usize)> {
    (i < F_PROFILE).then(|| (ServerId::from_raw((i / NKIND) as u16), i % NKIND))
}

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
static IN_FLIGHT: [AtomicBool; NFETCH] = [const { AtomicBool::new(false) }; NFETCH];
/// Frames left before fetch `i` may spawn again after a FAILED attempt (main-thread; [`pump`]
/// decrements). Stops a fast-failing network from spawning a worker every frame. PER FETCH AND PER
/// SOURCE on purpose, which is the third of `pms.rs`'s three multi-source lessons: a plex.tv
/// biography that cannot be reached (the TV is on a LAN with no internet — the case this app is
/// built to keep working) must not hold the shelves off for two seconds a go, and a share that
/// takes eight seconds to time out must not put the server that IS answering on its ladder.
static mut RETRY_CD: [u32; NFETCH] = [0; NFETCH];
/// ~2s at 60fps — the same backoff `browse.rs` uses for a failed page.
const RETRY_FRAMES: u32 = 120;

/// What a finished fetch delivers. Every arm carries an `Option`, and in ALL of them the `None`
/// means the same thing: the fetch FAILED (transport, parse, or a panicking worker) and must be
/// retried. It has to stay distinguishable from a successful answer that happens to be empty —
/// installing empty shelves on a failure is the "one wifi hiccup blanked a populated grid" bug
/// `browse.rs` carries its `total < 0` sentinel for; the profile has the same trap in a subtler
/// form (plex.tv answers 200 with an empty container for a person it has never heard of, which is
/// an ANSWER); and the resolve has it in the sharpest form of all, since "this server has no record
/// of them" is the NORMAL answer from a share and must never read as a fault.
enum Landing {
    /// One source's own `personId`. `Some("")` = answered, and this server has never heard of them.
    Resolve(Option<String>),
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
static SLOT: [Mutex<Option<Mail>>; NFETCH] = [const { Mutex::new(None) }; NFETCH];

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
/// those four move together — and it walks the WHOLE index space, not just the sources currently
/// held, so a slot that has left the roster cannot leave a latched flag behind it.
fn supersede() {
    GEN.fetch_add(1, Ordering::SeqCst);
    for i in 0..NFETCH {
        *SLOT[i].lock().unwrap_or_else(|e| e.into_inner()) = None;
        IN_FLIGHT[i].store(false, Ordering::SeqCst);
        unsafe { RETRY_CD[i] = 0 };
    }
}

// ---- the sources, and the merge ---------------------------------------------------------------

/// The servers this page asks: **every registered one**, in registry order.
///
/// Registry order, not "the one you arrived through first", is the point of the whole unit — the
/// page has to be the same page whichever credit row opened it. The ORIGIN is force-included when
/// the registry has never heard of it, which covers a host test and a boot that never reached
/// plex.tv; a `ServerId` that names no slot ([`ServerId::UNSET`]) is not a source at all, since
/// there is nothing to dial and no mailbox to land in.
///
/// **Read ONCE, here, on the main thread.** The roster only moves on a login or a profile switch
/// and both call [`reset`]; a person page lives for seconds. Re-reading it per frame would buy a
/// share appearing mid-page at the cost of `pms.rs`'s whole `sync_roster` dance.
fn sources(origin: ServerId, key: &str, name: &str) -> Vec<Src> {
    let mut out: Vec<Src> = Vec::new();
    for sid in crate::plex::server_ids().chain(std::iter::once(origin)) {
        if fx(sid, K_RESOLVE).is_none() || out.iter().any(|s| s.sid == sid) {
            continue;
        }
        let is_origin = sid == origin;
        out.push(Src {
            sid,
            // The origin needs no resolve: the credit row handed us its own `personId`, which is
            // both the round trip saved and the only id we could ever be sure of.
            local: is_origin.then(|| key.to_string()).filter(|k| !k.is_empty()),
            // …and a source we cannot even ASK is settled at birth. The cross-server resolve is a
            // NAME query, so a credit row that carried no name can never be joined on another
            // machine; contributing nothing is an answer, not a fetch to retry forever.
            resolved: is_origin || name.is_empty(),
            shelves: Default::default(),
            landed: false,
            roled: false,
        });
    }
    out
}

/// This server's OWN `personId` for the person `(name, guid)`, read out of its `/hubs/search`
/// answer. `None` means it has never heard of them — an ANSWER, not a failure.
///
/// **The guid is the join, and [`Tag::is_person`] is the comparison** — the same one the credit
/// rows already use, not a second one written here. A `tagKey` is plex.tv's global person id and
/// therefore the one key two machines can agree on; the numeric `personId` beside it is dense from
/// 1 on every server and names a different person on each.
///
/// The name is a FALLBACK, and only where there is no guid to join on: either the entry carries no
/// `tagKey`, or we were opened without one. **Nothing looser, and never against a guid we can see
/// and disagree with** — a search for "John Williams" returns both of them, and a name match there
/// would put the other man's films on this page. Showing fewer films is the lesser failure and it
/// is the one this rule picks.
///
/// Both of `search::Kind::Person`'s hubs are scanned, so a page opened from a DIRECTOR credit
/// resolves too — **and they are scanned in ITS order, `actor` before `director`, not in the order
/// the server sent them.** On PMS an actor tag and a director tag for one person are different rows
/// with different `id`s and the SAME `tagKey` (see [`Tag::filter`], which carries the role `id`
/// alone does not), and `search.rs` records that `/hubs/search` reorders its hubs per query. Filter
/// `mc.hub` and an actor-director whose `director` hub happened to come back first resolves to the
/// directing tag — after which `/library/people/{id}/media` returns only what they directed, and
/// every acting credit that server holds is silently gone. So the candidate list is built by walking
/// `want`, and the server's order decides nothing.
///
/// Hubs are matched on either `hubIdentifier` or `type` because `/hubs/search` sends the same token
/// in both (`docs/plex-openapi.json`'s own example), and `Hub::directory` is keyed by that word.
pub(crate) fn resolve_local(mc: &crate::plex::MediaContainer, name: &str, guid: &str) -> Option<String> {
    let mut people: Vec<&Tag> = Vec::new();
    for h in crate::search::Kind::Person.hubs() {
        for hub in mc.hub.iter().filter(|x| x.hub_identifier == *h || x.kind == *h) {
            people.extend(hub.directory.iter());
        }
    }
    // the guid join first, across EVERY person hub — a `director`-hub guid match still beats a
    // name match in `actor`, because a guid is proof and a name is a guess
    people
        .iter()
        .copied()
        .find(|t| t.is_person("", guid))
        .or_else(|| {
            people.iter().copied().find(|t| {
                !name.is_empty() && t.tag == name && (t.tag_key.is_empty() || guid.is_empty())
            })
        })
        .map(local_id)
        .filter(|id| !id.is_empty())
}

/// The `personId` a tag row addresses on ITS OWN server: the numeric id, or the `tagKey` when the
/// server sent no number — the same rule [`Person::key`] documents for the origin.
fn local_id(t: &Tag) -> String {
    if t.id != 0 {
        t.id.to_string()
    } else {
        t.tag_key.clone()
    }
}

/// Merge every source's shelves into the pair the screen draws. PURE — no statics, no I/O — which
/// is what makes the order, the budget and the parallel captions gradeable on the host.
///
/// Sources are taken in [`sources`] order and each keeps its own rows: [`split_by_type`] stamped
/// every `PmsMovie` with the `ServerId` it came from, so a card opens on the right machine however
/// far down the merged shelf it lands (`ui::person`'s activation reads `mm.sid`, and `Art::Poster`
/// resolves the artwork on that server too).
///
/// The per-shelf budget is divided by [`crate::pms::allot`] rather than spent first-come, for the
/// reason that function documents: a prolific actor's 24 films on the first source would otherwise
/// fill the row and leave every share behind it nothing — which is this unit's own bug, re-created
/// one level down. The heading's count is the sum of the REAL totals, not of what fitted.
///
/// **Nothing is deduplicated, deliberately.** Two servers that both hold a film contribute two
/// cards. The only identity that could join them is the portable `Metadata::guid`, which a
/// `PmsMovie` does not carry — and a title+year join would merge two different films *and* miss the
/// case docs/shared-servers.md measured on this very pair of servers, where the share's copy is
/// titled in another language. A visible duplicate is honest; a wrong merge hides a film.
fn merge_shelves(srcs: &[Src]) -> [Shelf; NSHELF] {
    let mut out: [Shelf; NSHELF] = Default::default();
    for (kind, sh_out) in out.iter_mut().enumerate() {
        let want: Vec<usize> = srcs.iter().map(|s| s.shelves[kind].items.len()).collect();
        let take = crate::pms::allot(SHELF_MAX, &want);
        for (i, s) in srcs.iter().enumerate() {
            let sh = &s.shelves[kind];
            sh_out.total += sh.total;
            for (j, m) in sh.items.iter().take(take[i]).enumerate() {
                sh_out.items.push(m.clone());
                // captions ride WITH their items, so the merged pair stays parallel even while one
                // source's roles batch is still out — `""` until it lands, exactly as `Person::role`
                // already reports for a credit with no named part
                sh_out.roles.push(sh.roles.get(j).cloned().unwrap_or_default());
            }
        }
    }
    out
}

/// Re-derive everything that is a function of the SOURCES: the merged shelves the screen draws, and
/// whether the page has an answer to show. The ONE place those two move, so no landing can update
/// one without the other.
fn resettle(p: &mut Person) {
    p.shelves = merge_shelves(&p.srcs);
    p.landed = p.srcs.iter().any(Src::has_content) || p.srcs.iter().all(Src::settled);
}

// ---- public surface --------------------------------------------------------------------------

/// Open the page for the person the ORIGIN server `sid` knows as `key`, with the header the caller
/// already has — the cast row's name + headshot. MAIN THREAD, NON-BLOCKING: nothing is fetched
/// here, [`pump`] spawns every request on the next frame, so the page mounts on its header
/// immediately and fills in around it.
///
/// `sid` is the server whose credit row this is, captured by the caller. It is where `key` means
/// something and where the headshot comes from — but it is NOT the only server read: [`sources`]
/// takes the whole roster, and every other entry resolves its own id from `guid` first.
pub(crate) fn open(sid: ServerId, key: &str, guid: &str, name: &str, thumb: &str) {
    supersede();
    let srcs = sources(sid, key, name);
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
            srcs,
            landed: false,
            profiled: false,
        });
        // A page with NO source at all has already answered — see `resettle`'s fold. Deriving it
        // here rather than hardcoding `landed: false` is what keeps that rule in one place.
        if let Some(p) = (*addr_of_mut!(CURRENT)).as_mut() {
            resettle(p);
        }
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
/// It is also what makes [`sources`]' read-once safe: the roster only changes on the paths that
/// call this.
pub(crate) fn reset() {
    close();
}

/// True while the open person has nothing to show yet — the page's spinner state. A failed fetch
/// stays "loading" on purpose: [`maybe_spawn`] retries after the backoff, so the spinner is telling
/// the truth about what the page is doing. See [`Person::landed`] for how the sources fold into it.
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
            // …and EVERY landing repaints, the failures included. `ui::idle` gates the whole frame
            // on a settled screen, so without this a shelf that arrives (or a spinner that should
            // stop) waits for the next keypress to become visible. This page had no such call at
            // all — the omission `search.rs`'s module doc warns the next store about.
            crate::ui::idle::invalidate();
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

/// Which server, and which entry of [`Person::srcs`], mailbox `i` belongs to. `None` when the page
/// does not hold that slot — a worker for a roster that has since been rebuilt, dropped whole and
/// WITHOUT the failure backoff, because nothing failed.
fn src_of(p: &Person, i: usize) -> Option<(ServerId, usize)> {
    let (sid, _) = un_fx(i)?;
    Some((sid, p.srcs.iter().position(|s| s.sid == sid)?))
}

/// Install ONE landing on the open person. Returns whether anything actually changed.
fn apply(i: usize, what: Landing) -> bool {
    let Some(p) = (unsafe { (*addr_of_mut!(CURRENT)).as_mut() }) else { return false };
    match what {
        // FAILED: leave the store exactly as it was and back off before retrying. One arm for all
        // four fetch kinds, because the countdown is per MAILBOX — a dead share backs off its own
        // three and nothing else's.
        Landing::Resolve(None) | Landing::Media(None) | Landing::Profile(None) | Landing::Roles(None) => {
            unsafe { RETRY_CD[i] = RETRY_FRAMES };
            false
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
                p.guid,
                p.roles,
                !p.born.is_empty(),
                !p.died.is_empty(),
                p.bio.len()
            ));
            true
        }
        Landing::Resolve(Some(id)) => {
            let Some((sid, si)) = src_of(p, i) else { return false };
            {
                let s = &mut p.srcs[si];
                s.local = (!id.is_empty()).then(|| id.clone());
                s.resolved = true;
            }
            // The SLOT, never the handle or the address: a plex.tv username is the friend's, and
            // the event log is what users send us.
            crate::log(&format!(
                "person: source {} resolve '{}' -> {}",
                sid.raw(),
                p.name,
                if id.is_empty() { "no record here".to_string() } else { format!("id={id}") }
            ));
            // …and "no record here" can be the last answer the page was waiting on, so the fold
            // has to run even though no shelf moved.
            resettle(p);
            true
        }
        Landing::Media(Some(shelves)) => {
            let Some((sid, si)) = src_of(p, i) else { return false };
            {
                let s = &mut p.srcs[si];
                crate::log(&format!(
                    "person: source {} '{}' movies={}/{} shows={}/{}",
                    sid.raw(),
                    p.name,
                    shelves[0].items.len(),
                    shelves[0].total,
                    shelves[1].items.len(),
                    shelves[1].total
                ));
                // Assigning the whole array is what carries the invariant: the captions described
                // the OLD items, so they go WITH them (a `Shelf` lands with `roles` empty) and
                // `roled` re-asks. As three loose fields this was three things to remember.
                s.shelves = shelves;
                s.landed = true;
                s.roled = false;
            }
            resettle(p);
            true
        }
        Landing::Roles(Some(RolesLanding { keys, pairs })) => {
            let Some((_, si)) = src_of(p, i) else { return false };
            {
                let s = &mut p.srcs[si];
                // A landing addressed to a shelf list that has since been REPLACED must not settle
                // the current one: the IN_FLIGHT doc allows a brief duplicate same-generation media
                // worker, so a second media landing can swap this source's shelves while a roles
                // batch for the first list is in flight. Refusing it (without arming the failure
                // backoff — nothing failed) leaves `roled` false, and `address` simply re-asks with
                // the keys that are now true.
                if keys != s.shelf_keys() {
                    return false;
                }
                // match by ratingKey, never by index: the pairs arrive in the batched response's
                // order, which the server does keep, but nothing about THIS store should depend on
                // it. The keys are this source's own, so no cross-server collision can reach here.
                for sh in s.shelves.iter_mut() {
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
                s.roled = true;
            }
            resettle(p);
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
/// `None` covers both "already answered" and **"not actionable"**, and the second is now the common
/// case rather than the exception: a person with no `tagKey` never asks plex.tv anything, a source
/// whose resolve came back empty never asks for a filmography, and neither is a failure or may spin
/// a worker every frame. The two per-source dependencies are expressed as keys rather than as
/// ordering machinery — `K_MEDIA` keys on the source's resolved id, `K_ROLES` on its shelf list,
/// and both are absent until the fetch before them has landed.
///
/// Called every frame for the page's whole life, once per mailbox, so it allocates only when it
/// will actually spawn.
fn address(i: usize, p: &Person) -> Option<Vec<String>> {
    if i == F_PROFILE {
        return (!p.profiled && !p.guid.is_empty()).then(|| vec![p.guid.clone()]);
    }
    let (sid, kind) = un_fx(i)?;
    let s = p.srcs.iter().find(|s| s.sid == sid)?;
    match kind {
        // the cross-server join: a NAME query, whose answer `resolve_local` joins back on the guid
        K_RESOLVE if !s.resolved => Some(vec![p.name.clone()]),
        K_MEDIA if s.resolved && !s.landed => s.local.clone().map(|id| vec![id]),
        K_ROLES if s.landed && !s.roled => {
            let keys = s.shelf_keys();
            (!keys.is_empty()).then(|| keys.into_iter().map(str::to_string).collect())
        }
        _ => None,
    }
}

/// One fetch per mailbox at a time, and only while the open person still wants it. Re-entered every
/// frame by [`pump`], which is what makes the failure path self-healing: a refused `spawn_small`
/// (the device's thread ceiling) or a transient network error simply retries after the backoff
/// instead of latching the page on a spinner forever.
fn maybe_spawn(i: usize) {
    if IN_FLIGHT[i].load(Ordering::SeqCst) || unsafe { RETRY_CD[i] > 0 } {
        return;
    }
    let Some(p) = current() else { return };
    let Some(arg) = address(i, p) else { return };
    let gen = GEN.load(Ordering::SeqCst);
    // the person's global id rides along: the resolve joins its search answer on it, and the roles
    // worker matches a credit row by either id space
    let guid = p.guid.clone();
    if i == F_PROFILE {
        IN_FLIGHT[i].store(true, Ordering::SeqCst);
        let spawned = crate::task::spawn_small("person", move || {
            // filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE (None), not
            // as an empty biography
            let prof = catch_unwind(|| fetch_profile(&arg[0])).unwrap_or(None);
            land(i, gen, Landing::Profile(prof));
        });
        if !spawned {
            IN_FLIGHT[i].store(false, Ordering::SeqCst);
        }
        return;
    }
    let Some((sid, kind)) = un_fx(i) else { return };
    // CAPTURE AT THE SPAWN SITE — `pms::kick` states the rule and this store now needs it for the
    // same reason. The worker is handed THIS server's own `&'static Client`, never `client()`: a
    // slot re-pointed mid-request cannot redirect a fetch already out (`plex::servers` leaks each
    // client precisely so the reference stays live), and a worker that asked which server is
    // current would answer for whichever machine the user has since walked onto.
    let Some(c) = crate::plex::client_for(sid) else {
        // a source whose slot holds no client has nothing to contribute right now — back off rather
        // than re-asking every frame; `pump` retries by itself
        unsafe { RETRY_CD[i] = RETRY_FRAMES };
        return;
    };
    // …and the SOURCE's own local id, for the roles worker's credit filter. The origin's `key` is
    // meaningless on any other machine, so filtering with it blanked every borrowed caption while
    // still paying for the batch.
    let local = p.srcs.iter().find(|s| s.sid == sid).and_then(|s| s.local.clone()).unwrap_or_default();
    IN_FLIGHT[i].store(true, Ordering::SeqCst);
    let spawned = crate::task::spawn_small("person", move || {
        // the mailbox is filled OUTSIDE the guard so a panicking fetch still lands — as a FAILURE
        // (None), not as an empty filmography / a source silently written off / no captions
        let what = match kind {
            K_RESOLVE => Landing::Resolve(
                catch_unwind(|| {
                    // sectionId 0 = every section. `opt_int` omits a zero, and scoping would be
                    // wrong anyway: this asks a server we have never addressed before whether it
                    // knows this person AT ALL, so it must see the whole library.
                    let mc = c.search(&arg[0], RESOLVE_LIMIT, 0)?;
                    // `""` is the ANSWER "no record of them here" — see `Landing::Resolve`
                    Some(resolve_local(&mc, &arg[0], &guid).unwrap_or_default())
                })
                .unwrap_or(None),
            ),
            K_MEDIA => Landing::Media(
                catch_unwind(|| {
                    let mc = c.person_media(&arg[0])?;
                    Some(split_by_type(&mc, sid))
                })
                .unwrap_or(None),
            ),
            K_ROLES => Landing::Roles(
                catch_unwind(|| {
                    let keys: Vec<&str> = arg.iter().map(String::as_str).collect();
                    let mc = c.metadata_many(&keys)?;
                    let pairs = roles_from(&mc, &local, &guid);
                    Some(RolesLanding { keys: arg.clone(), pairs })
                })
                .unwrap_or(None),
            ),
            _ => return, // `un_fx` only ever yields 0..NKIND
        };
        land(i, gen, what);
    });
    if !spawned {
        // nothing will ever fill the mailbox, and the flag is cleared only by a take — release it
        // here or this source never fetches again. `maybe_spawn` runs every frame, so this retries
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
/// Which tag IS this person is [`crate::plex::Tag::is_person`]'s job — both id spaces, because the
/// local id is the `tagKey` guid whenever the credit row carried no number. `id` is the id local to
/// **the server this response came from** ([`Src::local`]), never the origin's: they are different
/// numbers for the same person, and filtering a share's response with ours matched nothing while
/// still paying for the batch. Rows with **no part for them** (every CREW credit — a director
/// appears in `Director[]`, not `Role[]`) are simply absent from the result rather than carried as
/// empty pairs, and `apply` reads a missing key as `""`. Naming their department instead would need
/// the crew arrays this batch excludes, for a line the mockup does not ask for.
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
///
/// `sid` is the server the response came from, stamped onto every row — the fact that survives
/// [`merge_shelves`] and lets a card three sources deep open on the right machine.
pub(crate) fn split_by_type(mc: &crate::plex::MediaContainer, sid: ServerId) -> [Shelf; NSHELF] {
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
/// screen's focus/flow tests need neither a server nor the mailbox. Lands them on the FIRST source
/// (minting one for the open person's own server when the registry is empty, which is the state
/// `ui::person`'s tests open in) and then re-derives the merge, so what the screen reads came
/// through the same projection a real landing does.
#[cfg(test)]
pub(crate) fn install_for_test(movies: Vec<PmsMovie>, shows: Vec<PmsMovie>) {
    let Some(p) = (unsafe { (*addr_of_mut!(CURRENT)).as_mut() }) else { return };
    if p.srcs.is_empty() {
        p.srcs.push(Src {
            sid: p.sid,
            local: None,
            resolved: true,
            shelves: Default::default(),
            landed: false,
            roled: false,
        });
    }
    p.srcs[0].shelves = [movies, shows].map(|items| Shelf { total: items.len(), items, roles: Vec::new() });
    p.srcs[0].landed = true;
    p.srcs[0].roled = false;
    resettle(p);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::{Hub, MediaContainer, Metadata};

    /// Slot 0 — the server every single-source test opens on. A real slot, so its mailboxes exist,
    /// but nothing is registered at it: `maybe_spawn` therefore refuses to dial (`client_for` is
    /// `None`) and every test below drives the mailbox by hand.
    const S0: ServerId = ServerId::from_raw(0);
    const S1: ServerId = ServerId::from_raw(1);

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

    /// A movie row on a named server — the pair that has to survive the merge.
    fn movie_on(sid: ServerId, rk: &str) -> PmsMovie {
        PmsMovie { sid, rk: rk.to_string(), ..Default::default() }
    }

    /// One source, already landed with these shelves — the input [`merge_shelves`] takes.
    fn src_with(sid: ServerId, movies: Vec<PmsMovie>, total: usize, roles: Vec<String>) -> Src {
        let mut s = Src {
            sid,
            local: Some("1".into()),
            resolved: true,
            shelves: Default::default(),
            landed: true,
            roled: false,
        };
        s.shelves[0] = Shelf { total, items: movies, roles };
        s
    }

    /// A `/hubs/search` answer carrying one `actor` hub of `Directory[]` rows.
    fn search_answer(hub_kind: &str, tags: Vec<Tag>) -> MediaContainer {
        let mut mc = MediaContainer::default();
        mc.hub = vec![Hub {
            kind: hub_kind.to_string(),
            hub_identifier: hub_kind.to_string(),
            directory: tags,
            ..Default::default()
        }];
        mc
    }

    fn person_tag(name: &str, id: i64, guid: &str) -> Tag {
        Tag { tag: name.into(), id, tag_key: guid.into(), ..Default::default() }
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
        let s = split_by_type(&mc, S0);
        assert_eq!(s[0].items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(s[1].items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["1975"]);
        assert_eq!((s[0].total, s[1].total), (2, 1));
        assert!(s[0].items.iter().all(|m| m.sid == S0), "every row wears the server it came from");
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
        let s = split_by_type(&mc, S0);
        assert_eq!(s[0].items.len(), SHELF_MAX);
        assert_eq!(s[0].total, SHELF_MAX + 5, "the count is the RESPONSE's total, not the tile cap");
        assert!(s[1].items.is_empty(), "an episode/season is not a Show shelf tile");
    }

    // ---- the cross-server join --------------------------------------------------------------

    /// **The rule the whole unit turns on.** A second server's `personId` is a different number for
    /// the same person, so the only thing the two machines can be joined on is the `tagKey` — and
    /// the id read back is the OTHER server's, never ours.
    ///
    /// The namesake in the answer is the case that makes this more than bookkeeping: two actors
    /// share the name, the guid tells them apart, and a join that reached for the name would open
    /// the wrong man's filmography.
    #[test]
    fn a_second_servers_person_id_is_resolved_by_tag_key_not_by_name_or_number() {
        let mc = search_answer(
            "actor",
            vec![
                person_tag("John Williams", 4102, "5d776b1e880197001ec93f4a"), // the namesake
                person_tag("John Williams", 918, "5d77683d880197001ec9053c"),  // ours, elsewhere
            ],
        );
        // ours is `161` on the origin and `918` here — the number that comes back is THIS server's
        assert_eq!(
            resolve_local(&mc, "John Williams", "5d77683d880197001ec9053c"),
            Some("918".to_string())
        );
        // and the namesake resolves to their own id, never to ours
        assert_eq!(
            resolve_local(&mc, "John Williams", "5d776b1e880197001ec93f4a"),
            Some("4102".to_string())
        );
        // a person this server has never heard of is an ANSWER of None, not a match on the name
        assert_eq!(resolve_local(&mc, "John Williams", "0000000000000000000000ff"), None);
        // …and so is an answer with no person hub at all
        assert_eq!(
            resolve_local(&search_answer("movie", Vec::new()), "John Williams", "5d77683d880197001ec9053c"),
            None
        );
    }

    /// The name fallback exists only where there is **no guid to join on** — an entry the server
    /// sent no `tagKey` for, or a page opened without one. It must never fire against a guid we can
    /// see and disagree with: that is the namesake case above, and a page of the wrong person's
    /// films is worse than a page with fewer.
    #[test]
    fn the_name_fallback_is_exact_and_only_where_no_tag_key_can_decide() {
        // the server named no guid for its entry: the exact name is the only join there is
        let no_key = search_answer("actor", vec![person_tag("Idina Menzel", 77, "")]);
        assert_eq!(resolve_local(&no_key, "Idina Menzel", "5d77682a"), Some("77".to_string()));
        // …and it is EXACT: nothing looser may reach another person's filmography
        assert_eq!(resolve_local(&no_key, "Idina Menze", "5d77682a"), None);
        assert_eq!(resolve_local(&no_key, "idina menzel", "5d77682a"), None);
        assert_eq!(resolve_local(&no_key, "", "5d77682a"), None, "an empty name matches nothing");

        // the entry DOES carry a guid, and it is not ours — the name must not override it
        let other = search_answer("actor", vec![person_tag("Idina Menzel", 88, "5d776999")]);
        assert_eq!(resolve_local(&other, "Idina Menzel", "5d77682a"), None);
        // …but with no guid on OUR side there is nothing to disagree with, so the name joins
        assert_eq!(resolve_local(&other, "Idina Menzel", ""), Some("88".to_string()));

        // an entry with neither number nor tagKey addresses nothing and must not resolve to ""
        let empty = search_answer("actor", vec![person_tag("Idina Menzel", 0, "")]);
        assert_eq!(resolve_local(&empty, "Idina Menzel", ""), None);
        // a server that sent no number falls back to the tagKey, exactly as `Person::key` does
        let key_only = search_answer("actor", vec![person_tag("Idina Menzel", 0, "5d77682a")]);
        assert_eq!(resolve_local(&key_only, "Idina Menzel", "5d77682a"), Some("5d77682a".to_string()));

        // a DIRECTOR credit resolves too — both of `search::Kind::Person`'s hubs are scanned, so a
        // page opened from a crew row is not silently unresolvable on every other server
        let crew = search_answer("director", vec![person_tag("Nick Park", 459, "5d776826")]);
        assert_eq!(resolve_local(&crew, "Nick Park", "5d776826"), Some("459".to_string()));
    }

    /// An actor-director carries TWO tag rows on one server — same `tagKey`, different `id`, one per
    /// department — and `/hubs/search` reorders its hubs per query (`search.rs`'s own measurement).
    /// So the scan order has to be OURS, `actor` first, not the server's: take the director row and
    /// `/library/people/{id}/media` answers with only what they directed, silently dropping every
    /// acting credit that server holds.
    #[test]
    fn an_actor_director_keeps_their_acting_id_whatever_order_the_server_sent_its_hubs() {
        let guid = "5d77682b7a53e9001e73f2d1";
        let mut mc = MediaContainer::default();
        // the server put its DIRECTOR hub first, which it is free to do
        mc.hub = vec![
            Hub {
                kind: "director".into(),
                hub_identifier: "director".into(),
                directory: vec![person_tag("Clint Eastwood", 2201, guid)],
                ..Default::default()
            },
            Hub {
                kind: "actor".into(),
                hub_identifier: "actor".into(),
                directory: vec![person_tag("Clint Eastwood", 1104, guid)],
                ..Default::default()
            },
        ];
        assert_eq!(
            resolve_local(&mc, "Clint Eastwood", guid),
            Some("1104".to_string()),
            "the acting id must win — the server's hub order decides nothing"
        );
        // …and with no actor row at all, the director one is still better than nothing
        mc.hub.truncate(1);
        assert_eq!(resolve_local(&mc, "Clint Eastwood", guid), Some("2201".to_string()));
    }

    // ---- the merge ---------------------------------------------------------------------------

    /// The merge is the filmography: every source's rows, in source order, each still carrying the
    /// server it came from — which is what makes the card open on the right machine. The heading's
    /// count is the sum of the REAL totals, not of the tiles that fitted, and the captions stay
    /// index-parallel across the seam even while one source's roles batch is still out.
    #[test]
    fn the_merge_keeps_every_sources_rows_their_server_and_their_captions() {
        let srcs = vec![
            src_with(S0, vec![movie_on(S0, "1"), movie_on(S0, "2")], 30, vec!["Elsa".into(), "Nancy".into()]),
            // …the share's roles batch has not landed yet: its captions are absent, not empty
            src_with(S1, vec![movie_on(S1, "1"), movie_on(S1, "9")], 5, Vec::new()),
        ];
        let m = merge_shelves(&srcs);
        assert_eq!(m[0].items.len(), 4, "both sources contribute");
        assert_eq!(
            m[0].items.iter().map(|x| (x.sid, x.rk.as_str())).collect::<Vec<_>>(),
            vec![(S0, "1"), (S0, "2"), (S1, "1"), (S1, "9")],
            "one ratingKey on two servers is two different films — the pair is the identity"
        );
        assert_eq!(m[0].total, 35, "the heading counts what the PERSON has, across every source");
        assert_eq!(m[0].roles.len(), m[0].items.len(), "captions must stay index-parallel");
        assert_eq!(&m[0].roles[..2], &["Elsa".to_string(), "Nancy".to_string()]);
        assert!(m[0].roles[2..].iter().all(String::is_empty), "an un-captioned source reads blank");
        assert!(m[1].items.is_empty(), "neither source had a show");

        // and with nothing anywhere the merge is empty rather than a panic
        assert!(merge_shelves(&[]).iter().all(|s| s.items.is_empty() && s.total == 0));
    }

    /// A greedy source must not spend the whole shelf. Two sources with more films than the row can
    /// hold split it — the bug this unit exists for, re-created one level down: fill the row from
    /// the server you arrived through and the share is invisible again.
    #[test]
    fn one_prolific_source_cannot_starve_another_out_of_the_shelf() {
        let many = |sid: ServerId, n: usize| {
            (0..n).map(|i| movie_on(sid, &format!("{i}"))).collect::<Vec<_>>()
        };
        let srcs = vec![
            src_with(S0, many(S0, SHELF_MAX), SHELF_MAX, Vec::new()),
            src_with(S1, many(S1, SHELF_MAX), SHELF_MAX, Vec::new()),
        ];
        let m = merge_shelves(&srcs);
        assert_eq!(m[0].items.len(), SHELF_MAX, "the row still caps at the spring count");
        assert!(
            m[0].items.iter().any(|x| x.sid == S1),
            "the second source was starved out of the shelf entirely"
        );
        assert_eq!(m[0].total, 2 * SHELF_MAX, "…while the heading still counts everything");
    }

    // ---- the fetch machine -------------------------------------------------------------------

    /// Park EVERY fetch, so a take's RELEASE of a single-flight flag is observable rather than
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

    /// The mailbox of source `sid`'s fetch of kind `k` — every test below drives one by hand.
    fn at(sid: ServerId, k: usize) -> usize {
        fx(sid, k).expect("a real slot")
    }

    /// A late landing for the actor you already left must not repopulate the one you are looking
    /// at. `open` bumps the generation; `pump` drops anything older — and it must still clear the
    /// single-flight flag while doing so, or the NEW person can never fetch.
    #[test]
    fn a_landing_from_the_previous_person_is_discarded_but_still_releases_the_fetch() {
        let _serial = crate::testlock::serial();
        open(S0, "161", "5d776", "Idina Menzel", "");
        let stale = GEN.load(Ordering::SeqCst);
        open(S0, "465", "5d777", "Cynthia Erivo", ""); // supersedes: the fetch above is now obsolete

        IN_FLIGHT[at(S0, K_MEDIA)].store(true, Ordering::SeqCst);
        hold_off();
        land(at(S0, K_MEDIA), stale, media(vec![PmsMovie::default()], Vec::new()));
        assert!(!pump(), "a superseded landing must not publish");

        let p = current().expect("the new person stays open");
        assert_eq!(p.key, "465");
        assert!(p.shelf(0).is_empty(), "the previous actor's filmography leaked in");
        assert!(!p.landed, "a discarded landing must not settle the spinner");
        assert!(
            !IN_FLIGHT[at(S0, K_MEDIA)].load(Ordering::SeqCst),
            "the take must release the single-flight even for a landing it drops"
        );
        close();
    }

    /// A FAILED fetch (None) must leave a populated page alone and schedule a retry — the
    /// "one wifi hiccup blanked a populated grid" regression, in this store's shape.
    #[test]
    fn a_failed_fetch_keeps_the_shelves_and_backs_off_instead_of_publishing_empty() {
        let _serial = crate::testlock::serial();
        open(S0, "161", "5d776", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);
        // seed a populated, landed page the honest way (through the pump)
        land(at(S0, K_MEDIA), gen, media(vec![PmsMovie::default()], Vec::new()));
        hold_off();
        assert!(pump());
        assert_eq!(current().unwrap().shelf(0).len(), 1);

        land(at(S0, K_MEDIA), gen, Landing::Media(None)); // the retry fails
        hold_off();
        assert!(!pump(), "a failure publishes nothing");
        assert_eq!(current().unwrap().shelf(0).len(), 1, "the failure wiped a populated shelf");
        assert_eq!(
            unsafe { RETRY_CD[at(S0, K_MEDIA)] },
            RETRY_FRAMES,
            "a failure must back off before retrying"
        );
        close();
    }

    /// `close`/`reset` drop the mailboxes, and a single-flight flag is cleared ONLY by a successful
    /// take — so they must clear EVERY one (and the backoffs) themselves or the next person opened
    /// never fetches. The `browse.rs` latch, one store over, now once per fetch kind PER SOURCE:
    /// the whole index space is walked, so a slot that has left the roster cannot latch either.
    #[test]
    fn close_clears_every_single_flight_flag_and_retry_backoff() {
        let _serial = crate::testlock::serial();
        open(S0, "161", "5d776", "Idina Menzel", "");
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
    /// disturb the shelves, which come from a different service on a different mailbox. It stays a
    /// SINGLE fetch on purpose: plex.tv answers about the person, not about anybody's library.
    #[test]
    fn a_profile_landing_fills_the_header_without_touching_the_shelves() {
        let _serial = crate::testlock::serial();
        open(S0, "6059", "5d7768268718ba001e311be6", "Peter Sallis", "");
        let gen = GEN.load(Ordering::SeqCst);
        land(at(S0, K_MEDIA), gen, media(vec![PmsMovie::default()], Vec::new()));
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
        open(S0, "6059", "0000000000000000000000ff", "Nobody", "");
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
    /// OR its `tagKey` guid, because the local id is the guid whenever the credit row carried no
    /// number, and an id-only match then blanked every caption while still paying for the batch. A
    /// row where the person is crew only (no `Role[]` entry of theirs) yields `""`, never a
    /// neighbour's character.
    #[test]
    fn roles_from_matches_by_id_or_tag_key_and_leaves_crew_rows_blank() {
        let tag = |name: &str, role: &str, id: i64, guid: &str| Tag {
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
        // ANOTHER SERVER's local id for the same person is a different number, and the guid is what
        // carries the caption across — the reason the worker is handed `Src::local`, not the origin's
        assert_eq!(roles_from(&mc, "918", "5d7768268718ba001e311be6"), want);
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
        open(S0, "6059", "5d7768268718ba001e311be6", "Peter Sallis", "");
        let gen = GEN.load(Ordering::SeqCst);
        let (fm, fr) = (at(S0, K_MEDIA), at(S0, K_ROLES));
        let movie = |rk: &str| movie_on(S0, rk);
        land(fm, gen, media(vec![movie("1971"), movie("2005")], vec![movie("1975")]));
        hold_off();
        assert!(pump());

        // a landing addressed to keys the store no longer holds must be REFUSED — and without the
        // failure backoff: it is stale, not broken, and the re-ask must be free to go at once. The
        // sentinel countdown still parks the spawn (see `hold_off`), but is small enough that a
        // wrongly-armed RETRY_FRAMES would overwrite it visibly.
        let stale = RolesLanding {
            keys: vec!["9999".to_string()],
            pairs: vec![("9999".to_string(), "Nobody".to_string())],
        };
        land(fr, gen, Landing::Roles(Some(stale)));
        hold_off();
        unsafe { RETRY_CD[fr] = 5 };
        assert!(!pump(), "a mis-addressed caption landing published");
        assert_eq!(current().unwrap().role(0, 0), "", "a stale landing captioned the CURRENT list");
        assert_eq!(unsafe { RETRY_CD[fr] }, 4, "staleness is not a failure — no backoff armed (the sentinel just ticked)");

        // pairs arrive REVERSED relative to the shelves — the match is by rk, so it must not matter
        let keys = vec!["1971".to_string(), "2005".to_string(), "1975".to_string()];
        let pairs = vec![
            ("1975".to_string(), "Wallace".to_string()),
            ("2005".to_string(), "Wallace / Hutch (voice)".to_string()),
            ("1971".to_string(), "Wallace (voice)".to_string()),
        ];
        land(fr, gen, Landing::Roles(Some(RolesLanding { keys, pairs })));
        hold_off();
        assert!(pump(), "a caption landing is a change the screen must see");
        let p = current().unwrap();
        assert_eq!(p.role(0, 0), "Wallace (voice)");
        assert_eq!(p.role(0, 1), "Wallace / Hutch (voice)");
        assert_eq!(p.role(1, 0), "Wallace");
        assert_eq!(p.role(0, 99), "", "past-the-end reads are blank, not a panic");

        // a fresh media landing (same person) replaces the shelves — the captions go WITH them
        land(fm, gen, media(vec![movie("2005")], Vec::new()));
        hold_off();
        assert!(pump());
        assert_eq!(current().unwrap().role(0, 0), "", "a caption survived the list it was addressed to");
        // …and the cleared flag is what re-asks: the roles fetch is addressable again
        assert!(
            address(fr, current().unwrap()).is_some(),
            "the reset is what re-asks for the new list's captions"
        );
        close();
    }

    // ---- multi-source --------------------------------------------------------------------------

    /// Empty the registry around a test that needs real slots in it, and hand it back empty — the
    /// discipline `plex::servers`' own tests document: a client left registered at a port that
    /// closed is one another module's pump will dial on a background thread.
    struct FreshRegistry(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for FreshRegistry {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }
    fn fresh_registry() -> FreshRegistry {
        let g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        FreshRegistry(g)
    }

    /// **The bug, in one test.** The page is opened from ONE server's credit row and must ask every
    /// registered one: the origin with the id it was handed, each share through a resolve first.
    #[test]
    fn every_registered_server_is_a_source_and_only_the_origin_skips_the_resolve() {
        let _g = fresh_registry();
        // `register_for_test`, not the public `register`: the latter resolves the device id through
        // `session::load`, which mints and PERSISTS a uuid on a host that has no session file.
        let own = crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-person-test");
        let friend = crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-person-test");

        open(friend, "77", "5d77682a", "Idina Menzel", ""); // arrived through the SHARE
        let p = current().unwrap();
        assert_eq!(p.srcs.len(), 2, "both servers are sources however we arrived");

        // the ORIGIN was handed its id and asks for the filmography straight away…
        assert!(address(at(friend, K_RESOLVE), p).is_none(), "the origin must not re-resolve what it gave us");
        assert_eq!(address(at(friend, K_MEDIA), p), Some(vec!["77".to_string()]));
        // …while the other server must find its OWN id first, and cannot ask for media until it has
        assert_eq!(address(at(own, K_RESOLVE), p), Some(vec!["Idina Menzel".to_string()]));
        assert!(address(at(own, K_MEDIA), p).is_none(), "a filmography cannot be asked for by a foreign id");

        // the resolve lands: now, and only now, is that server's own filmography addressable
        let gen = GEN.load(Ordering::SeqCst);
        land(at(own, K_RESOLVE), gen, Landing::Resolve(Some("918".to_string())));
        hold_off();
        assert!(pump());
        assert_eq!(address(at(own, K_MEDIA), current().unwrap()), Some(vec!["918".to_string()]));
        close();
    }

    /// A server that has never heard of them contributes nothing — **and that is an answer.** It
    /// must not fail the other source, must not blank it, must arm no backoff, and must never ask
    /// that server for a filmography it has no id for. The other half is the timing: its instant
    /// "no" must not settle the page into "nothing in your libraries" while the server that DOES
    /// have them is still fetching.
    #[test]
    fn a_source_that_has_never_heard_of_them_contributes_nothing_without_failing_the_others() {
        let _g = fresh_registry();
        let own = crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-person-test");
        let friend = crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-person-test");
        open(own, "161", "5d77682a", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);

        // the share answers first, with nothing
        land(at(friend, K_RESOLVE), gen, Landing::Resolve(Some(String::new())));
        hold_off();
        pump();
        assert!(!current().unwrap().landed, "an empty answer must not settle a page still fetching");
        assert_eq!(unsafe { RETRY_CD[at(friend, K_RESOLVE)] }, RETRY_FRAMES - 1, "an ANSWER is not a failure");
        assert!(
            address(at(friend, K_MEDIA), current().unwrap()).is_none(),
            "a server with no record of them must not be asked for their filmography"
        );

        // …and the server that does have them fills the page by itself
        land(at(own, K_MEDIA), gen, media(vec![movie_on(own, "1971")], Vec::new()));
        hold_off();
        assert!(pump());
        let p = current().unwrap();
        assert_eq!(p.shelf(0).len(), 1);
        assert_eq!(p.shelf(0)[0].sid, own, "the row still names the machine it came from");
        assert!(p.landed, "one source answering is enough to stop the spinner");
        close();
    }

    /// The other direction of the same fold: when NO source has anything, the page may only say so
    /// once every one of them has answered. A share that answers instantly must not draw "nothing
    /// in your libraries" over the fetch still out on the other server.
    #[test]
    fn the_page_says_nothing_only_once_every_source_has_answered() {
        let _g = fresh_registry();
        let own = crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-person-test");
        let friend = crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-person-test");
        open(own, "161", "5d77682a", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);
        assert!(loading(), "a page that has asked nobody anything yet is loading");

        land(at(friend, K_RESOLVE), gen, Landing::Resolve(Some(String::new())));
        hold_off();
        pump();
        assert!(loading(), "one source's 'no' is not the page's answer");

        // the origin answers with an empty (but successful) filmography — now the page HAS an answer
        land(at(own, K_MEDIA), gen, media(Vec::new(), Vec::new()));
        hold_off();
        pump();
        assert!(!loading(), "every source has answered — the page is finished, not still loading");
        assert!(current().unwrap().shelf(0).is_empty());
        close();
    }

    /// The same flash from the other side. A `/media` response can SUCCEED and still put no tile on
    /// the page — `split_by_type` keeps only `movie` and `show` rows, so a person credited here in
    /// episodes alone lands exactly like that. Read as "we have something to show", it draws the
    /// empty read-out over a share still resolving, whose films then pop in on top of the words.
    #[test]
    fn a_successful_but_empty_filmography_is_not_something_to_show() {
        let _g = fresh_registry();
        let own = crate::plex::register_for_test("mach-own", "10.0.0.1", 32400, "tok", "cid-person-test");
        let friend = crate::plex::register_for_test("mach-friend", "10.0.0.2", 32400, "tok", "cid-person-test");
        open(own, "161", "5d77682a", "Idina Menzel", "");
        let gen = GEN.load(Ordering::SeqCst);

        // the origin succeeds with nothing shelvable while the share has not answered at all
        land(at(own, K_MEDIA), gen, media(Vec::new(), Vec::new()));
        hold_off();
        pump();
        assert!(loading(), "a successful EMPTY answer is not content — the share is still out");

        // …and the share then fills the page
        land(at(friend, K_RESOLVE), gen, Landing::Resolve(Some("918".to_string())));
        hold_off();
        pump();
        land(at(friend, K_MEDIA), gen, media(vec![movie_on(friend, "5274")], Vec::new()));
        hold_off();
        assert!(pump());
        assert!(!loading());
        assert_eq!(current().unwrap().shelf(0).len(), 1, "the borrowed film is the whole page");
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
