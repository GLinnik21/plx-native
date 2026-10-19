//! The parameters the system launched us with.
//!
//! SAM starts the process with one argument, a JSON object whose `parameters` member is what the
//! caller passed to `com.webos.applicationManager/launch` (measured on webOS 9.2.2, LG C2,
//! 2026-10-19): `{"event":"launch","reason":…,"appId":…,"parameters":{…},"interfaceVersion":2,…}`.
//! `main.c` hands it to [`offer`].
//!
//! A WARM launch (the app already running) sends its parameters as a `relaunch` event to the
//! `registerApp` subscription, which LG's SDL holds. The app cannot subscribe itself: SAM answers
//! an anonymous client `cannot find caller id`, the app-id service name is already taken in this
//! process, and `LSCallFromApplication` is refused as unprivileged (all three measured on the same
//! set). Instead `app::boot` wraps the slot SDL reads each reply's payload through and hands a
//! relaunch's payload here too. Where that slot is absent (webOS 4.x to 6.x) only a cold launch
//! carries parameters, and a caller closes the app first.
//!
//! The payload is kept raw here: what it MEANS (a Plex title to open) is the app's business.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static PENDING: Mutex<Option<String>> = Mutex::new(None);
/// Set with `PENDING`, so the per-frame [`take`] skips the lock while nothing has arrived.
static WAITING: AtomicBool = AtomicBool::new(false);

/// A launch payload: a cold start's `argv[1]`, or a relaunch event from any thread. The app decides
/// what it means; a newer payload replaces one not yet taken.
pub fn offer(raw: &str) {
    *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some(raw.to_owned());
    WAITING.store(true, Ordering::Release);
}

/// The launch payload not yet acted on, once. Cheap enough for every frame.
pub fn take() -> Option<String> {
    if !WAITING.swap(false, Ordering::Acquire) {
        return None;
    }
    PENDING.lock().unwrap_or_else(|p| p.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_the_argument_over_once() {
        let _serial = plx_base::testlock::serial();
        let _ = take();
        offer(r#"{"parameters":{"plex":{"ratingKey":"1"}}}"#);
        assert!(take().unwrap().contains(r#""1""#));
        assert_eq!(take(), None);
    }
}
