//! The common player's process-lifetime owners: subtitle and caption resources, the container's
//! track names, the audio-pick reason, and what the player screen publishes. Nothing here belongs
//! to a backend. The Feed backend's native mailbox and transport are `plx_play_feed::shared::
//! {FEED_SHARED, FEED_TX}`, which `Shared::reset` composes with the caption reset and which a feed
//! executor is handed as `&'static`.
use std::sync::{atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering}, Mutex};

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
    /// The server ended this logical playback ([`super::note_server_stop`]). A reload of the same
    /// playback keeps it; the next session drops it.
    pub server_stopped: AtomicBool,
    pub server_stop: Mutex<super::ServerStopNote>,
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            desired_sub_idx: AtomicI32::new(-1),
            track_names: Mutex::new(plx_playback_contract::tracks::TrackNames::new()),
            track_names_stamp: AtomicU32::new(0),
            ass_sources: Mutex::new(super::ass_source::Store::new()),
            ass_renderer: super::ass::Runtime::new(),
            sub_bitmaps: Mutex::new(Vec::new()),
            bitmap_generation: AtomicU64::new(0),
            audio_switch: AtomicU8::new(AudioSwitch::None as u8),
            audio_pick_ms: AtomicI64::new(0),
            server_stopped: AtomicBool::new(false),
            server_stop: Mutex::new(super::ServerStopNote::EMPTY),
        }
    }

    /// Container names are common presentation facts, independent of the executor.
    pub fn track_names_lenient(&self) -> std::sync::MutexGuard<'_, plx_playback_contract::tracks::TrackNames> {
        self.track_names.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reset the common stores for a real stop or a new item. Under the Feed backend the caller
    /// runs this inside the Feed's callback/reset barrier (`FeedStop::reset`, through the
    /// adapter), so a late native callback cannot interleave with it; the Feed's own mirrors are
    /// that verb's, not this store's.
    pub fn reset_session(&self) {
        self.reset_captions(false);
        *self.server_stop.lock().unwrap_or_else(|e| e.into_inner()) = super::ServerStopNote::EMPTY;
        self.server_stopped.store(false, Ordering::Relaxed);
    }

    /// [`Self::reset_session`] for a reload of the same item: an audio pick keeps its meaning and
    /// the bounded, already-read ASS sources survive.
    pub fn reset_session_for_reload(&self) {
        self.reset_captions(true);
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

/// What the player screen publishes for the HUD and the app loop. The Feed's pause and seek
/// transport is `plx_play_feed::shared::FEED_TX`, not here. The screen owns scrub intent; these
/// are its existing published scalar copies.
pub struct Transport {
    pub hud_until: AtomicU32,
    pub scrub_ns: AtomicI64,
}

impl Transport {
    pub const fn new() -> Self {
        Self {
            hud_until: AtomicU32::new(0),
            scrub_ns: AtomicI64::new(-1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reload_keeps_the_server_s_stop_and_the_next_session_drops_it() {
        let s = Shared::new();
        s.server_stopped.store(true, Ordering::Relaxed);
        s.reset_session_for_reload();
        assert!(s.server_stopped.load(Ordering::Relaxed), "a reload is the same playback the server ended");
        s.reset_session();
        assert!(!s.server_stopped.load(Ordering::Relaxed));
    }
}
