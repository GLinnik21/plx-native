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

use super::{Client, ServerId};

/// The identity of one registered [`Client`] at one moment.
///
/// `instance` is the client OBJECT: immutable, drawn once from the process-wide generation
/// sequence when the client is built, and every client is leaked and never freed, so two keys
/// with equal `instance` name the same object. `token` is the credential the client held then
/// ([`Client::token_gen`]), which moves on a retoken without replacing the object. Equal keys
/// therefore say "the same client, asking with the same credential".
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClientKey {
    pub instance: u32,
    pub token: u32,
}

impl ClientKey {
    pub fn of(client: &Client) -> Self {
        Self { instance: client.instance_gen(), token: client.token_gen() }
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
        assert_ne!(after, before);
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
        let (first, second) = (ClientKey { instance: 2, token: 1 }, ClientKey { instance: 3, token: 1 });
        let keys = ClientKeys::from_keys([(ServerId::UNSET, first), (ServerId::UNSET, second)]);
        assert_eq!(keys.key(ServerId::UNSET), Some(first), "from_keys does not special-case any slot");
    }

    #[test]
    fn a_refill_replaces_the_contents_in_place() {
        let (old, new) = (ClientKey { instance: 1, token: 1 }, ClientKey { instance: 2, token: 5 });
        let mut keys = ClientKeys::from_keys((0..8).map(|i| (ServerId::from_raw(i), old)));
        let capacity = keys.0.capacity();
        keys.refill([(ServerId::from_raw(0), new)]);
        assert_eq!(keys, ClientKeys::from_keys([(ServerId::from_raw(0), new)]));
        assert_eq!(keys.key(ServerId::from_raw(1)), None, "an entry the refill did not name is gone");
        assert_eq!(keys.0.capacity(), capacity, "the allocation is reused, not replaced");
    }
}
