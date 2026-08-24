//! The SERVER REGISTRY — every PMS this session knows about, which one is CURRENT, and the
//! `&'static Client` handed to ~30 call sites outside this module.
//!
//! This replaces `client.rs`'s `static PLEX: OnceLock<Client>`: one server per process, forever,
//! whose second `install` could only swap a token because its address was frozen by the first.
//! That single fact is what made browsing a SECOND (shared) server impossible — not the UI, not
//! the ops. So the singleton becomes a table, and `client()` becomes "the current entry of it"
//! with its signature untouched, which is why nothing outside `plex/` changed in the commit that
//! introduced this file.
//!
//! **Keyed by `machineIdentifier`.** The server's own permanent id is the only identity that
//! survives it changing address (LAN ↔ remote, DHCP, a relay), so the table keys on it and an
//! address is just where that key currently answers. [`install`] doesn't know the id — its callers
//! (`app.rs`, `auth.rs`) have only a stored session origin — so it registers with an EMPTY id and
//! matches on the origin; a later [`register_origin`] that does know the id adopts that slot
//! instead of adding a second one for the same server.
//!
//! **An address here is an [`Origin`], not a `(host, port)` pair** — scheme included, and parsed
//! from the URL plex.tv advertised rather than rebuilt from the address behind it (`origin.rs` has
//! the reasoning; the one-line version is that a `plex.direct` certificate is issued for the NAME).
//! [`register`] and [`register_with_client_id`] keep the pair-shaped spelling for callers that
//! genuinely hold nothing else, and say `Origin::http` out loud when they do.
//!
//! ## Why an atomic pointer table and not an `RwLock<Vec<Arc<Client>>>`
//!
//! `client()` is a HOT path: `posters::poster_key` calls it three times per key, for every
//! visible art tile, every frame (~25–40 tiles × 60 fps). An `RwLock` there buys nothing and
//! costs an atomic RMW pair per call plus a fairness stall whenever a login writes; an `Arc`
//! clone would be a refcount bump per call on top of changing every call site's type. So a read
//! is: one acquire load of `CURRENT`, one acquire load of a slot pointer, deref. No lock, no
//! refcount, no allocation. Writers (login, profile switch, adding a server) serialize on
//! `WRITE`, which no reader ever touches.
//!
//! **Each slot's `Client` is LEAKED, deliberately.** It is what makes `&'static Client` sound
//! without an `Arc`: the reference handed out at frame N must stay valid even if a re-point
//! lands at frame N+1 while a worker thread is mid-request with it. Re-pointing publishes a
//! NEW leaked `Client` and stores its pointer over the old one, so the worst case for a caller
//! holding the previous reference is one request to the address that server used to be at —
//! never a use-after-free. The leak is bounded by [`MAX_SERVERS`] pointers' worth of small
//! structs for the life of the process (a household has a handful of servers, and registration
//! happens on login / profile switch / server switch, never per frame), so "bounded and never
//! freed" is cheaper than the refcount it replaces.
//!
//! ## Signing out: slots are REVOKED, never freed and never reused
//!
//! Because nothing is freed, a sign-out cannot remove a server from the table — and until
//! [`revoke_all`] existed nothing tried, so the account that signed out stayed in the table with
//! its live per-server tokens and the next account browsed, searched and built Home from BOTH.
//! [`revoke_all`] raises a [`FLOOR`]: every slot below it is dead — invisible to [`ids`], resolved
//! to `None` by [`client_for`], and blanked to an empty token so a `&'static Client` grabbed before
//! the sign-out cannot dial with the old one either. [`revoke_for_profile_switch`] uses the same
//! token blanking without raising the floor: it temporarily deactivates this account's slots, then
//! the newly granted roster reactivates only the machines that profile may use.
//!
//! **A revoked slot number is never handed out again**, not even to the same `machineIdentifier`.
//! [`ServerId`]'s whole contract is that it names one server for the life of the process — it sits
//! in UI state, in routes, in queued jobs — and every per-server store in the app (`search`'s
//! mailboxes, `serverinfo`, `person`'s per-source records) is a flat array indexed by
//! [`ServerId::raw`]. Reusing a number would file the previous account's results under the new
//! account's server, which is the very leak this exists to close. The cost is that sign-out /
//! sign-in cycles consume slots: past [`MAX_SERVERS`] registration is refused and says so in the
//! log, which for a table of 16 and a household of one or two servers is a great many sign-outs
//! inside one process.

use super::client::Client;
use super::origin::Origin;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Slot ceiling. A Plex account's server list is a handful (own + shared); past this, a
/// registration is refused and logged rather than growing a table the hot path indexes.
///
/// `pub` because [`super::serverinfo`] holds one entry PER SERVER in a flat array indexed
/// by [`ServerId::raw`], and a second, independent ceiling there is a silent out-of-bounds
/// waiting to happen: this is the number that decides which ids can ever exist. `crate::person`
/// keys its per-source mailboxes the same way and is OUTSIDE this module tree, which is what
/// moved this from `pub(super)` — a store that guessed its own 16 would go out of bounds the day
/// this number moved.
pub const MAX_SERVERS: usize = 16;

/// A registry slot — a small `Copy` handle that names a server without borrowing it. Stable for
/// the life of the process, so it can sit in UI state, a route, or a queued job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ServerId(u16);

impl ServerId {
    /// The reserved "no server" value — what [`current`] reads before the first install, and
    /// what a `Client` built outside the registry (tests) carries. Never resolves to a client.
    pub const UNSET: ServerId = ServerId(u16::MAX);

    pub fn is_set(self) -> bool {
        self.0 != Self::UNSET.0
    }
    /// The raw slot number, for logging/persistence. Round trips through [`ServerId::from_raw`].
    pub fn raw(self) -> u16 {
        self.0
    }
    /// `const` so a fixed slot can name a server in a `const` — which is what lets the host tests
    /// that grade the identity rules build one without a registry, a socket or the serial lock.
    pub const fn from_raw(v: u16) -> ServerId {
        ServerId(v)
    }
    /// Slot index, `None` for UNSET — the one place the reserved value is turned away.
    fn index(self) -> Option<usize> {
        self.is_set().then_some(self.0 as usize)
    }
}

impl Default for ServerId {
    fn default() -> Self {
        Self::UNSET
    }
}

/// **THE item-identity rule**: do two server-scoped keys name the same item?
///
/// Every `ratingKey`, `librarySectionID`, `Part.key`, `Stream.id` and `personId` Plex issues is a
/// server-local integer dense from 1, so with two servers registered a bare key names an item on
/// *neither* of them in particular. Two servers colliding is therefore the NORMAL case, not an edge
/// one — it is measured (docs/shared-servers.md §2): the share's only section key is `1`, exactly
/// like our own `Movies`, and its ratingKeys start at 1 alongside ours. Anything that stores an
/// item and later looks it up again — the home catalog, the loaded detail, the playing-item store,
/// the BACK trail, a play queue — has to compare the PAIR, and they all compare it here so the
/// answer cannot drift between them.
///
/// [`ServerId::UNSET`] matches only itself, and that is the pre-registry state rather than a
/// wildcard: a row parsed before any server was installed (and every row a host test builds by
/// literal) carries UNSET, so UNSET-vs-UNSET is the single-server app behaving exactly as it did.
/// UNSET vs a real slot is deliberately NOT a match — a "server unknown" row must not be handed to
/// a caller asking about a specific machine, because the whole failure this rule exists to stop is
/// a confident answer about the wrong one. Every such miss degrades safely at its call site (a
/// catalog miss opens the page off-catalog, a playing-item miss costs one PMS fetch), which is the
/// property that makes strict equality the safe default here.
pub fn same_item(a: (ServerId, &str), b: (ServerId, &str)) -> bool {
    a.0 == b.0 && a.1 == b.1
}

/// What the ROSTER says ABOUT a registered server, as opposed to the address a request dials:
/// whose it is and what it is called. Set by whoever ingested the roster (plex.tv
/// `/api/v2/resources`, the `plxnative-servers` dev trigger, or a server naming itself through
/// `Client::friendly_name`), read by the Sources list.
///
/// Deliberately NOT fields on `Client`: a `Client` is re-pointed whenever the address moves, and
/// a re-point must not forget whose server it is. Published by the same leak-and-store the client
/// slots use, so [`facts`] hands out `&'static` on exactly the same terms as [`client_for`].
pub struct ServerFacts {
    /// The MACHINE name — "nas-home". "People in content, machines in settings": this is drawn
    /// only where the grant itself is the subject, i.e. the Sources list's group headers.
    pub name: String,
    /// The owner's plex.tv handle — "friend". **Empty on your own server**, and empty is the
    /// ABSENCE of an owner rather than an anonymous one: every surface that annotates a source
    /// draws nothing at all for it, no separator and no empty run.
    pub handle: String,
    /// This account owns the server (`account::Resource.owned`). Not derivable from an empty
    /// handle: a share whose `sourceTitle` plex.tv did not send is still a share.
    pub owned: bool,
}

/// The table. A null slot is unpopulated; a non-null one is a leaked `Client` that is never
/// freed (see the module doc), which is what makes the `unsafe` deref in [`client_for`] sound.
static SLOTS: [AtomicPtr<Client>; MAX_SERVERS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; MAX_SERVERS];
/// [`ServerFacts`] per slot, published and leaked exactly as `SLOTS` is. A null slot is a server
/// nobody has described yet — which is the honest state on a boot that never reached plex.tv.
static FACTS: [AtomicPtr<ServerFacts>; MAX_SERVERS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; MAX_SERVERS];
/// Slots populated so far — the high-water mark, MONOTONE for the life of the process. A revoked
/// slot keeps its number (see the module doc), so this is not the number of servers: that is the
/// population count of [`ACTIVE`].
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Which slots in the current account window are granted to the active profile. A profile switch
/// can remove one share without retiring every slot number above it; inactive clients stay leaked
/// but resolve to nothing, and their token is blanked before this bit is cleared.
static ACTIVE: AtomicU32 = AtomicU32::new(0);
/// Monotone epoch of the active roster's identity. It moves when a slot appears/disappears or is
/// re-pointed, even when the active COUNT stays the same, so cached fan-out stores can distinguish
/// `{0,1}` from `{0,2}` and can discard work aimed at a superseded origin.
static ROSTER_GEN: AtomicU32 = AtomicU32::new(1);
/// The first slot that still belongs to the signed-in account. Raised to [`COUNT`] by
/// [`revoke_all`]; everything below it is a credential of an account that has left.
///
/// A sign-out raises this past the whole account window. Profile visibility is separate in
/// [`ACTIVE`], because a managed user may be granted only a subset of the owner's servers.
static FLOOR: AtomicUsize = AtomicUsize::new(0);
/// The current/primary server as a raw `ServerId` — what `client()` answers with.
static CURRENT: AtomicU32 = AtomicU32::new(ServerId::UNSET.0 as u32);
/// Writers only (register / re-point / reset). Readers never take it — that is the whole point.
static WRITE: Mutex<()> = Mutex::new(());

// ---- reads: the hot path ----

/// The `Client` for one slot, `None` for [`ServerId::UNSET`], an unpopulated slot, or one
/// [`revoke_all`] has retired.
///
/// The revoked case is the reason this is the ONE door: ~30 call sites resolve a stored `ServerId`
/// through it, they all already handle `None` (that is the contract for an unpopulated slot), and
/// answering `None` here is what stops every one of them dialling the previous account's server
/// with the previous account's token. `Relaxed` on the floor: it is a lone `usize` publishing
/// nothing, and a reader that raced a sign-out by a microsecond would have read the old value a
/// microsecond earlier anyway.
pub fn client_for(id: ServerId) -> Option<&'static Client> {
    let i = id.index()?;
    if i < FLOOR.load(Ordering::Relaxed) || ACTIVE.load(Ordering::Acquire) & (1u32 << i) == 0 {
        return None;
    }
    let p = SLOTS.get(i)?.load(Ordering::Acquire);
    // SAFETY: a slot holds either null or a pointer from `Box::into_raw` that is NEVER freed
    // (see the module doc on the deliberate leak), published with a Release store paired to this
    // Acquire load. So a non-null read is always a fully-initialised, permanently-live `Client`.
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Which server `client()` answers with. `UNSET` before the first install.
///
/// **`Acquire`, and the pair with [`set_current`]'s `Release` is load-bearing on the TV.** The
/// invariant every reader depends on is *"if you can see this id, the slot it names is published"*
/// — `client_opt` reads this and then the slot, and `client()` PANICS on a null one. Relaxed on
/// both sides states no such ordering: ARMv7 is weakly ordered, so a poster worker calling
/// `client()` per art tile per frame could observe an id stored by the auth worker's `install`
/// before that thread's slot-pointer store landed, and take the panic. x86 would never show it,
/// which is exactly why it is spelled out here rather than left to a host test to catch.
pub fn current() -> ServerId {
    ServerId(CURRENT.load(Ordering::Acquire) as u16)
}

/// Point `client()` at another registered server. `false` (and no change) for an id that names
/// no client — retargeting to nothing would turn every `client()` into a panic.
pub fn set_current(id: ServerId) -> bool {
    let ok = client_for(id).is_some();
    if ok {
        // Release, pairing with `current()`'s Acquire: publishes the slot store that
        // `client_for` above just proved visible TO US, so it is visible to every later reader.
        CURRENT.store(id.0 as u32, Ordering::Release);
    }
    ok
}

/// The CURRENT server's `Client`. Panics if nothing has been installed — unchanged contract
/// (and unchanged message) from the singleton this replaced.
pub fn client() -> &'static Client {
    client_opt().expect("plex::install not called")
}

/// Non-panicking accessor for paths that can legitimately run before login (playback teardown,
/// the /tmp/plxnative-url + sample demo boots) — the old `route::CFG == None` guard semantics.
pub fn client_opt() -> Option<&'static Client> {
    client_for(current())
}

/// How many servers are registered **and still ours** — a sign-out takes the whole table down to
/// zero (see [`revoke_all`]).
///
/// Not a slot bound: slot numbers are permanent and profile visibility can be sparse. The caller
/// that wants slot NUMBERS wants [`ids`]; reading this as `0..count()` can hit an inactive share or
/// miss a live slot above it.
pub fn count() -> usize {
    ACTIVE.load(Ordering::Acquire).count_ones() as usize
}

pub fn roster_gen() -> u32 {
    ROSTER_GEN.load(Ordering::Acquire)
}

/// Every LIVE slot, in registration order — **the granted roster**, since a server is only
/// registered once the account was granted it, and only while that account is signed in.
/// Registration order matters to the one caller (`browse`'s section table appends in it, and the
/// session server registers first), so this is an ordered walk and not a set.
///
/// The walk starts at [`FLOOR`], not at 0: a sign-out retires the slots below it without
/// renumbering anything registered after (the module doc says why numbers are never reused).
pub fn ids() -> impl Iterator<Item = ServerId> {
    let hi = COUNT.load(Ordering::Acquire);
    let active = ACTIVE.load(Ordering::Acquire);
    (FLOOR.load(Ordering::Acquire).min(hi)..hi)
        .filter(move |&i| active & (1u32 << i) != 0)
        .map(|i| ServerId(i as u16))
}

/// What the roster says about one server, `None` until something has described it — and `None`
/// again once [`revoke_all`] has retired it, on the same floor [`client_for`] applies. The pair has
/// to answer alike or a stale `ServerId` held past a sign-out would still put the previous account's
/// machine name and the person who shared it on screen, off a slot nothing can dial.
pub fn facts(id: ServerId) -> Option<&'static ServerFacts> {
    let i = id.index()?;
    client_for(id)?;
    let p = FACTS.get(i)?.load(Ordering::Acquire);
    // SAFETY: identical to `client_for`'s — a slot holds null or a `Box::into_raw` pointer that is
    // never freed, published Release and read Acquire.
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Record what the roster says about a server. Idempotent and additive: a field given as empty
/// KEEPS whatever was already known, so a later describer that learned only the machine name (the
/// server naming itself over `GET /`) cannot blank an owner handle plex.tv already supplied — and
/// the two ingest paths can land in either order.
///
/// A no-op for an id that names no client: describing a slot nothing dials would leave a row in
/// the Sources list that cannot be browsed.
pub fn describe(id: ServerId, name: &str, handle: &str, owned: bool) {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(i) = id.index().filter(|_| client_for(id).is_some()) else { return };
    let old = facts(id);
    let merged = ServerFacts {
        name: pick(name, old.map(|f| f.name.as_str())),
        handle: pick(handle, old.map(|f| f.handle.as_str())),
        // `owned` has no "unknown", so the newest answer wins — but a describer that only learned
        // a name passes the flag it read back, which is what makes that a no-op rather than a lie.
        owned,
    };
    FACTS[i].store(Box::into_raw(Box::new(merged)), Ordering::Release);
    // A server learning its own name CHANGES PIXELS — the Search screen's scope line and the
    // Library's Source chip both read it — and it lands asynchronously, well after the first frame.
    // `ui::idle` gates the whole present on detected motion and cannot see a `static` being
    // written, so without this the new name sat invisible until the 2 s keepalive or the next
    // keypress: a screen showing "your server and your server" for two seconds after it knew better.
    crate::ui::idle::invalidate();
}

/// Record a server's MACHINE NAME and nothing else — for the describer that learned it from the
/// server itself (`GET /` → `friendlyName`) and knows nothing whatever about the grant.
///
/// It exists because [`describe`]'s `owned` has no "unknown" spelling, so a name-only describer has
/// to pass SOMETHING — and the one call site (`browse`'s discovery landing) computed it as
/// `handle.is_empty()`, which is precisely the derivation [`ServerFacts::owned`] documents as
/// wrong: a share whose `sourceTitle` plex.tv did not send has no handle and is still a share. So
/// every such share flipped to "ours" the moment its friendly name arrived — un-attributing it on
/// the detail page and the shelf headings, and pinning its libraries to Home by the ownership
/// default. Carrying the stored flag through is the only honest answer, and doing it HERE rather
/// than asking each call site to read [`facts`] back is what makes it unforgettable.
///
/// A slot nothing has described yet reads as ours, which is not a guess: the only registration that
/// does not describe is [`install`], the SESSION path, whose server is the account's own.
pub fn describe_name(id: ServerId, name: &str) {
    let owned = facts(id).map(|f| f.owned).unwrap_or(true);
    describe(id, name, "", owned);
}

/// `new` when it says something, else whatever was already known.
fn pick(new: &str, old: Option<&str>) -> String {
    if new.is_empty() { old.unwrap_or_default().to_owned() } else { new.to_owned() }
}

// ---- writes: registration ----

/// Is this slot the same SERVER as the one being registered?
///
/// Machine ids decide it whenever both sides have one — that is the identity that survives the
/// address moving. Otherwise the address is all there is: for the legacy `install` (no id at
/// all), and for a `register` that has learned an id for a slot registered without one, which is
/// the ADOPT case rather than a second slot for one server.
fn same_server(c: &Client, machine_id: &str, origin: &Origin) -> bool {
    if !machine_id.is_empty() && !c.machine_id().is_empty() {
        return c.machine_id() == machine_id;
    }
    c.origin() == origin
}

/// Publish a client into a slot. Takes the leak (see the module doc); the Release store pairs
/// with [`client_for`]'s Acquire load.
fn publish(id: ServerId, c: Client) {
    let p = Box::into_raw(Box::new(c));
    SLOTS[id.0 as usize].store(p, Ordering::Release);
}

/// Read a populated slot without applying profile visibility. Writers use this only while holding
/// [`WRITE`] so a profile-switch re-registration can find and reactivate the same machine id after
/// its old token was blanked. Slots below [`FLOOR`] are never searched: those belong to an account
/// that signed out and must never be adopted by the next one.
fn populated(id: ServerId) -> Option<&'static Client> {
    let i = id.index()?;
    let p = SLOTS.get(i)?.load(Ordering::Acquire);
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Publish one populated slot into the active profile's roster. Pointer/token writes happen first;
/// the Release bit is what makes them reachable through [`client_for`].
fn activate(id: ServerId) {
    let Some(i) = id.index() else { return };
    let bit = 1u32 << i;
    if ACTIVE.fetch_or(bit, Ordering::Release) & bit == 0 {
        ROSTER_GEN.fetch_add(1, Ordering::AcqRel);
    }
    if !current().is_set() {
        CURRENT.store(id.0 as u32, Ordering::Release);
    }
}

/// Register (or update) a server, returning its stable id — or [`ServerId::UNSET`] when the table
/// is full and there was nowhere to put it.
///
/// * an existing slot for the same server keeps its slot: the token is swapped IN PLACE, so
///   every `&'static Client` already handed out sees the new token — that is what the Plex Home
///   profile-switch path relies on;
/// * unless the address moved (or the slot's machine id was empty and is now known), in which
///   case the slot is RE-POINTED: a fresh `Client` is published over the old pointer;
/// * a server not in the table is appended.
///
/// **The full-table answer is a SENTINEL, and it used to be `current()`** — which reads as a
/// successful registration and is a lie about a completely different machine. The caller's very
/// next line is a `describe_server`, so a roster ingest that overflowed renamed the user's OWN
/// server to the share it could not fit and captioned it "Shared by <friend>"; `pms::sync_roster`
/// had to grow a de-duplication guard for the same return value. `ServerId::UNSET` resolves to
/// nothing everywhere by construction — [`describe`], [`set_current`] and [`client_for`] all turn
/// it away — so every caller degrades correctly without a full-table branch of its own.
///
/// Does NOT steal `current` from an established server — only the first registration sets it
/// (otherwise there is nothing for `client()` to answer with). Use [`set_current`] to switch,
/// or [`install`], which is the session path and always retargets.
pub fn register(machine_id: &str, host: &str, port: i32, token: &str) -> ServerId {
    register_origin(machine_id, &Origin::http(host, port), token)
}

/// [`register`], given the server's whole [`Origin`] instead of a plaintext address.
///
/// **This is the real one.** The `(host, port)` spelling above cannot say `https`, and an origin
/// is not something that can be rebuilt from an address afterwards: plex.tv advertises a server's
/// TLS origin as the `plex.direct` HOSTNAME while its `address` stays the dotted quad behind it,
/// and the certificate is issued for the name. So an origin has to be carried from where it was
/// parsed ([`super::probe::Candidate::origin`]) all the way to here, and the pair-shaped entry
/// points are kept only for callers that genuinely have nothing but an address.
pub fn register_origin(machine_id: &str, origin: &Origin, token: &str) -> ServerId {
    // The playback identity (`X-Plex-Client-Identifier`) is the persisted login identity, so it
    // comes from the session file — read LAZILY, i.e. only when a `Client` is actually built.
    // `session::load` can WRITE (it mints + persists the uuid when there is none), and the
    // commonest call here by far is the profile switch, which only swaps a token; the singleton
    // this replaced read the file exactly once, and so does this.
    let id = register_lazy(machine_id, origin, token, &|| super::session::load().client_id);
    // Every server the app actually talks to arrives through THIS function (the `_with_client_id`
    // seam below is the test one and deliberately does not), so it is the single place that keeps
    // each server's self-description — version + Plex Pass, issue #22's blind spot — fresh
    // without every caller remembering to. It used to sit in `install`, which reached only the
    // CURRENT server; a shared server registered beside it would have stayed permanently
    // `Unknown`, and the failure read-out blames a missing Pass on a known-free server only.
    // A worker fetch, single-flighted per server; nothing waits on it.
    super::serverinfo::refresh(id);
    id
}

/// [`register`] with the device id supplied — the seam that keeps the session file **and the
/// network** out of the registry proper, and out of the tests.
///
/// `pub(crate)` rather than `pub(super)` because tests OUTSIDE `plex/` need it too (`route.rs`
/// grades which server a `/:/timeline` POST reaches, which takes two registered clients).
///
/// **Two reasons a host test must come through here, and the second one cost a red CI run.**
///
/// 1. The public [`register`] resolves the device id through `session::load`, which MINTS AND
///    PERSISTS a uuid when there is none. A host test must not write one.
/// 2. [`register`] also fires [`serverinfo::refresh`](super::serverinfo::refresh), which spawns a
///    `task::spawn_small` worker that really opens a socket — on a **256 KiB** stack. In a DEBUG
///    build `stream::http_stream_boxed` builds its 64 KiB `HttpStream` as a stack temporary before
///    it reaches the heap (the optimizer elides that in the `--release` build the television
///    ships, and only there), so that worker needs somewhere north of 192 KiB on ARM64 and more
///    than 256 KiB on x86-64. A test that calls [`register`] therefore aborts the whole suite with
///    `thread '<unknown>' has overflowed its stack` — on Linux only, in a thread libtest never
///    named, at whatever test happened to be printing. `make check` passed on the dev Mac while CI
///    failed twice.
///
/// So: **no test in this crate may call [`register`] or [`register_origin`]**. The two seams here
/// are the whole test surface, and neither loads a session nor spawns anything.
pub(crate) fn register_with_client_id(machine_id: &str, host: &str, port: i32, token: &str, client_id: &str) -> ServerId {
    register_origin_with_client_id(machine_id, &Origin::http(host, port), token, client_id)
}

/// [`register_with_client_id`], given the whole [`Origin`] — the seam for a test that is about the
/// SCHEME, which the `(host, port)` form cannot express. Same contract: no session file, no worker.
pub(crate) fn register_origin_with_client_id(
    machine_id: &str,
    origin: &Origin,
    token: &str,
    client_id: &str,
) -> ServerId {
    register_lazy(machine_id, origin, token, &|| client_id.to_owned())
}

fn register_lazy(machine_id: &str, origin: &Origin, token: &str, client_id: &dyn Fn() -> String) -> ServerId {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let n = COUNT.load(Ordering::Acquire);
    let floor = FLOOR.load(Ordering::Acquire).min(n);
    // Search every populated slot in THIS account's window, including one deactivated by a profile
    // switch. Reusing that machine's stable id is how repeated profile changes avoid consuming the
    // 16-slot table; a sign-out raises FLOOR, so a previous account's slot is never considered.
    let found = (floor..n)
        .map(|i| ServerId(i as u16))
        .find(|&id| populated(id).is_some_and(|c| same_server(c, machine_id, origin)));

    if let Some(id) = found {
        let c = populated(id).expect("the matched slot is populated");
        // Keep an id we already know: a legacy address-keyed call must not blank it.
        let mid = if machine_id.is_empty() { c.machine_id() } else { machine_id };
        if c.origin() != origin || c.machine_id() != mid {
            // Re-point. The old `Client` stays alive and merely stale for anyone mid-request
            // with it; the fresh one also gets a fresh token generation, so token-baked caches
            // flush without a special case.
            //
            // `log_form`: the bare authority for a plaintext origin, so this line stays
            // byte-identical to the `{host}:{port}` it has always been and an archived log is
            // still comparable with a current one — and the whole URL the moment the scheme is
            // worth saying, which is the only way a headless run armed with `{"scheme":"https"}`
            // can be told from an http one at all. See `Origin::log_form`.
            crate::log(&format!("plex: server slot {} re-pointed to {}", id.0, origin.log_form()));
            // The pointer is leaked, so a worker may retain it forever. Defang that old reference
            // before publishing the replacement; later sign-out/profile revocation can only walk
            // the current pointer stored in this slot and cannot discover superseded clients.
            c.set_token("");
            publish(id, Client::new(id, mid, origin.clone(), token, &client_id()));
            ROSTER_GEN.fetch_add(1, Ordering::AcqRel);
        } else {
            c.set_token(token); // in place — every reference already handed out follows along
        }
        activate(id);
        return id;
    }

    if n >= MAX_SERVERS {
        // The sentinel, not `current()` — see this function's doc for what the old answer did to
        // the caller's `describe_server` one line later.
        crate::log("plex: server registry full — this server was NOT registered");
        return ServerId::UNSET;
    }
    let id = ServerId(n as u16);
    publish(id, Client::new(id, machine_id, origin.clone(), token, &client_id()));
    COUNT.store(n + 1, Ordering::Release); // after the pointer: a visible count implies a live slot
    activate(id);
    // Address only — the machineIdentifier is a permanent household fingerprint (see `ui::stats`)
    // and the event log is what users send us. `log_form` rather than `base`, for the reason the
    // re-point line above gives.
    crate::log(&format!("plex: server slot {} registered at {}", id.0, origin.log_form()));
    id
}

/// Install for a (re)login / profile switch — the SESSION path, unchanged signature and
/// unchanged single-server behaviour.
///
/// The caller has an address and no machine id (a stored session, or what the login resolved),
/// so the same address is the same server: a second call for it swaps the token in place exactly
/// as the old singleton did — which is every install this app makes today, and why nothing about
/// a single-server session changed. A call naming a DIFFERENT address now registers a second slot
/// and makes it current, where the singleton kept the FIRST server's address and quietly applied
/// the new token to it (a mis-target no caller could see, because the address was frozen).
pub fn install(origin: &Origin, token: &str) {
    let id = register_origin("", origin, token);
    set_current(id); // the session path always retargets: this is now the server we are using
    // (`register` already refreshed this server's self-description — see its doc.)
}

/// **Profile switch.** Blank every live token and hide every non-current slot before the new
/// profile's grants are registered. Unlike [`revoke_all`], this does not raise [`FLOOR`]:
/// registering a machine the new profile may use reactivates its stable slot, while an omitted
/// share stays invisible and its previously handed-out `Client` stays tokenless.
///
/// The current slot remains visible but tokenless until its new grant is installed. Keeping that
/// shell is load-bearing for the lock-free `client()` contract: a reader that sampled CURRENT just
/// before this write must still resolve the slot rather than panic. The switch response already
/// proved this primary machine is granted to the new profile, so the caller immediately re-tokens
/// it while holding auth's activation gate.
pub(crate) fn revoke_for_profile_switch() {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let live: Vec<ServerId> = ids().collect();
    let keep = current();
    let keep_live = client_for(keep).is_some();
    for id in &live {
        if let Some(c) = client_for(*id) {
            c.set_token("");
        }
    }
    let keep_mask = keep
        .index()
        .filter(|_| keep_live)
        .map_or(0, |i| 1u32 << i);
    ACTIVE.store(keep_mask, Ordering::Release);
    ROSTER_GEN.fetch_add(1, Ordering::AcqRel);
    if !keep_live {
        CURRENT.store(ServerId::UNSET.0 as u32, Ordering::Release);
    }
    if !live.is_empty() {
        crate::log(&format!("plex: {} server(s) revoked — profile changed", live.len()));
    }
    crate::ui::idle::invalidate();
}

/// Finish a profile/roster replacement with exactly the slots the caller just installed.
///
/// [`revoke_for_profile_switch`] temporarily retains `current` as a tokenless shell so a reader
/// which sampled its id immediately before the revoke cannot resolve a vanished slot. Once the
/// replacement grants have been published, that bridge must be removed: the old primary may no
/// longer be granted at all. This is the commit point that turns the temporary union into the
/// authoritative roster and, when necessary, moves `current` to the first installed survivor.
pub(crate) fn finish_profile_switch(installed: &[ServerId]) {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let floor = FLOOR.load(Ordering::Acquire);
    let mut exact = 0u32;
    let mut first = ServerId::UNSET;
    for &id in installed {
        let Some(i) = id.index() else { continue };
        if i < floor || populated(id).is_none() {
            continue;
        }
        exact |= 1u32 << i;
        if !first.is_set() {
            first = id;
        }
    }

    let old_current = current();
    let keep_current = old_current.index().is_some_and(|i| exact & (1u32 << i) != 0);
    let next = if keep_current { old_current } else { first };

    // Every installed slot was already activated, so publishing the new CURRENT first cannot
    // point at a hidden slot. Removing the bridge second avoids the inverse window: CURRENT still
    // naming the just-hidden old primary, which would make `client_opt()` transiently return None.
    CURRENT.store(next.0 as u32, Ordering::Release);
    let old = ACTIVE.swap(exact, Ordering::AcqRel);
    if old != exact {
        ROSTER_GEN.fetch_add(1, Ordering::AcqRel);
        crate::ui::idle::invalidate();
    }
}

/// **Sign-out.** Retire every registered server: `client()` answers with nothing again, [`ids`]
/// walks nothing, and each slot's token is blanked in place.
///
/// The blanking is the half that is easy to leave out and the half that matters most. Raising the
/// floor stops anything RESOLVING a `ServerId` — but ~30 call sites take a `&'static Client` and
/// hold it across a spawn (that is the whole reason the clients are leaked), so a worker already
/// out when the user signed out still has a reference with a live per-(user, server) token on it.
/// [`Client::set_token`] is in-place and every reference already handed out follows along, so after
/// this the worst that reference can do is send a tokenless request and get a 401.
///
/// It does NOT free anything and does not lower [`COUNT`] — see the module doc on why a slot number
/// is never handed out twice.
pub(crate) fn revoke_all() {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let n = COUNT.load(Ordering::Acquire);
    let floor = FLOOR.load(Ordering::Acquire);
    for id in ids() {
        if let Some(c) = client_for(id) {
            c.set_token("");
        }
    }
    // CURRENT first: for the instant between these two stores a reader must never see an id whose
    // slot the floor has already killed, which is the one state `client()`'s `expect` would take.
    CURRENT.store(ServerId::UNSET.0 as u32, Ordering::Release);
    ACTIVE.store(0, Ordering::Release);
    FLOOR.store(n, Ordering::Release);
    ROSTER_GEN.fetch_add(1, Ordering::AcqRel);
    if n > floor {
        crate::log(&format!("plex: {} server(s) revoked — signed out", n - floor));
    }
    // The Sources list, the Search scope line and every shelf heading are drawn from this table,
    // and `ui::idle` gates the whole present on detected motion — it cannot see a `static` being
    // written. Same reason `describe` invalidates.
    crate::ui::idle::invalidate();
}

/// Empty the table so each test starts from "nothing installed". Leaks whatever was registered
/// (that is the ordinary lifecycle here, not a test-only wart) and must be called under
/// [`crate::testlock::serial`] — the registry is a crate global.
///
/// `pub(crate)` for the same reason as [`register_with_client_id`]: a test outside `plex/` that
/// registers a server owes the rest of the suite an empty table on the way out, or the next test
/// to ask `client_opt()` gets `Some(a client whose port closed when that test returned)`.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    for s in SLOTS.iter() {
        s.store(std::ptr::null_mut(), Ordering::Release);
    }
    for f in FACTS.iter() {
        f.store(std::ptr::null_mut(), Ordering::Release);
    }
    COUNT.store(0, Ordering::Release);
    ACTIVE.store(0, Ordering::Release);
    ROSTER_GEN.store(1, Ordering::Release);
    // The floor goes back with the count, or every test after one that signed out would register
    // into slots the walk starts above — a table that reads as empty however much is in it.
    FLOOR.store(0, Ordering::Release);
    CURRENT.store(ServerId::UNSET.0 as u32, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here mutates the ONE registry, so they hold the crate-wide serialization lock
    /// (`lib.rs`'s `testlock`) rather than a module-local one: `client()` is reachable from other
    /// modules' tests too.
    ///
    /// It empties the registry on the way OUT as well as on the way in, and that half is
    /// load-bearing rather than tidy: `browse::pump` adopts every registered slot as a source and
    /// then spawns a discovery worker for it, so servers left behind here would have another
    /// module's tests dialling `10.0.0.1` on a background thread. The reset happens while the lock
    /// is still held (a struct's own `Drop` runs before its fields').
    struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Fresh {
        fn drop(&mut self) {
            reset_for_test();
        }
    }
    fn fresh() -> Fresh {
        let g = crate::testlock::serial();
        reset_for_test();
        Fresh(g)
    }

    /// The token as the wire would carry it — `with_token` is the only reader of the field.
    fn token_of(c: &Client) -> String {
        c.with_token("/x").rsplit('=').next().unwrap_or_default().to_owned()
    }

    fn reg(machine_id: &str, host: &str, token: &str) -> ServerId {
        register_with_client_id(machine_id, host, 32400, token, "test-client-id")
    }

    /// The table's basic contract: a registration round trips through its id, the reserved UNSET
    /// value resolves to nothing (rather than slot 0, which is the bug a sentinel of 0 would
    /// have), and an id that names an empty slot is `None`, not a wild pointer.
    #[test]
    fn a_registered_server_round_trips_and_an_unset_id_resolves_to_nothing() {
        let _g = fresh();
        assert!(client_opt().is_none(), "nothing installed yet");
        assert!(!current().is_set());
        assert!(client_for(ServerId::UNSET).is_none(), "the reserved value must never resolve");

        let id = reg("mach-A", "10.0.0.1", "tok-a");
        let c = client_for(id).expect("round trip");
        assert_eq!((c.host(), c.port(), c.machine_id()), ("10.0.0.1", 32400, "mach-A"));
        assert_eq!(c.id(), id, "the client knows its own slot");
        assert_eq!(count(), 1);
        // the first registration becomes current, so `client()` works exactly as after the first
        // `install` did under the singleton
        assert!(std::ptr::eq(client(), c));
        assert!(client_for(ServerId::from_raw(7)).is_none(), "an unpopulated slot");
    }

    /// The profile-switch path: same server, new per-user token. It must land IN PLACE — same
    /// slot, same pointer — because ~30 call sites hold a `&'static Client` from before the
    /// switch and must see the new token without re-fetching anything.
    #[test]
    fn re_registering_the_same_server_swaps_the_token_in_place() {
        let _g = fresh();
        let id = reg("mach-A", "10.0.0.1", "tok-a");
        let before: *const Client = client_for(id).unwrap();

        let again = reg("mach-A", "10.0.0.1", "tok-b");
        assert_eq!(again, id, "same server, same slot");
        assert_eq!(count(), 1, "no slot appended");
        let c = client_for(id).unwrap();
        assert!(std::ptr::eq(c, before), "the client was updated, not replaced");
        assert_eq!(token_of(c), "tok-b");

        // and the legacy address-keyed form (`install`, which knows no machine id) matches the
        // same slot on host+port alone
        let by_addr = reg("", "10.0.0.1", "tok-c");
        assert_eq!(by_addr, id);
        assert_eq!(count(), 1);
        assert_eq!(token_of(client_for(id).unwrap()), "tok-c");
        assert_eq!(client_for(id).unwrap().machine_id(), "mach-A", "a call with no id must not blank one");
    }

    /// **A scheme change is an ADDRESS change**, so it re-points the slot exactly as a new host or
    /// port does — a fresh `Client`, a fresh token generation, the tier reset to unknown.
    ///
    /// It cannot be observed any other way: `same_server` matches on the machine id whenever both
    /// sides have one, so the origin comparison only ever decides whether to RE-POINT. Get that
    /// comparison wrong — compare `host`/`port` and forget the scheme, which is exactly what the
    /// pair-shaped code did because it could not spell one — and the day a server moves to https
    /// the registry keeps a plaintext client for it, in place, with nothing in the log.
    ///
    /// Driven through [`register_origin_with_client_id`], NOT the public [`register_origin`] — see
    /// that seam's doc. This test was written against the public one and turned CI red on Linux
    /// with a stack overflow in an unnamed thread, because `register_origin` spawns a real
    /// `serverinfo` worker that opens a real socket on a 256 KiB stack.
    #[test]
    fn moving_a_server_to_https_re_points_its_slot() {
        let _g = fresh();
        let reg_at = |o: &Origin, tok: &str| register_origin_with_client_id("mach-A", o, tok, "test-client-id");
        let plain = Origin::http("10.0.0.1", 32400);
        let id = reg_at(&plain, "tok-a");
        let before: *const Client = client_for(id).unwrap();
        let gen_before = client_for(id).unwrap().token_gen();
        client_for(id).unwrap().set_link(crate::plex::probe::Location::Local);

        let tls = Origin::parse("https://10-0-0-1.hash.plex.direct:32400").expect("an origin");
        assert_eq!(reg_at(&tls, "tok-a"), id, "same machine, same slot");
        assert_eq!(count(), 1, "a scheme change is not a second server");

        let c = client_for(id).unwrap();
        assert!(!std::ptr::eq(c, before), "the slot was RE-POINTED, not updated in place");
        assert_eq!(c.origin(), &tls);
        assert_eq!(c.host(), "10-0-0-1.hash.plex.direct", "the name a certificate is issued for");
        assert_ne!(c.token_gen(), gen_before, "a fresh client means token-baked caches flush");
        assert_eq!(c.link(), None, "a new address is not evidence about the old tier");

        // and re-registering the SAME origin lands in place again, as any unchanged address does
        let same: *const Client = c;
        assert_eq!(reg_at(&tls, "tok-b"), id);
        assert!(std::ptr::eq(client_for(id).unwrap(), same), "unchanged origin, unchanged pointer");
    }

    /// A different server is a NEW slot, never a silent retarget of the old one — the singleton's
    /// defining limitation. `current` stays where it was until something moves it, and both
    /// clients remain individually reachable by id.
    #[test]
    fn a_different_server_appends_a_slot_and_current_moves_only_when_told() {
        let _g = fresh();
        let a = reg("mach-A", "10.0.0.1", "tok-a");
        let b = reg("mach-B", "10.0.0.2", "tok-b");
        assert_ne!(a, b);
        assert_eq!(count(), 2, "appended, not retargeted");
        assert_eq!(client_for(a).unwrap().host(), "10.0.0.1", "server A kept its address");
        assert!(std::ptr::eq(client(), client_for(a).unwrap()), "current stayed on A");

        assert!(set_current(b));
        assert!(std::ptr::eq(client(), client_for(b).unwrap()));
        assert_eq!(client().machine_id(), "mach-B");
        assert!(client_for(a).is_some(), "A is still registered and reachable by id");

        assert!(!set_current(ServerId::from_raw(9)), "an unknown id is refused");
        assert!(std::ptr::eq(client(), client_for(b).unwrap()), "…and changes nothing");

        // the legacy address-keyed path, on an address nobody is registered at, also appends
        let c = reg("", "10.0.0.3", "tok-c");
        assert_eq!(count(), 3);
        assert_eq!(client_for(c).unwrap().host(), "10.0.0.3");
    }

    /// Token generations are PER SERVER (they used to be one process-global counter). A profile
    /// switch on A must flush A's token-baked caches and leave B's alone — and the two servers'
    /// generations must differ, so a cache that only asks "did this number move" also flushes
    /// when `client()` starts answering with the other server.
    #[test]
    fn a_token_swap_moves_only_that_servers_generation() {
        let _g = fresh();
        let a = reg("mach-A", "10.0.0.1", "tok-a");
        let b = reg("mach-B", "10.0.0.2", "tok-b");
        let (ga, gb) = (client_for(a).unwrap().token_gen(), client_for(b).unwrap().token_gen());
        assert_ne!(ga, gb, "two servers never share a generation");

        reg("mach-A", "10.0.0.1", "tok-a2");
        assert_ne!(client_for(a).unwrap().token_gen(), ga, "A's token changed");
        assert_eq!(client_for(b).unwrap().token_gen(), gb, "B's did not");
    }

    /// The identity rule the whole app's stored rows compare on. The case it exists for is the
    /// FIRST assertion: the share's ratingKeys are dense from 1 exactly like ours, so a bare key
    /// matching is the bug, not the feature. No registry state is touched, so no lock is needed.
    #[test]
    fn one_rating_key_on_two_servers_is_two_different_items() {
        let (a, b) = (ServerId::from_raw(0), ServerId::from_raw(1));
        assert!(!same_item((a, "1"), (b, "1")), "the same key on two servers is two items");
        assert!(same_item((a, "1"), (a, "1")), "…and the same key on ONE server is one item");
        assert!(!same_item((a, "1"), (a, "2")), "a different key is a different item");

        // UNSET is the pre-registry / host-test state: it matches itself (the single-server app,
        // unchanged) and nothing else — a row whose server is unknown must never answer for a
        // named one.
        assert!(same_item((ServerId::UNSET, "1"), (ServerId::UNSET, "1")));
        assert!(!same_item((ServerId::UNSET, "1"), (a, "1")));
        assert!(!same_item((a, "1"), (ServerId::UNSET, "1")));
    }

    /// The roster half: [`ids`] walks every registered slot in registration order (that IS the
    /// granted roster), and [`describe`] MERGES rather than replaces — the two ingest paths (plex.tv,
    /// which knows the owner, and the server naming itself over `GET /`, which knows only the machine
    /// name) land in either order without either one blanking the other's field.
    #[test]
    fn the_roster_walks_in_registration_order_and_describing_it_never_blanks_a_known_field() {
        let _g = fresh();
        let a = reg("mach-A", "10.0.0.1", "tok-a");
        let b = reg("mach-B", "10.0.0.2", "tok-b");
        assert_eq!(ids().collect::<Vec<_>>(), vec![a, b], "registration order, the session server first");
        assert!(facts(a).is_none(), "nothing has described it yet — not an empty-named server");

        // plex.tv first (owner known, no machine name in this path), then the server itself
        describe(b, "", "friend", false);
        describe(b, "nas-home", "", false);
        let f = facts(b).expect("described");
        assert_eq!((f.name.as_str(), f.handle.as_str(), f.owned), ("nas-home", "friend", false));

        // and the other order, on the other slot
        describe(a, "mac-mini", "", true);
        describe(a, "", "", true);
        let f = facts(a).expect("described");
        assert_eq!((f.name.as_str(), f.handle.as_str(), f.owned), ("mac-mini", "", true));

        // a slot nothing dials is never described — a Sources row you cannot browse
        describe(ServerId::from_raw(9), "ghost", "nobody", false);
        assert!(facts(ServerId::from_raw(9)).is_none());
        assert!(facts(ServerId::UNSET).is_none(), "the reserved value must never resolve");
    }

    /// A slot registered by address only (what `install` can do) is ADOPTED once the machine id
    /// is learned: re-pointed in place of a second slot for one server. The old reference stays
    /// valid — merely stale — which is the whole reason the clients are leaked.
    #[test]
    fn learning_the_machine_id_adopts_the_address_only_slot() {
        let _g = fresh();
        let id = reg("", "10.0.0.1", "tok-a");
        let stale: &'static Client = client_for(id).unwrap();
        assert_eq!(stale.machine_id(), "");

        let same = reg("mach-A", "10.0.0.1", "tok-a");
        assert_eq!(same, id, "adopted, not appended");
        assert_eq!(count(), 1);
        assert_eq!(client_for(id).unwrap().machine_id(), "mach-A");
        assert_eq!(stale.host(), "10.0.0.1", "the replaced client is still readable, not freed");
        assert_eq!(token_of(stale), "", "a superseded leaked client cannot retain a credential");

        // and once known, the id is what identifies it — the same server at a new address
        // re-points that slot rather than appending
        let adopted: &'static Client = client_for(id).unwrap();
        let moved = reg("mach-A", "10.0.0.9", "tok-a");
        assert_eq!(moved, id);
        assert_eq!(count(), 1);
        assert_eq!(client_for(id).unwrap().host(), "10.0.0.9");
        assert_eq!(token_of(adopted), "", "every re-point defangs the pointer it supersedes");
    }

    /// **Signing out takes the servers with it.** Nothing here can be freed, so the leak that
    /// mattered was not memory: the account that left stayed in the table with its live
    /// per-(user, server) tokens, and every roster walk in the app (`pms::roster`,
    /// `browse::sync_roster`, `search`'s fan-out, the Sources list) is one of the two accessors
    /// asserted here.
    #[test]
    fn signing_out_retires_every_slot_and_defangs_the_references_already_handed_out() {
        let _g = fresh();
        let a = reg("mach-A", "10.0.0.1", "tok-a");
        let b = reg("mach-B", "10.0.0.2", "tok-b");
        describe(b, "nas-home", "friend", false);
        // the reference a worker took before the sign-out, which nothing can take back
        let inflight: &'static Client = client_for(b).unwrap();
        assert_eq!(token_of(inflight), "tok-b");

        revoke_all();

        assert_eq!(count(), 0, "no server is registered any more");
        assert_eq!(ids().collect::<Vec<_>>(), Vec::new(), "and the roster walk yields nothing");
        assert!(client_for(a).is_none() && client_for(b).is_none(), "a stored ServerId resolves to nothing");
        assert!(client_opt().is_none() && !current().is_set(), "and `client()` has nothing to answer with");
        // the description goes with the client: a stale id must not still name the friend who
        // shared it, off a slot nothing can dial
        assert!(facts(b).is_none(), "a slot that cannot be dialled is not described either");
        // the in-flight reference is still READABLE — it was never freed, which is the whole
        // reason these are leaked — but it can no longer dial as anybody
        assert_eq!(inflight.host(), "10.0.0.2");
        assert_eq!(token_of(inflight), "", "a worker mid-request must not carry the old credential");
    }

    #[test]
    fn a_profile_switch_reactivates_only_the_new_profiles_granted_servers() {
        let _g = fresh();
        let ours = reg("mach-A", "10.0.0.1", "owner-a");
        let share = reg("mach-B", "10.0.0.2", "owner-b");
        let old_ours: &'static Client = client_for(ours).unwrap();
        let old_share: &'static Client = client_for(share).unwrap();

        revoke_for_profile_switch();

        assert_eq!(count(), 1, "the current slot remains as a lock-free tokenless shell");
        assert_eq!(ids().collect::<Vec<_>>(), vec![ours]);
        assert_eq!(token_of(client_for(ours).unwrap()), "");
        assert!(client_for(share).is_none());
        assert_eq!(token_of(old_ours), "");
        assert_eq!(token_of(old_share), "", "an omitted share loses the old profile's credential");

        let again = reg("mach-A", "10.0.0.1", "managed-a");
        assert_eq!(again, ours, "a profile switch preserves the machine's stable slot");
        assert_eq!(ids().collect::<Vec<_>>(), vec![ours]);
        assert_eq!(token_of(client_for(ours).unwrap()), "managed-a");
        assert!(client_for(share).is_none(), "the ungranted share stays out of every roster walk");
        assert_eq!(token_of(old_share), "", "and even a stale reference remains defanged");
        assert!(std::ptr::eq(client(), client_for(ours).unwrap()));
    }

    #[test]
    fn a_removed_primary_shell_disappears_after_the_surviving_roster_is_committed() {
        let _g = fresh();
        let gone = reg("mach-A", "10.0.0.1", "owner-a");
        let survivor = reg("mach-B", "10.0.0.2", "owner-b");
        let stale_gone: &'static Client = client_for(gone).unwrap();

        revoke_for_profile_switch();
        assert_eq!(ids().collect::<Vec<_>>(), vec![gone], "the old current is only a temporary shell");
        assert_eq!(reg("mach-B", "10.0.0.2", "fresh-b"), survivor);
        set_current(survivor);
        finish_profile_switch(&[survivor]);

        assert_eq!(count(), 1);
        assert_eq!(ids().collect::<Vec<_>>(), vec![survivor]);
        assert!(client_for(gone).is_none(), "a revoked primary is absent from the authoritative roster");
        assert_eq!(token_of(stale_gone), "", "its already-issued reference stays defanged");
        assert!(std::ptr::eq(client(), client_for(survivor).unwrap()));
        assert_eq!(token_of(client()), "fresh-b");
    }

    /// The account that signs in next gets FRESH slot numbers, even for a machine the previous
    /// account was also granted. `ServerId` names one server for the life of the process — it sits
    /// in UI state, in routes, in queued jobs, and every per-server store in the app is a flat
    /// array indexed by one — so re-adopting a revoked slot would file the new account's results
    /// under the old account's rows.
    #[test]
    fn a_revoked_slot_is_never_resurrected_even_for_the_same_machine() {
        let _g = fresh();
        let before = reg("mach-A", "10.0.0.1", "tok-old");
        revoke_all();

        let after = reg("mach-A", "10.0.0.1", "tok-new");
        assert_ne!(after, before, "the same machine, a new account, a new slot");
        assert_eq!(count(), 1, "…and exactly one server is registered");
        assert_eq!(ids().collect::<Vec<_>>(), vec![after], "the walk starts above the floor");
        assert_eq!(token_of(client_for(after).unwrap()), "tok-new");
        assert!(client_for(before).is_none(), "the retired slot stays retired");
        // the first registration after a sign-out becomes current, exactly as it does at boot
        assert!(std::ptr::eq(client(), client_for(after).unwrap()));

        // and a description of the retired slot is still refused — it dials nothing
        describe(before, "ghost", "nobody", false);
        assert!(facts(before).is_none());
    }

    /// **A full table answers with a SENTINEL, not with `current()`.** The caller's next line is a
    /// `describe_server`, so the old answer renamed the user's OWN server to whichever share had
    /// nowhere to go and captioned it "Shared by <friend>" — `auth::install_roster` does exactly
    /// this pair, and `pms::sync_roster` had to grow a de-duplication guard for the same value.
    #[test]
    fn a_registration_that_does_not_fit_is_refused_rather_than_aliased_onto_the_current_server() {
        let _g = fresh();
        let ours = reg("mach-ours", "10.0.0.1", "tok-ours");
        describe(ours, "Mac mini", "", true);
        for i in 1..MAX_SERVERS {
            reg(&format!("mach-{i}"), &format!("10.0.1.{i}"), "tok");
        }
        assert_eq!(count(), MAX_SERVERS);

        let refused = reg("mach-overflow", "10.0.9.9", "tok-overflow");
        assert_eq!(refused, ServerId::UNSET, "there was nowhere to put it, and that is what it says");
        assert_eq!(count(), MAX_SERVERS, "nothing was appended");

        // the call site's very next line, verbatim — and it must land on nobody
        describe(refused, "nas-home", "friend", false);
        let f = facts(ours).expect("our own server is still described");
        assert_eq!((f.name.as_str(), f.handle.as_str(), f.owned), ("Mac mini", "", true), "ours was not renamed");
        assert!(!set_current(refused), "and it cannot become the current server either");
        assert!(std::ptr::eq(client(), client_for(ours).unwrap()));
    }

    /// [`describe_name`] is for the describer that learned a machine name off the server itself and
    /// knows nothing about the grant. `owned` has no "unknown" in [`describe`], so that caller had
    /// to pass something and computed it as `handle.is_empty()` — which is the derivation
    /// [`ServerFacts::owned`] documents as wrong, and flipped every handle-less share to "ours" the
    /// moment its friendly name arrived.
    #[test]
    fn naming_a_server_carries_its_ownership_through_untouched() {
        let _g = fresh();
        let a = reg("mach-A", "10.0.0.1", "tok-a");
        let b = reg("mach-B", "10.0.0.2", "tok-b");
        // the case the bug was invisible in: a share plex.tv sent no `sourceTitle` for, so there is
        // no handle to derive anything from — and it is a share all the same
        describe(a, "", "", false);
        describe(b, "", "friend", false);

        describe_name(a, "nas-home");
        describe_name(b, "nas-loft");

        assert_eq!(facts(a).map(|f| (f.name.as_str(), f.owned)), Some(("nas-home", false)), "a handle-less SHARE");
        assert_eq!(facts(b).map(|f| (f.name.as_str(), f.handle.as_str(), f.owned)), Some(("nas-loft", "friend", false)));

        // a slot nothing has described is one `install` put there, i.e. the account's own server
        let c = reg("mach-C", "10.0.0.3", "tok-c");
        describe_name(c, "Mac mini");
        assert_eq!(facts(c).map(|f| (f.name.as_str(), f.owned)), Some(("Mac mini", true)));
    }
}
