//! Editing the active profile's Plex account preferences. These calls are blocking and belong
//! on a worker. Capture binds a request to one profile; snapshots also bind saves to the exact
//! preference revision the user saw. A single I/O gate serializes settings reads and writes,
//! while the playback cache's flight id fences out older background reads.
use super::{AccountAudioProfile, AccountClient, AccountUser, AudioPreferences, AudioPreferencesCache,
    AudioPreferencesFlight, AudioPreferencesKey, AudioPreferencesOutcome, FetchPath};
use std::sync::Mutex;
use std::time::Instant;

static PREFERENCE_IO: Mutex<()> = Mutex::new(());
const CLIENTS_PLEX_TV: &str = "https://clients.plex.tv";

#[derive(Clone)]
pub struct PreferenceRequest {
    stamp: String,
    client_id: String,
    credential: String,
    user: crate::plex::session::UserRef,
    key: AudioPreferencesKey,
}

#[derive(Clone, Debug)]
pub struct PreferenceSnapshot {
    pub preferences: AudioPreferences,
    key: AudioPreferencesKey,
    revision: u64,
}

/// `None` leaves a setting untouched; `Some("")` clears a language preference.
#[derive(Clone, Debug, Default)]
pub struct PreferenceUpdate {
    pub auto_select_audio: Option<bool>,
    pub audio_language: Option<String>,
    pub subtitle_language: Option<String>,
    pub subtitle_mode: Option<i64>,
    pub subtitle_forced: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceError {
    Stale,
    Refused,
    TimedOut,
    Unavailable,
    InvalidResponse,
    InvalidPreference,
}

impl PreferenceError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Stale => plx_platform::i18n::msg::browse_preferences_stale(),
            Self::Refused => plx_platform::i18n::msg::browse_preferences_refused(),
            Self::TimedOut => plx_platform::i18n::msg::browse_preferences_timed_out(),
            Self::Unavailable => plx_platform::i18n::msg::browse_preferences_unavailable(),
            Self::InvalidResponse => plx_platform::i18n::msg::browse_preferences_invalid_response(),
            Self::InvalidPreference => plx_platform::i18n::msg::browse_preferences_invalid_preference(),
        }
    }
}

impl PreferenceRequest {
    /// Captures the active identity and session. No account request runs here (`session::peek` is a
    /// pure read of the session cache and schedules nothing); call only after live IO has been admitted.
    pub fn capture() -> Option<Self> {
        let current = crate::plex::session::current_snapshot();
        let user = current.user.clone()?;
        let credential = crate::plex::session::plex_tv_credential(&user)?;
        let client_id = crate::plex::session::peek().client_id.clone();
        if client_id.is_empty() { return None; }
        Some(Self { stamp: current.stamp.clone(), key: AudioPreferencesKey::new(&user, current.generation),
            client_id, credential, user })
    }

    /// Marker reads may prove the implicit owner through the captured account's Home roster.
    /// The request key still names the original seat, so no preference is moved to another profile.
    pub fn capture_marker_access() -> Option<Self> {
        Self::capture()?.bind_marker_owner(&crate::plex::session::peek())
    }
    fn bind_marker_owner(mut self, stored: &crate::plex::session::Session) -> Option<Self> {
        if self.user.id != 0 || !self.user.uuid.is_empty() { return Some(self); }
        if stored.user.id != 0 || !stored.user.uuid.is_empty() || !stored.active_profile_is_admin()
            || stored.account_token.is_empty() || self.credential != stored.account_token
            || self.stamp != crate::plex::grant::account_key(&stored.account_token) {
            return None;
        }
        let mut admins = stored.home_users.iter().filter(|user| user.admin);
        let admin = admins.next()?;
        if admins.next().is_some() || (admin.id == 0 && admin.uuid.is_empty()) { return None; }
        self.user.id = admin.id;
        self.user.uuid = admin.uuid.clone();
        Some(self)
    }
    pub fn is_current(&self) -> bool { self.key.is_current() }
    pub fn matches_profile(&self, profile: &crate::plex::session::CurrentProfile) -> bool {
        profile.stamp == self.stamp && profile.generation == self.key.generation && profile.user.as_ref().is_some_and(|u|
            u.id == self.key.id && u.uuid == self.key.uuid)
    }

    /// A fully synthetic receipt for composed UI tests; neither credentials nor preferences are
    /// read from disk or fetched from Plex. The caller publishes this identity under testlock.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture_for_test(user: crate::plex::session::UserRef, generation: u32,
        preferences: AudioPreferences) -> (Self, PreferenceSnapshot)
    {
        plx_base::testlock::assert_held("account preference receipt fixture");
        let key = AudioPreferencesKey::new(&user, generation);
        (Self { stamp: String::new(), client_id: "preference-fixture-client".into(), credential: "synthetic-token".into(),
            user, key: key.clone() }, PreferenceSnapshot { preferences, key, revision: 0 })
    }

    pub fn load(&self) -> Result<PreferenceSnapshot, PreferenceError> {
        self.load_with(&super::AUDIO_PREFERENCES_CACHE, &PREFERENCE_IO,
            || self.is_current(), |method, path, query| self.request(method, path, query))
    }

    pub fn save(&self, snapshot: &PreferenceSnapshot, update: PreferenceUpdate)
        -> Result<PreferenceSnapshot, PreferenceError>
    {
        self.save_with(&super::AUDIO_PREFERENCES_CACHE, &PREFERENCE_IO, snapshot, update,
            || self.is_current(), |method, path, query| self.request(method, path, query))
    }

    fn request(&self, method: &str, path: &str, query: &str)
        -> Result<plx_net::net::Resp, PreferenceError>
    {
        let base = super::plex_tv();
        let base = if path.ends_with("/profile") && base == super::PLEX_TV {
            CLIENTS_PLEX_TV
        } else { base };
        let url = if query.is_empty() { format!("{base}{path}") }
            else { format!("{base}{path}?{query}") };
        let client = AccountClient::new(&self.client_id, Some(&self.credential));
        let response = plx_net::net::request_evidence(&url, &client.headers(), method,
            (method == "PUT").then_some(b"".as_slice()), plx_net::net::API, false, None, None);
        super::note_response_contact(&url, &response);
        response.map_err(preference_failure).and_then(accepted)
    }

    fn load_with<F, C>(&self, cache: &AudioPreferencesCache, io: &Mutex<()>, current: C,
        mut request: F) -> Result<PreferenceSnapshot, PreferenceError>
    where F: FnMut(&str, &str, &str) -> Result<plx_net::net::Resp, PreferenceError>,
        C: Fn() -> bool
    {
        let _io = io.lock().unwrap_or_else(|e| e.into_inner());
        if !current() { return Err(PreferenceError::Stale); }
        let flight = cache.begin_settings(self.key.clone(), None)?;
        let result = (|| {
            // /profile itself has no identity fields. Prove the token's owner before accepting
            // its preferences, using the same id/uuid rules as playback's /user request.
            let user = accepted(request("GET", "/api/v2/user", "")?)?;
            let dto: AccountUser = serde_json::from_slice(&user.body)
                .map_err(|_| PreferenceError::InvalidResponse)?;
            dto.audio_preferences_for(&self.user).ok_or(PreferenceError::Refused)?;
            if !current() { return Err(PreferenceError::Stale); }
            let response = accepted(request("GET", "/api/v2/user/profile", "")?)?;
            let profile: AccountAudioProfile = serde_json::from_slice(&response.body)
                .map_err(|_| PreferenceError::InvalidResponse)?;
            Ok(profile.preferences())
        })();
        finish(cache, flight, result, current)
    }

    #[allow(clippy::too_many_arguments)]
    fn save_with<F, C>(&self, cache: &AudioPreferencesCache, io: &Mutex<()>,
        snapshot: &PreferenceSnapshot, update: PreferenceUpdate, current: C,
        mut request: F) -> Result<PreferenceSnapshot, PreferenceError>
    where F: FnMut(&str, &str, &str) -> Result<plx_net::net::Resp, PreferenceError>,
        C: Fn() -> bool
    {
        let _io = io.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.key != self.key || !current() { return Err(PreferenceError::Stale); }
        let (query, preferences) = update.apply(&snapshot.preferences)?;
        let flight = cache.begin_settings(self.key.clone(), Some(snapshot.revision))?;
        let result = if query.is_empty() { Ok(preferences) } else {
            request("PUT", "/api/v2/user/profile", &query).and_then(accepted)
                .map(|_| preferences)
        };
        finish(cache, flight, result, current)
    }
}

fn preference_failure(failure: plx_net::net::RequestFailure) -> PreferenceError {
    // A validated final refusal remains authoritative even if its body was truncated or reset.
    if matches!(failure.status, Some(401 | 403)) {
        PreferenceError::Refused
    } else if failure.cause == plx_net::net::RequestError::TimedOut {
        PreferenceError::TimedOut
    } else { PreferenceError::Unavailable }
}

fn accepted(response: plx_net::net::Resp) -> Result<plx_net::net::Resp, PreferenceError> {
    match response.status {
        200..=299 => Ok(response),
        401 | 403 => Err(PreferenceError::Refused),
        _ => Err(PreferenceError::Unavailable),
    }
}

fn finish<C: Fn() -> bool>(cache: &AudioPreferencesCache, flight: AudioPreferencesFlight,
    result: Result<AudioPreferences, PreferenceError>, current: C)
    -> Result<PreferenceSnapshot, PreferenceError>
{
    match result {
        Ok(preferences) => {
            let key = flight.key.clone();
            if !cache.complete(flight, AudioPreferencesOutcome::Available(preferences.clone()),
                FetchPath::Warm, Instant::now(), &current) {
                return Err(PreferenceError::Stale);
            }
            let state = cache.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.key.as_ref() != Some(&key) || !current() {
                return Err(PreferenceError::Stale);
            }
            Ok(PreferenceSnapshot { preferences, key, revision: state.revision })
        }
        Err(error) => { cache.cancel(&flight); Err(error) }
    }
}

impl AudioPreferencesCache {
    fn begin_settings(&self, key: AudioPreferencesKey, expected_revision: Option<u64>)
        -> Result<AudioPreferencesFlight, PreferenceError>
    {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(revision) = expected_revision {
            if state.key.as_ref() != Some(&key) || state.revision != revision {
                return Err(PreferenceError::Stale);
            }
        } else if state.key.as_ref() != Some(&key) {
            state.key = Some(key.clone()); state.entry = None;
        }
        // Replace any pre-save warm/play fetch's id. Its eventual response cannot overwrite
        // these settings; new playback reads join this flight within their normal budget.
        state.next_flight = state.next_flight.wrapping_add(1);
        let id = state.next_flight;
        state.flight = Some(id);
        self.changed.notify_all();
        Ok(AudioPreferencesFlight { key, id })
    }
}

impl PreferenceUpdate {
    fn apply(self, previous: &AudioPreferences) -> Result<(String, AudioPreferences), PreferenceError> {
        if self.subtitle_mode.is_some_and(|v| !(0..=2).contains(&v))
            || self.subtitle_forced.is_some_and(|v| !(0..=3).contains(&v)) {
            return Err(PreferenceError::InvalidPreference);
        }
        let mut next = previous.clone();
        let mut values: Vec<(&str, String)> = Vec::new();
        // Every audio-language choice (including Original) enables Plex's selection feature.
        let auto_select = self.audio_language.as_ref().map(|_| true).or(self.auto_select_audio);
        if let Some(value) = auto_select {
            if previous.auto_select_audio != Some(value) {
                values.push(("autoSelectAudio", value.to_string()));
            }
            next.auto_select_audio = Some(value);
        }
        for (name, requested, old, target) in [
            ("defaultAudioLanguage", self.audio_language, &previous.stated_language, &mut next.stated_language),
            ("defaultSubtitleLanguage", self.subtitle_language, &previous.subtitle_language, &mut next.subtitle_language),
        ] {
            if let Some(value) = requested {
                if old.as_deref().unwrap_or("") != value { values.push((name, value.clone())); }
                *target = (!value.is_empty()).then_some(value);
            }
        }
        for (name, requested, old, target) in [
            ("autoSelectSubtitle", self.subtitle_mode, previous.subtitle_mode, &mut next.subtitle_mode),
            ("defaultSubtitleForced", self.subtitle_forced, previous.subtitle_forced, &mut next.subtitle_forced),
        ] {
            if let Some(value) = requested {
                if old != value { values.push((name, value.to_string())); }
                *target = value;
            }
        }
        next.language = (next.auto_select_audio == Some(true))
            .then(|| next.stated_language.clone()).flatten();
        let query = values.into_iter().map(|(key, value)|
            format!("{key}={}", crate::plex::urlenc_str(&value)))
            .collect::<Vec<_>>().join("&");
        Ok((query, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PreferenceRequest {
        let user = crate::plex::session::UserRef { id: 7, uuid: "profile-7".into(),
            token: "pms-only-token".into(), plex_tv_token: Some("account-only-token".into()),
            ..Default::default() };
        PreferenceRequest { stamp: String::new(), client_id: "fixture-client".into(), credential: "account-only-token".into(),
            key: AudioPreferencesKey::new(&user, 4), user }
    }
    fn response(body: &str) -> plx_net::net::Resp {
        plx_net::net::Resp { status: 200, body: body.as_bytes().to_vec(), peer_pin: None, etag: None }
    }
    fn fixture(method: &str, path: &str, query: &str) -> Result<plx_net::net::Resp, PreferenceError> {
        assert_eq!(method, "GET"); assert!(query.is_empty());
        Ok(match path {
            "/api/v2/user" => response(r#"{"id":7,"uuid":"profile-7"}"#),
            "/api/v2/user/profile" => response(r#"{"autoSelectAudio":true,
                "defaultAudioLanguage":"fr-CA","defaultSubtitleLanguage":"en-GB",
                "autoSelectSubtitle":1,"defaultSubtitleForced":2}"#),
            _ => panic!("unexpected endpoint"),
        })
    }
    fn snapshot(cache: &AudioPreferencesCache) -> PreferenceSnapshot {
        request().load_with(cache, &Mutex::new(()), || true, fixture).unwrap()
    }

    #[test]
    fn account_regression_incomplete_refusal_keeps_the_account_error() {
        for status in [401, 403] {
            for cause in [plx_net::net::RequestError::TimedOut, plx_net::net::RequestError::Transport] {
                let failure = plx_net::net::RequestFailure { status: Some(status), cause,
                    body_limit: None, curl_rc: Some(92), verify: None };
                assert_eq!(preference_failure(failure), PreferenceError::Refused);
            }
        }
    }

    #[test]
    fn load_proves_account_identity_and_publishes_the_profile() {
        let cache = AudioPreferencesCache::new();
        let result = snapshot(&cache);
        assert_eq!(result.preferences.language.as_deref(), Some("fr-CA"));
        assert_eq!(result.preferences.subtitle_language.as_deref(), Some("en-GB"));
        assert_eq!(result.preferences.subtitle_mode, 1);
        assert_eq!(result.preferences.subtitle_forced, 2);
        let cached = super::super::audio_preferences_cached_at(&cache, request().key,
            std::time::Duration::from_millis(10), Instant::now, |_| panic!("load was not cached"), || true);
        assert_eq!(cached, AudioPreferencesOutcome::Available(result.preferences));
    }

    #[test]
    fn changed_keys_only_and_original_clears_language_with_auto_selection_enabled() {
        let previous = AudioPreferences { auto_select_audio: Some(false),
            stated_language: Some("fr".into()), subtitle_language: Some("en-GB".into()),
            ..Default::default() };
        let (query, next) = PreferenceUpdate { audio_language: Some(String::new()),
            subtitle_language: Some("en-GB".into()), ..Default::default() }.apply(&previous).unwrap();
        assert_eq!(query, "autoSelectAudio=true&defaultAudioLanguage=");
        assert_eq!(next.auto_select_audio, Some(true));
        assert_eq!(next.stated_language, None); assert_eq!(next.language, None);
        assert_eq!(next.subtitle_language, previous.subtitle_language);
    }

    #[test]
    fn regional_and_unfamiliar_languages_round_trip_without_normalizing() {
        let previous = AudioPreferences::default();
        let (query, next) = PreferenceUpdate { audio_language: Some("pt-BR".into()),
            subtitle_language: Some("es-419".into()), subtitle_mode: Some(2),
            subtitle_forced: Some(3), ..Default::default() }.apply(&previous).unwrap();
        assert_eq!(query, "autoSelectAudio=true&defaultAudioLanguage=pt-BR&defaultSubtitleLanguage=es-419&autoSelectSubtitle=2&defaultSubtitleForced=3");
        assert_eq!(next.language.as_deref(), Some("pt-BR"));
        assert_eq!(next.subtitle_language.as_deref(), Some("es-419"));
        assert!(PreferenceUpdate { subtitle_mode: Some(4), ..Default::default() }.apply(&next).is_err());
        assert!(PreferenceUpdate { subtitle_forced: Some(-1), ..Default::default() }.apply(&next).is_err());
    }

    #[test]
    fn identity_mismatch_never_reads_or_writes_another_profiles_preferences() {
        let cache = AudioPreferencesCache::new();
        let result = request().load_with(&cache, &Mutex::new(()), || true, |method, path, _| {
            assert_eq!(method, "GET"); assert_eq!(path, "/api/v2/user");
            Ok(response(r#"{"id":99,"uuid":"another-profile"}"#))
        });
        assert_eq!(result.unwrap_err(), PreferenceError::Refused);
        assert!(cache.state.lock().unwrap().flight.is_none());
    }

    #[test]
    fn failed_save_keeps_confirmed_preferences_and_can_be_retried() {
        let cache = AudioPreferencesCache::new(); let prior = snapshot(&cache);
        let patch = || PreferenceUpdate { subtitle_mode: Some(2), ..Default::default() };
        let result = request().save_with(&cache, &Mutex::new(()), &prior, patch(), || true,
            |_, _, _| Err(PreferenceError::Unavailable));
        assert_eq!(result.unwrap_err(), PreferenceError::Unavailable);
        assert!(cache.state.lock().unwrap().flight.is_none());
        let result = request().save_with(&cache, &Mutex::new(()), &prior, patch(), || true,
            |method, path, query| {
                assert_eq!((method, path, query), ("PUT", "/api/v2/user/profile", "autoSelectSubtitle=2"));
                Ok(plx_net::net::Resp { status: 204, body: Vec::new(), peer_pin: None, etag: None })
            }).unwrap();
        assert_eq!(result.preferences.subtitle_mode, 2);
    }

    #[test]
    fn confirmed_save_retires_older_fetches_and_snapshots() {
        let cache = AudioPreferencesCache::new(); let prior = snapshot(&cache);
        // A background fetch started before the edit must not put its old response over it.
        let old_read = cache.begin_settings(request().key, None).unwrap();
        let saved = request().save_with(&cache, &Mutex::new(()), &prior,
            PreferenceUpdate { subtitle_mode: Some(2), ..Default::default() }, || true,
            |_, _, _| Ok(response("{}"))).unwrap();
        assert!(!cache.complete(old_read, AudioPreferencesOutcome::Available(prior.preferences.clone()),
            FetchPath::Warm, Instant::now(), || true));
        let stale = request().save_with(&cache, &Mutex::new(()), &prior,
            PreferenceUpdate { subtitle_mode: Some(0), ..Default::default() }, || true,
            |_, _, _| panic!("stale snapshot reached the server"));
        assert_eq!(stale.unwrap_err(), PreferenceError::Stale);
        assert_eq!(saved.preferences.subtitle_mode, 2);
    }

    #[test]
    fn profile_switch_retires_a_save_response() {
        let cache = AudioPreferencesCache::new(); let prior = snapshot(&cache);
        let still_current = std::cell::Cell::new(true);
        let result = request().save_with(&cache, &Mutex::new(()), &prior,
            PreferenceUpdate { subtitle_mode: Some(2), ..Default::default() },
            || still_current.get(), |_, _, _| {
                cache.publish(AudioPreferencesKey { id: 99, uuid: "other".into(), generation: 5 });
                still_current.set(false); Ok(response("{}"))
            });
        assert_eq!(result.unwrap_err(), PreferenceError::Stale);
        let state = cache.state.lock().unwrap();
        assert_eq!(state.key.as_ref().unwrap().id, 99); assert!(state.entry.is_none());
    }

    #[test]
    fn concurrent_saves_serialize_and_reject_the_older_snapshot() {
        let cache = std::sync::Arc::new(AudioPreferencesCache::new());
        let io = std::sync::Arc::new(Mutex::new(())); let prior = snapshot(&cache);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_cache = cache.clone(); let first_io = io.clone(); let first_prior = prior.clone();
        let first = std::thread::spawn(move || request().save_with(&first_cache, &first_io, &first_prior,
            PreferenceUpdate { subtitle_mode: Some(2), ..Default::default() }, || true, |_, _, _| {
                entered_tx.send(()).unwrap(); release_rx.recv().unwrap(); Ok(response("{}"))
            }));
        entered_rx.recv().unwrap();
        let second = std::thread::spawn(move || request().save_with(&cache, &io, &prior,
            PreferenceUpdate { subtitle_mode: Some(0), ..Default::default() }, || true,
            |_, _, _| panic!("concurrent stale save reached the server")));
        release_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().preferences.subtitle_mode, 2);
        assert_eq!(second.join().unwrap().unwrap_err(), PreferenceError::Stale);
    }
}

/// Viewer capabilities from Plex Web's authenticated `/api/v2/features` collection. A server's
/// subscription is deliberately absent: a shared server owner cannot grant viewer features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkerAccess {
    pub intro: bool,
    pub credits: bool,
}
#[derive(serde::Deserialize)]
struct ViewerFeature { id: String }

impl PreferenceRequest {
    /// Worker-side read under the same captured profile credential as account preferences.
    pub fn load_marker_access(&self) -> Result<MarkerAccess, PreferenceError> {
        self.load_marker_access_with(|| self.matches_profile(&crate::plex::session::current_snapshot()),
            |path| self.request("GET", path, "").map(|r| r.body))
    }
    fn load_marker_access_with(&self, current: impl Fn() -> bool,
        mut read: impl FnMut(&str) -> Result<Vec<u8>, PreferenceError>) -> Result<MarkerAccess, PreferenceError> {
        if !current() { return Err(PreferenceError::Stale); }
        let user: AccountUser = serde_json::from_slice(&read("/api/v2/user")?)
            .map_err(|_| PreferenceError::InvalidResponse)?;
        if (self.user.id == 0 && self.user.uuid.is_empty()) || !user.matches_identity(&self.user) {
            return Err(PreferenceError::Refused);
        }
        if !current() { return Err(PreferenceError::Stale); }
        let features: Vec<ViewerFeature> = serde_json::from_slice(&read("/api/v2/features")?)
            .map_err(|_| PreferenceError::InvalidResponse)?;
        if !current() { return Err(PreferenceError::Stale); }
        Ok(MarkerAccess { intro: features.iter().any(|f| f.id == "intro-markers"),
            credits: features.iter().any(|f| f.id == "credits-markers") })
    }
}

#[cfg(test)]
mod marker_access_tests {
    use super::*;
    #[test]
    fn implicit_owner_uses_only_its_proven_admin_identity_and_preserves_the_seat() {
        use crate::plex::session::{CurrentProfile, HomeUserRef, Session, UserRef};
        let _guard = plx_base::testlock::serial();
        let (mut raw, _) = PreferenceRequest::fixture_for_test(UserRef::default(), 1, Default::default());
        let mut stored = Session { account_token: raw.credential.clone(), home_users: vec![HomeUserRef {
            id: 1, uuid: "demo-user".into(), admin: true, ..Default::default() }], ..Default::default() };
        raw.stamp = crate::plex::grant::account_key(&stored.account_token);
        let seat = CurrentProfile { user: Some(UserRef::default()), generation: 1, stamp: raw.stamp.clone() };
        assert_eq!(raw.load_marker_access_with(|| true, |_| Ok(br#"{"id":1,"uuid":"demo-user"}"#.to_vec())),
            Err(PreferenceError::Refused), "the raw unknown identity cannot authorize features");
        let proven = raw.clone().bind_marker_owner(&stored).expect("the implicit owner is proved");
        assert!(proven.matches_profile(&seat), "the account proof does not change the seat key");
        assert_eq!(proven.load_marker_access_with(|| true, |path| Ok(if path.ends_with("features") {
            br#"[{"id":"intro-markers"}]"#.to_vec()
        } else { br#"{"id":1,"uuid":"demo-user"}"#.to_vec() })), Ok(MarkerAccess { intro: true, credits: false }));
        for identity in [r#"{"id":2,"uuid":"demo-user"}"#, r#"{"id":1,"uuid":"other"}"#] {
            assert_eq!(proven.load_marker_access_with(|| true, |path| {
                assert_eq!(path, "/api/v2/user"); Ok(identity.as_bytes().to_vec())
            }), Err(PreferenceError::Refused));
        }
        let changed = CurrentProfile { user: seat.user.clone(), generation: seat.generation, stamp: format!("{}x", seat.stamp) };
        assert!(!proven.matches_profile(&changed));
        let mut bad = raw.clone(); bad.stamp.push('x'); assert!(bad.bind_marker_owner(&stored).is_none());
        let mut bad = raw.clone(); bad.credential.push('x'); assert!(bad.bind_marker_owner(&stored).is_none());
        stored.home_users.push(stored.home_users[0].clone());
        assert!(raw.clone().bind_marker_owner(&stored).is_none());
        stored.home_users.clear(); assert!(raw.clone().bind_marker_owner(&stored).is_none());
        stored.user.uuid = "managed".into(); assert!(raw.bind_marker_owner(&stored).is_none());
    }
    #[test]
    fn viewer_features_are_separate_identity_checked_and_fail_closed() {
        let _guard = plx_base::testlock::serial();
        let user = crate::plex::session::UserRef { id: 7, uuid: "viewer".into(), ..Default::default() };
        let (request, _) = PreferenceRequest::fixture_for_test(user, 1, Default::default());
        for (features, expected) in [("[]", MarkerAccess::default()),
            (r#"[{"id":"intro-markers"}]"#, MarkerAccess { intro: true, credits: false }),
            (r#"[{"id":"credits-markers"}]"#, MarkerAccess { intro: false, credits: true })] {
            let result = request.load_marker_access_with(|| true, |path| Ok(if path.ends_with("features") {
                features.as_bytes().to_vec()
            } else { br#"{"id":7,"uuid":"viewer"}"#.to_vec() }));
            assert_eq!(result, Ok(expected));
        }
        for identity in [r#"{"id":8,"uuid":"viewer"}"#, r#"{"id":7,"uuid":"other"}"#, "{}"] {
            let result = request.load_marker_access_with(|| true, |path| {
                assert_eq!(path, "/api/v2/user", "refused identity never asks for features");
                Ok(identity.as_bytes().to_vec())
            });
            assert_eq!(result, Err(PreferenceError::Refused));
        }
        for malformed in ["{}", "null", r#"[{"uuid":"feature-without-id"}]"#] {
            assert_eq!(request.load_marker_access_with(|| true, |path| Ok(if path.ends_with("features") {
                malformed.as_bytes().to_vec()
            } else { br#"{"id":7}"#.to_vec() })), Err(PreferenceError::InvalidResponse));
        }
        assert_eq!(request.load_marker_access_with(|| false, |_| panic!("stale read")), Err(PreferenceError::Stale));
    }
}
