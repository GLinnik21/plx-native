//! The crop an art tile samples (`art_uv`): every picture keeps its own aspect in its tile.

use super::*;
use crate::plex::ServerId;
use crate::ui::card_row::RowStyle;

/// The cast row's circle, at rest and popped. The shape is what matters; the position is not.
fn cast_circle(scale: f32) -> Rect {
    Rect::new(96.0, 700.0, RowStyle::CAST.w, RowStyle::CAST.h).scaled(scale)
}

fn headshot(key: &str) -> Art<'_> {
    Art::Person { sid: ServerId::UNSET, key, res: (300, 300) }
}

/// Texels per drawn pixel on each axis — equal ⇔ an even scale, i.e. no distortion.
fn texel_scale(uv: [f32; 4], tw: f32, th: f32, r: Rect) -> (f32, f32) {
    (uv[2] * tw / r.w, uv[3] * th / r.h)
}

/// **The reported bug.** A cast headshot is requested at 300×300 with `minSize=1`, which COVERS the
/// box, so a portrait photo decodes at 300×450. The tile used to sample the whole texture into the
/// 190×190 circle — a vertical squash to two-thirds. It must now sample an even-scaled window that
/// keeps the full width and rides high on the photo, at rest and through the focus pop.
#[test]
fn a_portrait_headshot_on_the_cast_row_is_cropped_not_squashed() {
    let (tw, th) = (300.0, 450.0);
    for scale in [1.0, RowStyle::CAST.focus_scale] {
        let r = cast_circle(scale);
        let uv = art_uv(&headshot("/library/metadata/1/thumb"), tw, th, r);
        let (sx, sy) = texel_scale(uv, tw, th, r);
        assert!((sx - sy).abs() < 1e-4, "cast headshot drawn with uneven scale {sx} x {sy} (uv {uv:?})");
        assert_eq!(uv[2], 1.0, "the full width of a portrait survives");
        assert!(
            uv[1] < (1.0 - uv[3]) * 0.5,
            "a headshot's crop must keep more of the top than the bottom: {uv:?}"
        );
    }
}

/// Every surface that draws a person's photo draws it through `Art::Person` — the cast row, the
/// person page's portrait and Search's person results — so all of them get the headshot crop.
#[test]
fn every_person_photo_takes_the_headshot_crop_and_other_art_is_even() {
    assert_eq!(art_crop(&headshot("k")), crate::ui::Crop::Headshot);
    assert_eq!(art_crop(&Art::Poster(None)), crate::ui::Crop::Centre);
    assert_eq!(art_crop(&Art::Still(None)), crate::ui::Crop::Centre);
    assert_eq!(
        art_crop(&Art::Thumb { sid: ServerId::UNSET, key: "k", res: (300, 300) }),
        crate::ui::Crop::Centre
    );
}

/// Art already at its tile's aspect is sampled whole, so posters in poster tiles and 16:9 stills in
/// the episode filmstrip draw exactly as before; only art of ANOTHER aspect changes, and it
/// changes from squashed to cropped — here the poster a landscape tile falls back to.
#[test]
fn art_at_its_tiles_aspect_is_untouched_and_a_poster_in_a_landscape_tile_is_cropped() {
    let poster = Rect::new(0.0, 0.0, 250.0, 375.0);
    assert_eq!(art_uv(&Art::Poster(None), 250.0, 375.0, poster), crate::gfx::UV_FULL);
    let still = Rect::new(0.0, 0.0, RowStyle::EPISODE.w, RowStyle::EPISODE.h);
    let uv = art_uv(&Art::Still(None), 250.0, 375.0, still);
    let (sx, sy) = texel_scale(uv, 250.0, 375.0, still);
    assert!((sx - sy).abs() < 1e-4, "poster fallback drawn with uneven scale {sx} x {sy}");
    assert!(uv[3] < 1.0 && uv[2] == 1.0, "a tall poster in a wide tile loses top and bottom: {uv:?}");
    // an undecoded texture answers the whole window, never a NaN
    assert_eq!(art_uv(&headshot("k"), 0.0, 0.0, cast_circle(1.0)), crate::gfx::UV_FULL);
}
