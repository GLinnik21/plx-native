//! **The sort value of a movie library grid** (Settings > Libraries > Under posters > Sort value):
//! while the grid is sorted by something other than Title or Random, every poster shows the one
//! fact it is sorted by on one line under it ([`label`], drawn by `plx_ui::card_row::draw_sort_value`).
//! The facts come from the listing the grid already holds (`PmsMovie` and its `SortInfo`); no request
//! is added. A sort the table does not know shows no value ([`shows_value`]), so the grid keeps the
//! Title/Details captions rather than inventing a fact; an item with no value for a known sort shows
//! an en dash, and a fact is never guessed (a missing year-less release date is a dash, not the year).
//!
//! The formats: ratings as a percentage beside the provider's mark (`captions::rating_mark`, the one
//! table), `2h 14m`, `4K` (+1 for a second version), `62 Mbps`, plays as a number, `TV-MA`, `2019`; and
//! dates as **Today**, **Yesterday**, `27 Oct` (this year) or `Oct 2024` (any other year), in the
//! interface language's own month names, never numeric. Every space is a no-break space, so a number
//! never parts from its unit. "Today" is an INPUT: a screen reads no clock, the application hands the
//! calendar date in (`LibraryLike::today`), and `CaptionCache` keys its labels on it, so a label
//! rebuilds when the day turns. Unix timestamps are converted to the OS time zone here, in the
//! presentation layer; the stores keep the server's seconds.
use plx_data::pms::PmsMovie;
use plx_platform::i18n::{current, msg, MonthWidth};
use plx_ui::{cards::{SortValue, TileLabel}, fmt};

/// The en dash a poster shows when it has no value for the sort.
const MISSING: &str = "\u{2013}";

/// The sort term a server declares reduced to the name the table below knows: `SortEntry.key` may
/// be scoped or compound (`movie.addedAt:desc`), and direction never changes the fact described.
fn term(key: &str) -> &str {
    let term = key.split(',').next().unwrap_or(key).split(':').next().unwrap_or(key);
    term.rsplit('.').next().unwrap_or(term)
}

/// Whether a grid sorted by `key` shows a value under every poster: every sort the table below
/// words, which leaves out Title, Random and any sort a later server declares.
pub(crate) fn shows_value(key: &str) -> bool {
    matches!(term(key), "originallyAvailableAt" | "rating" | "audienceRating" | "userRating" | "contentRating"
        | "duration" | "viewOffset" | "viewCount" | "addedAt" | "lastViewedAt" | "mediaHeight" | "mediaBitrate" | "year")
}

/// The label a movie grid draws under `item` while it is sorted by `key` and `today` is the
/// calendar date the application last read (`None`: it has none, and a date reads as month and year).
pub(crate) fn label(item: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> TileLabel {
    TileLabel { value: Some(value(item, key, today)), ..TileLabel::default() }
}

fn value(item: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> SortValue {
    let facts = item.sort_info.as_ref();
    let date = |stamp: Option<i64>| stamp.and_then(plx_platform::i18n::local_calendar_date).and_then(|date| date_value(date, today));
    let mut mark = &[][..];
    let mut plus = false;
    let text = if item.kind != 0 { None } else { match term(key) {
        "year" => (item.year > 0).then(|| item.year.to_string()),
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
                        if art == RatingArt::Imdb { format!("IMDb {}{}", fmt::rating_score(fmt::RatingScale::OutOfTen, value), fmt::rating_suffix(fmt::RatingScale::OutOfTen)) }
                        else if art == RatingArt::Tmdb { format!("TMDB {}", fmt::rating_score(fmt::RatingScale::Percent, value)) }
                        else { fmt::rating_score(fmt::RatingScale::Percent, value) }
                    }
                    None => format!("{}{}", fmt::rating_score(fmt::RatingScale::OutOfTen, value), fmt::rating_suffix(fmt::RatingScale::OutOfTen)),
                }
            })
        }
        "userRating" => facts.and_then(|f| f.user_rating).map(|v| format!("{}{}", fmt::rating_score(fmt::RatingScale::OutOfTen, v), fmt::rating_suffix(fmt::RatingScale::OutOfTen))),
        "contentRating" => (!item.rating.is_empty()).then(|| item.rating.clone()),
        "duration" => (item.dur_ns > 0).then(|| fmt::duration(item.dur_ns / 1_000_000)),
        "viewOffset" => {
            if let Some(value) = (item.resume_ms > 0 && item.resume_ms < item.dur_ns / 1_000_000).then(|| item.resume_frac()).flatten() { Some(fmt::percent((value * 100.0).round().clamp(0.0, 99.0) as i64)) }
            else if item.watched { Some(fmt::percent(100)) }
            else if item.dur_ns > 0 && item.resume_ms == 0 && item.unwatched { Some(fmt::percent(0)) }
            else { None }
        }
        "viewCount" => facts.and_then(|f| f.view_count).map(|n| current().number(n)),
        "addedAt" => date(item.added_at),
        "lastViewedAt" => date(facts.and_then(|f| f.last_viewed_at)),
        "mediaHeight" => {
            let width = facts.map_or(0, |f| f.width);
            let height = facts.map_or(0, |f| f.height);
            plus = facts.is_some_and(|f| f.media_count > 1);
            fmt::resolution(item.video_resolution.as_deref().unwrap_or(""), width, height)
        }
        "mediaBitrate" => {
            plus = facts.is_some_and(|f| f.media_count > 1);
            facts.and_then(|f| f.bitrate_kbps).map(fmt::bitrate)
        }
        _ => None,
    } };
    match text.filter(|text| !text.is_empty()) {
        Some(text) => SortValue { text: text.replace(' ', "\u{a0}"), plus, mark, missing: false },
        None => SortValue { text: MISSING.into(), plus: false, mark: &[], missing: true },
    }
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
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

    const TODAY: [i32; 3] = [2026, 10, 30];
    const ALL_KEYS: [&str; 13] = ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating",
        "duration", "viewOffset", "viewCount", "addedAt", "lastViewedAt", "mediaHeight", "mediaBitrate", "year"];
    const TEN: [Preference; 10] = [Preference::En, Preference::Be, Preference::De, Preference::Es, Preference::Fr,
        Preference::Hu, Preference::It, Preference::Pl, Preference::Pt, Preference::Ru];

    fn movie() -> PmsMovie {
        PmsMovie { title: "Film".into(), year: 1997, aired: "1997-12-12".into(),
            rating: "PG-13".into(), audience_rating: Some(8.5), added_at: Some(1_793_275_200),
            dur_ns: 134 * 60_000_000_000, video_resolution: Some("4k".into()), unwatched: true,
            sort_info: Some(SortInfo { rating: Some(10.0), rating_image: "rottentomatoes://image.rating.ripe".into(),
                audience_image: "rottentomatoes://image.rating.spilled".into(), user_rating: Some(8.0),
                view_count: Some(3), last_viewed_at: Some(1_793_188_800),
                bitrate_kbps: Some(8000), media_count: 1, ..Default::default() }), ..Default::default() }
    }
    fn read(m: &PmsMovie, key: &str, today: Option<[i32; 3]>) -> SortValue { value(m, key, today) }
    fn text(m: &PmsMovie, key: &str) -> String { read(m, key, Some(TODAY)).text }

    #[test]
    fn every_declared_sort_words_its_fact_and_the_others_show_no_value() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        for key in ALL_KEYS { assert!(shows_value(key), "{key}"); assert!(shows_value(&format!("movie.{key}:desc")), "{key} scoped"); }
        for key in ["titleSort", "title", "random", "futureServerSort", ""] { assert!(!shows_value(key), "{key:?}"); }
        let m = movie();
        for (key, expected) in [("originallyAvailableAt", "Dec\u{a0}1997"), ("rating", "100%"), ("audienceRating", "85%"),
            ("userRating", "8.0/10"), ("contentRating", "PG-13"), ("duration", "2h\u{a0}14m"), ("viewOffset", "0%"),
            ("viewCount", "3"), ("addedAt", "Yesterday"), ("lastViewedAt", "28\u{a0}Oct"), ("mediaHeight", "4K"),
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
        // A never-viewed film under Date viewed, and a release date that is only a year.
        let mut m = movie();
        m.sort_info.as_mut().unwrap().last_viewed_at = None;
        m.aired = "1997".into();
        assert!(read(&m, "lastViewedAt", Some(TODAY)).missing && read(&m, "originallyAvailableAt", Some(TODAY)).missing);
        assert!(read(&m, "duration", Some(TODAY)).text.contains("2h"), "other facts are untouched");
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
        for preference in TEN {
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
            info.view_count = Some(1_234_567);
            info.user_rating = Some(10.0);
            info.rating_image = "imdb://image.rating".into();
        }
        for preference in TEN {
            let _locale = language_on_this_thread_for_test(preference);
            let mut samples: Vec<SortValue> = ALL_KEYS.iter().map(|key| read(&m, key, Some(TODAY))).collect();
            samples.push(SortValue { text: msg::under_posters_value_yesterday().to_owned(), ..Default::default() });
            for month in 1..=12 {
                for date in [[2026, month, 28], [2025, month, 28]] {
                    samples.push(SortValue { text: date_value(date, Some(TODAY)).unwrap().replace(' ', "\u{a0}"), ..Default::default() });
                }
            }
            for value in samples {
                let full = if value.plus { format!("{}{}", value.text, plx_ui::cards::SORT_VALUE_PLUS) } else { value.text.clone() };
                let width = ShippedMeasure.width_str(&full, plx_ui::theme::size::CAPTION, true);
                assert!(width + 52.0 <= 250.0, "{preference:?}: {full:?} is {width}px with a mark's hang");
            }
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
