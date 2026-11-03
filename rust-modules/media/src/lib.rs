//! plx_media: the media layer of the PlxNative application core.
//!
//! The `ff` compatibility facade over `plx_play_feed`'s demux, the media credential facade (`curlio` over
//! `plx_net::range_io`), the adaptive-bitrate controller (`abr`), the buffer-feed engine (`player`)
//! and route selection (`route`). The HLS parser and access-unit queues live in `plx_play_feed`.
//! It is the `media` layer of `ci/module-layers.ini`: it
//! uses `playback-contract`, `media-codecs`, optional `play-feed` and `play-uri`, `base`, `machine`,
//! `platform`, `gfx`, `net`, `plex`, `telemetry` and `data`, and nothing above them. The remaining
//! media orchestration members (`abr`, `curlio`, `player` and
//! `route`) and the `ff` facade share a crate. The bundled FFmpeg binding, encoder and pure
//! audio DSP live in `plx_media_codecs` below this layer.
//!
//! Starfish/ACB calls back into `player` through `sf_on_event` and `acb_on_event`. The native
//! extern declarations live in the platform port; the Makefile's final link resolves them.
//!
//! `test-support` exposes the `cfg(test)` seams the layers above test through (`player::preview`,
//! `player::restore_state_for_test`, ...). The application crate enables it in `[dev-dependencies]`
//! only, so no shipped build sees it.

pub mod abr; // client-managed fixed-session HLS controller: estimate, propose, prime, then commit
#[cfg(feature = "play-feed")]
pub(crate) mod adaptive; // common adaptive policy and route transition; feed supplies physical media
#[cfg(feature = "play-feed")]
pub mod aq;
pub mod curlio; // the HTTPS media plane: a remote file pulled by byte range over libcurl-multi (stream.rs is the plaintext-socket twin)
#[cfg(feature = "play-feed")]
pub mod ff_pcm; // compatibility facade for the physical PCM decoder in play-feed
#[cfg(feature = "play-feed")]
pub mod ff; // compatibility facade for the physical FFmpeg demuxer in play-feed
#[path = "ff_subside.rs"]
pub(crate) mod subtitle_job; // optional subtitle transport and common presentation runner over a codec job
#[cfg(feature = "play-feed")]
pub mod hls; // compatibility facade for the strict parser/auth/timeline in play-feed
pub mod player; // buffer-feed video engine (was playback.c) -- step 5
pub mod speed_audio; // compatibility facade for the independent codec DSP
pub mod route; // play_movie route selection (direct-play vs transcode) -- step 3
