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
    /// What the seat is granted as of the last SUCCESSFUL answer. It outlives that answer's TTL, a
    /// re-read in flight and a failed one: the TTL only decides when to ask again, never what the
    /// page shows, so a row or an automatic offer does not drop out and come back every few
    /// minutes while a viewer sits on it. Cleared with the seat; `None` is "not known yet".
    settled: Option<MarkerAccess>,
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
        self.settled = None;
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
        if let Ok(access) = answer {
            self.settled = Some(access);
        }
        self.answer = Some(answer);
        self.at = now;
    }
    fn view(&self, now: u32, intro: MarkerSkipMode, credits: MarkerSkipMode) -> MarkerPrefsView {
        let grants = self.settled.unwrap_or_default();
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
/// **The event-log line for one marker-access read**: whose seat asked (`owner` is the Home
/// administrator or a profile-less sign-in, `member` any other Home profile), then what plex.tv
/// answered: the two grants, or the error's name. Never a response body, a token or an identity, so
/// a report that a profile lacks the skip rows says which of "no Pass in the list", "identity
/// refused", "no credential" or "unreachable" it was.
fn outcome_line(member: bool, answer: &Result<MarkerAccess, PreferenceError>) -> String {
    let seat = if member { "member" } else { "owner" };
    match answer {
        Ok(access) => format!("marker-grants: seat={seat} intro={} credits={}", access.intro, access.credits),
        Err(error) => format!("marker-grants: seat={seat} unavailable ({error:?})"),
    }
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
            plx_base::eventlog::log("marker-grants: unavailable (no plex.tv credential is held for the seated profile)");
            self.state
                .land(scope, Err(PreferenceError::Unavailable), now);
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.pending = Some((scope, rx));
        plx_base::task::spawn_small("marker-grants", move || {
            let result = request.load_marker_access();
            let member = !plx_plex::plex::session::peek().active_profile_is_admin();
            plx_base::eventlog::log(&outcome_line(member, &result));
            let _ = tx.send(result);
            plx_machine::present::wake_from_worker();
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
    /// The line names the seat and the grants or the error, and nothing else.
    #[test]
    fn the_outcome_line_names_the_seat_and_the_answer_only() {
        let both = Ok(MarkerAccess { intro: true, credits: true });
        assert_eq!(outcome_line(false, &both), "marker-grants: seat=owner intro=true credits=true");
        assert_eq!(outcome_line(true, &Ok(MarkerAccess::default())), "marker-grants: seat=member intro=false credits=false");
        for error in [PreferenceError::Refused, PreferenceError::Stale, PreferenceError::TimedOut,
            PreferenceError::Unavailable, PreferenceError::InvalidResponse] {
            assert_eq!(outcome_line(true, &Err(error)), format!("marker-grants: seat=member unavailable ({error:?})"));
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
            state
                .view(
                    300_004,
                    MarkerSkipMode::Automatic,
                    MarkerSkipMode::Automatic
                )
                .intro_granted,
            "an answer past its TTL is stale, not withdrawn: the view keeps the settled grants"
        );
        let next = state.begin(300_004).unwrap();
        state.land(next, Err(PreferenceError::TimedOut), 300_005);
        assert_eq!(state.begin(345_004), None);
        assert!(state.begin(345_005).is_some());
    }

    /// **The grants never flip while a seat sits on a page** (the Settings > Playback skip rows, the
    /// player's automatic offer): past its TTL an answer asks again but keeps standing, while the
    /// re-read is in flight and when it fails. Only a landed answer replaces it; only another seat
    /// clears it.
    #[test]
    fn settled_grants_stand_through_a_refresh_a_failure_and_only_a_new_answer_or_seat_changes_them() {
        let both = MarkerAccess { intro: true, credits: true };
        let view = |state: &AccessState, now| state.view(now, MarkerSkipMode::Automatic, MarkerSkipMode::Automatic);
        let mut state = AccessState::default();
        state.seat(Some(profile(1)));
        let scope = state.begin(0).unwrap();
        let loading = view(&state, 1);
        assert!(!loading.intro_granted && !loading.credits_granted, "nothing is known before the first answer");
        state.land(scope, Ok(both), 2);
        assert!(view(&state, 3).intro_granted && view(&state, 3).credits_granted);
        let ttl = plx_plex::plex::account::AUDIO_PREFERENCES_SUCCESS_TTL.as_millis() as u32;
        // TTL expiry: a refresh is due, the grants stand.
        let stale = view(&state, ttl + 3);
        assert!(stale.needs_refresh && stale.intro_granted && stale.credits_granted);
        // The refresh in flight: the grants stand, and no second refresh is asked for.
        let scope = state.begin(ttl + 3).unwrap();
        let loading = view(&state, ttl + 4);
        assert!(!loading.needs_refresh && loading.intro_granted && loading.credits_granted);
        // It fails: still standing, retried after the failure TTL.
        state.land(scope, Err(PreferenceError::TimedOut), ttl + 5);
        assert!(view(&state, ttl + 6).intro_granted, "a transient failure does not withdraw a settled grant");
        // It answers: the new answer replaces the old.
        let scope = state.begin(ttl + 60_000).unwrap();
        state.land(scope, Ok(MarkerAccess { intro: true, credits: false }), ttl + 60_001);
        let now = view(&state, ttl + 60_002);
        assert!(now.intro_granted && !now.credits_granted);
        // Another seat starts unknown.
        state.seat(Some(profile(2)));
        assert!(!view(&state, ttl + 60_003).intro_granted);
    }
}
