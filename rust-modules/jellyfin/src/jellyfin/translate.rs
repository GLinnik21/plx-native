//! Jellyfin DTOs into the app's common item model.
//!
//! The screens and stores above read `plx_plex::plex::{MediaContainer, Metadata, Media, MediaPart,
//! Stream, Hub, LibrarySection, Chapter, Marker}`. This module fills those from Jellyfin's answers
//! so nothing above the backend has to know which server it is talking to.
//!
//! Identity: an item's `rating_key` is its Jellyfin id. Its `key` follows the Plex convention the
//! data layer reads (a leaf's key is the item, a container's key ends in `/children`) under a
//! `/jf/items/` prefix that only this backend resolves. A library's `i64` section key comes from
//! [`SectionIds`].
//!
//! Units: durations and offsets become milliseconds, bitrates kbit/s, dates `YYYY-MM-DD` and
//! timestamps unix seconds, which is what the model holds.

use crate::jellyfin::dto::{
    BaseItemDto, BaseItemPerson, ChapterInfo, MediaSegmentDto, MediaSourceInfo, MediaStream, QueryResult,
    RemoteSubtitleInfo, UserLanguageConfig, UserLanguageUpdate,
};
use crate::jellyfin::blurhash;
use crate::jellyfin::ids::{normalize, SectionIds};
use crate::jellyfin::image_tags;
use crate::jellyfin::languages;
use crate::jellyfin::paths::{self, ImageKind};
use crate::jellyfin::time::{iso_date, iso_to_unix, ticks_to_ms};
use plx_plex::plex::account::AudioPreferences;
use plx_plex::plex::{
    Chapter, HexColor, Hub, LibrarySection, Marker, Media, MediaContainer, MediaId, MediaPart, Metadata, SearchResult, Stream,
    Tag, Trickplay, UltraBlurColors,
};

/// The key prefix for items this backend owns.
pub const ITEM_KEY_PREFIX: &str = "/jf/items/";

/// What a translation needs to know beyond the DTO itself.
pub struct Ctx<'a> {
    pub sections: &'a SectionIds,
    /// The library the items were listed from, when the caller knows it. Jellyfin items don't name
    /// their library directly, so a listing passes it down; a detail read may not know it.
    pub section_guid: Option<&'a str>,
}

impl Ctx<'_> {
    fn section_id(&self) -> i64 {
        self.section_guid.map_or(0, |g| self.sections.key(g))
    }
}

/// The common model's type for a Jellyfin item type, or `None` for one the app doesn't show.
pub fn kind_of(jf_type: &str) -> Option<&'static str> {
    Some(match jf_type {
        "Movie" => "movie",
        "Series" => "show",
        "Season" => "season",
        "Episode" => "episode",
        "BoxSet" => "collection",
        "Trailer" | "Video" | "MusicVideo" => "clip",
        _ => return None,
    })
}

/// The library type the app keys its screens on, from a view's `CollectionType`.
pub fn section_kind(collection_type: Option<&str>) -> &'static str {
    match collection_type.unwrap_or("") {
        "movies" | "homevideos" | "musicvideos" => "movie",
        "tvshows" => "show",
        "music" => "artist",
        "boxsets" => "collection",
        "photos" => "photo",
        "livetv" => "livetv",
        _ => "mixed",
    }
}

/// The user's libraries as sections. Views of kinds FinNative v1 doesn't browse (music, photos,
/// live TV, playlists) are kept, so the caller can decide whether to list them greyed out.
pub fn sections(views: &QueryResult<BaseItemDto>, ids: &SectionIds) -> MediaContainer {
    let directory: Vec<LibrarySection> = views
        .items
        .iter()
        .map(|v| LibrarySection {
            key: ids.key(&v.id).to_string(),
            kind: section_kind(v.collection_type.as_deref()).to_string(),
            title: v.name.clone(),
            size: v.child_count.unwrap_or(0),
        })
        .collect();
    MediaContainer {
        size: directory.len() as i64,
        total_size: directory.len() as i64,
        directory,
        ..Default::default()
    }
}

/// A page of items as a container.
pub fn container(page: &QueryResult<BaseItemDto>, ctx: &Ctx<'_>) -> MediaContainer {
    let metadata: Vec<Metadata> = page.items.iter().filter_map(|i| metadata(i, ctx)).collect();
    MediaContainer {
        size: metadata.len() as i64,
        total_size: page.total_record_count.max(page.items.len() as i64),
        offset: page.start_index,
        metadata,
        ..Default::default()
    }
}

/// A genre list as the directory rows of a Plex `/genre` value list: the id is the key.
pub fn genre_rows(page: &QueryResult<BaseItemDto>) -> MediaContainer {
    let directory: Vec<LibrarySection> = page
        .items
        .iter()
        .filter(|g| !g.id.is_empty() && !g.name.is_empty())
        .map(|g| LibrarySection { key: g.id.clone(), title: g.name.clone(), ..Default::default() })
        .collect();
    MediaContainer { size: directory.len() as i64, total_size: directory.len() as i64, directory, ..Default::default() }
}

/// A bare array (`/Items/Latest`, `/Items/{id}/SpecialFeatures`) as a container.
pub fn container_of(items: &[BaseItemDto], ctx: &Ctx<'_>) -> MediaContainer {
    let metadata: Vec<Metadata> = items.iter().filter_map(|i| metadata(i, ctx)).collect();
    MediaContainer {
        size: metadata.len() as i64,
        total_size: metadata.len() as i64,
        metadata,
        ..Default::default()
    }
}

/// A home or library shelf. `identifier` is the stable id the data layer recognises
/// (`home.continue`, `home.movies.recent`, `home.television.recent`, ...); `key` is the route the
/// shelf's "more" pages from.
/// A hub list of `hubs`, without the shelves that came back empty.
pub fn hubs(hubs: Vec<Hub>) -> MediaContainer {
    let hubs: Vec<Hub> = hubs.into_iter().filter(|h| !h.metadata.is_empty() || !h.directory.is_empty()).collect();
    MediaContainer { size: hubs.len() as i64, total_size: hubs.len() as i64, hub: hubs, ..Default::default() }
}

pub fn hub(identifier: &str, title: &str, key: &str, items: MediaContainer) -> Hub {
    let kind = items.metadata.first().map(|m| m.kind.clone()).unwrap_or_default();
    Hub {
        more: items.total_size > items.size,
        kind,
        hub_identifier: identifier.to_string(),
        key: key.to_string(),
        title: title.to_string(),
        size: items.size,
        total_size: items.total_size,
        metadata: items.metadata,
        ..Default::default()
    }
}

/// One item, or `None` for a type the app doesn't show.
pub fn metadata(i: &BaseItemDto, ctx: &Ctx<'_>) -> Option<Metadata> {
    let kind = kind_of(&i.kind)?;
    let id = normalize(&i.id);
    let ud = i.user_data.clone().unwrap_or_default();
    // The favourite mark travels beside the model (`super::favorites`): recorded here, where
    // every item read passes, and only when the answer carried the user's data at all.
    if let Some(u) = &i.user_data {
        super::favorites::note(&id, u.is_favorite);
    }
    note_image_tags(i, &id);
    let duration = ticks_to_ms(i.run_time_ticks);
    let is_container = matches!(kind, "show" | "season" | "collection");

    let mut m = Metadata {
        kind: kind.to_string(),
        key: if is_container {
            format!("{ITEM_KEY_PREFIX}{id}/children")
        } else {
            format!("{ITEM_KEY_PREFIX}{id}")
        },
        rating_key: id.clone(),
        guid: guid_of(i),
        title: i.name.clone(),
        library_section_id: ctx.section_id(),
        year: i.production_year.unwrap_or(0),
        content_rating: i.official_rating.clone().unwrap_or_default(),
        summary: i.overview.clone().unwrap_or_default(),
        tagline: i.taglines.first().cloned().unwrap_or_default(),
        studio: i.studios.first().map(|s| s.name.clone()).unwrap_or_default(),
        originally_available_at: i.premiere_date.as_deref().map(iso_date).unwrap_or_default(),
        duration,
        view_offset: ticks_to_ms(Some(ud.playback_position_ticks)),
        last_viewed_at: ud.last_played_date.as_deref().map(iso_to_unix).unwrap_or(0).max(0),
        index: i.index_number.unwrap_or(0),
        child_count: i.child_count.unwrap_or(0),
        updated_at: i.date_created.as_deref().map(iso_to_unix).unwrap_or(0).max(0),
        parent_index: i.parent_index_number,
        view_count: if ud.played { ud.play_count.max(1) } else { 0 },
        thumb: primary_image(i),
        art: backdrop_image(i),
        ultra_blur_colors: ultra_blur(i),
        genre: i.genres.iter().map(|g| tag(g)).collect(),
        country: i.production_locations.iter().map(|c| tag(c)).collect(),
        director: people(&i.people, "Director"),
        writer: people(&i.people, "Writer"),
        role: people(&i.people, "Actor"),
        chapter: chapters(&i.chapters, duration),
        media: i.media_sources.iter().map(|s| media(&id, s)).collect(),
        ..Default::default()
    };

    if kind == "clip" {
        // The data layer tells a trailer from the other extras by this (`Extra::is_trailer`).
        m.subtype = match (i.kind.as_str(), i.extra_type.as_deref().unwrap_or("")) {
            ("Trailer", _) | (_, "Trailer") => "trailer",
            (_, "BehindTheScenes") => "behindTheScenes",
            (_, "Scene" | "Sample") => "sceneOrSample",
            _ => "",
        }
        .to_string();
        if m.subtype == "trailer" {
            m.extra_type = 1;
        }
    }

    match kind {
        "episode" => {
            m.parent_rating_key = i.season_id.as_deref().map(normalize).unwrap_or_default();
            m.parent_title = i.season_name.clone().unwrap_or_default();
            m.grandparent_rating_key = i.series_id.as_deref().map(normalize).unwrap_or_default();
            m.grandparent_title = i.series_name.clone().unwrap_or_default();
            if let (Some(sid), Some(tag)) = (i.series_id.as_deref(), i.series_primary_image_tag.as_deref()) {
                m.grandparent_thumb = paths::image(&normalize(sid), ImageKind::Primary, Some(tag), 0, 0);
            }
        }
        "season" => {
            let show = i.series_id.as_deref().or(i.parent_id.as_deref());
            m.parent_rating_key = show.map(normalize).unwrap_or_default();
            m.parent_title = i.series_name.clone().unwrap_or_default();
            if let (Some(sid), Some(tag)) = (i.series_id.as_deref(), i.series_primary_image_tag.as_deref()) {
                m.parent_thumb = paths::image(&normalize(sid), ImageKind::Primary, Some(tag), 0, 0);
            }
        }
        _ => {}
    }

    if matches!(kind, "show" | "season") {
        // Episodes under it, and how many of them are watched. `RecursiveItemCount` counts episodes
        // on a show (seasons are folders) and `UnplayedItemCount` the unwatched ones.
        let leaves = i.recursive_item_count.or(i.child_count).unwrap_or(0).max(0);
        let unplayed = ud.unplayed_item_count.unwrap_or(if ud.played { 0 } else { leaves });
        m.leaf_count = leaves;
        m.viewed_leaf_count = (leaves - unplayed).clamp(0, leaves);
    }

    // Critic score: Jellyfin's `CriticRating` is Rotten Tomatoes' percentage, which the model holds
    // on a /10 scale with the tomato state in the image URI. `CommunityRating` is the metadata
    // provider's /10 audience score (TMDB by default).
    if let Some(c) = i.critic_rating.filter(|c| *c > 0.0) {
        m.rating = c / 10.0;
        m.rating_image =
            if c >= 60.0 { "rottentomatoes://image.rating.ripe" } else { "rottentomatoes://image.rating.rotten" }.into();
    }
    if let Some(a) = i.community_rating.filter(|a| *a > 0.0) {
        m.audience_rating = a;
        m.audience_rating_image = "themoviedb://image.rating".into();
    }
    Some(m)
}

/// The item's portable identity, for matching the same film across servers: its IMDb id, else its
/// TMDB or TVDB id, in the URI shape the model's `guid` holds.
fn guid_of(i: &BaseItemDto) -> String {
    for (provider, scheme) in [("Imdb", "imdb"), ("Tmdb", "tmdb"), ("Tvdb", "tvdb")] {
        if let Some(v) = i.provider_ids.get(provider).filter(|v| !v.is_empty()) {
            return format!("{scheme}://{v}");
        }
    }
    String::new()
}

/// Record the tags of the images the app asks for by item alone ([`super::image_tags`]), so a
/// logo or artwork the server replaced is a new request and not the cached old one.
fn note_image_tags(i: &BaseItemDto, id: &str) {
    for (name, kind) in [("Logo", ImageKind::Logo), ("Primary", ImageKind::Primary)] {
        if let Some(tag) = i.image_tags.get(name) {
            image_tags::note(id, kind, tag);
        }
    }
    if let Some(tag) = i.backdrop_image_tags.first() {
        image_tags::note(id, ImageKind::Backdrop, tag);
    }
    if let (Some(pid), Some(tag)) = (i.parent_logo_item_id.as_deref(), i.parent_logo_image_tag.as_deref()) {
        image_tags::note(&normalize(pid), ImageKind::Logo, tag);
    }
    if let (Some(pid), Some(tag)) = (i.parent_backdrop_item_id.as_deref(), i.parent_backdrop_image_tags.first()) {
        image_tags::note(&normalize(pid), ImageKind::Backdrop, tag);
    }
}

fn primary_image(i: &BaseItemDto) -> String {
    match i.image_tags.get("Primary") {
        Some(tag) => paths::image(&normalize(&i.id), ImageKind::Primary, Some(tag), 0, 0),
        None => String::new(),
    }
}

/// The item's own backdrop, else the one it inherits (an episode or season shows its series').
fn backdrop_image(i: &BaseItemDto) -> String {
    if let Some(tag) = i.backdrop_image_tags.first() {
        return paths::image(&normalize(&i.id), ImageKind::Backdrop, Some(tag), 0, 0);
    }
    match (i.parent_backdrop_item_id.as_deref(), i.parent_backdrop_image_tags.first()) {
        (Some(pid), Some(tag)) => paths::image(&normalize(pid), ImageKind::Backdrop, Some(tag), 0, 0),
        _ => String::new(),
    }
}

/// The page's ambient colours (PMS's `UltraBlurColors`) from the BlurHash of the backdrop
/// [`backdrop_image`] shows, else of the poster: `None` when the server sent no hash for either, or
/// a malformed one, and the wash then holds whatever ground it already had, as for a Plex item with
/// no envelope.
fn ultra_blur(i: &BaseItemDto) -> Option<UltraBlurColors> {
    let hash = |kind: &str, tag: Option<&String>| i.image_blur_hashes.get(kind)?.get(tag?);
    let backdrop = i.backdrop_image_tags.first().or(i.parent_backdrop_image_tags.first());
    let c = hash("Backdrop", backdrop)
        .and_then(|h| blurhash::corners(h))
        .or_else(|| hash("Primary", i.image_tags.get("Primary")).and_then(|h| blurhash::corners(h)))?;
    Some(UltraBlurColors {
        top_left: HexColor(c[0]),
        top_right: HexColor(c[1]),
        bottom_right: HexColor(c[2]),
        bottom_left: HexColor(c[3]),
    })
}

fn tag(name: &str) -> Tag {
    Tag { tag: name.to_string(), ..Default::default() }
}

fn people(all: &[BaseItemPerson], kind: &str) -> Vec<Tag> {
    all.iter()
        .filter(|p| p.kind == kind)
        .map(|p| Tag {
            tag: p.name.clone(),
            role: p.role.clone(),
            thumb: match p.primary_image_tag.as_deref() {
                Some(t) => paths::image(&normalize(&p.id), ImageKind::Primary, Some(t), 0, 0),
                None => String::new(),
            },
            tag_key: normalize(&p.id),
            ..Default::default()
        })
        .collect()
}

/// A page of person items as search hits: each a directory tag keyed by the person's id, the
/// key the person page opens, with the headshot when the server has one.
pub fn people_hits(page: &QueryResult<BaseItemDto>) -> MediaContainer {
    let search_result: Vec<SearchResult> = page
        .items
        .iter()
        .map(|i| SearchResult {
            metadata: None,
            directory: Some(Tag {
                tag: i.name.clone(),
                thumb: primary_image(i),
                tag_key: normalize(&i.id),
                ..Default::default()
            }),
        })
        .collect();
    MediaContainer {
        size: search_result.len() as i64,
        total_size: page.total_record_count.max(search_result.len() as i64),
        offset: page.start_index,
        search_result,
        ..Default::default()
    }
}

/// Chapters with end offsets: each ends where the next starts, the last at the item's end.
fn chapters(all: &[ChapterInfo], duration_ms: i64) -> Vec<Chapter> {
    let starts: Vec<i64> = all.iter().map(|c| ticks_to_ms(Some(c.start_position_ticks))).collect();
    all.iter()
        .enumerate()
        .map(|(n, c)| Chapter {
            index: n as i64 + 1,
            start_time_offset: starts[n],
            end_time_offset: starts.get(n + 1).copied().unwrap_or(duration_ms).max(starts[n]),
            tag: c.name.clone(),
            ..Default::default()
        })
        .collect()
}

/// The id of `i`'s media source whose part is `part_id` ([`part_id`]), the version that plays.
pub fn source_of_part(i: &BaseItemDto, part_id: i64) -> Option<&str> {
    i.media_sources.iter().find(|s| self::part_id(&s.id) == part_id).map(|s| s.id.as_str())
}

/// The narrowest picture width at or above this is the one scrubbed with: wide enough to read on
/// a television, and the sheet it comes on is the one the player has to decode (a 320-wide 10×10
/// sheet of 16:9 pictures is 3200×1800).
const TRICKPLAY_MIN_WIDTH: i64 = 240;

/// The scrubbing thumbnails of the version that plays (`source`, else the server's first), at the
/// narrowest width of at least [`TRICKPLAY_MIN_WIDTH`], else the widest there is. `None` when the
/// server made none, or describes them with a zero it cannot be scrubbed by.
pub fn trickplay(i: &BaseItemDto, source: Option<&str>) -> Option<Trickplay> {
    let first = source.or_else(|| i.media_sources.first().map(|s| s.id.as_str()));
    let widths = match first.and_then(|s| i.trickplay.get(s)) {
        Some(w) => w,
        None if i.trickplay.len() == 1 => i.trickplay.values().next()?,
        None => return None,
    };
    let usable = || widths.values().filter(|t| {
        t.width > 0 && t.height > 0 && t.tile_width > 0 && t.tile_height > 0 && t.thumbnail_count > 0 && t.interval > 0
    });
    let t = usable()
        .filter(|t| t.width >= TRICKPLAY_MIN_WIDTH)
        .min_by_key(|t| t.width)
        .or_else(|| usable().max_by_key(|t| t.width))?;
    let u = |v: i64| u32::try_from(v).ok();
    Some(Trickplay {
        width: u(t.width)?,
        height: u(t.height)?,
        tile_cols: u(t.tile_width)?,
        tile_rows: u(t.tile_height)?,
        count: u(t.thumbnail_count)?,
        interval_ms: t.interval,
    })
}

/// A person item as the person page's biography record.
pub fn person_profile(i: &BaseItemDto) -> plx_plex::plex::discover::PersonProfile {
    plx_plex::plex::discover::PersonProfile {
        title: i.name.clone(),
        summary: i.overview.clone().unwrap_or_default(),
        born_at: i.premiere_date.as_deref().map(iso_date).unwrap_or_default(),
        died_at: i.end_date.as_deref().map(iso_date).unwrap_or_default(),
        birth_place: i.production_locations.first().cloned().unwrap_or_default(),
        ..Default::default()
    }
}

/// Subtitle candidates in the shape of a Plex subtitle search: one subtitle stream row each, keyed
/// by the candidate's id (what the download is addressed by). A match on the file itself ranks above
/// every title match, then the most downloaded first.
pub fn subtitle_candidates(found: &[RemoteSubtitleInfo]) -> MediaContainer {
    let stream: Vec<Stream> = found
        .iter()
        .filter(|c| !c.id.is_empty())
        .map(|c| {
            let lang = c.three_letter_iso_language_name.clone();
            let format = c.format.to_ascii_lowercase();
            Stream {
                stream_type: 3,
                key: c.id.clone(),
                title: c.name.clone(),
                provider_title: c.provider_name.clone(),
                language: lang.clone(),
                language_code: lang,
                codec: format.clone(),
                format,
                extended_display_title: c.comment.clone(),
                score: c.download_count.max(0).saturating_add(if c.is_hash_match { 1 << 40 } else { 0 }),
                hearing_impaired: i64::from(c.hearing_impaired),
                forced: i64::from(c.forced),
                ..Default::default()
            }
        })
        .collect();
    MediaContainer { size: stream.len() as i64, total_size: stream.len() as i64, stream, ..Default::default() }
}

/// Skip markers from media segments. Recaps and previews have no marker kind in the model yet.
///
/// A credits marker is FINAL, the one Up Next counts down over and then leaves the episode on,
/// only when it runs to the end of the item (within [`FINAL_CREDITS_SLACK_MS`] of `duration_ms`),
/// as Plex's `final` means. Credits with more of the episode after them (a post-credits scene, a
/// preview, a block a detector placed early) keep the Skip Credits pill instead.
pub fn markers(segments: &[MediaSegmentDto], duration_ms: i64) -> Vec<Marker> {
    segments
        .iter()
        .filter_map(|s| {
            let kind = match s.kind.as_str() {
                "Intro" => "intro",
                "Outro" => "credits",
                "Commercial" => "commercial",
                _ => return None,
            };
            Some(Marker {
                kind: kind.to_string(),
                start_time_offset: ticks_to_ms(Some(s.start_ticks)),
                end_time_offset: ticks_to_ms(Some(s.end_ticks)),
                is_final: i64::from(kind == "credits" && runs_to_the_end(ticks_to_ms(Some(s.end_ticks)), duration_ms)),
                ..Default::default()
            })
        })
        .collect()
}

/// How far before the item's end credits may stop and still count as running to it: segment
/// detectors and chapter tables rarely land on the last frame.
pub const FINAL_CREDITS_SLACK_MS: i64 = 5_000;

fn runs_to_the_end(end_ms: i64, duration_ms: i64) -> bool {
    duration_ms > 0 && end_ms >= duration_ms - FINAL_CREDITS_SLACK_MS
}

/// Skip markers from chapter names, for a server with no segment provider: a chapter called
/// Intro, Opening or OP is the intro, one called Credits, Ending, ED or Outro is the credits. An
/// "intro" longer than [`CHAPTER_INTRO_MAX_MS`] is a mislabelled scene, not a title sequence, and
/// is left alone so the button never skips part of the story.
pub fn chapter_markers(chapters: &[Chapter], duration_ms: i64) -> Vec<Marker> {
    let kind_of = |name: &str| {
        let name: String = name.chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>().to_lowercase();
        match name.split_whitespace().collect::<Vec<_>>().join(" ").as_str() {
            "intro" | "opening" | "op" | "opening credits" | "opening theme" | "opening song" | "title sequence" => Some("intro"),
            "credits" | "end credits" | "ending" | "ending credits" | "closing credits" | "ed" | "ending theme" | "ending song"
            | "outro" => Some("credits"),
            _ => None,
        }
    };
    chapters
        .iter()
        .filter_map(|c| {
            let kind = kind_of(&c.tag)?;
            let length = c.end_time_offset - c.start_time_offset;
            if length <= 0 || (kind == "intro" && length > CHAPTER_INTRO_MAX_MS) {
                return None;
            }
            Some(Marker {
                kind: kind.to_string(),
                start_time_offset: c.start_time_offset,
                end_time_offset: c.end_time_offset,
                is_final: i64::from(kind == "credits" && runs_to_the_end(c.end_time_offset, duration_ms)),
            })
        })
        .collect()
}

/// The longest chapter [`chapter_markers`] takes for an intro.
pub const CHAPTER_INTRO_MAX_MS: i64 = 4 * 60 * 1000;

/// A Jellyfin codec name in the spelling the model (and the playback route) uses. Jellyfin reports
/// FFmpeg's names; the model uses Plex's, which differ for a handful of codecs.
pub fn codec_name(jf: &str) -> String {
    let c = jf.to_ascii_lowercase();
    match c.as_str() {
        "dts" => "dca".into(),
        "subrip" => "srt".into(),
        "pgssub" | "hdmv_pgs_subtitle" => "pgs".into(),
        "dvdsub" | "dvd_subtitle" => "vobsub".into(),
        "webvtt" => "vtt".into(),
        "ssa" => "ass".into(),
        _ => c,
    }
}

/// Plex's resolution bucket for a frame size.
fn resolution(width: i64, height: i64) -> &'static str {
    match (width, height) {
        (w, h) if w >= 3200 || h >= 1800 => "4k",
        (w, h) if w >= 1600 || h >= 900 => "1080",
        (w, h) if w >= 1100 || h >= 650 => "720",
        (w, h) if w >= 640 || h >= 400 => "480",
        _ => "sd",
    }
}

/// A part id the model can hold. Jellyfin media source ids are GUIDs; the model's part id is an
/// integer used as a cache and selection key, so it is a stable 63-bit hash of the GUID. The GUID
/// itself rides in the part key, which is what every request is built from.
pub fn part_id(media_source_id: &str) -> i64 {
    // FNV-1a, folded to a positive i64.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in normalize(media_source_id).bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h >> 1) as i64
}

/// The part key for a media source: the direct-play route without its token, which the backend's
/// `direct_play_url` adds. The file's container is spelled as the path's extension
/// (`/Videos/{id}/stream.mkv`, a route Jellyfin serves like `/stream`): the player decides it can
/// pull a file itself by that extension, as it does from a Plex part's file name.
pub fn part_key(item_id: &str, media_source_id: &str, container: &str) -> String {
    let ext = if container.is_empty() { String::new() } else { format!(".{container}") };
    format!(
        "/Videos/{}/stream{ext}?static=true&mediaSourceId={}",
        normalize(item_id),
        plx_plex::plex::urlenc_str(media_source_id)
    )
}

/// Jellyfin's container name (`mkv`, `mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`) as the single
/// name Plex uses for the same file (`mkv`, `mp4`, ...).
pub fn container_name(jf: &str) -> String {
    let names: Vec<&str> = jf.split(',').map(str::trim).collect();
    let has = |n: &str| names.iter().any(|x| x.eq_ignore_ascii_case(n));
    if has("mkv") || has("matroska") {
        "mkv".into()
    } else if has("webm") {
        "webm".into()
    } else if has("mp4") || has("m4v") || has("mov") {
        if has("m4v") { "m4v".into() } else if has("mp4") { "mp4".into() } else { "mov".into() }
    } else {
        names.first().map(|n| n.to_ascii_lowercase()).unwrap_or_default()
    }
}

/// One media source as the model's `Media` with its single `Part`.
pub fn media(item_id: &str, s: &MediaSourceInfo) -> Media {
    let video = s.media_streams.iter().find(|m| m.kind == "Video");
    let audio = s
        .default_audio_stream_index
        .and_then(|i| s.media_streams.iter().find(|m| m.kind == "Audio" && m.index == i))
        .or_else(|| s.media_streams.iter().find(|m| m.kind == "Audio" && m.is_default))
        .or_else(|| s.media_streams.iter().find(|m| m.kind == "Audio"));
    let (w, h) = video.map_or((0, 0), |v| (v.width.unwrap_or(0), v.height.unwrap_or(0)));
    let container = container_name(s.container.as_deref().unwrap_or(""));
    let streams: Vec<Stream> = s
        .media_streams
        .iter()
        .filter_map(|m| stream(item_id, s, m))
        .collect();
    Media {
        // A version is chosen by this id (`Metadata::media_version`), so it is the source's own.
        id: MediaId(part_id(&s.id)),
        title: s.name.clone().unwrap_or_default(),
        video_codec: video.map(|v| codec_name(&v.codec)).unwrap_or_default(),
        audio_codec: audio.map(|a| codec_name(&a.codec)).unwrap_or_default(),
        bitrate: s.bitrate.unwrap_or(0) / 1000,
        width: w,
        height: h,
        video_resolution: if w > 0 || h > 0 { resolution(w, h).to_string() } else { String::new() },
        container: container.clone(),
        aspect_ratio: if h > 0 { ((w as f64 / h as f64) * 100.0).round() / 100.0 } else { 0.0 },
        video_profile: video.and_then(|v| v.profile.clone()).unwrap_or_default().to_ascii_lowercase(),
        part: vec![MediaPart {
            id: part_id(&s.id),
            key: part_key(item_id, &s.id, &container),
            file: s.path.clone().unwrap_or_default(),
            size: s.size.unwrap_or(0),
            container,
            stream: streams,
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// One stream, or `None` for kinds the player has no use for (cover art, data, lyrics).
fn stream(item_id: &str, src: &MediaSourceInfo, m: &MediaStream) -> Option<Stream> {
    let stream_type = match m.kind.as_str() {
        "Video" => 1,
        "Audio" => 2,
        "Subtitle" => 3,
        _ => return None,
    };
    let selected = match stream_type {
        2 => src.default_audio_stream_index == Some(m.index),
        3 => src.default_subtitle_stream_index == Some(m.index),
        _ => true,
    };
    let range_type = m.video_range_type.as_deref().unwrap_or("");
    let dovi = range_type.starts_with("DOVI") || m.dv_profile.is_some();
    let lang = m.language.clone().unwrap_or_default();
    let mut s = Stream {
        // Jellyfin indexes streams per media source; the model's id need only be unique within one.
        id: m.index,
        stream_type,
        index: m.index,
        codec: codec_name(&m.codec),
        language: lang.clone(),
        language_code: lang,
        frame_rate: m.real_frame_rate.or(m.average_frame_rate).unwrap_or(0.0),
        color_trc: m.color_transfer.clone().unwrap_or_default(),
        bitrate: m.bit_rate.unwrap_or(0) / 1000,
        profile: m.profile.clone().unwrap_or_default().to_ascii_lowercase(),
        bit_depth: m.bit_depth.unwrap_or(0),
        channels: m.channels.unwrap_or(0),
        sampling_rate: m.sample_rate.unwrap_or(0),
        audio_channel_layout: m.channel_layout.clone().unwrap_or_default(),
        display_title: m.display_title.clone().unwrap_or_default(),
        extended_display_title: m.display_title.clone().unwrap_or_default(),
        title: m.title.clone().unwrap_or_default(),
        hearing_impaired: i64::from(m.is_hearing_impaired),
        forced: i64::from(m.is_forced),
        is_default: i64::from(m.is_default),
        selected: i64::from(selected),
        ..Default::default()
    };
    if dovi {
        s.dovi_present = 1;
        s.dovi_profile = m.dv_profile.unwrap_or(0);
        s.dovi_level = m.dv_level.unwrap_or(0);
        s.dovi_bl_compat_id = m.dv_bl_signal_compatibility_id.unwrap_or(0);
        s.dovi_rpu_present = m.rpu_present_flag.unwrap_or(0);
        s.dovi_el_present = m.el_present_flag.unwrap_or(0);
        s.dovi_bl_present = m.bl_present_flag.unwrap_or(0);
        s.dovi_version = match (m.dv_version_major, m.dv_version_minor) {
            (Some(a), Some(b)) => format!("{a}.{b}"),
            _ => String::new(),
        };
    }
    // A sidecar subtitle is fetched as a file. Plex keys only external files this way; text tracks
    // muxed into the container are demuxed by the player, so they keep an empty key here too.
    if stream_type == 3 && m.is_external {
        let fmt = if s.codec == "ass" { "ass" } else { "srt" };
        s.key = format!(
            "/Videos/{}/{}/Subtitles/{}/0/Stream.{fmt}",
            normalize(item_id),
            plx_plex::plex::urlenc_str(&src.id),
            m.index
        );
    }
    Some(s)
}

// ---- the user's audio and subtitle preferences ----
//
// The app's model is Plex's account profile (`AudioPreferences`); Jellyfin's is the user's
// `Configuration`. They map as:
//
// - audio: Plex's "auto-select audio" is Jellyfin's `PlayDefaultAudioTrack` negated (with it on,
//   Jellyfin plays the file's default track whatever its language), and the language is
//   `AudioLanguagePreference`, its code converted by [`languages`](super::languages).
// - subtitles, Jellyfin `SubtitleMode` → Plex (`subtitle_mode`, `subtitle_forced`):
//   `Smart` → (1 shown with foreign audio, 0), `Always` → (2 always, 0), `OnlyForced` →
//   (2 always, 2 only forced), `Default` and `None` → (0 manually selected, 0). Saving maps back
//   (0 → `None`, 1 → `Smart`, 2 → `OnlyForced` with "only forced", else `Always`), and only when the
//   pair changed, so a `Default` the user never touched stays `Default`. Jellyfin has no place for
//   Plex's "prefer forced" or "only non-forced", so those read back as 0 after a save.

/// A user's Jellyfin language settings as the app's preferences.
pub fn audio_preferences(c: &UserLanguageConfig) -> AudioPreferences {
    let language = |code: &str| Some(languages::to_app(code)).filter(|c| !c.is_empty());
    let auto_select_audio = !c.play_default_audio_track.unwrap_or(true);
    let stated_language = language(&c.audio_language_preference);
    let (subtitle_mode, subtitle_forced) = subtitle_mode(&c.subtitle_mode);
    AudioPreferences {
        language: if auto_select_audio { stated_language.clone() } else { None },
        auto_select_audio: Some(auto_select_audio),
        stated_language,
        subtitle_language: language(&c.subtitle_language_preference),
        subtitle_mode,
        subtitle_forced,
    }
}

/// Jellyfin's `SubtitleMode` as Plex's (`autoSelectSubtitle`, `defaultSubtitleForced`).
pub fn subtitle_mode(mode: &str) -> (i64, i64) {
    match mode {
        "Smart" => (1, 0),
        "Always" => (2, 0),
        "OnlyForced" => (2, 2),
        _ => (0, 0),
    }
}

/// Plex's (`autoSelectSubtitle`, `defaultSubtitleForced`) as Jellyfin's `SubtitleMode`.
pub fn jellyfin_subtitle_mode(mode: i64, forced: i64) -> &'static str {
    match (mode, forced) {
        (1, _) => "Smart",
        (2, 2) => "OnlyForced",
        (2, _) => "Always",
        _ => "None",
    }
}

/// What a Settings change from `previous` to `next` writes into the user's configuration: only the
/// keys whose value changed.
pub fn language_update(previous: &AudioPreferences, next: &AudioPreferences) -> UserLanguageUpdate {
    let code = |c: &Option<String>| c.as_deref().map_or_else(String::new, languages::to_jellyfin);
    let mut u = UserLanguageUpdate::default();
    if next.auto_select_audio != previous.auto_select_audio {
        u.play_default_audio_track = next.auto_select_audio.map(|on| !on);
    }
    if next.stated_language != previous.stated_language {
        u.audio_language_preference = Some(code(&next.stated_language));
    }
    if next.subtitle_language != previous.subtitle_language {
        u.subtitle_language_preference = Some(code(&next.subtitle_language));
    }
    if (next.subtitle_mode, next.subtitle_forced) != (previous.subtitle_mode, previous.subtitle_forced) {
        u.subtitle_mode = Some(jellyfin_subtitle_mode(next.subtitle_mode, next.subtitle_forced));
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::dto::BaseItemDto;

    #[test]
    fn an_extra_is_a_trailer_by_its_type_or_its_extra_type() {
        let ctx = Ctx { sections: &SectionIds::default(), section_guid: None };
        let subtype = |json: &str| {
            let dto: BaseItemDto = serde_json::from_str(json).unwrap();
            let m = metadata(&dto, &ctx).unwrap();
            (m.subtype, m.extra_type)
        };
        assert_eq!(subtype(r#"{"Id":"a1","Type":"Trailer"}"#), ("trailer".into(), 1));
        assert_eq!(subtype(r#"{"Id":"a2","Type":"Video","ExtraType":"Trailer"}"#), ("trailer".into(), 1));
        assert_eq!(subtype(r#"{"Id":"a3","Type":"Video","ExtraType":"BehindTheScenes"}"#), ("behindTheScenes".into(), 0));
        assert_eq!(subtype(r#"{"Id":"a4","Type":"Video"}"#), (String::new(), 0));
    }

    const MOVIE: &str = r#"{
        "Name": "Big Buck Bunny", "Id": "F137A2DD21BBC1B99AA5C0F6BF02A805", "Type": "Movie",
        "ProductionYear": 2008, "OfficialRating": "PG", "Overview": "A rabbit.",
        "Taglines": ["Payback time."], "Studios": [{"Name": "Blender Foundation", "Id": "s"}],
        "Genres": ["Animation", "Comedy"], "PremiereDate": "2008-04-10T00:00:00.0000000Z",
        "DateCreated": "2026-10-01T10:00:00.0000000Z", "RunTimeTicks": 5964000000,
        "CommunityRating": 6.5, "CriticRating": 92,
        "ProviderIds": {"Tmdb": "10378", "Imdb": "tt1254207"},
        "ImageTags": {"Primary": "p1", "Logo": "l1"}, "BackdropImageTags": ["b1"],
        "UserData": {"PlaybackPositionTicks": 1200000000, "PlayCount": 0, "Played": false,
                     "LastPlayedDate": "2026-10-12T20:00:00Z"},
        "People": [
            {"Name": "Sacha Goedegebure", "Id": "d1", "Type": "Director"},
            {"Name": "Bunny", "Id": "a1", "Role": "Himself", "Type": "Actor", "PrimaryImageTag": "h1"}
        ],
        "Chapters": [
            {"StartPositionTicks": 0, "Name": "Opening"},
            {"StartPositionTicks": 3000000000, "Name": "Payback"}
        ],
        "MediaSources": [{
            "Id": "f137a2dd21bbc1b99aa5c0f6bf02a805", "Path": "/media/bbb.mkv", "Container": "mkv",
            "Size": 1000000, "Bitrate": 8500000, "DefaultAudioStreamIndex": 1,
            "DefaultSubtitleStreamIndex": -1,
            "MediaStreams": [
                {"Type": "Video", "Index": 0, "Codec": "hevc", "Width": 3840, "Height": 2160,
                 "Profile": "Main 10", "BitDepth": 10, "RealFrameRate": 24, "BitRate": 8000000,
                 "VideoRange": "HDR", "VideoRangeType": "DOVIWithHDR10", "ColorTransfer": "smpte2084",
                 "DvVersionMajor": 1, "DvVersionMinor": 0, "DvProfile": 8, "DvLevel": 6,
                 "RpuPresentFlag": 1, "ElPresentFlag": 0, "BlPresentFlag": 1,
                 "DvBlSignalCompatibilityId": 1},
                {"Type": "Audio", "Index": 1, "Codec": "eac3", "Language": "eng", "Channels": 6,
                 "ChannelLayout": "5.1", "IsDefault": true, "DisplayTitle": "English - Dolby Digital+ - 5.1"},
                {"Type": "Audio", "Index": 2, "Codec": "dts", "Language": "por", "Channels": 6},
                {"Type": "Subtitle", "Index": 3, "Codec": "subrip", "Language": "por",
                 "IsExternal": true, "IsForced": true},
                {"Type": "Subtitle", "Index": 4, "Codec": "PGSSUB", "Language": "eng"},
                {"Type": "EmbeddedImage", "Index": 5, "Codec": "mjpeg"}
            ]
        }]
    }"#;

    fn parse(json: &str) -> BaseItemDto {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn named_chapters_stand_in_for_missing_segments() {
        let ch = |start: i64, end: i64, tag: &str| Chapter { start_time_offset: start, end_time_offset: end, tag: tag.into(), ..Default::default() };
        let all = [
            ch(0, 90_000, "Prologue"),
            ch(90_000, 180_000, "Opening"),
            ch(180_000, 1_200_000, "Part A"),
            ch(1_200_000, 1_290_000, "ED"),
            ch(1_290_000, 1_320_000, "Preview"),
        ];
        let m = chapter_markers(&all, 1_320_000);
        let got: Vec<_> = m.iter().map(|m| (m.kind.as_str(), m.start_time_offset, m.end_time_offset, m.is_final)).collect();
        assert_eq!(got, [("intro", 90_000, 180_000, 0), ("credits", 1_200_000, 1_290_000, 0)]);
        // Credits that run to the end are the final ones; a long "Intro" is a scene, not a title.
        let m = chapter_markers(&[ch(0, 600_000, "Intro"), ch(600_000, 700_000, "End Credits")], 700_000);
        let got: Vec<_> = m.iter().map(|m| (m.kind.as_str(), m.is_final)).collect();
        assert_eq!(got, [("credits", 1)]);
    }

    #[test]
    fn a_movie_fills_the_model() {
        let ids = SectionIds::new();
        let lib = "9d7ad6afe9afa2dab1a2f6e00ad28fa6";
        let ctx = Ctx { sections: &ids, section_guid: Some(lib) };
        let m = metadata(&parse(MOVIE), &ctx).unwrap();

        assert_eq!(m.kind, "movie");
        assert_eq!(m.rating_key, "f137a2dd21bbc1b99aa5c0f6bf02a805");
        assert_eq!(m.key, "/jf/items/f137a2dd21bbc1b99aa5c0f6bf02a805");
        assert_eq!(m.guid, "imdb://tt1254207");
        assert_eq!(m.library_section_id, 1);
        assert_eq!((m.year, m.content_rating.as_str()), (2008, "PG"));
        assert_eq!((m.tagline.as_str(), m.studio.as_str()), ("Payback time.", "Blender Foundation"));
        assert_eq!(m.originally_available_at, "2008-04-10");
        assert_eq!((m.duration, m.view_offset, m.view_count), (596_400, 120_000, 0));
        assert!(m.last_viewed_at > 0);
        assert_eq!(m.thumb, "/Items/f137a2dd21bbc1b99aa5c0f6bf02a805/Images/Primary?quality=90&tag=p1");
        assert_eq!(m.art, "/Items/f137a2dd21bbc1b99aa5c0f6bf02a805/Images/Backdrop?quality=90&tag=b1");
        assert_eq!(m.genre.iter().map(|g| g.tag.as_str()).collect::<Vec<_>>(), ["Animation", "Comedy"]);
        assert_eq!(m.director[0].tag, "Sacha Goedegebure");
        assert_eq!((m.role[0].tag.as_str(), m.role[0].role.as_str()), ("Bunny", "Himself"));
        assert!(m.role[0].thumb.contains("/Items/a1/Images/Primary"));
        assert_eq!((m.rating, m.rating_image.as_str()), (9.2, "rottentomatoes://image.rating.ripe"));
        assert_eq!(m.audience_rating, 6.5);

        assert_eq!(m.chapter.len(), 2);
        assert_eq!((m.chapter[0].start_time_offset, m.chapter[0].end_time_offset), (0, 300_000));
        assert_eq!((m.chapter[1].start_time_offset, m.chapter[1].end_time_offset), (300_000, 596_400));
    }

    #[test]
    fn a_media_source_becomes_media_part_and_streams() {
        let ids = SectionIds::new();
        let m = metadata(&parse(MOVIE), &Ctx { sections: &ids, section_guid: None }).unwrap();
        assert_eq!(m.library_section_id, 0, "no listing context, no section");
        let media = &m.media[0];
        assert_eq!((media.video_codec.as_str(), media.audio_codec.as_str()), ("hevc", "eac3"));
        assert_eq!((media.width, media.height, media.video_resolution.as_str()), (3840, 2160, "4k"));
        assert_eq!((media.bitrate, media.container.as_str(), media.video_profile.as_str()), (8500, "mkv", "main 10"));

        let part = &media.part[0];
        assert_eq!(
            part.key,
            "/Videos/f137a2dd21bbc1b99aa5c0f6bf02a805/stream.mkv?static=true&mediaSourceId=f137a2dd21bbc1b99aa5c0f6bf02a805"
        );
        assert!(part.id > 0);
        assert_eq!(part.stream.len(), 5, "the embedded cover image is dropped");

        let v = &part.stream[0];
        assert_eq!((v.stream_type, v.codec.as_str(), v.bit_depth, v.color_trc.as_str()), (1, "hevc", 10, "smpte2084"));
        assert_eq!((v.dovi_present, v.dovi_profile, v.dovi_level, v.dovi_bl_compat_id), (1, 8, 6, 1));
        assert_eq!(v.dovi_version, "1.0");

        let (eng, por) = (&part.stream[1], &part.stream[2]);
        assert_eq!((eng.codec.as_str(), eng.selected, eng.channels), ("eac3", 1, 6));
        assert_eq!((por.codec.as_str(), por.selected, por.language_code.as_str()), ("dca", 0, "por"));

        let (ext, pgs) = (&part.stream[3], &part.stream[4]);
        assert_eq!((ext.codec.as_str(), ext.forced, ext.selected), ("srt", 1, 0));
        assert_eq!(ext.key, "/Videos/f137a2dd21bbc1b99aa5c0f6bf02a805/f137a2dd21bbc1b99aa5c0f6bf02a805/Subtitles/3/0/Stream.srt");
        assert_eq!((pgs.codec.as_str(), pgs.key.as_str()), ("pgs", ""));
    }

    #[test]
    fn an_episode_links_to_its_season_and_show() {
        let e = parse(
            r#"{"Name":"Pilot","Id":"e1","Type":"Episode","SeriesName":"The Show","SeriesId":"S-1",
                "SeasonId":"se1","SeasonName":"Season 1","IndexNumber":3,"ParentIndexNumber":1,
                "SeriesPrimaryImageTag":"sp","ParentBackdropItemId":"S-1","ParentBackdropImageTags":["pb"],
                "UserData":{"Played":true,"PlayCount":2}}"#,
        );
        let ids = SectionIds::new();
        let m = metadata(&e, &Ctx { sections: &ids, section_guid: None }).unwrap();
        assert_eq!((m.kind.as_str(), m.key.as_str()), ("episode", "/jf/items/e1"));
        assert_eq!((m.parent_rating_key.as_str(), m.parent_title.as_str()), ("se1", "Season 1"));
        assert_eq!((m.grandparent_rating_key.as_str(), m.grandparent_title.as_str()), ("s1", "The Show"));
        assert_eq!((m.parent_index, m.index, m.view_count), (Some(1), 3, 2));
        assert_eq!(m.grandparent_thumb, "/Items/s1/Images/Primary?quality=90&tag=sp");
        assert_eq!(m.art, "/Items/s1/Images/Backdrop?quality=90&tag=pb", "an episode shows its series' backdrop");
    }

    #[test]
    fn a_show_counts_watched_episodes() {
        let s = parse(
            r#"{"Name":"The Show","Id":"s1","Type":"Series","ChildCount":2,"RecursiveItemCount":20,
                "UserData":{"UnplayedItemCount":5,"Played":false}}"#,
        );
        let ids = SectionIds::new();
        let m = metadata(&s, &Ctx { sections: &ids, section_guid: None }).unwrap();
        assert_eq!((m.kind.as_str(), m.key.as_str()), ("show", "/jf/items/s1/children"));
        assert_eq!((m.child_count, m.leaf_count, m.viewed_leaf_count), (2, 20, 15));
    }

    #[test]
    fn unsupported_types_are_skipped_and_pages_keep_their_totals() {
        let page: QueryResult<BaseItemDto> = serde_json::from_str(
            r#"{"Items":[{"Id":"a","Name":"A","Type":"Movie"},{"Id":"b","Name":"B","Type":"Audio"}],
                "TotalRecordCount":120,"StartIndex":24}"#,
        )
        .unwrap();
        let ids = SectionIds::new();
        let c = container(&page, &Ctx { sections: &ids, section_guid: None });
        assert_eq!((c.size, c.total_size, c.offset), (1, 120, 24));
        let h = hub("home.movies.recent", "Recently Added in Movies", "/jf/latest/1", c);
        assert!(h.more);
        assert_eq!((h.kind.as_str(), h.hub_identifier.as_str()), ("movie", "home.movies.recent"));
    }

    #[test]
    fn views_become_numbered_sections() {
        let views: QueryResult<BaseItemDto> = serde_json::from_str(
            r#"{"Items":[{"Id":"m","Name":"Movies","Type":"CollectionFolder","CollectionType":"movies","ChildCount":300},
                         {"Id":"t","Name":"Shows","Type":"CollectionFolder","CollectionType":"tvshows"},
                         {"Id":"x","Name":"Music","Type":"CollectionFolder","CollectionType":"music"}],
                "TotalRecordCount":3}"#,
        )
        .unwrap();
        let ids = SectionIds::new();
        let c = sections(&views, &ids);
        let got: Vec<_> = c.directory.iter().map(|d| (d.key.as_str(), d.kind.as_str(), d.title.as_str())).collect();
        assert_eq!(got, [("1", "movie", "Movies"), ("2", "show", "Shows"), ("3", "artist", "Music")]);
        assert_eq!(c.directory[0].size, 300);
        assert_eq!(ids.guid(2).as_deref(), Some("t"));
    }

    #[test]
    fn segments_become_skip_markers() {
        let segs: QueryResult<MediaSegmentDto> = serde_json::from_str(
            r#"{"Items":[{"Type":"Intro","StartTicks":300000000,"EndTicks":900000000},
                         {"Type":"Recap","StartTicks":0,"EndTicks":300000000},
                         {"Type":"Outro","StartTicks":24000000000,"EndTicks":25000000000},
                         {"Type":"Outro","StartTicks":26000000000,"EndTicks":27000000000}]}"#,
        )
        .unwrap();
        // The item ends 2 s after the last credits: those run to the end.
        let m = markers(&segs.items, 2_702_000);
        let got: Vec<_> = m.iter().map(|m| (m.kind.as_str(), m.start_time_offset, m.end_time_offset, m.is_final)).collect();
        assert_eq!(
            got,
            [("intro", 30_000, 90_000, 0), ("credits", 2_400_000, 2_500_000, 0), ("credits", 2_600_000, 2_700_000, 1)]
        );
    }

    /// Credits followed by more of the episode (a post-credits scene, a preview, a credits block
    /// detected early) are not final: Up Next does not count down over them, Skip Credits shows.
    #[test]
    fn credits_that_stop_before_the_end_are_not_final() {
        let segs: QueryResult<MediaSegmentDto> = serde_json::from_str(
            r#"{"Items":[{"Type":"Outro","StartTicks":12000000000,"EndTicks":12600000000}]}"#,
        )
        .unwrap();
        // Credits 20:00-21:00 in a 22:30 episode.
        assert_eq!(markers(&segs.items, 1_350_000)[0].is_final, 0);
        // The same credits in a 21:03 episode reach the end.
        assert_eq!(markers(&segs.items, 1_263_000)[0].is_final, 1);
        // An unknown length cannot place them: not final.
        assert_eq!(markers(&segs.items, 0)[0].is_final, 0);
    }

    #[test]
    fn codec_names_follow_the_model() {
        for (jf, model) in [("h264", "h264"), ("HEVC", "hevc"), ("dts", "dca"), ("truehd", "truehd"),
                            ("subrip", "srt"), ("PGSSUB", "pgs"), ("dvdsub", "vobsub"), ("ass", "ass")] {
            assert_eq!(codec_name(jf), model);
        }
    }

    #[test]
    fn part_ids_are_stable_and_positive() {
        let a = part_id("f137a2dd21bbc1b99aa5c0f6bf02a805");
        assert_eq!(a, part_id("F137A2DD-21BB-C1B9-9AA5-C0F6BF02A805"));
        assert!(a > 0);
        assert_ne!(a, part_id("9d7ad6afe9afa2dab1a2f6e00ad28fa6"));
    }

    #[test]
    fn jellyfin_container_lists_read_as_one_plex_name() {
        assert_eq!(container_name("mkv"), "mkv");
        assert_eq!(container_name("matroska,webm"), "mkv");
        assert_eq!(container_name("mov,mp4,m4a,3gp,3g2,mj2"), "mp4");
        assert_eq!(container_name("avi"), "avi");
        assert_eq!(container_name(""), "");
    }

    fn config(audio: &str, play_default: Option<bool>, subs: &str, mode: &str) -> UserLanguageConfig {
        UserLanguageConfig {
            audio_language_preference: audio.into(),
            play_default_audio_track: play_default,
            subtitle_language_preference: subs.into(),
            subtitle_mode: mode.into(),
        }
    }

    #[test]
    fn jellyfin_language_settings_read_as_the_apps_preferences() {
        let p = audio_preferences(&config("por", Some(false), "por", "Smart"));
        assert_eq!(p.language.as_deref(), Some("pt"));
        assert_eq!((p.auto_select_audio, p.stated_language.as_deref()), (Some(true), Some("pt")));
        assert_eq!((p.subtitle_language.as_deref(), p.subtitle_mode, p.subtitle_forced), (Some("pt"), 1, 0));

        // "Play default audio track" (the server's default) keeps the file's track: a stated
        // language, but none to select by.
        let p = audio_preferences(&config("eng", None, "", "Default"));
        assert_eq!((p.language, p.auto_select_audio, p.stated_language.as_deref()), (None, Some(false), Some("en")));
        assert_eq!((p.subtitle_language, p.subtitle_mode, p.subtitle_forced), (None, 0, 0));

        for (mode, plex) in [("Always", (2, 0)), ("OnlyForced", (2, 2)), ("None", (0, 0)), ("Default", (0, 0)), ("", (0, 0))] {
            assert_eq!(subtitle_mode(mode), plex, "{mode}");
        }
    }

    #[test]
    fn a_settings_change_writes_only_the_keys_it_changed() {
        let before = audio_preferences(&config("eng", Some(true), "", "Default"));
        assert!(language_update(&before, &before).is_empty(), "nothing changed, nothing written");

        let after = AudioPreferences {
            language: Some("pt-BR".into()),
            auto_select_audio: Some(true),
            stated_language: Some("pt-BR".into()),
            ..before.clone()
        };
        assert_eq!(
            language_update(&before, &after),
            UserLanguageUpdate {
                audio_language_preference: Some("pob".into()),
                play_default_audio_track: Some(false),
                ..Default::default()
            }
        );

        let smart = AudioPreferences { subtitle_language: Some("pt".into()), subtitle_mode: 1, ..before.clone() };
        let u = language_update(&before, &smart);
        assert_eq!((u.subtitle_language_preference.as_deref(), u.subtitle_mode), (Some("por"), Some("Smart")));

        let cleared = AudioPreferences { subtitle_language: None, subtitle_mode: 0, ..smart.clone() };
        let u = language_update(&smart, &cleared);
        assert_eq!((u.subtitle_language_preference.as_deref(), u.subtitle_mode), (Some(""), Some("None")));

        for ((mode, forced), jf) in [((2, 2), "OnlyForced"), ((2, 0), "Always"), ((2, 1), "Always"), ((1, 2), "Smart"), ((0, 2), "None")] {
            assert_eq!(jellyfin_subtitle_mode(mode, forced), jf, "{mode}/{forced}");
        }
    }

    #[test]
    fn a_replaced_logo_is_asked_for_by_its_new_tag() {
        let item = |logo: &str| {
            parse(&format!(r#"{{"Name":"Show","Id":"5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a","Type":"Series","ImageTags":{{"Logo":"{logo}"}}}}"#))
        };
        let ctx = Ctx { sections: &SectionIds::default(), section_guid: None };
        let rk = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
        let path = format!("/library/metadata/{rk}/clearLogo");
        metadata(&item("old"), &ctx).unwrap();
        let before = crate::jellyfin::client::plex_image_path(&path).unwrap();
        metadata(&item("new"), &ctx).unwrap();
        let after = crate::jellyfin::client::plex_image_path(&path).unwrap();
        assert!(before.ends_with("&tag=old") && after.ends_with("&tag=new"), "{before} -> {after}");
    }

    /// The four corners as sRGB bytes, to compare against the reference decoder's output.
    fn corner_bytes(m: &Metadata) -> Option<[[u8; 3]; 4]> {
        let c = m.ultra_blur_colors?.corners()?;
        Some(c.map(|p| p.map(|x| (x * 255.0 + 0.5) as u8)))
    }

    #[test]
    fn the_backdrop_blurhash_fills_the_ambient_corners() {
        // woltapp/blurhash's own sample hash; the corners are its reference decoder's pixels at
        // 15 % / 85 % of the frame, in `UltraBlurColors::corners`' ring order (tl, tr, br, bl).
        let e = parse(
            r#"{"Name":"Film","Id":"m1","Type":"Movie","ImageTags":{"Primary":"p1"},
                "BackdropImageTags":["b1"],
                "ImageBlurHashes":{"Backdrop":{"b1":"LEHV6nWB2yk8pyo0adR*.7kCMdnj"},
                                   "Primary":{"p1":"00336699"}}}"#,
        );
        let ids = SectionIds::new();
        let m = metadata(&e, &Ctx { sections: &ids, section_guid: None }).unwrap();
        let want = [[0x8f, 0xa4, 0xae], [0x90, 0xa5, 0xb0], [0x8f, 0x90, 0x90], [0x8d, 0x90, 0x8f]];
        let got = corner_bytes(&m).expect("a backdrop blurhash gives the page its colours");
        for (g, w) in got.iter().zip(want) {
            assert!(g.iter().zip(w).all(|(a, b)| a.abs_diff(b) <= 1), "{got:x?} vs {want:x?}");
        }
    }

    #[test]
    fn an_item_without_a_backdrop_hash_takes_its_series_or_primary_one() {
        // An episode's backdrop is its series': the hash rides under the inherited tag.
        let e = parse(
            r#"{"Name":"Pilot","Id":"e1","Type":"Episode","SeriesId":"s1",
                "ParentBackdropItemId":"s1","ParentBackdropImageTags":["pb"],
                "ImageBlurHashes":{"Backdrop":{"pb":"LEHV6nWB2yk8pyo0adR*.7kCMdnj"}}}"#,
        );
        let ids = SectionIds::new();
        let m = metadata(&e, &Ctx { sections: &ids, section_guid: None }).unwrap();
        assert_eq!(corner_bytes(&m).map(|c| c[0]), Some([0x8f, 0xa4, 0xae]));

        // No backdrop at all: the poster's hash. A 1x1 grid (size digit 0, no AC terms) is its DC
        // colour everywhere; the four DC digits are #336699 in base 83.
        let flat = {
            const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";
            let mut v = 0x33_66_99u32;
            let mut d = [0u8; 4];
            for x in d.iter_mut().rev() {
                *x = A[(v % 83) as usize];
                v /= 83;
            }
            format!("00{}", std::str::from_utf8(&d).unwrap())
        };
        let e = parse(&format!(
            r#"{{"Name":"Film","Id":"m2","Type":"Movie","ImageTags":{{"Primary":"p1"}},
                "ImageBlurHashes":{{"Primary":{{"p1":"{flat}"}}}}}}"#
        ));
        let m = metadata(&e, &Ctx { sections: &ids, section_guid: None }).unwrap();
        assert_eq!(corner_bytes(&m), Some([[0x33, 0x66, 0x99]; 4]));
    }

    #[test]
    fn a_malformed_or_missing_blurhash_leaves_the_colours_unset() {
        let ids = SectionIds::new();
        for hashes in [
            "",
            r#","ImageBlurHashes":{"Backdrop":{"b1":"LEHV6nWB2yk8pyo0adR*.7kCMdn"}}"#,
            r#","ImageBlurHashes":{"Backdrop":{"b1":"zz"}}"#,
            r#","ImageBlurHashes":{"Backdrop":{"b1":"éééééé"}}"#,
            r#","ImageBlurHashes":null"#,
        ] {
            let e = parse(&format!(r#"{{"Name":"Film","Id":"m1","Type":"Movie","BackdropImageTags":["b1"]{hashes}}}"#));
            let m = metadata(&e, &Ctx { sections: &ids, section_guid: None }).unwrap();
            assert!(m.ultra_blur_colors.is_none(), "{hashes}");
        }
    }
}
