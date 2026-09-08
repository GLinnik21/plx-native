//! **The who's-watching picker, as an owned `Screen`** (restructure spec §13, phase 6 —
//! `ui/profiles.rs` moved). The avatar row (`crate::ui::card_row`, a circular `RowStyle::PROFILES`
//! shelf — the same shelf motion the poster rows use) plus the "Sign out" footer, and the PIN
//! keypad for a protected profile.
//!
//! **One struct, no globals.** The legacy screen kept its whole scene in two `static mut`s
//! (`SCENE`, `FOOTER_POP`) and its own hand-rolled focus ladder (`Act`, `act`, `step_fc`,
//! `step_focus`, `nearest_col`); this file has neither. Focus is the ENGINE's — this screen
//! answers the §7.1 query protocol and reacts to `ScreenEvent::FocusMoved`, it never stores
//! "which avatar is selected" as its own truth. What it DOES keep as ordinary fields is: the
//! `CardRow` animation cache the avatar row's springs live in (a render cache, not logical state,
//! exactly like `RootPage::table`), the footer's `CtlPop` dip, and the PIN pad's own small state
//! machine (open/target/entry/submitting/error), which is genuinely this screen's own — nothing
//! else on the tree knows a PIN pad exists.
//!
//! **The PIN keypad is a `Grid` with a HOLE**, spec §7.1 and open question 2: 4 rows × 3 columns,
//! bottom-left empty (a phone dial pad's own layout, delete in the bottom-right where every phone
//! puts it). `ui/geom.rs`'s reusable `Grid` view is built for a DIFFERENT shape — a column of
//! shelves, one `Row` group per shelf — so it does not fit a single grid-with-a-hole group; the
//! walk below (`pad_neighbour`) is written by hand instead.
//!
//! **`GroupKind::Grid { holes, .. }` is metadata ONLY here — the production engine never reads
//! it.** The one place `GroupKind` is matched at all is `ui/focus.rs`'s `#[cfg(test)] mod tree`,
//! a fixture adapter for that module's own golden tables (pinned there by
//! `a_grid_hole_is_skipped_along_the_direction_of_travel`, which grades the FIXTURE tree, not
//! this screen); a real group's traversal always comes from its own `Focusable::neighbour`, which
//! is `pad_neighbour` for this one. An earlier version of this comment claimed `pad_neighbour`
//! deliberately reproduced that fixture's "skip along the direction of travel" rule as a
//! considered improvement over the legacy screen's sideways deflection — that claim was never
//! exercised by anything a user could reach, and for THIS grid's shape (the hole sits in the
//! grid's own LAST row) it is actively wrong: skipping straight down from '7' runs off the bottom
//! edge with nothing to land on, so the key press does nothing at all. `pad_neighbour` deflects
//! sideways instead, exactly as the legacy `nearest_col` did — see its own doc for the exact rule.
//! **The `holes` field is left in `pad_group_spec` as descriptive metadata, but it is a SECOND
//! source of truth for the same fact `KEYS` already carries** (`draw_pad`/`pad_place`/
//! `pad_group_of` all derive "is this cell a hole" from `KEYS[r][c].is_none()` directly, never
//! from `PAD_HOLES`). `declared_pad_holes_match_the_keys_and_edges` now checks that the declaration
//! and rendered cells agree, so a shape change cannot silently leave stale metadata behind.
//! A shared grid-with-a-hole helper in `ui/focus.rs`, built to WALK from the same
//! `holes` list a `GroupSpec` declares (rather than reading `KEYS` by hand here), would close that
//! gap for this screen and for the next grid-with-a-hole the app builds; none exists today.
//!
//! **The digits are handled in `step`; no raw `key(sym)` function survives** (spec open question
//! 2). A number key types straight into the PIN without moving the visual cursor (the remote's
//! digit buttons carry the same 48-57 ASCII range in `sym`/`wcode` the legacy `digit_of` read, and
//! that pure function is unchanged); OK on a keypad cell presses whatever cell the cursor is on,
//! delivered as an ordinary `ScreenEvent::Activate` — the keys are `ElemKind::Bare`, so the engine
//! fires that on the key-DOWN edge with no press arm at all, exactly as a `Card`/`Control` MUST
//! NOT (see the next paragraph).
//!
//! **The avatar row and the footer arm presses through DIFFERENT doors and commit through one.**
//! An avatar is `ElemKind::Card` (`ui/geom.rs`'s `Shelf`, unmodified — this is the first owned
//! screen to use it for a real `Card` group), the "Sign out" footer is `ElemKind::Control`, and
//! the PIN keys are `ElemKind::Bare`. Card and Control both arm a press through `Fx::Press` (a
//! holdable one for the avatar, a non-holdable one for the footer — `ui/screen.rs`'s doc on
//! `ElemKind`) and both commit through the SAME `ScreenEvent::PressCommit`, which is where this
//! screen reads `cx.focus.current` to learn which of the two just committed (the press machine
//! never says WHAT it pressed, only that a press attached to the current owner completed — the
//! same reason `screens::consent`'s own `PressCommit` arm reads `cx.focus.current`). Getting Bare
//! vs Card/Control right is the difference between a tile that dips on OK and one that does not:
//! a Bare element skips the whole press machine, so a keypad key drawn with a spring dip would be
//! animating a state the engine never actually enters.
//!
//! **The pad is modal over the picker.** While it is open, `groups()` answers with the pad's ONE
//! group and nothing else — no avatar row, no footer — which is what traps focus inside it: the
//! engine's geometric search has no other group to find, in any direction, so every key the pad
//! itself declines (an edge run into a wall) resolves to `Outcome::Nothing` rather than escaping
//! onto the roster underneath. **Closing it must not leave a verdict from the last attempt
//! following a fresh picker onto the screen** (the legacy `close_pad`'s own standing warning,
//! carried forward verbatim): every door out of the pad — BACK, a pointer click outside every
//! keypad cell, and a completed switch dropping back here on a NON-pin failure — runs through the
//! one [`ProfilesScreen::close_pad`], which is the only place `auth::dismiss_pin_error` is called
//! from. A rejected PIN is the one exception that keeps the pad up (the dot row flashes red and
//! the entry restarts) rather than closing it, because closing on every typo would send the user
//! back to re-pick the profile for each wrong digit.
//!
//! **A pointer click "outside the pad" has no generic answer here and has to be built by hand.**
//! Unlike a `Popover` on the `ModalStack` (whose `on_miss(style)` policy the container itself
//! consults), the PIN pad is not a container surface — it is a screen's own internal state — so
//! there is no `OnMiss::Dismiss` for it to opt into. `dispatch.rs` does deliver the raw
//! `ScreenEvent::Input(InputEvent{kind: InputKind::Click{hit,..},..})` to every owning screen
//! REGARDLESS of whether the click resolved onto a stop (confirmed by reading `frame_with`'s
//! ingest loop: the `Delivery::Screen(ScreenEvent::Input(ev))` push is unconditional on `ev.kind`,
//! after the pointer-specific hit resolution has already written `hit`), so `hit.is_none()` while
//! the pad is open is exactly "the click landed on none of the twelve cells" — the legacy
//! `pad_click`'s `PadClick::Dismiss` arm, reproduced from the frozen primitives rather than a
//! library addition.
#![allow(dead_code)] // struct fields/helpers read from `draw`, which the host suite never calls

use std::borrow::Cow;
use std::ffi::CString;

use crate::auth::{self, Phase};
use crate::ui::card_row;
use crate::ui::frame::Budget;
use crate::ui::geom;
use crate::ui::icons;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled, InputEvent, InputKind, Key,
    LogicalState, Machine, Measure,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::route_screen::RouteGround;
use crate::ui::screen::{
    At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, FocusTarget, Focusable,
    GroupKind, GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat,
    Step, Stop, Activate,
};
use crate::ui::widgets::{self, Art, Button, CtlPop, Spinner};
use crate::ui::{consts::SCR_H, consts::SCR_W, theme, Env, Painter, Rect, View};

use super::registry::{word, AppFx, AppLike, LoopReq};

/// The screen's own heading.
// `pub(crate)` for ONE external reader: `screens/onboard.rs` builds the breadcrumb a user
// sees when the Favourites editor is reached from the profile picker, and that crumb has to
// be this screen's own title or the two drift apart silently. It used to read the same
// constant out of the LEGACY `ui/profiles.rs`, which is what kept a fully dead 1,250-line
// module alive in the tree through phase 6 — one `const` holding a whole file hostage.
pub(crate) const TITLE: &str = "Who's watching?";

const ROW_Y: f32 = 384.0;
/// Name band offset below `ROW_Y` — derived from the SAME numbers the shelf pops by, so raising
/// the pop can't silently collide the name with the popped circle. Ported verbatim from
/// `ui/profiles.rs`'s `NAME_DY`.
const NAME_DY: f32 = card_row::RowStyle::PROFILES.h
    + card_row::RowStyle::PROFILES.h * (card_row::RowStyle::PROFILES.focus_scale - 1.0) * 0.5
    + theme::space::MD;
const PIN_LEN: usize = 4;
const FOOTER_Y: f32 = 780.0;
const FOOTER_H: f32 = 60.0;
/// The switch-failure message's vertical centre, below the pill with a full `space::XL` of air —
/// see `ui/profiles.rs`'s own draw site for why it lives here rather than under the name band.
const ERROR_Y: f32 = FOOTER_Y + FOOTER_H + theme::space::XL + theme::size::BODY as f32 * 0.5;

// PIN pad geometry: the label, dots and keypad are ONE centred unit (`pad_geom`'s doc has the
// reasoning, including why it is now `Measure`-driven rather than `crate::text::text_cap_band`-driven).
const PAD_KEY: f32 = 108.0;
const PAD_KGAP: f32 = 20.0;
const PAD_ROWS: usize = 4;
const PAD_COLS: usize = 3;
const PAD_GRID_H: f32 = PAD_ROWS as f32 * PAD_KEY + (PAD_ROWS - 1) as f32 * PAD_KGAP;
const PAD_GRID_W: f32 = PAD_COLS as f32 * PAD_KEY + (PAD_COLS - 1) as f32 * PAD_KGAP;
const PAD_TITLE_GRID: f32 = 152.0; // title draw-y → keypad top (the unit's overall pacing)
const PAD_DOT: f32 = 18.0; // entry-dot diameter
const PIN_ERR_S: f32 = 1.4; // wrong-PIN red-flash duration (s)
const PIN_ERR_HALF_S: f32 = 0.175; // …and one half-cycle of it, so the window is four full blinks

/// Keypad layout: 4 rows × 3 cols. `b'D'` = delete (bottom-right, where every phone dial pad puts
/// it); `None` = the one empty, unfocusable cell. Ported verbatim from `ui/profiles.rs::KEYS`.
const KEYS: [[Option<u8>; PAD_COLS]; PAD_ROWS] = [
    [Some(b'1'), Some(b'2'), Some(b'3')],
    [Some(b'4'), Some(b'5'), Some(b'6')],
    [Some(b'7'), Some(b'8'), Some(b'9')],
    [None, Some(b'0'), Some(b'D')],
];
/// `(row, col)` holes in the keypad grid — one, the bottom-left cell.
const PAD_HOLES: &[(usize, usize)] = &[(3, 0)];

/// The screen's `u32` element namespace: a roster tile is its own raw index (`geom::Shelf`'s
/// convention), so the footer and the keypad each need a base far above any realistic roster
/// length — the same carving the Settings family does for its own band/alert space
/// (`registry::BAND`/`ALERT`), just local to this screen since nothing outside it ever needs to
/// name one of these keys.
const FOOTER: u32 = 0x1000_0000;
/// Keypad cell `(r, c)` is `PAD_BASE + r * PAD_COLS + c`.
const PAD_BASE: u32 = 0x2000_0000;

const ROSTER_GROUP: GroupId = GroupId(1);

/// What an OK on this screen MEANS — decided without doing it.
///
/// Every outcome here is irreversible on the far side (a sign-out drops the session, a select
/// starts a profile switch), so the table is separated from its effect to keep it gradeable in a
/// host test. See the `PressCommit` arm for the history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Commit {
    SignOut,
    Select(usize),
    Nothing,
}

/// `pad_open` first, and deliberately: the keypad's own keys are `ElemKind::Bare` and never arm a
/// press, so a `PressCommit` arriving while the pad is up belongs to the picker UNDERNEATH it —
/// firing it would switch profile out from under an open PIN prompt.
///
/// An index past `roster_n` commits nothing rather than clamping: the roster can shrink under the
/// screen when a share is revoked, and switching to "whatever is at the end now" is a worse answer
/// than doing nothing.
fn commit_action(pad_open: bool, elem: Option<u32>, roster_n: usize) -> Commit {
    if pad_open {
        return Commit::Nothing;
    }
    match elem {
        None => Commit::Nothing,
        Some(FOOTER) => Commit::SignOut,
        Some(e) if (e as usize) < roster_n => Commit::Select(e as usize),
        Some(_) => Commit::Nothing,
    }
}
const FOOTER_GROUP: GroupId = GroupId(2);
const PAD_GROUP: GroupId = GroupId(3);

fn pad_rc(elem: u32) -> Option<(usize, usize)> {
    let i = elem.checked_sub(PAD_BASE)? as usize;
    (i < PAD_ROWS * PAD_COLS).then_some((i / PAD_COLS, i % PAD_COLS))
}
fn pad_elem(r: usize, c: usize) -> u32 {
    PAD_BASE + (r * PAD_COLS + c) as u32
}

/// The PIN keypad overlay state (open for a protected profile) — a field of the screen, not a
/// `static mut`. `Pad::new()` is what every door out of the pad replaces the whole struct with
/// (`ProfilesScreen::close_pad`'s doc), so there is exactly one place a stale verdict could
/// survive a close, and it is guarded there.
struct Pad {
    open: bool,
    /// The roster index this PIN unlocks.
    target: usize,
    entry: String,
    /// A full PIN is being verified (the legacy switch worker is in flight) — the pad stays up,
    /// showing a spinner in place of the dot row, and swallows every key but BACK.
    submitting: bool,
    /// Wrong-PIN flash: seconds still to run. Zero means no flash. While it runs the dot row
    /// blinks DANGER and the entry has already been restarted (`ProfilesScreen::tick`).
    error_s: f32,
}
impl Pad {
    const fn new() -> Self {
        Pad {
            open: false,
            target: 0,
            entry: String::new(),
            submitting: false,
            error_s: 0.0,
        }
    }
    fn opened(target: usize) -> Self {
        Pad {
            open: true,
            target,
            ..Self::new()
        }
    }
}

/// Which half of the wrong-PIN flash cycle the pad is in — `None` once the flash is over, `Some(true)`
/// on a lit half. Pure, ported verbatim from `ui/profiles.rs`: phased off ELAPSED time (not the
/// remainder) so a rejected PIN is red on the very frame it is rejected, at any duration.
fn pin_flash(error_s: f32) -> Option<bool> {
    (error_s > 0.0).then(|| (((PIN_ERR_S - error_s) / PIN_ERR_HALF_S) as i32) % 2 == 0)
}

/// Remote number key → keypad digit, ported verbatim: SDL gives a printable key its ASCII sym and
/// the webOS remote's number buttons carry the same 48-57 ('0'-'9') range in `wcode`.
fn digit_of(sym: u32, wcode: u32) -> Option<u8> {
    [sym, wcode]
        .into_iter()
        .find(|v| (48..=57).contains(v))
        .map(|v| v as u8)
}

/// Avatar-row geometry: (first tile's left x before scroll, per-tile stride). Centred when the
/// roster fits, else left-aligned so `CardRow` can scroll it. No `Measure` needed — every input is
/// a fixed style constant or the roster count. Ported verbatim from `ui/profiles.rs::row_geom`.
fn row_geom(n: usize) -> (f32, f32) {
    let sty = card_row::RowStyle::PROFILES;
    let slot = sty.w + sty.gap;
    let total = (n as f32 * slot - sty.gap).max(0.0);
    (((SCR_W as f32 - total) * 0.5).max(sty.margin_x), slot)
}

/// The centred "Sign out" pill under the roster — shared by `draw` and the footer's `Focusable`
/// group/placement, so a click can never land somewhere the ring is not drawn (`ui/table_screen.rs`'s
/// rule). `measure.width` with `bold: true` is EXACTLY `ui/profiles.rs`'s own
/// `text::text_width(..., 1)` — `Measure::width` takes a real bold flag, unlike `cap_h`/`line_h`
/// below, so this one needs no approximation at all.
fn footer_rect(measure: &dyn Measure) -> Rect {
    let tw = measure.width(c"Sign out", theme::size::BODY, true);
    let w = tw + 76.0;
    Rect::new((SCR_W as f32 - w) * 0.5, FOOTER_Y, w, FOOTER_H)
}

/// The PIN pad's one centred unit — title, dot row and keypad share ONE vertical placement, so a
/// change to any of the three pieces cannot silently leave a gap or a collision beside it. Ported
/// from `ui/profiles.rs`'s `pad_geom`, whose header explains why the block is centred as a unit
/// (it used to sit low on the panel — three independently hard-coded Ys).
///
/// **Reworked onto `Measure` rather than `crate::text::text_cap_band`, and that is a real
/// behaviour change, not a mechanical port.** A query this screen's `Focusable` impl answers (the
/// keypad's cell rects, read by the engine on every direction key) must be reachable from a host
/// test with no font loaded — `crate::text::text_cap_band` rasterizes a reference glyph and pulls
/// SDL2_ttf into the link the moment anything reachable from a test calls it, which is exactly how
/// `ui/profiles.rs`'s own pointer-dismissal hole was found ("the first version of this test called
/// `click` and `cargo test --lib` stopped building" — that module's `PadClick` doc). `line_h` is
/// the same BOLD-BLIND approximation `screens::login::status_action_rect`'s own `cap_h` local
/// already accepts for a control's vertical centring: the `Measure` trait has no bold-aware
/// cap-band accessor, and the slack this costs is a few px on a purely cosmetic vertical
/// centring — nothing downstream depends on the pixel being exact, only on `draw` and every
/// `Focusable` query reading the SAME number, which this function is the one place either computes.
fn pad_geom(measure: &dyn Measure) -> (f32, f32, f32) {
    let title_h = measure.line_h(theme::size::TITLE);
    let unit_h = title_h + PAD_TITLE_GRID + PAD_GRID_H;
    let unit_top = (SCR_H as f32 - unit_h) * 0.5;
    let grid_y = unit_top + title_h + PAD_TITLE_GRID;
    // the dot row centred in the air between the title's own bottom and the keypad's top
    let dots_y = (unit_top + title_h + grid_y) * 0.5 - PAD_DOT * 0.5;
    (unit_top, dots_y, grid_y)
}

/// Keypad cell geometry — shared by `draw` and every `Focusable` query on the pad group.
fn pad_key_rect(measure: &dyn Measure, r: usize, c: usize) -> Rect {
    let (_, _, grid_y) = pad_geom(measure);
    let gx = SCR_W as f32 * 0.5 - PAD_GRID_W * 0.5;
    Rect::new(
        gx + c as f32 * (PAD_KEY + PAD_KGAP),
        grid_y + r as f32 * (PAD_KEY + PAD_KGAP),
        PAD_KEY,
        PAD_KEY,
    )
}

fn pad_extent(measure: &dyn Measure) -> Rect {
    let (_, _, grid_y) = pad_geom(measure);
    Rect::new(SCR_W as f32 * 0.5 - PAD_GRID_W * 0.5, grid_y, PAD_GRID_W, PAD_GRID_H)
}

/// LEFT/RIGHT skip a hole IN THE SAME ROW (there is only ever one row to search, so "keep
/// travelling until an occupied cell or the edge" is unambiguous); UP/DOWN move exactly one row
/// and, landing on the hole, DEFLECT SIDEWAYS to the nearest occupied column in that new row
/// (`pad_nearest_col`) — ported verbatim from `ui/profiles.rs`'s retired `nearest_col`/
/// `step_focus`, not the engine's own fixture-only `GroupKind::Grid` walk (see the module doc for
/// why those two are not the same thing, and why matching the fixture's rule here was wrong for
/// THIS grid: the hole sits in the grid's own last row, so "skip along the direction of travel"
/// from directly above it runs off the bottom edge with nothing to land on — DOWN from '7' became
/// a dead press, exactly the regression this function exists to not have).
fn pad_neighbour(entry: EntryId, elem: u32, dir: Dir) -> Step<u32> {
    use crate::ui::machine::FocusKey;
    let Some((r, c)) = pad_rc(elem) else {
        return Step::Edge;
    };
    match dir {
        Dir::Left | Dir::Right => {
            let step: isize = if dir == Dir::Left { -1 } else { 1 };
            let mut cc = c as isize + step;
            while (0..PAD_COLS as isize).contains(&cc) {
                if KEYS[r][cc as usize].is_some() {
                    return Step::Move(FocusKey { entry, elem: pad_elem(r, cc as usize) });
                }
                cc += step;
            }
            Step::Edge
        }
        Dir::Up | Dir::Down => {
            let step: isize = if dir == Dir::Up { -1 } else { 1 };
            let rr = r as isize + step;
            if !(0..PAD_ROWS as isize).contains(&rr) {
                return Step::Edge;
            }
            let rr = rr as usize;
            Step::Move(FocusKey { entry, elem: pad_elem(rr, pad_nearest_col(rr, c)) })
        }
    }
}

/// Nearest OCCUPIED column to `c` in row `r` — the keypad's own hole deflection, ported verbatim
/// from `ui/profiles.rs`'s retired `nearest_col`: `c` itself if it is occupied, else the nearest
/// column by absolute distance (ties broken toward the LOWER column, same iteration order the
/// legacy function used — `[c - d, c + d]` before `[c + d]`), and column 0 if the whole row
/// somehow held no key at all (never true for `KEYS` today, which has exactly one hole).
fn pad_nearest_col(r: usize, c: usize) -> usize {
    if KEYS[r][c].is_some() {
        return c;
    }
    for d in 1..PAD_COLS {
        if let Some(cc) = c.checked_sub(d) {
            if KEYS[r][cc].is_some() {
                return cc;
            }
        }
        let cc = c + d;
        if cc < PAD_COLS && KEYS[r][cc].is_some() {
            return cc;
        }
    }
    0
}

fn pad_group_of(key: u32) -> Option<GroupId> {
    let (r, c) = pad_rc(key)?;
    KEYS[r][c].is_some().then_some(PAD_GROUP)
}

fn pad_place(measure: &dyn Measure, key: u32) -> Option<Placed> {
    let (r, c) = pad_rc(key)?;
    KEYS[r][c]?;
    let rect = pad_key_rect(measure, r, c);
    Some(Placed {
        rect,
        rest_rect: rect,
        clip: Rect::FULL,
        index: Some((r * PAD_COLS + c) as u32),
    })
}

/// Nearest keypad cell to a source placement, skipping the hole — the pad's own `column_near_x`-
/// style contract, asked once: on the very first `Enter` after the pad opens (`Seat::First` still
/// routes through here, per `FocusEngine::seat_in`), landing on `(0, 0)` ('1') because that cell is
/// nearest whatever `head_of` (the group's own top-left corner) hands it.
fn pad_seat(measure: &dyn Measure, entry: EntryId, from: Placed) -> crate::ui::machine::FocusKey<u32> {
    use crate::ui::machine::FocusKey;
    let (fx_, fy_) = (from.rect.cx(), from.rect.cy());
    let mut best: Option<(f32, usize, usize)> = None;
    for r in 0..PAD_ROWS {
        for c in 0..PAD_COLS {
            if KEYS[r][c].is_none() {
                continue;
            }
            let rect = pad_key_rect(measure, r, c);
            let d = (rect.cx() - fx_).abs() + (rect.cy() - fy_).abs();
            if best.map_or(true, |(bd, ..)| d < bd) {
                best = Some((d, r, c));
            }
        }
    }
    let (_, r, c) = best.unwrap_or((0.0, 0, 0));
    FocusKey { entry, elem: pad_elem(r, c) }
}

fn pad_reconcile(entry: EntryId, want: crate::ui::machine::FocusKey<u32>) -> crate::ui::machine::FocusKey<u32> {
    use crate::ui::machine::FocusKey;
    if let Some((r, c)) = pad_rc(want.elem) {
        if KEYS[r][c].is_some() {
            return want;
        }
    }
    FocusKey { entry, elem: pad_elem(0, 0) }
}

fn pad_group_spec(measure: &dyn Measure) -> GroupSpec {
    GroupSpec {
        id: PAD_GROUP,
        kind: GroupKind::Grid { cols: PAD_COLS, holes: PAD_HOLES },
        seat: Seat::First,
        reachable: AxisMask::BOTH,
        // Self-contained: while the pad is up it is the ONLY group `groups()` answers, so an edge
        // run into a wall has nowhere else to go — BACK (the pad's other way out, besides a
        // completed PIN) is answered in `step`, not through an edge rule.
        edge: [EdgeRule::Stop; 4],
        extent: pad_extent(measure),
        len: PAD_ROWS * PAD_COLS,
        elem: ElemKind::Bare,
    }
}

fn footer_group_spec(measure: &dyn Measure) -> GroupSpec {
    GroupSpec {
        id: FOOTER_GROUP,
        kind: GroupKind::Row { wrap: false },
        seat: Seat::First,
        // Never a LEFT/RIGHT destination — there is nothing beside it, and a stray horizontal
        // geometric search must not land here (`ui/geom.rs`'s `TabRow` sets the same mask for the
        // same reason: a track with one row above it).
        reachable: AxisMask::VERTICAL,
        edge: [EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
        extent: footer_rect(measure),
        len: 1,
        elem: ElemKind::Control,
    }
}

/// The screen's own `LogicalState` (§5.4). What it hashes is deliberately narrow: the roster COUNT
/// (whether the footer is the only reachable group), the pad's open/target/submitting/flashing
/// facts, and the PIN's LENGTH — **never the digits themselves**, which must never reach a log, a
/// recording or a divergence report (the same rule `diag/scrub.rs` states for a title: a PIN is
/// exactly as unloggable, and there is no scrubber standing between this struct and the recorder).
struct ProfilesState {
    roster_n: u32,
    pad_open: bool,
    pad_target: u32,
    pad_len: u32,
    pad_submitting: bool,
    pad_flashing: bool,
}
impl LogicalState for ProfilesState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.roster_n)
            .bool(self.pad_open)
            .u32(self.pad_target)
            .u32(self.pad_len)
            .bool(self.pad_submitting)
            .bool(self.pad_flashing);
    }
    fn probe(&self, out: &mut String) {
        // the PIN's length, never its digits
        out.push_str(&format!(
            "profiles n={} pad_open={} target={} pin_len={} submitting={} flashing={}",
            self.roster_n, self.pad_open, self.pad_target, self.pad_len, self.pad_submitting, self.pad_flashing
        ));
    }
}

pub(crate) struct ProfilesScreen {
    entry: EntryId,
    /// The avatar row's animation cache — focus-scale + scroll springs. A render cache, not
    /// logical state, exactly as `RootPage::table`'s `TableView` is: the engine owns WHICH avatar
    /// is focused, this owns how it gets there on screen.
    row: card_row::CardRow,
    /// `RowStyle::PROFILES` with `margin_x` overridden to this frame's centring offset
    /// (`row_geom`'s `start_x`) — refreshed every `tick` and at construction. `geom::Shelf`
    /// borrows this by reference, so it has to live as long as `&self`, which is why it is a
    /// field rather than a local `let` `Shelf::sty` could not outlive.
    row_sty: card_row::RowStyle,
    /// The footer's own focus pop — one control, so `CtlPop<1>`, exactly as `ui/profiles.rs`'s
    /// `FOOTER_POP` was, just owned rather than a second `static mut`.
    footer_pop: CtlPop<1>,
    /// Free-running rotation clock for the spinner (empty roster, PIN verification, a profile
    /// switch in flight). Render-only, never hashed.
    spin_ms: f32,
    ground: RouteGround,
    pad: Pad,
    state: ProfilesState,
}

impl ProfilesScreen {
    /// A fresh picker. **This constructor IS `ui/profiles.rs`'s old `enter()`**: the route mounts
    /// a brand new instance every time it arrives at `Route::Profiles` (`app/bridge.rs`'s
    /// `AppMounter::mount`, `app::input::enter_profiles_from_onboard`'s doc has the general
    /// argument for why that replaces a reset call), so there is no stale pad, no stale roster
    /// cursor and no leftover footer focus to clear by hand — only the one piece of state that
    /// lives OUTSIDE this screen and would otherwise survive a remount: a PIN verdict from
    /// whatever picker was on screen before this one (`auth::dismiss_pin_error`, the same call
    /// `close_pad` makes for every other door out of the pad — see that method's doc for why a
    /// fresh picker must never inherit one).
    pub(crate) fn new(entry: EntryId) -> Self {
        auth::dismiss_pin_error();
        let mut s = Self {
            entry,
            row: card_row::CardRow::new(),
            row_sty: card_row::RowStyle::PROFILES,
            footer_pop: CtlPop::new(),
            spin_ms: 0.0,
            ground: RouteGround::new(),
            pad: Pad::new(),
            state: ProfilesState {
                roster_n: 0,
                pad_open: false,
                pad_target: 0,
                pad_len: 0,
                pad_submitting: false,
                pad_flashing: false,
            },
        };
        s.ground.reset();
        s.refresh_row_sty();
        let n = auth::users().len();
        s.state = s.snapshot_state(n);
        s
    }

    fn refresh_row_sty(&mut self) {
        let n = auth::users().len();
        let (start_x, _) = row_geom(n);
        self.row_sty = card_row::RowStyle::PROFILES;
        self.row_sty.margin_x = start_x;
    }

    /// The avatar row as the frame's `Focusable` view — the same `card_row::tile_rect`/
    /// `column_near_x` formula `draw` places tiles by, via `ui/geom.rs`'s shared `Shelf` (the
    /// first REAL `Card` group on an owned screen; every prior use of `Shelf` was a host test).
    fn shelf(&self, n: usize) -> geom::Shelf<'_> {
        let pitch = self.row_sty.w + self.row_sty.gap;
        geom::Shelf {
            row: &self.row,
            n,
            sty: &self.row_sty,
            row_y: ROW_Y,
            size: (self.row_sty.w, self.row_sty.h),
            pitch,
            group: ROSTER_GROUP,
            entry: self.entry,
            extent: Rect::new(0.0, ROW_Y, SCR_W as f32, self.row_sty.h),
        }
    }

    fn has_spinner(&self, n: usize) -> bool {
        (n == 0 && !self.pad.open) || (self.pad.open && self.pad.submitting) || auth::phase() == Phase::Switching
    }

    fn snapshot_state(&self, n: usize) -> ProfilesState {
        ProfilesState {
            roster_n: n as u32,
            pad_open: self.pad.open,
            pad_target: self.pad.target as u32,
            pad_len: self.pad.entry.len() as u32,
            pad_submitting: self.pad.submitting,
            pad_flashing: self.pad.error_s > 0.0,
        }
    }

    /// PIN handlers run before Tick; publish their logical changes in the same step.
    fn sync_pin_state(&mut self) {
        self.state = self.snapshot_state(self.state.roster_n as usize);
    }

    /// Advance the wrong-PIN flash by one frame, and invalidate on the phase FLIPS alone — the two
    /// halves `ui/CLAUDE.md` demands of anything that animates from a CLOCK rather than a spring
    /// (`Xfade::tick`/`Spinner::draw`'s own standing hazard). Ported from `ui/profiles.rs`'s
    /// `step_pin_flash`, `ui::idle::invalidate()` replaced by `Effects::invalidate` — this screen
    /// has no `ui::idle` gate to report to; the dispatcher's own `Present` is fed exclusively
    /// through `Effects`/`fx.note`.
    fn step_pin_flash<H: AppLike>(pad: &mut Pad, dt: f32, fx: &mut Effects<'_, H>) {
        if pad.error_s <= 0.0 {
            return;
        }
        let was = pin_flash(pad.error_s);
        pad.error_s = (pad.error_s - dt).max(0.0);
        if pin_flash(pad.error_s) != was {
            fx.invalidate(Provenance::Input);
        }
    }

    fn tick<H: AppLike>(&mut self, dt: f32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.spin_ms += dt * 1000.0;
        self.refresh_row_sty();

        let cur = cx.focus.current.map(|k| k.elem);
        let footer_focused = !self.pad.open && cur == Some(FOOTER);
        // closed while the PIN pad is up, which is also when the control is not drawn at all
        self.footer_pop.step(footer_focused.then_some(0), dt);

        if self.pad.open {
            // A submitted PIN resolves off-thread (`auth::switch_thread`, not yet converted to
            // the addressed-progress shape `auth::LoginProgress` gives the sign-in flow — see
            // that enum's own module doc for the boundary): success routes the app away
            // (`Phase::Ready`); dropping back to `Phase::Profiles` means the switch failed. Only a
            // PIN-blaming failure flashes the dots red and stays up — closing the whole pad on a
            // typo made the user re-pick the profile for every wrong digit — any other failure
            // ("no access to this server", offline) closes the pad through `close_pad` so the
            // picker's own error banner can say WHY.
            if self.pad.submitting && auth::phase() == Phase::Profiles {
                if auth::pin_denied() {
                    self.pad.submitting = false;
                    self.pad.entry.clear();
                    self.pad.error_s = PIN_ERR_S;
                    fx.invalidate(Provenance::Input);
                } else {
                    self.close_pad(fx);
                }
            }
            Self::step_pin_flash(&mut self.pad, dt, fx);
        }

        let n = auth::users().len();
        let roster_focus = if !self.pad.open && !footer_focused {
            cur.filter(|&e| (e as usize) < n).map(|e| e as usize)
        } else {
            None
        };
        self.row.update(n, roster_focus, &self.row_sty, dt);

        if self.has_spinner(n) {
            fx.note(PresentEvent::Motion);
        }

        self.state = self.snapshot_state(n);
    }

    /// Spend a keypad digit/backspace. Ported from `ui/profiles.rs::press`: typing again cancels a
    /// wrong-PIN flash, a full PIN hands off to `auth::submit_pin` and the pad STAYS UP with a
    /// spinner in place of the dots (`ProfilesScreen::tick` watches the flow phase for the
    /// verdict) — closing here would dump the user back on the picker for every typo.
    fn press<H: AppLike>(&mut self, k: u8, fx: &mut Effects<'_, H>) {
        if self.pad.submitting {
            return;
        }
        self.pad.error_s = 0.0;
        if k == b'D' {
            self.pad.entry.pop();
            self.sync_pin_state();
            fx.invalidate(Provenance::Input);
            return;
        }
        if self.pad.entry.len() < PIN_LEN {
            self.pad.entry.push(k as char);
        }
        fx.invalidate(Provenance::Input);
        if self.pad.entry.len() == PIN_LEN {
            let (idx, pin) = (self.pad.target, self.pad.entry.clone());
            auth::submit_pin(idx, &pin);
            self.pad.submitting = true;
        }
        self.sync_pin_state();
    }

    /// Commit a roster tile (an avatar's `PressCommit`, or the dev `pickuser` trigger by way of
    /// `app/run.rs`'s own `auth::select_profile` call — that path bypasses this screen entirely
    /// and only ever names an UNPROTECTED index; see this lane's report for the open problem a
    /// protected one leaves): protected → open the PIN pad and re-seat focus onto it; else hand
    /// straight to `auth::select_profile`'s switch worker.
    fn select<H: AppLike>(&mut self, idx: usize, fx: &mut Effects<'_, H>) {
        let protected = auth::users().get(idx).map(|u| u.protected).unwrap_or(false);
        if protected {
            self.open_pad(idx, fx);
        } else {
            auth::select_profile(idx);
        }
    }

    fn open_pad<H: AppLike>(&mut self, idx: usize, fx: &mut Effects<'_, H>) {
        self.pad = Pad::opened(idx);
        self.sync_pin_state();
        self.seat_pad(fx);
    }

    /// Re-seat focus onto the pad's explicit first cell the moment it appears — the same correction
    /// `screens::login::LoginScreen::tick` makes when its one control appears mid-session: the
    /// container's default `Enter` already ran at MOUNT time, against whatever `groups()`
    /// answered then, and nothing else will ever ask the engine to look again unless this screen
    /// does.
    fn seat_pad<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(crate::ui::machine::FocusKey {
                    entry: self.entry, elem: pad_elem(0, 0),
                }),
            })),
        ));
        fx.invalidate(Provenance::Input);
    }

    /// Take the keypad down, and retire the PIN verdict with it.
    ///
    /// **Every door out of the pad comes through here** — BACK, a pointer click that misses every
    /// keypad cell, and a non-PIN switch failure (`tick`'s doc) — because the pad is the ONLY
    /// surface that asks about a PIN, so it is the only one that may answer about one: a
    /// `pin_denied` left standing after the keypad is gone is a verdict about a control that is no
    /// longer on screen. It does not CANCEL a submission still in flight under its own auth epoch
    /// — a switch the user walked away from can still land, as `Phase::Ready` if the PIN was
    /// right, or by re-raising the verdict this call just cleared if it was wrong (pre-existing;
    /// `ui/profiles.rs`'s own `close_pad` doc has the fuller account, unchanged by this port).
    fn close_pad<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        // Capture the protected avatar's own index BEFORE `self.pad` is reset — every door out
        // of the pad must drop focus back onto the profile the user was trying to unlock, not
        // wherever the roster's default seat happens to be. `Enter::Fresh { focus:
        // ContainerGroup(ROSTER_GROUP) }` reads like "return to the roster" but is not a no-op:
        // `ui/focus.rs::enter`'s `(None, ContainerGroup(g))` arm re-seats from
        // `head_of(spec.extent)`, and the shelf's own `Seat::Nearest` (`ui/geom.rs::Shelf::seat`)
        // resolves the extent's top-left corner to column 0 — so closing the pad on the THIRD
        // avatar silently threw focus onto the FIRST one, on all three doors out (BACK, a pointer
        // miss, and a non-PIN switch failure in `tick`). `FocusTarget::Elem` bypasses that seat
        // search entirely (`ui/focus.rs::enter`'s `(None, Elem(k))` arm takes the key outright),
        // which is what actually returns focus to the tile the user's attention was on.
        let target = self.pad.target as u32;
        self.pad = Pad::new();
        self.sync_pin_state();
        auth::dismiss_pin_error();
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(crate::ui::machine::FocusKey { entry: self.entry, elem: target }),
            })),
        ));
        fx.invalidate(Provenance::Input);
    }

    fn draw_name(p: Painter, u: &auth::UserTile, cx: f32, focused: bool) {
        let col = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
        let name = crate::text::elide(
            &u.title,
            card_row::RowStyle::PROFILES.w + card_row::RowStyle::PROFILES.gap - 12.0,
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

    fn draw_pad<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, cur: Option<u32>) {
        let (title_y, dots_y, _) = pad_geom(f.measure);
        let users = auth::users();
        let name = users.get(self.pad.target).map(|u| u.title.as_str()).unwrap_or("");
        if let Ok(t) = CString::new(format!("Enter {name}'s PIN")) {
            p.text(t.as_ptr(), SCR_W as f32 * 0.5, title_y, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
        }
        // 4 entry dots — replaced by a spinner while the PIN verifies; a rejected PIN pulses the
        // (all-filled) dots DANGER red, then the entry restarts on the same pad.
        if self.pad.submitting {
            Spinner::new(SCR_W as f32 * 0.5, dots_y + PAD_DOT * 0.5, 22.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&Env::inert(), p);
        } else {
            let flash = pin_flash(self.pad.error_s);
            let dgap = 34.0f32;
            let dw = PIN_LEN as f32 * PAD_DOT + (PIN_LEN as f32 - 1.0) * dgap;
            let mut dx = SCR_W as f32 * 0.5 - dw * 0.5;
            for i in 0..PIN_LEN {
                let filled = i < self.pad.entry.len();
                let col = match flash {
                    Some(lit) => theme::with_a(theme::DANGER, if lit { 1.0 } else { 0.16 }),
                    None => theme::with_a(theme::TEXT_PRIMARY, if filled { 1.0 } else { 0.28 }),
                };
                p.rect(Rect::new(dx, dots_y, PAD_DOT, PAD_DOT), PAD_DOT * 0.5, col, col, 0.0);
                dx += PAD_DOT + dgap;
            }
        }
        for (r, row) in KEYS.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let Some(k) = cell else { continue };
                let rect = pad_key_rect(f.measure, r, c);
                let foc = cur == Some(pad_elem(r, c));
                let (fill, ink) = if foc {
                    (theme::ACCENT, theme::ACCENT_INK)
                } else {
                    (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
                };
                p.rect(rect, 18.0, fill, fill, 0.0);
                if *k == b'D' {
                    // a real backspace glyph — the ⌫ codepoint is absent from appfont.ttf
                    let d = (rect.w * 0.42).round();
                    icons::draw(
                        p,
                        icons::Icon::Backspace,
                        Rect::new(rect.x + (rect.w - d) * 0.5, rect.y + (rect.h - d) * 0.5, d, d),
                        ink,
                    );
                } else if let Ok(lc) = CString::new((*k as char).to_string()) {
                    let ty = crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
                    p.text(lc.as_ptr(), rect.x + rect.w * 0.5, ty, theme::size::TITLE, ink, 1, 1);
                }
                f.stop(
                    p,
                    Stop {
                        key: crate::ui::machine::FocusKey { entry: self.entry, elem: pad_elem(r, c) },
                        rect,
                        rest_rect: rect,
                        clip: Rect::FULL,
                        hover: Hover::Focus,
                        // Bare, like every read-out action in the family: fires on the key-down
                        // edge with no hold and no press dip (`login.rs`'s bare control is the
                        // precedent for `Activate::Direct` on a `Bare` element).
                        activate: Activate::Direct,
                    },
                );
            }
        }
    }
}

/// A borrowed query view with one roster count, shared by production and isolated engine tests.
struct ProfilesView<'a> {
    screen: &'a ProfilesScreen,
    n: usize,
}

impl std::ops::Deref for ProfilesView<'_> {
    type Target = ProfilesScreen;
    fn deref(&self) -> &Self::Target { self.screen }
}

impl ProfilesScreen {
    fn focus_view(&self) -> ProfilesView<'_> {
        ProfilesView { screen: self, n: auth::users().len() }
    }
}

impl<H: AppLike> Focusable<H> for ProfilesView<'_> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.pad.open {
            out.push(pad_group_spec(cx.measure));
            return;
        }
        let n = self.n;
        if n > 0 {
            Focusable::<H>::groups(&self.shelf(n), cx, out);
        }
        // always present, even with an empty/loading roster — "reachable even while the roster is
        // empty/loading" is the picker's own standing rule (`ui/profiles.rs`'s `act` doc).
        out.push(footer_group_spec(cx.measure));
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        if self.pad.open {
            return pad_group_of(*key);
        }
        if *key == FOOTER {
            return Some(FOOTER_GROUP);
        }
        let n = self.n;
        Focusable::<H>::group_of(&self.shelf(n), key, cx)
    }
    fn neighbour(&self, key: crate::ui::machine::FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if self.pad.open {
            return pad_neighbour(key.entry, key.elem, dir);
        }
        if key.elem == FOOTER {
            // the footer never moves "inside itself" — every direction escalates to its own edge
            // rule, which is where ▲ reaching the roster and ▼/◀/▶ holding it are decided
            return Step::Edge;
        }
        let n = self.n;
        Focusable::<H>::neighbour(&self.shelf(n), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if self.pad.open {
            return pad_place(cx.measure, *key);
        }
        if *key == FOOTER {
            let r = footer_rect(cx.measure);
            return Some(Placed { rect: r, rest_rect: r, clip: Rect::FULL, index: Some(0) });
        }
        let n = self.n;
        Focusable::<H>::place(&self.shelf(n), key, cx, at)
    }
    fn reconcile(&self, want: crate::ui::machine::FocusKey<u32>, _cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        if self.pad.open {
            return pad_reconcile(self.entry, want);
        }
        if want.elem == FOOTER {
            return want;
        }
        let n = self.n;
        if n == 0 {
            return crate::ui::machine::FocusKey { entry: self.entry, elem: FOOTER };
        }
        if (want.elem as usize) >= n {
            return crate::ui::machine::FocusKey { entry: self.entry, elem: (n - 1) as u32 };
        }
        want
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        if self.pad.open {
            return pad_seat(cx.measure, self.entry, from);
        }
        if g == FOOTER_GROUP {
            return crate::ui::machine::FocusKey { entry: self.entry, elem: FOOTER };
        }
        let n = self.n;
        Focusable::<H>::seat(&self.shelf(n), g, from, cx)
    }
}

impl<H: AppLike> Focusable<H> for ProfilesScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        Focusable::<H>::groups(&self.focus_view(), cx, out)
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        Focusable::<H>::group_of(&self.focus_view(), key, cx)
    }
    fn neighbour(&self, key: crate::ui::machine::FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        Focusable::<H>::neighbour(&self.focus_view(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        Focusable::<H>::place(&self.focus_view(), key, cx, at)
    }
    fn reconcile(&self, key: crate::ui::machine::FocusKey<u32>, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        Focusable::<H>::reconcile(&self.focus_view(), key, cx)
    }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        Focusable::<H>::seat(&self.focus_view(), group, from, cx)
    }
}

impl<H: AppLike> Machine<H> for ProfilesScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), cx, fx);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                // the pad's own keys — Bare, so the engine fires this on the key-down edge with
                // no press arm at all (see the module doc)
                if self.pad.open {
                    if let Some((r, c)) = pad_rc(*e) {
                        if let Some(k) = KEYS[r][c] {
                            self.press(k, fx);
                        }
                    }
                }
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                // the roster avatar and the footer are the only two `Card`/`Control` groups this
                // screen ever declares (the pad's keys are Bare and never reach a press machine at
                // all), so `cx.focus.current` alone is enough to say which one just committed —
                // mirrors `screens::consent`'s own `PressCommit` arm reading the same field for
                // the same reason.
                //
                // The DECISION is [`commit_action`], a pure function, and the split is not
                // decoration: every arm below reaches a real worker (`auth::sign_out` and
                // `select` both start a switch), which is why this module's test doc explains it
                // presses none of them. The legacy `ui/profiles.rs` could grade the same table
                // freely because its ladder was a pure `act()`; when that module was deleted the
                // pin went with it, and the OK table — pill means sign out, tile means switch,
                // an empty roster means neither — had no analog here. A behaviour that only one
                // deleted file ever tested is exactly the kind this repository's rules forbid
                // losing to a refactor, so the decision moved out where it can still be graded.
                //
                // `auth::users()` is read only on the branch that can need it: the roster length
                // decides nothing for a footer commit or for one arriving while the pad is open.
                let elem = cx.focus.current.map(|k| k.elem);
                let roster_n = match elem {
                    Some(e) if !self.pad.open && e != FOOTER => auth::users().len(),
                    _ => 0,
                };
                match commit_action(self.pad.open, elem, roster_n) {
                    Commit::SignOut => auth::sign_out(),
                    Commit::Select(i) => self.select(i, fx),
                    Commit::Nothing => {}
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            // The pad's own door for a pointer click that lands on NONE of its twelve stops — see
            // the module doc for why this has to be built here rather than reached for a library
            // `OnMiss` policy (the pad is not a container surface).
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { hit, .. }, .. }) if self.pad.open => {
                if hit.is_none() {
                    self.close_pad(fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key, sym, wcode, edge: Edge::Down, .. },
                ..
            }) => {
                if self.pad.open {
                    if *key == Key::Back {
                        self.close_pad(fx);
                        return Handled::Yes;
                    }
                    if self.pad.submitting {
                        // verification in flight — only BACK acts, matching `ui/profiles.rs`'s
                        // own `pad_key`
                        return Handled::Yes;
                    }
                    if let Some(d) = digit_of(*sym, *wcode) {
                        self.press(d, fx);
                        return Handled::Yes;
                    }
                    // ▲▼◀▶/OK fall through to the engine's generic grid walk (`pad_neighbour`) and
                    // Bare activation
                    return Handled::No;
                }
                // BACK leaves the picker the way choosing the already-active profile does:
                // `auth::cancel` re-arms the resolved-credentials handoff with the persisted
                // session if there is one, else the loop hands the screen to the television's own
                // Home — `app::input::login_or_profiles_root_back`'s doc has the full account of
                // why `auth::cancel`'s answer has to be asked from the loop rather than here (it
                // needs the same `webos::take_root_press` cooldown `LoopReq::BackAtRoot`'s sibling
                // screens share, which this screen module may not name).
                if *key == Key::Back {
                    fx.push(Fx::App(AppFx::Loop(LoopReq::AuthBackAtRoot)));
                    return Handled::Yes;
                }
                Handled::No
            }
            _ => Handled::No,
        }
    }
}

impl<H: AppLike> Screen<H> for ProfilesScreen {
    fn name(&self) -> &'static str {
        word::PROFILES
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        // one of the family's three routes with nowhere for BACK to go INSIDE the app
        // (`ui/CLAUDE.md`'s route-family rule) — see this screen's BACK arm.
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = Painter::root();
        // The picker has no page of its own to layer over (same reasoning as
        // `screens::login::LoginScreen::draw`): draw the ambient ground off `Painter::root()`
        // rather than `f.painter`, so the wash never rides a page-transition cascade.
        self.ground.draw_default(p);
        let cur = f.focus.current.map(|k| k.elem);

        if self.pad.open {
            self.draw_pad(f, p, cur);
            return;
        }

        if let Ok(t) = CString::new(TITLE) {
            p.text(t.as_ptr(), SCR_W as f32 * 0.5, 168.0, theme::size::HERO, theme::TEXT_PRIMARY, 1, 1);
        }

        let users = auth::users();
        let n = users.len();
        let (start_x, slot) = row_geom(n);
        let scroll = self.row.scroll_x();
        let footer_focused = cur == Some(FOOTER);
        let roster_focus = (!footer_focused).then(|| cur.filter(|&e| (e as usize) < n).map(|e| e as usize)).flatten();
        let extent = Rect::new(0.0, ROW_Y, SCR_W as f32, self.row_sty.h);

        let mut focused_i = None;
        for (i, u) in users.iter().enumerate() {
            let cx_ = start_x + i as f32 * slot + self.row_sty.w * 0.5 - scroll;
            let base = Rect::new(cx_ - self.row_sty.w * 0.5, ROW_Y, self.row_sty.w, self.row_sty.h);
            let sc = self.row.scale(i);
            if roster_focus == Some(i) {
                focused_i = Some(i);
                continue; // draw the focused tile last (ring over neighbours)
            }
            card_row::draw_tile(
                p,
                Art::Thumb { sid: crate::plex::current_server(), key: &u.thumb, res: (300, 300) },
                base.scaled(sc),
                sc,
                &self.row_sty,
                None,
            );
            Self::draw_name(p, u, cx_, false);
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey { entry: self.entry, elem: i as u32 },
                    rect: base.scaled(sc),
                    rest_rect: base,
                    clip: extent,
                    hover: Hover::Focus,
                    activate: Activate::Press,
                },
            );
        }
        if let Some(i) = focused_i {
            let u = &users[i];
            let cx_ = start_x + i as f32 * slot + self.row_sty.w * 0.5 - scroll;
            let base = Rect::new(cx_ - self.row_sty.w * 0.5, ROW_Y, self.row_sty.w, self.row_sty.h);
            // fold the ui::press click dip into the focused avatar's pop (1.0 when idle)
            let sc = self.row.scale(i) * crate::ui::press::scale();
            Self::draw_name(p, u, cx_, true);
            card_row::draw_focused(
                p,
                Art::Thumb { sid: crate::plex::current_server(), key: &u.thumb, res: (300, 300) },
                base.scaled(sc),
                sc,
                &self.row_sty,
                None,
                &card_row::TileLabel::default(),
            );
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey { entry: self.entry, elem: i as u32 },
                    rect: base.scaled(sc),
                    rest_rect: base,
                    clip: extent,
                    hover: Hover::Focus,
                    activate: Activate::Press,
                },
            );
        }

        // "Sign out" — the picker is the only surface a user who doesn't recognise these profiles
        // ever sees, so it must offer a way out of the account.
        let footer_r = footer_rect(f.measure);
        Button::new(c"Sign out".as_ptr(), theme::size::BODY, footer_r)
            .focused(footer_focused)
            .scale(self.footer_pop.scale(0))
            .palette(self.ground.palette())
            .draw(&Env::inert(), p);
        f.stop(
            p,
            Stop {
                key: crate::ui::machine::FocusKey { entry: self.entry, elem: FOOTER },
                rect: footer_r,
                rest_rect: footer_r,
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Press,
            },
        );

        // roster not here yet (persisted seed empty, refresh in flight) — a spinner, not a blank page
        if users.is_empty() {
            Spinner::new(SCR_W as f32 * 0.5, ROW_Y + self.row_sty.h * 0.5, 26.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&Env::inert(), p);
        }

        // a failed switch (wrong PIN, offline) drops the flow back here with an error
        let err = auth::error();
        if !err.is_empty() && auth::phase() == Phase::Profiles {
            if let Ok(e) = CString::new(err) {
                let ey = crate::text::text_vcenter_y(theme::size::BODY, 0, ERROR_Y);
                p.text(e.as_ptr(), SCR_W as f32 * 0.5, ey, theme::size::BODY, theme::TEXT_SECONDARY, 1, 0);
            }
        }

        if auth::phase() == Phase::Switching {
            p.rect(Rect::FULL, 0.0, theme::scrim_black(0.88), theme::scrim_black(0.88), 0.0);
            Spinner::new(SCR_W as f32 * 0.5, 500.0, 26.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&Env::inert(), p);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

// avoid an unused-import warning on `widgets` (only `Button`/`CtlPop`/`Spinner` are named above,
// but the module import brings the glob-free path in for doc-links); referenced explicitly here
// so a future trim of the `use` list has one fewer thing to puzzle over.
#[allow(unused_imports)]
use widgets as _unused_widgets_module;

#[cfg(test)]
mod tests {
    //! The picker's pure geometry and the pad's own state machine, in the split every screen this
    //! restructure has already moved carries: pure functions (`pad_neighbour`, `pin_flash`,
    //! `digit_of`, `footer_rect`, `pad_geom`) are graded directly, and the live half drives a bare
    //! `ProfilesScreen` — built directly as a struct literal, exactly as `screens::login::
    //! LoginScreen`'s own `bare_screen` helper does — through `Machine::step` with a hand-built
    //! `Cx`/`Effects`, never through `crate::auth`'s process-global controller: this module's own
    //! roster/PIN worker calls (`select_profile`, `sign_out`, `submit_pin`) are exactly the arms
    //! `ui/profiles.rs`'s own test module refused to press ("No test presses OK against the
    //! singleton"), because they reach a real switch/sign-in worker. What differs from that
    //! module's split is WHY: there, the ladder was pure `act`/`step_fc` free functions with no
    //! engine at all; here, ordinary navigation (▲▼◀▶ across the roster and the footer) is the
    //! GENERIC engine's job (`ui/focus.rs`'s own exhaustive test suite already grades a `Row`
    //! group's stepping, an `EdgeRule::Geometric` search excluding an unreachable axis, and the
    //! "no focus yet → first group with len > 0" fallback this screen's empty-roster footer relies
    //! on) — so the tests below grade what is genuinely this screen's OWN: the pad's grid-with-a-
    //! hole walk, its geometry, its digit/backspace state machine, and the doors that close it.
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{Edge, FocusKey, FocusRead, InputEvent, InputKind, InputOwner, InstanceId, MachineId, PressRead, Source, Stamped, Tick};
    use crate::ui::present::Present;

    use super::super::family::InnerHost;

    static MEASURE: FixtureMeasure = FixtureMeasure;

    fn cx(focus: Option<FocusKey<u32>>) -> Cx<'static, InnerHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &MEASURE,
            press: PressRead::default(),
            focus: FocusRead { current: focus , ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// A screen with no read of `crate::auth` at all (unlike `ProfilesScreen::new`, which calls
    /// `auth::dismiss_pin_error` on the shared controller) — for grading the pad's own state
    /// machine and geometry in complete isolation from a process-global other tests in this binary
    /// may have left in an arbitrary state. Mirrors `screens::login::LoginScreen`'s own
    /// `bare_screen` helper.
    fn bare(pad: Pad) -> ProfilesScreen {
        ProfilesScreen {
            entry: EntryId(0),
            row: card_row::CardRow::new(),
            row_sty: card_row::RowStyle::PROFILES,
            footer_pop: CtlPop::new(),
            spin_ms: 0.0,
            ground: RouteGround::new(),
            pad,
            state: ProfilesState {
                roster_n: 0,
                pad_open: false,
                pad_target: 0,
                pad_len: 0,
                pad_submitting: false,
                pad_flashing: false,
            },
        }
    }

    fn step_ev(s: &mut ProfilesScreen, ev: &ScreenEvent<InnerHost>, focus: Option<FocusKey<u32>>) -> (Handled, Vec<Stamped<InnerHost>>) {
        let c = cx(focus);
        let mut present = Present::new();
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let handled = {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            Machine::<InnerHost>::step(s, ev, &c, &mut fx)
        };
        (handled, buf)
    }

    fn key_down(key: Key, sym: u32, wcode: u32) -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key, sym, wcode, edge: Edge::Down, at_edge: false },
        })
    }

    fn click(hit: Option<u32>) -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Script,
            kind: InputKind::Click { x: 0.0, y: 0.0, hit },
        })
    }

    // ---------------------------------------------------------------------------------------
    // the keypad grid's own hole-skip walk
    // ---------------------------------------------------------------------------------------

    #[test]
    fn pin_edits_publish_logical_state_before_the_next_tick() {
        let mut s = bare(Pad::opened(2));
        s.state = s.snapshot_state(3);
        step_ev(&mut s, &key_down(Key::Other, b'7' as u32, 0), None);
        assert_eq!(s.state.pad_len, 1);
        assert_eq!(s.state.pad_target, 2);
        let typed = Screen::<InnerHost>::state(&s).hash();
        step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 2)), None);
        assert_eq!(s.state.pad_len, 0, "delete must publish before returning early");
        assert_ne!(Screen::<InnerHost>::state(&s).hash(), typed);
        assert_eq!(s.state.roster_n, 3);
    }

    #[test]
    fn dispatcher_queued_pad_focus_reseat_does_not_reset_the_screen() {
        let mut s = bare(Pad::new());
        s.state.roster_n = 3;
        let mut present = Present::new();
        let mut effects = Vec::<Stamped<InnerHost>>::new();
        {
            let mut fx = Effects::new(&mut effects, MachineId::Instance(InstanceId(7)), &mut present);
            s.open_pad(2, &mut fx);
        }
        assert!(s.state.pad_open);
        assert_eq!(s.state.pad_target, 2);
        let event = effects.into_iter().find_map(|st| match st.fx {
            Fx::Deliver(owner, Delivery::Screen(event @ ScreenEvent::Enter(_))) => {
                assert_eq!(owner, MachineId::Instance(InstanceId(7)));
                Some(event)
            }
            _ => None,
        }).expect("opening must reseat the actual mounted instance");
        assert!(matches!(&event, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })
            if k.elem == pad_elem(0, 0)));
        step_ev(&mut s, &event, None);
        assert!(s.pad.open);
        assert_eq!(s.pad.target, 2);
        step_ev(&mut s, &key_down(Key::Other, b'7' as u32, 0), None);
        assert_eq!(s.pad.entry, "7");
        assert_eq!(s.state.pad_len, 1);
    }

    #[test]
    fn closing_pin_publishes_the_closed_state_before_the_next_tick() {
        let _serial = crate::testlock::serial();
        let mut s = bare(Pad { entry: "12".into(), ..Pad::opened(2) });
        s.state = s.snapshot_state(3);
        step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
        assert!(!s.state.pad_open);
        assert_eq!(s.state.pad_len, 0);
        assert!(!s.state.pad_submitting);
        assert_eq!(s.state.roster_n, 3);
    }

    #[test]
    fn declared_pad_holes_match_the_keys_and_edges() {
        let holes: Vec<(usize, usize)> = KEYS.iter().flatten().enumerate()
            .filter_map(|(i, value)| value.is_none().then_some((i / PAD_COLS, i % PAD_COLS))).collect();
        assert_eq!(PAD_HOLES, holes.as_slice());
        assert!(matches!(pad_neighbour(EntryId(1), pad_elem(3, 1), Dir::Right),
            Step::Move(key) if key.elem == pad_elem(3, 2)));
        assert!(matches!(pad_neighbour(EntryId(1), pad_elem(0, 0), Dir::Left), Step::Edge));
    }

    #[test]
    fn down_from_the_roster_reaches_the_footer_and_holds_there() {
        use crate::ui::focus::{FocusEngine, Outcome};
        let s = bare(Pad::new());
        let view = ProfilesView { screen: &s, n: 4 };
        let mut engine = FocusEngine::new();
        let owner = InputOwner::Entry(s.entry);
        engine.set(owner, FocusKey { entry: s.entry, elem: 2 }, Some(ROSTER_GROUP), crate::ui::screen::By::Restore);
        assert!(matches!(engine.move_dir(owner, &view, &[], Dir::Down, &cx(None)),
            Outcome::Moved { to, .. } if to.elem == FOOTER));
        for dir in [Dir::Down, Dir::Left, Dir::Right] {
            let _ = engine.move_dir(owner, &view, &[], dir, &cx(None));
            assert_eq!(engine.current(owner).unwrap().elem, FOOTER);
        }
        assert!(matches!(engine.move_dir(owner, &view, &[], Dir::Up, &cx(None)),
            Outcome::Moved { to, .. } if to.elem < 4));
    }

    #[test]
    fn the_footer_is_reachable_with_no_roster_at_all() {
        let s = bare(Pad::new());
        let view = ProfilesView { screen: &s, n: 0 };
        let mut engine = crate::ui::focus::FocusEngine::new();
        let owner = InputOwner::Entry(s.entry);
        engine.enter(owner, &view, FocusTarget::ContainerGroup(ROSTER_GROUP), None, &cx(None));
        assert_eq!(engine.current(owner).unwrap().elem, FOOTER);
    }

    #[test]
    fn the_roster_neighbour_clamps_at_both_ends() {
        let s = bare(Pad::new());
        let view = ProfilesView { screen: &s, n: 4 };
        assert!(matches!(Focusable::<InnerHost>::neighbour(&view,
            FocusKey { entry: s.entry, elem: 0 }, Dir::Left, &cx(None)), Step::Edge));
        assert!(matches!(Focusable::<InnerHost>::neighbour(&view,
            FocusKey { entry: s.entry, elem: 3 }, Dir::Right, &cx(None)), Step::Edge));
    }

    /// The cell both walks below have to step over.
    #[test]
    fn the_keypad_has_exactly_one_hole_at_the_bottom_left() {
        assert_eq!(KEYS[3][0], None);
        assert!(KEYS.iter().flatten().filter(|c| c.is_none()).count() == 1);
    }

    /// The keypad's own walk, over every edge and hole case the shape actually has — ported from
    /// `ui/profiles.rs`'s `the_keypad_walks_around_its_empty_cell`, over `pad_neighbour` rather
    /// than the retired free functions it called directly.
    #[test]
    fn pad_neighbour_walks_around_its_empty_cell() {
        let e = EntryId(1);
        // DOWN from '7' (row 2, col 0): the hole sits directly below it, so the move DEFLECTS
        // sideways to the nearest occupied column in row 3 — '0', one column over — exactly as
        // the legacy `nearest_col` did. This is the case the engine's own fixture-only "skip
        // along the direction of travel" rule gets wrong for this grid: skipping straight down
        // from here runs off the bottom edge with nothing to land on (see the module doc).
        match pad_neighbour(e, pad_elem(2, 0), Dir::Down) {
            Step::Move(k) => assert_eq!(pad_rc(k.elem), Some((3, 1)), "▼ off 7 lands on 0"),
            Step::Edge => panic!("▼ off 7 must deflect onto 0, not dead-end at the hole"),
        }
        // DOWN from '8' (row 2, col 1) is an ordinary vertical move, straight to '0' — the
        // destination column is already occupied, so there is nothing to deflect.
        match pad_neighbour(e, pad_elem(2, 1), Dir::Down) {
            Step::Move(k) => assert_eq!(pad_rc(k.elem), Some((3, 1))),
            Step::Edge => panic!("row 2 col 1 has an occupied cell directly below it"),
        }
        // LEFT from '0' (row 3, col 1) runs into the hole and stops — LEFT/RIGHT skip a hole
        // WITHIN their own row rather than deflecting to a different one, and there is nothing
        // further left of it here.
        assert!(matches!(pad_neighbour(e, pad_elem(3, 1), Dir::Left), Step::Edge));
        // RIGHT from delete (row 3, col 2) has nothing further right.
        assert!(matches!(pad_neighbour(e, pad_elem(3, 2), Dir::Right), Step::Edge));
        // UP from '1' (row 0, col 0), the grid's own top-left corner, leaves entirely.
        assert!(matches!(pad_neighbour(e, pad_elem(0, 0), Dir::Up), Step::Edge));
    }

    /// `pad_nearest_col` on its own, over exactly the three cases `ui/profiles.rs`'s retired
    /// `nearest_col` was pinned against — the property `pad_neighbour`'s vertical arm now
    /// restores after the engine's fixture-only "skip along the direction of travel" rule was
    /// found to reach it only in DECLARATION (the `holes` field is inert in production; see the
    /// module doc) while leaving DOWN off '7' a dead press in practice.
    #[test]
    fn pad_nearest_col_deflects_onto_the_nearest_occupied_column() {
        assert_eq!(pad_nearest_col(3, 0), 1, "▼ off 7 lands on 0");
        assert_eq!(pad_nearest_col(3, 2), 2, "▼ off 9 lands on delete, which is under it");
        assert_eq!(pad_nearest_col(1, 1), 1, "an occupied column is kept as it is");
    }

    // ---------------------------------------------------------------------------------------
    // pure geometry — no font, no live auth
    // ---------------------------------------------------------------------------------------

    /// The pad's centred unit stays on the panel and the keypad sits BELOW the title with the
    /// declared gap, whatever `Measure` answers — AND the unit is actually centred, not merely
    /// on-panel. The three ordering checks alone do not pin that: a `pad_geom` that hard-coded
    /// `unit_top` to some small on-panel constant (the exact pre-port bug this function exists to
    /// fix — see its own doc, "it used to sit low on the panel") would still satisfy every
    /// ordering assertion here while leaving a lopsided gap above the title and below the keypad.
    #[test]
    fn the_pad_is_one_centred_unit_with_the_keypad_below_the_title() {
        let m = FixtureMeasure;
        let (title_y, dots_y, grid_y) = pad_geom(&m);
        assert!(title_y > 0.0 && title_y < SCR_H as f32);
        assert!(grid_y > title_y, "the keypad sits below the title");
        assert!(dots_y > title_y && dots_y < grid_y, "the dot row sits between the two");
        assert!(grid_y + PAD_GRID_H < SCR_H as f32, "the keypad fits on the panel");
        // The unit's own top edge is `title_y` (the title is the first thing drawn in it) and its
        // bottom edge is `grid_y + PAD_GRID_H` (the keypad is the last) — a centred block puts an
        // EQUAL gap above and below those two edges.
        let top_gap = title_y;
        let bottom_gap = SCR_H as f32 - (grid_y + PAD_GRID_H);
        assert!(
            (top_gap - bottom_gap).abs() < 0.5,
            "the unit is not centred: {top_gap:.1}px above it, {bottom_gap:.1}px below it"
        );
    }

    /// `draw` and every `Focusable` query read `pad_key_rect` — pin the walk it is built from:
    /// columns advance in x, rows in y, and the hole's own coordinates are never asked for a key
    /// (there is no key THERE to draw or place).
    #[test]
    fn pad_key_rect_advances_by_column_and_row() {
        let m = FixtureMeasure;
        let r00 = pad_key_rect(&m, 0, 0);
        let r01 = pad_key_rect(&m, 0, 1);
        let r10 = pad_key_rect(&m, 1, 0);
        assert!((r01.x - r00.x - (PAD_KEY + PAD_KGAP)).abs() < 0.01);
        assert!((r00.x - r10.x).abs() < 0.01, "row does not move x");
        assert!((r10.y - r00.y - (PAD_KEY + PAD_KGAP)).abs() < 0.01);
        // `is_none()` rather than `assert_eq!(.., None)`: `Placed` deliberately carries no
        // `PartialEq` (it is geometry — two rects that differ in the last float are not a
        // meaningful inequality), and deriving one on a library type to satisfy one assertion
        // would put an equality into the engine's vocabulary that nothing else wants.
        assert!(
            pad_place(&m, pad_elem(3, 0)).is_none(),
            "no key exists at the hole"
        );
    }

    /// The footer pill widens with its label's own measured width — never a bare fixed box.
    /// `r.w > 76.0` alone does not pin that: a `footer_rect` that returned a hard-coded `300.0`
    /// (wider than the side padding by construction) would still pass it. Recomputing the exact
    /// expected width from the SAME `Measure` call `footer_rect` makes internally is what actually
    /// proves the pill tracks the label rather than a fixed number that happens to be bigger.
    #[test]
    fn the_footer_rect_widens_with_the_measured_label() {
        let m = FixtureMeasure;
        let tw = m.width(c"Sign out", theme::size::BODY, true);
        let r = footer_rect(&m);
        assert_eq!(r.w, tw + 76.0, "the pill's width must be the measured label plus its own side padding");
        assert!((r.cx() - SCR_W as f32 * 0.5).abs() < 0.01, "centred on the panel");
    }

    // ---------------------------------------------------------------------------------------
    // pin_flash / digit_of — pure, ported verbatim from `ui/profiles.rs`
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_pin_digit_is_read_from_either_field() {
        assert_eq!(digit_of(b'7' as u32, 0), Some(b'7'), "a dev keyboard's sym");
        assert_eq!(digit_of(0, 55), Some(b'7'), "the remote's wcode, same digit");
        assert_eq!(digit_of(b'0' as u32, 0), Some(b'0'));
        assert_eq!(digit_of(999, 0), None, "not a digit in either field");
    }

    /// A 1.4s error window reads as several distinct red pulses, not one — the assertion a static
    /// tint fails no matter how red it is. Ported from `ui/profiles.rs`'s own property test,
    /// minus the `ui::idle` half (this screen reports through `Effects` instead — see the next
    /// test for that half).
    #[test]
    fn the_wrong_pin_flash_blinks_at_least_four_times() {
        let dt = 1.0 / 60.0;
        let mut error_s = PIN_ERR_S;
        assert_eq!(pin_flash(error_s), Some(true), "opens LIT");
        let mut phases = vec![pin_flash(error_s)];
        let mut frames = 0;
        while error_s > 0.0 && frames < 600 {
            frames += 1;
            error_s = (error_s - dt).max(0.0);
            let now = pin_flash(error_s);
            if now != *phases.last().unwrap() {
                phases.push(now);
            }
        }
        assert_eq!(phases.last(), Some(&None), "the last report returns the dots to their resting ink");
        let lit = phases.iter().filter(|p| **p == Some(true)).count();
        assert!(lit >= 4, "a 1.4s window must read as several pulses — got {lit} ({phases:?})");
    }

    /// The flip reports to the dispatcher's `Present`, not to `ui::idle` — the new half of the
    /// property above. Graded on a fresh `Present` (whose own `dirty` starts `true` — "the first
    /// frame always draws" — so it is drained once before the assertion means anything).
    #[test]
    fn a_flash_flip_invalidates_the_present_gate() {
        let mut present = Present::new();
        present.take(0); // drain the fresh gate's own always-dirty first frame
        let mut pad = Pad { open: true, error_s: PIN_ERR_S, ..Pad::new() };
        let dt = 1.0 / 60.0;
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut saw_a_flip_invalidate = false;
        while pad.error_s > 0.0 {
            let was = pin_flash(pad.error_s);
            {
                let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
                ProfilesScreen::step_pin_flash(&mut pad, dt, &mut fx);
            }
            let now = pin_flash(pad.error_s);
            let asked = present.take(0);
            if now != was {
                assert!(asked, "the dot row changed colour and did not ask to be drawn");
                saw_a_flip_invalidate = true;
            } else {
                assert!(!asked, "a dot row mid-phase asked for a repaint");
            }
        }
        assert!(saw_a_flip_invalidate, "the loop above never actually crossed a phase boundary");
    }

    // ---------------------------------------------------------------------------------------
    // the pad's own doors — BACK, a pointer miss, digit entry, submitting
    // ---------------------------------------------------------------------------------------

    /// BACK takes the keypad down and retires the PIN verdict with it — the one property
    /// `ui/profiles.rs`'s own `every_door_out_of_the_keypad_goes_through_one_close` existed to
    /// protect, ported onto the real `step`/`close_pad` rather than a hand-rolled `Pad::new()`.
    ///
    /// **Pinned on `target: 2`, not 0** — with the pad opened on the FIRST avatar every past
    /// version of this bug (`close_pad` re-seating through `Enter::Fresh { focus:
    /// ContainerGroup(ROSTER_GROUP) }`, which resolves to the shelf's own `head_of` corner, tile
    /// 0) would still pass by accident. A protected profile deep in the roster is the case that
    /// actually distinguishes "closing returns to where the user was" from "closing always lands
    /// on profile 0".
    #[test]
    fn back_closes_the_pad_and_clears_the_verdict() {
        let _s = crate::testlock::serial();
        auth::set_pin_denied_for_test(true);
        let mut s = bare(Pad { open: true, target: 2, error_s: PIN_ERR_S, entry: "12".into(), ..Pad::new() });
        let (handled, effs) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
        assert_eq!(handled, Handled::Yes);
        assert!(!s.pad.open, "BACK takes the keypad down");
        assert_eq!(s.pad.error_s, 0.0, "…and the flash goes with it");
        assert!(!auth::pin_denied(), "…and the verdict, which is what leaks onto the roster behind it");
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                    if k.elem == 2
            )),
            "closing returns focus to the PROTECTED profile the pad was opened on (index 2), \
             not wherever the roster's default seat lands"
        );
        auth::set_pin_denied_for_test(false);
    }

    /// **The shared mechanism, tested directly.** All three doors out of the pad — BACK, a
    /// pointer miss (both above/below), and `tick`'s own non-PIN-failure branch (`ProfilesScreen`'s
    /// module doc, `close_pad`'s doc) — call this one function, so a fix or a regression in it
    /// moves all three at once. `tick`'s own call site cannot be driven from this module without
    /// also puppeting `auth.rs`'s network-epoch machinery just to make `auth::phase()` read
    /// `Phase::Profiles` (a different lane's file), so this test exercises `close_pad` on its own
    /// terms instead: it is the guard against the exact regression shape the bug had — reading
    /// `self.pad.target` AFTER `self.pad = Pad::new()` has already zeroed it, which is invisible
    /// to a caller and fails every door identically.
    #[test]
    fn close_pad_returns_focus_to_its_own_target_not_index_zero() {
        let _serial = crate::testlock::serial();
        let mut present = Present::new();
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let mut s = bare(Pad::opened(2));
        {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            s.close_pad(&mut fx);
        }
        assert!(!s.pad.open);
        assert!(
            buf.iter().any(|st| matches!(
                &st.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                    if k.elem == 2
            )),
            "close_pad must capture pad.target BEFORE replacing self.pad, or every caller reads back 0"
        );
    }

    /// With the pad CLOSED, every BACK is the root press — asked of the loop
    /// (`LoopReq::AuthBackAtRoot`) rather than performed here, exactly as `screens::login::
    /// LoginScreen`'s own `back_is_always_the_root_press` test states for its sibling screen.
    #[test]
    fn back_with_the_pad_closed_asks_the_loop_for_the_root_press() {
        let mut s = bare(Pad::new());
        let (handled, effs) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
        assert_eq!(handled, Handled::Yes);
        assert!(
            effs.iter().any(|st| matches!(&st.fx, Fx::App(AppFx::Loop(LoopReq::AuthBackAtRoot)))),
            "the picker has no panel of its own to close first — BACK asks the loop for the root press"
        );
    }

    /// A pointer click that resolves onto NONE of the pad's twelve stops (`hit: None`) is the
    /// pointer's own door out — the module doc's account of why this has to be answered from the
    /// raw `Click` event rather than a library `OnMiss` policy.
    #[test]
    fn a_pointer_click_outside_every_pad_stop_closes_it_too() {
        let _s = crate::testlock::serial();
        auth::set_pin_denied_for_test(true);
        // `target: 2`, not 0 — see `back_closes_the_pad_and_clears_the_verdict`'s doc for why the
        // first avatar cannot distinguish a real fix from the old default-seat bug.
        let mut s = bare(Pad { open: true, target: 2, error_s: PIN_ERR_S, ..Pad::new() });
        let (handled, effs) = step_ev(&mut s, &click(None), None);
        assert_eq!(handled, Handled::Yes);
        assert!(!s.pad.open, "the pointer's miss takes the keypad down too");
        assert!(!auth::pin_denied(), "the pointer's door owes the SAME cleanup BACK's does");
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                    if k.elem == 2
            )),
            "the pointer's door owes the SAME focus-return BACK's does — back to profile 2, not profile 0"
        );
        auth::set_pin_denied_for_test(false);

        // …and a click that DID hit a cell must not also close the pad — that is the Activate
        // event's job, not this one's.
        let mut s2 = bare(Pad::opened(0));
        let (_, _) = step_ev(&mut s2, &click(Some(pad_elem(0, 0))), None);
        assert!(s2.pad.open, "a hit is not a miss");
    }

    /// A number key types straight into the entry without moving the visual cursor, and a full PIN
    /// hands off to `submitting` — never actually reaching four digits here (that calls
    /// `auth::submit_pin`, the switch worker this module's tests do not press, mirroring
    /// `ui/profiles.rs`'s own "No test presses OK against the singleton").
    #[test]
    fn a_digit_key_types_into_the_entry_and_backspace_removes_one() {
        let mut s = bare(Pad::opened(0));
        step_ev(&mut s, &key_down(Key::Other, b'1' as u32, 0), None);
        step_ev(&mut s, &key_down(Key::Other, b'2' as u32, 0), None);
        assert_eq!(s.pad.entry, "12");
        assert!(!s.pad.submitting, "only three digits so far");
        step_ev(&mut s, &key_down(Key::Other, 0, 55 /* '7' by wcode */), None);
        assert_eq!(s.pad.entry, "127");
        // backspace via the delete cell's OWN digit path is not reachable through `digit_of` (it
        // is not in 48..=57) — `press` handles it directly, exercised below through `Activate`.
    }

    /// Backspace and typing-again both cancel a running wrong-PIN flash — `press`'s own first
    /// line, ported from `ui/profiles.rs`.
    #[test]
    fn typing_again_cancels_a_running_wrong_pin_flash() {
        let mut s = bare(Pad { open: true, error_s: PIN_ERR_S, entry: "1".into(), ..Pad::new() });
        step_ev(&mut s, &key_down(Key::Other, b'2' as u32, 0), None);
        assert_eq!(s.pad.error_s, 0.0);
    }

    /// While a PIN is submitting, only BACK acts — every digit is swallowed with no effect on the
    /// entry, matching `ui/profiles.rs::pad_key`'s own early return.
    #[test]
    fn only_back_acts_while_a_pin_is_submitting() {
        let mut s = bare(Pad { open: true, submitting: true, entry: "123".into(), ..Pad::new() });
        let (handled, _) = step_ev(&mut s, &key_down(Key::Other, b'4' as u32, 0), None);
        assert_eq!(handled, Handled::Yes, "swallowed, not ignored — nothing behind the pad may act on it");
        assert_eq!(s.pad.entry, "123", "the digit changed nothing");
        assert!(s.pad.submitting);
    }

    /// The keypad's Bare `Activate` — OK on a focused cell, or its pointer twin — presses that
    /// exact cell regardless of which key produced the event, exercised through the digit '5' at
    /// (1, 1) and the backspace at (3, 2).
    #[test]
    fn activating_a_pad_cell_presses_that_cell() {
        let mut s = bare(Pad::opened(0));
        step_ev(&mut s, &ScreenEvent::Activate(pad_elem(1, 1)), None); // '5'
        assert_eq!(s.pad.entry, "5");
        step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 2)), None); // delete
        assert_eq!(s.pad.entry, "");
        // the hole itself has no key behind it — activating it (which nothing on screen can ever
        // focus, since `pad_place`/`groups` never offer it) must still be a harmless no-op
        step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 0)), None);
        assert_eq!(s.pad.entry, "");
    }

    // ---------------------------------------------------------------------------------------
    // group topology — the pad traps focus; an empty roster still offers the footer
    // ---------------------------------------------------------------------------------------

    /// While the pad is open, `groups()` answers with ONE group and nothing else — the mechanism
    /// that traps focus inside it (`ui/CLAUDE.md`'s "the pad is modal over the picker" doc): the
    /// engine's geometric search has no other group to find in any direction.
    #[test]
    fn the_pad_is_the_only_group_while_it_is_open() {
        let s = bare(Pad::opened(0));
        let c = cx(None);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&s, &c, &mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, PAD_GROUP);
        assert_eq!(groups[0].elem, ElemKind::Bare);
        assert_eq!(groups[0].len, PAD_ROWS * PAD_COLS);
        assert!(matches!(groups[0].kind, GroupKind::Grid { cols: PAD_COLS, .. }));
    }

    /// With the pad closed and (as every host test's process necessarily has it, per
    /// `ui/profiles.rs`'s own "an unseeded roster is the case under test") no live roster, the
    /// footer is the ONLY group — "reachable even while the roster is empty/loading" — and it is
    /// reachable with no source key at all (the engine's own "no focus yet" fallback,
    /// `ui/focus.rs`'s `move_dir`).
    #[test]
    fn the_sign_out_pill_is_the_only_group_with_no_roster_on_screen() {
        // `auth::users()` reads the process-global controller — unguarded, this races every other
        // test in the binary that seeds or clears the roster (`[[test-suite-global-pollution]]`),
        // exactly as `back_closes_the_pad_and_clears_the_verdict` and
        // `a_pointer_click_outside_every_pad_stop_closes_it_too` already hold this lock for their
        // own reads of the shared PIN-denied flag.
        let _s = crate::testlock::serial();
        assert!(auth::users().is_empty(), "an unseeded roster is the case under test");
        let s = bare(Pad::new());
        let c = cx(None);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&s, &c, &mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, FOOTER_GROUP);
        assert_eq!(groups[0].elem, ElemKind::Control);
        assert_eq!(Focusable::<InnerHost>::group_of(&s, &FOOTER, &c), Some(FOOTER_GROUP));
        let from = Placed { rect: groups[0].extent, rest_rect: groups[0].extent, clip: Rect::FULL, index: None };
        assert_eq!(Focusable::<InnerHost>::seat(&s, FOOTER_GROUP, from, &c).elem, FOOTER);
    }

    /// `reconcile` never strands focus off the footer when the roster is empty, and clamps a
    /// stale roster index into range rather than losing it entirely otherwise.
    #[test]
    fn reconcile_falls_back_to_the_footer_with_an_empty_roster() {
        let _s = crate::testlock::serial(); // `auth::users()` is process-global — see the sibling test above
        let s = bare(Pad::new());
        let c = cx(None);
        let stray = FocusKey { entry: EntryId(0), elem: 3 };
        assert_eq!(Focusable::<InnerHost>::reconcile(&s, stray, &c).elem, FOOTER);

        // A key already ON the footer must reconcile to ITSELF — a genuinely different branch
        // (`if want.elem == FOOTER { return want; }`) from the empty-roster fallback just above,
        // even though with an EMPTY roster both branches answer `elem == FOOTER` and so are
        // indistinguishable by that field alone: deleting the identity branch entirely would still
        // pass an assertion that only checks `.elem`, because the `n == 0` fallback also lands on
        // FOOTER. A FOREIGN `entry` id tells the two apart — the identity branch returns `want`
        // completely untouched, while the fallback builds a fresh key off `self.entry` and would
        // silently overwrite it.
        let foreign_entry = EntryId(99);
        let on_footer = FocusKey { entry: foreign_entry, elem: FOOTER };
        let got = Focusable::<InnerHost>::reconcile(&s, on_footer, &c);
        assert_eq!(got.elem, FOOTER, "the footer reconciles to itself");
        assert_eq!(
            got.entry, foreign_entry,
            "…as an IDENTITY (the same key handed back), not a fresh one built off this screen's own entry"
        );
    }

    /// The half of the legacy `ui/profiles.rs`'s
    /// `the_pill_answers_every_key_while_it_holds_focus` that had no analog on this screen, restored
    /// when that module was deleted in phase 6.
    ///
    /// Its ◀/▶/▲/▼ half is gone BY DESIGN and is not restored here: stepping the roster and
    /// crossing to the footer are the shared focus engine's job now, and `ui/focus.rs`'s own suite
    /// grades them. What was this screen's own, and only ever pinned there, is what OK MEANS.
    #[test]
    fn ok_means_one_thing_on_the_pill_and_another_on_a_tile() {
        const N: usize = 3;

        // the two live answers, and they are not interchangeable
        assert_eq!(commit_action(false, Some(FOOTER), N), Commit::SignOut);
        assert_eq!(commit_action(false, Some(0), N), Commit::Select(0));
        assert_eq!(commit_action(false, Some(2), N), Commit::Select(2));

        // a tile index past the end of the roster commits NOTHING rather than clamping onto the
        // last one — the roster shrinks under this screen whenever a share is revoked
        assert_eq!(commit_action(false, Some(3), N), Commit::Nothing);
        assert_eq!(commit_action(false, Some(u32::MAX - 1), N), Commit::Nothing);

        // with no roster on screen the bottom arm has nothing to offer, but the pill still acts —
        // which is the whole reason the footer is reachable while the roster is still loading
        assert_eq!(commit_action(false, Some(0), 0), Commit::Nothing);
        assert_eq!(commit_action(false, Some(FOOTER), 0), Commit::SignOut);

        // nothing focused yet: a press cannot have attached to an element that does not exist
        assert_eq!(commit_action(false, None, N), Commit::Nothing);

        // and the pad swallows OK entirely. Its keys are Bare, so a commit arriving while it is
        // open was armed on the picker underneath — firing it would switch profile out from under
        // an open PIN prompt.
        assert_eq!(commit_action(true, Some(FOOTER), N), Commit::Nothing);
        assert_eq!(commit_action(true, Some(1), N), Commit::Nothing);
        assert_eq!(commit_action(true, None, N), Commit::Nothing);
    }
}
