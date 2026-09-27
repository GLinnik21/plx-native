//! Shared durable tier for all Plex image transcodes. The poster workers own fetch/decode;
//! this module owns canonical identities, bounded disk storage, and sign-out generations.
//!
//! The index is scanned once, lazily on a worker's first access. Steady-state reads/writes use
//! an in-memory ordered LRU, never a directory scan. Files contain only a versioned timestamp
//! header and compressed image bytes; names hash the server, source, and transformation.
//! Fetch time drives refresh independently of recency, which is persisted at most hourly.
//! Limits cover committed files; one atomic-write temporary can additionally occupy MAX_FILE.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(24 * 3600);
const TOUCH_AFTER: Duration = Duration::from_secs(3600);
const MAX_FILE: usize = 4 * 1024 * 1024;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
const MAGIC: &[u8; 8] = b"PLXIMG02";
const HEADER_LEN: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DiskKey {
    name: String,
    legacy: Option<String>,
}

pub(crate) struct CachedImage {
    pub bytes: Vec<u8>,
    pub stale: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Snapshot {
    pub hit: u64,
    pub miss: u64,
    pub write: u64,
    pub eviction: u64,
    pub entries: u64,
    pub bytes: u64,
}

/// Every transcode option participates, except the outer authentication token. Namespace is
/// the stable Plex machine identifier (or server origin when discovery has no identifier).
pub(crate) fn classify(server_namespace: &str, built_key: &str) -> Option<DiskKey> {
    let (path, query) = built_key.split_once('?')?;
    if path != "/photo/:/transcode" || server_namespace.is_empty() {
        return None;
    }
    let mut params: Vec<(String, String)> = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .filter(|(k, _)| !k.eq_ignore_ascii_case("X-Plex-Token"))
        .collect();
    let source_indices: Vec<usize> = params
        .iter()
        .enumerate()
        .filter_map(|(i, (k, _))| (k == "url").then_some(i))
        .collect();
    if source_indices.len() != 1 {
        return None;
    }
    let source_index = source_indices[0];
    if params[source_index].1.is_empty() {
        return None;
    }
    let avatar = is_avatar(&params[source_index].1);
    if avatar {
        params[source_index].1 = without_avatar_stamp(&params[source_index].1);
    }
    // The previous avatar cache knew only size/minSize=1, and ignored every source query.
    // Migrate only that exact request shape, never a different format, crop, or version.
    let legacy = if avatar
        && !params[source_index].1.contains('?')
        && params.iter().all(|(k, v)| {
            matches!(k.as_str(), "url" | "width" | "height") || (k == "minSize" && v == "1")
        }) {
        let value = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
        };
        Some(format!(
            "avatar-{}.img",
            hex_digest(
                format!(
                    "avatar\n{}\n{}x{}",
                    value("url"),
                    value("width"),
                    value("height")
                )
                .as_bytes()
            )
        ))
    } else {
        None
    };
    params.sort();
    let mut identity = Vec::new();
    for field in std::iter::once("plx-image-v2")
        .chain(std::iter::once(server_namespace))
        .chain(params.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]))
    {
        identity.extend_from_slice(&(field.len() as u64).to_le_bytes());
        identity.extend_from_slice(field.as_bytes());
    }
    Some(DiskKey {
        name: format!("image-{}.img", hex_digest(&identity)),
        legacy,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    crate::sha256::sha256(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn is_avatar(source: &str) -> bool {
    let Some(rest) = source
        .strip_prefix("https://plex.tv/")
        .or_else(|| source.strip_prefix("http://plex.tv/"))
    else {
        return false;
    };
    let mut parts = rest.split('?').next().unwrap_or(rest).split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some("users"), Some(id), Some("avatar")) if !id.is_empty())
}

fn without_avatar_stamp(source: &str) -> String {
    let Some((path, query)) = source.split_once('?') else {
        return source.into();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| percent_decode(pair.split_once('=').map_or(*pair, |(k, _)| k)) != "c")
        .collect();
    if kept.is_empty() {
        path.into()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// RFC3986 decoding: literal `+` is not a form-encoded space.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            ) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    entries: usize,
}

struct Entry {
    size: u64,
    order: u64,
    touched: SystemTime,
}

#[derive(Default)]
struct Index {
    initialized: bool,
    root: Option<PathBuf>,
    entries: HashMap<String, Entry>,
    lru: BTreeSet<(u64, String)>,
    bytes: u64,
    sequence: u64,
}

#[derive(Default)]
struct Counters {
    hit: AtomicU64,
    miss: AtomicU64,
    write: AtomicU64,
    eviction: AtomicU64,
    entries: AtomicU64,
    bytes: AtomicU64,
}

/// All filesystem access shares this gate, including reads and removes: an old worker cannot
/// read or delete a new account's entry after clear. Constructors do no filesystem work.
struct Cache {
    candidates: Vec<PathBuf>,
    limits: Limits,
    generation: AtomicU64,
    index: Mutex<Index>,
    counters: Counters,
}

impl Cache {
    fn new(candidates: Vec<PathBuf>, limits: Limits) -> Self {
        Self {
            candidates,
            limits,
            generation: AtomicU64::new(0),
            index: Mutex::new(Index::default()),
            counters: Counters::default(),
        }
    }

    fn initialize(&self, index: &mut Index) {
        if index.initialized {
            return;
        }
        index.initialized = true;
        index.root = self.candidates.iter().find(|p| probe_dir(p)).cloned();
        let Some(root) = index.root.clone() else {
            crate::log("imgcache: no writable directory; disk cache unavailable");
            return;
        };
        let Ok(files) = fs::read_dir(&root) else {
            return;
        };
        let mut found = Vec::new();
        for file in files.flatten() {
            let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.contains(".tmp") {
                let _ = fs::remove_file(file.path());
                continue;
            }
            if !cache_name(&name) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(file.path()) else {
                continue;
            };
            if !meta.is_file() {
                let _ = fs::remove_file(file.path());
                continue;
            }
            let max_size = MAX_FILE
                + if name.starts_with("image-") {
                    HEADER_LEN
                } else {
                    0
                };
            if (meta.len() == 0 || meta.len() > max_size as u64)
                && fs::remove_file(file.path()).is_ok()
            {
                continue;
            }
            found.push((meta.modified().unwrap_or(UNIX_EPOCH), name, meta.len()));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        for (mtime, name, size) in found {
            index.insert(name, size, mtime);
        }
        self.make_room(index, None, 0, 0);
        self.publish_size(index);
        crate::log(&format!(
            "imgcache: {} entries={} bytes={}",
            root.display(),
            index.entries.len(),
            index.bytes
        ));
    }

    fn read_at(&self, generation: u64, key: &DiskKey) -> Option<CachedImage> {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::Acquire) != generation {
            return None;
        }
        self.initialize(&mut index);
        let result = self.read_locked(&mut index, key);
        if result.is_some() {
            &self.counters.hit
        } else {
            &self.counters.miss
        }
        .fetch_add(1, Ordering::Relaxed);
        result
    }

    fn read_locked(&self, index: &mut Index, key: &DiskKey) -> Option<CachedImage> {
        let root = index.root.clone()?;
        let now = SystemTime::now();
        if index.entries.contains_key(&key.name) {
            match read_file(&root.join(&key.name), false) {
                Some((bytes, fetched)) => {
                    index.touch(&key.name, now);
                    return Some(CachedImage {
                        bytes,
                        stale: stale(fetched, now),
                    });
                }
                None => {
                    self.remove_locked(index, &key.name);
                }
            }
        }
        let legacy = key.legacy.as_ref()?;
        if !index.entries.contains_key(legacy) {
            return None;
        }
        let Some((bytes, fetched)) = read_file(&root.join(legacy), true) else {
            self.remove_locked(index, legacy);
            return None;
        };
        // Preserve the old refresh clock. A migration is not a successful server refresh.
        if self.write_locked(index, key, &bytes, fetched) {
            self.remove_locked(index, legacy);
        }
        Some(CachedImage {
            bytes,
            stale: stale(fetched, now),
        })
    }

    fn write_at(&self, generation: u64, key: &DiskKey, bytes: &[u8]) -> bool {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::Acquire) != generation {
            return false;
        }
        self.initialize(&mut index);
        self.write_locked(&mut index, key, bytes, unix_seconds(SystemTime::now()))
    }

    fn write_locked(&self, index: &mut Index, key: &DiskKey, bytes: &[u8], fetched: u64) -> bool {
        if bytes.is_empty() || bytes.len() > MAX_FILE {
            return false;
        }
        let Some(root) = index.root.clone() else {
            return false;
        };
        let size = (bytes.len() + HEADER_LEN) as u64;
        if size > self.limits.bytes || self.limits.entries == 0 {
            return false;
        }
        // One writer under the gate, and startup removes residue from an interrupted process.
        let tmp = root.join(format!("{}.tmp", key.name));
        let dst = root.join(&key.name);
        let write = || -> std::io::Result<()> {
            let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&fetched.to_le_bytes())?;
            f.write_all(&(bytes.len() as u64).to_le_bytes())?;
            f.write_all(bytes)
        };
        if write().is_err() {
            let _ = fs::remove_file(&tmp);
            return false;
        }
        let old_size = index.entries.get(&key.name).map_or(0, |e| e.size);
        let added_entries = usize::from(!index.entries.contains_key(&key.name));
        let enough = self.make_room(
            index,
            Some(&key.name),
            size.saturating_sub(old_size),
            added_entries,
        );
        // A smaller replacement can only reduce bytes; conservatively make_room need not
        // account for that saving until the replacement actually succeeds.
        if !enough || fs::rename(&tmp, &dst).is_err() {
            let _ = fs::remove_file(&tmp);
            self.publish_size(index);
            return false;
        }
        index.forget(&key.name);
        index.insert(key.name.clone(), size, SystemTime::now());
        self.counters.write.fetch_add(1, Ordering::Relaxed);
        self.publish_size(index);
        true
    }

    /// Failed unlinks remain accounted for. If capacity cannot be freed, decline the write.
    fn make_room(
        &self,
        index: &mut Index,
        protected: Option<&str>,
        added_bytes: u64,
        added_entries: usize,
    ) -> bool {
        while index.bytes.saturating_add(added_bytes) > self.limits.bytes
            || index.entries.len() + added_entries > self.limits.entries
        {
            let candidate = index
                .lru
                .iter()
                .find(|(_, name)| Some(name.as_str()) != protected)
                .map(|(_, name)| name.clone());
            let Some(name) = candidate else { return false };
            if !self.remove_locked(index, &name) {
                return false;
            }
            self.counters.eviction.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    fn remove_at(&self, generation: u64, key: &DiskKey) {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if self.generation.load(Ordering::Acquire) != generation {
            return;
        }
        self.initialize(&mut index);
        self.remove_locked(&mut index, &key.name);
        if let Some(legacy) = &key.legacy {
            self.remove_locked(&mut index, legacy);
        }
    }

    fn remove_locked(&self, index: &mut Index, name: &str) -> bool {
        let Some(root) = index.root.as_ref() else {
            return false;
        };
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        index.forget(name);
        self.publish_size(index);
        true
    }

    fn clear(&self) {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::AcqRel);
        // Sweep all candidates even when this process has never initialized the cache. A
        // previous install may have fallen back to a different writable location.
        for root in &self.candidates {
            let Ok(files) = fs::read_dir(root) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".img") || name.contains(".tmp") {
                    let _ = fs::remove_file(file.path());
                }
            }
        }
        *index = Index::default();
        self.publish_size(&index);
    }

    fn publish_size(&self, index: &Index) {
        self.counters
            .entries
            .store(index.entries.len() as u64, Ordering::Relaxed);
        self.counters.bytes.store(index.bytes, Ordering::Relaxed);
    }

    fn stats(&self) -> Snapshot {
        let c = &self.counters;
        Snapshot {
            hit: c.hit.load(Ordering::Relaxed),
            miss: c.miss.load(Ordering::Relaxed),
            write: c.write.load(Ordering::Relaxed),
            eviction: c.eviction.load(Ordering::Relaxed),
            entries: c.entries.load(Ordering::Relaxed),
            bytes: c.bytes.load(Ordering::Relaxed),
        }
    }
}

impl Index {
    fn insert(&mut self, name: String, size: u64, touched: SystemTime) {
        self.sequence += 1;
        self.bytes += size;
        self.lru.insert((self.sequence, name.clone()));
        self.entries.insert(
            name,
            Entry {
                size,
                order: self.sequence,
                touched,
            },
        );
    }

    fn forget(&mut self, name: &str) {
        if let Some(entry) = self.entries.remove(name) {
            self.bytes -= entry.size;
            self.lru.remove(&(entry.order, name.into()));
        }
    }

    fn touch(&mut self, name: &str, now: SystemTime) {
        let Some(entry) = self.entries.get_mut(name) else {
            return;
        };
        self.lru.remove(&(entry.order, name.into()));
        self.sequence += 1;
        entry.order = self.sequence;
        self.lru.insert((entry.order, name.into()));
        if now.duration_since(entry.touched).unwrap_or_default() >= TOUCH_AFTER {
            if let Some(root) = &self.root {
                if File::options()
                    .write(true)
                    .open(root.join(name))
                    .and_then(|f| f.set_modified(now))
                    .is_ok()
                {
                    entry.touched = now;
                }
            }
        }
    }
}

fn read_file(path: &Path, legacy: bool) -> Option<(Vec<u8>, u64)> {
    let max = MAX_FILE + if legacy { 0 } else { HEADER_LEN };
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > max as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // The bound also holds if another process replaced/grew the file after metadata().
    File::open(path)
        .ok()?
        .take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.is_empty() || bytes.len() > max {
        return None;
    }
    if legacy {
        return Some((bytes, unix_seconds(meta.modified().unwrap_or(UNIX_EPOCH))));
    }
    if bytes.len() <= HEADER_LEN || &bytes[..8] != MAGIC {
        return None;
    }
    let fetched = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let length = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    if length != (bytes.len() - HEADER_LEN) as u64 {
        return None;
    }
    bytes.drain(..HEADER_LEN);
    Some((bytes, fetched))
}

fn stale(fetched: u64, now: SystemTime) -> bool {
    let now = unix_seconds(now);
    fetched > now || now - fetched >= REFRESH_AFTER.as_secs()
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cache_name(name: &str) -> bool {
    name.strip_prefix("image-")
        .or_else(|| name.strip_prefix("avatar-"))
        .and_then(|s| s.strip_suffix(".img"))
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn probe_dir(path: &Path) -> bool {
    if fs::create_dir_all(path).is_err() {
        return false;
    }
    let probe = path.join(".probe");
    let ok = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&probe)
        .is_ok();
    let _ = fs::remove_file(probe);
    ok
}

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| {
        Cache::new(
            crate::paths::image_cache_candidates(),
            Limits {
                bytes: MAX_BYTES,
                entries: MAX_ENTRIES,
            },
        )
    })
}

pub(crate) fn generation() -> u64 {
    cache().generation.load(Ordering::Acquire)
}
pub(crate) fn read_at(generation: u64, key: &DiskKey) -> Option<CachedImage> {
    cache().read_at(generation, key)
}
pub(crate) fn write_at(generation: u64, key: &DiskKey, bytes: &[u8]) -> bool {
    cache().write_at(generation, key, bytes)
}
pub(crate) fn remove_at(generation: u64, key: &DiskKey) {
    cache().remove_at(generation, key);
}
pub(crate) fn clear() {
    cache().clear();
}
/// Lock-free and does not initialize the index: safe for frame-loop diagnostics.
pub(crate) fn stats() -> Snapshot {
    cache().stats()
}

#[cfg(test)]
mod tests;
