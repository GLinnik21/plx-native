//! Application side of the private storage-helper protocol.
//!
//! A missing helper, a stale rendezvous, a refused activation and a corrupt canonical object are
//! all unavailable storage — never an empty database.  Only the helper may decide that DB8 has no
//! canonical object, after an authenticated `Hello` on the same stream as the `Load` reply.

use super::{
    state::{self, CanonicalState, Expected, Flavor, Generation},
    wire::{
        self, AuthLoad, Descriptor, ErrorCode, Expectation, ProtectionOutcome, ReconcileStatus,
        Request, Response, WireMutation,
    },
};
use std::{
    fs::OpenOptions,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const START_DEADLINE: Duration = Duration::from_secs(12);
const IO_DEADLINE: Duration = Duration::from_secs(8);
const DESCRIPTOR_MAX: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientError {
    Unavailable,
    Authentication,
    Protocol,
    Corrupt,
    Invalid,
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) db_rev: String,
    pub(crate) state: CanonicalState,
    pub(crate) auth: AuthLoad,
    pub(crate) protection: Option<ProtectionOutcome>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StorageSnapshot { <redacted> }")
    }
}

pub(crate) enum Load {
    Missing,
    Present(Snapshot),
}

impl std::fmt::Debug for Load {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("Missing"),
            Self::Present(_) => f.write_str("Present(<redacted>)"),
        }
    }
}

fn flavor() -> Flavor {
    if crate::paths::flavour() == Some("debug") {
        Flavor::Debug
    } else {
        Flavor::Stable
    }
}

fn service_name() -> String {
    format!("{}.storage", crate::paths::app_id())
}

fn runtime_path() -> PathBuf {
    PathBuf::from(format!("/tmp/{}.storage-runtime", crate::paths::app_id()))
}

fn generation_text(generation: Generation) -> String {
    generation
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn wire_expectation(expected: Option<(&str, Expected)>) -> Expectation {
    match expected {
        None => Expectation::Missing {},
        Some((db_rev, expected)) => Expectation::Present {
            db_rev: db_rev.to_string(),
            epoch: expected.epoch.to_string(),
            auth_generation: generation_text(expected.auth_generation),
        },
    }
}

fn error(code: ErrorCode) -> ClientError {
    match code {
        ErrorCode::Authentication => ClientError::Authentication,
        ErrorCode::Protocol => ClientError::Protocol,
        ErrorCode::Corrupt => ClientError::Corrupt,
        ErrorCode::Invalid => ClientError::Invalid,
        ErrorCode::Unavailable | ErrorCode::Timeout | ErrorCode::Capability => {
            ClientError::Unavailable
        }
    }
}

/// Load through an authenticated helper. `Ok(Missing)` is therefore authoritative.
pub(crate) fn load() -> Result<Load, ClientError> {
    match transact(Request::Load {})? {
        Response::Missing => Ok(Load::Missing),
        Response::Loaded {
            db_rev,
            state: value,
            auth,
            protection,
        } => {
            let bytes = serde_json::to_vec(&value).map_err(|_| ClientError::Corrupt)?;
            let state =
                CanonicalState::decode(&bytes, flavor()).map_err(|_| ClientError::Corrupt)?;
            if db_rev.parse::<u64>().ok().map(|value| value.to_string()) != Some(db_rev.clone()) {
                return Err(ClientError::Corrupt);
            }
            Ok(Load::Present(Snapshot {
                db_rev,
                state,
                auth,
                protection,
            }))
        }
        Response::Error { code } => Err(error(code)),
        _ => Err(ClientError::Protocol),
    }
}

/// Submit one typed mutation. The digest covers the plaintext request, before the helper seals it.
pub(crate) fn commit(
    expected: Option<(&str, Expected)>,
    operation_id: Generation,
    mutation: WireMutation,
) -> Result<Response, ClientError> {
    let mut request = Request::Commit {
        expected: wire_expectation(expected),
        operation_id: generation_text(operation_id),
        digest: String::new(),
        mutation,
    };
    let digest = state::digest_bytes(&wire::request_digest_bytes(&request).map_err(error)?);
    let Request::Commit {
        digest: request_digest,
        ..
    } = &mut request
    else {
        unreachable!()
    };
    *request_digest = digest.clone();
    match transact(request.clone()) {
        Ok(response) => Ok(response),
        Err(first @ (ClientError::Unavailable | ClientError::Protocol)) => {
            match reconcile(operation_id, digest)? {
                response @ Response::Reconcile {
                    status: ReconcileStatus::Applied,
                    ..
                } => Ok(response),
                Response::Reconcile {
                    status: ReconcileStatus::NotApplied,
                    ..
                } => transact(request),
                response @ Response::Reconcile {
                    status: ReconcileStatus::Unknown,
                    ..
                } => Ok(response),
                Response::Reconcile {
                    status: ReconcileStatus::Invalid,
                    ..
                } => Err(ClientError::Invalid),
                Response::Error { code } => Err(error(code)),
                response @ Response::KeymanagerError { .. } => Ok(response),
                _ => Err(first),
            }
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn reconcile(operation_id: Generation, digest: String) -> Result<Response, ClientError> {
    transact(Request::Reconcile {
        operation_id: generation_text(operation_id),
        digest,
    })
}

fn transact(command: Request) -> Result<Response, ClientError> {
    let until = Instant::now() + START_DEADLINE;
    let mut hinted = false;
    loop {
        if let Ok((mut stream, descriptor)) = connect(&runtime_path()) {
            stream
                .set_read_timeout(Some(IO_DEADLINE))
                .map_err(|_| ClientError::Unavailable)?;
            stream
                .set_write_timeout(Some(IO_DEADLINE))
                .map_err(|_| ClientError::Unavailable)?;
            wire::write_frame(
                &mut stream,
                &Request::Hello {
                    protocol: wire::PROTOCOL,
                    nonce: descriptor.nonce.clone(),
                },
            )
            .map_err(|_| ClientError::Protocol)?;
            match wire::read_frame::<Response>(&mut stream).map_err(|_| ClientError::Protocol)? {
                Response::Hello {
                    protocol,
                    nonce,
                    helper_generation,
                    capabilities,
                } if protocol == wire::PROTOCOL
                    && nonce == descriptor.nonce
                    && helper_generation == descriptor.helper_generation
                    && capabilities.db8 => {}
                Response::Error { code } => return Err(error(code)),
                _ => return Err(ClientError::Protocol),
            }
            wire::write_frame(&mut stream, &command).map_err(|_| ClientError::Protocol)?;
            return wire::read_frame(&mut stream).map_err(|_| ClientError::Protocol);
        }
        if !hinted {
            crate::webos::activate_storage_helper(&service_name());
            hinted = true;
        }
        if Instant::now() >= until {
            return Err(ClientError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn connect(root: &Path) -> Result<(UnixStream, Descriptor), ClientError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|_| ClientError::Unavailable)?;
    let directory_meta = directory.metadata().map_err(|_| ClientError::Unavailable)?;
    if !directory_meta.is_dir()
        || directory_meta.uid() != unsafe { libc::getuid() }
        || directory_meta.mode() & 0o7777 != 0o700
    {
        return Err(ClientError::Authentication);
    }
    let anchor = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let descriptor = read_descriptor(&anchor.join(wire::DESCRIPTOR_NAME))?;
    if descriptor.protocol != wire::PROTOCOL
        || descriptor.socket != wire::SOCKET_NAME
        || descriptor.uid != unsafe { libc::getuid() }
        || descriptor.pid == 0
        || !hex_128(&descriptor.nonce)
        || !hex_128(&descriptor.helper_generation)
    {
        return Err(ClientError::Protocol);
    }
    let socket = anchor.join(wire::SOCKET_NAME);
    let socket_meta = std::fs::symlink_metadata(&socket).map_err(|_| ClientError::Unavailable)?;
    if !socket_meta.file_type().is_socket()
        || socket_meta.uid() != unsafe { libc::getuid() }
        || socket_meta.mode() & 0o7777 != 0o600
    {
        return Err(ClientError::Authentication);
    }
    let stream = UnixStream::connect(socket).map_err(|_| ClientError::Unavailable)?;
    authenticate(&stream)?;
    let named_meta = std::fs::symlink_metadata(root).map_err(|_| ClientError::Unavailable)?;
    if (named_meta.dev(), named_meta.ino()) != (directory_meta.dev(), directory_meta.ino()) {
        return Err(ClientError::Authentication);
    }
    Ok((stream, descriptor))
}

fn read_descriptor(path: &Path) -> Result<Descriptor, ClientError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ClientError::Unavailable)?;
    let meta = file.metadata().map_err(|_| ClientError::Unavailable)?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::getuid() }
        || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != 1
        || meta.len() > DESCRIPTOR_MAX
    {
        return Err(ClientError::Authentication);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(DESCRIPTOR_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ClientError::Unavailable)?;
    if bytes.len() as u64 > DESCRIPTOR_MAX {
        return Err(ClientError::Protocol);
    }
    serde_json::from_slice(&bytes).map_err(|_| ClientError::Protocol)
}

fn authenticate(stream: &UnixStream) -> Result<(), ClientError> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut _ as *mut libc::c_void,
            &mut length,
        )
    };
    if result != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || credentials.uid != unsafe { libc::getuid() }
        || credentials.pid <= 0
    {
        return Err(ClientError::Authentication);
    }
    Ok(())
}

fn hex_128(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_and_expectation_never_cross_a_json_float() {
        let generation = Generation([0xff; 16]);
        let Expectation::Present {
            db_rev,
            epoch,
            auth_generation,
        } = wire_expectation(Some((
            &u64::MAX.to_string(),
            Expected {
                epoch: u64::MAX,
                auth_generation: generation,
            },
        )))
        else {
            panic!("present expectation")
        };
        assert_eq!(db_rev, u64::MAX.to_string());
        assert_eq!(epoch, u64::MAX.to_string());
        assert_eq!(auth_generation, "ff".repeat(16));
    }

    #[test]
    fn closed_error_mapping_never_exposes_service_text() {
        assert_eq!(error(ErrorCode::Capability), ClientError::Unavailable);
        assert_eq!(
            error(ErrorCode::Authentication),
            ClientError::Authentication
        );
    }
}
