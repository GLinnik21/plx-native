//! Tiny shared display formatters — ONE home for the duration/clock/release-date strings so the
//! same value can't render differently across screens (the "2 hr 15 min" vs "2h 15m" vs
//! "0 hr 45 min" drift this replaces).

use plx_platform::i18n::{msg, LocaleContext, MonthWidth};

pub fn converts_on_server() -> &'static str { msg::core_converts_on_server() }

/// **The one duration** — a film's or episode's length, "2h 14m" / "14m", on every screen that
/// tells one (the captions under posters, the detail hero, About's Run Time, the episode rows, the
/// source list, the Info card, the HUD). There used to be a compact form ("10m", "2h 14m") and a
/// spelled-out one ("14 min"), and which a value wore depended on the screen it was on; the shape
/// has one home now and the minutes-first split lives in [`duration_minutes_in`] only.
pub fn duration(ms: i64) -> String {
    duration_in(plx_platform::i18n::current(), ms)
}

/// [`duration`] for a context other than the thread's (a caption that carries its own).
pub fn duration_in(cx: &LocaleContext, ms: i64) -> String {
    duration_minutes_in(cx, ms / 60_000)
}

/// [`duration`] from whole minutes (a fact the server already gave in minutes).
pub fn duration_minutes_in(cx: &LocaleContext, mins: i64) -> String {
    let (h, m) = split_hours(mins);
    if h > 0 { msg::core_duration_hours_in(cx, h, m) } else { msg::core_duration_minutes_in(cx, m) }
}

/// Whole minutes as `(hours, minutes)`, a negative count read as none.
fn split_hours(mins: i64) -> (i64, i64) {
    let mins = mins.max(0);
    (mins / 60, mins % 60)
}

/// **Seconds-scale duration** — "4.5 s" / "12 s". The two formatters above are minute-scale and
/// round a fifteen-second interval to "0m", which is a read-out saying nothing; this is for the
/// short spans the diagnostics panel reports (an Original shortfall's elapsed time, N13).
///
/// One decimal below ten seconds and none above, because the interesting distinctions are at the
/// bottom of the range — the difference between 2 s and 4.5 s of shortfall is a decision, and the
/// difference between 40 s and 41 s is not.
pub fn secs_short(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 10_000 {
        plx_platform::i18n::msg::core_seconds(&plx_platform::i18n::current().decimal(ms / 100, 1))
    } else {
        plx_platform::i18n::msg::core_seconds(&plx_platform::i18n::current().number(ms / 1_000))
    }
}

/// **N3: one duration, one release phrase.** The length of a film is one shape on every screen, and
/// the unreleased title's date line is one phrase on both pages that tell it.
#[cfg(test)]
mod one_shape_tests {
    use super::*;
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

    const MINUTE: i64 = 60_000;

    #[test]
    fn a_duration_is_one_compact_shape_whatever_the_screen() {
        let _g = language_on_this_thread_for_test(Preference::from_tag("en"));
        for (ms, want) in [(10 * MINUTE, "10m"), (134 * MINUTE, "2h 14m"), (14 * MINUTE, "14m"), (60 * MINUTE, "1h 0m"),
            (0, "0m"), (-5 * MINUTE, "0m"), (134 * MINUTE + 59_999, "2h 14m")] {
            assert_eq!(duration(ms), want, "{ms} ms");
        }
        let cx = plx_platform::i18n::current();
        assert_eq!(duration_minutes_in(cx, 134), duration_in(cx, 134 * MINUTE), "minutes and milliseconds are one rule");
        assert_eq!(time_left(8 * MINUTE), "8m left");
        assert_eq!(time_left(62 * MINUTE), "1h 2m left");
        assert_eq!(time_left(1), "1m left", "time left never reads zero");
        assert_eq!(time_left_minutes_in(cx, 0), "1m left");
    }

    /// **Time left is a duration with a "left" word** (design review N-d): "8m left", "1h 2m left",
    /// the unit marks every other length on screen wears ("6m", "2h 14m"), in every shipped language.
    #[test]
    fn time_left_wears_the_unit_marks_of_a_duration_in_every_language() {
        for tag in ["en", "de", "es", "fr", "hu", "it", "pt", "be"] {
            let _g = language_on_this_thread_for_test(Preference::from_tag(tag));
            let cx = plx_platform::i18n::current();
            for minutes in [8, 62] {
                let (left, length) = (time_left_minutes_in(cx, minutes), duration_minutes_in(cx, minutes));
                assert!(left.contains(&length), "{tag}: {left:?} does not carry the duration {length:?}");
            }
        }
    }

    #[test]
    fn every_shipped_language_tells_a_duration_with_both_numbers() {
        for tag in ["en", "de", "es", "fr", "hu", "it", "pt", "be"] {
            let _g = language_on_this_thread_for_test(Preference::from_tag(tag));
            let (hours, minutes) = (duration(134 * MINUTE), duration(14 * MINUTE));
            assert!(hours.contains('2') && hours.contains("14"), "{tag}: {hours}");
            assert!(minutes.contains("14") && !minutes.contains('0'), "{tag}: {minutes}");
        }
    }

    #[test]
    fn the_release_phrase_is_one_word_at_either_width_and_none_for_a_bad_date() {
        let _g = language_on_this_thread_for_test(Preference::from_tag("en"));
        assert_eq!(coming_on("2027-03-12", MonthWidth::Medium).as_deref(), Some("Coming 12 Mar"));
        assert_eq!(coming_on("2027-03-12", MonthWidth::Long).as_deref(), Some("Coming 12 March"));
        for bad in ["", "tomorrow", "2027-13-40", "2027-02-30", "2027-03", "2027-03-12-01"] {
            assert_eq!(coming_on(bad, MonthWidth::Medium), None, "{bad:?}");
        }
        for tag in ["en", "de", "es", "fr", "hu", "it", "pt", "be"] {
            let _g = language_on_this_thread_for_test(Preference::from_tag(tag));
            let lead = msg::watchlist_state_coming_on("");
            let (medium, long) = (coming_on("2027-03-12", MonthWidth::Medium).unwrap(), coming_on("2027-03-12", MonthWidth::Long).unwrap());
            assert!(medium.starts_with(lead.trim_end()), "{tag}: {medium} vs {lead:?}");
            assert!(long.starts_with(lead.trim_end()), "{tag}: {long} vs {lead:?}");
        }
    }
}

#[cfg(test)]
#[test]
fn seconds_scale_durations_keep_the_distinctions_that_matter() {
    assert_eq!(secs_short(0), "0.0 s");
    assert_eq!(secs_short(4_500), "4.5 s");
    assert_eq!(secs_short(9_999), "9.9 s");
    assert_eq!(secs_short(12_000), "12 s");
    // Negative is not a duration; it reads as none rather than as a minus sign on screen.
    assert_eq!(secs_short(-1), "0.0 s");
}

/// Time-remaining for a Continue-Watching item — "8m left" / "1h 2m left" (rounded up to
/// the next whole minute, floored at 1). `remaining_ms` is duration − viewOffset.
pub fn time_left(remaining_ms: i64) -> String {
    time_left_minutes_in(plx_platform::i18n::current(), (remaining_ms + 59_999) / 60_000)
}

/// [`time_left`] from whole minutes, in a given context; at least a minute (the line promises
/// time, and "0m left" is none).
pub fn time_left_minutes_in(cx: &LocaleContext, mins: i64) -> String {
    let (h, m) = split_hours(mins.max(1));
    if h > 0 { msg::core_time_left_hours_in(cx, h, m) } else { msg::core_time_left_minutes_in(cx, m) }
}

/// **The one release-date phrase** — "Coming 12 Mar" — for an unreleased title: the Watchlist's focus
/// caption and the catalogue page's line both read it, so the word cannot differ between them (one
/// said "Releasing", the other "Coming"). `release` is `YYYY-MM-DD` (what `originallyAvailableAt`
/// carries); anything else, or a day the calendar lacks, is no phrase. The width is the surface's:
/// a caption under a poster takes the abbreviated month, a page line with room the whole one.
pub fn coming_on(release: &str, width: MonthWidth) -> Option<String> {
    let mut parts = release.split('-');
    let _year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let date = plx_platform::i18n::current().month_day(month, day, width)?;
    Some(msg::watchlist_state_coming_on(&date))
}

/// Playback clock — "1:23:45" past the hour, else "3:45" (scrubber clocks, chapter stamps).
pub fn clock(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// An episode's ADDRESS inside its show — `"S2, E3"`, with no title. The detail page's hero meta
/// line draws it bare (the title band above it already IS the episode's name, so repeating it there
/// would be the one line on the page that says nothing new), and [`episode_kicker`] is this plus the
/// title. It lives here rather than as a `format!` beside its one caller because the two spellings
/// have to stay one vocabulary: a page that said "S2, E3" while the HUD said "S2E3" for the same
/// leaf is exactly the drift this module exists to prevent, and nothing but shared code enforces it.
/// **An episode's ADDRESS — `S1 · E3`** — the ONE spelling of it, in every place the app prints it:
/// the identifier printed inside a still's artwork, a poster's persistent mark, the Library grid's
/// and Recommended's title line, and the Under posters caption (`caption_policy`, the Continue
/// Watching deck's `cw_caption`). The separator is the MIDDLE DOT, the design's: the Library
/// mock's still ("`Library Screens.dc.html` E") and the redesign spec's details line by kind
/// ("Episode “S2 · E4 · 48 min”", `spec-collections.md`) both set it, as the official client's own
/// hold menu does. (The caption used to spell it from a catalog key of its own, with a bullet, so one
/// episode read two ways: on a poster's art and under it.)
///
/// `Library Screens.dc.html` E is also why it is the printed line rather than the episode's name: "four
/// tiles of one show differ only by number, so the number cannot be the thing behind focus. The
/// episode's *name* can: it distinguishes nothing until you have already picked a tile."
///
/// **Both parts are optional and the separator comes from what is PRESENT**, because a server that
/// sent no season or episode index is an ordinary answer here rather than a broken one — a specials
/// folder, a date-numbered series, an item mid-scan. With neither it is empty and the caller draws
/// nothing, not a line of leftover dots.
///
/// The MIDDOT is what separates this from [`episode_ordinal`]'s `S1, E3`, and the two are kept
/// apart deliberately: this one is a printed mark on artwork, sized and inked as its own line,
/// while the ordinal is a run inside sentences elsewhere in the app.
pub fn episode_address(season: i64, index: i64) -> String {
    episode_address_in(plx_platform::i18n::current(), season, index)
}

/// [`episode_address`] in `cx`'s language, for the formatters that carry a locale.
pub fn episode_address_in(cx: &LocaleContext, season: i64, index: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if season > 0 {
        parts.push(msg::core_season_in(cx, season));
    }
    if index > 0 {
        parts.push(msg::core_episode_in(cx, index));
    }
    parts.join(" \u{b7} ")
}

/// **What a season is called**, the ONE rule every surface that names a season reads (its caption
/// under a poster, its tab on Detail): the title the server gave it ("Season 6", "Specials"), else
/// "Season N" from the catalog, else "Specials" for the season numbered 0 that the server gave no
/// title (a specials folder). A number that is not positive is no number, so it is never printed
/// as one ("Season 0").
pub fn season_identity(number: i64, own: &str) -> String {
    season_identity_in(plx_platform::i18n::current(), number, own)
}

/// [`season_identity`] in `cx`'s language, for the formatters that carry a locale.
pub fn season_identity_in(cx: &LocaleContext, number: i64, own: &str) -> String {
    if !own.is_empty() {
        own.to_string()
    } else if number > 0 {
        msg::browse_detail_season_number_in(cx, number)
    } else {
        msg::browse_detail_specials_in(cx).to_string()
    }
}

/// A collection's size in the noun of what it holds — `"6 films"`, `"3 shows"`, else `"12 items"` —
/// from the collection's PMS `subtype` (a movie collection holds films, a show collection shows;
/// anything else, or none known yet, is items). ONE formatter for the Collection page's meta line,
/// every collection tile's caption (shelf, Collections page) and the search hit, so a collection is
/// never "6 items" on one screen and "6 films" on the next.
pub fn collection_count(subtype: Option<&str>, n: i64) -> String {
    collection_count_in(plx_platform::i18n::current(), subtype, n)
}

/// [`collection_count`] in `cx`'s language, for the formatters that carry a locale.
pub fn collection_count_in(cx: &LocaleContext, subtype: Option<&str>, n: i64) -> String {
    match subtype {
        Some("movie") => msg::browse_person_films_in(cx, n),
        Some("show") => msg::browse_person_shows_in(cx, n),
        _ => msg::browse_search_items_in(cx, n),
    }
}

/// **The noun the collection page's grid heading wears** — the one [`collection_count`] counts in
/// (design review N-g): "6 films" sat under an "Items" heading, two nouns for one set. The subtype
/// is read the same way, so the heading and the count cannot disagree.
pub fn collection_noun(subtype: Option<&str>) -> &'static str {
    match subtype {
        Some("movie") => msg::browse_collection_films(),
        Some("show") => msg::browse_collection_shows(),
        _ => msg::browse_collection_items(),
    }
}

/// A count of members with no kind known — `"1 item"`, `"12 items"`: [`collection_count`] before the
/// subtype is.
pub fn item_count(n: i64) -> String {
    collection_count(None, n)
}

/// An episode's ordinal inside a sentence, `"S2, E3"`, **from the parts the server numbered**: with
/// no season `"E3"`, with no episode number `"S2"`, and with neither the empty string, which a
/// caller leaves out ([`episode_kicker`], [`episode_context`]). A number that is not positive is no
/// number (the server sent none, or numbered the specials 0), so it is never printed as one:
/// "S1, E0" is not an address. [`episode_address_in`] is the same rule for the printed mark.
pub fn episode_ordinal(season: i64, index: i64) -> String {
    use plx_platform::i18n::msg;
    match (season > 0, index > 0) {
        (true, true) => msg::core_episode_ordinal(index, season),
        (false, true) => msg::core_episode(index),
        (true, false) => msg::core_season(season),
        (false, false) => String::new(),
    }
}

/// The non-empty `parts` joined with `sep`; none, none.
fn join_present(parts: &[&str], sep: &str) -> String {
    parts.iter().copied().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(sep)
}

/// A line naming an episode and its show, `"Show  ·  S2, E3"` (the route's pre-roll context): the
/// show, then the [`episode_ordinal`] when the episode has one. ONE formatter for the Detail page's
/// play and the app's own playback request, which each spelled it by hand.
pub fn episode_context(show: &str, season: i64, index: i64) -> String {
    join_present(&[show, &episode_ordinal(season, index)], "  \u{b7}  ")
}

/// **The kicker over an episode card on Detail**, `"EPISODE 3"`: the card's own number, and for an
/// episode the server did not number the KIND WORD in the same capitals ("EPISODE"), never "EPISODE
/// 0". The aired date is the card's bottom row already, so the kicker does not say it again (the
/// identity chain every other surface falls back on is address, then date, then kind word; the date
/// rung is this card's own row).
pub fn episode_card_kicker(index: i64) -> String {
    if index > 0 {
        plx_platform::i18n::msg::browse_detail_episode_number(index)
    } else {
        plx_platform::i18n::msg::browse_kind_episode().to_uppercase()
    }
}

/// The source attribution — `"Shared by friend"` — or `None` when there is nobody to credit.
///
/// **This is the WORDS and not the RULE.** `handle` arrives already decided:
/// `plex::servers::owner_credit` is the one place that says whether a server is credited to
/// anybody, `plex::ServerFacts::handle` is where its answer rests, and every caller here reads
/// that field. So an empty handle does not mean "an owned server" — it means *our own server, or
/// the household's server whichever Plex Home profile is watching, or a share plex.tv has not
/// named* — and this function must never grow a second opinion about which of those it is. The
/// field it reads used to be plex.tv's raw `sourceTitle`, which is how the app came to tell a
/// managed user that their own house's library was "Shared by" the person who pays for it.
///
/// `None` rather than an empty `String`, because ABSENCE is what the design specifies: no
/// separator, no empty run, no draw call. Both call sites were hand-rolling that guard.
///
/// ONE formatter for the same reason [`episode_kicker`] is one: this phrase is drawn by the hero's
/// meta line, the detail page's facts row and (with a sentence after it) the Library's failure
/// read-out. It was written twice with two different empty-handle behaviours and interpolated a
/// third time inline — exactly the drift this module exists to prevent.
pub fn shared_by(handle: &str) -> Option<String> {
    (!handle.is_empty()).then(|| plx_platform::i18n::msg::core_shared_by(handle))
}

/// The episode kicker — `"S2, E3 · Laura"`, the [`episode_ordinal`] with the episode's title after
/// it. ONE formatter, because this string is drawn by the transport HUD (the now-playing item), the
/// route's pre-roll ctx line and the Up Next caption, and the whole point of the last two is that
/// they read identically to the first. It was three separate literals before, with nothing keeping
/// them in step.
pub fn episode_kicker(season: i64, index: i64, title: &str) -> String {
    join_present(&[&episode_ordinal(season, index), title], " \u{b7} ")
}

/// The media badge for a video version — `"4K"` / `"1080p"` / `"SD"`, or None when the item has no
/// video (a show container, a music item, an unparsed response).
///
/// `res` is PMS's `Media.videoResolution` and is PREFERRED over the frame size, because the frame
/// size is the STORED one: a 2.35:1 1080p film is 1918x802 on the dev server, and a height rule
/// would badge it 720p. Verified values are `"4k"`, `"1080"`, `"720"`, `"576"`, `"sd"` — and PMS
/// sends every one of them as a numeric-looking STRING, which is exactly the shape this maps.
/// `width`/`height` are only the fallback for a version that omits the class.
pub fn resolution(res: &str, width: i64, height: i64) -> Option<String> {
    let r = res.trim().to_ascii_lowercase();
    if !r.is_empty() {
        // "4k"/"8k" are already the badge; a bare number is a scan-line count ("1080" → "1080p");
        // anything else (e.g. "sd") is a class name and reads as itself, upper-cased.
        return Some(match r.as_str() {
            _ if r.chars().all(|c| c.is_ascii_digit()) => format!("{r}p"),
            _ => r.to_ascii_uppercase(),
        });
    }
    // No class: fall back to the frame WIDTH, which (unlike height) is constant across aspect
    // ratios. Thresholds sit below each nominal width (3840/2560/1920/1280/1024) so a cropped or
    // anamorphic frame still lands in its own class, and the buckets are the SAME vocabulary the
    // class path emits — the two paths must not disagree about the same file.
    // saturating: these are wire values via the lenient `de_i64`, so a garbage 19-digit height
    // must not overflow (a debug-build panic, a silently wrapped badge in release)
    let w = if width > 0 {
        width
    } else {
        height.saturating_mul(16) / 9
    };
    Some(match w {
        w if w <= 0 => return None,
        w if w >= 3000 => "4K".to_string(),
        w if w >= 2400 => "1440p".to_string(),
        w if w >= 1700 => "1080p".to_string(),
        w if w >= 1100 => "720p".to_string(),
        w if w >= 900 => "576p".to_string(),
        _ => "SD".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        collection_count, collection_count_in, collection_noun, episode_card_kicker, episode_context, episode_kicker,
        episode_ordinal, resolution, season_identity_in,
    };

    /// **An episode the server did not number never prints a zero address**, in any language: the
    /// ordinal says the parts it has ("S1, E3", "E3", "S1") and nothing when it has none, and the
    /// lines built on it (the kicker over a title, the "show · address" context line, the Detail
    /// card's kicker) leave the empty part out rather than print "S1, E0" or a trailing dot.
    #[test]
    fn an_unnumbered_episode_never_prints_a_zero_address() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        for language in SHIPPED {
            let _g = language_on_this_thread_for_test(language);
            let tag = language.tag();
            assert_eq!(episode_ordinal(1, 3), plx_platform::i18n::msg::core_episode_ordinal(3, 1), "{tag}");
            for (season, index) in [(1, 0), (0, 3), (0, 0), (-1, -1), (2, -1)] {
                let ordinal = episode_ordinal(season, index);
                assert!(!ordinal.contains('0') && !ordinal.contains("-1"), "{tag}: ({season}, {index}) read {ordinal:?}");
            }
            assert_eq!(episode_ordinal(0, 0), "", "{tag}");
            assert_eq!(episode_ordinal(0, 3), plx_platform::i18n::msg::core_episode(3), "{tag}: no season, the episode alone");
            assert_eq!(episode_ordinal(2, 0), plx_platform::i18n::msg::core_season(2), "{tag}: no episode, the season alone");
            assert_eq!(episode_kicker(1, 0, "Pilot"), format!("{} \u{b7} Pilot", episode_ordinal(1, 0)), "{tag}");
            assert_eq!(episode_kicker(0, 0, "Pilot"), "Pilot", "{tag}: nothing numbered leaves the title alone");
            assert_eq!(episode_kicker(0, 0, ""), "", "{tag}");
            assert_eq!(episode_context("Show", 0, 0), "Show", "{tag}: no trailing dot");
            assert_eq!(episode_context("Show", 1, 3), format!("Show  \u{b7}  {}", episode_ordinal(1, 3)), "{tag}");
            let kicker = episode_card_kicker(0);
            assert!(!kicker.contains('0') && !kicker.is_empty(), "{tag}: the card's kicker without a number is {kicker:?}");
            assert_eq!(kicker, plx_platform::i18n::msg::browse_kind_episode().to_uppercase(), "{tag}: the kind word");
            assert_eq!(episode_card_kicker(3), plx_platform::i18n::msg::browse_detail_episode_number(3), "{tag}");
            assert_eq!(season_identity_in(plx_platform::i18n::current(), 0, ""), plx_platform::i18n::msg::browse_detail_specials(), "{tag}");
        }
    }

    /// **The collection page's heading uses the noun its count uses** (design review N-g), in every
    /// shipped language: "Films" over "6 films", "Shows" over "3 shows", "Items" over "12 items".
    /// A singular stem may match where an inflected plural does not: Russian "Фильмы" shares
    /// "фильм" with "1 фильм", but not the whole noun with "2 фильма" or "5 фильмов".
    #[test]
    fn the_collection_heading_names_the_noun_the_count_uses() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        for language in SHIPPED {
            let _g = language_on_this_thread_for_test(language);
            for subtype in [Some("movie"), Some("show"), Some("episode"), None] {
                let heading = collection_noun(subtype);
                let counts: Vec<_> = [1, 2, 5].into_iter().map(|n| collection_count(subtype, n)).collect();
                assert!(counts.iter().any(|count| {
                    let noun = count.split_once(' ').map(|(_, noun)| noun.to_lowercase()).unwrap();
                    heading.to_lowercase().starts_with(&noun)
                }), "{} {subtype:?}: {heading:?} over {counts:?}", language.tag());
                assert!(heading.chars().next().is_some_and(char::is_uppercase), "{heading:?} is a heading");
            }
        }
    }

    /// **One noun for a collection, from its subtype** (design review S20): the Collections page's
    /// caption, the Collection screen's meta line, a shelf tile and a search hit all count through
    /// this, so "6 items" and "6 films" cannot name one collection.
    #[test]
    fn a_collection_is_counted_in_the_noun_of_its_subtype() {
        let _g = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
        assert_eq!(collection_count(Some("movie"), 6), "6 films");
        assert_eq!(collection_count(Some("movie"), 1), "1 film");
        assert_eq!(collection_count(Some("show"), 3), "3 shows");
        assert_eq!(collection_count(Some("episode"), 12), "12 items", "a subtype with no noun of its own is items");
        assert_eq!(collection_count(None, 12), "12 items", "before the subtype is known");
        assert_eq!(super::item_count(1), "1 item");
        let de = plx_platform::i18n::LocaleContext::resolve(plx_platform::i18n::Preference::De, None, None, None, None);
        assert_eq!(collection_count_in(&de, Some("movie"), 6), plx_platform::i18n::msg::browse_person_films_in(&de, 6));
    }

    /// The mapping is fed PMS's own values, and PMS string-encodes them — including the ones that
    /// look like integers ("1080"), which is the case that must come out as "1080p" and not as a
    /// number or an empty badge.
    #[test]
    fn resolution_labels_pms_string_encoded_classes() {
        // verified live on the dev server: every videoResolution is a JSON string
        assert_eq!(resolution("4k", 3840, 2160).as_deref(), Some("4K"));
        assert_eq!(resolution("1080", 1920, 1080).as_deref(), Some("1080p"));
        assert_eq!(resolution("720", 1280, 720).as_deref(), Some("720p"));
        assert_eq!(resolution("576", 1024, 576).as_deref(), Some("576p"));
        assert_eq!(resolution("sd", 826, 452).as_deref(), Some("SD"));
        // whitespace/casing from the wire must not produce a second spelling of a badge
        assert_eq!(resolution(" 4K ", 0, 0).as_deref(), Some("4K"));
        // the class WINS over the frame size: a 2.35:1 1080p film is 1918x802 (a height rule
        // would call it 720p, a width rule 1080p — either way the server's own answer decides)
        assert_eq!(resolution("1080", 1918, 802).as_deref(), Some("1080p"));
    }

    #[test]
    fn resolution_falls_back_to_frame_width() {
        assert_eq!(resolution("", 3840, 2160).as_deref(), Some("4K"));
        assert_eq!(resolution("", 1918, 802).as_deref(), Some("1080p")); // scope 1080p, by WIDTH
        assert_eq!(resolution("", 1280, 720).as_deref(), Some("720p"));
        assert_eq!(resolution("", 720, 480).as_deref(), Some("SD"));
        // the fallback speaks the class path's vocabulary: the same file must not badge "576p"
        // with a videoResolution and "SD" without one
        assert_eq!(
            resolution("", 1024, 576).as_deref(),
            resolution("576", 1024, 576).as_deref()
        );
        assert_eq!(resolution("", 0, 1080).as_deref(), Some("1080p")); // width absent → from height
        assert_eq!(resolution("", 0, 0), None); // no video at all (a show container) → no badge
    }
}

/// A calendar date from a Plex `YYYY-MM-DD`, in the UI language's own short form (the locale's ICU
/// short date: `"2/1/1921"` in English). ONE spelling, because the same
/// value is drawn by several screens: detail's episode air dates, the person page's Born/Died line
/// and the landscape stills' release date (`registry::captions::still_label`).
/// `year` is the fallback when the item carries only a year (an episode with no `originallyAvailableAt`);
/// pass 0 when there is none, and an unparseable date yields the empty string rather than a
/// half-formatted one — every caller draws a date line only when it is non-empty.
pub fn pretty_date(iso: &str, year: i64) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(mo), Ok(da)) = (
            parts[0].parse::<i64>(),
            parts[1].parse::<usize>(),
            parts[2].parse::<i64>(),
        ) {
            if let (Ok(y), Ok(m), Ok(d)) = (i32::try_from(y), u8::try_from(mo), u8::try_from(da)) {
                if let Some(date) = plx_platform::i18n::current().date(y, m, d) { return date; }
            }
        }
    }
    if year > 0 {
        year.to_string()
    } else {
        String::new()
    }
}

/// The units a review score is quoted in — the one thing about a provider the formatters need.
/// The caller maps its own notion of a provider onto this (`screens::detail` maps
/// `metadata::RatingArt`), so the library names no provider and no application type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RatingScale {
    /// Out of ten, one decimal, with a trailing "/10" — IMDb ("7.4").
    OutOfTen,
    /// A whole percentage — Rotten Tomatoes and TMDB ("91%").
    Percent,
}

/// A review score, in the shape its own provider publishes it. PMS normalises every provider onto
/// one 0–10 scale, which is not how any of them are quoted: Rotten Tomatoes and TMDB are
/// PERCENTAGES (9.1 → "91%") and IMDb is out of ten ("7.4"). Printing the raw 9.1 beside a tomato
/// would read as a 9.1% score, so the badge's number is put back into the provider's own units here.
pub fn rating_score(scale: RatingScale, value: f64) -> String {
    match scale {
        RatingScale::OutOfTen => plx_platform::i18n::current().decimal((value * 10.0).round() as i64, 1),
        RatingScale::Percent => percent((value * 10.0).round().clamp(0.0, 100.0) as i64),
    }
}

/// A whole percentage in the UI locale: the catalog pattern owns the sign's placement (Spanish and
/// Belarusian set it off with a no-break space; English does not).
pub fn percent(value: i64) -> String {
    plx_platform::i18n::msg::core_percent(&plx_platform::i18n::current().number(value))
}

/// `value` rounded to `scale` fractional digits, in the UI locale's numerals and separators.
pub fn decimal(value: f64, scale: i16) -> String {
    let factor = 10i64.pow(u32::try_from(scale).unwrap_or(0)) as f64;
    plx_platform::i18n::current().decimal((value * factor).round() as i64, scale)
}

/// [`decimal`] that always carries a sign, as a delta is written (`+0.4`, `-1.2`).
pub fn signed_decimal(value: f64, scale: i16) -> String {
    let factor = 10i64.pow(u32::try_from(scale).unwrap_or(0)) as f64;
    let scaled = (value * factor).round() as i64;
    let digits = plx_platform::i18n::current().decimal(scaled, scale);
    if scaled >= 0 {
        format!("+{digits}")
    } else {
        digits
    }
}

/// A bitrate given in kbps: `28.9 Mbps` from a megabit up, otherwise whole `640 kbps`, in the UI
/// locale's numerals and unit spelling.
pub fn bitrate(kbps: i64) -> String {
    if kbps >= 1000 {
        plx_platform::i18n::msg::widgets_tracks_mbps(&decimal(kbps as f64 / 1000.0, 1))
    } else {
        plx_platform::i18n::msg::widgets_tracks_kbps(&plx_platform::i18n::current().number(kbps))
    }
}

/// A byte count in binary units with the UI locale's numerals — `2.50 GB`, `12.3 MB`, `640 kB`.
/// `scales` is the number of fractional digits for GB, MB and kB respectively; a count below a
/// kilobyte is whole bytes (`B`).
pub fn bytes(bytes: i64, scales: (i16, i16, i16)) -> String {
    use plx_platform::i18n::msg;
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        msg::core_unit_gb(&decimal(b / (K * K * K), scales.0))
    } else if b >= K * K {
        msg::core_unit_mb(&decimal(b / (K * K), scales.1))
    } else if b >= K {
        msg::core_unit_kb(&decimal(b / K, scales.2))
    } else {
        msg::core_unit_b(&plx_platform::i18n::current().number(bytes))
    }
}

/// The unit that trails a score, set a rung down in tertiary ink — see `widgets::RatingCell`.
///
/// Only an out-of-ten score (IMDb's) has one. A percentage carries its `%` inside [`rating_score`] because there the sign
/// is part of the number, whereas "/10" is a note about the SCALE: it is what stops an 8.1 being
/// read against the 69% beside it, and it is the reason the two can share a row at all.
pub fn rating_suffix(scale: RatingScale) -> &'static str {
    match scale {
        RatingScale::OutOfTen => "/10",
        RatingScale::Percent => "",
    }
}
