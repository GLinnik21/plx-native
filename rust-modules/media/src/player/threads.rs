//! player::threads — the media-side Load receipt and SendPtr spawn seam. The physical Load
//! worker lives in `plx_play_feed::native_load`. The demux thread body is `ff::demux` (libavformat over a custom
//! AVIO on stream.rs); its HttpStream box lives in the Engine (main owns it, closes it
//! to interrupt, and outlives the threads).
use super::SHARED;
use std::os::raw::{c_char, c_int};
use plx_play_feed::native_load::{self, NativeLoadPort};
use plx_play_feed::pcm_lead::PcmLead;
#[cfg(test)]
use plx_play_feed::native_load::load_alone;
use std::sync::atomic::Ordering;

/// raw ptr we assert is Send for the spawn (the boxes/queue outlive the thread).
pub struct SendPtr<T>(pub *mut T);
unsafe impl<T> Send for SendPtr<T> {}

/// Main-thread owned load correlation. The worker sees only this token and its return channel;
/// the adapter maps it back to the exact route attempt after Engine installation.
#[derive(Clone)]
pub(super) struct LoadReceipt {
    token: u64,
    sender: std::sync::mpsc::Sender<LoadOutcome>,
}
impl LoadReceipt {
    pub fn new(token: u64, sender: std::sync::mpsc::Sender<LoadOutcome>) -> Self {
        Self { token, sender }
    }
    pub fn token(&self) -> u64 { self.token }
    pub(super) fn deliver(self, native_epoch: u32, ok: bool, elapsed_ms: Option<u64>) {
        let _ = self.sender.send(LoadOutcome { token: self.token, native_epoch, ok, elapsed_ms });
    }
}

/// The native Load call's synchronous verdict, addressed without reading route state on-worker.
pub(super) struct LoadOutcome {
    pub token: u64,
    pub native_epoch: u32,
    pub ok: bool,
    /// Load time excluding the sound-output read; absent if the native epoch did not open its gate.
    pub elapsed_ms: Option<u64>,
}

/// The media-side implementation of the physical Load port. The lower worker never names
/// Shared, Engine or a route; the port keeps its existing epoch mirrors in this transition.
struct PlayerLoadPort;

impl NativeLoadPort for PlayerLoadPort {
    unsafe fn load(&self, payload: *mut c_char, epoch: u32) -> c_int {
        unsafe { super::sink().load(payload, epoch) }
    }

    fn mark_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<std::time::Duration> {
        SHARED.mark_native_load_returned(epoch, pcm_lead_ns)
    }

    fn mirror_elapsed_ms(&self, elapsed_ms: u64) {
        SHARED.native_load_elapsed_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    fn publish_load_failure(&self, epoch: u32) {
        SHARED.publish_native_load_failure(epoch);
    }
}

/// Spawned media worker's entry: physical Load executes below media, while this receipt stays
/// owned by the adapter's addressed token/attempt mapping.
pub(super) fn load_thread(
    payload: SendPtr<c_char>,
    native_epoch: u32,
    receipt: Option<LoadReceipt>,
    pcm_lead: PcmLead,
) {
    load_thread_with(payload, native_epoch, receipt, pcm_lead, plx_play_feed::pcm_lead::resolve_on_load_thread);
}

/// Same physical Load with a sound-output resolver supplied for the timing fixture.
fn load_thread_with(
    payload: SendPtr<c_char>,
    native_epoch: u32,
    receipt: Option<LoadReceipt>,
    pcm_lead: PcmLead,
    resolve_lead: impl FnOnce(PcmLead) -> Option<i64>,
) {
    native_load::run(&PlayerLoadPort, payload.0, native_epoch, pcm_lead, resolve_lead, |ok, elapsed_ms| {
        if let Some(receipt) = receipt {
            receipt.deliver(native_epoch, ok, elapsed_ms);
        }
    });
}

#[cfg(test)]
mod load_timing_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_settings_read_is_taken_out_of_the_load_time_and_never_below_zero() {
        let ms = Duration::from_millis;
        assert_eq!(load_alone(ms(900), ms(300)), ms(600));
        assert_eq!(load_alone(ms(900), ms(0)), ms(900), "a Load with no read reports what it took");
        assert_eq!(load_alone(ms(100), ms(300)), ms(0), "a clock that disagrees never goes negative");
    }

    /// A read that takes 400 ms in front of a Load that returns at once: the figures the Load-timing
    /// log and telemetry are made from say the Load was quick, while the wall clock the pump's
    /// `NATIVE_LOAD_BUDGET` reads still counts the read.
    #[cfg(feature = "hostsim")]
    #[test]
    fn a_slow_read_is_not_counted_as_load_time_but_is_counted_by_the_budget_clock() {
        const READ: Duration = Duration::from_millis(400);
        let _serial = plx_base::testlock::serial();
        SHARED.reset_session();
        crate::player::ffi_host::force_clocksink_for_test(true);
        let epoch = SHARED.begin_native_session().expect("native session");
        let payload = c"{}";
        load_thread_with(
            SendPtr(payload.as_ptr() as *mut c_char),
            epoch,
            None,
            PcmLead::FromOutput(plx_platform::devcaps::AudioStack::Liblxa),
            |_| {
                std::thread::sleep(READ);
                Some(400_000_000)
            },
        );
        let reported = SHARED.native_load_elapsed_ms.load(Ordering::Relaxed);
        let budget_clock = SHARED.native_load_elapsed(epoch).expect("the epoch is still Active");
        crate::player::ffi_host::force_clocksink_for_test(false);
        SHARED.test_force_native_idle();
        SHARED.reset_session();
        assert!(
            reported < READ.as_millis() as u64 / 2,
            "the Load returned at once, but its reported time is {reported} ms: the {READ:?} read is in it"
        );
        assert!(
            budget_clock >= READ,
            "NATIVE_LOAD_BUDGET's clock ({budget_clock:?}) must still count the read it waited behind"
        );
    }
}
