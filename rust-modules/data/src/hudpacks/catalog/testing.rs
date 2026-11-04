//! TEST ONLY signing keys and signed-index builders for the data layer's tests.
//!
//! The seeds are fixed constants, so every key here is public knowledge and none of them is the registry's:
//! [`ACCEPTED_KEYS`](super::ACCEPTED_KEYS) holds the contract's two keys, whose private halves do not exist in
//! this repository, and a test asserts that no key made here equals either.
use ed25519_compact::{KeyPair, Seed};
use plx_base::sha256::sha256;

use super::{AcceptedKey, KeyRole};

/// The seed of the TEST ONLY primary; `TEST_ONLY_STANDBY` is the standby's.
pub(crate) const TEST_ONLY_PRIMARY: u8 = 7;
pub(crate) const TEST_ONLY_STANDBY: u8 = 8;
/// A key no test accepts, to sign with a key the app does not know.
pub(crate) const TEST_ONLY_STRANGER: u8 = 9;

pub(crate) fn pair(seed: u8) -> KeyPair {
    KeyPair::from_seed(Seed::new([seed; 32]))
}

pub(crate) fn accepted(role: KeyRole, seed: u8) -> AcceptedKey {
    let key: [u8; 32] = *pair(seed).pk;
    let mut id = [0u8; 8];
    id.copy_from_slice(&sha256(&key)[..8]);
    AcceptedKey { role, id, key }
}

/// The key set tests pass where production passes `ACCEPTED_KEYS`: a primary and a standby.
pub(crate) fn keys() -> [AcceptedKey; 2] {
    [accepted(KeyRole::Primary, TEST_ONLY_PRIMARY), accepted(KeyRole::Standby, TEST_ONLY_STANDBY)]
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The contract's header line, then the newline.
pub(crate) fn header(id: &[u8; 8], signature: &[u8]) -> String {
    format!("{{\"format\":1,\"key\":\"{}\",\"sig\":\"{}\"}}\n", hex(id), hex(signature))
}

/// `index.signed` for `index`, signed by the key made from `seed`.
pub(crate) fn sign_with(seed: u8, index: &[u8]) -> Vec<u8> {
    let signer = accepted(KeyRole::Primary, seed);
    let signature = pair(seed).sk.sign(index, None);
    let mut out = header(&signer.id, signature.as_ref()).into_bytes();
    out.extend_from_slice(index);
    out
}

/// `index.signed` for `index`, signed by the TEST ONLY primary.
pub(crate) fn sign(index: &[u8]) -> Vec<u8> {
    sign_with(TEST_ONLY_PRIMARY, index)
}
