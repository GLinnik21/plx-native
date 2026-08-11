//! What the connected server says about ITSELF — its version and whether its owner holds a
//! **Plex Pass** — held as process state for the diagnostics surfaces.
//!
//! Exists because of issue #22: a free server cannot encode HEVC, dropped the video track, and
//! the app "had no idea such a thing existed" — the reviewer derived the cause from the server's
//! own transcoder logs. The Plex Pass audit (`docs/plex-pass-audit.md`) names the bug class this
//! kills: a claim true on the development environment (a Pass'd server) asserted as universal.
//! This module is the app finally *knowing*, so `ui::stats` can print it and
//! `player::error_shape` can name the cause in words.
//!
//! **Visibility only, never behavior.** Nothing here may feed a routing or profile decision —
//! see [`subscription`]'s doc for why that is a rule and not a gap.
//!
//! One fetch per install: `client::install` calls [`refresh`] on every session path (boot, QR
//! login, profile switch), so no caller has to remember to. The fetch is `GET /` on the PMS —
//! probed live against PMS 1.43.3 (2026-08-10): the root `MediaContainer` carries
//! `"myPlexSubscription": true|false` and `"version": "1.43.3.10861-…"`. Both ride the ordinary
//! typed client (`get_json`, JSON Accept + the `with_token` choke point), so nothing new touches
//! the transport and no token can appear here or in the log line.

use super::client::Client;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};
use std::sync::Mutex;

/// Does the server's owner hold a Plex Pass? A TRISTATE, not a bool with a default: `Unknown`
/// (never fetched, fetch failed, or a PMS old enough not to say) must stay distinguishable from
/// a real "no" — the error wording in `player::error_shape` blames a missing Pass only on a
/// known-free server, never on one we merely haven't heard from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Subscription {
    Unknown = 0,
    No = 1,
    Yes = 2,
}

impl Subscription {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::No,
            2 => Self::Yes,
            _ => Self::Unknown,
        }
    }
    /// The wire field → the tristate. `None` is a server that never said (the field predates
    /// nothing we can date, so absence is only ever "unknown", never "free"); the lenient adapter
    /// has already folded `true`/`"1"`/`1` to nonzero and `false`/`"0"` to 0.
    fn from_wire(v: Option<i64>) -> Self {
        match v {
            None => Self::Unknown,
            Some(0) => Self::No,
            Some(_) => Self::Yes,
        }
    }
}

/// `Subscription` as u8 (the enum's own discriminants). 0 = Unknown is the boot state.
static SUBSCRIPTION: AtomicU8 = AtomicU8::new(0);
/// The server's full build string ("1.43.3.10861-cd85035e7"), "" until a fetch lands. Behind a
/// Mutex, read at 2 Hz by the stats panel's sample step — never in a per-frame path.
static VERSION: Mutex<String> = Mutex::new(String::new());
/// Single-flight: boot installs the client and a profile pick re-installs it moments later; two
/// live workers would answer the same question twice and double the log line.
static INFLIGHT: AtomicBool = AtomicBool::new(false);

/// The server owner's Plex Pass state, for DIAGNOSTICS — the stats panel and the playback error
/// wording.
///
/// **Never a routing/profile input.** If you are about to gate a transcode target, a codec
/// profile, or a direct-play decision on this: don't, for two reasons that do not expire.
/// The transcode target is already a fallback CHAIN (`transcoder.rs`: `hevc,h264` /
/// `ac3,eac3,aac`), so the server picks what it can actually encode — degradation is handled
/// where the capability lives, server-side. And this value is fetched asynchronously and can be
/// stale or `Unknown` at decision time (boot races, a fetch that failed, an old PMS that never
/// says), so a decision built on it would be issue #22's bug again with the polarity flipped:
/// a dev-environment claim — this time "we know the subscription" — asserted as universal.
pub(crate) fn subscription() -> Subscription {
    Subscription::from_u8(SUBSCRIPTION.load(Relaxed))
}

/// The server's build string, "" while unknown. Clones under the lock — callers sample it (the
/// stats panel holds each sample for 500 ms), they don't call it per frame.
pub(crate) fn version() -> String {
    VERSION.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Fetch `GET /` on a worker and store what it says. Called by `client::install`, i.e. once per
/// session path; fire-and-forget, and a refused spawn (`task::spawn_small`'s EAGAIN case) only
/// costs the refresh — the tristate honestly stays `Unknown`.
pub(super) fn refresh() {
    if INFLIGHT.swap(true, Relaxed) {
        return; // a fetch is already in flight; it answers for this install too (same server)
    }
    let spawned = crate::task::spawn_small("serverinfo", || {
        fetch_once();
        INFLIGHT.store(false, Relaxed);
    });
    if !spawned {
        INFLIGHT.store(false, Relaxed);
    }
}

/// One round trip, one log line. On failure the stored state is KEPT, not wiped: host/port fix at
/// the first install (a later `install` is only a token swap — `client.rs`), so a stale answer
/// can never describe a different server, while wiping would let one wifi hiccup blank a fact we
/// already knew.
fn fetch_once() {
    let Some(c) = super::client_opt() else { return };
    let Some(mc) = c.server_root() else {
        crate::log("pms: server info unavailable (GET / failed) — subscription stays unknown");
        return;
    };
    let mut sub = Subscription::from_wire(mc.my_plex_subscription);
    // dev: /tmp/plxnative-nopass — pretend THIS server answered "no Plex Pass". The dev server
    // has one, so every Pass-conditional surface (the facts row's HDR warning, the read-out's
    // capsule, the stats Server row) is otherwise unreachable on the only TV we own. Checked
    // here, once per fetch, rather than in `subscription()` — that accessor is on per-frame
    // paths and `dev::flag` is a filesystem stat.
    if crate::dev::flag("nopass") {
        crate::log("pms: /tmp/plxnative-nopass — reporting subscription as No");
        sub = Subscription::No;
    }
    SUBSCRIPTION.store(sub as u8, Relaxed);
    if let Ok(mut g) = VERSION.lock() {
        *g = mc.version.clone();
    }
    // The version + subscription bit identify a RELEASE, not a household — safe to log, and safe
    // for the stats panel to photograph. The token never appears: the URL is built and consumed
    // inside `get_json`.
    crate::log(&format!(
        "pms: version={} plexPass={}",
        if mc.version.is_empty() { "unknown" } else { &mc.version },
        match sub {
            Subscription::Yes => "true",
            Subscription::No => "false",
            Subscription::Unknown => "unknown",
        }
    ));
}

impl Client {
    /// `GET /` — the server root: its self-description envelope (version, capabilities, and the
    /// owner's `myPlexSubscription`). The two fields consumed live on the shared `MediaContainer`
    /// (see `models.rs`) so the ordinary envelope parse serves this too.
    fn server_root(&self) -> Option<super::models::MediaContainer> {
        self.get_json("/")
    }
}

#[cfg(test)]
mod tests {
    use super::Subscription;
    use crate::plex::models::Envelope;

    /// The tristate mapping is the module's contract: absence is UNKNOWN (an old server that
    /// never says must not read as free), zero is a real "no", anything else a real "yes" — and
    /// the u8 round trip through the atomic must not invent a state.
    #[test]
    fn the_wire_field_maps_to_the_tristate_without_inventing_knowledge() {
        assert_eq!(Subscription::from_wire(None), Subscription::Unknown);
        assert_eq!(Subscription::from_wire(Some(0)), Subscription::No);
        assert_eq!(Subscription::from_wire(Some(1)), Subscription::Yes);
        for s in [Subscription::Unknown, Subscription::No, Subscription::Yes] {
            assert_eq!(Subscription::from_u8(s as u8), s, "u8 round trip");
        }
        // an unmapped u8 (a torn write could not produce one, but the match must still be total)
        assert_eq!(Subscription::from_u8(7), Subscription::Unknown);
    }

    /// The plex/CLAUDE.md lenient-number rule, applied to the root envelope: PMS sends
    /// `myPlexSubscription` as a JSON bool on 1.43 and is free to string-encode it elsewhere, and
    /// a strict field would fail the WHOLE root parse — reading as "server info unavailable" on
    /// exactly the endpoint issue #22 needs. All three shapes must land, and absence must stay
    /// `None` (→ Unknown), not default to a confident 0 (→ "no Plex Pass").
    #[test]
    fn the_root_envelope_survives_every_encoding_of_the_subscription_flag() {
        let as_bool = br#"{"MediaContainer":{"size":30,"version":"1.43.3.10861-cd85035e7",
            "myPlexSubscription":true}}"#;
        let e: Envelope = serde_json::from_slice(as_bool).expect("bool form");
        assert_eq!(e.media_container.version, "1.43.3.10861-cd85035e7");
        assert_eq!(Subscription::from_wire(e.media_container.my_plex_subscription), Subscription::Yes);

        let as_string = br#"{"MediaContainer":{"myPlexSubscription":"1","version":"1.42.0.9999-abc"}}"#;
        let e: Envelope = serde_json::from_slice(as_string).expect("string form");
        assert_eq!(Subscription::from_wire(e.media_container.my_plex_subscription), Subscription::Yes);

        let as_false = br#"{"MediaContainer":{"myPlexSubscription":false,"version":"1.43.3.10861-x"}}"#;
        let e: Envelope = serde_json::from_slice(as_false).expect("false form");
        assert_eq!(Subscription::from_wire(e.media_container.my_plex_subscription), Subscription::No);

        let absent = br#"{"MediaContainer":{"size":30,"version":"0.9.9"}}"#;
        let e: Envelope = serde_json::from_slice(absent).expect("absent form");
        assert_eq!(Subscription::from_wire(e.media_container.my_plex_subscription), Subscription::Unknown);
    }
}
