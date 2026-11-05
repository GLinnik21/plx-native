#![no_main]
//! `hud_format::bundle::read`: the 1.0 ZIP bundle, the most exposed parser (any file from a drop folder,
//! an address or the registry reaches it). Asserts what `Decoded` promises:
//!
//! * the digest is computed before anything is parsed and is `sha256` of exactly the bytes given, and
//!   an over-bound artifact never gets one;
//! * a package exists only when no finding is an error, and then it carries that digest and its
//!   manifest; a refusal always says why (an error finding);
//! * every finding has the lint shape (`plxnative_fuzz::lint_shaped`);
//! * an `expected` digest that differs refuses the artifact with the digest-mismatch finding alone, and
//!   the right digest changes nothing.
use libfuzzer_sys::{fuzz_mutator, fuzz_target};
use plx_appkit::hud_format::bundle::{read, MAX_ARTIFACT_BYTES};
use plx_appkit::hud_format::{codes, has_error};
use plx_base::sha256::sha256;

fuzz_target!(|data: &[u8]| {
    let decoded = read(data, None);
    for finding in &decoded.findings {
        plxnative_fuzz::lint_shaped(finding);
    }
    if data.len() > MAX_ARTIFACT_BYTES {
        assert!(decoded.sha256.is_none() && decoded.package.is_none() && !decoded.findings.is_empty());
        return;
    }
    let digest = sha256(data);
    assert_eq!(decoded.sha256, Some(digest), "the digest is of the bytes given");
    match &decoded.package {
        Some(package) => {
            assert!(!has_error(&decoded.findings), "a package with an error finding");
            assert_eq!(package.sha256(), digest);
            assert_eq!(decoded.manifest.as_ref(), Some(package.manifest()));
        }
        None => assert!(has_error(&decoded.findings), "a refusal without an error finding"),
    }
    let mut other = digest;
    other[0] ^= 1;
    let mismatch = read(data, Some(other));
    assert!(mismatch.package.is_none());
    assert_eq!(mismatch.findings.len(), 1);
    assert_eq!(mismatch.findings[0].code, codes::E1603.id);
    if decoded.package.is_some() {
        let same = read(data, Some(digest));
        assert!(same.package.is_some());
        assert_eq!(same.findings, decoded.findings);
    }
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    plxnative_fuzz::mutate_bundle(data, size, max_size, seed)
});
