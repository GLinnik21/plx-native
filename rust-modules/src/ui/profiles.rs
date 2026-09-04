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
use crate::ui::route_screen::RouteGround;
use crate::ui::widgets::{Art, Spinner};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::addr_of_mut;

/// The screen's own heading, and — since the consent route names where BACK goes — the word
/// another screen has to be able to say about this one. One constant, so the crumb and the
/// heading cannot drift apart.
pub(crate) const TITLE: &str = "Who's watching?";

const ROW_Y: f32 = 384.0;
/// Name band offset below `ROW_Y`. Anchored to the UNSCALED row like every `CardRow` label (a pop
/// must never shove the label), but spaced off the FOCUSED tile: at `focus_scale` the circle's
/// bottom edge drops `h·(focus_scale−1)/2`, and the name still keeps a full `space::MD` of air
/// under it. Derived, so raising the pop can't silently collide the two.
const NAME_DY: f32 = RowStyle::PROFILES.h
    + RowStyle::PROFILES.h * (RowStyle::PROFILES.focus_scale - 1.0) * 0.5
    + theme::space::MD;
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
const PIN_ERR_HALF_S: f32 = 0.175; // …and one half-cycle of it, so the window is four full blinks

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
    /// Wrong-PIN flash: SECONDS still to run (it is stepped by `dt`, which is seconds — the field
    /// was called `error_ms` and held neither). Zero means no flash. While it runs the dot row
    /// blinks DANGER and the entry has been restarted.
    error_s: f32,
}
impl Pad {
    const fn new() -> Self {
        Pad {
            open: false,
            target: 0,
            entry: String::new(),
            fr: 0,
            fc: 0,
            submitting: false,
            error_s: 0.0,
        }
    }
}

struct Scene {
    row: CardRow,
    fc: c_int,
    spin_ms: f32,
    pad: Pad,
    footer: bool, // focus is on the "Sign out" pill under the roster
    ground: RouteGround,
}

static mut SCENE: Option<Scene> = None;

fn scene() -> &'static mut Scene {
    unsafe {
        (*addr_of_mut!(SCENE))
            .as_mut()
            .expect("profiles::init not called")
    }
}

pub fn init() {
    unsafe {
        *addr_of_mut!(SCENE) = Some(Scene {
            row: CardRow::new(),
            fc: 0,
            spin_ms: 0.0,
            pad: Pad::new(),
            footer: false,
            ground: RouteGround::new(),
        });
    }
}

/// Reset focus when the screen (re)appears — call when routing into Profiles.
pub fn enter() {
    let s = scene();
    s.fc = 0;
    s.pad = Pad::new();
    // …and no verdict about the last keypad follows a fresh picker onto the screen (`close_pad`).
    auth::dismiss_pin_error();
    s.footer = false;
    s.ground.reset();
    crate::ui::idle::invalidate();
}

/// The footer control's FOCUS POP ([`crate::ui::widgets::CtlPop`]) — one control, and it still gets
/// a spring rather than a bare `if focused`: focus moves between the avatar row and this footer, so
/// the pill has to animate BOTH ways, and it is the only mark it has (a lone capsule below a row of
/// faces has nothing beside it to be compared against).
static mut FOOTER_POP: crate::ui::widgets::CtlPop<1> = crate::ui::widgets::CtlPop::new();

pub fn update(dt: f32) {
    let s = scene();
    s.spin_ms += dt * 1000.0;
    // …closed while the PIN pad is up, which is also when the control is not drawn at all.
    unsafe {
        (*std::ptr::addr_of_mut!(FOOTER_POP)).step((s.footer && !s.pad.open).then_some(0), dt)
    };
    if s.pad.open {
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
                s.pad.error_s = PIN_ERR_S;
                // the spinner has just become a flashing dot row, and no spring said so
                crate::ui::idle::invalidate();
            } else {
                close_pad(s);
            }
        }
        // AFTER that block, so the frame a rejection lands on is also the flash's first frame.
        step_pin_flash(&mut s.pad, dt);
    }
    let n = auth::users().len();
    if s.fc as usize >= n.max(1) {
        s.fc = 0;
    }
    // freeze the row focus (springs settle to unfocused) while the keypad is up or while the
    // Sign out footer holds focus — focus is exclusive, never on two controls at once
    let focus = if s.pad.open || s.footer {
        None
    } else {
        Some(s.fc as usize)
    };
    s.row.update(n, focus, &RowStyle::PROFILES, dt);
}

/// Take the keypad down, and retire the PIN verdict with it.
///
/// The pad is the ONLY surface that asks about a PIN, so it is the only one that may answer about
/// one: a `pin_denied` left standing after the keypad is gone is a verdict about a control that is
/// no longer on screen, and the roster behind it is a screen where every profile is a candidate.
/// The banner half of the same rule is `auth::switch_failure`, which no longer writes one at all.
///
/// **Every door out of the pad comes through here**, and there are THREE rather than the two that
/// are obvious from the key handler: BACK ([`pad_key`]), the non-PIN failure that closes the pad
/// ([`update`]), and a pointer click OUTSIDE the keypad ([`apply_pad_click`]) — the pad is not a
/// press surface, so that dismissal is handled on the button-down in a different function and was
/// left behind by the first version of this rule.
///
/// **It takes the pad down; it does not CANCEL a submission.** Closing while a PIN is still in
/// flight leaves that worker running under its own auth epoch, so a switch the user walked away
/// from can still land — as `Phase::Ready` if the PIN was right (they entered it; nothing is
/// bypassed, and cancelling would instead strand a correct sign-in), or by re-raising the verdict
/// this call just cleared if it was wrong. Pre-existing, and not a PIN boundary either way: the
/// roster banner a late rejection could reach no longer exists (`auth::switch_failure`).
fn close_pad(s: &mut Scene) {
    s.pad = Pad::new();
    auth::dismiss_pin_error();
    crate::ui::idle::invalidate(); // the whole overlay just left the screen
}

/// Advance the wrong-PIN flash by one frame, and ask [`crate::ui::idle`] for a frame on the phase
/// FLIPS alone — the two halves `ui/CLAUDE.md` demands of anything that animates from a CLOCK.
///
/// The gate detects motion by watching the two spring integrators, so a countdown is invisible to
/// it by construction; without this call the flash drew whatever half-cycle the last spinner frame
/// happened to buy and then froze, which is the whole of the reported "the dots only become
/// slightly red". Reporting only on the flips is the other half: eight repaints, not eighty-four.
fn step_pin_flash(pad: &mut Pad, dt: f32) {
    if pad.error_s <= 0.0 {
        return;
    }
    let was = pin_flash(pad.error_s);
    pad.error_s = (pad.error_s - dt).max(0.0);
    if pin_flash(pad.error_s) != was {
        crate::ui::idle::invalidate();
    }
}

/// Which half of the flash cycle the pad is in — `None` once the flash is over, `Some(true)` on a
/// lit half. Pure, and separate so [`step_pin_flash`] and [`draw_pad`] read ONE expression.
///
/// Phased off **elapsed** rather than remaining time, which is not a stylistic choice: keyed off
/// the remainder, the opening frame's bucket is `PIN_ERR_S / PIN_ERR_HALF_S` rounded by whatever
/// the two `f32`s happen to divide to, and it landed on the DIM half — so the one frame the frozen
/// present gate ever showed was the faint one. Elapsed starts at exactly `0.0`, so a rejected PIN
/// is red on the frame it is rejected, at every duration anyone might later pick.
fn pin_flash(error_s: f32) -> Option<bool> {
    (error_s > 0.0).then(|| (((PIN_ERR_S - error_s) / PIN_ERR_HALF_S) as i32) % 2 == 0)
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
    let s = scene();
    s.ground.draw_default(p);
    let users = auth::users();
    let env = Env {
        dt: 0.0,
        screen: Rect::FULL,
        fr: 0,
        fc: s.fc,
        sp: 0.0,
        hero_a: 0.0,
    };

    // The picker and its PIN pad keep their established centred composition. They only share the
    // pre-content ambient ground with the route screens; becoming two columns would make the
    // familiar profile interaction less direct without adding any useful information hierarchy.
    if s.pad.open {
        draw_pad(p, &env, s, &users);
        return;
    }

    if let Ok(t) = CString::new(TITLE) {
        p.text(
            t.as_ptr(),
            SCR_W as f32 * 0.5,
            168.0,
            theme::size::HERO,
            theme::TEXT_PRIMARY,
            1,
            1,
        );
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
        let is_foc = i as c_int == s.fc && !s.footer;
        if is_foc {
            focused = Some(i);
            continue; // draw the focused tile last (ring over neighbours)
        }
        card_row::draw_tile(
            p,
            Art::Thumb {
                sid: crate::plex::current_server(),
                key: &u.thumb,
                res: (300, 300),
            },
            base.scaled(sc),
            sc,
            &sty,
            None,
        );
        draw_name(p, u, cx, false);
    }
    if let Some(i) = focused {
        let u = &users[i];
        let cx = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
        let base = Rect::new(cx - sty.w * 0.5, ROW_Y, sty.w, sty.h);
        // fold the ui::press click dip into the focused avatar's pop (1.0 when idle)
        let sc = s.row.scale(i) * crate::ui::press::scale();
        // the roster draws its own names below the avatars, so the tile carries no label block
        card_row::draw_focused(
            p,
            Art::Thumb {
                sid: crate::plex::current_server(),
                key: &u.thumb,
                res: (300, 300),
            },
            base.scaled(sc),
            sc,
            &sty,
            None,
            &card_row::TileLabel::default(),
        );
        draw_name(p, u, cx, true);
    }

    // "Sign out" — the picker is the only surface a user who doesn't recognise these profiles
    // ever sees, so it must offer a way out of the account.
    crate::ui::widgets::Button::new(c"Sign out".as_ptr(), theme::size::BODY, footer_rect())
        .focused(s.footer)
        .scale(unsafe { std::ptr::addr_of!(FOOTER_POP).as_ref().unwrap().scale(0) })
        .palette(s.ground.palette())
        .draw(&env, p);

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
            p.text(
                e.as_ptr(),
                SCR_W as f32 * 0.5,
                ey,
                theme::size::BODY,
                theme::TEXT_SECONDARY,
                1,
                0,
            );
        }
    }

    if auth::phase() == Phase::Switching {
        p.rect(
            Rect::FULL,
            0.0,
            theme::scrim_black(0.88),
            theme::scrim_black(0.88),
            0.0,
        );
        Spinner::new(SCR_W as f32 * 0.5, 500.0, 26.0)
            .phase(s.spin_ms as u32)
            .tint(theme::TEXT_PRIMARY)
            .draw(&env, p);
    }
}

fn draw_name(p: Painter, u: &auth::UserTile, cx: f32, focused: bool) {
    let col = if focused {
        theme::TEXT_PRIMARY
    } else {
        theme::TEXT_SECONDARY
    };
    let name = crate::text::elide(
        &u.title,
        RowStyle::PROFILES.w + RowStyle::PROFILES.gap - 12.0,
        theme::size::LABEL,
        if focused { 1 } else { 0 },
        false,
    );
    if let Ok(nc) = CString::new(name) {
        p.text(
            nc.as_ptr(),
            cx,
            ROW_Y + NAME_DY,
            theme::size::LABEL,
            col,
            1,
            if focused { 1 } else { 0 },
        );
    }
}

fn draw_pad(p: Painter, env: &Env, s: &Scene, users: &[auth::UserTile]) {
    let (title_y, dots_y, _) = pad_geom();
    let name = users
        .get(s.pad.target)
        .map(|u| u.title.as_str())
        .unwrap_or("");
    if let Ok(t) = CString::new(format!("Enter {name}'s PIN")) {
        p.text(
            t.as_ptr(),
            SCR_W as f32 * 0.5,
            title_y,
            theme::size::TITLE,
            theme::TEXT_PRIMARY,
            1,
            1,
        );
    }
    // 4 entry dots — replaced by a spinner while the PIN verifies; a rejected PIN pulses the
    // (all-filled) dots DANGER red for PIN_ERR_S, then the entry restarts on the same pad
    if s.pad.submitting {
        Spinner::new(SCR_W as f32 * 0.5, dots_y + PAD_DOT * 0.5, 22.0)
            .phase(s.spin_ms as u32)
            .tint(theme::TEXT_PRIMARY)
            .draw(env, p);
    } else {
        let flash = pin_flash(s.pad.error_s);
        let dgap = 34.0f32;
        let dw = PIN_LEN as f32 * PAD_DOT + (PIN_LEN as f32 - 1.0) * dgap;
        let mut dx = SCR_W as f32 * 0.5 - dw * 0.5;
        for i in 0..PIN_LEN {
            let filled = i < s.pad.entry.len();
            // A BLINK, not a tint: the dark half drops below the resting unfilled dot (0.28), so
            // the row reads as going out and coming back rather than as sitting at a lower red.
            let col = match flash {
                Some(lit) => theme::with_a(theme::DANGER, if lit { 1.0 } else { 0.16 }),
                None => theme::with_a(theme::TEXT_PRIMARY, if filled { 1.0 } else { 0.28 }),
            };
            p.rect(
                Rect::new(dx, dots_y, PAD_DOT, PAD_DOT),
                PAD_DOT * 0.5,
                col,
                col,
                0.0,
            );
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
                    Rect::new(
                        rect.x + (rect.w - d) * 0.5,
                        rect.y + (rect.h - d) * 0.5,
                        d,
                        d,
                    ),
                    ink,
                );
            } else if let Ok(lc) = CString::new((*k as char).to_string()) {
                let ty = crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
                p.text(
                    lc.as_ptr(),
                    rect.x + rect.w * 0.5,
                    ty,
                    theme::size::TITLE,
                    ink,
                    1,
                    1,
                );
            }
        }
    }
    let _ = env;
}

/// Keypad cell geometry — shared by draw_pad and the pointer hit-test.
fn pad_key_rect(r: usize, c: usize) -> Rect {
    let (_, _, grid_y) = pad_geom();
    let gx = SCR_W as f32 * 0.5 - (3.0 * PAD_KEY + 2.0 * PAD_KGAP) * 0.5;
    Rect::new(
        gx + c as f32 * (PAD_KEY + PAD_KGAP),
        grid_y + r as f32 * (PAD_KEY + PAD_KGAP),
        PAD_KEY,
        PAD_KEY,
    )
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
/// pad is up). **Reports whether it parked on anything**, which is what lets [`press_at`] be one
/// line of it: a click in dead space parks nothing and must not arm a press on whatever happened
/// to be focused already.
pub fn pointer_focus(mx: f32, my: f32) -> bool {
    let s = scene();
    if s.pad.open {
        if let Some((r, c)) = pad_key_at(mx, my) {
            s.pad.fr = r;
            s.pad.fc = c;
            return true;
        }
        return false;
    }
    if let Some(i) = tile_at(s, mx, my) {
        s.fc = i as c_int;
        s.footer = false;
        return true;
    }
    if footer_rect().contains(mx, my) {
        s.footer = true;
        return true;
    }
    false
}

/// What a pointer click MEANS while the keypad is up — the pad's twin of [`act`], and pure for the
/// same reason that one is.
///
/// **It takes the HIT, not the coordinates**, and that is the whole of what makes it gradeable: the
/// hit-test is `pad_key_at` → `pad_geom` → `text::text_cap_band`, which measures a font and so pulls
/// GL into the link. The host suite links none, so a test that so much as *mentions* a function
/// reaching it fails at the LINKER and takes the entire test binary down rather than one case.
/// Coordinates in, and this decision would be untestable for a reason that has nothing to do with
/// the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PadClick {
    /// The keypad key at this `(row, col)`.
    Key(c_int, c_int),
    /// Anywhere else — including the bottom-left gap, which is inside the grid and is not a key:
    /// take the pad down, through [`close_pad`] like every other door out.
    Dismiss,
}

fn pad_click(hit: Option<(c_int, c_int)>) -> PadClick {
    match hit {
        Some((r, c)) => PadClick::Key(r, c),
        None => PadClick::Dismiss,
    }
}

/// Spend a [`PadClick`] on the scene — **the EFFECT half, and it is split out for the same reason
/// the classification is**: the bug was never the classification. It was that this dismissal used
/// to be an inline `s.pad = Pad::new()`, which is a correct-looking line that skips the cleanup
/// [`close_pad`] owes. A test that only grades `pad_click(None) == Dismiss` would stay green with
/// that line back, so the arm itself has to be reachable from one — and it is, because everything
/// below is already linked by the KEY path while the font measurement stays up in [`click`].
fn apply_pad_click(s: &mut Scene, action: PadClick) {
    match action {
        PadClick::Key(r, c) => {
            s.pad.fr = r;
            s.pad.fc = c;
            if let Some(k) = KEYS[r as usize][c as usize] {
                press(s, k);
            }
        }
        // the POINTER's door out of the pad, and it owes exactly what BACK owes
        PadClick::Dismiss => close_pad(s),
    }
}

/// Pointer click, **keypad only** since the control faces landed: press the key under the cursor, or
/// dismiss the pad like BACK on a click outside it ([`pad_click`] is that decision). Everything else
/// on this screen is a press surface and belongs to [`press_at`], which has already had its turn by
/// the time this is called — this doc still described the tile and Sign-out actions it no longer
/// performs.
pub fn click(mx: f32, my: f32) {
    let s = scene();
    if s.pad.open {
        apply_pad_click(s, pad_click(pad_key_at(mx, my)));
        return;
    }
    // …and everything OUTSIDE the pad is a press surface, so the click parks focus and stops:
    // `press_at` above has already had its turn, and reaching here means the point hit neither.
}

/// Pointer-down on a roster avatar or on the Sign-out footer: PARK focus on it and report the hit,
/// leaving the caller to arm the tvOS press and commit it through [`activate_focused`] on the
/// spring-back. The two are told apart afterwards by the same [`focus_is_avatar`] / [`focus_is_ctl`]
/// pair the key path asks, so a click and an OK animate identically and there is one activation
/// rather than a click's and a key's.
///
/// The PIN pad is not a press surface — its keys belong to no `CtlPop` and have no dip to show — so
/// an open pad reports `false` and [`click`] handles it on the button-down as it always did.
///
/// **One line of [`pointer_focus`], not a second copy of it.** This began as a verbatim copy of that
/// function's non-pad half, so the rule mapping a hit to `(fc, footer)` was written twice for one
/// picker; the pad guard below is the only thing that was ever actually different.
pub fn press_at(mx: f32, my: f32) -> bool {
    !scene().pad.open && pointer_focus(mx, my)
}

/// Dev/test hook (`plxnative-pickuser`): commit roster tile `idx` exactly like OK — a protected tile
/// opens the PIN pad (headless pad capture), an unprotected one switches.
pub fn pick(idx: usize) {
    let s = scene();
    s.fc = idx as c_int;
    select(s, idx);
}

/// The roster avatar holds focus (not the PIN pad or the Sign-out footer, and a roster exists) — the
/// only profiles focus that takes the tvOS press. OK on the footer / keypad activates immediately.
pub fn focus_is_avatar() -> bool {
    let s = scene();
    !s.pad.open && !s.footer && !auth::users().is_empty()
}

/// The Sign-out footer holds focus — the picker's one CONTROL FACE, and the other half of what
/// takes the tvOS press here ([`focus_is_avatar`] is the first). It has a `CtlPop` of its own
/// (`FOOTER_POP`), so the dip has somewhere to land; the PIN keypad's keys do not, which is why an
/// open pad answers `false` for both.
pub fn focus_is_ctl() -> bool {
    let s = scene();
    !s.pad.open && s.footer
}

/// Commit whatever the picker has focus on — the deferred OK activation (app.rs runs this on the
/// press spring-back, for an avatar and for the footer alike). Mirrors OK in [`key`], which is why
/// it dispatches rather than assuming a roster tile: both stops arm a press now, and a commit that
/// could only select an avatar would have signed nobody out.
pub fn activate_focused() {
    let s = scene();
    if s.footer {
        auth::sign_out();
        return;
    }
    let n = auth::users().len();
    if n > 0 {
        select(s, (s.fc.max(0) as usize).min(n - 1));
    }
}

/// Commit a roster tile (OK or pointer click): protected → PIN pad, else switch.
fn select(s: &mut Scene, idx: usize) {
    if auth::users().get(idx).map(|u| u.protected).unwrap_or(false) {
        s.pad = Pad {
            open: true,
            target: idx,
            ..Pad::new()
        };
    } else {
        auth::select_profile(idx);
    }
}

/// What a key MEANS on the picker — the ladder of [`key`] with its arms' effects lifted out.
///
/// The ORDER of those arms is the whole content of this function, and it is the screen's focus
/// exclusivity stated as a decision: the footer arm precedes the roster arm, so while the Sign out
/// pill holds focus the roster keys mean nothing and OK is the sign-out. [`update`] and [`draw`]
/// say the same thing from the paint side — the row is handed `None` for its focus and no tile is
/// drawn focused while `footer` is set — and a control that is not drawn as focused must not be
/// the one that acts.
///
/// It is pure so that order can be graded on the host, which cannot reach it through [`key`]: the
/// roster arm is gated on `auth::users()`, and every writer of that field sits behind a persisted
/// session or a plex.tv round trip (`auth::start_switch`'s seed, its roster worker, and the
/// sign-in thread's step 4). Both commits below the gate then have effects a unit run must not
/// fire — `SignOut` clears the persisted session and starts the login thread, `Select` reaches
/// `auth::select_profile`'s switch worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    /// ▼ — put focus on the Sign out pill.
    FocusFooter,
    /// ▲ — take it back to the roster.
    FocusRoster,
    /// OK with the pill focused.
    SignOut,
    /// ◀/▶ along the roster, as a direction.
    Step(c_int),
    /// OK with a roster avatar focused.
    Select,
    /// The key means nothing here.
    Ignore,
}

fn act(sym: c_uint, footer: bool, n: c_int) -> Act {
    if sym == SDLK_DOWN {
        Act::FocusFooter // the Sign out pill (reachable even while the roster is empty/loading)
    } else if sym == SDLK_UP {
        Act::FocusRoster
    } else if footer {
        if is_ok(sym) {
            Act::SignOut // the phase→route follower lands on the QR sign-in
        } else {
            Act::Ignore
        }
    } else if n > 0 {
        if sym == SDLK_LEFT {
            Act::Step(-1)
        } else if sym == SDLK_RIGHT {
            Act::Step(1)
        } else if is_ok(sym) {
            Act::Select
        } else {
            Act::Ignore
        }
    } else {
        Act::Ignore
    }
}

/// ◀/▶ along the roster: clamped at both ends, no wrap.
fn step_fc(fc: c_int, dir: c_int, n: c_int) -> c_int {
    if dir < 0 {
        (fc - 1).max(0)
    } else {
        (fc + 1).min(n - 1)
    }
}

pub fn key(sym: c_uint, wcode: c_uint) {
    let s = scene();
    if s.pad.open {
        pad_key(s, sym, wcode);
        return;
    }
    if is_back(sym, wcode) {
        // BACK leaves the picker the way choosing the ALREADY-ACTIVE profile does: `auth::cancel`
        // re-arms the resolved-credentials handoff with the persisted session, the main loop
        // installs it and routes Home. **Whether it may is `auth::may_resume`'s decision, not this
        // screen's**, and it reports false — we swallow the key — in three cases. There is no
        // usable session behind the picker (the roster straight after a sign-out, a dead end you
        // must choose your way out of). The BOOT picker stands over a PIN — a protected profile, or
        // one naming no profile at all, whose token is then the owner's. And, since the *Change
        // profile* picker DETACHES (`auth::detaches_active_profile`), that picker is a ROOT and
        // backs out to nothing whatever the profile behind it was: entering a protected profile,
        // pressing *Change profile* and then BACK was a way straight back inside it with no PIN.
        //
        // Swallowing is enough and no read-out is owed — nothing was attempted and failed, the key
        // simply does not act here — and every one of the three leaves a fully working picker with
        // the Sign out pill under it. What a BACK at a picker ROOT should do instead (leave the app
        // to the television's own Home, as the exit alert does from Home's root) is a separate
        // question and is not decided here.
        auth::cancel();
        return;
    }
    let n = auth::users().len() as c_int;
    match act(sym, s.footer, n) {
        Act::FocusFooter => s.footer = true,
        Act::FocusRoster => s.footer = false,
        Act::SignOut => auth::sign_out(),
        Act::Step(d) => s.fc = step_fc(s.fc, d, n),
        Act::Select => select(s, s.fc as usize),
        Act::Ignore => {}
    }
}

/// Remote number key → keypad digit: SDL gives printable keys their ASCII sym and the webOS
/// remote's number buttons carry the same 48–57 ('0'–'9') range in `wcode`.
fn digit_of(sym: c_uint, wcode: c_uint) -> Option<u8> {
    [sym, wcode]
        .into_iter()
        .find(|v| (48..=57).contains(v))
        .map(|v| v as u8)
}

fn pad_key(s: &mut Scene, sym: c_uint, wcode: c_uint) {
    if is_back(sym, wcode) {
        close_pad(s);
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
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, -1);
    } else if sym == SDLK_RIGHT {
        s.pad.fc = step_focus(s.pad.fr, s.pad.fc, 1);
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
fn step_focus(fr: c_int, fc: c_int, dir: c_int) -> c_int {
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
    s.pad.error_s = 0.0; // typing again cancels the wrong-PIN flash
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

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The picker's key ladder, in two halves — and the split is forced by what a host can reach.
    //!
    //! [`act`], [`step_fc`] and the keypad walkers are pure, so they are graded directly and need
    //! no lock. The live half drives the real `static mut SCENE` through [`key`], and presses only
    //! keys whose arms stay inside this module — ▼/▲, and (with the pill focused) the ◀/▶ the
    //! ladder ignores. **No test presses OK against the singleton**: on the pill that arm is
    //! `auth::sign_out` (the persisted session cleared, `plex::revoke_all`, a login thread
    //! started) and on an avatar it is `auth::select_profile`'s switch worker. Those two are what
    //! [`act`] exists to make assertable without firing them.
    //!
    //! The live tests take this module's own [`SCENELOCK`] for their whole body (`home.rs`'s
    //! `FOCUS` precedent — `SCENE` is screen-level `static mut`) and `crate::testlock::serial()`
    //! as well, because the ladder reads `auth::users()`, which is a crate-wide global.
    use super::*;
    use std::sync::Mutex;

    static SCENELOCK: Mutex<()> = Mutex::new(());

    /// A freshly-entered picker: the scene allocated as [`init`] leaves it, focus as [`enter`]
    /// leaves it. `init` replaces the whole `Scene`, so this is also the cleanup.
    fn boot() {
        init();
        enter();
    }

    /// Focus opens on the roster, and ▼/▲ are the path to the Sign out pill and back. Driven
    /// through the real [`key`] against the real scene, because where the screen *starts* is a
    /// property of [`enter`] rather than of the ladder.
    #[test]
    fn focus_opens_on_the_roster_and_the_pill_is_one_step_down() {
        let _s = crate::testlock::serial();
        let _g = SCENELOCK.lock().unwrap_or_else(|e| e.into_inner());
        boot();
        assert!(
            !scene().footer,
            "a picker opens with the roster focused, not the footer"
        );

        key(SDLK_DOWN, 0);
        assert!(scene().footer, "▼ off the roster is the Sign out pill");
        key(SDLK_DOWN, 0);
        assert!(
            scene().footer,
            "the pill is the last focus stop — ▼ again holds it"
        );

        key(SDLK_UP, 0);
        assert!(!scene().footer, "▲ brings focus back to the roster");
        key(SDLK_UP, 0);
        assert!(!scene().footer, "and holds there — the roster is the first");
    }

    /// The ▼ arm's own claim — the pill is *"reachable even while the roster is empty/loading"* —
    /// against an empty roster, which is the state this host is permanently in: every writer of
    /// `auth::users()` sits behind a persisted session or a plex.tv round trip, so nothing a unit
    /// run can do puts a tile on this screen.
    #[test]
    fn the_sign_out_pill_takes_focus_with_no_roster_on_screen() {
        let _s = crate::testlock::serial();
        let _g = SCENELOCK.lock().unwrap_or_else(|e| e.into_inner());
        boot();
        assert!(
            auth::users().is_empty(),
            "an unseeded roster is the case under test"
        );

        key(SDLK_DOWN, 0);
        assert!(
            scene().footer,
            "the pill is focusable with nothing above it to leave"
        );
        assert!(
            !focus_is_avatar(),
            "and there is no avatar for the deferred OK activation to commit"
        );

        key(SDLK_RIGHT, 0);
        assert_eq!(scene().fc, 0, "there is no tile to walk to");
        assert!(scene().footer, "and ◀/▶ are not a way off the pill");
    }

    /// **The footer arm precedes the roster arm.** That order is what makes focus exclusive on the
    /// input side: with the Sign out pill focused, ◀/▶ must not walk a cursor that is not drawn,
    /// and OK must be the sign-out rather than a profile switch. Swapped, the second block below
    /// answers `Step(-1)`/`Step(1)` and the OK loop answers `Select` on both sides — the pill
    /// driving the hidden roster while it is still the control drawn as focused.
    #[test]
    fn the_pill_answers_every_key_while_it_holds_focus() {
        const N: c_int = 3; // a roster with tiles in it — what makes the order observable at all

        // roster focused: ◀/▶ walk it, OK commits the tile under the ring
        assert_eq!(act(SDLK_LEFT, false, N), Act::Step(-1));
        assert_eq!(act(SDLK_RIGHT, false, N), Act::Step(1));

        // pill focused: the same two keys, and the roster is not what answers them
        assert_eq!(act(SDLK_LEFT, true, N), Act::Ignore);
        assert_eq!(act(SDLK_RIGHT, true, N), Act::Ignore);

        // …and OK, in every code `is_ok` accepts, means one thing on each side
        for ok in [SDLK_RETURN, SDLK_KP_ENTER, SDLK_SELECT] {
            assert_eq!(act(ok, true, N), Act::SignOut, "OK on the pill signs out");
            assert_eq!(
                act(ok, false, N),
                Act::Select,
                "OK on the roster commits the focused tile"
            );
        }

        // ▼/▲ are the same statement whichever control holds focus, and with or without a roster
        for footer in [false, true] {
            for n in [0, N] {
                assert_eq!(act(SDLK_DOWN, footer, n), Act::FocusFooter);
                assert_eq!(act(SDLK_UP, footer, n), Act::FocusRoster);
            }
        }

        // with no roster the bottom arm has nothing to offer — but the pill above it still acts
        assert_eq!(act(SDLK_LEFT, false, 0), Act::Ignore);
        assert_eq!(
            act(SDLK_RETURN, false, 0),
            Act::Ignore,
            "OK on an empty roster commits nothing"
        );
        assert_eq!(
            act(SDLK_RETURN, true, 0),
            Act::SignOut,
            "the pill signs out while the roster loads"
        );
    }

    /// ◀/▶ clamp at both ends of the roster.
    #[test]
    fn the_roster_cursor_clamps_at_both_ends() {
        assert_eq!(step_fc(1, -1, 3), 0);
        assert_eq!(step_fc(0, -1, 3), 0, "◀ on the first tile holds it");
        assert_eq!(step_fc(1, 1, 3), 2);
        assert_eq!(step_fc(2, 1, 3), 2, "▶ on the last tile holds it");
        assert_eq!(
            step_fc(0, 1, 1),
            0,
            "a one-profile roster has nowhere to walk"
        );
    }

    /// With the keypad up, [`key`] never reaches the picker's own ladder: the pad takes the key
    /// first, so ▼ steps the keypad rows and the footer flag is untouched. The other half of
    /// [`update`]'s "focus is exclusive, never on two controls at once".
    #[test]
    fn an_open_keypad_takes_the_key_before_the_picker_does() {
        let _s = crate::testlock::serial();
        let _g = SCENELOCK.lock().unwrap_or_else(|e| e.into_inner());
        boot();
        scene().pad = Pad {
            open: true,
            target: 0,
            ..Pad::new()
        };

        key(SDLK_DOWN, 0);
        assert_eq!(scene().pad.fr, 1, "▼ stepped the keypad row");
        assert!(
            !scene().footer,
            "and did not focus the Sign out pill behind the scrim"
        );

        boot(); // the pad is scene state — leave the singleton where `enter` leaves it
    }

    /// The keypad's bottom row has a blank where a phone dial pad has nothing, and neither walk
    /// may land focus on it: ◀ from `0` has no key to its left, and ▼ off `7` lands on `0` rather
    /// than on the gap directly under it.
    #[test]
    fn the_keypad_walks_around_its_empty_cell() {
        assert_eq!(
            KEYS[3][0], None,
            "the cell both walkers below have to step over"
        );
        assert_eq!(
            step_focus(3, 1, -1),
            1,
            "◀ from 0 finds nothing to its left and holds"
        );
        assert_eq!(step_focus(3, 1, 1), 2, "▶ from 0 reaches delete");
        assert_eq!(step_focus(0, 0, -1), 0, "a row edge clamps");
        assert_eq!(step_focus(0, 2, 1), 2);
        assert_eq!(nearest_col(3, 0), 1, "▼ off 7 lands on 0");
        assert_eq!(
            nearest_col(3, 2),
            2,
            "▼ off 9 lands on delete, which is under it"
        );
        assert_eq!(nearest_col(1, 1), 1, "an occupied column is kept as it is");
    }

    /// **Every door out of the keypad clears the PIN verdict, and there are THREE.**
    ///
    /// Two are obvious from the key handler — BACK, and the non-PIN failure `update` closes the pad
    /// for. The third is a pointer click OUTSIDE the keypad, and it is exactly the one the first
    /// version of `close_pad` missed: the pad is not a press surface, so `press_at` declines it and
    /// the dismissal happens on the button-down inside [`click`], a different function that still
    /// held its own bare `Pad::new()`. So this drives the two the host can reach through the REAL
    /// entry points rather than calling `close_pad` directly, which would grade nothing.
    ///
    /// The split is the module's usual one and is forced the usual way: BACK goes through the real
    /// [`key`] against the real scene, and the pointer door through [`pad_click`] +
    /// [`apply_pad_click`]. [`click`] itself cannot be NAMED here at all — its hit-test reaches
    /// `pad_geom` → `text::text_cap_band`, which measures a font, and the host suite links no GL, so
    /// mentioning it fails at the LINKER and takes the whole test binary down rather than one case.
    /// That is also how the pointer hole was found: the first version of this test called `click`
    /// and `cargo test --lib` stopped building.
    ///
    /// **The effect is graded, not just the classification.** `pad_click(None) == Dismiss` says
    /// nothing about the bug — the inline `s.pad = Pad::new()` classified the click correctly too.
    /// So the verdict is seeded LIVE and the arm is spent on the real scene; with that line back,
    /// the pad closes and `auth::pin_denied` stays true, which is exactly the shipped defect.
    #[test]
    fn every_door_out_of_the_keypad_goes_through_one_close() {
        let _s = crate::testlock::serial();
        let _g = SCENELOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mid_flash = || Pad {
            open: true,
            error_s: PIN_ERR_S,
            ..Pad::new()
        };

        // Door 1 — BACK, through the real key ladder.
        boot();
        scene().pad = mid_flash();
        auth::set_pin_denied_for_test(true);
        key(SDLK_ESCAPE, 0);
        assert!(!scene().pad.open, "BACK takes the keypad down");
        assert_eq!(scene().pad.error_s, 0.0, "…and the flash goes with it");
        assert!(
            !auth::pin_denied(),
            "…and the verdict, which is what leaks onto the roster behind it"
        );

        // Door 2 — the pointer. A hit on a KEY is that keypress; a miss (the surround, and equally
        // the bottom-left gap `pad_key_at` refuses) is the dismissal.
        assert_eq!(pad_click(Some((1, 1))), PadClick::Key(1, 1));
        assert_eq!(
            pad_click(None),
            PadClick::Dismiss,
            "a click that hit no key is the pointer's way out of the pad"
        );
        boot();
        scene().pad = mid_flash();
        auth::set_pin_denied_for_test(true);
        apply_pad_click(scene(), PadClick::Dismiss);
        assert!(!scene().pad.open, "the pointer takes the keypad down too");
        assert_eq!(scene().pad.error_s, 0.0);
        assert!(
            !auth::pin_denied(),
            "the pointer's door owes the SAME cleanup BACK's does — it used to blank the pad by \
             its own hand and skip it"
        );

        auth::set_pin_denied_for_test(false);
        boot(); // the pad is scene state — leave the singleton where `enter` leaves it
    }

    /// **A rejected PIN must BLINK, and a blink is a clock.**
    ///
    /// `ui::idle` gates the whole present, and it detects motion by watching the two spring
    /// integrators — so a countdown is invisible to it (`ui/CLAUDE.md`'s standing hazard, the one
    /// `Xfade::tick` and `Spinner::draw` both shipped without). The flash predates the gate by
    /// three weeks and never reported: after the last spinner frame's `invalidate` was spent the
    /// picker settled, the dot row froze on whichever half-cycle that frame caught, and what the
    /// user saw was the reported symptom — dots that "only become slightly red" and stay there.
    ///
    /// So this grades the two halves `ui/CLAUDE.md` demands of a clock-driven animation, over the
    /// whole flash window: a frame is asked for on **every** phase flip (including the one that
    /// ends the flash and returns the dots to their resting ink), and on **no** frame in between —
    /// a blink that reported every frame would hold the picker awake for 1.4 s to move eight quads.
    #[test]
    fn the_wrong_pin_flash_blinks_and_asks_for_a_frame_on_every_flip() {
        let _s = crate::testlock::serial();
        let _g = SCENELOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dt = 1.0 / 60.0;
        let mut pad = Pad {
            open: true,
            error_s: PIN_ERR_S,
            ..Pad::new()
        };

        // Graded on THIS thread's damage reports (`idle::take_local_damage`), never on
        // `should_present`: its sticky flag is process-wide, so any unlocked test on another
        // thread that repaints anything made a quiet frame here read as a repaint request, and
        // this test failed at random under load. Drain what the setup above raised.
        crate::ui::idle::frame_begin(dt);
        crate::ui::idle::take_local_damage();

        let mut phases = vec![pin_flash(pad.error_s)];
        assert_eq!(
            phases[0],
            Some(true),
            "the flash opens LIT — the first thing a rejected PIN does is go red"
        );
        let mut frames = 0;
        while pad.error_s > 0.0 && frames < 600 {
            frames += 1;
            let was = pin_flash(pad.error_s);
            crate::ui::idle::frame_begin(dt);
            step_pin_flash(&mut pad, dt);
            let now = pin_flash(pad.error_s);
            let asked = crate::ui::idle::take_local_damage() > 0;
            if now != was {
                phases.push(now);
                assert!(
                    asked,
                    "frame {frames}: the dot row changed colour and did not ask to be drawn — \
                     the present gate freezes it on one static tint"
                );
            } else {
                assert!(
                    !asked,
                    "frame {frames}: a dot row mid-phase asked for a repaint"
                );
            }
        }

        assert_eq!(pad.error_s, 0.0, "the flash runs out rather than sticking");
        assert_eq!(
            phases.last(),
            Some(&None),
            "…and the last report is the one that returns the dots to their resting ink"
        );
        // The lit halves, counted: this is what "flashed/blinked red" means as a number, and it is
        // the assertion a static tint fails no matter how red it is.
        let lit = phases.iter().filter(|p| **p == Some(true)).count();
        assert!(
            lit >= 4,
            "a 1.4s error window must read as several distinct red pulses, not one — got {lit} \
             ({phases:?})"
        );
    }

    /// A PIN digit is read from whichever field carries it — SDL gives a printable key its ASCII
    /// sym, and the remote's number buttons carry the same 48–57 range in `wcode`.
    #[test]
    fn a_pin_digit_is_read_from_either_field() {
        assert_eq!(
            digit_of('7' as c_uint, 0),
            Some(b'7'),
            "a dev keyboard's sym"
        );
        assert_eq!(
            digit_of(0, 55),
            Some(b'7'),
            "the remote's wcode, same digit"
        );
        assert_eq!(digit_of('0' as c_uint, 0), Some(b'0'));
        assert_eq!(digit_of(SDLK_LEFT, 0), None, "a D-pad key is not a digit");
        assert_eq!(
            digit_of(SDLK_RETURN, 0),
            None,
            "nor is OK — 13 is below the digit range"
        );
    }
}
