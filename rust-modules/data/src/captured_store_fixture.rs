//! Value-only fixtures: scripted observations never mutate the process registry.
use plx_plex::plex::{ClientFacts, ClientKey, ClientKeys, ServerFacts, ServerId};
pub(crate) fn key(token: u32) -> ClientKey { ClientKey { instance: 101, token, grant: 1 } }
pub(crate) fn roster(rows: &[(u16, Option<ClientKey>, &str)], generation: u32, facts: u32) -> ClientKeys {
    ClientKeys::captured(rows.first().map_or(ServerId::UNSET, |row| ServerId::from_raw(row.0)), generation, facts,
        rows.iter().map(|&(raw, key, handle)| {
            let described = Box::leak(Box::new(ServerFacts { name: format!("script-{raw}"), handle: handle.into(),
                owned: handle.is_empty(), home: false, owner_id: 42 }));
            (ServerId::from_raw(raw), key, ClientFacts { machine_id: format!("script-{raw}"),
                described: Some(described), source: Some((None, None)), stream_allowed: false })
        }))
}
