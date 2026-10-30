//! **The renderer of a compiled HUD** (`docs/hud-plugins.md`, "Architecture"): it draws the compact
//! state of a [`Compiled`] package through the shared widgets, computing nothing a frame could have
//! been told. `builtin:classic` is the only package that ships, and it draws here exactly as the
//! hand-written `classic::draw_hud` (since deleted) did: the census test in this module's tests holds the
//! renderer to the hashes that draw froze on the host, and the simulator's frame dump is the proof of pixels.
//! A package with no state for the frame draws nothing: [`draw`] returns `false`.
//!
//! **What it walks.** The state [`Compiled::lookup`] answers for the frame (`open == 0`: the sections
//! draw their own panels in this release), then its [`Span`]s in draw order. A `Fades` span draws on the
//! scrub's chrome alpha and is skipped when that alpha is spent; a `Steady` one draws at full alpha. In
//! a Skip state it walks [`CompiledState::skip_variant`], because the two labels have two widths. While
//! a panel owns the middle (`!transport`) only the scrim and the section tabs draw.
//!
//! **Motion stays here.** The focus springs, the title marquee, the scrub preview and the knob's spring
//! are the renderer's, read from the [`HudMotion`] the page owns.
//!
//! **What it does not draw yet** (each is a decision, not an omission): a section's selected face (the
//! `toggle_section` disc while its panel is open: section time, PR8), `rest: custom` (it takes the film),
//! the text binds of the sections and the strip, `hairline`, a strip, a facts row and an image. [`draw`]
//! draws nothing for them, so a package that needs them shows less rather than something wrong. A
//! title's declared `ink` and `bold` are not carried: classic's rule (an episode's kicker is primary and
//! bold, a context line secondary and regular) draws, and only the declared sizes are honoured.
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
use super::geometry::{frame_state, GROUP_ROW};
use super::model::HudModel;
use super::motion::HudMotion;
use super::state::{readout_owns_frame, Busy, ControlSlot};
use plx_machine::machine::GroupId;
use super::command::{HudCommand, SeekBy, TrackTab};
use crate::hud_format::compile::{Chrome, Compiled, CompiledState, DrawOp, FaceRef, Focus, Rest, Span, Widget};
use crate::skip_pill;
use crate::up_next::{self, Countdown, Decor, Layout};

/// Everything besides the compiled package, the motion and the painter that one frame of the
/// transport draws from, bundled.
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

/// The compiled state this frame draws, or `None` when the package has no state for it
/// ([`frame_state`], the lookup every geometry consumer takes too).
fn state_for<'c>(compiled: &'c Compiled, f: &Frame<'_>) -> Option<&'c CompiledState> {
    let chapters = crate::chapters_panel::has_chapters(f.meta);
    frame_state(compiled, f.slot, chapters, f.model.paused).map(|(state, skip)| state.skip_variant(skip))
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
    let mut kicker_rect: Option<(Rect, i32)> = None;
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
                DrawOp::Text { bind, rect, size, .. } => match text_of(bind) {
                    Some(Text::Kicker) => kicker_rect = Some((*rect, *size)),
                    Some(Text::Title) => draw_title(f, row, q, kicker_rect.take(), *rect, *size),
                    None => {}
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
                // not drawn by this renderer yet: see the module doc
                DrawOp::Ground { .. }
                | DrawOp::Image { .. }
                | DrawOp::Icon { .. }
                | DrawOp::Rule { .. }
                | DrawOp::Facts { .. }
                | DrawOp::Strip { .. } => {}
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
    true
}

/// A section tab: the one control that draws while a panel owns the middle.
fn is_section_tab(op: &DrawOp) -> bool {
    matches!(op, DrawOp::Control(c) if c.command == "toggle_section")
}

/// What a `Text` op's bind names. The ONE resolver of binds: an op draws through the arm its bind
/// resolves to, and a bind with none (`info.*`, `item.*`, `label.*`: the sections' and the strip's,
/// PR8's) draws nothing here.
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

    /// The renderer over one frame, with a fresh motion state.
    fn drawn(compiled: &Compiled, c: Case) -> (Census, bool) {
        let ps = plx_media::route::PlaybackSession::IDLE;
        let store = plx_data::stores::metadata::MetadataStore::default();
        let model = HudModel { paused: c.paused, duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
        let up = Countdown::default();
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
                ps: &ps, model: &model, up: &up, slot, busy: Busy::None, focus, btn,
                tab: 0, now: 1_000, transport: true, measure: &FM, meta: store.view(),
            };
            census(|| assert!(draw(compiled, &frame, &mut row, Painter::root()), "the reference has a compact state"))
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
    }
}
