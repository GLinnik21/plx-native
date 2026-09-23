//! Allowlisted, group-readable storage evidence. Only the diagnostics worker probes or publishes;
//! callers send typed outcomes, never protocol payloads or platform error text. No telemetry sink.

use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
};

use super::wire::{
    failure::{Detail, HelperFailure, Stage},
    ErrorCode,
};

pub(crate) const NAME: &str = crate::paths::runtime_file::STORAGE_DIAGNOSTICS;
const LIMIT: usize = 16 * 1024;
const MARKER: &str = "truncated value=true\n";

#[derive(Clone, Debug)]
struct HelperOutcome {
    seen: bool,
    failure: Option<HelperFailure>,
    pub attempts: u32,
    pub elapsed_ms: u64,
    changes: Vec<Option<HelperFailure>>,
    pub changes_truncated: bool,
}
impl HelperOutcome {
    fn pending() -> Self {
        Self {
            seen: false,
            failure: None,
            attempts: 0,
            elapsed_ms: 0,
            changes: Vec::new(),
            changes_truncated: false,
        }
    }
    fn observe(&mut self, failure: Option<HelperFailure>) {
        if self.changes.last() != Some(&failure) {
            if self.changes.len() < 32 {
                self.changes.push(failure);
            } else {
                self.changes_truncated = true;
            }
        }
        self.failure = failure;
    }
}

#[derive(Clone, Copy, Debug)]
struct Activation {
    detail: Option<Detail>,
    pub elapsed_ms: u64,
}

enum Update {
    Helper(HelperOutcome),
    Activation(Activation),
}
impl Update {
    fn same_status(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Helper(a), Self::Helper(b)) => (a.seen, a.failure) == (b.seen, b.failure),
            (Self::Activation(a), Self::Activation(b)) => a.detail == b.detail,
            _ => false,
        }
    }
}
#[derive(Default)]
struct Pending(std::collections::VecDeque<Update>);
impl Pending {
    fn push(&mut self, update: Update) {
        let kind = std::mem::discriminant(&update);
        if let Some(last) = self
            .0
            .iter_mut()
            .rev()
            .find(|last| std::mem::discriminant(*last) == kind)
        {
            if last.same_status(&update) {
                *last = update;
                return;
            }
        }
        self.0.push_back(update);
    }
}

struct Mailbox {
    pending: Mutex<Pending>,
    wake: Condvar,
}
static WORKER: Mutex<Option<Arc<Mailbox>>> = Mutex::new(None);
static DISABLED: AtomicBool = AtomicBool::new(false);
static PUBLICATION: Mutex<()> = Mutex::new(());

/// Boot and producers only admit typed state. A single worker retains the boot probe results.
pub(crate) fn start() {
    let _ = mailbox();
}
fn mailbox() -> Option<Arc<Mailbox>> {
    if DISABLED.load(Ordering::Acquire) {
        return None;
    }
    let mut worker = WORKER.lock().unwrap_or_else(|e| e.into_inner());
    if DISABLED.load(Ordering::Acquire) {
        return None;
    }
    if worker.is_none() {
        let mailbox = Arc::new(Mailbox {
            pending: Mutex::new(Pending::default()),
            wake: Condvar::new(),
        });
        let owned = mailbox.clone();
        if crate::task::spawn("storage diagnostics", move || run(owned)).is_none() {
            return None;
        }
        *worker = Some(mailbox);
    }
    worker.clone()
}
pub(crate) fn helper(failure: Option<HelperFailure>, attempts: u32, elapsed_ms: u64) {
    submit(Update::Helper(HelperOutcome {
        seen: true,
        failure,
        attempts,
        elapsed_ms,
        changes: Vec::new(),
        changes_truncated: false,
    }));
}
pub(crate) fn activation(detail: Detail, elapsed_ms: u64) {
    submit(Update::Activation(Activation {
        detail: Some(detail),
        elapsed_ms,
    }));
}
fn submit(update: Update) {
    if let Some(mailbox) = mailbox() {
        mailbox
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(update);
        mailbox.wake.notify_one();
    }
}
/// Permanently stop publication for this process. This is called from the frame thread and must
/// remain a non-blocking atomic; the erasure worker waits out publication and removes the file.
pub(crate) fn disable() {
    DISABLED.store(true, Ordering::Release);
}
#[cfg(test)]
pub(crate) fn reset_for_test() {
    DISABLED.store(false, Ordering::Release);
}
/// Called only from the existing erasure worker after [`disable`]. Waiting here cannot stall a
/// frame; once the publication lock is acquired, no earlier snapshot can still be renamed in.
pub(crate) fn finish_disable(root: &Path) -> Result<(), String> {
    let _block = guard();
    let _publication = PUBLICATION.lock().unwrap_or_else(|e| e.into_inner());
    let dir = directory(root).map_err(|error| format!("{NAME}: {error}"))?;
    let result = unlink(&dir, &destination_name());
    if result == 0 || result == libc::ENOENT {
        Ok(())
    } else {
        Err(format!("{NAME}: {}", io::Error::from_raw_os_error(result)))
    }
}
fn publish_if_enabled(
    snapshot: &mut Snapshot,
    root: &Path,
    published: &mut Option<(bool, Option<HelperFailure>, Option<Detail>)>,
) {
    if !DISABLED.load(Ordering::Acquire) {
        snapshot.publish_changed(root, published);
    }
}
fn run(mailbox: Arc<Mailbox>) {
    let _block = guard();
    let root = crate::paths::runtime_dir();
    let mut snapshot = Snapshot::boot();
    let mut published = None;
    publish_if_enabled(&mut snapshot, root, &mut published);
    loop {
        let update = {
            let mut pending = mailbox.pending.lock().unwrap_or_else(|e| e.into_inner());
            while pending.0.is_empty() {
                pending = mailbox
                    .wake
                    .wait(pending)
                    .unwrap_or_else(|e| e.into_inner());
            }
            pending.0.pop_front().unwrap()
        };
        match update {
            Update::Helper(helper) => {
                snapshot.helper.seen = helper.seen;
                snapshot.helper.observe(helper.failure);
                snapshot.helper.attempts = helper.attempts;
                snapshot.helper.elapsed_ms = helper.elapsed_ms;
            }
            Update::Activation(activation) => snapshot.activation = activation,
        }
        publish_if_enabled(&mut snapshot, root, &mut published);
    }
}
fn guard() -> crate::task::BlockingGuard {
    crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("storage diagnostics") })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Directory {
    Tmp,
    Runtime,
    MediaDeveloper,
    MediaInternal,
    AppDir,
    LegacyAppDir,
}
impl Directory {
    fn label(self) -> &'static str {
        match self {
            Self::Tmp => "tmp",
            Self::Runtime => "runtime",
            Self::MediaDeveloper => "media_developer",
            Self::MediaInternal => "media_internal",
            Self::AppDir => "app_dir",
            Self::LegacyAppDir => "legacy_app_dir",
        }
    }
}
struct Probe {
    label: Directory,
    uid: Option<u32>,
    gid: Option<u32>,
    mode: Option<u32>,
    readonly: Option<bool>,
    open: i32,
    stat: Option<i32>,
    mount: Option<i32>,
    create: Option<i32>,
    write: Option<i32>,
    close: Option<i32>,
    unlink: Option<i32>,
}
struct Snapshot {
    seq: u64,
    app_id: Identifier,
    flavour: Identifier,
    version: Identifier,
    uid: u32,
    euid: u32,
    gid: u32,
    egid: u32,
    groups: Vec<u32>,
    groups_errno: i32,
    groups_truncated: bool,
    probes: Vec<Probe>,
    helper: HelperOutcome,
    activation: Activation,
}
struct Identifier(String);
impl Identifier {
    fn new(value: &str) -> Self {
        Self(
            if !value.is_empty()
                && value.len() <= 96
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                value.to_owned()
            } else {
                "invalid".to_owned()
            },
        )
    }
}
impl Snapshot {
    fn boot() -> Self {
        let _block = guard();
        let mut directories = vec![
            (Directory::Tmp, PathBuf::from("/tmp")),
            (
                Directory::Runtime,
                crate::paths::runtime_dir().to_path_buf(),
            ),
        ];
        for path in crate::paths::session_candidates() {
            let Some(parent) = path.parent() else {
                continue;
            };
            if directories.iter().any(|(_, p)| p == parent) {
                continue;
            }
            let label = if parent == Path::new("/media/developer") {
                Directory::MediaDeveloper
            } else if parent == Path::new("/media/internal") {
                Directory::MediaInternal
            } else if parent == crate::paths::app_dir() {
                Directory::AppDir
            } else {
                Directory::LegacyAppDir
            };
            directories.push((label, parent.to_owned()));
        }
        let mut groups = vec![0; 256];
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        let groups_truncated = count > 256;
        let n = if (0..=256).contains(&count) {
            unsafe { libc::getgroups(256, groups.as_mut_ptr()) }
        } else {
            -1
        };
        let groups_errno = if groups_truncated {
            libc::E2BIG
        } else if n < 0 {
            errno()
        } else {
            0
        };
        groups.truncate(n.max(0) as usize);
        Self {
            seq: 0,
            app_id: Identifier::new(crate::paths::app_id()),
            flavour: Identifier::new(crate::paths::flavour().unwrap_or("stable")),
            version: Identifier::new(env!("PLX_VERSION")),
            uid: unsafe { libc::getuid() },
            euid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getgid() },
            egid: unsafe { libc::getegid() },
            groups,
            groups_errno,
            groups_truncated,
            probes: directories
                .iter()
                .take(8)
                .map(|(label, path)| probe(*label, path))
                .collect(),
            helper: HelperOutcome::pending(),
            activation: Activation {
                detail: None,
                elapsed_ms: 0,
            },
        }
    }
    fn semantic(&self) -> (bool, Option<HelperFailure>, Option<Detail>) {
        (
            self.helper.seen,
            self.helper.failure,
            self.activation.detail,
        )
    }
    fn publish_changed(
        &mut self,
        root: &Path,
        published: &mut Option<(bool, Option<HelperFailure>, Option<Detail>)>,
    ) {
        let semantic = self.semantic();
        if *published == Some(semantic) {
            return;
        }
        self.seq += 1;
        if publish(root, self.format().as_bytes()).is_ok() {
            *published = Some(semantic);
        }
    }
    fn format(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "identity schema=1 seq={} app_id={} flavour={} version={} uid={} euid={} gid={} egid={}",
            self.seq,
            self.app_id.0,
            self.flavour.0,
            self.version.0,
            self.uid,
            self.euid,
            self.gid,
            self.egid
        );
        let groups = if self.groups.is_empty() {
            "-".to_owned()
        } else {
            self.groups
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        let _ = writeln!(
            out,
            "groups values={} errno={} truncated={}",
            groups,
            errno_value(self.groups_errno),
            self.groups_truncated
        );
        for p in &self.probes {
            let mode = p.mode.map(|mode| format!("{mode:04o}"));
            let _ = writeln!(out, "dir label={} uid={} gid={} mode={} readonly={} open_errno={} stat_errno={} mount_errno={} create_errno={} write_errno={} close_errno={} unlink_errno={}",
                p.label.label(), optional_number(p.uid), optional_number(p.gid), optional(mode.as_deref()),
                optional_bool(p.readonly), errno_value(p.open), optional_errno(p.stat),
                optional_errno(p.mount), optional_errno(p.create), optional_errno(p.write),
                optional_errno(p.close), optional_errno(p.unlink));
        }
        let (activation_stage, activation_code) = self.activation.detail.map_or_else(
            || ("-".to_owned(), "-".to_owned()),
            |detail| (stage_label(detail.stage), optional_number(detail.code)),
        );
        let _ = writeln!(
            out,
            "activation stage={activation_stage} error_code={activation_code} elapsed_ms={}",
            self.activation.elapsed_ms
        );
        let _ = write_helper(
            &mut out,
            "helper",
            self.helper.seen,
            self.helper.failure,
            Some((
                self.helper.attempts,
                self.helper.elapsed_ms,
                self.helper.changes_truncated,
            )),
        );
        // Keep the latest outcome ahead of history so the size cap cannot hide recovery.
        for failure in &self.helper.changes {
            let _ = write_helper(&mut out, "history", true, *failure, None);
        }
        cap(out)
    }
}

fn write_helper(
    out: &mut String,
    record: &str,
    seen: bool,
    failure: Option<HelperFailure>,
    current: Option<(u32, u64, bool)>,
) -> std::fmt::Result {
    use std::fmt::Write;
    if !seen {
        write!(out, "{record} stage=pending errno=- code=- start_timeout=false activation_stage=- activation_error_code=- reported_stage=- reported_code=- wire_code=-")?;
        if let Some((attempts, elapsed_ms, history_truncated)) = current {
            write!(out, " attempts={attempts} elapsed_ms={elapsed_ms} history_truncated={history_truncated}")?;
        }
        return writeln!(out);
    }
    let Some(failure) = failure else {
        write!(out, "{record} stage=complete errno=- code=- start_timeout=false activation_stage=- activation_error_code=- reported_stage=- reported_code=- wire_code=-")?;
        if let Some((attempts, elapsed_ms, history_truncated)) = current {
            write!(out, " attempts={attempts} elapsed_ms={elapsed_ms} history_truncated={history_truncated}")?;
        }
        return writeln!(out);
    };
    let (errno, code) = detail_values(failure.observed);
    let (activation_stage, activation_code) = detail_fields(failure.activation);
    let (reported_stage, reported_code) = detail_fields(failure.helper);
    write!(out,
        "{record} stage={} errno={errno} code={code} start_timeout={} activation_stage={activation_stage} activation_error_code={activation_code} reported_stage={reported_stage} reported_code={reported_code} wire_code={}",
        stage_label(failure.observed.stage), failure.start_timeout, wire_code(failure.wire))?;
    if let Some((attempts, elapsed_ms, history_truncated)) = current {
        write!(
            out,
            " attempts={attempts} elapsed_ms={elapsed_ms} history_truncated={history_truncated}"
        )?;
    }
    writeln!(out)
}

fn detail_fields(detail: Option<Detail>) -> (String, String) {
    detail.map_or_else(
        || ("-".to_owned(), "-".to_owned()),
        |detail| (stage_label(detail.stage), optional_number(detail.code)),
    )
}

fn detail_values(detail: Detail) -> (String, String) {
    if matches!(
        detail.stage,
        Stage::RuntimeAbsent
            | Stage::RuntimeInvalid
            | Stage::SocketAbsent
            | Stage::SocketInvalid
            | Stage::Connect
            | Stage::PeerCredentials
            | Stage::DescriptorInvalid
            | Stage::SocketTimeout
            | Stage::Wire
    ) {
        (
            detail
                .code
                .map(errno_value)
                .unwrap_or_else(|| "-".to_owned()),
            "-".to_owned(),
        )
    } else {
        ("-".to_owned(), optional_number(detail.code))
    }
}

fn stage_label(stage: Stage) -> String {
    serde_json::to_value(stage)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}
fn optional(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}
fn optional_number(value: Option<impl std::fmt::Display>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned())
}
fn optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "-",
    }
}
fn optional_errno(value: Option<i32>) -> String {
    value.map(errno_value).unwrap_or_else(|| "-".to_owned())
}
fn errno_value(value: i32) -> String {
    let name = match value {
        libc::EPERM => Some("EPERM"),
        libc::ENOENT => Some("ENOENT"),
        libc::ESRCH => Some("ESRCH"),
        libc::EINTR => Some("EINTR"),
        libc::EIO => Some("EIO"),
        libc::ENXIO => Some("ENXIO"),
        libc::E2BIG => Some("E2BIG"),
        libc::EBADF => Some("EBADF"),
        libc::EAGAIN => Some("EAGAIN"),
        libc::ENOMEM => Some("ENOMEM"),
        libc::EACCES => Some("EACCES"),
        libc::EFAULT => Some("EFAULT"),
        libc::EBUSY => Some("EBUSY"),
        libc::EEXIST => Some("EEXIST"),
        libc::EXDEV => Some("EXDEV"),
        libc::ENODEV => Some("ENODEV"),
        libc::ENOTDIR => Some("ENOTDIR"),
        libc::EISDIR => Some("EISDIR"),
        libc::EINVAL => Some("EINVAL"),
        libc::ENFILE => Some("ENFILE"),
        libc::EMFILE => Some("EMFILE"),
        libc::ENOTTY => Some("ENOTTY"),
        libc::EFBIG => Some("EFBIG"),
        libc::ENOSPC => Some("ENOSPC"),
        libc::ESPIPE => Some("ESPIPE"),
        libc::EROFS => Some("EROFS"),
        libc::EMLINK => Some("EMLINK"),
        libc::EPIPE => Some("EPIPE"),
        libc::ERANGE => Some("ERANGE"),
        libc::ENAMETOOLONG => Some("ENAMETOOLONG"),
        libc::ENOSYS => Some("ENOSYS"),
        libc::ENOTEMPTY => Some("ENOTEMPTY"),
        libc::ELOOP => Some("ELOOP"),
        libc::EOVERFLOW => Some("EOVERFLOW"),
        libc::ENOTSOCK => Some("ENOTSOCK"),
        libc::EMSGSIZE => Some("EMSGSIZE"),
        libc::EPROTO => Some("EPROTO"),
        libc::ENOPROTOOPT => Some("ENOPROTOOPT"),
        libc::EPROTONOSUPPORT => Some("EPROTONOSUPPORT"),
        libc::EOPNOTSUPP => Some("EOPNOTSUPP"),
        libc::EAFNOSUPPORT => Some("EAFNOSUPPORT"),
        libc::EADDRINUSE => Some("EADDRINUSE"),
        libc::EADDRNOTAVAIL => Some("EADDRNOTAVAIL"),
        libc::ENETDOWN => Some("ENETDOWN"),
        libc::ENETUNREACH => Some("ENETUNREACH"),
        libc::ENETRESET => Some("ENETRESET"),
        libc::ECONNABORTED => Some("ECONNABORTED"),
        libc::ECONNRESET => Some("ECONNRESET"),
        libc::ENOBUFS => Some("ENOBUFS"),
        libc::EISCONN => Some("EISCONN"),
        libc::ENOTCONN => Some("ENOTCONN"),
        libc::ETIMEDOUT => Some("ETIMEDOUT"),
        libc::ECONNREFUSED => Some("ECONNREFUSED"),
        libc::EHOSTUNREACH => Some("EHOSTUNREACH"),
        _ => None,
    };
    match name {
        Some(name) => format!("{value}/{name}"),
        None => value.to_string(),
    }
}
fn wire_code(value: Option<ErrorCode>) -> &'static str {
    match value {
        Some(ErrorCode::Invalid) => "invalid",
        Some(ErrorCode::Unavailable) => "unavailable",
        Some(ErrorCode::Timeout) => "timeout",
        Some(ErrorCode::Capability) => "capability",
        Some(ErrorCode::Authentication) => "authentication",
        Some(ErrorCode::Protocol) => "protocol",
        Some(ErrorCode::Corrupt) => "corrupt",
        None => "-",
    }
}
fn cap(mut text: String) -> String {
    const COMPLETE: &str = "truncated value=false\n";
    if text.len() + COMPLETE.len() <= LIMIT {
        text.push_str(COMPLETE);
    } else {
        let mut end = LIMIT - MARKER.len();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let complete_line = text[..end].rfind('\n').map_or(0, |at| at + 1);
        text.truncate(complete_line);
        text.push_str(MARKER);
    }
    text
}
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
fn io_errno(error: io::Error) -> i32 {
    error.raw_os_error().unwrap_or(0)
}
fn directory(path: &Path) -> io::Result<File> {
    let _block = guard();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn unique() -> CString {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    CString::new(format!(
        ".plxdiag-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
    .unwrap()
}
fn destination_name() -> CString {
    CString::new(NAME).expect("fixed diagnostics filename")
}
fn create(dir: &File, name: &CString) -> io::Result<File> {
    let _block = guard();
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn unlink(dir: &File, name: &CString) -> i32 {
    let _block = guard();
    if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        0
    } else {
        errno()
    }
}
fn probe(label: Directory, path: &Path) -> Probe {
    let _block = guard();
    let mut p = Probe {
        label,
        uid: None,
        gid: None,
        mode: None,
        readonly: None,
        open: 0,
        stat: None,
        mount: None,
        create: None,
        write: None,
        close: None,
        unlink: None,
    };
    let dir = match directory(path) {
        Ok(d) => d,
        Err(e) => {
            p.open = io_errno(e);
            return p;
        }
    };
    match dir.metadata() {
        Ok(meta) => {
            p.uid = Some(meta.uid());
            p.gid = Some(meta.gid());
            p.mode = Some(meta.mode() & 0o7777);
            p.stat = Some(0);
        }
        Err(e) => p.stat = Some(io_errno(e)),
    }
    let mut mount: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(dir.as_raw_fd(), &mut mount) } == 0 {
        p.readonly = Some(mount.f_flag & libc::ST_RDONLY as libc::c_ulong != 0);
        p.mount = Some(0);
    } else {
        p.mount = Some(errno());
    }
    let name = unique();
    match create(&dir, &name) {
        Ok(mut file) => {
            p.create = Some(0);
            p.write = Some(file.write_all(&[0]).err().map(io_errno).unwrap_or(0));
            p.close = Some(if unsafe { libc::close(file.into_raw_fd()) } == 0 {
                0
            } else {
                errno()
            });
            p.unlink = Some(unlink(&dir, &name));
        }
        Err(e) => p.create = Some(io_errno(e)),
    }
    p
}
fn safe_destination(stat: &libc::stat, uid: u32) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG && stat.st_uid == uid && stat.st_nlink == 1
}
fn destination(dir: &File) -> io::Result<()> {
    let _block = guard();
    let name = destination_name();
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        if safe_destination(&stat, unsafe { libc::geteuid() }) {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::EPERM))
        }
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        }
    }
}
fn publish(root: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_with(root, bytes, || Ok(()))
}
fn publish_with(
    root: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let _block = guard();
    let _publication = PUBLICATION.lock().unwrap_or_else(|e| e.into_inner());
    if DISABLED.load(Ordering::Acquire) {
        return Err(io::Error::from_raw_os_error(libc::ECANCELED));
    }
    if bytes.len() > LIMIT {
        return Err(io::Error::from_raw_os_error(libc::EFBIG));
    }
    let dir = directory(root)?;
    destination(&dir)?;
    let name = unique();
    let mut file = create(&dir, &name)?;
    let result = (|| {
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.nlink() != 1
            || meta.gid() != unsafe { libc::getegid() }
        {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        file.write_all(bytes)?;
        if unsafe { libc::fchmod(file.as_raw_fd(), 0o640) } != 0 {
            return Err(io::Error::last_os_error());
        }
        before_rename()?;
        destination(&dir)?;
        if DISABLED.load(Ordering::Acquire) {
            return Err(io::Error::from_raw_os_error(libc::ECANCELED));
        }
        let destination = destination_name();
        if unsafe {
            libc::renameat(
                dir.as_raw_fd(),
                name.as_ptr(),
                dir.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = unlink(&dir, &name);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let path = std::env::temp_dir().join(unique().to_str().unwrap());
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> Snapshot {
        Snapshot {
            seq: 1,
            app_id: Identifier::new("com.beb.plxnative"),
            flavour: Identifier::new("stable"),
            version: Identifier::new("0.6.0"),
            uid: 6303,
            euid: 6303,
            gid: 5000,
            egid: 5000,
            groups: vec![29, 44, 5000],
            groups_errno: 0,
            groups_truncated: false,
            probes: Vec::new(),
            helper: HelperOutcome::pending(),
            activation: Activation {
                detail: None,
                elapsed_ms: 0,
            },
        }
    }

    fn helper_outcome(failure: Option<HelperFailure>) -> HelperOutcome {
        HelperOutcome {
            seen: true,
            failure,
            attempts: 1,
            elapsed_ms: 1,
            changes: Vec::new(),
            changes_truncated: false,
        }
    }

    #[test]
    fn formatter_accepts_only_bounded_identifiers_and_fixed_labels() {
        let _serial = crate::testlock::serial();
        for bad in [
            "/private/account",
            "token=secret",
            "two words",
            "line\nbreak",
            "{\"secret\":true}",
            "",
            &"x".repeat(97),
        ] {
            assert_eq!(Identifier::new(bad).0, "invalid");
        }
        let root = Root::new();
        let mut snapshot = fixture();
        snapshot.probes.extend([
            Probe {
                label: Directory::Tmp,
                uid: Some(0),
                gid: Some(0),
                mode: Some(0o1777),
                readonly: Some(false),
                open: 0,
                stat: Some(0),
                mount: Some(0),
                create: Some(libc::EACCES),
                write: None,
                close: None,
                unlink: None,
            },
            Probe {
                label: Directory::Runtime,
                uid: Some(6303),
                gid: Some(5000),
                mode: Some(0o700),
                readonly: Some(true),
                open: libc::ENOENT,
                stat: None,
                mount: None,
                create: None,
                write: None,
                close: None,
                unlink: None,
            },
        ]);
        let mut failure = HelperFailure::new(Stage::RuntimeAbsent, Some(libc::ENOENT));
        failure.start_timeout = true;
        failure.activation = Some(Detail::new(Stage::ActivationRejected, Some(-1)));
        failure.helper = Some(Detail::new(Stage::Db8, Some(-3963)));
        failure.wire = Some(ErrorCode::Unavailable);
        snapshot.helper.seen = true;
        snapshot.helper.failure = Some(failure);
        snapshot.helper.attempts = 3;
        snapshot.helper.elapsed_ms = 51;
        snapshot.helper.changes.push(Some(failure));
        let text = snapshot.format();
        assert!(text.starts_with("identity schema=1 seq=1 "));
        assert!(text.contains("\ngroups values=29,44,5000 errno=0 truncated=false\n"));
        assert!(text.contains("\ndir label=tmp uid=0 gid=0 mode=1777 readonly=false open_errno=0 stat_errno=0 mount_errno=0 create_errno=13/EACCES write_errno=- close_errno=- unlink_errno=-\n"));
        assert!(text.contains("\ndir label=runtime uid=6303 gid=5000 mode=0700 readonly=true open_errno=2/ENOENT stat_errno=- mount_errno=- create_errno=- write_errno=- close_errno=- unlink_errno=-\n"));
        assert!(text.contains("\nhelper stage=runtime-absent errno=2/ENOENT code=- start_timeout=true activation_stage=activation-rejected activation_error_code=-1 reported_stage=db8 reported_code=-3963 wire_code=unavailable attempts=3 elapsed_ms=51 history_truncated=false\n"));
        assert!(text.contains("\nhistory stage=runtime-absent errno=2/ENOENT code=- start_timeout=true activation_stage=activation-rejected activation_error_code=-1 reported_stage=db8 reported_code=-3963 wire_code=unavailable\n"));
        assert!(!text.contains(root.0.to_str().unwrap()));
        for rust_debug in ["Some(", "None", "Status {", "[29", "activation="] {
            assert!(
                !text.contains(rust_debug),
                "derived Debug leaked: {rust_debug}"
            );
        }
        for line in text.lines() {
            let (record, fields) = line.split_once(' ').expect("record has a leading key");
            assert!(
                [
                    "identity",
                    "groups",
                    "dir",
                    "activation",
                    "helper",
                    "history",
                    "truncated"
                ]
                .contains(&record),
                "unknown record key: {record}"
            );
            assert!(fields.split_ascii_whitespace().all(|field| {
                field
                    .split_once('=')
                    .is_some_and(|(key, value)| !key.is_empty() && !value.is_empty())
            }));
            assert!(!line.bytes().any(|byte| b"[]{}()\"'".contains(&byte)));
        }
        assert!(text.ends_with("truncated value=false\n"));
        for (label, expected) in [
            (Directory::Tmp, "tmp"),
            (Directory::Runtime, "runtime"),
            (Directory::MediaDeveloper, "media_developer"),
            (Directory::MediaInternal, "media_internal"),
            (Directory::AppDir, "app_dir"),
            (Directory::LegacyAppDir, "legacy_app_dir"),
        ] {
            assert_eq!(label.label(), expected);
        }
        snapshot.activation = Activation {
            detail: Some(Detail::new(Stage::ActivationRejected, Some(-42))),
            elapsed_ms: 9,
        };
        let text = snapshot.format();
        assert!(text.contains("activation stage=activation-rejected error_code=-42 elapsed_ms=9"));
        assert!(!text.contains("private"));
        assert_eq!(
            errno_value(libc::ENOENT),
            format!("{}/ENOENT", libc::ENOENT)
        );
        assert_eq!(errno_value(123_456), "123456");
    }

    #[test]
    fn pending_updates_preserve_failure_recovery_order() {
        let mut pending = Pending::default();
        let failure = HelperFailure::new(Stage::Connect, Some(libc::ECONNREFUSED));
        let failed = helper_outcome(Some(failure));
        pending.push(Update::Helper(failed.clone()));
        pending.push(Update::Activation(Activation {
            detail: Some(Detail::new(Stage::ActivationTimeout, None)),
            elapsed_ms: 600,
        }));
        pending.push(Update::Helper(failed));
        assert_eq!(
            pending.0.len(),
            2,
            "activation does not defeat helper deduplication"
        );
        pending.0.pop_back();
        pending.push(Update::Helper(helper_outcome(None)));
        assert_eq!(pending.0.len(), 2);
        let Update::Helper(first) = pending.0.pop_front().unwrap() else {
            panic!("helper");
        };
        let Update::Helper(second) = pending.0.pop_front().unwrap() else {
            panic!("helper");
        };
        assert_eq!(first.failure, Some(failure));
        assert_eq!(second.failure, None);
    }

    #[test]
    fn format_is_capped_with_an_explicit_truncation_marker() {
        for n in [0, LIMIT - 20, LIMIT, LIMIT * 2] {
            let text = cap("x".repeat(n));
            assert!(text.len() <= LIMIT);
            assert!(
                text.ends_with(if n + "truncated value=false\n".len() <= LIMIT {
                    "truncated value=false\n"
                } else {
                    MARKER
                })
            );
        }
        let text = cap(format!("identity value={}\n", "é".repeat(LIMIT)));
        assert!(text.len() <= LIMIT);
        assert_eq!(text, MARKER, "a capped snapshot contains no partial record");
    }

    #[test]
    fn publication_is_0640_even_under_a_restrictive_umask() {
        let _serial = crate::testlock::serial();
        struct Mask(libc::mode_t);
        impl Drop for Mask {
            fn drop(&mut self) {
                unsafe {
                    libc::umask(self.0);
                }
            }
        }
        let root = Root::new();
        let _mask = Mask(unsafe { libc::umask(0o777) });
        publish(&root.0, b"schema=1\n").unwrap();
        let meta = std::fs::metadata(root.0.join(NAME)).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o640);
        assert_eq!(meta.gid(), unsafe { libc::getegid() });
        assert_eq!(meta.nlink(), 1);
    }

    #[test]
    fn unsafe_destinations_and_directory_symlinks_are_rejected() {
        let _serial = crate::testlock::serial();
        let root = Root::new();
        let target = root.0.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&target, root.0.join(NAME)).unwrap();
        assert!(publish(&root.0, b"replacement").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        let link = root.0.join("directory-link");
        symlink(&root.0, &link).unwrap();
        assert!(publish(&link, b"replacement").is_err());
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        stat.st_mode = libc::S_IFREG | 0o640;
        stat.st_uid = unsafe { libc::geteuid() };
        stat.st_nlink = 1;
        assert!(safe_destination(&stat, stat.st_uid));
        assert!(
            !safe_destination(&stat, stat.st_uid.wrapping_add(1)),
            "foreign owner injection"
        );
        stat.st_nlink = 2;
        assert!(!safe_destination(&stat, stat.st_uid));
    }

    #[test]
    fn publication_failure_cleans_temp_and_preserves_previous_snapshot() {
        let _serial = crate::testlock::serial();
        let root = Root::new();
        publish(&root.0, b"previous").unwrap();
        assert!(publish_with(&root.0, b"replacement", || Err(
            io::Error::from_raw_os_error(libc::EIO)
        ))
        .is_err());
        assert_eq!(std::fs::read(root.0.join(NAME)).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
        // Recheck the destination after writing the temporary file, not only before creation.
        assert!(publish_with(&root.0, b"replacement", || {
            std::fs::remove_file(root.0.join(NAME))?;
            symlink("missing", root.0.join(NAME))
        })
        .is_err());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn duplicate_status_is_coalesced_and_recovery_is_published() {
        let _serial = crate::testlock::serial();
        let root = Root::new();
        let mut snapshot = fixture();
        snapshot.probes.push(probe(Directory::Runtime, &root.0));
        let mut published = None;
        snapshot.publish_changed(&root.0, &mut published);
        let boot = std::fs::read_to_string(root.0.join(NAME)).unwrap();
        snapshot.helper.attempts = 900;
        snapshot.helper.elapsed_ms = 9999;
        snapshot.publish_changed(&root.0, &mut published);
        assert_eq!(std::fs::read_to_string(root.0.join(NAME)).unwrap(), boot);
        snapshot.helper.seen = true;
        snapshot.helper.observe(Some(HelperFailure::new(
            Stage::Connect,
            Some(libc::ECONNREFUSED),
        )));
        snapshot.publish_changed(&root.0, &mut published);
        let failed = std::fs::read_to_string(root.0.join(NAME)).unwrap();
        assert_ne!(failed, boot);
        snapshot.helper.observe(None);
        snapshot.publish_changed(&root.0, &mut published);
        let recovered = std::fs::read_to_string(root.0.join(NAME)).unwrap();
        assert_ne!(failed, recovered);
        assert_eq!(snapshot.seq, 4);
        assert_eq!(
            boot.lines().find(|s| s.starts_with("dir=")),
            recovered.lines().find(|s| s.starts_with("dir="))
        );
        let previous = published;
        snapshot
            .helper
            .observe(Some(HelperFailure::new(Stage::Wire, Some(libc::EIO))));
        std::fs::remove_file(root.0.join(NAME)).unwrap();
        symlink("absent", root.0.join(NAME)).unwrap();
        snapshot.publish_changed(&root.0, &mut published);
        assert_eq!(
            published, previous,
            "a failed publication remains retryable"
        );
    }

    #[test]
    fn probe_creates_one_byte_and_removes_only_its_unique_file() {
        let _serial = crate::testlock::serial();
        let root = Root::new();
        let unrelated = root.0.join("untouched");
        std::fs::write(&unrelated, b"sentinel").unwrap();
        let result = probe(Directory::Runtime, &root.0);
        assert_eq!(
            (result.create, result.write, result.close, result.unlink),
            (Some(0), Some(0), Some(0), Some(0))
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"sentinel");
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
        assert!(!unique().to_str().unwrap().starts_with("plxnative-"));
        assert_eq!(
            probe(Directory::Runtime, &root.0.join("absent")).open,
            libc::ENOENT
        );
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn filesystem_work_is_refused_inside_a_frame() {
        let _serial = crate::testlock::serial();
        let root = Root::new();
        let _frame = crate::task::FrameScope::enter();
        assert!(std::panic::catch_unwind(|| publish(&root.0, b"schema=1\n")).is_err());
        assert!(std::panic::catch_unwind(|| probe(Directory::Runtime, &root.0)).is_err());
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[test]
    fn worker_start_follows_the_live_boot_identity_preamble() {
        let source = include_str!("../app/mod.rs");
        assert_eq!(
            source
                .matches("crate::storage::diagnostics::start()")
                .count(),
            1,
            "every start site must stay behind the identity preamble"
        );
        let pre_boot = source
            .split_once("fn pre_boot_diagnostics")
            .expect("pre_boot_diagnostics")
            .1
            .split_once("unsafe fn run_and_shutdown")
            .expect("end of pre_boot_diagnostics")
            .0;
        let install = pre_boot.find("\"install: id=").expect("install line");
        let app_dir = pre_boot
            .find("crate::paths::app_dir()")
            .expect("appdir line");
        assert!(install < app_dir);
        let body = source
            .split_once("fn enter_application")
            .expect("enter_application")
            .1
            .split_once("/// The ten-line public skeleton")
            .expect("end of enter_application")
            .0;
        let identity = body
            .find(".then(pre_boot_diagnostics)")
            .expect("identity preamble");
        let diagnostics = body
            .find("crate::storage::diagnostics::start()")
            .expect("diagnostics start");
        let boot = body.find("boot(pms_host").expect("application boot");
        assert!(identity < diagnostics && diagnostics < boot);
    }

    #[test]
    fn disable_prevents_a_later_status_from_republishing_the_deleted_snapshot() {
        let _serial = crate::testlock::serial();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                DISABLED.store(false, Ordering::Release);
            }
        }
        DISABLED.store(false, Ordering::Release);
        let _restore = Restore;
        let root = Root::new();
        let mut snapshot = fixture();
        let mut published = None;
        publish_if_enabled(&mut snapshot, &root.0, &mut published);
        assert!(root.0.join(NAME).exists());

        disable();
        std::fs::remove_file(root.0.join(NAME)).unwrap();
        snapshot.helper.seen = true;
        snapshot.helper.observe(None);
        publish_if_enabled(&mut snapshot, &root.0, &mut published);
        assert!(!root.0.join(NAME).exists());
        assert!(
            mailbox().is_none(),
            "disabled diagnostics must not restart a worker"
        );
    }

    #[test]
    fn disable_does_not_wait_for_an_in_progress_publication() {
        let _serial = crate::testlock::serial();
        DISABLED.store(false, Ordering::Release);
        let root = Root::new();
        let local = Arc::new(Mailbox {
            pending: Mutex::new(Pending::default()),
            wake: Condvar::new(),
        });
        let previous = WORKER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(local.clone());
        let (entered, publication_entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let publication_root = root.0.clone();
        let publisher = std::thread::spawn(move || {
            let _pending = local.pending.lock().unwrap_or_else(|e| e.into_inner());
            publish_with(&publication_root, b"schema=1\n", || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok(())
            })
        });
        publication_entered.recv().unwrap();
        let (result, returned) = std::sync::mpsc::channel();
        let caller = std::thread::spawn(move || {
            disable();
            let _ = result.send(());
        });
        let prompt = returned.recv_timeout(std::time::Duration::from_millis(100));
        release.send(()).unwrap();
        assert_eq!(
            publisher.join().unwrap().unwrap_err().raw_os_error(),
            Some(libc::ECANCELED)
        );
        caller.join().unwrap();
        *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = previous;
        DISABLED.store(false, Ordering::Release);
        assert!(prompt.is_ok(), "disable waited for the publisher's queue lock");
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }
}
