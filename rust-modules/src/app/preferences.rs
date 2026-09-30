//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
use crate::plex::account::{PreferenceError, PreferenceRequest};
use crate::screens::registry::{AccountPreferenceReply, PreferenceCmd};

pub(super) fn execute(command: PreferenceCmd) {
    match command {
        PreferenceCmd::Load { reply } => {
            let Some(request) = PreferenceRequest::capture() else {
                let _ = reply.send(AccountPreferenceReply {
                    request: None, outcome: Err(PreferenceError::Unavailable),
                });
                crate::ui::idle::invalidate();
                return;
            };
            // A refused spawn drops the reply sender. The screen reports its disconnected
            // receipt as retryable without claiming that an account call ran.
            crate::task::spawn_small("account preferences", move || {
                let outcome = request.load();
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::Save { request, base, update, reply } => {
            crate::task::spawn_small("account preferences", move || {
                let outcome = request.save(&base, update);
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::Quality { quality, reply } => {
            let _ = crate::storage_worker::submit_retained(move || {
                let _ = reply.send(crate::route::set_default_quality(quality));
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::Language { language, reply } => {
            let _ = crate::storage_worker::submit_retained(move || {
                let _ = reply.send(crate::plex::session::set_language(language));
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::DirectPlay { mode, reply } => {
            let _ = crate::storage_worker::submit_retained(move || {
                let _ = reply.send(crate::route::set_direct_play_mode(mode));
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::SubtitleSize { size, reply } => {
            let _ = crate::storage_worker::submit_retained(move || {
                let _ = reply.send(crate::route::set_subtitle_size(size));
                crate::ui::idle::invalidate();
            });
        }
        PreferenceCmd::SubtitlePosition { position, reply } => {
            let _ = crate::storage_worker::submit_retained(move || {
                let _ = reply.send(crate::route::set_subtitle_position(position));
                crate::ui::idle::invalidate();
            });
        }
    }
}
