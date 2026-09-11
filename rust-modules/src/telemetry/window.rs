//! Sparse window-lifecycle breadcrumbs attached to native crash events. No standalone usage
//! events and no per-frame trace: only lifecycle edges, WM queries and the first resumed frame.
//! Collection uses the existing crash-report consent. Values are closed labels and booleans,
//! plus SDL's three version bytes; no pointers, content, account data or runtime strings.

use serde_json::{Map, Value};

pub(crate) const LIMIT: usize = 16;

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    WillBackground,
    DidBackground,
    WillForeground,
    DidForeground,
    WmQuery,
    WmReady,
    WmFailed,
    WmWrongBackend,
    WmNoSurface,
    FirstFrame,
    FirstSwapComplete,
}

impl Stage {
    const ALL: [Self; 11] = [
        Self::WillBackground,
        Self::DidBackground,
        Self::WillForeground,
        Self::DidForeground,
        Self::WmQuery,
        Self::WmReady,
        Self::WmFailed,
        Self::WmWrongBackend,
        Self::WmNoSurface,
        Self::FirstFrame,
        Self::FirstSwapComplete,
    ];

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::WillBackground => "will_background",
            Self::DidBackground => "did_background",
            Self::WillForeground => "will_foreground",
            Self::DidForeground => "did_foreground",
            Self::WmQuery => "wm_query",
            Self::WmReady => "wm_ready",
            Self::WmFailed => "wm_failed",
            Self::WmWrongBackend => "wm_wrong_backend",
            Self::WmNoSurface => "wm_no_surface",
            Self::FirstFrame => "first_frame",
            Self::FirstSwapComplete => "first_swap_complete",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Observation {
    pub stage: Stage,
    pub playing: Option<bool>,
    pub version: Option<[u8; 3]>,
    pub display: Option<bool>,
    pub surface: Option<bool>,
}

impl Observation {
    pub(crate) fn step(stage: Stage, playing: Option<bool>) -> Self {
        Self {
            stage,
            playing,
            version: None,
            display: None,
            surface: None,
        }
    }
}

pub(crate) fn record(observation: Observation) {
    record_if_allowed(observation, super::native::record_window);
}

fn record_if_allowed(observation: Observation, emit: impl FnOnce(Observation)) {
    if super::consent::allows_errors() {
        emit(observation);
    }
}

pub(crate) fn lifecycle(event: u32, playing: bool) {
    let stage = match event {
        0x103 => Stage::WillBackground,
        0x104 => Stage::DidBackground,
        0x105 => Stage::WillForeground,
        0x106 => Stage::DidForeground,
        _ => return,
    };
    record(Observation::step(stage, Some(playing)));
}

// Sentry Native emits UTC RFC3339 timestamps with up to nine fractional digits. Retain only
// this fixed numeric shape, so event-to-crash timing survives without a free-text field.
fn timestamp(value: &Value) -> Option<&str> {
    let text = value.as_str()?;
    let b = text.as_bytes();
    if !(20..=30).contains(&b.len()) || b.last() != Some(&b'Z') {
        return None;
    }
    for (i, c) in b[..19].iter().enumerate() {
        let expected = match i {
            4 | 7 => Some(b'-'),
            10 => Some(b'T'),
            13 | 16 => Some(b':'),
            _ => None,
        };
        if expected.map_or(!c.is_ascii_digit(), |want| *c != want) {
            return None;
        }
    }
    if b.len() != 20
        && (b.len() < 22 || b[19] != b'.' || !b[20..b.len() - 1].iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    Some(text)
}

/// Reconstruct the closed schema rather than trusting SDK breadcrumb data. Native SDK versions
/// use either an array or {values: []}; accept both, drop every other breadcrumb family.
pub(super) fn sanitise(value: &Value) -> Value {
    let values = value
        .as_array()
        .or_else(|| value.get("values").and_then(Value::as_array));
    let mut clean: Vec<Value> = values.into_iter().flatten().filter_map(|v| {
        if v.get("category")?.as_str()? != "window" { return None; }
        let message = v.get("message")?.as_str()?;
        if !Stage::ALL.iter().any(|stage| stage.code() == message) { return None; }
        let source = v.get("data")?.as_object()?;
        let mut data = Map::new();
        for key in ["playing", "display", "surface"] {
            if let Some(value) = source.get(key) {
                data.insert(key.into(), Value::Bool(value.as_bool()?));
            }
        }
        for key in ["sdl_major", "sdl_minor", "sdl_patch"] {
            if let Some(value) = source.get(key) {
                let n = value.as_u64()?;
                if n > 255 { return None; }
                data.insert(key.into(), n.into());
            }
        }
        let mut breadcrumb = serde_json::json!({"type":"state", "category":"window", "level":"info",
            "message":message, "data":data});
        if let Some(stamp) = v.get("timestamp").and_then(timestamp) {
            breadcrumb["timestamp"] = stamp.into();
        }
        Some(breadcrumb)
    }).collect();
    if clean.len() > LIMIT {
        clean.drain(..clean.len() - LIMIT);
    }
    serde_json::json!({"values":clean})
}

pub(super) fn preview() -> Value {
    serde_json::json!({"values":[
        {"category":"window", "message":"did_foreground", "timestamp":"2000-01-01T00:00:00Z",
            "data":{"playing":true}},
        {"category":"window", "message":"wm_ready", "data":{"display":true, "surface":true,
            "sdl_major":2, "sdl_minor":0, "sdl_patch":5}},
        {"category":"window", "message":"first_frame", "data":{"playing":true}}
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs_are_bounded_and_drop_unknown_families_values_and_fields() {
        let good = serde_json::json!({"category":"window", "message":"wm_ready", "data":{
            "display":true,"surface":true,"sdl_major":2,"sdl_minor":0,"sdl_patch":5,
            "title":"private title", "pointer":"0xdeadbeef"}, "timestamp":"private", "url":"private"});
        let mut values = vec![good; LIMIT + 4];
        values.push(serde_json::json!({"category":"http", "message":"wm_ready", "data":{}}));
        values.push(serde_json::json!({"category":"window", "message":"private title", "data":{}}));
        values.push(serde_json::json!({"category":"window", "message":"wm_ready", "data":{"surface":"private"}}));
        values.push(serde_json::json!({"category":"window", "message":"wm_ready", "data":{"sdl_major":999}}));
        let out = sanitise(&Value::Array(values.clone()));
        assert_eq!(out["values"].as_array().unwrap().len(), LIMIT);
        let text = out.to_string();
        for bad in [
            "private",
            "title",
            "pointer",
            "deadbeef",
            "timestamp",
            "url",
            "999",
        ] {
            assert!(!text.contains(bad), "leaked {bad}");
        }
        assert_eq!(out, sanitise(&serde_json::json!({"values":values})));
    }

    #[test]
    fn withdrawal_stops_collection_before_the_native_backend() {
        let _guard = crate::testlock::serial();
        use crate::telemetry::consent::{self, Consent};
        let previous = consent::current();
        let step = Observation::step(Stage::WillBackground, Some(true));
        let mut captured = 0;
        consent::install(Consent::default());
        record_if_allowed(step, |_| captured += 1);
        let mut enabled = Consent::default();
        enabled.errors = true;
        enabled.asked_version = consent::POLICY_VERSION;
        consent::install(enabled);
        record_if_allowed(step, |_| captured += 1);
        consent::install(Consent::default());
        record_if_allowed(step, |_| captured += 1);
        consent::install(previous.unwrap_or_default());
        assert_eq!(
            captured, 1,
            "only the opted-in interval reaches native capture"
        );
    }

    #[test]
    fn native_step_timestamps_survive_but_arbitrary_strings_do_not() {
        for stamp in ["2026-09-11T20:25:53Z", "2026-09-11T20:25:53.190220Z"] {
            let value = serde_json::json!([{"category":"window", "message":"first_frame",
                "data":{"playing":true}, "timestamp":stamp}]);
            assert_eq!(sanitise(&value)["values"][0]["timestamp"], stamp);
        }
        for bad in [
            "private",
            "2026-09-11T20:25:53.Z",
            "2026-09-11T20:25:53.secretZ",
        ] {
            assert!(timestamp(&Value::String(bad.into())).is_none());
        }
    }

    #[test]
    fn preview_survives_the_real_allowlist() {
        assert_eq!(sanitise(&preview())["values"].as_array().unwrap().len(), 3);
    }
}
