//! The words of a value sort's rail: the label each band wears (`95%`, `<75%`, `2½h`, `3h+`, `10`,
//! `12+`, `Today`, `Oct`, `2025`, `2020s`) and the identity focus keeps for it. The data layer names a
//! band by a [`Mark`] token; what it is called in the viewer's language is decided here, from the
//! catalog and the locale's own number, percent and month formats, never from a list of languages.
//!
//! Every [`Mark`] has a label, so every family the data layer plans (`BandFamily::ALL`) can be drawn.
//! The shared rules:
//!
//! - **A percent is [`fmt::percent`]**, so the sign is placed as the catalog places it, under a poster
//!   and on the rail alike (`git grep '"core.percent"' -- 'locales/*/core.json'` shows which languages
//!   set it off with a no-break space).
//! - **An hour is the language's own short hour** (`library_rail.hours`: `h`, `ó`, `u`, `ч`, `г`,
//!   `godz.`), spaced as `core.duration_hours` spaces it, and the half hour is a `½` after the number.
//! - **`<` is written by the code, and so is the `+` of the count ladders** (`12+`, `10+`); the Duration
//!   ladder's `3h+` is `library_rail.hours_or_more`, a catalog string, so a language can put the `+` after
//!   its own unit.
//! - **A day is `library_rail.today`**: the word under a poster (`under_posters.value_today`), or its
//!   usual abbreviation where the word is too wide for the column (French `Auj.`).
//! - **A month is `LocaleContext::month_short`**, the stand-alone abbreviation (`Oct`, `okt.`, `окт.`),
//!   the name `month_year` writes before a year.
//! - **A year is its plain digits**, as the Year sort's line under a poster writes it: never
//!   `LocaleContext::number`, which would group it (`2,025`).
//! - **A decade is `library_rail.decade`** over the plain digits of its first year (`2020s`, `2020er`,
//!   `años 2020`, `2020-е`) and the last two digits of its last (`end`): a language with no short
//!   phrase that fits the column writes the span (`2020–29`, French and Hungarian). The app hands every
//!   language both; a language reads the ones it needs (`"optional": ["end"]` in the English entry).
//!
//! [`Mark`]: plx_data::browse::bands::Mark
use plx_data::browse::bands::Mark;
use plx_platform::i18n::msg;
use plx_ui::fmt;

/// The label of a band, in the language the UI is in. Exhaustive on purpose: a new [`Mark`] must say
/// what it is called here before it compiles.
pub(super) fn label(mark: Mark) -> String {
    let locale = plx_platform::i18n::current();
    match mark {
        Mark::Percent(n) => fmt::percent(i64::from(n)),
        Mark::BelowPercent(n) => format!("<{}", fmt::percent(i64::from(n))),
        Mark::Minutes(minutes) => msg::library_rail_hours(&hours_text(minutes)),
        Mark::MinutesOrMore(minutes) => msg::library_rail_hours_or_more(&hours_text(minutes)),
        Mark::BelowMinutes(minutes) => format!("<{}", msg::library_rail_hours(&hours_text(minutes))),
        Mark::Count(n) => locale.number(i64::from(n)),
        Mark::CountOrMore(n) => format!("{}+", locale.number(i64::from(n))),
        Mark::Today => msg::library_rail_today().to_owned(),
        // A band's month is always a real one (`plan` builds it from `1..=12`); the number only keeps the
        // match total, and would be visible rather than blank were it ever reached.
        Mark::Month(month) => locale.month_short(month).unwrap_or_else(|| locale.number(i64::from(month))),
        // Plain digits, as the Year sort's line under a poster writes a year: `locale.number` would group it.
        Mark::Year(year) => year.to_string(),
        Mark::Decade(decade) => msg::library_rail_decade(&decade.to_string(), &decade_end(decade)),
    }
}

/// The last two digits of the last year of the decade that starts at `decade`: 2020 -> `29`, 1990 -> `99`,
/// 1900 -> `09`. A language that writes a decade as a span (`2020–29`) reads it; the others leave it out.
fn decade_end(decade: i32) -> String {
    format!("{:02}", (decade + 9).rem_euclid(100))
}

/// A length in minutes as the number of hours the rail names, with the half hour as `½`: 150 is
/// `2½`, 60 is `1`. A ladder step is a whole or a half hour; any other remainder is dropped.
fn hours_text(minutes: u16) -> String {
    let hours = plx_platform::i18n::current().number(i64::from(minutes / 60));
    if minutes % 60 >= 30 { format!("{hours}\u{bd}") } else { hours }
}

/// The key a band's focus element is registered under: its token. Not its label, so a focused band
/// stays itself when the language changes, and a band `1` never shares an identity with the letter
/// `1` of a digit alphabet.
pub(super) fn identity(mark: Mark) -> String {
    format!("band {mark:?}")
}

/// The scale of the calendar a band stands on: a month, a year, a decade. The rest of the ladders
/// (`Today` included, a band of its own) are on none.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scale {
    Months,
    Years,
    Decades,
}

fn scale(mark: Mark) -> Option<Scale> {
    // Exhaustive on purpose: a new mark must say which scale it is on.
    match mark {
        Mark::Month(_) => Some(Scale::Months),
        Mark::Year(_) => Some(Scale::Years),
        Mark::Decade(_) => Some(Scale::Decades),
        Mark::Today | Mark::Percent(_) | Mark::BelowPercent(_) | Mark::Minutes(_) | Mark::MinutesOrMore(_)
        | Mark::BelowMinutes(_) | Mark::Count(_) | Mark::CountOrMore(_) => None,
    }
}

/// The rows (indices into `marks`, the bands top to bottom in the listing's own order) that start a new
/// scale and so stand one step lower (`BandRows::new`): where the months give way to the years, and the
/// years to the decades, `Jan` then `2025`, `2026` then `2020s`. Read between neighbours, so a rail that
/// reads the other way (an ascending sort: `2020s` then `2026`, `2025` then `Jan`) steps at the same
/// seam. `Today` is on no scale and starts none: it and the band after it stand a pitch apart, whether
/// that is `Oct` or, on the first of January, `2025`.
pub(super) fn steps(marks: &[Mark]) -> Vec<usize> {
    (1..marks.len())
        .filter(|&row| matches!((scale(marks[row - 1]), scale(marks[row])), (Some(above), Some(here)) if above != here))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::layout::{band_pill_pad, BAND_PILL_PAD, BAND_PILL_PAD_MIN, BAND_TRACK_LIMIT_W};

    /// The widest a band's label may be: the track limit less the least air each side.
    const BAND_LABEL_LIMIT_W: f32 = BAND_TRACK_LIMIT_W - 2.0 * BAND_PILL_PAD_MIN;
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_data::browse::bands::{plan, BandFamily};
    use plx_machine::machine::Measure;
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};

    /// The widest day the calendar families plan from: the last of a year has `Today` and all twelve months,
    /// and the oldest year the model draws makes the longest run of years and decades.
    const WIDEST_DAY: [i32; 3] = [2026, 12, 31];
    const OLDEST_YEAR: i32 = 1900;

    /// A family's bands, top to bottom, labelled in the language the thread is in.
    fn ladder(family: BandFamily) -> Vec<String> {
        plan(family, WIDEST_DAY, OLDEST_YEAR).iter().map(|band| label(band.mark())).collect()
    }

    fn english() -> impl Drop {
        let english = SHIPPED.into_iter().find(|language| language.tag() == "en").expect("English ships");
        language_on_this_thread_for_test(english)
    }

    #[test]
    fn the_english_ladders_read_as_the_boards_do() {
        let _guard = plx_base::testlock::serial();
        let _language = english();
        assert_eq!(ladder(BandFamily::Rating), ["95%", "90%", "85%", "80%", "75%", "<75%"]);
        assert_eq!(ladder(BandFamily::Duration), ["3h+", "2\u{bd}h", "2h", "1\u{bd}h", "1h", "<1h"]);
        assert_eq!(ladder(BandFamily::Unplayed), ["12+", "10", "8", "6", "4", "2", "1"]);
        assert_eq!(ladder(BandFamily::Plays), ["10+", "5", "3", "2", "1"]);
    }

    #[test]
    fn the_english_calendar_ladders_read_as_the_boards_do() {
        let _guard = plx_base::testlock::serial();
        let _language = english();
        // The owner's board for 31 October 2026, oldest item in 2024: Today, the months back to January, the years.
        let dates = |family| plan(family, [2026, 10, 31], 2024).iter().map(|band| label(band.mark())).collect::<Vec<_>>();
        let expected = ["Today", "Oct", "Sep", "Aug", "Jul", "Jun", "May", "Apr", "Mar", "Feb", "Jan", "2025", "2024"];
        for family in [BandFamily::DateAdded, BandFamily::DateViewed, BandFamily::LastEpisodeAdded] {
            assert_eq!(dates(family), expected, "{family:?}");
        }
        let release = plan(BandFamily::Release, [2026, 10, 31], 1990).iter().map(|band| label(band.mark())).collect::<Vec<_>>();
        assert_eq!(release, ["2026", "2020s", "2010s", "2000s", "1990s"]);
    }

    #[test]
    fn a_calendar_band_wears_the_languages_own_word_month_year_and_decade() {
        let _guard = plx_base::testlock::serial();
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let tag = language.tag();
            let locale = plx_platform::i18n::current();
            // Today: the word under a poster, or its usual abbreviation where that word is too wide.
            let (today, under_posters) = (label(Mark::Today).to_lowercase(), msg::under_posters_value_today().to_lowercase());
            assert!(under_posters.starts_with(today.trim_end_matches('.')), "{tag}: {today:?} is not {under_posters:?} or an abbreviation of it");
            // A month is the stand-alone abbreviation `month_year` writes before a year.
            for month in 1..=12u8 {
                assert_eq!(Some(label(Mark::Month(month))), locale.month_short(month), "{tag} {month}");
            }
            // A year is its plain digits (a grouped `2,025` or `2 025` would be a bug), and a decade is the
            // catalog's phrase over its first year: not the year itself, and one per decade.
            assert_eq!(label(Mark::Year(2025)), "2025", "{tag}");
            let (a, b) = (label(Mark::Decade(2020)), label(Mark::Decade(1990)));
            assert!(a.contains("2020") && b.contains("1990") && a != "2020" && b != "1990", "{tag}: {a:?} {b:?}");
        }
    }

    #[test]
    fn a_decade_ends_where_its_last_year_does_and_a_span_reads_start_and_end() {
        let _guard = plx_base::testlock::serial();
        assert_eq!([2020, 2010, 2000, 1990, 1900].map(decade_end), ["29", "19", "09", "99", "09"]);
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let tag = language.tag();
            // every language is handed both, and one that writes a span puts the end after an en dash
            for decade in [2020, 2000, 1990, 1900] {
                let text = label(Mark::Decade(decade));
                if let Some((start, end)) = text.split_once('\u{2013}') {
                    assert_eq!((start, end), (decade.to_string().as_str(), decade_end(decade).as_str()), "{tag}: {text:?}");
                }
            }
            // 2000 and 1900 end in the same two digits and are two different bands
            assert_ne!(label(Mark::Decade(2000)), label(Mark::Decade(1900)), "{tag}");
        }
    }

    fn steps_of(family: BandFamily, today: [i32; 3], oldest: i32, reversed: bool) -> Vec<usize> {
        let mut marks: Vec<Mark> = plan(family, today, oldest).iter().map(|band| band.mark()).collect();
        if reversed { marks.reverse(); }
        steps(&marks)
    }

    #[test]
    fn a_step_stands_where_the_scale_changes_and_never_beside_today() {
        // Today, Oct..Jan (rows 1-10), 2025 (row 11): the step is before the first year
        assert_eq!(steps_of(BandFamily::DateAdded, [2026, 10, 31], 2024, false), [11]);
        // Today, Jan, 2025: row 2 is the first year after a month
        assert_eq!(steps_of(BandFamily::DateViewed, [2026, 1, 5], 2025, false), [2]);
        // Today, 2025, 2024 on the first of January: a year straight after Today is no scale change
        assert_eq!(steps_of(BandFamily::DateAdded, [2026, 1, 1], 2024, false), Vec::<usize>::new());
        // a year's last day has twelve months and the same one step; a rail with no year has none
        assert_eq!(steps_of(BandFamily::LastEpisodeAdded, [2026, 12, 31], 1900, false), [13]);
        assert_eq!(steps_of(BandFamily::DateAdded, [2026, 10, 31], 2026, false), Vec::<usize>::new());
        // Release: the year, then the decades, one step before the first decade
        assert_eq!(steps_of(BandFamily::Release, [2026, 10, 31], 1950, false), [1]);
        assert_eq!(steps_of(BandFamily::Release, [2026, 10, 31], 2026, false), Vec::<usize>::new());
        // the fixed ladders have no scale to change
        for family in [BandFamily::Rating, BandFamily::Duration, BandFamily::Unplayed, BandFamily::Plays] {
            assert!(steps_of(family, [2026, 10, 31], 1900, false).is_empty(), "{family:?}");
        }
    }

    #[test]
    fn an_ascending_rail_steps_at_the_same_seams() {
        // 2024, 2025, then the months: the step is before Jan (row 2), and Today ends the rail
        assert_eq!(steps_of(BandFamily::DateAdded, [2026, 10, 31], 2024, true), [2]);
        // the decades, then the year: the step is before 2026 (row 4, after 1990s..2020s)
        assert_eq!(steps_of(BandFamily::Release, [2026, 10, 31], 1990, true), [4]);
        assert_eq!(steps_of(BandFamily::DateAdded, [2026, 1, 1], 2024, true), Vec::<usize>::new());
    }

    #[test]
    fn every_family_in_every_shipped_language_is_distinct_and_fits_the_rail_beside_the_posters() {
        let _guard = plx_base::testlock::serial();
        let mut out = Vec::new();
        let (mut checked, mut squeezed) = (0, 0);
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let tag = language.tag();
            for family in BandFamily::ALL {
                let labels = ladder(family);
                assert!(labels.len() >= 5, "{tag} {family:?}: a family plans at least a ladder");
                for (index, text) in labels.iter().enumerate() {
                    checked += 1;
                    if text.is_empty() || text.contains('\u{fffd}') { out.push(format!("{tag} {family:?} #{index}: {text:?} is empty or broken")); }
                    if labels[..index].contains(text) { out.push(format!("{tag} {family:?}: {text:?} appears twice")); }
                    let width = ShippedMeasure.width(&std::ffi::CString::new(text.as_str()).unwrap(), plx_ui::theme::size::MICRO, false);
                    // a label gives pill air back, down to the least, to stay inside the posters' limit
                    if width > BAND_LABEL_LIMIT_W || width + 2.0 * band_pill_pad(width) > BAND_TRACK_LIMIT_W + 0.01 {
                        out.push(format!("{tag} {family:?}: {text:?} is {width} wide; its pill passes the {BAND_TRACK_LIMIT_W} the posters leave, even with only {BAND_PILL_PAD_MIN} px each side"));
                    }
                    if width + 2.0 * BAND_PILL_PAD > BAND_TRACK_LIMIT_W { squeezed += 1; }
                }
            }
        }
        assert!(checked > SHIPPED.len() * BandFamily::ALL.len() * 5, "the loop saw every family in every language ({checked} labels)");
        assert!(out.is_empty(), "labels that do not fit the rail:\n  {}", out.join("\n  "));
        // the clamp is in use: some shipped decade phrases are wider than the board's 12 px pill leaves
        assert!(squeezed > 0, "no label needs the squeezed pill; the native decade phrases were meant to");
    }

    #[test]
    fn a_duration_band_wears_the_languages_own_short_hour_unit() {
        let _guard = plx_base::testlock::serial();
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let one_hour = label(Mark::Minutes(60));
            let spelled = fmt::duration_minutes_in(plx_platform::i18n::current(), 60);
            assert!(spelled.starts_with(&one_hour), "{}: the rail's {one_hour:?} is not the start of the catalog's {spelled:?}", language.tag());
            assert_eq!(label(Mark::MinutesOrMore(180)), format!("{}+", label(Mark::Minutes(180))), "{}: the top band is the whole hours and a plus", language.tag());
            assert_eq!(label(Mark::BelowMinutes(60)), format!("<{one_hour}"), "{}", language.tag());
            assert!(label(Mark::Minutes(150)).contains('\u{bd}') && !label(Mark::Minutes(120)).contains('\u{bd}'), "{}: a half hour is a ½", language.tag());
        }
    }

    #[test]
    fn a_percent_band_follows_the_languages_own_percent_and_a_band_is_identified_by_its_token() {
        let _guard = plx_base::testlock::serial();
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            assert_eq!(label(Mark::Percent(95)), fmt::percent(95), "{}", language.tag());
            assert_eq!(label(Mark::BelowPercent(75)), format!("<{}", fmt::percent(75)), "{}", language.tag());
        }
        assert_ne!(identity(Mark::Count(1)), "1", "a band never shares a focus identity with the letter `1`");
        assert_ne!(identity(Mark::Count(1)), identity(Mark::CountOrMore(1)));
        assert_ne!(identity(Mark::Percent(75)), identity(Mark::BelowPercent(75)));
        // …nor a year with a count of the same number, or a decade with the year that opens it
        assert_ne!(identity(Mark::Year(2025)), identity(Mark::Count(2025)));
        assert_ne!(identity(Mark::Year(2020)), identity(Mark::Decade(2020)));
        assert_ne!(identity(Mark::Month(1)), identity(Mark::Count(1)));
    }
}
