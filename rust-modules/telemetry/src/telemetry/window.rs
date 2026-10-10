//! Sparse window-lifecycle breadcrumbs attached to native crash events. No standalone usage
//! events and no per-frame trace: only lifecycle edges, WM queries, the native teardown and
//! exported-window steps a background/foreground cycle runs, and the first resumed frame.
//! Collection uses the existing crash-report consent. Values are closed labels and booleans,
//! plus SDL's three version bytes; no pointers, content, account data or runtime strings.

use serde_json::{Map, Value};

pub const LIMIT: usize = 16;

#[derive(Clone, Copy)]
pub enum Stage {
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
    WaylandReleased,
    NativeUnload,
    AcbPlaneUnload,
    NativeDestroyed,
    NativeQuarantined,
    ExportedWindowCreate,
    ExportedWindowDestroy,
    BackgroundDone,
    ForegroundDone,
}

impl Stage {
    const ALL: [Self; 20] = [
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
        Self::WaylandReleased,
        Self::NativeUnload,
        Self::AcbPlaneUnload,
        Self::NativeDestroyed,
        Self::NativeQuarantined,
        Self::ExportedWindowCreate,
        Self::ExportedWindowDestroy,
        Self::BackgroundDone,
        Self::ForegroundDone,
    ];

    pub fn code(self) -> &'static str {
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
            Self::WaylandReleased => "wayland_released",
            Self::NativeUnload => "native_unload",
            Self::AcbPlaneUnload => "acb_plane_unload",
            Self::NativeDestroyed => "native_destroyed",
            Self::NativeQuarantined => "native_quarantined",
            Self::ExportedWindowCreate => "exported_window_create",
            Self::ExportedWindowDestroy => "exported_window_destroy",
            Self::BackgroundDone => "background_done",
            Self::ForegroundDone => "foreground_done",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Observation {
    pub stage: Stage,
    pub playing: Option<bool>,
    pub version: Option<[u8; 3]>,
    pub display: Option<bool>,
    pub surface: Option<bool>,
}

impl Observation {
    pub fn step(stage: Stage, playing: Option<bool>) -> Self {
        Self {
            stage,
            playing,
            version: None,
            display: None,
            surface: None,
        }
    }
}

pub fn record(observation: Observation) {
    record_if_allowed(observation, |observation| {
        #[cfg(any(test, feature = "test-support"))]
        RECORDED.with(|r| r.borrow_mut().push(observation.stage.code()));
        super::native::record_window(observation);
    });
}

// Host-observable capture of what `record` let through: pushed AFTER the consent check, so a test
// sees exactly the labels the native backend would be handed. The same thread-local discipline as
// the other test-support captures; production builds carry none of it.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static RECORDED: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain the labels `record` accepted on this thread since the last call.
#[cfg(any(test, feature = "test-support"))]
pub fn take_recorded_for_test() -> Vec<&'static str> {
    RECORDED.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

fn record_if_allowed(observation: Observation, emit: impl FnOnce(Observation)) {
    if super::consent::allows_errors() {
        emit(observation);
    }
}

pub fn lifecycle(event: u32, playing: bool) {
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
        {"category":"window", "message":"native_unload", "data":{}},
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
        let _guard = plx_base::testlock::serial();
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

    /// S3a / h1 -- boot order, as far as a host can see it. `app::pre_boot_diagnostics` runs
    /// `telemetry::boot` (consent published, THEN `native::sync` arms the backend and sets
    /// `ACTIVE`) before `boot()` creates the window, so the first `window::record` (`wm_query`,
    /// system.rs `sys_grab_wayland`) must find consent already published. This drives the real
    /// `activate_initial` and checks the gate in front of the backend opens by the time it returns,
    /// and that a stage recorded BEFORE it is dropped, not queued: the first-run corner where the
    /// question is still unanswered at boot (so boot stages are lost if the owner opts in later)
    /// is policy ("a television whose owner said no is not traced"), pinned here as such.
    /// What it cannot see: `ACTIVE` is ARM-only, so the arming order itself stays a device fact.
    #[test]
    fn boot_publishes_consent_before_the_first_window_stage_is_recorded() {
        let _guard = plx_base::testlock::serial();
        use crate::telemetry::consent::{self, Consent};
        let previous = consent::current();
        consent::install(Consent::default());
        let emitted = std::cell::RefCell::new(Vec::new());
        let note = |o: Observation| emitted.borrow_mut().push(o.stage.code());
        record_if_allowed(Observation::step(Stage::WmQuery, None), note);
        assert!(emitted.borrow().is_empty(), "nothing is recorded before the decision is published");

        let mut stored = Consent::default();
        stored.errors = true;
        stored.asked_version = consent::POLICY_VERSION;
        let boot = crate::telemetry::activate_initial(stored);
        record_if_allowed(Observation::step(Stage::WmQuery, None), note);
        record_if_allowed(Observation::step(Stage::FirstFrame, Some(false)), note);
        drop(boot);
        consent::install(previous.unwrap_or_default());
        assert_eq!(
            *emitted.borrow(),
            ["wm_query", "first_frame"],
            "boot-time stages are recorded once boot has published the stored decision"
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

    /// S12a: the closed label set IS the wire vocabulary. Every `Stage` must come out of the real
    /// sanitiser as itself, so a label added to the enum but not to `ALL` (or the reverse) cannot
    /// silently vanish from a crash report, and the count is pinned so the set grows on purpose.
    #[test]
    fn every_stage_round_trips_the_sanitiser() {
        assert_eq!(Stage::ALL.len(), 20, "the closed label set changed: update PRIVACY.md too");
        let mut seen = std::collections::HashSet::new();
        for stage in Stage::ALL {
            assert!(seen.insert(stage.code()), "duplicate wire label {}", stage.code());
            let crumb = serde_json::json!({"timestamp":"2026-10-01T10:00:00.123456Z", "type":"state",
                "category":"window", "level":"info", "message":stage.code(), "data":{}});
            let out = sanitise(&Value::Array(vec![crumb]));
            let values = out["values"].as_array().unwrap();
            assert_eq!(values.len(), 1, "{} was dropped by the sanitiser", stage.code());
            assert_eq!(values[0]["message"], stage.code());
        }
    }

    /// The longest background + foreground cycle with playback must fit the 16-slot ring whole
    /// (PRIVACY.md says "last 16"), so its first step is still there when the cycle's last crashes.
    /// The webOS 5 exported-window path is the longer one (the ACB path swaps the two window steps
    /// for `acb_plane_unload`): 8 pre-existing steps plus 7 new ones.
    #[test]
    fn the_longest_background_cycle_fits_the_ring() {
        let cycle = [
            Stage::WillBackground, Stage::WaylandReleased, Stage::DidBackground,
            Stage::NativeUnload, Stage::NativeDestroyed, Stage::ExportedWindowDestroy,
            Stage::BackgroundDone, Stage::WillForeground, Stage::DidForeground, Stage::WmQuery,
            Stage::WmReady, Stage::ExportedWindowCreate, Stage::FirstFrame,
            Stage::FirstSwapComplete, Stage::ForegroundDone,
        ];
        assert!(cycle.len() <= LIMIT, "{} steps would evict the start of the cycle", cycle.len());
        assert!(cycle.iter().all(|s| Stage::ALL.iter().any(|a| a.code() == s.code())));
    }

    /// The test-support capture sits AFTER the consent check, so it shows exactly what the native
    /// backend would be handed: nothing without consent, the label with it, nothing again after
    /// withdrawal.
    #[test]
    fn the_capture_follows_consent() {
        let _guard = plx_base::testlock::serial();
        use crate::telemetry::consent::{self, Consent};
        let previous = consent::current();
        take_recorded_for_test();
        consent::install(Consent::default());
        record(Observation::step(Stage::NativeUnload, None));
        let mut enabled = Consent::default();
        enabled.errors = true;
        enabled.asked_version = consent::POLICY_VERSION;
        consent::install(enabled);
        record(Observation::step(Stage::NativeUnload, None));
        consent::install(Consent::default());
        record(Observation::step(Stage::BackgroundDone, Some(true)));
        consent::install(previous.unwrap_or_default());
        assert_eq!(take_recorded_for_test(), ["native_unload"]);
    }

    #[test]
    fn preview_survives_the_real_allowlist() {
        assert_eq!(sanitise(&preview())["values"].as_array().unwrap().len(), 4);
    }
}
