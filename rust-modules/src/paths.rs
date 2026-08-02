//! Where the app's own files live — resolved at runtime, never hardcoded.
//!
//! webOS installs a native app under ONE of two prefixes, and which one is not ours to choose:
//!
//! | installed by | app dir | jail profile |
//! |---|---|---|
//! | LG Developer Mode (`ares-install`) | `/media/developer/apps/usr/palm/applications/<id>` | `jail_native_devmode.conf` |
//! | webOS Homebrew Channel | `/media/cryptofs/apps/usr/palm/applications/<id>` | `jail_native.conf` |
//!
//! The two jails differ in what is WRITABLE, which is the part that bites:
//! `jail_native_devmode.conf` does `mount rw /media/developer` + `mount ro /media/internal`;
//! `jail_native.conf` does the opposite and contains no `/media/developer` at all. So a path that
//! is correct under one install is missing under the other.
//!
//! Both failures used to be silent. Hardcoded font paths fell through to the system DroidSans
//! while `init_text` still logged `ok=1` — the whole `theme::size` ladder rendered in a face with
//! no bold companion, so every bold rung became synthetic emboldening applied after grid-fitting.
//! The session file's `open` returned ENOENT into a best-effort `save()` that dropped it, so the
//! user re-did the QR sign-in on every boot with a fresh client id each time.
//!
//! `/proc/self/exe` is the answer, not `$HOME`: LG's own jail conf sets `HOME` **twice** (once to
//! `$APPDIR`, once to `/media/developer`) and which one survives differs between the two profiles.
//! `/proc` is `mount rw` in both, and read from inside the jail the link resolves jail-relative —
//! exactly the path the app must use. Kodi's `CPlatformWebOS::GetHomePath()` does the same thing.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The Developer Mode install dir. Only a last-resort fallback now — it is what the app used to
/// hardcode, so it keeps the historical behaviour if `/proc` is somehow unreadable.
const LEGACY_APP_DIR: &str = "/media/developer/apps/usr/palm/applications/com.beb.plxnative";

/// The directory the running executable sits in — i.e. where the ipk's payload was installed.
pub(crate) fn app_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(exe) = std::fs::read_link("/proc/self/exe") {
            if let Some(parent) = exe.parent() {
                if !parent.as_os_str().is_empty() {
                    crate::log(&format!("appdir: {} (from /proc/self/exe)", parent.display()));
                    return parent.to_path_buf();
                }
            }
        }
        // Not expected on device — worth a line in the log if it ever happens, because everything
        // below this point is a guess.
        crate::log(&format!("appdir: /proc/self/exe unreadable, falling back to {LEGACY_APP_DIR}"));
        PathBuf::from(LEGACY_APP_DIR)
    })
}

/// A file shipped inside the ipk, addressed by name.
pub(crate) fn in_app_dir(name: &str) -> PathBuf {
    app_dir().join(name)
}

/// Candidate locations for the persisted session, best first.
///
/// Ordering rationale — the first entry must stay first: `/media/developer/<id>-auth.json` is
/// deliberately OUTSIDE the app directory because appinstalld replaces that directory wholesale on
/// every (re)install, which silently signed the user out. It is writable in the Developer Mode
/// jail and it survives a reinstall, so it remains the preferred home.
///
/// Under the production jail that path does not exist, and the app dir itself is `root:5000 0755`
/// — not writable by the jailed uid. `/media/internal` is `mount rw` in that profile and is the
/// only persistent writable location there, so it is the second candidate. The app dir is third
/// on the theory that a future layout may make it writable; the legacy in-app-dir path is last and
/// is read-only in practice (migration).
pub(crate) fn session_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/media/developer/com.beb.plxnative-auth.json"),
        PathBuf::from("/media/internal/.com.beb.plxnative-auth.json"),
        in_app_dir("auth.json"),
        PathBuf::from(LEGACY_APP_DIR).join("auth.json"),
    ]
}

#[cfg(test)]
mod tests {
    /// `app_dir` must hand back an ABSOLUTE directory and must not panic when `/proc/self/exe`
    /// is absent — which is the case on the dev Mac, so this test exercises the fallback arm that
    /// the device never reaches.
    #[test]
    fn app_dir_is_absolute_and_infallible() {
        let d = super::app_dir();
        assert!(d.is_absolute(), "app dir must be absolute, got {}", d.display());
        assert!(!d.as_os_str().is_empty());
    }

    /// The preferred session path must stay OUTSIDE the app directory: appinstalld replaces the
    /// app dir wholesale on reinstall, and a session stored inside it is a silent sign-out.
    #[test]
    fn preferred_session_path_survives_a_reinstall() {
        let c = super::session_candidates();
        assert!(c.len() >= 2, "a single hardcoded path is the bug this list exists to fix");
        assert!(
            !c[0].starts_with(super::app_dir()),
            "the preferred session path must not live inside the app dir ({})",
            c[0].display()
        );
        assert!(c.iter().all(|p| p.is_absolute()));
    }
}
