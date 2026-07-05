//! Typed Plex API layer — one method per PMS operation the app uses.
//!
//! Replaces every hand-built Plex path/query string in the app with a typed `Client`
//! method (see `docs/plex-api-design.md` + `docs/plex-api-catalog.md`). Percent-encoding
//! (`crate::pms::urlenc_str`), the `X-Plex-Token` injection, and the raw-socket transport
//! (`crate::stream::http_get`/`http_put`/`http_open`) are centralised in `client.rs`, so no
//! op file can bypass them. Response bodies deserialize into `serde` DTOs (`models.rs`).
//!
//! Currently unused (`#![allow(dead_code)]`) — the existing `pms`/`route`/`metadata`/
//! `posters`/`player` call sites migrate onto this surface later.
#![allow(dead_code)]

mod client;
mod models;
mod params;

// Op files below only add `impl Client { … }` blocks (Rust allows multiple impls of one
// type across a crate) — declared here so those methods compile onto `Client`.
mod hubs;
mod library;
mod timeline;
mod transcoder;

// The re-exports are the public surface future call sites import; unused until they migrate.
#[allow(unused_imports)]
pub use client::{client, init, Client, StreamUrl};
#[allow(unused_imports)]
pub use models::*;
#[allow(unused_imports)]
pub use params::*;
