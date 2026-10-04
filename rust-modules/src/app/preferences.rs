//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
use plx_plex::plex::account::{PreferenceError, PreferenceRequest};
use crate::screens::registry::{AccountPreferenceReply, PreferenceCmd};

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
            plx_base::task::spawn_small("account preferences", move || {
                let outcome = request.load();
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Save { request, base, update, reply } => {
            plx_base::task::spawn_small("account preferences", move || {
                let outcome = request.save(&base, update);
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Quality { quality, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_media::route::set_default_quality(quality));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::Language { language, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_language(language));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::DirectPlay { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_media::route::set_direct_play_mode(mode));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::NextEpisode { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_media::route::set_next_episode_mode(mode));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::DeckPress { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_media::route::set_deck_press(mode));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::SkipInterval { interval, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_media::route::set_skip_interval(interval));
                plx_machine::idle::invalidate();
            });
        }
        // The optimistic picks run HERE, on the main thread: the live value is published before
        // anything is persisted and the persistence rides the storage worker on its own
        // (`route::select_subtitle_size`), so there is no outer worker submission to republish.
        PreferenceCmd::SubtitleSize { size, reply } => plx_media::route::select_subtitle_size(size, Some(reply)),
        PreferenceCmd::SubtitlePosition { position, reply } => {
            plx_media::route::select_subtitle_position(position, Some(reply))
        }
    }
}
