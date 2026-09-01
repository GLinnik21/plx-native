//! Retirement of the old persisted last-page bookmark.
//!
//! A cold boot now always settles on the route selected by the credential flow: normally Home.
//! Home's Hero and Continue Watching rows are the product's single resume affordance; reopening a
//! previous Detail or Library route here would compete with them and make Home's primary content
//! redundant. This policy does not affect the live app-switch lifecycle in `app.rs`: an in-process
//! playback can still be suspended and resumed while the app remains alive.
//!
//! Builds before 2026-09-01 wrote `lastplace.json` beside the persisted session. Keep this tiny
//! one-shot cleanup until those installations have had a chance to upgrade, so the retired route
//! and its server identifier do not remain on disk indefinitely. No new bookmark is ever written.

use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq)]
struct RetireResult {
    removed: usize,
    failed: usize,
}

/// Discard every obsolete bookmark location. Failure is soft: a stale convenience file must never
/// prevent the app from reaching Home.
pub(crate) fn retire() {
    let result = retire_paths(&crate::paths::obsolete_last_place_candidates());
    if result.removed != 0 {
        crate::log("coldstart: discarded obsolete last-page bookmark");
    }
    if result.failed != 0 {
        crate::log("coldstart: could not remove every obsolete last-page bookmark");
    }
}

fn retire_paths(paths: &[PathBuf]) -> RetireResult {
    let mut result = RetireResult::default();
    for path in paths {
        retire_one(path, &mut result);
        if let Some(tmp) = tmp_sibling(path) {
            retire_one(&tmp, &mut result);
        }
    }
    result
}

fn retire_one(path: &Path, result: &mut RetireResult) {
    match std::fs::remove_file(path) {
        Ok(()) => result.removed += 1,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => result.failed += 1,
    }
}

fn tmp_sibling(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".tmp");
    Some(path.with_file_name(tmp_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a bookmark from a build that restored Detail must be consumed, not interpreted.
    /// With no restore API left, the credential-selected Home route remains untouched.
    #[test]
    fn cold_boot_discards_previous_route_and_stays_home() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "plxnative-retired-coldstart-{}.json",
            std::process::id()
        ));
        let tmp = tmp_sibling(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(
            &path,
            br#"{"v":1,"kind":"item","machine":"mach-A","rk":"42"}"#,
        )
        .unwrap();
        std::fs::write(&tmp, b"partial old write").unwrap();

        assert_eq!(
            retire_paths(std::slice::from_ref(&path)),
            RetireResult {
                removed: 2,
                failed: 0
            }
        );
        assert!(!path.exists(), "the old Detail bookmark must be retired");
        assert!(
            !tmp.exists(),
            "its abandoned atomic-write sibling must go too"
        );

        assert_eq!(
            retire_paths(&[path]),
            RetireResult::default(),
            "cleanup must be harmless on every later Home boot"
        );
    }
}
