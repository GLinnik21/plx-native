//! One independently published rendezvous per installed service identity.
use super::wire::ErrorCode;
#[cfg(all(target_os = "linux", target_arch = "arm"))]
use super::wire::{Descriptor, DESCRIPTOR_NAME, PROTOCOL, SOCKET_NAME};
use std::path::Path;

pub fn app_identity(executable: &Path) -> Result<&str, ErrorCode> {
    if executable.file_name().and_then(|n| n.to_str()) != Some("plxnative-storage") {
        return Err(ErrorCode::Invalid);
    }
    let dir = executable.parent().ok_or(ErrorCode::Invalid)?;
    if dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        != Some("services")
    {
        return Err(ErrorCode::Invalid);
    }
    let app_id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| name.strip_suffix(".storage"))
        .ok_or(ErrorCode::Invalid)?;
    super::state::Flavor::from_app_id(app_id).ok_or(ErrorCode::Invalid)?;
    Ok(app_id)
}
#[cfg(all(target_os = "linux", target_arch = "arm"))]
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
};
#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub fn random_hex() -> Result<String, ErrorCode> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| ErrorCode::Unavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn safe_metadata(path: &Path, socket: bool) -> Result<fs::Metadata, ErrorCode> {
    let m = fs::symlink_metadata(path).map_err(|_| ErrorCode::Unavailable)?;
    if m.uid() != unsafe { libc::getuid() }
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || if socket {
            !m.file_type().is_socket()
        } else {
            !m.is_file()
        }
    {
        return Err(ErrorCode::Authentication);
    }
    Ok(m)
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub struct Runtime {
    pub listener: UnixListener,
    pub descriptor: Descriptor,
    directory: File,
    path: PathBuf,
    socket_inode: (u64, u64),
    descriptor_inode: (u64, u64),
}
#[cfg(all(target_os = "linux", target_arch = "arm"))]
impl Runtime {
    pub fn publish(app_id: &str) -> Result<Self, ErrorCode> {
        let path = PathBuf::from(format!("/tmp/{app_id}.storage-runtime"));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err(ErrorCode::Unavailable),
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|_| ErrorCode::Authentication)?;
        let metadata = directory.metadata().map_err(|_| ErrorCode::Unavailable)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::getuid() }
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(ErrorCode::Authentication);
        }
        // All operations are anchored to the checked directory descriptor. Even if the path is
        // replaced, they cannot escape into a substituted directory.
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        for (name, socket) in [(SOCKET_NAME, true), (DESCRIPTOR_NAME, false)] {
            let stale = anchored.join(name);
            match fs::symlink_metadata(&stale) {
                Ok(_) => {
                    safe_metadata(&stale, socket)?;
                    fs::remove_file(stale).map_err(|_| ErrorCode::Unavailable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err(ErrorCode::Unavailable),
            }
        }
        let listener =
            UnixListener::bind(anchored.join(SOCKET_NAME)).map_err(|_| ErrorCode::Unavailable)?;
        fs::set_permissions(
            anchored.join(SOCKET_NAME),
            fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| ErrorCode::Unavailable)?;
        let sm = safe_metadata(&anchored.join(SOCKET_NAME), true)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| ErrorCode::Unavailable)?;
        let descriptor = Descriptor {
            protocol: PROTOCOL,
            nonce: random_hex()?,
            helper_generation: random_hex()?,
            socket: SOCKET_NAME.into(),
            pid: std::process::id(),
            uid: unsafe { libc::getuid() },
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(anchored.join(DESCRIPTOR_NAME))
            .map_err(|_| ErrorCode::Unavailable)?;
        file.write_all(&serde_json::to_vec(&descriptor).map_err(|_| ErrorCode::Invalid)?)
            .map_err(|_| ErrorCode::Unavailable)?;
        let dm = safe_metadata(&anchored.join(DESCRIPTOR_NAME), false)?;
        // Publication path must still name the checked inode before advertising readiness.
        let current = fs::symlink_metadata(&path).map_err(|_| ErrorCode::Unavailable)?;
        if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(ErrorCode::Authentication);
        }
        Ok(Self {
            listener,
            descriptor,
            directory,
            path,
            socket_inode: (sm.dev(), sm.ino()),
            descriptor_inode: (dm.dev(), dm.ino()),
        })
    }

    pub fn clear_failure(&self) {
        let path = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd())).join("last-error");
        if safe_metadata(&path, false).is_ok() { let _ = fs::remove_file(path); }
    }

    /// Publish one payload-free failure stage for app diagnostics. It is transient, private to
    /// the app UID, and never participates in protocol decisions.
    pub fn record_failure(&self, stage: &str) {
        record_failure_in(&self.directory, &self.descriptor.helper_generation, stage);
    }
}

// Process-start snapshot: never read a later activation's nonce on a failure path.
#[cfg(all(target_os = "linux", target_arch = "arm"))]
static START_ATTEMPT: std::sync::OnceLock<Option<super::wire::activation::Attempt>> = std::sync::OnceLock::new();

#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub fn capture_start_attempt(app_id: &str) {
    START_ATTEMPT.get_or_init(|| super::wire::activation::Attempt::capture(
        Path::new(&format!("/tmp/{app_id}.storage-runtime"))));
}

/// Also used by BusCancel immediately before exit, when destructors cannot publish a failure.
#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub fn record_start_failure(app_id: &str) {
    if let (Some(Some(attempt)), Some(failure)) = (START_ATTEMPT.get(), super::wire::failure::last()) {
        attempt.record_failure(Path::new(&format!("/tmp/{app_id}.storage-runtime")), failure.observed);
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
fn record_failure_in(directory: &File, generation: &str, stage: &str) {
    const NAME: &str = "last-error";
    let detail = super::wire::failure::last().map(|f| f.observed)
        .or_else(|| super::wire::failure::parse_last_error(stage.as_bytes()));
    let Some(detail) = detail else { return; };
    let Ok(bytes) = serde_json::to_vec(&super::wire::failure::Record { helper_generation: generation.into(), detail }) else { return; };
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let path = anchored.join(NAME);
    match fs::symlink_metadata(&path) {
        Ok(_) if safe_metadata(&path, false).is_err() => return,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return,
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(&bytes);
    }
}

#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub fn authenticate(stream: &UnixStream) -> Result<(), ErrorCode> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0
        || len as usize != std::mem::size_of::<libc::ucred>()
        || cred.uid != unsafe { libc::getuid() }
        || cred.pid <= 0
    {
        return Err(ErrorCode::Authentication);
    }
    Ok(())
}
#[cfg(all(target_os = "linux", target_arch = "arm"))]
impl Drop for Runtime {
    fn drop(&mut self) {
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
        // Do not remove a successor's diagnostic if the rendezvous has been replaced.
        if safe_metadata(&anchored.join(DESCRIPTOR_NAME), false)
            .is_ok_and(|m| (m.dev(), m.ino()) == self.descriptor_inode) {
            self.clear_failure();
        }
        for (name, socket, identity) in [
            (SOCKET_NAME, true, self.socket_inode),
            (DESCRIPTOR_NAME, false, self.descriptor_inode),
        ] {
            let path = anchored.join(name);
            if let Ok(m) = safe_metadata(&path, socket) {
                if (m.dev(), m.ino()) == identity {
                    let _ = fs::remove_file(path);
                }
            }
        }
        // Keep the private directory and activation diagnostics; its ownership guards the next start.
        let _ = &self.path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nightly_app_identity() {
        assert_eq!(app_identity(Path::new("/media/developer/apps/usr/palm/services/com.beb.plxnative.nightly.storage/plxnative-storage")), Ok("com.beb.plxnative.nightly"));
    }

    #[test]
    fn rejects_foreign_or_malformed_install_paths() {
        for path in [
            "/usr/palm/services/com.beb.plxnative.typo.storage/plxnative-storage",
            "/usr/palm/services/com.beb.plxnative.nightly/plxnative-storage",
            "/usr/palm/applications/com.beb.plxnative.nightly.storage/plxnative-storage",
            "/usr/palm/services/com.beb.plxnative.nightly.storage/other",
            "plxnative-storage",
        ] {
            assert_eq!(app_identity(Path::new(path)), Err(ErrorCode::Invalid));
        }
    }

    #[test]
    fn packaged_app_identity() {
        let Ok(paths) = std::env::var("PLX_TEST_PACKAGED_HELPERS") else {
            return;
        };
        for path in paths.lines() {
            assert!(
                app_identity(Path::new(path)).is_ok(),
                "rejected packaged helper: {path}"
            );
        }
    }
}
