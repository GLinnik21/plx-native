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

/// How many payloads wait for the next frame. The app takes them every frame; more than a handful
/// can only mean the loop is not running (before SDL_Init has returned, or a stall), and the
/// newest are the ones that matter, so the oldest are dropped. 8 is far past a cold argv link plus
/// the relaunches a person can press before the first frame.
const MAX_PENDING: usize = 8;

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Set with `PENDING`, so the per-frame [`take_all`] skips the lock while nothing has arrived.
static WAITING: AtomicBool = AtomicBool::new(false);

/// A launch payload: a cold start's `argv[1]`, or a relaunch event from any thread. The app decides
/// what it means; payloads queue in arrival order so a link-less relaunch cannot erase a link that
/// no frame has taken yet.
pub fn offer(raw: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    if pending.len() == MAX_PENDING {
        pending.remove(0);
    }
    pending.push(raw.to_owned());
    WAITING.store(true, Ordering::Release);
}

/// Every launch payload not yet acted on, oldest first, once. Cheap enough for every frame.
pub fn take_all() -> Vec<String> {
    if !WAITING.swap(false, Ordering::Acquire) {
        return Vec::new();
    }
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|p| p.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_the_argument_over_once() {
        let _serial = plx_base::testlock::serial();
        let _ = take_all();
        offer(r#"{"parameters":{"plex":{"ratingKey":"1"}}}"#);
        let taken = take_all();
        assert!(taken.len() == 1 && taken[0].contains(r#""1""#));
        assert!(take_all().is_empty());
    }

    #[test]
    fn queues_in_order_and_drops_the_oldest_past_the_bound() {
        let _serial = plx_base::testlock::serial();
        let _ = take_all();
        for i in 0..MAX_PENDING + 2 {
            offer(&i.to_string());
        }
        let taken = take_all();
        assert_eq!(taken.len(), MAX_PENDING);
        assert_eq!(taken.first().map(String::as_str), Some("2"));
        assert_eq!(taken.last().map(String::as_str), Some(&*(MAX_PENDING + 1).to_string()));
    }
}
