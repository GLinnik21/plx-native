//! Every dev-trigger ARM — the code that reads `/tmp/plxnative-*` through [`plx_base::devtrig::flag`] /
//! [`plx_base::devtrig::read`] / [`plx_base::devtrig::latched_flag!`] and reacts to it — gathered on one file (UI
//! restructure spec v4 §3.3 step 2 / §11, phase 10 lane P). A PURE MOVE: no trigger was renamed,
//! no timing changed, no ordering changed. Before this phase the arms were spread across four
//! loop files — `app/boot.rs` (read-once boot flags), `app/run.rs`'s `dev_scripts` and the
//! oscillator continuations embedded in its frame phases, `app/content.rs` (the detail-page
//! headless walk), and one pre-SDL flag in `app/mod.rs` — each reaching straight into `App`'s
//! fields. They still reach `App`'s fields, through `&mut` — [`Scenarios`] is what changed: it is
//! the ONE struct, owned by `App` as `app.scenarios`, that gathers every arm's own retry counter,
//! oscillator phase and boot-time latch, so a field used by no production code has exactly one
//! home instead of living beside `route`/`trail`/`pages` on `App` itself. A field genuine
//! production code also reads (`t0`, `refresh_hubs_at`) stayed on `App`, as the phase's own
//! instructions require.
//!
//! **Entry points, called from the loop at the exact positions the arms used to sit at:**
//! [`pre_boot`] (today's `app/mod.rs:451`, before SDL exists), the individual `at_boot`-style
//! functions below (each named for its trigger, called inline from `app::boot::boot` in the same
//! order the arms always ran in — several of them are read directly at the boot-decision call
//! site rather than through one grand entry point, because the boot gate's own control flow
//! — which `BootTo` a login/token/session resolves to — is not itself a dev arm and must not move
//! with it), [`advance_content_boot`] (today's `app/content.rs`'s `ContentBoot` machinery),
//! [`each_frame`] (today's `dev_scripts`, plus the oscillator continuations that used to be
//! embedded inline in `app/run.rs`'s `update`/`land_results`/`heartbeat` phases, called from this
//! file's `*_tick` functions at those same phase boundaries).
//!
//! **Not scenarios, deliberately left where they were:** the recorder/replay path
//! (`plxnative-rec`/`plxnative-recplay`, `app::clock`) is a DIFFERENT kind of thing — it observes
//! or reproduces a whole session and must never decide which screen a boot starts on (`dev.rs`'s
//! `DIAG` doc says why) — so its own machinery (`app::recorder::Recplay`) stays in
//! `app/recorder.rs`; only the two raw trigger reads run through the thin passthroughs
//! [`rec_trigger`] / [`recplay_trigger`] below, for the same reason every other read in this crate
//! goes through one door. The DIAG list itself stays in `dev.rs`.

use crate::app::App;
use crate::app::run::Frame;
use plx_screens::registry::AppArg;
use plx_screens::registry::HomeCmd;
use plx_machine::machine::{Key, Tick};
use std::os::raw::c_int;

pub(crate) mod bench;
pub(crate) mod screenshot;
#[cfg(feature = "devtriggers")]
pub(crate) mod clock_fact;
#[cfg(feature = "devtriggers")]
pub(crate) mod poster_gate;
#[cfg(feature = "devtriggers")]
pub(crate) mod tls_selftest;
#[cfg(feature = "devtriggers")]
pub(crate) mod toast_probe;

/// The dev triggers read ONCE at boot and consulted by the loop every frame after (each is
/// documented where it is READ, below). Formerly `App::dev: DevFlags`; unchanged in shape.
pub(crate) struct DevFlags {
    pub(crate) detail_osc: bool,
    pub(crate) home_osc: bool,
    pub(crate) hero_osc: bool,
    pub(crate) home_fold_osc: bool,
    pub(crate) lib_osc: bool,
    pub(crate) lib_switch: bool,
    pub(crate) search_osc: bool,
    pub(crate) settings_boot: Option<String>,
    pub(crate) settings_osc: bool,
    pub(crate) modal_osc: bool,
    pub(crate) legal_doc: bool,
    pub(crate) alert_boot: bool,
    pub(crate) account_osc: bool,
    pub(crate) consent_osc: bool,
    pub(crate) onboard_osc: bool,
    pub(crate) nav_osc: bool,
    pub(crate) nav_osc_rk: String,
    /// `plxnative-nobudget`: read at boot, applied to the one `Budget` at boot, and kept here so
    /// a log reader can tell an A leg from a B leg by the flags the boot recorded.
    pub(crate) nobudget: bool,
}

/// Every dev-trigger arm's own retry counter, oscillator phase and one-shot latch — the state
/// half of the move (spec's "per-arm STATE… moved into ONE Scenarios struct owned by App").
/// `pub(crate)` throughout: `app::boot`/`app::run`/`app::content` still read and write these
/// fields directly through `&mut App`, exactly as they read `App`'s own fields.
pub(crate) struct Scenarios {
    #[cfg(feature = "devtriggers")]
    pub(crate) poster_gate: poster_gate::Scene,
    pub(crate) pick_user: Option<usize>,
    pub(crate) home_osc_last: u32,
    pub(crate) hero_osc_last: u32,
    pub(crate) home_fold_osc_last: u32,
    pub(crate) home_fold_down: bool,
    pub(crate) lib_osc_last: u32,
    pub(crate) lib_switch_last: u32,
    pub(crate) lib_switch_step: u32,
    pub(crate) search_osc_last: u32,
    pub(crate) settings_osc_last: u32,
    pub(crate) settings_osc_down: bool,
    pub(crate) modal_osc_last: u32,
    pub(crate) legal_doc_tried: bool,
    pub(crate) alert_tried: bool,
    /// how many DOWN presses `plxnative-alert` has spent walking to the delete row.
    pub(crate) alert_step: u8,
    pub(crate) account_osc_last: u32,
    pub(crate) account_osc_down: bool,
    pub(crate) consent_osc_last: u32,
    pub(crate) consent_osc_down: bool,
    pub(crate) onboard_osc_last: u32,
    pub(crate) onboard_osc_right: bool,
    pub(crate) nav_osc_last: u32,
    pub(crate) marker_tried: bool,
    pub(crate) press_tried: bool,
    pub(crate) press_release_at: u32,
    pub(crate) itemmenu_tried: bool,
    pub(crate) acct_tried: bool,
    /// `/tmp/plxnative-acct`'s value, once read (see `acct_arm`).
    pub(crate) acct_rest: Option<Option<u32>>,
    pub(crate) auto_tried: bool,
    pub(crate) replay_left: u32,
    pub(crate) grid_tried: bool,
    pub(crate) settings_tried: bool,
    pub(crate) seek_tried: bool,
    pub(crate) seek_script: Vec<String>,
    pub(crate) seek_script_at: u32,
    pub(crate) seek_gap_ms: u32,
    pub(crate) seek_script_last: i64,
    pub(crate) quality_script: Vec<plx_plex::plex::session::PlaybackQuality>,
    pub(crate) quality_script_at: u32,
    pub(crate) quality_gap_ms: u32,
    pub(crate) quality_tried: bool,
    pub(crate) quality_playing_since: Option<u32>,
    pub(crate) detail_tried: bool,
    /// `/tmp/plxnative-collection=<ratingKey>` — direct Collection-page boot.
    pub(crate) collection_tried: bool,
    /// The headless detail-page walk (`plxnative-detail`/`-play`), see [`ContentBoot`].
    pub(crate) content_boot: Option<ContentBoot>,
    pub(crate) play_tried: bool,
    /// `/tmp/plxnative-play=<rk>` between its ASYNC request and the landing it plays from:
    /// `(server, ratingKey, the frame clock at which the wait gives up)`. See [`play_arm`].
    pub(crate) play_await: Option<(plx_plex::plex::ServerId, String, u32)>,
    pub(crate) menu_tried: bool,
    pub(crate) menupick_tried: bool,
    /// dev: the row `/tmp/plxnative-menupick` still owes the track menu, as its RAW second field
    /// (an absolute row number, or a named Audio-tab target such as `"boost"`/`"loudness"` — see
    /// `menupick_arm`'s doc): the panel opens and is picked on separate frames, so the pick is
    /// carried here until the surface it names exists.
    pub(crate) menupick_target: Option<String>,
    /// `/tmp/plxnative-subtiming` — see [`subtiming_arm`] and [`Subtiming`].
    pub(crate) subtiming: Subtiming,
    /// `/tmp/plxnative-submenuosc` — see [`submenuosc_arm`] and [`SubmenuOsc`].
    pub(crate) submenu_osc: SubmenuOsc,
    /// `/tmp/plxnative-moreosc` — see [`moreosc_arm`]; the same state as `submenu_osc`.
    pub(crate) more_osc: SubmenuOsc,
    pub(crate) pause_tried: bool,
    /// An armed Pause edge: (due at, hold ms, the media position it also waits for).
    pub(crate) pause_script: Option<(u32, Option<u32>, Option<u32>)>,
    pub(crate) pause_resume_at: Option<u32>,
    /// `/tmp/plxnative-pushbench` — see [`bench`]'s module doc. `None` unarmed; cleared to `None`
    /// once its `n` cycles are done, the same shape `content_boot` uses to stop being ticked.
    pub(crate) push_bench: Option<bench::PushBench>,
    /// `/tmp/plxnative-modalbench` — see [`bench`]'s module doc.
    pub(crate) modal_bench: Option<bench::ModalBench>,
    /// `/tmp/plxnative-deepbench` — see [`bench`]'s module doc.
    pub(crate) deep_bench: Option<bench::DeepBench>,
    /// `/tmp/plxnative-herobench` — see [`bench`]'s module doc and [`bench::HeroBench`].
    pub(crate) hero_bench: Option<bench::HeroBench>,
    /// The boot-time trigger flags the loop consults every frame after.
    pub(crate) dev: DevFlags,
    /// The screenshot pipeline's arms (`plxnative-libtype`, `-libgrid`, `-libshelf`, `-libmenu`, `-clockstop`) — see [`screenshot`].
    pub(crate) shots: screenshot::ScreenshotArms,
}

// =================================================================================================
// pre-SDL (today's app/mod.rs:451)
// =================================================================================================

/// `/tmp/plxnative-stats` — force the Stats-for-nerds overlay on, before SDL or a screen exists,
/// so a playback test photographs the same ABR/pipeline evidence on every automated run rather
/// than depending on a previous manual toggle surviving into this session.
pub(crate) fn pre_boot() {
    if plx_base::devtrig::flag("stats") {
        crate::app::diagnostics::open();
    }
}

// =================================================================================================
// boot-time arms (today's app/boot.rs) — each named for its trigger, called inline from
// `app::boot::boot` in the exact order the reads always ran in. The boot-DECISION arms
// (`plxnative-login`, `-token`) are deliberately thin: `BootTo` is core boot control flow, not
// itself a dev arm, and moving its branches would risk the one thing this phase must not touch.
// =================================================================================================

/// `/tmp/plxnative-novsync` — uncap the swap interval so `fps=` reports the true GPU render rate.
pub(crate) fn novsync_armed() -> bool {
    plx_base::devtrig::flag("novsync")
}

/// `/tmp/plxnative-login` — force the QR login screen even with a usable session.
pub(crate) fn login_forced() -> bool {
    plx_base::devtrig::flag("login")
}

/// `/tmp/plxnative-token` — the harness/headless test identity, read once. Never logged.
pub(crate) fn dev_token() -> String {
    match plx_base::devtrig::read("token") {
        Some(s) if !s.is_empty() => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log("token: using /tmp/plxnative-token (test identity)");
            s
        }
        _ => String::new(),
    }
}

/// Optional primary endpoint for a synthetic PMS fixture. Used only with the explicitly
/// injected dev token; a persisted account is never redirected. Absent in shipping builds.
pub(crate) fn pms_origin() -> Option<plx_plex::plex::Origin> {
    plx_base::devtrig::read("pms-origin").and_then(|s| plx_plex::plex::Origin::parse(s.trim()))
}

plx_base::devtrig::latched_flag! { pub(crate) fn imagecache_stats_armed = "imagecache-stats"; }
plx_base::devtrig::latched_flag! {
    /// `/tmp/plxnative-imgtrace` — per-image poster timeline and every picture-lost event
    /// (`app/adapters/poster/trace.rs`). Absent in shipping builds.
    pub(crate) fn imgtrace_armed = "imgtrace";
}
plx_base::devtrig::latched_flag! {
    /// RAM-only control leg for cache performance comparisons; absent in shipping builds.
    pub(crate) fn imagecache_bypass_armed = "imagecache-bypass";
}

/// `/tmp/plxnative-pickuser=<index>` — force the boot picker and auto-select that roster tile.
pub(crate) fn pickuser_index() -> Option<usize> {
    plx_base::devtrig::read("pickuser").and_then(|s| s.parse().ok())
}

/// `/tmp/plxnative-logintest` — validate the plex.tv account path end to end on the device.
pub(crate) fn arm_logintest() {
    if plx_base::devtrig::flag("logintest") {
        let _ = plx_base::task::spawn_small("logintest", || {
            let sess = plx_plex::plex::session::load();
            let ac = plx_plex::plex::account::AccountClient::new(&sess.client_id, None);
            match ac.create_pin() {
                Ok(p) => plx_base::eventlog::log(&format!(
                    "logintest: create_pin ok id={} code_len={} authToken_null={}",
                    p.id,
                    p.code.len(),
                    p.auth_token.is_none()
                )),
                Err(evidence) => plx_base::eventlog::log(&format!("logintest: create_pin FAILED ({})",
                    plx_plex::plex::account::describe_evidence(&evidence))),
            }
        });
    }
}

/// `/tmp/plxnative-stillclock=<ms>` — see [`screenshot::arm_stillclock`].
pub(crate) fn arm_stillclock() {
    screenshot::arm_stillclock();
}

/// `/tmp/plxnative-anim` — the animation-diagnostic overlay (off by default).
pub(crate) fn arm_anim() {
    if plx_base::devtrig::flag("anim") {
        plx_ui::anim::set_enabled(true);
    }
}

/// `/tmp/plxnative-glassload` — the backdrop-glass LOAD DIAL.
pub(crate) fn arm_glassload(glass: &mut plx_ui::frame::glass::GlassPlan) {
    if let Some(v) = plx_base::devtrig::read("glassload") {
        glass.configure_dial(&v);
    }
}

/// `/tmp/plxnative-navblur` — the blurred-route-transition prototype.
pub(crate) fn arm_navblur(glass: &mut plx_ui::frame::glass::GlassPlan) {
    if let Some(v) = plx_base::devtrig::read("navblur") {
        glass.configure_navblur(&v);
    }
}

/// `/tmp/plxnative-overdraw` — the CPU-side per-draw-class overdraw ledger.
pub(crate) fn arm_overdraw() {
    if plx_base::devtrig::flag("overdraw") {
        plx_gfx::overdraw::set_ledger(true);
    }
}

/// `/tmp/plxnative-drawmask=<classes>` — refuse every draw of the named classes.
pub(crate) fn arm_drawmask() {
    if let Some(spec) = plx_base::devtrig::read("drawmask") {
        plx_gfx::overdraw::set_mask(&spec);
    }
}

/// `/tmp/plxnative-heroground` — the one-pass hero ground A/B.
pub(crate) fn arm_heroground() {
    if plx_base::devtrig::flag("heroground") {
        plx_ui::widgets::set_hero_ground(true);
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log("hero: one-pass ground ENABLED by /tmp/plxnative-heroground");
    }
}

/// `/tmp/plxnative-profile` / `/tmp/plxnative-hwcnt` — the two GPU-time profilers. Both present
/// is refused; either alone arms its mode.
///
/// Gated on `devtriggers` at the item level rather than left to `devtrig::read` folding to `None`:
/// `devtrig::read` already makes this whole function inert in a release build, but the disabled-both
/// diagnostic line below spells out both trigger names in full, and a compiled-but-unreachable
/// function still carries its own string literals into `--no-default-features` bytes. Compiling
/// the function out entirely is what actually keeps `plxnative-profile`/`plxnative-hwcnt` out of
/// the binary `ci/check-package.py`'s dev-trigger-catalog check inspects.
#[cfg(feature = "devtriggers")]
pub(crate) fn arm_profile_hwcnt() {
    match (plx_base::devtrig::read("profile"), plx_base::devtrig::read("hwcnt")) {
        (Some(_), Some(_)) => {
            plx_base::eventlog::log("PROFILE disabled: remove either /tmp/plxnative-profile or /tmp/plxnative-hwcnt");
        }
        (Some(filter), None) => plx_ui::profile::set_enabled(&filter),
        (None, Some(filter)) => plx_ui::profile::set_hwcnt_enabled(&filter),
        (None, None) => {}
    }
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn arm_profile_hwcnt() {}

/// `/tmp/plxnative-cpuprof` — the render thread's own per-phase CPU clock.
pub(crate) fn arm_cpuprof() {
    if plx_base::devtrig::flag("cpuprof") {
        plx_ui::profile::set_cpu_enabled();
    }
}

/// `/tmp/plxnative-noidle` — turn the whole-frame present gate off.
pub(crate) fn arm_noidle() {
    if plx_base::devtrig::flag("noidle") {
        plx_machine::idle::set_enabled(false);
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log("idle: present gate DISABLED by /tmp/plxnative-noidle");
    }
}

/// `/tmp/plxnative-audioenh=off|boost|loudness` — force the PERSISTED Boost Dialog / Normalize
/// Loudness preference (issue #266) at boot, harness-only.
///
/// Every other boot override here (`dev::playback_quality_override`, `arm_glassload`, …) is
/// deliberately in-memory-only, so a test can never mutate a real person's saved preference. This
/// one is the one exception, and on purpose: `player::set_audio_enhancements` is the SAME call a
/// person's own track-menu pick makes (it both updates the live route and re-persists through the
/// storage worker), and an on-device harness case needs the persisted value itself pinned before
/// boot, not just the in-memory copy — one case proves the cold-start "a saved preference turns an
/// otherwise-direct-playable route into a remux" path (`route::plan`'s cold-start audio branch) by
/// booting with the preference already ON, and the very next boot must not inherit whatever a
/// FAILED previous case's toggle left behind. A toggle-based reset cannot promise that: if the
/// pick never lands, the preference is stuck at whatever the last successful toggle set it to.
/// Forcing the value at boot, every time, is what makes a case's starting preference a property of
/// the manifest instead of of history — the same argument `run_case`'s per-case viewOffset reset
/// already makes for resume position.
///
/// `off` clears both flags; `boost`/`loudness` sets exactly one (never both — no case here needs
/// both at once, and a value this test-only can grow a second name later without breaking the
/// existing ones). An unrecognised value is ignored rather than guessed at.
pub(crate) fn arm_audio_enhancements() {
    let Some(v) = plx_base::devtrig::read("audioenh") else { return };
    let a = match v.trim() {
        "off" => plx_plex::plex::AudioEnhancements::NONE,
        "boost" => plx_plex::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
        "loudness" => plx_plex::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
        #[allow(unused_variables)]
        other => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!("audioenh: unrecognised value {other:?} — ignored"));
            return;
        }
    };
    plx_media::player::set_audio_enhancements(a);
    #[cfg(feature = "devtriggers")]
    plx_base::eventlog::log(&format!(
        "audioenh: forced boost_dialog={} normalize_loudness={} by /tmp/plxnative-audioenh",
        a.boost_dialog, a.normalize_loudness,
    ));
}

/// `/tmp/plxnative-nobudget` — the frame budget's A/B CONTROL LEG (spec §8.1, phase 11).
///
/// Present, admission is what it was before phase 11: the `Poster` quota of three per frame and
/// nothing else — no time ceiling, no solo rule, and a `Residency` upload (a backdrop, a hero
/// logo) spending one of those three exactly as it used to. It exists so a device A/B measures
/// this CHANGE and not the difference between two builds, and it is DIAG for the reason
/// `plxnative-drawmask` is: an A/B whose two legs boot to different screens has measured the
/// screen.
pub(crate) fn nobudget_armed() -> bool {
    plx_base::devtrig::flag("nobudget")
}

/// `/tmp/plxnative-detailosc`.
pub(crate) fn detailosc_armed() -> bool {
    plx_base::devtrig::flag("detailosc")
}
/// `/tmp/plxnative-homeosc`.
pub(crate) fn homeosc_armed() -> bool {
    plx_base::devtrig::flag("homeosc")
}
/// `/tmp/plxnative-heroosc`.
pub(crate) fn heroosc_armed() -> bool {
    plx_base::devtrig::flag("heroosc")
}
/// `/tmp/plxnative-homefoldosc`.
pub(crate) fn homefoldosc_armed() -> bool {
    plx_base::devtrig::flag("homefoldosc")
}
/// `/tmp/plxnative-libosc`.
pub(crate) fn libosc_armed() -> bool {
    plx_base::devtrig::flag("libosc")
}
/// `/tmp/plxnative-libswitch`.
pub(crate) fn libswitch_armed() -> bool {
    plx_base::devtrig::flag("libswitch")
}
/// `/tmp/plxnative-searchosc`.
pub(crate) fn searchosc_armed() -> bool {
    plx_base::devtrig::flag("searchosc")
}
/// `/tmp/plxnative-settings=<root|home|privacy|legal|playback|picker-quality|…>`.
pub(crate) fn settings_boot_value() -> Option<String> {
    plx_base::devtrig::read("settings")
}
/// `/tmp/plxnative-settingsosc`.
pub(crate) fn settingsosc_armed() -> bool {
    plx_base::devtrig::flag("settingsosc")
}
/// `/tmp/plxnative-modalosc`.
pub(crate) fn modalosc_armed() -> bool {
    plx_base::devtrig::flag("modalosc")
}
/// `/tmp/plxnative-legaldoc`.
pub(crate) fn legaldoc_armed() -> bool {
    plx_base::devtrig::flag("legaldoc")
}
/// `/tmp/plxnative-alert`.
pub(crate) fn alert_armed() -> bool {
    plx_base::devtrig::flag("alert")
}
/// `/tmp/plxnative-acctosc`.
pub(crate) fn acctosc_armed() -> bool {
    plx_base::devtrig::flag("acctosc")
}
/// `/tmp/plxnative-consentosc`.
pub(crate) fn consentosc_armed() -> bool {
    plx_base::devtrig::flag("consentosc")
}
/// `/tmp/plxnative-onboardosc`.
pub(crate) fn onboardosc_armed() -> bool {
    plx_base::devtrig::flag("onboardosc")
}
/// `/tmp/plxnative-navosc[=<ratingKey>]`.
pub(crate) fn navosc_value() -> Option<String> {
    plx_base::devtrig::read("navosc")
}
/// `/tmp/plxnative-pushbench[=<n>[,<ratingKey>]]` — `(n, ratingKey)`, defaulting `n` to 100 and
/// `ratingKey` to empty (the empty case is resolved against `navosc`'s own value by the caller,
/// `app::boot::boot`, before `bench::PushBench::new` ever sees it).
pub(crate) fn pushbench_value() -> Option<(u32, String)> {
    plx_base::devtrig::read("pushbench").map(|v| {
        let v = v.trim();
        if v.is_empty() {
            return (bench::DEFAULT_BENCH_N, String::new());
        }
        let (n, rk) = v.split_once(',').unwrap_or((v, ""));
        (parse_bench_n(n), rk.trim().to_string())
    })
}
/// `/tmp/plxnative-modalbench[=<n>[,<ratingKey>]]` — `(n, ratingKey)`, the same shape as
/// [`pushbench_value`] and for the same reason: the item menu leg needs its own ratingKey, and
/// piggy-backing on `navosc`'s value would also arm `navosc`'s own independent Home<->tab/Detail
/// bounce (`nav_osc = nav_osc_rk.is_some()` in `app::boot::boot`) — two competing navigators
/// racing the same nav stack while modalbench tries to measure. Empty is resolved against
/// `navosc`'s own value by the caller exactly as pushbench's empty case is, so a bare
/// `plxnative-navosc=<rk>` with no `plxnative-modalbench` value still works; a scene wanting the
/// item menu WITHOUT navosc's bounce sets its own `<rk>` here instead.
pub(crate) fn modalbench_value() -> Option<(u32, String)> {
    plx_base::devtrig::read("modalbench").map(|v| {
        let v = v.trim();
        if v.is_empty() {
            return (bench::DEFAULT_BENCH_N, String::new());
        }
        let (n, rk) = v.split_once(',').unwrap_or((v, ""));
        (parse_bench_n(n), rk.trim().to_string())
    })
}
/// `/tmp/plxnative-deepbench[=<depth>[,<ratingKey>]]` — `(depth, ratingKey)`, the same shape as
/// [`pushbench_value`]/[`modalbench_value`] and the same empty-value resolution (against
/// `navosc`'s own ratingKey, by the caller, `app::boot::boot`) — `Library` cannot stand in for a
/// missing ratingKey here the way it does for `PushBench`, so a `DeepBench` with no ratingKey at
/// all runs zero cycles (`bench::DeepBench::new`'s own doc says why) rather than falling back to
/// anything.
pub(crate) fn deepbench_value() -> Option<(u32, String)> {
    plx_base::devtrig::read("deepbench").map(|v| {
        let v = v.trim();
        if v.is_empty() {
            return (bench::DEFAULT_BENCH_N, String::new());
        }
        let (n, rk) = v.split_once(',').unwrap_or((v, ""));
        (parse_bench_n(n), rk.trim().to_string())
    })
}
/// `/tmp/plxnative-herobench[=<n>[,<period_ms>]]` — `(n, period_ms)`, defaulting to 100 flips'
/// worth of cycles at [`bench::DEFAULT_HERO_PERIOD_MS`]. A period below the carousel's flip
/// cooldown is raised to it by [`bench::HeroBench::new`].
pub(crate) fn herobench_value() -> Option<(u32, u32)> {
    plx_base::devtrig::read("herobench").map(|v| {
        let v = v.trim();
        let (n, period) = v.split_once(',').unwrap_or((v, ""));
        (
            parse_bench_n(n.trim()),
            period.trim().parse().unwrap_or(bench::DEFAULT_HERO_PERIOD_MS),
        )
    })
}
fn parse_bench_n(v: &str) -> u32 {
    if v.is_empty() {
        bench::DEFAULT_BENCH_N
    } else {
        v.parse().ok().filter(|n: &u32| *n > 0).unwrap_or(bench::DEFAULT_BENCH_N)
    }
}
/// `/tmp/plxnative-framedrop[=<ms>]`.
pub(crate) fn framedrop_value() -> Option<String> {
    plx_base::devtrig::read("framedrop")
}
/// `/tmp/plxnative-framering[=<ms>]` — the frame-drop detector's context ring
/// (`diag::heartbeat::FrameRing`): write only frames of `<ms>` or more (default 17) and their
/// neighbours, instead of every frame. `None` when unarmed.
pub(crate) fn framering_ms() -> Option<f64> {
    plx_base::devtrig::read("framering").map(|s| s.parse().ok().filter(|v: &f64| *v > 0.0).unwrap_or(17.0))
}
/// `/tmp/plxnative-firstrun`.
pub(crate) fn firstrun_armed() -> bool {
    plx_base::devtrig::flag("firstrun")
}
/// `/tmp/plxnative-acct[=<ms>]` — auto-open the profile menu (headless capture of the popover).
/// `None` when unarmed; `Some(None)` opens it as soon as Home is up; `Some(Some(ms))` waits until
/// the screen has been at rest for `ms` first (simulator only — see `acct_arm`).
pub(crate) fn acct_armed() -> Option<Option<u32>> {
    plx_base::devtrig::read("acct").map(|v| v.parse().ok())
}
/// `/tmp/plxnative-replay[=N]`'s raw content, for [`crate::app::boot::replay_budget`].
pub(crate) fn replay_trigger_value() -> Option<String> {
    plx_base::devtrig::read("replay")
}

// =================================================================================================
// the headless detail-page walk (today's app/content.rs)
// =================================================================================================

/// The `/tmp/plxnative-detail`/`-play` headless walk: which section/column to press into, whether
/// to activate the focused control, and whether to continue into the cast/crew filmography strip.
/// Moved verbatim out of `app/content.rs` (phase 10 lane P) — its own module doc said as much:
/// "Content navigation during the legacy route transition" was never true of this type, which
/// exists only to script a boot trigger through the real focus/activate path.
pub(crate) struct ContentBoot {
    /// The page this boot is waiting for, as its own identity. It was a `ui::trail::Node` — a
    /// whole history entry — for the `(sid, rk)` pair and the `Spot`'s season inside it.
    sid: plx_plex::plex::ServerId,
    rk: String,
    season: Option<i64>,
    down: u32,
    right: u32,
    activate: bool,
    filmography: bool,
    /// `/tmp/plxnative-bio` — once the person page has landed, present its biography sheet. It
    /// rides the same wait as `filmography` because it needs the same thing: the person's profile
    /// has to have ARRIVED, or the sheet is offered over a header that has not decided whether its
    /// prose is truncated yet.
    bio: bool,
    waiting_person: bool,
    ready_seen: bool,
}

impl ContentBoot {
    fn controlled(sid: plx_plex::plex::ServerId, input: &crate::app::bootstrap::ContentInitial) -> Self {
        Self { sid, rk: input.detail.clone(), season:None, down:input.detailsec, right:0,
            activate:input.detailok, filmography:input.filmography, bio:false,
            waiting_person:false, ready_seen:false }
    }
    /// Is the page this boot is waiting for the one on top?
    fn is_top(&self, d: &plx_ui::dispatch::Dispatcher<crate::app::bridge::AppHost>) -> bool {
        matches!(d.top_arg(), Some(AppArg::Content(plx_screens::registry::ContentArg::Detail { sid, rk }))
            if *sid == self.sid && *rk == self.rk)
    }

    pub(crate) fn new(sid: plx_plex::plex::ServerId, rk: String) -> Self {
        Self {
            sid,
            rk,
            season: None,
            down: plx_base::devtrig::read("detailsec").and_then(|s| s.parse().ok()).unwrap_or(0),
            right: plx_base::devtrig::read("detailcol").and_then(|s| s.parse().ok()).unwrap_or(0),
            activate: plx_base::devtrig::flag("detailok") || plx_base::devtrig::flag("detailplay"),
            filmography: plx_base::devtrig::flag("filmography"),
            bio: plx_base::devtrig::flag("bio"),
            waiting_person: false,
            ready_seen: false,
        }
    }

    fn admit_landing(&mut self, ready: bool) -> bool {
        let admitted = ready && self.ready_seen;
        self.ready_seen = ready;
        admitted
    }
}

/// `/tmp/plxnative-detailplay` — whether the headless Play from the content walk pins the HUD for
/// a headless capture (`HUD_HEADLESS_MS`) rather than the ordinary linger duration.
pub(crate) fn detailplay_forces_headless_hud() -> bool {
    plx_base::devtrig::flag("detailplay")
}

pub(crate) fn advance_content_boot(app: &mut App, fr: &Frame) {
    use crate::app::bridge;
    use plx_screens::registry::{AppArg, ContentArg};
    use plx_machine::machine::{Delivery, Fx, MachineId, NavOp};
    use plx_ui::screen::ScreenEvent;

    let Some(mut boot) = app.scenarios.content_boot.take() else { return };
    let ready = if boot.waiting_person {
        app.pages.nav.top_page().is_some_and(|entry| {
            let AppArg::Content(ContentArg::Person { sid, key, .. }) = &entry.arg else { return false };
            app.bridge.person_view().current().is_some_and(|p| p.sid == *sid && p.key == *key
                // **The BIO sheet waits for the person, not for the CREDITS.** The filmography
                // needs `credited`/`landed` because it is a list of them; the biography needs the
                // person's own facts, which the header already has. Requiring the credits here
                // would be requiring something a headless boot cannot have: they come from
                // plex.tv, which answers an injected server token 401 (`account: … HTTP 401` in
                // the event log), so the wait would never end and the trigger would present
                // nothing at all — measured on the simulator, 2026-09-10.
                && (boot.bio || (p.credited && p.landed)))
        })
    } else {
        let meta = app.bridge.metadata_view();
        let loaded = meta.current().map(|d|
            (d.sid, d.rk.as_str(), d.seasons.get(d.cur_season).map(|s| s.index)));
        boot.is_top(&app.pages)
            && detail_boot_ready(boot.sid, &boot.rk, boot.season, loaded, meta.detail_loading(), meta.season_loading())
    };
    // A complete landing must have passed through the screen's StoreChanged step first.
    if !boot.admit_landing(ready) {
        app.scenarios.content_boot = Some(boot);
        return;
    }
    if boot.waiting_person && boot.bio {
        // `/tmp/plxnative-bio` — the person page's biography sheet, presented through the same
        // door OK on the header uses (`ContentPanel::Bio`) and gated on the same predicate, so the
        // trigger cannot open a sheet an interactive press would have refused. It exists because
        // this sheet has no other headless route: OK on the header only opens it when the prose is
        // TRUNCATED, and the bio comes from plex.tv, which answers an injected server token 401 —
        // so pair it with `/tmp/plxnative-personbio=<text>` for a boot that has any prose at all.
        let available = app
            .pages
            .nav
            .top_page()
            .and_then(|e| e.inst.as_ref())
            .and_then(|i| i.screen.as_any())
            .and_then(|a| a.downcast_ref::<plx_screens::person::PersonScreen>())
            .is_some_and(|page| page.bio_available(app.bridge.person_view()));
        if let (Some(host), true) = (app.pages.top_page(), available) {
            bridge::open_content_panel(
                &mut app.pages,
                host,
                None,
                plx_screens::registry::ContentPanel::Bio,
            );
            return;
        }
        // Not offerable YET — the profile can land after the page does, and `bio_available` is a
        // measurement of prose that has not arrived. Keep the boot and ask again next frame rather
        // than spending the one chance on the first frame the page was up.
        app.scenarios.content_boot = Some(boot);
        return;
    }
    if boot.waiting_person {
        let person = app.pages.nav.top_page().map(|e| e.arg.clone());
        if let Some(AppArg::Content(ContentArg::Person { sid, key, .. })) = person {
            app.pages.nav.next_style = plx_ui::containers::modal::Style::Opaque { snapshot: true };
            app.pages.request(MachineId::Nav, NavOp::Present(AppArg::Content(
                ContentArg::Filmography { sid, key })));
            return;
        }
    } else if boot.is_top(&app.pages) {
        let key = if boot.down > 0 {
            boot.down -= 1;
            Some(Key::Down)
        } else if boot.right > 0 {
            boot.right -= 1;
            Some(Key::Right)
        } else { None };
        if let Some(key) = key {
            app.inputs.extend(bridge::script_key(key, Tick { ms: fr.now, dt_us: 0 }));
        } else {
            // `/tmp/plxnative-tracks=<n>` presents the page's own *Track information* sheet at
            // page `n` — the only caller that opens it anywhere but page 1, and the only way a
            // headless capture reaches page 2 at all. It goes through the same door the Languages
            // press does (`bridge::open_content_panel`), so the trigger cannot present a panel the
            // page would refuse: availability is the PAGE's answer about the page's own item.
            if let Some(pg) = app.boot_initial.is_none().then(|| plx_base::devtrig::read("tracks")).flatten() {
                let host = app.pages.top_page();
                let meta = app.bridge.metadata_view();
                let available = app
                    .pages
                    .nav
                    .top_page()
                    .and_then(|e| e.inst.as_ref())
                    .and_then(|i| i.screen.as_any())
                    .and_then(|a| a.downcast_ref::<plx_screens::detail::DetailScreen>())
                    .is_some_and(|d| d.tracks_available(meta));
                if let (Some(host), true) = (host, available) {
                    let (sid, rk) = (boot.sid, boot.rk.clone());
                    bridge::open_content_panel(
                        &mut app.pages,
                        host,
                        Some((sid, &rk)),
                        plx_screens::registry::ContentPanel::Tracks {
                            page: pg.trim().parse().unwrap_or(1),
                        },
                    );
                }
            }
            // `/tmp/plxnative-about` presents the page's own *About* sheet — the footer card's
            // synopsis read in full. It carries no page or cursor, so unlike `plxnative-tracks`
            // the trigger is a bare flag; like it, it goes through the same door the OK press on
            // that card uses (`bridge::open_content_panel` over `ContentPanel::About`), so the
            // headless boot cannot present a sheet an interactive press could not.
            //
            // It exists because this sheet has no other headless route: it opens from the About
            // footer's FIRST column, four sections down a page whose section count depends on the
            // item, so a `down`/`right` script that reached it on one film would miss it on the
            // next — and `fps:about-panel` needs the same screen every run.
            if app.boot_initial.is_none() && plx_base::devtrig::flag("about") {
                if let Some(host) = app.pages.top_page() {
                    let (sid, rk) = (boot.sid, boot.rk.clone());
                    bridge::open_content_panel(
                        &mut app.pages,
                        host,
                        Some((sid, &rk)),
                        plx_screens::registry::ContentPanel::About,
                    );
                }
            }
            if boot.activate {
                if let (Some(instance), Some(focus)) = (app.pages.top_page(), app.pages.focus()) {
                    app.pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                        Delivery::Screen(ScreenEvent::Activate(focus.elem))));
                }
            }
            if !boot.filmography && !boot.bio { return; }
            boot.waiting_person = true;
            boot.ready_seen = false;
        }
    }
    app.scenarios.content_boot = Some(boot);
}

fn detail_boot_ready(sid: plx_plex::plex::ServerId, rk: &str, want_season: Option<i64>,
    loaded: Option<(plx_plex::plex::ServerId, &str, Option<i64>)>, detail_loading: bool, season_loading: bool) -> bool {
    !detail_loading && !season_loading && loaded.is_some_and(|(server, key, season)|
        server == sid && key == rk && want_season.is_none_or(|wanted| season == Some(wanted)))
}

#[cfg(test)]
mod content_boot_tests {
    use super::*;

    #[test]
    fn delayed_detail_and_season_landings_do_not_consume_headless_directions() {
        let sid = plx_plex::plex::ServerId::UNSET;
        let mut boot = ContentBoot { sid, rk: "1001".into(), season: Some(2), down: 2, right: 1,
            activate: true, filmography: false, bio: false, waiting_person: false, ready_seen: false };
        let ready_for = |loaded, d, sl| detail_boot_ready(sid, "1001", Some(2), loaded, d, sl);
        let loaded = Some((sid, "1001", Some(2)));
        for ready in [
            ready_for(None, true, false),
            ready_for(loaded, true, false),
            ready_for(Some((sid, "1001", Some(1))), false, false),
            ready_for(loaded, false, true),
        ] {
            assert!(!boot.admit_landing(ready));
            assert_eq!((boot.down, boot.right), (2, 1));
        }
        assert!(!boot.admit_landing(ready_for(loaded, false, false)),
            "the landing frame is left for the screen to publish its sections");
        assert!(boot.admit_landing(ready_for(loaded, false, false)));
        assert_eq!((boot.down, boot.right), (2, 1));
        assert!(!ready_for(Some((sid, "1002", Some(2))), false, false));
    }
}

// =================================================================================================
// per-frame scripts (today's app/run.rs `dev_scripts`) — dev-only schedules keyed on `app.t0`;
// each fires once and latches. `each_frame` returns `false` when a refused trigger must end the
// iteration early (the loop `continue`s, as it always did).
// =================================================================================================

/// The `/tmp/plxnative-search[=<query>]` trigger's seed-and-stand. A host test can drive the
/// exact effects the trigger causes rather than re-typing them by hand — see
/// `app::search_owned_tests::a_seeded_boot_query_survives_the_freshly_mounted_screens_first_sync`,
/// which calls this function directly and does NOT drive [`plx_base::devtrig::read`] itself (that one line is
/// not covered by a host test; a full `App`/SDL frame would be needed to reach it).
pub(crate) fn apply_search_boot_trigger(
    q: &str,
    d: &mut plx_ui::dispatch::Dispatcher<crate::app::bridge::AppHost>,
    bridge: &mut crate::app::bridge::Bridge,
) {
    bridge.search_run(plx_data::stores::search::SearchCmd::SetQuery(q.trim().to_string()));
    // A peer of Home, exactly as an interactive press on the strip's last pill is: `SelectTab`,
    // not `Root` — boot has already rooted Home, and a pill press must still leave BACK returning
    // to it rather than discarding it.
    crate::app::bridge::nav_select_tab(d, AppArg::Search);
}

/// `now - at >= gap_ms`, read as SIGNED so a future `at` (a `delay=` in force) correctly does not
/// fire yet. See the tests below for the wrap and delay traps this predicate has to survive.
/// A trigger's value, read until it is first found armed and held from then on, so a per-frame
/// arm costs one read rather than one per frame. An unarmed trigger is looked for again next call.
fn latched<T: Copy>(slot: &mut Option<T>, read: impl FnOnce() -> Option<T>) -> Option<T> {
    if slot.is_none() {
        *slot = read();
    }
    *slot
}

fn script_step_due(now: u32, at: u32, gap_ms: u32) -> bool {
    (now.wrapping_sub(at) as i32) >= gap_ms as i32
}

#[cfg(test)]
mod trigger_latch_tests {
    use super::latched;

    #[test]
    fn an_armed_trigger_is_read_once_and_then_held() {
        let (mut slot, mut reads) = (None, 0);
        for _ in 0..5 {
            assert_eq!(latched(&mut slot, || { reads += 1; Some(Some(800u32)) }), Some(Some(800)));
        }
        assert_eq!(reads, 1, "an armed trigger's file was read on every frame");
    }

    #[test]
    fn an_unarmed_trigger_is_looked_for_until_it_appears() {
        let (mut slot, mut reads) = (None::<Option<u32>>, 0);
        assert_eq!(latched(&mut slot, || { reads += 1; None }), None);
        assert_eq!(latched(&mut slot, || { reads += 1; None }), None);
        assert_eq!(latched(&mut slot, || { reads += 1; Some(None) }), Some(None));
        assert_eq!(latched(&mut slot, || { reads += 1; Some(Some(1)) }), Some(None), "re-read once latched");
        assert_eq!(reads, 3);
    }
}

#[cfg(test)]
mod script_schedule_tests {
    use super::script_step_due;

    #[test]
    fn a_delay_longer_than_the_gap_does_not_fire_at_once() {
        let (now, gap, delay) = (1_000_000u32, 300u32, 95_000u32);
        let at = now.wrapping_sub(gap).wrapping_add(delay);
        assert!(!script_step_due(now, at, gap), "the delayed step fired immediately");
        assert!(!script_step_due(now.wrapping_add(delay - 1), at, gap), "fired one ms early");
        assert!(script_step_due(now.wrapping_add(delay), at, gap), "never fired at the delay");
    }

    #[test]
    fn an_undelayed_script_still_fires_at_once_then_one_gap_apart() {
        let (now, gap) = (1_000_000u32, 300u32);
        let at = now.wrapping_sub(gap);
        assert!(script_step_due(now, at, gap), "the first step must fire on arming");
        assert!(!script_step_due(now.wrapping_add(gap - 1), now, gap), "second step fired early");
        assert!(script_step_due(now.wrapping_add(gap), now, gap), "second step never fired");
    }

    #[test]
    fn the_predicate_survives_the_tick_wrap() {
        let (gap, at) = (300u32, u32::MAX - 100);
        assert!(!script_step_due(at.wrapping_add(299), at, gap));
        assert!(script_step_due(at.wrapping_add(300), at, gap));
    }
}

/// Pin the player HUD up for a headless capture — the shared tail of the menu/menupick/autopause
/// arms. Stays defined in `app/run.rs` (it draws on that module's own HUD plumbing); called from
/// here at the exact points the arms always called it.
use crate::app::run::pin_headless_hud;

fn autoplay_arm(app: &mut App, fr: &mut Frame) {
    use plx_screens::player::input::HUD_HEADLESS_MS;
    if !app.scenarios.auto_tried
        && !matches!(app.route(), AppArg::Player | AppArg::Login | AppArg::Profiles)
        && fr.now.wrapping_sub(app.t0) > 2000
    {
        app.scenarios.auto_tried = true;
        let playurl = plx_base::devtrig::flag("playurl");
        if plx_base::devtrig::flag("autoplay") || playurl {
            let requested = if playurl || plx_base::devtrig::flag("h265") {
                plx_media::route::clear_url(&mut app.player.session);
                true
            } else {
                let pidx = plx_base::devtrig::read("playidx")
                    .and_then(|s| s.parse::<c_int>().ok())
                    .unwrap_or(0);
                let snapshot = app.bridge.hubs_snapshot();
                let pmm = usize::try_from(pidx).ok().and_then(|i| {
                    let (hub, col) = (i / crate::app::COLS as usize, i % crate::app::COLS as usize);
                    snapshot.view().hub(hub).and_then(|h| h.items.get(col))
                });
                if let Some(pmm) = pmm {
                    let requested = plx_media::route::request_play_movie(&mut app.player.session, app.bridge.metadata_mut(), pmm, &crate::app::playback::movie_ctx(pmm));
                    if requested {
                        // ASYNC (phase 11): nothing here reads `metadata::current()` — the play
                        // plan came from the catalog row itself. The detail is wanted only so the
                        // player's Info card has a descriptor, and the landing's own
                        // `install_landed_detail` calls the same `sync_now_playing` the blocking
                        // load did. So there is nothing to wait for, and no reason to spend two
                        // PMS round trips of the SDL thread on the frame that starts a playback.
                        app.bridge.metadata_mut().run(plx_data::stores::metadata::MetadataCmd::RequestDetail { sid: pmm.sid, rk: pmm.rk.to_string() });
                    }
                    requested
                } else {
                    false
                }
            };
            if requested {
                crate::app::playback::start_playback(&mut app.player.session,
                    &mut app.adapters.player,
                    0,
                    crate::app::playback::Origin::Here,
                    HUD_HEADLESS_MS,
                    None,
                    &mut app.pages,
                    &mut app.bridge,
                );
            }
        }
    }
}

fn grid_library_search_heroidx_arm(app: &mut App, _fr: &mut Frame) {
    if !app.scenarios.grid_tried && _fr.now.wrapping_sub(app.t0) > 400 {
        app.scenarios.grid_tried = true;
        // `grid` alone (or `itemmenu`) seats the first card; `grid=<row>,<col>` any other.
        if let Some(v) = plx_base::devtrig::read("grid").or_else(|| plx_base::devtrig::flag("itemmenu").then(String::new)) {
            let (row, col) = screenshot::parse_cell(&v).unwrap_or((0, 0));
            app.bridge.home_command(HomeCmd::FocusGrid { row, col });
        }
        if let Some(s) = plx_base::devtrig::read("library") {
            let kind = match s.parse::<usize>().unwrap_or(0) {
                1 => plx_data::stores::browse::SecKind::Show,
                _ => plx_data::stores::browse::SecKind::Movie,
            };
            app.bridge.enter_library(kind);
            // Same pill semantics as the search trigger above: boot has already rooted Home,
            // so this is `SelectTab`, not a `Root` that would discard it.
            crate::app::bridge::nav_select_tab(&mut app.pages, AppArg::Library);
        }
        if let Some(q) = plx_base::devtrig::read("search") {
            apply_search_boot_trigger(&q, &mut app.pages, &mut app.bridge);
        }
        if let Some(s) = plx_base::devtrig::read("heroidx") {
            if let Ok(n) = s.parse::<c_int>() {
                app.bridge.home_command(HomeCmd::SelectHero(n));
            }
        }
        // `/tmp/plxnative-heropin=<n>` — `heroidx`, then HOLD that billboard (no auto-advance):
        // the screenshot pipeline's pin, so a settled capture shows the slot its scene named.
        if let Some(s) = plx_base::devtrig::read("heropin") {
            if let Ok(n) = s.parse::<c_int>() {
                app.bridge.home_command(HomeCmd::PinHero(n));
            }
        }
    }
}

fn settings_boot_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.settings_tried && fr.now.wrapping_sub(app.t0) > 800 {
        if matches!(app.route(), AppArg::Home) {
            app.scenarios.settings_tried = true;
            let page = match app.scenarios.dev.settings_boot.as_deref().map(str::trim).unwrap_or("root") {
                "" | "root" => plx_screens::family::SettingsPage::Root,
                "home" => plx_screens::family::SettingsPage::Favourites,
                "privacy" => plx_screens::family::SettingsPage::Privacy,
                "legal" => plx_screens::family::SettingsPage::Legal,
                "language" => plx_screens::family::SettingsPage::Language,
                "contribute" => plx_screens::family::SettingsPage::Contribute,
                "playback" => plx_screens::family::SettingsPage::Playback,
                // root → Playback → the Quality picker (`SettingsPage::boot_trail`), so BACK works.
                "picker-quality" => plx_screens::family::SettingsPage::Picker(plx_screens::family::PickerKind::Quality),
                "picker-next-episode" => plx_screens::family::SettingsPage::Picker(plx_screens::family::PickerKind::NextEpisode),
                "picker-deck-press" => plx_screens::family::SettingsPage::Picker(plx_screens::family::PickerKind::DeckPress),
                "picker-skip-interval" => plx_screens::family::SettingsPage::Picker(plx_screens::family::PickerKind::SkipInterval),
                "audio" => plx_screens::family::SettingsPage::AudioSubtitles,
                _other => {
                    #[cfg(feature = "devtriggers")]
                    plx_base::eventlog::log(&format!("BADTRIGGER settings-boot target {_other:?} unknown; opened root instead"));
                    plx_screens::family::SettingsPage::Root
                }
            };
            crate::app::bridge::open_settings_at(&mut app.pages, page);
        } else if fr.now.wrapping_sub(app.t0) > 12_000 {
            app.scenarios.settings_tried = true;
            plx_base::eventlog::log("settings: boot target timed out before Home became available");
        }
    }
}

fn press_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.press_tried && fr.now.wrapping_sub(app.t0) > 1600 {
        app.scenarios.press_tried = true;
        if plx_base::devtrig::flag("press")
            && ((matches!(app.route(), AppArg::Home) && app.bridge.home_grid_focused(&app.pages))
                || (matches!(app.route(), AppArg::Library) && crate::app::bridge::Bridge::library_card_focused(&app.pages)))
        {
            app.inputs.push(crate::app::bridge::script_key(Key::Ok,
                Tick { ms: fr.now, dt_us: 0 })[0].clone());
            app.scenarios.press_release_at = fr.now.wrapping_add(150).max(1);
        }
    }
    if app.scenarios.press_release_at != 0 && fr.now.wrapping_sub(app.scenarios.press_release_at) < 0x8000_0000 {
        app.scenarios.press_release_at = 0;
        app.input.press.release(fr.now);
        app.inputs.push(crate::app::bridge::script_key(Key::Ok,
            Tick { ms: fr.now, dt_us: 0 })[1].clone());
    }
}

/// `/tmp/plxnative-acct` — auto-open the profile menu (headless capture of the surface).
///
/// A per-frame ARM rather than a boot assignment, and that is what the surface changed: the menu
/// used to be a route the boot could simply name (`route = Route::Account { over: BarHost::Home }`
/// beside `account_menu::open()`), and it is now presented on the container's `ModalStack`, which
/// exists only once the loop is running. Same shape as `itemmenu_arm` beside it.
///
/// `acct=<ms>` holds the menu back until the screen has been at rest for `<ms>`
/// ([`screenshot::at_rest`]): a menu opened on the first Home frame freezes a page whose hero
/// backdrop has not arrived yet, and the documentation figure wants the menu over a LANDED Home.
///
/// The trigger's value is read once and held ([`latched`]): `acct` carries a value, so arming it
/// is an open+read rather than a stat, and a debug build on the television must not pay that on
/// every frame. Once the menu is open or the arm gives up, `acct_tried` ends it before any read.
fn acct_arm(app: &mut App, fr: &mut Frame) {
    if app.scenarios.acct_tried {
        return;
    }
    let Some(rest) = latched(&mut app.scenarios.acct_rest, crate::dev::scenarios::acct_armed) else {
        return;
    };
    if screenshot::at_rest(fr.now, rest) && matches!(app.route(), AppArg::Home) && app.pages.top_page().is_some() {
        app.scenarios.acct_tried = true;
        crate::app::bridge::open_account_menu(&mut app.pages);
    } else if fr.now.wrapping_sub(app.t0) > 12_000 {
        app.scenarios.acct_tried = true;
    }
}

/// `/tmp/plxnative-itemmenu` — snap into the grid and open the press-and-hold card menu on the
/// focused card (`fps:item-menu`, and the headless capture of the panel).
///
/// It presents through THE SAME PATH the hold does and always did — `HomeCmd::ItemMenu` is queued
/// for the mounted Home screen, whose `emit_item_menu` raises `HomeReq::ItemMenu`, which
/// `content::home_requests` turns into `bridge::open_item_menu`. Nothing here reaches around the
/// screen; the trigger exists because the interactive path is a real >=500 ms hold, which no boot
/// trigger can express.
///
/// **Done means the SURFACE is up**, not that the command was queued (phase 10). The queue is
/// data-dependent — `deliver_home_commands` holds `ItemMenu` back until the first catalog arrives,
/// and `request_home_menu` refuses while focus is on the strip rather than the grid — so latching
/// on the enqueue could mark the scene armed for a panel that never appeared, which reads on the
/// television as an fps scene measuring the wrong screen. The 12 s ceiling is what stops it
/// retrying forever on a boot that never reaches a grid at all.
fn itemmenu_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.itemmenu_tried && fr.now.wrapping_sub(app.t0) > 1800 {
        if plx_base::devtrig::flag("itemmenu") && matches!(app.route(), AppArg::Home) {
            app.bridge.request_home_menu(&app.pages);
            app.scenarios.itemmenu_tried = crate::app::bridge::item_menu_up(&app.pages)
                || fr.now.wrapping_sub(app.t0) > 12_000;
        } else {
            app.scenarios.itemmenu_tried = true;
        }
    }
}

/// `/tmp/plxnative-detail=<rk>` — boot straight onto a detail page (`fps:cold-open`).
///
/// **The request is the ASYNC one, and that is the whole scene.** This arm ran
/// `MetadataCmd::LoadDetailNow` until phase 11 — the deliberately BLOCKING load, two sequential
/// PMS round trips plus the `Detail` build, on the SDL thread, from inside `each_frame`, i.e.
/// inside the frame's `results` phase. `fps:cold-open` was therefore measuring a synchronous
/// double GET that the PRODUCT does not perform: an OK on a card raises
/// `MetadataCmd::RequestDetail` and mounts the page empty (`metadata::request_detail`,
/// `pump_detail`). The measured cost of the difference was `results=50 ms` of a 62 ms frame,
/// filed in TV session 5 as "async landings" — it was the one call in the frame that was not.
///
/// Nothing else about the arm changes: the page is still pushed in this frame, on the catalog
/// row's art and title, exactly as a press does, and the content fills in a beat later through
/// the same landing every other opener uses. The scene now measures the cold MOUNT of a detail
/// page, which is what its name says and what a user experiences.
fn detail_arm(app: &mut App, fr: &mut Frame) -> bool {
    if !app.scenarios.detail_tried && fr.now.wrapping_sub(app.t0) > 500 {
        app.scenarios.detail_tried = true;
        if let Some(rk) = plx_base::devtrig::read("detail") {
            let rk = rk.as_str();
            if !rk.is_empty() {
                let sid = match crate::app::boot::direct_trigger_server() {
                    Ok(sid) => sid,
                    Err(_e) => {
                        #[cfg(feature = "devtriggers")]
                        plx_base::eventlog::log(&format!("plxnative-detail: refused: {_e}"));
                        return false;
                    }
                };
                app.bridge.metadata_mut().run(plx_data::stores::metadata::MetadataCmd::RequestDetail { sid, rk: rk.to_string() });
                #[cfg(feature = "devtriggers")]
                plx_base::eventlog::log(&format!("plxnative-detail: rk={rk} server={} start", sid.raw()));
                // A HARD CUT onto the page: at boot there is no outgoing screen to replace, so a
                // dip would fade the page up out of nothing and read as a slow app rather than a
                // navigated one. `push_detail` + `seed_node` in one call.
                crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge, sid, rk, None, None);
                app.scenarios.content_boot = Some(ContentBoot::new(sid, rk.to_string()));
            }
        }
    }
    true
}

/// `/tmp/plxnative-collection=<ratingKey>` — mount the Collection page through the same argument
/// a kind-4 card produces. The mock/server supplies the header and children asynchronously.
fn collection_arm(app: &mut App, fr: &mut Frame) -> bool {
    if app.scenarios.collection_tried || fr.now.wrapping_sub(app.t0) <= 500 { return true; }
    app.scenarios.collection_tried = true;
    let Some(rk) = plx_base::devtrig::read("collection").filter(|rk| !rk.is_empty()) else { return true };
    let sid = match crate::app::boot::direct_trigger_server() {
        Ok(sid) => sid,
        Err(_e) => {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!("plxnative-collection: refused: {_e}"));
            return false;
        }
    };
    crate::app::bridge::nav_push(&mut app.pages, AppArg::Content(
        plx_screens::registry::ContentArg::Collection(plx_plex::plex::collections::CollectionRef::by_rk(
            sid, &rk, 0, plx_platform::i18n::msg::browse_collection_kind()))));
    #[cfg(feature = "devtriggers")]
    plx_base::eventlog::log(&format!("plxnative-collection: rk={rk} server={} start", sid.raw()));
    true
}

/// `/tmp/plxnative-play=<rk>` — fetch that item and play its leaf, headless. TWO frames at least,
/// since phase 11: the request goes off-thread on the arming frame and the play is dispatched on
/// the frame its landing arrives.
///
/// It used to be one frame, on `MetadataCmd::LoadDetailNow` — the deliberately BLOCKING load —
/// because the very next statement reads `metadata::current()` to derive the leaf's part and
/// codecs. Two sequential PMS round trips on the SDL thread, inside the frame's `results` phase.
/// The wait is the same wait with the loop still running: `pump_detail` installs the landing
/// route-unconditionally, and this arm re-checks each frame that the item now published IS the
/// one it asked for — by SERVER and key, since two servers in one household both number from 1.
///
/// **The `start` line stays where it always was, at the DISPATCH**, not at the request. The
/// harness's offline cases key on it (`tests/run.py`'s `resolve_pin`: the IPv6 re-point must
/// PRECEDE `plxnative-play: … start`), and moving a line earlier is exactly the kind of change
/// that turns an ordering assertion into a coin toss. The request gets its own `… request` line,
/// which carries no `start` and so cannot be mistaken for one.
///
/// Two ways the wait ends without a play, both logged rather than silent: the request settles
/// (`detail_request_status` answers `Some(false)`) with something other than this item published —
/// a failed or refused fetch keeps the previous item — or 12 s pass, the same ceiling every other
/// arm here uses for "this boot never got where it was going".
fn play_arm(app: &mut App, fr: &mut Frame) -> bool {
    if !app.scenarios.play_tried
        && !matches!(app.route(), AppArg::Player | AppArg::Login | AppArg::Profiles)
        && fr.now.wrapping_sub(app.t0) > 500
    {
        app.scenarios.play_tried = true;
        if let Some(rk) = plx_base::devtrig::read("play") {
            let rk = rk.as_str();
            if !rk.is_empty() {
                let sid = match crate::app::boot::direct_trigger_server() {
                    Ok(sid) => sid,
                    Err(_e) => {
                        #[cfg(feature = "devtriggers")]
                        plx_base::eventlog::log(&format!("plxnative-play: refused: {_e}"));
                        return false;
                    }
                };
                app.bridge.metadata_mut().run(plx_data::stores::metadata::MetadataCmd::RequestDetail { sid, rk: rk.to_string() });
                #[cfg(feature = "devtriggers")]
                plx_base::eventlog::log(&format!("plxnative-play: rk={rk} server={} request", sid.raw()));
                app.scenarios.play_await = Some((sid, rk.to_string(), fr.now.wrapping_add(12_000)));
            }
        }
    }
    play_await_tick(app, fr);
    true
}

/// The landing half of [`play_arm`], run every frame while a request is outstanding.
fn play_await_tick(app: &mut App, fr: &mut Frame) {
    use plx_screens::player::input::HUD_LINGER_MS;
    let Some((sid, rk, deadline)) = app.scenarios.play_await.clone() else { return };
    if matches!(app.route(), AppArg::Player) {
        app.scenarios.play_await = None;
        return;
    }
    let leaf = app.bridge.metadata_view().current()
        .filter(|d| plx_plex::plex::same_item((d.sid, &d.rk), (sid, &rk)))
        .map(|d| {
            if !d.part.is_empty() {
                (d.part.clone(), d.vcodec.clone(), d.acodec.clone(), d.title.clone(), d.resume_ms, d.dur_ms)
            } else if let Some(ep) = d.episodes.first() {
                (ep.part.clone(), ep.vcodec.clone(), ep.acodec.clone(), d.title.clone(), ep.resume_ms, ep.dur_ms)
            } else {
                (String::new(), String::new(), String::new(), d.title.clone(), 0, 0)
            }
        });
    let Some((part, vc, ac, title, resume_ms, dur_ms)) = leaf else {
        // nothing published for this item yet. Give up when the request itself has settled with
        // something else in place (a failed fetch keeps the previous item), or on the ceiling.
        let settled = app.bridge.metadata_view().detail_request_status(sid, &rk) == Some(false);
        let expired = fr.now.wrapping_sub(deadline) < u32::MAX / 2;
        if settled || expired {
            app.scenarios.play_await = None;
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!(
                "plxnative-play: rk={rk} server={} — no detail landed ({})",
                sid.raw(),
                if settled { "the fetch settled without it" } else { "12s" }
            ));
        }
        return;
    };
    app.scenarios.play_await = None;
    if part.is_empty() {
        #[cfg(feature = "devtriggers")]
        plx_base::eventlog::log(&format!("plxnative-play: rk={rk} server={} — nothing playable on it", sid.raw()));
        return;
    }
    #[cfg(feature = "devtriggers")]
    plx_base::eventlog::log(&format!("plxnative-play: rk={rk} server={} start", sid.raw()));
    if plx_media::route::request_play(&mut app.player.session, app.bridge.metadata_mut(), sid, &rk, &part, &vc, &ac, &title, "") {
        let resume = plx_data::metadata::resume_ns(resume_ms, dur_ms);
        crate::app::playback::start_playback(&mut app.player.session,
            &mut app.adapters.player,
            resume,
            crate::app::playback::Origin::Here,
            HUD_LINGER_MS,
            None,
            &mut app.pages,
            &mut app.bridge,
        );
    }
}

fn autoseek_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.seek_tried
        && matches!(app.route(), AppArg::Player)
        && crate::app::playback::dur() > 0
        && fr.now.wrapping_sub(app.t0) > 12000
    {
        app.scenarios.seek_tried = true;
        if let Some(s) = plx_base::devtrig::read("autoseek") {
            let mut steps: Vec<String> = s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
            let mut first_delay_ms = 0u32;
            loop {
                let Some(head) = steps.first().cloned() else { break };
                if let Some(g) = head.strip_prefix("gap=") {
                    app.scenarios.seek_gap_ms = g.parse().unwrap_or(300).max(50);
                } else if let Some(d) = head.strip_prefix("delay=") {
                    first_delay_ms = d.parse().unwrap_or(0);
                } else {
                    break;
                }
                steps.remove(0);
            }
            if steps.is_empty() {
                steps.push("140".to_string());
            }
            app.scenarios.seek_script_last = plx_media::player::playpos_ns();
            app.scenarios.seek_script_at = fr.now.wrapping_sub(app.scenarios.seek_gap_ms).wrapping_add(first_delay_ms);
            app.scenarios.seek_script = steps;
        }
    }
    if !app.scenarios.seek_script.is_empty()
        && matches!(app.route(), AppArg::Player)
        && script_step_due(fr.now, app.scenarios.seek_script_at, app.scenarios.seek_gap_ms)
    {
        let step = app.scenarios.seek_script.remove(0);
        app.scenarios.seek_script_at = fr.now;
        let t = if let Some(r) = step.strip_prefix('+') {
            app.scenarios.seek_script_last + r.parse::<i64>().unwrap_or(0) * 1_000_000_000
        } else if let Some(r) = step.strip_prefix('-') {
            app.scenarios.seek_script_last - r.parse::<i64>().unwrap_or(0) * 1_000_000_000
        } else {
            step.parse::<i64>().unwrap_or(140) * 1_000_000_000
        }.max(0);
        app.scenarios.seek_script_last = t;
        plx_base::eventlog::log(&format!("autoseek: step → {}s ({} left)", t / 1_000_000_000, app.scenarios.seek_script.len()));
        crate::app::playback::request_seek(t);
    }
}

fn qualityswitch_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.quality_tried {
        const QUALITY_SWITCH_OBSERVE_MS: u32 = 12_000;
        let playing = matches!(app.route(), AppArg::Player)
            && crate::app::playback::dur() > 0
            && plx_media::player::is_playing(&app.player.session);
        if !playing {
            app.scenarios.quality_playing_since = None;
        } else {
            let since = *app.scenarios.quality_playing_since.get_or_insert(fr.now);
            if fr.now.wrapping_sub(since) >= QUALITY_SWITCH_OBSERVE_MS {
                app.scenarios.quality_tried = true;
                if let Some((gap, qs)) = super::quality_switch_script() {
                    app.scenarios.quality_gap_ms = gap;
                    app.scenarios.quality_script_at = fr.now.wrapping_sub(gap);
                    app.scenarios.quality_script = qs;
                }
            }
        }
    }
    if !app.scenarios.quality_script.is_empty()
        && matches!(app.route(), AppArg::Player)
        && script_step_due(fr.now, app.scenarios.quality_script_at, app.scenarios.quality_gap_ms)
    {
        let q = app.scenarios.quality_script.remove(0);
        app.scenarios.quality_script_at = fr.now;
        plx_base::eventlog::log(&format!("quality: switch → {} ({} left)", super::quality_wire_name(q), app.scenarios.quality_script.len()));
        plx_media::route::set_quality(&mut app.player.session, q);
    }
}

fn autopause_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.pause_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 6000 {
        app.scenarios.pause_tried = true;
        if let Some(script) = super::pause_script() {
            app.scenarios.pause_script =
                Some((fr.now.wrapping_add(script.delay_ms), script.hold_ms, script.at_ms));
        }
    }
    if let Some((pause_at, hold_ms, at_ms)) = app.scenarios.pause_script {
        // In the simulator the clock sink is also told to stop on `at` exactly, so the pause
        // (accepted a little after the gate opens) freezes that position rather than wherever
        // scheduling had got to: the same frame and clock every run (`ffi_host.rs::stop_clock_at`).
        // Re-armed every frame until accepted: a Load in between rebases the fed timeline, and
        // the stop is kept in movie time, so re-arming is idempotent.
        #[cfg(feature = "hostsim")]
        if let Some(ms) = at_ms {
            plx_media::player::stop_sim_clock_at(Some(i64::from(ms) * 1_000_000));
        }
        let reached = at_ms.is_none_or(|ms| crate::app::playback::playpos() >= i64::from(ms) * 1_000_000);
        if matches!(app.route(), AppArg::Player) && script_step_due(fr.now, pause_at, 0) && reached {
            if crate::app::lifecycle::set_transport_paused(&mut app.adapters.player, true) {
                plx_base::eventlog::log(&format!(
                    "autopause: Pause accepted hold={}ms",
                    hold_ms.map_or_else(|| "forever".to_string(), |ms| ms.to_string()),
                ));
                app.scenarios.pause_script = None;
                app.scenarios.pause_resume_at = hold_ms.map(|hold| fr.now.wrapping_add(hold));
                #[cfg(feature = "hostsim")]
                plx_media::player::stop_sim_clock_at(None);
                pin_headless_hud(app, fr.now, None);
            }
        }
    }
    if let Some(resume_at) = app.scenarios.pause_resume_at {
        if matches!(app.route(), AppArg::Player) && script_step_due(fr.now, resume_at, 0) {
            if crate::app::lifecycle::set_transport_paused(&mut app.adapters.player, false) {
                plx_base::eventlog::log("autopause: Resume accepted");
                app.scenarios.pause_resume_at = None;
            }
        }
    }
}

fn menu_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.menu_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 6000 {
        app.scenarios.menu_tried = true;
        if let Some(t) = plx_base::devtrig::read("menu") {
            crate::app::bridge::open_player_overlay(&mut app.player.session,
                app.bridge.metadata_view(),
                &mut app.pages,
                plx_screens::player::overlay::OverlayKind::Tracks { tab: t.parse::<c_int>().unwrap_or(0) },
            );
            pin_headless_hud(app, fr.now, None);
        }
        if plx_base::devtrig::flag("more") {
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::More { quality: false });
            pin_headless_hud(app, fr.now, None);
        }
        if plx_base::devtrig::flag("info") {
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::Info);
            pin_headless_hud(app, fr.now, Some(0));
        }
        if plx_base::devtrig::flag("chapters") {
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::Chapters);
            pin_headless_hud(app, fr.now, Some(1));
        }
    }
}

/// `/tmp/plxnative-menupick=<tab>,<target>`: `target` is either an absolute `TableView` row
/// number (the original contract), on the Audio tab a NAMED target — `"boost"`/`"loudness"` —
/// resolved through the panel's own [`plx_appkit::track_menu::TrackRow`] identities
/// (`TrackMenuState::row_for_audio_target`), or on the Subtitles tab `"track:N"`, the N-th track in
/// page order (root tracks, then those behind Other languages), committed by its own index
/// (`TrackMenuState::sub_track_for_target` / `commit_sub_track`). A name survives a track-count change a hand-written
/// row number does not: `audio_enhancement_arm` below writes one instead of deriving the row
/// itself, which is the issue #266 PR4 fix this trigger inherits (see
/// `TrackMenuState::row_for_audio_target`'s doc for that history). An unrecognized name logs a
/// clear line and commits nothing, same shape as the existing "row already active" no-commit log.
fn menupick_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.menupick_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 7000 {
        app.scenarios.menupick_tried = true;
        if let Some(s) = plx_base::devtrig::read("menupick") {
            let mut it = s.split(',');
            let tab = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
            let target = it.next().map(|x| x.trim().to_string()).unwrap_or_else(|| "0".to_string());
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::Tracks { tab });
            app.scenarios.menupick_target = Some(target);
        }
    }
    if let Some(target) = app.scenarios.menupick_target.take() {
        let meta = app.bridge.metadata_view();
        match crate::app::bridge::player_overlay_mut(&mut app.pages) {
            Some(surface) => match surface.resolve_menupick_track(&target) {
                Some(i) => match surface.pick_sub_track(meta, i) {
                    Some(commit) => crate::app::playback::commit_track(&mut app.player.session, commit),
                    None => plx_base::eventlog::log(&format!("menupick: track {i} gave no commit")),
                },
                None => match surface.resolve_menupick_row(&target) {
                    Some(row) => match surface.pick_track_row(meta, row) {
                        Some(commit) => crate::app::playback::commit_track(&mut app.player.session, commit),
                        // The menu's own on_ok treats picking the already-active row as a no-op: no
                        // commit, no route transition line. Without this, a manifest case whose row
                        // no longer differs from the start pick (e.g. #210's file-default rule) fails
                        // downstream as "no route transition" with nothing pointing back at menupick.
                        None => plx_base::eventlog::log(&format!("menupick: row {row} already active — no commit")),
                    },
                    None => plx_base::eventlog::log(&format!("menupick: unknown target {target:?} — no commit")),
                },
            },
            None => app.scenarios.menupick_target = Some(target),
        }
    }
}

/// `/tmp/plxnative-submenuosc=<period_ms>`'s own state — see [`submenuosc_arm`].
#[derive(Debug, Default)]
pub(crate) struct SubmenuOsc {
    /// The period, read once when the arm first runs (`None` = not read yet, `Some(0)` = not armed).
    period: Option<u32>,
    /// The frame clock of the last key.
    last: u32,
    /// Where in [`submenuosc_next`]'s cycle the next key is.
    step: u8,
    /// Whether the Subtitles ROOT offered Other languages when last seen there (a pushed page's
    /// own form has no such row, so the root is the only place to ask).
    has_other: bool,
}

/// What one tick of the drill-in oscillator does: seat the cursor on a row (if any), then press `key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmenuStep {
    seat: Option<plx_appkit::track_menu::TrackRow>,
    key: Key,
    next: u8,
}

/// The oscillator's script, as a pure function of (cycle step, tab, page depth, has Other
/// languages): Subtitles root -> Style -> Size picker -> back -> back -> Other languages -> back ->
/// (LEFT at the root) the Audio tab -> (RIGHT) the Subtitles tab, and round again. A step whose
/// preconditions do not hold (the menu is somewhere the script did not expect, e.g. a key was
/// dropped) pops a page or restarts the cycle rather than pressing blind, so it re-syncs instead
/// of drifting. `Other languages` is skipped when the item has none.
fn submenuosc_next(step: u8, tab: c_int, depth: usize, has_other: bool) -> SubmenuStep {
    use plx_appkit::track_menu::{StyleField, TrackRow};
    let press = |seat: Option<TrackRow>, key: Key, next: u8| SubmenuStep { seat, key, next };
    let left = Key::Left;
    let right = Key::Right;
    match (step, tab, depth) {
        (0, 1, 0) => press(Some(TrackRow::Style), right, 1),
        (1, 1, 1) => press(Some(TrackRow::OpenField(StyleField::Size)), right, 2),
        (2, 1, 2) => press(None, left, 3),
        (3, 1, 1) => press(None, left, if has_other { 4 } else { 6 }),
        (4, 1, 0) => press(Some(TrackRow::OpenOther), right, 5),
        (5, 1, 1) => press(None, left, 6),
        (6, 1, 0) => press(None, left, 7),
        (7, 0, 0) => press(None, right, 0),
        // Off script: climb out of any page, then rejoin the cycle at the Subtitles root.
        (_, _, d) if d > 0 => press(None, left, step),
        (_, 0, _) => press(None, right, 0),
        _ => press(None, left, 0),
    }
}

/// `/tmp/plxnative-submenuosc=<period_ms>` — the device frame-time scene for the Tracks panel's
/// drill-in animation (panel resize + page slide). With the Subtitles menu open
/// (`plxnative-menu=1`) it presses one REAL key per period through the dispatcher
/// ([`submenuosc_next`]'s cycle), so every push, pop and tab switch runs the production handlers
/// and the spring they start. Shaped like `navosc`/`modalosc`: it never opens or dismisses
/// anything except that a menu dismissed under it (a stray key) is reopened on the Subtitles tab.
/// The period must exceed the slide's settle time (~0.5 s) to measure transitions, or be shorter
/// to measure interrupted ones; the manifest scene uses 900 ms.
fn submenuosc_arm(app: &mut App, fr: &mut Frame) {
    let period = *app.scenarios.submenu_osc.period.get_or_insert_with(|| {
        plx_base::devtrig::read("submenuosc").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0)
    });
    if period == 0 || !matches!(app.route(), AppArg::Player) || fr.now.wrapping_sub(app.t0) < 7000 {
        return;
    }
    plx_machine::idle::wake();
    if fr.now.wrapping_sub(app.scenarios.submenu_osc.last) < period {
        return;
    }
    app.scenarios.submenu_osc.last = fr.now;
    let probe = crate::app::bridge::player_overlay_mut(&mut app.pages).and_then(|s| s.tracks_probe());
    let Some((tab, depth, has_other)) = probe else {
        if !crate::app::bridge::player_overlay_up(&app.pages) {
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::Tracks { tab: 1 });
            pin_headless_hud(app, fr.now, None);
            app.scenarios.submenu_osc.step = 0;
        }
        return;
    };
    if tab == 1 && depth == 0 {
        app.scenarios.submenu_osc.has_other = has_other;
    }
    let step = submenuosc_next(app.scenarios.submenu_osc.step, tab, depth, app.scenarios.submenu_osc.has_other);
    if let Some(row) = step.seat {
        if let Some(surface) = crate::app::bridge::player_overlay_mut(&mut app.pages) {
            if !surface.seat_track_row(row) {
                plx_base::eventlog::log(&format!("submenuosc: no row {row:?} on tab {tab} depth {depth} — skipping the step"));
            }
        }
    }
    plx_base::eventlog::log(&format!("submenuosc: step {} -> {} tab={tab} depth={depth} key={:?}", app.scenarios.submenu_osc.step, step.next, step.key));
    app.scenarios.submenu_osc.step = step.next;
    app.inputs.extend(crate::app::bridge::script_key(step.key, Tick { ms: fr.now, dt_us: 0 }));
}

/// The More oscillator's script, a pure function of the page depth: on the root seat the Quality
/// row and press RIGHT (a push); on the Quality page press LEFT (a pop). Anything deeper climbs out.
fn moreosc_next(depth: usize) -> (bool, Key) {
    if depth == 0 { (true, Key::Right) } else { (false, Key::Left) }
}

/// `/tmp/plxnative-moreosc=<period_ms>` — the device frame-time scene for More's Quality drill-in
/// (panel resize + page slide), the same shape as [`submenuosc_arm`]: with More open
/// (`plxnative-more=1`) one REAL key per period through the dispatcher, alternating push and pop.
/// A More dismissed under it is reopened at the root.
fn moreosc_arm(app: &mut App, fr: &mut Frame) {
    let period = *app.scenarios.more_osc.period.get_or_insert_with(|| {
        plx_base::devtrig::read("moreosc").and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0)
    });
    if period == 0 || !matches!(app.route(), AppArg::Player) || fr.now.wrapping_sub(app.t0) < 7000 {
        return;
    }
    plx_machine::idle::wake();
    if fr.now.wrapping_sub(app.scenarios.more_osc.last) < period {
        return;
    }
    app.scenarios.more_osc.last = fr.now;
    let Some(depth) = crate::app::bridge::player_overlay_mut(&mut app.pages).and_then(|s| s.more_probe()) else {
        if !crate::app::bridge::player_overlay_up(&app.pages) {
            crate::app::bridge::open_player_overlay(&mut app.player.session, app.bridge.metadata_view(), &mut app.pages, plx_screens::player::overlay::OverlayKind::More { quality: false });
            pin_headless_hud(app, fr.now, None);
        }
        return;
    };
    let (seat, key) = moreosc_next(depth);
    if seat {
        let seated = crate::app::bridge::player_overlay_mut(&mut app.pages).is_some_and(|s| s.seat_more_quality());
        if !seated {
            // Nothing to push: say so, rather than let the scene grade a panel that never moved.
            plx_base::eventlog::log("moreosc: no Quality row");
            return;
        }
    }
    plx_base::eventlog::log(&format!("moreosc: depth={depth} key={key:?}"));
    app.inputs.extend(crate::app::bridge::script_key(key, Tick { ms: fr.now, dt_us: 0 }));
}

#[cfg(test)]
mod moreosc_script_tests {
    use super::moreosc_next;
    use plx_machine::machine::Key;

    #[test]
    fn the_root_pushes_the_quality_row_and_the_page_pops() {
        assert_eq!(moreosc_next(0), (true, Key::Right));
        assert_eq!(moreosc_next(1), (false, Key::Left));
        assert_eq!(moreosc_next(3), (false, Key::Left), "off script: climb out, never press blind");
    }
}

/// `/tmp/plxnative-subtiming`'s own state across frames — see [`subtiming_arm`].
#[derive(Debug, Default)]
pub(crate) struct Subtiming {
    /// `true` once stage 1 (the subtitle commit) has fired, so a later frame does not commit a
    /// second time.
    tried: bool,
    /// Stage 1's commit, held until `route::cur_sub_sid` agrees — stage 2's own gate — then
    /// cleared once the capsule has been asked to open.
    pending: Option<SubtimingPending>,
}

/// A stage-1 commit stage 2 is waiting on.
#[derive(Debug, Clone, Copy)]
struct SubtimingPending {
    /// The committed stream id.
    sid: i64,
    /// The frame clock at which it was armed — stage 2's deadline: if `route::cur_sub_sid` has not
    /// agreed within ~3 s of this, the arm opens the capsule anyway rather than waiting on a commit
    /// that may never land (see `subtiming_arm`'s own doc for why a commit can silently not
    /// converge).
    armed_at: u32,
}

/// [`subtiming_arm`]'s pure stage-2 decision: given the frame clock, when stage 1 armed (committed
/// or, on the fused fast path, opened directly), the session's live `route::cur_sub_sid`, and the
/// stream id stage 1 wants, what should this frame do?
///
/// [`SubtimingStep::Open`] once the commit has visibly landed; [`SubtimingStep::OpenMismatch`]
/// once 3 s have passed without it landing — a bound, not a guess: a retry can reset track
/// selection underneath this commit, or a transcode's PUT can simply never resolve, and a scene
/// that waits forever for `cur_sub_sid` to agree fails as "the arm logged nothing", with no way to
/// tell stage 1 even fired. Opening late (mismatched) is still useful evidence; never opening is
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubtimingStep {
    Wait,
    Open,
    OpenMismatch,
}

const SUBTIMING_DEADLINE_MS: u32 = 3_000;

fn subtiming_step(now: u32, armed_at: u32, cur_sid: i64, want_sid: i64) -> SubtimingStep {
    if cur_sid == want_sid {
        SubtimingStep::Open
    } else if now.wrapping_sub(armed_at) > SUBTIMING_DEADLINE_MS {
        SubtimingStep::OpenMismatch
    } else {
        SubtimingStep::Wait
    }
}

/// `/tmp/plxnative-subtiming` — the Timing capsule's own headless trigger (plan
/// `subtitle-menu-capsule` §5), self-contained rather than riding `plxnative-menupick`: that one
/// fires at 7000 ms and would stack a Tracks panel on top of the capsule this trigger wants alone.
///
/// **Stage 1**, once the item's PLAN has landed and it is not transcoding: pick the first embedded
/// TEXT subtitle, preferring English (on `movie_h264_ac3_1080p` that is index 2, the same track
/// `subtitle_text_srt` already selects, so the fixture server sees no new selection). The gate is
/// `route::cur_rk` being non-empty (the same "no plan yet" idiom `decision.rs` itself uses), NOT
/// `player::is_playing`: on this item the pipeline does not present its first decoded frame until
/// ~13 s into a 26 s scene, so gating on "playing" opened the capsule too late to clear warmup, or
/// — depending how long that particular run took to reach it — not at all. `route::cur_sub_sid` is
/// already meaningful the moment the plan lands (`decision.rs::apply_plan` sets it from the
/// resolved `/decision` verdict), long before a frame is on screen.
///
/// If the server already selected exactly this track (the `server-selected subtitle:` line —
/// `cur_sub_sid` already agrees), there is nothing to commit: skip straight to opening, with no
/// server write at all. Otherwise commit it exactly as `menupick_arm` commits a picked row, and arm
/// stage 2.
///
/// **Stage 2**, on a later frame: [`subtiming_step`] decides whether the commit has visibly landed
/// (open) or a 3 s deadline has passed without it (open anyway, logged as a mismatch) — see its own
/// doc for why an unbounded wait is not acceptable. No `pin_headless_hud` call in either path,
/// because the production capsule frame hides the HUD rather than pinning it
/// (`OverlayKind::hud_policy`, `PlayerScreen::set_hud_policy`).
fn subtiming_arm(app: &mut App, fr: &mut Frame) {
    use plx_data::metadata::sub_layout::is_image_sub_codec;
    if let Some(SubtimingPending { sid: want_sid, armed_at }) = app.scenarios.subtiming.pending {
        let cur_sid = plx_media::route::cur_sub_sid(&app.player.session);
        match subtiming_step(fr.now, armed_at, cur_sid, want_sid) {
            SubtimingStep::Wait => {}
            step => {
                app.scenarios.subtiming.pending = None;
                if step == SubtimingStep::OpenMismatch {
                    plx_base::eventlog::log(&format!(
                        "subtiming: opened (sid mismatch cur={cur_sid} want={want_sid})"
                    ));
                } else {
                    plx_base::eventlog::log("subtiming: opened");
                }
                open_timing(app);
            }
        }
        return;
    }
    if app.scenarios.subtiming.tried || !plx_base::devtrig::flag("subtiming") {
        return;
    }
    if !matches!(app.route(), AppArg::Player)
        || plx_media::route::cur_rk(&app.player.session).is_empty()
        || plx_media::route::is_transcoding(&app.player.session)
    {
        return;
    }
    let Some(item) = app.bridge.metadata_view().playing() else { return };
    let idx = item
        .subs
        .iter()
        .position(|s| !is_image_sub_codec(&s.codec) && s.lang_code == "eng")
        .or_else(|| item.subs.iter().position(|s| !is_image_sub_codec(&s.codec)));
    let Some(i) = idx else {
        plx_base::eventlog::log("subtiming: no text sub");
        app.scenarios.subtiming.tried = true;
        return;
    };
    let stream_id = item.subs[i].id;
    let render_ordinal = plx_data::metadata::sub_render_ordinal(&item.subs, i);
    app.scenarios.subtiming.tried = true;
    if plx_media::route::cur_sub_sid(&app.player.session) == stream_id {
        plx_base::eventlog::log(&format!("subtiming: already sid={stream_id} — opened without a commit"));
        open_timing(app);
        return;
    }
    plx_base::eventlog::log(&format!("subtiming: committed sid={stream_id}"));
    app.scenarios.subtiming.pending = Some(SubtimingPending { sid: stream_id, armed_at: fr.now });
    crate::app::playback::commit_track(
        &mut app.player.session,
        plx_appkit::track_menu::TrackCommit::Subtitle {
            render_ordinal,
            stream_id,
            sidecar_key: None,
            sidecar_codec: String::new(),
        },
    );
}

/// Present the Timing capsule on the player page — both of [`subtiming_arm`]'s open paths.
fn open_timing(app: &mut App) {
    crate::app::bridge::open_player_overlay(
        &app.player.session,
        app.bridge.metadata_view(),
        &mut app.pages,
        plx_screens::player::overlay::OverlayKind::Timing,
    );
}

#[cfg(test)]
mod submenuosc_script_tests {
    use super::submenuosc_next;
    use plx_machine::machine::Key;

    /// Follow the script against a model of the menu (RIGHT on a seated Nav row pushes, LEFT pops
    /// or, at the root, goes to Audio, RIGHT on Audio goes back): it must exercise a push, a pop
    /// and both tab switches, and return to where it began.
    fn walk(has_other: bool) -> (Vec<(i32, usize)>, u8) {
        let (mut step, mut tab, mut depth) = (0u8, 1i32, 0usize);
        let mut seen = Vec::new();
        for _ in 0..16 {
            let s = submenuosc_next(step, tab, depth, has_other);
            match (s.key, s.seat.is_some()) {
                (Key::Right, true) => depth += 1,
                (Key::Right, false) => tab = 1,
                (Key::Left, _) if depth > 0 => depth -= 1,
                (Key::Left, _) => tab = 0,
                _ => unreachable!(),
            }
            step = s.next;
            seen.push((tab, depth));
            if step == 0 && tab == 1 && depth == 0 {
                return (seen, step);
            }
        }
        (seen, step)
    }

    #[test]
    fn the_cycle_pushes_pops_switches_tabs_and_comes_back_to_the_root() {
        let (seen, step) = walk(true);
        assert_eq!(step, 0);
        assert!(seen.contains(&(1, 2)), "Style -> Size picker is reached: {seen:?}");
        assert!(seen.contains(&(0, 0)), "the Audio tab is visited: {seen:?}");
        assert_eq!(seen.last(), Some(&(1, 0)));
        let (without, _) = walk(false);
        assert!(without.len() < seen.len(), "no Other languages row, no Other languages leg");
    }

    #[test]
    fn an_off_script_menu_climbs_out_instead_of_pressing_blind() {
        // A page is open where the script wants the root: pop it, keep the step.
        let s = submenuosc_next(0, 1, 2, true);
        assert_eq!((s.key, s.seat, s.next), (Key::Left, None, 0));
        // Lost on the Audio tab at step 3: go back to Subtitles and restart.
        let s = submenuosc_next(3, 0, 0, true);
        assert_eq!((s.key, s.next), (Key::Right, 0));
    }
}

#[cfg(test)]
mod subtiming_step_tests {
    use super::{subtiming_step, SubtimingStep};

    #[test]
    fn opens_the_moment_cur_sub_sid_agrees() {
        assert_eq!(subtiming_step(1_000, 900, 2698, 2698), SubtimingStep::Open);
    }

    #[test]
    fn waits_before_the_deadline_while_it_disagrees() {
        assert_eq!(subtiming_step(1_000, 900, 0, 2698), SubtimingStep::Wait);
        assert_eq!(subtiming_step(3_899, 900, 0, 2698), SubtimingStep::Wait);
    }

    #[test]
    fn opens_mismatched_once_the_deadline_passes_without_agreement() {
        assert_eq!(subtiming_step(3_901, 900, 0, 2698), SubtimingStep::OpenMismatch);
    }

    #[test]
    fn agreement_wins_even_past_the_deadline() {
        assert_eq!(subtiming_step(9_000, 900, 2698, 2698), SubtimingStep::Open);
    }
}

fn marker_arm(app: &mut App, _fr: &mut Frame) {
    if !app.scenarios.marker_tried && matches!(app.route(), AppArg::Player) && plx_media::player::is_playing(&mut app.player.session) {
        match plx_base::devtrig::read("marker") {
            Some(s) => {
                let want = if s.eq_ignore_ascii_case("intro") {
                    plx_data::metadata::MarkerKind::Intro
                } else {
                    plx_data::metadata::MarkerKind::Credits
                };
                let meta = app.bridge.metadata_view();
                let markers = meta.playing_markers();
                if !markers.is_empty() {
                    app.scenarios.marker_tried = true;
                    if let Some(m) = markers.iter().find(|m| m.kind == want) {
                        let t = (m.start_ms - 5_000).max(0) * 1_000_000;
                        plx_base::eventlog::log(&format!("marker trigger: seek to {}s (5s before {:?})", t / 1_000_000_000, want));
                        crate::app::playback::request_seek(t);
                    } else {
                        plx_base::eventlog::log(&format!("marker trigger: item has no {want:?} marker"));
                    }
                }
            }
            None => app.scenarios.marker_tried = true,
        }
    }
}

/// Whether the replay-after-EOS arm should re-arm the next boot iteration. `handed_off_to_up_next`
/// is `finish_playback`'s own return — the only live signal that this EOS was a real exit rather
/// than an Up Next handoff, since the exit's `PopTo` only PARKS the navigation (applied at the
/// next commit) and `app.route()` still reads `Player` for the rest of the frame either way.
fn should_replay_after_eos(handed_off_to_up_next: bool, replay_left: u32, playurl_flag: bool) -> bool {
    !handed_off_to_up_next && replay_left > 0 && playurl_flag
}

/// `/tmp/plxnative-replay[=N]` — REPLAY AFTER COMPLETION (LG App Self Checklist #46). Called from
/// `app::run::playback_tick` right after `finish_playback` has left the player on a real EOS;
/// `handed_off_to_up_next` is that call's own return value, telling an Up Next handoff apart from
/// a real exit (see [`should_replay_after_eos`]). Re-arming `auto_tried` sends the next frame back
/// through the `playurl` entry, which calls `route::clear_url()` and lets `start_bufferfeed` read
/// the trigger again. The trigger is read once at boot (`replay_left`), so this cannot become an
/// endless loop from a file appearing mid-run, and `devtrig::flag` is `false` at COMPILE time in a
/// release build.
pub(crate) fn maybe_replay_after_eos(app: &mut App, handed_off_to_up_next: bool) {
    if should_replay_after_eos(handed_off_to_up_next, app.scenarios.replay_left, plx_base::devtrig::flag("playurl")) {
        app.scenarios.replay_left -= 1;
        app.scenarios.auto_tried = false;
        plx_base::eventlog::log(&format!(
            "replay: starting the finished stream again ({} left)", app.scenarios.replay_left
        ));
    }
}

#[cfg(test)]
mod replay_after_eos_tests {
    use super::should_replay_after_eos;

    #[test]
    fn an_up_next_handoff_never_rearms_the_replay_even_with_budget_and_flag() {
        assert!(!should_replay_after_eos(true, 3, true));
    }

    #[test]
    fn a_real_exit_rearms_only_with_budget_left_and_the_flag_set() {
        assert!(should_replay_after_eos(false, 1, true));
        assert!(!should_replay_after_eos(false, 0, true), "no budget left");
        assert!(!should_replay_after_eos(false, 1, false), "trigger not armed");
    }
}

/// The boot-trigger SCRIPTS (autoplay, grid, settings, press, itemmenu, detail, play, seek,
/// quality, pause, menu, menupick, subtiming, marker), called once per iteration from
/// `app::run::run` at exactly the position `dev_scripts` occupied. `false` propagates a refused
/// trigger (an invalid
/// `plxnative-server` slot) — the loop `continue`s exactly as it always did, skipping the rest of
/// this frame's arms and phases alike.
pub(crate) unsafe fn each_frame(app: &mut App, fr: &mut Frame) -> bool {
    #[cfg(feature = "devtriggers")]
    poster_gate::tick(app, fr.now);
    autoplay_arm(app, fr);
    grid_library_search_heroidx_arm(app, fr);
    settings_boot_arm(app, fr);
    press_arm(app, fr);
    itemmenu_arm(app, fr);
    acct_arm(app, fr);
    screenshot::libtype_arm(app, fr);
    screenshot::libgrid_arm(app, fr);
    screenshot::libshelf_arm(app, fr);
    screenshot::libmenu_arm(app, fr);
    screenshot::clockstop_arm(app, fr);
    if !detail_arm(app, fr) {
        return false;
    }
    if !collection_arm(app, fr) {
        return false;
    }
    if !play_arm(app, fr) {
        return false;
    }
    autoseek_arm(app, fr);
    qualityswitch_arm(app, fr);
    autopause_arm(app, fr);
    menu_arm(app, fr);
    menupick_arm(app, fr);
    subtiming_arm(app, fr);
    submenuosc_arm(app, fr);
    moreosc_arm(app, fr);
    marker_arm(app, fr);
    if crate::app::bridge::player(&app.pages).is_some() {
        plx_media::player::failure_fixture(&mut app.player.session);
    }
    true
}

/// Typed controlled-bootstrap scenarios. Ordinary arms may read live trigger files; replay may
/// execute only values restored into the App from its validated initial input.
pub(crate) fn controlled_each_frame(app: &mut App, fr: &mut Frame) -> bool {
    settings_boot_arm(app, fr);
    // A cold font/GL boot can spend more than 500 ms before the first dispatcher frame.
    // Do not consume the one-shot while Root(Home) is still queued: its first commit would
    // replace the Detail request and leave the controlled content flow on Home forever.
    if !app.scenarios.detail_tried && app.pages.top_screen().is_some()
        && fr.now.wrapping_sub(app.t0) > 500 {
        app.scenarios.detail_tried = true;
        if let Some(input) = app.boot_initial.as_ref().and_then(|init| init.content.as_ref()) {
            let sid = plx_plex::plex::ServerId::from_raw(0);
            app.scenarios.content_boot = Some(ContentBoot::controlled(sid, input));
            crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge, sid, &input.detail, None, None);
        }
    }
    true
}

// =================================================================================================
// oscillator continuations — the CONTENT of each `if app.dev.<x> { … }` block that used to be
// inline in `app/run.rs`'s `update`/`land_results`/`heartbeat` phases. The surrounding skeleton
// (`page_of`/`host_page_updates`, which page owns focus, which phase runs) stays in `run.rs`: it
// governs production per-page work too (`update_home_chrome`) and is not itself a dev arm.
// =================================================================================================

/// `/tmp/plxnative-pickuser=<index>` — auto-select that roster tile once the who's-watching
/// picker is up. Called from `app::run::update` at the position the arm always occupied.
pub(crate) fn pickuser_tick(app: &mut App) {
    if !(matches!(app.route(), AppArg::Profiles)
        && app.scenarios.pick_user.is_some()
        && app.bridge.auth_read().0.phase == plx_session::auth::Phase::Profiles
        && !app.bridge.auth_read().0.users.is_empty())
    {
        return;
    }
    let idx = app.scenarios.pick_user.take().unwrap();
    // **Ask the SAME question `screens::profiles::ProfilesScreen::select` asks before acting**,
    // because this call site cannot reach that method to ask it FOR us — see the phase-9 report
    // this arm's comment used to carry for the full account of why a protected roster index
    // refuses here rather than attempting a PIN-less switch plex.tv would refuse anyway.
    let protected = app.bridge.auth_read().0.users.get(idx).map(|u| u.protected).unwrap_or(false);
    if protected {
        plx_base::eventlog::log(&format!(
            "pickuser: roster index {idx} is PROTECTED — refusing rather than attempting \
             a PIN-less switch plex.tv would refuse anyway; this trigger has no door onto \
             the owned picker's own PIN pad yet"
        ));
    } else {
        plx_base::eventlog::log(&format!("pickuser: auto-selecting roster index {idx}"));
        crate::app::bridge::execute_session_command(&mut app.pages,
            plx_session::auth::SessionCmd::SelectProfile { index: idx, pin: None });
    }
}

/// `/tmp/plxnative-navosc` — bounce the route Home↔Library (or Home↔a named detail page) on a
/// timer. Called from `app::run::land_results` at the position the arm always occupied.
pub(crate) fn nav_osc_tick(app: &mut App, now: u32) {
    use plx_screens::registry::HomeTab;
    if app.scenarios.dev.nav_osc && now.wrapping_sub(app.scenarios.nav_osc_last) > 1400 {
        app.scenarios.nav_osc_last = now;
        match app.route() {
            AppArg::Home if !app.scenarios.dev.nav_osc_rk.is_empty() => {
                let rk = app.scenarios.dev.nav_osc_rk.clone();
                crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge,
                    plx_plex::plex::current_server(), &rk, None, None);
            }
            AppArg::Content(_) => crate::app::bridge::nav_pop(&mut app.pages),
            AppArg::Home => {
                if let Some(kind) = app.bridge.browse_directory().tab_kind(0) {
                    let tab = match kind {
                        plx_data::stores::browse::SecKind::Show => HomeTab::Shows,
                        _ => HomeTab::Movies,
                    };
                    crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge, tab, None, None);
                }
            }
            AppArg::Library => {
                let origin = crate::app::chrome::pill_at(
                    app.bridge.browse_directory(), 1);
                crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge,
                    HomeTab::Home, Some(origin), None);
            }
            _ => {}
        }
    }
}

/// `/tmp/plxnative-heroosc` — perpetually page the real hero carousel.
pub(crate) fn hero_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.hero_osc && now.wrapping_sub(app.scenarios.hero_osc_last) > 700 {
        app.scenarios.hero_osc_last = now;
        app.bridge.home_command(HomeCmd::Flip(1));
    }
}

/// `/tmp/plxnative-homefoldosc` — alternate the real hero↔first-shelf snap.
pub(crate) fn home_fold_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.home_fold_osc && now.wrapping_sub(app.scenarios.home_fold_osc_last) > 700 {
        app.scenarios.home_fold_osc_last = now;
        if app.scenarios.home_fold_down {
            app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col: 0 });
        } else {
            app.bridge.home_command(HomeCmd::Hero);
        }
        app.scenarios.home_fold_down = !app.scenarios.home_fold_down;
    }
}

/// `/tmp/plxnative-homeosc` — sweep the home grid focus top↔bottom to reproduce scroll judder.
pub(crate) fn home_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.home_osc && now.wrapping_sub(app.scenarios.home_osc_last) > 350 {
        app.scenarios.home_osc_last = now;
        let sym = if (now / 3000) % 2 == 0 { plx_ui::consts::SDLK_DOWN } else { plx_ui::consts::SDLK_UP };
        app.inputs.extend(crate::app::bridge::script_key(
            if sym == plx_ui::consts::SDLK_DOWN { Key::Down } else { Key::Up },
            Tick { ms: now, dt_us: 0 }));
    }
}

/// `/tmp/plxnative-libosc` — the Library twin of `homeosc`.
pub(crate) fn lib_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.lib_osc && matches!(app.route(), AppArg::Library) && now.wrapping_sub(app.scenarios.lib_osc_last) > 350 {
        app.scenarios.lib_osc_last = now;
        crate::app::bridge::Bridge::library_command(&mut app.pages, plx_screens::registry::LibraryCmd::Sweep);
    }
}

/// `/tmp/plxnative-libswitch` — cycle EVERY Library switch on a timer.
pub(crate) fn lib_switch_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.lib_switch && matches!(app.route(), AppArg::Library) && now.wrapping_sub(app.scenarios.lib_switch_last) > 1400 {
        app.scenarios.lib_switch_last = now;
        crate::app::bridge::Bridge::library_command(&mut app.pages, plx_screens::registry::LibraryCmd::SwitchStep(app.scenarios.lib_switch_step));
        app.scenarios.lib_switch_step = app.scenarios.lib_switch_step.wrapping_add(1);
    }
}

/// `/tmp/plxnative-searchosc` — the Search twin of `homeosc`/`libosc`.
pub(crate) fn search_osc_tick(app: &mut App, now: u32) {
    if matches!(app.route(), AppArg::Search) && app.scenarios.dev.search_osc && now.wrapping_sub(app.scenarios.search_osc_last) > 350 {
        app.scenarios.search_osc_last = now;
        let sym = if (now / 3000) % 2 == 0 { plx_ui::consts::SDLK_DOWN } else { plx_ui::consts::SDLK_UP };
        app.inputs.extend(crate::app::bridge::script_key(
            if sym == plx_ui::consts::SDLK_DOWN { Key::Down } else { Key::Up },
            Tick { ms: now, dt_us: 0 }));
    }
}

/// `/tmp/plxnative-acctosc` — drive the profile menu's own TableView.
pub(crate) fn account_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.account_osc && crate::app::bridge::account_menu_up(&app.pages) {
        plx_machine::idle::wake();
        if now.wrapping_sub(app.scenarios.account_osc_last) > 520 {
            app.scenarios.account_osc_last = now;
            // The surface's own focus engine moves the selection now, so the oscillator presses a
            // KEY through the dispatcher (`search_osc_tick`'s shape) instead of reaching into a
            // module global's `TableView`.
            let down = app.scenarios.account_osc_down;
            app.scenarios.account_osc_down = !down;
            app.inputs.extend(crate::app::bridge::script_key(
                if down { Key::Down } else { Key::Up },
                Tick { ms: now, dt_us: 0 }));
        }
    }
}

/// `/tmp/plxnative-modalosc` (with `plxnative-settings=root`) — open/dismiss Settings every 1.5 s.
pub(crate) fn modal_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.modal_osc && app.scenarios.settings_tried && now.wrapping_sub(app.scenarios.modal_osc_last) > 1500 {
        app.scenarios.modal_osc_last = now;
        if crate::app::bridge::settings_up(&app.pages) {
            crate::app::bridge::dismiss_surfaces(&mut app.pages);
        } else {
            crate::app::bridge::open_settings(&mut app.pages);
        }
    }
}

// =================================================================================================
// stress-bench oscillators (`/tmp/plxnative-pushbench`, `/tmp/plxnative-modalbench`) — see
// `bench`'s module doc for the pure state machine both ticks below drive. Half-periods reuse
// `nav_osc`'s 1400 ms and `modal_osc`'s 1500 ms exactly: "never faster than a user could
// plausibly drive" (spec) is the same argument those two already settled.
// =================================================================================================

const PUSH_BENCH_PERIOD_MS: u32 = 1400;
const MODAL_BENCH_PERIOD_MS: u32 = 1500;
/// `DeepBench`'s Detail/Person legs are the same nav_push cost `PushBench`'s own Detail/Person
/// legs measure, so this reuses `PushBench`'s half-period rather than inventing a fourth number.
const DEEP_BENCH_PERIOD_MS: u32 = PUSH_BENCH_PERIOD_MS;

/// `/proc/self/status`'s `VmRSS`, in kB — best effort, `0` where the file does not exist (the
/// macOS simulator host). Not cached: a bench cycle is seconds apart, so one extra file read per
/// cycle is noise next to the frame-time work it is timed beside.
fn read_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// `tex=<live textures>/<live kB>` from `gfx::tex_ledger` — appended after `rss_kb=` (the
/// harness's `BENCH_RE` anchors on the fields before it), so a cycle line that shows RSS growing
/// also says whether GL textures are what grew.
fn tex_field() -> String {
    let (n, bytes) = plx_gfx::gfx::tex_ledger::totals();
    format!("tex={n}/{}", bytes / 1024)
}


/// Called once per iteration, after the frame's Swap phase is stamped (`app::run::report`, right
/// after `present_and_swap`) — the narrowest point either bench's accumulator can read this
/// frame's total. Gated on `presented`: an iteration the idle gate skipped drew nothing, so
/// counting it toward `frames` or `worst_ms` would grade an absent frame as a fast one, exactly
/// the reasoning `Instruments::frame_drop_line`'s own `worst` peak already uses.
pub(crate) fn bench_frame_tick(app: &mut App, presented: bool, now: u32) {
    if !presented {
        return;
    }
    let total = app.frame_last_ms();
    let interval = app.frame_present_interval_ms();
    if let Some(b) = app.scenarios.push_bench.as_mut() {
        bench::bench_note_frame(&mut b.clock, now, total, interval);
    }
    if let Some(b) = app.scenarios.modal_bench.as_mut() {
        bench::bench_note_frame(&mut b.clock, now, total, interval);
    }
    if let Some(b) = app.scenarios.deep_bench.as_mut() {
        bench::bench_note_frame(&mut b.clock, now, total, interval);
    }
    if let Some(b) = app.scenarios.hero_bench.as_mut() {
        bench::bench_note_frame(&mut b.clock, now, total, interval);
    }
}

/// The once-per-bench `bench: kind=<k> settled` line — how long boot took to go still before the
/// first press, or that the cap ran out and the bench started on a page that never did.
fn log_bench_settled(kind: &str, waited_ms: u32, capped: bool) {
    plx_base::eventlog::log(&format!(
        "bench: kind={kind} settled after_ms={waited_ms}{}",
        if capped { " capped=1 (the root page never went still; cycle 1 may include boot)" } else { "" }
    ));
}

/// Opens `target` through the real bridge/nav call the interactive press uses, and returns the
/// target ACTUALLY opened — `Person` falls back to `Library` when no cast data has landed yet.
fn push_bench_open(app: &mut App, target: bench::PushTarget) -> bench::PushTarget {
    use plx_screens::registry::{ContentArg, HomeTab};
    match target {
        bench::PushTarget::Detail => {
            let rk = app.scenarios.push_bench.as_ref().unwrap().rk.clone();
            crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge,
                plx_plex::plex::current_server(), &rk, None, None);
            bench::PushTarget::Detail
        }
        bench::PushTarget::Person => {
            let person = app.scenarios.push_bench.as_ref().unwrap().person.clone();
            if let Some((sid, key, guid, name, thumb)) = person {
                crate::app::bridge::nav_push(&mut app.pages,
                    AppArg::Content(ContentArg::Person { sid, key, guid, name, thumb }));
                bench::PushTarget::Person
            } else {
                let b = app.scenarios.push_bench.as_mut().unwrap();
                if !b.person_fallback_logged {
                    b.person_fallback_logged = true;
                    plx_base::eventlog::log("bench: push cycle wanted Person but no cast data has landed yet \
                        — opening Library instead this cycle");
                }
                push_bench_open(app, bench::PushTarget::Library)
            }
        }
        bench::PushTarget::Library => {
            if let Some(kind) = app.bridge.browse_directory().tab_kind(0) {
                let tab = match kind {
                    plx_data::stores::browse::SecKind::Show => HomeTab::Shows,
                    _ => HomeTab::Movies,
                };
                crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge, tab, None, None);
            }
            bench::PushTarget::Library
        }
    }
}

/// …and its close, through `nav_pop`/`nav_tab` exactly as `nav_osc_tick`'s reverse leg does.
fn push_bench_close(app: &mut App, opened: bench::PushTarget) {
    use plx_screens::registry::HomeTab;
    match opened {
        bench::PushTarget::Detail | bench::PushTarget::Person => {
            crate::app::bridge::nav_pop(&mut app.pages);
        }
        bench::PushTarget::Library => {
            let origin = crate::app::chrome::pill_at(app.bridge.browse_directory(), 1);
            crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge, HomeTab::Home, Some(origin), None);
        }
    }
}

/// Opportunistically refresh the Person target from whichever Detail item is CURRENT — mirrors
/// `screens::detail::cast::action`'s own logic (that module is private to `detail`, so this is
/// the same read through `Detail::credit`/`Cast::person_key` directly rather than a visibility
/// change to reach it) against the FIRST cast credit, the same one a card-row OK at index 0 would
/// open. Cheap relative to a bench cycle's own 1400 ms period, so it runs on every push-bench tick
/// rather than only around the Detail leg.
fn push_bench_refresh_person(app: &mut App) {
    let Some(d) = app.bridge.metadata_view().current() else { return };
    let Some(c) = d.credit(0) else { return };
    let key = c.person_key();
    if key.is_empty() {
        return;
    }
    let person = (d.sid, key, c.tag_key.clone(), c.tag.clone(), c.thumb.clone());
    if let Some(b) = app.scenarios.push_bench.as_mut() {
        b.person = Some(person);
    }
}

/// `/tmp/plxnative-pushbench` — see `bench`'s module doc. Called from `land_results` beside
/// `nav_osc_tick`, the same phase boundary every route-changing dev arm runs at.
pub(crate) fn push_bench_tick(app: &mut App, now: u32) {
    if app.scenarios.push_bench.is_none() {
        return;
    }
    push_bench_refresh_person(app);
    let step = {
        let b = app.scenarios.push_bench.as_mut().unwrap();
        bench::bench_advance(&mut b.clock, now, PUSH_BENCH_PERIOD_MS)
    };
    match step {
        bench::BenchStep::Nothing => {}
        bench::BenchStep::Start(cycle) => {
            let b = app.scenarios.push_bench.as_ref().unwrap();
            let target = b.targets[bench::bench_target_index(b.targets.len(), cycle)];
            let opened = push_bench_open(app, target);
            app.scenarios.push_bench.as_mut().unwrap().opened = opened;
        }
        bench::BenchStep::Settled(waited, capped) => log_bench_settled("push", waited, capped),
        bench::BenchStep::Settle(_) => {
            let opened = app.scenarios.push_bench.as_ref().unwrap().opened;
            push_bench_close(app, opened);
        }
        bench::BenchStep::Report(cycle) => {
            let b = app.scenarios.push_bench.as_ref().unwrap();
            let c = &b.clock;
            plx_base::eventlog::log(&format!(
                "bench: kind=push cycle={}/{} target={} worst_ms={:.1} frames={} dur_ms={} rss_kb={} {} {}",
                cycle + 1, c.n, b.opened.name(), c.worst_ms(), c.frames(),
                now.wrapping_sub(c.cycle_start), read_rss_kb(), tex_field(), c.fields(),
            ));
        }
        bench::BenchStep::Done(n) => {
            plx_base::eventlog::log(&format!("bench: kind=push done cycles={n}"));
            app.scenarios.push_bench = None;
        }
    }
}

/// Present `target` through the real bridge call the interactive press uses.
fn modal_bench_open(app: &mut App, target: bench::ModalTarget) {
    use plx_screens::registry::{ContentPanel, ItemMenuArg, ItemMenuKind};
    match target {
        bench::ModalTarget::Settings => crate::app::bridge::open_settings(&mut app.pages),
        bench::ModalTarget::AccountMenu => crate::app::bridge::open_account_menu(&mut app.pages),
        bench::ModalTarget::About => {
            if let Some(host) = app.pages.top_page() {
                crate::app::bridge::open_content_panel(&mut app.pages, host, None, ContentPanel::About);
            }
        }
        bench::ModalTarget::ItemMenu => {
            let Some(host) = app.pages.nav.top_page().map(|e| e.id) else { return };
            let rk = app.scenarios.modal_bench.as_ref().unwrap().rk.clone();
            let sid = plx_plex::plex::current_server();
            let anchor_rect = plx_screens::item_menu::fallback_anchor();
            let arg = ItemMenuArg {
                sid,
                rk: rk.clone(),
                kind: ItemMenuKind::Card {
                    row: Box::new(plx_data::pms::PmsMovie { sid, rk, ..Default::default() }),
                    from_deck: false,
                },
                host,
                focus: None,
                anchor: [anchor_rect.x.to_bits(), anchor_rect.y.to_bits(), anchor_rect.w.to_bits(), anchor_rect.h.to_bits()],
                loaded_episode: false,
                from_home: true,
            };
            crate::app::bridge::open_item_menu(&mut app.pages, arg);
        }
    }
}

/// `/tmp/plxnative-modalbench` — see `bench`'s module doc. Called from `update` beside
/// `modal_osc_tick`, the same phase boundary every Settings-family dev arm runs at. Dismissal is
/// the single generic `dismiss_surfaces` every modal style already shares (`modal_osc_tick`'s own
/// reverse leg): a Compact/Sheet/Alert surface is dismissed the same way regardless of which one
/// is up.
pub(crate) fn modal_bench_tick(app: &mut App, now: u32) {
    if app.scenarios.modal_bench.is_none() {
        return;
    }
    let step = {
        let b = app.scenarios.modal_bench.as_mut().unwrap();
        bench::bench_advance(&mut b.clock, now, MODAL_BENCH_PERIOD_MS)
    };
    match step {
        bench::BenchStep::Nothing => {}
        bench::BenchStep::Start(cycle) => {
            let b = app.scenarios.modal_bench.as_ref().unwrap();
            let target = b.targets[bench::bench_target_index(b.targets.len(), cycle)];
            modal_bench_open(app, target);
        }
        bench::BenchStep::Settled(waited, capped) => log_bench_settled("modal", waited, capped),
        bench::BenchStep::Settle(_) => crate::app::bridge::dismiss_surfaces(&mut app.pages),
        bench::BenchStep::Report(cycle) => {
            let b = app.scenarios.modal_bench.as_ref().unwrap();
            let target = b.targets[bench::bench_target_index(b.targets.len(), cycle)];
            let c = &b.clock;
            plx_base::eventlog::log(&format!(
                "bench: kind=modal cycle={}/{} target={} worst_ms={:.1} frames={} dur_ms={} rss_kb={} {} {}",
                cycle + 1, c.n, target.name(), c.worst_ms(), c.frames(),
                now.wrapping_sub(c.cycle_start), read_rss_kb(), tex_field(), c.fields(),
            ));
        }
        bench::BenchStep::Done(n) => {
            plx_base::eventlog::log(&format!("bench: kind=modal done cycles={n}"));
            app.scenarios.modal_bench = None;
        }
    }
}

// =================================================================================================
// deep bench (`/tmp/plxnative-deepbench`) — see `bench`'s module doc for why each cycle here is
// one nav op, not a round trip, and why `Library` never appears in its rotation.
// =================================================================================================

/// Opportunistically refresh the Person target from whichever Detail item is CURRENT — the exact
/// twin of `push_bench_refresh_person`, kept separate because it writes into `deep_bench` rather
/// than `push_bench` (both benches may be armed at once, on different triggers).
fn deep_bench_refresh_person(app: &mut App) {
    let Some(d) = app.bridge.metadata_view().current() else { return };
    let Some(c) = d.credit(0) else { return };
    let key = c.person_key();
    if key.is_empty() {
        return;
    }
    let person = (d.sid, key, c.tag_key.clone(), c.tag.clone(), c.thumb.clone());
    if let Some(b) = app.scenarios.deep_bench.as_mut() {
        b.person = Some(person);
    }
}

/// Opens `target` through the same bridge/nav call `push_bench_open`'s Detail/Person arms use,
/// and returns the target ACTUALLY opened. **`Person` falls back to `Detail`, not `Library`**:
/// unlike `PushBench` (free to fall back to a peer swap because it closes back to depth 1 every
/// cycle regardless), this bench must grow by exactly one entry every push step, and only
/// `Detail`/`Person` do that (`bench::DeepBench::targets`'s doc) — re-pushing the SAME item this
/// step's Detail leg would have used keeps the depth invariant while still being a page a real
/// user's own back-chain could produce (an item linking to itself through a cast credit, or
/// simply pressed twice).
fn deep_bench_open(app: &mut App, target: bench::PushTarget) -> bench::PushTarget {
    use plx_screens::registry::ContentArg;
    match target {
        bench::PushTarget::Detail => {
            let rk = app.scenarios.deep_bench.as_ref().unwrap().rk.clone();
            crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge,
                plx_plex::plex::current_server(), &rk, None, None);
            bench::PushTarget::Detail
        }
        bench::PushTarget::Person => {
            let person = app.scenarios.deep_bench.as_ref().unwrap().person.clone();
            if let Some((sid, key, guid, name, thumb)) = person {
                crate::app::bridge::nav_push(&mut app.pages,
                    AppArg::Content(ContentArg::Person { sid, key, guid, name, thumb }));
                bench::PushTarget::Person
            } else {
                let b = app.scenarios.deep_bench.as_mut().unwrap();
                if !b.person_fallback_logged {
                    b.person_fallback_logged = true;
                    plx_base::eventlog::log("bench: deep push step wanted Person but no cast data has landed \
                        yet — re-pushing Detail instead this step (Library is not a safe fallback \
                        here, see DeepBench::targets's doc)");
                }
                deep_bench_open(app, bench::PushTarget::Detail)
            }
        }
        bench::PushTarget::Library => unreachable!("DeepBench::targets never includes Library"),
    }
}

/// The pop half's single generic close. `Detail`/`Person` are both ordinary stacking pages, so one
/// `nav_pop` (`push_bench_close`'s own Detail/Person arm) is the whole story — there is no
/// Library-shaped tab-close counterpart because Library never opens here.
fn deep_bench_close(app: &mut App) {
    crate::app::bridge::nav_pop(&mut app.pages);
}

/// `/tmp/plxnative-deepbench` — see `bench`'s module doc. Called from `land_results` beside
/// `push_bench_tick`, the same phase boundary: this bench changes the page stack every step too.
pub(crate) fn deep_bench_tick(app: &mut App, now: u32) {
    if app.scenarios.deep_bench.is_none() {
        return;
    }
    deep_bench_refresh_person(app);
    let step = {
        let b = app.scenarios.deep_bench.as_mut().unwrap();
        bench::bench_advance(&mut b.clock, now, DEEP_BENCH_PERIOD_MS)
    };
    match step {
        bench::BenchStep::Nothing => {}
        bench::BenchStep::Start(cycle) => {
            let depth = app.scenarios.deep_bench.as_ref().unwrap().depth;
            let (dir, _) = bench::deep_step(depth, cycle);
            let opened = match dir {
                bench::DeepDir::Push => {
                    let b = app.scenarios.deep_bench.as_ref().unwrap();
                    let target = b.targets[bench::bench_target_index(b.targets.len(), cycle)];
                    let opened = deep_bench_open(app, target);
                    app.scenarios.deep_bench.as_mut().unwrap().stack.push(opened);
                    opened
                }
                bench::DeepDir::Pop => {
                    deep_bench_close(app);
                    app.scenarios.deep_bench.as_mut().unwrap().stack.pop()
                        .unwrap_or(bench::PushTarget::Detail)
                }
            };
            let b = app.scenarios.deep_bench.as_mut().unwrap();
            b.dir = dir;
            b.opened = opened;
        }
        bench::BenchStep::Settled(waited, capped) => log_bench_settled("deep", waited, capped),
        bench::BenchStep::Settle(cycle) => {
            let b = app.scenarios.deep_bench.as_ref().unwrap();
            let c = &b.clock;
            plx_base::eventlog::log(&format!(
                "bench: kind=deep cycle={}/{} target={} dir={} depth={} worst_ms={:.1} \
                 frames={} dur_ms={} rss_kb={} {}",
                cycle + 1, c.n, b.opened.name(), b.dir.name(), b.stack.len(), c.worst_ms(),
                c.frames(), now.wrapping_sub(c.cycle_start), read_rss_kb(), c.fields(),
            ));
        }
        // A one-way clock never reports a round trip.
        bench::BenchStep::Report(_) => {}
        bench::BenchStep::Done(n) => {
            plx_base::eventlog::log(&format!("bench: kind=deep done cycles={n} rss_root_kb={}", read_rss_kb()));
            app.scenarios.deep_bench = None;
        }
    }
}

/// `/tmp/plxnative-herobench` — see [`bench::HeroBench`]: `n` cycles of two flips of the real Home
/// hero carousel through the same `HomeCmd::Flip` the Left/Right key lands on, walking three titles
/// and back. Called from `land_results` beside the other bench ticks. Each half of the round-trip
/// clock is one flip, measured to its settle point.
pub(crate) fn hero_bench_tick(app: &mut App, now: u32) {
    let Some(period) = app.scenarios.hero_bench.as_ref().map(|b| b.period_ms) else { return };
    let step = {
        let b = app.scenarios.hero_bench.as_mut().unwrap();
        bench::bench_advance(&mut b.clock, now, period)
    };
    match step {
        bench::BenchStep::Nothing => {}
        bench::BenchStep::Settled(waited, capped) => log_bench_settled("hero", waited, capped),
        bench::BenchStep::Start(cycle) | bench::BenchStep::Settle(cycle) => {
            app.bridge.home_command(HomeCmd::Flip(bench::hero_flip_dir(cycle)));
        }
        bench::BenchStep::Report(cycle) => {
            let b = app.scenarios.hero_bench.as_ref().unwrap();
            let c = &b.clock;
            plx_base::eventlog::log(&format!(
                "bench: kind=hero cycle={}/{} target={} worst_ms={:.1} frames={} dur_ms={} rss_kb={} {} {}",
                cycle + 1, c.n, if bench::hero_flip_dir(cycle) > 0 { "next" } else { "prev" },
                c.worst_ms(), c.frames(), now.wrapping_sub(c.cycle_start), read_rss_kb(),
                tex_field(), c.fields(),
            ));
        }
        bench::BenchStep::Done(n) => {
            plx_base::eventlog::log(&format!("bench: kind=hero done cycles={n}"));
            app.scenarios.hero_bench = None;
        }
    }
}

/// `/tmp/plxnative-legaldoc` (with `plxnative-settings=legal`) — one OK on the Legal index.
pub(crate) fn legal_doc_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.legal_doc
        && !app.scenarios.legal_doc_tried
        && crate::app::bridge::surface_word(&app.pages) == Some(plx_screens::registry::word::LEGAL)
    {
        app.scenarios.legal_doc_tried = true;
        let tick = Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 };
        app.inputs.extend(crate::app::bridge::script_key(Key::Ok, tick));
    }
}

/// `/tmp/plxnative-alert` (with `plxnative-settings=privacy`) — walk to and open the decision alert.
pub(crate) fn alert_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.alert_boot
        && !app.scenarios.alert_tried
        && crate::app::bridge::surface_word(&app.pages) == Some(plx_screens::registry::word::PRIVACY)
    {
        const ALERT_WALK: u8 = 16;
        let key = if app.scenarios.alert_step < ALERT_WALK {
            app.scenarios.alert_step += 1;
            Key::Down
        } else {
            app.scenarios.alert_tried = true;
            Key::Ok
        };
        let tick = Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 };
        app.inputs.extend(crate::app::bridge::script_key(key, tick));
    }
}

/// `/tmp/plxnative-settingsosc` — hold Settings open under a continuous focus sweep.
pub(crate) fn settings_osc_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.settings_osc && crate::app::bridge::settings_up(&app.pages) {
        plx_machine::idle::wake();
        if now.wrapping_sub(app.scenarios.settings_osc_last) > 520 {
            app.scenarios.settings_osc_last = now;
            let key = if app.scenarios.settings_osc_down { Key::Down } else { Key::Up };
            app.scenarios.settings_osc_down = !app.scenarios.settings_osc_down;
            let tick = Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 };
            app.inputs.extend(crate::app::bridge::script_key(key, tick));
        }
    }
}

/// `/tmp/plxnative-consentosc` — sweep the first-run consent question's focus.
pub(crate) fn consent_osc_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.consent_osc && crate::app::bridge::consent_up(&app.pages) {
        plx_machine::idle::invalidate();
        if now.wrapping_sub(app.scenarios.consent_osc_last) > 520 {
            app.scenarios.consent_osc_last = now;
            let key = if app.scenarios.consent_osc_down { Key::Down } else { Key::Up };
            app.scenarios.consent_osc_down = !app.scenarios.consent_osc_down;
            let tick = Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 };
            app.inputs.extend(crate::app::bridge::script_key(key, tick));
        }
    }
}

/// `/tmp/plxnative-onboardosc` — sweep the first-run sources editor's focus.
pub(crate) fn onboard_osc_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.onboard_osc && matches!(app.route(), AppArg::Onboard) {
        plx_machine::idle::invalidate();
        if now.wrapping_sub(app.scenarios.onboard_osc_last) > 520 {
            app.scenarios.onboard_osc_last = now;
            let key = if app.scenarios.onboard_osc_right { Key::Right } else { Key::Left };
            app.scenarios.onboard_osc_right = !app.scenarios.onboard_osc_right;
            let tick = Tick { ms: now, dt_us: (dt * 1_000_000.0) as u32 };
            app.inputs.extend(crate::app::bridge::script_key(key, tick));
        }
    }
}

/// `/tmp/plxnative-detailosc` — sweep the detail page's focus down↔up.
pub(crate) fn detail_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.detail_osc && matches!(app.route(), AppArg::Content(plx_screens::registry::ContentArg::Detail { .. })) {
        let key = if (now / 450) % 2 == 0 { Key::Down } else { Key::Up };
        app.inputs.extend(crate::app::bridge::script_key(key, Tick { ms: now, dt_us: 0 }));
    }
}

// =================================================================================================
// thin passthroughs — the trigger read is centralized here even though the reactive logic beside
// it is genuinely production code with one dev-only branch, or is the recorder/replay path this
// phase's instructions say to leave alone.
// =================================================================================================

/// `/tmp/plxnative-consent[=<crash|product>]` — forces either first-run purpose even on an
/// automated boot. Read by `app::input::maybe_ask_consent`, which stays production code with this
/// one dev-only branch: presenting the real consent screen is not itself a dev arm.
pub(crate) fn consent_override() -> Option<String> {
    plx_base::devtrig::read("consent")
}

/// `/tmp/plxnative-rec` — read by controlled-bootstrap preflight. The recorder/replay MECHANISM
/// stays in `app/recorder.rs` (this phase's instructions: it is not a scenario), but the raw
/// trigger read goes through the one door every other trigger does.
///
/// Compiled out, not merely guarded, in a release build: these three controlled-boot readers are
/// the recorder's whole `/tmp` surface, and a runtime `ENABLED` check still leaves the trigger
/// names in the binary's bytes, where `ci/check-package.py` grades them.
#[cfg(feature = "devtriggers")]
pub(crate) fn rec_trigger() -> Result<Option<String>, &'static str> {
    plx_ui::rec::mode_value(&plx_base::devtrig::path("rec")).map_err(|_| "invalid recorder trigger")
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn rec_trigger() -> Result<Option<String>, &'static str> {
    Ok(None)
}

/// `/tmp/plxnative-recplay` — see [`rec_trigger`].
#[cfg(feature = "devtriggers")]
pub(crate) fn recplay_trigger() -> Result<Option<String>, &'static str> {
    plx_ui::rec::mode_value(&plx_base::devtrig::path("recplay")).map_err(|_| "invalid replay trigger")
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn recplay_trigger() -> Result<Option<String>, &'static str> {
    Ok(None)
}

/// `/tmp/plxnative-app-init` — an explicit typed initial for a controlled boot, read by
/// `app::bootstrap` in place of capturing one. `None` when the trigger is absent (always, in a
/// release build); `Some(Err)` when it is present but unreadable. See [`rec_trigger`].
#[cfg(feature = "devtriggers")]
pub(crate) fn app_init_value() -> Option<Result<serde_json::Value, &'static str>> {
    plx_base::devtrig::flag("app-init").then(|| {
        plx_ui::rec::initial_value(&plx_base::devtrig::path("app-init"))
            .map_err(|_| "invalid explicit initial input")
    })
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn app_init_value() -> Option<Result<serde_json::Value, &'static str>> {
    None
}

// The sign-in trouble arms (`plxnative-signinfail`, `plxnative-readout`), the consent-state boot
// override (`plxnative-consentstate`) and the harness-driven test are READ by modules below this
// layer — `auth`, `telemetry`, the login screen — and only read trigger files, so their reads
// moved down beside them (`auth::scripted`, `telemetry::consent::state_override`,
// `screens::login::harness_driven`) instead of being reached up into from here.
