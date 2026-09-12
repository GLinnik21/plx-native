#[cfg(all(target_os = "linux", target_arch = "arm"))]
#[path = "../storage_service/auxv.rs"]
mod auxv;
#[path = "../storage_service/backend.rs"]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod backend;
#[path = "../storage_service/keymanager.rs"]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod keymanager;
#[path = "../storage/state.rs"]
#[allow(dead_code)] // Shared engine also exposes adapter APIs unused by this executable.
mod state;
#[path = "../storage_service/wire.rs"]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod wire;

#[cfg(all(target_os = "linux", target_arch = "arm"))]
#[path = "../storage_service/service.rs"]
mod service;

fn main() {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    if service::run().is_err() {
        std::process::exit(1);
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
    {
        eprintln!("plxnative-storage requires the webOS Linux service runtime");
        std::process::exit(1);
    }
}
