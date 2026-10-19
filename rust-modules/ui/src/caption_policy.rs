//! **Under posters** — what a focused poster's caption band shows and how tall that band is.
//!
//! One global setting (Settings > Libraries > Under posters): a Title switch (default Off) and a
//! Details picker. Only the FOCUSED row opens its band: 80 px for one line, 112 px for two,
//! nothing when the policy shows nothing; non-focused rows keep their 40 px spacing.
//!
//! **A line the setting turns on is never drawn empty**, whatever the tile is. The title line is the
//! name a reader knows the tile by (a season by its SHOW, an episode by its own title). The details
//! line is the picked kind's facts, and a tile that has none of them (a season has no length and
//! often no genre or rating) says its identity line instead (a season "Season 6", an episode its
//! "S1 • E8" address, a show or movie its year, a collection its size), so a band the policy
//! opens is never open over nothing. Under Nothing with Title Off no band opens at all.
//!
//! The Library grid, the Collections page, the Watchlist, Home's shelves and the Library's Recommended shelves (the screens build the focused
//! block through `registry::captions`, and hand a [`crate::cards::Shelf`] / grid the policy so the
//! room it reserves is [`band_h`] of the same value); Search, the person page and Detail's Related and
//! Extras shelves are not on the policy and keep the legacy band. The types and the two function signatures are
//! what the library, collection and settings packages compile against. [`band_h`] sizes the band; [`caption_lines`] words the lines in the
//! given locale, with the catalog's plurals.

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

    /// How many lines this policy can show at most (a tile with no name, or no fact and no identity,
    /// may leave fewer).
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
    /// A partly-watched item's position, for "Nm left".
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
    /// An episode's `(season, episode)` numbers (`0` for one the server did not number), which its
    /// default details line says in place of a year and length.
    pub episode: Option<(u32, u32)>,
    /// A SEASON tile's `(number, its own title)`: `0` for a season the server did not number, and the
    /// title the server gave it ("Season 6", "Specials"). `title` then holds the name of its SHOW, the
    /// name a reader knows the tile by, and this is the season's identity line.
    pub season: Option<(u32, String)>,
}

/// The caption's lines for `facts` under `policy`: `[title, details]` with absent lines `None`.
///
/// **One rule for every kind of tile: a line the setting turns on is never drawn empty.** The title
/// line is `facts.title`, the name a reader knows the tile by (the caller puts a season's SHOW there).
/// The details line is the picked kind's facts, and a tile that has none of them (a season has no
/// length and often no genre or rating, a movie may have no genre) says its identity line instead
/// ([`identity_line`]), so a band the policy opens is never open over nothing. Under
/// [`DetailsKind::None`] there is no details line and no fallback; a line is `None` only when the
/// setting turns it off (or the tile has no name or identity at all), never for want of one picked fact.
///
/// Every word on a line comes from the catalog in `locale` (the shared `core.*` / `browse.*`
/// duration, count and plural keys plus `captions.new_episodes`); the only literals are the
/// technical marks `HDR`, `4K`, `1080p` and the middle dot.
pub fn caption_lines(policy: &CaptionPolicy, facts: &CaptionFacts, locale: &LocaleContext) -> [Option<String>; 2] {
    let title = policy.title.then(|| facts.title.clone()).flatten().filter(|t| !t.is_empty());
    let picked = match policy.details {
        DetailsKind::None => return [title, None],
        DetailsKind::YearLength => year_length(facts, locale),
        DetailsKind::Genre => facts.genre.clone().filter(|g| !g.is_empty()),
        DetailsKind::Video => join(
            facts.video_resolution.as_deref().and_then(resolution_mark).into_iter()
                .chain(facts.hdr.then(|| "HDR".to_string())).collect(),
        ),
        DetailsKind::AgeRatingScore => join(
            facts.content_rating.clone().into_iter().filter(|r| !r.is_empty())
                .chain(facts.audience_rating.filter(|s| *s > 0.0)
                    .map(|s| locale.decimal((s * 10.0).round() as i64, 1))).collect(),
        ),
    };
    [title, picked.or_else(|| identity_line(facts, locale))]
}

fn join(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// PMS `videoResolution` ("4k", "1080", "720", "sd") as the mark a viewer knows.
fn resolution_mark(raw: &str) -> Option<String> {
    let r = raw.trim().to_ascii_lowercase();
    match r.as_str() {
        "" => None,
        "4k" => Some("4K".to_string()),
        "sd" => Some("SD".to_string()),
        n if n.bytes().all(|b| b.is_ascii_digit()) => Some(format!("{n}p")),
        other => Some(other.to_string()),
    }
}

/// An episode's address, "S1 • E8" (or "E8" for one the server did not give a season).
fn episode_address(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    let (season, ep) = f.episode.filter(|(_, ep)| *ep > 0)?;
    Some(if season > 0 {
        msg::widgets_card_season_episode_in(cx, i64::from(ep), i64::from(season))
    } else {
        msg::widgets_card_episode_in(cx, i64::from(ep))
    })
}

/// The default details line, by kind: a collection is its size, an episode its address, a show its
/// season count (or its unwatched new episodes), a started item the time it has left, anything else year and length
/// (a season has no length: its year, or nothing, which [`identity_line`] then answers).
fn year_length(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    if let Some(sub) = f.subtype.as_deref() {
        return Some(crate::fmt::collection_count_in(cx, Some(sub), i64::from(f.child_count?)));
    }
    if let Some(address) = episode_address(f, cx) {
        return Some(address);
    }
    let year = f.year.filter(|y| *y > 0).map(|y| y.to_string());
    if f.is_show {
        let unwatched = f.leaf_count.zip(f.viewed_leaf_count).map(|(l, v)| l.saturating_sub(v)).filter(|n| *n > 0);
        if let Some(n) = unwatched {
            return Some(msg::captions_new_episodes_in(cx, i64::from(n)));
        }
        let seasons = f.season_count.or(f.child_count).filter(|n| *n > 0)
            .map(|n| msg::browse_detail_seasons_in(cx, i64::from(n)));
        return join(year.into_iter().chain(seasons).collect());
    }
    let dur = f.duration_min.filter(|m| *m > 0);
    if let (Some(d), Some(off)) = (dur, f.view_offset_min.filter(|m| *m > 0)) {
        if off < d {
            return Some(crate::fmt::time_left_minutes_in(cx, i64::from(d - off)));
        }
    }
    let length = dur.map(|d| crate::fmt::duration_minutes_in(cx, i64::from(d)));
    join(year.into_iter().chain(length).collect())
}

/// What a tile IS, in one short line, for a tile the picked details kind has nothing to say about:
/// a collection its size, an episode its address ("S1 • E8"), a season its own title ("Season 6",
/// "Specials", or "Season N" from the catalog when the server named it nothing), anything else its
/// year. `None` for a tile with none of these.
fn identity_line(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    if let Some(sub) = f.subtype.as_deref() {
        return Some(crate::fmt::collection_count_in(cx, Some(sub), i64::from(f.child_count?)));
    }
    if let Some(address) = episode_address(f, cx) {
        return Some(address);
    }
    if let Some((number, own)) = &f.season {
        return if !own.is_empty() {
            Some(own.clone())
        } else {
            (*number > 0).then(|| msg::browse_detail_season_number_in(cx, i64::from(*number)))
        };
    }
    f.year.filter(|y| *y > 0).map(|y| y.to_string())
}

/// The caption's text shadow (`0 1px 8px` black 35%): the glyph path has no blur, so it is set as
/// an offset copy of the line, as `collection_tile`'s fan name is.
pub const CAPTION_SHADOW: [f32; 4] = [0.0, 0.0, 0.0, 0.35];
/// The shadow's downward offset.
pub const CAPTION_SHADOW_DY: f32 = 1.0;

#[cfg(test)]
#[path = "caption_policy_tests.rs"]
mod more_tests;

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
    fn lines_follow_the_policy_and_fall_back_to_identity_for_missing_facts() {
        let facts = CaptionFacts { title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), ..Default::default() };
        let l = plx_platform::i18n::current();
        let p = CaptionPolicy { title: true, details: DetailsKind::Genre };
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), Some("Crime".into())]);
        // no stream facts: the line is the item's identity (its year), never an open band over nothing
        let p = CaptionPolicy { title: false, details: DetailsKind::Video };
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy { title: false, details: DetailsKind::YearLength };
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy { title: true, details: DetailsKind::None };
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), None], "Nothing has no fallback");
        assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &CaptionFacts::default(), l), [None, None],
            "an item with no name and no facts has nothing to say");
    }
}
