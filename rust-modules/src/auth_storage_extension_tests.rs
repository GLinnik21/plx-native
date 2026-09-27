//! Storage-extension regressions: rebuilding a primary and refreshing an endpoint must carry
//! a stored source's opaque forward-compatible fields and its profile credentials.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

fn stored_source() -> SourceRef {
    serde_json::from_value(serde_json::json!({
        "machine_id":"synthetic-machine", "address":"old.example.invalid", "port":32400,
        "token":"synthetic-profile-token", "future":{"protected":"synthetic-future"}
    }))
    .unwrap()
}

#[test]
fn rebuilding_primary_preserves_the_sources_opaque_fields() {
    let source = stored_source();
    assert_eq!(server_ref(&source).extensions, source.extensions);
}

#[test]
fn endpoint_refresh_preserves_opaque_fields_and_profile_credentials() {
    let source = stored_source();
    let mut session = Session {
        sources: vec![source.clone()],
        ..Default::default()
    };
    let fresh = SourceRef {
        address: "new.example.invalid".into(),
        port: 32400,
        token: "different-account-token".into(),
        ..Default::default()
    };
    let (updated, changed) =
        apply_refreshed_endpoint(&mut session, "synthetic-machine", &fresh).unwrap();
    assert!(changed);
    assert_eq!(updated.extensions, source.extensions);
    assert_eq!(session.sources[0].extensions, source.extensions);
    assert_eq!(updated.token, "synthetic-profile-token");
}
