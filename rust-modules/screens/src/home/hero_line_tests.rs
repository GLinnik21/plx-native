//! The line Home's hero reads under its title, by the hero's state (owner's board CW-13 v2): a hero
//! being resumed says how much is left (the string the Continue Watching footer reads), an episode
//! names itself, a hero nothing has been watched of is introduced by the first sentence of its
//! summary; and the ladder it stands on keeps its anchor whatever the line is.

use super::*;
use plx_ui::landing_hero::{home_synopsis, HOME_LABEL_H};

const PREMISE: &str = "A woman crosses the frozen wastes. Years later she learns what she lost.";
const FIRST: &str = "A woman crosses the frozen wastes.";
const MIN: i64 = 60_000;

/// A 15-minute film with a summary of two sentences, not started.
fn movie() -> PmsMovie {
    PmsMovie { kind: 0, title: "Sintel".into(), summary: PREMISE.into(), dur_ns: 15 * MIN * 1_000_000, ..Default::default() }
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

fn label(hero: &PmsMovie) -> String {
    match hero_line(hero) {
        Some(HeroLine::Label(label)) => label,
        Some(HeroLine::Premise(premise)) => panic!("a label was expected, not the premise {premise:?}"),
        None => panic!("a label was expected, not nothing"),
    }
}

fn premise(hero: &PmsMovie) -> &str {
    match hero_line(hero) {
        Some(HeroLine::Premise(premise)) => premise,
        Some(HeroLine::Label(label)) => panic!("the premise was expected, not the label {label:?}"),
        None => panic!("the premise was expected, not nothing"),
    }
}

/// **A movie being resumed says its time left, in the footer's own words.** Seven minutes into a
/// fifteen-minute film is eight minutes left; the line is the string `fmt::footer_left` builds from
/// the time left the Continue Watching footer derives ([`tile_facts::of`]), so the card under the
/// hero and the hero say it identically.
#[test]
fn a_resumed_movie_reads_its_time_left_as_the_footer_does() {
    let hero = PmsMovie { resume_ms: 7 * MIN, ..movie() };
    assert!(hero_resumes(&hero), "the premise: its Play button says Resume");
    let line = label(&hero);
    assert_eq!(line, plx_ui::fmt::footer_left(8 * MIN));
    let footer = tile_facts::of(&hero).resume.expect("the footer's own derivation of a started card");
    assert_eq!(line, plx_ui::fmt::footer_left(footer.left_ms), "the footer under the hero says the same");
    assert!(line.contains('8'), "eight minutes left, not {line:?}");
    // the time left rounds UP to a whole minute, as the footer's does
    let rounded = PmsMovie { resume_ms: 13 * MIN + 30_000, ..movie() };
    assert_eq!(label(&rounded), plx_ui::fmt::footer_left(2 * MIN), "a minute and a half left reads two");
    let last = PmsMovie { resume_ms: 14 * MIN, ..movie() };
    assert_eq!(label(&last), plx_ui::fmt::footer_left(MIN), "the last minute the Play button still resumes");
    // an hour and more reads as hours, in the footer's shortest form
    let long = PmsMovie { dur_ns: 150 * MIN * 1_000_000, resume_ms: 30 * MIN, ..movie() };
    assert_eq!(label(&long), plx_ui::fmt::footer_left(120 * MIN));
}

/// **An episode names itself, resumed or not**: the existing episode label, whether it is half
/// watched or the next one on deck and not started.
#[test]
fn an_episode_reads_its_label_whether_resumed_or_on_deck() {
    let want = plx_ui::fmt::episode_kicker(2, 4, "The Night Crossing");
    assert!(want.contains("The Night Crossing"), "{want}");
    let on_deck = episode();
    assert_eq!(label(&on_deck), want, "on deck, not started");
    let resumed = PmsMovie { resume_ms: 7 * MIN, ..episode() };
    assert!(hero_resumes(&resumed), "the premise: it is being resumed");
    assert_eq!(label(&resumed), want, "resumed: the label, not the time left");
}

/// **A hero nothing has been watched of is introduced by its premise**: the first sentence of its
/// summary, never the whole of it. A film, a show, a film resumed within its first ten seconds (the
/// Play button says Play), a finished one whose resume point the server never cleared, and one with
/// no known duration all read it.
#[test]
fn a_hero_nothing_has_been_watched_of_is_introduced_by_its_first_sentence() {
    for (what, hero) in [
        ("a film, not started", movie()),
        ("a show", PmsMovie { kind: 1, ..movie() }),
        ("a film nine seconds in", PmsMovie { resume_ms: 9_000, ..movie() }),
        ("a film past 95 percent", PmsMovie { resume_ms: 15 * MIN - 10_000, ..movie() }),
        ("a film of no known length", PmsMovie { dur_ns: 0, resume_ms: 7 * MIN, ..movie() }),
    ] {
        assert_eq!(premise(&hero), FIRST, "{what}");
    }
}

/// **A line with nothing to say falls to the next**: an episode with nothing to name it is a resumed
/// film's time left, or its premise; a hero with no words at all has no line.
#[test]
fn a_line_with_nothing_to_say_falls_to_the_next() {
    let nameless = PmsMovie { title: String::new(), season_index: 0, ep_index: 0, ..episode() };
    assert_eq!(premise(&nameless), FIRST, "no label, not started: the premise");
    let resumed = PmsMovie { resume_ms: 7 * MIN, ..nameless };
    assert_eq!(label(&resumed), plx_ui::fmt::footer_left(8 * MIN), "no label, resumed: the time left");
    let mute = PmsMovie { summary: String::new(), ..movie() };
    assert!(hero_line(&mute).is_none(), "no summary, nothing started: no line");
    let blank = PmsMovie { summary: "  \n ".into(), ..movie() };
    assert!(hero_line(&blank).is_none(), "a blank summary is no summary");
    let resumed_mute = PmsMovie { resume_ms: 7 * MIN, ..mute };
    assert_eq!(label(&resumed_mute), plx_ui::fmt::footer_left(8 * MIN), "the time left needs no summary");
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
        ..movie()
    };
    let one = movie();
    let resumed = PmsMovie { resume_ms: 7 * MIN, ..movie() };
    let long_label = episode();
    assert_eq!(home_synopsis(FIRST).measure_h(theme::hero::HOME_SYNOPSIS_W), HOME_LABEL_H, "the premise: the first sentence is one line");
    assert_eq!(home_synopsis(premise(&two)).measure_h(theme::hero::HOME_SYNOPSIS_W), 2.0 * HOME_LABEL_H, "…and the other is elided at two");
    let stack = |hero: &PmsMovie| hero_ladder(hero).0;
    let (s_two, s_one, s_resumed, s_label) = (stack(&two), stack(&one), stack(&resumed), stack(&long_label));
    for (what, s, h) in [
        ("two-line premise", &s_two, 2.0 * HOME_LABEL_H),
        ("one-line premise", &s_one, HOME_LABEL_H),
        ("time left", &s_resumed, HOME_LABEL_H),
        ("episode label", &s_label, HOME_LABEL_H),
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
    let mute = PmsMovie { summary: String::new(), ..movie() };
    assert!(stack(&mute).block.is_none());
}

/// **The shade's copy column follows the line**: its foot is the block's foot, 638, for every state,
/// so the copy block the shade stands behind is the one the page draws.
#[test]
fn the_copy_column_the_shade_stands_behind_ends_where_the_line_does() {
    let m = &plx_ui::fixture::FixtureMeasure;
    for (what, hero) in [
        ("time left", PmsMovie { resume_ms: 7 * MIN, ..movie() }),
        ("episode", episode()),
        ("premise", movie()),
    ] {
        let rect = hero_copy_rect(&hero, m);
        assert_eq!(rect.y + rect.h, 638.0, "{what}: {rect:?}");
    }
}

/// **The page draws the line the state names**: a resumed film draws its time left and none of its
/// summary; a hero not started draws its first sentence and not the second; an episode its label.
#[test]
fn the_page_draws_the_line_the_state_names() {
    let _guard = plx_base::testlock::serial();
    let m = &plx_ui::fixture::FixtureMeasure;
    let drawn = |hero: &PmsMovie| {
        plx_gfx::text::capture_text_runs_for_test(|| hero_content(hero, Painter::recording(), 0.0, m)).join("\n")
    };
    let resumed = drawn(&PmsMovie { resume_ms: 7 * MIN, ..movie() });
    assert!(resumed.contains(&plx_ui::fmt::footer_left(8 * MIN)), "{resumed:?}");
    assert!(!resumed.contains("woman"), "a resumed film does not recite its summary: {resumed:?}");
    let fresh = drawn(&movie());
    assert!(fresh.contains("woman crosses"), "{fresh:?}");
    assert!(!fresh.contains("Years later"), "the second sentence stays on the Details page: {fresh:?}");
    let ep = drawn(&PmsMovie { resume_ms: 7 * MIN, ..episode() });
    assert!(ep.contains("The Night Crossing"), "{ep:?}");
    assert!(!ep.contains("left"), "an episode names itself, it does not count down: {ep:?}");
}
