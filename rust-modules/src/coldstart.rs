//! Where the app was when it last stopped — persisted, so a COLD start comes back to it.
//!
//! ## The gap this closes
//!
//! The app-switch lifecycle is already handled properly (`app.rs`'s `0x103`/`0x106` arms): going to
//! the background suspends the buffer-feed and drops to Home, coming back reloads and resumes at
//! the saved position with a single `Load`. All of that lives in the PROCESS, so it survives an
//! app switch and nothing else. A **cold** start — the television powered off and on, the mains
//! pulled, the app closed from the Recent List and launched again — begins with a process that has
//! never seen any of it, and until this module existed the boot route came purely from credential
//! state: signed out → Login, more than one profile → Profiles, otherwise Home. Where you actually
//! were was not written down anywhere.
//!
//! That is LG App Self Checklist **#3 (Reboot)**, whose matrix `docs/lg-self-checklist.md` records
//! as entirely unrun. This module is the persistence half of it. The VERDICT half is a device
//! session and is deliberately not decided here — see the note on `handlesRelaunch` at the bottom.
//!
//! ## What is persisted, and what is deliberately not
//!
//! A [`Place`]: the page, plus the item it was about. That is all. In particular the **playback
//! position is NOT here**, and adding it would be a second source of truth for something the
//! server already owns: PMS holds `viewOffset`, the app's `/:/timeline` reporter posts progress
//! every 10 s while playing, and `route.rs`'s resume path reads it back. A local copy could only
//! ever disagree with that — and it would disagree exactly when it matters, because the case this
//! module is about (the mains pulled) is the case where the local copy is the one that was never
//! flushed.
//!
//! For the same reason a session that was **playing** is restored to the played item's DETAIL page
//! and never to the player. Coming out of a power cycle straight into a decoding video is not a
//! restore, it is an ambush; the detail page puts Resume one press away and lets the server's own
//! `viewOffset` decide where that resumes from.
//!
//! ## Where the file lives, and why not in `/tmp`
//!
//! [`crate::paths::last_place_candidates`] — the same persistent directories the session file
//! uses, NOT the runtime root. The runtime root on a television is `/tmp`, which a reboot clears;
//! a cold-start restore whose state file is erased by the cold start restores nothing. That
//! function's doc carries the evidence and the two smaller consequences (the `plxnative-` trigger
//! namespace, and `tests/run.py`'s teardown glob).
//!
//! ## Why it is not in `Session`
//!
//! `plex::session` is the credentials file, and it fails as a unit: a parse failure there is a
//! silent sign-out on every boot. Its own fields carry that scar — every list in it is
//! soft-parsed, entry by entry, precisely so one bad element cannot cost the account token. A
//! convenience feature has no business inside that blast radius, so this owns its own file and its
//! own format, and every failure it can have costs one page of navigation.
//!
//! ## Two rules that are not obvious, and both exist to stop the feature erasing its own state
//!
//! **1. Nothing is recorded until the boot SEQUENCE has ended.** [`arm`] only reads the file;
//! [`take_restore`] decides. Between them the app may be on the QR sign-in, the who's-watching
//! picker or the first-run question, and every one of those hands over to Home when it is
//! answered. A recorder open through that window would write `home` over the record before anybody
//! could ask for it — permanently, on every boot, for exactly the multi-profile household the
//! `profile` field exists for. It is also why the profile is compared in `take_restore` and not in
//! `arm`: before the picker is answered, "who is watching" has no true answer.
//!
//! **2. Home is recorded as a RETURN, never as a landing** ([`State::left_home`]). Every boot ends
//! on Home whatever happened before it, including the boots where a restore was declined because
//! the item's server had not been re-registered yet. Recording that Home would destroy the record
//! by way of the one boot that could not use it; leaving Home *for* somewhere is a real answer to
//! "where was I", and arriving at it from the boot is not.
//!
//! ## Failure model
//!
//! Everything here fails SOFT to today's behaviour. An absent file, a truncated one, a hand-edited
//! one, an unwritable directory, a record belonging to a different profile, an item whose server
//! is no longer registered — each of those ends as "boot to Home", which is exactly what the app
//! did before this module existed. Nothing here can refuse a boot.
//!
//! ## `handlesRelaunch` / `nativeLifeCycleInterfaceVersion` — what this module does NOT claim
//!
//! `pkg/appinfo.json` sets `"handlesRelaunch": false` and `"nativeLifeCycleInterfaceVersion": 2`,
//! and nothing in this tree reads or reacts to either. This module changes neither, and does not
//! settle what they do on a native app on this firmware:
//!
//! * `handlesRelaunch` is documented by LG for RE-launching an app that is ALREADY RUNNING (the
//!   app decides whether it handles the second launch request itself, or is restarted). It says
//!   nothing about restoring state after a cold boot, which is a process that was not running.
//!   The page documenting it is written for the **web runtime**; per this repo's rule, a
//!   `develop/*` page is context for a native app and never proof.
//! * `nativeLifeCycleInterfaceVersion` is recorded in `docs/distribution.md` §1.6 as a webOS
//!   **OSE** property that no shipped native TV app sets — ours does, harmlessly.
//!
//! Both of those are answerable only with a television, by running the matrix in this feature's PR
//! body (remote power off/on, AC pull, Recent List, and both launch paths). Do not promote either
//! bullet to a verdict from a documentation page.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The on-disk format version. Bump it when a field's MEANING changes; an unrecognised version
/// reads as "no record", which is the same soft failure as an absent file.
const FORMAT: u32 = 1;

/// How long a page must be settled before it is written down, in `SDL_GetTicks` milliseconds.
///
/// Navigation is a burst — a press moves the route, and the next press is often a few hundred
/// milliseconds later — so writing on every change would put one file write per keypress on a
/// television's flash. Waiting for the page to settle collapses a browse to one write, and the
/// most that can be lost to a mains pull is the last page you had been on for under this long.
const SETTLE_MS: u32 = 1500;

/// A page a boot can be returned to. Owned, because it is what [`take_restore`] hands the caller
/// for a restore that happens frames later; [`PlaceRef`] is the borrowed twin the per-frame
/// recorder takes, so the steady state costs no allocation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) enum Place {
    #[default]
    Home,
    /// The browse grid, by TAB PILL index.
    ///
    /// A pill and not a section, for the reason the `plxnative-library=N` boot trigger's comment
    /// gives from the other side: the two library pills permanently mean Movies and TV Shows,
    /// rather than positions in a discovered table that reshuffles when a share changes. That
    /// makes the identity durable across the reboot this is persisted for.
    Library { tab: u32 },
    /// A detail page, or the item a playback was of.
    ///
    /// The server is named by its **`machineIdentifier`**, never by a [`crate::plex::ServerId`]:
    /// that is a registry SLOT, stable for the life of the process and meaningless in the next
    /// one. `rk` alone is not an identity either — every Plex `ratingKey` is server-local and
    /// dense from 1, so with a share registered a bare key names an item on neither machine in
    /// particular (`plex::same_item` is the same rule for the in-process case).
    Item { machine: String, rk: String },
}

/// [`Place`] as borrowed data — what [`note`] takes, so the common case (the page has not changed)
/// costs a comparison and no allocation at all on a 60 fps path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlaceRef<'a> {
    Home,
    Library { tab: u32 },
    Item { machine: &'a str, rk: &'a str },
}

impl Place {
    fn is(&self, r: PlaceRef<'_>) -> bool {
        match (self, r) {
            (Place::Home, PlaceRef::Home) => true,
            (Place::Library { tab: a }, PlaceRef::Library { tab: b }) => *a == b,
            (Place::Item { machine, rk }, PlaceRef::Item { machine: m, rk: k }) => {
                machine == m && rk == k
            }
            _ => false,
        }
    }
    fn of(r: PlaceRef<'_>) -> Place {
        match r {
            PlaceRef::Home => Place::Home,
            PlaceRef::Library { tab } => Place::Library { tab },
            PlaceRef::Item { machine, rk } => Place::Item {
                machine: machine.to_owned(),
                rk: rk.to_owned(),
            },
        }
    }
}

/// The file, as JSON.
///
/// **Flat, with a string `kind`, rather than a serde enum**, and that is the failure model rather
/// than a style: an unrecognised `kind` from a newer build (or a hand edit) degrades to "no
/// record" through one `match` arm here, while an externally-tagged enum makes the same input fail
/// the whole `Deserialize` — same outcome today, but it takes the diagnostics (`at`, `profile`)
/// down with it, and those are what a device session reads to tell "never written" from "written
/// and rejected". Every field is `#[serde(default)]` for the same reason.
#[derive(Serialize, Deserialize, Default)]
struct Record {
    /// The format this was written by — [`FORMAT`]. `#[serde(default)]` like every field beside
    /// it, and for the same reason: a record with no `v` must reach [`read_record`]'s version test
    /// and be rejected there, WITH its `profile`/`at` intact, rather than failing the whole
    /// `Deserialize` and taking the diagnostics a device session reads down with it.
    #[serde(default)]
    v: u32,
    /// The Plex Home user's `uuid`, `""` for the account owner — `session::current_profile_key`.
    /// A television is shared, and where the previous person was is not where you are.
    #[serde(default)]
    profile: String,
    /// `"home"` | `"library"` | `"item"`. Anything else is "no record".
    #[serde(default)]
    kind: String,
    #[serde(default)]
    tab: u32,
    #[serde(default)]
    machine: String,
    #[serde(default)]
    rk: String,
    /// Unix seconds when this was written, `0` when the clock would not answer.
    ///
    /// **Diagnostic only — the record does NOT expire.** An expiry was considered and declined:
    /// the clock is the unreliable half here (a set that has just come up off the mains has not
    /// necessarily reached NTP, and this repo already records pmlog's own wall clock running ~3 h
    /// off on the dev set), so a staleness gate would fail exactly at the moment the feature is
    /// supposed to work. "Open where I left off" is also what a television app is expected to do
    /// however long the set was off. This field is here so a device session can see WHEN a record
    /// was written without inferring it from file mtimes the jail may not preserve.
    #[serde(default)]
    at: i64,
}

/// How far through the boot this process is.
///
/// It exists because "read the record" and "decide what to do with it" happen at different times
/// and, on the who's-watching path, must: [`arm`] runs in the boot route selection, where the
/// profile is not known yet — nobody has said who they are — while the restore is resolved once the
/// boot SEQUENCE (sign-in, picker, first-run question) has ended on a real page. Reading the
/// profile at the second moment rather than the first is what makes this feature work at all for a
/// multi-user Plex Home, which is the household the `profile` field exists for.
enum Boot {
    /// Before [`arm`]. Nothing is recorded and nothing is restored.
    Cold,
    /// [`arm`] has read the file; the restore has not been resolved. **Recording is held off for
    /// this whole window**, so the boot's own screens cannot overwrite the record with the page
    /// they happen to be showing.
    Read(Option<Record>),
    /// [`take_restore`] has answered. Recording is live.
    Done,
}

/// The recorder's live state. A `Mutex` rather than the `static mut` most of this app's
/// main-thread state uses: it is touched once per frame and never contended, the lock is free at
/// that rate, and it is what lets the tests below drive the module directly.
struct State {
    /// Recording is on. `false` on an automated boot (see [`arm`]) and before boot.
    armed: bool,
    boot: Boot,
    /// The page as last observed by [`note`].
    last: Place,
    /// Has a page OTHER than Home been observed in this session?
    ///
    /// **Home is recorded as a RETURN, never as a landing**, and this is the whole of that rule.
    /// Every boot ends on Home whatever happened before it — the picker was answered, a restore
    /// was declined because the item's server has not been re-registered yet, a sign-in completed
    /// — and recording that Home would replace the previous session's page with the one place the
    /// app goes anyway. The record would then be destroyed by exactly the boots that could not use
    /// it, which is a feature that erases its own state. Leaving Home *for* somewhere is a real
    /// answer to "where was I"; arriving at it from the boot is not.
    left_home: bool,
    /// The tick `last` last CHANGED, `0` once it has been written down (or was never dirty).
    since: u32,
    /// What the file is believed to hold, as `(profile, place)`. `None` = unknown, so the next
    /// settle writes whatever it finds; that is the state after a record belonging to a DIFFERENT
    /// profile was read.
    ///
    /// **The profile is half of the key, not decoration.** Without it a Plex Home switch that
    /// leaves the page alone — switch profile from the account menu while standing on the same
    /// library tab — would find the place unchanged, write nothing, and leave a record stamped
    /// with the PREVIOUS profile: rejected as foreign on the next boot, for a page that is now
    /// genuinely this profile's.
    on_disk: Option<(String, Place)>,
    /// One log line for a persistent write failure, not one per settle.
    moaned: bool,
    /// Test hook: write here instead of [`crate::paths::last_place_candidates`].
    #[cfg(test)]
    file: Option<std::path::PathBuf>,
}

impl State {
    const fn new() -> State {
        State {
            armed: false,
            boot: Boot::Cold,
            last: Place::Home,
            left_home: false,
            since: 0,
            on_disk: None,
            moaned: false,
            #[cfg(test)]
            file: None,
        }
    }
    /// The suite's redirect, `None` in a shipping build — see [`paths`].
    #[cfg(test)]
    fn test_file(&self) -> Option<std::path::PathBuf> {
        self.file.clone()
    }
    #[cfg(not(test))]
    fn test_file(&self) -> Option<std::path::PathBuf> {
        None
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read the persisted place and arm the module. Called ONCE, from the boot route selection.
///
/// It does not decide anything: it reads the file and parks the answer in [`Boot::Read`], where
/// [`take_restore`] resolves it once the boot SEQUENCE has ended on a real page. Recording is held
/// off for that whole window, so the sign-in screen, the who's-watching picker and the first-run
/// question cannot overwrite the record with the page they are showing.
pub(crate) fn arm() {
    let mut st = state();
    st.armed = true;
    let rec = read_record(&paths(&st));
    match &rec {
        Some(r) => {
            st.on_disk = Some((r.profile.clone(), place_of_record(r)));
            crate::log(&format!(
                "coldstart: place on file kind={} at={}",
                r.kind, r.at
            ));
        }
        None => {
            // No record, or one this build cannot read. Treat the disk as already holding Home for
            // the account owner, so the ordinary first install — boot, look at Home, switch the
            // set off — writes nothing at all.
            st.on_disk = Some((String::new(), Place::Home));
            crate::log("coldstart: no place recorded — booting to the credential default");
        }
    }
    st.boot = Boot::Read(rec);
}

/// Is a restore still to be resolved? While this is `true` the caller is inside the boot sequence
/// and nothing is recorded.
pub(crate) fn restore_pending() -> bool {
    matches!(state().boot, Boot::Read(_))
}

/// Resolve the record read by [`arm`] into a page to restore, and start recording.
///
/// **The profile is compared HERE and not in [`arm`], and that is the whole reason this is a second
/// call.** On the who's-watching path nobody has said who they are when `arm` runs, so a comparison
/// there reads the PREVIOUS session's profile — the record would be rejected as foreign on every
/// boot of exactly the multi-user household it is keyed for, and then overwritten. By the time the
/// picker has been answered the current profile is real, and so is the answer.
///
/// `None` for an absent, unreadable or foreign record, and for one naming Home — which is where the
/// boot already is, so handing it back would only make the caller do nothing.
pub(crate) fn take_restore() -> Option<Place> {
    let mut st = state();
    if !matches!(st.boot, Boot::Read(_)) {
        return None;
    }
    let Boot::Read(rec) = std::mem::replace(&mut st.boot, Boot::Done) else {
        unreachable!()
    };
    let rec = rec?;
    if rec.profile != crate::plex::session::current_profile_key() {
        // Somebody else's page. Not restored, and `on_disk` goes UNKNOWN so this session's first
        // settle replaces the record rather than deciding the file already matches.
        st.on_disk = None;
        crate::log("coldstart: the recorded place belongs to another profile — ignoring it");
        return None;
    }
    match place_of_record(&rec) {
        Place::Home => None,
        p => Some(p),
    }
}

fn place_of_record(rec: &Record) -> Place {
    match rec.kind.as_str() {
        "library" => Place::Library { tab: rec.tab },
        // A record naming an item it cannot address is not an item. Both halves are required: the
        // machine id is the server's identity across boots, and without it `rk` names an item on
        // no machine in particular.
        "item" if !rec.machine.is_empty() && !rec.rk.is_empty() => Place::Item {
            machine: rec.machine.clone(),
            rk: rec.rk.clone(),
        },
        _ => Place::Home,
    }
}

/// Observe the page on screen. Called once per frame; writes at most once per settled page.
///
/// A no-op until [`arm`], on a boot that was never armed, and for as long as the restore is still
/// to be resolved ([`Boot::Read`]).
pub(crate) fn note(now: u32, p: PlaceRef<'_>) {
    let mut st = state();
    if !st.armed || !matches!(st.boot, Boot::Done) {
        return;
    }
    // Home is a RETURN or it is not recorded at all — see [`State::left_home`].
    if matches!(p, PlaceRef::Home) {
        if !st.left_home {
            return;
        }
    } else {
        st.left_home = true;
    }
    if !st.last.is(p) {
        st.last = Place::of(p);
        // `.max(1)` so the sentinel survives a tick counter that is genuinely 0 at boot — the same
        // guard `app.rs`'s deadline fields use.
        st.since = now.max(1);
        return;
    }
    if st.since == 0 || now.wrapping_sub(st.since) < SETTLE_MS {
        return;
    }
    st.since = 0;
    let profile = crate::plex::session::current_profile_key();
    if matches!(&st.on_disk, Some((who, place)) if *who == profile && *place == st.last) {
        return; // the file already says this, for this profile
    }
    if !flush(&mut st, &profile) {
        // Retry on the NEXT settle rather than never. `since = 0` above would otherwise make the
        // early return permanent for this page, so a one-off `ENOSPC`, a read-only mount, or the
        // window before `/media/internal` is mounted on a cold boot would cost the page for the
        // whole session instead of for 1.5 s.
        st.since = now.max(1);
    }
}

/// Write `st.last` down, best-effort, first candidate that accepts it. `false` = nowhere took it.
fn flush(st: &mut State, profile: &str) -> bool {
    let rec = Record {
        v: FORMAT,
        profile: profile.to_owned(),
        kind: match &st.last {
            Place::Home => "home",
            Place::Library { .. } => "library",
            Place::Item { .. } => "item",
        }
        .to_owned(),
        tab: match &st.last {
            Place::Library { tab } => *tab,
            _ => 0,
        },
        machine: match &st.last {
            Place::Item { machine, .. } => machine.clone(),
            _ => String::new(),
        },
        rk: match &st.last {
            Place::Item { rk, .. } => rk.clone(),
            _ => String::new(),
        },
        at: unix_now(),
    };
    let Ok(json) = serde_json::to_vec(&rec) else {
        return false;
    };
    for path in paths(st) {
        if write_atomic(&path, &json) {
            st.on_disk = Some((rec.profile, st.last.clone()));
            st.moaned = false;
            crate::log(&format!("coldstart: recorded place kind={}", rec.kind));
            return true;
        }
    }
    // Once, not once per settle: the symptom (never restores) is otherwise indistinguishable from
    // the feature not existing, and a line per retry would drown the log it has to be found in.
    if !st.moaned {
        st.moaned = true;
        crate::log(
            "coldstart: could not persist to ANY candidate path — cold start will not restore",
        );
    }
    false
}

/// Where this module reads and writes, best first.
///
/// One function in both configurations, with the test override short-circuiting it, rather than
/// two `#[cfg]` arms: an arm that replaced the whole body would leave
/// [`crate::paths::last_place_candidates`] unreferenced under `cfg(test)`, and this crate denies
/// warnings — so the production resolution would stop being COMPILED by the suite that is meant to
/// cover this module. Same trap `dev::path` records from the other side.
fn paths(st: &State) -> Vec<std::path::PathBuf> {
    if let Some(p) = st.test_file() {
        return vec![p];
    }
    crate::paths::last_place_candidates()
}

/// The first candidate that holds a record this build understands.
///
/// A candidate that is absent, unreadable, not JSON, or written by a FUTURE format is skipped
/// rather than fatal — the search simply runs out and the caller boots to the credential default.
fn read_record(cands: &[std::path::PathBuf]) -> Option<Record> {
    cands
        .iter()
        .filter_map(|p| crate::plex::session::read_owned_regular(p))
        .filter_map(|b| serde_json::from_slice::<Record>(&b).ok())
        .find(|r| r.v == FORMAT)
}

/// Whole-file replace that a reader can never catch half-written: sibling tmp, `fsync`, `rename`.
///
/// The same shape `plex::session::write_atomic` uses and for a weaker version of the same reason —
/// the writer here is racing the very event the file exists for. A mains pull between a truncate
/// and a write leaves zero bytes on disk, and zero bytes is a file that parses as nothing, so the
/// in-place write would lose the PREVIOUS place as well as the new one. The tmp file is a SIBLING
/// because `rename(2)` is only atomic within one filesystem and the jail's writable directories
/// are separate mounts.
///
/// `0600`: this is not a credential, but it does carry a `machineIdentifier`, which is a permanent
/// household fingerprint, and `/media/developer` on a rooted set is world-readable.
fn write_atomic(path: &std::path::Path, json: &[u8]) -> bool {
    crate::plex::session::write_atomic(path, json)
}

/// Wall-clock seconds, `0` when the clock will not answer. Diagnostic only — see [`Record::at`].
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module's statics are process-global, so every test here holds the crate-wide serial
    /// lock: `flush` and `take_restore` both reach `plex::session::current_profile_key`, which is
    /// shared with the suites in `plex/` and `ui/search/`, so a module-local mutex could not see
    /// the contention.
    fn fixture(name: &str) -> (std::path::PathBuf, std::sync::MutexGuard<'static, ()>) {
        let g = crate::testlock::serial();
        let p = std::env::temp_dir().join(format!("plxnative-coldstart-test-{name}.json"));
        let _ = std::fs::remove_file(&p);
        reboot(&p);
        (p, g)
    }

    /// A fresh PROCESS pointed at the same file: the state machine back at [`Boot::Cold`], nothing
    /// remembered, exactly as the next launch starts.
    fn reboot(path: &std::path::Path) {
        let mut st = state();
        *st = State::new();
        st.file = Some(path.to_path_buf());
    }

    /// A boot that lands straight on a page: `arm`, then the frame loop's first resolve.
    fn boot(path: &std::path::Path) -> Option<Place> {
        reboot(path);
        arm();
        take_restore()
    }

    /// Drive the recorder to a settled page, the way the frame loop does.
    fn settle(at: u32, p: PlaceRef<'_>) {
        note(at, p);
        note(at.wrapping_add(SETTLE_MS + 1), p);
    }

    #[test]
    fn a_recorded_place_reads_back() {
        let (path, _g) = fixture("roundtrip");
        arm();
        take_restore();
        settle(
            1_000,
            PlaceRef::Item {
                machine: "mach-A",
                rk: "42",
            },
        );
        assert!(path.exists(), "a settled page must have been written down");
        assert_eq!(
            boot(&path),
            Some(Place::Item {
                machine: "mach-A".into(),
                rk: "42".into()
            })
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The library tab survives, and Home is handed back as "nothing to do" rather than as a
    /// restore the caller then has to recognise.
    #[test]
    fn a_library_tab_round_trips_and_home_is_not_a_restore() {
        let (path, _g) = fixture("library");
        arm();
        take_restore();
        settle(1_000, PlaceRef::Library { tab: 3 });
        assert_eq!(boot(&path), Some(Place::Library { tab: 3 }));

        // …and back to Home, which IS recorded because this session left it — the restored library
        // is the page it left FROM.
        settle(9_000, PlaceRef::Library { tab: 3 });
        settle(11_000, PlaceRef::Home);
        assert_eq!(
            boot(&path),
            None,
            "Home is where an unrestored boot lands anyway"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **The failure model.** Every unreadable shape must cost the restore and nothing else —
    /// absent, empty (the mains-pull shape), truncated JSON, valid JSON of the wrong type, a
    /// record from a format this build does not know, and one with no version at all.
    #[test]
    fn a_corrupt_or_absent_record_falls_soft_to_todays_behaviour() {
        let (path, _g) = fixture("corrupt");
        assert_eq!(boot(&path), None, "absent");
        for bytes in [
            &b""[..],
            b"{",
            br#"{"v":1,"kind":"item","rk":"#,
            b"[]",
            br#"{"v":99,"kind":"library","tab":2}"#,
            br#"{"kind":"library","tab":2}"#,
            br#"{"v":1,"kind":"whatever-comes-next"}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(
                boot(&path),
                None,
                "{:?} must not restore anything",
                String::from_utf8_lossy(bytes)
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_symlink_cannot_supply_or_redirect_coldstart_state() {
        use std::os::unix::fs::symlink;
        let (path, _g) = fixture("symlink");
        let victim = path.with_extension("victim");
        std::fs::write(&victim, br#"{"v":1,"kind":"library","tab":7}"#).unwrap();
        symlink(&victim, &path).unwrap();
        assert_eq!(boot(&path), None, "a symlink must not be trusted on read");

        let _ = std::fs::remove_file(&path);
        std::fs::write(&victim, b"unchanged").unwrap();
        let mut tmp = path.file_name().unwrap().to_os_string();
        tmp.push(".tmp");
        let tmp = path.with_file_name(tmp);
        symlink(&victim, &tmp).unwrap();
        assert!(write_atomic(&path, br#"{"v":1,"kind":"home"}"#));
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(tmp);
        let _ = std::fs::remove_file(victim);
    }

    /// An `item` record missing either half of its identity is not an item: `rk` is server-local,
    /// so a record with no machine names an item on no machine in particular.
    #[test]
    fn a_half_addressed_item_is_not_restored() {
        let (path, _g) = fixture("halfitem");
        for bytes in [
            br#"{"v":1,"kind":"item","rk":"42"}"#.as_slice(),
            br#"{"v":1,"kind":"item","machine":"mach-A"}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(boot(&path), None);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A television is shared: another profile's page must not be restored onto yours. The stored
    /// profile is written by `flush` from `session::current_profile_key`, which is `""` in a host
    /// test, so this drives the foreign case by writing the record directly.
    #[test]
    fn another_profiles_place_is_not_restored() {
        let (path, _g) = fixture("profile");
        std::fs::write(
            &path,
            br#"{"v":1,"profile":"somebody-else","kind":"library","tab":2}"#,
        )
        .unwrap();
        assert_eq!(boot(&path), None);
        assert!(
            state().on_disk.is_none(),
            "a foreign record leaves the disk UNKNOWN, so the next settle replaces it"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **Nothing is recorded while the boot sequence is still running.** The who's-watching picker
    /// hands over to Home seconds after `arm`, and a recorder open through that window would write
    /// `home` over the record before anybody could ask for it — which is the shape that made this
    /// feature permanently inert for every multi-profile household.
    #[test]
    fn the_boot_sequence_can_neither_record_nor_lose_the_record() {
        let (path, _g) = fixture("bootseq");
        std::fs::write(&path, br#"{"v":1,"kind":"library","tab":4}"#).unwrap();
        reboot(&path);
        arm();
        // The picker is up: the frame loop calls neither `note` nor `take_restore`, so a very long
        // wait on that screen must change nothing at all.
        assert!(
            restore_pending(),
            "the restore waits for the sequence to end"
        );
        settle(1_000, PlaceRef::Home); // even if something did note Home…
        assert_eq!(
            read_record(&[path.clone()]).map(|r| r.tab),
            Some(4),
            "…the record is untouched"
        );
        // …and once a profile is picked the restore resolves against THAT profile.
        assert_eq!(take_restore(), Some(Place::Library { tab: 4 }));
        assert!(!restore_pending());
        let _ = std::fs::remove_file(&path);
    }

    /// **Home is a return, never a landing.** A boot whose restore was declined — the recorded
    /// item's server has not been re-registered yet, say — sits on Home, and that Home must not
    /// replace the record. Otherwise one boot with a slow roster refresh destroys it for good.
    #[test]
    fn a_declined_restore_does_not_erase_the_record() {
        let (path, _g) = fixture("declined");
        std::fs::write(
            &path,
            br#"{"v":1,"kind":"item","machine":"mach-A","rk":"42"}"#,
        )
        .unwrap();
        assert_eq!(
            boot(&path),
            Some(Place::Item {
                machine: "mach-A".into(),
                rk: "42".into()
            })
        );
        // The caller could not apply it. The app is on Home, for a long time.
        settle(1_000, PlaceRef::Home);
        settle(60_000, PlaceRef::Home);
        assert_eq!(
            read_record(&[path.clone()]).map(|r| r.rk),
            Some("42".into()),
            "still restorable next boot"
        );
        // Going somewhere real, and coming back, IS an answer to "where was I".
        settle(70_000, PlaceRef::Library { tab: 1 });
        settle(80_000, PlaceRef::Home);
        assert_eq!(
            read_record(&[path.clone()]).map(|r| r.kind),
            Some("home".into())
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Nothing is written before [`arm`], and nothing is written for a page that has not settled —
    /// a browse that walks through six pages in a second must cost one write, not six.
    #[test]
    fn only_a_settled_page_is_written() {
        let (path, _g) = fixture("settle");
        note(1_000, PlaceRef::Library { tab: 1 });
        note(9_000, PlaceRef::Library { tab: 1 });
        assert!(!path.exists(), "not armed");

        arm();
        take_restore();
        for (i, tab) in [1u32, 2, 3, 4, 5].iter().enumerate() {
            note(1_000 + i as u32 * 200, PlaceRef::Library { tab: *tab });
        }
        assert!(!path.exists(), "nothing settled");
        note(1_000 + 5 * 200 + SETTLE_MS, PlaceRef::Library { tab: 5 });
        assert_eq!(read_record(&[path.clone()]).map(|r| r.tab), Some(5));
        let _ = std::fs::remove_file(&path);
    }

    /// A page already on disk is not written again — the recorder must not put a file write on
    /// every 1.5 s of sitting still.
    #[test]
    fn an_unchanged_place_is_not_rewritten() {
        let (path, _g) = fixture("rewrite");
        arm();
        take_restore();
        settle(1_000, PlaceRef::Library { tab: 2 });
        assert!(std::fs::metadata(&path).unwrap().len() > 0);
        std::fs::write(&path, b"x").unwrap(); // a marker only a second write would erase
        settle(20_000, PlaceRef::Library { tab: 2 });
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"x",
            "the same page must not be written twice"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// An unwritable location costs the feature and nothing else — no panic, one log line however
    /// many pages are visited, and a RETRY on the next settle rather than a page given up on.
    #[test]
    fn an_unwritable_location_is_survivable_and_retried() {
        let (path, _g) = fixture("unwritable");
        let dead = std::path::PathBuf::from("/proc/plxnative-not-a-directory/lastplace.json");
        arm();
        take_restore();
        state().file = Some(dead);
        settle(1_000, PlaceRef::Library { tab: 1 });
        assert!(state().moaned, "the failure is logged");
        assert!(
            !matches!(&state().on_disk, Some((_, Place::Library { tab: 1 }))),
            "a write that failed must not be believed to have landed"
        );
        assert!(
            state().since != 0,
            "the same page is armed to try again, not given up on"
        );

        // The location comes back (a mount landing late, space freed): the SAME page writes without
        // the user having to navigate away and back.
        state().file = Some(path.clone());
        note(1_000 + 2 * SETTLE_MS + 2, PlaceRef::Library { tab: 1 });
        assert_eq!(read_record(&[path.clone()]).map(|r| r.tab), Some(1));
        let _ = std::fs::remove_file(&path);
    }
}
