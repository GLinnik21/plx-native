//! Collection listings, detail and membership reads.
//!
//! PMS has two collection id spaces: collection metadata uses a `ratingKey`, while member tags
//! use the collection's `index`. The helpers here keep those identities explicit and centralize
//! the live-observed joins between collection rows, hubs and member `Collection[]` tags.

use super::client::{Client, JsonStatusOutcome, QueryBuilder};
use super::models::{MediaContainer, Metadata};

/// A collection read preserves the server answers that collection UI must present distinctly.
pub(crate) enum CollectionOutcome {
    Ok(MediaContainer),
    Denied,
    Missing,
    Transport(CollectionError),
}

/// Failures other than the two collection-specific HTTP answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionError {
    RequestFailed,
    Http(i32),
    Malformed,
}

impl Client {
    /// `GET /library/sections/{section}/collections`, paged with both required parameters.
    pub(crate) fn section_collections(
        &self,
        section: i64,
        start: i64,
        size: i64,
    ) -> CollectionOutcome {
        let path = QueryBuilder::new(format!("/library/sections/{section}/collections"))
            .int("X-Plex-Container-Start", start)
            .int("X-Plex-Container-Size", size)
            .build();
        self.collection_get(&path)
    }

    /// `GET /library/metadata/{ratingKey}` for one collection's metadata.
    pub(crate) fn collection(&self, rating_key: &str) -> CollectionOutcome {
        self.collection_get(&format!("/library/metadata/{rating_key}"))
    }

    /// `GET /library/collections/{ratingKey}/children`, paged with both required parameters.
    pub(crate) fn collection_children(
        &self,
        rating_key: &str,
        start: i64,
        size: i64,
    ) -> CollectionOutcome {
        let path = QueryBuilder::new(format!("/library/collections/{rating_key}/children"))
            .int("X-Plex-Container-Start", start)
            .int("X-Plex-Container-Size", size)
            .build();
        self.collection_get(&path)
    }

    fn collection_get(&self, path: &str) -> CollectionOutcome {
        match self.get_json_status(path) {
            JsonStatusOutcome::Transport => {
                CollectionOutcome::Transport(CollectionError::RequestFailed)
            }
            JsonStatusOutcome::Response {
                status: 401 | 403, ..
            } => CollectionOutcome::Denied,
            JsonStatusOutcome::Response { status: 404, .. } => CollectionOutcome::Missing,
            JsonStatusOutcome::Response {
                status: 200..=299,
                parsed: Some(page),
            } => CollectionOutcome::Ok(page),
            JsonStatusOutcome::Response {
                status: 200..=299,
                parsed: None,
            } => CollectionOutcome::Transport(CollectionError::Malformed),
            JsonStatusOutcome::Response { status, .. } => {
                CollectionOutcome::Transport(CollectionError::Http(status))
            }
        }
    }
}

/// Extract the collection rating key from either supported member-listing route.
pub(crate) fn collection_rk_from_hub_key(key: &str) -> Option<&str> {
    let path = key.split_once('?').map_or(key, |(path, _)| path);
    let tail = path.strip_prefix("/library/collections/")?;
    let (rating_key, endpoint) = tail.split_once('/')?;
    (!rating_key.is_empty() && matches!(endpoint, "children" | "items")).then_some(rating_key)
}

/// Resolve a member tag to a full collection row using the strongest live-observed identity first.
pub(crate) fn resolve_tag<'a>(
    rows: &'a [Metadata],
    tag_id: i64,
    guid: &str,
    title: &str,
) -> Option<&'a Metadata> {
    (!guid.is_empty())
        .then(|| rows.iter().find(|row| row.guid == guid))
        .flatten()
        .or_else(|| {
            (tag_id != 0)
                .then(|| rows.iter().find(|row| row.index == tag_id))
                .flatten()
        })
        .or_else(|| {
            (!title.is_empty())
                .then(|| rows.iter().find(|row| row.title == title))
                .flatten()
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CollectionArt {
    Custom(String),
    Composite { rk: String, stamp: String },
    None,
}

/// Classify an automatic collection composite separately from an ordinary/custom poster path.
pub(crate) fn collection_art(thumb: Option<&str>) -> CollectionArt {
    let Some(thumb) = thumb.filter(|thumb| !thumb.is_empty()) else {
        return CollectionArt::None;
    };
    let path = thumb.split_once('?').map_or(thumb, |(path, _)| path);
    if let Some(tail) = path.strip_prefix("/library/collections/") {
        let mut segments = tail.split('/');
        if let (Some(rk), Some("composite"), Some(stamp), None) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) {
            if !rk.is_empty() && !stamp.is_empty() {
                return CollectionArt::Composite {
                    rk: rk.to_string(),
                    stamp: stamp.to_string(),
                };
            }
        }
    }
    CollectionArt::Custom(thumb.to_string())
}

pub(crate) fn is_collection_hub(hub_identifier: &str) -> bool {
    hub_identifier.starts_with("custom.collection.")
        || hub_identifier == "collection.related"
        || hub_identifier.starts_with("collection.related.")
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
    fn redacted_collection_shapes_parse_with_lenient_numbers_and_member_tags() {
        let section = page(br#"{"MediaContainer":{"size":1,"totalSize":"3","offset":0,"Metadata":[
            {"ratingKey":"420","key":"/library/collections/420/children","guid":"collection://fixture-a",
             "type":"collection","title":"Placeholder Collection","subtype":"movie","index":77,
             "thumb":"/library/collections/420/composite/1700000000?width=400","updatedAt":"1700000000",
             "childCount":"3"}]}}"#);
        let row = &section.metadata[0];
        assert_eq!((section.size, section.total_size, row.index), (1, 3, 77));
        assert_eq!((row.child_count, row.updated_at), (3, 1_700_000_000));
        assert_eq!(row.subtype, "movie");

        let member = page(
            br#"{"MediaContainer":{"Metadata":[{"ratingKey":"900","type":"movie",
            "title":"Placeholder Movie","Collection":[{"id":77,"filter":"collection=77",
            "tag":"Placeholder Collection","guid":"collection://fixture-a"}]}]}}"#,
        );
        let tag = &member.metadata[0].collection[0];
        assert_eq!(tag.id, 77);
        assert_eq!(tag.filter, "collection=77");
        assert_eq!(tag.guid, "collection://fixture-a");

        let odd_guids = page(
            br#"{"MediaContainer":{"Metadata":[{"Collection":[
            {"tag":"Null guid","guid":null},{"tag":"Numeric guid","guid":42}
            ]}]}}"#,
        );
        let tags = &odd_guids.metadata[0].collection;
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
    fn tag_resolution_prefers_guid_then_index_then_exact_title() {
        let rows = page(
            br#"{"MediaContainer":{"Metadata":[
            {"title":"Title Match","guid":"collection://wrong","index":5},
            {"title":"Other","guid":"collection://right","index":6},
            {"title":"Index Match","guid":"collection://third","index":77}]}}"#,
        )
        .metadata;
        assert_eq!(
            resolve_tag(&rows, 77, "collection://right", "Title Match")
                .unwrap()
                .title,
            "Other"
        );
        assert_eq!(
            resolve_tag(&rows, 77, "", "Title Match").unwrap().title,
            "Index Match"
        );
        assert_eq!(
            resolve_tag(&rows, 99, "", "Title Match").unwrap().title,
            "Title Match"
        );
        assert!(resolve_tag(&rows, 99, "", "Missing").is_none());
    }

    #[test]
    fn art_distinguishes_composites_custom_paths_and_absence() {
        assert_eq!(collection_art(None), CollectionArt::None);
        assert_eq!(collection_art(Some("")), CollectionArt::None);
        assert_eq!(
            collection_art(Some("/library/collections/420/composite/1700?width=400")),
            CollectionArt::Composite {
                rk: "420".into(),
                stamp: "1700".into()
            }
        );
        assert_eq!(
            collection_art(Some("/library/metadata/420/thumb/1700")),
            CollectionArt::Custom("/library/metadata/420/thumb/1700".into())
        );
    }

    #[test]
    fn recognizes_promoted_and_related_collection_hubs() {
        assert!(is_collection_hub("custom.collection.1.420.420"));
        assert!(is_collection_hub("collection.related"));
        assert!(is_collection_hub("collection.related.1.1"));
        assert!(!is_collection_hub("movie.similar"));
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
