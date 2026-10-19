//! **One vocabulary for the Sort & Filter panels** (design review S17, N1). The Library's sort rows
//! come from the server's per-library list and the Watchlist's from its own four keys; both name a
//! fact (a title, a date, a rating), and a fact has ONE name and ONE way to say which end comes
//! first, whichever page asks.
//!
//! - [`SortFact::of_pms_key`] (and the Watchlist's `SortKey::fact`) say which fact a key is; [`SortFact::label`]
//!   is its sentence-case name ("Critic rating" is not a second word for "Rating": the Library's
//!   `rating` key and the Watchlist's rating are the same fact).
//! - [`Axis::direction_word`] is the checked row's value ("A–Z", "Newest first"): a word says what
//!   is set, where a chevron inside a row reads as "opens".
//!
//! A key this table does not know keeps the server's title as its label (marked a server label by
//! the menu) and, if its axis is known, still gets the direction word. The table names every key
//! `docs/plex-openapi.json` lists for a movie library (`titleSort`, `originallyAvailableAt`,
//! `rating`, `audienceRating`, `duration`, `addedAt`, `lastViewedAt`, `mediaHeight`, `random`) and the
//! app's own `viewCount`: the server's titles are English Title Case whatever the language.

use plx_platform::i18n::msg;

/// What an ordering runs along, which decides how its two ends are worded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Names: A to Z, Z to A.
    Alphabetical,
    /// Dates: oldest first, newest first.
    Chronological,
    /// Counts and ratings: lowest first, highest first.
    Magnitude,
}

impl Axis {
    /// The word for the order: `descending` is the direction the page asks the server for (`:desc`).
    pub fn direction_word(self, descending: bool) -> &'static str {
        match (self, descending) {
            (Axis::Alphabetical, false) => msg::sort_facts_a_to_z(),
            (Axis::Alphabetical, true) => msg::sort_facts_z_to_a(),
            (Axis::Chronological, false) => msg::sort_facts_oldest_first(),
            (Axis::Chronological, true) => msg::sort_facts_newest_first(),
            (Axis::Magnitude, false) => msg::sort_facts_lowest_first(),
            (Axis::Magnitude, true) => msg::sort_facts_highest_first(),
        }
    }

    /// The axis of a PMS sort key: the table's fact's own, or for a key the table has no label for
    /// but whose direction is still known (`userRating`) the key's. A compound key
    /// (`show.titleSort,season.index`) is read by its first term. A random order has none.
    pub fn of_pms_key(key: &str) -> Option<Axis> {
        if let Some(fact) = SortFact::of_pms_key(key) {
            return fact.axis();
        }
        match pms_term(key) {
            "userRating" => Some(Axis::Magnitude),
            _ => None,
        }
    }
}

/// The first term of a PMS sort key without its table prefix: `show.titleSort,season.index` -> `titleSort`.
fn pms_term(key: &str) -> &str {
    let first = key.split(',').next().unwrap_or(key);
    first.rsplit('.').next().unwrap_or(first)
}

/// A fact a list can be ordered by, named once for every page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortFact {
    Title,
    DateAdded,
    ReleaseDate,
    Rating,
    AudienceRating,
    /// When the viewer last played it (`lastViewedAt`).
    LastViewed,
    /// How many times it was played (`viewCount`, which the app offers itself where the server does not).
    Plays,
    Duration,
    /// The video's height (`mediaHeight`).
    Resolution,
    Random,
}

impl SortFact {
    pub const ALL: [SortFact; 10] = [Self::Title, Self::DateAdded, Self::ReleaseDate, Self::Rating, Self::AudienceRating,
        Self::LastViewed, Self::Plays, Self::Duration, Self::Resolution, Self::Random];

    /// The fact a PMS `Sort` key orders by, if the table names it.
    pub fn of_pms_key(key: &str) -> Option<SortFact> {
        match pms_term(key) {
            "titleSort" | "title" => Some(Self::Title),
            "addedAt" => Some(Self::DateAdded),
            "originallyAvailableAt" => Some(Self::ReleaseDate),
            "rating" => Some(Self::Rating),
            "audienceRating" => Some(Self::AudienceRating),
            "lastViewedAt" => Some(Self::LastViewed),
            "viewCount" => Some(Self::Plays),
            "duration" => Some(Self::Duration),
            "mediaHeight" => Some(Self::Resolution),
            "random" => Some(Self::Random),
            _ => None,
        }
    }

    /// The name every panel shows for this fact.
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => msg::sort_facts_title(),
            Self::DateAdded => msg::sort_facts_date_added(),
            Self::ReleaseDate => msg::sort_facts_release_date(),
            Self::Rating => msg::sort_facts_rating(),
            Self::AudienceRating => msg::sort_facts_audience_rating(),
            Self::LastViewed => msg::sort_facts_last_viewed(),
            Self::Plays => msg::browse_library_plays(),
            Self::Duration => msg::widgets_tracks_duration(),
            Self::Resolution => msg::widgets_tracks_resolution(),
            Self::Random => msg::sort_facts_random(),
        }
    }

    /// What its ends are ordered along; `None` for a random order, which has no ends.
    pub fn axis(self) -> Option<Axis> {
        match self {
            Self::Title => Some(Axis::Alphabetical),
            Self::DateAdded | Self::ReleaseDate | Self::LastViewed => Some(Axis::Chronological),
            Self::Rating | Self::AudienceRating | Self::Plays | Self::Duration | Self::Resolution => Some(Axis::Magnitude),
            Self::Random => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};

    /// **A fact has one name and one pair of direction words, in sentence case.** The Library's
    /// "Critic Rating" and the Watchlist's "Rating" were the same fact in two words.
    #[test]
    fn a_fact_has_one_sentence_case_name_and_its_directions_are_words() {
        let _g = language_on_this_thread_for_test(Preference::En);
        let labels: Vec<_> = SortFact::ALL.iter().map(|f| f.label()).collect();
        assert_eq!(labels, ["Title", "Date added", "Release date", "Score", "Audience score", "Date viewed", "Plays",
            "Duration", "Resolution", "Random"]);
        for label in &labels {
            let rest = label.chars().skip(1).collect::<String>();
            assert_eq!(rest, rest.to_lowercase(), "{label:?}: sentence case, only the first letter is a capital");
        }
        for (key, fact) in [("titleSort", SortFact::Title), ("show.titleSort,season.index:nullsLast", SortFact::Title),
            ("addedAt", SortFact::DateAdded), ("originallyAvailableAt", SortFact::ReleaseDate),
            ("rating", SortFact::Rating), ("audienceRating", SortFact::AudienceRating), ("lastViewedAt", SortFact::LastViewed),
            ("viewCount", SortFact::Plays), ("duration", SortFact::Duration), ("mediaHeight", SortFact::Resolution),
            ("random", SortFact::Random)] {
            assert_eq!(SortFact::of_pms_key(key), Some(fact), "{key}");
        }
        assert_eq!(SortFact::of_pms_key("userRating"), None, "a key the table does not name keeps the server's title");
        let words = |axis: Axis| (axis.direction_word(false), axis.direction_word(true));
        assert_eq!(words(Axis::Alphabetical), ("A\u{2013}Z", "Z\u{2013}A"));
        assert_eq!(words(Axis::Chronological), ("Oldest first", "Newest first"));
        assert_eq!(words(Axis::Magnitude), ("Lowest first", "Highest first"));
        assert_eq!(Axis::of_pms_key("lastViewedAt"), Some(Axis::Chronological), "a date says which end is first");
        assert_eq!(Axis::of_pms_key("userRating"), Some(Axis::Magnitude), "an unlabelled rating still says which end is first");
        assert_eq!(Axis::of_pms_key("random"), None);
    }

    /// **A key the server titles in English is named by the table in every language** (design review
    /// S17): "Date Viewed" (Title Case) and "Randomly" were the server's own strings, English even in
    /// `be`, `es` and `fr`. Every key `docs/plex-openapi.json` lists for a movie library, and the
    /// app's own `viewCount`, takes the table's word and is marked the app's own text.
    #[test]
    fn every_key_a_server_titles_in_english_takes_the_tables_word_in_every_language() {
        use crate::browse::SortEntry;
        use plx_platform::i18n::SHIPPED;
        // (key, the server's title, the table's English word)
        let keys = [("lastViewedAt", "Date Viewed", "Date viewed"), ("random", "Randomly", "Random"),
            ("viewCount", "Plays", "Plays"), ("duration", "Duration", "Duration"), ("mediaHeight", "Resolution", "Resolution")];
        for language in SHIPPED {
            let _g = language_on_this_thread_for_test(language);
            for (key, server, english) in keys {
                let entry = SortEntry { key: key.into(), desc_key: String::new(), title: server.into(), default_desc: true };
                assert!(entry.label_is_ours(), "{} {key}: the table names it", language.tag());
                if language.tag() == "en" {
                    assert_eq!(entry.label(), english, "{key}");
                } else if server != "Plays" {
                    assert_ne!(entry.label(), server, "{} {key}: the server's English title leaked", language.tag());
                }
            }
        }
        let _g = language_on_this_thread_for_test(Preference::En);
        let entry = |key: &str| SortEntry { key: key.into(), desc_key: String::new(), title: "x".into(), default_desc: false };
        assert_eq!(entry("rating").label(), "Score", "one word for the critic number: the Under posters picker's");
        assert_eq!(entry("audienceRating").label(), "Audience score");
        assert_eq!(entry("random").direction_word(true), None, "a random order has no ends");
        assert_eq!(entry("lastViewedAt").direction_word(true), Some("Newest first"));
        assert_eq!(entry("viewCount").direction_word(true), Some("Highest first"));
    }
}
