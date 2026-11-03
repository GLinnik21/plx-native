//! Main-thread confined native buffer-feed execution. A prepared Load owns its clock and
//! callback epochs until the addressed route attempt installs its Engine.

use crate::aq::{self, AuQueue};
use crate::feed_state::{Engine, Source, PRES_NONE};
use crate::shared::PhysicalShared;
use plx_base::task::MainThread;
use plx_platform::tv::sink::VideoSink;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::Ordering;

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

/// Free both queued AU lanes and each held BufferFull retry AU.
pub fn drain_aq(eng: &mut Engine) {
    drain_one(eng.aq_video.as_mut());
    drain_one(eng.aq_audio.as_mut());
    eng.pending_video = None;
    eng.pending_audio = None;
}

fn drain_one(q: Option<&mut Box<AuQueue>>) {
    if let Some(q) = q {
        let qp = &mut **q as *mut AuQueue;
        let mut eof: c_int = 0;
        loop {
            let n = aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            unsafe { libc::free(n as *mut c_void) };
        }
    }
}

/// Re-prime at the currently presented physical point without changing the clock.
pub fn arm_live_clock_prime(eng: &mut Engine) {
    let shared = eng.shared;
    let presented = shared.pres_fed.load(Ordering::Relaxed);
    eng.seek_base_pts = if presented == PRES_NONE {
        eng.max_fed_video_pts.min(eng.max_fed_audio_pts.max(0))
    } else {
        presented
    };
    eng.prime_play = true;
}

/// Feed the looped local sample. The provider is called at every native verb so a release
/// entering the native-object fence cannot leave a cached unfenced sink in the executor.
pub fn feed_sample(
    mt: &MainThread,
    eng: &mut Engine,
    sink: fn() -> &'static dyn VideoSink,
) {
    let s = match &mut eng.source {
        Source::Sample(s) => s,
        _ => return,
    };
    let naus = s.au.len();
    if naus < 2 {
        return;
    }
    let mut fed = 0;
    while fed < 60 {
        if s.next >= naus - 1 {
            s.next = 0;
            s.loops += 1;
        }
        let off = s.au[s.next];
        let end = s.au[s.next + 1];
        let pts = (s.loops * (naus as i64 - 1) + s.next as i64) * 41708333;
        let r = unsafe { sink().feed(mt, s.data[off..].as_ptr(), (end - off) as u32, pts, 1) };
        if (r as u8) != b'O' {
            break;
        }
        s.next += 1;
        fed += 1;
    }
}
