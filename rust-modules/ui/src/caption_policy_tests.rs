//! T1: band heights per policy, the lines per locale (Belarusian plurals 1/2/5/21), and the
//! caption band's geometry at both sizes.

use super::*;
use crate::card_row::{self, LABEL_BAND_COLLAPSED};
use crate::poster_grid::{self, GridBand};
use plx_platform::i18n::{self, Language, Preference};

fn show(unwatched: u32) -> CaptionFacts {
    CaptionFacts {
        is_show: true,
        year: Some(2019),
        season_count: Some(3),
        leaf_count: Some(20 + unwatched),
        viewed_leaf_count: Some(20),
        ..Default::default()
    }
}

fn with_language<R>(l: Language, f: impl FnOnce(&LocaleContext) -> R) -> R {
    let _g = i18n::language_on_this_thread_for_test(Preference::from_tag(l.tag()));
    f(i18n::current())
}

#[test]
fn band_heights_follow_what_the_policy_can_show() {
    let p = |title, details| CaptionPolicy { title, details };
    assert_eq!(band_h(&p(false, DetailsKind::None)), 0.0);
    assert_eq!(band_h(&p(true, DetailsKind::None)), 80.0);
    assert_eq!(band_h(&p(false, DetailsKind::Genre)), 80.0);
    assert_eq!(band_h(&p(true, DetailsKind::Video)), 112.0);
}

#[test]
fn the_default_details_line_by_kind() {
    let cx = i18n::english();
    let p = CaptionPolicy::DEFAULT;
    let line = |f: &CaptionFacts| caption_lines(&p, f, cx)[1].clone();
    let movie = CaptionFacts { year: Some(2024), duration_min: Some(112), ..Default::default() };
    assert_eq!(line(&movie).as_deref(), Some("2024 \u{b7} 1h 52m"));
    let started = CaptionFacts { view_offset_min: Some(74), ..movie.clone() };
    assert_eq!(line(&started).as_deref(), Some("38m left"));
    let finished = CaptionFacts { view_offset_min: Some(112), ..movie };
    assert_eq!(line(&finished).as_deref(), Some("2024 \u{b7} 1h 52m"), "a past-end offset is not 'started'");
    assert_eq!(line(&show(0)).as_deref(), Some("2019 \u{b7} 3 seasons"));
    assert_eq!(line(&show(4)).as_deref(), Some("4 new episodes"));
    assert_eq!(line(&show(1)).as_deref(), Some("1 new episode"));
    let films = CaptionFacts { subtype: Some("movie".into()), child_count: Some(7), ..Default::default() };
    assert_eq!(line(&films).as_deref(), Some("7 films"));
}

#[test]
fn video_and_age_rating_lines() {
    let cx = i18n::english();
    let f = CaptionFacts {
        video_resolution: Some("4k".into()), hdr: true,
        content_rating: Some("PG-13".into()), audience_rating: Some(7.84), ..Default::default()
    };
    let at = |details| caption_lines(&CaptionPolicy { title: false, details }, &f, cx)[1].clone();
    assert_eq!(at(DetailsKind::Video).as_deref(), Some("4K \u{b7} HDR"));
    assert_eq!(at(DetailsKind::AgeRatingScore).as_deref(), Some("PG-13 \u{b7} 7.8"));
    let hd = CaptionFacts { video_resolution: Some("1080".into()), ..Default::default() };
    assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Video }, &hd, cx)[1].as_deref(), Some("1080p"));
}

#[test]
fn belarusian_new_episodes_use_all_four_plural_forms() {
    with_language(Language::Be, |cx| {
        let say = |n| caption_lines(&CaptionPolicy::DEFAULT, &show(n), cx)[1].clone().unwrap();
        assert_eq!(say(1), "1 новая серыя");
        assert_eq!(say(2), "2 новыя серыі");
        assert_eq!(say(5), "5 новых серый");
        assert_eq!(say(21), "21 новая серыя");
    });
}

#[test]
fn every_shipped_language_words_a_show_without_an_english_leak() {
    for &l in Language::SHIPPED.iter() {
        with_language(l, |cx| {
            for n in [1, 2, 5, 21] {
                let s = caption_lines(&CaptionPolicy::DEFAULT, &show(n), cx)[1].clone().unwrap();
                assert!(s.contains(&n.to_string()), "{l:?} {n}: {s}");
                if l != Language::En {
                    assert!(!s.contains("new episode"), "{l:?} {n} fell back to English: {s}");
                }
            }
        });
    }
}

#[test]
fn the_band_opens_only_the_focused_row_by_its_height() {
    for (band, open) in [(80.0, 40.0), (112.0, 72.0), (0.0, 0.0)] {
        assert_eq!(card_row::under_band_for(band, 0.0), LABEL_BAND_COLLAPSED);
        assert_eq!(card_row::under_band_for(band, 1.0) - LABEL_BAND_COLLAPSED, open);
        let g = poster_grid::Geom::DEFAULT;
        let focused = [GridBand { row: 1, expansion: 1.0 }];
        let r0 = poster_grid::cell_in_for(band, &g, 0, 400.0, 0.0, &focused);
        let r2 = poster_grid::cell_in_for(band, &g, 12, 400.0, 0.0, &focused);
        let rest = poster_grid::cell_in_for(band, &g, 12, 400.0, 0.0, &[]);
        assert_eq!(r0.y, 400.0, "rows above the focus do not move");
        assert!((r2.y - rest.y - open).abs() < 0.01, "rows below move down by what the band opens");
    }
}

#[test]
fn the_legacy_band_is_the_unchanged_geometry() {
    let g = poster_grid::Geom::DEFAULT;
    let focused = [GridBand { row: 1, expansion: 0.6 }];
    for index in [0, 6, 12, 40] {
        assert_eq!(
            poster_grid::cell_in(&g, index, 400.0, 30.0, &focused),
            poster_grid::cell_in_for(card_row::LEGACY_BAND_H, &g, index, 400.0, 30.0, &focused)
        );
    }
    for e in [0.0, 0.3, 0.7, 1.0] {
        assert_eq!(card_row::band_reveal(e), card_row::band_reveal_for(card_row::LEGACY_BAND_H, e));
    }
}

#[test]
fn the_caption_waits_for_its_room_at_both_band_sizes() {
    for band in [80.0, 112.0] {
        assert_eq!(card_row::band_reveal_for(band, 0.0), 0.0, "no caption on a closed band ({band})");
        assert!(card_row::band_reveal_for(band, 1.0) > 0.999, "whole caption on an open band ({band})");
    }
}

#[test]
fn a_policy_label_is_shadowed_and_drops_absent_lines() {
    let l = card_row::TileLabel::from_lines([None, Some("2019".into())]);
    assert!(l.shadowed && l.title.is_none() && l.caption.is_some());
    assert!(!card_row::TileLabel::title("x").shadowed);
}

struct Flat;
impl plx_machine::machine::Measure for Flat {
    fn width(&self, s: &std::ffi::CStr, size: i32, _: bool) -> f32 { s.to_bytes().len() as f32 * size as f32 * 0.5 }
    fn cap_h(&self, _: i32) -> f32 { 20.0 }
    fn line_h(&self, _: i32) -> f32 { 36.0 }
}

#[test]
fn the_details_line_under_a_poster_is_white_at_82_percent() {
    let _g = plx_base::testlock::serial();
    let mut l = card_row::TileLabel::from_lines([None, Some("2019 · 2h 6m".into())]);
    l.reveal = 1.0;
    let runs = crate::draw_census::capture_runs(|| {
        card_row::draw_label(crate::Painter::recording(), crate::Rect::new(100.0, 100.0, 200.0, 300.0), 1.0, &card_row::RowStyle::HOME, &l, &Flat);
    });
    let d = runs.iter().rfind(|r| r.text.contains("2019")).expect("the details run is drawn");
    assert_eq!(d.ink, [1.0, 1.0, 1.0, 0.82]);
    assert_eq!(d.ink, crate::theme::CAPTION_DETAIL_INK);
}

/// **A line the setting turns on is never drawn empty**, for every kind of tile and every picker:
/// the picked kind's facts when the tile has them, else its identity line.
#[test]
fn a_details_line_the_tile_has_no_facts_for_says_its_identity() {
    let cx = i18n::english();
    let at = |details, f: &CaptionFacts| caption_lines(&CaptionPolicy { title: false, details }, f, cx)[1].clone();
    let pickers = [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore];
    // a season: no length, here no year, genre or rating either
    let season = |own: &str, number| CaptionFacts {
        title: Some("The Office".into()), season: Some((number, own.into())), ..Default::default()
    };
    for details in pickers {
        assert_eq!(at(details, &season("Season 6", 6)).as_deref(), Some("Season 6"), "{details:?}");
        assert_eq!(at(details, &season("Specials", 0)).as_deref(), Some("Specials"), "its own title when it is not the numbered form");
        assert_eq!(at(details, &season("", 3)).as_deref(), Some("Season 3"), "the catalog's words when the server named it nothing");
    }
    assert_eq!(at(DetailsKind::YearLength, &season("", 0)), None, "a season with no title and no number has no identity");
    assert_eq!(at(DetailsKind::None, &season("Season 6", 6)), None, "Nothing shows no line, fallback or not");
    // the kind's own facts win over the identity
    let rich = CaptionFacts { year: Some(2009), genre: Some("Comedy".into()), content_rating: Some("TV-14".into()), ..season("Season 6", 6) };
    assert_eq!(at(DetailsKind::YearLength, &rich).as_deref(), Some("2009"));
    assert_eq!(at(DetailsKind::Genre, &rich).as_deref(), Some("Comedy"));
    assert_eq!(at(DetailsKind::AgeRatingScore, &rich).as_deref(), Some("TV-14"));
    assert_eq!(at(DetailsKind::Video, &rich).as_deref(), Some("Season 6"), "no stream facts: the season's identity, not its year");
    // an episode: its address; a show and a movie: their year; a collection: its size
    let episode = CaptionFacts { title: Some("Pilot".into()), episode: Some((1, 8)), ..Default::default() };
    let show = CaptionFacts { is_show: true, year: Some(2022), ..Default::default() };
    let movie = CaptionFacts { year: Some(1995), ..Default::default() };
    let films = CaptionFacts { subtype: Some("movie".into()), child_count: Some(7), ..Default::default() };
    for details in pickers {
        assert_eq!(at(details, &episode).as_deref(), Some("S1 \u{2022} E8"), "{details:?}");
        assert_eq!(at(details, &show).as_deref(), Some("2022"), "{details:?}");
        assert_eq!(at(details, &movie).as_deref(), Some("1995"), "{details:?}");
        assert_eq!(at(details, &films).as_deref(), Some("7 films"), "{details:?}");
    }
}

/// **No band over nothing**: a movie with no year, a show with no year and a collection of unknown
/// size have no fact and no identity to say, so the last fallback says what KIND they are, in the
/// locale's words, under every picker. Nothing still has no line, and a tile that is none of those
/// (here a bare default with no kind flag) has nothing to say.
#[test]
fn a_tile_with_no_facts_and_no_identity_says_what_kind_it_is() {
    let at = |details, f: &CaptionFacts, cx: &LocaleContext| caption_lines(&CaptionPolicy { title: false, details }, f, cx)[1].clone();
    let pickers = [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore];
    let movie = CaptionFacts { is_movie: true, year: Some(0), ..Default::default() };
    let show = CaptionFacts { is_show: true, ..Default::default() };
    let collection = CaptionFacts { subtype: Some("movie".into()), ..Default::default() };
    for &l in Language::SHIPPED.iter() {
        with_language(l, |cx| {
            for details in pickers {
                for (f, word) in [
                    (&movie, plx_platform::i18n::msg::browse_kind_movie_in(cx)),
                    (&show, plx_platform::i18n::msg::browse_kind_show_in(cx)),
                    (&collection, plx_platform::i18n::msg::browse_collection_kind_in(cx)),
                ] {
                    assert_eq!(at(details, f, cx).as_deref(), Some(&*word), "{l:?} {details:?}");
                }
            }
        });
    }
    let cx = i18n::english();
    assert_eq!(at(DetailsKind::Genre, &movie, cx).as_deref(), Some("Movie"));
    assert_eq!(at(DetailsKind::YearLength, &show, cx).as_deref(), Some("Show"));
    assert_eq!(at(DetailsKind::AgeRatingScore, &collection, cx).as_deref(), Some("Collection"));
    assert_eq!(at(DetailsKind::None, &movie, cx), None, "Nothing has no fallback");
    assert_eq!(at(DetailsKind::Genre, &CaptionFacts::default(), cx), None, "no kind, no name, no facts: nothing to say");
    // a year, a genre or a size beats the kind word
    assert_eq!(at(DetailsKind::YearLength, &CaptionFacts { year: Some(1995), ..movie.clone() }, cx).as_deref(), Some("1995"));
    assert_eq!(at(DetailsKind::Genre, &CaptionFacts { genre: Some("Crime".into()), ..movie.clone() }, cx).as_deref(), Some("Crime"));
    assert_eq!(at(DetailsKind::Genre, &CaptionFacts { child_count: Some(7), ..collection.clone() }, cx).as_deref(), Some("7 films"));
}

/// A tile from a source that is not the household's own is credited after its details line, so
/// two servers' copies of one title read apart; Nothing has no details line to credit it on.
#[test]
fn a_friends_server_is_credited_after_the_details_line() {
    let cx = i18n::english();
    let f = CaptionFacts {
        title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), is_movie: true,
        source: Some("@alex".into()), ..Default::default()
    };
    let lines = |title, details| caption_lines(&CaptionPolicy { title, details }, &f, cx);
    assert_eq!(lines(true, DetailsKind::Genre), [Some("Heat".into()), Some("Crime \u{b7} @alex".into())]);
    assert_eq!(lines(false, DetailsKind::YearLength), [None, Some("1995 \u{b7} @alex".into())]);
    assert_eq!(lines(true, DetailsKind::None), [Some("Heat".into()), None], "Nothing drops the credit with the line");
    let bare = CaptionFacts { year: Some(0), genre: None, ..f.clone() };
    assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &bare, cx)[1].as_deref(),
        Some("Movie \u{b7} @alex"), "the kind word is the line it is credited on");
    let none = CaptionFacts { source: Some(String::new()), ..f };
    assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &none, cx)[1].as_deref(), Some("Crime"), "an empty handle credits nothing");
}
