//! "Who's watching" — the Plex Home profile picker (+ the PIN keypad for protected users).
//!
//! The avatar row is the **shared** [`CardRow`] with a circular [`RowStyle::PROFILES`], so the big
//! round profile pictures get the exact same focus-magnification springs, scroll, and glow ring as
//! the poster/cast/Related shelves — no forked row logic. The screen reads its roster + flow phase
//! from [`crate::auth`]; picking a profile drives `auth::select_profile` / `auth::submit_pin`.
#![allow(non_upper_case_globals)]
use crate::auth::{self, Phase};
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::widgets::{Art, Spinner};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

const ROW_Y: f32 = 384.0;
const PIN_LEN: usize = 4;
const FOOTER_Y: f32 = 780.0; // "Sign out" pill, below the roster/name/error band

// PIN pad geometry: the label, dots and keypad are ONE centered unit (they used three independent
// hard-coded Ys, which left the whole block sitting low on the panel).
const PAD_KEY: f32 = 108.0;
const PAD_KGAP: f32 = 20.0;
const PAD_GRID_H: f32 = 4.0 * PAD_KEY + 3.0 * PAD_KGAP;
const PAD_TITLE_GRID: f32 = 152.0; // title draw-y → keypad top (the unit's overall pacing)
const PAD_DOT: f32 = 18.0; // entry-dot diameter
const PIN_ERR_S: f32 = 1.4; // wrong-PIN red-flash duration (s)

/// (title_y, dots_y, grid_y) with the whole unit — title ink top through keypad bottom —
/// vertically centered on the screen, and the dot row centered in the air between the title's
/// ink bottom and the keypad top (it used to hug the title).
fn pad_geom() -> (f32, f32, f32) {
    let (ct, cb) = crate::text::text_cap_band(theme::size::TITLE, 1);
    let span = PAD_TITLE_GRID + PAD_GRID_H - ct;
    let title_y = (SCR_H - span) * 0.5 - ct;
    let grid_y = title_y + PAD_TITLE_GRID;
    let dots_y = (title_y + cb + grid_y) * 0.5 - PAD_DOT * 0.5;
    (title_y, dots_y, grid_y)
}

// keypad: 4 rows × 3 cols. b'D' = delete (bottom-RIGHT, where every phone dial pad puts it);
// None = an empty (unfocusable) cell.
const KEYS: [[Option<u8>; 3]; 4] = [
    [Some(b'1'), Some(b'2'), Some(b'3')],
    [Some(b'4'), Some(b'5'), Some(b'6')],
    [Some(b'7'), Some(b'8'), Some(b'9')],
    [None, Some(b'0'), Some(b'D')],
];

/// The PIN keypad overlay state (open for a protected profile).
struct Pad {
    open: bool,
    target: usize, // user index the PIN unlocks
    entry: String,
    fr: c_int,
    fc: c_int,
    submitting: bool, // a full PIN is being verified (switch thread in flight) — pad stays up
    error_ms: f32,    // wrong-PIN flash countdown: dots pulse DANGER, entry restarts
}
impl Pad {
    const fn new() -> Self {
        Pad { open: false, target: 0, entry: String::new(), fr: 0, fc: 0, submitting: false, error_ms: 0.0 }
    }
}

struct Scene {
    row: CardRow,
    fc: c_int,
    spin_ms: f32,
    pad: Pad,
    footer: bool, // focus is on the "Sign out" pill under the roster
}

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).as_mut().expect("profiles::init not called") }
}

pub fn init() {
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene { row: CardRow::new(), fc: 0, spin_ms: 0.0, pad: Pad::new(), footer: false });
    }
}

/// Reset focus when the screen (re)appears — call when routing into Profiles.
pub fn enter() {
    let s = scene();
    s.fc = 0;
    s.pad = Pad::new();
    s.footer = false;
}

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    if s.pad.open {
        if s.pad.error_ms > 0.0 {
            s.pad.error_ms = (s.pad.error_ms - dt).max(0.0);
        }
        // a submitted PIN resolves off-thread: success routes the app away (Phase::Ready);
        // dropping back to Profiles means the switch failed. Only a PIN-blaming failure flashes
        // the dots red and stays up (closing the whole pad on a typo made the user re-pick the
        // profile every time); any other failure ("no access to this server", offline) closes the
        // pad so the picker's error banner can say WHY — a red flash there reads as a typo the
        // user would retry forever.
        if s.pad.submitting && auth::phase() == Phase::Profiles {
            if auth::pin_denied() {
                s.pad.submitting = false;
                s.pad.entry.clear();
                s.pad.error_ms = PIN_ERR_S;
            } else {
                s.pad = Pad::new();
            }
        }
    }
    let n = auth::users().len();
    if s.fc as usize >= n.max(1) {
        s.fc = 0;
    }
    // freeze the row focus (springs settle to unfocused) while the keypad is up or while the
    // Sign out footer holds focus — focus is exclusive, never on two controls at once
    let focus = if s.pad.open || s.footer { None } else { Some(s.fc as usize) };
    s.row.update(n, focus, &RowStyle::PROFILES, dt);
}

/// Avatar-row geometry: (first tile's left x before scroll, per-tile stride). Centered when the
/// roster fits, else left-aligned so CardRow can scroll it. Shared by draw + the pointer hit-test.
fn row_geom(n: usize) -> (f32, f32) {
    let sty = RowStyle::PROFILES;
    let slot = sty.w + sty.gap;
    let total = (n as f32 * slot - sty.gap).max(0.0);
    (((SCR_W as f32 - total) * 0.5).max(sty.margin_x), slot)
}

pub fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    p.rect(Rect::FULL, 0.0, theme::SURFACE_APP, theme::SURFACE_APP, 0.0);
    let s = scene();
    let users = auth::users();
    let env = Env { dt: 0.0, screen: Rect::FULL, fr: 0, fc: s.fc, sp: 0.0, hero_a: 0.0 };

    // title
    if let Ok(t) = CString::new("Who's watching?") {
        p.text(t.as_ptr(), SCR_W as f32 * 0.5, 168.0, theme::size::HERO, theme::TEXT_PRIMARY, 1, 1);
    }

    let sty = RowStyle::PROFILES;
    let n = users.len();
    let (start_x, slot) = row_geom(n);
    let scroll = s.row.scroll_x();

    let mut focused: Option<usize> = None;
    for (i, u) in users.iter().enumerate() {
        let cx = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
        let base = Rect::new(cx - sty.w * 0.5, ROW_Y, sty.w, sty.h);
        let sc = s.row.scale(i);
        let is_foc = i as c_int == s.fc && !s.pad.open && !s.footer;
        if is_foc {
            focused = Some(i);
            continue; // draw the focused tile last (ring over neighbours)
        }
        card_row::draw_tile(p, Art::Thumb { key: &u.thumb, res: (300, 300) }, base.scaled(sc), sc, &sty, None);
        draw_name(p, u, cx, false);
    }
    if let Some(i) = focused {
        let u = &users[i];
        let cx = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
        let base = Rect::new(cx - sty.w * 0.5, ROW_Y, sty.w, sty.h);
        let sc = s.row.scale(i);
        card_row::draw_focused(p, Art::Thumb { key: &u.thumb, res: (300, 300) }, base.scaled(sc), sc, &sty, None, std::ptr::null(), std::ptr::null());
        draw_name(p, u, cx, true);
    }

    // "Sign out" — the picker is the only surface a user who doesn't recognise these profiles
    // ever sees, so it must offer a way out of the account.
    if !s.pad.open {
        crate::ui::widgets::Button::new(c"Sign out".as_ptr(), theme::size::BODY, footer_rect())
            .focused(s.footer)
            .draw(&env, p);
    }

    // roster not here yet (persisted seed empty, refresh in flight) — a spinner, not a blank page
    if users.is_empty() {
        Spinner::new(SCR_W as f32 * 0.5, ROW_Y + sty.h * 0.5, 26.0)
            .phase(s.spin_ms as u32)
            .tint(theme::TEXT_PRIMARY)
            .draw(&env, p);
    }

    // a failed switch (wrong PIN, offline) drops the flow back here with an error — show it,
    // or the spinner just vanishes and the picker looks like it ignored the choice.
    let err = auth::error();
    if !err.is_empty() && !s.pad.open && auth::phase() == Phase::Profiles {
        if let Ok(e) = CString::new(err) {
            let ey = crate::text::text_vcenter_y(theme::size::BODY, 0, ROW_Y + sty.h + 96.0);
            p.text(e.as_ptr(), SCR_W as f32 * 0.5, ey, theme::size::BODY, theme::TEXT_SECONDARY, 1, 0);
        }
    }

    // switching spinner / keypad overlay — a near-opaque scrim (0.88): the roster behind is
    // context noise at this point, and a translucent wash read as a rendering glitch.
    match auth::phase() {
        Phase::Switching if !s.pad.open => {
            p.rect(Rect::FULL, 0.0, theme::scrim_black(0.88), theme::scrim_black(0.88), 0.0);
            Spinner::new(SCR_W as f32 * 0.5, 500.0, 26.0)
                .phase(s.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&env, p);
        }
        _ => {}
    }
    if s.pad.open {
        draw_pad(p, &env, s, &users);
    }
}

fn draw_name(p: Painter, u: &auth::UserTile, cx: f32, focused: bool) {
    let col = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let name = crate::text::elide(&u.title, RowStyle::PROFILES.w + RowStyle::PROFILES.gap - 12.0, theme::size::LABEL, if focused { 1 } else { 0 }, false);
    if let Ok(nc) = CString::new(name) {
        p.text(nc.as_ptr(), cx, ROW_Y + RowStyle::PROFILES.h + 28.0, theme::size::LABEL, col, 1, if focused { 1 } else { 0 });
    }
}

fn draw_pad(p: Painter, env: &Env, s: &Scene, users: &[auth::UserTile]) {
    // near-opaque scrim (0.9): PIN entry is its own screen, not a peek-through overlay
    p.rect(Rect::FULL, 0.0, theme::scrim_black(0.9), theme::scrim_black(0.9), 0.0);
    let (title_y, dots_y, _) = pad_geom();
    let name = users.get(s.pad.target).map(|u| u.title.as_str()).unwrap_or("");
    if let Ok(t) = CString::new(format!("Enter {name}'s PIN")) {
        p.text(t.as_ptr(), SCR_W as f32 * 0.5, title_y, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
    }
    // 4 entry dots — replaced by a spinner while the PIN verifies; a rejected PIN pulses the
    // (all-filled) dots DANGER red for PIN_ERR_S, then the entry restarts on the same pad
    if s.pad.submitting {
        Spinner::new(SCR_W as f32 * 0.5, dots_y + PAD_DOT * 0.5, 22.0)
            .phase(s.spin_ms as u32)
            .tint(theme::TEXT_PRIMARY)
            .draw(env, p);
    } else {
        let flash = s.pad.error_ms > 0.0;
        let blink = ((s.pad.error_ms * 5.0) as i32 % 2) == 0; // ~2.5Hz pulse
        let dgap = 34.0f32;
        let dw = PIN_LEN as f32 * PAD_DOT + (PIN_LEN as f32 - 1.0) * dgap;
        let mut dx = SCR_W as f32 * 0.5 - dw * 0.5;
        for i in 0..PIN_LEN {
            let filled = i < s.pad.entry.len();
            let col = if flash {
                theme::with_a(theme::DANGER, if blink { 1.0 } else { 0.35 })
            } else {
                theme::with_a(theme::TEXT_PRIMARY, if filled { 1.0 } else { 0.28 })
            };
            p.rect(Rect::new(dx, dots_y, PAD_DOT, PAD_DOT), PAD_DOT * 0.5, col, col, 0.0);
            dx += PAD_DOT + dgap;
        }
    }
    // keypad grid
    for (r, row) in KEYS.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let Some(k) = cell else { continue };
            let rect = pad_key_rect(r, c);
            let foc = r as c_int == s.pad.fr && c as c_int == s.pad.fc;
            let (fill, ink) = if foc {
                (theme::ACCENT, theme::ACCENT_INK)
            } else {
                (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
            };
            p.rect(rect, 18.0, fill, fill, 0.0);
            if *k == b'D' {
                // a real backspace glyph — the ⌫ codepoint is absent from appfont.ttf (drew blank)
                let d = (rect.w * 0.42).round();
                crate::ui::icons::draw(
                    p,
                    crate::ui::icons::Icon::Backspace,
                    Rect::new(rect.x + (rect.w - d) * 0.5, rect.y + (rect.h - d) * 0.5, d, d),
                    ink,
                );
            } else if let Ok(lc) = CString::new((*k as char).to_string()) {
                let ty = crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
                p.text(lc.as_ptr(), rect.x + rect.w * 0.5, ty, theme::size::TITLE, ink, 1, 1);
            }
        }
    }
    let _ = env;
}

/// Keypad cell geometry — shared by draw_pad and the pointer hit-test.
fn pad_key_rect(r: usize, c: usize) -> Rect {
    let (_, _, grid_y) = pad_geom();
    let gx = SCR_W as f32 * 0.5 - (3.0 * PAD_KEY + 2.0 * PAD_KGAP) * 0.5;
    Rect::new(gx + c as f32 * (PAD_KEY + PAD_KGAP), grid_y + r as f32 * (PAD_KEY + PAD_KGAP), PAD_KEY, PAD_KEY)
}

/// The centered "Sign out" pill under the roster — shared by draw + pointer hit-tests.
fn footer_rect() -> Rect {
    let tw = crate::text::text_width(c"Sign out".as_ptr(), theme::size::BODY, 1);
    let w = tw + 76.0;
    Rect::new((SCR_W - w) * 0.5, FOOTER_Y, w, 60.0)
}

/// The roster tile under the pointer (tile body or its name label; None in the gaps).
fn tile_at(s: &Scene, mx: f32, my: f32) -> Option<usize> {
    let n = auth::users().len();
    if n == 0 {
        return None;
    }
    let sty = RowStyle::PROFILES;
    if my < ROW_Y - 24.0 || my > ROW_Y + sty.h + 56.0 {
        return None;
    }
    let (start_x, slot) = row_geom(n);
    let x = mx - (start_x - s.row.scroll_x());
    if x < 0.0 {
        return None;
    }
    let i = (x / slot) as usize;
    (i < n && x - i as f32 * slot <= sty.w).then_some(i)
}

/// The keypad cell under the pointer (skips the empty cell).
fn pad_key_at(mx: f32, my: f32) -> Option<(c_int, c_int)> {
    for (r, row) in KEYS.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if cell.is_some() && pad_key_rect(r, c).contains(mx, my) {
                return Some((r as c_int, c as c_int));
            }
        }
    }
    None
}

/// Pointer hover: focus follows the cursor (roster tile / Sign out pill, or keypad key while the
/// pad is up).
pub fn pointer_focus(mx: f32, my: f32) {
    let s = scene();
    if s.pad.open {
        if let Some((r, c)) = pad_key_at(mx, my) {
            s.pad.fr = r;
            s.pad.fc = c;
        }
        return;
    }
    if let Some(i) = tile_at(s, mx, my) {
        s.fc = i as c_int;
        s.footer = false;
    } else if footer_rect().contains(mx, my) {
        s.footer = true;
    }
}

/// Pointer click: select the tile / press the keypad key / sign out under the cursor (same
/// actions as OK); a click outside an open keypad dismisses it like BACK.
pub fn click(mx: f32, my: f32) {
    let s = scene();
    if s.pad.open {
        if let Some((r, c)) = pad_key_at(mx, my) {
            s.pad.fr = r;
            s.pad.fc = c;
            if let Some(k) = KEYS[r as usize][c as usize] {
                press(s, k);
            }
        } else {
            s.pad = Pad::new();
        }
        return;
    }
    if let Some(i) = tile_at(s, mx, my) {
        s.fc = i as c_int;
        s.footer = false;
        select(s, i);
    } else if footer_rect().contains(mx, my) {
        auth::sign_out();
    }
}

/// Dev/test hook (`poc-pickuser`): commit roster tile `idx` exactly like OK — a protected tile
/// opens the PIN pad (headless pad capture), an unprotected one switches.
pub fn pick(idx: usize) {
    let s = scene();
    s.fc = idx as c_int;
    select(s, idx);
}

/// Commit a roster tile (OK or pointer click): protected → PIN pad, else switch.
fn select(s: &mut Scene, idx: usize) {
    if auth::users().get(idx).map(|u| u.protected).unwrap_or(false) {
        s.pad = Pad { open: true, target: idx, ..Pad::new() };
    } else {
        auth::select_profile(idx);
    }
}

pub fn key(sym: c_uint, wcode: c_uint) {
    let s = scene();
    if s.pad.open {
        pad_key(s, sym, wcode);
        return;
    }
    let n = auth::users().len() as c_int;
    if sym == SDLK_DOWN {
        s.footer = true; // Sign out pill (reachable even while the roster is empty/loading)
    } else if sym == SDLK_UP {
        s.footer = false;
    } else if s.footer {
        if is_ok(sym) {
            auth::sign_out(); // the phase→route follower lands on the QR sign-in
        }
    } else if n > 0 {
        if sym == SDLK_LEFT {
            s.fc = (s.fc - 1).max(0);
        } else if sym == SDLK_RIGHT {
            s.fc = (s.fc + 1).min(n - 1);
        } else if is_ok(sym) {
            select(s, s.fc as usize);
        }
    }
    // BACK on the picker does nothing — you must choose a profile (or Sign out).
}

/// Remote number key → keypad digit: SDL gives printable keys their ASCII sym and the webOS
/// remote's number buttons carry the same 48–57 ('0'–'9') range in `wcode`.
fn digit_of(sym: c_uint, wcode: c_uint) -> Option<u8> {
    [sym, wcode].into_iter().find(|v| (48..=57).contains(v)).map(|v| v as u8)
}

fn pad_key(s: &mut Scene, sym: c_uint, wcode: c_uint) {
    if is_back(sym, wcode) {
        s.pad = Pad::new();
        return;
    }
    if s.pad.submitting {
        return; // verification in flight — only BACK acts
    }
    if let Some(d) = digit_of(sym, wcode) {
        press(s, d); // remote number buttons type straight into the PIN
        return;
    }
    if sym == SDLK_LEFT {
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, -1, true);
    } else if sym == SDLK_RIGHT {
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, 1, true);
    } else if sym == SDLK_UP {
        s.pad.fr = (s.pad.fr - 1).max(0);
        s.pad.fc = nearest_col(s.pad.fr, s.pad.fc);
    } else if sym == SDLK_DOWN {
        s.pad.fr = (s.pad.fr + 1).min(KEYS.len() as c_int - 1);
        s.pad.fc = nearest_col(s.pad.fr, s.pad.fc);
    } else if is_ok(sym) {
        if let Some(k) = KEYS[s.pad.fr as usize][s.pad.fc as usize] {
            press(s, k);
        }
    }
}

/// Move the keypad column, skipping empty cells; clamps at the row edges.
fn step_focus(fr: c_int, fc: c_int, dir: c_int, _skip: bool) -> c_int {
    let row = &KEYS[fr as usize];
    let mut c = fc + dir;
    while c >= 0 && (c as usize) < row.len() {
        if row[c as usize].is_some() {
            return c;
        }
        c += dir;
    }
    fc
}

/// Nearest occupied column in `fr` to `fc` (for vertical moves onto a row with an empty cell).
fn nearest_col(fr: c_int, fc: c_int) -> c_int {
    let row = &KEYS[fr as usize];
    if row.get(fc as usize).map(|c| c.is_some()).unwrap_or(false) {
        return fc;
    }
    for d in 1..3 {
        for c in [fc - d, fc + d] {
            if c >= 0 && (c as usize) < row.len() && row[c as usize].is_some() {
                return c;
            }
        }
    }
    0
}

fn press(s: &mut Scene, k: u8) {
    if s.pad.submitting {
        return;
    }
    s.pad.error_ms = 0.0; // typing again cancels the wrong-PIN flash
    if k == b'D' {
        s.pad.entry.pop();
        return;
    }
    if s.pad.entry.len() < PIN_LEN {
        s.pad.entry.push(k as char);
    }
    if s.pad.entry.len() == PIN_LEN {
        let (idx, pin) = (s.pad.target, s.pad.entry.clone());
        auth::submit_pin(idx, &pin);
        // the pad STAYS UP: the dot row turns into a spinner, and a rejected PIN flashes the
        // dots red for another try (update() watches the flow phase) — it used to close and
        // dump the user back on the picker for every typo
        s.pad.submitting = true;
    }
}
