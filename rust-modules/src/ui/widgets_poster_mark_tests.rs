//! The poster watch-state mark and its write-verb twin (`poster_mark` vs. `row_watch_state`).

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_poster_nobody_has_started_wears_no_mark_at_all() {
    // the common case on any real server, and the whole reason the polarity inverted
    assert_eq!(poster_mark(&row(false, 0)), PosterMark::None);
}

#[test]
fn a_finished_poster_wears_the_watched_disc() {
    assert_eq!(poster_mark(&row(true, 0)), PosterMark::Watched);
}

#[test]
fn a_re_watch_in_flight_outranks_the_watched_flag() {
    // PMS reports BOTH on a finished-then-restarted item; the bar wins, so the tile never
    // wears two marks — and what it says is what the viewer is actually doing.
    let m = row(true, 30 * 60 * 1000);
    assert_eq!(poster_mark(&m), PosterMark::InProgress);
    assert!(
        m.resume_frac().is_some(),
        "InProgress must be exactly when the caller draws the bar"
    );
}

#[test]
fn a_part_watched_poster_that_was_never_finished_is_in_progress_too() {
    assert_eq!(
        poster_mark(&row(false, 30 * 60 * 1000)),
        PosterMark::InProgress
    );
}

#[test]
fn an_offset_the_server_never_cleared_is_finished_not_in_progress() {
    // resume AT or PAST the end: a full-width bar there read as a rendering bug, and it would
    // now also hide the disc the item has earned. Both the mark and the bar must agree.
    for resume in [100 * 60 * 1000, 200 * 60 * 1000] {
        let m = row(true, resume);
        assert_eq!(m.resume_frac(), None, "resume {resume} must not draw a bar");
        assert_eq!(poster_mark(&m), PosterMark::Watched, "resume {resume}");
    }
}

#[test]
fn a_row_with_no_runtime_cannot_be_in_progress() {
    // dur_ns == 0 (the server sent no duration): a fraction is undefined, so there is no bar to
    // draw and the watched flag alone decides.
    let mut m = row(true, 30 * 60 * 1000);
    m.dur_ns = 0;
    assert_eq!(m.resume_frac(), None);
    assert_eq!(poster_mark(&m), PosterMark::Watched);
    let mut m = row(false, 30 * 60 * 1000);
    m.dur_ns = 0;
    assert_eq!(poster_mark(&m), PosterMark::None);
}

#[test]
fn a_show_three_episodes_in_is_not_a_watched_show() {
    // The device capture that caught this: a library filtered to `unwatchedLeaves=1` — every
    // tile has an unseen episode by construction — had five posters wearing a watched disc,
    // because `!unwatched` is true for a container the moment ONE episode is played. A show is
    // marked only when it is DONE, so partly-watched sits with never-started under "no mark".
    let mut m = PmsMovie::default();
    m.kind = 1; // show
    m.unwatched = false; // some episode has been played…
    m.watched = false; // …but not all of them
    assert_eq!(poster_mark(&m), PosterMark::None);
    m.watched = true;
    assert_eq!(
        poster_mark(&m),
        PosterMark::Watched,
        "every leaf seen IS the disc"
    );
}

// ── …and the same three states asked as the WRITE-VERB question ───────────────────────────
//
// `row_watch_state` is `poster_mark` plus one rule, and the one rule is the whole reason it
// exists: a mark DESCRIBES, a menu row PROMISES. These grade the difference and the sameness.

/// **The only place the two resolvers disagree** — and it is the case the owner reported: a
/// show in the middle wears no poster mark (a tile cannot say where a series stands) but is
/// reachable from a menu in BOTH directions, so it is `InProgress` here and gets both rows.
#[test]
fn a_container_mid_run_is_in_the_middle_for_a_menu_though_it_wears_no_mark() {
    let mut m = PmsMovie::default();
    m.kind = 1; // show
    m.unwatched = false; // some episode has been played…
    m.watched = false; // …but not all of them
    assert_eq!(
        poster_mark(&m),
        PosterMark::None,
        "the tile still claims nothing"
    );
    assert_eq!(
        row_watch_state(&m),
        PosterMark::InProgress,
        "…but both verbs are reachable"
    );
    // and a SEASON is a container on the same terms — the rule is the flag pair, not the kind
    m.kind = 2;
    assert_eq!(row_watch_state(&m), PosterMark::InProgress);
}

/// The two ENDS are the same answer from both resolvers, on containers as much as leaves.
/// A container's `watched` is the strict `viewedLeafCount >= leafCount` (`pms::parse_item`), so
/// a finished show is genuinely finished and its menu offers only the way back — which is what
/// the shelf menu got wrong before, offering "Mark as Watched" on a show already done.
#[test]
fn the_two_ends_of_the_range_answer_the_same_either_way() {
    let mut show = PmsMovie::default();
    show.kind = 1;
    for (unwatched, watched, want) in [
        (true, false, PosterMark::None),
        (false, true, PosterMark::Watched),
    ] {
        show.unwatched = unwatched;
        show.watched = watched;
        assert_eq!(row_watch_state(&show), want);
        assert_eq!(
            poster_mark(&show),
            want,
            "an END is not where the two questions differ"
        );
    }
}

/// A LEAF is delegated whole, so every resume-point edge `poster_mark` keeps — an offset past
/// the end is finished, a row with no runtime cannot be in progress — holds for the menu too
/// without being restated. The flags are complements on a leaf, so the extra rule is unreachable
/// there by construction.
#[test]
fn a_leaf_asks_the_poster_and_gets_its_answer_unchanged() {
    for m in [
        row(false, 0),
        row(true, 0),
        row(false, 30 * 60 * 1000),
        row(true, 30 * 60 * 1000),
        row(true, 200 * 60 * 1000),
    ] {
        assert_eq!(
            row_watch_state(&m),
            poster_mark(&m),
            "a leaf must not answer twice"
        );
    }
}
