//! **I4, the z-order census** (`docs/hud-plugins.md`, "Invariants", I4).
//!
//! Same-`z` interactive rects never overlap; every item hit-tests back to itself (generalising
//! `every_disc_hit_tests_back_to_itself`); a lower-`z` group may underlie a higher one (the scrub grab
//! band); decor of every kind never overlaps a drawn interactive rect, except a `rect` ground under its own
//! cells and a strip card's own overlay and below content.
//!
//! The contract every check module keeps: ONE entry, `pub(super) fn check(cx: &LangCtx<'_>, out: &mut
//! Vec<Finding>)`, called once per language after the package compiled; it walks every state and pushes a
//! finding for each violation, built with `Finding::new(&codes::..)` and `.with(ctx)` where `ctx` starts
//! from `lint::ctx_for` and adds its `rects` (`lint::named`). The pointer is the node's
//! (`CompiledState::origin(op)` / `control_pointer(control)`). Pure: no IO, no globals.
//!
//! # The model ([`surfaces`])
//!
//! A compiled state is read once into [`Surfaces`], which `lint_safe` (I5) reads too, so both invariants
//! see the same rects:
//! - [`Target`]s, the interactive things in DRAW order: every control; the scrub bar (drawn at the track,
//!   hit at the grab band, which is the LOWEST stop whatever its band's `z`, F8 and #162, so the row and
//!   the tabs may stand over it); Up Next's two buttons and the Skip pill (at the `z` of the band the
//!   stand-in lives in); the chapters strip's card row (its viewport by the card's height: a card past the
//!   viewport is culled, so it is not a rect).
//! - [`Content`], the decor: ground, text, image, icon, rule and facts ops, the playbar's two clocks (app
//!   text the playbar draws on the clock line under the track, from `DrawOp::Playbar::clock_rects`; under
//!   `follow_head` the elapsed clock's rect is the whole track, as it can be anywhere along it), Up Next's
//!   still and caption, and every op of a strip card's template (each remembers the strip it belongs to).
//!
//! A control's `z` is its band's (`z`, default 0). The scrim is NOT decor here: it is the ground behind
//! the whole HUD and lies under every control by construction (I4 lists as decor text, images, icons,
//! rules, Up Next's still and caption, and exempts the `rect` grounds under their own cells).
//!
//! # The three checks
//!
//! - **`HUD-E1001` z-overlap.** Two targets of the same `z` whose drawn rects (or hit rects of the same
//!   hit layer) overlap by more than a hair. Touching edges are not an overlap. Pointer: the LATER
//!   target's.
//! - **`HUD-E1002` hit-self.** The topmost target at a press on the centre of each target's hit rect (and
//!   of its drawn rect, where that differs: the scrub bar's visible track) must be that target. Topmost is
//!   the highest layer, then the later in draw order, as the focus map stacks stops. So a lower-`z` group
//!   may underlie a higher one, provided no press at its centre lands on the higher one.
//! - **`HUD-E1003` decor-overlap.** A decor rect overlaps a target's DRAWN rect by more than a hair. Two
//!   exceptions, both said by I4: a `rect` ground underlies its own cells (a target whose pointer lies
//!   under the ground's node; Up Next's buttons and the Skip pill stand where the segments
//!   `up_next.replaces` names stand, so they are under that segment's ground too), and a strip card's own
//!   overlay and below content belongs to its card (the template op's origin lies under the strip's node:
//!   it is checked against every OTHER target).
//!
//! A Skip state is read in both layouts (the Intro pill with the layout as compiled, the Credits pill with
//! `skip_variant(Credits)`): the two labels have two widths. A fault is reported ONCE, by the first state that
//! shows it ([`dedupe`], shared with `lint_safe` and `lint_text`).

use plx_ui::consts::EDGE_TOL;
use plx_ui::Rect;

use super::compile::{CompiledState, DrawOp, SkipKind};
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};
use super::lint::{ctx_for, named, LangCtx, StateCtx};
use super::parse::{Package, Val};
use super::reach::Slot;
use super::validate::each_object;
use super::{codes, Context, Finding};

/// What kind of interactive thing a [`Target`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetKind {
    /// a placed control
    Control,
    /// the scrub bar: drawn at the track, hit at the grab band
    Scrub,
    /// one of Up Next's two buttons
    UpNext,
    /// the Skip pill
    Skip,
    /// the chapters strip's cards; sideways they may run past `SAFE` (I5)
    Cards,
}

/// One interactive thing of a state, with the rects it is drawn and hit at.
#[derive(Clone, Debug)]
pub(super) struct Target {
    pub kind: TargetKind,
    /// what a finding calls the drawn rect (a control's id, `scrub bar`, ...)
    pub name: String,
    /// what a finding calls the hit rect
    pub hit_name: String,
    /// the node that produced it
    pub pointer: String,
    pub drawn: Rect,
    pub hit: Rect,
    /// the layer the drawn rect lives on: its band's `z`
    pub z: i64,
    /// the layer the hit rect lives on: `z`, except the scrub grab band, the lowest stop of all
    pub hit_z: i64,
    /// Further nodes the target stands in besides its own `pointer`: a stand-in (Up Next's buttons, the Skip
    /// pill) is produced by the `up_next` object but takes the place of the segments `up_next.replaces` names,
    /// so a `rect` ground under one of those segments underlies it too. Empty for every other target.
    pub standing_in: Vec<String>,
}

impl Target {
    /// A target drawn and hit at one rect on layer `z`.
    pub(super) fn flat(
        kind: TargetKind,
        name: impl Into<String>,
        pointer: impl Into<String>,
        rect: Rect,
        z: i64,
    ) -> Target {
        let name = name.into();
        Target {
            kind,
            hit_name: name.clone(),
            name,
            pointer: pointer.into(),
            drawn: rect,
            hit: rect,
            z,
            hit_z: z,
            standing_in: Vec::new(),
        }
    }

    /// This target as standing in the nodes at `pointers` too (see [`Target::standing_in`]).
    fn replacing(self, pointers: &[String]) -> Target {
        Target { standing_in: pointers.to_vec(), ..self }
    }
}

/// What a [`Content`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Ground,
    Text,
    Image,
    Icon,
    Rule,
    Facts,
}

/// The strip card a [`Content`] belongs to.
#[derive(Clone, Debug)]
pub(super) struct Card {
    /// the strip's node
    pub strip: String,
    /// the card's art itself (the image the strip's [`Target`] already is)
    pub face: bool,
}

/// One piece of decor of a state.
#[derive(Clone, Debug)]
pub(super) struct Content {
    pub kind: Kind,
    /// what a finding calls the rect (`text title.main`, `icon eq`, `up_next still`)
    pub name: String,
    pub pointer: String,
    pub rect: Rect,
    /// `Some` when the op is part of a strip card's template
    pub card: Option<Card>,
}

/// A chapters strip of a state, for the reveal check of I5.
#[derive(Clone, Debug)]
pub(super) struct StripFacts {
    pub pointer: String,
    /// the strip's cell: the viewport the cards scroll in
    pub viewport: Rect,
    /// the focused card with its overlay and below content: the union of the art and every template op
    pub card: Rect,
}

/// One compiled layout under inspection: a state as compiled, and which Skip pill is the one shown.
pub(super) struct View<'a> {
    pub state: StateCtx<'a>,
    /// the layout as drawn under `skip`
    pub shown: &'a CompiledState,
    pub skip: SkipKind,
}

/// Everything a state draws that I4 and I5 read.
#[derive(Default)]
pub(super) struct Surfaces {
    /// the interactive things, in draw order
    pub targets: Vec<Target>,
    pub content: Vec<Content>,
    pub strips: Vec<StripFacts>,
}

/// Every layout of the package in this language: each state once, and a Skip state a second time under the
/// Credits pill.
pub(super) fn views<'a>(cx: &LangCtx<'a>) -> Vec<View<'a>> {
    let mut views = Vec::new();
    for state in cx.states() {
        views.push(View { state, shown: state.state, skip: SkipKind::Intro });
        if state.key.slot == Slot::Skip {
            views.push(View { state, shown: state.state.skip_variant(SkipKind::Credits), skip: SkipKind::Credits });
        }
    }
    views
}

/// Whether the node at `child` is `parent` or lies under it (RFC 6901: the next character is `/`).
pub(super) fn within(parent: &str, child: &str) -> bool {
    child == parent || child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))
}

/// The `z` of every band, found from the package: a compiled state keeps a control's band by id, not its
/// `z`; and where the stand-ins stand.
struct Bands<'p> {
    /// `(pointer, id, z)` of each band of `layout.block`
    list: Vec<(&'p str, &'p str, i64)>,
    /// the id of the band Up Next and Skip live in
    host: &'p str,
    /// the pointers of the segments `up_next.replaces` names: where the stand-ins take their place
    replaced: Vec<String>,
}

impl<'p> Bands<'p> {
    fn new(package: &'p Package) -> Bands<'p> {
        let layout = &package.layout;
        let list = match layout.get("block") {
            Some(Val::List(bands)) => bands
                .iter()
                .filter_map(Val::as_object)
                .map(|b| {
                    let id = b.get("id").and_then(Val::as_str).unwrap_or("");
                    (b.pointer.as_str(), id, b.get("z").and_then(Val::as_i64).unwrap_or(0))
                })
                .collect(),
            _ => Vec::new(),
        };
        let host = layout
            .get("up_next")
            .and_then(Val::as_object)
            .and_then(|u| u.get("band"))
            .and_then(Val::as_str)
            .unwrap_or("");
        let names: Vec<&str> = layout
            .get("up_next")
            .and_then(Val::as_object)
            .and_then(|u| u.get("replaces"))
            .and_then(Val::as_list)
            .map(|ids| ids.iter().filter_map(Val::as_str).collect())
            .unwrap_or_default();
        let mut replaced = Vec::new();
        each_object(layout, &mut |o| {
            if o.node == "segment" && o.get("id").and_then(Val::as_str).is_some_and(|id| names.contains(&id)) {
                replaced.push(o.pointer.clone());
            }
        });
        Bands { list, host, replaced }
    }

    /// The `z` of the band holding the node at `pointer`.
    fn z_of(&self, pointer: &str) -> i64 {
        self.list.iter().find(|(band, _, _)| within(band, pointer)).map_or(0, |&(_, _, z)| z)
    }

    /// The `z` of the band a stand-in (Up Next, Skip) lives in.
    fn standin_z(&self) -> i64 {
        self.list.iter().find(|&&(_, id, _)| id == self.host).map_or(0, |&(_, _, z)| z)
    }
}

/// The decor an op draws, with the name a finding calls it.
fn content_of(op: &DrawOp) -> Option<(Kind, Rect, String)> {
    Some(match op {
        DrawOp::Ground { rect, .. } => (Kind::Ground, *rect, "ground".to_owned()),
        DrawOp::Text { rect, bind, .. } => (Kind::Text, *rect, format!("text {bind}")),
        DrawOp::Image { rect, bind, .. } => (Kind::Image, *rect, format!("image {bind}")),
        DrawOp::Icon { rect, asset, .. } => (Kind::Icon, *rect, format!("icon {asset}")),
        DrawOp::Rule { rect, .. } => (Kind::Rule, *rect, "rule".to_owned()),
        DrawOp::Facts { rect, .. } => (Kind::Facts, *rect, "facts".to_owned()),
        _ => return None,
    })
}

/// Read `view` into its targets (draw order), decor and strips.
pub(super) fn surfaces(cx: &LangCtx<'_>, view: &View<'_>) -> Surfaces {
    let bands = Bands::new(cx.package);
    let state = view.shown;
    let mut s = Surfaces::default();
    let mut control = 0;
    for (i, op) in state.ops.iter().enumerate() {
        let origin = state.origin(i);
        if let Some((kind, rect, name)) = content_of(op) {
            s.content.push(Content { kind, name, pointer: origin.to_owned(), rect, card: None });
            continue;
        }
        match op {
            DrawOp::Control(c) => {
                let pointer = state.control_pointer(control);
                control += 1;
                s.targets.push(Target::flat(TargetKind::Control, c.id.clone(), pointer, c.rect, bands.z_of(pointer)));
            }
            DrawOp::Playbar { track, grab, clock_rects, .. } => {
                s.targets.push(Target {
                    kind: TargetKind::Scrub,
                    name: "scrub bar".to_owned(),
                    hit_name: "scrub grab".to_owned(),
                    pointer: origin.to_owned(),
                    drawn: *track,
                    hit: *grab,
                    z: bands.z_of(origin),
                    hit_z: i64::MIN,
                    standing_in: Vec::new(),
                });
                // The two clocks are app text the playbar draws itself (`clocks`): decor like any text, so a
                // control under the bar cannot be drawn over them.
                for (name, rect) in [("playbar elapsed clock", clock_rects.elapsed), ("playbar remaining clock", clock_rects.remaining)] {
                    s.content.push(Content { kind: Kind::Text, name: name.to_owned(), pointer: origin.to_owned(), rect, card: None });
                }
            }
            DrawOp::UpNext { rects, .. } => {
                let z = bands.standin_z();
                let stand = |name, rect| Target::flat(TargetKind::UpNext, name, origin, rect, z).replacing(&bands.replaced);
                s.targets.push(stand("up_next credits", rects.credits));
                s.targets.push(stand("up_next next", rects.next));
                if let Some(rect) = rects.still {
                    let (kind, name) = (Kind::Image, "up_next still".to_owned());
                    s.content.push(Content { kind, name, pointer: origin.to_owned(), rect, card: None });
                }
                if let Some(rect) = rects.caption {
                    let (kind, name) = (Kind::Text, "up_next caption".to_owned());
                    s.content.push(Content { kind, name, pointer: origin.to_owned(), rect, card: None });
                }
            }
            DrawOp::SkipPill { rects } => {
                let name = match view.skip {
                    SkipKind::Intro => "skip pill (intro)",
                    SkipKind::Credits => "skip pill (credits)",
                };
                let pill = rects[view.skip as usize];
                let pill = Target::flat(TargetKind::Skip, name, origin, pill, bands.standin_z());
                s.targets.push(pill.replacing(&bands.replaced));
            }
            DrawOp::Strip { rect, card, template, .. } => {
                let row = Rect::new(rect.x, card.y, rect.w, card.h);
                s.targets.push(Target::flat(TargetKind::Cards, "chapter cards", origin, row, bands.z_of(origin)));
                let mut whole = *card;
                for (n, card_op) in template.iter().enumerate() {
                    let Some((kind, rect, name)) = content_of(&card_op.op) else { continue };
                    whole = whole.union(rect);
                    let face = n == 0 && kind == Kind::Image && rect == *card;
                    let card = Some(Card { strip: origin.to_owned(), face });
                    s.content.push(Content { kind, name, pointer: card_op.origin.clone(), rect, card });
                }
                s.strips.push(StripFacts { pointer: origin.to_owned(), viewport: *rect, card: whole });
            }
            // The scrim is the ground behind the whole HUD (I4 exempts it); the rest draws no rect I4 reads.
            _ => {}
        }
    }
    s
}

/// Whether `a` and `b` overlap by more than [`EDGE_TOL`] on both axes (edges that touch are not an overlap).
pub(super) fn overlap(a: Rect, b: Rect) -> bool {
    let both = a.intersect(b);
    both.w > EDGE_TOL && both.h > EDGE_TOL
}

/// A pair of targets, the earlier and the later in draw order, whose rects collide on one layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Collision {
    pub earlier: usize,
    pub later: usize,
    /// the HIT rects collide (and the drawn ones do not)
    pub hit: bool,
}

/// Every pair of targets whose drawn rects overlap on the same `z`, or whose hit rects overlap in the same
/// hit layer. A pair is reported once, as a drawn collision when both are.
pub(super) fn same_layer_overlaps(targets: &[Target]) -> Vec<Collision> {
    let mut out = Vec::new();
    for (earlier, a) in targets.iter().enumerate() {
        for (later, b) in targets.iter().enumerate().skip(earlier + 1) {
            let drawn = a.z == b.z && overlap(a.drawn, b.drawn);
            let hit = a.hit_z == b.hit_z && overlap(a.hit, b.hit);
            if drawn || hit {
                out.push(Collision { earlier, later, hit: !drawn });
            }
        }
    }
    out
}

/// The target a press at `(x, y)` lands on: the highest hit layer, then the later in draw order.
pub(super) fn topmost(targets: &[Target], x: f32, y: f32) -> Option<usize> {
    targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.hit.contains(x, y))
        .max_by_key(|&(i, t)| (t.hit_z, i))
        .map(|(i, _)| i)
}

/// A target a press at its centre does not reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Miss {
    pub target: usize,
    /// what the press lands on instead, `None` for nothing
    pub winner: Option<usize>,
}

/// Every target whose hit rect's centre, or whose drawn rect's centre (what a viewer sees and presses),
/// hit-tests to something else.
pub(super) fn hit_misses(targets: &[Target]) -> Vec<Miss> {
    let mut out = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let probes = [(t.hit.cx(), t.hit.cy()), (t.drawn.cx(), t.drawn.cy())];
        let lost = probes.iter().map(|&(x, y)| topmost(targets, x, y)).find(|&top| top != Some(i));
        if let Some(winner) = lost {
            out.push(Miss { target: i, winner });
        }
    }
    out
}

/// A decor rect over a target's drawn rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Cover {
    pub content: usize,
    pub target: usize,
}

/// Every decor rect that overlaps a target's drawn rect, less the two exceptions of I4: a ground over the
/// targets that live under its own node (a stand-in lives where the segment it replaces does), and a strip
/// card's own content over that strip.
pub(super) fn decor_overlaps(targets: &[Target], content: &[Content]) -> Vec<Cover> {
    let mut out = Vec::new();
    for (c, decor) in content.iter().enumerate() {
        for (t, target) in targets.iter().enumerate() {
            let lives_under = |ground: &str| {
                within(ground, &target.pointer) || target.standing_in.iter().any(|home| within(ground, home))
            };
            let own_ground = decor.kind == Kind::Ground && lives_under(&decor.pointer);
            let own_card = decor
                .card
                .as_ref()
                .is_some_and(|card| target.kind == TargetKind::Cards && card.strip == target.pointer);
            if !(own_ground || own_card) && overlap(decor.rect, target.drawn) {
                out.push(Cover { content: c, target: t });
            }
        }
    }
    out
}

/// Whether `b` is the fault `a` is, found again in another state, slot or Skip layout: the same code, language,
/// pointer, message and text fact, and rects that agree on `x` and `w` (their names, which a layout may spell
/// differently, do not count). Where a fault was found (`state`, `slot`) is not a fact of the fault, and nor is
/// how high it stood there: the bands are bottom-anchored, so opening a section lifts every band above it and
/// a fault keeps its `x` and its width while its `y` moves.
pub(super) fn same_fault(a: &Finding, b: &Finding) -> bool {
    a.code == b.code
        && a.ctx.lang == b.ctx.lang
        && a.pointer == b.pointer
        && a.message == b.message
        && a.ctx.text == b.ctx.text
        && a.ctx.rects.len() == b.ctx.rects.len()
        && a.ctx.rects.iter().zip(&b.ctx.rects).all(|(p, q)| (p.x, p.w) == (q.x, q.w))
}

/// **One finding per fault.** Drop the findings of `out[from..]` that repeat an earlier one of that tail
/// ([`same_fault`]): the first state (in `Compiled::states()` order) that shows a fault names it, with its
/// rects, and the later states that show it again add nothing, so one round of fixes reads each fault once
/// however many states and slots draw it. `lint_text`, `lint_zorder` and `lint_safe` all end their `check` with
/// it (`lint_size` and `lint_menus` keep their own once-per-control and once-per-opener reports).
pub(super) fn dedupe(out: &mut Vec<Finding>, from: usize) {
    let mut kept: Vec<Finding> = Vec::new();
    for finding in out.drain(from..) {
        if !kept.iter().any(|seen| same_fault(seen, &finding)) {
            kept.push(finding);
        }
    }
    out.extend(kept);
}

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let from = out.len();
    for view in views(cx) {
        let s = surfaces(cx, &view);
        let at = |rects| Context { rects, ..ctx_for(cx, &view.state) };
        for c in same_layer_overlaps(&s.targets) {
            let (a, b) = (&s.targets[c.earlier], &s.targets[c.later]);
            let ((an, ar), (bn, br)) = if c.hit {
                ((&a.hit_name, a.hit), (&b.hit_name, b.hit))
            } else {
                ((&a.name, a.drawn), (&b.name, b.drawn))
            };
            out.push(
                Finding::new(
                    &codes::E1001,
                    b.pointer.clone(),
                    format!("`{an}` and `{bn}` are interactive rects of the same z and they overlap"),
                    "Space them apart (a larger gap, a smaller size), or put one band at a different `z`: only the scrub grab band may lie under another rect.",
                )
                .with(at(vec![named(an, ar), named(bn, br)])),
            );
        }
        for miss in hit_misses(&s.targets) {
            let t = &s.targets[miss.target];
            let (message, rects) = match miss.winner.map(|w| &s.targets[w]) {
                Some(w) => (
                    format!("a press on the centre of `{}` lands on `{}`", t.name, w.name),
                    vec![named(&t.hit_name, t.hit), named(&w.name, w.hit)],
                ),
                None => (
                    format!("a press on the centre of `{}` lands on nothing", t.name),
                    vec![named(&t.hit_name, t.hit)],
                ),
            };
            out.push(
                Finding::new(
                    &codes::E1002,
                    t.pointer.clone(),
                    message,
                    "Keep the rect clear of whatever stands over it, or give it the higher `z`: every item must hit-test back to itself.",
                )
                .with(at(rects)),
            );
        }
        for cover in decor_overlaps(&s.targets, &s.content) {
            let (d, t) = (&s.content[cover.content], &s.targets[cover.target]);
            out.push(
                Finding::new(
                    &codes::E1003,
                    d.pointer.clone(),
                    format!("`{}` overlaps the drawn rect of `{}`", d.name, t.name),
                    "Move the content off the control (a narrower `w`, a gap), or give the control a band with room: decor never covers an interactive rect.",
                )
                .with(at(vec![named(&d.name, d.rect), named(&t.name, t.drawn)])),
            );
        }
    }
    dedupe(out, from);
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[super::lint::testing::InvalidCase] = &[
    // The reference's centre segment (z 1) spread so its last item, `fwd`, reaches the right segment's
    // first, `chapters`: 184 px between six items centred on 960 ends at 1582, 2 px into `chapters`.
    // Neither centre lies in the other rect, so every item still hits itself.
    InvalidCase {
        name: "z-overlap: the corners of two same-z controls meet",
        code: "HUD-E1001",
        pointer: "/layout/block/2/cells/1/segment/items/0",
        base: Base::Reference,
        edits: &[(r#""id": "center", "gap": "LG""#, r#""id": "center", "gap": 184"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The same spread at 200 px: `fwd` and `chapters` now overlap by 42 px, and `fwd`'s centre (1596) lies in
    // `chapters` (1580..1632), the later in draw order: a press at `fwd`'s centre lands on `chapters`.
    InvalidCase {
        name: "hit-self: a press at a control's centre lands on a later control",
        code: "HUD-E1002",
        pointer: "/layout/block/2/cells/0/segment/items/5",
        base: Base::Reference,
        edits: &[(r#""id": "center", "gap": "LG""#, r#""id": "center", "gap": 200"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[("HUD-E1001", "/layout/block/2/cells/1/segment/items/0")],
    },
    // Classic's Info still stretched to 1600 px: it runs under the Info card's two buttons (the ground under
    // them is exempt, the image over them is not).
    InvalidCase {
        name: "decor-overlap: an image runs under a button",
        code: "HUD-E1003",
        pointer: "/layout/block/3/cells/0/rect/cells/0/image",
        base: Base::Classic,
        edits: &[(r#""w": 320, "h": 180"#, r#""w": 1600, "h": 180"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The reference's bar band given a second cell, and the playbar a fixed `w` of 1648 so the two overlay (a
    // `fill` playbar would take the room left of the new cell): a 40 px disc, end-aligned and seated on the
    // band's foot. The bar's cell is 60 px (the track's top to the foot of its clock line), so the disc stands
    // on the clock line at the end of the track, where the remaining clock is drawn: the stage 1e defect, where
    // the reference's More disc was drawn over `-52:33`. The clock is app text (I4 decor), so the disc is drawn
    // over it (E1003), and the disc leaves it none of its room (I10, E0601).
    InvalidCase {
        name: "decor-overlap: a disc stands on the playbar's remaining clock",
        code: "HUD-E1003",
        pointer: "/layout/block/1/cells/0/playbar",
        base: Base::Reference,
        edits: &[
            (r#""playbar": { "w": "fill", "fill": "gold""#, r#""playbar": { "w": 1648, "fill": "gold""#),
            (
                r#""knob": "always", "clocks": "ends", "clock_size": "CAPTION", "mark": "busy_only" } }"#,
                r#""knob": "always", "clocks": "ends", "clock_size": "CAPTION", "mark": "busy_only" } },
        { "align": "end", "valign": "bottom", "segment": { "id": "tail", "gap": "MD", "items": [
          { "id": "tail_skip", "command": "seek", "by": 60, "widget": "disc", "size": 40,
            "rest": "bare", "focus": "accent", "face": "fwd30" }
        ] } }"#,
            ),
        ],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[("HUD-E0601", "/layout/block/1/cells/0/playbar")],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;

    use super::super::lint::testing::{apply, assert_case, Base};
    use super::super::lint::{lint, LintLang};
    use super::INVALID;

    /// The codes this module reports.
    const CODES: [&str; 3] = ["HUD-E1001", "HUD-E1002", "HUD-E1003"];

    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(!INVALID.is_empty());
        for case in INVALID {
            assert_case(case);
        }
    }

    #[test]
    fn classic_and_reference_have_no_findings_of_this_module_in_any_shipped_language() {
        let langs = LintLang::shipped();
        assert_eq!(langs.len(), 11);
        for base in [Base::Classic, Base::Reference] {
            let found: Vec<_> = lint(base.text(), &ShippedMeasure, &langs)
                .into_iter()
                .filter(|f| CODES.contains(&f.code))
                .collect();
            assert!(found.is_empty(), "{base:?}: {found:#?}");
        }
    }

    // ---- the predicates, on synthetic rects (no package)

    use plx_ui::Rect;

    use super::super::lint::named;
    use super::super::{codes, Finding};
    use super::{
        decor_overlaps, dedupe, hit_misses, overlap, same_fault, same_layer_overlaps, topmost, within, Card, Collision,
        Content, Cover, Kind, Miss, Target, TargetKind,
    };

    fn control(name: &str, pointer: &str, rect: Rect, z: i64) -> Target {
        Target::flat(TargetKind::Control, name, pointer, rect, z)
    }

    fn decor(kind: Kind, pointer: &str, rect: Rect) -> Content {
        Content { kind, name: pointer.to_owned(), pointer: pointer.to_owned(), rect, card: None }
    }

    #[test]
    fn rects_overlap_only_by_more_than_a_hair_on_both_axes() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(!overlap(a, Rect::new(100.0, 0.0, 50.0, 100.0)), "touching edges");
        assert!(!overlap(a, Rect::new(0.0, 100.0, 100.0, 50.0)), "touching edges, vertical");
        assert!(!overlap(a, Rect::new(99.995, 0.0, 50.0, 100.0)), "a hair of float noise");
        assert!(!overlap(a, Rect::new(100.0, 100.0, 50.0, 50.0)), "touching corners");
        assert!(overlap(a, Rect::new(99.0, 0.0, 50.0, 100.0)), "one pixel in");
        assert!(overlap(a, Rect::new(25.0, 25.0, 10.0, 10.0)), "contained");
        assert!(overlap(Rect::new(25.0, 25.0, 10.0, 10.0), a), "containing");
        assert!(!overlap(a, Rect::new(300.0, 300.0, 10.0, 10.0)), "apart");
    }

    #[test]
    fn nodes_lie_within_a_pointer_only_on_a_segment_boundary() {
        assert!(within("/layout/block/1", "/layout/block/1"));
        assert!(within("/layout/block/1", "/layout/block/1/cells/0"));
        assert!(!within("/layout/block/1", "/layout/block/10"));
        assert!(!within("/layout/block/1", "/layout/block"));
    }

    #[test]
    fn same_layer_targets_collide_but_a_lower_layer_may_underlie() {
        let a = control("a", "/a", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        let b = control("b", "/b", Rect::new(90.0, 0.0, 100.0, 100.0), 0);
        assert_eq!(same_layer_overlaps(&[a.clone(), b.clone()]), [Collision { earlier: 0, later: 1, hit: false }]);
        let apart = control("b", "/b", Rect::new(100.0, 0.0, 100.0, 100.0), 0);
        assert!(same_layer_overlaps(&[a.clone(), apart]).is_empty(), "touching edges");
        let above = control("b", "/b", Rect::new(90.0, 0.0, 100.0, 100.0), 1);
        assert!(same_layer_overlaps(&[a, above]).is_empty(), "another z");
    }

    #[test]
    fn the_scrub_grab_band_is_the_lowest_layer_so_a_row_may_stand_over_it() {
        let bar = Target {
            kind: TargetKind::Scrub,
            name: "scrub bar".to_owned(),
            hit_name: "scrub grab".to_owned(),
            pointer: "/bar".to_owned(),
            drawn: Rect::new(0.0, 100.0, 1000.0, 8.0),
            hit: Rect::new(0.0, 28.0, 1000.0, 200.0),
            z: 0,
            hit_z: i64::MIN,
            standing_in: Vec::new(),
        };
        // A control row standing over the grab band, clear of the drawn track.
        let row = control("row", "/row", Rect::new(400.0, 140.0, 100.0, 60.0), 0);
        assert!(same_layer_overlaps(&[bar.clone(), row.clone()]).is_empty(), "grab lies on its own layer");
        assert!(hit_misses(&[bar.clone(), row.clone()]).is_empty(), "both centres still land on their owner");
        // A second grab-like target on the lowest layer collides with it by hit rect.
        let other = Target { name: "other".to_owned(), pointer: "/other".to_owned(), ..bar.clone() };
        let other = Target { hit: Rect::new(500.0, 28.0, 1000.0, 200.0), drawn: Rect::new(2000.0, 0.0, 8.0, 8.0), ..other };
        assert_eq!(same_layer_overlaps(&[bar, other]), [Collision { earlier: 0, later: 1, hit: true }]);
    }

    #[test]
    fn the_topmost_target_is_the_highest_layer_then_the_later_one() {
        let low = control("low", "/low", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        let high = control("high", "/high", Rect::new(50.0, 0.0, 100.0, 100.0), 1);
        let later = control("later", "/later", Rect::new(50.0, 0.0, 100.0, 100.0), 0);
        assert_eq!(topmost(&[low.clone(), high.clone()], 75.0, 50.0), Some(1));
        assert_eq!(topmost(&[high.clone(), low.clone()], 75.0, 50.0), Some(0), "the layer outranks the order");
        assert_eq!(topmost(&[low.clone(), later], 75.0, 50.0), Some(1), "same layer: the later is on top");
        assert_eq!(topmost(&[low, high], 20.0, 50.0), Some(0), "outside the upper rect");
        assert_eq!(topmost(&[], 0.0, 0.0), None);
    }

    #[test]
    fn a_lower_z_item_whose_centre_is_covered_by_a_higher_one_does_not_hit_itself() {
        let low = control("low", "/low", Rect::new(0.0, 0.0, 100.0, 100.0), 0);
        let over_centre = control("high", "/high", Rect::new(40.0, 40.0, 100.0, 100.0), 1);
        assert_eq!(hit_misses(&[low.clone(), over_centre]), [Miss { target: 0, winner: Some(1) }]);
        let over_corner = control("high", "/high", Rect::new(70.0, 70.0, 100.0, 100.0), 1);
        assert!(hit_misses(&[low.clone(), over_corner]).is_empty(), "a lower layer may underlie a higher one");
        // A lower layer below `low` that reaches under it but keeps its own centre clear.
        let under = control("under", "/under", Rect::new(60.0, 60.0, 200.0, 200.0), -1);
        assert!(hit_misses(&[low.clone(), under]).is_empty(), "the lower one underlies, its centre is clear");
        let buried = control("buried", "/buried", Rect::new(40.0, 40.0, 100.0, 100.0), -1);
        assert_eq!(hit_misses(&[low, buried]), [Miss { target: 1, winner: Some(0) }], "its centre lies under `low`");
    }

    #[test]
    fn a_target_whose_hit_rect_lies_off_its_drawn_centre_is_tested_at_both() {
        // Drawn at the left, hit at the right; a control stands over the drawn rect's centre.
        let split = Target {
            hit: Rect::new(200.0, 0.0, 100.0, 100.0),
            ..control("split", "/split", Rect::new(0.0, 0.0, 100.0, 100.0), 0)
        };
        let cover = control("cover", "/cover", Rect::new(40.0, 40.0, 20.0, 20.0), 0);
        assert_eq!(hit_misses(&[split, cover]), [Miss { target: 0, winner: Some(1) }]);
    }

    #[test]
    fn decor_over_a_drawn_interactive_rect_is_a_cover_unless_exempt() {
        let button = control("ok", "/layout/block/3/cells/0/rect/cells/1/control", Rect::new(100.0, 100.0, 80.0, 40.0), 0);
        let text = decor(Kind::Text, "/layout/block/3/cells/0/rect/cells/0/text", Rect::new(0.0, 100.0, 120.0, 40.0));
        assert_eq!(decor_overlaps(&[button.clone()], &[text]), [Cover { content: 0, target: 0 }]);
        let touching = decor(Kind::Text, "/t", Rect::new(0.0, 100.0, 100.0, 40.0));
        assert!(decor_overlaps(&[button.clone()], &[touching]).is_empty(), "touching edges");
        let ground = decor(Kind::Ground, "/layout/block/3/cells/0/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert!(decor_overlaps(&[button.clone()], &[ground]).is_empty(), "a ground under its own cells");
        let other = decor(Kind::Ground, "/layout/block/2/cells/0/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(decor_overlaps(&[button.clone()], &[other]), [Cover { content: 0, target: 0 }], "another band's ground");
        let image = decor(Kind::Image, "/layout/block/3/cells/0/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(decor_overlaps(&[button], &[image]), [Cover { content: 0, target: 0 }], "only a ground is exempt");
    }

    #[test]
    fn a_stand_in_lives_under_the_ground_of_the_segment_it_replaces() {
        let rect = Rect::new(100.0, 100.0, 80.0, 40.0);
        let mut next = Target::flat(TargetKind::UpNext, "up_next next", "/layout/up_next", rect, 0);
        let plate = decor(Kind::Ground, "/layout/block/0/cells/1/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(
            decor_overlaps(&[next.clone()], &[plate.clone()]),
            [Cover { content: 0, target: 0 }],
            "the `up_next` object itself is under no ground"
        );
        next.standing_in = vec!["/layout/block/0/cells/1/rect/cells/0/segment".to_owned()];
        assert!(decor_overlaps(&[next.clone()], &[plate]).is_empty(), "the plate under the replaced segment");
        let elsewhere = decor(Kind::Ground, "/layout/block/2/cells/0/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(
            decor_overlaps(&[next.clone()], &[elsewhere]),
            [Cover { content: 0, target: 0 }],
            "another band's ground"
        );
        let sibling = decor(Kind::Ground, "/layout/block/0/cells/1/rect/cells/1/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(
            decor_overlaps(&[next.clone()], &[sibling]),
            [Cover { content: 0, target: 0 }],
            "a ground beside the replaced segment, not around it"
        );
        let image = decor(Kind::Image, "/layout/block/0/cells/1/rect", Rect::new(0.0, 0.0, 400.0, 400.0));
        assert_eq!(decor_overlaps(&[next], &[image]), [Cover { content: 0, target: 0 }], "only a ground is exempt");
    }

    #[test]
    fn a_strip_cards_own_content_belongs_to_its_strip_only() {
        let cards = Target::flat(TargetKind::Cards, "cards", "/strip", Rect::new(0.0, 0.0, 800.0, 100.0), 0);
        let mut label = decor(Kind::Text, "/strip/card/0", Rect::new(10.0, 70.0, 100.0, 30.0));
        label.card = Some(Card { strip: "/strip".to_owned(), face: false });
        assert!(decor_overlaps(&[cards.clone()], &[label.clone()]).is_empty(), "the card's own overlay");
        let stranger = control("stranger", "/stranger", Rect::new(20.0, 60.0, 40.0, 40.0), 0);
        assert_eq!(
            decor_overlaps(&[cards.clone(), stranger.clone()], &[label.clone()]),
            [Cover { content: 0, target: 1 }],
            "it still may not cover another target"
        );
        label.card = Some(Card { strip: "/elsewhere".to_owned(), face: false });
        assert_eq!(decor_overlaps(&[cards], &[label]), [Cover { content: 0, target: 0 }], "another strip's content");
    }

    #[test]
    fn a_fault_found_again_in_another_state_is_dropped_whatever_the_state_the_slot_or_the_height() {
        let found = |state: &str, slot: &str, name: &str, rect: Rect, message: &str| {
            let mut f = Finding::new(&codes::E1003, "/a", message, "h");
            f.ctx.state = Some(state.to_owned());
            f.ctx.slot = Some(slot.to_owned());
            f.ctx.rects = vec![named(name, rect)];
            f
        };
        let rect = Rect::new(100.0, 700.0, 80.0, 40.0);
        let first = found("compact", "discs", "text", rect, "m");
        let lifted = Rect::new(100.0, 500.0, 80.0, 40.0);
        let again = found("chapters+info", "up_next", "text (credits layout)", lifted, "m");
        assert!(same_fault(&first, &again), "another state, slot, name and height, the same fault");
        assert!(!same_fault(&first, &found("compact", "discs", "text", Rect::new(110.0, 700.0, 80.0, 40.0), "m")), "x");
        assert!(!same_fault(&first, &found("compact", "discs", "text", Rect::new(100.0, 700.0, 90.0, 40.0), "m")), "w");
        assert!(!same_fault(&first, &found("compact", "discs", "text", rect, "other")), "another message");
        let mut elsewhere = first.clone();
        elsewhere.pointer = "/b".to_owned();
        assert!(!same_fault(&first, &elsewhere), "another node");
        let mut out = vec![first.clone(), again, found("info", "discs", "text", Rect::new(0.0, 0.0, 5.0, 5.0), "m")];
        dedupe(&mut out, 0);
        let states: Vec<_> = out.iter().map(|f| f.ctx.state.as_deref().unwrap()).collect();
        assert_eq!(states, ["compact", "info"], "the first state that shows a fault names it");
    }

    #[test]
    fn dedupe_drops_exact_repeats_of_the_tail_only() {
        let finding = |pointer: &str| Finding::new(&codes::E1001, pointer, "m", "h");
        let mut out = vec![finding("/a"), finding("/a")];
        let from = out.len();
        out.extend([finding("/b"), finding("/b"), finding("/a"), finding("/c")]);
        dedupe(&mut out, from);
        let pointers: Vec<_> = out.iter().map(|f| f.pointer.as_str()).collect();
        assert_eq!(pointers, ["/a", "/a", "/b", "/a", "/c"], "the head is not touched, the tail keeps first sightings");
    }

    // ---- the census reads what the layouts draw (so "no findings" above is not an empty census)

    use super::super::compile::{compile, CompileCtx};
    use super::super::lint::{LangCtx, LintOptions};
    use super::super::parse::parse;
    use super::super::reach::Slot;
    use super::{surfaces, views, Surfaces};

    /// Every layout of `base`'s English compile with its surfaces.
    fn census(base: Base) -> Vec<(Slot, Surfaces)> {
        census_of(base.text())
    }

    /// Every layout of the package in `text`, compiled in English, with its surfaces.
    fn census_of(text: &str) -> Vec<(Slot, Surfaces)> {
        let package = parse(text.as_bytes()).package.expect("the package parses");
        let lang = LintLang::for_tag("en").expect("English is shipped");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        let cx = LangCtx { package: &package, compiled: &compiled, measure: &ShippedMeasure, lang: &lang, opts: &opts };
        views(&cx).iter().map(|view| (view.state.key.slot, surfaces(&cx, view))).collect()
    }

    fn count(s: &Surfaces, kind: TargetKind) -> usize {
        s.targets.iter().filter(|t| t.kind == kind).count()
    }

    #[test]
    fn the_census_sees_the_controls_the_bar_the_cards_and_the_stand_ins_of_both_layouts() {
        for base in [Base::Classic, Base::Reference] {
            let all = census(base);
            assert!(!all.is_empty(), "{base:?}");
            let any = |f: &dyn Fn(&Surfaces) -> bool| all.iter().any(|(_, s)| f(s));
            assert!(any(&|s| count(s, TargetKind::Control) >= 7), "{base:?}: the control row");
            assert!(any(&|s| count(s, TargetKind::Scrub) == 1), "{base:?}: the bar");
            assert!(any(&|s| count(s, TargetKind::Cards) == 1 && !s.strips.is_empty()), "{base:?}: the chapter cards");
            assert!(any(&|s| s.content.iter().any(|c| c.card.is_some())), "{base:?}: a card's own content");
            assert!(any(&|s| s.content.iter().any(|c| c.kind == Kind::Text)), "{base:?}: text");
            for (slot, kind) in [(Slot::UpNext, TargetKind::UpNext), (Slot::Skip, TargetKind::Skip)] {
                let held: Vec<_> = all.iter().filter(|(s, _)| *s == slot).collect();
                assert!(!held.is_empty(), "{base:?}: {slot:?} states");
                assert!(held.iter().all(|(_, s)| count(s, kind) >= 1), "{base:?}: a {slot:?} state without its target");
            }
            let skips = all.iter().filter(|(s, _)| *s == Slot::Skip).count();
            let states = census_states(base);
            assert_eq!(skips, 2 * states, "{base:?}: each Skip state is read under both pills");
        }
    }

    /// How many Skip states `base` compiles to.
    fn census_states(base: Base) -> usize {
        let package = parse(base.text().as_bytes()).package.expect("the shipped package parses");
        let lang = LintLang::for_tag("en").expect("English is shipped");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        compiled.states().iter().filter(|s| s.key.slot == Slot::Skip).count()
    }

    // ---- a fault is one finding, however many states show it

    /// The findings of `code` that `text` yields in English, and how many distinct (pointer, message) pairs
    /// they make.
    fn in_en(text: &str, code: &str) -> (Vec<Finding>, usize) {
        let langs = [LintLang::for_tag("en").expect("shipped")];
        let found: Vec<_> = lint(text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code == code).collect();
        let faults: std::collections::BTreeSet<_> = found.iter().map(|f| (&f.pointer, &f.message)).collect();
        let distinct = faults.len();
        (found, distinct)
    }

    #[test]
    fn a_fault_in_every_state_is_one_finding_not_one_per_state_and_slot() {
        // The reference's centre row spread until `fwd` meets `chapters`: every state that places the row has it.
        let spread = [(r#""id": "center", "gap": "LG""#, r#""id": "center", "gap": 184"#)];
        let (found, distinct) = in_en(&apply(Base::Reference, &spread), "HUD-E1001");
        assert!(distinct >= 1, "the spread overlaps");
        assert_eq!(found.len(), distinct, "one E1001 per fault, not per state: {found:#?}");
        // The decor case: classic's Info still over the card's buttons shows in every state that opens Info.
        let wide = [(r#""w": 320, "h": 180"#, r#""w": 1600, "h": 180"#)];
        let (found, distinct) = in_en(&apply(Base::Classic, &wide), "HUD-E1003");
        assert!(distinct >= 1, "the still overlaps the buttons");
        assert_eq!(found.len(), distinct, "one E1003 per fault, not per state: {found:#?}");
    }

    // ---- a stand-in lives where the segment it replaces lives

    /// The `rect` ground classic's transport row would be drawn on: the node the row's segment lies under.
    const PLATE: &str = "/layout/block/0/cells/1/rect";

    /// Classic's transport row (the segment `up_next.replaces` names) on a `rect` plate, a natural design:
    /// the row becomes a cell of the plate, and so do the Up Next buttons and the Skip pill that take its place.
    const ROW_ON_A_PLATE: &[(&str, &str)] = &[
        (
            r#"{ "align": "end", "valign": "bottom", "segment": { "id": "row", "gap": 20, "items": ["#,
            r#"{ "align": "end", "valign": "bottom", "rect": { "ink": "PANEL_TOP", "pad": 8, "cells": [ { "align": "end", "segment": { "id": "row", "gap": 20, "items": ["#,
        ),
        (
            "\"focus\": \"accent\" }\n        ] } }\n      ] },\n      { \"id\": \"bar\"",
            "\"focus\": \"accent\" }\n        ] } } ] } }\n      ] },\n      { \"id\": \"bar\"",
        ),
    ];

    #[test]
    fn a_rect_plate_under_the_replaced_row_underlies_the_stand_ins_that_take_its_place() {
        let text = apply(Base::Classic, ROW_ON_A_PLATE);
        // The premise: in the Up Next and Skip states the stand-ins really lie on the plate, so a lint that
        // is silent has exempted something and not looked at nothing.
        let mut on_the_plate = 0;
        for (_, s) in census_of(&text) {
            let Some(ground) = s.content.iter().find(|c| c.kind == Kind::Ground && c.pointer == PLATE) else { continue };
            let standins = s.targets.iter().filter(|t| matches!(t.kind, TargetKind::UpNext | TargetKind::Skip));
            on_the_plate += standins.filter(|t| overlap(ground.rect, t.drawn)).count();
        }
        assert!(on_the_plate >= 3, "Up Next's two buttons and the Skip pill stand on the plate: {on_the_plate}");
        for tag in ["en", "ru"] {
            let langs = [LintLang::for_tag(tag).expect("shipped")];
            let found: Vec<_> = lint(&text, &ShippedMeasure, &langs)
                .into_iter()
                .filter(|f| f.code == codes::E1003.id)
                .collect();
            assert!(found.is_empty(), "{tag}: the plate underlies its own cells, stand-ins included: {found:#?}");
        }
    }
}
