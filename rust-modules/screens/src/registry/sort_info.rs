//! **The sort value of a Library grid** (Settings > Libraries > Under posters > Sort value): while a
//! movie or TV show library's grid (its movies, shows, seasons or episodes) is sorted by something
//! other than Title or Random, every card shows the one fact it is sorted by on one line under it
//! ([`label`], drawn by `plx_ui::card_row::draw_sort_value`).
//! The facts come from the listing the grid already holds (`PmsMovie` and its `SortInfo`, which a show,
//! season or episode row carries as a movie row does); no request is added. **Which sorts show a value
//! depends on the kind of row** ([`shows_value`]): a sort the table does not word for that kind (Title,
//! Random, the compound Show order Seasons and Episodes default to, a sort a later server declares, a
//! sort whose fact a kind of row cannot carry, such as an episode's Unplayed, a season's Resolution or a show's Duration)
//! shows NO line at all, and the grid keeps its Title/Details captions and its 40 px rows rather than
//! drawing a column of dashes. An item with no value for a sort that is worded for its kind shows an
//! en dash (data the server did not send), and a fact is never guessed (a missing year-less
//! release date is a dash, not the year).
//!
//! **TV shows.** A show's own facts feed the sorts a show library declares: Release date, the two scores,
//! Date viewed and Plays from the show row (a show has no length, progress or media of its own, so
//! Duration, Progress, Resolution and Bitrate are not worded for it and show no line), and **Unplayed** (`unviewedLeafCount`) from its episode
//! counts, worded as the catalogue's plural "9 episodes" / "1 episode" (`browse.detail.episodes`, the
//! language's own plural categories). A show with every episode watched has no line at all (the watched
//! tick says it) while its slot keeps the grid's row pitch; a show whose counts the server did not send
//! reads as the dash. **Last episode added (`episode.addedAt`) shows no value**: the listing carries no
//! date of a show's latest episode, and the show's own `addedAt`/`updatedAt` are other facts (measured:
//! neither follows that order), so the sort is not worded and the grid keeps its captions (`term` keeps
//! the `episode.` scope instead of stripping it down to the show's own `addedAt`).
//!
//! **Episodes and seasons.** An episode's row carries the facts a movie's does (its own air date, scores
//! and their providers, length, play state, dates and media), so an episode is worded for every term a
//! movie is, from ITS row: `originallyAvailableAt` is the air date, never the show's. A season's row
//! carries only the container facts (dates added and viewed, plays, the episode counts), so a season is
//! worded for those four. PMS lists, for a TV library (recorded 2026-10-31 on 1.43, `docs/pms-api.md` "Sort[]"),
//! Seasons: the compound Show order and Random; Episodes: the compound Show order, Release date,
//! Critic and Audience rating, Date added, Date viewed and Random. The Show order is the default and,
//! like Title and Random, shows no value; the others are worded. `episode.` and `season.` are the
//! item's OWN scope there (dropped, like `movie.`), while `show.` names the series (`show.titleSort`
//! leads the compound order) and is another fact, so it reads as a term the table does not word.
//!
//! The formats: ratings as a percentage beside the provider's mark (`captions::rating_mark`, the one
//! table), `2h 14m`, `4K` (+1 for a second version), `62.0 Mbps`, plays with their noun (`14 plays`, `1 play`),
//! `TV-MA`, `2019`; an item nobody played reads **Never** under Date viewed and Plays; and
//! dates as **Today**, **Yesterday**, `27 Oct` (this year) or `Oct 2024` (any other year), in the
//! interface language's own month names, never numeric. Every space is a no-break space, so a number
//! never parts from its unit. "Today" is an INPUT: a screen reads no clock, the application hands the
//! calendar date in (`LibraryLike::today`), and `CaptionCache` keys its labels on it, so a label
//! rebuilds when the day turns. Unix timestamps are converted to the OS time zone here, in the
//! presentation layer; the stores keep the server's seconds.
use std::os::raw::c_int;

use plx_data::pms::{PmsMovie, KIND_EPISODE, KIND_MOVIE, KIND_SEASON, KIND_SHOW};
use plx_platform::i18n::{current, msg, MonthWidth};
use plx_ui::{cards::{SortValue, TileLabel}, fmt};

/// The en dash a poster shows when it has no value for the sort.
const MISSING: &str = "\u{2013}";

/// The sort term a server declares reduced to the name the table below knows: `SortEntry.key` may
/// be scoped or compound (`movie.addedAt:desc`), and direction never changes the fact described. Only
/// a scope naming the listed item's OWN kind is dropped (`movie.`, `show.` on a movie or show row,
/// `season.` on a season, `episode.` on an episode): `episode.addedAt` on a SHOW listing is the date of
/// the show's latest episode, another fact than the show's `addedAt`, so it keeps its scope and reads as
/// a term the table does not word; so does `show.titleSort`, the series name the compound order of a
/// season or an episode listing starts with.
fn term(kind: c_int, key: &str) -> &str {
    let first = key.split(',').next().unwrap_or(key).split(':').next().unwrap_or(key);
    match first.split_once('.') {
        Some((scope, rest)) if own_scope(kind, scope) => rest,
        _ => first,
    }
}

/// Whether `scope` is how a PMS sort key spells the kind of item listed (`kind`: a `PmsMovie::kind`).
fn own_scope(kind: c_int, scope: &str) -> bool {
    match kind {
        KIND_MOVIE | KIND_SHOW => matches!(scope, "movie" | "show"),
        KIND_SEASON => scope == "season",
        KIND_EPISODE => scope == "episode",
        _ => false,
    }
}

/// The sort terms worded for a MOVIE row: every fact its listing carries.
const MOVIE_TERMS: [&str; 13] = ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating",
    "duration", "viewOffset", "viewCount", "addedAt", "lastViewedAt", "mediaHeight", "mediaBitrate", "year"];
/// A SHOW row's: the movie terms its listing row carries (its scores, content rating, plays, dates and
/// year; one show lacking a fact reads as the dash) and the episodes left. Not a length, progress or
/// media: those are one episode's or none (a show row's `duration` is an episode's), so those sorts
/// show no line for a show.
const SHOW_TERMS: [&str; 10] = ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating",
    "viewCount", "addedAt", "lastViewedAt", "year", "unviewedLeafCount"];
/// An EPISODE row's: the movie terms, from the episode's own facts (its air date, its length, its media).
const EPISODE_TERMS: [&str; 13] = MOVIE_TERMS;
/// A SEASON row's: the container facts its listing carries. It has no score, length or media of its
/// own, and no air date of its own either, so none of those sorts is worded for it.
const SEASON_TERMS: [&str; 4] = ["addedAt", "lastViewedAt", "viewCount", "unviewedLeafCount"];

/// The terms worded for a row of `kind` (a `PmsMovie::kind`): none for a collection.
fn terms_of(kind: c_int) -> &'static [&'static str] {
    match kind {
        KIND_MOVIE => &MOVIE_TERMS,
        KIND_SHOW => &SHOW_TERMS,
        KIND_SEASON => &SEASON_TERMS,
        KIND_EPISODE => &EPISODE_TERMS,
        _ => &[],
    }
}

/// Whether a grid of `kind` rows (a `PmsMovie::kind`) sorted by `key` shows a value under every card:
/// every sort the table words for that kind. It leaves out Title, Random, the compound Show order,
/// Last episode added, any sort a later server declares and any sort whose fact a row of that kind
/// does not carry; for those the grid keeps its captions.
pub(crate) fn shows_value(kind: c_int, key: &str) -> bool {
    terms_of(kind).contains(&term(kind, key))
}

/// The label a library grid draws under `item` while it is sorted by `key` and `today` is the
/// calendar date the application last read (`None`: it has none, and a date reads as month and year).
/// A show with nothing left to play has a label with no value line (the grid keeps its slot), and so
/// does any item whose kind the sort is not worded for ([`shows_value`]): never a dash for a fact the
/// kind of row cannot carry.
pub(crate) fn label(item: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> TileLabel {
    let value = if shows_value(item.kind, key) { value(item, key, today) } else { None };
    TileLabel { value, ..TileLabel::default() }
}

/// "Never", the quiet word for an item nobody played (Date viewed, Plays): drawn like the dash, at 50%
/// white and never bold, because an empty value stays quiet.
fn never() -> SortValue {
    SortValue { text: msg::under_posters_value_never().to_owned(), plus: false, mark: &[], missing: true }
}

/// A release year as a value line: its digits, or the dash for a year the server did not send. The
/// one wording of the Year sort, which the Under posters preview also shows without an item to ask.
pub(crate) fn year_value(year: i32) -> SortValue {
    if year > 0 { SortValue { text: year.to_string(), plus: false, mark: &[], missing: false } }
    else { SortValue { text: MISSING.into(), plus: false, mark: &[], missing: true } }
}

/// The line under `item` for a sort [`shows_value`] words for its kind: the dash for a fact the server
/// did not send, "Never" for something never played, and `None` when the sort's fact says there is
/// nothing to show (a show whose every episode is watched).
fn value(item: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> Option<SortValue> {
    // A show's or a season's own row has no length, progress or media: those are one episode's, or none.
    let container = matches!(item.kind, KIND_SHOW | KIND_SEASON);
    let facts = item.sort_info.as_ref();
    let date = |stamp: Option<i64>| stamp.and_then(plx_platform::i18n::local_calendar_date).and_then(|date| date_value(date, today));
    // Never played: the row carries its sort facts and nothing on it says the item was ever started or
    // watched (a watched row whose count was cleared for the read-back is not this: it reads as the
    // dash until the count lands). Empty values that mean something say it; the dash is for data the
    // server does not have.
    let never_played = facts.is_some() && !item.watched && item.resume_ms == 0
        && item.viewed_leaf_count.unwrap_or(0) == 0;
    let out_of_ten = |v: f64| format!("{}{}", fmt::rating_score(fmt::RatingScale::OutOfTen, v), fmt::rating_suffix(fmt::RatingScale::OutOfTen));
    let mut mark = &[][..];
    let mut plus = false;
    let text = if terms_of(item.kind).is_empty() { None } else { match term(item.kind, key) {
        "year" => return Some(year_value(item.year)),
        "originallyAvailableAt" => {
            // No year fallback: a bare year is not a complete release date.
            let iso = item.aired.split(['T', ' ']).next().unwrap_or("");
            let mut parts = iso.splitn(3, '-').map(str::parse::<i32>);
            match (parts.next(), parts.next(), parts.next()) {
                (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) => date_value([y, m, d], today),
                _ => None,
            }
        }
        key @ ("rating" | "audienceRating") => {
            let value = if key == "rating" { facts.and_then(|f| f.rating) } else { item.audience_rating.map(f64::from) };
            let image = facts.map_or("", |f| if key == "rating" { &f.rating_image } else { &f.audience_image });
            value.filter(|v| v.is_finite() && *v > 0.0 && *v <= 10.0).map(|value| {
                use plx_data::metadata::RatingArt;
                match RatingArt::from_image(image) {
                    Some(art) => {
                        mark = crate::registry::captions::rating_mark(art);
                        if art == RatingArt::Imdb { format!("IMDb {}", out_of_ten(value)) }
                        else if art == RatingArt::Tmdb { format!("TMDB {}", fmt::rating_score(fmt::RatingScale::Percent, value)) }
                        else { fmt::rating_score(fmt::RatingScale::Percent, value) }
                    }
                    None => out_of_ten(value),
                }
            })
        }
        "userRating" => facts.and_then(|f| f.user_rating).map(out_of_ten),
        "contentRating" => (!item.rating.is_empty()).then(|| item.rating.clone()),
        // A show row's duration is one episode's and it has no media of its own: those are not the show's facts.
        "duration" if !container => (item.dur_ns > 0).then(|| fmt::duration(item.dur_ns / 1_000_000)),
        "viewOffset" if !container => {
            if let Some(value) = (item.resume_ms > 0 && item.resume_ms < item.dur_ns / 1_000_000).then(|| item.resume_frac()).flatten() { Some(fmt::percent((value * 100.0).round().clamp(0.0, 99.0) as i64)) }
            else if item.watched { Some(fmt::percent(100)) }
            else if item.dur_ns > 0 && item.resume_ms == 0 && item.unwatched { Some(fmt::percent(0)) }
            else { None }
        }
        // Plays always carry their noun ("14 plays", "1 play"), and an item nobody played says so.
        "viewCount" => match facts.and_then(|f| f.view_count) {
            Some(n) if n > 0 => Some(msg::under_posters_value_plays(n).to_string()),
            _ if never_played => return Some(never()),
            _ => None,
        },
        "addedAt" => date(item.added_at),
        "lastViewedAt" => match facts.and_then(|f| f.last_viewed_at) {
            Some(stamp) => date(Some(stamp)),
            None if never_played => return Some(never()),
            None => None,
        },
        "mediaHeight" if !container => {
            let width = facts.map_or(0, |f| f.width);
            let height = facts.map_or(0, |f| f.height);
            plus = facts.is_some_and(|f| f.media_count > 1);
            fmt::resolution(item.video_resolution.as_deref().unwrap_or(""), width, height)
        }
        "mediaBitrate" if !container => {
            plus = facts.is_some_and(|f| f.media_count > 1);
            facts.and_then(|f| f.bitrate_kbps).map(fmt::bitrate)
        }
        // Episodes left to play: the server's own sort counts them the same way (`leafCount` minus
        // `viewedLeafCount`), and a show the poster marks watched (`viewed >= leaf`) has nothing to
        // say, so it gets no line at all rather than a "0 episodes".
        "unviewedLeafCount" if container => match item.leaf_count.zip(item.viewed_leaf_count) {
            Some((all, seen)) if all.saturating_sub(seen) == 0 => return None,
            Some((all, seen)) => Some(msg::browse_detail_episodes(i64::from(all - seen)).to_string()),
            None => None,
        },
        _ => None,
    } };
    Some(match text.filter(|text| !text.is_empty()) {
        Some(text) => SortValue { text: text.replace(' ', "\u{a0}"), plus, mark, missing: false },
        None => SortValue { text: MISSING.into(), plus: false, mark: &[], missing: true },
    })
}

/// A date as **Today**, **Yesterday**, `27 Oct` (this year) or `Oct 2024` (any other year): the
/// interface language's skeletons (MMMd, yMMM) over its own month names. With no `today` (a host that
/// has no calendar date) every date is month and year, which is never wrong.
fn date_value(date: [i32; 3], today: Option<[i32; 3]>) -> Option<String> {
    let [year, month, day] = date;
    let (month, day) = (u8::try_from(month).ok()?, u8::try_from(day).ok()?);
    if let Some(today) = today {
        match civil_days(today) - civil_days(date) {
            0 => return Some(msg::under_posters_value_today().to_owned()),
            1 => return Some(msg::under_posters_value_yesterday().to_owned()),
            _ => {}
        }
        if year == today[0] { return current().month_day(month, day, MonthWidth::Medium); }
    }
    current().month_year(year, month)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's `days_from_civil`): the one
/// arithmetic "Yesterday" needs, across month and year ends alike.
fn civil_days([year, month, day]: [i32; 3]) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((i64::from(month) + 9) % 12) + 2) / 5 + i64::from(day) - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::pms::SortInfo;
    use plx_platform::i18n::{language_on_this_thread_for_test, Language, Preference, SHIPPED};

    const TODAY: [i32; 3] = [2026, 10, 30];
    const ALL_KEYS: [&str; 13] = ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating",
        "duration", "viewOffset", "viewCount", "addedAt", "lastViewedAt", "mediaHeight", "mediaBitrate", "year"];

    /// What the current thread's language's OWN catalogue file (`locales/<tag>/<file>.json`) says for `key`,
    /// read as text rather than through the compiled catalogue: a plain string, or the object of plural
    /// forms. English wraps its entry (`{"value": .., "description": ..}`); the other languages do not.
    fn catalogue_entry(file: &str, key: &str) -> serde_json::Value {
        let tag = plx_platform::i18n::current().language().tag();
        let path = format!("{}/../../locales/{tag}/{file}.json", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let catalogue: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let entry = catalogue.get(key).unwrap_or_else(|| panic!("{path} has no {key}")).clone();
        match entry.get("value") { Some(value) => value.clone(), None => entry }
    }

    /// The phrase the current language's own catalogue file words `count` with: the form of the count's
    /// plural category (ICU's, the same rules `languages.json` declares), the file's `other` for a
    /// category it does not spell, the count filled in, every space a no-break space.
    fn catalogue_phrase(entry: &serde_json::Value, count: u32) -> String {
        let form = match entry {
            serde_json::Value::String(text) => text,
            forms => {
                let category = format!("{:?}", plx_platform::i18n::current().plural(i64::from(count))).to_lowercase();
                forms.get(&category).or_else(|| forms.get("other")).and_then(|form| form.as_str())
                    .unwrap_or_else(|| panic!("no {category} or other form in {forms}"))
            }
        };
        form.replace("{count}", &count.to_string()).replace(' ', "\u{a0}")
    }

    /// "N <noun>" in every shipped language's plural categories: the line a count draws is the language's
    /// own catalogue phrase for the count's category with a no-break space between number and noun, and
    /// the counts cross the plural boundaries (one / other, and one / few / many by the last digits) so a
    /// form pinned to the wrong category, or one rule for every language, shows. `line` words a count
    /// through the code under test; 0 is not asked (nothing draws at 0).
    fn counts_follow_the_catalogue(file: &str, key: &str, mut line: impl FnMut(u32) -> String) {
        let counts = [1u32, 2, 3, 4, 5, 6, 11, 12, 14, 21, 22, 24, 25, 100, 101, 102, 111];
        for preference in SHIPPED {
            let _locale = language_on_this_thread_for_test(preference);
            let entry = catalogue_entry(file, key);
            let mut categories = std::collections::BTreeSet::new();
            for n in counts {
                let drawn = line(n);
                assert_eq!(drawn, catalogue_phrase(&entry, n), "{preference:?} {n}");
                assert!(drawn.starts_with(&format!("{n}\u{a0}")) && !drawn.contains(' '), "{preference:?} {n}: {drawn:?}");
                categories.insert(format!("{:?}", plx_platform::i18n::current().plural(i64::from(n))));
            }
            // The counts are not all one category, and a language that declares `few` meets it.
            assert!(categories.len() >= 2, "{preference:?}: {categories:?}");
            let language = plx_platform::i18n::current().language();
            let declared = Language::PLURALS.iter().find(|(l, _)| *l == language).expect("declared plurals").1;
            if declared.contains(&"few") { assert!(categories.contains("Few") && categories.contains("Many"), "{preference:?}: {categories:?}"); }
        }
    }

    fn movie() -> PmsMovie {
        PmsMovie { title: "Film".into(), year: 1997, aired: "1997-12-12".into(),
            rating: "PG-13".into(), audience_rating: Some(8.5), added_at: Some(1_793_275_200),
            dur_ns: 134 * 60_000_000_000, video_resolution: Some("4k".into()), unwatched: true,
            sort_info: Some(SortInfo { rating: Some(10.0), rating_image: "rottentomatoes://image.rating.ripe".into(),
                audience_image: "rottentomatoes://image.rating.spilled".into(), user_rating: Some(8.0),
                view_count: Some(3), last_viewed_at: Some(1_793_188_800),
                bitrate_kbps: Some(8000), media_count: 1, ..Default::default() }), ..Default::default() }
    }
    /// A TV show row as a listing sends it: its own facts, its episode counts, no media.
    fn show() -> PmsMovie {
        PmsMovie { kind: KIND_SHOW, title: "Series".into(), year: 2019, aired: "2019-09-17".into(),
            rating: "TV-MA".into(), audience_rating: Some(8.1), added_at: Some(1_793_275_200),
            leaf_count: Some(37), viewed_leaf_count: Some(28),
            sort_info: Some(SortInfo { rating: Some(8.7), rating_image: "rottentomatoes://image.rating.ripe".into(),
                audience_image: "themoviedb://image.rating".into(), view_count: Some(4),
                last_viewed_at: Some(1_793_188_800), ..Default::default() }), ..Default::default() }
    }
    fn read(m: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> SortValue {
        value(m, key, today).expect("this fact draws a line")
    }
    fn text(m: &PmsMovie, key: &str) -> String { read(m, key, Some(TODAY)).text }

    #[test]
    fn a_show_words_the_facts_its_listing_carries_and_the_movie_only_sorts_show_no_line() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let s = show();
        for (key, expected) in [("year", "2019"), ("originallyAvailableAt", "Sep\u{a0}2019"), ("rating", "87%"),
            ("audienceRating", "TMDB\u{a0}81%"), ("contentRating", "TV-MA"), ("viewCount", "4\u{a0}plays"), ("addedAt", "Yesterday"),
            ("lastViewedAt", "28\u{a0}Oct"), ("unviewedLeafCount", "9\u{a0}episodes")] {
            assert!(shows_value(KIND_SHOW, key), "{key}");
            assert_eq!(text(&s, key), expected, "{key}");
            assert_eq!(text(&s, &format!("show.{key}:desc")), expected, "{key} scoped");
            assert!(!read(&s, key, Some(TODAY)).missing, "{key}");
        }
        assert!(!read(&s, "rating", Some(TODAY)).mark.is_empty(), "the critic score wears its provider's mark");
        // A show row parses `userRating` as any row does: no rating on this one is the dash (data the
        // server did not send), a rated one is worded.
        let v = read(&s, "userRating", Some(TODAY));
        assert!(v.missing && v.text == "\u{2013}" && !v.plus && v.mark.is_empty(), "userRating");
        let mut rated = show();
        rated.sort_info.as_mut().expect("a show row carries sort facts").user_rating = Some(9.0);
        assert_eq!(text(&rated, "userRating"), "9.0/10");
        // A show has no length, progress or media of its own (its row's duration is one episode's):
        // those sorts are not worded for it and show NO line, never a column of dashes.
        let mut with_episode_runtime = show();
        with_episode_runtime.dur_ns = 22 * 60_000_000_000;
        for key in ["duration", "viewOffset", "mediaHeight", "mediaBitrate"] {
            assert!(!shows_value(KIND_SHOW, key), "{key}");
            assert!(!shows_value(KIND_SHOW, &format!("show.{key}:desc")), "{key} scoped");
            assert!(label(&s, key, Some(TODAY)).value.is_none(), "{key}");
            assert!(label(&with_episode_runtime, key, Some(TODAY)).value.is_none(), "{key} with an episode's runtime");
        }
    }

    #[test]
    fn last_episode_added_is_not_worded_from_the_shows_own_dates() {
        // `episode.addedAt` is the date of a show's latest episode. The listing carries no such date, and the
        // show's own `addedAt` / `updatedAt` are other facts (measured against PMS 1.43.5: neither follows
        // that order), so the sort reads as one the table does not word and the grid keeps its captions.
        for key in ["episode.addedAt", "episode.addedAt:desc", "episode.addedAt:desc,titleSort"] {
            assert!(!shows_value(KIND_SHOW, key), "{key}");
        }
        assert_ne!(term(KIND_SHOW, "episode.addedAt"), "addedAt");
        // The scopes that name the listed item's own kind still read as its fact.
        for key in ["addedAt", "movie.addedAt:desc", "show.addedAt", "show.addedAt:desc"] { assert!(shows_value(KIND_SHOW, key), "{key}"); }
        for key in ["titleSort", "random", "show.titleSort,index", "show.titleSort:desc,season.index:nullsLast"] {
            assert!(!shows_value(KIND_SHOW, key), "{key}");
        }
        // Were it ever asked for a value, the show's own date must not come back as the answer.
        let s = show();
        assert!(read(&s, "episode.addedAt", Some(TODAY)).missing);
    }

    #[test]
    fn unplayed_counts_the_episodes_left_and_says_nothing_at_zero() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let mut s = show();
        assert_eq!(text(&s, "unviewedLeafCount"), "9\u{a0}episodes");
        s.viewed_leaf_count = Some(36);
        assert_eq!(text(&s, "unviewedLeafCount"), "1\u{a0}episode");
        // Every episode watched (or the server counted more watched than listed): no line, no dash. The
        // label is then empty, and the grid's slot keeps its row pitch (it is the grid's, not the label's).
        for seen in [37, 40] {
            s.viewed_leaf_count = Some(seen);
            assert!(value(&s, "unviewedLeafCount", Some(TODAY)).is_none(), "{seen}");
            let l = label(&s, "unviewedLeafCount", Some(TODAY));
            assert!(l.value.is_none() && l.title.is_none() && l.caption.is_none(), "{seen}");
        }
        // The server sent no counts at all: a fact it does not have is a dash.
        s.leaf_count = None; s.viewed_leaf_count = None;
        assert!(read(&s, "unviewedLeafCount", Some(TODAY)).missing);
        // A movie has no episodes to count.
        assert!(read(&movie(), "unviewedLeafCount", Some(TODAY)).missing);
    }

    /// The plural boundaries of "N episodes" in every shipped language: the category of the count, in the
    /// language's own words (`browse.detail.episodes` in its catalogue file).
    #[test]
    fn unplayed_follows_each_languages_plural_rules() {
        let mut s = show();
        counts_follow_the_catalogue("browse", "browse.detail.episodes", |n| {
            s.leaf_count = Some(n); s.viewed_leaf_count = Some(0);
            text(&s, "unviewedLeafCount")
        });
    }

    #[test]
    fn every_declared_sort_words_its_fact_and_the_others_show_no_value() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        for key in ALL_KEYS { assert!(shows_value(0, key), "{key}"); assert!(shows_value(0, &format!("movie.{key}:desc")), "{key} scoped"); }
        for key in ["titleSort", "title", "random", "futureServerSort", "", "unviewedLeafCount"] { assert!(!shows_value(0, key), "{key:?}"); }
        let m = movie();
        for (key, expected) in [("originallyAvailableAt", "Dec\u{a0}1997"), ("rating", "100%"), ("audienceRating", "85%"),
            ("userRating", "8.0/10"), ("contentRating", "PG-13"), ("duration", "2h\u{a0}14m"), ("viewOffset", "0%"),
            ("viewCount", "3\u{a0}plays"), ("addedAt", "Yesterday"), ("lastViewedAt", "28\u{a0}Oct"), ("mediaHeight", "4K"),
            ("mediaBitrate", "8.0\u{a0}Mbps"), ("year", "1997")] {
            assert_eq!(text(&m, key), expected, "{key}");
            assert_eq!(text(&m, &format!("movie.{key}:desc")), expected, "{key} scoped");
            assert!(!read(&m, key, Some(TODAY)).missing);
        }
        assert!(!read(&m, "rating", Some(TODAY)).mark.is_empty());
        assert_ne!(read(&m, "rating", Some(TODAY)).mark, read(&m, "audienceRating", Some(TODAY)).mark);
        // Every value is one line: no ordinary space survives, so a number never parts from its unit.
        for key in ALL_KEYS { assert!(!text(&m, key).contains(' '), "{key}"); }
        // The label carries the value and nothing else: no title or details line under a value.
        let l = label(&m, "year", Some(TODAY));
        assert!(l.title.is_none() && l.caption.is_none() && l.value.is_some());
    }

    #[test]
    fn a_missing_value_is_an_en_dash_never_a_guess() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let bare = PmsMovie::default();
        let collection = PmsMovie { kind: plx_data::pms::KIND_COLLECTION, ..movie() };
        for key in ALL_KEYS {
            for item in [&bare, &collection] {
                let v = read(item, key, Some(TODAY));
                assert!(v.missing && v.text == "\u{2013}" && v.mark.is_empty() && !v.plus, "{key}");
            }
        }
        // A release date that is only a year, and a critic score the server did not send, are data the
        // server does not have: the dash.
        let mut m = movie();
        m.aired = "1997".into();
        m.sort_info.as_mut().unwrap().rating = None;
        assert!(read(&m, "originallyAvailableAt", Some(TODAY)).missing);
        assert_eq!(text(&m, "rating"), "\u{2013}");
        assert!(read(&m, "duration", Some(TODAY)).text.contains("2h"), "other facts are untouched");
    }

    /// An empty value says what it means: "Never" under Date viewed and Plays for something never played
    /// (50% white like the dash, never bold), movies and shows alike; the dash stays for data the server
    /// does not have.
    #[test]
    fn date_viewed_and_plays_say_never_for_something_nobody_played() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let quiet = |v: &SortValue| v.missing && v.text == "Never" && v.mark.is_empty() && !v.plus;
        for mut item in [movie(), show()] {
            let facts = item.sort_info.as_mut().unwrap();
            facts.last_viewed_at = None;
            facts.view_count = Some(0);
            item.watched = false;
            item.viewed_leaf_count = Some(0);
            for key in ["lastViewedAt", "viewCount", "show.lastViewedAt:desc", "movie.viewCount:desc"] {
                assert!(quiet(&read(&item, key, Some(TODAY))), "{:?} {key}", item.kind);
            }
            // A count the row does not carry at all reads the same.
            item.sort_info.as_mut().unwrap().view_count = None;
            assert!(quiet(&read(&item, "viewCount", Some(TODAY))));
            // Something started or watched is not "never": its count was cleared for the read-back, so the
            // dash waits for the number rather than saying a false word.
            let mut seen = item.clone();
            seen.watched = true;
            for key in ["lastViewedAt", "viewCount"] {
                let v = read(&seen, key, Some(TODAY));
                assert!(v.missing && v.text == "\u{2013}", "{:?} {key}", item.kind);
            }
            let mut begun = item.clone();
            begun.resume_ms = 60_000;
            assert_eq!(read(&begun, "viewCount", Some(TODAY)).text, "\u{2013}");
        }
        // One play and a played-through film: the count with its noun, the date as a date.
        let mut m = movie();
        m.sort_info.as_mut().unwrap().view_count = Some(1);
        assert_eq!(text(&m, "viewCount"), "1\u{a0}play");
        m.sort_info.as_mut().unwrap().view_count = Some(14);
        assert_eq!(text(&m, "viewCount"), "14\u{a0}plays");
        assert!(!read(&m, "viewCount", Some(TODAY)).missing);
        // A row with no sort facts at all (not a listing row) is data the server did not send: the dash.
        assert_eq!(text(&PmsMovie::default(), "viewCount"), "\u{2013}");
    }

    /// "N plays" in every shipped language's plural categories (the same rule "N episodes" follows).
    #[test]
    fn plays_follow_each_languages_plural_rules() {
        let mut m = movie();
        counts_follow_the_catalogue("under_posters", "under_posters.value_plays", |n| {
            m.sort_info.as_mut().unwrap().view_count = Some(i64::from(n));
            text(&m, "viewCount")
        });
    }

    /// "Never" is the language's own catalogue word, quiet like the dash, in every shipped language.
    #[test]
    fn never_is_translated_and_quiet_in_every_shipped_language() {
        let en = { let _l = language_on_this_thread_for_test(Preference::En); msg::under_posters_value_never().to_owned() };
        assert_eq!(en, "Never");
        for preference in SHIPPED {
            let _locale = language_on_this_thread_for_test(preference);
            let word = catalogue_entry("under_posters", "under_posters.value_never");
            let word = word.as_str().expect("one word");
            assert_eq!(msg::under_posters_value_never(), word, "{preference:?}");
            if preference != Preference::En { assert_ne!(word, en, "{preference:?} is translated"); }
            let v = never();
            assert!(v.missing && v.text == word && v.mark.is_empty() && !v.plus, "{preference:?}");
        }
    }

    #[test]
    fn progress_rewatch_versions_and_unknown_rating_provider_are_honest() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let mut m = movie();
        m.watched = true; m.unwatched = false;
        assert_eq!(text(&m, "viewOffset"), "100%");
        m.resume_ms = m.dur_ns / 1_000_000 / 2;
        assert_eq!(text(&m, "viewOffset"), "50%");
        m.watched = false; m.resume_ms = i64::MAX;
        // Avoid using resume_frac on a corrupt wire offset (see the caption's range guard).
        assert!(read(&m, "viewOffset", Some(TODAY)).missing);
        assert!(!read(&m, "mediaHeight", Some(TODAY)).plus && !read(&m, "mediaBitrate", Some(TODAY)).plus);
        m.sort_info.as_mut().unwrap().media_count = 2;
        let (height, bitrate) = (read(&m, "mediaHeight", Some(TODAY)), read(&m, "mediaBitrate", Some(TODAY)));
        assert!(height.plus && height.text == "4K" && bitrate.plus, "a second version adds +1, not words");
        m.sort_info.as_mut().unwrap().rating_image = "unknown://score".into();
        assert_eq!(text(&m, "rating"), "10.0/10");
        assert!(read(&m, "rating", Some(TODAY)).mark.is_empty());
        m.sort_info.as_mut().unwrap().rating_image = "imdb://image.rating".into();
        assert_eq!(text(&m, "rating"), "IMDb\u{a0}10.0/10");
    }

    #[test]
    fn dates_are_today_yesterday_this_year_or_month_and_year() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let on = |date: [i32; 3], today: [i32; 3]| date_value(date, Some(today)).unwrap();
        assert_eq!(on([2026, 10, 30], TODAY), "Today");
        assert_eq!(on([2026, 10, 29], TODAY), "Yesterday");
        assert_eq!(on([2026, 10, 28], TODAY), "28 Oct", "this year: day and month");
        assert_eq!(on([2026, 1, 5], TODAY), "5 Jan");
        assert_eq!(on([2025, 12, 31], TODAY), "Dec 2025", "an earlier year: month and year");
        assert_eq!(on([2025, 10, 30], TODAY), "Oct 2025", "a year ago to the day is not this year");
        assert_eq!(on([2026, 11, 2], TODAY), "2 Nov", "a date after today in this year keeps its day");
        assert_eq!(on([2027, 3, 2], TODAY), "Mar 2027");
        // Yesterday across a month end, a leap day and a year end.
        assert_eq!(on([2026, 2, 28], [2026, 3, 1]), "Yesterday");
        assert_eq!(on([2024, 2, 29], [2024, 3, 1]), "Yesterday");
        assert_eq!(on([2024, 2, 28], [2024, 3, 1]), "28 Feb", "two days before March 1st in a leap year");
        assert_eq!(on([2026, 12, 31], [2027, 1, 1]), "Yesterday");
        assert_eq!(on([2026, 12, 30], [2027, 1, 1]), "Dec 2026");
        // With no calendar date every date is month and year, never numeric.
        assert_eq!(date_value([2026, 10, 30], None).unwrap(), "Oct 2026");
        assert!(date_value([2026, 13, 1], Some(TODAY)).is_none());
        // The day turning over rebuilds the same stamp's label: Today, then Yesterday, then a date.
        let m = movie();
        let on_day = |today: [i32; 3]| read(&m, "addedAt", Some(today)).text;
        let day = plx_platform::i18n::local_calendar_date(1_793_275_200).unwrap();
        let next = |d: [i32; 3], by: i32| [d[0], d[1], d[2] + by];
        assert_eq!(on_day(day), "Today");
        assert_eq!(on_day(next(day, 1)), "Yesterday");
        assert_eq!(on_day(next(day, 2)), format!("{}\u{a0}Oct", day[2]));
    }

    #[test]
    fn dates_use_named_months_in_every_shipped_language_and_never_numerals_alone() {
        let en_today = { let _l = language_on_this_thread_for_test(Preference::En); msg::under_posters_value_today().to_owned() };
        for preference in SHIPPED {
            let _locale = language_on_this_thread_for_test(preference);
            let (today, yesterday) = (msg::under_posters_value_today(), msg::under_posters_value_yesterday());
            assert!(!today.is_empty() && today != yesterday, "{preference:?}");
            if preference != Preference::En { assert_ne!(today, en_today, "{preference:?} is translated"); }
            for month in 1..=12 {
                for text in [date_value([2026, month, 12], Some(TODAY)).unwrap(), date_value([2020, month, 12], Some(TODAY)).unwrap()] {
                    assert!(!text.contains('/') && text.chars().any(char::is_alphabetic), "{preference:?} {month}: {text}");
                }
            }
        }
    }

    /// The longest value of every kind fits the 250 px poster in bold CAPTION in every shipped language,
    /// with the mark's hang where it has one: the fit ladder (drop +1, then the mark) is never reached, and
    /// nothing is ever cut with an ellipsis.
    #[test]
    fn every_value_fits_its_poster_in_every_shipped_language() {
        use plx_base::fontcov::advances::ShippedMeasure;
        use plx_machine::machine::Measure;
        let mut m = movie();
        m.dur_ns = (10 * 60 + 59) * 60_000_000_000;
        m.rating = "NC-17".into();
        m.video_resolution = Some("1080".into());
        {
            let info = m.sort_info.as_mut().unwrap();
            info.bitrate_kbps = Some(99_900);
            info.media_count = 2;
            // A three-digit play count (the bound "N episodes" is held to too). Known limit, reported: in
            // Spanish a FOUR-digit count ("9999 reproducciones", 255 px) is 5 px wider than the tile.
            info.view_count = Some(999);
            info.user_rating = Some(10.0);
            info.rating_image = "imdb://image.rating".into();
        }
        let mut too_wide = Vec::new();
        for preference in SHIPPED {
            let _locale = language_on_this_thread_for_test(preference);
            let mut samples: Vec<SortValue> = ALL_KEYS.iter().map(|key| read(&m, key, Some(TODAY))).collect();
            samples.push(SortValue { text: msg::under_posters_value_yesterday().to_owned(), ..Default::default() });
            samples.push(never());
            // The longest "N episodes": every plural category's word, with a three-digit count.
            let mut s = show();
            for n in [1u32, 2, 5, 21, 22, 999] {
                s.leaf_count = Some(n); s.viewed_leaf_count = Some(0);
                samples.push(read(&s, "unviewedLeafCount", Some(TODAY)));
            }
            for month in 1..=12 {
                for date in [[2026, month, 28], [2025, month, 28]] {
                    samples.push(SortValue { text: date_value(date, Some(TODAY)).unwrap().replace(' ', "\u{a0}"), ..Default::default() });
                }
            }
            for value in samples {
                let full = if value.plus { format!("{}{}", value.text, plx_ui::cards::SORT_VALUE_PLUS) } else { value.text.clone() };
                let width = ShippedMeasure.width_str(&full, plx_ui::theme::size::CAPTION, true);
                // The mark hangs left of the number (20 px + 6 px, twice for the centred number), so a value
                // with none has the whole tile: a play count is a number and a noun, no mark.
                let hang = if value.mark.is_empty() { 0.0 } else { 52.0 };
                if width + hang > 250.0 { too_wide.push(format!("{preference:?}: {full:?} is {width}px with a hang of {hang}px")); }
            }
        }
        assert!(too_wide.is_empty(), "{too_wide:#?}");
    }

    /// An episode row as a `type=4` listing sends it: its own air date, scores and providers, length,
    /// dates, play state and media.
    fn episode() -> PmsMovie {
        PmsMovie { kind: KIND_EPISODE, title: "Pilot".into(), show_title: "Series".into(), season_index: 1, ep_index: 22,
            year: 2014, aired: "2014-05-12".into(), rating: "TV-14".into(), audience_rating: Some(8.4),
            added_at: Some(1_793_275_200), dur_ns: 22 * 60_000_000_000, video_resolution: Some("1080".into()), unwatched: true,
            sort_info: Some(SortInfo { rating: Some(7.5), rating_image: "rottentomatoes://image.rating.ripe".into(),
                audience_image: "themoviedb://image.rating".into(), user_rating: Some(9.0), view_count: Some(2),
                last_viewed_at: Some(1_793_188_800), bitrate_kbps: Some(6500), media_count: 1, ..Default::default() }),
            ..Default::default() }
    }
    /// A season row as a `type=3` listing sends it: the container facts, no score, length or media.
    fn season() -> PmsMovie {
        PmsMovie { kind: KIND_SEASON, title: "Season 1".into(), show_title: "Series".into(), season_index: 1,
            added_at: Some(1_793_275_200), leaf_count: Some(22), viewed_leaf_count: Some(13),
            sort_info: Some(SortInfo { view_count: Some(9), last_viewed_at: Some(1_793_188_800), ..Default::default() }),
            ..Default::default() }
    }

    /// The sort keys a real PMS (1.43, recorded in `docs/pms-api.md`, "Sort[]") lists for a TV library's Seasons
    /// and Episodes views, in the form the app holds them (`SortEntry::key`).
    const SHOW_ORDER_SEASONS: &str = "show.titleSort,index";
    const SHOW_ORDER_EPISODES: &str = "show.titleSort,season.index:nullsLast,episode.index:nullsLast,\
        episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id";

    #[test]
    fn an_episode_words_the_movie_terms_from_its_own_row() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let e = episode();
        for (key, expected) in [("originallyAvailableAt", "May\u{a0}2014"), ("rating", "75%"), ("audienceRating", "TMDB\u{a0}84%"),
            ("userRating", "9.0/10"), ("contentRating", "TV-14"), ("duration", "22m"), ("viewOffset", "0%"),
            ("viewCount", "2\u{a0}plays"), ("addedAt", "Yesterday"), ("lastViewedAt", "28\u{a0}Oct"), ("mediaHeight", "1080p"),
            ("mediaBitrate", "6.5\u{a0}Mbps"), ("year", "2014")] {
            assert!(shows_value(KIND_EPISODE, key), "{key}");
            assert_eq!(text(&e, key), expected, "{key}");
            // `episode.` is the listed item's own scope there; direction never changes the fact.
            assert_eq!(text(&e, &format!("episode.{key}:desc")), expected, "{key} scoped");
            assert!(shows_value(KIND_EPISODE, &format!("episode.{key}:desc")), "{key} scoped");
            // `show.` names the series: another fact, so no episode sort words it.
            assert!(!shows_value(KIND_EPISODE, &format!("show.{key}")), "show.{key}");
            assert!(!read(&e, key, Some(TODAY)).missing, "{key}");
        }
        // The scores read as the movie path reads them: the critic score beside its provider's mark, TMDB's
        // audience score as the provider's word (the table gives it no mark).
        assert!(!read(&e, "rating", Some(TODAY)).mark.is_empty());
        assert!(read(&e, "audienceRating", Some(TODAY)).mark.is_empty());
        // The air date is the EPISODE's, never the series': no year fallback, and a date the server did not
        // send is the dash.
        let mut undated = episode();
        undated.aired.clear();
        assert!(read(&undated, "originallyAvailableAt", Some(TODAY)).missing);
        undated.aired = "2014".into();
        assert!(read(&undated, "originallyAvailableAt", Some(TODAY)).missing, "a bare year is not an air date");
        // No audience score on the row: the dash, never another score.
        let mut unscored = episode();
        unscored.audience_rating = None;
        let dash = read(&unscored, "audienceRating", Some(TODAY));
        assert!(dash.missing && dash.text == "\u{2013}" && dash.mark.is_empty());
        // Every value is one line.
        for key in ALL_KEYS { assert!(!text(&e, key).contains(' '), "{key}"); }
        // The label carries the value alone: the air date is not drawn a second time as a caption.
        let l = label(&e, "originallyAvailableAt", Some(TODAY));
        assert!(l.title.is_none() && l.caption.is_none() && l.value.is_some());
    }

    #[test]
    fn a_season_words_its_container_facts_and_no_other_sort() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let s = season();
        for (key, expected) in [("addedAt", "Yesterday"), ("lastViewedAt", "28\u{a0}Oct"), ("viewCount", "9\u{a0}plays"),
            ("unviewedLeafCount", "9\u{a0}episodes")] {
            assert!(shows_value(KIND_SEASON, key), "{key}");
            assert_eq!(text(&s, key), expected, "{key}");
            assert_eq!(text(&s, &format!("season.{key}:desc")), expected, "{key} scoped");
            assert!(!read(&s, key, Some(TODAY)).missing, "{key}");
        }
        // A season has no score, length, media, rating or air date of its own: those sorts show no line.
        for key in ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating", "duration",
            "viewOffset", "mediaHeight", "mediaBitrate", "year"] {
            assert!(!shows_value(KIND_SEASON, key), "{key}");
            assert!(label(&s, key, Some(TODAY)).value.is_none(), "{key} draws no line, not a dash");
        }
        // Every episode watched: no line at all, the slot keeps its pitch (the show's rule).
        let mut done = season();
        done.viewed_leaf_count = Some(22);
        assert!(label(&done, "unviewedLeafCount", Some(TODAY)).value.is_none());
        assert_eq!(done.kind, KIND_SEASON);
    }

    /// A sort whose fact the listing does not carry for a kind of row shows NO line: the sort is not
    /// worded for that kind, so the grid keeps its captions and its 40 px rows, rather than a column of
    /// dashes. This is not the dash: the dash is a sort that IS worded for the kind whose fact one item
    /// lacks. The Show order Seasons and Episodes default to is Title's twin and has no value.
    #[test]
    fn a_sort_the_listing_does_not_carry_shows_no_line_and_not_a_column_of_dashes() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let descending = |key: &str| key.replacen(',', ":desc,", 1);
        for key in ["titleSort", "random", "futureServerSort", "", "show.addedAt", "show.titleSort",
            SHOW_ORDER_SEASONS, SHOW_ORDER_EPISODES, "show.titleSort:desc,index"] {
            assert!(!shows_value(KIND_SEASON, key), "season {key:?}");
            assert!(!shows_value(KIND_EPISODE, key), "episode {key:?}");
            assert!(label(&season(), key, Some(TODAY)).value.is_none(), "season {key:?}");
            assert!(label(&episode(), key, Some(TODAY)).value.is_none(), "episode {key:?}");
        }
        assert!(!shows_value(KIND_EPISODE, &descending(SHOW_ORDER_EPISODES)) && !shows_value(KIND_SEASON, &descending(SHOW_ORDER_SEASONS)));
        // Another kind's scope is another fact: `episode.addedAt` on a season is its latest episode's date,
        // `season.addedAt` on an episode its season's.
        assert!(!shows_value(KIND_SEASON, "episode.addedAt") && !shows_value(KIND_EPISODE, "season.addedAt"));
        assert!(label(&season(), "episode.addedAt", Some(TODAY)).value.is_none());
        assert!(label(&episode(), "season.addedAt", Some(TODAY)).value.is_none());
        // An episode has no episode counts to subtract.
        assert!(!shows_value(KIND_EPISODE, "unviewedLeafCount"));
        assert!(label(&episode(), "unviewedLeafCount", Some(TODAY)).value.is_none());
        // A collection is worded for nothing.
        for key in ALL_KEYS { assert!(!shows_value(plx_data::pms::KIND_COLLECTION, key), "{key}"); }
        assert!(label(&PmsMovie { kind: plx_data::pms::KIND_COLLECTION, ..movie() }, "addedAt", Some(TODAY)).value.is_none());
        // What a real PMS lists for the two views: the Show order and Random show nothing, the rest read.
        for (kind, listed, valued) in [
            (KIND_SEASON, vec![SHOW_ORDER_SEASONS, "random"], vec![]),
            (KIND_EPISODE, vec![SHOW_ORDER_EPISODES, "originallyAvailableAt", "rating", "audienceRating", "addedAt", "lastViewedAt", "random"],
                vec!["originallyAvailableAt", "rating", "audienceRating", "addedAt", "lastViewedAt"]),
        ] {
            let got: Vec<_> = listed.iter().copied().filter(|key| shows_value(kind, key)).collect();
            assert_eq!(got, valued, "kind {kind}");
        }
    }

    /// Never-played episodes and seasons say so under Date viewed and Plays (50% white, never bold); the
    /// dash stays for a fact the server did not send; a played one reads its count and date.
    #[test]
    fn an_episode_and_a_season_nobody_played_say_never() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let quiet = |v: &SortValue| v.missing && v.text == "Never" && v.mark.is_empty() && !v.plus;
        for mut item in [episode(), season()] {
            let facts = item.sort_info.as_mut().unwrap();
            facts.last_viewed_at = None;
            facts.view_count = Some(0);
            item.watched = false;
            item.viewed_leaf_count = Some(0);
            for key in ["lastViewedAt", "viewCount"] {
                assert!(quiet(&read(&item, key, Some(TODAY))), "{} {key}", item.kind);
            }
            // Started or watched is not "never".
            let mut seen = item.clone();
            seen.watched = true;
            for key in ["lastViewedAt", "viewCount"] { assert_eq!(read(&seen, key, Some(TODAY)).text, "\u{2013}", "{} {key}", item.kind); }
        }
        let mut one = episode();
        one.sort_info.as_mut().unwrap().view_count = Some(1);
        assert_eq!(text(&one, "viewCount"), "1\u{a0}play");
    }

    /// The words an episode's and a season's values are made of follow every shipped language: the plural
    /// categories of "N plays" and "N episodes", the day words, the month names and "Never".
    #[test]
    fn episode_and_season_values_are_localised_in_every_shipped_language() {
        let mut e = episode();
        counts_follow_the_catalogue("under_posters", "under_posters.value_plays", |n| {
            e.sort_info.as_mut().unwrap().view_count = Some(i64::from(n));
            text(&e, "viewCount")
        });
        let mut sn = season();
        counts_follow_the_catalogue("browse", "browse.detail.episodes", |n| {
            sn.leaf_count = Some(n); sn.viewed_leaf_count = Some(0);
            text(&sn, "unviewedLeafCount")
        });
        let en = { let _l = language_on_this_thread_for_test(Preference::En); (text(&episode(), "addedAt"), text(&episode(), "lastViewedAt")) };
        assert_eq!((en.0.as_str(), en.1.as_str()), ("Yesterday", "28\u{a0}Oct"));
        for preference in SHIPPED {
            let _locale = language_on_this_thread_for_test(preference);
            let e = episode();
            // The day words are the language's own catalogue words, the air date and the dates are its
            // month names (never numerals alone), and an episode nobody watched says the language's "Never".
            assert_eq!(text(&e, "addedAt"), msg::under_posters_value_yesterday().replace(' ', "\u{a0}"), "{preference:?}");
            let aired = text(&e, "originallyAvailableAt");
            assert!(aired.chars().any(char::is_alphabetic) && !aired.contains('/') && aired.contains("2014"), "{preference:?}: {aired}");
            let mut unplayed = episode();
            unplayed.sort_info.as_mut().unwrap().view_count = Some(0);
            unplayed.sort_info.as_mut().unwrap().last_viewed_at = None;
            unplayed.viewed_leaf_count = Some(0);
            assert_eq!(read(&unplayed, "viewCount", Some(TODAY)).text, msg::under_posters_value_never(), "{preference:?}");
            // Scores read as the movie path reads them: a percentage, the provider's mark for the critic score.
            let score = read(&e, "audienceRating", Some(TODAY));
            assert!(score.text.contains("84"), "{preference:?}: {}", score.text);
            assert!(!read(&e, "rating", Some(TODAY)).mark.is_empty(), "{preference:?}");
        }
    }

    #[test]
    fn verdict_marks_keep_the_shared_provider_mapping() {
        use plx_data::metadata::RatingArt;
        let mark = crate::registry::captions::rating_mark;
        assert_eq!(mark(RatingArt::TomatoRotten)[0].1, plx_ui::theme::RATING_MUTED);
        assert_ne!(mark(RatingArt::PopcornUpright)[0].1, mark(RatingArt::PopcornSpilled)[0].1,
            "audience verdicts retain main's colour distinction");
        let mut m = movie();
        for (key, field, image) in [("rating", true, "rotten"), ("audienceRating", false, "upright"),
            ("audienceRating", false, "spilled")] {
            let wire = format!("rottentomatoes://image.rating.{image}");
            if field { m.sort_info.as_mut().unwrap().rating_image = wire; }
            else { m.sort_info.as_mut().unwrap().audience_image = wire; }
            assert_eq!(read(&m, key, Some(TODAY)).mark, mark(RatingArt::from_image(
                if field { &m.sort_info.as_ref().unwrap().rating_image } else { &m.sort_info.as_ref().unwrap().audience_image }
            ).unwrap()));
        }
        for key in ["originallyAvailableAt", "duration", "addedAt", "lastViewedAt", "year", "viewCount"] {
            assert!(read(&m, key, Some(TODAY)).mark.is_empty(), "{key}");
        }
    }
}

#[cfg(test)]
#[test]
fn review_preserves_main_rating_marks() {
    use plx_data::metadata::RatingArt as A;
    use plx_ui::{icons::Icon, theme};
    let mark = crate::registry::captions::rating_mark;
    assert_eq!(mark(A::TomatoRotten), &[(Icon::TomatoHollow, theme::RATING_MUTED),
        (Icon::TomatoCalyx, theme::RATING_MUTED)]);
    assert_eq!(mark(A::PopcornUpright), &[(Icon::Crowd, theme::RATING_AUDIENCE)]);
    assert_eq!(mark(A::PopcornSpilled), &[(Icon::Crowd, theme::RATING_MUTED)]);
}

#[cfg(test)]
#[test]
fn review_timestamp_dates_follow_timezone() {
    // Each timezone gets a fresh process: libc's timezone state is not changed under other tests.
    if let Ok(expected) = std::env::var("PLX_SORT_DATE_TEST_DAY") {
        let _locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let stamp = 1_793_233_800; // 2026-10-29 00:30 UTC
        let item = PmsMovie { added_at: Some(stamp),
            sort_info: Some(plx_data::pms::SortInfo { last_viewed_at: Some(stamp), ..Default::default() }),
            ..Default::default() };
        for key in ["addedAt", "lastViewedAt"] {
            // "Today" is the calendar date handed in, here Oct 29 in the zone under test.
            let value = label(&item, key, Some([2026, 10, 29])).value.unwrap();
            assert_eq!(value.text, expected);
        }
        let wire = serde_json::to_string(&item).unwrap();
        assert!(!wire.contains("added_date") && !wire.contains("last_viewed_date"));
        assert_eq!(serde_json::from_str::<PmsMovie>(&wire).unwrap().added_at, Some(stamp));
        return;
    }
    for (tz, day) in [("UTC0", "Today"), ("EST5", "Yesterday"), ("JST-9", "Today")] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::sort_info::review_timestamp_dates_follow_timezone", "--nocapture"])
            .env("TZ", tz).env("PLX_SORT_DATE_TEST_DAY", day).status().unwrap();
        assert!(status.success(), "date formatting failed under {tz}");
    }
}
