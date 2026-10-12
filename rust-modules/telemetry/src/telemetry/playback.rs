//! Handled playback errors for Sentry: one terminal event, preceded by a bounded typed trace.
//!
//! This is deliberately not a log uploader. Every value accepted here is a closed enum from
//! [`super::classes`], a bool (`attempt_started`), a bounded ordinal (`attempt_ordinal`: `1` to `9`,
//! then `9+`), or one of two bounded integers: `http_status` (100-599, else 0) and `pipeline_error`
//! (the firmware's own `i32` error number). `attempt_origin` and `attempt_ordinal` say why the
//! failed engine open was made and which open of the playback it was. The fingerprint is
//! `["playback-error", kind]`, except for origin `preview` (a failed detail-page trailer), which
//! groups apart as `["playback-error", "preview", kind]`. There is no
//! title, URL, rating key, playhead, duration, measured bitrate or free-text error slot. Sparse transitions are retained in memory only while error reporting is
//! enabled, and nothing is queued unless the viewer actually reaches `PlaybackState::Error`.

use super::classes::{
    AttemptOrigin, FailureClass, PlaybackErrorContext, TraceEvent, TraceOutcome, TraceStep,
};
use serde_json::{Map, Value};

/// How telemetry erases the in-memory trace the player keeps for the current attempt. The trace
/// lives in the player's shared state, which this layer cannot name, so the player's eraser is
/// installed at boot (`player::report::install_trace_eraser`, from `app::enter_application`),
/// before this layer first loads a decision and before anything can play. Not when the first full
/// attempt arms a trace: a failed detail-page preview seals a trace without ever arming one.
static CLEAR_ERROR_TRACE: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Register the function [`clear_error_trace`] calls. The first registration wins.
pub fn install_error_trace_clear(clear: fn()) {
    let _ = CLEAR_ERROR_TRACE.set(clear);
}

/// Forget the in-memory playback trace immediately, as error reporting is withdrawn or the account
/// that consented ends. A no-op until the eraser is installed, which a booted app does first.
pub fn clear_error_trace() {
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
pub fn event_body(
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
        "release": super::release(),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": super::app_version()},
        "logger": "playback",
        "transaction": "playback",
        "culprit": format!("playback::{code}"),
        // A preview is not a playback the viewer asked for: its failures group apart. Every other
        // origin keeps `["playback-error", code]` exactly.
        "fingerprint": if context.attempt_origin == AttemptOrigin::Preview {
            serde_json::json!(["playback-error", "preview", code])
        } else {
            serde_json::json!(["playback-error", code])
        },
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
            "playback.attempted_delivery": context.attempted_delivery.code(),
            "playback.attempt_started": if context.attempt_started { "yes" } else { "no" },
            "playback.attempt_origin": context.attempt_origin.code(),
            "playback.attempt_ordinal": context.attempt_ordinal.code(),
            "playback.http_status": context.http_status.to_string(),
            "playback.transport": context.transport.code(),
            "playback.transfer_age": context.transfer_age.code(),
            "playback.pipeline_error": context.pipeline_error.to_string(),
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
            "http_status": context.http_status,
            "attempted_delivery": context.attempted_delivery.code(),
            "attempt_started": context.attempt_started,
            "attempt_origin": context.attempt_origin.code(),
            "attempt_ordinal": context.attempt_ordinal.code(),
            "buffer": context.buffer.code(),
            "transport": context.transport.code(),
            "transfer_age": context.transfer_age.code(),
            "pipeline_error": context.pipeline_error,
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

/// How many times the player has asked for a handled report, whether or not consent let one
/// through. Test seam: "exactly one report for the user-visible failure" is a count of calls.
#[cfg(any(test, feature = "test-support"))]
static REPORT_CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// See [`REPORT_CALLS`].
#[cfg(any(test, feature = "test-support"))]
pub fn report_calls_for_test() -> u32 {
    REPORT_CALLS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Queue one handled event and ask the existing background sender to flush it. No network work is
/// performed on the render thread.
pub fn report_error(kind: FailureClass, context: PlaybackErrorContext, trace: &[TraceStep]) {
    #[cfg(any(test, feature = "test-support"))]
    REPORT_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !super::consent::allows_errors() || !super::sender::has_sentry() {
        return;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        plx_base::eventlog::log("telemetry: no /dev/urandom — handled playback error was not queued");
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
        // `Some(false)` is a failed write, not a size verdict: no spool path is writable (the app
        // directory is read-only on that install), or the write itself failed. An oversized
        // record logs its own cap line, so this wording must not claim "did not fit".
        Some(false) => plx_base::eventlog::log(
            "telemetry: handled playback error was not written to the durable spool \
             (no writable spool path, or the write failed)",
        ),
        None => {} // consent changed while the event was being shaped
    }
}

/// Representative handled-error payload built through the real serializer. Per-report random and
/// runtime-build values are visible placeholders; the other values are representative members of
/// the closed domains disclosed beside the preview. No consent-time identifier is minted.
pub fn preview_event() -> Vec<u8> {
    use crate::telemetry::classes::{
        AttemptOrdinal, AttemptOrigin, AudioCodecClass, BufferClass, DecisionCodeClass, DeliveryClass,
        DeliveryReason, HttpClass, OriginalProbePhase, PipelineClass, QualityClass, RasterClass, RateClass, RefusalContext,
        TraceAge, TraceDirection, TraceOutcome, TransferAgeClass, TransportClass, VideoCodecClass,
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
            transport: TransportClass::RecvError,
            transfer_age: TransferAgeClass::S30To90,
            started: true,
            attempted_delivery: DeliveryClass::Direct,
            attempt_started: false,
            attempt_origin: AttemptOrigin::QualityChange,
            attempt_ordinal: AttemptOrdinal::new(2),
            http_status: 404,
            pipeline_error: 601,
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
pub fn preview_domains() -> String {
    use crate::telemetry::classes::{
        BufferClass as B, DeliveryClass as D, DeliveryReason as W, HttpClass as H,
        OriginalProbePhase as P, PipelineClass as L, QualityClass as Q, RasterClass as X,
        RateClass as R, TraceAge as A, TraceDirection as I, TraceOutcome as O,
        TransferAgeClass as T, TransportClass as K,
    };
    use crate::telemetry::classes::{AudioCodecClass as Z, DecisionCodeClass as N, VideoCodecClass as V};
    use crate::telemetry::classes::{AttemptOrdinal, AttemptOrigin};
    use FailureClass as F;
    use plx_platform::i18n::msg;
    // The labels are the reader's words; the codes after them are the wire values themselves.
    let domains: [(&str, String); 24] = [
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
        // The route the failed attempt was opening rides every report (a refusal's own block names
        // the same value), so its row is the whole delivery domain.
        (msg::core_preview_domain_attempted_delivery(), codes(&[D::Unknown, D::Direct, D::Remux, D::Hls, D::Transcode], D::code)),
        (msg::core_preview_domain_attempt_started(), "yes / no".to_owned()),
        (msg::core_preview_domain_attempt_origin(), codes(&AttemptOrigin::ALL, AttemptOrigin::code)),
        (msg::core_preview_domain_attempt_ordinal(), codes(&AttemptOrdinal::all().collect::<Vec<_>>(), AttemptOrdinal::code)),
        // The three rows below ride a `decision_refused` report only (`PlaybackErrorContext::
        // refusal`); the labels say so, and each list is its enum's own `ALL`/variant set.
        (msg::core_preview_domain_refusal_codes(), codes(&N::ALL, N::code)),
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
        // The one NUMERIC field, so its domain is its range rather than a list of words: the TV
        // firmware's own error number (0 = none recorded), already clamped to `i32`.
        (msg::core_preview_domain_pipeline_error(), format!("{} to {}", i32::MIN, i32::MAX)),
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
        // Numeric, so its domain is its range: the exact status beside the class above (0 = none).
        (msg::core_preview_domain_http_status(), "0 / 100 to 599".to_owned()),
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
        (msg::core_preview_domain_transport(), codes(&K::ALL, K::code)),
        (msg::core_preview_domain_transfer_age(), codes(&T::ALL, T::code)),
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
        (msg::core_preview_domain_delivery_reason(), codes(&W::ALL, W::code)),
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
        AttemptOrdinal, AttemptOrigin, AudioCodecClass, BufferClass, DecisionCodeClass,
        DeliveryClass, HttpClass, OriginalProbePhase, PipelineClass, QualityClass, RasterClass, RateClass, RefusalContext,
        TraceAge, TraceOutcome, TransferAgeClass, TransportClass, VideoCodecClass,
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
            transport: TransportClass::None,
            transfer_age: TransferAgeClass::Unknown,
            started: true,
            attempted_delivery: DeliveryClass::Hls,
            attempt_started: true,
            attempt_origin: AttemptOrigin::Fresh,
            attempt_ordinal: AttemptOrdinal::FIRST,
            http_status: 0,
            pipeline_error: 0,
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
            ["install", "model", "probe", "revision", "rtkmem", "soc", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["playback"]),
            [
                "attempt_ordinal",
                "attempt_origin",
                "attempt_started",
                "attempted_delivery",
                "buffer",
                "declared_rate",
                "delivery",
                "http",
                "http_status",
                "media_rate",
                "pipeline",
                "pipeline_error",
                "raster",
                "requested_quality",
                "selected_quality",
                "started",
                "transfer_age",
                "transport",
                "type",
            ]
        );
        assert_eq!(
            keys(&v["tags"]),
            [
                "playback.attempt_ordinal",
                "playback.attempt_origin",
                "playback.attempt_started",
                "playback.attempted_delivery",
                "playback.declared_rate",
                "playback.delivery",
                "playback.http_status",
                "playback.kind",
                "playback.pipeline_error",
                "playback.requested_quality",
                "playback.selected_quality",
                "playback.started",
                "playback.transfer_age",
                "playback.transport",
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
                "playback.attempt_ordinal",
                "playback.attempt_origin",
                "playback.attempt_started",
                "playback.attempted_delivery",
                "playback.declared_rate",
                "playback.delivery",
                "playback.http_status",
                "playback.kind",
                "playback.pipeline_error",
                "playback.refusal_general",
                "playback.refusal_transcode",
                "playback.requested_quality",
                "playback.selected_quality",
                "playback.source_audio",
                "playback.source_video",
                "playback.started",
                "playback.transfer_age",
                "playback.transport",
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
        let privacy = include_str!("../../../../PRIVACY.md");
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
        let privacy = include_str!("../../../../PRIVACY.md");
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

    /// **Issue #504.** A transfer that dies mid-body used to reach Sentry as a bare
    /// `playback_interrupted`. The libcurl result class and the age of the transfer ride along as
    /// closed codes, as a searchable tag AND in the playback context, and never as a number.
    #[test]
    fn a_transport_failure_is_tagged_and_in_the_context_by_class_and_age_bucket() {
        let mut ctx = context();
        ctx.transport = TransportClass::RecvError;
        ctx.transfer_age = TransferAgeClass::S30To90;
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::PlaybackInterrupted,
            ctx,
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(v["tags"]["playback.transport"], "recv_error");
        assert_eq!(v["tags"]["playback.transfer_age"], "30-90s");
        assert_eq!(v["contexts"]["playback"]["transport"], "recv_error");
        assert_eq!(v["contexts"]["playback"]["transfer_age"], "30-90s");
        // The fingerprint is unchanged: the new fields explain a group, they do not split it.
        assert_eq!(
            v["fingerprint"],
            serde_json::json!(["playback-error", "playback_interrupted"])
        );
        // A report with no transport failure says so in words, rather than omitting the keys.
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            FailureClass::PlaybackInterrupted,
            context(),
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(v["tags"]["playback.transport"], "none");
        assert_eq!(v["tags"]["playback.transfer_age"], "unknown");
    }

    /// The mapping from the code libcurl returned: every code a mid-body death produces has its
    /// own class, anything else is `other` (never the number), and success is `none`.
    #[test]
    fn transport_class_maps_the_mid_body_curl_codes_and_nothing_leaks() {
        for (rc, want) in [
            (0, TransportClass::None),
            (18, TransportClass::Partial),
            (56, TransportClass::RecvError),
            (55, TransportClass::SendError),
            (52, TransportClass::GotNothing),
            (28, TransportClass::Timeout),
            (16, TransportClass::Http2),
            (92, TransportClass::Http2),
            (7, TransportClass::Other),
            (35, TransportClass::Other),
            (-1, TransportClass::Other),
            (9_999, TransportClass::Other),
        ] {
            assert_eq!(TransportClass::from_curl_rc(rc), want, "rc {rc}");
        }
        let mut codes: Vec<&str> = TransportClass::ALL.iter().map(|c| c.code()).collect();
        let n = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), n, "every class has its own code");
        assert!(
            codes.iter().all(|c| c.parse::<i64>().is_err()),
            "a code is a word, never the libcurl number: {codes:?}"
        );
    }

    /// The age bucket edges: a fixed-timeout cut (the suspected 60 s proxy) must land in ONE
    /// bucket whatever the jitter, and the edges must not drift.
    #[test]
    fn transfer_age_buckets_hold_a_fixed_timeout_in_one_bucket() {
        for (ms, want) in [
            (-1, TransferAgeClass::Unknown),
            (0, TransferAgeClass::Under5s),
            (4_999, TransferAgeClass::Under5s),
            (5_000, TransferAgeClass::S5To30),
            (29_999, TransferAgeClass::S5To30),
            (30_000, TransferAgeClass::S30To90),
            (59_000, TransferAgeClass::S30To90),
            (60_000, TransferAgeClass::S30To90),
            (61_500, TransferAgeClass::S30To90),
            (89_999, TransferAgeClass::S30To90),
            (90_000, TransferAgeClass::S90To300),
            (299_999, TransferAgeClass::S90To300),
            (300_000, TransferAgeClass::Over300s),
            (86_400_000, TransferAgeClass::Over300s),
        ] {
            assert_eq!(TransferAgeClass::from_ms(ms), want, "age {ms} ms");
        }
    }

    /// The consent screen shows the person every value the wire can carry, in their own words, and
    /// `PRIVACY.md` names them too: a value in the payload that neither lists is a payload nobody
    /// was shown.
    #[test]
    fn consent_preview_and_privacy_name_every_transport_and_age_value() {
        let legend = preview_domains();
        let privacy = include_str!("../../../../PRIVACY.md");
        for value in TransportClass::ALL
            .iter()
            .map(|c| c.code())
            .chain(TransferAgeClass::ALL.iter().map(|c| c.code()))
        {
            assert!(legend.contains(value), "preview domain omitted {value}");
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
        assert!(legend.contains(
            "none / partial / recv_error / send_error / got_nothing / timeout / http2 / reopen_failed / other"
        ));
        assert!(legend.contains("unknown / <5s / 5-30s / 30-90s / 90-300s / 300s+"));
    }

    /// **The attempt that failed, beside the session it failed in.** After a mid-play switch whose
    /// new route never opened, `delivery` is the route the session had and `started` is true for
    /// the whole session; the report also says which route was being opened, whether THAT open
    /// ever put a picture up, and the exact HTTP status the source answered with. Closed values
    /// and one bounded integer, as a tag to search on and in the playback context.
    #[test]
    fn the_failed_attempt_is_tagged_and_in_the_context_beside_the_session_fields() {
        let mut ctx = context();
        ctx.delivery = DeliveryClass::Hls;
        ctx.started = true;
        ctx.attempted_delivery = DeliveryClass::Direct;
        ctx.attempt_started = false;
        ctx.http = HttpClass::ClientError;
        ctx.http_status = 404;
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::MediaSource,
            ctx,
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(v["tags"]["playback.delivery"], "hls", "the session's route stays for continuity");
        assert_eq!(v["tags"]["playback.attempted_delivery"], "original_direct");
        assert_eq!(v["tags"]["playback.started"], "yes");
        assert_eq!(v["tags"]["playback.attempt_started"], "no");
        assert_eq!(v["tags"]["playback.http_status"], "404");
        let c = &v["contexts"]["playback"];
        assert_eq!(c["attempted_delivery"], "original_direct");
        assert_eq!(c["attempt_started"], false);
        assert_eq!(c["started"], true);
        assert_eq!(c["http"], "4xx");
        assert_eq!(c["http_status"], 404);
        assert!(c["http_status"].is_u64(), "an integer, not a string");
        // Grouping is unchanged: these explain a group, they do not split it.
        assert_eq!(v["fingerprint"], serde_json::json!(["playback-error", "media_source"]));
    }

    /// **Why the failed attempt was made, and which open of the playback it was.** Both ride as a
    /// tag to search on and in the playback context, as closed strings (the ordinal is `1`..`9`
    /// then `9+`, a string in both places so the saturated value fits). Neither the cause nor the
    /// ordinal splits a group, with one exception: the `preview` origin groups apart under its own
    /// fingerprint segment, because a trailer is not a playback (asserted below).
    #[test]
    fn the_attempt_origin_and_ordinal_are_tagged_and_in_the_context_as_closed_strings() {
        for origin in AttemptOrigin::ALL {
            for (n, want) in [(1, "1"), (2, "2"), (9, "9"), (10, "9+"), (500, "9+"), (0, "1")] {
                let mut ctx = context();
                ctx.attempt_origin = origin;
                ctx.attempt_ordinal = AttemptOrdinal::new(n);
                let v: Value = serde_json::from_slice(&event_body(
                    &"a".repeat(32),
                    "0123456789abcdef",
                    Some(&"e".repeat(32)),
                    FailureClass::MediaSource,
                    ctx,
                    &[],
                ))
                .expect("handled event JSON");
                assert_eq!(v["tags"]["playback.attempt_origin"], origin.code());
                assert_eq!(v["contexts"]["playback"]["attempt_origin"], origin.code());
                assert_eq!(v["tags"]["playback.attempt_ordinal"], want, "ordinal {n}");
                assert_eq!(v["contexts"]["playback"]["attempt_ordinal"], want, "ordinal {n}");
                // The cause explains a group, it does not split it; the one exception is a
                // preview, which is not a playback at all and groups apart (below).
                let want_fingerprint = if origin == AttemptOrigin::Preview {
                    serde_json::json!(["playback-error", "preview", "media_source"])
                } else {
                    serde_json::json!(["playback-error", "media_source"])
                };
                assert_eq!(v["fingerprint"], want_fingerprint, "{}", origin.code());
            }
        }
        // The wire values are written out, never derived from a variant name.
        let codes: Vec<_> = AttemptOrigin::ALL.iter().map(|o| o.code()).collect();
        assert_eq!(
            codes,
            ["fresh", "user_retry", "quality_change", "auto_handoff", "recovery", "resume", "preview"],
        );
        assert_eq!(AttemptOrdinal::all().count(), 10, "1..=9 and the saturated value");
    }

    /// **A failed detail-page preview is its own Sentry issue.** A trailer is not a playback the
    /// viewer asked for, so its failures must not pile into the playback groups (PLX-NATIVE-15).
    /// The fingerprint carries `preview` for that origin alone; every other origin keeps the
    /// `["playback-error", code]` shape byte for byte, for every failure kind.
    #[test]
    fn only_a_preview_failure_groups_apart_and_every_other_fingerprint_is_unchanged() {
        use crate::telemetry::classes::FailureClass as F;
        for kind in [F::MediaSource, F::TvPipeline, F::DecisionRefused, F::Unspecified] {
            for origin in AttemptOrigin::ALL {
                let mut ctx = context();
                ctx.attempt_origin = origin;
                let v: Value = serde_json::from_slice(&event_body(
                    &"a".repeat(32),
                    "0123456789abcdef",
                    Some(&"e".repeat(32)),
                    kind,
                    ctx,
                    &[],
                ))
                .expect("handled event JSON");
                let want = if origin == AttemptOrigin::Preview {
                    serde_json::json!(["playback-error", "preview", kind.code()])
                } else {
                    serde_json::json!(["playback-error", kind.code()])
                };
                assert_eq!(v["fingerprint"], want, "{} / {}", origin.code(), kind.code());
                assert_eq!(v["tags"]["playback.kind"], kind.code(), "the kind is still the kind");
            }
        }
    }

    /// The consent screen shows what a report can carry: the sample event holds every new key, the
    /// legend names each field with its domain, and PRIVACY.md describes them.
    #[test]
    fn the_consent_sample_legend_and_privacy_list_every_attempt_field() {
        use plx_platform::i18n::msg;
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        for key in [
            "attempted_delivery",
            "attempt_started",
            "attempt_origin",
            "attempt_ordinal",
            "http_status",
        ] {
            assert!(v["contexts"]["playback"].get(key).is_some(), "sample context omitted {key}");
            assert!(v["tags"].get(format!("playback.{key}")).is_some(), "sample tags omitted {key}");
        }
        assert!(v["contexts"]["playback"]["http_status"].is_u64());
        let legend = preview_domains();
        for label in [
            msg::core_preview_domain_attempted_delivery(),
            msg::core_preview_domain_attempt_started(),
            msg::core_preview_domain_attempt_origin(),
            msg::core_preview_domain_attempt_ordinal(),
            msg::core_preview_domain_http_status(),
        ] {
            assert!(legend.contains(label), "legend omitted the row {label:?}");
        }
        assert!(legend.contains("yes / no"), "the attempt_started domain");
        assert!(legend.contains("100 to 599"), "the http_status domain");
        // Every origin and every ordinal the producer can send is listed, in the enum's own order.
        assert!(
            legend.contains(
                "fresh / user_retry / quality_change / auto_handoff / recovery / resume / preview"
            ),
            "the attempt_origin domain",
        );
        assert!(legend.contains("1 / 2 / 3 / 4 / 5 / 6 / 7 / 8 / 9 / 9+"), "the attempt_ordinal domain");
        assert!(legend.contains("quality_pick"), "the delivery reason the fixed-rung pick notes");
        // The attempted route rides EVERY report now, so its row lists the whole delivery domain
        // (including `unknown`), not only the three a refusal can name.
        let all = "unknown / original_direct / original_remux / hls / progressive_transcode";
        assert_eq!(legend.matches(all).count(), 2, "delivery and attempted delivery both list it");
        let privacy = include_str!("../../../../PRIVACY.md");
        for key in [
            "attempted_delivery",
            "attempt_started",
            "attempt_origin",
            "attempt_ordinal",
            "http_status",
        ] {
            assert!(privacy.contains(key), "PRIVACY.md omitted {key}");
        }
        for origin in AttemptOrigin::ALL {
            // Backticked, as PRIVACY.md writes every wire value: a bare word such as `preview`
            // already appears in its prose, so a bare match would pass for an omission.
            let quoted = format!("`{}`", origin.code());
            assert!(privacy.contains(&quoted), "PRIVACY.md omitted the origin {}", origin.code());
        }
        assert!(privacy.contains("9+") && privacy.contains("quality_pick"));
    }

    /// **The firmware's error number.** A `tv_pipeline` report used to say only that the pipeline
    /// failed; the number the firmware gave (601 = resource allocation) rides along as a tag to
    /// search on and as an integer in the playback context. It is a number, not a class: the
    /// callback's string is free text and has no field.
    #[test]
    fn a_pipeline_error_number_is_tagged_and_in_the_context_as_an_integer() {
        let mut ctx = context();
        ctx.pipeline_error = 601;
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            FailureClass::TvPipeline,
            ctx,
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(v["tags"]["playback.pipeline_error"], "601");
        assert_eq!(v["contexts"]["playback"]["pipeline_error"], 601);
        assert!(v["contexts"]["playback"]["pipeline_error"].is_i64(), "an integer, not a string");
        // Grouping is unchanged: the number explains a group, it does not split it.
        assert_eq!(v["fingerprint"], serde_json::json!(["playback-error", "tv_pipeline"]));
        // No number recorded says 0 in the same two places.
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            FailureClass::TvPipeline,
            context(),
            &[],
        ))
        .expect("handled event JSON");
        assert_eq!(v["tags"]["playback.pipeline_error"], "0");
        assert_eq!(v["contexts"]["playback"]["pipeline_error"], 0);
    }

    /// The consent screen lists the field and its whole domain, and `PRIVACY.md` names it: a key in
    /// the payload that neither mentions is a payload nobody was shown.
    #[test]
    fn consent_preview_and_privacy_name_the_pipeline_error_number() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        assert_eq!(v["contexts"]["playback"]["pipeline_error"], 601, "{v}");
        assert_eq!(v["tags"]["playback.pipeline_error"], "601");
        let legend = preview_domains();
        assert!(
            legend.contains(&format!("{} to {}", i32::MIN, i32::MAX)),
            "the legend states the whole integer range: {legend}"
        );
        let privacy = include_str!("../../../../PRIVACY.md");
        assert!(privacy.contains("pipeline_error"), "PRIVACY.md omitted pipeline_error");
    }

    #[test]
    fn the_consent_sample_carries_the_transport_keys() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        assert!(v["contexts"]["playback"]["transport"].is_string(), "{v}");
        assert!(v["contexts"]["playback"]["transfer_age"].is_string());
        assert!(v["tags"]["playback.transport"].is_string());
        assert!(v["tags"]["playback.transfer_age"].is_string());
    }
}
