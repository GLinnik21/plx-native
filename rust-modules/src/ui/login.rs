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
use crate::ui::widgets::Spinner;
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

struct Scene {
    spin_ms: f32,
    qr_tex: u32, // GL texture of Plex's QR PNG (0 until decoded+uploaded)
    ground: RouteGround,
}

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
        });
    }
}

/// Mount the auth route without replacing the cached QR texture. A fresh auth flow invalidates the
/// texture from [`update`] when it reaches `Creating`; this only resets the visit's visual ground.
pub fn enter() {
    scene().ground.reset();
    crate::ui::idle::invalidate();
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
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
        Phase::Error => draw_status(p, &env, s, &auth::error(), true),
        Phase::Discovering => draw_status(p, &env, s, "Finding your server\u{2026}", false),
        _ => draw_status(p, &env, s, "Connecting to Plex\u{2026}", false),
    }
}

fn draw_waiting(p: Painter, env: &Env, s: &mut Scene) {
    let layout = RouteLayout::screen();
    layout.draw_narrative(
        p,
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

fn draw_status(p: Painter, env: &Env, s: &Scene, msg: &str, error: bool) {
    let layout = RouteLayout::screen();
    layout.draw_narrative(
        p,
        if error {
            "Couldn’t sign in"
        } else {
            "Sign in to Plex"
        },
        msg,
        theme::size::LABEL,
    );
    let cx = layout.content.cx();
    let cy = layout.content.cy();
    if error {
        TextView::new(
            "Press OK to try again",
            theme::size::BODY,
            theme::TEXT_TERTIARY,
        )
        .h(HAlign::Center)
        .draw(p, Rect::new(layout.content.x, cy, layout.content.w, 50.0));
    } else {
        Spinner::new(cx, cy - theme::space::LG, 26.0)
            .phase(s.spin_ms as u32)
            .tint(theme::TEXT_PRIMARY)
            .draw(env, p);
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
    if auth::phase() == Phase::Error && is_ok(sym) {
        auth::retry();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;

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
