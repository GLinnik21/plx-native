#![no_main]
//! `plx_data::hudpacks::catalog::CacheRecord::decode`: the registry cache file the app reads at boot (the
//! last signed index, its validators and the times it was fetched and attempted), read from disk on the
//! storage worker. A file anyone could have written is read like a reply: its body must be a whole
//! `index.signed` and is verified again with the same function (PR14), so a PR13-era file holding a bare
//! index, or any forged one, is refused, and with it the ETag it carried.
//!
//! Against the shipped keys (`ACCEPTED_KEYS`), for ANY bytes: no panic, and nothing is accepted that carries a
//! body. The one record that decodes is the one that remembers only an attempt (no body, never validated: it
//! has nothing to verify), so a record that decodes must encode with an empty body; every refusal is one of the
//! three that precede a verified signature. A record that decodes encodes within `CACHE_BYTE_LIMIT` and
//! decodes back to the same record.
//!
//! What follows the signature check is reached with TEST ONLY keys (`plxnative_fuzz::signing`): the bytes as
//! given may be a cache file that verifies, and the fuzzer's bytes are signed as an index, wrapped as a cache
//! file and decoded, which must say exactly what the contract's later steps say (`signing::verdict`).
use libfuzzer_sys::fuzz_target;
use plx_data::hudpacks::catalog::{CacheRecord, Refusal, ACCEPTED_KEYS, CACHE_BYTE_LIMIT};
use plxnative_fuzz::signing;
use serde_json::{json, Value};

/// A record decodes back from what it encodes, and what it encodes is within the bound.
fn round_trips(record: CacheRecord, keys: &[plx_data::hudpacks::catalog::AcceptedKey]) -> Vec<u8> {
    let encoded = record.encode();
    assert!(encoded.len() <= CACHE_BYTE_LIMIT, "{} bytes", encoded.len());
    assert_eq!(CacheRecord::decode(&encoded, keys), Ok(record), "encode then decode changed the record");
    encoded
}

fuzz_target!(|data: &[u8]| {
    // The shipped keys.
    match CacheRecord::decode(data, &ACCEPTED_KEYS) {
        Ok(record) => {
            assert!(data.len() <= CACHE_BYTE_LIMIT);
            let encoded = round_trips(record, &ACCEPTED_KEYS);
            let value: Value = serde_json::from_slice(&encoded).expect("a record encodes as JSON");
            assert_eq!(value["body"], "", "a record with a body was accepted under the shipped keys");
        }
        Err(refusal) => {
            assert!(matches!(refusal, Refusal::Malformed | Refusal::UnknownKey | Refusal::BadSignature), "{refusal:?}");
        }
    }

    // The TEST ONLY keys on the bytes as given (a seed may be a cache file that verifies).
    if let Ok(record) = CacheRecord::decode(data, signing::keys()) {
        round_trips(record, signing::keys());
    }

    // The TEST ONLY keys on the bytes signed as an index and wrapped as a cache file. A signed body that is not
    // text cannot be written into the file's JSON string, so there is nothing to decode.
    let want = signing::verdict(data);
    if !signing::worth_signing(data, &want) {
        return;
    }
    let signed = signing::sign_with(signing::PRIMARY, data);
    let Ok(body) = std::str::from_utf8(&signed) else { return };
    let fields = u64::from(data.first().copied().unwrap_or(0));
    let file = json!({
        "protocol": 1,
        "checked_at": (fields % 3 != 0).then_some(1_700_000_000 + fields),
        "attempted_at": (fields % 5 != 0).then_some(1_700_000_100 + fields),
        "etag": String::from_utf8_lossy(&data[..data.len().min(8)]),
        "body": body,
    });
    let file = serde_json::to_vec(&file).expect("a JSON value serialises");
    match (CacheRecord::decode(&file, signing::keys()), want) {
        (Ok(record), Ok(_)) => {
            round_trips(record, signing::keys());
        }
        (Err(got), Err(want)) => assert_eq!(got, want),
        (got, want) => panic!("decode said {got:?} for an index the contract answers {want:?}"),
    }
});
