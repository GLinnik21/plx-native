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
    ground: RouteGround,
    /// How long the CURRENT auth phase has been on screen. Distinct from `spin_ms`, which is a
    /// free-running rotation clock: this one is reset by [`update`] whenever the phase changes,
    /// because the only question it answers is "has this particular wait gone on too long".
    phase_ms: f32,
    phase: Phase,
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
            ground: RouteGround::new(),
            phase_ms: 0.0,
            phase: Phase::default(),
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
    s.phase = auth::phase();
    crate::ui::idle::invalidate();
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    // Each phase gets its own clock. A flow that walks Creating → Waiting → Discovering is making
    // progress, and restarting the timer at every step is what stops a slow-but-healthy sign-in
    // from being offered a way out of itself.
    if s.phase != auth::phase() {
        s.phase = auth::phase();
        s.phase_ms = 0.0;
    } else {
        s.phase_ms += dt * 1000.0;
    }
    // A retry that needs a new account sign-in enters Creating (hence a new QR); a discovery-only
    // retry stays in Discovering and deliberately keeps the already-authorized account credential.
    // Release the cached texture only for the former so its replacement can upload.
    // The GL texture has to be DELETED, not merely forgotten: `ensure_qr_tex` allocates a fresh id
    // on every miss (`img_upload_rgba` never reuses the old one), so zeroing the handle alone
    // orphaned a full 400x400-ish RGBA QR bitmap per sign-in retry, with nothing left holding its
    // id to free it later. `gfx::delete_tex` no-ops on 0, and `update` is main-thread (the app
    // loop's Route::Login arm), which is where GL deletes must happen.
    if auth::phase() == Phase::Creating && s.qr_tex != 0 {
        crate::gfx::delete_tex(s.qr_tex);
        s.qr_tex = 0;
    }
}

/// Decode + upload Plex's QR PNG once, caching the GL texture. Main (draw) thread only.
fn ensure_qr_tex(s: &mut Scene) {
    if s.qr_tex != 0 {
        return;
    }
    let png = auth::qr_png();
    if png.is_empty() {
        return;
    }
    let (mut w, mut h): (c_int, c_int) = (0, 0);
    let px = crate::img::img_decode_rgba(png.as_ptr(), png.len() as c_int, &mut w, &mut h);
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

    TextView::new("plex.tv/link", theme::size::TITLE, theme::TEXT_HEADING)
        .bold()
        .h(HAlign::Center)
        .draw(p, right.url);

    // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just show it.
    let card = right.card;
    p.rrect(card, 24.0, 24.0, theme::SURFACE_QR_PLATE);
    ensure_qr_tex(s);
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
    let pin = auth::pin_code().to_uppercase();
    if let Ok(code) = CString::new(pin) {
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
    let status_w = crate::text::text_width(
        c"Waiting for you to sign in…".as_ptr(),
        theme::size::BODY,
        0,
    );
    let sx = right.status.cx() - (wr * 2.0 + theme::space::SM + status_w) * 0.5;
    Spinner::new(sx + wr, wy, wr)
        .phase(s.spin_ms as u32)
        .tint(theme::TEXT_SECONDARY)
        .draw(env, p);
    if let Ok(w) = CString::new("Waiting for you to sign in\u{2026}") {
        let ty = crate::text::text_vcenter_y(theme::size::BODY, 0, wy);
        p.text(
            w.as_ptr(),
            sx + wr * 2.0 + theme::space::SM,
            ty,
            theme::size::BODY,
            theme::TEXT_SECONDARY,
            0,
            0,
        );
    }
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
    // The escape from a WEDGED sign-in, offered only once the wait has stopped looking normal.
    // Without it this screen had no exit at all on a first-ever boot: `auth::cancel` resumes a
    // stored session and there is none, so BACK is swallowed and the spinner is forever.
    if is_ok(sym) && escape_ready(scene()) {
        crate::log("login: user restarted a stalled sign-in");
        auth::retry();
        // The retry usually re-enters the phase it just left (a stalled `Creating` starts another
        // `Creating`), and `update` only zeroes the clock when the ENUM changes — so without this
        // the fresh attempt inherits the dead one's age and shows its way out immediately.
        scene().phase_ms = 0.0;
        return;
    }
    // BACK backs out of the sign-in — but only when there is somewhere to back out TO. This screen
    // is reached two ways: a first-ever boot with no session (nothing behind it — the QR screen is
    // the whole app) and the Home account menu's "Sign in" (a working session is still on disk).
    // `auth::cancel` is the one that knows which, so it decides: it resumes the stored session and
    // the main loop routes Home, or reports false and we swallow the key, preserving the
    // long-standing "no exit until sign-in completes" behaviour exactly where it belongs.
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
