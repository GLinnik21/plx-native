//! **The section arms of the render walk** (`docs/hud-plugins.md`, D6 and PR8): what a `ground`, a
//! `rule`, an `icon`, an `image`, a `text` of the sections' own binds, a `facts` row and a `button` draw
//! when [`super::render::draw_state`] meets them in an open-section state. They are arms of the ONE walk
//! over [`CompiledState::ops`](crate::hud_format::compile::CompiledState), never a second renderer, and
//! every one is placed by the compile: the rect an op carries is where it draws, as the title's are.
//!
//! **Where the data comes from.** [`HudModel`](super::model::HudModel) is `Copy` ("no borrowed string and
//! no allocation"), so the Info rows and the chapter list cannot join it as the PR7 handoff wrote; the
//! frame's [`MetadataView`](plx_data::metadata::MetadataView) already carries both, sampled by the app,
//! and `state_for` already asks it whether the item has chapters. The arms read the item through
//! [`info_content`], the one reading the hand-written card draws from too.
//!
//! **What a text op's `y` is.** The compile places a text's BOX top at `rect.y` (the title and the chapter
//! names are drawn by `Painter::text` there), whereas [`TextView`] takes the CAP top. [`line_frame`] is the
//! one conversion, so a block drawn here sits where the same box drawn by `Painter::text` would.

use std::ffi::CString;

use plx_ui::hstack::{Item, Kept};
use plx_ui::icons::Icon;
use plx_ui::text_view::TextView;
use plx_ui::theme;
use plx_ui::vstack::VStack;
use plx_ui::widgets::{resolve_tex_wh_on, Button};
use plx_ui::{Painter, Rect, View};

use super::classic::tab_labels;
use super::command::HudCommand;
use super::render::Frame;
use crate::chip_row::{draw_chip_row, ChipRow};
use crate::hud_format::compile::{Compiled, CompiledState, ControlRect, DrawOp, Live, OpGroup};
use crate::info_panel::{actions, info_chips, info_content, FactsShape};

type Rgba = [f32; 4];

/// The frame's answer to a `Live` term's NAME. Only `transport.paused` is a name the sections' own ops can
/// key on; every other name (`item.*` is the strip card's, drawn by its own walk) is false here.
pub(super) fn holds<'a>(f: &'a Frame<'_>) -> impl Fn(&str) -> bool + 'a {
    move |name| name == "transport.paused" && f.model.paused
}

/// A `Ground`: the section's panel, one rounded fill.
pub(super) fn ground(q: Painter, rect: Rect, radius: f32, ink: &Live<Rgba>, f: &Frame<'_>) {
    q.rrect(rect, radius, radius, ink.at(holds(f)));
}

/// A `Rule`: one hairline across `rect`'s width, in the package's ink.
pub(super) fn rule(q: Painter, rect: Rect, ink: &Live<Rgba>, f: &Frame<'_>) {
    plx_ui::widgets::hairline_in(q, rect.x, rect.y, rect.w, ink.at(holds(f)));
}

/// An `Icon`: a package asset's glyph filling its cell.
pub(super) fn icon(compiled: &crate::hud_format::compile::Compiled, q: Painter, rect: Rect, asset: &str, ink: &Live<Rgba>, f: &Frame<'_>) {
    if let Some(svg) = compiled.glyph(asset) {
        plx_ui::icons::draw_svg(q, svg, rect, ink.at(holds(f)));
    }
}

/// An `Image`. `info.still` is the playing item's 16:9 still, cropped to the box and never squashed (a 4:3
/// still or a movie's art of any aspect comes back at its own shape: `minSize=1` covers the box, it does
/// not fit it), from the PLAYING item's server; a still that has not arrived is the flat placeholder.
pub(super) fn image(f: &Frame<'_>, q: Painter, rect: Rect, bind: &str, radius: f32) {
    if bind != "info.still" {
        return;
    }
    let Some(c) = info_content(f.meta) else { return };
    let mut drawn = false;
    if !c.thumb.is_empty() {
        let (t, tw, th) = resolve_tex_wh_on(
            plx_media::route::item_sid(plx_media::route::cur_sid(f.ps)).raw(),
            &c.thumb,
            480,
            270,
            0,
        );
        if t != 0 {
            q.tex_uv(t, rect.cover_uv(tw, th, plx_ui::Crop::Centre), rect, radius, theme::TINT_WHITE);
            drawn = true;
        }
    }
    if !drawn {
        plx_ui::placeholder::flat(q, plx_ui::placeholder::Reason::InfoPanelStill, &c.thumb, rect, radius);
    }
}

/// The text a section's `text` bind names, or `None` for a bind with no arm here (`title.*` is the title
/// pair's, `item.*` the strip card's).
fn text_of(f: &Frame<'_>, bind: &str) -> Option<String> {
    match bind {
        "info.title" => info_content(f.meta).map(|c| c.title),
        "info.summary" => info_content(f.meta).map(|c| c.summary),
        "label.info" => tab_labels(false)[0].map(str::to_owned),
        "label.chapters" => tab_labels(true)[1].map(str::to_owned),
        _ => None,
    }
}

/// Whether `bind` is one of the section text binds [`text`] draws.
pub(super) fn is_section_text(bind: &str) -> bool {
    matches!(bind, "info.title" | "info.summary" | "label.info" | "label.chapters")
}

/// The frame a [`TextView`] draws a line box into so the box's top lands on `top`: the view takes the CAP
/// top, the compile places the box top ([`module doc`](self)).
pub(super) fn line_frame(rect: Rect, size: i32, bold: bool) -> Rect {
    let (cap_top, _) = plx_gfx::text::text_cap_band(size, i32::from(bold));
    Rect::new(rect.x, rect.y + cap_top, rect.w, 0.0)
}

/// The shared [`TextView`] a section `text` draws through: wrapped at the compiled `max_lines` and `leading`.
fn text_view<'a>(content: &'a str, size: i32, bold: bool, ink: Rgba, max_lines: u8, leading: f32) -> TextView<'a> {
    let view = TextView::new(content, size, ink).leading(leading).max_lines(usize::from(max_lines));
    if bold {
        view.bold()
    } else {
        view
    }
}

/// A section `text`: wrapped through the shared [`TextView`] at the compiled width, `max_lines` and `leading`.
#[allow(clippy::too_many_arguments)]
pub(super) fn text(
    f: &Frame<'_>,
    q: Painter,
    rect: Rect,
    bind: &str,
    size: i32,
    bold: &Live<bool>,
    ink: &Live<Rgba>,
    alpha: f32,
    max_lines: u8,
    leading: f32,
) {
    let Some(content) = text_of(f, bind) else { return };
    let bold = bold.at(holds(f));
    let view = text_view(&content, size, bold, ink.at(holds(f)), max_lines, leading);
    view.draw(if alpha < 1.0 { q.alpha(alpha) } else { q }, line_frame(rect, size, bold));
}

/// A `Facts` row, built and fit to the cell's width (tail-first drop): the chips and the cut, which the
/// group measures before it draws (`n == 0` drops the row) exactly as the Info card does.
pub(super) struct FactsRun {
    chips: ChipRow,
    kept: Kept,
    /// how many chips survive the cut
    pub n: usize,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn facts_run(
    f: &Frame<'_>,
    rect: Rect,
    order: &[String],
    badges: &[String],
    genres_max: u32,
    playback_fact: bool,
    bold: &Live<bool>,
    ink: &Live<Rgba>,
) -> Option<FactsRun> {
    let c = info_content(f.meta)?;
    let (order, badges): (Vec<&str>, Vec<&str>) = (order.iter().map(String::as_str).collect(), badges.iter().map(String::as_str).collect());
    let shape = FactsShape {
        order: &order,
        badges: &badges,
        genres_max: genres_max as usize,
        playback_fact,
        ink: ink.at(holds(f)),
        bold: i32::from(bold.at(holds(f))),
    };
    let chips = info_chips(&c, &shape, f.ps, f.measure);
    let row = chips.row(rect.x);
    let kept = row.fit(rect.w);
    let n = row.kept(kept).placed().count();
    Some(FactsRun { chips, kept, n })
}

impl FactsRun {
    /// Draw the surviving chips centred on `my`.
    fn draw(&self, q: Painter, x: f32, my: f32, f: &Frame<'_>) {
        draw_chip_row(q, &self.chips, self.chips.row(x).kept(self.kept), my, f.measure);
    }
}

/// A `Facts` row: the metadata line and the capability badges, centred on the cell's height.
#[allow(clippy::too_many_arguments)]
pub(super) fn facts(
    f: &Frame<'_>,
    q: Painter,
    rect: Rect,
    order: &[String],
    badges: &[String],
    genres_max: u32,
    playback_fact: bool,
    bold: &Live<bool>,
    ink: &Live<Rgba>,
) {
    if let Some(run) = facts_run(f, rect, order, badges, genres_max, playback_fact, bold, ink) {
        run.draw(q, rect.x, rect.y + rect.h * 0.5, f);
    }
}

/// **A group's children, stacked from the REAL heights** ([`OpGroup`]; `docs/hud-plugins.md`, PR8). The hand-written
/// Info card does this by hand: it centres [title, synopsis, tags] in CAP-TOP coordinates from the heights
/// it actually wraps and drops what is absent. This is the same run through the same [`VStack`]: each child is
/// an [`Item`] of its real height (a text, `lines x leading`, absent when it is a block of more than one line
/// with nothing to say; a facts row, its compiled height, absent when the cut keeps no chip; anything else,
/// its compiled height), carrying the cell's `gap_before` / `gap_after`, anchored where the compile's `valign`
/// put the box. A text draws with its CAP top on the span (the compile's box top is a line box above it), a facts
/// row centred on its span, a fixed child at its span, moved by the distance it moved.
///
/// A single line (a heading) keeps its line when it is empty, as the card's title does; a block collapses.
pub(super) fn group(compiled: &Compiled, f: &Frame<'_>, q: Painter, state: &CompiledState, g: &OpGroup) {
    enum Body<'a> {
        Text { view: TextView<'a>, rect: Rect, alpha: f32 },
        Facts { run: Option<FactsRun>, rect: Rect },
        Fixed(&'a [DrawOp]),
    }
    // the texts a child names, owned first so the views can borrow them
    let ops_of = |c: &crate::hud_format::compile::GroupChild| &state.ops[c.ops.clone()];
    let contents: Vec<Option<String>> = g
        .children
        .iter()
        .map(|c| match ops_of(c) {
            [DrawOp::Text { bind, .. }] => text_of(f, bind),
            _ => None,
        })
        .collect();
    let mut bodies = Vec::with_capacity(g.children.len());
    let mut items = Vec::with_capacity(g.children.len());
    for (c, content) in g.children.iter().zip(&contents) {
        let (body, h) = match ops_of(c) {
            [DrawOp::Text { rect, size, bold, ink, alpha, max_lines, leading, .. }] => {
                let text = content.as_deref().unwrap_or("");
                let view = text_view(text, *size, bold.at(holds(f)), ink.at(holds(f)), *max_lines, *leading);
                // a block with nothing to say takes no room; a heading keeps its line
                let h = (!text.is_empty() || (*max_lines == 1 && content.is_some())).then(|| view.measure_h(rect.w));
                (Body::Text { view, rect: *rect, alpha: *alpha }, h)
            }
            [DrawOp::Facts { rect, order, badges, genres_max, playback_fact, bold, ink, .. }] => {
                let run = facts_run(f, *rect, order, badges, *genres_max, *playback_fact, bold, ink);
                let h = run.as_ref().is_some_and(|r| r.n > 0).then_some(rect.h);
                (Body::Facts { run, rect: *rect }, h)
            }
            ops => (Body::Fixed(ops), Some(c.h)),
        };
        let item = h.map_or_else(Item::absent, Item::new);
        let item = c.after.map_or(item, |a| item.after(a));
        items.push(c.before.map_or(item, |b| item.before(b)));
        bodies.push(body);
    }
    for span in VStack::new(&items, g.gap, g.anchor).placed() {
        match &bodies[span.i] {
            Body::Text { view, rect, alpha } => {
                view.draw(if *alpha < 1.0 { q.alpha(*alpha) } else { q }, Rect::new(rect.x, span.y, rect.w, 0.0));
            }
            Body::Facts { run: Some(run), rect } => run.draw(q, rect.x, span.y + rect.h * 0.5, f),
            Body::Facts { run: None, .. } => {}
            Body::Fixed(ops) => {
                let moved = q.translate(0.0, span.y - g.children[span.i].y);
                for op in *ops {
                    match op {
                        DrawOp::Image { rect, bind, radius } => image(f, moved, *rect, bind, *radius),
                        DrawOp::Icon { rect, asset, ink } => icon(compiled, moved, *rect, asset, ink, f),
                        DrawOp::Rule { rect, ink } => rule(moved, *rect, ink, f),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The glyph of an `actions` button: the hand-written card's (Play on From the Beginning, Info on Go to
/// Show / Movie), not the disc row's `Restart`, because a button has a label beside it.
fn button_icon(act: Option<HudCommand>) -> Icon {
    match act {
        Some(HudCommand::FromBeginning) => Icon::Play,
        _ => Icon::Info,
    }
}

/// An `actions` `button`, at the rect the compile placed it (one width for the column, ten languages
/// measured). Its label is the command's app string for the playing item (`actions`); `focused` and
/// `scale` are the section group's (stage 2 registers it), at rest until then.
pub(super) fn button(f: &Frame<'_>, q: Painter, e: &plx_ui::Env, c: &ControlRect, focused: bool, scale: f32) {
    let label = match c.act {
        Some(HudCommand::FromBeginning) => actions(f.meta)[0],
        Some(HudCommand::GoToDetail) => actions(f.meta)[1],
        _ => return,
    };
    let Ok(label) = CString::new(label) else { return };
    Button::new(label.as_ptr(), theme::size::BODY, c.rect)
        .icon(button_icon(c.act))
        .focused(focused)
        .scale(scale)
        .draw(e, q);
}
