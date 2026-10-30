//! **The HUD layout compiler** (`docs/hud-plugins.md`, "Architecture", "Package format 1.0", "Cost"):
//! a parsed [`Package`] laid out ONCE in every state [`reach`](super::reach) enumerates, into the rects
//! the player reads and a flat draw list a renderer walks. Nothing is computed per frame afterwards.
//!
//! **Pure.** No IO, no global, no clock, no `cfg(test)` fork. The two things a layout needs from
//! outside arrive as values: text measurement as the existing [`Measure`] capability (the one
//! `classic::geometry` takes, so both sides measure identically), and the app's own strings as
//! [`AppLabels`] in [`CompileCtx`] (a pill is labelled by its command's app string; the caller builds
//! the labels from the viewer's language and a different language is a different compile).
//!
//! **States.** The compiler walks `reach`'s own enumeration (`Layout::valuations`, `Layout::explore`)
//! and evaluates every `when` with its own `Layout::holds`: there is no second state walk and no second
//! condition evaluator, so the state count is `reach`'s by construction. A state is
//! (open sections, slot) under a valuation of `chapters.present` and `title.is_episode`; a condition
//! the package does not name has the one value it holds.
//!
//! **`transport.paused`** is an axis of the compile ONLY when the package names it in a `when` or in a
//! number or gap (a key that moves geometry): then every state is compiled twice, paused and playing
//! ([`StateKey::paused`]), and the renderer picks one per frame. It never enters `reach`'s valuations (R1
//! counts it as holding either way), so a package that does not name it has `reach`'s state count exactly.
//! An ink or bold keyed on it moves nothing and stays [`Live`].
//!
//! **`item.*`** (`item.focused`, `item.is_current`) is the strip CARD's, and a card is laid out once for
//! every card. So a card's cell is always laid out and keeps its room, and whether a card DRAWS it is a
//! [`Live`] over the cell's `item.*` terms ([`CardOp::present`]); an ink or bold keyed on one is a [`Live`]
//! too. A number or gap keyed on one cannot be (it would be a different layout per card): the base value
//! applies and the compile reports a [`CompileFinding`], never a silent default.
//!
//! **Layout.** Bands stack bottom-up in one `plx_ui::vstack::VStack` (a band's `gap_before` is the gap
//! to the band above it, an absent band gives and takes none). A band's cells pack per alignment group
//! in `plx_ui::hstack::HStack` runs, `gap` apart, with `gap_before`/`gap_after` as `Item::before` /
//! `Item::after`; a segment is one `HStack` of controls; a `vstack` is a `VStack`. Widths come from the
//! same widget functions the hand-written layout calls (`TabPill::width_measured`,
//! `Button::pill_w_measured`, `classic::stand_in_w`, `up_next::layout_at`), never a re-derived padding.
//!
//! **Fill.** A start-aligned `w: fill` cell runs to the nearest edge of the next present cell of a
//! later group (a control's rect scaled by `CTRL_FOCUS_SCALE`, a slot stand-in's leftmost rect), minus
//! the band's `gap`: `classic::title_right` made the rule. A fill in a centre or end group takes the
//! row's free room, shared equally. A `facts`, `rect`, `playbar` or `strip` cell with no `w` is a fill
//! cell; server text with no `w` is too.
//!
//! **Stand-ins.** Up Next and Skip replace the segments `up_next.replaces` names. Those segments keep
//! their place in the layout (the others stay where they are) but draw and focus nothing; the stand-in
//! lands on the union of their Discs-slot rects, at the edge `up_next.align` names, so the transport
//! never jumps when a segment begins. Skip has TWO labels and so two widths: the state carries both
//! rects ([`SkipKind`]).
//!
//! **Cost** ([`Cost`]) is REPORTED per state, never enforced: the ceilings are calibrated on the
//! television (PR7, PR8). Its terms: draw ops, glyph runs, texture bytes and blended pixel area.

use std::ffi::CString;

use plx_machine::machine::Measure;
use plx_media::player::text_style::HudBand;
use plx_ui::consts::{SAFE, SCR_H, SCR_W};
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::vstack::{Anchor, VStack};
use plx_ui::widgets::{Button, TabPill, CTRL_FOCUS_SCALE, HAIRLINE_H};
use plx_ui::{theme, Rect};

use super::parse::{Cond, CondTerm, Object, Package, Val};
use super::reach::{Layout, Slot, State, Valuation};
use super::schema;
use super::{pointer_push, validate::each_object};
use crate::player_hud::classic::{stand_in_w, KNOB_GLOW_R};
use crate::player_hud::geometry::{HudGeometry, RowAnchor};
use crate::player_hud::BTN_N;
use crate::up_next;

/// `StateKey::open` bit for the chapters section.
pub const OPEN_CHAPTERS: u8 = 1;
/// `StateKey::open` bit for the info section.
pub const OPEN_INFO: u8 = 2;

/// A colour, straight (not premultiplied) RGBA: `theme`'s own representation.
pub type Rgba = [f32; 4];

// ---------------------------------------------------------------------------------------------------
// The inputs
// ---------------------------------------------------------------------------------------------------

/// The app's own strings a layout measures, in the viewer's language. Plain data: the caller builds it
/// (from `plx_platform::i18n` in the app), so the compiler reads no registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppLabels {
    /// the Info tab (`widgets_player_info`), also the text of a `label.info` binding
    pub tab_info: String,
    /// the Chapters tab (`widgets_player_chapters`), also the text of `label.chapters`
    pub tab_chapters: String,
    /// the Up Next primary (`widgets_next_episode`)
    pub next_episode: String,
    /// the Up Next secondary (`widgets_next_credits`)
    pub watch_credits: String,
    /// the Skip pill over an intro
    pub skip_intro: String,
    /// the Skip pill over credits
    pub skip_credits: String,
    /// the Info card's From the Beginning button
    pub from_beginning: String,
    /// the Info card's Go to Show button (an episode)
    pub go_to_show: String,
    /// the Info card's Go to Movie button
    pub go_to_movie: String,
}

impl AppLabels {
    /// The label a `pill` or `button` carries for `command` (a `toggle_section`'s `section` picks the
    /// tab); an empty string for a command with no app string here, which measures as padding only.
    fn of(&self, command: &str, section: Option<&str>, episode: bool) -> &str {
        match (command, section) {
            ("toggle_section", Some("info")) => &self.tab_info,
            ("toggle_section", Some("chapters")) => &self.tab_chapters,
            ("from_beginning", _) => &self.from_beginning,
            ("go_to_detail", _) if episode => &self.go_to_show,
            ("go_to_detail", _) => &self.go_to_movie,
            _ => "",
        }
    }

    /// Every label `command` can show, whichever variant the item holds: an `actions` button takes
    /// one width over all of them.
    fn variants(&self, command: &str, section: Option<&str>) -> Vec<&str> {
        match command {
            "go_to_detail" => vec![&self.go_to_show, &self.go_to_movie],
            _ => vec![self.of(command, section, true)],
        }
    }
}

/// Everything besides the package and the measure that a compile depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileCtx {
    pub labels: AppLabels,
}

/// The plan's cache key for a compiled package: the package's bytes' digest, the app release (a layout
/// rule can change between releases) and the language (a label's width does). The TYPE only: the caller
/// hashes, and no cache, store or global lives here.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompileKey {
    pub sha256: [u8; 32],
    pub app_version: String,
    pub lang: String,
}

// ---------------------------------------------------------------------------------------------------
// The outputs
// ---------------------------------------------------------------------------------------------------

/// One state of the HUD: the open sections (bits [`OPEN_CHAPTERS`], [`OPEN_INFO`]), what occupies the
/// control row, and the valuation of the content conditions it was compiled under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateKey {
    pub open: u8,
    pub slot: Slot,
    /// `chapters.present`; `true` for a package that does not name it
    pub chapters: bool,
    /// `title.is_episode`; `true` for a package that does not name it
    pub episode: bool,
    /// `transport.paused`: an axis of the compile only when a `when` or a number or gap of the package
    /// names it (see the module doc); `false` for a package that does not
    pub paused: bool,
}

/// Which Skip pill: the two labels have two widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipKind {
    Intro,
    Credits,
}

/// A control's widget (`docs/hud-plugins.md`, "Control item").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widget {
    Disc,
    Pill,
    Button,
}

/// A control's resting face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rest {
    Film,
    Bare,
    Solid,
    Custom,
}

/// A value that depends on something only the frame knows (`transport.paused`, `item.*`): the value
/// when no case holds, and the cases in document order, the first holding one winning.
#[derive(Clone, Debug, PartialEq)]
pub struct Live<T> {
    pub base: T,
    pub cases: Vec<(CondTerm, T)>,
}

impl<T: Copy> Live<T> {
    fn fixed(base: T) -> Self {
        Self { base, cases: Vec::new() }
    }

    /// Every value it can take: the base, then each case's.
    pub fn each(&self) -> impl Iterator<Item = &T> {
        std::iter::once(&self.base).chain(self.cases.iter().map(|(_, v)| v))
    }

    /// The value this frame: the first case whose term holds, else the base. `name_holds` answers
    /// whether the term's NAME (`transport.paused`, `item.focused`) is true this frame; the term's own
    /// negation is applied here.
    pub fn at(&self, mut name_holds: impl FnMut(&str) -> bool) -> T {
        self.cases
            .iter()
            .find(|(term, _)| name_holds(&term.name) != term.negated)
            .map_or(self.base, |(_, v)| *v)
    }
}

/// One control, placed: its id, its command, the widget it is and the rect it is DRAWN at.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlRect {
    pub id: String,
    pub command: String,
    pub widget: Widget,
    pub rest: Rest,
    pub rect: Rect,
    /// the band holding it
    pub band: String,
    /// the section a `toggle_section` opens (`info`, `chapters`)
    pub section: Option<String>,
}

/// The scrub bar's rects: the track, and the pointer grab band around it (the lowest stop, #162).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybarRects {
    pub track: Rect,
    pub grab: Rect,
}

/// What the Up Next offer occupies. The still and caption are decor; the two buttons are the targets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UpNextRects {
    pub still: Option<Rect>,
    pub caption: Option<Rect>,
    pub credits: Rect,
    pub next: Rect,
}

/// Where the stand-ins land: the row they replace at its floor width, the anchor, and what each of
/// the two would occupy.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Standin {
    /// The replaced row's region at its floor width (`classic::ctrl_row_hit_rect`): the row GROUP's
    /// extent, identical in every slot.
    pub row: Rect,
    pub anchor: RowAnchor,
    pub up_next: Option<UpNextRects>,
    /// The Skip pill per [`SkipKind`] (index `SkipKind as usize`).
    pub skip: Option<[Rect; 2]>,
}

/// One thing to draw, in draw order. Coordinates are the logical 1920 x 1080 canvas.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    /// the dark ground behind the HUD
    Scrim { rect: Rect },
    /// a `rect` cell's filled panel
    Ground { rect: Rect, ink: Live<Rgba>, radius: f32 },
    Text {
        rect: Rect,
        bind: String,
        size: i32,
        bold: Live<bool>,
        ink: Live<Rgba>,
        alpha: f32,
        max_lines: u8,
        leading: f32,
    },
    Image { rect: Rect, bind: String, radius: f32 },
    Icon { rect: Rect, asset: String, ink: Live<Rgba> },
    Rule { rect: Rect, ink: Live<Rgba> },
    Facts {
        rect: Rect,
        size: i32,
        bold: Live<bool>,
        ink: Live<Rgba>,
        order: Vec<String>,
        badges: Vec<String>,
        genres_max: u32,
        playback_fact: bool,
    },
    Playbar {
        track: Rect,
        grab: Rect,
        fill: Live<Rgba>,
        track_ink: Live<Rgba>,
        clock_size: i32,
    },
    /// The chapters strip: `visible` cards at `pitch`, each laid out as `template` is at card 0.
    Strip {
        rect: Rect,
        card: Rect,
        pitch: f32,
        visible: usize,
        radius: f32,
        template: Vec<CardOp>,
    },
    Control(ControlRect),
    /// The Up Next offer (slot UpNext only); its inside is app-drawn.
    UpNext { rects: UpNextRects, credits_rest: Rest, next_rest: Rest },
    /// The Skip pill (slot Skip only), at both widths.
    SkipPill { rects: [Rect; 2] },
}

/// One op of a strip card's template and whether THIS card draws it: the card's `item.*` conditions
/// are the frame's, so presence is a [`Live`] over them, evaluated per card per frame.
#[derive(Clone, Debug, PartialEq)]
pub struct CardOp {
    pub op: DrawOp,
    pub present: Live<bool>,
}

/// One op's share of the cost terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Contributor {
    /// the op's index in [`CompiledState::ops`]
    pub op: usize,
    pub draw_ops: usize,
    pub glyph_runs: usize,
    pub texture_bytes: u64,
    pub blended_px: u64,
}

/// A state's cost, REPORTED (the ceilings are calibrated on the television in PR7 and PR8).
///
/// - `draw_ops`: one per op, except a strip (its cards' ops: `visible` x the template, plus the focus
///   ring; an upper bound where a card's `item.*` condition would drop an op) and the Up Next offer (two
///   pills, plus the still and the caption it draws).
/// - `glyph_runs`: one per text; the playbar's two clocks; a facts row's chips; a pill's or button's
///   label; the Up Next offer's two labels and caption; the Skip pill's label.
/// - `texture_bytes`: 4 bytes per pixel of every image (a strip's per visible card, Up Next's still) and
///   every glyph a disc or an icon cell draws (`size` squared).
/// - `blended_px`: the area, rounded per rect, of every translucent fill: the scrim, a ground or rule
///   whose ink is translucent, the playbar's track, a control's plate unless its rest face is `solid` or
///   `bare`. A rounded corner is not subtracted: it is an upper bound.
/// - `top`: the three ops that contribute most, by blended area, then texture bytes, then glyph runs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cost {
    pub draw_ops: usize,
    pub glyph_runs: usize,
    pub texture_bytes: u64,
    pub blended_px: u64,
    pub top: Vec<Contributor>,
}

/// One state, laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledState {
    pub key: StateKey,
    /// every placed control, in draw order, keyed by its id
    pub controls: Vec<ControlRect>,
    /// the draw list: the scrim first, then the bands in `z` order
    pub ops: Vec<DrawOp>,
    pub scrim: Option<Rect>,
    /// every present band's box, top to bottom
    pub bands: Vec<(String, Rect)>,
    pub playbar: Option<PlaybarRects>,
    pub standin: Standin,
    /// the origin of the cell holding the title text (`Rect::new(x, y, 0, 0)`)
    pub title: Rect,
    /// the bottom band's origin and height (`Rect::new(x, y, 0, h)`)
    pub tab_row: Rect,
    /// the control ids of the replaced row's segments in declared order, `None` for a control that is
    /// absent in this valuation: the stable item indices of [`HudGeometry::items`]
    pub row_ids: Vec<Option<String>>,
    pub cost: Cost,
    /// The package's caption band ([`Compiled::band`]): the same in every state.
    pub band: HudBand,
    /// A Skip state only, and only when the credits pill changes anything: the whole state as laid out
    /// when the pill is the CREDITS one (see [`CompiledState::skip_variant`]).
    pub skip_alt: Option<Box<CompiledState>>,
}

impl CompiledState {
    /// The state as it draws under `skip`: the Skip pill's two labels have two widths, and a start-aligned
    /// fill in the pill's band (a title) stops short of the one actually shown. `self` is the
    /// [`SkipKind::Intro`] layout and every non-Skip state's; a Skip state whose credits layout differs
    /// carries it in [`CompiledState::skip_alt`].
    pub fn skip_variant(&self, skip: SkipKind) -> &CompiledState {
        match (skip, &self.skip_alt) {
            (SkipKind::Credits, Some(alt)) => alt,
            _ => self,
        }
    }

    /// The control called `id`.
    pub fn control(&self, id: &str) -> Option<&ControlRect> {
        self.controls.iter().find(|c| c.id == id)
    }

    /// The ops that are decor: everything but a control (the scrim included).
    pub fn decor(&self) -> impl Iterator<Item = &DrawOp> {
        self.ops.iter().filter(|op| !matches!(op, DrawOp::Control(_)))
    }

    /// **This state as the report the player's consumers read** ([`HudGeometry`]): the grab band, the
    /// bar, the row at its floor width and its anchor, item `i` of whatever occupies the row at the
    /// rect it is drawn at (the first [`BTN_N`] controls of the replaced row's segments in declared
    /// order for the package's own controls, `[credits, next]` for Up Next, `[pill]` for Skip), the tab
    /// row's and the title's origins, and the two section tabs' pills.
    pub fn geometry(&self, skip: SkipKind) -> HudGeometry {
        let shown = self.skip_variant(skip);
        let mut g = HudGeometry {
            grab: Rect::default(),
            playbar: Rect::default(),
            row: self.standin.row,
            row_anchor: self.standin.anchor,
            items: [None; BTN_N as usize],
            tab_row: self.tab_row,
            title: shown.title,
            tabs: [self.tab("info"), self.tab("chapters")],
            band: self.band,
        };
        if let Some(bar) = self.playbar {
            g.grab = bar.grab;
            g.playbar = bar.track;
        }
        match self.key.slot {
            Slot::Discs => {
                // PR7 OBLIGATION: `HudGeometry::items` holds `BTN_N` = 5 stops (classic's row), so this zip
                // drops every row item past the fifth. The reference has nine (forward, chapters, info and
                // More among them) and R1 guarantees More: they stay in `controls` and in `row_ids`, whole,
                // and widening `items` to the longest row a package can place is PR7's change to the
                // report and its consumers (the focus group, the pointer stops), not this compiler's.
                for (cell, id) in g.items.iter_mut().zip(&self.row_ids) {
                    *cell = id.as_deref().and_then(|id| self.control(id)).map(|c| c.rect);
                }
            }
            Slot::UpNext => {
                if let Some(u) = self.standin.up_next {
                    g.items[up_next::BTN_CREDITS as usize] = Some(u.credits);
                    g.items[up_next::BTN_NEXT as usize] = Some(u.next);
                }
            }
            Slot::Skip => g.items[0] = self.standin.skip.map(|s| s[skip as usize]),
        }
        g
    }

    /// The pill of the `toggle_section` control that opens `section`.
    fn tab(&self, section: &str) -> Option<Rect> {
        self.controls.iter().find(|c| c.command == "toggle_section" && c.section.as_deref() == Some(section)).map(|c| c.rect)
    }

    /// The top of the highest drawn extent: interactive only (a control, the bar with its knob, Up Next's
    /// two buttons, the Skip pill at either width), or under `block` every drawn extent but the scrim,
    /// Up Next's still and caption included.
    fn top(&self, block: bool) -> Option<f32> {
        let mut ys: Vec<f32> = self.controls.iter().map(|c| c.rect.y).collect();
        ys.extend(self.playbar.map(|p| bar_top(p.track)));
        if let Some(u) = self.standin.up_next {
            ys.extend([u.credits.y, u.next.y]);
            if block {
                ys.extend(u.still.map(|r| r.y));
                ys.extend(u.caption.map(|r| r.y));
            }
        }
        ys.extend(self.standin.skip.into_iter().flatten().map(|r| r.y));
        if block {
            for op in &self.ops {
                match op {
                    DrawOp::Scrim { .. } | DrawOp::UpNext { .. } | DrawOp::SkipPill { .. } | DrawOp::Control(_) => {}
                    DrawOp::Ground { rect, .. }
                    | DrawOp::Text { rect, .. }
                    | DrawOp::Image { rect, .. }
                    | DrawOp::Icon { rect, .. }
                    | DrawOp::Rule { rect, .. }
                    | DrawOp::Facts { rect, .. }
                    | DrawOp::Strip { rect, .. } => ys.push(rect.y),
                    DrawOp::Playbar { track, .. } => ys.push(bar_top(*track)),
                }
            }
        }
        ys.into_iter().reduce(f32::min)
    }

    /// Set the package's one caption band on this state (and on the credits layout it carries).
    fn set_band(&mut self, band: HudBand) {
        self.band = band;
        if let Some(alt) = self.skip_alt.as_deref_mut() {
            alt.band = band;
        }
    }
}

/// The top of the bar as the caption band counts it: the track, or the focused knob's glow around the
/// playhead where that reaches higher ([`KNOB_GLOW_R`] above the track's middle), so the band does not
/// move with focus ("Captions": the bar and its knob).
fn bar_top(track: Rect) -> f32 {
    track.y.min(track.y + track.h * 0.5 - KNOB_GLOW_R)
}

/// A package compiled in every state `reach` enumerates.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    states: Vec<CompiledState>,
    /// which of `chapters.present`, `title.is_episode` and `transport.paused` the package names; one it
    /// does not name has a single value (`true`, `true`, `false`)
    named: Named,
    /// the one caption band, in every state
    band: HudBand,
    findings: Vec<CompileFinding>,
}

/// Which dimensions of a compile the package names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Named {
    chapters: bool,
    episode: bool,
    paused: bool,
}

/// A value the compiler could not carry as the package wrote it. Not a registered finding code: PR5d
/// gives it one (the `06xx` family beside the text-fit codes) and prints it with the other lint findings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileFinding {
    /// the keyed value's RFC 6901 pointer
    pub pointer: String,
    /// the `item.*` term that keys it
    pub term: String,
    /// what is wrong, in one sentence
    pub message: String,
}

impl Compiled {
    /// Every state, in `reach`'s order (valuation by valuation, breadth first from the root).
    pub fn states(&self) -> &[CompiledState] {
        &self.states
    }

    /// The state compiled under exactly `key`.
    pub fn state(&self, key: StateKey) -> Option<&CompiledState> {
        self.states.iter().find(|s| s.key == key)
    }

    /// The state for `open` and `slot` under the content the viewer has: a condition the package does
    /// not name is ignored, so a query about `episode` finds the one valuation there is.
    pub fn lookup(&self, open: u8, slot: Slot, chapters: bool, episode: bool, paused: bool) -> Option<&CompiledState> {
        self.state(StateKey {
            open,
            slot,
            chapters: chapters || !self.named.chapters,
            episode: episode || !self.named.episode,
            paused: paused && self.named.paused,
        })
    }

    /// What the compiler could not carry as written (see [`CompileFinding`]); empty for a package that
    /// keys no number or gap on a strip card's `item.*` term.
    pub fn findings(&self) -> &[CompileFinding] {
        &self.findings
    }

    /// **The geometry report of one state** ([`CompiledState::geometry`]); `None` for a state the
    /// package cannot reach. The Skip pill is the one item whose width depends on a runtime label, so
    /// `skip` picks its rect.
    pub fn geometry(&self, key: StateKey, skip: SkipKind) -> Option<HudGeometry> {
        self.state(key).map(|s| s.geometry(skip))
    }

    /// **The caption band this layout occupies** (`docs/hud-plugins.md`, "Captions"): the top of the
    /// highest drawn interactive extent (under `band: "block"`, of any drawn extent) of the compact
    /// state, over its three slots and every valuation, the bar counting with its focused knob so the
    /// band never moves with focus. Classic's is `HudBand::CLASSIC`. It is also the `band` of every
    /// [`HudGeometry`] this package reports: one value, not two.
    pub fn band(&self) -> HudBand {
        self.band
    }
}

// ---------------------------------------------------------------------------------------------------
// The compiler
// ---------------------------------------------------------------------------------------------------

/// Lay `package` out in every reachable state. Total over a package that parsed: a key the table made
/// optional and the package left out reads as its default (a gap 0, a one-line text, the centre).
pub fn compile(package: &Package, measure: &dyn Measure, ctx: CompileCtx) -> Compiled {
    let layout = Layout::read(&package.layout);
    let frame = scan_frame(&package.layout);
    let mut states = Vec::new();
    let pauses: &[bool] = if frame.paused { &[false, true] } else { &[false] };
    for &paused in pauses {
        for v in layout.valuations() {
            for state in layout.explore(v) {
                let env = Env { package, layout: &layout, measure, labels: &ctx.labels, state, v, paused };
                states.push(env.compile_state());
            }
        }
    }
    let block = package.layout.get("band").and_then(Val::as_str) == Some("block");
    let band = states
        .iter()
        .filter(|s| s.key.open == 0)
        .filter_map(|s| s.top(block))
        .reduce(f32::min)
        .map_or(HudBand::CLASSIC, HudBand::from_controls_top);
    for state in &mut states {
        state.set_band(band);
    }
    Compiled {
        states,
        named: Named { chapters: layout.named_chapters, episode: layout.named_episode, paused: frame.paused },
        band,
        findings: frame.findings,
    }
}

/// What a package's conditions ask of the frame, read from the typed tree.
struct Frame {
    /// a `when`, or a number or gap, names `transport.paused`: it is an axis of the compile
    paused: bool,
    /// a number or gap keyed on an `item.*` term
    findings: Vec<CompileFinding>,
}

/// Is a keyed value a paint (an ink, `bold`)? Those are carried [`Live`]; every other keyable key (a
/// number, a gap, a width) moves geometry.
fn is_paint(base: &Val) -> bool {
    matches!(base, Val::Token(_) | Val::Ref(_) | Val::Bool(_))
}

fn scan_frame(root: &Object) -> Frame {
    let mut frame = Frame { paused: false, findings: Vec::new() };
    each_object(root, &mut |object| {
        for (key, value) in &object.fields {
            match value {
                Val::Cond(cond) => frame.paused |= cond.terms.iter().any(|t| t.name == PAUSED),
                Val::Keyed(keyed) if !is_paint(&keyed.base) => {
                    for term in keyed.cases.iter().flat_map(|(cond, _)| &cond.terms) {
                        if term.name == PAUSED {
                            frame.paused = true;
                        } else if is_item(&term.name) {
                            frame.findings.push(CompileFinding {
                                pointer: pointer_push(&object.pointer, key),
                                term: term.name.clone(),
                                message: format!(
                                    "`{key}` is keyed on `{}`, but a strip card is laid out once for every card, so only its base value applies",
                                    term.name
                                ),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    });
    frame
}

/// The twelve content kinds of a cell.
const CELL_KINDS: [&str; 12] = [
    "text", "image", "icon", "rule", "rect", "facts", "vstack", "spacer", "playbar", "segment", "strip", "actions",
];

/// The cell's one content key and its object.
fn content(cell: &Object) -> Option<(&'static str, &Object)> {
    CELL_KINDS.iter().find_map(|&k| cell.get(k).and_then(Val::as_object).map(|o| (k, o)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grp {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Dim {
    Px(f32),
    Fill,
}

#[derive(Clone, Copy, Debug)]
struct Size {
    w: Dim,
    h: f32,
    /// a vertical `fill` rule: as tall as its row
    stretch: bool,
}

impl Size {
    const fn new(w: Dim, h: f32) -> Self {
        Self { w, h, stretch: false }
    }
}

/// One cell of a row, measured.
struct CellIn<'p> {
    kind: &'static str,
    o: &'p Object,
    grp: Grp,
    valign: VAlign,
    size: Size,
    before: Option<f32>,
    after: Option<f32>,
    /// a segment the stand-in replaces
    replaced: bool,
    /// the `item.*` terms of its `when`: the card draws it only when they hold
    gate: Vec<CondTerm>,
}

/// A segment's or an `actions` cell's controls: index-stable, `None` where absent.
struct Controls<'p> {
    ctl: Vec<Option<Ctl<'p>>>,
    items: Vec<Item>,
    gap: f32,
}

struct Ctl<'p> {
    o: &'p Object,
    id: &'p str,
    command: &'p str,
    widget: Widget,
    rest: Rest,
    size: f32,
}

/// The placement mode of one pass over the bands.
struct Mode<'m> {
    /// the segment ids Up Next replaces
    replaces: &'m [String],
    /// replaced segments keep their place but draw and focus nothing
    hide: bool,
    /// the stand-in's leftmost (scaled) edge, for the host band's fill
    standin_left: Option<f32>,
    /// the band that hosts the stand-in
    host: &'m str,
}

/// What one pass over the bands produced.
#[derive(Default)]
struct Out {
    ops: Vec<DrawOp>,
    /// `ops[i]`'s presence under the `item.*` terms in force when it was pushed (a strip card's)
    present: Vec<Live<bool>>,
    /// the `item.*` terms of the `when`s around what is being emitted
    gate: Vec<CondTerm>,
    controls: Vec<ControlRect>,
    /// the rects of the replaced segments
    replaced: Vec<Rect>,
    title: Option<Rect>,
    playbar: Option<PlaybarRects>,
    band: String,
}

impl Out {
    /// Append `op`, present when every `item.*` term in force holds.
    fn push(&mut self, op: DrawOp) {
        self.present.push(presence(&self.gate));
        self.ops.push(op);
    }
}

struct Band<'p> {
    id: &'p str,
    before: Option<f32>,
    gap: f32,
    z: i64,
    cells: Vec<&'p Object>,
}

struct Env<'e, 'p> {
    package: &'p Package,
    layout: &'e Layout<'p>,
    measure: &'e dyn Measure,
    labels: &'e AppLabels,
    state: State,
    v: Valuation,
    /// `transport.paused` for this compile
    paused: bool,
}

/// A default the table gives `key` of `node`, as its JSON text.
fn default_text(node: &str, key: &str) -> Option<&'static str> {
    schema::field(schema::node(node)?, key)?.default
}

fn default_num(node: &str, key: &str) -> f32 {
    default_text(node, key).and_then(|t| t.parse().ok()).unwrap_or(0.0)
}

fn default_name(node: &str, key: &str) -> &'static str {
    default_text(node, key).map_or("", |t| t.trim_matches('"'))
}

/// The condition a package names to key something on the pause.
const PAUSED: &str = "transport.paused";

/// An `item.*` term: the strip card's, never the state's.
fn is_item(name: &str) -> bool {
    name.starts_with("item.")
}

fn is_live(name: &str) -> bool {
    name == PAUSED || is_item(name)
}

/// The `item.*` terms of `o`'s `when`.
fn item_terms(o: &Object) -> Vec<CondTerm> {
    match o.get("when") {
        Some(Val::Cond(cond)) => cond.terms.iter().filter(|t| is_item(&t.name)).cloned().collect(),
        _ => Vec::new(),
    }
}

/// Whether a card draws what `terms` gate: present unless one fails, each failure the term's negation.
fn presence(terms: &[CondTerm]) -> Live<bool> {
    Live {
        base: true,
        cases: terms.iter().map(|t| (CondTerm { negated: !t.negated, name: t.name.clone() }, false)).collect(),
    }
}

impl<'e, 'p> Env<'e, 'p> {
    // ---- reading the package ----

    /// Does `cond` hold in this state, under this compile's `transport.paused`? An `item.*` term is the
    /// card's: callers strip those first ([`Self::when`], [`Self::resolve`]), as `reach` holds them false.
    fn holds(&self, cond: &Cond) -> bool {
        self.layout.holds(cond, self.state, self.v, self.paused)
    }

    /// Is the element `o` laid out in this state? A strip card's `item.*` terms are not asked: a card's
    /// cells are laid out once for every card and keep their room whichever cards draw them (whether
    /// one does is [`item_terms`]', carried per card).
    fn when(&self, o: &Object) -> bool {
        match o.get("when") {
            Some(Val::Cond(cond)) => {
                let kept = Cond { terms: cond.terms.iter().filter(|t| !is_item(&t.name)).cloned().collect() };
                self.holds(&kept)
            }
            _ => true,
        }
    }

    /// A keyed value resolved in this state: the first case that holds, else the base. A case keyed on an
    /// `item.*` term is skipped (a layout cannot depend on the card: `scan_frame` reports it).
    fn resolve<'v>(&self, val: &'v Val) -> &'v Val {
        match val {
            Val::Keyed(k) => k
                .cases
                .iter()
                .filter(|(cond, _)| !cond.terms.iter().any(|t| is_item(&t.name)))
                .find(|(cond, _)| self.holds(cond))
                .map_or(k.base.as_ref(), |(_, v)| v),
            v => v,
        }
    }

    fn num(&self, o: &Object, key: &str) -> Option<f32> {
        self.resolve(o.get(key)?).as_f64().map(|n| n as f32)
    }

    /// A gap: a spacing token or px.
    fn gap(&self, o: &Object, key: &str) -> Option<f32> {
        match self.resolve(o.get(key)?) {
            Val::Name(token) => schema::space_token(token),
            other => other.as_f64().map(|n| n as f32),
        }
    }

    fn dim(&self, o: &Object, key: &str) -> Option<Dim> {
        match self.resolve(o.get(key)?) {
            Val::Name(n) if n == "fill" => Some(Dim::Fill),
            other => other.as_f64().map(|n| Dim::Px(n as f32)),
        }
    }

    fn size_px(&self, o: &Object, key: &str) -> i32 {
        o.get(key).and_then(|v| self.resolve(v).as_str()).and_then(schema::size_rung).unwrap_or(theme::size::BODY)
    }

    fn name<'v>(&self, o: &'v Object, key: &str) -> Option<&'v str> {
        self.resolve(o.get(key)?).as_str()
    }

    fn rgba(&self, val: &Val) -> Rgba {
        match val {
            Val::Token(name) => schema::ink_token(name).unwrap_or([1.0, 0.0, 1.0, 1.0]),
            Val::Ref(name) => self
                .package
                .palette
                .iter()
                .find(|(n, _)| n == name)
                .map_or([1.0, 0.0, 1.0, 1.0], |(_, c)| [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, 1.0]),
            _ => [1.0, 0.0, 1.0, 1.0],
        }
    }

    /// A value that may be keyed on the frame: the cases whose condition is the frame's are kept (both
    /// values); the state's are decided now.
    fn live<T: Copy>(&self, val: &Val, leaf: impl Fn(&Val) -> T) -> Live<T> {
        let Val::Keyed(k) = val else { return Live::fixed(leaf(val)) };
        let mut cases = Vec::new();
        for (cond, v) in &k.cases {
            match cond.terms.first() {
                Some(term) if is_live(&term.name) => cases.push((term.clone(), leaf(v))),
                _ if self.holds(cond) => return Live { base: leaf(v), cases },
                _ => {}
            }
        }
        Live { base: leaf(&k.base), cases }
    }

    fn live_ink(&self, o: &Object, key: &str) -> Live<Rgba> {
        o.get(key).map_or_else(|| Live::fixed([1.0, 0.0, 1.0, 1.0]), |v| self.live(v, |v| self.rgba(v)))
    }

    fn live_bold(&self, o: &Object) -> Live<bool> {
        o.get("bold").map_or_else(|| Live::fixed(false), |v| self.live(v, |v| v.as_bool().unwrap_or(false)))
    }

    fn list<'v>(&self, o: &'v Object, key: &str) -> &'v [Val] {
        o.get(key).and_then(Val::as_list).unwrap_or(&[])
    }

    fn objects<'v>(&self, o: &'v Object, key: &str) -> Vec<&'v Object> {
        self.list(o, key).iter().filter_map(Val::as_object).collect()
    }

    // ---- controls ----

    fn widget(o: &Object) -> Widget {
        match o.get("widget").and_then(Val::as_str) {
            Some("pill") => Widget::Pill,
            Some("button") => Widget::Button,
            _ => Widget::Disc,
        }
    }

    fn rest(o: &Object) -> Rest {
        match o.get("rest").and_then(Val::as_str).unwrap_or_else(|| default_name("item", "rest")) {
            "bare" => Rest::Bare,
            "solid" => Rest::Solid,
            "custom" => Rest::Custom,
            _ => Rest::Film,
        }
    }

    fn label(&self, item: &Object) -> &str {
        let command = item.get("command").and_then(Val::as_str).unwrap_or("");
        let section = item.get("section").and_then(Val::as_str);
        self.labels.of(command, section, self.v.episode)
    }

    /// The width a control draws at: a disc is its size, a pill and a button are as wide as their label.
    fn control_w(&self, item: &Object, widget: Widget, size: f32) -> f32 {
        let label = self.label(item);
        match widget {
            Widget::Disc => size,
            Widget::Pill => TabPill::width_measured(label, theme::size::BODY, self.measure),
            Widget::Button => Button::pill_w_measured(&cstring(label), theme::size::BODY, true, None, self.measure),
        }
    }

    /// Is this control gone in this valuation? Its own `when`, or Previous / Next, which come and go
    /// together with an episode (F6: the pair is the app's, not the package's, to collapse).
    fn absent(&self, item: &Object) -> bool {
        let pair = matches!(item.get("command").and_then(Val::as_str), Some("previous" | "next"));
        !self.when(item) || (pair && !self.v.episode)
    }

    /// The controls of a segment's (or `actions`') `items`; with `floor` the ones that CAN be absent
    /// (a Previous / Next, an item with a `when`) are collapsed whatever the valuation holds.
    fn controls(&self, o: &'p Object, gap: f32, floor: bool) -> Controls<'p> {
        let mut ctl = Vec::new();
        let mut items = Vec::new();
        for item in self.objects(o, "items") {
            let can_go = floor
                && (item.get("when").is_some() || matches!(item.get("command").and_then(Val::as_str), Some("previous" | "next")));
            if can_go || self.absent(item) {
                ctl.push(None);
                items.push(Item::absent());
                continue;
            }
            let widget = Self::widget(item);
            let size = item.get("size").and_then(Val::as_f64).unwrap_or(0.0) as f32;
            let mut it = Item::new(self.control_w(item, widget, size));
            if let Some(g) = self.gap(item, "gap_after") {
                it = it.after(g);
            }
            if let Some(g) = self.gap(item, "gap_before") {
                it = it.before(g);
            }
            items.push(it);
            ctl.push(Some(Ctl {
                o: item,
                id: item.get("id").and_then(Val::as_str).unwrap_or(""),
                command: item.get("command").and_then(Val::as_str).unwrap_or(""),
                widget,
                rest: Self::rest(item),
                size,
            }));
        }
        Controls { ctl, items, gap }
    }

    /// Does an `actions` cell run down (`column`) rather than along (`row`, the default)?
    fn column(&self, o: &Object) -> bool {
        self.name(o, "axis").unwrap_or_else(|| default_name("actions", "axis")) == "column"
    }

    /// An `actions` cell's one button width: the widest label over every variant, floored at `min_w`.
    fn action_w(&self, o: &Object) -> f32 {
        let widest = self
            .objects(o, "items")
            .into_iter()
            .filter(|item| self.when(item))
            .flat_map(|item| {
                let command = item.get("command").and_then(Val::as_str).unwrap_or("");
                let section = item.get("section").and_then(Val::as_str);
                self.labels.variants(command, section)
            })
            .map(|label| Button::pill_w_measured(&cstring(label), theme::size::BODY, true, None, self.measure))
            .fold(0.0, f32::max);
        widest.max(self.num(o, "min_w").unwrap_or(0.0))
    }

    // ---- sizes ----

    fn text_h(&self, o: &Object) -> f32 {
        let size = self.size_px(o, "size");
        let leading = self.num(o, "leading").unwrap_or_else(|| self.measure.line_h(size));
        let lines = self.num(o, "max_lines").unwrap_or(1.0);
        leading * lines
    }

    fn text_label(&self, o: &Object) -> Option<&str> {
        match o.get("bind").and_then(Val::as_str) {
            Some("label.chapters") => Some(&self.labels.tab_chapters),
            Some("label.info") => Some(&self.labels.tab_info),
            _ => None,
        }
    }

    /// A cell's width (a number or fill) and height.
    fn size_of(&self, kind: &str, o: &'p Object) -> Size {
        match kind {
            "text" => {
                let w = self.dim(o, "w").unwrap_or_else(|| {
                    self.text_label(o).map_or(Dim::Fill, |label| {
                        let bold = self.live_bold(o).base;
                        Dim::Px(self.measure.width_str(label, self.size_px(o, "size"), bold))
                    })
                });
                Size::new(w, self.text_h(o))
            }
            "image" => Size::new(Dim::Px(self.num(o, "w").unwrap_or(0.0)), self.num(o, "h").unwrap_or(0.0)),
            "icon" => {
                let s = self.num(o, "size").unwrap_or(0.0);
                Size::new(Dim::Px(s), s)
            }
            "rule" => {
                let length = self.dim(o, "length").unwrap_or(Dim::Px(0.0));
                if self.name(o, "axis") == Some("v") {
                    match length {
                        Dim::Px(h) => Size::new(Dim::Px(HAIRLINE_H), h),
                        Dim::Fill => Size { w: Dim::Px(HAIRLINE_H), h: 0.0, stretch: true },
                    }
                } else {
                    Size::new(length, HAIRLINE_H)
                }
            }
            "rect" => {
                let pad = self.num(o, "pad").unwrap_or(0.0);
                let h = self.num(o, "h").unwrap_or_else(|| self.row_h(&self.objects(o, "cells")) + 2.0 * pad);
                Size::new(Dim::Fill, h)
            }
            "facts" => Size::new(Dim::Fill, self.measure.line_h(self.size_px(o, "size"))),
            "vstack" => {
                let (heights, widest) = self.vstack_items(o);
                let h = VStack::new(&heights.0, heights.1, Anchor::Top(0.0)).height();
                let w = self.dim(o, "w").unwrap_or(if widest > 0.0 { Dim::Px(widest) } else { Dim::Fill });
                Size::new(w, h)
            }
            "spacer" => Size::new(Dim::Px(self.num(o, "w").unwrap_or(0.0)), 0.0),
            "playbar" => Size::new(self.dim(o, "w").unwrap_or(Dim::Fill), self.num(o, "thickness").unwrap_or(0.0)),
            "segment" => {
                let c = self.controls(o, self.gap(o, "gap").unwrap_or(0.0), false);
                let w = HStack::new(&c.items, c.gap, Align::Leading(0.0)).width();
                let h = c.ctl.iter().flatten().map(|c| c.size).fold(0.0, f32::max);
                Size::new(Dim::Px(w), h)
            }
            "strip" => {
                let card_h = self.num(o, "card_h").unwrap_or(0.0);
                let below = o.get("card").and_then(Val::as_object).map_or(0.0, |card| {
                    let cells = self.below_cells(card);
                    if cells.is_empty() {
                        0.0
                    } else {
                        self.gap(card, "below_gap").unwrap_or(0.0) + cells.iter().map(|(_, t)| self.text_h(t)).sum::<f32>()
                    }
                });
                Size::new(self.dim(o, "w").unwrap_or(Dim::Fill), card_h + below)
            }
            "actions" => {
                let n = self.objects(o, "items").into_iter().filter(|i| self.when(i)).count() as f32;
                let (bw, ih, gap) = (self.action_w(o), self.num(o, "item_h").unwrap_or(0.0), self.gap(o, "gap").unwrap_or(0.0));
                let span = |len: f32, each: f32| len * each + (len - 1.0).max(0.0) * gap;
                if self.column(o) {
                    Size::new(Dim::Px(bw), span(n, ih))
                } else {
                    Size::new(Dim::Px(span(n, bw)), ih)
                }
            }
            _ => Size::new(Dim::Px(0.0), 0.0),
        }
    }

    /// The tallest present cell of a row.
    fn row_h(&self, cells: &[&'p Object]) -> f32 {
        cells
            .iter()
            .filter(|c| self.when(c))
            .filter_map(|c| content(c))
            .map(|(kind, o)| self.size_of(kind, o))
            .filter(|s| !s.stretch)
            .map(|s| s.h)
            .fold(0.0, f32::max)
    }

    /// The text cells under a strip card's art.
    fn below_cells(&self, card: &'p Object) -> Vec<(&'p Object, &'p Object)> {
        self.objects(card, "below")
            .into_iter()
            .filter(|cell| self.when(cell))
            .filter_map(|cell| cell.get("text").and_then(Val::as_object).map(|t| (cell, t)))
            .collect()
    }

    /// A vstack's present children as stack items, with the stack's gap, and its widest fixed child.
    fn vstack_items(&self, o: &'p Object) -> ((Vec<Item>, f32), f32) {
        let mut items = Vec::new();
        let mut widest = 0.0f32;
        for cell in self.objects(o, "cells") {
            if !self.when(cell) {
                continue;
            }
            let Some((kind, c)) = content(cell) else { continue };
            let size = self.size_of(kind, c);
            if let Dim::Px(w) = size.w {
                widest = widest.max(w);
            }
            items.push(self.gapped(Item::new(size.h), cell));
        }
        ((items, self.gap(o, "gap").unwrap_or(0.0)), widest)
    }

    /// `item` with the cell's `gap_before` / `gap_after`.
    fn gapped(&self, item: Item, cell: &Object) -> Item {
        let item = self.gap(cell, "gap_after").map_or(item, |g| item.after(g));
        self.gap(cell, "gap_before").map_or(item, |g| item.before(g))
    }

    // ---- rows ----

    fn cell_in(&self, cell: &'p Object, replaces: &[String]) -> Option<CellIn<'p>> {
        if !self.when(cell) {
            return None;
        }
        let (kind, o) = content(cell)?;
        let replaced = kind == "segment" && o.get("id").and_then(Val::as_str).is_some_and(|id| replaces.iter().any(|r| r == id));
        Some(CellIn {
            kind,
            o,
            grp: match cell.get("align").and_then(Val::as_str) {
                Some("center") => Grp::Center,
                Some("end") => Grp::End,
                _ => Grp::Start,
            },
            valign: match cell.get("valign").and_then(Val::as_str) {
                Some("top") => VAlign::Top,
                Some("bottom") => VAlign::Bottom,
                _ => VAlign::Center,
            },
            size: self.size_of(kind, o),
            before: self.gap(cell, "gap_before"),
            after: self.gap(cell, "gap_after"),
            replaced,
            gate: item_terms(cell),
        })
    }

    /// Every cell's `(x, w)` with `widths` as drawn: the groups pack independently, each anchored on its
    /// own edge of `boxr`.
    fn pack(&self, row: &[CellIn<'p>], widths: &[f32], gap: f32, boxr: Rect) -> Vec<(f32, f32)> {
        let mut out = vec![(0.0, 0.0); row.len()];
        for (grp, align) in [
            (Grp::Start, Align::Leading(boxr.x)),
            (Grp::Center, Align::Centre(boxr.x + boxr.w * 0.5)),
            (Grp::End, Align::Trailing(boxr.x + boxr.w)),
        ] {
            let idx: Vec<usize> = (0..row.len()).filter(|&i| row[i].grp == grp).collect();
            let items: Vec<Item> = idx
                .iter()
                .map(|&i| {
                    let mut it = Item::new(widths[i]);
                    if let Some(g) = row[i].after {
                        it = it.after(g);
                    }
                    if let Some(g) = row[i].before {
                        it = it.before(g);
                    }
                    it
                })
                .collect();
            for span in HStack::new(&items, gap, align).placed() {
                out[idx[span.i]] = (span.x, span.w);
            }
        }
        out
    }

    /// The left edges the fill of an earlier group must stop short of: each control scaled by the focus
    /// pop it may reach, any other cell as it stands.
    fn left_edges(&self, cell: &CellIn<'p>, x: f32, w: f32) -> Vec<f32> {
        let scaled = |x: f32, w: f32| Rect::new(x, 0.0, w, 1.0).scaled(CTRL_FOCUS_SCALE).x;
        match cell.kind {
            "segment" => {
                let c = self.controls(cell.o, self.gap(cell.o, "gap").unwrap_or(0.0), false);
                HStack::new(&c.items, c.gap, Align::Leading(x)).placed().map(|s| scaled(s.x, s.w)).collect()
            }
            "actions" => vec![scaled(x, w)],
            _ => vec![x],
        }
    }

    /// Lay out `cells` in `boxr`: pack, resolve the fills, then emit each cell at its rect.
    fn place_row(&self, cells: &[&'p Object], gap: f32, boxr: Rect, mode: &Mode<'_>, out: &mut Out) {
        let row: Vec<CellIn<'p>> = cells.iter().filter_map(|c| self.cell_in(c, mode.replaces)).collect();
        if row.is_empty() {
            return;
        }
        let mut widths: Vec<f32> = row.iter().map(|c| if let Dim::Px(w) = c.size.w { w } else { 0.0 }).collect();
        let fills = |grp_start: bool| -> Vec<usize> {
            (0..row.len()).filter(|&i| row[i].size.w == Dim::Fill && (row[i].grp == Grp::Start) == grp_start).collect()
        };

        // a fill in the centre or the end takes the row's free room, shared equally
        let loose = fills(false);
        if !loose.is_empty() {
            let mut packed = 0.0;
            let mut groups = 0;
            for grp in [Grp::Start, Grp::Center, Grp::End] {
                let items: Vec<Item> = (0..row.len())
                    .filter(|&i| row[i].grp == grp)
                    .map(|i| {
                        let it = Item::new(widths[i]);
                        let it = row[i].after.map_or(it, |g| it.after(g));
                        row[i].before.map_or(it, |g| it.before(g))
                    })
                    .collect();
                if !items.is_empty() {
                    packed += HStack::new(&items, gap, Align::Leading(0.0)).width();
                    groups += 1;
                }
            }
            let free = (boxr.w - packed - gap * (groups - 1).max(0) as f32).max(0.0);
            for &i in &loose {
                widths[i] = free / loose.len() as f32;
            }
        }

        // a fill at the start runs to the nearest edge of the next present cell of a later group
        for f in fills(true) {
            widths[f] = 0.0;
            let at = self.pack(&row, &widths, gap, boxr);
            let (x, _) = at[f];
            let group_end = (0..row.len())
                .filter(|&i| row[i].grp == Grp::Start)
                .map(|i| at[i].0 + at[i].1)
                .fold(x, f32::max);
            let later = (0..row.len())
                .filter(|&i| row[i].grp != Grp::Start && !(mode.hide && row[i].replaced))
                .flat_map(|i| self.left_edges(&row[i], at[i].0, at[i].1))
                .chain(mode.standin_left)
                .reduce(f32::min);
            let limit = later.map_or(boxr.x + boxr.w, |l| l - gap);
            widths[f] = (limit - x - (group_end - x)).max(0.0);
        }

        let at = self.pack(&row, &widths, gap, boxr);
        for (i, cell) in row.iter().enumerate() {
            let (x, w) = at[i];
            let h = if cell.size.stretch { boxr.h } else { cell.size.h };
            let y = match cell.valign {
                VAlign::Top => boxr.y,
                VAlign::Center => boxr.y + (boxr.h - h) * 0.5,
                VAlign::Bottom => boxr.y + boxr.h - h,
            };
            let r = Rect::new(x, y, w, h);
            if cell.replaced {
                out.replaced.push(r);
                if mode.hide {
                    continue;
                }
            }
            let mark = out.gate.len();
            out.gate.extend(cell.gate.iter().cloned());
            self.emit(cell.kind, cell.o, r, mode, out);
            out.gate.truncate(mark);
        }
    }

    // ---- emission ----

    fn emit(&self, kind: &str, o: &'p Object, r: Rect, mode: &Mode<'_>, out: &mut Out) {
        match kind {
            "text" => {
                let bind = o.get("bind").and_then(Val::as_str).unwrap_or("").to_owned();
                if bind.starts_with("title.") && out.title.is_none() {
                    out.title = Some(Rect::new(r.x, r.y, 0.0, 0.0));
                }
                out.push(DrawOp::Text {
                    rect: r,
                    bind,
                    size: self.size_px(o, "size"),
                    bold: self.live_bold(o),
                    ink: self.live_ink(o, "ink"),
                    alpha: self.num(o, "alpha").unwrap_or_else(|| default_num("text", "alpha")),
                    max_lines: self.num(o, "max_lines").map_or(1, |n| n.clamp(1.0, 255.0) as u8),
                    leading: self.num(o, "leading").unwrap_or_else(|| self.measure.line_h(self.size_px(o, "size"))),
                });
            }
            "image" => out.push(DrawOp::Image {
                rect: r,
                bind: o.get("bind").and_then(Val::as_str).unwrap_or("").to_owned(),
                radius: self.num(o, "radius").unwrap_or(0.0),
            }),
            "icon" => out.push(DrawOp::Icon {
                rect: r,
                asset: o.get("asset").and_then(Val::as_str).unwrap_or("").to_owned(),
                ink: self.live_ink(o, "ink"),
            }),
            "rule" => out.push(DrawOp::Rule { rect: r, ink: self.live_ink(o, "ink") }),
            "facts" => out.push(DrawOp::Facts {
                rect: r,
                size: self.size_px(o, "size"),
                bold: self.live_bold(o),
                ink: self.live_ink(o, "ink"),
                order: names(self.list(o, "order")),
                badges: names(self.list(o, "badges")),
                genres_max: self.num(o, "genres_max").map_or(0, |n| n.max(0.0) as u32),
                playback_fact: o.get("playback_fact").and_then(Val::as_bool).unwrap_or(false),
            }),
            "spacer" => {}
            "rect" => self.emit_rect(o, r, mode, out),
            "vstack" => self.emit_vstack(o, r, mode, out),
            "playbar" => {
                let above = self.num_or_default(o, "grab", "above");
                let below = self.num_or_default(o, "grab", "below");
                let grab = Rect::new(r.x, r.y - above, r.w, above + r.h + below);
                out.playbar = Some(PlaybarRects { track: r, grab });
                out.push(DrawOp::Playbar {
                    track: r,
                    grab,
                    fill: self.live_ink(o, "fill"),
                    track_ink: self.live_ink(o, "track"),
                    clock_size: self.size_px(o, "clock_size"),
                });
            }
            "segment" => {
                let c = self.controls(o, self.gap(o, "gap").unwrap_or(0.0), false);
                for span in HStack::new(&c.items, c.gap, Align::Leading(r.x)).placed() {
                    if let Some(ctl) = &c.ctl[span.i] {
                        let y = r.y + (r.h - ctl.size) * 0.5;
                        self.push_control(ctl, Rect::new(span.x, y, span.w, ctl.size), out);
                    }
                }
            }
            "strip" => self.emit_strip(o, r, mode, out),
            "actions" => self.emit_actions(o, r, out),
            _ => {}
        }
    }

    /// `node.key.sub`'s number: the package's value, else the table's default.
    fn num_or_default(&self, o: &Object, sub: &str, key: &str) -> f32 {
        o.get(sub)
            .and_then(Val::as_object)
            .and_then(|g| self.num(g, key))
            .unwrap_or_else(|| default_num(sub, key))
    }

    fn push_control(&self, ctl: &Ctl<'_>, rect: Rect, out: &mut Out) {
        let c = ControlRect {
            id: ctl.id.to_owned(),
            command: ctl.command.to_owned(),
            widget: ctl.widget,
            rest: ctl.rest,
            rect,
            band: out.band.clone(),
            section: ctl.o.get("section").and_then(Val::as_str).map(str::to_owned),
        };
        out.controls.push(c.clone());
        out.push(DrawOp::Control(c));
    }

    fn emit_rect(&self, o: &'p Object, r: Rect, mode: &Mode<'_>, out: &mut Out) {
        let (pad, bleed) = (self.num(o, "pad").unwrap_or(0.0), self.num(o, "bleed").unwrap_or(0.0));
        let left = (r.x - bleed).max(0.0);
        let right = (r.x + r.w + bleed).min(SCR_W);
        let ground = Rect::new(left, r.y, right - left, r.h);
        out.push(DrawOp::Ground { rect: ground, ink: self.live_ink(o, "ink"), radius: self.num(o, "radius").unwrap_or(0.0) });
        let inner = Rect::new(ground.x + pad, ground.y + pad, ground.w - 2.0 * pad, ground.h - 2.0 * pad);
        let cells = self.objects(o, "cells");
        self.place_row(&cells, self.gap(o, "gap").unwrap_or(0.0), inner, mode, out);
    }

    fn emit_vstack(&self, o: &'p Object, r: Rect, mode: &Mode<'_>, out: &mut Out) {
        if o.get("cells").and_then(Val::as_list).is_some_and(|c| {
            c.iter().filter_map(Val::as_object).any(|cell| {
                cell.get("text").and_then(Val::as_object).and_then(|t| t.get("bind")).and_then(Val::as_str).is_some_and(|b| b.starts_with("title."))
            })
        }) && out.title.is_none()
        {
            out.title = Some(Rect::new(r.x, r.y, 0.0, 0.0));
        }
        let ((items, gap), _) = self.vstack_items(o);
        let stack = VStack::new(&items, gap, Anchor::Top(r.y));
        let cells: Vec<&Object> = self.objects(o, "cells").into_iter().filter(|c| self.when(c)).filter(|c| content(c).is_some()).collect();
        for span in stack.placed() {
            let cell = cells[span.i];
            let Some((kind, c)) = content(cell) else { continue };
            let w = match self.size_of(kind, c).w {
                Dim::Px(w) => w,
                Dim::Fill => r.w,
            };
            let mark = out.gate.len();
            out.gate.extend(item_terms(cell));
            self.emit(kind, c, Rect::new(r.x, span.y, w, span.h), mode, out);
            out.gate.truncate(mark);
        }
    }

    fn emit_strip(&self, o: &'p Object, r: Rect, mode: &Mode<'_>, out: &mut Out) {
        let Some(card) = o.get("card").and_then(Val::as_object) else { return };
        let (card_w, card_h) = (self.num(o, "card_w").unwrap_or(0.0), self.num(o, "card_h").unwrap_or(0.0));
        let pitch = card_w + self.gap(o, "gap").unwrap_or(0.0);
        let visible = if pitch > 0.0 { (r.w / pitch).floor() as usize + 1 } else { 0 };
        let art = Rect::new(r.x, r.y, card_w, card_h);
        let mut tmp = Out::default();
        tmp.push(DrawOp::Image {
            rect: art,
            bind: card.get("art").and_then(Val::as_object).and_then(|a| a.get("bind")).and_then(Val::as_str).unwrap_or("").to_owned(),
            radius: self.num(o, "radius").unwrap_or(0.0),
        });
        let overlay = self.objects(card, "overlay");
        self.place_row(&overlay, 0.0, art, &Mode { replaces: &[], hide: false, standin_left: None, host: "" }, &mut tmp);
        let mut y = art.y + card_h + self.gap(card, "below_gap").unwrap_or(0.0);
        for (cell, text) in self.below_cells(card) {
            let h = self.text_h(text);
            let w = match self.dim(text, "w") {
                Some(Dim::Px(w)) => w,
                _ => card_w,
            };
            // the cell keeps its room on every card; a card draws it when its `item.*` terms hold
            tmp.gate = item_terms(cell);
            self.emit("text", text, Rect::new(art.x, y, w, h), mode, &mut tmp);
            tmp.gate.clear();
            y += h;
        }
        let template = tmp.ops.into_iter().zip(tmp.present).map(|(op, present)| CardOp { op, present }).collect();
        out.push(DrawOp::Strip { rect: r, card: art, pitch, visible, radius: self.num(o, "radius").unwrap_or(0.0), template });
    }

    fn emit_actions(&self, o: &'p Object, r: Rect, out: &mut Out) {
        let c = self.controls(o, self.gap(o, "gap").unwrap_or(0.0), false);
        let (bw, ih) = (self.action_w(o), self.num(o, "item_h").unwrap_or(0.0));
        let column = self.column(o);
        let live: Vec<&Ctl<'_>> = c.ctl.iter().flatten().collect();
        let spans: Vec<(f32, f32)> = if column {
            let heights = vec![Item::new(ih); live.len()];
            VStack::new(&heights, c.gap, Anchor::Top(r.y)).placed().map(|s| (s.y, s.h)).collect()
        } else {
            let widths = vec![Item::new(bw); live.len()];
            HStack::new(&widths, c.gap, Align::Leading(r.x)).placed().map(|s| (s.x, s.w)).collect()
        };
        for (ctl, (at, len)) in live.into_iter().zip(spans) {
            let rect = if column { Rect::new(r.x, at, bw, len) } else { Rect::new(at, r.y, len, ih) };
            self.push_control(ctl, rect, out);
        }
    }

    // ---- bands ----

    fn bands(&self) -> Vec<Band<'p>> {
        let Some(Val::List(list)) = self.package.layout.get("block") else { return Vec::new() };
        list.iter()
            .filter_map(Val::as_object)
            .filter(|b| self.when(b))
            .map(|b| Band {
                id: b.get("id").and_then(Val::as_str).unwrap_or(""),
                before: self.gap(b, "gap_before"),
                gap: self.gap(b, "gap").unwrap_or(0.0),
                z: b.get("z").and_then(Val::as_i64).unwrap_or(0),
                cells: self.objects(b, "cells"),
            })
            .collect()
    }

    /// One pass over the present bands, in `z` order, each in its box.
    fn fill_bands(&self, bands: &[Band<'p>], boxes: &[Rect], mode: &Mode<'_>) -> Out {
        let mut out = Out::default();
        let mut order: Vec<usize> = (0..bands.len()).collect();
        order.sort_by_key(|&i| bands[i].z);
        for i in order {
            out.band = bands[i].id.to_owned();
            let band_mode = Mode {
                replaces: mode.replaces,
                hide: mode.hide,
                standin_left: if bands[i].id == mode.host { mode.standin_left } else { None },
                host: mode.host,
            };
            self.place_row(&bands[i].cells, bands[i].gap, boxes[i], &band_mode, &mut out);
        }
        out
    }

    /// This state, and for a Skip state the credits layout beside it when the longer or shorter pill
    /// changes anything (see [`CompiledState::skip_variant`]).
    fn compile_state(&self) -> CompiledState {
        let mut state = self.compile_for(SkipKind::Intro);
        if self.state.slot == Slot::Skip {
            let credits = self.compile_for(SkipKind::Credits);
            if credits != state {
                state.skip_alt = Some(Box::new(credits));
            }
        }
        state
    }

    /// The state as it draws when the Skip pill shown is `skip` (it matters in a Skip state only).
    fn compile_for(&self, skip: SkipKind) -> CompiledState {
        let layout = &self.package.layout;
        let inset = self.gap(layout, "inset").unwrap_or(0.0);
        let x_inset = self.gap(layout, "x_inset").unwrap_or(0.0);
        let bands = self.bands();

        // the vertical chain: the bands in one bottom-anchored stack
        let heights: Vec<Item> = bands
            .iter()
            .map(|b| {
                let h = self.row_h(&b.cells);
                b.before.map_or(Item::new(h), |g| Item::new(h).before(g))
            })
            .collect();
        let stack = VStack::new(&heights, 0.0, Anchor::Bottom(SAFE.y + SAFE.h - inset));
        let boxes: Vec<Rect> = stack
            .placed()
            .map(|s| Rect::new(SAFE.x + x_inset, s.y, SAFE.w - 2.0 * x_inset, s.h))
            .collect();

        let up = layout.get("up_next").and_then(Val::as_object);
        let replaces: Vec<String> = up.map(|u| names(self.list(u, "replaces"))).unwrap_or_default();
        let host = up.and_then(|u| u.get("band")).and_then(Val::as_str).unwrap_or("");

        // the Discs pass places everything, and so says where the replaced segments stand
        let discs = self.fill_bands(&bands, &boxes, &Mode { replaces: &replaces, hide: false, standin_left: None, host });
        let host_box = bands.iter().zip(&boxes).find(|(b, _)| b.id == host).map(|(_, r)| *r);
        let standin = match (up, host_box) {
            (Some(u), Some(_)) => self.standin(u, &discs.replaced, &replaces),
            _ => Standin::default(),
        };

        let slot = self.state.slot;
        let mut out = if slot == Slot::Discs {
            discs
        } else {
            let left = match slot {
                Slot::UpNext => standin.up_next.map(|u| {
                    let scaled = |r: Rect| r.scaled(CTRL_FOCUS_SCALE).x;
                    [Some(scaled(u.credits)), Some(scaled(u.next)), u.still.map(|r| r.x), u.caption.map(|r| r.x)]
                        .into_iter()
                        .flatten()
                        .fold(f32::MAX, f32::min)
                }),
                // the pill actually shown: a start-aligned fill stops at ITS edge, as `classic::title_right` does
                _ => standin.skip.map(|s| s[skip as usize].scaled(CTRL_FOCUS_SCALE).x),
            };
            self.fill_bands(&bands, &boxes, &Mode { replaces: &replaces, hide: true, standin_left: left, host })
        };

        // the stand-in's ops, then the scrim in front of everything
        match slot {
            Slot::UpNext => {
                if let Some(rects) = standin.up_next {
                    let rest = |key: &str| up.and_then(|u| u.get(key)).and_then(Val::as_object).map_or(Rest::Film, Self::rest);
                    out.push(DrawOp::UpNext { rects, credits_rest: rest("watch_credits"), next_rest: rest("play_next") });
                }
            }
            Slot::Skip => {
                if let Some(rects) = standin.skip {
                    out.push(DrawOp::SkipPill { rects });
                }
            }
            Slot::Discs => {}
        }
        let block_top = boxes.iter().map(|b| b.y).reduce(f32::min).unwrap_or(SCR_H);
        let scrim = layout.get("scrim").and_then(Val::as_object).and_then(|s| {
            let top = match self.name(s, "cover")? {
                "fixed" => SCR_H - self.num(s, "height").unwrap_or(0.0),
                _ => (block_top - self.num(s, "extra").unwrap_or(0.0)).max(0.0),
            };
            Some(Rect::new(0.0, top, SCR_W, SCR_H - top))
        });
        let mut ops: Vec<DrawOp> = scrim.map(|rect| DrawOp::Scrim { rect }).into_iter().collect();
        ops.append(&mut out.ops);

        let row_ids = self.row_ids(host, &replaces);
        let cost = cost_of(&ops);
        // bound first: a `title:` initialiser is a localization boundary (`ci/check-localization.py`),
        // and the title here is a RECT, not text
        let title = out.title.unwrap_or_default();
        CompiledState {
            key: StateKey { open: self.state.open, slot, chapters: self.v.chapters, episode: self.v.episode, paused: self.paused },
            controls: out.controls,
            ops,
            scrim,
            bands: bands.iter().zip(&boxes).map(|(b, r)| (b.id.to_owned(), *r)).collect(),
            playbar: out.playbar,
            standin,
            title,
            tab_row: boxes.last().map_or_else(Rect::default, |b| Rect::new(b.x, b.y, 0.0, b.h)),
            row_ids,
            cost,
            band: HudBand::CLASSIC,
            skip_alt: None,
        }
    }

    /// The replaced segments' control ids in declared order (`None` where absent in this valuation).
    fn row_ids(&self, host: &str, replaces: &[String]) -> Vec<Option<String>> {
        let Some(Val::List(list)) = self.package.layout.get("block") else { return Vec::new() };
        let Some(band) = list.iter().filter_map(Val::as_object).find(|b| b.get("id").and_then(Val::as_str) == Some(host)) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for cell in self.objects(band, "cells") {
            let Some(seg) = cell.get("segment").and_then(Val::as_object) else { continue };
            if !seg.get("id").and_then(Val::as_str).is_some_and(|id| replaces.iter().any(|r| r == id)) {
                continue;
            }
            if !self.when(cell) {
                ids.extend(self.objects(seg, "items").iter().map(|_| None));
                continue;
            }
            for item in self.objects(seg, "items") {
                ids.push((!self.absent(item)).then(|| item.get("id").and_then(Val::as_str).unwrap_or("").to_owned()));
            }
        }
        ids
    }

    // ---- the stand-ins ----

    /// Where the offer lands: the union of the replaced segments, at the edge of THAT union `up_next.align`
    /// names (the end edge of the replaced segments, not the band's: another end-aligned segment may stand
    /// between them).
    fn standin(&self, up: &'p Object, replaced: &[Rect], replaces: &[String]) -> Standin {
        let Some(first) = replaced.first() else { return Standin::default() };
        let union = replaced.iter().fold(*first, |u, r| {
            let (x0, y0) = (u.x.min(r.x), u.y.min(r.y));
            let (x1, y1) = ((u.x + u.w).max(r.x + r.w), (u.y + u.h).max(r.y + r.h));
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        });
        let align = self.name(up, "align").unwrap_or("end");
        // the floor: one start- or end-aligned replaced segment, every item that can be absent collapsed
        let floor = self.floor(up, replaces);
        let w_next = stand_in_w(self.measure.width_str(&self.labels.next_episode, theme::size::BODY, true), floor.unwrap_or(0.0));
        let w_credits = Button::pill_w_measured(&cstring(&self.labels.watch_credits), theme::size::BODY, false, None, self.measure);
        // the pills are one run, right-anchored at `edge`; its width comes from the offer's own layout
        let probe = up_next::layout_at(RowAnchor { right: 0.0, top: union.y, h: union.h }, w_next, w_credits);
        let run = probe.next.x + probe.next.w - probe.credits.x;
        let edge = |run: f32| match align {
            "start" => union.x + run,
            "center" => union.x + union.w * 0.5 + run * 0.5,
            _ => union.x + union.w,
        };
        let anchor = RowAnchor { right: edge(run), top: union.y, h: union.h };
        let l = up_next::layout_at(anchor, w_next, w_credits);
        // bound first: a `caption:` initialiser is a localization boundary (`ci/check-localization.py`),
        // and this caption is a RECT, not text
        let still = (up.get("still").and_then(Val::as_bool) == Some(true)).then_some(l.still);
        let caption = (up.get("caption").and_then(Val::as_bool) == Some(true)).then_some(l.caption);
        let up_next = UpNextRects { still, caption, credits: l.credits, next: l.next };
        let skip = [&self.labels.skip_intro, &self.labels.skip_credits].map(|label| {
            let w = stand_in_w(self.measure.width_str(label, theme::size::BODY, true), floor.unwrap_or(0.0));
            let x = match align {
                "start" => union.x,
                "center" => union.x + union.w * 0.5 - w * 0.5,
                _ => union.x + union.w - w,
            };
            Rect::new(x, union.y, w, union.h)
        });
        let row = match (floor, align) {
            (Some(f), "start") => Rect::new(union.x, union.y, f, union.h),
            (Some(f), "center") => Rect::new(union.x + union.w * 0.5 - f * 0.5, union.y, f, union.h),
            (Some(f), _) => Rect::new(union.x + union.w - f, union.y, f, union.h),
            (None, _) => union,
        };
        Standin { row, anchor, up_next: Some(up_next), skip: Some(skip) }
    }

    /// The floor width of a stand-in: when `replaces` names ONE start- or end-aligned segment, that
    /// segment's width with every item that can be absent collapsed. A centred or multi-segment replace
    /// has none.
    fn floor(&self, up: &Object, replaces: &[String]) -> Option<f32> {
        let [only] = replaces else { return None };
        let band = self.name(up, "band")?;
        let Some(Val::List(list)) = self.package.layout.get("block") else { return None };
        let band = list.iter().filter_map(Val::as_object).find(|b| b.get("id").and_then(Val::as_str) == Some(band))?;
        let cell = self
            .objects(band, "cells")
            .into_iter()
            .find(|c| c.get("segment").and_then(Val::as_object).and_then(|s| s.get("id")).and_then(Val::as_str) == Some(only))?;
        if cell.get("align").and_then(Val::as_str) == Some("center") {
            return None;
        }
        let seg = cell.get("segment").and_then(Val::as_object)?;
        let c = self.controls(seg, self.gap(seg, "gap").unwrap_or(0.0), true);
        Some(HStack::new(&c.items, c.gap, Align::Leading(0.0)).width())
    }
}

fn names(list: &[Val]) -> Vec<String> {
    list.iter().filter_map(Val::as_str).map(str::to_owned).collect()
}

/// A label as the C string the widget functions take; an embedded NUL (never in a real label) is an
/// empty label.
fn cstring(label: &str) -> CString {
    CString::new(label).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------------------------------

fn area(r: Rect) -> u64 {
    (r.w * r.h).max(0.0).round() as u64
}

/// Is the ink translucent in any value it can take? (The cost is an upper bound.)
fn translucent(ink: &Live<Rgba>) -> bool {
    ink.each().any(|c| c[3] < 1.0)
}

/// A control's plate is blended unless its rest face is opaque or there is none.
fn plate(c: &ControlRect) -> u64 {
    if matches!(c.rest, Rest::Solid | Rest::Bare) {
        0
    } else {
        area(c.rect)
    }
}

fn plate_rest(rest: Rest, r: Rect) -> u64 {
    if matches!(rest, Rest::Solid | Rest::Bare) {
        0
    } else {
        area(r)
    }
}

/// One op's terms.
fn weigh(op: &DrawOp) -> Contributor {
    let mut c = Contributor { draw_ops: 1, ..Contributor::default() };
    match op {
        DrawOp::Scrim { rect } => c.blended_px = area(*rect),
        DrawOp::Ground { rect, ink, .. } => c.blended_px = if translucent(ink) { area(*rect) } else { 0 },
        DrawOp::Text { .. } => c.glyph_runs = 1,
        DrawOp::Image { rect, .. } => c.texture_bytes = 4 * area(*rect),
        DrawOp::Icon { rect, .. } => c.texture_bytes = 4 * area(*rect),
        DrawOp::Rule { rect, ink } => c.blended_px = if translucent(ink) { area(*rect) } else { 0 },
        DrawOp::Facts { order, badges, .. } => c.glyph_runs = order.len() + badges.len(),
        DrawOp::Playbar { track, track_ink, .. } => {
            c.glyph_runs = 2;
            c.blended_px = if translucent(track_ink) { area(*track) } else { 0 };
        }
        DrawOp::Strip { visible, template, .. } => {
            let cards = template.iter().map(|card| weigh(&card.op)).fold(Contributor::default(), |a, b| Contributor {
                op: 0,
                draw_ops: a.draw_ops + b.draw_ops,
                glyph_runs: a.glyph_runs + b.glyph_runs,
                texture_bytes: a.texture_bytes + b.texture_bytes,
                blended_px: a.blended_px + b.blended_px,
            });
            c.draw_ops = cards.draw_ops * visible + 1;
            c.glyph_runs = cards.glyph_runs * visible;
            c.texture_bytes = cards.texture_bytes * *visible as u64;
            c.blended_px = cards.blended_px * *visible as u64;
        }
        DrawOp::Control(ctl) => {
            c.blended_px = plate(ctl);
            match ctl.widget {
                Widget::Disc => c.texture_bytes = 4 * (ctl.rect.h * ctl.rect.h).round() as u64,
                Widget::Pill | Widget::Button => c.glyph_runs = 1,
            }
        }
        DrawOp::UpNext { rects, credits_rest, next_rest } => {
            c.draw_ops = 2 + usize::from(rects.still.is_some()) + usize::from(rects.caption.is_some());
            c.glyph_runs = 2 + usize::from(rects.caption.is_some());
            c.texture_bytes = rects.still.map_or(0, |r| 4 * area(r));
            c.blended_px = plate_rest(*credits_rest, rects.credits) + plate_rest(*next_rest, rects.next);
        }
        DrawOp::SkipPill { rects } => {
            c.glyph_runs = 1;
            c.blended_px = rects.iter().map(|r| area(*r)).max().unwrap_or(0);
        }
    }
    c
}

fn cost_of(ops: &[DrawOp]) -> Cost {
    let mut parts: Vec<Contributor> = ops.iter().enumerate().map(|(i, op)| Contributor { op: i, ..weigh(op) }).collect();
    let mut cost = Cost {
        draw_ops: parts.iter().map(|p| p.draw_ops).sum(),
        glyph_runs: parts.iter().map(|p| p.glyph_runs).sum(),
        texture_bytes: parts.iter().map(|p| p.texture_bytes).sum(),
        blended_px: parts.iter().map(|p| p.blended_px).sum(),
        top: Vec::new(),
    };
    parts.sort_by(|a, b| {
        b.blended_px
            .cmp(&a.blended_px)
            .then(b.texture_bytes.cmp(&a.texture_bytes))
            .then(b.glyph_runs.cmp(&a.glyph_runs))
            .then(a.op.cmp(&b.op))
    });
    cost.top = parts.into_iter().take(3).collect();
    cost
}
