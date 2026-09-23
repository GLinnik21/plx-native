//! Audio and subtitle track selection tests: the server-selection/file-default
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


/// **With no Plex language preference, the file's default track wins — English has no say.**
/// Issue #202: a French MKV whose default-flagged track is French, beside an English one, opened
/// in English because of a built-in English rung. Both orderings, so the pick is the flag and
/// not the list position.
#[test]
fn the_files_default_track_wins_without_a_language_preference() {
    let tracks = [trk(1, "ac3", "fre", true), trk(2, "ac3", "eng", false)];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((0, "ac3".into(), 1)));
    let tracks = [trk(1, "ac3", "eng", false), trk(2, "eac3", "fre", true)];
    assert_eq!(pick_dp_audio(&tracks, "eac3"), Some((1, "eac3".into(), 2)));
}


/// The re-encode half of #202: a video transcode of the same French file must not swap the
/// French default for an English track either — it keeps what direct play would have chosen.
#[test]
fn a_reencode_without_a_preference_keeps_the_files_default_language() {
    let tracks = [trk(1, "ac3", "fre", true), trk(2, "dca", "eng", false)];
    let dp = pick_dp_audio(&tracks, "ac3").map(|(_, _, id)| id).unwrap_or(0);
    assert_eq!(dp, 1, "direct play takes the French default");
    assert_eq!(encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()), 1,
        "a cold re-encode names the French default, not the English DTS");
}

#[test]
fn account_audio_language_selects_french_for_every_cold_play_path() {
    let tracks = [server_selected(trk(1, "ac3", "eng", true)),
        trk(2, "ac3", "fre", false)];
    let prefs = AudioLangPrefs { show: None, account: Some("fr") };
    let (_, _, dp) = pick_dp_audio_pref(&tracks, "ac3", prefs).unwrap();
    assert_eq!(dp, 2, "direct play should honour defaultAudioLanguage");
    assert_eq!(encode_audio_id(true, dp, 0, &tracks, prefs), 2,
        "remux copies the account-language track");
    assert_eq!(encode_audio_id(false, dp, 0, &tracks, prefs), 2,
        "re-encode names the account-language track");
}

#[test]
fn account_audio_language_diagnostic_covers_every_outcome() {
    let no_tracks = [];
    assert_eq!(
        account_audio_language_log(&AccountAudioLanguage::NoCredential, &no_tracks, None),
        "route: account audio language — no plex.tv credential for this profile",
    );
    assert_eq!(
        account_audio_language_log(&AccountAudioLanguage::Unavailable, &no_tracks, None),
        "route: account audio language — unavailable (request failed)",
    );
    assert_eq!(
        account_audio_language_log(&AccountAudioLanguage::TimedOut, &no_tracks, None),
        "route: account audio language — unavailable (timed out)",
    );
    let not_set = |auto_select_audio, stated_language: Option<&str>| {
        AccountAudioLanguage::NotSet {
            auto_select_audio, stated_language: stated_language.map(str::to_owned),
        }
    };
    assert_eq!(
        account_audio_language_log(&not_set(Some(true), None), &no_tracks, None),
        "route: account audio language — not set",
    );
    assert_eq!(
        account_audio_language_log(&not_set(Some(false), Some("ru")), &no_tracks, None),
        "route: account audio language — automatic audio selection off (language ru)",
    );
    assert_eq!(
        account_audio_language_log(&not_set(None, None), &no_tracks, None),
        "route: account audio language — automatic audio selection not reported (language not set)",
    );

    let account = AccountAudioLanguage::Set("fr".into());
    let tracks = [trk(1, "ac3", "eng", true), trk(2, "ac3", "fre", false)];
    assert_eq!(
        account_audio_language_log(&account, &tracks, Some(&(1, "ac3".into(), 2))),
        "route: account prefers audio fr — playing that track",
    );
    assert_eq!(
        account_audio_language_log(&account, &tracks, Some(&(0, "ac3".into(), 1))),
        "route: account prefers audio fr — outranked by the PMS selection or show preference",
    );

    let tracks = [trk(1, "ac3", "eng", true), trk(2, "dca", "fra", false)];
    assert_eq!(
        account_audio_language_log(&account, &tracks, Some(&(0, "ac3".into(), 1))),
        "route: account prefers audio fr — a track in it exists but is not direct-playable; using the usual order",
    );
    let tracks = [trk(1, "ac3", "eng", true)];
    assert_eq!(
        account_audio_language_log(&account, &tracks, Some(&(0, "ac3".into(), 1))),
        "route: account prefers audio fr — no track in it; using the usual order",
    );
}

/// PMS account auto-picks and manual picks are indistinguishable, so both outrank the show.
#[test]
fn pms_nondefault_selection_outranks_the_show_language() {
    let tracks = [trk(1, "ac3", "hun", true),
        server_selected(trk(2, "ac3", "eng", false))];
    let prefs = AudioLangPrefs { show: Some("hu-HU"), account: None };
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", prefs), Some((1, "ac3".into(), 2)));
}

#[test]
fn show_language_outranks_account_language() {
    let tracks = [trk(1, "ac3", "eng", true), trk(2, "ac3", "fra", false),
        trk(3, "ac3", "hun", false)];
    let prefs = AudioLangPrefs { show: Some("hu-HU"), account: Some("fr") };
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", prefs), Some((2, "ac3".into(), 3)));
    assert_eq!(encode_audio_id(false, 3, 0, &tracks, prefs), 3);
}

#[test]
fn unmatched_or_unset_account_language_falls_to_the_file_default() {
    let tracks = [trk(1, "ac3", "eng", true), trk(2, "ac3", "fra", false)];
    for account in [Some("de"), Some(""), Some("-1"), Some("  ")] {
        let prefs = AudioLangPrefs { show: None, account };
        assert_eq!(pick_dp_audio_pref(&tracks, "ac3", prefs), Some((0, "ac3".into(), 1)),
            "{account:?}");
        assert_eq!(encode_audio_id(false, 1, 0, &tracks, prefs), 1, "{account:?}");
    }
}

#[test]
fn account_language_matches_all_french_spellings() {
    for stream in ["fr", "fre", "fra"] {
        let tracks = [trk(1, "ac3", "eng", true), trk(2, "ac3", stream, false)];
        let prefs = AudioLangPrefs { show: None, account: Some("fr") };
        assert_eq!(pick_dp_audio_pref(&tracks, "ac3", prefs), Some((1, "ac3".into(), 2)),
            "{stream}");
        assert_eq!(encode_audio_id(false, 2, 0, &tracks, prefs), 2, "{stream}");
    }
}


/// A PMS selection the pipeline cannot decode (French DTS) is answered by a
/// direct-playable track in THAT language, not by the file's default in another one.
#[test]
fn a_non_playable_pick_is_answered_by_a_sibling_in_its_language() {
    let tracks = [
        trk(1, "ac3", "eng", true),
        server_selected(trk(2, "dca", "fre", false)),
        trk(3, "ac3", "fre", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((2, "ac3".into(), 3)));
}


#[test]
fn the_flagged_default_wins_over_an_earlier_track() {
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
fn the_servers_selected_track_outranks_the_files_default() {
    // A user picks the second Russian dub on their phone. The file's default is still the
    // FIRST direct-playable track, which the ladder would otherwise hand back on every play.
    let tracks = [
        trk(2693, "ac3", "rus", true),
        server_selected(trk(2694, "ac3", "rus", false)),
        trk(2695, "ac3", "eng", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((1, "ac3".into(), 2694)));
}


#[test]
fn a_selection_that_only_echoes_the_files_default_is_not_a_pick() {
    // PMS reports a selected audio stream on every part — for one nobody has touched it is
    // just the container's default flag coming back (The Morning Show: the Russian default
    // reads `selected`). The ladder lands on that default through rung 3 either way; what the
    // gate protects is a SHOW preference, which an echo must not outrank.
    let tracks = [
        server_selected(trk(10975, "eac3", "rus", true)),
        trk(10976, "eac3", "hun", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "eac3"), Some((0, "eac3".into(), 10975)));
    assert_eq!(
        pick_dp_audio_pref(&tracks, "eac3", AudioLangPrefs { show: Some("hu-HU"), account: None }),
        Some((1, "eac3".into(), 10976))
    );
}


#[test]
fn a_selected_track_that_cannot_direct_play_falls_through_to_the_ladder() {
    // A live shape off the server: it holds the English DTS track (a PMS selection — it is
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
        encode_audio_id(true, dp, 0, &tracks, AudioLangPrefs::default()),
        2663,
        "remux copies the sibling"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()),
        2669,
        "cold re-encode keeps the selected DTS"
    );
    assert_eq!(
        encode_audio_id(false, dp, 2669, &tracks, AudioLangPrefs::default()),
        2669,
        "a retry/session pick of that DTS is kept"
    );
    assert_eq!(
        encode_audio_id(true, dp, 2669, &tracks, AudioLangPrefs::default()),
        2663,
        "remux still copies the sibling even when a DTS pick is in env"
    );
    assert_eq!(
        encode_audio_id(false, dp, dp, &tracks, AudioLangPrefs::default()),
        dp,
        "retry after remux keeps the sibling already playing"
    );
}


/// A selected flag that only echoes the container default is not a 720p pick: with a show
/// preference set, treating it as one would name the Russian default over the preferred dub.
#[test]
fn a_reencode_does_not_treat_a_default_echo_as_a_pick() {
    let tracks = [
        server_selected(trk(10975, "eac3", "rus", true)),
        trk(10976, "eac3", "hun", false),
    ];
    let dp = pick_dp_audio_pref(&tracks, "eac3", AudioLangPrefs { show: Some("hu-HU"), account: None })
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 10976, "the show's language picks the Hungarian track");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks, AudioLangPrefs { show: Some("hu-HU"), account: None }),
        10976,
        "remux copies Hungarian"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs { show: Some("hu-HU"), account: None }),
        10976,
        "cold re-encode keeps Hungarian, not the echoed Russian default"
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
        encode_audio_id(true, dp, 0, &tracks, AudioLangPrefs::default()),
        2673,
        "remux copies the English AC3"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()),
        2669,
        "cold re-encode keeps the selected DTS"
    );
}


/// Unselected English DTS beside a Russian AC3 default, no preference: remux copies the
/// default, and a re-encode keeps it too — nothing asked for English (#202).
#[test]
fn a_reencode_keeps_the_default_over_an_unselected_foreign_dts() {
    let tracks = [
        server_selected(trk(2663, "ac3", "rus", true)),
        trk(2669, "dca", "eng", false),
    ];
    let dp = pick_dp_audio(&tracks, "dca")
        .map(|(_, _, id)| id)
        .unwrap_or(0);
    assert_eq!(dp, 2663, "smart-DP sibling is the Russian AC3");
    assert_eq!(
        encode_audio_id(true, dp, 0, &tracks, AudioLangPrefs::default()),
        2663,
        "remux copies the sibling"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()),
        2663,
        "cold re-encode keeps the Russian default, not the unselected English DTS"
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
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()),
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
        encode_audio_id(true, dp, 0, &tracks, AudioLangPrefs::default()),
        0,
        "remux has no sibling to name"
    );
    assert_eq!(
        encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs::default()),
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
            "rung 1: a PMS selection wins even against English",
            vec![
                trk(1, "eac3", "rus", true),
                server_selected(trk(2, "eac3", "deu", false)),
                trk(3, "eac3", "eng", false),
            ],
            "eac3",
            Some((1, "eac3".into(), 2)),
        ),
        (
            "rung 1 needs a non-default PMS selection: the default echo is not one",
            vec![
                server_selected(trk(1, "eac3", "rus", true)),
                trk(2, "eac3", "eng", false),
            ],
            "eac3",
            Some((0, "eac3".into(), 1)),
        ),
        (
            "rung 1 is skipped when the pick can't direct-play, not obeyed by transcoding",
            vec![
                trk(1, "ac3", "rus", true),
                server_selected(trk(2, "dca", "eng", false)),
                trk(3, "ac3", "eng", false),
            ],
            "ac3",
            Some((2, "ac3".into(), 3)), // rung 2: a DP track in the pick's language
        ),
        (
            "no selection at all → the file's default, never a built-in English (#202)",
            vec![trk(1, "ac3", "rus", true), trk(2, "ac3", "eng", false)],
            "ac3",
            Some((0, "ac3".into(), 1)),
        ),
        (
            "rung 3: the file's flagged default, wherever it sits",
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


/// **A show's preferred audio language beats the file's flag.** The reported case (#160): a
/// series set to Hungarian whose episodes carry the Hungarian dub as the FILE default beside an
/// English track opened in English, through a built-in English rung since removed (#202).
#[test]
fn a_shows_preferred_audio_language_is_honoured() {
    let tracks = [trk(1, "ac3", "hun", true), trk(2, "ac3", "eng", false)];
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", AudioLangPrefs { show: Some("hu-HU"), account: None }), Some((0, "ac3".into(), 1)));
    // …and the other way round: Hungarian second, English default — the preference moves it
    let tracks = [trk(1, "ac3", "eng", true), trk(2, "eac3", "hun", false)];
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", AudioLangPrefs { show: Some("hu-HU"), account: None }), Some((1, "eac3".into(), 2)));
    // a preferred track that cannot direct-play does not force a transcode
    let tracks = [trk(1, "ac3", "eng", true), trk(2, "dca", "hun", false)];
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", AudioLangPrefs { show: Some("hu-HU"), account: None }), Some((0, "ac3".into(), 1)));
    // a DIFFERENT track chosen for this episode elsewhere still outranks the show's default
    let tracks = [trk(1, "ac3", "hun", true), server_selected(trk(2, "ac3", "eng", false))];
    assert_eq!(pick_dp_audio_pref(&tracks, "ac3", AudioLangPrefs { show: Some("hu-HU"), account: None }), Some((1, "ac3".into(), 2)));
}

#[test]
fn preferred_language_tags_match_both_three_letter_spellings() {
    assert!(lang_matches("hu-HU", "hun"));
    assert!(lang_matches("de-DE", "ger") && lang_matches("de-DE", "deu"));
    assert!(lang_matches("pt-BR", "por"));
    assert!(lang_matches("en-GB", "eng"));
    assert!(!lang_matches("hu-HU", "eng"));
    assert!(!lang_matches("", "hun"));
    assert!(!lang_matches("hu-HU", ""));
}

/// **A show's subtitle settings turn a subtitle on at the start of a direct play.**
#[test]
fn a_shows_subtitle_settings_choose_the_starting_subtitle() {
    let prefs = |mode: i32| crate::plex::ShowLangPrefs {
        audio: None,
        subtitle: Some("hu-HU".into()),
        subtitle_mode: mode,
    };
    let forced = |mut s: crate::metadata::Stream| {
        s.forced = true;
        s
    };
    let subs = [sub(10, 3, "eng", false), forced(sub(11, 4, "hun", false)), sub(12, 5, "hun", false)];
    // always: the FULL Hungarian track, not the forced one before it
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(2), "hun"), Some((12, 2)));
    // shown with foreign audio: on for English audio, off for Hungarian audio
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(1), "eng"), Some((12, 2)));
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(1), "hun"), None);
    // manual and account default: nothing, as before
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(0), "eng"), None);
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(-1), "eng"), None);
    // a sidecar is not client-renderable on direct play, so it is never the pick
    let subs = [sub(10, 3, "eng", false), sub(20, 4, "hun", true)];
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(2), "eng"), None);
    // a subtitle the server already has selected is a choice: the show's default stays out
    let subs = [server_selected(sub(10, 3, "eng", false)), sub(12, 4, "hun", false)];
    assert_eq!(pick_dp_subtitle_pref(&subs, &prefs(2), "eng"), Some((10, 0)));
}

#[test]
fn show_settings_are_read_out_of_a_setting_list() {
    use crate::plex::{Setting, ShowLangPrefs};
    let s = |id: &str, v: &str| Setting { id: id.into(), value: v.into() };
    assert_eq!(ShowLangPrefs::from_settings(&[s("episodeSort", "-1")]), None);
    assert_eq!(
        ShowLangPrefs::from_settings(&[
            s("audioLanguage", "hu-HU"),
            s("subtitleLanguage", ""),
            s("subtitleMode", "1"),
        ]),
        Some(ShowLangPrefs { audio: Some("hu-HU".into()), subtitle: None, subtitle_mode: 1 })
    );
    assert_eq!(
        ShowLangPrefs::from_settings(&[s("audioLanguage", "")]),
        Some(ShowLangPrefs { audio: None, subtitle: None, subtitle_mode: -1 })
    );
}

#[test]
fn a_show_audio_preference_survives_a_video_transcode() {
    let tracks = [trk(1, "ac3", "hun", true), trk(2, "ac3", "eng", false)];
    let (_, _, dp) = pick_dp_audio_pref(&tracks, "ac3", AudioLangPrefs { show: Some("hu-HU"), account: None }).unwrap();
    assert_eq!(encode_audio_id(false, dp, 0, &tracks, AudioLangPrefs { show: Some("hu-HU"), account: None }), 1,
        "lowering video quality must not replace the show's chosen audio with English");
}

#[test]
fn transcode_show_preference_preserves_overrides_and_missing_language_fallback() {
    let tracks = [trk(1, "ac3", "hun", true), trk(2, "dts", "eng", false)];
    assert_eq!(encode_audio_id(false, 1, 2, &tracks, AudioLangPrefs { show: Some("hu-HU"), account: None }), 2);
    assert_eq!(encode_audio_id(true, 1, 2, &tracks, AudioLangPrefs { show: Some("en-US"), account: None }), 1);
    // no track in the show's language: the direct-play pick (the Hungarian default), not English
    assert_eq!(encode_audio_id(false, 1, 0, &tracks, AudioLangPrefs { show: Some("fr-FR"), account: None }), 1);
    let tracks = [trk(1, "ac3", "hun", true), server_selected(trk(2, "dts", "eng", false))];
    assert_eq!(encode_audio_id(false, 1, 0, &tracks, AudioLangPrefs { show: Some("hu-HU"), account: None }), 2);
}

/// **One precedence for every path.** A PMS selection the pipeline cannot decode (English DTS)
/// still outranks the show's language: direct play carries it as the English AC3
/// sibling, a re-encode names the DTS itself. The direct-play pick used to hand this case to the
/// show's Hungarian while the re-encode named the DTS — two answers to one question.
#[test]
fn a_real_pick_outranks_the_show_language_on_every_path() {
    let prefs = AudioLangPrefs { show: Some("hu-HU"), account: None };
    let tracks = [
        trk(1, "ac3", "hun", true),
        server_selected(trk(2, "dca", "eng", false)),
        trk(3, "ac3", "eng", false),
    ];
    let (_, _, dp) = pick_dp_audio_pref(&tracks, "ac3", prefs).unwrap();
    assert_eq!(dp, 3, "direct play: the English AC3 beside the picked DTS");
    assert_eq!(encode_audio_id(false, dp, 0, &tracks, prefs), 2, "re-encode: the picked DTS");
    assert_eq!(encode_audio_id(true, dp, 0, &tracks, prefs), 3, "remux copies the sibling");
}

/// A pick direct play cannot carry hands over to the NEXT entry in the ranking, not straight to
/// the file's default: French DTS picked, no playable French track, show set to Hungarian →
/// direct play and remux take the Hungarian AC3; a re-encode still encodes the French pick.
#[test]
fn an_uncarriable_pick_falls_to_the_show_language_before_the_default() {
    let prefs = AudioLangPrefs { show: Some("hu-HU"), account: None };
    let tracks = [
        trk(1, "ac3", "rus", true),
        server_selected(trk(2, "dca", "fre", false)),
        trk(3, "ac3", "hun", false),
    ];
    let (_, _, dp) = pick_dp_audio_pref(&tracks, "ac3", prefs).unwrap();
    assert_eq!(dp, 3, "direct play: the show's Hungarian, not the Russian default");
    assert_eq!(encode_audio_id(true, dp, 0, &tracks, prefs), 3, "remux copies it");
    assert_eq!(encode_audio_id(false, dp, 0, &tracks, prefs), 2, "re-encode: the French pick");
}

/// A picked stream and its sibling may spell one language two ways (`fre` / `fra`); the
/// sibling still counts as the pick's language.
#[test]
fn a_picks_sibling_matches_across_iso_639_spellings() {
    let tracks = [
        trk(1, "ac3", "eng", true),
        server_selected(trk(2, "dca", "fre", false)),
        trk(3, "ac3", "fra", false),
    ];
    assert_eq!(pick_dp_audio(&tracks, "ac3"), Some((2, "ac3".into(), 3)));
    assert!(lang_matches("fre", "fra") && lang_matches("ger", "deu") && lang_matches("no", "nob"));
    assert!(!lang_matches("fre", "eng") && !lang_matches("", "") && !lang_matches("xx", "yy"));
}

/// `""` and `"-1"` are Plex's "Account default": unset, so the file's default wins.
#[test]
fn an_account_default_show_setting_is_no_preference() {
    use crate::plex::{Setting, ShowLangPrefs};
    let s = |id: &str, v: &str| Setting { id: id.into(), value: v.into() };
    assert_eq!(
        ShowLangPrefs::from_settings(&[s("audioLanguage", "-1")]).and_then(|p| p.audio),
        None
    );
    let tracks = [trk(1, "ac3", "fre", true), trk(2, "ac3", "eng", false)];
    for show in ["", "-1", " -1 "] {
        let prefs = AudioLangPrefs { show: Some(show), account: None };
        assert_eq!(pick_dp_audio_pref(&tracks, "ac3", prefs), Some((0, "ac3".into(), 1)), "{show:?}");
        assert_eq!(encode_audio_id(false, 1, 0, &tracks, prefs), 1, "{show:?}");
    }
}
