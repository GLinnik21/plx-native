//! **A title on the household's Seerr server** (docs/seerr.md): the page a Search Request card
//! opens (`ContentArg::SeerrTitle { kind, tmdb }`), and the one place a request is made.
//!
//! It reads the Seerr store's view (`SearchLike::seerr`) and asks it through `StoreCmd::Seerr`:
//! the title's detail when it mounts, the request when the pill is pressed, the detail once more
//! when a request went through (so the status line says what the server now says). A movie's pill
//! requests it; a show's requests every season the server neither holds nor was asked for. For a
//! user Seerr lets choose where a request goes, the pill opens the request menu
//! (`screens::seerr_request`) instead, which sends on the same slot. A title with nothing left to
//! request shows its status and no pill.
//!
//! The hero is laid out the way the catalog page's is (`detail_layout::TITLE_BOTTOM` and the
//! `theme::space` rungs, the backdrop through a household server's photo proxy, the shared scrim).
use std::ffi::CString;

use plx_appkit::hero_layout;
use plx_data::seerr::{Availability, MediaKind, SeerrData, SeerrError, SeerrOp, TitleDetail};
use plx_data::stores::seerr::{offer, Answer, Offer, SeerrCmd, SeerrStore, Slot};
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, Measure,
    Tick,
};
use plx_machine::present::Provenance;
use plx_platform::i18n::msg;
use plx_ui::hero_logo::{HeroLogo, LogoRung};
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use plx_ui::hstack::Item;
use plx_ui::vstack::VFlow;
use plx_ui::widgets::{Button, ControlGround, CtlPop, StatusKind, StatusOverlay};
use plx_ui::{detail_layout, theme, Env, Painter, Rect, View};

use crate::registry::{AppFx, ContentPanel, ContentReq, SearchLike};

pub const PILL_ELEM: u32 = 0;
const GROUP: GroupId = GroupId(0);

/// The status line: what the last request said, else what the server says of the title.
pub fn status_line(detail: &TitleDetail, request: Option<&Answer>) -> Option<&'static str> {
    match request {
        Some(Answer::Ready(SeerrData::Requested)) => return Some(msg::seerr_request_done()),
        Some(Answer::Failed(SeerrError::AlreadyRequested)) => return Some(msg::seerr_request_already()),
        Some(Answer::Failed(SeerrError::Forbidden)) => return Some(msg::seerr_request_forbidden()),
        Some(Answer::Failed(_)) => return Some(msg::seerr_request_failed()),
        _ => {}
    }
    match detail.title.status {
        Availability::Pending => Some(msg::seerr_status_pending()),
        Availability::Processing => Some(msg::seerr_status_processing()),
        Availability::PartlyAvailable => Some(msg::seerr_status_partly()),
        Availability::Available => Some(msg::seerr_status_available()),
        Availability::Blocked => Some(msg::seerr_status_blocked()),
        Availability::Unknown => None,
    }
}

fn pill_label(kind: Option<MediaKind>, state: &Offer) -> &'static std::ffi::CStr {
    match (state, kind) {
        (Offer::Sending, _) => msg::seerr_title_requesting_c(),
        (_, Some(MediaKind::Tv)) => msg::seerr_title_request_seasons_c(),
        _ => msg::seerr_title_request_c(),
    }
}

/// The hero's rows, each a named rung under the last one's measured bottom.
struct Rows {
    meta: f32,
    status: Option<f32>,
    summary: f32,
    pill: f32,
}

fn rows(has_status: bool, summary: &str, measure: &dyn Measure) -> Rows {
    let mut flow = VFlow::new(detail_layout::TITLE_BOTTOM, theme::space::SM);
    flow.push(Item::new(0.0));
    let meta = flow.push(Item::new(measure.cap_h(theme::size::BODY))).map_or(0.0, |s| s.y);
    let status = flow.push(if has_status { Item::new(measure.cap_h(theme::size::LABEL)) } else { Item::absent() }).map(|s| s.y);
    let summary_h = if summary.is_empty() {
        0.0
    } else {
        plx_ui::hero_synopsis(summary, "").with_measure(measure).measure_h(detail_layout::HERO_TEXT_W)
    };
    let summary_item = if summary.is_empty() { Item::new(0.0).before(0.0) } else { Item::new(summary_h).before(theme::space::MD) };
    let summary = flow.push(summary_item).map_or(0.0, |s| s.y);
    let pill = flow.push(Item::new(hero_layout::CD).before(theme::space::LG)).map_or(0.0, |s| s.y);
    Rows { meta, status, summary, pill }
}

/// The page of one Seerr title.
pub struct SeerrTitleScreen {
    entry: EntryId,
    kind: MediaKind,
    tmdb: i64,
    /// The request answer the detail was last read again for, so a request that went through
    /// refreshes the detail once.
    refreshed: bool,
    pop: CtlPop<1>,
}

impl SeerrTitleScreen {
    /// The heartbeat word, byte-identical to the route word (`app/words.rs`).
    pub const WORD: &'static str = "seerr_title";

    pub const SHAPE: &'static str = "SeerrTitleScreen{entry:u32,tv:bool,tmdb:u64,refreshed:bool}";

    pub fn new(entry: EntryId, kind: MediaKind, tmdb: i64) -> Self {
        Self { entry, kind, tmdb, refreshed: false, pop: CtlPop::new() }
    }

    fn detail_slot(&self) -> Slot {
        Slot::Detail(self.kind, self.tmdb)
    }

    fn request_slot(&self) -> Slot {
        Slot::Request(self.kind, self.tmdb)
    }

    fn ask<H: SearchLike>(&self, slot: Slot, op: SeerrOp, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))));
    }

    fn ask_detail<H: SearchLike>(&self, fx: &mut Effects<'_, H>) {
        self.ask(self.detail_slot(), SeerrOp::Detail { kind: self.kind, tmdb: self.tmdb }, fx);
    }

    /// The detail this page asked for, once it has landed.
    fn detail<'a>(&self, seerr: Option<&'a SeerrStore>) -> Option<&'a TitleDetail> {
        seerr?.title_detail(self.kind, self.tmdb)
    }

    fn request<'a>(&self, seerr: Option<&'a SeerrStore>) -> Option<&'a Answer> {
        seerr?.answer(&self.request_slot())
    }

    fn pill(&self, seerr: Option<&SeerrStore>) -> Offer {
        self.detail(seerr).map_or(Offer::Nothing, |detail| offer(detail, self.request(seerr)))
    }

    fn rows(&self, seerr: Option<&SeerrStore>, measure: &dyn Measure) -> Option<Rows> {
        let detail = self.detail(seerr)?;
        let summary = detail.title.overview.as_deref().unwrap_or("");
        Some(rows(status_line(detail, self.request(seerr)).is_some(), summary, measure))
    }

    fn pill_rect(&self, seerr: Option<&SeerrStore>, measure: &dyn Measure) -> Option<Rect> {
        let state = self.pill(seerr);
        if state == Offer::Nothing {
            return None;
        }
        let rows = self.rows(seerr, measure)?;
        let w = hero_layout::pill_w(measure, pill_label(Some(self.kind), &state), theme::size::BODY, true, None)
            .max(hero_layout::PW);
        Some(hero_layout::row_rect(&[w], 0, rows.pill))
    }

    fn activate<H: SearchLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if elem != PILL_ELEM {
            return Handled::No;
        }
        let seerr = H::seerr(cx);
        let Offer::Request(seasons) = self.pill(seerr) else { return Handled::Yes };
        if seerr.is_some_and(SeerrStore::may_choose_options) {
            if let Some(rect) = self.pill_rect(seerr, cx.measure) {
                let anchor = [rect.x, rect.y, rect.w, rect.h].map(f32::to_bits);
                fx.push(Fx::App(AppFx::Content(ContentReq::Panel(ContentPanel::SeerrRequest {
                    kind: self.kind, tmdb: self.tmdb, anchor,
                }))));
            }
        } else {
            let op = SeerrOp::Request { kind: self.kind, tmdb: self.tmdb, seasons, options: Default::default() };
            self.ask(self.request_slot(), op, fx);
            fx.invalidate(Provenance::Input);
        }
        Handled::Yes
    }

    /// A request that went through is read back once: the detail is asked again. A request on its
    /// way (from the pill or the request menu) owes a fresh read-back.
    fn follow_request<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let request = self.request(H::seerr(cx));
        if matches!(request, Some(Answer::Loading)) {
            self.refreshed = false;
        }
        let done = matches!(request, Some(Answer::Ready(SeerrData::Requested)));
        if done && !self.refreshed {
            self.refreshed = true;
            self.ask_detail(fx);
        }
    }

    /// A household server whose photo proxy fetches TMDB art (the first household source Search
    /// reads); `None` draws the neutral ground.
    fn art_server<H: SearchLike>(cx: &Cx<'_, H>) -> Option<u16> {
        H::search(cx).scope().sources().iter().find(|source| source.household).map(|source| source.sid.raw())
    }

    fn draw_ground<H: SearchLike>(&self, f: &DrawFrame<'_, '_, H>, p: Painter) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let backdrop = self.detail(H::seerr(f.cx)).and_then(|d| d.title.backdrop.as_deref())
            .and_then(|path| plx_plex::plex::seerr::tmdb_image(path, "w1280"));
        if let (Some(url), Some(server)) = (backdrop, Self::art_server(f.cx)) {
            let (tex, w, h) = plx_ui::widgets::resolve_tex_wh_on(server, &url, plx_ui::consts::SCR_W as i32,
                plx_ui::consts::SCR_H as i32, 0);
            if tex != 0 {
                p.tex_uv(tex, plx_gfx::gfx::UV_FULL, Rect::FULL.cover(w, h), 0.0, theme::TINT_WHITE);
            }
        }
        plx_ui::widgets::hero_scrim(p, 1.0, false);
    }

    fn draw_title<H: SearchLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, detail: &TitleDetail) {
        let seerr = H::seerr(f.cx);
        let measure = f.measure;
        let Some(rows) = self.rows(seerr, measure) else { return };
        let x = plx_ui::consts::MARGIN_X;
        let band_h = plx_ui::hero_logo::band_h(LogoRung::Hero);
        HeroLogo::new(0, "", &detail.title.title, LogoRung::Hero)
            .draw(p, Rect::new(x, detail_layout::TITLE_BOTTOM - band_h, detail_layout::HERO_TEXT_W, band_h), measure);

        let mut meta = vec![match self.kind {
            MediaKind::Movie => msg::browse_kind_movie().to_string(),
            MediaKind::Tv => msg::browse_kind_tv_show().to_string(),
        }];
        meta.extend(detail.title.year.filter(|&y| y > 0).map(|y| y.to_string()));
        let runs: Vec<&str> = meta.iter().map(String::as_str).collect();
        let (cap_top, _) = plx_gfx::text::text_cap_band(theme::size::BODY, 0);
        plx_ui::widgets::dotted_run(p, &runs, x, rows.meta - cap_top, theme::size::BODY, theme::TEXT_SECONDARY, theme::space::SM);

        if let (Some(y), Some(text)) = (rows.status, status_line(detail, self.request(seerr))) {
            if let Ok(text) = CString::new(text) {
                Label::new(text.as_ptr(), theme::size::LABEL, theme::TEXT_PRIMARY).bold().v(VAlign::CapTop).h(HAlign::Left)
                    .draw(p, Rect::new(x, y, detail_layout::HERO_TEXT_W, measure.cap_h(theme::size::LABEL)));
            }
        }
        if let Some(summary) = detail.title.overview.as_deref().filter(|s| !s.is_empty()) {
            plx_ui::hero_synopsis(summary, "").with_measure(measure)
                .draw(p, Rect::new(x, rows.summary, detail_layout::HERO_TEXT_W, 0.0));
        }

        let state = self.pill(seerr);
        let Some(rect) = self.pill_rect(seerr, measure) else { return };
        let focused = f.cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == PILL_ELEM);
        Button::new(pill_label(Some(self.kind), &state).as_ptr(), theme::size::BODY, rect)
            .focused(focused)
            .ground(ControlGround::Keyed)
            .scale(self.pop.scale(0))
            .draw(&Env::inert(), p);
        f.stop(p, Stop { key: self.key(), rect, rest_rect: rect, clip: Rect::FULL, hover: Hover::Focus, activate: Activate::Press });
    }

    fn draw_verdict(&self, p: Painter, text: &str) {
        let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
        let Ok(text) = CString::new(text) else { return };
        let band = Rect::new(0.0, StatusOverlay::FULL_ANCHOR_TOP, plx_ui::consts::SCR_W, sz as f32);
        let label = Label::new(text.as_ptr(), sz, ink).h(HAlign::Center).v(VAlign::CapTop);
        if bold { label.bold() } else { label }.draw(p, band);
    }

    fn key(&self) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: PILL_ELEM }
    }

    fn tick(&mut self, t: Tick, focused: bool) {
        self.pop.step(focused.then_some(0), t.dt());
    }
}

impl LogicalState for SeerrTitleScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).bool(self.kind == MediaKind::Tv).u64(self.tmdb as u64).bool(self.refreshed);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("seerr_title tv={} refreshed={}", self.kind == MediaKind::Tv, self.refreshed));
    }
}

impl<H: SearchLike> Machine<H> for SeerrTitleScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.ask_detail(fx);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                let focused = cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == PILL_ELEM);
                self.tick(*t, focused && self.pill(H::seerr(cx)) != Offer::Nothing);
                self.follow_request(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, cx, fx),
            ScreenEvent::PressCommit(_) => match cx.focus.current.filter(|k| k.entry == self.entry) {
                Some(key) => self.activate(key.elem, cx, fx),
                None => Handled::No,
            },
            ScreenEvent::Input(input) => match input.kind {
                InputKind::Key { key: Key::Back, edge: Edge::Down, .. } => {
                    fx.push(Fx::App(AppFx::Content(ContentReq::Back)));
                    Handled::Yes
                }
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

impl<H: SearchLike> Focusable<H> for SeerrTitleScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.pill(H::seerr(cx)) == Offer::Nothing {
            return;
        }
        out.push(GroupSpec { id: GROUP, kind: GroupKind::Row { wrap: false }, seat: Seat::First, reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4], extent: Rect::FULL, len: 1, elem: ElemKind::Control });
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        (*elem == PILL_ELEM && self.pill(H::seerr(cx)) != Offer::Nothing).then_some(GROUP)
    }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, H>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        if *elem != PILL_ELEM {
            return None;
        }
        let rect = self.pill_rect(H::seerr(cx), cx.measure)?;
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.pill(H::seerr(cx)) != Offer::Nothing { self.key() } else { want }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        self.key()
    }
}

impl<H: SearchLike> Screen<H> for SeerrTitleScreen {
    fn name(&self) -> &'static str {
        Self::WORD
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _: &mut plx_ui::frame::Budget, _: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        self.draw_ground(f, p);
        let seerr = H::seerr(f.cx);
        match seerr.and_then(|s| s.answer(&self.detail_slot())) {
            Some(Answer::Ready(SeerrData::Detail(detail))) => {
                let detail = detail.clone();
                self.draw_title(f, p, &detail);
            }
            Some(Answer::Failed(_)) => self.draw_verdict(p, msg::seerr_title_unreachable()),
            _ => {}
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "seerr_title_tests.rs"]
mod tests;
