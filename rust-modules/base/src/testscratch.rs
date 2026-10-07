//! Per-process scratch directories for tests, removed when the test process exits.
//!
//! A test binary that needs a floor beneath its per-test fixtures (the session file an
//! un-redirected test resolves to, the persistent-state root) used to create
//! `$TMPDIR/<name>-<pid>` and leave it: the only removal was the NEXT process that happened to
//! reuse the pid, which is almost never, so every suite run left a handful of directories behind
//! (about 1,100 a day across the fleet; 38,000 in `$TMPDIR` on one Mac). [`process_dir`] keeps the
//! per-pid name, which is what makes two concurrent test binaries safe, and registers ONE `atexit`
//! handler that removes every directory it handed out. Libtest ends a normal run by returning from
//! `main` (or `process::exit`), both of which run `atexit` handlers; a killed or aborted process
//! still leaves its directory, which no in-process mechanism can prevent.
//!
//! `ci/test_check_collisions.py` runs test binaries with `TMPDIR` pointed at an empty directory and
//! fails if anything is left, so a new fixture that leaks the same way cannot come back quietly.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static OWNED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

extern "C" fn remove_owned() {
    let owned = std::mem::take(&mut *OWNED.lock().unwrap_or_else(|e| e.into_inner()));
    for dir in owned {
        remove_tree(&dir);
    }
}

/// `$TMPDIR/plxnative-<label>-<pid>`: empty, created now, and removed (read-only subdirectories
/// included) when this process exits normally. Each call with the same `label` returns the same
/// directory afresh each time (and replaces an earlier one), so callers hold the result in a
/// `OnceLock`.
pub fn process_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("plxnative-{label}-{}", std::process::id()));
    // A directory a dead process with this pid left behind is nothing this run may read.
    remove_tree(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut owned = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    if owned.is_empty() {
        // SAFETY: `remove_owned` is a plain `extern "C" fn()` that touches only this module's lock.
        unsafe { libc::atexit(remove_owned) };
    }
    owned.push(dir.clone());
    dir
}

/// Remove `path` and everything under it, including a tree whose directories a test made
/// read-only (a directory without write permission cannot have its entries unlinked, so
/// `remove_dir_all` alone leaves it behind). Best effort: a missing path is success.
pub fn remove_tree(path: &Path) {
    restore_access(path);
    let _ = std::fs::remove_dir_all(path);
}

fn restore_access(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    if !meta.is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            restore_access(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_tree_takes_a_read_only_tree_with_files_in_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = process_dir("testscratch-selftest").join("tree");
        let sealed = root.join("sealed");
        std::fs::create_dir_all(&sealed).unwrap();
        std::fs::write(sealed.join("f"), b"x").unwrap();
        std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o500)).unwrap();
        remove_tree(&root);
        assert!(!root.exists(), "a read-only subdirectory kept the tree alive");
    }

    #[test]
    fn a_process_dir_is_per_pid_empty_and_created() {
        let dir = process_dir("testscratch-pid");
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(dir.to_string_lossy().ends_with(&format!("-{}", std::process::id())));
    }
}
