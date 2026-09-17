//! MDE decision tests: part id extraction and the 2000-status refusal reason.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn part_id_is_read_from_the_parts_segment() {
    assert_eq!(
        part_id_of("/library/parts/98765/1712345678/file.mkv"),
        98765
    );
    assert_eq!(part_id_of("/library/parts/1/0/file.mp4"), 1);
    // a query string rides along on the real keys
    assert_eq!(part_id_of("/library/parts/42/17/file.mkv?download=0"), 42);
}


#[test]
fn part_id_is_zero_when_there_is_no_parts_segment() {
    assert_eq!(part_id_of(""), 0);
    assert_eq!(part_id_of("/library/metadata/1234"), 0);
    assert_eq!(
        part_id_of("/library/parts"),
        0,
        "trailing `parts` with no id"
    );
    assert_eq!(part_id_of("/library/parts/notanumber/file.mkv"), 0);
}


/// The direct-play gate: MKV and MP4/M4V parts are fed to the demuxer untouched — everything
/// else takes the remux branch. mp4 moved sides on 2026-08-11 (issue #22): the mkv-only gate
/// dated from an unseekable AVIO, and on a server that cannot transcode it turned every mp4
/// into a failure.
#[test]
fn mkv_and_mp4_parts_are_direct_playable() {
    assert!(part_is_streamable("/library/parts/1/2/movie.mkv"));
    assert!(
        part_is_streamable("/library/parts/1/2/movie.mkv?x=1"),
        "the query must not defeat it"
    );
    assert!(part_is_streamable("/library/parts/1/2/movie.mp4"));
    assert!(part_is_streamable("/library/parts/1/2/movie.m4v"));
    assert!(
        !part_is_streamable("/library/parts/1/2/movie.mov"),
        "mov still remuxes"
    );
    assert!(!part_is_streamable(""));
    assert!(
        !part_is_streamable("/library/parts/1/2/mkv.avi"),
        "the extension, not a substring"
    );
    assert!(
        !part_is_streamable("/library/parts/1/2/mp4.avi"),
        "the extension, not a substring"
    );
}


/// The pre-flight refusal, graded off a real `/decision` body. Four properties, and each one is
/// a way the old "parse it and only log it" behaviour went wrong:
///   * a `2000` verdict IS a refusal, and it hands back the TRANSCODE sentence — the one that
///     names the cause — rather than the general text that merely restates the code;
///   * a healthy decision (`1001`, "conversion OK") is not one, or every transcode in the
///     library would stop;
///   * a body with no verdict at all is not one either — absent is not a refusal, and it is
///     what an older server and every failed/unparseable fetch look like;
///   * a refusal with no sentence still refuses. The CODE is the decision; the text is only
///     the human line, and a server that stays quiet must not thereby become playable.
#[test]
fn a_2000_decision_is_a_refusal_and_quotes_the_reason_the_server_named() {
    fn mc(json: &[u8]) -> crate::plex::MediaContainer {
        serde_json::from_slice::<crate::plex::Envelope>(json)
            .expect("parse")
            .media_container
    }
    // the live PMS 1.43.3 answer for a VP9 source
    let refused = mc(br#"{"MediaContainer":{"generalDecisionCode":2000,
        "generalDecisionText":"Neither direct play nor conversion is available.",
        "transcodeDecisionCode":4007,
        "transcodeDecisionText":"Cannot convert this item. Implementation for video encoder 'vp9' not found."}}"#);
    assert_eq!(
        refusal(&refused).as_deref(),
        Some("Cannot convert this item. Implementation for video encoder 'vp9' not found."),
        "the transcode sentence names the cause; the general one only restates the code"
    );

    // only the general sentence came back — quote that instead of nothing
    let general_only = mc(br#"{"MediaContainer":{"generalDecisionCode":"2000",
        "generalDecisionText":"Neither direct play nor conversion is available."}}"#);
    assert_eq!(
        refusal(&general_only).as_deref(),
        Some("Neither direct play nor conversion is available.")
    );

    // refused, and said nothing about why: still a stop, with no line to quote
    let silent = mc(br#"{"MediaContainer":{"generalDecisionCode":2000}}"#);
    assert_eq!(
        refusal(&silent).as_deref(),
        Some(""),
        "the CODE is the decision, not the text"
    );

    // "Direct play not available; Conversion OK." — the ordinary transcode, which must proceed
    let ok = mc(
        br#"{"MediaContainer":{"generalDecisionCode":1001,"transcodeDecisionCode":1001,
        "transcodeDecisionText":"Direct play not available; Conversion OK."}}"#,
    );
    assert!(refusal(&ok).is_none());

    // no verdict block at all (an older server, or a body we could not parse into one)
    assert!(
        refusal(&mc(br#"{"MediaContainer":{"size":1}}"#)).is_none(),
        "absent is not a refusal"
    );
}

