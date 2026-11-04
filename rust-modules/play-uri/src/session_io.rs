//! Shared URI transport and exported-window actuator. Both the persistent backend and the
//! diagnostic script feed answers into the same ownership reducer; this module owns no state.
use crate::uri_lifecycle::{Event, Fx};
use crate::uri_protocol::{build_load, Message};
use plx_base::task::MainThread;
use plx_platform::tv::sink::DisplaySink;
use plx_platform::tv::uri::UriMedia;
use std::ffi::CStr;
use std::os::raw::c_int;

/// Execute one transport effect and return its addressed immediate answer. Other effects are
/// delivered to the caller, which owns the attempt and decides what they mean.
pub(crate) fn transport_answer(
    effect: &Fx, transport: &dyn UriMedia, url: &str, app_id: &str,
) -> Option<Event> {
    match effect {
        Fx::Open { request, shape } => {
            let result = transport.open(*shape).map_err(|text| Message::transport_failure(&text));
            Some(Event::OpenAnswered { request: *request, result })
        }
        Fx::Call { session, request, call } => {
            let payload = match call {
                crate::uri_protocol::UriCall::Load { window_id, bps } =>
                    build_load(url, app_id, window_id.as_deref(), *bps),
                other => other.payload().unwrap_or_default(),
            };
            let result = transport.call(*session, *request, call.method(), &payload);
            let message = result.as_ref().err()
                .map(|text| Message::transport_failure(text)).unwrap_or_default();
            Some(Event::CallQueued { session: *session, request: *request,
                ok: result.is_ok(), message })
        }
        Fx::Close { session } => Some(Event::CloseQueued {
            session: *session, ok: transport.close(*session).is_ok(),
        }),
        _ => None,
    }
}

/// Copy the native id before another window call can rewrite its backing buffer.
pub(crate) fn create_exported_window(sink: &dyn DisplaySink, mt: &MainThread) -> Option<String> {
    let id = unsafe { sink.create_window(mt) };
    (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned())
}

pub(crate) fn place_exported_window(
    sink: &dyn DisplaySink, mt: &MainThread, source: (c_int, c_int),
    canvas: (c_int, c_int),
) -> c_int {
    unsafe { sink.place_window(mt, source.0, source.1, 0, 0, canvas.0, canvas.1) }
}

pub(crate) fn destroy_exported_window(sink: &dyn DisplaySink, mt: &MainThread) {
    unsafe { sink.destroy_window(mt) }
}
