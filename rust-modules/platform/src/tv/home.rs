//! Handing the screen back: the latch that lets one BACK press at a root speak for a burst of
//! taps, and the two hooks into the platform that perform and finish the request.

/// How long one root press speaks for. Comfortably longer than the platform's 600 ms bus budget,
/// so a burst of taps cannot queue several full-budget stalls back to back, and short enough to be
/// under the time it takes a person to notice nothing happened and press again.
const COOLDOWN: std::time::Duration = std::time::Duration::from_millis(2000);

/// When the last root press was claimed, and the whole of the latch. `Mutex` rather than an atomic
/// clock because [`std::time::Instant`] is opaque: this is touched once per BACK press at a root,
/// never on a frame path.
static LAST_REQUEST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// **Claim the root press.** `true` when this one is live; `false` when a recent one still speaks
/// for it, and then the WHOLE press must do nothing.
///
/// A HELD back never gets here — a hardware auto-repeat carries `state & 0x100` and goes to
/// `app.rs`'s `on_auto_repeat`, which has no BACK action — but five separate taps are five separate
/// fresh presses. Two things make that expensive: on the failing path each one spends the
/// platform's 600 ms bus budget on the SDL main thread, and on the sign-in screen each one runs
/// `auth::cancel`, which is destructive whatever it answers. So the claim is what `app.rs` takes
/// FIRST, before either.
pub fn take_root_press() -> bool {
    take_root_press_at(std::time::Instant::now())
}

/// [`take_root_press`] with the clock passed in — the whole of the expiry rule, so the boundary is
/// gradeable without sleeping through it.
fn take_root_press_at(now: std::time::Instant) -> bool {
    let mut last = LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(age) = last.map(|t| now.saturating_duration_since(t)) {
        if age < COOLDOWN {
            plx_base::eventlog::log(&format!(
                "gohome: a root press {} ms ago still speaks for this one — ignoring it",
                age.as_millis()
            ));
            return false;
        }
    }
    *last = Some(now);
    true
}

/// **Hand a claim back**, for the press that turned out not to be a root press after all — the one
/// on the sign-in screen or the picker that DID have somewhere to go inside the app. Without this
/// the cooldown would swallow the real root BACK the user presses a moment later on the Home they
/// were just returned to.
pub fn release_root_press() {
    *LAST_REQUEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(any(test, feature = "test-support"))]
static HOME_REQUESTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// How many root presses [`go_home`] has actually ACTED on, this process — a press swallowed by
/// the cooldown does not count. **Test-only**, and the only way a host test can grade a call whose
/// whole effect is on a television.
#[cfg(any(test, feature = "test-support"))]
pub fn home_requests() -> u32 {
    HOME_REQUESTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// **Show the television's own Home, and keep running** — the port's request, which is not a quit:
/// the process stays alive and comes back as the same live process. Not rate-limited here;
/// [`take_root_press`] is, so call this only having claimed a press.
pub fn go_home() {
    #[cfg(any(test, feature = "test-support"))]
    HOME_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (super::port().go_home)()
}

/// The frame thread's half of a root press: the platform's fallback leg, when its off-thread leg
/// asked for one.
pub fn poll() {
    (super::port().poll_home)()
}

#[cfg(test)]
mod go_home_tests {
    use super::*;
    use std::time::Instant;

    /// **The expiry BOUNDARY, on a clock the test owns.** Driving it through [`take_root_press`]
    /// alone could only ever prove the suppressing half: clearing the latch to simulate time
    /// passing bypasses the elapsed-time branch entirely, so that test would pass with an
    /// infinite `COOLDOWN`. This one never sleeps and still grades the comparison.
    #[test]
    fn a_claim_speaks_for_exactly_the_cooldown_and_not_a_moment_longer() {
        let _g = plx_base::testlock::serial();
        let base = Instant::now();
        release_root_press();
        assert!(take_root_press_at(base), "a cold latch admits the press");
        assert!(
            !take_root_press_at(base + COOLDOWN / 2),
            "…and speaks for the whole cooldown"
        );
        release_root_press();
        assert!(take_root_press_at(base));
        assert!(
            take_root_press_at(base + COOLDOWN),
            "…but not past its end: a first attempt that achieved nothing stays retryable"
        );
        release_root_press();
    }

    /// The other half, through the real entry points: a burst of taps is ONE platform call, and a
    /// press handed back leaves the next one live.
    #[test]
    fn a_burst_of_root_presses_is_one_platform_call_and_a_release_undoes_the_claim() {
        let _g = plx_base::testlock::serial();
        release_root_press();
        let before = home_requests();
        for _ in 0..5 {
            if take_root_press() {
                go_home();
            }
        }
        assert_eq!(
            home_requests(),
            before + 1,
            "five taps must not queue five platform calls"
        );
        release_root_press();
        assert!(
            take_root_press(),
            "a handed-back claim must not swallow the next root press"
        );
        release_root_press();
    }
}
