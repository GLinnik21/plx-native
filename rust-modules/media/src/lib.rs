//! plx_media: the media layer of the PlxNative application core.
//!
//! The media credential facade (`curlio` over
//! `plx_net::range_io`), the adaptive-bitrate controller (`abr`), the buffer-feed engine (`player`)
//! and route selection (`route`). The HLS parser and access-unit queues live in `plx_play_feed`.
//! It is the `media` layer of `ci/module-layers.ini`: it
//! uses `playback-contract`, `media-codecs`, `base`, `machine`,
//! `platform`, `gfx`, `net`, `plex`, `telemetry` and `data`, and nothing above them. The remaining
//! media orchestration members (`abr`, `curlio`, `player` and
//! `route`) and the `ff` facade share a crate. The bundled FFmpeg binding, encoder and pure
//! audio DSP live in `plx_media_codecs` below this layer.
//!
//! Starfish calls back into the Feed backend through `sf_on_event` (`plx_play_feed::sf_event`); the ACB completion callback,
//! `acb_on_event`, is the platform port's (`webos::video_sink`) and feeds the plane owner's mailbox.
//! The native extern declarations live in the platform port; the Makefile's final link resolves them.
//!
//! `test-support` exposes the `cfg(test)` seams the layers above test through (`player::preview`,
//! `player::restore_state_for_test`, ...). The application crate enables it in `[dev-dependencies]`
//! only, so no shipped build sees it.

pub mod abr; // client-managed fixed-session HLS controller: estimate, propose, prime, then commit
pub(crate) mod adaptive; // common adaptive policy and route transition; feed supplies physical media
pub mod curlio; // the HTTPS media plane: a remote file pulled by byte range over libcurl-multi (stream.rs is the plaintext-socket twin)
pub mod ff; // compatibility facade for the physical FFmpeg demuxer in play-feed
#[path = "ff_subside.rs"]
pub(crate) mod subtitle_job; // optional subtitle transport and common presentation runner over a codec job
pub mod player; // buffer-feed video engine (was playback.c) -- step 5
pub mod speed_audio; // compatibility facade for the independent codec DSP
pub mod route; // play_movie route selection (direct-play vs transcode) -- step 3

#[cfg(test)]
mod http_test_support;
