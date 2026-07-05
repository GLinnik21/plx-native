//! The player transport HUD (was draw_hud in playback.c). A procedural draw over the
//! gfx/text primitives; reads the live playback state via crate::player (TX +
//! playpos_ns/duration_ns) and the route HUD strings via route::title_cptr/ctxline_cptr.
//! A View/ProgressBar refactor onto retui is a follow-up.
#![allow(dead_code)]
use crate::gfx::{draw_rect, draw_rrect};
use crate::text::draw_text;
use std::ffi::CString;
use std::sync::atomic::Ordering::Relaxed;

const SCR_W: f32 = 1920.0;
const SCR_H: f32 = 1080.0;

fn fmt_time(ns: i64, neg: bool) -> String {
    let t = if ns > 0 { ns / 1_000_000_000 } else { 0 };
    let (h, m, s) = ((t / 3600) as i32, ((t / 60) % 60) as i32, (t % 60) as i32);
    let sign = if neg { "-" } else { "" };
    if h > 0 {
        format!("{sign}{h}:{m:02}:{s:02}")
    } else {
        format!("{sign}{m}:{s:02}")
    }
}

// crude but recognizable control glyphs built from rects
fn icon_subs(x: f32, y: f32, s: f32, c: &[f32; 4]) {
    let d = [0.0f32, 0.0, 0.0, 0.62];
    draw_rect(x, y + s * 0.06, s, s * 0.66, 0.0, 7.0, c.as_ptr(), c.as_ptr(), 0.0);
    draw_rect(x + s * 0.16, y + s * 0.24, s * 0.68, s * 0.10, 0.0, 2.0, d.as_ptr(), d.as_ptr(), 0.0);
    draw_rect(x + s * 0.16, y + s * 0.44, s * 0.42, s * 0.10, 0.0, 2.0, d.as_ptr(), d.as_ptr(), 0.0);
}
fn icon_audio(x: f32, y: f32, s: f32, c: &[f32; 4]) {
    draw_rect(x, y + s * 0.34, s * 0.15, s * 0.42, 0.0, 2.0, c.as_ptr(), c.as_ptr(), 0.0);
    draw_rect(x + s * 0.28, y + s * 0.12, s * 0.15, s * 0.64, 0.0, 2.0, c.as_ptr(), c.as_ptr(), 0.0);
    draw_rect(x + s * 0.56, y + s * 0.26, s * 0.15, s * 0.50, 0.0, 2.0, c.as_ptr(), c.as_ptr(), 0.0);
    draw_rect(x + s * 0.84, y + s * 0.44, s * 0.15, s * 0.32, 0.0, 2.0, c.as_ptr(), c.as_ptr(), 0.0);
}
fn icon_pip(x: f32, y: f32, s: f32, c: &[f32; 4]) {
    let d = [0.0f32, 0.0, 0.0, 0.62];
    draw_rect(x, y + s * 0.10, s, s * 0.62, 0.0, 6.0, c.as_ptr(), c.as_ptr(), 0.0);
    draw_rect(x + s * 0.10, y + s * 0.20, s * 0.80, s * 0.42, 0.0, 4.0, d.as_ptr(), d.as_ptr(), 0.0);
    draw_rect(x + s * 0.46, y + s * 0.36, s * 0.44, s * 0.30, 0.0, 3.0, c.as_ptr(), c.as_ptr(), 0.0);
}
fn draw_iconbtn(x: f32, y: f32, s: f32, which: i32, focused: bool) {
    let (bg, gc) = if focused {
        ([0.96f32, 0.96, 0.98, 0.97], [0.06f32, 0.07, 0.09, 1.0])
    } else {
        ([1.0f32, 1.0, 1.0, 0.15], [0.94f32, 0.95, 1.0, 1.0])
    };
    draw_rect(x, y, s, s, 0.0, 15.0, bg.as_ptr(), bg.as_ptr(), 0.0);
    let pad = s * 0.28;
    let gs = s - 2.0 * pad;
    let (gx, gy) = (x + pad, y + pad);
    match which {
        0 => icon_subs(gx, gy, gs, &gc),
        1 => icon_audio(gx, gy, gs, &gc),
        _ => icon_pip(gx, gy, gs, &gc),
    }
}

/// naive word-wrap to `max` chars/line (on word boundaries)
fn wrap(s: &str, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s.to_string()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// client-rendered subtitle line(s), bottom-center, synced to the video clock. Drawn
/// every frame independent of the transport HUD; hidden when subtitles are off or no
/// cue is active at the current position.
pub(crate) fn draw_subtitles(hud_up: bool) {
    let text = match crate::player::active_subtitle(crate::player::playpos_ns()) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return,
    };
    let mut lines: Vec<String> = Vec::new();
    for seg in text.split('\n') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        for l in wrap(seg, 42) {
            if lines.len() < 3 {
                lines.push(l);
            }
        }
    }
    if lines.is_empty() {
        return;
    }
    let sz = 36;
    let lh = 48.0f32;
    let n = lines.len() as f32;
    let cx = SCR_W * 0.5;
    // sit near the bottom normally; lift above the scrubber/tabs while the HUD is up
    let baseline = if hud_up { SCR_H - 300.0 } else { SCR_H - 100.0 };
    let block_top = baseline - n * lh;
    let white = [1.0f32, 1.0, 1.0, 1.0];
    let outline = [0.0f32, 0.0, 0.0, 0.85];
    for (i, ln) in lines.iter().enumerate() {
        let top = block_top + i as f32 * lh;
        if let Ok(cs) = CString::new(ln.as_str()) {
            // dark outline (4 offsets) then bright white bold text — legible over any scene
            for (dx, dy) in [(-2.0f32, 0.0f32), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0)] {
                draw_text(cs.as_ptr(), cx + dx, top + 4.0 + dy, sz, outline.as_ptr(), 1, 1);
            }
            draw_text(cs.as_ptr(), cx, top + 4.0, sz, white.as_ptr(), 1, 1);
        }
    }
}

pub(crate) fn draw_hud() {
    {
        // bottom scrim: transparent -> dark
        let clr = [0.0f32, 0.0, 0.0, 0.0];
        let drk = [0.0f32, 0.0, 0.0, 0.86];
        draw_rect(0.0, SCR_H - 470.0, SCR_W, 470.0, 0.0, 0.0, clr.as_ptr(), drk.as_ptr(), 0.0);

        let white = [0.98f32, 0.98, 1.0, 1.0];
        let dim = [0.72f32, 0.74, 0.80, 0.95];
        let track = [1.0f32, 1.0, 1.0, 0.24];
        let mx = 90.0f32;

        draw_text(crate::route::ctxline_cptr(), mx, SCR_H - 312.0, 24, dim.as_ptr(), 0, 0);
        draw_text(crate::route::title_cptr(), mx, SCR_H - 278.0, 54, white.as_ptr(), 0, 1);

        // right control buttons: subtitles, audio, pip
        let bs = 58.0f32;
        let bby = SCR_H - 288.0;
        let mut bbx = SCR_W - 90.0 - bs;
        draw_iconbtn(bbx, bby, bs, 2, false);
        bbx -= bs + 22.0;
        draw_iconbtn(bbx, bby, bs, 1, false);
        bbx -= bs + 22.0;
        draw_iconbtn(bbx, bby, bs, 0, false);

        // scrubber
        let sx = mx;
        let sw = SCR_W - 2.0 * mx;
        let sy = SCR_H - 198.0;
        let sh = 8.0f32;
        let scrub = crate::player::TX.scrub_ns.load(Relaxed);
        let dispos = if scrub >= 0 { scrub } else { crate::player::playpos_ns() };
        let dur = crate::player::duration_ns();
        let mut frac = if dur > 0 { dispos as f64 / dur as f64 } else { 0.0 };
        if frac < 0.0 {
            frac = 0.0;
        }
        if frac > 1.0 {
            frac = 1.0;
        }
        draw_rect(sx, sy, sw, sh, 0.0, sh * 0.5, track.as_ptr(), track.as_ptr(), 0.0);
        let fw = (sw as f64 * frac) as f32;
        if fw > sh * 0.5 {
            draw_rrect(sx, sy, fw, sh, sh * 0.5, 0.0, white.as_ptr());
        } else if fw > 0.0 {
            draw_rrect(sx, sy, fw, sh, fw * 0.5, 0.0, white.as_ptr());
        }
        let hx = sx + fw;
        if scrub >= 0 {
            draw_rect(hx - 9.0, sy + sh * 0.5 - 9.0, 18.0, 18.0, 0.0, 9.0, white.as_ptr(), white.as_ptr(), 0.0);
        } else {
            draw_rect(hx - 1.5, sy - 4.0, 3.0, sh + 8.0, 0.0, 0.0, white.as_ptr(), white.as_ptr(), 0.0);
        }

        // elapsed under the playhead; remaining at right; pause glyph only when paused
        let te = CString::new(fmt_time(dispos, false)).unwrap_or_default();
        let tr = CString::new(fmt_time(dur - dispos, true)).unwrap_or_default();
        let ty = sy + 26.0;
        let ew = draw_text(te.as_ptr(), hx - 12.0, ty, 24, white.as_ptr(), 0, 0);
        if crate::player::TX.paused.load(Relaxed) {
            let gx = hx - 12.0 + ew + 14.0;
            let gs = 20.0f32;
            let gyy = ty + 3.0;
            let w = gs * 0.32;
            draw_rect(gx, gyy, w, gs, 0.0, 2.0, white.as_ptr(), white.as_ptr(), 0.0);
            draw_rect(gx + w * 1.9, gyy, w, gs, 0.0, 2.0, white.as_ptr(), white.as_ptr(), 0.0);
        }
        draw_text(tr.as_ptr(), sx + sw, ty, 24, dim.as_ptr(), 2, 0);

        // plain-text tabs
        let tabdim = [0.68f32, 0.70, 0.76, 0.95];
        let py = SCR_H - 122.0;
        let mut px = mx;
        px += draw_text(c"Info".as_ptr(), px, py, 28, white.as_ptr(), 0, 1) + 44.0;
        draw_text(c"Chapters".as_ptr(), px, py, 28, tabdim.as_ptr(), 0, 1);
    }
}
