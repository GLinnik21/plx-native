//! Replay (restructure spec §5.5): a recording is a REGRESSION ASSERTION pinned to the build
//! lineage that recorded it. `--targets` mode feeds each recorded input with its recorded
//! resolution and each recorded adapter result at its recorded frame, drives the dispatcher on
//! the recorded ticks (`VirtualClock`), and grades the logical-state hash stream — every mismatch
//! is its own line, and replay CONTINUES, so a change reads as a list of pointwise diffs rather
//! than an avalanche. `--resolve` (phase 3b) additionally runs the engine for real: every input
//! resolves through the focus engine and the recorded `fo` (focus) record is compared after each
//! frame — a mismatch is its own line and replay CONTINUES FROM THE RECORDED resolution, so one
//! weighting change is a list of pointwise diffs. `--targets` runs the same engine but takes
//! the recording's answer silently: it grades the application's machines, not the engine.
//!
//! The application supplies the decoding of its own inputs and results (`Codec`), because the
//! library does not know what an `ElemKey` or an `AsyncPayload` is.
#![allow(dead_code)] // phase 2: the product driver lands with the recorder trigger

use serde_json::Value;

use super::dispatch::{Dispatcher, Rig, Tap};
use super::geom::IndexElem;
use super::machine::{Addr, EntryId, FocusKey, Host, InputEvent, Tick};
use super::rec::Recording;

/// The two replay modes (§5.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Feed each input with its recorded resolution: the engine is bypassed.
    Targets,
    /// Run the engine for real and grade its answer against the recording, continuing from the
    /// recorded resolution on a mismatch.
    Resolve,
}

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
    /// `--resolve`: focus resolutions that differed `(frame, recorded, got)` as `(entry, elem)`.
    pub focus_diffs: Vec<(u64, Option<(u32, u32, Option<u32>)>, Option<(u32, u32, Option<u32>)>)>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.divergences.is_empty()
            && self.measure_miss.is_none()
            && self.present_diffs.is_empty()
            && self.focus_diffs.is_empty()
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
        for (f, rec, got) in &self.focus_diffs {
            out.push(format!("focus f={f} recorded={rec:?} got={got:?}"));
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
) -> Report
where
    H::Elem: IndexElem,
{
    run(rec, codec, d, rig, measure_miss, Mode::Targets)
}

/// Replay `rec` in `--resolve` mode: the engine runs for real and is graded.
pub fn run_resolve<H: Host>(
    rec: &Recording,
    codec: &dyn Codec<H>,
    d: &mut Dispatcher<H>,
    rig: &mut dyn Rig<H>,
    measure_miss: &dyn Fn() -> Option<(String, i32, bool)>,
) -> Report
where
    H::Elem: IndexElem,
{
    run(rec, codec, d, rig, measure_miss, Mode::Resolve)
}

fn focus_of<H: Host>(d: &Dispatcher<H>) -> Option<(u32, u32, Option<u32>)>
where
    H::Elem: IndexElem,
{
    d.focus_record()
}

pub fn run<H: Host>(
    rec: &Recording,
    codec: &dyn Codec<H>,
    d: &mut Dispatcher<H>,
    rig: &mut dyn Rig<H>,
    measure_miss: &dyn Fn() -> Option<(String, i32, bool)>,
    mode: Mode,
) -> Report
where
    H::Elem: IndexElem,
{
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
        // Both modes run the engine — a screen hears the `FocusMoved` its recorded twin heard, so
        // its own state stays comparable. They differ in what a mismatch IS: `Targets` takes the
        // recording's answer silently (the engine is not what is being graded); `Resolve`
        // reports it and then continues from the recording.
        let r = d.frame(rig, tick, inputs, results, &mut silent);
        report.frames += 1;
        if let Some(rec_bit) = fr.present {
            if rec_bit != r.presented {
                report.present_diffs.push((fr.f, rec_bit, r.presented));
            }
        }
        if let Some(recorded) = fr.focus {
            let got = focus_of(d);
            if mode == Mode::Resolve && got != recorded {
                report.focus_diffs.push((fr.f, recorded, got));
            }
            // continue from the recording (both modes): the recorded key is the frame's answer
            if got != recorded {
                d.set_focus_in(
                    recorded.map(|(e, k, _)| FocusKey {
                        entry: EntryId(e),
                        elem: H::Elem::of_index(k),
                    }),
                    recorded.and_then(|(_, _, g)| g.map(super::machine::GroupId)),
                );
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
