//! FinNative: the disk-cache identity of an alternative backend's image request. Upstream's
//! `imgcache::classify` knows only Plex's `/photo/:/transcode`, so Jellyfin's
//! `/Items/{id}/Images/{kind}?tag=..&fillWidth=..` would never reach the disk tier. The request
//! carries no credential (the token travels in a header), and the image tag in it changes when the
//! artwork does, so the whole request is the identity.

use plx_platform::imgcache::{classify_baked, DiskKey};

const KIND: &str = "alt-image";

pub(super) fn disk_key(namespace: &str, built_key: &str) -> Option<DiskKey> {
    if !plx_plex::plex::alt_installed() {
        return None;
    }
    key_of(namespace, built_key)
}

fn key_of(namespace: &str, built_key: &str) -> Option<DiskKey> {
    let (path, query) = built_key.split_once('?').unwrap_or((built_key, "-"));
    if !path.contains("/Images/") {
        return None;
    }
    classify_baked(namespace, KIND, path, query, 1, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &str = "machine:abc";

    #[test]
    fn an_image_request_is_keyed_by_its_whole_request() {
        let a = key_of(NS, "/Items/i1/Images/Primary?quality=90&tag=t1&fillWidth=300");
        assert!(a.is_some());
        assert_eq!(a, key_of(NS, "/Items/i1/Images/Primary?quality=90&tag=t1&fillWidth=300"));
        assert_ne!(a, key_of(NS, "/Items/i1/Images/Primary?quality=90&tag=t2&fillWidth=300"), "new tag");
        assert_ne!(a, key_of(NS, "/Items/i1/Images/Primary?quality=90&tag=t1&fillWidth=600"), "new size");
        assert_ne!(a, key_of("machine:other", "/Items/i1/Images/Primary?quality=90&tag=t1&fillWidth=300"));
    }

    #[test]
    fn only_image_requests_get_a_key() {
        assert!(key_of(NS, "/Videos/v/Trickplay/320/0.jpg").is_none());
        assert!(key_of(NS, "").is_none());
        assert!(key_of(NS, "/Items/i1/Images/Backdrop").is_some(), "a request without a query");
    }
}
