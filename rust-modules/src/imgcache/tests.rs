use super::*;

const AVATAR: &str = "/photo/:/transcode?width=300&height=300&minSize=1&url=https%3A%2F%2Fplex.tv%2Fusers%2F0123abcd%2Favatar%3Fc%3D1700000000&X-Plex-Token=tok";
const POSTER: &str = "/photo/:/transcode?width=250&height=375&minSize=1&url=%2Flibrary%2Fmetadata%2F42%2Fthumb%2F1&X-Plex-Token=t";

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "plx-imgcache-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn cache(&self, entries: usize, bytes: u64) -> Cache {
        Cache::new(vec![self.0.clone()], Limits { entries, bytes })
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn poster(id: usize) -> DiskKey {
    classify(
        "server-a",
        &POSTER.replace("%2F42%2F", &format!("%2F{id}%2F")),
    )
    .unwrap()
}

#[test]
fn library_art_has_a_durable_disk_key() {
    assert!(classify("server-a", POSTER).is_some());
}

#[test]
fn keys_include_namespace_source_version_and_every_transform() {
    let name = classify("server-a", POSTER).unwrap().name;
    assert_ne!(name, classify("server-b", POSTER).unwrap().name);
    for key in [
        POSTER.replace("width=250", "width=251"),
        POSTER.replace("height=375", "height=376"),
        POSTER.replace("minSize=1", "minSize=0"),
        POSTER.replace("thumb%2F1", "thumb%2F2"),
        POSTER.replace("thumb%2F1", "thumb%2F1%3Fv%3D2"),
        format!("{POSTER}&format=png"),
        format!("{POSTER}&upscale=0"),
        format!("{POSTER}&quality=90"),
    ] {
        assert_ne!(name, classify("server-a", &key).unwrap().name, "{key}");
    }
    assert_eq!(
        name,
        classify(
            "server-a",
            &POSTER.replace("X-Plex-Token=t", "X-Plex-Token=another")
        )
        .unwrap()
        .name
    );
    let shuffled = "/photo/:/transcode?X-Plex-Token=rotated&minSize=1&height=375&url=%2flibrary%2fmetadata%2f42%2fthumb%2f1&width=250";
    assert_eq!(name, classify("server-a", shuffled).unwrap().name);
    assert!(name.starts_with("image-") && name.ends_with(".img"));
    assert!(cache_name(&name));
    for bad in [
        "no-query",
        "/other?url=image",
        "/photo/:/transcode?width=1",
        "/photo/:/transcode?url=",
        "/photo/:/transcode?url=a&url=b",
    ] {
        assert!(classify("server-a", bad).is_none());
    }
    assert!(classify("", POSTER).is_none());
}

#[test]
fn avatar_only_ignores_roster_stamp_and_preserves_other_queries() {
    let key = classify("server-a", AVATAR).unwrap();
    assert_eq!(
        key,
        classify("server-a", &AVATAR.replace("1700000000", "1700000001")).unwrap()
    );
    assert_eq!(
        key,
        classify("server-a", &AVATAR.replace("%3Fc%3D1700000000", "")).unwrap()
    );
    assert_ne!(
        key.name,
        classify(
            "server-a",
            &AVATAR.replace("%3Fc%3D1700000000", "%3Fc%3D1700000000%26version%3D2")
        )
        .unwrap()
        .name
    );
    let other = AVATAR.replace("plex.tv", "metadata-static.plex.tv");
    assert_ne!(
        classify("server-a", &other).unwrap().name,
        classify("server-a", &other.replace("1700000000", "1700000001"))
            .unwrap()
            .name
    );
    assert!(classify("server-a", &format!("{AVATAR}&format=png"))
        .unwrap()
        .legacy
        .is_none());
    assert_eq!(
        percent_decode("plus+stays%2Bbad%zz%2"),
        "plus+stays+bad%zz%2"
    );
}

#[test]
fn more_than_a_thousand_images_survive_reopen_without_network() {
    let dir = TestDir::new();
    let cache = dir.cache(MAX_ENTRIES, MAX_BYTES);
    for id in 0..1200 {
        assert!(cache.write_at(0, &poster(id), &id.to_le_bytes()));
    }
    assert_eq!(cache.stats().entries, 1200);
    drop(cache);
    let reopened = dir.cache(MAX_ENTRIES, MAX_BYTES);
    for id in 0..1200 {
        let hit = reopened.read_at(0, &poster(id)).unwrap();
        assert_eq!(hit.bytes, id.to_le_bytes());
        assert!(!hit.stale);
    }
    let stats = reopened.stats();
    assert_eq!(stats.hit, 1200);
    assert_eq!(stats.miss, 0);
    assert_eq!(stats.write, 0);
    assert_eq!(stats.entries, 1200);
    assert_eq!(
        stats.bytes,
        1200 * (HEADER_LEN + std::mem::size_of::<usize>()) as u64
    );
}

#[test]
fn count_limit_evicts_least_recently_read_and_not_the_recent_hit() {
    let dir = TestDir::new();
    let cache = dir.cache(3, MAX_BYTES);
    for id in 0..3 {
        assert!(cache.write_at(0, &poster(id), b"data"));
    }
    assert!(cache.read_at(0, &poster(0)).is_some());
    assert!(cache.write_at(0, &poster(3), b"next"));
    assert!(cache.read_at(0, &poster(1)).is_none());
    for id in [0, 2, 3] {
        assert!(cache.read_at(0, &poster(id)).is_some());
    }
    assert_eq!(cache.stats().eviction, 1);
    assert_eq!(cache.stats().entries, 3);
}

#[test]
fn byte_limit_and_replacement_account_for_header_and_old_size() {
    let dir = TestDir::new();
    let cache = dir.cache(10, 2 * (HEADER_LEN + 20) as u64);
    assert!(cache.write_at(0, &poster(1), &[1; 20]));
    assert!(cache.write_at(0, &poster(2), &[2; 20]));
    assert!(cache.write_at(0, &poster(2), &[3; 30]));
    assert!(cache.read_at(0, &poster(1)).is_none());
    assert_eq!(cache.stats().bytes, (HEADER_LEN + 30) as u64);
    assert!(cache.write_at(0, &poster(2), &[4; 2]));
    assert_eq!(cache.stats().bytes, (HEADER_LEN + 2) as u64);
    assert!(!cache.write_at(0, &poster(3), &[0; 100]));
    assert_eq!(cache.stats().entries, 1);
    assert_eq!(cache.read_at(0, &poster(2)).unwrap().bytes, [4; 2]);
}

#[test]
fn startup_enforces_new_limits_and_recovers_orphan_temporary_files() {
    let dir = TestDir::new();
    let initial = dir.cache(20, MAX_BYTES);
    for id in 0..10 {
        assert!(initial.write_at(0, &poster(id), b"image"));
        File::options()
            .write(true)
            .open(dir.0.join(&poster(id).name))
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(100 + id as u64))
            .unwrap();
    }
    fs::write(
        dir.0.join(format!("{}.tmp123-4", poster(0).name)),
        b"partial",
    )
    .unwrap();
    drop(initial);
    let cache = dir.cache(4, 3 * (HEADER_LEN + 5) as u64);
    assert!(cache.read_at(0, &poster(9)).is_some());
    assert_eq!(cache.stats().entries, 3);
    assert_eq!(cache.stats().eviction, 7);
    assert!(cache.read_at(0, &poster(6)).is_none());
    assert!(fs::read_dir(&dir.0).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(".tmp")));
}

#[test]
fn reads_do_not_refresh_fetch_time_and_recency_survives_reopen() {
    let dir = TestDir::new();
    let cache = dir.cache(2, MAX_BYTES);
    let old = unix_seconds(SystemTime::now()) - REFRESH_AFTER.as_secs() - 1;
    {
        let mut index = cache.index.lock().unwrap();
        cache.initialize(&mut index);
        for id in 0..2 {
            assert!(cache.write_locked(&mut index, &poster(id), b"data", old));
            let stamp = UNIX_EPOCH + Duration::from_secs(old + id as u64);
            File::options()
                .write(true)
                .open(dir.0.join(&poster(id).name))
                .unwrap()
                .set_modified(stamp)
                .unwrap();
            index.entries.get_mut(&poster(id).name).unwrap().touched = stamp;
        }
    }
    assert!(cache.read_at(0, &poster(0)).unwrap().stale);
    assert_eq!(
        read_file(&dir.0.join(&poster(0).name), false).unwrap().1,
        old
    );
    let touched = fs::metadata(dir.0.join(&poster(0).name))
        .unwrap()
        .modified()
        .unwrap();
    assert!(touched > UNIX_EPOCH + Duration::from_secs(old));
    assert!(cache.read_at(0, &poster(0)).unwrap().stale);
    assert_eq!(
        fs::metadata(dir.0.join(&poster(0).name))
            .unwrap()
            .modified()
            .unwrap(),
        touched,
        "do not write metadata on every hit"
    );
    drop(cache);
    let reopened = dir.cache(2, MAX_BYTES);
    assert!(reopened.write_at(0, &poster(2), b"new"));
    assert!(reopened.read_at(0, &poster(1)).is_none());
    assert!(reopened.read_at(0, &poster(0)).unwrap().stale);
}

#[test]
fn corrupt_empty_oversized_and_truncated_files_are_misses() {
    let dir = TestDir::new();
    let cache = dir.cache(10, MAX_BYTES);
    assert!(!cache.write_at(0, &poster(0), b""));
    assert!(!cache.write_at(0, &poster(0), &vec![0; MAX_FILE + 1]));
    for id in 0..4 {
        assert!(cache.write_at(0, &poster(id), b"some bytes"));
    }
    fs::write(dir.0.join(&poster(0).name), b"not a valid header").unwrap();
    File::options()
        .write(true)
        .open(dir.0.join(&poster(1).name))
        .unwrap()
        .set_len((MAX_FILE + HEADER_LEN + 1) as u64)
        .unwrap();
    File::options()
        .write(true)
        .open(dir.0.join(&poster(2).name))
        .unwrap()
        .set_len(0)
        .unwrap();
    File::options()
        .write(true)
        .open(dir.0.join(&poster(3).name))
        .unwrap()
        .set_len((HEADER_LEN + 1) as u64)
        .unwrap();
    for id in 0..4 {
        assert!(cache.read_at(0, &poster(id)).is_none());
    }
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(cache.stats().bytes, 0);
    // Oversized residue present before initialization is bounded and removed too.
    File::create(dir.0.join(&poster(5).name))
        .unwrap()
        .set_len((MAX_FILE * 4) as u64)
        .unwrap();
    let reopened = dir.cache(10, MAX_BYTES);
    assert!(reopened.read_at(0, &poster(5)).is_none());
    assert_eq!(reopened.stats().entries, 0);
}

#[test]
fn legacy_avatar_migrates_without_resetting_refresh_clock() {
    let dir = TestDir::new();
    let key = classify("server-a", AVATAR).unwrap();
    let legacy = dir.0.join(key.legacy.as_ref().unwrap());
    fs::write(&legacy, b"legacy image").unwrap();
    File::options()
        .write(true)
        .open(&legacy)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(100))
        .unwrap();
    let cache = dir.cache(100, MAX_BYTES);
    let hit = cache.read_at(0, &key).unwrap();
    assert_eq!(hit.bytes, b"legacy image");
    assert!(hit.stale);
    assert!(!legacy.exists());
    assert!(dir.0.join(&key.name).exists());
    assert_eq!(read_file(&dir.0.join(&key.name), false).unwrap().1, 100);
    assert_eq!(cache.stats().entries, 1);
}

#[test]
fn clear_scrubs_all_candidates_and_rejects_old_reads_writes_and_removes() {
    let primary = TestDir::new();
    let fallback = TestDir::new();
    let cache = Cache::new(
        vec![primary.0.clone(), fallback.0.clone()],
        Limits {
            entries: 10,
            bytes: MAX_BYTES,
        },
    );
    assert!(cache.write_at(0, &poster(0), b"old account"));
    fs::write(fallback.0.join(&poster(1).name), b"other location").unwrap();
    fs::write(fallback.0.join("avatar-old.img.tmp123"), b"partial").unwrap();
    fs::write(fallback.0.join("keep.txt"), b"unrelated").unwrap();
    cache.clear();
    assert_eq!(cache.generation.load(Ordering::Acquire), 1);
    assert!(!fallback.0.join(&poster(1).name).exists());
    assert!(!fallback.0.join("avatar-old.img.tmp123").exists());
    assert!(fallback.0.join("keep.txt").exists());
    assert!(!cache.write_at(0, &poster(0), b"late fetch"));
    assert!(cache.write_at(1, &poster(0), b"new account"));
    assert!(cache.read_at(0, &poster(0)).is_none());
    cache.remove_at(0, &poster(0));
    assert_eq!(cache.read_at(1, &poster(0)).unwrap().bytes, b"new account");
}

#[test]
fn concurrent_clear_waits_for_gate_and_late_write_is_rejected() {
    let dir = TestDir::new();
    let cache = std::sync::Arc::new(dir.cache(10, MAX_BYTES));
    assert!(cache.write_at(0, &poster(0), b"old"));
    let held = cache.index.lock().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let worker_cache = cache.clone();
    let clear = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        worker_cache.clear();
    });
    started_rx.recv().unwrap();
    assert_eq!(cache.generation.load(Ordering::Acquire), 0);
    drop(held);
    clear.join().unwrap();
    assert!(!cache.write_at(0, &poster(0), b"late"));
    assert_eq!(cache.stats().entries, 0);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn failed_atomic_write_keeps_previous_entry_and_accurate_accounting() {
    let dir = TestDir::new();
    let cache = dir.cache(10, MAX_BYTES);
    assert!(cache.write_at(0, &poster(0), b"original"));
    let before = cache.stats();
    fs::create_dir(dir.0.join(format!("{}.tmp", poster(0).name))).unwrap();
    assert!(!cache.write_at(0, &poster(0), b"replacement"));
    assert_eq!(cache.stats().bytes, before.bytes);
    assert_eq!(cache.stats().entries, before.entries);
    assert_eq!(cache.stats().write, before.write);
    assert_eq!(cache.read_at(0, &poster(0)).unwrap().bytes, b"original");
}

#[test]
fn unavailable_storage_is_harmless_and_stats_do_not_initialize_it() {
    let dir = TestDir::new();
    let file = dir.0.join("regular-file");
    fs::write(&file, b"cannot create directory here").unwrap();
    let cache = Cache::new(
        vec![file],
        Limits {
            entries: 10,
            bytes: MAX_BYTES,
        },
    );
    assert_eq!(cache.stats().entries, 0);
    assert!(!cache.index.lock().unwrap().initialized);
    assert!(cache.read_at(0, &poster(0)).is_none());
    assert!(!cache.write_at(0, &poster(0), b"image"));
    assert_eq!(cache.stats().miss, 1);
}
