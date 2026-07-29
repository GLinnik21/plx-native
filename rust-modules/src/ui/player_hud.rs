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
use std::os::raw::{c_int, c_uint};
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

// The control row, exported for `skip_pill` — while a marker is under the playhead the Skip button
// STANDS IN for the two discs, and it has to land on exactly their row and right edge or the
// transport visibly jumps when a segment begins.
/// control-row height (one disc's diameter)
pub(crate) const CTRL_H: f32 = BTN_S;
/// control-row top
pub(crate) const CTRL_Y: f32 = BTN_Y;
/// control-row right edge — 80px margin, matching the track-menu panel (right:80)
pub(crate) const CTRL_RIGHT: f32 = SCR_W - 80.0;
/// width the Subtitles+Audio pair occupies, the floor for anything replacing them
pub(crate) const CTRL_PAIR_W: f32 = 2.0 * BTN_S + BTN_GAP;

/// The shared control-row slot for anything that STANDS IN for the disc pair: right-aligned to the
/// discs' own edge, and never narrower than the pair it replaces so the row does not visibly shrink
/// when it appears. ONE geometry for both stand-ins and for the pointer hit-test.
///
/// The measured width is memoised per label: `text::text_width` is an uncached `TTF_SizeUTF8`, and
/// the labels are compile-time constants whose width can never change — re-measuring them 2-3× a
/// frame is exactly the thrash `text::elide`'s memo exists to avoid.
pub(crate) fn ctrl_slot(label: &str) -> Rect {
    use std::ptr::addr_of_mut;
    const PAD_X: f32 = 34.0;
    static mut MEMO: Vec<(String, f32)> = Vec::new();
    let memo = unsafe { &mut *addr_of_mut!(MEMO) };
    let w = match memo.iter().find(|(l, _)| l == label) {
        Some((_, w)) => *w,
        None => {
            let measured = CString::new(label)
                .ok()
                .map(|c| crate::text::text_width(c.as_ptr(), theme::size::BODY, 1) + 2.0 * PAD_X)
                .unwrap_or(0.0)
                .max(CTRL_PAIR_W);
            // `text_width` reads 0 until `init_text` has run — don't cache a pre-init measurement
            if measured > CTRL_PAIR_W {
                memo.push((label.to_string(), measured));
            }
            measured
        }
    };
    Rect::new(CTRL_RIGHT - w, CTRL_Y, w, CTRL_H)
}

/// What currently occupies the transport's right-hand control row.
///
/// This is the one concept the skip / Up Next feature introduces, and it exists as a TYPE because
/// the alternative — re-deriving the three-way choice at each of the draw, the OK handler, the
/// pointer handler, the LEFT/RIGHT pin and `icon_hit` — encodes its precedence in the order of five
/// separate if-chains, with nothing to keep them agreeing and nothing to test.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlSlot {
    /// the ordinary Subtitles + Audio pair
    Discs,
    /// a marker segment is under the playhead
    Skip(crate::ui::skip_pill::Prompt),
    /// …and the show has another episode queued, which outranks skipping the credits. Carries the
    /// segment for the same reason `Skip` does — so the row has a stable IDENTITY.
    UpNext(crate::metadata::Marker),
}

impl ControlSlot {
    /// How many focusable items the row holds — what the LEFT/RIGHT clamp needs, as DATA instead of
    /// a hand-written `btn = 0` pin beside a `clamp(0, 1)`.
    pub(crate) fn items(self) -> c_int {
        match self {
            ControlSlot::Discs => 2,
            _ => 1,
        }
    }
    /// whether the Subtitles/Audio discs are the current occupant
    pub(crate) fn is_discs(self) -> bool {
        matches!(self, ControlSlot::Discs)
    }
    /// Which SEGMENT this row is offering, if any — the row's identity for "have I already offered
    /// this?". Deliberately not the `ControlSlot` itself: `active_marker` is gated on `is_playing`,
    /// so a momentary drop out of Playing mid-segment reads as "no segment" and flips the slot to
    /// `Discs` and back. Keyed on the segment, that round trip is not a new offer; keyed on the
    /// slot, it was — and every flicker re-raised the HUD over an intro the user was just watching.
    pub(crate) fn offer(self) -> Option<(crate::metadata::MarkerKind, i64)> {
        match self {
            ControlSlot::Discs => None,
            ControlSlot::Skip(pr) => Some((pr.marker.kind, pr.marker.start_ms)),
            ControlSlot::UpNext(m) => Some((m.kind, m.start_ms)),
        }
    }
    /// Pointer hit-test for whatever occupies the row — ONE entry point, so the click path can
    /// never consult geometry belonging to a control that is not on screen.
    pub(crate) fn hit(self, cx: f32, cy: f32) -> bool {
        match self {
            ControlSlot::UpNext(_) => crate::ui::up_next::hit(cx, cy),
            ControlSlot::Skip(pr) => crate::ui::skip_pill::rect(pr).contains(cx, cy),
            ControlSlot::Discs => false,
        }
    }
}

/// PURE precedence: given the segment under the playhead and whether the queue has a successor,
/// which control owns the row. Host-testable, and the ONLY place the ordering is written down.
///
/// Up Next outranks Skip Credits deliberately: with somewhere to go, "next episode" is the better
/// offer, and Skip Credits stays for the last episode of a show, where there is nowhere to go.
pub(crate) fn slot_for(marker: Option<crate::metadata::Marker>, has_next: bool) -> ControlSlot {
    match marker {
        Some(m) => {
            let pr = crate::ui::skip_pill::prompt_for(m);
            if has_next && m.kind == crate::metadata::MarkerKind::Credits {
                ControlSlot::UpNext(m)
            } else {
                ControlSlot::Skip(pr)
            }
        }
        None => ControlSlot::Discs,
    }
}

/// Sample the live globals ONCE and resolve the row. Call this once per frame and pass the result
/// around: `playpos_ns` is written by LG's media thread and `player::pump` runs between the input
/// handlers and the draw, so re-deriving per call site let a keypress dispatch to a control that
/// the same frame then declined to draw.
pub(crate) fn slot() -> ControlSlot {
    slot_for(crate::metadata::active_marker(), crate::route::up_next().is_some())
}

/// x of control button `idx` (0 = Subtitles on the left, 1 = Audio on the right)
fn btn_x(idx: i32) -> f32 {
    let audio_x = CTRL_RIGHT - BTN_S;
    if idx == 0 {
        audio_x - BTN_S - BTN_GAP
    } else {
        audio_x
    }
}

/// which control button a pointer at (cx,cy) is over: 0 = Subtitles, 1 = Audio, or None.
/// The single source of truth for the button rects, shared with draw_hud.
///
/// `slot` is passed in rather than re-derived: while a stand-in owns the row the discs are not on
/// screen, and a click in that band must not open a track menu the user cannot see.
pub(crate) fn icon_hit(slot: ControlSlot, cx: f32, cy: f32) -> Option<i32> {
    if !slot.is_discs() || cy < BTN_Y || cy > BTN_Y + BTN_S {
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
/// `focus`: 0 = scrubber, 1 = the right control row, 2 = bottom tabs. `btn` (0..1) / `tab` (0..1) are the
/// focused item within their row (only meaningful when `focus` selects that row). `now` drives the
/// loading spinner's rotation. `transport`: draw the scrubber/title/buttons/clocks; pass false for
/// Info mode, where only the scrim + bottom tabs render and the Info card fills the middle.
pub(crate) fn draw_hud(slot: ControlSlot, focus: i32, btn: i32, tab: i32, now: u32, transport: bool) {
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
        // A seek keeps its existing in-transport spinner (beside the elapsed clock): overlaying
        // the centre of the screen for a sub-second reposition would be noisier than the bug.
        let kind = match st {
            crate::player::PlaybackState::Error => Some(StatusKind::Failed),
            s if s.is_busy() && crate::player::frames() == 0 => Some(StatusKind::Working),
            _ => None,
        };
        if let Some(kind) = kind {
            // sits above the transport block, whose geometry is the documented SCR_H-offset carve-out
            const OVERLAY_BOTTOM: f32 = 340.0;
            StatusOverlay::new(Rect::new(0.0, 0.0, SCR_W, SCR_H - OVERLAY_BOTTOM), st.caption(), kind)
                .phase(now)
                .draw(&e, p);
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

    // The right control row, from the slot the CALLER resolved — so what is drawn and what a
    // keypress activates are the same value, not two derivations of it.
    match slot {
        ControlSlot::UpNext(_) => crate::ui::up_next::draw(p, focus == 1, now),
        ControlSlot::Skip(pr) => crate::ui::skip_pill::draw(p, pr, focus == 1),
        ControlSlot::Discs => {
            TransportButton::new(0, Rect::new(btn_x(0), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 0).draw(&e, p);
            TransportButton::new(1, Rect::new(btn_x(1), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 1).draw(&e, p);
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{Marker, MarkerKind};
    use crate::ui::skip_pill::SkipAction;

    fn marker(kind: MarkerKind, final_seg: bool) -> Marker {
        Marker { kind, start_ms: 1_000, end_ms: 2_000, final_seg }
    }

    /// The control row's precedence, which used to live in the ORDER of five separate if-chains
    /// across `player_hud` and `app.rs` — the draw, the OK handler, the pointer handler, the
    /// LEFT/RIGHT clamp and `icon_hit` — with nothing keeping them in step and nothing to test.
    #[test]
    fn the_control_row_picks_the_most_specific_occupant() {
        // no segment under the playhead → the ordinary Subtitles + Audio pair
        assert!(slot_for(None, false).is_discs());
        assert!(slot_for(None, true).is_discs(), "a queued successor alone changes nothing");
        assert_eq!(slot_for(None, true).items(), 2, "the disc pair is the only two-item row");

        // an intro is always Skip, successor or not — "what's next" is an end-of-episode idea
        for has_next in [false, true] {
            let slot = slot_for(Some(marker(MarkerKind::Intro, false)), has_next);
            assert!(matches!(slot, ControlSlot::Skip(p) if p.kind == MarkerKind::Intro));
            assert_eq!(slot.items(), 1, "a stand-in is the row's only item");
        }

        // credits WITH somewhere to go → Up Next outranks Skip Credits…
        assert!(matches!(slot_for(Some(marker(MarkerKind::Credits, true)), true), ControlSlot::UpNext(_)));
        // …and WITHOUT (a show's last episode) it stays Skip Credits, which is the whole reason
        // both still exist
        assert!(matches!(
            slot_for(Some(marker(MarkerKind::Credits, true)), false),
            ControlSlot::Skip(p) if p.kind == MarkerKind::Credits
        ));
    }

    /// A `final` credits segment runs to the end of the item, so skipping it FINISHES rather than
    /// seeks — seeking to its stated end would race the decoder against its own last frames.
    #[test]
    fn only_a_final_credits_segment_finishes_the_item() {
        let fin = match slot_for(Some(marker(MarkerKind::Credits, true)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(fin, SkipAction::Finish);

        // a mid-item credits segment (a post-credits scene follows) seeks past it and plays on
        let mid = match slot_for(Some(marker(MarkerKind::Credits, false)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(mid, SkipAction::Seek(2_000 * 1_000_000));
        // …as does an intro, `final` flag or not (PMS only sets it on credits)
        let intro = match slot_for(Some(marker(MarkerKind::Intro, true)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(intro, SkipAction::Seek(2_000 * 1_000_000));
    }
}
