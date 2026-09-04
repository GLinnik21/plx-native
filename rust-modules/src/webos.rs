//! Which webOS this television actually is — and the one thing the app ever ASKS the platform to
//! do, which is to take the screen back ([`go_home`]).
//!
//! # Why the app needs to know, when it never did before
//!
//! Until now the only thing it asked was "does `libAcbAPI` exist" (`starfish.c`'s `vp_mode`), which
//! splits the world at exactly webOS 5.0 and nowhere else. That was enough while 4.x was the only
//! target. It is not enough now: Kodi — which plays video on 5, 6 and 10 — carries gates at
//! `>= 6` (audio re-setup) and `< 11` / `>= 11` (a seek fallback, a changed signature), and with a
//! single boolean those are literally inexpressible here.
//!
//! The more immediate value is smaller and worth more: **a bug report from hardware nobody here
//! owns currently cannot say which firmware it came from.** The webOS 6/10 playback failure was
//! reported as one thing and is probably two, and the logs could not tell them apart.
//!
//! # Where it comes from
//!
//! `/var/run/nyx/os_info.json`, read once, at boot. Kodi asks
//! `luna://com.webos.service.config/getConfigs` for `tv.nyx.platformCode`; this is the same
//! information from the file nyx writes, and it needs no LS2 client, no subscription and no
//! thread. Verified present on the dev set (webOS 4.5), which reports:
//!
//! ```text
//! "webos_release": "4.10.2",  "webos_release_codename": "goldilocks2-grampians"
//! ```
//!
//! The CODENAME is the more useful half and is why this reads the file rather than a version
//! service: webosbrew's own compatibility data buckets firmware by codename, one library set per
//! bucket (their `library-version` guide — `goldilocks` is 4.0~4.4, `goldilocks2` 4.5~4.10). So
//! logging it says which of THEIR buckets a report belongs to, not just a number.
//!
//! Parsed by hand rather than through a JSON crate: this is a flat object of string values written
//! by the platform, the crate has no JSON dependency, and a parser that cannot fail is the right
//! shape for something that must never keep the app from booting.
use std::sync::OnceLock;

const OS_INFO: &str = "/var/run/nyx/os_info.json";

/// What the set said about itself. Owned strings rather than borrows into the file, because the
/// file is read once and dropped; `OnceLock` because this is written exactly once, at boot, and
/// read from the render thread every frame the diagnostics panel is up.
#[derive(Debug, Default, Clone)]
pub(crate) struct Info {
    /// e.g. "4.10.2" — empty when unknown
    pub release: String,
    /// e.g. "goldilocks2-grampians" — webosbrew buckets firmware by this
    pub codename: String,
    /// e.g. "4.1.0"
    pub api: String,
    /// e.g. "webOS TV"
    pub name: String,
    /// leading component of `release`, or 0 when unknown
    pub major: u32,
}

static INFO: OnceLock<Info> = OnceLock::new();

/// What the set reported. All-empty with `major == 0` when the file could not be read — which is
/// the honest answer and is what the panel prints.
pub(crate) fn info() -> &'static Info {
    INFO.get_or_init(Info::default)
}

/// Pull `"key": "value"` out of a flat JSON object. Returns None rather than erroring: nothing
/// here is worth failing a boot over.
fn field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let at = s.find(&format!("\"{key}\""))?;
    let rest = &s[at + key.len() + 2..];
    let colon = rest.find(':')?;
    let open = rest[colon..].find('"')? + colon + 1;
    let close = rest[open..].find('"')? + open;
    Some(&rest[open..close])
}

/// Everything [`probe`] extracts, as a pure function of the file's text — so the parse is testable
/// without a filesystem, and an unreadable file and an unparseable one land on the same value.
fn parse(s: &str) -> Info {
    let get = |k: &str| field(s, k).unwrap_or_default().to_string();
    let release = get("webos_release");
    let major = release
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .unwrap_or(0);
    Info {
        release,
        codename: get("webos_release_codename"),
        api: get("webos_api_version"),
        name: get("webos_name"),
        major,
    }
}

/// Read it once and log it. Called at boot; safe to call when the file does not exist.
pub(crate) fn probe() {
    let info = match std::fs::read_to_string(OS_INFO) {
        Ok(s) => parse(&s),
        Err(e) => {
            crate::log(&format!(
                "webos: {OS_INFO} unreadable ({e}) — version unknown"
            ));
            Info::default()
        }
    };
    if info.major > 0 {
        crate::log(&format!(
            "webos: {} release={} codename={} api={} major={}",
            info.name, info.release, info.codename, info.api, info.major
        ));
    }
    let _ = INFO.set(info);
    probe_hw();
}

// ---- which SET this is, as opposed to which webOS ---------------------------------------------

/// nyx's other file. Same directory, same flat shape, written by the same platform component.
const DEVICE_INFO: &str = "/var/run/nyx/device_info.json";

/// The hardware, for a report that comes from a television nobody here owns.
///
/// [`Info`] answers "which firmware"; this answers "which SET". They are different questions and
/// the second one has been unanswerable: a webOS 6 playback failure on an OLED and on an LCD of
/// the same firmware are two bugs, and nothing in a log said which had been seen. The **board** is
/// the SoC generation (`k8hp`, `o22`, …) and is the field a decode or plane failure actually
/// correlates with.
///
/// Every field is EMPTY when unknown, never a plausible default — same rule as [`Info`], for the
/// same reason: a snapshot that invents a model is worse than one that admits it does not know.
#[derive(Debug, Default, Clone)]
pub(crate) struct Hardware {
    /// e.g. "49SM9000PLA"
    pub model: String,
    /// e.g. "HE_DTV_W19H_AFAAABAA" or the SoC name — whichever key this firmware carries
    pub board: String,
    pub hw_revision: String,
}

impl Hardware {
    /// The set as one line — `model · board · hw` with the empty parts left out, and an EMPTY
    /// string when nothing answered (the caller decides what "unknown" reads as on its surface).
    /// One definition for the two photographable surfaces that print it, the diagnostics panel's
    /// "Set" row and the failure read-out's support line, so they cannot drift.
    pub(crate) fn set_line(&self) -> String {
        [
            self.model.as_str(),
            self.board.as_str(),
            self.hw_revision.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

impl Info {
    /// `webOS 4.10.2`, or `webOS unknown` when the file could not be read — the release is the
    /// one field a stranger's report needs, and the word "unknown" is the honest reading of an
    /// empty one rather than a plausible default.
    pub(crate) fn release_line(&self) -> String {
        if self.major == 0 {
            "webOS unknown".to_string()
        } else {
            format!("webOS {}", self.release)
        }
    }
}

static HW: OnceLock<Hardware> = OnceLock::new();

/// What the set is. All-empty when the file could not be read.
///
/// Shared by the opt-in compatibility telemetry and the local lab snapshot. The values come from
/// the same boot probe, so diagnostics never need to rediscover or reinterpret the device later.
pub(crate) fn device() -> &'static Hardware {
    HW.get_or_init(Hardware::default)
}

/// Pure, and tolerant of which spelling a firmware uses: nyx has carried both snake_case and
/// camelCase for these keys across releases, and we have exactly one device to check against — so
/// each field takes the first spelling that answers rather than betting on one.
fn parse_hw(s: &str) -> Hardware {
    let first = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|k| field(s, k))
            .unwrap_or_default()
            .to_string()
    };
    Hardware {
        model: first(&["modelName", "model_name", "device_name"]),
        board: first(&["boardType", "board_type", "platform_code", "chip_name"]),
        hw_revision: first(&["hardware_revision", "hardwareRevision", "hardware_version"]),
    }
}

/// Read it once and log it. Called from [`probe`], so it shares that call's one boot slot.
fn probe_hw() {
    let hw = match std::fs::read_to_string(DEVICE_INFO) {
        Ok(s) => parse_hw(&s),
        Err(e) => {
            crate::log(&format!(
                "webos: {DEVICE_INFO} unreadable ({e}) — model/board unknown"
            ));
            Hardware::default()
        }
    };
    if !hw.model.is_empty() || !hw.board.is_empty() {
        crate::log(&format!(
            "webos: model={} board={} hw={}",
            hw.model, hw.board, hw.hw_revision
        ));
    }
    let _ = HW.set(hw);
}

// ---- the ROOT press: give the screen back, without ending the process -------------------------

/// **Show the television's own Home, and keep running.**
///
/// This is what BACK does when the app's navigation has nowhere left to go — see `app.rs`'s
/// `back_at_root`. It is NOT a quit: the process stays alive, webOS backgrounds it (`0x103`/`0x104`
/// reach the SDL loop, and `0x105`/`0x106` when the launcher tile brings it back), and the app
/// comes back as the SAME LIVE PROCESS. Not necessarily to the same picture, and not necessarily to
/// the same route: workers keep running while backgrounded, so a sign-in that was mid-flow may have
/// completed and routed on, and one this press had to restart (`app.rs::after_cancel`) shows a
/// fresh QR by design. The contract is the process, and everything else is whatever the app would
/// have been doing had nobody pressed anything.
///
/// # Why this is the platform's answer and not ours
///
/// LG's own back-button guide states the behaviour the platform performs when BACK is pressed at
/// an app's entry page: *"a popup asking whether to exit the app is displayed on webOS TV 6.0 or
/// higher, or the **Home launcher is launched** on webOS TV 5.0 or lower"*
/// (<https://webostv.developer.lge.com/develop/guides/back-button>, LG vendor documentation — it
/// describes the WEB runtime's `webOS.platformBack()`, which a native app cannot call, but it is
/// LG stating what the ROOT press means on this platform). The dev set is 4.10.2, so "launch the
/// Home launcher" is the behaviour to reproduce, and the app's previous "Exit PlxNative?" question
/// was the webOS 6+ shape applied to a webOS 4 television — and it quit, which even there is only
/// one of the two answers.
///
/// # The two mechanisms, in the order they are tried
///
/// 1. **SAM.** `luna://com.webos.applicationManager/launch` with the launcher's id. Same service
///    and method this repo already drives against this television from the Makefile and
///    `tools/tv-session.sh` (`launch`, `closeByAppId`), so the bus, the method and the payload
///    shape are device-proven; only the id is new. **Device-verified 2026-09-04 (webOS 4.10.2):**
///    `SAM accepted in 16ms`, and the launcher comes up as the RIBBON over the app — the app stays
///    on screen behind it, keeps drawing, keeps its pid, receives NO `LIFECYCLE: background`, and a
///    SAM `launch` of the app's own id brings it back with no ribbon. That is what the HOME key
///    does on this firmware, so it is the behaviour; this doc used to say launching Home
///    "backgrounds us", which is the webOS 5+ full-screen launcher's shape and not this set's. It
///    took a plain anonymous `LSRegister` to get there — [`ls2`]'s module doc has the table of
///    what the hub answers to each registration shape.
/// 2. **Minimize the surface.** `SDL_MinimizeWindow`, which on LG's SDL fork reaches the webOS
///    shell. The fork does bind that protocol: the harvested `libSDL2-2.0.so.0.4.1` off this set
///    carries `wl_webos_shell`, `wl_webos_shell_interface` and `wl_webos_shell_surface_interface`
///    in `.rodata`, and it is how `SDL_WEBOS_ACCESS_POLICY_KEYS_BACK` (set by `src/main.c`) becomes
///    the surface property that lets BACK reach this app at all. `wl_webos_shell_surface.set_state`
///    with `WL_WEBOS_SHELL_SURFACE_STATE_MINIMIZED = 1` is LG's own protocol
///    (`wayland-webos-shell-client-protocol.h` / `webos-shell.xml` in the NDK sysroot), and it is
///    what Kodi's webOS port drives from its `SetMinimized`. What is NOT proven from the binary is
///    that LG's fork wires `SDL_MinimizeWindow` to it — the library is stripped and the driver's
///    hook is an internal function pointer — so this is the FALLBACK and not the mechanism: SDL's
///    own contract makes an unimplemented hook a silent no-op, which is the correct failure here.
///
/// If BOTH legs fail the user stays exactly where they were, which is the right last resort: the
/// alternative is ending the process, and "must not terminate the application" is the whole of what
/// was asked for. `SDL_MinimizeWindow` returns void, so the app cannot even know the second leg
/// failed — the log line says what was ASKED, never that it worked, and the screenshot is what
/// says whether it did.
///
/// `/tmp/plxnative-gohome=<sam|minimize>` forces one leg, so both can be exercised in one device
/// session instead of only whichever happens to answer first. Compiled out under `RELEASE=1`.
///
/// **Not rate-limited here** — [`take_root_press`] is, and `app.rs` claims the press through it
/// before it does anything else, `auth::cancel` included. Call this only having claimed one.
pub(crate) fn go_home() {
    #[cfg(test)]
    HOME_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let forced = crate::dev::read("gohome");
    let forced = forced.as_deref().map(str::trim).unwrap_or("");
    let mode = match forced {
        "sam" | "minimize" | "probe" => forced,
        _ => "auto",
    };
    // The FIRST line of every root press, and the one that makes the rest of them readable: which
    // legs are even eligible. Without it a reader cannot tell a forced run from an ordinary one,
    // and the device evidence for this change is read by somebody who did not write it.
    crate::log(&format!("gohome: request mode={mode}"));
    if mode == "probe" {
        ls2_probe();
        return;
    }
    if mode != "minimize" && launch_home() {
        return;
    }
    if mode == "sam" {
        crate::log("gohome: no fallback — the trigger forced SAM only");
        return;
    }
    minimize();
}

/// How long one root press speaks for. Comfortably longer than [`ls2::BUDGET`], so a burst of taps
/// cannot queue several full-budget stalls back to back, and short enough to be under the time it
/// takes a person to notice nothing happened and press again.
const COOLDOWN: std::time::Duration = std::time::Duration::from_millis(2000);

/// When the last root press was claimed, and the whole of the latch. `Mutex` rather than an atomic
/// clock because [`std::time::Instant`] is opaque: this is touched once per BACK press at a root,
/// never on a frame path.
static LAST_REQUEST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// **Claim the root press.** `true` when this one is live; `false` when a recent one still speaks
/// for it, and then the WHOLE press must do nothing.
///
/// A HELD back never gets here — a hardware auto-repeat carries `state & 0x100` and goes to
/// `app.rs`'s `on_auto_repeat`, which has no BACK action — but five separate taps are five separate
/// fresh presses. Two things make that expensive: on the failing path each one spends
/// [`ls2::BUDGET`] on the SDL main thread, and on the sign-in screen each one runs `auth::cancel`,
/// which is destructive whatever it answers. So the claim is what `app.rs` takes FIRST, before
/// either.
pub(crate) fn take_root_press() -> bool {
    take_root_press_at(std::time::Instant::now())
}

/// [`take_root_press`] with the clock passed in — the whole of the expiry rule, so the boundary is
/// gradeable without sleeping through it.
fn take_root_press_at(now: std::time::Instant) -> bool {
    let mut last = LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(age) = last.map(|t| now.saturating_duration_since(t)) {
        if age < COOLDOWN {
            crate::log(&format!(
                "gohome: a root press {} ms ago still speaks for this one — ignoring it",
                age.as_millis()
            ));
            return false;
        }
    }
    *last = Some(now);
    true
}

/// **Hand a claim back**, for the press that turned out not to be a root press after all — the one
/// on the sign-in screen or the picker that DID have somewhere to go inside the app. Without this
/// the cooldown would swallow the real root BACK the user presses a moment later on the Home they
/// were just returned to.
pub(crate) fn release_root_press() {
    *LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The Home launcher's app id.
///
/// Community tier (webosbrew's commands cheatsheet and the widely copied
/// `luna-send … /launch '{"id":"com.webos.app.home"}'` recipe), which is why the reply is graded
/// rather than assumed: a wrong id comes back `returnValue: false` and falls through to the
/// minimize leg instead of leaving the user on a screen whose BACK did nothing.
const HOME_APP_ID: &str = "com.webos.app.home";

#[cfg(test)]
static HOME_REQUESTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// How many root presses [`go_home`] has actually ACTED on, this process — a press swallowed by
/// the cooldown does not count. **Test-only**, and the only way a host test can grade a call whose
/// whole effect is on a television.
#[cfg(test)]
pub(crate) fn home_requests() -> u32 {
    HOME_REQUESTS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod go_home_tests {
    use super::*;
    use std::time::Instant;

    /// **The expiry BOUNDARY, on a clock the test owns.** Driving it through [`take_root_press`]
    /// alone could only ever prove the suppressing half: clearing the latch to simulate time
    /// passing bypasses the elapsed-time branch entirely, so that test would pass with an
    /// infinite `COOLDOWN`. This one never sleeps and still grades the comparison.
    #[test]
    fn a_claim_speaks_for_exactly_the_cooldown_and_not_a_moment_longer() {
        let _g = crate::testlock::serial();
        let base = Instant::now();
        release_root_press();
        assert!(take_root_press_at(base), "a cold latch admits the press");
        assert!(
            !take_root_press_at(base + COOLDOWN / 2),
            "…and speaks for the whole cooldown"
        );
        release_root_press();
        assert!(take_root_press_at(base));
        assert!(
            take_root_press_at(base + COOLDOWN),
            "…but not past its end: a first attempt that achieved nothing stays retryable"
        );
        release_root_press();
    }

    /// The other half, through the real entry points: a burst of taps is ONE platform call, and a
    /// press handed back leaves the next one live.
    #[test]
    fn a_burst_of_root_presses_is_one_platform_call_and_a_release_undoes_the_claim() {
        let _g = crate::testlock::serial();
        release_root_press();
        let before = home_requests();
        for _ in 0..5 {
            if take_root_press() {
                go_home();
            }
        }
        assert_eq!(
            home_requests(),
            before + 1,
            "five taps must not queue five platform calls"
        );
        release_root_press();
        assert!(
            take_root_press(),
            "a handed-back claim must not swallow the next root press"
        );
        release_root_press();
    }
}

#[cfg(any(feature = "hostsim", test))]
fn launch_home() -> bool {
    crate::log(&format!(
        "gohome: no LS2 bus off-device — the root press would launch {HOME_APP_ID} on a television"
    ));
    true
}

#[cfg(any(feature = "hostsim", test))]
fn minimize() {}

#[cfg(any(feature = "hostsim", test))]
fn ls2_probe() {
    crate::log("gohome: no LS2 bus off-device — nothing to probe");
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
fn ls2_probe() {
    ls2::probe();
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
fn launch_home() -> bool {
    let payload = format!("{{\"id\":\"{HOME_APP_ID}\"}}");
    let started = std::time::Instant::now();
    let outcome = ls2::call_once("luna://com.webos.applicationManager/launch", &payload);
    let ms = started.elapsed().as_millis();
    match outcome {
        // The whole reply, not a parse: it is one short platform-authored line, it names the
        // refusal when there is one, and `diag::scrub` runs over it like every other log write. The
        // elapsed time is logged beside it because `ls2::BUDGET` was chosen without a measurement,
        // and this is the only place one can ever be taken.
        Ok(reply) => {
            let ok =
                reply.contains("\"returnValue\":true") || reply.contains("\"returnValue\": true");
            let verdict = if ok { "accepted" } else { "rejected" };
            crate::log(&format!("gohome: SAM {verdict} in {ms}ms → {reply}"));
            ok
        }
        // **Four different failures used to arrive as one sentence**, which is exactly the kind of
        // log that wastes a device session: "SAM did not answer" read the same whether the bus
        // refused this app a registration, the call was never submitted, or the reply really did
        // time out. They are three different bugs and only one of them is about SAM.
        Err(ls2::Fail::Setup { stage, detail }) if detail.is_empty() => {
            crate::log(&format!("gohome: LS2 setup failed stage={stage} after {ms}ms"));
            false
        }
        // The hub's own words, when it gave any. The register refusal that shipped with this
        // branch (`Can not find service "" permissions`) was legible ONLY in ls-hubd's log,
        // which nobody reading the app's evidence knew to open.
        Err(ls2::Fail::Setup { stage, detail }) => {
            crate::log(&format!(
                "gohome: LS2 setup failed stage={stage} after {ms}ms — {detail}"
            ));
            false
        }
        Err(ls2::Fail::Timeout) => {
            crate::log(&format!("gohome: SAM timed out in {ms}ms"));
            false
        }
    }
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
fn minimize() {
    let win = WINDOW.load(std::sync::atomic::Ordering::Relaxed);
    if win.is_null() {
        crate::log("gohome: no window bound — cannot leave the foreground");
        return;
    }
    // Returns void: SDL has no way to say whether the driver implemented the hook, so this line
    // says what was ASKED and never that it worked. The screenshot is the evidence.
    unsafe { SDL_MinimizeWindow(win) };
    crate::log("gohome: fallback=SDL minimize — asked, and SDL cannot say whether it took");
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
static WINDOW: std::sync::atomic::AtomicPtr<std::os::raw::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(all(not(feature = "hostsim"), not(test)))]
extern "C" {
    fn SDL_MinimizeWindow(win: *mut std::os::raw::c_void);
}

/// Hand this module the SDL window, once, at boot — `textinput::bind`'s shape and for its reason:
/// the window is created deep inside `plex_run` and the platform call needs it a long way from
/// there.
#[cfg(all(not(feature = "hostsim"), not(test)))]
pub(crate) fn bind_window(win: *mut std::os::raw::c_void) {
    WINDOW.store(win, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(any(feature = "hostsim", test))]
pub(crate) fn bind_window(_win: *mut std::os::raw::c_void) {}

/// One LS2 request/reply, on the calling thread.
///
/// **The one LS2 client this process has**, shared by every caller that speaks to the bus from
/// this application: [`super::launch_home`] (one 600 ms call on the SDL main thread) and
/// `keymanager::platform::Client` (a registration kept alive across begin → finish, 4 s budget).
/// Until 2026-09-04 it was two private copies, both registering with
/// `LSRegisterApplicationService(NULL, app_id)`, and on this set that registration is REFUSED:
/// `ls-hubd LSHUB_ROLE_FILE: Can not find service "" permissions for executable ".../plxnative"`
/// once per root press, the app logging `stage=register after 2ms` having freed the LSError
/// unread. The role file appinstalld generates for a native app
/// (`/var/palm/ls2-dev/roles/{prv,pub}/<app id>.json` on a Developer-Mode set, read off the
/// television) allows `""` and the app id as names, and grants outbound permissions to the app id
/// only.
///
/// **What the hub actually answers, measured on the set through [`probe`] (2026-09-04):**
///
/// | registration | answer |
/// |---|---|
/// | `LSRegisterApplicationService(app_id, app_id)` | `-1028 Attempted to register for a service name that already exists` — this PROCESS already holds the app id: `ls-monitor -l` lists it beside an anonymous client, both ours, from boot; ACB is the one component handed the app id (`AcbAPI_initialize`, `player::acb_init` at boot) |
/// | `LSRegisterApplicationService(NULL, app_id)` | `-1027 Invalid permissions for (null)` — the `""` name has no permissions entry |
/// | **`LSRegister(NULL)`** | registered, and `com.webos.applicationManager/getForegroundAppInfo` answered it |
///
/// So every caller registers as a plain anonymous client, [`register`]. The role file still
/// decides what such a client may CALL — the probe proves SAM's read-only method; `launch_home`
/// grades the reply of the one that matters — and the hub's refusal, when there is one, travels
/// in [`RegisterFail`] to the caller's log line instead of being freed. Anonymous clients do not
/// collide with each other, so nothing here is serialised; a registration lives for one caller's
/// use and is unregistered on drop, as both copies always did.
#[cfg(all(not(feature = "hostsim"), not(test)))]
pub(crate) mod ls2 {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int, c_void};
    use std::time::{Duration, Instant};

    /// How long [`call_once`] waits for SAM's reply.
    ///
    /// **Be honest about what this costs when it is SPENT**: the call runs on the SDL main thread,
    /// so a bus that never answers stops event handling, updates, drawing and the remote FIFO for
    /// the whole budget — about 36 frames at 60 Hz, not the "one dropped frame" this comment
    /// claimed when it was written. It is not an end-to-end ceiling either: registration,
    /// cancellation and unregistration sit outside it.
    ///
    /// It is still the right shape, for two reasons. The press being served is a request to LEAVE,
    /// so the frames being missed are the last ones anybody looks at; and the healthy path does not
    /// spend this — `go_home` logs the measured round trip on every call precisely so the budget
    /// stops being a guess. [`super::take_root_press`]'s cooldown is what bounds the failing
    /// path.
    const BUDGET: Duration = Duration::from_millis(600);

    #[repr(C)]
    struct LSError {
        error_code: c_int,
        message: *mut c_char,
        file: *const c_char,
        line: c_int,
        func: *const c_char,
        padding: *mut c_void,
        magic: libc::c_ulong,
    }

    extern "C" {
        fn LSErrorInit(error: *mut LSError) -> bool;
        fn LSErrorFree(error: *mut LSError);
        fn LSRegisterApplicationService(
            name: *const c_char,
            app_id: *const c_char,
            handle: *mut *mut c_void,
            error: *mut LSError,
        ) -> bool;
        fn LSRegister(name: *const c_char, handle: *mut *mut c_void, error: *mut LSError) -> bool;
        fn LSGmainContextAttach(
            handle: *mut c_void,
            context: *mut c_void,
            error: *mut LSError,
        ) -> bool;
        fn LSCallOneReply(
            handle: *mut c_void,
            uri: *const c_char,
            payload: *const c_char,
            callback: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
            context: *mut c_void,
            token: *mut libc::c_ulong,
            error: *mut LSError,
        ) -> bool;
        fn LSCallCancel(handle: *mut c_void, token: libc::c_ulong, error: *mut LSError) -> bool;
        fn LSMessageGetPayload(message: *mut c_void) -> *const c_char;
        fn LSUnregister(handle: *mut c_void, error: *mut LSError) -> bool;
        fn g_main_context_new() -> *mut c_void;
        fn g_main_context_iteration(context: *mut c_void, may_block: c_int) -> c_int;
        fn g_main_context_unref(context: *mut c_void);
    }

    extern "C" fn on_reply(_h: *mut c_void, message: *mut c_void, context: *mut c_void) -> bool {
        if context.is_null() || message.is_null() {
            return true;
        }
        let payload = unsafe { LSMessageGetPayload(message) };
        if !payload.is_null() {
            unsafe {
                *(context as *mut Option<String>) =
                    Some(CStr::from_ptr(payload).to_string_lossy().into_owned());
            }
        }
        true
    }

    /// The app id as a C string, for [`probe`]'s app-service shapes — `register` itself passes
    /// no name at all (module doc).
    fn app_id_cstring() -> Result<CString, RegisterFail> {
        CString::new(crate::paths::app_id()).map_err(|_| RegisterFail::Setup {
            stage: "app-id",
            detail: String::new(),
        })
    }

    /// The hub's own account of a failure, for a log line — `LSError` is freed by the caller and
    /// its message with it, which is how the register refusal went unexplained for a device session.
    fn error_text(error: &LSError) -> String {
        if error.message.is_null() {
            return format!("code {}", error.error_code);
        }
        let msg = unsafe { CStr::from_ptr(error.message) }.to_string_lossy();
        format!("code {}: {}", error.error_code, msg.trim())
    }

    fn reset(error: &mut LSError) {
        unsafe {
            LSErrorFree(error);
            *error = std::mem::zeroed();
            LSErrorInit(error);
        }
    }

    /// Why there is no registration: the STAGE (`app-id`, `glib-context`, `register`, `attach`)
    /// and, when the hub gave one, its own message.
    #[derive(Debug)]
    pub(crate) enum RegisterFail {
        Setup { stage: &'static str, detail: String },
    }

    impl std::fmt::Display for RegisterFail {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RegisterFail::Setup { stage, detail } if detail.is_empty() => {
                    write!(f, "setup failed stage={stage}")
                }
                RegisterFail::Setup { stage, detail } => {
                    write!(f, "setup failed stage={stage} ({detail})")
                }
            }
        }
    }

    /// A live registration on the bus, with the private glib context its replies arrive on.
    /// Drop unregisters it and frees the context — or, should the hub refuse the unregister,
    /// leaks the context on purpose rather than free storage a live handle is attached to.
    pub(crate) struct Registration {
        handle: *mut c_void,
        context: *mut c_void,
    }

    /// Register as a plain anonymous client — the one shape the hub accepts from this process
    /// (module doc) — on a private glib context.
    pub(crate) fn register() -> Result<Registration, RegisterFail> {
        let mut error: LSError = unsafe { std::mem::zeroed() };
        unsafe { LSErrorInit(&mut error) };
        let context = unsafe { g_main_context_new() };
        if context.is_null() {
            unsafe { LSErrorFree(&mut error) };
            return Err(RegisterFail::Setup {
                stage: "glib-context",
                detail: String::new(),
            });
        }
        let mut handle = std::ptr::null_mut();
        let registered = unsafe { LSRegister(std::ptr::null(), &mut handle, &mut error) };
        if !registered || handle.is_null() {
            let detail = error_text(&error);
            unsafe {
                LSErrorFree(&mut error);
                g_main_context_unref(context);
            }
            return Err(RegisterFail::Setup {
                stage: "register",
                detail,
            });
        }
        reset(&mut error);
        if !unsafe { LSGmainContextAttach(handle, context, &mut error) } {
            let detail = error_text(&error);
            reset(&mut error);
            unsafe {
                LSUnregister(handle, &mut error);
                LSErrorFree(&mut error);
                g_main_context_unref(context);
            }
            return Err(RegisterFail::Setup {
                stage: "attach",
                detail,
            });
        }
        unsafe { LSErrorFree(&mut error) };
        Ok(Registration { handle, context })
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            let mut error: LSError = unsafe { std::mem::zeroed() };
            unsafe { LSErrorInit(&mut error) };
            let unregistered = unsafe { LSUnregister(self.handle, &mut error) };
            if unregistered {
                unsafe { g_main_context_unref(self.context) };
            } else {
                // A handle that would not unregister is still attached to this context, so the
                // context is leaked rather than freed under it — a bounded leak, once per failed
                // teardown, against a use-after-free (Codex review, 2026-09-04).
                crate::log(&format!(
                    "ls2: unregister refused — leaking its glib context ({})",
                    error_text(&error)
                ));
            }
            unsafe { LSErrorFree(&mut error) };
        }
    }

    /// Why a call produced no reply. **Not one value**, because the ways this fails are different
    /// bugs and only one of them is about the service being called — and the person reading the
    /// log will not be the person who wrote this.
    pub(crate) enum Fail {
        /// The bus, glib or the call itself never got as far as being sent. The stage names which,
        /// and `detail` carries the hub's words when it gave any.
        Setup { stage: &'static str, detail: String },
        /// It WAS sent and the budget elapsed with no reply.
        Timeout,
    }

    impl From<RegisterFail> for Fail {
        fn from(f: RegisterFail) -> Self {
            match f {
                RegisterFail::Setup { stage, detail } => Fail::Setup { stage, detail },
            }
        }
    }

    impl Registration {
        /// One call, one reply, waited for up to `budget` on this registration's own context. An
        /// `Err` never means "the method said no" — a refusal comes back as the platform's own JSON
        /// in the `Ok`, for the caller to grade.
        pub(crate) fn call(&self, uri: &str, payload: &str, budget: Duration) -> Result<String, Fail> {
            let setup = |stage| Fail::Setup {
                stage,
                detail: String::new(),
            };
            let uri = CString::new(uri).map_err(|_| setup("uri"))?;
            let payload = CString::new(payload).map_err(|_| setup("payload"))?;
            let mut error: LSError = unsafe { std::mem::zeroed() };
            unsafe { LSErrorInit(&mut error) };
            // The reply slot lives on the HEAP, not this frame: a registration can outlive this
            // call (`keymanager` keeps one across begin → finish), so a late dispatch of a call
            // that timed out would otherwise write into a returned stack frame. It is taken back
            // below on every path except a refused cancel, where it is leaked on purpose.
            let slot: *mut Option<String> = Box::into_raw(Box::new(None));
            let mut token = 0;
            let called = unsafe {
                LSCallOneReply(
                    self.handle,
                    uri.as_ptr(),
                    payload.as_ptr(),
                    on_reply,
                    slot as *mut c_void,
                    &mut token,
                    &mut error,
                )
            };
            if !called {
                let detail = error_text(&error);
                unsafe {
                    LSErrorFree(&mut error);
                    drop(Box::from_raw(slot));
                }
                return Err(Fail::Setup {
                    stage: "call",
                    detail,
                });
            }
            let until = Instant::now() + budget;
            while unsafe { (*slot).is_none() } && Instant::now() < until {
                unsafe { g_main_context_iteration(self.context, 0) };
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut owned = true;
            if unsafe { (*slot).is_none() } && token != 0 {
                reset(&mut error);
                // A successful cancel removes the callback; a REFUSED one leaves it registered,
                // and then the slot has to stay valid for as long as this handle might dispatch
                // into it — which is the registration's lifetime, so it is leaked (Codex review,
                // 2026-09-04: dropping it regardless was a use-after-free on a retained handle).
                if !unsafe { LSCallCancel(self.handle, token, &mut error) } {
                    crate::log(&format!(
                        "ls2: cancel refused after a timeout — leaking the reply slot ({})",
                        error_text(&error)
                    ));
                    owned = false;
                }
            }
            unsafe { LSErrorFree(&mut error) };
            if !owned {
                return Err(Fail::Timeout);
            }
            let reply = unsafe { Box::from_raw(slot) };
            reply.ok_or(Fail::Timeout)
        }
    }

    /// Register, call, wait up to [`BUDGET`], unregister — the main-thread shape.
    pub(super) fn call_once(uri: &str, payload: &str) -> Result<String, Fail> {
        let registration = register()?;
        registration.call(uri, payload, BUDGET)
    }

    /// `/tmp/plxnative-gohome=probe`: try every registration shape the role file could accept, log
    /// what the hub answers to each — the LSError text this app freed unread for a whole device
    /// session — and, through whichever registered, ONE read-only SAM call. Evidence, not a leg:
    /// its answer is the table in the module doc and decided [`register`]'s shape, and it runs
    /// jailed as the app, under the app's uid and exe path, the only place the hub's answer means
    /// anything. Kept so the next firmware can be asked the same question in one root press.
    pub(super) fn probe() {
        let app_id = match app_id_cstring() {
            Ok(n) => n,
            Err(e) => {
                crate::log(&format!("ls2probe: {e}"));
                return;
            }
        };
        let shapes: [(&str, bool, bool); 3] = [
            ("app-service name=appid", true, true),
            ("app-service name=NULL", false, true),
            ("plain LSRegister name=NULL", false, false),
        ];
        for (label, named, app_service) in shapes {
            let mut error: LSError = unsafe { std::mem::zeroed() };
            unsafe { LSErrorInit(&mut error) };
            let mut handle = std::ptr::null_mut();
            let name = if named { app_id.as_ptr() } else { std::ptr::null() };
            let ok = unsafe {
                if app_service {
                    LSRegisterApplicationService(name, app_id.as_ptr(), &mut handle, &mut error)
                } else {
                    LSRegister(name, &mut handle, &mut error)
                }
            };
            if !ok || handle.is_null() {
                crate::log(&format!(
                    "ls2probe: {label}: register REFUSED — {}",
                    error_text(&error)
                ));
                unsafe { LSErrorFree(&mut error) };
                continue;
            }
            crate::log(&format!("ls2probe: {label}: registered"));
            reset(&mut error);
            let context = unsafe { g_main_context_new() };
            if context.is_null() {
                crate::log(&format!("ls2probe: {label}: no glib context"));
                unsafe {
                    LSUnregister(handle, &mut error);
                    LSErrorFree(&mut error);
                }
                continue;
            }
            let attached = unsafe { LSGmainContextAttach(handle, context, &mut error) };
            if attached {
                let registration = Registration { handle, context };
                let uri = "luna://com.webos.applicationManager/getForegroundAppInfo";
                match registration.call(uri, "{}", BUDGET) {
                    Ok(r) => crate::log(&format!("ls2probe: {label}: getForegroundAppInfo → {r}")),
                    Err(Fail::Timeout) => {
                        crate::log(&format!("ls2probe: {label}: getForegroundAppInfo timed out"))
                    }
                    Err(Fail::Setup { stage, detail }) => crate::log(&format!(
                        "ls2probe: {label}: call failed stage={stage} ({detail})"
                    )),
                }
            } else {
                crate::log(&format!(
                    "ls2probe: {label}: attach failed — {}",
                    error_text(&error)
                ));
                reset(&mut error);
                unsafe {
                    LSUnregister(handle, &mut error);
                    g_main_context_unref(context);
                }
            }
            unsafe { LSErrorFree(&mut error) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file off the dev set, verbatim. The parser has to survive the platform's
    /// formatting, not a tidied version of it.
    const REAL: &str = r#"{
    "core_os_kernel_version": "4.4.84-169.gld4tv.5",
    "core_os_name": "Rockhopper",
    "core_os_release": "4.10.2-31",
    "core_os_release_codename": "goldilocks2-grampians",
    "encryption_key_type": "prodkey",
    "webos_api_version": "4.1.0",
    "webos_build_datetime": "20250827000043",
    "webos_name": "webOS TV",
    "webos_prerelease": "",
    "webos_release": "4.10.2",
    "webos_release_codename": "goldilocks2-grampians"
}"#;

    #[test]
    fn reads_the_dev_sets_real_os_info() {
        assert_eq!(field(REAL, "webos_release"), Some("4.10.2"));
        assert_eq!(
            field(REAL, "webos_release_codename"),
            Some("goldilocks2-grampians")
        );
        assert_eq!(field(REAL, "webos_api_version"), Some("4.1.0"));
        assert_eq!(field(REAL, "webos_name"), Some("webOS TV"));
    }

    /// `webos_release` must not be satisfied by `core_os_release`, which appears FIRST in the file
    /// and whose value ("4.10.2-31") differs. A substring search that ignored the quotes would
    /// return the wrong field on every real device.
    #[test]
    fn does_not_match_a_longer_key_that_contains_it() {
        assert_ne!(field(REAL, "webos_release"), field(REAL, "core_os_release"));
        assert_eq!(field(REAL, "core_os_release"), Some("4.10.2-31"));
    }

    /// An empty value is a value, not a miss — `webos_prerelease` is empty on a shipping set.
    #[test]
    fn an_empty_string_value_parses_as_empty() {
        assert_eq!(field(REAL, "webos_prerelease"), Some(""));
    }

    /// Garbage in must not panic: this runs during boot, before anything is on screen.
    #[test]
    fn malformed_input_is_none_not_a_panic() {
        for bad in [
            "",
            "{",
            "{\"webos_release\"",
            "{\"webos_release\":",
            "not json at all",
        ] {
            assert_eq!(field(bad, "webos_release"), None, "input {bad:?}");
        }
    }

    /// Both spellings of the hardware keys resolve, because we have one television to check
    /// against and nyx has carried both across releases.
    #[test]
    fn the_hardware_record_takes_whichever_spelling_the_firmware_uses() {
        let snake =
            r#"{"model_name":"49SM9000PLA","board_type":"HE_DTV_W19H","hardware_revision":"1.0"}"#;
        let camel = r#"{"modelName":"OLED55C1","boardType":"k8hp","hardwareRevision":"2.0"}"#;
        assert_eq!(parse_hw(snake).model, "49SM9000PLA");
        assert_eq!(parse_hw(snake).board, "HE_DTV_W19H");
        assert_eq!(parse_hw(camel).model, "OLED55C1");
        assert_eq!(parse_hw(camel).board, "k8hp");
        assert_eq!(parse_hw(camel).hw_revision, "2.0");
    }

    /// Unknown is EMPTY, never a guess — and never a panic, since this runs during boot.
    #[test]
    fn an_unreadable_device_info_yields_an_empty_record() {
        let hw = parse_hw("not json at all");
        assert!(hw.model.is_empty() && hw.board.is_empty() && hw.hw_revision.is_empty());
    }

    /// Absent key, present file.
    #[test]
    fn a_missing_key_is_none() {
        assert_eq!(field(REAL, "no_such_key"), None);
    }

    #[test]
    fn parses_the_whole_record() {
        let i = parse(REAL);
        assert_eq!((i.release.as_str(), i.major), ("4.10.2", 4));
        assert_eq!(i.codename, "goldilocks2-grampians");
        assert_eq!(i.name, "webOS TV");
    }

    /// The unknown case must be EMPTY with major 0, never a plausible-looking default. A panel that
    /// invents "4.0" for a set it could not read is worse than one that admits it does not know.
    #[test]
    fn an_unreadable_file_yields_no_version_rather_than_a_guess() {
        let i = parse("not json at all");
        assert_eq!(i.major, 0);
        assert!(i.release.is_empty() && i.codename.is_empty());
    }

    /// A two-component release still yields its major — LG has shipped "6.0" as well as "4.10.2".
    #[test]
    fn a_short_release_still_gives_a_major() {
        assert_eq!(parse(r#"{"webos_release": "6.0"}"#).major, 6);
        assert_eq!(parse(r#"{"webos_release": "10.0.1"}"#).major, 10);
    }

    #[test]
    fn the_set_and_release_lines_omit_what_is_unknown_and_never_invent_a_set() {
        let hw = Hardware {
            model: "49SM9000PLA".into(),
            board: "HE_DTV_W19H".into(),
            hw_revision: String::new(),
        };
        assert_eq!(hw.set_line(), "49SM9000PLA · HE_DTV_W19H");
        assert_eq!(Hardware::default().set_line(), "");
        let i = Info {
            release: "4.10.2".into(),
            major: 4,
            ..Default::default()
        };
        assert_eq!(i.release_line(), "webOS 4.10.2");
        assert_eq!(Info::default().release_line(), "webOS unknown");
    }
}
