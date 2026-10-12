//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
use plx_media::route::{choose, How};
use plx_plex::plex::account::{PreferenceError, PreferenceRequest};
use plx_plex::plex::session::{DeckPressPref, DirectPlayModePref, NextEpisodeModePref, QualityPref,
    SkipIntervalPref, SubtitlePositionPref, SubtitleSizePref};
use plx_screens::registry::{AccountPreferenceReply, PreferenceCmd};

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
        // Every live setting saves through the one write door; each row's own `SETTINGS` policy
        // (`How::settings`) decides whether the pick is confirmed first (`Confirm`: persisted on
        // the storage worker, published only once durable) or published at once and persisted
        // behind it (`Optimistic`, which runs HERE on the main thread so no outer worker
        // submission republishes).
        PreferenceCmd::Quality { quality, reply } => choose::<QualityPref>(quality, How::settings::<QualityPref>(reply)),
        PreferenceCmd::Language { language, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_language(language));
                plx_machine::idle::invalidate();
            });
        }
        PreferenceCmd::DirectPlay { mode, reply } => {
            choose::<DirectPlayModePref>(mode, How::settings::<DirectPlayModePref>(reply))
        }
        PreferenceCmd::NextEpisode { mode, reply } => {
            choose::<NextEpisodeModePref>(mode, How::settings::<NextEpisodeModePref>(reply))
        }
        PreferenceCmd::DeckPress { mode, reply } => choose::<DeckPressPref>(mode, How::settings::<DeckPressPref>(reply)),
        PreferenceCmd::SkipInterval { interval, reply } => {
            choose::<SkipIntervalPref>(interval, How::settings::<SkipIntervalPref>(reply))
        }
        PreferenceCmd::SubtitleSize { size, reply } => {
            choose::<SubtitleSizePref>(size, How::settings::<SubtitleSizePref>(reply))
        }
        PreferenceCmd::SubtitlePosition { position, reply } => {
            choose::<SubtitlePositionPref>(position, How::settings::<SubtitlePositionPref>(reply))
        }
    }
}
