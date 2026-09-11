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
