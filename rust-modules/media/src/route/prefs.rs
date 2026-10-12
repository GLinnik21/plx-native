//! **The nine install-wide player settings, live: one cell each, one restore, one write door.**
//!
//! The persisted half of each setting is a [`Setting`] row in `plex::session::prefs`; this is the
//! other half, the in-memory copy the player reads every frame (a process-global [`LiveCell`], an
//! atomic rather than a field of any session because a preference OUTLIVES a playback). Before this
//! module each setting was spelled out as its own static, getter, restore and setter, with five
//! different write styles; now each is one [`LiveSetting`] impl and everything else is generic:
//!
//! * [`restore_all`] puts every cell where a [`Preferences`] record says. Boot, the credentials
//!   handoff and a profile switch all call it, so there is one list and it cannot differ between
//!   them. A new live setting is one more row in [`LIVE_ROWS`], and `prefs_tests::t_live_*` fails
//!   until plex's `LIVE` flag and that list agree.
//! * [`choose`] is the one write door. [`How::Confirm`] persists first and publishes only a
//!   durable write ("a failed save claims nothing"); [`How::Optimistic`] publishes now and
//!   persists FIFO on the shared storage worker, never republishing, so a newer pick can never be
//!   overwritten by an older write's completion. The value is run through
//!   [`LiveSetting::normalize`] BEFORE either, so what is stored is what is published.
//! * The getters and `restore_*` / `set_*` / `select_*` functions below are thin wrappers kept so
//!   a caller does not have to name a marker.
//!
//! **What `restore_*` does NOT do** is write: it is the in-memory half only, used where the value
//! comes FROM the record (a legacy session's missing quality stays missing until an explicit
//! choice, so a future Auto-ready build cannot reinterpret an old install).
use super::plan::{supported_quality, Quality, QUALITY_LADDER};
use plx_plex::plex::session::{
    persist_pref, AudioEnhancementsPref, DeckPress, DeckPressPref, DirectPlayMode, DirectPlayModePref,
    NextEpisodeMode, NextEpisodeModePref, Preferences, QualityPref, Setting, SkipInterval,
    SkipIntervalPref, SubtitlePosition, SubtitlePositionPref, SubtitleSize, SubtitleSizePref, SubtitleTone,
    SubtitleTonePref,
};
use plx_plex::plex::session::prefs::SettingsWrite;
use plx_plex::plex::AudioEnhancements;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::Sender;

// ---- the cell -------------------------------------------------------------------------------

/// A value that packs into the one byte a [`LiveCell`] holds. The mapping is each type's own
/// in-memory index (the quality ladder's position, a tone's rung, an enhancement's two bits), so a
/// cell reads exactly what the atomic it replaced read, and an out-of-range byte falls back to the
/// type's documented default, never to a neighbouring rung.
pub trait Packed: Copy {
    /// What the cell holds before anything has restored it.
    const SEED: u8;
    fn to_u8(self) -> u8;
    fn from_u8(b: u8) -> Self;
}

/// One live setting's process-global value. `set` is the ONLY writer, so there is one place a
/// test-lock assertion, a publish counter or a future change notification goes.
pub struct LiveCell<T: Packed> {
    a: AtomicU8,
    /// Names the global in the test-lock panic.
    #[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
    name: &'static str,
    #[cfg(any(test, feature = "test-support"))]
    publishes: std::sync::atomic::AtomicU32,
    _t: PhantomData<fn() -> T>,
}

impl<T: Packed> LiveCell<T> {
    pub const fn new(name: &'static str) -> Self {
        LiveCell {
            a: AtomicU8::new(T::SEED),
            name,
            #[cfg(any(test, feature = "test-support"))]
            publishes: std::sync::atomic::AtomicU32::new(0),
            _t: PhantomData,
        }
    }

    /// Safe from any thread; a reader takes no lock.
    pub fn get(&self) -> T {
        T::from_u8(self.a.load(Relaxed))
    }

    /// Publish `v`. A cell is a process global read without a lock, so in tests a write without
    /// `testlock::serial()` lands mid some OTHER test's read (`on_deck_hevc_p5_preview_uses_the_
    /// selected_episodes_codec` flaked exactly this way); the assertion turns that into a
    /// deterministic panic in the culprit.
    pub fn set(&self, v: T) {
        #[cfg(any(test, feature = "test-support"))]
        {
            // Counted BEFORE the assertion: a publish from a thread that does not hold the lock
            // (the storage worker) panics there, and a panic that is swallowed on that thread must
            // still be visible to a test that counts publishes.
            self.publishes.fetch_add(1, Relaxed);
            plx_base::testlock::assert_held(self.name);
        }
        self.a.store(v.to_u8(), Relaxed);
    }

    /// The raw byte, for the tests that pin the packing.
    #[cfg(any(test, feature = "test-support"))]
    pub fn raw(&self) -> u8 {
        self.a.load(Relaxed)
    }

    /// How many times [`set`](Self::set) has published, so a test can tell one publish from two.
    #[cfg(any(test, feature = "test-support"))]
    pub fn publishes(&self) -> u32 {
        self.publishes.load(Relaxed)
    }
}

/// A `static` [`LiveCell`]; the macro writes nothing else, so the packing and the seed stay on
/// the type's [`Packed`] impl where a reader looks for them.
macro_rules! live_cell {
    ($(#[$meta:meta])* $vis:vis $name:ident: $ty:ty, $what:literal) => {
        $(#[$meta])*
        $vis static $name: LiveCell<$ty> = LiveCell::new($what);
    };
}

/// A type whose in-memory index is its own `index()` / `from_index()` (which already fall back to
/// the type's default when out of range).
macro_rules! packed_by_index {
    ($($ty:ty = $seed:literal),* $(,)?) => {$(
        impl Packed for $ty {
            const SEED: u8 = $seed;
            fn to_u8(self) -> u8 { self.index() }
            fn from_u8(b: u8) -> Self { <$ty>::from_index(b) }
        }
    )*};
}
packed_by_index!(NextEpisodeMode = 0, SkipInterval = 1, DeckPress = 0, SubtitleSize = 1, SubtitlePosition = 0, SubtitleTone = 0);

impl Packed for DirectPlayMode {
    const SEED: u8 = 0;
    fn to_u8(self) -> u8 {
        match self {
            DirectPlayMode::Auto => 0,
            DirectPlayMode::Forced => 1,
            DirectPlayMode::Disabled => 2,
        }
    }
    fn from_u8(b: u8) -> Self {
        match b {
            1 => DirectPlayMode::Forced,
            2 => DirectPlayMode::Disabled,
            _ => DirectPlayMode::Auto,
        }
    }
}

/// The quality ceiling packs as its position in [`QUALITY_LADDER`]; out of range is `Original`,
/// never a neighbouring rung, for the same reason the more menu resolves a press by row identity
/// rather than position: the ladder can grow or shrink. The seed is Original even before the boot
/// gate restores the session: no call path may turn a missing preference into Auto simply because
/// initialization order changed.
impl Packed for Quality {
    const SEED: u8 = 1;
    fn to_u8(self) -> u8 {
        quality_index(self)
    }
    fn from_u8(b: u8) -> Self {
        quality_from_index(b)
    }
}

/// An in-memory index back to a rung — out of range is `Original`, never a neighbouring rung.
pub(super) fn quality_from_index(i: u8) -> Quality {
    QUALITY_LADDER.get(i as usize).copied().unwrap_or(Quality::Original)
}

pub(super) fn quality_index(q: Quality) -> u8 {
    QUALITY_LADDER.iter().position(|&r| r == q).unwrap_or(1) as u8
}

/// The two audio-DSP switches (issue #266) as two bits.
const ENH_BOOST_DIALOG: u8 = 1;
const ENH_NORMALIZE_LOUDNESS: u8 = 2;
impl Packed for AudioEnhancements {
    const SEED: u8 = 0;
    fn to_u8(self) -> u8 {
        let boost = if self.boost_dialog { ENH_BOOST_DIALOG } else { 0 };
        let normalize = if self.normalize_loudness { ENH_NORMALIZE_LOUDNESS } else { 0 };
        boost | normalize
    }
    fn from_u8(bits: u8) -> Self {
        AudioEnhancements {
            boost_dialog: bits & ENH_BOOST_DIALOG != 0,
            normalize_loudness: bits & ENH_NORMALIZE_LOUDNESS != 0,
        }
    }
}

// ---- the cells ------------------------------------------------------------------------------

live_cell!(
    /// Install-wide Direct Play mode; each resolve captures its own immutable copy.
    pub(super) DIRECT_PLAY_MODE: DirectPlayMode, "direct-play preference"
);
live_cell!(
    /// What the player does at an episode's credits when a successor is queued. Read by
    /// `appkit::player_hud::slot` every frame and by `finish_playback` at the end of the stream.
    pub(super) NEXT_EPISODE_MODE: NextEpisodeMode, "next episode preference"
);
live_cell!(
    /// How far one Left/Right press jumps in the player and the trailer transport. Read per press
    /// by `appkit::player_hud::scrub_step_ns`.
    pub(super) SKIP_INTERVAL: SkipInterval, "skip interval preference"
);
live_cell!(
    /// What OK does on a Continue Watching card. Read by every deck's press and draw (Home's and
    /// the Library's) and by the item menu when it builds its rows; `DeckPress::press_plays` is the
    /// one question they all ask of it.
    pub(super) DECK_PRESS: DeckPress, "deck press preference"
);
live_cell!(
    /// The client-rendered subtitle caption's text size.
    pub(super) SUBTITLE_SIZE: SubtitleSize, "subtitle size preference"
);
live_cell!(
    /// The client-rendered subtitle caption's vertical placement. Only the plain-text caption draw
    /// moves with this; image (PGS/VobSub) captions and native ASS/SSA keep their own placement.
    pub(super) SUBTITLE_POSITION: SubtitlePosition, "subtitle position preference"
);
live_cell!(
    /// The user's current quality pick. An atomic rather than a field on `Session` because it
    /// OUTLIVES a playback (it is a preference, not session state) and because `appkit::more_menu`
    /// reads it to draw the checkmark while `ResolveEnv::snapshot` reads it to hand the worker a
    /// copy.
    pub(super) QUALITY: Quality, "the playback quality ceiling"
);

// ---- the rows -------------------------------------------------------------------------------

/// A live [`Setting`]: what the media layer adds to the persisted row.
pub trait LiveSetting: Setting {
    /// The value in the cell now.
    fn live() -> Self::Value;
    /// Publish `v` into the cell. Prefer [`choose`] or [`restore_all`]: this does not persist.
    fn publish(v: Self::Value);
    /// How many times the cell has been published (tests only).
    #[cfg(any(test, feature = "test-support"))]
    fn publishes() -> u32;
    /// Media policy applied to a value BEFORE it is published or persisted, so what is stored is
    /// what is live. Quality: [`supported_quality`] (a rung this build does not offer is Original).
    fn normalize(v: Self::Value) -> Self::Value {
        v
    }
    /// What a durable Confirm pick reports, exactly once (usage telemetry).
    fn after_saved(_v: Self::Value) {}
    /// The setting's name in the one failure line.
    fn log_label() -> &'static str;
    /// A value a caller of [`restore_all`] forces over the record's (the dev quality override).
    fn overridden(_ov: &Overrides) -> Option<Self::Value> {
        None
    }
}

/// A plain row: the marker, its cell, and its label. Anything a row does beyond that (clamp,
/// telemetry, an override) is spelled out beside it.
macro_rules! live_setting {
    ($marker:ty, $cell:path, $label:literal $(, $($body:tt)*)?) => {
        impl LiveSetting for $marker {
            fn live() -> <$marker as Setting>::Value { $cell.get() }
            fn publish(v: <$marker as Setting>::Value) { $cell.set(v) }
            #[cfg(any(test, feature = "test-support"))]
            fn publishes() -> u32 { $cell.publishes() }
            fn log_label() -> &'static str { $label }
            $($($body)*)?
        }
    };
}

live_setting!(DirectPlayModePref, DIRECT_PLAY_MODE, "direct play mode");
live_setting!(DeckPressPref, DECK_PRESS, "deck press");
live_setting!(SubtitleSizePref, SUBTITLE_SIZE, "subtitle size");
live_setting!(SubtitlePositionPref, SUBTITLE_POSITION, "subtitle position");
live_setting!(SubtitleTonePref, crate::player::SUBTITLE_TONE, "subtitle tone");
live_setting!(AudioEnhancementsPref, crate::player::AUDIO_ENHANCEMENTS, "audio enhancements");
live_setting!(NextEpisodeModePref, NEXT_EPISODE_MODE, "next episode mode",
    fn after_saved(mode: NextEpisodeMode) {
        plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::FeatureUsed {
            feature: plx_telemetry::diag::schema::Feature::NextEpisode(mode),
        });
    }
);
live_setting!(SkipIntervalPref, SKIP_INTERVAL, "skip interval",
    fn after_saved(interval: SkipInterval) {
        plx_telemetry::diag::event(plx_telemetry::diag::schema::DiagEvent::FeatureUsed {
            feature: plx_telemetry::diag::schema::Feature::SkipInterval(interval),
        });
    }
);
live_setting!(QualityPref, QUALITY, "playback quality",
    fn normalize(q: Quality) -> Quality {
        supported_quality(q)
    }
    fn overridden(ov: &Overrides) -> Option<Quality> {
        ov.quality
    }
);

/// A live row as data, so a test (and [`restore_all`]) can walk them all.
#[derive(Clone, Copy)]
pub struct LiveRow {
    pub key: &'static str,
    /// Always `S::LIVE`; carried so the tie test compares the same `{key: live}` map on both sides.
    pub live: bool,
    restore: fn(&Preferences, &Overrides),
}

impl LiveRow {
    pub const fn of<S: LiveSetting>() -> LiveRow {
        LiveRow { key: S::KEY, live: S::LIVE, restore: restore_row::<S> }
    }
}

/// Every setting plex declares LIVE, once. `prefs_tests::t_live_*` holds this to
/// `plex::session::prefs::DESCRIPTORS` in both directions, by key.
pub const LIVE_ROWS: &[LiveRow] = &[
    LiveRow::of::<QualityPref>(),
    LiveRow::of::<DirectPlayModePref>(),
    LiveRow::of::<SubtitleTonePref>(),
    LiveRow::of::<SubtitleSizePref>(),
    LiveRow::of::<SubtitlePositionPref>(),
    LiveRow::of::<NextEpisodeModePref>(),
    LiveRow::of::<SkipIntervalPref>(),
    LiveRow::of::<DeckPressPref>(),
    LiveRow::of::<AudioEnhancementsPref>(),
];

// ---- restore --------------------------------------------------------------------------------

/// What a caller of [`restore_all`] forces over the record. Each caller computes it exactly as it
/// did when it spelled the list out: boot passes none when the harness is controlled and the dev
/// quality override otherwise; the credentials handoff passes the override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub quality: Option<Quality>,
}

fn restore_row<S: LiveSetting>(prefs: &Preferences, ov: &Overrides) {
    restore_live::<S>(S::overridden(ov).unwrap_or_else(|| S::get(prefs)));
}

fn restore_live<S: LiveSetting>(v: S::Value) {
    S::publish(S::normalize(v));
}

/// **Put every live cell where `prefs` says**, without writing anything back. The one restore:
/// boot, the credentials handoff after a sign-in, and a profile switch all call it. A legacy
/// session's missing quality resolves to Original but stays missing on disk until the user makes an
/// explicit choice, so a future Auto-ready build still cannot reinterpret that old install.
pub fn restore_all(prefs: &Preferences, ov: Overrides) {
    for row in LIVE_ROWS {
        (row.restore)(prefs, &ov);
    }
}

// ---- the write door -------------------------------------------------------------------------

/// How a pick is written.
pub enum How {
    /// Persist first, on the shared storage worker; publish only once the write is durable, then
    /// reply with whether it was. A failed save claims nothing.
    Confirm(Sender<bool>),
    /// Publish now, on the caller's thread; persist FIFO on the worker behind it, reply (if asked)
    /// with whether that write was durable, and NEVER republish: live = the latest pick, durable
    /// writes are ordered, last submitted wins, and a failure keeps the pick for this session.
    Optimistic(Option<Sender<bool>>),
}

impl How {
    /// The way a Settings row saves `S`, as `S::SETTINGS` declares it, so the policy lives in one
    /// place and the dispatcher cannot disagree with it.
    pub fn settings<S: Setting>(reply: Sender<bool>) -> How {
        match S::SETTINGS {
            Some(SettingsWrite::Confirm) => How::Confirm(reply),
            Some(SettingsWrite::Optimistic) | None => How::Optimistic(Some(reply)),
        }
    }
}

/// **The one write door for a live setting.** `v` is normalized first, so the clamp happens before
/// publish and before persist.
pub fn choose<S: LiveSetting>(v: S::Value, how: How) {
    choose_via::<S>(v, how, S::normalize);
}

fn choose_via<S: LiveSetting>(v: S::Value, how: How, normalize: impl FnOnce(S::Value) -> S::Value) {
    let v = normalize(v);
    match how {
        How::Confirm(reply) => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let saved = confirm_via::<S>(v, |v| v);
                let _ = reply.send(saved);
                plx_machine::idle::invalidate();
            });
        }
        How::Optimistic(reply) => {
            S::publish(v);
            plx_machine::idle::invalidate();
            let _ = plx_base::storage_worker::submit_retained(move || {
                let saved = persist_pref::<S>(v);
                if !saved {
                    plx_base::eventlog::log(&format!(
                        "{}: durable write failed (live value kept for this session)",
                        S::log_label()
                    ));
                }
                if let Some(reply) = reply {
                    let _ = reply.send(saved);
                }
                plx_machine::idle::invalidate();
            });
        }
    }
}

/// The blocking half of [`How::Confirm`], for a caller already on the storage worker (and the
/// tests, which grade it without a thread): persist, then publish and report only if the write is
/// durable. `true` also when the record already held the value (nothing to write, nothing to
/// refuse).
pub fn confirm<S: LiveSetting>(v: S::Value) -> bool {
    confirm_via::<S>(v, S::normalize)
}

fn confirm_via<S: LiveSetting>(v: S::Value, normalize: impl FnOnce(S::Value) -> S::Value) -> bool {
    let v = normalize(v);
    let saved = persist_pref::<S>(v);
    if saved {
        S::publish(v);
        plx_machine::idle::invalidate();
        S::after_saved(v);
    }
    saved
}

/// How many times `S`'s cell has been published, so a test can tell one publish from two.
#[cfg(any(test, feature = "test-support"))]
pub fn publishes_for_test<S: LiveSetting>() -> u32 {
    S::publishes()
}

// ---- the getters and thin wrappers ----------------------------------------------------------

pub fn direct_play_mode() -> DirectPlayMode { DirectPlayModePref::live() }
pub fn restore_direct_play_mode(mode: DirectPlayMode) { restore_live::<DirectPlayModePref>(mode) }
/// Blocking persistence seam; Settings dispatches it through [`choose`].
pub fn set_direct_play_mode(mode: DirectPlayMode) -> bool { confirm::<DirectPlayModePref>(mode) }

pub fn next_episode_mode() -> NextEpisodeMode { NextEpisodeModePref::live() }
pub fn restore_next_episode_mode(mode: NextEpisodeMode) { restore_live::<NextEpisodeModePref>(mode) }
/// Blocking persistence seam. The live value changes only once the write is durable, so a failed
/// save claims nothing.
pub fn set_next_episode_mode(mode: NextEpisodeMode) -> bool { confirm::<NextEpisodeModePref>(mode) }

pub fn skip_interval() -> SkipInterval { SkipIntervalPref::live() }
pub fn restore_skip_interval(interval: SkipInterval) { restore_live::<SkipIntervalPref>(interval) }
/// Blocking persistence seam, as [`set_next_episode_mode`].
pub fn set_skip_interval(interval: SkipInterval) -> bool { confirm::<SkipIntervalPref>(interval) }

pub fn deck_press() -> DeckPress { DeckPressPref::live() }
pub fn restore_deck_press(mode: DeckPress) { restore_live::<DeckPressPref>(mode) }
/// Blocking persistence seam, as [`set_next_episode_mode`].
pub fn set_deck_press(mode: DeckPress) -> bool { confirm::<DeckPressPref>(mode) }

pub fn subtitle_size() -> SubtitleSize { SubtitleSizePref::live() }
pub fn restore_subtitle_size(size: SubtitleSize) { restore_live::<SubtitleSizePref>(size) }
/// **Pick a caption size: one optimistic operation, on the main thread** (see [`How::Optimistic`]).
/// `reply` (Settings) learns whether the write was durable.
pub fn select_subtitle_size(size: SubtitleSize, reply: Option<Sender<bool>>) {
    choose::<SubtitleSizePref>(size, How::Optimistic(reply));
}

pub fn subtitle_position() -> SubtitlePosition { SubtitlePositionPref::live() }
pub fn restore_subtitle_position(position: SubtitlePosition) { restore_live::<SubtitlePositionPref>(position) }
/// [`select_subtitle_size`] for the caption's vertical placement.
pub fn select_subtitle_position(position: SubtitlePosition, reply: Option<Sender<bool>>) {
    choose::<SubtitlePositionPref>(position, How::Optimistic(reply));
}

/// The selected ceiling. Safe from any thread; the resolve worker gets a COPY through
/// `ResolveEnv` rather than reading this, per that struct's own rule.
pub fn quality() -> Quality { QualityPref::live() }
/// Restore the persisted preference without writing it back. The distinction matters for legacy
/// sessions: their missing field resolves to Original but remains missing until the user makes an
/// explicit choice, so a future Auto-ready build still cannot reinterpret that old install.
pub fn restore_quality(q: Quality) { restore_live::<QualityPref>(q) }
/// Settings' default quality: blocking, clamped to a rung this build offers BEFORE it is written,
/// published only once durable.
pub fn set_default_quality(q: Quality) -> bool { confirm::<QualityPref>(q) }

#[cfg(test)]
#[path = "prefs_tests.rs"]
mod prefs_tests;
