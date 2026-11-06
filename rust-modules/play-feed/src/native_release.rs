//! Physical native release tail. The common owner supplies its sink, callback epoch and
//! telemetry observer; this module executes the audited gate, retire, plane, D1 order.

use crate::shared::PhysicalShared;
use plx_base::task::MainThread;

/// The five main-thread native verbs needed after Unload returns. The common composition
/// supplies its installed sink without giving this physical runner a process-global lookup.
pub trait NativeReleasePort {
    fn callback_gate_retire(&self, mt: &MainThread) -> bool;
    fn callback_intercepts(&self, mt: &MainThread) -> u32;
    fn plane_unload(&self, mt: &MainThread);
    fn destroy(&self, mt: &MainThread) -> bool;
    fn quarantine(&self, mt: &MainThread);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeObjectDisposition {
    Destroy,
    Quarantine,
}

/// D1 frees state a late firmware hook may still dereference. Every proof is mandatory.
pub fn native_object_disposition(
    unload_completed: bool,
    callback_gate_proven: bool,
    rust_epoch_retired: bool,
) -> NativeObjectDisposition {
    if unload_completed && callback_gate_proven && rust_epoch_retired {
        NativeObjectDisposition::Destroy
    } else {
        NativeObjectDisposition::Quarantine
    }
}

// The common telemetry owner records these at the exact native transition; the step is contract
// vocabulary so the owner names it without naming this crate.
pub use plx_playback_contract::feed::ReleaseStep;

/// Execute the tail after Unload returns. Keep the C callback gate ahead of Rust epoch
/// retirement, ACB plane unload ahead of D1, and quarantine whenever proof is incomplete.
pub fn finish_unloaded_object(
    port: &impl NativeReleasePort,
    shared: &PhysicalShared,
    mt: &MainThread,
    epoch: u32,
    acb_bound: impl FnOnce() -> bool,
    mut mark: impl FnMut(&'static str),
    mut record: impl FnMut(ReleaseStep),
) {
    record(ReleaseStep::NativeUnload);
    let callback_gate_proven = port.callback_gate_retire(mt);
    mark("gate_retire");
    let callback_intercepts = port.callback_intercepts(mt);
    let unload_completed = shared.native_unload_completed(epoch);
    let rust_epoch_retired = shared.retire_native_session(epoch);
    if !unload_completed {
        plx_base::eventlog::log(&format!(
            "native lifecycle: Unload returned without type=23 epoch={epoch}",
        ));
    }
    if !callback_gate_proven {
        plx_base::eventlog::log(&format!(
            "native lifecycle: callback interposition unproven epoch={epoch} intercepted={callback_intercepts}",
        ));
    }
    if !rust_epoch_retired {
        plx_base::eventlog::log(&format!(
            "native lifecycle: failed to retire epoch={epoch} after Unload",
        ));
    }
    mark("retire");
    if acb_bound() {
        port.plane_unload(mt);
        mark("acb_unload");
        record(ReleaseStep::AcbPlaneUnload);
    }
    match native_object_disposition(unload_completed, callback_gate_proven, rust_epoch_retired) {
        NativeObjectDisposition::Destroy => {
            let destroyed = port.destroy(mt);
            mark("sf_destroy");
            if !destroyed {
                plx_base::eventlog::log("native lifecycle: C seam rejected D1 and quarantined the object");
                record(ReleaseStep::NativeQuarantined);
            } else {
                record(ReleaseStep::NativeDestroyed);
            }
        }
        NativeObjectDisposition::Quarantine => {
            port.quarantine(mt);
            mark("quarantine");
            record(ReleaseStep::NativeQuarantined);
        }
    }
}

#[cfg(test)]
mod native_lifecycle_proof_tests {
    use super::{native_object_disposition, NativeObjectDisposition};

    #[test]
    fn destructor_requires_every_independent_runtime_proof() {
        assert_eq!(
            native_object_disposition(true, true, true),
            NativeObjectDisposition::Destroy,
        );
        for (unload_completed, callback_gate_proven, rust_epoch_retired) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
            (false, false, false),
        ] {
            assert_eq!(
                native_object_disposition(
                    unload_completed,
                    callback_gate_proven,
                    rust_epoch_retired,
                ),
                NativeObjectDisposition::Quarantine,
            );
        }
    }
}
