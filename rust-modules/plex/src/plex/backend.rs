//! An alternative media-server backend behind the `Client` surface (FinNative).
//!
//! Compiled only with the `alt-backend` feature. Without it there is no `backend` module and no hook
//! in any `Client` method, so the code is what upstream ships.
//!
//! The screens, stores and player call `Client` methods and read the model types in `models.rs`.
//! A [`Backend`] answers those same calls from another kind of server, in the same model. When one
//! is installed, each hooked `Client` method hands its call to it and returns the answer, and the
//! method's Plex body does not run. Every method has a default that answers "nothing here"
//! (`None`, `false`, an empty path), which is how a Plex-only feature (Plex Discover, agent
//! subtitle search) disappears for a server that has no such thing rather than being imitated.
//! The plex.tv account's audio and subtitle preferences are hooked too (`account.rs`), so a server
//! that keeps its own per-user preferences can stand in for them.
//!
//! The hook is the [`alt!`] macro at the top of each method body.

use crate::plex::account::{AudioPreferences, PreferenceError};
use crate::plex::notify::{Notification, NotifyEnd};
use crate::plex::{
    MediaContainer, Metadata, PageReq, SearchKind, SectionQuery, StreamUrl, TimelineReport, TranscodeSpec, Trickplay,
};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;

/// What a non-Plex server answers. Signatures follow the `Client` methods of the same name.
#[allow(unused_variables)]
pub trait Backend: Send + Sync {
    // ---- the server ----
    fn friendly_name(&self) -> Option<String> {
        None
    }
    fn machine_identity(&self) -> Option<String> {
        None
    }

    // ---- libraries and shelves ----
    fn sections(&self) -> Option<MediaContainer> {
        None
    }
    fn home_hubs(&self, count: i64) -> Option<MediaContainer> {
        None
    }
    fn home_hubs_page(&self, count: i64, req: PageReq) -> Option<MediaContainer> {
        None
    }
    fn library_hubs(&self, section_key: i64, count: i64) -> Option<MediaContainer> {
        None
    }
    fn library_hubs_page(&self, section_key: i64, count: i64, req: PageReq) -> Option<MediaContainer> {
        None
    }
    fn hub_items_paged(&self, key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        None
    }
    /// The dedicated Continue Watching hub: one `Hub` holding the deck's first `count` cards.
    fn continue_watching(&self, count: i64) -> Option<MediaContainer> {
        None
    }
    /// The deck's listing by offset: the cards themselves in `metadata`, not wrapped in a hub.
    fn continue_watching_page(&self, start: i64, size: i64) -> Option<MediaContainer> {
        None
    }
    fn remove_from_continue_watching(&self, rating_key: &str) -> bool {
        false
    }
    fn section_items_query(&self, q: &SectionQuery<'_>) -> Option<MediaContainer> {
        None
    }
    /// A section's secondary directory (`genre`, `firstCharacter`, ...) under `filters`, as
    /// `Client::section_directory` answers it.
    fn section_directory(&self, section_key: i64, directory: &str, filters: &[(String, String)]) -> Option<MediaContainer> {
        None
    }
    fn search(&self, query: &str, limit: i64, section_id: i64) -> Option<MediaContainer> {
        None
    }
    fn search_typed(&self, query: &str, kind: SearchKind, req: PageReq) -> Option<MediaContainer> {
        None
    }

    // ---- items ----
    /// An item id from outside the app (a launch parameter, `app/launch.rs`) in the form this
    /// server's rating keys take, or `None` when it cannot be one of this server's items.
    fn item_key(&self, raw: &str) -> Option<String> {
        Some(raw.to_string())
    }
    fn metadata(&self, rating_key: &str) -> Option<Metadata> {
        None
    }
    fn metadata_many(&self, rating_keys: &[&str]) -> Option<MediaContainer> {
        None
    }
    fn children_paged(&self, rating_key: &str, start: i64, size: i64) -> Option<MediaContainer> {
        None
    }
    fn extras(&self, rating_key: &str) -> Option<MediaContainer> {
        None
    }
    fn related(&self, rating_key: &str) -> Option<MediaContainer> {
        None
    }
    fn trickplay(&self, rating_key: &str, part_id: i64) -> Option<Trickplay> {
        None
    }
    fn trickplay_sheet(&self, rating_key: &str, part_id: i64, info: &Trickplay, sheet: u32) -> Option<Vec<u8>> {
        None
    }
    fn person_media(&self, person_id: &str) -> Option<MediaContainer> {
        None
    }
    /// A person's biography record, in place of plex.tv's (`AccountClient::person_profile`, same
    /// answers: `None` is a failure to retry, an empty profile a person the server has nothing on).
    fn person_profile(&self, person_id: &str) -> Option<crate::plex::discover::PersonProfile> {
        None
    }
    /// A person's filmography beyond the library, in place of plex.tv's
    /// (`AccountClient::person_credits`, same answers).
    fn person_credits(&self, person_id: &str) -> Option<Vec<crate::plex::discover::CreditGroup>> {
        None
    }
    fn scrobble(&self, rating_key: &str) -> bool {
        false
    }
    fn unscrobble(&self, rating_key: &str) -> bool {
        false
    }
    /// Subtitle candidates for an item in one language (`Client::search_subtitles`; `language` is
    /// the two-letter code that call already checked), as subtitle `Stream` rows whose `key` is the
    /// handle `add_subtitle` takes. `None` when the search could not be made.
    fn search_subtitles(&self, rating_key: &str, language: &str) -> Option<MediaContainer> {
        None
    }
    /// Download one candidate into the item (`Client::add_subtitle`).
    fn add_subtitle(&self, rating_key: &str, candidate_key: &str) -> bool {
        false
    }

    // ---- artwork, subtitles, playback ----
    /// A fetchable path (relative to the client's origin) for a model image path sized `w`×`h`.
    fn image_transcode_path(&self, src_path: &str, w: i64, h: i64, png: bool) -> String {
        String::new()
    }
    /// Whether the picture at the fetchable `path` is shown to every profile (a who's-watching
    /// tile), so a profile switch's credential revocation must not take it off the screen.
    fn art_outlives_grant(&self, path: &str) -> bool {
        false
    }
    fn sidecar_subtitle_detailed(&self, key: &str, codec: &str) -> Result<Vec<u8>, String> {
        Err("not supported by this server".into())
    }
    /// The server's direct-play verdict for the version at `media_index` of an item
    /// (`Client::mde_decision`): a container whose `selected` version's first part carries
    /// `decision` (`directplay` when the file may be played as it is).
    fn mde_decision(&self, rating_key: &str, media_index: usize, session: &str, audio_stream_id: i64, subtitle_stream_id: i64,
        forced: bool) -> Option<MediaContainer> {
        None
    }
    fn direct_play_url(&self, part_key: &str, session: &str) -> Option<StreamUrl> {
        None
    }
    /// The play queue for one item, as the `MediaContainer` a Plex server answers `POST
    /// /playQueues` with (`playQueueID`, the selected item, the rows with their
    /// `playQueueItemID`s); `Client::create_play_queue` maps it the way it maps Plex's.
    fn play_queue(&self, rating_key: &str, continuous: bool) -> Option<MediaContainer> {
        None
    }
    fn timeline(&self, r: &TimelineReport<'_>) -> bool {
        false
    }
    /// The server's answer to a transcode request (`Client::transcode_decision`): a container
    /// whose first part's streams carry the codecs the stream will ARRIVE in, or a refusal
    /// (`generalDecisionCode` 2000).
    fn transcode_decision(&self, spec: &TranscodeSpec<'_>) -> Option<MediaContainer> {
        None
    }
    fn transcode_start_url(&self, spec: &TranscodeSpec<'_>) -> Option<StreamUrl> {
        None
    }
    /// Stop the encoder for `session` (`Client::transcode_stop` and `transcode_stop_physical`).
    fn transcode_stop(&self, session: &str) -> bool {
        false
    }
    fn transcode_session_present(&self, session: &str) -> Option<bool> {
        None
    }
    fn transcode_resource_reconciled(&self, session: &str) -> Option<bool> {
        None
    }

    // ---- favourites ([`crate::plex::favorite_mark`]) ----
    /// `rating_key`'s favourite mark, as last read or pressed; `None` for a server that keeps none.
    fn favorite_mark(&self, rating_key: &str) -> Option<bool> {
        None
    }
    /// Make `rating_key` a favourite, or not: the mark flips at once, the server is told off the
    /// frame thread, and a refusal flips it back.
    fn set_favorite_mark(&self, rating_key: &str, favourite: bool) {}
    /// Has a favourite write landed since the last call? FRAME THREAD.
    fn take_favorite_landed(&self) -> bool {
        false
    }

    // ---- who is signed in ----
    /// The signed-in user's name and avatar (an image path this backend resolves), for the
    /// account chip when there is no Plex Home profile to show. No request: answered from what
    /// sign-in already learned.
    fn signed_in_user(&self) -> Option<(String, String)> {
        None
    }
    /// Moves whenever [`Self::signed_in_user`]'s answer may have changed (a sign-in, a sign-out,
    /// a saved sign-in resumed), so a reader on the frame thread can ask again only then.
    fn signed_in_generation(&self) -> u32 {
        0
    }

    // ---- live updates ----
    /// `Client::notifications`: run one feed of what other clients are playing until it ends. A
    /// server with no such feed answers `Unsupported`, which parks it for the session.
    fn notifications(&self, off: &plx_base::task::OffFrame, cancel: &AtomicBool, on: &mut dyn FnMut(Notification))
        -> NotifyEnd {
        NotifyEnd::Unsupported
    }

    // ---- the signed-in user's audio and subtitle preferences ----
    /// What playback picks tracks by (`AccountClient::audio_preferences`) and Settings' Audio &
    /// Subtitles page shows (`PreferenceRequest::load`), in place of the plex.tv profile's.
    fn audio_preferences(&self) -> Result<AudioPreferences, PreferenceError> {
        Err(PreferenceError::Unavailable)
    }
    /// Save one Settings change (`PreferenceRequest::save`): `next` is `previous`, the preferences
    /// the page showed, with that change applied. Answers the preferences now in force.
    fn save_audio_preferences(&self, previous: &AudioPreferences, next: &AudioPreferences)
        -> Result<AudioPreferences, PreferenceError> {
        Err(PreferenceError::Unavailable)
    }
}

static BACKEND: OnceLock<Box<dyn Backend>> = OnceLock::new();

/// Install the backend for this run. The first install wins; FinNative installs one at boot, before
/// any `Client` method can be called.
pub fn install(b: Box<dyn Backend>) {
    let _ = BACKEND.set(b);
}

/// The installed backend, if any.
pub fn active() -> Option<&'static dyn Backend> {
    BACKEND.get().map(|b| b.as_ref())
}
