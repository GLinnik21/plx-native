//! `/tmp/plxnative-clockfact=nokey|keychanged|engaged:<year>` — plant a wrong-clock fact at boot,
//! so the read-outs and the toast that read `net::keypin`'s facts can be looked at in the simulator
//! with no television, no certificate and no clock to change (issue #378).
//!
//! * `nokey` — [`keypin::Blocked::NoKey`]: a failed Home or Library names a wrong clock.
//! * `keychanged` — [`keypin::Blocked::KeyChanged`]: the same, for a server whose key changed.
//! * `engaged:<year>` — key mode engaged with the device believing `<year>`; `engaged` alone
//!   plants "year unknown". It raises `app::clock_notice`'s toast, which needs the TV's
//!   notification service and so only logs its attempt off-device.
//!
//! The fact is SYNTHETIC and says so in the log (`keypin::plant`): it is filed under a host no
//! session binds, so no projection clears it. Read once at boot; absent from shipping builds.

use plx_net::net::keypin::{self, Blocked, Planted};

/// The fact a trigger value names, or `None` for anything else.
pub(crate) fn parse(value: &str) -> Option<Planted> {
    let value = value.trim();
    match value {
        "nokey" => Some(Planted::Blocked(Blocked::NoKey)),
        "keychanged" => Some(Planted::Blocked(Blocked::KeyChanged)),
        "engaged" => Some(Planted::Engaged(None)),
        _ => value.strip_prefix("engaged:")?.trim().parse().ok().map(|year| Planted::Engaged(Some(year))),
    }
}

/// Called once from `app::boot`, after the session's key projection. A no-op without the trigger;
/// a value that names no fact is logged and ignored.
pub(crate) fn arm_at_boot() {
    let Some(value) = plx_base::devtrig::read("clockfact") else { return };
    match parse(&value) {
        Some(fact) => keypin::plant(fact),
        None => plx_base::eventlog::log("clockfact IGNORED — expected nokey, keychanged or engaged[:<year>]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trigger_names_three_facts_and_nothing_else() {
        assert_eq!(parse("nokey"), Some(Planted::Blocked(Blocked::NoKey)));
        assert_eq!(parse("keychanged\n"), Some(Planted::Blocked(Blocked::KeyChanged)));
        assert_eq!(parse("engaged"), Some(Planted::Engaged(None)));
        assert_eq!(parse("engaged:2019"), Some(Planted::Engaged(Some(2019))));
        for bad in ["", "NoKey", "engaged:", "engaged:soon", "key", "nokey:1"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
