use crate::{
    backend::Backend,
    keymanager::{self, Rpc},
    state::Flavor,
    wire,
};
use std::{
    io,
    time::{Duration, Instant},
};
use wire::{Capabilities, ErrorCode, Request, Response, PROTOCOL};
#[path = "bus.rs"]
mod bus;
#[path = "runtime.rs"]
mod runtime;

impl Rpc for bus::Bus {
    fn call(
        &mut self,
        uri: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, ErrorCode> {
        bus::Bus::call(self, uri, payload)
    }
}

pub fn run() -> Result<(), ErrorCode> {
    // A root-launched copy cannot impersonate the installer-assigned application UID.
    if unsafe { libc::getuid() } == 0 {
        return Err(ErrorCode::Authentication);
    }
    unsafe {
        libc::umask(0o077);
    }
    let executable = std::env::current_exe().map_err(|_| ErrorCode::Invalid)?;
    let app_id = runtime::app_identity(&executable)?;
    let service = format!("{app_id}.storage");
    // Acquiring this name precedes any stale-file removal, serializing conforming helpers.
    let rpc = bus::Bus::register(&service)?;
    let runtime = runtime::Runtime::publish(app_id)?;
    let mut backend = Backend::new(
        rpc,
        if app_id.ends_with(".debug") {
            Flavor::Debug
        } else {
            Flavor::Stable
        },
        service,
    );
    let mut idle = Instant::now();
    while idle.elapsed() < Duration::from_secs(30) {
        backend.rpc.pump();
        match runtime.listener.accept() {
            Ok((mut stream, _)) => {
                idle = Instant::now();
                if runtime::authenticate(&stream).is_err() {
                    continue;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .map_err(|_| ErrorCode::Unavailable)?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(8)))
                    .map_err(|_| ErrorCode::Unavailable)?;
                let hello = wire::read_frame::<Request>(&mut stream);
                if !matches!(hello,Ok(Request::Hello {protocol,nonce}) if protocol==PROTOCOL && nonce==runtime.descriptor.nonce)
                {
                    let _ = wire::write_frame(
                        &mut stream,
                        &Response::Error {
                            code: ErrorCode::Protocol,
                        },
                    );
                    continue;
                }
                // Setup and capability probing are bounded outgoing requests; neither advertises
                // a keymanager success on old firmware nor silently selects weaker protection.
                let db8 = backend.setup().is_ok();
                let keymanager = keymanager::available(&mut backend.rpc);
                let response = Response::Hello {
                    protocol: PROTOCOL,
                    nonce: runtime.descriptor.nonce.clone(),
                    helper_generation: runtime.descriptor.helper_generation.clone(),
                    capabilities: Capabilities { keymanager, db8 },
                };
                if wire::write_frame(&mut stream, &response).is_err() {
                    continue;
                }
                let reply = match wire::read_frame::<Request>(&mut stream) {
                    Ok(request) if db8 => backend.dispatch(request),
                    Ok(_) => Response::Error {
                        code: ErrorCode::Unavailable,
                    },
                    Err(_) => Response::Error {
                        code: ErrorCode::Protocol,
                    },
                };
                if matches!(reply, Response::Error { .. }) {
                    runtime.record_failure(crate::backend::last_error_stage());
                }
                let _ = wire::write_frame(&mut stream, &reply);
                idle = Instant::now();
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(_) => return Err(ErrorCode::Unavailable),
        }
    }
    Ok(())
}
