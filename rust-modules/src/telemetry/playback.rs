//! Handled playback errors for Sentry: one terminal event, preceded by a bounded typed trace.
//!
//! This is deliberately not a log uploader. Every value accepted here is a closed enum from
//! `player::report`; there is no title, URL, rating key, playhead, duration, measured bitrate or
//! free-text error slot. Sparse transitions are retained in memory only while error reporting is
//! enabled, and nothing is queued unless the viewer actually reaches `PlaybackState::Error`.

use crate::player::report::{PlaybackErrorContext, TraceEvent, TraceStep};
use crate::player::FailureKind;
use serde_json::{Map, Value};

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
                    crate::player::report::TraceOutcome::Deadline
                        | crate::player::report::TraceOutcome::Transport
                        | crate::player::report::TraceOutcome::Inconclusive
                        | crate::player::report::TraceOutcome::ServerState
                        | crate::player::report::TraceOutcome::Refused
                ) {
                    "error"
                } else {
                    "info"
                },
            )
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

/// Pure body builder. `dist` is passed in so the consent preview can exercise this exact
/// serialiser without reading `/proc/self/exe` or minting an id before consent.
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    kind: FailureKind,
    context: PlaybackErrorContext,
    trace: &[TraceStep],
) -> Vec<u8> {
    let code = kind.code();
    let breadcrumbs: Vec<Value> = trace.iter().copied().map(breadcrumb).collect();
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("CARGO_PKG_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("CARGO_PKG_VERSION")},
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
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Queue one handled event and ask the existing background sender to flush it. No network work is
/// performed on the render thread.
pub(crate) fn report_error(kind: FailureKind, context: PlaybackErrorContext, trace: &[TraceStep]) {
    if !super::consent::allows_errors() || !super::sender::has_sentry() {
        return;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — handled playback error was not queued");
        return;
    };
    let body = event_body(&event_id, super::sentry::build_id(), kind, context, trace);
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, super::consent::allows_errors) {
        Some(true) => super::flush_soon(),
        Some(false) => {
            crate::log("telemetry: handled playback error did not fit the durable spool")
        }
        None => {} // consent changed while the event was being shaped
    }
}

/// Representative handled-error payload built through the real serializer. Per-report random and
/// runtime-build values are visible placeholders; the other values are representative members of
/// the closed domains disclosed beside the preview. No consent-time identifier is minted.
pub(crate) fn preview_event() -> Vec<u8> {
    use crate::player::report::{
        BufferClass, DeliveryClass, DeliveryReason, HttpClass, OriginalProbePhase, PipelineClass,
        QualityClass, RasterClass, RateClass, TraceAge, TraceDirection, TraceOutcome,
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
                kind: FailureKind::PlaybackInterrupted,
            },
        },
    ];
    event_body(
        "<random per-error event id>",
        "<running ELF build id>",
        FailureKind::PlaybackInterrupted,
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
    use crate::player::report::{
        BufferClass as B, DeliveryClass as D, DeliveryReason as W, HttpClass as H,
        OriginalProbePhase as P, PipelineClass as L, QualityClass as Q, RasterClass as X,
        RateClass as R, TraceAge as A, TraceDirection as I, TraceOutcome as O,
    };
    use FailureKind as F;
    format!(
        "Closed handled-playback domains:\n\
         failure kind (including retained historical codes): {}\n\
         delivery: {}\n\
         quality: {}\n\
         observed rate: {}\n\
         raster: {}\n\
         pipeline: {}\n\
         HTTP: {}\n\
         buffer: {}\n\
         elapsed: {}\n\
         HLS direction: {}\n\
         delivery reason: {}\n\
         Original phase (including retained historical codes): {}\n\
         Original outcome: {}",
        codes(
            &[
                F::DecisionRefused,
                F::NoVideoTranscodeTarget,
                F::NoVideoTrack,
                F::MediaSource,
                F::PlaybackInterrupted,
                F::TvPipeline,
                F::OriginalRollback,
                F::Unspecified,
            ],
            F::code,
        ),
        codes(&[D::Direct, D::Remux, D::Hls, D::Transcode], D::code),
        codes(
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
        ),
        codes(
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
        ),
        codes(&[X::Unknown, X::Sd, X::Hd, X::Fhd, X::Uhd], X::code),
        codes(&[L::Loading, L::Playing, L::Bound, L::Streaming], L::code),
        codes(
            &[
                H::None,
                H::Success,
                H::ClientError,
                H::ServerError,
                H::Other
            ],
            H::code
        ),
        codes(
            &[
                B::Unknown,
                B::Empty,
                B::Under3s,
                B::S3To10,
                B::S10To30,
                B::Over30s
            ],
            B::code
        ),
        codes(
            &[
                A::Under1s,
                A::S1To3,
                A::S3To10,
                A::S10To30,
                A::S30To120,
                A::Over2m
            ],
            A::code
        ),
        codes(&[I::Up, I::Down, I::Refresh], I::code),
        codes(
            &[
                W::LinkFallback,
                W::OriginalRecovery,
                W::OriginalOpenRollback
            ],
            W::code
        ),
        codes(
            &[
                P::RetireHls,
                P::SampleSource,
                P::CloseSource,
                P::RestoreHls,
                P::OpenHls,
                P::CommitHls
            ],
            P::code
        ),
        codes(
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
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::report::{
        BufferClass, DeliveryClass, HttpClass, OriginalProbePhase, PipelineClass, QualityClass,
        RasterClass, RateClass, TraceAge, TraceOutcome,
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
                    kind: FailureKind::OriginalRollback,
                },
            },
        ];
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            FailureKind::OriginalRollback,
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
            "user",
            "request",
        ] {
            assert!(
                !all.iter().any(|k| k == forbidden),
                "forbidden key {forbidden}: {all:?}"
            );
        }
    }

    #[test]
    fn handled_error_schema_keys_are_exact_for_every_breadcrumb_shape() {
        use crate::player::report::{DeliveryReason, TraceDirection};

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
                kind: FailureKind::OriginalRollback,
            },
        ]
        .map(|event| TraceStep {
            age: TraceAge::S3To10,
            event,
        });
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            FailureKind::OriginalRollback,
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
            ]
        );
        assert_eq!(keys(&v["sdk"]), ["name", "version"]);
        assert_eq!(keys(&v["contexts"]), ["playback"]);
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
