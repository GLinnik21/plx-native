#![no_main]
//! `hud_format::bundle::archive::read`: the canonical STORE-only ZIP32 profile, a bounded structural
//! pass and then the `zip` crate. Asserts what the profile promises of an archive it opens: at most
//! `MAX_ENTRIES` members, each at most `MAX_MEMBER_BYTES`, every path a valid package path and no path
//! twice; and what it promises of a refusal: a member is named only after its path passed the portable
//! grammar. Whatever opens also packs again when it fits the writer's budget, and the repacked archive
//! opens to the same members: the writer and the reader agree on the profile.
use libfuzzer_sys::{fuzz_mutator, fuzz_target};
use plx_appkit::hud_format::bundle::{archive, valid_path, MAX_ENTRIES, MAX_MEMBER_BYTES};
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    match archive::read(data) {
        Ok(files) => {
            assert!(!files.is_empty() && files.len() <= MAX_ENTRIES, "{} members", files.len());
            let mut seen = BTreeSet::new();
            for file in &files {
                assert!(file.bytes.len() <= MAX_MEMBER_BYTES);
                assert!(valid_path(&file.path), "member path {:?}", file.path);
                assert!(seen.insert(file.path.clone()), "member {:?} twice", file.path);
            }
            if let Ok(packed) = archive::pack(&files) {
                let mut sorted = files.clone();
                sorted.sort_by(|a, b| a.path.cmp(&b.path));
                assert_eq!(archive::read(&packed), Ok(sorted), "pack then read changed the members");
            }
        }
        Err(error) => {
            assert!(!error.message.is_empty());
            if let Some(member) = &error.member {
                assert!(valid_path(member), "an error names {member:?}, which is not a package path");
            }
        }
    }
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    plxnative_fuzz::mutate_bundle(data, size, max_size, seed)
});
