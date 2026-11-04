//! Common subtitle resources composed beside the physical feed transport.
//! The existing process-lifetime owner stays singular; a feed executor receives only `feed`.
#[cfg(feature = "play-feed")]
use std::ops::Deref;
use std::sync::{atomic::{AtomicI32, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering}, Mutex};

/// A viewer audio pick names the next rebuild for at most this long.
pub const AUDIO_PICK_WINDOW_MS: i64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioSwitch { None = 0, Picked = 1, Reloading = 2 }
impl AudioSwitch {
    pub fn from_u8(value: u8) -> Self {
        match value { 1 => Self::Picked, 2 => Self::Reloading, _ => Self::None }
    }
}

pub struct Shared {
    #[cfg(feature = "play-feed")]
    pub feed: plx_play_feed::shared::PhysicalShared,
    /// Selected client-rendered subtitle track; -1 disables the common overlay.
    pub desired_sub_idx: AtomicI32,
    /// **The container's own per-track names, which PMS does not always send** (D -> M).
    ///
    /// `audio` and `subs`, each in FILE order, so position N is the N-th audio / subtitle stream of
    /// the part — the ordinal `metadata::audio_ordinal` and `metadata::sub_render_ordinal`
    /// already resolve a track-menu row to. Empty strings for a file that tags nothing, and an
    /// empty Vec until a demuxer has opened.
    ///
    /// It exists because for an **MP4** part PMS sends no `Stream.title` at all, though the file
    /// names every track (`"Полные Jaskier"`, `"Форс. iTunes"`). See `ff::stream_name`. So the
    /// picker's rows are the only place this reaches, and only on DIRECT PLAY: a transcode is a
    /// remux the server built, and its tags are whatever the server put there.
    pub track_names: Mutex<plx_playback_contract::tracks::TrackNames>,
    /// Bumped each time `track_names` is replaced or cleared. The main thread delivers the names to
    /// the route session when this moves (`pump::deliver_container_names`), so the route reads a
    /// copy it owns and never this registry.
    pub track_names_stamp: AtomicU32,
    pub ass_sources: Mutex<super::ass_source::Store>,
    pub ass_renderer: super::ass::Runtime,
    pub sub_bitmaps: Mutex<Vec<super::subtitle_bitmap::SubBitmap>>,
    /// Bumped while holding `sub_bitmaps` at reset, so an old producer can
    /// never append pixels after the successor has cleared that store.
    pub bitmap_generation: AtomicU64,
    /// Common presentation reason for the next rebuild, independent of its executor. A queued
    /// audio pick becomes Reloading on a reload, then clears at the first reported frame or a
    /// full reset. Picked expires if its rebuild never arrives.
    pub audio_switch: AtomicU8,
    pub audio_pick_ms: AtomicI64,
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            #[cfg(feature = "play-feed")]
            feed: plx_play_feed::shared::PhysicalShared::new(),
            desired_sub_idx: AtomicI32::new(-1),
            track_names: Mutex::new(plx_playback_contract::tracks::TrackNames::new()),
            track_names_stamp: AtomicU32::new(0),
            ass_sources: Mutex::new(super::ass_source::Store::new()),
            ass_renderer: super::ass::Runtime::new(),
            sub_bitmaps: Mutex::new(Vec::new()),
            bitmap_generation: AtomicU64::new(0),
            audio_switch: AtomicU8::new(AudioSwitch::None as u8),
            audio_pick_ms: AtomicI64::new(0),
        }
    }

    /// Container names are common presentation facts, independent of the executor.
    pub fn track_names_lenient(&self) -> std::sync::MutexGuard<'_, plx_playback_contract::tracks::TrackNames> {
        self.track_names.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Map common report classification to the physical first-wins diagnostic latch.
    #[cfg(feature = "play-feed")]
    pub fn note_demux_exit(&self, exit: plx_telemetry::telemetry::classes::DemuxExitClass) {
        self.feed.note_demux_exit_latch(exit.latch());
    }

    #[cfg(feature = "play-feed")]
    pub fn fail_demux(&self, exit: plx_telemetry::telemetry::classes::DemuxExitClass) {
        self.feed.fail_demux_latch(exit.latch());
    }

    pub fn reset_session(&self) {
        self.reset(false);
    }

    pub fn reset_session_for_reload(&self) {
        self.reset(true);
    }

    fn reset(&self, for_reload: bool) {
        #[cfg(feature = "play-feed")]
        {
            let duration = self.feed.duration_ns.load(Ordering::Relaxed);
            self.feed.reset_session_with(|| self.reset_captions(for_reload));
            if for_reload { self.feed.duration_ns.store(duration, Ordering::Relaxed); }
        }
        #[cfg(not(feature = "play-feed"))]
        self.reset_captions(for_reload);
    }

    fn reset_captions(&self, for_reload: bool) {
        // A reload keeps an audio pick's meaning; a new item or teardown retires it.
        let next = match AudioSwitch::from_u8(self.audio_switch.load(Ordering::Relaxed)) {
            AudioSwitch::Picked | AudioSwitch::Reloading if for_reload => AudioSwitch::Reloading,
            _ => AudioSwitch::None,
        };
        self.audio_switch.store(next as u8, Ordering::Relaxed);
        *self.track_names.lock().unwrap_or_else(|e| e.into_inner()) = plx_playback_contract::tracks::TrackNames::new();
        self.track_names_stamp.fetch_add(1, Ordering::Release);
        let mut sources = self.ass_sources.lock().unwrap_or_else(|e| e.into_inner());
        if for_reload { sources.retain_for_reload(); } else { sources.reset(); }
        drop(sources);
        let mut bitmaps = self.sub_bitmaps.lock().unwrap();
        self.bitmap_generation.fetch_add(1, Ordering::Release);
        bitmaps.clear();
    }
}

#[cfg(feature = "play-feed")]

impl Deref for Shared {
    type Target = plx_play_feed::shared::PhysicalShared;
    fn deref(&self) -> &Self::Target { &self.feed }
}

/// Common UI publications live beside, rather than inside, optional native feed state.
/// The screen owns scrub intent; these are its existing published scalar copies.
pub struct Transport {
    #[cfg(feature = "play-feed")]
    pub feed: plx_play_feed::shared::Transport,
    pub hud_until: AtomicU32,
    pub scrub_ns: AtomicI64,
}

impl Transport {
    pub const fn new() -> Self {
        Self {
            #[cfg(feature = "play-feed")]
            feed: plx_play_feed::shared::Transport::new(),
            hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1),
        }
    }
}

#[cfg(feature = "play-feed")]
impl Deref for Transport {
    type Target = plx_play_feed::shared::Transport;
    fn deref(&self) -> &Self::Target { &self.feed }
}
