//! A small on-disk image cache — the foundation, used today for ONE class of art: Plex Home
//! profile avatars on the who's-watching screen.
//!
//! ## Why it exists, and why only avatars
//!
//! Every image the app draws is fetched through the Plex server's `/photo/:/transcode` proxy and
//! held in memory by `posters.rs` for the run. Posters, backdrops and hero art come from the
//! server itself, one hop away, and so load offline as they do online (`docs/shared-servers.md`,
//! the offline section). Avatars do not: a profile's `thumb` is an absolute `https://plex.tv/users/
//! …/avatar` URL that the SERVER fetches from plex.tv on the app's behalf, so with the uplink down
//! the picker shows blank circles — the one screen a household sees on every boot, blank on the
//! one day this app most needs to look alive (owner, 2026-09-06). A few small images per house,
//! changing rarely: exactly the shape a disk cache is for. Cast headshots
//! (`metadata-static.plex.tv`) are the same failure and NOT cached here yet — thousands per
//! library, a different bound and a different eviction question — and posters never will be.
//!
//! ## How it is wired
//!
//! No caller names it. `posters.rs`'s worker classifies the key it is about to fetch
//! ([`classify`]: the built `/photo/:/transcode?…url=<encoded source>…` path is decoded and the
//! source graded), and for a durable class **the file wins whenever there is one**; only a miss
//! fetches, and the fetched bytes are written after they decode. The file key is a SHA-256 of
//! the class, the source URL WITHOUT its query and the requested size — plex.tv's `?c=` is the
//! time it answered, not a version of the picture (see [`classify`]) — and a file older than
//! [`REFRESH_AFTER`] is fetched again when the network is there, so a changed avatar shows up
//! within a day. Neither the server nor the token is part of the key: an avatar is the
//! person's, not the server's. (The first cut fetched first and used
//! the file as a fallback; offline, the server's own request to plex.tv hangs for the whole
//! transfer budget, both poster workers sat on avatars, and the picker showed no faces — owner,
//! 2026-09-06.)
//!
//! ## Bounds and hygiene
//!
//! One directory, chosen once from [`crate::paths::image_cache_candidates`] (the session file's
//! search order, as directories). Writes are atomic (tmp + rename), a file over [`MAX_FILE`] is
//! not stored, and every write prunes the class to its [`Class::cap`] newest files by mtime, so
//! the cache cannot grow past a few hundred kilobytes whatever a roster does. Sign-out empties it
//! ([`clear`], from `auth::forget_account`): the pictures belong to the account that left.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A cached file older than this is refreshed from the network when the network is there — the
/// key no longer changes with the picture, so age is what says "look again". A day: a changed
/// avatar shows up by tomorrow, and an unchanged one costs one fetch a day.
pub(crate) const REFRESH_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The largest file this cache stores. A 300×300 avatar is 10–40 KB; the ceiling is for a proxy
/// answering with something other than the picture.
const MAX_FILE: usize = 512 * 1024;

/// A durable class of art, each with its own cap. Adding one is an arm in [`classify`] and a row
/// here; nothing else in the app changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    /// `https://plex.tv/users/<id>/avatar…` — a Plex Home profile picture.
    Avatar,
}

impl Class {
    fn tag(self) -> &'static str {
        match self {
            Class::Avatar => "avatar",
        }
    }
    /// Files kept per class, newest first. A Plex Home holds at most 15 profiles; two sizes each
    /// and a changed picture or two still fit with room to spare.
    fn cap(self) -> usize {
        match self {
            Class::Avatar => 48,
        }
    }
}

/// What the worker holds for a durable fetch: the class and the file it maps to.
#[derive(Clone, Debug)]
pub(crate) struct DiskKey {
    class: Class,
    name: String,
}

/// Grade a poster-store key (the built `/photo/:/transcode?…` request path). `None` for the
/// ordinary case — server-relative art, or anything this cache does not hold.
pub(crate) fn classify(built_key: &str) -> Option<DiskKey> {
    let query = built_key.split_once('?')?.1;
    let mut source = None;
    let mut w = "";
    let mut h = "";
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "url" => source = Some(percent_decode(v)),
            "width" => w = v,
            "height" => h = v,
            _ => {}
        }
    }
    let source = source?;
    let class = class_of(&source)?;
    // **The query is not part of the identity.** plex.tv stamps an avatar URL's `?c=` with the
    // time it answered the roster request, not with a version of the picture — one session file
    // held five different values for three unchanged faces, two of them stamped in the same
    // second (device, 2026-09-06) — so keying on it made every online boot a miss and every
    // refresh a new file. Staleness is handled by age instead ([`REFRESH_AFTER`]).
    let source = source.split('?').next().unwrap_or(&source).to_string();
    let digest = crate::sha256::sha256(format!("{}\n{}\n{w}x{h}", class.tag(), source).as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(DiskKey {
        class,
        name: format!("{}-{hex}.img", class.tag()),
    })
}

/// The one rule per class. Host AND path shape, both, so a future plex.tv route does not fall
/// into the avatar cap by sharing a host.
fn class_of(source: &str) -> Option<Class> {
    let rest = source
        .strip_prefix("https://plex.tv/")
        .or_else(|| source.strip_prefix("http://plex.tv/"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    let mut segs = path.split('/');
    match (segs.next(), segs.next(), segs.next()) {
        (Some("users"), Some(id), Some("avatar")) if !id.is_empty() => Some(Class::Avatar),
        _ => None,
    }
}

/// `%XX` → byte, `+` left alone (the store key is RFC 3986-encoded, not form-encoded). A bad
/// escape is passed through as-is: the worst outcome is a source that classifies as nothing.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(b[i + 1]), hex(b[i + 2])) {
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

// ---- the directory ----

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Bumped by [`clear`]. A worker reads it before a fetch and hands it back to [`write_at`], so
/// bytes fetched under the account that just signed out cannot land on disk after the sweep.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Serializes "check the generation, then write" against "bump the generation, then sweep":
/// without it a sweep could land between a worker's check and its rename, and the departed
/// account's picture would be recreated by a write that had passed the check.
static WRITE_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Uniqueness for temporary files across the poster workers, which share a pid.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current sign-out generation — capture it before a fetch, see [`write_at`].
pub(crate) fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// The cache directory, probed once: the first candidate that can be created and written. `None`
/// (logged once) leaves every read a miss and every write a no-op — the app is unchanged.
fn dir() -> Option<&'static Path> {
    DIR.get_or_init(|| {
        for cand in crate::paths::image_cache_candidates() {
            if probe_dir(&cand) {
                crate::log(&format!("imgcache: {}", cand.display()));
                return Some(cand);
            }
        }
        crate::log("imgcache: no writable directory — avatars will not survive a relaunch");
        None
    })
    .as_deref()
}

fn probe_dir(p: &Path) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The cached bytes for `key`, if any. Touches the file's mtime on a hit, so the LRU prune keeps
/// what is being looked at.
pub(crate) fn read(key: &DiskKey) -> Option<Vec<u8>> {
    read_in(dir()?, key)
}

/// Store `bytes` under `key` (atomically), then prune the class to its cap — unless a sign-out
/// ([`clear`]) happened since `gen` was read, in which case the bytes belong to an account that
/// has left and are dropped. Oversized input is declined rather than truncated. Returns whether
/// the file is now on disk.
pub(crate) fn write_at(gen: u64, key: &DiskKey, bytes: &[u8]) -> bool {
    let _g = WRITE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if generation() != gen {
        return false;
    }
    dir().is_some_and(|d| write_in(d, key, bytes))
}

/// How long ago `key`'s file was written or last refreshed. `None` when there is no file.
pub(crate) fn age(key: &DiskKey) -> Option<std::time::Duration> {
    let m = std::fs::metadata(dir()?.join(&key.name)).ok()?.modified().ok()?;
    Some(m.elapsed().unwrap_or_default())
}

/// Retire one file — the poster worker's answer to a cached file that no longer decodes.
pub(crate) fn remove(key: &DiskKey) {
    if let Some(d) = dir() {
        let _ = std::fs::remove_file(d.join(&key.name));
    }
}

/// Remove every cached file, and retire every fetch already in flight ([`write_at`]). Sign-out.
pub(crate) fn clear() {
    let _g = WRITE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    if let Some(d) = dir() {
        clear_in(d);
    }
}

fn read_in(d: &Path, key: &DiskKey) -> Option<Vec<u8>> {
    let p = d.join(&key.name);
    let bytes = std::fs::read(&p).ok()?;
    if bytes.is_empty() {
        return None;
    }
    // mtime is the file's WRITE time — the refresh clock ([`REFRESH_AFTER`]) and the prune's
    // order — so a read leaves it alone.
    Some(bytes)
}

fn write_in(d: &Path, key: &DiskKey, bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > MAX_FILE {
        return false;
    }
    let dst = d.join(&key.name);
    // pid AND a sequence: two poster workers share the pid, and a colliding temporary name
    // would have one worker's rename take the other's bytes (or its unlink take its file).
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = d.join(format!("{}.tmp{}-{seq}", key.name, std::process::id()));
    let ok = std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &dst).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    prune_in(d, key.class);
    true
}

/// Keep the newest `cap` files of `class` (by mtime); remove the rest. Temporary files are
/// left alone: another worker may be between its write and its rename, and a failed write
/// removes its own. (They are swept by [`clear`], where nothing is in flight by contract.)
fn prune_in(d: &Path, class: Class) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let prefix = format!("{}-", class.tag());
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with(&prefix) || !name.ends_with(".img") {
            continue;
        }
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push((mtime, p));
    }
    if files.len() <= class.cap() {
        return;
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in files.into_iter().skip(class.cap()) {
        let _ = std::fs::remove_file(p);
    }
}

fn clear_in(d: &Path) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "img") || p.to_string_lossy().contains(".tmp") {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "/photo/:/transcode?width=300&height=300&minSize=1&url=https%3A%2F%2Fplex.tv%2Fusers%2F0123abcd%2Favatar%3Fc%3D1700000000&X-Plex-Token=tok";

    #[test]
    fn a_plex_avatar_classifies_and_everything_else_does_not() {
        let k = classify(AVATAR).expect("an avatar key");
        assert_eq!(k.class, Class::Avatar);
        assert!(k.name.starts_with("avatar-") && k.name.ends_with(".img"));
        assert_eq!(k.name.len(), "avatar-".len() + 64 + ".img".len());
        // server-relative art, a headshot on the metadata CDN, an avatar-looking path on another
        // host, and a plex.tv route that is not an avatar
        for key in [
            "/photo/:/transcode?width=250&height=375&minSize=1&url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1&X-Plex-Token=t",
            "/photo/:/transcode?width=300&height=300&minSize=1&url=https%3A%2F%2Fmetadata-static.plex.tv%2Fa%2Fpeople%2Fabc.jpg&X-Plex-Token=t",
            "/photo/:/transcode?width=300&height=300&minSize=1&url=https%3A%2F%2Fevil.example%2Fusers%2Fx%2Favatar&X-Plex-Token=t",
            "/photo/:/transcode?width=300&height=300&minSize=1&url=https%3A%2F%2Fplex.tv%2Fapi%2Fv2%2Fusers%2Fx&X-Plex-Token=t",
            "/photo/:/transcode?width=300&height=300&minSize=1&X-Plex-Token=t",
            "no-query-at-all",
        ] {
            assert!(classify(key).is_none(), "{key}");
        }
    }

    /// The token and the server are not part of the identity; the size and the cache-buster are.
    #[test]
    fn the_file_name_ignores_the_token_and_honours_size_and_cache_buster() {
        let a = classify(AVATAR).unwrap().name;
        let b = classify(&AVATAR.replace("X-Plex-Token=tok", "X-Plex-Token=other")).unwrap().name;
        assert_eq!(a, b);
        let c = classify(&AVATAR.replace("width=300&height=300", "width=150&height=150")).unwrap().name;
        assert_ne!(a, c);
        // plex.tv's `?c=` stamp is the roster request's time, not the picture's version
        let d = classify(&AVATAR.replace("1700000000", "1700000001")).unwrap().name;
        assert_eq!(a, d);
        let e = classify(&AVATAR.replace("%3Fc%3D1700000000", "")).unwrap().name;
        assert_eq!(a, e, "no query at all is the same picture");
    }

    #[test]
    fn a_fetch_that_straddles_a_sign_out_writes_nothing() {
        let _g = crate::testlock::serial();
        let gen = generation();
        clear(); // bumps the generation whether or not a directory exists
        assert_ne!(generation(), gen);
        let k = classify(AVATAR).unwrap();
        assert!(!write_at(gen, &k, b"x"), "stale generation: declined before touching the disk");
        // and the interleaving the gate exists for: a sweep that starts while a write holds
        // the gate finishes AFTER it, and a write that starts while a sweep holds it sees the
        // new generation — a stale-generation write can never land after the sweep.
        let now = generation();
        let held = WRITE_GATE.lock().unwrap();
        let sweeper = std::thread::spawn(clear);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(generation(), now, "the sweep waits for the gate");
        drop(held);
        sweeper.join().unwrap();
        assert!(!write_at(now, &k, b"x"), "the generation it read is gone");
    }

    #[test]
    fn age_is_the_write_time_and_a_read_does_not_move_it() {
        let d = temp_root("age");
        let k = classify(AVATAR).unwrap();
        assert!(write_in(&d, &k, b"x"));
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options().append(true).open(d.join(&k.name)).unwrap().set_modified(t).unwrap();
        let _ = read_in(&d, &k);
        let m = std::fs::metadata(d.join(&k.name)).unwrap().modified().unwrap();
        assert_eq!(m, t, "a read must not refresh the clock");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn percent_decoding_is_rfc3986_and_lenient() {
        assert_eq!(percent_decode("a%2Fb%3Fc%3D1"), "a/b?c=1");
        assert_eq!(percent_decode("plus+stays"), "plus+stays");
        assert_eq!(percent_decode("bad%zz%2"), "bad%zz%2");
    }

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("plx-imgcache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_write_round_trips_atomically_and_clear_empties() {
        let d = temp_root("rt");
        let k = classify(AVATAR).unwrap();
        assert!(read_in(&d, &k).is_none());
        assert!(write_in(&d, &k, b"jpegbytes"));
        assert_eq!(read_in(&d, &k).unwrap(), b"jpegbytes");
        assert!(!write_in(&d, &k, b""), "empty is not stored");
        assert!(!write_in(&d, &k, &vec![0u8; MAX_FILE + 1]), "oversized is declined");
        assert_eq!(read_in(&d, &k).unwrap(), b"jpegbytes", "a declined write leaves the file");
        assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.path().to_string_lossy().contains(".tmp")));
        clear_in(&d);
        assert!(read_in(&d, &k).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_class_is_pruned_to_its_cap_newest_first() {
        let d = temp_root("prune");
        let cap = Class::Avatar.cap();
        let mut names = Vec::new();
        for i in 0..cap + 5 {
            let key = AVATAR.replace("0123abcd", &format!("user{i:03}"));
            let k = classify(&key).unwrap();
            assert!(write_in(&d, &k, b"x"));
            // distinct mtimes on a coarse filesystem clock
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + i as u64);
            std::fs::File::options().append(true).open(d.join(&k.name)).unwrap().set_modified(t).unwrap();
            names.push(k);
        }
        // one more write triggers the prune against the stamped clock
        let last = classify(&AVATAR.replace("0123abcd", "userlast")).unwrap();
        assert!(write_in(&d, &last, b"x"));
        let kept = std::fs::read_dir(&d).unwrap().flatten().count();
        assert_eq!(kept, cap);
        assert!(read_in(&d, &names[0]).is_none(), "the oldest went first");
        assert!(read_in(&d, &names[cap + 4]).is_some(), "the newest stayed");
        let _ = std::fs::remove_dir_all(&d);
    }
}
