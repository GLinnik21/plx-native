//! Device-backed protection for the persisted Plex session, as the storage layer sees it: seal a
//! secret, open it again, drop its key. The platform's key manager is behind the port; the
//! envelope types live here so the on-disk shape does not depend on which platform sealed it.
//! Beside them, [`StoreMode`] says whether this platform keeps a credential across launches at
//! all.
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Which key store sealed an envelope: an opaque tag, written to disk exactly as spelled.
///
/// It is not a closed set. The tag is the only thing in the stored envelope that says which
/// platform's store can open it, and a platform names its own in its own code ([`Backend::new`]);
/// no shared code branches on one, but the legacy-migration decoder
/// (`plex::session::persistence::decode_legacy_at_depth`), which accepts only `keymanager3` from
/// the files earlier builds wrote. A tag that no installed platform knows is REFUSED when the
/// envelope is opened (the platform's `open` answers `None`, the session layer reports the file
/// locked) and is never read as another store's: the stored text is not ours to reinterpret.
///
/// Serde-transparent, so the wire form is the bare string: an envelope written before this type
/// stopped being an enum reads back unchanged, and one written now reads in an older build.
/// `the_sealed_envelope_keeps_its_wire_form_for_every_backend_tag` pins both directions.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Backend(Cow<'static, str>);

impl Backend {
    /// webOS's public `com.webos.service.keymanager3`: AES-GCM under a key the service holds.
    pub const KEYMANAGER3: Backend = Backend::new("keymanager3");
    /// webOS's archival `com.palm.keymanager`. Only ever READ from a file an interim build
    /// wrote: nothing seals with it and nothing opens it (its AES-CFB does not authenticate).
    pub const PALM_KEYMANAGER: Backend = Backend::new("palm_keymanager");

    /// A tag a platform names for its own store. The spelling is a wire format: it is stored in
    /// every sealed session, so it never changes once a build has written it.
    pub const fn new(tag: &'static str) -> Self {
        Self(Cow::Borrowed(tag))
    }

    /// The tag as stored.
    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    (super::services().secure.seal)(plain)
}

pub fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.open)(sealed);
    }
    (super::services().secure.open)(sealed)
}

pub fn remove(backend: &Backend, key: &str) {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(store) = STORE_FOR_TEST.with(|cell| cell.get()) {
        return (store.remove)(backend, key);
    }
    (super::services().secure.remove)(backend, key)
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

/// The persistence mode of this process: what the session layer reads before it touches a store.
/// The platform states it once, as the `mode` of its [`SecureHooks`](super::SecureHooks); a process
/// that installed none is a host test, whose `NONE` group says [`StoreMode::Durable`], which is
/// what every platform before this value existed was.
pub fn mode() -> StoreMode {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(mode) = mode_override() {
        return mode;
    }
    super::services().secure.mode
}

/// A host test's replacement for the platform's stated mode. It cannot be thread-local like
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

    /// **The sealed envelope's wire form, pinned for every backend tag a TV may have written.** The
    /// envelope is what `plex::session` stores on disk, so a changed spelling of a tag would leave
    /// every saved session on every TV unreadable. The literals are what this type wrote when the
    /// test was added; a refactor of `Backend` must keep them byte for byte, in both directions.
    /// `tests/fixtures/persistence/v0.6.*/secure-envelope-v1.json` carries the `keymanager3` tag
    /// inside the session file's wrapper, and nothing pinned `palm_keymanager` at all.
    #[test]
    fn the_sealed_envelope_keeps_its_wire_form_for_every_backend_tag() {
        let sealed = |backend| Sealed {
            backend,
            key: "plxnative.session.v1".into(),
            iv: "MDEyMzQ1Njc4OWFi".into(),
            data: "Y2lwaGVydGV4dA==".into(),
        };
        let cases = [
            (
                Backend::KEYMANAGER3,
                r#"{"backend":"keymanager3","key":"plxnative.session.v1","iv":"MDEyMzQ1Njc4OWFi","data":"Y2lwaGVydGV4dA=="}"#,
            ),
            (
                Backend::PALM_KEYMANAGER,
                r#"{"backend":"palm_keymanager","key":"plxnative.session.v1","iv":"MDEyMzQ1Njc4OWFi","data":"Y2lwaGVydGV4dA=="}"#,
            ),
        ];
        for (backend, literal) in cases {
            let value = sealed(backend);
            assert_eq!(serde_json::to_string(&value).unwrap(), literal, "{value:?} serializes to its literal");
            assert_eq!(serde_json::from_str::<Sealed>(literal).unwrap(), value, "{literal} reads back as {value:?}");
        }
    }

    /// The 0.6.3 to 0.6.5 files carry an `identity` member the envelope has no field for: it is
    /// ignored on read, and a rewrite no longer carries it. Pinned beside the tags above because
    /// both are the same promise: a file an earlier build wrote still opens.
    #[test]
    fn an_envelope_with_a_member_this_type_no_longer_has_still_reads() {
        let literal = r#"{"backend":"keymanager3","key":"plxnative.session.v1","iv":"MDEyMzQ1Njc4OWFi","data":"Y2lwaGVydGV4dA==","identity":"named"}"#;
        let read: Sealed = serde_json::from_str(literal).unwrap();
        assert_eq!(read.backend, Backend::KEYMANAGER3);
        assert_eq!(read.key, "plxnative.session.v1");
        assert_eq!(read.iv, "MDEyMzQ1Njc4OWFi");
        assert_eq!(read.data, "Y2lwaGVydGV4dA==");
    }

    /// A tag nobody here has a name for is carried through as written, and no constant equals it:
    /// it is the platform's `open` that refuses it, never this type that reads it as another store.
    #[test]
    fn a_tag_no_platform_knows_is_kept_verbatim_and_equals_no_known_tag() {
        let literal = r#"{"backend":"host_file","key":"plxnative.session.v1","iv":"aXY=","data":"ZGF0YQ=="}"#;
        let read: Sealed = serde_json::from_str(literal).unwrap();
        assert_eq!(read.backend.as_str(), "host_file");
        assert_ne!(read.backend, Backend::KEYMANAGER3);
        assert_ne!(read.backend, Backend::PALM_KEYMANAGER);
        assert_eq!(serde_json::to_string(&read).unwrap(), literal, "written back as it was read");
        assert_eq!(read.backend, Backend::new("host_file"));
    }

    /// With no platform installed nothing opens an envelope, whatever its tag: a refusal is the
    /// absence of an answer, not a reinterpretation.
    #[test]
    fn no_installed_platform_opens_any_tag() {
        for backend in [Backend::KEYMANAGER3, Backend::PALM_KEYMANAGER, Backend::new("host_file")] {
            let sealed = Sealed { backend, key: "k".into(), iv: "i".into(), data: "d".into() };
            assert!(open(&sealed).is_none(), "{sealed:?}");
        }
    }

    #[test]
    fn the_default_mode_is_durable() {
        let _serial = plx_base::testlock::serial();
        assert_eq!(StoreMode::default(), StoreMode::Durable);
        assert_eq!(mode(), StoreMode::Durable, "no services are installed in a test process");
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
