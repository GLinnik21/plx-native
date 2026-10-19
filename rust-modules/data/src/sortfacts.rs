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
//! the menu) and, if its axis is known, still gets the direction word.

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

    /// The axis of a PMS sort key the table has no label for but whose direction is still known
    /// ("Date Viewed", "Plays"). A compound key (`show.titleSort,season.index`) is read by its first term.
    pub fn of_pms_key(key: &str) -> Option<Axis> {
        if let Some(fact) = SortFact::of_pms_key(key) {
            return Some(fact.axis());
        }
        match pms_term(key) {
            "lastViewedAt" => Some(Axis::Chronological),
            "viewCount" | "userRating" | "duration" => Some(Axis::Magnitude),
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
}

impl SortFact {
    pub const ALL: [SortFact; 5] = [Self::Title, Self::DateAdded, Self::ReleaseDate, Self::Rating, Self::AudienceRating];

    /// The fact a PMS `Sort` key orders by, if the table names it.
    pub fn of_pms_key(key: &str) -> Option<SortFact> {
        match pms_term(key) {
            "titleSort" | "title" => Some(Self::Title),
            "addedAt" => Some(Self::DateAdded),
            "originallyAvailableAt" => Some(Self::ReleaseDate),
            "rating" => Some(Self::Rating),
            "audienceRating" => Some(Self::AudienceRating),
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
        }
    }

    pub fn axis(self) -> Axis {
        match self {
            Self::Title => Axis::Alphabetical,
            Self::DateAdded | Self::ReleaseDate => Axis::Chronological,
            Self::Rating | Self::AudienceRating => Axis::Magnitude,
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
        assert_eq!(labels, ["Title", "Date added", "Release date", "Rating", "Audience rating"]);
        for label in &labels {
            let rest = label.chars().skip(1).collect::<String>();
            assert_eq!(rest, rest.to_lowercase(), "{label:?}: sentence case, only the first letter is a capital");
        }
        for (key, fact) in [("titleSort", SortFact::Title), ("show.titleSort,season.index:nullsLast", SortFact::Title),
            ("addedAt", SortFact::DateAdded), ("originallyAvailableAt", SortFact::ReleaseDate),
            ("rating", SortFact::Rating), ("audienceRating", SortFact::AudienceRating)] {
            assert_eq!(SortFact::of_pms_key(key), Some(fact), "{key}");
        }
        assert_eq!(SortFact::of_pms_key("random"), None, "a key the table does not name keeps the server's title");
        let words = |axis: Axis| (axis.direction_word(false), axis.direction_word(true));
        assert_eq!(words(Axis::Alphabetical), ("A\u{2013}Z", "Z\u{2013}A"));
        assert_eq!(words(Axis::Chronological), ("Oldest first", "Newest first"));
        assert_eq!(words(Axis::Magnitude), ("Lowest first", "Highest first"));
        assert_eq!(Axis::of_pms_key("lastViewedAt"), Some(Axis::Chronological), "an unlabelled date still says which end is first");
        assert_eq!(Axis::of_pms_key("random"), None);
    }
}
