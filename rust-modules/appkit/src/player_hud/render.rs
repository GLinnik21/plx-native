//! **The renderer of a compiled HUD** (`docs/hud-plugins.md`, "Architecture"): it draws the compact
//! state of a [`Compiled`] package through the shared widgets, computing nothing a frame could have
//! been told. `builtin:classic` is the only package that ships, and it draws here exactly as the
//! hand-written [`classic::draw_hud`] does (the census test in this module's tests is the proof on the
//! host; the simulator's frame dump is the proof of pixels).
//!
//! **What it walks.** The state [`Compiled::lookup`] answers for the frame (`open == 0`: the sections
//! draw their own panels in this release), then its [`Span`]s in draw order. A `Fades` span draws on the
//! scrub's chrome alpha and is skipped when that alpha is spent; a `Steady` one draws at full alpha. In
//! a Skip state it walks [`CompiledState::skip_variant`], because the two labels have two widths. While
//! a panel owns the middle (`!transport`) only the scrim and the section tabs draw.
//!
//! **Motion stays here.** The focus springs, the title marquee, the scrub preview and the knob's spring
//! are the renderer's, read from the same [`HudMotion`] the hand-written draw reads.
//!
//! **What it does not draw yet** (each is a decision, not an omission): an op whose widget no classic
//! control selects (`bare` / `ring` faces, package icons, `hairline`), a strip, a facts row and an
//! image. Those belong to the reference's compact state and to PR8's sections; [`draw`] draws nothing for
//! them, so a package that needs them shows less rather than something wrong.
//!
//! **The kicker is the one place it overrides the compile.** The compile folds `title.is_episode` into
//! the state ([`StateKey::episode`] is the ROW's pair boolean, because that is what classic's golden
//! equates it with), so a real episode with no queue neighbours compiles in the movie state. The kicker's
//! face is therefore chosen from `now_playing().is_real_episode`, as the hand-written draw does, and only
//! the rects are taken from the ops.

use std::ffi::CString;

use plx_ui::theme;
use plx_ui::widgets::{ControlGround, TabPill, TransportButton};
use plx_ui::{Painter, Rect, View};

use super::classic::{
    draw_player_title_at, draw_playbar_look, draw_scrim_at, draw_scrub_preview, hud_env, hud_tab_state, tab_labels,
    tab_of_section, Kicker, Playbar, PlaybarLook, TitleLook,
};
use super::geometry::GROUP_ROW;
use super::model::HudModel;
use super::motion::HudMotion;
use super::state::{readout_owns_frame, Busy, ControlSlot};
use plx_machine::machine::GroupId;
use crate::hud_format::compile::{Chrome, Compiled, CompiledState, DrawOp, SkipKind, Span, Widget};
use crate::hud_format::reach::Slot;
use crate::skip_pill;
use crate::up_next::{self, Countdown, Decor, Layout};

/// Everything besides the compiled package, the motion and the painter that one frame of the
/// transport draws from: [`classic::draw_hud`]'s inputs, bundled.
pub struct Frame<'a> {
    pub ps: &'a plx_media::route::PlaybackSession,
    pub model: &'a HudModel,
    pub up: &'a Countdown,
    pub slot: ControlSlot,
    pub busy: Busy,
    pub focus: GroupId,
    pub btn: i32,
    pub tab: i32,
    pub now: u32,
    /// false while an Info card or Chapters strip has taken the middle
    pub transport: bool,
    pub measure: &'a dyn plx_machine::machine::Measure,
    pub meta: plx_data::metadata::MetadataView<'a>,
}

/// The compiled state this frame draws, or `None` when the package has no state for it.
fn state_for<'c>(compiled: &'c Compiled, f: &Frame<'_>) -> Option<&'c CompiledState> {
    let (kind, skip) = match f.slot {
        ControlSlot::Discs(_) => (Slot::Discs, SkipKind::Intro),
        ControlSlot::UpNext(_) => (Slot::UpNext, SkipKind::Intro),
        ControlSlot::Skip(pr) => (
            Slot::Skip,
            if pr.kind == plx_data::metadata::MarkerKind::Credits { SkipKind::Credits } else { SkipKind::Intro },
        ),
    };
    // The ROW's pair boolean (`Controls::pair`), not the kicker's predicate: see the module doc.
    let episode = matches!(f.slot, ControlSlot::Discs(c) if c.pair());
    let chapters = crate::chapters_panel::has_chapters(f.meta);
    compiled.lookup(0, kind, chapters, episode, f.model.paused).map(|s| s.skip_variant(skip))
}

/// Draw the transport from `compiled` into `p`. Returns false, having drawn nothing, when the package
/// has no state for this frame (the caller then draws what it drew before the renderer existed).
pub fn draw(compiled: &Compiled, f: &Frame<'_>, row: &mut HudMotion, p: Painter) -> bool {
    // A FAILURE owns the frame: the read-out is `draw_readout`'s, drawn by the caller AFTER this.
    if readout_owns_frame(f.busy) {
        row.reset_title();
        return true;
    }
    let Some(state) = state_for(compiled, f) else { return false };
    let e = hud_env();

    // While a scrub is live the `fades` bands (the title block, the control row, the tabs) fade away so
    // the card has the room; the bar, the knob and both clocks stay. The same alpha restores them.
    let chrome_a = row.preview.motion.chrome_alpha(f.now);
    let chrome = chrome_a > 0.004;
    let pc = p.alpha(chrome_a);
    if !f.transport || !chrome {
        row.reset_title();
    }

    // the kicker's rect, remembered until the title's op arrives: the pair draws as one (one clip, one
    // marquee key, the paused reveal restarting when either line changes)
    let mut kicker_rect: Option<Rect> = None;
    let mut bar: Option<Playbar> = None;
    let mut run = |span: &Span| {
        if span.chrome == Chrome::Fades && !chrome {
            return;
        }
        let q = if span.chrome == Chrome::Fades { pc } else { p };
        for op in &state.ops[span.ops.clone()] {
            match op {
                DrawOp::Scrim { rect } => draw_scrim_at(q, *rect),
                _ if !f.transport && !is_section_tab(op) => {}
                DrawOp::Text { bind, rect, .. } if bind == "title.kicker" => kicker_rect = Some(*rect),
                DrawOp::Text { bind, rect, .. } if bind == "title.main" => {
                    draw_title(f, row, q, kicker_rect.take(), *rect);
                }
                DrawOp::Control(c) => draw_control(f, row, &e, q, state, c),
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
                DrawOp::Playbar { track, fill, track_ink, clock_size, .. } => {
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
                    };
                    draw_playbar_look(q, live, look, f.measure);
                    bar = Some(live);
                }
                // not drawn by this renderer yet: see the module doc
                DrawOp::Ground { .. }
                | DrawOp::Text { .. }
                | DrawOp::Image { .. }
                | DrawOp::Icon { .. }
                | DrawOp::Rule { .. }
                | DrawOp::Facts { .. }
                | DrawOp::Strip { .. } => {}
            }
        }
        // right after the bar's band, so the still floats over the title block and the row above it
        // and under the tabs below, as the hand-written draw orders them
        if let Some(live) = bar.take() {
            draw_scrub_preview(p, f.ps, f.model, row, live);
        }
    };
    // The stand-in's ops are the LAST span, carrying the id of the band that hosts them; they draw right
    // after that band's own ops, where the hand-written draw puts the row, and so before the bar.
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
    true
}

/// A section tab: the one control that draws while a panel owns the middle.
fn is_section_tab(op: &DrawOp) -> bool {
    matches!(op, DrawOp::Control(c) if c.command == "toggle_section")
}

/// The title pair, in the cell the compile placed it: the kicker's rect and the main line's. The main
/// line's right edge is the bound both lines fit before (`title_right`, golden-pinned).
fn draw_title(f: &Frame<'_>, row: &mut HudMotion, q: Painter, kicker: Option<Rect>, main: Rect) {
    let look = TitleLook {
        x: main.x,
        kicker_y: kicker.map_or(TitleLook::default().kicker_y, |k| k.y),
        title_y: main.y,
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

/// One control, at the rect the compile placed it.
fn draw_control(
    f: &Frame<'_>,
    row: &HudMotion,
    e: &plx_ui::Env,
    q: Painter,
    state: &CompiledState,
    c: &crate::hud_format::compile::ControlRect,
) {
    match c.widget {
        // A disc of the replaced row: its stable declared index (`CompiledState::row_ids`) is the
        // control's identity for the springs and the cursor, and picks the glyph and the enabled bit
        // from the occupant's `Controls`. Only classic's five commands have a glyph here; a package's
        // other commands (and a package icon) wait for the compile to carry the control's face.
        Widget::Disc => {
            let ControlSlot::Discs(controls) = f.slot else { return };
            let Some(i) = state.row_ids.iter().position(|id| id.as_deref() == Some(c.id.as_str())) else { return };
            let Ok(i) = i32::try_from(i) else { return };
            if i >= super::state::BTN_N {
                return;
            }
            TransportButton::new(i, c.rect)
                .icon(controls.icon(i))
                .enabled(controls.enabled(i))
                .focused(f.focus == GROUP_ROW && f.btn == i)
                .ground(ControlGround::Unkeyed)
                .scale(row.scale(i))
                .draw(e, q);
        }
        // A section tab. Its label and its index are the tab strip's (`tab_labels`), so what is
        // drawn, hit and selected are the one list.
        Widget::Pill => {
            let Some(section) = c.section.as_deref() else { return };
            let Some(i) = tab_of_section(section) else { return };
            let labels = tab_labels(crate::chapters_panel::has_chapters(f.meta));
            let Some(Ok(label)) = usize::try_from(i).ok().and_then(|i| labels[i]).map(CString::new) else { return };
            let (focused, selected) = hud_tab_state(f.transport, f.focus, f.tab, i);
            TabPill::new(label.as_ptr(), theme::size::BODY, c.rect)
                .focused(focused)
                .selected(selected)
                .ground(ControlGround::Unkeyed)
                .draw(e, q);
        }
        Widget::Button => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player_hud::classic;
    use crate::player_hud::geometry::{GROUP_SCRUB, GROUP_TABS};
    use crate::player_hud::state::Controls;
    use crate::skip_pill::prompt_for;
    use plx_data::metadata::{Marker, MarkerKind};
    use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
    use plx_ui::draw_census::{capture, capture_icons, capture_inks, capture_runs, Run};
    use plx_ui::fixture::FixtureMeasure;
    use plx_ui::icons::Icon;

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

    /// The hand-written draw and the renderer, over the same frame, each with a fresh motion state.
    fn both(compiled: &Compiled, c: Case) -> (Census, Census, bool) {
        let ps = plx_media::route::PlaybackSession::IDLE;
        let store = plx_data::stores::metadata::MetadataStore::default();
        let model = HudModel { paused: c.paused, duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
        let up = Countdown::default();
        let mut hand_row = HudMotion::new();
        let hand = census(|| {
            classic::draw_hud(&ps, &model, &mut hand_row, &up, c.slot, Busy::None, c.focus, c.btn, c.tab, 1_000, c.transport, &FM, store.view());
        });
        let mut row = HudMotion::new();
        let frame = Frame {
            ps: &ps,
            model: &model,
            up: &up,
            slot: c.slot,
            busy: Busy::None,
            focus: c.focus,
            btn: c.btn,
            tab: c.tab,
            now: 1_000,
            transport: c.transport,
            measure: &FM,
            meta: store.view(),
        };
        let mut drew = false;
        let new = census(|| drew = draw(compiled, &frame, &mut row, Painter::root()));
        (hand, new, drew)
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

    /// **Classic through the renderer is classic through the hand-written draw**: every primitive,
    /// glyph, text run and fill the two hand the painter is the same list, in the same order, for every
    /// slot, focus, pause and panel state, in every shipped language. The state the host cannot reach is
    /// the title text and Up Next's card (they need a playing item): the simulator's frame dump covers
    /// them.
    #[test]
    fn classic_through_the_renderer_draws_what_the_hand_written_hud_draws() {
        let mut compared = 0;
        for language in SHIPPED {
            let _language = language_on_this_thread_for_test(language);
            let cx = plx_platform::i18n::current();
            let hud = crate::player_hud::loaded::load_classic("0.0.0-test", cx, &FM).expect("classic loads");
            for c in cases() {
                let (hand, new, drew) = both(&hud.compiled, c);
                assert!(drew, "{c:?}: the renderer has a state for every classic frame");
                assert!(!hand.0.is_empty(), "{c:?}: the hand-written draw draws something");
                assert_eq!(hand.0, new.0, "{} {c:?}: the primitive census", language.tag());
                assert_eq!(hand.1, new.1, "{} {c:?}: the text runs", language.tag());
                assert_eq!(hand.2, new.2, "{} {c:?}: the glyphs", language.tag());
                assert_eq!(hand.3, new.3, "{} {c:?}: the fills", language.tag());
                compared += 1;
            }
        }
        assert_eq!(compared, SHIPPED.len() * cases().len(), "a count, so a loop that compares less cannot pass");
    }

    /// A failure owns the frame: nothing of the transport draws, and the title's marquee resets.
    #[test]
    fn a_failure_read_out_owns_the_frame_in_the_renderer_too() {
        let hud = crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM).unwrap();
        let ps = plx_media::route::PlaybackSession::IDLE;
        let store = plx_data::stores::metadata::MetadataStore::default();
        let model = HudModel::default();
        let up = Countdown::default();
        let mut row = HudMotion::new();
        let frame = Frame {
            ps: &ps,
            model: &model,
            up: &up,
            slot: ControlSlot::Discs(Controls::NONE),
            busy: Busy::Readout(plx_ui::widgets::StatusKind::Failed, c"x"),
            focus: GROUP_SCRUB,
            btn: 0,
            tab: 0,
            now: 0,
            transport: true,
            measure: &FM,
            meta: store.view(),
        };
        let drawn = census(|| assert!(draw(&hud.compiled, &frame, &mut row, Painter::root())));
        assert!(drawn.0.is_empty(), "{:?}", drawn.0);
    }
}
