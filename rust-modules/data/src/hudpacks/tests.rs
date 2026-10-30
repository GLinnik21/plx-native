//! `HudStore` tests: events in, effects out. `Rig` is a runner over an in-memory disk: it executes the
//! store's effects the way PR10b's runners will and feeds each answer back as the event it is, so a
//! test reads the effects in the order the store pushed them. A pack here is the text
//! `id@version[#flag]`; the rig's validator reads it the way the appkit validator reads the real
//! format (`#newer` is a file for a newer PlxNative, `#bad` does not validate).

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use super::*;

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
        format: "1.0".to_string(),
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

fn record(bytes: &[u8], source: Source) -> Record {
    Record::new(Sha::of(bytes), source, 7)
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
            rig.files.insert(index_path(dir), index_bytes(&index, &Map::new()).to_vec());
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
        HudEffect::ScanDir { at, .. } => format!("scan {}", at.path().display()),
        HudEffect::ReadFile { at, .. } => format!("read {}", at.path().display()),
        HudEffect::WriteFile { at, .. } => format!("write {}", at.path().display()),
        HudEffect::Delete { at } => format!("delete {}", at.path().display()),
        HudEffect::Validate { sha, .. } => format!("validate {}", sha.hex()),
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
        | HudEffect::Validate { tag, .. } => *tag,
        other => panic!("{other:?} carries no tag"),
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
    assert_eq!(written["org.example.hud"], Record::new(sha, Source::Address, 100));
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
    assert_eq!(r.index_of(0)["org.example.hud"], Record::new(sha, Source::Address, 100));
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
    assert_eq!(r.notices.last(), Some(&Notice::NotWritten { sha: Sha::of(&bytes) }));
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
    r.files.insert(root(0).join("drop/mine.plxhud.json"), bytes.clone());
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
    r.files.insert(index_path(1), index_bytes(&index, &Map::new()).to_vec());
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
    let meta = Meta { id: "org.example.hud".into(), name: "n".into(), version: "1.0.0".into(), format: "1.0".into() };
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
    r.files.insert(index_path(1), index_bytes(&index, &Map::new()).to_vec());
    // A third entry whose file is gone.
    let gone = pack("org.example.gone", "1.0.0");
    let mut index = r.index_of(0);
    index.insert("org.example.gone".to_string(), record(&gone, Source::Address));
    r.files.insert(index_path(0), index_bytes(&index, &Map::new()).to_vec());
    // The good pack's file is swapped for other bytes under its name.
    let swapped = pack("org.example.swapped", "1.0.0");
    r.files.insert(pack_path(2, Sha::of(&swapped)), pack("org.example.other", "9.9.9"));
    let mut index = Index::new();
    index.insert("org.example.swapped".to_string(), record(&swapped, Source::Address));
    r.files.insert(index_path(2), index_bytes(&index, &Map::new()).to_vec());
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
    let dropped = root(0).join("drop/a.plxhud.json");
    let mut r = Rig::new();
    r.files.insert(dropped.clone(), bytes.clone());
    r.files.insert(root(0).join("drop/readme.txt"), b"not a pack".to_vec());
    r.files.insert(root(0).join("drop/.hidden.plxhud.json"), b"x".to_vec());
    r.files.insert(root(1).join("drop/big.plxhud.json"), vec![0u8; MAX + 1]);
    r.refresh();
    let drops = r.m.drops();
    assert_eq!(drops.len(), 2, "{drops:?}");
    assert_eq!((drops[0].name.as_str(), drops[0].sha, drops[0].status), ("a.plxhud.json", Some(sha), Ok(())));
    assert_eq!((drops[1].name.as_str(), drops[1].sha, drops[1].status), ("big.plxhud.json", None, Err(Refusal::TooLarge)));

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
    r.files.insert(root(1).join("drop/later.plxhud.json"), later.clone());
    r.refresh();
    let mark = r.log.len();
    assert_eq!(r.send(HudEvent::InstallDrop { sha: Sha::of(&later), now: 9 }), Handled::Yes);
    assert_eq!(r.lines(mark)[0], format!("read {}", root(1).join("drop/later.plxhud.json").display()));
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
    let not_written = Notice::NotWritten { sha };
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
    r.files.insert(root(0).join(DROP_DIR).join("a.plxhud.json"), bytes.clone());
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
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::Yes);
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
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"]);
    r.install(&b, Source::Address, 3);
    assert!(r.files.contains_key(&pack_path(1, Sha::of(&b))));
    assert_eq!(r.files[&index_path(0)], newer.as_bytes());

    // Removing a package a read-only candidate lists cannot be done there: it says so.
    r.send(HudEvent::Removed { id: "org.example.a".into() });
    assert_eq!(r.notices.last(), Some(&Notice::RemoveIncomplete { id: "org.example.a".into() }));
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

/// A record this build reads, with a field a later build added, is listed and written back with that
/// field: a rewrite for another package must not strip what it does not know.
#[test]
fn a_record_with_a_field_a_later_build_added_is_written_back_with_it() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let text = format!(
        r#"{{"v":1,"packs":{{"org.example.a":{{"sha":"{}","source":"drop","at":1,"pinned":true,"note":{{"k":[1,2]}}}}}}}}"#,
        Sha::of(&a).hex()
    );
    let mut r = rig_with_index(text, &a);
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"], "it is a record this build reads");
    r.install(&b, Source::Address, 3);
    let packs = packs_of(&r, 0);
    assert_eq!(packs.keys().collect::<Vec<_>>(), ["org.example.a", "org.example.b"]);
    assert_eq!(packs["org.example.a"]["pinned"], true);
    assert_eq!(packs["org.example.a"]["note"], serde_json::json!({"k": [1, 2]}));
    assert_eq!(packs["org.example.a"]["source"], "drop");
    assert_eq!(packs["org.example.a"]["at"], 1);
    // Removing the other package rewrites the file again; the field is still there.
    r.send(HudEvent::Removed { id: "org.example.b".into() });
    assert_eq!(packs_of(&r, 0)["org.example.a"]["pinned"], true);
}

/// A record whose source this build does not know is not listed, and is written back as it was.
#[test]
fn a_record_with_a_source_this_build_does_not_know_is_written_back_unchanged() {
    let a = pack("org.example.a", "1.0.0");
    let b = pack("org.example.b", "1.0.0");
    let text = format!(
        r#"{{"v":1,"packs":{{
            "org.example.a":{{"sha":"{}","source":"drop","at":1}},
            "org.example.future":{{"sha":"{}","source":"carrier-pigeon","at":2}}
        }}}}"#,
        Sha::of(&a).hex(),
        Sha::of(b"f").hex()
    );
    let mut r = rig_with_index(text, &a);
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"]);
    r.install(&b, Source::Address, 3);
    let packs = packs_of(&r, 0);
    assert_eq!(packs.keys().collect::<Vec<_>>(), ["org.example.a", "org.example.b", "org.example.future"]);
    assert_eq!(
        packs["org.example.future"],
        serde_json::json!({"sha": Sha::of(b"f").hex(), "source": "carrier-pigeon", "at": 2})
    );
}

/// An install of an id that only an unreadable record names replaces that record, says so, and leaves
/// nothing of it behind: a removal that follows in the same listing must not write it back.
///
/// Why replace and not refuse: the list never shows such a record, so no one can remove it, and a
/// refusal would block the id for good (a corrupt record, or one a later build wrote, is exactly what
/// the user is trying to get past). An explicit install of an id is the user choosing the package of
/// that id; the notice is how the loss is not silent.
#[test]
fn an_install_over_an_unreadable_record_of_its_id_replaces_it_and_says_so() {
    let a = pack("org.example.a", "1.0.0");
    let old = b"the pack the unreadable record named".to_vec();
    let text = format!(
        r#"{{"v":1,"packs":{{"org.example.a":{{"sha":"{}","source":"carrier-pigeon","at":2}}}}}}"#,
        Sha::of(&old).hex()
    );
    let mut r = rig_with_index(text, &old);
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC], "not listed: this build cannot read it");

    r.install(&a, Source::Address, 3);
    assert_eq!(
        r.notices,
        [
            Notice::ReplacedUnreadable { id: "org.example.a".into() },
            Notice::Installed { id: "org.example.a".into(), version: "1.0.0".into(), sha: Sha::of(&a), until_restart: false },
        ]
    );
    assert_eq!(packs_of(&r, 0)["org.example.a"]["source"], "address");
    assert!(!r.files.contains_key(&pack_path(0, Sha::of(&old))), "the replaced record's pack is not left behind");
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"]);

    // The old record is gone from the store's memory too: removing the package empties the id.
    r.send(HudEvent::Removed { id: "org.example.a".into() });
    assert!(packs_of(&r, 0).is_empty(), "{:?}", packs_of(&r, 0));
    assert_eq!(r.ids(), [BUILTIN_CLASSIC]);
}

/// A removal takes every record of the id out, the ones this build cannot read included: a listed copy
/// in one candidate and an unreadable one in another both go, and an id only an unreadable record
/// names can be removed too.
#[test]
fn a_removal_drops_the_unreadable_records_of_the_id_too() {
    let a = pack("org.example.a", "1.0.0");
    let unreadable = |id: &str, sha: Sha| {
        format!(r#"{{"v":1,"packs":{{"{id}":{{"sha":"{}","source":"carrier-pigeon","at":2}}}}}}"#, sha.hex())
    };
    let mut r = Rig::with(&[(&a, Source::Drop, &[1])]);
    r.files.insert(index_path(0), unreadable("org.example.a", Sha::of(b"x")).into_bytes());
    r.refresh();
    assert_eq!(r.ids(), [BUILTIN_CLASSIC, "org.example.a"], "candidate 1 lists it");
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.a".into() }), Handled::Yes);
    assert!(packs_of(&r, 0).is_empty() && packs_of(&r, 1).is_empty());
    assert_eq!(r.notices.last(), Some(&Notice::Removed { id: "org.example.a".into() }));

    // Only an unreadable record names it: no listing offers a remove, but the command is honoured.
    let mut r = Rig::new();
    r.files.insert(index_path(0), unreadable("org.example.ghost", Sha::of(b"x")).into_bytes());
    r.refresh();
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.ghost".into() }), Handled::Yes);
    assert!(packs_of(&r, 0).is_empty());
    assert_eq!(r.notices.last(), Some(&Notice::Removed { id: "org.example.ghost".into() }));
    assert_eq!(r.send(HudEvent::Removed { id: "org.example.ghost".into() }), Handled::No);
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
fn the_index_file_round_trips_and_a_bad_record_costs_only_itself() {
    let mut index = Index::new();
    index.insert("org.example.a".to_string(), Record::new(Sha::of(b"a"), Source::Registry, 5));
    index.insert("org.example.b".to_string(), Record::new(Sha::of(b"b"), Source::Drop, 6));
    assert_eq!(parse_index(&index_bytes(&index, &Map::new())).index, index);
    assert_eq!(
        index_bytes(&index, &Map::new()),
        index_bytes(&index.clone(), &Map::new()),
        "the same index is the same bytes"
    );

    let good = Sha::of(b"g").hex();
    let text = format!(
        r#"{{"v":1,"packs":{{
            "org.example.good":{{"sha":"{good}","source":"drop","at":1}},
            "org.example.nosha":{{"source":"drop","at":1}},
            "org.example.badsha":{{"sha":"XYZ","source":"drop","at":1}},
            "org.example.newsource":{{"sha":"{good}","source":"carrier-pigeon","at":1}},
            "builtin:classic":{{"sha":"{good}","source":"drop","at":1}}
        }}}}"#
    );
    let parsed = parse_index(text.as_bytes());
    assert_eq!(parsed.index.keys().collect::<Vec<_>>(), ["org.example.good"]);
    // The ones this build cannot read are kept to be written back; the claim on the built-in is not.
    assert_eq!(
        parsed.kept.keys().collect::<Vec<_>>(),
        ["org.example.badsha", "org.example.newsource", "org.example.nosha"]
    );
    assert!(parsed.access == Access::Writable);
    for text in [&b"not json"[..], b"", b"[]"] {
        let parsed = parse_index(text);
        assert!(parsed.index.is_empty() && parsed.access == Access::ReadOnly, "{text:?}");
    }
}

#[test]
fn only_package_file_names_can_be_dropped_in() {
    assert!(drop_name_ok("my.plxhud.json"));
    for bad in [".plxhud.json", ".hidden.plxhud.json", "a/b.plxhud.json", "a\\b.plxhud.json", "x.json", "x.plxhud.json.bak", "../x.plxhud.json"] {
        assert!(!drop_name_ok(bad), "{bad:?}");
    }
}
