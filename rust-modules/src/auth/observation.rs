//! Pointer-free observations delivered to the logical Session owner. Native launch handles are
//! extracted by the application adapter, never retained in an owner event or replayable effect.

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) enum Observation {
    Login(LoginProgress),
    Registry(RegistryProgress),
    HomeRoster(HomeRosterProgress),
    ServerRoster(ServerRosterProgress),
    Endpoint(EndpointFact),
    ProfileSwitch(ProfileSwitchProgress),
    ProfileRoster(ProfileRosterProgress),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct EndpointFact {
    pub epoch: u64,
    pub expected: SessionIdentity,
    pub sid: u16,
    pub machine_id: String,
    pub fresh: Option<SourceRef>,
}

impl Observation {
    pub(crate) fn write(&self, w: &mut crate::ui::machine::Canon) {
        use owner::{write_profile, write_server, write_sources, write_tile, write_user};
        match self {
            Self::Login(progress) => {
                w.u8(0);
                match progress {
                    LoginProgress::CodeReplacing { epoch } => { w.u8(0).u64(*epoch); }
                    LoginProgress::CodeReady { epoch, code, qr_png } => {
                        w.u8(1).u64(*epoch).str(code).seq(qr_png.len());
                        for byte in qr_png { w.u8(*byte); }
                    }
                    LoginProgress::Authorized { epoch, token } => { w.u8(2).u64(*epoch).str(token); }
                    LoginProgress::Failed { epoch, message } => { w.u8(3).u64(*epoch).str(message); }
                    LoginProgress::SignedIn { epoch, server, sources, users } => {
                        w.u8(4).u64(*epoch); write_server(w, server); write_sources(w, sources);
                        w.seq(users.len()); for user in users { write_tile(w, user); }
                    }
                }
            }
            Self::Registry(progress) => {
                w.u8(1);
                match progress {
                    RegistryProgress::Activate { epoch, expected, candidate } => {
                        w.u8(0).u64(*epoch); w.option(expected.as_ref(), write_identity);
                        w.str(&candidate.machine_id).str(&candidate.token).str(&candidate.name)
                            .str(&candidate.credit).bool(candidate.owned).str(&candidate.origin.base())
                            .str(&candidate.address);
                        owner::write_tier(w, Some(candidate.location)); w.bool(candidate.ipv6);
                    }
                    RegistryProgress::Settled { epoch, expected, probe } => {
                        w.u8(1).u64(*epoch); w.option(expected.as_ref(), write_identity); write_probe(w, probe);
                    }
                    RegistryProgress::Install { epoch, expected, sources, primary } => {
                        w.u8(2).u64(*epoch); w.option(expected.as_ref(), write_identity);
                        write_sources(w, sources); w.option(*primary, |w, p| { w.u64(p as u64); });
                    }
                }
            }
            Self::HomeRoster(progress) => {
                w.u8(2).u64(progress.epoch); write_identity(w, &progress.expected);
                w.option(progress.users.as_ref(), |w, users| {
                    w.seq(users.len()); for user in users { write_tile(w, user); }
                });
            }
            Self::ServerRoster(progress) => {
                w.u8(3).u64(progress.epoch); write_identity(w, &progress.expected);
                match &progress.outcome {
                    ServerRosterOutcome::Unreachable => { w.u8(0); }
                    ServerRosterOutcome::NoReachable => { w.u8(1); }
                    ServerRosterOutcome::Reconcile { resources, found, household, settled } => {
                        w.u8(2); write_resources(w, resources); write_sources(w, found);
                        w.seq(household.len()); for id in household { w.u64(*id as u64); }
                        write_probes(w, settled);
                    }
                }
            }
            Self::Endpoint(progress) => {
                w.u8(4).u64(progress.epoch); write_identity(w, &progress.expected);
                w.u32(u32::from(progress.sid)).str(&progress.machine_id);
                w.option(progress.fresh.as_ref(), |w, source| write_sources(w, std::slice::from_ref(source)));
            }
            Self::ProfileSwitch(progress) => {
                w.u8(5).u64(progress.epoch); write_identity(w, &progress.expected);
                match &progress.outcome {
                    ProfileSwitchOutcomeProgress::Failed { error, pin_denied } => { w.u8(0).str(error).bool(*pin_denied); }
                    ProfileSwitchOutcomeProgress::Ready { delta, probes } => {
                        w.u8(1); write_server(w, &delta.server); write_sources(w, &delta.sources);
                        write_user(w, &delta.user); w.option(delta.cache.as_ref(), write_profile);
                        write_probes(w, probes);
                    }
                }
            }
            Self::ProfileRoster(progress) => {
                w.u8(6).u64(progress.epoch); write_identity(w, &progress.expected);
                write_resources(w, &progress.resources); write_sources(w, &progress.reached);
                write_probes(w, &progress.probes);
            }
        }
    }

    pub(crate) fn matches_request(&self, pending: &owner::Pending, terminal: bool) -> bool {
        use owner::{SessionOp, StreamPhase};
        let (epoch, expected, compatible) = match self {
            Self::Login(p) => {
                let login = matches!(pending.key.op, SessionOp::Login | SessionOp::Rediscover);
                match p {
                    LoginProgress::CodeReplacing { epoch } | LoginProgress::CodeReady { epoch, .. }
                    | LoginProgress::Authorized { epoch, .. } =>
                        (*epoch, None, pending.key.op == SessionOp::Login && !terminal),
                    LoginProgress::Failed { epoch, .. } | LoginProgress::SignedIn { epoch, .. } =>
                        (*epoch, None, login && terminal),
                }
            }
            Self::Registry(p) => {
                let (epoch, expected) = match p {
                    RegistryProgress::Activate { epoch, expected, .. }
                    | RegistryProgress::Settled { epoch, expected, .. }
                    | RegistryProgress::Install { epoch, expected, .. } => (*epoch, expected.as_ref()),
                };
                let compatible = match pending.key.op {
                    SessionOp::Login | SessionOp::Rediscover => expected.is_none(),
                    SessionOp::ServerRoster => expected.is_some(),
                    _ => false,
                };
                (epoch, expected, compatible && !terminal)
            }
            Self::HomeRoster(p) => (p.epoch, Some(&p.expected),
                pending.key.op == SessionOp::HomeRoster && terminal),
            Self::ServerRoster(p) => (p.epoch, Some(&p.expected),
                pending.key.op == SessionOp::ServerRoster && terminal),
            Self::Endpoint(p) => (p.epoch, Some(&p.expected),
                pending.key.op == SessionOp::Endpoint(p.sid) && terminal),
            Self::ProfileSwitch(p) => (p.epoch, Some(&p.expected),
                pending.key.op == SessionOp::ProfileSwitch && pending.phase == StreamPhase::Running
                    && (terminal || matches!(p.outcome, ProfileSwitchOutcomeProgress::Ready { .. }))),
            Self::ProfileRoster(p) => (p.epoch, Some(&p.expected),
                pending.key.op == SessionOp::ProfileSwitch && pending.phase == StreamPhase::ProfileSeated && terminal),
        };
        compatible && epoch == pending.key.epoch && expected.is_none_or(|expected|
            expected.client_id == pending.expected.client_id
                && expected.account_token == pending.expected.account_token
                && expected.profile_uuid == pending.expected.profile_uuid)
    }

    pub(crate) fn from_transport(value: AuthProgress) -> (Self, Option<ClientLifecycle>) {
        let observation = match value {
            AuthProgress::Login(p) => Self::Login(p),
            AuthProgress::Registry(p) => Self::Registry(p),
            AuthProgress::HomeRoster(p) => Self::HomeRoster(p),
            AuthProgress::ServerRoster(p) => Self::ServerRoster(p),
            AuthProgress::ProfileSwitch(p) => Self::ProfileSwitch(p),
            AuthProgress::ProfileRoster(p) => Self::ProfileRoster(p),
            AuthProgress::Endpoint(p) => return (Self::Endpoint(EndpointFact {
                epoch: p.epoch, expected: p.expected, sid: p.id.raw(),
                machine_id: p.machine_id, fresh: p.fresh,
            }), p.lifecycle),
        };
        (observation, None)
    }
}

fn write_identity(w: &mut crate::ui::machine::Canon, identity: &SessionIdentity) {
    w.str(&identity.client_id).str(&identity.account_token).str(&identity.profile_uuid);
}

#[cfg(test)]
mod identity_canon_tests {
    use super::*;

    #[test]
    fn observation_identity_canon_is_three_fields_without_legacy_authority_tag() {
        use crate::ui::machine::Canon;
        let identity = SessionIdentity::of(&Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-account".into(),
            user: UserRef { uuid: "synthetic-profile".into(), ..Default::default() },
            ..Default::default()
        });
        let mut current = Canon::new();
        write_identity(&mut current, &identity);
        let mut expected = Canon::new();
        expected.str(&identity.client_id).str(&identity.account_token).str(&identity.profile_uuid);
        assert_eq!(current.finish(), expected.finish());
        let mut legacy = Canon::new();
        legacy.str(&identity.client_id).str(&identity.account_token).str(&identity.profile_uuid).u8(0);
        assert_ne!(current.finish(), legacy.finish(), "removal changes the canonical stream, not just Rust layout");
        let encoded = serde_json::to_value(&identity).unwrap();
        assert!(encoded.get("authority").is_none());
        let restored: SessionIdentity = serde_json::from_value(encoded).unwrap();
        let mut roundtrip = Canon::new();
        write_identity(&mut roundtrip, &restored);
        assert_eq!(current.finish(), roundtrip.finish());
    }
}

fn write_probe(w: &mut crate::ui::machine::Canon, probe: &SettledProbe) {
    w.str(&probe.machine_id).u8(match probe.outcome {
        Outcome::Reachable => 0, Outcome::WrongServer => 1, Outcome::Unauthorized => 2, Outcome::Unreachable => 3,
    });
    owner::write_tier(w, probe.tier);
}

fn write_probes(w: &mut crate::ui::machine::Canon, probes: &[SettledProbe]) {
    w.seq(probes.len()); for probe in probes { write_probe(w, probe); }
}

fn write_resources(w: &mut crate::ui::machine::Canon, resources: &[Resource]) {
    w.seq(resources.len());
    for resource in resources {
        w.str(&resource.name).str(&resource.client_identifier).str(&resource.provides)
            .bool(resource.owned).str(&resource.access_token);
        w.option(resource.source_title.as_ref(), |w, title| { w.str(title); });
        w.u64(resource.owner_id as u64).bool(resource.home).bool(resource.presence)
            .bool(resource.public_address_matches).bool(resource.https_required).seq(resource.connections.len());
        for connection in &resource.connections {
            w.str(&connection.protocol).str(&connection.address).u64(connection.port as u64)
                .str(&connection.uri).bool(connection.local).bool(connection.relay).bool(connection.ipv6);
        }
    }
}

/// The account parser remains authoritative. Serialize its complete existing data shape without
/// changing account.rs or substituting a smaller grant model for its real policy inputs.
pub(super) mod resources {
    use super::*;
    use serde::ser::{SerializeSeq, SerializeStruct};

    struct ResourceRead<'a>(&'a Resource);
    struct ConnectionRead<'a>(&'a crate::plex::account::Connection);
    struct ConnectionsRead<'a>(&'a [crate::plex::account::Connection]);

    impl Serialize for ConnectionRead<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let c = self.0;
            let mut s = serializer.serialize_struct("Connection", 7)?;
            s.serialize_field("protocol", &c.protocol)?;
            s.serialize_field("address", &c.address)?;
            s.serialize_field("port", &c.port)?;
            s.serialize_field("uri", &c.uri)?;
            s.serialize_field("local", &c.local)?;
            s.serialize_field("relay", &c.relay)?;
            s.serialize_field("IPv6", &c.ipv6)?;
            s.end()
        }
    }
    impl Serialize for ConnectionsRead<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut s = serializer.serialize_seq(Some(self.0.len()))?;
            for c in self.0 { s.serialize_element(&ConnectionRead(c))?; }
            s.end()
        }
    }
    impl Serialize for ResourceRead<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let r = self.0;
            let mut s = serializer.serialize_struct("Resource", 12)?;
            s.serialize_field("name", &r.name)?;
            s.serialize_field("clientIdentifier", &r.client_identifier)?;
            s.serialize_field("provides", &r.provides)?;
            s.serialize_field("owned", &r.owned)?;
            s.serialize_field("accessToken", &r.access_token)?;
            s.serialize_field("sourceTitle", &r.source_title)?;
            s.serialize_field("ownerId", &r.owner_id)?;
            s.serialize_field("home", &r.home)?;
            s.serialize_field("presence", &r.presence)?;
            s.serialize_field("publicAddressMatches", &r.public_address_matches)?;
            s.serialize_field("httpsRequired", &r.https_required)?;
            s.serialize_field("connections", &ConnectionsRead(&r.connections))?;
            s.end()
        }
    }
    pub fn serialize<S: serde::Serializer>(value: &[Resource], serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_seq(Some(value.len()))?;
        for r in value { s.serialize_element(&ResourceRead(r))?; }
        s.end()
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Resource>, D::Error> {
        Vec::<Resource>::deserialize(deserializer)
    }
}

pub(super) mod origin {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &Origin, serializer: S) -> Result<S::Ok, S::Error> {
        value.base().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Origin, D::Error> {
        let value = String::deserialize(deserializer)?;
        Origin::parse(&value).ok_or_else(|| serde::de::Error::custom("invalid Session observation origin"))
    }
}

pub(super) mod outcome {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &Outcome, serializer: S) -> Result<S::Ok, S::Error> {
        let tag: u8 = match value {
            Outcome::Reachable => 0, Outcome::WrongServer => 1,
            Outcome::Unauthorized => 2, Outcome::Unreachable => 3,
        };
        tag.serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Outcome, D::Error> {
        match u8::deserialize(deserializer)? {
            0 => Ok(Outcome::Reachable), 1 => Ok(Outcome::WrongServer),
            2 => Ok(Outcome::Unauthorized), 3 => Ok(Outcome::Unreachable),
            _ => Err(serde::de::Error::custom("invalid Session probe outcome")),
        }
    }
}

pub(super) mod arc {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &Arc<Observation>, serializer: S) -> Result<S::Ok, S::Error> {
        value.as_ref().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Arc<Observation>, D::Error> {
        Observation::deserialize(deserializer).map(Arc::new)
    }
}

pub(super) mod address {
    use serde::{Deserialize, Serialize};
    use crate::ui::machine::{Addr, InstanceId, MachineId, RequestId, StoreOrd};
    pub fn serialize<S: serde::Serializer>(value: &Addr, serializer: S) -> Result<S::Ok, S::Error> {
        let (tag, id): (u8, u32) = match value.to {
            MachineId::Session => (0, 0), MachineId::Consent => (1, 0),
            MachineId::Input => (2, 0), MachineId::Present => (3, 0), MachineId::Nav => (4, 0),
            MachineId::Player => (5, 0), MachineId::Store(id) => (6, id.0),
            MachineId::Instance(id) => (7, id.0), MachineId::Cache => (8, 0),
        };
        (tag, id, value.req.0).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Addr, D::Error> {
        let (tag, id, req) = <(u8, u32, u32)>::deserialize(deserializer)?;
        let to = match (tag, id) {
            (0, 0) => MachineId::Session, (1, 0) => MachineId::Consent,
            (2, 0) => MachineId::Input, (3, 0) => MachineId::Present,
            (4, 0) => MachineId::Nav, (5, 0) => MachineId::Player,
            (6, id) => MachineId::Store(StoreOrd(id)), (7, id) => MachineId::Instance(InstanceId(id)),
            (8, 0) => MachineId::Cache,
            _ => return Err(serde::de::Error::custom("invalid Session delivery address")),
        };
        Ok(Addr { to, req: RequestId(req) })
    }
}

pub(super) mod server_id {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &ServerId, serializer: S) -> Result<S::Ok, S::Error> {
        value.raw().serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<ServerId, D::Error> {
        u16::deserialize(deserializer).map(ServerId::from_raw)
    }
}
