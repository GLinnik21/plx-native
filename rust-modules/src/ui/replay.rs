//! Replay (restructure spec §5.5): a recording is a REGRESSION ASSERTION pinned to the build
//! lineage that recorded it. `--targets` mode feeds each recorded input with its recorded
//! resolution and each recorded adapter result at its recorded frame, drives the dispatcher on
//! the recorded ticks (`VirtualClock`), and grades the logical-state hash stream — every mismatch
//! is its own line, and replay CONTINUES, so a change reads as a list of pointwise diffs rather
//! than an avalanche. `--resolve` (phase 3b) additionally runs the engine and the hit map.
//!
//! The application supplies the decoding of its own inputs and results (`Codec`), because the
//! library does not know what an `ElemKey` or an `AsyncPayload` is.
#![allow(dead_code)] // phase 2: the product driver lands with the recorder trigger

use serde_json::Value;

use super::dispatch::{Dispatcher, Rig, Tap};
use super::machine::{Addr, Host, InputEvent, Tick};
use super::rec::Recording;

/// How the application spells its inputs and results in a recording.
pub trait Codec<H: Host> {
    fn decode_input(&self, v: &Value) -> Option<InputEvent<H::Elem>>;
    fn decode_result(&self, v: &Value) -> Option<(Addr, H::Msg)>;
}

/// One pointwise mismatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub frame: u64,
    pub expected: u64,
    pub got: u64,
    /// The recorded input kinds on that frame, for the `--safe` printer (never payloads).
    pub inputs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub frames: u64,
    pub graded: u64,
    pub divergences: Vec<Divergence>,
    /// A measurement the table could not answer: a hard failure.
    pub measure_miss: Option<(String, i32, bool)>,
    /// Present bits that differed `(frame, recorded, got)`.
    pub present_diffs: Vec<(u64, bool, bool)>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.divergences.is_empty() && self.measure_miss.is_none() && self.present_diffs.is_empty()
    }

    /// The `--safe` rendering: frame indices, event kinds, hashes; never a string payload.
    pub fn safe_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.divergences {
            out.push(format!(
                "diverge f={} expected={:#018x} got={:#018x} inputs=[{}]",
                d.frame,
                d.expected,
                d.got,
                d.inputs.join(",")
            ));
        }
        for (f, rec, got) in &self.present_diffs {
            out.push(format!("present f={f} recorded={rec} got={got}"));
        }
        if let Some((_, sz, bold)) = &self.measure_miss {
            out.push(format!("measure-miss sz={sz} bold={bold}"));
        }
        out
    }
}

/// A tap that records nothing — the replay's own driver grades hashes itself.
struct Silent;
impl<H: Host> Tap<H> for Silent {}

/// Replay `rec` in `--targets` mode against a fresh dispatcher and rig.
pub fn run_targets<H: Host>(
    rec: &Recording,
    codec: &dyn Codec<H>,
    d: &mut Dispatcher<H>,
    rig: &mut dyn Rig<H>,
    measure_miss: &dyn Fn() -> Option<(String, i32, bool)>,
) -> Report {
    let mut report = Report::default();
    let mut silent = Silent;
    for fr in &rec.frames {
        let tick = fr.tick.unwrap_or(Tick::default());
        let inputs: Vec<InputEvent<H::Elem>> = fr
            .inputs
            .iter()
            .filter_map(|v| codec.decode_input(v))
            .collect();
        let results: Vec<(Addr, H::Msg)> = fr
            .results
            .iter()
            .filter_map(|v| codec.decode_result(v))
            .collect();
        let r = d.frame(rig, tick, inputs, results, &mut silent);
        report.frames += 1;
        if let Some(rec_bit) = fr.present {
            if rec_bit != r.presented {
                report.present_diffs.push((fr.f, rec_bit, r.presented));
            }
        }
        if let Some(expected) = fr.st {
            report.graded += 1;
            let got = d.state_hash();
            if got != expected {
                report.divergences.push(Divergence {
                    frame: fr.f,
                    expected,
                    got,
                    inputs: fr
                        .inputs
                        .iter()
                        .map(|v| v["kind"].as_str().unwrap_or("?").to_string())
                        .collect(),
                });
            }
        }
        if let Some(miss) = measure_miss() {
            report.measure_miss = Some(miss);
            break;
        }
    }
    report
}
