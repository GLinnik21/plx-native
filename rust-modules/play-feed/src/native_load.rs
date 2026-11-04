//! The physical Starfish Load worker. The media adapter owns its receipt and route correlation;
//! this worker receives only the native epoch, resolved PCM plan and a narrow physical port.
use crate::pcm_lead::PcmLead;
use std::os::raw::{c_char, c_int};
use std::time::{Duration, Instant};

/// Epoch-scoped physical operations whose storage remains in the media player's current Shared
/// bridge. The worker holds no Engine, route or product report state.
pub trait NativeLoadPort {
    /// # Safety
    /// `payload` is a live NUL-terminated Load declaration for the duration of the call.
    unsafe fn load(&self, payload: *mut c_char, epoch: u32) -> c_int;
    fn mark_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<Duration>;
    fn mirror_elapsed_ms(&self, elapsed_ms: u64);
    fn publish_load_failure(&self, epoch: u32);
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
