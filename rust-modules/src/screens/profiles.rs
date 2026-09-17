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
//! following a fresh picker onto the screen.** Every door out of the pad emits typed
//! [`auth::SessionCmd::DismissPinError`] through [`ProfilesScreen::close_pad`]; the mounter emits
//! the same command beside a fresh constructor. A rejected PIN is the one exception that keeps the
//! pad up (the dot row flashes red and the entry restarts) rather than closing it.
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
use std::sync::Arc;

use crate::auth::{self, Phase};
use crate::ui::card_row;
use crate::ui::frame::Budget;
use crate::ui::geom;
use crate::ui::icons;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled, InputEvent, InputKind, Key,
    LogicalState, Machine, Measure, Tick,
};
use crate::ui::present::Provenance;
use crate::ui::route_screen::RouteGround;
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, FocusTarget,
    Focusable, GroupKind, GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent,
    Seat, Step, Stop,
};
use crate::ui::widgets::{self, Art, Button, CtlPop, Spinner};
use crate::ui::{consts::SCR_H, consts::SCR_W, theme, Env, Painter, Rect, View};

use super::registry::{word, AppFx, AppLike, AppMsg, AuthLike};

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
    Rect::new(
        SCR_W as f32 * 0.5 - PAD_GRID_W * 0.5,
        grid_y,
        PAD_GRID_W,
        PAD_GRID_H,
    )
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
                    return Step::Move(FocusKey {
                        entry,
                        elem: pad_elem(r, cc as usize),
                    });
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
            Step::Move(FocusKey {
                entry,
                elem: pad_elem(rr, pad_nearest_col(rr, c)),
            })
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
fn pad_seat(
    measure: &dyn Measure,
    entry: EntryId,
    from: Placed,
) -> crate::ui::machine::FocusKey<u32> {
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
    FocusKey {
        entry,
        elem: pad_elem(r, c),
    }
}

fn pad_reconcile(
    entry: EntryId,
    want: crate::ui::machine::FocusKey<u32>,
) -> crate::ui::machine::FocusKey<u32> {
    use crate::ui::machine::FocusKey;
    if let Some((r, c)) = pad_rc(want.elem) {
        if KEYS[r][c].is_some() {
            return want;
        }
    }
    FocusKey {
        entry,
        elem: pad_elem(0, 0),
    }
}

fn pad_group_spec(measure: &dyn Measure) -> GroupSpec {
    GroupSpec {
        id: PAD_GROUP,
        kind: GroupKind::Grid {
            cols: PAD_COLS,
            holes: PAD_HOLES,
        },
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
        edge: [
            EdgeRule::Geometric,
            EdgeRule::Stop,
            EdgeRule::Stop,
            EdgeRule::Stop,
        ],
        extent: footer_rect(measure),
        len: 1,
        elem: ElemKind::Control,
    }
}

fn phase_disc(phase: Phase) -> u8 {
    match phase {
        Phase::Idle => 0,
        Phase::Creating => 1,
        Phase::Waiting => 2,
        Phase::Discovering => 3,
        Phase::Profiles => 4,
        Phase::Switching => 5,
        Phase::Ready => 6,
        Phase::Error => 7,
        Phase::Deleted => 8,
    }
}

/// The screen's own `LogicalState` (§5.4): local pad/request state plus the retained Session facts
/// that reduce an accepted selection ACK. Two instances with the same pending epoch but different
/// cached publications can take different next transitions, so flow epoch, phase and denial are
/// part of the canon. Ordered protection flags also determine whether an avatar opens a pad or
/// emits a selection. Canon retains the same roster Arc and encodes only those flags, without
/// copying the roster on Tick. The PIN's LENGTH is included, **never its digits**, and the human probe prints
/// no request correlation or flow epoch.
struct ProfilesState {
    roster_n: u32,
    roster: Arc<[auth::UserTile]>,
    flow_epoch: u64,
    phase: u8,
    pin_denied: bool,
    pad_open: bool,
    pad_target: u32,
    pad_len: u32,
    pad_submitting: bool,
    pad_flashing: bool,
    next_correlation: Option<u32>,
    selection_correlation: Option<u32>,
    selection_epoch: Option<u64>,
}
impl LogicalState for ProfilesState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.roster_n)
            .u64(self.flow_epoch)
            .u8(self.phase)
            .bool(self.pin_denied)
            .bool(self.pad_open)
            .u32(self.pad_target)
            .u32(self.pad_len)
            .bool(self.pad_submitting)
            .bool(self.pad_flashing)
            .option(self.next_correlation, |w, correlation| {
                w.u32(correlation);
            })
            .option(self.selection_correlation, |w, correlation| {
                w.u32(correlation);
            })
            .option(self.selection_epoch, |w, epoch| {
                w.u64(epoch);
            });
        w.seq(self.roster.len());
        for user in self.roster.iter() {
            w.bool(user.protected);
        }
    }
    fn probe(&self, out: &mut String) {
        // the PIN's length, never its digits
        out.push_str(&format!(
            "profiles n={} phase={} denied={} pad_open={} target={} pin_len={} submitting={} flashing={} correlation_live={} selection_pending={} selection_accepted={}",
            self.roster_n,
            self.phase,
            self.pin_denied,
            self.pad_open,
            self.pad_target,
            self.pad_len,
            self.pad_submitting,
            self.pad_flashing,
            self.next_correlation.is_some(),
            self.selection_correlation.is_some(),
            self.selection_epoch.is_some(),
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PendingSelection {
    AwaitingAck {
        correlation: u32,
        pad: bool,
    },
    Accepted {
        correlation: u32,
        pad: bool,
        flow_epoch: u64,
    },
}

impl PendingSelection {
    fn correlation(self) -> u32 {
        match self {
            Self::AwaitingAck { correlation, .. } | Self::Accepted { correlation, .. } => {
                correlation
            }
        }
    }

    fn accepted_epoch(self) -> Option<u64> {
        match self {
            Self::AwaitingAck { .. } => None,
            Self::Accepted { flow_epoch, .. } => Some(flow_epoch),
        }
    }

    fn pad(self) -> bool {
        match self {
            Self::AwaitingAck { pad, .. } | Self::Accepted { pad, .. } => pad,
        }
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
    /// switch in flight), in ms — cached each tick from [`spin_phase`](Self::spin_phase)'s
    /// `advance`. Render-only, never hashed.
    spin_ms: f32,
    /// The underlying clock for [`spin_ms`](Self::spin_ms) (`motion::Phase`, phase 12 D4): reports
    /// `Motion` from inside its own `advance` rather than the raw `+= dt` this used to be, with
    /// `fx.note(Motion)` a separate, easy-to-forget line below it.
    spin_phase: crate::ui::motion::Phase,
    ground: RouteGround,
    pad: Pad,
    users: Arc<[auth::UserTile]>,
    phase: Phase,
    error: Arc<str>,
    pin_denied: bool,
    flow_epoch: u64,
    next_correlation: Option<u32>,
    pending_selection: Option<PendingSelection>,
    state: ProfilesState,
}

impl ProfilesScreen {
    /// A fresh picker. **This constructor IS `ui/profiles.rs`'s old `enter()`**: the container
    /// mounts a brand new instance every time it roots at `AppArg::Profiles` (`app/bridge.rs`'s
    /// `AppMounter::mount`, `app::input::enter_profiles_from_onboard`'s doc has the general
    /// argument for why that replaces a reset call), so there is no stale pad, no stale roster
    /// cursor and no leftover footer focus to clear by hand — only the one piece of state that
    /// lives OUTSIDE this screen and would otherwise survive a remount: the mounter carries a
    /// typed `DismissPinError` beside construction, while this instance starts with no local PIN
    /// verdict so its first paint does not wait for that command to drain.
    pub(crate) fn new(entry: EntryId, auth: auth::SessionRead<'_>) -> Self {
        let snapshot = auth.0;
        let mut s = Self {
            entry,
            row: card_row::CardRow::new(),
            row_sty: card_row::RowStyle::PROFILES,
            footer_pop: CtlPop::new(),
            spin_ms: 0.0,
            spin_phase: crate::ui::motion::Phase::default(),
            ground: RouteGround::new(),
            pad: Pad::new(),
            users: Arc::clone(&snapshot.users),
            phase: snapshot.phase,
            error: Arc::clone(&snapshot.error),
            // The mounter carries `DismissPinError` beside construction. The local first paint
            // must already be fresh while that command is still in the drain.
            pin_denied: false,
            flow_epoch: snapshot.flow_epoch,
            next_correlation: Some(1),
            pending_selection: None,
            state: ProfilesState {
                roster_n: 0,
                roster: Arc::clone(&snapshot.users),
                flow_epoch: snapshot.flow_epoch,
                phase: phase_disc(snapshot.phase),
                pin_denied: false,
                pad_open: false,
                pad_target: 0,
                pad_len: 0,
                pad_submitting: false,
                pad_flashing: false,
                next_correlation: Some(1),
                selection_correlation: None,
                selection_epoch: None,
            },
        };
        s.ground.reset();
        let n = s.users.len();
        s.refresh_row_sty(n);
        s.state = s.snapshot_state(n);
        s
    }

    fn refresh_row_sty(&mut self, n: usize) {
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
        (n == 0 && !self.pad.open)
            || (self.pad.open && self.pad.submitting)
            || self.phase == Phase::Switching
    }

    fn snapshot_state(&self, n: usize) -> ProfilesState {
        let selection_correlation = self.pending_selection.map(PendingSelection::correlation);
        let selection_epoch = self
            .pending_selection
            .and_then(PendingSelection::accepted_epoch);
        ProfilesState {
            roster_n: n as u32,
            roster: Arc::clone(&self.users),
            flow_epoch: self.flow_epoch,
            phase: phase_disc(self.phase),
            pin_denied: self.pin_denied,
            pad_open: self.pad.open,
            pad_target: self.pad.target as u32,
            pad_len: self.pad.entry.len() as u32,
            pad_submitting: self.pad.submitting,
            pad_flashing: self.pad.error_s > 0.0,
            next_correlation: self.next_correlation,
            selection_correlation,
            selection_epoch,
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

    fn resync(&mut self, auth: auth::SessionRead<'_>) {
        let snapshot = auth.0;
        self.users = Arc::clone(&snapshot.users);
        self.phase = snapshot.phase;
        self.error = Arc::clone(&snapshot.error);
        self.pin_denied = snapshot.pin_denied;
        self.flow_epoch = snapshot.flow_epoch;
    }

    fn tick<H: AuthLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let dt = t.dt();
        self.resync(H::auth(cx));
        self.reconcile_selection(fx);
        let n = self.users.len();
        self.refresh_row_sty(n);

        let cur = cx.focus.current.map(|k| k.elem);
        let footer_focused = !self.pad.open && cur == Some(FOOTER);
        // closed while the PIN pad is up, which is also when the control is not drawn at all
        self.footer_pop.step(footer_focused.then_some(0), dt);

        if self.pad.open {
            Self::step_pin_flash(&mut self.pad, dt, fx);
        }

        let roster_focus = if !self.pad.open && !footer_focused {
            cur.filter(|&e| (e as usize) < n).map(|e| e as usize)
        } else {
            None
        };
        self.row.update(n, roster_focus, &self.row_sty, dt);

        if self.has_spinner(n) {
            self.spin_ms = self.spin_phase.advance(t, &mut fx.present());
        }

        self.state = self.snapshot_state(n);
    }

    /// Spend a keypad digit/backspace. Ported from `ui/profiles.rs::press`: typing again cancels a
    /// wrong-PIN flash, a full PIN emits a typed Session command and the pad STAYS UP with a
    /// spinner in place of the dots while its result is pending.
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
            self.request_selection(idx, Some(pin), true, fx);
        }
        self.sync_pin_state();
    }

    /// Commit a roster tile (an avatar's `PressCommit`, or the dev `pickuser` trigger by way of
    /// `app/run.rs`'s own profile-selection command — that path bypasses this screen entirely
    /// and only ever names an UNPROTECTED index; see this lane's report for the open problem a
    /// protected one leaves): protected → open the PIN pad and re-seat focus onto it; else hand
    /// straight to Session's switch worker.
    fn select<H: AppLike>(&mut self, idx: usize, fx: &mut Effects<'_, H>) {
        if self.pending_selection.is_some_and(PendingSelection::pad) {
            return;
        }
        let protected = self.users.get(idx).map(|u| u.protected).unwrap_or(false);
        if protected {
            // Opening another avatar's pad replaces the local unprotected choice immediately.
            // Its late ACK must not settle this fresh pad; Session remains the flow owner.
            self.pending_selection = None;
            self.open_pad(idx, fx);
        } else {
            self.request_selection(idx, None, false, fx);
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
                    entry: self.entry,
                    elem: pad_elem(0, 0),
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
    /// longer on screen. Closing clears this instance's pending correlation, so a late ACK cannot
    /// settle a reopened pad. It does not cancel Session's accepted flow; a later publication may
    /// still route the app away, but without a matching local pending epoch its denial cannot be
    /// assigned to another pad.
    fn close_pad<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        self.retire_pad(true, fx);
    }

    fn retire_pad<H: AppLike>(&mut self, dismiss_pin_error: bool, fx: &mut Effects<'_, H>) {
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
        self.pending_selection = None;
        self.sync_pin_state();
        self.pin_denied = false;
        if dismiss_pin_error {
            fx.push(Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError)));
        }
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(crate::ui::machine::FocusKey {
                    entry: self.entry,
                    elem: target,
                }),
            })),
        ));
        fx.invalidate(Provenance::Input);
    }

    fn request_selection<H: AppLike>(
        &mut self,
        index: usize,
        pin: Option<String>,
        pad: bool,
        fx: &mut Effects<'_, H>,
    ) -> bool {
        if self.pending_selection.is_some_and(PendingSelection::pad) {
            return false;
        }
        // Allocate before replacing an unprotected request: exhaustion must leave the original
        // choice and its ACK correlation intact, both before and after acceptance.
        let Some(reply) = self.allocate_reply(fx) else {
            self.sync_pin_state();
            return false;
        };
        self.pending_selection = Some(PendingSelection::AwaitingAck {
            correlation: reply.correlation,
            pad,
        });
        if pad {
            self.pad.submitting = true;
        }
        fx.push(Fx::App(AppFx::Session(
            auth::SessionCmd::SelectProfileWithReply { index, pin, reply },
        )));
        self.sync_pin_state();
        true
    }

    fn selection_reply<H: AppLike>(
        &mut self,
        request: u32,
        correlation: u32,
        accepted: bool,
        flow_epoch: u64,
        fx: &mut Effects<'_, H>,
    ) {
        if request != correlation {
            return;
        }
        let Some(PendingSelection::AwaitingAck {
            correlation: pending,
            pad,
        }) = self.pending_selection
        else {
            return;
        };
        if pending != correlation {
            return;
        }
        if !accepted {
            self.pending_selection = None;
            if pad && self.pad.open {
                self.pad.submitting = false;
                self.pad.entry.clear();
            }
            self.sync_pin_state();
            fx.invalidate(Provenance::Input);
            return;
        }
        self.pending_selection = Some(PendingSelection::Accepted {
            correlation,
            pad,
            flow_epoch,
        });
        self.reconcile_selection(fx);
        self.sync_pin_state();
    }

    fn reconcile_selection<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(PendingSelection::Accepted { flow_epoch, .. }) = self.pending_selection else {
            return;
        };
        if self.flow_epoch < flow_epoch {
            return;
        }
        let pad = self.pending_selection.is_some_and(PendingSelection::pad);
        if self.flow_epoch > flow_epoch {
            self.pending_selection = None;
            if pad && self.pad.open {
                self.retire_pad(false, fx);
            } else {
                self.sync_pin_state();
            }
            return;
        }
        match self.phase {
            Phase::Switching => {}
            Phase::Profiles => {
                self.pending_selection = None;
                if pad && self.pad.open {
                    if self.pin_denied {
                        self.pad.submitting = false;
                        self.pad.entry.clear();
                        self.pad.error_s = PIN_ERR_S;
                        self.sync_pin_state();
                        fx.invalidate(Provenance::Input);
                    } else {
                        self.retire_pad(true, fx);
                    }
                } else {
                    self.sync_pin_state();
                }
            }
            Phase::Ready => {
                self.pending_selection = None;
                if pad && self.pad.open {
                    self.retire_pad(false, fx);
                } else {
                    self.sync_pin_state();
                }
            }
            _ => {}
        }
    }

    fn allocate_reply<H: AppLike>(&mut self, fx: &Effects<'_, H>) -> Option<auth::owner::ReplyTo> {
        let crate::ui::machine::MachineId::Instance(instance) = fx.from() else {
            return None;
        };
        let correlation = self.next_correlation?;
        let next = correlation.checked_add(1)?;
        self.next_correlation = Some(next);
        Some(auth::owner::ReplyTo {
            instance: instance.0,
            correlation,
        })
    }

    fn request_root_back<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(reply) = self.allocate_reply(fx) else {
            self.sync_pin_state();
            return;
        };
        fx.push(Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot {
            reply,
        })));
        self.sync_pin_state();
    }

    fn draw_name(
        p: Painter,
        u: &auth::UserTile,
        cx: f32,
        focused: bool,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        let col = if focused {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        let name = crate::text::elide_by(
            &u.title,
            card_row::RowStyle::PROFILES.w + card_row::RowStyle::PROFILES.gap - 12.0,
            false,
            |t| measure.width_str(t, theme::size::LABEL, focused),
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
        let name = self
            .users
            .get(self.pad.target)
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
                        Rect::new(
                            rect.x + (rect.w - d) * 0.5,
                            rect.y + (rect.h - d) * 0.5,
                            d,
                            d,
                        ),
                        ink,
                    );
                } else if let Ok(lc) = CString::new((*k as char).to_string()) {
                    let ty =
                        crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
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
                f.stop(
                    p,
                    Stop {
                        key: crate::ui::machine::FocusKey {
                            entry: self.entry,
                            elem: pad_elem(r, c),
                        },
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
    fn deref(&self) -> &Self::Target {
        self.screen
    }
}

impl ProfilesScreen {
    fn focus_view(&self) -> ProfilesView<'_> {
        ProfilesView {
            screen: self,
            n: self.users.len(),
        }
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
    fn neighbour(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        cx: &Cx<'_, H>,
    ) -> Step<u32> {
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
            return Some(Placed {
                rect: r,
                rest_rect: r,
                clip: Rect::FULL,
                index: Some(0),
            });
        }
        let n = self.n;
        Focusable::<H>::place(&self.shelf(n), key, cx, at)
    }
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        if self.pad.open {
            return pad_reconcile(self.entry, want);
        }
        if want.elem == FOOTER {
            return want;
        }
        let n = self.n;
        if n == 0 {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: FOOTER,
            };
        }
        if (want.elem as usize) >= n {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: (n - 1) as u32,
            };
        }
        want
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        if self.pad.open {
            return pad_seat(cx.measure, self.entry, from);
        }
        if g == FOOTER_GROUP {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: FOOTER,
            };
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
    fn neighbour(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        cx: &Cx<'_, H>,
    ) -> Step<u32> {
        Focusable::<H>::neighbour(&self.focus_view(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        Focusable::<H>::place(&self.focus_view(), key, cx, at)
    }
    fn reconcile(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        Focusable::<H>::reconcile(&self.focus_view(), key, cx)
    }
    fn seat(
        &self,
        group: GroupId,
        from: Placed,
        cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        Focusable::<H>::seat(&self.focus_view(), group, from, cx)
    }
}

impl<H: AuthLike> Machine<H> for ProfilesScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx, fx);
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
                // The DECISION is [`commit_action`], a pure function; each live arm emits a typed
                // Session command, so the OK table remains gradeable without real account work.
                //
                // The retained roster count is read only on the branch that can need it.
                let elem = cx.focus.current.map(|k| k.elem);
                let roster_n = match elem {
                    Some(e) if !self.pad.open && e != FOOTER => self.users.len(),
                    _ => 0,
                };
                match commit_action(self.pad.open, elem, roster_n) {
                    Commit::SignOut => {
                        fx.push(Fx::App(AppFx::Session(auth::SessionCmd::SignOut)));
                    }
                    Commit::Select(i) => self.select(i, fx),
                    Commit::Nothing => {}
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            // The pad's own door for a pointer click that lands on NONE of its twelve stops — see
            // the module doc for why this has to be built here rather than reached for a library
            // `OnMiss` policy (the pad is not a container surface).
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Click { hit, .. },
                ..
            }) if self.pad.open => {
                if hit.is_none() {
                    self.close_pad(fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key,
                        sym,
                        wcode,
                        edge: Edge::Down,
                        ..
                    },
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
                // At root, Session/core owns resume, cooldown and platform handling. This screen
                // contributes only the addressed typed request.
                if *key == Key::Back {
                    self.request_root_back(fx);
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::Async(
                crate::ui::machine::RequestId(request),
                AppMsg::BackReply {
                    correlation,
                    resumed: _resumed,
                },
            ) if request == correlation => Handled::Yes,
            ScreenEvent::Async(
                crate::ui::machine::RequestId(request),
                AppMsg::SelectionReply {
                    correlation,
                    accepted,
                    flow_epoch,
                },
            ) => {
                self.selection_reply(*request, *correlation, *accepted, *flow_epoch, fx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: AuthLike> Screen<H> for ProfilesScreen {
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

        let users = &self.users;
        let n = users.len();
        let (start_x, slot) = row_geom(n);
        let scroll = self.row.scroll_x();
        let footer_focused = cur == Some(FOOTER);
        let roster_focus = (!footer_focused)
            .then(|| cur.filter(|&e| (e as usize) < n).map(|e| e as usize))
            .flatten();
        let extent = Rect::new(0.0, ROW_Y, SCR_W as f32, self.row_sty.h);

        let mut focused_i = None;
        for (i, u) in users.iter().enumerate() {
            let cx_ = start_x + i as f32 * slot + self.row_sty.w * 0.5 - scroll;
            let base = Rect::new(
                cx_ - self.row_sty.w * 0.5,
                ROW_Y,
                self.row_sty.w,
                self.row_sty.h,
            );
            let sc = self.row.scale(i);
            if roster_focus == Some(i) {
                focused_i = Some(i);
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
                &self.row_sty,
                None,
            );
            Self::draw_name(p, u, cx_, false, f.measure);
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: i as u32,
                    },
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
            let base = Rect::new(
                cx_ - self.row_sty.w * 0.5,
                ROW_Y,
                self.row_sty.w,
                self.row_sty.h,
            );
            // fold the ui::press click dip into the focused avatar's pop (1.0 when idle)
            let sc = self.row.scale(i) * crate::ui::press::scale();
            Self::draw_name(p, u, cx_, true, f.measure);
            card_row::draw_focused(
                p,
                Art::Thumb {
                    sid: crate::plex::current_server(),
                    key: &u.thumb,
                    res: (300, 300),
                },
                base.scaled(sc),
                sc,
                &self.row_sty,
                None,
                &card_row::TileLabel::default(),
                f.measure,
            );
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: i as u32,
                    },
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
                key: crate::ui::machine::FocusKey {
                    entry: self.entry,
                    elem: FOOTER,
                },
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
        if !self.error.is_empty() && self.phase == Phase::Profiles {
            if let Ok(e) = CString::new(self.error.as_ref()) {
                let ey = crate::text::text_vcenter_y(theme::size::BODY, 0, ERROR_Y);
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

        if self.phase == Phase::Switching {
            p.rect(
                Rect::FULL,
                0.0,
                theme::scrim_black(0.88),
                theme::scrim_black(0.88),
                0.0,
            );
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
#[path = "profiles_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "profiles_selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "profiles_session_tests.rs"]
mod session_tests;

#[cfg(test)]
#[path = "profiles_pin_tests.rs"]
mod pin_tests;

#[cfg(test)]
#[path = "profiles_pad_geometry_tests.rs"]
mod pad_geometry_tests;
