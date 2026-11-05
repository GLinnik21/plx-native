#![no_main]
//! `plx_data::hudpacks::catalog::verify` and `Reply::http`: the signed registry index (`index.signed`), the one
//! door a network reply to the gallery passes (PR14). The body comes from a host the app does not control the
//! content of, and the gallery accepts it only with a valid Ed25519 signature by a key compiled into the app.
//!
//! The invariant against the shipped keys (`ACCEPTED_KEYS`), for ANY bytes: no panic, and never accepted.
//! Arbitrary bytes cannot carry a valid signature by those keys, so every input must come back a `Refusal`, and
//! one of the three that precede the signature check's success (`Malformed`, `UnknownKey`, `BadSignature`):
//! `Index` and `NoSerial` are what follows a verified signature, and a header with no newline in its first
//! `HEADER_BYTE_LIMIT + 1` bytes is `Malformed`. `Reply::http` is `verify` on a 200's body; its result is
//! private, so its `Debug` text stands in for it.
//!
//! What follows the signature check is reached with TEST ONLY keys (`plxnative_fuzz::signing`, the data
//! layer's own test seeds; none is the registry's): the fuzzer's bytes are signed as an index, and `verify` must
//! say exactly what the contract's later steps say (`signing::verdict`): the index parses, carries a `serial`
//! and the whole body is text. A key the set does not hold is `UnknownKey`, a signature with a bit flipped
//! `BadSignature`, and the standby key verifies like the primary.
use libfuzzer_sys::fuzz_target;
use plx_data::hudpacks::catalog::{parse, verify, Refusal, Reply, ACCEPTED_KEYS, HEADER_BYTE_LIMIT, INDEX_BYTE_LIMIT};
use plxnative_fuzz::signing;

fn debug_accepts(reply: &Reply) -> bool {
    format!("{reply:?}").contains("Verified")
}

fuzz_target!(|data: &[u8]| {
    // The shipped keys: nothing is accepted.
    match verify(data, &ACCEPTED_KEYS) {
        Ok(verified) => panic!("{} bytes accepted under the shipped keys (serial {})", data.len(), verified.serial),
        Err(refusal) => {
            assert!(matches!(refusal, Refusal::Malformed | Refusal::UnknownKey | Refusal::BadSignature), "{refusal:?}");
            assert!(!refusal.cause().is_empty());
            if !data[..data.len().min(HEADER_BYTE_LIMIT + 1)].contains(&b'\n') {
                assert_eq!(refusal, Refusal::Malformed, "a header line with no newline");
            }
        }
    }
    for status in [200u16, 204, 304, 404, 500] {
        let reply = Reply::http(status, Some("\"etag\"".to_owned()), data, &ACCEPTED_KEYS);
        assert!(!debug_accepts(&reply), "a reply accepted {} bytes under the shipped keys: {reply:?}", data.len());
    }

    // The TEST ONLY keys, on the bytes as given: a body that happens to verify is the whole body.
    if let Ok(verified) = verify(data, signing::keys()) {
        assert_eq!(verified.body.as_bytes(), data);
        assert_eq!(verified.index.serial, Some(verified.serial));
    }

    // The TEST ONLY keys, on the bytes signed as an index: what comes after the signature check.
    let want = signing::verdict(data);
    if !signing::worth_signing(data, &want) {
        return;
    }
    let signed = signing::sign_with(signing::PRIMARY, data);
    let got = verify(&signed, signing::keys());
    match (&got, &want) {
        (Ok(verified), Ok(serial)) => {
            assert_eq!(verified.serial, *serial);
            assert_eq!(verified.body.as_bytes(), &signed[..]);
            assert_eq!(Some(&*verified.index), parse(data).as_ref());
            assert!(debug_accepts(&Reply::http(200, None, &signed, signing::keys())));
            assert!(!debug_accepts(&Reply::http(304, None, &signed, signing::keys())));
        }
        (Err(got), Err(want)) => assert_eq!(got, want),
        _ => panic!("verify said {got:?} for an index the contract answers {want:?}"),
    }

    // The key rules, on a quarter of those (each probe signs once).
    if data.len() <= INDEX_BYTE_LIMIT && data.iter().map(|&b| u32::from(b)).sum::<u32>() % 4 == 0 {
        let stranger = verify(&signing::sign_with(signing::STRANGER, data), signing::keys());
        assert_eq!(stranger.err(), Some(Refusal::UnknownKey));
        let tampered = verify(&signing::sign_tampered(signing::PRIMARY, data), signing::keys());
        assert_eq!(tampered.err(), Some(Refusal::BadSignature));
        let standby = verify(&signing::sign_with(signing::STANDBY, data), signing::keys());
        assert_eq!(standby.map(|v| v.serial), got.map(|v| v.serial), "the standby verifies like the primary");
    }
});
