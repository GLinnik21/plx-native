//! The physical Starfish Load worker. The media adapter owns its receipt and route correlation;
//! this worker receives only the native epoch, resolved PCM plan and a narrow physical port.
use crate::feed_resources::LoadReceipt;
use crate::pcm_lead::PcmLead;
use crate::shared::FEED_SHARED;
use crate::SendPtr;
use plx_platform::tv::sink::VideoSink;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Epoch-scoped physical operations the Load worker needs: the Load itself, and the epoch gate and
/// Load-time mirrors of [`FEED_SHARED`]. The worker holds no Engine, route or product report state.
pub trait NativeLoadPort {
    /// # Safety
    /// `payload` is a live NUL-terminated Load declaration for the duration of the call.
    unsafe fn load(&self, payload: *mut c_char, epoch: u32) -> c_int;
    fn mark_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<Duration>;
    fn mirror_elapsed_ms(&self, elapsed_ms: u64);
    fn publish_load_failure(&self, epoch: u32);
}

/// The Load worker's port: the epoch gate, the Load-time mirror and the failure publication are
/// [`FEED_SHARED`]'s; the Load itself goes through the `sink` the composition supplies (the
/// `FeedHooks.sink` shape, so the fence an off-frame `Unload` raises applies to a Load too).
struct SharedLoadPort {
    sink: fn() -> &'static dyn VideoSink,
}

impl NativeLoadPort for SharedLoadPort {
    unsafe fn load(&self, payload: *mut c_char, epoch: u32) -> c_int {
        unsafe { (self.sink)().load(payload, epoch) }
    }

    fn mark_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<Duration> {
        FEED_SHARED.mark_native_load_returned(epoch, pcm_lead_ns)
    }

    fn mirror_elapsed_ms(&self, elapsed_ms: u64) {
        FEED_SHARED.native_load_elapsed_ms.store(elapsed_ms, Ordering::Relaxed);
    }

    fn publish_load_failure(&self, epoch: u32) {
        FEED_SHARED.publish_native_load_failure(epoch);
    }
}

/// The `load_thread` body: resolve the sound output's PCM lead off the frame, Load through `sink`,
/// and hand the addressed result to `receipt`.
pub fn load_thread(
    sink: fn() -> &'static dyn VideoSink,
    payload: SendPtr<c_char>,
    native_epoch: u32,
    receipt: Option<LoadReceipt>,
    pcm_lead: PcmLead,
) {
    load_thread_with(sink, payload, native_epoch, receipt, pcm_lead, crate::pcm_lead::resolve_on_load_thread);
}

fn load_thread_with(
    sink: fn() -> &'static dyn VideoSink,
    payload: SendPtr<c_char>,
    native_epoch: u32,
    receipt: Option<LoadReceipt>,
    pcm_lead: PcmLead,
    resolve_lead: impl FnOnce(PcmLead) -> Option<i64>,
) {
    run(&SharedLoadPort { sink }, payload.0, native_epoch, pcm_lead, resolve_lead, |ok, elapsed_ms| {
        if let Some(receipt) = receipt {
            receipt.deliver(native_epoch, ok, elapsed_ms);
        }
    });
}

/// The time Starfish `Load` itself took: the clock from `begin_native_session` less the
/// sound-output read done before `Load`. The timeout budget still uses the whole clock.
pub fn load_alone(since_issued: Duration, settings_read: Duration) -> Duration {
    since_issued.saturating_sub(settings_read)
}

/// Construct and synchronously Load on the media worker. The C seam refuses concurrent verbs
/// until `Load` returns; its epoch gate opens before the addressed result is delivered, and a
/// synchronous failure is published only after that delivery, exactly as before extraction.
pub fn run(
    port: &impl NativeLoadPort,
    payload: *mut c_char,
    native_epoch: u32,
    pcm_lead: PcmLead,
    resolve_lead: impl FnOnce(PcmLead) -> Option<i64>,
    deliver: impl FnOnce(bool, Option<u64>),
) {
    // Read the sound output before Load and off the frame thread. Nothing is fed until the
    // returned lead is published with the native epoch gate.
    let read_started = Instant::now();
    let pcm_lead_ns = resolve_lead(pcm_lead);
    let read_took = read_started.elapsed();
    plx_base::eventlog::log("SMP: calling Load (uid=NULL)");
    let ok = unsafe { port.load(payload, native_epoch) };
    plx_base::eventlog::log(&format!("SMP: Load returned ok={ok}"));

    // Developer hold: after Load returns, before the epoch gate flips. It exercises the same
    // Load budget and callback deferral path as a blocked firmware Load.
    if let Some(ms) = plx_base::devtrig::holdload_delay_ms() {
        plx_base::eventlog::log(&format!(
            "holdload: armed — holding the Load-returned flag for {ms}ms"
        ));
        std::thread::sleep(Duration::from_millis(ms));
    }

    // Opening the epoch gate, mirroring its time and logging all precede the receipt. Keep the
    // native elapsed mirror at this transition even when no product receipt was requested.
    let elapsed_ms = port.mark_load_returned(native_epoch, pcm_lead_ns).map(|elapsed| {
        let elapsed_ms = load_alone(elapsed, read_took).as_millis() as u64;
        port.mirror_elapsed_ms(elapsed_ms);
        plx_base::eventlog::log(&format!("native: Load returned after {elapsed_ms}ms"));
        elapsed_ms
    });
    deliver(ok != 0, elapsed_ms);
    if ok == 0 {
        port.publish_load_failure(native_epoch);
    }
}

#[cfg(test)]
mod load_timing_tests {
    use super::*;

    #[test]
    fn the_settings_read_is_taken_out_of_the_load_time_and_never_below_zero() {
        let ms = Duration::from_millis;
        assert_eq!(load_alone(ms(900), ms(300)), ms(600));
        assert_eq!(load_alone(ms(900), ms(0)), ms(900), "a Load with no read reports what it took");
        assert_eq!(load_alone(ms(100), ms(300)), ms(0), "a clock that disagrees never goes negative");
    }

    #[cfg(feature = "hostsim")]
    mod hostsim {
        use super::*;
        fn test_sink() -> &'static dyn VideoSink { crate::ffi_host::test_sink::sink() }

        /// A read that takes 400 ms in front of a Load that returns at once: the figures the
        /// Load-timing log and telemetry are made from say the Load was quick, while the wall clock
        /// the pump's `NATIVE_LOAD_BUDGET` reads still counts the read.
        #[test]
        fn a_slow_read_is_not_counted_as_load_time_but_is_counted_by_the_budget_clock() {
            const READ: Duration = Duration::from_millis(400);
            let _serial = plx_base::testlock::serial();
            FEED_SHARED.reset_session_with(|| ());
            crate::ffi_host::force_clocksink_for_test(true);
            let epoch = FEED_SHARED.begin_native_session().expect("native session");
            let payload = c"{}";
            load_thread_with(
                test_sink,
                SendPtr(payload.as_ptr() as *mut c_char),
                epoch,
                None,
                PcmLead::FromOutput(plx_platform::devcaps::AudioStack::Liblxa),
                |_| {
                    std::thread::sleep(READ);
                    Some(400_000_000)
                },
            );
            let reported = FEED_SHARED.native_load_elapsed_ms.load(Ordering::Relaxed);
            let budget_clock = FEED_SHARED.native_load_elapsed(epoch).expect("the epoch is still Active");
            crate::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            FEED_SHARED.reset_session_with(|| ());
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
}
