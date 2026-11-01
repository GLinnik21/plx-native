//! The runners of the player-HUD package store (`plx_data::hudpacks::HudStore`, `docs/hud-plugins.md`,
//! "Storage per install mode") and the driver that keeps what the player draws in step with it.
//!
//! **The store is pure**; this file is its IO and nothing else. [`HudRunner`] executes the store's
//! `ScanDir`, `ReadFile`, `WriteFile` and `Delete` on the storage worker, answers `Validate` with
//! `appkit::hud_format::admit` (parse, invariants, the app's format range) and returns each result as the
//! machine's own addressed event. The effects reach the worker in the order the store pushed them, so
//! a `Delete` never overtakes the write it follows. Every answer wakes the frame loop
//! (`idle::invalidate`), because an idle loop does not iterate on its own.
//!
//! **The driver** ([`HudDriver`], owned by the Bridge) steps the store with the answers, executes the
//! effects it returns (`Selection` persists the profile's pick, `Notice` is kept for the Settings page) and
//! decides what the player draws: the compiled package of the store's `resolved` selection, compiled
//! with the television's own measurer and cached per (package sha256, app version, language), or
//! `builtin:classic`.
//!
//! * **Never on the frame path.** A tick with nothing in flight and nothing changed is a flag test. A
//!   compile is two ticks (the lint, then the load), never the draw, and until it lands the player draws what it had. Nothing here runs on selection except through the Settings
//!   page's commands and the seat trigger.
//! * **Never while a player is up.** The measurer is the main thread's, so a pack that lands while a player page is
//!   mounted or a playback is starting waits ([`Env::player`]); the compile runs on the first tick with no player, and the next
//!   playback draws the choice. Normal boots compile on the way to Home, before any player. The lint and the load are two ticks,
//!   so one frame never carries both compiles.
//! * **Never while a player is up.** The measurer is the main thread's, so a pack that lands while a player page is
//!   mounted or a playback is starting waits ([`Env::player`]); the compile runs on the first tick with no player, and the next
//!   playback draws the choice. Normal boots compile on the way to Home, before any player. The lint and the load are two ticks,
//!   so one frame never carries both compiles.
//! * **Fit is the TV's call.** A package that parses and passes the invariants can still not fit this
//!   language (`lint` with the TV's measurer, errors only: the TV applies no margin). It draws as classic
//!   in that language, one `hud: fallback` line is logged per (package, language), the selection is
//!   kept, and the page shows the reason ([`HudDriver::unfit`]).
//! * **No second compile path.** The compile is `player_hud::loaded::load`, the one the dev package and
//!   classic use.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plx_appkit::hud_format::admit::{admit, Admission, Cannot};
use plx_appkit::hud_format::lint::{lint_with, LintLang, LintOptions};
use plx_appkit::hud_format::parse::MAX_PACKAGE_BYTES;
use plx_appkit::hud_format::Severity;
use plx_appkit::player_hud::loaded::{load, LoadedHud};
use plx_base::storage_worker::submit_retained;
use plx_data::hudpacks::{
    Candidate, Entry, HudEffect, HudEvent, HudStore, Loc, Meta, Notice, ReadError, Setup, Sha, Unloadable, Verdict,
    WriteOutcome, BUILTIN_CLASSIC,
};
use plx_machine::machine::Measure;
use plx_platform::i18n::LocaleContext;

// ---------------------------------------------------------------------------------------------
// The runner: the store's effects as blocking IO on the storage worker
// ---------------------------------------------------------------------------------------------

/// The directories the store keeps files in, best first, for this build ([`plx_base::paths::hud_candidates`]):
/// the one the `/tmp` runtime root backs on a device is volatile, so a package only it took is "installed
/// until restart". `max_pack_bytes` is the format's own bound.
pub(crate) fn live_setup() -> Setup {
    let runtime = plx_base::paths::in_runtime_dir("huds");
    let candidates = plx_base::paths::hud_candidates()
        .into_iter()
        .map(|dir| {
            let volatile = !plx_base::paths::ENV_STEERABLE && dir == runtime;
            Candidate { dir, volatile }
        })
        .collect();
    Setup { candidates, max_pack_bytes: MAX_PACKAGE_BYTES }
}

/// One effect's blocking work. `None` is an effect with no answer (`Delete`).
fn run_effect(effect: HudEffect, app_version: &str) -> Option<HudEvent> {
    match effect {
        HudEffect::ScanDir { tag, at } => Some(HudEvent::Scanned { tag, result: scan_dir(&at) }),
        HudEffect::ReadFile { tag, at, max_bytes } => Some(HudEvent::Read { tag, result: read_file(&at, max_bytes) }),
        HudEffect::WriteFile { tag, at, bytes } => Some(HudEvent::Written { tag, outcome: write_file(&at, &bytes) }),
        HudEffect::Delete { at } => {
            delete_file(&at);
            None
        }
        HudEffect::Validate { tag, sha, bytes } => Some(HudEvent::Validated { tag, sha, verdict: verdict(&bytes, app_version) }),
        // Not IO: the driver handles these.
        HudEffect::Selection { .. } | HudEffect::Notice(_) => None,
    }
}

/// `appkit`'s verdict as the store's.
fn verdict(bytes: &[u8], app_version: &str) -> Verdict {
    let meta = |f: plx_appkit::hud_format::admit::Facts| Meta { id: f.id, name: f.name, version: f.version, format: f.format };
    match admit(bytes, app_version) {
        Admission::Runs(facts) => Verdict::Loadable(meta(facts)),
        Admission::Cannot(facts, why) => Verdict::Unloadable(
            meta(facts),
            match why {
                Cannot::NewerFormat => Unloadable::NewerFormat,
                Cannot::OlderFormat => Unloadable::OlderFormat,
                Cannot::AppRange => Unloadable::AppRange,
            },
        ),
        Admission::Invalid => Verdict::Invalid,
    }
}

/// A root in a directory anyone can write ([`plx_base::paths::hud_dir_is_private`]) is ours alone: not a
/// symlink and owned by this uid. The store trusts the `source` an `installed.json` claims, so it may
/// not read one a stranger could have written. Every other root keeps the mode it has.
fn root_is_trusted(root: &Path) -> bool {
    root_trusted_as(root, plx_base::paths::hud_dir_is_private(root))
}

/// [`root_is_trusted`] for a root known to be `private` or not. A private root that does not exist yet is
/// trusted (there is nothing of a stranger's to read, and creating it checks it again afterwards); one that
/// exists must be a real directory of ours that no one else can write.
fn root_trusted_as(root: &Path, private: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    if !private {
        return true;
    }
    match std::fs::symlink_metadata(root) {
        Ok(meta) => meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Whether `file` (already known to be a regular file, not a link) is one this process may believe: under a
/// private root it must be ours; the drop folder is written by whoever copies a file in, so it is not held to it.
fn file_is_trusted(at: &Loc, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    !plx_base::paths::hud_dir_is_private(&at.root)
        || at.rel.starts_with("drop/")
        || meta.uid() == unsafe { libc::geteuid() }
}

fn classify(error: &std::io::Error) -> ReadError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ReadError::Missing
    } else {
        ReadError::Failed
    }
}

fn scan_dir(at: &Loc) -> Result<Vec<String>, ReadError> {
    if !root_is_trusted(&at.root) {
        return Err(ReadError::Failed);
    }
    let dir = at.path();
    let entries = std::fs::read_dir(&dir).map_err(|e| classify(&e))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| classify(&e))?;
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

fn read_file(at: &Loc, max_bytes: usize) -> Result<Vec<u8>, ReadError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    if !root_is_trusted(&at.root) {
        return Err(ReadError::Failed);
    }
    let path = at.path();
    let meta = std::fs::symlink_metadata(&path).map_err(|e| classify(&e))?;
    if !meta.is_file() || !file_is_trusted(at, &meta) {
        return Err(ReadError::Failed);
    }
    if meta.len() > max_bytes as u64 {
        return Err(ReadError::TooLarge);
    }
    let mut bytes = Vec::new();
    // Never follow a link swapped in after the check, and never block on a FIFO swapped in for the file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|e| classify(&e))?;
    if !file.metadata().map_err(|e| classify(&e))?.is_file() {
        return Err(ReadError::Failed);
    }
    // One byte over the bound proves a file that grew after the check.
    file.take(max_bytes as u64 + 1).read_to_end(&mut bytes).map_err(|e| classify(&e))?;
    if bytes.len() > max_bytes {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// Create the directory (owner-only when the root must be private) and every folder to the file.
fn make_dirs(at: &Loc) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    if plx_base::paths::hud_dir_is_private(&at.root) {
        builder.mode(0o700);
    }
    let path = at.path();
    builder.create(path.parent().unwrap_or(&at.root))?;
    if root_is_trusted(&at.root) {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the store's root is not ours"))
    }
}

/// Beside the file, then renamed over it: a reader sees the old file or the whole new one.
fn write_file(at: &Loc, bytes: &[u8]) -> WriteOutcome {
    use std::io::Write;
    if make_dirs(at).is_err() {
        return WriteOutcome::Refused;
    }
    let path = at.path();
    // One name per target, so a write that died leaves a file the next write overwrites.
    let mut temp = path.clone().into_os_string();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    match written {
        Ok(()) => WriteOutcome::Done,
        Err(_) => {
            let _ = std::fs::remove_file(&temp);
            WriteOutcome::Refused
        }
    }
}

fn delete_file(at: &Loc) {
    if root_is_trusted(&at.root) {
        let _ = std::fs::remove_file(at.path());
    }
}

/// The bytes of the pack `sha` from the first candidate that holds a copy whose hash is `sha`: what the
/// player compiles. A file that was altered, or went missing since the listing, is `None`.
fn read_pack(candidates: &[PathBuf], sha: Sha) -> Option<Vec<u8>> {
    let rel = format!("packs/{}.plxhud.json", sha.hex());
    candidates.iter().find_map(|root| {
        let bytes = read_file(&Loc { root: root.clone(), rel: rel.clone() }, MAX_PACKAGE_BYTES).ok()?;
        (Sha::of(&bytes) == sha).then_some(bytes)
    })
}

enum Executor {
    /// The storage worker: the shipped one.
    Worker,
    /// Runs each effect where it is started: a host test's, so a test needs no thread and no clock.
    #[cfg(test)]
    Inline,
}

enum Slot<T> {
    Waiting(std::sync::mpsc::Receiver<T>),
    #[cfg(test)]
    Done(Option<T>),
}

impl<T> Slot<T> {
    /// The answer, `Some(None)` for a worker that died without one, `None` while it is still running.
    fn poll(&mut self) -> Option<Option<T>> {
        match self {
            Slot::Waiting(answer) => match answer.try_recv() {
                Ok(answer) => Some(Some(answer)),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(None),
            },
            #[cfg(test)]
            Slot::Done(answer) => Some(answer.take()),
        }
    }
}

impl Executor {
    fn run<T: Send + 'static>(&self, job: impl FnOnce() -> T + Send + 'static) -> Slot<T> {
        match self {
            Executor::Worker => {
                // The answer is sent BEFORE the loop is woken: a wake that came first could find nothing
                // yet, let the loop go idle again and never see the answer.
                let (reply, answer) = std::sync::mpsc::channel();
                let _ = submit_retained(move || {
                    let _ = reply.send(job());
                    plx_machine::idle::invalidate();
                });
                Slot::Waiting(answer)
            }
            #[cfg(test)]
            Executor::Inline => Slot::Done(Some(job())),
        }
    }
}

/// The IO effects of the store in flight, in the order they were started.
pub(crate) struct HudRunner {
    exec: Executor,
    app_version: Arc<str>,
    effects: VecDeque<Slot<Option<HudEvent>>>,
}

impl HudRunner {
    pub(crate) fn live(app_version: &str) -> Self {
        Self { exec: Executor::Worker, app_version: app_version.into(), effects: VecDeque::new() }
    }

    #[cfg(test)]
    pub(crate) fn inline(app_version: &str) -> Self {
        Self { exec: Executor::Inline, app_version: app_version.into(), effects: VecDeque::new() }
    }

    /// Nothing is running.
    pub(crate) fn is_idle(&self) -> bool {
        self.effects.is_empty()
    }

    /// Start the IO effects of `fx`, in order, and leave the ones that are not IO in `fx`.
    fn start(&mut self, fx: &mut Vec<HudEffect>) {
        let mut rest = Vec::new();
        for effect in fx.drain(..) {
            if matches!(effect, HudEffect::Selection { .. } | HudEffect::Notice(_)) {
                rest.push(effect);
                continue;
            }
            let version = Arc::clone(&self.app_version);
            self.effects.push_back(self.exec.run(move || run_effect(effect, &version)));
        }
        *fx = rest;
    }

    /// Answers that are ready, oldest first: the front of the queue only, so the order the store pushed is the order it
    /// hears them in.
    fn ready(&mut self, out: &mut Vec<HudEvent>) {
        while let Some(front) = self.effects.front_mut() {
            let Some(answer) = front.poll() else { break };
            self.effects.pop_front();
            out.extend(answer.flatten());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The driver: the store, its runner and what the player draws
// ---------------------------------------------------------------------------------------------

/// Why a package does not fit the language: the first error the lint found (its code, `HUD-E0602`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unfit {
    pub code: &'static str,
}

/// What the compile of the resolved selection is waiting on.
enum Want {
    /// Nothing to compile: the built-in.
    Classic,
    /// Compile the package `sha` once its bytes arrive.
    Pack(Sha),
}

/// A pack read in flight for the compile.
struct Reading {
    sha: Sha,
    slot: Slot<Option<Vec<u8>>>,
}

/// A pack that was read and has not been compiled yet: the lint is one tick's work and the load the next's, so
/// one frame never carries both.
struct Pending {
    sha: Sha,
    bytes: Vec<u8>,
    /// The lint passed: the next step is the load.
    linted: bool,
}

/// What a compile needs from the app: the measurer and the language. Handed in per tick, never stored.
pub(crate) struct Env<'a> {
    pub measure: &'a dyn Measure,
    pub cx: &'a LocaleContext,
    pub app_version: &'a str,
    /// A player page is mounted or a playback is starting: no compile runs now ([`HudDriver`]'s rule).
    pub player: bool,
}

pub(crate) struct HudDriver {
    store: HudStore,
    runner: HudRunner,
    /// The directories, when a test supplies them; `None` reads `paths::hud_candidates()` each time.
    setup: Option<Setup>,
    started: bool,
    /// The language the last tick saw: a different one recompiles classic and the selection.
    language: Option<&'static str>,
    classic: Option<LoadedHud>,
    /// The compile of the last package that fit: (sha, language tag), and the result. One entry: the
    /// resolved selection is one package, and the language and the app version are fixed per process.
    compiled: Option<(Sha, &'static str, LoadedHud)>,
    unfit: BTreeMap<(Sha, &'static str), Unfit>,
    /// The `hud: fallback` lines written, once per (package, language, code): a failed read must not use up the line
    /// of a later fit failure.
    logged: BTreeSet<(Sha, &'static str, &'static str)>,
    reading: Option<Reading>,
    /// The bytes that landed and wait for their compile ([`Pending`]).
    pending: Option<Pending>,
    /// Packs whose bytes could not be read for the compile: classic draws until the next listing or command
    /// gives them another try (a read that failed once is not a verdict on the package).
    read_failed: BTreeSet<Sha>,
    shown: Option<Arc<plx_appkit::hud_format::compile::Compiled>>,
    selection: Vec<String>,
    /// The last pick that could not be persisted because no profile was seated: it stays for the process, and a
    /// seat arriving replaces it ([`HudDriver::selected_for`]).
    unsaved: Option<String>,
    notices: Vec<Notice>,
    log: Vec<String>,
    dirty: bool,
    /// How many ticks went past the early return (the ones that did work): a still HUD adds none, which
    /// `classic_with_nothing_installed_adds_no_work_per_frame` reads.
    #[cfg_attr(not(test), allow(dead_code))] // read only by that test; the counting is the same in every build
    worked: u64,
    revision: u64,
}

/// What the driver asks the Bridge to do with the selection: persist `id` for the seated profile.
pub(crate) type Selection = String;

impl HudDriver {
    pub(crate) fn new(runner: HudRunner) -> Self {
        Self {
            store: HudStore::new(),
            runner,
            setup: None,
            started: false,
            language: None,
            classic: None,
            compiled: None,
            unfit: BTreeMap::new(),
            logged: BTreeSet::new(),
            reading: None,
            pending: None,
            read_failed: BTreeSet::new(),
            shown: None,
            selection: Vec::new(),
            unsaved: None,
            notices: Vec::new(),
            log: Vec::new(),
            dirty: false,
            worked: 0,
            revision: 0,
        }
    }

    /// A driver that does nothing: a controlled replay or record run, whose measurer answers only the keys
    /// its recording holds.
    pub(crate) fn off() -> Self {
        Self::new(HudRunner::live(""))
    }

    /// Begin: keep the compiled classic as what draws until a package lands.
    pub(crate) fn start(&mut self, classic: LoadedHud, app_version: &str) {
        self.shown = Some(Arc::clone(&classic.compiled));
        self.classic = Some(classic);
        self.runner.app_version = app_version.into();
        self.started = true;
        self.dirty = true;
    }

    #[cfg(test)]
    pub(crate) fn started(&self) -> bool {
        self.started
    }

    /// The store, for the Settings page.
    pub(crate) fn store(&self) -> &HudStore {
        &self.store
    }

    /// Bumped when anything the page draws changed.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Read every candidate again (HUD settings opened, a profile seated): `selected` is the profile's pick.
    pub(crate) fn refresh(&mut self, selected: String) {
        if !self.started {
            return;
        }
        self.step(HudEvent::Refresh { setup: self.setup(), selected });
    }

    /// The viewer picked a package (or the built-in).
    pub(crate) fn select(&mut self, id: String) {
        if self.started {
            self.step(HudEvent::Selected { id });
        }
    }

    pub(crate) fn remove(&mut self, id: String) {
        if self.started {
            self.step(HudEvent::Removed { id });
        }
    }

    /// Install the drop file the page showed under `sha`; the viewer has confirmed. `now` is the unix time.
    pub(crate) fn install_drop(&mut self, sha: Sha, now: u64) {
        if self.started {
            self.step(HudEvent::InstallDrop { sha, now });
        }
    }

    /// Why `sha` draws as classic in this process's language, when the lint refused it.
    #[cfg(test)]
    pub(crate) fn unfit(&self, sha: Sha, language: &'static str) -> Option<&Unfit> {
        self.unfit.get(&(sha, language))
    }

    /// The packages the television refused in `language` (what the page marks).
    pub(crate) fn unfit_shas(&self, language: &'static str) -> Vec<Sha> {
        self.unfit.keys().filter(|(_, l)| *l == language).map(|(sha, _)| *sha).collect()
    }

    /// The picks to persist, oldest first.
    pub(crate) fn take_selections(&mut self) -> Vec<Selection> {
        std::mem::take(&mut self.selection)
    }

    /// A pick the owner could not persist (no seat): keep it for the process.
    pub(crate) fn note_unsaved(&mut self, id: String) {
        self.unsaved = Some(id);
    }

    /// The selection a `Refresh` starts from: with no seat, the unsaved pick of this process if there is one, else
    /// `stored`; with a seat, the profile's own `stored` value, and the unsaved pick is dropped for good.
    pub(crate) fn selected_for(&mut self, seated: bool, stored: String) -> String {
        if seated {
            self.unsaved = None;
            stored
        } else {
            self.unsaved.clone().unwrap_or(stored)
        }
    }

    /// What finished since the last call (an install, a removal, a refusal).
    pub(crate) fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// The `hud: fallback` lines to log, one per (package, language), each only the once.
    pub(crate) fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn step(&mut self, event: HudEvent) {
        self.read_failed.clear();
        let mut fx = Vec::new();
        self.store.step(event, &mut fx);
        self.apply(fx);
        self.dirty = true;
        self.revision += 1;
    }

    fn apply(&mut self, mut fx: Vec<HudEffect>) {
        self.runner.start(&mut fx);
        for effect in fx {
            match effect {
                HudEffect::Selection { id } => self.selection.push(id),
                HudEffect::Notice(notice) => self.notices.push(notice),
                _ => {}
            }
        }
    }

    /// One tick: take the answers that landed, and compile the resolved selection if it needs it. Returns the
    /// compiled HUD the player must now draw when that changed. With nothing in flight and nothing
    /// changed it is three flag tests.
    pub(crate) fn tick(&mut self, env: &Env<'_>) -> Option<LoadedHud> {
        if !self.started {
            return None;
        }
        let language = env.cx.language().tag();
        if self.language != Some(language) {
            if self.language.is_some() {
                self.classic = Some(plx_appkit::player_hud::loaded::load_classic(env.app_version, env.cx, env.measure));
            }
            self.language = Some(language);
            self.dirty = true;
        }
        let waiting = self.pending.is_some() && !env.player;
        if self.runner.is_idle() && self.reading.is_none() && !self.dirty && !waiting {
            return None;
        }
        self.worked += 1;
        let mut events = Vec::new();
        self.runner.ready(&mut events);
        for event in events {
            let mut fx = Vec::new();
            self.store.step(event, &mut fx);
            self.apply(fx);
            self.dirty = true;
            self.revision += 1;
        }
        self.land_pack(env);
        self.advance(env);
        if !self.runner.is_idle() || self.store.busy() {
            return None;
        }
        self.dirty = false;
        match self.want() {
            Want::Classic => self.show(self.classic.clone()),
            Want::Pack(sha) => {
                let language = env.cx.language().tag();
                if self.compiled.as_ref().is_some_and(|(s, l, _)| *s == sha && *l == language) {
                    let hud = self.compiled.as_ref().map(|(_, _, h)| h.clone());
                    return self.show(hud);
                }
                if self.unfit.contains_key(&(sha, language)) || self.read_failed.contains(&sha) {
                    return self.show(self.classic.clone());
                }
                if self.reading.as_ref().is_none_or(|r| r.sha != sha) && self.pending.as_ref().is_none_or(|p| p.sha != sha) {
                    self.reading = Some(self.begin_read(sha));
                }
                None
            }
        }
    }

    /// What the resolved selection needs.
    fn want(&self) -> Want {
        let resolved = self.store.resolved();
        if resolved == BUILTIN_CLASSIC {
            return Want::Classic;
        }
        match self.store.entries().into_iter().find(|e: &Entry| e.id == resolved).and_then(|e| e.sha) {
            Some(sha) => Want::Pack(sha),
            None => Want::Classic,
        }
    }

    fn setup(&self) -> Setup {
        self.setup.clone().unwrap_or_else(live_setup)
    }

    fn begin_read(&self, sha: Sha) -> Reading {
        let roots: Vec<PathBuf> = self.setup().candidates.into_iter().map(|c| c.dir).collect();
        Reading { sha, slot: self.runner.exec.run(move || read_pack(&roots, sha)) }
    }

    /// The pack bytes landed: lint them in the language with the television's measurer, then compile.
    fn land_pack(&mut self, env: &Env<'_>) {
        let Some(reading) = self.reading.as_mut() else { return };
        let Some(answer) = reading.slot.poll() else { return };
        let sha = reading.sha;
        self.reading = None;
        self.dirty = true;
        let language = env.cx.language().tag();
        let Some(bytes) = answer.flatten() else {
            self.read_failed.insert(sha);
            if self.logged.insert((sha, language, "read")) {
                self.log.push(format!("hud: fallback package={} language={language} code=read", self.resolved_id()));
            }
            self.revision += 1;
            return;
        };
        self.pending = Some(Pending { sha, bytes, linted: false });
        self.revision += 1;
    }

    /// One compile step, off the player: the lint of the landed bytes in this language, or on the tick after it the
    /// load. Nothing runs while a player is mounted or a playback is starting (`Env::player`): the measurer is the main
    /// thread's, so the bytes wait and the compile runs on the first tick with no player, and the next playback
    /// draws the choice. Waiting is not a fallback and logs nothing.
    fn advance(&mut self, env: &Env<'_>) {
        if env.player {
            return;
        }
        let Some(mut pending) = self.pending.take() else { return };
        self.dirty = true;
        self.revision += 1;
        let language = env.cx.language().tag();
        let sha = pending.sha;
        let step = if pending.linted { self.load_step(&pending.bytes, env) } else { self.lint_step(&pending.bytes, env, language) };
        match step {
            Ok(None) => {
                pending.linted = true;
                self.pending = Some(pending);
            }
            Ok(Some(hud)) => {
                self.unfit.remove(&(sha, language));
                self.compiled = Some((sha, language, hud));
            }
            Err(code) => {
                self.unfit.insert((sha, language), Unfit { code });
                if self.logged.insert((sha, language, code)) {
                    self.log.push(format!("hud: fallback package={} language={language} code={code}", self.resolved_id()));
                }
            }
        }
    }

    fn resolved_id(&self) -> String {
        self.store.resolved().to_owned()
    }

    /// The fit check in this language: `Err(code)` is the first error finding, `Ok(None)` is "fits, load it next tick".
    fn lint_step(&self, bytes: &[u8], env: &Env<'_>, language: &'static str) -> Result<Option<LoadedHud>, &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "utf8")?;
        let lang = LintLang { tag: language, labels: plx_appkit::hud_format::compile::AppLabels::in_language(env.cx) };
        let findings = lint_with(text, env.measure, &[lang], &LintOptions { text_skew: 0.0 });
        match findings.iter().find(|f| f.severity == Severity::Error) {
            Some(first) => Err(first.code),
            None => Ok(None),
        }
    }

    /// The compile the player draws from.
    fn load_step(&self, bytes: &[u8], env: &Env<'_>) -> Result<Option<LoadedHud>, &'static str> {
        let hud = load(bytes, env.app_version, env.cx, env.measure).map_err(|_| "parse")?;
        // A package with no state for some frame the transport can be in draws nothing there: classic does instead.
        if plx_appkit::player_hud::loaded::answers_every_frame(&hud.compiled) { Ok(Some(hud)) } else { Err("states") }
    }

    /// Hand the player `hud` when it is not what it already draws.
    fn show(&mut self, hud: Option<LoadedHud>) -> Option<LoadedHud> {
        let hud = hud?;
        if self.shown.as_ref().is_some_and(|s| Arc::ptr_eq(s, &hud.compiled)) {
            return None;
        }
        self.shown = Some(Arc::clone(&hud.compiled));
        Some(hud)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_appkit::hud_format::admit::Facts;
    use plx_appkit::hud_format::CLASSIC_PACKAGE;
    use plx_appkit::player_hud::loaded::load_classic;
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_data::hudpacks::{Refusal, Source};
    use plx_platform::i18n::{LocaleContext, Preference};

    const REFERENCE: &str = include_str!("../../../appkit/src/hud_format/fixtures/reference.plxhud.json");
    const MINIMAL: &str = include_str!("../../../appkit/src/hud_format/fixtures/minimal.plxhud.json");
    const REFERENCE_ID: &str = "com.plxnative.examples.reference";
    const APP: &str = "0.2.1";

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Dir {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("plx-huds-{}-{n}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("drop")).unwrap();
            Dir(dir)
        }
        fn drop_file(&self, name: &str, text: &str) {
            std::fs::write(self.0.join("drop").join(name), text).unwrap();
        }
        fn setup(&self) -> Setup {
            Setup { candidates: vec![Candidate { dir: self.0.clone(), volatile: false }], max_pack_bytes: MAX_PACKAGE_BYTES }
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn driver(dir: &Dir) -> HudDriver {
        let mut driver = HudDriver::new(HudRunner::inline(APP));
        driver.setup = Some(dir.setup());
        let classic = load_classic(APP, plx_platform::i18n::english(), &ShippedMeasure);
        driver.start(classic, APP);
        driver
    }

    fn english() -> LocaleContext {
        LocaleContext::resolve(Preference::SHIPPED[0], None, None, None, None)
    }

    /// Tick until nothing changes; the HUDs the driver handed out, in order.
    fn settle(driver: &mut HudDriver, cx: &LocaleContext) -> Vec<LoadedHud> {
        let env = Env { measure: &ShippedMeasure, cx, app_version: APP, player: false };
        let mut shown = Vec::new();
        for _ in 0..64 {
            shown.extend(driver.tick(&env));
        }
        assert!(driver.runner.is_idle() && driver.reading.is_none() && !driver.store.busy(), "the driver settles");
        shown
    }

    fn entry(driver: &HudDriver, id: &str) -> Option<Entry> {
        driver.store().entries().into_iter().find(|e| e.id == id)
    }

    fn install_reference(dir: &Dir, driver: &mut HudDriver, cx: &LocaleContext) {
        dir.drop_file("reference.plxhud.json", REFERENCE);
        driver.refresh(String::new());
        settle(driver, cx);
        let drops = driver.store().drops();
        assert_eq!(drops.len(), 1, "{drops:?}");
        assert_eq!(drops[0].status, Ok(()));
        driver.install_drop(drops[0].sha.unwrap(), 1_700_000_000);
        settle(driver, cx);
    }

    /// **A package in the drop folder installs, is selectable and the player draws it.** The whole path through
    /// the runner on a real directory: list, install, select, compile.
    #[test]
    fn a_dropped_package_installs_is_selectable_and_is_what_the_player_draws() {
        let dir = Dir::new("drop-installs");
        let mut driver = driver(&dir);
        let cx = english();
        install_reference(&dir, &mut driver, &cx);
        let installed = entry(&driver, REFERENCE_ID).expect("installed");
        assert_eq!((installed.trust, installed.source), (plx_data::hudpacks::Trust::Unreviewed, Some(Source::Drop)));
        assert!(dir.0.join("packs").join(format!("{}.plxhud.json", installed.sha.unwrap().hex())).is_file());
        assert!(dir.0.join("installed.json").is_file());
        assert!(driver.take_notices().iter().any(|n| matches!(n, Notice::Installed { id, .. } if id == REFERENCE_ID)));

        assert!(settle(&mut driver, &cx).is_empty(), "installing alone changes nothing the player draws");
        driver.select(REFERENCE_ID.to_string());
        let shown = settle(&mut driver, &cx);
        assert_eq!(shown.len(), 1, "one compile lands");
        assert_eq!(driver.take_selections(), [REFERENCE_ID]);
        let classic = load_classic(APP, &cx, &ShippedMeasure);
        assert_ne!(shown[0].compiled.states().len(), 0);
        assert!(!Arc::ptr_eq(&shown[0].compiled, &classic.compiled), "not classic");
        assert_eq!(shown[0].key.sha256, plx_base::sha256::sha256(REFERENCE.as_bytes()), "compiled from the installed bytes");
        assert_eq!(driver.store().resolved(), REFERENCE_ID);

        driver.select(BUILTIN_CLASSIC.to_string());
        let back = settle(&mut driver, &cx);
        assert_eq!(back.len(), 1, "back to classic is one hand-over");
        assert_eq!(driver.take_selections(), [BUILTIN_CLASSIC]);
    }

    /// The selection survives a restart (a new driver, the profile's setting handed in) and is per profile: a
    /// second profile's empty setting reads the built-in from the same directory.
    #[test]
    fn the_selection_survives_a_restart_and_a_second_profile_keeps_its_own() {
        let dir = Dir::new("restart");
        let cx = english();
        let mut first = driver(&dir);
        install_reference(&dir, &mut first, &cx);
        first.select(REFERENCE_ID.to_string());
        settle(&mut first, &cx);

        let mut restarted = driver(&dir);
        restarted.refresh(REFERENCE_ID.to_string());
        let shown = settle(&mut restarted, &cx);
        assert_eq!(shown.len(), 1, "the restarted process draws the package");
        assert_eq!(restarted.store().selected(), REFERENCE_ID);

        let mut other = driver(&dir);
        other.refresh(String::new());
        assert!(settle(&mut other, &cx).is_empty(), "a profile that picked nothing draws classic");
        assert_eq!(other.store().selected(), BUILTIN_CLASSIC);
        assert!(entry(&other, REFERENCE_ID).is_some(), "though the package is installed for the household");
    }

    /// A selected package that is gone or corrupt draws classic and the selection is kept.
    #[test]
    fn a_missing_or_corrupt_selected_package_draws_classic_and_keeps_the_selection() {
        let cx = english();
        for damage in ["missing", "corrupt"] {
            let dir = Dir::new(damage);
            let mut first = driver(&dir);
            install_reference(&dir, &mut first, &cx);
            let sha = entry(&first, REFERENCE_ID).unwrap().sha.unwrap();
            let pack = dir.0.join("packs").join(format!("{}.plxhud.json", sha.hex()));
            if damage == "missing" {
                std::fs::remove_file(&pack).unwrap();
            } else {
                std::fs::write(&pack, b"{ not a package").unwrap();
            }
            let mut again = driver(&dir);
            again.refresh(REFERENCE_ID.to_string());
            assert!(settle(&mut again, &cx).is_empty(), "{damage}: classic keeps drawing");
            assert_eq!(again.store().selected(), REFERENCE_ID, "{damage}: the selection is kept");
            assert_eq!(again.store().resolved(), BUILTIN_CLASSIC, "{damage}");
        }
    }

    /// A dropped file claiming `builtin:classic` is refused, nothing is written, and the built-in stays
    /// selectable. The id literal is the one `appkit` accepts, which `data` cannot compare (it is above).
    #[test]
    fn a_dropped_file_claiming_builtin_classic_is_refused_and_the_builtin_stays_selectable() {
        assert_eq!(BUILTIN_CLASSIC, plx_appkit::hud_format::schema::BUILTIN_CLASSIC_ID, "the two layers spell the built-in alike");
        let dir = Dir::new("builtin-id");
        let mut driver = driver(&dir);
        let cx = english();
        dir.drop_file("imposter.plxhud.json", CLASSIC_PACKAGE);
        driver.refresh(String::new());
        settle(&mut driver, &cx);
        let drops = driver.store().drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].status, Err(Refusal::BuiltinId));
        driver.install_drop(drops[0].sha.unwrap(), 1);
        settle(&mut driver, &cx);
        assert!(driver.take_notices().iter().any(|n| matches!(n, Notice::Refused { reason: Refusal::BuiltinId, .. })));
        assert!(!dir.0.join("packs").exists() && !dir.0.join("installed.json").exists(), "nothing was written");
        let ids: Vec<_> = driver.store().entries().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, [BUILTIN_CLASSIC], "one built-in row, and the file made no second one");
        driver.select(BUILTIN_CLASSIC.to_string());
        assert_eq!(driver.take_selections(), [BUILTIN_CLASSIC]);
    }

    /// A package that does not fit one language falls back there only: one `hud: fallback` line, the reason on
    /// the row, the selection kept, and it draws again in a language it fits.
    #[test]
    fn a_package_that_does_not_fit_a_language_falls_back_there_only() {
        let en = english();
        let slack = |text: &str| -> f32 {
            let lang = LintLang::for_tag("en").unwrap();
            let found = lint_with(text, &ShippedMeasure, &[lang], &LintOptions { text_skew: 1e9 });
            let fact = found
                .iter()
                .find(|f| f.pointer == "/layout/block/4/cells/0/strip/card/below/1/text")
                .and_then(|f| f.ctx.text.clone())
                .expect("the strip card's fallback title is measured");
            fact.measured
        };
        let exact = slack(REFERENCE);
        let tight = REFERENCE.replace("\"card_w\": 192", &format!("\"card_w\": {exact}"));
        assert_ne!(tight, REFERENCE);
        let fits = |tag: &str| {
            let lang = LintLang::for_tag(tag).unwrap();
            lint_with(&tight, &ShippedMeasure, &[lang], &LintOptions { text_skew: 0.0 }).iter().all(|f| f.severity != Severity::Error)
        };
        assert!(fits("en"), "the card is exactly as wide as English needs");
        let narrow_tag = Preference::SHIPPED.iter().map(|p| p.tag()).find(|t| !fits(t)).expect("a language needs more than English");
        let narrow = LocaleContext::resolve(Preference::SHIPPED.into_iter().find(|p| p.tag() == narrow_tag).unwrap(), None, None, None, None);

        let dir = Dir::new("unfit");
        let mut driver = driver(&dir);
        dir.drop_file("tight.plxhud.json", &tight);
        driver.refresh(String::new());
        settle(&mut driver, &en);
        let sha = driver.store().drops()[0].sha.unwrap();
        driver.install_drop(sha, 1);
        settle(&mut driver, &en);
        driver.select(REFERENCE_ID.to_string());

        let in_narrow = settle(&mut driver, &narrow);
        assert_eq!(in_narrow.len(), 1, "classic, compiled for the new language, is the one hand-over");
        assert_eq!((in_narrow[0].key.lang.as_str(), in_narrow[0].key.sha256), (narrow_tag, plx_base::sha256::sha256(CLASSIC_PACKAGE.as_bytes())));
        assert!(driver.unfit(sha, narrow.language().tag()).is_some_and(|u| u.code == "HUD-E0601"));
        assert_eq!(driver.store().selected(), REFERENCE_ID, "the selection is kept");
        let lines = driver.take_log();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("hud: fallback ") && lines[0].contains(narrow_tag) && lines[0].contains("HUD-E0601"), "{lines:?}");
        settle(&mut driver, &narrow);
        assert!(driver.take_log().is_empty(), "once per package and language");

        let in_english = settle(&mut driver, &en);
        assert_eq!(in_english.len(), 1, "back in a language it fits, the package draws");
        assert!(driver.unfit(sha, en.language().tag()).is_none());
        assert_eq!(driver.take_log().len(), 0);
    }

    /// A failed read and a later fit failure are two causes, and each is logged once: the first must not use up the
    /// slot of the second.
    #[test]
    fn a_failed_read_does_not_hide_a_later_fit_failure_from_the_log() {
        let dir = Dir::new("log-per-code");
        let en = english();
        let mut driver = driver(&dir);
        let slack = |text: &str| -> f32 {
            let lang = LintLang::for_tag("en").unwrap();
            let found = lint_with(text, &ShippedMeasure, &[lang], &LintOptions { text_skew: 1e9 });
            found.iter().find(|f| f.pointer == "/layout/block/4/cells/0/strip/card/below/1/text").and_then(|f| f.ctx.text.clone()).expect("measured").measured
        };
        let tight = REFERENCE.replace("\"card_w\": 192", &format!("\"card_w\": {}", slack(REFERENCE)));
        let fits = |tag: &str| {
            let lang = LintLang::for_tag(tag).unwrap();
            lint_with(&tight, &ShippedMeasure, &[lang], &LintOptions { text_skew: 0.0 }).iter().all(|f| f.severity != Severity::Error)
        };
        let narrow_tag = Preference::SHIPPED.iter().map(|p| p.tag()).find(|t| !fits(t)).expect("a language needs more than English");
        let narrow = LocaleContext::resolve(Preference::SHIPPED.into_iter().find(|p| p.tag() == narrow_tag).unwrap(), None, None, None, None);
        dir.drop_file("tight.plxhud.json", &tight);
        driver.refresh(String::new());
        settle(&mut driver, &narrow);
        let sha = driver.store().drops()[0].sha.unwrap();
        driver.install_drop(sha, 1);
        settle(&mut driver, &narrow);
        let pack = dir.0.join("packs").join(format!("{}.plxhud.json", sha.hex()));
        let kept = std::fs::read(&pack).unwrap();
        std::fs::remove_file(&pack).unwrap();
        driver.select(REFERENCE_ID.to_string());
        settle(&mut driver, &narrow);
        let first = driver.take_log();
        assert!(first.len() == 1 && first[0].contains("code=read"), "{first:?}");
        std::fs::write(&pack, kept).unwrap();
        driver.select(REFERENCE_ID.to_string());
        settle(&mut driver, &narrow);
        let second = driver.take_log();
        assert!(second.len() == 1 && second[0].contains("code=HUD-E0601"), "the real cause is logged too: {second:?}");
        let _ = en;
    }

    /// **No compile while a player is up.** The bytes of the chosen package land in the same tick a playback starts: they
    /// wait (no compile, no hand-over, no fallback line, no fit verdict), and the compile runs after the player leaves,
    /// the lint in one tick and the load in the next, so one frame never carries both.
    #[test]
    fn a_pack_that_lands_with_a_player_up_waits_and_compiles_in_two_ticks_after_it_leaves() {
        let dir = Dir::new("player-up");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let tick = |driver: &mut HudDriver, player: bool| {
            let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player };
            driver.tick(&env)
        };
        // the playback starts: every tick the listing, the read and the landing take has a player up
        for _ in 0..64 {
            assert!(tick(&mut driver, true).is_none(), "nothing is handed to a player that is up");
        }
        assert!(driver.pending.is_some(), "the bytes landed and wait");
        assert!(driver.compiled.is_none() && driver.unfit.is_empty(), "no compile ran, and waiting is not a verdict");
        assert!(driver.take_log().is_empty(), "waiting is not a fallback");
        let before = driver.worked;
        for _ in 0..100 {
            tick(&mut driver, true);
        }
        assert!(driver.worked <= before + 100 && driver.pending.is_some() && driver.compiled.is_none(), "still waiting");

        // the player leaves: the lint is one tick's work, the load the next's
        assert!(tick(&mut driver, false).is_none());
        assert!(driver.compiled.is_none(), "the lint tick compiles nothing the player draws from");
        assert!(driver.pending.as_ref().is_some_and(|p| p.linted), "linted, the load is next");
        let shown = tick(&mut driver, false);
        assert!(shown.is_some_and(|h| h.key.sha256 == plx_base::sha256::sha256(REFERENCE.as_bytes())), "the load tick hands the package over");
        assert!(driver.pending.is_none() && driver.compiled.is_some());
        assert!(driver.take_log().is_empty());
    }

    /// A player that comes up between the lint and the load holds the load too.
    #[test]
    fn a_player_that_starts_between_the_lint_and_the_load_holds_the_load() {
        let dir = Dir::new("player-between");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let env = |player| Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player };
        let mut guard = 0;
        while !driver.pending.as_ref().is_some_and(|p| p.linted) && guard < 64 {
            driver.tick(&env(false));
            guard += 1;
        }
        assert!(driver.pending.as_ref().is_some_and(|p| p.linted));
        for _ in 0..10 {
            assert!(driver.tick(&env(true)).is_none());
        }
        assert!(driver.compiled.is_none(), "the load waits for the player to leave");
        assert!(driver.tick(&env(false)).is_some());
    }

    /// **With no seat the pick stays for the process.** A dev-token boot never seats, so a pick cannot be persisted; it is
    /// kept, survives a reopened page and a Refresh (which resolves it), and a seat arriving replaces it with that
    /// profile's own value.
    #[test]
    fn a_pick_with_no_seat_survives_a_refresh_and_a_seat_replaces_it() {
        let dir = Dir::new("unseated");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let picks = driver.take_selections();
        assert_eq!(picks, [REFERENCE_ID]);
        // the owner found no seat to persist it for
        for id in picks {
            driver.note_unsaved(id);
        }
        // the page is reopened: the owner refreshes from the stored value, which is empty
        let selected = driver.selected_for(false, String::new());
        assert_eq!(selected, REFERENCE_ID, "the unsaved pick is what Refresh resolves");
        driver.refresh(selected);
        let shown = settle(&mut driver, &cx);
        assert_eq!(driver.store().selected(), REFERENCE_ID);
        assert!(shown.is_empty() || shown.len() == 1);
        assert_eq!(driver.store().resolved(), REFERENCE_ID, "and the player still draws it");
        // a profile is seated: its own value (here the built-in) replaces the process's pick
        assert_eq!(driver.selected_for(true, String::new()), "");
        assert_eq!(driver.selected_for(false, String::new()), "", "and the pick is gone for good");
    }

    /// **Classic with nothing installed adds nothing per frame.** Once the listing has settled, a tick changes
    /// no store state, starts no read, compiles nothing and hands the player nothing, however often it runs.
    #[test]
    fn classic_with_nothing_installed_adds_no_work_per_frame() {
        let dir = Dir::new("zero");
        let mut driver = driver(&dir);
        let cx = english();
        driver.refresh(String::new());
        assert!(settle(&mut driver, &cx).is_empty());
        let (revision, worked) = (driver.revision(), driver.worked);
        let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: false };
        for _ in 0..1000 {
            assert!(driver.tick(&env).is_none());
        }
        assert_eq!(driver.worked, worked, "a still HUD's tick returns on its first flag test: no entries() Vec, no clone of the compile");
        assert_eq!(driver.revision(), revision, "no store event");
        assert!(driver.compiled.is_none() && driver.reading.is_none() && driver.runner.is_idle(), "no compile, no read, no IO");
        assert!(!driver.dirty, "and the next tick returns on its first flag test");
        assert!(driver.take_log().is_empty() && driver.take_notices().is_empty() && driver.take_selections().is_empty());
    }

    /// A driver that was never started (a controlled run, a test rig) does nothing at all.
    #[test]
    fn an_unstarted_driver_does_nothing() {
        let mut driver = HudDriver::off();
        let cx = english();
        driver.refresh("x".into());
        driver.select("y".into());
        let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: false };
        assert!(driver.tick(&env).is_none());
        assert!(!driver.started() && driver.runner.is_idle() && driver.revision() == 0);
    }

    /// The runner's own refusals: a symlink is neither listed nor read, a file over the bound is too large, and a
    /// write lands whole under its name.
    #[test]
    fn the_runner_refuses_links_and_oversize_files_and_writes_whole_files() {
        let dir = Dir::new("runner");
        dir.drop_file("real.plxhud.json", MINIMAL);
        std::os::unix::fs::symlink(dir.0.join("drop/real.plxhud.json"), dir.0.join("drop/link.plxhud.json")).unwrap();
        let at = |rel: &str| Loc { root: dir.0.clone(), rel: rel.to_string() };
        assert_eq!(scan_dir(&at("drop")), Ok(vec!["real.plxhud.json".to_string()]), "the link is not a file");
        assert_eq!(read_file(&at("drop/link.plxhud.json"), 1 << 20), Err(ReadError::Failed));
        assert_eq!(read_file(&at("drop/real.plxhud.json"), 10), Err(ReadError::TooLarge));
        assert_eq!(read_file(&at("drop/none.json"), 10), Err(ReadError::Missing));
        assert_eq!(scan_dir(&at("nowhere")), Err(ReadError::Missing));
        assert_eq!(write_file(&at("packs/a.json"), b"hello"), WriteOutcome::Done);
        assert_eq!(std::fs::read(dir.0.join("packs/a.json")).unwrap(), b"hello");
        assert_eq!(std::fs::read_dir(dir.0.join("packs")).unwrap().count(), 1, "no temporary file is left");
        delete_file(&at("packs/a.json"));
        delete_file(&at("packs/a.json"));
        assert!(!dir.0.join("packs/a.json").exists());
        let blocked = Loc { root: dir.0.join("drop/real.plxhud.json"), rel: "packs/b.json".into() };
        assert_eq!(write_file(&blocked, b"x"), WriteOutcome::Refused, "a root that is a file takes no write");
    }

    /// A private root (one in a directory anyone can write) is trusted when it does not exist yet, and when it is a
    /// directory of ours that others cannot write; never when it is a link, or open to others.
    #[test]
    fn a_private_root_is_trusted_only_when_it_is_ours_and_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new("private-root");
        let missing = dir.0.join("not-yet");
        assert!(root_trusted_as(&missing, true), "a root that does not exist yet can be made");
        assert!(read_file(&Loc { root: missing.clone(), rel: "installed.json".into() }, 10) == Err(ReadError::Missing));
        let ours = dir.0.join("ours");
        std::fs::create_dir(&ours).unwrap();
        std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(root_trusted_as(&ours, true));
        std::fs::set_permissions(&ours, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!root_trusted_as(&ours, true), "a root anyone can write is not believed");
        assert!(root_trusted_as(&ours, false), "an ordinary root keeps the mode it has");
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&ours, &link).unwrap();
        assert!(!root_trusted_as(&link, true), "a link is not a root");
    }

    /// A read that failed once is not a verdict: classic draws, no fit reason is recorded, and the next
    /// command gives the package another try.
    #[test]
    fn a_pack_that_cannot_be_read_is_retried_after_the_next_command_and_is_not_unfit() {
        let dir = Dir::new("read-retry");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        let sha = entry(&driver, REFERENCE_ID).unwrap().sha.unwrap();
        let pack = dir.0.join("packs").join(format!("{}.plxhud.json", sha.hex()));
        let kept = std::fs::read(&pack).unwrap();
        driver.select(REFERENCE_ID.to_string());
        std::fs::remove_file(&pack).unwrap();
        assert!(settle(&mut driver, &cx).is_empty(), "classic draws while the bytes cannot be read");
        assert!(driver.unfit(sha, "en").is_none(), "a read failure is not a fit verdict");
        assert!(driver.take_log().iter().any(|l| l.contains("code=read")));
        std::fs::write(&pack, kept).unwrap();
        driver.select(REFERENCE_ID.to_string());
        assert_eq!(settle(&mut driver, &cx).len(), 1, "the next command tries again, and the package draws");
    }

    #[test]
    fn the_verdict_maps_appkit_onto_the_stores_and_never_trusts_what_does_not_parse() {
        assert!(matches!(verdict(REFERENCE.as_bytes(), APP), Verdict::Loadable(Meta { ref id, .. }) if id == REFERENCE_ID));
        assert_eq!(verdict(b"{", APP), Verdict::Invalid);
        let newer = MINIMAL.replacen("\"format\": \"1.0\"", "\"format\": \"9.0\"", 1);
        assert!(matches!(verdict(newer.as_bytes(), APP), Verdict::Unloadable(_, Unloadable::NewerFormat)));
        let _ = Facts { id: String::new(), name: String::new(), version: String::new(), format: String::new() };
    }
}
