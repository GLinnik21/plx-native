//! The player transport HUD. Composed from retui widgets (Spinner / TransportButton / TabPill /
//! ProgressBar-style scrubber) drawn through a `Painter`. The video-overlay subtitle draws below stay
//! on the raw text/tex primitives (they composite directly over the video plane, outside the
//! transport HUD).
//!
//! This was one 3600-line file; it is now a directory so that a layout can be swapped without
//! touching the rules, and so that the draw path reads no player global (the localized labels it draws go through
//! `plx_platform::i18n`, [`classic::tab_labels`] among them):
//!
//! | module | what it is |
//! |---|---|
//! | [`state`] | the HUD's LOGIC, no pixels: the control slot (which disc / pill / pair occupies the row), the busy and transport marks, the failure read-out's rows and actions, the scrub tuning. Re-exported here, so `player_hud::slot`, `player_hud::Action`, `player_hud::scrub_speed` keep their paths |
//! | [`model`] | [`model::HudModel`]: the per-frame, read-only snapshot of every player global the presentation reads. The draws take what they paint and the values they read as arguments; the scrub preview and the renderer's `Frame` carry the snapshot |
//! | [`motion`] | the clock prewarm ([`motion::ClockWarm`]), the pop-in of a row ([`motion::RowPop`]) and the HUD's per-frame animation state ([`motion::HudMotion`], formerly `TransportRow`) |
//! | [`geometry`] | the focus GROUPS ([`geometry::GROUP_SCRUB`] ..), [`geometry::HudGeometry`] and [`geometry::RowAnchor`]: what a layout reports back to focus, the pointer, the overscan audit and the stand-ins |
//! | [`command`] | [`command::HudCommand`], the closed vocabulary of things a control asks the app to do, with its app-owned enabled predicates, and classic's five discs as controls |
//! | [`classic`] | the transport's primitives and `builtin:classic`'s constants: the draws the renderer calls (scrim, title, playbar, clocks, scrub preview), the three the trailer calls (scrim, title, playbar), the failure read-out, `ROW_ANCHOR`; the tab pills are the renderer's. The arrangement is the compiled package's |
//! | [`loaded`] | a package parsed and compiled once, never in the draw ([`loaded::LoadedHud`]), and the production [`AppLabels`](crate::hud_format::compile::AppLabels) constructor over a passed-in locale |
//! | [`render`] | the renderer: it draws a [`Compiled`](crate::hud_format::compile::Compiled) package's state through the shared widgets; `builtin:classic` is the embedded package it draws |
//! | [`seek_jump`] | the long-seek jump chord |
//!
//! What stays here is the caption drawing (`draw_subtitle_message`, `draw_subtitle_bitmap`), which
//! composites over the video plane and belongs to no layout, and `overscan_rects`.
#![allow(dead_code)]
pub mod classic;
pub mod command;
pub mod geometry;
pub mod loaded;
pub mod model;
pub mod motion;
pub mod render;
pub mod seek_jump;
pub mod state;
use plx_gfx::gfx::{delete_tex, upload_rgba};
use plx_machine::machine::Measure;
// The caption's look (face, pitch, wrap, lift, anchors) is one definition, media's `text_style`,
// shared with the synthesized ASS header (plan `libass-text-subs` §2.5, PR 5a).
pub use plx_media::player::text_style::{HudBand, CAPSULE_BOTTOM_Y};
use plx_media::player::text_style::{
    position_lift, size_scale, wrap_width, CAPTION_ANCHOR_LINES, CAPTION_FACE, CAPTION_FLOOR_Y,
    CAPTION_INK_CLEARANCE, CAPTION_LINE_PITCH, CAPTION_TEXT_INSET, SUB_BASE_Y,
};
pub use state::*;
use model::HudModel;
use plx_ui::consts::{SAFE, SCR_H, SCR_W};
use plx_ui::theme;
use plx_ui::{Painter, Rect};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};

/// Shared caption placement for the libass fault message and the sidecar failure notice. Text size and
/// vertical placement follow the viewer's Settings > Playback picks (`route::subtitle_size`,
/// `route::subtitle_position`) — native ASS/SSA keeps its authored style instead
/// (`docs/ass-subtitles.md`). The message is a short fixed string, so it is never paged: a block
/// taller than the area is drawn whole at the smallest step. `band` is the HUD's, as its layout
/// reports it ([`geometry::HudGeometry::band`]): the ceiling the block lifts to while `hud_up`.
pub fn draw_subtitle_message(text: &str, hud_up: bool, band: HudBand, model: &HudModel, measure: &dyn Measure) {
    let (scale, baseline, floor) = caption_geometry(model, hud_up, band);
    let lines = caption_wrap(text, scale, measure);
    if lines.is_empty() {
        return;
    }
    let lay = caption_layout(lines.len(), scale, baseline, floor);
    draw_caption_block(&lines, &lay, model);
}

/// The viewer's size and the y a caption block is placed from: sit near the bottom normally; lift
/// above the scrubber/tabs while the HUD is up; the viewer's Position pick lifts it further still,
/// same direction, on top of that. The third value is the lowest y the block may reach — with the
/// HUD up that is the band's ceiling, so a block that grows down cannot run into the scrubber.
fn caption_geometry(model: &HudModel, hud_up: bool, band: HudBand) -> (f32, f32, f32) {
    caption_geometry_for(model.sub_size, model.sub_position, hud_up, band)
}

/// [`caption_geometry`] for explicit picks (the tie test pins it against `text_style::today_baseline`).
fn caption_geometry_for(
    size: plx_plex::plex::session::SubtitleSize,
    position: plx_plex::plex::session::SubtitlePosition,
    hud_up: bool,
    band: HudBand,
) -> (f32, f32, f32) {
    let scale = size_scale(size);
    let baseline = (if hud_up { band.ceil_y } else { SUB_BASE_Y }) - position_lift(position);
    let floor = if hud_up { band.ceil_y } else { CAPTION_FLOOR_Y };
    (scale, baseline, floor)
}

fn draw_caption_block(lines: &[String], lay: &CaptionLayout, model: &HudModel) {
    let cx = SCR_W * 0.5;
    let ink = subtitle_ink(model); // white unless the viewer picked a dimmer tone (track menu)
    let outline = theme::scrim_black(0.85);
    let p = Painter::root();
    for (i, ln) in lines.iter().enumerate() {
        let top = lay.line_top(i);
        if let Ok(cs) = CString::new(ln.as_str()) {
            // dark outline (4 offsets) then the bold caption in its tone — legible over any scene
            for (dx, dy) in [(-2.0f32, 0.0f32), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0)] {
                p.text(cs.as_ptr(), cx + dx, top + CAPTION_TEXT_INSET + dy, lay.face, outline, 1, 1);
            }
            p.text(cs.as_ptr(), cx, top + CAPTION_TEXT_INSET, lay.face, ink, 1, 1);
        }
    }
}

/// The caption's lines: each newline-separated segment word-wrapped to [`wrap_width`],
/// measured by `measure` in the bold face it is drawn in. Every wrapped line is kept, so a long cue
/// is never cut short; [`caption_layout`] fits the block.
fn caption_wrap(text: &str, scale: f32, measure: &dyn Measure) -> Vec<String> {
    let face = (CAPTION_FACE * scale).round() as c_int;
    let measure = |s: &str| measure.width_str(s, face, true);
    let width = wrap_width(scale);
    let mut lines: Vec<String> = Vec::new();
    for seg in text.split('\n') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        lines.extend(wrap_px(seg, width, &measure));
    }
    lines
}

/// Greedy word-wrap of `s` to `width` px as `measure` reports them. A word wider than `width` on
/// its own keeps a line to itself rather than being cut.
fn wrap_px(s: &str, width: f32, measure: &impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && measure(&format!("{cur} {word}")) > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Where one caption block is drawn: `face` is the text size in px, `pitch` the line pitch, `top`
/// the y of the first line's box, `lines` how many lines the block holds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CaptionLayout {
    face: i32,
    pitch: f32,
    top: f32,
    lines: usize,
}

impl CaptionLayout {
    fn line_top(&self, i: usize) -> f32 {
        self.top + i as f32 * self.pitch
    }
}

/// The steps a caption block too tall for its area is stepped down through, as factors of the
/// viewer's own size. The first step is that size, so a block that fits is drawn as it always was.
const CAPTION_FIT_STEPS: [f32; 4] = [1.0, 0.875, 0.75, 0.625];

/// Place one caption block of `lines` lines at the viewer's `scale`, its bottom on `baseline` for a
/// block of up to [`CAPTION_ANCHOR_LINES`] lines. A taller block keeps that anchor's top and grows
/// DOWN, so it moves away from the Timing capsule rather than up into it. If the block would pass
/// `floor`, the size steps down through [`CAPTION_FIT_STEPS`] until it fits; at the smallest step it
/// is returned anyway (a failure message is short, so this is a bound, not a path).
fn caption_layout(lines: usize, scale: f32, baseline: f32, floor: f32) -> CaptionLayout {
    let anchored = lines.min(CAPTION_ANCHOR_LINES) as f32;
    let top = baseline - anchored * CAPTION_LINE_PITCH * scale;
    let mut lay = CaptionLayout { face: 0, pitch: 0.0, top, lines };
    for &step in CAPTION_FIT_STEPS.iter() {
        let s = scale * step;
        lay = CaptionLayout { face: (CAPTION_FACE * s).round() as i32, pitch: CAPTION_LINE_PITCH * s, top, lines };
        if top + lines as f32 * lay.pitch <= floor {
            break;
        }
    }
    lay
}

/// **The highest `controls_top_y` a HUD may report**: the top at which a caption block of
/// [`CAPTION_ANCHOR_LINES`] lines at the largest `SubtitleSize` and the highest `SubtitlePosition`
/// still fits between [`SAFE`]'s top and the baseline [`caption_geometry_for`] lifts it to. A band
/// whose row top is above it would push that block off the safe area. Computed from the shared
/// `size_scale`, `position_lift` and `caption_layout`, so it follows them, and rounded UP to a whole
/// pixel so the bound is never optimistic. A plain fn on purpose: a lazy static would be a new
/// global. Not a `MAX_*` constant, so `ci/list-caps.ini` has nothing to classify.
pub fn min_controls_top_y() -> f32 {
    use plx_plex::plex::session::{SubtitlePosition, SubtitleSize};
    let scale = SubtitleSize::LADDER.iter().map(|&s| size_scale(s)).fold(0.0, f32::max);
    let raise = SubtitlePosition::LADDER.iter().map(|&p| position_lift(p)).fold(0.0, f32::max);
    // A block whose bottom is on y = 0 has its top at minus its height.
    let height = -caption_layout(CAPTION_ANCHOR_LINES, scale, 0.0, f32::MAX).top;
    (SAFE.y + height + raise + CAPTION_INK_CLEARANCE).ceil()
}

/// The ink both subtitle draws use this frame: the viewer's tone ([`HudModel::sub_tone`])
/// resolved on [`theme::SUBTITLE_INKS`]. A rung the table does not have is white, never a
/// neighbour.
pub fn subtitle_ink(model: &HudModel) -> [f32; 4] {
    subtitle_ink_for(model.sub_tone)
}

fn subtitle_ink_for(tone: plx_plex::plex::session::SubtitleTone) -> [f32; 4] {
    theme::SUBTITLE_INKS
        .get(tone.index() as usize)
        .copied()
        .unwrap_or(theme::SUBTITLE_INKS[0])
}

/// Map a decoded image-subtitle rect from the stream's `cw`×`ch` authoring canvas onto the video
/// rect — which is always the full panel here (the video track is authored 1920×1080; see the
/// `docs/agent-reference.md`). A subtitle canvas is the picture's own storage grid, so this is exactly the
/// stretch the TV applies to the video itself: identity for 1080p PGS, 2.67×/2.25 for a 720×480
/// NTSC VobSub, 0.5× for a 4K PGS track. `cw`/`ch` of 0 means the decoder never declared a canvas
/// — then the rect is used 1:1, which is what this path did unconditionally before.
///
/// Deliberately NOT snapped to whole pixels: this is scaled content, and the crispness contract
/// (`docs/agent-reference.md`) snaps 1:1-texel content only.
fn sub_screen_rect(r: (i32, i32, i32, i32), cw: i32, ch: i32) -> Rect {
    let sx = if cw > 0 { SCR_W / cw as f32 } else { 1.0 };
    let sy = if ch > 0 { SCR_H / ch as f32 } else { 1.0 };
    Rect::new(
        r.0 as f32 * sx,
        r.1 as f32 * sy,
        r.2 as f32 * sx,
        r.3 as f32 * sy,
    )
}

/// How far UP to shift the rects of a display set that overhang the transport, so they clear it.
/// Nothing moves while the HUD is down, and the shift never exceeds the overhanging group's own
/// top, so a set too tall to lift is clamped rather than pushed off the top of the screen.
///
/// The lift is measured over the OVERHANGING rects only, and (per [`overhangs`]) applies only to
/// them. Measuring the whole set breaks the case multi-rect adds: a set carrying a sign at
/// y≈100 and dialogue at y≈950 would clamp the lift to 100 — sliding the sign for no reason
/// while leaving the dialogue still behind the scrim. Measuring the group and moving it as ONE
/// unit is also why this is not a per-rect lift: two-line dialogue is two rects with different
/// overhangs, and lifting each by its own would collapse them onto each other.
fn hud_lift<I: Iterator<Item = Rect>>(rects: I, hud_up: bool, band: HudBand) -> f32 {
    if !hud_up {
        return 0.0;
    }
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for r in rects.filter(|r| overhangs(*r, band)) {
        top = top.min(r.y);
        bottom = bottom.max(r.y + r.h);
    }
    if top > bottom {
        return 0.0; // nothing overhangs
    }
    (bottom - band.ceil_y).max(0.0).min(top.max(0.0))
}

/// Does this rect reach below the band's ceiling? Only such rects take the lift.
fn overhangs(r: Rect, band: HudBand) -> bool {
    r.y + r.h > band.ceil_y
}

/// Client-rendered IMAGE subtitles (PGS/VobSub): composite the active decoded display set over
/// the video, scaled from the subtitle stream's own authoring canvas into the video rect
/// (`sub_screen_rect`) and lifted clear of the HUD's `band` when `hud_up` (`hud_lift`). EVERY rect
/// of the set is drawn — two-line dialogue and sign-plus-dialogue are authored as separate rects.
/// Caches one GL texture per rect and re-uploads only when the active cue changes (every few
/// seconds); the cached rects are the unlifted screen rects, so the lift can follow the HUD
/// frame by frame without re-uploading. Main-thread only (GL). This is the image counterpart to
/// the text path (libass) — a selected track is either text or image, so at most one of the two draws a
/// cue at a time.
pub fn draw_subtitle_bitmap(cache: &mut SubtitleBitmaps, hud_up: bool, band: HudBand, model: &HudModel) {
    let set = &mut cache.set;
    let sel = model.sub_sel;
    if sel < 0 {
        for (t, _) in set.drain(..) {
            delete_tex(t);
        }
        cache.bytes = 0;
        cache.key = i64::MIN;
        cache.sel = i32::MIN; // reset BOTH halves of the key — neither should prop the other up
        return;
    }
    match model.bitmap_key {
        None => cache.key = i64::MIN, // gap between cues — draw nothing this frame
        Some(k) => {
            if k != cache.key || sel != cache.sel {
                let (cw, ch, rects) = match (model.bitmap_by_key)(k) {
                    Some(v) => v,
                    None => return, // cue evicted between key lookup and fetch
                };
                // retire the surplus first, then re-spec the ids we keep: upload_rgba reuses
                // a non-zero id, so a steady 1-rect stream never allocates a texture twice.
                for (t, _) in set.drain(rects.len().min(set.len())..) {
                    delete_tex(t);
                }
                for (i, r) in rects.iter().enumerate() {
                    let dst = sub_screen_rect((r.x, r.y, r.w, r.h), cw, ch);
                    let prev = set.get(i).map_or(0, |(t, _)| *t);
                    // the store holds the set indexed; expand it once, here, per cue change
                    let rgba = r.to_rgba();
                    let tex = upload_rgba(prev, r.w, r.h, rgba.as_ptr());
                    match set.get_mut(i) {
                        Some(slot) => *slot = (tex, dst),
                        None => set.push((tex, dst)),
                    }
                }
                // The whole set is re-uploaded above, so its residency is the new set's, not a
                // running total: `set.drain(rects.len()..)` already retired any surplus and every
                // surviving id was re-spec'd to a rect of THIS display set.
                cache.bytes = rects
                    .iter()
                    .map(|r| r.w.max(0) as usize * r.h.max(0) as usize * 4)
                    .sum();
                cache.key = k;
                cache.sel = sel;
            }
            let lift = hud_lift(set.iter().map(|(_, r)| *r), hud_up, band);
            // the tone TINTS a bitmap: the image shader multiplies by it, so a PGS cue keeps its
            // authored colours and outline and only gives up light (white is the identity)
            let tint = subtitle_ink(model);
            let p = Painter::root();
            for (tex, r) in set.iter() {
                let dy = if overhangs(*r, band) { lift } else { 0.0 };
                p.tex(*tex, Rect::new(r.x, r.y - dy, r.w, r.h), 0.0, tint);
            }
        }
    }
}

/// **The image-subtitle display set on screen, and the two halves of its cache key** — a RENDER
/// resource of the player's instance (`PlayerScreen::render`), not module state.
///
/// It was three `static mut`s inside [`draw_subtitle_bitmap`] until restructure phase 9. The key is
/// `(track, start_ns)` and not `start_ns` alone: two image tracks of the same file routinely start
/// a display set on the SAME pts, so keying on the timestamp alone leaves the outgoing track's
/// bitmap on screen after a switch between two image tracks.
///
/// The GL names in `set` are OWNED: [`Self::release`] deletes them, and the player's instance calls
/// it when the screen is unmounted — a static could never be told that a playback had ended.
pub struct SubtitleBitmaps {
    set: Vec<(c_uint, Rect)>,
    /// The SOURCE pixels behind `set`, in bytes — `sum(w * h * 4)` over the display set's rects as
    /// they were uploaded, which the screen rects in `set` cannot answer (those are where each
    /// bitmap LANDS, scaled into the video rect, not how large the texture is).
    ///
    /// It exists so `PlayerScreen` can state what it holds of the frame's render residency
    /// (`Screen::render_report`, §8.3 rule (c)) instead of the frame plan guessing. It moves with
    /// the textures in every path that uploads, retires or releases them.
    bytes: usize,
    key: i64,
    sel: i32,
}

impl Default for SubtitleBitmaps {
    fn default() -> Self {
        Self::new()
    }
}

impl SubtitleBitmaps {
    pub const fn new() -> Self {
        Self {
            set: Vec::new(),
            bytes: 0,
            key: i64::MIN,
            sel: i32::MIN,
        }
    }
    /// What this display set holds of the frame's render residency: one texture per rect of the
    /// set, and the pixels behind them.
    pub fn render_report(&self) -> plx_ui::frame::RenderReport {
        plx_ui::frame::RenderReport {
            textures: self.set.len() as u32,
            bytes: self.bytes,
        }
    }
    /// A display set of the given SOURCE pixel sizes, without GL — the seam the residency tests
    /// use, since a host test uploads nothing (`gfx::delete_tex` no-ops under `cfg(test)`, so the
    /// stand-in ids are safe to release). The ids are stand-ins; only the count and the bytes are
    /// what anything reads off this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn stub(sizes: &[(i32, i32)]) -> Self {
        Self {
            set: sizes
                .iter()
                .enumerate()
                .map(|(i, _)| (i as c_uint + 1, Rect::new(0.0, 0.0, 0.0, 0.0)))
                .collect(),
            bytes: sizes.iter().map(|(w, h)| *w as usize * *h as usize * 4).sum(),
            key: 1,
            sel: 0,
        }
    }
    /// Retire every uploaded texture. Main thread only (GL), like every other call in this module.
    pub fn release(&mut self) {
        for (t, _) in self.set.drain(..) {
            delete_tex(t);
        }
        self.bytes = 0;
        self.key = i64::MIN;
        self.sel = i32::MIN;
    }
}

/// **The geometry the audits grade**: the embedded `builtin:classic`, compiled, in its compact Discs state
/// (the same report the player page places from). The audit has no measurer of its own and none of these
/// facts needs one, so the compile runs on the shipped advances; it is the same package and the same
/// numbers the (since deleted) hand-written layout reported (`compiled_classic_matches_the_hand_written_layout`, against
/// the frozen golden).
#[cfg(any(test, feature = "test-support"))]
pub fn audit_geometry() -> geometry::HudGeometry {
    let hud = loaded::load_classic("audit", plx_platform::i18n::english(), &plx_base::fontcov::advances::ShippedMeasure);
    geometry::geometry(&hud.compiled, ControlSlot::Discs(Controls::NONE), false, false)
}

/// **The transport's outermost drawn chrome, for the overscan audit**
/// ([`plx_ui::consts::SAFE`]). Private geometry, so the rects are built where they are drawn
/// rather than restated in the module that grades them.
///
/// The bottom tab row is measured at its widest — `Info` + `Chapters`, the pair an item with
/// chapters draws — because a pill's width is the label's and the audit has to grade the widest
/// state a screen can be in, not the one a screenshot happened to catch. That measurement needs a
/// font, which the host suite cannot link, so it is bounded by the row's OWN height instead: what
/// the audit is about here is the row's left edge and its BOTTOM, and both are font-independent.
#[cfg(any(test, feature = "test-support"))]
pub fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let g = audit_geometry();
    out.push(("player scrubber", g.playbar));
    out.push(("player control row", g.row));
    out.push(("player bottom tab row", g.tab_row));
    out.push(("player title / elapsed clock", g.title));
}

#[cfg(test)]
mod tests;
