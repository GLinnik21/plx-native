//! Audio and subtitle track selection tests: the English-preference/server-selection
//! ladder, direct-play-eligible fallbacks, and subtitle stream id resolution.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn an_empty_track_list_falls_back_to_the_codec_default() {
    assert_eq!(
        pick_dp_audio(&[], "ac3").map(|(i, c, _)| (i, c)),
        Some((-1, "ac3".into()))
    );
    assert!(
        pick_dp_audio(&[], "truehd").is_none(),
        "a non-direct-playable default must transcode"
    );
}


#[test]
fn english_wins_over_the_files_default_track() {
    // The Office ships a Russian "kubik" track flagged default; we must not open in it.
    let tracks = [trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
}


#[test]
fn the_flagged_default_wins_when_no_english_track_is_direct_playable() {
    let tracks = [trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2)));
}


#[test]
fn smart_dp_takes_a_playable_sibling_over_a_non_playable_default() {
    // A 4K HEVC item: TrueHD default + an AC3 sibling — direct-play beats the server's
    // video-downscaling transcode.
    let tracks = [trk(1, "truehd", "eng", true), trk(2, "ac3", "eng", false)];
    assert_eq!(pick_dp_audio(&tracks, "truehd"), Some((1, "ac3".into(), 2)));
}


#[test]
fn no_direct_playable_track_means_transcode() {
    let tracks = [trk(1, "truehd", "eng", true), trk(2, "dts", "eng", false)];
    assert!(pick_dp_audio(&tracks, "truehd").is_none());
}


#[test]
fn the_servers_selected_track_outranks_the_english_preference() {
    // A user picks the second Russian dub on their phone. English is still the
    // FIRST direct-playable track, so the old ladder handed back English on every play.
    let tracks = [
        trk(2693, "ac3", "rus", true),
        server_selected(trk(2694, "ac3", "rus", false)),
        trk(2695, "ac3", "eng", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2694)));
}


#[test]
fn a_selection_that_only_echoes_the_files_default_does_not_beat_english() {
    // THE gate that keeps the English rung alive. PMS reports a selected audio stream on
    // every part — for one nobody has touched it is just the container's default flag coming
    // back (The Morning Show: the Russian default reads `selected`). Treating that as a
    // choice would reinstate exactly the foreign-dub-on-open bug rung 2 exists to prevent.
    let tracks = [
        server_selected(trk(10975, "eac3", "rus", true)),
        trk(10976, "eac3", "eng", false),
    ];
    assert_eq!(
        pick_dp_audio(&tracks, "eac3"),
        Some((1, "eac3".into(), 10976))
    );
}


#[test]
fn a_selected_track_that_cannot_direct_play_falls_through_to_the_ladder() {
    // A live shape off the server: it holds the English DTS track (a real pick — it is
    // not the file default), which this pipeline cannot decode. Honouring it would force a
    // whole-video transcode for one audio track, so the ladder runs on instead.
    let tracks = [
        trk(2663, "ac3", "rus", true),
        server_selected(trk(2669, "dca", "eng", false)),
        trk(2673, "ac3", "eng", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "dca"), Some((2, "ac3".into(), 2673)));
}


/// The 720p re-encode must name the selected DTS, not the Russian AC3 sibling smart-DP
/// would copy. Remux still names that sibling — a copy of DTS would not play.
#[test]
fn a_reencode_keeps_the_selected_dts_instead_of_the_ac3_sibling() {
    let tracks = [
        trk(2663, "ac3", "rus", true),
        server_selected(trk(2669, "dca", "eng", false)),
    ];
    let dp = pick_dp_audio(&tracks, "dca")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 2663, "smart-DP sibling is the Russian AC3");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks),
        2663,
        "remux copies the sibling"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        2669,
        "cold re-encode keeps the selected DTS"
    );
    assert_eq!(
        encode_audio_id(false, dp, 2669, &tracks),
        2669,
        "a retry/session pick of that DTS is kept"
    );
    assert_eq!(
        encode_audio_id(true, dp, 2669, &tracks),
        2663,
        "remux still copies the sibling even when a DTS pick is in env"
    );
    assert_eq!(
        encode_audio_id(false, dp, dp, &tracks),
        dp,
        "retry after remux keeps the sibling already playing"
    );
}


/// A selected flag that only echoes the container default is not a 720p pick. Treating it
/// as one would open The Morning Show in the Russian default the English rung exists to skip.
#[test]
fn a_reencode_does_not_treat_a_default_echo_as_a_pick() {
    let tracks = [
        server_selected(trk(10975, "eac3", "rus", true)),
        trk(10976, "eac3", "eng", false),
    ];
    let dp = pick_dp_audio(&tracks, "eac3")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 10976, "smart-DP / pref-lang sibling is English");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks),
        10976,
        "remux copies English"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        10976,
        "cold re-encode keeps English, not the echoed Russian default"
    );
}


/// Live three-track shape: selected English DTS plus an English AC3 sibling. Remux copies
/// the AC3; re-encode names the DTS.
#[test]
fn a_reencode_names_selected_dts_not_the_english_ac3_sibling() {
    let tracks = [
        trk(2663, "ac3", "rus", true),
        server_selected(trk(2669, "dca", "eng", false)),
        trk(2673, "ac3", "eng", false),
    ];
    let dp = pick_dp_audio(&tracks, "dca")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 2673, "smart-DP sibling is the English AC3");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks),
        2673,
        "remux copies the English AC3"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        2669,
        "cold re-encode keeps the selected DTS"
    );
}


/// Unselected English DTS beside a Russian AC3 sibling: remux still copies the sibling, but
/// a re-encode can consume the English track PREF_AUDIO_LANG would have taken if it were DP.
#[test]
fn a_reencode_names_pref_lang_dts_when_the_sibling_is_foreign() {
    let tracks = [
        server_selected(trk(2663, "ac3", "rus", true)),
        trk(2669, "dca", "eng", false),
    ];
    let dp = pick_dp_audio(&tracks, "dca")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 2663, "smart-DP sibling is the Russian AC3");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks),
        2663,
        "remux copies the sibling"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        2669,
        "cold re-encode names unselected English DTS, not the Russian AC3"
    );
}


/// First-English-any-codec would PUT TrueHD here. The sibling is already English, so 720p
/// keeps that AC3 copy instead of re-encoding lossless.
#[test]
fn a_reencode_keeps_an_english_ac3_sibling_over_truehd() {
    let tracks = [
        server_selected(trk(1, "truehd", "eng", true)),
        trk(2, "ac3", "eng", false),
    ];
    let dp = pick_dp_audio(&tracks, "truehd")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 2, "smart-DP sibling is the English AC3");
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        2,
        "re-encode must not replace the English AC3 with TrueHD"
    );
}


/// No AC3 sibling: smart-DP has nothing to copy. A real selected DTS must still be named,
/// not omitted (PUT 0 encodes the TrueHD default).
#[test]
fn a_reencode_names_selected_dts_when_there_is_no_ac3_sibling() {
    let tracks = [
        trk(1, "truehd", "eng", true),
        server_selected(trk(2, "dca", "eng", false)),
    ];
    let dp = pick_dp_audio(&tracks, "truehd")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 0, "no direct-playable track");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks),
        0,
        "remux has no sibling to name"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks),
        2,
        "cold re-encode names the selected DTS, not 0"
    );
}


/// The whole ladder, rung by rung, with the selected flag switched on and off — the order is
/// the contract, and every row here is a shape the live server actually serves.
#[test]
fn the_audio_ladder_walks_its_rungs_in_order() {
    let cases: [(
        &str,
        Vec<crate::metadata::Stream>,
        &str,
        Option<(i32, String, i64)>,
    ); 7] = [
        (
            "rung 1: a real server pick wins even against English",
            vec![
                trk(1, "eac3", "rus", true),
                server_selected(trk(2, "eac3", "deu", false)),
                trk(3, "eac3", "eng", false),
            ],
            "eac3",
            Some((1, "eac3".into(), 2)),
        ),
        (
            "rung 1 needs a real pick: the default echoed back is not one",
            vec![
                server_selected(trk(1, "eac3", "rus", true)),
                trk(2, "eac3", "eng", false),
            ],
            "eac3",
            Some((1, "eac3".into(), 2)),
        ),
        (
            "rung 1 is skipped when the pick can't direct-play, not obeyed by transcoding",
            vec![
                trk(1, "ac3", "rus", true),
                server_selected(trk(2, "dca", "eng", false)),
                trk(3, "ac3", "eng", false),
            ],
            "ac3",
            Some((2, "ac3".into(), 3)), // rung 2 (English) still applies
        ),
        (
            "rung 2: no selection at all → the English preference, as before",
            vec![trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)],
            "ac3",
            Some((1, "ac3".into(), 2)),
        ),
        (
            "rung 3: no English → the file's flagged default",
            vec![trk(1, "ac3", "deu", false), trk(2, "ac3", "fra", true)],
            "ac3",
            Some((1, "ac3".into(), 2)),
        ),
        (
            "rung 4: a selected non-DP track with only a foreign DP sibling — smart-DP",
            vec![
                server_selected(trk(1, "truehd", "eng", false)),
                trk(2, "ac3", "fra", false),
            ],
            "truehd",
            Some((1, "ac3".into(), 2)),
        ),
        (
            "nothing direct-playable, selected or not → transcode",
            vec![
                server_selected(trk(1, "truehd", "eng", false)),
                trk(2, "dts", "rus", true),
            ],
            "truehd",
            None,
        ),
    ];
    for (what, tracks, acodec, want) in cases {
        assert_eq!(pick_dp_audio(&tracks, acodec), want, "{what}");
    }
}


#[test]
fn the_selected_subtitle_resolves_to_the_renderers_embedded_ordinal() {
    // Document order is NOT container order and a sidecar sits in the middle of the list:
    // the renderer counts only embedded streams, sorted on PMS `Stream.index` — the same
    // identifier space the track menu commits (metadata::sub_render_ordinal).
    let subs = [
        sub(10, 7, "fra", true),  // sidecar — not in the container, not counted
        sub(11, 3, "rus", false), // embedded, container-first
        server_selected(sub(12, 4, "eng", false)),
    ];
    assert_eq!(pick_dp_subtitle(&subs), Some((12, 1)));
}


#[test]
fn an_external_selected_subtitle_is_left_off() {
    // A sidecar has no demux ordinal; forcing a transcode to obey a stored
    // flag is not a trade the user asked for, so the direct-play path leaves subs off.
    let subs = [
        server_selected(sub(10, 3, "eng", true)),
        sub(11, 4, "rus", false),
    ];
    assert_eq!(pick_dp_subtitle(&subs), None);
}


#[test]
fn mde_subtitle_stream_id_names_advertised_codecs_and_zeroes_the_rest() {
    assert_eq!(mde_subtitle_stream_id(&[]), 0);
    assert_eq!(
        mde_subtitle_stream_id(&[server_selected(sub(10, 3, "eng", true))]),
        0,
        "sidecar → 0"
    );
    let mut pgs = server_selected(sub(12, 4, "eng", false));
    pgs.codec = "pgs".into();
    assert_eq!(mde_subtitle_stream_id(&[pgs]), 12);
    let mut mov = server_selected(sub(14, 4, "eng", false));
    mov.codec = "mov_text".into();
    assert_eq!(mde_subtitle_stream_id(&[mov]), 14);
    let mut dvd = server_selected(sub(15, 4, "eng", false));
    dvd.codec = "dvd_subtitle".into();
    assert_eq!(mde_subtitle_stream_id(&[dvd]), 15);
    let mut obscure = server_selected(sub(13, 4, "eng", false));
    obscure.codec = "vplayer".into();
    assert_eq!(
        mde_subtitle_stream_id(&[obscure]),
        0,
        "unadvertised but still client-rendered → 0 so MDE does not transcode"
    );
}


#[test]
fn no_selected_subtitle_means_subtitles_stay_off() {
    assert_eq!(pick_dp_subtitle(&[]), None);
    let subs = [sub(10, 3, "eng", false), sub(11, 4, "rus", false)];
    assert_eq!(
        pick_dp_subtitle(&subs),
        None,
        "the file's own tracks are not an instruction"
    );
}


#[test]
fn a_selection_with_no_stream_id_is_left_off_rather_than_half_applied() {
    // id and ordinal travel together: the id is what the menu checkmark and the timeline
    // report key on, so an id-less stream would render subtitles while the menu said Off.
    let subs = [server_selected(sub(0, 3, "eng", false))];
    assert_eq!(pick_dp_subtitle(&subs), None);
}

