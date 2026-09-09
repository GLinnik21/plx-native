//! Home adapter payload codec. Client objects are resources: encode their logical instance id,
//! never a pointer, origin or token. Decoding requires an explicit recorded-id → client mapping;
//! the replay bootstrap owns that mapping, including superseded clients still named by arrivals.
//! This module neither consults the live registry nor starts requests.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{Landing, LandingClient, SourceBuild};

pub(crate) const SHAPE: &str = "HubsResultV1{gen:u32,seq:u32,sid:u16,client:Option<u32>,token_gen:u32,build:Option<{cw:[{last_viewed_at:i64,m:PmsMovie}],shelves:[{title:str,hub_id:str,key:str,items:[PmsMovie]}]}>};PmsMovie{sid:u16,sec:i64,title:str,year:i32,rating:str,dur_ns:i64,part:str,thumb:str,still:str,art:str,summary:str,rk:str,vcodec:str,acodec:str,blur:[[f32bits;3];4],has_blur:bool,kind:i32,resume_ms:i64,show_rk:str,season_index:i32,show_title:str,ep_index:i32,unwatched:bool,watched:bool,aired:str}";

pub(crate) fn encode(landing: &Landing) -> Value {
    let Landing { gen, seq, sid, client, token_gen, build } = landing;
    json!({ "kind": "hubs", "version": 1, "gen": gen, "seq": seq,
        "sid": sid.raw(), "client": client.map(|c| c.instance),
        "token_gen": token_gen, "build": build })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    kind: String,
    version: u32,
    gen: u32,
    seq: u32,
    #[serde(with = "server_id")]
    sid: crate::plex::ServerId,
    #[serde(deserialize_with = "required_option")]
    client: Option<u32>,
    token_gen: u32,
    #[serde(deserialize_with = "required_option")]
    build: Option<SourceBuild>,
}

fn required_option<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// A decoder never guesses that an unknown client means an unchecked fixture. `None` is only
/// reproduced when the encoded payload explicitly had no client (host fixtures); a real worker
/// always supplies one. The mapping may bind a recorded id to a different process's instance id.
/// Preserve the request's token generation verbatim so stale-token refusal still runs on delivery.
#[cfg_attr(not(test), allow(dead_code))] // replay bootstrap/result injection is the next caller
pub(crate) fn decode(
    value: Value,
    mut client: impl FnMut(u32) -> Option<&'static crate::plex::Client>,
) -> Result<Landing, &'static str> {
    let p: Payload = serde_json::from_value(value).map_err(|_| "invalid hubs result")?;
    if p.kind != "hubs" || p.version != 1 { return Err("unsupported hubs result"); }
    let resolved = match p.client {
        Some(id) => {
            let c = client(id).ok_or("unresolved hubs client")?;
            if c.id() != p.sid { return Err("wrong hubs client server"); }
            Some(LandingClient { instance: id, resource: c })
        }
        None => None,
    };
    Ok(Landing { gen: p.gen, seq: p.seq, sid: p.sid, client: resolved,
        token_gen: p.token_gen, build: p.build })
}

pub(super) mod server_id {
    use super::*;
    pub fn serialize<S: serde::Serializer>(id: &crate::plex::ServerId, s: S) -> Result<S::Ok, S::Error> {
        id.raw().serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<crate::plex::ServerId, D::Error> {
        let n = u16::deserialize(d)?;
        if usize::from(n) >= crate::plex::MAX_SERVERS && n != u16::MAX {
            return Err(serde::de::Error::custom("invalid server slot"));
        }
        Ok(crate::plex::ServerId::from_raw(n))
    }
}

/// JSON floats cannot carry NaN payloads or guarantee negative-zero preservation. The payload
/// keeps the bits, just as canonical state does; encoding never silently turns a colour into null.
pub(super) mod blur_bits {
    use super::*;
    pub fn serialize<S: serde::Serializer>(v: &[[f32; 3]; 4], s: S) -> Result<S::Ok, S::Error> {
        v.map(|row| row.map(f32::to_bits)).serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[[f32; 3]; 4], D::Error> {
        Ok(<[[u32; 3]; 4]>::deserialize(d)?.map(|row| row.map(f32::from_bits)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pms::{CwItem, PmsMovie, Shelf};

    fn landing() -> Landing {
        // Exhaustive on purpose: adding a row field owes a codec/shape review, not an implicit
        // default in the only test claiming complete row preservation.
        let m = PmsMovie {
            sid: crate::plex::ServerId::from_raw(3), sec: 8, title: "Episode".into(), year: 2026,
            rating: "TV-14".into(), dur_ns: i64::MAX, part: "/part".into(), thumb: "/poster".into(),
            still: "/still".into(), art: "/art".into(), summary: "Summary".into(), rk: "17".into(),
            vcodec: "hevc".into(), acodec: "aac".into(),
            blur: [[-0.0, f32::from_bits(0x7fc01234), f32::INFINITY]; 4], has_blur: true,
            kind: 3, resume_ms: 12345, show_rk: "10".into(), season_index: 2,
            show_title: "Show".into(), ep_index: 4, unwatched: false, watched: true,
            aired: "2026-09-08".into(),
        };
        Landing { gen: 7, seq: 19, sid: m.sid, client: None, token_gen: 5,
            build: Some(SourceBuild {
                cw: vec![CwItem { last_viewed_at: i64::MAX, m: m.clone() }],
                shelves: vec![Shelf { title: "Shelf".into(), hub_id: "provider.hub".into(),
                    key: "/hub/key".into(), items: vec![m] }],
            }) }
    }

    #[test]
    fn the_complete_projection_round_trips_without_float_or_integer_loss() {
        let encoded = encode(&landing());
        let text = serde_json::to_string(&encoded).unwrap();
        let decoded = decode(serde_json::from_str(&text).unwrap(), |_| panic!("fixture has no client")).unwrap();
        assert_eq!(encode(&decoded), encoded);
        let m = &decoded.build.as_ref().unwrap().shelves[0].items[0];
        assert_eq!(m.blur[0].map(f32::to_bits), [0x80000000, 0x7fc01234, 0x7f800000]);
        assert_eq!(m.dur_ns, i64::MAX);
    }

    #[test]
    fn failure_empty_success_and_missing_payload_are_distinct() {
        let mut l = landing();
        l.build = None;
        let failed = encode(&l);
        assert!(decode(failed.clone(), |_| None).unwrap().build.is_none());
        l.build = Some(SourceBuild::default());
        let empty = encode(&l);
        assert_ne!(failed, empty);
        assert!(decode(empty, |_| None).unwrap().build.is_some());
        for key in ["client", "build", "token_gen", "seq"] {
            let mut absent = failed.clone();
            absent.as_object_mut().unwrap().remove(key);
            assert!(decode(absent, |_| None).is_err(), "missing {key} must not acquire a default");
        }
        for (key, value) in [("version", json!(2)), ("sid", json!(100)), ("unexpected", json!(1))] {
            let mut bad = failed.clone();
            bad[key] = value;
            assert!(decode(bad, |_| None).is_err());
        }
    }

    #[test]
    fn decoding_requires_the_exact_client_mapping_and_preserves_stale_token_tags() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("codec-machine", "codec.invalid", 32400, "secret-codec-token", "private-device");
        let old = crate::plex::client_for(sid).unwrap();
        let l = Landing { gen: 1, seq: 2, sid, client: Some(LandingClient::live(old)), token_gen: old.token_gen(), build: None };
        old.set_token("replacement-secret");
        let encoded = encode(&l);
        let text = encoded.to_string();
        for secret in ["codec-machine", "codec.invalid", "secret-codec-token", "private-device", "replacement-secret"] {
            assert!(!text.contains(secret));
        }
        assert!(decode(encoded.clone(), |_| None).is_err());
        let restored = decode(encoded.clone(), |id| (id == old.instance_gen()).then_some(old)).unwrap();
        assert!(std::ptr::eq(restored.client.unwrap().resource, old));
        assert_eq!(restored.token_gen, l.token_gen);
        assert_ne!(restored.token_gen, old.token_gen(), "do not upgrade a stale request while decoding");
        assert_eq!(crate::plex::register_for_test("codec-machine", "repointed.invalid", 32400,
            "repointed-secret", "private-device"), sid);
        let current = crate::plex::client_for(sid).unwrap();
        assert_ne!(current.instance_gen(), old.instance_gen());
        let stale = decode(encoded.clone(), |id| (id == old.instance_gen()).then_some(old)).unwrap();
        assert!(!std::ptr::eq(stale.client.unwrap().resource, current), "do not rebind a late arrival to the live slot");
        // A replay bootstrap may bind this recorded instance to another process's resource.
        // Re-encoding that binding must retain the recorded identity, not its new allocation id.
        let remapped = decode(encoded.clone(), |_| Some(current)).unwrap();
        assert_eq!(encode(&remapped), encoded);
        let other_sid = crate::plex::register_for_test("codec-other", "other.invalid", 32400, "other-secret", "private-device");
        assert!(decode(encoded, |_| crate::plex::client_for(other_sid)).is_err());
        crate::plex::reset_servers_for_test();
    }
}
