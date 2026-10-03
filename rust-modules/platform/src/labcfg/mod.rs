//! **The Lab Diagnostics configuration, and the two questions the lower layers ask of it.**
//!
//! The application's `lab` module is the bridge itself — the ring, the upload, the command long-poll, the toast —
//! and it names the whole application, so it lives in the top layer. Two of its answers are needed
//! far below that: `ui::consts::is_bound` has to know whether a press is the configured trigger,
//! and the account menu and the player overflow have to know whether to show the *Send
//! diagnostics* row. Both are functions of one thing, `lab.json`, whose reader names nothing above
//! `plx_base::paths`. That reader and those two answers are therefore here, in `platform`, beside
//! the other things this television keeps (`webos`, `devcaps`, `keymanager`); the application's
//! `lab` module reads the same `config` from above.
//!
//! The gating is exactly what it was under `lab`: without the `lab-diagnostics` cargo feature the
//! reader is not compiled at all, and both answers below are constant `false` at compile time, so
//! the call sites in `ui/` and `screens/` carry no `#[cfg]` of their own.

#[cfg(feature = "lab-diagnostics")]
pub mod config;

/// Is this press the configured lab trigger? Consulted by `ui::consts::is_bound` so that pressing
/// it does not ALSO wake the player HUD and abort an armed click — the unsupported-key invariant
/// in `docs/remote-keys.md` §6, seen from the side of a key that is genuinely bound in this build.
#[inline]
pub fn is_trigger_key(_sym: u32, _wcode: u32) -> bool {
    #[cfg(feature = "lab-diagnostics")]
    {
        return config::get().is_some_and(|c| c.is_trigger(_sym, _wcode));
    }
    #[cfg(not(feature = "lab-diagnostics"))]
    false
}

/// Should the lab entry appear in the account menu / player overflow? False in every build that
/// does not have a working lab configuration, so the row cannot be a dead control.
#[inline]
pub fn menu_row_enabled() -> bool {
    #[cfg(feature = "lab-diagnostics")]
    {
        return config::get().is_some();
    }
    #[cfg(not(feature = "lab-diagnostics"))]
    false
}
