//! The parameters the system launched us with.
//!
//! SAM starts the process with one argument, a JSON object whose `parameters` member is what the
//! caller passed to `com.webos.applicationManager/launch` (measured on webOS 9.2.2, LG C2,
//! 2026-10-19): `{"event":"launch","reason":…,"appId":…,"parameters":{…},"interfaceVersion":2,…}`.
//! `main.c` hands it to [`offer`].
//!
//! Only a COLD launch carries parameters the app can read. Launched again while it runs, SAM sends
//! the new parameters as a `relaunch` event to the app's `registerApp` subscription, which LG's SDL
//! holds; SDL raises the window and drops them. The app cannot hold that subscription itself: SAM
//! answers an anonymous client `cannot find caller id`, the app-id service name is already taken in
//! this process, and `LSCallFromApplication` is refused as unprivileged (all three measured on the
//! same set). A caller that needs its parameters honoured closes the app first.
//!
//! The payload is kept raw here: what it MEANS (a Plex title to open) is the app's business.
use std::sync::Mutex;

static PENDING: Mutex<Option<String>> = Mutex::new(None);

/// The launch argument of a cold start, as `argv[1]` carried it; the app decides what it means.
pub fn offer(raw: &str) {
    *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some(raw.to_owned());
}

/// The launch payload not yet acted on, once.
pub fn take() -> Option<String> {
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
