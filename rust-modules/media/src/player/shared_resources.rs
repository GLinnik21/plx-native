//! Common subtitle resources composed beside the physical feed transport.
//! The existing process-lifetime owner stays singular; a feed executor receives only `feed`.
#[cfg(feature = "play-feed")]
use std::ops::Deref;
use std::sync::{atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering}, Mutex};

pub struct Shared {
    #[cfg(feature = "play-feed")]
    pub feed: plx_play_feed::shared::PhysicalShared,
    pub ass_sources: Mutex<super::ass_source::Store>,
    pub ass_renderer: super::ass::Runtime,
    pub sub_bitmaps: Mutex<Vec<super::subtitle_bitmap::SubBitmap>>,
    /// Bumped while holding `sub_bitmaps` at reset, so an old producer can
    /// never append pixels after the successor has cleared that store.
    pub bitmap_generation: AtomicU64,
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            #[cfg(feature = "play-feed")]
            feed: plx_play_feed::shared::PhysicalShared::new(),
            ass_sources: Mutex::new(super::ass_source::Store::new()),
            ass_renderer: super::ass::Runtime::new(),
            sub_bitmaps: Mutex::new(Vec::new()),
            bitmap_generation: AtomicU64::new(0),
        }
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
            self.feed.reset_session_with(for_reload, || self.reset_captions(for_reload));
            if for_reload { self.feed.duration_ns.store(duration, Ordering::Relaxed); }
        }
        #[cfg(not(feature = "play-feed"))]
        self.reset_captions(for_reload);
    }

    fn reset_captions(&self, for_reload: bool) {
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
