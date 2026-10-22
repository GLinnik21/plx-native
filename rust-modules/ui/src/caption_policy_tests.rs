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
        assert_eq!(card_row::room_for(card_row::LEGACY_BAND_H, e, 0.0), crate::consts::UNDER_LABEL_AIR + card_row::under_band(e));
    }
}

/// **The room the caption rule counts under a card is the gap the shared shelf pitch keeps there.**
/// `room_for` assumes the next shelf's heading (or the next grid row's tile) rests `UNDER_LABEL_AIR`
/// past the band, which is what `ROW_PITCH_FIXED` (every shelf column's fixed part) and the grid's
/// pitch carry. The owners that build their own flow pin themselves to the same numbers: the person
/// page (`a_shelf_here_pitches_like_a_shelf_on_home`), Search (`layout::block_h` against `ROW_PITCH`),
/// the Library's shelves (`shelf_pitch_keeps_the_shared_air`), Detail's section gaps
/// (`a_shelf_is_followed_by_the_shared_air`), and `Stack::frame`, which refuses a page whose shelf keeps
/// less than that air under it while another shelf stands below.
#[test]
fn the_rules_room_is_the_gap_the_shared_pitch_keeps_under_a_card() {
    use crate::consts::{CARD_DY, CARD_H, ROW_PITCH, TITLE_DY, UNDER_LABEL_AIR};
    let above_the_next = TITLE_DY + CARD_DY + CARD_H;
    for band in [card_row::LEGACY_BAND_H, 0.0, 80.0, 112.0] {
        for e in [0.0, 0.25, 0.5, 1.0] {
            let room = card_row::room_for(band, e, 0.0);
            let pitch_gap = card_row::ROW_PITCH_FIXED + card_row::under_band_for(band, e) - above_the_next;
            assert!((room - pitch_gap).abs() < 1e-3, "band {band} e {e}: the rule counts {room} under a card, the pitch keeps {pitch_gap}");
        }
    }
    assert_eq!(ROW_PITCH - above_the_next, card_row::room_for(card_row::LEGACY_BAND_H, 1.0, 0.0), "a focused shelf's pitch is the rule's room at the legacy band");
    assert_eq!(card_row::room_for(card_row::LEGACY_BAND_H, 0.0, 0.0), LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR, "a collapsed band keeps the air");
    let grid = poster_grid::Geom::DEFAULT;
    assert_eq!(grid.pitch() - grid.h, LABEL_BAND_COLLAPSED + UNDER_LABEL_AIR, "a grid row keeps the same air under its band");
}

/// **A line waits for the room that holds its box**, at both band sizes: on a closed band nothing is
/// drawn, on an open one the whole block is, and on no frame between is a line drawn while the room
/// under its card is smaller than its box (the row under it would be on top of it). The lines come
/// from `TileLabel::lines`, the boxes the block is drawn with.
#[test]
fn the_caption_waits_for_its_room_at_both_band_sizes() {
    for (band, two_lines) in [(80.0, false), (112.0, true)] {
        let lines = |e: f32| {
            let label = card_row::TileLabel::from_lines([Some("t".into()), two_lines.then(|| "d".into())]).revealed(band, e, 0.0);
            label.lines()
        };
        assert!(lines(0.0).iter().flatten().all(|l| l.alpha == 0.0), "no caption on a closed band ({band})");
        assert!(lines(1.0).iter().flatten().all(|l| l.alpha > 0.999), "whole caption on an open band ({band})");
        let mut last = [0.0f32; 2];
        for step in 0..=200 {
            let e = step as f32 / 200.0;
            let drawn = lines(e);
            // the title (the first line) is on screen before the details line under it, never after
            if let [Some(title), Some(details)] = drawn {
                assert!(title.alpha >= details.alpha, "band {band}: the details line leads the title at {step}/200");
            }
            for (k, line) in drawn.iter().enumerate() {
                let Some(line) = line else { continue };
                assert!(line.alpha == 0.0 || card_row::room_for(band, e, 0.0) >= line.bottom,
                    "band {band}, e {e}: line {k} is drawn (alpha {}) with {} px of room under a {} px box",
                    line.alpha, card_row::room_for(band, e, 0.0), line.bottom);
                assert!(line.alpha >= last[k], "band {band}: line {k} dims while its band opens (e {e})");
                last[k] = line.alpha;
            }
        }
    }
}

/// A block with no title starts its details line where the title would have been: its box (and so
/// the room it waits for) is the details line's own, not the second line's.
#[test]
fn a_details_only_block_waits_for_the_room_its_own_box_needs() {
    let label = card_row::TileLabel::from_lines([None, Some("d".into())]);
    let [title, details] = label.lines();
    assert!(title.is_none());
    let details = details.unwrap();
    let both = card_row::TileLabel::from_lines([Some("t".into()), Some("d".into())]).lines();
    assert!(details.bottom < both[1].unwrap().bottom, "alone, the details line sits where the title would");
    assert_eq!(details.top, both[0].unwrap().top);
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
    l.reveal = None;
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
    assert_eq!(at(DetailsKind::YearLength, &season("", 0)).as_deref(), Some("Specials"),
        "a season with no title and no number is the specials season, never an empty line");
    assert_eq!(at(DetailsKind::None, &season("Season 6", 6)), None, "Nothing shows no line, fallback or not");
    // the kind's own facts win over the identity
    let rich = CaptionFacts { year: Some(2009), genre: Some("Comedy".into()), content_rating: Some("TV-14".into()), ..season("Season 6", 6) };
    assert_eq!(at(DetailsKind::YearLength, &rich).as_deref(), Some("Season 6"),
        "a season's identity beats its year: 'Season 6', not a bare '2009' that names neither show nor season");
    assert_eq!(at(DetailsKind::Genre, &rich).as_deref(), Some("Comedy"));
    assert_eq!(at(DetailsKind::AgeRatingScore, &rich).as_deref(), Some("TV-14"));
    assert_eq!(at(DetailsKind::Video, &rich).as_deref(), Some("Season 6"), "no stream facts: the season's identity, not its year");
    // an episode: its address; a show and a movie: their year; a collection: its size
    let episode = CaptionFacts { title: Some("Pilot".into()), episode: Some((1, 8)), ..Default::default() };
    let show = CaptionFacts { is_show: true, year: Some(2022), ..Default::default() };
    let movie = CaptionFacts { year: Some(1995), ..Default::default() };
    let films = CaptionFacts { subtype: Some("movie".into()), child_count: Some(7), ..Default::default() };
    for details in pickers {
        assert_eq!(at(details, &episode).as_deref(), Some("S1 \u{b7} E8"), "{details:?}");
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

/// **S2: the identity fallback ends in a word for every kind.** An episode the server numbered and
/// dated nothing for says "Episode"; a season with no title says "Season N", or "Specials" for the
/// season numbered 0, even when it has a year: no kind has a chain that ends empty.
#[test]
fn every_kind_ends_its_identity_chain_in_a_word() {
    let cx = i18n::english();
    let pickers = [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore];
    let at = |details, f: &CaptionFacts| caption_lines(&CaptionPolicy { title: false, details }, f, cx)[1].clone();
    let bare_episode = CaptionFacts { title: Some("Pilot".into()), episode: Some((0, 0)), ..Default::default() };
    let dated_episode = CaptionFacts { year: Some(2019), ..bare_episode.clone() };
    let untitled_season = |number, year| CaptionFacts {
        title: Some("The Office".into()), season: Some((number, String::new())), year, ..Default::default()
    };
    for details in pickers {
        assert_eq!(at(details, &bare_episode).as_deref(), Some("Episode"), "{details:?}");
        assert_eq!(at(details, &dated_episode).as_deref(), Some("2019"), "{details:?}: a year beats the kind word");
        assert_eq!(at(details, &untitled_season(0, None)).as_deref(), Some("Specials"), "{details:?}");
        assert_eq!(at(details, &untitled_season(0, Some(2009))).as_deref(), Some("Specials"), "{details:?}: a year does not empty it");
        assert_eq!(at(details, &untitled_season(4, None)).as_deref(), Some("Season 4"), "{details:?}");
    }
    assert_eq!(at(DetailsKind::None, &bare_episode), None, "Nothing has no fallback");
    for &l in Language::SHIPPED.iter() {
        with_language(l, |cx| {
            assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &bare_episode, cx)[1].as_deref(),
                Some(&*plx_platform::i18n::msg::browse_kind_episode_in(cx)), "{l:?}");
            assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &untitled_season(0, None), cx)[1].as_deref(),
                Some(&*plx_platform::i18n::msg::browse_detail_specials_in(cx)), "{l:?}");
        });
    }
}

/// **S4: "N new episodes" is for a show in progress.** A show nobody opened has every episode
/// unwatched, and "62 new episodes" says nothing a reader needs: it reads its year and seasons.
#[test]
fn a_show_reads_new_episodes_only_when_it_is_in_progress() {
    let cx = i18n::english();
    let line = |f: &CaptionFacts| caption_lines(&CaptionPolicy::DEFAULT, f, cx)[1].clone();
    let unopened = CaptionFacts { leaf_count: Some(62), viewed_leaf_count: Some(0), ..show(0) };
    assert_eq!(line(&unopened).as_deref(), Some("2019 \u{b7} 3 seasons"), "viewed 0: nothing is 'new' to a show never opened");
    let no_count = CaptionFacts { viewed_leaf_count: None, ..show(4) };
    assert_eq!(line(&no_count).as_deref(), Some("2019 \u{b7} 3 seasons"), "no viewed count: not known to be started");
    assert_eq!(line(&show(4)).as_deref(), Some("4 new episodes"), "started with 4 unwatched");
    assert_eq!(line(&show(0)).as_deref(), Some("2019 \u{b7} 3 seasons"), "all watched: nothing new");
}

/// **S1 + S3: ONE rule for a collection tile, on every surface.** The Title switch governs its title
/// line like any kind; its details line is its size under every details kind but Nothing (a server
/// content rating, a genre never replace it); Nothing opens nothing. A name its face cuts is carried
/// by the details line when the title is off, inside the band the policy opened.
#[test]
fn a_collection_follows_the_policy_like_any_kind() {
    let cx = i18n::english();
    let films = CaptionFacts {
        title: Some("Marvel".into()), subtype: Some("movie".into()), child_count: Some(7),
        content_rating: Some("PG-13".into()), genre: Some("Action".into()), ..Default::default()
    };
    let lines = |title, details, f: &CaptionFacts| caption_lines(&CaptionPolicy { title, details }, f, cx);
    for details in [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
        assert_eq!(lines(false, details, &films), [None, Some("7 films".into())], "{details:?}: its size, whatever is picked");
        assert_eq!(lines(true, details, &films), [Some("Marvel".into()), Some("7 films".into())], "{details:?}");
    }
    assert_eq!(lines(true, DetailsKind::None, &films), [Some("Marvel".into()), None]);
    assert_eq!(lines(false, DetailsKind::None, &films), [None, None], "Nothing opens nothing");
    let cut = CaptionFacts { name_cut: true, ..films.clone() };
    assert_eq!(lines(false, DetailsKind::YearLength, &cut), [None, Some("Marvel \u{b7} 7 films".into())],
        "a cut name rides the one line the policy opened");
    assert_eq!(lines(false, DetailsKind::Genre, &cut), [None, Some("Marvel \u{b7} 7 films".into())]);
    assert_eq!(lines(true, DetailsKind::Genre, &cut), [Some("Marvel".into()), Some("7 films".into())], "the title is on: said once");
    assert_eq!(lines(false, DetailsKind::None, &cut), [None, None], "Nothing opens nothing, a cut name or not");
    assert_eq!(lines(true, DetailsKind::None, &cut), [Some("Marvel".into()), None]);
}

/// **S6: an episode whose art prints its address does not say it twice.** Its details line names the
/// show instead; without a show it keeps the address, and the address everywhere else.
#[test]
fn an_episode_whose_art_prints_its_address_names_its_show() {
    let cx = i18n::english();
    let at = |details, f: &CaptionFacts| caption_lines(&CaptionPolicy { title: false, details }, f, cx)[1].clone();
    let marked = CaptionFacts {
        title: Some("Pilot".into()), episode: Some((3, 4)), show: Some("Severance".into()), address_on_art: true, ..Default::default()
    };
    for details in [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
        assert_eq!(at(details, &marked).as_deref(), Some("Severance"), "{details:?}");
        assert_eq!(at(details, &CaptionFacts { address_on_art: false, ..marked.clone() }).as_deref(), Some("S3 \u{b7} E4"), "{details:?}");
    }
    assert_eq!(at(DetailsKind::YearLength, &CaptionFacts { show: None, ..marked }).as_deref(), Some("S3 \u{b7} E4"),
        "no show to name: the address is better than nothing");
}

/// **A season whose art prints "SEASON 3" does not say its number twice either** (the Collection
/// screen's members): its details line names the show, and a season with no show says nothing
/// rather than "Season 3" under "SEASON 3". With the title on, the show is never read twice (the
/// title line already names it, and so does an orphan season's own title).
#[test]
fn a_season_whose_art_prints_its_number_names_its_show_or_nothing() {
    let cx = i18n::english();
    let lines = |title, details, f: &CaptionFacts| caption_lines(&CaptionPolicy { title, details }, f, cx);
    let marked = CaptionFacts {
        title: Some("The Office".into()), season: Some((3, "Season 3".into())), show: Some("The Office".into()),
        address_on_art: true, ..Default::default()
    };
    for details in [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
        assert_eq!(lines(false, details, &marked), [None, Some("The Office".into())], "{details:?}: the show, not the number");
        assert_eq!(lines(true, details, &marked), [Some("The Office".into()), None], "{details:?}: the show is not read twice");
        let plain = CaptionFacts { address_on_art: false, ..marked.clone() };
        assert_eq!(lines(false, details, &plain), [None, Some("Season 3".into())], "{details:?}: the number where the art does not print it");
    }
    let orphan = CaptionFacts { title: Some("Season 3".into()), show: None, ..marked.clone() };
    assert_eq!(lines(false, DetailsKind::YearLength, &orphan), [None, None], "no show to name: nothing, not 'Season 3' under 'SEASON 3'");
    let unmarked_orphan = CaptionFacts { address_on_art: false, ..orphan };
    assert_eq!(lines(true, DetailsKind::YearLength, &unmarked_orphan), [Some("Season 3".into()), None],
        "an orphan season under Title On is its title once, never 'Season 3' over 'Season 3'");
}
