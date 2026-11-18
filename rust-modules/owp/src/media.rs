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

fn valid_series(series: &Json) -> bool {
    series.members().is_some() && series.get("title").is_some_and(titled) && optional(series, "year", any_int) && optional(series, "ids", valid_ids)
}

fn valid_file(file: &Json) -> bool {
    file.members().is_some()
        && optional(file, "sizeBytes", int_at_least(0))
        && optional(file, "oshash", valid_oshash)
        && optional(file, "name", valid_file_name)
}

fn valid_ext(ext: &Json) -> bool {
    ext.members().is_some_and(|members| members.iter().all(|(name, _)| valid_ext_name(name))) && !ext.holds_unpaired()
}

fn kind(media: &Json) -> Option<&str> {
    media.get("kind").and_then(Json::as_text).map(|text| text.value.as_str())
}

fn valid_shape(media: &Json) -> bool {
    let kind = kind(media);
    media.members().is_some()
        && matches!(kind, Some("movie" | "episode" | "video"))
        && media.get("title").is_some_and(titled)
        && optional(media, "year", any_int)
        && optional(media, "ids", valid_ids)
        && (kind != Some("episode") || media.get("series").is_some())
        && optional(media, "series", valid_series)
        && optional(media, "season", int_at_least(0))
        && optional(media, "episode", int_at_least(0))
        && media.get("durationMs").and_then(Json::as_int).is_some_and(|ms| (1..=MAX_DURATION_MS).contains(&ms))
        && optional(media, "file", valid_file)
        && optional(media, "ext", valid_ext)
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

pub fn check(media: &Json, direction: Direction) -> bool {
    if !valid_shape(media) {
        return false;
    }
    match direction {
        Direction::Receive => true,
        Direction::Send => {
            let ext_fits = media.get("ext").is_none_or(|ext| ext.compact().len() <= MAX_EXT_BYTES);
            known_members(media).compact().len() <= MAX_MEDIA_BYTES && ext_fits
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
