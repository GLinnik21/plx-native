//! **Under posters** — what a focused poster's caption band shows and how tall that band is.
//!
//! One global setting (Settings > Libraries > Under posters): a Title switch (default Off) and a
//! Details picker. Only the FOCUSED row opens its band: 80 px for one line, 112 px for two,
//! nothing when the policy shows nothing; non-focused rows keep their 40 px spacing.
//!
//! CONTRACT FREEZE: the types and the two function signatures are what the library, collection
//! and settings packages compile against. [`band_h`] is final. [`caption_lines`] is a first cut
//! (no pluralised or per-locale strings yet); P3 completes it in place with the
//! `captions.json` catalog keys and the locale-correct plurals, and may change nothing else here.

use plx_platform::i18n::LocaleContext;

/// Band height with nothing to show.
pub const BAND_NONE: f32 = 0.0;
/// Band height for one caption line.
pub const BAND_ONE_LINE: f32 = 80.0;
/// Band height for two caption lines.
pub const BAND_TWO_LINES: f32 = 112.0;

/// Which facts the details line carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailsKind {
    /// "2019 · 2h 14m" (default).
    #[default]
    YearLength,
    Genre,
    /// "4K · HDR".
    Video,
    /// "PG-13 · 7.8".
    AgeRatingScore,
    None,
}

/// The setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionPolicy {
    /// Show the title above the details.
    pub title: bool,
    pub details: DetailsKind,
}

impl CaptionPolicy {
    pub const DEFAULT: CaptionPolicy = CaptionPolicy { title: false, details: DetailsKind::YearLength };

    /// How many lines this policy can show at most (a missing fact may leave fewer).
    pub fn max_lines(&self) -> usize {
        usize::from(self.title) + usize::from(self.details != DetailsKind::None)
    }
}

/// The focused caption band's height: 0 / 80 / 112.
pub fn band_h(policy: &CaptionPolicy) -> f32 {
    match policy.max_lines() {
        0 => BAND_NONE,
        1 => BAND_ONE_LINE,
        _ => BAND_TWO_LINES,
    }
}

/// Everything a caption can be built from, as plain values; the caller copies them from its row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptionFacts {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub duration_min: Option<u32>,
    /// A partly-watched item's position, for "N min left".
    pub view_offset_min: Option<u32>,
    pub genre: Option<String>,
    pub video_resolution: Option<String>,
    pub hdr: bool,
    pub content_rating: Option<String>,
    /// 0..10.
    pub audience_rating: Option<f32>,
    pub leaf_count: Option<u32>,
    pub viewed_leaf_count: Option<u32>,
    pub season_count: Option<u32>,
    pub child_count: Option<u32>,
    /// A collection's subtype (`movie`, `show`, …).
    pub subtype: Option<String>,
    pub is_show: bool,
}

/// The caption's lines for `facts` under `policy`: `[title, details]` with absent lines `None`.
/// A line with nothing to say is `None`, so a missing fact never leaves an empty string behind.
pub fn caption_lines(policy: &CaptionPolicy, facts: &CaptionFacts, _locale: &LocaleContext) -> [Option<String>; 2] {
    let title = policy.title.then(|| facts.title.clone()).flatten().filter(|t| !t.is_empty());
    let join = |parts: Vec<String>| (!parts.is_empty()).then(|| parts.join(" \u{b7} "));
    let details = match policy.details {
        DetailsKind::None => None,
        DetailsKind::YearLength => join(
            facts.year.filter(|y| *y > 0).map(|y| y.to_string()).into_iter()
                .chain(facts.duration_min.filter(|m| *m > 0).map(|m| crate::fmt::dur_short(i64::from(m) * 60_000)))
                .collect(),
        ),
        DetailsKind::Genre => facts.genre.clone().filter(|g| !g.is_empty()),
        DetailsKind::Video => join(
            facts.video_resolution.clone().into_iter().filter(|r| !r.is_empty())
                .chain(facts.hdr.then(|| "HDR".to_string())).collect(),
        ),
        DetailsKind::AgeRatingScore => join(
            facts.content_rating.clone().into_iter().filter(|r| !r.is_empty())
                .chain(facts.audience_rating.filter(|s| *s > 0.0).map(|s| format!("{s:.1}"))).collect(),
        ),
    };
    [title, details]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_year_length_without_title_and_one_line_tall() {
        let p = CaptionPolicy::default();
        assert_eq!(p, CaptionPolicy::DEFAULT);
        assert!(!p.title);
        assert_eq!(p.details, DetailsKind::YearLength);
        assert_eq!(band_h(&p), 80.0);
    }

    #[test]
    fn band_height_is_zero_for_nothing_80_for_one_line_112_for_two() {
        let none = CaptionPolicy { title: false, details: DetailsKind::None };
        let title_only = CaptionPolicy { title: true, details: DetailsKind::None };
        let both = CaptionPolicy { title: true, details: DetailsKind::Genre };
        assert_eq!([band_h(&none), band_h(&title_only), band_h(&both)], [0.0, 80.0, 112.0]);
    }

    #[test]
    fn lines_follow_the_policy_and_skip_missing_facts() {
        let facts = CaptionFacts { title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), ..Default::default() };
        let l = plx_platform::i18n::current();
        let p = CaptionPolicy { title: true, details: DetailsKind::Genre };
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), Some("Crime".into())]);
        let p = CaptionPolicy { title: false, details: DetailsKind::Video };
        assert_eq!(caption_lines(&p, &facts, l), [None, None]);
        let p = CaptionPolicy { title: false, details: DetailsKind::YearLength };
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
    }
}
