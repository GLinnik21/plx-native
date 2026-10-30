//! Artwork and playback URLs, and transcode sessions.

use super::*;
use crate::jellyfin::playback_info;

impl JfClient {
    // ---- artwork and playback URLs ----

    /// An image path from the model (`Metadata::thumb`/`art`) sized for drawing
    /// (`Client::image_transcode_path`): the path (under the server's base path) the app's `Client`
    /// fetches from its own origin. Jellyfin scales on the server, like Plex's photo transcoder.
    pub fn image_path(&self, model_path: &str, w: i64, h: i64) -> String {
        let model_path = plex_image_path(model_path).unwrap_or_else(|| model_path.to_string());
        let model_path = model_path.as_str();
        let mut path = format!("{}{model_path}", base_path(&self.base));
        if w > 0 {
            paths::append_param(&mut path, "fillWidth", &w.to_string());
        }
        if h > 0 {
            paths::append_param(&mut path, "fillHeight", &h.to_string());
        }
        path
    }

    /// The direct-play URL for a part key from the model (`Client::direct_play_url`).
    pub fn direct_play_url(&self, part_key: &str, play_session_id: Option<&str>) -> String {
        let mut path = part_key.to_string();
        if let Some(s) = play_session_id {
            paths::append_param(&mut path, "playSessionId", &plx_plex::plex::urlenc_str(s));
        }
        format!("{}{}", self.base, self.with_token(&path))
    }

    /// `path` with this client's token in the query, the way the player and subtitle fetches carry it.
    pub(super) fn with_token(&self, path: &str) -> String {
        let mut path = path.to_string();
        paths::append_param(&mut path, "ApiKey", &plx_plex::plex::urlenc_str(self.token().unwrap_or("")));
        path
    }

    /// The URL of a transcode ([`paths::transcode`]) with this client's device and token. The play
    /// session is remembered as transcoding until [`Self::stop_transcode`].
    pub fn transcode_url(&self, t: &paths::Transcode<'_>) -> String {
        let t = paths::Transcode { device_id: &self.identity.device_id, ..t.clone() };
        self.transcoding
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(t.play_session_id.to_string());
        format!("{}{}", self.base, paths::transcode(&t, self.token().unwrap_or("")))
    }

    /// The server's verdict for playing `item_id` on a device described by `profile`
    /// ([`playback_info`]). `None` when the server could not be asked.
    pub fn playback_info(
        &self,
        item_id: &str,
        media_source_id: Option<&str>,
        audio_index: Option<i64>,
        subtitle_index: Option<i64>,
        profile: &serde_json::Value,
    ) -> Option<playback_info::Verdict> {
        let path = paths::playback_info(self.user_id(), item_id, media_source_id, audio_index, subtitle_index);
        let body = serde_json::json!({ "DeviceProfile": profile }).to_string();
        // The user's policy decides whether the server may encode at all: the user as read in the
        // last few minutes (Home's settings read it; a Dashboard edit drops it, through the
        // server's `UserUpdated`), else asked beside the verdict, since neither needs the other.
        // An unanswered ask reads as allowed, the server's default.
        let (a, policy) = both(
            || self.send(Verb::Post, &path, Some(&body)),
            || UserDto::deserialize(&*self.user_value(RECENT_CONFIG).ok()?).ok()?.policy,
        );
        let policy = policy.flatten().unwrap_or_default();
        let a = a?;
        if !a.ok() {
            return None;
        }
        let r: playback_info::Response = serde_json::from_slice(&a.body).ok()?;
        let encoders = playback_info::Encoders {
            video: policy.enable_video_playback_transcoding.unwrap_or(true),
            audio: policy.enable_audio_playback_transcoding.unwrap_or(true),
        };
        playback_info::verdict(&r, media_source_id, encoders)
    }

    /// The server's own transcode stream ([`playback_info::Verdict::url`]) for this play session:
    /// under the session id this client reports and stops it by, from `start_ticks`, with this
    /// client's token. Remembered as transcoding until [`Self::stop_transcode`].
    pub fn server_transcode_url(&self, url: &str, play_session_id: &str, start_ticks: i64) -> String {
        self.transcoding
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(play_session_id.to_string());
        let mut u = paths::with_param(url, "PlaySessionId", &plx_plex::plex::urlenc_str(play_session_id));
        if start_ticks > 0 {
            u = paths::with_param(&u, "StartTimeTicks", &start_ticks.to_string());
        }
        u = paths::without_param(&u, "api_key");
        u = paths::with_param(&u, "ApiKey", &plx_plex::plex::urlenc_str(self.token().unwrap_or("")));
        format!("{}{u}", self.base)
    }

    /// End the server's encoder for a play session.
    pub fn stop_transcode(&self, play_session_id: &str) -> bool {
        self.transcoding.lock().unwrap_or_else(|p| p.into_inner()).remove(play_session_id);
        self.send(Verb::Delete, &paths::stop_encoding(&self.identity.device_id, play_session_id), None)
            .is_some_and(|a| a.ok())
    }

    pub(super) fn is_transcoding(&self, play_session_id: &str) -> bool {
        self.transcoding.lock().unwrap_or_else(|p| p.into_inner()).contains(play_session_id)
    }
}
