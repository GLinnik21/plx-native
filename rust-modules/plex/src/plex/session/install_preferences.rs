//! Credential-free install-wide choices (the UI language and the Interface resolution) retained by
//! host stores and explicitly supported file fallbacks.
//! Native DB8 remains authoritative whenever it answers; these files cannot authorize login.
use super::*;
use std::path::PathBuf;

/// The install-wide choices a sign-out keeps, because the next launch reads them before anyone is
/// signed in: the language, and the window size `construct` asks `SDL_CreateWindow` for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retained {
    pub language: plx_platform::i18n::Preference,
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
    version: u8,
    language: plx_platform::i18n::Preference,
    /// Absent (every file written before the setting) is Automatic, and Automatic is not written, so
    /// a file for an owner who never chose is byte-identical to one an older build wrote. Soft:
    /// a spelling this build does not know costs the resolution, never the language.
    #[serde(default, deserialize_with = "prefs::de_soft_ui_resolution", skip_serializing_if = "prefs::is_default_ui_resolution")]
    ui_resolution: UiResolution,
}

fn candidates() -> Vec<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    let redirected = TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).is_some()
        || TEST_CANDIDATES.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    #[cfg(not(any(test, feature = "test-support")))]
    let redirected = false;
    let mut paths = Vec::new();
    if !cfg!(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))
        && !redirected {
        paths.push(plx_base::paths::persistent_state_root().join("language.json"));
    }
    paths.extend(auth_paths().into_iter().map(|path| path.with_extension("language.json")));
    paths
}

pub(super) fn load() -> Option<Retained> {
    candidates().into_iter().find_map(|path| {
        let bytes = read_owned_regular(&path)?;
        let saved: Saved = serde_json::from_slice(&bytes).ok()?;
        (saved.version == 1).then_some(Retained { language: saved.language, ui_resolution: saved.ui_resolution })
    })
}

/// Called under Session IO on the persistence worker, before credentials are cleared.
pub(super) fn save(kept: Retained) -> bool {
    // Absence already means System and Automatic, and need not create a file on a fresh installation.
    if kept == Retained::default() && load().is_none() { return true; }
    let bytes = serde_json::to_vec(&Saved { version: 1, language: kept.language, ui_resolution: kept.ui_resolution })
        .expect("install preference serialization");
    for path in candidates() {
        if write_atomic(&path, &bytes).is_ok()
            && std::fs::File::open(path.parent().expect("preference parent"))
                .and_then(|directory| directory.sync_all()).is_ok()
            && read_owned_regular(&path).as_deref() == Some(bytes.as_slice()) {
            return true;
        }
    }
    false
}

/// Delete every candidate, including a stale earlier fallback. No credential resource is read.
pub(super) fn erase() -> Vec<String> {
    candidates().into_iter().filter_map(|path| {
        plx_platform::storage::remove_file_or_prove_absent(&path)
            .err().map(|error| format!("{}: {error}", path.display()))
    }).collect()
}
