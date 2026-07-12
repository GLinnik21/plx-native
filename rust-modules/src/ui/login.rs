//! The sign-in screen: Plex's own server-rendered QR PNG (fetched by the auth flow, decoded +
//! tinted here) plus the typed short-code fallback, driven by the [`crate::auth`] flow phase.
//! Scanning the QR on a phone opens plex.tv pre-filled with the pin; the flow's background poll
//! then advances us onward.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::consts::*;
use crate::ui::label::HAlign;
use crate::ui::text_view::TextView;
use crate::ui::widgets::Spinner;
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

struct Scene {
    spin_ms: f32,
    qr_tex: u32, // GL texture of Plex's QR PNG (0 until decoded+uploaded)
}

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().expect("login::init not called") }
}

pub fn init() {
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene { spin_ms: 0.0, qr_tex: 0 });
    }
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    // a retry mints a new pin (hence a new QR) — drop the cached texture so it re-uploads.
    if auth::phase() == Phase::Creating {
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
    p.rect(Rect::FULL, 0.0, theme::SURFACE_APP, theme::SURFACE_APP, 0.0);
    let s = scene();
    let env = Env { dt: 0.0, screen: Rect::FULL, fr: 0, fc: 0, sp: 0.0, hero_a: 0.0 };

    match auth::phase() {
        Phase::Waiting => draw_waiting(p, &env, s),
        Phase::Error => draw_status(p, &env, s, &auth::error(), true),
        Phase::Discovering => draw_status(p, &env, s, "Finding your server\u{2026}", false),
        _ => draw_status(p, &env, s, "Connecting to Plex\u{2026}", false),
    }
}

fn draw_waiting(p: Painter, env: &Env, s: &mut Scene) {
    // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just show it.
    let card = Rect::new(360.0, 330.0, 400.0, 400.0);
    p.rrect(card, 24.0, 24.0, theme::FILL_PRIMARY);
    ensure_qr_tex(s);
    if s.qr_tex != 0 {
        let pad = 30.0;
        let inner = Rect::new(card.x + pad, card.y + pad, card.w - 2.0 * pad, card.h - 2.0 * pad);
        // Plex's PNG is WHITE modules on a transparent ground; tint black so the modules render dark
        // on the white card (the transparent ground shows the card) → a scannable black-on-white QR.
        p.tex(s.qr_tex, inner, 0.0, theme::scrim_black(1.0));
    } else {
        Spinner::new(card.x + card.w * 0.5, card.y + card.h * 0.5, 22.0)
            .phase(s.spin_ms as u32)
            .tint(theme::scrim_black(0.5))
            .draw(env, p);
    }

    // right column
    let col_x = 860.0;
    let col_w = 660.0;
    TextView::new("Sign in to Plex", theme::size::HERO, theme::TEXT_PRIMARY)
        .bold()
        .max_lines(1)
        .draw(p, Rect::new(col_x, 348.0, col_w, 90.0));
    TextView::new(
        "Scan the code with your phone camera, or go to plex.tv/link and enter this code:",
        theme::size::BODY,
        theme::TEXT_SECONDARY,
    )
    .leading(38.0)
    .max_lines(3)
    .draw(p, Rect::new(col_x, 452.0, col_w, 130.0));

    // the short code, large
    if let Ok(code) = CString::new(auth::pin_code().to_uppercase()) {
        p.text(code.as_ptr(), col_x, 636.0, theme::size::HERO, theme::TEXT_PRIMARY, 0, 1);
    }

    // waiting status
    Spinner::new(col_x + 16.0, 748.0, 15.0).phase(s.spin_ms as u32).tint(theme::TEXT_TERTIARY).draw(env, p);
    if let Ok(w) = CString::new("Waiting for you to sign in\u{2026}") {
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 0, 748.0);
        p.text(w.as_ptr(), col_x + 44.0, ty, theme::size::CAPTION, theme::TEXT_TERTIARY, 0, 0);
    }
}

fn draw_status(p: Painter, env: &Env, s: &Scene, msg: &str, error: bool) {
    let cx = SCR_W as f32 * 0.5;
    if error {
        TextView::new(msg, theme::size::TITLE, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .max_lines(2)
            .draw(p, Rect::new(cx - 500.0, 452.0, 1000.0, 120.0));
        TextView::new("Press OK to try again", theme::size::BODY, theme::TEXT_TERTIARY)
            .h(HAlign::Center)
            .draw(p, Rect::new(cx - 400.0, 600.0, 800.0, 50.0));
    } else {
        Spinner::new(cx, 470.0, 26.0).phase(s.spin_ms as u32).tint(theme::TEXT_PRIMARY).draw(env, p);
        TextView::new(msg, theme::size::TITLE, theme::TEXT_SECONDARY)
            .h(HAlign::Center)
            .draw(p, Rect::new(cx - 500.0, 552.0, 1000.0, 60.0));
    }
}

pub fn key(sym: c_uint, _wcode: c_uint) {
    if auth::phase() == Phase::Error && is_ok(sym) {
        auth::retry();
    }
    // otherwise the login screen just waits — no exit until sign-in completes.
}
