//! One independently published rendezvous per installed service identity.
use super::wire::{Descriptor, ErrorCode, DESCRIPTOR_NAME, PROTOCOL, SOCKET_NAME};
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
    path::{Path, PathBuf},
};

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
    match dir.file_name().and_then(|n| n.to_str()) {
        Some("com.beb.plxnative.storage") => Ok("com.beb.plxnative"),
        Some("com.beb.plxnative.debug.storage") => Ok("com.beb.plxnative.debug"),
        _ => Err(ErrorCode::Invalid),
    }
}
pub fn random_hex() -> Result<String, ErrorCode> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| ErrorCode::Unavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
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

pub struct Runtime {
    pub listener: UnixListener,
    pub descriptor: Descriptor,
    directory: File,
    path: PathBuf,
    socket_inode: (u64, u64),
    descriptor_inode: (u64, u64),
}
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

    /// Publish one payload-free failure stage for rooted diagnostics. It is transient, private to
    /// the app UID, and never participates in protocol decisions.
    pub fn record_failure(&self, stage: &str) {
        const NAME: &str = "last-error";
        if stage.len() > 64 || !stage.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return;
        }
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
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
            let _ = file.write_all(stage.as_bytes());
        }
    }
}
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
impl Drop for Runtime {
    fn drop(&mut self) {
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()));
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
        // Leave the empty private directory: its ownership is the next activation's guard.
        let _ = &self.path;
    }
}
