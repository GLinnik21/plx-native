//! Home hero metadata and the independent resume pill.
use super::*;
use plx_ui::landing_hero::{home_synopsis, HOME_LABEL_H};

const PREMISE: &str = "A woman crosses the frozen wastes. Years later she learns what she lost.";
const FIRST: &str = "A woman crosses the frozen wastes.";
const MIN: i64 = 60_000;

/// A 15-minute film with a summary of two sentences, not started.
fn movie() -> PmsMovie {
    PmsMovie { kind: 0, title: "Sintel".into(), summary: PREMISE.into(), genre: Some("Fantasy".into()), secondary_genre: Some("Adventure".into()), dur_ns: 15 * MIN * 1_000_000, ..Default::default() }
}

/// An episode: season 2, episode 4, "The Night Crossing".
fn episode() -> PmsMovie {
    PmsMovie {
        kind: 3,
        title: "The Night Crossing".into(),
        show_title: "Sintel".into(),
        season_index: 2,
        ep_index: 4,
        ..movie()
    }
}

fn premise(hero: &PmsMovie) -> &str {
    match hero_line(hero) {
        Some(HeroLine::Premise(premise)) => premise,
        _ => panic!("the premise was expected"),
    }
}

#[test]
fn resume_pill_rounds_up_and_uses_the_existing_resume_rule() {
    let hero = PmsMovie { resume_ms: 13 * MIN + 30_000, ..movie() };
    let (label, progress) = hero_action(&hero);
    assert_eq!(label.to_str().unwrap(), plx_ui::fmt::duration(2 * MIN));
    assert_eq!(progress, hero.resume_frac());
    let almost_finished = PmsMovie { resume_ms: 14 * MIN + 20_000, ..hero.clone() };
    assert!(hero_action(&almost_finished).1.is_none());
    let unknown = PmsMovie { dur_ns: 0, ..hero };
    let (label, progress) = hero_action(&unknown);
    assert_eq!(&*label, plx_appkit::hero_layout::play_label(true));
    assert_eq!(progress, None);
}

#[test]
fn unplayed_movies_and_shows_get_the_identity_band() {
    for hero in [movie(), PmsMovie { resume_ms: 9_000, ..movie() }] {
        assert!(matches!(hero_line(&hero), Some(HeroLine::Metadata { .. })));
    }
    let show = PmsMovie { kind: plx_data::pms::KIND_SHOW, child_count: 2, ..movie() };
    assert!(matches!(hero_line(&show), Some(HeroLine::Metadata { .. })));
    assert_eq!(premise(&PmsMovie { viewed_leaf_count: Some(1), ..show }), FIRST);
    assert_eq!(premise(&PmsMovie { watched: true, ..movie() }), FIRST);
    let bare = PmsMovie::default();
    assert!(matches!(hero_line(&bare), Some(HeroLine::Metadata { date, length, genres, rating, .. })
        if date.is_empty() && length.is_empty() && genres.is_empty() && rating.is_empty()));
}

/// **The ladder keeps its anchor whatever the line**: the block's foot is 638 (the board's number, 30
/// above the action row) for a label, a one-line premise and a two-line one; the title's foot a
/// gap above the block; and a one-line block brings the title down by the line it does not take.
#[test]
fn the_ladder_keeps_its_anchor_for_every_line() {
    let two = PmsMovie {
        summary: "Somewhere past the last road, beyond every map anyone has drawn and every name anyone \
                  remembers, a woman crosses the frozen wastes, hunting the dragon she raised, \
                  carrying everything she still owns, and nothing she wants to keep"
            .into(),
        kind: 1,
        watched: true,
        ..movie()
    };
    let one = PmsMovie { kind: 1, watched: true, ..movie() };
    let resumed = PmsMovie { resume_ms: 7 * MIN, ..movie() };
    let long_label = episode();
    assert_eq!(home_synopsis(FIRST).measure_h(theme::hero::HOME_SYNOPSIS_W), HOME_LABEL_H, "the premise: the first sentence is one line");
    assert_eq!(home_synopsis(premise(&two)).measure_h(theme::hero::HOME_SYNOPSIS_W), 2.0 * HOME_LABEL_H, "…and the other is elided at two");
    let stack = |hero: &PmsMovie| hero_ladder(hero).0;
    let (s_two, s_one, s_resumed, s_label) = (stack(&two), stack(&one), stack(&resumed), stack(&long_label));
    for (what, s, h) in [
        ("two-line premise", &s_two, 2.0 * HOME_LABEL_H),
        ("one-line premise", &s_one, HOME_LABEL_H),
        ("movie identity", &s_resumed, HOME_LABEL_H),
        ("episode identity", &s_label, HOME_LABEL_H),
    ] {
        let block = s.block.expect(what);
        assert_eq!(block.h, h, "{what}: the block's height");
        assert_eq!(block.y + block.h, 638.0, "{what}: the block's foot is the board's 638");
        assert_eq!(block.y + block.h, theme::hero::HOME_ACTIONS_Y - theme::hero::HOME_ACTIONS_GAP);
        assert_eq!(s.title.y + s.title.h + theme::hero::HOME_TITLE_GAP, block.y, "{what}: the title's foot a gap above the block");
    }
    assert_eq!(s_one.title.y - s_two.title.y, HOME_LABEL_H, "a one-line block brings the title down a line");
    assert_eq!((s_resumed.title.y, s_resumed.block), (s_label.title.y, s_label.block), "a label is a label");
    // no line: the title stands alone over the actions
    let mute = PmsMovie { summary: String::new(), kind: plx_data::pms::KIND_SHOW, watched: true, ..movie() };
    assert!(stack(&mute).block.is_none());
}

/// The hero prints the metadata row in the same order for the three title types.
#[test]
fn the_page_draws_episode_show_and_movie_metadata() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let drawn = |hero: &PmsMovie| {
        plx_gfx::text::capture_text_runs_for_test(|| hero_content(hero, Painter::recording(), 0.0, m)).join("\n")
    };
    let film = PmsMovie { year: 2010, rating: "PG-13".into(), video_resolution: Some("4k".into()), ..movie() };
    let fresh = drawn(&film);
    assert_eq!(fresh, drawn(&PmsMovie { resume_ms: 7 * MIN, ..film }));
    for word in ["Movie", "2010", "15m", "Fantasy, Adventure", "4K", "PG-13"] {
        assert!(fresh.contains(word), "{fresh:?}");
    }
    let show = drawn(&PmsMovie {
        kind: plx_data::pms::KIND_SHOW, year: 2025, child_count: 2, rating: "TV-14".into(),
        video_resolution: Some("4k".into()), ..movie()
    });
    for word in ["TV Show", "2025", "15m", "Fantasy, Adventure", "TV-14"] {
        assert!(show.contains(word), "{show:?}");
    }
    assert!(!show.contains("4K"), "{show:?}");
    let ep = drawn(&PmsMovie {
        resume_ms: 7 * MIN, aired: "2001-09-19".into(), rating: "TV-14".into(),
        video_resolution: Some("1080".into()), ..episode()
    });
    for word in [plx_ui::fmt::episode_address(2, 4), plx_ui::fmt::hero_date("2001-09-19", 0),
        "15m".into(), "1080p".into(), "TV-14".into()] {
        assert!(ep.contains(&word), "{ep:?}");
    }
    assert!(!ep.contains("left"), "{ep:?}");
}

#[test]
fn sparse_episode_uses_its_title_or_omits_the_empty_row() {
    let mut ep = PmsMovie { kind: plx_data::pms::KIND_EPISODE, show_title: "Series".into(), title: "Pilot".into(), ..Default::default() };
    assert!(matches!(hero_line(&ep), Some(HeroLine::Metadata { identity, .. }) if identity == "Pilot"));
    ep.show_title.clear();
    assert!(hero_line(&ep).is_none(), "the logo rung already carries the episode's title");
}
