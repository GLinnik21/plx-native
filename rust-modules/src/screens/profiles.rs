//! "Who's watching" — the Plex Home profile picker, plus the PIN keypad for a protected profile —
//! as an OWNED `Screen` (restructure spec §13, phase 6). Ported from the legacy `ui/profiles.rs`
//! (kept, not deleted: `ui/CLAUDE.md`'s table still names it as the design record of what moved).
//!
//! **One struct, no globals.** The legacy screen was a `static mut Scene` plus a `static mut
//! FOOTER_POP`; both are fields of this instance now, owned by whichever `InstanceId` the
//! container minted for it, so two mounts (there is only ever one in practice, but nothing here
//! assumes that any more) cannot share state.
//!
//! **The avatar row and the Sign-out footer are two different focus GROUPS, and they arm the tvOS
//! press through two different `ElemKind`s** — `Card` for an avatar (it dips like a poster;
//! `card_row`'s own focus spring plus the folded `press::scale()` are what already make a poster
//! tile pop, and an avatar tile is drawn through the exact same `card_row::draw_focused` call a
//! poster is), `Control` for the footer (a `CtlPop<1>`, exactly the shared control-face treatment
//! `ui/CLAUDE.md`'s `widgets.rs` row describes). The PIN keypad's digits are neither: they are
//! `Bare` and commit on the key-DOWN with no press to show, because a phone dial pad's keys do not
//! dip — see [`KEYS`] and [`ProfilesView`] below.
//!
//! **The PIN pad TRAPS focus by controlling which groups exist, not by a nested modal surface.**
//! While it is open, [`ProfilesView::groups`] reports the grid ALONE — the same mechanism
//! `screens/consent.rs`'s "Delete all local data?" alert uses over its table — so a direction key
//! that would otherwise escape the grid has nowhere to land and the engine answers `Outcome::
//! Nothing`. Opening and closing the pad are both explicit re-seats of the engine's focus onto a
//! named element (never onto "the group" and its default corner), addressed through
//! [`crate::ui::machine::Effects::from`] rather than a stored `InstanceId` — this screen mounts
//! directly on the app's own outer stack, the same shape `OnboardScreen`'s first-run mounting
//! does, so the `RouteSurface::forward` trick `ConsentPage` uses (a literal `InstanceId(0)`,
//! rewritten by the surface that hosts it) is not available and would silently misaddress the
//! request here. **Getting the close-side re-seat right is not a nicety — it is a correctness
//! requirement.** Once the pad closes, [`ProfilesView::groups`] goes back to reporting the avatar
//! row and the footer, and if the engine's last-known focus key were left pointing at a PIN-cell
//! index (0..11) instead of being explicitly moved back onto the target avatar, that stale index
//! could resolve as a *different, real avatar* the moment the roster has that many tiles —
//! silently handing focus to the wrong profile. This is the mechanical form of the legacy
//! `close_pad`'s own warning ("no verdict about the last keypad follows a fresh picker onto the
//! screen"), carried into a model where a stale key is not just a stale READ-OUT but a stale
//! ADDRESS.
//!
//! **The keypad's hole does NOT use the engine's own "skip along the direction of travel" grid
//! algorithm, and that is deliberate, not a shortcut.** `ui/focus.rs`'s golden-table fixture (the
//! one place `GroupKind::Grid`'s hole-skipping is actually implemented, in its test-only `tree::
//! Tree`) walks FURTHER in the SAME axis when a step lands on a hole — correct for a hole
//! somewhere in the middle of a grid, and wrong here, because this pad's one hole sits at the
//! bottom-LEFT CORNER of the last row: continuing downward from '7' would immediately run off the
//! grid's bottom edge and refuse the move, where a real phone dial pad's '0' key sits one column
//! over on that same row and a DOWN press is supposed to reach it. The legacy screen's own
//! `nearest_col`/`step_focus` pair encoded the dial-pad convention instead — vertical moves snap
//! SIDEWAYS to the nearest real key in the row they land on, horizontal moves skip the hole within
//! their own row and hold at the edges — and this file ports that pair verbatim
//! ([`pad_step_col`], [`pad_nearest_col`]) rather than reaching for the generic algorithm. A
//! reviewer expecting "the engine already has grid-with-holes support" should read this paragraph
//! before assuming this file reinvents it by mistake.
//!
//! **BACK's root-press decision is NOT routed through `LoopReq::BackAtRoot`, even though
//! `registry.rs`'s doc for that variant lists "the picker" and "sign-in" among the roots it
//! covers.** It cannot be: `app/input.rs::back_at_root` (what draining `BackAtRoot` runs) claims
//! `webos::take_root_press()` and, only if that succeeds, calls `webos::go_home()` — but on this
//! screen the SAME claim has to be taken BEFORE the destructive `auth::cancel()` call, specifically
//! so a burst of BACK taps costs at most one `cancel()` (the legacy `app/input.rs::key_onboarding`
//! comment: "rate-limiting only the platform call would still let a burst of taps back out once
//! per press"). Emitting `LoopReq::BackAtRoot` for the refusal case would make the loop take a
//! SECOND, independent claim a few frames later — which fails immediately, because the first claim
//! is still held — so `go_home()` would silently never run. This screen therefore reproduces
//! `key_onboarding`'s Profiles arm directly ([`ProfilesScreen::back_at_root`]), calling `webos::`
//! and `auth::cancel` itself rather than asking the loop to. See this lane's report for the same
//! note addressed to whoever builds the Login screen, which faces an identical trap.
#![allow(dead_code)] // phase 6: not yet wired into `app/bridge.rs`'s mount match (a sibling lane's job)

use std::borrow::Cow;
use std::ffi::CString;

use crate::auth::{self, Phase};
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::icons::{self, Icon};
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent,
    InputKind, Key, LogicalState, Machine, Measure,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::route_screen::RouteGround;
use crate::ui::screen::{
    At, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable, GroupKind, GroupSpec,
    Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
};
use crate::ui::widgets::{Art, Button, CtlPop, Spinner};
use crate::ui::{press, theme, Env, Painter, Rect, View};

use super::registry::AppLike;

/// The screen's own heading — unchanged from `ui::profiles::TITLE`, and deliberately read from
/// there rather than re-typed here: `screens::onboard`'s `CRUMB_PROFILES` already names that
/// constant as the single source of what BACK from the first-run Favourites screen calls this
/// screen, and duplicating the literal would let the two drift the day either one is edited.
pub(crate) const TITLE: &str = crate::ui::profiles::TITLE;

const ROW_Y: f32 = 384.0;
const NAME_DY: f32 = RowStyle::PROFILES.h
    + RowStyle::PROFILES.h * (RowStyle::PROFILES.focus_scale - 1.0) * 0.5
    + theme::space::MD;
const PIN_LEN: usize = 4;
const FOOTER_Y: f32 = 780.0;
const FOOTER_H: f32 = 60.0;
const ERROR_Y: f32 = FOOTER_Y + FOOTER_H + theme::space::XL + theme::size::BODY as f32 * 0.5;

// PIN pad geometry — the label, dots and keypad are ONE centred unit (`ui/profiles.rs`'s own
// comment on this point survives unchanged: three independent hard-coded Ys left the block sitting
// low on the panel; this derives all three from one another instead).
const PAD_KEY: f32 = 108.0;
const PAD_KGAP: f32 = 20.0;
const PAD_ROWS: usize = 4;
const PAD_COLS: usize = 3;
const PAD_GRID_H: f32 = PAD_ROWS as f32 * PAD_KEY + (PAD_ROWS as f32 - 1.0) * PAD_KGAP;
const PAD_TITLE_GRID: f32 = 152.0;
const PAD_DOT: f32 = 18.0;
const PIN_ERR_S: f32 = 1.4;
const PIN_ERR_HALF_S: f32 = 0.175;

/// (title_y, dots_y, grid_y), the whole unit centred on the screen — ported verbatim from
/// `ui::profiles::pad_geom`. **Reaches the real loaded font** (`text::text_cap_band` measures a
/// capital "H" through SDL2_ttf), so nothing that calls this — directly or through
/// [`pad_extent`]/[`pad_key_rect`] — can be exercised by a host test; the same boundary the legacy
/// module drew around its own `pad_geom`/`pad_key_rect`/`pad_key_at`.
fn pad_geom() -> (f32, f32, f32) {
    let (ct, cb) = crate::text::text_cap_band(theme::size::TITLE, 1);
    let span = PAD_TITLE_GRID + PAD_GRID_H - ct;
    let title_y = (SCR_H - span) * 0.5 - ct;
    let grid_y = title_y + PAD_TITLE_GRID;
    let dots_y = (title_y + cb + grid_y) * 0.5 - PAD_DOT * 0.5;
    (title_y, dots_y, grid_y)
}

/// Keypad cell geometry, screen space — shared by [`draw_pad`] and the pad's `Focusable::place`,
/// so a click and the drawn key are the SAME rect (spec §7.1: geometry IS `place`).
fn pad_key_rect(r: usize, c: usize) -> Rect {
    let (_, _, grid_y) = pad_geom();
    let gx = SCR_W * 0.5 - (PAD_COLS as f32 * PAD_KEY + (PAD_COLS as f32 - 1.0) * PAD_KGAP) * 0.5;
    Rect::new(
        gx + c as f32 * (PAD_KEY + PAD_KGAP),
        grid_y + r as f32 * (PAD_KEY + PAD_KGAP),
        PAD_KEY,
        PAD_KEY,
    )
}

/// The grid's bounding box, for the pad's one `GroupSpec::extent` — never actually searched
/// geometrically into (the pad is the only group reported while it is open), but every
/// `GroupSpec` names one, and the number is meaningful for a future caller who unions extents.
fn pad_extent() -> Rect {
    pad_key_rect(0, 0).union(pad_key_rect(PAD_ROWS - 1, PAD_COLS - 1))
}

// keypad: 4 rows × 3 cols. b'D' = delete (bottom-right); None = the one empty, unfocusable cell.
const KEYS: [[Option<u8>; PAD_COLS]; PAD_ROWS] = [
    [Some(b'1'), Some(b'2'), Some(b'3')],
    [Some(b'4'), Some(b'5'), Some(b'6')],
    [Some(b'7'), Some(b'8'), Some(b'9')],
    [None, Some(b'0'), Some(b'D')],
];
/// `(row, col)` of the grid's one unfocusable cell — [`KEYS`]'s `None`, restated as data for the
/// `GroupSpec::kind`'s `holes` list. Pinned against `KEYS` itself by
/// [`tests::pad_holes_matches_the_keys_table`], so the two cannot drift silently.
const PAD_HOLES: &[(usize, usize)] = &[(3, 0)];

/// The element namespace while the pad is CLOSED: an avatar is its plain roster index (0..n, small
/// and never colliding with either sentinel below), the footer is [`FOOTER`]. While the pad is
/// OPEN the SAME numeric range (0..12) means a grid cell instead — safe because
/// [`ProfilesView::groups`] never reports both interpretations in the same frame, so nothing ever
/// asks "which does this number mean" without already knowing which mode it is in.
const FOOTER: u32 = u32::MAX;
/// The pad's background "click outside a key" target — [`PadClick::Dismiss`]'s new-model home.
/// Distinct from [`FOOTER`] only for readability; the two are never live at once either.
const PAD_DISMISS: u32 = u32::MAX - 1;

const AVATAR_GROUP: GroupId = GroupId(0);
const FOOTER_GROUP: GroupId = GroupId(1);
const PAD_GROUP: GroupId = GroupId(2);

/// Which half of the wrong-PIN flash cycle is showing — `None` once the flash has run out. Ported
/// verbatim from `ui::profiles::pin_flash`: phased off ELAPSED time so the opening frame is always
/// the lit half, whatever `PIN_ERR_S`/`PIN_ERR_HALF_S` later become.
fn pin_flash(error_s: f32) -> Option<bool> {
    (error_s > 0.0).then(|| (((PIN_ERR_S - error_s) / PIN_ERR_HALF_S) as i32) % 2 == 0)
}

/// A PIN digit, read from whichever field carries it — SDL gives a printable key its ASCII sym,
/// the remote's number buttons carry the same 48–57 range in `wcode`. Ported from `ui::profiles::
/// digit_of`; the field TYPES changed (the new input model carries `sym`/`wcode` as plain `u32`,
/// not the raw `c_uint` the SDL event byte-offsets used), the range check did not.
fn digit_of(sym: u32, wcode: u32) -> Option<u8> {
    [sym, wcode].into_iter().find(|v| (48..=57).contains(v)).map(|v| v as u8)
}

/// Avatar-row geometry: (first tile's left x before scroll, per-tile stride). Centred when the
/// roster fits, else left-aligned at the row's own margin so `CardRow` can scroll it — ported
/// verbatim from `ui::profiles::row_geom`. **This is why the avatar group is a hand-written
/// `Focusable` rather than [`crate::ui::geom::Shelf`]**: that library view always positions tile
/// `i` at `sty.margin_x + i*pitch`, with no per-frame origin of its own, because every OTHER
/// shelf in the app (Home, Library, Related) is meant to sit at a fixed left margin and scroll —
/// this is the one screen whose roster is short enough, and centred enough, to want something
/// `Shelf` was never asked to do. Reusing it by overwriting `RowStyle::PROFILES.margin_x` per
/// frame was considered and rejected: `CardRow::update`'s OWN scroll-into-view math reads
/// `sty.margin_x` too (as the viewport's side inset), so a caller-side override for POSITIONING
/// would silently also shrink the SCROLL viewport on every frame the roster is centred — two
/// unrelated computations sharing one field, which is exactly the kind of coupling a per-frame
/// override cannot see. Pure arithmetic, host-testable.
fn row_geom(n: usize) -> (f32, f32) {
    let sty = RowStyle::PROFILES;
    let slot = sty.w + sty.gap;
    let total = (n as f32 * slot - sty.gap).max(0.0);
    (((SCR_W - total) * 0.5).max(sty.margin_x), slot)
}

/// The centred "Sign out" pill under the roster — shared by [`draw`](ProfilesScreen::draw) and the
/// footer's `Focusable::place`/`groups`. Takes the `Measure` CAPABILITY (spec §4.3) rather than
/// `crate::text::text_width` directly, unlike the rest of this pad's geometry: this is what makes
/// the footer's focus/hit geometry — unlike the pad's — exercisable against `FixtureMeasure` on
/// the host, unlike the legacy screen's `footer_rect`, which called the free function and so could
/// only ever be exercised on a device or the simulator.
fn footer_rect(m: &dyn Measure) -> Rect {
    let tw = m.width(c"Sign out", theme::size::BODY, false);
    let w = tw + 76.0;
    Rect::new((SCR_W - w) * 0.5, FOOTER_Y, w, FOOTER_H)
}

/// One avatar's step within the roster — LEFT/RIGHT only, clamped at both ends (no wrap): the
/// generalisation `ui::geom::Shelf::neighbour` would give if it took a caller-supplied origin (see
/// [`row_geom`]'s doc for why this screen cannot just reuse that view). Pure, host-testable.
fn step_avatar(entry: EntryId, elem: u32, dir: Dir, n: usize) -> Step<u32> {
    match dir {
        Dir::Left if elem > 0 => Step::Move(FocusKey { entry, elem: elem - 1 }),
        Dir::Right if (elem as usize) + 1 < n => Step::Move(FocusKey { entry, elem: elem + 1 }),
        _ => Step::Edge,
    }
}

fn pad_pos(elem: u32) -> (i32, i32) {
    ((elem / PAD_COLS as u32) as i32, (elem % PAD_COLS as u32) as i32)
}
fn pad_elem(r: i32, c: i32) -> u32 {
    r as u32 * PAD_COLS as u32 + c as u32
}
fn pad_key(entry: EntryId, r: i32, c: i32) -> FocusKey<u32> {
    FocusKey { entry, elem: pad_elem(r, c) }
}
fn is_hole(r: i32, c: i32) -> bool {
    PAD_HOLES.contains(&(r as usize, c as usize))
}

/// Move the keypad column, skipping the one empty cell within THIS row; holds at the row's own
/// edges. Ported verbatim from `ui::profiles::step_focus`.
fn pad_step_col(fr: i32, fc: i32, dir: i32) -> Option<i32> {
    let mut c = fc + dir;
    while (0..PAD_COLS as i32).contains(&c) {
        if !is_hole(fr, c) {
            return Some(c);
        }
        c += dir;
    }
    None
}

/// Nearest occupied column in row `fr` to `fc` — for a vertical move landing on a row with the
/// hole in it. Ported verbatim from `ui::profiles::nearest_col`; see the module doc for why this
/// SIDEWAYS-SNAP rule, and not the engine's generic "keep going in the same direction" grid
/// algorithm, is what belongs here.
fn pad_nearest_col(fr: i32, fc: i32) -> i32 {
    if !is_hole(fr, fc) {
        return fc;
    }
    for d in 1..PAD_COLS as i32 {
        for c in [fc - d, fc + d] {
            if (0..PAD_COLS as i32).contains(&c) && !is_hole(fr, c) {
                return c;
            }
        }
    }
    0
}

/// The pad's own within-grid stepping. Ported from `ui::profiles::pad_key`'s four direction arms
/// (the digit/backspace/BACK arms live in [`ProfilesScreen::step`] instead, since those MUTATE —
/// this is the pure query the engine's `Focusable::neighbour` asks). UP/DOWN clamp at the grid's
/// own top/bottom row (matching the legacy screen's `.max(0)`/`.min(3)` clamps, which read as a
/// no-op hold at the edges rather than an escape) and re-snap the column with [`pad_nearest_col`];
/// LEFT/RIGHT walk within the row via [`pad_step_col`] and report `Step::Edge` — not a same-key
/// move — when nothing is found, which the engine's own `FocusEngine::set` already treats as no
/// motion at all.
fn pad_neighbour(entry: EntryId, elem: u32, dir: Dir) -> Step<u32> {
    let (fr, fc) = pad_pos(elem);
    match dir {
        Dir::Up if fr > 0 => Step::Move(pad_key(entry, fr - 1, pad_nearest_col(fr - 1, fc))),
        Dir::Down if fr < PAD_ROWS as i32 - 1 => {
            Step::Move(pad_key(entry, fr + 1, pad_nearest_col(fr + 1, fc)))
        }
        Dir::Left => match pad_step_col(fr, fc, -1) {
            Some(c) => Step::Move(pad_key(entry, fr, c)),
            None => Step::Edge,
        },
        Dir::Right => match pad_step_col(fr, fc, 1) {
            Some(c) => Step::Move(pad_key(entry, fr, c)),
            None => Step::Edge,
        },
        _ => Step::Edge,
    }
}

/// The PIN keypad overlay — open for a protected profile. A field of the screen (spec §6.1: no
/// `static mut`), not a nested container: it changes what [`ProfilesView::groups`] reports rather
/// than mounting a second `Screen`.
struct Pad {
    open: bool,
    /// Which roster index this pad is unlocking.
    target: usize,
    /// The digits typed so far. **Never written into [`ProfilesState`]'s hash or probe** — a PIN
    /// is a household credential, and `diag/scrub.rs`'s whole discipline is that a call site
    /// simply never writes the sensitive value rather than trusting a downstream redaction pass to
    /// catch it; a hash is not a redaction here either, since four digits is a ten-thousand-value
    /// space a stale recording's hash could be brute-forced against. Only the LENGTH is logical
    /// state (see [`ProfilesState`]).
    entry: String,
    /// Render mirror of the focused cell, set from `ScreenEvent::FocusMoved` — read by
    /// [`draw_pad`] to know which key to draw lit. Not part of [`ProfilesState`]: the engine's own
    /// focus key is already hashed by the dispatcher, and every other screen in this family
    /// (`OnboardState`, `ConsentState`) leaves its own focus mirrors out of its `LogicalState` too.
    fr: i32,
    fc: i32,
    /// A full PIN has been submitted and is being verified off-thread; the pad stays up with a
    /// spinner in place of the dot row.
    submitting: bool,
    /// Wrong-PIN flash: seconds still to run, stepped by `dt`. Pure RENDER/timer state, like
    /// `spin_ms` — see [`pin_flash`]'s doc for why the hash and the probe never see it.
    error_s: f32,
}
impl Pad {
    fn closed() -> Self {
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

/// The screen's hashed, restored, recorded state (spec §5.4). Deliberately narrow: everything
/// continuous (`row`'s springs, `footer_pop`, `spin_ms`, the pad's `error_s`) is RENDER state a
/// replay reconstructs identically from ticks and focus moves, not a branch point — the same line
/// every sibling screen in this family (`OnboardState`, `ConsentState`) already draws.
struct ProfilesState {
    pad_open: bool,
    /// Which roster index the open pad targets — `0` when the pad is closed, which is a real
    /// value (index 0) rather than "none": harmless, since nothing reads it while `pad_open` is
    /// false, and cheaper than an `Option` for a field this narrow.
    pad_target: u8,
    submitting: bool,
    /// The typed PIN's LENGTH only — see [`Pad::entry`]'s doc for why the digits themselves never
    /// reach here.
    entry_len: u8,
}
impl LogicalState for ProfilesState {
    fn write(&self, w: &mut Canon) {
        w.bool(self.pad_open)
            .u32(self.pad_target as u32)
            .bool(self.submitting)
            .u32(self.entry_len as u32);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "profiles pad_open={} pad_target={} submitting={} entry_len={}",
            self.pad_open, self.pad_target, self.submitting, self.entry_len
        ));
    }
}

pub(crate) struct ProfilesScreen {
    entry: EntryId,
    row: CardRow,
    footer_pop: CtlPop<1>,
    spin_ms: f32,
    ground: RouteGround,
    /// Render mirror of the focused avatar index, set from `FocusMoved`. Meaningless (and unread)
    /// while `footer` or `pad.open` is true — the same "focus is exclusive" rule the legacy
    /// screen's `update`/`draw` encoded by handing `CardRow::update` a bare `None` in those cases.
    fc: usize,
    /// Render mirror: the Sign-out control holds focus.
    footer: bool,
    pad: Pad,
    state: ProfilesState,
}

impl ProfilesScreen {
    pub(crate) fn new(entry: EntryId) -> Self {
        // Deliberately does NOT call `auth::dismiss_pin_error()` here — that global reset belongs
        // to [`Self::on_mount`], which the container's `Mount` → `Enter(Fresh{..})` pair always
        // delivers immediately after construction in production (`ui::screen::push_sequence`), so
        // nothing observable is lost by not duplicating it here. Keeping construction itself free
        // of a crate-wide side effect is what lets a pure geometry test build a `ProfilesScreen`
        // with `crate::testlock::serial()` NOT held, without racing a sibling test's own use of
        // `auth::set_pin_denied_for_test`/`pin_denied` — the trap a first draft of this file fell
        // into by calling the same reset from both places.
        Self {
            entry,
            row: CardRow::new(),
            footer_pop: CtlPop::new(),
            spin_ms: 0.0,
            ground: RouteGround::new(),
            fc: 0,
            footer: false,
            pad: Pad::closed(),
            state: ProfilesState {
                pad_open: false,
                pad_target: 0,
                submitting: false,
                entry_len: 0,
            },
        }
    }

    fn sync_state(&mut self) {
        self.state = ProfilesState {
            pad_open: self.pad.open,
            pad_target: self.pad.target as u8,
            submitting: self.pad.submitting,
            entry_len: self.pad.entry.len() as u8,
        };
    }

    /// Initialize a newly mounted screen. Focus `Enter` events are also used for local focus
    /// reseats (opening/closing the PIN pad), so initialization belongs to `Mount`, which the
    /// dispatcher delivers exactly once for this instance, rather than to every `Enter`.
    fn on_mount<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        self.pad = Pad::closed();
        self.footer = false;
        self.ground.reset();
        auth::dismiss_pin_error();
        self.sync_state();
        fx.invalidate(Provenance::Lifecycle);
    }

    /// Open the pad for roster tile `idx`, and ask the engine to seat on its first key (`'1'`).
    /// Addressed through `fx.from()`, not a stored `InstanceId` — see the module doc's paragraph
    /// on why this screen cannot use `ConsentPage`'s `InstanceId(0)` placeholder.
    fn open_pad<H: AppLike>(&mut self, idx: usize, fx: &mut Effects<'_, H>) {
        self.pad = Pad {
            open: true,
            target: idx,
            ..Pad::closed()
        };
        self.sync_state();
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(pad_key(self.entry, 0, 0)),
            })),
        ));
        fx.invalidate(Provenance::Input);
    }

    /// Take the pad down and seat the engine back on the avatar whose PIN it was, clearing the PIN
    /// verdict with it. **Every door out of the pad calls this — BACK, a background click, and a
    /// resolved non-PIN failure noticed on `Tick`** — for the reason `ui::profiles::close_pad`'s
    /// own doc gives: the pad is the only surface that asks about a PIN, so it is the only one that
    /// may answer about one, and a `pin_denied` left standing after it is gone is a verdict about a
    /// control no longer on screen.
    fn close_pad<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        let target = self.pad.target as u32;
        self.pad = Pad::closed();
        self.sync_state();
        auth::dismiss_pin_error();
        let me = fx.from();
        fx.push(Fx::Deliver(
            me,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: target }),
            })),
        ));
        fx.invalidate(Provenance::Input);
    }

    /// Commit a roster tile (OK or a click): a protected profile opens the pad, else the switch
    /// starts directly. Reads `auth::users()` fresh — this half is the effectful twin of
    /// [`ProfilesView`]'s pure, host-testable geometry, exactly as `ui::profiles::select` sat
    /// beside the pure `act`.
    fn select<H: AppLike>(&mut self, idx: usize, fx: &mut Effects<'_, H>) {
        if auth::users().get(idx).map(|u| u.protected).unwrap_or(false) {
            self.open_pad(idx, fx);
        } else {
            auth::select_profile(idx);
            fx.invalidate(Provenance::Input);
        }
    }

    /// Spend one keypad press — a digit, or `'D'` (backspace) — exactly as `ui::profiles::press`
    /// did: typing cancels a running wrong-PIN flash, and a completed PIN submits and turns the
    /// dot row into a spinner rather than closing the pad (a typo used to bounce the user back to
    /// re-picking the profile every time).
    fn press<H: AppLike>(&mut self, k: u8, fx: &mut Effects<'_, H>) {
        if self.pad.submitting {
            return;
        }
        self.pad.error_s = 0.0;
        if k == b'D' {
            self.pad.entry.pop();
        } else if self.pad.entry.len() < PIN_LEN {
            self.pad.entry.push(k as char);
        }
        if self.pad.entry.len() == PIN_LEN {
            let (idx, pin) = (self.pad.target, self.pad.entry.clone());
            auth::submit_pin(idx, &pin);
            self.pad.submitting = true;
        }
        self.sync_state();
        fx.invalidate(Provenance::Input);
    }

    /// **BACK at the picker's own root** — ported from `app/input.rs::key_onboarding`'s
    /// `Route::Profiles` arm of `onboarding_back`, now that this screen answers its own input and
    /// that ladder never sees it. See the module doc for why this does NOT go through
    /// `LoopReq::BackAtRoot`: the root-press cooldown has to wrap the destructive `auth::cancel()`
    /// call itself, and only this screen (which is about to make that call) can claim it at the
    /// right moment.
    fn back_at_root(&mut self) {
        if !crate::webos::take_root_press() {
            return;
        }
        let backed_out = crate::auth::cancel();
        let phase = crate::auth::phase();
        crate::log(&format!(
            "back: root route=profiles phase={phase:?} backed_out={backed_out}"
        ));
        if backed_out {
            // there WAS somewhere to go inside the app after all — hand the claim back so the
            // real root BACK the user presses on the Home they land on is not eaten by the
            // cooldown this press just started
            crate::webos::release_root_press();
        } else {
            crate::webos::go_home();
        }
    }
}

/// The frame's pure focus/hit geometry — built fresh per query, exactly as every sibling screen's
/// `*View` is (`OnboardView`, `ConsentView`). `n` is the roster size, taken as a field rather than
/// read from `auth::users()` inside every method: that is what makes the avatar/footer half of
/// this geometry host-testable at all — `auth::users()` is permanently empty on this host (every
/// writer of it sits behind a persisted session or a plex.tv round trip), so a `Focusable` that
/// called it directly could only ever be exercised against zero, exactly the boundary
/// `ui::profiles::act`'s own `n: c_int` parameter existed to cross.
struct ProfilesView<'a> {
    screen: &'a ProfilesScreen,
    n: usize,
}

impl ProfilesView<'_> {
    fn sty_and_pitch(&self) -> (RowStyle, f32, f32) {
        let sty = RowStyle::PROFILES;
        let (start_x, pitch) = row_geom(self.n);
        (sty, start_x, pitch)
    }
}

impl<H: AppLike> Focusable<H> for ProfilesView<'_> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let s = self.screen;
        if s.pad.open {
            out.push(GroupSpec {
                id: PAD_GROUP,
                // `holes` names the ONE convention the engine's generic grid walk cannot express
                // for this pad (see the module doc) — carried here anyway so a caller reading the
                // declared shape sees the truth, even though this screen's own `neighbour` (never
                // the generic one) is what actually walks it.
                kind: GroupKind::Grid { cols: PAD_COLS, holes: PAD_HOLES },
                seat: Seat::First,
                reachable: crate::ui::screen::AxisMask::BOTH,
                edge: [EdgeRule::Stop; 4],
                extent: pad_extent(),
                len: PAD_ROWS * PAD_COLS,
                elem: ElemKind::Bare,
            });
            return;
        }
        let (_, start_x, pitch) = self.sty_and_pitch();
        if self.n > 0 {
            let sty = RowStyle::PROFILES;
            let w = (self.n as f32 - 1.0) * pitch + sty.w;
            out.push(GroupSpec {
                id: AVATAR_GROUP,
                kind: GroupKind::Row { wrap: false },
                seat: Seat::Nearest,
                reachable: crate::ui::screen::AxisMask::BOTH,
                // UP: nothing above the roster (Stop, matching "the roster is first" — ▲ holds).
                // DOWN: the footer sits below it (Geometric finds it by extent). LEFT/RIGHT: the
                // roster's own ends hold (Stop) — there is nothing beside it to reach.
                edge: [EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                extent: Rect::new(start_x, ROW_Y, w.max(0.0), sty.h),
                len: self.n,
                elem: ElemKind::Card,
            });
        }
        out.push(GroupSpec {
            id: FOOTER_GROUP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: crate::ui::screen::AxisMask::BOTH,
            // UP reaches the roster geometrically (or, with an empty roster, finds no group at all
            // and simply holds — `Outcome::Nothing`, matching "the pill is focusable with nothing
            // above it to leave"). DOWN/LEFT/RIGHT: the footer is the last stop.
            edge: [EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
            extent: footer_rect(cx.measure),
            len: 1,
            elem: ElemKind::Control,
        });
    }

    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        let s = self.screen;
        if s.pad.open {
            return (*key < (PAD_ROWS * PAD_COLS) as u32).then_some(PAD_GROUP);
        }
        if *key == FOOTER {
            return Some(FOOTER_GROUP);
        }
        (*key < self.n as u32).then_some(AVATAR_GROUP)
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let s = self.screen;
        if s.pad.open {
            return pad_neighbour(key.entry, key.elem, dir);
        }
        if key.elem == FOOTER {
            return Step::Edge; // the footer is one element with nothing beside it
        }
        step_avatar(key.entry, key.elem, dir, self.n)
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let s = self.screen;
        if s.pad.open {
            let (r, c) = pad_pos(*key);
            if r < 0 || r >= PAD_ROWS as i32 {
                return None;
            }
            let rect = pad_key_rect(r as usize, c as usize);
            return Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(*key) });
        }
        if *key == FOOTER {
            let r = footer_rect(cx.measure);
            return Some(Placed { rect: r, rest_rect: r, clip: Rect::FULL, index: Some(0) });
        }
        let i = *key as usize;
        if i >= self.n {
            return None;
        }
        let (sty, start_x, pitch) = self.sty_and_pitch();
        let rest = card_row::tile_rect(i, start_x, pitch, s.row.scroll_x(), ROW_Y, (sty.w, sty.h));
        let scale = match at {
            At::Drawn => s.row.scale(i),
            At::SpringTarget => {
                if s.row.focus() == i as i32 {
                    sty.focus_scale
                } else {
                    1.0
                }
            }
        };
        Some(Placed { rect: rest.scaled(scale), rest_rect: rest, clip: Rect::FULL, index: Some(*key) })
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let s = self.screen;
        if s.pad.open {
            let clamped = want.elem.min((PAD_ROWS * PAD_COLS - 1) as u32);
            let (r, c) = pad_pos(clamped);
            if !is_hole(r, c) {
                return FocusKey { entry: s.entry, elem: clamped };
            }
            return pad_key(s.entry, 0, 0);
        }
        if want.elem == FOOTER {
            return want; // always a valid destination
        }
        if self.n == 0 {
            // the roster this key named just vanished from under it (a sign-out mid-frame, a
            // roster refresh that shrank to nothing) — the footer is the only thing left to stand on
            return FocusKey { entry: s.entry, elem: FOOTER };
        }
        FocusKey { entry: s.entry, elem: want.elem.min(self.n as u32 - 1) }
    }

    fn seat(&self, g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let s = self.screen;
        if g == PAD_GROUP {
            return pad_key(s.entry, 0, 0);
        }
        if g == FOOTER_GROUP {
            return FocusKey { entry: s.entry, elem: FOOTER };
        }
        // AVATAR_GROUP: nearest column by x, `card_row::column_near_x`'s own contract — the rule
        // every shelf-based screen in this app seats a vertical arrival by.
        if self.n == 0 {
            return FocusKey { entry: s.entry, elem: FOOTER };
        }
        let (sty, start_x, pitch) = self.sty_and_pitch();
        let cx_ = from.rect.x + from.rect.w * 0.5;
        let scroll = s.row.scroll_x();
        let guess = ((cx_ - start_x + scroll) / pitch).max(0.0) as usize;
        let from_i = from.index.map_or(guess, |i| i as usize);
        let i = card_row::column_near_x(cx_, start_x, pitch, sty.w, scroll, self.n, from_i);
        FocusKey { entry: s.entry, elem: i as u32 }
    }
}

impl ProfilesScreen {
    fn view(&self) -> ProfilesView<'_> {
        ProfilesView { screen: self, n: auth::users().len() }
    }
}

impl<H: AppLike> Focusable<H> for ProfilesScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        Focusable::<H>::groups(&self.view(), cx, out)
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        Focusable::<H>::group_of(&self.view(), key, cx)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        Focusable::<H>::neighbour(&self.view(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        Focusable::<H>::place(&self.view(), key, cx, at)
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        Focusable::<H>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        Focusable::<H>::seat(&self.view(), g, from, cx)
    }
}

impl<H: AppLike> Machine<H> for ProfilesScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.on_mount(fx);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                let dt = t.dt();
                self.spin_ms += dt * 1000.0;
                self.footer_pop.step((self.footer && !self.pad.open).then_some(0), dt);
                if self.pad.open {
                    // a submitted PIN resolves off-thread: success routes the app away from this
                    // screen entirely (the loop's own phase→route follower, unaffected by this
                    // migration); dropping back to `Phase::Profiles` means the switch failed. Only
                    // a PIN-blaming failure flashes red and stays up — anything else (offline, no
                    // access to this server) closes the pad so the roster's own error banner can
                    // say why, which a red flash would misread as a typo worth retrying forever.
                    if self.pad.submitting && auth::phase() == Phase::Profiles {
                        if auth::pin_denied() {
                            self.pad.submitting = false;
                            self.pad.entry.clear();
                            self.pad.error_s = PIN_ERR_S;
                            self.sync_state();
                            fx.note(PresentEvent::Motion); // the spinner has just become a flash
                        } else {
                            self.close_pad(fx);
                        }
                    }
                    // step the flash AFTER that block, so the frame a rejection lands on is also
                    // the flash's own first frame — and ask for a repaint only on a PHASE FLIP
                    // (`ui/CLAUDE.md`'s standing hazard: a clock-driven animation is invisible to
                    // motion detection built only on spring integrators).
                    let was = pin_flash(self.pad.error_s);
                    self.pad.error_s = (self.pad.error_s - dt).max(0.0);
                    if pin_flash(self.pad.error_s) != was {
                        fx.note(PresentEvent::Motion);
                    }
                }
                let n = auth::users().len();
                if self.fc >= n.max(1) {
                    self.fc = 0; // defensive: the roster shrank under a stale render mirror
                }
                let focus = (!self.pad.open && !self.footer && n > 0).then_some(self.fc.min(n.saturating_sub(1)));
                self.row.update(n, focus, &RowStyle::PROFILES, dt);
                if n == 0 {
                    fx.note(PresentEvent::Motion); // the loading spinner in place of the roster
                }
                if auth::phase() == Phase::Switching {
                    fx.note(PresentEvent::Motion); // the full-screen switching spinner
                }
                Handled::Yes
            }
            // `Enter` is a focus protocol event. The dispatcher seats the engine after this
            // returns; local pad open/close paths use the same event to request a reseat.
            ScreenEvent::Enter(_) => Handled::Yes,
            ScreenEvent::FocusMoved { to, .. } => {
                if self.pad.open {
                    let (r, c) = pad_pos(to.elem);
                    self.pad.fr = r;
                    self.pad.fc = c;
                } else if to.elem == FOOTER {
                    self.footer = true;
                } else {
                    self.footer = false;
                    self.fc = to.elem as usize;
                }
                Handled::Yes
            }
            // The PIN keypad's own keys — `Bare`, so they commit on the OK key-DOWN or a direct
            // click with no press to arm. `PAD_DISMISS` is the pad's background: a click that hit
            // neither a real key nor a `None` cell within the grid's own bounds — the pointer's
            // door out of the pad, `ui::profiles::PadClick::Dismiss`'s new-model home.
            ScreenEvent::Activate(e) => {
                if self.pad.open {
                    if *e == PAD_DISMISS {
                        self.close_pad(fx);
                    } else {
                        let (r, c) = pad_pos(*e);
                        if let Some(Some(k)) = KEYS.get(r as usize).and_then(|row| row.get(c as usize)) {
                            self.press(*k, fx);
                        }
                    }
                }
                Handled::Yes
            }
            // The avatar (`Card`, holdable) and the footer (`Control`) both commit here — the one
            // place `ui::profiles::activate_focused` used to dispatch on `s.footer`, now read off
            // the engine's own current key instead of a self-owned boolean flag.
            ScreenEvent::PressCommit(_) => {
                if !self.pad.open {
                    if let Some(k) = cx.focus.current {
                        if k.elem == FOOTER {
                            auth::sign_out();
                            fx.invalidate(Provenance::Input);
                        } else {
                            self.select(k.elem as usize, fx);
                        }
                    }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. },
                ..
            }) => {
                if self.pad.open {
                    self.close_pad(fx);
                } else {
                    self.back_at_root();
                }
                Handled::Yes
            }
            // Remote number buttons type straight into the PIN, whichever cell happens to hold
            // focus — `ui::profiles::pad_key`'s own `digit_of` check ran before its own nav arms
            // for the same reason. Not gated on `key` at all (only on the raw `sym`/`wcode`
            // range): OK's and the D-pad's own codes never fall in 48..=57, so nothing here can
            // shadow them.
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { sym, wcode, edge: Edge::Down, .. },
                ..
            }) if self.pad.open => match digit_of(*sym, *wcode) {
                Some(d) => {
                    self.press(d, fx);
                    Handled::Yes
                }
                None => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

impl<H: AppLike> Screen<H> for ProfilesScreen {
    fn name(&self) -> &'static str {
        // Byte-identical to the route word `tests/manifest.json` selects fps samples by
        // (`app/mod.rs::route_word(Route::Profiles) == "profiles"`) — it must not change.
        "profiles"
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        // Not a route-family screen — the picker keeps its own bespoke centred composition
        // (`ui::profiles`'s own module doc), so there is no chevron to draw and BACK's meaning is
        // decided by `back_at_root`, not by a crumb naming a place inside the app.
        None
    }
    fn prepare(&mut self, _b: &mut crate::ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, H>) {
        let p = f.painter;
        self.ground.draw_default(p);
        let users = auth::users();
        let env = Env::inert();

        if self.pad.open {
            self.draw_pad(f, &users);
            return;
        }

        if let Ok(t) = CString::new(TITLE) {
            p.text(t.as_ptr(), SCR_W * 0.5, 168.0, theme::size::HERO, theme::TEXT_PRIMARY, 1, 1);
        }

        let sty = RowStyle::PROFILES;
        let n = users.len();
        let (start_x, slot) = row_geom(n);
        let scroll = self.row.scroll_x();

        // a big low-z background stop over the whole panel would be wrong here — unlike the pad,
        // the picker's dead space is not a dismiss target, so no catch-all `Activate::Direct` stop
        // is registered for it

        let mut focused: Option<usize> = None;
        for (i, u) in users.iter().enumerate() {
            let cx_ = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
            let base = Rect::new(cx_ - sty.w * 0.5, ROW_Y, sty.w, sty.h);
            let sc = self.row.scale(i);
            let is_foc = i == self.fc && !self.footer;
            if is_foc {
                focused = Some(i);
                continue; // the focused tile draws last, ring over neighbours
            }
            card_row::draw_tile(
                p,
                Art::Thumb { sid: crate::plex::current_server(), key: &u.thumb, res: (300, 300) },
                base.scaled(sc),
                sc,
                &sty,
                None,
            );
            f.stop(
                p,
                crate::ui::screen::Stop {
                    key: FocusKey { entry: self.entry, elem: i as u32 },
                    rect: base.scaled(sc),
                    rest_rect: base,
                    clip: Rect::FULL,
                    hover: crate::ui::screen::Hover::Focus,
                    activate: crate::ui::screen::Activate::Press,
                },
            );
            draw_name(p, &u.title, cx_, false);
        }
        if let Some(i) = focused {
            let u = &users[i];
            let cx_ = start_x + i as f32 * slot + sty.w * 0.5 - scroll;
            let base = Rect::new(cx_ - sty.w * 0.5, ROW_Y, sty.w, sty.h);
            // fold the ui::press click dip into the focused avatar's pop (1.0 when idle) —
            // `press::scale()` reads the SHARED published snapshot `ui/press.rs` describes: the
            // dispatcher's own `InputMachine.press` publishes to the same thread-local this global
            // reads, so this is correct for an engine-armed press, not a leftover legacy call.
            let sc = self.row.scale(i) * press::scale();
            card_row::draw_focused(
                p,
                Art::Thumb { sid: crate::plex::current_server(), key: &u.thumb, res: (300, 300) },
                base.scaled(sc),
                sc,
                &sty,
                None,
                &card_row::TileLabel::default(),
            );
            f.stop(
                p,
                crate::ui::screen::Stop {
                    key: FocusKey { entry: self.entry, elem: i as u32 },
                    rect: base.scaled(sc),
                    rest_rect: base,
                    clip: Rect::FULL,
                    hover: crate::ui::screen::Hover::Focus,
                    activate: crate::ui::screen::Activate::Press,
                },
            );
            draw_name(p, &u.title, cx_, true);
        }

        let footer_r = footer_rect(f.measure);
        Button::new(c"Sign out".as_ptr(), theme::size::BODY, footer_r)
            .focused(self.footer)
            .scale(self.footer_pop.scale(0))
            .palette(self.ground.palette())
            .draw(&env, p);
        f.stop(
            p,
            crate::ui::screen::Stop {
                key: FocusKey { entry: self.entry, elem: FOOTER },
                rect: footer_r,
                rest_rect: footer_r,
                clip: Rect::FULL,
                hover: crate::ui::screen::Hover::Focus,
                activate: crate::ui::screen::Activate::Press,
            },
        );

        if users.is_empty() {
            Spinner::new(SCR_W * 0.5, ROW_Y + sty.h * 0.5, 26.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&env, p);
        }

        let err = auth::error();
        if !err.is_empty() && auth::phase() == Phase::Profiles {
            if let Ok(e) = CString::new(err) {
                let ey = crate::text::text_vcenter_y(theme::size::BODY, 0, ERROR_Y);
                p.text(e.as_ptr(), SCR_W * 0.5, ey, theme::size::BODY, theme::TEXT_SECONDARY, 1, 0);
            }
        }

        if auth::phase() == Phase::Switching {
            p.rect(Rect::FULL, 0.0, theme::scrim_black(0.88), theme::scrim_black(0.88), 0.0);
            Spinner::new(SCR_W * 0.5, 500.0, 26.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&env, p);
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl ProfilesScreen {
    /// The pad's own draw — ported from `ui::profiles::draw_pad`, with a low-z background stop
    /// added for the pointer's "click outside a key" dismissal (`PadClick::Dismiss`'s new home)
    /// and a real per-key stop for every non-hole cell, registered AFTER the background so a key
    /// always wins the hit map's "last stop wins" z-order over the catch-all beneath it.
    fn draw_pad<H: AppLike>(&mut self, f: &mut DrawFrame<'_, H>, users: &[auth::UserTile]) {
        let p = f.painter;
        let env = Env::inert();
        let (title_y, dots_y, _) = pad_geom();

        // the background dismiss target — the whole panel, `Hover::Ignore` so hovering dead space
        // between keys does not steal focus off whatever key already holds it (mirrors
        // `ui::profiles::pointer_focus`'s pad branch, which moved focus only on a real key hit).
        f.stop(
            p,
            crate::ui::screen::Stop {
                key: FocusKey { entry: self.entry, elem: PAD_DISMISS },
                rect: Rect::FULL,
                rest_rect: Rect::FULL,
                clip: Rect::FULL,
                hover: crate::ui::screen::Hover::Ignore,
                activate: crate::ui::screen::Activate::Direct,
            },
        );

        let name = users.get(self.pad.target).map(|u| u.title.as_str()).unwrap_or("");
        if let Ok(t) = CString::new(format!("Enter {name}'s PIN")) {
            p.text(t.as_ptr(), SCR_W * 0.5, title_y, theme::size::TITLE, theme::TEXT_PRIMARY, 1, 1);
        }
        if self.pad.submitting {
            Spinner::new(SCR_W * 0.5, dots_y + PAD_DOT * 0.5, 22.0)
                .phase(self.spin_ms as u32)
                .tint(theme::TEXT_PRIMARY)
                .draw(&env, p);
        } else {
            let flash = pin_flash(self.pad.error_s);
            let dgap = 34.0f32;
            let dw = PIN_LEN as f32 * PAD_DOT + (PIN_LEN as f32 - 1.0) * dgap;
            let mut dx = SCR_W * 0.5 - dw * 0.5;
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
        for r in 0..PAD_ROWS {
            for c in 0..PAD_COLS {
                let Some(k) = KEYS[r][c] else { continue };
                let rect = pad_key_rect(r, c);
                let foc = r as i32 == self.pad.fr && c as i32 == self.pad.fc;
                let (fill, ink) = if foc {
                    (theme::ACCENT, theme::ACCENT_INK)
                } else {
                    (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
                };
                p.rect(rect, 18.0, fill, fill, 0.0);
                if k == b'D' {
                    let d = (rect.w * 0.42).round();
                    icons::draw(
                        p,
                        Icon::Backspace,
                        Rect::new(rect.x + (rect.w - d) * 0.5, rect.y + (rect.h - d) * 0.5, d, d),
                        ink,
                    );
                } else if let Ok(lc) = CString::new((k as char).to_string()) {
                    let ty = crate::text::text_vcenter_y(theme::size::TITLE, 1, rect.y + rect.h * 0.5);
                    p.text(lc.as_ptr(), rect.x + rect.w * 0.5, ty, theme::size::TITLE, ink, 1, 1);
                }
                f.stop(
                    p,
                    crate::ui::screen::Stop {
                        key: FocusKey { entry: self.entry, elem: pad_elem(r as i32, c as i32) },
                        rect,
                        rest_rect: rect,
                        clip: Rect::FULL,
                        hover: crate::ui::screen::Hover::Focus,
                        activate: crate::ui::screen::Activate::Direct,
                    },
                );
            }
        }
    }
}

fn draw_name(p: Painter, title: &str, cx_: f32, focused: bool) {
    let col = if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
    let name = crate::text::elide(
        title,
        RowStyle::PROFILES.w + RowStyle::PROFILES.gap - 12.0,
        theme::size::LABEL,
        if focused { 1 } else { 0 },
        false,
    );
    if let Ok(nc) = CString::new(name) {
        p.text(
            nc.as_ptr(),
            cx_,
            ROW_Y + NAME_DY,
            theme::size::LABEL,
            col,
            1,
            if focused { 1 } else { 0 },
        );
    }
}

// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Ported from `ui::profiles`'s own test module, split the same way it was and for the same
    //! reason: a pure half (`digit_of`, `pin_flash`, the pad's own column walkers) needs no
    //! fixture at all, and a `Focusable`/`FocusEngine`-driven half exercises the REAL navigation
    //! contract — a step up from the legacy module's own `act()`-only coverage of the roster↔
    //! footer ladder, since `ui::geom.rs`'s own precedent (its `a_grid_seats_by_the_column_near_x_
    //! contract` test) is what this borrows its harness shape from. Nothing here calls
    //! `auth::select_profile`/`sign_out`/`submit_pin`/`cancel` — those reach a switch worker, a
    //! login thread or the platform, exactly the boundary `ui::profiles`'s own tests drew.
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{FocusRead, InputOwner, PressRead, Tick};
    use crate::ui::present::Present;
    use crate::ui::focus::{FocusEngine, Outcome};

    use super::super::family::InnerHost;

    const E: EntryId = EntryId(9);
    const OWNER: InputOwner = InputOwner::Entry(E);

    fn cx(m: &FixtureMeasure) -> Cx<'_, InnerHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: OWNER,
        }
    }

    fn screen() -> ProfilesScreen {
        ProfilesScreen::new(E)
    }

    fn view(s: &ProfilesScreen, n: usize) -> ProfilesView<'_> {
        ProfilesView { screen: s, n }
    }

    // -----------------------------------------------------------------------------------------
    // pure geometry
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_pin_digit_is_read_from_either_field() {
        assert_eq!(digit_of(b'7' as u32, 0), Some(b'7'), "a dev keyboard's sym");
        assert_eq!(digit_of(0, 55), Some(b'7'), "the remote's wcode, same digit");
        assert_eq!(digit_of(b'0' as u32, 0), Some(b'0'));
        assert_eq!(digit_of(999, 0), None, "a D-pad-shaped sym is not a digit");
        assert_eq!(digit_of(13, 0), None, "nor is OK — 13 is below the digit range");
    }

    #[test]
    fn pad_holes_matches_the_keys_table() {
        for r in 0..PAD_ROWS {
            for c in 0..PAD_COLS {
                assert_eq!(
                    KEYS[r][c].is_none(),
                    PAD_HOLES.contains(&(r, c)),
                    "KEYS[{r}][{c}] and PAD_HOLES disagree about whether this cell is the hole"
                );
            }
        }
    }

    /// The keypad's bottom row has a blank where a phone dial pad has nothing: ◀ from `0` has no
    /// key to its left, and ▼ off `7` lands on `0` rather than on the gap directly under it — the
    /// legacy behaviour this whole module's doc argues the generic grid-with-holes algorithm would
    /// get wrong.
    #[test]
    fn the_keypad_walks_around_its_empty_cell() {
        assert!(is_hole(3, 0), "the cell both walkers below have to step around");
        assert_eq!(pad_step_col(3, 1, -1), None, "◀ from '0' finds nothing to its left and holds");
        assert_eq!(pad_step_col(3, 1, 1), Some(2), "▶ from '0' reaches delete");
        assert_eq!(pad_step_col(0, 0, -1), None, "a row edge holds");
        assert_eq!(pad_step_col(0, 2, 1), None);
        assert_eq!(pad_nearest_col(3, 0), 1, "▼ off '7' lands on '0'");
        assert_eq!(pad_nearest_col(3, 2), 2, "▼ off '9' lands on delete, directly under it");
        assert_eq!(pad_nearest_col(1, 1), 1, "an occupied column is kept as it is");

        match pad_neighbour(E, pad_elem(2, 0), Dir::Down) {
            Step::Move(k) => assert_eq!(k.elem, pad_elem(3, 1), "DOWN off '7' reaches '0', not the hole"),
            Step::Edge => panic!("DOWN off '7' is a move"),
        }
        match pad_neighbour(E, pad_elem(0, 1), Dir::Up) {
            Step::Edge => {}
            Step::Move(_) => panic!("UP off the top row holds"),
        }
        match pad_neighbour(E, pad_elem(3, 1), Dir::Right) {
            Step::Move(k) => assert_eq!(k.elem, pad_elem(3, 2)),
            Step::Edge => panic!("RIGHT from '0' reaches delete"),
        }
    }

    #[test]
    fn pin_flash_opens_lit_and_blinks_out() {
        assert_eq!(pin_flash(0.0), None, "no flash running");
        assert_eq!(pin_flash(PIN_ERR_S), Some(true), "a rejected PIN opens red, not dim");
        let mut phases = Vec::new();
        let mut t = PIN_ERR_S;
        let dt = 1.0 / 60.0;
        while t > 0.0 {
            let now = pin_flash(t);
            if phases.last() != Some(&now) {
                phases.push(now);
            }
            t = (t - dt).max(0.0);
        }
        // The loop's guard samples only positive times; record the exact endpoint explicitly so
        // the assertion covers the production function's terminal `None` state as well.
        let end = pin_flash(0.0);
        if phases.last() != Some(&end) {
            phases.push(end);
        }
        assert_eq!(phases.last(), Some(&None), "the flash ends dark, not stuck lit");
        let lit = phases.iter().filter(|p| **p == Some(true)).count();
        assert!(lit >= 4, "a 1.4s window must read as several distinct pulses, got {phases:?}");
    }

    // -----------------------------------------------------------------------------------------
    // avatar ↔ footer navigation, through the real Focusable/FocusEngine contract
    // -----------------------------------------------------------------------------------------

    /// The engine's own account of "▼ off the roster is the Sign out pill, and holds there; ▲
    /// brings it back and holds at the roster too" — `ui::profiles`'s own
    /// `focus_opens_on_the_roster_and_the_pill_is_one_step_down`, driven through the geometry
    /// itself rather than through the retired `act`/`key` ladder.
    #[test]
    fn down_from_the_roster_reaches_the_footer_and_holds_there() {
        let m = FixtureMeasure;
        let c = cx(&m);
        let s = screen();
        let v = view(&s, 3);
        let mut e: FocusEngine<u32> = FocusEngine::new();
        assert!(matches!(
            e.enter(OWNER, &v, FocusTarget::ContainerGroup(AVATAR_GROUP), None, &c),
            Outcome::Moved { .. }
        ));
        assert_eq!(e.current(OWNER).unwrap().elem, 0, "a fresh enter lands on the first avatar");

        assert!(matches!(e.move_dir(OWNER, &v, &[], Dir::Down, &c), Outcome::Moved { .. }));
        assert_eq!(e.current(OWNER).unwrap().elem, FOOTER);
        assert!(
            matches!(e.move_dir(OWNER, &v, &[], Dir::Down, &c), Outcome::Nothing),
            "the pill is the last stop — ▼ again holds it"
        );

        assert!(matches!(e.move_dir(OWNER, &v, &[], Dir::Up, &c), Outcome::Moved { .. }));
        assert_ne!(e.current(OWNER).unwrap().elem, FOOTER, "▲ returns to the roster");
        assert!(
            matches!(e.move_dir(OWNER, &v, &[], Dir::Up, &c), Outcome::Nothing),
            "…and holds there — the roster is the first stop"
        );
    }

    /// `ui::profiles`'s own `the_sign_out_pill_takes_focus_with_no_roster_on_screen`: with an
    /// empty roster the avatar group is not declared at all, so a fresh enter's default target
    /// (`ContainerGroup(AVATAR_GROUP)`) falls through to the first group that DOES have elements —
    /// the footer — which is exactly "reachable even while the roster is empty/loading".
    #[test]
    fn the_footer_is_reachable_with_no_roster_at_all() {
        let m = FixtureMeasure;
        let c = cx(&m);
        let s = screen();
        let v = view(&s, 0);
        let mut e: FocusEngine<u32> = FocusEngine::new();
        let outcome = e.enter(OWNER, &v, FocusTarget::ContainerGroup(AVATAR_GROUP), None, &c);
        assert!(matches!(outcome, Outcome::Moved { .. }), "an empty roster must not leave nothing focused");
        assert_eq!(e.current(OWNER).unwrap().elem, FOOTER);
    }

    /// The roster's own LEFT/RIGHT clamp at both ends — `ui::profiles::step_fc`'s new home. LEFT/
    /// RIGHT are declared `Stop` at the group's own edges, so a probe past either end never even
    /// reaches the geometric search; the clamp is `step_avatar`'s Edge answer alone.
    #[test]
    fn the_roster_neighbour_clamps_at_both_ends() {
        assert!(matches!(step_avatar(E, 0, Dir::Left, 3), Step::Edge), "◀ on the first tile holds");
        assert!(matches!(step_avatar(E, 1, Dir::Left, 3), Step::Move(k) if k.elem == 0));
        assert!(matches!(step_avatar(E, 2, Dir::Right, 3), Step::Edge), "▶ on the last tile holds");
        assert!(matches!(step_avatar(E, 1, Dir::Right, 3), Step::Move(k) if k.elem == 2));
        assert!(matches!(step_avatar(E, 0, Dir::Right, 1), Step::Edge), "a one-profile roster has nowhere to walk");
    }

    /// **Focus is exclusive**: while the pad is open, `groups()` reports the grid ALONE — the
    /// roster and the footer are not reachable by any direction, which is what "the pad traps
    /// focus" means mechanically. `ui::profiles`'s own
    /// `an_open_keypad_takes_the_key_before_the_picker_does` pinned the same property from the
    /// key-ladder side; this pins it from the declared shape the engine actually reasons over.
    #[test]
    fn only_the_grid_group_is_reachable_while_the_pad_is_open() {
        let m = FixtureMeasure;
        let c = cx(&m);
        let mut s = screen();
        s.pad = Pad { open: true, target: 0, ..Pad::closed() };
        let v = view(&s, 3);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&v, &c, &mut groups);
        assert_eq!(groups.len(), 1, "no avatar group and no footer group while the pad is up");
        assert_eq!(groups[0].id, PAD_GROUP);
        assert_eq!(groups[0].len, PAD_ROWS * PAD_COLS);
    }

    /// A digit typed anywhere on the pad reaches [`ProfilesScreen::press`] through the SAME real
    /// `step()` the dispatcher calls, whichever cell happens to hold focus — `ui::profiles::
    /// pad_key`'s own property, that typing does not depend on navigating to the key first.
    #[test]
    fn a_typed_digit_reaches_the_pad_regardless_of_which_cell_holds_focus() {
        let m = FixtureMeasure;
        let c = cx(&m);
        let mut present = Present::new();
        let mut buf: Vec<crate::ui::machine::Stamped<InnerHost>> = Vec::new();
        let mut s = screen();
        s.pad = Pad { open: true, target: 0, fr: 2, fc: 2, ..Pad::closed() };
        let ev = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Script,
            kind: InputKind::Key {
                key: Key::Other,
                sym: b'5' as u32,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        let handled = {
            let mut fx = Effects::new(&mut buf, crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)), &mut present);
            Machine::<InnerHost>::step(&mut s, &ev, &c, &mut fx)
        };
        assert_eq!(handled, Handled::Yes);
        assert_eq!(s.pad.entry, "5", "the digit typed regardless of the focused cell (2,2)");
    }

    /// Every door out of the pad clears the PIN verdict — BACK, ported from `ui::profiles`'s
    /// `every_door_out_of_the_keypad_goes_through_one_close`'s first door. The pointer's own door
    /// (`PAD_DISMISS`) shares the exact same `close_pad` call, so it is not re-tested separately —
    /// the risk that test guarded against was two SEPARATE cleanup paths disagreeing, and there is
    /// only one now.
    #[test]
    fn back_closes_the_pad_and_clears_the_pin_verdict() {
        let _s = crate::testlock::serial();
        let m = FixtureMeasure;
        let c = cx(&m);
        let mut present = Present::new();
        let mut buf: Vec<crate::ui::machine::Stamped<InnerHost>> = Vec::new();
        let mut s = screen();
        s.pad = Pad { open: true, target: 0, error_s: PIN_ERR_S, ..Pad::closed() };
        auth::set_pin_denied_for_test(true);
        let ev = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Script,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        });
        {
            let mut fx = Effects::new(&mut buf, crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)), &mut present);
            Machine::<InnerHost>::step(&mut s, &ev, &c, &mut fx);
        }
        assert!(!s.pad.open, "BACK takes the pad down");
        assert!(!auth::pin_denied(), "…and the verdict, which is what would leak onto the roster behind it");
        assert!(
            buf.iter().any(|st| matches!(
                &st.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                    if k.elem == 0
            )),
            "closing re-seats the engine on the avatar whose pad this was, not on the group's default corner"
        );
        auth::set_pin_denied_for_test(false);
    }

    /// Opening the keypad queues a focus reseat through the dispatcher. That reseat must not be
    /// mistaken for a fresh screen entry: delivering the queued `Enter` is the first frame in
    /// which the dispatcher can observe the newly opened pad, and it must still be open then.
    #[test]
    fn dispatcher_queued_pad_focus_reseat_does_not_reset_the_screen() {
        let m = FixtureMeasure;
        let c = cx(&m);
        let mut present = Present::new();
        let mut buf: Vec<crate::ui::machine::Stamped<InnerHost>> = Vec::new();
        let mut s = screen();
        {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            s.open_pad(2, &mut fx);
        }
        assert!(s.pad.open, "the activation side opens the pad before queuing its focus reseat");
        let queued_focus = buf
            .iter()
            .find_map(|st| match &st.fx {
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::Elem(k),
                }))) => Some(*k),
                _ => None,
            })
            .expect("opening the pad queues an element-focused Enter");
        buf.clear();
        {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            Machine::<InnerHost>::step(
                &mut s,
                &ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::Elem(queued_focus),
                }),
                &c,
                &mut fx,
            );
        }
        assert!(s.pad.open, "a queued focus reseat must not run fresh-entry initialization");
        assert_eq!(s.pad.target, 2, "the reseat must preserve the selected profile");

        let digit = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Script,
            kind: InputKind::Key {
                key: Key::Other,
                sym: b'5' as u32,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            assert_eq!(Machine::<InnerHost>::step(&mut s, &digit, &c, &mut fx), Handled::Yes);
        }
        assert_eq!(s.pad.entry, "5", "the keypad remains live after the queued focus reseat");
    }
}
