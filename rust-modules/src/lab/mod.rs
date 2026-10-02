//! **Lab Diagnostics** — the app pushing its own log off a television nobody here can reach.
//!
//! LG Cloud Test Lab rents physical sets on webOS/SoC combinations we do not own. It gives a
//! picture and a virtual remote, and **no console, no ssh, no stdout and no way to download a
//! file**. So a bug can be reproduced on a k8hpp webOS 10 set, watched happening, and the agent
//! fixing it cannot see one line of `plxnative-events.log`. Every other diagnostic surface in this
//! repository assumes ssh (`crate::dev`'s ~44 `/tmp` triggers, the remote FIFO, the capture
//! listener, `make -s print-eventlog`) and is therefore unreachable there.
//!
//! This module is the bridge: a **bounded ring of the log lines the app already writes**, plus the
//! structured state `crate::player::Diag` already carries, uploaded over pinned TLS to a receiver
//! on the developer's Mac (`tools/plxnative-lab`), triggered by a remote button, a menu row or the
//! optional authenticated command channel. `docs/lab-diagnostics.md` is the design note; read it
//! before extending any of this.
//!
//! # Three rules, all structural rather than a matter of care
//!
//! 1. **It is not in any build a user can install.** The whole module is behind the
//!    `lab-diagnostics` cargo feature, which — unlike `devtools`/`devtriggers` — is **not in the
//!    default set at all**, so a release build cannot acquire it by forgetting a flag. Without the
//!    feature every entry point below is a compile-time no-op: no ring, no allocation, no key arm,
//!    no config read, no socket, no thread.
//! 2. **Call sites carry no `#[cfg]`.** The event log's tap, `app.rs`'s key ladder,
//!    `ui::consts::is_bound` and the two menus all call plain functions that fold away (`labcfg`'s
//!    two answers, for `ui/`, `appkit/` and `screens/`). That is `crate::devtrig`'s shape and it is
//!    deliberate: hand-written `#[cfg]` PAIRS at call sites are the one hazard
//!    `.claude/hooks/release-config-check.py` exists for, and the gating lives in two files
//!    (`lab/mod.rs`, `labcfg/mod.rs`) instead of eight.
//! 3. **Nothing enters the payload that is not already allowed on a photograph.** The envelope is
//!    built from `Diag`, `webos::Info` and `devcaps::Caps` — numbers, bools, enums and short
//!    platform strings — under the same no-URL / no-credential / no-identity rule `app::diagnostics`
//!    states at length, and every ring record passes [`snapshot::scrub`] on the way out on top of
//!    the `redact_tokens` it already passed on the way in. See [`snapshot`].
//!
//! # What it deliberately is not
//!
//! Not analytics, not a crash service and not general device administration. Lab Control can ask
//! this app to replay only its bounded synthetic-input/test token grammar; it cannot invoke a
//! shell, read a file, call an arbitrary URL or control webOS outside this SDL process. Both
//! directions are initiated by the television as pinned, authenticated HTTPS POSTs.

// `lab.json`'s reader, `is_trigger_key` and `menu_row_enabled` live in `crate::labcfg` (platform):
// `ui/` and `screens/` ask those two questions and may not name this module.
#[cfg(feature = "lab-diagnostics")]
use crate::labcfg::config;

#[cfg(feature = "lab-diagnostics")]
pub(crate) mod control;
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod snapshot;
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod toast; // the upload read-out; it moved here from `ui/` because only this module draws it
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod upload;

/// A command delivered by Lab Control and waiting for the SDL main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlCommand {
    pub id: u32,
    pub token: String,
}

/// Read `lab.json`, log what was found, and start the uptime clock. Called once at boot, beside
/// `webos::probe`/`devcaps::probe`.
///
/// Logs either the endpoint and session id (never the secret, never the pin — the pin is not
/// secret but printing a 44-character base64 blob into every log helps nobody) or the one reason
/// the feature is inert. A lab build whose configuration failed to parse must SAY so on line three
/// of the log, because the alternative is a tester pressing a button in a rented lab hour and
/// getting silence that looks exactly like a key that was never delivered.
pub(crate) fn boot() {
    #[cfg(feature = "lab-diagnostics")]
    {
        crate::eventlog::ring::start_clock();
        match config::get() {
            Some(c) => crate::eventlog::log(&format!(
                "lab: armed session={} endpoint={} control={} triggers={:?} ring={}rec/{}KiB",
                c.session,
                c.endpoint,
                if c.control { "on" } else { "off" },
                c.trigger_wcodes,
                crate::eventlog::ring::MAX_RECORDS,
                crate::eventlog::ring::MAX_BYTES / 1024
            )),
            None => crate::eventlog::log(&format!("lab: INERT — {}", config::why_not())),
        }
    }
}

/// Start the outbound command poll after libcurl's process-global initialisation.
pub(crate) fn start_control() {
    #[cfg(feature = "lab-diagnostics")]
    control::start();
}

/// Commands waiting to enter SDL. Empty in every non-lab build.
pub(crate) fn take_commands() -> Vec<ControlCommand> {
    #[cfg(feature = "lab-diagnostics")]
    {
        return control::take();
    }
    #[cfg(not(feature = "lab-diagnostics"))]
    Vec::new()
}

/// Acknowledge main-thread dispatch to the long-poll worker.
pub(crate) fn command_done(_id: u32, _ok: bool) {
    #[cfg(feature = "lab-diagnostics")]
    control::finish(_id, _ok);
}

/// The key ladder's lab arm: `true` when the press was taken and the ladder must `continue`.
///
/// It sits at the TOP of the chain, above every modal, on purpose — the screen a tester most
/// wants a snapshot of is the playback failure read-out, whose own arm `continue`s on every key.
#[inline]
pub(crate) fn key_press(_sym: u32, _wcode: u32, _ps: &crate::route::PlaybackSession) -> bool {
    #[cfg(feature = "lab-diagnostics")]
    {
        if crate::labcfg::is_trigger_key(_sym, _wcode) {
            request_upload("key", _ps);
            return true;
        }
    }
    false
}

/// Snapshot now and upload. `reason` is recorded in the envelope so a snapshot taken from the menu
/// is distinguishable from one taken with the remote (which is how the colour-button question gets
/// settled — see `docs/lab-diagnostics.md` §7).
///
/// **Main thread only**: `player::diag` is main-thread by contract. The blocking work happens on
/// a worker.
///
/// The session is a PARAMETER for the reason every other `player::diag` caller's is (phase 9):
/// there is no `route::decision::SESSION` global left to read it out of, and the loop owns it.
pub(crate) fn request_upload(_reason: &str, _ps: &crate::route::PlaybackSession) {
    #[cfg(feature = "lab-diagnostics")]
    upload::request(_reason, _ps);
}

/// The app's current route, by the name the heartbeat uses. Stored for the next snapshot's
/// envelope; called once per frame from the tail of the loop.
#[inline]
pub(crate) fn note_route(_r: &'static str) {
    #[cfg(feature = "lab-diagnostics")]
    upload::note_route(_r);
}

/// Expire the toast. Called once per frame from the update block.
#[inline]
pub(crate) fn update(_now: u32) {
    #[cfg(feature = "lab-diagnostics")]
    toast::update(_now);
}

/// Draw the toast, over everything, on every route. `stats` is the diagnostics read-out's frame
/// when it is on screen: the toast sits immediately below it, and neither may cover the other.
#[inline]
pub(crate) fn draw(_stats: Option<crate::ui::Rect>) {
    #[cfg(feature = "lab-diagnostics")]
    toast::draw(_stats);
}
