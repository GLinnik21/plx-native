//! Device-backed protection for the persisted Plex session, as the storage layer sees it: seal a
//! secret, open it again, drop its key. The platform's key manager is behind the port; the
//! envelope types live here so the on-disk shape does not depend on which platform sealed it.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Keymanager3,
    PalmKeymanager,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Sealed {
    pub backend: Backend,
    pub key: String,
    pub iv: String,
    pub data: String,
}

/// A host test's replacement for the platform key manager, installed per thread.
#[cfg(any(test, feature = "test-support"))]
pub struct TestStore {
    pub seal: fn(&[u8]) -> Option<Sealed>,
    pub open: fn(&Sealed) -> Option<Vec<u8>>,
    pub remove: fn(&Backend, &str),
}

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    pub static STORE_FOR_TEST: std::cell::Cell<Option<&'static TestStore>> =
        const { std::cell::Cell::new(None) };
}

pub fn seal(plain: &[u8]) -> Option<Sealed> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.seal)(plain);
    }
    (super::port().seal)(plain)
}

pub fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.open)(sealed);
    }
    (super::port().open)(sealed)
}

pub fn remove(backend: &Backend, key: &str) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.remove)(backend, key);
    }
    (super::port().remove)(backend, key)
}
