//! Which `Client` a server slot holds, as a plain value.
//!
//! A store's step decides from its inputs, and "is the client I opened this page with still the
//! one the registry holds" is not a read the step makes of the process-global registry
//! (`client_for`). That read is an input, so the host makes it and hands the answer in:
//! [`ClientKeys`] is the registry's answer for every active slot, taken immediately before a step
//! in the same call, and the step looks a slot up with [`ClientKeys::key`].
//!
//! There is deliberately no reader here. The one that fills a [`ClientKeys`] from the live
//! registry is the application's (`app::adapters::stores::refill_client_keys`), beside the IO the
//! answer feeds, so no data-layer step can reach the registry through this type. [`ClientKeys::of`]
//! and [`ClientKeys::from_keys`] build one from values, for the tests that drive a step.
//!
//! A WORKER that holds the snapshot its step decided under reaches other servers through
//! [`KeyedClients`], which takes the registry read as an argument from its host for the same
//! reason (the view-state write's fan-out, `data::viewstate::finish_view_write_job`).

use super::{Client, ServerId};

/// The identity of one registered [`Client`] at one moment.
///
/// `instance` is the client OBJECT: immutable, drawn once from the process-wide generation
/// sequence when the client is built, and every client is leaked and never freed, so two keys
/// with equal `instance` name the same object. `token` is the credential the client held then
/// ([`Client::token_gen`]), which moves on a retoken without replacing the object. `grant` is
/// WHOSE credential it was ([`Client::grant_epoch`]): it survives a live token replaced by another
/// live one and moves across a revocation, so a step that keeps what a server served through a
/// refresh can tell a refresh from a change of identity. Every `grant` move also moves `token`, so
/// equal keys still say "the same client, asking with the same credential".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClientKey {
    pub instance: u32,
    pub token: u32,
    pub grant: u32,
}

impl ClientKey {
    pub fn of(client: &Client) -> Self {
        Self { instance: client.instance_gen(), token: client.token_gen(), grant: client.grant_epoch() }
    }
}

/// The [`ClientKey`] of every server slot that held a client when the value was filled; a slot
/// that is unregistered, unpopulated, retired by a sign-out or [`ServerId::UNSET`] has none.
/// `key(sid)` is `client_for(sid).map(ClientKey::of)` for every `sid`, as of the fill.
///
/// A handful of entries at most, so it is a list searched in order. A host that fills one every
/// frame keeps ONE and refills it in place ([`ClientKeys::refill`]), which keeps its capacity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ClientKeys(Vec<(ServerId, ClientKey)>, Vec<(ServerId, ClientFacts)>, ServerId, u32, u32);

/// Immutable described evidence is separate from Browse's undescribed-owned fallback.
#[derive(Clone)]
pub struct ClientFacts {
    pub machine_id: String,
    pub described: Option<&'static super::ServerFacts>,
    /// None rejects an incoherent observation; Some((None, tier)) means not probed.
    pub source: Option<(Option<super::probe::Outcome>, Option<super::probe::Location>)>,
    pub stream_allowed: bool,
}

impl PartialEq for ClientFacts {
    fn eq(&self, other: &Self) -> bool {
        self.machine_id == other.machine_id && self.source == other.source
                    && self.stream_allowed == other.stream_allowed
            && match (self.described, other.described) {
                (None, None) => true,
                (Some(a), Some(b)) => a.name == b.name && a.handle == b.handle
                    && a.owned == b.owned && a.home == b.home && a.owner_id == b.owner_id,
                _ => false,
            }
    }
}
impl Eq for ClientFacts {}
impl std::fmt::Debug for ClientFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientFacts").field("machine_id", &self.machine_id)
            .field("described", &self.described.is_some()).field("source", &self.source)
                        .field("stream_allowed", &self.stream_allowed).finish()
    }
}
impl Default for ClientKeys {
    fn default() -> Self { Self(Vec::new(), Vec::new(), ServerId::UNSET, 0, 0) }
}

impl ClientKeys {
    /// A complete value observation supplied by a scripted host. Registered slots without
    /// eligible keys remain distinct from absent slots, just as in a live refill.
    pub fn captured(current: ServerId, roster_gen: u32, facts_gen: u32,
        rows: impl IntoIterator<Item = (ServerId, Option<ClientKey>, ClientFacts)>) -> Self {
        let mut keys = Self::default();
        keys.2 = current;
        keys.3 = roster_gen;
        keys.4 = facts_gen;
        for (sid, key, facts) in rows {
            if let Some(key) = key { keys.0.push((sid, key)); }
            keys.1.push((sid, facts));
        }
        keys
    }

    /// The keys of these clients.
    pub fn of<'a>(clients: impl IntoIterator<Item = (ServerId, &'a Client)>) -> Self {
        Self::from_keys(clients.into_iter().map(|(sid, client)| (sid, ClientKey::of(client))))
    }

    /// These keys, as given. A slot named twice answers with its first entry.
    pub fn from_keys(keys: impl IntoIterator<Item = (ServerId, ClientKey)>) -> Self {
        let mut result = Self::default();
        result.refill(keys);
        result
    }

    /// Replace the contents with `keys`, keeping the allocation.
    pub fn refill(&mut self, keys: impl IntoIterator<Item = (ServerId, ClientKey)>) {
        self.0.clear();
        self.0.extend(keys);
        self.1.clear();
        self.2 = ServerId::UNSET;
        self.3 = 0;
        self.4 = 0;
    }

    pub fn current(&self) -> ServerId { self.2 }
    pub fn roster_gen(&self) -> u32 { self.3 }
    pub fn facts_gen(&self) -> u32 { self.4 }
    /// Compatibility key-only constructors have no metadata; never guess the current slot.
    pub fn slots(&self) -> impl Iterator<Item = ServerId> + '_ {
        self.1.iter().map(|(sid, _)| *sid)
            .chain(self.0.iter().filter(|_| self.1.is_empty()).map(|(sid, _)| *sid))
    }
    pub fn facts(&self, sid: ServerId) -> Option<&ClientFacts> {
        self.1.iter().find(|(slot, _)| *slot == sid).map(|(_, facts)| facts)
    }

    /// Replace only scripted key values, retaining the separately captured complete roster.
    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_keys_for_test(&mut self, keys: impl IntoIterator<Item = (ServerId, ClientKey)>) {
        self.0.clear();
        self.0.extend(keys);
    }

    /// Host-only registry capture through explicit readers. Reuses both buffers and machine strings.
    /// Eligibility is read on EVERY fill, irrespective of generations. The acquire-backed probe
    /// reader runs before tier and the second eligible lookup checks instance/token/grant again.
    /// A raced row keeps its registered slot and evidence but has no active key or source pair.
    pub fn refill_roster(
        &mut self, current: ServerId, roster_gen: u32, facts_gen: u32,
        slots: impl IntoIterator<Item = ServerId>,
        lookup: impl FnMut(ServerId) -> Option<&'static Client>,
        describe: impl FnMut(ServerId) -> Option<&'static super::ServerFacts>,
        probe: impl FnMut(ServerId) -> Option<super::probe::Outcome>,
    ) {
        self.refill_roster_with_stream_policy(current, roster_gen, facts_gen, slots, lookup, describe, probe, |_| false);
    }

    pub fn stream_allowed(&self, sid: ServerId) -> bool {
        self.key(sid).is_some() && self.facts(sid).is_some_and(|f| f.stream_allowed)
    }

    pub fn refill_roster_with_stream_policy(
        &mut self, current: ServerId, roster_gen: u32, facts_gen: u32,
        slots: impl IntoIterator<Item = ServerId>,
        mut lookup: impl FnMut(ServerId) -> Option<&'static Client>,
        mut describe: impl FnMut(ServerId) -> Option<&'static super::ServerFacts>,
        mut probe: impl FnMut(ServerId) -> Option<super::probe::Outcome>,
        mut stream_allowed: impl FnMut(&Client) -> bool,
    ) {
        self.0.clear();
        self.2 = current;
        self.3 = roster_gen;
        self.4 = facts_gen;
        let mut count = 0;
        for sid in slots {
            if count == self.1.len() {
                self.1.push((sid, ClientFacts { machine_id: String::new(), described: None, source: None, stream_allowed: false }));
            }
            let row = &mut self.1[count];
            row.0 = sid;
            let described = describe(sid);
            let unchanged = match (row.1.described, described) {
                (None, None) => true,
                (Some(a), Some(b)) => a.name == b.name && a.handle == b.handle
                    && a.owned == b.owned && a.home == b.home && a.owner_id == b.owner_id,
                _ => false,
            };
            if !unchanged { row.1.described = described; }
            row.1.source = None;
            row.1.stream_allowed = false;
            if let Some(client) = lookup(sid) {
                let key = ClientKey::of(client);
                let outcome = probe(sid);
                let tier = client.link();
                let allowed = stream_allowed(client);
                if lookup(sid).is_some_and(|now| ClientKey::of(now) == key)
                    && ClientKey::of(client) == key {
                    self.0.push((sid, key));
                    if row.1.machine_id != client.machine_id() {
                        row.1.machine_id.clear();
                        row.1.machine_id.push_str(client.machine_id());
                    }
                    row.1.source = Some((outcome, tier));
                    row.1.stream_allowed = allowed;
                } else {
                    row.1.machine_id.clear();
                }
            } else {
                row.1.machine_id.clear();
            }
            count += 1;
        }
        self.1.truncate(count);
    }

    /// The key of the client `sid` held, `None` when it held none.
    pub fn key(&self, sid: ServerId) -> Option<ClientKey> {
        self.0.iter().find(|(slot, _)| *slot == sid).map(|&(_, key)| key)
    }

    /// Every slot that held a client, with its key, in fill order.
    pub fn iter(&self) -> impl Iterator<Item = (ServerId, ClientKey)> + '_ {
        self.0.iter().copied()
    }
}

/// Captured clock reasons and the coherent source identities they describe. The host fills this
/// from its already captured client roster; screen code never reads keypin or the registry.
#[derive(Clone, Debug, Default)]
pub struct ClockFacts {
    revision: u64,
    initialized: bool,
    current: ServerId,
    empty: Option<plx_net::net::keypin::Blocked>,
    rows: Vec<ClockRow>,
}

#[derive(Clone, Debug)]
struct ClockRow {
    sid: ServerId,
    machine: String,
    blocked: Option<plx_net::net::keypin::Blocked>,
}

impl ClockFacts {
    /// Explicit value input for a scripted host; never reads a process-global source.
    pub fn scripted(revision: u64, current: ServerId, empty: Option<plx_net::net::keypin::Blocked>,
        rows: impl IntoIterator<Item = (ServerId, String, Option<plx_net::net::keypin::Blocked>)>) -> Self {
        Self { revision, initialized: true, current, empty,
            rows: rows.into_iter().map(|(sid, machine, blocked)| ClockRow { sid, machine, blocked }).collect() }
    }

    pub fn revision(&self) -> u64 { self.revision }
    pub fn current_machine(&self) -> Option<&str> { self.machine(self.current) }
    pub fn machine(&self, sid: ServerId) -> Option<&str> {
        self.rows.iter().find(|row| row.sid == sid).map(|row| row.machine.as_str())
    }
    pub fn blocked_for(&self, machine: Option<&str>) -> Option<plx_net::net::keypin::Blocked> {
        match machine {
            None | Some("") => self.empty,
            Some(machine) => self.rows.iter().find(|row| row.machine == machine).and_then(|row| row.blocked),
        }
    }

    /// Refresh only when the revision or the coherent source roster changed. The retained vector
    /// and its strings are reused. `read` is supplied by the host, not by this value.
    /// The cheap, allocation-free guard before touching a retained publication.
    pub fn needs_refill(&self, revision: u64, keys: &ClientKeys) -> bool {
        let sources = || keys.iter().filter_map(|(sid, _)| keys.facts(sid).map(|facts| (sid, facts)));
        !self.initialized || self.revision != revision || self.current != keys.current()
            || self.rows.len() != sources().count()
            || !self.rows.iter().zip(sources()).all(|(row, (sid, facts))|
                row.sid == sid && row.machine == facts.machine_id)
    }
    pub fn refill(&mut self, revision: u64, keys: &ClientKeys,
        mut read: impl FnMut(&str) -> Option<plx_net::net::keypin::Blocked>) -> bool {
        if !self.needs_refill(revision, keys) { return false; }
        let mut changed = self.current != keys.current();
        let empty = read("");
        changed |= self.empty != empty;
        self.empty = empty;
        let mut count = 0;
        for (sid, _) in keys.iter() {
            let Some(facts) = keys.facts(sid) else { continue };
            if count == self.rows.len() {
                self.rows.push(ClockRow { sid, machine: String::new(), blocked: None });
                changed = true;
            }
            let row = &mut self.rows[count];
            changed |= row.sid != sid || row.machine != facts.machine_id;
            row.sid = sid;
            if row.machine != facts.machine_id {
                row.machine.clear();
                row.machine.push_str(&facts.machine_id);
            }
            let blocked = read(&row.machine);
            changed |= row.blocked != blocked;
            row.blocked = blocked;
            count += 1;
        }
        changed |= self.rows.len() != count;
        self.rows.truncate(count);
        self.current = keys.current();
        self.revision = revision;
        self.initialized = true;
        changed
    }
}

/// A key-checked way to a server's client, for a WORKER that holds the snapshot its step decided
/// under. A step decides from a [`ClientKeys`] taken just before it; the worker runs later, and a
/// profile switch re-tokens the registry's `Client` objects IN PLACE (`servers::revoke_for_profile_switch`):
/// `instance` stays and `token` moves. A worker that read the registry again would send the
/// request under the NEW profile's credential, to a server the old profile's step chose.
///
/// [`KeyedClients::client`] answers a slot only while the registry's client still carries the key
/// the snapshot holds. The check covers the moment a request is BUILT; a swap while a request is
/// already on the wire is not covered, the same limit as the runner's own launch check.
///
/// The registry read itself is the `lookup` the host passes in (the application passes
/// `plex::client_for`), so building a [`KeyedClients`] names no registry and a data-layer worker
/// that holds one reads none. `lookup` is a `fn` pointer, which is `Send + Copy`: the value can be
/// built inside a worker, from the job's own snapshot, and cannot capture the caller's stack.
#[derive(Clone, Copy)]
pub struct KeyedClients<'a> {
    keys: &'a ClientKeys,
    lookup: fn(ServerId) -> Option<&'static Client>,
}

impl<'a> KeyedClients<'a> {
    pub fn new(keys: &'a ClientKeys, lookup: fn(ServerId) -> Option<&'static Client>) -> Self {
        Self { keys, lookup }
    }

    /// Described evidence from the captured roster, never the live registry.
    pub fn facts(&self, sid: ServerId) -> Option<&ClientFacts> { self.keys.facts(sid) }

    /// The client `sid` holds, only while its key matches the snapshot. `None` when the snapshot
    /// holds no key for `sid` (the step never saw that slot), when the registry holds no client
    /// for it, or when the client it holds is a different object or carries a moved credential.
    pub fn client(&self, sid: ServerId) -> Option<&'static Client> {
        let key = self.keys.key(sid)?;
        (self.lookup)(sid).filter(|client| ClientKey::of(client) == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::Origin;

    fn a_client(slot: u16, token: &str) -> Client {
        Client::new(ServerId::from_raw(slot), "mach", Origin::http("10.0.0.1", 32400), token, "cid-42")
    }

    #[test]
    fn a_key_names_the_client_and_its_credential() {
        let (a, b) = (a_client(1, "tok-a"), a_client(2, "tok-b"));
        let before = ClientKey::of(&a);
        assert_eq!(before, ClientKey::of(&a), "the same client, the same credential");
        assert_ne!(before.instance, ClientKey::of(&b).instance, "two clients are two instances");

        a.set_token("tok-a2");
        let after = ClientKey::of(&a);
        assert_eq!(after.instance, before.instance, "a token swap does not replace the client");
        assert_ne!(after.token, before.token, "…but it moves the credential");
        assert_eq!(after.grant, before.grant, "…not whose it is: a live token replaced by a live one");
        assert_ne!(after, before);

        a.set_token("");
        assert_ne!(ClientKey::of(&a).grant, after.grant, "a revocation changes whose credential it is");
    }

    #[test]
    fn a_slot_answers_with_its_own_key_and_a_slot_without_a_client_with_none() {
        let (a, b) = (a_client(1, "tok-a"), a_client(2, "tok-b"));
        let keys = ClientKeys::of([(ServerId::from_raw(1), &a), (ServerId::from_raw(2), &b)]);
        assert_eq!(keys.key(ServerId::from_raw(1)), Some(ClientKey::of(&a)));
        assert_eq!(keys.key(ServerId::from_raw(2)), Some(ClientKey::of(&b)));
        assert_eq!(keys.key(ServerId::from_raw(3)), None, "never registered");
        assert_eq!(keys.key(ServerId::UNSET), None, "the reserved slot has no client");
        assert_eq!(ClientKeys::default().key(ServerId::from_raw(1)), None);
    }

    #[test]
    fn a_key_given_for_a_slot_is_answered_as_given_and_the_first_entry_wins() {
        let (first, second) = (ClientKey { instance: 2, token: 1, grant: 1 }, ClientKey { instance: 3, token: 1, grant: 1 });
        let keys = ClientKeys::from_keys([(ServerId::UNSET, first), (ServerId::UNSET, second)]);
        assert_eq!(keys.key(ServerId::UNSET), Some(first), "from_keys does not special-case any slot");
    }

    #[test]
    fn a_refill_replaces_the_contents_in_place() {
        let (old, new) = (ClientKey { instance: 1, token: 1, grant: 1 }, ClientKey { instance: 2, token: 5, grant: 1 });
        let mut keys = ClientKeys::from_keys((0..8).map(|i| (ServerId::from_raw(i), old)));
        let capacity = keys.0.capacity();
        keys.refill([(ServerId::from_raw(0), new)]);
        assert_eq!(keys, ClientKeys::from_keys([(ServerId::from_raw(0), new)]));
        assert_eq!(keys.key(ServerId::from_raw(1)), None, "an entry the refill did not name is gone");
        assert_eq!(keys.0.capacity(), capacity, "the allocation is reused, not replaced");
    }

    // The registry as a test double: a `fn` pointer cannot capture, so the slots live in a static.
    // Each test uses slots of its own, so the parallel tests never read each other's.
    static SLOTS: std::sync::Mutex<Vec<(ServerId, &'static Client)>> = std::sync::Mutex::new(Vec::new());

    fn registry(sid: ServerId) -> Option<&'static Client> {
        SLOTS.lock().unwrap().iter().find(|(slot, _)| *slot == sid).map(|&(_, client)| client)
    }

    fn register(slot: u16, token: &str) -> (ServerId, &'static Client) {
        let sid = ServerId::from_raw(slot);
        let client: &'static Client = Box::leak(Box::new(a_client(slot, token)));
        let mut slots = SLOTS.lock().unwrap();
        slots.retain(|(held, _)| *held != sid);
        slots.push((sid, client));
        (sid, client)
    }

    #[test]
    fn a_slot_whose_key_matches_answers_with_its_client() {
        let (sid, client) = register(901, "tok-a");
        let keys = ClientKeys::of([(sid, client)]);
        let found = KeyedClients::new(&keys, registry).client(sid).expect("the key matches");
        assert!(std::ptr::eq(found, client), "the registry's own client, not a copy");
    }

    #[test]
    fn a_slot_whose_credential_moved_after_the_snapshot_answers_with_none() {
        let (sid, client) = register(902, "tok-a");
        let keys = ClientKeys::of([(sid, client)]);
        let clients = KeyedClients::new(&keys, registry);
        assert!(clients.client(sid).is_some(), "before the swap");
        client.set_token("tok-b"); // a profile switch re-tokens the same object in place
        assert!(clients.client(sid).is_none(), "the same object under another credential is not the client the step chose");
    }

    #[test]
    fn a_slot_the_snapshot_never_held_answers_with_none() {
        let (held, client) = register(903, "tok-a");
        let (unseen, _) = register(904, "tok-b");
        let keys = ClientKeys::of([(held, client)]);
        let clients = KeyedClients::new(&keys, registry);
        assert!(clients.client(unseen).is_none(), "registered after the snapshot: not the step's to use");
        assert!(clients.client(ServerId::UNSET).is_none());
        assert!(clients.client(ServerId::from_raw(905)).is_none(), "never registered");
    }

    #[test]
    fn a_slot_re_pointed_at_a_new_client_after_the_snapshot_answers_with_none() {
        let (sid, old) = register(906, "tok-a");
        let keys = ClientKeys::of([(sid, old)]);
        let (_, replaced) = register(906, "tok-a"); // same slot, same token text, a different object
        assert!(!std::ptr::eq(old, replaced));
        assert!(KeyedClients::new(&keys, registry).client(sid).is_none(), "the instance moved");
    }
}

#[cfg(test)]
#[path = "client_keys_roster_tests.rs"]
mod roster_tests;

#[cfg(test)]
mod clock_facts_tests {
    use super::*;
    use plx_net::net::keypin::Blocked;
    use std::sync::Arc;

    #[test]
    fn retained_clock_facts_reuse_arc_buffers_and_machine_strings() {
        let sid = ServerId::from_raw(907);
        let client: &'static Client = Box::leak(Box::new(Client::new(sid, "clock-captured-machine",
            super::super::Origin::http("127.0.0.1", 9), "token", "clock-test")));
        let mut keys = ClientKeys::default();
        keys.refill_roster(sid, 1, 1, [sid], |_| Some(client), |_| None, |_| None);
        let mut facts = Arc::new(ClockFacts::default());
        assert!(Arc::make_mut(&mut facts).refill(1, &keys, |id|
            (id == "clock-captured-machine").then_some(Blocked::NoKey)));
        let published = Arc::clone(&facts);
        let buffer = facts.rows.as_ptr();
        let machine = facts.rows[0].machine.as_ptr();
        let capacities = (facts.rows.capacity(), facts.rows[0].machine.capacity());
        for _ in 0..20 {
            assert!(!facts.needs_refill(1, &keys));
            assert!(Arc::ptr_eq(&published, &facts));
            assert_eq!(facts.rows.as_ptr(), buffer);
            assert_eq!(facts.rows[0].machine.as_ptr(), machine);
        }
        drop(published);
        assert!(!Arc::make_mut(&mut facts).refill(2, &keys, |id|
            (id == "clock-captured-machine").then_some(Blocked::NoKey)));
        assert_eq!(facts.rows.as_ptr(), buffer);
        assert_eq!(facts.rows[0].machine.as_ptr(), machine);
        assert_eq!((facts.rows.capacity(), facts.rows[0].machine.capacity()), capacities);
        assert_eq!(facts.current_machine(), Some("clock-captured-machine"));
        assert_eq!(facts.blocked_for(Some("clock-captured-machine")), Some(Blocked::NoKey));
        assert_eq!(facts.blocked_for(None), None);
    }

    #[test]
    fn scripted_empty_id_fact_is_explicit() {
        let facts = ClockFacts::scripted(3, ServerId::UNSET, Some(Blocked::KeyChanged), []);
        assert_eq!(facts.blocked_for(None), Some(Blocked::KeyChanged));
        assert_eq!(facts.blocked_for(Some("")), Some(Blocked::KeyChanged));
        assert_eq!(facts.blocked_for(Some("other")), None);
    }
}
