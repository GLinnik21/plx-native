//! The canonical STORE-only ZIP32 profile. A bounded structural pass runs before the ZIP crate
//! allocates its index. File content and CRC validation then use the maintained ZIP reader.

use std::io::{Cursor, Read, Write};
use std::ops::Range;
use std::sync::Arc;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use super::{
    valid_path, MAX_ARTIFACT_BYTES, MAX_DIRECTORY_BYTES, MAX_ENTRIES, MAX_MEMBER_BYTES,
    MAX_PATH_BYTES,
};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const DATE: u16 = 33; // 1980-01-01 in the DOS date field.
const MADE_BY: u16 = 0x030a; // Unix, ZIP version 1.0.
const MODE: u32 = 0o100644 << 16;

/// One immutable source-snapshot or decoded archive member. Paths have one package-root meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub bytes: Arc<[u8]>,
}

impl File {
    pub fn new(path: impl Into<String>, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }
}

/// An archive failure. `member` is set only after its path passes the portable-path grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub member: Option<String>,
    pub message: String,
}

impl Error {
    fn archive(message: impl Into<String>) -> Self {
        Self {
            member: None,
            message: message.into(),
        }
    }

    fn member(path: &str, message: impl Into<String>) -> Self {
        Self {
            member: Some(path.to_owned()),
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct Member<'a> {
    path: &'a str,
    data: Range<usize>,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let value = bytes
        .get(at..at + 2)
        .ok_or_else(|| Error::archive("truncated ZIP header"))?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let value = bytes
        .get(at..at + 4)
        .ok_or_else(|| Error::archive("truncated ZIP header"))?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn span(start: u64, length: u64, limit: usize) -> Result<Range<usize>, Error> {
    let end = start
        .checked_add(length)
        .filter(|end| *end <= limit as u64)
        .ok_or_else(|| Error::archive("ZIP member range exceeds the artifact"))?;
    let start =
        usize::try_from(start).map_err(|_| Error::archive("ZIP offset exceeds this platform"))?;
    let end =
        usize::try_from(end).map_err(|_| Error::archive("ZIP offset exceeds this platform"))?;
    Ok(start..end)
}

/// Count actual records and bound every variable-length field before a general ZIP parser sees it.
fn preflight(bytes: &[u8]) -> Result<Vec<Member<'_>>, Error> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(Error::archive(
            "bundle exceeds the package artifact byte limit",
        ));
    }
    let end = bytes
        .len()
        .checked_sub(22)
        .ok_or_else(|| Error::archive("truncated ZIP end record"))?;
    if u32_at(bytes, end)? != END
        || u16_at(bytes, end + 4)? != 0
        || u16_at(bytes, end + 6)? != 0
        || u16_at(bytes, end + 20)? != 0
    {
        return Err(Error::archive(
            "expected one terminating ZIP32 end record with no comment",
        ));
    }
    let declared = usize::from(u16_at(bytes, end + 10)?);
    if declared == 0 || declared > MAX_ENTRIES || usize::from(u16_at(bytes, end + 8)?) != declared {
        return Err(Error::archive(
            "ZIP entry count is outside the package profile",
        ));
    }
    let directory_bytes = u64::from(u32_at(bytes, end + 12)?);
    if directory_bytes > MAX_DIRECTORY_BYTES as u64 {
        return Err(Error::archive(
            "ZIP central directory exceeds its byte limit",
        ));
    }
    let directory = span(u64::from(u32_at(bytes, end + 16)?), directory_bytes, end)?;
    if directory.end != end {
        return Err(Error::archive(
            "ZIP central directory must immediately precede the end record",
        ));
    }
    let mut members: Vec<Member<'_>> = Vec::new();
    let mut at = directory.start;
    let mut local_end = 0;
    let mut payload_bytes = 0u64;
    while at < directory.end {
        if members.len() >= MAX_ENTRIES {
            return Err(Error::archive("actual ZIP entry count exceeds its limit"));
        }
        span(at as u64, 46, directory.end)?;
        if u32_at(bytes, at)? != CENTRAL
            || u16_at(bytes, at + 4)? != MADE_BY
            || u16_at(bytes, at + 6)? != 10
            || u16_at(bytes, at + 8)? != 0
            || u16_at(bytes, at + 10)? != 0
            || u16_at(bytes, at + 12)? != 0
            || u16_at(bytes, at + 14)? != DATE
            || u16_at(bytes, at + 30)? != 0
            || u16_at(bytes, at + 32)? != 0
            || u16_at(bytes, at + 34)? != 0
            || u16_at(bytes, at + 36)? != 0
            || u32_at(bytes, at + 38)? != MODE
        {
            return Err(Error::archive(
                "ZIP member metadata differs from the canonical STORE profile",
            ));
        }
        let name_length = usize::from(u16_at(bytes, at + 28)?);
        if name_length == 0 || name_length > MAX_PATH_BYTES {
            return Err(Error::archive(
                "ZIP member name exceeds the portable-path limit",
            ));
        }
        let name = span(at as u64 + 46, name_length as u64, directory.end)?;
        let path = std::str::from_utf8(&bytes[name.clone()])
            .ok()
            .filter(|path| valid_path(path))
            .ok_or_else(|| Error::archive("ZIP member has an invalid package-relative path"))?;
        if members.last().is_some_and(|previous| previous.path >= path) {
            return Err(Error::member(
                path,
                "ZIP names must be unique and sorted by ASCII path",
            ));
        }
        let size = u64::from(u32_at(bytes, at + 20)?);
        if size != u64::from(u32_at(bytes, at + 24)?) {
            return Err(Error::member(
                path,
                "STORE compressed and uncompressed sizes differ",
            ));
        }
        payload_bytes = payload_bytes
            .checked_add(size)
            .filter(|sum| *sum <= MAX_MEMBER_BYTES as u64)
            .ok_or_else(|| {
                Error::member(path, "bundle member payloads exceed their aggregate limit")
            })?;
        let local = u64::from(u32_at(bytes, at + 42)?);
        if local != local_end as u64 {
            return Err(Error::member(
                path,
                "ZIP local records overlap, contain gaps, or have a different order",
            ));
        }
        let header = span(local, 30, directory.start)?;
        let here = header.start;
        if u32_at(bytes, here)? != LOCAL
            || u16_at(bytes, here + 4)? != 10
            || u16_at(bytes, here + 6)? != 0
            || u16_at(bytes, here + 8)? != 0
            || u16_at(bytes, here + 10)? != 0
            || u16_at(bytes, here + 12)? != DATE
            || u32_at(bytes, here + 14)? != u32_at(bytes, at + 16)?
            || u64::from(u32_at(bytes, here + 18)?) != size
            || u64::from(u32_at(bytes, here + 22)?) != size
            || usize::from(u16_at(bytes, here + 26)?) != name_length
            || u16_at(bytes, here + 28)? != 0
        {
            return Err(Error::member(
                path,
                "ZIP local and central headers disagree",
            ));
        }
        let local_name = span(local + 30, name_length as u64, directory.start)?;
        if bytes[local_name.clone()] != bytes[name.clone()] {
            return Err(Error::member(
                path,
                "ZIP local and central member names disagree",
            ));
        }
        let data = span(local_name.end as u64, size, directory.start)?;
        local_end = data.end;
        members.push(Member { path, data });
        at = name.end;
    }
    if at != directory.end || local_end != directory.start || members.len() != declared {
        return Err(Error::archive(
            "ZIP record count or covered byte ranges disagree with the end record",
        ));
    }
    Ok(members)
}

/// Read bounded immutable members after structural preflight. The ZIP crate checks file CRCs.
pub fn read(bytes: &[u8]) -> Result<Vec<File>, Error> {
    let members = preflight(bytes)?;
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::archive(e.to_string()))?;
    archive
        .validate_header_consistency()
        .map_err(|e| Error::archive(e.to_string()))?;
    if archive.len() != members.len() || archive.has_duplicate_names() {
        return Err(Error::archive(
            "ZIP reader disagrees with the bounded member inventory",
        ));
    }
    let mut files = Vec::with_capacity(members.len());
    for (index, member) in members.iter().enumerate() {
        let file = archive
            .by_index(index)
            .map_err(|e| Error::member(member.path, e.to_string()))?;
        let mut content = Vec::with_capacity(member.data.len());
        file.take(member.data.len() as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|e| Error::member(member.path, e.to_string()))?;
        if content.len() != member.data.len() {
            return Err(Error::member(
                member.path,
                "ZIP member size differs from the bytes read",
            ));
        }
        files.push(File::new(member.path, content));
    }
    Ok(files)
}

/// Pack an already-bounded immutable source snapshot with fixed ZIP metadata. Content bytes and
/// JSON member order are preserved. The caller validates the manifest and resource closure.
pub fn pack(files: &[File]) -> Result<Vec<u8>, Error> {
    if files.is_empty() || files.len() > MAX_ENTRIES {
        return Err(Error::archive(
            "source file count is outside the package profile",
        ));
    }
    let mut sorted: Vec<&File> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut payload = 0u64;
    let mut wire = 22u64;
    for (index, file) in sorted.iter().enumerate() {
        if !valid_path(&file.path) {
            return Err(Error::archive(
                "source member has an invalid package-relative path",
            ));
        }
        if index > 0 && sorted[index - 1].path == file.path {
            return Err(Error::member(
                &file.path,
                "source member path is declared twice",
            ));
        }
        payload = payload
            .checked_add(file.bytes.len() as u64)
            .filter(|sum| *sum <= MAX_MEMBER_BYTES as u64)
            .ok_or_else(|| {
                Error::member(&file.path, "source payloads exceed their aggregate limit")
            })?;
        wire = wire
            .checked_add(file.bytes.len() as u64 + 76 + 2 * file.path.len() as u64)
            .filter(|sum| *sum <= MAX_ARTIFACT_BYTES as u64)
            .ok_or_else(|| Error::archive("packed source exceeds the artifact byte limit"))?;
    }
    let cursor = Cursor::new(Vec::with_capacity(wire as usize));
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(
            DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
                .map_err(|_| Error::archive("invalid canonical ZIP timestamp"))?,
        )
        .system(System::Unix)
        .external_attributes(MODE)
        .large_file(false)
        .with_alignment(1);
    for file in sorted {
        writer
            .start_file(&file.path, options)
            .map_err(|e| Error::member(&file.path, e.to_string()))?;
        writer
            .write_all(&file.bytes)
            .map_err(|e| Error::member(&file.path, e.to_string()))?;
    }
    let bytes = writer
        .finish()
        .map_err(|e| Error::archive(e.to_string()))?
        .into_inner();
    preflight(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Vec<File> {
        vec![
            File::new("manifest.json", &b"{\"b\":1,\"a\":2}"[..]),
            File::new("assets/play.svg", &b"alpha\n"[..]),
        ]
    }

    #[test]
    fn canonical_writer_preserves_payload_bytes_and_ignores_input_file_order() {
        let mut files = source();
        let bytes = pack(&files).unwrap();
        files.reverse();
        assert_eq!(pack(&files).unwrap(), bytes);
        assert_eq!(read(&bytes).unwrap(), files);
        assert_eq!(&bytes[..4], &LOCAL.to_le_bytes());
        assert_eq!(u16_at(&bytes, 4).unwrap(), 10);
        assert_eq!(u16_at(&bytes, 6).unwrap(), 0);
        assert_eq!(u16_at(&bytes, 12).unwrap(), DATE);
        assert_eq!(
            bytes.len(),
            files
                .iter()
                .map(|f| f.bytes.len() + 76 + 2 * f.path.len())
                .sum::<usize>()
                + 22
        );
    }

    #[test]
    fn only_portable_paths_enter_the_writer() {
        for path in [
            "",
            "/absolute",
            "../escape",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\b",
            "A.svg",
            "a:",
            "a?",
            "a*",
            "a b",
            "a%2fb",
            "con.svg",
            "lpt9.txt",
            "a.",
            "a/b/c/d/e/f/g/h/i",
            "a\0b",
        ] {
            assert!(pack(&[File::new(path, &b""[..])]).is_err(), "{path:?}");
        }
        assert!(pack(&[File::new("icons/com0.svg", &b""[..])]).is_ok());
        assert!(pack(&[File::new("x".repeat(MAX_PATH_BYTES + 1), &b""[..])]).is_err());
    }

    #[test]
    fn crc_failure_and_content_edits_are_detected() {
        let mut bytes = pack(&source()).unwrap();
        let at = preflight(&bytes).unwrap()[0].data.start;
        bytes[at] ^= 1;
        assert!(preflight(&bytes).is_ok());
        assert!(read(&bytes).is_err());
    }

    #[test]
    fn all_noncanonical_header_features_are_rejected_before_zip_construction() {
        let original = pack(&source()).unwrap();
        let end = original.len() - 22;
        let directory = u32_at(&original, end + 16).unwrap() as usize;
        for (at, value) in [
            (4, 20),
            (6, 1),
            (8, 8),
            (10, 1),
            (28, 1),
            (directory + 4, 20),
            (directory + 8, 8),
            (directory + 10, 8),
            (directory + 30, 1),
            (directory + 32, 1),
            (directory + 34, 1),
            (directory + 38, 1),
            (directory + 42, 1),
            (end + 4, 1),
            (end + 8, 1),
            (end + 10, 1),
            (end + 20, 1),
        ] {
            let mut bytes = original.clone();
            bytes[at] = value;
            assert!(preflight(&bytes).is_err(), "header byte {at}");
        }
        let mut trailing = original.clone();
        trailing.push(0);
        assert!(preflight(&trailing).is_err());
        let mut prefix = vec![0];
        prefix.extend(original);
        assert!(preflight(&prefix).is_err());
    }

    #[test]
    fn size_and_entry_limits_are_checked_before_writer_or_reader_allocations() {
        assert!(read(&vec![0; MAX_ARTIFACT_BYTES + 1]).is_err());
        assert!(pack(&[File::new("large", vec![0; MAX_MEMBER_BYTES + 1])]).is_err());
        let many: Vec<File> = (0..=MAX_ENTRIES)
            .map(|n| File::new(format!("f{n:03}"), &b""[..]))
            .collect();
        assert!(pack(&many).is_err());
        assert!(pack(&[File::new("a", &b"x"[..]), File::new("a", &b"y"[..])]).is_err());
    }

    #[test]
    fn truncated_and_mutated_archives_never_panic() {
        let original = pack(&source()).unwrap();
        for length in 0..original.len() {
            assert!(read(&original[..length]).is_err());
        }
        for at in 0..original.len() {
            let mut bytes = original.clone();
            bytes[at] ^= 0xff;
            let _ = read(&bytes);
        }
    }
}
