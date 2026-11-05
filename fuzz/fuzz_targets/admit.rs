#![no_main]
//! `plx_appkit::hud_format::admit::admit`: the television's whole admission path over a package's bytes
//! (archive, manifest, strict JSON, the descriptor walk, the SVG masks, reachability and the
//! package-level invariants), then the version gate.
//!
//! Asserts what `admit.rs` promises: it is pure (same bytes and version, same verdict); nothing over
//! the artifact bound is anything but `Invalid`; and every id and name it hands the list is safe to
//! list, whichever way the package was judged: a reverse-DNS id (`valid_package_id`; "a hostile file
//! cannot put anything else in the list") and a name of at most 64 characters with no control
//! character. The app version is one of a few, picked by the input's length, so the `app.min` /
//! `app.max` comparisons and the unparseable-version arm are reached from the same corpus.
use libfuzzer_sys::{fuzz_mutator, fuzz_target};
use plx_appkit::hud_format::admit::{admit, Admission, Facts};
use plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES;
use plx_data::hudpacks::valid_package_id;

const APP_VERSIONS: [&str; 5] = ["0.9.0", "0.10.0-dev", "0.0.1", "1.2.3+build", "not-a-version"];

fn listable(facts: &Facts) {
    assert!(valid_package_id(&facts.id), "id {:?} is not a reverse-DNS id", facts.id);
    assert!(facts.name.chars().count() <= 64, "name of {} characters", facts.name.chars().count());
    assert!(!facts.name.chars().any(char::is_control), "control character in name {:?}", facts.name);
}

fuzz_target!(|data: &[u8]| {
    let app = APP_VERSIONS[data.len() % APP_VERSIONS.len()];
    let verdict = admit(data, app);
    match &verdict {
        Admission::Runs(facts) | Admission::Cannot(facts, _) => {
            assert!(data.len() <= MAX_ARTIFACT_BYTES, "an over-bound artifact was not refused");
            listable(facts);
            assert_eq!(verdict, admit(data, app), "admit is pure");
        }
        Admission::Invalid => {}
    }
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    plxnative_fuzz::mutate_bundle(data, size, max_size, seed)
});
