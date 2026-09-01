//! **The PostHog capture body, hand-rolled** — the pure half: two endpoint shapes, one privacy flag.
//!
//! Like [`super::sentry`], everything here is a function from values to bytes. No socket, no
//! endpoint, no key. What it exists to get right is two things a reader would reasonably assume are
//! the same and are not, plus one flag whose absence is a privacy posture nobody chose.
//!
//! # The two shapes differ in WHERE the identity goes — verified against the vendor docs
//!
//! This was carried into the plan as an externally-sourced claim nobody here had checked. It is
//! true, and the consequence is worse than "a different field order":
//!
//! | | `/i/v0/e/` | `/batch/` |
//! |---|---|---|
//! | `api_key` | top level | top level, **once for the whole batch** |
//! | `distinct_id` | **top level** | **inside each entry's `properties`** |
//! | events | one, top level | an array under `batch` |
//!
//! Put `distinct_id` at the top of a batch ENTRY and it is not an error — it is an ordinary
//! property named `distinct_id` that PostHog ignores, so every event in the batch arrives with no
//! identity at all and the ingest still answers 200. That is the failure this module is shaped to
//! make impossible, and [`the_two_endpoints_disagree_about_where_identity_goes`] is where it is
//! pinned.
//!
//! # `$process_person_profile: false`, and why it is not a parameter
//!
//! PostHog's API-captured events are **identified by default**. Without this flag every install
//! gets a person profile — a privacy posture this project did not choose and would have to declare
//! — and the event costs about four times as much against the free tier. So it is not an argument
//! and not a default: [`props`] writes it unconditionally, LAST, so it also wins over anything a
//! caller managed to put under the same key. A test asserts it for every [`DiagEvent`] variant
//! rather than for one sample, because "we remembered on this path" is exactly the shape of claim
//! that turns out to be false on the seventh path.
//!
//! [`DiagEvent`]: crate::diag::schema::DiagEvent
//!
//! # Timestamps belong to the durable event
//!
//! The queue captures Unix milliseconds with the event. This pure serializer converts that stored
//! value to ISO 8601 at send time and reads no clock, so an offline spool cannot move an event into
//! the next launch merely because that is when delivery resumed.
//!
//! # One item here is uncalled, on purpose
//!
//! [`batch`] has none: the worker sends one record at a time, because a spool commit acknowledges
//! by `event_id` and a batch that half-succeeds cannot say which half. It is kept because the two
//! endpoints put `distinct_id` in DIFFERENT places — top level for `/i/v0/e/`, inside `properties`
//! for `/batch/` — which is the trap it exists to have already solved.
//!
//! This header used to say no sender existed at all. It does: [`super::sender`].

#[cfg(test)]
use crate::diag::schema::{self, Value};
use crate::diag::schema::{DiagEvent, UsageContext, UsageEnvelope, UsageValue};

/// The flag that keeps an event anonymous. Spelled once, here, so a typo is one test away rather
/// than one silent person profile per install.
const ANON: &str = "$process_person_profile";

/// The property bag for one event: the schema's own fields, then the anonymity flag written LAST so
/// it cannot be overridden by anything above it.
///
/// `distinct_id` is deliberately NOT added here — it goes in a different place per endpoint, which
/// is the whole point of this module, so each shape adds it itself and the test can tell them apart.
#[cfg(test)]
fn props(e: DiagEvent, environment: &str) -> serde_json::Map<String, serde_json::Value> {
    let (_, fields) = schema::serialize(e);
    let mut m = serde_json::Map::new();
    // Which side of the world this build is on. A parameter rather than a constant read here, so
    // this module keeps knowing nothing about credentials — `sender` derives it from the pair it
    // was given and passes it down.
    m.insert(
        "environment".into(),
        serde_json::Value::String(environment.to_string()),
    );
    for (k, v) in fields {
        // Exhaustive on purpose: a new `Value` arm must be a compile error here, not a field that
        // quietly stops being sent.
        m.insert(
            k.to_string(),
            match v {
                Value::Str(s) => serde_json::Value::String(s.to_string()),
                Value::Int(n) => serde_json::Value::Number(n.into()),
            },
        );
    }
    // Last, and unconditional. See the module doc.
    m.insert(ANON.into(), serde_json::Value::Bool(false));
    m
}

fn envelope_props(
    event: &UsageEnvelope,
    environment: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut properties = serde_json::Map::new();
    properties.insert("environment".into(), environment.into());
    for (key, value) in &event.fields {
        properties.insert(
            key.clone(),
            match value {
                UsageValue::Str(s) => s.clone().into(),
                UsageValue::Int(n) => (*n).into(),
            },
        );
    }
    let context = &event.context;
    for (key, value) in [
        ("app_version", &context.app_version),
        ("webos_release", &context.webos_release),
        ("webos_api", &context.webos_api),
        ("webos_codename", &context.webos_codename),
        ("device_model", &context.device_model),
        ("soc", &context.soc),
        ("hardware_revision", &context.hardware_revision),
        ("server_connection", &context.server_connection),
        ("ip_version", &context.ip_version),
    ] {
        properties.insert(key.into(), value.clone().into());
    }
    properties.insert("$session_id".into(), event.session_id.clone().into());
    properties.insert(ANON.into(), false.into());
    properties
}

/// Render a durable internal event for PostHog only when it is about to leave the spool.
pub(crate) fn captured(
    api_key: &str,
    distinct_id: &str,
    event: &UsageEnvelope,
    environment: &str,
) -> Vec<u8> {
    render_captured(
        api_key,
        distinct_id,
        event,
        environment,
        &rfc3339_millis(event.occurred_at_ms),
    )
}

fn render_captured(
    api_key: &str,
    distinct_id: &str,
    event: &UsageEnvelope,
    environment: &str,
    timestamp: &str,
) -> Vec<u8> {
    let body = serde_json::json!({
        "api_key": api_key,
        "event": event.name,
        "distinct_id": distinct_id,
        "properties": envelope_props(event, environment),
        "timestamp": timestamp,
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The consent screen's exact sender shape, with runtime-only metadata visibly labelled.
pub(crate) fn preview(
    api_key: &str,
    distinct_id: &str,
    event: DiagEvent,
    environment: &str,
) -> Vec<u8> {
    let envelope = UsageEnvelope::capture_with_context(
        event,
        0,
        "<random session id>",
        UsageContext::preview(),
    );
    render_captured(api_key, distinct_id, &envelope, environment, "<event time>")
}

/// UTC RFC3339 without a clock dependency. Input is Unix milliseconds captured with the event.
fn rfc3339_millis(epoch_ms: u64) -> String {
    let seconds = epoch_ms / 1_000;
    let millis = epoch_ms % 1_000;
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    // Howard Hinnant's civil_from_days, with Unix day zero shifted to the civil epoch.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = day_seconds / 3_600;
    let minute = day_seconds % 3_600 / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Legacy/test-only shape helper for **`POST <host>/i/v0/e/`**. Production durable events use
/// [`captured`] and always carry their original occurrence time; this helper keeps the older
/// endpoint-shape tests able to exercise an explicitly absent timestamp.
#[cfg(test)]
pub(crate) fn single(
    api_key: &str,
    distinct_id: &str,
    e: DiagEvent,
    environment: &str,
    timestamp: Option<&str>,
) -> Vec<u8> {
    let (name, _) = schema::serialize(e);
    let mut body = serde_json::Map::new();
    body.insert("api_key".into(), api_key.into());
    body.insert("event".into(), name.into());
    body.insert("distinct_id".into(), distinct_id.into());
    body.insert(
        "properties".into(),
        serde_json::Value::Object(props(e, environment)),
    );
    if let Some(ts) = timestamp {
        body.insert("timestamp".into(), ts.into());
    }
    serde_json::to_vec(&serde_json::Value::Object(body)).unwrap_or_default()
}

/// A body for **`POST <host>/batch/`** — many events, one `api_key`, and `distinct_id` INSIDE each
/// entry's `properties`.
///
/// `historical_migration` is sent explicitly as `false`. It is optional, but this is a spool that
/// can be days behind a television that was switched off, so "are you backfilling history?" is a
/// question the payload will look like it is answering. Saying no out loud costs one field.
// Genuinely uncalled, and re-audited rather than inherited: the worker sends one record at a
// time, because a spool commit acknowledges by `event_id` and a batch that half-succeeds cannot
// say which half. Kept because the two endpoints put `distinct_id` in DIFFERENT places — top
// level for `/i/v0/e/`, inside `properties` for `/batch/` — which is the trap this function
// exists to have already solved when a batch is worth having.
#[cfg(test)]
pub(crate) fn batch(
    api_key: &str,
    distinct_id: &str,
    environment: &str,
    events: &[(DiagEvent, &str)],
) -> Vec<u8> {
    let entries: Vec<serde_json::Value> = events
        .iter()
        .map(|(e, ts)| {
            let (name, _) = schema::serialize(*e);
            let mut p = props(*e, environment);
            // THE difference. Top-level here would be an ordinary ignored property, and the batch
            // would arrive with no identity while still answering 200.
            p.insert(
                "distinct_id".into(),
                serde_json::Value::String(distinct_id.to_string()),
            );
            serde_json::json!({ "event": name, "properties": p, "timestamp": ts })
        })
        .collect();
    let body = serde_json::json!({
        "api_key": api_key,
        "historical_migration": false,
        "batch": entries,
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, so a property asserted below is asserted for the whole schema rather than for
    /// one convenient sample.
    fn all() -> Vec<DiagEvent> {
        vec![
            DiagEvent::AppLaunch,
            DiagEvent::RouteEntered { screen: "home" },
            DiagEvent::SignInStarted,
            DiagEvent::SignInCompleted,
            DiagEvent::SignInFailed {
                kind: schema::SignInFailure::Authorization,
            },
            DiagEvent::SignInCancelled,
            DiagEvent::FeatureUsed {
                feature: schema::Feature::Pause,
            },
            DiagEvent::PlaybackRequested { playback_id: 7 },
            DiagEvent::PlaybackStarted {
                playback_id: 7,
                mode: "direct",
                raster: "fhd",
                fps: "24",
                video: "h264",
                audio: "ac3",
                startup: "1-3s",
            },
            DiagEvent::PlaybackFailed {
                playback_id: 7,
                mode: "direct",
                kind: "network",
            },
            DiagEvent::PlaybackCancelled {
                playback_id: 7,
                mode: "direct",
            },
            DiagEvent::PlaybackAbandoned {
                playback_id: 7,
                mode: "direct",
            },
            DiagEvent::PlaybackEnded {
                playback_id: 7,
                mode: "direct",
                watched: "most",
            },
        ]
    }

    fn parse(b: &[u8]) -> serde_json::Value {
        serde_json::from_slice(b).expect("the body is valid JSON")
    }

    /// **THE ONE THAT MATTERS.** Every event, on both endpoints, carries
    /// `$process_person_profile: false`.
    ///
    /// Asserted across every variant and both shapes because PostHog's default is IDENTIFIED: the
    /// cost of forgetting is not an error, it is a person profile per install — a privacy posture
    /// this project did not choose, would have to declare to LG, and would find out about from a
    /// dashboard rather than from a failure.
    #[test]
    fn every_event_on_both_endpoints_is_anonymous() {
        for e in all() {
            let s = parse(&single(
                "phc_k",
                "id1",
                e,
                "test",
                Some("2026-08-29T00:00:00Z"),
            ));
            assert_eq!(
                s["properties"][ANON],
                serde_json::json!(false),
                "a single-event body was not anonymous: {e:?}"
            );
            let b = parse(&batch(
                "phc_k",
                "id1",
                "test",
                &[(e, "2026-08-29T00:00:00Z")],
            ));
            assert_eq!(
                b["batch"][0]["properties"][ANON],
                serde_json::json!(false),
                "a batched body was not anonymous: {e:?}"
            );
        }
    }

    /// **The two endpoints disagree about where identity goes**, and this is the assertion that
    /// keeps them apart. Verified against PostHog's own capture documentation.
    ///
    /// The batch half is the dangerous one: a top-level `distinct_id` on an entry is not rejected,
    /// it is an ordinary property PostHog ignores — so the events arrive unattributed and the
    /// ingest still answers 200. There is no failure to notice.
    #[test]
    fn the_two_endpoints_disagree_about_where_identity_goes() {
        let s = parse(&single(
            "phc_k",
            "id1",
            DiagEvent::AppLaunch,
            "test",
            Some("2026-08-29T00:00:00Z"),
        ));
        assert_eq!(
            s["distinct_id"], "id1",
            "/i/v0/e/ carries distinct_id at the top level"
        );
        assert!(
            s["properties"].get("distinct_id").is_none(),
            "/i/v0/e/ must NOT also put it in properties"
        );

        let b = parse(&batch(
            "phc_k",
            "id1",
            "test",
            &[(DiagEvent::AppLaunch, "2026-08-29T00:00:00Z")],
        ));
        let entry = &b["batch"][0];
        assert_eq!(
            entry["properties"]["distinct_id"], "id1",
            "/batch/ carries distinct_id INSIDE each entry's properties"
        );
        assert!(
            entry.get("distinct_id").is_none(),
            "a top-level distinct_id on a batch entry is silently ignored — the batch would arrive \
             unattributed and still answer 200"
        );
    }

    /// The key is sent ONCE for a batch, at the top, and never repeated per entry — that is the
    /// shape the endpoint documents, and repeating it would put the project token in the body N
    /// times for no benefit.
    #[test]
    fn a_batch_carries_one_api_key_at_the_top() {
        let evs: Vec<(DiagEvent, &str)> = all()
            .into_iter()
            .map(|e| (e, "2026-08-29T00:00:00Z"))
            .collect();
        let b = parse(&batch("phc_k", "id1", "test", &evs));
        assert_eq!(b["api_key"], "phc_k");
        assert_eq!(b["historical_migration"], serde_json::json!(false));
        let arr = b["batch"].as_array().expect("batch is an array");
        assert_eq!(arr.len(), evs.len());
        for entry in arr {
            assert!(
                entry.get("api_key").is_none(),
                "the key is not repeated per entry"
            );
            assert!(entry["event"].is_string());
        }
    }

    /// A schema field reaches `properties` under its own name, beside the flag rather than instead
    /// of it — the ordinary case, asserted because `props` writes the flag last and an overzealous
    /// version of that could clobber real fields.
    #[test]
    fn schema_fields_survive_beside_the_anonymity_flag() {
        let s = parse(&single(
            "phc_k",
            "id1",
            DiagEvent::RouteEntered { screen: "detail" },
            "test",
            Some("2026-08-29T00:00:00Z"),
        ));
        assert_eq!(s["event"], "route.entered");
        assert_eq!(s["properties"]["screen"], "detail");
        assert_eq!(s["properties"][ANON], serde_json::json!(false));
    }

    /// The event name in the body is the SCHEMA's name, not a variant name stringified — the two
    /// have drifted in other projects, and `PRIVACY.md` documents the schema's.
    #[test]
    fn the_wire_name_is_the_schema_name() {
        for e in all() {
            let (name, _) = schema::serialize(e);
            let s = parse(&single(
                "phc_k",
                "id1",
                e,
                "test",
                Some("2026-08-29T00:00:00Z"),
            ));
            assert_eq!(s["event"], name);
            assert!(schema::EVENT_SPECS.iter().any(|s| s.name == name));
        }
    }

    /// **No timestamp means no field**, not an empty string — which is what lets PostHog stamp on
    /// arrival, the right default given this television's ~3h clock skew.
    #[test]
    fn an_absent_timestamp_is_omitted_rather_than_blank() {
        let b = parse(&single("phc_k", "id1", DiagEvent::AppLaunch, "test", None));
        assert!(
            b.get("timestamp").is_none(),
            "a blank timestamp would be worse than none"
        );
        assert_eq!(b["event"], "app.launch");
        let with = parse(&single(
            "phc_k",
            "id1",
            DiagEvent::AppLaunch,
            "test",
            Some("T"),
        ));
        assert_eq!(with["timestamp"], "T");
    }

    /// An empty batch is still a well-formed body rather than a panic or a malformed array. A spool
    /// flush with nothing in it is an ordinary event, not an error.
    #[test]
    fn an_empty_batch_is_still_well_formed() {
        let b = parse(&batch("phc_k", "id1", "test", &[]));
        assert_eq!(b["batch"].as_array().map(|a| a.len()), Some(0));
        assert_eq!(b["api_key"], "phc_k");
    }

    #[test]
    fn a_durable_event_keeps_its_original_time_and_session() {
        let context = UsageContext {
            app_version: "0.5.0".into(),
            webos_release: "4.10.2".into(),
            webos_api: "4.1.0".into(),
            webos_codename: "goldilocks2-grampians".into(),
            device_model: "m16p3s".into(),
            soc: "M19_DVB".into(),
            hardware_revision: "BOARD_PT_1ST".into(),
            server_connection: "local".into(),
            ip_version: "v4".into(),
        };
        let event = UsageEnvelope::capture_with_context(
            DiagEvent::RouteEntered { screen: "detail" },
            1_787_961_234_567,
            "0198f00d-1234-4567-89ab-0123456789ab",
            context,
        );
        let body = parse(&captured("phc_k", "install", &event, "test"));
        assert_eq!(body["timestamp"], "2026-08-28T23:53:54.567Z");
        assert_eq!(
            body["properties"]["$session_id"],
            "0198f00d-1234-4567-89ab-0123456789ab"
        );
        assert_eq!(body["properties"]["screen"], "detail");
        assert_eq!(body["properties"]["webos_release"], "4.10.2");
        assert_eq!(body["properties"]["soc"], "M19_DVB");
        assert_eq!(body["properties"]["server_connection"], "local");
        assert_eq!(body["properties"]["ip_version"], "v4");
        assert_eq!(body["properties"][ANON], false);
    }
}
