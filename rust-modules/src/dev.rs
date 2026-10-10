//! The `/tmp` developer-trigger surface, behind one door.
//!
//! This app is driven headlessly by ~44 files under `/tmp/plxnative-*`: which screen to boot to,
//! which item to play, which URL to stream, whether to auto-press OK, which PMS token to use.
//! That is how `tests/run.py` and every capture scene work, and it is not going away.
//!
//! It must not exist in a public build. `/tmp` is the SHARED system `/tmp` in the production jail
//! too (mode 1777, both jail profiles), so on an ordinary user's TV every one of those files is a
//! behaviour switch any co-resident process can throw. Three are outright takeovers:
//! `plxnative-token` beats the signed-in session (`app.rs`'s boot gate), `plxnative-servers` hands
//! the app a whole additional server — an address AND the token to trust it with (see [`servers`])
//! — and `plxnative-url` replaces the stream the player feeds.
//!
//! So every read goes through [`plx_base::devtrig`] — the primitives (`flag`, `read`, `latched_flag!`,
//! `read_sample`, `no_wan`, …), a base-layer module no application type can leak into — and that
//! door is `#[cfg]`-gated on the `devtriggers` feature. In a `--no-default-features` build
//! `devtrig::flag` is `false` and `devtrig::read` is `None` at COMPILE time, so no trigger can be
//! armed. Storage and diagnostics still use runtime files; none are developer triggers. THIS
//! module is the application-layer half: the typed triggers only app code consumes, the directory
//! scans ([`any_trigger_present`], [`armed_triggers`]) and the one-shot boot instruments.
//!
//! That does NOT keep a trigger's NAME out of the binary. A branch behind `flag`/`read` usually
//! folds away, but not reliably: once the answer is carried through a struct field (the
//! `nobudget` flag on `DevFlags`), the optimizer may keep the branch and its string literals
//! in a release build, and `ci/check-package.py` fails the package because it greps the shipped
//! bytes for every trigger name this module's [`DIAG`] and `devtrig`'s `CONTROLLED` list. So every
//! statement whose literal names a trigger (a log line saying `/tmp/plxnative-…`) carries its own
//! `#[cfg(feature = "devtriggers")]`. Never rely on constant folding for this.
//!
//! Two rules for anything added later:
//!
//! 1. **Never open a `/tmp` path directly.** Read through `plx_base::devtrig`. The grep that audits
//!    this (`/tmp/plxnative-` outside this module and the unconditional log sinks) is the only
//!    thing keeping the property true. The two profiler logs are dev-only and listed in [`DIAG`]
//!    below.
//! 2. **A gate is not always a path.** `any_trigger_present` scans the whole directory and names
//!    no file at all — it was the one surface a literal-replacement sweep would have missed, and
//!    it silently changes which screen the app boots to. Structural surfaces that take no name
//!    (the capture listener's `INADDR_ANY` socket, the remote FIFO's `mkfifo`) are gated at their
//!    call sites in `app.rs` for the same reason.
//!
//! The unconditional LOG sinks are deliberately NOT here and stay in every build: they are creates, not
//! reads, they are how on-device crash triage works at all, and writing them is not a way for
//! another process to steer this one.
//!
//! **Since UI restructure phase 10, [`scenarios`] is where a read gets ACTED on.** Both modules
//! reach `/tmp` only through the one door, [`plx_base::devtrig`] (`flag`/`read`); `dev::scenarios`
//! gathers every ARM — the app-core code that calls through this door and reacts — that used to be scattered
//! across `app/boot.rs`, `app/run.rs`, `app/content.rs` and `app/mod.rs`, plus the per-arm state
//! (oscillator phases, retry latches) those arms used to keep on `App` itself. Read that module's
//! doc before adding a new trigger that `app/` consumes.

pub(crate) mod scenarios;

/// The demo video's frame dump (simulator only): see the module doc.
#[cfg(all(feature = "hostsim", feature = "devtriggers"))]
pub(crate) mod framedump;

/// Files that are pure diagnostics rather than automation — see [`any_trigger_present`].
///
/// Every log this app writes belongs here, not just its trigger. `plxnative-anim` was listed and
/// `plxnative-anim.log` was not, while arming the overlay creates exactly that file and nothing
/// ever removes it (`make run` clears only the event log; `tests/run.py` spares every `*.log` by
/// design) — so a single historical anim session skipped the who's-watching picker on every later
/// boot, interactive ones included.
// `test` as well as the feature: `any_trigger_present` is the only caller and it is cfg'd out of a
// release build, but the test below asserts this list's contents and runs with default features.
#[cfg(any(feature = "devtriggers", test))]
const DIAG: [&str; 37] = [
    "plxnative-diag.log",
    "plxnative-events.log",
    "plxnative-stderr.log",
    "plxnative-crash.log",
    "plxnative-anim.log",
    "plxnative-profile",
    "plxnative-gputime.jsonl",
    "plxnative-hwcnt",
    "plxnative-hwcnt.jsonl",
    // The render thread's own per-phase clock ([`plx_ui::profile`]'s CPU mode). DIAG for the
    // reason the two GPU profilers are: it observes a screen, and must not move the boot away
    // from the screen it was armed to observe.
    "plxnative-cpuprof",
    "plxnative-anim",
    "plxnative-remote",
    "plxnative-capture",
    "plxnative-noidle",
    // The focus fingerprint ([`crate::focusprobe`]). Diagnostic for `noidle`'s reason and one of
    // its own: it only READS focus and writes a log line, and a (route × key) characterization
    // harness has to be able to observe the who's-watching picker, which a non-DIAG trigger would
    // suppress — the observer would remove the screen it was armed to watch.
    "plxnative-focus",
    // The two OVERDRAW surfaces and the hero-ground fold ([`plx_gfx::overdraw`],
    // `docs/backdrop-blur-profiling.md` Part 5). All three are measurement knobs whose whole
    // method is an A/B against an unmasked control leg — and a non-DIAG trigger suppresses the
    // who's-watching picker, so the control leg and the masked leg would boot to DIFFERENT
    // SCREENS and the difference between them would be the screen, not the class being priced.
    // That is the exact failure this list exists to stop, and it is invisible in the numbers.
    "plxnative-overdraw",
    "plxnative-drawmask",
    "plxnative-heroground",
    // The FRAME BUDGET's A/B control leg (`ui/frame/budget.rs`, spec §8.1): admission as it was
    // before phase 11 — quota only, no time ceiling, no solo rule. DIAG for exactly the argument
    // the three above make: its whole method is an A/B against an unmasked control leg, and a
    // non-DIAG trigger would boot the two legs to DIFFERENT SCREENS, so what the numbers measured
    // would be the screen and not the admission rule.
    "plxnative-nobudget",
    // LG's own GStreamer logging ([`arm_gst_logging`]) and the file it writes. Both are DIAG for
    // the same reason `plxnative-profile` is: the whole point is to observe a playback that would
    // otherwise be unobservable, and a non-DIAG trigger would silently move the boot screen out
    // from under the very session being measured.
    "plxnative-gstlog",
    "plxnative-gst.log",
    // The forced Stats-for-nerds overlay. It only changes presentation, and grading a playback
    // case without the read-out risks a failure whose evidence was never put on screen.
    "plxnative-stats",
    // A deliberate crash happens before the picker could ever be reached, so whether it suppresses
    // one is moot — but leaving it out of this list would be a silent inconsistency for the next
    // reader, and the honest reading is that it changes no screen.
    "plxnative-crashtest",
    // The FRAME-DROP DETECTOR (`app.rs`, the `FRAMEDROP` line and `worstframe=`). It observes the
    // frame it is armed on and changes no screen; the harness's `worst_ceiling_ms` /
    // `stall_ceiling_ms` gates arm it under every fps scene, and a scene whose gate moved the boot
    // away from the screen it grades would fail as "never entered this screen".
    "plxnative-framedrop",
    // The compositor frame-callback probe (`system.rs`): extra fields on that same line, from one
    // `wl_surface.frame` request per present. An observer for the reason `framedrop` is.
    "plxnative-framecb",
    // The detector's context ring: which of the same lines are written, never which screen they
    // are written about.
    "plxnative-framering",
    // libwayland's own protocol log ([`arm_wayland_debug`]): an observer of the present path.
    "plxnative-wldebug",
    // The poster pipeline's observers: the cache counters and the per-image timeline
    // (`app/adapters/poster/trace.rs`). Both only READ the store and write log lines, and the boot
    // they exist to trace is the owner's everyday one — who's-watching picker, then Home. A
    // non-DIAG trigger suppresses that picker, so the trace would observe a different boot from
    // the one it was armed to explain.
    "plxnative-imagecache-stats",
    "plxnative-imgtrace",
    "plxnative-imagecache-bypass",
    // The deterministic RECORDER and its replay trigger (`ui/rec.rs`, NOT YET IN THE TREE — reserved
    // here first so the recorder cannot land as a non-DIAG trigger and move the boot screen out
    // from under the session it records; restructure spec §5.3). Both observe or reproduce a session and must not decide
    // which screen it starts on — a recording of the who's-watching picker has to be possible.
    // `recplay` is a NEW name: `plxnative-replay[=N]` is the EOS replay COUNTER, non-DIAG, and
    // stays exactly as it is.
    "plxnative-rec",
    "plxnative-recplay",
    "plxnative-guard", // diagnostic policy only; never changes the boot screen
    // Boot-latched Dolby Vision capability A/B. They alter only the platform answer used by the
    // route policy, so an experiment must not independently replace Home with the profile picker.
    "plxnative-dvcaps0",
    "plxnative-dvcaps1",
    // The AV1 framing A/B (`media::ff`'s `av1obu` tokens: notd, noseq, raw). It alters only
    // how an AV1 packet is framed before the decoder sees it, so an on-device comparison of the
    // variants must not move the boot screen out from under the playback being compared.
    "plxnative-av1obu",
    // The side subtitle reader (`media::player::subside`): content = the 0-based position of the
    // embedded subtitle stream to draw over a plain remux, read at each playback start.
    "plxnative-subside",
];

/// **Turn on the TELEVISION'S OWN GStreamer logging** — `/tmp/plxnative-gstlog`.
///
/// This is the only instrument that can see inside LG's Dolby Vision chain. That chain is
/// `dvbin` → `h265parse` → `dvsplitter` → {`lxvideodec`, `dvmdparse`} → `dualsequencer`, all of it
/// closed, and the app's own logs stop at `Feed()`. Decompilation established that
/// `mediapipeline::PlayerFactory::create()` calls `gst_debug_is_active()` and, when it is, honours
/// `GST_DEBUG_FILE_OVERWRITE` / `GST_DEBUG_FILE` by installing its own log function — so these four
/// variables are read by libpf itself and need no cooperation from us beyond setting them.
///
/// **Timing is the whole reason this is here and not later.** Neither `libpf` nor `libplayerAPIs`
/// imports `gst_init`; they use LG's lazy `gst_cool_init_check`, which does not run until a player
/// is created. `plex_run` is therefore comfortably early — but anything that arms this AFTER the
/// first `Load` would be setting variables nobody reads again.
///
/// An empty trigger takes the five Dolby categories at level 6; content overrides the whole
/// `GST_DEBUG` spec, so `dvbin:9,dualsequencer:9` or `*:3` both work. The log goes to the runtime
/// directory beside the event log.
///
/// **Not free.** Level 6 on five categories is a lot of formatted I/O on an ARM TV and it is not a
/// setting to leave armed while measuring anything about frame pacing.
#[cfg(feature = "devtriggers")]
pub(crate) fn arm_gst_logging() {
    let Some(spec) = plx_base::devtrig::read("gstlog") else { return };
    let spec = if spec.is_empty() {
        "dvbin:6,dvsplitter:6,dvsplitter_algo:6,dvmdparse:6,dualsequencer:6".to_string()
    } else {
        spec
    };
    let log = plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::GST);
    // SAFETY: single-threaded here by construction — `plex_run` has not yet minted a worker, and
    // this runs before SDL init. `set_var` is only unsound against a concurrent reader.
    std::env::set_var("GST_DEBUG", &spec);
    std::env::set_var("GST_DEBUG_FILE", &log);
    std::env::set_var("GST_DEBUG_FILE_OVERWRITE", "enable");
    std::env::set_var("GST_DEBUG_NO_COLOR", "1");
    plx_base::eventlog::log(&format!("gstlog: GST_DEBUG={spec} -> {}", log.display()));
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn arm_gst_logging() {}

/// **Turn on libwayland-client's protocol log** — `/tmp/plxnative-wldebug`.
///
/// `WAYLAND_DEBUG` makes the client library print every request it sends and every event it
/// dispatches, each with a microsecond wall-clock stamp, to stderr (`plxnative-stderr.log`). It is
/// the one place the compositor's side of a present is visible from inside the app: when
/// `wl_buffer.release` and `wl_callback.done` actually arrive, against when the driver's
/// `attach`/`commit` went out. libwayland reads the variable in `wl_display_connect`, so this has
/// to run before SDL opens its video device. The stamps are `CLOCK_REALTIME`; the line logged
/// here carries the offset to `CLOCK_MONOTONIC`, which is what `FRAMEDROP`'s `mono=` is.
///
/// **Not free**: a dozen formatted lines a frame. Arm it for the leg that needs it, and read
/// pacing from another.
#[cfg(feature = "devtriggers")]
pub(crate) fn arm_wayland_debug() {
    if !plx_base::devtrig::flag("wldebug") {
        return;
    }
    // SAFETY (of the environment write): the caller (`app::pre_boot_diagnostics`) runs this before
    // `telemetry::boot`, whose `sentry_init` is the first step that starts threads (sentry-native's
    // "sentry-tele" pool; `lab::boot` starts none, its poll thread comes later from
    // `lab::start_control`). Checked against vendor/sentry-native-src/src/sentry_telemetry.c.
    // `arm_gst_logging`'s own write runs after it and is not changed here.
    std::env::set_var("WAYLAND_DEBUG", "client");
    let stamp = |clock| {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a plain clock read into a local.
        unsafe { libc::clock_gettime(clock, &mut ts) };
        ts.tv_sec as i64 * 1_000_000 + ts.tv_nsec as i64 / 1000
    };
    let (real, mono) = (stamp(libc::CLOCK_REALTIME), stamp(libc::CLOCK_MONOTONIC));
    plx_base::eventlog::log(&format!("wldebug: WAYLAND_DEBUG=client realtime_us={real} mono_us={mono} offset_us={}", real - mono));
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn arm_wayland_debug() {}

/// Parse `/tmp/plxnative-server=<slot>`, the optional server half of a direct-screen trigger.
///
/// A Plex `ratingKey` is only unique together with its server.  The original `plxnative-play`
/// trigger predated the multi-server registry and therefore meant "that key on the current
/// server".  Keeping the slot in a separate trigger preserves that wire format for the regression
/// harness while allowing `tv-session --server N` to name the other half explicitly.
///
/// Bounds are checked here rather than left to `ServerId::from_raw`: that constructor also exists
/// for compact stores and deliberately accepts any `u16`; a hand-written dev trigger must not turn
/// an arbitrary number into a plausible identity and then silently fall back elsewhere.
#[cfg(any(feature = "devtriggers", test))]
fn parse_server_slot(s: &str) -> Result<u16, String> {
    let slot = s
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("{s:?} is not a server slot"))?;
    if (slot as usize) >= plx_plex::plex::MAX_SERVERS {
        return Err(format!(
            "server slot {slot} is outside 0..{}",
            plx_plex::plex::MAX_SERVERS
        ));
    }
    Ok(slot)
}

/// The optional registry slot selected for direct screen automation.
///
/// `None` means the old/current-server behaviour.  `Some(Err)` is intentionally distinct: a bad
/// explicit identity must fail closed rather than play the same numeric rating key from whichever
/// server happens to be current.
#[cfg(feature = "devtriggers")]
pub(crate) fn server_slot() -> Option<Result<u16, String>> {
    plx_base::devtrig::read("server").map(|s| parse_server_slot(&s))
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn server_slot() -> Option<Result<u16, String>> {
    None
}

/// **Crash the app on purpose** — `plxnative-crashtest=<segv|abrt|bus|ill|trap|panic|unwind>`.
///
/// Not a feature. An INSTRUMENT for the instrument, and it exists because of the rule this repo
/// keeps re-learning: prove the instrument can see the thing before you read its silence, and
/// prove the recorder records before you trust an empty recording. The fallback C crash tracer is
/// always one witness; when crash consent and a DSN are present, Sentry Native's out-of-process
/// handler is a second witness that can safely inspect the stopped process. Until
/// 2026-08-29 nothing had ever exercised it deliberately — which is how it went seven weeks with a
/// re-raise that did not re-raise, silently costing every crash its `WIFSIGNALED` status — and only
/// that: no crashd backtrace was lost, because this firmware writes no core and so produces none.
/// The log looked perfectly normal either way, which is the whole reason it went seven weeks.
///
/// `segv` is a genuine null write, so the kernel raises the signal from a faulting instruction and
/// the record carries a real faulting PC and a real `si_addr`. The rest go through `raise`, which
/// proves five of the seven `sigaction` calls took but cannot produce a meaningful PC.
///
/// `panic` is a Rust panic inside an `extern "C"` CALLBACK — the shape of `ff::read_cb` under
/// libav: the hook writes its `*** RUST PANIC` line, the unwind stops at the callback's own
/// boundary, and the process aborts with `plex_run`'s frame intact. That leaves BOTH a panic
/// record and a native SIGABRT envelope, the pair `telemetry::crashreport` must send as the one
/// panic.
///
/// `unwind` panics straight in here instead, so the unwind crosses `plex_run`'s own frame before
/// aborting at its `extern "C"` boundary. That used to drop the telemetry `Guard` on the way past
/// and stop the native backend before the abort landed (dev set, 2026-09-19), so no envelope was
/// written. `Drop for Guard` (`telemetry::native`) now checks `std::thread::panicking()` and
/// returns early instead of tearing the backend down, so the backend stays armed through the
/// unwind and the boundary abort still produces a native SIGABRT envelope (PR #168). `panic`
/// remains the shape worth exercising for a real panic under libav; `unwind` is the regression
/// test for the `Guard` fix itself.
///
/// **Compiled out of a release build** with the rest of `devtriggers`, so a shipped binary has no
/// path to it at all. Called after telemetry boot (so native capture can be armed) but before SDL
/// or any screen is created, so it remains reachable when the fault being chased prevents UI boot.
#[cfg(feature = "devtriggers")]
pub(crate) fn crash_on_purpose() {
    let Some(kind) = plx_base::devtrig::read("crashtest") else {
        return;
    };
    if kind == "panic" {
        extern "C" fn callback() {
            panic!("crashtest: deliberate panic");
        }
        plx_base::eventlog::log("crashtest: DELIBERATE crash, kind=panic (aborts at an extern \"C\" callback)");
        callback();
        return;
    }
    if kind == "unwind" {
        plx_base::eventlog::log(
            "crashtest: DELIBERATE crash, kind=unwind (unwinds plex_run to its extern \"C\" boundary)",
        );
        panic!("crashtest: deliberate unwinding panic");
    }
    // The signal numbers are Linux's, written out rather than taken from a libc crate: this crate
    // binds no libc, and these five have been stable in the Linux ABI since it had one.
    let sig = match kind.as_str() {
        "" | "segv" => 11,
        "abrt" => 6,
        "bus" => 7,
        "ill" => 4,
        "trap" => 5,
        other => {
            plx_base::eventlog::log(&format!("crashtest: unknown kind {other:?} — not crashing"));
            return;
        }
    };
    // Logged BEFORE the fault, and flushed by `plx_base::eventlog::log`'s own O_APPEND write, so the event log
    // says the death was deliberate. Without this line a deliberate crash is indistinguishable
    // from the real one somebody is hunting.
    plx_base::eventlog::log(&format!(
        "crashtest: DELIBERATE crash, kind={kind} signal={sig}"
    ));
    if sig == 11 {
        // A real memory fault. `write_volatile` so the optimiser cannot decide a null write is
        // undefined and therefore removable — which it may, and then this proves nothing.
        unsafe { std::ptr::null_mut::<u8>().write_volatile(1) };
    }
    unsafe extern "C" {
        fn raise(sig: i32) -> i32;
    }
    unsafe { raise(sig) };
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn crash_on_purpose() {}

/// `plxnative-softfloat`: the ARM half of `plx_machine::motion`'s differential claim (spec §4.2). Runs the
/// 4,096-operand table through this binary's own arithmetic, logs its hash beside the host's
/// pinned one, and writes the words to `plxnative-softfloat.tbl` in the runtime root so a
/// divergence can be diffed word by word. `make softfloat-probe` is the recipe.
#[cfg(feature = "devtriggers")]
pub(crate) fn softfloat_probe() {
    if !plx_base::devtrig::flag("softfloat") {
        return;
    }
    let host = plx_machine::motion::DIFFERENTIAL_HASH_HOST;
    let here = plx_machine::motion::differential_hash();
    plx_base::eventlog::log(&format!(
        "softfloat: n={} hash={here:#018x} host={host:#018x} {}",
        plx_machine::motion::DIFFERENTIAL_N,
        if here == host { "MATCH" } else { "DIVERGE" }
    ));
    let mut t = Vec::new();
    plx_machine::motion::differential_table(&mut t);
    let body: String = t.iter().map(|w| format!("{w:08x}\n")).collect();
    let path = plx_base::paths::runtime_dir().join("plxnative-softfloat.tbl");
    if let Err(e) = std::fs::write(&path, body) {
        plx_base::eventlog::log(&format!("softfloat: table write failed: {e}"));
    }
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn softfloat_probe() {}

/// A test-only playback policy override from `plxnative-quality`.
///
/// The server matrix grades established direct-play/remux/transcode routes. It must not silently
/// become an Auto-HLS matrix because the television happened to persist that user preference in
/// an earlier run. This is deliberately an in-memory boot override: writing the session would
/// make a test change the owner's real preference. Unknown and empty values fail closed by
/// producing no override.
pub(crate) fn playback_quality_override() -> Option<plx_plex::plex::session::PlaybackQuality> {
    let value = plx_base::devtrig::read("quality")?;
    parse_playback_quality(&value)
}

/// The inverse of [`parse_playback_quality`], and it lives here so the two cannot drift.
///
/// The log line a test greps must carry the SAME string the trigger accepts. `Quality::label()`
/// is display text ("1080p \u{b7} 8 Mbps") and would make a case state its rung twice, in two
/// spellings, with nothing keeping them in step — which is the shape that rots.
pub(crate) fn quality_wire_name(q: plx_plex::plex::session::PlaybackQuality) -> &'static str {
    use plx_plex::plex::session::PlaybackQuality as Q;
    match q {
        Q::Auto => "auto",
        Q::Original => "original",
        Q::P1080High => "1080p_20_mbps",
        Q::P1080 => "1080p_8_mbps",
        Q::P720 => "720p_4_mbps",
        Q::P720Low => "720p_2_mbps",
        Q::P480 => "480p_720_kbps",
    }
}

fn parse_playback_quality(value: &str) -> Option<plx_plex::plex::session::PlaybackQuality> {
    use plx_plex::plex::session::PlaybackQuality;
    match value {
        "auto" => Some(PlaybackQuality::Auto),
        "original" => Some(PlaybackQuality::Original),
        "1080p_20_mbps" => Some(PlaybackQuality::P1080High),
        "1080p_8_mbps" => Some(PlaybackQuality::P1080),
        "720p_4_mbps" => Some(PlaybackQuality::P720),
        "720p_2_mbps" => Some(PlaybackQuality::P720Low),
        "480p_720_kbps" => Some(PlaybackQuality::P480),
        _ => None,
    }
}

/// **Switch the playback quality MID-PLAYBACK** — `plxnative-qualityswitch=[gap=<ms>,]<q>[,<q>…]`.
///
/// The one thing [`playback_quality_override`] above cannot do. That is a BOOT override: it decides
/// what the playback starts as and is read once. What a person actually does at the television is
/// start something, watch it, and then change the quality while it plays — which re-asks the
/// routing question against a stream already on screen, reloads if the answer moved, and (on the
/// way out of Auto) tears down a running ABR controller. None of that is reachable from a boot
/// value, and none of it was reachable from a test at all.
///
/// Same vocabulary as `plxnative-quality`, deliberately — one spelling of a rung across both
/// triggers, so a case cannot name a quality one of them accepts and the other silently ignores.
/// Same grammar as `plxnative-autoseek`: an optional leading `gap=<ms>` then comma-separated steps
/// fired one per gap. There is no default gap and none is invented: with a single step no cadence
/// exists to state, and a case wanting several states its own, in the manifest, where a reader can
/// see it beside the assertions it enables.
///
/// **This writes the stored preference, and that is not incidental.** `route::set_quality` persists
/// through `plex::session::update`, because a person picking a rung means it. A test therefore
/// leaves the value behind — on the `debug` flavour's own session, never the install you watch
/// with, and `tests/run.py` writes `plxnative-quality` on every server case, so the next boot
/// overrides whatever this left. Both halves are load-bearing; neither alone would make it safe.
fn parse_quality_switch_script(
    raw: &str,
) -> Option<(u32, Vec<plx_plex::plex::session::PlaybackQuality>)> {
    let mut steps: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    let mut gap_ms = None;
    if let Some(g) = steps.first().and_then(|f| f.strip_prefix("gap=")) {
        gap_ms = Some(g.parse().ok()?);
        steps.remove(0);
    }
    // A sequence needs an authored cadence. Without one, several valid rungs would all fire on
    // successive render loops: technically ordered, but not the mid-playback interactions the
    // trigger claims to reproduce.
    if steps.len() > 1 && gap_ms.is_none() {
        return None;
    }
    // Fail the WHOLE script when one rung is unparseable. Running a valid subset would still
    // change playback to a sequence nobody requested, merely without inventing a default rung.
    let qs: Vec<_> = steps
        .iter()
        .map(|t| parse_playback_quality(t))
        .collect::<Option<Vec<_>>>()?;
    if qs.is_empty() {
        None
    } else {
        Some((gap_ms.unwrap_or(0), qs))
    }
}

pub(crate) fn quality_switch_script() -> Option<(u32, Vec<plx_plex::plex::session::PlaybackQuality>)> {
    parse_quality_switch_script(&plx_base::devtrig::read("qualityswitch")?)
}

/// One synchronized user Pause, optionally followed by Resume —
/// `plxnative-autopause=[delay=<ms>,][at=<ms>,][hold=<ms>]`.
///
/// An empty file preserves the original paused-HUD capture contract: pause at the player's
/// ordinary six-second dev gate and stay paused. A non-empty script may delay that edge and name a
/// finite accepted hold. `at` also holds the edge until the PUBLISHED playhead has reached that
/// media position (and, in the simulator, stops the clock sink exactly on it, so the pause freezes
/// that position: `ffi_host.rs::stop_clock_at`): a wall-clock delay pauses wherever the host's
/// scheduling has got playback to, which moves from run to run, and the documentation's player
/// figure wants the same frame and the same clock every time. Unknown/duplicate/invalid fields fail the whole trigger closed; silently
/// substituting a duration would exercise a different interleaving from the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PauseScript {
    pub(crate) delay_ms: u32,
    pub(crate) hold_ms: Option<u32>,
    pub(crate) at_ms: Option<u32>,
}

fn parse_pause_script(raw: &str) -> Option<PauseScript> {
    if raw.trim().is_empty() {
        return Some(PauseScript {
            delay_ms: 0,
            hold_ms: None,
            at_ms: None,
        });
    }
    let mut delay_ms = None;
    let mut hold_ms = None;
    let mut at_ms = None;
    for field in raw
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
    {
        if let Some(value) = field.strip_prefix("delay=") {
            if delay_ms.is_some() {
                return None;
            }
            delay_ms = Some(value.parse().ok()?);
        } else if let Some(value) = field.strip_prefix("at=") {
            if at_ms.is_some() {
                return None;
            }
            at_ms = Some(value.parse().ok()?);
        } else if let Some(value) = field.strip_prefix("hold=") {
            if hold_ms.is_some() {
                return None;
            }
            let value: u32 = value.parse().ok()?;
            if value == 0 {
                return None;
            }
            hold_ms = Some(value);
        } else {
            return None;
        }
    }
    Some(PauseScript {
        delay_ms: delay_ms.unwrap_or(0),
        hold_ms,
        at_ms,
    })
}

pub(crate) fn pause_script() -> Option<PauseScript> {
    parse_pause_script(&plx_base::devtrig::read("autopause")?)
}

/// One ADDITIONAL server's credentials, injected for an automated run — see [`servers`].
///
/// Deliberately **not** `Debug`: `token` is a live per-(user,server) PMS access token, and a
/// derived `Debug` is exactly how a secret reaches a log by accident. [`DevServer::describe`] is
/// the only formatter this type has, and it prints everything *but* the token.
#[derive(serde::Deserialize, Clone)]
pub(crate) struct DevServer {
    /// Display name. Cosmetic — what a server picker would label it.
    #[serde(default)]
    pub(crate) name: String,
    /// The server's `machineIdentifier`: its IDENTITY, and the only thing that distinguishes it
    /// from the primary once both are installed. Public, not a secret.
    #[serde(default)]
    pub(crate) machine_id: String,
    /// Address reachable **from the TV**. `address` is accepted as an alias because that is what
    /// `plex::session::ServerRef` calls the same field, and copying one into the other by hand is
    /// the obvious way to write this file.
    #[serde(default, alias = "address")]
    pub(crate) host: String,
    #[serde(default = "default_port")]
    pub(crate) port: i64,
    /// `"http"` (the default) or `"https"` — the scheme this server is reached at.
    ///
    /// **This is how an https origin is exercised headlessly**, without a plex.tv account that has
    /// one and without a television that can reach it: write `"scheme": "https"` here and the
    /// whole control plane below `plex::register_origin` sees a TLS origin. Everything the origin
    /// model changed is otherwise invisible from outside the app, because every real origin today
    /// is `http://`.
    ///
    /// A value that is neither fails the WHOLE trigger to deserialize, which
    /// [`parse_servers`] turns into a logged error rather than a silently dropped server —
    /// deliberately: a typo'd scheme that quietly meant `http` would be a run grading the thing it
    /// was armed to test as working.
    #[serde(default)]
    pub(crate) scheme: plx_plex::plex::Scheme,
    /// Proven connection class for a conditioned test origin.
    ///
    /// A LAN proxy can stand in front of a remote PMS so the harness can shape the whole media
    /// link.  Its private address is not evidence that the PMS became local, while dropping the
    /// original `remote` classification disables the cold source probe the experiment exists to
    /// exercise.  Omitted stays `None`: naming an address is never enough to invent a tier.
    #[serde(default)]
    pub(crate) tier: Option<plx_plex::plex::probe::Location>,
    /// This identity's per-(user,server) access token **for this server**. A shared server is a
    /// separate authority: the account token gets a 401 from it, which is the whole reason one
    /// `plxnative-token` cannot express two servers. A SECRET — never logged.
    #[serde(default)]
    pub(crate) token: String,
    /// The owner's plex.tv handle (`sourceTitle` on the wire) — "friend". EMPTY means **your own
    /// server**, which is what the Sources list draws as the absence of an owner rather than as an
    /// anonymous one, so a harness overlay that omits it injects an owned server by definition.
    /// Public, like the machine name: it is the string every browsing surface says out loud.
    #[serde(default, alias = "sourceTitle", alias = "source_title")]
    pub(crate) handle: String,
    /// The literal to dial `host` at WITHOUT resolving it — the `Connection.address` plex.tv
    /// advertises beside a `plex.direct` `uri`, which is what a stored session persists. This is
    /// how a headless run puts a pinned TLS origin through the registry (`/tmp/plxnative-nowan`
    /// beside it is the offline reproduction). It is validated exactly as a session's address is
    /// ([`plx_plex::plex::ResolvePin::for_origin`]): a value the `host` label does not encode pins
    /// nothing. Omitted: no pin, unchanged behaviour for every overlay written before it existed.
    #[serde(default, alias = "address_pin", alias = "resolve")]
    pub(crate) pin: String,
}

fn default_port() -> i64 {
    32400
}

impl DevServer {
    /// Everything about this server except the token, for the event log.
    pub(crate) fn describe(&self) -> String {
        // A SHARED server is someone else's machine, and this line goes to
        // `/tmp/plxnative-events.log` — the file that gets pasted into issues and PR bodies. Four
        // PR bodies leaked exactly these fields on 2026-08-14 and had to be redacted after the
        // fact, which a public repository does not really allow. So a share names NOTHING that
        // identifies it or its owner: not the server's name, not the plex.tv handle, not the
        // address, not the machineIdentifier.
        //
        // The token was already excluded (`describe_never_carries_the_token`), but a token is not
        // the only thing here worth protecting — an address plus a handle is a person's home.
        //
        // What survives is what DEBUGGING actually needs: that a share is present at all, whether
        // its credentials are complete, and a stable `ref` so two lines about the same server can
        // be correlated within one log without identifying it outside one. An OWNED server is the
        // user's own machine, already in the boot line and in `config.local.h`, so it is unchanged.
        if !self.handle.is_empty() {
            return format!("SHARED ref={} port_set={}", self.reference(), self.port > 0);
        }
        // by CHARS, not bytes: a machineIdentifier is hex in practice, but a hand-written file is
        // whatever someone typed, and slicing a byte range mid-codepoint panics.
        let mut mid: String = self.machine_id.chars().take(8).collect();
        if self.machine_id.chars().nth(8).is_some() {
            mid.push_str("..");
        }
        // The origin's `log_form`, not `{host}:{port}`: this trigger's whole reason for having a
        // `scheme` field is to put a TLS origin through the registry headlessly, and a description
        // that cannot say which one it injected is the `[[silent-instrument-trap]]` again. It is
        // byte-identical for the plaintext servers every overlay writes today.
        let where_ = self
            .origin()
            .map(|o| o.log_form())
            .unwrap_or_else(|| format!("{}:{}", self.host, self.port));
        format!(
            "name={:?} handle={:?} {where_} mid={mid}",
            self.name, self.handle
        )
    }

    /// A short, stable, NON-reversible tag for a shared server — enough to tell two shares apart in
    /// one log and to follow one across a boot, and useless to anyone reading that log elsewhere.
    ///
    /// FNV-1a over the machineIdentifier: not a cryptographic choice, a legibility one. The id is
    /// the right input because it is the only field that survives the server changing address.
    fn reference(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.machine_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        format!("{:06x}", h & 0xff_ffff)
    }
    /// Are these credentials complete enough to reach the server at all?
    ///
    /// The port is judged by [`plx_plex::plex::probe::dial_port`], not by `> 0`: `app.rs` registers
    /// every server that passes this with `s.port as c_int`, and this file is a hand-written JSON
    /// blob under `/tmp` — an out-of-range `i64` wraps in that cast into a port nobody wrote down.
    pub(crate) fn usable(&self) -> bool {
        !self.token.is_empty() && self.origin().is_some()
    }

    /// **Where this server is** — the [`plx_plex::plex::Origin`] to register it at, `None` when the
    /// trigger did not write enough to dial.
    ///
    /// The port goes through `probe::dial_port` for the reason [`DevServer::usable`] gives: this
    /// is a hand-written JSON blob under `/tmp`, and `port as i32` wraps an out-of-range `i64`
    /// into a plausible-looking one.
    pub(crate) fn origin(&self) -> Option<plx_plex::plex::Origin> {
        if self.host.is_empty() {
            return None;
        }
        plx_plex::plex::probe::dial_port(self.port)
            .map(|p| plx_plex::plex::Origin::new(self.scheme, &self.host, p))
    }

    /// The resolve pin for [`DevServer::origin`], from the `pin` field — `None` when absent or
    /// when the label does not encode it.
    pub(crate) fn resolve_pin(&self) -> Option<plx_plex::plex::ResolvePin> {
        let origin = self.origin()?;
        plx_plex::plex::ResolvePin::for_origin(&origin, &self.pin)
    }
}

/// Parse the `servers` trigger's content: a JSON array of [`DevServer`], or a single bare object.
///
/// An `Err` rather than an empty list on malformed JSON, deliberately — a run that injected
/// credentials and got them silently dropped would grade as "the feature is broken", when the real
/// fault is a typo in the harness overlay. The caller logs the parse error.
#[cfg(any(feature = "devtriggers", test))]
fn parse_servers(s: &str) -> Result<Vec<DevServer>, String> {
    // Many FIRST: `untagged` tries the variants in order, and a derived struct deserializer also
    // accepts a SEQUENCE (positional fields), so with `One` first an empty `[]` came back as one
    // all-defaults server instead of no servers at all.
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ManyOrOne {
        Many(Vec<DevServer>),
        One(DevServer),
    }
    if s.trim().is_empty() {
        return Ok(Vec::new()); // an empty file means "no extra servers", not a syntax error
    }
    match serde_json::from_str::<ManyOrOne>(s) {
        Ok(ManyOrOne::Many(v)) => Ok(v),
        Ok(ManyOrOne::One(d)) => Ok(vec![d]),
        Err(e) => Err(e.to_string()),
    }
}

/// The EXTRA servers this boot was given credentials for — `/tmp/plxnative-servers`.
///
/// Purely ADDITIVE: the primary server is still the compiled-in host plus `plxnative-token` (or the
/// signed-in session), untouched, so a run that names one server behaves exactly as it always has.
/// This is the channel for the *second* authority — a friend's shared server, which has its own
/// `machineIdentifier` and its own access token and answers 401 to anybody else's.
///
/// Read and parsed **once**. `tests/run.py` wipes `/tmp/plxnative-*` between cases and again on
/// exit (pass, fail or Ctrl-C — that is how a live token stops surviving in world-readable `/tmp`),
/// so a second read could legitimately see the file gone. The credentials a boot was handed are a
/// property of that boot, not of what `/tmp` happens to hold when someone asks.
#[cfg(feature = "devtriggers")]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    static ONCE: std::sync::OnceLock<Result<Vec<DevServer>, String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| match plx_base::devtrig::read("servers") {
        Some(s) => parse_servers(&s),
        None => Ok(Vec::new()),
    })
    .clone()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    Ok(Vec::new())
}

/// Is ANY non-diagnostic trigger armed? Used to skip the boot who's-watching picker, so that a
/// headless run lands on a deterministic Home.
///
/// This is the surface with no path literal: it `read_dir`s the runtime root and matches by
/// prefix, so in a release build it would still have run — and still have changed the boot
/// screen from a squatted file — after every named read had been compiled out.
///
/// **A trigger is a FILE.** Nothing else here names a path, so nothing else could have caught a
/// non-file entry whose name happens to match: a directory called `plxnative-anything` sitting in
/// the runtime root would read as an armed trigger and permanently suppress the boot picker,
/// silently changing which screen this install comes up on with no line in any log. That became
/// reachable the moment two installs could share `/tmp` — the obvious name for a second install's
/// runtime root is exactly `plxnative-<flavour>`, which is why `paths::resolve_runtime_dir` spells
/// it with a DOT instead. Two independent reasons is the right number for a failure this quiet.
#[cfg(feature = "devtriggers")]
pub(crate) fn any_trigger_present() -> bool {
    std::fs::read_dir(plx_base::paths::runtime_dir())
        .ok()
        .map(|rd| rd.filter_map(|e| e.ok()).any(|e| is_armed_trigger(&e)))
        .unwrap_or(false)
}

/// Every `plxnative-*` FILE in the runtime root, by name (DIAG entries included), sorted — the
/// recorder's header lists them so a replay can say what the recording boot had armed. Names
/// only: a trigger's CONTENT can be a query or a path and never enters a recording.
#[cfg(feature = "devtriggers")]
pub(crate) fn armed_triggers() -> Vec<String> {
    armed_triggers_in(plx_base::paths::runtime_dir())
}
#[cfg(feature = "devtriggers")]
fn armed_triggers_in(root: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    // Runtime logs share the prefix and are not triggers
                    (n.starts_with("plxnative-") && !n.ends_with(".log")).then_some(n)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn armed_triggers() -> Vec<String> {
    Vec::new()
}

#[cfg(feature = "devtriggers")]
fn is_armed_trigger(entry: &std::fs::DirEntry) -> bool {
    if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
        return false;
    }
    let name = entry.file_name();
    let name = name.to_string_lossy();
    name.starts_with("plxnative-") && !DIAG.contains(&name.as_ref())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn any_trigger_present() -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn server_slot_trigger_accepts_only_a_registry_slot() {
        assert_eq!(super::parse_server_slot("0"), Ok(0));
        assert_eq!(super::parse_server_slot("15"), Ok(15));
        for invalid in ["", "-1", "16", "1.0", "server-1"] {
            assert!(
                super::parse_server_slot(invalid).is_err(),
                "{invalid:?} must not silently target another server"
            );
        }
    }

    #[test]
    fn quality_trigger_accepts_only_persisted_policy_spellings() {
        use plx_plex::plex::session::PlaybackQuality;

        assert_eq!(
            super::parse_playback_quality("auto"),
            Some(PlaybackQuality::Auto)
        );
        assert_eq!(
            super::parse_playback_quality("original"),
            Some(PlaybackQuality::Original)
        );
        assert_eq!(
            super::parse_playback_quality("720p_4_mbps"),
            Some(PlaybackQuality::P720)
        );
        for invalid in ["", "Auto", "720p", "unlimited"] {
            assert_eq!(super::parse_playback_quality(invalid), None, "{invalid}");
        }
    }

    /// The DIAG list must name every log the app writes, or that log permanently suppresses the
    /// boot picker. This asserts the property against the paths the code actually opens rather
    /// than against a copy of the list, so adding another log sink without listing it fails here.
    #[test]
    fn diag_names_every_log_this_app_writes() {
        for log in plx_base::paths::runtime_file::LOGS {
            assert!(super::DIAG.contains(&log), "{log} is written by this app but absent from DIAG — it would suppress the boot picker forever");
        }
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn storage_diagnostics_is_never_an_armed_trigger() {
        let _serial = plx_base::testlock::serial();
        let root = std::env::temp_dir().join(format!(".plx-diag-trigger-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        std::fs::write(root.join(plx_platform::storage::diagnostics::NAME), b"schema=1\n").unwrap();
        let entry = std::fs::read_dir(&root).unwrap().next().unwrap().unwrap();
        assert!(!super::is_armed_trigger(&entry));
        assert!(super::armed_triggers_in(&root).is_empty());
        std::fs::write(root.join("plxnative-home"), b"").unwrap();
        assert_eq!(super::armed_triggers_in(&root), vec!["plxnative-home"]);
    }

    /// The poster pipeline's two observers — the per-image timeline and the cache counters — must
    /// see the boot they were armed on. Armed as ordinary triggers they suppressed the
    /// who's-watching picker, so the picker → Home path (the owner's everyday boot) could not be
    /// traced at all.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_poster_observers_are_never_armed_triggers() {
        let _serial = plx_base::testlock::serial();
        let root = std::env::temp_dir().join(format!(".plx-poster-observers-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        for name in ["plxnative-imgtrace", "plxnative-imagecache-stats"] {
            std::fs::write(root.join(name), b"").unwrap();
        }
        // `is_armed_trigger` is the predicate `any_trigger_present` (the picker suppression) applies
        // to each runtime entry; `armed_triggers_in` lists DIAG files too, by design.
        let armed: Vec<String> = std::fs::read_dir(&root).unwrap().filter_map(|e| e.ok())
            .filter(super::is_armed_trigger)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(armed.is_empty(), "a poster observer counts as an armed trigger: {armed:?}");
    }

    /// A DIRECTORY whose name matches the trigger prefix must not read as an armed trigger.
    ///
    /// The failure it prevents is silent and permanent: `any_trigger_present` suppresses the boot
    /// who's-watching picker, so a squatted entry changes which screen this install comes up on
    /// with nothing logged anywhere. It became reachable when two installs started sharing `/tmp`
    /// — the second install's runtime root is a directory sitting right there.
    // `is_armed_trigger` itself is compiled out entirely under `--no-default-features` (it is
    // `devtriggers`-only, not merely dead code behind the runtime `ENABLED` check below), so this
    // test cannot exist in that build at all rather than just skip at runtime.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn a_directory_is_not_an_armed_trigger() {
        if !plx_base::devtrig::ENABLED {
            return; // a release build reads nothing
        }
        // Test the exact entry rather than scanning the whole host /tmp. Developers legitimately
        // keep captured TV artifacts there, and their names are intentionally outside DIAG.
        let _g = plx_base::testlock::serial();
        // Per PROCESS, not a fixed name: the runtime dir is the host's /tmp here, shared with every
        // other `cargo test` on this Mac, and two suites running at once (a second checkout's
        // `make check`) removed each other's entry between the create and the read_dir.
        let d = plx_base::paths::in_runtime_dir(&format!(
            "plxnative-notatrigger-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let entry = std::fs::read_dir(d.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.path() == d)
            .unwrap();
        let armed = super::is_armed_trigger(&entry);
        let _ = std::fs::remove_dir_all(&d);
        assert!(
            !armed,
            "a directory named {} read as an armed trigger",
            d.display()
        );
    }

    /// **How an https origin is exercised without a plex.tv account that has one.** The
    /// `plxnative-servers` trigger is the only surface that can inject a server the app did not
    /// discover, so it is also the only way any lane or the integrator can put a TLS origin
    /// through `plex::register_origin` headlessly.
    ///
    /// The default is `http`, because that is what every server this app has ever talked to is and
    /// what every overlay written before this field meant.
    #[test]
    fn a_dev_server_scheme_defaults_to_http_and_can_be_told_https() {
        let one = |json: &str| {
            super::parse_servers(json)
                .expect("parses")
                .pop()
                .expect("one server")
        };

        let plain = one(r#"{"machine_id":"m","host":"10.0.0.2","port":32400,"token":"t"}"#);
        assert_eq!(
            plain.scheme,
            plx_plex::plex::Scheme::Http,
            "an overlay that says nothing means http"
        );
        assert_eq!(
            plain.origin().expect("dialable").base(),
            "http://10.0.0.2:32400"
        );

        let tls = one(
            r#"{"machine_id":"m","host":"nas.hash.plex.direct","port":32400,"token":"t","scheme":"https"}"#,
        );
        assert!(tls.origin().expect("dialable").is_tls());
        assert_eq!(
            tls.origin().unwrap().base(),
            "https://nas.hash.plex.direct:32400"
        );
        assert!(tls.usable());

        // A scheme this app does not speak fails the WHOLE trigger, loudly — the caller logs the
        // parse error. Silently meaning `http` would be a run grading the very thing it was armed
        // to test as working.
        assert!(super::parse_servers(r#"{"host":"h","token":"t","scheme":"ftp"}"#).is_err());

        // and the port narrowing still applies: an out-of-range one costs the server, not the run
        let wrapped = one(r#"{"machine_id":"m","host":"10.0.0.2","port":4294999696,"token":"t"}"#);
        assert!(
            wrapped.origin().is_none() && !wrapped.usable(),
            "32400 is what that number wraps to"
        );
    }

    /// A conditioned WAN run terminates at a LAN proxy, so the endpoint's address cannot recover
    /// the server tier.  The harness must be able to preserve the discovery result explicitly,
    /// while old payloads remain unknown rather than silently becoming remote.
    #[test]
    fn a_dev_server_tier_is_explicit_and_defaults_to_unknown() {
        let one = |json: &str| {
            super::parse_servers(json)
                .expect("parses")
                .pop()
                .expect("one server")
        };
        assert_eq!(one(r#"{"host":"10.0.0.2","token":"t"}"#).tier, None);
        assert_eq!(
            one(r#"{"host":"10.0.0.2","token":"t","tier":"remote"}"#).tier,
            Some(plx_plex::plex::probe::Location::Remote)
        );
        assert!(
            super::parse_servers(r#"{"host":"10.0.0.2","token":"t","tier":"wan"}"#).is_err(),
            "an unknown spelling must not silently change the experiment"
        );
    }

    /// The wire format the harness writes: a JSON ARRAY of servers, and — because a human arming
    /// this by hand will write one server — a bare OBJECT too.
    #[test]
    fn servers_parse_array_and_single_object() {
        let arr = r#"[{"name":"Mine","machine_id":"aaaa1111bbbb","host":"10.0.0.2","port":32400,
                       "token":"t1"},
                      {"name":"Friend","machine_id":"cccc2222dddd","host":"10.0.0.9","port":32401,
                       "token":"t2"}]"#;
        let v = super::parse_servers(arr).expect("array must parse");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].host, "10.0.0.9");
        assert_eq!(v[1].port, 32401);
        assert!(v.iter().all(|s| s.usable()));

        let one = r#"{"name":"Friend","host":"10.0.0.9","token":"t2"}"#;
        let v = super::parse_servers(one).expect("a bare object must parse too");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].port, 32400, "port must default to the PMS port, not 0");
    }

    /// The harness's payload, verbatim — `tests/run.py::shared_servers_json` emits exactly this
    /// (compact, an array of one, these SIX keys). It is the only automated link between the two
    /// halves of the mechanism: rename a field on either side and this fails instead of a device
    /// run quietly booting with one server.
    ///
    /// **`handle` is the load-bearing one**, and it is the reason this assertion is worth more than
    /// it looks. `app.rs` derives `owned = handle.is_empty()` from it, which decides whether the
    /// injected server's libraries are pinned to Home and whether they get tab pills of their own
    /// (`browse::tabs`). Drop or rename it on the Python side and every injected server silently
    /// becomes one of YOURS — a friend's libraries pinned and pilled — with nothing else failing.
    #[test]
    fn servers_parse_the_harness_payload_verbatim() {
        let payload = concat!(
            r#"[{"name":"Bob's Plex","machine_id":"friend222","handle":"bob","host":"10.0.0.9","#,
            r#""port":32400,"token":"FRIENDTOK"}]"#
        );
        let v = super::parse_servers(payload).expect("the harness payload must parse");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "Bob's Plex");
        assert_eq!(v[0].machine_id, "friend222");
        assert_eq!(
            v[0].handle, "bob",
            "the owner's handle — an empty one means YOUR OWN server"
        );
        assert_eq!(v[0].host, "10.0.0.9");
        assert_eq!(v[0].port, 32400);
        assert!(v[0].usable());

        // …and the wire spelling plex.tv itself uses, so a hand-written file copied straight off a
        // /api/v2/resources row works too
        let wire = r#"[{"sourceTitle":"bob","host":"10.0.0.9","token":"t"}]"#;
        assert_eq!(super::parse_servers(wire).unwrap()[0].handle, "bob");
    }

    /// `address` is what `plex::session::ServerRef` calls the host field, so a file written by
    /// copying one is the likeliest hand-authored shape there is.
    #[test]
    fn servers_accept_address_as_host_alias() {
        let v = super::parse_servers(r#"[{"address":"10.0.0.9","token":"t"}]"#).unwrap();
        assert_eq!(v[0].host, "10.0.0.9");
        assert!(v[0].usable());
    }

    /// Malformed JSON must be an ERROR, never an empty list: a run that injected credentials and
    /// had them silently dropped looks like a broken feature instead of a typo'd overlay.
    #[test]
    fn servers_malformed_is_an_error_not_an_empty_list() {
        assert!(super::parse_servers("{not json").is_err());
        assert!(
            super::parse_servers("").unwrap().is_empty(),
            "an empty file = no extra servers"
        );
        assert!(super::parse_servers("[]").unwrap().is_empty());
        // …and the error text goes to the EVENT LOG, so it must not quote the input back. It is a
        // half-written credentials file: the byte after the truncation could be the token.
        // (matched rather than `unwrap_err`, which wants `T: Debug` — the absent derive this type
        // relies on for its no-secret-in-a-log property is load-bearing right here.)
        let e = match super::parse_servers(r#"[{"token":"SECRETTOKENVALUE""#) {
            Err(e) => e,
            Ok(_) => panic!("truncated JSON must not parse"),
        };
        assert!(
            !e.contains("SECRETTOKENVALUE"),
            "the parse error echoed the input: {e}"
        );
    }

    /// Half a credential is worse than none — it reaches the server and 401s. The boot log says so
    /// per entry rather than installing it.
    ///
    /// The last entry is the one that is not obviously broken: `app.rs` registers whatever passes
    /// this with `s.port as c_int`, and `4_294_999_696 as i32` is **32400**, so a hand-written
    /// number no port can be would have installed a server pointing at the most ordinary port
    /// there is. Judged by `plex::probe::dial_port`, it is refused like the rest.
    #[test]
    fn servers_incomplete_credentials_are_not_usable() {
        let v = super::parse_servers(
            r#"[{"host":"10.0.0.9"},{"token":"t"},{"host":"10.0.0.9","port":0,"token":"t"},
                {"host":"10.0.0.9","port":4294999696,"token":"t"}]"#,
        )
        .unwrap();
        assert!(
            v.iter().all(|s| !s.usable()),
            "no-token / no-host / no-port / a port that wraps"
        );
    }

    /// The one formatter this type has must not be a way for a token to reach the event log.
    #[test]
    fn describe_never_carries_the_token() {
        let v = super::parse_servers(
            r#"[{"name":"Friend","machine_id":"0123456789abcdef","host":"10.0.0.9",
                 "token":"SECRETTOKENVALUE"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        assert!(
            !d.contains("SECRETTOKENVALUE"),
            "describe() leaked the token: {d}"
        );
        assert!(d.contains("10.0.0.9:32400"), "{d}");
        assert!(
            d.contains("mid=01234567.."),
            "the machine id is truncated, not dropped: {d}"
        );
    }

    /// …and a SHARED server names nothing at all. The event log is pasted into public issues and PR
    /// bodies — it has already happened, to these exact fields — so a share's owner, machine and
    /// address must not be in it. The token was never the only thing worth protecting here.
    #[test]
    fn describe_redacts_everything_identifying_about_someone_elses_server() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"0123456789abcdef","host":"10.9.9.7",
                 "port":31234,"token":"SECRETTOKENVALUE","handle":"friend"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        for leak in [
            "SECRETTOKENVALUE",
            "Film Club",
            "friend",
            "10.9.9.7",
            "31234",
            "0123456789",
            "01234567",
        ] {
            assert!(!d.contains(leak), "describe() leaked {leak:?}: {d}");
        }
        assert!(d.contains("SHARED"), "a share still says it is one: {d}");
        assert!(
            d.contains("port_set=true"),
            "…and that its credentials look complete: {d}"
        );
    }

    /// The correlation tag is stable for one server and different for another — the whole point of
    /// keeping a tag rather than dropping the field. A reader can follow one share across a boot
    /// and tell two shares apart, and learn nothing about either.
    #[test]
    fn the_shared_reference_is_stable_per_machine_and_distinct_across_them() {
        let mk = |mid: &str| {
            super::parse_servers(&format!(
                r#"[{{"machine_id":"{mid}","host":"10.9.9.7","token":"t","handle":"friend"}}]"#
            ))
            .unwrap()[0]
                .describe()
        };
        assert_eq!(
            mk("aaaaaaaaaaaa"),
            mk("aaaaaaaaaaaa"),
            "same machine, same tag"
        );
        assert_ne!(
            mk("aaaaaaaaaaaa"),
            mk("bbbbbbbbbbbb"),
            "two shares must be tellable apart"
        );
    }

    /// The tag is computed in TWO languages: `tests/run.py`'s `server_ref`/`describe_server` print
    /// the same line for the same server, so a harness transcript and an event log can be read as
    /// one story — and so that the harness, which also prints to a pasteable stream, is held to
    /// this module's redaction contract rather than to its own.
    ///
    /// Nothing links the two implementations at build time, so the whole line is pinned to a
    /// literal here. **If this assertion has to change, `run.py`'s copy changes with it.**
    #[test]
    fn the_shared_reference_is_the_same_tag_the_harness_prints() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"abcd1234efgh","host":"10.9.9.7",
                 "port":31234,"token":"t","handle":"friend"}]"#,
        )
        .unwrap();
        assert_eq!(v[0].describe(), "SHARED ref=71c955 port_set=true");
    }

    /// `plxnative-servers` must NOT be exempt from the picker-suppression scan: it names a host and
    /// carries the token to trust it with, which is automation of the strongest kind. Listing it in
    /// DIAG would let a headless run boot to the who's-watching picker instead of Home.
    #[test]
    fn servers_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-servers"));
    }

    // ---- plxnative-playurl (the pipeline test tier's one trigger) ----

    /// The trigger decides WHAT THE APP PLAYS. Listing it in DIAG would leave a headless pipeline
    /// run booting to the who's-watching picker — with no session, to the sign-in screen — instead
    /// of into the player.
    #[test]
    fn playurl_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-playurl"));
    }

    /// **One vocabulary for a rung, in both directions.** `plxnative-quality` and
    /// `plxnative-qualityswitch` accept these strings and `quality: switch → …` prints them, so a
    /// case states its rung once and asserts on the same word. A one-way table would let the log
    /// drift from what the trigger accepts and the drift would show up as a case that arms
    /// correctly and then matches nothing — indistinguishable from the feature not working.
    #[test]
    fn every_quality_wire_name_parses_back_to_itself() {
        use plx_plex::plex::session::PlaybackQuality as Q;
        for q in [
            Q::Auto,
            Q::Original,
            Q::P1080High,
            Q::P1080,
            Q::P720,
            Q::P720Low,
            Q::P480,
        ] {
            let name = super::quality_wire_name(q);
            assert_eq!(
                super::parse_playback_quality(name),
                Some(q),
                "{name} does not round-trip"
            );
        }
    }

    /// The script grammar, including the two ways it must FAIL CLOSED. A typo that resolved to a
    /// default would switch the playback to something the case never asked for — the same hazard
    /// `plxnative-abrpin` documents for its own value.
    #[test]
    fn a_quality_script_fails_closed_and_never_substitutes() {
        use plx_plex::plex::session::PlaybackQuality as Q;
        let parse = super::parse_quality_switch_script;
        assert_eq!(
            parse("720p_4_mbps"),
            Some((0, vec![Q::P720])),
            "one step needs no cadence"
        );
        assert_eq!(
            parse("gap=9000,1080p_8_mbps,auto"),
            Some((9_000, vec![Q::P1080, Q::Auto])),
            "a leading gap is consumed, not treated as a rung",
        );
        assert_eq!(
            parse("gap=9000,nonsense"),
            None,
            "a typo must arm NOTHING, not a default"
        );
        assert_eq!(
            parse("gap=oops,auto"),
            None,
            "an invalid cadence must not become zero"
        );
        assert_eq!(
            parse("720p_4_mbps,auto"),
            None,
            "a multi-step script must state its cadence",
        );
        assert_eq!(parse(""), None);
        assert_eq!(
            parse("nonsense,auto"),
            None,
            "one invalid rung must not leave a valid subset running",
        );
    }

    #[test]
    fn a_pause_script_preserves_the_legacy_hold_and_fails_closed() {
        let parse = super::parse_pause_script;
        assert_eq!(
            parse(""),
            Some(super::PauseScript {
                delay_ms: 0,
                hold_ms: None,
                at_ms: None,
            }),
            "the empty screenshot trigger remains a permanent Pause",
        );
        assert_eq!(
            parse("delay=25000,hold=6000"),
            Some(super::PauseScript {
                delay_ms: 25_000,
                hold_ms: Some(6_000),
                at_ms: None,
            }),
        );
        assert_eq!(
            parse("at=432000"),
            Some(super::PauseScript {
                delay_ms: 0,
                hold_ms: None,
                at_ms: Some(432_000),
            }),
            "a position-gated pause: the published playhead, not a wall-clock delay",
        );
        assert_eq!(parse("at=1,at=2"), None);
        assert_eq!(parse("at=soon"), None);
        assert_eq!(parse("hold=0"), None);
        assert_eq!(parse("delay=10,delay=20"), None);
        assert_eq!(parse("hold=oops"), None);
        assert_eq!(parse("resume=6000"), None);
    }
}
