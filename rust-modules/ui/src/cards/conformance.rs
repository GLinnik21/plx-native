//! Tier 2 of the card conformance suite: black-box cases run against every real card screen.
//!
//! `ui` may not name a screen, so this module owns only the DRIVERS: the [`CardHarness`] a screen
//! implements (mount with N cards, a focus, events, ticks, `Focusable::place`, the drawn rect and
//! scale of a card, canon, landings and memory) and one function per case. The per-screen table,
//! the harness impls and the shrink-only expected-failure list (`ci/allow/cards-conformance.txt`)
//! live in `screens`, which can name the screens.
//!
//! A case that cannot be driven black-box on a screen returns [`Outcome::Unsupported`] with the
//! reason. It is never weakened to pass: a failing case is the deliverable.
#![cfg(any(test, feature = "test-support"))]

use crate::screen::{At, By, Dir, Placed};
use crate::Rect;

/// What `Focusable::neighbour` answered, with the key reduced to the element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nb {
    To(u32),
    Edge,
}

/// A change to the shelf/grid's content while a card is focused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    /// The first two cards swap places.
    Reorder,
    /// A new card lands before the focused one.
    InsertAbove,
    /// The focused card leaves the content.
    RemoveFocused,
}

/// The result of one case on one screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    Unsupported(&'static str),
}

/// A card screen driven through its public contract only: `Screen::step` events, `Focusable`, and
/// the draw geometry the screen already exposes to its own tests. Elements are `u32`, which every
/// card screen's `Host::Elem` is. A harness keeps its own stand-in for the focus engine: the
/// current focus is whatever the last [`CardHarness::focus`] set, delivered as `FocusMoved`.
pub trait CardHarness {
    /// The card elements in reading order.
    fn cards(&self) -> Vec<u32>;
    /// The focused element.
    fn focused(&self) -> Option<u32>;
    /// Move focus to `elem` the way the engine does: set `cx.focus.current`, then deliver
    /// `FocusMoved { from, to, by }`.
    fn focus(&mut self, elem: u32, by: By);
    /// Run `frames` 60 Hz ticks. True when any of them reported page motion (the idle gate).
    fn tick(&mut self, frames: u32) -> bool;
    /// `ScreenEvent::Cover` (a menu opened over the page).
    fn cover(&mut self);
    /// `ScreenEvent::Uncover` (the menu closed).
    fn uncover(&mut self);
    /// The live press scale `cx.press` carries from now on (1.0 = no press).
    fn set_press(&mut self, scale: f32);
    /// `Focusable::neighbour` from `elem`.
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb;
    /// `Focusable::place` for `elem` at `at`.
    fn place(&self, elem: u32, at: At) -> Option<Placed>;
    /// The rect the screen draws `elem` with (and registers as its stop) at the given press, or
    /// `None` when the screen exposes no such read.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect>;
    /// The focus-scale the screen draws `elem` at, 1.0 at rest, or `None` when not exposed.
    fn scale(&self, elem: u32) -> Option<f32>;
    /// The scale a SETTLED focused card draws at, for this screen's card style.
    fn focus_scale(&self) -> f32;
    /// The screen's canonical state hash.
    fn canon(&self) -> u64;
    /// A stable label for the item behind `elem` (its rating key), valid across instances.
    fn identity(&self, elem: u32) -> String;
    /// The scroll offset when the screen exposes one.
    fn scroll(&self) -> Option<f32> {
        None
    }
    /// The furthest the scroll offset may reach for the current cards, when the screen exposes one
    /// (a horizontal strip's; a grid's vertical range belongs to its owner and is not checked).
    fn scroll_max(&self) -> Option<f32> {
        None
    }
    /// The column count of a grid layout, `None` for a horizontal strip. A landing may move a
    /// focused card to another COLUMN of a grid (its x changes with its index); a strip's card
    /// must not move at all.
    fn columns(&self) -> Option<usize> {
        None
    }
    /// Apply a landing, then deliver what the host does on one: the store-changed event and the
    /// engine's `reconcile` of the focused element. `Err` when the harness cannot stage it.
    fn landing(&mut self, l: Landing) -> Result<(), &'static str>;
    /// `by` new cards arrive before every card shown (a window sliding back, a refetch inserting
    /// ahead): the store-changed event only, no focus change. `Err` when the harness cannot stage
    /// it ([`shifted_under_a_focus_change`] is then `Unsupported`).
    fn shift(&mut self, by: usize) -> Result<(), &'static str> {
        let _ = by;
        Err("this harness cannot shift its cards")
    }
    /// `memory_at(focus)`, a fresh instance, `RestoreMemory`, the content landing, and the
    /// engine's `reconcile` of the old focus: the fresh instance, focused where it re-seated.
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str>;
}

/// How a case builds its screen: `n` cards, mounted and ticked once, nothing focused.
pub type Mount = fn(n: usize) -> Box<dyn CardHarness>;

/// The cases, in the order the suite runs them. The name is the key of the expected-failure list.
pub const CASES: [(&str, fn(Mount) -> Outcome); 7] = [
    ("walk", walk),
    ("hold_then_move", hold_then_move),
    ("pop_rule", pop_rule),
    ("geometry", geometry),
    ("landing", landing),
    ("memory", memory),
    ("determinism_idle", determinism_idle),
];

const SETTLE: u32 = 180;
const EPS: f32 = 0.0005;

fn fail(msg: impl Into<String>) -> Outcome {
    Outcome::Fail(msg.into())
}

fn rect_eq(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5 && (a.w - b.w).abs() < 0.5 && (a.h - b.h).abs() < 0.5
}

fn show(r: Rect) -> String {
    format!("({:.1},{:.1} {:.1}x{:.1})", r.x, r.y, r.w, r.h)
}

/// Mount 8 cards and focus the first as a restore, settled.
fn settled_first(mount: Mount) -> Result<(Box<dyn CardHarness>, Vec<u32>), Outcome> {
    let mut h = mount(8);
    let cards = h.cards();
    if cards.len() < 3 {
        return Err(fail(format!("mounted with 8 items but exposes {} cards", cards.len())));
    }
    h.tick(2);
    h.focus(cards[0], By::Restore);
    h.tick(SETTLE);
    Ok((h, cards))
}

/// Walk: from every reachable card, every direction lands on a placeable element or an edge.
pub fn walk(mount: Mount) -> Outcome {
    let mut h = mount(14);
    h.tick(2);
    let cards = h.cards();
    if cards.len() < 4 {
        return fail(format!("mounted with 14 items but exposes {} cards", cards.len()));
    }
    let mut bad = Vec::new();
    for &c in &cards {
        h.focus(c, By::Restore);
        h.tick(2);
        for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
            if let Nb::To(e) = h.neighbour(c, dir) {
                if h.place(e, At::Drawn).is_none() {
                    bad.push(format!("{c} {dir:?} -> {e} (unplaceable)"));
                }
            }
        }
        if h.place(c, At::Drawn).is_none() {
            bad.push(format!("{c} itself is unplaceable"));
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Hold, then move: a covering menu leaves the focused tile's pop settled; after the menu closes
/// and a Dir move, the old tile returns to rest and the new one grows from rest and settles.
pub fn hold_then_move(mount: Mount) -> Outcome {
    let (mut h, cards) = match settled_first(mount) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (a, b) = (cards[0], cards[1]);
    let (Some(rest), Some(full)) = (h.scale(b), h.scale(a)) else {
        return Outcome::Unsupported("the screen exposes no tile scale read");
    };
    if full < rest + EPS {
        return fail(format!("a settled focused card is not popped before the menu: {full} vs rest {rest}"));
    }
    h.cover();
    h.tick(60);
    let covered = h.scale(a).unwrap_or(0.0);
    if (covered - full).abs() > 0.005 {
        return fail(format!("the covered focused tile left its settled pop: {covered} vs {full}"));
    }
    h.uncover();
    h.tick(2);
    h.focus(b, By::Dir);
    h.tick(1);
    let (new, old) = (h.scale(b).unwrap_or(0.0), h.scale(a).unwrap_or(0.0));
    if !(new > rest + EPS && new < full - EPS) {
        return fail(format!("after the menu the new tile does not grow from rest: {new} (rest {rest}, full {full})"));
    }
    if !(old > rest + EPS && old < full - EPS) {
        return fail(format!("after the menu the old tile does not let go over frames: {old} (rest {rest}, full {full})"));
    }
    h.tick(SETTLE);
    let (new, old) = (h.scale(b).unwrap_or(0.0), h.scale(a).unwrap_or(0.0));
    if (new - full).abs() > 0.005 || (old - rest).abs() > 0.005 {
        return fail(format!("did not settle: new {new} (want {full}), old {old} (want {rest})"));
    }
    Outcome::Pass
}

/// Pop rule: a Dir or Pointer move grows from rest; a restore or reconcile arrival is adopted whole.
pub fn pop_rule(mount: Mount) -> Outcome {
    let mut h = mount(8);
    let cards = h.cards();
    if cards.len() < 4 {
        return fail("fewer than 4 cards");
    }
    h.tick(2);
    let Some(rest) = h.scale(cards[3]) else {
        return Outcome::Unsupported("the screen exposes no tile scale read");
    };
    let full = h.focus_scale();
    let mut bad = Vec::new();
    for (idx, by) in [(0usize, By::Restore), (1, By::Reconcile)] {
        let mut h = mount(8);
        h.tick(2);
        h.focus(cards[idx], by);
        h.tick(1);
        let s = h.scale(cards[idx]).unwrap_or(0.0);
        if (s - full).abs() > 0.005 {
            bad.push(format!("{by:?} arrival drew {s}, not adopted whole ({full})"));
        }
    }
    for by in [By::Dir, By::Pointer] {
        let mut h = mount(8);
        h.tick(2);
        h.focus(cards[0], By::Restore);
        h.tick(SETTLE);
        h.focus(cards[1], by);
        h.tick(1);
        let s = h.scale(cards[1]).unwrap_or(0.0);
        if !(s > rest + EPS && s < full - EPS) {
            bad.push(format!("{by:?} move drew {s}, not growing from rest {rest} toward {full}"));
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Geometry: the placed rect equals the drawn rect at press 1.0 and at a mid press, and
/// `rest_rect` is the settled focus-scaled rect (the L1 contract in the [`super`] doc).
pub fn geometry(mount: Mount) -> Outcome {
    let (mut h, cards) = match settled_first(mount) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let c = cards[0];
    let mut bad = Vec::new();
    // 0.96: mid-dip (the press machine dips 1.0 -> 0.918).
    for press in [1.0f32, 0.96] {
        h.set_press(press);
        let Some(drawn) = h.drawn_rect(c, press) else {
            return Outcome::Unsupported("the screen exposes no drawn-rect read");
        };
        let Some(placed) = h.place(c, At::Drawn) else {
            return fail("the focused card is not placeable");
        };
        if !rect_eq(placed.rect, drawn) {
            bad.push(format!("press {press}: place {} != drawn {}", show(placed.rect), show(drawn)));
        }
    }
    h.set_press(1.0);
    if let (Some(placed), Some(drawn)) = (h.place(c, At::Drawn), h.drawn_rect(c, 1.0)) {
        if !rect_eq(placed.rest_rect, drawn) {
            bad.push(format!("rest_rect {} is not the settled focused rect {}", show(placed.rest_rect), show(drawn)));
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Landing: reorder, insert-above and remove-focused keep the focused identity (removal falls to
/// a placeable element); on the frame the landing becomes visible every card but the focused one
/// is at rest (the tile now at the old index lets go of nothing), and a focused element that
/// stayed the same item has not moved on screen, and the scroll stays within range on the landing's
/// frame and every one after it until rest.
pub fn landing(mount: Mount) -> Outcome {
    let mut bad = Vec::new();
    for l in [Landing::Reorder, Landing::InsertAbove, Landing::RemoveFocused] {
        let mut h = mount(8);
        let cards = h.cards();
        if cards.len() < 3 {
            return fail("fewer than 3 cards");
        }
        h.tick(2);
        h.focus(cards[1], By::Restore);
        h.tick(SETTLE);
        let before = h.identity(cards[1]);
        let rect_before = h.place(cards[1], At::Drawn).map(|p| p.rect);
        if let Err(why) = h.landing(l) {
            return Outcome::Unsupported(why);
        }
        h.tick(1);
        if let Some(now) = h.focused() {
            let lifted: Vec<u32> = h
                .cards()
                .into_iter()
                .filter(|&c| c != now && h.scale(c).is_some_and(|s| (s - 1.0).abs() > 0.001))
                .collect();
            if !lifted.is_empty() {
                bad.push(format!("{l:?}: cards {lifted:?} are off rest on the frame after the landing"));
            }
            if l != Landing::RemoveFocused && h.identity(now) == before {
                if let (Some(was), Some(is)) = (rect_before, h.place(now, At::Drawn).map(|p| p.rect)) {
                    // a grid's column legitimately moves with the index; its row and size may not
                    let moved = !(rect_eq(Rect::new(0.0, was.y, was.w, was.h), Rect::new(0.0, is.y, is.w, is.h))
                        && (h.columns().is_some() || (was.x - is.x).abs() < 0.5));
                    if moved {
                        bad.push(format!("{l:?}: the focused tile moved on screen, {} -> {}", show(was), show(is)));
                    }
                }
            }
        }
        // the scroll never leaves its range: not on the frame of the landing, nor on any frame
        // until rest (a shift the row cannot honour clamps instead of overscrolling and gliding back)
        let outside = |h: &dyn CardHarness| {
            h.scroll().filter(|&sx| sx < -0.5 || h.scroll_max().is_some_and(|max| sx > max + 0.5))
        };
        let mut under = outside(&*h).map(|sx| (0, sx));
        for frame in 1..SETTLE {
            if under.is_some() {
                break;
            }
            h.tick(1);
            under = outside(&*h).map(|sx| (frame, sx));
        }
        if let Some((frame, sx)) = under {
            bad.push(format!("{l:?}: scrolled to {sx} (outside 0..={:?}) on frame {frame} after the landing", h.scroll_max()));
        }
        let Some(now) = h.focused() else {
            bad.push(format!("{l:?}: focus was lost"));
            continue;
        };
        if h.place(now, At::Drawn).is_none() {
            bad.push(format!("{l:?}: focus rests on an unplaceable element"));
            continue;
        }
        let identity = h.identity(now);
        match l {
            Landing::RemoveFocused if identity == before => bad.push(format!("{l:?}: the removed item is still focused")),
            Landing::Reorder | Landing::InsertAbove if identity != before => {
                bad.push(format!("{l:?}: focus moved from {before} to {identity}"))
            }
            _ => {}
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Memory: `memory_at` then a fresh instance and `RestoreMemory` gives the same focused identity.
pub fn memory(mount: Mount) -> Outcome {
    let mut h = mount(14);
    let cards = h.cards();
    if cards.len() < 4 {
        return fail("fewer than 4 cards");
    }
    h.tick(2);
    h.focus(cards[3], By::Dir);
    h.tick(SETTLE);
    let (id, scroll) = (h.identity(cards[3]), h.scroll());
    let mut fresh = match h.memory_roundtrip() {
        Ok(f) => f,
        Err(why) => return Outcome::Unsupported(why),
    };
    fresh.tick(SETTLE);
    let Some(now) = fresh.focused() else {
        return fail("the fresh instance has no focus after RestoreMemory");
    };
    let mut bad = Vec::new();
    if fresh.identity(now) != id {
        bad.push(format!("focus came back as {} not {id}", fresh.identity(now)));
    }
    if let (Some(a), Some(b)) = (scroll, fresh.scroll()) {
        if (a - b).abs() > 1.0 {
            bad.push(format!("scroll {b} != {a}"));
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Determinism and idle: the same script twice gives the same canon; motion is reported while a
/// pop runs and the screen goes quiet at rest.
pub fn determinism_idle(mount: Mount) -> Outcome {
    let run = || {
        let mut h = mount(8);
        let cards = h.cards();
        h.tick(2);
        h.focus(cards[0], By::Restore);
        h.tick(30);
        h.focus(cards[1], By::Dir);
        h.tick(20);
        h.cover();
        h.tick(3);
        h.uncover();
        h.tick(SETTLE);
        h.canon()
    };
    if run() != run() {
        return fail("the same event script produced different canon");
    }
    let (mut h, cards) = match settled_first(mount) {
        Ok(v) => v,
        Err(o) => return o,
    };
    h.focus(cards[1], By::Dir);
    if !h.tick(1) {
        return fail("no motion reported on the first frame of a pop");
    }
    h.tick(SETTLE);
    if h.tick(1) {
        return fail("motion still reported after the pop settled");
    }
    Outcome::Pass
}

/// The requirement under every landing rule: focus never moves under the user, in any
/// interleaving. Cards arrive before every card shown ([`CardHarness::shift`]) between the same two
/// ticks as the engine hands focus on, by every cause (a key, the pointer, a restore, a reconcile
/// naming another card, a reconcile naming the same card), the focus change first or the shift
/// first. A card that stayed must stay where it is drawn, and the focused one must end up drawn.
/// Not one of [`CASES`]: a screen whose harness cannot shift reports `Unsupported`.
pub fn shifted_under_a_focus_change(mount: Mount) -> Outcome {
    let mut bad = Vec::new();
    for (by, same) in [(By::Dir, false), (By::Pointer, false), (By::Restore, false), (By::Reconcile, false), (By::Reconcile, true)] {
        for focus_first in [true, false] {
            let mut h = mount(40);
            h.tick(2);
            let cards = h.cards();
            let cols = h.columns().unwrap_or(1);
            let at = if cols > 1 { 2 * cols + 1 } else { 20 };
            if cards.len() <= at + 1 {
                return fail(format!("mounted with 40 items but exposes {} cards", cards.len()));
            }
            h.focus(cards[at], By::Restore);
            h.tick(SETTLE);
            let (stayed, target) = (cards[at - 1], if same { cards[at] } else { cards[at + 1] });
            let Some(before) = h.drawn_rect(stayed, 1.0) else {
                return Outcome::Unsupported("the screen exposes no drawn rect");
            };
            let why = format!("{by:?} same={same} focus_first={focus_first}");
            if focus_first {
                h.focus(target, by);
            }
            // whole rows for a grid (its columns hold), half a window for a strip
            if let Err(why) = h.shift(if cols > 1 { cols * 3 } else { 12 }) {
                return Outcome::Unsupported(why);
            }
            if !focus_first {
                h.focus(target, by);
            }
            h.tick(1);
            match h.drawn_rect(stayed, 1.0) {
                Some(now) if (now.x - before.x).abs() < 100.0 && (now.y - before.y).abs() < 100.0 => {}
                now => bad.push(format!("{why}: a card that stayed went from {} to {}", show(before), now.map_or("nowhere".into(), show))),
            }
            h.tick(SETTLE);
            let on = h.place(target, At::Drawn).map(|p| p.rect);
            if !on.is_some_and(|r| r.cx() > 0.0 && r.cx() < crate::consts::SCR_W && r.cy() > 0.0 && r.cy() < crate::consts::SCR_H) {
                bad.push(format!("{why}: the focused card ended at {}", on.map_or("nowhere".into(), show)));
            }
        }
    }
    if bad.is_empty() { Outcome::Pass } else { fail(bad.join("; ")) }
}

/// Run every case on every screen of `table`, return the matrix as `(screen, case, outcome)`.
/// Takes `testlock::serial()` for the whole run: every harness reaches thread-local focus, press
/// and idle state.
pub fn run_all(table: &[(&'static str, Mount)]) -> Vec<(&'static str, &'static str, Outcome)> {
    let _guard = plx_base::testlock::serial();
    let mut out = Vec::new();
    for &(screen, mount) in table {
        for (name, case) in CASES {
            out.push((screen, name, case(mount)));
        }
    }
    out
}

/// Compare a matrix with the expected-failure list (`screen case  # reason` per line). Returns
/// the complaints: an unlisted failure, a listed case that now passes, a listed case that is
/// unsupported, a malformed or duplicate line.
pub fn check_expected(matrix: &[(&'static str, &'static str, Outcome)], list: &str) -> Vec<String> {
    let mut complaints = Vec::new();
    let mut listed: Vec<(String, String)> = Vec::new();
    for line in list.lines() {
        let body = line.trim();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        // `<source file>\t<screen> <case>\t# <reason>`
        let mut fields = body.splitn(3, '\t');
        let (_file, keys, reason) = (fields.next(), fields.next().unwrap_or(""), fields.next().unwrap_or(""));
        let mut it = keys.split_whitespace();
        match (it.next(), it.next(), it.next()) {
            (Some(s), Some(c), None) if !reason.trim().is_empty() => {
                if listed.iter().any(|(a, b)| a == s && b == c) {
                    complaints.push(format!("duplicate line: {s} {c}"));
                }
                listed.push((s.into(), c.into()));
            }
            _ => complaints.push(format!("malformed line (want `screen case  # reason`): {body}")),
        }
    }
    for (s, c) in &listed {
        match matrix.iter().find(|(ms, mc, _)| ms == s && mc == c) {
            None => complaints.push(format!("{s} {c}: listed but not in the matrix; remove it from the list")),
            Some((_, _, Outcome::Pass)) => complaints.push(format!("{s} {c}: now PASSES; remove it from ci/allow/cards-conformance.txt")),
            Some((_, _, Outcome::Unsupported(why))) => complaints.push(format!("{s} {c}: is unsupported ({why}), not failing; remove it from the list")),
            Some((_, _, Outcome::Fail(_))) => {}
        }
    }
    for (s, c, o) in matrix {
        if let Outcome::Fail(msg) = o {
            if !listed.iter().any(|(ls, lc)| ls == s && lc == c) {
                complaints.push(format!("{s} {c}: FAILS and is not listed: {msg}"));
            }
        }
    }
    complaints
}
