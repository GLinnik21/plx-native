//! The video sink: the verbs the player speaks to the television's media pipeline. The webOS port
//! implements them over Starfish and ACB (the native `StarfishSink`); the simulator implements them over its
//! own clock (`plx_play_feed::ffi_host`).
use plx_base::task::MainThread;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_uint};
pub use super::plane::PlaneCompletion;

/// Which video-plane binding this television has. See `src/starfish.h`'s `VP_*` and the long
/// comment at the top of `src/video_plane.c`.
pub const VP_NONE: c_int = 0;
/// The legacy ACB plane; controlled URI experiments require this mode and a boot-created handle.
pub const VP_ACB: c_int = 1;
pub const VP_EXPORTED: c_int = 2;

/// **Exclusive ownership of the constructed native object for its `Unload`, movable off the main
/// thread.** `unload` is the one control verb that takes this instead of a `&MainThread`: on the dev
/// set it blocks for 110-150 ms (measured 2026-10-15, release build: a ~35 ms driver `ioctl` in D
/// state, then a ~70 ms wait for the pipeline thread), so a real stop issues it on a worker
/// (`player::engine`'s off-frame release) rather than on the frame that handled BACK.
///
/// Only the main thread can [`issue`](Self::issue) one, and it does so at the moment it gives the
/// object up: the Engine has been taken out of the adapter, every worker that touched the object has
/// been joined, the Rust callback epoch is closed to all but UNLOADCOMPLETED, and nothing else may
/// reach the seam until the release is finished (the adapter is that fence). The precedent is
/// `load`, which already runs on a worker with the object exclusively its own. Not `Clone`: one
/// ticket is one Unload.
#[derive(Debug)]
pub struct UnloadTicket(());

impl UnloadTicket {
    pub fn issue(_mt: &MainThread) -> Self {
        UnloadTicket(())
    }
}

/// The volume a freshly loaded pipeline starts at (libpf's `CustomPipeline::getVolume` defaults to
/// 100 when the property is absent): what the paused preroll restores after muting.
pub const PIPELINE_DEFAULT_VOLUME: c_int = 100;

/// What `setState(UNLOADED)` answered (`src/starfish.h`, `acb_unload`). `rv` is the library's own
/// return value: 1 accepted (a completion callback will carry `task`), -2 not registered (nothing
/// was sent, no callback comes), 0 no ACB handle or refused. `task` is the id the call wrote and
/// is meaningful ONLY when `rv == 1`; the seam writes it from a local, never from the slot the
/// other verbs share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneUnload {
    pub rv: c_int,
    pub task: c_long,
}

impl PlaneUnload {
    /// A sink with no plane to unload: nothing was sent, nothing will complete.
    pub const NOT_REGISTERED: Self = Self { rv: super::plane::UNLOAD_NOT_REGISTERED, task: 0 };
}

/// What the paced URI bind's last step answered (`src/starfish.h`, `acb_finish`): the full-screen
/// `setDisplayWindow` and `setState(PLAYING)`. `rv` is 1 only when BOTH calls were accepted, and
/// then each call's firmware task id is in `display_task` / `playing_task`; the firmware completes
/// each call with its own id, and `tv::plane::Plane` matches those two completions (its `Shown`).
/// The ids are meaningful ONLY when `rv == 1`: the seam copies each id out of the library's task
/// slot only for a call that answered 1, and writes 0 otherwise, as [`PlaneUnload::task`] is
/// meaningful only for an answered unload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneFinish {
    pub rv: c_int,
    pub display_task: c_long,
    pub playing_task: c_long,
}

impl PlaneFinish {
    /// A sink with no plane to show: nothing was sent, nothing will complete.
    pub const REFUSED: Self = Self { rv: 0, display_task: 0, playing_task: 0 };
    /// Both calls were accepted and carry the task ids to wait for.
    pub fn accepted(&self) -> bool { self.rv == 1 }
}

/// Shared display surface for URI playback and the buffer-feed player. Window and ACB verbs
/// retain their original main-thread token and native call order. Only the two memoized window
/// queries are token-free; neither mutates the pipeline.
///
/// # Safety
/// Pointer arguments must satisfy the C seam's NUL-termination and lifetime requirements.
/// Every mutating method requires `&MainThread` and is called in the player's bind order.
pub trait DisplaySink: Sync {
    fn window_mode(&self) -> c_int;
    /// The exported windowId the sink holds, or `""` when none was created. Never null. The
    /// storage is the sink's own long-lived buffer (the C seam's `g_window_id`), rewritten only by
    /// `create_window`/`destroy_window` on the main thread, so a caller copies it at once.
    fn window_id(&self) -> &'static CStr;
    /// # Safety
    /// Returns null when no window could be created (or the mode is not `VP_EXPORTED`); callers
    /// must check before reading the id. A non-null pointer is the sink's own buffer, valid until
    /// `destroy_window`.
    unsafe fn create_window(&self, mt: &MainThread) -> *const c_char;
    #[allow(clippy::too_many_arguments)]
    unsafe fn place_window(&self, mt: &MainThread, src_w: c_int, src_h: c_int, dst_x: c_int,
        dst_y: c_int, dst_w: c_int, dst_h: c_int) -> c_int;
    unsafe fn destroy_window(&self, mt: &MainThread);
    unsafe fn plane_create(&self, mt: &MainThread, app_id: *const c_char, player_type: c_int) -> c_long;
    /// Make `handle` (a value [`plane_create`](Self::plane_create) returned) the one every other
    /// `plane_*` verb addresses; true when the sink knows it. The boot creates up to two handles
    /// (`tv::plane::PlaneHandle`), and only the plane owner calls this, when it grants a bind: one
    /// binding at a time makes the current handle the bound one. A sink that holds one handle has
    /// nothing to select, and says so.
    unsafe fn plane_select(&self, _mt: &MainThread, _handle: c_long) -> bool { false }
    /// The library confirmed `AcbAPI_initialize` (its answer was exactly 1) for the CURRENT handle. A
    /// created handle alone is not that. Only meaningful at the boot, right after that handle's
    /// create: the boot reads it once per handle into the plane owner (`HandleSeat::initialized`),
    /// which is what the paced URI bind's gate and the product binding read afterwards.
    unsafe fn plane_initialized(&self, mt: &MainThread) -> bool;
    #[cfg(feature = "devtriggers")]
    unsafe fn plane_probe_dass_available(&self, mt: &MainThread) -> bool;
    /// `setSinkType(MAIN)` + `setMediaId`, as `plane_bind`'s first half: 1 only when both were
    /// accepted. The paced URI bind calls it once the service has loaded the media.
    unsafe fn plane_media_id(&self, mt: &MainThread, media_id: *const c_char) -> c_int;
    /// `setState(FOREGROUND, LOADED)`: `plane_bind`'s second half, `setState`'s own answer.
    unsafe fn plane_loaded(&self, mt: &MainThread) -> c_int;
    #[cfg(feature = "devtriggers")]
    unsafe fn plane_probe_connect_dass(&self, mt: &MainThread) -> c_int;
    /// The full-screen `setDisplayWindow` and `setState(PLAYING)` of the paced URI bind; `rv` is 1
    /// only when both were accepted (`plane_start`, Feed's, does not check), and then the answer
    /// carries both calls' task ids. Dispatch only: a call is complete when its completion arrives
    /// through [`plane_completions`](Self::plane_completions); route it through
    /// `tv::plane::PlaneOwner::show`, which is the only caller of both.
    unsafe fn plane_finish(&self, mt: &MainThread) -> PlaneFinish;
    unsafe fn plane_bind(&self, mt: &MainThread, media_id: *const c_char);
    unsafe fn plane_send_video_data(&self, mt: &MainThread, source_info: *const c_char) -> c_int;
    unsafe fn plane_send_atmos(&self, mt: &MainThread, media_id: *const c_char) -> c_int;
    unsafe fn plane_start(&self, mt: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long);
    /// `setState(UNLOADED)`: dispatch only. The release is complete when the completion carrying
    /// the returned task arrives through [`plane_completions`](Self::plane_completions); route it
    /// through `tv::plane::PlaneOwner`, which is the only caller of both.
    unsafe fn plane_unload(&self, mt: &MainThread) -> PlaneUnload;
    /// Drain the completion callbacks the firmware delivered on its own thread since the last
    /// call, oldest first, with a `Dropped` marker if the mailbox overflowed. One consumer only:
    /// `PlaneOwner::pump`.
    fn plane_completions(&self, mt: &MainThread) -> Vec<PlaneCompletion>;
    unsafe fn plane_pause(&self, mt: &MainThread);
    unsafe fn plane_resume(&self, mt: &MainThread);
}

/// Buffer-feed controls over the Starfish seam (`src/starfish.h`). Window and ACB
/// verbs are shared with URI playback through [`DisplaySink`].
///
/// Same arguments and C return values as the seam; `&MainThread` on every method but `load` (the
/// media worker calls it) and `unload` (an [`UnloadTicket`], so a worker may issue it).
///
/// # Safety
/// Every `unsafe fn` here forwards to a C verb that wants the arguments `src/starfish.h` documents:
/// a payload or id pointer must be a valid NUL-terminated string for the call, and `feed`'s
/// `p`/`size` a readable buffer. Apart from `load` and `unload`, the verbs must be called on the
/// main thread (which `&MainThread` witnesses), in the order the player's pump drives them.
pub trait VideoSink: DisplaySink {
    /// Positive proof that a joined Load never left a native object behind. A zero `ready()`
    /// answer is insufficient: firmware also reports zero after quarantining a live object.
    /// The default keeps real firmware conservative; a sink with its own object ledger may
    /// provide this only after the caller has joined the Load worker for this attempt.
    fn no_object_proof(&self, _mt: &MainThread) -> bool { false }
    /// # Safety
    /// Unlike every other verb, `load` runs off the main thread (the media worker calls it, after
    /// `create_window` has put the windowId into `payload`), so an implementation must not touch
    /// main-thread state. `payload` must stay valid for the call.
    unsafe fn load(&self, payload: *const c_char, epoch: u32) -> c_int;
    unsafe fn ready(&self, mt: &MainThread) -> c_int;
    unsafe fn is_load_completed(&self, mt: &MainThread) -> c_int;
    unsafe fn play(&self, mt: &MainThread) -> c_int;
    unsafe fn pause(&self, mt: &MainThread) -> c_int;
    /// This pipeline's output volume, 0..100 (`StarfishMediaAPIs::setVolume`), applied at once. The
    /// paused-preroll mute; the library's bool proves nothing. Per pipeline, so a fresh `load`
    /// starts at [`PIPELINE_DEFAULT_VOLUME`] again. There is no getter: `getVolume` returns an int
    /// on 4.5 against the NDK header's `const char *` (see `src/starfish.c`), so it is not bound.
    unsafe fn set_volume(&self, mt: &MainThread, volume: c_int) -> c_int;
    unsafe fn flush(&self, mt: &MainThread) -> c_int;
    unsafe fn push_eos(&self, mt: &MainThread) -> c_int;
    unsafe fn set_time_to_decode(&self, mt: &MainThread, position_ns: i64) -> c_int;
    unsafe fn set_content_info(&self, mt: &MainThread, position_ns: i64) -> c_int;
    unsafe fn send_segment(&self, mt: &MainThread) -> c_int;
    /// LG's public `StarfishMediaAPIs::Seek(millis)` (issue #568): a flushing seek whose new segment
    /// starts at `ms`, so the PTS fed afterwards must be on that same timeline. It flushes itself,
    /// so the caller must not `flush` around it (a `flush` is a seek to 0), and it must not race a
    /// `feed`. `ms` is never negative. Returns 0 when refused, which also means `feed` may now be
    /// rejected until a seek or flush succeeds.
    unsafe fn seek(&self, mt: &MainThread, ms: i64) -> c_int;
    /// True when the library's `Feed` is the pre-C++11 copy-on-write-string build (webOS 3.x).
    /// A memoized read, so it takes no `MainThread`.
    fn feed_abi_is_cow(&self) -> bool;
    unsafe fn feed(&self, mt: &MainThread, p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
    /// # Safety
    /// May run off the main thread, on the strength of `owner`: the caller holds the only route to
    /// the object until the release that issued the ticket has finished (see [`UnloadTicket`]).
    unsafe fn unload(&self, owner: &UnloadTicket);
    unsafe fn callback_gate_retire(&self, mt: &MainThread) -> c_int;
    unsafe fn callback_intercepts(&self, mt: &MainThread) -> u32;
    unsafe fn destroy(&self, mt: &MainThread) -> c_int;
    unsafe fn quarantine(&self, mt: &MainThread);
}

/// The installed sink: the only way out of `tv` for the Starfish verbs, and only `player/` may
/// call it (`ci/check-deps.sh`, rule `sink`).
#[cfg_attr(all(any(test, feature = "test-support"), feature = "hostsim"), allow(dead_code))]
pub fn installed() -> &'static dyn VideoSink { super::port().sink }

/// Resolve the shared display interface in composition/IO code, then inject it into a driver.
pub fn display_installed() -> &'static dyn DisplaySink { super::port().display_sink }

/// Refused native verbs, shared by `NoSink` and `RefusingSink`. Memoized queries
/// remain each sink's own.
macro_rules! refuse_video_verbs {
    () => {
        unsafe fn load(&self, _payload: *const c_char, _epoch: u32) -> c_int { 0 }
        unsafe fn ready(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn is_load_completed(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn play(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn pause(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn set_volume(&self, _mt: &MainThread, _volume: c_int) -> c_int { 0 }
        unsafe fn flush(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn push_eos(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn set_time_to_decode(&self, _mt: &MainThread, _position_ns: i64) -> c_int { 0 }
        unsafe fn set_content_info(&self, _mt: &MainThread, _position_ns: i64) -> c_int { 0 }
        unsafe fn send_segment(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn seek(&self, _mt: &MainThread, _ms: i64) -> c_int { 0 }
        unsafe fn feed(&self, _mt: &MainThread, _p: *const u8, _size: c_uint, _pts: i64, _es_data: c_int) -> c_char {
            b'e' as c_char
        }
        unsafe fn unload(&self, _owner: &UnloadTicket) {}
        unsafe fn callback_gate_retire(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn callback_intercepts(&self, _mt: &MainThread) -> u32 { 0 }
        unsafe fn destroy(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn quarantine(&self, _mt: &MainThread) {}
    };
}

macro_rules! refuse_window_verbs {
    () => {
        unsafe fn create_window(&self, _mt: &MainThread) -> *const c_char { std::ptr::null() }
        #[allow(clippy::too_many_arguments)]
        unsafe fn place_window(&self, _mt: &MainThread, _src_w: c_int, _src_h: c_int, _dst_x: c_int,
            _dst_y: c_int, _dst_w: c_int, _dst_h: c_int) -> c_int { 0 }
        unsafe fn destroy_window(&self, _mt: &MainThread) {}
        unsafe fn plane_create(&self, _mt: &MainThread, _app_id: *const c_char, _player_type: c_int) -> c_long { 0 }
    };
}

/// The developer-only `connectDass` experiment verbs, refused.
macro_rules! refuse_dev_plane_verbs {
    () => {
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_dass_available(&self, _mt: &MainThread) -> bool { false }
        #[cfg(feature = "devtriggers")]
        unsafe fn plane_probe_connect_dass(&self, _mt: &MainThread) -> c_int { 0 }
    };
}

macro_rules! refuse_plane_verbs {
    () => {
        unsafe fn plane_initialized(&self, _mt: &MainThread) -> bool { false }
        unsafe fn plane_media_id(&self, _mt: &MainThread, _media_id: *const c_char) -> c_int { 0 }
        unsafe fn plane_loaded(&self, _mt: &MainThread) -> c_int { 0 }
        unsafe fn plane_finish(&self, _mt: &MainThread) -> PlaneFinish { PlaneFinish::REFUSED }
        unsafe fn plane_bind(&self, _mt: &MainThread, _media_id: *const c_char) {}
        unsafe fn plane_send_video_data(&self, _mt: &MainThread, _source_info: *const c_char) -> c_int { -1 }
        unsafe fn plane_send_atmos(&self, _mt: &MainThread, _media_id: *const c_char) -> c_int { 0 }
        unsafe fn plane_start(&self, _mt: &MainThread, _x: c_long, _y: c_long, _w: c_long, _h: c_long) {}
        unsafe fn plane_pause(&self, _mt: &MainThread) {}
        unsafe fn plane_resume(&self, _mt: &MainThread) {}
    };
}

macro_rules! refuse_display_verbs {
    () => {
        refuse_window_verbs!();
        refuse_dev_plane_verbs!();
        refuse_plane_verbs!();
    };
}

/// The two plane verbs a refusing sink answers with nothing to unload and nothing completed.
/// Separate from [`refuse_display_verbs`] so a test double can script just these.
macro_rules! refuse_plane_unload {
    () => {
        unsafe fn plane_unload(&self, _mt: &MainThread) -> PlaneUnload { PlaneUnload::NOT_REGISTERED }
        fn plane_completions(&self, _mt: &MainThread) -> Vec<PlaneCompletion> { Vec::new() }
    };
}

/// No sink: exactly `ffi_host.rs`'s disabled answers.
pub struct NoSink;

impl DisplaySink for NoSink {
    refuse_display_verbs!();
    refuse_plane_unload!();
    fn window_mode(&self) -> c_int { VP_NONE }
    fn window_id(&self) -> &'static CStr { c"" }
}

impl VideoSink for NoSink {
    refuse_video_verbs!();
    fn feed_abi_is_cow(&self) -> bool { false }
}

/// A fence in front of a real sink (read through the getter it holds, so a static can name it): every verb is refused (NoSink's answers, nothing reaches the
/// wrapped sink) while the three memoized queries, which say nothing about the native object,
/// keep the wrapped sink's real answer. the Feed's `SinkFence` hands this out while an off-frame
/// `Unload` owns the object, so the main thread cannot reach what the worker is inside.
pub struct RefusingSink(pub fn() -> &'static dyn VideoSink);

impl DisplaySink for RefusingSink {
    refuse_display_verbs!();
    refuse_plane_unload!();
    fn window_mode(&self) -> c_int { (self.0)().window_mode() }
    fn window_id(&self) -> &'static CStr { (self.0)().window_id() }
}

impl VideoSink for RefusingSink {
    refuse_video_verbs!();
    fn feed_abi_is_cow(&self) -> bool { (self.0)().feed_abi_is_cow() }
}

/// The task ids [`ScriptedPlaneSink`] answers `plane_finish` with: the device log's own (LOADED,
/// video data, window and PLAYING are tasks 1 to 4 on a fresh handle, `docs/measurements/
/// acb-release-fence-2026-11-04.md`), so a test completes them with `mailbox.push(task, 1)`.
#[cfg(any(test, feature = "test-support"))]
pub const SCRIPTED_DISPLAY_TASK: c_long = 3;
#[cfg(any(test, feature = "test-support"))]
pub const SCRIPTED_PLAYING_TASK: c_long = 4;

/// A display sink for tests of the plane owner and of the backends that use it. Every verb is
/// refused as `NoSink` does except the plane unload (whose answer a test scripts), the completion
/// mailbox (which a test, or the unload itself with `complete_at_once`, fills), and the ACB bind
/// verbs, which answer success unless a test lists them in `failing` and record each call in
/// `calls` (`plane_send_video_data` records the length of its payload, never its content).
#[cfg(any(test, feature = "test-support"))]
pub struct ScriptedPlaneSink {
    pub unload_answer: std::sync::Mutex<PlaneUnload>,
    /// When set, `plane_unload` queues `(task, 1)` as the firmware would, before it returns.
    pub complete_at_once: std::sync::atomic::AtomicBool,
    pub mailbox: super::plane::CompletionQueue,
    pub unloads: std::sync::atomic::AtomicUsize,
    /// What `plane_initialized` answers.
    pub initialized: std::sync::atomic::AtomicBool,
    /// Verb names (`"plane_media_id"`, ...) that answer failure.
    pub failing: std::sync::Mutex<Vec<&'static str>>,
    /// Every ACB verb called, in order.
    pub calls: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedPlaneSink {
    pub fn new(unload_answer: PlaneUnload) -> Self {
        Self {
            unload_answer: std::sync::Mutex::new(unload_answer),
            complete_at_once: std::sync::atomic::AtomicBool::new(false),
            mailbox: super::plane::CompletionQueue::new(8),
            unloads: std::sync::atomic::AtomicUsize::new(0),
            initialized: std::sync::atomic::AtomicBool::new(true),
            failing: std::sync::Mutex::new(Vec::new()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }
    pub fn unload_count(&self) -> usize { self.unloads.load(std::sync::atomic::Ordering::Relaxed) }
    /// The ACB verbs called so far, in order.
    pub fn calls(&self) -> Vec<String> { self.calls.lock().unwrap().clone() }
    /// Record `verb` and answer 1 (accepted) unless it is listed in `failing`.
    fn answer(&self, verb: &'static str, detail: String) -> c_int {
        self.calls.lock().unwrap().push(if detail.is_empty() { verb.to_owned() } else { format!("{verb} {detail}") });
        c_int::from(!self.failing.lock().unwrap().contains(&verb))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DisplaySink for ScriptedPlaneSink {
    refuse_window_verbs!();
    refuse_dev_plane_verbs!();
    fn window_mode(&self) -> c_int { VP_ACB }
    fn window_id(&self) -> &'static CStr { c"" }
    unsafe fn plane_initialized(&self, _mt: &MainThread) -> bool {
        self.initialized.load(std::sync::atomic::Ordering::Relaxed)
    }
    unsafe fn plane_select(&self, _mt: &MainThread, handle: c_long) -> bool {
        self.answer("plane_select", handle.to_string()) == 1
    }
    unsafe fn plane_media_id(&self, _mt: &MainThread, media_id: *const c_char) -> c_int {
        self.answer("plane_media_id", unsafe { CStr::from_ptr(media_id) }.to_string_lossy().into_owned())
    }
    unsafe fn plane_loaded(&self, _mt: &MainThread) -> c_int { self.answer("plane_loaded", String::new()) }
    unsafe fn plane_finish(&self, _mt: &MainThread) -> PlaneFinish {
        if self.answer("plane_finish", String::new()) == 1 {
            PlaneFinish { rv: 1, display_task: SCRIPTED_DISPLAY_TASK, playing_task: SCRIPTED_PLAYING_TASK }
        } else {
            PlaneFinish::REFUSED
        }
    }
    unsafe fn plane_bind(&self, _mt: &MainThread, _media_id: *const c_char) {
        self.answer("plane_bind", String::new());
    }
    unsafe fn plane_send_video_data(&self, _mt: &MainThread, source_info: *const c_char) -> c_int {
        self.answer("plane_send_video_data", format!("len={}", unsafe { CStr::from_ptr(source_info) }.to_bytes().len()))
    }
    unsafe fn plane_send_atmos(&self, _mt: &MainThread, _media_id: *const c_char) -> c_int { 0 }
    unsafe fn plane_start(&self, _mt: &MainThread, _x: c_long, _y: c_long, _w: c_long, _h: c_long) {}
    unsafe fn plane_pause(&self, _mt: &MainThread) { self.answer("plane_pause", String::new()); }
    unsafe fn plane_resume(&self, _mt: &MainThread) { self.answer("plane_resume", String::new()); }
    unsafe fn plane_unload(&self, _mt: &MainThread) -> PlaneUnload {
        self.unloads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.calls.lock().unwrap().push("plane_unload".to_owned());
        let answer = *self.unload_answer.lock().unwrap();
        if answer.rv == 1 && self.complete_at_once.load(std::sync::atomic::Ordering::Relaxed) {
            self.mailbox.push(i64::from(answer.task), 1);
        }
        answer
    }
    fn plane_completions(&self, _mt: &MainThread) -> Vec<PlaneCompletion> { self.mailbox.drain() }
}

/// One exported-window verb [`ScriptedWindowSink`] was asked for, in the order it was asked.
/// `Place` carries the source size and the whole destination rectangle `(x, y, w, h)`.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCall {
    Create,
    Place { src: (c_int, c_int), dst: (c_int, c_int, c_int, c_int) },
    Destroy,
}

/// A display sink in `VP_EXPORTED` mode for tests of a backend that owns an exported window (webOS
/// 5+): it hands out one window id, answers every placement with `place_rv`, and records each
/// window verb in `calls`. Every other verb is refused as `NoSink` does.
#[cfg(any(test, feature = "test-support"))]
pub struct ScriptedWindowSink {
    /// What `place_window` answers.
    pub place_rv: c_int,
    /// Every window verb called, in order.
    pub calls: std::sync::Mutex<Vec<WindowCall>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedWindowSink {
    pub fn new(place_rv: c_int) -> Self { Self { place_rv, calls: std::sync::Mutex::new(Vec::new()) } }
    /// The window verbs called so far, in order.
    pub fn calls(&self) -> Vec<WindowCall> { self.calls.lock().unwrap().clone() }
    /// How many times a window was destroyed.
    pub fn destroys(&self) -> usize { self.calls().iter().filter(|call| **call == WindowCall::Destroy).count() }
}

#[cfg(any(test, feature = "test-support"))]
impl DisplaySink for ScriptedWindowSink {
    refuse_dev_plane_verbs!();
    refuse_plane_verbs!();
    refuse_plane_unload!();
    fn window_mode(&self) -> c_int { VP_EXPORTED }
    fn window_id(&self) -> &'static CStr { c"scripted-window" }
    unsafe fn create_window(&self, _mt: &MainThread) -> *const c_char {
        self.calls.lock().unwrap().push(WindowCall::Create);
        c"scripted-window".as_ptr()
    }
    unsafe fn place_window(&self, _mt: &MainThread, src_w: c_int, src_h: c_int, dst_x: c_int,
        dst_y: c_int, dst_w: c_int, dst_h: c_int) -> c_int {
        self.calls.lock().unwrap().push(WindowCall::Place {
            src: (src_w, src_h), dst: (dst_x, dst_y, dst_w, dst_h) });
        self.place_rv
    }
    unsafe fn destroy_window(&self, _mt: &MainThread) { self.calls.lock().unwrap().push(WindowCall::Destroy); }
    unsafe fn plane_create(&self, _mt: &MainThread, _app_id: *const c_char, _player_type: c_int) -> c_long { 0 }
}
