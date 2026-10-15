use super::*;

fn span(start_ms: i64, end_ms: i64) -> Span<'static> {
    Span { start_ms, end_ms, motion: Motion::Known(false) }
}

#[test]
fn libass_clock_matches_string2timecode() {
    // The fraction is CENTISECONDS whatever its length; fewer than four numbers is 0.
    assert_eq!(libass_clock_ms("0:01:02.50"), 62_500);
    assert_eq!(libass_clock_ms("0:00:01.5"), 1_050);
    assert_eq!(libass_clock_ms("0:00:01.123"), 2_230);
    assert_eq!(libass_clock_ms("1:00:00"), 0, "no fraction: sscanf yields 3 of 4, libass returns 0");
    assert_eq!(libass_clock_ms("0:00:01."), 0);
    assert_eq!(libass_clock_ms("x"), 0);
    assert_eq!(libass_clock_ms(""), 0);
    assert_eq!(libass_clock_ms("  0: 0: 1. 5"), 1_050, "%d skips leading white space");
    assert_eq!(libass_clock_ms("0 :00:01.00"), 0, "the literal colon is not preceded by white space");
    assert_eq!(libass_clock_ms("0:00:01.50trailing"), 1_500, "text after the number is ignored");
    assert_eq!(libass_clock_ms("-1:00:00.00"), -3_600_000);
    assert_eq!(libass_clock_ms("+1:00:00.00"), 3_600_000);
    assert_eq!(libass_clock_ms("10:00:00.00"), 36_000_000);
}

#[test]
fn validity_is_the_gap_between_the_surrounding_boundaries() {
    let spans = [span(1000, 3000), span(5000, 6000)];
    let v = validity(spans, 1000);
    assert_eq!((v.from_ms, v.until_ms, v.animated), (1000, 3000, false));
    let v = validity(spans, 2999);
    assert_eq!((v.from_ms, v.until_ms), (1000, 3000));
    let v = validity(spans, 3000);
    assert_eq!((v.from_ms, v.until_ms), (3000, 5000), "end is exclusive: the gap starts at 3000");
    let v = validity(spans, 0);
    assert_eq!((v.from_ms, v.until_ms), (i64::MIN, 1000));
    let v = validity(spans, 9000);
    assert_eq!((v.from_ms, v.until_ms), (6000, i64::MAX));
    assert!(validity(spans, 2000).contains(2000) && !validity(spans, 2000).contains(3000));
}

#[test]
fn overlap_and_never_active_spans() {
    let v = validity([span(0, 4000), span(2000, 3000)], 2500);
    assert_eq!((v.from_ms, v.until_ms), (2000, 3000));
    let v = validity([span(500, 500), span(700, 600)], 100);
    assert_eq!((v.from_ms, v.until_ms), (i64::MIN, i64::MAX), "zero or negative duration is no boundary");
}

#[test]
fn only_active_events_make_a_frame_animated() {
    let karaoke = Span { start_ms: 0, end_ms: 1000, motion: Motion::Packet(b"0,0,Default,,0,0,0,,{\\kf50}AB") };
    let plain = Span { start_ms: 2000, end_ms: 3000, motion: Motion::Packet(b"1,0,Default,,0,0,0,,hello") };
    assert!(validity([karaoke, plain], 500).animated);
    assert!(!validity([karaoke, plain], 2500).animated, "the karaoke line is over");
}

#[test]
fn animation_tags_and_effects() {
    for text in [
        "{\\t(0,100,\\fs40)}a", "{\\move(1,2,3,4)}a", "{\\fad(100,100)}a", "{\\fade(0,1,2,3,4,5,6)}a",
        "{\\k20}a", "{\\K20}a", "{\\kf20}a", "{\\ko20}a", "{\\T(0,1,\\c&H0&)}a",
    ] {
        assert!(text_animated(text.as_bytes()), "{text}");
    }
    for text in ["plain", "{\\an8\\pos(1,2)}a", "{\\fs30\\b1}a", "a\\Nb", "{\\fnTimes}x"] {
        assert!(!text_animated(text.as_bytes()), "{text}");
    }
    assert!(packet_animated(b"0,0,Default,,0,0,0,Scroll up;1;2;3,text"));
    assert!(!packet_animated(b"0,0,Default,,0,0,0,,text"));
    assert!(!packet_animated(b"0,0,Default,,0,0,0, ,text"), "a blank Effect is empty");
}
