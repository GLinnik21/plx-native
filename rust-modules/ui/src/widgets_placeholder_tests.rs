//! **The placeholder counter at every placeholder DRAW the UI library owns** (`placeholder.rs`).
//!
//! Each test draws through the real primitive on the host GL stubs and asks the counter what the
//! draw said. Each fails when its hook is removed: the draw still happens, the count does not.
//! The hero logo is tested in `hero_logo.rs`, Home's backdrop in `screens/src/home/placeholder_tests.rs`
//! and the Detail backdrop's condition in `screens/src/detail/placeholder_tests.rs`. The screens'
//! other sites (spinners, the Loading title, the stale strip, the info panel's still) draw through
//! GL the host cannot run and are held by `ci/check-placeholders.py` alone.

use super::*;
use crate::placeholder::{self, Reason};

fn frame_rect() -> Rect {
    Rect::new(100.0, 100.0, 190.0, 285.0)
}

fn poster(thumb: &str) -> TileFacts<'_> {
    TileFacts { thumb, ..TileFacts::default() }
}

fn drawn(art: Art) -> placeholder::Frame {
    placeholder::capture_declared(|| card(Painter::root(), frame_rect(), art, 8.0, false, 1.0, 0.0)).1
}

// ---- Poster ----------------------------------------------------------------------------------

#[test]
fn a_poster_with_no_row_yet_counts_a_card_skeleton() {
    // `Art::Poster(None)`: a not-yet-loaded index. It short-circuits before any resolve.
    let f = drawn(Art::Poster(None));
    assert_eq!((f.count, f.of(Reason::CardSkeleton)), (1, 1), "{f:?}");
}

#[test]
fn a_poster_with_an_empty_thumb_counts_a_card_skeleton() {
    // `tex::resolve_wh_on` returns (0, ..) for an empty path before it asks the source.
    let f = drawn(Art::Poster(Some(poster(""))));
    assert_eq!((f.count, f.of(Reason::CardSkeleton)), (1, 1), "{f:?}");
}

#[test]
fn a_poster_whose_art_has_not_arrived_counts_its_thumb_as_the_key() {
    let f = drawn(Art::Poster(Some(poster("/library/metadata/7/thumb/1"))));
    assert_eq!(f.entries.len(), 1);
    assert_eq!(f.entries[0].reason, Reason::CardSkeleton);
    assert_eq!(f.entries[0].key, "/library/metadata/7/thumb/1");
}

/// The neutral collection tile is drawn by `collection_tile::draw` (text and icon, which the host
/// cannot rasterize), on the never-sentinel `CARD_ABSENT`, and `card_named` returns right after it:
/// it reaches no counting call. What this pins is the decision that sends a tile down that path —
/// a collection with no `thumb` — and, beside it, that one WITH a thumb is a poster that can wait.
#[test]
fn a_collection_with_no_artwork_takes_the_absence_path_and_one_with_a_thumb_does_not() {
    let bare = TileFacts { kind: TileKind::Collection, title: "Marvel", thumb: "", ..TileFacts::default() };
    assert_eq!(neutral_collection_name(Some(&bare)), Some("Marvel"));
    let arted = TileFacts { thumb: "/library/collections/1/composite/2", ..bare };
    assert_eq!(neutral_collection_name(Some(&arted)), None);
    let f = drawn(Art::Poster(Some(arted)));
    assert_eq!(f.of(Reason::CardSkeleton), 1, "a collection whose art is on its way is a skeleton: {f:?}");
}

// ---- Still -----------------------------------------------------------------------------------

#[test]
fn a_still_with_no_row_yet_counts_a_card_skeleton() {
    let f = drawn(Art::Still(None));
    assert_eq!((f.count, f.of(Reason::CardSkeleton)), (1, 1), "{f:?}");
}

#[test]
fn a_still_with_nothing_to_fetch_counts_a_card_skeleton() {
    let f = drawn(Art::Still(Some(TileFacts::default())));
    assert_eq!((f.count, f.of(Reason::CardSkeleton)), (1, 1), "{f:?}");
}

#[test]
fn a_still_whose_art_has_not_arrived_counts_its_still_key() {
    let m = TileFacts { still: "/library/metadata/9/thumb/2", ..TileFacts::default() };
    let f = drawn(Art::Still(Some(m)));
    assert_eq!(f.entries.len(), 1, "{f:?}");
    assert_eq!(f.entries[0].key, "/library/metadata/9/thumb/2");
}

/// The "fused still with no texture" line of the plan is the same draw: the fused pass declines
/// texture 0 before it paints, so a still with no picture always reaches the skeleton arm that
/// counts it — there is no second ground to hook.
#[test]
fn a_still_with_no_texture_is_never_fused() {
    let fused = placeholder::capture_declared(|| {
        Painter::root().tex_carded_still(
            0, plx_gfx::gfx::UV_FULL, frame_rect(), 8.0, 0.0, 60.0, theme::scrim(STILL_SCRIM_A),
        )
    });
    assert!(!fused.0, "tex_carded_still(0, ..) must decline so the skeleton arm draws and counts");
    assert_eq!(fused.1.count, 0, "the fused pass itself is not a counting site");
}

// ---- Thumb and Person -----------------------------------------------------------------------

#[test]
fn a_thumb_whose_art_has_not_arrived_counts_a_card_ground() {
    let f = drawn(Art::Thumb { sid: 0, key: "/photo/x", res: (300, 300) });
    assert_eq!((f.count, f.of(Reason::CardGround)), (1, 1), "{f:?}");
    assert_eq!(f.entries[0].key, "/photo/x");
}

#[test]
fn a_person_still_loading_counts_a_card_ground() {
    let f = drawn(Art::Person { sid: 0, key: "/library/metadata/3/thumb", res: (300, 300) });
    assert_eq!((f.count, f.of(Reason::CardGround)), (1, 1), "{f:?}");
}

/// A whole `card` for a keyless person also draws the User glyph, which the host cannot
/// rasterize; the ground it sits on is `person_ground`, tested here directly.
#[test]
fn a_person_the_server_has_no_headshot_of_is_absence_and_counts_nothing() {
    let f = placeholder::capture_declared(|| person_ground(Painter::root(), "", frame_rect(), 8.0)).1;
    assert_eq!(f.count, 0, "an empty key is a glyph on the absent ground, not a wait: {f:?}");
    let f = placeholder::capture_declared(|| person_ground(Painter::root(), "/k", frame_rect(), 8.0)).1;
    assert_eq!(f.count, 1, "a key behind a missing texture is a fetch in flight: {f:?}");
}

#[test]
fn a_recording_pass_over_a_card_counts_nothing() {
    let f = placeholder::capture_declared(|| {
        card(Painter::recording(), frame_rect(), Art::Poster(None), 8.0, false, 1.0, 0.0);
        card_named(Painter::recording(), frame_rect(), Art::Still(None), None, 8.0, true, 1.07, 1.0);
    })
    .1;
    assert_eq!(f.count, 0, "{f:?}");
}

// ---- skeleton_bar / skeleton_sheet -----------------------------------------------------------

#[test]
fn a_skeleton_bar_counts_where_it_is_drawn() {
    let f = placeholder::capture_declared(|| skeleton_bar(Painter::root(), Rect::new(0.0, 0.0, 400.0, 28.0), 0.3)).1;
    assert_eq!((f.count, f.of(Reason::SkeletonBar)), (1, 1), "{f:?}");
}

#[test]
fn a_skeleton_sheet_counts_where_it_is_drawn() {
    let f = placeholder::capture_declared(|| skeleton_sheet(Painter::root(), Rect::new(0.0, 0.0, 190.0, 285.0), 8.0, 0.3)).1;
    assert_eq!((f.count, f.of(Reason::SkeletonSheet)), (1, 1), "{f:?}");
}

#[test]
fn a_recorded_skeleton_counts_nothing() {
    let f = placeholder::capture_declared(|| {
        skeleton_bar(Painter::recording(), Rect::new(0.0, 0.0, 400.0, 28.0), 0.3);
        skeleton_sheet(Painter::recording(), Rect::new(0.0, 0.0, 190.0, 285.0), 8.0, 0.3);
    })
    .1;
    assert_eq!(f.count, 0, "{f:?}");
}

// ---- StatusOverlay -----------------------------------------------------------------------------

fn status(kind: StatusKind) -> placeholder::Frame {
    placeholder::capture_declared(|| {
        StatusOverlay::new(Rect::FULL, c"Loading\u{2026}", kind)
            .draw_measured(&Env::inert(), Painter::root(), &crate::fixture::FixtureMeasure)
    })
    .1
}

#[test]
fn a_working_readout_counts_with_its_caption() {
    let f = status(StatusKind::Working);
    assert_eq!((f.count, f.of(Reason::WorkingReadout)), (1, 1), "{f:?}");
    assert_eq!(f.entries[0].key, "Loading\u{2026}");
}

#[test]
fn a_failed_or_empty_readout_is_an_answer_not_a_wait() {
    assert_eq!(status(StatusKind::Failed).count, 0);
    assert_eq!(status(StatusKind::Empty).count, 0);
}

// ---- profile chip ------------------------------------------------------------------------------

fn chip(thumb: &str) -> placeholder::Frame {
    placeholder::capture_declared(|| {
        let data = ProfileChipRead { src: 0, thumb, initial: c"G", name: c"Guest", name_w: 60.0 };
        profile_chip_with(Painter::root(), data, 0.0, None)
    })
    .1
}

#[test]
fn a_profile_chip_whose_picture_is_on_its_way_counts() {
    let f = chip("/photo/avatar");
    assert_eq!((f.count, f.of(Reason::ProfileChip)), (1, 1), "{f:?}");
}

#[test]
fn a_profile_with_no_picture_is_absence_and_counts_nothing() {
    assert_eq!(chip("").count, 0);
}

// ---- the tokens ----------------------------------------------------------------------------------

/// The accounting adds no pixel: outside the sentinel build the three tokens are the stops they
/// always were, and the absence ground is the very same stop.
#[cfg(not(feature = "placeholder-sentinel"))]
#[test]
fn the_placeholder_tokens_keep_their_default_values() {
    assert_eq!(theme::SKELETON_TOP, theme::CARD_PLACEHOLDER);
    assert_eq!(theme::CARD_ABSENT, theme::CARD_PLACEHOLDER);
    assert_ne!(theme::SKELETON_TOP, theme::SKELETON_BOT);
    assert_eq!(theme::CARD_PLACEHOLDER, [31.0 / 255.0, 33.0 / 255.0, 41.0 / 255.0, 1.0]);
    assert_eq!(theme::SKELETON_BOT, [20.0 / 255.0, 23.0 / 255.0, 28.0 / 255.0, 1.0]);
}
