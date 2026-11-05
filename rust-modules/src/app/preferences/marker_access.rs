//! Runner for profile-scoped viewer features. State transitions take identities, replies and
//! frame time as values. Only the runner's `ensure` starts IO, after AppFx admission.
use plx_appkit::skip_policy::MarkerPrefsView;
use plx_plex::plex::account::{MarkerAccess, PreferenceError, PreferenceRequest};
use plx_plex::plex::session::{CurrentProfile, MarkerSkipMode};
use std::sync::mpsc::{self, Receiver};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileKey {
    generation: u32,
    stamp: String,
    id: i64,
    uuid: String,
}
impl ProfileKey {
    fn matches(&self, profile: &CurrentProfile) -> bool {
        self.generation == profile.generation
            && self.stamp == profile.stamp
            && profile
                .user
                .as_ref()
                .is_some_and(|u| self.id == u.id && self.uuid == u.uuid)
    }
    fn of(profile: &CurrentProfile) -> Option<Self> {
        let user = profile.user.as_ref()?;
        if profile.stamp.is_empty() {
            return None;
        }
        Some(Self {
            generation: profile.generation,
            stamp: profile.stamp.clone(),
            id: user.id,
            uuid: user.uuid.clone(),
        })
    }
}
#[derive(Default)]
struct AccessState {
    profile: Option<ProfileKey>,
    scope: u64,
    answer: Option<Result<MarkerAccess, PreferenceError>>,
    at: u32,
    awaiting: bool,
}
impl AccessState {
    fn align(&mut self, profile: &CurrentProfile) -> bool {
        if self
            .profile
            .as_ref()
            .is_some_and(|key| key.matches(profile))
            || (self.profile.is_none() && (profile.user.is_none() || profile.stamp.is_empty()))
        {
            return false;
        }
        self.seat(ProfileKey::of(profile))
    }
    fn seat(&mut self, profile: Option<ProfileKey>) -> bool {
        if self.profile == profile {
            return false;
        }
        self.profile = profile;
        self.scope = self.scope.wrapping_add(1).max(1);
        self.answer = None;
        self.awaiting = false;
        true
    }
    fn fresh(&self, now: u32) -> bool {
        let ttl = match self.answer {
            Some(Ok(_)) => plx_plex::plex::account::AUDIO_PREFERENCES_SUCCESS_TTL,
            _ => plx_plex::plex::account::AUDIO_PREFERENCES_FAILURE_TTL,
        };
        self.answer.is_some() && now.wrapping_sub(self.at) < ttl.as_millis() as u32
    }
    fn begin(&mut self, now: u32) -> Option<u64> {
        if self.profile.is_none() || self.awaiting || self.fresh(now) {
            return None;
        }
        self.awaiting = true;
        self.answer = None;
        Some(self.scope)
    }
    fn land(&mut self, scope: u64, answer: Result<MarkerAccess, PreferenceError>, now: u32) {
        if scope != self.scope || !self.awaiting {
            return;
        }
        self.awaiting = false;
        self.answer = Some(answer);
        self.at = now;
    }
    fn view(&self, now: u32, intro: MarkerSkipMode, credits: MarkerSkipMode) -> MarkerPrefsView {
        let grants = if self.fresh(now) {
            self.answer.and_then(Result::ok).unwrap_or_default()
        } else {
            MarkerAccess::default()
        };
        MarkerPrefsView {
            scope: if self.profile.is_some() {
                self.scope
            } else {
                0
            },
            needs_refresh: self.profile.is_some() && !self.awaiting && !self.fresh(now),
            intro_granted: grants.intro,
            credits_granted: grants.credits,
            intro,
            credits,
        }
    }
}
#[derive(Default)]
pub(crate) struct MarkerAccessRunner {
    state: AccessState,
    pending: Option<(u64, Receiver<Result<MarkerAccess, PreferenceError>>)>,
}
impl MarkerAccessRunner {
    pub fn poll(
        &mut self,
        profile: &CurrentProfile,
        now: u32,
        intro: MarkerSkipMode,
        credits: MarkerSkipMode,
    ) -> MarkerPrefsView {
        if self.state.align(profile) {
            self.pending = None;
        }
        if let Some((scope, rx)) = &self.pending {
            let answer = match rx.try_recv() {
                Ok(answer) => Some(answer),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(PreferenceError::Unavailable)),
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(answer) = answer {
                self.state.land(*scope, answer, now);
                self.pending = None;
            }
        }
        self.state.view(now, intro, credits)
    }
    pub fn ensure(&mut self, profile: &CurrentProfile, now: u32) {
        if self.state.align(profile) {
            self.pending = None;
        }
        let Some(scope) = self.state.begin(now) else {
            return;
        };
        let Some(request) = PreferenceRequest::capture_marker_access().filter(|r| r.matches_profile(profile))
        else {
            self.state
                .land(scope, Err(PreferenceError::Unavailable), now);
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.pending = Some((scope, rx));
        plx_base::task::spawn_small("marker-grants", move || {
            let result = request.load_marker_access();
            let _ = tx.send(result);
            plx_machine::idle::invalidate();
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn profile(generation: u32) -> ProfileKey {
        ProfileKey {
            generation,
            stamp: "account".into(),
            id: 1,
            uuid: "viewer".into(),
        }
    }
    #[test]
    fn replies_are_fenced_across_a_b_a_and_freshness_is_bounded() {
        let mut state = AccessState::default();
        state.seat(Some(profile(1)));
        let first = state.begin(0).unwrap();
        assert_eq!(state.begin(1), None);
        state.seat(Some(profile(2)));
        state.seat(Some(profile(3)));
        let last = state.begin(2).unwrap();
        state.land(
            first,
            Ok(MarkerAccess {
                intro: true,
                credits: true,
            }),
            3,
        );
        assert!(
            !state
                .view(3, MarkerSkipMode::Automatic, MarkerSkipMode::Automatic)
                .intro_granted
        );
        state.land(
            last,
            Ok(MarkerAccess {
                intro: true,
                credits: false,
            }),
            4,
        );
        assert!(
            state
                .view(4, MarkerSkipMode::Automatic, MarkerSkipMode::Automatic)
                .intro_granted
        );
        assert!(
            !state
                .view(
                    300_004,
                    MarkerSkipMode::Automatic,
                    MarkerSkipMode::Automatic
                )
                .intro_granted
        );
        let next = state.begin(300_004).unwrap();
        state.land(next, Err(PreferenceError::TimedOut), 300_005);
        assert_eq!(state.begin(345_004), None);
        assert!(state.begin(345_005).is_some());
    }
}
