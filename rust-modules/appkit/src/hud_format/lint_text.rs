//! **I10, app text fits** (`docs/hud-plugins.md`, "Text fit: the app is the enforcement point"; D10).
//!
//! App text (tab and button labels, clocks, the chapters fallback, the kicker's numbering) must fit, in every
//! language, inside the room it has: its box, the right edge of `SAFE`, the nearest control it would grow
//! into on its line and the right edge of the fixed-width `vstack` it lives in. Server text (titles, chapter names, the
//! synopsis) may elide or wrap and is never checked. A Skip state's credits layout is checked as well as the
//! intro one.
//!
//! **Which texts.** A compiled `DrawOp::Text` is app text by its `bind`, and the string it shows in THIS
//! language is the worst case of its shape, measured with the lint's `Measure` at the op's size in every
//! weight the op can draw (`Live<bool>` bold):
//! * `label.chapters`, `label.info`: the app's label, from [`LintLang::labels`](super::lint::LintLang).
//! * `item.start`: `fmt::clock`, `H:MM:SS` with two hour digits, each digit the widest of its position
//!   (measured, so a font whose digits differ in width is covered; no tabular-figure assumption). The clock
//!   is the same in every language, so a finding on it collapses to one with no `lang`.
//! * `item.title`: the app's numbered fallback `widgets_chapters_number(N)` when the server sent no chapter
//!   name, `N` three digits (the spec's PR12 corpus line: "`item.title`'s fallback
//!   `widgets_chapters_number(N)` up to 3 digits"). The Binding table says it "is app text and is drawn
//!   whole", so it is held to ONE line of room even when the cell has `max_lines` above 1.
//! * `title.kicker`, in a state where the item is an episode: D10, "The kicker mixes the two ("S2, E3 · "
//!   is the app's, the episode title the server's): only the server tail elides, and lint checks that the
//!   app prefix fits". The prefix is `core_episode_ordinal` plus the middle-dot separator, the season two
//!   digits and the episode three (no spec figure exists: a season past 99 does not occur, and an episode
//!   past 999 is a daily show's, which the 3-digit rule of the chapters corpus does not cover either). In a
//!   non-episode state the kicker is the item's context line, which is server text.
//!
//! `title.main`, `info.title` and `info.summary` are server text. The labels on pills, buttons, the Up Next
//! pair and the Skip pill are not Text ops: the compiler sizes those rects FROM their labels
//! (`TabPill::width_measured`, `Button::pill_w_measured`, `stand_in_w`), so "the label fits its rect" holds
//! by construction. What a long label can break is the rects' overlap and `SAFE`, which I4 and I5 report, and
//! the width of the fixed-`w` `vstack` a pill or button lives in, which nothing else sees (the rect is sized
//! from the label whatever room the cell has, so it draws past the `vstack`'s end with nothing beside it to
//! collide with). That last one is this module's: each pill or button is judged by its drawn rect against the
//! enclosing `vstack`'s right edge ([`Scan::control`]), as `HUD-E0601` at the control's item, with the label
//! it shows as the string. A disc has a fixed face and is no label.
//!
//! **The playbar's two clocks** are app text the playbar draws itself, and the compiler gives each a rect
//! (`DrawOp::Playbar::clock_rects`, [`clocks`]): the widest string the app can print, on the clock line under
//! the track. Each is judged as that string in the weight it is drawn in ([`Scan::clocks`]): the elapsed one
//! grows right from the bar's start, the remaining one left from the bar's end, and the room each has is the
//! controls on the clock line on the side it grows towards and, under `clocks: ends`, the other clock. `SAFE`
//! is not a limit here (the rect is the string, I5 reads it). Under `follow_head` the elapsed clock travels
//! the whole bar and has no room of its own to judge. The findings point at the playbar.
//!
//! **The room** is the smallest of the limits below, measured from the text's own `x` (it draws left to
//! right from there):
//! * its own box (`rect.w`), for every text but a hugging `label.*`: D10, "A text cell bound to an app label
//!   or an app-formatted value hugs its measured width", so a hugging label's rect IS its text, has slack
//!   exactly 0 by construction, and would warn under any positive `CI_TEXT_SKEW` if it counted;
//! * the right edge of `SAFE`;
//! * the nearest control (or Up Next / Skip pill) that starts at or right of the text and overlaps its line,
//!   at the REST rect (as I4 reads it; the compiler's fill rule uses the `CTRL_FOCUS_SCALE`d edge, which
//!   leaves room for the focus pop and which a hugging label does not get);
//! * the enclosing `vstack`s' fixed `w` ("a `vstack` of fixed `w`", Text fit): the vstack's right edge is its
//!   compiled `x` (`CompiledState::vstack_rect`) plus `w`, and the text's room is the distance from the TEXT's own
//!   `x` to it, since a cell nested in a `rect` or behind an image does not start at the vstack's `x`.
//!
//! A strip card's text is held to its card's width alone: the card is the box (`w` or `card_w`), and a
//! far card running past `SAFE` is I5's reveal rule.
//!
//! **Verdicts** ([`judge`]): `HUD-E0601` when `measured > room`; `HUD-W0602` when it fits but
//! `room - measured < skew` (strictly: a text with exactly `skew` to spare passes), the skew being
//! [`CI_TEXT_SKEW`] unless the caller says otherwise, so at the default 0 it cannot fire. Both point at the
//! text cell (a strip card's text at its template node) and carry `lang`, `state`, `slot`, the string, its
//! measured width, the room and the `rects` it concerns: the text's own, then the limits it reaches. **A text
//! that does not fit is ONE finding**, however many states and slots draw it: the first state in
//! `Compiled::states()` order names it ([`dedupe`](super::lint_zorder::dedupe); a strip card sits higher while a
//! section is open, but its string, its room and its horizontal extent are the same).
//!
//! Pure: no IO, no globals; the language is `cx.lang` and the numbered strings come from THAT language's
//! catalog through a `LocaleContext` built from its tag (the way `LintLang` builds its labels), never from
//! `i18n::current()`.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use plx_machine::machine::Measure;
use plx_platform::i18n::{msg, LocaleContext, Preference};
use plx_ui::consts::SAFE;
use plx_ui::fmt::KICKER_SEPARATOR;
use plx_ui::Rect;

use super::clocks::{self, ClockRects};
use super::compile::{AppLabels, ClockMode, CompiledState, ControlRect, DrawOp, SkipKind, Widget};
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};
use super::lint::{ctx_for, named, LangCtx, StateCtx};
use super::lint_zorder::dedupe;
use super::parse::{Object, Package, Val};
use super::validate::each_object;
use super::{codes, Finding, TextFact};

/// **`CI_TEXT_SKEW`** in px: the measurement skew between the pinned simulator and the TV that registry CI
/// requires app text to fit with to spare (`HUD-W0602`, a warning locally, an error in registry CI)
/// (`docs/hud-plugins.md`, "Text fit: the app is the enforcement point", item (c) Headroom: "Until it is
/// measured the margin is 0 and the TV decides"; and "LLM authoring", item 2: "`HUD-W0602` app text fits but
/// inside `CI_TEXT_SKEW` (an error in registry CI)"). PR12 measures it; until then it is `0.0`. It is a measured
/// margin, not a cap on a list, and is named as one.
pub(super) const CI_TEXT_SKEW: f32 = 0.0;

/// The digits of the numbers the lint formats, and the shape of the clock. Spelt once so the doc, the code and
/// the tests agree: a chapter number up to 3 digits (the spec), a season of 2, an episode of 3, hours of 2.
const CHAPTER_DIGITS: usize = 3;
const SEASON_DIGITS: usize = 2;
const EPISODE_DIGITS: usize = 3;

/// Report this module's findings for the language of `cx`: each fault once, by the first state that shows it
/// ([`dedupe`]).
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let from = out.len();
    let nodes = nodes_by_pointer(cx.package);
    let texts = Texts::new(cx);
    for state in cx.states() {
        let scan = Scan {
            cx,
            state: &state,
            nodes: &nodes,
            texts: &texts,
        };
        let mut found = Vec::new();
        scan.layout(state.state, SkipKind::Intro, &mut found);
        if let Some(credits) = state.state.skip_alt.as_deref() {
            scan.layout(credits, SkipKind::Credits, &mut found);
        }
        out.extend(found);
    }
    dedupe(out, from);
}

/// Every object of the package by its pointer: how a text finds the `vstack`s around it.
fn nodes_by_pointer(package: &Package) -> BTreeMap<&str, &Object> {
    let mut nodes = BTreeMap::new();
    each_object(&package.layout, &mut |o| {
        nodes.insert(o.pointer.as_str(), o);
    });
    nodes
}

// ---------------------------------------------------------------------------------------------------
// The app text and its strings
// ---------------------------------------------------------------------------------------------------

/// The app text a `bind` names, or `None` for server text and for a bind that is no text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppText {
    /// `label.chapters` / `label.info`
    Label,
    /// `item.start`
    ChapterStart,
    /// `item.title`: the numbered fallback of a chapter the server did not name
    ChapterFallback,
    /// `title.kicker`: the episode numbering prefix
    KickerPrefix,
}

fn app_text(bind: &str) -> Option<AppText> {
    match bind {
        "label.chapters" | "label.info" => Some(AppText::Label),
        "item.start" => Some(AppText::ChapterStart),
        "item.title" => Some(AppText::ChapterFallback),
        "title.kicker" => Some(AppText::KickerPrefix),
        _ => None,
    }
}

/// The strings of one language: the app's labels, the measurer, and the language's catalog for the numbered
/// strings (built once, on first use).
struct Texts<'a> {
    labels: &'a AppLabels,
    tag: &'static str,
    measure: &'a dyn Measure,
    locale: OnceCell<Option<LocaleContext>>,
}

impl<'a> Texts<'a> {
    fn new(cx: &LangCtx<'a>) -> Texts<'a> {
        Texts {
            labels: &cx.lang.labels,
            tag: cx.lang.tag,
            measure: cx.measure,
            locale: OnceCell::new(),
        }
    }

    /// The catalog of the language. `None` for a tag that is not one of the eleven shipped (a `LintLang` built by
    /// hand): its numbered strings are then not checked.
    fn locale(&self) -> Option<&LocaleContext> {
        self.locale.get_or_init(|| locale_of(self.tag)).as_ref()
    }

    /// The widest string `kind` can show for `bind`, at `size` in any weight of `bolds`, and its width.
    fn widest(
        &self,
        kind: AppText,
        bind: &str,
        size: i32,
        bolds: &[bool],
    ) -> Option<(String, f32)> {
        let candidates: Vec<String> = match kind {
            AppText::Label => {
                vec![if bind == "label.chapters" {
                    self.labels.tab_chapters.clone()
                } else {
                    self.labels.tab_info.clone()
                }]
            }
            AppText::ChapterStart => bolds
                .iter()
                .map(|&bold| self.widest_clock(size, bold))
                .collect(),
            AppText::ChapterFallback => {
                let locale = self.locale()?;
                (1..=9)
                    .map(|d| msg::widgets_chapters_number_in(locale, repdigit(d, CHAPTER_DIGITS)))
                    .collect()
            }
            AppText::KickerPrefix => {
                let locale = self.locale()?;
                (1..=9)
                    .map(|d| {
                        let ordinal = msg::core_episode_ordinal_in(
                            locale,
                            repdigit(d, EPISODE_DIGITS),
                            repdigit(d, SEASON_DIGITS),
                        );
                        format!("{ordinal}{KICKER_SEPARATOR}")
                    })
                    .collect()
            }
        };
        widest_of(self.measure, candidates, size, bolds)
    }

    /// `HH:MM:SS` with each position's widest digit at `size` and `bold`: the playbar's clock string
    /// ([`clocks::widest_clock`]), whose elapsed form is `fmt::clock`.
    fn widest_clock(&self, size: i32, bold: bool) -> String {
        clocks::widest_clock(self.measure, size, bold, false)
    }

    /// The digit of `digits` that is widest at `size` and `bold`; the lowest of equals.
    fn widest_digit(&self, size: i32, bold: bool, digits: RangeInclusive<u32>) -> u32 {
        clocks::widest_digit(self.measure, size, bold, digits)
    }
}

/// The catalog of the shipped language called `tag`, resolved from its preference alone (no system locale,
/// no process language).
fn locale_of(tag: &str) -> Option<LocaleContext> {
    Preference::SHIPPED
        .into_iter()
        .find(|p| p.tag() == tag)
        .map(|p| LocaleContext::resolve(p, None, None, None, None))
}

/// `digit` written `count` times, as a number (7, 3 -> 777).
fn repdigit(digit: i64, count: usize) -> i64 {
    digit.to_string().repeat(count).parse().unwrap_or(0)
}

/// The widest of `candidates` over every weight in `bolds`, with that width; the first of equals.
fn widest_of(
    measure: &dyn Measure,
    candidates: Vec<String>,
    size: i32,
    bolds: &[bool],
) -> Option<(String, f32)> {
    let mut best: Option<(String, f32)> = None;
    for candidate in candidates {
        let width = bolds
            .iter()
            .map(|&bold| measure.width_str(&candidate, size, bold))
            .fold(f32::NEG_INFINITY, f32::max);
        if best.as_ref().is_none_or(|(_, w)| width > *w) {
            best = Some((candidate, width));
        }
    }
    best
}

// ---------------------------------------------------------------------------------------------------
// The room and the verdict
// ---------------------------------------------------------------------------------------------------

/// What bounds a text on the right.
#[derive(Clone, Debug, PartialEq)]
enum Bound {
    /// the text's own box (its card's width, a fill, a `w`)
    Box,
    /// the right edge of `SAFE`
    Safe,
    /// a control (or the Up Next / Skip pill) the text would grow into, by its id and drawn rect
    Control(String, Rect),
    /// the right edge of an enclosing `vstack` of a fixed `w`
    Vstack,
    /// the playbar's other clock, by name and rect: the two share the clock line
    Clock(&'static str, Rect),
}

/// One limit on a text's room: the width it leaves from the text's own `x`, and what sets it.
#[derive(Clone, Debug, PartialEq)]
struct Limit {
    room: f32,
    bound: Bound,
}

/// Where a text op sits: in a band, or in the template of a strip card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Site {
    Band,
    Card,
}

/// Every limit on the room of a text at `rect`. A hugging label's `rect.w` is its own text, so it is no limit
/// (`hugs`); a strip card's text has its box alone; every other text is also held to the right edge of `SAFE`,
/// to the controls in `obstacles` it would grow into on its line, and to `vstack_right`, the nearest right edge
/// among the fixed-`w` vstacks around it.
fn limits(
    rect: Rect,
    site: Site,
    hugs: bool,
    obstacles: &[(String, Rect)],
    vstack_right: Option<f32>,
) -> Vec<Limit> {
    let mut limits = Vec::new();
    if site == Site::Card || !hugs {
        limits.push(Limit {
            room: rect.w,
            bound: Bound::Box,
        });
    }
    if site == Site::Card {
        return limits;
    }
    limits.push(Limit {
        room: SAFE.x + SAFE.w - rect.x,
        bound: Bound::Safe,
    });
    for (id, control) in obstacles {
        let on_the_line = control.y < rect.y + rect.h && control.y + control.h > rect.y;
        if on_the_line && control.x >= rect.x {
            limits.push(Limit {
                room: control.x - rect.x,
                bound: Bound::Control(id.clone(), *control),
            });
        }
    }
    if let Some(right) = vstack_right {
        limits.push(Limit {
            room: right - rect.x,
            bound: Bound::Vstack,
        });
    }
    limits
}

/// Which way a playbar clock grows when its string gets wider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grows {
    /// the elapsed clock stands at the bar's start
    Right,
    /// the remaining clock ends at the bar's end
    Left,
}

/// Every limit on the room of a playbar clock at `rect`, measured from the edge it is anchored to: the controls
/// on its clock line that lie on the side it grows towards (as I4 reads them, at their rest rects), and the
/// other clock, `neighbour`, which shares the line. `SAFE` is I5's: the clock's rect is its widest string.
fn clock_limits(
    rect: Rect,
    grows: Grows,
    obstacles: &[(String, Rect)],
    neighbour: Option<(&'static str, Rect)>,
) -> Vec<Limit> {
    let room = |other: Rect| match grows {
        Grows::Right => (other.x >= rect.x).then(|| other.x - rect.x),
        Grows::Left => (other.x + other.w <= rect.x + rect.w).then(|| rect.x + rect.w - (other.x + other.w)),
    };
    let mut limits = Vec::new();
    for (id, control) in obstacles {
        let on_the_line = control.y < rect.y + rect.h && control.y + control.h > rect.y;
        if let Some(room) = room(*control).filter(|_| on_the_line) {
            limits.push(Limit {
                room,
                bound: Bound::Control(id.clone(), *control),
            });
        }
    }
    if let Some((name, other)) = neighbour {
        if let Some(room) = room(other) {
            limits.push(Limit {
                room,
                bound: Bound::Clock(name, other),
            });
        }
    }
    limits
}

/// The limit that binds: the smallest room, the first of equals.
fn binding(limits: &[Limit]) -> Option<&Limit> {
    limits.iter().min_by(|a, b| a.room.total_cmp(&b.room))
}

/// What a measured width does against its room and the skew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// fits with at least `skew` to spare
    Fits,
    /// fits, but with less than `skew` to spare (`HUD-W0602`)
    Margin,
    /// wider than the room (`HUD-E0601`)
    Overflows,
}

/// The verdict for `measured` against `room`: an exact fit fits, and `skew` is a strict margin.
fn judge(measured: f32, room: f32, skew: f32) -> Verdict {
    if measured > room {
        Verdict::Overflows
    } else if room - measured < skew {
        Verdict::Margin
    } else {
        Verdict::Fits
    }
}

/// The nearest right edge among the fixed-`w` `vstack`s that enclose the node at `pointer`, if any: each
/// `vstack`'s own x (`placed`, its compiled rect) plus its `w`. A text in a `vstack` cell does not start at the
/// `vstack`'s x (a `rect` cell insets by its pad, and a cell behind an image of its row starts further right),
/// so its room to that edge is not the `w`. A `vstack` the state does not place (`placed` is `None`) bounds
/// nothing.
fn enclosing_vstack_right(
    pointer: &str,
    nodes: &BTreeMap<&str, &Object>,
    placed: &dyn Fn(&str) -> Option<Rect>,
) -> Option<f32> {
    let mut nearest: Option<f32> = None;
    let mut end = pointer.len();
    while let Some(slash) = pointer[..end].rfind('/') {
        end = slash;
        let Some(node) = nodes.get(&pointer[..end]) else {
            continue;
        };
        if node.node != "vstack" {
            continue;
        }
        let w = match node.get("w") {
            Some(Val::Int(w)) => *w as f32,
            Some(Val::Num(w)) => *w as f32,
            _ => continue,
        };
        let Some(vstack) = placed(&pointer[..end]) else {
            continue;
        };
        let right = vstack.x + w;
        nearest = Some(nearest.map_or(right, |a| a.min(right)));
    }
    nearest
}

/// The controls (and Up Next / Skip pills) a layout draws, by id, as the drawn rects.
fn obstacles(layout: &CompiledState, skip: SkipKind) -> Vec<(String, Rect)> {
    let mut found = Vec::new();
    for op in &layout.ops {
        match op {
            DrawOp::Control(c) => found.push((c.id.clone(), c.rect)),
            DrawOp::UpNext { rects, .. } => {
                found.push(("up_next.credits".to_owned(), rects.credits));
                found.push(("up_next.next".to_owned(), rects.next));
            }
            DrawOp::SkipPill { rects } => found.push(("skip".to_owned(), rects[skip as usize])),
            _ => {}
        }
    }
    found
}

// ---------------------------------------------------------------------------------------------------
// Walking a layout
// ---------------------------------------------------------------------------------------------------

/// One text op to judge.
struct TextOp<'a> {
    pointer: &'a str,
    rect: Rect,
    bind: &'a str,
    size: i32,
    site: Site,
}

/// A text that does not fit, or fits within the skew, and everything its finding reports.
struct Judged<'a> {
    op: &'a TextOp<'a>,
    string: String,
    measured: f32,
    limits: Vec<Limit>,
    verdict: Verdict,
}

/// What a scan of one state needs: the language, the state, the package's nodes and the language's strings.
struct Scan<'a, 'b> {
    cx: &'b LangCtx<'a>,
    state: &'b StateCtx<'a>,
    nodes: &'b BTreeMap<&'a str, &'a Object>,
    texts: &'b Texts<'a>,
}

impl Scan<'_, '_> {
    /// Judge every app text of `layout` (the state, or its credits layout) and push the findings `found` does
    /// not hold yet.
    fn layout(&self, layout: &CompiledState, skip: SkipKind, found: &mut Vec<Finding>) {
        let obstacles = obstacles(layout, skip);
        for (i, op) in layout.ops.iter().enumerate() {
            match op {
                DrawOp::Text {
                    rect,
                    bind,
                    size,
                    bold,
                    ..
                } => {
                    let text = TextOp {
                        pointer: layout.origin(i),
                        rect: *rect,
                        bind,
                        size: *size,
                        site: Site::Band,
                    };
                    let bolds: Vec<bool> = bold.each().copied().collect();
                    self.text(&text, &bolds, layout, skip, &obstacles, found);
                }
                DrawOp::Strip { template, .. } => {
                    for card in template {
                        if let DrawOp::Text {
                            rect,
                            bind,
                            size,
                            bold,
                            ..
                        } = &card.op
                        {
                            let text = TextOp {
                                pointer: &card.origin,
                                rect: *rect,
                                bind,
                                size: *size,
                                site: Site::Card,
                            };
                            let bolds: Vec<bool> = bold.each().copied().collect();
                            self.text(&text, &bolds, layout, skip, &obstacles, found);
                        }
                    }
                }
                // The playbar's two clocks are app text it draws itself.
                DrawOp::Playbar {
                    clock_rects,
                    clock_size,
                    clocks: mode,
                    ..
                } => self.clocks(
                    layout.origin(i),
                    clock_rects,
                    *clock_size,
                    *mode,
                    &obstacles,
                    skip,
                    found,
                ),
                _ => {}
            }
        }
        for (i, control) in layout.controls.iter().enumerate() {
            self.control(control, layout.control_pointer(i), layout, skip, found);
        }
    }

    /// Judge a label-sized control (`pill`, `button`) against the fixed-`w` `vstack` it lives in. The compiler
    /// sizes its rect FROM its label (or `min_w`), whatever room the cell has, so a label that outgrows the
    /// `vstack` draws past its end and nothing clips it. The rect, not the label, is measured: padding, `min_w`
    /// and the widest variant of the label (an `actions` button takes one width over all of them) are in it.
    /// Other limits (`SAFE`, a control beside it) are I5's and I4's: they see the rect, and fail on a
    /// collision. A disc has a fixed face and is no label.
    fn control(
        &self,
        control: &ControlRect,
        pointer: &str,
        layout: &CompiledState,
        skip: SkipKind,
        found: &mut Vec<Finding>,
    ) {
        if control.widget == Widget::Disc {
            return;
        }
        let Some(right) =
            enclosing_vstack_right(pointer, self.nodes, &|vstack| layout.vstack_rect(vstack))
        else {
            return;
        };
        let (measured, room) = (control.rect.w, right - control.rect.x);
        let verdict = judge(measured, room, self.cx.opts.text_skew);
        if verdict == Verdict::Fits {
            return;
        }
        let shown = self.texts.labels.of(
            &control.command,
            control.section.as_deref(),
            layout.key.episode,
        );
        // a control with no app label (a `min_w` button of `open_more`) is named by its id
        let string = if shown.is_empty() { &control.id } else { shown };
        let mut ctx = ctx_for(self.cx, self.state);
        ctx.text = Some(TextFact {
            string: string.to_owned(),
            measured,
            room,
        });
        let own = if skip == SkipKind::Credits {
            format!("{} (credits layout)", control.id)
        } else {
            control.id.clone()
        };
        ctx.rects.push(named(&own, control.rect));
        ctx.rects.push(named(
            "vstack",
            Rect::new(control.rect.x, control.rect.y, room, control.rect.h),
        ));
        let what = if control.widget == Widget::Pill {
            "pill"
        } else {
            "button"
        };
        found.push(
            match verdict {
                Verdict::Overflows => Finding::new(
                    &codes::E0601,
                    pointer,
                    format!(
                        "the {what} `{}` (\"{string}\") is {measured:.1} px wide but has {room:.1} px before the end of its vstack",
                        control.id
                    ),
                    "A control is as wide as its label and never clips: give its vstack room (a wider `w`), or \
                     place the control outside the vstack, so the longest translation fits",
                ),
                _ => Finding::new(
                    &codes::W0602,
                    pointer,
                    format!(
                        "the {what} `{}` (\"{string}\") fits with {:.1} px to spare before the end of its vstack, less \
                         than the {:.1} px the measurement skew needs",
                        control.id,
                        room - measured,
                        self.cx.opts.text_skew
                    ),
                    "Leave at least the measurement skew between the simulator and the TV free beside the control",
                ),
            }
            .with(ctx),
        );
    }

    fn text(
        &self,
        op: &TextOp<'_>,
        bolds: &[bool],
        layout: &CompiledState,
        skip: SkipKind,
        obstacles: &[(String, Rect)],
        found: &mut Vec<Finding>,
    ) {
        let Some(kind) = app_text(op.bind) else {
            return;
        };
        // a non-episode's kicker is the item's context line, which is server text
        if kind == AppText::KickerPrefix && !layout.key.episode {
            return;
        }
        let Some((string, measured)) = self.texts.widest(kind, op.bind, op.size, bolds) else {
            return;
        };
        let hugs = kind == AppText::Label
            && op.site == Site::Band
            && self
                .nodes
                .get(op.pointer)
                .is_some_and(|node| node.get("w").is_none());
        let vstack_right = if op.site == Site::Band {
            enclosing_vstack_right(op.pointer, self.nodes, &|vstack| layout.vstack_rect(vstack))
        } else {
            None
        };
        let limits = limits(op.rect, op.site, hugs, obstacles, vstack_right);
        self.judge_text(op, string, measured, limits, skip, found);
    }

    /// The playbar's two clocks. Each is the widest string the app can print in the weight it is drawn in
    /// ([`clocks::widest_clock`]), judged against the room on the clock line: the elapsed clock grows right
    /// from the bar's start, the remaining one grows left from its end, and under `ends` each is also held
    /// short of the other. Under `follow_head` the elapsed clock travels the whole bar, so it has no room of
    /// its own to judge (I4 reads its lane as decor); the remaining clock is still judged against the
    /// controls beside it.
    #[allow(clippy::too_many_arguments)]
    fn clocks(
        &self,
        pointer: &str,
        rects: &ClockRects,
        size: i32,
        mode: ClockMode,
        obstacles: &[(String, Rect)],
        skip: SkipKind,
        found: &mut Vec<Finding>,
    ) {
        let ends = mode == ClockMode::Ends;
        let (elapsed, remaining) = ("playbar elapsed clock", "playbar remaining clock");
        for (bind, rect, bold, is_remaining, grows, other) in [
            (elapsed, rects.elapsed, clocks::ELAPSED_BOLD, false, Grows::Right, (remaining, rects.remaining)),
            (remaining, rects.remaining, clocks::REMAINING_BOLD, true, Grows::Left, (elapsed, rects.elapsed)),
        ] {
            if !ends && !is_remaining {
                continue;
            }
            let string = clocks::widest_clock(self.cx.measure, size, bold, is_remaining);
            let measured = self.cx.measure.width_str(&string, size, bold);
            let limits = clock_limits(rect, grows, obstacles, ends.then_some(other));
            let op = TextOp {
                pointer,
                rect,
                bind,
                size,
                site: Site::Band,
            };
            self.judge_text(&op, string, measured, limits, skip, found);
        }
    }

    /// Judge `string`, `measured` wide, against the tightest of `limits` and push its finding if it does not
    /// fit.
    fn judge_text(
        &self,
        op: &TextOp<'_>,
        string: String,
        measured: f32,
        limits: Vec<Limit>,
        skip: SkipKind,
        found: &mut Vec<Finding>,
    ) {
        let Some(tightest) = binding(&limits) else {
            return;
        };
        let verdict = judge(measured, tightest.room, self.cx.opts.text_skew);
        if verdict == Verdict::Fits {
            return;
        }
        let finding = self.finding(
            &Judged {
                op,
                string,
                measured,
                limits,
                verdict,
            },
            skip,
        );
        found.push(finding);
    }

    /// The finding for a text that does not fit (`HUD-E0601`) or fits within the skew (`HUD-W0602`).
    fn finding(&self, judged: &Judged<'_>, skip: SkipKind) -> Finding {
        let Judged {
            op,
            string,
            measured,
            limits,
            verdict,
        } = judged;
        let (measured, tightest) = (*measured, binding(limits).map_or(0.0, |l| l.room));
        let mut ctx = ctx_for(self.cx, self.state);
        ctx.text = Some(TextFact {
            string: string.clone(),
            measured,
            room: tightest,
        });
        let own = if skip == SkipKind::Credits {
            "text (credits layout)"
        } else {
            "text"
        };
        ctx.rects.push(named(own, op.rect));
        // the limits the text reaches, nearest first; for a margin, the one it is close to
        let mut reached: Vec<&Limit> = match verdict {
            Verdict::Overflows => limits.iter().filter(|l| l.room < measured).collect(),
            _ => binding(limits).into_iter().collect(),
        };
        reached.sort_by(|a, b| a.room.total_cmp(&b.room));
        for limit in &reached {
            match &limit.bound {
                Bound::Box => {}
                Bound::Safe => ctx.rects.push(named("safe", SAFE)),
                Bound::Control(id, rect) => ctx.rects.push(named(id, *rect)),
                Bound::Vstack => ctx.rects.push(named(
                    "vstack",
                    Rect::new(op.rect.x, op.rect.y, limit.room, op.rect.h),
                )),
                Bound::Clock(name, rect) => ctx.rects.push(named(name, *rect)),
            }
        }
        let before = match binding(limits).map(|l| &l.bound) {
            Some(Bound::Safe) => "the right edge of SAFE".to_owned(),
            Some(Bound::Control(id, _)) => format!("the control `{id}`"),
            Some(Bound::Vstack) => "the end of its vstack".to_owned(),
            Some(Bound::Clock(name, _)) => format!("the {name}"),
            _ => "the end of its box".to_owned(),
        };
        match verdict {
            Verdict::Overflows => Finding::new(
                &codes::E0601,
                op.pointer,
                format!("the app text \"{string}\" is {measured:.1} px wide but has {tightest:.1} px before {before}"),
                "App text hugs and never elides: give it room (a wider box, card or vstack, or the cell further \
                 from the control or the edge) so the longest translation fits",
            ),
            _ => Finding::new(
                &codes::W0602,
                op.pointer,
                format!(
                    "the app text \"{string}\" fits with {:.1} px to spare before {before}, less than the {:.1} px \
                     the measurement skew needs",
                    tightest - measured,
                    self.cx.opts.text_skew
                ),
                "Leave at least the measurement skew between the simulator and the TV free beside the text",
            ),
        }
        .with(ctx)
    }
}

/// The reference's info-card vstack cut to 160 px, with its synopsis replaced by the "Chapters" label at the
/// `DISPLAY` size: the label is wider than the vstack, the kicker above it (144 px) is not.
#[cfg(test)]
const LABEL_IN_NARROW_VSTACK: &[(&str, &str)] = &[
    ("\"w\": 760", "\"w\": 160"),
    (
        "{ \"text\": { \"bind\": \"info.summary\", \"size\": \"CAPTION\", \"ink\": \"TEXT_SECONDARY\", \"bold\": false, \"max_lines\": 2, \"w\": \"fill\" } }",
        "{ \"text\": { \"bind\": \"label.chapters\", \"size\": \"DISPLAY\", \"ink\": \"TEXT_SECONDARY\", \"bold\": false } }",
    ),
];

/// The reference's Chapters heading as the second cell of a `rect` that fills a 300 px vstack, after a 200 px
/// image: the label's x is the vstack's plus 200.
#[cfg(test)]
const LABEL_BEHIND_AN_IMAGE_IN_A_VSTACK: &[(&str, &str)] = &[(
    "{ \"align\": \"start\", \"text\": { \"bind\": \"label.chapters\", \"size\": \"LABEL\", \"ink\": \"TEXT_PRIMARY\", \"bold\": false } }",
    "{ \"align\": \"start\", \"vstack\": { \"gap\": 0, \"w\": 300, \"cells\": [ { \"rect\": { \"ink\": \"PANEL_TOP\", \"cells\": [ \
     { \"align\": \"start\", \"image\": { \"bind\": \"info.still\", \"w\": 200, \"h\": 40 } }, \
     { \"align\": \"start\", \"text\": { \"bind\": \"label.chapters\", \"size\": \"LABEL\", \"ink\": \"TEXT_PRIMARY\", \"bold\": false } } \
     ] } } ] } }",
)];

/// Classic's Info card column cut to a 200 px vstack, its synopsis cell replaced by an `actions` row with one
/// From the Beginning button.
#[cfg(test)]
const BUTTON_IN_NARROW_VSTACK: &[(&str, &str)] = &[
    ("\"vstack\": { \"gap\": 6, \"w\": \"fill\", \"cells\": [", "\"vstack\": { \"gap\": 6, \"w\": 200, \"cells\": ["),
    (
        "{ \"text\": { \"bind\": \"info.summary\", \"size\": \"LABEL\", \"ink\": \"TEXT_SECONDARY\", \"leading\": 29, \"max_lines\": 2 } },",
        "{ \"actions\": { \"axis\": \"row\", \"gap\": 16, \"item_h\": 50, \"items\": [ \
         { \"id\": \"fb2\", \"command\": \"from_beginning\", \"widget\": \"button\", \"size\": 50 } ] } },",
    ),
];

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[InvalidCase] = &[
    InvalidCase {
        name: "a card too narrow for the chapter-name fallback",
        code: "HUD-E0601",
        pointer: "/layout/block/4/cells/0/strip/card/below/1/text",
        base: Base::Reference,
        edits: &[("\"card_w\": 192", "\"card_w\": 100")],
        langs: &["en"],
        skew: 0.0,
        also: &[],
    },
    InvalidCase {
        name: "a vstack too narrow for the episode numbering prefix",
        code: "HUD-E0601",
        pointer: "/layout/block/6/cells/1/vstack/cells/0/text",
        base: Base::Reference,
        edits: &[("\"w\": 760", "\"w\": 100")],
        langs: &["en"],
        skew: 0.0,
        also: &[],
    },
    InvalidCase {
        name: "a tab label in a vstack narrower than the label",
        code: "HUD-E0601",
        pointer: "/layout/block/6/cells/1/vstack/cells/2/text",
        base: Base::Reference,
        edits: LABEL_IN_NARROW_VSTACK,
        langs: &["en"],
        skew: 0.0,
        also: &[],
    },
    // The Chapters heading in a 300 px vstack, behind a 200 px image inside a `rect` cell: the label starts 200 px
    // into the vstack, so only 100 px remain before the vstack's end. Measured from the label's own x (the text
    // of any vstack cell is not at the vstack's x) its room would read 300 and nothing would be found.
    InvalidCase {
        name: "a tab label behind an image in a rect cell of a vstack, past the vstack's end",
        code: "HUD-E0601",
        pointer: "/layout/block/3/cells/0/vstack/cells/0/rect/cells/1/text",
        base: Base::Reference,
        edits: LABEL_BEHIND_AN_IMAGE_IN_A_VSTACK,
        langs: &["en"],
        skew: 0.0,
        also: &[],
    },
    // A label-sized control in a fixed-`w` vstack: classic's Info card column cut to 200 px, its synopsis cell
    // swapped for an `actions` row holding one From the Beginning button. `emit_actions` sizes the button from
    // its label (or `min_w`) whatever the cell's width, so it runs to x 700..809 depending on the language, past
    // the vstack's end at 662, and nothing stands beside it for I4 or I5 to find
    InvalidCase {
        name: "a button wider than the fixed-width vstack it lives in",
        code: "HUD-E0601",
        pointer: "/layout/block/3/cells/0/rect/cells/1/vstack/cells/1/actions/items/0",
        base: Base::Classic,
        edits: BUTTON_IN_NARROW_VSTACK,
        langs: &[],
        skew: 0.0,
        also: &[],
    },
    InvalidCase {
        name: "a card that fits the chapter-name fallback with less than the skew to spare",
        code: "HUD-W0602",
        pointer: "/layout/block/4/cells/0/strip/card/below/1/text",
        base: Base::Reference,
        edits: &[("\"card_w\": 192", "\"card_w\": 190")],
        langs: &["en"],
        skew: 47.0,
        also: &[],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::CStr;

    use plx_base::fontcov::advances::ShippedMeasure;

    use super::super::compile::{compile, CompileCtx};
    use super::super::lint::testing::{apply, assert_case, Base};
    use super::super::lint::{lint, lint_with, LintLang, LintOptions};
    use super::super::parse::parse;
    use super::super::schema::size_rung;
    use super::super::BUILTIN_CLASSIC;
    use super::*;

    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
    const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");
    const HERO: &str = include_str!("fixtures/hero-center.plxhud.json");
    const TABS: &str = include_str!("fixtures/tabs.plxhud.json");
    const PALETTE: &str = include_str!("fixtures/palette.plxhud.json");

    const KICKER: &str = "/layout/block/0/cells/0/vstack/cells/0/text";
    const INFO_KICKER: &str = "/layout/block/6/cells/1/vstack/cells/0/text";
    const CLOCK: &str = "/layout/block/4/cells/0/strip/card/below/0/text";
    const FALLBACK: &str = "/layout/block/4/cells/0/strip/card/below/1/text";
    const CHAPTERS_LABEL: &str = "/layout/block/3/cells/0/text";
    /// the playbar's two clocks, app text drawn by the playbar and judged at its pointer
    const PLAYBAR_CLOCKS: &str = "/layout/block/1/cells/0/playbar";

    /// A measurer with chosen metrics: `7` is the widest glyph (16 px), every other glyph 10 px, and bold
    /// adds 4 px a glyph, so what the check picks is visible.
    struct Glyphs;

    impl Measure for Glyphs {
        fn width(&self, s: &CStr, _sz: i32, bold: bool) -> f32 {
            s.to_string_lossy()
                .chars()
                .map(|c| if c == '7' { 16.0 } else { 10.0 } + if bold { 4.0 } else { 0.0 })
                .sum()
        }
        fn cap_h(&self, sz: i32) -> f32 {
            sz as f32
        }
        fn line_h(&self, sz: i32) -> f32 {
            sz as f32
        }
    }

    /// This module's check alone, on `text` in `lang`, measured by `measure`, with `skew`.
    fn check_in(text: &str, lang: &LintLang, measure: &dyn Measure, skew: f32) -> Vec<Finding> {
        let parsed = parse(text.as_bytes());
        let package = parsed.package.unwrap_or_else(|| {
            panic!(
                "the text parses: {:?}",
                parsed
                    .findings
                    .iter()
                    .map(|f| (f.code, f.pointer.clone(), f.message.clone()))
                    .collect::<Vec<_>>()
            )
        });
        let compiled = compile(
            &package,
            measure,
            CompileCtx {
                labels: lang.labels.clone(),
            },
        );
        let opts = LintOptions { text_skew: skew };
        let cx = LangCtx {
            package: &package,
            compiled: &compiled,
            measure,
            lang,
            opts: &opts,
        };
        let mut out = Vec::new();
        check(&cx, &mut out);
        out
    }

    fn in_en(text: &str, skew: f32) -> Vec<Finding> {
        check_in(
            text,
            &LintLang::for_tag("en").unwrap(),
            &ShippedMeasure,
            skew,
        )
    }

    /// Every text finding of `lint_with` on `text` in all eleven languages.
    fn text_findings(text: &str, skew: f32) -> Vec<Finding> {
        lint_with(
            text,
            &ShippedMeasure,
            &LintLang::shipped(),
            &LintOptions { text_skew: skew },
        )
        .into_iter()
        .filter(|f| f.code == "HUD-E0601" || f.code == "HUD-W0602")
        .collect()
    }

    fn pointers(findings: &[Finding]) -> BTreeSet<&str> {
        findings.iter().map(|f| f.pointer.as_str()).collect()
    }

    /// The slack (room minus measured) the text at `pointer` has in English in `text`, read off a lint under a
    /// skew no text can clear.
    fn slack_en(text: &str, pointer: &str) -> f32 {
        let found = in_en(text, 1e9);
        let fact = found
            .iter()
            .find(|f| f.pointer == pointer)
            .unwrap_or_else(|| panic!("no finding at {pointer}"))
            .ctx
            .text
            .clone()
            .unwrap();
        fact.room - fact.measured
    }

    // ---- the cases ---------------------------------------------------------------------------------

    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(INVALID.iter().any(|c| c.code == "HUD-E0601"));
        assert!(INVALID.iter().any(|c| c.code == "HUD-W0602"));
        for case in INVALID {
            assert_case(case);
        }
    }

    #[test]
    fn the_default_skew_is_zero_so_no_text_is_within_the_margin() {
        assert_eq!(CI_TEXT_SKEW, 0.0);
        assert_eq!(LintOptions::default().text_skew, CI_TEXT_SKEW);
    }

    // ---- the packages that ship --------------------------------------------------------------------

    #[test]
    fn classic_and_the_fixtures_have_no_text_findings_in_any_language() {
        let langs = LintLang::shipped();
        assert_eq!(langs.len(), 11);
        for (name, text) in [
            ("classic", BUILTIN_CLASSIC),
            ("reference", REFERENCE),
            ("minimal", MINIMAL),
            ("hero-center", HERO),
            ("tabs", TABS),
            ("palette", PALETTE),
        ] {
            let found: Vec<Finding> = lint(text, &ShippedMeasure, &langs)
                .into_iter()
                .filter(|f| f.code == "HUD-E0601" || f.code == "HUD-W0602")
                .collect();
            assert!(
                found.is_empty(),
                "{name}: {:#?}",
                found
                    .iter()
                    .map(|f| (&f.pointer, &f.message))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn that_silence_is_not_a_check_that_looked_at_nothing() {
        // Under a skew nothing clears, every app text of the reference answers, in every language.
        let found = text_findings(REFERENCE, 1e9);
        assert!(found.iter().all(|f| f.code == "HUD-W0602"));
        let pointers = pointers(&found);
        for expected in [CHAPTERS_LABEL, CLOCK, FALLBACK, KICKER, INFO_KICKER, PLAYBAR_CLOCKS] {
            assert!(
                pointers.contains(expected),
                "{expected} was not checked: {pointers:?}"
            );
        }
        // ... and nothing else: no server text, no control label.
        assert_eq!(pointers.len(), 6, "{pointers:?}");
        // the languages differ in the strings the app writes ...
        let tags: BTreeSet<_> = found
            .iter()
            .filter(|f| f.pointer == CHAPTERS_LABEL)
            .map(|f| f.ctx.lang.clone())
            .collect();
        assert_eq!(tags.len(), 11, "{tags:?}");
        // ... and the clock is the same in all of them, so its finding collapsed to one.
        let clocks: Vec<_> = found.iter().filter(|f| f.pointer == CLOCK).collect();
        assert!(clocks.iter().all(|f| f.ctx.lang.is_none()), "{clocks:?}");
    }

    // ---- the verdict, end to end -------------------------------------------------------------------

    #[test]
    fn an_exact_fit_passes_and_one_pixel_less_overflows() {
        let slack = slack_en(REFERENCE, FALLBACK);
        let measured = in_en(REFERENCE, 1e9)
            .iter()
            .find(|f| f.pointer == FALLBACK)
            .unwrap()
            .ctx
            .text
            .clone()
            .unwrap()
            .measured;
        assert_eq!(measured + slack, 192.0, "the card is 192 wide");
        let with_card = |w: f32| REFERENCE.replace("\"card_w\": 192", &format!("\"card_w\": {w}"));
        let exact = in_en(&with_card(measured), 0.0);
        assert!(exact.is_empty(), "{exact:?}");
        let one_short = in_en(&with_card(measured - 1.0), 0.0);
        assert_eq!(
            pointers(&one_short),
            BTreeSet::from([FALLBACK]),
            "{one_short:?}"
        );
        assert_eq!(one_short[0].code, "HUD-E0601");
    }

    #[test]
    fn the_margin_is_strict_at_the_skew() {
        let slack = slack_en(REFERENCE, FALLBACK);
        assert_eq!(
            slack, 48.0,
            "the reference's chapter fallback leaves 48 px in a 192 px card"
        );
        let warned = |skew: f32| {
            let langs = [LintLang::for_tag("en").unwrap()];
            lint_with(
                REFERENCE,
                &ShippedMeasure,
                &langs,
                &LintOptions { text_skew: skew },
            )
            .into_iter()
            .any(|f| f.code == "HUD-W0602" && f.pointer == FALLBACK)
        };
        assert!(!warned(slack - 0.5), "more room than the skew asks");
        assert!(!warned(slack), "exactly the skew to spare is enough");
        assert!(warned(slack + 0.5), "less room than the skew asks");
    }

    #[test]
    fn a_finding_reports_the_string_its_width_the_room_and_the_rects() {
        let found = in_en(
            &apply(Base::Reference, &[("\"w\": 760", "\"w\": 100")]),
            0.0,
        );
        let kicker: Vec<_> = found.iter().filter(|f| f.pointer == INFO_KICKER).collect();
        assert!(!kicker.is_empty());
        for f in kicker {
            assert_eq!(f.code, "HUD-E0601");
            let fact = f.ctx.text.clone().unwrap();
            assert_eq!(fact.string, "S11, E111 \u{b7} ");
            assert_eq!(fact.room, 100.0);
            assert!(fact.measured > 100.0);
            assert_eq!(f.ctx.lang.as_deref(), Some("en"));
            assert!(f.ctx.state.is_some() && f.ctx.slot.is_some());
            let names: Vec<_> = f.ctx.rects.iter().map(|r| r.name.as_str()).collect();
            assert_eq!(names, ["text", "vstack"]);
            assert!(f.message.contains("100.0 px"), "{}", f.message);
        }
    }

    #[test]
    fn a_hugging_label_is_held_by_safe_not_by_its_own_box() {
        // The label's rect is its text (slack 0), so it would warn under any skew if its box counted. It
        // sits in no vstack, so SAFE is what bounds it.
        let found = in_en(REFERENCE, 1e9);
        let label = found.iter().find(|f| f.pointer == CHAPTERS_LABEL).unwrap();
        let fact = label.ctx.text.clone().unwrap();
        assert_eq!(fact.string, "Chapters");
        assert!(fact.room > fact.measured + 1000.0, "{fact:?}");
        let names: Vec<_> = label.ctx.rects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["text", "safe"], "SAFE binds it");
    }

    // ---- what is and is not app text ---------------------------------------------------------------

    #[test]
    fn server_text_is_never_checked() {
        // Every vstack of the info card down to 1 px: only the kicker's app prefix answers, not the title
        // or the synopsis the server writes.
        let narrow = apply(Base::Reference, &[("\"w\": 760", "\"w\": 1")]);
        let found = in_en(&narrow, 0.0);
        assert_eq!(pointers(&found), BTreeSet::from([INFO_KICKER]), "{found:?}");
    }

    #[test]
    fn the_kicker_prefix_is_checked_only_where_the_item_is_an_episode() {
        let text = apply(Base::Reference, &[("\"w\": 760", "\"w\": 100")]);
        let package = parse(text.as_bytes()).package.unwrap();
        let lang = LintLang::for_tag("en").unwrap();
        let compiled = compile(
            &package,
            &ShippedMeasure,
            CompileCtx {
                labels: lang.labels.clone(),
            },
        );
        assert!(
            compiled.states().iter().any(|s| !s.key.episode),
            "the reference compiles movie states too"
        );
        let found = check_in(&text, &lang, &ShippedMeasure, 0.0);
        assert!(found.iter().any(|f| f.pointer == INFO_KICKER));
        assert!(
            found
                .iter()
                .all(|f| !f.ctx.state.as_deref().unwrap().contains("movie")),
            "{found:?}"
        );
    }

    #[test]
    fn a_text_that_does_not_fit_is_one_finding_however_many_states_show_it() {
        let narrow = REFERENCE.replace("\"card_w\": 192", "\"card_w\": 100");
        let found: Vec<_> = in_en(&narrow, 0.0)
            .into_iter()
            .filter(|f| f.code == "HUD-E0601")
            .collect();
        assert!(
            !found.is_empty(),
            "the 100 px card overflows the chapter-name fallback"
        );
        // The card sits 197 px higher while the Info page is open (its y differs between the states), and
        // `movie` / `paused` change nothing about the strip: a text is still one fault.
        let texts: BTreeSet<_> = found
            .iter()
            .map(|f| {
                (
                    f.pointer.as_str(),
                    f.ctx.text.as_ref().map(|t| t.string.as_str()),
                )
            })
            .collect();
        assert_eq!(
            found.len(),
            texts.len(),
            "one E0601 per distinct text, not per state: {found:#?}"
        );
    }

    const BUTTON: &str = "/layout/block/3/cells/0/rect/cells/1/vstack/cells/1/actions/items/0";

    /// The label-sized control in a narrow vstack, in English: what the finding reports.
    #[test]
    fn a_button_past_its_vstack_reports_its_label_its_width_and_the_room() {
        let text = apply(Base::Classic, BUTTON_IN_NARROW_VSTACK);
        let lang = LintLang::for_tag("en").unwrap();
        let found: Vec<Finding> = check_in(&text, &lang, &ShippedMeasure, 0.0);
        assert_eq!(found.len(), 1, "one fault, one finding: {found:#?}");
        let f = &found[0];
        assert_eq!((f.code, f.pointer.as_str()), ("HUD-E0601", BUTTON));
        let fact = f.ctx.text.clone().expect("the label's width and room");
        assert_eq!(fact.string, lang.labels.from_beginning);
        assert!(fact.measured > fact.room, "{fact:?}");
        let names: Vec<&str> = f.ctx.rects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["fb2", "vstack"]);
        // the vstack rect runs from the button's x to the vstack's end: its width is the room
        assert_eq!(f.ctx.rects[1].w, fact.room);
        assert_eq!(f.ctx.rects[0].w, fact.measured);
        assert!(
            f.message.contains("button `fb2`") && f.message.contains(&lang.labels.from_beginning),
            "{}",
            f.message
        );
    }

    /// The label is the language's: each of the eleven languages names its own, and a language whose button fits
    /// is silent (a pill or button is judged by its own rect, not by a fixed list of languages).
    #[test]
    fn a_button_past_its_vstack_is_found_in_the_languages_it_is_wider_in() {
        let text = apply(Base::Classic, BUTTON_IN_NARROW_VSTACK);
        let found = text_findings(&text, 0.0);
        let tags: BTreeSet<&str> = found.iter().filter_map(|f| f.ctx.lang.as_deref()).collect();
        assert_eq!(
            tags.len(),
            11,
            "the button outgrows 200 px in every shipped language: {tags:?}"
        );
        assert!(
            found.iter().all(|f| f.pointer == BUTTON),
            "{:?}",
            pointers(&found)
        );
    }

    /// A button inside its vstack is silent, and the check looked: widen the vstack and only a skew nothing
    /// clears finds it, as the margin warning.
    #[test]
    fn a_button_inside_its_vstack_is_silent_until_a_skew_it_cannot_clear() {
        let wide =
            apply(Base::Classic, BUTTON_IN_NARROW_VSTACK).replace("\"w\": 200,", "\"w\": 1000,");
        assert!(
            in_en(&wide, 0.0).is_empty(),
            "a 1000 px vstack holds the button"
        );
        let margin: Vec<Finding> = in_en(&wide, 1e9)
            .into_iter()
            .filter(|f| f.pointer == BUTTON)
            .collect();
        assert_eq!(margin.len(), 1, "{margin:#?}");
        assert_eq!(margin[0].code, "HUD-W0602");
    }

    /// A disc has a fixed face and no label: a disc past a vstack is not this check's.
    #[test]
    fn a_disc_is_not_a_label_sized_control() {
        let text = apply(Base::Classic, BUTTON_IN_NARROW_VSTACK)
            .replace(
                "\"widget\": \"button\", \"size\": 50 } ] } },",
                "\"widget\": \"disc\", \"size\": 50 } ] } },",
            )
            .replace("\"w\": 200,", "\"w\": 20,");
        assert!(text.contains("\"widget\": \"disc\", \"size\": 50") && text.contains("\"w\": 20,"));
        let found = in_en(&text, 0.0);
        assert!(found.iter().all(|f| f.pointer != BUTTON), "{found:#?}");
    }

    #[test]
    fn the_clock_is_one_finding_for_all_languages_and_the_fallback_one_per_language() {
        let narrow = REFERENCE.replace("\"card_w\": 192", "\"card_w\": 60");
        let found = text_findings(&narrow, 0.0);
        // the clock is the same in every language and in every state: ONE finding, with no language
        let clock: Vec<_> = found.iter().filter(|f| f.pointer == CLOCK).collect();
        assert_eq!(clock.len(), 1, "{clock:?}");
        assert!(clock.iter().all(|f| f.ctx.lang.is_none()), "{clock:?}");
        // the fallback is a different string in every language: one finding each
        let fallback: BTreeSet<_> = found
            .iter()
            .filter(|f| f.pointer == FALLBACK)
            .map(|f| f.ctx.lang.clone().unwrap())
            .collect();
        assert_eq!(fallback.len(), 11, "{fallback:?}");
        assert_eq!(
            found.iter().filter(|f| f.pointer == FALLBACK).count(),
            11,
            "{found:?}"
        );
    }

    #[test]
    fn a_language_the_app_does_not_ship_still_gets_its_labels_checked_but_no_numbered_strings() {
        let en = LintLang::for_tag("en").unwrap();
        let hand_built = LintLang {
            tag: "xx",
            labels: en.labels.clone(),
        };
        let narrow_card = REFERENCE.replace("\"card_w\": 192", "\"card_w\": 60");
        let found = check_in(&narrow_card, &hand_built, &ShippedMeasure, 0.0);
        // the clock needs no catalog; the numbered fallback does
        assert_eq!(pointers(&found), BTreeSet::from([CLOCK]), "{found:?}");
        let narrow_label = apply(Base::Reference, LABEL_IN_NARROW_VSTACK);
        let found = check_in(&narrow_label, &hand_built, &ShippedMeasure, 0.0);
        assert!(
            found
                .iter()
                .any(|f| f.pointer == "/layout/block/6/cells/1/vstack/cells/2/text"),
            "{found:?}"
        );
    }

    #[test]
    fn the_widest_weight_a_text_can_draw_is_the_one_measured() {
        // The classic's kicker is bold in an episode state and plain otherwise: the prefix is only checked
        // in the episode states, and the width reported is the bold one.
        let found = in_en(BUILTIN_CLASSIC, 1e9);
        let fact = found
            .iter()
            .find(|f| f.pointer == KICKER)
            .unwrap()
            .ctx
            .text
            .clone()
            .unwrap();
        let caption = size_rung("CAPTION").unwrap();
        let plain = ShippedMeasure.width_str(&fact.string, caption, false);
        let bold = ShippedMeasure.width_str(&fact.string, caption, true);
        assert!(
            bold > plain,
            "the premise: bold is wider ({bold} vs {plain})"
        );
        assert_eq!(fact.measured, bold);
    }

    #[test]
    fn the_credits_layout_of_a_skip_state_is_checked_too() {
        // Narrow the band until the kicker's room is between the two Skip pills: the intro pill leaves it
        // enough, the credits pill (wider, in German) does not.
        let tight = BUILTIN_CLASSIC.replace("\"x_inset\": 0", "\"x_inset\": 590");
        let de = LintLang::for_tag("de").unwrap();
        let found = check_in(&tight, &de, &ShippedMeasure, 0.0);
        let skip: Vec<_> = found
            .iter()
            .filter(|f| f.ctx.slot.as_deref() == Some("skip") && f.pointer == KICKER)
            .collect();
        assert!(!skip.is_empty(), "{found:?}");
        for f in &skip {
            assert_eq!(f.code, "HUD-E0601");
            assert_eq!(
                f.ctx.rects[0].name, "text (credits layout)",
                "only the credits layout is too tight"
            );
            let fact = f.ctx.text.clone().unwrap();
            assert!(fact.room < fact.measured);
        }
        // The intro layout of the same state has the room, so the credits layout is where the fault is:
        // a check that stopped at `state.state` would pass this package.
        let intro: Vec<_> = skip
            .iter()
            .filter(|f| f.ctx.rects[0].name == "text")
            .collect();
        assert!(intro.is_empty(), "{intro:?}");
    }

    // ---- the strings -------------------------------------------------------------------------------

    fn en_texts<'a>(lang: &'a LintLang, measure: &'a dyn Measure) -> Texts<'a> {
        Texts {
            labels: &lang.labels,
            tag: lang.tag,
            measure,
            locale: OnceCell::new(),
        }
    }

    fn widest_of_kind(
        texts: &Texts<'_>,
        kind: AppText,
        bind: &str,
        size: i32,
        bolds: &[bool],
    ) -> (String, f32) {
        texts.widest(kind, bind, size, bolds).expect("a string")
    }

    #[test]
    fn the_widest_digit_of_each_position_is_measured_not_assumed() {
        let lang = LintLang::for_tag("en").unwrap();
        let texts = en_texts(&lang, &Glyphs);
        assert_eq!(texts.widest_digit(24, false, 1..=9), 7);
        assert_eq!(
            texts.widest_digit(24, false, 0..=5),
            0,
            "no 7 among 0..=5: the lowest of equals"
        );
        let (clock, width) =
            widest_of_kind(&texts, AppText::ChapterStart, "item.start", 24, &[false]);
        // hours 77 (the widest tens and ones), minutes and seconds 07 (the widest ones; no 7 among the tens)
        assert_eq!(clock, "77:07:07");
        assert_eq!(
            width,
            4.0 * 16.0 + 4.0 * 10.0,
            "four 7s, two colons, two 0s"
        );
    }

    #[test]
    fn the_numbered_strings_use_the_widest_digit_in_the_languages_catalog() {
        let lang = LintLang::for_tag("en").unwrap();
        let texts = en_texts(&lang, &Glyphs);
        let (fallback, _) =
            widest_of_kind(&texts, AppText::ChapterFallback, "item.title", 24, &[false]);
        assert_eq!(fallback, "Chapter 777");
        let (kicker, _) =
            widest_of_kind(&texts, AppText::KickerPrefix, "title.kicker", 24, &[false]);
        assert_eq!(kicker, "S77, E777 \u{b7} ");
        let de = LintLang::for_tag("de").unwrap();
        let (fallback_de, _) = widest_of_kind(
            &en_texts(&de, &Glyphs),
            AppText::ChapterFallback,
            "item.title",
            24,
            &[false],
        );
        assert!(
            fallback_de.contains("777") && fallback_de != fallback,
            "{fallback_de}"
        );
    }

    #[test]
    fn a_bold_weight_widens_what_is_measured_and_the_wider_weight_wins() {
        let lang = LintLang::for_tag("en").unwrap();
        let texts = en_texts(&lang, &Glyphs);
        let (_, plain) = widest_of_kind(&texts, AppText::Label, "label.chapters", 24, &[false]);
        let (_, both) =
            widest_of_kind(&texts, AppText::Label, "label.chapters", 24, &[false, true]);
        assert_eq!(plain, 80.0);
        assert_eq!(both, 80.0 + 4.0 * 8.0);
    }

    #[test]
    fn repdigit_writes_a_digit_n_times() {
        assert_eq!(repdigit(7, 3), 777);
        assert_eq!(repdigit(1, 2), 11);
        assert_eq!(repdigit(9, 1), 9);
    }

    // ---- the room ----------------------------------------------------------------------------------

    #[test]
    fn judge_is_exact_at_the_room_and_strict_at_the_skew() {
        assert_eq!(judge(100.0, 100.0, 0.0), Verdict::Fits, "an exact fit fits");
        assert_eq!(judge(100.5, 100.0, 0.0), Verdict::Overflows);
        assert_eq!(judge(101.0, 100.0, 0.0), Verdict::Overflows, "one px over");
        assert_eq!(
            judge(99.0, 100.0, 0.0),
            Verdict::Fits,
            "at skew 0 nothing is within the margin"
        );
        assert_eq!(
            judge(90.0, 100.0, 10.0),
            Verdict::Fits,
            "exactly the skew to spare"
        );
        assert_eq!(judge(90.5, 100.0, 10.0), Verdict::Margin, "inside the skew");
        assert_eq!(
            judge(100.0, 100.0, 10.0),
            Verdict::Margin,
            "an exact fit has no margin"
        );
        assert_eq!(judge(100.5, 100.0, 10.0), Verdict::Overflows);
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn a_strip_cards_text_has_its_card_alone() {
        let ls = limits(
            rect(100.0, 50.0, 192.0, 24.0),
            Site::Card,
            false,
            &[("c".into(), rect(120.0, 50.0, 10.0, 10.0))],
            Some(30.0),
        );
        assert_eq!(
            ls,
            [Limit {
                room: 192.0,
                bound: Bound::Box
            }]
        );
    }

    #[test]
    fn a_hugging_label_leaves_out_its_own_box() {
        let ls = limits(rect(100.0, 50.0, 80.0, 24.0), Site::Band, true, &[], None);
        assert_eq!(
            ls,
            [Limit {
                room: SAFE.x + SAFE.w - 100.0,
                bound: Bound::Safe
            }]
        );
        let boxed = limits(rect(100.0, 50.0, 80.0, 24.0), Site::Band, false, &[], None);
        assert_eq!(boxed.len(), 2);
        assert_eq!(
            boxed[0],
            Limit {
                room: 80.0,
                bound: Bound::Box
            }
        );
    }

    #[test]
    fn a_control_bounds_the_text_only_when_it_is_right_of_it_on_its_line() {
        let text = rect(100.0, 50.0, 80.0, 24.0);
        let right_on_line = ("a".to_owned(), rect(300.0, 40.0, 60.0, 40.0));
        let right_other_line = ("b".to_owned(), rect(300.0, 80.0, 60.0, 40.0));
        let right_above = ("c".to_owned(), rect(300.0, 0.0, 60.0, 50.0));
        let left_on_line = ("d".to_owned(), rect(20.0, 40.0, 60.0, 40.0));
        let at_text = ("e".to_owned(), rect(100.0, 40.0, 60.0, 40.0));
        let ls = limits(
            text,
            Site::Band,
            true,
            &[
                right_on_line.clone(),
                right_other_line,
                right_above,
                left_on_line,
                at_text.clone(),
            ],
            None,
        );
        assert_eq!(
            ls,
            [
                Limit {
                    room: SAFE.x + SAFE.w - 100.0,
                    bound: Bound::Safe
                },
                Limit {
                    room: 200.0,
                    bound: Bound::Control("a".to_owned(), right_on_line.1)
                },
                Limit {
                    room: 0.0,
                    bound: Bound::Control("e".to_owned(), at_text.1)
                },
            ]
        );
    }

    #[test]
    fn the_vstack_width_is_a_limit_and_the_smallest_room_binds() {
        let ls = limits(
            rect(100.0, 50.0, 500.0, 24.0),
            Site::Band,
            false,
            &[("a".into(), rect(400.0, 50.0, 60.0, 24.0))],
            Some(250.0),
        );
        let tightest = binding(&ls).unwrap();
        assert_eq!(
            tightest.room, 150.0,
            "the room runs from the text's x to the vstack's right edge"
        );
        assert_eq!(tightest.bound, Bound::Vstack);
        let tie = [
            Limit {
                room: 5.0,
                bound: Bound::Box,
            },
            Limit {
                room: 5.0,
                bound: Bound::Safe,
            },
        ];
        assert_eq!(
            binding(&tie).unwrap().bound,
            Bound::Box,
            "the first of equals"
        );
        assert!(binding(&[]).is_none());
    }

    fn vstack(pointer: &str, w: Option<Val>) -> Object {
        Object {
            node: "vstack",
            pointer: pointer.to_owned(),
            fields: w.map(|w| vec![("w".to_owned(), w)]).unwrap_or_default(),
        }
    }

    #[test]
    fn the_nearest_enclosing_vstack_edge_counts() {
        let outer = vstack("/a", Some(Val::Int(400)));
        let middle = vstack("/a/vstack/cells/1", Some(Val::Num(250.5)));
        let fill = vstack(
            "/a/vstack/cells/1/vstack/cells/0",
            Some(Val::Name("fill".to_owned())),
        );
        let bare = vstack("/b", None);
        let cell = Object {
            node: "text",
            pointer: "/a/vstack/cells/1/vstack/cells/0/vstack/cells/3/text".to_owned(),
            fields: vec![],
        };
        let nodes: BTreeMap<&str, &Object> = [&outer, &middle, &fill, &bare, &cell]
            .into_iter()
            .map(|o| (o.pointer.as_str(), o))
            .collect();
        // the outer vstack is placed at x 100, the middle one at x 200, the fill one at 210; /b is placed too
        let placed = |pointer: &str| match pointer {
            "/a" => Some(rect(100.0, 0.0, 400.0, 90.0)),
            "/a/vstack/cells/1" => Some(rect(200.0, 0.0, 250.5, 90.0)),
            "/a/vstack/cells/1/vstack/cells/0" => Some(rect(210.0, 0.0, 240.0, 90.0)),
            "/b" => Some(rect(50.0, 0.0, 10.0, 10.0)),
            _ => None,
        };
        // the fill vstack is no limit; of the fixed ones the middle edge (200 + 250.5) is nearer than the outer (100 + 400)
        assert_eq!(
            enclosing_vstack_right(&cell.pointer, &nodes, &placed),
            Some(450.5)
        );
        // inside the outer one alone, its edge is its x plus its w
        assert_eq!(
            enclosing_vstack_right("/a/vstack/cells/0/text", &nodes, &placed),
            Some(500.0)
        );
        assert_eq!(
            enclosing_vstack_right("/b/vstack/cells/0/text", &nodes, &placed),
            None
        );
        assert_eq!(
            enclosing_vstack_right("/elsewhere/text", &nodes, &placed),
            None
        );
        // a vstack the state does not place bounds nothing
        assert_eq!(
            enclosing_vstack_right("/a/vstack/cells/0/text", &nodes, &|_| None),
            None
        );
    }
}
