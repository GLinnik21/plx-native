//! One function per Jellyfin endpoint the app calls, building its path and query string.
//!
//! Every Jellyfin query is built here, the way every PMS query is built in `plx_plex::plex`; the
//! one exception is the direct-play part key, which `translate::part_key` writes into the model.
//! Paths target Jellyfin 10.10+ (`/UserItems/...`, `/UserViews`, `/MediaSegments`); 10.9 and older still
//! serve most of them under `/Users/{userId}/...`, which this client does not use.
//!
//! The functions return a path relative to the server's base URL (which may itself carry a path,
//! such as `https://host/jellyfin`). Request bodies for the POST endpoints are built by the
//! `*_body` functions as JSON strings.

use plx_plex::plex::urlenc_str as enc;

/// The item fields a listing (a shelf, a grid page) asks for: enough to draw a card and its
/// progress, nothing that makes the server load streams or people for every row.
pub const LIST_FIELDS: &str = "PrimaryImageAspectRatio,DateCreated,ChildCount,RecursiveItemCount,Overview,Genres";

/// The item fields a detail page asks for.
pub const DETAIL_FIELDS: &str = "Overview,Genres,People,Studios,Taglines,ProductionLocations,\
MediaSources,MediaStreams,Chapters,ProviderIds,DateCreated,ChildCount,RecursiveItemCount,\
PrimaryImageAspectRatio,Trickplay";

/// The image kinds the app draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Primary,
    Backdrop,
    Logo,
}

impl ImageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageKind::Primary => "Primary",
            ImageKind::Backdrop => "Backdrop",
            ImageKind::Logo => "Logo",
        }
    }
}

/// Sort orders the library grid offers, in the server's spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    /// By the title shown. Jellyfin's `SortName` is not used: the server computes it once, from
    /// the name an item had when it was first scanned (often the folder's original-language
    /// title), and keeps it after the metadata gives the item a translated name, so a library
    /// sorted by it reads out of order.
    Title,
    DateCreated,
    PremiereDate,
    CommunityRating,
    Runtime,
    DatePlayed,
    Random,
}

impl SortBy {
    pub fn as_str(self) -> &'static str {
        match self {
            SortBy::Title => "Name",
            SortBy::DateCreated => "DateCreated,Name",
            SortBy::PremiereDate => "PremiereDate,Name",
            SortBy::CommunityRating => "CommunityRating,Name",
            SortBy::Runtime => "Runtime,Name",
            SortBy::DatePlayed => "DatePlayed,Name",
            SortBy::Random => "Random",
        }
    }
}

/// A library or search listing.
#[derive(Clone, Debug, Default)]
pub struct ItemsQuery<'a> {
    pub parent_id: Option<&'a str>,
    /// `Movie`, `Series`, `Episode`, `BoxSet`, ... comma-separated.
    pub include_item_types: Option<&'a str>,
    pub recursive: bool,
    pub sort_by: Option<SortBy>,
    pub descending: bool,
    pub start_index: i64,
    pub limit: i64,
    /// Ask for the total only: `limit=0` (a zero `limit` field otherwise means no limit), so the
    /// answer carries `TotalRecordCount` and no rows.
    pub count_only: bool,
    /// `IsUnplayed`, `IsPlayed`, `IsResumable`, `IsFavorite`, comma-separated.
    pub filters: Option<&'a str>,
    /// The A–Z rail: items whose sort name starts with this.
    pub name_starts_with: Option<&'a str>,
    pub search_term: Option<&'a str>,
    pub ids: Option<&'a str>,
    /// Items this person appears in (cast or crew).
    pub person_ids: Option<&'a str>,
    /// Items in this genre (a `/Genres` row's id).
    pub genre_ids: Option<&'a str>,
    pub fields: Option<&'a str>,
}

pub fn public_info() -> String {
    "/System/Info/Public".into()
}

/// `POST`, body [`authenticate_by_name_body`].
pub fn authenticate_by_name() -> String {
    "/Users/AuthenticateByName".into()
}

pub fn authenticate_by_name_body(username: &str, password: &str) -> String {
    serde_json::json!({ "Username": username, "Pw": password }).to_string()
}

/// The signed-in user (the token's owner).
pub fn users_me() -> String {
    "/Users/Me".into()
}

/// One user, with their `Configuration` (the audio and subtitle preferences among it).
pub fn user(user_id: &str) -> String {
    format!("/Users/{}", enc(user_id))
}

/// `POST` a user's whole `Configuration`: the server replaces it, so every key goes back. 10.10
/// keeps `/Users/{userId}/Configuration` only as an obsolete alias of this.
pub fn user_configuration(user_id: &str) -> String {
    format!("/Users/Configuration?userId={}", enc(user_id))
}

/// `POST`: start a Quick Connect request; the answer carries the code to show and the secret to poll with.
pub fn quick_connect_initiate() -> String {
    "/QuickConnect/Initiate".into()
}

/// Poll a Quick Connect request until `Authenticated` is true.
pub fn quick_connect_state(secret: &str) -> String {
    format!("/QuickConnect/Connect?secret={}", enc(secret))
}

/// `POST`, body [`quick_connect_authenticate_body`]: trade an approved secret for a token.
pub fn quick_connect_authenticate() -> String {
    "/Users/AuthenticateWithQuickConnect".into()
}

pub fn quick_connect_authenticate_body(secret: &str) -> String {
    serde_json::json!({ "Secret": secret }).to_string()
}

/// The user's libraries, in the order they arranged them.
pub fn user_views(user_id: &str) -> String {
    format!("/UserViews?userId={}", enc(user_id))
}

/// Continue Watching: partly watched movies and episodes, most recent first; `parent_id` narrows
/// it to one library. The item types are spelled out because newer servers roll progress up to
/// series and season folders and list those as resumable too.
pub fn resume(user_id: &str, parent_id: Option<&str>, start: i64, limit: i64) -> String {
    let mut p = format!(
        "/UserItems/Resume?userId={}&mediaTypes=Video&includeItemTypes=Movie,Episode,Video&startIndex={}&limit={}&fields={}&enableTotalRecordCount=true&imageTypeLimit=1&enableImageTypes=Primary,Backdrop,Thumb",
        enc(user_id),
        start.max(0),
        limit.max(1),
        LIST_FIELDS
    );
    if let Some(id) = parent_id {
        p.push_str(&format!("&parentId={}", enc(id)));
    }
    p
}

/// `GET`: subtitle candidates for an item in one language (an ISO 639-2 code, `por`).
pub fn remote_subtitles(item_id: &str, language: &str) -> String {
    format!("/Items/{}/RemoteSearch/Subtitles/{}?isPerfectMatch=false", enc(item_id), enc(language))
}

/// `POST`: download one subtitle candidate into the item.
pub fn remote_subtitle(item_id: &str, subtitle_id: &str) -> String {
    format!("/Items/{}/RemoteSearch/Subtitles/{}", enc(item_id), enc(subtitle_id))
}

/// Next Up: the next unwatched episode of each show in progress.
pub fn next_up(user_id: &str, start: i64, limit: i64) -> String {
    format!(
        "/Shows/NextUp?userId={}&startIndex={}&limit={}&fields={}&enableTotalRecordCount=true&disableFirstEpisode=false&enableResumable=false",
        enc(user_id),
        start.max(0),
        limit.max(1),
        LIST_FIELDS
    )
}

/// The next episode of one show: the one after the last watched, or its first.
pub fn series_next_up(user_id: &str, series_id: &str) -> String {
    format!(
        "/Shows/NextUp?userId={}&seriesId={}&limit=1&fields={}&disableFirstEpisode=false&enableResumable=false",
        enc(user_id),
        enc(series_id),
        LIST_FIELDS
    )
}

/// Recently added in one library. Answers a bare JSON array, not a `QueryResult`.
/// `hide_played` leaves out what the user has already watched.
pub fn latest(user_id: &str, parent_id: &str, limit: i64, hide_played: bool) -> String {
    format!(
        "/Items/Latest?userId={}&parentId={}&limit={}&fields={}&groupItems=true{}",
        enc(user_id),
        enc(parent_id),
        limit.max(1),
        LIST_FIELDS,
        if hide_played { "&isPlayed=false" } else { "" }
    )
}

/// A library grid page, a search, or a lookup by ids.
pub fn items(user_id: &str, q: &ItemsQuery<'_>) -> String {
    let mut p = format!("/Items?userId={}", enc(user_id));
    let mut add = |k: &str, v: &str| p.push_str(&format!("&{k}={}", enc(v)));
    if let Some(v) = q.parent_id {
        add("parentId", v);
    }
    if let Some(v) = q.include_item_types {
        add("includeItemTypes", v);
    }
    if q.recursive {
        add("recursive", "true");
    }
    if let Some(s) = q.sort_by {
        add("sortBy", s.as_str());
        add("sortOrder", if q.descending { "Descending" } else { "Ascending" });
    }
    if let Some(v) = q.filters {
        add("filters", v);
    }
    if let Some(v) = q.name_starts_with {
        add("nameStartsWith", v);
    }
    if let Some(v) = q.search_term {
        add("searchTerm", v);
    }
    if let Some(v) = q.ids {
        add("ids", v);
    }
    if let Some(v) = q.person_ids {
        add("personIds", v);
    }
    if let Some(v) = q.genre_ids {
        add("genreIds", v);
    }
    add("fields", q.fields.unwrap_or(LIST_FIELDS));
    p.push_str(&format!("&startIndex={}&enableTotalRecordCount=true", q.start_index.max(0)));
    if q.count_only {
        p.push_str("&limit=0");
    } else if q.limit > 0 {
        p.push_str(&format!("&limit={}", q.limit));
    }
    p
}

/// The genres of one library's `item_types` (`Movie`, `Series`, ...), by name: the Genre filter's
/// value list.
pub fn genres(user_id: &str, parent_id: &str, item_types: &str) -> String {
    format!(
        "/Genres?userId={}&parentId={}&includeItemTypes={}&sortBy=SortName&sortOrder=Ascending",
        enc(user_id),
        enc(parent_id),
        enc(item_types)
    )
}

/// People whose name matches `term`, one page (the People tab of a search).
pub fn persons(user_id: &str, term: &str, start: i64, limit: i64) -> String {
    let mut p = format!(
        "/Persons?userId={}&searchTerm={}&fields=PrimaryImageAspectRatio&startIndex={}&enableTotalRecordCount=true",
        enc(user_id),
        enc(term),
        start.max(0)
    );
    if limit > 0 {
        p.push_str(&format!("&limit={limit}"));
    }
    p
}

/// One item with everything a detail page shows.
pub fn item(user_id: &str, item_id: &str) -> String {
    format!("/Items/{}?userId={}&fields={}", enc(item_id), enc(user_id), DETAIL_FIELDS)
}

/// An item's scrubbing-thumbnail layouts and the versions they belong to.
pub fn item_trickplay(user_id: &str, item_id: &str) -> String {
    format!("/Items/{}?userId={}&fields=Trickplay,MediaSources", enc(item_id), enc(user_id))
}

/// One sheet of an item's scrubbing thumbnails at `width`, of media source `media_source_id` (the
/// server's first when `None`).
pub fn trickplay_sheet(item_id: &str, width: u32, sheet: u32, media_source_id: Option<&str>) -> String {
    let source = media_source_id.map(|id| format!("?mediaSourceId={}", enc(id))).unwrap_or_default();
    format!("/Videos/{}/Trickplay/{width}/{sheet}.jpg{source}", enc(item_id))
}

pub fn seasons(user_id: &str, series_id: &str) -> String {
    format!(
        "/Shows/{}/Seasons?userId={}&fields={}&enableUserData=true",
        enc(series_id),
        enc(user_id),
        LIST_FIELDS
    )
}

pub fn episodes(user_id: &str, series_id: &str, season_id: &str, start: i64, limit: i64) -> String {
    let mut p = format!(
        "/Shows/{}/Episodes?userId={}&seasonId={}&fields={}&enableUserData=true&startIndex={}",
        enc(series_id),
        enc(user_id),
        enc(season_id),
        LIST_FIELDS,
        start.max(0)
    );
    if limit > 0 {
        p.push_str(&format!("&limit={limit}"));
    }
    p
}

/// A show's episodes in order, from `start_item_id` on: a play queue's window.
pub fn episodes_from(user_id: &str, series_id: &str, start_item_id: &str, limit: i64) -> String {
    format!(
        "/Shows/{}/Episodes?userId={}&startItemId={}&fields={}&enableUserData=true&limit={}",
        enc(series_id),
        enc(user_id),
        enc(start_item_id),
        LIST_FIELDS,
        limit.max(1)
    )
}

pub fn similar(user_id: &str, item_id: &str, limit: i64) -> String {
    format!(
        "/Items/{}/Similar?userId={}&limit={}&fields={}",
        enc(item_id),
        enc(user_id),
        limit.max(1),
        LIST_FIELDS
    )
}

/// The item's own trailer files. Answers a bare JSON array; `SpecialFeatures` leaves them out.
pub fn local_trailers(user_id: &str, item_id: &str) -> String {
    format!("/Items/{}/LocalTrailers?userId={}", enc(item_id), enc(user_id))
}

/// Extras other than trailers. Answers a bare JSON array.
pub fn special_features(user_id: &str, item_id: &str) -> String {
    format!("/Items/{}/SpecialFeatures?userId={}", enc(item_id), enc(user_id))
}

/// `POST` makes the item a favourite, `DELETE` takes it off.
pub fn favorite(user_id: &str, item_id: &str) -> String {
    format!("/UserFavoriteItems/{}?userId={}", enc(item_id), enc(user_id))
}

/// [`favorite`]'s older spelling, `/Users/{userId}/FavoriteItems/{itemId}`, for a server without
/// the newer route.
pub fn favorite_legacy(user_id: &str, item_id: &str) -> String {
    format!("/Users/{}/FavoriteItems/{}", enc(user_id), enc(item_id))
}

/// `POST` marks watched, `DELETE` marks unwatched.
pub fn played(user_id: &str, item_id: &str) -> String {
    format!("/UserPlayedItems/{}?userId={}", enc(item_id), enc(user_id))
}

/// The user's data for one item (`POST`: only the fields sent change).
pub fn user_data(user_id: &str, item_id: &str) -> String {
    format!("/UserItems/{}/UserData?userId={}", enc(item_id), enc(user_id))
}

/// This app's display preferences for the user, a per-user store the server keeps for each client.
pub fn display_preferences(user_id: &str) -> String {
    format!("/DisplayPreferences/finnative?userId={}&client=finnative", enc(user_id))
}

/// Intro, credits and commercial ranges (Jellyfin 10.10+, filled by a segment provider plugin).
pub fn media_segments(item_id: &str) -> String {
    format!(
        "/MediaSegments/{}?includeSegmentTypes=Intro&includeSegmentTypes=Outro&includeSegmentTypes=Commercial",
        enc(item_id)
    )
}

/// A transcode the server runs for this player: progressive Matroska, the shape the player already
/// demuxes. The server copies a track whose codec is in its list when copying is allowed, and
/// otherwise encodes it to the list's first codec.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcode<'a> {
    pub item_id: &'a str,
    pub media_source_id: &'a str,
    pub play_session_id: &'a str,
    pub device_id: &'a str,
    /// Comma-separated, the encode target first.
    pub video_codecs: &'a str,
    pub audio_codecs: &'a str,
    pub copy_video: bool,
    pub copy_audio: bool,
    /// The source stream indexes to carry (`None`: the server's default).
    pub audio_index: Option<i64>,
    /// A subtitle to burn into the picture.
    pub burn_subtitle: Option<i64>,
    /// Bits per second, and the frame the picture may not exceed.
    pub max_bitrate: Option<i64>,
    pub max_width: Option<i64>,
    pub max_height: Option<i64>,
    /// Where the stream starts, in ticks (0 = the beginning).
    pub start_ticks: i64,
}

pub fn transcode(t: &Transcode<'_>, token: &str) -> String {
    let mut p = format!(
        "/Videos/{}/stream.mkv?static=false&container=mkv&mediaSourceId={}&playSessionId={}&deviceId={}\
         &videoCodec={}&audioCodec={}&allowVideoStreamCopy={}&allowAudioStreamCopy={}",
        enc(t.item_id),
        enc(t.media_source_id),
        enc(t.play_session_id),
        enc(t.device_id),
        enc(t.video_codecs),
        enc(t.audio_codecs),
        t.copy_video,
        t.copy_audio,
    );
    if let Some(i) = t.audio_index {
        p.push_str(&format!("&audioStreamIndex={i}"));
    }
    if let Some(i) = t.burn_subtitle {
        p.push_str(&format!("&subtitleStreamIndex={i}&subtitleMethod=Encode"));
    }
    if let Some(b) = t.max_bitrate {
        p.push_str(&format!("&maxStreamingBitrate={b}&videoBitRate={b}"));
    }
    if let Some(w) = t.max_width {
        p.push_str(&format!("&maxWidth={w}"));
    }
    if let Some(h) = t.max_height {
        p.push_str(&format!("&maxHeight={h}"));
    }
    if t.start_ticks > 0 {
        p.push_str(&format!("&startTimeTicks={}", t.start_ticks));
    }
    p.push_str(&format!("&ApiKey={}", enc(token)));
    p
}

/// A user's avatar.
pub fn user_image(user_id: &str, tag: &str) -> String {
    format!("/Users/{}/Images/Primary?quality=90&tag={}", enc(user_id), enc(tag))
}

/// Whether `path` addresses a user's avatar ([`user_image`]), under any query or prefix.
pub fn is_user_image(path: &str) -> bool {
    path.find("/Users/").is_some_and(|at| path[at..].contains("/Images/Primary"))
}

/// An image, scaled by the server to fill `w`×`h` (either may be 0 to leave it free).
pub fn image(item_id: &str, kind: ImageKind, tag: Option<&str>, w: i64, h: i64) -> String {
    let mut p = format!("/Items/{}/Images/{}?quality=90", enc(item_id), kind.as_str());
    if let Some(t) = tag {
        p.push_str(&format!("&tag={}", enc(t)));
    }
    if w > 0 {
        p.push_str(&format!("&fillWidth={w}"));
    }
    if h > 0 {
        p.push_str(&format!("&fillHeight={h}"));
    }
    p
}

/// `POST`: playback started.
pub fn playing() -> String {
    "/Sessions/Playing".into()
}

/// `POST`: periodic position report.
pub fn playing_progress() -> String {
    "/Sessions/Playing/Progress".into()
}

/// `POST`: playback stopped; the server stores the resume point from this.
pub fn playing_stopped() -> String {
    "/Sessions/Playing/Stopped".into()
}

/// `POST` with a `DeviceProfile`: the server's verdict for playing an item on this device.
pub fn playback_info(
    user_id: &str,
    item_id: &str,
    media_source_id: Option<&str>,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
) -> String {
    let mut p = format!(
        "/Items/{}/PlaybackInfo?userId={}&isPlayback=true&autoOpenLiveStream=false",
        enc(item_id),
        enc(user_id)
    );
    if let Some(id) = media_source_id {
        p.push_str(&format!("&mediaSourceId={}", enc(id)));
    }
    if let Some(i) = audio_index {
        p.push_str(&format!("&audioStreamIndex={i}"));
    }
    if let Some(i) = subtitle_index {
        p.push_str(&format!("&subtitleStreamIndex={i}"));
    }
    p
}

/// `DELETE`: stop this device's transcode for a play session.
pub fn stop_encoding(device_id: &str, play_session_id: &str) -> String {
    format!(
        "/Videos/ActiveEncodings?deviceId={}&playSessionId={}",
        enc(device_id),
        enc(play_session_id)
    )
}

// ---- query parameters ----

/// `url` with `key=value` set (replacing any value it had, whatever the key's case).
pub fn with_param(url: &str, key: &str, value: &str) -> String {
    let mut url = without_param(url, key);
    append_param(&mut url, key, value);
    url
}

/// `url` without `key` (whatever its case).
pub fn without_param(url: &str, key: &str) -> String {
    let Some((path, query)) = url.split_once('?') else { return url.to_string() };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.split('=').next().unwrap_or("").eq_ignore_ascii_case(key))
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// The value of `key` (whatever its case) in `url`'s query.
pub fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

/// Append `key=value` to `path`'s query, starting one when it has none.
pub fn append_param(path: &mut String, key: &str, value: &str) {
    path.push(if path.contains('?') { '&' } else { '?' });
    path.push_str(key);
    path.push('=');
    path.push_str(value);
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_users_picture_is_a_user_image() {
        assert!(is_user_image(&format!("{}&fillWidth=300&ApiKey=k", user_image("5f0a", "t1"))));
        assert!(!is_user_image(&image("5f0a", ImageKind::Primary, None, 300, 0)));
    }

    use super::*;

    #[test]
    fn library_grid_query_carries_every_part() {
        let p = items(
            "u1",
            &ItemsQuery {
                parent_id: Some("lib1"),
                include_item_types: Some("Movie"),
                recursive: true,
                sort_by: Some(SortBy::DateCreated),
                descending: true,
                start_index: 48,
                limit: 24,
                filters: Some("IsUnplayed"),
                name_starts_with: Some("B"),
                ..Default::default()
            },
        );
        assert!(p.starts_with("/Items?userId=u1&parentId=lib1&includeItemTypes=Movie&recursive=true"), "{p}");
        assert!(p.contains("&sortBy=DateCreated%2CName&sortOrder=Descending"), "{p}");
        assert!(p.contains("&filters=IsUnplayed&nameStartsWith=B"), "{p}");
        assert!(p.ends_with("&startIndex=48&enableTotalRecordCount=true&limit=24"), "{p}");
    }

    #[test]
    fn a_count_only_query_asks_for_no_rows() {
        let p = items("u1", &ItemsQuery { include_item_types: Some("BoxSet"), count_only: true, ..Default::default() });
        assert!(p.ends_with("&startIndex=0&enableTotalRecordCount=true&limit=0"), "{p}");
    }

    #[test]
    fn search_terms_are_encoded() {
        let p = items("u", &ItemsQuery { search_term: Some("star wars & co"), recursive: true, ..Default::default() });
        assert!(p.contains("&searchTerm=star%20wars%20%26%20co"), "{p}");
    }

    #[test]
    fn image_sizes_are_optional() {
        assert_eq!(image("i", ImageKind::Primary, Some("t"), 300, 0), "/Items/i/Images/Primary?quality=90&tag=t&fillWidth=300");
        assert_eq!(image("i", ImageKind::Backdrop, None, 0, 0), "/Items/i/Images/Backdrop?quality=90");
    }

    #[test]
    fn auth_bodies_are_json() {
        assert_eq!(authenticate_by_name_body("alice", "p\"w"), r#"{"Pw":"p\"w","Username":"alice"}"#);
        assert_eq!(quick_connect_authenticate_body("s"), r#"{"Secret":"s"}"#);
    }
}
