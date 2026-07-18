//! Typed Plex API layer — one method per PMS operation the app uses.
//!
//! Replaces every hand-built Plex path/query string in the app with a typed `Client`
//! method (see `docs/plex-api-design.md` + `docs/plex-api-catalog.md`). Percent-encoding
//! (`crate::pms::urlenc_str`), the `X-Plex-Token` injection, and the raw-socket transport
//! (`crate::stream::http_get`/`http_put`/`http_open`) are centralised in `client.rs`, so no
//! op file can bypass them. Response bodies deserialize into `serde` DTOs (`models.rs`).
//!
//! The READ layer is live: `plex::install` is called at boot/login (`app.rs`, `auth.rs`) and
//! `pms`/`metadata`/`posters`/`detail` all go through `client()`. Only the playback/decision
//! path (`route.rs`, `transcoder.rs`) still bypasses it — that migration is deferred (see
//! `docs/plex-api-migration.md`). The module-wide allow covers the deliberately-scaffolded
//! ops kept for that migration.
#![allow(dead_code)]

mod client;
mod models;

// Op files below only add `impl Client { … }` blocks (Rust allows multiple impls of one
// type across a crate) — declared here so those methods compile onto `Client`.
mod hubs;
mod library;
mod transcoder;

// The plex.tv ACCOUNT surface (login/discovery/home-users) — a separate service from the PMS
// `Client` above (own host + HTTPS transport), so it's its own `AccountClient`, not an `impl`.
// pub(crate): the login/boot code (app.rs) + the UI screens construct these directly.
pub(crate) mod account;
pub(crate) mod session;

// The re-exports are the public surface the call sites import.
#[allow(unused_imports)]
pub use client::{client, install, Client, StreamUrl};
#[allow(unused_imports)]
pub use models::*;
