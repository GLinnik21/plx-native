//! The small, bounded persistence backend used by the session and consent owners.
//!
//! This module deliberately knows nothing about either owner's JSON schema. A [`Record`]'s
//! payload is an opaque UTF-8 string: in particular, a secure session envelope is never parsed,
//! normalised, or re-serialised here.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const FORMAT: &str = "plxnative-record";
const VERSION: u64 = 1;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The two records currently owned by the application.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKey {
    Session,
    Consent,
}

impl RecordKey {
    fn wire(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Consent => "consent",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Session => "session.json",
            Self::Consent => "consent.json",
        }
    }
}

impl fmt::Debug for RecordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Session => "Session",
            Self::Consent => "Consent",
        })
    }
}

/// The opaque state carried by a record.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum RecordState {
    Data { payload: String },
    Cleared,
}

impl fmt::Debug for RecordState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Never put a token-bearing payload in a debug line or panic message.
            Self::Data { .. } => f.write_str("Data { payload: <redacted> }")?,
            Self::Cleared => f.write_str("Cleared")?,
        }
        Ok(())
    }
}

/// A versioned value without any domain-specific deserialization.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) revision: u64,
    pub(crate) state: RecordState,
}

impl Record {
    pub(crate) fn data(revision: u64, payload: String) -> Self {
        Self {
            revision,
            state: RecordState::Data { payload },
        }
    }

    pub(crate) fn cleared(revision: u64) -> Self {
        Self {
            revision,
            state: RecordState::Cleared,
        }
    }
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("revision", &self.revision)
            .field("state", &self.state)
            .finish()
    }
}

/// A commit's post-rename outcome. `Uncertain` means the new name is visible, but a power loss
/// could still lose it (or the post-rename readback did not prove it). It is never reported for a
/// failure before `rename(2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitReceipt {
    Durable,
    Uncertain { stage: CommitStage, errno: i32 },
}

/// The operation at which an I/O outcome occurred. These names intentionally carry no path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitStage {
    CreateTemp,
    Write,
    FileSync,
    Rename,
    ParentOpen,
    ParentSync,
    Readback,
    Cleanup,
}

/// A persistence failure that is safe to print: it has no path and never contains a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoreError {
    Io { stage: CommitStage, errno: i32 },
    RootNotDirectory,
    RootWrongOwner,
    RootUnsafeMode,
    DestinationNotRegular,
    DestinationWrongOwner,
    DestinationUnsafeMode,
    RecordNotRegular,
    RecordWrongOwner,
    RecordUnsafeMode,
    TooLarge,
    InvalidUtf8,
    InvalidSchema,
    UnknownFormat,
    UnsupportedVersion,
    DomainKeyMismatch,
    RootChanged,
}

/// A bounded read of an owned legacy file. `trusted` is false when group/other write bits were
/// present on the same fd used for the read; the caller must never parse those bytes as consent or
/// credentials unless it first installs a durable revocation barrier.
pub(crate) fn read_owned_bytes(path: &Path) -> Result<Option<(Vec<u8>, bool)>, StoreError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(CommitStage::Readback, error)),
    };
    let meta = file
        .metadata()
        .map_err(|error| io_error(CommitStage::Readback, error))?;
    if !meta.is_file() {
        return Err(StoreError::RecordNotRegular);
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(StoreError::RecordWrongOwner);
    }
    let trusted = meta.mode() & 0o022 == 0;
    if trusted && meta.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error(CommitStage::Readback, error))?;
    }
    if meta.len() > MAX_BYTES {
        return Err(StoreError::TooLarge);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(CommitStage::Readback, error))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(StoreError::TooLarge);
    }
    Ok(Some((bytes, trusted)))
}

/// The backend contract. There is intentionally no blanket implementation: callers hold the
/// single-writer contract for each `JsonStore` instance.
pub(crate) trait RecordStore {
    fn load(&self, key: RecordKey) -> Result<Option<Record>, StoreError>;
    fn commit(&self, key: RecordKey, record: &Record) -> Result<CommitReceipt, StoreError>;
    fn cleanup(&self, key: RecordKey) -> Result<(), StoreError>;
}

pub(crate) fn open(root: PathBuf) -> Result<impl RecordStore, StoreError> {
    JsonStore::new(root)
}

#[derive(Clone)]
pub(crate) struct JsonStore {
    root: PathBuf,
    directory: std::sync::Arc<File>,
    #[cfg(test)]
    injected: std::sync::Arc<std::sync::Mutex<Option<CommitStage>>>,
}

/// Cross-store test injection for adapters which open a short-lived store per operation. This is
/// deliberately consumed only at the requested commit stage, so a preceding `load()` cannot steal
/// a failure intended for the following commit.
#[cfg(test)]
static NEXT_COMMIT_FAILURE: std::sync::Mutex<Option<CommitStage>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn inject_next_commit_failure_for_test(stage: CommitStage) {
    *NEXT_COMMIT_FAILURE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(stage);
}

impl JsonStore {
    /// Open an already-selected state root. The root is never created or searched for here.
    pub(crate) fn new(root: PathBuf) -> Result<Self, StoreError> {
        let directory = std::sync::Arc::new(validate_root(&root)?);
        Ok(Self {
            root,
            directory,
            #[cfg(test)]
            injected: std::sync::Arc::new(std::sync::Mutex::new(None)),
        })
    }

    fn check_root(&self) -> Result<(), StoreError> {
        let named = std::fs::symlink_metadata(&self.root)
            .map_err(|e| io_error(CommitStage::ParentOpen, e))?;
        let held = self
            .directory
            .metadata()
            .map_err(|e| io_error(CommitStage::ParentOpen, e))?;
        if !named.is_dir() || named.dev() != held.dev() || named.ino() != held.ino() {
            return Err(StoreError::RootChanged);
        }
        validate_root_metadata(&held)
    }

    #[cfg(test)]
    pub(crate) fn inject_failure(&self, stage: CommitStage) {
        *self.injected.lock().unwrap_or_else(|e| e.into_inner()) = Some(stage);
    }

    #[cfg(test)]
    fn take_failure(&self, stage: CommitStage) -> Option<i32> {
        let mut failure = self.injected.lock().unwrap_or_else(|e| e.into_inner());
        if *failure == Some(stage) {
            *failure = None;
            Some(libc::EIO)
        } else {
            let mut next = NEXT_COMMIT_FAILURE
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *next == Some(stage) {
                *next = None;
                Some(libc::EIO)
            } else {
                None
            }
        }
    }

    #[cfg(not(test))]
    fn take_failure(&self, _stage: CommitStage) -> Option<i32> {
        None
    }

    /// Remove only abandoned temporary files for `key`, using the held directory descriptor.
    /// Callers serialize this with commits for the same key. Unrelated names, symlinks, foreign
    /// owners, and widened-mode files are left untouched.
    pub(crate) fn cleanup_temporaries(&self, key: RecordKey) -> Result<(), StoreError> {
        self.check_root()?;
        let prefix = format!(".{}.new-", key.file_name());
        let names =
            directory_names(&self.directory).map_err(|e| io_error(CommitStage::Cleanup, e))?;
        let mut removed = false;
        for name in names {
            if !safe_temp_name(&name, &prefix) {
                continue;
            }
            let Ok(file) = open_at(&self.directory, &name, libc::O_RDONLY, 0) else {
                continue;
            };
            let meta = match file.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if !meta.is_file()
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0
            {
                continue;
            }
            if unlink_at(&self.directory, &name).is_ok() {
                removed = true;
            }
        }
        if removed {
            self.directory
                .sync_all()
                .map_err(|e| io_error(CommitStage::Cleanup, e))?;
        }
        self.check_root()
    }
}

impl RecordStore for JsonStore {
    fn load(&self, key: RecordKey) -> Result<Option<Record>, StoreError> {
        self.check_root()?;
        let bytes = match read_owned_regular(&self.directory, key.file_name()) {
            Ok(bytes) => bytes,
            Err(ReadFailure::Missing) => {
                // An absent child is still a result from the selected root. Do not authorize
                // migration/import based on it if the named root was swapped away meanwhile.
                self.check_root()?;
                return Ok(None);
            }
            Err(ReadFailure::Error(e)) => return Err(e),
        };
        let parsed = parse_record(&bytes, key).map(Some);
        // The descriptor protects all child operations, but the selected pathname can still be
        // renamed away while we read. Do not publish a record after that root identity changed.
        if parsed.is_ok() {
            self.check_root()?;
        }
        parsed
    }

    fn commit(&self, key: RecordKey, record: &Record) -> Result<CommitReceipt, StoreError> {
        self.check_root()?;
        if matches!(&record.state, RecordState::Data { payload } if payload.len() as u64 > MAX_BYTES)
        {
            return Err(StoreError::TooLarge);
        }
        let bytes = encode_record(key, record)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(StoreError::TooLarge);
        }
        let destination = key.file_name();
        let replacing_cleared = matches!(&record.state, RecordState::Cleared);
        validate_destination(&self.directory, destination, replacing_cleared)?;
        let (tmp, mut file) = create_temp(&self.directory, destination, |stage| {
            self.take_failure(stage)
        })?;

        if let Some(errno) = self.take_failure(CommitStage::Write) {
            remove_at(&self.directory, &tmp);
            return Err(StoreError::Io {
                stage: CommitStage::Write,
                errno,
            });
        }
        if let Err(e) = file.write_all(&bytes) {
            remove_at(&self.directory, &tmp);
            return Err(io_error(CommitStage::Write, e));
        }
        if let Some(errno) = self.take_failure(CommitStage::FileSync) {
            remove_at(&self.directory, &tmp);
            return Err(StoreError::Io {
                stage: CommitStage::FileSync,
                errno,
            });
        }
        if let Err(e) = file.sync_all() {
            remove_at(&self.directory, &tmp);
            return Err(io_error(CommitStage::FileSync, e));
        }
        drop(file);

        if let Some(errno) = self.take_failure(CommitStage::Rename) {
            remove_at(&self.directory, &tmp);
            return Err(StoreError::Io {
                stage: CommitStage::Rename,
                errno,
            });
        }
        if let Err(e) = rename_at(&self.directory, &tmp, destination) {
            remove_at(&self.directory, &tmp);
            return Err(io_error(CommitStage::Rename, e));
        }

        // Everything from this point follows a successful rename and is therefore uncertain on
        // failure. Still attempt every post-rename step so the readback can prove the visible name.
        let mut uncertainty = None;
        if let Some(errno) = self.take_failure(CommitStage::ParentOpen) {
            uncertainty = Some((CommitStage::ParentOpen, errno));
        } else {
            match self.check_root() {
                Ok(()) => {
                    if let Some(errno) = self.take_failure(CommitStage::ParentSync) {
                        uncertainty = Some((CommitStage::ParentSync, errno));
                    } else if let Err(e) = self.directory.sync_all() {
                        uncertainty =
                            Some((CommitStage::ParentSync, e.raw_os_error().unwrap_or(0)));
                    }
                }
                Err(_) => uncertainty = Some((CommitStage::ParentOpen, libc::EIO)),
            }
        }

        if let Some(errno) = self.take_failure(CommitStage::Readback) {
            return Ok(CommitReceipt::Uncertain {
                stage: CommitStage::Readback,
                errno,
            });
        }
        match self.load(key) {
            Ok(Some(found)) if found == *record => {}
            Ok(_) | Err(_) => {
                uncertainty = Some((CommitStage::Readback, libc::EIO));
            }
        }
        Ok(match uncertainty {
            Some((stage, errno)) => CommitReceipt::Uncertain { stage, errno },
            None => CommitReceipt::Durable,
        })
    }

    fn cleanup(&self, key: RecordKey) -> Result<(), StoreError> {
        self.cleanup_temporaries(key)
    }
}

enum ReadFailure {
    Missing,
    Error(StoreError),
}

fn validate_root(root: &Path) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(root)
        .map_err(|e| io_error(CommitStage::ParentOpen, e))?;
    validate_root_metadata(
        &file
            .metadata()
            .map_err(|e| io_error(CommitStage::ParentOpen, e))?,
    )?;
    Ok(file)
}

fn validate_root_metadata(meta: &std::fs::Metadata) -> Result<(), StoreError> {
    if !meta.is_dir() {
        return Err(StoreError::RootNotDirectory);
    }
    let current_uid = unsafe { libc::geteuid() };
    if meta.uid() != current_uid && meta.uid() != 0 {
        return Err(StoreError::RootWrongOwner);
    }
    if !trusted_root(meta.uid(), meta.gid(), meta.mode(), current_uid) {
        return Err(StoreError::RootUnsafeMode);
    }
    Ok(())
}

fn trusted_root(uid: u32, gid: u32, mode: u32, current_uid: u32) -> bool {
    // IPK installs state/ as root:5000 0775. File contents still require this app's uid/0600.
    // The shared Developer Mode group may rename names; this does not promise anti-replay.
    (uid == current_uid || uid == 0) && mode & 0o002 == 0 && (mode & 0o020 == 0 || gid == 5000)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoveDisposition {
    Removed,
    Absent,
}

fn classify_remove_error(
    error: io::Error,
    absence_probe: io::Result<()>,
) -> io::Result<RemoveDisposition> {
    match absence_probe {
        // Some read-only filesystems reject unlink from the parent before looking up the child.
        // Only a no-follow metadata lookup that independently observes no directory entry turns
        // that failure into successful cleanup. A present symlink is present; an inaccessible or
        // otherwise unverifiable name retains the original unlink error.
        Err(probe_error) if probe_error.kind() == io::ErrorKind::NotFound => {
            Ok(RemoveDisposition::Absent)
        }
        Ok(()) | Err(_) => Err(error),
    }
}

pub(crate) fn remove_file_or_prove_absent(path: &Path) -> io::Result<RemoveDisposition> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(RemoveDisposition::Removed),
        Err(error) => classify_remove_error(
            error,
            std::fs::symlink_metadata(path).map(|_| ()),
        ),
    }
}

// All child names below are generated internally, with no path separators. Keep the already-open
// directory descriptor across every operation: swapping the pathname cannot redirect our I/O.
fn open_at(directory: &File, name: &str, flags: i32, mode: libc::mode_t) -> io::Result<File> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid record name"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn remove_at(directory: &File, name: &str) {
    if let Ok(name) = std::ffi::CString::new(name) {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0);
        }
    }
}

fn unlink_at(directory: &File, name: &str) -> io::Result<()> {
    let name = std::ffi::CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary name"))?;
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

struct Directory(*mut libc::DIR);

impl Drop for Directory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::closedir(self.0) };
        }
    }
}

fn directory_names(directory: &File) -> io::Result<Vec<String>> {
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let dir = unsafe { libc::fdopendir(duplicate) };
    if dir.is_null() {
        unsafe { libc::close(duplicate) };
        return Err(io::Error::last_os_error());
    }
    let dir = Directory(dir);
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        if let Ok(name) = name.to_str() {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn safe_temp_name(name: &str, prefix: &str) -> bool {
    let Some(suffix) = name.strip_prefix(prefix) else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|b| b.is_ascii_hexdigit())
}

fn rename_at(directory: &File, from: &str, to: &str) -> io::Result<()> {
    let from = std::ffi::CString::new(from).expect("internally generated name");
    let to = std::ffi::CString::new(to).expect("fixed record name");
    let result = unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn validate_destination(
    directory: &File,
    name: &str,
    replacing_cleared: bool,
) -> Result<(), StoreError> {
    let file = match open_at(directory, name, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(CommitStage::CreateTemp, e)),
    };
    let meta = file
        .metadata()
        .map_err(|e| io_error(CommitStage::CreateTemp, e))?;
    if !meta.is_file() {
        return Err(StoreError::DestinationNotRegular);
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(StoreError::DestinationWrongOwner);
    }
    // A clear is a revocation: replacing an owned writable file is safe because the new bytes are
    // created privately and atomically. A data commit must not overwrite bytes another uid could
    // have changed since our last write.
    if meta.mode() & 0o022 != 0 && !replacing_cleared {
        return Err(StoreError::DestinationUnsafeMode);
    }
    Ok(())
}

fn read_owned_regular(directory: &File, name: &str) -> Result<Vec<u8>, ReadFailure> {
    let file = open_at(directory, name, libc::O_RDONLY, 0).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            ReadFailure::Missing
        } else {
            ReadFailure::Error(io_error(CommitStage::Readback, e))
        }
    })?;
    let meta = file
        .metadata()
        .map_err(|e| ReadFailure::Error(io_error(CommitStage::Readback, e)))?;
    if !meta.is_file() {
        return Err(ReadFailure::Error(StoreError::RecordNotRegular));
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(ReadFailure::Error(StoreError::RecordWrongOwner));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(ReadFailure::Error(StoreError::RecordUnsafeMode));
    }
    // Read-only widening does not forge contents, but must be repaired before returning secrets.
    if meta.mode() & 0o077 != 0 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| ReadFailure::Error(io_error(CommitStage::Readback, e)))?;
    }
    if meta.len() > MAX_BYTES {
        return Err(ReadFailure::Error(StoreError::TooLarge));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ReadFailure::Error(io_error(CommitStage::Readback, e)))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ReadFailure::Error(StoreError::TooLarge));
    }
    Ok(bytes)
}

fn create_temp(
    directory: &File,
    destination: &str,
    mut injected: impl FnMut(CommitStage) -> Option<i32>,
) -> Result<(String, File), StoreError> {
    if let Some(errno) = injected(CommitStage::CreateTemp) {
        return Err(StoreError::Io {
            stage: CommitStage::CreateTemp,
            errno,
        });
    }
    for _ in 0..16 {
        let mut random = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|e| io_error(CommitStage::CreateTemp, e))?;
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!(".{destination}.new-{suffix}");
        match open_at(
            directory,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        ) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(CommitStage::CreateTemp, e)),
        }
    }
    Err(StoreError::Io {
        stage: CommitStage::CreateTemp,
        errno: libc::EEXIST,
    })
}

fn io_error(stage: CommitStage, error: io::Error) -> StoreError {
    StoreError::Io {
        stage,
        errno: error.raw_os_error().unwrap_or(0),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    format: String,
    version: u64,
    domain: String,
    key: String,
    revision: u64,
    state: WireState,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum WireState {
    Data { payload: String },
    Cleared,
}

fn encode_record(key: RecordKey, record: &Record) -> Result<Vec<u8>, StoreError> {
    let state = match &record.state {
        RecordState::Data { payload } => WireState::Data {
            payload: payload.clone(),
        },
        RecordState::Cleared => WireState::Cleared,
    };
    serde_json::to_vec(&WireRecord {
        format: FORMAT.to_owned(),
        version: VERSION,
        domain: key.wire().to_owned(),
        key: key.wire().to_owned(),
        revision: record.revision,
        state,
    })
    .map_err(|_| StoreError::InvalidSchema)
}

fn parse_record(bytes: &[u8], expected: RecordKey) -> Result<Record, StoreError> {
    std::str::from_utf8(bytes).map_err(|_| StoreError::InvalidUtf8)?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| StoreError::InvalidSchema)?;
    match value.get("format") {
        Some(serde_json::Value::String(s)) if s == FORMAT => {}
        _ => return Err(StoreError::UnknownFormat),
    }
    // Inspect the version before deserializing the current schema. A newer writer may have added
    // fields denied by `deny_unknown_fields`; it is still a future-version record, not malformed
    // data from this version.
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(version) if version != VERSION => return Err(StoreError::UnsupportedVersion),
        Some(_) => {}
        None => return Err(StoreError::InvalidSchema),
    }
    let wire: WireRecord = serde_json::from_value(value).map_err(|_| StoreError::InvalidSchema)?;
    if wire.version != VERSION {
        return Err(StoreError::UnsupportedVersion);
    }
    if wire.domain != expected.wire() || wire.key != expected.wire() || wire.domain != wire.key {
        return Err(StoreError::DomainKeyMismatch);
    }
    let state = match wire.state {
        WireState::Data { payload } => RecordState::Data { payload },
        WireState::Cleared => RecordState::Cleared,
    };
    Ok(Record {
        revision: wire.revision,
        state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_unlink_is_complete_only_when_nofollow_metadata_proves_absence() {
        let readonly_parent = io::Error::from_raw_os_error(libc::EROFS);
        let absent = Err(io::Error::from(io::ErrorKind::NotFound));
        assert!(matches!(
            classify_remove_error(readonly_parent, absent),
            Ok(RemoveDisposition::Absent)
        ));

        for probe in [Ok(()), Err(io::Error::from_raw_os_error(libc::EACCES))] {
            let error = classify_remove_error(io::Error::from_raw_os_error(libc::EROFS), probe)
                .expect_err("present or unverifiable target must retain unlink failure");
            assert_eq!(error.raw_os_error(), Some(libc::EROFS));
        }
    }

    #[test]
    fn ordinary_missing_file_is_reported_absent_by_the_production_helper() {
        let path = std::env::temp_dir().join(format!(
            "plxnative-cleanup-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            remove_file_or_prove_absent(&path).unwrap(),
            RemoveDisposition::Absent
        );
    }
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "plxnative-storage-{}-{}",
                std::process::id(),
                rand_suffix()
            ));
            std::fs::create_dir(&p).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(1);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn packaged_root_group_writable_state_is_supported() {
        assert!(trusted_root(0, 5000, 0o775, 6586));
        assert!(!trusted_root(0, 5000, 0o777, 6586));
        assert!(!trusted_root(0, 42, 0o775, 6586));
        assert!(!trusted_root(1234, 5000, 0o775, 6586));
    }

    #[test]
    fn replacing_the_opened_root_with_a_symlink_cannot_redirect_a_write() {
        let d = Scratch::new();
        let root = d.0.join("state");
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        let store = JsonStore::new(root.clone()).unwrap();
        std::fs::rename(&root, d.0.join("old-state")).unwrap();
        symlink(&elsewhere, &root).unwrap();
        assert!(store
            .commit(RecordKey::Session, &Record::cleared(1))
            .is_err());
        assert!(!elsewhere.join("session.json").exists());
    }

    #[test]
    fn replacing_the_opened_root_cannot_turn_missing_into_an_authorized_load() {
        let d = Scratch::new();
        let root = d.0.join("state");
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        let store = JsonStore::new(root.clone()).unwrap();
        std::fs::rename(&root, d.0.join("old-state")).unwrap();
        symlink(&elsewhere, &root).unwrap();
        assert_eq!(store.load(RecordKey::Session), Err(StoreError::RootChanged));
    }

    #[test]
    fn exact_payload_and_cleared_roundtrip() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        let payload = r#" {"sealed":"\u00e9", "bytes":[1,2]} "#.to_owned();
        let record = Record::data(7, payload);
        assert_eq!(
            s.commit(RecordKey::Session, &record),
            Ok(CommitReceipt::Durable)
        );
        assert_eq!(s.load(RecordKey::Session).unwrap(), Some(record));
        let cleared = Record::cleared(8);
        assert_eq!(
            s.commit(RecordKey::Consent, &cleared),
            Ok(CommitReceipt::Durable)
        );
        assert_eq!(s.load(RecordKey::Consent).unwrap(), Some(cleared));
    }

    #[test]
    fn schema_and_domain_failures_are_distinct() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        std::fs::write(d.0.join("session.json"), br#"{"format":"other"}"#).unwrap();
        assert_eq!(s.load(RecordKey::Session), Err(StoreError::UnknownFormat));
        std::fs::write(d.0.join("session.json"), br#"{"format":"plxnative-record","version":1,"domain":"consent","key":"session","revision":1,"state":"Cleared"}"#).unwrap();
        assert_eq!(
            s.load(RecordKey::Session),
            Err(StoreError::DomainKeyMismatch)
        );
    }

    #[test]
    fn symlink_and_fifo_are_rejected_without_blocking() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        let target = d.0.join("target");
        std::fs::write(&target, b"not ours").unwrap();
        symlink(&target, d.0.join("session.json")).unwrap();
        assert!(matches!(
            s.load(RecordKey::Session),
            Err(StoreError::Io { .. })
        ));
        let _ = std::fs::remove_file(d.0.join("session.json"));
        let c = std::ffi::CString::new(d.0.join("session.json").as_os_str().as_encoded_bytes())
            .unwrap();
        unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
        assert_eq!(
            s.load(RecordKey::Session),
            Err(StoreError::RecordNotRegular)
        );
    }

    #[test]
    fn writable_data_and_size_bound_are_rejected() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        let p = d.0.join("session.json");
        std::fs::write(&p, b"{}").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o622)).unwrap();
        assert_eq!(
            s.load(RecordKey::Session),
            Err(StoreError::RecordUnsafeMode)
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let f = File::create(&p).unwrap();
        f.set_len(MAX_BYTES + 1).unwrap();
        assert_eq!(s.load(RecordKey::Session), Err(StoreError::TooLarge));
    }

    #[test]
    fn clear_replaces_owned_writable_destination_to_revoke_it() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        let p = d.0.join("session.json");
        let old = Record::data(1, "secret-placeholder".into());
        assert_eq!(
            s.commit(RecordKey::Session, &old),
            Ok(CommitReceipt::Durable)
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o622)).unwrap();
        let cleared = Record::cleared(2);
        assert_eq!(
            s.commit(RecordKey::Session, &cleared),
            Ok(CommitReceipt::Durable)
        );
        assert_eq!(s.load(RecordKey::Session).unwrap(), Some(cleared));
    }

    #[test]
    fn pre_and_post_rename_failures_have_different_shapes() {
        let d = Scratch::new();
        let s = JsonStore::new(d.0.clone()).unwrap();
        let r = Record::data(1, "{}".to_owned());
        s.inject_failure(CommitStage::Write);
        assert!(matches!(
            s.commit(RecordKey::Session, &r),
            Err(StoreError::Io {
                stage: CommitStage::Write,
                ..
            })
        ));
        assert!(s.load(RecordKey::Session).unwrap().is_none());
        s.inject_failure(CommitStage::Readback);
        assert_eq!(
            s.commit(RecordKey::Session, &r),
            Ok(CommitReceipt::Uncertain {
                stage: CommitStage::Readback,
                errno: libc::EIO
            })
        );
        assert_eq!(s.load(RecordKey::Session).unwrap(), Some(r));
    }

    #[test]
    fn every_commit_failure_point_preserves_a_whole_record_across_reopen() {
        for stage in [
            CommitStage::CreateTemp,
            CommitStage::Write,
            CommitStage::FileSync,
            CommitStage::Rename,
            CommitStage::ParentOpen,
            CommitStage::ParentSync,
            CommitStage::Readback,
        ] {
            let d = Scratch::new();
            let store = JsonStore::new(d.0.clone()).unwrap();
            let old = Record::data(11, "old exact payload".into());
            let next = Record::cleared(12);
            assert_eq!(
                store.commit(RecordKey::Session, &old),
                Ok(CommitReceipt::Durable)
            );
            store.inject_failure(stage);
            let result = store.commit(RecordKey::Session, &next);
            let before_rename = matches!(
                stage,
                CommitStage::CreateTemp
                    | CommitStage::Write
                    | CommitStage::FileSync
                    | CommitStage::Rename
            );
            if before_rename {
                assert_eq!(
                    result,
                    Err(StoreError::Io {
                        stage,
                        errno: libc::EIO
                    })
                );
            } else {
                assert_eq!(
                    result,
                    Ok(CommitReceipt::Uncertain {
                        stage,
                        errno: libc::EIO
                    })
                );
            }
            drop(store);
            let reopened = JsonStore::new(d.0.clone()).unwrap();
            assert_eq!(
                reopened.load(RecordKey::Session).unwrap(),
                Some(if before_rename { old } else { next })
            );
            assert_eq!(
                std::fs::read_dir(&d.0).unwrap().count(),
                1,
                "temporary file survived {stage:?}"
            );
        }
    }

    #[test]
    fn committed_files_are_private_and_read_only_widening_is_repaired() {
        let d = Scratch::new();
        let store = JsonStore::new(d.0.clone()).unwrap();
        let record = Record::data(1, "secret-placeholder".into());
        store.commit(RecordKey::Session, &record).unwrap();
        let path = d.0.join("session.json");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            store.load(RecordKey::Session).unwrap(),
            Some(record.clone())
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!format!("{record:?}").contains("secret-placeholder"));
    }

    #[test]
    fn cleanup_removes_only_abandoned_owned_temporaries() {
        let d = Scratch::new();
        let store = JsonStore::new(d.0.clone()).unwrap();
        let abandoned =
            d.0.join(".session.json.new-0123456789abcdef0123456789abcdef");
        let widened =
            d.0.join(".session.json.new-fedcba9876543210fedcba9876543210");
        let unrelated = d.0.join(".session.json.new-not-hex");
        let other_key =
            d.0.join(".consent.json.new-0123456789abcdef0123456789abcdef");
        for path in [&abandoned, &widened, &unrelated, &other_key] {
            std::fs::write(path, b"temporary").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::set_permissions(&widened, std::fs::Permissions::from_mode(0o644)).unwrap();
        store.cleanup_temporaries(RecordKey::Session).unwrap();
        assert!(!abandoned.exists());
        assert!(widened.exists());
        assert!(unrelated.exists());
        assert!(other_key.exists());
    }

    #[test]
    fn a_future_record_version_is_not_a_missing_record() {
        let bytes = br#"{"format":"plxnative-record","version":2,"domain":"session","key":"session","revision":1,"state":"Cleared"}"#;
        assert_eq!(
            parse_record(bytes, RecordKey::Session),
            Err(StoreError::UnsupportedVersion)
        );
        assert_eq!(
            parse_record(&[0xff], RecordKey::Session),
            Err(StoreError::InvalidUtf8)
        );
        let future_with_new_field = br#"{"format":"plxnative-record","version":2,"domain":"session","key":"session","revision":1,"state":"Cleared","new_field":true}"#;
        assert_eq!(
            parse_record(future_with_new_field, RecordKey::Session),
            Err(StoreError::UnsupportedVersion)
        );
    }

    #[test]
    fn missing_root_is_not_created_and_root_symlink_is_rejected() {
        let d = Scratch::new();
        let missing = d.0.join("missing");
        assert!(JsonStore::new(missing.clone()).is_err());
        assert!(!missing.exists());
        let link = d.0.join("link");
        symlink(&d.0, &link).unwrap();
        assert!(JsonStore::new(link).is_err());
    }
}
