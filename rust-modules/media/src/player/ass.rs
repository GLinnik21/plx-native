//! Styled subtitles, rendered by our pinned libass on one worker. The SDL thread
//! only replaces a bounded request slot and reads an immutable RGBA publication;
//! it never parses fonts/events, rasterizes, or waits for the native renderer.
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

mod mailbox;
use mailbox::{Job, Kind, Mailbox, Rendered};
use super::ass_timing::{self, Validity};

const MAX_SCRIPT_BYTES: usize = super::ass_script::WINDOW_BYTES;
const MAX_EVENT_BYTES: usize = 256 * 1024;
// No event-count bound here: `ass_source` holds an embedded track, and `ass_script` a long sidecar
// script, to a window far under the native library's own guard (`plx_ass_chunk` refuses past
// 20 000), so a second count would only be a way to hide a track that is otherwise showable.
pub const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
/// Bookkeeping charged per font on top of its bytes, so a file of countless tiny attachments is
/// bounded by the same byte budget without a font-count constant.
const FONT_OVERHEAD_BYTES: usize = 1024;
const MAX_PIXELS: usize = 3840 * 2160;
const MAX_REGIONS: usize = 64;

mod aligns;
pub use aligns::AlignSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Event {
    pub start_ms: i64,
    pub duration_ms: i64,
    /// The complete Matroska ASS packet, including ReadOrder and Layer.
    pub payload: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Font {
    pub name: String,
    pub data: Arc<[u8]>,
}

/// The memory bound on embedded fonts: fonts are admitted in container order until their bytes
/// (plus a small per-font overhead) fill `MAX_FONT_BYTES`. A font that does not fit is skipped
/// and a smaller later one may still fit; text naming a skipped font renders in the app's
/// default face instead of the track being refused or its text dropped.
#[derive(Default)]
pub struct FontBudget {
    used: usize,
}

impl FontBudget {
    pub fn admit(&mut self, len: usize) -> bool {
        let cost = len.saturating_add(FONT_OVERHEAD_BYTES);
        if len == 0 || self.used.saturating_add(cost) > MAX_FONT_BYTES {
            return false;
        }
        self.used += cost;
        true
    }
}

#[derive(Clone, Debug)]
pub enum Content {
    Embedded {
        header: Arc<[u8]>,
        events: Arc<[Event]>,
        fonts: Arc<[Font]>,
    },
    Script {
        bytes: Arc<[u8]>,
        fonts: Arc<[Font]>,
    },
}

/// Where a source's styling comes from, which decides the geometry it renders in (plan
/// `libass-text-subs` §2.1) and whether the viewer's text settings restyle it (§2.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    /// Written by the file's author: the video viewport, and the header is never ours to change.
    #[default]
    Authored,
    /// Synthesized from a text subtitle (`text_ass::header`): the full logical screen, and the
    /// header follows the viewer's size and position picks. The picks the header was synthesized
    /// for travel with it, so the worker sets the line spacing of the header it loads, not of
    /// whatever the viewer has picked since.
    Text(super::text_style::TextStyle),
}

impl Origin {
    pub fn is_text(self) -> bool {
        matches!(self, Origin::Text(_))
    }

    /// Extra pixels between lines the origin's renderer is set to, and whether it breaks lines by
    /// the Unicode rules (plan §4.6, D8). An authored script gets libass's own pitch and the
    /// breaking it always had.
    fn renderer_settings(self) -> Option<(f64, bool)> {
        match self {
            Origin::Authored => None,
            Origin::Text(style) => Some((super::text_style::line_spacing(style.scale()), true)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Source {
    /// Identity of one timeline, independent of packet revision. Embedded seeks
    /// replace it; cached sidecar scripts also use clear()'s cancellation epoch.
    /// Zero is reserved for Off.
    pub id: u64,
    /// Packet append changes this revision without hiding the current frame.
    pub revision: u64,
    pub origin: Origin,
    pub content: Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Straight (not premultiplied) RGBA, composited in libass image-list order.
    pub rgba: Arc<[u8]>,
}

/// **Why a styled subtitle cannot be shown**, as a closed set. The player draws [`Fault::message`]
/// in the UI language; the log names the variant, so it reads the same on every television.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    StartFailed,
    InvalidFile,
    InvalidTrack,
    InvalidEvents,
    TrackTooLarge,
    RenderFailed,
    Unavailable,
    FontsUnavailable,
    InitFailed,
    FontTooLarge,
    Unreadable,
    FontName,
    TrackFonts,
    TooManyEvents,
    InvalidRegions,
    InvalidPixels,
    OverlappingRegions,
}

impl Fault {
    pub fn message(self) -> &'static str {
        use plx_platform::i18n::msg;
        match self {
            Fault::StartFailed => msg::widgets_ass_start_failed(),
            Fault::InvalidFile => msg::widgets_ass_invalid_file(),
            Fault::InvalidTrack => msg::widgets_ass_invalid_track(),
            Fault::InvalidEvents => msg::widgets_ass_invalid_events(),
            Fault::TrackTooLarge => msg::widgets_ass_track_too_large(),
            Fault::RenderFailed => msg::widgets_ass_render_failed(),
            Fault::Unavailable => msg::widgets_ass_unavailable(),
            Fault::FontsUnavailable => msg::widgets_ass_fonts_unavailable(),
            Fault::InitFailed => msg::widgets_ass_init_failed(),
            Fault::FontTooLarge => msg::widgets_ass_font_too_large(),
            Fault::Unreadable => msg::widgets_ass_unreadable(),
            Fault::FontName => msg::widgets_ass_font_name(),
            Fault::TrackFonts => msg::widgets_ass_track_fonts(),
            Fault::TooManyEvents => msg::widgets_ass_too_many_events(),
            Fault::InvalidRegions => msg::widgets_ass_invalid_regions(),
            Fault::InvalidPixels => msg::widgets_ass_invalid_pixels(),
            Fault::OverlappingRegions => msg::widgets_ass_overlapping_regions(),
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    pub source_id: u64,
    /// Changes only when pixels/placement, canvas or the error state changes.
    pub serial: u64,
    pub width: i32,
    pub height: i32,
    /// Disjoint regions; an empty list clears output between dialogue events.
    pub rects: Vec<Rect>,
    pub error: Option<Fault>,
}

pub fn next_source_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What one rendered frame is for. `now_ms` is NOT part of it: the clock moves every tick, and a
/// frame carries the window of playheads it stays right for ([`mailbox`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    epoch: u64,
    source_id: u64,
    revision: u64,
    width: i32,
    height: i32,
    storage_width: i32,
    storage_height: i32,
}

pub struct Runtime {
    source_id: AtomicU64,
    epoch: AtomicU64,
    started: OnceLock<bool>,
    worker: OnceLock<std::thread::Thread>,
    mailbox: Mutex<Mailbox>,
}

impl Runtime {
    pub const fn new() -> Self {
        Self {
            source_id: AtomicU64::new(0),
            epoch: AtomicU64::new(1),
            started: OnceLock::new(),
            worker: OnceLock::new(),
            mailbox: Mutex::new(Mailbox::new()),
        }
    }

    fn wake(&self) {
        // Unlike a condition-variable notification, this permit survives the
        // gap between checking the mailbox and parking. Before registration,
        // the worker has not checked the startup mailbox yet.
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}

#[inline]
fn runtime() -> &'static Runtime {
    &super::SHARED.ass_renderer
}

/// MAIN THREAD: ask for the frame right for `now_ms` and return only this selection's last
/// completed frame. Contention drops this tick's request rather than waiting.
pub fn request(
    source: Arc<Source>,
    now_ms: i64,
    width: i32,
    height: i32,
    storage_width: i32,
    storage_height: i32,
) -> Option<Arc<Frame>> {
    if source.id == 0 {
        clear();
        return None;
    }
    if runtime().source_id.load(Ordering::Acquire) != source.id {
        runtime().epoch.fetch_add(1, Ordering::AcqRel);
        runtime().source_id.store(source.id, Ordering::Release);
    }
    let key = Key {
        epoch: runtime().epoch.load(Ordering::Acquire),
        source_id: source.id,
        revision: source.revision,
        width,
        height,
        storage_width: if storage_width > 0 {
            storage_width
        } else {
            width
        },
        storage_height: if storage_height > 0 {
            storage_height
        } else {
            height
        },
    };
    let started = *runtime()
        .started
        .get_or_init(|| plx_base::task::spawn("ass-render", worker).is_some());
    let mut mailbox = runtime().mailbox.try_lock().ok()?;
    if !started {
        return Some(mailbox.publish_error(key, error_frame(key, Fault::StartFailed)));
    }
    let requested = mailbox.request(key, now_ms, &source);
    if requested.promoted {
        super::ass_perf::note_ahead_hit();
    }
    if requested.wake {
        runtime().wake();
    }
    mailbox.view(&key)
}

/// MAIN THREAD: the alignment classes (bottom, middle, top) of the cues active in the frame
/// [`request`] returned with this `serial`, or `None` when that frame is not the published one any
/// more, is not a Text-origin one, or the mailbox is busy this tick (the caller keeps what it
/// had). The screen reads it to decide which regions the HUD lifts (plan `libass-text-subs`
/// §2.4); asking by serial means another frame's classes are never applied to this one.
pub fn published_aligns(serial: u64) -> Option<AlignSet> {
    let mailbox = runtime().mailbox.try_lock().ok()?;
    mailbox.aligns_of(serial)
}

/// MAIN THREAD: cancellation is immediate even while libass is rendering.
pub fn clear() {
    clear_runtime(runtime());
}

fn clear_runtime(runtime: &Runtime) {
    if runtime.source_id.swap(0, Ordering::AcqRel) == 0 {
        return;
    }
    runtime.epoch.fetch_add(1, Ordering::AcqRel);
    // Cleanup belongs to the worker. In particular, cancellation must neither
    // wait for its mailbox lock nor free a large frame/source on the SDL thread.
    runtime.wake();
}

/// Move retired resources out so native destruction takes place after the
/// caller releases the mailbox lock.
fn retire_off(
    runtime: &Runtime,
    mailbox: &mut Mailbox,
    engine: &mut Engine,
) -> Option<(Mailbox, Engine)> {
    if runtime.source_id.load(Ordering::Acquire) != 0 || (engine.is_empty() && mailbox.is_empty()) {
        return None;
    }
    Some((std::mem::take(mailbox), std::mem::take(engine)))
}

fn next_serial() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn error_frame(key: Key, message: Fault) -> Arc<Frame> {
    Arc::new(Frame {
        source_id: key.source_id,
        serial: next_serial(),
        width: key.width,
        height: key.height,
        rects: Vec::new(),
        error: Some(message),
    })
}

fn worker() {
    runtime()
        .worker
        .set(std::thread::current())
        .expect("one styled subtitle worker");
    super::ass_perf::log_worker_tid();
    let mut engine = Engine::default();
    loop {
        let job = {
            let mut mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(retired) = retire_off(runtime(), &mut mailbox, &mut engine) {
                    // Native destruction can be slow too: release the mailbox first.
                    drop(mailbox);
                    super::ass_perf::flush();
                    drop(retired);
                    mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                if let Some(job) = mailbox.take() {
                    break job;
                }
                drop(mailbox);
                // A queue/cancel between the check above and this park leaves a
                // permit, so it cannot strand native resources until another key.
                // A truly idle worker needs no periodic wakeup.
                std::thread::park();
                mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            }
        };
        if let Some(queued) = job.queued {
            super::ass_perf::note_nwait(queued.elapsed());
        }
        super::ass_perf::begin_call();
        let live = || runtime().epoch.load(Ordering::Acquire) == job.key.epoch;
        let rendered = live().then(|| {
            let job_started = std::time::Instant::now();
            let rendered = engine.render(&job);
            super::ass_perf::record_job(job_started.elapsed(), job.kind != Kind::Normal);
            rendered
        });
        let finished = {
            let mut mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            // Every exit finishes the job, so the mailbox never believes one is still in flight.
            let outcome = rendered.filter(|_| live());
            let ran = outcome.is_some();
            let finished = mailbox.finish(&job, outcome);
            ran.then_some(finished)
        };
        let Some(finished) = finished else { continue };
        super::ass_perf::finish_call(finished.changed, finished.pub_bytes);
        if finished.wake {
            plx_machine::present::wake_from_worker();
        }
    }
}

pub(super) fn validate(source: &Source) -> Result<(), Fault> {
    match &source.content {
        Content::Script { bytes, .. } => {
            // The whole-script bound is for a script loaded whole; one past it is fed in windows
            // (`ScriptIndex::windowed`), each within the same bound except a single line larger than it,
            // which the renderer refuses.
            if bytes.is_empty()
                || (bytes.len() > MAX_SCRIPT_BYTES && !super::ass_script::ScriptIndex::indexable(bytes.len()))
                || bytes.contains(&0)
            {
                return Err(Fault::InvalidFile);
            }
        }
        Content::Embedded {
            header,
            events,
            ..
        } => {
            if header.is_empty()
                || header.len() > MAX_SCRIPT_BYTES
                || header.contains(&0)
            {
                return Err(Fault::InvalidTrack);
            }
            let mut bytes = header.len();
            for event in events.iter() {
                if event.payload.is_empty()
                    || event.payload.len() > MAX_EVENT_BYTES
                    || event.payload.contains(&0)
                    || event.duration_ms <= 0
                    || event.start_ms.checked_add(event.duration_ms).is_none()
                {
                    return Err(Fault::InvalidEvents);
                }
                bytes += event.payload.len();
                if bytes > MAX_SCRIPT_BYTES {
                    return Err(Fault::TrackTooLarge);
                }
            }
        }
    }
    // Fonts are never grounds to refuse a track: `Native::open` skips one it cannot use.
    Ok(())
}

/// Pruning expired events and appending preserves libass's parsed events, font
/// caches and ReadOrder set, including a long event spanning many short ones.
fn append_from(old: &Source, new: &Source) -> Option<usize> {
    if old.id != new.id {
        return None;
    }
    match (&old.content, &new.content) {
        (
            Content::Embedded {
                header: oh,
                events: oe,
                fonts: of,
            },
            Content::Embedded {
                header: nh,
                events: ne,
                fonts: nf,
            },
        ) if oh == nh && of == nf => {
            if ne.starts_with(oe) {
                return Some(oe.len());
            }
            let deadline = prune_deadline(ne);
            let retained: Vec<_> = oe
                .iter()
                .filter(|e| e.start_ms.saturating_add(e.duration_ms) >= deadline)
                .collect();
            (retained.len() <= ne.len()
                && retained.iter().zip(ne.iter()).all(|(old, new)| *old == new))
            .then_some(retained.len())
        }
        _ => None,
    }
}

fn prune_deadline(events: &[Event]) -> i64 {
    events
        .iter()
        .map(|e| e.start_ms.saturating_add(e.duration_ms))
        .min()
        .unwrap_or(i64::MAX)
}

/// Two embedded sources the same renderer can show one after the other: equal header bytes and
/// equal fonts, whichever timeline (`id`) each belongs to. A seek, a demux reopen of the same file
/// and a switch between tracks that share a header all qualify; `plx_ass_flush_events` plus a
/// re-append then replaces the events and keeps the parsed header, the fonts and their caches.
fn same_embedded_resources(old: &Source, new: &Source) -> bool {
    matches!((&old.content, &new.content), (
        Content::Embedded { header: oh, fonts: of, .. },
        Content::Embedded { header: nh, fonts: nf, .. }
    ) if oh == nh && of == nf)
}

/// The synthesized header (or whole script) of a source: what a restyle replaces.
fn track_text(source: &Source) -> &[u8] {
    match &source.content {
        Content::Embedded { header, .. } => header,
        Content::Script { bytes, .. } => bytes,
    }
}

/// A settings change (plan §2.5, §4.6 case 3): both sources are Text-origin tracks of one kind with
/// equal fonts and a different synthesized header. Everything else a header change means is a new
/// renderer.
fn restyles(old: &Source, new: &Source) -> bool {
    old.origin.is_text()
        && new.origin.is_text()
        && std::mem::discriminant(&old.content) == std::mem::discriminant(&new.content)
        && source_fonts(old) == source_fonts(new)
        && track_text(old) != track_text(new)
}

/// A sidecar script's timings, kept whether or not it is windowed: what a rendered frame is valid
/// for depends on every line. A script with more lines than one window holds also records the
/// window the renderer holds now, `lo..hi`; a playhead outside it is fed again ([`super::ass_script`]).
struct Scripted {
    index: super::ass_script::ScriptIndex,
    window: Option<(i64, i64)>,
}

#[derive(Default)]
struct Engine {
    source: Option<Arc<Source>>,
    native: Option<Native>,
    script: Option<Scripted>,
    /// The parse behind [`Engine::aligns`] for a Text script that is not windowed.
    aligns_index: Option<aligns::ScriptAligns>,
    frame: Option<Arc<Frame>>,
    error: Option<Fault>,
}

impl Engine {
    fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.native.is_none()
            && self.frame.is_none()
            && self.error.is_none()
    }

    /// Load `source` fresh. A sidecar script with more lines than one window holds is fed the lines
    /// around `now_ms` only; any other source goes in whole.
    fn open(&mut self, source: &Source, now_ms: i64) -> Result<(), Fault> {
        self.native = None;
        self.script = None;
        let mut windowed = None;
        if let Content::Script { bytes, fonts } = &source.content {
            if let Some(index) = super::ass_script::ScriptIndex::parse(bytes) {
                if index.windowed() {
                    let window = index.window(now_ms);
                    windowed = Some(Native::open_data(fonts, &window.text, 1)?);
                    self.script = Some(Scripted { index, window: Some((window.lo_ms, window.hi_ms)) });
                } else {
                    self.script = Some(Scripted { index, window: None });
                }
            }
        }
        let mut native = match windowed {
            Some(native) => native,
            None => Native::open(source)?,
        };
        // A Text track's renderer breaks lines by the Unicode rules and spaces them for the size
        // its header was synthesized for (plan §4.6); an authored one keeps libass's defaults.
        if let Some((spacing, wrap)) = source.origin.renderer_settings() {
            if !(native.set_wrap_unicode(wrap) && native.set_line_spacing(spacing)) {
                return Err(Fault::InitFailed);
            }
        }
        self.native = Some(native);
        Ok(())
    }

    /// The alignment classes active at `now_ms` in a Text-origin `source`; `None` for an Authored
    /// one, which the screen never lifts.
    fn aligns(&mut self, source: &Source, now_ms: i64) -> Option<AlignSet> {
        if !source.origin.is_text() || self.error.is_some() {
            return None;
        }
        Some(match &source.content {
            Content::Embedded { events, .. } => aligns::of_events(events, now_ms),
            Content::Script { .. } => match &self.script {
                Some(scripted) => aligns::of_script(&scripted.index, now_ms),
                None => aligns::ScriptAligns::at(&mut self.aligns_index, source, now_ms),
            },
        })
    }

    /// The viewer changed the text size or position: `source` is the synthesized track for the new
    /// picks, of the same fonts. Swap the track in place on the held renderer (every opened face
    /// stays), put the held events back, and set the line spacing the new size needs.
    fn restyle(&mut self, source: &Source, now_ms: i64) -> Result<(), Fault> {
        let Some(native) = self.native.as_mut() else {
            return self.open(source, now_ms);
        };
        match &source.content {
            Content::Embedded { header, .. } => {
                native.replace_track(header, 0)?;
                native.append(source, 0)?;
            }
            Content::Script { bytes, .. } => {
                self.script = None;
                match super::ass_script::ScriptIndex::parse(bytes) {
                    Some(index) if index.windowed() => {
                        let window = index.window(now_ms);
                        native.replace_track(&window.text, 1)?;
                        self.script = Some(Scripted { index, window: Some((window.lo_ms, window.hi_ms)) });
                    }
                    parsed => {
                        native.replace_track(bytes, 1)?;
                        self.script = parsed.map(|index| Scripted { index, window: None });
                    }
                }
            }
        }
        // The spacing of the header being loaded, not of the viewer's picks now: they may have moved
        // again since the header was built, and then another restyle is already on its way.
        match source.origin.renderer_settings() {
            Some((spacing, _)) if !native.set_line_spacing(spacing) => Err(Fault::InitFailed),
            _ => Ok(()),
        }
    }

    /// Feed a windowed script again when the playhead has left the window it holds (a seek either
    /// way, or the end of the window). The old track is released first, so two windows are never
    /// held at once.
    fn slide(&mut self, source: &Source, now_ms: i64) -> Result<(), Fault> {
        let (Some(Scripted { index, window: Some(held) }), Content::Script { fonts, .. }) =
            (self.script.as_mut(), &source.content)
        else {
            return Ok(());
        };
        if (held.0..held.1).contains(&now_ms) {
            return Ok(());
        }
        let window = index.window(now_ms);
        *held = (window.lo_ms, window.hi_ms);
        // Every window repeats the script's header, `[Fonts]` included, so the held renderer
        // already has the fonts: swap the track in place rather than reopening every face.
        match self.native.as_mut() {
            Some(native) => native.replace_track(&window.text, 1),
            None => {
                self.native = Some(Native::open_data(fonts, &window.text, 1)?);
                Ok(())
            }
        }
    }

    /// The frame for `job` and the window of playheads it stays right for.
    fn render(&mut self, job: &Job) -> Rendered {
        let frame = self.render_frame(job);
        let validity = if frame.error.is_some() {
            Validity::ALWAYS
        } else {
            self.validity(&job.source, job.now_ms)
        };
        let aligns = self.aligns(&job.source, job.now_ms);
        Rendered { frame, validity, aligns }
    }

    /// The window at `now_ms` from every event's timing, clamped to the script window held.
    fn validity(&self, source: &Source, now_ms: i64) -> Validity {
        use ass_timing::{Motion, Span};
        let mut validity = match (&source.content, &self.script) {
            (Content::Embedded { events, .. }, _) => ass_timing::validity(
                events.iter().map(|e| Span {
                    start_ms: e.start_ms,
                    end_ms: e.start_ms.saturating_add(e.duration_ms),
                    motion: Motion::Packet(&e.payload),
                }),
                now_ms,
            ),
            (Content::Script { .. }, Some(script)) => ass_timing::validity(script.index.spans(), now_ms),
            // unreachable (a script is parsed on open): render per tick rather than guess
            (Content::Script { .. }, None) => {
                Validity { from_ms: now_ms, until_ms: now_ms.saturating_add(1), animated: true }
            }
        };
        if let Some((lo, hi)) = self.script.as_ref().and_then(|s| s.window) {
            validity.from_ms = validity.from_ms.max(lo);
            validity.until_ms = validity.until_ms.min(hi);
        }
        validity
    }

    fn render_frame(&mut self, request: &Job) -> Arc<Frame> {
        let key = request.key;
        let source_changed = self.source.as_ref().is_none_or(|old| {
            old.id != request.source.id || old.revision != request.source.revision
        });
        if source_changed {
            self.error = validate(&request.source).err();
            if self.error.is_none() {
                let append = self
                    .source
                    .as_ref()
                    .and_then(|old| append_from(old, &request.source));
                let result = if let (Some(from), Some(native)) = (append, self.native.as_mut()) {
                    let Content::Embedded { events, .. } = &request.source.content else {
                        unreachable!()
                    };
                    unsafe { plx_ass_prune_before(native.0, prune_deadline(events)) };
                    native.append(&request.source, from)
                } else if self
                    .source
                    .as_ref()
                    .is_some_and(|old| same_embedded_resources(old, &request.source))
                    && self.native.is_some()
                {
                    // An out-of-order or size-capped window may not be expressible
                    // as expiry + append, and a seek or a track switch brings a different
                    // timeline. Reset events only, retaining parsed header, fonts and caches.
                    let native = self.native.as_mut().unwrap();
                    unsafe { plx_ass_flush_events(native.0) };
                    native.append(&request.source, 0)
                } else if self.source.as_ref().is_some_and(|old| restyles(old, &request.source)) {
                    self.restyle(&request.source, request.now_ms)
                } else {
                    self.open(&request.source, request.now_ms)
                };
                self.error = result.err();
            }
            self.source = Some(request.source.clone());
            if let Some(error) = self.error {
                self.native = None;
                plx_base::eventlog::log(&format!("subtitle: ASS refused: {error:?}"));
            }
        }
        if self.error.is_none() {
            if let Err(error) = self.slide(&request.source, request.now_ms) {
                self.error = Some(error);
                self.native = None;
                plx_base::eventlog::log(&format!("subtitle: ASS refused: {error:?}"));
            }
        }
        if let Some(error) = self.error {
            if let Some(old) = &self.frame {
                if old.source_id == key.source_id
                    && old.width == key.width
                    && old.height == key.height
                    && old.error == Some(error)
                {
                    return old.clone();
                }
            }
            let frame = error_frame(key, error);
            self.frame = Some(frame.clone());
            return frame;
        }
        let previous = self
            .frame
            .as_ref()
            .filter(|frame| {
                frame.source_id == key.source_id
                    && frame.width == key.width
                    && frame.height == key.height
            })
            .map_or(&[][..], |frame| frame.rects.as_slice());
        let result = self
            .native
            .as_mut()
            .expect("loaded ASS source")
            .render(key, request.now_ms, previous);
        let frame = match result {
            Ok(None) => match &self.frame {
                Some(frame) if frame.error.is_none() => {
                    if frame.source_id == key.source_id
                        && frame.width == key.width
                        && frame.height == key.height
                    {
                        return frame.clone();
                    }
                    // libass reports "unchanged" against the track the renderer showed before a
                    // reuse (a seek, a track switch with the same header): the pixels stand, but
                    // the frame must name the source it now answers for, or it is never shown.
                    Arc::new(Frame {
                        source_id: key.source_id,
                        serial: next_serial(),
                        width: key.width,
                        height: key.height,
                        rects: frame.rects.clone(),
                        error: None,
                    })
                }
                Some(frame) => return frame.clone(),
                None => error_frame(key, Fault::RenderFailed),
            },
            Ok(Some(rects)) => {
                if let Some(old) = &self.frame {
                    if old.source_id == key.source_id
                        && old.width == key.width
                        && old.height == key.height
                        && old.error.is_none()
                        && old.rects == rects
                    {
                        return old.clone();
                    }
                }
                Arc::new(Frame {
                    source_id: key.source_id,
                    serial: next_serial(),
                    width: key.width,
                    height: key.height,
                    rects,
                    error: None,
                })
            }
            Err(error) => {
                // The facade may have rejected the frame mid-draw: a kept renderer would answer a
                // later reuse with "unchanged" and hand back this error frame for a new source.
                self.native = None;
                self.error = Some(error);
                plx_base::eventlog::log(&format!("subtitle: ASS render failed: {error:?}"));
                error_frame(key, error)
            }
        };
        self.frame = Some(frame.clone());
        frame
    }
}

#[repr(C)]
#[derive(Default)]
pub struct Bitmap {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    bytes: usize,
    rgba: *const u8,
}

#[repr(C)]
#[derive(Default)]
pub struct NativeFrame {
    count: usize,
    regions: *const Bitmap,
}

/// `PlxAssStats` in `include/ass.h`.
#[repr(C)]
#[derive(Default)]
pub struct NativeStats {
    library_fonts: u32,
    font_scans: u32,
    tracks: u32,
}

/// The facade ABI this file is written against (`PLX_ASS_ABI_VERSION` in `include/ass.h`); a
/// library reporting any other number is not loaded. ABI 4 added only the UI text surface
/// (`plx_ass_text_*`, bound by `plx_gfx::rtl`); every function this file calls is unchanged.
const NATIVE_ABI: u32 = 4;

/// libass's glyph-count and bitmap-MiB cache bounds for every renderer.
const CACHE_GLYPHS: i32 = 1000;
const CACHE_BITMAP_MIB: i32 = 24;

plx_base::dynlib! {
    native_ass: ["libass-plx-host.so.0", "libass-plx.so.0", "libass-plx.0.dylib"] {
        fn plx_ass_abi_version() -> u32;
        fn plx_ass_create(default_font: *const c_char) -> *mut c_void;
        fn plx_ass_destroy(ctx: *mut c_void);
        fn plx_ass_add_font(ctx: *mut c_void, name: *const c_char, data: *const u8, len: usize) -> i32;
        fn plx_ass_load(ctx: *mut c_void, data: *const u8, len: usize, script: i32) -> i32;
        fn plx_ass_chunk(ctx: *mut c_void, data: *const u8, len: usize, start_ms: i64, duration_ms: i64) -> i32;
        fn plx_ass_prune_before(ctx: *mut c_void, deadline_ms: i64);
        fn plx_ass_flush_events(ctx: *mut c_void);
        fn plx_ass_replace_track(ctx: *mut c_void, data: *const u8, len: usize, script: i32) -> i32;
        fn plx_ass_set_features(ctx: *mut c_void, wrap_unicode: i32) -> i32;
        fn plx_ass_set_cache_limits(ctx: *mut c_void, glyph_max: i32, bitmap_mib: i32) -> i32;
        fn plx_ass_set_line_spacing(ctx: *mut c_void, spacing: f64) -> i32;
        fn plx_ass_stats(ctx: *const c_void, out: *mut NativeStats) -> i32;
        fn plx_ass_render(ctx: *mut c_void, now_ms: i64, width: i32, height: i32, storage_width: i32, storage_height: i32, frame: *mut NativeFrame) -> i32;
    }
}

struct Native(*mut c_void);

#[cfg(test)]
thread_local! {
    /// Renderers created on this thread: the cost D12's reuse exists to avoid (a library, a
    /// renderer and every face re-read). Per thread because the native tests run in parallel.
    static OPEN_DATA_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn source_fonts(source: &Source) -> &[Font] {
    let (Content::Embedded { fonts, .. } | Content::Script { fonts, .. }) = &source.content;
    fonts
}

fn asset_dir() -> &'static std::path::Path {
    // Host unit tests execute in target/debug/deps; their bundled native fixture
    // and fonts live in pkg. This override cannot enter the television binary.
    #[cfg(any(test, feature = "test-support"))]
    {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../pkg"))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        plx_base::paths::app_dir()
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        unsafe { plx_ass_destroy(self.0) };
    }
}

impl Native {
    fn open(source: &Source) -> Result<Self, Fault> {
        match &source.content {
            Content::Embedded { header, .. } => {
                let mut native = Self::open_data(source_fonts(source), header, 0)?;
                native.append(source, 0)?;
                Ok(native)
            }
            Content::Script { bytes, .. } => Self::open_data(source_fonts(source), bytes, 1),
        }
    }

    /// A renderer holding `fonts` and the track `data` reads as: a script (`script` 1) or a codec
    /// private header (0).
    fn open_data(fonts: &[Font], data: &[u8], script: i32) -> Result<Self, Fault> {
        #[cfg(test)]
        OPEN_DATA_CALLS.with(|c| c.set(c.get() + 1));
        static LOADED: OnceLock<bool> = OnceLock::new();
        let loaded = *LOADED.get_or_init(|| {
            native_ass::load(Some(asset_dir())).ok() && unsafe { plx_ass_abi_version() } == NATIVE_ABI
        });
        if !loaded {
            return Err(Fault::Unavailable);
        }
        let fallback = asset_dir().join("appfont-cjk.ttf");
        let fallback = CString::new(fallback.to_string_lossy().as_bytes())
            .map_err(|_| Fault::FontsUnavailable)?;
        let ptr = unsafe { plx_ass_create(fallback.as_ptr()) };
        if ptr.is_null() {
            return Err(Fault::InitFailed);
        }
        let mut native = Self(ptr);
        native.configure()?;
        // Inter, Inter Bold, then the per-script Noto faces (`fonts.rs`: the order is load-bearing,
        // and the files are read here, per renderer, never kept). The CJK face is libass's DEFAULT
        // path (`plx_ass_create`), which maps the file instead of copying it; as a memory font it
        // would cost a ~13 MB malloc+copy per renderer, reachable only by the family name
        // "Noto Sans CJK KR", while glyph fallback never goes through it.
        for path in fonts::bundled(asset_dir())? {
            let bytes = std::fs::read(&path).map_err(|_| Fault::FontsUnavailable)?;
            if bytes.len() > MAX_FONT_BYTES {
                return Err(Fault::FontTooLarge);
            }
            let name = path.file_name().and_then(|n| n.to_str()).ok_or(Fault::FontsUnavailable)?;
            native.font(name, &bytes)?;
        }
        // A font the renderer cannot take (unnamed, over the budget, rejected by the native
        // side) is skipped: the text still shows, in the default face.
        let mut budget = FontBudget::default();
        for font in fonts.iter() {
            if font.name.is_empty() || font.name.len() > 1024 || font.name.contains('\0')
                || !budget.admit(font.data.len())
            {
                continue;
            }
            let _ = native.font(&font.name, &font.data);
        }
        if unsafe { plx_ass_load(native.0, data.as_ptr(), data.len(), script) } < 0 {
            return Err(Fault::Unreadable);
        }
        Ok(native)
    }

    /// The renderer settings every renderer starts with. Authored scripts break lines the way
    /// they always have (at spaces and `\N`), with no extra line spacing.
    fn configure(&mut self) -> Result<(), Fault> {
        let applied = unsafe {
            plx_ass_set_cache_limits(self.0, CACHE_GLYPHS, CACHE_BITMAP_MIB) >= 0
                && plx_ass_set_features(self.0, 0) >= 0
                && plx_ass_set_line_spacing(self.0, 0.0) >= 0
        };
        applied.then_some(()).ok_or(Fault::InitFailed)
    }

    /// Break lines by the Unicode rules (libunibreak) in the loaded track and every later one.
    fn set_wrap_unicode(&mut self, on: bool) -> bool {
        unsafe { plx_ass_set_features(self.0, i32::from(on)) >= 0 }
    }

    /// Extra pixels between lines at the render size; negative tightens.
    fn set_line_spacing(&mut self, spacing: f64) -> bool {
        unsafe { plx_ass_set_line_spacing(self.0, spacing) >= 0 }
    }

    /// Swap the loaded track for `data` on the same library and renderer, keeping every opened
    /// face. Only for a track whose `[Fonts]` section is the first one's (a window of the same
    /// script, or a header without one): the new track's fonts are not extracted.
    fn replace_track(&mut self, data: &[u8], script: i32) -> Result<(), Fault> {
        if unsafe { plx_ass_replace_track(self.0, data.as_ptr(), data.len(), script) } < 0 {
            return Err(Fault::Unreadable);
        }
        Ok(())
    }

    #[cfg(test)]
    fn stats(&self) -> NativeStats {
        let mut stats = NativeStats::default();
        assert!(unsafe { plx_ass_stats(self.0, &mut stats) } >= 0);
        stats
    }

    fn font(&mut self, name: &str, bytes: &[u8]) -> Result<(), Fault> {
        let name = CString::new(name).map_err(|_| Fault::FontName)?;
        if unsafe { plx_ass_add_font(self.0, name.as_ptr(), bytes.as_ptr(), bytes.len()) } < 0 {
            return Err(Fault::TrackFonts);
        }
        Ok(())
    }

    fn append(&mut self, source: &Source, from: usize) -> Result<(), Fault> {
        if let Content::Embedded { events, .. } = &source.content {
            for event in &events[from..] {
                if unsafe {
                    plx_ass_chunk(
                        self.0,
                        event.payload.as_ptr(),
                        event.payload.len(),
                        event.start_ms,
                        event.duration_ms,
                    )
                } < 0
                {
                    return Err(Fault::TooManyEvents);
                }
            }
        }
        Ok(())
    }

    fn render(&mut self, key: Key, now_ms: i64, previous: &[Rect]) -> Result<Option<Vec<Rect>>, Fault> {
        let mut frame = NativeFrame::default();
        let started = std::time::Instant::now();
        let result = unsafe {
            plx_ass_render(
                self.0,
                now_ms,
                key.width,
                key.height,
                key.storage_width,
                key.storage_height,
                &mut frame,
            )
        };
        super::ass_perf::record_render(started.elapsed());
        if result < 0 {
            return Err(Fault::RenderFailed);
        }
        if result == 0 {
            return Ok(None);
        }
        if frame.count == 0 {
            return Ok(Some(Vec::new()));
        }
        if frame.count > MAX_REGIONS || frame.regions.is_null() {
            return Err(Fault::InvalidRegions);
        }
        let bitmaps = unsafe { std::slice::from_raw_parts(frame.regions, frame.count) };
        let mut rects: Vec<Rect> = Vec::with_capacity(frame.count);
        let mut total_pixels = 0usize;
        for bitmap in bitmaps {
            let pixels = (bitmap.width as usize).checked_mul(bitmap.height as usize);
            if bitmap.width <= 0
                || bitmap.height <= 0
                || bitmap.x < 0
                || bitmap.y < 0
                || bitmap
                    .x
                    .checked_add(bitmap.width)
                    .is_none_or(|x| x > key.width)
                || bitmap
                    .y
                    .checked_add(bitmap.height)
                    .is_none_or(|y| y > key.height)
                || pixels.is_none_or(|n| n > MAX_PIXELS || n * 4 != bitmap.bytes)
                || bitmap.rgba.is_null()
            {
                return Err(Fault::InvalidPixels);
            }
            total_pixels += pixels.unwrap();
            if total_pixels > MAX_PIXELS
                || rects.iter().any(|r| {
                    r.x < bitmap.x + bitmap.width
                        && bitmap.x < r.x + r.width
                        && r.y < bitmap.y + bitmap.height
                        && bitmap.y < r.y + r.height
                })
            {
                return Err(Fault::OverlappingRegions);
            }
            let pixels = unsafe { std::slice::from_raw_parts(bitmap.rgba, bitmap.bytes) };
            let same_pixels = |old: &&Rect| {
                old.width == bitmap.width
                    && old.height == bitmap.height
                    && old.rgba.as_ref() == pixels
            };
            // Compare before copying. Static signs retain their immutable pixel allocation
            // while another region animates; translations can reuse it at a new position too.
            let old = previous
                .iter()
                .filter(|r| r.x == bitmap.x && r.y == bitmap.y)
                .find(same_pixels)
                .or_else(|| previous.iter().find(same_pixels));
            let rgba = old.map_or_else(|| Arc::from(pixels), |r| r.rgba.clone());
            rects.push(Rect {
                x: bitmap.x,
                y: bitmap.y,
                width: bitmap.width,
                height: bitmap.height,
                rgba,
            });
        }
        Ok(Some(rects))
    }
}

mod fonts;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod mailbox_tests;
