//! **Frozen copies of what `main` (commit cab5d041) parses, as plain serde structs.**
//!
//! Test-only. This is the old build, as far as the persisted record is concerned: the file a
//! rolled-back or older install would read, and the preference parse it would run over a DB8
//! record that a newer build wrote. Tier 1 of the track-memory plan feeds new-build bytes through
//! these types and asserts today's keys still parse as before. They must NOT track the live
//! `Session`, `CanonicalSessionPreferences` or `SubtitleOffsets`: A1 rewrites those, and these
//! outlive the rewrite. Do not "keep them in sync"; a change here means history was rewritten.
//!
//! What is frozen: the three containers named above and every LIST element type they hold
//! (`LastLibrary`/`TypedLib`, `LibraryViews`/`LibraryView`, `SubtitleOffsets`/`SubtitleOffsetEntry`,
//! `PlaintextConsent`, `ServerKeyPin`, `HomePins`/`PinnedLib`, `RecentSearches`), with the
//! `de_soft_*` parse policies they use, copied attribute for attribute.
//!
//! What is NOT frozen, and is shared with the live module: the credential records (`ServerRef`,
//! `UserRef`, `HomeUserRef`, `SourceRef`, `ProfileCreds`) and the scalar enums
//! (`PlaybackQuality`, `DirectPlayMode`, `SubtitleTone`, `SubtitleSize`, `SubtitlePosition`,
//! `NextEpisodeMode`, `SkipInterval`, `DeckPress`, `AudioEnhancements`, `PlaintextChoice`). The
//! plan changes none of them (their spellings are on-disk contracts the A0 JSON rows pin), and
//! copying the credential records would copy a security surface for no tier-1 gain. The cached
//! `profiles` list of the legacy file uses a simpler per-entry soft parse than `main`'s
//! `parse_profiles` (which also drops duplicate and mismatched uuids); no tier-1 case depends on
//! that difference.
#![allow(dead_code)]

use crate::plex::AudioEnhancements;
use super::{
    DeckPress, DirectPlayMode, HomeUserRef, NextEpisodeMode, PlaintextChoice,
    PlaybackQuality, ProfileCreds, ServerRef, SkipInterval, SourceRef, SubtitlePosition,
    SubtitleSize, SubtitleTone, UserRef,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Opaque(pub BTreeMap<String, Value>);

impl Opaque {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Opaque {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

// ---- the list element types, frozen -------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct PinnedLib {
    pub machine_id: String,
    pub key: i64,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct HomePins {
    pub user: String,
    pub asked: bool,
    pub on: Vec<PinnedLib>,
    pub off: Vec<PinnedLib>,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RecentSearches {
    pub user: String,
    pub terms: Vec<String>,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlaintextConsent {
    pub machine_id: String,
    #[serde(default)]
    pub account: String,
    pub choice: PlaintextChoice,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ServerKeyPin {
    pub machine_id: String,
    pub pin: String,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct TypedLib {
    pub kind: String,
    pub machine_id: String,
    pub key: i64,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LastLibrary {
    pub user: String,
    pub libs: Vec<TypedLib>,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LibraryView {
    pub machine_id: String,
    pub key: i64,
    pub sort: String,
    pub desc: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unwatched: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub genre: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub listing: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub listing_sort: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub listing_desc: bool,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LibraryViews {
    pub user: String,
    #[serde(deserialize_with = "de_soft_vec")]
    pub libs: Vec<LibraryView>,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SubtitleOffsetEntry {
    pub machine_id: String,
    pub rating_key: String,
    pub offset_ms: i64,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SubtitleOffsets {
    pub user: String,
    #[serde(deserialize_with = "de_soft_vec")]
    pub items: Vec<SubtitleOffsetEntry>,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

// ---- the DB8 public preferences, frozen ---------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct CanonicalSessionPreferences {
    #[serde(default)]
    pub language: plx_platform::i18n::Preference,
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub playback_quality: Option<PlaybackQuality>,
    #[serde(default, deserialize_with = "de_soft_direct_play_mode")]
    pub direct_play_mode: DirectPlayMode,
    #[serde(default, deserialize_with = "de_soft_bool")]
    pub auto_sign_in: bool,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub last_library: Vec<LastLibrary>,
    #[serde(rename = "library_sorts", default, deserialize_with = "de_soft_vec",
        skip_serializing_if = "Vec::is_empty")]
    pub library_views: Vec<LibraryViews>,
    #[serde(default, deserialize_with = "de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub subtitle_offsets: Vec<SubtitleOffsets>,
    #[serde(default, deserialize_with = "de_soft_hero_blur")]
    pub last_hero_blur: Option<[[f32; 3]; 4]>,
    #[serde(default = "default_true", deserialize_with = "de_soft_bool_on")]
    pub trailer_autoplay: bool,
    #[serde(default, deserialize_with = "de_soft_subtitle_tone")]
    pub subtitle_tone: SubtitleTone,
    #[serde(default, deserialize_with = "de_soft_subtitle_size", skip_serializing_if = "is_default_subtitle_size")]
    pub subtitle_size: SubtitleSize,
    #[serde(default, deserialize_with = "de_soft_subtitle_position", skip_serializing_if = "is_default_subtitle_position")]
    pub subtitle_position: SubtitlePosition,
    #[serde(default, deserialize_with = "de_soft_next_episode_mode", skip_serializing_if = "is_default_next_episode_mode")]
    pub next_episode_mode: NextEpisodeMode,
    #[serde(default, deserialize_with = "de_soft_skip_interval", skip_serializing_if = "is_default_skip_interval")]
    pub skip_interval: SkipInterval,
    #[serde(default, deserialize_with = "de_soft_deck_press", skip_serializing_if = "is_default_deck_press")]
    pub deck_press: DeckPress,
    #[serde(default, deserialize_with = "de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub plaintext_consent: Vec<PlaintextConsent>,
    #[serde(default, deserialize_with = "de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub server_key_pins: Vec<ServerKeyPin>,
    #[serde(default, deserialize_with = "de_soft_audio_enhancements", skip_serializing_if = "AudioEnhancements::is_none")]
    pub audio_enhancements: AudioEnhancements,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

impl Default for CanonicalSessionPreferences {
    fn default() -> Self {
        Self {
            language: plx_platform::i18n::Preference::System,
            playback_quality: None,
            direct_play_mode: DirectPlayMode::Auto,
            auto_sign_in: false,
            last_library: Vec::new(),
            library_views: Vec::new(),
            subtitle_offsets: Vec::new(),
            last_hero_blur: None,
            trailer_autoplay: true,
            subtitle_tone: SubtitleTone::White,
            subtitle_size: SubtitleSize::Medium,
            subtitle_position: SubtitlePosition::Low,
            next_episode_mode: NextEpisodeMode::Countdown,
            skip_interval: SkipInterval::Seconds10,
            deck_press: DeckPress::Details,
            plaintext_consent: Vec::new(),
            server_key_pins: Vec::new(),
            audio_enhancements: AudioEnhancements::NONE,
            extensions: BTreeMap::new(),
        }
    }
}

impl CanonicalSessionPreferences {
    /// What `join_canonical` and `public_session` did on `main`: a preferences value that is not
    /// an object, or fails to parse at all, is the whole-value default.
    pub fn parse(preferences: &Value) -> Self {
        serde_json::from_value::<Self>(preferences.clone()).unwrap_or_default()
    }
}

// ---- the legacy file, frozen --------------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    #[serde(default, skip_serializing_if = "plx_platform::i18n::Preference::is_system")]
    pub language: plx_platform::i18n::Preference,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub account_token: String,
    #[serde(default)]
    pub server: ServerRef,
    #[serde(default)]
    pub user: UserRef,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_users: Vec<HomeUserRef>,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub sources: Vec<SourceRef>,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_pins: Vec<HomePins>,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub recent_searches: Vec<RecentSearches>,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub last_library: Vec<LastLibrary>,
    #[serde(rename = "library_sorts", default, deserialize_with = "de_soft_vec",
        skip_serializing_if = "Vec::is_empty")]
    pub library_views: Vec<LibraryViews>,
    #[serde(default, deserialize_with = "de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub subtitle_offsets: Vec<SubtitleOffsets>,
    #[serde(default, deserialize_with = "de_profile_cache")]
    pub profiles: Vec<ProfileCreds>,
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub playback_quality: Option<PlaybackQuality>,
    #[serde(default, deserialize_with = "de_soft_direct_play_mode")]
    pub direct_play_mode: DirectPlayMode,
    #[serde(default, deserialize_with = "de_soft_bool")]
    pub auto_sign_in: bool,
    #[serde(default = "default_true", deserialize_with = "de_soft_bool_on")]
    pub trailer_autoplay: bool,
    #[serde(default, deserialize_with = "de_soft_subtitle_tone")]
    pub subtitle_tone: SubtitleTone,
    #[serde(default, deserialize_with = "de_soft_subtitle_size", skip_serializing_if = "is_default_subtitle_size")]
    pub subtitle_size: SubtitleSize,
    #[serde(default, deserialize_with = "de_soft_subtitle_position", skip_serializing_if = "is_default_subtitle_position")]
    pub subtitle_position: SubtitlePosition,
    #[serde(default, deserialize_with = "de_soft_next_episode_mode", skip_serializing_if = "is_default_next_episode_mode")]
    pub next_episode_mode: NextEpisodeMode,
    #[serde(default, deserialize_with = "de_soft_skip_interval", skip_serializing_if = "is_default_skip_interval")]
    pub skip_interval: SkipInterval,
    #[serde(default, deserialize_with = "de_soft_deck_press", skip_serializing_if = "is_default_deck_press")]
    pub deck_press: DeckPress,
    #[serde(default, deserialize_with = "de_soft_hero_blur")]
    pub last_hero_blur: Option<[[f32; 3]; 4]>,
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub plaintext_consent: Vec<PlaintextConsent>,
    #[serde(default, deserialize_with = "de_soft_vec", skip_serializing_if = "Vec::is_empty")]
    pub server_key_pins: Vec<ServerKeyPin>,
    #[serde(default, deserialize_with = "de_soft_audio_enhancements", skip_serializing_if = "AudioEnhancements::is_none")]
    pub audio_enhancements: AudioEnhancements,
    #[serde(flatten, default, skip_serializing_if = "Opaque::is_empty")]
    pub extensions: Opaque,
}

// ---- the parse policies, frozen -----------------------------------------------------------

fn de_soft_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let Ok(v) = Value::deserialize(d) else { return Ok(Vec::new()) };
    Ok(match v {
        Value::Array(items) => items.into_iter().filter_map(|it| serde_json::from_value::<T>(it).ok()).collect(),
        _ => Vec::new(),
    })
}

fn de_profile_cache<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ProfileCreds>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(match value {
        Value::Array(entries) => entries.into_iter().filter_map(|e| serde_json::from_value(e).ok()).collect(),
        _ => Vec::new(),
    })
}

fn de_soft_hero_blur<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[[f32; 3]; 4]>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

fn de_soft_direct_play_mode<'de, D: Deserializer<'de>>(d: D) -> Result<DirectPlayMode, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

fn de_soft_playback_quality<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PlaybackQuality>, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(None) };
    Ok(serde_json::from_value::<Option<PlaybackQuality>>(v).unwrap_or(None))
}

macro_rules! soft_enum {
    ($de:ident, $is_default:ident, $ty:ty) => {
        fn $de<'de, D: Deserializer<'de>>(d: D) -> Result<$ty, D::Error> {
            let Ok(v) = Value::deserialize(d) else { return Ok(<$ty>::default()) };
            Ok(serde_json::from_value::<$ty>(v).unwrap_or_default())
        }
        fn $is_default(value: &$ty) -> bool {
            *value == <$ty>::default()
        }
    };
}
soft_enum!(de_soft_subtitle_tone, is_default_subtitle_tone, SubtitleTone);
soft_enum!(de_soft_subtitle_size, is_default_subtitle_size, SubtitleSize);
soft_enum!(de_soft_subtitle_position, is_default_subtitle_position, SubtitlePosition);
soft_enum!(de_soft_next_episode_mode, is_default_next_episode_mode, NextEpisodeMode);
soft_enum!(de_soft_skip_interval, is_default_skip_interval, SkipInterval);
soft_enum!(de_soft_deck_press, is_default_deck_press, DeckPress);

fn de_soft_audio_enhancements<'de, D: Deserializer<'de>>(d: D) -> Result<AudioEnhancements, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(AudioEnhancements::NONE) };
    Ok(serde_json::from_value::<AudioEnhancements>(v).unwrap_or(AudioEnhancements::NONE))
}

fn de_soft_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(false) };
    Ok(v.as_bool().unwrap_or(false))
}

fn default_true() -> bool {
    true
}

fn de_soft_bool_on<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(true) };
    Ok(v.as_bool().unwrap_or(true))
}
