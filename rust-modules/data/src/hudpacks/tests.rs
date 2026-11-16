//! `HudStore` tests: events in, effects out. `Rig` is a runner over an in-memory disk: it executes the
//! store's effects the way `app/adapters/huds.rs`'s runners do and feeds each answer back as the event it is, so a
//! test reads the effects in the order the store pushed them. A pack here is the text
//! `id@version[#flag]`; the rig's validator reads it the way the appkit validator reads the real
//! format (`#newer` is a file for a newer PlxNative, `#bad` does not validate).

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use super::*;
use serde_json::Value;

const MAX: usize = 1024;

fn root(n: usize) -> PathBuf {
    PathBuf::from(format!("/c{n}"))
}

/// Three candidates: two ordinary and a volatile last.
fn setup() -> Setup {
    Setup {
        candidates: (0..3).map(|n| Candidate { dir: root(n), volatile: n == 2 }).collect(),
        max_pack_bytes: MAX,
    }
}

fn pack(id: &str, version: &str) -> Vec<u8> {
    format!("{id}@{version}").into_bytes()
}

fn flagged(id: &str, version: &str, flag: &str) -> Vec<u8> {
    format!("{id}@{version}#{flag}").into_bytes()
}

fn validator(bytes: &[u8]) -> Verdict {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let (head, flag) = text.split_once('#').unwrap_or((text.as_str(), ""));
    if flag == "bad" {
        return Verdict::Invalid;
    }
    let Some((id, version)) = head.split_once('@') else { return Verdict::Invalid };
    let meta = Meta {
        id: id.to_string(),
        name: format!("{id} name"),
        version: version.to_string(),
        package_format: "1.0".to_string(), hud_format: "1.0".to_string(),
    };
    match flag {
        "newer" => Verdict::Unloadable(meta, Unloadable::NewerFormat),
        _ => Verdict::Loadable(meta),
    }
}

fn pack_path(n: usize, sha: Sha) -> PathBuf {
    root(n).join(pack_rel(sha))
}

fn index_path(n: usize) -> PathBuf {
    root(n).join(INDEX_FILE)
}

fn record_at(sha: Sha, source: Source, at: u64) -> Record {
    Record::new(sha, source, at, "1.0", "1.0")
}

fn record(bytes: &[u8], source: Source) -> Record {
    record_at(Sha::of(bytes), source, 7)
}

struct Rig {
    m: HudStore,
    files: BTreeMap<PathBuf, Vec<u8>>,
    /// Writes to these paths, or under these roots, are refused.
    refuse: Vec<PathBuf>,
    /// Reads of these paths fail (they exist, but cannot be read).
    unreadable: Vec<PathBuf>,
    /// The next effect this matches is sent and its answer never comes (the runner lost it); the flag
    /// says whether a write reached the disk before that. Nothing after it in the queue runs.
    lose: Option<(fn(&HudEffect) -> bool, bool)>,
    log: Vec<HudEffect>,
    notices: Vec<Notice>,
    selections: Vec<String>,
    /// What the network answers by URL; a URL not listed is `Unreachable`. Every request is logged.
    network: BTreeMap<String, Result<Vec<u8>, FetchError>>,
}

impl Rig {
    fn new() -> Self {
        Rig {
            m: HudStore::new(),
            files: BTreeMap::new(),
            refuse: Vec::new(),
            unreadable: Vec::new(),
            lose: None,
            log: Vec::new(),
            notices: Vec::new(),
            selections: Vec::new(),
            network: BTreeMap::new(),
        }
    }

    /// A rig whose disk already holds these packs, each installed in the listed candidates.
    fn with(installed: &[(&[u8], Source, &[usize])]) -> Self {
        let mut rig = Rig::new();
        let mut indexes: BTreeMap<usize, Index> = BTreeMap::new();
        for (bytes, source, dirs) in installed {
            let sha = Sha::of(bytes);
            let id = match validator(bytes) {
                Verdict::Loadable(m) | Verdict::Unloadable(m, _) => m.id,
                Verdict::Invalid => "x.invalid".to_string(),
            };
            for dir in *dirs {
                rig.files.insert(pack_path(*dir, sha), bytes.to_vec());
                indexes.entry(*dir).or_default().insert(id.clone(), record(bytes, *source));
            }
        }
        for (dir, index) in indexes {
            rig.files.insert(index_path(dir), index_bytes(&index).to_vec());
        }
        rig
    }

    fn refresh(&mut self) {
        self.refresh_selecting("");
    }

    fn refresh_selecting(&mut self, selected: &str) {
        let handled = self.send(HudEvent::Refresh { setup: setup(), selected: selected.to_string() });
        assert_eq!(handled, Handled::Yes);
        assert!(!self.m.busy(), "the rig answers every request, so a listing ends");
    }

    fn install(&mut self, bytes: &[u8], source: Source, now: u64) {
        assert_eq!(self.send(HudEvent::Install { bytes: bytes.to_vec(), source, now }), Handled::Yes);
        assert!(!self.m.busy());
    }

    fn send(&mut self, ev: HudEvent) -> Handled {
        let mut fx = Vec::new();
        let handled = self.m.step(ev, &mut fx);
        self.run(fx);
        handled
    }

    /// Execute effects in order and feed the answers back until nothing is asked.
    fn run(&mut self, fx: Vec<HudEffect>) {
        let mut queue: VecDeque<HudEffect> = fx.into();
        while let Some(effect) = queue.pop_front() {
            self.log.push(effect.clone());
            if let Some((matches, landed)) = self.lose {
                if matches(&effect) {
                    self.lose = None;
                    if let (true, HudEffect::WriteFile { at, bytes, .. }) = (landed, &effect) {
                        self.files.insert(at.path(), bytes.to_vec());
                    }
                    queue.clear();
                    continue;
                }
            }
            let answer = match effect {
                HudEffect::Catalog(_) | HudEffect::FetchRegistry { .. } => panic!("registry work requires the catalog rig"),
                HudEffect::ScanDir { tag, at } => {
                    let dir = at.path();
                    let names = self
                        .files
                        .keys()
                        .filter(|p| p.parent() == Some(dir.as_path()))
                        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .collect();
                    Some(HudEvent::Scanned { tag, result: Ok(names) })
                }
                HudEffect::ReadFile { tag, at, max_bytes } => {
                    let result = match self.files.get(&at.path()) {
                        _ if self.unreadable.contains(&at.path()) => Err(ReadError::Failed),
                        None => Err(ReadError::Missing),
                        Some(bytes) if bytes.len() > max_bytes => Err(ReadError::TooLarge),
                        Some(bytes) => Ok(bytes.clone()),
                    };
                    Some(HudEvent::Read { tag, result })
                }
                HudEffect::WriteFile { tag, at, bytes } => {
                    let path = at.path();
                    let outcome = if self.refuse.contains(&path) || self.refuse.contains(&at.root) {
                        WriteOutcome::Refused
                    } else {
                        self.files.insert(path, bytes.to_vec());
                        WriteOutcome::Done
                    };
                    Some(HudEvent::Written { tag, outcome })
                }
                HudEffect::Delete { at } => {
                    self.files.remove(&at.path());
                    None
                }
                HudEffect::Validate { tag, sha, bytes } => {
                    Some(HudEvent::Validated { tag, sha, verdict: validator(&bytes) })
                }
                HudEffect::Fetch { tag, address, max_bytes } => {
                    assert_eq!(max_bytes, MAX, "the fetch is bounded by the store's package limit");
                    let result = self.network.get(address.url()).cloned().unwrap_or(Err(FetchError::Unreachable));
                    Some(HudEvent::Fetched { tag, result })
                }
                HudEffect::Selection { id } => {
                    self.selections.push(id);
                    None
                }
                HudEffect::Notice(notice) => {
                    self.notices.push(notice);
                    None
                }
            };
            if let Some(ev) = answer {
                let mut more = Vec::new();
                let handled = self.m.step(ev, &mut more);
                assert_eq!(handled, Handled::Yes, "the store asked for this answer");
                queue.extend(more);
            }
        }
    }

    /// The log as short lines, from the first effect after `mark`.
    fn lines(&self, mark: usize) -> Vec<String> {
        self.log[mark..].iter().map(line).collect()
    }

    fn index_of(&self, dir: usize) -> Index {
        parse_index(self.files.get(&index_path(dir)).map_or(&b""[..], |b| b.as_slice())).index
    }

    fn ids(&self) -> Vec<String> {
        self.m.entries().into_iter().map(|e| e.id).collect()
    }

    fn entry(&self, id: &str) -> Entry {
        self.m.entries().into_iter().find(|e| e.id == id).unwrap_or_else(|| panic!("{id} is not listed"))
    }
}

fn line(effect: &HudEffect) -> String {
    match effect {
        HudEffect::Catalog(e) => format!("catalog {e:?}"),
        HudEffect::FetchRegistry { package, .. } => format!("registry {}", package.sha256),
        HudEffect::ScanDir { at, .. } => format!("scan {}", at.path().display()),
        HudEffect::ReadFile { at, .. } => format!("read {}", at.path().display()),
        HudEffect::WriteFile { at, .. } => format!("write {}", at.path().display()),
        HudEffect::Delete { at } => format!("delete {}", at.path().display()),
        HudEffect::Validate { sha, .. } => format!("validate {}", sha.hex()),
        HudEffect::Fetch { address, .. } => format!("fetch {}", address.url()),
        HudEffect::Selection { id } => format!("select {id}"),
        HudEffect::Notice(n) => format!("notice {n:?}"),
    }
}

/// A store that has listed an empty disk and is idle.
fn listed() -> HudStore {
    let mut r = Rig::new();
    r.refresh();
    r.m
}

/// Step once and return the effects, for the tests that read a single exchange.
fn step(m: &mut HudStore, ev: HudEvent) -> (Handled, Vec<HudEffect>) {
    let mut fx = Vec::new();
    let handled = m.step(ev, &mut fx);
    (handled, fx)
}

fn tag_of(effect: &HudEffect) -> Tag {
    match effect {
        HudEffect::ScanDir { tag, .. }
        | HudEffect::ReadFile { tag, .. }
        | HudEffect::WriteFile { tag, .. }
        | HudEffect::Fetch { tag, .. }
        | HudEffect::Validate { tag, .. } => *tag,
        other @ (HudEffect::Catalog(_) | HudEffect::FetchRegistry { .. } | HudEffect::Delete { .. } | HudEffect::Selection { .. } | HudEffect::Notice(_)) => panic!("{other:?} carries no tag"),
    }
}

fn is_validate(effect: &HudEffect) -> bool {
    matches!(effect, HudEffect::Validate { .. })
}

fn is_pack_write(effect: &HudEffect) -> bool {
    matches!(effect, HudEffect::WriteFile { at, .. } if at.rel.starts_with("packs/"))
}

fn is_index_write(effect: &HudEffect) -> bool {
    matches!(effect, HudEffect::WriteFile { at, .. } if at.rel == INDEX_FILE)
}

fn is_drop_read(effect: &HudEffect) -> bool {
    matches!(effect, HudEffect::ReadFile { at, .. } if at.rel.starts_with("drop/"))
}

/// A rig whose disk holds `installed`, listed, with an install of `bytes` started and the answer to
/// the request `lose` matches lost. The store is left busy, waiting for it.
fn stalled_install(
    installed: &[(&[u8], Source, &[usize])],
    bytes: &[u8],
    lose: fn(&HudEffect) -> bool,
    landed: bool,
) -> Rig {
    let mut r = Rig::with(installed);
    r.refresh();
    r.lose = Some((lose, landed));
    assert_eq!(r.send(HudEvent::Install { bytes: bytes.to_vec(), source: Source::Address, now: 9 }), Handled::Yes);
    assert!(r.m.busy(), "the answer never came");
    r
}

// ---- the behaviours the PR is for ----

/// The pack is written first and `installed.json` second, and nothing is listed until the second
/// write landed: the index is the switch.
#[test]
fn install_writes_the_pack_then_the_index() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    // One store driven by hand, so every exchange is read as it happens.
    let mut m2 = listed();
    let (handled, fx) = step(&mut m2, HudEvent::Install { bytes: bytes.clone(), source: Source::Address, now: 100 });
    assert_eq!(handled, Handled::Yes);
    assert_eq!(fx.len(), 1);
    assert!(matches!(&fx[0], HudEffect::Validate { sha: s, .. } if *s == sha));

    let verdict = validator(&bytes);
    let (_, fx) = step(&mut m2, HudEvent::Validated { tag: tag_of(&fx[0]), sha, verdict });
    assert_eq!(fx.iter().map(line).collect::<Vec<_>>(), [format!("write {}", pack_path(0, sha).display())]);
    assert_eq!(m2.entries().len(), 1, "the pack alone lists nothing");

    let (_, fx) = step(&mut m2, HudEvent::Written { tag: tag_of(&fx[0]), outcome: WriteOutcome::Done });
    assert_eq!(fx.iter().map(line).collect::<Vec<_>>(), [format!("write {}", index_path(0).display())]);
    let HudEffect::WriteFile { bytes: index, .. } = &fx[0] else { panic!("{fx:?}") };
    let written = parse_index(index).index;
    assert_eq!(written["org.example.hud"], record_at(sha, Source::Address, 100));
    assert_eq!(m2.entries().len(), 1, "the index is not down yet");

    let (_, fx) = step(&mut m2, HudEvent::Written { tag: tag_of(&fx[0]), outcome: WriteOutcome::Done });
    assert_eq!(
        fx,
        [HudEffect::Notice(Notice::Installed {
            id: "org.example.hud".into(),
            version: "1.0.0".into(),
            sha,
            until_restart: false,
        })]
    );
    assert_eq!(m2.entries().len(), 2);
    assert!(!m2.busy());
}

/// The same flow through the rig, as the whole effect list: validate, pack, index, notice.
#[test]
fn an_install_asks_validate_pack_index_in_that_order() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let mut r = Rig::new();
    r.refresh();
    let mark = r.log.len();
    r.install(&bytes, Source::Address, 100);
    let lines = r.lines(mark);
    assert_eq!(lines[0], format!("validate {}", sha.hex()));
    assert_eq!(lines[1], format!("write {}", pack_path(0, sha).display()));
    assert_eq!(lines[2], format!("write {}", index_path(0).display()));
    assert!(lines[3].starts_with("notice Installed"), "{lines:?}");
    assert_eq!(lines.len(), 4);
    assert_eq!(r.files[&pack_path(0, sha)], bytes);
    assert_eq!(r.index_of(0)["org.example.hud"], record_at(sha, Source::Address, 100));
}

/// A candidate that refuses the pack, or the index, is passed over; the first that takes both is used.
#[test]
fn a_refused_write_moves_to_the_next_candidate() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);

    let mut r = Rig::new();
    r.refuse.push(root(0));
    r.refresh();
    let mark = r.log.len();
    r.install(&bytes, Source::Drop, 5);
    let writes: Vec<_> = r.lines(mark).into_iter().filter(|l| l.starts_with("write")).collect();
    assert_eq!(
        writes,
        [
            format!("write {}", pack_path(0, sha).display()),
            format!("write {}", pack_path(1, sha).display()),
            format!("write {}", index_path(1).display()),
        ]
    );
    assert!(r.files.contains_key(&pack_path(1, sha)) && !r.files.contains_key(&pack_path(0, sha)));
    assert!(matches!(r.notices.last(), Some(Notice::Installed { until_restart: false, .. })));

    // The pack went down but the index was refused: the pack is taken back, and the next one is tried.
    let mut r = Rig::new();
    r.refuse.push(index_path(0));
    r.refresh();
    r.install(&bytes, Source::Drop, 5);
    assert!(!r.files.contains_key(&pack_path(0, sha)), "an unlisted pack is deleted");
    assert!(r.files.contains_key(&pack_path(1, sha)) && r.files.contains_key(&index_path(1)));
    assert!(r.m.entries().iter().any(|e| e.id == "org.example.hud"));
}

/// Only the last, volatile candidate marks a package "until restart", and only when no earlier one
/// took it; with every candidate refusing, nothing is installed.
#[test]
fn only_the_volatile_candidate_gives_until_restart() {
    let bytes = pack("org.example.hud", "1.0.0");

    let mut r = Rig::new();
    r.refresh();
    r.install(&bytes, Source::Address, 1);
    assert!(!r.entry("org.example.hud").until_restart);

    // A later candidate is not volatile for being later: the second persistent directory holds it for good.
    let mut r = Rig::new();
    r.refuse.extend([root(0)]);
    r.refresh();
    r.install(&bytes, Source::Address, 1);
    assert!(matches!(r.notices.last(), Some(Notice::Installed { until_restart: false, .. })));
    assert!(r.files.contains_key(&pack_path(1, Sha::of(&bytes))));
    assert!(!r.entry("org.example.hud").until_restart);

    let mut r = Rig::new();
    r.refuse.extend([root(0), root(1)]);
    r.refresh();
    r.install(&bytes, Source::Address, 1);
    assert!(matches!(r.notices.last(), Some(Notice::Installed { until_restart: true, .. })));
    assert!(r.entry("org.example.hud").until_restart);
    // Read back from the disk, it is still the volatile candidate that holds it.
    r.refresh();
    assert!(r.entry("org.example.hud").until_restart);

    let mut r = Rig::new();
    r.refuse.extend([root(0), root(1), root(2)]);
    r.refresh();
    r.install(&bytes, Source::Address, 1);
    assert_eq!(r.notices.last(), Some(&Notice::NotWritten { sha: Some(Sha::of(&bytes)) }));
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
    assert!(r.files.is_empty());
}

/// One sha in two candidates is one package, read and validated once; a corrupt copy falls through to
/// the next.
#[test]
fn the_same_sha_in_two_directories_lists_once() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let mut r = Rig::with(&[(&bytes, Source::Address, &[0, 2])]);
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.hud"]);
    let reads = r.log.iter().filter(|e| matches!(e, HudEffect::ReadFile { at, .. } if at.rel.starts_with(PACKS_DIR))).count();
    let validations = r.log.iter().filter(|e| matches!(e, HudEffect::Validate { sha: s, .. } if *s == sha)).count();
    assert_eq!((reads, validations), (1, 1));
    assert!(!r.entry("org.example.hud").until_restart, "the first candidate holds it");

    // Candidate 0's copy is damaged: the volatile one is read instead, and says so.
    r.files.insert(pack_path(0, sha), b"damaged".to_vec());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.hud"]);
    assert!(r.entry("org.example.hud").until_restart);
}

/// `builtin:classic` is refused from every source that is not compiled in, and the built-in stays
/// listed and selectable.
#[test]
fn a_drop_file_with_the_builtin_id_is_refused_and_the_builtin_stays_selectable() {
    let bytes = pack(BUILTIN_CLASSIC, "1.0.0");
    let sha = Sha::of(&bytes);
    let mut r = Rig::new();
    r.files.insert(root(0).join("drop/mine.plxhud"), bytes.clone());
    r.refresh();
    let drops = r.m.drops();
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].status, Err(Refusal::BuiltinId));

    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::InstallDrop { sha, now: 1 }), Handled::Yes);
    assert_eq!(r.notices.last(), Some(&Notice::Refused { sha, reason: Refusal::BuiltinId }));
    assert!(r.lines(mark).iter().all(|l| !l.starts_with("write")), "{:?}", r.lines(mark));
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);

    // Bytes from an address are refused the same way, and an index that claims it lists nothing.
    r.install(&bytes, Source::Address, 2);
    assert_eq!(r.notices.last(), Some(&Notice::Refused { sha, reason: Refusal::BuiltinId }));
    let mut index = Index::new();
    index.insert(BUILTIN_CLASSIC.to_string(), record(&bytes, Source::Registry));
    r.files.insert(index_path(1), index_bytes(&index).to_vec());
    r.files.insert(pack_path(1, sha), bytes.clone());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
    assert_eq!(r.entry(BUILTIN_CLASSIC).trust, Trust::BuiltIn);

    // The built-in is selectable (and so is "nothing picked").
    assert_eq!(r.send(HudEvent::Selected { id: BUILTIN_CLASSIC.to_string() }), Handled::Yes);
    assert_eq!(r.selections, [BUILTIN_CLASSIC]);
    assert!(r.entry(BUILTIN_CLASSIC).selected);
    assert_eq!(r.m.resolved(), BUILTIN_CLASSIC);
}

/// A strictly higher version of an id replaces the installed one (its pack is deleted); a lower or an
/// equal one does not, and the same bytes again are "already installed".
#[test]
fn a_higher_version_replaces_the_lower_one_and_a_lower_one_does_not() {
    let v100 = pack("org.example.hud", "1.0.0");
    let v110 = pack("org.example.hud", "1.1.0");
    let v105 = pack("org.example.hud", "1.0.5");
    let v110b = flagged("org.example.hud", "1.1.0", "other");
    let mut r = Rig::new();
    r.refresh();
    r.install(&v100, Source::Address, 1);
    assert_eq!(r.entry("org.example.hud").meta.unwrap().version, "1.0.0");

    let mark = r.log.len();
    r.install(&v110, Source::Address, 2);
    assert_eq!(r.entry("org.example.hud").meta.unwrap().version, "1.1.0");
    assert_eq!(r.ids().len(), 2, "one row per id");
    assert!(r.lines(mark).contains(&format!("delete {}", pack_path(0, Sha::of(&v100)).display())));
    assert!(!r.files.contains_key(&pack_path(0, Sha::of(&v100))));
    assert_eq!(r.index_of(0).len(), 1);

    for (bytes, reason) in [
        (&v105, Refusal::NotNewer),
        (&v100, Refusal::NotNewer),
        (&v110b, Refusal::NotNewer),
        (&v110, Refusal::AlreadyInstalled),
    ] {
        let mark = r.log.len();
        r.install(bytes, Source::Address, 3);
        assert_eq!(r.notices.last(), Some(&Notice::Refused { sha: Sha::of(bytes), reason }));
        assert!(r.lines(mark).iter().all(|l| !l.starts_with("write")), "{:?}", r.lines(mark));
        assert_eq!(r.entry("org.example.hud").meta.unwrap().version, "1.1.0");
    }

    // Versions order by number, not by text.
    let v19 = pack("org.example.other", "1.9.0");
    let v110 = pack("org.example.other", "1.10.0");
    r.install(&v19, Source::Address, 4);
    r.install(&v110, Source::Address, 5);
    assert_eq!(r.entry("org.example.other").meta.unwrap().version, "1.10.0");
}

/// Removing the selected package selects the built-in, and says so for the profile to store.
#[test]
fn removing_the_selected_package_selects_the_builtin() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let other = pack("org.example.other", "2.0.0");
    let mut r = Rig::new();
    r.refresh();
    r.install(&bytes, Source::Address, 1);
    r.install(&other, Source::Address, 1);
    assert_eq!(r.send(HudEvent::Selected { id: "org.example.hud".into() }), Handled::Yes);
    assert_eq!((r.m.selected(), r.m.resolved()), ("org.example.hud", "org.example.hud"));
    assert_eq!(r.selections, ["org.example.hud"]);

    // Removing one that is not selected leaves the selection alone.
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.other".into() }), Handled::Yes);
    assert_eq!(r.m.selected(), "org.example.hud");
    assert_eq!(r.selections, ["org.example.hud"]);

    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.hud".into() }), Handled::Yes);
    assert_eq!(r.m.selected(), BUILTIN_CLASSIC);
    assert_eq!(r.selections, ["org.example.hud", BUILTIN_CLASSIC]);
    assert_eq!(r.notices.last(), Some(&Notice::Removed { id: "org.example.hud".into() }));
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
    assert!(r.entry(BUILTIN_CLASSIC).selected);
    let lines = r.lines(mark);
    assert_eq!(lines[0], format!("write {}", index_path(0).display()));
    assert_eq!(lines[1], format!("delete {}", pack_path(0, sha).display()));
    assert!(r.index_of(0).is_empty());

    // The built-in and an unknown id are not removable.
    assert_eq!(r.send(HudEvent::Removed { id: BUILTIN_CLASSIC.into() }), Handled::No);
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.hud".into() }), Handled::No);
}

/// A removal rewrites every candidate that lists the id, and one it could not rewrite is reported.
#[test]
fn removal_clears_every_candidate_and_reports_one_it_could_not() {
    let bytes = pack("org.example.hud", "1.0.0");
    let mut r = Rig::with(&[(&bytes, Source::Drop, &[0, 1])]);
    r.refresh();
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.hud".into() }), Handled::Yes);
    assert!(r.index_of(0).is_empty() && r.index_of(1).is_empty());
    assert_eq!(r.notices.last(), Some(&Notice::Removed { id: "org.example.hud".into() }));

    let mut r = Rig::with(&[(&bytes, Source::Drop, &[0, 1])]);
    r.refuse.push(index_path(1));
    r.refresh();
    r.send(HudEvent::Selected { id: "org.example.hud".into() });
    r.send(HudEvent::Removed { id: "org.example.hud".into() });
    assert_eq!(r.notices.last(), Some(&Notice::RemoveIncomplete { id: "org.example.hud".into() }));
    assert!(!r.index_of(1).is_empty(), "the refusing candidate keeps its entry");
    assert_eq!(r.m.selected(), "org.example.hud", "it is still listed, so it is still the selection");
}

/// A verdict that fails is not installable: nothing is written and nothing is listed.
#[test]
fn a_validated_with_a_failing_verdict_is_not_installable() {
    let bytes = b"anything".to_vec();
    let sha = Sha::of(&bytes);
    let meta = Meta { id: "org.example.hud".into(), name: "n".into(), version: "1.0.0".into(), package_format: "1.0".into(), hud_format: "1.0".into() };
    for (verdict, reason) in [
        (Verdict::Invalid, Refusal::Invalid),
        (Verdict::Unloadable(meta.clone(), Unloadable::NewerFormat), Refusal::Unloadable(Unloadable::NewerFormat)),
        (Verdict::Unloadable(meta.clone(), Unloadable::OlderFormat), Refusal::Unloadable(Unloadable::OlderFormat)),
        (Verdict::Unloadable(meta.clone(), Unloadable::AppRange), Refusal::Unloadable(Unloadable::AppRange)),
        (Verdict::Loadable(Meta { version: "1.0".into(), ..meta.clone() }), Refusal::Invalid),
    ] {
        let mut m = listed();
        let (_, fx) = step(&mut m, HudEvent::Install { bytes: bytes.clone(), source: Source::Address, now: 1 });
        let (handled, fx) = step(&mut m, HudEvent::Validated { tag: tag_of(&fx[0]), sha, verdict });
        assert_eq!(handled, Handled::Yes);
        assert_eq!(fx, [HudEffect::Notice(Notice::Refused { sha, reason })]);
        assert_eq!(m.entries().len(), 1);
        assert!(!m.busy());
    }

    // Through the rig: a file that does not validate and one for a newer app.
    let mut r = Rig::new();
    r.refresh();
    let mark = r.log.len();
    r.install(&flagged("org.example.hud", "1.0.0", "bad"), Source::Address, 1);
    r.install(&flagged("org.example.hud", "1.0.0", "newer"), Source::Address, 1);
    assert!(r.lines(mark).iter().all(|l| !l.starts_with("write")));
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
}

// ---- what a listing reads, and what it trusts ----

/// Every candidate's index is read first, in order; a pack is read only once its sha is known.
#[test]
fn a_listing_reads_each_index_then_each_pack_then_the_drop_folders() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let mut r = Rig::with(&[(&a, Source::Address, &[0]), (&b, Source::Registry, &[1])]);
    r.refresh();
    let lines = r.lines(0);
    let at = |needle: String| lines.iter().position(|l| *l == needle).unwrap_or_else(|| panic!("{needle} in {lines:?}"));
    let (i0, i1, i2) = (
        at(format!("read {}", index_path(0).display())),
        at(format!("read {}", index_path(1).display())),
        at(format!("read {}", index_path(2).display())),
    );
    assert_eq!((i0 + 1, i1 + 1), (i1, i2), "the three indexes come first, in candidate order");
    let packs = [Sha::of(&a), Sha::of(&b)].map(|s| s.hex());
    for sha in packs {
        assert!(at(format!("validate {sha}")) > i2);
    }
    let scan0 = at(format!("scan {}", root(0).join(DROP_DIR).display()));
    assert!(scan0 > at(format!("validate {}", Sha::of(&b).hex())));
    assert_eq!(r.entry("org.example.a").trust, Trust::Unreviewed);
    assert_eq!(r.entry("org.example.b").trust, Trust::Registry);
}

/// An unreviewed source is never promoted: a drop and an address are Unreviewed, and only a source
/// that says registry is Registry.
#[test]
fn trust_follows_the_source_and_only_the_registry_is_reviewed() {
    assert_eq!(Source::Drop.trust(), Trust::Unreviewed);
    assert_eq!(Source::Address.trust(), Trust::Unreviewed);
    assert_eq!(Source::Registry.trust(), Trust::Registry);
    let mut r = Rig::new();
    r.refresh();
    r.install(&pack("org.example.a", "1.0.0"), Source::Address, 1);
    r.install(&pack("org.example.b", "1.0.0"), Source::Registry, 1);
    assert_eq!(r.entry("org.example.a").trust, Trust::Unreviewed);
    assert_eq!(r.entry("org.example.b").trust, Trust::Registry);
    assert_eq!(r.entry("org.example.a").source, Some(Source::Address));
}

/// A pack that no longer loads (an app update dropped its format) stays listed and marked, and cannot
/// be selected; a selection that names it plays the built-in.
#[test]
fn a_pack_on_disk_that_no_longer_loads_is_listed_but_not_selectable() {
    let bytes = flagged("org.example.old", "1.0.0", "newer");
    let mut r = Rig::with(&[(&bytes, Source::Address, &[0])]);
    r.refresh_selecting("org.example.old");
    assert_eq!(r.entry("org.example.old").loadable, Err(Unloadable::NewerFormat));
    assert_eq!(r.m.selected(), "org.example.old");
    assert_eq!(r.m.resolved(), BUILTIN_CLASSIC);
    assert_eq!(r.send(HudEvent::Selected { id: "org.example.old".into() }), Handled::No);
    assert!(r.selections.is_empty());
}

/// A pack is trusted by its content, not its name or its index: a file whose bytes do not hash to its
/// name, an index entry naming another id than the pack declares, and an entry with no file are not
/// packages.
#[test]
fn a_pack_is_listed_only_if_its_name_its_index_entry_and_its_bytes_agree() {
    let good = pack("org.example.good", "1.0.0");
    let liar = pack("org.example.liar", "1.0.0");
    let mut r = Rig::with(&[(&good, Source::Address, &[0]), (&liar, Source::Address, &[1])]);
    // The liar's index entry says another id.
    let mut index = Index::new();
    index.insert("org.example.claimed".to_string(), record(&liar, Source::Address));
    r.files.insert(index_path(1), index_bytes(&index).to_vec());
    // A third entry whose file is gone.
    let gone = pack("org.example.gone", "1.0.0");
    let mut index = r.index_of(0);
    index.insert("org.example.gone".to_string(), record(&gone, Source::Address));
    r.files.insert(index_path(0), index_bytes(&index).to_vec());
    // The good pack's file is swapped for other bytes under its name.
    let swapped = pack("org.example.swapped", "1.0.0");
    r.files.insert(pack_path(2, Sha::of(&swapped)), pack("org.example.other", "9.9.9"));
    let mut index = Index::new();
    index.insert("org.example.swapped".to_string(), record(&swapped, Source::Address));
    r.files.insert(index_path(2), index_bytes(&index).to_vec());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.good"]);
}

/// One id in two candidates: the higher version wins, and on a tie the earlier candidate.
#[test]
fn across_candidates_the_higher_version_wins_and_a_tie_goes_to_the_earlier() {
    let old = pack("org.example.hud", "1.0.0");
    let new = pack("org.example.hud", "1.2.0");
    let mut r = Rig::with(&[(&old, Source::Address, &[0]), (&new, Source::Drop, &[1])]);
    r.refresh();
    assert_eq!(r.entry("org.example.hud").meta.unwrap().version, "1.2.0");
    assert_eq!(r.entry("org.example.hud").source, Some(Source::Drop));

    let a = flagged("org.example.hud", "1.0.0", "a");
    let b = flagged("org.example.hud", "1.0.0", "b");
    let mut r = Rig::with(&[(&a, Source::Address, &[1]), (&b, Source::Drop, &[0])]);
    r.refresh();
    assert_eq!(r.entry("org.example.hud").sha, Some(Sha::of(&b)));
}

// ---- the drop folder ----

/// A drop file is listed with the verdict, installs only if what is read again is what was listed, and
/// is Unreviewed; the folder is left alone.
#[test]
fn a_drop_file_installs_as_unreviewed_when_it_is_still_what_was_listed() {
    let bytes = pack("org.example.dropped", "1.0.0");
    let sha = Sha::of(&bytes);
    let dropped = root(0).join("drop/a.plxhud");
    let mut r = Rig::new();
    r.files.insert(dropped.clone(), bytes.clone());
    r.files.insert(root(0).join("drop/readme.txt"), b"not a pack".to_vec());
    r.files.insert(root(0).join("drop/.hidden.plxhud"), b"x".to_vec());
    r.files.insert(root(1).join("drop/big.plxhud"), vec![0u8; MAX + 1]);
    r.refresh();
    let drops = r.m.drops();
    assert_eq!(drops.len(), 2, "{drops:?}");
    assert_eq!((drops[0].name.as_str(), drops[0].sha, drops[0].status), ("a.plxhud", Some(sha), Ok(())));
    assert_eq!((drops[1].name.as_str(), drops[1].sha, drops[1].status), ("big.plxhud", None, Err(Refusal::TooLarge)));

    // Changed on disk since the listing: refused, nothing written.
    r.files.insert(dropped.clone(), pack("org.example.dropped", "9.9.9"));
    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::InstallDrop { sha, now: 9 }), Handled::Yes);
    assert_eq!(r.notices.last(), Some(&Notice::Refused { sha, reason: Refusal::Changed }));
    assert!(r.lines(mark).iter().all(|l| !l.starts_with("write")));

    r.files.insert(dropped.clone(), bytes.clone());
    assert_eq!(r.send(HudEvent::InstallDrop { sha, now: 9 }), Handled::Yes);
    assert!(matches!(r.notices.last(), Some(Notice::Installed { sha: s, .. }) if *s == sha));
    let entry = r.entry("org.example.dropped");
    assert_eq!((entry.trust, entry.source), (Trust::Unreviewed, Some(Source::Drop)));
    assert!(r.files.contains_key(&dropped), "the drop file is left where it is");
    assert_eq!(r.m.drops()[0].status, Err(Refusal::AlreadyInstalled));

    // The drop folder of a later candidate is read from there.
    let later = pack("org.example.later", "2.0.0");
    r.files.insert(root(1).join("drop/later.plxhud"), later.clone());
    r.refresh();
    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::InstallDrop { sha: Sha::of(&later), now: 9 }), Handled::Yes);
    assert_eq!(r.lines(mark)[0], format!("read {}", root(1).join("drop/later.plxhud").display()));
    assert!(matches!(r.notices.last(), Some(Notice::Installed { id, .. }) if id == "org.example.later"));

    // An unlisted sha is not a drop file.
    assert_eq!(r.send(HudEvent::InstallDrop { sha: Sha::of(b"nothing"), now: 1 }), Handled::No);
}

// ---- addressing and busy ----

/// An answer for a tag nothing awaits changes nothing, an install or a removal waits for the idle, and
/// a Refresh is always taken: it replaces a listing, an install or a removal in flight.
#[test]
fn stale_answers_and_busy_commands_are_not_handled() {
    let bytes = pack("org.example.hud", "1.0.0");
    let mut m = HudStore::new();
    let (_, fx) = step(&mut m, HudEvent::Refresh { setup: setup(), selected: String::new() });
    let first = tag_of(&fx[0]);
    // Listing in flight: no install or removal, a wrong tag is ignored.
    assert!(m.busy());
    assert_eq!(step(&mut m, HudEvent::Install { bytes: bytes.clone(), source: Source::Address, now: 1 }).0, Handled::No);
    assert_eq!(step(&mut m, HudEvent::Removed { id: "org.example.hud".into() }).0, Handled::No);
    let wrong = Tag(first.0.wrapping_add(100));
    assert_eq!(step(&mut m, HudEvent::Read { tag: wrong, result: Err(ReadError::Failed) }).0, Handled::No);
    // A Refresh supersedes it: the old tag is stale now.
    let (handled, fx) = step(&mut m, HudEvent::Refresh { setup: setup(), selected: String::new() });
    assert_eq!(handled, Handled::Yes);
    assert_ne!(tag_of(&fx[0]), first);
    assert_eq!(step(&mut m, HudEvent::Read { tag: first, result: Err(ReadError::Failed) }).0, Handled::No);

    // An install in flight is not displaced by a command, and an answer of the wrong kind is ignored.
    let mut m = listed();
    let (_, fx) = step(&mut m, HudEvent::Install { bytes, source: Source::Address, now: 1 });
    assert_eq!(step(&mut m, HudEvent::Install { bytes: pack("org.example.other", "1.0.0"), source: Source::Address, now: 2 }).0, Handled::No);
    assert_eq!(step(&mut m, HudEvent::Read { tag: tag_of(&fx[0]), result: Err(ReadError::Failed) }).0, Handled::No);
    assert!(m.busy());
    // A Refresh is the one command it does not hold back: it takes over from the install.
    assert_eq!(step(&mut m, HudEvent::Refresh { setup: setup(), selected: String::new() }).0, Handled::Yes);
}

/// An answer that never comes, or comes in the wrong kind, must not leave the store busy for the life
/// of the process: reopening HUD settings (a `Refresh`) takes over from an install or a removal, and
/// the answers the old one was still owed are stale.
#[test]
fn a_refresh_takes_over_from_an_install_or_removal_that_never_finished() {
    let bytes = pack("org.example.hud", "1.0.0");
    let mut m = listed();
    let (_, fx) = step(&mut m, HudEvent::Install { bytes: bytes.clone(), source: Source::Address, now: 1 });
    let owed = tag_of(&fx[0]);
    assert!(m.busy());
    let (handled, fx) = step(&mut m, HudEvent::Refresh { setup: setup(), selected: String::new() });
    assert_eq!(handled, Handled::Yes);
    assert_ne!(tag_of(&fx[1]), owed, "the notice for the displaced install comes first");
    let verdict = validator(&bytes);
    assert_eq!(step(&mut m, HudEvent::Validated { tag: owed, sha: Sha::of(&bytes), verdict }).0, Handled::No);

    let mut r = Rig::with(&[(&bytes, Source::Address, &[0])]);
    r.refresh();
    let (handled, fx) = step(&mut r.m, HudEvent::Removed { id: "org.example.hud".into() });
    assert_eq!(handled, Handled::Yes);
    let owed = tag_of(&fx[0]);
    assert!(r.m.busy());
    assert_eq!(r.send(HudEvent::Refresh { setup: setup(), selected: String::new() }), Handled::Yes);
    assert!(!r.m.busy());
    let ack = HudEvent::Written { tag: owed, outcome: WriteOutcome::Done };
    assert_eq!(step(&mut r.m, ack).0, Handled::No);
    // Nothing was removed on the way: the disk still lists it.
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.hud"]);
}

/// A Refresh that displaces an install ends it with a notice, whatever step it had reached: nothing
/// else will say it did not finish. The listing that follows shows what the disk holds, so an install
/// whose index write did reach the disk is listed anyway.
#[test]
fn a_refresh_that_displaces_an_install_says_it_was_not_written() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let not_written = Notice::NotWritten { sha: Some(sha) };
    for (step, lose, landed, listed) in [
        ("validating", is_validate as fn(&HudEffect) -> bool, false, false),
        ("writing the pack", is_pack_write, false, false),
        ("writing the pack, which landed", is_pack_write, true, false),
        ("writing the index", is_index_write, false, false),
        ("writing the index, which landed", is_index_write, true, true),
    ] {
        let mut r = stalled_install(&[], &bytes, lose, landed);
        let mark = r.log.len();
        r.refresh();
        assert_eq!(r.log[mark], HudEffect::Notice(not_written.clone()), "{step}: the notice leads");
        assert_eq!(r.notices, [not_written.clone()], "{step}");
        assert_eq!(r.ids().len(), 1 + usize::from(listed), "{step}: the listing shows the disk");
    }

    // A drop file being read again before it is installed.
    let mut r = Rig::new();
    r.files.insert(root(0).join(DROP_DIR).join("a.plxhud"), bytes.clone());
    r.refresh();
    r.lose = Some((is_drop_read, false));
    assert_eq!(r.send(HudEvent::InstallDrop { sha, now: 9 }), Handled::Yes);
    assert!(r.m.busy());
    r.refresh();
    assert_eq!(r.notices, [not_written]);
}

/// A Refresh that displaces a removal ends it with `RemoveIncomplete`: the candidates it had not
/// rewritten yet still list the id.
#[test]
fn a_refresh_that_displaces_a_removal_says_it_is_incomplete() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let id = "org.example.hud";
    let mut r = Rig::with(&[(&bytes, Source::Drop, &[0, 1])]);
    r.refresh();
    r.lose = Some((is_index_write, true));
    assert_eq!(r.send(HudEvent::Removed { id: id.into() }), Handled::Yes);
    assert!(r.m.busy(), "the first candidate's answer never came");
    let mark = r.log.len();
    r.refresh();
    let incomplete = Notice::RemoveIncomplete { id: id.into() };
    assert_eq!(r.log[mark], HudEffect::Notice(incomplete.clone()));
    assert_eq!(r.notices, [incomplete]);
    // The disk is as the half-done removal left it, and the listing says so.
    assert!(r.index_of(0).is_empty() && !r.index_of(1).is_empty());
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, id]);
    // The pack of the candidate it did rewrite is no longer named by anything: gone. The other is kept.
    assert!(!r.files.contains_key(&pack_path(0, sha)));
    assert!(r.files.contains_key(&pack_path(1, sha)));
}

/// The pack an install was writing when a Refresh displaced it is deleted once the listing has read
/// the indexes again and no index names it; which pack that is depends on whether the index write
/// reached the disk, which only the disk knows.
#[test]
fn a_pack_a_displaced_install_left_unlisted_is_deleted_by_the_listing_that_follows() {
    let v1 = pack("org.example.hud", "1.0.0");
    let v2 = pack("org.example.hud", "2.0.0");
    let (s1, s2) = (Sha::of(&v1), Sha::of(&v2));
    let installed: &[(&[u8], Source, &[usize])] = &[(&v1, Source::Address, &[0])];
    let version = |r: &Rig| r.entry("org.example.hud").meta.unwrap().version;

    // The pack landed and the index was never written: v2's pack is the orphan.
    let mut r = stalled_install(installed, &v2, is_pack_write, true);
    assert!(r.files.contains_key(&pack_path(0, s2)));
    r.refresh();
    assert!(!r.files.contains_key(&pack_path(0, s2)));
    assert!(r.files.contains_key(&pack_path(0, s1)), "the installed one is untouched");
    assert_eq!(version(&r), "1.0.0");

    // The index write reached the disk and its answer was lost: v2 is the package, v1's pack the orphan.
    let mut r = stalled_install(installed, &v2, is_index_write, true);
    r.refresh();
    assert_eq!(version(&r), "2.0.0");
    assert!(r.files.contains_key(&pack_path(0, s2)));
    assert!(!r.files.contains_key(&pack_path(0, s1)));

    // The index write never reached the disk: v2's pack is the orphan, v1 is still the package.
    let mut r = stalled_install(installed, &v2, is_index_write, false);
    r.refresh();
    assert_eq!(version(&r), "1.0.0");
    assert!(!r.files.contains_key(&pack_path(0, s2)));
    assert!(r.files.contains_key(&pack_path(0, s1)));
}

/// The reclaim never costs a pack an index it could not read might name, or one an unreadable record
/// of the same file still names.
#[test]
fn a_reclaim_spares_what_an_index_it_could_not_read_may_name() {
    let v2 = pack("org.example.hud", "2.0.0");
    let s2 = Sha::of(&v2);

    // Candidate 0's index is unreadable when the listing comes: the pack may be listed there.
    let mut r = stalled_install(&[], &v2, is_pack_write, true);
    r.unreadable.push(index_path(0));
    r.refresh();
    assert!(r.files.contains_key(&pack_path(0, s2)));

    // A record this build cannot read still names the pack of the package a removal takes out.
    let a = pack("org.example.a", "1.0.0");
    let text = format!(
        r#"{{"v":1,"packs":{{
            "org.example.a":{{"sha":"{0}","source":"drop","at":1}},
            "org.example.b":{{"sha":"{0}","source":"carrier-pigeon","at":2}}
        }}}}"#,
        Sha::of(&a).hex()
    );
    let mut r = Rig::new();
    r.files.insert(index_path(0), text.into_bytes());
    r.files.insert(pack_path(0, Sha::of(&a)), a.clone());
    r.refresh();
    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::No);
    assert!(r.lines(mark).iter().all(|l| !l.starts_with("delete")), "{:?}", r.lines(mark));
    assert!(r.files.contains_key(&pack_path(0, Sha::of(&a))));
}

// ---- an installed.json this build cannot fully read is never rewritten from a partial view ----

/// A candidate whose index could not be READ (a failure, as opposed to the file not being there) is
/// unknown, not empty: installing into it would overwrite the packages it lists. The install goes to
/// the next candidate and the unread index is left byte for byte.
#[test]
fn an_index_that_could_not_be_read_is_never_overwritten() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let mut r = Rig::with(&[(&a, Source::Address, &[0])]);
    let before = r.files[&index_path(0)].clone();
    r.unreadable.push(index_path(0));
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC], "what could not be read is not listed");
    r.install(&b, Source::Address, 3);
    assert!(r.files.contains_key(&pack_path(1, Sha::of(&b))), "the install went to the next candidate");
    assert!(!r.files.contains_key(&pack_path(0, Sha::of(&b))));
    assert_eq!(r.files[&index_path(0)], before);
    // Once it reads again, its packages are back, and so is the winner by candidate order.
    r.unreadable.clear();
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a", "org.example.b"]);

    // An index that is simply not there is an empty candidate, which is writable.
    let mut r = Rig::new();
    r.refresh();
    r.install(&b, Source::Address, 3);
    assert!(r.files.contains_key(&pack_path(0, Sha::of(&b))));
}

/// An index that is not JSON, or that a newer build wrote in a layout this one does not know, lists
/// what it can and is never written.
#[test]
fn an_index_from_a_newer_build_or_garbage_is_listed_but_never_rewritten() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let newer = format!(
        r#"{{"v":2,"packs":{{"org.example.a":{{"sha":"{}","source":"drop","at":1}}}}}}"#,
        Sha::of(&a).hex()
    );
    let mut r = Rig::new();
    r.files.insert(index_path(0), newer.clone().into_bytes());
    r.files.insert(pack_path(0, Sha::of(&a)), a.clone());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
    r.install(&b, Source::Address, 3);
    assert!(r.files.contains_key(&pack_path(1, Sha::of(&b))));
    assert_eq!(r.files[&index_path(0)], newer.as_bytes());

    // Removing a package a read-only candidate lists cannot be done there: it says so.
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::No);
    assert_eq!(r.files[&index_path(0)], newer.as_bytes());

    let mut r = Rig::new();
    r.files.insert(index_path(0), b"not json".to_vec());
    r.refresh();
    r.install(&b, Source::Address, 3);
    assert!(r.files.contains_key(&pack_path(1, Sha::of(&b))));
    assert_eq!(r.files[&index_path(0)], b"not json");
}

/// The packs object of the `installed.json` a rig holds for candidate `n`.
fn packs_of(r: &Rig, n: usize) -> Map<String, Value> {
    let doc: Value = serde_json::from_slice(&r.files[&index_path(n)]).unwrap();
    doc["packs"].as_object().unwrap().clone()
}

/// A rig whose candidate 0 holds this `installed.json` and the pack of `org.example.a`.
fn rig_with_index(text: String, a: &[u8]) -> Rig {
    let mut r = Rig::new();
    r.files.insert(index_path(0), text.into_bytes());
    r.files.insert(pack_path(0, Sha::of(a)), a.to_vec());
    r
}

/// Every malformed current-contract index is preserved whole. Neither a partial interpretation
/// nor a later install/removal may rewrite its evidence or delete a potentially referenced archive.
#[test]
fn malformed_indexes_are_read_only_and_never_rewritten_or_reclaimed() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let sha = Sha::of(&a).hex();
    let record = format!(r#"{{"sha":"{sha}","source":"drop","at":1,"package_format":"1.0","hud_format":"1.0"}}"#);
    let good = format!(r#"{{"v":1,"packs":{{"org.example.a":{record}}}}}"#);
    let edits = [
        format!(r#"[1,{{"org.example.a":{record}}}]"#),
        good.replace(&record, &format!(r#"["{sha}","drop",1,"1.0","1.0"]"#)),
        good.replace(r#""v":1,"#, ""),
        good.replace(r#""v":1"#, r#""v":0"#),
        good.replace(r#""v":1"#, r#""v":2"#),
        good.replace(r#""v":1"#, r#""v":2,"v":1"#),
        good.replace(r#""v":1"#, r#""v":1,"unknown":true"#),
        good.replace(r#""source":"drop""#, r#""source":"carrier-pigeon""#),
        good.replace(r#""at":1"#, r#""at":1,"unknown":true"#),
        good.replace(r#""at":1"#, r#""at":2,"at":1"#),
        good.replace(&format!(r#""sha":"{sha}""#), &format!(r#""sha":"{}","sha":"{sha}""#, Sha::of(b"other").hex())),
        good.replace(r#""package_format":"1.0","#, ""),
        good.replace(r#""hud_format":"1.0""#, r#""hud_format":"2.0""#),
        good.replace("org.example.a", ""),
        good.replace("org.example.a", "../bad"),
        good.replace("org.example.a", "Upper.Case"),
        good.replace("org.example.a", "builtin:classic"),
        good.replace(&format!(r#""org.example.a":{record}"#), &format!(r#""org.example.a":{record},"org.example.a":{record}"#)),
    ];
    for text in edits {
        let mut r = rig_with_index(text.clone(), &a);
        r.refresh();
        assert_eq!(r.ids(), [BUILTIN_CLASSIC], "{text}");
        r.install(&b, Source::Address, 3);
        assert_eq!(r.files[&index_path(0)], text.as_bytes(), "evidence is untouched");
        assert!(r.files.contains_key(&pack_path(0, Sha::of(&a))), "old blob is retained");
        assert!(r.files.contains_key(&pack_path(1, Sha::of(&b))), "new install uses a writable candidate");
        assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::No);
    }
}

#[test]
fn emitted_indexes_bind_both_formats_and_the_complete_archive_digest() {
    let bytes = pack("org.example.a", "1.0.0");
    let mut r = Rig::new();
    r.refresh();
    r.install(&bytes, Source::Address, 9);
    let record = &packs_of(&r, 0)["org.example.a"];
    assert_eq!(record, &serde_json::json!({
        "sha": Sha::of(&bytes).hex(), "source": "address", "at": 9,
        "package_format": "1.0", "hud_format": "1.0"
    }));
}

#[test]
fn removal_reports_incomplete_when_an_unknown_candidate_may_retain_the_id() {
    let bytes = pack("org.example.a", "1.0.0");
    let mut r = Rig::with(&[(&bytes, Source::Drop, &[0, 1])]);
    let mut doc: Value = serde_json::from_slice(&r.files[&index_path(0)]).unwrap();
    doc["unknown"] = true.into();
    let evidence = doc.to_string().into_bytes();
    r.files.insert(index_path(0), evidence.clone());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"], "candidate one lists the HUD");
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::Yes);
    assert_eq!(r.notices.last(), Some(&Notice::RemoveIncomplete { id: "org.example.a".into() }));
    assert_eq!(r.files[&index_path(0)], evidence);
    assert!(r.files.contains_key(&pack_path(0, Sha::of(&bytes))));
    assert!(!r.files.contains_key(&pack_path(1, Sha::of(&bytes))));
}

/// An install over the byte bound is refused before anything is asked of the runner.
#[test]
fn bytes_over_the_bound_are_refused_unvalidated() {
    let mut r = Rig::new();
    r.refresh();
    let big = vec![b'x'; MAX + 1];
    let mark = r.log.len();
    r.install(&big, Source::Address, 1);
    assert_eq!(r.lines(mark).len(), 1);
    assert_eq!(r.notices.last(), Some(&Notice::Refused { sha: Sha::of(&big), reason: Refusal::TooLarge }));
}

// ---- the small pure parts ----

#[test]
fn versions_order_by_number_and_malformed_ones_do_not_parse() {
    let v = |s: &str| Semver::parse(s).unwrap_or_else(|| panic!("{s}"));
    assert!(v("1.10.0") > v("1.9.0"));
    assert!(v("2.0.0") > v("1.99.99"));
    assert!(v("1.0.1") > v("1.0.0"));
    assert_eq!(v("0.0.0"), v("0.0.0"));
    for bad in ["", "1", "1.0", "1.0.0.0", "01.0.0", "1.0.0-beta", "1.0.0+1", "1.a.0", "-1.0.0", " 1.0.0", "1..0", "99999999999999999999.0.0"] {
        assert_eq!(Semver::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn a_sha_round_trips_through_its_hex_and_only_64_lowercase_digits_parse() {
    let sha = Sha::of(b"abc");
    assert_eq!(sha.hex(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(Sha::parse(&sha.hex()), Some(sha));
    assert_eq!(Sha::parse(&sha.hex().to_uppercase()), None);
    assert_eq!(Sha::parse(&sha.hex()[1..]), None);
    assert_eq!(Sha::parse(&format!("{}é", &sha.hex()[2..])), None);
}

#[test]
fn the_index_file_round_trips_and_malformed_roots_are_read_only() {
    let mut index = Index::new();
    index.insert("org.example.a".to_string(), record_at(Sha::of(b"a"), Source::Registry, 5));
    index.insert("org.example.b".to_string(), record_at(Sha::of(b"b"), Source::Drop, 6));
    assert_eq!(parse_index(&index_bytes(&index)).index, index);
    assert_eq!(
        index_bytes(&index),
        index_bytes(&index.clone()),
        "the same index is the same bytes"
    );

    for text in [&b"not json"[..], b"", b"[]", b"{}", b"{\"v\":1}", b"{\"v\":1,\"packs\":[]}"] {
        let parsed = parse_index(text);
        assert!(parsed.index.is_empty() && parsed.access == Access::ReadOnly, "{text:?}");
    }
}

#[test]
fn only_package_file_names_can_be_dropped_in() {
    assert!(drop_name_ok("my.plxhud"));
    for bad in [".plxhud", ".hidden.plxhud", "a/b.plxhud", "a\\b.plxhud", "x.json", "x.plxhud.bak", "../x.plxhud"] {
        assert!(!drop_name_ok(bad), "{bad:?}");
    }
}

// ---- install from an address (PR11) ----

const HUD_URL: &str = "https://github.com/org/hud/releases/latest/download/hud.plxhud";

fn address(text: &str) -> address::Address {
    address::parse(text).unwrap_or_else(|e| panic!("{text:?}: {e:?}"))
}

/// A rig listing an empty disk whose network answers `HUD_URL` with `body`.
fn fetching(body: Result<Vec<u8>, FetchError>) -> Rig {
    let mut r = Rig::new();
    r.network.insert(HUD_URL.to_owned(), body);
    r.refresh();
    r
}

fn fetch_event() -> HudEvent {
    HudEvent::Fetch { address: address("gh:org/hud") }
}

/// A fetch asks the network lane for the URL, validates what came back, and then HOLDS it: nothing
/// is written until the viewer confirms, and what is written hashes to the sha the viewer was shown.
#[test]
fn a_fetch_is_offered_before_anything_is_written_and_installs_the_bytes_the_viewer_saw() {
    let bytes = pack("org.example.hud", "1.2.0");
    let sha = Sha::of(&bytes);
    let mut r = fetching(Ok(bytes.clone()));
    let mark = r.log.len();
    assert_eq!(r.send(fetch_event()), Handled::Yes);
    assert_eq!(r.lines(mark), [format!("fetch {HUD_URL}"), format!("validate {}", sha.hex())]);
    assert!(r.m.busy(), "an offer is open work");
    assert!(r.files.keys().all(|p| !p.to_string_lossy().contains("packs/")), "nothing on disk yet: {:?}", r.files.keys());
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);

    let offer = r.m.offer().expect("a loadable fetch is offered");
    assert_eq!((offer.sha, offer.source, offer.meta.id.as_str(), offer.meta.version.as_str()), (sha, Source::Address, "org.example.hud", "1.2.0"));
    assert_eq!(offer.from.as_ref().map(|a| (a.url(), a.host(), a.dev_source())), Some((HUD_URL, "github.com", false)));
    assert_eq!(offer.source.trust(), Trust::Unreviewed);

    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::InstallOffer { sha, now: 77 }), Handled::Yes);
    let lines = r.lines(mark);
    assert_eq!(lines[..2], [format!("write {}", pack_path(0, sha).display()), format!("write {}", index_path(0).display())]);
    assert!(!r.m.busy());
    assert!(r.m.offer().is_none());
    let written = &r.files[&pack_path(0, sha)];
    assert_eq!(Sha::of(written), sha, "the bytes on disk hash to the sha the viewer saw");
    assert_eq!(r.index_of(0)["org.example.hud"], record_at(sha, Source::Address, 77));
    assert_eq!(r.entry("org.example.hud").source, Some(Source::Address));
    assert_eq!(r.entry("org.example.hud").trust, Trust::Unreviewed);
    assert!(matches!(r.notices.last(), Some(Notice::Installed { sha: s, .. }) if *s == sha));
}

/// A viewer who cancels leaves nothing: no file, no index, no notice, and the store takes the next
/// request.
#[test]
fn declining_an_offer_writes_nothing() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);
    let mut r = fetching(Ok(bytes));
    let files = r.files.clone();
    assert_eq!(r.send(fetch_event()), Handled::Yes);
    assert!(r.m.offer().is_some());
    // The wrong sha is not the offer the viewer saw: neither confirms nor declines it.
    let other = Sha::of(b"other");
    assert_eq!(r.send(HudEvent::InstallOffer { sha: other, now: 1 }), Handled::No);
    assert_eq!(r.send(HudEvent::DeclineOffer { sha: other }), Handled::No);
    assert!(r.m.offer().is_some());

    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::DeclineOffer { sha }), Handled::Yes);
    assert!(r.lines(mark).is_empty(), "no effect: {:?}", r.lines(mark));
    assert!(r.notices.is_empty());
    assert!(!r.m.busy() && r.m.offer().is_none());
    assert_eq!(r.files, files, "the disk is as it was");
    // Once declined there is nothing to confirm.
    assert_eq!(r.send(HudEvent::InstallOffer { sha, now: 1 }), Handled::No);
    assert_eq!(r.send(fetch_event()), Handled::Yes, "the next request is taken");
}

/// Each way a fetch fails ends with its own notice and writes nothing.
#[test]
fn each_failed_fetch_names_its_reason_and_writes_nothing() {
    for reason in [FetchError::NotFound, FetchError::TooLarge, FetchError::Unreachable, FetchError::Certificate] {
        let mut r = fetching(Err(reason));
        let files = r.files.clone();
        assert_eq!(r.send(fetch_event()), Handled::Yes);
        assert_eq!(r.notices, [Notice::FetchFailed { reason }]);
        assert!(!r.m.busy() && r.m.offer().is_none());
        assert_eq!(r.files, files, "{reason:?}");
    }
}

/// What the body is gets the existing refusals: not a package, a package this app cannot run, one
/// that claims the built-in's id, the one already installed, one that is not newer, and one over the
/// bound that came back anyway. Nothing is offered or written for any of them.
#[test]
fn a_fetched_body_that_is_refused_is_never_offered() {
    let installed = pack("org.example.hud", "2.0.0");
    let cases: [(&str, Vec<u8>, Refusal); 6] = [
        ("not a package", flagged("a.b", "1.0.0", "bad"), Refusal::Invalid),
        ("newer format", flagged("a.b", "1.0.0", "newer"), Refusal::Unloadable(Unloadable::NewerFormat)),
        ("built-in id", pack(BUILTIN_CLASSIC, "1.0.0"), Refusal::BuiltinId),
        ("already installed", installed.clone(), Refusal::AlreadyInstalled),
        ("not newer", pack("org.example.hud", "1.0.0"), Refusal::NotNewer),
        ("over the bound", vec![b'x'; MAX + 1], Refusal::TooLarge),
    ];
    for (what, body, reason) in cases {
        let mut r = Rig::with(&[(&installed, Source::Drop, &[0])]);
        r.network.insert(HUD_URL.to_owned(), Ok(body.clone()));
        r.refresh();
        let files = r.files.clone();
        assert_eq!(r.send(fetch_event()), Handled::Yes, "{what}");
        assert_eq!(r.notices, [Notice::Refused { sha: Sha::of(&body), reason }], "{what}");
        assert!(!r.m.busy() && r.m.offer().is_none(), "{what}");
        assert_eq!(r.files, files, "{what}: nothing written");
    }
}

/// Busy is `Handled::No`, like an install while busy: a second fetch while one is in flight or an
/// offer is open, and a fetch while anything else runs. The answer of a fetch nothing awaits is
/// dropped, and so is an answer carrying another request's tag.
#[test]
fn a_fetch_while_busy_is_not_handled_and_a_late_answer_is_dropped() {
    let bytes = pack("org.example.hud", "1.0.0");
    let mut m = listed();
    let (handled, fx) = step(&mut m, fetch_event());
    assert_eq!(handled, Handled::Yes);
    let awaited = tag_of(&fx[0]);
    assert!(matches!(&fx[0], HudEffect::Fetch { max_bytes: 1024, .. }));
    assert!(m.busy());
    assert_eq!(step(&mut m, fetch_event()).0, Handled::No, "a second request while one is in flight");
    assert_eq!(step(&mut m, HudEvent::Install { bytes: bytes.clone(), source: Source::Address, now: 1 }).0, Handled::No);
    assert_eq!(step(&mut m, HudEvent::Removed { id: "org.example.hud".into() }).0, Handled::No);
    let stranger = Tag(awaited.0.wrapping_add(50));
    assert_eq!(step(&mut m, HudEvent::Fetched { tag: stranger, result: Ok(bytes.clone()) }).0, Handled::No);
    assert!(m.busy(), "the wrong tag changed nothing");

    // The right answer is taken, once; a repeat of it finds nothing awaiting it.
    let (handled, fx) = step(&mut m, HudEvent::Fetched { tag: awaited, result: Ok(bytes.clone()) });
    assert_eq!(handled, Handled::Yes);
    assert!(is_validate(&fx[0]));
    let verdict = validator(&bytes);
    let (_, held) = step(&mut m, HudEvent::Validated { tag: tag_of(&fx[0]), sha: Sha::of(&bytes), verdict });
    assert!(held.is_empty(), "held for the viewer: {held:?}");
    assert_eq!(step(&mut m, HudEvent::Fetched { tag: awaited, result: Ok(bytes.clone()) }).0, Handled::No);
    assert_eq!(step(&mut m, fetch_event()).0, Handled::No, "a second request while an offer is open");

    // Idle store: nothing awaits a fetch.
    let mut idle = listed();
    assert_eq!(step(&mut idle, HudEvent::Fetched { tag: awaited, result: Ok(bytes) }).0, Handled::No);
    assert!(!idle.busy());
    // A request answered before the Refresh that replaced it is stale.
    let (_, fx) = step(&mut idle, fetch_event());
    let owed = tag_of(&fx[0]);
    assert_eq!(step(&mut idle, HudEvent::Refresh { setup: setup(), selected: String::new() }).0, Handled::Yes);
    assert_eq!(step(&mut idle, HudEvent::Fetched { tag: owed, result: Err(FetchError::NotFound) }).0, Handled::No);
}

/// A `Refresh` replaces a fetch in flight and an offer waiting, and ends each with the notice an
/// install gets, `NotWritten`; nothing reaches the disk. This is also what leaving the page costs:
/// the page's return opens HUD settings again, which is a `Refresh`, so an offer does not wait for
/// the return, it goes (the simpler of the two; the page asks again).
#[test]
fn a_refresh_ends_a_fetch_in_flight_and_an_offer_waiting_with_not_written() {
    let bytes = pack("org.example.hud", "1.0.0");
    let sha = Sha::of(&bytes);

    // In flight: the answer never came back before the Refresh.
    let mut m = listed();
    let (_, fx) = step(&mut m, fetch_event());
    let owed = tag_of(&fx[0]);
    let (handled, fx) = step(&mut m, HudEvent::Refresh { setup: setup(), selected: String::new() });
    assert_eq!(handled, Handled::Yes);
    assert_eq!(fx[0], HudEffect::Notice(Notice::NotWritten { sha: None }), "no bytes, so no sha yet");
    assert_eq!(step(&mut m, HudEvent::Fetched { tag: owed, result: Ok(bytes.clone()) }).0, Handled::No, "the late answer is dropped");

    // Waiting for the viewer.
    let mut r = fetching(Ok(bytes));
    let files = r.files.clone();
    assert_eq!(r.send(fetch_event()), Handled::Yes);
    assert!(r.m.offer().is_some());
    let mark = r.log.len();
    r.refresh();
    assert_eq!(r.log[mark], HudEffect::Notice(Notice::NotWritten { sha: Some(sha) }), "the notice leads");
    assert_eq!(r.notices, [Notice::NotWritten { sha: Some(sha) }]);
    assert!(r.m.offer().is_none());
    assert_eq!(r.files, files, "nothing reached the disk");
    assert_eq!(r.send(HudEvent::InstallOffer { sha, now: 1 }), Handled::No, "the offer is gone");
}

/// An install from an address replaces a lower version of the id like any other source.
#[test]
fn a_fetched_higher_version_replaces_the_installed_one() {
    let old = pack("org.example.hud", "1.0.0");
    let new = pack("org.example.hud", "1.1.0");
    let (old_sha, new_sha) = (Sha::of(&old), Sha::of(&new));
    let mut r = Rig::with(&[(&old, Source::Drop, &[0])]);
    r.network.insert(HUD_URL.to_owned(), Ok(new));
    r.refresh();
    assert_eq!(r.send(fetch_event()), Handled::Yes);
    assert_eq!(r.send(HudEvent::InstallOffer { sha: new_sha, now: 5 }), Handled::Yes);
    assert_eq!(r.entry("org.example.hud").sha, Some(new_sha));
    assert!(!r.files.contains_key(&pack_path(0, old_sha)), "the replaced pack is deleted");
}

// ---- re-fetch a dev source (PR15) ----

const DEV_URL: &str = "http://192.168.1.20:8123/hud.plxhud";

/// Fetch `text` and confirm the offer, the way the Settings page does.
fn fetch_and_confirm(r: &mut Rig, text: &str, bytes: &[u8], now: u64) {
    assert_eq!(r.send(HudEvent::Fetch { address: address(text) }), Handled::Yes);
    assert_eq!(r.send(HudEvent::InstallOffer { sha: Sha::of(bytes), now }), Handled::Yes);
    assert!(!r.m.busy());
}

/// The address a package came from is recorded for a development source only: the `http://` address of a
/// machine on the home network. A published address, a drop file and the registry record none, and the
/// index a store writes without a development source is the one it always wrote.
#[test]
fn an_install_from_a_dev_source_records_its_address_and_no_other_install_does() {
    let dev = pack("org.example.dev", "1.0.0");
    let published = pack("org.example.published", "1.0.0");
    let dropped = pack("org.example.dropped", "1.0.0");
    let registry = pack("org.example.registry", "1.0.0");
    let mut r = Rig::new();
    r.network.insert(DEV_URL.to_owned(), Ok(dev.clone()));
    r.network.insert(HUD_URL.to_owned(), Ok(published.clone()));
    r.refresh();
    fetch_and_confirm(&mut r, "gh:org/hud", &published, 5);
    fetch_and_confirm(&mut r, DEV_URL, &dev, 6);
    r.install(&dropped, Source::Drop, 7);
    r.install(&registry, Source::Registry, 8);

    assert_eq!(r.entry("org.example.dev").dev_source, Some(address(DEV_URL)));
    for id in ["org.example.published", "org.example.dropped", "org.example.registry"] {
        assert_eq!(r.entry(id).dev_source, None, "{id}");
    }
    assert_eq!(r.entry(BUILTIN_CLASSIC).dev_source, None);
    let packs = packs_of(&r, 0);
    assert_eq!(packs["org.example.dev"]["from"], DEV_URL);
    for id in ["org.example.published", "org.example.dropped", "org.example.registry"] {
        assert!(packs[id].get("from").is_none(), "{id} carries no address: {:?}", packs[id]);
    }
}

/// The address is state like the rest of the record: a store that starts from the disk it left lists the
/// package with it, and fetching that address asks the network lane for the recorded URL, again.
#[test]
fn the_recorded_address_is_read_back_and_fetches_the_next_build() {
    let first = pack("org.example.dev", "1.0.0");
    let next = pack("org.example.dev", "1.0.1");
    let mut r = Rig::new();
    r.network.insert(DEV_URL.to_owned(), Ok(first.clone()));
    r.refresh();
    fetch_and_confirm(&mut r, DEV_URL, &first, 5);

    // A restart: a new store over the files the first one wrote.
    let mut again = Rig::new();
    again.files = r.files.clone();
    again.network.insert(DEV_URL.to_owned(), Ok(next.clone()));
    again.refresh();
    let recorded = again.entry("org.example.dev").dev_source.expect("the address survives a restart");
    assert_eq!((recorded.url(), recorded.host(), recorded.dev_source()), (DEV_URL, "192.168.1.20", true));

    let mark = again.log.len();
    assert_eq!(again.send(HudEvent::Fetch { address: recorded }), Handled::Yes);
    assert_eq!(again.lines(mark), [format!("fetch {DEV_URL}"), format!("validate {}", Sha::of(&next).hex())]);
    assert!(again.m.offer().is_some_and(|offer| offer.meta.version == "1.0.1" && offer.source == Source::Address));
    assert_eq!(again.entry("org.example.dev").sha, Some(Sha::of(&first)), "nothing is installed before the viewer confirms");
    assert_eq!(again.send(HudEvent::InstallOffer { sha: Sha::of(&next), now: 9 }), Handled::Yes);
    let entry = again.entry("org.example.dev");
    assert_eq!((entry.sha, entry.meta.map(|m| m.version)), (Some(Sha::of(&next)), Some("1.0.1".to_owned())));
    assert_eq!(entry.dev_source, Some(address(DEV_URL)), "the next build is still the development one");
}

/// A package that a later install replaces from somewhere else is no longer a development one.
#[test]
fn an_install_from_elsewhere_forgets_the_dev_address() {
    let dev = pack("org.example.hud", "1.0.0");
    let released = pack("org.example.hud", "2.0.0");
    let mut r = Rig::new();
    r.network.insert(DEV_URL.to_owned(), Ok(dev.clone()));
    r.network.insert(HUD_URL.to_owned(), Ok(released.clone()));
    r.refresh();
    fetch_and_confirm(&mut r, DEV_URL, &dev, 5);
    assert!(r.entry("org.example.hud").dev_source.is_some());
    fetch_and_confirm(&mut r, "gh:org/hud", &released, 6);
    assert_eq!(r.entry("org.example.hud").dev_source, None);
    assert!(packs_of(&r, 0)["org.example.hud"].get("from").is_none());
}

/// The store trusts the disk it reads, and fails closed on an address it would not have recorded: one
/// that is not a development source, or that sits on a record from another source, or that is no address.
/// The package is listed all the same, and the index stays writable.
#[test]
fn an_address_the_store_would_not_have_recorded_is_not_a_dev_source() {
    let a = pack("org.example.a", "1.0.0");
    let sha = Sha::of(&a).hex();
    let index = |source: &str, from: &str| {
        format!(r#"{{"v":1,"packs":{{"org.example.a":{{"sha":"{sha}","source":"{source}","at":1,"package_format":"1.0","hud_format":"1.0","from":{from}}}}}}}"#)
    };
    let dev = format!("\"{DEV_URL}\"");
    let cases = [
        ("address", dev.clone(), true),
        ("address", format!("\"{HUD_URL}\""), false),
        ("address", "\"http://8.8.8.8/hud.plxhud\"".to_owned(), false),
        ("address", "\"http://printer.local/hud.plxhud\"".to_owned(), false),
        ("address", "\"not an address\"".to_owned(), false),
        ("address", "\"\"".to_owned(), false),
        ("registry", dev.clone(), false),
        ("drop", dev, false),
    ];
    for (source, from, expect) in cases {
        let mut r = rig_with_index(index(source, &from), &a);
        r.refresh();
        let entry = r.entry("org.example.a");
        assert_eq!(entry.dev_source.is_some(), expect, "{source} {from}");
        assert_eq!(entry.sha.map(|s| s.hex()), Some(sha.clone()), "the package is listed: {source} {from}");
        let other = pack("org.example.b", "1.0.0");
        r.install(&other, Source::Drop, 2);
        assert!(r.entry("org.example.b").sha.is_some(), "the index is not read-only: {source} {from}");
    }
}

fn registry_fixture(bytes: &[u8], revoked: bool) -> Vec<u8> {
    let meta = match validator(bytes) { Verdict::Loadable(meta) => meta, Verdict::Unloadable(..) | Verdict::Invalid => panic!("fixture") };
    let sha = Sha::of(bytes).hex();
    let art = "b".repeat(64);
    let base = catalog::ORIGIN;
    serde_json::to_vec(&serde_json::json!({"format":1,"serial":1000,"entries":[{
        "id":meta.id,"name":meta.name,"version":meta.version,"package_format":"1.0","hud_format":"1.0",
        "author":"Author","license":"CC0-1.0","app":{},"sha256":sha,"bytes":bytes.len(),
        "url":format!("{base}/p/{sha}.plxhud"),"unfit":[],
        "previews":[{"state":"compact","lang":"en","file":format!("{art}.png"),"sha256":art,
            "url":format!("{base}/previews/{sha}/{art}.png")}]
    }],"revoked":if revoked {serde_json::json!([{"sha256":sha,"reason":"Withdrawn layout"}])} else {serde_json::json!([])}})).unwrap()
}

fn publish_catalog(store: &mut HudStore, bytes: Vec<u8>) -> Vec<HudEffect> {
    let mut fx=vec![];
    store.step(HudEvent::Catalog(catalog::Event::Open {candidates:vec![],now:10}),&mut fx);
    let tag=fx.iter().find_map(|e| match e {HudEffect::Catalog(catalog::CatalogEffect::Get {tag,..})=>Some(*tag),HudEffect::Catalog(_) | HudEffect::FetchRegistry { .. } | HudEffect::ScanDir { .. } | HudEffect::ReadFile { .. } | HudEffect::WriteFile { .. } | HudEffect::Delete { .. } | HudEffect::Fetch { .. } | HudEffect::Validate { .. } | HudEffect::Selection { .. } | HudEffect::Notice(_) => None}).unwrap();
    fx.clear();
    let signed=catalog::testing::sign(&bytes);
    store.step(HudEvent::Catalog(catalog::Event::Reply {tag,result:Ok(catalog::Reply::http(200,None,&signed,&catalog::testing::keys()))}),&mut fx);
    fx
}

#[test]
fn revoked_hash_disables_every_install_source_and_survives_later_disk_listing() {
    let bytes=pack("com.example.revoked","1.0.0");let sha=Sha::of(&bytes);
    for source in [Source::Drop,Source::Address,Source::Registry] {
        let mut rig=Rig::with(&[(&bytes,source,&[0])]);
        // Cache first, then the installed pack: neither order may re-enable the selected hash.
        publish_catalog(&mut rig.m,registry_fixture(&bytes,true));
        rig.refresh_selecting("com.example.revoked");
        assert_eq!(rig.m.resolved(),BUILTIN_CLASSIC);
        assert_eq!(rig.m.selected(),BUILTIN_CLASSIC);
        assert_eq!(rig.m.entries()[1].loadable,Err(Unloadable::Revoked));
        assert_eq!(rig.send(HudEvent::Selected {id:"com.example.revoked".into()}),Handled::No);
        rig.install(&bytes,source,20);
        assert_eq!(rig.notices.last(),Some(&Notice::Refused {sha,reason:Refusal::Unloadable(Unloadable::Revoked)}));
    }
    let mut rig=Rig::with(&[(&bytes,Source::Drop,&[0])]);rig.refresh_selecting("com.example.revoked");
    let fx=publish_catalog(&mut rig.m,registry_fixture(&bytes,true));
    assert!(fx.contains(&HudEffect::Selection {id:BUILTIN_CLASSIC.into()}));
    assert_eq!(rig.m.resolved(),BUILTIN_CLASSIC);
}

#[test]
fn registry_fetch_binds_download_bytes_and_manifest_before_offering_and_never_selects_on_install() {
    let bytes=pack("com.example.registry","1.0.0");let sha=Sha::of(&bytes);
    for changed in [false,true] {
        let mut rig=Rig::new();rig.refresh();publish_catalog(&mut rig.m,registry_fixture(&bytes,false));
        let mut fx=vec![];
        rig.m.step(HudEvent::FetchRegistry {sha,app:"0.10.0".into(),language:"en".into()},&mut fx);
        let tag=match &fx[0] {HudEffect::FetchRegistry {tag,package,..}=> {assert_eq!(package.sha(),sha);*tag},HudEffect::Catalog(_) | HudEffect::ScanDir { .. } | HudEffect::ReadFile { .. } | HudEffect::WriteFile { .. } | HudEffect::Delete { .. } | HudEffect::Fetch { .. } | HudEffect::Validate { .. } | HudEffect::Selection { .. } | HudEffect::Notice(_) => panic!("registry GET")};
        let mut answer=vec![];
        rig.m.step(HudEvent::Fetched {tag,result:Ok(if changed {pack("com.example.other","1.0.0")} else {bytes.clone()})},&mut answer);
        if changed {
            assert!(rig.m.offer().is_none());assert!(!rig.m.busy());
            assert!(matches!(answer.as_slice(),[HudEffect::Notice(Notice::Refused {reason:Refusal::Changed,..})]));
        } else {
            rig.run(answer);
            let offer=rig.m.offer().expect("validated offer");assert_eq!(offer.source,Source::Registry);
            assert_eq!(rig.m.entries().len(),1,"confirmation must precede the first write");
            rig.send(HudEvent::InstallOffer {sha,now:20});
            assert_eq!(rig.m.entries()[1].trust,Trust::Registry);
            assert_eq!(rig.m.selected(),BUILTIN_CLASSIC,"install is distinct from Use HUD");
            assert_eq!(rig.send(HudEvent::Selected {id:"com.example.registry".into()}),Handled::Yes);
        }
    }
}
