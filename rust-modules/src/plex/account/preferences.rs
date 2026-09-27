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
pub(crate) struct PreferenceRequest {
    client_id: String,
    credential: String,
    user: crate::plex::session::UserRef,
    key: AudioPreferencesKey,
}

#[derive(Clone, Debug)]
pub(crate) struct PreferenceSnapshot {
    pub preferences: AudioPreferences,
    key: AudioPreferencesKey,
    revision: u64,
}

/// `None` leaves a setting untouched; `Some("")` clears a language preference.
#[derive(Clone, Debug, Default)]
pub(crate) struct PreferenceUpdate {
    pub auto_select_audio: Option<bool>,
    pub audio_language: Option<String>,
    pub subtitle_language: Option<String>,
    pub subtitle_mode: Option<i64>,
    pub subtitle_forced: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreferenceError {
    Stale,
    Refused,
    TimedOut,
    Unavailable,
    InvalidResponse,
    InvalidPreference,
}

impl PreferenceError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Stale => "Your profile or preferences changed. Reload settings and try again.",
            Self::Refused => "Plex did not authorize this account request. Sign in again and retry.",
            Self::TimedOut => "Plex took too long to respond. Please retry.",
            Self::Unavailable => "Could not reach Plex. Check your connection and retry.",
            Self::InvalidResponse => "Plex returned an unexpected response. Please retry.",
            Self::InvalidPreference => "This preference value is not supported.",
        }
    }
}

impl PreferenceRequest {
    /// No network or storage I/O: the session and active identity are immutable live snapshots.
    pub(crate) fn capture() -> Option<Self> {
        let current = crate::plex::session::current_snapshot();
        let user = current.user.clone()?;
        let credential = crate::plex::session::plex_tv_credential(&user)?;
        let client_id = crate::plex::session::peek().client_id.clone();
        if client_id.is_empty() { return None; }
        Some(Self { key: AudioPreferencesKey::new(&user, current.generation),
            client_id, credential, user })
    }

    pub(crate) fn is_current(&self) -> bool { self.key.is_current() }

    pub(crate) fn load(&self) -> Result<PreferenceSnapshot, PreferenceError> {
        self.load_with(&super::AUDIO_PREFERENCES_CACHE, &PREFERENCE_IO,
            || self.is_current(), |method, path, query| self.request(method, path, query))
    }

    pub(crate) fn save(&self, snapshot: &PreferenceSnapshot, update: PreferenceUpdate)
        -> Result<PreferenceSnapshot, PreferenceError>
    {
        self.save_with(&super::AUDIO_PREFERENCES_CACHE, &PREFERENCE_IO, snapshot, update,
            || self.is_current(), |method, path, query| self.request(method, path, query))
    }

    fn request(&self, method: &str, path: &str, query: &str)
        -> Result<crate::net::Resp, PreferenceError>
    {
        let base = super::plex_tv();
        let base = if path.ends_with("/profile") && base == super::PLEX_TV {
            CLIENTS_PLEX_TV
        } else { base };
        let url = if query.is_empty() { format!("{base}{path}") }
            else { format!("{base}{path}?{query}") };
        let client = AccountClient::new(&self.client_id, Some(&self.credential));
        let response = crate::net::request_evidence(&url, &client.headers(), method,
            (method == "PUT").then_some(b"".as_slice()), crate::net::API, false, None, None);
        super::note_response_contact(&url, &response);
        response.map_err(|failure| if failure.cause == crate::net::RequestError::TimedOut {
            PreferenceError::TimedOut
        } else { PreferenceError::Unavailable }).and_then(accepted)
    }

    fn load_with<F, C>(&self, cache: &AudioPreferencesCache, io: &Mutex<()>, current: C,
        mut request: F) -> Result<PreferenceSnapshot, PreferenceError>
    where F: FnMut(&str, &str, &str) -> Result<crate::net::Resp, PreferenceError>,
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
    where F: FnMut(&str, &str, &str) -> Result<crate::net::Resp, PreferenceError>,
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

fn accepted(response: crate::net::Resp) -> Result<crate::net::Resp, PreferenceError> {
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
            format!("{key}={}", crate::pms::urlenc_str(&value)))
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
        PreferenceRequest { client_id: "fixture-client".into(), credential: "account-only-token".into(),
            key: AudioPreferencesKey::new(&user, 4), user }
    }
    fn response(body: &str) -> crate::net::Resp {
        crate::net::Resp { status: 200, body: body.as_bytes().to_vec() }
    }
    fn fixture(method: &str, path: &str, query: &str) -> Result<crate::net::Resp, PreferenceError> {
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
                Ok(crate::net::Resp { status: 204, body: Vec::new() })
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
