//! Owned auth states. Only SessionMachine::transition installs a runtime Attempt.
//! Arbitrary legacy flags live solely in read-only history, never in active work authority.
use super::*;

pub(super) struct Common {
    pub(super) authority: BootstrapAuthority,
    pub(super) disk_identity: Identity,
    pub(super) persisted: PersistedSession,
    pub(super) committed_credentials: CredentialPatch,
    pub(super) signin_notice: SignInNotice,
    pub(super) next_qr: u64,
    pub(super) epoch: u64,
    pub(super) next_req: u32,
    pub(super) pending: BTreeMap<u32, Pending>,
    pub(super) pending_commit: Option<PendingCommit>,
    pub(super) persistence_purpose: Option<PersistencePurpose>,
    pub(super) commit_phase: CommitPhase,
    pub(super) admitted_persistence: Option<AdmittedPersistence>,
    pub(super) persistence_warning: Option<PersistenceWarning>,
    pub(super) held_handoff: Option<HeldHandoff>,
    pub(super) persistence_warning_answered: bool,
    pub(super) unconfirmed_fresh_prior: Option<Identity>,
    pub(super) inbox: VecDeque<SessionEnvelope>,
    pub(super) pump_pending: bool,
    pub(super) active_profile: Option<UserRef>,
    pub(super) profile_scope: ProfileScope,
    pub(super) delete_leftovers: usize,
    pub(super) incident: Option<IncidentOffer>,
    pub(super) incidents_seen: Vec<IncidentKey>,
    pub(super) next_incident: u32,
    pub(super) switch_refused_for: Option<Identity>,
}


pub(super) struct QrHistory { pub(super) code: String, pub(super) png: Vec<u8>, pub(super) generation: u64, pub(super) replaced: bool, pub(super) trouble: bool }
pub(super) struct PickerPresentation { pub(super) picker: Picker, pub(super) users: Vec<UserTile>, pub(super) error: String, pub(super) pin_denied: bool }
// Read-only retention: never confers work or selection authority.
struct HistoricalPresentation { qr: QrHistory, picker: PickerPresentation }
struct ImportedIntent(bool);
struct HandoffIntent(bool);

pub(super) struct IdleData { boot: BootStage, signin: SignInHistory, history: HistoricalPresentation, residue: ImportedIntent }
pub(super) struct CreatingData { login: LoginStage, signin: SignInHistory, qr: QrHistory, picker_history: PickerPresentation, residue: ImportedIntent }
pub(super) struct WaitingData { login: LoginStage, signin: SignInHistory, qr: QrHistory, picker_history: PickerPresentation, residue: ImportedIntent }
pub(super) struct DiscoveringData { discovery: DiscoveryStage, signin: SignInHistory, history: HistoricalPresentation, residue: ImportedIntent }
pub(super) struct ProfilesData { setup: PickerStage, choice: Authorization, signin: SignInHistory, qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
pub(super) struct SwitchingData { selection: SelectionStage, signin: SignInHistory, qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
pub(super) struct ReadyData { handoff: Handoff, roster: Option<ProfileStream>, signin: SignInHistory, history: HistoricalPresentation, intent: HandoffIntent }
pub(super) struct ErrorData { retry: RetryAuthority, signin: SignInHistory, qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
pub(super) struct DeletedData { signin: SignInHistory, history: HistoricalPresentation, residue: ImportedIntent }
pub(super) struct ErasingData { signin: SignInHistory, history: HistoricalPresentation, residue: ImportedIntent, prior_phase: Phase, pending_erase: (u64, bool) }


// A cross-phase coordinator debt, not authorization to run a sign-in or save credentials.
#[derive(Clone, Copy)]
pub(super) enum SignInNotice { Settled, Owed }
impl SignInNotice { pub(super) fn owed(self) -> bool { matches!(self, Self::Owed) } }
#[derive(Clone, Copy)]
enum HistoricalAuthorization { Stored, Fresh }
struct SignInHistory {
    authorization: HistoricalAuthorization,
    retry: Option<super::super::DiscoveryRetryProgress>,
    plaintext: Option<super::super::PlaintextVerdict>,
    account: Option<String>,
}
impl SignInHistory {
    fn authorized(&self) -> bool { matches!(self.authorization, HistoricalAuthorization::Fresh) }
}

#[derive(Clone, Copy)]
enum Authorization { Stored, Fresh }
impl Authorization { fn is_fresh(self) -> bool { matches!(self, Self::Fresh) } }
#[derive(Clone, Copy)]
enum LoginStage { Dormant, Preparing { boundary_req: u32, login_req: u32 }, Authorizing { req: u32 } }
impl LoginStage {
    fn owns(self, req: u32) -> bool { matches!(self, Self::Authorizing { req: owned } if owned == req) }
    fn promote(self, req: u32) -> Self { if matches!(self, Self::Preparing { login_req, .. } if login_req == req) { Self::Authorizing { req } } else { self } }
}
#[derive(Clone, Copy)]
enum DiscoveryStage { Dormant(Authorization), Running { req: u32, authorization: Authorization } }
impl DiscoveryStage {
    fn authorization(self) -> Authorization { match self { Self::Dormant(a) | Self::Running { authorization: a, .. } => a } }
    fn owns(self, req: u32) -> bool { matches!(self, Self::Running { req: owned, .. } if owned == req) }
    fn cancelled(self) -> Self { Self::Dormant(self.authorization()) }
    fn routine(self) -> Self { match self { Self::Dormant(_) => Self::Dormant(Authorization::Stored), Self::Running { req, .. } => Self::Running { req, authorization: Authorization::Stored } } }
}
#[derive(Clone, Copy)]
enum SelectionStage { Dormant(Authorization), Selecting { req: u32, authorization: Authorization } }
impl SelectionStage {
    fn authorization(self) -> Authorization { match self { Self::Dormant(a) | Self::Selecting { authorization: a, .. } => a } }
    fn owns(self, req: u32) -> bool { matches!(self, Self::Selecting { req: owned, .. } if owned == req) }
    fn cancelled(self) -> Self { Self::Dormant(self.authorization()) }
    fn routine(self) -> Self { match self { Self::Dormant(_) => Self::Dormant(Authorization::Stored), Self::Selecting { req, .. } => Self::Selecting { req, authorization: Authorization::Stored } } }
}
#[derive(Clone, Copy)]
enum BootStage { Dormant, ActivatingDev { req: u32 }, RestoringStored { req: u32 } }
impl BootStage {
    fn owns(self, req: u32) -> bool { matches!(self, Self::ActivatingDev { req: owned } | Self::RestoringStored { req: owned } if owned == req) }
}
#[derive(Clone, Copy)]
enum PickerStage { Idle, Installing { req: u32 } }
impl PickerStage { fn owns(self, req: u32) -> bool { matches!(self, Self::Installing { req: owned } if owned == req) } }
#[derive(Clone, Copy)]
enum FinalCommitStage { Unrequested, Committing { req: u32 } }
#[derive(Clone, Copy)]
enum Handoff { Available, Pending { authorization: Authorization, commit: FinalCommitStage } }
impl Handoff {
    fn authorization(self) -> Authorization { match self { Self::Available => Authorization::Stored, Self::Pending { authorization, .. } => authorization } }
    fn routine(self) -> Self { match self { Self::Available => Self::Available, Self::Pending { commit, .. } => Self::Pending { authorization: Authorization::Stored, commit } } }
    fn cancelled(self) -> Self { match self { Self::Available => Self::Available, Self::Pending { authorization, .. } => Self::Pending { authorization, commit: FinalCommitStage::Unrequested } } }
    fn owns(self, req: u32) -> bool { matches!(self, Self::Pending { commit: FinalCommitStage::Committing { req: owned }, .. } if owned == req) }
}
#[derive(Clone, Copy)]
struct ProfileStream { req: u32 }
#[derive(Clone, Copy)]
enum RetryAuthority { Login, Discovery(Authorization) }
impl RetryAuthority {
    fn of(a: Authorization) -> Self { if a.is_fresh() { Self::Discovery(a) } else { Self::Login } }
    fn authorization(self) -> Authorization { match self { Self::Login => Authorization::Stored, Self::Discovery(a) => a } }
}

pub(super) enum Attempt {
    Idle(IdleData),
    Creating(CreatingData),
    Waiting(WaitingData),
    Discovering(DiscoveringData),
    Profiles(ProfilesData),
    Switching(SwitchingData),
    Ready(ReadyData),
    Error(ErrorData),
    Deleted(DeletedData),
    Erasing(ErasingData),
}

pub(super) struct Partition { pub(super) common: Common, pub(super) attempt: Attempt }



struct PresentationRef<'a> { phase: Phase, qr: &'a QrHistory, picker: &'a PickerPresentation, apply_pending: bool, pending_erase: Option<(u64, bool)> }
impl Partition {
    pub(super) fn from_init(init: SessionInit) -> Self {
        let SessionInit { authority, disk_identity, phase, picker, persisted, committed_credentials, pin_code, qr_png, users, error, pin_denied, authorized_in_flow, signin_active, apply_pending, code_replaced, qr_gen, next_qr, epoch, next_req, pending, mut pending_commit, persistence_purpose, commit_phase, admitted_persistence, persistence_warning, held_handoff, persistence_warning_answered, unconfirmed_fresh_prior, pending_erase, inbox, pump_pending, active_profile, profile_scope, delete_leftovers, incident, incidents_seen, next_incident, link_trouble, discovery_retry, plaintext, signin_account, switch_refused_for } = init;
        if let Some(commit) = pending_commit.as_mut() { commit.delta.destination = CommitDestination::from_init(&commit.delta); }
        let authorization = if authorized_in_flow && !persisted.account_token.is_empty() { Authorization::Fresh } else { Authorization::Stored };
        let owned = |op: SessionOp| pending.iter().rev().find_map(|(&req, p)| (p.key.op == op && p.key.epoch == epoch && p.expected.matches(&persisted)).then_some(req));
        let login = owned(SessionOp::Login).map_or(LoginStage::Dormant, |req| LoginStage::Authorizing { req });
        let discovery = owned(SessionOp::Login).or_else(|| owned(SessionOp::Rediscover)).map_or(DiscoveryStage::Dormant(authorization), |req| DiscoveryStage::Running { req, authorization });
        let selection = owned(SessionOp::ProfileSwitch).map_or(SelectionStage::Dormant(authorization), |req| SelectionStage::Selecting { req, authorization });
        let roster = owned(SessionOp::ProfileSwitch).filter(|req| pending[req].phase == StreamPhase::ProfileSeated).map(|req| ProfileStream { req });
        let commit_req = |op: SessionOp| pending_commit.as_ref().and_then(|c| pending.get(&c.req).filter(|p| p.key.op == op && p.key.epoch == epoch && c.epoch == epoch && p.expected.matches(&persisted)).map(|_| c.req));
        let boot = if matches!(&authority, BootstrapAuthority::DevPms { .. }) && pending_commit.as_ref().is_some_and(|c| matches!(&c.delta.dev, Some(DevCommitDelta::Activated))) {
            commit_req(SessionOp::DevBoundary).map_or(BootStage::Dormant, |req| BootStage::ActivatingDev { req })
        } else if matches!(&authority, BootstrapAuthority::Account { .. }) && persisted.can_go_local() && pending_commit.as_ref().is_some_and(|c| matches!(c.delta.destination, CommitDestination::StoredReady)) {
            commit_req(SessionOp::Ready).map_or(BootStage::Dormant, |req| BootStage::RestoringStored { req })
        } else { BootStage::Dormant };
        let setup = if matches!(&authority, BootstrapAuthority::Account { .. }) && !persisted.account_token.is_empty() {
            commit_req(SessionOp::Picker).map_or(PickerStage::Idle, |req| PickerStage::Installing { req })
        } else { PickerStage::Idle };
        let commit = if pending_commit.as_ref().is_some_and(|c| matches!(c.delta.destination, CommitDestination::ReadyCommitted)) {
            commit_req(SessionOp::Ready).map_or(FinalCommitStage::Unrequested, |req| FinalCommitStage::Committing { req })
        } else { FinalCommitStage::Unrequested };
        let handoff = if apply_pending && persisted.can_go_local() { Handoff::Pending { authorization, commit } } else { Handoff::Available };
        let creating = pending_commit.as_ref().and_then(|c| if let Some(DevCommitDelta::StartAccount { login_req }) = c.delta.dev { Some(LoginStage::Preparing { boundary_req: c.req, login_req }) } else { None }).unwrap_or(login);
        let signin_notice = if signin_active { SignInNotice::Owed } else { SignInNotice::Settled };
        let common = Common { authority, disk_identity, persisted, committed_credentials, signin_notice, next_qr, epoch, next_req, pending, pending_commit, persistence_purpose, commit_phase, admitted_persistence, persistence_warning, held_handoff, persistence_warning_answered, unconfirmed_fresh_prior, inbox, pump_pending, active_profile, profile_scope, delete_leftovers, incident, incidents_seen, next_incident, switch_refused_for };
        let signin = SignInHistory { authorization: if authorized_in_flow { HistoricalAuthorization::Fresh } else { HistoricalAuthorization::Stored }, retry: discovery_retry, plaintext, account: signin_account };
        let qr = QrHistory { code: pin_code, png: qr_png, generation: qr_gen, replaced: code_replaced, trouble: link_trouble };
        let picker = PickerPresentation { picker, users, error, pin_denied };
        let attempt = if let Some(pending_erase) = pending_erase {
            Attempt::Erasing(ErasingData { signin, history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending), prior_phase: phase, pending_erase })
        } else { match phase {
            Phase::Idle => Attempt::Idle(IdleData { boot, signin, history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
            Phase::Creating => Attempt::Creating(CreatingData { login: creating, signin, qr, picker_history: picker, residue: ImportedIntent(apply_pending) }),
            Phase::Waiting => Attempt::Waiting(WaitingData { login, signin, qr, picker_history: picker, residue: ImportedIntent(apply_pending) }),
            Phase::Discovering => Attempt::Discovering(DiscoveringData { discovery, signin, history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
            Phase::Profiles => Attempt::Profiles(ProfilesData { setup, choice: authorization, signin, qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Switching => Attempt::Switching(SwitchingData { selection, signin, qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Ready => Attempt::Ready(ReadyData { handoff, roster, signin, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(apply_pending) }),
            Phase::Error => Attempt::Error(ErrorData { retry: RetryAuthority::of(authorization), signin, qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Deleted => Attempt::Deleted(DeletedData { signin, history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
        }};
        Self { common, attempt }
    }

}
#[derive(Clone, Copy)]
pub(super) struct PartitionView<'a> { pub(super) common: &'a Common, pub(super) attempt: &'a Attempt }
#[cfg(test)]
impl Partition {
    pub(super) fn phase(&self) -> Phase { self.attempt.phase() }
    pub(super) fn qr(&self) -> &QrHistory { self.attempt.qr() }
    pub(super) fn picker_data(&self) -> &PickerPresentation { self.attempt.picker_data() }
    pub(super) fn pending_erase(&self) -> Option<(u64, bool)> { self.attempt.pending_erase() }
    pub(super) fn view(&self) -> PartitionView<'_> { PartitionView { common: &self.common, attempt: &self.attempt } }
    pub(super) fn snapshot_init(&self) -> SessionInit { self.view().snapshot_init() }
}
impl PartitionView<'_> {
    pub(super) fn roster_dead_end(&self) -> bool { self.attempt.phase() == Phase::Profiles && self.attempt.picker_data().users.is_empty() && !self.common.pending.values().any(|pending| pending.key.op == SessionOp::HomeRoster) }
    pub(super) fn dead_end_resumes(&self) -> bool { matches!(self.common.authority, BootstrapAuthority::DevPms { .. }) || self.common.committed_credentials.merge_into(&self.common.persisted).can_go_local() }
    pub(super) fn switch_refused(&self) -> bool { self.common.switch_refused_for.as_ref().is_some_and(|id| id.matches(&self.common.persisted)) }
    pub(super) fn readout_account(&self) -> Option<&str> { self.attempt.readout_account() }
    pub(super) fn roster_readout_up(&self) -> bool { self.attempt.roster_readout_up() }
    /// Allocate an owned DTO only at the explicit snapshot boundary.
    pub(super) fn snapshot_init(&self) -> SessionInit {
        let p = self.attempt.presentation();
        SessionInit {
            authority: self.common.authority.clone(),
            disk_identity: self.common.disk_identity.clone(),
            phase: p.phase,
            picker: p.picker.picker,
            persisted: self.common.persisted.clone(),
            committed_credentials: self.common.committed_credentials.clone(),
            pin_code: p.qr.code.clone(),
            qr_png: p.qr.png.clone(),
            users: p.picker.users.clone(),
            error: p.picker.error.clone(),
            pin_denied: p.picker.pin_denied,
            authorized_in_flow: self.attempt.projected_authorization(),
            signin_active: self.common.signin_notice.owed(),
            apply_pending: p.apply_pending,
            code_replaced: p.qr.replaced,
            qr_gen: p.qr.generation,
            next_qr: self.common.next_qr.clone(),
            epoch: self.common.epoch.clone(),
            next_req: self.common.next_req.clone(),
            pending: self.common.pending.clone(),
            pending_commit: self.common.pending_commit.clone(),
            persistence_purpose: self.common.persistence_purpose.clone(),
            commit_phase: self.common.commit_phase.clone(),
            admitted_persistence: self.common.admitted_persistence.clone(),
            persistence_warning: self.common.persistence_warning.clone(),
            held_handoff: self.common.held_handoff.clone(),
            persistence_warning_answered: self.common.persistence_warning_answered.clone(),
            unconfirmed_fresh_prior: self.common.unconfirmed_fresh_prior.clone(),
            pending_erase: p.pending_erase,
            inbox: self.common.inbox.clone(),
            pump_pending: self.common.pump_pending.clone(),
            active_profile: self.common.active_profile.clone(),
            profile_scope: self.common.profile_scope.clone(),
            delete_leftovers: self.common.delete_leftovers.clone(),
            incident: self.common.incident.clone(),
            incidents_seen: self.common.incidents_seen.clone(),
            next_incident: self.common.next_incident.clone(),
            link_trouble: p.qr.trouble,
            discovery_retry: self.attempt.discovery_retry(),
            plaintext: self.attempt.plaintext().cloned(),
            signin_account: self.attempt.signin_account().map(str::to_owned),
            switch_refused_for: self.common.switch_refused_for.clone(),
        }
    }
}

impl Attempt {
    pub(super) fn placeholder() -> Self {
        Self::Idle(IdleData { boot: BootStage::Dormant, history: HistoricalPresentation {
            qr: QrHistory { code: String::new(), png: Vec::new(), generation: 0, replaced: false, trouble: false },
            picker: PickerPresentation { picker: Picker::Boot, users: Vec::new(), error: String::new(), pin_denied: false },
        }, residue: ImportedIntent(false), signin: SignInHistory { authorization: HistoricalAuthorization::Stored, retry: None, plaintext: None, account: None } })
    }
    fn retained_signin(&self) -> &SignInHistory {
        match self {
            Self::Idle(d) => &d.signin,
            Self::Creating(d) => &d.signin,
            Self::Waiting(d) => &d.signin,
            Self::Discovering(d) => &d.signin,
            Self::Profiles(d) => &d.signin,
            Self::Switching(d) => &d.signin,
            Self::Ready(d) => &d.signin,
            Self::Error(d) => &d.signin,
            Self::Deleted(d) => &d.signin,
            Self::Erasing(d) => &d.signin,
        }
    }
    pub(super) fn phase(&self) -> Phase { self.presentation().phase }
    pub(super) fn qr(&self) -> &QrHistory { self.presentation().qr }
    pub(super) fn picker_data(&self) -> &PickerPresentation { self.presentation().picker }
    pub(super) fn pending_erase(&self) -> Option<(u64, bool)> { self.presentation().pending_erase }
    pub(super) fn projected_authorization(&self) -> bool { self.retained_signin().authorized() }
    pub(super) fn discovery_retry(&self) -> Option<super::super::DiscoveryRetryProgress> { self.retained_signin().retry }
    pub(super) fn plaintext(&self) -> Option<&super::super::PlaintextVerdict> { self.retained_signin().plaintext.as_ref() }
    pub(super) fn signin_account(&self) -> Option<&str> { self.retained_signin().account.as_deref() }
    pub(super) fn readout_account(&self) -> Option<&str> { self.signin_account().filter(|_| self.phase() == Phase::Error && self.picker_data().error == plx_platform::i18n::msg::browse_auth_no_servers()) }
    pub(super) fn roster_readout_up(&self) -> bool { is_roster_readout(self.phase(), &self.picker_data().users, &self.picker_data().error) }
    fn authorization(&self) -> Authorization {
        match self {
            Self::Discovering(d) => d.discovery.authorization(),
            Self::Profiles(d) => d.choice,
            Self::Switching(d) => d.selection.authorization(),
            Self::Ready(d) => d.handoff.authorization(),
            Self::Error(d) => d.retry.authorization(),
            Self::Idle(_) | Self::Creating(_) | Self::Waiting(_) | Self::Deleted(_) | Self::Erasing(_) => Authorization::Stored,
        }
    }
    fn login_stage(&self) -> LoginStage {
        match self {
            Self::Creating(d) => d.login,
            Self::Waiting(d) => d.login,
            Self::Idle(_) | Self::Discovering(_) | Self::Profiles(_) | Self::Switching(_)
                | Self::Ready(_) | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => LoginStage::Dormant,
        }
    }
    pub(super) fn fresh_authority(&self) -> bool { self.authorization().is_fresh() }
    pub(super) fn handoff_pending(&self) -> bool { matches!(self, Self::Ready(d) if matches!(d.handoff, Handoff::Pending { .. })) }
    #[cfg(test)]
    pub(super) fn active_signin(&self) -> bool {
        match self {
            Self::Creating(d) => matches!(d.login, LoginStage::Authorizing { .. }),
            Self::Waiting(d) => matches!(d.login, LoginStage::Authorizing { .. }),
            Self::Discovering(d) => matches!(d.discovery, DiscoveryStage::Running { .. }),
            Self::Idle(_) | Self::Profiles(_) | Self::Switching(_) | Self::Ready(_)
                | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => false,
        }
    }
    pub(super) fn owns_work(&self, req: u32, pending: &Pending) -> bool {
        match pending.key.op {
            SessionOp::Login => match self {
                Self::Creating(d) => d.login.owns(req), Self::Waiting(d) => d.login.owns(req), Self::Discovering(d) => d.discovery.owns(req),
                Self::Idle(_) | Self::Profiles(_) | Self::Switching(_) | Self::Ready(_) | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => false,
            },
            SessionOp::Rediscover => matches!(self, Self::Discovering(d) if d.discovery.owns(req)),
            SessionOp::ProfileSwitch => match self {
                Self::Switching(d) => d.selection.owns(req),
                Self::Ready(d) => pending.phase == StreamPhase::ProfileSeated && d.roster.is_some_and(|stream| stream.req == req),
                Self::Idle(_) | Self::Creating(_) | Self::Waiting(_) | Self::Discovering(_) | Self::Profiles(_) | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => false,
            },
            SessionOp::DevBoundary => match self {
                Self::Idle(d) => matches!(d.boot, BootStage::ActivatingDev { req: owned } if owned == req),
                Self::Creating(d) => matches!(d.login, LoginStage::Preparing { boundary_req, .. } if boundary_req == req),
                Self::Waiting(_) | Self::Discovering(_) | Self::Profiles(_) | Self::Switching(_) | Self::Ready(_) | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => false,
            },
            SessionOp::Ready => match self {
                Self::Idle(d) => matches!(d.boot, BootStage::RestoringStored { req: owned } if owned == req),
                Self::Ready(d) => d.handoff.owns(req),
                Self::Creating(_) | Self::Waiting(_) | Self::Discovering(_) | Self::Profiles(_) | Self::Switching(_) | Self::Error(_) | Self::Deleted(_) | Self::Erasing(_) => false,
            },
            SessionOp::Picker => matches!(self, Self::Profiles(d) if d.setup.owns(req)),
            SessionOp::HomeRoster | SessionOp::ServerRoster | SessionOp::Endpoint(_) => !matches!(self, Self::Erasing(_)),
        }
    }
    pub(super) fn login_failed(self, message: String, revoke: bool) -> Self {
        let authorization = self.authorization();
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, mut signin: SignInHistory, authorization: Authorization, message: String, revoke: bool) -> Attempt {
            qr.trouble = false; picker.error = message;

            signin.retry = None; signin.plaintext = None; signin.account = None;
            if revoke { signin.authorization = HistoricalAuthorization::Stored; }
            Attempt::Error(ErrorData { retry: if revoke { RetryAuthority::Login } else { RetryAuthority::of(authorization) }, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, message, revoke),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, message, revoke),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, message, revoke),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, revoke),
            Self::Erasing(mut d) => {
                d.history.qr.trouble = false; d.history.picker.error = message;
                d.signin.retry = None; d.signin.plaintext = None; d.signin.account = None;
                if revoke { d.signin.authorization = HistoricalAuthorization::Stored; }
                d.prior_phase = Phase::Error;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn code_replacing(self) -> Self {
        let login = self.login_stage();
        fn destination(mut qr: QrHistory, picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, login: LoginStage) -> Attempt {
            qr.code.clear(); qr.png.clear(); qr.replaced = true; qr.trouble = false;
            Attempt::Creating(CreatingData { login, qr, picker_history: picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, login),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Erasing(mut d) => {
                d.history.qr.code.clear(); d.history.qr.png.clear(); d.history.qr.replaced = true; d.history.qr.trouble = false;
                d.prior_phase = Phase::Creating;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn code_ready(self, generation: u64, code: String, png: Vec<u8>) -> Self {
        let login = self.login_stage();
        fn destination(mut qr: QrHistory, picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, login: LoginStage, generation: u64, code: String, png: Vec<u8>) -> Attempt {
            qr.generation = generation; qr.code = code; qr.png = png;
            Attempt::Waiting(WaitingData { login, qr, picker_history: picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login, generation, code, png),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login, generation, code, png),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login, generation, code, png),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login, generation, code, png),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login, generation, code, png),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login, generation, code, png),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, login, generation, code, png),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login, generation, code, png),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login, generation, code, png),
            Self::Erasing(mut d) => {
                d.history.qr.generation = generation; d.history.qr.code = code; d.history.qr.png = png;
                d.prior_phase = Phase::Waiting;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn authorized(self) -> Self {
        let login = self.login_stage();
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, mut signin: SignInHistory, login: LoginStage) -> Attempt {
            qr.trouble = false; picker.error.clear();
            signin.authorization = HistoricalAuthorization::Fresh; signin.retry = None;
            Attempt::Discovering(DiscoveringData { discovery: match login { LoginStage::Authorizing { req } => DiscoveryStage::Running { req, authorization: Authorization::Fresh }, LoginStage::Dormant | LoginStage::Preparing { .. } => DiscoveryStage::Dormant(Authorization::Stored) }, history: HistoricalPresentation { qr, picker }, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, login),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, login),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, login),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, login),
            Self::Erasing(mut d) => {
                d.history.qr.trouble = false; d.history.picker.error.clear();
                d.signin.authorization = HistoricalAuthorization::Fresh; d.signin.retry = None;
                d.prior_phase = Phase::Discovering;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn restart_discovery(self, req: u32) -> Self {
        let authorization = self.authorization();
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, mut signin: SignInHistory, authorization: Authorization, req: u32) -> Attempt {
            qr.trouble = false; picker.error.clear();
             signin.plaintext = None; signin.account = None;
            Attempt::Discovering(DiscoveringData { discovery: DiscoveryStage::Running { req, authorization }, history: HistoricalPresentation { qr, picker }, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, req),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Erasing(mut d) => {
                d.history.qr.trouble = false; d.history.picker.error.clear();
                d.signin.plaintext = None; d.signin.account = None;
                d.prior_phase = Phase::Discovering;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn restart_login(self, req: u32) -> Self {
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, mut signin: SignInHistory, req: u32) -> Attempt {
            qr.code.clear(); qr.png.clear(); qr.generation = 0; qr.replaced = false; qr.trouble = false;
            picker.picker = Picker::Boot; picker.users.clear(); picker.error.clear(); picker.pin_denied = false; residue.0 = false;
            signin.authorization = HistoricalAuthorization::Stored;
            signin.plaintext = None; signin.account = None;
            Attempt::Creating(CreatingData { login: LoginStage::Authorizing { req }, qr, picker_history: picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, req),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, req),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, req),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, req),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, req),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, req),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, req),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, req),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, req),
            Self::Erasing(mut d) => {
                d.history.qr.code.clear(); d.history.qr.png.clear(); d.history.qr.generation = 0; d.history.qr.replaced = false; d.history.qr.trouble = false;
                d.history.picker.picker = Picker::Boot; d.history.picker.users.clear(); d.history.picker.error.clear(); d.history.picker.pin_denied = false; d.residue.0 = false;
                d.signin.authorization = HistoricalAuthorization::Stored;
                d.signin.plaintext = None; d.signin.account = None;
                d.prior_phase = Phase::Creating;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn resume_back(self) -> Self {
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, mut signin: SignInHistory) -> Attempt {
            qr.code.clear(); qr.png.clear(); qr.generation = 0; qr.replaced = false;
            picker.users.clear(); picker.error.clear(); picker.pin_denied = false; residue.0 = true;
            signin.authorization = HistoricalAuthorization::Stored;
            Attempt::Ready(ReadyData { handoff: Handoff::Pending { authorization: Authorization::Stored, commit: FinalCommitStage::Unrequested }, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.history.qr.code.clear(); d.history.qr.png.clear(); d.history.qr.generation = 0; d.history.qr.replaced = false;
                d.history.picker.users.clear(); d.history.picker.error.clear(); d.history.picker.pin_denied = false; d.residue.0 = true;
                d.signin.authorization = HistoricalAuthorization::Stored;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn resume_dev_back(self) -> Self {
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory) -> Attempt {
            picker.error.clear();
            Attempt::Ready(ReadyData { handoff: Handoff::Available, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.history.picker.error.clear();
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn begin_dev_account(self, boundary_req: u32, login_req: u32) -> Self {
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, boundary_req: u32, login_req: u32) -> Attempt {
            picker.error.clear(); residue.0 = false;
            Attempt::Creating(CreatingData { login: LoginStage::Preparing { boundary_req, login_req }, qr, picker_history: picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, boundary_req, login_req),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, boundary_req, login_req),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, boundary_req, login_req),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, boundary_req, login_req),
            Self::Erasing(mut d) => {
                d.history.picker.error.clear(); d.residue.0 = false;
                d.prior_phase = Phase::Creating;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn activate_dev(self) -> Self {
        fn destination(qr: QrHistory, picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory) -> Attempt {
            residue.0 = false;
            Attempt::Ready(ReadyData { handoff: Handoff::Available, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.residue.0 = false;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn ready_exhausted(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            residue.0 = false; picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.residue.0 = false; d.history.picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn switch_commit_refused(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            residue.0 = false; picker.error = plx_platform::i18n::msg::browse_auth_switch_failed().into();
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.residue.0 = false; d.history.picker.error = plx_platform::i18n::msg::browse_auth_switch_failed().into();
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn dev_commit_refused(self) -> Self {
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory) -> Attempt {
            residue.0 = false; picker.error = plx_platform::i18n::msg::browse_auth_authority_failed().into();
            Attempt::Error(ErrorData { retry: RetryAuthority::Login, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.residue.0 = false; d.history.picker.error = plx_platform::i18n::msg::browse_auth_authority_failed().into();
                d.prior_phase = Phase::Error;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn switch_admission_refused(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.history.picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn open_dev_picker(self) -> Self {
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory) -> Attempt {
            picker.users.clear(); picker.error = roster_refused().into();
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: Authorization::Stored, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.history.picker.users.clear(); d.history.picker.error = roster_refused().into();
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn open_picker(self, kind: Picker, fallback: Vec<UserTile>) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization, kind: Picker, fallback: Vec<UserTile>) -> Attempt {
            picker.error.clear(); if picker.users.is_empty() { picker.users = fallback; } picker.picker = kind;
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, kind, fallback),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, kind, fallback),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, kind, fallback),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, kind, fallback),
            Self::Erasing(mut d) => {
                d.history.picker.error.clear(); if d.history.picker.users.is_empty() { d.history.picker.users = fallback; } d.history.picker.picker = kind;
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn reuse_profile(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            picker.error.clear(); residue.0 = true;
            Attempt::Ready(ReadyData { handoff: Handoff::Pending { authorization, commit: FinalCommitStage::Unrequested }, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.history.picker.error.clear(); d.residue.0 = true;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn select_profile(self, req: u32) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization, req: u32) -> Attempt {
            picker.pin_denied = false;
            Attempt::Switching(SwitchingData { selection: SelectionStage::Selecting { req, authorization }, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, req),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Erasing(mut d) => {
                d.history.picker.pin_denied = false;
                d.prior_phase = Phase::Switching;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn switch_dropped(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            picker.pin_denied = false; picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.history.picker.pin_denied = false; d.history.picker.error = plx_platform::i18n::msg::browse_auth_switch_retry().into();
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn switch_failed(self, message: String, denied: bool) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization, message: String, denied: bool) -> Attempt {
            picker.error = message; picker.pin_denied = denied;
            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, denied),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, message, denied),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, message, denied),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, denied),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, denied),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, denied),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, message, denied),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, message, denied),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, message, denied),
            Self::Erasing(mut d) => {
                d.history.picker.error = message; d.history.picker.pin_denied = denied;
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn signin_profiles(self, users: Vec<UserTile>) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, residue: ImportedIntent, signin: SignInHistory, authorization: Authorization, users: Vec<UserTile>) -> Attempt {
            picker.picker = Picker::SignedIn; picker.users = users;

            Attempt::Profiles(ProfilesData { setup: PickerStage::Idle, choice: authorization, qr_history: qr, picker, residue, signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, users),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, users),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, users),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, users),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, users),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, users),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, users),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, users),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, users),
            Self::Erasing(mut d) => {
                d.history.picker.picker = Picker::SignedIn; d.history.picker.users = users;
                d.prior_phase = Phase::Profiles;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn signin_ready(self) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, authorization: Authorization) -> Attempt {
            residue.0 = true;

            Attempt::Ready(ReadyData { handoff: Handoff::Pending { authorization, commit: FinalCommitStage::Unrequested }, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization),
            Self::Erasing(mut d) => {
                d.residue.0 = true;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn profile_ready(self, req: u32) -> Self {
        let authorization = self.authorization();
        fn destination(qr: QrHistory, mut picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory, authorization: Authorization, req: u32) -> Attempt {
            residue.0 = true; picker.error.clear(); picker.pin_denied = false;
            Attempt::Ready(ReadyData { handoff: Handoff::Pending { authorization, commit: FinalCommitStage::Unrequested }, roster: Some(ProfileStream { req }), history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin, authorization, req),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin, authorization, req),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin, authorization, req),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin, authorization, req),
            Self::Erasing(mut d) => {
                d.residue.0 = true; d.history.picker.error.clear(); d.history.picker.pin_denied = false;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn stored_ready(self) -> Self {
        fn destination(qr: QrHistory, picker: PickerPresentation, mut residue: ImportedIntent, signin: SignInHistory) -> Attempt {
            residue.0 = false;
            Attempt::Ready(ReadyData { handoff: Handoff::Available, roster: None, history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(residue.0), signin })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.residue, d.signin),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, ImportedIntent(d.intent.0), d.signin),
            Self::Error(d) => destination(d.qr_history, d.picker, d.residue, d.signin),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.residue, d.signin),
            Self::Erasing(mut d) => {
                d.residue.0 = false;
                d.prior_phase = Phase::Ready;
                Self::Erasing(d)
            }
        }
    }
    pub(super) fn link_trouble(self, present: bool) -> Self {
        match self {
            Self::Idle(mut d) => { d.history.qr.trouble = present;  Self::Idle(d) },
            Self::Creating(mut d) => { d.qr.trouble = present;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.qr.trouble = present;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.history.qr.trouble = present;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.qr_history.trouble = present;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.qr_history.trouble = present;  Self::Switching(d) },
            Self::Ready(mut d) => { d.history.qr.trouble = present;  Self::Ready(d) },
            Self::Error(mut d) => { d.qr_history.trouble = present;  Self::Error(d) },
            Self::Deleted(mut d) => { d.history.qr.trouble = present;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.history.qr.trouble = present;  Self::Erasing(d) },
        }
    }
    pub(super) fn dismiss_pin(self) -> Self {
        match self {
            Self::Idle(mut d) => { d.history.picker.pin_denied = false;  Self::Idle(d) },
            Self::Creating(mut d) => { d.picker_history.pin_denied = false;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.picker_history.pin_denied = false;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.history.picker.pin_denied = false;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.picker.pin_denied = false;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.picker.pin_denied = false;  Self::Switching(d) },
            Self::Ready(mut d) => { d.history.picker.pin_denied = false;  Self::Ready(d) },
            Self::Error(mut d) => { d.picker.pin_denied = false;  Self::Error(d) },
            Self::Deleted(mut d) => { d.history.picker.pin_denied = false;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.history.picker.pin_denied = false;  Self::Erasing(d) },
        }
    }
    pub(super) fn roster_failed(self, message: String) -> Self {
        match self {
            Self::Idle(mut d) => { d.history.picker.error = message;  Self::Idle(d) },
            Self::Creating(mut d) => { d.picker_history.error = message;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.picker_history.error = message;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.history.picker.error = message;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.picker.error = message;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.picker.error = message;  Self::Switching(d) },
            Self::Ready(mut d) => { d.history.picker.error = message;  Self::Ready(d) },
            Self::Error(mut d) => { d.picker.error = message;  Self::Error(d) },
            Self::Deleted(mut d) => { d.history.picker.error = message;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.history.picker.error = message;  Self::Erasing(d) },
        }
    }
    pub(super) fn begin_signin(self, req: u32) -> Self {
        match self {
            Self::Idle(d) => {  Self::Idle(d) },
            Self::Creating(mut d) => { d.login = d.login.promote(req); Self::Creating(d) },
            Self::Waiting(d) => {  Self::Waiting(d) },
            Self::Discovering(d) => {  Self::Discovering(d) },
            Self::Profiles(d) => {  Self::Profiles(d) },
            Self::Switching(d) => {  Self::Switching(d) },
            Self::Ready(d) => {  Self::Ready(d) },
            Self::Error(d) => {  Self::Error(d) },
            Self::Deleted(d) => {  Self::Deleted(d) },
            Self::Erasing(d) => {  Self::Erasing(d) },
        }
    }
    pub(super) fn consume_authorization(self) -> Self {
        match self {
            Self::Idle(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Idle(d) },
            Self::Creating(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.discovery = d.discovery.routine(); Self::Discovering(d) },
            Self::Profiles(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.choice = Authorization::Stored; Self::Profiles(d) },
            Self::Switching(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.selection = d.selection.routine(); Self::Switching(d) },
            Self::Ready(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.handoff = d.handoff.routine(); Self::Ready(d) },
            Self::Error(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.retry = RetryAuthority::Login; Self::Error(d) },
            Self::Deleted(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Erasing(d) },
        }
    }
    pub(super) fn discovery_trouble(self, progress: super::super::DiscoveryRetryProgress) -> Self {
        match self {
            Self::Idle(mut d) => { d.signin.retry = Some(progress);  Self::Idle(d) },
            Self::Creating(mut d) => { d.signin.retry = Some(progress);  Self::Creating(d) },
            Self::Waiting(mut d) => { d.signin.retry = Some(progress);  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.signin.retry = Some(progress);  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.signin.retry = Some(progress);  Self::Profiles(d) },
            Self::Switching(mut d) => { d.signin.retry = Some(progress);  Self::Switching(d) },
            Self::Ready(mut d) => { d.signin.retry = Some(progress);  Self::Ready(d) },
            Self::Error(mut d) => { d.signin.retry = Some(progress);  Self::Error(d) },
            Self::Deleted(mut d) => { d.signin.retry = Some(progress);  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.signin.retry = Some(progress);  Self::Erasing(d) },
        }
    }
    pub(super) fn discovery_settled(self, run: super::super::DiscoveryRetryRun) -> Self {
        match self {
            Self::Idle(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Idle(d) },
            Self::Creating(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Creating(d) },
            Self::Waiting(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Waiting(d) },
            Self::Discovering(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Discovering(d) },
            Self::Profiles(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Profiles(d) },
            Self::Switching(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Switching(d) },
            Self::Ready(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Ready(d) },
            Self::Error(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Error(d) },
            Self::Deleted(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Deleted(d) },
            Self::Erasing(mut d) => { if d.signin.retry.is_some_and(|retry| retry.run == run) { d.signin.retry = None; }  Self::Erasing(d) },
        }
    }
    pub(super) fn discovery_failed(self, plaintext: Option<super::super::PlaintextVerdict>, account: Option<String>) -> Self {
        match self {
            Self::Idle(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Idle(d) },
            Self::Creating(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Switching(d) },
            Self::Ready(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Ready(d) },
            Self::Error(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Error(d) },
            Self::Deleted(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.signin.plaintext = plaintext; d.signin.account = account;  Self::Erasing(d) },
        }
    }
    pub(super) fn plaintext_declined(self, choice: plx_plex::plex::session::PlaintextChoice) -> Self {
        match self {
            Self::Idle(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.history.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Idle(d) },
            Self::Creating(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.picker_history.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Creating(d) },
            Self::Waiting(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.picker_history.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Waiting(d) },
            Self::Discovering(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.history.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Discovering(d) },
            Self::Profiles(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Profiles(d) },
            Self::Switching(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Switching(d) },
            Self::Ready(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.history.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Ready(d) },
            Self::Error(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Error(d) },
            Self::Deleted(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.history.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Deleted(d) },
            Self::Erasing(mut d) => { if let Some(verdict) = d.signin.plaintext.as_mut() { verdict.choice = choice; d.history.picker.error = super::super::insecure_only_copy(Some(verdict)).into_owned(); }  Self::Erasing(d) },
        }
    }
    pub(super) fn roster_arrived(self, users: Vec<UserTile>) -> Self {
        match self {
            Self::Idle(mut d) => { d.history.picker.users = users;  Self::Idle(d) },
            Self::Creating(mut d) => { d.picker_history.users = users;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.picker_history.users = users;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.history.picker.users = users;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.picker.users = users;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.picker.users = users;  Self::Switching(d) },
            Self::Ready(mut d) => { d.history.picker.users = users;  Self::Ready(d) },
            Self::Error(mut d) => { d.picker.users = users;  Self::Error(d) },
            Self::Deleted(mut d) => { d.history.picker.users = users;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.history.picker.users = users;  Self::Erasing(d) },
        }
    }
    pub(super) fn ready_committed(self) -> Self {
        match self {
            Self::Idle(mut d) => { d.residue.0 = false;  Self::Idle(d) },
            Self::Creating(mut d) => { d.residue.0 = false;  Self::Creating(d) },
            Self::Waiting(mut d) => { d.residue.0 = false;  Self::Waiting(d) },
            Self::Discovering(mut d) => { d.residue.0 = false;  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.residue.0 = false;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.residue.0 = false;  Self::Switching(d) },
            Self::Ready(mut d) => { d.intent.0 = false; d.handoff = Handoff::Available; Self::Ready(d) },
            Self::Error(mut d) => { d.residue.0 = false;  Self::Error(d) },
            Self::Deleted(mut d) => { d.residue.0 = false;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.residue.0 = false;  Self::Erasing(d) },
        }
    }
    pub(super) fn erase_authority(self) -> Self {
        match self {
            Self::Idle(mut d) => { d.boot = BootStage::Dormant; d.signin.authorization = HistoricalAuthorization::Stored;  Self::Idle(d) },
            Self::Creating(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.login = LoginStage::Dormant; Self::Creating(d) },
            Self::Waiting(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.login = LoginStage::Dormant; Self::Waiting(d) },
            Self::Discovering(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.discovery = DiscoveryStage::Dormant(Authorization::Stored); Self::Discovering(d) },
            Self::Profiles(mut d) => { d.setup = PickerStage::Idle; d.signin.authorization = HistoricalAuthorization::Stored; d.choice = Authorization::Stored; Self::Profiles(d) },
            Self::Switching(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.selection = SelectionStage::Dormant(Authorization::Stored); Self::Switching(d) },
            Self::Ready(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.handoff = Handoff::Available; d.roster = None; Self::Ready(d) },
            Self::Error(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored; d.retry = RetryAuthority::Login; Self::Error(d) },
            Self::Deleted(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.signin.authorization = HistoricalAuthorization::Stored;  Self::Erasing(d) },
        }
    }
    pub(super) fn begin_dev_activation(self, req: u32) -> Self {
        match self {
            Self::Idle(mut d) => { d.boot = BootStage::ActivatingDev { req }; Self::Idle(d) },
            Self::Creating(d) => {  Self::Creating(d) },
            Self::Waiting(d) => {  Self::Waiting(d) },
            Self::Discovering(d) => {  Self::Discovering(d) },
            Self::Profiles(d) => {  Self::Profiles(d) },
            Self::Switching(d) => {  Self::Switching(d) },
            Self::Ready(d) => {  Self::Ready(d) },
            Self::Error(d) => {  Self::Error(d) },
            Self::Deleted(d) => {  Self::Deleted(d) },
            Self::Erasing(d) => {  Self::Erasing(d) },
        }
    }
    pub(super) fn begin_stored_restore(self, req: u32) -> Self {
        match self {
            Self::Idle(mut d) => { d.boot = BootStage::RestoringStored { req }; Self::Idle(d) },
            Self::Creating(d) => {  Self::Creating(d) },
            Self::Waiting(d) => {  Self::Waiting(d) },
            Self::Discovering(d) => {  Self::Discovering(d) },
            Self::Profiles(d) => {  Self::Profiles(d) },
            Self::Switching(d) => {  Self::Switching(d) },
            Self::Ready(d) => {  Self::Ready(d) },
            Self::Error(d) => {  Self::Error(d) },
            Self::Deleted(d) => {  Self::Deleted(d) },
            Self::Erasing(d) => {  Self::Erasing(d) },
        }
    }
    pub(super) fn install_picker(self, req: u32) -> Self {
        match self {
            Self::Idle(d) => {  Self::Idle(d) },
            Self::Creating(d) => {  Self::Creating(d) },
            Self::Waiting(d) => {  Self::Waiting(d) },
            Self::Discovering(d) => {  Self::Discovering(d) },
            Self::Profiles(mut d) => { d.setup = PickerStage::Installing { req }; Self::Profiles(d) },
            Self::Switching(d) => {  Self::Switching(d) },
            Self::Ready(d) => {  Self::Ready(d) },
            Self::Error(d) => {  Self::Error(d) },
            Self::Deleted(d) => {  Self::Deleted(d) },
            Self::Erasing(d) => {  Self::Erasing(d) },
        }
    }
    pub(super) fn begin_ready_commit(self, req: u32) -> Self {
        match self.consume_authorization() {
            Self::Ready(mut d) => { if let Handoff::Pending { authorization, .. } = d.handoff { d.handoff = Handoff::Pending { authorization, commit: FinalCommitStage::Committing { req } }; } Self::Ready(d) },
            Self::Idle(d) => Self::Idle(d),
            Self::Creating(d) => Self::Creating(d),
            Self::Waiting(d) => Self::Waiting(d),
            Self::Discovering(d) => Self::Discovering(d),
            Self::Profiles(d) => Self::Profiles(d),
            Self::Switching(d) => Self::Switching(d),
            Self::Error(d) => Self::Error(d),
            Self::Deleted(d) => Self::Deleted(d),
            Self::Erasing(d) => Self::Erasing(d),
        }
    }
    pub(super) fn work_retired(self, req: u32) -> Self {
        match self {
            Self::Idle(mut d) => { if d.boot.owns(req) { d.boot = BootStage::Dormant; } Self::Idle(d) },
            Self::Creating(mut d) => { if d.login.owns(req) || matches!(d.login, LoginStage::Preparing { boundary_req, login_req } if boundary_req == req || login_req == req) { d.login = LoginStage::Dormant; } Self::Creating(d) },
            Self::Waiting(mut d) => { if d.login.owns(req) { d.login = LoginStage::Dormant; } Self::Waiting(d) },
            Self::Discovering(mut d) => { if d.discovery.owns(req) { d.discovery = d.discovery.cancelled(); } Self::Discovering(d) },
            Self::Profiles(mut d) => { if d.setup.owns(req) { d.setup = PickerStage::Idle; } Self::Profiles(d) },
            Self::Switching(mut d) => { if d.selection.owns(req) { d.selection = d.selection.cancelled(); } Self::Switching(d) },
            Self::Ready(mut d) => { if d.roster.is_some_and(|stream| stream.req == req) { d.roster = None; } if d.handoff.owns(req) { d.handoff = d.handoff.cancelled(); } Self::Ready(d) },
            Self::Error(d) => Self::Error(d),
            Self::Deleted(d) => Self::Deleted(d),
            Self::Erasing(d) => Self::Erasing(d),
        }
    }

    pub(super) fn epoch_advanced(self) -> Self {
        match self {
            Self::Idle(mut d) => { d.boot = BootStage::Dormant; d.signin.retry = None;  Self::Idle(d) },
            Self::Creating(mut d) => { d.signin.retry = None; d.login = LoginStage::Dormant; Self::Creating(d) },
            Self::Waiting(mut d) => { d.signin.retry = None; d.login = LoginStage::Dormant; Self::Waiting(d) },
            Self::Discovering(mut d) => { d.signin.retry = None; d.discovery = d.discovery.cancelled(); Self::Discovering(d) },
            Self::Profiles(mut d) => { d.setup = PickerStage::Idle; d.signin.retry = None;  Self::Profiles(d) },
            Self::Switching(mut d) => { d.signin.retry = None; d.selection = d.selection.cancelled(); Self::Switching(d) },
            Self::Ready(mut d) => { d.signin.retry = None; d.roster = None; d.handoff = d.handoff.cancelled(); Self::Ready(d) },
            Self::Error(mut d) => { d.signin.retry = None;  Self::Error(d) },
            Self::Deleted(mut d) => { d.signin.retry = None;  Self::Deleted(d) },
            Self::Erasing(mut d) => { d.signin.retry = None;  Self::Erasing(d) },
        }
    }
    pub(super) fn begin_erase(self, epoch: u64, sign_in: bool) -> Self {
        fn destination(mut qr: QrHistory, mut picker: PickerPresentation, signin: SignInHistory, phase: Phase, epoch: u64, sign_in: bool) -> Attempt {
            qr.code.clear(); qr.png.clear(); qr.generation = 0; qr.replaced = false;
            picker.picker = Picker::Boot; picker.users.clear(); picker.error.clear(); picker.pin_denied = false;
            Attempt::Erasing(ErasingData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(false), signin, prior_phase: phase, pending_erase: (epoch, sign_in) })
        }
        match self {
            Self::Idle(d) => destination(d.history.qr, d.history.picker, d.signin, Phase::Idle, epoch, sign_in),
            Self::Creating(d) => destination(d.qr, d.picker_history, d.signin, Phase::Creating, epoch, sign_in),
            Self::Waiting(d) => destination(d.qr, d.picker_history, d.signin, Phase::Waiting, epoch, sign_in),
            Self::Discovering(d) => destination(d.history.qr, d.history.picker, d.signin, Phase::Discovering, epoch, sign_in),
            Self::Profiles(d) => destination(d.qr_history, d.picker, d.signin, Phase::Profiles, epoch, sign_in),
            Self::Switching(d) => destination(d.qr_history, d.picker, d.signin, Phase::Switching, epoch, sign_in),
            Self::Ready(d) => destination(d.history.qr, d.history.picker, d.signin, Phase::Ready, epoch, sign_in),
            Self::Error(d) => destination(d.qr_history, d.picker, d.signin, Phase::Error, epoch, sign_in),
            Self::Deleted(d) => destination(d.history.qr, d.history.picker, d.signin, Phase::Deleted, epoch, sign_in),
            Self::Erasing(d) => destination(d.history.qr, d.history.picker, d.signin, d.prior_phase, epoch, sign_in),
        }
    }
    pub(super) fn finish_erase(self) -> Self {
        match self {
            Self::Idle(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.history.qr, picker: d.history.picker }, residue: d.residue, signin: d.signin }),
            Self::Creating(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.qr, picker: d.picker_history }, residue: d.residue, signin: d.signin }),
            Self::Waiting(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.qr, picker: d.picker_history }, residue: d.residue, signin: d.signin }),
            Self::Discovering(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.history.qr, picker: d.history.picker }, residue: d.residue, signin: d.signin }),
            Self::Profiles(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.qr_history, picker: d.picker }, residue: d.residue, signin: d.signin }),
            Self::Switching(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.qr_history, picker: d.picker }, residue: d.residue, signin: d.signin }),
            Self::Ready(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.history.qr, picker: d.history.picker }, residue: ImportedIntent(d.intent.0), signin: d.signin }),
            Self::Error(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.qr_history, picker: d.picker }, residue: d.residue, signin: d.signin }),
            Self::Deleted(d) => Self::Deleted(DeletedData { history: HistoricalPresentation { qr: d.history.qr, picker: d.history.picker }, residue: d.residue, signin: d.signin }),
            Self::Erasing(d) => Self::Deleted(DeletedData { history: d.history, residue: d.residue, signin: d.signin }),
        }
    }
}
impl Attempt {
    fn presentation(&self) -> PresentationRef<'_> {
        match self {
            Self::Idle(data) => PresentationRef { phase: Phase::Idle, qr: &data.history.qr, picker: &data.history.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Creating(data) => PresentationRef { phase: Phase::Creating, qr: &data.qr, picker: &data.picker_history, apply_pending: data.residue.0, pending_erase: None },
            Self::Waiting(data) => PresentationRef { phase: Phase::Waiting, qr: &data.qr, picker: &data.picker_history, apply_pending: data.residue.0, pending_erase: None },
            Self::Discovering(data) => PresentationRef { phase: Phase::Discovering, qr: &data.history.qr, picker: &data.history.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Profiles(data) => PresentationRef { phase: Phase::Profiles, qr: &data.qr_history, picker: &data.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Switching(data) => PresentationRef { phase: Phase::Switching, qr: &data.qr_history, picker: &data.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Ready(data) => PresentationRef { phase: Phase::Ready, qr: &data.history.qr, picker: &data.history.picker, apply_pending: data.intent.0, pending_erase: None },
            Self::Error(data) => PresentationRef { phase: Phase::Error, qr: &data.qr_history, picker: &data.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Deleted(data) => PresentationRef { phase: Phase::Deleted, qr: &data.history.qr, picker: &data.history.picker, apply_pending: data.residue.0, pending_erase: None },
            Self::Erasing(data) => PresentationRef { phase: data.prior_phase, qr: &data.history.qr, picker: &data.history.picker, apply_pending: data.residue.0, pending_erase: Some(data.pending_erase) },
        }
    }
}

impl<'a> LegacyV5Projection<'a> {
    pub(super) fn from_partition(partition: PartitionView<'a>) -> Self {
        let c = partition.common;
        let attempt = partition.attempt;
        let p = attempt.presentation();
        Self {
            disk_identity: &c.disk_identity,
            authority: &c.authority,
            phase: p.phase,
            picker: p.picker.picker,
            persisted: &c.persisted,
            committed_credentials: &c.committed_credentials,
            pin_code: &p.qr.code,
            qr_png: &p.qr.png,
            users: &p.picker.users,
            error: &p.picker.error,
            pin_denied: p.picker.pin_denied,
            authorized_in_flow: attempt.projected_authorization(),
            signin_active: c.signin_notice.owed(),
            apply_pending: p.apply_pending,
            code_replaced: p.qr.replaced,
            qr_gen: p.qr.generation,
            next_qr: c.next_qr,
            epoch: c.epoch,
            next_req: c.next_req,
            delete_leftovers: c.delete_leftovers,
            profile_scope: c.profile_scope,
            active_profile: &c.active_profile,
            pending: &c.pending,
            persistence_purpose: c.persistence_purpose,
            commit_phase: &c.commit_phase,
            persistence_warning: &c.persistence_warning,
            held_handoff: &c.held_handoff,
            persistence_warning_answered: c.persistence_warning_answered,
            pending_commit: &c.pending_commit,
            pending_erase: p.pending_erase,
            pump_pending: c.pump_pending,
            inbox: &c.inbox,
            incident: &c.incident,
            incidents_seen: &c.incidents_seen,
            next_incident: c.next_incident,
            link_trouble: p.qr.trouble,
            switch_refused_for: &c.switch_refused_for,
            plaintext: &attempt.retained_signin().plaintext,
            discovery_retry: attempt.discovery_retry(),
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;


    fn projected_fields(p: &LegacyV5Projection<'_>) -> Vec<serde_json::Value> {
        vec![
            serde_json::to_value(&p.disk_identity).unwrap(),
            serde_json::to_value(&p.authority).unwrap(),
            serde_json::to_value(&p.phase).unwrap(),
            serde_json::to_value(&p.picker).unwrap(),
            serde_json::to_value(&p.persisted).unwrap(),
            serde_json::to_value(&p.committed_credentials).unwrap(),
            serde_json::to_value(&p.pin_code).unwrap(),
            serde_json::to_value(&p.qr_png).unwrap(),
            serde_json::to_value(&p.users).unwrap(),
            serde_json::to_value(&p.error).unwrap(),
            serde_json::to_value(&p.pin_denied).unwrap(),
            serde_json::to_value(&p.authorized_in_flow).unwrap(),
            serde_json::to_value(&p.signin_active).unwrap(),
            serde_json::to_value(&p.apply_pending).unwrap(),
            serde_json::to_value(&p.code_replaced).unwrap(),
            serde_json::to_value(&p.qr_gen).unwrap(),
            serde_json::to_value(&p.next_qr).unwrap(),
            serde_json::to_value(&p.epoch).unwrap(),
            serde_json::to_value(&p.next_req).unwrap(),
            serde_json::to_value(&p.delete_leftovers).unwrap(),
            serde_json::to_value(&p.profile_scope).unwrap(),
            serde_json::to_value(&p.active_profile).unwrap(),
            serde_json::to_value(&p.pending).unwrap(),
            serde_json::to_value(&p.persistence_purpose).unwrap(),
            serde_json::to_value(&p.commit_phase).unwrap(),
            serde_json::to_value(&p.persistence_warning).unwrap(),
            serde_json::to_value(&p.held_handoff).unwrap(),
            serde_json::to_value(&p.persistence_warning_answered).unwrap(),
            serde_json::to_value(&p.pending_commit).unwrap(),
            serde_json::to_value(&p.pending_erase).unwrap(),
            serde_json::to_value(&p.pump_pending).unwrap(),
            serde_json::to_value(&p.inbox).unwrap(),
            serde_json::to_value(&p.incident).unwrap(),
            serde_json::to_value(&p.incidents_seen).unwrap(),
            serde_json::to_value(&p.next_incident).unwrap(),
            serde_json::to_value(&p.link_trouble).unwrap(),
            serde_json::to_value(&p.switch_refused_for).unwrap(),
            serde_json::to_value(&p.plaintext).unwrap(),
            serde_json::to_value(&p.discovery_retry).unwrap(),
        ]
    }

    // Imported-residue fixtures intentionally include states no addressed runtime event admits.
    // Construct their owned successor while exercising the production exit and entry plans.
    fn fixture_after_attempt(init: SessionInit, event: AttemptEvent) -> (SessionMachine, Vec<SessionFx>) {
        let mut owner = SessionMachine::from_init(init);
        let exit = ExitFx::attempt(owner.view(), &event);
        let entry = EntryFx::attempt(owner.view(), &event);
        if let Some(notice) = signin_notice(&event) { owner.common.signin_notice = notice; }
        let mut effects = Vec::new();
        exit.emit(&mut |fx| effects.push(fx));
        owner.state = next_attempt(owner.state, event);
        entry.emit(&mut |fx| effects.push(fx));
        assert!(owner.transition(&SessionEvent::RebuildPublication, 0, &mut effects));
        (owner, effects)
    }

    fn fixture(phase: Phase, erase: Option<(u64, bool)>) -> SessionInit {
        let mut s = SessionInit::captured(PersistedSession { client_id: "representation-client".into(), ..Default::default() });
        s.phase = phase;
        s.pending_erase = erase;
        s.picker = Picker::ChangeProfile;
        s.pin_code = "retained-code".into();
        s.qr_png = vec![8, 3, 7];
        s.qr_gen = 91;
        s.next_qr = 127;
        s.code_replaced = true;
        s.link_trouble = true;
        s.users = vec![UserTile { uuid: "retained-user".into(), ..Default::default() }];
        s.error = "retained-error".into();
        s.pin_denied = true;
        s.apply_pending = true;
        s.authorized_in_flow = true;
        s.signin_active = true;
        s.signin_account = Some("private-account".into());
        s
    }

    #[test]
    fn attempt_characterization_login_failure_preserves_imported_presentation() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for erase in [None, Some((77, true))] {
                let mut init = fixture(phase, erase);
                let mut owner = SessionMachine::from_init(init.clone());
                let mut effects = Vec::new();
                owner.fail_login("synthetic failure", None, &mut |effect| effects.push(effect));
                assert!(matches!(effects.as_slice(), [SessionFx::Coordinator(
                    CoordinatorAction::SignInFailed { phase: actual })] if *actual == phase));
                init.phase = Phase::Error;
                init.error = "synthetic failure".into();
                init.link_trouble = false;
                init.signin_active = false;
                init.signin_account = None;
                init.incident = None;
                assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), serde_json::to_value(&init).unwrap());
                assert_eq!(owner.snapshot_init().signin_account, None);
            }
        }
    }

    #[test]
    fn attempt_representation_all_phases_and_erase_preserve_every_field() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for erase in [None, Some((0, false)), Some((77, true))] {
                let init = fixture(phase, erase);
                let expected = serde_json::to_value(&init).unwrap();
                let expected_fields = projected_fields(&LegacyV5Projection::from_init(&init));
                let hash = init.hash();
                let code_ptr = init.pin_code.as_ptr();
                let png_ptr = init.qr_png.as_ptr();
                let users_ptr = init.users.as_ptr();
                let error_ptr = init.error.as_ptr();
                let partition = Partition::from_init(init);
                let projection = LegacyV5Projection::from_partition(partition.view());
                assert_eq!(projected_fields(&projection), expected_fields);
                assert_eq!(projection.phase, phase);
                assert_eq!(projection.pending_erase, erase);
                assert_eq!(projection.pin_code.as_ptr(), code_ptr);
                assert_eq!(projection.qr_png.as_ptr(), png_ptr);
                assert_eq!(projection.users.as_ptr(), users_ptr);
                assert_eq!(projection.error.as_ptr(), error_ptr);
                let mut canon = Canon::new();
                projection.write(&mut canon);
                assert_eq!(canon.finish(), hash);
                let snapshot = partition.snapshot_init();
                assert_eq!(serde_json::to_value(&snapshot).unwrap(), expected);
                assert_eq!(snapshot.signin_account.as_deref(), Some("private-account"));
                assert!(expected.get("signin_account").is_none());
                assert_eq!(snapshot.hash(), hash);
                assert!(partition.common.pending.is_empty());
                if erase.is_some() { assert!(matches!(partition.attempt, Attempt::Erasing(_))); }
            }
        }
    }

    #[test]
    fn attempt_runtime_moves_retain_presentation_and_erase_projection() {
        let init = fixture(Phase::Waiting, Some((77, true)));
        let code_ptr = init.pin_code.as_ptr();
        let users_ptr = init.users.as_ptr();
        let mut runtime = Partition::from_init(init);
        let attempt = std::mem::replace(&mut runtime.attempt, Attempt::placeholder());
        runtime.attempt = attempt.authorized();
        assert_eq!(runtime.phase(), Phase::Discovering);
        assert_eq!(runtime.qr().code.as_ptr(), code_ptr);
        assert_eq!(runtime.picker_data().users.as_ptr(), users_ptr);
        assert_eq!(runtime.pending_erase(), Some((77, true)));
        let attempt = std::mem::replace(&mut runtime.attempt, Attempt::placeholder());
        runtime.attempt = attempt.login_failed("updated".into(), false);
        let attempt = std::mem::replace(&mut runtime.attempt, Attempt::placeholder());
        runtime.attempt = attempt.ready_committed();
        let projected = LegacyV5Projection::from_partition(runtime.view());
        assert_eq!(projected.phase, Phase::Error);
        assert_eq!(projected.pending_erase, Some((77, true)));
        assert_eq!(projected.pin_code.as_ptr(), code_ptr);
        assert_eq!(projected.users.as_ptr(), users_ptr);
        assert_eq!(projected.error.as_str(), "updated");
        assert!(!projected.apply_pending && !projected.link_trouble);
        let mut canon = Canon::new();
        projected.write(&mut canon);
        assert_eq!(canon.finish(), runtime.snapshot_init().hash());
        let mut init = runtime.snapshot_init();
        init.pending_erase = None;
        // Move the code into the imported DTO so the pointer assertion still tests retention.
        init.pin_code = match runtime.attempt {
            Attempt::Erasing(data) => data.history.qr.code,
            Attempt::Idle(_) | Attempt::Creating(_) | Attempt::Waiting(_) | Attempt::Discovering(_)
            | Attempt::Profiles(_) | Attempt::Switching(_) | Attempt::Ready(_) | Attempt::Error(_)
            | Attempt::Deleted(_) => panic!("fixture retains its erase"),
        };
        runtime = Partition::from_init(init);
        assert_eq!(runtime.phase(), Phase::Error);
        assert!(runtime.pending_erase().is_none());
        assert_eq!(runtime.qr().code.as_ptr(), code_ptr);
    }

    #[test]
    fn attempt_representation_pin_dismissal_replaces_only_the_verdict_in_every_imported_phase() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for erase in [None, Some((77, true))] {
                let init = fixture(phase, erase);
                let code = init.pin_code.as_ptr();
                let users = init.users.as_ptr();
                let mut expected = init.clone();
                expected.pin_denied = false;
                let mut owner = SessionMachine::from_init(init);
                let effects = super::super::scenarios::step(&mut owner,
                    SessionEvent::Command(Command::DismissPinError));
                assert!(effects.is_empty());
                assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(),
                    serde_json::to_value(&expected).unwrap());
                assert_eq!(owner.snapshot_init().signin_account, expected.signin_account);
                assert_eq!(owner.state.qr().code.as_ptr(), code);
                assert_eq!(owner.state.picker_data().users.as_ptr(), users);
                let publication = owner.publication();
                assert!(super::super::scenarios::step(&mut owner,
                    SessionEvent::Command(Command::DismissPinError)).is_empty());
                assert!(Arc::ptr_eq(&publication, &owner.publication()));
            }
        }
    }

    #[test]
    fn attempt_runtime_qr_flow_moves_only_the_accepted_presentation_patch() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for erase in [None, Some((77, true))] {
                for event in 0..4 {
                    let init = fixture(phase, erase);
                    let users = init.users.as_ptr();
                    let mut expected = init.clone();
                    let flow = match event {
                        0 => {
                            expected.phase = Phase::Creating;
                            expected.pin_code.clear();
                            expected.qr_png.clear();
                            expected.code_replaced = true;
                            expected.link_trouble = false;
                            AttemptEvent::CodeReplacing
                        }
                        1 => {
                            expected.phase = Phase::Waiting;
                            expected.qr_gen = 128;
                            expected.pin_code = "new-code".into();
                            expected.qr_png = vec![4, 5];
                            AttemptEvent::CodeReady { generation: 128, code: "new-code".into(), png: vec![4, 5] }
                        }
                        2 => {
                            expected.phase = Phase::Discovering;
                            expected.link_trouble = false;
                            expected.error.clear();
                            AttemptEvent::Authorized
                        }
                        _ => {
                            expected.link_trouble = false;
                            AttemptEvent::LinkTrouble { present: false }
                        }
                    };
                    let (state, _effects) = fixture_after_attempt(init, flow);
                    assert_eq!(serde_json::to_value(state.snapshot_init()).unwrap(),
                        serde_json::to_value(&expected).unwrap());
                    assert_eq!(state.snapshot_init().signin_account, expected.signin_account);
                    assert_eq!(state.state.picker_data().users.as_ptr(), users);
                }
            }
        }
    }

    #[test]
    fn attempt_runtime_named_flow_patches_preserve_unwritten_imported_residue() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for event in 0..8 {
                let init = fixture(phase, (event == 4).then_some((77, true)));
                let users = init.users.as_ptr();
                let mut expected = init.clone();
                let flow = match event {
                    0 => {
                        expected.phase = Phase::Discovering;
                        expected.link_trouble = false;
                        expected.error.clear(); expected.signin_account = None;
                        AttemptEvent::RestartDiscovery { req: 29 }
                    }
                    1 => {
                        expected.phase = Phase::Creating;
                        expected.picker = Picker::Boot;
                        expected.pin_code.clear(); expected.qr_png.clear(); expected.qr_gen = 0;
                        expected.code_replaced = false; expected.link_trouble = false;
                        expected.users.clear(); expected.error.clear(); expected.pin_denied = false;
                        expected.apply_pending = false; expected.authorized_in_flow = false; expected.signin_account = None;
                        AttemptEvent::RestartLogin { req: 29 }
                    }
                    2 => {
                        expected.phase = Phase::Ready;
                        expected.pin_code.clear(); expected.qr_png.clear(); expected.qr_gen = 0;
                        expected.code_replaced = false;
                        expected.users.clear(); expected.error.clear(); expected.pin_denied = false;
                        expected.apply_pending = true; expected.authorized_in_flow = false; expected.signin_active = false;
                        AttemptEvent::ResumeBack
                    }
                    3 => {
                        expected.pending_erase = Some((78, false)); expected.picker = Picker::Boot;
                        expected.pin_code.clear(); expected.qr_png.clear(); expected.qr_gen = 0;
                        expected.code_replaced = false;
                        expected.users.clear(); expected.error.clear(); expected.pin_denied = false;
                        expected.apply_pending = false;
                        AttemptEvent::BeginErase { epoch: 78, sign_in: false }
                    }
                    4 => { expected.pending_erase = None; expected.phase = Phase::Deleted; AttemptEvent::FinishErase }
                    5 => { expected.phase = Phase::Creating; expected.error.clear(); expected.apply_pending = false; AttemptEvent::BeginDevAccount { boundary_req: 28, login_req: 29 } }
                    6 => { expected.phase = Phase::Ready; expected.apply_pending = false; AttemptEvent::ActivateDev }
                    _ => { expected.phase = Phase::Ready; expected.error.clear(); AttemptEvent::ResumeDevBack }
                };
                let (state, _effects) = fixture_after_attempt(init, flow);
                let actual = state.snapshot_init();
                assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap());
                assert_eq!(actual.signin_account, expected.signin_account);
                if matches!(event, 0 | 4 | 5 | 6 | 7) {
                    assert_eq!(state.state.picker_data().users.as_ptr(), users);
                }
            }
        }
    }

    #[test]
    fn attempt_representation_sparse_imports_do_not_invent_code_or_handoff() {
        for phase in [Phase::Waiting, Phase::Discovering, Phase::Switching, Phase::Ready] {
            let mut init = SessionInit::captured(PersistedSession::default());
            init.phase = phase;
            init.qr_gen = 42;
            init.code_replaced = true;
            init.link_trouble = true;
            let expected = serde_json::to_value(&init).unwrap();
            let hash = init.hash();
            let partition = Partition::from_init(init);
            let p = LegacyV5Projection::from_partition(partition.view());
            assert!(p.pin_code.is_empty() && p.qr_png.is_empty());
            assert_eq!(p.qr_gen, 42);
            assert!(p.code_replaced && p.link_trouble);
            assert!(!p.apply_pending);
            assert!(partition.common.pending.is_empty());
            let snapshot = partition.snapshot_init();
            assert_eq!(serde_json::to_value(snapshot).unwrap(), expected);
            let mut canon = Canon::new();
            p.write(&mut canon);
            assert_eq!(canon.finish(), hash);
        }
    }

    #[test]
    fn attempt_representation_private_readout_is_common_not_history_or_hash() {
        let mut init = fixture(Phase::Discovering, None);
        let before = init.hash();
        init.signin_account = Some("another-private-account".into());
        assert_eq!(init.hash(), before);
        let partition = Partition::from_init(init);
        assert_eq!(partition.attempt.signin_account(), Some("another-private-account"));
        let p = partition.attempt.presentation();
        assert_eq!(p.qr.code, "retained-code");
        assert_eq!(p.picker.users[0].uuid, "retained-user");
        assert_eq!(partition.snapshot_init().signin_account.as_deref(), Some("another-private-account"));
    }
    #[test]
    fn attempt_runtime_all_owned_destinations_preserve_arbitrary_imported_residue() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Profiles, Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
            for erase in [None, Some((77, true))] {
                for case in 0..16 {
                    let init = fixture(phase, erase);
                    let code = init.pin_code.as_ptr();
                    let mut expected = init.clone();
                    let users = vec![UserTile { uuid: "new-roster".into(), ..Default::default() }];
                    let event = match case {
                        0 => { expected.phase = Phase::Profiles; expected.apply_pending = false;
                            expected.error = plx_platform::i18n::msg::browse_auth_switch_retry().into(); AttemptEvent::ReadyExhausted }
                        1 => { expected.phase = Phase::Profiles; expected.apply_pending = false;
                            expected.error = plx_platform::i18n::msg::browse_auth_switch_failed().into(); AttemptEvent::SwitchCommitRefused }
                        2 => { expected.phase = Phase::Error; expected.apply_pending = false;
                            expected.error = plx_platform::i18n::msg::browse_auth_authority_failed().into(); AttemptEvent::DevCommitRefused }
                        3 => { expected.phase = Phase::Profiles;
                            expected.error = plx_platform::i18n::msg::browse_auth_switch_retry().into(); AttemptEvent::SwitchAdmissionRefused }
                        4 => { expected.phase = Phase::Profiles; expected.users.clear();
                            expected.error = roster_refused().into(); AttemptEvent::OpenDevPicker }
                        5 => { expected.phase = Phase::Profiles; expected.error.clear(); expected.picker = Picker::Boot;
                            AttemptEvent::OpenPicker { kind: Picker::Boot, fallback: users } }
                        6 => { expected.phase = Phase::Ready; expected.error.clear(); expected.apply_pending = true;
                            AttemptEvent::ReuseProfile }
                        7 => { expected.phase = Phase::Switching; expected.pin_denied = false; AttemptEvent::SelectProfile { req: 29 } }
                        8 => { expected.phase = Phase::Profiles; expected.pin_denied = false;
                            expected.error = plx_platform::i18n::msg::browse_auth_switch_retry().into(); AttemptEvent::SwitchDropped }
                        9 => { expected.phase = Phase::Profiles; expected.error = "profile failure".into(); expected.pin_denied = false;
                            AttemptEvent::SwitchFailed { message: "profile failure".into(), denied: false } }
                        10 => { expected.phase = Phase::Profiles; expected.picker = Picker::SignedIn; expected.users = users.clone(); expected.signin_active = false;
                            AttemptEvent::SigninProfiles { users } }
                        11 => { expected.phase = Phase::Ready; expected.apply_pending = true; expected.signin_active = false; AttemptEvent::SigninReady }
                        12 => { expected.phase = Phase::Ready; expected.apply_pending = true; expected.error.clear(); expected.pin_denied = false;
                            AttemptEvent::ProfileReady { req: 29 } }
                        13 => { expected.phase = Phase::Ready; expected.apply_pending = false; AttemptEvent::StoredReady }
                        14 => { expected.users = users.clone(); AttemptEvent::RosterArrived { users } }
                        _ => { expected.apply_pending = false; AttemptEvent::ReadyCommitted }
                    };
                    let (owner, transition_effects) = fixture_after_attempt(init, event);
                    let mut effects = Vec::new();
                    let mut coordinator = Vec::new();
                    for fx in transition_effects {
                        if let SessionFx::Coordinator(action) = fx { coordinator.push(action); }
                        else { effects.push(fx); }
                    }
                    if matches!(case, 10 | 11) {
                        assert!(matches!(coordinator.as_slice(), [CoordinatorAction::SignInCompleted]));
                    } else { assert!(coordinator.is_empty()); }
                    let actual = owner.snapshot_init();
                    assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap(), "phase={phase:?} case={case}");
                    assert_eq!(actual.signin_account, expected.signin_account);
                    assert_eq!(owner.state.qr().code.as_ptr(), code);
                    if erase.is_some() {
                        assert!(matches!(&owner.state, Attempt::Erasing(_)));
                    }
                    if case == 7 && erase.is_none() {
                        assert!(matches!(&owner.state, Attempt::Switching(data) if data.selection.owns(29)));
                        assert!(matches!(effects.as_slice(), [SessionFx::Capture { req: 29, epoch: 1, request: SessionReadRequest::ProfilePolicy }]));
                    } else { assert!(effects.is_empty()); }
                }
            }
        }
    }

    #[test]
    fn attempt_runtime_historical_tiles_are_not_selection_authority() {
        for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering,
            Phase::Ready, Phase::Error, Phase::Deleted] {
            let init = fixture(phase, None);
            let expected = serde_json::to_value(&init).unwrap();
            let mut owner = SessionMachine::from_init(init);
            let publication = owner.publication();
            let effects = super::super::scenarios::step(&mut owner,
                SessionEvent::Command(Command::SelectProfile { index: 0, pin: None }));
            assert!(effects.is_empty());
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), expected);
            assert!(Arc::ptr_eq(&publication, &owner.publication()));
        }
    }

    #[test]
    fn attempt_runtime_ready_history_cannot_reactivate_a_legacy_login_claim() {
        use super::super::super::{LoginProgress, observation::Observation};
        use plx_machine::machine::RequestId;
        let mut init = fixture(Phase::Ready, None);
        init.persisted.account_token = "synthetic-stored-account".into();
        init.apply_pending = false;
        let key = SessionWorkKey { epoch: init.epoch, op: SessionOp::Login };
        init.pending.insert(31, Pending { key, expected: Identity::of(&init.persisted), lifecycle: None,
            last_arrival: None, phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(31)) });
        let expected = serde_json::to_value(&init).unwrap();
        let hash = init.hash();
        let epoch = init.epoch;
        for (terminal, progress) in [
            (false, LoginProgress::CodeReady { epoch, code: "STALE".into(), qr_png: vec![99] }),
            (false, LoginProgress::Authorized { epoch, token: "synthetic-stale-token".into() }),
            (true, LoginProgress::Failed { epoch, message: "stale failure".into(),
                incident: IncidentContext::internal(InternalClass::WorkerDropped), plaintext: None, account: None }),
        ] {
            let mut owner = SessionMachine::from_init(init.clone());
            assert!(owner.snapshot_init().signin_active && owner.snapshot_init().authorized_in_flow);
            assert!(!owner.state.active_signin());
            assert!(!owner.state.fresh_authority());
            assert!(!owner.work_is_current(31, key, AdmissionId(31)));
            let publication = owner.publication();
            let envelope = SessionEnvelope { addr: Addr { to: MachineId::Session, req: RequestId(31) }, key,
                admission: AdmissionId(31), arrival: 1, terminal, lifecycle: None,
                outcome: SessionArrival::Data(Arc::new(Observation::Login(progress))) };
            let receipt = Receipt::of(&envelope);
            let effects = super::super::scenarios::step(&mut owner, SessionEvent::Result(envelope));
            assert!(matches!(effects.as_slice(), [SessionFx::Acknowledge(receipts)] if receipts == &vec![receipt]));
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), expected);
            assert_eq!(owner.snapshot_init().signin_account, init.signin_account);
            assert_eq!(owner.subhash(), hash);
            assert!(Arc::ptr_eq(&publication, &owner.publication()));
        }
    }

    #[test]
    fn attempt_runtime_ready_history_cannot_authorize_capture_admission_or_commit() {
        use plx_machine::machine::RequestId;
        let mut init = fixture(Phase::Ready, None);
        init.apply_pending = false;
        let key = SessionWorkKey { epoch: init.epoch, op: SessionOp::Login };
        init.pending.insert(31, Pending { key, expected: Identity::of(&init.persisted), lifecycle: None,
            last_arrival: None, phase: StreamPhase::Running, capture: Some(CaptureIntent::Login),
            admission: AdmissionState::Awaiting(AdmissionId(31)) });
        init.pending_commit = Some(PendingCommit { req: 31, epoch: init.epoch, arrival: 5, terminal: true,
            writes_credentials: false, receipt: None,
            delta: CommitDelta { phase: Some(Phase::Profiles), users: Some(Vec::new()), ..Default::default() },
            admitted_revision: None, purpose: None, fresh: false });
        let expected = serde_json::to_value(&init).unwrap();
        let mut owner = SessionMachine::from_init(init);
        assert!(!owner.read_is_current(31, key.epoch, SessionReadRequest::LoginClientId));
        assert!(!owner.commit_is_current(31, key.epoch, 5));
        assert!(owner.commit_permit(31, key.epoch, 5).is_none());
        let publication = owner.publication();
        let addr = Addr { to: MachineId::Session, req: RequestId(31) };
        for event in [
            SessionEvent::Read(SessionReadReply { addr, epoch: key.epoch, value: SessionReadValue::LoginClientId("stale-client".into()) }),
            SessionEvent::Admission(AdmissionReply { addr, key, correlation: AdmissionId(31), accepted: true }),
            SessionEvent::Commit(CommitReply { req: 31, epoch: key.epoch, arrival: 5, admission: CommitAdmission::RegistryOnly }),
        ] {
            assert!(super::super::scenarios::step(&mut owner, event).is_empty());
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), expected);
            assert!(Arc::ptr_eq(&publication, &owner.publication()));
        }
    }

    #[test]
    fn attempt_runtime_foreground_commit_claims_require_the_phase_owned_slot() {
        for (phase, op) in [(Phase::Idle, SessionOp::DevBoundary), (Phase::Idle, SessionOp::Ready),
            (Phase::Profiles, SessionOp::Picker), (Phase::Ready, SessionOp::Ready),
            (Phase::Ready, SessionOp::DevBoundary), (Phase::Ready, SessionOp::Picker),
            (Phase::Profiles, SessionOp::Ready), (Phase::Error, SessionOp::DevBoundary)] {
            let mut init = fixture(phase, None);
            init.apply_pending = false;
            let key = SessionWorkKey { epoch: init.epoch, op };
            init.pending.insert(41, Pending { key, expected: Identity::of(&init.persisted), lifecycle: None,
                last_arrival: None, phase: StreamPhase::Running, capture: None, admission: AdmissionState::NotRequested });
            let delta = match op {
                SessionOp::DevBoundary => CommitDelta { dev: Some(DevCommitDelta::Activated), ..Default::default() },
                SessionOp::Ready => CommitDelta { phase: (phase == Phase::Idle).then_some(Phase::Ready), ready: Some(false), activate_profile: true, ..Default::default() },
                SessionOp::Picker => CommitDelta { users: Some(Vec::new()), ..Default::default() },
                SessionOp::Login | SessionOp::Rediscover | SessionOp::HomeRoster | SessionOp::ServerRoster
                    | SessionOp::ProfileSwitch | SessionOp::Endpoint(_) => unreachable!(),
            };
            init.pending_commit = Some(PendingCommit { req: 41, epoch: init.epoch, arrival: 3, terminal: true,
                writes_credentials: false, receipt: None, delta, admitted_revision: None, purpose: None, fresh: false });
            let expected = serde_json::to_value(&init).unwrap();
            let hash = init.hash();
            let mut owner = SessionMachine::from_init(init);
            let publication = owner.publication();
            assert!(!owner.commit_is_current(41, key.epoch, 3), "{phase:?}");
            assert!(owner.commit_permit(41, key.epoch, 3).is_none());
            let effects = super::super::scenarios::step(&mut owner, SessionEvent::Commit(CommitReply {
                req: 41, epoch: key.epoch, arrival: 3, admission: CommitAdmission::RegistryOnly }));
            assert!(effects.is_empty());
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), expected);
            assert_eq!(owner.subhash(), hash);
            assert!(Arc::ptr_eq(&publication, &owner.publication()));
        }
    }

    #[test]
    fn attempt_runtime_foreground_commit_owners_survive_compatibility_import() {
        for case in 0..4 {
            let mut persisted = PersistedSession { client_id: "synthetic-client".into(), account_token: "synthetic-account".into(), ..Default::default() };
            persisted.server.address = "127.0.0.1".into(); persisted.server.port = 32400;
            persisted.server.token = "synthetic-pms".into(); persisted.user.token = "synthetic-pms".into();
            let mut init = SessionInit::captured(persisted);
            let command = match case {
                0 => { init.authority = BootstrapAuthority::DevPms { primary: init.persisted.server.clone(), extras: Vec::new() }; Command::ActivateDevBootstrap }
                1 => Command::ResumeStored,
                2 => Command::StartSwitch(Picker::Boot),
                _ => { init.phase = Phase::Ready; init.apply_pending = true; Command::TakeReady }
            };
            let mut owner = SessionMachine::from_init(init);
            let effects = super::super::scenarios::step(&mut owner, SessionEvent::Command(command));
            assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })));
            let captured = owner.snapshot_init();
            let commit = captured.pending_commit.as_ref().unwrap();
            let reply = CommitReply { req: commit.req, epoch: commit.epoch, arrival: commit.arrival, admission: CommitAdmission::RegistryOnly };
            let bytes = serde_json::to_vec(&captured).unwrap();
            let decoded: SessionInit = serde_json::from_slice(&bytes).unwrap();
            let mut restored = SessionMachine::from_init(decoded);
            assert!(owner.commit_is_current(reply.req, reply.epoch, reply.arrival));
            assert!(restored.commit_is_current(reply.req, reply.epoch, reply.arrival));
            let original_fx = super::super::scenarios::step(&mut owner, SessionEvent::Commit(reply));
            let restored_fx = super::super::scenarios::step(&mut restored, SessionEvent::Commit(reply));
            assert_eq!(serde_json::to_value(&original_fx).unwrap(), serde_json::to_value(&restored_fx).unwrap());
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), serde_json::to_value(restored.snapshot_init()).unwrap());
            assert_eq!(owner.subhash(), restored.subhash());
            assert_eq!(serde_json::to_vec(&captured).unwrap(), bytes);
        }
    }

}
