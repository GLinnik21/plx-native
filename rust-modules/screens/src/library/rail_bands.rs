//! The words of a value sort's rail: the label each band wears (`95%`, `<75%`, `2½h`, `3h+`, `10`,
//! `12+`) and the identity focus keeps for it. The data layer names a band by a [`Mark`] token; what it
//! is called in the viewer's language is decided here, from the catalog and the locale's own number and
//! percent formats, never from a list of languages.
//!
//! Only the fixed ladders (rating, duration, unplayed, plays) are drawn yet: the calendar marks
//! (`Today`, a month, a year, a decade) wait for the job that asks the data layer for them
//! (`BandFamily::rail_ready`), and have no label. The shared rules:
//!
//! - **A percent is [`fmt::percent`]**, so the sign is placed as the catalog places it, under a poster
//!   and on the rail alike (`git grep '"core.percent"' -- 'locales/*/core.json'` shows which languages
//!   set it off with a no-break space).
//! - **An hour is the language's own short hour** (`library_rail.hours`: `h`, `ó`, `u`, `ч`, `г`,
//!   `godz.`), spaced as `core.duration_hours` spaces it, and the half hour is a `½` after the number.
//! - **`<` is written by the code, and so is the `+` of the count ladders** (`12+`, `10+`); the Duration
//!   ladder's `3h+` is `library_rail.hours_or_more`, a catalog string, so a language can put the `+` after
//!   its own unit.
//!
//! [`Mark`]: plx_data::browse::bands::Mark
use plx_data::browse::bands::Mark;
use plx_platform::i18n::msg;
use plx_ui::fmt;

/// The label of a band, in the language the UI is in. A mark no ladder drawn today produces (a
/// calendar band) reads as its debug token, after a debug build's `debug_assert!` has fired first:
/// unreachable while `BandFamily::rail_ready` keeps those families off, and visibly wrong, not blank,
/// the day one slips through.
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
        Mark::Today | Mark::Month(_) | Mark::Year(_) | Mark::Decade(_) => {
            debug_assert!(false, "{mark:?} has no label yet: its family is not `rail_ready`");
            format!("{mark:?}")
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::layout::{BAND_PILL_PAD, BAND_TRACK_LIMIT_W};
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_data::browse::bands::BandFamily;
    use plx_machine::machine::Measure;
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};

    const FIXED: [BandFamily; 4] = [BandFamily::Rating, BandFamily::Duration, BandFamily::Unplayed, BandFamily::Plays];

    fn ladder(family: BandFamily) -> Vec<String> {
        family.fixed_ladder().expect("a fixed ladder").iter().map(|band| label(band.mark())).collect()
    }

    #[test]
    fn the_english_ladders_read_as_the_boards_do() {
        let _guard = plx_base::testlock::serial();
        let english = SHIPPED.into_iter().find(|language| language.tag() == "en").expect("English ships");
        let _language = language_on_this_thread_for_test(english);
        assert_eq!(ladder(BandFamily::Rating), ["95%", "90%", "85%", "80%", "75%", "<75%"]);
        assert_eq!(ladder(BandFamily::Duration), ["3h+", "2\u{bd}h", "2h", "1\u{bd}h", "1h", "<1h"]);
        assert_eq!(ladder(BandFamily::Unplayed), ["12+", "10", "8", "6", "4", "2", "1"]);
        assert_eq!(ladder(BandFamily::Plays), ["10+", "5", "3", "2", "1"]);
    }

    #[test]
    fn every_ladder_in_every_shipped_language_is_distinct_and_fits_the_rail_beside_the_posters() {
        let _guard = plx_base::testlock::serial();
        let mut out = Vec::new();
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let tag = language.tag();
            for family in FIXED {
                let labels = ladder(family);
                for (index, text) in labels.iter().enumerate() {
                    if text.is_empty() || text.contains('\u{fffd}') { out.push(format!("{tag} {family:?} #{index}: {text:?} is empty or broken")); }
                    if labels[..index].contains(text) { out.push(format!("{tag} {family:?}: {text:?} appears twice")); }
                    let width = ShippedMeasure.width(&std::ffi::CString::new(text.as_str()).unwrap(), plx_ui::theme::size::MICRO, false);
                    if width + 2.0 * BAND_PILL_PAD > BAND_TRACK_LIMIT_W {
                        out.push(format!("{tag} {family:?}: {text:?} is {width} wide; its pill passes the {BAND_TRACK_LIMIT_W} the posters leave"));
                    }
                }
            }
        }
        assert!(out.is_empty(), "labels that do not fit the rail:\n  {}", out.join("\n  "));
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
    }
}
