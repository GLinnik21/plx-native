//! `convert_streams`: a Dolby Vision record's survival across multiple video streams.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **A Dolby Vision record must survive a second video stream that has none.** `fps` and `hdr`
/// take the LAST `streamType: 1` stream in the part and that is harmless for both; the DV
/// record is read by `route::video_direct_plays`, so blanking it back to the all-zero default
/// re-opens the direct-play gate and the file plays in the wrong colours — the exact bug the
/// gate exists for. No part on the dev server carries two video streams today (all 540 leaves
/// swept 2026-08-21), which is precisely why this is a test and not a measurement: embedded
/// cover art is an ordinary thing for a library to contain and nothing else here would notice.
#[test]
fn a_dolby_vision_record_is_not_erased_by_a_later_video_stream() {
    let p5 = video_stream(Some((5, 0, 0)));
    let cover = video_stream(None);
    let dovi = convert_streams(&[p5, cover]).dovi;
    assert!(
        dovi.present,
        "the P5 record must outlive a second video stream"
    );
    assert_eq!(dovi.profile, 5);
    assert!(
        dovi.base_layer_unusable(),
        "and must still forbid a server-side COPY of it"
    );
    // …and, undeclared, must still refuse direct play — the record surviving is what both of
    // those turn on, so the cover-art stream must not be able to blank it
    assert_eq!(
        dovi.presentation(false),
        crate::metadata::DvPresentation::Refuse("no cross-compatible base layer")
    );
    assert_eq!(
        dovi.presentation(true).declared().map(|n| n.profile_id),
        Some(5)
    );
}

/// The ordinary single-video-stream shapes, so the guard above cannot be read as "any DV
/// record anywhere wins": a part with no Dolby Vision at all still produces the all-zero
/// record that refuses nothing.
#[test]
fn a_part_with_no_dolby_vision_reports_no_record() {
    let s = convert_streams(&[video_stream(None)]);
    let (hdr, dovi) = (s.hdr, s.dovi);
    assert_eq!(dovi, Dovi::default());
    assert!(!dovi.base_layer_unusable());
    assert!(!hdr, "no DV and no PQ/HLG transfer is not HDR");
}
