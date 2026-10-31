//! Which `Client` a server slot holds, as a plain value.
//!
//! A store's step decides from its inputs, and "is the client I opened this page with still the
//! one the registry holds" is not a read the step makes of the process-global registry
//! (`client_for`). That read is an input, so the host makes it and hands the answer in:
//! [`ClientKeys`] is the registry's answer for every active slot, taken immediately before a step
//! in the same call. The step looks a slot up with [`ClientKeys::key`] (the client and its
//! credential generation) or, when it must tell a refreshed token from a new asker,
//! [`ClientKeys::held`] ([`HeldClient`]: the key plus the grant epoch the credential spoke for).
//!
//! There is deliberately no reader here. The one that fills a [`ClientKeys`] from the live
//! registry is the application's (`app::adapters::stores::refill_client_keys`), beside the IO the
//! answer feeds, so no data-layer step can reach the registry through this type. [`ClientKeys::of`]
//! builds one from clients, for the tests that drive a step; `ClientKeys::from_keys` (test-support
//! only) builds one from bare keys, whose grants are unproven.
//!
//! A WORKER that holds the snapshot its step decided under reaches other servers through
//! [`KeyedClients`], which takes the registry read as an argument from its host for the same
//! reason (the view-state write's fan-out, `data::viewstate::finish_view_write_job`).

use super::{Client, Credential, ServerId};

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
    /// The key of `client` now. `token` and `grant` are read as ONE [`Client::credential`], so a
    /// reading never pairs a moved token with the previous identity; a poisoned token lock falls
    /// back to the two separate reads.
    pub fn of(client: &Client) -> Self {
        let instance = client.instance_gen();
        match client.credential() {
            Some(Credential { token_gen, grant_epoch }) => Self { instance, token: token_gen, grant: grant_epoch },
            None => Self { instance, token: client.token_gen(), grant: client.grant_epoch() },
        }
    }
}

/// A [`ClientKey`] plus the IDENTITY its credential spoke for ([`Client::grant_epoch`]), as one
/// reading of a client: what a step needs to tell "the token was refreshed" from "someone else is
/// asking now". `key` alone cannot: a live-to-live refresh and a profile switch both move
/// `key.token`; only a switch (or a sign-out, a withdrawn plaintext grant, a foreign retoken)
/// passes a revocation edge and moves the grant epoch.
///
/// `grant` is `None` when the epoch is UNPROVEN (a key built from values, or a poisoned token
/// lock): an unproven grant is never taken for the same asker unless nothing moved at all
/// ([`HeldClient::same_asker`]). The fields are public, like [`ClientKey`]'s, so a fixture can
/// write one as a literal.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HeldClient {
    pub key: ClientKey,
    pub grant: Option<u32>,
}

impl HeldClient {
    /// The key and the grant epoch of `client`, from ONE [`Client::credential`] call: the token
    /// generation and the epoch of one edit, read under the client's token lock, so a step cannot
    /// pair a moved token with the previous identity. Two separate reads would reopen exactly that
    /// tear, so this calls `credential()` once and destructures it.
    pub fn of(client: &Client) -> Self {
        let instance = client.instance_gen();
        match client.credential() {
            Some(Credential { token_gen, grant_epoch }) => Self {
                key: ClientKey { instance, token: token_gen, grant: grant_epoch },
                grant: Some(grant_epoch),
            },
            // A poisoned token lock: no proof of the identity, so none is claimed.
            None => Self { key: ClientKey::of(client), grant: None },
        }
    }

    /// Is `self` the same ASKER as `then`: the same client object, and either nothing moved at all
    /// or both grants are proven and equal (a token refreshed in place, no revocation edge between
    /// the two readings). **An unproven grant never makes a moved token a refresh**: fail closed.
    pub fn same_asker(self, then: HeldClient) -> bool {
        self.key.instance == then.key.instance
            && (self == then || matches!((self.grant, then.grant), (Some(a), Some(b)) if a == b))
    }
}

/// The [`HeldClient`] of every server slot that held a client when the value was filled; a slot
/// that is unregistered, unpopulated, retired by a sign-out or [`ServerId::UNSET`] has none.
/// `key(sid)` is `client_for(sid).map(ClientKey::of)` for every `sid`, as of the fill.
///
/// A handful of entries at most, so it is a list searched in order. A host that fills one every
/// frame keeps ONE and refills it in place ([`ClientKeys::refill_of`]), which keeps its capacity.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct ClientKeys(Vec<(ServerId, HeldClient)>);

impl ClientKeys {
    /// The keys, with their grants, of these clients.
    pub fn of<'a>(clients: impl IntoIterator<Item = (ServerId, &'a Client)>) -> Self {
        Self(clients.into_iter().map(|(sid, client)| (sid, HeldClient::of(client))).collect())
    }

    /// Replace the contents with these clients' readings, keeping the allocation.
    pub fn refill_of<'a>(&mut self, clients: impl IntoIterator<Item = (ServerId, &'a Client)>) {
        self.0.clear();
        self.0.extend(clients.into_iter().map(|(sid, client)| (sid, HeldClient::of(client))));
    }

    /// These keys, as given, with an UNPROVEN grant. A slot named twice answers with its first entry.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_keys(keys: impl IntoIterator<Item = (ServerId, ClientKey)>) -> Self {
        Self(keys.into_iter().map(|(sid, key)| (sid, HeldClient { key, grant: None })).collect())
    }

    /// [`ClientKeys::refill_of`] for values: keys as given, grant unproven.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refill(&mut self, keys: impl IntoIterator<Item = (ServerId, ClientKey)>) {
        self.0.clear();
        self.0.extend(keys.into_iter().map(|(sid, key)| (sid, HeldClient { key, grant: None })));
    }

    /// `self` with the grant of the slot `sid` proven as `grant`. A slot not held is left alone.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_grant(mut self, sid: ServerId, grant: u32) -> Self {
        for (slot, held) in &mut self.0 {
            if *slot == sid {
                held.grant = Some(grant);
            }
        }
        self
    }

    /// The client `sid` held and the identity it spoke for, `None` when it held none.
    pub fn held(&self, sid: ServerId) -> Option<HeldClient> {
        self.0.iter().find(|(slot, _)| *slot == sid).map(|&(_, held)| held)
    }

    /// Every slot named, in order (a slot named twice appears twice).
    pub fn slots(&self) -> impl Iterator<Item = ServerId> + '_ {
        self.0.iter().map(|&(sid, _)| sid)
    }

    /// The key of the client `sid` held, `None` when it held none.
    pub fn key(&self, sid: ServerId) -> Option<ClientKey> {
        self.held(sid).map(|held| held.key)
    }

    /// The slots of `self` that `seen` held TOO and whose client is still the same asker
    /// ([`HeldClient::same_asker`]), each with `self`'s reading: the NOW keys, never `seen`'s, so a
    /// slot refreshed in place passes the full-key check a worker makes against this value.
    /// For the fan-out of a write queued under `seen`: another server is reached only while it is
    /// the asker the press saw.
    ///
    /// **A slot `seen` did not hold is omitted (fail closed).** A roster listed under another
    /// account's token allocates a NEW slot and client carrying the foreign token with no
    /// revocation edge to compare, and a slot admitted blank to live after the capture moved its
    /// grant; neither can be proven to speak for the asker that pressed. Duplicate slot entries
    /// answer with their first (the [`ClientKeys::held`] rule): each DISTINCT slot is judged by
    /// `self.held(sid)`, so an omitted first entry is not resurrected by a later duplicate.
    pub fn without_changed_hands(&self, seen: &ClientKeys) -> ClientKeys {
        let mut kept = Vec::new();
        for (i, &(sid, _)) in self.0.iter().enumerate() {
            if self.0[..i].iter().any(|&(earlier, _)| earlier == sid) {
                continue; // judged with its first entry
            }
            let Some(now) = self.held(sid) else { continue };
            if seen.held(sid).is_some_and(|then| now.same_asker(then)) {
                kept.push((sid, now));
            }
        }
        ClientKeys(kept)
    }

    /// Every slot that held a client, with its key, in fill order.
    pub fn iter(&self) -> impl Iterator<Item = (ServerId, ClientKey)> + '_ {
        self.0.iter().map(|&(sid, held)| (sid, held.key))
    }
}

/// A key-checked way to a server's client, for a WORKER that holds the snapshot its step decided
/// under. A step decides from a [`ClientKeys`] taken just before it; the worker runs later, and a
/// profile switch re-tokens the registry's `Client` objects IN PLACE (`servers::revoke_for_profile_switch`):
/// `instance` stays and `token` moves. A worker that read the registry again would send the
/// request under the NEW profile's credential, to a server the old profile's step chose.
///
/// [`KeyedClients::client`] answers a slot only while the registry's client still carries the key
/// the snapshot holds. (The snapshot a view-state write carries is already filtered by its step:
/// [`ClientKeys::without_changed_hands`] leaves out every slot that changed hands since the press
/// or that the press did not see, so such a slot answers `None` here too.) The check covers the moment a request is BUILT; a swap while a request is
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

    // ---- the grant: which identity a credential spoke for --------------------------------------

    /// A key written as a value; `ClientKey::grant` is 0 throughout (the `HeldClient::grant` the
    /// test gives a slot is the identity these tests judge by).
    fn key(instance: u32, token: u32) -> ClientKey {
        ClientKey { instance, token, grant: 0 }
    }

    fn sid(n: u16) -> ServerId {
        ServerId::from_raw(n)
    }

    #[test]
    fn a_key_set_carries_the_grant_of_each_client() {
        let (a, b) = (a_client(1, "tok-a"), a_client(2, "tok-b"));
        let keys = ClientKeys::of([(sid(1), &a), (sid(2), &b)]);
        for (slot, client) in [(sid(1), &a), (sid(2), &b)] {
            let held = keys.held(slot).expect("held");
            assert_eq!(held.key, ClientKey::of(client), "the key is the client's own");
            assert_eq!(held.grant, Some(client.grant_epoch()), "and the grant is the client's epoch, not its token generation");
        }
        assert_eq!(keys.held(sid(3)), None);

        // a live token replaced by another live one keeps the identity and moves the key
        let before = HeldClient::of(&a);
        a.set_token("tok-a2");
        let refreshed = HeldClient::of(&a);
        assert_ne!(refreshed.key, before.key);
        assert_eq!(refreshed.grant, before.grant, "a refresh is the same identity");
        assert!(refreshed.same_asker(before) && before.same_asker(refreshed));

        // a revocation edge (live to blank to live) moves both
        a.set_token("");
        a.set_token("tok-a3");
        let revoked = HeldClient::of(&a);
        assert_ne!(revoked.key.token, refreshed.key.token);
        assert_ne!(revoked.grant, refreshed.grant, "a blank edge is a new identity");
        assert!(!revoked.same_asker(refreshed));
        assert!(!revoked.same_asker(before));
    }

    #[test]
    fn same_asker_is_true_only_for_one_client_object_and_a_proven_unmoved_grant() {
        let held = |instance, token, grant| HeldClient { key: key(instance, token), grant };
        // (then, now, same asker?)
        let table = [
            // nothing moved at all: the same asker, proven or not
            (held(1, 1, None), held(1, 1, None), true),
            (held(1, 1, Some(7)), held(1, 1, Some(7)), true),
            // the token moved, the proven grant did not: a refresh
            (held(1, 1, Some(7)), held(1, 2, Some(7)), true),
            // the grant moved: someone else is asking
            (held(1, 1, Some(7)), held(1, 2, Some(8)), false),
            (held(1, 1, Some(7)), held(1, 1, Some(8)), false),
            // an unproven grant never turns a moved token into a refresh
            (held(1, 1, None), held(1, 2, None), false),
            (held(1, 1, Some(7)), held(1, 2, None), false),
            (held(1, 1, None), held(1, 2, Some(7)), false),
            (held(1, 1, Some(7)), held(1, 1, None), false),
            (held(1, 1, None), held(1, 1, Some(7)), false),
            // another client object is another asker, whatever the grants say
            (held(1, 1, Some(7)), held(2, 1, Some(7)), false),
            (held(1, 1, Some(7)), held(2, 2, Some(7)), false),
            (held(1, 1, None), held(2, 1, None), false),
        ];
        for (then, now, same) in table {
            assert_eq!(now.same_asker(then), same, "now {now:?} against then {then:?}");
            assert_eq!(then.same_asker(now), same, "then {then:?} against now {now:?}");
        }
    }

    /// The equality the rest of the code leans on: wherever the grant moved, the key moved with
    /// it, so every other `ClientKey ==` consumer sees what it saw before the grant was carried.
    #[test]
    fn grant_moved_implies_key_moved() {
        let c = a_client(7, "tok");
        let mut last = HeldClient::of(&c);
        let edits: [&dyn Fn(&Client); 8] = [
            &|c| c.set_token("tok"),
            &|c| c.set_token("other"),
            &|c| c.set_token(""),
            &|c| c.set_token(""),
            &|c| c.set_token("again"),
            &|c| assert!(!c.revoke_unless_same("again")),
            &|c| assert!(c.revoke_unless_same("someone-else")),
            &|c| c.set_token("again"),
        ];
        for edit in edits {
            edit(&c);
            let now = HeldClient::of(&c);
            if now.grant != last.grant {
                assert_ne!(now.key, last.key, "a moved grant came with an unmoved key");
            }
            last = now;
        }
    }

    #[test]
    fn a_refill_of_clients_replaces_the_contents_in_place() {
        let (a, b) = (a_client(1, "tok-a"), a_client(2, "tok-b"));
        let mut keys = ClientKeys::of((0..8).map(|i| (sid(i), if i % 2 == 0 { &a } else { &b })));
        let capacity = keys.0.capacity();
        keys.refill_of([(sid(0), &b)]);
        assert_eq!(keys, ClientKeys::of([(sid(0), &b)]));
        assert_eq!(keys.held(sid(1)), None, "an entry the refill did not name is gone");
        assert_eq!(keys.0.capacity(), capacity, "the allocation is reused, not replaced");
    }

    /// The whole guarantee of reading the pair under the lock lives in `HeldClient::of`, because
    /// that is the one reader a step uses: while a writer is inside its critical section the
    /// reading waits for it.
    #[test]
    fn held_client_of_is_read_under_the_token_lock() {
        let c = a_client(1, "tok");
        let writer = c.token.write().unwrap();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let c = &c;
            scope.spawn(move || tx.send(HeldClient::of(c)).unwrap());
            assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
                "the reading returned while a writer held the token lock");
            drop(writer);
            let held = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the reading finishes once the writer is done");
            assert_eq!(held, HeldClient::of(c));
        });
    }

    /// `ClientKey::of` is the reader `KeyedClients::client` checks a snapshot against: it reads the
    /// pair under the lock too, so a worker never compares a torn key.
    #[test]
    fn client_key_of_is_read_under_the_token_lock() {
        let c = a_client(1, "tok");
        let writer = c.token.write().unwrap();
        std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::channel();
            let c = &c;
            scope.spawn(move || tx.send(ClientKey::of(c)).unwrap());
            assert!(rx.recv_timeout(std::time::Duration::from_millis(100)).is_err(),
                "the key was read while a writer held the token lock");
            drop(writer);
            let key = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the reading finishes once the writer is done");
            assert_eq!(key, ClientKey::of(c));
        });
    }

    #[test]
    fn a_poisoned_token_lock_gives_a_key_with_no_proven_grant() {
        let c = a_client(1, "tok");
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = c.token.write().unwrap();
            panic!("poisoning the token lock on purpose");
        }));
        let held = HeldClient::of(&c);
        assert_eq!(held.grant, None);
        assert_eq!(held.key, ClientKey::of(&c));
        assert!(!HeldClient { key: ClientKey { token: held.key.token + 1, ..held.key }, grant: None }.same_asker(held),
            "an unproven reading never matches a moved one");
    }

    // ---- the fan-out snapshot: which slots a queued write may still reach ----------------------

    /// `seen`: slots 1..=5 as a press captured them, all proven (grant 10 + slot). `now`: slot 1
    /// refreshed in place (key moved, grant kept), slot 2 changed hands (grant moved), slot 3 the
    /// same grant on ANOTHER client object, slot 4 an unproven reading with a moved key, slot 5
    /// untouched, slot 6 not held at the capture.
    #[test]
    fn a_fan_out_snapshot_omits_a_slot_that_changed_hands_and_keeps_one_refreshed_in_place() {
        let seen = ClientKeys::from_keys((1..=5).map(|n| (sid(n), key(u32::from(n), 1))))
            .with_grant(sid(1), 11).with_grant(sid(2), 12).with_grant(sid(3), 13)
            .with_grant(sid(4), 14).with_grant(sid(5), 15);
        let now = ClientKeys::from_keys([
            (sid(1), key(1, 2)), (sid(2), key(2, 2)), (sid(3), key(99, 1)), (sid(4), key(4, 2)),
            (sid(5), key(5, 1)), (sid(6), key(6, 1)),
        ])
        .with_grant(sid(1), 11).with_grant(sid(2), 120).with_grant(sid(3), 13)
        .with_grant(sid(5), 15).with_grant(sid(6), 16);

        let kept = now.without_changed_hands(&seen);

        assert_eq!(kept.held(sid(1)), now.held(sid(1)), "refreshed in place: kept, under the NOW key and not the captured one");
        assert_ne!(kept.held(sid(1)), seen.held(sid(1)));
        assert_eq!(kept.held(sid(2)), None, "changed hands: its grant moved");
        assert_eq!(kept.held(sid(3)), None, "an equal grant on another client object is another asker");
        assert_eq!(kept.held(sid(4)), None, "an unproven reading is not a refresh");
        assert_eq!(kept.held(sid(5)), now.held(sid(5)), "untouched: kept");
        assert_eq!(kept.held(sid(6)), None, "not held at the capture: nothing proves it is the same asker");
        assert_eq!(kept.0.len(), 2, "and nothing else");
    }

    #[test]
    fn a_fan_out_snapshot_omits_a_slot_the_capture_did_not_hold() {
        let seen = ClientKeys::from_keys([(sid(1), key(1, 1))]).with_grant(sid(1), 5);
        let now = ClientKeys::from_keys([(sid(1), key(1, 1)), (sid(2), key(2, 1))])
            .with_grant(sid(1), 5).with_grant(sid(2), 6);
        let kept = now.without_changed_hands(&seen);
        assert_eq!(kept.held(sid(1)), now.held(sid(1)));
        assert_eq!(kept.held(sid(2)), None, "registered after the capture");
        assert!(now.without_changed_hands(&ClientKeys::default()).0.is_empty(), "an empty capture omits everything");
    }

    /// A slot named twice answers with its first entry; a later duplicate must not bring back a
    /// slot whose first entry was omitted.
    #[test]
    fn a_fan_out_snapshot_judges_a_duplicated_slot_by_its_first_entry() {
        let seen = ClientKeys::from_keys([(sid(1), key(1, 1))]).with_grant(sid(1), 5);
        let now = ClientKeys::from_keys([(sid(1), key(9, 1)), (sid(1), key(1, 1))]).with_grant(sid(1), 5);
        let kept = now.without_changed_hands(&seen);
        assert_eq!(kept.held(sid(1)), None, "the first entry is another client object");
        assert!(kept.0.is_empty(), "the later duplicate is not emitted either");
    }
}
