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

/// C5: a remembered EXTERNAL subtitle prefers this file's sidecar of the same language, because the
/// memo says what kind of file the viewer chose. The embedded track of that language is only the
/// answer when no sidecar is (an embedded memo, which never sees a sidecar, is unchanged).
#[test]
fn an_external_memo_takes_the_same_language_sidecar_before_an_embedded_track() {
    let mut subs = vec![sub(1, "eng", ""), sub(2, "eng", "")];
    subs[1].external = true;
    let external = SubtitleTrackMemo { external: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs, &external, anyone), Ok(1), "the sidecar, though the embedded track is first");
    assert_eq!(match_subtitle(&subs, &memo_sub("eng"), anyone), Ok(0), "an embedded memo still takes the embedded track");
    assert_eq!(
        match_subtitle(&subs[..1], &external, anyone),
        Ok(0),
        "no sidecar on this file: the same-language embedded track, as before"
    );
}

/// C5: language and `forced` stay hard for a sidecar. Another language's file is never the answer,
/// and a forced sidecar is not the full one (nor the reverse).
#[test]
fn an_external_memo_never_takes_another_language_or_the_other_forced_sidecar() {
    let mut subs = vec![sub(1, "spa", ""), sub(2, "eng", "Forced")];
    subs[0].external = true;
    subs[1].external = true;
    let external = SubtitleTrackMemo { external: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs[..1], &external, anyone), Err("language"), "a Spanish sidecar is not English");
    assert_eq!(match_subtitle(&subs, &external, anyone), Err("forced"), "the forced sidecar is not the full one");
    let forced = SubtitleTrackMemo { external: true, forced: true, ..memo_sub("eng") };
    assert_eq!(match_subtitle(&subs, &forced, anyone), Ok(1));
    assert_eq!(match_subtitle(&subs[..1], &forced, anyone), Err("language"));
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

// ---- Forced only once the container's names are known (FO, §4.9) -------------------------------

fn names(subs: &[&str]) -> TrackNames {
    TrackNames { audio: Vec::new(), subs: subs.iter().map(|s| s.to_string()).collect() }
}

/// An MP4 part: PMS gives its subtitle streams no `title`, so the file's own track name is the only
/// thing that says which one is forced. The resolve (no names yet) finds none; the names do.
#[test]
fn forced_only_pick_finds_a_track_only_the_containers_name_reveals() {
    let subs = vec![sub(1, "eng", ""), sub(2, "eng", "")];
    assert_eq!(match_forced_only(&subs, "eng", "", anyone), None, "setup: the streams alone name no forced track");
    assert_eq!(forced_only_pick(&subs, &names(&["", ""]), "eng", ""), None, "names that say nothing find nothing");
    assert_eq!(forced_only_pick(&subs, &names(&["English", "English (Forced)"]), "eng", ""), Some(1));
    assert_eq!(
        forced_only_pick(&subs, &names(&["", "Forced"]), "spa", ""),
        None,
        "and the audio's language still decides: Spanish audio, an English forced track, nothing"
    );
}

/// A sidecar is not in the container (its ordinal names no track), and an embedded track in a codec
/// the demuxer does not render is not a candidate.
#[test]
fn forced_only_pick_never_takes_a_sidecar_or_an_undrawable_codec() {
    let mut subs = vec![sub(1, "eng", "Forced"), sub(2, "eng", "Forced")];
    subs[0].external = true;
    subs[1].codec = "dvb_teletext".into();
    assert_eq!(forced_only_pick(&subs, &names(&["Forced"]), "eng", ""), None);
    subs[1].codec = "srt".into();
    assert_eq!(forced_only_pick(&subs, &names(&["Forced"]), "eng", ""), Some(1));
}

/// **The first decoded frame's re-match**: what the memo picks over the named streams, when it
/// differs from what the landing showed. Nothing when it is the same, and nothing for Off.
#[test]
fn rematch_with_names_reports_a_change_only() {
    let subs = vec![sub(55001, "eng", ""), sub(55003, "eng", "")];
    let forced_only = SubtitleMemo::ForcedOnly { lang: "eng".into() };
    let named = names(&["", "Forced"]);
    assert_eq!(
        rematch_with_names(&subs, &named, &forced_only, "eng", 0),
        Some(Rematched { id: 55003, ordinal: 1 }),
        "the landing showed nothing, the names reveal the forced track"
    );
    assert_eq!(rematch_with_names(&subs, &named, &forced_only, "eng", 55003), None, "already on screen: no change");
    assert_eq!(rematch_with_names(&subs, &named, &SubtitleMemo::Off, "eng", 55001), None, "Off has nothing to find");

    let forced_track = SubtitleMemo::Track(SubtitleTrackMemo { forced: true, ..memo_sub("eng") });
    assert_eq!(
        rematch_with_names(&subs, &named, &forced_track, "eng", 55001),
        Some(Rematched { id: 55003, ordinal: 1 }),
        "a remembered FORCED track is found by the same names"
    );
    let japanese = SubtitleMemo::Track(memo_sub("jpn"));
    assert_eq!(
        rematch_with_names(&subs, &named, &japanese, "eng", 0),
        None,
        "a track memo that still matches nothing leaves the landing alone"
    );
}

/// Forced only can come to NOTHING: the landing showed a stream, and with the names there is no
/// forced track in the playing audio's language, so the answer is `id 0` (never a full track).
#[test]
fn rematch_with_names_for_forced_only_can_come_to_nothing() {
    let subs = vec![sub(55001, "eng", ""), sub(55003, "spa", "")];
    let forced_only = SubtitleMemo::ForcedOnly { lang: String::new() };
    assert_eq!(
        rematch_with_names(&subs, &names(&["", "Forced"]), &forced_only, "eng", 55001),
        Some(Rematched { id: 0, ordinal: -1 }),
        "English audio, only a Spanish forced track: nothing, however the landing came to show 55001"
    );
}

// ---- the write half: a memo built from a pick matches that pick back --------------------------

/// A file whose subtitle names exercise every reading the menu makes: a plain track, a flagged
/// forced one, a track the NAME makes forced (no flag), an SDH one by name, a sidecar and a bitmap.
fn menu_subtitles() -> Vec<Stream> {
    let mut subs = vec![
        sub(1, "eng", "English"),
        sub(2, "eng", "English forced"),
        sub(3, "eng", "Forced"),
        sub(4, "eng", "English SDH"),
        sub(5, "spa", "Spanish"),
    ];
    subs[1].forced = true;
    subs[3].codec = "pgs".into();
    subs[4].external = true;
    subs
}

/// **The memo is the menu's own reading.** Every subtitle of the file, remembered through
/// [`memo_of_subtitle`] and matched back with [`match_subtitle`], is found again: the matcher's hard
/// constraint on `forced` is read from `sub_traits` (the menu's FORCED badge, which a NAME can earn),
/// and a memo that took the raw `Stream.forced` instead would write `forced: false` for the track
/// titled "Forced" and the next episode's hard constraint would decline that very track.
#[test]
fn a_memo_written_from_a_subtitle_pick_matches_that_subtitle_back() {
    let subs = menu_subtitles();
    for (i, s) in subs.iter().enumerate() {
        let SubtitleMemo::Track(memo) = memo_of_subtitle(s) else { panic!("a track is remembered as a Track") };
        assert_eq!(match_subtitle(&subs, &memo, anyone), Ok(i), "track {i} ({:?}) is found again by {memo:?}", s.title);
    }
}

/// What each field of a subtitle memo says, for the readings that are not the raw stream's.
#[test]
fn a_subtitle_memo_records_forced_sdh_kind_and_sidecar_as_the_menu_reads_them() {
    let subs = menu_subtitles();
    let track = |i: usize| match memo_of_subtitle(&subs[i]) {
        SubtitleMemo::Track(t) => t,
        other => panic!("{other:?}"),
    };
    assert!(track(2).forced && !subs[2].forced, "titled Forced, flag unset: remembered forced");
    assert!(track(1).forced, "flagged forced");
    assert!(!track(0).forced);
    assert!(track(3).sdh, "titled SDH: remembered SDH");
    assert_eq!(track(3).kind, "image", "a PGS track is a bitmap");
    assert_eq!(track(0).kind, "text");
    assert!(track(4).external && !track(0).external, "a sidecar is told from an embedded track");
    assert_eq!((track(4).lang.as_str(), track(4).title.as_str()), ("spa", "Spanish"));
}

/// **The audio memo is the planner's own reading too**: a commentary-titled track that shares its
/// language with another is a commentary, and the memo says so, so it is matched back (and the main
/// track is not taken for it). An audio-description track is told from a plain one.
#[test]
fn a_memo_written_from_an_audio_pick_matches_that_track_back() {
    let mut described = audio(4, "eng", "ac3", 2, "");
    described.ad = true;
    let tracks = vec![
        audio(1, "eng", "eac3", 6, ""),
        audio(2, "eng", "ac3", 2, "Director's Commentary"),
        audio(3, "jpn", "eac3", 6, ""),
        described,
    ];
    let commentary_of = |i: usize| super::super::plan::audio_is_commentary(&tracks, i);
    for i in 0..tracks.len() {
        let memo = memo_of_audio(&tracks, i).expect("a track of the file");
        assert_eq!(match_audio(&tracks, &memo, commentary_of), Ok(i), "track {i} is found again by {memo:?}");
    }
    assert!(memo_of_audio(&tracks, 1).unwrap().commentary, "a commentary that shares its language is a commentary");
    assert!(!memo_of_audio(&tracks, 0).unwrap().commentary);
    assert!(memo_of_audio(&tracks, 3).unwrap().ad);
    assert_eq!(memo_of_audio(&tracks, 2).unwrap(), memo_audio("jpn", "eac3", 6), "language, codec, channels");
}

/// The only track of its language is just that language's audio, however it is titled: the same
/// notion the planner applies, so the memo does not call it a commentary.
#[test]
fn a_lone_commentary_titled_track_is_remembered_as_its_languages_audio() {
    let tracks = vec![audio(1, "eng", "eac3", 6, ""), audio(2, "jpn", "ac3", 2, "Commentary")];
    assert!(!memo_of_audio(&tracks, 1).unwrap().commentary);
    let commentary_of = |i: usize| super::super::plan::audio_is_commentary(&tracks, i);
    assert_eq!(match_audio(&tracks, &memo_of_audio(&tracks, 1).unwrap(), commentary_of), Ok(1));
}

#[test]
fn an_audio_index_that_names_no_track_is_remembered_as_nothing() {
    let tracks = vec![audio(1, "eng", "eac3", 6, "")];
    assert_eq!(memo_of_audio(&tracks, 1), None);
    assert_eq!(memo_of_audio(&[], 0), None);
}
