//! The track-memory matchers, pure: a descriptor in, a track of THIS file (or the reason there is
//! none) out. The ladder they sit in is graded through `build_stream` in `plan_track_memory_tests`.
use super::*;

fn audio(id: i64, lang: &str, codec: &str, channels: i64, title: &str) -> Stream {
    Stream {
        id,
        index: id,
        lang_code: lang.into(),
        codec: codec.into(),
        channels,
        title: title.into(),
        ..Default::default()
    }
}

fn sub(id: i64, lang: &str, title: &str) -> Stream {
    Stream { id, index: id, lang_code: lang.into(), codec: "srt".into(), title: title.into(), ..Default::default() }
}

fn memo_audio(lang: &str, codec: &str, channels: u8) -> AudioMemo {
    AudioMemo { lang: lang.into(), codec: codec.into(), channels, ..Default::default() }
}

fn memo_sub(lang: &str) -> SubtitleTrackMemo {
    SubtitleTrackMemo { lang: lang.into(), kind: "text".into(), ..Default::default() }
}

fn anyone(_: &Stream) -> bool {
    true
}

fn no_commentary(_: usize) -> bool {
    false
}

#[test]
fn audio_matches_the_remembered_language_over_the_file_default() {
    let mut tracks = vec![audio(1, "eng", "eac3", 6, ""), audio(2, "jpn", "eac3", 6, "")];
    tracks[0].default = true;
    assert_eq!(match_audio(&tracks, &memo_audio("jpn", "eac3", 6), no_commentary), Ok(1));
}

#[test]
fn audio_language_is_hard_and_folds_spellings() {
    let tracks = vec![audio(1, "eng", "eac3", 6, ""), audio(2, "fra", "ac3", 2, "")];
    assert_eq!(match_audio(&tracks, &memo_audio("jpn", "eac3", 6), no_commentary), Err("language"));
    // a `fre` memo and a `fra` stream are one language
    assert_eq!(match_audio(&tracks, &memo_audio("fre", "ac3", 2), no_commentary), Ok(1));
}

#[test]
fn audio_commentary_and_description_are_hard() {
    let tracks = vec![audio(1, "eng", "ac3", 2, "Director's Commentary"), audio(2, "eng", "ac3", 2, "")];
    let commentary_is_first = |i: usize| i == 0;
    // a plain pick never lands on the commentary, and a commentary pick never on the main track
    assert_eq!(match_audio(&tracks, &memo_audio("eng", "ac3", 2), commentary_is_first), Ok(1));
    let memo = AudioMemo { commentary: true, ..memo_audio("eng", "ac3", 2) };
    assert_eq!(match_audio(&tracks, &memo, commentary_is_first), Ok(0));
    assert_eq!(match_audio(&tracks[1..], &memo, no_commentary), Err("commentary"));

    let mut described = audio(3, "eng", "ac3", 2, "");
    described.ad = true;
    let plain = vec![audio(1, "eng", "ac3", 2, ""), described.clone()];
    assert_eq!(match_audio(&plain, &memo_audio("eng", "ac3", 2), no_commentary), Ok(0));
    assert_eq!(match_audio(&plain[1..], &memo_audio("eng", "ac3", 2), no_commentary), Err("audio description"));
    let wants_ad = AudioMemo { ad: true, ..memo_audio("eng", "ac3", 2) };
    assert_eq!(match_audio(&plain, &wants_ad, no_commentary), Ok(1));
}

#[test]
fn audio_soft_keys_rank_codec_then_channels_then_the_default_then_file_order() {
    let tracks = vec![
        audio(1, "eng", "ac3", 2, ""),
        audio(2, "eng", "eac3", 2, ""),
        audio(3, "eng", "eac3", 6, ""),
        audio(4, "eng", "eac3", 6, ""),
    ];
    // codec and channels both agree on 3 and 4; the earliest of the best wins
    assert_eq!(match_audio(&tracks, &memo_audio("eng", "eac3", 6), no_commentary), Ok(2));
    // codec outranks channels
    assert_eq!(match_audio(&tracks, &memo_audio("eng", "eac3", 2), no_commentary), Ok(1));
    // and a track with the codec but the wrong channel count beats one with the channels but not the codec
    let split = vec![audio(1, "eng", "ac3", 6, ""), audio(2, "eng", "eac3", 2, "")];
    assert_eq!(match_audio(&split, &memo_audio("eng", "eac3", 6), no_commentary), Ok(1));
    // nothing agrees but the language: the file's default, else the first
    let mut dts = tracks.clone();
    dts[3].default = true;
    assert_eq!(match_audio(&dts, &memo_audio("eng", "dts", 8), no_commentary), Ok(3));
    assert_eq!(match_audio(&tracks, &memo_audio("eng", "dts", 8), no_commentary), Ok(0));
}

#[test]
fn a_memo_with_no_language_matches_only_a_track_with_none() {
    let tracks = vec![audio(1, "eng", "ac3", 2, ""), audio(2, "", "ac3", 2, "")];
    assert_eq!(match_audio(&tracks, &memo_audio("", "ac3", 2), no_commentary), Ok(1));
    assert_eq!(match_audio(&tracks[..1], &memo_audio("", "ac3", 2), no_commentary), Err("language"));
}

#[test]
fn subtitle_language_and_forced_are_hard() {
    let subs = vec![sub(1, "eng", ""), sub(2, "spa", "")];
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), anyone), Ok(0));
    assert_eq!(match_subtitle(&subs, &memo_sub("fra"), anyone), Err("language"));
    let forced_memo = SubtitleTrackMemo { forced: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs, &forced_memo, anyone), Err("forced"), "a full track is not the forced one");
    let mut flagged = subs.clone();
    flagged[0].forced = true;
    assert_eq!(match_subtitle(&flagged, &forced_memo, anyone), Ok(0));
    assert_eq!(match_subtitle(&flagged, &memo_sub("eng"), anyone), Err("forced"), "and the forced one is not the full track");
}

/// A track whose NAME says forced is forced to the matcher exactly as it is to the menu's FORCED
/// badge, with the flag unset: a memo written from the menu matches back, and a plain memo does not
/// land on it.
#[test]
fn title_only_forced_matches() {
    let subs = vec![sub(1, "eng", "English"), sub(2, "eng", "Forced")];
    assert!(sub_traits(&subs[1]).forced);
    assert!(!sub_traits(&subs[0]).forced);
    let forced_memo = SubtitleTrackMemo { forced: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs, &forced_memo, anyone), Ok(1));
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), anyone), Ok(0));
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), Some(1));
}

#[test]
fn subtitle_soft_keys_rank_sdh_then_kind_then_embedded_then_file_order() {
    let mut subs = vec![sub(1, "eng", ""), sub(2, "eng", ""), sub(3, "eng", "")];
    subs[1].sdh = true;
    subs[2].sdh = true;
    subs[2].external = true;
    let sdh = SubtitleTrackMemo { sdh: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs, &sdh, anyone), Ok(1), "SDH agrees; the embedded one beats the sidecar");
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), anyone), Ok(0), "a non-SDH memo takes the non-SDH track");
    let mut image = subs.clone();
    image[0].codec = "pgs".into();
    image[1].sdh = false;
    assert_eq!(match_subtitle(&image, &memo_sub("eng"), anyone), Ok(1), "text kind agrees, the PGS one does not");
}

#[test]
fn a_sidecar_is_used_only_when_nothing_embedded_matches_and_the_caller_can_carry_it() {
    let mut subs = vec![sub(1, "eng", "")];
    subs[0].external = true;
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), anyone), Ok(0));
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), |s| !s.external), Err("language"));
}

#[test]
fn forced_only_takes_the_forced_track_in_the_playing_audios_language() {
    let mut subs = vec![sub(1, "eng", ""), sub(2, "spa", ""), sub(3, "eng", "")];
    subs[1].forced = true;
    subs[2].forced = true;
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), Some(2));
    assert_eq!(match_forced_only(&subs, "spa", "", anyone), Some(1), "an audio switch re-points it");
    assert_eq!(match_forced_only(&subs[..2], "eng", "", anyone), None, "only a Spanish forced track under English audio: nothing");
    assert_eq!(match_forced_only(&subs[..1], "eng", "", anyone), None, "a full track is never the answer");
}

#[test]
fn forced_only_uses_the_memos_language_only_for_untagged_audio() {
    let mut subs = vec![sub(1, "eng", ""), sub(2, "spa", "")];
    subs[0].forced = true;
    subs[1].forced = true;
    assert_eq!(match_forced_only(&subs, "", "spa", anyone), Some(1));
    assert_eq!(match_forced_only(&subs, "und", "spa", anyone), Some(1));
    assert_eq!(match_forced_only(&subs, "eng", "spa", anyone), Some(0), "tagged audio wins over the memo");
    assert_eq!(match_forced_only(&subs, "", "", anyone), None);
}

#[test]
fn forced_only_prefers_a_non_sdh_then_an_embedded_track() {
    let mut subs = vec![sub(1, "eng", ""), sub(2, "eng", ""), sub(3, "eng", "")];
    for s in &mut subs {
        s.forced = true;
    }
    subs[0].sdh = true;
    subs[1].external = true;
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), Some(2), "not SDH, embedded");
    assert_eq!(match_forced_only(&subs[..2], "eng", "", anyone), Some(1), "a non-SDH sidecar beats an SDH embedded track");
    let only_external = vec![subs[1].clone()];
    assert_eq!(match_forced_only(&only_external, "eng", "", anyone), Some(0), "an external forced track alone is used");
    assert_eq!(match_forced_only(&only_external, "eng", "", |s| !s.external), None, "unless the caller cannot carry it");
}

#[test]
fn forced_commentary_is_skipped() {
    let mut subs = vec![sub(1, "eng", "Director's Commentary"), sub(2, "eng", "")];
    subs[0].forced = true;
    assert!(sub_traits(&subs[0]).commentary);
    assert!(sub_traits(&subs[0]).forced, "the module doc's claim: a flagged commentary IS forced to a matcher");
    let forced_memo = SubtitleTrackMemo { forced: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs[..1], &forced_memo, anyone), Ok(0), "so a forced Track memo can land on it: commentary only ranks");
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), None, "but it is never the answer to Forced only");
    subs[1].forced = true;
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), Some(1));
}
