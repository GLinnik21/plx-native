//! Handled playback errors for Sentry: one terminal event, preceded by a bounded typed trace.
//!
//! This is deliberately not a log uploader. Every value accepted here is a closed enum from
//! [`super::classes`]; there is no title, URL, rating key, playhead, duration, measured bitrate or
//! free-text error slot. Sparse transitions are retained in memory only while error reporting is
//! enabled, and nothing is queued unless the viewer actually reaches `PlaybackState::Error`.

use super::classes::{FailureClass, PlaybackErrorContext, TraceEvent, TraceOutcome, TraceStep};
use serde_json::{Map, Value};

/// How telemetry erases the in-memory trace the player keeps for the current attempt. The trace
/// lives in the player's shared state, which this layer cannot name, so the player installs the
/// eraser the first time it arms a trace (`player::report::requested`). Unset, there is no trace to
/// erase: nothing is retained before that first attempt arms one.
static CLEAR_ERROR_TRACE: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the function [`clear_error_trace`] calls. The first registration wins; the player
/// registers the same function every time.
pub(crate) fn install_error_trace_clear(clear: fn()) {
    let _ = CLEAR_ERROR_TRACE.set(clear);
}

/// Forget the in-memory playback trace immediately, as error reporting is withdrawn or the account
/// that consented ends. A no-op until the player has installed its eraser.
pub(crate) fn clear_error_trace() {
    if let Some(clear) = CLEAR_ERROR_TRACE.get() {
        clear();
    }
}

fn put(data: &mut Map<String, Value>, key: &'static str, value: &'static str) {
    data.insert(key.to_string(), Value::String(value.to_string()));
}

fn breadcrumb(step: TraceStep) -> Value {
    let mut data = Map::new();
    put(&mut data, "elapsed", step.age.code());
    let (message, level) = match step.event {
        TraceEvent::Requested { selected } => {
            put(&mut data, "selected", selected.code());
            ("playback requested", "info")
        }
        TraceEvent::Presented {
            delivery,
            requested,
            declared_rate,
            raster,
        } => {
            put(&mut data, "delivery", delivery.code());
            put(&mut data, "requested", requested.code());
            put(&mut data, "declared_rate", declared_rate.code());
            put(&mut data, "raster", raster.code());
            ("picture presented", "info")
        }
        TraceEvent::SeekRequested => ("seek requested", "info"),
        TraceEvent::QualitySelected { selected } => {
            put(&mut data, "selected", selected.code());
            ("quality selected", "info")
        }
        TraceEvent::DeliveryRequested {
            delivery,
            requested,
            reason,
        } => {
            put(&mut data, "delivery", delivery.code());
            put(&mut data, "requested", requested.code());
            put(&mut data, "reason", reason.code());
            ("delivery requested", "info")
        }
        TraceEvent::HlsCommitted {
            direction,
            requested,
        } => {
            put(&mut data, "direction", direction.code());
            put(&mut data, "requested", requested.code());
            ("HLS request committed", "info")
        }
        TraceEvent::OriginalProbe { phase, outcome } => {
            put(&mut data, "phase", phase.code());
            put(&mut data, "outcome", outcome.code());
            (
                "Original check phase",
                if matches!(
                    outcome,
                    TraceOutcome::Deadline
                        | TraceOutcome::Transport
                        | TraceOutcome::Inconclusive
                        | TraceOutcome::ServerState
                        | TraceOutcome::Refused
                ) {
                    "error"
                } else {
                    "info"
                },
            )
        }
        TraceEvent::LoadGateOpened { elapsed } => {
            put(&mut data, "load_elapsed", elapsed.code());
            ("load gate opened", "info")
        }
        TraceEvent::Failed { kind } => {
            put(&mut data, "kind", kind.code());
            ("playback failed", "error")
        }
    };
    serde_json::json!({
        "type": "state",
        "category": "playback",
        "level": level,
        "message": message,
        "data": Value::Object(data),
    })
}

/// Pure body builder. `dist` and `errors_id` are passed in so the consent preview can exercise this
/// exact serialiser without reading `/proc/self/exe` or minting an id before consent. `errors_id`
/// is the crash-report identifier, attached as `user.id` through the one shared
/// [`super::sentry::attach_user`]; the SDK scope does not reach a body this module builds by hand
/// — which is also why [`super::sentry::attach_hardware_context`] must be called here explicitly:
/// unlike a native crash, this event never touches the scope `sdk::start` put the `webos`/
/// `hardware` contexts on, so without that call it would carry no compatibility context at all.
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    kind: FailureClass,
    context: PlaybackErrorContext,
    trace: &[TraceStep],
) -> Vec<u8> {
    let code = kind.code();
    let breadcrumbs: Vec<Value> = trace.iter().copied().map(breadcrumb).collect();
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("PLX_VERSION")},
        "logger": "playback",
        "transaction": "playback",
        "culprit": format!("playback::{code}"),
        "fingerprint": ["playback-error", code],
        "exception": {"values": [{
            "type": "PlaybackError",
            "value": code,
            "mechanism": {"type": "playback", "handled": true},
        }]},
        "tags": {
            "playback.kind": code,
            "playback.delivery": context.delivery.code(),
            "playback.selected_quality": context.selected.code(),
            "playback.requested_quality": context.requested.code(),
            "playback.declared_rate": context.declared_rate.code(),
            "playback.started": if context.started { "yes" } else { "no" },
        },
        "contexts": {"playback": {
            "type": "playback",
            "delivery": context.delivery.code(),
            "selected_quality": context.selected.code(),
            "requested_quality": context.requested.code(),
            "declared_rate": context.declared_rate.code(),
            "media_rate": context.media_rate.code(),
            "raster": context.raster.code(),
            "pipeline": context.pipeline.code(),
            "http": context.http.code(),
            "buffer": context.buffer.code(),
            "started": context.started,
        }},
        "breadcrumbs": {"values": breadcrumbs},
    });
    if let Some(r) = context.refusal {
        // Tags so an issue can be searched and grouped on them, a context so one event reads in
        // one place. Every value is the `code()` of a closed enum; the server's own sentence has
        // no field here and no path to one (see `PlayVerdict::Server` — it stays on the device).
        for (key, value) in [
            ("playback.refusal_general", r.general.code()),
            ("playback.refusal_transcode", r.transcode.code()),
            ("playback.attempted_delivery", r.attempted.code()),
            ("playback.source_video", r.source_video.code()),
            ("playback.source_audio", r.source_audio.code()),
        ] {
            body["tags"][key] = Value::String(value.to_string());
        }
        body["contexts"]["refusal"] = serde_json::json!({
            "type": "refusal",
            "general_code": r.general.code(),
            "transcode_code": r.transcode.code(),
            "attempted_delivery": r.attempted.code(),
            "source_video": r.source_video.code(),
            "source_audio": r.source_audio.code(),
        });
    }
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    super::sentry::attach_user(&mut body, errors_id);
    super::sentry::attach_hardware_context(&mut body);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Queue one handled event and ask the existing background sender to flush it. No network work is
/// performed on the render thread.
pub(crate) fn report_error(kind: FailureClass, context: PlaybackErrorContext, trace: &[TraceStep]) {
    if !super::consent::allows_errors() || !super::sender::has_sentry() {
        return;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::eventlog::log("telemetry: no /dev/urandom — handled playback error was not queued");
        return;
    };
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        super::consent::errors_id().as_deref(),
        kind,
        context,
        trace,
    );
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, super::consent::allows_errors) {
        Some(true) => super::flush_soon(),
        Some(false) => {
            crate::eventlog::log("telemetry: handled playback error did not fit the durable spool")
        }
        None => {} // consent changed while the event was being shaped
    }
}

/// Representative handled-error payload built through the real serializer. Per-report random and
/// runtime-build values are visible placeholders; the other values are representative members of
/// the closed domains disclosed beside the preview. No consent-time identifier is minted.
pub(crate) fn preview_event() -> Vec<u8> {
    use crate::telemetry::classes::{
        AudioCodecClass, BufferClass, DecisionCodeClass, DeliveryClass, DeliveryReason, HttpClass,
        OriginalProbePhase, PipelineClass, QualityClass, RasterClass, RateClass, RefusalContext,
        TraceAge, TraceDirection, TraceOutcome, VideoCodecClass,
    };
    let trace = [
        TraceStep {
            age: TraceAge::Under1s,
            event: TraceEvent::Requested {
                selected: QualityClass::Auto,
            },
        },
        TraceStep {
            age: TraceAge::S1To3,
            event: TraceEvent::Presented {
                delivery: DeliveryClass::Hls,
                requested: QualityClass::M4,
                declared_rate: RateClass::M3To6,
                raster: RasterClass::Hd,
            },
        },
        TraceStep {
            age: TraceAge::S3To10,
            event: TraceEvent::SeekRequested,
        },
        TraceStep {
            age: TraceAge::S10To30,
            event: TraceEvent::QualitySelected {
                selected: QualityClass::Original,
            },
        },
        TraceStep {
            age: TraceAge::S10To30,
            event: TraceEvent::DeliveryRequested {
                delivery: DeliveryClass::Direct,
                requested: QualityClass::Original,
                reason: DeliveryReason::OriginalRecovery,
            },
        },
        TraceStep {
            age: TraceAge::S30To120,
            event: TraceEvent::HlsCommitted {
                direction: TraceDirection::Up,
                requested: QualityClass::M22,
            },
        },
        TraceStep {
            age: TraceAge::S30To120,
            event: TraceEvent::OriginalProbe {
                phase: OriginalProbePhase::SampleSource,
                outcome: TraceOutcome::ServerState,
            },
        },
        TraceStep {
            age: TraceAge::S30To120,
            event: TraceEvent::Failed {
                kind: FailureClass::PlaybackInterrupted,
            },
        },
    ];
    event_body(
        "<random per-error event id>",
        "<running ELF build id>",
        Some(super::native::PREVIEW_USER_ID),
        FailureClass::PlaybackInterrupted,
        PlaybackErrorContext {
            delivery: DeliveryClass::Hls,
            selected: QualityClass::Auto,
            requested: QualityClass::M22,
            declared_rate: RateClass::M3To6,
            media_rate: RateClass::M1To3,
            raster: RasterClass::Uhd,
            pipeline: PipelineClass::Streaming,
            http: HttpClass::ServerError,
            buffer: BufferClass::S3To10,
            started: true,
            // Representative of a `decision_refused` report's extra block, shown here so the
            // consent screen's sample carries every key a report can; a real report has it only
            // when the server refused the plan.
            refusal: Some(RefusalContext {
                general: DecisionCodeClass::C2000,
                transcode: DecisionCodeClass::C4007,
                attempted: DeliveryClass::Transcode,
                source_video: VideoCodecClass::Vp9,
                source_audio: AudioCodecClass::Eac3,
            }),
        },
        &trace,
    )
}

fn codes<T: Copy>(values: &[T], code: fn(T) -> &'static str) -> String {
    values
        .iter()
        .copied()
        .map(code)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Closed value domains for the representative handled-error payload above. Generated from the
/// same enum `code()` methods as the serializer, so the consent screen does not imply that its one
/// sample value is the only possible one.
pub(crate) fn preview_domains() -> String {
    use crate::telemetry::classes::{
        BufferClass as B, DeliveryClass as D, DeliveryReason as W, HttpClass as H,
        OriginalProbePhase as P, PipelineClass as L, QualityClass as Q, RasterClass as X,
        RateClass as R, TraceAge as A, TraceDirection as I, TraceOutcome as O,
    };
    use crate::telemetry::classes::{AudioCodecClass as Z, DecisionCodeClass as N, VideoCodecClass as V};
    use FailureClass as F;
    use crate::i18n::msg;
    // The labels are the reader's words; the codes after them are the wire values themselves.
    let domains: [(&str, String); 17] = [
        (msg::core_preview_domain_failure_kind(), codes(
            &[
                F::DecisionRefused,
                F::PlaybackPolicy,
                F::NoVideoTranscodeTarget,
                F::NoVideoTrack,
                F::MediaSource,
                F::PlaybackInterrupted,
                F::TvPipeline,
                F::OriginalRollback,
                F::Unspecified,
            ],
            F::code,
        )),
        (msg::core_preview_domain_delivery(), codes(&[D::Unknown, D::Direct, D::Remux, D::Hls, D::Transcode], D::code)),
        // The four rows below ride a `decision_refused` report only (`PlaybackErrorContext::
        // refusal`); the labels say so, and each list is its enum's own `ALL`/variant set.
        (msg::core_preview_domain_refusal_codes(), codes(&N::ALL, N::code)),
        (msg::core_preview_domain_attempted_delivery(), codes(&[D::Remux, D::Hls, D::Transcode], D::code)),
        (msg::core_preview_domain_source_video(), codes(&V::ALL, V::code)),
        (msg::core_preview_domain_source_audio(), codes(&Z::ALL, Z::code)),
        (msg::core_preview_domain_quality(), codes(
            &[
                Q::Unknown,
                Q::Auto,
                Q::Original,
                Q::K320,
                Q::K720,
                Q::M2,
                Q::M4,
                Q::M6,
                Q::M8,
                Q::M10,
                Q::M12,
                Q::M14,
                Q::M16,
                Q::M18,
                Q::M20,
                Q::M22,
            ],
            Q::code,
        )),
        (msg::core_preview_domain_observed_rate(), codes(
            &[
                R::Unknown,
                R::Under1m,
                R::M1To3,
                R::M3To6,
                R::M6To12,
                R::M12To20,
                R::Over20m
            ],
            R::code,
        )),
        (msg::core_preview_domain_raster(), codes(&[X::Unknown, X::Sd, X::Hd, X::Fhd, X::Uhd], X::code)),
        (msg::core_preview_domain_pipeline(), codes(&[L::Loading, L::Playing, L::Bound, L::Streaming], L::code)),
        (msg::core_preview_domain_http(), codes(
            &[
                H::None,
                H::Success,
                H::ClientError,
                H::ServerError,
                H::Other
            ],
            H::code
        )),
        (msg::core_preview_domain_buffer(), codes(
            &[
                B::Unknown,
                B::Empty,
                B::Under3s,
                B::S3To10,
                B::S10To30,
                B::Over30s
            ],
            B::code
        )),
        (msg::core_preview_domain_elapsed(), codes(
            &[
                A::Under1s,
                A::S1To3,
                A::S3To10,
                A::S10To30,
                A::S30To120,
                A::Over2m
            ],
            A::code
        )),
        (msg::core_preview_domain_hls_direction(), codes(&[I::Up, I::Down, I::Refresh], I::code)),
        (msg::core_preview_domain_delivery_reason(), codes(
            &[
                W::LinkFallback,
                W::OriginalRecovery,
                W::OriginalOpenRollback
            ],
            W::code
        )),
        (msg::core_preview_domain_original_phase(), codes(
            &[
                P::RetireHls,
                P::SampleSource,
                P::CloseSource,
                P::RestoreHls,
                P::OpenHls,
                P::CommitHls
            ],
            P::code
        )),
        (msg::core_preview_domain_original_outcome(), codes(
            &[
                O::Started,
                O::Succeeded,
                O::NoBody,
                O::Deadline,
                O::Transport,
                O::Inconclusive,
                O::ServerState,
                O::Refused
            ],
            O::code
        )),
    ];
    let mut out = msg::core_preview_domains().to_owned();
    for (label, values) in domains {
        out.push('\n');
        out.push_str(&msg::core_preview_domain_line(label, &values));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::classes::{
        AudioCodecClass, BufferClass, DecisionCodeClass, DeliveryClass, HttpClass,
        OriginalProbePhase, PipelineClass, QualityClass, RasterClass, RateClass, RefusalContext,
        TraceAge, TraceOutcome, VideoCodecClass,
    };

    fn context() -> PlaybackErrorContext {
        PlaybackErrorContext {
            delivery: DeliveryClass::Hls,
            selected: QualityClass::Auto,
            requested: QualityClass::M22,
            declared_rate: RateClass::M3To6,
            media_rate: RateClass::M1To3,
            raster: RasterClass::Uhd,
            pipeline: PipelineClass::Streaming,
            http: HttpClass::ServerError,
            buffer: BufferClass::S3To10,
            started: true,
            refusal: None,
        }
    }

    #[test]
    fn handled_error_is_one_grouped_event_with_the_typed_causal_sequence() {
        // Legacy-wire fixture: builds before 2026-08-31 emitted this destructive probe sequence.
        // It remains serializable so historical dashboards keep stable codes; preview_event above
        // deliberately demonstrates the current non-destructive SampleSource path instead.
        let trace = [
            TraceStep {
                age: TraceAge::Under1s,
                event: TraceEvent::Requested {
                    selected: QualityClass::Auto,
                },
            },
            TraceStep {
                age: TraceAge::S1To3,
                event: TraceEvent::Presented {
                    delivery: DeliveryClass::Hls,
                    requested: QualityClass::M22,
                    declared_rate: RateClass::M3To6,
                    raster: RasterClass::Hd,
                },
            },
            TraceStep {
                age: TraceAge::S30To120,
                event: TraceEvent::OriginalProbe {
                    phase: OriginalProbePhase::RetireHls,
                    outcome: TraceOutcome::Deadline,
                },
            },
            TraceStep {
                age: TraceAge::S30To120,
                event: TraceEvent::Failed {
                    kind: FailureClass::OriginalRollback,
                },
            },
        ];
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::OriginalRollback,
            context(),
            &trace,
        ))
        .expect("handled event JSON");
        assert_eq!(v["exception"]["values"][0]["mechanism"]["handled"], true);
        assert_eq!(
            v["fingerprint"],
            serde_json::json!(["playback-error", "original_rollback"])
        );
        assert_eq!(v["contexts"]["playback"]["requested_quality"], "22m");
        assert_eq!(v["contexts"]["playback"]["declared_rate"], "3-6m");
        let crumbs = v["breadcrumbs"]["values"].as_array().expect("breadcrumbs");
        assert_eq!(crumbs.len(), 4);
        assert_eq!(crumbs[1]["data"]["requested"], "22m");
        assert_eq!(crumbs[1]["data"]["declared_rate"], "3-6m");
        assert_eq!(crumbs[2]["data"]["phase"], "retire_hls");
        assert_eq!(crumbs[2]["data"]["outcome"], "deadline");
    }

    /// Regression: a handled playback failure must carry the same `hardware`/`webos` sandbox
    /// contexts a native crash carries, not just its own `playback` context.
    #[test]
    fn handled_playback_error_carries_the_same_hardware_context_as_a_crash() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::PlaybackInterrupted,
            context(),
            &[],
        ))
        .expect("handled event JSON");
        assert!(
            v["contexts"]["hardware"].is_object(),
            "no hardware context: {v}"
        );
        assert!(v["contexts"]["hardware"]["rtkmem"].is_string());
        assert!(v["contexts"]["hardware"]["install"].is_string());
        assert!(v["contexts"]["hardware"]["soc"].is_string());
        assert_eq!(v["contexts"]["webos"]["type"], "webos");
        assert!(v["contexts"]["webos"]["release"].is_string());
        // The playback-specific context must still be there beside the two new ones.
        assert!(v["contexts"]["playback"].is_object());
    }

    #[test]
    fn handled_error_schema_has_no_content_or_identity_slots() {
        fn keys(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    for (k, v) in m {
                        out.push(k.clone());
                        keys(v, out);
                    }
                }
                Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
                _ => {}
            }
        }
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        let mut all = Vec::new();
        keys(&v, &mut all);
        for forbidden in [
            "title",
            "rating_key",
            "url",
            "path",
            "position",
            "duration",
            "host",
            "address",
            "token",
            "email",
            "username",
            "ip_address",
            "request",
        ] {
            assert!(
                !all.iter().any(|k| k == forbidden),
                "forbidden key {forbidden}: {all:?}"
            );
        }
        // The one identity slot is the crash-report id, as `user.id` and nothing beside it.
        let user = v["user"].as_object().expect("user object");
        assert_eq!(user.keys().collect::<Vec<_>>(), vec!["id"]);
        assert_eq!(v["user"]["id"], super::super::native::PREVIEW_USER_ID);
    }

    /// With no crash-report id there is no `user` key at all — never an empty object, which
    /// Relay would still count as a user.
    #[test]
    fn no_errors_id_means_no_user_key() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            FailureClass::OriginalRollback,
            context(),
            &[],
        ))
        .expect("handled event JSON");
        assert!(v.get("user").is_none());
    }

    #[test]
    fn handled_error_schema_keys_are_exact_for_every_breadcrumb_shape() {
        use crate::telemetry::classes::{DeliveryReason, TraceDirection};

        fn keys(v: &Value) -> Vec<&str> {
            let mut out: Vec<_> = v
                .as_object()
                .expect("object")
                .keys()
                .map(String::as_str)
                .collect();
            out.sort_unstable();
            out
        }

        let trace = [
            TraceEvent::Requested {
                selected: QualityClass::Auto,
            },
            TraceEvent::Presented {
                delivery: DeliveryClass::Hls,
                requested: QualityClass::M22,
                declared_rate: RateClass::M3To6,
                raster: RasterClass::Hd,
            },
            TraceEvent::SeekRequested,
            TraceEvent::QualitySelected {
                selected: QualityClass::Original,
            },
            TraceEvent::DeliveryRequested {
                delivery: DeliveryClass::Direct,
                requested: QualityClass::Original,
                reason: DeliveryReason::OriginalRecovery,
            },
            TraceEvent::HlsCommitted {
                direction: TraceDirection::Up,
                requested: QualityClass::M22,
            },
            TraceEvent::OriginalProbe {
                phase: OriginalProbePhase::RestoreHls,
                outcome: TraceOutcome::Inconclusive,
            },
            TraceEvent::Failed {
                kind: FailureClass::OriginalRollback,
            },
        ]
        .map(|event| TraceStep {
            age: TraceAge::S3To10,
            event,
        });
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::OriginalRollback,
            context(),
            &trace,
        ))
        .expect("handled event JSON");

        assert_eq!(
            keys(&v),
            [
                "breadcrumbs",
                "contexts",
                "culprit",
                "dist",
                "environment",
                "event_id",
                "exception",
                "fingerprint",
                "level",
                "logger",
                "platform",
                "release",
                "sdk",
                "tags",
                "transaction",
                "user",
            ]
        );
        assert_eq!(keys(&v["sdk"]), ["name", "version"]);
        assert_eq!(keys(&v["contexts"]), ["hardware", "playback", "webos"]);
        assert_eq!(
            keys(&v["contexts"]["webos"]),
            ["api", "codename", "name", "release", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["hardware"]),
            ["install", "model", "revision", "rtkmem", "soc", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["playback"]),
            [
                "buffer",
                "declared_rate",
                "delivery",
                "http",
                "media_rate",
                "pipeline",
                "raster",
                "requested_quality",
                "selected_quality",
                "started",
                "type",
            ]
        );
        assert_eq!(
            keys(&v["tags"]),
            [
                "playback.declared_rate",
                "playback.delivery",
                "playback.kind",
                "playback.requested_quality",
                "playback.selected_quality",
                "playback.started",
            ]
        );
        assert_eq!(keys(&v["exception"]), ["values"]);
        assert_eq!(keys(&v["breadcrumbs"]), ["values"]);
        assert_eq!(
            keys(&v["exception"]["values"][0]),
            ["mechanism", "type", "value"]
        );
        assert_eq!(
            keys(&v["exception"]["values"][0]["mechanism"]),
            ["handled", "type"]
        );

        let crumbs = v["breadcrumbs"]["values"].as_array().expect("breadcrumbs");
        let want_data = [
            vec!["elapsed", "selected"],
            vec![
                "declared_rate",
                "delivery",
                "elapsed",
                "raster",
                "requested",
            ],
            vec!["elapsed"],
            vec!["elapsed", "selected"],
            vec!["delivery", "elapsed", "reason", "requested"],
            vec!["direction", "elapsed", "requested"],
            vec!["elapsed", "outcome", "phase"],
            vec!["elapsed", "kind"],
        ];
        assert_eq!(crumbs.len(), want_data.len());
        for (crumb, want) in crumbs.iter().zip(want_data) {
            assert_eq!(
                keys(crumb),
                ["category", "data", "level", "message", "type"]
            );
            assert_eq!(keys(&crumb["data"]), want);
        }
    }

    /// A refused plan's report carries the refusal block as tags (to search and group on) and as a
    /// context (to read), every value a closed code; a report with no refusal carries neither, which
    /// `handled_error_schema_keys_are_exact_for_every_breadcrumb_shape` pins key by key.
    #[test]
    fn a_refusal_context_adds_exactly_its_closed_tags_and_context_keys() {
        fn keys(v: &Value) -> Vec<&str> {
            let mut out: Vec<_> = v.as_object().expect("object").keys().map(String::as_str).collect();
            out.sort_unstable();
            out
        }
        let mut ctx = context();
        ctx.delivery = DeliveryClass::Unknown;
        ctx.requested = QualityClass::Unknown;
        ctx.refusal = Some(RefusalContext {
            general: DecisionCodeClass::C2000,
            transcode: DecisionCodeClass::C4007,
            attempted: DeliveryClass::Remux,
            source_video: VideoCodecClass::Hevc,
            source_audio: AudioCodecClass::TrueHd,
        });
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::DecisionRefused,
            ctx,
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(
            keys(&v["contexts"]),
            ["hardware", "playback", "refusal", "webos"]
        );
        assert_eq!(
            keys(&v["contexts"]["refusal"]),
            ["attempted_delivery", "general_code", "source_audio", "source_video", "transcode_code", "type"]
        );
        assert_eq!(v["contexts"]["refusal"]["general_code"], "2000");
        assert_eq!(v["contexts"]["refusal"]["transcode_code"], "4007");
        assert_eq!(v["contexts"]["refusal"]["attempted_delivery"], "original_remux");
        assert_eq!(v["contexts"]["refusal"]["source_video"], "hevc");
        assert_eq!(v["contexts"]["refusal"]["source_audio"], "truehd");
        assert_eq!(
            keys(&v["tags"]),
            [
                "playback.attempted_delivery",
                "playback.declared_rate",
                "playback.delivery",
                "playback.kind",
                "playback.refusal_general",
                "playback.refusal_transcode",
                "playback.requested_quality",
                "playback.selected_quality",
                "playback.source_audio",
                "playback.source_video",
                "playback.started",
            ]
        );
        assert_eq!(v["tags"]["playback.delivery"], "unknown", "no route was installed");
        assert_eq!(v["tags"]["playback.refusal_transcode"], "4007");
        assert_eq!(v["tags"]["playback.attempted_delivery"], "original_remux");
        assert_eq!(v["tags"]["playback.source_video"], "hevc");
        // Grouping is unchanged: the fingerprint is the failure kind, as before.
        assert_eq!(v["fingerprint"], serde_json::json!(["playback-error", "decision_refused"]));
    }

    #[test]
    fn the_consent_sample_carries_every_key_a_refusal_report_can() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        assert!(v["contexts"]["refusal"].is_object(), "{v}");
        assert!(v["tags"]["playback.refusal_general"].is_string());
    }

    #[test]
    fn consent_preview_and_privacy_name_every_refusal_domain_value() {
        use crate::telemetry::classes::{AudioCodecClass, DecisionCodeClass, VideoCodecClass};
        let legend = preview_domains();
        let privacy = include_str!("../../../PRIVACY.md");
        let mut values: Vec<&str> = Vec::new();
        values.extend(DecisionCodeClass::ALL.iter().map(|c| c.code()));
        values.extend(VideoCodecClass::ALL.iter().map(|c| c.code()));
        values.extend(AudioCodecClass::ALL.iter().map(|c| c.code()));
        for value in values {
            assert!(legend.contains(value), "preview domain omitted {value}");
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
        // The whole domain, in order, once per field: the consent screen lists it as written.
        assert!(legend.contains("absent / 2000 / 2003 / 4007 / other_1xxx / other_2xxx / other_3xxx / other_4xxx / other"));
        assert!(legend.contains("original_remux / hls / progressive_transcode"));
        assert!(legend.contains("unknown / h264 / hevc / av1 / vp9 / mpeg2 / other"));
        assert!(legend.contains("unknown / aac / ac3 / eac3 / truehd / dts / flac / mp3 / opus / other"));
    }

    #[test]
    fn consent_preview_lists_every_delivery_code_including_the_refused_unknown() {
        // A refused plan reports `unknown`; a value the wire can carry but the consent screen does
        // not list would be a payload the person was never shown.
        assert!(preview_domains()
            .contains("unknown / original_direct / original_remux / hls / progressive_transcode"));
    }

    #[test]
    fn consent_preview_and_privacy_name_the_closed_failure_domains() {
        let legend = preview_domains();
        let privacy = include_str!("../../../PRIVACY.md");
        for value in [
            "playback_interrupted",
            "original_rollback",
            "inconclusive",
            "server_state",
            "original_open_rollback",
            "refresh",
        ] {
            assert!(legend.contains(value), "preview domain omitted {value}");
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
    }
}
