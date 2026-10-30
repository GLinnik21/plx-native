//! The Jellyfin backend of FinNative.
//!
//! FinNative is PlxNative's engine (UI, player, webOS port) built with a Jellyfin backend in place
//! of the Plex one. The screens, stores and player above this layer render the Plex layer's item
//! model (`plx_plex::plex::{MediaContainer, Metadata, Media, MediaPart, Stream, ...}`), so this
//! layer's job is to speak Jellyfin's REST API and hand back that same model:
//!
//! - [`dto`]: serde types for the Jellyfin responses the app reads (`BaseItemDto`, `MediaSourceInfo`,
//!   `MediaStream`, `QueryResult`, auth and Quick Connect results, media segments).
//! - [`auth`]: the `Authorization: MediaBrowser ...` header every request carries.
//! - [`paths`]: one function per Jellyfin endpoint, building its path and query.
//! - [`ids`]: the table that gives Jellyfin's GUID library ids the small integer section keys the
//!   app's section APIs take.
//! - [`translate`]: Jellyfin DTOs into the common model.
//! - [`blurhash`]: an image's BlurHash as the four ambient corner colours (`UltraBlurColors`).
//! - [`client`]: one signed-in server: sign-in (password and Quick Connect) and the reads the
//!   screens make, answering in the common model.
//! - [`backend`]: the `plx_plex` `Backend` that answers the app's `Client` calls from the
//!   signed-in [`client::JfClient`].
//! - [`discovery`]: finding a server on the local network (UDP 7359).
//! - [`qrpng`]: the sign-in screen's QR code, as a PNG.
//! - [`playback_info`]: the server's own playback verdict for this device (`PlaybackInfo`).
//! - [`transcode`]: a transcode request as a Jellyfin stream, and the decision the player reads.
//! - [`live`]: the server's WebSocket as the app's live-update feed, over [`socket`], a minimal
//!   WebSocket client.
//! - [`time`]: ticks and ISO-8601 dates into the milliseconds and unix seconds the model uses.
//! - [`languages`]: Jellyfin's ISO 639-2 language codes as the app's (Plex's account codes).

pub mod auth;
pub mod backend;
pub mod blurhash;
pub mod client;
pub mod discovery;
pub mod dto;
pub mod favorites;
pub mod ids;
pub mod image_tags;
pub mod languages;
pub mod live;
pub mod paths;
pub mod playback_info;
pub mod qrpng;
pub mod socket;
pub mod time;
pub mod transcode;
pub mod translate;
