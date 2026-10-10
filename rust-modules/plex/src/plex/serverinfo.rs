//! What the connected server says about ITSELF — its version and whether its owner holds a
//! **Plex Pass** — held as process state for the diagnostics surfaces.
//!
//! Exists because of issue #22: a free server cannot encode HEVC, dropped the video track, and
//! the app "had no idea such a thing existed" — the reviewer derived the cause from the server's
//! own transcoder logs. The Plex Pass audit (`docs/plex-pass-audit.md`) names the bug class this
//! kills: a claim true on the development environment (a Pass'd server) asserted as universal.
//! This module is the app finally *knowing*, so `app::diagnostics` can print it and
//! `player::error_shape` can name the cause in words.
//!
//! **Visibility only, never behavior.** Nothing here may feed a routing or profile decision —
//! see [`subscription`]'s doc for why that is a rule and not a gap. "Visibility" includes whether
//! a Plex Pass feature is OFFERED at all (issue #266's audio enhancement, `route::enhancements_offered`):
//! the subscription decides that a toggle exists, and never which flavour or profile a stream gets.
//!
//! **PER SERVER, keyed the way the registry is.** This was one process-global answer, justified
//! by "host/port fix at the first install (a later install is only a token swap)" — a premise
//! [`super::servers`] retired: the table re-targets the current server, and a household can hold
//! its own server and a friend's share at once. A single global would mean the stats panel and
//! `player::error_shape` describing SERVER A's build and Plex Pass while the user is looking at
//! (or failing to play) something from server B — issue #22's bug with the polarity flipped, a
//! claim true of the development environment asserted as universal. So state is an array indexed
//! by [`ServerId::raw`], the registry's own slot number, and every accessor comes in two forms:
//! `_of(id)` for a caller that knows which server it is talking about, and the bare form for one
//! that means "the current server" (which is what every reader meant when there was only one).
//!
//! One fetch per registration: `servers::register` calls [`refresh`] for the server it just
//! registered — own or shared — so no caller has to remember to. The fetch is `GET /` on that
//! PMS — probed live against PMS 1.43.3 (2026-08-10): the root `MediaContainer` carries
//! `"myPlexSubscription": true|false` and `"version": "1.43.3.10861-…"`. Both ride the ordinary
//! typed client (`get_json`, JSON Accept + the `with_token` choke point), so nothing new touches
//! the transport and no token can appear here or in the log line.

use super::client::Client;
use super::servers::ServerId;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Does the server's owner hold a Plex Pass? A TRISTATE, not a bool with a default: `Unknown`
/// (never fetched, fetch failed, or a PMS old enough not to say) must stay distinguishable from
/// a real "no" — the error wording in `player::error_shape` blames a missing Pass only on a
/// known-free server, never on one we merely haven't heard from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Subscription {
    #[default]
    Unknown = 0,
    No = 1,
    Yes = 2,
}

impl Subscription {
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

/// What this module keeps about one server. A server with no entry reads as the boot state:
/// `Unknown`, no build string, no fetch in flight.
#[derive(Default)]
struct Entry {
    /// The owner's Plex Pass answer, `Unknown` until a fetch lands.
    sub: Subscription,
    /// The server's full build string ("1.43.3.10861-cd85035e7"), "" until its fetch lands.
    version: String,
    /// Single-flight: boot registers a server and a profile pick re-registers it moments later;
    /// two live workers would answer the same question twice and double the log line. Per server
    /// rather than global because two servers' fetches are different questions — a shared server
    /// registered while our own is mid-fetch must not be silently skipped.
    inflight: bool,
}

/// Every server's [`Entry`], keyed by its registry slot number. A map, not an array indexed by the
/// slot: the registry issues slot numbers past any fixed size (a sign-out consumes one per
/// registration), and a server that a fixed table cannot hold must still answer for itself.
///
/// One `Mutex` for all three fields, because they are always read and written together and every
/// critical section is a few field moves — never a round trip (the fetch happens first, the store
/// second). The readers are the detail hero's per-frame subscription read and the stats panel's
/// 2 Hz sample; an uncontended lock is an atomic compare-exchange, the same cost the three
/// atomics it replaces paid per read.
static SERVERS: Mutex<BTreeMap<u16, Entry>> = Mutex::new(BTreeMap::new());

/// Run `f` on the table, recovering from a poisoned lock: a panic in another reader must not turn
/// every later answer into "nothing known".
fn with_table<R>(f: impl FnOnce(&mut BTreeMap<u16, Entry>) -> R) -> R {
    f(&mut SERVERS.lock().unwrap_or_else(|e| e.into_inner()))
}

/// [`subscription`] for a NAMED server — what a caller uses once it knows which server the item,
/// the failed playback or the panel row belongs to. `Unknown` for a server that has not answered
/// yet, and for an id that names nothing.
pub fn subscription_of(id: ServerId) -> Subscription {
    if super::client_for(id).is_none() {
        return Subscription::Unknown;
    }
    with_table(|t| t.get(&id.raw()).map_or(Subscription::Unknown, |e| e.sub))
}

/// [`version`] for a NAMED server; "" while unknown.
pub fn version_of(id: ServerId) -> String {
    if super::client_for(id).is_none() {
        return String::new();
    }
    with_table(|t| t.get(&id.raw()).map(|e| e.version.clone()).unwrap_or_default())
}

/// The CURRENT server owner's Plex Pass state, for DIAGNOSTICS — the stats panel and the playback
/// error wording.
///
/// "Current" is a real choice, not a default: a reader that knows WHICH server it is describing
/// must call [`subscription_of`] instead, or with two servers registered it will happily attribute
/// one server's subscription to the other's item.
///
/// **Never a routing/profile input.** If you are about to gate a transcode target, a codec
/// profile, or a direct-play decision on this: don't, for two reasons that do not expire.
/// The transcode target is already a fallback CHAIN (`transcoder.rs`: `hevc,h264` /
/// `ac3,eac3,aac`), so the server picks what it can actually encode — degradation is handled
/// where the capability lives, server-side. And this value is fetched asynchronously and can be
/// stale or `Unknown` at decision time (boot races, a fetch that failed, an old PMS that never
/// says), so a decision built on it would be issue #22's bug again with the polarity flipped:
/// a dev-environment claim — this time "we know the subscription" — asserted as universal.
///
/// **What it MAY decide is whether a Plex Pass feature is offered** (issue #266's I8): the audio
/// enhancement (`route::enhancements_offered`) is shown, and asked for, only on a server known to
/// answer `Yes`. That is visibility, not routing — `Unknown` and `No` both hide it, so a stale or
/// missing answer degrades to exactly the app without the feature, never to a different flavour,
/// and once the viewer has opted in the request still goes through the ordinary flavour machinery
/// (`route::flavors_allowed`) and falls back if the server refuses. It never picks the flavour or
/// the profile for anything that is not that opt-in.
pub fn subscription() -> Subscription {
    subscription_of(super::current_server())
}

/// The CURRENT server's build string, "" while unknown. Clones under the lock — callers sample it
/// (the stats panel holds each sample for 500 ms), they don't call it per frame. A caller that
/// knows which server it means wants [`version_of`], for [`subscription`]'s reason.
pub fn version() -> String {
    version_of(super::current_server())
}

/// Fetch `GET /` for ONE server on a worker and store what it says. Called by
/// `servers::register`, i.e. once per registration (own server or shared); fire-and-forget, and a
/// refused spawn (`task::spawn_small`'s EAGAIN case) only costs the refresh — the tristate
/// honestly stays `Unknown`.
///
/// **The client is resolved HERE, at the spawn site, and moved in.** The worker never asks the
/// registry anything: `client()` inside it would mean "whichever server is current when the
/// thread happens to run", which with two servers is how server A's build string ends up filed
/// under server B. The `&'static Client` is sound to hold across the spawn because registry
/// clients are leaked (see `servers.rs`) — a re-point mid-fetch costs one round trip to where
/// that server used to be, never a dangling reference.
pub(super) fn refresh(id: ServerId) {
    if !id.is_set() {
        return;
    }
    let Some(c) = super::client_for(id) else {
        return;
    }; // nothing registered there to ask
    let claimed = with_table(|t| {
        let e = t.entry(id.raw()).or_default();
        // a fetch for THIS server is already in flight; it answers for this one too
        !std::mem::replace(&mut e.inflight, true)
    });
    if !claimed {
        return;
    }
    let spawned = plx_base::task::spawn_small("serverinfo", move || {
        fetch_once(id, c);
        set_inflight(id, false);
    });
    if !spawned {
        set_inflight(id, false);
    }
}

fn set_inflight(id: ServerId, on: bool) {
    with_table(|t| t.entry(id.raw()).or_default().inflight = on);
}

/// One round trip, one log line, for the server whose client was captured at the spawn site.
///
/// On failure that server's stored state is KEPT, not wiped. The old justification for keeping it
/// ("host/port fix at the first install") died with the singleton; the surviving one is per-slot
/// and stronger: a registry slot is one SERVER for the life of the process — `register` matches
/// on `machineIdentifier`, and a slot whose address moves is the same machine at a new address —
/// so a stale answer still describes the server it is filed under, while wiping would let one
/// wifi hiccup blank a fact we already knew.
fn fetch_once(id: ServerId, c: &Client) {
    if !id.is_set() {
        return;
    }
    let i = id.raw();
    let Some(mc) = c.server_root() else {
        plx_base::eventlog::log(&format!(
            "pms: server {i} info unavailable (GET / failed) — subscription stays unknown"
        ));
        return;
    };
    let mut sub = Subscription::from_wire(mc.my_plex_subscription);
    // dev: /tmp/plxnative-nopass — pretend the server answered "no Plex Pass". The dev server
    // has one, so every Pass-conditional surface (the facts row's HDR warning, the read-out's
    // capsule, the stats Server row) is otherwise unreachable on the only TV we own. It applies
    // to EVERY server, deliberately: it is a "what does a free server look like" switch, not a
    // per-server override. Checked here, once per fetch, rather than in `subscription()` — that
    // accessor is on per-frame paths and `devtrig::flag` is a filesystem stat.
    if plx_base::devtrig::flag("nopass") {
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log("pms: /tmp/plxnative-nopass — reporting subscription as No");
        sub = Subscription::No;
    }
    store(id, sub, &mc.version);
    // The version + subscription bit identify a RELEASE, not a household — safe to log, and safe
    // for the stats panel to photograph. The SLOT number names which server said it, because the
    // `machineIdentifier` that really identifies one is a permanent household fingerprint
    // (`servers::register` keeps it out of the log for the same reason), and the address is the
    // owner's. The token never appears: the URL is built and consumed inside `get_json`.
    plx_base::eventlog::log(&format!(
        "pms: server {i} version={} plexPass={}",
        if mc.version.is_empty() {
            "unknown"
        } else {
            &mc.version
        },
        match sub {
            Subscription::Yes => "true",
            Subscription::No => "false",
            Subscription::Unknown => "unknown",
        }
    ));
}

/// Publish one server's answer into its slot. Split from [`fetch_once`] so the SCOPING is
/// host-testable without a socket: above this line is a round trip, below it is per-slot state,
/// and the property that matters — one server's answer never reaching another's reader — lives
/// entirely below.
///
/// **This is an async LANDING, so it owes the present gate a poke.** It runs on a worker, seconds
/// after boot or after a share is registered, and it changes what a SETTLED screen says: the stats
/// panel's Server row, the detail hero's "hardware conversion needs \[PLEX PASS\]" note, and the
/// failure read-out's capsule all read it. Without the [`plx_machine::idle::invalidate`] call the new
/// fact arrives invisibly and the screen keeps stating the old one until the next keypress — bounded
/// only by the 2 s keepalive, which is insurance and not pacing (that module's rule: *a new async
/// landing that repaints must add a call there*). Unconditional rather than change-guarded: a
/// re-registration that stores the same answer costs one frame, and a guard here would be a second
/// place that has to agree with what the readers draw.
fn store(id: ServerId, sub: Subscription, version: &str) {
    if !id.is_set() {
        return;
    }
    with_table(|t| {
        let e = t.entry(id.raw()).or_default();
        e.sub = sub;
        e.version = version.to_owned();
    });
    plx_machine::idle::invalidate();
}

/// Test-only: publish one server's answer without a round trip.
///
/// [`store`] is this module's private seam, and the suites that need it are the ones grading its
/// READERS — `player::playing_subscription` and `screens::detail::hero::item_subscription`, both of which
/// answer "whose server is this item on" and neither of which can reach a real PMS. Callers must
/// hold `plx_base::testlock::serial()`: the per-server table is a crate global, and it deliberately
/// outlive `servers::reset_for_test` (they are keyed on the SLOT, not on the client), so a test
/// must seed every slot it reads rather than assume a boot state.
#[cfg(any(test, feature = "test-support"))]
pub fn store_for_test(id: ServerId, sub: Subscription, version: &str) {
    store(id, sub, version);
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
    use super::*;
    use crate::plex::models::Envelope;

    /// The tristate mapping is the module's contract: absence is UNKNOWN (an old server that
    /// never says must not read as free), zero is a real "no", anything else a real "yes" — and
    /// the state round trip through the table must not invent a state.
    #[test]
    fn the_wire_field_maps_to_the_tristate_without_inventing_knowledge() {
        assert_eq!(Subscription::from_wire(None), Subscription::Unknown);
        assert_eq!(Subscription::from_wire(Some(0)), Subscription::No);
        assert_eq!(Subscription::from_wire(Some(1)), Subscription::Yes);
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
        assert_eq!(
            Subscription::from_wire(e.media_container.my_plex_subscription),
            Subscription::Yes
        );

        let as_string =
            br#"{"MediaContainer":{"myPlexSubscription":"1","version":"1.42.0.9999-abc"}}"#;
        let e: Envelope = serde_json::from_slice(as_string).expect("string form");
        assert_eq!(
            Subscription::from_wire(e.media_container.my_plex_subscription),
            Subscription::Yes
        );

        let as_false =
            br#"{"MediaContainer":{"myPlexSubscription":false,"version":"1.43.3.10861-x"}}"#;
        let e: Envelope = serde_json::from_slice(as_false).expect("false form");
        assert_eq!(
            Subscription::from_wire(e.media_container.my_plex_subscription),
            Subscription::No
        );

        let absent = br#"{"MediaContainer":{"size":30,"version":"0.9.9"}}"#;
        let e: Envelope = serde_json::from_slice(absent).expect("absent form");
        assert_eq!(
            Subscription::from_wire(e.media_container.my_plex_subscription),
            Subscription::Unknown
        );
    }

    /// **The reason this module stopped being one global.** A Plex Pass claim is about ONE
    /// server's owner: with our own server and a friend's share both registered, an answer filed
    /// under A must be invisible to a reader asking about B, and the bare accessors must follow
    /// `current` rather than the last fetch that happened to land.
    ///
    /// Registry state is a crate global, so this holds `testlock::serial()` (the registry's own
    /// tests do too) and it registers through the client-id seam — the plain `register` fetches
    /// server info, which on the host would mean a worker thread dialling a fictional address.
    ///
    /// The guard empties the registry on the way OUT as well as in — `servers`' own `Fresh`
    /// discipline. This test used to reset only on entry, and the two servers it left behind read
    /// as a live two-server roster to any later test that derives one: `pms::sync_roster` kept a
    /// seeded source table alive on exactly that, and which test inherited the leak depended on
    /// parallel completion order — the worst shape a flake can take.
    #[test]
    fn one_servers_plex_pass_is_never_read_as_the_others() {
        use super::super::servers;
        struct Fresh(#[allow(dead_code)] plx_base::testlock::Serial);
        impl Drop for Fresh {
            fn drop(&mut self) {
                servers::reset_for_test();
            }
        }
        let _g = Fresh(plx_base::testlock::serial());
        servers::reset_for_test();
        let reg =
            |m: &str, host: &str| servers::register_with_client_id(m, host, 32400, "tok", "cid");
        let (a, b) = (reg("mach-A", "10.0.0.1"), reg("mach-B", "10.0.0.2"));
        // the per-server table outlives `reset_for_test` (it is keyed on the slot, not the client),
        // so start from the boot state explicitly rather than from what an earlier test left
        store(a, Subscription::Unknown, "");
        store(b, Subscription::Unknown, "");

        store(a, Subscription::Yes, "1.43.3.10861-cd85035e7");
        assert_eq!(subscription_of(a), Subscription::Yes);
        assert_eq!(version_of(a), "1.43.3.10861-cd85035e7");
        assert_eq!(
            subscription_of(b),
            Subscription::Unknown,
            "B has not answered — not 'free'"
        );
        assert_eq!(version_of(b), "", "and it has no build string either");

        store(b, Subscription::No, "1.32.0.6918-free");
        assert_eq!(
            subscription_of(a),
            Subscription::Yes,
            "B's answer left A's alone"
        );
        assert_eq!(version_of(a), "1.43.3.10861-cd85035e7");

        // the bare accessors mean "the current server", which is what every reader that has not
        // yet learned which server it is describing gets
        assert!(servers::set_current(a));
        assert_eq!(subscription(), Subscription::Yes);
        assert_eq!(version(), "1.43.3.10861-cd85035e7");
        assert!(servers::set_current(b));
        assert_eq!(
            subscription(),
            Subscription::No,
            "switching servers switches the claim"
        );
        assert_eq!(version(), "1.32.0.6918-free");

        // an id that names no server is "nothing known" — never slot 0's answer, never a panic,
        // and a write through one must not reach a real server's entry
        assert_eq!(subscription_of(ServerId::UNSET), Subscription::Unknown);
        assert_eq!(version_of(ServerId::UNSET), "");
        let unregistered = ServerId::from_raw(u16::MAX - 1);
        assert_eq!(subscription_of(unregistered), Subscription::Unknown);
        store(ServerId::UNSET, Subscription::Yes, "nowhere");
        store(unregistered, Subscription::Yes, "nowhere");
        assert_eq!(subscription_of(a), Subscription::Yes);
        assert_eq!(
            subscription_of(b),
            Subscription::No,
            "no write landed in a real slot"
        );

        assert!(servers::set_current(a));
        servers::revoke_for_profile_switch();
        assert_eq!(
            subscription_of(a),
            Subscription::Yes,
            "the current shell still names this server"
        );
        assert_eq!(
            subscription_of(b),
            Subscription::Unknown,
            "an inactive sparse slot exposes no old subscription"
        );
        assert_eq!(version_of(b), "", "nor its old build string");
    }

    /// A registration past the old sixteen-slot table must keep its own facts: the per-server
    /// state is keyed by slot number and grows, so slot 40 is as real as slot 0.
    #[test]
    fn a_server_past_the_old_sixteen_slot_table_keeps_its_own_facts() {
        use super::super::servers;
        struct Fresh(#[allow(dead_code)] plx_base::testlock::Serial);
        impl Drop for Fresh {
            fn drop(&mut self) {
                servers::reset_for_test();
            }
        }
        let _g = Fresh(plx_base::testlock::serial());
        servers::reset_for_test();
        let ids: Vec<ServerId> = (0..41)
            .map(|n| {
                servers::register_with_client_id(&format!("mach-{n}"), &format!("10.0.2.{n}"), 32400, "tok", "cid")
            })
            .collect();
        let far = ids[40];
        assert_eq!(far.raw(), 40);
        store(far, Subscription::Yes, "1.43.3.10861-far");
        assert_eq!(subscription_of(far), Subscription::Yes);
        assert_eq!(version_of(far), "1.43.3.10861-far");
        assert_eq!(subscription_of(ids[3]), Subscription::Unknown, "slot 3 did not answer");
        assert_eq!(version_of(ids[3]), "");
        assert!(servers::ids().any(|id| id == far), "the far server is in the roster the readers walk");
    }

    /// **This is an async LANDING, so it owes the frame gate a poke.** The answer arrives on a
    /// worker seconds after boot — later still for a share registered while the user is reading a
    /// page — by which time the screen that reads it has settled and stopped presenting: the stats
    /// panel's Server row, the detail hero's "hardware conversion needs [PLEX PASS]" note, and the
    /// failure read-out's capsule. Without the `plx_machine::idle::invalidate` inside [`store`] the new fact
    /// reaches the panel only on the next keypress (bounded by the 2 s keepalive, which is
    /// insurance and not pacing) — the "arrives invisibly" failure `plx_machine::idle::invalidate`'s
    /// call-site list exists to prevent.
    ///
    /// Takes `testlock::serial()`: the gate's flag is a crate global that `plx_machine::idle`'s own
    /// "a settled screen does not repaint" assertions read, so an unlocked test here would fail
    /// ANOTHER module's tests intermittently — `ui::xfade`'s test module states the same rule, and
    /// this borrows its isolation helper.
    #[test]
    fn a_landed_server_fact_asks_the_frame_gate_to_repaint() {
        let _g = plx_base::testlock::serial();
        // `frame_begin` forgets last frame's motion, `note_present` retires the 2 s keepalive, and
        // `should_present` TAKES-AND-CLEARS — so each call answers for exactly the frames since the
        // previous one, and nothing but the store below can be what answered.
        let asked = || {
            plx_machine::idle::set_enabled(true); // a test that disabled the gate leaves it off
            plx_machine::idle::frame_begin(1.0 / 60.0);
            plx_machine::idle::note_present(0);
            plx_machine::idle::should_present(0)
        };
        asked(); // drain whatever the previous test left on the shared flag
        assert!(
            !asked(),
            "the gate is quiet before the landing — or this proves nothing"
        );

        let id = ServerId::from_raw(0);
        store(id, Subscription::No, "1.32.0.6918-free");
        assert!(
            asked(),
            "a subscription that just landed must reach the screen stating it"
        );

        store(id, Subscription::Unknown, "");
        assert!(asked(), "…and so must the boot state being put back");

        // a write through an id that names no server publishes nothing, so it wakes nothing
        store(ServerId::UNSET, Subscription::Yes, "nowhere");
        assert!(!asked(), "a write that landed in no slot changed no screen");
    }
}
