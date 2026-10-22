//! The `0` sentinel: a re-resolve of an item whose plan has not landed yet must not read the
//! session's cleared track selection as the viewer's choice.
//!
//! `request_play_inner` resets `cur_audio` / `cur_sub_sid` to 0 for the incoming item, and 0 means
//! both "subtitles Off" and "nothing landed yet". A retry carries what the session held when it
//! was built, so one built in that window used to force subtitles OFF (overriding the account
//! default) and audio to the server default. The retry now carries a selection only when one was
//! KNOWN (`PlaybackSession::selection_known`): a playable landing, or a viewer pick.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;

/// The environment a resolve worker would be handed for `retry`: a fresh snapshot taken AFTER the
/// request cleared the selection, with the retry applied over it.
fn env_after(ps: &PlaybackSession, retry: RetryContext) -> ResolveEnv {
    let mut env = ResolveEnv::snapshot(
        ps,
        plx_data::stores::metadata::MetadataStore::default().view(),
        unregistered_sid(),
        "rk-selection",
    );
    apply_retry(&mut env, retry);
    env
}

/// The retry `retry_current_play` resolves: built BEFORE the request clears the selection, applied
/// to the environment snapshotted AFTER it.
fn resolve_env_of_retry(ps: &mut PlaybackSession, direct_play: Option<DirectPlayMode>) -> ResolveEnv {
    let retry = retry_context_with(ps, 0, direct_play);
    clear_track_selection(ps);
    env_after(ps, retry)
}

/// A playable landing that carries audio 17 and subtitle `sub_sid`.
fn playable(sub_sid: i64) -> Plan {
    Plan {
        url: "http://fixture.invalid/selection.mkv".into(),
        audio: Some(CarriedAudio::named(17, -1)),
        sub_sid,
        ..Default::default()
    }
}

/// A quality change, a Retry or a contract bump while the plan is still loading: the new request
/// has cleared the selection and nothing has landed, so the re-resolve leaves the fresh defaults
/// (the account's subtitle rule) in charge instead of forcing subtitles Off.
#[test]
fn contract_bump_before_landing_keeps_the_account_subtitle() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    // The outgoing item had a subtitle; a new play request retires it.
    apply_plan(&mut ps, playable(23), "rk-previous");
    clear_track_selection(&mut ps);
    assert_eq!(cur_sub_sid(&ps), 0, "the request has cleared the selection; nothing has landed");

    // The contract bump re-resolves via `current_retry_context`.
    let env = env_after(&ps, current_retry_context(&ps, 0));

    assert_eq!(
        env.subtitle_override, None,
        "nothing landed: the account's subtitle rule decides, subtitles are not forced Off",
    );
    assert_eq!((env.audio_sid, env.sub_sid), (0, 0), "the fresh defaults stand");
    reset_session(&mut ps);
}

/// The Forced refusal installs a plan with no URL (its verdict still reaches the read-out), and
/// *Switch to Auto and play* retries it. That landing selected nothing, so the retry must not pin
/// subtitles Off, and must still resolve under Auto.
#[test]
fn forced_refusal_then_switch_to_auto_keeps_the_subtitle() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(
        &mut ps,
        Plan {
            direct_play_mode: DirectPlayMode::Forced,
            verdict: Some(PlayVerdict::Forced(ForcedFailure::AudioNeedsConversion { codec: String::new() })),
            ..Default::default()
        },
        "rk-forced",
    );
    assert!(!has_url(&ps), "a refusal has no playable URL");

    let env = resolve_env_of_retry(&mut ps, Some(DirectPlayMode::Auto));

    assert_eq!(env.direct_play_mode, DirectPlayMode::Auto, "the retry resolves under Auto");
    assert_eq!(env.subtitle_override, None, "a refused plan chose no subtitle, so none is pinned");
    assert_eq!((env.audio_sid, env.sub_sid), (0, 0));
    reset_session(&mut ps);
}

/// A landed playback's retry carries what is playing: the burned subtitle and the carried audio.
#[test]
fn landed_burned_retry_keeps_cur_sub_sid() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, playable(50), "rk-burned");

    let env = resolve_env_of_retry(&mut ps, None);

    assert_eq!(env.subtitle_override, Some(50));
    assert_eq!((env.audio_sid, env.sub_sid), (17, 50));
    reset_session(&mut ps);
}

/// A landing whose plan selected NO subtitle is a known Off: the retry keeps it Off.
#[test]
fn landed_off_retry_stays_off() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, playable(0), "rk-off");

    let env = resolve_env_of_retry(&mut ps, None);

    assert_eq!(env.subtitle_override, Some(0), "a known Off is carried, as the viewer left it");
    assert_eq!((env.audio_sid, env.sub_sid), (17, 0));
    reset_session(&mut ps);
}

/// A viewer's pick is a known selection even where no playable plan ever landed: Off chosen from
/// the track menu survives a retry (it is the viewer's word, not the sentinel).
#[test]
fn a_viewer_subtitle_pick_is_known_without_a_landing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, Plan::default(), "rk-no-landing");
    commit_subtitle_selection(&mut ps, -1, 0, false);

    let env = resolve_env_of_retry(&mut ps, None);

    assert_eq!(env.subtitle_override, Some(0), "the viewer's Off is carried");
    reset_session(&mut ps);
}

/// The audio twin of the above: a viewer's audio pick marks the selection known.
#[test]
fn a_viewer_audio_pick_is_known_without_a_landing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, Plan::default(), "rk-no-landing");
    commit_audio_selection(&mut ps, CarriedAudio::named(31, -1));

    let env = resolve_env_of_retry(&mut ps, None);

    assert_eq!(env.audio_sid, 31, "the viewer's audio pick is carried");
    assert_eq!(env.subtitle_override, Some(0), "the pair is carried together");
    reset_session(&mut ps);
}

/// What `retry_current_play` does to the session: build the retry, then let the request clear the
/// selection while keeping the pair the retry carried.
fn request_retry(ps: &mut PlaybackSession) -> RetryContext {
    let retry = retry_context_with(ps, 0, None);
    begin_track_selection(ps, Some(retry));
    retry
}

/// The viewer chose subtitles Off on a landed item, pressed Retry, and changed quality while that
/// retry loads: the contract bump's retry must still carry the Off (the account's subtitle rule
/// must not come back on).
#[test]
fn an_explicit_off_survives_a_contract_bump_during_a_retry() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, playable(0), "rk-off-retry");
    request_retry(&mut ps);
    assert!(!ps.selection_known, "the retry's request has cleared the live selection");

    let retry = current_retry_context(&ps, 0);

    assert_eq!(retry.selection, Some((17, 0)), "the retry's own pair is carried on");
    assert_eq!(env_after(&ps, retry).subtitle_override, Some(0), "the explicit Off holds");
    reset_session(&mut ps);
}

/// A landed subtitle, a retry, a refusal of that retry, then *Switch to Auto and play*: the refused
/// landing selected nothing, so the second retry still carries the first one's subtitle.
#[test]
fn a_refused_retry_keeps_the_selection_it_carried() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, playable(23), "rk-refused-retry");
    request_retry(&mut ps);
    apply_plan(
        &mut ps,
        Plan {
            direct_play_mode: DirectPlayMode::Forced,
            verdict: Some(PlayVerdict::Forced(ForcedFailure::AudioNeedsConversion { codec: String::new() })),
            ..Default::default()
        },
        "rk-refused-retry",
    );
    assert!(!has_url(&ps), "a refusal has no playable URL");

    let env = resolve_env_of_retry(&mut ps, Some(DirectPlayMode::Auto));

    assert_eq!(env.direct_play_mode, DirectPlayMode::Auto);
    assert_eq!(env.subtitle_override, Some(23), "the subtitle the first retry carried survives");
    assert_eq!((env.audio_sid, env.sub_sid), (17, 23));
    reset_session(&mut ps);
}

/// A NEW item's request carries nothing, however the previous one ended.
#[test]
fn a_new_item_request_carries_no_selection() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, playable(23), "rk-previous");
    request_retry(&mut ps);
    begin_track_selection(&mut ps, None);

    assert_eq!(current_retry_context(&ps, 0).selection, None);
    reset_session(&mut ps);
}
