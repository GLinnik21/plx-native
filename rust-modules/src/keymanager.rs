//! Device-backed protection for the persisted Plex session.
//!
//! TV 24+ documents the public `com.webos.service.keymanager3` service. The version number alone
//! is not a capability test, so we probe the running firmware and its LS2 policy. Older firmware's
//! archival `com.palm.keymanager` AES-CFB interface is deliberately not used: it provides no
//! authenticated-encryption primitive, and ciphertext integrity is part of the storage contract.
//!
//! This module deliberately uses LS2 as an unprivileged in-app client — the process's one client
//! in `webos::ls2` — and no root-only broker, filesystem or HAL symbol. A normal SAM-launched app
//! therefore follows the same unprivileged call path on development and retail sets; the retail
//! LS2 entitlement itself is capability-probed at runtime and denial selects the mode-0600
//! fallback.
//!
//! # Identity: WHO the key belongs to (issue #76)
//!
//! **LG's key manager keys ownership by the caller's LS2 identity** — the application id when the
//! hub supplies one, else the sender's service name (decompiled from the v1 `keymanager` on the
//! dev set, 2026-09-10). A client that registers ANONYMOUSLY is therefore a different owner every
//! launch, which is the working root cause of "sealed on Monday, cannot be opened on Tuesday" on
//! any set without `libAcbAPI` (webOS 5 and newer — issue #76's reporters are at platform releases
//! 5.6.2 through 11.2.0): the envelope is intact and the key is not this launch's to use.
//!
//! So since 2026-09-10 this module's connection asks for a stable identity when it can get one,
//! and falls back to the anonymous registration that always shipped when it cannot —
//! [`resolve_registration`] is the decision, taken once per process and logged as one
//! `keymanager: identity=<app_id|named|anonymous> (<reason>)` line, with
//! [`registered_with_app_id`] and [`registered_with_name`] carrying the same fact into every
//! storage report so the first report off a reporter's set settles which branch that firmware
//! takes.
//!
//! **THREE shapes, preferred in this order**, because LG's rule has two keys and this app can ask
//! for either:
//!
//!  1. [`Identity::AppId`] — `LSRegisterApplicationService(app_id, app_id)`. The hub is told an
//!     application id, which is the rule's FIRST key.
//!  2. [`Identity::Named`] — `LSRegister(app_id)`. No application id is declared, so the key
//!     manager falls to the rule's SECOND key, the sender's service name — equally stable across
//!     launches, and the shape a dev-mode role file's `allowedNames` actually lists.
//!  3. [`Identity::Anonymous`] — `LSRegister(NULL)`. A different owner every launch: the bug.
//!
//! **What the dev set actually answers, and the correction it forced.** This section used to say
//! the app-service form got `-1028 Attempted to register for a service name that already exists`
//! *because ACB holds it*. That reading was taken through the `gohome=probe` leg, which fires on a
//! root BACK press — **after** `player::acb_init`. Measured at BOOT on 2026-09-10, before
//! anything of ours had registered, the same set answers **`-1027` "Invalid permissions"** to
//! `LSRegisterApplicationService` for the app id AND for `NULL`, while plain `LSRegister(NULL)`
//! registers and completes an outbound `getForegroundAppInfo`. `-1027` for a shape that asks for
//! no name at all cannot be a name-taken verdict, and the app id was free at that instant
//! (`acb create=1` is 29 lines further down the same log). So on webOS 4.10 with the role file
//! appinstalld generates, the refusal tracks the **registration API**, not the name: that role
//! grants `LSRegister` and refuses `LSRegisterApplicationService` whatever it is handed.
//! `docs/measurements/ls2-identity-tv-2026-09-10.md` is the record.
//!
//! Both readings agree on the only thing the identity decision depends on — this app never
//! obtains the app-id *application-service* identity on that firmware — and the boot one is
//! stronger, because it removes ACB as the explanation. It is also what put shape 2 on this list:
//! the role file allows the app id as a NAME, and nothing had ever asked for it through the API
//! that role does grant. **Measured on the dev set 2026-09-10 (same doc, §10.2): the hub GRANTS
//! it** — `ls2probe: plain LSRegister name=appid: registered`, an outbound `getForegroundAppInfo`
//! completed under that name, and `acb create=1` still exactly once in the same log, the probe
//! having released the name before `player::acb_init`. So the shape is real, and on that firmware
//! it is granted at boot. **What no television has yet done is USE it**, and the two halves of why
//! are worth keeping apart: on webOS 4.10 the ACB gate below short-circuits before this module
//! asks (measured, same session, §10.1 — `keymanager: identity=anonymous` unchanged), and on
//! webOS 5.x and later, where `libAcbAPI` does not exist and the gate cannot fire, **nothing has
//! been measured at all**. So [`Identity::Named`] has sealed nothing anywhere, for one reason on
//! the set we have and for no reason we have evidence about on the sets issue #76 is really about.
//!
//! **ACB is still why webOS 4 and a set without `libAcbAPI` (webOS 5 and newer) differ, and the
//! gate is still ACB's.**
//! `libAcbAPI` is the video-plane binding on webOS 4, `player::acb_init` hands it the app id at
//! boot (`AcbAPI_initialize`), and that registration holds the app id as a bus name for the life
//! of the process — `ls-monitor -l` listed the app id and an anonymous client, both ours.
//! **webOS 5.0 deleted `libAcbAPI` outright** (`src/starfish.c`'s `vp_mode`, which is why the
//! exported-window path exists), so on a reporter's set nothing in the process holds that name and
//! the hub's answer is the only question.
//!
//! Hence the gate is `player::acb_holds_app_id()` and NOT the hub's reply — this is a DESIGN
//! order, since the dev set cannot actually test it either way: `plex::session::load` runs
//! BEFORE `acb_init`, so on webOS 4 the name may well be free when this code first asks, and a
//! yes there would be taken at ACB's expense — trading a session that has to be typed again for a
//! television that plays no picture. **The gate covers BOTH named shapes**, since
//! `LSRegister(app_id)` asks for the very bus name ACB is about to take: on webOS 4 this module
//! attempts neither and settles `anonymous` without asking the hub anything. The
//! `-1028`/`-1027` fallbacks stay anyway — `-1028` as the belt to that gate's braces, and `-1027`
//! for a hub that refuses a registration SHAPE outright, which is what this very set does to the
//! app-service form.
//!
//! **This is a PROBABLE cause under test, not an established one.** The ownership rule is read
//! off LG's own binary and the anonymous registration is a real per-launch owner, but nobody has
//! yet watched a set without `libAcbAPI` seal under `app_id` or `named` and reopen it after a
//! power cycle. The
//! identity this launch got is on the first line of the evidence (`keymanager: identity=…`) and in
//! every storage report's `registered_with_app_id`/`registered_with_name`, which is what a
//! reporter's first report settles.
//!
//! # The identity is PINNED to the envelope, not chosen per launch
//!
//! A freely chosen identity would be the instability this fix exists to remove: a bus name that is
//! free on one launch and held on the next would flip the key's owner, and a key sealed as one
//! owner is not openable as the other. So:
//!
//!  * [`Sealed::identity`] records **which identity sealed it** (`app_id`/`named`/`anonymous`, and
//!    absent means `anonymous` — the only shape older builds ever used). The probe file and the
//!    proven marker carry the same word.
//!  * [`open`] registers under **the identity the envelope records**, never the one this launch
//!    could get — `Client::new(Some(identity))`. **The ACB gate applies to that ask too**
//!    ([`acb_withholds_required_identity`], 2026-09-11): an envelope recorded as `app_id`/`named`
//!    is not opened at all on a set whose `libAcbAPI` holds the app id, because asking for that
//!    bus name is the thing the gate exists to prevent whoever wants it. Until then the open side
//!    went straight to the hub, and the hub GRANTING the name was the bad outcome.
//!  * An identity that cannot be obtained on this launch is [`ClientError::IdentityUnavailable`]
//!    → `StorageStage::IdentityUnavailable`, and it is **TRANSIENT**: the envelope is kept, the
//!    cross-launch refused marker is NOT written, and the report says which of the two it was.
//!    Nothing was learned about the key — only about a name.
//!  * `plex::session`'s proven marker is proof for **one identity**: an install proven as `app_id`
//!    has proven nothing about anonymous, so a launch whose identity differs stays on the 0600
//!    file and plants a fresh probe under the identity it actually has.
//!
//! **The connection model is unchanged: one registration per logical keymanager operation**
//! (`modern_crypt` holds it across `begin` → `finish`), NOT one held for the process. Nothing
//! about the identity fix needs a long-lived registration — ownership is the NAME, which each
//! connection asks for and releases — and holding the app-id name for the process would be the
//! one shape that could collide with a future in-process component wanting it, exactly as ACB
//! does today. `/tmp/plxnative-ls2identity` (`webos::ls2_identity_probe_if_armed`) is how a set
//! that has no keymanager3 at all can still answer which identity its hub grants.

use crate::telemetry::storage::StorageStage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

/// Log `keymanager: FAKE service armed mode=<mode>` at boot when `plxnative-keymanager` is armed
/// — see `fake::log_if_armed`. Called from `plex_run` before anything could touch `seal`/`open`,
/// so no capture from an armed run can be mistaken for a real service.
///
/// Unconditionally calls [`crate::dev::keymanager_fake_mode`] (never `None` at compile time in a
/// `--no-default-features` build, but still worth calling) rather than being itself gated on the
/// `devtriggers` feature — the same shape every other dev-trigger reader here uses
/// (`player::threads::load_thread` calls `dev::holdload_delay_ms()` unconditionally, `player::
/// engine` calls `dev::playurl()` the same way) so a release build's stub stays reachable instead
/// of tripping `-D dead-code`.
pub(crate) fn boot_log_fake_if_armed() {
    let armed = crate::dev::keymanager_fake_mode().is_some();
    #[cfg(feature = "devtriggers")]
    if armed {
        fake::log_if_armed();
    }
    #[cfg(not(feature = "devtriggers"))]
    let _ = armed;
}

const KEY_NAME: &str = "plxnative.session.v1";
const UNKNOWN: u8 = 0;
const MODERN: u8 = 1;
const UNAVAILABLE: u8 = 3;
static SELECTED: AtomicU8 = AtomicU8::new(UNKNOWN);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Backend {
    Keymanager3,
    PalmKeymanager,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Sealed {
    pub backend: Backend,
    pub key: String,
    pub iv: String,
    pub data: String,
    /// **WHICH LS2 IDENTITY SEALED THIS** — the owner the key belongs to, recorded so [`open`]
    /// asks the hub for the same one instead of for whatever this launch could get (the module
    /// doc's "Identity" section). Absent in every envelope written before 2026-09-10, which
    /// `#[serde(default)]` reads as [`Identity::Anonymous`] — correct by construction, since
    /// anonymous is the only shape those builds ever registered with.
    #[serde(default)]
    pub identity: Identity,
}

/// The stage and (when the failure reached a service reply) numeric `errorCode` of the most
/// recent seal/open refusal or missing-field reply THIS PROCESS has seen — issue #76's storage
/// telemetry reads this after a `seal`/`open` failure to attach a real stage and code to a handled
/// report, rather than re-deriving one from a log line. `None` on a process that has never seen a
/// service refusal (including a process with no key manager at all, since that path never reaches
/// a service call). Cleared on [`remove`]: a stale refusal from a previous account's key must never
/// be attached to a report about a fresh one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastRefusal {
    pub stage: StorageStage,
    pub error_code: Option<i64>,
}

static LAST_REFUSAL: Mutex<Option<LastRefusal>> = Mutex::new(None);

pub(crate) fn last_refusal() -> Option<LastRefusal> {
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_last_refusal(stage: StorageStage, error_code: Option<i64>) {
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(LastRefusal { stage, error_code });
}

/// What `generateKey` told THIS process about the device key it asked for — issue #76's identity
/// decider. LG's key manager keys ownership by the caller's bus identity, and this app registers
/// anonymously, so on an affected set every launch is a new "owner" that cannot open the previous
/// launch's key. The cheap probe: `-10002` ("key already exists") means the key a PRIOR launch
/// created is still the one this launch's registration is recognized as owning — the shared-owner
/// hypothesis holds; a bare success means a NEW key was minted this call, which — on a launch that
/// follows a launch that already sealed something — is the confirmed shape: the owner changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeyOutcome {
    /// `generateKey` answered `-10002` — a key by this name already existed.
    Existed,
    /// `generateKey` succeeded — a new key was minted this call.
    Created,
}

impl KeyOutcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Existed => "existed",
            Self::Created => "created",
        }
    }
}

const KEY_OUTCOME_UNKNOWN: u8 = 0;
const KEY_OUTCOME_EXISTED: u8 = 1;
const KEY_OUTCOME_CREATED: u8 = 2;
static LAST_KEY_OUTCOME: AtomicU8 = AtomicU8::new(KEY_OUTCOME_UNKNOWN);

/// The most recent `generateKey` outcome THIS PROCESS has seen, or `None` when this process has
/// never called it — including a process that has only ever opened/read (`open`/`open_checked`
/// never call `generateKey`) or one with no key manager at all. `telemetry::storage`'s live
/// (seal-time) reports read this right after `seal`; a CROSS-LAUNCH caller
/// (`plex::session`'s probe/refused-marker plumbing) must never read the live value here for a
/// report about an EARLIER launch's seal — it persists the outcome it observed at seal time
/// instead, exactly because this is a process-global that says nothing about a prior process.
pub(crate) fn last_key_outcome() -> Option<KeyOutcome> {
    match LAST_KEY_OUTCOME.load(Ordering::Relaxed) {
        KEY_OUTCOME_EXISTED => Some(KeyOutcome::Existed),
        KEY_OUTCOME_CREATED => Some(KeyOutcome::Created),
        _ => None,
    }
}

/// Record `generateKey`'s outcome for [`last_key_outcome`] and log it — ONE line per process, not
/// per call: a healthy install reseals on every profile switch, and the fact worth a line is which
/// branch this LAUNCH took, not that it keeps taking it.
fn note_key_outcome(outcome: KeyOutcome) {
    LAST_KEY_OUTCOME.store(
        match outcome {
            KeyOutcome::Existed => KEY_OUTCOME_EXISTED,
            KeyOutcome::Created => KEY_OUTCOME_CREATED,
        },
        Ordering::Relaxed,
    );
    log_once(
        "key_outcome".to_string(),
        format!("keymanager: generateKey -> {}", outcome.code()),
    );
}

/// Which LS2 identity this process's keymanager connections registered under — the choice the
/// module doc's "Identity" section explains, taken once per process by [`resolve_registration`].
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Identity {
    /// `LSRegisterApplicationService(app_id, app_id)` — the bus name is this install's app id AND
    /// the hub is told an application id, so keymanager3 sees the same owner on every launch
    /// through the FIRST key of LG's ownership rule.
    AppId,
    /// `LSRegister(app_id)` — a plain registration under a name, with no application id declared.
    /// The hub therefore gives keymanager3 the SECOND key of that rule, the sender's service
    /// name, which is equally stable across launches. Added 2026-09-10, after the dev set
    /// answered `-1027` to the application-service form at boot for both names — a verdict on
    /// that API rather than on the name, leaving the shape the role file's `allowedNames`
    /// actually lists never asked. `webos::ls2::register_named`.
    Named,
    /// `LSRegister(NULL)` — the hub mints the name. What every launch did before 2026-09-10, and
    /// still what a set whose ACB already holds the app id gets.
    #[default]
    Anonymous,
}

impl Identity {
    /// Every variant, for the same reason `telemetry::storage`'s `StorageStage::ALL` exists: the
    /// vocabulary test that grades these codes has to be driven off the live enum rather than off
    /// a hand-written literal beside it. `Named` was added on 2026-09-10 and a literal there would
    /// have gone stale in that commit.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[Self::AppId, Self::Named, Self::Anonymous];

    /// Adding a variant without adding it to [`Identity::ALL`] fails to compile here.
    #[cfg(test)]
    #[allow(dead_code)]
    fn _assert_all_variants_covered(v: Self) {
        match v {
            Self::AppId | Self::Named | Self::Anonymous => {}
        }
    }

    /// The wire word, and the SAME closed vocabulary on every surface that records one — the
    /// envelope, the probe, the proven marker. Two spellings of one fact is how a later launch
    /// ends up unable to decide which owner sealed a file.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::AppId => "app_id",
            Self::Named => "named",
            Self::Anonymous => "anonymous",
        }
    }
}

const IDENTITY_UNSET: u8 = 0;
const IDENTITY_APP_ID: u8 = 1;
const IDENTITY_ANONYMOUS: u8 = 2;
const IDENTITY_NAMED: u8 = 3;
/// Latched by [`resolve_registration`]. `UNSET` until the first keymanager registration of the
/// process — including on a set with no keymanager3 at all, where nothing here ever registers.
static IDENTITY: AtomicU8 = AtomicU8::new(IDENTITY_UNSET);

/// The latched [`Identity`], or `None` on a process that has never registered for a keymanager
/// call (no seal, no open, or no key manager on this firmware at all).
pub(crate) fn identity() -> Option<Identity> {
    match IDENTITY.load(Ordering::Relaxed) {
        IDENTITY_APP_ID => Some(Identity::AppId),
        IDENTITY_NAMED => Some(Identity::Named),
        IDENTITY_ANONYMOUS => Some(Identity::Anonymous),
        _ => None,
    }
}

/// Issue #76's "owner_hint": whether the LS2 registration this module seals through identifies
/// itself with an application id — the **application-service** form specifically, which is what
/// this bool has always meant and keeps meaning.
///
/// A PROBED fact since 2026-09-10, where it used to be the closed constant `false` — see the
/// module doc. `false` on a process that never registered at all, and `false` for the plain named
/// registration, which is a different owner key entirely: [`registered_with_name`] is that one.
/// The two are derived from the same one-way latch, so they can never both be true.
pub(crate) fn registered_with_app_id() -> bool {
    identity() == Some(Identity::AppId)
}

/// The sibling probed fact: whether this process's keymanager registration took the app id as a
/// plain BUS NAME (`LSRegister(app_id)`, [`Identity::Named`]) rather than as an application
/// service.
///
/// It is a second field on the storage report rather than a widening of
/// [`registered_with_app_id`] because the two are different keys of LG's ownership rule and a
/// reporter's set may grant one and refuse the other — which is exactly the thing issue #76 needs
/// told apart. `false` on a process that never registered, on an anonymous one, and on one that
/// got the application-service form.
pub(crate) fn registered_with_name() -> bool {
    identity() == Some(Identity::Named)
}

/// Decide, ONCE for the process, which LS2 identity a keymanager connection registers under, and
/// hand back the registration that decision produced **together with the identity it actually
/// used**.
///
/// The identity is returned rather than left for the caller to read back off the latch, and that
/// is load-bearing (review finding, 2026-09-10): the latch is a process global any other
/// connection may DOWNGRADE at any moment, so a caller that reads it after the fact can label an
/// envelope with an owner that is not the one its own key belongs to — see
/// [`Client::identity`](platform::Client::identity) and `modern_crypt`.
///
/// The seam is generic over the registration and its error so it is testable on the host: the
/// device passes `webos::ls2`'s two register functions, a test passes closures that count their
/// calls and return whatever the hub is being made to say. `webos::ls2` does not exist in a host
/// build at all, so this is the only part of the decision a `make check` can see — and it is the
/// part with the branches.
///
/// The rules, in order:
///
///  1. **`acb_holds_app_id` — do not ask, in EITHER named shape.** This is a DESIGN order, not a
///     reading off the hub's reply: on a firmware that has `libAcbAPI`, `player::acb_init` hands
///     the app id to `AcbAPI_initialize` at boot and that registration holds the name for the life
///     of the process, so taking the name first would not merely be refused later, it would make
///     ACB's own registration the one that fails — i.e. trade a session that has to be typed again
///     for a television that plays no picture. `LSRegister(app_id)` asks for the *same bus name*
///     as the application-service form, so the gate covers both: on a webOS 4 set this function
///     attempts neither and settles anonymous. `session::load` runs BEFORE `acb_init`, so at the
///     moment this code first runs the name may well still be free, and "the hub said yes" is
///     exactly the wrong evidence to act on there. The dev set cannot actually test that ordering
///     either way — measured BEFORE `acb_init`
///     (`docs/measurements/ls2-identity-tv-2026-09-10.md` §2.1) the application-service form is
///     refused `-1027` with the app id still free, i.e. a verdict on the SHAPE, so neither that
///     `-1027` nor the `-1028` measured after `acb_init` proves what asking first would have cost
///     ACB here. The gate stays the design order regardless.
///  2. Otherwise ask for the **application-service** form first. It is the strongest of the three:
///     the hub is told an application id, which is the FIRST key of LG's ownership rule.
///  3. On a refusal, ask for the **plain named** form, `LSRegister(app_id)`. This step exists
///     because of a measurement: on the dev set (webOS 4.10, Developer Mode role), at BOOT and
///     before ACB held anything, `LSRegisterApplicationService` was refused **`-1027` "Invalid
///     permissions"** for `app_id` *and* for `NULL`, while plain `LSRegister(NULL)` registered and
///     completed an outbound call — so the refusal tracks that API rather than the name, and the
///     role file's `allowedNames` (which lists both `""` and the app id) had never been asked for
///     the app id through the API it does grant.
///     `docs/measurements/ls2-identity-tv-2026-09-10.md` is the record — and §10.2 (run 4, the
///     same set, 2026-09-10) is the answer that shape was waiting for: **the hub GRANTS it.**
///     `ls2probe: plain LSRegister name=appid: registered`, an outbound `getForegroundAppInfo`
///     completed under that name, and `acb create=1` still exactly once in the same log. What that
///     does NOT license is this module asking for it there: the ACB gate (rule 1) short-circuits
///     first on webOS 4, so that firmware still settles `anonymous` — and no 5.x-or-later set has
///     been measured at all, which is where the shape would first be used.
///  4. A refusal of both falls back to the anonymous registration, unchanged from what every
///     launch did before — `-1028` (somebody holds the name), `-1027` (refused for this
///     executable's role: on the application-service form that is a verdict on the registration
///     SHAPE, per rule 3, while on the named form there really is a name being asked for and it is
///     a verdict on that) and anything else are all the same fallback and different log reasons,
///     and the anonymous line carries BOTH refusals so a reporter's log says which of the two
///     shapes the hub was answering about.
///
/// The latch is one-way and ordered: once the process is anonymous it stays anonymous, and once it
/// is `named` it never re-asks for the application-service form (that answer does not change
/// within a process, and re-asking would cost a refused registration per connection). A first
/// attempt that was GRANTED keeps asking for the same shape on every later connection (the name is
/// released when each connection drops), and a later refusal downgrades the process, logs why, and
/// moves [`registered_with_app_id`]/[`registered_with_name`] with it — a report must never claim an
/// owner this process no longer has.
///
/// Two threads reaching the unset latch together both attempt, and that is deliberate rather than
/// locked: the worst case is one extra registration and one deduplicated log line, where a mutex
/// on this path would sit under the session and auth locks a seal already holds. If the second
/// attempt is refused because the FIRST is still holding the name (`-1028`), the fallback is the
/// same anonymous registration it would have taken anyway.
// Compiled where a real LS2 bus exists — the television — and in every test build, which is
// where the decision is graded. A `hostsim` binary has no bus at all: its `platform::Client`
// never registers, so this would be dead code there and `-D warnings` says so.
#[cfg(any(all(not(feature = "hostsim"), not(test)), test))]
pub(crate) fn resolve_registration<R, E>(
    acb_holds_app_id: bool,
    app_service: impl FnOnce() -> Result<R, String>,
    named: impl FnOnce() -> Result<R, String>,
    anonymous: impl FnOnce() -> Result<R, E>,
) -> Result<(R, Identity), E> {
    let latched = IDENTITY.load(Ordering::Relaxed);
    if latched == IDENTITY_ANONYMOUS {
        return anonymous().map(|r| (r, Identity::Anonymous));
    }
    if latched == IDENTITY_UNSET && acb_holds_app_id {
        settle(
            Identity::Anonymous,
            "identity",
            "libAcbAPI holds the app id on this firmware",
        );
        return anonymous().map(|r| (r, Identity::Anonymous));
    }
    // A refusal AFTER a name was granted once is a different fact from a refusal on the first
    // attempt, and the report fields move either way — so it gets its own line rather than being
    // swallowed by the first line's dedup key.
    let key = if latched == IDENTITY_UNSET {
        "identity"
    } else {
        "identity-downgrade"
    };
    // Both refusals, in the order they were collected, so the anonymous line says which shape the
    // hub was answering about rather than leaving a reader to guess from one code.
    let mut refusals: Vec<String> = Vec::new();
    // A process already latched to `named` has had its answer about the application-service form;
    // asking again would buy a refused registration per connection and nothing else.
    if latched != IDENTITY_NAMED {
        match app_service() {
            Ok(registration) => {
                if latched == IDENTITY_UNSET {
                    settle(Identity::AppId, "identity", "granted by the hub");
                }
                return Ok((registration, Identity::AppId));
            }
            Err(reason) => refusals.push(format!("app-service: {reason}")),
        }
    }
    match named() {
        Ok(registration) => {
            if latched != IDENTITY_NAMED {
                settle(
                    Identity::Named,
                    key,
                    &format!(
                        "the hub grants the app id as a plain bus name ({})",
                        refusals.join("; ")
                    ),
                );
            }
            Ok((registration, Identity::Named))
        }
        Err(reason) => {
            refusals.push(format!("named: {reason}"));
            settle(Identity::Anonymous, key, &refusals.join("; "));
            anonymous().map(|r| (r, Identity::Anonymous))
        }
    }
}

/// **The ACB gate, asked on the OPEN side** — where the identity is not this launch's to choose
/// but the envelope's to demand. `true` when `required` is a named shape on a set whose
/// `libAcbAPI` holds (or is about to hold) the app id, i.e. when [`platform::Client::new`] must
/// answer [`ClientError::IdentityUnavailable`] without asking the hub anything.
///
/// It is the same DESIGN order [`resolve_registration`] applies to the seal side, and it was
/// missing here (review finding, 2026-09-11): `Client::new(Some(AppId|Named))` went straight to
/// the registration, so an envelope recorded under a name — what a set that once granted one
/// leaves behind, and what a `--no-default-features` build reads on the next launch — sent this
/// process to the hub for the very bus name `player::acb_init` is about to take. The hub GRANTING
/// it is the bad outcome, not the good one: winning that race trades a sign-in that must be typed
/// again for a television that shows no picture. Refusing costs nothing that is not already lost —
/// `IdentityUnavailable` is transient by construction, so the envelope is kept, no cross-launch
/// refused marker is written, and a set whose ACB is gone (webOS 5.0 deleted `libAcbAPI`) opens
/// the same file normally.
// Compiled where a real LS2 bus exists — the television — and in every test build, which is
// where the decision is graded. A `hostsim` binary has no bus at all: its `platform::Client`
// never registers, so this would be dead code there and `-D warnings` says so.
#[cfg(any(all(not(feature = "hostsim"), not(test)), test))]
fn acb_withholds_required_identity(required: Identity, acb_holds_app_id: bool) -> bool {
    acb_holds_app_id && matches!(required, Identity::AppId | Identity::Named)
}

/// Why a [`platform::Client`] could not be built. TWO outcomes, because they are two different
/// facts about the same envelope and only one of them is a verdict on it.
///
///  * [`ClientError::Setup`] — no registration of any kind was possible (no bus, no glib context,
///    a hub that refused even the anonymous shape). What every failure here used to be.
///  * [`ClientError::IdentityUnavailable`] — the connection was for an envelope sealed under a
///    SPECIFIC identity (the module doc's "Identity" section) and that identity could not be
///    obtained on this launch. **Transient by construction**: the envelope is intact, its key is
///    intact, and the only thing missing is the name — which a later launch may well get, so
///    nothing about this may arm the cross-launch refused marker. `plex::session` grades it as
///    `StorageStage::IdentityUnavailable` and keeps the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientError {
    Setup,
    // A `hostsim` binary has no bus, so nothing there can fail to obtain a name: the variant is
    // constructed by the device client and by the scripted one a test arms, and by neither in
    // between. Kept whole rather than `#[cfg]`'d away so `note_client_error`'s match — the thing
    // that decides a marker is not written — reads the same in every configuration.
    #[cfg_attr(all(feature = "hostsim", not(test)), allow(dead_code))]
    IdentityUnavailable,
}

/// Resolve this process's [`Identity`] WITHOUT calling a service — used by `plex::session` to ask
/// "is the proven marker on disk proof about the identity this launch actually seals under".
///
/// Costs one registration and its unregistration on the first call of the process and nothing
/// after (the latch), which is why the caller only reaches it once a proven marker exists — an
/// install that is going to seal is one that registers anyway. `None` when no registration at all
/// is possible: a host build, or a set where even the anonymous shape fails.
pub(crate) fn ensure_identity() -> Option<Identity> {
    if let Some(id) = identity() {
        return Some(id);
    }
    // The CLIENT's own identity, not the latch re-read afterwards, for `resolve_registration`'s
    // reason: between building this connection and asking the latch, another one may have moved it.
    Some(platform::Client::new(None).ok()?.identity())
}

/// The `(<reason>)` half of the `keymanager: identity=anonymous (…)` line, for a hub that refused
/// the app-id registration. Split out of the device-only call site so the two codes that MEAN
/// something are graded where a host test can see it.
///
///  * **`-1028`** — the name is taken. On a webOS 4 set that is our own ACB, and this branch is
///    the belt to `acb_holds_app_id`'s braces: it is what a set reaches if ACB ever claims the id
///    before this code asks, or if a second connection here races the first.
///  * **`-1027`** — the hub refused this registration SHAPE for this executable's role, not
///    necessarily a missing permissions entry for the name: the dev set's own role file DOES list
///    the app id with `outbound: ["*"]` and still answers `-1027` for
///    `LSRegisterApplicationService`, for either name, before ACB has taken anything
///    (`docs/measurements/ls2-identity-tv-2026-09-10.md` §2.1) — only plain `LSRegister` is
///    granted on that firmware. A set whose role omits the name entirely would land here too; the
///    sign-in still works either way.
///  * anything else — the hub's own words, carried whole. A stage that never reached the hub at
///    all (no glib context, an app id with an interior NUL) arrives here too, with `code` 0.
// Compiled where a real LS2 bus exists — the television — and in every test build, which is
// where the decision is graded. A `hostsim` binary has no bus at all: its `platform::Client`
// never registers, so this would be dead code there and `-D warnings` says so.
#[cfg(any(all(not(feature = "hostsim"), not(test)), test))]
fn app_service_refusal_reason(code: i32, detail: &str) -> String {
    match code {
        -1028 => format!("the app id is already registered on this bus ({code})"),
        -1027 => format!("the hub refused this registration shape for this executable's role ({code})"),
        _ => format!("app-service registration refused — {detail}"),
    }
}

/// The same grading for the PLAIN NAMED shape, `LSRegister(app_id)`. It is a separate function
/// rather than a shared one because the two codes mean different things here:
///
///  * **`-1028`** — the name is taken, and for the named shape that is the ONLY reading. It is the
///    belt to `acb_holds_app_id`'s braces on a set where ACB claimed the id before this ran, or a
///    second connection racing the first.
///  * **`-1027`** — the hub found the app id absent from this executable's role-file
///    `allowedNames`. That is a stronger statement than the app-service form's `-1027`, which the
///    dev set gives even for `name=NULL` (i.e. about the API, not the name); here there IS a name
///    being asked for, so a refusal really is about it.
///  * anything else — the hub's own words, carried whole, including a stage that never reached the
///    hub (no glib context, an app id with an interior NUL) at `code` 0.
// Compiled where a real LS2 bus exists — the television — and in every test build, which is
// where the decision is graded. A `hostsim` binary has no bus at all: its `platform::Client`
// never registers, so this would be dead code there and `-D warnings` says so.
#[cfg(any(all(not(feature = "hostsim"), not(test)), test))]
fn named_refusal_reason(code: i32, detail: &str) -> String {
    match code {
        -1028 => format!("the app id is already registered on this bus ({code})"),
        -1027 => {
            format!("this executable's role file does not allow the app id as a bus name ({code})")
        }
        _ => format!("plain named registration refused — {detail}"),
    }
}

/// Latch an [`Identity`] and say so, once per `key` **and per identity settled**.
///
/// The dedup key is `<key>:<identity>`, not `key` alone (review finding, 2026-09-11). A process
/// can settle more than one verdict — `app_id` → `named` → `anonymous` is one walk a hub can force
/// by refusing one shape at a time — and every downgrade shares the caller's one
/// `identity-downgrade` key, so the SECOND one was swallowed by the first's. That is the step that
/// costs the install its stable owner, and the two report bools
/// ([`registered_with_app_id`]/[`registered_with_name`]) move with it, so a reporter's log going
/// quiet there is exactly the line issue #76 needs.
// Compiled where a real LS2 bus exists — the television — and in every test build, which is
// where the decision is graded. A `hostsim` binary has no bus at all: its `platform::Client`
// never registers, so this would be dead code there and `-D warnings` says so.
#[cfg(any(all(not(feature = "hostsim"), not(test)), test))]
fn settle(identity: Identity, key: &str, reason: &str) {
    IDENTITY.store(
        match identity {
            Identity::AppId => IDENTITY_APP_ID,
            Identity::Named => IDENTITY_NAMED,
            Identity::Anonymous => IDENTITY_ANONYMOUS,
        },
        Ordering::Relaxed,
    );
    // The same closed vocabulary every other surface records — `Identity::code`, not a second
    // spelling of it.
    let name = identity.code();
    log_once(
        format!("{key}:{name}"),
        format!("keymanager: identity={name} ({reason})"),
    );
}

/// Map a `log_refusal`/`log_missing_field` call site's own `method` label to the telemetry stage
/// vocabulary. `None` for a method this module does not (yet) classify — today that is nothing,
/// since every caller passes one of these five labels.
fn stage_for_method(method: &str) -> Option<StorageStage> {
    match method {
        "generateKey" => Some(StorageStage::GenerateKey),
        "begin(encrypt)" => Some(StorageStage::BeginEncrypt),
        "begin(decrypt)" => Some(StorageStage::BeginDecrypt),
        "finish(encrypt)" => Some(StorageStage::FinishEncrypt),
        "finish(decrypt)" => Some(StorageStage::FinishDecrypt),
        _ => None,
    }
}

/// Record a call that got NO reply to classify: the client is dead (its budget ran out —
/// `NoReply`) or the call never got that far (a failed registration, or an LS2 setup failure —
/// `Unreachable`). Published to the global exactly like a refusal, and to `local` for the caller
/// that decides something on it (`open_checked`'s marker gate). Issue #76, second review: the
/// first review treated these as "no evidence", and the trace under a stalled service showed the
/// cost — no marker, so every launch re-pays the budget, and no report, so nothing ever says so.
fn note_unanswered(client: Option<&platform::Client>, local: &mut Option<LastRefusal>) {
    let stage = if client.is_some_and(platform::Client::is_dead) {
        StorageStage::NoReply
    } else {
        StorageStage::Unreachable
    };
    note_stage(stage, local);
}

/// [`note_unanswered`]'s tail, for a call that already knows its stage — the identity a sealed
/// envelope names being unobtainable on this launch ([`ClientError::IdentityUnavailable`]) is the
/// one that is NOT a verdict on the envelope, so it must reach `local` (the caller's own,
/// race-free copy) exactly like a refusal does rather than being flattened into `Unreachable`.
fn note_stage(stage: StorageStage, local: &mut Option<LastRefusal>) {
    set_last_refusal(stage, None);
    *local = Some(LastRefusal {
        stage,
        error_code: None,
    });
}

/// Grade a [`platform::Client`] that could not be built. The identity case is the one that must
/// NOT read as `Unreachable`: a caller that cannot tell them apart writes the cross-launch refused
/// marker for a name it could simply ask for again next launch.
fn note_client_error(e: ClientError, local: &mut Option<LastRefusal>) {
    match e {
        ClientError::Setup => note_unanswered(None, local),
        ClientError::IdentityUnavailable => note_stage(StorageStage::IdentityUnavailable, local),
    }
}

pub(crate) fn seal(plain: &[u8]) -> Option<Sealed> {
    match SELECTED.load(Ordering::Relaxed) {
        // Trust the fast path rather than re-verifying every save: the round trip already ran
        // once, at promotion below, and a `seal` that pays a second LS2 registration plus two
        // more budgeted calls on every credential write is a cost this path (the SDL main thread,
        // under the auth and session locks) cannot absorb for free (Codex review 2026-09-04;
        // issue #76 review). It is NOT what catches a backend whose key is unusable from a
        // DIFFERENT launch or registration than the one that sealed it — `plex::session`'s
        // `LOCKED_STATE` does, from the read side, which is the only side that can see a launch
        // boundary at all.
        MODERN => {
            if let Some(sealed) = modern_crypt(plain, None, None, &mut None) {
                return Some(sealed);
            }
            SELECTED.store(UNKNOWN, Ordering::Relaxed);
        }
        UNAVAILABLE => return None,
        _ => {}
    }

    if modern_key_ready() {
        if let Some(sealed) = modern_crypt(plain, None, None, &mut None) {
            if round_trips(&sealed, plain) {
                SELECTED.store(MODERN, Ordering::Relaxed);
                log("session protection: keymanager3");
                return Some(sealed);
            }
            SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
            return None;
        }
    }
    SELECTED.store(UNAVAILABLE, Ordering::Relaxed);
    log("session protection: no usable key manager; using the 0600 file fallback");
    None
}

/// Prove a just-sealed envelope actually opens before `seal` hands it back to be persisted.
/// Mirrors what [`open`] will do later — same call shape, new registration — because a backend
/// that answers `encrypt` but not `decrypt` (or answers with a shape [`open`] cannot parse) is
/// exactly the failure this exists to catch before it reaches disk.
fn round_trips(sealed: &Sealed, plain: &[u8]) -> bool {
    match open(sealed) {
        Some(bytes) if bytes == plain => return true,
        // `open` returned bytes, but not the ones this call just sealed — a genuine mismatch, as
        // opposed to a service refusal `open` (via `log_refusal`/`log_missing_field`) has already
        // recorded a more specific stage and code for.
        Some(_) => set_last_refusal(StorageStage::RoundtripMismatch, None),
        None => {}
    }
    log(
        "session protection: keymanager3 sealed but could not open its own envelope — using the 0600 file fallback",
    );
    false
}

pub(crate) fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    open_checked(sealed).0
}

/// [`open`], plus — alongside the result — whatever refusal or missing-field reply THIS SPECIFIC
/// call saw, straight from the call itself rather than re-read from the [`LAST_REFUSAL`] global
/// afterward.
///
/// **Issue #76 review: `LAST_REFUSAL` is a process-global**, written by `seal`, `open` and
/// `round_trips` from however many threads call them, and read long after the fact by
/// `save_locked`'s seal-failure report. A caller that needs to know "did THIS attempt specifically
/// see a refusal" — `plex::session::read_locked`'s cross-launch marker gate, which must never
/// treat a bare timeout/registration failure (no service reached at all) as the proven refusal it
/// requires — cannot get that answer safely from the shared global: another thread's unrelated
/// `open`/`seal` call can write over it between this call returning and the global being read
/// (measured: an early version of this fix cleared `LAST_REFUSAL` at `open`'s own entry to scope
/// it, and that turned every `open` call anywhere in the process into a writer of the shared
/// global, which then intermittently raced a `testlock::serial()`-held `keymanager.rs` unit test
/// that never touches this file at all — six-of-six clean once the entry clear was replaced by
/// this call-local return value instead). This function is therefore the one to reach for when the
/// answer decides something (a marker write); the bare [`open`]/[`last_refusal`] pair remains for
/// callers that only want the STANDING fact (a report's error-code enrichment, a preview).
pub(crate) fn open_checked(sealed: &Sealed) -> (Option<Vec<u8>>, Option<LastRefusal>) {
    if sealed.key != KEY_NAME {
        return (None, None);
    }
    let mut local = None;
    let sealed_result = match sealed.backend {
        // **The identity the ENVELOPE records, never the one this launch could get.** A key
        // sealed as `app_id` is not openable by an anonymous registration and the reverse, so
        // asking for anything else here would decrypt nothing and report the wrong reason for it.
        Backend::Keymanager3 => modern_crypt(
            sealed.data.as_bytes(),
            Some(&sealed.iv),
            Some(sealed.identity),
            &mut local,
        ),
        // Kept only so an interim/pre-release envelope deserializes as locked instead of being
        // mistaken for plaintext. AES-CFB does not authenticate the file, so never open it — and,
        // issue #76 review: record that fact for the cross-launch caller (`plex::session`'s probe
        // check among them) instead of handing back a bare `(None, None)` that reads as "no
        // evidence at all".
        Backend::PalmKeymanager => {
            local = Some(LastRefusal {
                stage: crate::telemetry::storage::StorageStage::EnvelopeLocked,
                error_code: None,
            });
            None
        }
    };
    // A `finish(decrypt)` that itself succeeded can still hand back an `output` this build cannot
    // base64-decode — `modern_crypt` returns `Some` (it never inspects the payload), so without
    // this the caller sees `(None, None)`, indistinguishable from "no evidence was ever gathered".
    let plain = match sealed_result.map(|s| b64::decode(&s.data)) {
        Some(Some(bytes)) => Some(bytes),
        Some(None) => {
            if local.is_none() {
                local = Some(LastRefusal {
                    stage: crate::telemetry::storage::StorageStage::FinishDecrypt,
                    error_code: None,
                });
            }
            None
        }
        None => None,
    };
    (plain, local)
}

pub(crate) fn remove(backend: &Backend, key: &str) {
    if key != KEY_NAME {
        return;
    }
    let _ = match backend {
        Backend::Keymanager3 => call(
            "luna://com.webos.service.keymanager3/removeKey",
            &json!({"name": key}),
        ),
        Backend::PalmKeymanager => call(
            "luna://com.palm.keymanager/remove",
            &json!({"keyname": key}),
        ),
    };
    // `clear()` deletes the key and the file in one sign-out. A later sign-in in the same process
    // must run key creation again rather than trusting the now-stale backend cache.
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    // A stale refusal from the account that just signed out must never be attached to a report
    // about the one that signs in next.
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // Same reasoning for the key-outcome vocabulary: a fresh sign-in earns its own `generateKey`
    // call and must not inherit the account that just signed out's.
    LAST_KEY_OUTCOME.store(KEY_OUTCOME_UNKNOWN, Ordering::Relaxed);
}

fn succeeded(v: &Value) -> bool {
    v.get("returnValue").and_then(Value::as_bool) == Some(true)
}

fn error_code(v: &Value) -> Option<i64> {
    v.get("errorCode").and_then(Value::as_i64)
}

fn error_text(v: &Value) -> String {
    v.get("errorText")
        .and_then(Value::as_str)
        .unwrap_or("no errorText")
        .to_string()
}

/// Route through here rather than `crate::log` directly so a test can capture what this module
/// says without a real event-log file — see the `tests` module below.
#[cfg(not(test))]
fn log(m: &str) {
    crate::log(m);
}
#[cfg(test)]
fn log(m: &str) {
    tests::capture(m);
}

/// Which `(method, errorCode)` refusals and `(method, field)` missing-field replies this PROCESS
/// has already logged — a `Mutex`, not a `thread_local!`, because `seal`/`open` run on more than
/// one thread (the SDL main thread via a synchronous session save, and each auth worker
/// `task::spawn_small` starts fresh for a sign-in or roster refresh) and a per-thread cache would
/// dedupe only within one thread, re-logging the same refusal once per worker. A television's
/// keymanager3 answers the same shape call after call (a token is re-sealed on every profile
/// switch), so without a process-wide cache a bad or absent service would fill the primary event
/// log with the same line every few seconds.
static LOGGED_ONCE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn logged_once() -> &'static Mutex<HashSet<String>> {
    LOGGED_ONCE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn log_once(key: String, message: String) {
    let first = logged_once()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key);
    if first {
        log(&message);
    }
}

/// Log a service-refused reply exactly once per distinct `(method, errorCode)` — never the
/// request or reply body, since that is where `data`/`iv`/`handle`/`output` live.
///
/// `errorText` is an arbitrary string the SERVICE chose, not one this module controls, and three of
/// the calls that can refuse (`finish(encrypt)`/`finish(decrypt)`/`begin(decrypt)`) carry sensitive
/// bytes in their own REQUEST — the base64 session plaintext/ciphertext on the two `finish` calls,
/// the GCM IV on `begin(decrypt)` — a service that echoes any of its input back in an error string
/// must never be able to turn this refusal line into a leak. Both `finish` call sites and
/// `begin(decrypt)`'s own call site pass `""` here unconditionally rather than relying on
/// `sanitize_error_text` alone: its base64-run screen (24 characters) is well past a 16-character
/// base64 IV, so it cannot see that one on its own.
fn log_refusal(method: &str, code: i64, text: &str) {
    if let Some(stage) = stage_for_method(method) {
        set_last_refusal(stage, Some(code));
    }
    let safe = sanitize_error_text(text);
    log_once(
        format!("refusal:{method}:{code}"),
        if safe.is_empty() {
            format!("keymanager: {method} refused errorCode={code}")
        } else {
            format!("keymanager: {method} refused errorCode={code} ({safe})")
        },
    );
}

/// Bound `errorText` to a short human sentence and refuse anything shaped like the secret it must
/// never carry. keymanager3's documented errors ("key not found", "iv not set", …) are a handful
/// of words; nothing legitimate needs more than [`MAX_ERROR_TEXT`] characters or a long run of
/// base64 alphabet.
fn sanitize_error_text(text: &str) -> String {
    const MAX_ERROR_TEXT: usize = 64;
    if has_long_base64_run(text) {
        return String::new();
    }
    text.chars().take(MAX_ERROR_TEXT).collect()
}

/// True if `text` contains an unbroken run of base64-alphabet characters long enough to be a
/// fragment of encoded session bytes rather than a word in a human sentence.
fn has_long_base64_run(text: &str) -> bool {
    const RUN: usize = 24; // ~18 decoded bytes — already past any plausible error word
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' {
            run += 1;
            if run >= RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Log a reply that came back `returnValue:true` but missing the one field the caller needed —
/// a shape keymanager3's own reference documents but this module has never observed on real
/// hardware (the dev set predates the service entirely).
fn log_missing_field(method: &str, field: &str) {
    if let Some(stage) = stage_for_method(method) {
        set_last_refusal(stage, None);
    }
    log_once(
        format!("missing:{method}:{field}"),
        format!("keymanager: {method} reply had no {field}"),
    );
}

fn modern_key_ready() -> bool {
    // `None`: this is the SEAL side, so it takes whichever identity this process resolves to and
    // the envelope records it below. `open` is the side that must ask for a specific one.
    let mut client = match platform::Client::new(None) {
        Ok(client) => client,
        Err(e) => {
            note_client_error(e, &mut None);
            return false;
        }
    };
    let Some(v) = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/generateKey",
        &json!({
            "name": KEY_NAME,
            "params": {
                "type": "AES", "size": 256, "mode": ["GCM"],
                "purpose": ["encrypt", "decrypt"], "padding": ["None"]
            }
        }),
    ) else {
        note_unanswered(Some(&client), &mut None);
        return false;
    };
    if succeeded(&v) {
        note_key_outcome(KeyOutcome::Created);
        return true;
    }
    match error_code(&v) {
        Some(-10002) => {
            // "key already exists" — an earlier session already created it.
            note_key_outcome(KeyOutcome::Existed);
            true
        }
        // The LS2 HUB answering for a service that is not on this firmware — every set before
        // webOS 24, the dev set included. Measured on the webOS 4.10 dev set (2026-09-10,
        // `luna-send` as root): `errorCode:-1`, `"Service does not exist: com.webos.service.
        // keymanager3."`, in under 2 ms. That is the ABSENCE of a service, not a refusal by one,
        // so it publishes no `last_refusal` (the seal-failure report would otherwise fire from
        // every such install's first save) and is logged as what it is, once.
        Some(-1) if service_absent(&error_text(&v)) => {
            log_once(
                "absent".to_string(),
                "keymanager: keymanager3 is not on this firmware; using the 0600 file".to_string(),
            );
            false
        }
        Some(code) => {
            log_refusal("generateKey", code, &error_text(&v));
            false
        }
        None => false,
    }
}

/// The hub's own wording for a service nobody registered — `ls-hubd`'s reply, not the service's,
/// which is why it is matched by text: `-1` alone is also what a REAL keymanager3 uses for an
/// unknown method (measured against the legacy `com.webos.service.keymanager` the same day:
/// `-1`, `Unknown method "generateKey" for category "/"`).
fn service_absent(error_text: &str) -> bool {
    error_text.starts_with("Service does not exist")
}

/// `local` is filled in alongside the global `LAST_REFUSAL` at every `log_refusal`/
/// `log_missing_field` call below, so a caller that needs to know THIS call's own outcome (rather
/// than the shared global's, which another thread can write over) has a race-free answer —
/// `open_checked`'s doc has the reasoning. Pass `&mut None` when the caller only wants `LAST_REFUSAL`
/// updated as a side effect and does not need the value back (both `seal` call sites).
fn modern_crypt(
    input: &[u8],
    iv: Option<&str>,
    identity: Option<Identity>,
    local: &mut Option<LastRefusal>,
) -> Option<Sealed> {
    // Keymanager3's operation handle belongs to this logical client operation. Keep one LS2
    // registration alive across begin → finish (and abort on failure) instead of assuming a
    // handle survives the caller disconnecting between two one-shot bus calls.
    let mut client = match platform::Client::new(identity) {
        Ok(client) => client,
        Err(e) => {
            note_client_error(e, local);
            return None;
        }
    };
    // **Read from the CONNECTION, before any call is made** (review finding, 2026-09-10). This is
    // the field a LATER launch opens the envelope with, so it has to name the owner this key
    // actually belongs to. It used to be `identity.or_else(self::identity)`, evaluated after
    // `begin`/`finish` returned — and the latch it fell back to is a process global that a
    // concurrent connection losing a granted name moves to `anonymous` and never back. A seal
    // performed under `app_id` could therefore reach disk labelled `anonymous`; the next launch
    // asks for the anonymous owner's key, gets a `-20030` tag mismatch, and grades the install a
    // genuine REFUSAL — a permanent downgrade to plaintext over a label.
    let connection_identity = client.identity();
    let decrypt = iv.is_some();
    let purpose = if decrypt { "decrypt" } else { "encrypt" };
    let begin_method = if decrypt { "begin(decrypt)" } else { "begin(encrypt)" };
    let finish_method = if decrypt { "finish(decrypt)" } else { "finish(encrypt)" };
    // Mirrors `stage_for_method`'s own mapping so `local` always agrees with what `log_refusal`/
    // `log_missing_field` just published to the global — computed once here rather than at each
    // of the four call sites below.
    let begin_stage = stage_for_method(begin_method);
    let finish_stage = stage_for_method(finish_method);
    // Every field here is in keymanager3's own published ParamSet
    // (webostv.developer.lge.com/develop/references/keymanager3): `type`, `mode`, `purpose` and
    // `padding` on generateKey/begin, plus `iv` on a decrypt begin. There is no `mac_length`
    // field documented anywhere in that ParamSet — the default MAC length applies to both
    // directions of a GCM operation, and the app used to send one anyway.
    let mut params = json!({
        "type": "AES", "mode": ["GCM"], "purpose": [purpose], "padding": ["None"]
    });
    if let Some(iv) = iv {
        params["iv"] = Value::String(iv.to_string());
    }
    let Some(begin) = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/begin",
        &json!({"name": KEY_NAME, "params": params}),
    ) else {
        note_unanswered(Some(&client), local);
        return None;
    };
    if !succeeded(&begin) {
        let code = error_code(&begin);
        if let Some(code) = code {
            // `begin(decrypt)`'s own REQUEST carries the IV (`params.iv`, set above) — a service
            // that echoed it back in `errorText` would need only sanitize_error_text's 24-char
            // base64-run screen to fail (a 12-byte GCM IV is 16 base64 characters), so this call's
            // text is dropped unconditionally, exactly like both `finish` calls already are.
            // `begin(encrypt)`'s request carries no such field.
            let text = if decrypt { "" } else { &error_text(&begin) };
            log_refusal(begin_method, code, text);
        }
        // Issue #76 review: a `returnValue:false` reply that carries NO `errorCode` at all must
        // still record the STAGE — the field report's case (1) is exactly this shape leaving
        // `open_checked` answering `(None, None)`, which a cross-launch caller cannot tell apart
        // from "nothing was ever attempted".
        *local = begin_stage.map(|stage| LastRefusal { stage, error_code: code });
        return None;
    }
    let Some(handle) = begin.get("handle").and_then(Value::as_str).map(str::to_string) else {
        log_missing_field(begin_method, "handle");
        *local = begin_stage.map(|stage| LastRefusal { stage, error_code: None });
        return None;
    };
    let generated_iv = iv
        .map(str::to_string)
        .or_else(|| begin.get("iv").and_then(Value::as_str).map(str::to_string));
    let Some(generated_iv) = generated_iv else {
        log_missing_field(begin_method, "iv");
        *local = begin_stage.map(|stage| LastRefusal { stage, error_code: None });
        abort_modern(&mut client, &handle);
        return None;
    };
    let data = if decrypt {
        // Issue #76 review, field report case (3): a non-UTF-8 decrypt input used to `?`-return
        // straight out of this function, which both left `local` untouched (a cross-launch caller
        // saw no evidence at all) and leaked the handle this `begin` call just opened — this build
        // never asked the service to `abort` it.
        let Ok(s) = std::str::from_utf8(input) else {
            *local = begin_stage.map(|stage| LastRefusal { stage, error_code: None });
            abort_modern(&mut client, &handle);
            return None;
        };
        s.to_string()
    } else {
        b64::encode(input)
    };
    let finish = call_with(
        &mut client,
        "luna://com.webos.service.keymanager3/finish",
        &json!({"handle": handle, "data": data}),
    );
    let Some(finish) = finish else {
        note_unanswered(Some(&client), local);
        abort_modern(&mut client, &handle);
        return None;
    };
    if !succeeded(&finish) {
        let code = error_code(&finish);
        if let Some(code) = code {
            // `finish`'s own REQUEST is `{"handle": handle, "data": data}` — the base64 session
            // plaintext or ciphertext. `errorCode` alone (-20030 verification failed, -20052 iv
            // not set, -10001 key not found, …) is the whole diagnostic; `errorText` on this call
            // is never logged at all, `sanitize_error_text`'s bound notwithstanding — the request
            // it is answering is exactly the shape a payload-echoing reply would leak.
            log_refusal(finish_method, code, "");
        }
        // Same codeless-refusal gap as `begin` above.
        *local = finish_stage.map(|stage| LastRefusal { stage, error_code: code });
        abort_modern(&mut client, &handle);
        return None;
    }
    let Some(output) = finish.get("output").and_then(Value::as_str).map(str::to_string) else {
        log_missing_field(finish_method, "output");
        *local = finish_stage.map(|stage| LastRefusal { stage, error_code: None });
        abort_modern(&mut client, &handle);
        return None;
    };
    Some(Sealed {
        backend: Backend::Keymanager3,
        key: KEY_NAME.to_string(),
        iv: generated_iv,
        data: output,
        identity: connection_identity,
    })
}

fn abort_modern(client: &mut platform::Client, handle: &str) {
    let _ = call_with(
        client,
        "luna://com.webos.service.keymanager3/abort",
        &json!({"handle": handle}),
    );
}

fn call(uri: &str, payload: &Value) -> Option<Value> {
    let mut client = platform::Client::new(None).ok()?;
    call_with(&mut client, uri, payload)
}

fn call_with(client: &mut platform::Client, uri: &str, payload: &Value) -> Option<Value> {
    client
        .call(uri, &payload.to_string())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Test-only bridge into the private scripted backend, for another module's tests
/// (`plex::session`'s issue #76 coverage) that need a keymanager3 double without duplicating one.
/// `mod platform` below is not `pub`, so its `pub(crate)` script hooks are otherwise unreachable
/// outside this file — Rust visibility follows the whole path, not just the leaf item. Also resets
/// the cached backend selection and the log-dedup cache, the same clean slate `keymanager`'s own
/// tests share, so a caller does not have to know those exist to get a predictable `seal`/`open`.
#[cfg(test)]
pub(crate) fn arm_for_test(entries: Vec<(&'static str, Result<Value, ()>)>) {
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
    platform::script_for_test(entries);
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_KEY_OUTCOME.store(KEY_OUTCOME_UNKNOWN, Ordering::Relaxed);
    // The process-global identity latch is reset by this module's own `tests::reset()` but was
    // missing here and from `disarm_for_test` — every OTHER door `plex::session`'s tests use to
    // reach this module. A test that left the latch armed (e.g. one exercising
    // `resolve_registration`'s app-id grant) would leak that identity into the next test's own
    // `seal`, silently turning what should be a sealing save into a plaintext one (review finding,
    // 2026-09-10: `make check` measured 3 failures in 24 runs from exactly this leak).
    IDENTITY.store(IDENTITY_UNSET, Ordering::Relaxed);
}

/// Test-only: undo [`arm_for_test`] — `Client::new` goes back to refusing (the default every
/// non-scripted test relies on), and the backend/log caches are reset again so a later scripted or
/// unscripted call in the same process starts clean.
#[cfg(test)]
pub(crate) fn disarm_for_test() {
    SELECTED.store(UNKNOWN, Ordering::Relaxed);
    logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
    platform::reset_for_test();
    *LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_KEY_OUTCOME.store(KEY_OUTCOME_UNKNOWN, Ordering::Relaxed);
    // **The identity latch too, and it was missing here** — `platform::reset_for_test` puts the
    // scripted HUB back to "grants nothing" but the latch is a separate process global that
    // `resolve_registration` writes ONCE and never revises downward. A test that armed a hub
    // granting a stable identity therefore left it latched for every later test in the process,
    // where `ensure_identity()` returns it without asking anyone — and a later test whose proven
    // marker says `anonymous` then silently fails to seal. That was invisible while every session
    // test armed a hub that refused, i.e. latched `Anonymous`; it surfaced the moment one armed a
    // hub that grants. Same reasoning as `testlock::serial()`: reset the shared global in the
    // shared teardown, not per test.
    IDENTITY.store(IDENTITY_UNSET, Ordering::Relaxed);
}

/// Test-only bridge for the same reason [`arm_for_test`] is one: `plex::session`'s issue #76
/// marker coverage needs to assert that a save gated on the persisted refused-storage marker never
/// even reaches the scripted backend — a call list that is not merely `Err`-free but genuinely
/// EMPTY is the only thing that tells "never asked" apart from "asked and refused".
#[cfg(test)]
pub(crate) fn calls_for_test() -> Vec<(String, String)> {
    platform::calls_for_test()
}

/// Test-only, for the same reason [`arm_for_test`] is: `plex::session`'s identity coverage needs
/// to drive the hub's answer (does ACB hold the app id here, is the app-service registration
/// granted, is the plain NAMED one granted) without a bus. Resets the identity latch too — a
/// scripted hub that contradicts a latched identity would grade nothing.
#[cfg(test)]
pub(crate) fn arm_identity_for_test(
    acb_holds_app_id: bool,
    app_id_granted: bool,
    named_granted: bool,
) {
    IDENTITY.store(IDENTITY_UNSET, Ordering::Relaxed);
    platform::set_hub_for_test(acb_holds_app_id, app_id_granted, named_granted);
}

/// Test-only: the identity each keymanager connection was constructed FOR, in order — `None` for
/// a seal (resolve), `Some(id)` for an open (the envelope's own).
#[cfg(test)]
pub(crate) fn requested_identities_for_test() -> Vec<Option<Identity>> {
    platform::requested_identities_for_test()
}

#[cfg(any(feature = "hostsim", test))]
mod platform {
    /// `dead` mirrors the real client's: a scripted `Err(())` reply IS the timeout shape.
    ///
    /// `fake_mode` exists only outside `cfg(test)`: under `cargo test` (including the `hostsim`
    /// feature pass `make check` runs) the scripted `script` backend is the whole story, exactly
    /// as before issue #76 — the fake service is a SIMULATOR-only behaviour, armed by a real
    /// `/tmp/plxnative-keymanager` trigger file that a unit test never writes.
    pub(super) struct Client {
        dead: bool,
        /// The identity this connection actually registered under — see
        /// `keymanager::resolve_registration`'s doc for why it is carried here rather than
        /// re-read off the process latch.
        #[cfg_attr(not(test), allow(dead_code))]
        identity: super::Identity,
        #[cfg(all(feature = "devtriggers", not(test)))]
        fake_mode: Option<super::fake::Mode>,
    }

    /// Test-only scripted backend. There is no keymanager3 anywhere off-device — the dev TV
    /// predates the service and the simulator has no LS2 bus at all — so `Client` answers
    /// `Err(())` unconditionally UNLESS a test has armed a script, in which case it plays that
    /// script back instead. A hostsim (non-test) build never arms one, so its behaviour is
    /// unchanged: no keymanager3, same as always.
    #[cfg(test)]
    mod script {
        use serde_json::Value;
        use std::cell::RefCell;
        use std::collections::{HashMap, VecDeque};

        thread_local! {
            /// Queued replies per LUNA method (the URI's last path segment: `generateKey`,
            /// `begin`, `finish`, `abort`, `removeKey`), consumed FIFO — so a test can queue a
            /// `begin` success for the encrypt half of an operation and a `begin` refusal for the
            /// decrypt half, in the order those calls actually happen.
            static REPLIES: RefCell<HashMap<&'static str, VecDeque<Result<Value, ()>>>> =
                RefCell::new(HashMap::new());
            /// Every `(uri, payload)` this client sent, in order — lets a test assert on the
            /// REQUEST shape, not only the reply (e.g. that `begin` no longer sends `mac_length`).
            static CALLS: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
            /// What the scripted HUB grants: whether this firmware's ACB holds the app id,
            /// whether the app-service registration would be granted if asked, and whether the
            /// plain NAMED one would. Default `(false, false, false)` — the hub refuses both
            /// named shapes — so every test written before the identity fix keeps sealing exactly
            /// as it always did, as `anonymous`.
            static HUB: RefCell<(bool, bool, bool)> = const { RefCell::new((false, false, false)) };
            /// Every identity a client was constructed FOR, in order: `None` for the seal side
            /// (take what this process resolves to), `Some(id)` for an open, which names the
            /// identity the envelope recorded. This is what proves an open asks for the ENVELOPE'S
            /// identity rather than for whatever is available.
            static REQUESTED: RefCell<Vec<Option<super::super::Identity>>> =
                const { RefCell::new(Vec::new()) };
            /// **The concurrent-downgrade simulator.** `Some(n)` means: after the `n`-th LUNA
            /// call of this script, move the process identity latch to `anonymous`, as another
            /// thread's connection losing a granted name would. It exists because the hazard it
            /// reproduces is a RACE between two connections — the scripted backend and its hub
            /// are `thread_local!`, so a second real thread sees no script at all and the race
            /// cannot be staged honestly. Simulated, therefore, and narrowly: nothing but the
            /// latch is touched, which is exactly the variable the bug read.
            static DOWNGRADE_AFTER: RefCell<Option<u32>> = const { RefCell::new(None) };
            static CALL_COUNT: RefCell<u32> = const { RefCell::new(0) };
        }

        pub(super) fn downgrade_identity_after(n: u32) {
            DOWNGRADE_AFTER.with(|d| *d.borrow_mut() = Some(n));
            CALL_COUNT.with(|c| *c.borrow_mut() = 0);
        }

        fn maybe_downgrade() {
            let n = CALL_COUNT.with(|c| {
                let mut c = c.borrow_mut();
                *c += 1;
                *c
            });
            if DOWNGRADE_AFTER.with(|d| *d.borrow()) == Some(n) {
                super::super::IDENTITY
                    .store(super::super::IDENTITY_ANONYMOUS, std::sync::atomic::Ordering::Relaxed);
            }
        }

        pub(super) fn set_hub(acb_holds_app_id: bool, app_id_granted: bool, named_granted: bool) {
            HUB.with(|h| *h.borrow_mut() = (acb_holds_app_id, app_id_granted, named_granted));
        }

        pub(super) fn acb_holds_app_id() -> bool {
            HUB.with(|h| h.borrow().0)
        }

        pub(super) fn app_id_granted() -> bool {
            HUB.with(|h| h.borrow().1)
        }

        pub(super) fn named_granted() -> bool {
            HUB.with(|h| h.borrow().2)
        }

        pub(super) fn record_requested(identity: Option<super::super::Identity>) {
            REQUESTED.with(|r| r.borrow_mut().push(identity));
        }

        pub(super) fn requested() -> Vec<Option<super::super::Identity>> {
            REQUESTED.with(|r| r.borrow().clone())
        }

        pub(super) fn armed() -> bool {
            REPLIES.with(|r| !r.borrow().is_empty())
        }

        pub(super) fn set(entries: Vec<(&'static str, Result<Value, ()>)>) {
            REQUESTED.with(|r| r.borrow_mut().clear());
            REPLIES.with(|r| {
                let mut r = r.borrow_mut();
                r.clear();
                for (method, reply) in entries {
                    r.entry(method).or_default().push_back(reply);
                }
            });
            CALLS.with(|c| c.borrow_mut().clear());
        }

        pub(super) fn reset() {
            REPLIES.with(|r| r.borrow_mut().clear());
            CALLS.with(|c| c.borrow_mut().clear());
            REQUESTED.with(|r| r.borrow_mut().clear());
            DOWNGRADE_AFTER.with(|d| *d.borrow_mut() = None);
            CALL_COUNT.with(|c| *c.borrow_mut() = 0);
            set_hub(false, false, false);
        }

        pub(super) fn record_call(uri: &str, payload: &str) {
            CALLS.with(|c| c.borrow_mut().push((uri.to_string(), payload.to_string())));
            maybe_downgrade();
        }

        pub(super) fn calls() -> Vec<(String, String)> {
            CALLS.with(|c| c.borrow().clone())
        }

        pub(super) fn next_reply(uri: &str) -> Result<Value, ()> {
            let method = uri.rsplit('/').next().unwrap_or("");
            REPLIES.with(|r| {
                r.borrow_mut()
                    .get_mut(method)
                    .and_then(VecDeque::pop_front)
                    .unwrap_or(Err(()))
            })
        }
    }

    /// Test-only: script the reply keymanager3 gives to each LUNA method. Replaces any previous
    /// script and clears the recorded call log.
    #[cfg(test)]
    pub(crate) fn script_for_test(entries: Vec<(&'static str, Result<serde_json::Value, ()>)>) {
        script::set(entries);
    }

    /// Test-only: clear the script — `Client::new` goes back to refusing, the default every
    /// non-scripted test relies on.
    #[cfg(test)]
    pub(crate) fn reset_for_test() {
        script::reset();
    }

    /// Test-only: every request this client sent, in call order.
    #[cfg(test)]
    pub(crate) fn calls_for_test() -> Vec<(String, String)> {
        script::calls()
    }

    /// Test-only: what the scripted hub answers about identity — whether this firmware's ACB
    /// holds the app id, whether the app-service registration is granted when asked, and whether
    /// the plain named one is.
    #[cfg(test)]
    pub(crate) fn set_hub_for_test(
        acb_holds_app_id: bool,
        app_id_granted: bool,
        named_granted: bool,
    ) {
        script::set_hub(acb_holds_app_id, app_id_granted, named_granted);
    }

    /// Test-only: the identity every client so far was constructed FOR, in order.
    #[cfg(test)]
    pub(crate) fn requested_identities_for_test() -> Vec<Option<super::Identity>> {
        script::requested()
    }

    /// Test-only: see `script::DOWNGRADE_AFTER`.
    #[cfg(test)]
    pub(crate) fn downgrade_identity_after_calls_for_test(n: u32) {
        script::downgrade_identity_after(n);
    }

    impl Client {
        /// Same contract as the device client's: `None` resolves the process identity through
        /// `keymanager::resolve_registration` (the real decision, driven here by the scripted
        /// hub), `Some(id)` demands exactly that identity and refuses with
        /// [`super::ClientError::IdentityUnavailable`] when the scripted hub will not grant it —
        /// or when the scripted set's ACB holds the app id, which refuses a required NAME
        /// regardless of what the hub would answer, exactly as the device client does.
        #[allow(unused_variables)]
        pub(super) fn new(
            required: Option<super::Identity>,
        ) -> Result<Self, super::ClientError> {
            #[cfg(test)]
            if script::armed() {
                script::record_requested(required);
                return match required {
                    Some(id)
                        if super::acb_withholds_required_identity(
                            id,
                            script::acb_holds_app_id(),
                        ) =>
                    {
                        Err(super::ClientError::IdentityUnavailable)
                    }
                    Some(super::Identity::AppId) if !script::app_id_granted() => {
                        Err(super::ClientError::IdentityUnavailable)
                    }
                    Some(super::Identity::Named) if !script::named_granted() => {
                        Err(super::ClientError::IdentityUnavailable)
                    }
                    Some(id) => Ok(Self {
                        dead: false,
                        identity: id,
                    }),
                    None => super::resolve_registration(
                        script::acb_holds_app_id(),
                        || {
                            script::app_id_granted()
                                .then_some(())
                                .ok_or_else(|| super::app_service_refusal_reason(-1028, "scripted"))
                        },
                        || {
                            script::named_granted()
                                .then_some(())
                                .ok_or_else(|| super::named_refusal_reason(-1028, "scripted"))
                        },
                        || Ok(()),
                    )
                    .map(|((), identity)| Self {
                        dead: false,
                        identity,
                    })
                    .map_err(|_: super::ClientError| super::ClientError::Setup),
                };
            }
            #[cfg(all(feature = "devtriggers", not(test)))]
            if let Some(mode) = super::fake::armed_mode() {
                return Ok(Self {
                    dead: false,
                    // The fake service bypasses LS2 entirely — nothing registered — so the honest
                    // answer is whatever the process has resolved to so far, and `Anonymous` on a
                    // launch that never registered at all.
                    identity: required.or_else(super::identity).unwrap_or_default(),
                    fake_mode: Some(mode),
                });
            }
            Err(super::ClientError::Setup)
        }

        /// The identity this connection registered under.
        pub(super) fn identity(&self) -> super::Identity {
            self.identity
        }

        pub(super) fn is_dead(&self) -> bool {
            self.dead
        }

        #[allow(unused_variables)]
        pub(super) fn call(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            #[cfg(test)]
            {
                if self.dead {
                    return Err(());
                }
                script::record_call(uri, payload);
                let reply = script::next_reply(uri).map(|v| v.to_string());
                if reply.is_err() {
                    self.dead = true;
                }
                return reply;
            }
            #[cfg(all(feature = "devtriggers", not(test)))]
            if let Some(mode) = self.fake_mode {
                if self.dead {
                    return Err(());
                }
                let result = super::fake::call(mode, uri, payload);
                if result.is_err() || super::fake::is_dead(mode) {
                    self.dead = true;
                }
                return result;
            }
            #[cfg(not(test))]
            Err(())
        }
    }
}

#[cfg(all(not(feature = "hostsim"), not(test)))]
mod platform {
    use std::time::Duration;

    /// Keymanager3's budget. A key generation on a cold set is not a 600 ms affair, and this
    /// client never runs on the press path `webos::ls2::BUDGET` is sized for.
    const BUDGET: Duration = Duration::from_secs(4);

    /// One registration on the bus, kept alive for the length of a logical keymanager operation
    /// (`modern_crypt` needs begin → finish on ONE connection). The registration itself is the
    /// process-wide `webos::ls2` client — the shape it registers with and the reason are there.
    ///
    /// **A service that stalls once is not asked again on this client.** Registration succeeds on
    /// the dev set since 2026-09-04, which makes [`BUDGET`] REACHABLE from a synchronous session
    /// save for the first time, and `modern_crypt`'s begin → (finish | abort) is two calls: a
    /// keymanager3 that hangs on the first would otherwise cost two budgets on a path that holds
    /// the auth and session locks (Codex review, 2026-09-04). A timeout marks the client dead and
    /// every later call on it answers at once; `seal` then records the backend unavailable.
    /// `Fake` exists only with `devtriggers` — see `keymanager::fake`'s own doc. A release build
    /// (`--no-default-features`) never constructs it: `Client::new` below checks the trigger only
    /// under the same `#[cfg]`, so on that build this variant is simply never reached, and it is
    /// gone from the binary rather than merely unreachable at runtime.
    enum Inner {
        Real {
            registration: crate::webos::ls2::Registration,
            dead: bool,
        },
        #[cfg(feature = "devtriggers")]
        Fake { mode: super::fake::Mode, dead: bool },
    }

    pub(super) struct Client {
        inner: Inner,
        /// The identity this connection registered under — see
        /// `keymanager::resolve_registration`'s doc for why the seal side reads it from here
        /// rather than from the process latch after the fact.
        identity: super::Identity,
    }

    /// The app-id registration, with the hub's refusal already turned into the `(<reason>)` both
    /// call sites below need — the identity decision's log line, and the "kept, not refused" line
    /// an envelope that names an identity this launch cannot get gets instead.
    fn app_service_registration() -> Result<crate::webos::ls2::Registration, String> {
        crate::webos::ls2::register_app_service()
            .map_err(|refused| super::app_service_refusal_reason(refused.code, &refused.to_string()))
    }

    /// The same, for the plain named shape — `LSRegister(app_id)`, the middle preference. Its
    /// refusal is graded by its OWN function: for this shape a `-1027` really is about the name,
    /// where the app-service form gives one even when handed no name at all.
    fn named_registration() -> Result<crate::webos::ls2::Registration, String> {
        crate::webos::ls2::register_named()
            .map_err(|refused| super::named_refusal_reason(refused.code, &refused.to_string()))
    }

    impl Client {
        /// `required` is `None` on the seal side (take whichever identity this process resolves
        /// to) and `Some(id)` on the open side, where the envelope names the owner its key
        /// belongs to. A required identity is never silently substituted: registering as somebody
        /// else would decrypt nothing and report a service refusal for what is really a name this
        /// launch could not get, so it comes back as
        /// [`super::ClientError::IdentityUnavailable`] instead.
        ///
        /// **A required NAME is refused before the hub is asked on a set whose ACB holds the app
        /// id** — [`super::acb_withholds_required_identity`], the same design order
        /// `resolve_registration` applies to the `None` side.
        pub(super) fn new(
            required: Option<super::Identity>,
        ) -> Result<Self, super::ClientError> {
            #[cfg(feature = "devtriggers")]
            if let Some(mode) = super::fake::armed_mode() {
                return Ok(Self {
                    inner: Inner::Fake { mode, dead: false },
                    // The fake service bypasses LS2 entirely — nothing registered — so the honest
                    // answer is whatever the process has resolved to so far, and `Anonymous` on a
                    // launch that never registered at all. The ACB gate below is skipped with the
                    // rest of the registration for the same reason: there is no bus name to lose.
                    identity: required.or_else(super::identity).unwrap_or_default(),
                });
            }
            if required.is_some_and(|id| {
                super::acb_withholds_required_identity(id, crate::player::acb_holds_app_id())
            }) {
                crate::log(
                    "keymanager: this envelope was sealed under a bus name libAcbAPI holds on this firmware — not asking for it, and keeping the envelope",
                );
                return Err(super::ClientError::IdentityUnavailable);
            }
            let (registration, identity) = match required {
                // Which identity to seal under, and why, is `keymanager::resolve_registration`'s
                // decision — taken once for the process and reused by every later connection.
                // `acb_holds_app_id` is the firmware fact it needs: on a set with `libAcbAPI`,
                // ACB is handed the app id at `player::acb_init` and holds that bus name for the
                // process.
                None => super::resolve_registration(
                    crate::player::acb_holds_app_id(),
                    app_service_registration,
                    named_registration,
                    crate::webos::ls2::register,
                )
                .map_err(|e| {
                    crate::log(&format!("keymanager: LS2 {e}"));
                    super::ClientError::Setup
                })?,
                Some(super::Identity::Anonymous) => (
                    crate::webos::ls2::register().map_err(|e| {
                        crate::log(&format!("keymanager: LS2 {e}"));
                        super::ClientError::Setup
                    })?,
                    super::Identity::Anonymous,
                ),
                Some(super::Identity::AppId) => (
                    app_service_registration().map_err(|reason| {
                        crate::log(&format!(
                            "keymanager: this envelope was sealed as app_id and this launch cannot register as one ({reason}) — keeping it"
                        ));
                        super::ClientError::IdentityUnavailable
                    })?,
                    super::Identity::AppId,
                ),
                // Never substituted by the app-service form, even though that is the STRONGER
                // identity: keymanager3 keyed this envelope's key by the sender service name, so
                // registering as an application service would be a different owner and would
                // decrypt nothing — the same reasoning the `AppId` arm is not substituted by this
                // one.
                Some(super::Identity::Named) => (
                    named_registration().map_err(|reason| {
                        crate::log(&format!(
                            "keymanager: this envelope was sealed as named and this launch cannot register under that name ({reason}) — keeping it"
                        ));
                        super::ClientError::IdentityUnavailable
                    })?,
                    super::Identity::Named,
                ),
            };
            Ok(Self {
                inner: Inner::Real {
                    registration,
                    dead: false,
                },
                identity,
            })
        }

        /// The identity this connection registered under.
        pub(super) fn identity(&self) -> super::Identity {
            self.identity
        }

        pub(super) fn is_dead(&self) -> bool {
            match &self.inner {
                Inner::Real { dead, .. } => *dead,
                #[cfg(feature = "devtriggers")]
                Inner::Fake { dead, .. } => *dead,
            }
        }

        pub(super) fn call(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            #[cfg(feature = "devtriggers")]
            if let Inner::Fake { mode, dead } = &mut self.inner {
                if *dead {
                    return Err(());
                }
                let mode = *mode;
                let result = super::fake::call(mode, uri, payload);
                if result.is_err() || super::fake::is_dead(mode) {
                    *dead = true;
                }
                return result;
            }
            self.call_real(uri, payload)
        }

        fn call_real(&mut self, uri: &str, payload: &str) -> Result<String, ()> {
            #[cfg(feature = "devtriggers")]
            let Inner::Real { registration, dead } = &mut self.inner else {
                unreachable!("the fake variant is handled by `call` before reaching here")
            };
            #[cfg(not(feature = "devtriggers"))]
            let Inner::Real { registration, dead } = &mut self.inner;
            if *dead {
                return Err(());
            }
            let started = std::time::Instant::now();
            match registration.call(uri, payload, BUDGET) {
                Ok(reply) => Ok(reply),
                Err(crate::webos::ls2::Fail::Timeout) => {
                    *dead = true;
                    crate::log(&format!(
                        "keymanager: no reply in {} ms — this client asks nothing more",
                        started.elapsed().as_millis()
                    ));
                    Err(())
                }
                Err(crate::webos::ls2::Fail::Setup { stage, detail }) => {
                    crate::log(&format!("keymanager: call failed stage={stage} ({detail})"));
                    Err(())
                }
            }
        }
    }
}

mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub(super) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = (chunk[0] as u32) << 16
                | (*chunk.get(1).unwrap_or(&0) as u32) << 8
                | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - i * 6)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
        if text.len() % 4 != 0 {
            return None;
        }
        let mut acc = 0u32;
        let mut bits = 0u32;
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        for ch in text.bytes() {
            if ch == b'=' {
                break;
            }
            let value = ALPHABET.iter().position(|&x| x == ch)? as u32;
            acc = (acc << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}

/// A dev-only, in-process fake `com.webos.service.keymanager3` — issue #76.
///
/// The reporter's failure ("seals and round-trips in-process but the envelope never reopens on
/// the NEXT launch") needs a backend whose key genuinely changes across a process boundary, and
/// nothing else here can produce that: no firmware this project has ever run against carries the
/// real service (the dev set predates it entirely), and the scripted [`platform`] test double
/// answers one fixed script rather than behaving like a stateful service across calls. Selected at
/// runtime by [`crate::dev::keymanager_fake_mode`] instead of the real LS2 client (device builds)
/// or the always-refusing stub (simulator builds), so the failure — and the fix — are reproducible
/// on `make sim` and on a debug install pointed at a television with no keymanager3 at all.
///
/// **Never real crypto.** The seal is a keyed XOR stream plus an FNV-style tag over
/// `key || iv || ciphertext`, which is enough to (a) look like an AES-GCM envelope on the wire —
/// `handle`/`iv`/`data`/`output`, base64 throughout — and (b) DETECT a foreign key on decrypt
/// (the whole point: a wrong key must fail exactly like the real service's GCM tag check, with
/// `errorCode -20030`), and nothing more. Entirely absent from a `--no-default-features` build —
/// see `no_fake_in_release_build` below.
#[cfg(feature = "devtriggers")]
pub(crate) mod fake {
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// The trigger's parsed mode — see `dev.rs`'s `plxnative-keymanager` doc for the grammar.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Mode {
        /// A fresh random key every PROCESS, never persisted: an envelope sealed by this process
        /// opens fine in THIS process and fails the tag check in the next one.
        PerProcess,
        /// The key is persisted in the runtime dir (`plxnative-keymanager-key`), so an envelope
        /// keeps opening across launches — the probe this issue's fix relies on passing.
        Healthy,
        /// Every call answers nothing — the no-reply shape ([`crate::webos::ls2::Fail::Timeout`]'s
        /// analogue for this fake).
        Stall,
        /// `begin` always refuses with this service `errorCode`.
        Refuse(i64),
        /// `begin` always refuses with `returnValue:false` and no `errorCode` at all.
        NoCode,
        /// `finish(decrypt)` always succeeds but hands back an `output` this build cannot
        /// base64-decode.
        BadOutput,
        /// The service does not exist on this firmware — `generateKey` answers exactly the shape
        /// `keymanager::service_absent` recognises.
        Absent,
    }

    fn parse(raw: &str) -> Option<Mode> {
        match raw {
            "perprocess" => Some(Mode::PerProcess),
            "healthy" => Some(Mode::Healthy),
            "stall" => Some(Mode::Stall),
            "nocode" => Some(Mode::NoCode),
            "badoutput" => Some(Mode::BadOutput),
            "absent" => Some(Mode::Absent),
            _ => raw
                .strip_prefix("refuse=")
                .and_then(|c| c.trim().parse().ok())
                .map(Mode::Refuse),
        }
    }

    /// The armed mode, read and parsed exactly once for the process — the same once-at-boot
    /// contract [`crate::dev::servers`]/`playurl` use for their own trigger.
    #[cfg(not(test))]
    pub(crate) fn armed_mode() -> Option<Mode> {
        static ONCE: OnceLock<Option<Mode>> = OnceLock::new();
        *ONCE.get_or_init(|| crate::dev::keymanager_fake_mode().as_deref().and_then(parse))
    }

    /// Log `keymanager: FAKE service armed mode=<mode>` at boot, from `plex_run`, before anything
    /// could have called `seal`/`open` — so no capture or log excerpt from an armed run can be
    /// mistaken for a real service, even one that happens to seal nothing that boot.
    pub(crate) fn log_if_armed() {
        let Some(raw) = crate::dev::keymanager_fake_mode() else {
            return;
        };
        match parse(&raw) {
            Some(_) => crate::log(&format!("keymanager: FAKE service armed mode={raw}")),
            None => crate::log(&format!(
                "keymanager: plxnative-keymanager={raw:?} is not a recognised fake mode — real client behaviour is unchanged"
            )),
        }
    }

    // ---- key material -----------------------------------------------------------------------

    fn random_key() -> [u8; 32] {
        use std::time::{SystemTime, UNIX_EPOCH};
        let mut seed = std::process::id() as u64;
        seed ^= SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // A stack address as one more entropy source — not cryptographic, only enough that two
        // processes started in the same millisecond still diverge.
        seed ^= &seed as *const u64 as u64;
        let mut state = seed | 1;
        let mut key = [0u8; 32];
        for chunk in key.chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }
        key
    }

    fn key_hex(key: &[u8; 32]) -> String {
        key.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn key_from_hex(s: &str) -> Option<[u8; 32]> {
        let s = s.trim();
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(out)
    }

    const KEY_FILE: &str = "plxnative-keymanager-key";

    fn healthy_key() -> [u8; 32] {
        // Test builds only (`devtriggers` is in the default feature set): per-PROCESS, not
        // per-machine — `paths::in_runtime_dir` resolves to the literal `/tmp` in a test build, so
        // two `cargo test` processes in different worktrees sharing one machine used to delete and
        // remint each other's key mid-test, failing `fake::tests::healthy_reopens_across_a_
        // simulated_process` in a fleet (review finding, 2026-09-10).
        #[cfg(test)]
        let key_file = format!("{KEY_FILE}.{}", std::process::id());
        #[cfg(not(test))]
        let key_file = KEY_FILE.to_string();
        let path = crate::paths::in_runtime_dir(&key_file);
        // Owned-regular-file read and an atomic 0600 write, same as every other app-owned file in
        // the shared 1777 runtime root — this key seals the real `<id>-auth.json` envelope
        // whenever a debug install has `plxnative-keymanager=healthy` armed, so a world-readable
        // key beside a "sealed" session would defeat the seal for anyone else in that directory.
        if let Some((bytes, trust)) = crate::plex::session::read_owned_regular_trusted(&path) {
            if trust.content_trusted() {
                if let Some(k) = std::str::from_utf8(&bytes).ok().and_then(key_from_hex) {
                    return k;
                }
            }
            // A write-widened key file means another uid in the shared runtime root could have
            // chosen these bytes — sealing with them would not provably be this process's own key,
            // which defeats the point of the seal it is about to perform. Fall through and mint a
            // fresh one instead of trusting forged content (review finding, 2026-09-10).
        }
        let k = random_key();
        let _ = crate::plex::session::write_atomic(&path, key_hex(&k).as_bytes());
        k
    }

    /// The process-lifetime key for every mode except [`Mode::Healthy`] — fresh every process,
    /// never written to disk.
    #[cfg(not(test))]
    fn per_process_key() -> [u8; 32] {
        static KEY: OnceLock<[u8; 32]> = OnceLock::new();
        *KEY.get_or_init(random_key)
    }

    #[cfg(test)]
    pub(crate) fn reset_per_process_key_for_test() {
        // There is no real way to un-run a `OnceLock`, so the test double for "a new process"
        // is a second cell behind the same accessor — see `per_process_key`'s test-only twin
        // below, which `key_for` reads instead once a test has armed it.
        PER_PROCESS_GENERATION
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }

    #[cfg(test)]
    static PER_PROCESS_GENERATION: Mutex<Option<[u8; 32]>> = Mutex::new(None);

    #[cfg(test)]
    fn per_process_key_for_test() -> [u8; 32] {
        let mut slot = PER_PROCESS_GENERATION.lock().unwrap_or_else(|e| e.into_inner());
        *slot.get_or_insert_with(random_key)
    }

    fn key_for(mode: Mode) -> [u8; 32] {
        match mode {
            Mode::Healthy => healthy_key(),
            #[cfg(test)]
            _ => per_process_key_for_test(),
            #[cfg(not(test))]
            _ => per_process_key(),
        }
    }

    // ---- keyed XOR stream + tag --------------------------------------------------------------

    fn keystream_byte(key: &[u8; 32], iv: &[u8], index: usize) -> u8 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in key.iter().chain(iv.iter()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h ^= index as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
        (h ^ (h >> 32)) as u8
    }

    fn xor_crypt(key: &[u8; 32], iv: &[u8], data: &[u8]) -> Vec<u8> {
        data.iter()
            .enumerate()
            .map(|(i, b)| b ^ keystream_byte(key, iv, i))
            .collect()
    }

    const TAG_LEN: usize = 8;

    /// A MAC-*shaped* tag: not cryptographically sound, but a wrong key changes every bit of it
    /// with overwhelming probability, which is all a fake needs to detect "a different process's
    /// key" the way the real service's GCM tag detects a genuinely wrong key.
    fn mac(key: &[u8; 32], iv: &[u8], ciphertext: &[u8]) -> [u8; TAG_LEN] {
        let mut h: u64 = 0xd6e8_feb8_6659_fd93;
        for b in key.iter().chain(iv.iter()).chain(ciphertext.iter()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h.to_le_bytes()
    }

    // ---- handle bookkeeping ------------------------------------------------------------------

    struct HandleState {
        decrypt: bool,
        iv: Vec<u8>,
    }

    fn handles() -> &'static Mutex<HashMap<String, HandleState>> {
        static H: OnceLock<Mutex<HashMap<String, HandleState>>> = OnceLock::new();
        H.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn next_handle() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        format!(
            "fake-{}",
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    // ---- the service itself ------------------------------------------------------------------

    /// Is `mode` the "every call gets no reply" shape? Mirrors the real client's `is_dead()`
    /// contract: once true, `keymanager::note_unanswered` records `StorageStage::NoReply` instead
    /// of `Unreachable`.
    pub(crate) fn is_dead(mode: Mode) -> bool {
        matches!(mode, Mode::Stall)
    }

    /// Answer one LUNA call the way `com.webos.service.keymanager3` would, for the armed `mode`.
    /// `uri`'s last path segment selects the method; `payload` is the JSON request body.
    pub(crate) fn call(mode: Mode, uri: &str, payload: &str) -> Result<String, ()> {
        if mode == Mode::Stall {
            return Err(());
        }
        let method = uri.rsplit('/').next().unwrap_or("");
        let req: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let reply = match method {
            "generateKey" => generate_key(mode),
            "begin" => begin(mode, &req),
            "finish" => finish(mode, &req),
            "abort" => {
                if let Some(h) = req.get("handle").and_then(Value::as_str) {
                    handles().lock().unwrap_or_else(|e| e.into_inner()).remove(h);
                }
                json!({"returnValue": true})
            }
            "removeKey" => json!({"returnValue": true}),
            _ => json!({"returnValue": false, "errorCode": -1, "errorText": "unknown method"}),
        };
        Ok(reply.to_string())
    }

    fn absent_reply() -> Value {
        json!({
            "returnValue": false, "errorCode": -1,
            "errorText": "Service does not exist: com.webos.service.keymanager3."
        })
    }

    fn generate_key(mode: Mode) -> Value {
        if mode == Mode::Absent {
            return absent_reply();
        }
        let _ = key_for(mode); // materialise (and, for Healthy, persist) the key up front
        json!({"returnValue": true})
    }

    fn begin(mode: Mode, req: &Value) -> Value {
        match mode {
            Mode::Absent => return absent_reply(),
            Mode::Refuse(code) => {
                return json!({"returnValue": false, "errorCode": code, "errorText": "fake refusal"});
            }
            Mode::NoCode => return json!({"returnValue": false}),
            _ => {}
        }
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let decrypt = params
            .get("purpose")
            .and_then(Value::as_array)
            .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some("decrypt")));
        let iv: Vec<u8> = if decrypt {
            let Some(iv) = params
                .get("iv")
                .and_then(Value::as_str)
                .and_then(super::b64::decode)
            else {
                return json!({"returnValue": false, "errorCode": -20052, "errorText": "iv not set"});
            };
            iv
        } else {
            random_key()[..12].to_vec()
        };
        let handle = next_handle();
        handles().lock().unwrap_or_else(|e| e.into_inner()).insert(
            handle.clone(),
            HandleState {
                decrypt,
                iv: iv.clone(),
            },
        );
        let mut reply = json!({"returnValue": true, "handle": handle});
        if !decrypt {
            reply["iv"] = Value::String(super::b64::encode(&iv));
        }
        reply
    }

    fn finish(mode: Mode, req: &Value) -> Value {
        let handle = req.get("handle").and_then(Value::as_str).unwrap_or("");
        let data = req.get("data").and_then(Value::as_str).unwrap_or("");
        let Some(state) = handles().lock().unwrap_or_else(|e| e.into_inner()).remove(handle) else {
            return json!({"returnValue": false, "errorCode": -10001, "errorText": "handle not found"});
        };
        let key = key_for(mode);
        if state.decrypt {
            if mode == Mode::BadOutput {
                return json!({"returnValue": true, "output": "!!!not-base64!!!"});
            }
            let Some(blob) = super::b64::decode(data) else {
                return json!({"returnValue": false, "errorCode": -20030, "errorText": "verification failed"});
            };
            if blob.len() < TAG_LEN {
                return json!({"returnValue": false, "errorCode": -20030, "errorText": "verification failed"});
            }
            let (ciphertext, tag) = blob.split_at(blob.len() - TAG_LEN);
            if tag != mac(&key, &state.iv, ciphertext) {
                // The exact shape the reporter's "opens in-process, refuses cross-process" bug
                // produces on real hardware: a genuine GCM tag mismatch, `errorCode -20030`.
                return json!({"returnValue": false, "errorCode": -20030, "errorText": "verification failed"});
            }
            let plain = xor_crypt(&key, &state.iv, ciphertext);
            json!({"returnValue": true, "output": super::b64::encode(&plain)})
        } else {
            let Some(plain) = super::b64::decode(data) else {
                return json!({"returnValue": false, "errorCode": -20031, "errorText": "bad input"});
            };
            let ciphertext = xor_crypt(&key, &state.iv, &plain);
            let tag = mac(&key, &state.iv, &ciphertext);
            let mut blob = ciphertext;
            blob.extend_from_slice(&tag);
            json!({"returnValue": true, "output": super::b64::encode(&blob)})
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn reset() {
            let _ = std::fs::remove_file(
                crate::paths::in_runtime_dir(&format!("{KEY_FILE}.{}", std::process::id())),
            );
            handles().lock().unwrap_or_else(|e| e.into_inner()).clear();
            reset_per_process_key_for_test();
        }

        /// `perprocess`: an envelope sealed and opened by begin/finish calls that share this
        /// process's key round-trips; simulating a NEW process (resetting the key) makes the same
        /// envelope refuse with the real service's own tag-mismatch code.
        #[test]
        fn perprocess_reopens_in_process_and_refuses_across_a_simulated_process() {
            let _guard = crate::testlock::serial();
            reset();
            let mode = Mode::PerProcess;

            let begin_enc: Value = serde_json::from_str(
                &call(mode, "luna://.../begin", &json!({"params": {"purpose": ["encrypt"]}}).to_string())
                    .unwrap(),
            )
            .unwrap();
            let handle = begin_enc["handle"].as_str().unwrap().to_string();
            let iv = begin_enc["iv"].as_str().unwrap().to_string();
            let plain = super::super::b64::encode(b"issue-76 plaintext");
            let finish_enc: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle, "data": plain}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            let sealed = finish_enc["output"].as_str().unwrap().to_string();

            // Same process: opens fine.
            let begin_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../begin",
                    &json!({"params": {"purpose": ["decrypt"], "iv": iv}}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(begin_dec["returnValue"], true);
            let handle2 = begin_dec["handle"].as_str().unwrap().to_string();
            let finish_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle2, "data": sealed}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(finish_dec["returnValue"], true);
            assert_eq!(
                super::super::b64::decode(finish_dec["output"].as_str().unwrap()).unwrap(),
                b"issue-76 plaintext"
            );

            // Simulate a new process: the key changes, and the same envelope now fails at
            // finish(decrypt) with the real service's own verification-failed code.
            reset_per_process_key_for_test();
            let begin_dec2: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../begin",
                    &json!({"params": {"purpose": ["decrypt"], "iv": iv}}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            let handle3 = begin_dec2["handle"].as_str().unwrap().to_string();
            let finish_dec2: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle3, "data": sealed}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(finish_dec2["returnValue"], false);
            assert_eq!(finish_dec2["errorCode"], -20030);
        }

        /// `healthy`: the key survives a simulated process boundary because it is read back from
        /// the persisted file rather than the per-process cell.
        #[test]
        fn healthy_reopens_across_a_simulated_process() {
            let _guard = crate::testlock::serial();
            reset();
            let mode = Mode::Healthy;

            let begin_enc: Value = serde_json::from_str(
                &call(mode, "luna://.../begin", &json!({"params": {"purpose": ["encrypt"]}}).to_string())
                    .unwrap(),
            )
            .unwrap();
            let handle = begin_enc["handle"].as_str().unwrap().to_string();
            let iv = begin_enc["iv"].as_str().unwrap().to_string();
            let plain = super::super::b64::encode(b"issue-76 plaintext");
            let finish_enc: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle, "data": plain}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            let sealed = finish_enc["output"].as_str().unwrap().to_string();

            // Simulate a new process — unlike `perprocess`, `healthy`'s key is disk-backed and
            // does not move.
            reset_per_process_key_for_test();
            let begin_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../begin",
                    &json!({"params": {"purpose": ["decrypt"], "iv": iv}}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            let handle2 = begin_dec["handle"].as_str().unwrap().to_string();
            let finish_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle2, "data": sealed}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(finish_dec["returnValue"], true);
            assert_eq!(
                super::super::b64::decode(finish_dec["output"].as_str().unwrap()).unwrap(),
                b"issue-76 plaintext"
            );
            reset();
        }

        #[test]
        fn stall_answers_nothing() {
            assert!(is_dead(Mode::Stall));
            assert!(call(Mode::Stall, "luna://.../generateKey", "{}").is_err());
        }

        #[test]
        fn refuse_carries_the_requested_code() {
            let _guard = crate::testlock::serial();
            reset();
            let v: Value =
                serde_json::from_str(&call(Mode::Refuse(-20099), "luna://.../begin", "{}").unwrap())
                    .unwrap();
            assert_eq!(v["returnValue"], false);
            assert_eq!(v["errorCode"], -20099);
        }

        #[test]
        fn nocode_refuses_with_no_error_code_field() {
            let _guard = crate::testlock::serial();
            reset();
            let v: Value =
                serde_json::from_str(&call(Mode::NoCode, "luna://.../begin", "{}").unwrap()).unwrap();
            assert_eq!(v["returnValue"], false);
            assert!(v.get("errorCode").is_none());
        }

        #[test]
        fn badoutput_finish_decrypt_is_not_base64() {
            let _guard = crate::testlock::serial();
            reset();
            let mode = Mode::BadOutput;
            let begin_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../begin",
                    &json!({"params": {"purpose": ["decrypt"], "iv": super::super::b64::encode(b"0123456789ab")}})
                        .to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            let handle = begin_dec["handle"].as_str().unwrap().to_string();
            let finish_dec: Value = serde_json::from_str(
                &call(
                    mode,
                    "luna://.../finish",
                    &json!({"handle": handle, "data": "irrelevant"}).to_string(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(finish_dec["returnValue"], true);
            assert!(super::super::b64::decode(finish_dec["output"].as_str().unwrap()).is_none());
        }

        #[test]
        fn absent_answers_generate_key_with_the_hub_service_missing_shape() {
            let v: Value =
                serde_json::from_str(&call(Mode::Absent, "luna://.../generateKey", "{}").unwrap())
                    .unwrap();
            assert_eq!(v["returnValue"], false);
            assert_eq!(v["errorCode"], -1);
            assert_eq!(
                v["errorText"],
                "Service does not exist: com.webos.service.keymanager3."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        b64, open, platform, remove, seal, Backend, Sealed, StorageStage, MODERN, SELECTED,
        UNAVAILABLE, UNKNOWN,
    };
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    thread_local! {
        static CAPTURED: RefCell<Vec<String>> = RefCell::new(Vec::new());
    }

    /// Called by [`super::log`] instead of writing a real event-log file. A brand-new test
    /// thread's thread-local storage starts empty, but every test below resets it explicitly —
    /// the fixture is what a scripted round trip actually asserts on, not an assumption about the
    /// test harness's thread reuse policy.
    pub(super) fn capture(m: &str) {
        CAPTURED.with(|c| c.borrow_mut().push(m.to_string()));
    }

    fn captured() -> Vec<String> {
        CAPTURED.with(|c| c.borrow().clone())
    }

    /// Shared setup for every scripted-backend test: a clean slate for the backend cache, the
    /// log-dedup cache, the captured lines and the scripted client — held under `testlock::serial`
    /// because `SELECTED` and the log dedup cache are process globals other keymanager tests also
    /// touch.
    fn reset() {
        SELECTED.store(UNKNOWN, Ordering::Relaxed);
        super::logged_once().lock().unwrap_or_else(|e| e.into_inner()).clear();
        CAPTURED.with(|c| c.borrow_mut().clear());
        platform::reset_for_test();
        *super::LAST_REFUSAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        super::LAST_KEY_OUTCOME.store(super::KEY_OUTCOME_UNKNOWN, Ordering::Relaxed);
        // The identity latch is a process global like the two above, and it is one-way by design
        // — a test that inherited another's `Anonymous` would silently grade nothing.
        super::IDENTITY.store(super::IDENTITY_UNSET, Ordering::Relaxed);
    }

    /// Issue #76's identity decision, graded on the host. `webos::ls2` does not exist in a test
    /// build, so the three registrations are closures: `app` counts how often the app-service
    /// shape was ASKED FOR (a set where nothing may hold that name must never be asked at all),
    /// `named` the plain `LSRegister(app_id)` shape, `anon` the fallback, and the payload is a
    /// string standing in for a `Registration`.
    struct Attempts {
        app: std::cell::Cell<u32>,
        named: std::cell::Cell<u32>,
        anon: std::cell::Cell<u32>,
    }

    impl Attempts {
        fn new() -> Self {
            Self {
                app: std::cell::Cell::new(0),
                named: std::cell::Cell::new(0),
                anon: std::cell::Cell::new(0),
            }
        }

        /// One `resolve_registration` call where the plain named shape is refused `-1027` — the
        /// two-shape world every test written before 2026-09-10 assumed, kept as its own spelling
        /// so those tests still read as being about the app-service decision alone.
        fn resolve(&self, acb: bool, answer: Result<(), i32>) -> Result<&'static str, ()> {
            self.resolve_both(acb, answer, Err(-1027))
        }

        /// One `resolve_registration` call: each named shape answers its own `Ok(())` (granted)
        /// or `Err(code)` (refused with that hub code); the anonymous shape always registers. The
        /// identity `resolve_registration` reports beside the registration is dropped here — the
        /// tests below read it back off `super::identity()` — and graded directly by
        /// [`Attempts::resolve_full`]'s own caller.
        fn resolve_both(
            &self,
            acb: bool,
            app_answer: Result<(), i32>,
            named_answer: Result<(), i32>,
        ) -> Result<&'static str, ()> {
            self.resolve_full(acb, app_answer, named_answer).map(|(r, _)| r)
        }

        /// [`resolve_both`], keeping the identity beside the registration.
        fn resolve_full(
            &self,
            acb: bool,
            app_answer: Result<(), i32>,
            named_answer: Result<(), i32>,
        ) -> Result<(&'static str, super::Identity), ()> {
            super::resolve_registration(
                acb,
                || {
                    self.app.set(self.app.get() + 1);
                    app_answer
                        .map(|()| "app-service")
                        .map_err(|code| super::app_service_refusal_reason(code, "hub said no"))
                },
                || {
                    self.named.set(self.named.get() + 1);
                    named_answer
                        .map(|()| "named")
                        .map_err(|code| super::named_refusal_reason(code, "hub said no"))
                },
                || {
                    self.anon.set(self.anon.get() + 1);
                    Ok("anonymous")
                },
            )
        }
    }

    #[test]
    fn a_hub_that_grants_the_app_id_gives_the_keymanager_that_identity() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve(false, Ok(())), Ok("app-service"));
        assert_eq!((a.app.get(), a.anon.get()), (1, 0));
        assert_eq!(super::identity(), Some(super::Identity::AppId));
        assert!(super::registered_with_app_id());
        assert!(captured()
            .iter()
            .any(|l| l == "keymanager: identity=app_id (granted by the hub)"));
    }

    /// The ACB rule, and it is an ORDER rather than a preference: `session::load` runs before
    /// `player::acb_init`, so on a webOS 4 set the hub would very likely GRANT this name — and
    /// taking it is what would leave ACB without it. The assertion that matters is the zero.
    #[test]
    fn a_set_whose_acb_owns_the_app_id_is_never_asked_for_it() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve(true, Ok(())), Ok("anonymous"));
        assert_eq!((a.app.get(), a.anon.get()), (0, 1));
        assert_eq!(super::identity(), Some(super::Identity::Anonymous));
        assert!(!super::registered_with_app_id());
        assert!(captured().iter().any(
            |l| l == "keymanager: identity=anonymous (libAcbAPI holds the app id on this firmware)"
        ));
    }

    /// Both named shapes refused. The line names BOTH refusals, labelled by shape — a reporter's
    /// log has to say which of the two the hub was answering about, and one bare `-1027` cannot.
    #[test]
    fn a_refused_app_id_falls_back_to_the_anonymous_registration() {
        for (code, reason) in [
            (-1028, "the app id is already registered on this bus (-1028)"),
            (
                -1027,
                "the hub refused this registration shape for this executable's role (-1027)",
            ),
            (-9999, "app-service registration refused — hub said no"),
        ] {
            let _guard = crate::testlock::serial();
            reset();
            let a = Attempts::new();
            assert_eq!(a.resolve(false, Err(code)), Ok("anonymous"));
            assert_eq!((a.app.get(), a.named.get(), a.anon.get()), (1, 1, 1));
            assert_eq!(super::identity(), Some(super::Identity::Anonymous));
            assert!(!super::registered_with_app_id());
            assert!(!super::registered_with_name());
            let expected = format!(
                "keymanager: identity=anonymous (app-service: {reason}; named: this executable's \
                 role file does not allow the app id as a bus name (-1027))"
            );
            assert!(
                captured().iter().any(|l| l == &expected),
                "code {code} logged {:?}",
                captured()
            );
        }
    }

    /// **The new shape, and the one issue #76 hangs on**: the application-service form is refused
    /// and the plain named one is granted. The dev set's boot measurement is exactly the left half
    /// of this (`-1027` for the app-service API whatever name it is handed); the right half is
    /// what no television has answered yet.
    #[test]
    fn an_app_service_refusal_falls_to_the_plain_named_registration_before_anonymous() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(false, Err(-1027), Ok(())), Ok("named"));
        assert_eq!((a.app.get(), a.named.get(), a.anon.get()), (1, 1, 0));
        assert_eq!(super::identity(), Some(super::Identity::Named));
        assert!(super::registered_with_name());
        assert!(
            !super::registered_with_app_id(),
            "`app_id` keeps meaning the application-service form and nothing else"
        );
        assert!(
            captured().iter().any(|l| l
                == "keymanager: identity=named (the hub grants the app id as a plain bus name \
                    (app-service: the hub refused this registration shape for this executable's \
                    role (-1027)))"),
            "logged {:?}",
            captured()
        );
    }

    /// A `-1028` on the NAMED shape — somebody already holds that bus name — falls to anonymous,
    /// exactly as an app-service `-1028` does. This is the belt to `acb_holds_app_id`'s braces
    /// pointed at the second shape: it is what a set reaches if something claims the id between
    /// the gate and this call.
    #[test]
    fn a_named_registration_refused_1028_falls_back_to_anonymous() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(false, Err(-1027), Err(-1028)), Ok("anonymous"));
        assert_eq!((a.app.get(), a.named.get(), a.anon.get()), (1, 1, 1));
        assert_eq!(super::identity(), Some(super::Identity::Anonymous));
        assert!(!super::registered_with_app_id());
        assert!(!super::registered_with_name());
        assert!(
            captured().iter().any(|l| l
                == "keymanager: identity=anonymous (app-service: the hub refused this \
                    registration shape for this executable's role (-1027); named: the app id is \
                    already registered on this bus (-1028))"),
            "logged {:?}",
            captured()
        );
    }

    /// A process latched to `named` never asks for the application-service form again — that
    /// answer does not change inside a process, and re-asking would buy a refused registration on
    /// every connection. It DOES keep asking for its own name, which each connection releases.
    #[test]
    fn a_named_process_stops_asking_for_the_application_service_form() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(false, Err(-1027), Ok(())), Ok("named"));
        assert_eq!(a.resolve_both(false, Ok(()), Ok(())), Ok("named"));
        assert_eq!(a.resolve_both(false, Ok(()), Ok(())), Ok("named"));
        assert_eq!((a.app.get(), a.named.get(), a.anon.get()), (1, 3, 0));
        assert_eq!(super::identity(), Some(super::Identity::Named));
        assert_eq!(
            captured()
                .iter()
                .filter(|l| l.starts_with("keymanager: identity="))
                .count(),
            1
        );
    }

    /// Losing a granted NAME downgrades the process the way losing a granted app id does, on its
    /// own `identity-downgrade` line, and moves the report field with it.
    #[test]
    fn losing_a_granted_name_downgrades_the_process() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(false, Err(-1027), Ok(())), Ok("named"));
        assert!(super::registered_with_name());
        assert_eq!(a.resolve_both(false, Ok(()), Err(-1028)), Ok("anonymous"));
        assert!(!super::registered_with_name());
        assert!(
            !super::registered_with_app_id(),
            "the app-service form is not re-asked once the process is `named`"
        );
        assert_eq!(super::identity(), Some(super::Identity::Anonymous));
        let lines: Vec<String> = captured()
            .into_iter()
            .filter(|l| l.starts_with("keymanager: identity="))
            .collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].starts_with("keymanager: identity=anonymous (named: "), "{lines:?}");
    }

    /// **A SECOND downgrade is its own line** (review finding, 2026-09-11). The dedup key was the
    /// bare string `identity-downgrade` for every downgrade a process takes, so an `app_id` →
    /// `named` → `anonymous` walk logged the middle step and then went SILENT for the step that
    /// actually costs the install its stable owner — the one a reporter's log has to carry, since
    /// `registered_with_app_id`/`registered_with_name` move with it and a report must never claim
    /// an owner this process no longer has. Keying on the identity being settled is what makes
    /// each distinct verdict say itself once.
    #[test]
    fn a_second_downgrade_is_logged_rather_than_swallowed_by_the_first_ones_key() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(false, Ok(()), Ok(())), Ok("app-service"));
        // The hub takes the application-service form away; the plain name is still granted.
        assert_eq!(a.resolve_both(false, Err(-1027), Ok(())), Ok("named"));
        // …and then the name goes too — the step that leaves this process with no stable owner.
        assert_eq!(a.resolve_both(false, Err(-1027), Err(-1028)), Ok("anonymous"));
        assert!(!super::registered_with_app_id() && !super::registered_with_name());
        let lines: Vec<String> = captured()
            .into_iter()
            .filter(|l| l.starts_with("keymanager: identity="))
            .collect();
        assert_eq!(lines.len(), 3, "one line per settled identity: {lines:?}");
        assert!(lines[0].starts_with("keymanager: identity=app_id "), "{lines:?}");
        assert!(lines[1].starts_with("keymanager: identity=named "), "{lines:?}");
        assert!(lines[2].starts_with("keymanager: identity=anonymous "), "{lines:?}");
    }

    /// **The ACB gate covers BOTH named shapes.** `LSRegister(app_id)` asks for the very bus name
    /// `AcbAPI_initialize` is about to take, so a webOS 4 set must not try it either — the
    /// assertion that matters is the pair of zeroes.
    #[test]
    fn a_set_whose_acb_owns_the_app_id_is_asked_for_neither_named_shape() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve_both(true, Ok(()), Ok(())), Ok("anonymous"));
        assert_eq!((a.app.get(), a.named.get(), a.anon.get()), (0, 0, 1));
        assert_eq!(super::identity(), Some(super::Identity::Anonymous));
        assert!(!super::registered_with_name());
    }

    /// The two report bools come off ONE latch, so no process can ever claim both owner keys.
    #[test]
    fn both_owner_hints_cannot_be_true_at_once() {
        let _guard = crate::testlock::serial();
        for (app_answer, named_answer) in [
            (Ok(()), Ok(())),
            (Err(-1027), Ok(())),
            (Err(-1027), Err(-1028)),
        ] {
            reset();
            let a = Attempts::new();
            let _ = a.resolve_both(false, app_answer, named_answer);
            assert!(!(super::registered_with_app_id() && super::registered_with_name()));
        }
    }

    /// `named` is a first-class wire word: it round-trips through the envelope both ways, and the
    /// spelling is the same one every other surface records.
    #[test]
    fn the_named_identity_round_trips_through_the_envelope() {
        let sealed = Sealed {
            backend: Backend::Keymanager3,
            key: super::KEY_NAME.into(),
            iv: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
            data: "c2VjcmV0".into(),
            identity: super::Identity::Named,
        };
        let text = serde_json::to_string(&sealed).expect("an envelope serializes");
        assert!(text.contains(r#""identity":"named""#), "{text}");
        let back: Sealed = serde_json::from_str(&text).expect("and parses back");
        assert_eq!(back.identity, super::Identity::Named);
        assert_eq!(super::Identity::Named.code(), "named");
        assert_eq!(
            serde_json::to_value(super::Identity::Named).unwrap(),
            serde_json::json!("named")
        );
    }

    /// An envelope sealed as `named` is opened by ASKING for that name, never by substituting the
    /// stronger application-service identity — a different owner decrypts nothing — and a hub that
    /// will not grant it is the same TRANSIENT stage the `app_id` case is.
    #[test]
    fn an_envelope_sealed_as_named_asks_for_that_name_and_nothing_else() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": true, "handle": "never-reached"})),
        )]);
        // The hub grants the app-service form and refuses the name: the strongest identity going
        // is available and must still not be substituted.
        super::arm_identity_for_test(false, true, false);
        let sealed = Sealed {
            backend: Backend::Keymanager3,
            key: super::KEY_NAME.into(),
            iv: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
            data: "c2VjcmV0".into(),
            identity: super::Identity::Named,
        };
        let (plain, refusal) = super::open_checked(&sealed);
        let asked = super::requested_identities_for_test();
        let calls = platform::calls_for_test();
        platform::reset_for_test();

        assert!(plain.is_none());
        assert_eq!(
            refusal,
            Some(super::LastRefusal {
                stage: StorageStage::IdentityUnavailable,
                error_code: None,
            })
        );
        assert_eq!(asked, vec![Some(super::Identity::Named)]);
        assert!(calls.is_empty(), "no service call was made: {calls:?}");
    }

    /// `resolve_registration` reports which identity it produced, for every branch — the value
    /// `Client` stores and `modern_crypt` labels an envelope with, instead of re-reading the
    /// shared latch afterwards.
    #[test]
    fn an_identity_is_returned_beside_the_registration() {
        let _guard = crate::testlock::serial();
        for (acb, app_answer, named_answer, want_reg, want_id) in [
            (true, Ok(()), Ok(()), "anonymous", super::Identity::Anonymous),
            (false, Ok(()), Ok(()), "app-service", super::Identity::AppId),
            (false, Err(-1027), Ok(()), "named", super::Identity::Named),
            (
                false,
                Err(-1027),
                Err(-1028),
                "anonymous",
                super::Identity::Anonymous,
            ),
        ] {
            reset();
            let a = Attempts::new();
            assert_eq!(
                a.resolve_full(acb, app_answer, named_answer),
                Ok((want_reg, want_id))
            );
        }
        // And the fifth branch: a process already latched anonymous short-circuits, and still says
        // so rather than leaving the caller to guess.
        reset();
        let a = Attempts::new();
        let _ = a.resolve_both(false, Err(-1027), Err(-1028));
        assert_eq!(
            a.resolve_full(false, Ok(()), Ok(())),
            Ok(("anonymous", super::Identity::Anonymous))
        );
    }

    /// **An envelope records the identity ITS OWN CONNECTION registered under, not whatever the
    /// process latch says by the time the ciphertext comes back.**
    ///
    /// The latch is a process global that any concurrent connection may DOWNGRADE — losing a
    /// granted name moves it to `anonymous` and never back. Reading it after `begin`/`finish`, as
    /// this code did until 2026-09-10, means a seal performed under `app_id` can be written to
    /// disk labelled `anonymous`: the next launch then asks keymanager3 for the ANONYMOUS owner's
    /// key, gets a `-20030` tag mismatch, and — because that is a real service answer rather than
    /// an unobtainable name — grades the install a genuine refusal and downgrades it to plaintext
    /// for good. The label is what a later launch acts on, so it has to be the identity that was
    /// USED.
    ///
    /// **The red here was SIMULATED rather than historical** (`platform::script`'s
    /// `DOWNGRADE_AFTER`): the real trigger is a second connection on another thread, and the
    /// scripted backend and its hub are `thread_local!`, so a genuine race cannot be staged in
    /// this suite. Nothing but the latch is moved, which is precisely the variable the bug read.
    #[test]
    fn a_seal_records_the_identity_its_own_connection_used_not_the_latch_afterwards() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"})),
            ),
            ("finish", Ok(json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="}))),
        ]);
        super::arm_identity_for_test(false, true, false);
        // Another connection loses the name immediately after this operation's first call.
        platform::downgrade_identity_after_calls_for_test(1);

        let sealed = super::modern_crypt(b"plain", None, None, &mut None)
            .expect("the scripted backend seals");
        let latch = super::identity();
        platform::reset_for_test();

        assert_eq!(
            latch,
            Some(super::Identity::Anonymous),
            "the simulation must actually have moved the latch, or this test grades nothing"
        );
        assert_eq!(
            sealed.identity,
            super::Identity::AppId,
            "the envelope names the identity its own connection registered under"
        );
    }

    /// A seal on a set that grants only the plain name records `named`, so the launch after it
    /// opens by asking for the same one.
    #[test]
    fn a_seal_on_a_named_only_set_records_named() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            ("generateKey", Ok(json!({"returnValue": true}))),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"})),
            ),
            ("finish", Ok(json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="}))),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": super::b64::encode(b"plain")})),
            ),
        ]);
        super::arm_identity_for_test(false, false, true);
        let sealed = seal(b"plain").expect("the scripted backend round trips");
        platform::reset_for_test();
        assert_eq!(sealed.identity, super::Identity::Named);
    }

    /// A seal is three connections (`generateKey`, `begin`→`finish`, then the round-trip `open`)
    /// and a launch is many seals. The decision is taken ONCE: an anonymous process never asks
    /// the hub again, and a granted one keeps asking for the id it was given.
    #[test]
    fn the_identity_decision_is_taken_once_for_the_process() {
        let _guard = crate::testlock::serial();
        reset();
        let refused = Attempts::new();
        assert_eq!(refused.resolve(false, Err(-1027)), Ok("anonymous"));
        assert_eq!(refused.resolve(false, Err(-1027)), Ok("anonymous"));
        assert_eq!(refused.resolve(false, Ok(())), Ok("anonymous"));
        assert_eq!((refused.app.get(), refused.anon.get()), (1, 3));

        reset();
        let granted = Attempts::new();
        assert_eq!(granted.resolve(false, Ok(())), Ok("app-service"));
        assert_eq!(granted.resolve(false, Ok(())), Ok("app-service"));
        assert_eq!((granted.app.get(), granted.anon.get()), (2, 0));
        assert!(super::registered_with_app_id());
        // One line, not two: the second connection re-registers under an identity nothing changed.
        assert_eq!(
            captured()
                .iter()
                .filter(|l| l.starts_with("keymanager: identity="))
                .count(),
            1
        );
    }

    /// Losing a name that WAS granted moves the report field with it — a storage report must never
    /// claim an owner this process no longer has — and says so on its own line rather than being
    /// swallowed by the first line's dedup key.
    #[test]
    fn losing_a_granted_app_id_downgrades_the_process_and_the_report_field() {
        let _guard = crate::testlock::serial();
        reset();
        let a = Attempts::new();
        assert_eq!(a.resolve(false, Ok(())), Ok("app-service"));
        assert!(super::registered_with_app_id());
        assert_eq!(a.resolve(false, Err(-1028)), Ok("anonymous"));
        assert!(!super::registered_with_app_id());
        assert_eq!(super::identity(), Some(super::Identity::Anonymous));
        let lines: Vec<String> = captured()
            .into_iter()
            .filter(|l| l.starts_with("keymanager: identity="))
            .collect();
        assert_eq!(
            lines,
            vec![
                "keymanager: identity=app_id (granted by the hub)".to_string(),
                "keymanager: identity=anonymous (app-service: the app id is already registered on \
                 this bus (-1028); named: this executable's role file does not allow the app id \
                 as a bus name (-1027))"
                    .to_string(),
            ]
        );
    }

    /// **The envelope's identity is what an open asks for, and a hub that will not grant it is a
    /// TRANSIENT stage rather than a refusal.** The service is never reached — no call is made at
    /// all — so nothing here is evidence about the key, which is exactly why `plex::session` must
    /// not turn it into the cross-launch refused marker.
    #[test]
    fn an_unobtainable_envelope_identity_is_transient_and_makes_no_call() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": true, "handle": "never-reached"})),
        )]);
        super::arm_identity_for_test(false, false, false);
        let sealed = Sealed {
            backend: Backend::Keymanager3,
            key: super::KEY_NAME.into(),
            iv: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
            data: "c2VjcmV0".into(),
            identity: super::Identity::AppId,
        };
        let (plain, refusal) = super::open_checked(&sealed);
        let asked = super::requested_identities_for_test();
        let calls = platform::calls_for_test();
        platform::reset_for_test();

        assert!(plain.is_none());
        assert_eq!(
            refusal,
            Some(super::LastRefusal {
                stage: StorageStage::IdentityUnavailable,
                error_code: None,
            })
        );
        assert_eq!(asked, vec![Some(super::Identity::AppId)]);
        assert!(calls.is_empty(), "no service call was made: {calls:?}");
    }

    /// **The OPEN side honours the ACB gate too** (review finding, 2026-09-11). The gate is a
    /// DESIGN order rather than a hub verdict — on a set where `libAcbAPI` is about to take the app
    /// id, this module must not ask for either named shape, because winning that race costs the
    /// television its video plane (`resolve_registration`'s doc). `Client::new(None)` obeyed it and
    /// `Client::new(Some(AppId|Named))` did not, so an envelope recorded as named/app_id — which is
    /// exactly what a set that ONCE granted the name leaves behind — sent this launch to the hub
    /// for that very name anyway. The hub granting it is the bad case, not the good one, which is
    /// why both shapes are scripted GRANTED here: a gate that only holds where the hub refuses is
    /// not a gate at all.
    #[test]
    fn an_envelope_sealed_under_a_name_is_not_opened_where_acb_will_hold_it() {
        let _guard = crate::testlock::serial();
        for identity in [super::Identity::Named, super::Identity::AppId] {
            reset();
            platform::script_for_test(vec![(
                "begin",
                Ok(json!({"returnValue": true, "handle": "never-reached"})),
            )]);
            // A hub that WOULD grant both shapes, on a set whose ACB holds the app id.
            super::arm_identity_for_test(true, true, true);
            let sealed = Sealed {
                backend: Backend::Keymanager3,
                key: super::KEY_NAME.into(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
                data: "c2VjcmV0".into(),
                identity,
            };
            let (plain, refusal) = super::open_checked(&sealed);
            let calls = platform::calls_for_test();
            platform::reset_for_test();

            assert!(plain.is_none(), "{identity:?}");
            assert_eq!(
                refusal,
                Some(super::LastRefusal {
                    stage: StorageStage::IdentityUnavailable,
                    error_code: None,
                }),
                "the name is unavailable BY DESIGN here, which is transient — the envelope is \
                 kept and no refused marker may be written ({identity:?})"
            );
            assert!(calls.is_empty(), "no service call was made: {calls:?}");
        }
    }

    /// A seal records the identity it registered under — the field a later launch opens by.
    #[test]
    fn a_seal_records_the_identity_it_registered_under() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            ("generateKey", Ok(json!({"returnValue": true}))),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"})),
            ),
            ("finish", Ok(json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="}))),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": super::b64::encode(b"plain")})),
            ),
        ]);
        super::arm_identity_for_test(false, true, false);
        let sealed = seal(b"plain").expect("the scripted backend round trips");
        platform::reset_for_test();
        assert_eq!(sealed.identity, super::Identity::AppId);
    }

    /// An envelope written before the field existed is anonymous, which is what those builds were.
    #[test]
    fn an_envelope_without_an_identity_field_reads_as_anonymous() {
        let sealed: Sealed = serde_json::from_str(
            r#"{"backend":"keymanager3","key":"k","iv":"i","data":"d"}"#,
        )
        .expect("an 0.6.2 envelope still parses");
        assert_eq!(sealed.identity, super::Identity::Anonymous);
        assert_eq!(
            serde_json::to_value(super::Identity::AppId).unwrap(),
            serde_json::json!("app_id"),
            "the wire word is the same one every other surface records"
        );
    }

    /// A process that never registered reports `false` — the same answer an anonymous one gives,
    /// and the same answer the closed constant this replaced always gave.
    #[test]
    fn a_process_that_never_registered_claims_no_app_id() {
        let _guard = crate::testlock::serial();
        reset();
        assert_eq!(super::identity(), None);
        assert!(!super::registered_with_app_id());
    }

    /// The anonymous shape's own failure is the caller's failure: `resolve_registration` decides
    /// an identity, it does not invent a registration.
    #[test]
    fn an_anonymous_registration_failure_reaches_the_caller() {
        let _guard = crate::testlock::serial();
        reset();
        let got: Result<(&str, super::Identity), &str> = super::resolve_registration(
            true,
            || Ok::<&str, String>("app-service"),
            || Ok::<&str, String>("named"),
            || Err("no bus"),
        );
        assert_eq!(got, Err("no bus"));
        assert!(!super::registered_with_app_id());
    }

    #[test]
    fn base64_round_trips_binary_and_padding() {
        for bytes in [
            &b""[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &[0, 255, 1, 2],
        ] {
            assert_eq!(b64::decode(&b64::encode(bytes)).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn removing_the_key_invalidates_the_backend_cache() {
        let _guard = crate::testlock::serial();
        SELECTED.store(MODERN, Ordering::Relaxed);
        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNKNOWN);
    }

    #[test]
    fn unauthenticated_legacy_ciphertext_is_never_opened() {
        let sealed = Sealed {
            backend: Backend::PalmKeymanager,
            key: super::KEY_NAME.into(),
            iv: "legacy-iv".into(),
            data: b64::encode(b"attacker-controlled ciphertext"),
            identity: crate::keymanager::Identity::Anonymous,
        };
        assert!(open(&sealed).is_none());
    }

    fn generate_key_ok() -> (&'static str, Result<serde_json::Value, ()>) {
        ("generateKey", Ok(json!({"returnValue": true})))
    }

    /// Issue #76's identity decider: a `generateKey` that SUCCEEDS outright — a brand new key —
    /// records [`super::KeyOutcome::Created`] and logs exactly one line, even across two seals in
    /// the same process (a healthy install reseals on every profile switch).
    #[test]
    fn a_successful_generate_key_records_created_and_logs_once() {
        let _guard = crate::testlock::serial();
        reset();
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv}))),
            ("finish", Ok(json!({"returnValue": true, "output": b64::encode(b"ct")}))),
            generate_key_ok(),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            ("finish", Ok(json!({"returnValue": true, "output": b64::encode(b"issue-76 plaintext")}))),
        ]);

        assert!(seal(b"issue-76 plaintext").is_some());
        assert_eq!(super::last_key_outcome(), Some(super::KeyOutcome::Created));
        let created_lines = captured()
            .iter()
            .filter(|l| l.contains("generateKey -> created"))
            .count();
        assert_eq!(created_lines, 1, "one line per process, not per generateKey call");
        platform::reset_for_test();
    }

    /// The other half: `generateKey` answering `-10002` ("key already exists") records
    /// [`super::KeyOutcome::Existed`] — the shape a launch that follows a launch which already
    /// sealed something takes when the SAME registration is recognized as the key's owner.
    #[test]
    fn a_minus_10002_generate_key_records_existed() {
        let _guard = crate::testlock::serial();
        reset();
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            (
                "generateKey",
                Ok(json!({"returnValue": false, "errorCode": -10002, "errorText": "key already exists"})),
            ),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv}))),
            ("finish", Ok(json!({"returnValue": true, "output": b64::encode(b"ct")}))),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            ("finish", Ok(json!({"returnValue": true, "output": b64::encode(b"issue-76 plaintext")}))),
        ]);

        assert!(seal(b"issue-76 plaintext").is_some());
        assert_eq!(super::last_key_outcome(), Some(super::KeyOutcome::Existed));
        assert!(
            captured().iter().any(|l| l.contains("generateKey -> existed")),
            "expected the existed log line, got: {:?}",
            captured()
        );
        platform::reset_for_test();
    }

    /// A process that has never called `generateKey` at all (no key manager, or an open-only
    /// path) reports no outcome — `None` must read as "unknown", never as either closed value.
    #[test]
    fn a_process_that_never_generated_a_key_has_no_outcome() {
        let _guard = crate::testlock::serial();
        reset();
        assert_eq!(super::last_key_outcome(), None);
    }

    /// (a) A backend that seals fine but cannot open what it just sealed must not be trusted —
    /// `seal` returns `None`, the backend is marked unavailable, and the refusal is logged once.
    #[test]
    fn seal_refuses_a_backend_that_cannot_open_its_own_envelope() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": "key not found"
                })),
            ),
        ]);

        let result = seal(plain);

        assert!(result.is_none(), "a backend that cannot open its own envelope must not be trusted");
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        let lines = captured();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("begin(decrypt) refused errorCode=-10001"))
                .count(),
            1,
            "expected exactly one refusal line, got: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("could not open its own envelope")),
            "expected the round-trip failure line, got: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (b) A backend that genuinely round-trips is trusted: `seal` returns `Some`, and `open`
    /// recovers the original bytes from that envelope.
    #[test]
    fn seal_trusts_a_backend_that_round_trips() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
            ),
        ]);

        let sealed = seal(plain).expect("a round-tripping backend must be trusted");
        assert_eq!(SELECTED.load(Ordering::Relaxed), MODERN);
        assert_eq!(sealed.backend, Backend::Keymanager3);

        // `open` mints its own fresh registration, exactly like a later boot would.
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": true, "handle": "h-dec2"})),
        ), (
            "finish",
            Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
        )]);
        assert_eq!(open(&sealed).as_deref(), Some(&plain[..]));
        platform::reset_for_test();
    }

    /// (c) A `finish` reply that succeeded but carries no `output` is a missing-field failure,
    /// not a silent `None` — it must be logged, and `seal` must still refuse cleanly.
    #[test]
    fn seal_logs_a_finish_reply_with_no_output() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            ("finish", Ok(json!({"returnValue": true}))),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        let lines = captured();
        assert!(
            lines
                .iter()
                .any(|l| l == "keymanager: finish(encrypt) reply had no output"),
            "expected the missing-field line, got: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (c2) Issue #76 review: `finish`'s own request carries the base64 session plaintext or
    /// ciphertext, so a service that echoes any of its input back in `errorText` must never see
    /// that string reach the log — not truncated, not partially, not at all. Only `errorCode` may
    /// appear.
    #[test]
    fn finish_refusal_never_logs_errortext_even_if_the_service_echoes_the_request() {
        let _guard = crate::testlock::serial();
        reset();
        let leaked_secret = b64::encode(b"X-Plex-Token=super-secret-account-token-value");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -20030,
                    "errorText": format!("verification failed for data={leaked_secret}")
                })),
            ),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        let lines = captured();
        assert!(
            lines.iter().any(|l| l == "keymanager: finish(encrypt) refused errorCode=-20030"),
            "expected a code-only refusal line, got: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains(&leaked_secret)),
            "the service's echoed payload must never reach the log: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (c3) `begin`'s request carries no secret, so its `errorText` may still be logged — but a
    /// reply shaped like it is carrying one (a long base64-alphabet run) is dropped outright, and
    /// an ordinary one is bounded rather than trusted to stay short forever.
    #[test]
    fn begin_refusal_drops_a_long_base64_looking_errortext_but_keeps_an_ordinary_one() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": "key not found"
                })),
            ),
        ]);
        assert!(seal(b"issue-76 plaintext").is_none());
        assert!(
            captured()
                .iter()
                .any(|l| l == "keymanager: begin(encrypt) refused errorCode=-10001 (key not found)"),
            "an ordinary short errorText is kept: {:?}",
            captured()
        );
        platform::reset_for_test();

        reset();
        let base64_shaped = b64::encode(b"this looks exactly like an encoded secret payload");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({
                    "returnValue": false,
                    "errorCode": -10001,
                    "errorText": base64_shaped
                })),
            ),
        ]);
        assert!(seal(b"issue-76 plaintext").is_none());
        let lines = captured();
        assert!(
            lines.iter().any(|l| l == "keymanager: begin(encrypt) refused errorCode=-10001"),
            "a base64-shaped errorText is dropped to code-only: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains(&base64_shaped)),
            "must never log the base64-shaped text: {lines:?}"
        );
        platform::reset_for_test();
    }

    /// (f) Issue #76 review: once a backend is trusted (`SELECTED == MODERN`), `seal` must not pay
    /// a second decrypt round trip on every later save — that doubles keymanager3's LS2 budget on
    /// a path that holds the auth and session locks. Script only ONE decrypt reply (consumed by
    /// the promotion's `round_trips` check) and two encrypts; if the second `seal` attempted to
    /// re-verify, the exhausted decrypt queue would make `open` fail and `seal` return `None`.
    #[test]
    fn seal_does_not_re_verify_on_the_cached_modern_fast_path() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc1", "iv": iv.clone()})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-1")})),
            ),
            // The promotion's own round-trip verify — the only decrypt this test scripts.
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(plain)})),
            ),
            // A second encrypt for the fast-path call below. No second decrypt is scripted.
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc2", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-2")})),
            ),
        ]);

        assert!(seal(plain).is_some(), "the promotion round trip succeeds");
        assert_eq!(SELECTED.load(Ordering::Relaxed), MODERN);

        let second = seal(plain);
        assert!(
            second.is_some(),
            "the fast path must not fail merely because no second decrypt was scripted"
        );
        assert_eq!(second.unwrap().data, b64::encode(b"ciphertext-2"));
        platform::reset_for_test();
    }

    /// (d) `begin`'s request never carries the undocumented `mac_length` field.
    #[test]
    fn begin_request_carries_no_mac_length() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
        ]);

        let _ = seal(b"issue-76 plaintext");

        let begin_calls: Vec<_> = platform::calls_for_test()
            .into_iter()
            .filter(|(uri, _)| uri.ends_with("/begin"))
            .collect();
        assert!(!begin_calls.is_empty(), "expected at least one begin call");
        for (_, payload) in begin_calls {
            assert!(
                !payload.contains("mac_length"),
                "begin request still carries mac_length: {payload}"
            );
        }
        platform::reset_for_test();
    }

    /// (e) Removing the key resets the backend cache (kept from the original suite, now
    /// exercised through the shared `reset` fixture too).
    #[test]
    fn remove_resets_selected_via_shared_fixture() {
        let _guard = crate::testlock::serial();
        reset();
        SELECTED.store(MODERN, Ordering::Relaxed);
        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNKNOWN);
    }

    /// (g) Issue #76 storage telemetry: a `begin(decrypt)` refusal reached through `seal`'s own
    /// round-trip proof publishes the exact stage and service error code — the shape
    /// `seal_refuses_a_backend_that_cannot_open_its_own_envelope` above exercises without asserting
    /// on it.
    #[test]
    fn last_refusal_publishes_the_stage_and_code_of_a_begin_decrypt_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        assert_eq!(super::last_refusal(), None, "clean slate");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            (
                "begin",
                Ok(json!({"returnValue": false, "errorCode": -10001, "errorText": "key not found"})),
            ),
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::BeginDecrypt,
                error_code: Some(-10001),
            })
        );
        platform::reset_for_test();
    }

    /// (h) A missing-field reply (no `errorCode` at all) publishes its stage with no code, rather
    /// than being invisible to the telemetry wiring.
    #[test]
    fn last_refusal_records_a_missing_field_with_no_error_code() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": b64::encode(b"0123456789ab")})),
            ),
            ("finish", Ok(json!({"returnValue": true}))), // no output
        ]);

        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::FinishEncrypt,
                error_code: None,
            })
        );
        platform::reset_for_test();
    }

    /// (m) A firmware with NO keymanager3 at all — the dev set (webOS 4.10) is one; the service
    /// is documented for TV 24+ but present on earlier firmware too (issue #76's reporters carry
    /// it on 5.6.2, 9.2.2 and 10.3.1) — is answered by the LS2 hub itself, at once, with `errorCode:-1` and
    /// `"Service does not exist: com.webos.service.keymanager3."` (measured on the webOS 4.10 dev
    /// set, 2026-09-10, `luna-send` as root). That is the ABSENCE of a service, not a refusal by
    /// one: it must publish no `last_refusal` (or every such install's first save would send a
    /// StorageError), and it is not evidence for anything.
    #[test]
    fn an_absent_service_is_not_a_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![(
            "generateKey",
            Ok(json!({
                "returnValue": false, "errorCode": -1,
                "errorText": "Service does not exist: com.webos.service.keymanager3."
            })),
        )]);
        assert!(seal(b"issue-76 plaintext").is_none());
        assert_eq!(SELECTED.load(Ordering::Relaxed), UNAVAILABLE);
        assert_eq!(super::last_refusal(), None, "an absent service refused nothing");
        platform::reset_for_test();
    }

    /// (i) A genuine round-trip MISMATCH — `open` succeeds but hands back different bytes than
    /// what was sealed, never a service refusal — publishes `RoundtripMismatch` with no code, since
    /// no service reply carried an `errorCode` for this failure.
    #[test]
    fn last_refusal_records_a_genuine_roundtrip_mismatch() {
        let _guard = crate::testlock::serial();
        reset();
        let plain = b"issue-76 plaintext";
        let iv = b64::encode(b"0123456789ab");
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": true, "handle": "h-enc", "iv": iv})),
            ),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"ciphertext-placeholder")})),
            ),
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            (
                "finish",
                Ok(json!({"returnValue": true, "output": b64::encode(b"not the original plaintext")})),
            ),
        ]);

        assert!(seal(plain).is_none());
        assert_eq!(
            super::last_refusal(),
            Some(super::LastRefusal {
                stage: StorageStage::RoundtripMismatch,
                error_code: None,
            })
        );
        platform::reset_for_test();
    }

    /// (k) Issue #76 review, field report case (1): a `begin(decrypt)` refusal with NO `errorCode`
    /// at all must still return `Some(refusal)` from [`super::open_checked`] — before this fix it
    /// answered `(None, None)`, indistinguishable from "nothing was attempted", which is exactly
    /// what let `plex::session`'s cross-launch marker go unarmed for a whole extra launch.
    #[test]
    fn open_checked_records_a_refusal_with_no_error_code() {
        let _guard = crate::testlock::serial();
        reset();
        let sealed = Sealed {
            backend: Backend::Keymanager3,
            key: super::KEY_NAME.into(),
            iv: b64::encode(b"0123456789ab"),
            data: b64::encode(b"ciphertext"),
            identity: crate::keymanager::Identity::Anonymous,
        };
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": false, "errorText": "nope"})),
        )]);
        let (plain, refusal) = super::open_checked(&sealed);
        platform::reset_for_test();
        assert!(plain.is_none());
        assert_eq!(
            refusal,
            Some(super::LastRefusal {
                stage: StorageStage::BeginDecrypt,
                error_code: None,
            }),
            "a codeless refusal must still be evidence, not silence"
        );
    }

    /// (l) Issue #76 review, field report case (2): a `finish(decrypt)` that SUCCEEDS but answers
    /// with an `output` this build cannot base64-decode must be recorded as `FinishDecrypt` — before
    /// this fix `open_checked` ran `b64::decode` after `modern_crypt` had already returned, so the
    /// decode failure was invisible to the caller deciding whether to arm the cross-launch marker.
    #[test]
    fn open_checked_records_a_finish_decrypt_output_that_is_not_base64() {
        let _guard = crate::testlock::serial();
        reset();
        let sealed = Sealed {
            backend: Backend::Keymanager3,
            key: super::KEY_NAME.into(),
            iv: b64::encode(b"0123456789ab"),
            data: b64::encode(b"ciphertext"),
            identity: crate::keymanager::Identity::Anonymous,
        };
        platform::script_for_test(vec![
            ("begin", Ok(json!({"returnValue": true, "handle": "h-dec"}))),
            ("finish", Ok(json!({"returnValue": true, "output": "!!!!"}))),
        ]);
        let (plain, refusal) = super::open_checked(&sealed);
        platform::reset_for_test();
        assert!(plain.is_none());
        assert_eq!(
            refusal,
            Some(super::LastRefusal {
                stage: StorageStage::FinishDecrypt,
                error_code: None,
            })
        );
    }

    /// (m) Issue #76 review, field report case (3): a non-UTF-8 decrypt input aborts the handle it
    /// just opened (instead of leaking it) and records `BeginDecrypt` — proven by calling
    /// `modern_crypt` directly, since a real `Sealed.data` (a `String`) can never carry invalid
    /// UTF-8 through the public `open`/`open_checked` door.
    #[test]
    fn modern_crypt_aborts_the_handle_on_non_utf8_decrypt_input() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![(
            "begin",
            Ok(json!({"returnValue": true, "handle": "h-dec", "iv": b64::encode(b"0123456789ab")})),
        )]);
        let invalid_utf8: &[u8] = &[0xff, 0xfe, 0xfd];
        let mut local = None;
        let result = super::modern_crypt(
            invalid_utf8,
            Some("iv"),
            Some(super::Identity::Anonymous),
            &mut local,
        );
        let calls = platform::calls_for_test();
        platform::reset_for_test();
        assert!(result.is_none());
        assert_eq!(
            local,
            Some(super::LastRefusal {
                stage: StorageStage::BeginDecrypt,
                error_code: None,
            })
        );
        assert!(
            calls.iter().any(|(uri, _)| uri.ends_with("/abort")),
            "the handle `begin` opened must be aborted rather than leaked, got {calls:?}"
        );
    }

    /// (n) A `PalmKeymanager` envelope is never opened (no authenticated-encryption primitive), and
    /// [`super::open_checked`] must say so as evidence (`EnvelopeLocked`) rather than silence.
    #[test]
    fn open_checked_records_a_palm_keymanager_envelope_as_envelope_locked() {
        let sealed = Sealed {
            backend: Backend::PalmKeymanager,
            key: super::KEY_NAME.into(),
            iv: "legacy-iv".into(),
            data: b64::encode(b"attacker-controlled ciphertext"),
            identity: crate::keymanager::Identity::Anonymous,
        };
        let (plain, refusal) = super::open_checked(&sealed);
        assert!(plain.is_none());
        assert_eq!(
            refusal,
            Some(super::LastRefusal {
                stage: StorageStage::EnvelopeLocked,
                error_code: None,
            })
        );
    }

    /// (j) `remove` (sign-out) clears the last refusal — a stale verdict about the account that
    /// just left must never be attached to a report about the one that signs in next.
    #[test]
    fn removing_the_key_clears_the_last_refusal() {
        let _guard = crate::testlock::serial();
        reset();
        platform::script_for_test(vec![
            generate_key_ok(),
            (
                "begin",
                Ok(json!({"returnValue": false, "errorCode": -10001, "errorText": "key not found"})),
            ),
        ]);
        assert!(seal(b"plain").is_none());
        assert!(super::last_refusal().is_some());
        platform::reset_for_test();

        remove(&Backend::Keymanager3, super::KEY_NAME);
        assert_eq!(super::last_refusal(), None);
    }

    /// Issue #76: the fake `keymanager3` double must have no trace in a `--no-default-features`
    /// (release) build. It is checked by source inspection rather than a build matrix here — the
    /// project's own `release-config-check` hook and CI both compile
    /// `--no-default-features` on every change already; this test pins the ONE `#[cfg]` line that
    /// makes that compilation drop the fake module entirely, so a later edit that moved or removed
    /// the guard fails loudly here instead of only showing up as a much bigger diff in a release
    /// audit.
    #[test]
    fn the_fake_keymanager_service_is_gated_on_devtriggers() {
        let src = include_str!("keymanager.rs");
        let marker = "pub(crate) mod fake {";
        let idx = src
            .find(marker)
            .expect("the fake keymanager service module must exist");
        let before = &src[..idx];
        let guard_line = before
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        assert_eq!(
            guard_line.trim(),
            "#[cfg(feature = \"devtriggers\")]",
            "the fake keymanager service module must be gated on the devtriggers feature, or a \
             --no-default-features (release) build would still carry it"
        );
    }
}
