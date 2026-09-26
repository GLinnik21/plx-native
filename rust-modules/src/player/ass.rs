//! Styled subtitles, rendered by our pinned libass on one worker. The SDL thread
//! only replaces a bounded request slot and reads an immutable RGBA publication;
//! it never parses fonts/events, rasterizes, or waits for the native renderer.
use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_SCRIPT_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 20_000;
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FONTS: usize = 128;
const MAX_PIXELS: usize = 3840 * 2160;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Event {
    pub start_ms: i64,
    pub duration_ms: i64,
    /// The complete Matroska ASS packet, including ReadOrder and Layer.
    pub payload: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Font {
    pub name: String,
    pub data: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub(crate) enum Content {
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

#[derive(Clone, Debug)]
pub(crate) struct Source {
    /// Identity of one timeline, independent of packet revision. Embedded seeks
    /// replace it; cached sidecar scripts also use clear()'s cancellation epoch.
    /// Zero is reserved for Off.
    pub id: u64,
    /// Packet append changes this revision without hiding the current frame.
    pub revision: u64,
    pub content: Content,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Straight (not premultiplied) RGBA, composited in libass image-list order.
    pub rgba: Arc<[u8]>,
}

#[derive(Debug)]
pub(crate) struct Frame {
    pub source_id: u64,
    /// Changes only when pixels/placement, canvas or the error state changes.
    pub serial: u64,
    pub width: i32,
    pub height: i32,
    /// None clears the old texture (a valid gap between dialogue events).
    pub rect: Option<Rect>,
    pub error: Option<&'static str>,
}

pub(crate) fn next_source_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    epoch: u64,
    source_id: u64,
    revision: u64,
    now_ms: i64,
    width: i32,
    height: i32,
    storage_width: i32,
    storage_height: i32,
}

struct Request {
    key: Key,
    source: Arc<Source>,
}

#[derive(Default)]
struct Mailbox {
    pending: Option<Request>,
    requested: Option<Key>,
    published: Option<(u64, Arc<Frame>)>,
}

pub(crate) struct Runtime {
    source_id: AtomicU64,
    epoch: AtomicU64,
    started: OnceLock<bool>,
    worker: OnceLock<std::thread::Thread>,
    mailbox: Mutex<Mailbox>,
}

impl Runtime {
    pub(crate) const fn new() -> Self {
        Self {
            source_id: AtomicU64::new(0),
            epoch: AtomicU64::new(1),
            started: OnceLock::new(),
            worker: OnceLock::new(),
            mailbox: Mutex::new(Mailbox {
                pending: None,
                requested: None,
                published: None,
            }),
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

/// MAIN THREAD: replace the latest request and return only this selection's last
/// completed frame. Contention drops this tick's request rather than waiting.
pub(crate) fn request(
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
        now_ms,
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
        .get_or_init(|| crate::task::spawn("styled subtitles", worker).is_some());
    if !started {
        let mut mailbox = runtime().mailbox.try_lock().ok()?;
        if let Some((epoch, frame)) = &mailbox.published {
            if *epoch == key.epoch && frame.width == width && frame.height == height {
                return Some(frame.clone());
            }
        }
        let frame = error_frame(key, "Couldn't start styled subtitles");
        mailbox.published = Some((key.epoch, frame.clone()));
        return Some(frame);
    }
    let mut mailbox = runtime().mailbox.try_lock().ok()?;
    if mailbox.requested != Some(key) {
        mailbox.pending = Some(Request { key, source });
        mailbox.requested = Some(key);
        runtime().wake();
    }
    mailbox.published.as_ref().and_then(|(epoch, frame)| {
        (*epoch == key.epoch
            && frame.source_id == key.source_id
            && frame.width == width
            && frame.height == height)
            .then(|| frame.clone())
    })
}

/// MAIN THREAD: cancellation is immediate even while libass is rendering.
pub(crate) fn clear() {
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
    if runtime.source_id.load(Ordering::Acquire) != 0
        || (engine.is_empty()
            && mailbox.pending.is_none()
            && mailbox.requested.is_none()
            && mailbox.published.is_none())
    {
        return None;
    }
    Some((std::mem::take(mailbox), std::mem::take(engine)))
}

fn next_serial() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn error_frame(key: Key, message: &'static str) -> Arc<Frame> {
    Arc::new(Frame {
        source_id: key.source_id,
        serial: next_serial(),
        width: key.width,
        height: key.height,
        rect: None,
        error: Some(message),
    })
}

fn worker() {
    runtime()
        .worker
        .set(std::thread::current())
        .expect("one styled subtitle worker");
    let mut engine = Engine::default();
    loop {
        let request = {
            let mut mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(retired) = retire_off(runtime(), &mut mailbox, &mut engine) {
                    // Native destruction can be slow too: release the mailbox first.
                    drop(mailbox);
                    drop(retired);
                    mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                if let Some(request) = mailbox.pending.take() {
                    break request;
                }
                drop(mailbox);
                // A queue/cancel between the check above and this park leaves a
                // permit, so it cannot strand native resources until another key.
                // A truly idle worker needs no periodic wakeup.
                std::thread::park();
                mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            }
        };
        if runtime().epoch.load(Ordering::Acquire) != request.key.epoch {
            continue;
        }
        let frame = engine.render(&request);
        if runtime().epoch.load(Ordering::Acquire) != request.key.epoch {
            continue;
        }
        let changed = {
            let mut mailbox = runtime().mailbox.lock().unwrap_or_else(|e| e.into_inner());
            if runtime().epoch.load(Ordering::Acquire) != request.key.epoch {
                continue;
            }
            let changed = mailbox.published.as_ref().is_none_or(|(epoch, old)| {
                *epoch != request.key.epoch || old.serial != frame.serial
            });
            mailbox.published = Some((request.key.epoch, frame));
            changed
        };
        if changed {
            crate::ui::present::wake_from_worker();
        }
    }
}

fn validate(source: &Source) -> Result<(), &'static str> {
    match &source.content {
        Content::Script { bytes, .. } => {
            if bytes.is_empty() || bytes.len() > MAX_SCRIPT_BYTES || bytes.contains(&0) {
                return Err("This styled subtitle file is invalid or too large");
            }
        }
        Content::Embedded {
            header,
            events,
            fonts,
        } => {
            if header.is_empty()
                || header.len() > MAX_SCRIPT_BYTES
                || header.contains(&0)
                || events.len() > MAX_EVENTS
                || fonts.len() > MAX_FONTS
            {
                return Err("This styled subtitle track is invalid or too large");
            }
            let mut bytes = header.len();
            for event in events.iter() {
                if event.payload.is_empty()
                    || event.payload.len() > MAX_EVENT_BYTES
                    || event.payload.contains(&0)
                    || event.duration_ms <= 0
                    || event.start_ms.checked_add(event.duration_ms).is_none()
                {
                    return Err("This styled subtitle track contains invalid events");
                }
                bytes += event.payload.len();
                if bytes > MAX_SCRIPT_BYTES {
                    return Err("This styled subtitle track is too large");
                }
            }
        }
    }
    let (Content::Embedded { fonts, .. } | Content::Script { fonts, .. }) = &source.content;
    if fonts.len() > MAX_FONTS {
        return Err("This styled subtitle track contains too many fonts");
    }
    let mut bytes = 0;
    for font in fonts.iter() {
        if font.name.is_empty()
            || font.name.len() > 1024
            || font.name.contains('\0')
            || font.data.is_empty()
            || font.data.len() > MAX_FONT_BYTES
        {
            return Err("This styled subtitle track contains invalid fonts");
        }
        bytes += font.data.len();
        if bytes > MAX_FONT_BYTES {
            return Err("This styled subtitle track contains too many fonts");
        }
    }
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

fn same_embedded_resources(old: &Source, new: &Source) -> bool {
    matches!((&old.content, &new.content), (
        Content::Embedded { header: oh, fonts: of, .. },
        Content::Embedded { header: nh, fonts: nf, .. }
    ) if old.id == new.id && oh == nh && of == nf)
}

#[derive(Default)]
struct Engine {
    source: Option<Arc<Source>>,
    native: Option<Native>,
    frame: Option<Arc<Frame>>,
    error: Option<&'static str>,
}

impl Engine {
    fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.native.is_none()
            && self.frame.is_none()
            && self.error.is_none()
    }

    fn render(&mut self, request: &Request) -> Arc<Frame> {
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
                    // as expiry + append. Reset events only, retaining parsed fonts.
                    let native = self.native.as_mut().unwrap();
                    unsafe { plx_ass_flush_events(native.0) };
                    native.append(&request.source, 0)
                } else {
                    self.native = None;
                    Native::open(&request.source).map(|native| self.native = Some(native))
                };
                self.error = result.err();
            }
            self.source = Some(request.source.clone());
            if let Some(error) = self.error {
                self.native = None;
                crate::log(&format!("subtitle: ASS refused: {error}"));
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
        let result = self.native.as_mut().expect("loaded ASS source").render(key);
        let frame = match result {
            Ok(None) => {
                if let Some(frame) = &self.frame {
                    return frame.clone();
                }
                error_frame(key, "Couldn't render this styled subtitle")
            }
            Ok(Some(rect)) => {
                if let Some(old) = &self.frame {
                    if old.source_id == key.source_id
                        && old.width == key.width
                        && old.height == key.height
                        && old.error.is_none()
                        && old.rect == rect
                    {
                        return old.clone();
                    }
                }
                Arc::new(Frame {
                    source_id: key.source_id,
                    serial: next_serial(),
                    width: key.width,
                    height: key.height,
                    rect,
                    error: None,
                })
            }
            Err(error) => {
                self.error = Some(error);
                crate::log(&format!("subtitle: ASS render failed: {error}"));
                error_frame(key, error)
            }
        };
        self.frame = Some(frame.clone());
        frame
    }
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct Bitmap {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    bytes: usize,
    rgba: *const u8,
}

crate::dynlib! {
    native_ass: ["libass-plx-host.so.0", "libass-plx.so.0", "libass-plx.0.dylib"] {
        fn plx_ass_abi_version() -> u32;
        fn plx_ass_create(default_font: *const c_char) -> *mut c_void;
        fn plx_ass_destroy(ctx: *mut c_void);
        fn plx_ass_add_font(ctx: *mut c_void, name: *const c_char, data: *const u8, len: usize) -> i32;
        fn plx_ass_load(ctx: *mut c_void, data: *const u8, len: usize, script: i32) -> i32;
        fn plx_ass_chunk(ctx: *mut c_void, data: *const u8, len: usize, start_ms: i64, duration_ms: i64) -> i32;
        fn plx_ass_prune_before(ctx: *mut c_void, deadline_ms: i64);
        fn plx_ass_flush_events(ctx: *mut c_void);
        fn plx_ass_render(ctx: *mut c_void, now_ms: i64, width: i32, height: i32, storage_width: i32, storage_height: i32, bitmap: *mut Bitmap) -> i32;
    }
}

struct Native(*mut c_void);

fn asset_dir() -> &'static std::path::Path {
    // Host unit tests execute in target/debug/deps; their bundled native fixture
    // and fonts live in pkg. This override cannot enter the television binary.
    #[cfg(test)]
    {
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../pkg"))
    }
    #[cfg(not(test))]
    {
        crate::paths::app_dir()
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        unsafe { plx_ass_destroy(self.0) };
    }
}

impl Native {
    fn open(source: &Source) -> Result<Self, &'static str> {
        static LOADED: OnceLock<bool> = OnceLock::new();
        let loaded = *LOADED.get_or_init(|| {
            native_ass::load(Some(asset_dir())).ok() && unsafe { plx_ass_abi_version() } == 1
        });
        if !loaded {
            return Err("Styled subtitle support is unavailable in this installation");
        }
        let fallback = asset_dir().join("appfont-cjk.ttf");
        let fallback = CString::new(fallback.to_string_lossy().as_bytes())
            .map_err(|_| "Couldn't load subtitle fonts")?;
        let ptr = unsafe { plx_ass_create(fallback.as_ptr()) };
        if ptr.is_null() {
            return Err("Couldn't initialize styled subtitles");
        }
        let mut native = Self(ptr);
        for name in ["appfont.ttf", "appfont-bold.ttf", "appfont-cjk.ttf"] {
            let bytes = std::fs::read(asset_dir().join(name))
                .map_err(|_| "Couldn't load subtitle fonts")?;
            if bytes.len() > MAX_FONT_BYTES {
                return Err("The installed subtitle font is too large");
            }
            native.font(name, &bytes)?;
        }
        let (Content::Embedded { fonts, .. } | Content::Script { fonts, .. }) = &source.content;
        for font in fonts.iter() {
            native.font(&font.name, &font.data)?;
        }
        let (data, script) = match &source.content {
            Content::Embedded { header, .. } => (header.as_ref(), 0),
            Content::Script { bytes, .. } => (bytes.as_ref(), 1),
        };
        if unsafe { plx_ass_load(native.0, data.as_ptr(), data.len(), script) } < 0 {
            return Err("This styled subtitle file could not be read");
        }
        native.append(source, 0)?;
        Ok(native)
    }

    fn font(&mut self, name: &str, bytes: &[u8]) -> Result<(), &'static str> {
        let name = CString::new(name).map_err(|_| "This subtitle font name is invalid")?;
        if unsafe { plx_ass_add_font(self.0, name.as_ptr(), bytes.as_ptr(), bytes.len()) } < 0 {
            return Err("Couldn't load this subtitle's fonts");
        }
        Ok(())
    }

    fn append(&mut self, source: &Source, from: usize) -> Result<(), &'static str> {
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
                    return Err("This styled subtitle track contains too many events");
                }
            }
        }
        Ok(())
    }

    fn render(&mut self, key: Key) -> Result<Option<Option<Rect>>, &'static str> {
        let mut bitmap = Bitmap::default();
        let result = unsafe {
            plx_ass_render(
                self.0,
                key.now_ms,
                key.width,
                key.height,
                key.storage_width,
                key.storage_height,
                &mut bitmap,
            )
        };
        if result < 0 {
            return Err("Couldn't render this styled subtitle");
        }
        if result == 0 {
            return Ok(None);
        }
        if bitmap.width == 0 && bitmap.height == 0 {
            return Ok(Some(None));
        }
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
            return Err("The styled subtitle renderer returned invalid pixels");
        }
        let rgba = unsafe { std::slice::from_raw_parts(bitmap.rgba, bitmap.bytes) };
        Ok(Some(Some(Rect {
            x: bitmap.x,
            y: bitmap.y,
            width: bitmap.width,
            height: bitmap.height,
            rgba: Arc::from(rgba),
        })))
    }
}

#[cfg(test)]
mod tests;
