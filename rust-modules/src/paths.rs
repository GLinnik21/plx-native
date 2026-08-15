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

/// May the ENVIRONMENT relocate this app's paths?
///
/// Only in a build that is being steered from outside — the desktop simulator. A television
/// binary, dev or release, resolves everything from the device, and this is a compile-time
/// constant so that is not a policy but a guarantee.
///
/// **The guarantee is load-bearing, and the reason is in `src/main.c`.** That file opens
/// `/tmp/plxnative-events.log` (:140), `/tmp/plxnative-crash.log` (:165) and
/// `/tmp/plxnative-stderr.log` (:168) by absolute literal, before any Rust runs, and it cannot see
/// this module. Rust's `log()` appends to the first of those and the panic hook to the second — so
/// on a television the two languages MUST resolve to the same files. If the environment could move
/// the Rust half, `main.c` would truncate the event log at `/tmp` while Rust wrote somewhere else,
/// and the log a crash report is built from would be missing whichever half you did not look at.
///
/// This used to be two different gates for the same kind of override — `devtriggers` for the
/// runtime root, `hostsim` for the app dir. `devtriggers` is on in every dev TV build, so the
/// split-brain above was reachable on the device; and a `--no-default-features --features hostsim`
/// build had the opposite hole, writing its event log to a shared `/tmp` while the simulator
/// binary truncated one inside the instance root.
pub(crate) const ENV_STEERABLE: bool = cfg!(feature = "hostsim");

/// The Developer Mode install dir. Only a last-resort fallback now — it is what the app used to
/// hardcode, so it keeps the historical behaviour if `/proc` is somehow unreadable.
const LEGACY_APP_DIR: &str = "/media/developer/apps/usr/palm/applications/com.beb.plxnative";

/// The directory the running executable sits in — i.e. where the ipk's payload was installed.
///
/// `std::env::current_exe` IS the `/proc/self/exe` read on Linux, so this is the same syscall the
/// reasoning above is about — it just also answers on a host build, where the simulator's fonts sit
/// next to the simulator binary rather than under a webOS install prefix.
pub(crate) fn app_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // The simulator's binary lives in `target/<profile>/`, which is not where `appfont.ttf`
        // and the icons are — those ship in `pkg/`. Rather than copy assets around on every
        // build, the simulator is told where the payload is. Compile-time gated, so a television
        // build has no such override and resolves exactly as documented above.
        if ENV_STEERABLE {
            if let Some(d) = std::env::var_os("PLXNATIVE_APP_DIR") {
                let p = PathBuf::from(d);
                if !p.as_os_str().is_empty() {
                    crate::log(&format!("appdir: {} (PLXNATIVE_APP_DIR)", p.display()));
                    return p;
                }
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            // A macOS **application bundle** puts the executable in `Contents/MacOS/` and its
            // payload in `Contents/Resources/` — the layout every Mac tool, `codesign` included,
            // expects. So on that one platform "next to the binary" is the wrong answer by one
            // hop, and getting it wrong is the SILENT font failure this module's doc opens with:
            // `text.rs` would fall through to a system face and still log `ok=1`.
            if let Some(res) = macos_bundle_resources(&exe) {
                crate::log(&format!("appdir: {} (macOS bundle)", res.display()));
                return res;
            }
            if let Some(parent) = exe.parent() {
                if !parent.as_os_str().is_empty() {
                    crate::log(&format!("appdir: {} (from current_exe)", parent.display()));
                    return parent.to_path_buf();
                }
            }
        }
        // Not expected on device — worth a line in the log if it ever happens, because everything
        // below this point is a guess.
        crate::log(&format!("appdir: current_exe unreadable, falling back to {LEGACY_APP_DIR}"));
        PathBuf::from(LEGACY_APP_DIR)
    })
}

/// A file shipped inside the ipk, addressed by name.
pub(crate) fn in_app_dir(name: &str) -> PathBuf {
    app_dir().join(name)
}

/// `…/Contents/Resources` when `exe` is the executable of a macOS **application bundle**, else
/// `None` — the one platform where the app's payload is not the executable's own directory.
///
/// Structural, not name-based: it asks whether the two directories above the binary are literally
/// `Contents/MacOS`, which is what makes something a bundle. A `PlxNative.app` renamed by the
/// person it was sent to still answers yes; a loose `plxnative-sim` in `target-sim/debug` still
/// answers no, so the dev loop is untouched.
///
/// Must not log — [`runtime_dir`] calls the sibling below and that function's doc explains why a
/// log there deadlocks the process. Cheap enough to keep both on the same rule.
#[cfg(target_os = "macos")]
fn macos_bundle_resources(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let res = contents.join("Resources");
    res.is_dir().then_some(res)
}

#[cfg(not(target_os = "macos"))]
fn macos_bundle_resources(_exe: &Path) -> Option<PathBuf> {
    None
}

/// Where a **bundled macOS app** keeps everything it writes: `~/Library/Application
/// Support/PlxNative`.
///
/// The default runtime root is `/tmp`, which is right on the television and wrong for a Mac app
/// somebody was sent: `auth.json` lives in this root (see [`session_candidates`]), and `/tmp` is
/// swept, so the friend would re-do the QR sign-in every few days without ever learning why. The
/// app bundle itself is not an option either — it may sit in a read-only `/Applications`, and on a
/// signed bundle writing inside `Contents/` invalidates the signature.
///
/// `None` unless this really is a bundle: a plain `make sim-run` keeps taking `/tmp` (or its
/// `PLXNATIVE_RUNTIME_DIR` instance root) exactly as before, so no harness recipe moves.
#[cfg(target_os = "macos")]
fn macos_app_support() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    macos_bundle_resources(&exe)?;
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join("Library/Application Support/PlxNative"))
}

#[cfg(not(target_os = "macos"))]
fn macos_app_support() -> Option<PathBuf> {
    None
}

/// Where this instance's RUNTIME surfaces live — the `plxnative-*` dev triggers, the remote FIFO,
/// and the logs Rust itself writes (the event log and the panic hook's crash log). Not the app's
/// own files; that is [`app_dir`].
///
/// On the television `src/main.c` opens those same two logs by absolute literal before any Rust
/// runs, so the two languages must agree — which is precisely what [`ENV_STEERABLE`] guarantees.
///
/// `/tmp` on the television, always. Both jail profiles mount the shared system `/tmp`, and every
/// tool, skill and harness recipe addresses those files by absolute path.
///
/// `PLXNATIVE_RUNTIME_DIR` overrides it for exactly one reason: the television serializes the whole
/// dev loop. There is one set, one app instance, and `tests/run.py` jobs kill each other's app if
/// two run at once — so the single global `/tmp/plxnative-*` namespace has never cost anything. A
/// host build removes that constraint, and then the namespace becomes the constraint instead: two
/// simulators booting different screens would read one `plxnative-library` trigger and drain one
/// FIFO. Pointing each instance at its own directory is what lets several run side by side, and
/// keeps every existing recipe — the trigger names, the `ok`/`down`/`ck:X,Y` tokens — working
/// verbatim inside it.
///
/// The override is gated on [`ENV_STEERABLE`], so a television build of any feature set resolves
/// to the literal `/tmp` at compile time and cannot be steered by the environment.
///
/// **This function must never log, and nothing it calls may log.** `crate::log` resolves the event
/// log's path through this very `OnceLock`, so a `log()` inside the initializer re-enters
/// `get_or_init` on the lock it is already holding and deadlocks the process — before the first
/// line of output exists to explain why. That is not hypothetical: it is what the first version of
/// this function did, and the simulator hung silently at startup with an empty log and no stderr.
///
/// There is nothing to announce anyway. On a television the answer is always `/tmp`, and the one
/// configuration that overrides it is the one that passed the value in.
pub(crate) fn runtime_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        resolve_runtime_dir(std::env::var_os("PLXNATIVE_RUNTIME_DIR"), macos_app_support())
    })
}

/// The television's runtime root, and the only one a television build can ever have.
const DEFAULT_RUNTIME_DIR: &str = "/tmp";

/// The decision behind [`runtime_dir`], as a pure function of the environment.
///
/// Split out solely so it is testable: `runtime_dir` memoises into a process-wide `OnceLock`, so a
/// test that set the variable and called it would fix the value for every LATER test in the same
/// binary — including the one asserting that an empty trigger reads as `Some("")`, which resolves
/// its own write path through this same root. That is the crate-global-seam hazard `testlock`
/// exists for, and here it is avoidable outright rather than lockable.
///
/// `bundled` is the second-choice root for a host build that is a real macOS app bundle (see
/// [`macos_app_support`]). It is a PARAMETER rather than a call inside, for the same testability
/// reason: it reads `current_exe` and `HOME`, neither of which a test can pin.
fn resolve_runtime_dir(env: Option<std::ffi::OsString>, bundled: Option<PathBuf>) -> PathBuf {
    // Tier-B `cfg!` rather than `#[cfg]`: both arms stay type-checked in every feature set, so the
    // host branch cannot rot while nobody is building the simulator.
    if ENV_STEERABLE {
        if let Some(d) = env {
            if !d.is_empty() {
                return PathBuf::from(d);
            }
        }
        // An explicit instance root still wins — `make sim-shot` and every parallel-simulator
        // recipe pass one, and a bundle handed a root must honour it.
        if let Some(p) = bundled {
            return p;
        }
    }
    PathBuf::from(DEFAULT_RUNTIME_DIR)
}

/// A runtime surface inside [`runtime_dir`], addressed by its bare name (`plxnative-…` included).
///
/// Everything that opens one of these goes through here, so the instance root has a single
/// definition. `dev.rs` is still the only module allowed to name a TRIGGER — this is the path
/// arithmetic underneath it, shared with the remote FIFO.
pub(crate) fn in_runtime_dir(name: &str) -> PathBuf {
    runtime_dir().join(name)
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
    let mut v = Vec::new();
    // A steerable build gets its own identity, first. Without this every concurrent simulator
    // falls through the two `/media/…` candidates (absent off-device) into `in_app_dir`, i.e. the
    // repo's own `pkg/auth.json` — so they SHARE one session file containing `account_token`,
    // `server.token` and `user.token`. That breaks the very concurrency the instance root exists
    // to provide (one simulator switching profile mutates another's), and it drops live
    // credentials into the payload directory of a public repository. The `.gitignore` entry for
    // that path is a guard against the symptom; this is the cause.
    if ENV_STEERABLE {
        v.push(in_runtime_dir("auth.json"));
    }
    v.extend([
        PathBuf::from("/media/developer/com.beb.plxnative-auth.json"),
        PathBuf::from("/media/internal/.com.beb.plxnative-auth.json"),
        in_app_dir("auth.json"),
        PathBuf::from(LEGACY_APP_DIR).join("auth.json"),
    ]);
    v
}

#[cfg(test)]
mod tests {
    /// `app_dir` must hand back an ABSOLUTE directory and must not panic however it got there.
    ///
    /// This used to say it exercised the LEGACY_APP_DIR fallback, because `read_link` of
    /// `/proc/self/exe` fails on the dev Mac. It no longer does: `current_exe` answers on both
    /// platforms, so the fallback is now a guard neither reaches and this asserts the property
    /// rather than the arm.
    #[test]
    fn app_dir_is_absolute_and_infallible() {
        let d = super::app_dir();
        assert!(d.is_absolute(), "app dir must be absolute, got {}", d.display());
        assert!(!d.as_os_str().is_empty());
    }

    /// An absent or empty `PLXNATIVE_RUNTIME_DIR` must resolve to the television's `/tmp`. Empty
    /// matters on its own: `FOO= cmd` sets the variable to an empty string rather than unsetting
    /// it, and joining a name onto an empty root yields a RELATIVE path — trigger reads would then
    /// silently follow the process's working directory.
    #[test]
    fn absent_or_empty_runtime_root_is_the_television() {
        assert_eq!(super::resolve_runtime_dir(None, None), std::path::Path::new("/tmp"));
        assert_eq!(super::resolve_runtime_dir(Some("".into()), None), std::path::Path::new("/tmp"));
    }

    /// A macOS app bundle writes under `~/Library/Application Support`, and an explicit instance
    /// root still beats it. Both halves matter: the first is what keeps a friend's sign-in from
    /// being swept out of `/tmp`, the second is what keeps `make sim-shot`'s `PLXNATIVE_RUNTIME_DIR`
    /// authoritative if the binary is ever run out of a bundle by the harness.
    #[test]
    fn a_bundled_app_writes_to_its_own_support_dir_unless_told_otherwise() {
        if !super::ENV_STEERABLE {
            return; // a television build has neither concept
        }
        let sup = std::path::PathBuf::from("/Users/x/Library/Application Support/PlxNative");
        assert_eq!(super::resolve_runtime_dir(None, Some(sup.clone())), sup);
        assert_eq!(super::resolve_runtime_dir(Some("".into()), Some(sup.clone())), sup);
        assert_eq!(
            super::resolve_runtime_dir(Some("/run/sim-a".into()), Some(sup)),
            std::path::Path::new("/run/sim-a")
        );
    }

    /// The bundle detector must answer on SHAPE — `…/Contents/MacOS/<exe>` — and must not be
    /// fooled by a binary that merely sits in a directory called `MacOS`, nor confused by the
    /// bundle having been renamed. Only meaningful where the function has a body.
    #[test]
    #[cfg(target_os = "macos")]
    fn only_a_real_contents_macos_layout_reads_as_a_bundle() {
        use std::path::Path;
        // No `Contents` above it → not a bundle, whatever it is called.
        assert!(super::macos_bundle_resources(Path::new("/x/MacOS/plxnative")).is_none());
        // Right shape, but `Resources/` does not exist on this filesystem → still None, because
        // the point of the probe is finding the payload, not recognising a layout.
        assert!(super::macos_bundle_resources(Path::new("/x/Contents/MacOS/plxnative")).is_none());
        // The dev-loop binary must keep resolving to its own directory.
        assert!(super::macos_bundle_resources(Path::new("/repo/target-sim/debug/plxnative-sim")).is_none());
    }

    /// Two instances given different roots must not share a namespace — the whole point of the
    /// override. Asserted on the composed trigger path, not just the root, because that is what
    /// actually collides.
    #[test]
    fn distinct_runtime_roots_give_distinct_trigger_paths() {
        if !super::ENV_STEERABLE {
            return; // a television build cannot be redirected, by design
        }
        let a = super::resolve_runtime_dir(Some("/run/sim-a".into()), None).join("plxnative-library");
        let b = super::resolve_runtime_dir(Some("/run/sim-b".into()), None).join("plxnative-library");
        assert_ne!(a, b);
        assert!(a.is_absolute() && b.is_absolute());
    }

    /// A television build must not be relocatable by the environment at all — `src/main.c` opens
    /// the logs by absolute literal and the two languages have to agree. Compile-time half of the
    /// [`super::ENV_STEERABLE`] guarantee.
    #[test]
    fn a_television_build_ignores_the_environment() {
        if super::ENV_STEERABLE {
            return;
        }
        assert_eq!(
            super::resolve_runtime_dir(Some("/anywhere".into()), Some("/also/anywhere".into())),
            std::path::Path::new("/tmp")
        );
    }

    /// The preferred session path must stay OUTSIDE the app directory: appinstalld replaces the
    /// app dir wholesale on reinstall, and a session stored inside it is a silent sign-out.
    #[test]
    fn preferred_session_path_survives_a_reinstall() {
        let c = super::session_candidates();
        assert!(c.len() >= 2, "a single hardcoded path is the bug this list exists to fix");
        // Holds for both arms: the television's first candidate is `/media/developer/…`, and a
        // steerable build's is the instance root — neither is inside the app dir.
        assert!(
            !c[0].starts_with(super::app_dir()),
            "the preferred session path must not live inside the app dir ({})",
            c[0].display()
        );
        assert!(c.iter().all(|p| p.is_absolute()));
    }
}
