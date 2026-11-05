#![no_main]
//! `plx_data::hudpacks::catalog::parse`: the registry index, fetched over HTTPS from a host the app
//! does not control the content of. Asserts what the parser promises of an index it accepts ("reject a
//! complete malformed index; never accept a prefix or follow a URL supplied by an author"):
//!
//! * the format is 1 and the bytes are within `INDEX_BYTE_LIMIT`;
//! * every id is a valid package id and no id repeats; every hash, in an entry or a revocation, is 64
//!   lowercase hex digits and no revoked hash repeats (so `Package::sha`, which `expect`s a checked
//!   hash, cannot panic);
//! * every URL is the content-addressed one under `ORIGIN` the hash dictates, never one the author chose;
//! * the accessors a gallery row calls (`fits`, `compatible`, `preview`) answer for any app version
//!   and language without panicking, and a package with previews has one to show;
//! * serialising the accepted index and parsing that gives the same index back.
use libfuzzer_sys::fuzz_target;
use plxnative_fuzz as _; // links the host libraries `build.rs` asks for; see `src/lib.rs`
use plx_data::hudpacks::catalog::{parse, INDEX_BYTE_LIMIT, ORIGIN};
use plx_data::hudpacks::{valid_package_id, Sha};
use std::collections::BTreeSet;

fuzz_target!(|data: &[u8]| {
    let Some(index) = parse(data) else { return };
    assert!(data.len() <= INDEX_BYTE_LIMIT);
    assert_eq!(index.format, 1);
    let mut revoked = BTreeSet::new();
    for r in &index.revoked {
        assert!(Sha::parse(&r.sha256).is_some(), "revoked hash {:?}", r.sha256);
        assert!(revoked.insert(&r.sha256), "revoked twice: {:?}", r.sha256);
    }
    let mut ids = BTreeSet::new();
    for p in &index.entries {
        assert!(valid_package_id(&p.id) && ids.insert(&p.id), "id {:?}", p.id);
        assert_eq!(p.sha().hex(), p.sha256, "hash {:?}", p.sha256);
        assert_eq!(p.url, format!("{ORIGIN}/p/{}.plxhud", p.sha256));
        assert!(p.bytes > 0 && !p.previews.is_empty());
        for art in &p.previews {
            assert_eq!(art.url, format!("{ORIGIN}/previews/{}/{}", p.sha256, art.file));
            assert_eq!(art.file, format!("{}.png", art.sha256));
        }
        for (app, lang) in [("0.9.0", "en"), ("0.0.1", "de"), ("99.0.0-dev", "x"), ("", "")] {
            let _ = (p.fits(app, lang), p.compatible(app, lang, usize::MAX));
            assert!(p.preview(lang).is_some(), "an entry with previews has none to show");
        }
    }
    let again = serde_json::to_vec(&index).expect("an accepted index serialises");
    assert_eq!(parse(&again), Some(index), "serialise then parse changed the index");
});
