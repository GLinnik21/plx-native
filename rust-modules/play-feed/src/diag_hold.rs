//! **SCRATCH BUILD ONLY (`diag/progressive-v5-hold`, plan v5 W1): never merged.** The knobs of the
//! progressive hold prototype and its observation-only statics.
//!
//! Plan v5 W1/T3 measures the constants `k`, `h`, the queue-empty gate and the hold headroom on
//! the TV before they ship, so this build reads them from a developer-trigger file at Load instead
//! of compiling them in. That test-only read path is exactly what must NOT ship: the real W3 build
//! takes the constants from [`plx_playback_contract::clock_debt::DebtParams::REPLAY`] (or whatever
//! T3/T5 settle) and has no file, no [`DiagHold`], and no statics here.
//!
//! The file is the developer trigger `diag-hold` (`/tmp/plxnative-diag-hold` on the television, in
//! the install's runtime root, `$(make -s print-rundir)/plxnative-diag-hold`, which is
//! `/tmp/com.beb.plxnative.debug/plxnative-diag-hold` for the debug flavour), read through
//! `plx_base::devtrig::read`, so it exists only in a `devtriggers` build. Arm it with
//! `tools/tv-session.sh up ... --arm "diag-hold=k=0.05 h=0.4 headroom_mib=24"`: `up` wipes every
//! trigger, then writes this one before the launch. The Load logs `diag-hold: knobs (trigger)` if the
//! file was read and `(defaults)` if it was not seen. Tokens are `key=value`,
//! separated by whitespace or commas; `#` starts a comment to the end of the line:
//!
//! | key | meaning | default |
//! |---|---|---|
//! | `k` | CUSUM drift, seconds forgiven per second (0.05 = 5%) | `0.05` |
//! | `h` | CUSUM threshold, seconds of `S` | `0.4` |
//! | `gate_kib` | video-queue KiB above which the stream is not starved (S resets) | `1024` |
//! | `headroom_mib` | extra MiB the video queue may hold while a hold stands (T7), `0` = off; given back on the first tick the hold is no longer a progressive hold (a resume, a seek, a stop) | `0` |
//! | `auto_hold` | `0`: no progressive hold under Auto (`HoldRoute::ProgressiveSupervised` maps to none, D4's T2b-failed fallback); `1`: hold | `1` |
//!
//! Example (T3 default with the T7 grant): `k=0.05 h=0.4 gate_kib=1024 headroom_mib=24`.
//! An absent or empty file is the defaults. An unknown key or an unparsable value is logged and
//! ignored. The knobs are read once per Load (every `launch_native_open`), so a change takes
//! effect at the next boot of the stream.

use plx_playback_contract::clock_debt::{ClockDebt, DebtParams};
use std::os::raw::c_long;

/// The prototype's knobs, parsed from the `diag-hold` trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagHold {
    pub params: DebtParams,
    /// Extra video-queue bytes granted while a hold stands; `0` grants nothing (T7).
    pub headroom_bytes: c_long,
    /// Hold under Auto (a progressive stream with an Original watch)?
    pub auto_hold: bool,
}

impl DiagHold {
    pub const DEFAULT: DiagHold = DiagHold { params: DebtParams::REPLAY, headroom_bytes: 0, auto_hold: true };

    /// Parse the trigger's text. Returns the knobs and one note per token that was not understood.
    pub fn parse(text: &str) -> (DiagHold, Vec<String>) {
        let mut cfg = DiagHold::DEFAULT;
        let mut notes = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            for tok in line.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
                let Some((key, val)) = tok.split_once('=') else {
                    notes.push(format!("{tok}: not key=value"));
                    continue;
                };
                let ok = match key {
                    "k" => val.parse::<f64>().ok().filter(|v| (0.0..1.0).contains(v)).map(|v| {
                        cfg.params.k_permille = (v * 1000.0).round() as i64;
                    }),
                    "h" => val.parse::<f64>().ok().filter(|v| *v > 0.0).map(|v| {
                        cfg.params.h_us = (v * 1_000_000.0).round() as i64;
                    }),
                    "gate_kib" => val.parse::<usize>().ok().map(|v| cfg.params.gate_bytes = v.saturating_mul(1024)),
                    "headroom_mib" => val
                        .parse::<c_long>()
                        .ok()
                        .filter(|v| *v >= 0)
                        .map(|v| cfg.headroom_bytes = v.saturating_mul(1024 * 1024)),
                    "auto_hold" => match val {
                        "0" => Some(cfg.auto_hold = false),
                        "1" => Some(cfg.auto_hold = true),
                        _ => None,
                    },
                    _ => {
                        notes.push(format!("{tok}: unknown key"));
                        continue;
                    }
                };
                if ok.is_none() {
                    notes.push(format!("{tok}: bad value"));
                }
            }
        }
        (cfg, notes)
    }

    /// Read the trigger once and log what was understood. Defaults when it is absent.
    pub fn load() -> DiagHold {
        let text = plx_base::devtrig::read("diag-hold");
        let from_file = text.is_some();
        let (cfg, notes) = DiagHold::parse(text.as_deref().unwrap_or(""));
        plx_base::eventlog::log(&format!(
            "diag-hold: knobs {} k={}pm h={}ms gate={}B headroom={}B auto_hold={}{}",
            if from_file { "(trigger)" } else { "(defaults)" },
            cfg.params.k_permille,
            cfg.params.h_us / 1_000,
            cfg.params.gate_bytes,
            cfg.headroom_bytes,
            cfg.auto_hold as u8,
            if notes.is_empty() { String::new() } else { format!(" IGNORED: {}", notes.join("; ")) },
        ));
        cfg
    }
}

/// The Engine's per-Load state of the debt detector: the knobs, the pure step's state, the last
/// native position callback it was stepped with, and what the last hold began on (for the resume
/// line, which is printed by a different function after the debt state has been reset).
#[derive(Clone, Copy, Debug)]
pub struct DebtWatch {
    pub cfg: DiagHold,
    pub debt: ClockDebt,
    /// `FEED_SHARED.dg_pos_ticks` as of the last step: a step happens once per native callback.
    pub seen_pos_ticks: u32,
    /// `S` and the debt since the reset when the last hold began, microseconds.
    pub hold_s_us: i64,
    pub hold_debt_us: i64,
    /// Which arm began the last hold: `"debt"`, `"silence"` or `"both"`.
    pub hold_arm: &'static str,
    /// Holds begun on this Engine.
    pub holds: u32,
    /// The hold's headroom grant (T7) stands on the video queue and has not been given back.
    pub headroom_granted: bool,
}

impl DebtWatch {
    pub fn new(cfg: DiagHold) -> DebtWatch {
        DebtWatch { cfg, debt: ClockDebt::Ineligible, seen_pos_ticks: 0, hold_s_us: 0, hold_debt_us: 0, hold_arm: "", holds: 0, headroom_granted: false }
    }

    /// The knobs from the trigger (at Load).
    pub fn load() -> DebtWatch {
        DebtWatch::new(DiagHold::load())
    }

    /// Something stopped the clock on purpose, or the hold began: forget the debt.
    pub fn reset(&mut self) {
        self.debt = ClockDebt::Ineligible;
    }
}

impl Default for DebtWatch {
    fn default() -> Self {
        DebtWatch::new(DiagHold::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_trigger_is_the_replay_defaults() {
        let (cfg, notes) = DiagHold::parse("");
        assert_eq!(cfg, DiagHold::DEFAULT);
        assert!(notes.is_empty());
        assert_eq!(cfg.params.k_permille, 50);
        assert_eq!(cfg.params.h_us, 400_000);
        assert_eq!(cfg.params.gate_bytes, 1 << 20);
        assert_eq!(cfg.headroom_bytes, 0);
        assert!(cfg.auto_hold);
    }

    #[test]
    fn every_knob_parses_and_comments_and_separators_are_ignored() {
        let (cfg, notes) = DiagHold::parse("k=0.08 h=0.25,gate_kib=512 # a comment h=9\nheadroom_mib=24\nauto_hold=0");
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(cfg.params.k_permille, 80);
        assert_eq!(cfg.params.h_us, 250_000);
        assert_eq!(cfg.params.gate_bytes, 512 * 1024);
        assert_eq!(cfg.headroom_bytes, 24 * 1024 * 1024);
        assert!(!cfg.auto_hold);
    }

    #[test]
    fn a_bad_token_is_reported_and_changes_nothing() {
        let (cfg, notes) = DiagHold::parse("k=2 h=zero gate_kib=-1 bogus=1 loose auto_hold=2");
        assert_eq!(cfg, DiagHold::DEFAULT);
        assert_eq!(notes.len(), 6, "{notes:?}");
    }
}
