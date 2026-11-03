//! Bounded, data-only HUD bundles. The archive profile and all package decisions are pure.

pub mod archive;

/// Package profiles and HUD schemas evolve independently.
pub const CURRENT: (u8, u8) = (1, 0);
/// Maximum complete transport artifact for package profile 1.0.
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum sum of all member payloads for package profile 1.0.
pub const MAX_MEMBER_BYTES: usize = 1024 * 1024;
/// Maximum manifest source length.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum central-directory metadata, checked before constructing the ZIP reader.
pub const MAX_DIRECTORY_BYTES: usize = 64 * 1024;
/// Maximum regular-file entries in a bundle, including manifest, HUD and notices.
pub const MAX_ENTRIES: usize = 128;
/// Maximum bytes in a portable package-root-relative path.
pub const MAX_PATH_BYTES: usize = 128;
/// Maximum path segments, including the filename.
pub const MAX_PATH_DEPTH: usize = 8;
/// One allocation budget shared by the manifest and HUD JSON trees.
pub const JSON_DECODE_BYTES: usize = 16 * 1024 * 1024;

/// Portable package path. Every segment starts with a lower-case ASCII letter or digit, then
/// contains only lower-case ASCII letters, digits, dots, underscores and hyphens.
pub fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return false;
    }
    let mut depth = 0;
    for segment in path.split('/') {
        depth += 1;
        let first = segment.as_bytes().first().copied();
        if depth > MAX_PATH_DEPTH
            || !first.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            || segment.ends_with('.')
            || !segment.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
        {
            return false;
        }
        let base = segment.split('.').next().unwrap_or("");
        if matches!(base, "con" | "prn" | "aux" | "nul")
            || base
                .strip_prefix("com")
                .or_else(|| base.strip_prefix("lpt"))
                .is_some_and(|suffix| {
                    suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
                })
        {
            return false;
        }
    }
    true
}
