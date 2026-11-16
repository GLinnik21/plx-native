//! Retained screen inputs. The host captures these at its publication boundary; screens never
//! resolve them from process state. Empty inputs explicitly represent a cold or controlled host.
use std::sync::Arc;
use plx_plex::plex::{grant::PlaintextVerdict, session::{CurrentProfile, PlaintextChoice, Session}, ServerId};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrantInputs {
    pub offers: Vec<PlaintextVerdict>,
    pub choices: Vec<(String, PlaintextChoice)>,
    pub connected: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimePolicy {
    pub harness_driven: bool,
    pub lookahead_rows: usize,
}
impl RuntimePolicy {
    /// Shared product/fixture rule; the app owns any trigger read and its latch.
    pub fn lookahead_rows_for(trigger: Option<&str>) -> usize {
        trigger.and_then(|text| text.trim().parse::<usize>().ok()).map(|rows| rows.min(4)).unwrap_or(3)
    }
}
impl Default for RuntimePolicy {
    fn default() -> Self { Self { harness_driven: false, lookahead_rows: 3 } }
}

#[derive(Clone)]
pub struct ScreenInputs {
    pub runtime_policy: RuntimePolicy,
    /// Confirmed install-wide next-launch choice, independent of the seated profile/cache.
    pub install_language: plx_platform::i18n::Preference,
    /// Boot-fixed preference used for restart decisions, not the display catalog accessor.
    pub running_language: plx_platform::i18n::Preference,
    /// Complete app-boundary presentation; controlled hosts never sample live diagnostics.
    pub player_presentation: plx_appkit::player_hud::Presentation,
    /// Effective app-boundary capture, including optimistic picks not yet persisted.
    /// Absent in cold/legacy hosts: resolve only this host's retained Session and Profile.
    pub preferences: Option<plx_plex::plex::session::ProfilePrefs>,
    pub session: Arc<Session>,
    pub settled: bool,
    pub visible_generation: u64,
    /// Advances only when the host publishes different screen-visible facts.
    pub revision: u64,
    pub profile: Arc<CurrentProfile>,
    pub current_server: ServerId,
    /// Stable machine-to-slot identities captured from the host's S4 client keys.
    pub servers: Arc<Vec<(String, ServerId)>>,
    pub grants: Arc<GrantInputs>,
    /// A host-local semantic revision, independent of server-roster changes.
    pub grant_generation: u64,
    pub resolution_offered: bool,
}
impl Default for ScreenInputs {
    fn default() -> Self {
        Self { session: Arc::new(Session::default()), settled: false, visible_generation: 0, revision: 0,
            profile: Arc::new(CurrentProfile { user: None, generation: 0, stamp: String::new() }),
            current_server: ServerId::UNSET, servers: Default::default(), grants: Arc::new(GrantInputs::default()),
            grant_generation: 0, resolution_offered: false, runtime_policy: RuntimePolicy::default(), preferences: None,
            install_language: Default::default(), running_language: Default::default(), player_presentation: Default::default() }
    }
}
impl ScreenInputs {
    pub fn preference<S: plx_plex::plex::session::ProfileSetting>(&self) -> S::Value {
        if let Some(value) = self.preferences.as_ref().and_then(S::own) { return value; }
        // Resolve the retained stamp directly: Seat::of also logs an unstamped publication.
        let own = self.profile.user.as_ref().filter(|_| !self.profile.stamp.is_empty()).and_then(|user|
            plx_plex::plex::session::own_prefs(&self.session.prefs.profile_prefs, &user.uuid, &self.profile.stamp));
        plx_plex::plex::session::effective::<S>(&self.session.prefs, own)

    }
    pub fn settled_session(&self) -> Option<&Arc<Session>> { self.settled.then_some(&self.session) }
    pub fn same_facts(&self, other: &Self) -> bool {
        self.runtime_policy == other.runtime_policy
            && self.install_language == other.install_language
            && self.running_language == other.running_language
            && self.player_presentation == other.player_presentation
                    && self.preferences == other.preferences
            && self.visible_generation == other.visible_generation
            && self.settled == other.settled
            && self.session.ui_resolution() == other.session.ui_resolution()
            && self.current_server == other.current_server
            && self.servers == other.servers
            && self.resolution_offered == other.resolution_offered
            && self.grant_generation == other.grant_generation
            && self.profile.generation == other.profile.generation
            && self.profile.stamp == other.profile.stamp
            && match (&self.profile.user, &other.profile.user) {
                (None, None) => true,
                (Some(a), Some(b)) => a.id == b.id && a.uuid == b.uuid && a.title == b.title,
                _ => false,
            }
    }
    pub fn session_key(&self) -> (u64, bool, u32) {
        (self.revision, self.settled, self.profile.generation)
    }
}

#[cfg(test)]
pub(crate) fn empty_fixture() -> &'static ScreenInputs {
    static EMPTY: std::sync::OnceLock<ScreenInputs> = std::sync::OnceLock::new();
    EMPTY.get_or_init(ScreenInputs::default)
}

/// The explicit registry boundary of legacy fixture hosts, never a production screen fallback.
#[cfg(test)]
pub(crate) fn capture_fixture() -> ScreenInputs {
    use plx_plex::plex::{grant, session};
    let mut keys = plx_plex::plex::ClientKeys::default();
    keys.refill_roster(plx_plex::plex::current_server(), plx_plex::plex::server_roster_gen(),
        plx_plex::plex::server_facts_gen(), plx_plex::plex::server_ids(),
        plx_plex::plex::client_for, plx_plex::plex::server_facts, plx_plex::plex::server_probe_result);
    let servers = keys.iter().filter_map(|(sid, _)| keys.facts(sid)
        .map(|facts| (facts.machine_id.clone(), sid))).collect();
    let settled = session::peek_settled();
    let stored = settled.clone().unwrap_or_else(session::peek);
    ScreenInputs { grants: Arc::new(GrantInputs { offers: grant::offers(),
        choices: grant::choices(&stored.prefs.plaintext_consent, &stored.account_token),
        connected: grant::granted_machines() }), session: stored, settled: settled.is_some(),
        visible_generation: session::visible_generation(),
        revision: { let (cache, profile) = session::screen_input_revision(); cache.wrapping_add(profile) },
        profile: session::current_snapshot(),
        current_server: plx_plex::plex::current_server(), servers: Arc::new(servers), grant_generation: grant::revision(),
        resolution_offered: plx_platform::tv::device::ui_resolution_offered(), runtime_policy: RuntimePolicy::default(),
        install_language: plx_platform::i18n::saved_preference(), running_language: plx_platform::i18n::current().preference(), player_presentation: Default::default(),
        preferences: crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.get()).map(|setting|
            session::ProfilePrefs { under_posters: Some(setting), ..Default::default() }) }
}

/// Explicit caption-policy input for neutral card fixtures; never a production fallback.
#[cfg(test)]
pub(crate) fn caption_fixture() -> &'static ScreenInputs {
    Box::leak(Box::new(ScreenInputs {
        preferences: Some(plx_plex::plex::session::ProfilePrefs {
            under_posters: Some(crate::registry::captions::fixture_setting()), ..Default::default()
        }), ..Default::default()
    }))
}

/// Immutable publications for legacy fixture contexts that outlive their helper call.
/// Each capture is deliberately independent, like the fixtures' leaked catalog rows.
#[cfg(test)]
pub(crate) fn publication_fixture() -> &'static ScreenInputs {
    Box::leak(Box::new(capture_fixture()))
}
