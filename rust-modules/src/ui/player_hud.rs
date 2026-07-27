//! The player transport HUD. Composed from retui widgets (Spinner / TransportButton / TabPill /
//! ProgressBar-style scrubber) drawn through a `Painter`, reading the live playback state via
//! crate::player (TX + playpos_ns/duration_ns) and the route HUD strings via
//! route::title_cptr/ctxline_cptr. The video-overlay subtitle draws below stay on the raw text/tex
//! primitives (they composite directly over the video plane, outside the transport HUD).
#![allow(dead_code)]
use crate::gfx::{delete_tex, upload_rgba};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::theme;
use crate::ui::widgets::{Spinner, StatusKind, StatusOverlay, TabPill, TransportButton};
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_uint;
use std::sync::atomic::Ordering::Relaxed;

/// The HUD widgets draw purely from their own fields and ignore their Env.
fn hud_env() -> Env {
    Env::inert()
}

// The now-playing title under the playbar is a HUD *display* title — deliberately larger than
// `theme::size::TITLE`, so it sits OUTSIDE the shared type scale as a documented carve-out (like the
// subtitle caption in `draw_subtitles`). Both are media chrome with their own legibility contract and
// both are already well above the couch floor; they're named here rather than left as bare literals.
const HUD_TITLE_SZ: i32 = 54;

/// the shared playback clock ([`crate::ui::fmt::clock`]) with the HUD's leading '-' for remaining
fn fmt_time(ns: i64, neg: bool) -> String {
    let c = crate::ui::fmt::clock(ns / 1_000_000);
    if neg {
        format!("-{c}")
    } else {
        c
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
    let sz = 36; // subtitle caption: media chrome, a documented carve-out from theme::size (see HUD_TITLE_SZ)
    let lh = 48.0f32;
    let n = lines.len() as f32;
    let cx = SCR_W * 0.5;
    // sit near the bottom normally; lift above the scrubber/tabs while the HUD is up
    let baseline = if hud_up { SCR_H - 300.0 } else { SCR_H - 100.0 };
    let block_top = baseline - n * lh;
    let white = [1.0f32, 1.0, 1.0, 1.0]; // subtitles stay pure white for legibility (carve-out)
    let outline = theme::scrim_black(0.85);
    let p = Painter::root();
    for (i, ln) in lines.iter().enumerate() {
        let top = block_top + i as f32 * lh;
        if let Ok(cs) = CString::new(ln.as_str()) {
            // dark outline (4 offsets) then bright white bold text — legible over any scene
            for (dx, dy) in [(-2.0f32, 0.0f32), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0)] {
                p.text(cs.as_ptr(), cx + dx, top + 4.0 + dy, sz, outline, 1, 1);
            }
            p.text(cs.as_ptr(), cx, top + 4.0, sz, white, 1, 1);
        }
    }
}

/// Client-rendered IMAGE subtitles (PGS/VobSub): composite the active decoded bitmap over the
/// video at its 1920×1080 canvas coords (== our UI, so used directly). Caches the GL texture and
/// re-uploads only when the active cue changes (every few seconds). Main-thread only (GL). This
/// is the image counterpart to draw_subtitles — a selected track is either text or image, so at
/// most one of the two draws a cue at a time.
pub(crate) fn draw_subtitle_bitmap() {
    static mut TEX: c_uint = 0;
    static mut KEY: i64 = i64::MIN;
    static mut RECT: (f32, f32, f32, f32) = (0.0, 0.0, 0.0, 0.0);
    unsafe {
        if crate::player::desired_sub_idx() < 0 {
            if TEX != 0 {
                delete_tex(TEX);
                TEX = 0;
            }
            KEY = i64::MIN;
            return;
        }
        match crate::player::active_bitmap_key(crate::player::playpos_ns()) {
            None => KEY = i64::MIN, // gap between cues — draw nothing this frame
            Some(k) => {
                if k != KEY {
                    if let Some((x, y, w, h, rgba)) = crate::player::bitmap_by_key(k) {
                        TEX = upload_rgba(TEX, w, h, rgba.as_ptr());
                        RECT = (x as f32, y as f32, w as f32, h as f32);
                        KEY = k;
                    } else {
                        return; // cue evicted between key lookup and fetch
                    }
                }
                let white = [1.0f32, 1.0, 1.0, 1.0];
                Painter::root().tex(TEX, Rect::new(RECT.0, RECT.1, RECT.2, RECT.3), 0.0, white);
            }
        }
    }
}

// ---- HUD geometry (shared by draw_hud + the pointer hit-tests in app.rs) ----
const SB_X: f32 = 90.0; // scrubber (and title) left margin
pub(crate) const fn sb_w() -> f32 {
    SCR_W - 2.0 * SB_X
}
const SB_Y: f32 = SCR_H - 198.0;
const SB_H: f32 = 8.0;
const BTN_S: f32 = 64.0; // right-side control button size (mockup ≈ 68)
const BTN_GAP: f32 = 22.0;
const BTN_Y: f32 = SCR_H - 288.0;

/// x of control button `idx` (0 = Subtitles on the left, 1 = Audio on the right)
fn btn_x(idx: i32) -> f32 {
    // 80px right margin — matches the track-menu panel (right:80) so their right edges line up
    let audio_x = SCR_W - 80.0 - BTN_S;
    if idx == 0 {
        audio_x - BTN_S - BTN_GAP
    } else {
        audio_x
    }
}

/// which control button a pointer at (cx,cy) is over: 0 = Subtitles, 1 = Audio, or None.
/// The single source of truth for the button rects, shared with draw_hud.
pub(crate) fn icon_hit(cx: f32, cy: f32) -> Option<i32> {
    if cy < BTN_Y || cy > BTN_Y + BTN_S {
        return None;
    }
    (0..2).find(|&idx| {
        let x = btn_x(idx);
        cx >= x && cx <= x + BTN_S
    })
}

/// Pointer hit-test for the scrub-bar grab band (the scrubber's shared geometry, like `icon_hit`
/// for the buttons): `Some(frac 0..1 along the bar)` when (cx,cy) lands in the band. The band is
/// deliberately much taller than the bar itself — a pointer grab zone.
pub(crate) fn scrub_hit(cx: f32, cy: f32) -> Option<f32> {
    let band = cy > SCR_H - 270.0 && cy < SCR_H - 110.0;
    (band && cx >= SB_X && cx <= SB_X + sb_w()).then(|| ((cx - SB_X) / sb_w()).clamp(0.0, 1.0))
}
/// frac along the bar for a drag at `mx` (x only — an engaged drag tracks the pointer even when
/// it wanders off the band vertically).
pub(crate) fn scrub_frac_x(mx: f32) -> f32 {
    ((mx - SB_X) / sb_w()).clamp(0.0, 1.0)
}

/// draw a clock string centred on `cx` as ONE label box, clamped so it stays inside [lo, hi].
/// (The old last-':'-anchor read visibly lopsided once the clock grew to H:MM:SS — "1:05" left of
/// the knob vs "53" right.) The box width is measured on a same-shape template with every digit
/// as '0', so the box — and the returned extents — stay stable while digits tick instead of
/// wobbling with proportional digit widths. Returns the label's (left, right) x extents.
fn draw_clock(p: Painter, text: &str, cx: f32, y: f32, sz: i32, col: [f32; 4], lo: f32, hi: f32) -> (f32, f32) {
    let template: String = text.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
    let w = CString::new(template).ok().map(|t| crate::text::text_width(t.as_ptr(), sz, 1)).unwrap_or(0.0);
    let half = w * 0.5;
    let cx = cx.clamp(lo + half, (hi - half).max(lo + half));
    if let Ok(cs) = CString::new(text) {
        p.text(cs.as_ptr(), cx, y, sz, col, 1, 1);
    }
    (cx - half, cx + half)
}

/// The transport HUD, composed from retui widgets through a root `Painter`.
/// `focus`: 0 = scrubber, 1 = right buttons, 2 = bottom tabs. `btn` (0..1) / `tab` (0..1) are the
/// focused item within their row (only meaningful when `focus` selects that row). `now` drives the
/// loading spinner's rotation. `transport`: draw the scrubber/title/buttons/clocks; pass false for
/// Info mode, where only the scrim + bottom tabs render and the Info card fills the middle.
pub(crate) fn draw_hud(focus: i32, btn: i32, tab: i32, now: u32, transport: bool) {
    let p = Painter::root();
    let e = hud_env();

    // bottom scrim: transparent -> dark
    let clr = theme::scrim_black(0.0);
    let drk = theme::scrim_black(0.86);
    p.rect(Rect::new(0.0, SCR_H - 470.0, SCR_W, 470.0), 0.0, clr, drk, 0.0);

    let white = theme::TEXT_PRIMARY;
    let dim = theme::TEXT_SECONDARY;
    let track = theme::RAIL_TRACK;

    // The load/seek/failure read-out, above the transport. Before this, `PlaybackState` was
    // published every frame and NOTHING read it: the initial load drew a live-looking transport at
    // 0:00 / -0:00, and a dead producer (`PlaybackState::Error`, which is deliberately not
    // `is_busy()`) drew a fully black screen with no message and no hint that BACK is the way out.
    {
        let st = crate::player::state();
        let kind = match st {
            crate::player::PlaybackState::Error => Some(StatusKind::Failed),
            s if s.is_busy() => Some(StatusKind::Working),
            _ => None,
        };
        // A seek keeps its existing in-transport spinner (beside the elapsed clock) — overlaying
        // the centre of the screen for a sub-second reposition would be far noisier than the bug.
        let overlay = matches!(st, crate::player::PlaybackState::Error)
            || (kind.is_some() && crate::player::frames() == 0);
        if let (Some(kind), true) = (kind, overlay) {
            // CString must outlive the draw (Label/StatusOverlay hold a non-owning ptr)
            if let Ok(cs) = CString::new(st.caption()) {
                StatusOverlay::new(Rect::new(0.0, 0.0, SCR_W, SCR_H - 340.0), cs.as_ptr(), kind)
                    .phase(now)
                    .draw(&e, p);
            }
        }
    }

    if transport {
    // title block under the playbar: for an episode, "S1, E1 · Episode Name" (white) sits above the
    // SHOW title; for a movie, the route ctxline over the movie title. (Apple-TV layout.)
    if let Some(n) = crate::metadata::now_playing().filter(|n| n.is_episode) {
        if let Ok(cs) = CString::new(format!("S{}, E{} · {}", n.season, n.index, n.ep_title)) {
            p.text(cs.as_ptr(), SB_X, SCR_H - 312.0, theme::size::CAPTION, white, 0, 1);
        }
        if let Ok(cs) = CString::new(n.title.clone()) {
            p.text(cs.as_ptr(), SB_X, SCR_H - 278.0, HUD_TITLE_SZ, white, 0, 1);
        }
    } else {
        p.text(crate::route::ctxline_cptr(), SB_X, SCR_H - 312.0, theme::size::CAPTION, dim, 0, 0);
        p.text(crate::route::title_cptr(), SB_X, SCR_H - 278.0, HUD_TITLE_SZ, white, 0, 1);
    }

    // right control buttons: Subtitles, Audio
    TransportButton::new(0, Rect::new(btn_x(0), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 0).draw(&e, p);
    TransportButton::new(1, Rect::new(btn_x(1), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 1).draw(&e, p);

    // scrubber
    let sx = SB_X;
    let sw = sb_w();
    let sy = SB_Y;
    let sh = SB_H;
    let scrub = crate::player::TX.scrub_ns.load(Relaxed);
    // while a seek is loading, freeze the playhead at the target (no wobble through the reopen);
    // else follow the live scrub preview, else the real playhead.
    let loading = crate::player::loading();
    let dispos = if loading && crate::player::seek_display_ns() >= 0 {
        crate::player::seek_display_ns()
    } else if scrub >= 0 {
        scrub
    } else {
        crate::player::playpos_ns()
    };
    let dur = crate::player::duration_ns();
    let frac = if dur > 0 { (dispos as f64 / dur as f64).clamp(0.0, 1.0) } else { 0.0 };
    p.rect(Rect::new(sx, sy, sw, sh), sh * 0.5, track, track, 0.0);
    let fw = (sw as f64 * frac) as f32;
    if fw > sh * 0.5 {
        p.rrect(Rect::new(sx, sy, fw, sh), sh * 0.5, 0.0, white);
    } else if fw > 0.0 {
        p.rrect(Rect::new(sx, sy, fw, sh), fw * 0.5, 0.0, white);
    }
    // playhead: a focus-glowing knob when the scrubber is focused, a plain knob while scrubbing,
    // else a thin tick.
    let hx = sx + fw;
    let cy = sy + sh * 0.5;
    if focus == 0 {
        let glow = [1.0f32, 1.0, 1.0, 0.22];
        p.rect(Rect::new(hx - 17.0, cy - 17.0, 34.0, 34.0), 17.0, glow, glow, 0.0);
        p.rect(Rect::new(hx - 11.0, cy - 11.0, 22.0, 22.0), 11.0, white, white, 0.0);
    } else if scrub >= 0 {
        p.rect(Rect::new(hx - 9.0, cy - 9.0, 18.0, 18.0), 9.0, white, white, 0.0);
    } else {
        p.rect(Rect::new(hx - 1.5, sy - 4.0, 3.0, sh + 8.0), 0.0, white, white, 0.0);
    }

    // elapsed under the playhead (':' centered on the knob, clamped to the bar); remaining at the
    // right — hidden once the moving elapsed label would overlap it (near the end of the movie).
    let ty = sy + 30.0;
    let (el_l, el_r) = draw_clock(p, &fmt_time(dispos, false), hx, ty, theme::size::CAPTION, white, sx, sx + sw);
    let rem = fmt_time(dur - dispos, true);
    let rem_w = rem.chars().count() as f32 * theme::size::CAPTION as f32 * 0.52;
    let rem_l = sx + sw - rem_w;
    let rem_shown = el_r + 20.0 < rem_l;
    if rem_shown {
        if let Ok(cs) = CString::new(rem.as_str()) {
            p.text(cs.as_ptr(), sx + sw, ty, theme::size::CAPTION, dim, 2, 0);
        }
    }
    // transport state indicator just past the elapsed clock — a Pause glyph while paused, a loading
    // spinner while a seek resolves, and NOTHING while playing (a state read-out, not an action
    // toggle). Centered on the clock's line box; drops to the clock's LEFT when the right side is
    // against the remaining label / screen edge.
    let paused = crate::player::TX.paused.load(Relaxed);
    if loading || paused {
        // pause bars under-fill their viewBox (14/24 tall) — a 30px box renders ~17px of ink,
        // matching the CAPTION clock's cap height so the glyph reads as the label's size.
        let isz = 30.0f32;
        let need = isz + 6.0;
        let right_ok = el_r + 14.0 + need < if rem_shown { rem_l - 8.0 } else { sx + sw };
        let gx = if right_ok { el_r + 14.0 } else { el_l - 14.0 - need };
        let icy = ty + crate::text::text_height(theme::size::CAPTION, 1) * 0.5; // vertical center of the clock line
        if loading {
            Spinner::new(gx + isz * 0.5, icy, 12.0).phase(now).tint(white).draw(&e, p);
        } else {
            crate::ui::icons::draw(p, crate::ui::icons::Icon::Pause, Rect::new(gx, icy - isz * 0.5, isz, isz), white);
        }
    }
    } // end `if transport`

    // bottom tabs as pills — Chapters only appears when the item actually has chapters
    let tabs: &[&str] = if crate::ui::chapters_panel::has_chapters() {
        &["Info", "Chapters"]
    } else {
        &["Info"]
    };
    // tabs match the transport control buttons' height (BTN_S), centred vertically between the
    // play bar (scrubber, at SB_Y) and the bottom edge of the screen
    let ph = BTN_S; // = 64, same as the Subtitles/Audio buttons
    let py = (SB_Y + SCR_H) * 0.5 - ph * 0.5;
    let mut px = SB_X;
    for (i, label) in tabs.iter().enumerate() {
        let on = focus == 2 && tab == i as i32;
        let pw = TabPill::width(label.chars().count(), theme::size::BODY);
        if let Ok(cs) = CString::new(*label) {
            TabPill::new(cs.as_ptr(), theme::size::BODY, Rect::new(px, py, pw, ph)).focused(on).draw(&e, p);
        }
        px += pw + 16.0;
    }
}
