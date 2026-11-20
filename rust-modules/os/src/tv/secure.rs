//! Device-backed protection for the persisted Plex session, as the storage layer sees it: seal a
//! secret, open it again, drop its key. The platform's key manager is behind the port; the
//! envelope types live here so the on-disk shape does not depend on which platform sealed it.
//! Beside them, [`StoreMode`] says whether this platform keeps a credential across launches at
//! all.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

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

/// Whether this platform keeps the signed-in session across launches.
///
/// `Durable` is every platform that has a credential store: the session is written to it and read
/// back at the next launch. `MemoryOnly` is a platform with none the app may use: the session
/// lives in this process alone and no credential is ever written, so the next launch asks for a
/// sign-in again. What still persists there is the install's own choices (its client id, the
/// language and the Interface resolution), which hold no credential and cannot authorize a login.
///
/// A [`seal`] that answers `None` is NOT `MemoryOnly`: the session layer then writes a plaintext
/// file of its own, so only this value says "no credential on disk".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StoreMode {
    #[default]
    Durable,
    MemoryOnly,
}

/// The mode the platform published at startup, set once. Unset reads as [`StoreMode::Durable`],
/// which is what every platform before this value existed was.
static MODE: OnceLock<StoreMode> = OnceLock::new();

/// Publish the platform's mode before the first session read. Once; later calls are ignored.
pub fn publish_mode(mode: StoreMode) {
    let _ = MODE.set(mode);
}

/// The persistence mode of this process: what the session layer reads before it touches a store.
pub fn mode() -> StoreMode {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(mode) = mode_override() {
        return mode;
    }
    MODE.get().copied().unwrap_or_default()
}

/// A host test's replacement for the published mode. It cannot be thread-local like
/// [`STORE_FOR_TEST`]: the session writes on `plx_base::storage_worker`, a thread the test that
/// sets it does not own. It is a process global, so [`set_mode_for_test`] asserts the crate test
/// lock is held and its guard clears the override.
#[cfg(any(test, feature = "test-support"))]
static MODE_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(OVERRIDE_NONE);
#[cfg(any(test, feature = "test-support"))]
const OVERRIDE_NONE: u8 = 0;
#[cfg(any(test, feature = "test-support"))]
const OVERRIDE_DURABLE: u8 = 1;
#[cfg(any(test, feature = "test-support"))]
const OVERRIDE_MEMORY_ONLY: u8 = 2;

#[cfg(any(test, feature = "test-support"))]
fn mode_override() -> Option<StoreMode> {
    match MODE_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst) {
        OVERRIDE_DURABLE => Some(StoreMode::Durable),
        OVERRIDE_MEMORY_ONLY => Some(StoreMode::MemoryOnly),
        _ => None,
    }
}

/// Clears the mode override when dropped, so a failing test cannot leave `MemoryOnly` behind for
/// the next one.
#[cfg(any(test, feature = "test-support"))]
#[must_use = "the override ends when this guard drops"]
pub struct ModeReset(());

#[cfg(any(test, feature = "test-support"))]
impl Drop for ModeReset {
    fn drop(&mut self) {
        MODE_OVERRIDE.store(OVERRIDE_NONE, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Run the calling test under `mode` until the returned guard drops. The caller holds
/// `plx_base::testlock::serial()` for the whole test, as for every crate global.
#[cfg(any(test, feature = "test-support"))]
pub fn set_mode_for_test(mode: StoreMode) -> ModeReset {
    plx_base::testlock::assert_held("secure::set_mode_for_test");
    MODE_OVERRIDE.store(
        match mode {
            StoreMode::Durable => OVERRIDE_DURABLE,
            StoreMode::MemoryOnly => OVERRIDE_MEMORY_ONLY,
        },
        std::sync::atomic::Ordering::SeqCst,
    );
    ModeReset(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_mode_is_durable() {
        let _serial = plx_base::testlock::serial();
        assert_eq!(StoreMode::default(), StoreMode::Durable);
        assert_eq!(mode(), StoreMode::Durable, "nothing is published in a test process");
    }

    #[test]
    fn the_override_is_read_from_any_thread_and_the_guard_clears_it() {
        let _serial = plx_base::testlock::serial();
        {
            let _mode = set_mode_for_test(StoreMode::MemoryOnly);
            assert_eq!(mode(), StoreMode::MemoryOnly);
            assert_eq!(
                std::thread::spawn(mode).join().unwrap(),
                StoreMode::MemoryOnly,
                "the session writes on a worker thread"
            );
        }
        assert_eq!(mode(), StoreMode::Durable);
    }

    #[test]
    fn a_failing_test_cannot_leave_memory_only_behind() {
        let _serial = plx_base::testlock::serial();
        let failed = std::panic::catch_unwind(|| {
            let _mode = set_mode_for_test(StoreMode::MemoryOnly);
            panic!("a test body that fails while the override is set");
        });
        assert!(failed.is_err());
        assert_eq!(mode(), StoreMode::Durable);
    }

    #[test]
    #[should_panic(expected = "without crate::testlock::serial()")]
    fn the_setter_refuses_a_caller_that_does_not_hold_the_test_lock() {
        let _ = set_mode_for_test(StoreMode::MemoryOnly);
    }
}
