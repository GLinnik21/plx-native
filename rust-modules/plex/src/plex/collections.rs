//! Collection listings, detail and membership reads.
//!
//! PMS has two collection id spaces: collection metadata uses a `ratingKey`, while member tags
//! use the collection's `index`. The helpers here keep those identities explicit and centralize
//! the live-observed joins between collection rows, hubs and member `Collection[]` tags.

use super::client::{Client, JsonStatusOutcome};
use super::models::{MediaContainer, Metadata};
use super::paging::{paged_path, PageReq};
use super::ServerId;

/// One collection as a link names it: by `rk` (its ratingKey) when the link carries one, else by
/// `sec` + `tag` (the member-tag id space) for the collection store to resolve against the
/// section's collection listing. `name` is the title shown until the collection's own metadata
/// lands — it is never identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionRef {
    pub sid: ServerId,
    pub rk: String,
    pub sec: i64,
    pub tag: i64,
    pub name: String,
}

impl CollectionRef {
    /// A link that knows the collection's ratingKey (a promoted shelf, a library tile).
    pub fn by_rk(sid: ServerId, rk: &str, sec: i64, name: &str) -> Self {
        Self { sid, rk: rk.to_owned(), sec, tag: 0, name: name.to_owned() }
    }

    /// A link that knows only the collection's tag id within a section (a member's
    /// `collection.related` hub, a tag-shaped search hit).
    pub fn by_tag(sid: ServerId, sec: i64, tag: i64, name: &str) -> Self {
        Self { sid, rk: String::new(), sec, tag, name: name.to_owned() }
    }

    /// THE collection identity rule. Two ratingKeys compare when both sides carry one; otherwise
    /// the server, section and a non-zero tag id must all agree. A ratingKey is never compared
    /// with a tag id, and a zero tag names nothing.
    pub fn same_collection(&self, other: &Self) -> bool {
        if self.sid != other.sid {
            return false;
        }
        if !self.rk.is_empty() && !other.rk.is_empty() {
            self.rk == other.rk
        } else {
            self.sec == other.sec && self.tag != 0 && self.tag == other.tag
        }
    }

    /// Names no collection at all — neither a ratingKey nor a tag to resolve.
    pub fn is_identityless(&self) -> bool {
        self.rk.is_empty() && self.tag == 0
    }
}

/// A collection read preserves the server answers that collection UI must present distinctly.
/// Every other failure — no response, an unexpected status, a malformed 2xx body — is one
/// retryable `Transport`: the page presents them identically.
pub enum CollectionOutcome {
    Ok(MediaContainer),
    Denied,
    Missing,
    Transport,
}

impl Client {
    /// `GET /library/sections/{section}/collections`, paged with both required parameters.
    pub fn section_collections(
        &self,
        section: i64,
        start: i64,
        size: i64,
    ) -> CollectionOutcome {
        let path = paged_path(
            &format!("/library/sections/{section}/collections"),
            PageReq::window(start, size),
        );
        self.collection_get(&path)
    }

    /// `GET /library/metadata/{ratingKey}` for one collection's metadata.
    pub fn collection(&self, rating_key: &str) -> CollectionOutcome {
        self.collection_get(&format!("/library/metadata/{rating_key}"))
    }

    /// `GET /library/collections/{ratingKey}/children`, paged with both required parameters.
    pub fn collection_children(
        &self,
        rating_key: &str,
        start: i64,
        size: i64,
    ) -> CollectionOutcome {
        let path = paged_path(
            &format!("/library/collections/{rating_key}/children"),
            PageReq::window(start, size),
        );
        self.collection_get(&path)
    }

    fn collection_get(&self, path: &str) -> CollectionOutcome {
        match self.get_json_status(path) {
            JsonStatusOutcome::Transport => CollectionOutcome::Transport,
            JsonStatusOutcome::Response {
                status: 401 | 403, ..
            } => CollectionOutcome::Denied,
            JsonStatusOutcome::Response { status: 404, .. } => CollectionOutcome::Missing,
            JsonStatusOutcome::Response {
                status: 200..=299,
                parsed: Some(page),
            } => CollectionOutcome::Ok(page),
            JsonStatusOutcome::Response { .. } => CollectionOutcome::Transport,
        }
    }
}

/// Extract the collection rating key from either supported member-listing route.
fn collection_rk_from_hub_key(key: &str) -> Option<&str> {
    let path = key.split_once('?').map_or(key, |(path, _)| path);
    let tail = path.strip_prefix("/library/collections/")?;
    let (rating_key, endpoint) = tail.split_once('/')?;
    (!rating_key.is_empty() && matches!(endpoint, "children" | "items")).then_some(rating_key)
}

/// Resolve a member tag to a full collection row: the tag id (a collection row's `index`) first,
/// then the exact title.
pub fn resolve_tag<'a>(rows: &'a [Metadata], tag_id: i64, title: &str) -> Option<&'a Metadata> {
    (tag_id != 0)
        .then(|| rows.iter().find(|row| row.index == tag_id))
        .flatten()
        .or_else(|| {
            (!title.is_empty())
                .then(|| rows.iter().find(|row| row.title == title))
                .flatten()
        })
}

/// The `(ratingKey, stamp)` of an automatic collection composite
/// (`/library/collections/{rk}/composite/{stamp}`, possibly with a query); `None` for a custom
/// poster path or no art at all.
pub fn composite_parts(thumb: &str) -> Option<(&str, &str)> {
    let path = thumb.split_once('?').map_or(thumb, |(path, _)| path);
    let mut segments = path.strip_prefix("/library/collections/")?.split('/');
    match (segments.next(), segments.next(), segments.next(), segments.next()) {
        (Some(rk), Some("composite"), Some(stamp), None) if !rk.is_empty() && !stamp.is_empty() => {
            Some((rk, stamp))
        }
        _ => None,
    }
}

/// The collection a promoted `custom.collection.{section}.{rk}.{rk}` hub lists, for a linked shelf
/// heading: `(section, ratingKey)`. The rating key comes from the hub's listing `key`
/// (`/library/collections/{rk}/children`) and falls back to the identifier's own segment; the
/// section is the identifier's first segment (0 when it does not parse). `collection.related` hubs
/// answer `None` — they are keyed by TAG id and a Detail page resolves them through the store.
fn promoted_collection_hub<'a>(
    hub_identifier: &'a str,
    key: &'a str,
) -> Option<(i64, &'a str)> {
    let tail = hub_identifier.strip_prefix("custom.collection.")?;
    let mut segments = tail.split('.');
    let section = segments.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let rk = collection_rk_from_hub_key(key)
        .or_else(|| segments.next().filter(|rk| !rk.is_empty()))?;
    Some((section, rk))
}

/// Where a promoted collection shelf's linked heading leads: the collection its hub lists, titled
/// with the shelf's heading. `section` is the section the shelf was published under, used when the
/// identifier's own section segment does not parse. `None` for every other hub.
pub fn promoted_collection_link(
    sid: ServerId,
    hub_identifier: &str,
    key: &str,
    title: &str,
    section: i64,
) -> Option<CollectionRef> {
    let (sec, rk) = promoted_collection_hub(hub_identifier, key)?;
    Some(CollectionRef::by_rk(sid, rk, if sec != 0 { sec } else { section }, title))
}

/// A member's own-collection hub from `/library/metadata/{rk}/related`:
/// `collection.related.{section}.{n}`, whose `key` filters its section by the collection's TAG id
/// (`/library/sections/{s}/all?type=1&tagId={tag}&sort=…`). Answers `(section, tag)` — the section
/// from the key's path, else the identifier's first segment, else 0; the tag from `tagId`, else 0
/// (the collection store then resolves by title alone). Any other hub answers `None`.
pub fn related_collection_hub(hub_identifier: &str, key: &str) -> Option<(i64, i64)> {
    let tail = hub_identifier.strip_prefix("collection.related")?;
    if !(tail.is_empty() || tail.starts_with('.')) {
        return None;
    }
    let (path, query) = key.split_once('?').unwrap_or((key, ""));
    let section = path
        .strip_prefix("/library/sections/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|s| s.parse().ok())
        .or_else(|| tail.strip_prefix('.')?.split('.').next()?.parse().ok())
        .unwrap_or(0);
    let tag = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("tagId="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some((section, tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "devtriggers")]
    use crate::plex::{Origin, ServerId};
    #[cfg(feature = "devtriggers")]
    use std::io::{Read, Write};

    fn page(json: &[u8]) -> MediaContainer {
        serde_json::from_slice::<super::super::models::Envelope>(json)
            .expect("collection fixture")
            .media_container
    }

    #[test]
    fn redacted_collection_shapes_parse_with_lenient_numbers_and_tag_guids() {
        let section = page(br#"{"MediaContainer":{"size":1,"totalSize":"3","offset":0,"Metadata":[
            {"ratingKey":"420","key":"/library/collections/420/children","guid":"collection://fixture-a",
             "type":"collection","title":"Placeholder Collection","subtype":"movie","index":77,
             "thumb":"/library/collections/420/composite/1700000000?width=400","updatedAt":"1700000000",
             "childCount":"3"}]}}"#);
        let row = &section.metadata[0];
        assert_eq!((section.size, section.total_size, row.index), (1, 3, 77));
        assert_eq!((row.child_count, row.updated_at), (3, 1_700_000_000));
        assert_eq!(row.subtype, "movie");

        let odd_guids = page(
            br#"{"MediaContainer":{"Metadata":[{"Genre":[
            {"tag":"Null guid","guid":null},{"tag":"Numeric guid","guid":42}
            ]}]}}"#,
        );
        let tags = &odd_guids.metadata[0].genre;
        assert_eq!(tags[0].guid, "");
        assert_eq!(tags[1].guid, "42");

        let children = page(
            br#"{"MediaContainer":{"size":"2","totalSize":"3","offset":"1",
            "Metadata":[{"ratingKey":"901","type":"movie","title":"Placeholder One"},
            {"ratingKey":"902","type":"movie","title":"Placeholder Two"}]}}"#,
        );
        assert_eq!(
            (children.size, children.total_size, children.offset),
            (2, 3, 1)
        );
    }

    #[test]
    fn hub_keys_accept_children_and_items_only() {
        assert_eq!(
            collection_rk_from_hub_key("/library/collections/420/children"),
            Some("420")
        );
        assert_eq!(
            collection_rk_from_hub_key("/library/collections/abc/items?start=1"),
            Some("abc")
        );
        assert_eq!(collection_rk_from_hub_key("/library/collections/420"), None);
        assert_eq!(
            collection_rk_from_hub_key("/library/collections/420/thumb"),
            None
        );
    }

    #[test]
    fn tag_resolution_prefers_index_then_exact_title() {
        let rows = page(
            br#"{"MediaContainer":{"Metadata":[
            {"title":"Title Match","guid":"collection://wrong","index":5},
            {"title":"Other","guid":"collection://right","index":6},
            {"title":"Index Match","guid":"collection://third","index":77}]}}"#,
        )
        .metadata;
        assert_eq!(
            resolve_tag(&rows, 77, "Title Match").unwrap().title,
            "Index Match"
        );
        assert_eq!(
            resolve_tag(&rows, 99, "Title Match").unwrap().title,
            "Title Match"
        );
        assert!(resolve_tag(&rows, 99, "Missing").is_none());
    }

    #[test]
    fn art_distinguishes_composites_custom_paths_and_absence() {
        assert_eq!(composite_parts(""), None);
        assert_eq!(
            composite_parts("/library/collections/420/composite/1700?width=400"),
            Some(("420", "1700"))
        );
        assert_eq!(composite_parts("/library/metadata/420/thumb/1700"), None);
        assert_eq!(composite_parts("/library/collections/420/composite/1700/x"), None);
    }

    #[test]
    fn a_promoted_collection_hub_names_its_section_and_rating_key() {
        assert_eq!(
            promoted_collection_hub(
                "custom.collection.1.420.420",
                "/library/collections/420/children"
            ),
            Some((1, "420"))
        );
        assert_eq!(
            promoted_collection_hub("custom.collection.2.77.77", ""),
            Some((2, "77")),
            "an absent key falls back to the identifier's own rating key"
        );
        assert_eq!(
            promoted_collection_hub("collection.related.1.1", "/library/sections/1/all?tagId=9"),
            None,
            "a related hub is tag-keyed and is resolved by the collection store"
        );
        assert_eq!(promoted_collection_hub("movie.recentlyadded.1", "/x"), None);
    }

    /// Home and the Library build a promoted shelf's link through one function, so the section a
    /// link carries when the identifier's own segment does not parse is the publishing section on
    /// both — never a bare 0 on one of them.
    #[test]
    fn a_promoted_link_falls_back_to_the_publishing_section() {
        let sid = ServerId::from_raw(3);
        let key = "/library/collections/420/children";
        assert_eq!(
            promoted_collection_link(sid, "custom.collection.1.420.420", key, "Set", 9),
            Some(CollectionRef::by_rk(sid, "420", 1, "Set"))
        );
        assert_eq!(
            promoted_collection_link(sid, "custom.collection.x.420.420", key, "Set", 9),
            Some(CollectionRef::by_rk(sid, "420", 9, "Set"))
        );
        assert_eq!(promoted_collection_link(sid, "movie.similar", key, "Set", 9), None);
    }

    #[test]
    fn collection_identity_never_compares_a_tag_with_a_rating_key() {
        let sid = ServerId::from_raw(2);
        let at = |rk: &str, tag| CollectionRef { sid, rk: rk.into(), sec: 4, tag, name: "A".into() };
        assert!(at("50077", 77).same_collection(&at("50077", 99)),
            "two resolved identities compare their ratingKey");
        assert!(!at("50077", 77).same_collection(&at("50078", 77)),
            "different non-empty ratingKeys do not fall through to tag identity");
        assert!(at("", 77).same_collection(&at("", 77)),
            "tag-only identities compare server, section and non-zero tag");
        assert!(at("50077", 77).same_collection(&at("", 77)),
            "a resolved identity still matches the tag route it was resolved from");
        assert!(!at("", 0).same_collection(&at("", 0)), "zero is not a tag identity");
        assert!(!at("50077", 0).same_collection(&at("", 50077)),
            "a ratingKey is never compared to a numeric tag id");
        assert!(!at("50077", 0).same_collection(&CollectionRef { sid: ServerId::from_raw(5), ..at("50077", 0) }),
            "another server's collection is another collection");
        assert!(at("", 0).is_identityless() && !at("", 7).is_identityless() && !at("1", 0).is_identityless());
    }

    #[test]
    fn a_related_collection_hub_names_its_section_and_tag() {
        assert_eq!(
            related_collection_hub(
                "collection.related.2.1",
                "/library/sections/3/all?type=1&tagId=812&sort=originallyAvailableAt,year:nullsLast"
            ),
            Some((3, 812)),
            "the key's own section and tag win"
        );
        assert_eq!(
            related_collection_hub("collection.related.2.1", ""),
            Some((2, 0)),
            "a keyless hub still names its section; the store resolves the title"
        );
        assert_eq!(related_collection_hub("collection.relatedness", "/x"), None);
        assert_eq!(related_collection_hub("movie.similar.1", "/x"), None);
        assert_eq!(
            related_collection_hub("custom.collection.1.420.420", "/library/collections/420/children"),
            None
        );
    }

    // Dev-only: this fixture drives a plaintext loopback PMS with a real client that carries a
    // token, which a store build's `CredentialPolicy::HttpsOnly` refuses before the request ever
    // reaches the wire (see `http::credential_transport_allowed`) — the connection this test
    // waits on then never arrives. See `client.rs`'s
    // `malformed_2xx_remains_a_response_after_its_deadline_passes` for the same gating.
    #[cfg(feature = "devtriggers")]
    fn outcome_for(status: &str) -> Option<CollectionOutcome> {
        // The agent sandbox denies loopback binds; the coordinator and ordinary host suite run
        // this branch. This is the same skip convention used by the transport's own tests.
        let Ok(listener) = std::net::TcpListener::bind("127.0.0.1:0") else {
            return None;
        };
        let port = listener.local_addr().unwrap().port();
        let status = status.to_string();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept collection request");
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).expect("read collection request");
            let request = String::from_utf8_lossy(&request[..n]);
            assert!(request.starts_with("GET /library/metadata/420"));
            assert!(request.contains("Accept: application/json"));
            let body = r#"{"MediaContainer":{}}"#;
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write collection response");
        });
        let client = Client::new(
            ServerId::UNSET,
            "fixture",
            Origin::http("127.0.0.1", port as i32),
            "",
            "cid",
        );
        let outcome = client.collection("420");
        server.join().unwrap();
        Some(outcome)
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn authorization_and_absence_keep_their_http_meanings() {
        let Some(denied) = outcome_for("403 Forbidden") else {
            return;
        };
        assert!(matches!(denied, CollectionOutcome::Denied));
        assert!(matches!(
            outcome_for("404 Not Found"),
            Some(CollectionOutcome::Missing)
        ));
    }
}
