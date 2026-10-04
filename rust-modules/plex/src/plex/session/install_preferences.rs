//! Credential-free language retained by host stores and explicitly supported file fallbacks.
//! Native DB8 remains authoritative whenever it answers; these files cannot authorize login.
use super::*;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    language: plx_platform::i18n::Preference,
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

pub(super) fn load() -> Option<plx_platform::i18n::Preference> {
    candidates().into_iter().find_map(|path| {
        let bytes = read_owned_regular(&path)?;
        let saved: Saved = serde_json::from_slice(&bytes).ok()?;
        (saved.version == 1).then_some(saved.language)
    })
}

/// Called under Session IO on the persistence worker, before credentials are cleared.
pub(super) fn save(language: plx_platform::i18n::Preference) -> bool {
    // Absence already means System, and need not create a file on a fresh installation.
    if language == plx_platform::i18n::Preference::System && load().is_none() { return true; }
    let bytes = serde_json::to_vec(&Saved { version: 1, language }).expect("language serialization");
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
