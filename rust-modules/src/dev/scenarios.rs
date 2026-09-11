//! Every dev-trigger ARM — the code that reads `/tmp/plxnative-*` through [`super::flag`] /
//! [`super::read`] / [`super::latched_flag!`] and reacts to it — gathered on one file (UI
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
use crate::screens::registry::AppArg;
use crate::screens::registry::HomeCmd;
use crate::ui::machine::{Key, Tick};
use std::os::raw::c_int;

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
    pub(crate) glass_hz_armed: bool,
    /// `plxnative-nobudget`: read at boot, applied to the one `Budget` at boot, and kept here so
    /// a log reader can tell an A leg from a B leg by the flags the boot recorded.
    pub(crate) nobudget: bool,
}

/// Every dev-trigger arm's own retry counter, oscillator phase and one-shot latch — the state
/// half of the move (spec's "per-arm STATE… moved into ONE Scenarios struct owned by App").
/// `pub(crate)` throughout: `app::boot`/`app::run`/`app::content` still read and write these
/// fields directly through `&mut App`, exactly as they read `App`'s own fields.
pub(crate) struct Scenarios {
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
    pub(crate) auto_tried: bool,
    pub(crate) replay_left: u32,
    pub(crate) grid_tried: bool,
    pub(crate) settings_tried: bool,
    pub(crate) seek_tried: bool,
    pub(crate) seek_script: Vec<String>,
    pub(crate) seek_script_at: u32,
    pub(crate) seek_gap_ms: u32,
    pub(crate) seek_script_last: i64,
    pub(crate) quality_script: Vec<crate::plex::session::PlaybackQuality>,
    pub(crate) quality_script_at: u32,
    pub(crate) quality_gap_ms: u32,
    pub(crate) quality_tried: bool,
    pub(crate) quality_playing_since: Option<u32>,
    pub(crate) detail_tried: bool,
    /// The headless detail-page walk (`plxnative-detail`/`-play`), see [`ContentBoot`].
    pub(crate) content_boot: Option<ContentBoot>,
    pub(crate) play_tried: bool,
    /// `/tmp/plxnative-play=<rk>` between its ASYNC request and the landing it plays from:
    /// `(server, ratingKey, the frame clock at which the wait gives up)`. See [`play_arm`].
    pub(crate) play_await: Option<(crate::plex::ServerId, String, u32)>,
    pub(crate) menu_tried: bool,
    pub(crate) menupick_tried: bool,
    /// dev: the row `/tmp/plxnative-menupick` still owes the track menu — see `App.menupick_row`'s
    /// old doc: the panel opens and is picked on separate frames, so the pick is carried here until
    /// the surface it names exists.
    pub(crate) menupick_row: Option<c_int>,
    pub(crate) pause_tried: bool,
    pub(crate) pause_script: Option<(u32, Option<u32>)>,
    pub(crate) pause_resume_at: Option<u32>,
    /// The boot-time trigger flags the loop consults every frame after.
    pub(crate) dev: DevFlags,
}

// =================================================================================================
// pre-SDL (today's app/mod.rs:451)
// =================================================================================================

/// `/tmp/plxnative-stats` — force the Stats-for-nerds overlay on, before SDL or a screen exists,
/// so a playback test photographs the same ABR/pipeline evidence on every automated run rather
/// than depending on a previous manual toggle surviving into this session.
pub(crate) fn pre_boot() {
    if crate::dev::flag("stats") {
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
    crate::dev::flag("novsync")
}

/// `/tmp/plxnative-login` — force the QR login screen even with a usable session.
pub(crate) fn login_forced() -> bool {
    crate::dev::flag("login")
}

/// `/tmp/plxnative-token` — the harness/headless test identity, read once. Never logged.
pub(crate) fn dev_token() -> String {
    match crate::dev::read("token") {
        Some(s) if !s.is_empty() => {
            crate::log("token: using /tmp/plxnative-token (test identity)");
            s
        }
        _ => String::new(),
    }
}

/// `/tmp/plxnative-pickuser=<index>` — force the boot picker and auto-select that roster tile.
pub(crate) fn pickuser_index() -> Option<usize> {
    crate::dev::read("pickuser").and_then(|s| s.parse().ok())
}

/// `/tmp/plxnative-logintest` — validate the plex.tv account path end to end on the device.
pub(crate) fn arm_logintest() {
    if crate::dev::flag("logintest") {
        let _ = crate::task::spawn_small("logintest", || {
            let sess = crate::plex::session::load();
            let ac = crate::plex::account::AccountClient::new(&sess.client_id, None);
            match ac.create_pin() {
                Some(p) => crate::log(&format!(
                    "logintest: create_pin ok id={} code_len={} authToken_null={}",
                    p.id,
                    p.code.len(),
                    p.auth_token.is_none()
                )),
                None => crate::log("logintest: create_pin FAILED (transport/TLS/link/deser)"),
            }
        });
    }
}

/// `/tmp/plxnative-anim` — the animation-diagnostic overlay (off by default).
pub(crate) fn arm_anim() {
    if crate::dev::flag("anim") {
        crate::ui::anim::set_enabled(true);
    }
}

/// `/tmp/plxnative-glassload` — the backdrop-glass LOAD DIAL.
pub(crate) fn arm_glassload(glass: &mut crate::ui::frame::glass::GlassPlan) {
    if let Some(v) = crate::dev::read("glassload") {
        glass.configure_dial(&v);
    }
}

/// `/tmp/plxnative-navblur` — the blurred-route-transition prototype.
pub(crate) fn arm_navblur(glass: &mut crate::ui::frame::glass::GlassPlan) {
    if let Some(v) = crate::dev::read("navblur") {
        glass.configure_navblur(&v);
    }
}

/// `/tmp/plxnative-overdraw` — the CPU-side per-draw-class overdraw ledger.
pub(crate) fn arm_overdraw() {
    if crate::dev::flag("overdraw") {
        crate::ui::overdraw::set_ledger(true);
    }
}

/// `/tmp/plxnative-drawmask=<classes>` — refuse every draw of the named classes.
pub(crate) fn arm_drawmask() {
    if let Some(spec) = crate::dev::read("drawmask") {
        crate::ui::overdraw::set_mask(&spec);
    }
}

/// `/tmp/plxnative-heroground` — the one-pass hero ground A/B.
pub(crate) fn arm_heroground() {
    if crate::dev::flag("heroground") {
        crate::ui::widgets::set_hero_ground(true);
        crate::log("hero: one-pass ground ENABLED by /tmp/plxnative-heroground");
    }
}

/// `/tmp/plxnative-glasshz=<presents-per-refresh>` — the shared dynamic-backdrop cadence knob.
/// Returns whether it was armed (used to gate the heartbeat's `snap=` field).
pub(crate) fn arm_glasshz() -> bool {
    if let Some(v) = crate::dev::read("glasshz") {
        let asked: u32 = v.parse().unwrap_or(0);
        let got = crate::ui::widgets::set_dynamic_period(asked);
        crate::log(&format!(
            "blur: dynamic cadence asked={asked} presents-per-refresh={got}"
        ));
        true
    } else {
        false
    }
}

/// `/tmp/plxnative-profile` / `/tmp/plxnative-hwcnt` — the two GPU-time profilers. Both present
/// is refused; either alone arms its mode.
pub(crate) fn arm_profile_hwcnt() {
    match (crate::dev::read("profile"), crate::dev::read("hwcnt")) {
        (Some(_), Some(_)) => {
            crate::log("PROFILE disabled: remove either /tmp/plxnative-profile or /tmp/plxnative-hwcnt");
        }
        (Some(filter), None) => crate::ui::profile::set_enabled(&filter),
        (None, Some(filter)) => crate::ui::profile::set_hwcnt_enabled(&filter),
        (None, None) => {}
    }
}

/// `/tmp/plxnative-cpuprof` — the render thread's own per-phase CPU clock.
pub(crate) fn arm_cpuprof() {
    if crate::dev::flag("cpuprof") {
        crate::ui::profile::set_cpu_enabled();
    }
}

/// `/tmp/plxnative-noidle` — turn the whole-frame present gate off.
pub(crate) fn arm_noidle() {
    if crate::dev::flag("noidle") {
        crate::ui::idle::set_enabled(false);
        crate::log("idle: present gate DISABLED by /tmp/plxnative-noidle");
    }
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
    crate::dev::flag("nobudget")
}

/// `/tmp/plxnative-detailosc`.
pub(crate) fn detailosc_armed() -> bool {
    crate::dev::flag("detailosc")
}
/// `/tmp/plxnative-homeosc`.
pub(crate) fn homeosc_armed() -> bool {
    crate::dev::flag("homeosc")
}
/// `/tmp/plxnative-heroosc`.
pub(crate) fn heroosc_armed() -> bool {
    crate::dev::flag("heroosc")
}
/// `/tmp/plxnative-homefoldosc`.
pub(crate) fn homefoldosc_armed() -> bool {
    crate::dev::flag("homefoldosc")
}
/// `/tmp/plxnative-libosc`.
pub(crate) fn libosc_armed() -> bool {
    crate::dev::flag("libosc")
}
/// `/tmp/plxnative-libswitch`.
pub(crate) fn libswitch_armed() -> bool {
    crate::dev::flag("libswitch")
}
/// `/tmp/plxnative-searchosc`.
pub(crate) fn searchosc_armed() -> bool {
    crate::dev::flag("searchosc")
}
/// `/tmp/plxnative-settings=<root|home|privacy|legal>`.
pub(crate) fn settings_boot_value() -> Option<String> {
    crate::dev::read("settings")
}
/// `/tmp/plxnative-settingsosc`.
pub(crate) fn settingsosc_armed() -> bool {
    crate::dev::flag("settingsosc")
}
/// `/tmp/plxnative-modalosc`.
pub(crate) fn modalosc_armed() -> bool {
    crate::dev::flag("modalosc")
}
/// `/tmp/plxnative-legaldoc`.
pub(crate) fn legaldoc_armed() -> bool {
    crate::dev::flag("legaldoc")
}
/// `/tmp/plxnative-alert`.
pub(crate) fn alert_armed() -> bool {
    crate::dev::flag("alert")
}
/// `/tmp/plxnative-acctosc`.
pub(crate) fn acctosc_armed() -> bool {
    crate::dev::flag("acctosc")
}
/// `/tmp/plxnative-consentosc`.
pub(crate) fn consentosc_armed() -> bool {
    crate::dev::flag("consentosc")
}
/// `/tmp/plxnative-onboardosc`.
pub(crate) fn onboardosc_armed() -> bool {
    crate::dev::flag("onboardosc")
}
/// `/tmp/plxnative-navosc[=<ratingKey>]`.
pub(crate) fn navosc_value() -> Option<String> {
    crate::dev::read("navosc")
}
/// `/tmp/plxnative-framedrop[=<ms>]`.
pub(crate) fn framedrop_value() -> Option<String> {
    crate::dev::read("framedrop")
}
/// `/tmp/plxnative-firstrun`.
pub(crate) fn firstrun_armed() -> bool {
    crate::dev::flag("firstrun")
}
/// `/tmp/plxnative-acct` — auto-open the profile menu (headless capture of the popover).
pub(crate) fn acct_armed() -> bool {
    crate::dev::flag("acct")
}
/// `/tmp/plxnative-replay[=N]`'s raw content, for [`crate::app::boot::replay_budget`].
pub(crate) fn replay_trigger_value() -> Option<String> {
    crate::dev::read("replay")
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
    sid: crate::plex::ServerId,
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
    /// Is the page this boot is waiting for the one on top?
    fn is_top(&self, d: &crate::ui::dispatch::Dispatcher<crate::app::bridge::AppHost>) -> bool {
        matches!(d.top_arg(), Some(AppArg::Content(crate::screens::registry::ContentArg::Detail { sid, rk }))
            if *sid == self.sid && *rk == self.rk)
    }

    pub(crate) fn new(sid: crate::plex::ServerId, rk: String) -> Self {
        Self {
            sid,
            rk,
            season: None,
            down: crate::dev::read("detailsec").and_then(|s| s.parse().ok()).unwrap_or(0),
            right: crate::dev::read("detailcol").and_then(|s| s.parse().ok()).unwrap_or(0),
            activate: crate::dev::flag("detailok") || crate::dev::flag("detailplay"),
            filmography: crate::dev::flag("filmography"),
            bio: crate::dev::flag("bio"),
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
    crate::dev::flag("detailplay")
}

pub(crate) fn advance_content_boot(app: &mut App, fr: &Frame) {
    use crate::app::bridge;
    use crate::screens::registry::{AppArg, ContentArg};
    use crate::ui::machine::{Delivery, Fx, MachineId, NavOp};
    use crate::ui::screen::ScreenEvent;

    let Some(mut boot) = app.scenarios.content_boot.take() else { return };
    let ready = if boot.waiting_person {
        app.pages.nav.top_page().is_some_and(|entry| {
            let AppArg::Content(ContentArg::Person { sid, key, .. }) = &entry.arg else { return false };
            crate::person::current().is_some_and(|p| p.sid == *sid && p.key == *key
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
        let loaded = crate::metadata::current().map(|d|
            (d.sid, d.rk.as_str(), d.seasons.get(d.cur_season).map(|s| s.index)));
        boot.is_top(&app.pages)
            && detail_boot_ready(boot.sid, &boot.rk, boot.season, loaded, crate::metadata::detail_loading(), crate::metadata::season_loading())
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
            .and_then(|a| a.downcast_ref::<crate::screens::person::PersonScreen>())
            .is_some_and(|page| page.bio_available());
        if let (Some(host), true) = (app.pages.top_page(), available) {
            bridge::open_content_panel(
                &mut app.pages,
                host,
                None,
                crate::screens::registry::ContentPanel::Bio,
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
            app.pages.nav.next_style = crate::ui::containers::modal::Style::Opaque { snapshot: true };
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
            if let Some(pg) = crate::dev::read("tracks") {
                let host = app.pages.top_page();
                let available = app
                    .pages
                    .nav
                    .top_page()
                    .and_then(|e| e.inst.as_ref())
                    .and_then(|i| i.screen.as_any())
                    .and_then(|a| a.downcast_ref::<crate::screens::detail::DetailScreen>())
                    .is_some_and(|d| d.tracks_available());
                if let (Some(host), true) = (host, available) {
                    let (sid, rk) = (boot.sid, boot.rk.clone());
                    bridge::open_content_panel(
                        &mut app.pages,
                        host,
                        Some((sid, &rk)),
                        crate::screens::registry::ContentPanel::Tracks {
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
            if crate::dev::flag("about") {
                if let Some(host) = app.pages.top_page() {
                    let (sid, rk) = (boot.sid, boot.rk.clone());
                    bridge::open_content_panel(
                        &mut app.pages,
                        host,
                        Some((sid, &rk)),
                        crate::screens::registry::ContentPanel::About,
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

fn detail_boot_ready(sid: crate::plex::ServerId, rk: &str, want_season: Option<i64>,
    loaded: Option<(crate::plex::ServerId, &str, Option<i64>)>, detail_loading: bool, season_loading: bool) -> bool {
    !detail_loading && !season_loading && loaded.is_some_and(|(server, key, season)|
        server == sid && key == rk && want_season.is_none_or(|wanted| season == Some(wanted)))
}

#[cfg(test)]
mod content_boot_tests {
    use super::*;

    #[test]
    fn delayed_detail_and_season_landings_do_not_consume_headless_directions() {
        let sid = crate::plex::ServerId::UNSET;
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
/// which calls this function directly and does NOT drive [`super::read`] itself (that one line is
/// not covered by a host test; a full `App`/SDL frame would be needed to reach it).
pub(crate) fn apply_search_boot_trigger(q: &str, d: &mut crate::ui::dispatch::Dispatcher<crate::app::bridge::AppHost>) {
    crate::stores::search::apply(crate::stores::search::SearchCmd::SetQuery(q.trim().to_string()));
    // A peer of Home, exactly as an interactive press on the strip's last pill is — and a ROOT
    // rather than a push, because at boot there is nothing above the root to stand on.
    crate::app::bridge::nav_root(d, AppArg::Search);
}

/// `now - at >= gap_ms`, read as SIGNED so a future `at` (a `delay=` in force) correctly does not
/// fire yet. See the tests below for the wrap and delay traps this predicate has to survive.
fn script_step_due(now: u32, at: u32, gap_ms: u32) -> bool {
    (now.wrapping_sub(at) as i32) >= gap_ms as i32
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
    use crate::screens::player::input::HUD_HEADLESS_MS;
    if !app.scenarios.auto_tried
        && !matches!(app.route(), AppArg::Player | AppArg::Login | AppArg::Profiles)
        && fr.now.wrapping_sub(app.t0) > 2000
    {
        app.scenarios.auto_tried = true;
        let playurl = crate::dev::flag("playurl");
        if crate::dev::flag("autoplay") || playurl {
            let requested = if playurl || crate::dev::flag("h265") {
                crate::route::clear_url(&mut app.player.session);
                true
            } else {
                let pidx = crate::dev::read("playidx")
                    .and_then(|s| s.parse::<c_int>().ok())
                    .unwrap_or(0);
                if let Some(pmm) = usize::try_from(pidx).ok().and_then(|i| crate::pms::hub_item(i / crate::app::COLS as usize, i % crate::app::COLS as usize)) {
                    let requested = crate::route::request_play_movie(&mut app.player.session, pmm);
                    if requested {
                        // ASYNC (phase 11): nothing here reads `metadata::current()` — the play
                        // plan came from the catalog row itself. The detail is wanted only so the
                        // player's Info card has a descriptor, and the landing's own
                        // `install_landed_detail` calls the same `sync_now_playing` the blocking
                        // load did. So there is nothing to wait for, and no reason to spend two
                        // PMS round trips of the SDL thread on the frame that starts a playback.
                        crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid: pmm.sid, rk: pmm.rk.to_string() });
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
        if crate::dev::flag("grid") || crate::dev::flag("itemmenu") {
            app.bridge.home_command(HomeCmd::FocusGrid { row: 0, col: 0 });
        }
        if let Some(s) = crate::dev::read("library") {
            let kind = match s.parse::<usize>().unwrap_or(0) {
                1 => crate::browse::SecKind::Show,
                _ => crate::browse::SecKind::Movie,
            };
            app.bridge.enter_library(kind);
            crate::app::bridge::nav_root(&mut app.pages, AppArg::Library);
        }
        if let Some(q) = crate::dev::read("search") {
            apply_search_boot_trigger(&q, &mut app.pages);
        }
        if let Some(s) = crate::dev::read("heroidx") {
            if let Ok(n) = s.parse::<c_int>() {
                app.bridge.home_command(HomeCmd::SelectHero(n));
            }
        }
    }
}

fn settings_boot_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.settings_tried && fr.now.wrapping_sub(app.t0) > 800 {
        if matches!(app.route(), AppArg::Home) {
            app.scenarios.settings_tried = true;
            let page = match app.scenarios.dev.settings_boot.as_deref().map(str::trim).unwrap_or("root") {
                "" | "root" => crate::screens::family::SettingsPage::Root,
                "home" => crate::screens::family::SettingsPage::Favourites,
                "privacy" => crate::screens::family::SettingsPage::Privacy,
                "legal" => crate::screens::family::SettingsPage::Legal,
                other => {
                    crate::log(&format!("BADTRIGGER settings-boot target {other:?} unknown; opened root instead"));
                    crate::screens::family::SettingsPage::Root
                }
            };
            crate::app::bridge::open_settings_at(&mut app.pages, page);
        } else if fr.now.wrapping_sub(app.t0) > 12_000 {
            app.scenarios.settings_tried = true;
            crate::log("settings: boot target timed out before Home became available");
        }
    }
}

fn press_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.press_tried && fr.now.wrapping_sub(app.t0) > 1600 {
        app.scenarios.press_tried = true;
        if crate::dev::flag("press")
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
fn acct_arm(app: &mut App, fr: &mut Frame) {
    if app.scenarios.acct_tried || !crate::dev::scenarios::acct_armed() {
        return;
    }
    if matches!(app.route(), AppArg::Home) && app.pages.top_page().is_some() {
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
        if crate::dev::flag("itemmenu") && matches!(app.route(), AppArg::Home) {
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
        if let Some(rk) = crate::dev::read("detail") {
            let rk = rk.as_str();
            if !rk.is_empty() {
                let sid = match crate::app::boot::direct_trigger_server() {
                    Ok(sid) => sid,
                    Err(e) => {
                        crate::log(&format!("plxnative-detail: refused: {e}"));
                        return false;
                    }
                };
                crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid, rk: rk.to_string() });
                crate::log(&format!("plxnative-detail: rk={rk} server={} start", sid.raw()));
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
        if let Some(rk) = crate::dev::read("play") {
            let rk = rk.as_str();
            if !rk.is_empty() {
                let sid = match crate::app::boot::direct_trigger_server() {
                    Ok(sid) => sid,
                    Err(e) => {
                        crate::log(&format!("plxnative-play: refused: {e}"));
                        return false;
                    }
                };
                crate::stores::metadata::apply(crate::stores::metadata::MetadataCmd::RequestDetail { sid, rk: rk.to_string() });
                crate::log(&format!("plxnative-play: rk={rk} server={} request", sid.raw()));
                app.scenarios.play_await = Some((sid, rk.to_string(), fr.now.wrapping_add(12_000)));
            }
        }
    }
    play_await_tick(app, fr);
    true
}

/// The landing half of [`play_arm`], run every frame while a request is outstanding.
fn play_await_tick(app: &mut App, fr: &mut Frame) {
    use crate::screens::player::input::HUD_LINGER_MS;
    let Some((sid, rk, deadline)) = app.scenarios.play_await.clone() else { return };
    if matches!(app.route(), AppArg::Player) {
        app.scenarios.play_await = None;
        return;
    }
    let leaf = crate::metadata::current()
        .filter(|d| crate::plex::same_item((d.sid, &d.rk), (sid, &rk)))
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
        let settled = crate::metadata::detail_request_status(sid, &rk) == Some(false);
        let expired = fr.now.wrapping_sub(deadline) < u32::MAX / 2;
        if settled || expired {
            app.scenarios.play_await = None;
            crate::log(&format!(
                "plxnative-play: rk={rk} server={} — no detail landed ({})",
                sid.raw(),
                if settled { "the fetch settled without it" } else { "12s" }
            ));
        }
        return;
    };
    app.scenarios.play_await = None;
    if part.is_empty() {
        crate::log(&format!("plxnative-play: rk={rk} server={} — nothing playable on it", sid.raw()));
        return;
    }
    crate::log(&format!("plxnative-play: rk={rk} server={} start", sid.raw()));
    if crate::route::request_play(&mut app.player.session, sid, &rk, &part, &vc, &ac, &title, "") {
        let resume = crate::metadata::resume_ns(resume_ms, dur_ms);
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
        if let Some(s) = crate::dev::read("autoseek") {
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
            app.scenarios.seek_script_last = crate::player::playpos_ns();
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
        crate::log(&format!("autoseek: step → {}s ({} left)", t / 1_000_000_000, app.scenarios.seek_script.len()));
        crate::app::playback::request_seek(t);
    }
}

fn qualityswitch_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.quality_tried {
        const QUALITY_SWITCH_OBSERVE_MS: u32 = 12_000;
        let playing = matches!(app.route(), AppArg::Player)
            && crate::app::playback::dur() > 0
            && crate::player::is_playing(&app.player.session);
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
        crate::log(&format!("quality: switch → {} ({} left)", super::quality_wire_name(q), app.scenarios.quality_script.len()));
        crate::route::set_quality(&mut app.player.session, q);
    }
}

fn autopause_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.pause_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 6000 {
        app.scenarios.pause_tried = true;
        if let Some(script) = super::pause_script() {
            app.scenarios.pause_script = Some((fr.now.wrapping_add(script.delay_ms), script.hold_ms));
        }
    }
    if let Some((pause_at, hold_ms)) = app.scenarios.pause_script {
        if matches!(app.route(), AppArg::Player) && script_step_due(fr.now, pause_at, 0) {
            if crate::app::lifecycle::set_transport_paused(&mut app.adapters.player, true) {
                crate::log(&format!(
                    "autopause: Pause accepted hold={}ms",
                    hold_ms.map_or_else(|| "forever".to_string(), |ms| ms.to_string()),
                ));
                app.scenarios.pause_script = None;
                app.scenarios.pause_resume_at = hold_ms.map(|hold| fr.now.wrapping_add(hold));
                pin_headless_hud(app, fr.now, None);
            }
        }
    }
    if let Some(resume_at) = app.scenarios.pause_resume_at {
        if matches!(app.route(), AppArg::Player) && script_step_due(fr.now, resume_at, 0) {
            if crate::app::lifecycle::set_transport_paused(&mut app.adapters.player, false) {
                crate::log("autopause: Resume accepted");
                app.scenarios.pause_resume_at = None;
            }
        }
    }
}

fn menu_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.menu_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 6000 {
        app.scenarios.menu_tried = true;
        if let Some(t) = crate::dev::read("menu") {
            crate::app::bridge::open_player_overlay(&mut app.player.session,
                &mut app.pages,
                crate::screens::player::overlay::OverlayKind::Tracks { tab: t.parse::<c_int>().unwrap_or(0) },
            );
            pin_headless_hud(app, fr.now, None);
        }
        if crate::dev::flag("info") {
            crate::app::bridge::open_player_overlay(&mut app.player.session, &mut app.pages, crate::screens::player::overlay::OverlayKind::Info);
            pin_headless_hud(app, fr.now, Some(0));
        }
        if crate::dev::flag("chapters") {
            crate::app::bridge::open_player_overlay(&mut app.player.session, &mut app.pages, crate::screens::player::overlay::OverlayKind::Chapters);
            pin_headless_hud(app, fr.now, Some(1));
        }
    }
}

fn menupick_arm(app: &mut App, fr: &mut Frame) {
    if !app.scenarios.menupick_tried && matches!(app.route(), AppArg::Player) && fr.now.wrapping_sub(app.t0) > 7000 {
        app.scenarios.menupick_tried = true;
        if let Some(s) = crate::dev::read("menupick") {
            let mut it = s.split(',');
            let tab = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
            let row = it.next().and_then(|x| x.trim().parse::<c_int>().ok()).unwrap_or(0);
            crate::app::bridge::open_player_overlay(&mut app.player.session, &mut app.pages, crate::screens::player::overlay::OverlayKind::Tracks { tab });
            app.scenarios.menupick_row = Some(row);
        }
    }
    if let Some(row) = app.scenarios.menupick_row.take() {
        match crate::app::bridge::player_overlay_mut(&mut app.pages) {
            Some(surface) => {
                if let Some(commit) = surface.pick_track_row(&app.player.session, row) {
                    crate::app::playback::commit_track(&mut app.player.session, commit);
                }
            }
            None => app.scenarios.menupick_row = Some(row),
        }
    }
}

fn marker_arm(app: &mut App, _fr: &mut Frame) {
    if !app.scenarios.marker_tried && matches!(app.route(), AppArg::Player) && crate::player::is_playing(&mut app.player.session) {
        match crate::dev::read("marker") {
            Some(s) => {
                let want = if s.eq_ignore_ascii_case("intro") {
                    crate::metadata::MarkerKind::Intro
                } else {
                    crate::metadata::MarkerKind::Credits
                };
                let markers = crate::metadata::playing_markers();
                if !markers.is_empty() {
                    app.scenarios.marker_tried = true;
                    if let Some(m) = markers.iter().find(|m| m.kind == want) {
                        let t = (m.start_ms - 5_000).max(0) * 1_000_000;
                        crate::log(&format!("marker trigger: seek to {}s (5s before {:?})", t / 1_000_000_000, want));
                        crate::app::playback::request_seek(t);
                    } else {
                        crate::log(&format!("marker trigger: item has no {want:?} marker"));
                    }
                }
            }
            None => app.scenarios.marker_tried = true,
        }
    }
}

/// `/tmp/plxnative-replay[=N]` — REPLAY AFTER COMPLETION (LG App Self Checklist #46). Called from
/// `app::run::playback_tick` right after `finish_playback` has left the player on a real EOS (an
/// Up Next handoff would have RETURNED there instead, which the caller's `matches!` on `Route`
/// already told apart). Re-arming `auto_tried` sends the next frame back through the `playurl`
/// entry, which calls `route::clear_url()` and lets `start_bufferfeed` read the trigger again.
/// The trigger is read once at boot (`replay_left`), so this cannot become an endless loop from a
/// file appearing mid-run, and `dev::flag` is `false` at COMPILE time in a release build.
pub(crate) fn maybe_replay_after_eos(app: &mut App) {
    if app.scenarios.replay_left > 0
        && !matches!(app.route(), AppArg::Player)
        && crate::dev::flag("playurl")
    {
        app.scenarios.replay_left -= 1;
        app.scenarios.auto_tried = false;
        crate::log(&format!(
            "replay: starting the finished stream again ({} left)", app.scenarios.replay_left
        ));
    }
}

/// The boot-trigger SCRIPTS (autoplay, grid, settings, press, itemmenu, detail, play, seek,
/// quality, pause, menu, marker), called once per iteration from `app::run::run` at exactly the
/// position `dev_scripts` occupied. `false` propagates a refused trigger (an invalid
/// `plxnative-server` slot) — the loop `continue`s exactly as it always did, skipping the rest of
/// this frame's arms and phases alike.
pub(crate) unsafe fn each_frame(app: &mut App, fr: &mut Frame) -> bool {
    autoplay_arm(app, fr);
    grid_library_search_heroidx_arm(app, fr);
    settings_boot_arm(app, fr);
    press_arm(app, fr);
    itemmenu_arm(app, fr);
    acct_arm(app, fr);
    if !detail_arm(app, fr) {
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
    marker_arm(app, fr);
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
        && crate::auth::phase() == crate::auth::Phase::Profiles
        && !crate::auth::users().is_empty())
    {
        return;
    }
    let idx = app.scenarios.pick_user.take().unwrap();
    // **Ask the SAME question `screens::profiles::ProfilesScreen::select` asks before acting**,
    // because this call site cannot reach that method to ask it FOR us — see the phase-9 report
    // this arm's comment used to carry for the full account of why a protected roster index
    // refuses here rather than attempting a PIN-less switch plex.tv would refuse anyway.
    let protected = crate::auth::users().get(idx).map(|u| u.protected).unwrap_or(false);
    if protected {
        crate::log(&format!(
            "pickuser: roster index {idx} is PROTECTED — refusing rather than attempting \
             a PIN-less switch plex.tv would refuse anyway; this trigger has no door onto \
             the owned picker's own PIN pad yet"
        ));
    } else {
        crate::log(&format!("pickuser: auto-selecting roster index {idx}"));
        crate::auth::select_profile(idx);
    }
}

/// `/tmp/plxnative-navosc` — bounce the route Home↔Library (or Home↔a named detail page) on a
/// timer. Called from `app::run::land_results` at the position the arm always occupied.
pub(crate) fn nav_osc_tick(app: &mut App, now: u32) {
    use crate::screens::registry::HomeTab;
    if app.scenarios.dev.nav_osc && now.wrapping_sub(app.scenarios.nav_osc_last) > 1400 {
        app.scenarios.nav_osc_last = now;
        match app.route() {
            AppArg::Home if !app.scenarios.dev.nav_osc_rk.is_empty() => {
                let rk = app.scenarios.dev.nav_osc_rk.clone();
                crate::app::bridge::open_detail(&mut app.pages, &mut app.bridge,
                    crate::plex::current_server(), &rk, None, None);
            }
            AppArg::Content(_) => crate::app::bridge::nav_pop(&mut app.pages),
            AppArg::Home => {
                if let Some(kind) = crate::browse::tab_kind(0) {
                    let tab = match kind {
                        crate::browse::SecKind::Show => HomeTab::Shows,
                        _ => HomeTab::Movies,
                    };
                    crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge, tab, None, None);
                }
            }
            AppArg::Library => crate::app::bridge::nav_tab(&mut app.pages, &mut app.bridge,
                HomeTab::Home, Some(crate::app::chrome::pill_at(1)), None),
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
        let sym = if (now / 3000) % 2 == 0 { crate::ui::consts::SDLK_DOWN } else { crate::ui::consts::SDLK_UP };
        app.inputs.extend(crate::app::bridge::script_key(
            if sym == crate::ui::consts::SDLK_DOWN { Key::Down } else { Key::Up },
            Tick { ms: now, dt_us: 0 }));
    }
}

/// `/tmp/plxnative-libosc` — the Library twin of `homeosc`.
pub(crate) fn lib_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.lib_osc && matches!(app.route(), AppArg::Library) && now.wrapping_sub(app.scenarios.lib_osc_last) > 350 {
        app.scenarios.lib_osc_last = now;
        crate::app::bridge::Bridge::library_command(&mut app.pages, crate::screens::registry::LibraryCmd::Sweep);
    }
}

/// `/tmp/plxnative-libswitch` — cycle EVERY Library switch on a timer.
pub(crate) fn lib_switch_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.lib_switch && matches!(app.route(), AppArg::Library) && now.wrapping_sub(app.scenarios.lib_switch_last) > 1400 {
        app.scenarios.lib_switch_last = now;
        crate::app::bridge::Bridge::library_command(&mut app.pages, crate::screens::registry::LibraryCmd::SwitchStep(app.scenarios.lib_switch_step));
        app.scenarios.lib_switch_step = app.scenarios.lib_switch_step.wrapping_add(1);
    }
}

/// `/tmp/plxnative-searchosc` — the Search twin of `homeosc`/`libosc`.
pub(crate) fn search_osc_tick(app: &mut App, now: u32) {
    if matches!(app.route(), AppArg::Search) && app.scenarios.dev.search_osc && now.wrapping_sub(app.scenarios.search_osc_last) > 350 {
        app.scenarios.search_osc_last = now;
        let sym = if (now / 3000) % 2 == 0 { crate::ui::consts::SDLK_DOWN } else { crate::ui::consts::SDLK_UP };
        app.inputs.extend(crate::app::bridge::script_key(
            if sym == crate::ui::consts::SDLK_DOWN { Key::Down } else { Key::Up },
            Tick { ms: now, dt_us: 0 }));
    }
}

/// `/tmp/plxnative-acctosc` — drive the profile menu's own TableView.
pub(crate) fn account_osc_tick(app: &mut App, now: u32) {
    if app.scenarios.dev.account_osc && crate::app::bridge::account_menu_up(&app.pages) {
        crate::ui::idle::wake();
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

/// `/tmp/plxnative-legaldoc` (with `plxnative-settings=legal`) — one OK on the Legal index.
pub(crate) fn legal_doc_tick(app: &mut App, now: u32, dt: f32) {
    if app.scenarios.dev.legal_doc
        && !app.scenarios.legal_doc_tried
        && crate::app::bridge::surface_word(&app.pages) == Some(crate::screens::registry::word::LEGAL)
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
        && crate::app::bridge::surface_word(&app.pages) == Some(crate::screens::registry::word::PRIVACY)
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
        crate::ui::idle::wake();
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
        crate::ui::idle::invalidate();
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
        crate::ui::idle::invalidate();
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
    if app.scenarios.dev.detail_osc && matches!(app.route(), AppArg::Content(crate::screens::registry::ContentArg::Detail { .. })) {
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
    crate::dev::read("consent")
}

/// `/tmp/plxnative-rec` — read by `app::recorder::Recplay::arm`. The recorder/replay MECHANISM
/// stays in `app/recorder.rs` (this phase's instructions: it is not a scenario), but the raw
/// trigger read goes through the one door every other trigger does.
pub(crate) fn rec_trigger() -> Option<String> {
    crate::dev::read("rec")
}

/// `/tmp/plxnative-recplay` — see [`rec_trigger`].
pub(crate) fn recplay_trigger() -> Option<String> {
    crate::dev::read("recplay")
}
