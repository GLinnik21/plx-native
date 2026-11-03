//! **Per-profile settings, live: one cell each, one restore, one write door.**
//! Includes instant D-pad seeking, hero trailer autoplay and Under posters.
//!
//! The persisted half of each setting is a [`Setting`] row in `plex::session::prefs`; this is the
//! other half, the in-memory copy the player reads every frame (a process-global [`LiveCell`], an
//! atomic rather than a field of any session because a preference OUTLIVES a playback). The cells
//! hold the SEATED profile's values (`session::Seat`): the profile's own pick, else the frozen
//! install-wide key beside it ([`effective`]). Each setting is one [`LiveSetting`] impl and
//! everything else is generic:
//!
//! * [`restore_profile_rows`] puts every cell where a [`Seat`] says, and runs ONLY when the seat
//!   changes (the app's `SeatWatch`, fed by the profile publication) and once at process start for
//!   `Unseated`. [`restore_install_rows`] reseeds the mirrors from a record (boot and the credentials
//!   handoff). A new live setting is one more row in [`LIVE_ROWS`], and `prefs_tests::t_live_*` fails
//!   until plex's `LIVE` flag and that list agree.
//! * [`choose`] is the one write door. It captures the seat on the caller's (main) thread and
//!   refuses `Unseated`: a pick belongs to somebody. [`How::Confirm`] persists first and publishes
//!   only a durable write ("a failed save claims nothing") and only if that profile is still seated;
//!   [`How::Optimistic`] publishes now and persists FIFO on the shared storage worker, never
//!   republishing, so a newer pick can never be overwritten by an older write's completion. The
//!   value is run through [`LiveSetting::normalize`] BEFORE either, so what is stored is what is
//!   published.
//! * The getters and `restore_*` / `set_*` / `select_*` functions below are thin wrappers kept so
//!   a caller does not have to name a marker.
//!
//! * The memory door ([`remember`] / [`recall`] / [`MemoryMirror`], below) is the same idea for the
//!   per-profile, per-item lists a profile builds up (the subtitle offsets are its first tenant):
//!   plex owns the schema and the store (`plex::session::prefs::ProfileList`), and this is the
//!   in-memory copy the player reads, written through on the main thread so a read is never older
//!   than the last pick. [`restore_install_rows`] reseeds it. Both doors need a seat (a profile to
//!   remember FOR).
//!
//! **What `restore_*` does NOT do** is write: it is the in-memory half only, used where the value
//! comes FROM the record (a legacy session's missing quality stays missing until an explicit
//! choice, so a future Auto-ready build cannot reinterpret an old install). The same holds for the
//! frozen top-level keys: no pick in this module ever writes one.
use super::plan::{supported_quality, Quality, QUALITY_LADDER};
use plx_plex::plex::session::{
    DolbyVisionMode, DolbyVisionModePref, InstantDpadSeeking, InstantDpadSeekingPref, effective, own_prefs, persist_profile_pref, AudioEnhancementsPref, DeckPress, DeckPressPref, DirectPlayMode,
    DirectPlayModePref, HdrOverlayBrightness, HdrOverlayBrightnessPref, NextEpisodeMode, NextEpisodeModePref, Preferences, ProfilePrefs, ProfilePrefsRecord,
    ProfileSetting, ProfileWrite, QualityPref, Row, Setting, SkipInterval, SkipIntervalPref, SubtitlePosition,
    SubtitlePositionPref, SubtitleSize, SubtitleSizePref, SubtitleTone, SubtitleTonePref, TrailerAutoplayPref, UnderPosters,
    UnderPostersPref,
};
use plx_plex::plex::session::prefs::{
    MemoryEntry, ProfileKeyed, ProfileList, ProfileMemory, ProfileRecords, SettingsWrite, TrackMemoryEntry,
};
use plx_plex::plex::session::{self, Seat, Session, SubtitleOffsetEntry};
use plx_plex::plex::AudioEnhancements;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

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
packed_by_index!(DolbyVisionMode = 0, InstantDpadSeeking = 0, NextEpisodeMode = 0, SkipInterval = 1, DeckPress = 0, SubtitleSize = 1, SubtitlePosition = 0, SubtitleTone = 0, HdrOverlayBrightness = 1);

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

/// Under posters is one byte: the title switch in bit 0, the details' ladder index in bits 1-3
/// and "sort value Off" in bit 4 ([`UnderPosters::pack`]). The seed, 0, is the default (no title,
/// year and length, sort value On), so a read
/// before any restore is what the install did before the setting existed.
impl Packed for UnderPosters {
    const SEED: u8 = 0;
    fn to_u8(self) -> u8 {
        self.pack()
    }
    fn from_u8(b: u8) -> Self {
        UnderPosters::unpack(b)
    }
}

/// The hero trailer switch is one byte, 0 or 1. The seed is ON, the product default, so a read before
/// any restore is what the install did before the setting existed.
impl Packed for bool {
    const SEED: u8 = 1;
    fn to_u8(self) -> u8 {
        self as u8
    }
    fn from_u8(b: u8) -> Self {
        b != 0
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

live_cell!(pub(super) INSTANT_DPAD_SEEKING: InstantDpadSeeking, "instant D-pad seeking");
live_cell!(pub(super) DOLBY_VISION_MODE: DolbyVisionMode, "Dolby Vision mode");

// ---- the cells ------------------------------------------------------------------------------

live_cell!(
    /// The seated profile's Direct Play mode; each resolve captures its own immutable copy.
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
    /// What OK does on a Continue Watching card: play, or open its page (a movie's own; an
    /// episode's SHOW, on the card's season). Read by every deck's press and draw (Home's and
    /// the Library's) and by the item menu when it builds its rows; `DeckPress::press_plays` is the
    /// one question they all ask of it.
    pub(super) DECK_PRESS: DeckPress, "deck press preference"
);
live_cell!(
    /// What sits under the focused poster (Settings > Libraries > Under posters). Read by every
    /// poster grid and row each frame, through `route::under_posters`.
    pub(super) UNDER_POSTERS: UnderPosters, "under posters preference"
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
    /// How bright the player's overlay and captions are drawn while the picture is HDR. Read once per
    /// frame by the app's player draw (`route::overlay_gain`).
    pub(super) HDR_OVERLAY_BRIGHTNESS: HdrOverlayBrightness, "HDR overlay brightness preference"
);
live_cell!(
    /// The seated profile's current quality pick. An atomic rather than a field on `Session` because it
    /// OUTLIVES a playback (it is a preference, not session state) and because `appkit::more_menu`
    /// reads it to draw the checkmark while `ResolveEnv::snapshot` reads it to hand the worker a
    /// copy.
    pub(super) QUALITY: Quality, "the playback quality ceiling"
);

// ---- the rows -------------------------------------------------------------------------------

/// A live per-profile [`Setting`]: what the media layer adds to the persisted row.
pub trait LiveSetting: ProfileSetting {
    /// The value in the cell now.
    fn live() -> Self::Value;
    /// Publish `v` into the cell. Prefer [`choose`] or [`restore_profile_rows`]: this does not persist.
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
    /// A value a caller of [`restore_profile_rows`] forces over the record's (the dev quality override).
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
live_setting!(InstantDpadSeekingPref, INSTANT_DPAD_SEEKING, "instant D-pad seeking");
live_setting!(DolbyVisionModePref, DOLBY_VISION_MODE, "Dolby Vision mode");
live_setting!(DeckPressPref, DECK_PRESS, "deck press");
live_setting!(UnderPostersPref, UNDER_POSTERS, "under posters");
live_setting!(SubtitleSizePref, SUBTITLE_SIZE, "subtitle size");
live_setting!(SubtitlePositionPref, SUBTITLE_POSITION, "subtitle position");
live_setting!(HdrOverlayBrightnessPref, HDR_OVERLAY_BRIGHTNESS, "HDR overlay brightness");
live_setting!(SubtitleTonePref, crate::player::SUBTITLE_TONE, "subtitle tone");
live_setting!(AudioEnhancementsPref, crate::player::AUDIO_ENHANCEMENTS, "audio enhancements");
live_setting!(TrailerAutoplayPref, crate::player::preview::TRAILER_AUTOPLAY, "hero trailer autoplay");
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

/// A live row as data, so a test (and [`restore_profile_rows`]) can walk them all.
#[derive(Clone, Copy)]
pub struct LiveRow {
    pub key: &'static str,
    /// Always `S::LIVE`; carried so the tie test compares the same `{key: live}` map on both sides.
    pub live: bool,
    /// Always `S::SCOPE`; every live row is `Profile` since the nine moved (T-live checks it).
    pub scope: plx_plex::plex::session::prefs::Scope,
    restore: fn(&Preferences, Option<&ProfilePrefs>, &Overrides),
}

impl LiveRow {
    pub const fn of<S: LiveSetting>() -> LiveRow {
        LiveRow { key: S::KEY, live: S::LIVE, scope: S::SCOPE, restore: restore_row::<S> }
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
    LiveRow::of::<HdrOverlayBrightnessPref>(),
    LiveRow::of::<NextEpisodeModePref>(),
    LiveRow::of::<SkipIntervalPref>(),
    LiveRow::of::<InstantDpadSeekingPref>(),
    LiveRow::of::<DolbyVisionModePref>(),
    LiveRow::of::<DeckPressPref>(),
    LiveRow::of::<UnderPostersPref>(),
    LiveRow::of::<AudioEnhancementsPref>(),
    LiveRow::of::<TrailerAutoplayPref>(),
];

// ---- restore --------------------------------------------------------------------------------

/// What a caller of [`restore_profile_rows`] forces over the record. Each caller computes it
/// exactly as it did when it spelled the list out: boot passes none when the harness is controlled
/// and the dev quality override otherwise; the seat trigger passes the override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub quality: Option<Quality>,
}

fn restore_row<S: LiveSetting>(legacy: &Preferences, own: Option<&ProfilePrefs>, ov: &Overrides) {
    restore_live::<S>(S::overridden(ov).unwrap_or_else(|| effective::<S>(legacy, own)));
}

fn restore_live<S: LiveSetting>(v: S::Value) {
    S::publish(S::normalize(v));
}

/// **Reseed the mirrors from `record`** (the memory door's lists and the profiles' own player
/// values) without writing anything back. Boot and the credentials handoff after a sign-in call it;
/// it puts no cell anywhere (that is the seat's: [`restore_profile_rows`]).
///
/// The mirrors are reseeded only when the tenure or the account stamp they were seeded under has
/// moved ([`KeyedMirror::reseed`]); a restore that changes neither leaves them alone, because the
/// mirror is AHEAD of the file by exactly the writes still queued.
pub fn restore_install_rows(record: &Session) {
    PROFILE_PREFS.reseed(record);
    for row in MEMORY_ROWS {
        (row.reseed)(record);
    }
}

/// **Put every live cell where `seat` says**, without writing anything back: the seated profile's
/// own value of each setting from the mirror, else the legacy install-wide key in `record`, else the
/// type's default ([`effective`]); `Unseated` has nobody to own a value and reads the legacy one.
/// A legacy session's missing quality resolves to Original but stays missing on disk until the
/// profile makes an explicit choice, so a future Auto-ready build still cannot reinterpret that old
/// install. `ov` is forced over the result.
///
/// The one restore of the player's cells. It runs when the seat CHANGES (a republication of the same
/// seat restores nothing, so a pick in flight is not clobbered) and once at process start with
/// `Unseated`. `record` supplies the frozen legacy keys and reseeds the profile mirror if its tenure
/// or stamp moved; the profile's own values are read from the mirror, never from `record`, because
/// the mirror is ahead of a file that still has writes queued.
pub fn restore_profile_rows(record: &Session, seat: &Seat, ov: Overrides) {
    PROFILE_PREFS.reseed(record);
    let own = PROFILE_PREFS.own(seat);
    for row in LIVE_ROWS {
        (row.restore)(&record.prefs, own.as_ref(), &ov);
    }
}

// ---- the memory door ------------------------------------------------------------------------

/// A tenant of the memory door: a [`MemoryEntry`] that has a process-global mirror here.
pub trait MemoryTenant: MemoryEntry + Send + 'static {
    fn mirror() -> &'static MemoryMirror<Self>;
    /// The name in the one refusal line.
    fn log_label() -> &'static str;
}

/// What a mirror was seeded under: the revocation tenure, and the account stamp
/// (`grant::account_key` of the account token).
type Seed = (u64, String);

struct MirrorState<R> {
    /// `None` until the first [`KeyedMirror::reseed`]: a mirror nobody restored answers nothing.
    seeded: Option<Seed>,
    lists: Vec<R>,
}

/// **The in-memory copy of one per-profile list**, seeded from the session at
/// [`restore_install_rows`] and written through by [`remember`] (the memory door) or [`choose`] (the
/// profiles' own player values) on the caller's (main) thread, so a read sees a pick the moment it
/// is made instead of after the queued file write lands.
///
/// Generic over the record ([`ProfileKeyed`]), because two shapes share it: a [`ProfileList`] of
/// remembered entries ([`MemoryMirror`]) and a [`ProfilePrefsRecord`] of one profile's own settings.
///
/// It is valid only for the tenure it was seeded under: a sign-out or account replacement moves
/// the tenure, after which a read sees nothing and a write is refused until the next restore
/// reseeds it. The stamp changes with a new sign-in WITHOUT a tenure change (the token is replaced,
/// nothing was revoked), and the restore that follows the handoff reseeds on that too.
pub struct KeyedMirror<R: ProfileKeyed + Clone> {
    state: Mutex<MirrorState<R>>,
}

/// The mirror of a memory-door tenant's list.
pub type MemoryMirror<E> = KeyedMirror<ProfileList<E>>;

impl<R: ProfileKeyed + Clone> KeyedMirror<R> {
    pub const fn new() -> Self {
        KeyedMirror { state: Mutex::new(MirrorState { seeded: None, lists: Vec::new() }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MirrorState<R>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Seed from `record`'s list if the mirror was never seeded or was seeded under another tenure
    /// or account stamp; otherwise leave it, since it already holds everything the record does
    /// plus the writes still in flight.
    pub fn reseed(&self, record: &Session) {
        let seed = (session::tenure(), plx_plex::plex::grant::account_key(&record.account_token));
        let mut state = self.lock();
        if state.seeded.as_ref() != Some(&seed) {
            state.lists = R::slot(record).clone();
            state.seeded = Some(seed);
        }
    }

    /// Whether the mirror was seeded under the tenure that stands now.
    fn valid(state: &MirrorState<R>) -> bool {
        state.seeded.as_ref().is_some_and(|(tenure, _)| *tenure == session::tenure())
    }
}

impl<E: MemoryEntry> MemoryMirror<E> {
    /// The account stamp the mirror was seeded under: the one its reads and writes go through, so
    /// an [`ACCOUNT_BOUND`](MemoryEntry::ACCOUNT_BOUND) list (offsets, track memory) shows this
    /// account's records and not another's. A tenant that is not account-bound keeps no stamp, as
    /// it never had one.
    fn stamp(state: &MirrorState<ProfileList<E>>) -> &str {
        match &state.seeded {
            Some((_, stamp)) if E::ACCOUNT_BOUND => stamp,
            _ => "",
        }
    }
}

/// The profiles' own setting values, as of the last pick (not the last write that
/// landed): what [`restore_profile_rows`] reads, because `session::peek()` is write-after and a
/// switch back to a profile must not show a pre-pick value.
static PROFILE_PREFS: KeyedMirror<ProfilePrefsRecord> = KeyedMirror::new();

impl KeyedMirror<ProfilePrefsRecord> {
    /// The seated profile's own values; nothing while unseated, never seeded, or seeded before a
    /// sign-out.
    fn own(&self, seat: &Seat) -> Option<ProfilePrefs> {
        let Seat::Seated { user, stamp } = seat else { return None };
        let state = self.lock();
        if !Self::valid(&state) {
            return None;
        }
        own_prefs(&state.lists, user, stamp).cloned()
    }

    /// Record the pick for the profile that made it, whoever is seated now. Refused (and the live
    /// value still published by the caller) while the mirror is not valid for the current tenure.
    fn put<S: ProfileSetting>(&self, write: &ProfileWrite, v: S::Value) {
        let mut state = self.lock();
        if Self::valid(&state) {
            plx_plex::plex::session::put_own::<S>(&mut state.lists, write.user(), write.stamp(), v);
        }
    }
}

/// Seed the profiles' own-values mirror from `record` whatever it was seeded under before, so a test
/// does not inherit another test's picks (the mirror is a process global, and a restore under an
/// unchanged tenure and stamp deliberately keeps it).
#[cfg(any(test, feature = "test-support"))]
pub fn seed_profile_prefs_for_test(record: &Session) {
    unseed_profile_prefs_for_test();
    PROFILE_PREFS.reseed(record);
}

/// Put the profiles' own-values mirror back to never-seeded, which is how a process looks before its
/// first restore.
#[cfg(any(test, feature = "test-support"))]
pub fn unseed_profile_prefs_for_test() {
    let mut state = PROFILE_PREFS.lock();
    state.seeded = None;
    state.lists.clear();
}

/// What the profiles' own-values mirror holds now (the picks, not the file).
#[cfg(any(test, feature = "test-support"))]
pub fn profile_prefs_mirror_for_test() -> Vec<ProfilePrefsRecord> {
    PROFILE_PREFS.lock().lists.clone()
}

/// A mirror as data, so [`restore_install_rows`] can walk them all.
#[derive(Clone, Copy)]
pub struct MemoryRow {
    reseed: fn(&Session),
}

impl MemoryRow {
    pub const fn of<E: MemoryTenant>() -> MemoryRow {
        MemoryRow { reseed: reseed_mirror::<E> }
    }
}

fn reseed_mirror<E: MemoryTenant>(record: &Session) {
    E::mirror().reseed(record);
}

/// Every tenant of the memory door, once. (The profiles' own player values are reseeded beside them.)
pub const MEMORY_ROWS: &[MemoryRow] = &[MemoryRow::of::<SubtitleOffsetEntry>(), MemoryRow::of::<TrackMemoryEntry>()];

static SUBTITLE_OFFSETS: MemoryMirror<SubtitleOffsetEntry> = MemoryMirror::new();
static TRACK_MEMORY: MemoryMirror<TrackMemoryEntry> = MemoryMirror::new();

impl MemoryTenant for SubtitleOffsetEntry {
    fn mirror() -> &'static MemoryMirror<Self> {
        &SUBTITLE_OFFSETS
    }
    fn log_label() -> &'static str {
        "subtitle offset"
    }
}

/// **Remember a change to one entry of a per-profile list: the memory door's write.** `f` turns the
/// entry that is there NOW (`None` if none) into the one to hold (`None`, or an entry that
/// [`forgets`](MemoryEntry::forgets), removes it). It is applied twice, and that is the point: to
/// the mirror right here on the caller's thread, so [`recall`] sees it at once, and as a DELTA
/// inside the queued session update, against whatever the file holds when that runs (a half-entry
/// merges with the file's other half, and a failed write followed by the same pick writes again).
///
/// The queued write is `session::queue_update`, so its guard stands: it is dropped if the tenure
/// moved, or the client id or account token changed, between queue and run. This is account data.
/// A pick whose file write fails or is dropped by that guard stays recalled (the mirror is not taken
/// back) until the next restart or tenure/stamp reseed.
///
/// Refused (returns `false`; neither the mirror nor the file changes) when the mirror is not valid
/// for the current tenure: never seeded, or seeded before a sign-out. Otherwise the entry would be
/// visible in a session whose file write is refused.
///
/// **Refused without a seat for `user`.** `user` is the key the caller read from
/// `session::current_profile_key()`, where `""` is both the account owner and "no profile
/// published"; the memory door asks the seat instead, so a run that never seated anybody (a dev
/// token, a replay) cannot write to the owner's record of whatever account is on disk.
///
/// Both tenants are account-bound, so the mirror write and the queued file write carry the account
/// stamp (the mirror's, and the file's own `account_key`) and [`recall`] reads under it. `track_memory`
/// is read through [`recall`] and [`recall_all`] and written here, by `commit_audio_pick` and
/// `commit_subtitle_pick` (the track menu's pick) and by nothing else but the harness's
/// `plxnative-trackmemory=clear` and `=fill` (`dev::scenarios::arm_track_memory`), which remove and
/// write entries through this same door.
pub fn remember<E: MemoryTenant>(
    user: &str,
    key: E::Key,
    f: impl Fn(Option<&E>) -> Option<E> + Clone + Send + 'static,
) -> bool {
    if !session::is_seated_as(user) {
        plx_base::eventlog::log(&format!("{}: not remembered — no profile seated", E::log_label()));
        return false;
    }
    {
        let mut state = E::mirror().lock();
        if !MemoryMirror::valid(&state) {
            drop(state);
            plx_base::eventlog::log(&format!("{}: not remembered — no valid session", E::log_label()));
            return false;
        }
        let stamp = MemoryMirror::stamp(&state).to_string();
        state.lists.upsert_with(user, &stamp, &key, f.clone());
    }
    let user = user.to_string();
    session::queue_update(move |cur| {
        // The file's own account: the guard has already dropped this edit if it is not the one
        // the mirror was seeded under.
        let stamp = if E::ACCOUNT_BOUND { plx_plex::plex::grant::account_key(&cur.account_token) } else { String::new() };
        cur.with_memory::<E>(|m| m.upsert_with(&user, &stamp, &key, f))
    })
}

impl MemoryTenant for TrackMemoryEntry {
    fn mirror() -> &'static MemoryMirror<Self> {
        &TRACK_MEMORY
    }
    fn log_label() -> &'static str {
        "track memory"
    }
}

/// Seed `E`'s mirror from `record` whatever it was seeded under before, so a test does not inherit
/// another test's remembered entries (the mirror is a process global, and a restore under an
/// unchanged tenure and stamp deliberately keeps it).
#[cfg(any(test, feature = "test-support"))]
pub fn seed_memory_for_test<E: MemoryTenant>(record: &Session) {
    unseed_memory_for_test::<E>();
    E::mirror().reseed(record);
}

/// Put `E`'s mirror back to never-seeded, which is how a process looks before its first restore.
#[cfg(any(test, feature = "test-support"))]
pub fn unseed_memory_for_test<E: MemoryTenant>() {
    let mut state = E::mirror().lock();
    state.seeded = None;
    state.lists.clear();
}

/// **What this profile remembered for `key`**, as of the last [`remember`] (not the last write that
/// landed). Nothing when the mirror was never seeded or the tenure has moved since it was, and
/// nothing unless `user` is the seated profile.
pub fn recall<E: MemoryTenant>(user: &str, key: &E::Key) -> Option<E> {
    if !session::is_seated_as(user) {
        return None;
    }
    let state = E::mirror().lock();
    if !MemoryMirror::valid(&state) {
        return None;
    }
    state.lists.get(user, MemoryMirror::stamp(&state), key).cloned()
}

/// **Everything this profile remembered**, oldest first, as of the last [`remember`]: the whole
/// list, for a reader that does not know its key yet (a play raised off a shelf does not know its
/// show until the item is fetched). Nothing when the mirror was never seeded or the tenure has
/// moved since it was, nothing unless `user` is the seated profile (the seat gate of [`recall`]: an
/// unseated boot or a dev token reads no track memory), and for an account-bound tenant nothing
/// without a stamp. It is the record
/// stamped as the mirror is: a tenant that [adopts](MemoryEntry::ADOPTS_UNSTAMPED) unstamped
/// records does not show the ones not yet adopted here (only [`recall`] falls back to them), and
/// only `track_memory` is read this way.
pub fn recall_all<E: MemoryTenant>(user: &str) -> Vec<E> {
    if !session::is_seated_as(user) {
        return Vec::new();
    }
    let state = E::mirror().lock();
    if !MemoryMirror::valid(&state) {
        return Vec::new();
    }
    let account = MemoryMirror::stamp(&state);
    if E::ACCOUNT_BOUND && account.is_empty() && !E::ADOPTS_UNSTAMPED {
        return Vec::new();
    }
    state.lists.position_of(user, &account).map(|at| state.lists[at].items.clone()).unwrap_or_default()
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

/// Declares every Settings row's pick in one list: each arm names a variant and the marker whose
/// value type it carries. The macro generates the enum and [`Choice::KEYS`] from that list, so a
/// variant cannot exist without its setting's key being listed (`prefs_tests::t_settings_*`).
macro_rules! choices {
    ($($variant:ident => $marker:ident),* $(,)?) => {
        /// One Settings pick: which live setting, and its typed value. The Settings screen sends
        /// one of these; the app maps it to [`choose`] in one place.
        pub enum Choice {
            $($variant(<$marker as Setting>::Value)),*
        }
        impl Choice {
            /// The key of every setting a Settings row saves through [`Choice`], in declaration order.
            pub const KEYS: &'static [&'static str] = &[$(<$marker as Row>::KEY),*];
        }
    };
}

choices! {
    Quality => QualityPref,
    DirectPlay => DirectPlayModePref,
    NextEpisode => NextEpisodeModePref,
    SkipInterval => SkipIntervalPref,
    InstantDpadSeeking => InstantDpadSeekingPref,
    DolbyVision => DolbyVisionModePref,
    DeckPress => DeckPressPref,
    UnderPosters => UnderPostersPref,
    TrailerAutoplay => TrailerAutoplayPref,
    SubtitleSize => SubtitleSizePref,
    SubtitlePosition => SubtitlePositionPref,
    HdrOverlayBrightness => HdrOverlayBrightnessPref,
}

/// **The one write door for a live setting.** `v` is normalized first, so the clamp happens before
/// publish and before persist. The seat is captured HERE, on the caller's thread: the pick is the
/// seated profile's whoever is seated when the worker runs, and it is refused when nobody is
/// ([`Seat::Unseated`]).
pub fn choose<S: LiveSetting>(v: S::Value, how: How) {
    choose_via::<S>(v, how, S::normalize);
}

fn choose_via<S: LiveSetting>(v: S::Value, how: How, normalize: impl FnOnce(S::Value) -> S::Value) {
    let v = normalize(v);
    let Some(write) = ProfileWrite::capture() else {
        // Nobody to own the pick: nothing is persisted and nothing is claimed. An optimistic pick
        // still shows live (for this seat only: the next seat change restores over it).
        plx_base::eventlog::log(&format!("prefs: {} not saved — no profile seated", S::KEY));
        match how {
            How::Confirm(reply) => {
                let _ = reply.send(false);
            }
            How::Optimistic(reply) => {
                S::publish(v);
                plx_machine::idle::invalidate();
                if let Some(reply) = reply {
                    let _ = reply.send(false);
                }
            }
        }
        return;
    };
    match how {
        How::Confirm(reply) => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let saved = confirm_with::<S>(&write, v);
                let _ = reply.send(saved);
                plx_machine::idle::invalidate();
            });
        }
        How::Optimistic(reply) => {
            PROFILE_PREFS.put::<S>(&write, v);
            S::publish(v);
            plx_machine::idle::invalidate();
            let _ = plx_base::storage_worker::submit_retained(move || {
                let saved = persist_profile_pref::<S>(&write, v);
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
/// tests, which grade it without a thread): persist for the profile seated NOW, then publish and
/// report only if the write is durable. `true` also when the record already held the value (nothing
/// to write, nothing to refuse). `false`, with nothing written, while nobody is seated.
pub fn confirm<S: LiveSetting>(v: S::Value) -> bool {
    confirm_via::<S>(v, S::normalize)
}

fn confirm_via<S: LiveSetting>(v: S::Value, normalize: impl FnOnce(S::Value) -> S::Value) -> bool {
    let v = normalize(v);
    match ProfileWrite::capture() {
        Some(write) => confirm_with::<S>(&write, v),
        None => {
            plx_base::eventlog::log(&format!("prefs: {} not saved — no profile seated", S::KEY));
            false
        }
    }
}

/// [`confirm`] for the profile `write` captured. Durable: the pick is recorded in the mirror under
/// the profile that made it WHATEVER is seated now (a switch back to it must show the pick), and
/// published to the cells only if that profile still is the seated one. The reply is true either way.
fn confirm_with<S: LiveSetting>(write: &ProfileWrite, v: S::Value) -> bool {
    let saved = persist_profile_pref::<S>(write, v);
    if saved {
        PROFILE_PREFS.put::<S>(write, v);
        if session::current_seat() == write.seat() {
            S::publish(v);
        }
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

pub fn instant_dpad_seeking() -> InstantDpadSeeking { InstantDpadSeekingPref::live() }
pub fn restore_instant_dpad_seeking(seeking: InstantDpadSeeking) { restore_live::<InstantDpadSeekingPref>(seeking) }

pub fn dolby_vision_mode() -> DolbyVisionMode { DolbyVisionModePref::live() }
pub fn restore_dolby_vision_mode(mode: DolbyVisionMode) { restore_live::<DolbyVisionModePref>(mode) }

pub fn skip_interval() -> SkipInterval { SkipIntervalPref::live() }
pub fn restore_skip_interval(interval: SkipInterval) { restore_live::<SkipIntervalPref>(interval) }
/// Blocking persistence seam, as [`set_next_episode_mode`].
pub fn set_skip_interval(interval: SkipInterval) -> bool { confirm::<SkipIntervalPref>(interval) }

pub fn deck_press() -> DeckPress { DeckPressPref::live() }
pub fn restore_deck_press(mode: DeckPress) { restore_live::<DeckPressPref>(mode) }
/// Blocking persistence seam, as [`set_next_episode_mode`].
pub fn set_deck_press(mode: DeckPress) -> bool { confirm::<DeckPressPref>(mode) }

/// What sits under the focused poster, for the seated profile. Safe from any thread; a reader takes
/// no lock.
pub fn under_posters() -> UnderPosters { UnderPostersPref::live() }
pub fn restore_under_posters(v: UnderPosters) { restore_live::<UnderPostersPref>(v) }
/// Blocking persistence seam, as [`set_next_episode_mode`].
pub fn set_under_posters(v: UnderPosters) -> bool { confirm::<UnderPostersPref>(v) }

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

pub fn hdr_overlay_brightness() -> HdrOverlayBrightness { HdrOverlayBrightnessPref::live() }
pub fn restore_hdr_overlay_brightness(brightness: HdrOverlayBrightness) {
    restore_live::<HdrOverlayBrightnessPref>(brightness)
}
/// [`select_subtitle_size`] for the HDR overlay brightness.
pub fn select_hdr_overlay_brightness(brightness: HdrOverlayBrightness, reply: Option<Sender<bool>>) {
    choose::<HdrOverlayBrightnessPref>(brightness, How::Optimistic(reply));
}

/// Whether the picture the viewer SEES is HDR, given the playing item's HDR flag and the route.
///
/// The flag is the source's. A direct play or a remux (`transcoding && remux`) keeps the video
/// stream as it is, so the TV is shown HDR; a server re-encode (`transcoding && !remux`) is
/// tone-mapped to SDR by the server's default, so the overlay needs no dimming.
pub fn hdr_picture(source_hdr: bool, transcoding: bool, remux: bool) -> bool {
    source_hdr && (!transcoding || remux)
}

/// The RGB multiplier the player's overlay and subtitles are drawn with: `1.0` unless the picture
/// is HDR (see [`hdr_picture`]), else the viewer's [`HdrOverlayBrightness`] rung. The pure core of
/// [`overlay_gain`], so the rule is testable without a session.
pub fn overlay_gain_for(source_hdr: bool, transcoding: bool, remux: bool, pick: HdrOverlayBrightness) -> f32 {
    if hdr_picture(source_hdr, transcoding, remux) { pick.factor() } else { 1.0 }
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
