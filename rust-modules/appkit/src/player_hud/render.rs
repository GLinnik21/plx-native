//! **The renderer of a compiled HUD** (`docs/hud-plugins.md`, "Architecture"): it draws the compact
//! state of a [`Compiled`] package through the shared widgets, computing nothing a frame could have
//! been told. `builtin:classic` is the only package that ships, and it draws here exactly as the
//! hand-written `classic::draw_hud` (since deleted) did: the census test in this module's tests holds the
//! renderer to the hashes that draw froze on the host, and the simulator's frame dump is the proof of pixels.
//! A package with no state for the frame draws nothing: [`draw`] returns `false`.
//!
//! **What it walks.** The state [`Compiled::lookup`] answers for the frame ([`draw`]: the frame's real open bits,
//! `Frame::open`, from the screen's `HudState::sections`, and for an inline package the sections still fading out
//! as well: [`HudMotion::presented`]), then its [`Span`]s in draw
//! order, through [`draw_state`], which walks any state, an open section's included. A `Fades` span draws on
//! the scrub's chrome alpha and is skipped when that alpha is spent; a `Steady` one draws at full alpha.
//! In a Skip state it walks [`CompiledState::skip_variant`], because the two labels have two widths. A MODAL
//! section's page owns the middle: `transport` is false AND the state is the section's, and it draws whole. A
//! compact state with the middle withheld (`!transport`, `open == 0`) draws only the scrim and the section tabs,
//! which only a test constructs now.
//! The section arms (a ground, a rule, an icon, the still, the sections' text, a facts row, an `actions`
//! button) are `sections.rs`'s.
//!
//! **Motion stays here.** The focus springs, the title marquee, the scrub preview and the knob's spring
//! are the renderer's, read from the [`HudMotion`] the page owns.
//!
//! **A section appears and fades as the panels did** (`docs/hud-plugins.md`, D6). Its appear spring is the
//! page's ([`HudMotion::section_appear`], stepped by the screen's tick, never here); the spans a section owns
//! ([`Span::section`]) are painted as ONE layer, faded by it and raised by `Popover::RISE * (1 - appear)`, and the
//! tabs, the scrim and the title/bar/row stay outside it. A MODAL package draws the logical state whole and lays a
//! section still fading out over it; an INLINE one draws the block of the presented sections, with each band above an
//! appearing or fading section `(1 - appear)` of its lift lower than its rest place (`lift`). A `Shelf` stands at a
//! screen y whatever its painter translates by, so the strip is told the layer's translate.
//!
//! **A section is drawn from its state.** The state's own bits say which sections are open, so a
//! `toggle_section` control reads selected (a pill: `TabPill`'s selected face; a disc: its `solid` rest face)
//! while its section is open, and an open section's `actions` buttons pop on `GROUP_SECTION` ([`HudMotion::section_scale`]).
//! A text group of a section (`CompiledState::groups`) is stacked HERE from the wrapped heights, as the Info card
//! stacks its own ([`sections::group`]).
//!
//! **The chapters strip** is a `cards::Shelf` held in the [`HudMotion`] ([`super::strip`]); the walk hands it the
//! `DrawFrame` the screen draws under and the Shelf paints, its springs stepped by the screen's tick. It draws
//! the strip's geometry and classic's inks: a package's declared card inks are not read (PR8 stage 3).
//!
//! **What it does not draw yet** (each is a decision, not an omission): `rest: custom` (it takes the film).
//! [`draw`] draws nothing for it, so a package that needs it shows less
//! rather than something wrong. A title's declared `ink` and `bold` are not carried: classic's rule (an
//! episode's kicker is primary and bold, a context line secondary and regular) draws, and only the
//! declared sizes are honoured.
//!
//! **What PR10b owes before a package can be installed** (only `builtin:classic` ships and the dev trigger is
//! the one way in, so no shipped HUD reaches any of these; each is a way a package draws or behaves wrongly
//! without a finding):
//! - a package with `title.main` and no `title.kicker` still draws the kicker, at classic's y
//!   (`draw_title`, from `TitleLook::default()`);
//! - a `button`, or a `pill` with no `section`, gets a focus stop and draws nothing (`draw_control`);
//! - the scrub preview is hard-wired to `SB_X` / `SB_Y` / `sb_w` (`classic::draw_scrub_preview`), so a package
//!   that places the bar elsewhere gets the preview card on classic's track;
//! - `ClockWarm` prewarms the clocks at CAPTION (and the scrub's grown size) only (`motion.rs`), so a package
//!   whose clocks use another rung is not prewarmed;
//! - `Compiled::glyph` re-hashes the asset's source for every disc on every frame.
//!
//! **The kicker is the one place it overrides the compile.** The compile folds `title.is_episode` into
//! the state ([`StateKey::episode`] is the ROW's pair boolean, because that is what classic's golden
//! equates it with), so a real episode with no queue neighbours compiles in the movie state. The kicker's
//! face is therefore chosen from `now_playing().is_real_episode`, as classic's draw always did, and only
//! the rects are taken from the ops.

use std::ffi::CString;

use plx_ui::theme;
use plx_ui::icons::Icon;
use plx_ui::widgets::{ControlGround, TabPill, TransportButton, TransportFocus, TransportRest};
use plx_ui::{Painter, Rect, View};

use super::classic::{
    draw_player_title_at, draw_playbar_look, draw_scrim_at, draw_scrub_preview, hud_env, hud_tab_state, tab_labels,
    tab_of_section, Kicker, Playbar, PlaybarLook, TitleLook,
};
use super::geometry::{frame_state, GROUP_ROW, GROUP_SECTION};
use super::sections;
use super::model::HudModel;
use super::motion::HudMotion;
use super::state::{readout_owns_frame, Busy, ControlSlot};
use plx_machine::machine::{GroupId, Host};
use plx_ui::geom::IndexElem;
use plx_ui::screen::DrawFrame;
use super::command::{HudCommand, SeekBy, Section, TrackTab};
use crate::hud_format::compile::{Chrome, Compiled, CompiledState, DrawOp, FaceRef, Focus, Rest, Span, Widget, OPEN_CHAPTERS, OPEN_INFO};
use crate::skip_pill;
use crate::up_next::{self, Countdown, Decor, Layout};

/// Everything besides the compiled package, the motion and the painter that one frame of the
/// transport draws from, bundled.
pub struct Frame<'a> {
    /// The entry the screen draws under: the key the chapters strip's Shelf reads focus from.
    pub entry: plx_machine::machine::EntryId,
    pub ps: &'a plx_media::route::PlaybackSession,
    pub model: &'a HudModel,
    pub up: &'a Countdown,
    pub slot: ControlSlot,
    pub busy: Busy,
    pub focus: GroupId,
    pub btn: i32,
    pub tab: i32,
    /// The sections that are open (`StateKey::open`, `0` when none is): which state the frame walks. The screen
    /// hands the discs' `slot` while any is open (D7), so a stand-in waits behind it.
    pub open: u8,
    pub now: u32,
    /// false while a section has taken the middle (the transport's bar and row are not on screen)
    pub transport: bool,
    pub measure: &'a dyn plx_machine::machine::Measure,
    pub meta: plx_data::metadata::MetadataView<'a>,
}

/// The compiled state this frame draws, or `None` when the package has no state for it
/// ([`frame_state`], the lookup every geometry consumer takes too).
fn state_for<'c>(compiled: &'c Compiled, open: u8, f: &Frame<'_>) -> Option<&'c CompiledState> {
    let chapters = crate::chapters_panel::has_chapters(f.meta);
    frame_state(compiled, open, f.slot, chapters, f.model.paused).map(|(state, skip)| state.skip_variant(skip))
}

/// **How the sections' motion shapes one walk** (`docs/hud-plugins.md`, "Sections"): each section's appear, which
/// paints the spans it owns as ONE layer (faded by it, risen by `Popover::RISE` over `1 - appear`: what the panels
/// drew), and, for an inline package, how far each band above an appearing or fading section stands below its
/// place in the presented state.
struct Layers {
    /// Chapters, then Info
    appear: [f32; 2],
    /// `(band id, dy)` of the bands that stand lower than their rest place, in logical pixels
    lift: Vec<(String, f32)>,
}

impl Layers {
    /// Every section at rest: what a state walked on its own (a test's) draws.
    fn settled() -> Self {
        Self { appear: [1.0; 2], lift: Vec::new() }
    }

    /// The layers of `state`, the state of the `base` bits, from the motion `row` holds.
    fn of(compiled: &Compiled, state: &CompiledState, base: u8, f: &Frame<'_>, inline: bool, row: &HudMotion) -> Self {
        let appear = [row.section_appear(Section::Chapters), row.section_appear(Section::Info)];
        let lift = if inline { lift(compiled, state, base, f, &appear) } else { Vec::new() };
        Self { appear, lift }
    }

    /// The appear of the first section named in `bits`, `1` for a span of the transport.
    fn appear_of(&self, bits: u8) -> f32 {
        if bits & OPEN_CHAPTERS != 0 {
            self.appear[0]
        } else if bits & OPEN_INFO != 0 {
            self.appear[1]
        } else {
            1.0
        }
    }

    /// `p` for a span that belongs to the sections `bits`: faded and raised as the panels were.
    fn paint(&self, bits: u8, p: Painter) -> Painter {
        let a = self.appear_of(bits);
        if a >= 1.0 {
            return p;
        }
        p.alpha(a).translate(0.0, plx_ui::popover::Popover::RISE * (1.0 - a))
    }

    /// How far below its rest place the band `id` stands.
    fn lift_of(&self, id: &str) -> f32 {
        self.lift.iter().find(|(band, _)| band == id).map_or(0.0, |(_, dy)| *dy)
    }
}

/// **Where the bands above an appearing or fading section stand.** An inline package lays the sections' bands
/// under the transport's, so opening one lifts the bands above it. Each of those stands `(1 - appear)` of that
/// lift LOWER than its rest place in `state`, so at appear 0 the block is where the state without the section
/// puts it and the mask drops with no jump. The lift is read from the compiler's geometry: the band's box in
/// `state` against the same band's box in the state without the section, so it is whatever the package laid out
/// (a keyed `gap_before` included) and nothing is tuned per package. No state is kept per op.
fn lift(compiled: &Compiled, state: &CompiledState, base: u8, f: &Frame<'_>, appear: &[f32; 2]) -> Vec<(String, f32)> {
    let mut out: Vec<(String, f32)> = Vec::new();
    for section in [Section::Chapters, Section::Info] {
        let bit = section.open_bit();
        let a = appear[if bit == OPEN_CHAPTERS { 0 } else { 1 }];
        if base & bit == 0 || a >= 1.0 {
            continue;
        }
        let Some(without) = state_for(compiled, base & !bit, f) else { continue };
        let owned = |id: &str| state.spans.iter().any(|span| span.section & bit != 0 && span.band == id);
        let Some(first) = state.bands.iter().position(|(id, _)| owned(id)) else { continue };
        for (id, rect) in &state.bands[..first] {
            let Some((_, rest)) = without.bands.iter().find(|(other, _)| other == id) else { continue };
            let dy = (1.0 - a) * (rest.y - rect.y);
            match out.iter_mut().find(|(band, _)| band == id) {
                Some(entry) => entry.1 += dy,
                None => out.push((id.clone(), dy)),
            }
        }
    }
    out
}

/// Draw the transport from `compiled` into `p`. Returns false, having drawn nothing, when the package
/// has no state for this frame (the caller then draws what it drew before the renderer existed).
pub fn draw<H: Host>(compiled: &Compiled, f: &Frame<'_>, df: &DrawFrame<'_, '_, H>, row: &mut HudMotion, p: Painter) -> bool
where
    H::Elem: IndexElem,
{
    // A FAILURE owns the frame: the read-out is `draw_readout`'s, drawn by the caller AFTER this.
    if readout_owns_frame(f.busy) {
        row.reset_title();
        return true;
    }
    // A MODAL package draws the logical state whole (the transport comes back the moment a section is put away, as
    // it did under the panels) and lays a section still fading out over it; an INLINE one draws the block as the
    // presented sections lay it out.
    let presented = row.presented();
    let inline = !compiled.sections_modal();
    let base = if inline { f.open | presented } else { f.open };
    let Some(state) = state_for(compiled, base, f) else { return false };
    let layers = Layers::of(compiled, state, base, f, inline, row);
    walk(compiled, state, f, df, row, p, &layers, 0);
    if !inline {
        for section in [Section::Chapters, Section::Info] {
            let bit = section.open_bit();
            if presented & !base & bit == 0 {
                continue;
            }
            if let Some(leaving) = state_for(compiled, bit, f) {
                walk(compiled, leaving, f, df, row, p, &Layers::of(compiled, leaving, bit, f, false, row), bit);
            }
        }
    }
    true
}

/// Walk one compiled state's ops into `p`. [`draw`] hands it the state the frame's open bits select (the compact
/// one with no section open, an open-section state otherwise); a test hands it any state it builds
/// (`compiled.lookup(OPEN_INFO | OPEN_CHAPTERS, ..)`). The failure read-out is [`draw`]'s: it owns the frame
/// before any state is chosen.
pub fn draw_state<H: Host>(compiled: &Compiled, state: &CompiledState, f: &Frame<'_>, df: &DrawFrame<'_, '_, H>, row: &mut HudMotion, p: Painter)
where
    H::Elem: IndexElem,
{
    walk(compiled, state, f, df, row, p, &Layers::settled(), 0);
}

/// [`draw_state`] under `layers`: the spans of the section bits `only` alone when it is not `0` (a section still
/// fading out over a state that no longer holds it), else every span.
#[allow(clippy::too_many_arguments)]
fn walk<H: Host>(
    compiled: &Compiled,
    state: &CompiledState,
    f: &Frame<'_>,
    df: &DrawFrame<'_, '_, H>,
    row: &mut HudMotion,
    p: Painter,
    layers: &Layers,
    only: u8,
) where
    H::Elem: IndexElem,
{
    let e = hud_env();
    // a panel owning the middle hides the compact state's transport; a section state IS the panel
    let hides_transport = state.key.open == 0 && !f.transport;

    // While a scrub is live the `fades` bands (the title block, the control row, the tabs) fade away so
    // the card has the room; the bar, the knob and both clocks stay. The same alpha restores them.
    let chrome_a = row.preview.motion.chrome_alpha(f.now);
    let chrome = chrome_a > 0.004;
    let pc = p.alpha(chrome_a);
    let walked = p;
    if !f.transport || !chrome {
        row.reset_title();
    }

    // the kicker's rect, remembered until the title's op arrives: the pair draws as one (one clip, one
    // marquee key, the paused reveal restarting when either line changes)
    let mut kicker_rect: Option<(Rect, i32)> = None;
    let mut bar: Option<Playbar> = None;
    let mut run = |span: &Span| {
        if only != 0 && span.section & only == 0 {
            return;
        }
        if span.chrome == Chrome::Fades && !chrome {
            return;
        }
        let q = if span.chrome == Chrome::Fades { pc } else { p };
        // an appearing or fading section lifts the bands above it; its own spans are one layer
        let dy = layers.lift_of(&span.band);
        let (q, p) = if dy != 0.0 { (q.translate(0.0, dy), p.translate(0.0, dy)) } else { (q, p) };
        let q = layers.paint(span.section, q);
        // a Shelf stands at a SCREEN y whatever translate its painter carries, so the strip is told the layer's
        let moved = q.dy() - walked.dy();
        // the end of the group being drawn: its children are drawn together, from their real heights
        let mut grouped_to = 0;
        for (at, op) in state.ops[span.ops.clone()].iter().enumerate().map(|(k, op)| (span.ops.start + k, op)) {
            if at < grouped_to {
                continue;
            }
            if let Some(g) = state.groups.iter().find(|g| g.ops.start == at) {
                sections::group(compiled, f, q, state, g);
                grouped_to = g.ops.end;
                continue;
            }
            match op {
                DrawOp::Scrim { rect } => draw_scrim_at(q, *rect),
                _ if hides_transport && !is_section_tab(op) => {}
                DrawOp::Text { bind, rect, size, bold, ink, alpha, max_lines, leading } => match text_of(bind) {
                    Some(Text::Kicker) => kicker_rect = Some((*rect, *size)),
                    Some(Text::Title) => draw_title(f, row, q, kicker_rect.take(), *rect, *size),
                    None => sections::text(f, q, *rect, bind, *size, bold, ink, *alpha, *max_lines, *leading),
                },
                DrawOp::Control(c) => draw_control(compiled, f, row, &e, q, state, c),
                DrawOp::UpNext { rects, .. } => {
                    if let ControlSlot::UpNext(_) = f.slot {
                        let layout = Layout {
                            still: rects.still.unwrap_or_default(),
                            caption: rects.caption.unwrap_or_default(),
                            credits: rects.credits,
                            next: rects.next,
                        };
                        let decor = Decor { still: rects.still.is_some(), caption: rects.caption.is_some() };
                        up_next::draw_over(f.ps, &layout, decor, row, f.up, q, f.focus == GROUP_ROW, f.btn, f.now, f.measure);
                    }
                }
                DrawOp::SkipPill { rects } => {
                    if let ControlSlot::Skip(pr) = f.slot {
                        let shown = rects[usize::from(pr.kind == plx_data::metadata::MarkerKind::Credits)];
                        skip_pill::draw_at(row, q, pr, shown, f.focus == GROUP_ROW);
                    }
                }
                DrawOp::Playbar { track, fill, track_ink, clock_size, knob, clocks, mark, .. } => {
                    let mut live = Playbar::live(f.model, f.busy, f.focus, row.since_play_ms(f.now), f.now);
                    live.clock_grow = row.preview.motion.clock_grow(f.now);
                    // a cancelled scrub's knob returns to the playhead rather than jumping to it
                    live.pos_ns = row.spring.apply(live.pos_ns, f.now);
                    let holds = |name: &str| name == "transport.paused" && f.model.paused;
                    let look = PlaybarLook {
                        track: *track,
                        fill: fill.at(holds),
                        track_ink: track_ink.at(holds),
                        clock_size: *clock_size,
                        knob: *knob,
                        clocks: *clocks,
                        mark: *mark,
                    };
                    draw_playbar_look(q, live, look, f.measure);
                    bar = Some(live);
                }
                DrawOp::Ground { rect, ink, radius } => sections::ground(q, *rect, *radius, ink, f),
                DrawOp::Rule { rect, ink } => sections::rule(q, *rect, ink, f),
                DrawOp::Icon { rect, asset, ink } => sections::icon(compiled, q, *rect, asset, ink, f),
                DrawOp::Image { rect, bind, radius } => sections::image(f, q, *rect, bind, *radius),
                DrawOp::Facts { rect, order, badges, genres_max, playback_fact, bold, ink, .. } => {
                    sections::facts(f, q, *rect, order, badges, *genres_max, *playback_fact, bold, ink)
                }
                // not drawn by this renderer yet: see the module doc
                DrawOp::Strip { .. } => sections::strip(f, df, row, q, op, moved),
            }
        }
        // right after the bar's band, so the still floats over the title block and the row above it
        // and under the tabs below, as classic orders them
        if let Some(live) = bar.take() {
            draw_scrub_preview(p, f.ps, f.model, row, live);
        }
    };
    // The stand-in's ops are the LAST span, carrying the id of the band that hosts them; they draw right
    // after that band's own ops, where classic puts the row, and so before the bar.
    for (i, span) in state.spans.iter().enumerate() {
        let standin = !span.band.is_empty() && state.spans[..i].iter().any(|s| s.band == span.band);
        if standin {
            continue;
        }
        run(span);
        if !span.band.is_empty() {
            if let Some(next) = state.spans[i + 1..].iter().find(|s| s.band == span.band) {
                run(next);
            }
        }
    }
}

/// A section tab: the one control that draws while a panel owns the middle.
fn is_section_tab(op: &DrawOp) -> bool {
    matches!(op, DrawOp::Control(c) if c.command == "toggle_section")
}

/// What a `Text` op's bind names. The ONE resolver of the title's binds: a bind with none (`info.*` and
/// `label.*`: the sections', drawn by `sections::text`; `item.*`: the strip card's) is not the title pair's.
enum Text {
    /// the kicker line: an episode's numbering and title, else the item's context line
    Kicker,
    /// the playing item's title
    Title,
}

fn text_of(bind: &str) -> Option<Text> {
    match bind {
        "title.kicker" => Some(Text::Kicker),
        "title.main" => Some(Text::Title),
        _ => None,
    }
}

/// The title pair, in the cell the compile placed it: the kicker's rect and size, and the main line's. The
/// main line's right edge is the bound both lines fit before (the `w: fill` cell's, golden-pinned for classic in
/// `classic-title-right.golden.tsv`).
fn draw_title(f: &Frame<'_>, row: &mut HudMotion, q: Painter, kicker: Option<(Rect, i32)>, main: Rect, title_size: i32) {
    let classic = TitleLook::default();
    let look = TitleLook {
        x: main.x,
        kicker_y: kicker.map_or(classic.kicker_y, |(k, _)| k.y),
        title_y: main.y,
        kicker_size: kicker.map_or(classic.kicker_size, |(_, size)| size),
        title_size,
    };
    let right = main.x + main.w;
    let paused = f.model.paused;
    if let Some(n) = f.meta.now_playing().filter(|n| n.is_real_episode) {
        let kicker = CString::new(plx_ui::fmt::episode_kicker(n.season, n.index, &n.ep_title)).unwrap_or_default();
        // an interior-NUL title drew nothing before the title was factored out: it still does
        if let Ok(title) = CString::new(n.title.clone()) {
            draw_player_title_at(q, look, Kicker::Episode(kicker.as_ptr()), title.as_ptr(), right, f.measure, paused, &row.title_clock);
        }
    } else {
        draw_player_title_at(
            q,
            look,
            Kicker::Context(plx_media::route::ctxline_cptr(f.ps)),
            plx_media::route::title_cptr(f.ps),
            right,
            f.measure,
            paused,
            &row.title_clock,
        );
    }
}

/// The glyph a disc draws: one of the app's, or a package asset's SVG.
enum Glyph<'a> {
    Icon(Icon),
    Svg(plx_ui::icons::Svg<'a>),
}

/// **The state key a command's `face` map is read at** (`docs/hud-plugins.md`, "Faces"): the glyph set
/// each command takes. A command with no state takes `default`.
fn face_state(act: HudCommand, m: &HudModel) -> &'static str {
    match act {
        HudCommand::TogglePlay if m.paused => "paused",
        HudCommand::TogglePlay => "playing",
        HudCommand::Previous if m.controls.restart => "restart",
        HudCommand::ToggleStats if m.stats => "on",
        HudCommand::ToggleStats => "off",
        _ => "default",
    }
}

/// The glyph of a command whose package wrote no `face`: the app's own. For classic's five commands it is
/// `Controls::icon`'s, so the replaced row and the old one cannot disagree.
fn classic_glyph(act: Option<HudCommand>, m: &HudModel) -> Icon {
    match act {
        Some(HudCommand::Previous) if m.controls.restart => Icon::Restart,
        Some(HudCommand::Previous) => Icon::Previous,
        Some(HudCommand::Next) => Icon::Next,
        Some(HudCommand::OpenTracks(TrackTab::Audio)) => Icon::Audio,
        Some(HudCommand::OpenMore(_)) => Icon::More,
        Some(HudCommand::TogglePlay) if m.paused => Icon::Play,
        Some(HudCommand::TogglePlay) => Icon::Pause,
        Some(HudCommand::Seek(by)) => match by {
            SeekBy::Secs(n) if n.get() < 0 => Icon::Rewind,
            SeekBy::Secs(_) => Icon::FastForward,
            SeekBy::Setting(super::command::SeekDir::Back) => Icon::Rewind,
            SeekBy::Setting(super::command::SeekDir::Forward) => Icon::FastForward,
        },
        Some(HudCommand::FromBeginning) => Icon::Restart,
        Some(HudCommand::GoToDetail | HudCommand::ToggleStats | HudCommand::ToggleSection(_)) => Icon::Info,
        Some(HudCommand::OpenTracks(TrackTab::Subtitles)) | None => Icon::Cc,
    }
}

/// The glyph the control `c` draws now: its `face` picked at the command's state, else the command's own.
fn glyph_of<'a>(compiled: &'a Compiled, c: &crate::hud_format::compile::ControlRect, m: &HudModel) -> Glyph<'a> {
    let picked = match c.act {
        Some(HudCommand::Seek(SeekBy::Setting(_))) => c.face.pick_interval(m.skip_secs),
        Some(act) => c.face.pick(face_state(act, m)),
        None => c.face.pick("default"),
    };
    match picked {
        Some(FaceRef::Builtin(icon)) => Glyph::Icon(*icon),
        Some(FaceRef::Asset(id)) => compiled.glyph(id).map_or(Glyph::Icon(classic_glyph(c.act, m)), Glyph::Svg),
        None => Glyph::Icon(classic_glyph(c.act, m)),
    }
}

/// Whether `c` is a `toggle_section` control of a section this state has open (`StateKey::open`): the state the
/// walk is in says which sections are open, whatever the policy (a modal section hides the row, an inline one
/// leaves it live), so a section's tab reads selected and its disc wears its `solid` face from the state alone
/// (`docs/hud-plugins.md`, `toggle_section`).
fn section_open(state: &CompiledState, c: &crate::hud_format::compile::ControlRect) -> bool {
    matches!(c.act, Some(HudCommand::ToggleSection(s)) if state.key.open & s.open_bit() != 0)
}

/// One control, at the rect the compile placed it.
fn draw_control(
    compiled: &Compiled,
    f: &Frame<'_>,
    row: &HudMotion,
    e: &plx_ui::Env,
    q: Painter,
    state: &CompiledState,
    c: &crate::hud_format::compile::ControlRect,
) {
    match c.widget {
        // A disc of the replaced row: its stable declared index (`CompiledState::row_ids`) is the
        // control's identity for the springs and the cursor. Its command, and so its enabled bit, is the
        // package's (`ControlRect::act`, with the slot's sampled `Controls` in the model so the discs and
        // the model cannot disagree); its glyph is its `face` at the command's state, else the command's
        // own; its rest and focus faces are the widget's.
        Widget::Disc => {
            let ControlSlot::Discs(controls) = f.slot else { return };
            let Some(i) = state.row_ids.iter().position(|id| id.as_deref() == Some(c.id.as_str())) else { return };
            let Ok(i) = i32::try_from(i) else { return };
            let model = HudModel { controls, ..*f.model };
            let button = TransportButton::new(i, c.rect)
                .enabled(c.act.is_some_and(|act| act.enabled(&model)))
                .focused(f.focus == GROUP_ROW && f.btn == i)
                .ground(ControlGround::Unkeyed)
                .scale(row.scale(i))
                .rest_face(match c.rest {
                    // an open section's own control wears the `solid` face, whatever it rests in
                    _ if section_open(state, c) => TransportRest::Solid,
                    // `custom` (allowed inks) is not drawn yet: it takes the film
                    Rest::Film | Rest::Custom => TransportRest::Film,
                    Rest::Bare => TransportRest::Bare,
                    Rest::Solid => TransportRest::Solid,
                })
                .focus_face(match c.focus {
                    Focus::Accent => TransportFocus::Accent,
                    Focus::Ring => TransportFocus::Ring,
                });
            match glyph_of(compiled, c, &model) {
                Glyph::Icon(icon) => button.icon(icon).draw(e, q),
                Glyph::Svg(svg) => button.svg(svg).draw(e, q),
            }
        }
        // A section tab. Its label and its index are the tab strip's (`tab_labels`), so what is
        // drawn, hit and selected are the one list.
        Widget::Pill => {
            let Some(section) = c.section.as_deref() else { return };
            let Some(i) = tab_of_section(section) else { return };
            let labels = tab_labels(crate::chapters_panel::has_chapters(f.meta));
            let Some(Ok(label)) = usize::try_from(i).ok().and_then(|i| labels[i]).map(CString::new) else { return };
            let (focused, selected) = hud_tab_state(f.transport, f.focus, f.tab, i);
            // the state the walk is in says which sections are open: their tabs read selected, whatever the
            // policy (a modal section hides the row, an inline one leaves it live)
            let selected = selected || section_open(state, c);
            TabPill::new(label.as_ptr(), theme::size::BODY, c.rect)
                .focused(focused)
                .selected(selected)
                .ground(ControlGround::Unkeyed)
                .draw(e, q);
        }
        // An `actions` button of a section: the focus and the pop are the section group's.
        Widget::Button => {
            let Some(i) = state
                .controls
                .iter()
                .filter(|k| k.widget == Widget::Button)
                .position(|k| k.id == c.id)
            else {
                return;
            };
            let focused = f.focus == GROUP_SECTION && usize::try_from(f.btn).is_ok_and(|b| b == i);
            sections::button(f, q, e, c, focused, row.section_scale(i));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_hud::geometry::{GROUP_SCRUB, GROUP_TABS};
    use crate::player_hud::state::Controls;
    use crate::skip_pill::prompt_for;
    use plx_data::metadata::{Marker, MarkerKind};
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
    use plx_ui::draw_census::{capture, capture_icons, capture_inks, capture_runs, Run};
    use plx_ui::fixture::FixtureMeasure;
    use plx_ui::icons::Icon;

    use plx_machine::machine::{EntryId, FocusRead, Host, InputOwner, PressRead, Tick};

    /// A host with `u32` elements: the least a `DrawFrame` needs for the renderer to paint the strip's Shelf.
    pub(super) struct HostFixture;
    impl Host for HostFixture {
        type Arg = plx_ui::fixture::FixtureArg;
        type Fx = plx_ui::fixture::FixtureFx;
        type Msg = plx_ui::fixture::FixtureMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = plx_ui::fixture::FixtureInit;
        type Memory = ();
    }

    /// Run `f` over a frame whose focus reads `focus`.
    pub(super) fn with_df_focus<R>(focus: FocusRead<u32>, f: impl FnOnce(&DrawFrame<'_, '_, HostFixture>) -> R) -> R {
        let cx = plx_machine::machine::Cx {
            views: (),
            tick: Tick::default(),
            measure: &FM,
            focus,
            press: PressRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        };
        f(&DrawFrame::new(&cx, Painter::root()))
    }

    pub(super) fn with_df<R>(f: impl FnOnce(&DrawFrame<'_, '_, HostFixture>) -> R) -> R {
        with_df_focus(FocusRead::default(), f)
    }

    const FM: FixtureMeasure = FixtureMeasure;
    const PAIR: Controls = Controls { previous: true, next: true, restart: false };
    const RESTART: Controls = Controls { previous: true, next: false, restart: true };

    /// Every primitive a frame hands the painter: the command census, the text runs, the glyphs and the
    /// fills with their effective alpha, in draw order.
    type Census = (Vec<(u64, Rect)>, Vec<Run>, Vec<(Icon, Rect)>, Vec<(u64, Rect, f32)>);

    fn census(draw: impl FnOnce()) -> Census {
        let (mut runs, mut icons, mut inks) = (Vec::new(), Vec::new(), Vec::new());
        let prims = plx_ui::record_walk(|| {
            capture(|| {
                inks = capture_inks(|| {
                    icons = capture_icons(|| {
                        runs = capture_runs(draw);
                    });
                });
            })
        });
        (prims, runs, icons, inks)
    }

    fn marker(kind: MarkerKind, final_seg: bool) -> Marker {
        Marker { kind, start_ms: 1_000, end_ms: 2_000, final_seg }
    }

    /// One frame's inputs.
    #[derive(Clone, Copy)]
    struct Case {
        name: &'static str,
        slot: ControlSlot,
        focus: GroupId,
        btn: i32,
        tab: i32,
        paused: bool,
        transport: bool,
    }

    impl std::fmt::Debug for Case {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "[{} focus={:?} btn={} tab={} paused={} transport={}]", self.name, self.focus, self.btn, self.tab, self.paused, self.transport)
        }
    }

    /// The renderer over one frame, with a fresh motion state.
    fn drawn(compiled: &Compiled, c: Case) -> (Census, bool) {
        let ps = plx_media::route::PlaybackSession::IDLE;
        let store = plx_data::stores::metadata::MetadataStore::default();
        let model = HudModel { paused: c.paused, duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
        let up = Countdown::default();
        let mut row = HudMotion::new();
        let frame = Frame {
            entry: EntryId(0), ps: &ps,
            model: &model,
            up: &up,
            slot: c.slot,
            busy: Busy::None,
            focus: c.focus,
            btn: c.btn,
            tab: c.tab,
            open: 0,
            now: 1_000,
            transport: c.transport,
            measure: &FM,
            meta: store.view(),
        };
        let mut drew = false;
        let new = census(|| drew = with_df(|df| draw(compiled, &frame, df, &mut row, Painter::root())));
        (new, drew)
    }

    fn cases() -> Vec<Case> {
        let slots = [
            ("movie", ControlSlot::Discs(Controls::NONE)),
            ("pair", ControlSlot::Discs(PAIR)),
            ("restart", ControlSlot::Discs(RESTART)),
            ("skip-intro", ControlSlot::Skip(prompt_for(marker(MarkerKind::Intro, false)))),
            ("skip-credits", ControlSlot::Skip(prompt_for(marker(MarkerKind::Credits, false)))),
            ("up-next", ControlSlot::UpNext(marker(MarkerKind::Credits, true))),
        ];
        let mut out = Vec::new();
        for (name, slot) in slots {
            for paused in [false, true] {
                for transport in [true, false] {
                    // focus on the bar, on every stable stop of the row, and on both tabs
                    let mut focuses = vec![(GROUP_SCRUB, 0, 0)];
                    focuses.extend((0..slot.items()).map(|i| (GROUP_ROW, i, 0)));
                    focuses.extend([(GROUP_TABS, 0, 0), (GROUP_TABS, 0, 1)]);
                    for (focus, btn, tab) in focuses {
                        out.push(Case { name, slot, focus, btn, tab, paused, transport });
                    }
                }
            }
        }
        out
    }

    /// **What the hand-written `classic::draw_hud` drew, frozen.** `census-classic.golden.tsv` has one line
    /// per (language, case): `lang TAB case index TAB case TAB hash`, the FNV-1a hash of the frame's whole
    /// census (the primitives, text runs, glyphs and fills with effective alpha, in order, as `{:?}`). Written
    /// from `classic::draw_hud` at 93fc5091d (the layout PR5c's golden matched at 70a7aa66a), before that
    /// draw was deleted; the renderer must reproduce each hash. The `nl` lines were written from the same
    /// hand-written draw at edd3d0da6 with the Dutch catalog applied, after the language arrived.
    /// `FixtureMeasure` only, so the values do not depend on a font.
    const FROZEN_CENSUS: &str = include_str!("census-classic.golden.tsv");

    /// FNV-1a, 64 bit: a hash whose algorithm is fixed here (`DefaultHasher`'s is not).
    fn fnv1a(text: &str) -> u64 {
        text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    }

    fn census_hash(c: &Census) -> u64 {
        fnv1a(&format!("{c:?}"))
    }

    fn frozen_hash(language: &str, index: usize) -> u64 {
        let line = FROZEN_CENSUS
            .lines()
            .find(|l| {
                let mut f = l.split('\t');
                f.next() == Some(language) && f.next().and_then(|i| i.parse::<usize>().ok()) == Some(index)
            })
            .unwrap_or_else(|| panic!("no frozen census for {language} #{index}"));
        u64::from_str_radix(line.split('\t').nth(3).expect("a hash"), 16).expect("a hex hash")
    }

    /// **Classic through the renderer is the frozen census of the hand-written draw**: every primitive,
    /// glyph, text run and fill the renderer hands the painter hashes to the line `census-classic.golden.tsv`
    /// froze from the hand-written `classic::draw_hud` (93fc5091d), for every slot, focus, pause and panel
    /// state, in every shipped language. The state the host cannot reach is the title text and Up Next's
    /// card (they need a playing item): the simulator's frame dump covers them.
    #[test]
    fn classic_through_the_renderer_draws_what_the_hand_written_hud_draws() {
        let mut compared = 0;
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let cx = plx_platform::i18n::current();
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", cx, &FM);
            for (index, c) in cases().into_iter().enumerate() {
                let (new, drew) = drawn(&hud.compiled, c);
                assert!(drew, "{c:?}: the renderer has a state for every classic frame");
                assert!(!new.0.is_empty(), "{c:?}: the renderer draws something");
                assert_eq!(census_hash(&new), frozen_hash(language.tag(), index), "{} {c:?}: the renderer's frame is not the frozen census:\n{new:#?}", language.tag());
                compared += 1;
            }
        }
        assert_eq!(compared, SHIPPED.len() * cases().len(), "a count, so a loop that compares less cannot pass");
        assert_eq!(FROZEN_CENSUS.lines().count(), compared, "the frozen file holds one line per case");
    }

    /// A failure owns the frame: nothing of the transport draws, and the title's marquee resets.
    #[test]
    fn a_failure_read_out_owns_the_frame_in_the_renderer_too() {
        let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
        let ps = plx_media::route::PlaybackSession::IDLE;
        let store = plx_data::stores::metadata::MetadataStore::default();
        let model = HudModel::default();
        let up = Countdown::default();
        let mut row = HudMotion::new();
        let frame = Frame {
            entry: EntryId(0), ps: &ps,
            model: &model,
            up: &up,
            slot: ControlSlot::Discs(Controls::NONE),
            busy: Busy::Readout(plx_ui::widgets::StatusKind::Failed, c"x"),
            focus: GROUP_SCRUB,
            btn: 0,
            tab: 0,
            open: 0,
            now: 0,
            transport: true,
            measure: &FM,
            meta: store.view(),
        };
        let drawn = census(|| assert!(with_df(|df| draw(&hud.compiled, &frame, df, &mut row, Painter::root()))));
        assert!(drawn.0.is_empty(), "{:?}", drawn.0);
    }

    /// **The Chapters strip through the renderer** (`draw_state` over classic's `OPEN_CHAPTERS` state, the
    /// strip a `cards::Shelf`) against the hand-written strip (`ChaptersState::draw`) on the same playing item,
    /// both at rest on the same chapter. The screen does not switch to it yet, so this is the one place the two
    /// meet. Every difference between them is D11's (`docs/hud-plugins.md`) or listed in the PR's handoff.
    mod chapters_section {
        use super::*;
        use crate::chapters_panel::ChaptersState;
        use crate::hud_format::compile::OPEN_CHAPTERS;
        use crate::hud_format::Slot;
        use crate::player_hud::strip::{Cards, Geometry};
        use plx_data::metadata::Chapter;
        use plx_machine::machine::{By, Effects, FocusKey, InstanceId, MachineId, ScreenEvent};
        use plx_machine::present::Present;
        use plx_ui::draw_census::capture_rings;

        const ENTRY: EntryId = EntryId(0);

        fn chapters(names: &[&str]) -> Vec<Chapter> {
            names
                .iter()
                .enumerate()
                .map(|(i, t)| Chapter { index: i as i64 + 1, start_ms: i as i64 * 125_000, title: (*t).into(), thumb: format!("/library/metadata/{i}/thumb") })
                .collect()
        }

        /// What one side drew: the census, its rings (rect, radius) and the runs that are the strip's.
        struct Side {
            runs: Vec<Run>,
            rings: Vec<(Rect, f32)>,
        }

        fn side(c: Census, rings: Vec<(Rect, f32)>) -> Side {
            // the renderer draws the section tabs too (the hand-written strip was only the strip)
            let mut runs: Vec<Run> = c.1.into_iter().filter(|r| r.text != "Chapters" && r.text != "Info").collect();
            runs.sort_by(|a, b| (a.at.x, a.at.y, &a.text).partial_cmp(&(b.at.x, b.at.y, &b.text)).unwrap());
            Side { runs, rings }
        }

        /// (the renderer's side, the hand-written strip's side) with focus on chapter `sel`, scroll at rest.
        fn both(items: Vec<Chapter>, sel: u32) -> (Side, Side) {
            both_at(items, sel, 1.0)
        }

        /// [`both`] with the section at appear `appear` on both sides: the renderer's spring and the strip's argument.
        fn both_at(items: Vec<Chapter>, sel: u32, appear: f32) -> (Side, Side) {
            let _serial = plx_base::testlock::serial();
            let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
            let mut store = plx_data::stores::metadata::MetadataStore::default();
            plx_data::metadata::set_playing_chapters_for_test(store.state_mut(), items);
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
            let state = hud.compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).expect("classic has a chapters state");
            let geo = state.ops.iter().find_map(Geometry::of).expect("the chapters state has a strip");
            let ps = plx_media::route::PlaybackSession::IDLE;
            let model = HudModel::default();
            let up = Countdown::default();
            let mut row = HudMotion::new();
            row.set_section_appear_for_test(Section::Chapters, appear);
            let frame = Frame {
                entry: ENTRY, ps: &ps, model: &model, up: &up, slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None,
                focus: GROUP_SCRUB, btn: 0, tab: 0, open: OPEN_CHAPTERS, now: 1_000, transport: false, measure: &FM, meta: store.view(),
            };
            let key = FocusKey { entry: ENTRY, elem: crate::player_hud::state::ELEM_CHAPTER_BASE + sel };
            let focus = FocusRead { current: Some(key), ..FocusRead::default() };
            // the screen's tick: the engine said focus arrived on `sel`, then frames pass until the Shelf rests
            with_df_focus(focus.clone(), |df| {
                let src = Cards::<u32>::new(store.view().playing_chapters(), 0, geo);
                let mut out = Vec::new();
                let mut present = Present::new();
                let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(9)), &mut present);
                let shelf = row.strip(ENTRY, &geo);
                shelf.on(&ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore }, df, &src, &mut fx);
                for k in 1..=240u32 {
                    shelf.on(&ScreenEvent::Tick(Tick { ms: k * 17, dt_us: 16_667 }), df, &src, &mut fx);
                }
            });
            let mut new = None;
            let rings = capture_rings(|| new = Some(census(|| with_df_focus(focus, |df| assert!(draw(&hud.compiled, &frame, df, &mut row, Painter::root()))))));
            let new = side(new.unwrap(), rings);
            let mut panel = ChaptersState::new(store.view());
            panel.set_sel(sel as std::os::raw::c_int);
            for _ in 0..240 {
                panel.update(0.016_667, store.view());
            }
            let mut old = None;
            let rings = capture_rings(|| old = Some(census(|| panel.draw(&ps, appear, &FM, store.view()))));
            (new, side(old.unwrap(), rings))
        }

        fn close(a: f32, b: f32) -> bool {
            (a - b).abs() < 0.01
        }

        /// A chapter the server named nothing draws "Chapter N" on both sides, in the same place.
        #[test]
        fn an_unnamed_chapter_is_named_by_its_number_as_the_strip_names_it() {
            let (new, old) = both(chapters(&["Opening", "", "The Chase"]), 0);
            let named = |s: &Side| s.runs.iter().find(|r| r.text == "Chapter 2").map(|r| (r.size, r.ink, r.bold, r.at.x, r.at.y));
            assert!(named(&old).is_some(), "the strip draws `Chapter 2`: {:?}", old.runs);
            assert_eq!(named(&new), named(&old));
        }

        /// **D11's marquee, the one visible difference on a chapter at rest.** The hand-written strip elides a
        /// name wider than the card (288) wherever it stands; the Shelf elides an UNFOCUSED name the same way
        /// (still) and sets the FOCUSED one in its block, which is as wide as the Shelf's label window
        /// (`under_budget`: two cards and two gaps) and glides past that. The unfocused names are equal.
        #[test]
        fn a_long_focused_name_shows_whole_where_the_strip_elides_it() {
            let long = "The Very Long Chapter Name That Does Not Fit A Card";
            let (new, old) = both(chapters(&[long, "Dinner", long]), 0);
            let name = |s: &Side, at: f32| s.runs.iter().find(|r| r.size == theme::size::LABEL && (r.at.x - at).abs() < 1.0).map(|r| (r.text.clone(), r.at.w));
            let (focused_new, focused_old) = (name(&new, 96.0).expect("the focused name"), name(&old, 96.0).expect("the strip's"));
            assert!(focused_old.1 <= 288.0 && focused_old.0.ends_with('\u{2026}'), "the strip elides it to the card: {focused_old:?}");
            assert_eq!(focused_new.0, long, "the Shelf sets the whole name in its window");
            assert!(focused_new.1 > 288.0, "…wider than the card: {focused_new:?}");
            let third = 96.0 + 2.0 * (288.0 + 24.0);
            assert_eq!(name(&new, third), name(&old, third), "an unfocused name elides as it did");
            let times = |s: &Side| s.runs.iter().filter(|r| r.size == theme::size::CAPTION).map(|r| (r.text.clone(), r.at.x.round(), r.at.y.round())).collect::<Vec<_>>();
            assert_eq!(times(&new), times(&old), "…and every time stands as it did");
        }

        /// The strip draws what the hand-written strip draws: the same runs (the words, their size, ink, weight and
        /// alignment, where each stands and how wide it is) and the same ring (rect and corner), on the first and the
        /// second chapter (the two seats both scroll rules leave at 0).
        #[test]
        fn a_strip_at_rest_draws_what_the_hand_written_strip_draws() {
            for sel in [0, 1] {
                let (new, old) = both(chapters(&["Opening", "Dinner", "The Chase", "Aftermath", "Credits"]), sel);
                assert_eq!(old.runs.len(), 10, "sel {sel}: a name and a time on each of five cards: {:?}", old.runs);
                assert_eq!(new.runs.len(), old.runs.len(), "sel {sel}: the same runs:\nrenderer {:#?}\nstrip    {:#?}", new.runs, old.runs);
                for (n, o) in new.runs.iter().zip(&old.runs) {
                    assert_eq!((&n.text, n.size, n.ink, n.bold, n.align), (&o.text, o.size, o.ink, o.bold, o.align), "sel {sel}: the run `{}`", o.text);
                    assert!(
                        close(n.at.x, o.at.x) && close(n.at.y, o.at.y) && close(n.at.w, o.at.w),
                        "sel {sel}: `{}` stands at {:?} where the strip draws it at {:?}", o.text, n.at, o.at
                    );
                    assert_eq!((n.clip, n.fade, n.lead), (o.clip, o.fade, o.lead), "sel {sel}: `{}`'s window", o.text);
                }
                assert_eq!(old.rings.len(), 1, "sel {sel}: the focused card wears one ring");
                assert_eq!(new.rings.len(), 1, "sel {sel}: {:?}", new.rings);
                let (n, o) = (new.rings[0], old.rings[0]);
                assert!(close(n.0.x, o.0.x) && close(n.0.y, o.0.y) && close(n.0.w, o.0.w) && close(n.0.h, o.0.h), "sel {sel}: the ring's rect {:?} against {:?}", n.0, o.0);
                assert!(close(n.1, o.1), "sel {sel}: the ring's corner {} against the strip's {}", n.1, o.1);
            }
        }

        /// **The strip's appear is the strip's**: at every point of the spring the section path draws what the
        /// hand-written strip drew at that appear (`Popover::painter`'s fade and `Popover::RISE` rise, as one layer):
        /// the same words, in the same ink and place, and the same ring.
        #[test]
        fn the_strip_at_every_appear_is_what_the_hand_written_strip_drew() {
            for appear in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let (new, old) = both_at(chapters(&["Opening", "Dinner", "The Chase"]), 0, appear);
                assert_eq!(new.runs.len(), old.runs.len(), "appear {appear}: the same runs:\nrenderer {:#?}\nstrip    {:#?}", new.runs, old.runs);
                for (n, o) in new.runs.iter().zip(&old.runs) {
                    assert_eq!((&n.text, n.size, n.bold, n.align), (&o.text, o.size, o.bold, o.align), "appear {appear}: the run `{}`", o.text);
                    assert!(
                        n.ink.iter().zip(&o.ink).all(|(a, b)| (a - b).abs() < 0.001),
                        "appear {appear}: `{}` is inked {:?} where the strip inks it {:?}", o.text, n.ink, o.ink
                    );
                    assert!(
                        close(n.at.x, o.at.x) && close(n.at.y, o.at.y) && close(n.at.w, o.at.w),
                        "appear {appear}: `{}` stands at {:?} where the strip draws it at {:?}", o.text, n.at, o.at
                    );
                }
                assert_eq!(new.rings.len(), old.rings.len(), "appear {appear}: the focused card's ring");
                for (n, o) in new.rings.iter().zip(&old.rings) {
                    assert!(close(n.0.x, o.0.x) && close(n.0.y, o.0.y) && close(n.0.w, o.0.w) && close(n.0.h, o.0.h), "appear {appear}: the ring {:?} against {:?}", n.0, o.0);
                }
            }
        }
    }

    /// **The Info section through the renderer** (`draw_state` over classic's `OPEN_INFO` state), against the
    /// hand-written card (`InfoPanelState::draw`) on the same playing item. The screen does not switch to it
    /// yet, so this is the one place the two meet.
    mod info_section {
        use super::*;
        use crate::hud_format::compile::{OPEN_CHAPTERS, OPEN_INFO};
        use crate::hud_format::Slot;
        use crate::info_panel::{button_rect, card_geometry, InfoPanelState};
        use plx_data::metadata::Detail;

        fn playing() -> Detail {
            Detail {
                title: "The Long Title".into(),
                summary: "A summary of the film that is long enough to run onto a second line when the column is narrow, and then some more words besides.".into(),
                year: 2001,
                dur_ms: 7_320_000,
                rating: "PG-13".into(),
                genres: vec!["Drama".into(), "Thriller".into(), "Crime".into()],
                thumb: "/library/metadata/1/thumb".into(),
                ..Detail::default()
            }
        }

        /// (the renderer's census, the hand-written card's census) over one playing item
        fn both(detail: Detail) -> (Census, Census) {
            both_at(detail, 1.0)
        }

        /// [`both`] with the section at appear `appear` on both sides: the renderer's spring and the card's argument.
        fn both_at(detail: Detail, appear: f32) -> (Census, Census) {
            let _serial = plx_base::testlock::serial();
            let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
            let mut store = plx_data::stores::metadata::MetadataStore::default();
            plx_data::metadata::set_current_for_test(store.state_mut(), Some(detail));
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
            assert!(hud.compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).is_some(), "classic has an info state");
            let ps = plx_media::route::PlaybackSession::IDLE;
            let model = HudModel::default();
            let up = Countdown::default();
            let mut row = HudMotion::new();
            row.set_section_appear_for_test(Section::Info, appear);
            let frame = Frame {
                entry: EntryId(0), ps: &ps, model: &model, up: &up, slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None,
                focus: GROUP_SCRUB, btn: 0, tab: 0, open: OPEN_INFO, now: 1_000, transport: false, measure: &FM, meta: store.view(),
            };
            let new = census(|| with_df(|df| assert!(draw(&hud.compiled, &frame, df, &mut row, Painter::root()))));
            let old = census(|| InfoPanelState::new().draw(&ps, appear, &FM, store.view()));
            (new, old)
        }

        /// **The card's appear is the card's**: at every point of the spring the section path draws what the
        /// hand-written card drew at that appear (its fills with their cascaded alpha, its words, where they stand,
        /// risen by `Popover::RISE * (1 - appear)`), and ONLY the card: the tab row under it is the same at every
        /// appear, as main painted it outside the panel's `.alpha(appear)`.
        #[test]
        fn the_card_at_every_appear_is_what_the_hand_written_card_drew_and_the_tabs_do_not_move() {
            fn near(a: &Rect, b: &Rect) -> bool {
                (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01 && (a.w - b.w).abs() < 0.01 && (a.h - b.h).abs() < 0.01
            }
            // the inks the card did not draw: the tab row's. (The card's focused button wears a glow, tag 5, the
            // section state draws none while the focus is not on the section: that is focus, not appear.)
            fn tabs(new: &Census, old: &Census) -> Vec<(u64, Rect, f32)> {
                let mut rest = new.3.clone();
                for (tag, rect, alpha) in old.3.iter().filter(|(tag, ..)| *tag != 5) {
                    let at = rest
                        .iter()
                        .position(|(t, r, a)| t == tag && near(r, rect) && (a - alpha).abs() < 0.001)
                        .unwrap_or_else(|| panic!("the card inks {tag} in {rect:?} at alpha {alpha}, the renderer does not: {:?}", new.3));
                    rest.remove(at);
                }
                rest
            }
            let (settled_new, settled_old) = both_at(playing(), 1.0);
            let still = tabs(&settled_new, &settled_old);
            for appear in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let (new, old) = both_at(playing(), appear);
                assert_eq!(tabs(&new, &old), still, "appear {appear}: the tab row neither fades nor rises with the card");
                fn words(c: &Census) -> Vec<&Run> {
                    let mut runs: Vec<&Run> = c.1.iter().filter(|r| r.text != "Info" && r.text != "Chapters").collect();
                    runs.sort_by(|a, b| (a.at.x, a.at.y, &a.text).partial_cmp(&(b.at.x, b.at.y, &b.text)).unwrap());
                    runs
                }
                let (n, o) = (words(&new), words(&old));
                assert_eq!(n.len(), o.len(), "appear {appear}: the same words");
                for (n, o) in n.into_iter().zip(o) {
                    assert_eq!((&n.text, n.size, n.bold), (&o.text, o.size, o.bold), "appear {appear}: `{}`", o.text);
                    // the colour of a button's label is its focus's (the card opens on its first button), the fade is the alpha
                    assert!(
                        (n.ink[3] - o.ink[3]).abs() < 0.001,
                        "appear {appear}: `{}` is inked {:?} where the card inks it {:?}", o.text, n.ink, o.ink
                    );
                    assert!(near(&n.at, &o.at), "appear {appear}: `{}` stands at {:?} where the card draws it at {:?}", o.text, n.at, o.at);
                }
            }
        }

        fn run<'a>(c: &'a Census, text: &str) -> &'a Run {
            c.1.iter().find(|r| r.text == text).unwrap_or_else(|| panic!("no run `{text}` in {:?}", c.1))
        }

        /// The ground, the buttons, the glyphs and every run's x, width and size are the card's. The renderer
        /// draws the section tabs too (the compiled state is the panel AND the tab row under it, the hand-written
        /// card was only the panel), so it has one run more: the Info tab's label.
        #[test]
        fn the_ground_the_buttons_the_glyphs_and_the_columns_are_the_cards() {
            let (new, old) = both(playing());
            let (card, _) = card_geometry();
            let rect_of = |c: &Census, r: Rect| c.0.iter().filter(|(_, at)| *at == r).count();
            assert!(rect_of(&new, card) > 0 && rect_of(&new, card) == rect_of(&old, card), "the ground is painted as the card paints it");
            assert_eq!(new.2, old.2, "the glyphs: Play on From the Beginning, Info on Go to Show/Movie");
            assert_eq!(new.1.len(), old.1.len() + 1, "the card's runs and the Info tab's label");
            // the runs in a common order (the tab's label is not the card's: it stands at the screen's left)
            fn ordered(c: &Census) -> Vec<&Run> {
                let mut runs: Vec<&Run> = c.1.iter().filter(|r| r.text != "Info").collect();
                runs.sort_by(|a, b| (a.at.x, &a.text).partial_cmp(&(b.at.x, &b.text)).unwrap());
                runs
            }
            assert_eq!(ordered(&new).len(), ordered(&old).len());
            for (n, o) in ordered(&new).into_iter().zip(ordered(&old)) {
                assert_eq!((&n.text, n.size, n.at.x, n.at.w), (&o.text, o.size, o.at.x, o.at.w), "size, left edge and width of `{}`", o.text);
            }
            // the two buttons' labels are placed by the compile at `button_rect`: the SAME y as the card draws
            for label in ["From Beginning", "Go to Movie"] {
                assert_eq!(run(&new, label).at, run(&old, label).at, "`{label}`");
            }
            let _ = button_rect(0, &FM);
        }

        /// Every run of the two censuses but the tab's label, in a common order (the renderer also draws the
        /// Info tab, which the card does not), as (text, size, x, y, w).
        fn runs(c: &Census) -> Vec<(String, i32, f32, f32, f32)> {
            let mut runs: Vec<(String, i32, f32, f32, f32)> =
                c.1.iter().filter(|r| r.text != "Info").map(|r| (r.text.clone(), r.size, r.at.x, r.at.y, r.at.w)).collect();
            runs.sort_by(|a, b| (a.2, a.3, &a.0).partial_cmp(&(b.2, b.3, &b.0)).unwrap());
            runs
        }

        /// **The text block sits exactly where the card puts it.** The card centres [title, synopsis, tags] in
        /// CAP-TOP coordinates from the REAL wrapped heights and drops what is absent; the renderer places the
        /// compiled `vstack` at draw time the same way (`sections::group`), so every run of the block, its y
        /// included, is the card's. `want` names the runs the case must contain (a case whose census lacks them
        /// would be comparing nothing).
        fn same_block(detail: Detail, want: &[&str], absent: &[&str]) {
            let (new, old) = both(detail);
            let (a, b) = (runs(&new), runs(&old));
            for w in want {
                assert!(b.iter().any(|r| r.0.starts_with(w)), "the card draws a `{w}` run: {b:?}");
            }
            for w in absent {
                assert!(!b.iter().any(|r| r.0.starts_with(w)), "the card draws no `{w}` run: {b:?}");
            }
            assert_eq!(a.len(), b.len(), "the same runs:\nrenderer {a:?}\ncard     {b:?}");
            for (n, o) in a.iter().zip(&b) {
                assert_eq!((&n.0, n.1), (&o.0, o.1), "the same run");
                assert!(
                    (n.2 - o.2).abs() < 0.001 && (n.3 - o.3).abs() < 0.001 && (n.4 - o.4).abs() < 0.001,
                    "`{}` stands at ({}, {}, w {}) where the card draws it at ({}, {}, w {})",
                    o.0, n.2, n.3, n.4, o.2, o.3, o.4
                );
            }
        }

        #[test]
        fn a_two_line_synopsis_and_a_full_facts_row_sit_where_the_card_puts_them() {
            same_block(playing(), &["The Long Title", "A summary of the film", "Drama"], &[]);
        }

        #[test]
        fn a_one_line_synopsis_sits_where_the_card_puts_it() {
            same_block(Detail { summary: "A short one.".into(), ..playing() }, &["The Long Title", "A short one.", "Drama"], &[]);
        }

        #[test]
        fn an_item_with_no_synopsis_drops_the_synopsis_and_its_gap() {
            same_block(Detail { summary: String::new(), ..playing() }, &["The Long Title", "Drama"], &["A summary"]);
        }

        /// `n == 0` drops the tag row, which is not the same as having no genres: an item with a year and a
        /// duration but no genres still has its row, and an item with none of the facts has none.
        #[test]
        fn an_item_with_no_facts_drops_the_tag_row() {
            let bare = Detail { genres: vec![], year: 0, dur_ms: 0, rating: String::new(), ..playing() };
            same_block(bare.clone(), &["The Long Title", "A summary of the film"], &["Drama", "2001", "PG-13"]);
            same_block(Detail { summary: String::new(), ..bare }, &["The Long Title"], &["A summary", "Drama", "2001"]);
        }

        #[test]
        fn an_item_with_facts_but_no_genres_still_has_its_tag_row() {
            same_block(Detail { genres: vec![], ..playing() }, &["The Long Title", "A summary of the film", "2001"], &["Drama"]);
        }

        /// **A group never leaves the rect the compiler promised** (lint, Cost and the census read the worst
        /// case): whatever is absent, the block's texts stand inside the group's compiled rect. The group stacks
        /// in CAP-TOP coordinates, so a text's cap top (its box top, a run's `at.y`, plus the face's cap offset) is
        /// what is compared; the full block fills the rect exactly.
        #[test]
        fn a_group_stays_inside_its_worst_case_rect() {
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
            let state = hud.compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).expect("an info state");
            assert_eq!(state.groups.len(), 1, "classic's info text block is one group");
            let rect = state.groups[0].rect;
            let bare = Detail { genres: vec![], year: 0, dur_ms: 0, rating: String::new(), ..playing() };
            for detail in [
                playing(),
                Detail { summary: "A short one.".into(), ..playing() },
                Detail { summary: String::new(), ..playing() },
                bare.clone(),
                Detail { summary: String::new(), ..bare },
            ] {
                let (new, _) = both(detail);
                let mut seen = 0;
                for r in new.1.iter().filter(|r| r.text == "The Long Title" || r.text.starts_with("A s")) {
                    let cap = r.at.y + plx_gfx::text::text_cap_band(r.size, i32::from(r.text == "The Long Title")).0;
                    assert!(cap >= rect.y - 0.5 && cap <= rect.y + rect.h, "`{}` at cap top {cap} leaves the group {rect:?}", r.text);
                    seen += 1;
                }
                assert!(seen > 0, "the case draws the title: {:?}", new.1);
            }
        }

        /// A `vstack` that holds the title pair is not a group: the reference's open Info state stacks its
        /// title and summary at the compiler's rects, its summary last and top-anchored, so there is nothing to collapse.
        #[test]
        fn a_package_vstack_with_the_title_pair_stays_static() {
            let text = include_str!("../hud_format/fixtures/reference.plxhud.json");
            let compiled = crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FM)
                .expect("the reference loads")
                .compiled;
            let state = compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).expect("the reference has an info state");
            assert!(state.groups.is_empty(), "the reference's info vstack is static: {:?}", state.groups.iter().map(|g| g.rect).collect::<Vec<_>>());
        }

        #[test]
        fn an_open_chapters_state_has_no_info_arms_and_the_gate_only_hides_the_compact_state() {
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
            assert!(hud.compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).is_some());
        }
    }

    /// **An open section's focus**: the buttons' pop on `GROUP_SECTION` and the tab's selected state, both read
    /// from the walk (the state it is in, the group the focus is on), so the screen has nothing to hand in but
    /// the group.
    mod section_focus {
        use super::*;
        use crate::hud_format::compile::OPEN_INFO;
        use crate::hud_format::Slot;

        fn info_frame_census(transport: bool, tab: i32, focus: GroupId, btn: i32, row: &mut HudMotion) -> Census {
            let _serial = plx_base::testlock::serial();
            let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
            let mut store = plx_data::stores::metadata::MetadataStore::default();
            plx_data::metadata::set_current_for_test(store.state_mut(), Some(plx_data::metadata::Detail { title: "T".into(), ..Default::default() }));
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM);
            let state = hud.compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).expect("an info state");
            let ps = plx_media::route::PlaybackSession::IDLE;
            let model = HudModel::default();
            let up = Countdown::default();
            let frame = Frame {
                entry: EntryId(0), ps: &ps, model: &model, up: &up, slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None,
                focus, btn, tab, open: 0, now: 1_000, transport, measure: &FM, meta: store.view(),
            };
            census(|| with_df(|df| draw_state(&hud.compiled, state, &frame, df, row, Painter::root())))
        }

        /// The Info tab reads selected because the STATE has Info open, not because the transport is hidden: an
        /// inline policy keeps the transport (and the tab cursor) live.
        #[test]
        fn the_tab_of_an_open_section_reads_selected_whatever_the_transport_and_the_cursor() {
            let mut row = HudMotion::new();
            let modal = info_frame_census(false, 0, GROUP_SCRUB, 0, &mut row);
            let inline = info_frame_census(true, 1, GROUP_SCRUB, 0, &mut row);
            assert_eq!(census_hash(&modal), census_hash(&inline), "the Info tab is drawn selected in both");
        }

        /// The `toggle_section` face is the STATE's: a section's control is selected while its bit is open, a
        /// sibling's is not, and the compact state (no bit) selects none. The reference's two are discs, so
        /// this is also the disc's `solid` face (`draw_control` reads the same answer).
        #[test]
        fn a_toggle_section_control_reads_selected_while_its_section_is_open() {
            let text = include_str!("../hud_format/fixtures/reference.plxhud.json");
            let compiled = crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FM)
                .expect("the reference loads")
                .compiled;
            let open = |bits: u8| {
                let state = compiled.lookup(bits, Slot::Discs, true, true, false).expect("a state");
                let on = |id: &str| state.controls.iter().find(|c| c.id == id).map(|c| section_open(state, c));
                (on("info"), on("chapters"))
            };
            assert_eq!(open(0), (Some(false), Some(false)), "the compact state selects none");
            assert_eq!(open(OPEN_INFO), (Some(true), Some(false)), "Info open selects the Info control alone");
        }

        #[test]
        fn a_section_button_pops_only_while_the_section_group_has_focus() {
            use crate::player_hud::geometry::GROUP_SECTION;
            let slot = ControlSlot::Discs(Controls::NONE);
            let mut row = HudMotion::new();
            for _ in 0..20 {
                row.step(slot, GROUP_SECTION, 1, 0.05, 1_000);
            }
            assert!(row.section_scale(1) > 1.0, "the focused section button pops: {}", row.section_scale(1));
            assert_eq!(row.section_scale(0), 1.0, "its sibling does not");
            assert_eq!(row.scale(1), 1.0, "and the control row's own springs are not the section's");
            for _ in 0..40 {
                row.step(slot, GROUP_ROW, 1, 0.05, 1_000);
            }
            assert_eq!(row.section_scale(1), 1.0, "focus moved off the section group: it settles");
        }

        /// The drawn button wears the pop: the focused button's census differs from its resting one.
        #[test]
        fn the_focused_section_button_is_drawn_popped() {
            use crate::player_hud::geometry::GROUP_SECTION;
            let slot = ControlSlot::Discs(Controls::NONE);
            let mut rest = HudMotion::new();
            let mut popped = HudMotion::new();
            for _ in 0..20 {
                popped.step(slot, GROUP_SECTION, 0, 0.05, 1_000);
            }
            let at_rest = info_frame_census(false, 0, GROUP_SECTION, 0, &mut rest);
            let lifted = info_frame_census(false, 0, GROUP_SECTION, 0, &mut popped);
            assert_ne!(census_hash(&at_rest), census_hash(&lifted), "the popped button is bigger than the resting one");
        }
    }

    /// **The reference's compact state draws.** Its faces, rest and focus faces and playbar options all
    /// reach the painter: the asset glyphs, the bare discs, the ring on the focused Play disc, the
    /// permanent knob and the clocks at the bar's ends.
    mod reference {
        use super::*;
        use crate::hud_format::compile::{ClockMode, KnobMode};

        fn reference() -> std::sync::Arc<Compiled> {
            let text = include_str!("../hud_format/fixtures/reference.plxhud.json");
            crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FM)
                .expect("the reference loads")
                .compiled
        }

        fn frame_census(compiled: &Compiled, focus: GroupId, btn: i32, model: HudModel) -> Census {
            frame_census_with(compiled, ControlSlot::Discs(Controls::NONE), focus, btn, model)
        }

        fn frame_census_with(compiled: &Compiled, slot: ControlSlot, focus: GroupId, btn: i32, model: HudModel) -> Census {
            let ps = plx_media::route::PlaybackSession::IDLE;
            let store = plx_data::stores::metadata::MetadataStore::default();
            let up = Countdown::default();
            let mut row = HudMotion::new();
            let frame = Frame {
                entry: EntryId(0), ps: &ps, model: &model, up: &up, slot, busy: Busy::None, focus, btn,
                tab: 0, open: 0, now: 1_000, transport: true, measure: &FM, meta: store.view(),
            };
            census(|| assert!(with_df(|df| draw(compiled, &frame, df, &mut row, Painter::root())), "the reference has a compact state"))
        }

        fn model() -> HudModel {
            HudModel { duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() }
        }

        fn ordinal(compiled: &Compiled, id: &str) -> i32 {
            let state = compiled.lookup(0, crate::hud_format::Slot::Discs, false, false, false).unwrap();
            state.row_ids.iter().position(|r| r.as_deref() == Some(id)).unwrap_or_else(|| panic!("no `{id}`")) as i32
        }

        #[test]
        fn the_package_faces_reach_the_painter() {
            let compiled = reference();
            let (hud_seen, svgs) = {
                let mut seen = None;
                let svgs = plx_ui::draw_census::capture_svgs(|| seen = Some(frame_census(&compiled, GROUP_SCRUB, 0, model())));
                (seen.unwrap(), svgs)
            };
            let key = |id: &str| compiled.glyph(id).map(|g| g.key).unwrap();
            for id in ["subs-bubble", "back10", "fwd30"] {
                assert!(svgs.iter().any(|(k, _)| *k == key(id)), "the `{id}` asset draws on its disc: {svgs:?}");
            }
            // the app's own glyphs for the commands whose faces are `builtin:`: the play disc shows its
            // `playing` face (pause) and the Info section disc its own; a movie has no Previous or Next
            let icons: Vec<Icon> = hud_seen.2.iter().map(|(i, _)| *i).collect();
            assert_eq!(icons, [Icon::Pause, Icon::Info], "a playing movie");
            let paused = frame_census(&compiled, GROUP_SCRUB, 0, HudModel { paused: true, ..model() });
            assert!(paused.2.iter().any(|(i, _)| *i == Icon::Play), "the play disc's `paused` face: {:?}", paused.2);
            let pair = frame_census_with(&compiled, ControlSlot::Discs(PAIR), GROUP_SCRUB, 0, model());
            let icons: Vec<Icon> = pair.2.iter().map(|(i, _)| *i).collect();
            assert!(icons.contains(&Icon::Previous) && icons.contains(&Icon::Next), "an episode pair: {icons:?}");
            let restart = frame_census_with(&compiled, ControlSlot::Discs(RESTART), GROUP_SCRUB, 0, model());
            assert!(restart.2.iter().any(|(i, _)| *i == Icon::Restart), "Previous's `restart` face");
        }

        #[test]
        fn the_play_disc_wears_the_ring_when_focused() {
            let compiled = reference();
            let play = ordinal(&compiled, "play");
            let rings = |c: &Census| c.0.iter().filter(|(t, _)| *t == 3).count();
            let rest = frame_census(&compiled, GROUP_SCRUB, 0, model());
            let focused = frame_census(&compiled, GROUP_ROW, play, model());
            assert_eq!(rings(&focused), rings(&rest) + 1, "one ring arrives with the focus on Play");
            // the other discs keep the accent focus: no ring on them
            let subs = ordinal(&compiled, "subs");
            let on_subs = frame_census(&compiled, GROUP_ROW, subs, model());
            assert_eq!(rings(&on_subs), rings(&rest), "a disc whose focus is `accent` draws none");
        }

        #[test]
        fn the_bar_keeps_its_knob_and_its_clocks_at_the_ends() {
            let compiled = reference();
            let state = compiled.lookup(0, crate::hud_format::Slot::Discs, false, false, false).unwrap();
            let (track, knob, clocks) = state
                .ops
                .iter()
                .find_map(|op| match op {
                    DrawOp::Playbar { track, knob, clocks, .. } => Some((*track, *knob, *clocks)),
                    _ => None,
                })
                .expect("the reference places a bar");
            assert_eq!((knob, clocks), (KnobMode::Always, ClockMode::Ends));
            // the remote on a disc: the bar is at rest, so its knob is the permanent one
            let drawn = frame_census(&compiled, GROUP_ROW, 0, model());
            let elapsed = drawn.1.iter().find(|r| r.text == "1:30").unwrap_or_else(|| panic!("{:?}", drawn.1));
            assert!((elapsed.at.x - elapsed.at.w * 0.5 - track.x).abs() < 1.0, "elapsed starts the bar: {:?} vs {track:?}", elapsed.at);
            assert!(drawn.0.iter().any(|(_, r)| (r.w - 18.0).abs() < 0.01 && (r.h - 18.0).abs() < 0.01), "the permanent knob");
        }

        /// A disc whose command is disabled has no focus stop and draws its disabled ink: the seek
        /// discs need a known duration.
        #[test]
        fn a_seek_disc_without_a_duration_draws_disabled() {
            let compiled = reference();
            let live = frame_census(&compiled, GROUP_SCRUB, 0, model());
            let dead = frame_census(&compiled, GROUP_SCRUB, 0, HudModel { duration_ns: 0, ..model() });
            assert_ne!(live.3, dead.3, "the fills differ when the seek discs go dead");
            assert_ne!(live.0, dead.0);
        }

        /// **An inline section's ops stacked above it ride one delta** (`docs/hud-plugins.md`, "Sections"): the
        /// reference lays the Info band UNDER the transport, so opening it lifts the title, the bar and the row.
        /// At appear `a` each of them stands `(1 - a)` of that lift lower than where the open state puts it, so at
        /// `0` the block is where the closed state puts it and at `1` where the open one does. The bar's clocks and
        /// the row's discs are the ops above the band; both ride the SAME delta.
        #[test]
        fn the_ops_above_an_inline_section_stand_one_delta_lower_while_it_appears() {
            use crate::hud_format::compile::OPEN_INFO;
            let compiled = reference();
            let at = |open: u8, appear: Option<f32>| -> Census {
                let ps = plx_media::route::PlaybackSession::IDLE;
                let store = plx_data::stores::metadata::MetadataStore::default();
                let up = Countdown::default();
                let model = model();
                let mut row = HudMotion::new();
                if let Some(a) = appear {
                    row.set_section_appear_for_test(Section::Info, a);
                }
                let frame = Frame {
                    entry: EntryId(0), ps: &ps, model: &model, up: &up, slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None,
                    focus: GROUP_SCRUB, btn: 0, tab: 0, open, now: 1_000, transport: true, measure: &FM, meta: store.view(),
                };
                census(|| assert!(with_df(|df| draw(&compiled, &frame, df, &mut row, Painter::root()))))
            };
            let clock = |c: &Census| c.1.iter().find(|r| r.text == "1:30").map(|r| r.at.y).expect("the bar's elapsed clock");
            let disc = |c: &Census| c.2.iter().find(|(i, _)| *i == Icon::Pause).map(|(_, r)| r.y).expect("the play disc");
            let closed = at(0, None);
            let open = at(OPEN_INFO, Some(1.0));
            let (clock_lift, disc_lift) = (clock(&closed) - clock(&open), disc(&closed) - disc(&open));
            assert!(clock_lift > 8.0, "opening the section lifts the bar by something real: {clock_lift}");
            assert!((clock_lift - disc_lift).abs() < 0.01, "…and the row by the same: {clock_lift} against {disc_lift}");
            for appear in [0.0, 0.5, 1.0] {
                let drawn = at(OPEN_INFO, Some(appear));
                let owed = (1.0 - appear) * clock_lift;
                assert!(
                    (clock(&drawn) - (clock(&open) + owed)).abs() < 0.01,
                    "appear {appear}: the clock stands at {} where the open state's {} plus {owed} puts it", clock(&drawn), clock(&open)
                );
                assert!(
                    (disc(&drawn) - (disc(&open) + owed)).abs() < 0.01,
                    "appear {appear}: the play disc stands at {} where the open state's {} plus {owed} puts it", disc(&drawn), disc(&open)
                );
            }
        }
    }
}
