//! The player-HUD package store: which packages are installed, which one is selected, and the rules
//! that admit a new one (`docs/hud-plugins.md`, "Storage per install mode", "Trust and safety").
//!
//! `HudStore` is a tested `State + HudEvent -> Effects` machine in the shape of
//! `plx_media::player::recovery::OpenRecovery` (not a `StoreJob` store: it has no outbox and no
//! `Stores` wiring, so look for none). Its state is private and its one writer is
//! [`step`](HudStore::step). It starts no worker, reads no global, reads no clock and performs no
//! IO: every file it wants read, written, listed or deleted leaves as a [`HudEffect`] carrying its
//! address, and every answer returns as one addressed [`HudEvent`]. The directories it keeps files in
//! ([`Setup`], built by the runner from `paths::hud_candidates()`), the byte bound a file is read
//! under, the current time of an install and the profile's selection all arrive on events.
//!
//! **Validation is the caller's.** The format lives in `appkit::hud_format`, above this layer, so the
//! store never parses a package. It hashes the bytes it holds (computation) and asks for the verdict
//! with a [`HudEffect::Validate`]; the runner parses and compiles and answers [`HudEvent::Validated`].
//! Nothing is decided before that answer, and the sha the store trusts is always its own.
//!
//! **Layout** per candidate directory: `packs/<sha256>.plxhud.json` (content-addressed, so an update is
//! a new file), `installed.json` (id -> sha, source, installed-at), `drop/` (the sideload drop folder).
//! `index-cache.json` belongs to the registry gallery (PR13) and is neither read nor written here.
//! A listing reads every candidate; one sha found in two of them is one package; for one id the higher
//! `version` wins, and on a tie the earlier candidate.
//!
//! **Install** (from bytes, or from a drop file): hash, validate, judge ([`Refusal`] lists what is
//! refused), then write the pack FIRST and `installed.json` second, so the switch is one atomic rename
//! of the index. The first candidate that takes both writes is used; a package only the last,
//! volatile (`/tmp`-backed) candidate took is [`Entry::until_restart`]. A new package replaces an
//! installed one of its id only with a strictly higher `version` ([`Semver`]). `builtin:classic` is
//! refused from every source: the built-in entry is compiled in, always listed, always selectable.
//!
//! **Unreadable is not empty.** An `installed.json` that is missing is an empty candidate, free to take
//! the first install. One that failed to read, is not JSON, or says it is from a newer layout is listed
//! as far as it parses and never written (an install goes to the next candidate, a removal reports
//! itself incomplete), and a partial view never overwrites what it did not see: a record this build
//! cannot read (a source or a sha it does not know) and the fields of a record it can read that a later
//! build added are written back unchanged by every rewrite of their file. Only an install of the same
//! id replaces an unreadable record (the list never shows it, so no one could remove it; the install
//! says so with [`Notice::ReplacedUnreadable`]), and a removal of the id takes it out.
//!
//! **Work is sequential**: at most one request is in flight, so a runner may execute effects in the
//! order they are pushed and memory stays bounded by one file, with no cap on how many packages exist.
//! A `Refresh` replaces work in flight, so it ends an install or removal it displaces with a notice and,
//! once the indexes are read again, deletes the packs that work may have left that no index names (an
//! unreadable index shields its candidate). A process that dies mid-install is not covered: the store
//! keeps no list of the packs on disk, only of those an index names, and a pack left that way costs
//! bytes, never correctness (installing the same package again writes over it).
//!
//! | event | in state | effects, next state |
//! |---|---|---|
//! | `Refresh` | any: it replaces a listing, install or removal in flight (an install ends `NotWritten`, a removal `RemoveIncomplete`) | per candidate `ReadFile(installed.json)`; `Delete` of a displaced work's pack no index names; then `ReadFile(packs/<sha>)` + `Validate` per distinct sha; then `ScanDir(drop)` and `ReadFile` + `Validate` per file; idle |
//! | `Install` | idle | `Validate`; on a loadable verdict `WriteFile(pack)` then `WriteFile(installed.json)`; `Notice::Installed` or `Notice::Refused`/`NotWritten` |
//! | `InstallDrop` | idle | `ReadFile(drop file)`, then as `Install` (the bytes must hash to the sha the user saw) |
//! | `Removed` | idle | `WriteFile(installed.json)` per candidate holding a record of the id (an unreadable one too), `Delete(pack)`; `Selection(builtin)` when the selected one went |
//! | `Selected` | any | `Selection(id)` for the built-in or a listed, loadable package; else not handled |
//!
//! A pair not listed (a result for a tag nothing awaits, an install while busy) is [`Handled::No`] and
//! changes nothing. A runner that loses an answer is recovered by the next `Refresh`.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use plx_base::sha256::sha256;
use plx_machine::machine::Handled;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The id of the built-in HUD: compiled in, always listed, always selectable, and claimed by no file.
/// The same literal as `appkit::hud_format::schema::BUILTIN_CLASSIC_ID` (the format accepts it so the
/// built-in's own file parses). appkit is above this layer, so nothing here can compare the two: the
/// test that pins them equal belongs where both are visible, with the runner that wires this store
/// (PR10b).
pub const BUILTIN_CLASSIC: &str = "builtin:classic";

const PACKS_DIR: &str = "packs";
const DROP_DIR: &str = "drop";
const INDEX_FILE: &str = "installed.json";
const PACK_SUFFIX: &str = ".plxhud.json";
/// The `v` of an `installed.json` this build writes. A build that adds a field to a record keeps `v`
/// (an older build carries the field through, [`Record::extra`]); a change an older build could not
/// carry through (a new top-level key, a different meaning for a field) raises it, and an older build
/// then reads the file and never writes it.
const INDEX_FORMAT: u64 = 1;

// ---------------------------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------------------------

/// The address of one request: the effect carries it and the answering event returns it unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag(u32);

/// A package's identity: the SHA-256 of its bytes. Content-addressed, so a name can never claim other
/// content than it holds.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha([u8; 32]);

impl Sha {
    /// The hash of `bytes` (`plx_base::sha256`).
    pub fn of(bytes: &[u8]) -> Self {
        Sha(sha256(bytes))
    }

    /// The 64 lowercase hex digits a pack file is named by.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The inverse of [`hex`](Self::hex); anything but 64 lowercase hex digits is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(Sha(out))
    }
}

impl std::fmt::Debug for Sha {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha({})", self.hex())
    }
}

/// A package's `version`: numeric `MAJOR.MINOR.PATCH` (the format admits no pre-release or build
/// metadata), ordered field by field. It only orders updates of one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Semver(u64, u64, u64);

impl Semver {
    /// `None` for anything but three plain decimal numbers (no sign, no leading zero) joined by dots.
    pub fn parse(text: &str) -> Option<Self> {
        fn number(part: &str) -> Option<u64> {
            let plain = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            if plain {
                part.parse().ok()
            } else {
                None
            }
        }
        let mut parts = text.split('.');
        let version = Semver(number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?);
        parts.next().is_none().then_some(version)
    }
}

/// Where a package came from. A sideload and a drop are unreviewed; the registry is the only reviewed
/// source and sets itself (PR13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A file in `drop/`, copied onto the set over ssh.
    Drop,
    /// An address typed in Settings (`gh:owner/repo[@tag]`, an `https://` URL, an RFC1918 `http://`).
    Address,
    /// The registry gallery.
    Registry,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Drop => "drop",
            Source::Address => "address",
            Source::Registry => "registry",
        }
    }

    /// The tier a package from this source has.
    pub fn trust(self) -> Trust {
        match self {
            Source::Drop | Source::Address => Trust::Unreviewed,
            Source::Registry => Trust::Registry,
        }
    }
}

/// How far a package is vouched for. Every tier passes the format invariants; only the registry's is
/// reviewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Compiled in.
    BuiltIn,
    /// CI plus human review.
    Registry,
    /// A sideload or a drop: no review, and Settings says so.
    Unreviewed,
}

/// What the validator read out of a package that parsed. The store never reads `name`; it is carried
/// for the list the Settings page draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub format: String,
}

/// Why a package that parsed cannot run on this app. A file that stays on disk with one of these is
/// listed and marked; a new install with one is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unloadable {
    /// "Made for a newer PlxNative": an unsupported major, or a minor above the app's.
    NewerFormat,
    /// "Uses an older HUD format": a major the app no longer compiles.
    OlderFormat,
    /// Outside the package's own `app.min`/`app.max`.
    AppRange,
}

/// The runner's answer to a [`HudEffect::Validate`]: the strict parse and the compile, reduced to what
/// the store decides on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Parses, compiles, and runs on this app.
    Loadable(Meta),
    /// Parses, but this app cannot run it.
    Unloadable(Meta, Unloadable),
    /// Does not parse or breaks an invariant: nothing about it is trusted, not even its id.
    Invalid,
}

/// Why an install is refused. Nothing is written for any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Claims `builtin:classic`, which only the compiled-in entry may carry.
    BuiltinId,
    /// Did not validate (or carries a version that is not `MAJOR.MINOR.PATCH`).
    Invalid,
    /// Validated, but this app cannot run it.
    Unloadable(Unloadable),
    /// This exact package is the installed one of its id.
    AlreadyInstalled,
    /// An installed package of the id has the same or a higher version; remove it first to go back.
    NotNewer,
    /// Larger than the bound the runner set.
    TooLarge,
    /// A drop file no longer holds what the list showed.
    Changed,
}

/// A file read or a directory listing failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Larger than the effect's `max_bytes`; nothing was read.
    TooLarge,
    /// There is no such file or directory: the runner saw `NotFound` and nothing else. An empty
    /// candidate is not an unreadable one.
    Missing,
    /// Unreadable, not a directory, or any other failure: what is there is unknown.
    Failed,
}

/// The outcome of a [`HudEffect::WriteFile`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The whole file is on disk under its name (written beside it and renamed).
    Done,
    /// The directory could not be made or the file not written: read-only, full, not permitted.
    Refused,
}

/// One place the store may keep files, best first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub dir: PathBuf,
    /// A restart clears it (the `/tmp`-backed device fallback): a package only this took is
    /// installed "until restart".
    pub volatile: bool,
}

/// The directories and the bound the runner hands the store when a listing starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    /// `paths::hud_candidates()` in order, each marked volatile or not by the runner.
    pub candidates: Vec<Candidate>,
    /// The most bytes of one file the store asks to read (`appkit::hud_format`'s package bound, which
    /// the validator enforces too; also bounds an `installed.json`) and the most an install may carry.
    /// `Setup::default()` leaves it 0, so an `Install` before the first `Refresh` is refused as
    /// [`Refusal::TooLarge`]: the runner that wires the store (PR10b) sets it from `appkit`'s
    /// `MAX_PACKAGE_BYTES` in the `Setup` it sends.
    pub max_pack_bytes: usize,
}

/// A place under one candidate directory: the runner makes the root (owner-only when
/// `paths::hud_dir_is_private`) and any folder between it and the file. The runner refuses a root
/// that is a symlink or that another uid owns, and a file it cannot confirm is its own: the store
/// trusts the `source` an `installed.json` claims, so a directory anyone can write is not one it may
/// read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub root: PathBuf,
    /// Relative to the root, `/`-separated.
    pub rel: String,
}

impl Loc {
    /// The file's path.
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.rel)
    }
}

/// What the store reports to the Settings page when an operation ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Installed { id: String, version: String, sha: Sha, until_restart: bool },
    Refused { sha: Sha, reason: Refusal },
    /// Every candidate refused the pack or the index: nothing was installed.
    NotWritten { sha: Sha },
    Removed { id: String },
    /// Some candidate kept the entry (its index could not be rewritten), so the id may list again.
    RemoveIncomplete { id: String },
    /// An install took the place of a record of its id in `installed.json` that this build cannot read
    /// (so the list never showed it). Sent just before `Installed`.
    ReplacedUnreadable { id: String },
}

/// The events [`HudStore::step`] takes.
#[derive(Debug)]
pub enum HudEvent {
    /// HUD settings opened (or the store started): forget the listing and read every candidate. Always
    /// taken, and it replaces whatever is in flight. A listing is dropped silently. An install ends
    /// with [`Notice::NotWritten`] and a removal with [`Notice::RemoveIncomplete`] (the listing that
    /// follows shows what the disk holds, which may be the finished work: an answer can be lost after
    /// the write landed), and a pack the displaced work may have left that no index names after the
    /// re-read is deleted. `selected` is the profile's setting (empty: the built-in).
    Refresh { setup: Setup, selected: String },
    /// A [`HudEffect::ScanDir`] answered with the file names in the directory.
    Scanned { tag: Tag, result: Result<Vec<String>, ReadError> },
    /// A [`HudEffect::ReadFile`] answered.
    Read { tag: Tag, result: Result<Vec<u8>, ReadError> },
    /// A [`HudEffect::Validate`] answered.
    Validated { tag: Tag, sha: Sha, verdict: Verdict },
    /// A [`HudEffect::WriteFile`] answered.
    Written { tag: Tag, outcome: WriteOutcome },
    /// Install these bytes (a fetch, PR11; the user has confirmed). `now` is the unix time recorded as
    /// installed-at.
    Install { bytes: Vec<u8>, source: Source, now: u64 },
    /// Install the drop file the listing showed under this sha (the user has confirmed).
    InstallDrop { sha: Sha, now: u64 },
    /// The user picked a package (or the built-in).
    Selected { id: String },
    /// The user removed a package.
    Removed { id: String },
}

/// The work the store asks of the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudEffect {
    /// List a directory: [`HudEvent::Scanned`].
    ScanDir { tag: Tag, at: Loc },
    /// Read a file, at most `max_bytes`: [`HudEvent::Read`].
    ReadFile { tag: Tag, at: Loc, max_bytes: usize },
    /// Write a file whole and atomically (beside it, then renamed): [`HudEvent::Written`].
    WriteFile { tag: Tag, at: Loc, bytes: Arc<[u8]> },
    /// Delete a file; one already gone is not an error. Nothing returns: a pack left behind costs
    /// bytes, never correctness.
    Delete { at: Loc },
    /// Parse and compile these bytes (the format lives above this layer): [`HudEvent::Validated`].
    Validate { tag: Tag, sha: Sha, bytes: Arc<[u8]> },
    /// The selection the profile setting must now hold (the store changed it, or the user picked it).
    Selection { id: String },
    /// An operation ended: the Settings page says so.
    Notice(Notice),
}

/// One row of the package list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    /// What the package declares; `None` for the built-in, which the page names itself.
    pub meta: Option<Meta>,
    /// `None` for the built-in.
    pub sha: Option<Sha>,
    pub source: Option<Source>,
    pub trust: Trust,
    /// Whether this app can run it. The built-in always can.
    pub loadable: Result<(), Unloadable>,
    /// Only the volatile candidate holds it: a restart clears it.
    pub until_restart: bool,
    /// The profile's selection names this row. It may name a package that cannot load: what plays is
    /// [`HudStore::resolved`].
    pub selected: bool,
}

/// One file found in `drop/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropFile {
    pub name: String,
    /// `None` when the file was too large to read.
    pub sha: Option<Sha>,
    pub meta: Option<Meta>,
    /// `Ok` when installing it now would be accepted.
    pub status: Result<(), Refusal>,
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

type Index = BTreeMap<String, Record>;

/// One `installed.json` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    sha: Sha,
    source: Source,
    at: u64,
    /// The fields of the entry this build does not know (a later build added them): written back with
    /// it by every rewrite, and gone with it when the record is replaced or removed.
    extra: Map<String, Value>,
}

impl Record {
    fn new(sha: Sha, source: Source, at: u64) -> Self {
        Record { sha, source, at, extra: Map::new() }
    }
}

/// Whether the store may rewrite a candidate's `installed.json`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Access {
    #[default]
    Writable,
    /// What is there could not be read whole (a read failure, text that is not JSON, a layout from a
    /// newer build): it is listed as far as it parses and never written, so a partial view cannot
    /// overwrite what it did not see.
    ReadOnly,
}

#[derive(Clone, Default)]
struct Dir {
    index: Index,
    /// The entries of the file this build cannot read, written back unchanged with every rewrite.
    kept: Map<String, Value>,
    access: Access,
}

/// A package whose bytes were read and validated.
struct Loaded {
    meta: Meta,
    loadable: Result<(), Unloadable>,
    volatile: bool,
}

struct Dropped {
    /// The candidate whose `drop/` holds the file.
    dir: usize,
    name: String,
    found: Found,
}

enum Found {
    TooLarge,
    Parsed { sha: Sha, verdict: Verdict },
}

/// A listing step.
enum Job {
    /// Read candidate `dir`'s `installed.json`.
    Index(usize),
    /// Turn the indexes read so far into `Pack` jobs.
    Plan,
    /// Load the pack with this sha from the first of `dirs` that holds a good copy.
    Pack { sha: Sha, dirs: VecDeque<usize> },
    /// List candidate `dir`'s `drop/`.
    Scan(usize),
    /// Read and validate one drop file.
    Drop { dir: usize, name: String },
}

enum Stage {
    Io,
    Validation(Sha),
}

struct Flight {
    tag: Tag,
    job: Job,
    stage: Stage,
}

struct Listing {
    todo: VecDeque<Job>,
    flight: Option<Flight>,
    /// Packs a displaced install or removal may have left unlisted, by candidate directory: deleted
    /// once the indexes are read again, if none of them names the pack.
    stray: Vec<(PathBuf, Sha)>,
}

/// Bytes on their way to the disk, and what is known about them.
struct Offer {
    sha: Sha,
    source: Source,
    now: u64,
    bytes: Arc<[u8]>,
}

enum Write {
    Pack,
    /// The index being written, and the sha this id had in that directory before (its pack is
    /// deleted once nothing there names it).
    Index { index: Index, replaced: Option<Sha> },
}

enum Step {
    /// A drop file is being read again so that what is installed is what was listed.
    Reading { sha: Sha, now: u64 },
    Validating(Offer),
    Writing { offer: Offer, meta: Meta, dir: usize, what: Write },
}

struct Installing {
    tag: Tag,
    step: Step,
}

struct Removing {
    id: String,
    /// The candidates still to rewrite.
    dirs: VecDeque<usize>,
    incomplete: bool,
    flight: Option<Rewrite>,
}

/// An index write of a removal, in flight: what the candidate holds once it lands.
struct Rewrite {
    tag: Tag,
    dir: usize,
    index: Index,
    kept: Map<String, Value>,
    /// The packs the removed records named (a record this build cannot read may name one too).
    gone: Vec<Sha>,
}

enum Op {
    Idle,
    Listing(Listing),
    Installing(Installing),
    Removing(Removing),
}

/// The HUD package store. See the module doc.
pub struct HudStore {
    setup: Setup,
    /// One per candidate, in order.
    dirs: Vec<Dir>,
    /// Every package read and validated this listing (or installed since), by sha.
    packs: BTreeMap<Sha, Loaded>,
    drops: Vec<Dropped>,
    selected: String,
    op: Op,
    tags: u32,
}

impl Default for HudStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HudStore {
    pub fn new() -> Self {
        HudStore {
            setup: Setup::default(),
            dirs: Vec::new(),
            packs: BTreeMap::new(),
            drops: Vec::new(),
            selected: BUILTIN_CLASSIC.to_string(),
            op: Op::Idle,
            tags: 0,
        }
    }

    // -------- readers --------

    /// Whether a listing, install or removal is running (installing and removing wait for the idle).
    pub fn busy(&self) -> bool {
        !matches!(self.op, Op::Idle)
    }

    /// The selection as the profile holds it; it may name a package that is gone or cannot load.
    pub fn selected(&self) -> &str {
        &self.selected
    }

    /// What a playback starts with: the selection when it is listed and loadable, else the built-in.
    pub fn resolved(&self) -> &str {
        if self.selected != BUILTIN_CLASSIC
            && self.winners().get(self.selected.as_str()).is_some_and(|w| w.loaded.loadable.is_ok())
        {
            &self.selected
        } else {
            BUILTIN_CLASSIC
        }
    }

    /// The package list: the built-in first, then one row per installed id, by id.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out = vec![Entry {
            id: BUILTIN_CLASSIC.to_string(),
            meta: None,
            sha: None,
            source: None,
            trust: Trust::BuiltIn,
            loadable: Ok(()),
            until_restart: false,
            selected: self.selected == BUILTIN_CLASSIC,
        }];
        for (id, win) in self.winners() {
            out.push(Entry {
                id: id.to_string(),
                meta: Some(win.loaded.meta.clone()),
                sha: Some(win.record.sha),
                source: Some(win.record.source),
                trust: win.record.source.trust(),
                loadable: win.loaded.loadable,
                until_restart: win.loaded.volatile,
                selected: self.selected == id,
            });
        }
        out
    }

    /// The files found in `drop/`, one row per distinct sha, with whether installing them would be
    /// accepted now.
    pub fn drops(&self) -> Vec<DropFile> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for dropped in &self.drops {
            match &dropped.found {
                Found::TooLarge => out.push(DropFile {
                    name: dropped.name.clone(),
                    sha: None,
                    meta: None,
                    status: Err(Refusal::TooLarge),
                }),
                Found::Parsed { sha, verdict } => {
                    if seen.contains(sha) {
                        continue;
                    }
                    seen.push(*sha);
                    let meta = match verdict {
                        Verdict::Loadable(meta) | Verdict::Unloadable(meta, _) => Some(meta.clone()),
                        Verdict::Invalid => None,
                    };
                    out.push(DropFile {
                        name: dropped.name.clone(),
                        sha: Some(*sha),
                        meta,
                        status: self.judge(*sha, verdict).map(|_| ()),
                    });
                }
            }
        }
        out
    }

    // -------- the one writer --------

    /// The one writer. The table is in the module doc; a pair it does not list answers
    /// [`Handled::No`] and changes nothing.
    pub fn step(&mut self, ev: HudEvent, fx: &mut Vec<HudEffect>) -> Handled {
        match ev {
            HudEvent::Refresh { setup, selected } => {
                // Always taken: it is how a store whose runner lost an answer recovers. Whatever it
                // displaces wrote atomically and put the index last, so disk is consistent, and the
                // answers the displaced work is still owed carry a stale tag.
                self.begin_listing(setup, selected, fx);
                Handled::Yes
            }
            HudEvent::Selected { id } => self.select(id, fx),
            HudEvent::Install { bytes, source, now } => {
                if self.busy() {
                    return Handled::No;
                }
                let sha = Sha::of(&bytes);
                if bytes.len() > self.setup.max_pack_bytes {
                    fx.push(HudEffect::Notice(Notice::Refused { sha, reason: Refusal::TooLarge }));
                } else {
                    self.op = self.validate(Offer { sha, source, now, bytes: bytes.into() }, fx);
                }
                Handled::Yes
            }
            HudEvent::InstallDrop { sha, now } => {
                if self.busy() {
                    return Handled::No;
                }
                let Some((dir, name)) = self.drops.iter().find_map(|d| match &d.found {
                    Found::Parsed { sha: s, .. } if *s == sha => Some((d.dir, d.name.clone())),
                    _ => None,
                }) else {
                    return Handled::No;
                };
                let tag = self.tag();
                fx.push(self.read(tag, dir, format!("{DROP_DIR}/{name}")));
                self.op = Op::Installing(Installing { tag, step: Step::Reading { sha, now } });
                Handled::Yes
            }
            HudEvent::Removed { id } => self.begin_removal(id, fx),
            result @ (HudEvent::Scanned { .. }
            | HudEvent::Read { .. }
            | HudEvent::Validated { .. }
            | HudEvent::Written { .. }) => {
                let (op, handled) = match std::mem::replace(&mut self.op, Op::Idle) {
                    Op::Idle => (Op::Idle, Handled::No),
                    Op::Listing(listing) => self.listing_result(listing, result, fx),
                    Op::Installing(installing) => self.installing_result(installing, result, fx),
                    Op::Removing(removing) => self.removing_result(removing, result, fx),
                };
                self.op = op;
                handled
            }
        }
    }

    // -------- helpers --------

    fn tag(&mut self) -> Tag {
        self.tags = self.tags.wrapping_add(1);
        Tag(self.tags)
    }

    fn loc(&self, dir: usize, rel: String) -> Loc {
        Loc { root: self.setup.candidates.get(dir).map(|c| c.dir.clone()).unwrap_or_default(), rel }
    }

    fn read(&self, tag: Tag, dir: usize, rel: String) -> HudEffect {
        HudEffect::ReadFile { tag, at: self.loc(dir, rel), max_bytes: self.setup.max_pack_bytes }
    }

    fn volatile(&self, dir: usize) -> bool {
        self.setup.candidates.get(dir).is_some_and(|c| c.volatile)
    }

    fn select(&mut self, id: String, fx: &mut Vec<HudEffect>) -> Handled {
        let allowed = id == BUILTIN_CLASSIC
            || self.winners().get(id.as_str()).is_some_and(|w| w.loaded.loadable.is_ok());
        if !allowed {
            return Handled::No;
        }
        self.selected = id.clone();
        fx.push(HudEffect::Selection { id });
        Handled::Yes
    }

    /// The installed package of each id: across candidates the higher version wins, and on a tie the
    /// earlier candidate. An entry whose pack was not read, or whose pack declares another id, is not a
    /// package.
    fn winners(&self) -> BTreeMap<&str, Winner<'_>> {
        let mut best: BTreeMap<&str, Winner<'_>> = BTreeMap::new();
        for dir in &self.dirs {
            for (id, record) in &dir.index {
                let Some(loaded) = self.packs.get(&record.sha) else { continue };
                if loaded.meta.id != *id {
                    continue;
                }
                let win = Winner { record, loaded };
                let better = best.get(id.as_str()).map_or(true, |have| {
                    Semver::parse(&win.loaded.meta.version) > Semver::parse(&have.loaded.meta.version)
                });
                if better {
                    best.insert(id.as_str(), win);
                }
            }
        }
        best
    }

    /// Whether `verdict` for `sha` may be installed now, and the package it describes.
    fn judge(&self, sha: Sha, verdict: &Verdict) -> Result<Meta, Refusal> {
        let (meta, unloadable) = match verdict {
            Verdict::Invalid => return Err(Refusal::Invalid),
            Verdict::Loadable(meta) => (meta, None),
            Verdict::Unloadable(meta, why) => (meta, Some(*why)),
        };
        if meta.id == BUILTIN_CLASSIC {
            return Err(Refusal::BuiltinId);
        }
        if let Some(why) = unloadable {
            return Err(Refusal::Unloadable(why));
        }
        let Some(new) = Semver::parse(&meta.version) else { return Err(Refusal::Invalid) };
        if let Some(have) = self.winners().get(meta.id.as_str()) {
            if have.record.sha == sha {
                return Err(Refusal::AlreadyInstalled);
            }
            if Semver::parse(&have.loaded.meta.version).is_some_and(|old| new <= old) {
                return Err(Refusal::NotNewer);
            }
        }
        Ok(meta.clone())
    }

    /// Whether an index of candidate `dir` names the pack, in a record this build reads or one it keeps.
    fn referenced(&self, dir: usize, sha: Sha) -> bool {
        self.dirs.get(dir).is_some_and(|d| {
            d.index.values().any(|r| r.sha == sha) || d.kept.values().any(|raw| raw_sha(raw) == Some(sha))
        })
    }

    fn notice(fx: &mut Vec<HudEffect>, notice: Notice) {
        fx.push(HudEffect::Notice(notice));
    }

    // -------- listing --------

    /// End the install or removal a `Refresh` displaces with its notice, and return the packs it may
    /// have left behind. It must run before the new `Setup` replaces the candidates it indexes.
    fn displaced(&mut self, fx: &mut Vec<HudEffect>) -> Vec<(PathBuf, Sha)> {
        let at = |store: &Self, dir: usize, sha: Sha| store.setup.candidates.get(dir).map(|c| (c.dir.clone(), sha));
        let mut stray = Vec::new();
        match std::mem::replace(&mut self.op, Op::Idle) {
            Op::Idle | Op::Listing(_) => {}
            Op::Installing(Installing { step, .. }) => {
                let sha = match step {
                    Step::Reading { sha, .. } => sha,
                    Step::Validating(offer) => offer.sha,
                    Step::Writing { offer, dir, what, .. } => {
                        // The pack is down or on its way. If the index write reached the disk the id's
                        // old pack is the unlisted one, if not the new one is: only a re-read says.
                        stray.extend(at(self, dir, offer.sha));
                        if let Write::Index { replaced: Some(old), .. } = what {
                            stray.extend(at(self, dir, old).filter(|_| old != offer.sha));
                        }
                        offer.sha
                    }
                };
                Self::notice(fx, Notice::NotWritten { sha });
            }
            Op::Removing(Removing { id, flight, .. }) => {
                if let Some(Rewrite { dir, gone, .. }) = flight {
                    // The rewrite may have reached the disk: the packs it unlisted are the stray ones.
                    stray.extend(gone.into_iter().filter_map(|sha| at(self, dir, sha)));
                }
                Self::notice(fx, Notice::RemoveIncomplete { id });
            }
        }
        stray
    }

    fn begin_listing(&mut self, setup: Setup, selected: String, fx: &mut Vec<HudEffect>) {
        let stray = self.displaced(fx);
        let n = setup.candidates.len();
        self.dirs = vec![Dir::default(); n];
        self.packs.clear();
        self.drops.clear();
        self.selected = if selected.is_empty() { BUILTIN_CLASSIC.to_string() } else { selected };
        self.setup = setup;
        let mut todo: VecDeque<Job> = (0..n).map(Job::Index).collect();
        todo.push_back(Job::Plan);
        todo.extend((0..n).map(Job::Scan));
        self.op = self.advance(Listing { todo, flight: None, stray }, fx);
    }

    /// Delete the packs a displaced operation may have left that no index names now that every index
    /// has been read again. A candidate whose index could not be read whole is left alone: it may name
    /// them.
    fn reclaim(&self, stray: Vec<(PathBuf, Sha)>, fx: &mut Vec<HudEffect>) {
        for (root, sha) in stray {
            let Some(dir) = self.setup.candidates.iter().position(|c| c.dir == root) else { continue };
            if self.dirs[dir].access == Access::Writable && !self.referenced(dir, sha) {
                fx.push(HudEffect::Delete { at: self.loc(dir, pack_rel(sha)) });
            }
        }
    }

    /// The `Pack` jobs for every distinct sha the indexes name, each trying its candidates in order.
    fn plan_packs(&self) -> Vec<Job> {
        let mut by_sha: BTreeMap<Sha, VecDeque<usize>> = BTreeMap::new();
        for (dir, state) in self.dirs.iter().enumerate() {
            for record in state.index.values() {
                let dirs = by_sha.entry(record.sha).or_default();
                if dirs.back() != Some(&dir) {
                    dirs.push_back(dir);
                }
            }
        }
        by_sha.into_iter().map(|(sha, dirs)| Job::Pack { sha, dirs }).collect()
    }

    /// Issue the next listing request, or finish.
    fn advance(&mut self, mut listing: Listing, fx: &mut Vec<HudEffect>) -> Op {
        while let Some(job) = listing.todo.pop_front() {
            let tag = self.tag();
            let effect = match &job {
                Job::Plan => {
                    self.reclaim(std::mem::take(&mut listing.stray), fx);
                    for planned in self.plan_packs().into_iter().rev() {
                        listing.todo.push_front(planned);
                    }
                    continue;
                }
                Job::Index(dir) => self.read(tag, *dir, INDEX_FILE.to_string()),
                Job::Pack { sha, dirs } => match dirs.front() {
                    Some(dir) => self.read(tag, *dir, pack_rel(*sha)),
                    None => continue,
                },
                Job::Scan(dir) => HudEffect::ScanDir { tag, at: self.loc(*dir, DROP_DIR.to_string()) },
                Job::Drop { dir, name } => self.read(tag, *dir, format!("{DROP_DIR}/{name}")),
            };
            fx.push(effect);
            listing.flight = Some(Flight { tag, job, stage: Stage::Io });
            return Op::Listing(listing);
        }
        Op::Idle
    }

    fn listing_result(&mut self, mut listing: Listing, ev: HudEvent, fx: &mut Vec<HudEffect>) -> (Op, Handled) {
        let Some(flight) = listing.flight.take() else { return (Op::Listing(listing), Handled::No) };
        let Flight { tag, job, stage } = flight;
        let addressed = match &ev {
            HudEvent::Scanned { tag: t, .. }
            | HudEvent::Read { tag: t, .. }
            | HudEvent::Validated { tag: t, .. }
            | HudEvent::Written { tag: t, .. } => *t == tag,
            _ => false,
        };
        if !addressed {
            listing.flight = Some(Flight { tag, job, stage });
            return (Op::Listing(listing), Handled::No);
        }
        match (job, stage, ev) {
            (Job::Index(dir), Stage::Io, HudEvent::Read { result, .. }) => {
                self.dirs[dir] = match result {
                    Ok(bytes) => parse_index(&bytes),
                    // Nothing there yet: an empty candidate, free to take the first install.
                    Err(ReadError::Missing) => Dir::default(),
                    // Unknown, not empty: list nothing from it and write nothing to it.
                    Err(ReadError::TooLarge | ReadError::Failed) => Dir { access: Access::ReadOnly, ..Dir::default() },
                };
            }
            (Job::Scan(dir), Stage::Io, HudEvent::Scanned { result, .. }) => {
                let names = result.unwrap_or_default();
                for name in names.into_iter().filter(|n| drop_name_ok(n)).rev() {
                    listing.todo.push_front(Job::Drop { dir, name });
                }
            }
            (Job::Pack { sha, mut dirs }, Stage::Io, HudEvent::Read { result, .. }) => match result {
                Ok(bytes) if Sha::of(&bytes) == sha => {
                    let tag = self.tag();
                    fx.push(HudEffect::Validate { tag, sha, bytes: bytes.into() });
                    listing.flight = Some(Flight { tag, job: Job::Pack { sha, dirs }, stage: Stage::Validation(sha) });
                    return (Op::Listing(listing), Handled::Yes);
                }
                _ => {
                    // Missing, unreadable, too large or not what its name says: try the next copy.
                    dirs.pop_front();
                    if !dirs.is_empty() {
                        listing.todo.push_front(Job::Pack { sha, dirs });
                    }
                }
            },
            (Job::Pack { sha, dirs }, Stage::Validation(asked), HudEvent::Validated { sha: got, verdict, .. })
                if asked == got =>
            {
                let volatile = dirs.front().is_some_and(|d| self.volatile(*d));
                match verdict {
                    Verdict::Loadable(meta) => {
                        self.packs.insert(sha, Loaded { meta, loadable: Ok(()), volatile });
                    }
                    Verdict::Unloadable(meta, why) => {
                        self.packs.insert(sha, Loaded { meta, loadable: Err(why), volatile });
                    }
                    Verdict::Invalid => {}
                }
            }
            (Job::Drop { dir, name }, Stage::Io, HudEvent::Read { result, .. }) => match result {
                Ok(bytes) => {
                    let sha = Sha::of(&bytes);
                    let tag = self.tag();
                    fx.push(HudEffect::Validate { tag, sha, bytes: bytes.into() });
                    listing.flight = Some(Flight { tag, job: Job::Drop { dir, name }, stage: Stage::Validation(sha) });
                    return (Op::Listing(listing), Handled::Yes);
                }
                Err(ReadError::TooLarge) => self.drops.push(Dropped { dir, name, found: Found::TooLarge }),
                Err(ReadError::Missing | ReadError::Failed) => {}
            },
            (Job::Drop { dir, name }, Stage::Validation(asked), HudEvent::Validated { sha: got, verdict, .. })
                if asked == got =>
            {
                self.drops.push(Dropped { dir, name, found: Found::Parsed { sha: got, verdict } });
            }
            (job, stage, _) => {
                listing.flight = Some(Flight { tag, job, stage });
                return (Op::Listing(listing), Handled::No);
            }
        }
        (self.advance(listing, fx), Handled::Yes)
    }

    // -------- install --------

    /// Ask the runner whether these bytes are a package this app can run.
    fn validate(&mut self, offer: Offer, fx: &mut Vec<HudEffect>) -> Op {
        let tag = self.tag();
        fx.push(HudEffect::Validate { tag, sha: offer.sha, bytes: offer.bytes.clone() });
        Op::Installing(Installing { tag, step: Step::Validating(offer) })
    }

    fn installing_result(&mut self, installing: Installing, ev: HudEvent, fx: &mut Vec<HudEffect>) -> (Op, Handled) {
        let Installing { tag, step } = installing;
        let addressed = match &ev {
            HudEvent::Scanned { tag: t, .. }
            | HudEvent::Read { tag: t, .. }
            | HudEvent::Validated { tag: t, .. }
            | HudEvent::Written { tag: t, .. } => *t == tag,
            _ => false,
        };
        if !addressed {
            return (Op::Installing(Installing { tag, step }), Handled::No);
        }
        let op = match (step, ev) {
            (Step::Reading { sha, now }, HudEvent::Read { result, .. }) => match result {
                Ok(bytes) if Sha::of(&bytes) == sha => {
                    self.validate(Offer { sha, source: Source::Drop, now, bytes: bytes.into() }, fx)
                }
                _ => {
                    Self::notice(fx, Notice::Refused { sha, reason: Refusal::Changed });
                    Op::Idle
                }
            },
            (Step::Validating(offer), HudEvent::Validated { sha, verdict, .. }) if sha == offer.sha => {
                match self.judge(offer.sha, &verdict) {
                    Err(reason) => {
                        Self::notice(fx, Notice::Refused { sha: offer.sha, reason });
                        Op::Idle
                    }
                    Ok(meta) => self.write_pack(offer, meta, 0, fx),
                }
            }
            (Step::Writing { offer, meta, dir, what }, HudEvent::Written { outcome, .. }) => match (what, outcome) {
                (Write::Pack, WriteOutcome::Done) => {
                    // The pack is down: switch the candidate's index to it.
                    let mut index = self.dirs[dir].index.clone();
                    let replaced = index
                        .insert(meta.id.clone(), Record::new(offer.sha, offer.source, offer.now))
                        .map(|old| old.sha);
                    let tag = self.tag();
                    fx.push(HudEffect::WriteFile {
                        tag,
                        at: self.loc(dir, INDEX_FILE.to_string()),
                        bytes: index_bytes(&index, &self.dirs[dir].kept),
                    });
                    Op::Installing(Installing {
                        tag,
                        step: Step::Writing { offer, meta, dir, what: Write::Index { index, replaced } },
                    })
                }
                (Write::Pack, WriteOutcome::Refused) => self.write_pack(offer, meta, dir + 1, fx),
                (Write::Index { index, replaced }, WriteOutcome::Done) => {
                    self.dirs[dir].index = index;
                    // A record of this id that only the kept entries held is replaced by the one just
                    // written (`index_bytes` left it out): forget it, or a removal would write it back.
                    let unreadable = self.dirs[dir].kept.remove(&meta.id);
                    let until_restart = self.volatile(dir);
                    self.packs
                        .insert(offer.sha, Loaded { meta: meta.clone(), loadable: Ok(()), volatile: until_restart });
                    let mut old = Vec::new();
                    for sha in replaced.into_iter().chain(unreadable.as_ref().and_then(raw_sha)) {
                        if sha != offer.sha && !old.contains(&sha) && !self.referenced(dir, sha) {
                            old.push(sha);
                            fx.push(HudEffect::Delete { at: self.loc(dir, pack_rel(sha)) });
                        }
                    }
                    if unreadable.is_some() {
                        Self::notice(fx, Notice::ReplacedUnreadable { id: meta.id.clone() });
                    }
                    Self::notice(
                        fx,
                        Notice::Installed { id: meta.id, version: meta.version, sha: offer.sha, until_restart },
                    );
                    Op::Idle
                }
                (Write::Index { .. }, WriteOutcome::Refused) => {
                    // The pack is on disk but unlisted there: take it back and try the next candidate.
                    if !self.referenced(dir, offer.sha) {
                        fx.push(HudEffect::Delete { at: self.loc(dir, pack_rel(offer.sha)) });
                    }
                    self.write_pack(offer, meta, dir + 1, fx)
                }
            },
            (step, _) => return (Op::Installing(Installing { tag, step }), Handled::No),
        };
        (op, Handled::Yes)
    }

    /// Write the pack to candidate `dir`; with no candidate left, nothing was installed.
    fn write_pack(&mut self, offer: Offer, meta: Meta, dir: usize, fx: &mut Vec<HudEffect>) -> Op {
        // A candidate whose index is read-only cannot list the new package, so it is not a candidate.
        let Some(dir) = (dir..self.dirs.len()).find(|d| self.dirs[*d].access == Access::Writable) else {
            Self::notice(fx, Notice::NotWritten { sha: offer.sha });
            return Op::Idle;
        };
        let tag = self.tag();
        fx.push(HudEffect::WriteFile {
            tag,
            at: self.loc(dir, pack_rel(offer.sha)),
            bytes: offer.bytes.clone(),
        });
        Op::Installing(Installing { tag, step: Step::Writing { offer, meta, dir, what: Write::Pack } })
    }

    // -------- removal --------

    fn begin_removal(&mut self, id: String, fx: &mut Vec<HudEffect>) -> Handled {
        if self.busy() || id == BUILTIN_CLASSIC {
            return Handled::No;
        }
        let dirs: VecDeque<usize> = (0..self.dirs.len())
            .filter(|d| self.dirs[*d].index.contains_key(&id) || self.dirs[*d].kept.contains_key(&id))
            .collect();
        if dirs.is_empty() {
            return Handled::No;
        }
        self.op = self.advance_removal(Removing { id, dirs, incomplete: false, flight: None }, fx);
        Handled::Yes
    }

    fn advance_removal(&mut self, mut removing: Removing, fx: &mut Vec<HudEffect>) -> Op {
        while let Some(dir) = removing.dirs.pop_front() {
            if self.dirs[dir].access == Access::ReadOnly {
                // Cannot be taken out of an index nobody may write: the package stays listed.
                removing.incomplete = true;
                continue;
            }
            // The record of the id in this candidate, whether this build reads it or only keeps it.
            let mut index = self.dirs[dir].index.clone();
            let mut kept = self.dirs[dir].kept.clone();
            let mut gone: Vec<Sha> = index.remove(&removing.id).map(|r| r.sha).into_iter().collect();
            let unreadable = kept.remove(&removing.id);
            gone.extend(unreadable.as_ref().and_then(raw_sha).filter(|sha| !gone.contains(sha)));
            if gone.is_empty() && unreadable.is_none() {
                continue;
            }
            let tag = self.tag();
            fx.push(HudEffect::WriteFile {
                tag,
                at: self.loc(dir, INDEX_FILE.to_string()),
                bytes: index_bytes(&index, &kept),
            });
            removing.flight = Some(Rewrite { tag, dir, index, kept, gone });
            return Op::Removing(removing);
        }
        // Whatever no candidate lists any more is forgotten, and the selection follows the package.
        let dirs = &self.dirs;
        self.packs.retain(|sha, _| dirs.iter().any(|d| d.index.values().any(|r| r.sha == *sha)));
        let listed = self.winners().contains_key(removing.id.as_str());
        if !listed && self.selected == removing.id {
            self.selected = BUILTIN_CLASSIC.to_string();
            fx.push(HudEffect::Selection { id: self.selected.clone() });
        }
        let id = removing.id;
        Self::notice(
            fx,
            if removing.incomplete || listed { Notice::RemoveIncomplete { id } } else { Notice::Removed { id } },
        );
        Op::Idle
    }

    fn removing_result(&mut self, mut removing: Removing, ev: HudEvent, fx: &mut Vec<HudEffect>) -> (Op, Handled) {
        let HudEvent::Written { tag, outcome } = ev else { return (Op::Removing(removing), Handled::No) };
        match removing.flight.take() {
            Some(Rewrite { tag: awaited, dir, index, kept, gone }) if awaited == tag => {
                match outcome {
                    WriteOutcome::Done => {
                        self.dirs[dir].index = index;
                        self.dirs[dir].kept = kept;
                        for sha in gone.into_iter().filter(|sha| !self.referenced(dir, *sha)) {
                            fx.push(HudEffect::Delete { at: self.loc(dir, pack_rel(sha)) });
                        }
                    }
                    WriteOutcome::Refused => removing.incomplete = true,
                }
                (self.advance_removal(removing, fx), Handled::Yes)
            }
            other => {
                removing.flight = other;
                (Op::Removing(removing), Handled::No)
            }
        }
    }
}

/// The installed package of one id, as the list shows it.
struct Winner<'a> {
    record: &'a Record,
    loaded: &'a Loaded,
}

fn pack_rel(sha: Sha) -> String {
    format!("{PACKS_DIR}/{}{PACK_SUFFIX}", sha.hex())
}

/// A file name the drop folder may offer: a package file's name, with no way out of the folder.
fn drop_name_ok(name: &str) -> bool {
    name.len() > PACK_SUFFIX.len()
        && name.ends_with(PACK_SUFFIX)
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

// ---------------------------------------------------------------------------------------------
// installed.json
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct IndexFile {
    #[serde(default)]
    v: u64,
    #[serde(default)]
    packs: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct IndexRecord {
    sha: String,
    source: Source,
    at: u64,
    /// What a later build added to the entry; [`Record::extra`].
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// The sha an entry of `installed.json` names, read off the raw JSON: it works for a record this build
/// cannot otherwise read (an unknown source) too, so the pack that record names is not deleted from
/// under it.
fn raw_sha(raw: &Value) -> Option<Sha> {
    raw.get("sha").and_then(Value::as_str).and_then(Sha::parse)
}

/// The entries of an `installed.json`, leniently. One bad record (an unknown source, a sha that is not
/// 64 lowercase hex digits, a missing field) costs only itself: it is not listed, and it is kept to be
/// written back unchanged. A record that parses keeps any field beyond `sha`, `source` and `at`
/// ([`Record::extra`]). A claim on `builtin:classic` is dropped outright. A file that is not JSON,
/// or that a newer layout wrote (`v` above [`INDEX_FORMAT`]), lists what parses and is read-only.
fn parse_index(bytes: &[u8]) -> Dir {
    // An object, not just anything serde would fill a struct from (an empty array reads as all defaults).
    let file = serde_json::from_slice::<Value>(bytes)
        .ok()
        .filter(Value::is_object)
        .and_then(|doc| serde_json::from_value::<IndexFile>(doc).ok());
    let Some(file) = file else { return Dir { access: Access::ReadOnly, ..Dir::default() } };
    let mut dir = Dir { access: if file.v > INDEX_FORMAT { Access::ReadOnly } else { Access::Writable }, ..Dir::default() };
    for (id, value) in file.packs {
        if id == BUILTIN_CLASSIC {
            continue;
        }
        let record = serde_json::from_value::<IndexRecord>(value.clone())
            .ok()
            .and_then(|r| Some(Record { extra: r.extra, ..Record::new(Sha::parse(&r.sha)?, r.source, r.at) }));
        match (record, value) {
            (Some(record), _) => {
                dir.index.insert(id, record);
            }
            (None, Value::Object(raw)) => {
                dir.kept.insert(id, Value::Object(raw));
            }
            (None, _) => {}
        }
    }
    dir
}

/// An `installed.json` holding `index` and, unchanged, the `kept` entries no record of `index`
/// replaces; keys in order so the bytes are the same for the same state.
fn index_bytes(index: &Index, kept: &Map<String, Value>) -> Arc<[u8]> {
    let mut packs = Map::new();
    for (id, raw) in kept {
        if !index.contains_key(id) {
            packs.insert(id.clone(), raw.clone());
        }
    }
    for (id, record) in index {
        let mut entry = record.extra.clone();
        entry.insert("sha".to_string(), Value::String(record.sha.hex()));
        entry.insert("source".to_string(), Value::String(record.source.as_str().to_string()));
        entry.insert("at".to_string(), Value::from(record.at));
        packs.insert(id.clone(), Value::Object(entry));
    }
    let mut doc = Map::new();
    doc.insert("v".to_string(), Value::from(INDEX_FORMAT));
    doc.insert("packs".to_string(), Value::Object(packs));
    Value::Object(doc).to_string().into_bytes().into()
}

#[cfg(test)]
mod tests;
