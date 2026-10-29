//! Movie-grid captions from the listing snapshot, formatted once in CaptionCache.
//! Unix dates are converted in the presentation layer; stores retain the server timestamps.
use plx_data::pms::PmsMovie;
use plx_platform::i18n::{current, msg};
use plx_ui::{cards::TileLabel, fmt};

pub(crate) fn label(item: &PmsMovie, key: &str) -> TileLabel {
    let title = (!item.title.is_empty()).then(|| item.title.clone());
    let year = (item.year > 0).then(|| item.year.to_string());
    let mut label = TileLabel::from_lines([title, year]).within_tile();
    if item.kind == plx_data::pms::KIND_COLLECTION {
        label.caption = std::ffi::CString::new(fmt::collection_count(item.subtype.as_deref(), item.child_count)).ok();
        return label;
    }
    if item.kind != 0 { return label; }
    // SortEntry.key may be scoped/compound. Direction never changes the fact being described.
    let term = key.split(',').next().unwrap_or(key).split(':').next().unwrap_or(key);
    let key = term.rsplit('.').next().unwrap_or(term);
    let facts = item.sort_info.as_ref();
    let date = |value: Option<i64>| value.and_then(plx_platform::i18n::local_calendar_date)
        .and_then(|[y, m, d]| current().date_medium(y, m as u8, d as u8));
    let mut mark = &[][..];
    let value = match key {
        "titleSort" | "title" | "year" | "random" => None,
        "originallyAvailableAt" => {
            // No year fallback: it already has its own line, and is not a complete release date.
            let iso = item.aired.split(['T', ' ']).next().unwrap_or("");
            let value = fmt::pretty_date_medium(iso, 0);
            (!value.is_empty()).then_some(value)
        }
        "rating" | "audienceRating" => {
            let value = if key == "rating" { facts.and_then(|f| f.rating) }
                else { item.audience_rating.map(f64::from) };
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
            fmt::resolution(item.video_resolution.as_deref().unwrap_or(""), width, height)
                .map(|value| version(value, facts.is_some_and(|f| f.media_count > 1)))
        }
        "mediaBitrate" => facts.and_then(|f| f.bitrate_kbps)
            .map(|value| version(fmt::bitrate(value), facts.is_some_and(|f| f.media_count > 1))),
        _ => None,
    };
    label.sort_info = value.and_then(|value| std::ffi::CString::new(value).ok());
    label.sort_mark = mark;
    label.sort_bold = matches!(key, "rating" | "audienceRating" | "userRating");
    label
}

fn version(value: String, multiple: bool) -> String {
    if multiple { format!("{value} · {}", msg::under_posters_default_version()) } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::pms::SortInfo;
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

    fn movie() -> PmsMovie {
        PmsMovie { title: "Film".into(), year: 1997, aired: "1997-12-12".into(),
            rating: "PG-13".into(), audience_rating: Some(8.5), added_at: Some(1_793_275_200),
            dur_ns: 134 * 60_000_000_000, video_resolution: Some("4k".into()), unwatched: true,
            sort_info: Some(SortInfo { rating: Some(10.0), rating_image: "rottentomatoes://image.rating.ripe".into(),
                audience_image: "rottentomatoes://image.rating.spilled".into(), user_rating: Some(8.0),
                view_count: Some(3), last_viewed_at: Some(1_793_188_800),
                bitrate_kbps: Some(8000), media_count: 1, ..Default::default() }), ..Default::default() }
    }
    fn text(m: &PmsMovie, key: &str) -> Option<String> { label(m, key).sort_info.map(|s| s.to_string_lossy().into_owned()) }

    #[test]
    fn all_fifteen_sort_facts_preserve_identity_and_do_not_invent_missing_values() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let m = movie();
        for key in ["titleSort", "year", "random", "futureServerSort"] {
            let l = label(&m, key);
            assert_eq!(l.title.unwrap().to_str().unwrap(), "Film");
            assert_eq!(l.caption.unwrap().to_str().unwrap(), "1997");
            assert!(l.sort_info.is_none());
        }
        for (key, expected) in [("originallyAvailableAt", "Dec 12, 1997"), ("rating", "100%"),
            ("audienceRating", "85%"), ("userRating", "8.0/10"), ("contentRating", "PG-13"),
            ("duration", "2h 14m"), ("viewOffset", "0%"), ("viewCount", "3"),
            ("addedAt", "Oct 29, 2026"), ("lastViewedAt", "Oct 28, 2026"),
            ("mediaHeight", "4K"), ("mediaBitrate", "8.0 Mbps")] {
            assert_eq!(text(&m, key).as_deref(), Some(expected), "{key}");
            assert_eq!(text(&m, &format!("movie.{key}:desc")), text(&m, key));
        }
        assert!(!label(&m, "rating").sort_mark.is_empty());
        assert_ne!(label(&m, "rating").sort_mark, label(&m, "audienceRating").sort_mark);
        let bare = PmsMovie::default();
        for key in ["originallyAvailableAt", "rating", "audienceRating", "userRating", "contentRating", "duration", "viewOffset", "viewCount", "addedAt", "lastViewedAt", "mediaHeight", "mediaBitrate"] {
            assert!(text(&bare, key).is_none(), "{key}");
        }
    }

    #[test]
    fn progress_rewatch_versions_and_unknown_rating_provider_are_honest() {
        let _locale = language_on_this_thread_for_test(Preference::En);
        let mut m = movie();
        m.watched = true; m.unwatched = false;
        assert_eq!(text(&m, "viewOffset").as_deref(), Some("100%"));
        m.resume_ms = m.dur_ns / 1_000_000 / 2;
        assert_eq!(text(&m, "viewOffset").as_deref(), Some("50%"));
        m.watched = false; m.resume_ms = i64::MAX;
        // Avoid using resume_frac on a corrupt wire offset (see the caption's range guard).
        assert!(text(&m, "viewOffset").is_none());
        m.sort_info.as_mut().unwrap().media_count = 2;
        assert_eq!(text(&m, "mediaHeight").as_deref(), Some("4K · Default"));
        m.sort_info.as_mut().unwrap().rating_image = "unknown://score".into();
        assert_eq!(text(&m, "rating").as_deref(), Some("10.0/10"));
        assert!(label(&m, "rating").sort_mark.is_empty());
    }

    #[test]
    fn russian_numbers_units_and_calendar_dates_use_shared_formatting() {
        let _locale = language_on_this_thread_for_test(Preference::Ru);
        let m = movie();
        assert_eq!(text(&m, "userRating").as_deref(), Some("8,0/10"));
        assert_eq!(text(&m, "addedAt"), current().date_medium(2026, 10, 29));
        assert_eq!(text(&m, "duration"), Some(fmt::duration(134 * 60_000)));
        assert_eq!(text(&m, "mediaBitrate"), Some(fmt::bitrate(8000)));
    }

    #[test]
    fn web_caption_feedback_dates_use_named_months() {
        let _locale = language_on_this_thread_for_test(Preference::Ru);
        let m = movie();
        let date = text(&m, "addedAt").unwrap();
        assert!(date.contains("окт.") && date.contains("2026") && !date.contains('/'), "{date}");
    }

    #[test]
    fn review_preserves_the_existing_russian_sort_names() {
        let _locale = language_on_this_thread_for_test(Preference::Ru);
        assert_eq!(plx_data::sortfacts::SortFact::Rating.label(), "Оценка");
        assert_eq!(plx_data::sortfacts::SortFact::AudienceRating.label(), "Оценка зрителей");
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
            assert_eq!(label(&m, key).sort_mark, mark(RatingArt::from_image(
                if field { &m.sort_info.as_ref().unwrap().rating_image } else { &m.sort_info.as_ref().unwrap().audience_image }
            ).unwrap()));
            assert!(label(&m, key).sort_bold);
        }
        for key in ["originallyAvailableAt", "duration", "addedAt", "lastViewedAt"] {
            assert!(!label(&m, key).sort_bold, "{key}");
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
    if let Ok(day) = std::env::var("PLX_SORT_DATE_TEST_DAY") {
        let _locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        let stamp = 1_793_233_800; // 2026-10-29 00:30 UTC
        let item = PmsMovie { added_at: Some(stamp),
            sort_info: Some(plx_data::pms::SortInfo { last_viewed_at: Some(stamp), ..Default::default() }),
            ..Default::default() };
        for key in ["addedAt", "lastViewedAt"] {
            let value = label(&item, key).sort_info.unwrap();
            assert_eq!(value.to_str().unwrap(), day);
        }
        let wire = serde_json::to_string(&item).unwrap();
        assert!(!wire.contains("added_date") && !wire.contains("last_viewed_date"));
        assert_eq!(serde_json::from_str::<PmsMovie>(&wire).unwrap().added_at, Some(stamp));
        return;
    }
    for (tz, day) in [("UTC0", "Oct 29, 2026"), ("EST5", "Oct 28, 2026"), ("JST-9", "Oct 29, 2026")] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry::sort_info::review_timestamp_dates_follow_timezone", "--nocapture"])
            .env("TZ", tz).env("PLX_SORT_DATE_TEST_DAY", day).status().unwrap();
        assert!(status.success(), "date formatting failed under {tz}");
    }
}
