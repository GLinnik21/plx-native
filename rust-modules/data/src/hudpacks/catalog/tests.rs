use super::*;
use serde_json::{json, Value};

fn index() -> Value {
    let sha = "a".repeat(64);
    let art = "b".repeat(64);
    json!({"format":1,"entries":[{
        "id":"com.example.hud","name":"Example","version":"1.0.0","author":"Author","license":"CC0-1.0",
        "package_format":"1.0","hud_format":"1.0","app":{},"bytes":200,"sha256":sha,
        "url":format!("{ORIGIN}/p/{sha}.plxhud"),
        "previews":[{"state":"compact","lang":"en","file":format!("{art}.png"),"sha256":art,
            "url":format!("{ORIGIN}/previews/{sha}/{art}.png")}],"unfit":[],"sdk":{"source_sha":"ignored provenance"}
    }],"revoked":[]})
}

fn body(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn contract_matches_registry_and_rejects_every_cross_origin_resource() {
    assert!(parse(&body(&index())).is_some());
    for path in [
        "url",
        "preview-url",
        "preview-file",
        "sha",
        "duplicate",
        "bounds",
        "format",
        "version",
    ] {
        let mut value = index();
        match path {
            "url" => {
                value["entries"][0]["url"] = json!("https://glinnik21.github.io.evil/p/pack.plxhud")
            }
            "preview-url" => {
                value["entries"][0]["previews"][0]["url"] = json!("https://evil.test/image.png")
            }
            "preview-file" => value["entries"][0]["previews"][0]["file"] = json!("../escape.png"),
            "sha" => value["entries"][0]["sha256"] = json!("invalid"),
            "duplicate" => {
                let p = value["entries"][0].clone();
                value["entries"].as_array_mut().unwrap().push(p);
            }
            "bounds" => value["entries"][0]["app"] = json!({"min":"2.0.0","max":"1.0.0"}),
            "format" => value["format"] = json!(2),
            "version" => value["entries"][0]["version"] = json!("v1.0.0"),
            _ => unreachable!(),
        }
        assert!(parse(&body(&value)).is_none(), "{path}");
    }
    assert!(parse(br#"{"format":1,"format":1,"entries":[],"revoked":[]}"#).is_none());
    assert!(parse(&vec![b' '; INDEX_BYTE_LIMIT + 1]).is_none());
}

#[test]
fn compatibility_hides_future_formats_and_uses_latest_language_transition() {
    let mut value = index();
    value["entries"][0]["unfit"] = json!([
        {"from":"0.10.1","langs":["fr"]}, {"from":"0.10.2","langs":[]}]);
    let mut package = parse(&body(&value)).unwrap().entries.remove(0);
    assert!(package.compatible("0.10.0", "fr", 1000));
    assert!(package.compatible("0.10.0-dev", "fr", 1000));
    assert!(!package.compatible("0.10.1-nightly.4", "fr", 1000));
    assert!(package.compatible("0.10.2+build.7", "fr", 1000));
    assert!(!package.compatible("dev", "fr", 1000));
    assert!(!package.compatible("0.10.1", "fr", 1000));
    assert!(package.compatible("0.10.1", "en", 1000));
    assert!(package.compatible("0.10.2", "fr", 1000));
    package.package_format = "2.0".into();
    assert!(!package.compatible("0.10.2", "fr", 1000));
    package.package_format = "1.0".into();
    package.hud_format = "1.1".into();
    assert!(!package.compatible("0.10.2", "fr", 1000));
    package.hud_format = "1.0".into();
    package.app.max = Some("0.10.1".into());
    assert!(!package.compatible("0.10.2", "fr", 1000));
}

fn open(catalog: &mut Catalog, now: u64) -> Vec<CatalogEffect> {
    let mut fx = vec![];
    catalog.step(
        Event::Open {
            candidates: vec![],
            now,
        },
        &mut fx,
    );
    fx
}

fn respond(catalog: &mut Catalog, status: u16, etag: Option<&str>, data: Vec<u8>) -> Vec<CatalogEffect> {
    let mut fx = vec![];
    assert!(catalog.step(
        Event::Reply {
            tag: catalog.generation,
            result: Ok(Reply::http(status, etag.map(str::to_owned), data))
        },
        &mut fx
    ));
    fx
}

#[test]
fn only_a_settings_open_fetches_and_daily_304_keeps_content_and_advances_date() {
    let mut catalog = Catalog::default();
    assert!(catalog.snapshot.index.is_none());
    assert_eq!(
        open(&mut catalog, 10),
        vec![CatalogEffect::Get { tag: 1, etag: None }]
    );
    assert!(
        open(&mut catalog, 11).is_empty(),
        "duplicate view must not restart a live fetch"
    );
    respond(&mut catalog, 200, Some("W/\"one\""), body(&index()));
    let original = catalog.snapshot.index.clone();
    assert_eq!(catalog.snapshot.checked_at, Some(10));
    assert!(open(&mut catalog, 10 + REFRESH_SECONDS - 1).is_empty());
    assert_eq!(
        open(&mut catalog, 10 + REFRESH_SECONDS),
        vec![CatalogEffect::Get {
            tag: 3,
            etag: Some("W/\"one\"".into())
        }]
    );
    respond(&mut catalog, 304, None, vec![]);
    assert_eq!(catalog.snapshot.index, original);
    assert_eq!(catalog.snapshot.checked_at, Some(10 + REFRESH_SECONDS));
    assert!(!catalog.snapshot.refresh_failed);
}

#[test]
fn failed_or_invalid_reply_keeps_cache_and_backs_off_even_after_restart() {
    for status in [200, 302, 404, 503] {
        let mut catalog = Catalog::default();
        open(&mut catalog, 1);
        respond(&mut catalog, 200, Some("\"one\""), body(&index()));
        open(&mut catalog, 1 + REFRESH_SECONDS);
        let candidate = Candidate {
            dir: "cache-root".into(),
            volatile: false,
        };
        catalog.candidates = vec![candidate.clone()];
        let fx = respond(&mut catalog, status, None, b"broken".to_vec());
        assert!(catalog.snapshot.refresh_failed);
        assert!(catalog.snapshot.index.is_some());
        assert_eq!(catalog.snapshot.checked_at, Some(1));
        let CatalogEffect::Write { record, .. } = &fx[0] else {
            panic!("cache write")
        };
        let mut restarted = Catalog::default();
        let mut fx2 = vec![];
        restarted.step(
            Event::Open {
                candidates: vec![candidate],
                now: 2 + REFRESH_SECONDS,
            },
            &mut fx2,
        );
        fx2.clear();
        restarted.step(
            Event::Read {
                tag: 1,
                result: Ok(CacheRecord::decode(&record.encode()).unwrap()),
            },
            &mut fx2,
        );
        assert!(fx2.is_empty(), "persisted failure backoff");
        assert!(restarted.snapshot.index.is_some());
        assert_eq!(restarted.snapshot.checked_at, Some(1));
    }
}

#[test]
fn stale_replies_and_unbound_304_cannot_create_catalog_or_inject_request_headers() {
    let mut catalog = Catalog::default();
    open(&mut catalog, 1);
    let mut fx = vec![];
    assert!(!catalog.step(
        Event::Reply {
            tag: 0,
            result: Ok(Reply::http(200, None, body(&index())))
        },
        &mut fx
    ));
    respond(&mut catalog, 304, Some("\"unbound\""), vec![]);
    assert!(catalog.snapshot.index.is_none());
    assert!(catalog.snapshot.refresh_failed);
    open(&mut catalog, 1 + REFRESH_SECONDS);
    respond(
        &mut catalog,
        200,
        Some("\"one\"\r\nX-Evil: yes"),
        body(&index()),
    );
    assert!(catalog.etag.is_none());
}

#[test]
fn revocation_reason_is_retained_when_the_package_is_not_in_entries() {
    let mut value = index();
    value["entries"] = json!([]);
    value["revoked"] = json!([{"sha256":"a".repeat(64),"reason":"Unsafe layout"}]);
    let snapshot = Snapshot {
        index: parse(&body(&value)).map(Arc::new),
        ..Snapshot::default()
    };
    assert_eq!(
        snapshot.revoked(Sha::parse(&"a".repeat(64)).unwrap()),
        Some("Unsafe layout")
    );
}

#[test]
fn boot_never_dials_and_a_crash_during_get_keeps_the_persisted_attempt_backoff() {
    let candidate = Candidate {
        dir: "root".into(),
        volatile: false,
    };
    let mut catalog = Catalog::default();
    let mut fx = vec![];
    catalog.step(
        Event::Load {
            candidates: vec![candidate.clone()],
        },
        &mut fx,
    );
    assert!(matches!(fx.as_slice(), [CatalogEffect::Read { .. }]));
    fx.clear();
    catalog.step(
        Event::Read {
            tag: 1,
            result: Err(ReadError::Missing),
        },
        &mut fx,
    );
    assert!(
        fx.is_empty(),
        "boot may not fetch an absent or expired index"
    );
    catalog.step(
        Event::Open {
            candidates: vec![candidate.clone()],
            now: 10,
        },
        &mut fx,
    );
    fx.clear();
    catalog.step(
        Event::Read {
            tag: 2,
            result: Err(ReadError::Missing),
        },
        &mut fx,
    );
    let record = match &fx[0] {
        CatalogEffect::Write { record, .. } => record.clone(),
        _ => panic!("attempt persisted before network"),
    };
    fx.clear();
    catalog.step(
        Event::Written {
            tag: 2,
            outcome: WriteOutcome::Done,
        },
        &mut fx,
    );
    assert_eq!(fx, vec![CatalogEffect::Get { tag: 2, etag: None }]);
    let mut restarted = Catalog::default();
    fx.clear();
    restarted.step(
        Event::Open {
            candidates: vec![candidate],
            now: 11,
        },
        &mut fx,
    );
    fx.clear();
    restarted.step(
        Event::Read {
            tag: 1,
            result: Ok(CacheRecord::decode(&record.encode()).unwrap()),
        },
        &mut fx,
    );
    assert!(
        fx.is_empty(),
        "restart cannot retry a request still inside the daily backoff"
    );
}
