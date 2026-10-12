//! **The install-wide player settings, declared once.**
//!
//! [`Preferences`] is the nine player scalars that used to be spelled, field for field, in
//! [`Session`] AND in the canonical (DB8) preference record: the struct field, the manual
//! `Default`, the copy in and out of the canonical form, the getter and a `with_<x>` builder, ten
//! sites per setting. It is flattened into BOTH on-disk forms (`#[serde(flatten)]`, declared before
//! each form's own `extensions` so that a flattened struct takes its keys first and the unknown-key
//! map still sees only keys nobody models), so the legacy file and the canonical public
//! preferences carry exactly the keys they always did, under exactly the attributes they always had.
//!
//! **Every serde attribute below is copied character for character from where it used to sit**,
//! and no two parse policies were unified: each row keeps the soft parse it was written with
//! (`Setting::PARSE` names it, and `prefs_tests::t_parse_each_row_keeps_its_declared_policy` drives it). A reader of the wire format
//! never sees this module.
//!
//! One [`Setting`] impl per field, keyed by a MARKER type (never by the value type: two settings
//! of one type, or a stored `Option` read as a plain value, would collide). It carries the key,
//! the `Stored` and `Value` types, the [`Scope`], whether the row is account-bound, whether the
//! media layer applies it live, how a Settings row saves it, and its parse policy. [`DESCRIPTORS`]
//! lists them, and the tests hold that table to the struct and to platform's account-bound list,
//! both ways, so a field without a row (or a row without a field) fails instead of drifting.
use super::{
    DeckPress, DirectPlayMode, NextEpisodeMode, PlaybackQuality, Session, SkipInterval,
    SubtitlePosition, SubtitleSize, SubtitleTone,
};
use serde::{Deserialize, Deserializer, Serialize};

/// The nine install-wide player settings, flattened into [`Session`] and into the canonical
/// preference record. Defaults are each type's own `Default`, which A0 pins as equal to the soft
/// parse's fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Preferences {
    /// The install's playback-quality preference. `None` is deliberately distinct from an
    /// explicit value: every session written before this field existed lands there and must keep
    /// the old **Original** behaviour rather than being migrated onto automatic playback.
    ///
    /// A newly-created session writes an explicit default through [`PlaybackQuality::fresh_default`].
    /// That default may become Auto only when the playback owner exposes a positive readiness
    /// gate. The integrated HLS prime/swap path opens it for fresh installs; old files remain
    /// Original because their absent field is not reinterpreted. Unknown or malformed future
    /// values soften to `None`, and therefore Original,
    /// instead of making the credentials file fail to parse.
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub playback_quality: Option<PlaybackQuality>,
    /// Install-wide original-stream override; malformed/future values remain automatic.
    #[serde(default, deserialize_with = "de_soft_direct_play_mode")]
    pub direct_play_mode: DirectPlayMode,
    /// **How bright the client-rendered subtitles are drawn** — white, or a rung of the grey
    /// ladder under it. Install-wide like [`Preferences::playback_quality`], because it answers a fact
    /// about the PANEL (an HDR picture maps graphics white to a searing level) rather than about
    /// whoever is watching. Absence is white, which is what every build before the field drew.
    /// Soft-parsed: an unknown spelling costs the preference, never the credentials.
    #[serde(default, deserialize_with = "de_soft_subtitle_tone")]
    pub subtitle_tone: SubtitleTone,
    /// **The client-rendered subtitle caption's text size.** Install-wide like
    /// [`Preferences::subtitle_tone`]. Absence is Medium — see [`SubtitleSize`]'s own doc. Skipped at
    /// the default so a session predating this field — committed replay fixtures included —
    /// serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_subtitle_size", skip_serializing_if = "is_default_subtitle_size")]
    pub subtitle_size: SubtitleSize,
    /// **The client-rendered subtitle caption's vertical placement.** Install-wide like
    /// [`Preferences::subtitle_tone`]. Absence is Low — see [`SubtitlePosition`]'s own doc. Skipped at
    /// the default for the same reason as [`Preferences::subtitle_size`].
    #[serde(default, deserialize_with = "de_soft_subtitle_position", skip_serializing_if = "is_default_subtitle_position")]
    pub subtitle_position: SubtitlePosition,
    /// **What the player does when an episode with a successor reaches its credits** — see
    /// [`NextEpisodeMode`]. Install-wide like [`Preferences::playback_quality`]. Absence is
    /// Countdown, which is what every build before the field did. Skipped at the default so a
    /// session predating this field — committed replay fixtures included — serializes exactly as
    /// it did before.
    #[serde(default, deserialize_with = "de_soft_next_episode_mode", skip_serializing_if = "is_default_next_episode_mode")]
    pub next_episode_mode: NextEpisodeMode,
    /// **How far one Left/Right press jumps in the player** — see [`SkipInterval`]. Install-wide
    /// like [`Preferences::playback_quality`], one value for both directions. Absence is 10 s, the
    /// hop every build before the field made. Skipped at the default so a session predating this
    /// field — committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_skip_interval", skip_serializing_if = "is_default_skip_interval")]
    pub skip_interval: SkipInterval,
    /// **What OK does on a Continue Watching card** — see [`DeckPress`]. Install-wide like
    /// [`Preferences::playback_quality`]. Absence is Details, so a session written before the field
    /// existed opens the page on OK. Skipped at the default so a session predating this field —
    /// committed replay fixtures included — serializes exactly as it did before.
    #[serde(default, deserialize_with = "de_soft_deck_press", skip_serializing_if = "is_default_deck_press")]
    pub deck_press: DeckPress,
    /// Plex Pass per-track transcoder DSP preference (issue #266): dialog boost / loudness
    /// normalization. `NONE` by default and skipped on write while `NONE`, so every session
    /// persisted before the field existed — and every viewer who never opted in — round-trips
    /// byte-identical to what it wrote before. Restored into `player::audio_enhancements` at boot
    /// and at the credentials handoff; written by `player::set_audio_enhancements`. Soft-parsed
    /// like every preference in this struct: an unknown shape costs the preference, never the
    /// credentials.
    #[serde(default, deserialize_with = "de_soft_audio_enhancements", skip_serializing_if = "crate::plex::AudioEnhancements::is_none")]
    pub audio_enhancements: crate::plex::AudioEnhancements,
}

/// How long a setting's value lives, and who owns the write. Every row today is `Install`; the
/// other scopes are declared so the descriptor vocabulary is complete before the rows that need
/// them land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The television: shared by every profile, kept across sign-out, never account-bound.
    Install,
    /// The signed-in account: cleared with the credentials on sign-out.
    Account,
    /// One Plex Home profile.
    Profile,
    /// One show, for one profile.
    PerShow,
    /// One item, for one profile.
    PerItem,
}

/// Which soft-parse policy a row's `deserialize_with` implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parse {
    /// Garbage reads as the type's `Default` (the scalar enums).
    Soft,
    /// A list parses element by element: the good elements are kept (`de_soft_vec`).
    SoftVec,
    /// A switch whose product default is on: garbage reads as `true` (`de_soft_bool_on`).
    BoolOn,
    /// A switch whose product default is off: garbage reads as `false` (`de_soft_bool`).
    BoolOff,
    /// Garbage reads as `None` (quality, the hero blur).
    OptionalSoft,
}

/// How a Settings row saves the value. `Confirm` claims nothing until the write is durable;
/// `Optimistic` publishes first and persists in order behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsWrite {
    Confirm,
    Optimistic,
}

/// One setting, keyed by a marker type.
pub trait Setting: 'static {
    /// The field's type in [`Preferences`].
    type Stored;
    /// What callers speak. Equal to `Stored` except where the stored form carries "never chosen"
    /// (quality: `Option<PlaybackQuality>` stored, `PlaybackQuality` spoken).
    type Value: Copy + PartialEq + Send + 'static;
    /// The on-disk key (a serde rename is honoured here).
    const KEY: &'static str;
    const SCOPE: Scope;
    /// Cleared with the credentials at sign-out (`storage::state::ACCOUNT_BOUND_PREFERENCES`).
    const ACCOUNT_BOUND: bool;
    /// The media layer applies the value live (a cell restored at boot and at the handoff).
    const LIVE: bool;
    /// How a Settings row saves it; `None` when no Settings row exists (the player panel owns it).
    const SETTINGS: Option<SettingsWrite>;
    const PARSE: Parse;
    fn stored(p: &Preferences) -> &Self::Stored;
    fn stored_mut(p: &mut Preferences) -> &mut Self::Stored;
    /// quality: `unwrap_or(Original)`.
    fn get(p: &Preferences) -> Self::Value;
    /// quality: `Some(v)`.
    fn to_stored(v: Self::Value) -> Self::Stored;
    fn put(p: &mut Preferences, v: Self::Value) {
        *Self::stored_mut(p) = Self::to_stored(v);
    }
}

/// A marker and its trivial impl, for a row whose `Stored` and `Value` are one type. It carries no
/// schema: the serde attributes stay on the [`Preferences`] field, where rustfmt and a reviewer
/// can see them.
macro_rules! setting {
    ($marker:ident: $ty:ty = $field:ident, $key:literal, $scope:ident,
     account $bound:literal, live $live:literal, settings $settings:expr, parse $parse:ident) => {
        pub struct $marker;
        impl Setting for $marker {
            type Stored = $ty;
            type Value = $ty;
            const KEY: &'static str = $key;
            const SCOPE: Scope = Scope::$scope;
            const ACCOUNT_BOUND: bool = $bound;
            const LIVE: bool = $live;
            const SETTINGS: Option<SettingsWrite> = $settings;
            const PARSE: Parse = Parse::$parse;
            fn stored(p: &Preferences) -> &$ty { &p.$field }
            fn stored_mut(p: &mut Preferences) -> &mut $ty { &mut p.$field }
            fn get(p: &Preferences) -> $ty { p.$field }
            fn to_stored(v: $ty) -> $ty { v }
        }
    };
}

/// Playback quality. `Stored = Option<PlaybackQuality>` (absent means "never chosen", read as
/// Original) and `Value = PlaybackQuality`: hand-written because it is the one row whose two types
/// differ.
pub struct QualityPref;
impl Setting for QualityPref {
    type Stored = Option<PlaybackQuality>;
    type Value = PlaybackQuality;
    const KEY: &'static str = "playback_quality";
    const SCOPE: Scope = Scope::Install;
    const ACCOUNT_BOUND: bool = false;
    const LIVE: bool = true;
    const SETTINGS: Option<SettingsWrite> = Some(SettingsWrite::Confirm);
    const PARSE: Parse = Parse::OptionalSoft;
    fn stored(p: &Preferences) -> &Option<PlaybackQuality> { &p.playback_quality }
    fn stored_mut(p: &mut Preferences) -> &mut Option<PlaybackQuality> { &mut p.playback_quality }
    fn get(p: &Preferences) -> PlaybackQuality { p.playback_quality.unwrap_or(PlaybackQuality::Original) }
    fn to_stored(v: PlaybackQuality) -> Option<PlaybackQuality> { Some(v) }
}

setting!(DirectPlayModePref: DirectPlayMode = direct_play_mode, "direct_play_mode", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(NextEpisodeModePref: NextEpisodeMode = next_episode_mode, "next_episode_mode", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SkipIntervalPref: SkipInterval = skip_interval, "skip_interval", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(DeckPressPref: DeckPress = deck_press, "deck_press", Install,
    account false, live true, settings Some(SettingsWrite::Confirm), parse Soft);
setting!(SubtitleSizePref: SubtitleSize = subtitle_size, "subtitle_size", Install,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
setting!(SubtitlePositionPref: SubtitlePosition = subtitle_position, "subtitle_position", Install,
    account false, live true, settings Some(SettingsWrite::Optimistic), parse Soft);
// Tone and enhancements have no Settings row: the player panel is their only writer.
setting!(SubtitleTonePref: SubtitleTone = subtitle_tone, "subtitle_tone", Install,
    account false, live true, settings None, parse Soft);
setting!(AudioEnhancementsPref: crate::plex::AudioEnhancements = audio_enhancements,
    "audio_enhancements", Install, account false, live true, settings None, parse Soft);

/// A setting as data, so a test (and a later generic restore) can walk them all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub key: &'static str,
    pub scope: Scope,
    pub account_bound: bool,
    pub live: bool,
    pub settings: Option<SettingsWrite>,
    pub parse: Parse,
}

impl Descriptor {
    pub const fn of<S: Setting>() -> Self {
        Descriptor {
            key: S::KEY,
            scope: S::SCOPE,
            account_bound: S::ACCOUNT_BOUND,
            live: S::LIVE,
            settings: S::SETTINGS,
            parse: S::PARSE,
        }
    }
}

/// Every [`Preferences`] field, once. `prefs_tests::t_fields_every_field_is_described_by_exactly_one_setting` holds this to the struct's own serde
/// field list both ways.
pub const DESCRIPTORS: &[Descriptor] = &[
    Descriptor::of::<QualityPref>(),
    Descriptor::of::<DirectPlayModePref>(),
    Descriptor::of::<SubtitleTonePref>(),
    Descriptor::of::<SubtitleSizePref>(),
    Descriptor::of::<SubtitlePositionPref>(),
    Descriptor::of::<NextEpisodeModePref>(),
    Descriptor::of::<SkipIntervalPref>(),
    Descriptor::of::<DeckPressPref>(),
    Descriptor::of::<AudioEnhancementsPref>(),
];

impl Session {
    /// This session with `S` set to `value`, every other field intact.
    pub fn with_pref<S: Setting>(&self, value: S::Value) -> Self {
        let mut next = self.clone();
        S::put(&mut next.prefs, value);
        next
    }
}

/// Future or malformed direct-play policies retain the automatic compatibility checks.
fn de_soft_direct_play_mode<'de, D>(d: D) -> Result<DirectPlayMode, D::Error>
where D: Deserializer<'de> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// Playback quality is a preference, not a credential gate. A value written by a newer build or
/// damaged by a hand edit therefore degrades to the legacy-safe Original mode rather than making
/// the enclosing [`Session`] disappear.
fn de_soft_playback_quality<'de, D>(d: D) -> Result<Option<PlaybackQuality>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<PlaybackQuality>>(v).unwrap_or(None))
}

/// The subtitle tone is a preference too: a spelling this build does not know degrades to white.
fn de_soft_subtitle_tone<'de, D>(d: D) -> Result<SubtitleTone, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitleTone::White);
    };
    Ok(serde_json::from_value::<SubtitleTone>(v).unwrap_or_default())
}

/// The subtitle size is a preference too: a spelling this build does not know degrades to Medium.
fn de_soft_subtitle_size<'de, D>(d: D) -> Result<SubtitleSize, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitleSize::Medium);
    };
    Ok(serde_json::from_value::<SubtitleSize>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_subtitle_size(size: &SubtitleSize) -> bool {
    *size == SubtitleSize::default()
}

/// The subtitle position is a preference too: a spelling this build does not know degrades to Low.
fn de_soft_subtitle_position<'de, D>(d: D) -> Result<SubtitlePosition, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SubtitlePosition::Low);
    };
    Ok(serde_json::from_value::<SubtitlePosition>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_subtitle_position(position: &SubtitlePosition) -> bool {
    *position == SubtitlePosition::default()
}

/// The next-episode mode is a preference too: a spelling this build does not know degrades to Countdown.
fn de_soft_next_episode_mode<'de, D>(d: D) -> Result<NextEpisodeMode, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(NextEpisodeMode::Countdown);
    };
    Ok(serde_json::from_value::<NextEpisodeMode>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_next_episode_mode(mode: &NextEpisodeMode) -> bool {
    *mode == NextEpisodeMode::default()
}

/// The skip interval is a preference too: a spelling this build does not know degrades to 10 s.
fn de_soft_skip_interval<'de, D>(d: D) -> Result<SkipInterval, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(SkipInterval::default());
    };
    Ok(serde_json::from_value::<SkipInterval>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_skip_interval(interval: &SkipInterval) -> bool {
    *interval == SkipInterval::default()
}

/// The deck-press mode is a preference too: a spelling this build does not know degrades to Details.
fn de_soft_deck_press<'de, D>(d: D) -> Result<DeckPress, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(DeckPress::default());
    };
    Ok(serde_json::from_value::<DeckPress>(v).unwrap_or_default())
}

/// `skip_serializing_if` needs a function, not just `PartialEq` with `Default::default()`.
fn is_default_deck_press(mode: &DeckPress) -> bool {
    *mode == DeckPress::default()
}

/// The audio-enhancement toggle is a preference too: an unknown shape degrades to both flags
/// off rather than failing the enclosing [`Session`].
fn de_soft_audio_enhancements<'de, D>(d: D) -> Result<crate::plex::AudioEnhancements, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(crate::plex::AudioEnhancements::NONE);
    };
    Ok(serde_json::from_value::<crate::plex::AudioEnhancements>(v).unwrap_or(crate::plex::AudioEnhancements::NONE))
}

