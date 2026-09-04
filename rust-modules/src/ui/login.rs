//! The sign-in screen: Plex's own server-rendered QR PNG (fetched by the auth flow, decoded +
//! tinted here) plus the typed short-code fallback, driven by the [`crate::auth`] flow phase.
//! Scanning the QR on a phone opens plex.tv pre-filled with the pin; the flow's background poll
//! then advances us onward.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::consts::*;
use crate::ui::label::HAlign;
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
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene {
            spin_ms: 0.0,
            qr_tex: 0,
            qr_gen: auth::qr_generation(),
            ground: RouteGround::new(),
            phase_ms: 0.0,
            wait: wait_id(),
            delete_leftovers: 0,
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
    o.draw(env, p);
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
    );
}

fn draw_failed(p: Painter, env: &Env, s: &Scene) {
    let reason = CString::new(auth::error()).unwrap_or_default();
    draw_readout(
        p,
        env,
        s,
        c"Couldn\u{2019}t sign in",
        StatusKind::Failed,
        (!reason.is_empty()).then_some(reason.as_c_str()),
        Some(ESCAPE),
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
    );
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

    let wr = 15.0;
    let wy = right.status.cy();
    let status = waiting_status(qr.replaced, qr_escape_offered(s.phase_ms));
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
fn waiting_status(code_replaced: bool, stalled: bool) -> &'static std::ffi::CStr {
    if stalled {
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
        status: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG + code_h + theme::space::MD,
            layout.content.w,
            status_h,
        ),
    }
}

pub fn key(sym: c_uint, wcode: c_uint) {
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
        assert!(says(waiting_status(false, false), b"Waiting"));
        assert!(
            says(waiting_status(true, false), b"expired"),
            "it names what happened; a code that simply changes reads as a fault"
        );
        // …and the sentence that carries an ACTION outranks the one that carries history.
        assert!(says(waiting_status(true, true), b"press OK"));
        assert!(says(waiting_status(false, true), b"press OK"));
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
        for r in [q.url, q.card, q.code, q.status] {
            assert!(r.x >= route.content.x);
            assert!(r.x + r.w <= route.content.x + route.content.w);
            assert!(inside_safe(r));
        }
    }
}
