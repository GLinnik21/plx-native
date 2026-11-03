//! plx_media: the media layer of the PlxNative application core.
//!
//! Feed demux (`ff`, through `plx_media_codecs`), the media credential facade (`curlio` over
//! `plx_net::range_io`), the adaptive-bitrate controller (`abr`), the buffer-feed engine (`player`)
//! and route selection (`route`). The HLS parser and access-unit queues live in `plx_play_feed`.
//! It is the `media` layer of `ci/module-layers.ini`: it
//! uses `playback-contract`, `play-feed`, `media-codecs`, optional `play-uri`, `base`, `machine`,
//! `platform`, `gfx`, `net`, `plex`, `telemetry` and `data`, and nothing above them. The remaining
//! feed orchestration members (`abr`, `curlio`, `ff`, `player` and
//! `route`) share a crate. The bundled FFmpeg binding, encoder and pure
//! audio DSP live in `plx_media_codecs` below this layer.
//!
//! The Starfish/ACB surface is declared here and nowhere else: `sf_on_event` and `acb_on_event`
//! (`player`), which `src/starfish.c` calls, and the externs of `player::ffi`. The crate links nothing itself; the Makefile's final link resolves them.
//!
//! `test-support` exposes the `cfg(test)` seams the layers above test through (`player::preview`,
//! `player::restore_state_for_test`, ...). The application crate enables it in `[dev-dependencies]`
//! only, so no shipped build sees it.

pub mod abr; // client-managed fixed-session HLS controller: estimate, propose, prime, then commit
pub(crate) mod adaptive; // common adaptive policy and route transition; feed supplies physical media
pub mod aq;
pub mod curlio; // the HTTPS media plane: a remote file pulled by byte range over libcurl-multi (stream.rs is the plaintext-socket twin)
pub mod ff_pcm; // FLAC decoded in the app and fed as PCM (libpf has no FLAC input)
pub mod ff; // THE demuxer -- the FFmpeg 9.0 this app BUNDLES and pins (majors 63/63/61), dlopen'd by absolute path beside the binary, never the television's
#[path = "ff_subside.rs"]
pub(crate) mod subtitle_job; // optional subtitle transport and common presentation runner over a codec job
pub mod hls; // strict parser/auth/timeline for the measured one-variant PMS HLS shape
pub mod player; // buffer-feed video engine (was playback.c) -- step 5
pub mod speed_audio; // compatibility facade for the independent codec DSP
pub mod route; // play_movie route selection (direct-play vs transcode) -- step 3
