//! play_movie route selection (direct-play vs transcode) + the stream URL, transcode session, and
//! HUD strings, split in phase 9 (spec §9) into two modules by ONE rule: [`plan`] holds functions
//! of their arguments alone — no `static mut`, no `PlayerControl`, no `plex::Client` held across a
//! call, no `task::spawn*` — plus the plain data types they need, which is what lets
//! [`plan::build_stream`] run on the resolve worker; [`decision`] holds everything else — the
//! main-thread `Session`, the synchronized `PlayerControl`, and every PMS/native effect. Every
//! item this module re-exports keeps its old `route::` path, so nothing outside `route/` had to
//! change. `ci/check-deps.sh`'s `wall` gate holds `plan.rs` to zero
//! `Instant::now`/`SystemTime::now`/`.elapsed()`; `decision.rs` is not scanned by that gate (a
//! network/adapter effect is allowed to read wall time) but as of this split carries none either.

mod decision;
mod plan;

pub(crate) use decision::*;
pub(crate) use plan::*;
