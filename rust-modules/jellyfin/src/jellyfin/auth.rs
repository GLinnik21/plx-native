//! The `Authorization` header Jellyfin expects on every request.
//!
//! ```text
//! Authorization: MediaBrowser Client="FinNative", Device="LG webOS TV", DeviceId="…", Version="0.1.0", Token="…"
//! ```
//!
//! `Client`, `Device` and `DeviceId` are how the server lists this client under Dashboard → Devices
//! and Activity, and `DeviceId` is what ties a Quick Connect request and a playback session to this
//! install, so it must stay the same across launches. `Token` is absent until a user signs in.

/// The identity this install presents to a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIdentity {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
}

impl ClientIdentity {
    /// The header value, with `token` when signed in.
    pub fn header(&self, token: Option<&str>) -> String {
        let mut h = format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
            quote(&self.client),
            quote(&self.device),
            quote(&self.device_id),
            quote(&self.version),
        );
        if let Some(t) = token.filter(|t| !t.is_empty()) {
            h.push_str(&format!(", Token=\"{}\"", quote(t)));
        }
        h
    }
}

/// A header parameter value: the server reads each one between double quotes and URL-decodes it,
/// so a quote, comma or non-ASCII character in a device name is percent-encoded.
fn quote(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            0x20..=0x7e if !matches!(b, b'"' | b',' | b'%' | b'=') => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ClientIdentity {
        ClientIdentity {
            client: "FinNative".into(),
            device: "LG webOS TV".into(),
            device_id: "abc123".into(),
            version: "0.1.0".into(),
        }
    }

    #[test]
    fn header_without_and_with_a_token() {
        assert_eq!(
            id().header(None),
            r#"MediaBrowser Client="FinNative", Device="LG webOS TV", DeviceId="abc123", Version="0.1.0""#
        );
        assert!(id().header(Some("tok")).ends_with(r#", Token="tok""#));
        assert!(!id().header(Some("")).contains("Token"), "an empty token is no token");
    }

    #[test]
    fn values_that_would_break_the_header_are_encoded() {
        let mut i = id();
        i.device = "Living Room TV".into();
        let h = i.header(None);
        assert!(h.contains(r#"Device="Living Room TV""#), "{h}");
    }
}
