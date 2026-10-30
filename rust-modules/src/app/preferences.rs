//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
use plx_media::route::{choose, Choice, How};
use plx_plex::plex::account::{PreferenceError, PreferenceRequest};
use plx_plex::plex::session::{InstantDpadSeekingPref, DeckPressPref, DirectPlayModePref, HdrOverlayBrightnessPref, NextEpisodeModePref, QualityPref,
    SkipIntervalPref, SubtitlePositionPref, SubtitleSizePref, TrailerAutoplayPref, UnderPostersPref};
use plx_screens::registry::{AccountPreferenceReply, PreferenceCmd};
use std::sync::mpsc::Sender;

/// The one place a Settings pick meets the write door: each row's own `SETTINGS` policy
/// (`How::settings`) decides whether the pick is confirmed first or published at once.
fn choose_setting(choice: Choice, reply: Sender<bool>) {
    match choice {
        Choice::Quality(v) => choose::<QualityPref>(v, How::settings::<QualityPref>(reply)),
        Choice::DirectPlay(v) => choose::<DirectPlayModePref>(v, How::settings::<DirectPlayModePref>(reply)),
        Choice::NextEpisode(v) => choose::<NextEpisodeModePref>(v, How::settings::<NextEpisodeModePref>(reply)),
        Choice::InstantDpadSeeking(v) => choose::<InstantDpadSeekingPref>(v, How::settings::<InstantDpadSeekingPref>(reply)),
        Choice::SkipInterval(v) => choose::<SkipIntervalPref>(v, How::settings::<SkipIntervalPref>(reply)),
        Choice::DeckPress(v) => choose::<DeckPressPref>(v, How::settings::<DeckPressPref>(reply)),
        Choice::UnderPosters(v) => choose::<UnderPostersPref>(v, How::settings::<UnderPostersPref>(reply)),
        Choice::TrailerAutoplay(v) => choose::<TrailerAutoplayPref>(v, How::settings::<TrailerAutoplayPref>(reply)),
        Choice::SubtitleSize(v) => choose::<SubtitleSizePref>(v, How::settings::<SubtitleSizePref>(reply)),
        Choice::SubtitlePosition(v) => choose::<SubtitlePositionPref>(v, How::settings::<SubtitlePositionPref>(reply)),
        Choice::HdrOverlayBrightness(v) => choose::<HdrOverlayBrightnessPref>(v, How::settings::<HdrOverlayBrightnessPref>(reply)),
    }
}

/// Write `user`'s way of signing in to Seerr (`None` is its Plex account): the record replaces the
/// stored one whole, session included.
fn keep_seerr_sign_in(user: &str, sign_in: Option<plx_plex::plex::session::SeerrSignIn>) -> bool {
    plx_plex::plex::session::set_seerr_sign_in(user, sign_in)
}

/// The Seerr store's changed session for the seated profile's Seerr account (`None` forgets it),
/// written on the storage worker. Nothing waits on it: the store already holds what it says.
pub(super) fn keep_seerr_session(session: Option<plx_plex::plex::seerr::SeerrSession>) {
    let current = plx_plex::plex::session::peek();
    let user = plx_plex::plex::session::current_profile_key();
    let Some(mine) = current.prefs.seerr_sign_ins.iter().find(|s| s.user == user).cloned() else { return };
    let address = current.prefs.seerr_address.clone();
    let next = plx_plex::plex::session::SeerrSignIn {
        address,
        cookie: session.map(|s| s.cookie().to_string()).unwrap_or_default(),
        ..mine
    };
    let _ = plx_base::storage_worker::submit_retained(move || {
        let _ = keep_seerr_sign_in(&user, Some(next));
    });
}

pub(super) fn execute(command: PreferenceCmd) {
    match command {
        PreferenceCmd::Load { reply } => {
            let Some(request) = PreferenceRequest::capture() else {
                let _ = reply.send(AccountPreferenceReply {
                    request: None, outcome: Err(PreferenceError::Unavailable),
                });
                plx_machine::idle::invalidate();
                return;
            };
            // A refused spawn drops the reply sender. The screen reports its disconnected
            // receipt as retryable without claiming that an account call ran.
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.load();
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Save { request, base, update, reply } => {
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.save(&base, update);
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        // Every live setting saves through the one write door, in `choose_setting`.
        PreferenceCmd::Choose(choice, reply) => choose_setting(choice, reply),
        PreferenceCmd::UiResolution { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_ui_resolution(mode));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Language { language, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_language(language));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::SeerrAddress { address, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_seerr_address(address.trim()));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::SeerrMethod { email, reply } => {
            let sign_in = email.map(|email| plx_plex::plex::session::SeerrSignIn {
                address: plx_plex::plex::session::peek().prefs.seerr_address.clone(),
                email: email.trim().to_string(),
                ..Default::default()
            });
            let user = plx_plex::plex::session::current_profile_key();
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(keep_seerr_sign_in(&user, sign_in));
                plx_machine::idle::invalidate();
            });
        }
        // Signing in steps the Seerr store, which the Bridge owns: it answers these itself
        // (`Bridge::app_effect`) and never hands them here.
        PreferenceCmd::SeerrConnect | PreferenceCmd::SeerrSignIn { .. } =>
            debug_assert!(false, "signing in to Seerr is the Bridge's"),
    }
}
