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
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ClientKeys(Vec<(ServerId, ClientKey)>);

impl ClientKeys {
    /// The keys of these clients.
    pub fn of<'a>(clients: impl IntoIterator<Item = (ServerId, &'a Client)>) -> Self {
        Self::from_keys(clients.into_iter().map(|(sid, client)| (sid, ClientKey::of(client))))
    }

    /// These keys, as given. A slot named twice answers with its first entry.
    pub fn from_keys(keys: impl IntoIterator<Item = (ServerId, ClientKey)>) -> Self {
        Self(keys.into_iter().collect())
    }

    /// Replace the contents with `keys`, keeping the allocation.
    pub fn refill(&mut self, keys: impl IntoIterator<Item = (ServerId, ClientKey)>) {
        self.0.clear();
        self.0.extend(keys);
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
