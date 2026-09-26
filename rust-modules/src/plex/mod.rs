//! Typed Plex API layer — one method per PMS operation the app uses.
//!
//! Replaces every hand-built Plex path/query string in the app with a typed `Client`
//! method (see `docs/plex-api-design.md` + `docs/plex-api-catalog.md`). Percent-encoding
//! (`crate::pms::urlenc_str`), the `X-Plex-Token` injection, and the origin-aware HTTP(S)
//! transport (`crate::http`) are centralised in `client.rs`, so no op file can bypass them.
//! Response bodies deserialize into `serde` DTOs (`models.rs`).
//!
//! The WHOLE surface is live: `plex::install` is called at boot/login (`app.rs`, `auth.rs`);
//! the read layer (`pms`/`metadata`/`posters`/`detail`) and the playback layer (`route.rs`
//! decision/start/stop/selection, PlayQueue/identity, and the player's `/:/timeline`) all go
//! through `client()` (history: `docs/plex-api-migration.md`). The module-wide allow covers
//! the ops written ahead of a UI feature (browse/leaves — no callers yet). `search` had one
//! too until the Search screen landed and `crate::search` began calling it; the name was left
//! in this list for several commits afterwards, long enough for a test-manifest note to cite
//! it as the check for whether that screen existed.
#![allow(dead_code)]

mod client;
pub(crate) mod identity; // ONE X-Plex-* identity for both transports (plex.tv headers + PMS query)
mod models;
mod params;
// WHICH servers exist and which one is current. `client()`/`client_opt()` live here now (they
// mean "the current server"); `client.rs` is just the type. See its module doc for why the hot
// path is an atomic pointer table rather than a lock.
mod servers;

// Op files below only add `impl Client { … }` blocks (Rust allows multiple impls of one
// type across a crate) — declared here so those methods compile onto `Client`.
mod hubs;
mod library;
/// Whole-file text subtitles are bounded before transport allocation and parsing.
pub(crate) const SIDECAR_MAX_BYTES: usize = 4 * 1024 * 1024;
mod timeline;
mod transcoder;

// The server's self-description (version + Plex Pass tristate), refreshed by `install` on every
// session path. pub(crate) because it is a DIAGNOSTICS surface with readers outside this layer
// (`app::diagnostics`, `player::error_shape`) — and deliberately nothing else: see its module doc for
// why subscription state must never become a routing input.
pub(crate) mod serverinfo;

// The plex.tv ACCOUNT surface (login/discovery/home-users) — a separate service from the PMS
// `Client` above (own host + HTTPS transport), so it's its own `AccountClient`, not an `impl`.
// pub(crate): the login/boot code (app.rs) + the UI screens construct these directly.
pub(crate) mod account;
pub(crate) mod session;

// WHERE a server is, as one value: scheme + host + port, parsed from a URL and never assembled
// from an address. The type every layer below the transport passes instead of a bare
// `(host, port)` pair — read its module doc before adding a call site, in particular the bracket
// invariant that keeps `host()` (the resolver's node) and `authority()` (URL serialization) from
// being confused for one another.
pub(crate) mod origin;

// Which of a server's advertised addresses are worth dialling, and in what order. PURE policy over
// an `account::Resource` — no socket, no thread — so the rules that decide reachability are gradeable
// on the host, which is the only tier that can grade them at all: the failures they prevent are an
// 8-second timeout and a probe that answers as the wrong machine.
pub(crate) mod probe;

// **May a credential go to this origin** — the one authority every credential consumer asks, and
// the consented, network- and identity-bound plaintext grants behind its answer (see its doc).
pub(crate) mod grant;

// Which libraries feed Home, PER PROFILE. Pure policy over the section table and one profile's
// persisted answer — no store, no screen — so the household default, the recorded answer and the
// never-empty floor are graded on the host rather than observed on a television.
pub(crate) mod pins;

// The plex.tv METADATA PROVIDER (`discover.provider.plex.tv`) — a third service, but one that
// shares the account API's transport + identity headers exactly, so it only adds an
// `impl AccountClient` block (same pattern as the PMS op files above).
pub(crate) mod discover;

// The re-exports are the public surface the call sites import.
pub(crate) use client::ArtFetch;
pub(crate) use client::JsonDeadlineOutcome;
// The one link/IP ⇄ u8 encode/decode pair — shared by `Client`'s own atomics and
// `player::report`'s packed attempt snapshot, so the two never keep a private copy each.
pub(crate) use client::{decode_ip, decode_link, encode_ip, encode_link};
#[allow(unused_imports)]
pub use client::{Client, IpVersion, StreamUrl};
// WHERE a server is, as one value. `Origin` is what `register_origin`/`install` take and what a
// `Client` carries; `Scheme` is re-exported beside it because `dev::DevServer` deserializes one
// straight out of the `plxnative-servers` trigger.
#[allow(unused_imports)]
pub use origin::{plex_direct_literal, url_host, CredentialPolicy, Origin, ResolvePin, Scheme};
// The registry surface. `client`/`client_opt`/`install` keep the exact signatures they had as
// singleton accessors, so every call site outside `plex/` reads unchanged; `client_for`,
// `register`, `set_current` and `ServerId` are the multi-server additions.
#[allow(unused_imports)]
pub use servers::{
    client, client_for, client_opt, commit_if_current, commit_reachability_if_current,
    count as server_count, current as current_server, describe as describe_server,
    describe_name as describe_server_name, facts as server_facts, ids as server_ids, install,
    facts_gen as server_facts_gen, is_household, owner_credit,
    probe_result as server_probe_result, publish_probe_result, register,
    roster_gen as server_roster_gen, same_item, set_current, Grant, GrantEvidence,
    ServerFacts,
    ServerId, MAX_SERVERS,
};
// Sign-out. `pub(crate)` like the function itself: retiring the whole table is `auth::sign_out`'s
// to call and nothing else's — a caller that merely wants to stop using a server wants
// `set_current`, and one that wants to forget a share wants plex.tv to stop granting it.
#[allow(unused_imports)]
pub(crate) use servers::{finish_profile_switch, id_of_machine, revoke_all, revoke_for_profile_switch};
// The registry as a TEST FIXTURE, for suites outside this module (`route.rs` grades which server a
// `/:/timeline` POST reaches). `register_for_test` skips the `session::load` the public `register`
// does — that call mints and PERSISTS a device uuid, which a host test has no business writing —
// and `reset_servers_for_test` is what keeps the table a per-test fixture instead of a growing
// process-global holding clients whose loopback ports closed when their test returned. Both must be
// called under `crate::testlock::serial`.
#[allow(unused_imports)]
pub use models::*;
#[allow(unused_imports)]
pub use params::*;
#[cfg(test)]
pub(crate) use servers::{
    register_pinned_with_client_id_and_policy, register_with_client_id as register_for_test,
    reset_for_test as reset_servers_for_test, write_held_for_test,
};
// Only reached from `auth::register_observed_origin`'s `#[cfg(test)]` arm and from test files, so
// a plain `cargo check --lib` (which builds no test code at all) sees no caller.
#[allow(unused_imports)]
pub(crate) use servers::register_pinned_with_client_id;

/// Evidence from the authenticated admission request made when an identity winner is considered
/// for primary. Reaching `/identity` proves the machine; this proves the exact per-(profile,
/// machine) token can browse it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointAdmission {
    Usable,
    Refused(i32),
    Http(i32),
    Timeout,
    Transport,
    InsecureOnly,
    Malformed,
}

fn classify_endpoint_response(status: i32, parsed: bool) -> EndpointAdmission {
    if matches!(status, 401 | 403) {
        EndpointAdmission::Refused(status)
    } else if !(200..300).contains(&status) {
        EndpointAdmission::Http(status)
    } else if parsed {
        EndpointAdmission::Usable
    } else {
        EndpointAdmission::Malformed
    }
}

#[cfg(test)]
pub(crate) fn endpoint_admission_from_reply(status: i32, body: &[u8]) -> EndpointAdmission {
    classify_endpoint_response(status, serde_json::from_slice::<models::Envelope>(body).is_ok())
}

/// Validate a freshly reached source with `GET /library/sections`, using the source's own token,
/// origin and resolve pin through the ordinary PMS client/transport boundary.
pub(crate) fn admit_source_until(source: &session::SourceRef, client_id: &str,
    overall_deadline: std::time::Instant) -> EndpointAdmission {
    let Some(origin) = source.origin() else { return EndpointAdmission::Transport };
    if !grant::credential_allowed_for(&source.machine_id, &origin) {
        return EndpointAdmission::InsecureOnly;
    }
    let client = Client::new(ServerId::UNSET, &source.machine_id, origin, &source.token, client_id)
        .with_resolve_pin(source.resolve_pin());
    let now = std::time::Instant::now();
    if now >= overall_deadline { return EndpointAdmission::Timeout; }
    let attempt_budget = if source.tier == Some(probe::Location::Local) {
        std::time::Duration::from_secs(5)
    } else {
        std::time::Duration::from_secs(10)
    };
    let deadline = now
        .checked_add(attempt_budget)
        .unwrap_or(overall_deadline)
        .min(overall_deadline);
    match client.get_json_with_headers_until("/library/sections", &[], deadline) {
        JsonDeadlineOutcome::Response { reply, parsed } =>
            classify_endpoint_response(reply.status, parsed.is_some()),
        JsonDeadlineOutcome::Deadline => EndpointAdmission::Timeout,
        JsonDeadlineOutcome::Transport => EndpointAdmission::Transport,
    }
}

pub(crate) fn admit_source(source: &session::SourceRef, client_id: &str) -> EndpointAdmission {
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(10))
        .unwrap_or_else(std::time::Instant::now);
    admit_source_until(source, client_id, deadline)
}
// #95 step 8: connection facts applied AT registration, atomically with the registry write. See
// `servers::ConnectionFacts`'s doc for why `None` means "leave unchanged" rather than "unknown".
// `register_origin` took the `ConnectionFacts` parameter directly rather than keeping a
// connection-less twin beside it — every production caller already knows a tier (or `None`) at
// registration time. `register_captured_origin_with_connection` is reached only from production
// call sites that are themselves feature-conditional today; `#[allow]` keeps a
// `--no-default-features` `cargo check` (which builds no test code) from flagging it as dead while
// it still has a real, if narrower, production caller.
#[allow(unused_imports)]
pub(crate) use servers::{
    register_captured_origin_with_connection, register_origin, ConnectionFacts,
};
// The projected play-queue row + the identity rule that locates one: op-file items rather than
// wire DTOs, so they are re-exported by name (route.rs names the row in `Plan`/`QueueInfo` — the
// rest of `timeline` is reached through `Client`'s methods and needs none).
#[allow(unused_imports)]
pub use timeline::{queue_index_of, QueueRow};
// DP_AUDIO_CODECS rides along for `devcaps`, which intersects it with the device's own codec
// table — the caps snapshot is what `is_dp_audio` and the profile string then both read.
// `DP_SUBTITLE_CODECS` / `is_dp_subtitle` are the subtitle twin: the profile's `subtitleCodec=`
// list and route's MDE `subtitleStreamID` gate, so a selected PGS cannot be advertised in one
// and omitted from the other.
// `link_policy` is the other gate on the same decision: `is_dp_audio` asks what the PIPELINE can
// decode, `link_policy` asks what the CONNECTION can carry, and `route::build_stream` must pass
// both before it streams a file itself.
#[allow(unused_imports)]
pub use transcoder::{
    DP_AUDIO_CODECS, DP_SUBTITLE_CODECS, LinkPolicy, is_dp_audio, is_dp_subtitle, link_policy,
};
