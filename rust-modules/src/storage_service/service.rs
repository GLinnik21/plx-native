use crate::{
    backend::Backend,
    keymanager::{self, Rpc},
    runtime,
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
    let flavor = Flavor::from_app_id(app_id).ok_or(ErrorCode::Invalid)?;
    let service = format!("{app_id}.storage");
    // Acquiring this name precedes any stale-file removal, serializing conforming helpers.
    let rpc = bus::Bus::register(&service)?;
    let runtime = runtime::Runtime::publish(app_id)?;
    let mut backend = Backend::new(rpc, flavor, service);
    let mut idle = Instant::now();
    while idle.elapsed() < Duration::from_secs(30) {
        backend.rpc.pump();
        match runtime.listener.accept() {
            Ok((mut stream, _)) => {
                idle = Instant::now();
                if runtime::authenticate(&stream).is_err() {
                    continue;
                }
                runtime.clear_failure();
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
                wire::failure::clear();
                let db8 = backend.setup().is_ok();
                if !db8 { runtime.record_failure("none"); }
                // A capability probe must not overwrite the DB8 failure being diagnosed.
                let setup_failure = wire::failure::last();
                let keymanager = keymanager::available(&mut backend.rpc);
                if let Some(failure) = setup_failure {
                    wire::failure::remember(failure.observed.stage, failure.observed.code);
                } else { wire::failure::clear(); }
                let response = Response::Hello {
                    protocol: PROTOCOL,
                    nonce: runtime.descriptor.nonce.clone(),
                    helper_generation: runtime.descriptor.helper_generation.clone(),
                    capabilities: Capabilities { keymanager, db8 },
                };
                if wire::write_frame(&mut stream, &response).is_err() {
                    continue;
                }
                if db8 { wire::failure::clear(); }
                let reply = match wire::read_frame::<Request>(&mut stream) {
                    Ok(request) if db8 => backend.dispatch(request),
                    Ok(_) => Response::Error {
                        code: ErrorCode::Unavailable,
                    },
                    Err(_) => Response::Error {
                        code: ErrorCode::Protocol,
                    },
                };
                if reply.failure_code().is_some() {
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
