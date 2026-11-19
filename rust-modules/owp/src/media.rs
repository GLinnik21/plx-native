use crate::json::{Json, Text};
use crate::text::{equal_ignoring_ascii_case, plain};

pub const MAX_DURATION_MS: i64 = 604_800_000;
pub const MAX_MEDIA_BYTES: usize = 4096;
pub const MAX_EXT_BYTES: usize = 1024;
pub const NAMESPACES: [&str; 8] = ["imdb", "tmdb", "tvdb", "plex", "anilist", "anidb", "kitsu", "mal"];
const MEMBERS: [&str; 10] = ["kind", "title", "year", "ids", "series", "season", "episode", "durationMs", "file", "ext"];
const SERIES_MEMBERS: [&str; 3] = ["title", "year", "ids"];
const FILE_MEMBERS: [&str; 3] = ["sizeBytes", "oshash", "name"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRule {
    Object,
    Kind,
    Title,
    Year,
    Ids,
    Series,
    SeriesTitle,
    SeriesYear,
    SeriesIds,
    Season,
    Episode,
    DurationMs,
    File,
    FileSizeBytes,
    FileOshash,
    FileName,
    Ext,
    RepeatedName,
    ExtNumber,
    ExtSize,
    Size,
}

fn optional(object: &Json, name: &str, rule: impl Fn(&Json) -> bool) -> bool {
    object.get(name).is_none_or(rule)
}

fn int_at_least(min: i64) -> impl Fn(&Json) -> bool {
    move |value| value.as_int().is_some_and(|i| i >= min)
}

fn any_int(value: &Json) -> bool {
    value.as_int().is_some()
}

fn titled(value: &Json) -> bool {
    value.as_text().is_some_and(|text| plain(text, usize::MAX))
}

fn valid_ids(value: &Json) -> bool {
    value.members().is_some_and(|members| {
        members.iter().all(|(name, id)| !name.unpaired && id.as_text().is_some_and(|text| !text.unpaired && !text.value.is_empty()))
    })
}

fn valid_ext_name(name: &Text) -> bool {
    let mut labels = 0;
    let all_valid = name.value.split('.').all(|label| {
        labels += 1;
        !label.is_empty() && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    !name.unpaired && all_valid && labels >= 2
}

fn valid_file_name(value: &Json) -> bool {
    value.as_text().is_some_and(|text| plain(text, usize::MAX) && !text.value.contains(['/', '\\']) && text.value != "." && text.value != "..")
}

fn valid_oshash(value: &Json) -> bool {
    value.as_text().is_some_and(|text| text.value.len() == 16 && text.value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn require(holds: bool, rule: MediaRule) -> Result<(), MediaRule> {
    if holds { Ok(()) } else { Err(rule) }
}

fn check_series(series: &Json) -> Result<(), MediaRule> {
    require(series.members().is_some(), MediaRule::Series)?;
    require(series.get("title").is_some_and(titled), MediaRule::SeriesTitle)?;
    require(optional(series, "year", any_int), MediaRule::SeriesYear)?;
    require(optional(series, "ids", valid_ids), MediaRule::SeriesIds)
}

fn check_file(file: &Json) -> Result<(), MediaRule> {
    require(file.members().is_some(), MediaRule::File)?;
    require(optional(file, "sizeBytes", int_at_least(0)), MediaRule::FileSizeBytes)?;
    require(optional(file, "oshash", valid_oshash), MediaRule::FileOshash)?;
    require(optional(file, "name", valid_file_name), MediaRule::FileName)
}

fn valid_ext(ext: &Json) -> bool {
    ext.members().is_some_and(|members| members.iter().all(|(name, _)| valid_ext_name(name))) && !ext.holds_unpaired()
}

fn kind(media: &Json) -> Option<&str> {
    media.get("kind").and_then(Json::as_text).map(|text| text.value.as_str())
}

fn check_shape(media: &Json) -> Result<(), MediaRule> {
    let kind = kind(media);
    require(media.members().is_some(), MediaRule::Object)?;
    require(matches!(kind, Some("movie" | "episode" | "video")), MediaRule::Kind)?;
    require(media.get("title").is_some_and(titled), MediaRule::Title)?;
    require(optional(media, "year", any_int), MediaRule::Year)?;
    require(optional(media, "ids", valid_ids), MediaRule::Ids)?;
    require(kind != Some("episode") || media.get("series").is_some(), MediaRule::Series)?;
    media.get("series").map_or(Ok(()), check_series)?;
    require(optional(media, "season", int_at_least(0)), MediaRule::Season)?;
    require(optional(media, "episode", int_at_least(0)), MediaRule::Episode)?;
    require(media.get("durationMs").and_then(Json::as_int).is_some_and(|ms| (1..=MAX_DURATION_MS).contains(&ms)), MediaRule::DurationMs)?;
    media.get("file").map_or(Ok(()), check_file)?;
    require(optional(media, "ext", valid_ext), MediaRule::Ext)
}

fn only(value: &Json, names: &[&str]) -> Json {
    let members = value.members().unwrap_or_default();
    Json::Object(
        names
            .iter()
            .filter_map(|name| members.iter().find(|(key, _)| key.value == *name))
            .map(|(key, member)| {
                let member = match key.value.as_str() {
                    "series" => only(member, &SERIES_MEMBERS),
                    "file" => only(member, &FILE_MEMBERS),
                    _ => member.clone(),
                };
                (key.clone(), member)
            })
            .collect(),
    )
}

pub fn known_members(media: &Json) -> Json {
    only(media, &MEMBERS)
}

pub fn check(media: &Json, direction: Direction) -> Result<(), MediaRule> {
    check_shape(media)?;
    match direction {
        Direction::Receive => Ok(()),
        Direction::Send => {
            require(!media.repeats_a_member_name(), MediaRule::RepeatedName)?;
            require(media.get("ext").is_none_or(|ext| !ext.holds_a_non_finite_number()), MediaRule::ExtNumber)?;
            require(media.get("ext").is_none_or(|ext| ext.compact().len() <= MAX_EXT_BYTES), MediaRule::ExtSize)?;
            require(known_members(media).compact().len() <= MAX_MEDIA_BYTES, MediaRule::Size)
        }
    }
}

fn id_in<'a>(ids: Option<&'a Json>, namespace: &str) -> Option<&'a str> {
    ids?.get(namespace).and_then(Json::as_text).map(|text| text.value.as_str()).filter(|id| !id.is_empty())
}

fn share_an_id(a: Option<&Json>, b: Option<&Json>) -> bool {
    NAMESPACES.iter().any(|ns| matches!((id_in(a, ns), id_in(b, ns)), (Some(x), Some(y)) if x == y))
}

fn differ_on_an_id(a: Option<&Json>, b: Option<&Json>) -> bool {
    NAMESPACES.iter().any(|ns| matches!((id_in(a, ns), id_in(b, ns)), (Some(x), Some(y)) if x != y))
}

fn equal_ints(a: Option<&Json>, b: Option<&Json>) -> bool {
    matches!((a.and_then(Json::as_int), b.and_then(Json::as_int)), (Some(x), Some(y)) if x == y)
}

fn equal_names(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a.and_then(Json::as_text), b.and_then(Json::as_text)) {
        (Some(x), Some(y)) => equal_ignoring_ascii_case(&x.value, &y.value),
        _ => false,
    }
}

fn same_episode(a: &Json, b: &Json) -> bool {
    if share_an_id(a.get("ids"), b.get("ids")) {
        return true;
    }
    if differ_on_an_id(a.get("ids"), b.get("ids")) {
        return false;
    }
    let (Some(series_a), Some(series_b)) = (a.get("series"), b.get("series")) else { return false };
    share_an_id(series_a.get("ids"), series_b.get("ids")) && equal_ints(a.get("season"), b.get("season")) && equal_ints(a.get("episode"), b.get("episode"))
}

fn same_video(a: &Json, b: &Json) -> bool {
    let (Some(file_a), Some(file_b)) = (a.get("file"), b.get("file")) else { return false };
    equal_names(file_a.get("oshash"), file_b.get("oshash"))
        || (equal_ints(file_a.get("sizeBytes"), file_b.get("sizeBytes")) && equal_names(file_a.get("name"), file_b.get("name")))
}

pub fn same_title(a: &Json, b: &Json) -> bool {
    if kind(a) != kind(b) {
        return false;
    }
    match kind(a) {
        Some("movie") => share_an_id(a.get("ids"), b.get("ids")),
        Some("episode") => same_episode(a, b),
        Some("video") => same_video(a, b),
        _ => false,
    }
}
