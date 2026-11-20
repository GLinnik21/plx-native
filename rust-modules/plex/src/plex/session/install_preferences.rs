//! Credential-free install-wide choices (the UI language and the Interface resolution) retained by
//! file stores and explicitly supported file fallbacks.
//! The storage helper (DB8) remains authoritative whenever the platform states one
//! ([`Authority::Helper`]) and it answers; these files cannot authorize login.
//!
//! On a [`StoreMode::MemoryOnly`] platform this file is also where the install's own client id
//! lives (version 2): the one thing a launch must know before anyone signs in and that no credential
//! store is there to keep. The id is an install label, not a secret, and cannot authorize a login.
use super::*;
use plx_os::tv::authority::{authority, Authority};
use plx_os::tv::secure::{mode, StoreMode};
use std::path::PathBuf;

/// The install-wide choices a sign-out keeps, because the next launch reads them before anyone is
/// signed in: the language, and the window size `construct` asks `SDL_CreateWindow` for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retained {
    pub language: plx_os::i18n::Preference,
    pub ui_resolution: UiResolution,
}

impl Retained {
    /// The signed-out session: defaults, plus exactly what is retained. The one place a
    /// Cleared/Locked read becomes a [`Session`].
    pub(super) fn into_session(self) -> Session {
        Session { language: self.language, prefs: Preferences { ui_resolution: self.ui_resolution, ..Default::default() }, ..Default::default() }
    }

    /// What a session being signed out leaves behind.
    pub(super) fn of(session: &Session) -> Self {
        Retained { language: session.language, ui_resolution: session.ui_resolution() }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    /// 1 is the file every platform has always written. 2 adds `client_id` and is written only under
    /// [`StoreMode::MemoryOnly`], so a webOS or desktop file stays byte-identical to an older build's.
    version: u8,
    language: plx_os::i18n::Preference,
    /// Absent (every file written before the setting) is Automatic, and Automatic is not written, so
    /// a file for an owner who never chose is byte-identical to one an older build wrote. Soft:
    /// a spelling this build does not know costs the resolution, never the language.
    #[serde(default, deserialize_with = "prefs::de_soft_ui_resolution", skip_serializing_if = "prefs::is_default_ui_resolution")]
    ui_resolution: UiResolution,
    /// The install's client id: present in a version 2 file and in no other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}

impl Saved {
    /// What a file holds when this build accepts it: version 1 without an id, version 2 with one.
    fn accepted(self) -> Option<(Retained, Option<String>)> {
        let id_ok = match self.version {
            1 => self.client_id.is_none(),
            2 => self.client_id.as_deref().is_some_and(|id| !id.is_empty()),
            _ => false,
        };
        id_ok.then_some((Retained { language: self.language, ui_resolution: self.ui_resolution }, self.client_id))
    }
}

/// Whether the install's own state root holds a `language.json`. webOS keeps these choices in DB8
/// and never in this root; a memory-only platform has no DB8, so its root is where they live. A
/// redirected test fixture names its own files instead.
fn keeps_language_file(authority: Authority, mode: StoreMode, redirected: bool) -> bool {
    (authority == Authority::Files || mode == StoreMode::MemoryOnly) && !redirected
}

fn candidates() -> Vec<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    let redirected = TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).is_some()
        || TEST_CANDIDATES.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    #[cfg(not(any(test, feature = "test-support")))]
    let redirected = false;
    let mut paths = Vec::new();
    if keeps_language_file(authority(), mode(), redirected) {
        paths.push(plx_base::paths::persistent_state_root().join("language.json"));
    }
    paths.extend(auth_paths().into_iter().map(|path| path.with_extension("language.json")));
    paths
}

/// The first candidate this build accepts: the retained choices and, from a version 2 file, the
/// install's client id.
pub(super) fn read() -> Option<(Retained, Option<String>)> {
    candidates().into_iter().find_map(|path| {
        let bytes = read_owned_regular(&path)?;
        serde_json::from_slice::<Saved>(&bytes).ok()?.accepted()
    })
}

pub(super) fn load() -> Option<Retained> {
    read().map(|(kept, _)| kept)
}

/// Write `bytes` to the first candidate that takes them and reads back exactly.
fn write_first(bytes: &[u8]) -> bool {
    for path in candidates() {
        if write_atomic(&path, bytes).is_ok()
            && std::fs::File::open(path.parent().expect("preference parent"))
                .and_then(|directory| directory.sync_all()).is_ok()
            && read_owned_regular(&path).as_deref() == Some(bytes) {
            return true;
        }
    }
    false
}

/// Called under Session IO on the persistence worker, before credentials are cleared.
pub(super) fn save(kept: Retained) -> bool {
    // Absence already means System and Automatic, and need not create a file on a fresh installation.
    if kept == Retained::default() && load().is_none() { return true; }
    let bytes = serde_json::to_vec(&Saved { version: 1, language: kept.language, ui_resolution: kept.ui_resolution, client_id: None })
        .expect("install preference serialization");
    write_first(&bytes)
}

/// The [`StoreMode::MemoryOnly`] write: the same choices plus the install's client id, version 2.
/// An empty id has nothing to carry yet and writes the version 1 file.
pub(super) fn save_v2(kept: Retained, client_id: &str) -> bool {
    if client_id.is_empty() { return save(kept); }
    if read() == Some((kept, Some(client_id.to_owned()))) { return true; }
    let bytes = serde_json::to_vec(&Saved {
        version: 2,
        language: kept.language,
        ui_resolution: kept.ui_resolution,
        client_id: Some(client_id.to_owned()),
    }).expect("install preference serialization");
    write_first(&bytes)
}

/// Delete every candidate, including a stale earlier fallback. No credential resource is read.
pub(super) fn erase() -> Vec<String> {
    candidates().into_iter().filter_map(|path| {
        plx_os::storage::remove_file_or_prove_absent(&path)
            .err().map(|error| format!("{}: {error}", path.display()))
    }).collect()
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    /// The install's own `language.json` is where a file platform keeps its choices and where a
    /// memory-only platform keeps the only thing that outlives the process. A television whose
    /// storage helper holds them has none, and a redirected fixture names its own files.
    #[test]
    fn the_language_file_is_kept_unless_the_helper_holds_the_choices() {
        use Authority::{Files, Helper};
        use StoreMode::{Durable, MemoryOnly};
        assert!(keeps_language_file(Files, Durable, false), "the desktop and Tizen keep the file");
        assert!(keeps_language_file(Files, MemoryOnly, false));
        assert!(!keeps_language_file(Helper, Durable, false), "webOS keeps these choices in DB8, never in this root");
        assert!(keeps_language_file(Helper, MemoryOnly, false), "a memory-only platform has no DB8 to hold them");
        for (authority, mode) in [(Files, Durable), (Files, MemoryOnly), (Helper, MemoryOnly)] {
            assert!(!keeps_language_file(authority, mode, true), "a redirected fixture never reaches the real root");
        }
    }
}
