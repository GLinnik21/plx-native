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
//! decides what the player draws: the compiled package of the store's `resolved` selection, fitted with the
//! television's own measurer and priced by the HUD cost model, and cached as one entry keyed by (package sha256, language)
//! (the app version is fixed for the process), or `builtin:classic`.
//!
//! * **Never on the frame path.** A tick with nothing in flight and nothing changed is a flag test. A compile is three
//!   ticks, one stage each (the fit, the price, the load; [`Stage`]), never the draw, and until it lands the player draws
//!   what it had. The television's lint alone takes a whole frame's budget, so one frame never carries two compiles. The
//!   bytes landing is not a compile. The tick that lands them runs the fit unless a player is up; then the pack waits at
//!   `Stage::Fit` for the first tick with no player. Nothing here runs on selection except through the Settings page's
//!   commands and the seat trigger.
//! * **Never while a player is up.** The measurer is the main thread's, so a pack that lands while a player page is
//!   mounted or a playback is starting waits ([`Env::player`]), at whichever stage it has reached: no compile and no
//!   hand-over, and the player keeps what it has for that playback. A refusal found at landing is still logged then,
//!   because landing does not wait for the player: an unreadable pack (`code=read`) or faces that cannot be read
//!   (`code=faces`). The compile continues on the first tick with no player, and the next playback draws the choice.
//!   Normal boots compile on the way to Home, before any player.
//! * **Only the pack the selection names is compiled.** A pack the viewer has moved away from is dropped wherever it is:
//!   a read that lands for it is dropped on landing (its price model is kept, being the process's), and a staged one
//!   spends no more stages, so a `hud: fallback` line always concerns the wanted pack. A language change drops the staged
//!   pack too, and the compile starts again at the fit in the new language.
//! * **Fit is the TV's call; the price is the model's.** A package that parses and passes the invariants can still not
//!   fit this language (`lint_fit` with the television's measurer, errors only: the TV applies no margin), or cost more
//!   than a ceiling allows (`lint_price`, `HUD-E1401`). The price is NEVER the television's measure: the ceilings are numbers
//!   taken with one model, the whole-pixel advances of `appfont.ttf` and `appfont-bold.ttf` and a line height of 1.21 x size
//!   (`WholePixelMeasure`), because this app runs on many firmwares whose SDL_ttf builds hint differently, and a ceiling
//!   priced by each firmware's measure would refuse, on one of them, a package that sits exactly at it. Either failure draws
//!   as classic in that language, one `hud: fallback` line is logged per (package, language, cause) so a failed read does not
//!   hide a later failure, the selection is kept, and the page shows the reason ([`HudDriver::unfit`]).
//! * **The model's faces are read on the storage worker.** Reading and parsing two faces of ~400 KB is IO and never
//!   touches a frame: the job of the first pack read that reaches the bytes reads them too (`plx_gfx::text::read_price_model`,
//!   the paths `plx_gfx::text::app_font_path` returns), and the driver keeps the model for the process. If the faces
//!   cannot be read or parsed, the package falls back to classic with the code `faces` and never reaches the player
//!   unpriced: the price is fail-closed. That is no verdict on the package: a store event (a re-pick, a Refresh, an
//!   install) clears it and the next pack read tries the faces again, with no second `hud: fallback` line.
//! * **With no seat a pick stays for the process.** A dev-token boot seats no profile, so a pick cannot be persisted: the
//!   driver keeps it ([`HudDriver::selected_for`]), a reopened page and a `Refresh` resolve it, and a seat arriving
//!   replaces it with that profile's own value.
//! * **No second compile path.** The load is `player_hud::loaded::load`, the one the dev package and
//!   classic use.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plx_appkit::hud_format::admit::{admit, Admission, Cannot};
use plx_appkit::hud_format::lint::{lint_fit, lint_price, LintLang, LintOptions};
use plx_appkit::hud_format::parse::MAX_PACKAGE_BYTES;
use plx_appkit::hud_format::{Finding, Severity};
use plx_appkit::player_hud::loaded::{load, LoadedHud};
use plx_base::fontcov::advances::WholePixelMeasure;
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

/// Why a package draws as classic in the language, and the `code` of its `hud: fallback` line: the first error the fit
/// pass found (its code, `HUD-E0601`), `HUD-E1401` from the price pass, `faces` when the price model cannot be read, a
/// load refusal (`parse`, `states`) or `utf8`. A pack that could not be read has the line's code `read` and no `Unfit`: it
/// is a read that failed (`HudDriver::read_failed`), no verdict on the package, and the page does not mark its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unfit {
    pub code: &'static str,
}

/// The [`Unfit`] code of a package whose price model could not be read (`HudDriver::price`).
const FACES: &str = "faces";

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
    slot: Slot<Landed>,
}

/// What a pack read brings back to the main thread.
struct Landed {
    /// The pack's bytes, `None` when no candidate holds a copy that is still the pack.
    bytes: Option<Vec<u8>>,
    /// The price model, read in the same job when the driver had none ([`HudDriver::price`]) and the pack was read: the
    /// two faces are IO and two parses, which no frame may carry.
    model: Option<Result<WholePixelMeasure, String>>,
}

/// Where a landed pack is in its compile. One stage is one tick's work (`HudDriver::advance`): the television's lint
/// alone takes a whole frame's budget, so one frame never carries two compiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// `lint_fit`: every check but the cost, measured by the television's own text measure.
    Fit,
    /// `lint_price`: `HUD-E1401` alone, priced by the model ([`HudDriver::price`]) and never by the text measure.
    Price,
    /// `load`: the compile the player draws from.
    Load,
}

/// A pack that was read and has not been compiled yet, waiting at its [`Stage`].
struct Pending {
    sha: Sha,
    bytes: Vec<u8>,
    stage: Stage,
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
    /// The regular and the bold face of the price model, when a test supplies them; `None` is the shipped pair
    /// (`plx_gfx::text::app_font_path`).
    faces: Option<[PathBuf; 2]>,
    /// **The price model** every package is priced by (`lint_price`), whatever the television's text measure does: the
    /// whole-pixel advances of the two faces and a line height of 1.21 x size, so a package at a ceiling is accepted on
    /// every firmware. Read once for the process, on the storage worker, in the job of the first pack read that
    /// reaches the bytes; `None` until it lands, and again after a read that failed. That failure is no verdict on the
    /// package either, like `read_failed`: it draws classic as `faces`, and the next store event (a re-pick, a Refresh,
    /// an install) clears that entry from `unfit` (`HudDriver::step`), so the next pack read tries the faces again; `logged`
    /// keeps its entry, so the line stays once. A package is never priced by anything else, so with no model it falls back.
    price: Option<WholePixelMeasure>,
    started: bool,
    /// The language the last tick saw: a different one (no shipped process has two: `i18n`'s `CURRENT` is a `OnceLock`)
    /// recompiles classic, drops the staged pack and compiles the selection again.
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
            faces: None,
            price: None,
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

    /// The `hud: fallback` lines to log, one per (package, language, cause), each only the once.
    pub(crate) fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn step(&mut self, event: HudEvent) {
        // A store event tries again what failed for want of a file: a pack that could not be read and a pair of faces that
        // could not be read are no verdict on the package. (`logged` stays: the line is once per package, language and cause.)
        self.read_failed.clear();
        self.unfit.retain(|_, unfit| unfit.code != FACES);
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
            // A pack staged in the last language was fitted and priced in it: it starts again at the fit in this one, and is
            // never loaded here unfitted and stamped for it.
            self.pending = None;
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

    /// Whether the resolved selection is the package `sha`.
    fn wants(&self, sha: Sha) -> bool {
        matches!(self.want(), Want::Pack(wanted) if wanted == sha)
    }

    fn setup(&self) -> Setup {
        self.setup.clone().unwrap_or_else(live_setup)
    }

    fn face_paths(&self) -> [PathBuf; 2] {
        self.faces.clone().unwrap_or_else(|| [plx_gfx::text::app_font_path(false), plx_gfx::text::app_font_path(true)])
    }

    /// The worker's job for the pack `sha`: its bytes and, when the driver has no price model yet, the two faces too,
    /// read and parsed in the same job (once the pack is read; a pack that is gone needs no model yet).
    fn begin_read(&self, sha: Sha) -> Reading {
        let roots: Vec<PathBuf> = self.setup().candidates.into_iter().map(|c| c.dir).collect();
        let faces = self.price.is_none().then(|| self.face_paths());
        let job = move || {
            let bytes = read_pack(&roots, sha);
            let model = faces.filter(|_| bytes.is_some()).map(|[regular, bold]| plx_gfx::text::read_price_model(&regular, &bold));
            Landed { bytes, model }
        };
        Reading { sha, slot: self.runner.exec.run(job) }
    }

    /// The pack bytes landed: keep the price model that came with them, and, if the selection still names the pack, wait for
    /// the compile's first stage. A pack the selection has moved away from is dropped: the model is the process's and is kept,
    /// the pack is nobody's.
    fn land_pack(&mut self, env: &Env<'_>) {
        let Some(reading) = self.reading.as_mut() else { return };
        let Some(answer) = reading.slot.poll() else { return };
        let sha = reading.sha;
        self.reading = None;
        self.dirty = true;
        self.revision += 1;
        let language = env.cx.language().tag();
        let Landed { bytes, model } = answer.unwrap_or(Landed { bytes: None, model: None });
        match model {
            Some(Ok(model)) => self.price = Some(model),
            Some(Err(reason)) => self.log.push(format!("hud: price model unavailable: {reason}")),
            None => {}
        }
        if !self.wants(sha) {
            // The viewer moved on while the read was out: it lands as nothing. A pack nobody wants stages no compile, records
            // no verdict and logs no line, and a `fall_back` always concerns the pack the selection names.
            return;
        }
        let Some(bytes) = bytes else {
            self.read_failed.insert(sha);
            if self.logged.insert((sha, language, "read")) {
                self.log.push(format!("hud: fallback package={} language={language} code=read", self.resolved_id()));
            }
            return;
        };
        if self.price.is_none() {
            // Fail closed: a package is never handed to the player unpriced.
            self.fall_back(sha, language, FACES);
            return;
        }
        self.pending = Some(Pending { sha, bytes, stage: Stage::Fit });
    }

    /// One compile stage, off the player: the fit, the price or the load of the landed bytes in this language, each on
    /// its own tick. Nothing runs while a player is mounted or a playback is starting (`Env::player`): the text measure
    /// is the main thread's, so the bytes wait at their stage and the compile continues on the first tick with no player,
    /// and the next playback draws the choice. Waiting is not a fallback and logs nothing. A pack the selection no longer
    /// names is dropped instead of advanced (after the player gate: a player up holds it too).
    fn advance(&mut self, env: &Env<'_>) {
        if env.player {
            return;
        }
        let Some(mut pending) = self.pending.take() else { return };
        self.dirty = true;
        if !self.wants(pending.sha) {
            // The selection moved on while the pack waited: it spends no stage, and what follows in the tick reads the new one.
            return;
        }
        self.revision += 1;
        let language = env.cx.language().tag();
        let sha = pending.sha;
        let next = match pending.stage {
            Stage::Fit => self.fit_step(&pending.bytes, env, language).map(|()| Stage::Price),
            Stage::Price => self.price_step(&pending.bytes, env, language).map(|()| Stage::Load),
            Stage::Load => match self.load_step(&pending.bytes, env) {
                Ok(hud) => {
                    self.unfit.remove(&(sha, language));
                    self.compiled = Some((sha, language, hud));
                    return;
                }
                Err(code) => Err(code),
            },
        };
        match next {
            Ok(stage) => {
                pending.stage = stage;
                self.pending = Some(pending);
            }
            Err(code) => self.fall_back(sha, language, code),
        }
    }

    /// `sha` draws as classic in `language`, for the cause `code`: the page shows the reason and one line is logged
    /// for the (package, language, cause). The selection is kept.
    fn fall_back(&mut self, sha: Sha, language: &'static str, code: &'static str) {
        self.unfit.insert((sha, language), Unfit { code });
        self.dirty = true;
        if self.logged.insert((sha, language, code)) {
            self.log.push(format!("hud: fallback package={} language={language} code={code}", self.resolved_id()));
        }
    }

    fn resolved_id(&self) -> String {
        self.store.resolved().to_owned()
    }

    fn lint_lang(env: &Env<'_>, language: &'static str) -> LintLang {
        LintLang { tag: language, labels: plx_appkit::hud_format::compile::AppLabels::in_language(env.cx) }
    }

    /// `Err(code)` of the first error finding, `Ok` when there is none: the fallback's cause.
    fn first_error(findings: &[Finding]) -> Result<(), &'static str> {
        match findings.iter().find(|f| f.severity == Severity::Error) {
            Some(first) => Err(first.code),
            None => Ok(()),
        }
    }

    /// **Stage one, the fit** in this language, measured by the television's own text measure: every check but the cost.
    /// Errors only; the television applies no margin.
    fn fit_step(&self, bytes: &[u8], env: &Env<'_>, language: &'static str) -> Result<(), &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "utf8")?;
        Self::first_error(&lint_fit(text, env.measure, &[Self::lint_lang(env, language)], &LintOptions { text_skew: 0.0 }))
    }

    /// **Stage two, the price** in this language: `HUD-E1401` alone, priced by the model and never by `env.measure`. The
    /// ceilings are numbers taken with the model, so a text measure whose line height or hinting differs (every
    /// firmware's SDL_ttf) must not move the verdict.
    fn price_step(&self, bytes: &[u8], env: &Env<'_>, language: &'static str) -> Result<(), &'static str> {
        let text = std::str::from_utf8(bytes).map_err(|_| "utf8")?;
        let Some(price) = self.price.as_ref() else { return Err(FACES) };
        Self::first_error(&lint_price(text, price, &[Self::lint_lang(env, language)]).findings)
    }

    /// **Stage three, the compile** the player draws from.
    fn load_step(&self, bytes: &[u8], env: &Env<'_>) -> Result<LoadedHud, &'static str> {
        let hud = load(bytes, env.app_version, env.cx, env.measure).map_err(|_| "parse")?;
        // A package with no state for some frame the transport can be in draws nothing there: classic does instead.
        if plx_appkit::player_hud::loaded::answers_every_frame(&hud.compiled) { Ok(hud) } else { Err("states") }
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

    /// The shipped regular and bold face as the host has them (`pkg/`): the driver's own pair is
    /// `plx_gfx::text::app_font_path`, the installed directory's, which a unit test has no copy of.
    fn pkg_faces() -> [PathBuf; 2] {
        let pkg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pkg");
        [pkg.join("appfont.ttf"), pkg.join("appfont-bold.ttf")]
    }

    fn driver(dir: &Dir) -> HudDriver {
        let mut driver = HudDriver::new(HudRunner::inline(APP));
        driver.setup = Some(dir.setup());
        driver.faces = Some(pkg_faces());
        let classic = load_classic(APP, plx_platform::i18n::english(), &ShippedMeasure);
        driver.start(classic, APP);
        driver
    }

    fn english() -> LocaleContext {
        LocaleContext::resolve(Preference::SHIPPED[0], None, None, None, None)
    }

    /// Tick until nothing changes; the HUDs the driver handed out, in order.
    fn settle(driver: &mut HudDriver, cx: &LocaleContext) -> Vec<LoadedHud> {
        settle_with(driver, cx, &ShippedMeasure)
    }

    /// [`settle`] with `measure` as the television's text measure.
    fn settle_with(driver: &mut HudDriver, cx: &LocaleContext, measure: &dyn Measure) -> Vec<LoadedHud> {
        let env = Env { measure, cx, app_version: APP, player: false };
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

    /// The shipped widths, a call counted for each question asked: what the television's text measure is asked.
    struct Counting<'a>(&'a std::cell::Cell<usize>);
    impl Measure for Counting<'_> {
        fn width(&self, s: &std::ffi::CStr, sz: i32, bold: bool) -> f32 {
            self.0.set(self.0.get() + 1);
            ShippedMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            self.0.set(self.0.get() + 1);
            ShippedMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            self.0.set(self.0.get() + 1);
            ShippedMeasure.line_h(sz)
        }
    }

    /// A text measure whose line height is not the price model's: the model's 1.21 x size rounded up to a whole pixel, as a
    /// firmware whose SDL_ttf reports a whole-pixel line skip. The widths are the model's, so the fit is unmoved; the cost of
    /// a package that sits at a ceiling is not.
    struct WholePixelLines;
    impl Measure for WholePixelLines {
        fn width(&self, s: &std::ffi::CStr, sz: i32, bold: bool) -> f32 {
            ShippedMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            ShippedMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            ShippedMeasure.line_h(sz).ceil()
        }
    }

    /// The reference with one more text line under its title: over the compact draw-ops ceiling (the reference's episode is
    /// at it) and the one- and two-section ones, whatever measure prices it (`lint_cost::INVALID`'s case), and clean for
    /// the fit. Under its own id, so it is a package of its own.
    fn over_a_ceiling() -> String {
        const TITLE_LINE: &str =
            r#"{ "text": { "bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } }"#;
        const KICKER: &str =
            r#"{ "text": { "bind": "title.kicker", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" } }"#;
        assert_eq!(REFERENCE.matches(TITLE_LINE).count(), 1, "the reference's title line");
        REFERENCE.replace(TITLE_LINE, &format!("{TITLE_LINE},\n{KICKER}")).replace(REFERENCE_ID, HEAVY_ID)
    }
    const HEAVY_ID: &str = "com.example.heavy";
    const MINIMAL_ID: &str = "com.plxnative.examples.minimal";

    /// Drop `text` in the folder and install it, selecting nothing.
    fn install(dir: &Dir, driver: &mut HudDriver, cx: &LocaleContext, name: &str, text: &str, id: &str) -> Sha {
        dir.drop_file(name, text);
        driver.refresh(String::new());
        settle(driver, cx);
        let drop = driver.store().drops().into_iter().find(|d| d.name == name).expect("the file is listed");
        driver.install_drop(drop.sha.expect("a hashed file"), 1_700_000_000);
        settle(driver, cx);
        entry(driver, id).expect("installed").sha.expect("a package")
    }

    /// [`install`] and select it: the driver is now to compile it.
    fn install_and_select(dir: &Dir, driver: &mut HudDriver, cx: &LocaleContext, name: &str, text: &str, id: &str) -> Sha {
        let sha = install(dir, driver, cx, name, text, id);
        driver.select(id.to_string());
        sha
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

    /// **Two profiles each pick, and each keeps its own.** The string that restarts a driver is not passed in by hand: it is
    /// what the driver's pick became in the session record of the profile that was seated when it was made (the owner's
    /// persist), read back for each seat. Profile A picks one installed package and B another; after a restart each draws its own.
    #[test]
    fn two_profiles_that_each_pick_keep_their_own_across_a_restart() {
        let _serial = plx_base::testlock::serial();
        let session = plx_plex::plex::session::TempSession::new("hud-two-profiles-driver");
        let dir = Dir::new("two-profiles");
        let cx = english();
        let mut first = driver(&dir);
        dir.drop_file("reference.plxhud.json", REFERENCE);
        dir.drop_file("minimal.plxhud.json", MINIMAL);
        first.refresh(String::new());
        settle(&mut first, &cx);
        for drop in first.store().drops() {
            first.install_drop(drop.sha.expect("a hashed file"), 1_700_000_000);
            settle(&mut first, &cx);
        }
        let minimal_id = "com.plxnative.examples.minimal";
        assert!(entry(&first, REFERENCE_ID).is_some() && entry(&first, minimal_id).is_some(), "both are installed for the household");

        // the owner's persist for the seat of the moment (`Bridge::huds_settle`)
        let pick = |driver: &mut HudDriver, id: &str| {
            driver.select(id.to_string());
            settle(driver, &cx);
            let write = plx_plex::plex::session::ProfileWrite::capture().expect("a profile is seated");
            for picked in driver.take_selections() {
                assert!(plx_plex::plex::session::persist_profile_hud(&write, &picked), "the pick is saved");
            }
        };
        let stamp = session.seated("a", "account-token");
        pick(&mut first, REFERENCE_ID);
        session.seated("b", "account-token");
        pick(&mut first, minimal_id);

        let stored = plx_plex::plex::session::peek();
        let seat = |user: &str| plx_plex::plex::session::Seat::Seated { user: user.into(), stamp: stamp.clone() };
        for (user, id, sha) in [
            ("a", REFERENCE_ID, plx_base::sha256::sha256(REFERENCE.as_bytes())),
            ("b", minimal_id, plx_base::sha256::sha256(MINIMAL.as_bytes())),
        ] {
            let mut restarted = driver(&dir);
            let own = plx_plex::plex::session::stored_effective_hud(&stored, &seat(user));
            assert_eq!(own, id, "profile {user}'s own pick is what the persist left for it");
            restarted.refresh(own);
            let shown = settle(&mut restarted, &cx);
            assert_eq!(restarted.store().selected(), id);
            assert!(shown.len() == 1 && shown[0].key.sha256 == sha, "profile {user} draws its own package after a restart");
        }
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

    /// The reference with its info panel's text column no wider than English's kicker needs (a fixed-width column holds
    /// the text, so the cost of the package is the reference's own): it fits and prices in English, and a language whose
    /// kicker is longer does not fit it. The narrow language is the first shipped one the fit refuses.
    fn tight_reference() -> (String, &'static str) {
        const KICKER: &str = "/layout/block/6/cells/1/vstack/cells/0/text";
        let lang = LintLang::for_tag("en").unwrap();
        let found = lint_fit(REFERENCE, &ShippedMeasure, &[lang], &LintOptions { text_skew: 1e9 });
        let needed = found
            .iter()
            .find(|f| f.pointer == KICKER)
            .and_then(|f| f.ctx.text.clone())
            .expect("the kicker above the title is measured")
            .measured;
        let tight = REFERENCE.replace("\"w\": 760", &format!("\"w\": {}", needed.ceil() as u32));
        assert_ne!(tight, REFERENCE);
        let fits = |tag: &str| {
            let lang = LintLang::for_tag(tag).unwrap();
            lint_fit(&tight, &ShippedMeasure, &[lang], &LintOptions { text_skew: 0.0 }).iter().all(|f| f.severity != Severity::Error)
        };
        assert!(fits("en"), "the column is as wide as English needs");
        let narrow = Preference::SHIPPED.iter().map(|p| p.tag()).find(|t| !fits(t)).expect("a language needs more than English");
        (tight, narrow)
    }

    /// A package that does not fit one language falls back there only: one `hud: fallback` line, the reason on
    /// the row, the selection kept, and it draws again in a language it fits.
    #[test]
    fn a_package_that_does_not_fit_a_language_falls_back_there_only() {
        let en = english();
        let (tight, narrow_tag) = tight_reference();
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
        assert!(driver.unfit(sha, en.language().tag()).is_none(), "{:?}", driver.unfit(sha, en.language().tag()));
        assert_eq!(driver.take_log().len(), 0);
    }

    /// A failed read and a later fit failure are two causes, and each is logged once: the first must not use up the
    /// slot of the second.
    #[test]
    fn a_failed_read_does_not_hide_a_later_fit_failure_from_the_log() {
        let dir = Dir::new("log-per-code");
        let en = english();
        let mut driver = driver(&dir);
        let (tight, narrow_tag) = tight_reference();
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
    /// the fit, the price and the load on three ticks, so one frame never carries two of them.
    #[test]
    fn a_pack_that_lands_with_a_player_up_waits_and_compiles_in_three_ticks_after_it_leaves() {
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
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Fit), "the bytes landed and wait, not even fitted");
        assert!(driver.compiled.is_none() && driver.unfit.is_empty(), "no compile ran, and waiting is not a verdict");
        assert!(driver.take_log().is_empty(), "waiting is not a fallback");
        let before = driver.worked;
        for _ in 0..100 {
            tick(&mut driver, true);
        }
        assert_eq!(driver.worked, before, "a tick with a player up and bytes waiting returns on its flag tests: it is no work per frame");
        assert!(driver.pending.is_some() && driver.compiled.is_none(), "still waiting");

        // the player leaves: the fit is one tick's work, the price the next's, the load the third's
        assert!(tick(&mut driver, false).is_none());
        assert!(driver.compiled.is_none(), "the fit tick compiles nothing the player draws from");
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Price), "fitted, the price is next");
        assert!(tick(&mut driver, false).is_none());
        assert!(driver.compiled.is_none(), "the price tick compiles nothing the player draws from");
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Load), "priced, the load is next");
        let shown = tick(&mut driver, false);
        assert!(shown.is_some_and(|h| h.key.sha256 == plx_base::sha256::sha256(REFERENCE.as_bytes())), "the load tick hands the package over");
        assert!(driver.pending.is_none() && driver.compiled.is_some());
        assert!(driver.take_log().is_empty());
    }

    /// **The playback starts in the very tick the bytes land** (a cold launch with a play link): every tick before it had no
    /// player and put the read in flight, the one that lands it has one. The bytes wait unfitted, with no fallback line.
    #[test]
    fn a_playback_that_starts_in_the_tick_the_bytes_land_waits_for_it_to_end() {
        let dir = Dir::new("player-same-tick");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let env = |player| Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player };
        let mut guard = 0;
        while driver.reading.is_none() && guard < 64 {
            assert!(driver.tick(&env(false)).is_none());
            guard += 1;
        }
        assert!(driver.reading.is_some() && driver.pending.is_none(), "the read is in flight and has not landed");
        assert!(driver.tick(&env(true)).is_none(), "the tick the playback starts in lands the bytes");
        assert!(driver.reading.is_none() && driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Fit), "landed in that tick, and not fitted");
        assert!(driver.compiled.is_none() && driver.unfit.is_empty() && driver.take_log().is_empty(), "no compile, no verdict, no fallback line");
        let before = driver.worked;
        for _ in 0..50 {
            assert!(driver.tick(&env(true)).is_none());
        }
        assert_eq!(driver.worked, before, "and the playback's ticks add no work");
        assert!(driver.tick(&env(false)).is_none(), "the first tick after it fits");
        assert!(driver.tick(&env(false)).is_none(), "the next prices");
        assert!(driver.tick(&env(false)).is_some(), "the third loads and hands the package over");
        assert!(driver.take_log().is_empty());
    }

    /// **Nothing advances while a player is up, at any stage.** A player that comes up after the fit holds the price, and one
    /// that comes up after the price holds the load: the stage stands, the text measure is not asked a thing, nothing is handed
    /// over, nothing is logged, and the ticks are no work.
    #[test]
    fn a_player_that_starts_at_any_stage_holds_the_next_one() {
        let dir = Dir::new("player-between");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let asked = std::cell::Cell::new(0);
        let measure = Counting(&asked);
        let env = |player| Env { measure: &measure, cx: &cx, app_version: APP, player };
        let mut guard = 0;
        while driver.pending.is_none() && guard < 64 {
            driver.tick(&env(false));
            guard += 1;
        }
        // the landing tick fitted it, so the price is next
        for (stage, hands_over) in [(Stage::Price, false), (Stage::Load, true)] {
            assert!(driver.pending.as_ref().is_some_and(|p| p.stage == stage), "{stage:?} is next");
            let (worked, asked_before) = (driver.worked, asked.get());
            for _ in 0..10 {
                assert!(driver.tick(&env(true)).is_none());
            }
            assert!(driver.pending.as_ref().is_some_and(|p| p.stage == stage), "{stage:?} waits for the player to leave");
            assert_eq!((driver.worked, asked.get()), (worked, asked_before), "and the player's ticks are no work and no compile");
            assert!(driver.compiled.is_none() && driver.unfit.is_empty() && driver.take_log().is_empty());
            assert_eq!(driver.tick(&env(false)).is_some(), hands_over, "{stage:?} runs on the first tick with no player");
        }
        assert!(driver.compiled.is_some(), "the load ran once the player left");
    }

    /// **Fit, price and load are three successive ticks, never two in one**, and each asks only what it should: the fit and
    /// the load are laid out by the television's text measure, the price by the model alone, so the price tick does not ask
    /// the text measure a thing. The bytes landing is no compile: the tick that lands them runs the fit.
    #[test]
    fn fit_price_and_load_land_on_three_successive_ticks_and_the_price_never_asks_the_text_measure() {
        let dir = Dir::new("three-ticks");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let asked = std::cell::Cell::new(0);
        let measure = Counting(&asked);
        let env = Env { measure: &measure, cx: &cx, app_version: APP, player: false };
        let mut guard = 0;
        while driver.pending.is_none() {
            assert_eq!(asked.get(), 0, "nothing is compiled before the bytes land");
            assert!(driver.tick(&env).is_none());
            guard += 1;
            assert!(guard < 64, "the bytes land");
        }
        let fit = asked.replace(0);
        assert!(fit > 0, "the tick that lands the bytes fits them with the text measure");
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Price) && driver.compiled.is_none());

        assert!(driver.tick(&env).is_none());
        assert_eq!(asked.replace(0), 0, "the price tick asks the text measure nothing: it is the model's");
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Load) && driver.compiled.is_none(), "priced, the load is next");

        let shown = driver.tick(&env);
        assert!(asked.replace(0) > 0, "the load tick lays the package out with the text measure");
        assert!(shown.is_some() && driver.pending.is_none() && driver.compiled.is_some(), "the third tick hands the package over");
        assert!(driver.take_log().is_empty());
        for _ in 0..20 {
            assert!(driver.tick(&env).is_none());
        }
        assert_eq!(asked.get(), 0, "and nothing compiles again");
    }

    /// **A package at a ceiling is accepted on a firmware whose text measure has another line height.** The price is the
    /// model's, never the text measure's: the reference sits exactly at several ceilings, and priced by a measure whose line
    /// height is a whole pixel (`WholePixelLines`) it reads over one, `HUD-E1401`, though the same bytes pass everywhere else.
    #[test]
    fn a_package_at_a_ceiling_is_accepted_whatever_line_height_the_text_measure_has() {
        let lang = LintLang::for_tag("en").unwrap();
        assert!(
            lint_price(REFERENCE, &WholePixelLines, &[lang.clone()]).findings.iter().any(|f| f.code == "HUD-E1401"),
            "premise: priced by this measure the reference is over a ceiling"
        );
        assert!(lint_price(REFERENCE, &ShippedMeasure, &[lang.clone()]).findings.is_empty(), "and priced by the model it sits at them");
        assert!(
            lint_fit(REFERENCE, &WholePixelLines, &[lang], &LintOptions { text_skew: 0.0 }).iter().all(|f| f.severity != Severity::Error),
            "premise: the fit is unmoved by it"
        );

        let dir = Dir::new("ceiling-line-height");
        let cx = english();
        let mut driver = driver(&dir);
        install_reference(&dir, &mut driver, &cx);
        driver.select(REFERENCE_ID.to_string());
        let shown = settle_with(&mut driver, &cx, &WholePixelLines);
        assert_eq!(shown.len(), 1, "the package is accepted and handed over");
        assert!(driver.unfit(entry(&driver, REFERENCE_ID).unwrap().sha.unwrap(), "en").is_none());
        assert!(driver.take_log().is_empty(), "with no fallback line");
    }

    /// **A package over a ceiling is refused at the price stage**: the fit passed, so the refusal is the price's, one
    /// `hud: fallback` line with `HUD-E1401`, classic is what the player keeps drawing, the load never ran and the selection
    /// is kept.
    #[test]
    fn a_package_over_a_ceiling_is_refused_at_the_price_stage_and_classic_draws() {
        let dir = Dir::new("over-ceiling");
        let cx = english();
        let mut driver = driver(&dir);
        let sha = install_and_select(&dir, &mut driver, &cx, "heavy.plxhud.json", &over_a_ceiling(), HEAVY_ID);
        let asked = std::cell::Cell::new(0);
        let measure = Counting(&asked);
        let env = Env { measure: &measure, cx: &cx, app_version: APP, player: false };
        let mut guard = 0;
        while driver.pending.is_none() {
            assert!(driver.tick(&env).is_none());
            guard += 1;
            assert!(guard < 64, "the bytes land");
        }
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Price), "the fit passed");
        assert!(driver.unfit.is_empty() && driver.take_log().is_empty(), "and said nothing");
        asked.set(0);
        assert!(driver.tick(&env).is_none(), "the price tick hands the player nothing");
        assert!(driver.pending.is_none() && driver.compiled.is_none(), "refused at the price stage: the load never ran");
        assert_eq!(driver.unfit(sha, "en"), Some(&Unfit { code: "HUD-E1401" }));
        let lines = driver.take_log();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("hud: fallback ") && lines[0].contains(HEAVY_ID) && lines[0].contains("language=en") && lines[0].contains("code=HUD-E1401"),
            "{lines:?}"
        );
        assert_eq!(driver.store().selected(), HEAVY_ID, "the selection is kept");
        for _ in 0..20 {
            assert!(driver.tick(&env).is_none(), "classic keeps drawing");
        }
        assert_eq!(asked.get(), 0, "and nothing compiles again");
        assert!(driver.take_log().is_empty(), "once per package, language and cause");
        assert_eq!(driver.unfit_shas("en"), [sha], "and the page marks the row");
    }

    /// **The price is fail-closed.** With the model's faces missing or unreadable the package falls back to classic with its
    /// own code, `faces`, and is never fitted, loaded or handed to the player: it must not reach it unpriced.
    #[test]
    fn a_package_is_never_handed_to_the_player_unpriced_when_the_faces_cannot_be_read() {
        for damage in ["missing", "corrupt"] {
            let dir = Dir::new(damage);
            let cx = english();
            let mut driver = driver(&dir);
            let faces = [dir.0.join("regular.ttf"), dir.0.join("bold.ttf")];
            if damage == "corrupt" {
                for face in &faces {
                    std::fs::write(face, b"not a font").unwrap();
                }
            }
            driver.faces = Some(faces);
            dir.drop_file("reference.plxhud.json", REFERENCE);
            driver.refresh(String::new());
            settle(&mut driver, &cx);
            driver.install_drop(driver.store().drops()[0].sha.unwrap(), 1_700_000_000);
            settle(&mut driver, &cx);
            let sha = entry(&driver, REFERENCE_ID).unwrap().sha.unwrap();
            driver.select(REFERENCE_ID.to_string());
            let asked = std::cell::Cell::new(0);
            let measure = Counting(&asked);
            assert!(settle_with(&mut driver, &cx, &measure).is_empty(), "{damage}: classic keeps drawing");
            assert_eq!(asked.get(), 0, "{damage}: the package was neither fitted nor loaded");
            assert!(driver.compiled.is_none() && driver.price.is_none() && driver.pending.is_none(), "{damage}");
            assert_eq!(driver.unfit(sha, "en"), Some(&Unfit { code: "faces" }), "{damage}: the row carries a reason");
            assert_eq!(driver.store().selected(), REFERENCE_ID, "{damage}: the selection is kept");
            let lines = driver.take_log();
            assert!(lines.iter().any(|l| l.starts_with("hud: price model unavailable: ") && l.contains("regular.ttf")), "{damage}: {lines:?}");
            assert_eq!(lines.iter().filter(|l| l.starts_with("hud: fallback ")).count(), 1, "{damage}: {lines:?}");
            assert!(lines.iter().any(|l| l.contains("code=faces") && l.contains(REFERENCE_ID) && l.contains("language=en")), "{damage}: {lines:?}");
            assert!(settle(&mut driver, &cx).is_empty() && driver.take_log().is_empty(), "{damage}: once, not on every tick");
        }
    }

    /// **The faces are read once**, on the worker, in the job of the first pack read that reaches the bytes: that job carries
    /// the model, no later one does, and a package read after the files are gone is still priced.
    #[test]
    fn the_price_model_is_read_once_for_the_process_and_in_the_pack_reads_job() {
        fn is_send<T: Send>() {}
        is_send::<WholePixelMeasure>();
        is_send::<Landed>();

        let dir = Dir::new("faces-once");
        let cx = english();
        let mut driver = driver(&dir);
        let faces = [dir.0.join("regular.ttf"), dir.0.join("bold.ttf")];
        for (copy, shipped) in faces.iter().zip(pkg_faces()) {
            std::fs::copy(shipped, copy).unwrap();
        }
        driver.faces = Some(faces.clone());
        let reference = install_and_select(&dir, &mut driver, &cx, "reference.plxhud.json", REFERENCE, REFERENCE_ID);
        let minimal = install_and_select(&dir, &mut driver, &cx, "minimal.plxhud.json", MINIMAL, "com.plxnative.examples.minimal");
        driver.select(REFERENCE_ID.to_string());
        assert!(driver.price.is_none(), "nothing has been read yet");
        // the first pack read carries the faces, in its own job; the listing and the install read none
        let mut job = driver.begin_read(reference);
        let landed = job.slot.poll().flatten().expect("the inline job ran");
        assert!(landed.bytes.is_some() && landed.model.as_ref().is_some_and(|m| m.is_ok()), "the first pack read reads the faces too");
        assert!(settle(&mut driver, &cx).len() == 1 && driver.price.is_some(), "and the driver keeps the model");
        // once it is kept, no pack read carries it again
        let mut job = driver.begin_read(reference);
        assert!(job.slot.poll().flatten().expect("the inline job ran").model.is_none());

        // the files are gone: a second package is still priced, from the model the process has
        for face in &faces {
            std::fs::remove_file(face).unwrap();
        }
        driver.select("com.plxnative.examples.minimal".to_string());
        let shown = settle(&mut driver, &cx);
        assert!(shown.len() == 1 && shown[0].key.sha256 == plx_base::sha256::sha256(MINIMAL.as_bytes()), "priced without a second read");
        assert!(driver.unfit(minimal, "en").is_none() && driver.take_log().is_empty());
    }

    /// **`builtin:classic` never reaches the lint**, and never asks for the faces: it is compiled at boot by `load_classic` and
    /// is handed back by a selection with no stage at all, with the model's faces missing.
    #[test]
    fn builtin_classic_never_reaches_the_lint_or_the_faces() {
        let dir = Dir::new("classic-never-lints");
        let cx = english();
        let mut driver = driver(&dir);
        driver.faces = Some([dir.0.join("nowhere-regular.ttf"), dir.0.join("nowhere-bold.ttf")]);
        install_reference(&dir, &mut driver, &cx);
        let asked = std::cell::Cell::new(0);
        let measure = Counting(&asked);
        // the built-in is the selection, and a refresh reads it again
        driver.select(BUILTIN_CLASSIC.to_string());
        assert!(settle_with(&mut driver, &cx, &measure).is_empty(), "classic is already what draws");
        driver.refresh(String::new());
        assert!(settle_with(&mut driver, &cx, &measure).is_empty());
        assert_eq!(asked.get(), 0, "no lint, no load: the text measure was not asked a thing");
        assert!(driver.price.is_none() && driver.reading.is_none() && driver.pending.is_none() && driver.compiled.is_none(), "no stage, no faces");
        assert!(driver.unfit.is_empty() && driver.take_log().is_empty());
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

    /// The staged packs the tests below leave in the driver: (the stage it waits at, the package, its id). The heavy one
    /// passes the fit and is refused by the price, so a stage run for it that should not have run is a `hud: fallback` line.
    fn staged_packs() -> [(Stage, String, &'static str); 3] {
        [
            (Stage::Fit, REFERENCE.to_string(), REFERENCE_ID),
            (Stage::Price, over_a_ceiling(), HEAVY_ID),
            (Stage::Load, REFERENCE.to_string(), REFERENCE_ID),
        ]
    }

    /// Select the installed `id` and tick until its pack waits at `stage`, then stop. The fit is reached with a player up, because
    /// the tick that lands the bytes runs it otherwise; the others wait at the stage the tick before them reached.
    fn wait_at(driver: &mut HudDriver, cx: &LocaleContext, id: &str, stage: Stage) {
        driver.select(id.to_string());
        let env = Env { measure: &ShippedMeasure, cx, app_version: APP, player: stage == Stage::Fit };
        let mut guard = 0;
        while !driver.pending.as_ref().is_some_and(|p| p.stage == stage) {
            assert!(driver.tick(&env).is_none());
            guard += 1;
            assert!(guard < 64, "the pack reaches {stage:?}");
        }
    }

    /// Tick with no player until the pack `sha` has landed and been fitted, then once per remaining stage: the fit, the price
    /// and the load on three successive ticks, the last one handing the package over.
    fn compiles_in_three_ticks(driver: &mut HudDriver, cx: &LocaleContext, measure: &dyn Measure, sha: Sha, text: &str) {
        let env = Env { measure, cx, app_version: APP, player: false };
        let mut guard = 0;
        while driver.pending.is_none() {
            assert!(driver.tick(&env).is_none());
            guard += 1;
            assert!(guard < 64, "the bytes land");
        }
        assert!(driver.pending.as_ref().is_some_and(|p| p.sha == sha && p.stage == Stage::Price), "the tick that landed them fitted them");
        assert!(driver.tick(&env).is_none() && driver.compiled.is_none());
        assert!(driver.pending.as_ref().is_some_and(|p| p.stage == Stage::Load), "priced, the load is next");
        let shown = driver.tick(&env);
        assert!(shown.is_some_and(|h| h.key.sha256 == plx_base::sha256::sha256(text.as_bytes())), "the third tick hands the package over");
        let language = cx.language().tag();
        assert!(driver.compiled.as_ref().is_some_and(|(s, l, _)| *s == sha && *l == language) && driver.pending.is_none());
    }

    /// A language that is not English: the one a driver is moved to.
    fn other_language() -> LocaleContext {
        let other = Preference::SHIPPED.into_iter().find(|p| p.tag() != "en").expect("a second shipped language");
        LocaleContext::resolve(other, None, None, None, None)
    }

    /// **A pack the viewer moved away from is dropped, at whatever stage it waits.** The new pick does not wait on it, it is
    /// not compiled, it does not overwrite the compile, and a refusal it would have met is not logged against the pick that
    /// replaced it (`fall_back` names the resolved selection). The new pack then takes the fit, the price and the load on
    /// three successive ticks.
    #[test]
    fn a_pack_the_viewer_moved_away_from_is_dropped_at_every_stage() {
        for (stage, text, id) in staged_packs() {
            let dir = Dir::new("moved-on");
            let cx = english();
            let mut driver = driver(&dir);
            let old = install(&dir, &mut driver, &cx, "old.plxhud.json", &text, id);
            let new = install(&dir, &mut driver, &cx, "minimal.plxhud.json", MINIMAL, MINIMAL_ID);
            wait_at(&mut driver, &cx, id, stage);
            assert!(driver.pending.as_ref().is_some_and(|p| p.sha == old), "{stage:?}: the old pack waits");

            driver.select(MINIMAL_ID.to_string());
            let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: false };
            assert!(driver.tick(&env).is_none());
            assert!(driver.pending.is_none(), "{stage:?}: the old pack is dropped, and spends no stage");
            assert!(driver.compiled.is_none() && driver.unfit.is_empty(), "{stage:?}: it compiled nothing and was refused nothing");
            assert!(driver.take_log().is_empty(), "{stage:?}: and said nothing, least of all about the new pick");
            assert!(driver.reading.as_ref().is_some_and(|r| r.sha == new), "{stage:?}: the new pick is being read");

            compiles_in_three_ticks(&mut driver, &cx, &ShippedMeasure, new, MINIMAL);
            assert!(driver.take_log().is_empty() && driver.unfit.is_empty(), "{stage:?}");
        }
    }

    /// **The same when the pick goes back to classic**: classic is what draws, the old pack is not compiled and no
    /// `hud: fallback` line names `builtin:classic`.
    #[test]
    fn a_pack_the_viewer_went_back_to_classic_from_is_dropped_at_every_stage() {
        for (stage, text, id) in staged_packs() {
            let dir = Dir::new("back-to-classic");
            let cx = english();
            let mut driver = driver(&dir);
            install(&dir, &mut driver, &cx, "old.plxhud.json", &text, id);
            wait_at(&mut driver, &cx, id, stage);

            driver.select(BUILTIN_CLASSIC.to_string());
            let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: false };
            for _ in 0..20 {
                assert!(driver.tick(&env).is_none(), "{stage:?}: classic is already what draws");
            }
            assert!(driver.pending.is_none() && driver.reading.is_none(), "{stage:?}: the old pack is dropped");
            assert!(driver.compiled.is_none() && driver.unfit.is_empty(), "{stage:?}: it compiled nothing and was refused nothing");
            let classic = driver.classic.as_ref().map(|c| Arc::clone(&c.compiled));
            assert!(driver.shown.as_ref().zip(classic.as_ref()).is_some_and(|(shown, classic)| Arc::ptr_eq(shown, classic)), "{stage:?}: classic is shown");
            assert!(driver.take_log().is_empty(), "{stage:?}: no fallback line, for the old pack or for classic");
        }
    }

    /// **A read in flight across a pick lands as nothing.** The read was for a pack nobody wants any more: no pending, no
    /// `read_failed`, no line for it (a read that found nothing is not a refusal of the new pick), but the price model that
    /// came with the answer IS kept, because it is the process's and not the pack's.
    #[test]
    fn a_read_in_flight_across_a_pick_lands_as_nothing_but_keeps_the_price_model() {
        for (found, wanted) in [(true, MINIMAL_ID), (false, MINIMAL_ID), (true, BUILTIN_CLASSIC), (false, BUILTIN_CLASSIC)] {
            let dir = Dir::new("read-in-flight");
            let cx = english();
            let mut driver = driver(&dir);
            let reference = install(&dir, &mut driver, &cx, "reference.plxhud.json", REFERENCE, REFERENCE_ID);
            install(&dir, &mut driver, &cx, "minimal.plxhud.json", MINIMAL, MINIMAL_ID);
            driver.select(REFERENCE_ID.to_string());
            let (answer, slot) = std::sync::mpsc::channel();
            driver.reading = Some(Reading { sha: reference, slot: Slot::Waiting(slot) });
            driver.select(wanted.to_string());
            assert!(driver.price.is_none());

            let [regular, bold] = pkg_faces();
            let model = plx_gfx::text::read_price_model(&regular, &bold);
            let bytes = found.then(|| REFERENCE.as_bytes().to_vec());
            answer.send(Landed { bytes, model: Some(model) }).unwrap();
            let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: false };
            let shown = driver.tick(&env);
            let what = format!("found={found}, wanted={wanted}");
            assert!(shown.is_none(), "{what}");
            assert!(driver.pending.is_none(), "{what}: the pack is not staged");
            assert!(driver.read_failed.is_empty() && driver.unfit.is_empty(), "{what}: and no verdict is recorded");
            assert!(driver.take_log().is_empty(), "{what}: and no line is logged");
            assert!(driver.price.is_some(), "{what}: the price model that came with the answer is kept");
        }
    }

    /// **A language change drops the staged pack, at whatever stage it waits**, and the compile in the new language starts
    /// again at the fit: a pack staged in one language is never loaded in the next unfitted and unpriced and stamped for it.
    /// No shipped process changes language (`i18n`'s `CURRENT` is a `OnceLock`), but the driver must hold where the branch exists.
    #[test]
    fn a_language_change_drops_the_staged_pack_and_the_compile_starts_again_at_the_fit() {
        let other = other_language();
        for stage in [Stage::Fit, Stage::Price, Stage::Load] {
            let dir = Dir::new("language-moves");
            let en = english();
            let mut driver = driver(&dir);
            let sha = install(&dir, &mut driver, &en, "reference.plxhud.json", REFERENCE, REFERENCE_ID);
            wait_at(&mut driver, &en, REFERENCE_ID, stage);
            assert!(driver.pending.as_ref().is_some_and(|p| p.sha == sha && p.stage == stage));

            let asked = std::cell::Cell::new(0);
            let measure = Counting(&asked);
            let env = Env { measure: &measure, cx: &other, app_version: APP, player: false };
            driver.tick(&env);
            assert!(driver.pending.is_none(), "{stage:?}: the staged pack is dropped");
            assert!(driver.compiled.is_none() && driver.unfit.is_empty(), "{stage:?}: nothing was loaded, stamped for the new language");

            asked.set(0);
            compiles_in_three_ticks(&mut driver, &other, &measure, sha, REFERENCE);
            assert!(asked.get() > 0, "{stage:?}: the new language's compile was fitted");
            assert!(driver.unfit.is_empty() && driver.take_log().is_empty(), "{stage:?}: and fits there");
        }
    }

    /// **A face that cannot be read is not a verdict on the package.** The faces are missing: `code=faces`, one line. A
    /// store event tries them again (still missing: the diagnostic line again, and no second `hud: fallback` line, which is
    /// once per package, language and cause). With the faces back, the next event compiles the package.
    #[test]
    fn missing_faces_are_tried_again_after_a_store_event_and_the_package_then_compiles() {
        let dir = Dir::new("faces-come-back");
        let cx = english();
        let mut driver = driver(&dir);
        let faces = [dir.0.join("regular.ttf"), dir.0.join("bold.ttf")];
        driver.faces = Some(faces.clone());
        install_reference(&dir, &mut driver, &cx);
        let sha = entry(&driver, REFERENCE_ID).unwrap().sha.unwrap();
        driver.select(REFERENCE_ID.to_string());
        assert!(settle(&mut driver, &cx).is_empty(), "classic draws while the price model is missing");
        assert_eq!(driver.unfit(sha, "en"), Some(&Unfit { code: "faces" }));
        let first = driver.take_log();
        assert_eq!(first.iter().filter(|l| l.starts_with("hud: fallback ") && l.contains("code=faces")).count(), 1, "{first:?}");

        // a store event with the faces still missing: tried again, and the verdict stands, with no second fallback line
        driver.select(REFERENCE_ID.to_string());
        assert!(settle(&mut driver, &cx).is_empty());
        assert_eq!(driver.unfit(sha, "en"), Some(&Unfit { code: "faces" }));
        let again = driver.take_log();
        assert!(again.len() == 1 && again[0].starts_with("hud: price model unavailable: "), "the faces were read again: {again:?}");

        // the faces are back: the next store event compiles the package
        for (copy, shipped) in faces.iter().zip(pkg_faces()) {
            std::fs::copy(shipped, copy).unwrap();
        }
        driver.select(REFERENCE_ID.to_string());
        let shown = settle(&mut driver, &cx);
        assert!(shown.len() == 1 && shown[0].key.sha256 == plx_base::sha256::sha256(REFERENCE.as_bytes()), "the package draws");
        assert!(driver.unfit(sha, "en").is_none() && driver.price.is_some());
        assert!(driver.take_log().is_empty(), "and no second `hud: fallback` line for the cause already logged");
    }

    /// **Faces that cannot be read are refused at landing, which does not wait for the player**: the line is logged while
    /// a player is up (a refusal found at landing is not a compile), and nothing is staged.
    #[test]
    fn missing_faces_are_logged_at_landing_even_while_a_player_is_up() {
        let dir = Dir::new("faces-player-up");
        let cx = english();
        let mut driver = driver(&dir);
        driver.faces = Some([dir.0.join("regular.ttf"), dir.0.join("bold.ttf")]);
        install_reference(&dir, &mut driver, &cx);
        let sha = entry(&driver, REFERENCE_ID).unwrap().sha.unwrap();
        driver.select(REFERENCE_ID.to_string());
        let env = Env { measure: &ShippedMeasure, cx: &cx, app_version: APP, player: true };
        for _ in 0..64 {
            assert!(driver.tick(&env).is_none());
        }
        assert!(driver.pending.is_none() && driver.compiled.is_none(), "nothing is staged or compiled");
        assert_eq!(driver.unfit(sha, "en"), Some(&Unfit { code: "faces" }));
        let lines = driver.take_log();
        assert_eq!(lines.iter().filter(|l| l.starts_with("hud: fallback ") && l.contains("code=faces")).count(), 1, "{lines:?}");
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
