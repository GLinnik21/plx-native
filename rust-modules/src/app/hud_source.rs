//! Bounded HUD artifact reads and authoring snapshots. This is the IO shell around the pure bundle
//! module. Source files are opened component by component beneath an already-open directory.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use plx_appkit::hud_format::bundle::MAX_ARTIFACT_BYTES;

#[derive(Debug)]
pub(crate) enum ReadError {
    Io(io::Error),
    TooLarge,
    NotRegular,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::TooLarge => formatter.write_str("input exceeds its byte limit"),
            Self::NotRegular => formatter.write_str("input must be a regular file"),
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> Self { Self::Io(error) }
}

fn read_file(file: File, limit: usize) -> Result<Vec<u8>, ReadError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() { return Err(ReadError::NotRegular); }
    if metadata.len() > limit as u64 { return Err(ReadError::TooLarge); }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit { return Err(ReadError::TooLarge); }
    Ok(bytes)
}

pub(crate) fn read_artifact(path: &Path) -> Result<Vec<u8>, ReadError> {
    let file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    read_file(file, MAX_ARTIFACT_BYTES)
}

#[cfg(any(test, feature = "hostsim"))]
mod authoring {
    use super::*;
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use plx_appkit::hud_format::{bundle::{self, archive, Decoded}, codes, Finding};
    use plx_ui::rec::strict_json::Budget;

    #[derive(Clone, Debug)]
    pub(crate) enum Source {
        Archive(Vec<u8>),
        Directory(Vec<archive::File>),
        Rejected(Decoded),
    }

    fn rejected(manifest: Option<bundle::Manifest>, member: Option<&str>, message: String) -> Source {
        let mut finding = Finding::new(&codes::E1602, "", message, "provide the declared regular files within the package profile");
        finding.file = member.map(str::to_owned);
        Source::Rejected(Decoded::source_error(manifest, vec![finding]))
    }

    fn read_at(root: &File, path: &str, limit: usize) -> Result<Vec<u8>, ReadError> {
        let mut directory = root.try_clone()?;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            let name = CString::new(part).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid source component"))?;
            let is_directory = parts.peek().is_some();
            let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
                | if is_directory { libc::O_DIRECTORY } else { libc::O_NONBLOCK };
            // The directory descriptor anchors every lookup. O_NOFOLLOW applies to every component.
            let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
            if descriptor < 0 { return Err(io::Error::last_os_error().into()); }
            // A successful openat returns a fresh owned descriptor.
            let opened = unsafe { File::from_raw_fd(descriptor) };
            if !is_directory { return read_file(opened, limit); }
            directory = opened;
        }
        Err(ReadError::NotRegular)
    }

    /// Snapshot exactly the declared files. A semantic source failure becomes a structured report
    /// with a null digest because there is no complete artifact yet.
    pub(crate) fn read_source(path: &str) -> Result<Source, String> {
        let path = Path::new(path);
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            return match read_artifact(path) {
                Ok(bytes) => Ok(Source::Archive(bytes)),
                Err(ReadError::TooLarge) => Ok(rejected(None, None, "artifact exceeds its byte limit".to_owned())),
                Err(error) => Err(error.to_string()),
            };
        }
        let root = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(path).map_err(|error| error.to_string())?;
        let bytes = match read_at(&root, bundle::manifest::FILE, bundle::MAX_MANIFEST_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => return Ok(rejected(None, Some(bundle::manifest::FILE), error.to_string())),
        };
        let manifest = match bundle::manifest::read(&bytes, &Budget::new(bundle::JSON_DECODE_BYTES)) {
            Ok(manifest) => manifest,
            Err(findings) => return Ok(Source::Rejected(Decoded::source_error(None, findings))),
        };
        let mut total = bytes.len();
        let mut files = vec![archive::File::new(bundle::manifest::FILE, bytes)];
        for member in manifest.files().skip(1) {
            let remaining = bundle::MAX_MEMBER_BYTES.saturating_sub(total);
            let limit = if member == manifest.entry {
                remaining.min(plx_appkit::hud_format::parse::MAX_HUD_JSON_BYTES)
            } else if manifest.resources.iter().any(|(_, path)| path == member) {
                remaining.min(plx_appkit::hud_format::svg_subset::MAX_ASSET_BYTES)
            } else { remaining };
            let bytes = match read_at(&root, member, limit) {
                Ok(bytes) => bytes,
                Err(error) => return Ok(rejected(Some(manifest.clone()), Some(member), error.to_string())),
            };
            total += bytes.len();
            files.push(archive::File::new(member, bytes));
        }
        Ok(Source::Directory(files))
    }

    /// Publish complete bytes beside their destination and rename them into place. A temporary
    /// filename already present is refused, so an output directory cannot supply a symlink target.
    pub(crate) fn write_artifact(path: &str, bytes: &[u8]) -> Result<(), String> {
        let path = Path::new(path);
        let mut temporary = path.as_os_str().to_os_string();
        temporary.push(format!(".tmp-{}", std::process::id()));
        let temporary = std::path::PathBuf::from(temporary);
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(&temporary).map_err(|error| error.to_string())?;
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            File::open(path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new(".")))?.sync_all()
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temporary); }
        result.map_err(|error: io::Error| error.to_string())
    }
}

#[cfg(any(test, feature = "hostsim"))]
pub(crate) use authoring::{read_source, write_artifact, Source};

#[cfg(test)]
mod tests {
    use super::*;
    use plx_appkit::hud_format::{bundle, fixtures};
    use std::os::unix::fs::symlink;

    fn source(name: &str, label: &str) -> std::path::PathBuf {
        let root = plx_base::testscratch::process_dir(label);
        for file in fixtures::files(name) {
            let path = root.join(file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, file.bytes).unwrap();
        }
        root
    }

    #[test]
    fn a_directory_snapshot_packs_the_exact_bytes_it_already_read() {
        let root = source("minimal", "hud-source-snapshot");
        let Source::Directory(files) = read_source(root.to_str().unwrap()).unwrap() else { panic!("source snapshot") };
        std::fs::write(root.join("hud.json"), b"{}").unwrap();
        let (bytes, decoded) = bundle::pack(&files).unwrap();
        assert!(decoded.package.is_some());
        let output = root.join("test.plxhud");
        write_artifact(output.to_str().unwrap(), &bytes).unwrap();
        assert_eq!(read_artifact(&output).unwrap(), bytes);
        assert_eq!(bundle::read(&read_artifact(&output).unwrap(), None).sha256, decoded.sha256);
    }

    #[test]
    fn source_symlink_leaves_and_ancestors_are_refused() {
        for (label, ancestor) in [("hud-source-symlink-leaf", false), ("hud-source-symlink-parent", true)] {
            let root = source("reference", label);
            let member = if ancestor { "assets" } else { "assets/eq.svg" };
            let original = root.join(member);
            let target = root.join("outside");
            std::fs::rename(&original, &target).unwrap();
            symlink(&target, &original).unwrap();
            let Source::Rejected(decoded) = read_source(root.to_str().unwrap()).unwrap() else { panic!("symlink was followed") };
            assert!(decoded.package.is_none() && decoded.sha256.is_none());
        }
    }

    #[test]
    fn oversized_transport_never_reports_a_prefix_digest() {
        let root = plx_base::testscratch::process_dir("hud-source-oversized");
        let path = root.join("huge.plxhud");
        File::create(&path).unwrap().set_len(MAX_ARTIFACT_BYTES as u64 + 1).unwrap();
        let Source::Rejected(decoded) = read_source(path.to_str().unwrap()).unwrap() else { panic!("oversized file") };
        assert!(decoded.sha256.is_none());
    }

    #[test]
    fn an_existing_temporary_symlink_cannot_redirect_output() {
        let root = plx_base::testscratch::process_dir("hud-source-atomic-output");
        let output = root.join("hud.plxhud");
        let protected = root.join("protected");
        std::fs::write(&output, b"previous").unwrap();
        std::fs::write(&protected, b"kept").unwrap();
        let temporary = root.join(format!("hud.plxhud.tmp-{}", std::process::id()));
        symlink(&protected, &temporary).unwrap();
        assert!(write_artifact(output.to_str().unwrap(), b"replacement").is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), b"kept");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous");
    }
}
