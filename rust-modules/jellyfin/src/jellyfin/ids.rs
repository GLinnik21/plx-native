//! Small integer keys for Jellyfin's GUID library ids.
//!
//! The app's section APIs (`library_hubs(section_key: i64, ..)`, `LibrarySection::key`,
//! `Metadata::library_section_id`) were written against Plex, whose library sections are numbered
//! `1`, `2`, `3`. A Jellyfin library is a view with a 32-hex-digit GUID. This table hands each GUID
//! a stable positive number the first time it is seen and answers both directions after that, so
//! the layers above keep their `i64` keys and this layer turns them back into GUIDs on the way out.
//!
//! A number is stable for the life of the table (one signed-in server for one run of the app); it is
//! never persisted, because anything the app persists about a section is keyed by the server's own
//! id through [`SectionIds::guid`], not by this number.

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct SectionIds {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    by_guid: HashMap<String, i64>,
    by_key: Vec<String>, // key N is by_key[N - 1]
}

impl SectionIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// The key for `guid`, assigning the next one on first sight. GUIDs compare case-insensitively
    /// and without dashes, because Jellyfin spells the same id both ways across endpoints.
    pub fn key(&self, guid: &str) -> i64 {
        let norm = normalize(guid);
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(&k) = g.by_guid.get(&norm) {
            return k;
        }
        g.by_key.push(norm.clone());
        let k = g.by_key.len() as i64;
        g.by_guid.insert(norm, k);
        k
    }

    /// The GUID a key was handed for, or `None` for a key this table never issued.
    pub fn guid(&self, key: i64) -> Option<String> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        usize::try_from(key).ok().and_then(|k| k.checked_sub(1)).and_then(|i| g.by_key.get(i).cloned())
    }
}

/// A Jellyfin GUID in its canonical form: lowercase hex, no dashes.
pub fn normalize(guid: &str) -> String {
    guid.chars().filter(|c| *c != '-').map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_stable_and_reversible() {
        let ids = SectionIds::new();
        let a = ids.key("f137a2dd21bbc1b99aa5c0f6bf02a805");
        let b = ids.key("9d7ad6afe9afa2dab1a2f6e00ad28fa6");
        assert_eq!((a, b), (1, 2));
        assert_eq!(ids.key("F137A2DD-21BB-C1B9-9AA5-C0F6BF02A805"), 1, "dashes and case are the same id");
        assert_eq!(ids.guid(2).as_deref(), Some("9d7ad6afe9afa2dab1a2f6e00ad28fa6"));
        assert_eq!(ids.guid(0), None);
        assert_eq!(ids.guid(3), None);
        assert_eq!(ids.guid(-1), None);
    }
}
