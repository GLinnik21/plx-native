//! Device-backed protection for the persisted Plex session, as the storage layer sees it: seal a
//! secret, open it again, drop its key. The platform's key manager is behind the port; the
//! envelope types live here so the on-disk shape does not depend on which platform sealed it.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Backend {
    Keymanager3,
    PalmKeymanager,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Sealed {
    pub backend: Backend,
    pub key: String,
    pub iv: String,
    pub data: String,
}

/// A host test's replacement for the platform key manager, installed per thread.
#[cfg(test)]
pub(crate) struct TestStore {
    pub(crate) seal: fn(&[u8]) -> Option<Sealed>,
    pub(crate) open: fn(&Sealed) -> Option<Vec<u8>>,
    pub(crate) remove: fn(&Backend, &str),
}

#[cfg(test)]
std::thread_local! {
    pub(crate) static STORE_FOR_TEST: std::cell::Cell<Option<&'static TestStore>> =
        const { std::cell::Cell::new(None) };
}

pub(crate) fn seal(plain: &[u8]) -> Option<Sealed> {
    #[cfg(test)]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.seal)(plain);
    }
    (super::port().seal)(plain)
}

pub(crate) fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    #[cfg(test)]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.open)(sealed);
    }
    (super::port().open)(sealed)
}

pub(crate) fn remove(backend: &Backend, key: &str) {
    #[cfg(test)]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.remove)(backend, key);
    }
    (super::port().remove)(backend, key)
}
