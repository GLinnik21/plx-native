//! **The facts a details line under a poster can carry**, and the set of them a viewer ticked
//! (Settings > Libraries > Under posters > Movie details / TV show details).
//!
//! One definition for the two layers that must agree and may not name each other: the data layer
//! keeps the set per profile (`plx_plex::plex::session::UnderPosters`) and the ui layer words the
//! line from it (`plx_ui::caption_policy`), the way [`crate::hold`] is the one cut a paged store and
//! its band both apply.
//!
//! A set holds **at most [`DetailParts::MAX`] parts, shown in a FIXED order** (the order of
//! [`DetailPart`]'s variants, so a set can never be reordered by the order it was ticked in). A
//! movie offers [`DetailPart::MOVIE`], a TV show [`DetailPart::SHOW`] (no video quality, no length;
//! seasons instead); [`DetailParts::kept`] holds a set to what a kind offers and to the maximum, and
//! is the one place a stored or unpacked value is made valid.

/// One fact a details line can carry. The declaration order IS the line's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetailPart {
    /// The release year (a show's first year): "2024".
    Year,
    /// A movie's running time: "1h 52m".
    Length,
    /// A show's season count: "6 seasons".
    Seasons,
    /// The first genre: "Drama".
    Genre,
    /// A movie's video quality: "4K · HDR".
    VideoQuality,
    /// The age rating: "PG-13".
    AgeRating,
    /// The audience score: "8.1".
    Score,
}

impl DetailPart {
    /// What a movie offers, in the line's order.
    pub const MOVIE: [DetailPart; 6] = [
        DetailPart::Year,
        DetailPart::Length,
        DetailPart::Genre,
        DetailPart::VideoQuality,
        DetailPart::AgeRating,
        DetailPart::Score,
    ];
    /// What a TV show offers, in the line's order.
    pub const SHOW: [DetailPart; 5] =
        [DetailPart::Year, DetailPart::Seasons, DetailPart::Genre, DetailPart::AgeRating, DetailPart::Score];

    /// The part's bit in a [`DetailParts`].
    pub const fn bit(self) -> u8 {
        1 << self as u8
    }

    /// The spelling the stored setting uses.
    pub const fn name(self) -> &'static str {
        match self {
            DetailPart::Year => "year",
            DetailPart::Length => "length",
            DetailPart::Seasons => "seasons",
            DetailPart::Genre => "genre",
            DetailPart::VideoQuality => "video_quality",
            DetailPart::AgeRating => "age_rating",
            DetailPart::Score => "score",
        }
    }

    /// The part a stored spelling names; `None` for one this build does not know.
    pub fn from_name(name: &str) -> Option<DetailPart> {
        [DetailPart::Year, DetailPart::Length, DetailPart::Seasons, DetailPart::Genre, DetailPart::VideoQuality, DetailPart::AgeRating, DetailPart::Score]
            .into_iter()
            .find(|p| p.name() == name)
    }
}

/// A set of [`DetailPart`]s: one bit per part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DetailParts(u8);

impl DetailParts {
    /// The most parts a line carries: four fit under a poster.
    pub const MAX: usize = 4;
    /// No details line.
    pub const NONE: DetailParts = DetailParts(0);
    /// Every bit a part can occupy.
    const ALL: u8 = (1 << 7) - 1;
    /// The default movie line: "2024 · 1h 52m".
    pub const MOVIE_DEFAULT: DetailParts = DetailParts(DetailPart::Year.bit() | DetailPart::Length.bit());
    /// The default TV show line: "2019 · 6 seasons".
    pub const SHOW_DEFAULT: DetailParts = DetailParts(DetailPart::Year.bit() | DetailPart::Seasons.bit());

    /// The set of exactly `parts`, unchecked against a kind or the maximum ([`Self::kept`] does that).
    pub fn of(parts: &[DetailPart]) -> DetailParts {
        DetailParts(parts.iter().fold(0, |bits, p| bits | p.bit()))
    }

    /// The raw bits, for packing.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set the raw `bits` hold (a bit no part owns is dropped).
    pub const fn from_bits(bits: u8) -> DetailParts {
        DetailParts(bits & Self::ALL)
    }

    pub const fn contains(self, part: DetailPart) -> bool {
        self.0 & part.bit() != 0
    }

    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The parts, in the line's fixed order.
    pub fn iter(self) -> impl Iterator<Item = DetailPart> {
        [DetailPart::Year, DetailPart::Length, DetailPart::Seasons, DetailPart::Genre, DetailPart::VideoQuality, DetailPart::AgeRating, DetailPart::Score]
            .into_iter()
            .filter(move |p| self.contains(*p))
    }

    /// Whether `part` can be ticked now: it already is, or there is room.
    pub fn fits(self, part: DetailPart) -> bool {
        self.contains(part) || self.len() < Self::MAX
    }

    /// This set with `part` unticked if it is ticked, else ticked if it fits (else unchanged).
    pub fn toggled(self, part: DetailPart) -> DetailParts {
        if self.contains(part) {
            DetailParts(self.0 & !part.bit())
        } else if self.fits(part) {
            DetailParts(self.0 | part.bit())
        } else {
            self
        }
    }

    /// **The set held to what a kind offers and to [`Self::MAX`]**: parts outside `offered` are
    /// dropped, and a set over the maximum keeps its FIRST [`Self::MAX`] parts in the line's order
    /// (deterministic, whatever produced it: a stored value from a later build, a hand-edited file).
    pub fn kept(self, offered: &[DetailPart]) -> DetailParts {
        let mut kept = DetailParts::NONE;
        for part in self.iter().filter(|p| offered.contains(p)) {
            if kept.len() < Self::MAX {
                kept.0 |= part.bit();
            }
        }
        kept
    }

    /// [`Self::kept`] for a movie.
    pub fn for_movies(self) -> DetailParts {
        self.kept(&DetailPart::MOVIE)
    }

    /// [`Self::kept`] for a TV show.
    pub fn for_shows(self) -> DetailParts {
        self.kept(&DetailPart::SHOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DetailPart as P;

    #[test]
    fn the_order_is_fixed_whatever_order_parts_were_ticked_in() {
        let a = DetailParts::NONE.toggled(P::Score).toggled(P::Year).toggled(P::Genre);
        let b = DetailParts::NONE.toggled(P::Year).toggled(P::Genre).toggled(P::Score);
        assert_eq!(a, b);
        assert_eq!(a.iter().collect::<Vec<_>>(), [P::Year, P::Genre, P::Score]);
    }

    #[test]
    fn a_fifth_part_does_not_fit_and_an_unticked_one_makes_room() {
        let four = DetailParts::of(&[P::Year, P::Length, P::Genre, P::VideoQuality]);
        assert_eq!(four.len(), DetailParts::MAX);
        assert!(!four.fits(P::Score) && four.fits(P::Year));
        assert_eq!(four.toggled(P::Score), four, "OK on a part that does not fit does nothing");
        assert!(four.toggled(P::Genre).fits(P::Score));
    }

    #[test]
    fn kept_drops_what_a_kind_does_not_offer_and_clamps_to_the_first_four() {
        let all = DetailParts::from_bits(0xFF);
        assert_eq!(all.len(), 7);
        assert_eq!(all.for_movies().iter().collect::<Vec<_>>(), [P::Year, P::Length, P::Genre, P::VideoQuality]);
        assert_eq!(all.for_shows().iter().collect::<Vec<_>>(), [P::Year, P::Seasons, P::Genre, P::AgeRating]);
        let foreign = DetailParts::of(&[P::Seasons, P::Length]);
        assert_eq!(foreign.for_movies(), DetailParts::of(&[P::Length]));
        assert_eq!(foreign.for_shows(), DetailParts::of(&[P::Seasons]));
    }

    #[test]
    fn names_round_trip_and_the_defaults_are_the_old_year_and_length() {
        for p in DetailParts::from_bits(0xFF).iter() {
            assert_eq!(P::from_name(p.name()), Some(p));
        }
        assert_eq!(P::from_name("future_part"), None);
        assert_eq!(DetailParts::MOVIE_DEFAULT.iter().collect::<Vec<_>>(), [P::Year, P::Length]);
        assert_eq!(DetailParts::SHOW_DEFAULT.iter().collect::<Vec<_>>(), [P::Year, P::Seasons]);
        assert_eq!(DetailParts::from_bits(DetailParts::MOVIE_DEFAULT.bits()), DetailParts::MOVIE_DEFAULT);
    }
}
