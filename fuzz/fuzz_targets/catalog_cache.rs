#![no_main]
//! `plx_data::hudpacks::catalog::CacheRecord::decode`: the registry cache file the app reads at boot (the
//! last index, its validators and the times it was fetched and attempted), read from disk on the storage
//! worker. It wraps the index as an escaped string inside a JSON object and runs the index parser on it,
//! so the fuzzer sees the same parser through a second door. Asserts that a record that decodes encodes
//! back to bytes within the cache bound that decode to the same record.
use libfuzzer_sys::fuzz_target;
use plxnative_fuzz as _; // links the host libraries `build.rs` asks for; see `src/lib.rs`
use plx_data::hudpacks::catalog::{CacheRecord, CACHE_BYTE_LIMIT};

fuzz_target!(|data: &[u8]| {
    let Some(record) = CacheRecord::decode(data) else { return };
    assert!(data.len() <= CACHE_BYTE_LIMIT);
    let encoded = record.encode();
    assert!(encoded.len() <= CACHE_BYTE_LIMIT, "{} bytes", encoded.len());
    assert_eq!(CacheRecord::decode(&encoded), Some(record), "encode then decode changed the record");
});
