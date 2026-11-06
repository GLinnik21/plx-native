use super::*;
use crate::hudpacks::catalog::testing::{self, TEST_ONLY_PRIMARY, TEST_ONLY_STANDBY, TEST_ONLY_STRANGER};
use plx_base::sha256::sha256;
use serde_json::{json, Value};

fn index() -> Value {
    let sha = "a".repeat(64);
    let art = "b".repeat(64);
    json!({"format":1,"serial":1000,"entries":[{
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

/// `index.signed` for `value`, signed by the TEST ONLY primary.
fn signed(value: &Value) -> Vec<u8> {
    testing::sign(&body(value))
}

/// An index published at `serial`, signed by the TEST ONLY primary.
fn signed_at(serial: u64) -> Vec<u8> {
    let mut value = index();
    value["serial"] = json!(serial);
    signed(&value)
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
                value["entries"][0]["url"] = json!("https://huds.plxnative.com.evil/p/pack.plxhud")
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
            result: Ok(Reply::http(status, etag.map(str::to_owned), &data, &testing::keys()))
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
    respond(&mut catalog, 200, Some("W/\"one\""), signed(&index()));
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
        respond(&mut catalog, 200, Some("\"one\""), signed(&index()));
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
        let Some(CatalogEffect::Write { record, .. }) = fx.iter().find(|e| matches!(e, CatalogEffect::Write { .. })) else {
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
                result: Ok(CacheRecord::decode(&record.encode(), &testing::keys()).unwrap()),
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
            result: Ok(Reply::http(200, None, &signed(&index()), &testing::keys()))
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
        signed(&index()),
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
            result: Ok(CacheRecord::decode(&record.encode(), &testing::keys()).unwrap()),
        },
        &mut fx,
    );
    assert!(
        fx.is_empty(),
        "restart cannot retry a request still inside the daily backoff"
    );
}

// ---- the signed index: the contract's acceptance steps, in order ----

const PRIMARY_KEY_HEX: &str = "45ef4dd7f0ba0b0531014245a4b9ecbc1b1c1ebc848fa2b5d7ae5bc537c6658a";
const STANDBY_KEY_HEX: &str = "c80a7613abd76a596c4c21875c4abb280c1b0e279a79b2c43e86ce9e0b41a792";

#[test]
fn the_compiled_in_keys_are_exactly_the_contracts_two_and_each_id_names_its_key() {
    let contract = [
        (KeyRole::Primary, "4fe9b53b661ff2e8", PRIMARY_KEY_HEX),
        (KeyRole::Standby, "437641d803b6932b", STANDBY_KEY_HEX),
    ];
    assert_eq!(ACCEPTED_KEYS.len(), contract.len());
    for (accepted, (role, id, key)) in ACCEPTED_KEYS.iter().zip(contract) {
        assert_eq!(accepted.role, role);
        assert_eq!(testing::hex(&accepted.key), key, "{role:?}");
        assert_eq!(testing::hex(&accepted.id), id, "{role:?}");
        assert_eq!(accepted.id[..], sha256(&accepted.key)[..8], "{role:?}: the id is the first 8 bytes of SHA-256 over the key");
        ed25519_compact::PublicKey::new(accepted.key).validate().expect("a canonical point of large order");
    }
    // Tests sign with their own keys: none of them is the registry's.
    for test in testing::keys() {
        assert!(ACCEPTED_KEYS.iter().all(|k| k.key != test.key && k.id != test.id));
    }
}

fn refusal(wire: &[u8]) -> Refusal {
    verify(wire, &testing::keys()).expect_err("must be refused")
}

#[test]
fn a_good_signature_by_the_primary_or_the_standby_is_accepted_with_its_serial_and_the_exact_body() {
    for seed in [TEST_ONLY_PRIMARY, TEST_ONLY_STANDBY] {
        let wire = testing::sign_with(seed, &body(&index()));
        let accepted = verify(&wire, &testing::keys()).expect("accepted");
        assert_eq!(accepted.serial, 1000);
        assert_eq!(accepted.index.serial, Some(1000));
        assert_eq!(accepted.index.entries.len(), 1);
        assert_eq!(accepted.body.as_bytes(), wire.as_slice(), "the cache stores the bytes that were signed over");
    }
}

/// The header and index of a well-formed signed body, to rebuild a damaged one from.
struct Parts {
    key: String,
    sig: String,
    index: Vec<u8>,
}

fn parts(value: &Value) -> Parts {
    let wire = signed(value);
    let split = wire.iter().position(|b| *b == b'\n').unwrap();
    let header: Value = serde_json::from_slice(&wire[..split]).unwrap();
    Parts {
        key: header["key"].as_str().unwrap().to_owned(),
        sig: header["sig"].as_str().unwrap().to_owned(),
        index: wire[split + 1..].to_vec(),
    }
}

fn framed(header: &str, index: &[u8]) -> Vec<u8> {
    [header.as_bytes(), b"\n", index].concat()
}

fn header(key: &str, sig: &str) -> String {
    format!(r#"{{"format":1,"key":"{key}","sig":"{sig}"}}"#)
}

#[test]
fn every_way_a_header_can_be_wrong_is_malformed() {
    let p = parts(&index());
    let (k, s) = (p.key.as_str(), p.sig.as_str());
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated signature hex", framed(&header(k, &s[..127]), &p.index)),
        ("signature one digit too long", framed(&header(k, &format!("{s}0")), &p.index)),
        ("upper-case signature hex", framed(&header(k, &s.to_uppercase()), &p.index)),
        ("signature that is not hex", framed(&header(k, &format!("g{}", &s[1..])), &p.index)),
        ("key one digit short", framed(&header(&k[..15], s), &p.index)),
        ("upper-case key hex", framed(&header(&k.to_uppercase(), s), &p.index)),
        ("an extra key", framed(&format!(r#"{{"format":1,"key":"{k}","sig":"{s}","extra":1}}"#), &p.index)),
        ("a missing key", framed(&format!(r#"{{"format":1,"sig":"{s}"}}"#), &p.index)),
        ("a missing sig", framed(&format!(r#"{{"format":1,"key":"{k}"}}"#), &p.index)),
        ("a missing format", framed(&format!(r#"{{"key":"{k}","sig":"{s}"}}"#), &p.index)),
        ("a repeated key", framed(&format!(r#"{{"format":1,"key":"{k}","key":"{k}","sig":"{s}"}}"#), &p.index)),
        ("format 2", framed(&format!(r#"{{"format":2,"key":"{k}","sig":"{s}"}}"#), &p.index)),
        ("format as a string", framed(&format!(r#"{{"format":"1","key":"{k}","sig":"{s}"}}"#), &p.index)),
        ("a header that is an array, which a derived reader would take field by field", framed(&format!(r#"[1,"{k}","{s}"]"#), &p.index)),
        ("a header that ends in a carriage return", framed(&format!("{}\r", header(k, s)), &p.index)),
        ("a header with leading whitespace", framed(&format!(" {}", header(k, s)), &p.index)),
        ("no newline at all", header(k, s).into_bytes()),
        ("a bare PR13-era index with no header", body(&index())),
        ("a bare pretty-printed index", serde_json::to_vec_pretty(&index()).unwrap()),
        ("an empty body", Vec::new()),
        // Otherwise a valid header: only its length is wrong. JSON allows the padding, the bound does not.
        ("a 257-byte header", framed(&format!(r#"{{"format":1,"key":"{k}","sig":"{s}"{}}}"#, " ".repeat(257 - 174)), &p.index)),
    ];
    for (name, wire) in cases {
        assert_eq!(refusal(&wire), Refusal::Malformed, "{name}");
    }
}

#[test]
fn an_index_part_over_the_limit_is_malformed_even_under_a_good_signature() {
    // Parse alone would say `index` here; the framing bound comes first and says `malformed`.
    let wire = testing::sign(&vec![b' '; INDEX_BYTE_LIMIT + 1]);
    assert_eq!(refusal(&wire), Refusal::Malformed);
    assert_eq!(refusal(&testing::sign(&vec![b' '; INDEX_BYTE_LIMIT])), Refusal::Index);
}

#[test]
fn a_key_the_app_does_not_accept_is_named_before_any_signature_is_checked() {
    let stranger = testing::sign_with(TEST_ONLY_STRANGER, &body(&index()));
    assert_eq!(refusal(&stranger), Refusal::UnknownKey);
    let p = parts(&index());
    let unknown = framed(&header("0123456789abcdef", &p.sig), &p.index);
    assert_eq!(refusal(&unknown), Refusal::UnknownKey);
    // A known id on a signature the stranger made: the key is known, the signature is not that key's.
    let sig = testing::pair(TEST_ONLY_STRANGER).sk.sign(&p.index, None);
    let forged = framed(&header(&p.key, &testing::hex(sig.as_ref())), &p.index);
    assert_eq!(refusal(&forged), Refusal::BadSignature);
}

#[test]
fn a_changed_index_byte_or_signature_byte_or_a_signature_over_another_body_is_a_bad_signature() {
    let p = parts(&index());
    let mut flipped = p.index.clone();
    let at = flipped.windows(7).position(|w| w == b"Example").unwrap();
    flipped[at] = b'F';
    assert!(parse(&flipped).is_some(), "the damaged index is still a valid index, so only the signature can refuse it");
    assert_eq!(refusal(&framed(&header(&p.key, &p.sig), &flipped)), Refusal::BadSignature);

    for digit in [0, 63, 64, 127] {
        let mut sig = p.sig.clone().into_bytes();
        sig[digit] = if sig[digit] == b'0' { b'1' } else { b'0' };
        let sig = String::from_utf8(sig).unwrap();
        assert_eq!(refusal(&framed(&header(&p.key, &sig), &p.index)), Refusal::BadSignature, "digit {digit}");
    }

    // The skew case: the header and signature of one deploy over the index of another.
    let other = parts(&{
        let mut v = index();
        v["serial"] = json!(1001);
        v
    });
    assert_ne!(other.index, p.index);
    assert_eq!(refusal(&framed(&header(&p.key, &p.sig), &other.index)), Refusal::BadSignature);
}

#[test]
fn verification_is_strict_about_the_scalar_and_about_weak_keys() {
    let p = parts(&index());
    // S + L satisfies the equation but is not the canonical encoding of S: RFC 8032 section 5.1.7 says reject.
    const L: [u8; 32] = [
        0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0x10,
    ];
    let mut sig: Vec<u8> = (0..64).map(|i| u8::from_str_radix(&p.sig[2 * i..2 * i + 2], 16).unwrap()).collect();
    let mut carry = 0u16;
    for i in 0..32 {
        let sum = u16::from(sig[32 + i]) + u16::from(L[i]) + carry;
        sig[32 + i] = sum as u8;
        carry = sum >> 8;
    }
    assert_eq!(carry, 0);
    assert_eq!(refusal(&framed(&header(&p.key, &testing::hex(&sig)), &p.index)), Refusal::BadSignature);

    // The identity point as the public key, with R = identity and S = 0, "signs" every message in a verifier that
    // does not refuse a small-order key. The accepted-key set here names that key, so only the crate's check stops it.
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let mut id = [0u8; 8];
    id.copy_from_slice(&sha256(&identity)[..8]);
    let weak = AcceptedKey { role: KeyRole::Primary, id, key: identity };
    let mut forged = [0u8; 64];
    forged[0] = 1;
    let wire = framed(&header(&testing::hex(&id), &testing::hex(&forged)), &p.index);
    assert_eq!(verify(&wire, &[weak]).unwrap_err(), Refusal::BadSignature);
}

#[test]
fn a_signed_body_that_is_not_an_index_with_a_serial_is_refused_after_its_signature() {
    let signed_json = |edit: &dyn Fn(&mut Value)| {
        let mut v = index();
        edit(&mut v);
        signed(&v)
    };
    let cases: Vec<(&str, Vec<u8>, Refusal)> = vec![
        ("no serial", signed_json(&|v| {
            v.as_object_mut().unwrap().remove("serial");
        }), Refusal::NoSerial),
        ("a null serial", signed_json(&|v| v["serial"] = Value::Null), Refusal::NoSerial),
        ("a negative serial", signed_json(&|v| v["serial"] = json!(-1)), Refusal::Index),
        ("a string serial", signed_json(&|v| v["serial"] = json!("1000")), Refusal::Index),
        ("a fractional serial", signed_json(&|v| v["serial"] = json!(1000.5)), Refusal::Index),
        ("an index format this build does not read", signed_json(&|v| v["format"] = json!(2)), Refusal::Index),
        ("a foreign url", signed_json(&|v| v["entries"][0]["url"] = json!("https://evil.test/p.plxhud")), Refusal::Index),
        ("bytes that are not JSON", testing::sign(b"\xff\xfe not json"), Refusal::Index),
        ("an empty index", testing::sign(b""), Refusal::Index),
    ];
    for (name, wire, want) in cases {
        assert_eq!(refusal(&wire), want, "{name}");
    }
}

fn candidate() -> Candidate {
    Candidate { dir: "cache-root".into(), volatile: false }
}

/// A 200 for `wire` against a catalog that holds the serial-1000 index and whose cache is `candidate()`.
fn refused_by_a_catalog_holding_1000(wire: Vec<u8>) -> (Catalog, Vec<CatalogEffect>) {
    let mut catalog = Catalog::default();
    open(&mut catalog, 1);
    respond(&mut catalog, 200, Some("\"one\""), signed_at(1000));
    open(&mut catalog, 1 + REFRESH_SECONDS);
    catalog.candidates = vec![candidate()];
    let fx = respond(&mut catalog, 200, Some("\"two\""), wire);
    (catalog, fx)
}

#[test]
fn every_refusal_keeps_the_cached_index_logs_its_cause_once_and_backs_off() {
    let p = parts(&index());
    let mut no_serial = index();
    no_serial.as_object_mut().unwrap().remove("serial");
    let cases: Vec<(Refusal, Vec<u8>)> = vec![
        (Refusal::Malformed, b"<html>captive portal</html>".to_vec()),
        (Refusal::UnknownKey, testing::sign_with(TEST_ONLY_STRANGER, &body(&index()))),
        (Refusal::BadSignature, framed(&header(&p.key, &p.sig), b"{}")),
        (Refusal::Index, testing::sign(b"{}")),
        (Refusal::NoSerial, signed(&no_serial)),
        (Refusal::Rollback, signed_at(999)),
    ];
    for (cause, wire) in cases {
        let (mut catalog, fx) = refused_by_a_catalog_holding_1000(wire);
        let name = cause.cause();
        assert_eq!(
            fx.iter().filter(|e| matches!(e, CatalogEffect::Refused { .. })).collect::<Vec<_>>(),
            vec![&CatalogEffect::Refused { cause }],
            "{name}: exactly one log effect, naming the cause"
        );
        assert_eq!(catalog.snapshot.index.as_ref().and_then(|i| i.serial), Some(1000), "{name}: the cached index stays");
        assert!(catalog.snapshot.previous.is_none(), "{name}");
        assert!(catalog.snapshot.refresh_failed, "{name}");
        assert!(!catalog.snapshot.fetching, "{name}");
        assert_eq!(catalog.snapshot.checked_at, Some(1), "{name}: the date of the last verified index");
        assert_eq!(catalog.etag.as_deref(), Some("\"one\""), "{name}: the validator of the cached index, not of the refused body");
        let Some(CatalogEffect::Write { record, .. }) = fx.iter().find(|e| matches!(e, CatalogEffect::Write { .. })) else {
            panic!("{name}: the attempt is persisted")
        };
        let kept = CacheRecord::decode(&record.encode(), &testing::keys()).expect("the persisted cache is still the verified one");
        assert_eq!(kept.index.as_ref().and_then(|i| i.serial), Some(1000), "{name}");
        // The attempt counts under the daily backoff: nothing is requested again until the day is up.
        let mut after = vec![];
        catalog.step(Event::Written { tag: catalog.generation, outcome: WriteOutcome::Done }, &mut after);
        assert!(after.is_empty(), "{name}");
        assert!(open(&mut catalog, 2 + REFRESH_SECONDS).is_empty(), "{name}: backoff");
    }
}

#[test]
fn a_serial_lower_than_the_cached_one_is_refused_and_an_equal_or_higher_one_is_accepted() {
    let mut catalog = Catalog::default();
    open(&mut catalog, 1);
    respond(&mut catalog, 200, Some("\"one\""), signed_at(1000));
    let held = |c: &Catalog| c.snapshot.index.as_ref().and_then(|i| i.serial);
    assert_eq!(held(&catalog), Some(1000));

    let mut now = 1;
    for (serial, accepted) in [(999, false), (0, false), (1000, true), (1001, true), (1000, false), (u64::MAX, true), (7, false)] {
        let before = held(&catalog);
        now += REFRESH_SECONDS;
        open(&mut catalog, now);
        let fx = respond(&mut catalog, 200, None, signed_at(serial));
        if accepted {
            assert_eq!(held(&catalog), Some(serial), "serial {serial}");
            assert!(!catalog.snapshot.refresh_failed, "serial {serial}");
            assert_eq!(catalog.snapshot.checked_at, Some(now), "serial {serial}");
            assert!(!fx.iter().any(|e| matches!(e, CatalogEffect::Refused { .. })), "serial {serial}");
        } else {
            assert_eq!(held(&catalog), before, "serial {serial}");
            assert!(catalog.snapshot.refresh_failed, "serial {serial}");
            assert!(fx.contains(&CatalogEffect::Refused { cause: Refusal::Rollback }), "serial {serial}");
        }
    }
}

#[test]
fn a_304_keeps_the_verified_state_and_refuses_nothing() {
    let mut catalog = Catalog::default();
    open(&mut catalog, 1);
    respond(&mut catalog, 200, Some("\"one\""), signed_at(1000));
    let original = catalog.snapshot.index.clone();
    let body = catalog.body.clone();
    open(&mut catalog, 1 + REFRESH_SECONDS);
    let fx = respond(&mut catalog, 304, Some("\"one\""), vec![]);
    assert!(!fx.iter().any(|e| matches!(e, CatalogEffect::Refused { .. })));
    assert_eq!(catalog.snapshot.index, original);
    assert_eq!(catalog.body, body, "the signed body the cache re-verifies is unchanged");
    assert_eq!(catalog.snapshot.checked_at, Some(1 + REFRESH_SECONDS));
    assert!(!catalog.snapshot.refresh_failed);
}

#[test]
fn the_cache_stores_the_signed_body_and_verifies_it_again_on_every_load() {
    let mut catalog = Catalog::default();
    open(&mut catalog, 1);
    catalog.candidates = vec![candidate()];
    let fx = respond(&mut catalog, 200, Some("\"one\""), signed_at(1234));
    let Some(CatalogEffect::Write { record, .. }) = fx.iter().find(|e| matches!(e, CatalogEffect::Write { .. })) else {
        panic!("cache write")
    };
    let file = record.encode();
    let stored: Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(stored["body"].as_str().unwrap().as_bytes(), signed_at(1234).as_slice());
    assert_eq!(&CacheRecord::decode(&file, &testing::keys()).unwrap(), record);

    // The same file read by a build that does not accept the key it was signed with is refused whole.
    assert_eq!(CacheRecord::decode(&file, &ACCEPTED_KEYS).unwrap_err(), Refusal::UnknownKey);

    // A tampered file: one byte of the index changed under the stored signature.
    let mut tampered = stored.clone();
    tampered["body"] = json!(stored["body"].as_str().unwrap().replacen("Example", "Exampl3", 1));
    assert_eq!(CacheRecord::decode(&serde_json::to_vec(&tampered).unwrap(), &testing::keys()).unwrap_err(), Refusal::BadSignature);
    // A body smuggled in under "no body, never validated" does not escape verification.
    let smuggled = json!({"protocol": 1, "checked_at": null, "attempted_at": 3, "etag": null, "body": "junk"});
    assert_eq!(CacheRecord::decode(&serde_json::to_vec(&smuggled).unwrap(), &testing::keys()).unwrap_err(), Refusal::Malformed);
}

#[test]
fn a_pr13_era_cache_is_dropped_with_its_etag_so_the_next_request_is_unconditional() {
    // PR13 wrote the bare index.json and its ETag under the same protocol number.
    let pr13 = serde_json::to_vec(&json!({
        "protocol": 1, "checked_at": 5, "attempted_at": 5, "etag": "\"pr13\"",
        "body": String::from_utf8(body(&index())).unwrap(),
    }))
    .unwrap();
    assert_eq!(CacheRecord::decode(&pr13, &testing::keys()).unwrap_err(), Refusal::Malformed);

    // The runner turns that refusal into `ReadError::Failed`; the machine then has no cache and no validator.
    let mut catalog = Catalog::default();
    let mut fx = vec![];
    catalog.step(Event::Open { candidates: vec![candidate()], now: 1_000_000 }, &mut fx);
    assert!(matches!(fx.as_slice(), [CatalogEffect::Read { .. }]));
    fx.clear();
    catalog.step(Event::Read { tag: catalog.generation, result: Err(ReadError::Failed) }, &mut fx);
    let Some(CatalogEffect::Write { record, .. }) = fx.first() else { panic!("the attempt is persisted before dialing") };
    assert!(!String::from_utf8(record.encode()).unwrap().contains("pr13"), "the stale validator is gone from the file too");
    fx.clear();
    catalog.step(Event::Written { tag: catalog.generation, outcome: WriteOutcome::Done }, &mut fx);
    assert_eq!(fx, vec![CatalogEffect::Get { tag: catalog.generation, etag: None }]);
    assert!(catalog.snapshot.index.is_none());
}
