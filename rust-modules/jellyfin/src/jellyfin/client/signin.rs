//! Signing in (password and Quick Connect), and the user's language preferences.

use super::*;

impl JfClient {
    // ---- server and sign-in ----

    /// Is there a Jellyfin server at this address, and what is it called?
    pub fn probe(&self) -> Result<PublicSystemInfo, SignInError> {
        let a = self.send(Verb::Get, &paths::public_info(), None).ok_or(SignInError::Unreachable)?;
        if !a.ok() {
            return Err(SignInError::NotJellyfin);
        }
        let info: PublicSystemInfo = serde_json::from_slice(&a.body).map_err(|_| SignInError::NotJellyfin)?;
        if info.id.is_empty() {
            return Err(SignInError::NotJellyfin);
        }
        Ok(info)
    }

    /// Sign in with a username and password.
    pub fn sign_in(&mut self, username: &str, password: &str) -> Result<Session, SignInError> {
        let body = paths::authenticate_by_name_body(username, password);
        let a = self
            .send(Verb::Post, &paths::authenticate_by_name(), Some(&body))
            .ok_or(SignInError::Unreachable)?;
        self.adopt(a)
    }

    /// Start Quick Connect. The answer's `code` is what the user types into another signed-in
    /// Jellyfin app; poll [`Self::quick_connect_poll`] with its `secret`.
    pub fn quick_connect_start(&self) -> Result<QuickConnectResult, SignInError> {
        let a = self
            .send(Verb::Post, &paths::quick_connect_initiate(), None)
            .ok_or(SignInError::Unreachable)?;
        match a.status {
            200..=299 => serde_json::from_slice(&a.body).map_err(|_| SignInError::NotJellyfin),
            401 | 403 => Err(SignInError::QuickConnectDisabled),
            s => Err(SignInError::Server(s)),
        }
    }

    /// One poll of a Quick Connect request: `Ok(None)` while the user hasn't approved it yet,
    /// `Ok(Some(session))` once they have (the client is then signed in).
    pub fn quick_connect_poll(&mut self, secret: &str) -> Result<Option<Session>, SignInError> {
        let a = self
            .send(Verb::Get, &paths::quick_connect_state(secret), None)
            .ok_or(SignInError::Unreachable)?;
        if a.status == 404 {
            return Err(SignInError::Rejected); // the request expired
        }
        if !a.ok() {
            return Err(SignInError::Server(a.status));
        }
        let state: QuickConnectResult = serde_json::from_slice(&a.body).map_err(|_| SignInError::NotJellyfin)?;
        if !state.authenticated {
            return Ok(None);
        }
        let body = paths::quick_connect_authenticate_body(secret);
        let a = self
            .send(Verb::Post, &paths::quick_connect_authenticate(), Some(&body))
            .ok_or(SignInError::Unreachable)?;
        self.adopt(a).map(Some)
    }

    pub(super) fn adopt(&mut self, a: Answer) -> Result<Session, SignInError> {
        match a.status {
            200..=299 => {}
            401 | 403 => return Err(SignInError::Rejected),
            s => return Err(SignInError::Server(s)),
        }
        let r: AuthenticationResult = serde_json::from_slice(&a.body).map_err(|_| SignInError::NotJellyfin)?;
        if r.access_token.is_empty() || r.user.id.is_empty() {
            return Err(SignInError::NotJellyfin);
        }
        let s = Session {
            user_id: normalize(&r.user.id),
            user_name: r.user.name,
            avatar_tag: r.user.primary_image_tag,
            token: r.access_token,
            server_id: normalize(&r.server_id),
        };
        self.session = Some(s.clone());
        Ok(s)
    }

    // ---- the user's preferences ----

    /// The signed-in user's audio and subtitle settings.
    pub fn language_preferences(&self) -> Result<UserLanguageConfig, PreferenceError> {
        UserLanguageConfig::deserialize(&self.user_configuration(FRESH)?).map_err(|_| PreferenceError::InvalidResponse)
    }

    /// Change the signed-in user's audio and subtitle settings, answering them as saved. The server
    /// replaces the whole configuration on a save, so it is read first and posted back whole, with
    /// only `update`'s keys changed: a setting this client does not model is never reset.
    pub fn save_language_preferences(&self, update: &UserLanguageUpdate) -> Result<UserLanguageConfig, PreferenceError> {
        let mut config = self.user_configuration(Duration::ZERO)?;
        let (Some(keys), Ok(serde_json::Value::Object(changed))) = (config.as_object_mut(), serde_json::to_value(update))
        else {
            return Err(PreferenceError::InvalidResponse);
        };
        keys.extend(changed);
        let body = config.to_string();
        preference_answer(self.send(Verb::Post, &paths::user_configuration(self.user_id()), Some(&body)))?;
        self.forget_user();
        serde_json::from_value(config).map_err(|_| PreferenceError::InvalidResponse)
    }

    /// The signed-in user's whole `Configuration`, as the server sent it in the last `fresh_for`
    /// ([`Self::user_value`]).
    pub(super) fn user_configuration(&self, fresh_for: Duration) -> Result<serde_json::Value, PreferenceError> {
        match self.user_value(fresh_for)?.get("Configuration") {
            Some(config @ serde_json::Value::Object(_)) => Ok(config.clone()),
            _ => Err(PreferenceError::InvalidResponse),
        }
    }
}
