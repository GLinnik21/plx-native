//! `/tmp/plxnative-toast=<text>` — ask the television's notification service, in-process and jailed
//! as the app, whether it will show `<text>` as a system toast. A measurement, not the feature: the
//! product toast is `app::clock_notice`, and this trigger asks the same service with arbitrary
//! text, plain and as the app, logging each outcome.
//!
//! **Two attempts, each on its own log line, so a refusal is never silent:**
//!
//! 1. `toast-probe plain: …` — the plain anonymous call, `sourceId` set to this app's id.
//! 2. `toast-probe as-app: …` — the same payload through `LSCallFromApplicationOneReply`.
//!
//! Each line carries the service's whole reply (`reply=<json>`, a refusal's `errorText` included) or
//! the bus failure's `stage`/`code`/`detail`. The attempts run on a worker, [`LEAD_IN`] after boot
//! and [`GAP`] apart, so a screenshot can be timed against the log (a toast lives only a few
//! seconds). Read once at boot; absent from shipping builds with the rest of the trigger surface.

use plx_platform::tv::toast::{probe_line, send, Identity};
use std::time::Duration;

/// Boot settle time before the first attempt, so the first toast is not behind the splash.
const LEAD_IN: Duration = Duration::from_secs(6);
/// Between the two attempts, long enough that each can be told apart on screen.
const GAP: Duration = Duration::from_secs(8);

/// Called once from `app::boot`. A no-op without the trigger or with an empty value.
pub(crate) fn arm_at_boot() {
    let Some(text) = plx_base::devtrig::read("toast").filter(|t| !t.is_empty()) else { return };
    plx_base::eventlog::log("toast-probe armed");
    if plx_base::task::spawn("toast-probe", move || run(&text)).is_none() {
        plx_base::eventlog::log("toast-probe IGNORED — the worker thread could not start");
    }
}

fn run(text: &str) {
    std::thread::sleep(LEAD_IN);
    for (i, (label, identity)) in [("plain", Identity::Anonymous), ("as-app", Identity::AsApp)]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            std::thread::sleep(GAP);
        }
        plx_base::eventlog::log(&probe_line(label, &send(text, identity)));
    }
}
