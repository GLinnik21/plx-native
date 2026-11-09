//! The URI-mode probe's driver (issue #1064, dev trigger `plxnative-uriprobe`): the application's
//! side of media's probe port (`plx_media::player::uriprobe_drive`).
//!
//! Media keeps the route half: the claimed start attempt, the settle into the route reducer, the
//! facts the attempt view takes, the adapter's one probe slot, and every route-side `uriprobe:` log
//! line. This module is the other half. It reads the trigger text into a variant and the native ABR
//! rates ([`Probe`], via `plx_play_uri::uriprobe::Trigger`), opens the independent driver over the
//! transport, window verbs and plane owner media resolved for it ([`Launch`], over
//! `plx_play_uri::uriprobe_drive::UriSession`), and wraps the running session as media's
//! [`UriProbeSession`] ([`UriSession`]). [`install`] hands the probe to the adapter at boot.
//!
//! The probe is owner-authorized debug code: it changes how ONE play is carried out, builds no
//! Engine, and reports nothing to Plex. It is compiled with `devtriggers` and `play-uri` only.

use plx_base::task::MainThread;
use plx_media::player::adapter::PlayerAdapter;
use plx_media::player::uriprobe_drive::{ProbeLaunch, ProbeOpen, ProbeSettle, UriProbe, UriProbeSession};
use plx_media::route::RouteStartAttempt;
use plx_play_uri::uriprobe::{AbrBps, Firmware, Kind, Settle, Trigger, Variant};
use plx_play_uri::uriprobe_drive::{DriverConfig, UriSession as CoreSession};

/// Install the probe on the player adapter, once, at boot (`compose_backends`).
pub(crate) fn install(adapter: &mut PlayerAdapter) {
    adapter.install_uri_probe(Box::new(Probe));
}

/// The variant and native ABR rates a `plxnative-uriprobe` trigger text names; the text which names
/// no variant leaves the probe unarmed and the play normal.
struct Probe;

impl UriProbe for Probe {
    fn armed(&self, text: &str) -> Option<Box<dyn ProbeLaunch>> {
        let trigger = Trigger::from_trigger(text)?;
        Some(Box::new(Launch { variant: trigger.variant, bps: trigger.bps }))
    }

    /// The Feed's native object is free of an earlier session when its start guard can be armed
    /// (a parked timed-out Load still owns it otherwise). A build with no Feed has no session to wait on.
    fn native_slot_available(&self) -> bool {
        #[cfg(feature = "play-feed")]
        {
            plx_play_feed::native_executor::NativeSessionStartGuard::arm(&plx_play_feed::shared::FEED_SHARED).is_some()
        }
        #[cfg(not(feature = "play-feed"))]
        {
            true
        }
    }
}

/// An armed trigger, waiting for the route start media claims.
struct Launch {
    variant: Variant,
    bps: AbrBps,
}

impl ProbeLaunch for Launch {
    fn open(self: Box<Self>, input: ProbeOpen<'_>, mt: &MainThread) -> Box<dyn UriProbeSession> {
        Box::new(UriSession::open(input, self.variant, self.bps, running_firmware(), mt))
    }
}

/// The firmware this set reported at boot: the one global the probe reads, read here at the IO
/// boundary so the machine under it stays pure. All-empty (printed as `-`) when the boot probe
/// could not read it.
fn running_firmware() -> Firmware {
    let info = plx_platform::tv::device::info();
    Firmware { release: info.release.clone(), build: info.build.clone() }
}

/// One URI session held by the player adapter. The URL stays inside the independent driver.
pub(crate) struct UriSession(CoreSession<RouteStartAttempt>);

impl UriSession {
    fn open(input: ProbeOpen<'_>, variant: Variant, bps: AbrBps, firmware: Firmware, mt: &MainThread) -> UriSession {
        let ProbeOpen { attempt, url, transport, display, plane, log, app_id, segmented_hls, release_allowance_ms } = input;
        let kind = if segmented_hls { Kind::Hls } else { Kind::Direct };
        let config = DriverConfig {
            attempt, variant, bps, kind, url: url.to_owned(),
            app_id, firmware, transport,
            sink: display,
            log, plane,
            release_allowance_ms,
        };
        UriSession(CoreSession::open(config, mt))
    }
}

fn settle_of(how: Settle) -> ProbeSettle {
    match how {
        Settle::Started => ProbeSettle::Started,
        Settle::StartFailed => ProbeSettle::StartFailed,
    }
}

impl UriProbeSession for UriSession {
    fn plane_open(&self) -> bool { self.0.plane_open() }
    fn closing(&self) -> bool { self.0.closing() }
    fn route_start_attempt(&self) -> RouteStartAttempt { self.0.attempt() }
    fn acb_exit_quarantine(&self) -> bool { self.0.acb_exit_quarantine() }
    fn ready_to_release(&self) -> bool { self.0.ready_to_release() }
    fn advance(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread) -> Option<i64> {
        let poll = self.0.poll();
        let now_ms = self.0.now_ms();
        self.0.advance(&mut |attempt, how| settle(attempt, settle_of(how)), mt, poll, now_ms);
        self.0.reported_position_ms()
    }
    fn stop(&mut self, settle: &mut dyn FnMut(RouteStartAttempt, ProbeSettle) -> bool, mt: &MainThread) {
        self.0.stop(&mut |attempt, how| settle(attempt, settle_of(how)), mt);
    }
    fn release(self: Box<Self>, mt: &MainThread) {
        self.0.release(mt);
    }
}

#[cfg(test)]
#[path = "uriprobe_tests.rs"]
mod tests;
