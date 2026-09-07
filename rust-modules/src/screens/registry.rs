//! **The application bundle's SCREEN-SIDE half** (restructure spec §3.1, phase 5b): the effects a
//! screen may ask for (`AppFx`), the messages a machine receives (`AppMsg`), and the requests an
//! owned screen makes of the legacy LOOP (`LoopReq`) while the two coexist (§14).
//!
//! The concrete `Host` impl — the `Arg` enum that names every screen, the mounter's one `match` —
//! lives in `app/bridge.rs` and not here, for one reason the layer rule cannot argue with: its
//! `Arg` still carries the legacy `Route` (§14: "`Route` survives only as its argument"), and
//! `Route` is `app`-private. So the screens are GENERIC over any host that carries this bundle
//! ([`AppLike`]), and the bridge instantiates them for its `AppHost`; the Settings family
//! instantiates the same screens a second time for the surface's own inner stack
//! (`screens::settings::InnerHost`), which is how one `OnboardScreen` mounts twice (§6.2).
//!
//! `LoopReq` is a DEBT with a phase number on each variant: a request the loop performs because
//! the machine that should own it (Session, Player, Navigation over the app's real stack) is not
//! on the dispatcher yet. The bridge drains them after every dispatcher frame.

use crate::stores::{StoreCmd, StoreId};
use crate::ui::machine::Host;

/// The application's effects (spec §3.1). `Store` since phase 4; `Consent` and `Loop` since 5b.
pub(crate) enum AppFx {
    /// A store command, executed as a `Deliver` to the store machine in the same drain.
    ///
    /// **`#[allow(dead_code)]` because nothing CONSTRUCTS it outside a test yet.** The bridge
    /// matches it (`app/bridge.rs`'s `app_fx` drain turns it into the `Deliver` above), and a
    /// match is not a construction as far as `dead_code` is concerned, so `-D warnings` fails the
    /// `--no-default-features` gate on the variant alone. The owned screens that mutate a store
    /// still call `stores::<store>::apply` directly — the synchronous shim phase 4 landed — and
    /// the variant becomes live the first time one emits its mutation as an EFFECT instead
    /// (spec §14's "same drain" ordering, phase 6). Delete this attribute then; it costs nothing
    /// while it is stale, but it is a claim about the tree and should not outlive being true.
    #[allow(dead_code)]
    Store(StoreId, StoreCmd),
    /// The consent MACHINE's command (§2.2): it owns the two decisions and publishes them.
    Consent(ConsentCmd),
    /// A request of the legacy loop (§14) — see [`LoopReq`].
    Loop(LoopReq),
}

/// The application's messages (spec §3.1).
pub(crate) enum AppMsg {
    Store(StoreCmd),
}

/// What the consent machine is told (§2.3): a person's answer to both questions at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConsentCmd {
    Record { errors: bool, usage: bool },
}

/// What an owned screen asks the LEGACY LOOP to do, because the owner of that decision is not on
/// the dispatcher yet. Each names the phase that retires it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoopReq {
    /// BACK at a root the platform owns (Home, the picker, sign-in, the FIRST consent question):
    /// hand the screen to the television. Retires with the Navigation root rule (phase 12).
    BackAtRoot,
    /// Privacy & data → Delete all local data, confirmed: erase, sign out, land on sign-in.
    /// Retires when Session owns the sign-in (phase 6).
    DeleteAllLocalData,
    /// The first-run Favourites screen (`Route::Onboard`) finished: enter Home. Retires with the
    /// route enum (phase 12, after 6 puts Login/Profiles/Onboard on one stack).
    OnboardDone,
    /// The first-run Favourites screen's BACK: the profile picker. Same retirement.
    OnboardBack,
}

/// Any host that carries this bundle. The screens under `screens/` are written against it, so the
/// bridge's `AppHost` and the Settings surface's inner host both mount them unchanged.
pub(crate) trait AppLike: Host<Elem = u32, Fx = AppFx, Msg = AppMsg> {}
impl<H: Host<Elem = u32, Fx = AppFx, Msg = AppMsg>> AppLike for H {}

/// The heartbeat words an owned screen can name (§15.3's word table) — the same alphabet
/// `app::route_word`/`overlay_word` print, so the fps tier's `overlay=` selection cannot drift
/// from the screen that owns the frame.
pub(crate) mod word {
    pub(crate) const SETTINGS: &str = "settings";
    pub(crate) const PRIVACY: &str = "privacy";
    pub(crate) const LEGAL: &str = "legal";
    pub(crate) const CONSENT: &str = "consent";
    pub(crate) const ONBOARD: &str = "onboard";
}

/// An element key for a route-family screen: table rows are their index; the action band's
/// controls sit above [`BAND`], so one `u32` namespace serves both groups of a screen.
///
/// **This number is repeated, not shared, and the repeat is `ui::table_screen::BAND_BASE`.**
/// `table_screen.rs` is a LIBRARY module and cannot name `screens::registry` (the layer rule:
/// `ui/` never names `screens/`), so the one place that actually MINTS a band element
/// (`BandPart::key`) carries its own copy of this literal with a comment pointing back here. The
/// assertion below is what keeps that a documented duplication rather than a silent one: if a
/// future edit moves this constant without moving its twin, `band_index`/`alert_index` would
/// misresolve every control in the family's action row (Privacy & data's Share/Don't Share, every
/// screen's Done/Try again) the next time anyone TYPED the mismatch, rather than the next time
/// anyone ran the app on a television.
pub(crate) const BAND: u32 = 0x4000_0000;
/// The decision alert's two answers (Cancel, Delete), above the band.
pub(crate) const ALERT: u32 = 0x4000_0100;

const _: () = assert!(
    BAND == crate::ui::table_screen::BAND_BASE,
    "screens::registry::BAND and ui::table_screen::BAND_BASE are the same address in two crates \
     that cannot import from each other; keep them numerically identical"
);

/// The inverse of [`band_index`]: where a family screen's Nth band control lives in the shared
/// `u32` namespace. Currently unused by any screen — every band element in the tree today is
/// minted by `table_screen::BandPart::key` (which carries its own copy of the same arithmetic,
/// for the layer reason on [`BAND`]'s doc) — kept here as the one place that STATES the forward
/// direction, and pinned by a round-trip test against `band_index` so the two cannot drift apart
/// silently if a future screen starts calling it directly instead of `BandPart`.
///
/// **`#[allow(dead_code)]` because its only callers are test modules** — this one's round-trip
/// test and `screens::onboard`'s, which mints the band key it presses through here rather than
/// writing `BAND + 0` out by hand. A `cfg(test)`-only caller is invisible to `dead_code` in the
/// build that ships, so `-D warnings` fails `--no-default-features` on it; the same reason
/// `screens::onboard::probe_fields` carries one. Deleting the function instead would delete the
/// only STATEMENT of the forward direction in this crate and leave `table_screen::BandPart::key`'s
/// copy of the arithmetic unpaired, which is the drift the const assert above exists to prevent.
#[allow(dead_code)]
pub(crate) fn band_elem(i: usize) -> u32 {
    BAND + i as u32
}
/// **`then`, not `then_some`, and the difference is a panic.** `bool::then_some` takes its value
/// by VALUE, so the subtraction is evaluated whatever the condition says — and every ordinary
/// table row is an `elem` far BELOW `BAND`, so `elem - BAND` underflows. The dev profile has
/// overflow checks on, so that is an outright panic on the commonest input this function has
/// ("attempt to subtract with overflow"), reached from any screen in the family that asks whether
/// the focused row is a band control. `then` takes a closure and so runs the arithmetic only on
/// the branch that already proved it cannot underflow.
///
/// A release build would not have panicked, which is what makes this worth a comment rather than
/// a silent edit: overflow wraps there, the guard is still `false`, and the function still answers
/// `None`. So the bug was invisible on the television and fatal in `make check` — the reverse of
/// the usual direction, and not something to re-derive from the diff.
pub(crate) fn band_index(elem: u32) -> Option<usize> {
    (elem >= BAND && elem < ALERT).then(|| (elem - BAND) as usize)
}
pub(crate) fn alert_index(elem: u32) -> Option<usize> {
    (elem >= ALERT && elem < ALERT + 2).then(|| (elem - ALERT) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row index stays itself under `band_elem`/`band_index` for the whole practical range of
    /// an action row (never more than two controls in this family today, but the round trip is
    /// asserted well past that so a widened band does not silently wrap into the alert's space).
    #[test]
    fn band_elem_and_band_index_round_trip() {
        for i in 0..64usize {
            let e = band_elem(i);
            assert!(e >= BAND && e < ALERT, "band_elem({i}) = {e:#x} left the band's own range");
            assert_eq!(band_index(e), Some(i));
        }
    }

    /// A raw table-row index (always far below [`BAND`]) is never mistaken for a band or alert
    /// control — the three ranges the family's `u32` namespace is carved into must not overlap.
    #[test]
    fn a_table_row_index_is_neither_a_band_nor_an_alert_element() {
        for row in [0u32, 1, 2, 41, 4095] {
            assert_eq!(band_index(row), None, "row {row} must not resolve as a band control");
            assert_eq!(alert_index(row), None, "row {row} must not resolve as an alert answer");
        }
    }

    /// The alert's two answers (Cancel, Delete) are the only two elements in its range, and the
    /// band's own top control does not spill into it.
    #[test]
    fn the_alert_range_holds_exactly_two_answers_just_above_the_band() {
        assert_eq!(alert_index(ALERT), Some(0));
        assert_eq!(alert_index(ALERT + 1), Some(1));
        assert_eq!(alert_index(ALERT + 2), None, "the alert's range is exactly two elements wide");
        assert_eq!(band_index(ALERT - 1), Some((ALERT - 1 - BAND) as usize), "the band's range runs right up to the alert's");
        assert_eq!(band_index(ALERT), None, "…and stops there — the two ranges must not overlap");
    }
}
