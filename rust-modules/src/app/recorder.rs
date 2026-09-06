//! The product's recorder and replay driver (restructure spec §5.3, §5.5) over the LEGACY loop.
//!
//! **What phase 2 records**: every frame's tick and present bit; every input the loop acted on
//! (SDL keys as `sym`/`wcode`/edge, remote-FIFO and lab tokens, pointer motion and clicks, the
//! four lifecycle codes); and on every frame that had an input, `st` — the hash of what IS a
//! machine today: the press (`ui::press::Press`, a `LogicalState`), the route and overlay words,
//! and the focus fingerprint (`focusprobe`), which is the legacy screens' state as one line. No
//! adapter result is recorded yet — the stores still talk to the network themselves (phase 4) —
//! so a replay runs LIVE against the synthetic PMS, and what it grades is the machine set above.
//!
//! **Replay (`--targets`)**: `plxnative-recplay=<dir>` drives the loop on the recorded ticks (the
//! `AppClock`), re-injects each frame's inputs through the same synthesis the remote FIFO uses,
//! compares `st` frame by frame, logs every mismatch as its own `replay: diverge` line and
//! CONTINUES, then logs one summary and ends the run. A landing arriving on a different frame than
//! it did when recorded is the expected source of a divergence in this phase and is reported,
//! never hidden.
//!
//! Arming is at boot only (`Writer::open` refuses any other frame). The directory is the runtime
//! root's `plxnative-recordings/latest` (not `plxnative-rec/`, which is the trigger FILE's own
//! name) — private, gitignored, refused by the outbound guard; a
//! committed fixture is `tools/plxnative-rec import` of one taken against `tests/mock_pms.py`.
#![allow(clippy::too_many_arguments)]

use serde_json::{json, Value};

use crate::ui::machine::{Canon, LogicalState, Tick};
use crate::ui::rec::{DirSink, Header, RecError, Recording, Writer};

/// The application's initial conditions (`H::Init`, spec §5.3): what a recording starts from.
/// Every word here is a protocol constant or a number, so the probe text is synthetic by
/// construction (tests/fixtures/replay/ALPHABET.json carries its pattern).
pub(super) struct AppInit {
    pub route: &'static str,
    pub session: bool,
    pub servers: u32,
    pub consent_asked: u32,
    pub consent_errors: bool,
    pub consent_usage: bool,
    pub seed: u32,
}

impl AppInit {
    pub const SHAPE: &'static str =
        "AppInit{route:str,session:bool,servers:u32,consent_asked:u32,consent_errors:bool,consent_usage:bool,seed:u32}";
}

impl LogicalState for AppInit {
    fn write(&self, w: &mut Canon) {
        w.str(self.route)
            .bool(self.session)
            .u32(self.servers)
            .u32(self.consent_asked)
            .bool(self.consent_errors)
            .bool(self.consent_usage)
            .u32(self.seed);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "route={} session={} servers={} consent={}/{}/{} seed={}",
            self.route,
            self.session as u8,
            self.servers,
            self.consent_asked,
            self.consent_errors as u8,
            self.consent_usage as u8,
            self.seed
        ));
    }
}

/// The state SHAPE of what phase 2 hashes: bump by changing a `SHAPE` string, never silently.
pub(super) fn state_fp() -> u64 {
    crate::ui::rec::state_fp(&[
        crate::ui::press::Press::SHAPE,
        "AppFrame{route:str,overlay:str,focus:str}",
        AppInit::SHAPE,
    ])
}

/// The hash of the frame's logical state (spec §5.4), as phase 2 defines it.
pub(super) fn state_hash(press: &crate::ui::press::Press, route: &str, overlay: &str, focus: &str) -> u64 {
    let mut c = Canon::new();
    press.write(&mut c);
    c.str(route).str(overlay).str(focus);
    c.finish()
}

pub(super) struct Rec {
    w: Writer,
    f: u64,
    events: bool,
    spent_ns: u64,
}

pub(super) struct Replay {
    rec: Recording,
    at: usize,
    graded: u64,
    diverged: u64,
    present_diffs: u64,
    started: bool,
}

pub(super) enum Recplay {
    Off,
    Recording(Rec),
    Replaying(Replay),
}

impl Recplay {
    /// Read the two triggers ONCE at boot. Both armed is refused (a run cannot record its own
    /// replay); a recording is refused on a boot that is not frame 0 by `Writer::open` itself.
    pub(super) fn arm(init: &AppInit, triggers: Vec<String>) -> Recplay {
        let rec = crate::dev::read("rec");
        let play = crate::dev::read("recplay");
        match (rec, play) {
            (Some(_), Some(_)) => {
                crate::log("rec: REFUSED — plxnative-rec and plxnative-recplay are both armed");
                Recplay::Off
            }
            (Some(opt), None) => {
                let dir = crate::paths::runtime_dir().join("plxnative-recordings").join("latest");
                let _ = std::fs::remove_dir_all(&dir);
                let sink = match DirSink::create(&dir) {
                    Ok(s) => s,
                    Err(e) => {
                        crate::log(&format!("rec: REFUSED — cannot create the directory: {e}"));
                        return Recplay::Off;
                    }
                };
                let mut header = Header::new(state_fp(), init);
                header.clock_start_ms = super::clock::now();
                header.build = env!("PLX_VERSION").to_string();
                header.features = features();
                header.triggers = triggers;
                header.blobs = opt == "blobs";
                match Writer::open(Box::new(sink), &header, 0) {
                    Ok(w) => {
                        crate::log(&format!("rec: recording to {}", dir.display()));
                        Recplay::Recording(Rec {
                            w,
                            f: 0,
                            events: false,
                            spent_ns: 0,
                        })
                    }
                    Err(e) => {
                        crate::log(&format!("rec: REFUSED — {e:?}"));
                        Recplay::Off
                    }
                }
            }
            (None, Some(dir)) => {
                let path = std::path::Path::new(&dir);
                match Recording::load(path, state_fp()) {
                    Ok(rec) => {
                        crate::log(&format!(
                            "replay: {} frames from {} (build {} recorded, {} replaying)",
                            rec.frames.len(),
                            path.display(),
                            rec.header.build,
                            env!("PLX_VERSION")
                        ));
                        if let Some(diff) = triggers_differ(&rec.header.triggers, &triggers) {
                            crate::log(&format!("replay: TRIGGERS DIFFER — {diff}"));
                        }
                        let mut probe = String::new();
                        init.probe(&mut probe);
                        if probe != rec.header.init_probe {
                            crate::log(&format!(
                                "replay: INITIAL CONDITIONS DIFFER — recorded [{}] now [{}]",
                                rec.header.init_probe, probe
                            ));
                        }
                        Recplay::Replaying(Replay {
                            rec,
                            at: 0,
                            graded: 0,
                            diverged: 0,
                            present_diffs: 0,
                            started: false,
                        })
                    }
                    Err(RecError::Schema { theirs, ours }) => {
                        crate::log(&format!("replay: REFUSED — schema {theirs} recorded, {ours} here"));
                        Recplay::Off
                    }
                    Err(RecError::StateShape { theirs, ours }) => {
                        crate::log(&format!(
                            "replay: REFUSED — state shape {theirs:#x} recorded, {ours:#x} here"
                        ));
                        Recplay::Off
                    }
                    Err(e) => {
                        crate::log(&format!("replay: REFUSED — {e:?}"));
                        Recplay::Off
                    }
                }
            }
            (None, None) => Recplay::Off,
        }
    }

    /// Replay: the recording boot's clock at arming — what the replaying boot's own origin
    /// (`App.t0`, the dev-script and heartbeat origin) is re-seated to, so a delay measured from
    /// boot means the same thing on both sides.
    pub(super) fn clock_start(&self) -> Option<u32> {
        match self {
            Recplay::Replaying(r) => Some(r.rec.header.clock_start_ms),
            _ => None,
        }
    }

    /// Replay: the recorded tick of the NEXT frame, or `None` when the recording is exhausted.
    pub(super) fn replay_tick(&self) -> Option<Tick> {
        match self {
            Recplay::Replaying(r) => r.rec.frames.get(r.at).and_then(|f| f.tick),
            _ => None,
        }
    }

    /// Replay: the inputs recorded for the current frame, to be re-injected before ingest.
    pub(super) fn replay_inputs(&self) -> Vec<Value> {
        match self {
            Recplay::Replaying(r) => r.rec.frames.get(r.at).map(|f| f.inputs.clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub(super) fn tick(&mut self, now: u32, dt: f32) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.tick(r.f, Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 });
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(super) fn input(&mut self, encoded: Value) {
        if let Recplay::Recording(r) = self {
            let t0 = std::time::Instant::now();
            r.w.input(r.f, encoded);
            r.events = true;
            r.spent_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub(super) fn present(&mut self, bit: bool) {
        match self {
            Recplay::Recording(r) => {
                let t0 = std::time::Instant::now();
                r.w.present(r.f, bit, None);
                r.spent_ns += t0.elapsed().as_nanos() as u64;
            }
            Recplay::Replaying(r) => {
                if let Some(fr) = r.rec.frames.get(r.at) {
                    if let Some(rec_bit) = fr.present {
                        if rec_bit != bit {
                            r.present_diffs += 1;
                            crate::log(&format!("replay: present f={} recorded={rec_bit} got={bit}", fr.f));
                        }
                    }
                }
            }
            Recplay::Off => {}
        }
    }

    /// The frame's tail. `hash` is computed only when a state record is due (an event frame while
    /// recording; a graded frame while replaying). Returns `true` when a replay has just ended.
    pub(super) fn end_frame(&mut self, hash: &dyn Fn() -> u64) -> bool {
        match self {
            Recplay::Off => false,
            Recplay::Recording(r) => {
                let t0 = std::time::Instant::now();
                if r.events {
                    r.w.state(r.f, hash());
                }
                if let Err(e) = r.w.flush_frame() {
                    crate::log(&format!("rec: write failed, stopping: {e:?}"));
                }
                r.events = false;
                r.f += 1;
                r.spent_ns += t0.elapsed().as_nanos() as u64;
                false
            }
            Recplay::Replaying(r) => {
                r.started = true;
                if let Some(fr) = r.rec.frames.get(r.at) {
                    if let Some(expected) = fr.st {
                        r.graded += 1;
                        let got = hash();
                        if got != expected {
                            r.diverged += 1;
                            crate::log(&format!(
                                "replay: diverge f={} expected={expected:#018x} got={got:#018x} inputs={}",
                                fr.f,
                                fr.inputs.len()
                            ));
                        }
                    }
                }
                r.at += 1;
                if r.at >= r.rec.frames.len() {
                    crate::log(&format!(
                        "replay: done frames={} graded={} diverged={} present_diffs={} verdict={}",
                        r.rec.frames.len(),
                        r.graded,
                        r.diverged,
                        r.present_diffs,
                        if r.diverged == 0 && r.present_diffs == 0 { "SAME" } else { "DIVERGED" }
                    ));
                    return true;
                }
                false
            }
        }
    }

    /// Microseconds the recorder spent this second — the heartbeat's `rec=`; resets.
    pub(super) fn take_spent_us(&mut self) -> Option<u64> {
        match self {
            Recplay::Recording(r) => {
                let us = r.spent_ns / 1000;
                r.spent_ns = 0;
                Some(us)
            }
            _ => None,
        }
    }

    pub(super) fn finish(self) {
        if let Recplay::Recording(r) = self {
            r.w.finish();
            crate::log("rec: finished");
        }
    }
}

/// The recorded boot's trigger set against this boot's, as one line naming what is missing and
/// what is extra (the recorder's own `plxnative-rec` and the replay's `plxnative-recplay` are
/// the expected difference and are not reported). `None` when the sets agree. Dev flags reach
/// the loop from the filesystem until phase 4 turns them into recorded `Sys` results, so this is
/// phase 2's assertion that a replay boot was armed the way the recording boot was.
pub(super) fn triggers_differ(recorded: &[String], now: &[String]) -> Option<String> {
    let skip = |n: &str| n == "plxnative-rec" || n == "plxnative-recplay";
    let missing: Vec<&str> = recorded
        .iter()
        .map(String::as_str)
        .filter(|n| !skip(n) && !now.iter().any(|m| m == n))
        .collect();
    let extra: Vec<&str> = now
        .iter()
        .map(String::as_str)
        .filter(|n| !skip(n) && !recorded.iter().any(|m| m == n))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    Some(format!("missing=[{}] extra=[{}]", missing.join(","), extra.join(",")))
}

fn features() -> Vec<String> {
    let mut v = Vec::new();
    if cfg!(feature = "devtools") {
        v.push("devtools".into());
    }
    if cfg!(feature = "devtriggers") {
        v.push("devtriggers".into());
    }
    if cfg!(feature = "hostsim") {
        v.push("hostsim".into());
    }
    if cfg!(feature = "lab-diagnostics") {
        v.push("lab-diagnostics".into());
    }
    v
}

/// Input encodings — the application's half of the codec (spec §5.5); `replay_inputs` hands
/// these back to the loop, which re-injects them by kind.
pub(super) fn enc_key(sym: u32, wcode: u32, down: bool, repeat: bool) -> Value {
    json!({"kind": "key", "sym": sym, "wcode": wcode, "down": down, "repeat": repeat})
}

pub(super) fn enc_token(tok: &str) -> Value {
    json!({"kind": "token", "tok": tok})
}

pub(super) fn enc_pointer(kind: &str, x: i32, y: i32) -> Value {
    json!({"kind": kind, "x": x, "y": y})
}

pub(super) fn enc_lifecycle(code: u32) -> Value {
    json!({"kind": "lifecycle", "code": code})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_probe_is_synthetic_and_the_shape_is_pinned() {
        let init = AppInit {
            route: "home",
            session: true,
            servers: 1,
            consent_asked: 3,
            consent_errors: true,
            consent_usage: false,
            seed: 7,
        };
        let mut p = String::new();
        init.probe(&mut p);
        assert_eq!(p, "route=home session=1 servers=1 consent=3/1/0 seed=7");
        // Re-pin only with a named reason: a shape change invalidates every committed fixture.
        assert_eq!(state_fp(), 0x8216_7933_2b91_39ba);
    }

    #[test]
    fn a_replay_boot_armed_differently_from_the_recording_is_reported() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            triggers_differ(&s(&["plxnative-focus", "plxnative-rec"]), &s(&["plxnative-focus", "plxnative-recplay"])),
            None,
            "the two driver triggers are the expected difference"
        );
        assert_eq!(
            triggers_differ(&s(&["plxnative-focus", "plxnative-grid"]), &s(&["plxnative-focus", "plxnative-noidle"])),
            Some("missing=[plxnative-grid] extra=[plxnative-noidle]".to_string())
        );
    }

    #[test]
    fn the_state_hash_moves_with_the_focus_line_and_the_press() {
        let mut press = crate::ui::press::Press::new();
        let a = state_hash(&press, "home", "", "focus route=home sel=0");
        let b = state_hash(&press, "home", "", "focus route=home sel=1");
        assert_ne!(a, b);
        press.begin(10);
        let c = state_hash(&press, "home", "", "focus route=home sel=0");
        assert_ne!(a, c);
    }
}
