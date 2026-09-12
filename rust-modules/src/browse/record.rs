//! Private discovery wire values. Resource pointers are supplied only by initial bindings.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(crate) struct Result {
    epoch: u32,
    source: usize,
    instance: u32,
    landing: SrcLanding,
}

impl std::fmt::Debug for Result {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscoveryResult")
    }
}

impl Result {
    pub(crate) fn request_id(&self) -> u32 { self.source as u32 }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    kind: String,
    version: u32,
    epoch: u32,
    source: u32,
    sid: u16,
    client: u32,
    token_gen: u32,
    name: String,
    what: What,
}

#[derive(Serialize, Deserialize)]
enum What {
    Sections(Option<Vec<(i64, String, String)>>),
    Counts(Vec<(i64, i64)>),
}

pub(crate) fn take() -> Option<Result> {
    let (epoch, source, landing) = SRC_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    Some(Result { epoch, source, instance: landing.client.instance_gen(), landing })
}

pub(crate) fn encode(result: &Result) -> serde_json::Value {
    let landing = &result.landing;
    serde_json::to_value(Wire { kind: "discovery".into(), version: 1,
        epoch: result.epoch, source: result.source as u32, sid: landing.client.id().raw(),
        client: result.instance, token_gen: landing.token_gen, name: landing.name.clone(),
        what: match &landing.what {
            SrcWhat::Sections(list) => What::Sections(list.as_ref().map(|list| list.iter()
                .map(|(key, title, kind)| (*key, title.clone(), kind.wire().into())).collect())),
            SrcWhat::Counts(counts) => What::Counts(counts.clone()),
        },
    }).expect("discovery wire is serializable")
}

pub(crate) fn decode(value: serde_json::Value,
    mut bind: impl FnMut(u32) -> Option<&'static crate::plex::Client>) -> std::result::Result<Result, &'static str> {
    let wire: Wire = serde_json::from_value(value).map_err(|_| "invalid discovery result")?;
    if wire.kind != "discovery" || wire.version != 1 { return Err("unsupported discovery result"); }
    let client = bind(wire.client).ok_or("unbound discovery client")?;
    if client.id().raw() != wire.sid || client.token_gen() != wire.token_gen {
        return Err("discovery client mismatch");
    }
    let what = match wire.what {
        What::Counts(counts) => SrcWhat::Counts(counts),
        What::Sections(list) => SrcWhat::Sections(list.map(|list| list.into_iter().map(|(key, title, kind)| {
            SecKind::from_wire(&kind).map(|kind| (key, title, kind)).ok_or("invalid discovery section")
        }).collect::<std::result::Result<Vec<_>, _>>()).transpose()?),
    };
    Ok(Result { epoch: wire.epoch, source: wire.source as usize, instance: wire.client,
        landing: SrcLanding { client, token_gen: wire.token_gen, name: wire.name, what } })
}

pub(crate) fn validate_binding(value: serde_json::Value, instance: u32) -> std::result::Result<u32, &'static str> {
    let wire: Wire = serde_json::from_value(value).map_err(|_| "invalid discovery result")?;
    if wire.kind != "discovery" || wire.version != 1 || wire.sid != 0 || wire.source != 0
        || wire.client != instance || wire.token_gen != instance {
        return Err("unsupported discovery binding");
    }
    if let What::Sections(Some(list)) = wire.what {
        if list.iter().any(|(_, _, kind)| SecKind::from_wire(kind).is_none()) {
            return Err("invalid discovery section");
        }
    }
    Ok(wire.source)
}

pub(crate) fn apply(result: &Result, preferences: Option<&crate::plex::session::Session>)
    -> Option<crate::stores::EndpointRefresh> {
    apply_discovery(result.epoch, result.source, result.landing.clone(), preferences)
}
