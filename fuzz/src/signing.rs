//! TEST ONLY signing keys for the registry-index targets.
//!
//! Arbitrary bytes cannot carry a valid signature by the keys the app ships (`ACCEPTED_KEYS`), so a target
//! that only feeds those keys never gets past the signature check: everything after it (the index parser, the
//! `serial` rule, the whole-body UTF-8 rule, the cache file's re-verification) would go unfuzzed. These keys
//! let a target sign what the fuzzer wrote and look past that door.
//!
//! The seeds are the fixed constants `rust-modules/data/src/hudpacks/catalog/testing.rs` uses for the data
//! layer's own tests (that module is `cfg(test)` and `pub(crate)`, so it is mirrored here): every key made
//! here is public knowledge and none of them is the registry's, whose private halves exist nowhere in this
//! repository. [`keys`] asserts that no key made here equals one of `ACCEPTED_KEYS`; no private key is
//! committed, printed or read by anything in this package.

use std::sync::OnceLock;

use ed25519_compact::{KeyPair, Seed};
use plx_base::sha256::sha256;
use plx_data::hudpacks::catalog::{parse, AcceptedKey, KeyRole, Refusal, ACCEPTED_KEYS, INDEX_BYTE_LIMIT};

/// The seed of the TEST ONLY primary; [`STANDBY`] is the standby's, [`STRANGER`] a key no set accepts.
pub const PRIMARY: u8 = 7;
pub const STANDBY: u8 = 8;
pub const STRANGER: u8 = 9;

fn pair(seed: u8) -> &'static KeyPair {
    static PAIRS: OnceLock<[KeyPair; 3]> = OnceLock::new();
    let pairs = PAIRS.get_or_init(|| [PRIMARY, STANDBY, STRANGER].map(|s| KeyPair::from_seed(Seed::new([s; 32]))));
    match seed {
        PRIMARY => &pairs[0],
        STANDBY => &pairs[1],
        STRANGER => &pairs[2],
        _ => panic!("no TEST ONLY key with seed {seed}"),
    }
}

fn accepted(role: KeyRole, seed: u8) -> AcceptedKey {
    let key: [u8; 32] = *pair(seed).pk;
    let mut id = [0u8; 8];
    id.copy_from_slice(&sha256(&key)[..8]);
    AcceptedKey { role, id, key }
}

/// The key set a target passes where production passes `ACCEPTED_KEYS`: a primary and a standby.
pub fn keys() -> &'static [AcceptedKey; 2] {
    static KEYS: OnceLock<[AcceptedKey; 2]> = OnceLock::new();
    KEYS.get_or_init(|| {
        let keys = [accepted(KeyRole::Primary, PRIMARY), accepted(KeyRole::Standby, STANDBY)];
        for made in keys.iter().chain([&accepted(KeyRole::Primary, STRANGER)]) {
            assert!(ACCEPTED_KEYS.iter().all(|real| real.key != made.key && real.id != made.id));
        }
        keys
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `index.signed` for `index` signed by the key made from `seed`: the contract's header line, a newline, the
/// index. `tamper` flips one bit of the signature first.
fn sign_inner(seed: u8, index: &[u8], tamper: bool) -> Vec<u8> {
    let signer = accepted(KeyRole::Primary, seed);
    let mut signature: [u8; 64] = *pair(seed).sk.sign(index, None);
    if tamper {
        signature[0] ^= 1;
    }
    let mut out = format!("{{\"format\":1,\"key\":\"{}\",\"sig\":\"{}\"}}\n", hex(&signer.id), hex(&signature)).into_bytes();
    out.extend_from_slice(index);
    out
}

/// `index.signed` for `index`, signed by the key made from `seed`.
pub fn sign_with(seed: u8, index: &[u8]) -> Vec<u8> {
    sign_inner(seed, index, false)
}

/// The same with the first signature bit flipped: the header is well formed and names an accepted key, but
/// the signature cannot verify.
pub fn sign_tampered(seed: u8, index: &[u8]) -> Vec<u8> {
    sign_inner(seed, index, true)
}

/// What `verify` must say of a body whose header is well formed and names an accepted key and whose signature
/// verifies over `index` (a body made by [`sign_with`] with one of [`keys`]): the contract's steps after the
/// signature, in order. The serial it returns is the one `verify` reports. Written from the contract in
/// `catalog.rs`'s module doc, not from `verify`'s code, so the two can disagree.
///
/// Note the last step: `parse` reads with `serde_json::from_slice`, which does not check the UTF-8 of a string
/// it ignores (a field the index type does not name), so an index can parse and still not be text; the signed
/// body must be, and `verify` refuses it as malformed.
pub fn verdict(index: &[u8]) -> Result<u64, Refusal> {
    if index.len() > INDEX_BYTE_LIMIT {
        return Err(Refusal::Malformed);
    }
    let serial = parse(index).ok_or(Refusal::Index)?.serial.ok_or(Refusal::NoSerial)?;
    std::str::from_utf8(index).map_err(|_| Refusal::Malformed)?;
    Ok(serial)
}

/// Whether to spend a signature on `index`, given its [`verdict`]. Always unless it is "not an index", the common
/// outcome of a mutated input, which is then checked on one input in eight: a signature costs about a millisecond
/// with this build's size-optimised Ed25519, ten times a parse, and the deep outcomes are the rare ones.
pub fn worth_signing(index: &[u8], verdict: &Result<u64, Refusal>) -> bool {
    *verdict != Err(Refusal::Index) || index.iter().map(|&b| u32::from(b)).sum::<u32>() % 8 == 0
}
