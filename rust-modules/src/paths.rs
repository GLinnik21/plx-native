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
/// **The guarantee is load-bearing, and the reason is in `src/main.c`.** That file opens the event
/// log, the crash log and the stderr capture before any Rust runs, and Rust's `log()` then appends
/// to the first of those and the panic hook to the second — so on a television the two languages
/// MUST resolve to the same files. If the environment could move the Rust half, `main.c` would
/// truncate the event log at one path while Rust wrote to another, and the log a crash report is
/// built from would be missing whichever half you did not look at.
///
/// `main.c` used to name all three by ABSOLUTE LITERAL, and this constant was the whole guarantee
/// on its own. It cannot be, now that the root varies per install ([`app_id`]): a second literal
/// in C would be a second definition of a value that moves. So `main.c` calls
/// [`plx_runtime_path`] instead — one resolver, asked from both languages — and this constant
/// keeps the other half of the promise, that the resolver's answer cannot be steered from outside
/// on a television.
///
/// This used to be two different gates for the same kind of override — `devtriggers` for the
/// runtime root, `hostsim` for the app dir. `devtriggers` is on in every dev TV build, so the
/// split-brain above was reachable on the device; and a `--no-default-features --features hostsim`
/// build had the opposite hole, writing its event log to a shared `/tmp` while the simulator
/// binary truncated one inside the instance root.
pub(crate) const ENV_STEERABLE: bool = cfg!(feature = "hostsim");

/// The app id this project ships to users, and the one every install falls back to.
///
/// It is a FALLBACK and a comparison value — never the answer on its own. See [`app_id`]: which
/// app this process is depends on where it was installed, not on what it was compiled with.
pub(crate) const STABLE_APP_ID: &str = "com.beb.plxnative";

/// The Developer Mode install dir. Only a last-resort fallback now — it is what the app used to
/// hardcode, so it keeps the historical behaviour if `/proc` is somehow unreadable.
const LEGACY_APP_DIR: &str = "/media/developer/apps/usr/palm/applications/com.beb.plxnative";

/// **Which install this process is** — `com.beb.plxnative` for the app users get, or
/// `com.beb.plxnative.<flavour>` for a developer build sitting beside it on the same television.
///
/// Read from the INSTALL DIRECTORY, not compiled in, and that is the whole design.
///
/// The soundness argument is about OUR OWN package, not about firmware behaviour we cannot
/// observe from a desk. `ci/mkipk.py` lays the payload down at `applications/<id>/` with the id
/// taken from `appinfo.json`, and three independent gates assert the two agree — `mkipk.py` exits
/// if the staged directory does not match, `ci/check-package.py` re-derives the id FROM that
/// directory and checks it against the staged descriptor and the control file, and `make deploy`
/// checks it again before scp'ing. So the directory this binary is running from is the id its own
/// package declared, which is the id webOS was asked to register. (Whether webOS additionally
/// POLICES a mismatch — the usual claim, and a plausible one — is a firmware behaviour nothing in
/// this repository can settle; `docs/two-installs.md` §6 keeps it on the device list rather than
/// asserting it here.) Deriving the id here means:
///
///   * `pkg/plxnative` stays ONE artifact. Nothing about the flavour reaches codegen, so there is
///     no second `--target-dir`, no second stamp, and no way for `make deploy` to ship a binary
///     compiled for the other install — the identity comes from where it LANDS. That matters here
///     specifically: this project's classic failure is the stale artifact that make thinks is
///     fresh (see the Makefile's `pkg/.build-config` comment), and a compiled-in id would have
///     added a fresh axis of it.
///   * a binary copied by hand into the wrong app directory identifies as that directory's app
///     rather than lying about which one it is.
///
/// The shape test is STRUCTURAL rather than name-based, exactly like [`macos_bundle_resources`]:
/// the parent directory is the id only when ITS parent is literally `applications`. That is what
/// makes both webOS prefixes answer (`/media/developer/apps/…` under Developer Mode and
/// `/media/cryptofs/apps/…` under the Homebrew Channel) while a host build — where the binary sits
/// in `target-sim/debug/` and the parent is named `debug` — falls through to the stable id instead
/// of inventing an app called `debug`.
///
/// **This function must never log, and nothing it calls may log.** `crate::log` resolves the event
/// log's path through [`runtime_dir`], which resolves through here; a `log()` on this path
/// re-enters a `OnceLock` it is already initializing and hangs the process before the first line
/// of output exists to explain why. `runtime_dir`'s doc records that deadlock being hit for real.
/// It is also why this reads `current_exe` itself rather than calling [`app_dir`], which logs.
pub(crate) fn app_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| installed_app_id(&exe))
            .unwrap_or_else(|| STABLE_APP_ID.to_string())
    })
}

/// The webOS app id implied by an executable path, or `None` when that path is not a webOS install.
///
/// Split out from [`app_id`] purely so it is testable — `app_id` memoises into a process-wide
/// `OnceLock` and reads the real `current_exe`, neither of which a test can pin.
fn installed_app_id(exe: &Path) -> Option<String> {
    let dir = exe.parent()?;
    if dir.parent()?.file_name()? != "applications" {
        return None;
    }
    Some(dir.file_name()?.to_string_lossy().into_owned())
}

/// The flavour suffix of [`app_id`] — `None` for the app users get, `Some("debug")` for
/// `com.beb.plxnative.debug`.
///
/// One definition, so no caller has to know how a flavour is spelled. Everything that must differ
/// between two installs on one television — the runtime root, the session file, the plex.tv device
/// name — asks this rather than parsing the id again.
pub(crate) fn flavour() -> Option<&'static str> {
    app_id().strip_prefix(STABLE_APP_ID)?.strip_prefix('.')
}

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
/// On the television `src/main.c` opens those same logs before any Rust runs, through
/// [`plx_runtime_path`] — the same resolver, so the two languages cannot disagree — and
/// [`ENV_STEERABLE`] guarantees that answer is not steerable from outside.
///
/// `/tmp` on the television for the app users install, always; `/tmp/<app id>` for a developer
/// build sitting beside it (see [`resolve_runtime_dir`]). Both jail profiles mount the shared
/// system `/tmp`, and every tool, skill and harness recipe addresses those files by absolute path
/// — which is why `make -s print-rundir` exists rather than a second copy of the rule.
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
        let d = resolve_runtime_dir(std::env::var_os("PLXNATIVE_RUNTIME_DIR"), macos_app_support(), app_id());
        ensure_runtime_dir(&d);
        d
    })
}

/// Create the runtime root if it is not `/tmp` itself, and make it world-writable + sticky.
///
/// **1777, deliberately, and it must be a separate `chmod`.** Two different uids write into this
/// directory and neither can be made to go second: the app runs jailed under its own uid and
/// creates its logs there, while `tests/run.py` and `tools/tv-session.sh` arm triggers there over
/// ssh **as root, before the app has ever booted**. Whoever gets there first sets the mode, so any
/// owner-only mode locks the other one out — and the Makefile already records what that failure
/// looks like from the other side: a root-owned event log the jailed app cannot write leaves the
/// file at 0 bytes, which every tool in this repo reports as "no line found", i.e. exactly like a
/// total regression. `/tmp` on the television is itself 1777 for this reason; a per-install root
/// inside it must not be stricter. `create_dir_all` applies the process umask to its mode, which
/// would silently drop the group/other bits, hence the explicit `set_permissions` after it.
///
/// Best-effort and silent by necessity: this runs inside [`runtime_dir`]'s `OnceLock` initializer,
/// where logging deadlocks (see that function's doc). A failure here surfaces as the log the app
/// could not open, which is the same signal by a slower route.
fn ensure_runtime_dir(d: &Path) {
    // Television builds only, and only for a flavoured root. A host build's root is created by
    // `make sim` and written by exactly one uid, so there is nothing here to solve — and
    // `PLXNATIVE_RUNTIME_DIR` can point anywhere, including inside somebody's home directory,
    // which is not a thing to silently chmod 1777.
    if ENV_STEERABLE || d == Path::new(DEFAULT_RUNTIME_DIR) {
        return;
    }
    let _ = std::fs::create_dir_all(d);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o1777));
    }
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
/// reason: it reads `current_exe` and `HOME`, neither of which a test can pin. `app_id` is a
/// parameter for the same reason.
///
/// **A flavoured install gets its own root, and the app users get keeps `/tmp` byte for byte.**
/// Two installs on one television otherwise share one event log (the launching one TRUNCATES it),
/// one append-only crash log with nothing in it saying which binary faulted, one `plxnative-remote`
/// FIFO, one `:8910` capture listener and one trigger namespace — a set of collisions whose
/// symptom is never an error, only evidence about the wrong process. Because every one of those
/// surfaces already composes on [`in_runtime_dir`], moving the ROOT separates all of them at once,
/// and leaves the ~40 trigger names, the `dev::DIAG` list and every existing recipe spelled
/// exactly as they are — inside the new root.
///
/// **The separator is a DOT and that is load-bearing.** `dev::any_trigger_present` scans this
/// directory for entries beginning `plxnative-` to decide whether the boot is automated, so a
/// sibling root spelled with a HYPHEN after `plxnative` would itself read as an armed trigger and
/// silently suppress the OTHER install's who's-watching picker — changing which screen it boots to,
/// with no log line anywhere. The full app id contains no `plxnative-`, so it cannot. (The bad name
/// is deliberately not written out here: the trigger catalog this project publishes is a `grep` for
/// that very prefix over these sources, so spelling it would mint a catalog entry for a trigger
/// that must never exist.) (That function also
/// grew a file-type filter, which is the second, independent reason.)
fn resolve_runtime_dir(
    env: Option<std::ffi::OsString>,
    bundled: Option<PathBuf>,
    app_id: &str,
) -> PathBuf {
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
    if app_id == STABLE_APP_ID {
        return PathBuf::from(DEFAULT_RUNTIME_DIR);
    }
    Path::new(DEFAULT_RUNTIME_DIR).join(app_id)
}

/// **The runtime root, for `src/main.c`** — write `<root>/<name>` into `out`, and return 1 on
/// success or 0 if it does not fit.
///
/// This exists because of an ordering fact and a guarantee that used to be a promise. `main.c`
/// opens the event log, the crash log and the stderr capture before a single line of Rust runs,
/// and it cannot see this module — so until now it named all three by absolute literal, and
/// [`ENV_STEERABLE`] existed to guarantee that Rust could never resolve them anywhere else. With a
/// per-install runtime root that guarantee has to hold across a value that is no longer constant,
/// and two definitions of it — one in C, one here — is precisely the split-brain that doc warns
/// about: `main.c` would truncate a log at `/tmp` while Rust appended to another, and the crash
/// report you built afterwards would be missing whichever half you did not look at.
///
/// So there is still exactly one resolver, and C asks it. Calling into the staticlib this early is
/// safe — it touches no global beyond its own `OnceLock` and allocates — and it must stay that
/// way: like everything on this path it MUST NOT log, or it re-enters the lock it is initializing
/// (see [`runtime_dir`]).
///
/// # Safety
/// `name` must be a valid NUL-terminated C string; `out` must point to at least `cap` writable
/// bytes. The result is NUL-terminated whenever this returns 1.
#[no_mangle]
pub unsafe extern "C" fn plx_runtime_path(
    name: *const std::os::raw::c_char,
    out: *mut std::os::raw::c_char,
    cap: usize,
) -> std::os::raw::c_int {
    if name.is_null() || out.is_null() || cap == 0 {
        return 0;
    }
    let name = std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned();
    let p = in_runtime_dir(&name);
    let bytes = p.as_os_str().as_encoded_bytes();
    if bytes.len() + 1 > cap {
        return 0;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), bytes.len());
    *out.add(bytes.len()) = 0;
    1
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
    // Named for THIS install, so two flavours on one television do not share one sign-in. The
    // file holds the client identifier, the account token, every per-(user, server) PMS token and
    // the Plex Home roster — sharing it means a sign-out in the developer build signs you out of
    // the build you watch with, a profile switch in one decides who the other resumes as, and two
    // processes race a file whose `save()` is best-effort. The two installs are genuinely two
    // devices; `plex::identity::device_name` is what keeps them distinguishable in the account's
    // authorized-device list once they are.
    let id = app_id();
    v.extend([
        PathBuf::from(format!("/media/developer/{id}-auth.json")),
        PathBuf::from(format!("/media/internal/.{id}-auth.json")),
        in_app_dir("auth.json"),
    ]);
    // The legacy in-app-dir path is a MIGRATION source and it names the SHIPPED install's directory
    // by literal, so only the shipped install may offer it. `session::load` takes the first
    // candidate that EXISTS — so on a flavoured install, whose own three files are all absent on
    // first boot, this entry would hand a developer build the other install's account token, every
    // per-(user, server) PMS token and the Plex Home roster, which it would then write back under
    // its own name. Exactly the sharing the three lines above exist to prevent, arriving through
    // the one entry that was not made flavour-aware with them.
    if flavour().is_none() {
        v.push(PathBuf::from(LEGACY_APP_DIR).join("auth.json"));
    }
    v
}

/// Candidate locations for the telemetry decision, best first — **the same tier as the session**,
/// and that choice has a consequence worth stating rather than discovering.
///
/// It goes here, not in [`runtime_dir`], because the runtime root on a television is `/tmp` and
/// `/tmp` is cleared by a reboot: a consent decision that evaporated overnight would re-ask a
/// person who had already answered, which is both worse for them and the exact pattern that makes
/// a consent prompt feel like nagging rather than a choice.
///
/// **So it outlives an uninstall, and it outlives a change of owner short of a factory reset.**
/// webOS gives a native app no uninstall hook, so nothing can clear this on the way out. That is
/// why the file holds a DECISION and, only after opt-in, one random identifier — and why
/// withdrawing consent DELETES that identifier rather than merely disabling it. Recorded in
/// `PRIVACY.md`, because a user cannot audit a file they cannot reach.
///
/// Outside the `plxnative-` trigger namespace by construction, since it is not in the runtime root
/// at all — so it cannot suppress the who's-watching picker the way anything in `/tmp` would.
/// The spool, beside the decision that authorised it.
///
/// **Same directories, same search order, different file** — and not merged into
/// `telemetry.json` for one reason: the decision is small, rewritten rarely and must survive
/// anything, while the spool is up to half a megabyte rewritten after every flush. Sharing one file
/// would put the consent record itself at risk on every single upload, which is the one piece of
/// state whose loss changes what the app is allowed to do.
pub(crate) fn telemetry_spool_candidates() -> Vec<PathBuf> {
    telemetry_candidates()
        .into_iter()
        .map(|p| p.with_file_name(p.file_name().map_or_else(
            || "telemetry-spool.bin".into(),
            |n| n.to_string_lossy().replace("telemetry.json", "telemetry-spool.bin"),
        )))
        .collect()
}

pub(crate) fn telemetry_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // A steerable build keeps its own, for exactly the reason the session file does: several
    // simulators must not share one decision (or one identifier).
    if ENV_STEERABLE {
        v.push(in_runtime_dir("telemetry.json"));
    }
    let id = app_id();
    v.extend([
        PathBuf::from(format!("/media/developer/{id}-telemetry.json")),
        PathBuf::from(format!("/media/internal/.{id}-telemetry.json")),
        in_app_dir("telemetry.json"),
    ]);
    // No legacy/migration entry, and no `flavour().is_none()` arm: there is no older location, and
    // the two installs are two devices — a decision taken in one is not a decision about the other.
    v
}

/// Candidate locations for the persisted **last place** ([`crate::coldstart`]), best first.
///
/// The same three directories [`session_candidates`] uses, and deliberately **NOT**
/// [`runtime_dir`] — which is the obvious home for a file this small and this disposable, and is
/// the wrong one for exactly the event this file exists to survive. The runtime root on a
/// television is `/tmp`, and `/tmp` there is cleared by a reboot: this repo already leans on that
/// in the other direction (`tools/tv-lock.sh` puts the device lease under `/tmp` *because* a
/// reboot should release it), and [`crate::plex::session`] says the same from the other side when
/// it can reach none of the paths below — "login will not survive a reboot". A cold-start restore
/// whose state file is erased by the cold start is a no-op, so this goes where the session goes.
///
/// Two further consequences of that choice, both worth keeping:
///
/// * **It is outside the trigger namespace.** Any file in the runtime root whose name begins
///   `plxnative-` marks the boot as automated and suppresses the who's-watching picker
///   (`crate::dev::any_trigger_present`) — `tools/tv-lock.sh` records the same trap from the other
///   side. A state file written there on every ordinary boot would silently change which screen
///   every ordinary boot lands on. Here the question cannot arise.
/// * **`tests/run.py`'s teardown would not clear it anyway.** That sweep is `plxnative-*` in the
///   runtime root, so a differently-named file there would survive between cases just as this one
///   does. Determinism for the harness is bought where it belongs instead — `coldstart` refuses to
///   record at all on an automated boot.
///
/// No legacy/migration entry, unlike [`session_candidates`]: there is no older location to adopt,
/// and losing this file costs a page of navigation rather than a sign-in.
pub(crate) fn last_place_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // A steerable build keeps its own, for the reason the session file does: several simulators
    // run side by side, each with its own instance root, and one shared file would have them
    // restoring each other's page. Named WITHOUT the `plxnative-` prefix — off-device the runtime
    // root is where the triggers live, so the prefix is the one thing this name must not have.
    if ENV_STEERABLE {
        v.push(in_runtime_dir("lastplace.json"));
    }
    // Named for THIS install, so two flavours on one television do not restore each other's page.
    let id = app_id();
    v.extend([
        PathBuf::from(format!("/media/developer/{id}-lastplace.json")),
        PathBuf::from(format!("/media/internal/.{id}-lastplace.json")),
        in_app_dir("lastplace.json"),
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

    /// The app id is the INSTALL DIRECTORY's name, and only when that directory really is a webOS
    /// `applications/<id>` — which is what stops a host build from calling itself `debug`.
    ///
    /// Both install prefixes must answer, because which one a set uses is not ours to choose:
    /// Developer Mode unpacks under `/media/developer/apps`, the Homebrew Channel under
    /// `/media/cryptofs/apps`, and the app has to be the same app either way.
    #[test]
    fn the_app_id_is_the_install_directory_and_only_a_real_one() {
        use std::path::Path;
        let dev = "/media/developer/apps/usr/palm/applications/com.beb.plxnative.debug/plxnative";
        let hbc = "/media/cryptofs/apps/usr/palm/applications/com.beb.plxnative/plxnative";
        assert_eq!(super::installed_app_id(Path::new(dev)).as_deref(), Some("com.beb.plxnative.debug"));
        assert_eq!(super::installed_app_id(Path::new(hbc)).as_deref(), Some("com.beb.plxnative"));
        // A host build: the parent is `debug`, whose parent is `target-sim` — not `applications`.
        // Without this arm the simulator would mint an app called `debug`, take `/tmp/debug` as its
        // runtime root and look for a session file named after it.
        assert!(super::installed_app_id(Path::new("/repo/rust-modules/target-sim/debug/plxnative-sim")).is_none());
        // The macOS bundle layout, for the same reason.
        assert!(super::installed_app_id(Path::new("/A/PlxNative.app/Contents/MacOS/plxnative-sim")).is_none());
        // …and the real process resolves to SOMETHING, on either platform, without panicking.
        assert!(!super::app_id().is_empty());
    }

    /// A flavour is a dotted SUFFIX of the stable id, and the app users get has none. The whole
    /// two-install mechanism keys on this, so the two cases are worth pinning: a `Some("")` (the
    /// id with a bare trailing dot) or a `Some(_)` for the stable id would give the shipped app a
    /// flavoured runtime root and a renamed session file on the next boot.
    #[test]
    fn only_a_dotted_suffix_is_a_flavour() {
        assert_eq!(super::STABLE_APP_ID.strip_prefix(super::STABLE_APP_ID).and_then(|r| r.strip_prefix('.')), None);
        assert_eq!(
            "com.beb.plxnative.debug".strip_prefix(super::STABLE_APP_ID).and_then(|r| r.strip_prefix('.')),
            Some("debug")
        );
        // The real one must agree with the real id, whatever this binary turned out to be.
        assert_eq!(super::flavour().is_some(), super::app_id() != super::STABLE_APP_ID);
    }

    /// Two installs must not share a runtime namespace, and the app users get must keep `/tmp`
    /// exactly — every skill recipe, harness glob and doc line addresses those paths absolutely.
    ///
    /// The dot in the flavoured root is asserted on purpose: `dev::any_trigger_present` scans the
    /// root for entries starting `plxnative-`, so a root spelled with a hyphen there would read as
    /// an armed trigger from the OTHER install and silently suppress its boot picker. (Asserted on
    /// the prefix rather than on a literal, for the catalog-grep reason `resolve_runtime_dir` gives.)
    #[test]
    fn a_flavoured_install_gets_its_own_runtime_root() {
        if super::ENV_STEERABLE {
            return; // a host build answers from the environment first; this is the television rule
        }
        let stable = super::resolve_runtime_dir(None, None, super::STABLE_APP_ID);
        let debug = super::resolve_runtime_dir(None, None, "com.beb.plxnative.debug");
        assert_eq!(stable, std::path::Path::new("/tmp"));
        assert_eq!(debug, std::path::Path::new("/tmp/com.beb.plxnative.debug"));
        assert_ne!(stable.join("plxnative-events.log"), debug.join("plxnative-events.log"));
        let name = debug.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.starts_with("plxnative-"), "{name} would read as an armed trigger to the other install");
    }

    /// An absent or empty `PLXNATIVE_RUNTIME_DIR` must resolve to the television's `/tmp`. Empty
    /// matters on its own: `FOO= cmd` sets the variable to an empty string rather than unsetting
    /// it, and joining a name onto an empty root yields a RELATIVE path — trigger reads would then
    /// silently follow the process's working directory.
    #[test]
    fn absent_or_empty_runtime_root_is_the_television() {
        assert_eq!(super::resolve_runtime_dir(None, None, super::STABLE_APP_ID), std::path::Path::new("/tmp"));
        assert_eq!(super::resolve_runtime_dir(Some("".into()), None, super::STABLE_APP_ID), std::path::Path::new("/tmp"));
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
        assert_eq!(super::resolve_runtime_dir(None, Some(sup.clone()), super::STABLE_APP_ID), sup);
        assert_eq!(super::resolve_runtime_dir(Some("".into()), Some(sup.clone()), super::STABLE_APP_ID), sup);
        assert_eq!(
            super::resolve_runtime_dir(Some("/run/sim-a".into()), Some(sup), super::STABLE_APP_ID),
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
        let a = super::resolve_runtime_dir(Some("/run/sim-a".into()), None, super::STABLE_APP_ID).join("plxnative-library");
        let b = super::resolve_runtime_dir(Some("/run/sim-b".into()), None, super::STABLE_APP_ID).join("plxnative-library");
        assert_ne!(a, b);
        assert!(a.is_absolute() && b.is_absolute());
    }

    /// A television build must not be relocatable by the environment at all — `src/main.c` opens
    /// the logs through [`super::plx_runtime_path`], i.e. through this very function, so an
    /// environment that could move one half would move only that half's later reads. Compile-time
    /// half of the [`super::ENV_STEERABLE`] guarantee.
    #[test]
    fn a_television_build_ignores_the_environment() {
        if super::ENV_STEERABLE {
            return;
        }
        assert_eq!(
            super::resolve_runtime_dir(Some("/anywhere".into()), Some("/also/anywhere".into()), super::STABLE_APP_ID),
            std::path::Path::new("/tmp")
        );
    }

    /// Two installs must not share one sign-in. The file holds the client identifier, the account
    /// token, every per-(user, server) PMS token and the Plex Home roster, so a shared one means a
    /// sign-out in the developer build signs you out of the build you watch with — and two
    /// processes racing a best-effort `save()`.
    ///
    /// Asserted on the property (the two id's candidate lists are disjoint where it matters)
    /// rather than on the literals, so renaming the file cannot silently retire the guarantee.
    #[test]
    fn each_install_gets_its_own_session_file() {
        let named = |id: &str| {
            vec![
                std::path::PathBuf::from(format!("/media/developer/{id}-auth.json")),
                std::path::PathBuf::from(format!("/media/internal/.{id}-auth.json")),
            ]
        };
        let a = named(super::STABLE_APP_ID);
        let b = named("com.beb.plxnative.debug");
        assert!(a.iter().all(|p| !b.contains(p)), "{a:?} and {b:?} share a session file");
        // …and the real list really is built that way, whichever install this binary is.
        let real = super::session_candidates();
        assert!(
            real.iter().any(|p| p.to_string_lossy().contains(super::app_id())),
            "no candidate names this install ({}): {real:?}",
            super::app_id()
        );
        // The WHOLE list, not just the entries built from the id. The legacy in-app-dir candidate
        // is a literal naming the shipped install's directory, and `session::load` takes the first
        // candidate that EXISTS — so on a flavoured install, whose own files are absent on first
        // boot, an ungated legacy entry silently adopts the other install's sign-in. Asserting only
        // the first two entries (which this test did) cannot see that.
        if super::flavour().is_some() {
            for c in &real {
                assert!(
                    !c.to_string_lossy().contains(super::LEGACY_APP_DIR),
                    "a flavoured install offers the shipped install's session file: {}",
                    c.display()
                );
            }
        }
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
