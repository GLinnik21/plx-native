//! Phase-owned presentation imported from the legacy DTO. Work authority remains outside Attempt.
use super::*;

pub(super) struct Common {
    pub(super) authority: BootstrapAuthority,
    pub(super) disk_identity: Identity,
    pub(super) persisted: PersistedSession,
    pub(super) committed_credentials: CredentialPatch,
    pub(super) authorized_in_flow: bool,
    pub(super) signin_active: bool,
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
    pub(super) discovery_retry: Option<super::super::DiscoveryRetryProgress>,
    pub(super) plaintext: Option<super::super::PlaintextVerdict>,
    pub(super) signin_account: Option<String>,
    pub(super) switch_refused_for: Option<Identity>,
}

pub(super) struct QrHistory { pub(super) code: String, pub(super) png: Vec<u8>, pub(super) generation: u64, pub(super) replaced: bool, pub(super) trouble: bool }
pub(super) struct PickerPresentation { pub(super) picker: Picker, pub(super) users: Vec<UserTile>, pub(super) error: String, pub(super) pin_denied: bool }
// Read-only retention: never confers work or selection authority.
struct HistoricalPresentation { qr: QrHistory, picker: PickerPresentation }
struct ImportedIntent(bool);
struct HandoffIntent(bool);

struct IdleData { history: HistoricalPresentation, residue: ImportedIntent }
struct CreatingData { qr: QrHistory, picker_history: PickerPresentation, residue: ImportedIntent }
struct WaitingData { qr: QrHistory, picker_history: PickerPresentation, residue: ImportedIntent }
struct DiscoveringData { history: HistoricalPresentation, residue: ImportedIntent }
struct ProfilesData { qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
struct SwitchingData { qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
struct ReadyData { history: HistoricalPresentation, intent: HandoffIntent }
struct ErrorData { qr_history: QrHistory, picker: PickerPresentation, residue: ImportedIntent }
struct DeletedData { history: HistoricalPresentation, residue: ImportedIntent }
struct ErasingData { history: HistoricalPresentation, residue: ImportedIntent, prior_phase: Phase, pending_erase: (u64, bool) }

enum Attempt {
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

pub(super) struct Partition { pub(super) common: Common, attempt: Attempt }

// Integration checkpoint: phase moves preserve every imported presentation value. Typed
// transition entry/exit plans will replace this private facade in the next phase.
impl Partition {
    pub(super) fn phase(&self) -> Phase { self.attempt.presentation().phase }
    pub(super) fn pending_erase(&self) -> Option<(u64, bool)> { self.attempt.presentation().pending_erase }
    pub(super) fn apply_pending(&self) -> bool { self.attempt.presentation().apply_pending }
    pub(super) fn qr(&self) -> &QrHistory { self.attempt.presentation().qr }
    pub(super) fn picker_data(&self) -> &PickerPresentation { self.attempt.presentation().picker }
    pub(super) fn qr_mut(&mut self) -> &mut QrHistory { self.attempt.presentation_mut().0 }
    pub(super) fn picker_data_mut(&mut self) -> &mut PickerPresentation { self.attempt.presentation_mut().1 }
    pub(super) fn set_apply_pending(&mut self, value: bool) { *self.attempt.presentation_mut().2 = value; }
    pub(super) fn set_phase(&mut self, phase: Phase) { self.remap(Some(phase), None); }
    pub(super) fn set_pending_erase(&mut self, pending: Option<(u64, bool)>) { self.remap(None, Some(pending)); }
    fn remap(&mut self, phase: Option<Phase>, erase: Option<Option<(u64, bool)>>) {
        // The placeholder is transient, owns no retained state and is never observable.
        let placeholder = Attempt::Idle(IdleData {
            history: HistoricalPresentation {
                qr: QrHistory { code: String::new(), png: Vec::new(), generation: 0, replaced: false, trouble: false },
                picker: PickerPresentation { picker: Picker::Boot, users: Vec::new(), error: String::new(), pin_denied: false },
            }, residue: ImportedIntent(false),
        });
        let old = std::mem::replace(&mut self.attempt, placeholder);
        let (old_phase, qr, picker, intent, old_erase) = old.into_parts();
        self.attempt = Attempt::build(phase.unwrap_or(old_phase), qr, picker, intent, erase.unwrap_or(old_erase));
    }
    pub(super) fn roster_readout_up(&self) -> bool {
        let p = self.attempt.presentation(); is_roster_readout(p.phase, &p.picker.users, &p.picker.error)
    }
    pub(super) fn roster_dead_end(&self) -> bool {
        let p = self.attempt.presentation(); p.phase == Phase::Profiles && p.picker.users.is_empty()
            && !self.common.pending.values().any(|pending| pending.key.op == SessionOp::HomeRoster)
    }
    pub(super) fn dead_end_resumes(&self) -> bool {
        matches!(self.common.authority, BootstrapAuthority::DevPms { .. })
            || self.common.committed_credentials.merge_into(&self.common.persisted).can_go_local()
    }
    pub(super) fn readout_account(&self) -> Option<&str> {
        let p = self.attempt.presentation(); self.common.signin_account.as_deref().filter(|_| p.phase == Phase::Error
            && p.picker.error == plx_platform::i18n::msg::browse_auth_no_servers())
    }
    pub(super) fn switch_refused(&self) -> bool {
        self.common.switch_refused_for.as_ref().is_some_and(|id| id.matches(&self.common.persisted))
    }
    pub(super) fn refuse_switch(&mut self) {
        self.picker_data_mut().error = roster_refused().into();
        self.common.switch_refused_for = Some(Identity::of(&self.common.persisted));
    }
}
impl Attempt {
    fn presentation_mut(&mut self) -> (&mut QrHistory, &mut PickerPresentation, &mut bool) {
        match self {
            Self::Idle(d) => (&mut d.history.qr, &mut d.history.picker, &mut d.residue.0),
            Self::Creating(d) => (&mut d.qr, &mut d.picker_history, &mut d.residue.0),
            Self::Waiting(d) => (&mut d.qr, &mut d.picker_history, &mut d.residue.0),
            Self::Discovering(d) => (&mut d.history.qr, &mut d.history.picker, &mut d.residue.0),
            Self::Profiles(d) => (&mut d.qr_history, &mut d.picker, &mut d.residue.0),
            Self::Switching(d) => (&mut d.qr_history, &mut d.picker, &mut d.residue.0),
            Self::Ready(d) => (&mut d.history.qr, &mut d.history.picker, &mut d.intent.0),
            Self::Error(d) => (&mut d.qr_history, &mut d.picker, &mut d.residue.0),
            Self::Deleted(d) => (&mut d.history.qr, &mut d.history.picker, &mut d.residue.0),
            Self::Erasing(d) => (&mut d.history.qr, &mut d.history.picker, &mut d.residue.0),
        }
    }
    fn into_parts(self) -> (Phase, QrHistory, PickerPresentation, bool, Option<(u64, bool)>) {
        match self {
            Self::Idle(d) => (Phase::Idle, d.history.qr, d.history.picker, d.residue.0, None),
            Self::Creating(d) => (Phase::Creating, d.qr, d.picker_history, d.residue.0, None),
            Self::Waiting(d) => (Phase::Waiting, d.qr, d.picker_history, d.residue.0, None),
            Self::Discovering(d) => (Phase::Discovering, d.history.qr, d.history.picker, d.residue.0, None),
            Self::Profiles(d) => (Phase::Profiles, d.qr_history, d.picker, d.residue.0, None),
            Self::Switching(d) => (Phase::Switching, d.qr_history, d.picker, d.residue.0, None),
            Self::Ready(d) => (Phase::Ready, d.history.qr, d.history.picker, d.intent.0, None),
            Self::Error(d) => (Phase::Error, d.qr_history, d.picker, d.residue.0, None),
            Self::Deleted(d) => (Phase::Deleted, d.history.qr, d.history.picker, d.residue.0, None),
            Self::Erasing(d) => (d.prior_phase, d.history.qr, d.history.picker, d.residue.0, Some(d.pending_erase)),
        }
    }
    fn build(phase: Phase, qr: QrHistory, picker: PickerPresentation, intent: bool, erase: Option<(u64, bool)>) -> Self {
        if let Some(pending_erase) = erase {
            return Self::Erasing(ErasingData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(intent), prior_phase: phase, pending_erase });
        }
        match phase {
            Phase::Idle => Self::Idle(IdleData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(intent) }),
            Phase::Creating => Self::Creating(CreatingData { qr, picker_history: picker, residue: ImportedIntent(intent) }),
            Phase::Waiting => Self::Waiting(WaitingData { qr, picker_history: picker, residue: ImportedIntent(intent) }),
            Phase::Discovering => Self::Discovering(DiscoveringData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(intent) }),
            Phase::Profiles => Self::Profiles(ProfilesData { qr_history: qr, picker, residue: ImportedIntent(intent) }),
            Phase::Switching => Self::Switching(SwitchingData { qr_history: qr, picker, residue: ImportedIntent(intent) }),
            Phase::Ready => Self::Ready(ReadyData { history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(intent) }),
            Phase::Error => Self::Error(ErrorData { qr_history: qr, picker, residue: ImportedIntent(intent) }),
            Phase::Deleted => Self::Deleted(DeletedData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(intent) }),
        }
    }
}
// Common fields remain one orthogonal owner; the facade is temporary for handler wiring.
impl std::ops::Deref for Partition {
    type Target = Common;
    fn deref(&self) -> &Common { &self.common }
}
impl std::ops::DerefMut for Partition {
    fn deref_mut(&mut self) -> &mut Common { &mut self.common }
}
struct PresentationRef<'a> { phase: Phase, qr: &'a QrHistory, picker: &'a PickerPresentation, apply_pending: bool, pending_erase: Option<(u64, bool)> }

impl Partition {
    pub(super) fn from_init(init: SessionInit) -> Self {
        let SessionInit { authority, disk_identity, phase, picker, persisted, committed_credentials, pin_code, qr_png, users, error, pin_denied, authorized_in_flow, signin_active, apply_pending, code_replaced, qr_gen, next_qr, epoch, next_req, pending, pending_commit, persistence_purpose, commit_phase, admitted_persistence, persistence_warning, held_handoff, persistence_warning_answered, unconfirmed_fresh_prior, pending_erase, inbox, pump_pending, active_profile, profile_scope, delete_leftovers, incident, incidents_seen, next_incident, link_trouble, discovery_retry, plaintext, signin_account, switch_refused_for } = init;
        let common = Common { authority, disk_identity, persisted, committed_credentials, authorized_in_flow, signin_active, next_qr, epoch, next_req, pending, pending_commit, persistence_purpose, commit_phase, admitted_persistence, persistence_warning, held_handoff, persistence_warning_answered, unconfirmed_fresh_prior, inbox, pump_pending, active_profile, profile_scope, delete_leftovers, incident, incidents_seen, next_incident, discovery_retry, plaintext, signin_account, switch_refused_for };
        let qr = QrHistory { code: pin_code, png: qr_png, generation: qr_gen, replaced: code_replaced, trouble: link_trouble };
        let picker = PickerPresentation { picker, users, error, pin_denied };
        let attempt = if let Some(pending_erase) = pending_erase {
            Attempt::Erasing(ErasingData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending), prior_phase: phase, pending_erase })
        } else { match phase {
            Phase::Idle => Attempt::Idle(IdleData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
            Phase::Creating => Attempt::Creating(CreatingData { qr, picker_history: picker, residue: ImportedIntent(apply_pending) }),
            Phase::Waiting => Attempt::Waiting(WaitingData { qr, picker_history: picker, residue: ImportedIntent(apply_pending) }),
            Phase::Discovering => Attempt::Discovering(DiscoveringData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
            Phase::Profiles => Attempt::Profiles(ProfilesData { qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Switching => Attempt::Switching(SwitchingData { qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Ready => Attempt::Ready(ReadyData { history: HistoricalPresentation { qr, picker }, intent: HandoffIntent(apply_pending) }),
            Phase::Error => Attempt::Error(ErrorData { qr_history: qr, picker, residue: ImportedIntent(apply_pending) }),
            Phase::Deleted => Attempt::Deleted(DeletedData { history: HistoricalPresentation { qr, picker }, residue: ImportedIntent(apply_pending) }),
        }};
        Self { common, attempt }
    }

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
            authorized_in_flow: self.common.authorized_in_flow.clone(),
            signin_active: self.common.signin_active.clone(),
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
            discovery_retry: self.common.discovery_retry.clone(),
            plaintext: self.common.plaintext.clone(),
            signin_account: self.common.signin_account.clone(),
            switch_refused_for: self.common.switch_refused_for.clone(),
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
    pub(super) fn from_partition(partition: &'a Partition) -> Self {
        let c = &partition.common;
        let p = partition.attempt.presentation();
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
            authorized_in_flow: c.authorized_in_flow,
            signin_active: c.signin_active,
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
            plaintext: &c.plaintext,
            discovery_retry: c.discovery_retry,
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
                let projection = LegacyV5Projection::from_partition(&partition);
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
        let init = fixture(Phase::Waiting, None);
        let code_ptr = init.pin_code.as_ptr();
        let users_ptr = init.users.as_ptr();
        let mut runtime = Partition::from_init(init);
        runtime.set_phase(Phase::Discovering);
        assert_eq!(runtime.phase(), Phase::Discovering);
        assert_eq!(runtime.qr().code.as_ptr(), code_ptr);
        assert_eq!(runtime.picker_data().users.as_ptr(), users_ptr);
        runtime.set_pending_erase(Some((77, true)));
        assert_eq!(runtime.pending_erase(), Some((77, true)));
        runtime.set_phase(Phase::Error);
        runtime.qr_mut().trouble = false;
        runtime.picker_data_mut().error = "updated".into();
        runtime.set_apply_pending(false);
        let projected = LegacyV5Projection::from_partition(&runtime);
        assert_eq!(projected.phase, Phase::Error);
        assert_eq!(projected.pending_erase, Some((77, true)));
        assert_eq!(projected.pin_code.as_ptr(), code_ptr);
        assert_eq!(projected.users.as_ptr(), users_ptr);
        assert_eq!(projected.error.as_str(), "updated");
        assert!(!projected.apply_pending && !projected.link_trouble);
        let mut canon = Canon::new();
        projected.write(&mut canon);
        assert_eq!(canon.finish(), runtime.snapshot_init().hash());
        runtime.set_pending_erase(None);
        assert_eq!(runtime.phase(), Phase::Error);
        assert!(runtime.pending_erase().is_none());
        assert_eq!(runtime.qr().code.as_ptr(), code_ptr);
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
            let p = LegacyV5Projection::from_partition(&partition);
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
        assert_eq!(partition.common.signin_account.as_deref(), Some("another-private-account"));
        let p = partition.attempt.presentation();
        assert_eq!(p.qr.code, "retained-code");
        assert_eq!(p.picker.users[0].uuid, "retained-user");
        assert_eq!(partition.snapshot_init().signin_account.as_deref(), Some("another-private-account"));
    }
}
