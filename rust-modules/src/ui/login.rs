//! The sign-in screen: Plex's own server-rendered QR PNG (fetched by the auth flow, decoded +
//! tinted here) plus the typed short-code fallback, driven by the [`crate::auth`] flow phase.
//! Scanning the QR on a phone opens plex.tv pre-filled with the pin; the flow's background poll
//! then advances us onward.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::consts::*;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
use crate::ui::label::{HAlign, Label};
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

struct Scene {
    spin_ms: f32,
    qr_tex: u32, // GL texture of Plex's QR PNG (0 until decoded+uploaded)
    /// Which code `qr_tex` holds ([`auth::qr_generation`]). The cache key, and the reason a code
    /// replaced mid-`Waiting` cannot be drawn after it has died.
    qr_gen: u64,
    ground: RouteGround,
    /// How long the CURRENT wait has been on screen. Distinct from `spin_ms`, which is a
    /// free-running rotation clock: this one is reset by [`update`] whenever the wait changes,
    /// because the only question it answers is "has this particular wait gone on too long".
    phase_ms: f32,
    /// What that wait IS — see [`wait_id`]. A phase alone was not enough once a code could be
    /// replaced without leaving [`Phase::Waiting`].
    wait: (Phase, u64),
    /// How many files the last **Delete all local data** could not unlink. Drives the wording of
    /// the `Deleted` read-out, which must not claim a wipe it did not achieve.
    delete_leftovers: usize,
    /// The link-health sentence ([`auth::link_detail`]) this screen last drew, or `None` while
    /// plex.tv was answering. Cached so [`update`] can tell whether the text on screen actually
    /// changed — the link snapshot is sampled every frame, but the sentence itself only moves
    /// roughly once a poll (~2 s), and a settled screen must not be woken every frame by a value
    /// that reads the same both times (see `ui::idle`'s discrete-change rule).
    last_link_detail: Option<String>,
    /// **Issue #75** — the one-off sign-in report alert, opened over the failed or stuck read-out.
    report_alert: DecisionAlert,
    /// Which attempt [`report_alert`] was last opened (or answered) for — see [`offer_alert`].
    /// `None` before this boot has offered one at all. Never explicitly reset on a flow restart:
    /// `auth::trouble_snapshot`'s attempt id keeps climbing, so a fresh attempt's id can never
    /// equal an old one this field remembers, and the alert opens again on its own.
    report_offered_for: Option<u64>,
    /// **Issue #75 review.** Did the most recent deliberate "Send report" press fail to queue
    /// (no Sentry endpoint, no `/dev/urandom`, or the durable spool refused it)? Reset to `false`
    /// every time [`report_alert`] opens for a new attempt, so a stale failure from a previous
    /// trouble cannot linger over one this attempt never tried to send. Drives
    /// [`report_note`]'s third state — without it a failed press was completely silent, dismissing
    /// the alert and leaving the read-out byte-identical to "Not now".
    report_send_failed: bool,
}

/// How long a working phase runs before the read-out grows a way out.
///
/// **Not zero**: a healthy LAN discovery finishes in well under a second, and a control that
/// flashes past on every sign-in is noise that teaches people to ignore it. **Not longer**: from
/// the sofa a spinner that will never stop looks exactly like one that is about to, and until this
/// existed there was no way at all out of a wedged sign-in — BACK is swallowed on a first-ever
/// boot (`auth::cancel` has no stored session to resume), so the only exit was killing the app.
const ESCAPE_AFTER_MS: f32 = 12_000.0;

/// Whether a wait has run long enough to be worth offering an escape from.
///
/// Pure and separate from the draw so the threshold is gradeable on the host; the state it reads
/// lives in the SDL loop's own scene.
fn escape_offered(phase_ms: f32) -> bool {
    phase_ms >= ESCAPE_AFTER_MS
}

/// The verb on both the failed and the stuck read-out, because it is the same call underneath.
///
/// `auth::retry` bumps the auth epoch, so a worker still blocked in the wedged request has its
/// result discarded when it finally returns, and it re-runs only the leg that failed — discovery
/// when the pin already yielded an account credential, a whole fresh pin when it did not.
const ESCAPE: &std::ffi::CStr = c"Try again";

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe {
        (*addr_of_mut!(SCENE))
            .as_mut()
            .expect("login::init not called")
    }
}

pub fn init() {
    let mut report_alert = DecisionAlert::new();
    // The "Send report" answer ends nothing — it is the opposite of the delete alert's
    // `Tone::Destructive` default, and shipping it red would say otherwise.
    report_alert.set_tone(Tone::Neutral);
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene {
            spin_ms: 0.0,
            qr_tex: 0,
            qr_gen: auth::qr_generation(),
            ground: RouteGround::new(),
            phase_ms: 0.0,
            wait: wait_id(),
            delete_leftovers: 0,
            last_link_detail: None,
            report_alert,
            report_offered_for: None,
            report_send_failed: false,
        });
    }
}

/// Mount the auth route without replacing the cached QR texture. A fresh auth flow invalidates the
/// texture from [`update`] when it reaches `Creating`; this only resets the visit's visual ground.
pub fn enter() {
    let s = scene();
    s.ground.reset();
    // A fresh visit is a fresh wait, whatever phase the last one died in.
    s.phase_ms = 0.0;
    s.wait = wait_id();
    // **Issue #75 review.** A one-off report alert left OPEN when the flow leaves this route
    // (the link recovers and finishes the sign-in right after `note_waiting_trouble` opened it,
    // with nobody dismissing it) would otherwise reappear over the NEXT visit's fresh QR screen —
    // a sign-out then a re-entry here — and swallow every key exactly as it does while genuinely
    // open. `close()`, not `dismiss()`: an instant hide is right for "the subject vanished out
    // from under the alert" (`Popover::close`'s own case), which this is — the trouble this alert
    // was about belongs to the attempt that just ended.
    s.report_alert.close();
    crate::ui::idle::invalidate();
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    // Each wait gets its own clock. A flow that walks Creating → Waiting → Discovering is making
    // progress, and restarting the timer at every step is what stops a slow-but-healthy sign-in
    // from being offered a way out of itself.
    let live = wait_id();
    if wait_restarted(s.wait, live) {
        s.wait = live;
        s.phase_ms = 0.0;
    } else {
        s.phase_ms += dt * 1000.0;
    }
    drop_a_stale_qr(s, auth::qr_generation());
    // `link_detail` is sampled from a mutex updated roughly once a poll (~2 s), not from a spring
    // `ui::idle` can already see — so this screen must report the discrete change itself, exactly
    // as `Xfade::tick`/`Spinner::draw` do for their own clocks. Comparing against the LAST DRAWN
    // sentence (not e.g. a bare `unanswered` counter) is what stops a healthy link — where every
    // poll answers and `link_detail` stays `None` forever — from invalidating every frame.
    let detail = auth::link_detail(&auth::link_state());
    if link_detail_changed(&s.last_link_detail, &detail) {
        s.last_link_detail = detail;
        crate::ui::idle::invalidate();
    }
    s.report_alert.update(dt);
    // **Issue #75.** While stuck (not merely erroring), note the trouble only once the screen's
    // own stalled-wait escape is already on offer — the same gate as the escape itself, so the
    // report alert and "press OK for a new code" arrive together rather than the alert jumping the
    // gun on a sign-in that is still perfectly healthy.
    if qr_escape_ready(s) && auth::link_unreachable(&auth::link_state()) {
        auth::note_waiting_trouble();
    }
    if let Some(attempt) = offer_alert(
        s.report_offered_for,
        auth::trouble_snapshot().map(|(a, _, reported)| (a, reported)),
    ) {
        s.report_offered_for = Some(attempt);
        // A failure to send belongs to the press that failed, not to whatever this attempt does
        // next — reset so a stale "couldn't be sent" from an earlier trouble cannot linger over
        // an alert that has not been pressed yet.
        s.report_send_failed = false;
        s.report_alert.open_with_body(REPORT_BODY);
    }
}

/// **Issue #75.** PURE. Should the one-off report alert open now? `offered_for` is the attempt
/// [`Scene::report_alert`] was last opened (or answered) for; `snapshot` is `(attempt,
/// auto_reported)` from [`auth::trouble_snapshot`] when a trouble exists for the CURRENT attempt.
/// Returns the attempt id to open for, or `None` to leave the alert alone.
///
/// Never a bool — comparing attempt IDs rather than "is there a trouble right now" is what stops a
/// fresh flow reset (whose new attempt starts with no trouble at all, then earns one) from being
/// read as "still the trouble already answered".
fn offer_alert(offered_for: Option<u64>, snapshot: Option<(u64, bool)>) -> Option<u64> {
    let (attempt, auto_reported) = snapshot?;
    if auto_reported || offered_for == Some(attempt) {
        return None;
    }
    Some(attempt)
}

/// **Issue #75.** PURE. The caption drawn once a trouble has actually left the television — by
/// either path, the automatic standing-consent send or the one-off press — or, once a deliberate
/// "Send report" press has failed to queue, a caption saying so. `None` while neither is true.
/// `sent` wins over `send_failed` (a trouble the standing path already reported is "sent" even if
/// a later one-off press against a NEW trouble failed to queue — the two can never both be true
/// for the same trouble, since `send_trouble_once` only runs when `!reported`, but the precedence
/// is stated here rather than left as an accident of argument order).
fn report_note(sent: bool, send_failed: bool) -> Option<&'static std::ffi::CStr> {
    if sent {
        Some(c"A report about this sign-in was sent \u{2014} thank you.")
    } else if send_failed {
        Some(c"That report couldn\u{2019}t be sent right now.")
    } else {
        None
    }
}

/// The current attempt's report note, if one is owed — the one call site both [`draw_failed`] and
/// [`draw_waiting`] use, so the two can never disagree about what "sent" (or "failed to send")
/// means. `send_failed` is per-screen-instance state (reset per attempt in [`update`]), not part
/// of `auth::Trouble`, because it describes a UI press outcome the auth layer never needed to know.
fn current_report_note(send_failed: bool) -> Option<&'static std::ffi::CStr> {
    let sent = auth::trouble_snapshot().is_some_and(|(_, _, reported)| reported);
    report_note(sent, send_failed)
}

/// **Issue #75.** Everything the one-off report alert's body states about what it sends — the
/// stage of the sign-in, a coarse class of plex.tv's last answer with its bare status/error code,
/// bucketed try counts and durations, how the sign-in is stored on this television, and this
/// app's version — and what it does not: no PIN, no code, no account, no token, no address, no
/// identifier of any kind. It does NOT name webOS version or TV model —
/// `telemetry::signin::event_body` attaches neither to this report (its only fields are the ones
/// this sentence lists, pinned exactly by `event_body_top_level_keys_are_exact`), and this text
/// used to claim it did, overstating what actually leaves the television. **Issue #76 added the
/// storage clause** — the payload gained `signin.storage`/`contexts.signin.storage` and this text
/// used to fall silent on it, understating what leaves the television instead.
const REPORT_BODY: &str = "This includes the sign-in stage, a class of \
plex.tv\u{2019}s last answer with its bare status or error code, rounded try counts and \
durations, how your sign-in is stored, and this app\u{2019}s version. It never includes your \
PIN, code, account, token, or this television\u{2019}s address, and it carries no identifier.";

/// Is the one-off report alert open (or still fading out)? `app.rs` checks this before its
/// onboarding-screen root BACK rule fires, so the alert can claim BACK for itself instead of
/// backing the whole sign-in out.
///
/// **`visible()`, not `is_open()`.** `DecisionAlert::dismiss` sets `open = false` at once and
/// then fades for a few frames — `is_open()` alone treats that fade as "closed", so a second BACK
/// during it fell through to the root rule and handed the whole screen to the television
/// (`webos::go_home`) while the alert was still visibly on top of it. `visible()` claims input for
/// the remainder of the fade instead, and [`key`]'s own alert branch (below) does the same.
pub fn modal_open() -> bool {
    scene().report_alert.visible()
}

/// **Issue #75 review.** A pointer click at `(mx, my)` while the one-off report alert is open:
/// hits an answer and, when it does, performs exactly the action [`key`]'s `is_ok` branch performs
/// — send (if the hit answer is `Destructive`) then dismiss — refusing entirely when the click
/// misses both buttons.
///
/// **This exists because `app.rs`'s `Route::Login` pointer arm used to synthesize a bare OK key
/// for every click on this route**, which was harmless while the only target was "Try again" but,
/// once this alert could open, meant a click ANYWHERE on the screen — the scrim, outside the
/// panel, a click aimed at "Not now" — activated whichever answer the last D-pad press had
/// focused. Gated on [`DecisionAlert::settled`] for the reason every other pointer caller in this
/// app gates on it (`ui::route_screen`'s rule 11): a fast click during the entrance spring must
/// not land on a panel that is still displaced and nearly invisible.
pub fn alert_press_at(mx: f32, my: f32) -> bool {
    let s = scene();
    if !s.report_alert.is_open() || !s.report_alert.settled() {
        return false;
    }
    if !s.report_alert.press_at(mx, my) {
        return false;
    }
    if s.report_alert.choice() == Choice::Destructive {
        s.report_send_failed = !auth::send_trouble_once();
    }
    s.report_alert.dismiss();
    true
}

/// PURE. Has the sentence the sign-in screen draws under its status changed since the last frame?
/// Trivial today (`!=`), but factored out because it is the one thing standing between a settled
/// FAILED read-out and a per-frame `ui::idle::invalidate()` — a later refinement (e.g. rounding
/// `failing_for` to the second so the trailing "…, 41 s" does not itself force a wake every
/// second) belongs here, not inlined into [`update`].
fn link_detail_changed(prev: &Option<String>, next: &Option<String>) -> bool {
    prev != next
}

/// Release the cached QR texture as soon as it stops describing the code the flow is showing.
///
/// **Keyed on [`auth::qr_generation`], not on the phase, and that swap is this screen's half of
/// issue #30.** The rule used to be "a retry enters `Creating`, so drop it there" — which was true
/// of the only way a code could ever change. It no longer is: a pin that runs out is now replaced
/// automatically, and the flow returns to the same `Waiting` it was already in. A cache keyed on
/// the phase would have gone on drawing the dead code — sharp, scannable, and pointing at a pin
/// plex.tv had forgotten — for the whole of its successor's life. `Creating` is still checked, as
/// the belt to the generation's braces: it is the one moment a flow is known to have thrown its
/// code away before any replacement exists.
///
/// The texture has to be DELETED, not merely forgotten: [`ensure_qr_tex`] allocates a fresh id on
/// every miss (`img_upload_rgba` never reuses the old one), so zeroing the handle alone orphaned a
/// full 400x400-ish RGBA QR bitmap per sign-in retry, with nothing left holding its id to free it
/// later. `gfx::delete_tex` no-ops on 0, and both callers are on the main thread (the app loop's
/// `Route::Login` arm), which is where GL deletes must happen.
fn drop_a_stale_qr(s: &mut Scene, live: u64) {
    if !qr_cache_stale(s.qr_gen, live, auth::phase()) {
        return;
    }
    crate::gfx::delete_tex(s.qr_tex);
    s.qr_tex = 0;
    s.qr_gen = live;
}

/// Whether the cached QR bitmap has stopped describing the code the flow is showing.
///
/// Pure and split out from the delete for the reason every other rule on this screen is: the
/// caller frees a GL texture, so no host test can reach it, and this is the half that decides
/// whether a dead code stays on the television.
fn qr_cache_stale(cached: u64, live: u64, phase: Phase) -> bool {
    cached != live || phase == Phase::Creating
}

/// Decode + upload Plex's QR PNG once, caching the GL texture. Main (draw) thread only.
fn ensure_qr_tex(s: &mut Scene, qr: &auth::QrCode) {
    // The SNAPSHOT's generation, not a fresh read: the texture about to be uploaded and the number
    // it is cached under must come from one lock, or a later frame keys the new bitmap by the old
    // code. Both call sites run the same rule, so a replaced code can never be drawn out of a
    // cache that `update` happened not to have reached yet this frame.
    drop_a_stale_qr(s, qr.generation);
    if s.qr_tex != 0 || qr.png.is_empty() {
        return;
    }
    let (mut w, mut h): (c_int, c_int) = (0, 0);
    let px = crate::img::img_decode_rgba(qr.png.as_ptr(), qr.png.len() as c_int, &mut w, &mut h);
    if !px.is_null() {
        s.qr_tex = crate::img::img_upload_rgba(px, w, h);
        crate::img::img_free(px);
    }
}

pub fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    let s = scene();
    // The QR screen is the first thing a new user sees, before Home has any artwork to lend it.
    // Use the shared pre-content route ground rather than a local grey clear.
    s.ground.draw_default(p);
    let env = Env::inert();

    match auth::phase() {
        Phase::Waiting => draw_waiting(p, &env, s),
        Phase::Error => draw_failed(p, &env, s),
        Phase::Deleted => draw_deleted(p, &env, s),
        Phase::Discovering => draw_working(p, &env, s, "Finding your server\u{2026}"),
        _ => draw_working(p, &env, s, "Connecting to Plex\u{2026}"),
    }
    // Every read-out on this route carries the identification footer (issue #75) — not just the
    // two that used to call it — because a television can die on ANY of these four screens, and a
    // "Connecting to Plex…" spinner stuck on an offline set is exactly the frame most likely to get
    // photographed and posted.
    draw_footer(p);
    // **Issue #75.** The one-off report alert draws LAST, over everything on this route including
    // the footer — its scrim is meant to cover the whole screen it is answering for.
    s.report_alert.draw_scrim();
    s.report_alert.draw(
        c"Send a report about this sign-in problem?",
        c"Not now",
        c"Send report",
    );
}

/// The three non-QR states are ONE centred read-out, not the two-column route.
///
/// **They used to be that route**, with the title and a sentence in the narrative column and a
/// lone spinner floating in the content column — which is the composition for a screen that has a
/// LIST or a document on the right, and reads as a broken one when the right-hand side holds a
/// single 26px ring. `StatusOverlay` is the app's existing answer for "the whole surface is
/// waiting": spinner over verdict over an optional reason over the one action, centred on the area
/// the wait is ABOUT — `Rect::FULL` here, since none of this screen exists yet.
fn draw_readout(
    p: Painter,
    env: &Env,
    s: &Scene,
    caption: &std::ffi::CStr,
    kind: StatusKind,
    reason: Option<&std::ffi::CStr>,
    action: Option<&'static std::ffi::CStr>,
    // The plex.tv link-health sentence (issue #75) — `None` everywhere but [`draw_failed`], the
    // one caller that has something to say about the WIRE rather than the account flow.
    detail: Option<&std::ffi::CStr>,
    // **Issue #75.** "A report about this sign-in was sent" — drawn LAST, below whatever else this
    // read-out drew (the action pill if there is one, the detail sentence if there is one of
    // those too), once the current attempt's trouble has actually left the television.
    note: Option<&std::ffi::CStr>,
) {
    let mut o = StatusOverlay::new(Rect::FULL, caption, kind).phase(s.spin_ms as u32);
    if let Some(r) = reason {
        o = o.reason(r);
    }
    if let Some(a) = action {
        // The only control on the screen, so it holds focus by construction — there is nowhere
        // else for the ring to be, and OK must reach it without a press to move focus first.
        o = o.action(a).focused(true);
    }
    // Computed before the draw (geometry, not paint) and used after it: the detail sentence sits
    // BELOW everything the read-out already draws, and today that is always the action pill —
    // every caller that passes `detail` also passes `action`, which the `expect` below states
    // rather than lets a future caller discover as a mis-placed line.
    // `detail` is only ever passed alongside `action` (every caller today does — `draw_failed` is
    // the only one that passes `detail`, and it always passes `Some(ESCAPE)` with it) — but this
    // draws inside the SDL frame loop, where an unwind kills the app on the television. A future
    // caller that got the pairing wrong should lose the line, not the process.
    debug_assert!(
        detail.is_none() || o.action_frame().is_some(),
        "draw_readout's `detail` is only ever passed alongside an `action`"
    );
    o.draw(env, p);
    let line_h = crate::text::text_height(theme::size::CAPTION, 0);
    // The bottom edge of whatever the read-out has drawn so far — the action pill if there is one,
    // else the panel itself. `detail` and `note` stack below it in that order, each owing the next
    // one `theme::space::SM` of air.
    let mut below = o.action_frame().map(|a| a.y + a.h);
    if let Some(d) = detail {
        let y = below.map_or(o.frame.y + o.frame.h, |b| b + theme::space::SM);
        Label::new(d.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, Rect::new(o.frame.x, y, o.frame.w, line_h));
        below = Some(y + line_h);
    }
    if let Some(n) = note {
        let y = below.map_or(o.frame.y + o.frame.h, |b| b + theme::space::SM);
        Label::new(n.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, Rect::new(o.frame.x, y, o.frame.w, line_h));
    }
}

fn draw_working(p: Painter, env: &Env, s: &Scene, msg: &str) {
    let caption = CString::new(msg).unwrap_or_default();
    let stuck = escape_ready(s);
    draw_readout(
        p,
        env,
        s,
        &caption,
        StatusKind::Working,
        // The reason arrives WITH the control, and only then: it exists to explain why a button
        // just appeared under a spinner that was doing fine a moment ago.
        stuck.then_some(c"This is taking longer than usual."),
        stuck.then_some(ESCAPE),
        None,
        None,
    );
}

fn draw_failed(p: Painter, env: &Env, s: &Scene) {
    let reason = CString::new(auth::error()).unwrap_or_default();
    // A SETTLED read-out, not `auth::link_detail` — this screen has no live poll left to flicker
    // against, so it does not wait for a second miss. That distinction matters on the dominant
    // issue-#75 path: a pin-CREATION failure records exactly one miss and the flow is over, so
    // gating this on two misses meant the curl reason could never reach the one screen the user
    // actually photographs.
    let detail = auth::link_detail_settled(&auth::link_state()).and_then(|d| CString::new(d).ok());
    draw_readout(
        p,
        env,
        s,
        c"Couldn\u{2019}t sign in",
        StatusKind::Failed,
        (!reason.is_empty()).then_some(reason.as_c_str()),
        Some(ESCAPE),
        detail.as_deref(),
        current_report_note(s.report_send_failed),
    );
}

/// What the delete actually achieved, as the two lines it is honest to draw.
///
/// **A partial wipe may not be reported as a whole one**, and that is not pedantry: the files this
/// sweep can fail on include the TELEMETRY decision, so a survivor is re-read on the next launch
/// and a consent the user believed they had deleted comes back. The session is gone either way —
/// `auth::erase_local_state` is unconditional — so the verdict stays true and the reason carries
/// the qualification.
fn deleted_readout(leftovers: usize) -> (&'static std::ffi::CStr, &'static std::ffi::CStr) {
    if leftovers == 0 {
        (
            c"Local data deleted",
            c"Credentials, preferences, telemetry and local diagnostics have been removed.",
        )
    } else {
        (
            c"Signed out, and most local data deleted",
            c"Some files could not be removed and may still be on this television.",
        )
    }
}

/// Record what the delete left behind, before the app routes here.
pub fn note_delete_leftovers(n: usize) {
    scene().delete_leftovers = n;
}

/// **Empty, not Failed.** Deleting everything is a completed action the user asked for, so it must
/// not wear the danger tint — the same distinction `StatusKind::Empty` carries for a library with
/// nothing in it. A partial one is still not a FAILURE either: what it did do, it did.
fn draw_deleted(p: Painter, env: &Env, s: &Scene) {
    let (verdict, reason) = deleted_readout(s.delete_leftovers);
    draw_readout(
        p,
        env,
        s,
        verdict,
        StatusKind::Empty,
        Some(reason),
        Some(c"Sign in"),
        None,
        None,
    );
}

/// PURE. The permanent identification line — issue #75's phone photograph needs to say which
/// build and which television it is looking at, not only what plex.tv is doing. `release` and
/// `set` are already-formatted strings (see [`draw_footer`]) with `"?"` already substituted by
/// the caller for whatever the platform did not answer, so this function never special-cases
/// emptiness itself — it is a plain, deterministic join.
fn footer_line(version: &str, release: &str, set: &str) -> String {
    format!(
        "{} {version} \u{00B7} {release} \u{00B7} {set}",
        crate::plex::identity::PRODUCT
    )
}

/// Draw [`footer_line`] bottom-left inside the safe area, in the family's caption rung and its
/// dimmest ink — a permanent line that must never compete with the read-out it sits under.
///
/// Built from [`crate::webos::Info::release_line`] and [`crate::webos::Hardware::set_line`] —
/// **not** a third, local spelling of "which firmware, which set" — so this screen, the
/// diagnostics panel and the playback failure read-out cannot drift apart on the same two facts,
/// and so the SoC/board field `set_line` carries (the one that actually correlates with a decode
/// or plane failure) reaches the one screen a stranger with a broken sign-in will photograph.
/// `set_line` returns an empty string when nyx never answered; this footer substitutes its own
/// `"?"` for that case, same as an unresolved `release_line`.
fn draw_footer(p: Painter) {
    let release = crate::webos::info().release_line();
    let set = crate::webos::device().set_line();
    let set: &str = if set.is_empty() { "?" } else { &set };
    let line = footer_line(crate::plex::identity::VERSION, &release, set);
    let Ok(text) = CString::new(line) else {
        return;
    };
    let h = crate::text::text_height(theme::size::CAPTION, 0);
    Label::new(text.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .draw(p, Rect::new(SAFE.x, SAFE.y + SAFE.h - h, SAFE.w, h));
}

fn draw_waiting(p: Painter, env: &Env, s: &mut Scene) {
    let layout = RouteLayout::screen();
    layout.draw_narrative(
        p,
        None,
        "Sign in to Plex",
        "Use your phone camera to scan the code, or link this television manually with the address and code shown here.",
        theme::size::LABEL,
    );
    let right = qr_layout(layout);
    // ONE read of the code, used for the bitmap, the digits and the sentence beneath them.
    let qr = auth::qr_snapshot();

    TextView::new("plex.tv/link", theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .draw(p, right.url);

    // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just show it.
    let card = right.card;
    p.rrect(card, 24.0, 24.0, theme::SURFACE_QR_PLATE);
    ensure_qr_tex(s, &qr);
    if s.qr_tex != 0 {
        let pad = 30.0;
        let inner = Rect::new(
            card.x + pad,
            card.y + pad,
            card.w - 2.0 * pad,
            card.h - 2.0 * pad,
        );
        // Plex's PNG is WHITE modules on a transparent ground; tint black so the modules render dark
        // on the white card (the transparent ground shows the card) → a scannable black-on-white QR.
        p.tex(s.qr_tex, inner, 0.0, theme::scrim_black(1.0));
    } else {
        Spinner::new(card.x + card.w * 0.5, card.y + card.h * 0.5, 22.0)
            .phase(s.spin_ms as u32)
            .tint(theme::scrim_black(0.5))
            .draw(env, p);
    }

    // The manual code and waiting state remain in the same right-column stack as the URL and QR.
    // Both use couch-readable type rungs; this is an alternative sign-in path, not fine print.
    if let Ok(code) = CString::new(qr.code.to_uppercase()) {
        p.text(
            code.as_ptr(),
            right.code.cx(),
            right.code.y,
            theme::size::DISPLAY,
            theme::TEXT_PRIMARY,
            1,
            1,
        );
    }

    let link = auth::link_state();
    let wr = 15.0;
    let wy = right.status.cy();
    let status = waiting_status(
        qr.replaced,
        qr_escape_offered(s.phase_ms),
        auth::link_unreachable(&link),
    );
    let status_w = crate::text::text_width(status.as_ptr(), theme::size::BODY, 0);
    let sx = right.status.cx() - (wr * 2.0 + theme::space::SM + status_w) * 0.5;
    Spinner::new(sx + wr, wy, wr)
        .phase(s.spin_ms as u32)
        .tint(theme::TEXT_SECONDARY)
        .draw(env, p);
    let ty = crate::text::text_vcenter_y(theme::size::BODY, 0, wy);
    p.text(
        status.as_ptr(),
        sx + wr * 2.0 + theme::space::SM,
        ty,
        theme::size::BODY,
        theme::TEXT_SECONDARY,
        0,
        0,
    );

    // The plex.tv link-health sentence (issue #75) — silent while the link is healthy, so an
    // ordinary sign-in draws exactly what it always did.
    if let Some(detail) = auth::link_detail(&link) {
        // **`TextView`, not a single-line `Label`.** The longest curl reasons (a TLS/cert failure's
        // sentence) run past 1100px at this rung — well over the 884px right column — and a `Label`
        // neither elides nor wraps, so the tail of exactly the sentence this screen exists to show
        // ran off the panel's right edge. Two lines, word-wrapped by measured pixel width, fit the
        // column the same way every other multi-line block in this app does.
        TextView::new(&detail, theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .max_lines(2)
            .draw(p, right.detail);
    }
    // **Issue #75.** "A report was sent" — drawn once the current attempt's trouble has actually
    // left the television, under the link-health sentence in the same secondary stack.
    if let Some(note) = current_report_note(s.report_send_failed) {
        Label::new(note.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, right.note);
    }
}

/// The line under the code, which has to answer a question that only exists now that a code can be
/// replaced: *why is this not the code I was looking at*.
///
/// A pin lives fifteen minutes and is re-minted when it runs out, so somebody who walked away
/// mid-sign-in — or whose phone has just told them the OLD code was linked — comes back to
/// different digits. Saying nothing there reads as the television having lost track of itself, and
/// it is exactly the moment they need to be told to scan again. Same position, same rung, same
/// spinner: one sentence swapped for another, never a second read-out.
/// **`stalled` outranks `code_replaced`**: one of these sentences carries an ACTION, and a line
/// that explains history is worth less than the one that offers a way forward.
///
/// **`unreachable` outranks BOTH, for the same argument carried one step further (issue #75).** A
/// new code cannot help while plex.tv itself is not answering — pressing OK just mints another pin
/// nobody can poll — so once two consecutive polls have come back empty, the sentence that names
/// the real action (check this TV's connection) is worth more than either a code offer or a
/// history note, exactly as `stalled`'s own sentence already outranks `code_replaced`'s. The
/// [`auth::link_detail`] line drawn under this one carries the *why*; this one only ever needs to
/// say *what to do*.
fn waiting_status(
    code_replaced: bool,
    stalled: bool,
    unreachable: bool,
) -> &'static std::ffi::CStr {
    if unreachable {
        c"Can\u{2019}t reach plex.tv — check this TV\u{2019}s internet connection"
    } else if stalled {
        c"Still waiting — press OK for a new code"
    } else if code_replaced {
        c"That code expired — scan this one"
    } else {
        c"Waiting for you to sign in…"
    }
}

/// How long a QR code may go unscanned before the screen offers to replace it on request.
///
/// **A separate, much longer clock than [`ESCAPE_AFTER_MS`], because this wait is not a stall.**
/// Twelve seconds is right for a spinner that should have finished in one; a code on screen is
/// waiting for a person to find their phone, unlock it, open a camera and tap a link, and nagging
/// them at twelve seconds would be wrong every time. A full minute of a code that has already been
/// scanned is not.
///
/// It exists because the automatic replacement below cannot cover the case the issue reported: the
/// phone says *Account linked* while our polls are being answered `Pending` or nothing at all, and
/// the person watching knows something the television does not. Waiting out the rest of a
/// fifteen-minute lease is not a recovery.
const QR_ESCAPE_AFTER_MS: f32 = 60_000.0;

/// **What the screen is waiting ON**, as the pair the clock in [`Scene::phase_ms`] is timing.
///
/// The phase alone was the whole identity while a code could only change by leaving `Waiting`. It
/// cannot be any more: a pin that runs out is replaced automatically, `Waiting → Creating →
/// Waiting`, and `update` samples once a frame — so a replacement completed between two samples
/// (a paused main loop, a long frame) is invisible, and the FRESH code inherits the dead one's
/// age. It would then offer "press OK for a new code" about a code that had existed for a
/// millisecond. Including the generation makes the reset exact rather than probable.
fn wait_id() -> (Phase, u64) {
    (auth::phase(), auth::qr_generation())
}

/// Is what the screen is waiting on a DIFFERENT thing from what it was waiting on last frame?
///
/// Trivial, and separate anyway, because the rule it encodes is not: a new CODE restarts the clock
/// exactly as a new PHASE does, and the version that compared phases alone is the one that would
/// offer to replace a code a millisecond old.
fn wait_restarted(seen: (Phase, u64), live: (Phase, u64)) -> bool {
    seen != live
}

/// Whether the QR screen is offering its own replacement right now. Pure, and — like
/// [`escape_ready`] — the ONE predicate behind both the sentence and the key, so a control that
/// is not drawn can never be activated.
fn qr_escape_offered(phase_ms: f32) -> bool {
    phase_ms >= QR_ESCAPE_AFTER_MS
}

fn qr_escape_ready(s: &Scene) -> bool {
    auth::phase() == Phase::Waiting && qr_escape_offered(s.phase_ms)
}

/// The complete manual-link stack in the content column.
///
/// The QR itself is centred on the television's Y axis as requested, while the URL, manual code
/// and waiting state flow from its edges. That makes the scan target visually central without
/// separating the fallback credentials into unrelated screen coordinates.
#[derive(Clone, Copy)]
struct QrLayout {
    url: Rect,
    card: Rect,
    code: Rect,
    status: Rect,
    /// The plex.tv link-health sentence (issue #75), directly under `status` — the same secondary
    /// stack, one rung down in size, since it is de-emphasized diagnostic text rather than the
    /// status line's own verdict.
    detail: Rect,
    /// **Issue #75.** "A report about this sign-in was sent", directly under `detail` — drawn only
    /// once the current attempt's trouble has actually been sent, in the same secondary stack.
    note: Rect,
}

fn qr_layout(layout: RouteLayout) -> QrLayout {
    const SIDE: f32 = 420.0;
    let card = Rect::new(
        layout.content.cx() - SIDE * 0.5,
        Rect::FULL.cy() - SIDE * 0.5,
        SIDE,
        SIDE,
    );
    let url_h = theme::size::TITLE as f32 + theme::space::XS;
    let code_h = theme::size::DISPLAY as f32 + theme::space::XS;
    let status_h = theme::size::BODY as f32 + theme::space::SM;
    // Two lines, not one: the diagnostic sentence this band holds can run to the longest curl
    // reason plus a try count and a duration, which does not fit one CAPTION line at this column's
    // 884px width (see `draw_waiting`'s `TextView` call). `TextView`'s own default leading (`sz *
    // 1.32`) is what the wrap actually draws at, so the reserved band uses the same formula rather
    // than a second, driftable one.
    let detail_h = theme::size::CAPTION as f32 * 1.32 * 2.0;
    // **Issue #75 review flagged this line as under-reserved** (`CAPTION`'s bare 24 against
    // `draw_footer`'s own measured ~30px line), and drawing this box a `space::XS` taller was the
    // obvious fix by analogy with `url_h`/`code_h`/`status_h` above — **and it is refused,
    // verified rather than assumed**: at this stack's tallest content (a two-line `detail`
    // sentence, which is exactly when `note` is likeliest to be showing too — both fire once a
    // sign-in is stuck), the taller box pushed `note`'s bottom edge to 1033.36 against `SAFE`'s
    // own 1026, failing `qr_is_vertically_centred_and_the_whole_link_stack_stays_in_the_right_
    // column`'s containment assertion — trading a footer-overlap risk for an outright safe-area
    // violation, a worse defect than the one being fixed. The stack has no slack left to spend at
    // this box alone: closing the gap for real means shrinking a gap earlier in the stack
    // (`card`↔`status`'s `space::LG` + `space::MD`, sized for the ordinary QR/short-code case) or
    // the `detail` reservation itself, both a layout decision beyond a text-and-logic review.
    // `note_h` stays bare `CAPTION`, as `qr_is_vertically_centred_and_the_whole_link_stack_stays_
    // in_the_right_column` already requires; a device capture of the worst case (stuck sign-in,
    // already-sent report, a long curl reason) is what should decide which upstream gap gives.
    let note_h = theme::size::CAPTION as f32;
    let status_y = card.y + card.h + theme::space::LG + code_h + theme::space::MD;
    let detail_y = status_y + status_h + theme::space::SM;
    QrLayout {
        url: Rect::new(
            layout.content.x,
            card.y - theme::space::LG - url_h,
            layout.content.w,
            url_h,
        ),
        card,
        code: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG,
            layout.content.w,
            code_h,
        ),
        status: Rect::new(layout.content.x, status_y, layout.content.w, status_h),
        detail: Rect::new(layout.content.x, detail_y, layout.content.w, detail_h),
        note: Rect::new(
            layout.content.x,
            detail_y + detail_h + theme::space::XS,
            layout.content.w,
            note_h,
        ),
    }
}

pub fn key(sym: c_uint, wcode: c_uint) {
    // **Issue #75.** The one-off report alert claims every key while it is open OR still fading
    // out — nothing reaches the read-out underneath, exactly as `ui::consent`'s delete alert does.
    // `visible()`, not `is_open()`: see [`modal_open`]'s doc for why a second BACK during the exit
    // fade must still be swallowed here rather than falling through to the root rule.
    if scene().report_alert.visible() {
        let s = scene();
        if is_back(sym, wcode) {
            s.report_alert.dismiss();
        } else if sym == SDLK_LEFT {
            s.report_alert.move_focus(-1);
        } else if sym == SDLK_RIGHT {
            s.report_alert.move_focus(1);
        } else if is_ok(sym) {
            if s.report_alert.choice() == Choice::Destructive {
                s.report_send_failed = !auth::send_trouble_once();
            }
            s.report_alert.dismiss();
        }
        return;
    }
    if auth::phase() == Phase::Deleted && is_ok(sym) {
        auth::start_login();
        return;
    }
    if auth::phase() == Phase::Error && is_ok(sym) {
        auth::retry();
        return;
    }
    // **The two timed escapes are ONE press, and they must be, because they share one clock.**
    // The QR screen's *press OK for a new code* (60 s) and the stalled spinner's *Try again*
    // (12 s) both hang off `phase_ms`, so a wait that leaves `Waiting` for `Discovering` between
    // the draw and the key made the first predicate false and the SECOND one true — on the old
    // code's timer, down the unguarded path. `auth::restart_stalled_wait` takes the wait this
    // screen actually timed and refuses if the flow has moved on, so the phase it lands on cannot
    // disagree with the phase that earned the control. A `false` means exactly that happened and
    // the press is swallowed; the main loop is about to route away from here anyway.
    if is_ok(sym) && (qr_escape_ready(scene()) || escape_ready(scene())) {
        // "requested", not "restarted": the press may still be refused a line later, and the
        // event log is the one place this failure is read from — a claim it did something is
        // exactly the wrong thing to have written there.
        crate::log("login: user requested a restart of a stalled sign-in");
        if auth::restart_stalled_wait(scene().wait) {
            // The restart usually re-enters the phase it just left (a stalled `Creating` starts
            // another `Creating`), and `update` only zeroes the clock when the wait's IDENTITY
            // changes — a fresh code changes it, a re-entered phase may not — so without this the
            // new attempt could inherit the dead one's age and show its way out immediately.
            scene().phase_ms = 0.0;
        }
        return;
    }
    // BACK backs out of the sign-in — but only when there is somewhere to back out TO. This screen
    // is reached two ways: a first-ever boot with no session (nothing behind it — the QR screen is
    // the whole app) and the Home account menu's "Sign in" (a working session is still on disk).
    // `auth::cancel` is the one that knows which, so it decides: it resumes the stored session and
    // the main loop routes Home, or reports false and leaves the flow running. In practice this
    // arm is reached only from a path that bypassed `app::key_onboarding`'s root rule — that rule
    // claims every BACK here first and sends a refused one to the television's Home.
    if is_back(sym, wcode) {
        auth::cancel();
    }
    // otherwise the login screen just waits — the pin poll drives the phase from a worker thread.
}

/// The phases that are genuinely WAITING ON A NETWORK CALL and can therefore stall.
///
/// **An allowlist, not "everything that is not terminal".** It was the latter for an hour, which
/// swept in `Ready`, `Profiles` and `Switching` — phases the main loop routes away from on its
/// next pass. Key input is dispatched before that pass, so an OK aimed at the escape control the
/// user could still see would have called `auth::retry` on a flow that had already SUCCEEDED,
/// replacing a completed handoff with a fresh sign-in. `Idle` is excluded for the same reason
/// from the other side: nothing is owed, so there is nothing to retry.
fn working_phase(phase: Phase) -> bool {
    matches!(phase, Phase::Creating | Phase::Discovering)
}

/// Whether the read-out is showing its way out right now.
///
/// One predicate for the draw AND the key handler, so a control that is not drawn can never be
/// activated — the rule `player_hud::transport_hidden` states for the same hazard.
fn escape_ready(s: &Scene) -> bool {
    working_phase(auth::phase()) && escape_offered(s.phase_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;

    /// **A partial wipe may not be reported as a whole one.** The sweep's candidate lists span
    /// both webOS install prefixes and the jail profiles disagree about which are writable, so a
    /// survivor is ordinary — and the survivor can be the TELEMETRY decision, which is then
    /// re-read on the next launch. Saying "telemetry has been removed" over that is the one
    /// sentence on this screen that could be actively false.
    #[test]
    fn a_partial_wipe_does_not_claim_a_whole_one() {
        let (whole, whole_why) = deleted_readout(0);
        let (partial, partial_why) = deleted_readout(2);
        assert_ne!(whole, partial);
        assert!(whole_why.to_bytes().windows(9).any(|w| w == b"telemetry"));
        assert!(
            !partial_why.to_bytes().windows(9).any(|w| w == b"telemetry"),
            "a partial wipe must not name what it may have failed to delete"
        );
        assert!(
            partial.to_bytes().windows(10).any(|w| w == b"Signed out"),
            "…but it still states what it DID do: the session is gone either way"
        );
    }

    /// **A stalled sign-in has to be escapable, and until 2026-09-02 it was not.** BACK on this
    /// screen goes through `auth::cancel`, which resumes a STORED session — on a first-ever boot
    /// there is none, so the key is swallowed by design and the only way out of a hung discovery
    /// was killing the app. The control appears on a clock, so the whole rule is a pure predicate.
    #[test]
    fn a_wait_that_stops_looking_normal_grows_a_way_out() {
        assert!(!escape_offered(0.0), "a fresh wait offers nothing");
        assert!(
            !escape_offered(ESCAPE_AFTER_MS - 1.0),
            "nor does a healthy one — a button that flashes past teaches people to ignore it"
        );
        assert!(escape_offered(ESCAPE_AFTER_MS));
    }

    /// **The escape belongs ONLY to the two phases that wait on a network call.** A terminal
    /// state carries its own control, and — the reason this is an allowlist rather than "not
    /// terminal" — `Ready`, `Profiles` and `Switching` are phases the main loop routes away from
    /// on its NEXT pass. Keys are dispatched before that pass, so an escape offered there could
    /// call `auth::retry` on a flow that had already succeeded and replace the handoff with a
    /// fresh sign-in.
    #[test]
    fn only_a_phase_waiting_on_the_network_can_be_stalled() {
        assert!(working_phase(Phase::Creating));
        assert!(working_phase(Phase::Discovering));
        for settled in [
            Phase::Idle,
            Phase::Waiting,
            Phase::Profiles,
            Phase::Switching,
            Phase::Ready,
            Phase::Error,
            Phase::Deleted,
        ] {
            assert!(
                !working_phase(settled),
                "{settled:?} is not a wait this screen may offer to restart"
            );
        }
    }

    /// **A code that has been replaced may not go on being drawn**, which is the login screen's
    /// half of issue #30. The cache used to be keyed on the phase — sound while the only way to
    /// get a new code was a retry, which passes through `Creating`. A pin that runs out is now
    /// re-minted automatically and the flow returns to the same `Waiting` it was already in, so a
    /// phase-keyed cache would have kept a sharp, scannable QR on screen pointing at a pin plex.tv
    /// had forgotten — for the whole of its successor's life.
    #[test]
    fn a_replaced_code_invalidates_the_cached_qr_even_without_a_phase_change() {
        assert!(
            qr_cache_stale(4, 5, Phase::Waiting),
            "a new code was published while the screen never left Waiting"
        );
        assert!(
            !qr_cache_stale(5, 5, Phase::Waiting),
            "…and the settled case must not re-upload a texture every frame"
        );
        // the belt beside those braces: a flow that has thrown its code away has no successor yet,
        // so there is no generation to compare against, only a phase that says the QR is gone.
        assert!(qr_cache_stale(5, 5, Phase::Creating));
    }

    /// The one line under the code has to explain a swap the user did not ask for — including to
    /// somebody whose phone has just told them the OLD code was linked.
    #[test]
    fn a_swapped_code_says_so_rather_than_changing_under_the_user() {
        let says =
            |s: &std::ffi::CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        assert!(says(waiting_status(false, false, false), b"Waiting"));
        assert!(
            says(waiting_status(true, false, false), b"expired"),
            "it names what happened; a code that simply changes reads as a fault"
        );
        // …and the sentence that carries an ACTION outranks the one that carries history.
        assert!(says(waiting_status(true, true, false), b"press OK"));
        assert!(says(waiting_status(false, true, false), b"press OK"));
    }

    /// **`unreachable` outranks both `stalled` and `code_replaced` (issue #75).** A fresh code or a
    /// "press OK" offer are both things a working plex.tv could act on; while plex.tv itself is not
    /// answering, neither helps, so the sentence that names the real action — check this
    /// television's own connection — wins regardless of what else is true of the wait.
    #[test]
    fn unreachable_outranks_every_other_waiting_sentence() {
        let says =
            |s: &std::ffi::CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        for (code_replaced, stalled) in [(false, false), (true, false), (false, true), (true, true)]
        {
            assert!(
                says(
                    waiting_status(code_replaced, stalled, true),
                    b"Can\xe2\x80\x99t reach plex.tv"
                ),
                "code_replaced={code_replaced} stalled={stalled}: unreachable must win regardless"
            );
        }
        // …and stays silent about the link whenever plex.tv is answering, whatever else is true.
        assert!(!says(
            waiting_status(true, true, false),
            b"Can\xe2\x80\x99t reach plex.tv"
        ));
    }

    /// **The QR screen's clock is not the spinner's, and it must not be.**
    ///
    /// `ESCAPE_AFTER_MS` is 12 s because a discovery spinner should have finished in one. A code
    /// on screen is waiting for a person to find a phone, unlock it, open a camera and tap a link,
    /// so offering to replace it at twelve seconds would be wrong on every healthy sign-in. It is
    /// offered eventually because the automatic replacement cannot cover the reported case: the
    /// phone says *Account linked* while our polls say nothing, and waiting out the rest of a
    /// fifteen-minute lease is not a recovery.
    #[test]
    fn the_qr_screen_offers_a_new_code_on_a_much_longer_clock_than_a_stalled_spinner() {
        assert!(QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0);
        assert!(!qr_escape_offered(0.0));
        assert!(
            !qr_escape_offered(ESCAPE_AFTER_MS),
            "a sign-in that is merely twelve seconds old is going fine"
        );
        assert!(
            QR_ESCAPE_AFTER_MS < 900_000.0,
            "…and it must arrive well inside a code's own fifteen-minute life, or it is not a \
             recovery from anything"
        );
        assert!(qr_escape_offered(QR_ESCAPE_AFTER_MS));
    }

    /// **A new code starts a new clock, even if the phase change between them was never sampled.**
    ///
    /// `update` samples once a frame. An automatic replacement is `Waiting → Creating → Waiting`,
    /// so a long frame or a paused loop can miss the middle entirely — and a clock keyed on the
    /// phase alone would then hand the fresh code its predecessor's age and offer to replace it
    /// immediately.
    #[test]
    fn a_replaced_code_restarts_the_wait_even_when_the_phase_never_appeared_to_change() {
        let old_code = (Phase::Waiting, 7u64);
        assert!(
            !wait_restarted(old_code, (Phase::Waiting, 7)),
            "the same code in the same phase is the same wait, and the clock must keep running"
        );
        assert!(
            wait_restarted(old_code, (Phase::Waiting, 8)),
            "a new code is a new wait, whatever the phase appeared to do in between — this is \
             the case a phase-only comparison misses, and it hands a one-millisecond-old code \
             its predecessor's sixty seconds"
        );
        assert!(
            wait_restarted(old_code, (Phase::Discovering, 7)),
            "…and the original rule still holds: a step forward is a fresh wait"
        );
    }

    #[test]
    fn qr_is_vertically_centred_and_the_whole_link_stack_stays_in_the_right_column() {
        let route = RouteLayout::screen();
        let q = qr_layout(route);
        assert_eq!(q.card.cy(), Rect::FULL.cy());
        // `detail` (issue #75's diagnostic sentence) and `note` (the one-off report's "sent" line)
        // join the same right-column stack the URL, QR and status line already share, and must
        // stay inside the safe area exactly as they do — each is drawn whenever it has something
        // to say, not only in a screenshot taken while designing the layout.
        for r in [q.url, q.card, q.code, q.status, q.detail, q.note] {
            assert!(r.x >= route.content.x);
            assert!(r.x + r.w <= route.content.x + route.content.w);
            assert!(inside_safe(r));
        }
        assert!(
            q.detail.y >= q.status.y + q.status.h,
            "the link-health sentence must sit BELOW the status line, never overlap it"
        );
        assert!(
            q.note.y >= q.detail.y + q.detail.h,
            "the report-sent note must sit BELOW the link-health sentence, never overlap it"
        );
    }

    // ---- issue #75: plex.tv link health on the sign-in screen ----

    /// The footer is a plain, deterministic join — no locale formatting, no truncation — so a
    /// caller can trust its shape without reading the implementation. Built on
    /// `crate::plex::identity::PRODUCT` rather than a literal, so a product rename cannot leave
    /// this the one surface still saying the old name.
    #[test]
    fn footer_line_joins_the_three_facts_with_the_dot_separator() {
        assert_eq!(
            footer_line(
                crate::plex::identity::VERSION,
                "webOS 4.10.2",
                "49SM9000PLA"
            ),
            format!(
                "{} {} \u{00B7} webOS 4.10.2 \u{00B7} 49SM9000PLA",
                crate::plex::identity::PRODUCT,
                crate::plex::identity::VERSION
            )
        );
    }

    /// [`draw_footer`] substitutes `"?"` and `release_line`'s own `"webOS unknown"` before calling
    /// in, and the pure function must not try to be clever about an already-substituted
    /// placeholder — it is just another string here.
    #[test]
    fn footer_line_passes_an_unknown_placeholder_through_unchanged() {
        assert_eq!(
            footer_line("0.7.0-dev", "webOS unknown", "?"),
            format!("{} 0.7.0-dev \u{00B7} webOS unknown \u{00B7} ?", crate::plex::identity::PRODUCT)
        );
    }

    /// The one thing standing between a screen that must keep animating (the link sentence counts
    /// up while plex.tv stays unreachable) and one that must stop (a healthy sign-in, where the
    /// sentence is `None` forever): comparing the drawn value, not a bare "did a poll happen" flag.
    #[test]
    fn link_detail_changed_is_silent_on_a_repeated_value_and_reports_a_real_one() {
        assert!(!link_detail_changed(&None, &None), "healthy the whole time");
        assert!(link_detail_changed(&None, &Some("x".into())), "went bad");
        assert!(
            !link_detail_changed(&Some("a".into()), &Some("a".into())),
            "same sentence redrawn is not a change"
        );
        assert!(
            link_detail_changed(&Some("a".into()), &Some("b".into())),
            "the try count or duration advanced — a real change while still unreachable"
        );
        assert!(link_detail_changed(&Some("x".into()), &None), "recovered");
    }

    // ---- issue #75: the one-off sign-in report alert ----

    /// **A fresh attempt with no trouble yet offers nothing**, whatever this screen last offered
    /// an alert for.
    #[test]
    fn no_trouble_offers_no_alert() {
        assert_eq!(offer_alert(None, None), None);
        assert_eq!(offer_alert(Some(3), None), None);
    }

    /// **A new attempt's trouble is offered exactly once**, and never again for the SAME attempt
    /// once it has been answered — dismissed or sent, `offered_for` records either the same way.
    #[test]
    fn a_trouble_is_offered_once_per_attempt() {
        assert_eq!(
            offer_alert(None, Some((5, false))),
            Some(5),
            "a fresh trouble with nothing offered yet must open"
        );
        assert_eq!(
            offer_alert(Some(5), Some((5, false))),
            None,
            "already offered (and, however it was answered) for this same attempt — must not reopen"
        );
        assert_eq!(
            offer_alert(Some(5), Some((6, false))),
            Some(6),
            "a LATER attempt's trouble is a different question and must open on its own"
        );
    }

    /// **A trouble already sent automatically (standing consent) is never offered as a one-off** —
    /// the person has nothing left to press, whatever this screen has or hasn't offered before.
    #[test]
    fn an_auto_reported_trouble_is_never_offered() {
        assert_eq!(offer_alert(None, Some((5, true))), None);
        assert_eq!(offer_alert(Some(1), Some((5, true))), None);
    }

    /// The "sent" caption draws only once something has actually left the television, "sent"
    /// wins over a stale "failed", and a failed press with nothing sent yet gets its own caption
    /// rather than silence.
    #[test]
    fn the_sent_note_only_draws_once_something_was_actually_sent() {
        assert_eq!(report_note(false, false), None);
        assert!(report_note(true, false).is_some());
        assert_ne!(report_note(true, false), report_note(false, true));
        assert!(report_note(false, true).is_some());
        assert_eq!(
            report_note(true, true),
            report_note(true, false),
            "a sent trouble reads as sent even if some earlier press on it had failed"
        );
    }

    /// **REPORT_BODY must not be truncated by the alert it is drawn in.** The alert's body view
    /// caps at a fixed line count (`decision_alert::DecisionAlert::body_view`, 8 lines) — it used to
    /// be 4, which cut this exact text off mid-sentence and dropped the whole "it carries no
    /// identifier" half, the one privacy disclosure a first-run person sees before pressing Send.
    ///
    /// **A character budget, not a measurement, and that is forced rather than lazy**: the real
    /// wrap goes through `text::text_width`, i.e. SDL2_ttf, and a host test that reaches it does
    /// not fail — it does not LINK (`_TTF_OpenFont` undefined for the test binary; the symbols are
    /// dead-stripped until a test references the path). The budget is the simulator's own number:
    /// at `decision_alert::BODY_W` this body wraps to 7 lines of about 42 characters
    /// (`/tmp/sim-issue75e/failed-alert.png`, 2026-09-10), so 8 lines hold roughly 336 and the
    /// budget keeps one line of slack under that. A longer body needs a new capture, not a bigger
    /// number.
    #[test]
    fn report_body_stays_inside_the_alerts_line_cap() {
        const BUDGET_CHARS: usize = 300;
        let n = REPORT_BODY.chars().count();
        assert!(
            n <= BUDGET_CHARS,
            "REPORT_BODY is {n} characters; over {BUDGET_CHARS} it no longer fits the alert's \
             8-line body cap — shorten it, never let the privacy disclosure fall off silently"
        );
        assert!(
            REPORT_BODY.contains("carries no identifier"),
            "the disclosure's last sentence is the one truncation used to eat"
        );
        assert!(
            REPORT_BODY.contains("how your sign-in is stored"),
            "issue #76 added a storage field to the payload; the disclosure must name it too"
        );
    }
}
