//! Private activation correlation, independent of clocks and never included in reports.
use super::failure::Detail;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt}},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 4096;
const ATTEMPT: &str = "activation.json";
const FAILURE: &str = "start-error";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    nonce: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartFailure {
    activation_nonce: String,
    detail: Detail,
}

impl Attempt {
    /// Publish before sending the activation hint. Failure to publish disables correlation.
    pub fn begin(root: &Path) -> Option<Self> {
        let attempt = Self { nonce: random_nonce().ok()? };
        write_json(root, ATTEMPT, &attempt, true).ok()?;
        Some(attempt)
    }

    /// Capture ONCE before LS2 registration, never again when reporting a failure. Otherwise
    /// an older process failing late could echo a newer activation's nonce.
    pub fn capture(root: &Path) -> Option<Self> {
        let attempt: Self = serde_json::from_slice(&read_private(root, ATTEMPT)?).ok()?;
        (attempt.nonce.len() == 32 && attempt.nonce.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(attempt)
    }

    pub fn nonce(&self) -> &str { &self.nonce }

    pub fn record_failure(&self, root: &Path, detail: Detail) {
        let record = StartFailure { activation_nonce: self.nonce.clone(), detail };
        let _ = write_json(root, FAILURE, &record, false);
    }
}

pub fn read_failure(root: &Path, nonce: &str) -> Option<Detail> {
    let record: StartFailure = serde_json::from_slice(&read_private(root, FAILURE)?).ok()?;
    (record.activation_nonce == nonce).then_some(record.detail)
}

fn random_nonce() -> io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn directory(root: &Path, create: bool) -> io::Result<(File, PathBuf)> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
    }
    let dir = OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root)?;
    let meta = dir.metadata()?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o7777 != 0o700 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    #[cfg(target_os = "linux")]
    let anchor = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
    #[cfg(not(target_os = "linux"))]
    let anchor = { let _ = dir.as_raw_fd(); root.to_owned() };
    Ok((dir, anchor))
}

fn regular(meta: &fs::Metadata) -> bool {
    meta.is_file() && meta.uid() == unsafe { libc::getuid() }
        && meta.mode() & 0o7777 == 0o600 && meta.nlink() == 1
}

fn read_private(root: &Path, name: &str) -> Option<Vec<u8>> {
    let (_dir, anchor) = directory(root, false).ok()?;
    let mut file = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(anchor.join(name)).ok()?;
    let meta = file.metadata().ok()?;
    if !regular(&meta) || meta.len() > MAX_BYTES { return None; }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
}

fn write_json(root: &Path, name: &str, value: &impl Serialize, create: bool) -> io::Result<()> {
    let (_dir, anchor) = directory(root, create)?;
    let path = anchor.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !regular(&meta) => return Err(io::ErrorKind::PermissionDenied.into()),
        Ok(_) => (),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > MAX_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
    // Atomic replacement keeps a reader from seeing half of a newly published nonce/record.
    let temp = anchor.join(format!(".activation-{}", random_nonce()?));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&temp)?;
    let result = file.write_all(&bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::failure::Stage;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("plx-activation-{name}-{}", std::process::id()))
    }

    #[test]
    fn captured_attempt_cannot_echo_a_newer_activation() {
        let root = root("capture");
        let first = Attempt::begin(&root).unwrap();
        let helper = Attempt::capture(&root).unwrap();
        let second = Attempt::begin(&root).unwrap();
        assert_ne!(first.nonce(), second.nonce());
        let detail = Detail::new(Stage::BusCancel, Some(-13));
        // The old process fails after the new challenge has already been published.
        helper.record_failure(&root, detail);
        assert_eq!(read_failure(&root, second.nonce()), None);
        assert_eq!(read_failure(&root, first.nonce()), Some(detail));
        let new_helper = Attempt::capture(&root).unwrap();
        new_helper.record_failure(&root, detail);
        assert_eq!(read_failure(&root, second.nonce()), Some(detail));
        assert_eq!(read_failure(&root, first.nonce()), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_files_are_bounded_closed_and_never_follow_symlinks() {
        let root = root("validation");
        let attempt = Attempt::begin(&root).unwrap();
        let detail = Detail::new(Stage::BusRegister, Some(-13));
        attempt.record_failure(&root, detail);
        let path = root.join(FAILURE);
        let good = fs::read(&path).unwrap();
        for bytes in [
            serde_json::to_vec(&serde_json::json!({"activation_nonce":attempt.nonce(),
                "detail":detail,"owner":"private"})).unwrap(),
            serde_json::to_vec(&serde_json::json!({"activation_nonce":attempt.nonce(),
                "detail":{"stage":"bus-register","code":-13,"path":"private"}})).unwrap(),
            { let mut bytes = good.clone(); bytes.resize(4097, b' '); bytes },
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(read_failure(&root, attempt.nonce()), None);
        }
        fs::write(&path, &good).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_failure(&root, attempt.nonce()), None);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let target = root.join("target");
        fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert_eq!(read_failure(&root, attempt.nonce()), None);
        attempt.record_failure(&root, Detail::new(Stage::BusAttach, None));
        assert_eq!(fs::read(&target).unwrap(), good);
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert_eq!(read_failure(&root, attempt.nonce()), None);
        fs::remove_file(&path).unwrap();
        fs::remove_file(root.join(ATTEMPT)).unwrap();
        symlink(&target, root.join(ATTEMPT)).unwrap();
        assert!(Attempt::capture(&root).is_none());
        assert!(Attempt::begin(&root).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
