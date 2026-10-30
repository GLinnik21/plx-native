//! Serde types for the Jellyfin responses the app reads.
//!
//! Only the fields the app uses are modelled; serde skips the rest. Jellyfin sends PascalCase keys
//! and omits nulls inconsistently across versions, so every field defaults. Field names follow the
//! server's OpenAPI document (10.10 / 10.11).

use serde::{Deserialize, Deserializer, Serialize};

/// A `null` reads as the type's default. Jellyfin sends `null` for empty arrays, strings and counts
/// on some versions and omits them on others; `#[serde(default)]` covers only the omission.
fn nul<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// `QueryResult<T>`: a page of items with the total the query matched.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase", default, bound(deserialize = "T: Deserialize<'de>"))]
pub struct QueryResult<T> {
    #[serde(deserialize_with = "nul")]
    pub items: Vec<T>,
    #[serde(deserialize_with = "nul")]
    pub total_record_count: i64,
    #[serde(deserialize_with = "nul")]
    pub start_index: i64,
}

// By hand: the derive would demand `T: Default`, which no item type needs to be.
impl<T> Default for QueryResult<T> {
    fn default() -> Self {
        Self { items: Vec::new(), total_record_count: 0, start_index: 0 }
    }
}

/// A named entry of a list such as `Studios[]`.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct NameId {
    #[serde(deserialize_with = "nul")]
    pub name: String,
}

/// The per-user state on an item.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserItemData {
    #[serde(deserialize_with = "nul")]
    pub playback_position_ticks: i64,
    #[serde(deserialize_with = "nul")]
    pub play_count: i64,
    #[serde(deserialize_with = "nul")]
    pub played: bool,
    pub last_played_date: Option<String>,
    pub unplayed_item_count: Option<i64>,
    #[serde(deserialize_with = "nul")]
    pub is_favorite: bool,
}

/// One cast or crew credit.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItemPerson {
    #[serde(deserialize_with = "nul")]
    pub name: String,
    #[serde(deserialize_with = "nul")]
    pub id: String,
    #[serde(deserialize_with = "nul")]
    pub role: String,
    /// `Actor`, `Director`, `Writer`, `Producer`, `GuestStar`, ...
    #[serde(rename = "Type", deserialize_with = "nul")]
    pub kind: String,
    pub primary_image_tag: Option<String>,
}

/// A chapter mark.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct ChapterInfo {
    #[serde(deserialize_with = "nul")]
    pub start_position_ticks: i64,
    #[serde(deserialize_with = "nul")]
    pub name: String,
}

/// One elementary stream of a media source.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaStream {
    /// `Video`, `Audio`, `Subtitle`, `EmbeddedImage`, `Data`, `Lyric`.
    #[serde(rename = "Type", deserialize_with = "nul")]
    pub kind: String,
    #[serde(deserialize_with = "nul")]
    pub index: i64,
    #[serde(deserialize_with = "nul")]
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub display_title: Option<String>,
    #[serde(deserialize_with = "nul")]
    pub is_default: bool,
    #[serde(deserialize_with = "nul")]
    pub is_forced: bool,
    #[serde(deserialize_with = "nul")]
    pub is_hearing_impaired: bool,
    #[serde(deserialize_with = "nul")]
    pub is_external: bool,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channel_layout: Option<String>,
    pub bit_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub average_frame_rate: Option<f64>,
    pub real_frame_rate: Option<f64>,
    pub profile: Option<String>,
    /// `SDR`, `HDR10`, `HDR10Plus`, `HLG`, `DOVI`, `DOVIWithHDR10`, `DOVIWithHLG`, `DOVIWithSDR`, ...
    pub video_range_type: Option<String>,
    pub color_transfer: Option<String>,
    pub dv_version_major: Option<i64>,
    pub dv_version_minor: Option<i64>,
    pub dv_profile: Option<i64>,
    pub dv_level: Option<i64>,
    pub rpu_present_flag: Option<i64>,
    pub el_present_flag: Option<i64>,
    pub bl_present_flag: Option<i64>,
    pub dv_bl_signal_compatibility_id: Option<i64>,
}

/// One playable version of an item (Plex's `Media` + `Part` in one).
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSourceInfo {
    #[serde(deserialize_with = "nul")]
    pub id: String,
    /// The version's name (`"2160p HDR"`, else the file's), shown in the version list.
    pub name: Option<String>,
    pub path: Option<String>,
    pub container: Option<String>,
    pub size: Option<i64>,
    /// Bits per second.
    pub bitrate: Option<i64>,
    #[serde(deserialize_with = "nul")]
    pub media_streams: Vec<MediaStream>,
    pub default_audio_stream_index: Option<i64>,
    pub default_subtitle_stream_index: Option<i64>,
}

/// The item DTO every listing and detail read returns.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct BaseItemDto {
    #[serde(deserialize_with = "nul")]
    pub id: String,
    #[serde(deserialize_with = "nul")]
    pub name: String,
    /// `Movie`, `Series`, `Season`, `Episode`, `BoxSet`, `CollectionFolder`, `UserView`, `Trailer`,
    /// `Video`, `Person`, `Folder`, ...
    #[serde(rename = "Type", deserialize_with = "nul")]
    pub kind: String,
    /// On an extra: `Trailer`, `BehindTheScenes`, `Scene`, `Sample`, `Clip`, ...
    pub extra_type: Option<String>,
    /// On a library view: `movies`, `tvshows`, `music`, `boxsets`, `homevideos`, ...
    pub collection_type: Option<String>,
    pub production_year: Option<i64>,
    pub official_rating: Option<String>,
    pub overview: Option<String>,
    #[serde(deserialize_with = "nul")]
    pub taglines: Vec<String>,
    #[serde(deserialize_with = "nul")]
    pub studios: Vec<NameId>,
    #[serde(deserialize_with = "nul")]
    pub genres: Vec<String>,
    #[serde(deserialize_with = "nul")]
    pub production_locations: Vec<String>,
    pub premiere_date: Option<String>,
    /// A person's date of death (a series' last air date).
    pub end_date: Option<String>,
    pub date_created: Option<String>,
    pub run_time_ticks: Option<i64>,
    pub community_rating: Option<f64>,
    pub critic_rating: Option<f64>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub child_count: Option<i64>,
    pub recursive_item_count: Option<i64>,
    pub parent_id: Option<String>,
    pub series_id: Option<String>,
    pub series_name: Option<String>,
    pub season_id: Option<String>,
    pub season_name: Option<String>,
    pub series_primary_image_tag: Option<String>,
    /// `Primary`, `Thumb`, `Logo`, `Banner`, ... to the image's cache tag.
    #[serde(deserialize_with = "nul")]
    pub image_tags: std::collections::HashMap<String, String>,
    #[serde(deserialize_with = "nul")]
    pub backdrop_image_tags: Vec<String>,
    pub parent_backdrop_item_id: Option<String>,
    /// The item whose logo this one inherits (an episode or season shows its series'), and its tag.
    pub parent_logo_item_id: Option<String>,
    pub parent_logo_image_tag: Option<String>,
    #[serde(deserialize_with = "nul")]
    pub parent_backdrop_image_tags: Vec<String>,
    /// Image kind (`Backdrop`, `Primary`, ...) to image tag to that image's BlurHash. The server
    /// sends it with the images themselves (it is not one of `Fields`), inherited tags included.
    #[serde(deserialize_with = "nul")]
    pub image_blur_hashes: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    pub user_data: Option<UserItemData>,
    #[serde(deserialize_with = "nul")]
    pub people: Vec<BaseItemPerson>,
    #[serde(deserialize_with = "nul")]
    pub media_sources: Vec<MediaSourceInfo>,
    #[serde(deserialize_with = "nul")]
    pub chapters: Vec<ChapterInfo>,
    #[serde(deserialize_with = "nul")]
    pub provider_ids: std::collections::HashMap<String, String>,
    /// Scrubbing thumbnails (10.9+): media source id, then picture width (as a string key), to
    /// how the pictures of that width are laid out. Only asked for by `paths::item_trickplay`.
    #[serde(deserialize_with = "nul")]
    pub trickplay: std::collections::HashMap<String, std::collections::HashMap<String, TrickplayInfoDto>>,
}

/// One width of an item's scrubbing thumbnails: `thumbnail_count` pictures of `width`×`height`,
/// `interval` ms apart, on sheets of `tile_width`×`tile_height` pictures.
#[derive(Deserialize, Default, Debug, Clone, Copy)]
#[serde(rename_all = "PascalCase", default)]
pub struct TrickplayInfoDto {
    pub width: i64,
    pub height: i64,
    pub tile_width: i64,
    pub tile_height: i64,
    pub thumbnail_count: i64,
    pub interval: i64,
}

/// One entry of `/MediaSegments/{itemId}` (Jellyfin 10.10+).
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSegmentDto {
    /// `Intro`, `Outro`, `Recap`, `Preview`, `Commercial`, `Unknown`.
    #[serde(rename = "Type", deserialize_with = "nul")]
    pub kind: String,
    #[serde(deserialize_with = "nul")]
    pub start_ticks: i64,
    #[serde(deserialize_with = "nul")]
    pub end_ticks: i64,
}

/// `GET /System/Info/Public`: what an unauthenticated client may learn about a server.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct PublicSystemInfo {
    #[serde(deserialize_with = "nul")]
    pub server_name: String,
    #[serde(deserialize_with = "nul")]
    pub id: String,
}

/// The signed-in user.
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserDto {
    #[serde(deserialize_with = "nul")]
    pub id: String,
    #[serde(deserialize_with = "nul")]
    pub name: String,
    pub server_id: Option<String>,
    /// Set when the user has an avatar.
    pub primary_image_tag: Option<String>,
    pub policy: Option<UserPolicy>,
}

/// The keys of a user's `Policy` this client reads.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserPolicy {
    /// "Allow video playback that requires transcoding". Without it the server copies the picture
    /// into its stream whatever its decision said (`EncodingHelper::TryStreamCopy`).
    pub enable_video_playback_transcoding: Option<bool>,
    /// "Allow audio playback that requires transcoding": the same rule for the sound.
    pub enable_audio_playback_transcoding: Option<bool>,
}

/// The audio and subtitle keys of a user's `Configuration` (`GET /Users/{userId}`).
#[derive(Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserLanguageConfig {
    /// ISO 639-2 (`por`), or empty for "any language".
    #[serde(deserialize_with = "nul")]
    pub audio_language_preference: String,
    /// "Play default audio track regardless of language". Absent reads as the server's default, `true`.
    pub play_default_audio_track: Option<bool>,
    #[serde(deserialize_with = "nul")]
    pub subtitle_language_preference: String,
    /// `Default`, `Always`, `OnlyForced`, `None` or `Smart`.
    #[serde(deserialize_with = "nul")]
    pub subtitle_mode: String,
}

/// The keys of a user's `Configuration` that shape Home (`GET /Users/{userId}`).
#[derive(Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserHomeConfig {
    /// "Hide watched content from latest media". Absent reads as the server's default, `true`.
    pub hide_played_in_latest: Option<bool>,
    /// Library ids with no Latest shelf.
    pub latest_items_excludes: Vec<String>,
    /// Library ids in the order the user arranged them; a library not listed follows, in the server's order.
    pub ordered_views: Vec<String>,
    /// Library ids the user hid from My Media.
    pub my_media_excludes: Vec<String>,
}

/// The keys of [`UserLanguageConfig`] a save changes; `None` leaves the server's value.
#[derive(Serialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub struct UserLanguageUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_language_preference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_default_audio_track: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_language_preference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle_mode: Option<&'static str>,
}

impl UserLanguageUpdate {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `POST /Users/AuthenticateByName` and `/Users/AuthenticateWithQuickConnect`.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct AuthenticationResult {
    #[serde(deserialize_with = "nul")]
    pub user: UserDto,
    #[serde(deserialize_with = "nul")]
    pub access_token: String,
    #[serde(deserialize_with = "nul")]
    pub server_id: String,
}

/// `POST /QuickConnect/Initiate` and `GET /QuickConnect/Connect?secret=`.
#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct QuickConnectResult {
    #[serde(deserialize_with = "nul")]
    pub authenticated: bool,
    #[serde(deserialize_with = "nul")]
    pub secret: String,
    /// The six-digit code the user types into another signed-in Jellyfin client.
    #[serde(deserialize_with = "nul")]
    pub code: String,
    pub device_id: Option<String>,
}


/// One candidate of `GET /Items/{id}/RemoteSearch/Subtitles/{language}`, offered by a subtitle
/// provider plugin installed on the server (OpenSubtitles, …).
#[derive(Deserialize, Default, Debug, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct RemoteSubtitleInfo {
    /// The handle the download is addressed by, e.g. `opensubtitles_12345`.
    #[serde(deserialize_with = "nul")]
    pub id: String,
    #[serde(deserialize_with = "nul")]
    pub provider_name: String,
    /// The release name.
    #[serde(deserialize_with = "nul")]
    pub name: String,
    #[serde(deserialize_with = "nul")]
    pub format: String,
    #[serde(rename = "ThreeLetterISOLanguageName", deserialize_with = "nul")]
    pub three_letter_iso_language_name: String,
    #[serde(deserialize_with = "nul")]
    pub comment: String,
    #[serde(deserialize_with = "nul")]
    pub download_count: i64,
    /// The provider matched the file itself (its hash), not only the title.
    #[serde(deserialize_with = "nul")]
    pub is_hash_match: bool,
    #[serde(deserialize_with = "nul")]
    pub hearing_impaired: bool,
    #[serde(deserialize_with = "nul")]
    pub forced: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_episode_listing_parses() {
        let json = r#"{"Items":[{"Name":"Pilot","Id":"a1","Type":"Episode","SeriesName":"Show",
            "SeriesId":"s1","SeasonId":"se1","IndexNumber":1,"ParentIndexNumber":1,
            "RunTimeTicks":27000000000,"UserData":{"PlaybackPositionTicks":6000000000,
            "PlayCount":0,"Played":false},"ImageTags":{"Primary":"t1"},"SomethingNew":{"x":1}}],
            "TotalRecordCount":42,"StartIndex":0}"#;
        let q: QueryResult<BaseItemDto> = serde_json::from_str(json).unwrap();
        assert_eq!(q.total_record_count, 42);
        let e = &q.items[0];
        assert_eq!((e.kind.as_str(), e.index_number, e.parent_index_number), ("Episode", Some(1), Some(1)));
        assert_eq!(e.user_data.as_ref().unwrap().playback_position_ticks, 6_000_000_000);
        assert_eq!(e.image_tags.get("Primary").map(String::as_str), Some("t1"));
    }

    #[test]
    fn nulls_read_as_absent() {
        let json = r#"{"Id":"x","Name":"n","Type":"Movie","Overview":null,"UserData":null,
            "ProductionYear":null}"#;
        let m: BaseItemDto = serde_json::from_str(json).unwrap();
        assert!(m.overview.is_none() && m.user_data.is_none() && m.production_year.is_none());
    }

    #[test]
    fn quick_connect_parses() {
        let q: QuickConnectResult = serde_json::from_str(
            r#"{"Authenticated":false,"Secret":"abc","Code":"123456","DeviceId":"d","DateAdded":"2026-10-13T22:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!((q.code.as_str(), q.authenticated), ("123456", false));
    }
}
