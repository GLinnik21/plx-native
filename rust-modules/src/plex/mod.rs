//! Typed Plex API layer — one method per PMS operation the app uses.
//!
//! Replaces every hand-built Plex path/query string in the app with a typed `Client`
//! method (see `docs/plex-api-design.md` + `docs/plex-api-catalog.md`). Percent-encoding
//! (`crate::pms::urlenc_str`), the `X-Plex-Token` injection, and the raw-socket transport
//! (`crate::stream::http_get`/`http_put`/`http_open`) are centralised in `client.rs`, so no
//! op file can bypass them. Response bodies deserialize into `serde` DTOs (`models.rs`).
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
mod timeline;
mod transcoder;

// The server's self-description (version + Plex Pass tristate), refreshed by `install` on every
// session path. pub(crate) because it is a DIAGNOSTICS surface with readers outside this layer
// (`ui::stats`, `player::error_shape`) — and deliberately nothing else: see its module doc for
// why subscription state must never become a routing input.
pub(crate) mod serverinfo;

// The plex.tv ACCOUNT surface (login/discovery/home-users) — a separate service from the PMS
// `Client` above (own host + HTTPS transport), so it's its own `AccountClient`, not an `impl`.
// pub(crate): the login/boot code (app.rs) + the UI screens construct these directly.
pub(crate) mod account;
pub(crate) mod session;

// Which of a server's advertised addresses are worth dialling, and in what order. PURE policy over
// an `account::Resource` — no socket, no thread — so the rules that decide reachability are gradeable
// on the host, which is the only tier that can grade them at all: the failures they prevent are an
// 8-second timeout and a probe that answers as the wrong machine.
pub(crate) mod probe;

// The plex.tv METADATA PROVIDER (`discover.provider.plex.tv`) — a third service, but one that
// shares the account API's transport + identity headers exactly, so it only adds an
// `impl AccountClient` block (same pattern as the PMS op files above).
pub(crate) mod discover;

// The re-exports are the public surface the call sites import.
#[allow(unused_imports)]
pub use client::{Client, StreamUrl};
// The registry surface. `client`/`client_opt`/`install` keep the exact signatures they had as
// singleton accessors, so every call site outside `plex/` reads unchanged; `client_for`,
// `register`, `set_current` and `ServerId` are the multi-server additions.
#[allow(unused_imports)]
pub use servers::{
    client, client_for, client_opt, count as server_count, current as current_server, describe as describe_server,
    describe_name as describe_server_name, facts as server_facts, ids as server_ids, install, register, same_item,
    set_current, ServerFacts, ServerId, MAX_SERVERS,
};
// Sign-out. `pub(crate)` like the function itself: retiring the whole table is `auth::sign_out`'s
// to call and nothing else's — a caller that merely wants to stop using a server wants
// `set_current`, and one that wants to forget a share wants plex.tv to stop granting it.
#[allow(unused_imports)]
pub(crate) use servers::revoke_all;
// The registry as a TEST FIXTURE, for suites outside this module (`route.rs` grades which server a
// `/:/timeline` POST reaches). `register_for_test` skips the `session::load` the public `register`
// does — that call mints and PERSISTS a device uuid, which a host test has no business writing —
// and `reset_servers_for_test` is what keeps the table a per-test fixture instead of a growing
// process-global holding clients whose loopback ports closed when their test returned. Both must be
// called under `crate::testlock::serial`.
#[cfg(test)]
pub(crate) use servers::{register_with_client_id as register_for_test, reset_for_test as reset_servers_for_test};
#[allow(unused_imports)]
pub use models::*;
#[allow(unused_imports)]
pub use params::*;
// The projected play-queue row + the identity rule that locates one: op-file items rather than
// wire DTOs, so they are re-exported by name (route.rs names the row in `Plan`/`QueueInfo` — the
// rest of `timeline` is reached through `Client`'s methods and needs none).
#[allow(unused_imports)]
pub use timeline::{queue_index_of, QueueRow};
// DP_AUDIO_CODECS rides along for `devcaps`, which intersects it with the device's own codec
// table — the caps snapshot is what `is_dp_audio` and the profile string then both read.
// `link_policy` is the other gate on the same decision: `is_dp_audio` asks what the PIPELINE can
// decode, `link_policy` asks what the CONNECTION can carry, and `route::build_stream` must pass
// both before it streams a file itself.
#[allow(unused_imports)]
pub use transcoder::{is_dp_audio, link_policy, LinkPolicy, DP_AUDIO_CODECS};
