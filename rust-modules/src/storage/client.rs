//! Application side of the private storage-helper protocol.
//!
//! A missing helper, a stale rendezvous, a refused activation and a corrupt canonical object are
//! all unavailable storage — never an empty database.  Only the helper may decide that DB8 has no
//! canonical object, after an authenticated `Hello` on the same stream as the `Load` reply.
//! A helper that never connects gets one startup window. After that expires, each call tries
//! once and returns unavailable immediately on failure, with activation hints rate-limited.
//! A successful connection restores the normal startup policy.

use super::{
    state::{self, CanonicalState, Expected, Flavor, Generation},
    wire::{
        self, AuthLoad, Descriptor, ErrorCode, Expectation, ProtectionOutcome, ReconcileStatus,
        Request, Response, WireMutation,
    },
};
#[allow(unused_imports)]
// Unix transport is Linux-only; protocol coordinator is host-testable.
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

use wire::failure::{self, Stage};

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

pub(crate) fn flavor() -> Result<Flavor, ClientError> {
    Flavor::from_app_id(crate::paths::app_id()).ok_or(ClientError::Invalid)
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

fn failed(stage: Stage, errno: Option<i32>, error: ClientError) -> ClientError {
    let activation = failure::last().and_then(|f| f.activation);
    failure::remember(stage, errno);
    failure::update(|f| f.activation = activation);
    error
}

fn io_failed(stage: Stage, error: std::io::Error) -> ClientError {
    failed(stage, error.raw_os_error(), ClientError::Unavailable)
}

fn error(code: ErrorCode) -> ClientError {
    if !failure::last().is_some_and(|f| matches!(f.observed.stage, Stage::Wire | Stage::HelloRejected)) {
        failed(Stage::Wire, None, classify_error(code));
        failure::update(|f| f.wire = Some(code));
    }
    classify_error(code)
}

fn classify_error(code: ErrorCode) -> ClientError {
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
    load_with(&mut NativeTransport)
}

/// Injectable external I/O boundary, shared by the shipping coordinator and host fault tests.
pub(crate) trait Transport {
    fn transact(&mut self, request: Request) -> Result<Response, ClientError>;
}
impl<F: FnMut(Request) -> Result<Response, ClientError>> Transport for F {
    fn transact(&mut self, request: Request) -> Result<Response, ClientError> {
        self(request)
    }
}
pub(crate) struct NativeTransport;
impl Transport for NativeTransport {
    fn transact(&mut self, request: Request) -> Result<Response, ClientError> {
        transact(request)
    }
}

pub(crate) fn load_with(transport: &mut dyn Transport) -> Result<Load, ClientError> {
    match transport.transact(Request::Load {})? {
        Response::Missing => Ok(Load::Missing),
        Response::Loaded {
            db_rev,
            state: value,
            auth,
            protection,
        } => {
            let bytes = serde_json::to_vec(&value).map_err(|_| ClientError::Corrupt)?;
            let state =
                CanonicalState::decode(&bytes, flavor()?).map_err(|_| ClientError::Corrupt)?;
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
    commit_with(&mut NativeTransport, expected, operation_id, mutation)
}

pub(crate) fn commit_with(
    transport: &mut dyn Transport,
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
    match transport.transact(request.clone()) {
        Ok(response) => Ok(response),
        Err(first @ (ClientError::Unavailable | ClientError::Protocol)) => {
            match transport.transact(Request::Reconcile {
                operation_id: generation_text(operation_id),
                digest,
            })? {
                response @ Response::Reconcile {
                    status: ReconcileStatus::Applied,
                    ..
                } => Ok(response),
                Response::Reconcile {
                    status: ReconcileStatus::NotApplied,
                    ..
                } => transport.transact(request),
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

/// Stage B supplies the application resource adapter's nonblocking LS2 activation hint.
/// An absent hint never becomes permission to load a legacy file.
static ACTIVATION_HINT: std::sync::OnceLock<fn(&str)> = std::sync::OnceLock::new();
pub(crate) fn install_activation_hint(hint: fn(&str)) -> Result<(), fn(&str)> {
    ACTIVATION_HINT.set(hint)
}

/// The helper is a dynamic LS2 service: nothing starts it but a call to it (0.6.6's
/// `activate_storage_helper`). Without this a television with no running helper never answers.
#[cfg(all(
    target_os = "linux",
    target_arch = "arm",
    not(feature = "hostsim"),
    not(test)
))]
fn activate(service: &str) -> failure::Detail {
    crate::webos::activate_storage_helper(service)
}

#[cfg(all(
    target_os = "linux",
    not(all(target_arch = "arm", not(feature = "hostsim"), not(test)))
))]
fn activate(_service: &str) -> failure::Detail {
    failure::Detail::new(Stage::Unsupported, None)
}

#[cfg(not(target_os = "linux"))]
fn transact(_command: Request) -> Result<Response, ClientError> {
    let _block = crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("storage helper transact") });
    Err(failed(Stage::Unsupported, None, ClientError::Unavailable))
}

/// Startup wait policy, kept separate from sockets so deadline and recovery are host-testable.
#[derive(Default)]
struct StartGate {
    failed: bool,
    last_hint: Option<Instant>,
    activation: Option<failure::Detail>,
}

impl StartGate {
    /// Called only after a connect attempt: a failed start never prevents trying a live helper.
    fn wait_after_failed_connect(&mut self, now: Instant, until: Instant) -> bool {
        self.failed |= now >= until;
        !self.failed
    }

    /// Re-hint at most once per startup window, even if several callers keep trying storage.
    fn hint_due(&mut self, now: Instant) -> bool {
        if self
            .last_hint
            .is_some_and(|last| now.saturating_duration_since(last) < START_DEADLINE)
        {
            return false;
        }
        self.last_hint = Some(now);
        true
    }

    fn connected(&mut self) {
        *self = Self::default();
    }
}

/// Shared by loads, commits and reconciliation. Never held across I/O, activation or sleep.
static START_GATE: std::sync::Mutex<StartGate> = std::sync::Mutex::new(StartGate {
    failed: false,
    last_hint: None,
    activation: None,
});

#[cfg(target_os = "linux")]
fn transact(command: Request) -> Result<Response, ClientError> {
    let _block = crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("storage helper transact") });
    failure::clear();
    let until = Instant::now() + START_DEADLINE;
    let mut hinted = false;
    loop {
        let connected = connect(&runtime_path());
        if let Ok((mut stream, descriptor)) = connected {
            let activation = {
                let mut gate = START_GATE.lock().unwrap_or_else(|e| e.into_inner());
                let activation = gate.activation;
                gate.connected();
                activation
            };
            failure::clear();
            failure::remember(Stage::HelloRejected, None);
            failure::update(|f| f.activation = activation);
            stream
                .set_read_timeout(Some(IO_DEADLINE))
                .map_err(|e| io_failed(Stage::SocketTimeout, e))?;
            stream
                .set_write_timeout(Some(IO_DEADLINE))
                .map_err(|e| io_failed(Stage::SocketTimeout, e))?;
            wire::write_frame(
                &mut stream,
                &Request::Hello {
                    protocol: wire::PROTOCOL,
                    nonce: descriptor.nonce.clone(),
                },
            )
            .map_err(|_| failed(Stage::Wire, None, ClientError::Protocol))?;
            let hello = wire::read_frame::<Response>(&mut stream)
                .map_err(|_| failed(Stage::HelloRejected, None, ClientError::Protocol))?;
            let generation = match &hello {
                Response::Hello { protocol, nonce, helper_generation, .. }
                    if *protocol == wire::PROTOCOL && *nonce == descriptor.nonce
                        && *helper_generation == descriptor.helper_generation => Some(helper_generation.as_str()),
                _ => None,
            };
            // A matching Hello can report DB8 setup failure; it still proves the generation.
            if let Err(error) = validate_hello(hello.clone(), &descriptor) {
                read_helper_failure(&runtime_path(), generation);
                return Err(error);
            }
            wire::write_frame(&mut stream, &command).map_err(|_| failed(Stage::Wire, None, ClientError::Protocol))?;
            let response = wire::read_frame::<Response>(&mut stream).map_err(|_| failed(Stage::Wire, None, ClientError::Protocol))?;
            if let Some(code) = response.failure_code() {
                failed(Stage::Wire, None, classify_error(code));
                failure::update(|f| f.wire = Some(code));
                read_helper_failure(&runtime_path(), Some(&descriptor.helper_generation));
            } else {
                failure::clear();
            }
            return Ok(response);
        }
        let (wait, hint) = {
            let mut gate = START_GATE.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            (gate.wait_after_failed_connect(now, until), !hinted && gate.hint_due(now))
        };
        if hint {
            let activate_helper = || {
                let detail = match ACTIVATION_HINT.get() {
                    Some(hint) => {
                        hint(&service_name());
                        failure::Detail::new(Stage::ActivationSent, None)
                    }
                    None => activate(&service_name()),
                };
                START_GATE.lock().unwrap_or_else(|e| e.into_inner()).activation = Some(detail);
            };
            if wait {
                activate_helper();
            } else {
                // The native fallback hint itself waits for LS2. A memoized failure must
                // return immediately even when that hint needs another platform round trip.
                let _ = crate::storage_worker::submit(activate_helper);
            }
            hinted = true;
        }
        if !wait {
            // Keep the connect/authentication/protocol observation beside the historical
            // control-flow value. Never turn a refused peer into an empty database.
            let error = start_unavailable(connected.err().unwrap());
            let activation = START_GATE.lock().unwrap_or_else(|e| e.into_inner()).activation;
            failure::update(|f| f.activation = activation);
            read_helper_failure(&runtime_path(), None);
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn validate_hello(response: Response, descriptor: &Descriptor) -> Result<(), ClientError> {
    match response {
        Response::Hello { protocol, nonce, helper_generation, capabilities }
            if protocol == wire::PROTOCOL && nonce == descriptor.nonce
                && helper_generation == descriptor.helper_generation && capabilities.db8 => Ok(()),
        Response::Error { code } => {
            let error = failed(Stage::HelloRejected, None, classify_error(code));
            failure::update(|f| f.wire = Some(code));
            Err(error)
        }
        _ => Err(failed(Stage::HelloRejected, None, ClientError::Protocol)),
    }
}

fn peer_rejection(errno: Option<i32>, valid_length: bool, uid_matches: bool, valid_pid: bool) -> Option<failure::Detail> {
    if errno.is_some() || !valid_length || !valid_pid {
        Some(failure::Detail::new(Stage::PeerCredentials, errno))
    } else if !uid_matches {
        Some(failure::Detail::new(Stage::PeerUidMismatch, None))
    } else { None }
}

fn start_unavailable(error: ClientError) -> ClientError {
    if failure::last().is_none() {
        let stage = match error {
            ClientError::Authentication => Stage::PeerCredentials,
            ClientError::Protocol => Stage::DescriptorInvalid,
            _ => Stage::Connect,
        };
        failure::remember(stage, None);
    }
    failure::update(|f| f.start_timeout = true);
    ClientError::Unavailable
}

/// Read only a bounded regular file under the same private directory rules as rendezvous.
/// Consume only records bound to the generation established by a matching Hello.
/// Without that proof, keep the current connection/activation observation.
fn read_helper_failure(root: &Path, generation: Option<&str>) {
    let Some(generation) = generation else { return; };
    let Ok(directory) = OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root) else { return; };
    let Ok(meta) = directory.metadata() else { return; };
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o7777 != 0o700 { return; }
    #[cfg(target_os = "linux")]
    let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join("last-error");
    #[cfg(not(target_os = "linux"))]
    let path = root.join("last-error");
    let Ok(mut file) = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path) else { return; };
    let Ok(meta) = file.metadata() else { return; };
    if !meta.is_file() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != 1 || meta.len() > DESCRIPTOR_MAX { return; }
    let mut bytes = Vec::new();
    if Read::by_ref(&mut file).take(DESCRIPTOR_MAX + 1).read_to_end(&mut bytes).is_err() { return; }
    if let Some(detail) = failure::Record::parse(&bytes, generation) {
        failure::update(|f| f.helper = Some(detail));
    }
}

#[cfg(target_os = "linux")]
fn connect(root: &Path) -> Result<(UnixStream, Descriptor), ClientError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|e| {
            let stage = if e.raw_os_error() == Some(libc::ENOENT) { Stage::RuntimeAbsent } else { Stage::RuntimeInvalid };
            io_failed(stage, e)
        })?;
    let directory_meta = directory.metadata().map_err(|e| io_failed(Stage::RuntimeInvalid, e))?;
    if !directory_meta.is_dir()
        || directory_meta.uid() != unsafe { libc::getuid() }
        || directory_meta.mode() & 0o7777 != 0o700
    {
        return Err(failed(Stage::RuntimeInvalid, None, ClientError::Authentication));
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
        return Err(failed(Stage::DescriptorInvalid, None, ClientError::Protocol));
    }
    let socket = anchor.join(wire::SOCKET_NAME);
    let socket_meta = std::fs::symlink_metadata(&socket).map_err(|e| io_failed(
        if e.raw_os_error() == Some(libc::ENOENT) { Stage::SocketAbsent } else { Stage::SocketInvalid }, e))?;
    if !socket_meta.file_type().is_socket()
        || socket_meta.uid() != unsafe { libc::getuid() }
        || socket_meta.mode() & 0o7777 != 0o600
    {
        return Err(failed(Stage::SocketInvalid, None, ClientError::Authentication));
    }
    let stream = UnixStream::connect(socket).map_err(|e| io_failed(Stage::Connect, e))?;
    authenticate(&stream)?;
    let named_meta = std::fs::symlink_metadata(root).map_err(|e| io_failed(Stage::RuntimeInvalid, e))?;
    if (named_meta.dev(), named_meta.ino()) != (directory_meta.dev(), directory_meta.ino()) {
        return Err(failed(Stage::RuntimeInvalid, None, ClientError::Authentication));
    }
    Ok((stream, descriptor))
}

fn read_descriptor(path: &Path) -> Result<Descriptor, ClientError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| io_failed(Stage::DescriptorInvalid, e))?;
    let meta = file.metadata().map_err(|e| io_failed(Stage::DescriptorInvalid, e))?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::getuid() }
        || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != 1
        || meta.len() > DESCRIPTOR_MAX
    {
        return Err(failed(Stage::DescriptorInvalid, None, ClientError::Authentication));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(DESCRIPTOR_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_failed(Stage::DescriptorInvalid, e))?;
    if bytes.len() as u64 > DESCRIPTOR_MAX {
        return Err(failed(Stage::DescriptorInvalid, None, ClientError::Protocol));
    }
    serde_json::from_slice(&bytes).map_err(|_| failed(Stage::DescriptorInvalid, None, ClientError::Protocol))
}

#[cfg(target_os = "linux")]
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
    let errno = (result != 0).then(|| std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
    if let Some(detail) = peer_rejection(errno,
        length as usize == std::mem::size_of::<libc::ucred>(),
        credentials.uid == unsafe { libc::getuid() }, credentials.pid > 0) {
        return Err(failed(detail.stage, detail.code, ClientError::Authentication));
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
    fn helper_failure_peer_and_hello_classification() {
        assert_eq!(peer_rejection(Some(libc::EPERM), true, true, true),
            Some(failure::Detail::new(Stage::PeerCredentials, Some(libc::EPERM))));
        assert_eq!(peer_rejection(None, true, false, true).unwrap().stage, Stage::PeerUidMismatch);
        assert_eq!(peer_rejection(None, false, true, true).unwrap().stage, Stage::PeerCredentials);
        assert_eq!(peer_rejection(None, true, true, false).unwrap().stage, Stage::PeerCredentials);
        assert!(peer_rejection(None, true, true, true).is_none());
        let descriptor = Descriptor {
            protocol: wire::PROTOCOL, socket: wire::SOCKET_NAME.into(), uid: 1, pid: 1,
            nonce: "01".repeat(16), helper_generation: "02".repeat(16),
        };
        for code in [ErrorCode::Unavailable, ErrorCode::Timeout, ErrorCode::Capability,
            ErrorCode::Authentication, ErrorCode::Protocol, ErrorCode::Corrupt, ErrorCode::Invalid] {
            failure::clear();
            assert_eq!(validate_hello(Response::Error { code }, &descriptor), Err(classify_error(code)));
            let detail = failure::last().unwrap();
            assert_eq!(detail.observed.stage, Stage::HelloRejected);
            assert_eq!(detail.wire, Some(code));
        }
        for (protocol, nonce, generation, db8) in [
            (0, descriptor.nonce.clone(), descriptor.helper_generation.clone(), true),
            (wire::PROTOCOL, "private-fixture".into(), descriptor.helper_generation.clone(), true),
            (wire::PROTOCOL, descriptor.nonce.clone(), "wrong".into(), true),
            (wire::PROTOCOL, descriptor.nonce.clone(), descriptor.helper_generation.clone(), false),
        ] {
            assert_eq!(validate_hello(Response::Hello { protocol, nonce, helper_generation: generation,
                capabilities: wire::Capabilities { db8, keymanager: false } }, &descriptor), Err(ClientError::Protocol));
        }
    }

    #[test]
    fn helper_failure_connect_error_kept_at_start_deadline() {
        // The old loop discarded these observations and returned only Unavailable.
        for (stage, error, code) in [
            (Stage::RuntimeAbsent, ClientError::Unavailable, Some(libc::ENOENT)),
            (Stage::RuntimeInvalid, ClientError::Authentication, None),
            (Stage::SocketAbsent, ClientError::Unavailable, Some(libc::ENOENT)),
            (Stage::SocketInvalid, ClientError::Authentication, None),
            (Stage::Connect, ClientError::Unavailable, Some(libc::ECONNREFUSED)),
            (Stage::PeerUidMismatch, ClientError::Authentication, None),
            (Stage::PeerCredentials, ClientError::Authentication, Some(libc::EIO)),
            (Stage::DescriptorInvalid, ClientError::Protocol, None),
        ] {
            failure::clear();
            let observed = failed(stage, code, error);
            let mut gate = StartGate::default();
            let now = Instant::now();
            assert!(!gate.wait_after_failed_connect(now, now));
            assert_eq!(start_unavailable(observed), ClientError::Unavailable);
            let detail = failure::last().unwrap();
            assert_eq!(detail.observed, failure::Detail::new(stage, code));
            assert!(detail.start_timeout);
        }
    }

    #[test]
    fn helper_failure_descriptor_validation_and_last_error_are_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("plx-helper-diag-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("rendezvous.json");
        for (mode, bytes, expected) in [
            (0o644, b"{}".as_slice(), ClientError::Authentication),
            (0o600, b"not-json".as_slice(), ClientError::Protocol),
        ] {
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            failure::clear();
            assert_eq!(read_descriptor(&path).err(), Some(expected));
            assert_eq!(failure::last().unwrap().observed.stage, Stage::DescriptorInvalid);
        }
        let last = root.join("last-error");
        let detail = failure::Detail::new(Stage::Db8, Some(-3963));
        std::fs::write(&last, serde_json::to_vec(&failure::Record { helper_generation: "01".repeat(16), detail }).unwrap()).unwrap();
        std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o600)).unwrap();
        read_helper_failure(&root, Some(&"01".repeat(16)));
        assert_eq!(failure::last().unwrap().helper, Some(detail));
        std::fs::write(&last, b"token=private-fixture hostname=private-host").unwrap();
        failure::remember(Stage::HelloRejected, None);
        read_helper_failure(&root, Some(&"01".repeat(16)));
        assert_eq!(failure::last().unwrap().helper, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn previous_helper_failure_cannot_override_current_connect_failure() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("plx-helper-stale-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let last = root.join("last-error");
        std::fs::write(&last, serde_json::to_vec(&failure::Detail::new(Stage::Db8, Some(-3963))).unwrap()).unwrap();
        std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o600)).unwrap();
        failure::remember(Stage::Connect, Some(libc::ECONNREFUSED));
        read_helper_failure(&root, None);
        let current = failure::last().unwrap();
        assert_eq!(current.helper, None);
        assert_eq!(current.line(), format!("storage: helper · connect refused ({})", libc::ECONNREFUSED));
        // Legacy unbound records cannot be attributed even after a successful Hello.
        read_helper_failure(&root, Some(&"02".repeat(16)));
        assert_eq!(failure::last().unwrap().helper, None);
        let detail = failure::Detail::new(Stage::Db8, Some(-3963));
        let record = failure::Record { helper_generation: "01".repeat(16), detail };
        std::fs::write(&last, serde_json::to_vec(&record).unwrap()).unwrap();
        // An orphan from an unclean exit, or a successor replacing the file, must not match.
        for generation in [None, Some("02".repeat(16))] {
            read_helper_failure(&root, generation.as_deref());
            assert_eq!(failure::last().unwrap().helper, None);
        }
        read_helper_failure(&root, Some(&record.helper_generation));
        assert_eq!(failure::last().unwrap().helper, Some(detail));
        assert_eq!(failure::last().unwrap().line(), "storage: helper · db8 (-3963)");
        let report = serde_json::to_string(&failure::last().unwrap()).unwrap();
        assert!(!report.contains(&record.helper_generation));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn helper_failure_descriptor_open_keeps_errno() {
        use super::wire::failure::{self, Stage};
        failure::clear();
        let missing = std::env::temp_dir().join(format!("plx-helper-missing-{}", std::process::id()));
        assert!(read_descriptor(&missing).is_err());
        assert_eq!(failure::last().map(|f| f.observed),
            Some(failure::Detail::new(Stage::DescriptorInvalid, Some(libc::ENOENT))));
    }

    #[test]
    fn a_failed_start_makes_later_calls_return_without_waiting() {
        let mut gate = StartGate::default();
        let now = Instant::now();
        let until = now + START_DEADLINE;
        assert!(
            gate.wait_after_failed_connect(now, until),
            "the first call allows startup time"
        );
        assert!(!gate.wait_after_failed_connect(until, until));

        let next = until + Duration::from_millis(25);
        assert!(
            !gate.wait_after_failed_connect(next, next + START_DEADLINE),
            "a failed start must not buy another twelve-second wait"
        );
        let later = next + START_DEADLINE * 10;
        assert!(
            !gate.wait_after_failed_connect(later, later + START_DEADLINE),
            "elapsed time alone must not forget an unreachable helper"
        );
    }

    #[test]
    fn a_successful_connect_resets_the_failed_start() {
        let mut gate = StartGate::default();
        let now = Instant::now();
        assert!(!gate.wait_after_failed_connect(now, now));
        assert!(!gate.wait_after_failed_connect(now, now + START_DEADLINE));

        gate.connected();
        assert!(
            gate.wait_after_failed_connect(now, now + START_DEADLINE),
            "a recovered helper gets the normal startup window if it later restarts"
        );
    }

    #[test]
    fn activation_hints_are_rate_limited_without_reopening_the_start_window() {
        let mut gate = StartGate::default();
        let now = Instant::now();
        assert!(
            gate.hint_due(now),
            "the first failed connect hints activation immediately"
        );
        assert!(!gate.hint_due(now + START_DEADLINE / 2));
        assert!(!gate.wait_after_failed_connect(now + START_DEADLINE, now + START_DEADLINE));

        let next = now + START_DEADLINE;
        assert!(
            gate.hint_due(next),
            "a later call may re-hint a missing helper"
        );
        assert!(
            !gate.hint_due(next),
            "other callers share the hint rate limit"
        );
        assert!(
            !gate.wait_after_failed_connect(next, next + START_DEADLINE),
            "re-hinting must not restart the blocking wait"
        );

        gate.connected();
        assert!(
            gate.hint_due(next),
            "a recovered helper may be activated again if it restarts"
        );
    }

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
