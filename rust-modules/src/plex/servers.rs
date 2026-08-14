//! The SERVER REGISTRY — every PMS this session knows about, which one is CURRENT, and the
//! `&'static Client` handed to ~30 call sites outside this module.
//!
//! This replaces `client.rs`'s `static PLEX: OnceLock<Client>`: one server per process, forever,
//! whose second `install` could only swap a token because host/port were frozen by the first.
//! That single fact is what made browsing a SECOND (shared) server impossible — not the UI, not
//! the ops. So the singleton becomes a table, and `client()` becomes "the current entry of it"
//! with its signature untouched, which is why nothing outside `plex/` changed in the commit that
//! introduced this file.
//!
//! **Keyed by `machineIdentifier`.** The server's own permanent id is the only identity that
//! survives it changing address (LAN ↔ remote, DHCP, a relay), so the table keys on it and an
//! address is just where that key currently answers. The legacy `install(host, port, token)`
//! doesn't know the id — its callers (`app.rs`, `auth.rs`) have only a stored session address —
//! so it registers with an EMPTY id and matches on the address; a later [`register`] that does
//! know the id adopts that slot instead of adding a second one for the same server.
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

use super::client::Client;
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
/// Slots populated so far. Only writers and the match scan read it; `client_for` does not.
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// The current/primary server as a raw `ServerId` — what `client()` answers with.
static CURRENT: AtomicU32 = AtomicU32::new(ServerId::UNSET.0 as u32);
/// Writers only (register / re-point / reset). Readers never take it — that is the whole point.
static WRITE: Mutex<()> = Mutex::new(());

// ---- reads: the hot path ----

/// The `Client` for one slot, `None` for [`ServerId::UNSET`] or an unpopulated slot.
pub fn client_for(id: ServerId) -> Option<&'static Client> {
    let p = SLOTS.get(id.index()?)?.load(Ordering::Acquire);
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

/// How many servers are registered.
pub fn count() -> usize {
    COUNT.load(Ordering::Acquire)
}

/// Every registered slot, in registration order — **the granted roster**, since a server is only
/// registered once the account was granted it. Registration order matters to the one caller
/// (`browse`'s section table appends in it, and the session server registers first), so this is an
/// ordered walk and not a set.
pub fn ids() -> impl Iterator<Item = ServerId> {
    (0..count() as u16).map(ServerId)
}

/// What the roster says about one server, `None` until something has described it.
pub fn facts(id: ServerId) -> Option<&'static ServerFacts> {
    let p = FACTS.get(id.index()?)?.load(Ordering::Acquire);
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
fn same_server(c: &Client, machine_id: &str, host: &str, port: i32) -> bool {
    if !machine_id.is_empty() && !c.machine_id().is_empty() {
        return c.machine_id() == machine_id;
    }
    c.host() == host && c.port() == port
}

/// Publish a client into a slot. Takes the leak (see the module doc); the Release store pairs
/// with [`client_for`]'s Acquire load.
fn publish(id: ServerId, c: Client) {
    let p = Box::into_raw(Box::new(c));
    SLOTS[id.0 as usize].store(p, Ordering::Release);
}

/// Register (or update) a server, returning its stable id.
///
/// * an existing slot for the same server keeps its slot: the token is swapped IN PLACE, so
///   every `&'static Client` already handed out sees the new token — that is what the Plex Home
///   profile-switch path relies on;
/// * unless the address moved (or the slot's machine id was empty and is now known), in which
///   case the slot is RE-POINTED: a fresh `Client` is published over the old pointer;
/// * a server not in the table is appended.
///
/// Does NOT steal `current` from an established server — only the first registration sets it
/// (otherwise there is nothing for `client()` to answer with). Use [`set_current`] to switch,
/// or [`install`], which is the session path and always retargets.
pub fn register(machine_id: &str, host: &str, port: i32, token: &str) -> ServerId {
    // The playback identity (`X-Plex-Client-Identifier`) is the persisted login identity, so it
    // comes from the session file — read LAZILY, i.e. only when a `Client` is actually built.
    // `session::load` can WRITE (it mints + persists the uuid when there is none), and the
    // commonest call here by far is the profile switch, which only swaps a token; the singleton
    // this replaced read the file exactly once, and so does this.
    let id = register_lazy(machine_id, host, port, token, &|| super::session::load().client_id);
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

/// [`register`] with the device id supplied — the seam that keeps the session file out of the
/// registry proper (and out of the tests).
///
/// `pub(crate)` rather than `pub(super)` because tests OUTSIDE `plex/` need it too (`route.rs`
/// grades which server a `/:/timeline` POST reaches, which takes two registered clients): the
/// public [`register`] resolves the device id through `session::load`, which MINTS AND PERSISTS a
/// uuid when there is none, and a host test must not write one.
pub(crate) fn register_with_client_id(machine_id: &str, host: &str, port: i32, token: &str, client_id: &str) -> ServerId {
    register_lazy(machine_id, host, port, token, &|| client_id.to_owned())
}

fn register_lazy(machine_id: &str, host: &str, port: i32, token: &str, client_id: &dyn Fn() -> String) -> ServerId {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let n = COUNT.load(Ordering::Acquire);
    let found = (0..n)
        .map(|i| ServerId(i as u16))
        .find(|&id| client_for(id).is_some_and(|c| same_server(c, machine_id, host, port)));

    if let Some(id) = found {
        let c = client_for(id).expect("the matched slot is populated");
        // Keep an id we already know: a legacy address-keyed call must not blank it.
        let mid = if machine_id.is_empty() { c.machine_id() } else { machine_id };
        if c.host() != host || c.port() != port || c.machine_id() != mid {
            // Re-point. The old `Client` stays alive and merely stale for anyone mid-request
            // with it; the fresh one also gets a fresh token generation, so token-baked caches
            // flush without a special case.
            crate::log(&format!("plex: server slot {} re-pointed to {host}:{port}", id.0));
            publish(id, Client::new(id, mid, host, port, token, &client_id()));
        } else {
            c.set_token(token); // in place — every reference already handed out follows along
        }
        return id;
    }

    if n >= MAX_SERVERS {
        crate::log("plex: server registry full — keeping the current server");
        return current();
    }
    let id = ServerId(n as u16);
    publish(id, Client::new(id, machine_id, host, port, token, &client_id()));
    COUNT.store(n + 1, Ordering::Release); // after the pointer: a visible count implies a live slot
    // Address only — the machineIdentifier is a permanent household fingerprint (see `ui::stats`)
    // and the event log is what users send us.
    crate::log(&format!("plex: server slot {} registered at {host}:{port}", id.0));
    if !current().is_set() {
        // Release: this store is what makes the `publish` above reachable through `client()`, so it
        // must not be seen before it (see `current()`).
        CURRENT.store(id.0 as u32, Ordering::Release);
    }
    id
}

/// Install for a (re)login / profile switch — the SESSION path, unchanged signature and
/// unchanged single-server behaviour.
///
/// The caller has an address and no machine id (a stored session, or what the login resolved),
/// so the same address is the same server: a second call for it swaps the token in place exactly
/// as the old singleton did — which is every install this app makes today, and why nothing about
/// a single-server session changed. A call naming a DIFFERENT address now registers a second slot
/// and makes it current, where the singleton kept the FIRST server's host/port and quietly applied
/// the new token to it (a mis-target no caller could see, because host/port were frozen).
pub fn install(host: &str, port: i32, token: &str) {
    let id = register("", host, port, token);
    set_current(id); // the session path always retargets: this is now the server we are using
    // (`register` already refreshed this server's self-description — see its doc.)
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

        // and once known, the id is what identifies it — the same server at a new address
        // re-points that slot rather than appending
        let moved = reg("mach-A", "10.0.0.9", "tok-a");
        assert_eq!(moved, id);
        assert_eq!(count(), 1);
        assert_eq!(client_for(id).unwrap().host(), "10.0.0.9");
    }
}
