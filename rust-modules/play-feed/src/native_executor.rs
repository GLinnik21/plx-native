//! Main-thread confined native buffer-feed execution. A prepared Load owns its clock and
//! callback epochs until the addressed route attempt installs its Engine.

use crate::shared::PhysicalShared;

/// Retracts an initial clock hold when a prepared Load is superseded before installation.
pub struct ClockStartGuard {
    shared: &'static PhysicalShared,
    armed: bool,
}

impl ClockStartGuard {
    pub fn arm(shared: &'static PhysicalShared, user_held: bool, seek_preroll: bool) -> Option<Self> {
        shared
            .arm_initial_clock_hold(user_held, seek_preroll)
            .then_some(Self { shared, armed: true })
    }

    pub fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for ClockStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.cancel_clock_session_start();
        }
    }
}

/// Retires the exact native callback epoch if Load construction does not install an Engine.
pub struct NativeSessionStartGuard {
    shared: &'static PhysicalShared,
    epoch: u32,
    armed: bool,
}

impl NativeSessionStartGuard {
    pub fn arm(shared: &'static PhysicalShared) -> Option<Self> {
        shared
            .begin_native_session()
            .map(|epoch| Self { shared, epoch, armed: true })
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for NativeSessionStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.retire_native_session(self.epoch);
        }
    }
}
