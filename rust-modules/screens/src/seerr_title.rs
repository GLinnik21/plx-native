//! **A title on the household's Seerr server** (docs/seerr.md): the page a Seerr card or a request
//! row opens (`ContentArg::SeerrTitle { kind, tmdb }`), the one place a request is made, and the
//! place a request manager approves or declines one.
//!
//! It reads the Seerr store's view (`SearchLike::seerr`) and asks it through `StoreCmd::Seerr`:
//! the title's detail when it mounts, the request when the pill is pressed, a decision when Approve
//! or Decline is, and the detail once more when a request or a decision went through (so the page
//! says what the server now says). A movie's pill requests it; a show's requests every season the
//! server neither holds nor was asked for. For a user Seerr lets choose where a request goes, the
//! pill opens the request menu (`screens::seerr_request`) instead, which sends on the same slot. A
//! title with nothing left to request has no pill.
//!
//! Under the title it says what Seerr knows ([`Rows`]): TMDB's facts (kind, year, runtime, score,
//! genres, network or studio), the availability, the newest request (its state, who asked and when,
//! who answered), the download while one moves (with the amber band), and a show's seasons as
//! chips, each with its own state. For a user with Manage Requests
//! (`SeerrStore::may_manage_requests`) and a request waiting for approval, Approve and Decline
//! stand beside the pill; they answer the newest waiting request (`Slot::Decide`).
//!
//! The hero is laid out the way the catalog page's is (`detail_layout::TITLE_BOTTOM` and the
//! `theme::space` rungs, the backdrop through a household server's photo proxy, the shared scrim).
use std::ffi::{CStr, CString};

use plx_appkit::hero_layout;
use plx_data::seerr::{
    Availability, Decision, MediaKind, MediaRequest, RequestStatus, SeerrData, SeerrError, SeerrOp, TitleDetail,
};
use plx_data::stores::seerr::{offer, Answer, Offer, SeerrCmd, SeerrStore, Slot};
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, Measure,
    Tick,
};
use plx_machine::present::Provenance;
use plx_platform::i18n::msg;
use plx_ui::hero_logo::{HeroLogo, LogoRung};
use plx_ui::hstack::{Flow, Item};
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use plx_ui::vstack::VFlow;
use plx_ui::widgets::{Button, ControlGround, CtlPop, StatusKind, StatusOverlay};
use plx_ui::{detail_layout, fmt, theme, Env, Painter, Rect, View};

use crate::registry::seerr_cards::{answered_line, asked_line, dotted, download_line, request_chip, season_state};
use crate::registry::{AppFx, ContentPanel, ContentReq, SearchLike};

/// The controls, by element; each is also its pop slot.
pub const PILL_ELEM: u32 = 0;
pub const APPROVE_ELEM: u32 = 1;
pub const DECLINE_ELEM: u32 = 2;
const GROUP: GroupId = GroupId(0);

/// The download band under its line: the card band's height.
const BAND_H: f32 = 5.0;
/// A season chip's line, and the air between two lines of chips.
const CHIP_GAP: f32 = theme::space::XS;

/// The status line: what the last decision or request said, else what the server says of the title.
pub fn status_line(detail: &TitleDetail, request: Option<&Answer>, decision: Option<&Answer>) -> Option<&'static str> {
    match decision {
        Some(Answer::Failed(SeerrError::Forbidden)) => return Some(msg::seerr_decide_forbidden()),
        Some(Answer::Failed(SeerrError::AlreadyRequested)) => return Some(msg::seerr_decide_conflict()),
        Some(Answer::Failed(_)) => return Some(msg::seerr_decide_failed()),
        _ => {}
    }
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

/// TMDB's facts as the meta line says them: kind, year, runtime, score, two genres, network or
/// studio, each left out when the server did not say.
pub fn facts(kind: MediaKind, detail: &TitleDetail) -> Vec<String> {
    let f = &detail.facts;
    let mut out = vec![match kind {
        MediaKind::Movie => msg::browse_kind_movie().to_string(),
        MediaKind::Tv => msg::browse_kind_tv_show().to_string(),
    }];
    out.extend(detail.title.year.filter(|&y| y > 0).map(|y| y.to_string()));
    out.extend(f.runtime_min.map(|m| fmt::duration_minutes_in(plx_platform::i18n::current(), i64::from(m))));
    out.extend(f.rating.map(|r| fmt::rating_score(fmt::RatingScale::Percent, f64::from(r))));
    out.extend(f.genres.iter().take(2).cloned());
    out.extend(f.studio.clone());
    out
}

/// The newest request of the title, and the newest still waiting for approval.
fn newest(detail: &TitleDetail) -> Option<&MediaRequest> {
    detail.requests.first()
}

pub fn pending(detail: &TitleDetail) -> Option<&MediaRequest> {
    detail.requests.iter().find(|r| r.status == RequestStatus::PendingApproval)
}

/// The request line: the newest request's state, who asked and when, who answered.
pub fn request_line(detail: &TitleDetail) -> Option<String> {
    let request = newest(detail)?;
    Some(dotted(&[request_chip(request), &asked_line(request), &answered_line(request)]))
}

/// A show's seasons as their chips say them: "Season 1 · Available".
pub fn season_chips(detail: &TitleDetail) -> Vec<String> {
    detail.seasons.iter().map(|s| dotted(&[&fmt::season_identity(i64::from(s.number), ""), season_state(s.status)])).collect()
}

/// Chips `widths` wide, laid in lines no wider than `w` from `x`: each chip's `(x, line)`.
fn chip_lines(widths: &[f32], x: f32, w: f32) -> Vec<(f32, usize)> {
    let (mut out, mut line, mut flow) = (Vec::new(), 0, Flow::new(x, theme::space::XS));
    for &cw in widths {
        if flow.next_x() > x && flow.next_x() + cw > x + w {
            (line, flow) = (line + 1, Flow::new(x, theme::space::XS));
        }
        out.push((flow.push(Item::new(cw)).map_or(x, |s| s.x), line));
    }
    out
}

/// The hero's rows, each a named rung under the last one's measured bottom.
pub struct Rows {
    meta: f32,
    status: Option<f32>,
    request: Option<f32>,
    download: Option<f32>,
    seasons: Option<f32>,
    summary: f32,
    pill: f32,
}

/// What the rows hold, measured.
struct Content<'a> {
    status: bool,
    request: bool,
    download: bool,
    chip_lines: usize,
    summary: &'a str,
}

fn rows(c: &Content<'_>, measure: &dyn Measure) -> Rows {
    let line = |on: bool, sz| if on { Item::new(measure.cap_h(sz)) } else { Item::absent() };
    let mut flow = VFlow::new(detail_layout::TITLE_BOTTOM, theme::space::SM);
    flow.push(Item::new(0.0));
    let meta = flow.push(Item::new(measure.cap_h(theme::size::BODY))).map_or(0.0, |s| s.y);
    let status = flow.push(line(c.status, theme::size::LABEL)).map(|s| s.y);
    let request = flow.push(line(c.request, theme::size::LABEL)).map(|s| s.y);
    let download = flow.push(if c.download {
        Item::new(measure.cap_h(theme::size::LABEL) + theme::space::XS + BAND_H)
    } else {
        Item::absent()
    }).map(|s| s.y);
    let chip_h = measure.cap_h(theme::size::CAPTION) + 2.0 * 6.0;
    let seasons = flow.push(if c.chip_lines > 0 {
        Item::new(c.chip_lines as f32 * chip_h + (c.chip_lines - 1) as f32 * CHIP_GAP)
    } else {
        Item::absent()
    }).map(|s| s.y);
    let summary_h = if c.summary.is_empty() {
        0.0
    } else {
        plx_ui::hero_synopsis(c.summary, "").with_measure(measure).measure_h(detail_layout::HERO_TEXT_W)
    };
    let summary_item = if c.summary.is_empty() { Item::new(0.0).before(0.0) } else { Item::new(summary_h).before(theme::space::MD) };
    let summary = flow.push(summary_item).map_or(0.0, |s| s.y);
    let pill = flow.push(Item::new(hero_layout::CD).before(theme::space::LG)).map_or(0.0, |s| s.y);
    Rows { meta, status, request, download, seasons, summary, pill }
}

/// The page of one Seerr title.
pub struct SeerrTitleScreen {
    entry: EntryId,
    kind: MediaKind,
    tmdb: i64,
    /// The request answer the detail was last read again for, so a request that went through
    /// refreshes the detail once.
    refreshed: bool,
    /// The request this page last approved or declined, and whether the detail was read again once
    /// that answer landed.
    deciding: Option<i64>,
    decided: bool,
    pop: CtlPop<3>,
}

impl SeerrTitleScreen {
    /// The heartbeat word, byte-identical to the route word (`app/words.rs`).
    pub const WORD: &'static str = "seerr_title";

    pub const SHAPE: &'static str = "SeerrTitleScreen{entry:u32,tv:bool,tmdb:u64,refreshed:bool,deciding:opt<i64>,decided:bool}";

    pub fn new(entry: EntryId, kind: MediaKind, tmdb: i64) -> Self {
        Self { entry, kind, tmdb, refreshed: false, deciding: None, decided: false, pop: CtlPop::new() }
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

    /// The answer to this page's last decision.
    fn decision<'a>(&self, seerr: Option<&'a SeerrStore>) -> Option<&'a Answer> {
        seerr?.answer(&Slot::Decide(self.deciding?))
    }

    fn pill(&self, seerr: Option<&SeerrStore>) -> Offer {
        self.detail(seerr).map_or(Offer::Nothing, |detail| offer(detail, self.request(seerr)))
    }

    /// The request Approve and Decline answer: the newest one waiting, for a user who may decide,
    /// unless this page's decision on it already went through.
    fn decidable<'a>(&self, seerr: Option<&'a SeerrStore>) -> Option<&'a MediaRequest> {
        let store = seerr?;
        if !store.may_manage_requests() {
            return None;
        }
        let request = pending(self.detail(seerr)?)?;
        let done = self.deciding == Some(request.id) && matches!(self.decision(seerr), Some(Answer::Ready(SeerrData::Decided)));
        (!done).then_some(request)
    }

    /// Which decision is on its way, if one is.
    fn sending(&self, seerr: Option<&SeerrStore>) -> Option<Decision> {
        let id = self.deciding?;
        if !matches!(self.decision(seerr), Some(Answer::Loading)) {
            return None;
        }
        match seerr?.asked(&Slot::Decide(id)) {
            Some(SeerrOp::Decide { decision, .. }) => Some(*decision),
            _ => None,
        }
    }

    /// The controls on the row, left to right, with their labels.
    fn controls(&self, seerr: Option<&SeerrStore>) -> Vec<(u32, &'static CStr)> {
        let mut out = Vec::new();
        let state = self.pill(seerr);
        if state != Offer::Nothing {
            let label = match (&state, self.kind) {
                (Offer::Sending, _) => msg::seerr_title_requesting_c(),
                (_, MediaKind::Tv) => msg::seerr_title_request_seasons_c(),
                _ => msg::seerr_title_request_c(),
            };
            out.push((PILL_ELEM, label));
        }
        if self.decidable(seerr).is_some() {
            let sending = self.sending(seerr);
            out.push((APPROVE_ELEM, if sending == Some(Decision::Approve) {
                msg::seerr_decide_approving_c()
            } else {
                msg::seerr_decide_approve_c()
            }));
            out.push((DECLINE_ELEM, if sending == Some(Decision::Decline) {
                msg::seerr_decide_declining_c()
            } else {
                msg::seerr_decide_decline_c()
            }));
        }
        out
    }

    fn rows(&self, seerr: Option<&SeerrStore>, measure: &dyn Measure) -> Option<Rows> {
        let detail = self.detail(seerr)?;
        let chips = season_chips(detail);
        let widths: Vec<f32> = chips.iter().map(|c| plx_ui::widgets::keyline_chip_w(c, measure)).collect();
        let chip_lines = chip_lines(&widths, plx_ui::consts::MARGIN_X, detail_layout::HERO_TEXT_W)
            .last().map_or(0, |(_, line)| line + 1);
        Some(rows(&Content {
            status: status_line(detail, self.request(seerr), self.decision(seerr)).is_some(),
            request: request_line(detail).is_some(),
            download: download_line(&detail.downloads).is_some(),
            chip_lines,
            summary: detail.title.overview.as_deref().unwrap_or(""),
        }, measure))
    }

    /// Each control's rect, in the row's order.
    fn control_rects(&self, seerr: Option<&SeerrStore>, measure: &dyn Measure) -> Vec<(u32, Rect)> {
        let controls = self.controls(seerr);
        let Some(rows) = self.rows(seerr, measure).filter(|_| !controls.is_empty()) else { return Vec::new() };
        let widths: Vec<f32> = controls.iter()
            .map(|(_, label)| hero_layout::pill_w(measure, label, theme::size::BODY, true, None).max(hero_layout::PW))
            .collect();
        controls.iter().enumerate().map(|(i, (elem, _))| (*elem, hero_layout::row_rect(&widths, i, rows.pill))).collect()
    }

    fn control_rect(&self, elem: u32, seerr: Option<&SeerrStore>, measure: &dyn Measure) -> Option<Rect> {
        self.control_rects(seerr, measure).into_iter().find(|(e, _)| *e == elem).map(|(_, r)| r)
    }

    fn activate<H: SearchLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let seerr = H::seerr(cx);
        match elem {
            PILL_ELEM => {
                let Offer::Request(seasons) = self.pill(seerr) else { return Handled::Yes };
                if seerr.is_some_and(SeerrStore::may_choose_options) {
                    if let Some(rect) = self.control_rect(PILL_ELEM, seerr, cx.measure) {
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
            }
            APPROVE_ELEM | DECLINE_ELEM => {
                let Some(id) = self.decidable(seerr).map(|r| r.id) else { return Handled::Yes };
                if self.sending(seerr).is_some() {
                    return Handled::Yes;
                }
                let decision = if elem == APPROVE_ELEM { Decision::Approve } else { Decision::Decline };
                (self.deciding, self.decided) = (Some(id), false);
                self.ask(Slot::Decide(id), SeerrOp::Decide { id, decision }, fx);
                fx.invalidate(Provenance::Input);
            }
            _ => return Handled::No,
        }
        Handled::Yes
    }

    /// A request or a decision that went through is read back once: the detail is asked again. A
    /// request on its way (from the pill or the request menu) owes a fresh read-back. A decision
    /// the server says was already made (409) is read back too: somebody else answered first.
    fn follow<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let seerr = H::seerr(cx);
        let request = self.request(seerr);
        if matches!(request, Some(Answer::Loading)) {
            self.refreshed = false;
        }
        if matches!(request, Some(Answer::Ready(SeerrData::Requested))) && !self.refreshed {
            self.refreshed = true;
            self.ask_detail(fx);
        }
        let landed = matches!(self.decision(seerr),
            Some(Answer::Ready(SeerrData::Decided) | Answer::Failed(SeerrError::AlreadyRequested)));
        if landed && !self.decided {
            self.decided = true;
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

    fn line(p: Painter, text: &str, y: f32, bold: bool, ink: [f32; 4], measure: &dyn Measure) {
        let w = detail_layout::HERO_TEXT_W;
        let fitted = plx_machine::machine::elide_by(text, w, false, |s| measure.width_str(s, theme::size::LABEL, bold));
        let Ok(text) = CString::new(fitted) else { return };
        let label = Label::new(text.as_ptr(), theme::size::LABEL, ink).v(VAlign::CapTop).h(HAlign::Left);
        if bold { label.bold() } else { label }
            .draw(p, Rect::new(plx_ui::consts::MARGIN_X, y, w, measure.cap_h(theme::size::LABEL)));
    }

    fn draw_title<H: SearchLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, detail: &TitleDetail) {
        let seerr = H::seerr(f.cx);
        let measure = f.measure;
        let Some(rows) = self.rows(seerr, measure) else { return };
        let x = plx_ui::consts::MARGIN_X;
        let band_h = plx_ui::hero_logo::band_h(LogoRung::Hero);
        HeroLogo::new(0, "", &detail.title.title, LogoRung::Hero)
            .draw(p, Rect::new(x, detail_layout::TITLE_BOTTOM - band_h, detail_layout::HERO_TEXT_W, band_h), measure);

        // Whole facts drop off the end until the line fits the hero's text column: a genre or a
        // studio is the server's word and may be long, and half a fact says nothing.
        let meta = facts(self.kind, detail);
        let mut runs: Vec<&str> = meta.iter().map(String::as_str).collect();
        let run = plx_ui::widgets::DottedRun::new(theme::size::BODY, theme::TEXT_SECONDARY, theme::space::SM).measured(measure);
        while runs.len() > 1 && run.width(measure, &runs) > detail_layout::HERO_TEXT_W {
            runs.pop();
        }
        let (cap_top, _) = plx_gfx::text::text_cap_band(theme::size::BODY, 0);
        run.draw(p, &runs, x, rows.meta - cap_top);

        if let (Some(y), Some(text)) = (rows.status, status_line(detail, self.request(seerr), self.decision(seerr))) {
            Self::line(p, text, y, true, theme::TEXT_PRIMARY, measure);
        }
        if let (Some(y), Some(text)) = (rows.request, request_line(detail)) {
            Self::line(p, &text, y, false, theme::TEXT_SECONDARY, measure);
        }
        if let (Some(y), Some(text), Some(progress)) =
            (rows.download, download_line(&detail.downloads), plx_data::seerr::progress(&detail.downloads))
        {
            Self::line(p, &text, y, false, theme::TEXT_SECONDARY, measure);
            let top = y + measure.cap_h(theme::size::LABEL) + theme::space::XS;
            plx_ui::widgets::progress_bar(p, Rect::new(x, top, detail_layout::HERO_TEXT_W, BAND_H), BAND_H * 0.5, BAND_H,
                progress.fraction);
        }
        if let Some(y) = rows.seasons {
            let chips = season_chips(detail);
            let widths: Vec<f32> = chips.iter().map(|c| plx_ui::widgets::keyline_chip_w(c, measure)).collect();
            let chip_h = measure.cap_h(theme::size::CAPTION) + 2.0 * 6.0;
            for (chip, (cx, line)) in chips.iter().zip(chip_lines(&widths, x, detail_layout::HERO_TEXT_W)) {
                let cy = y + line as f32 * (chip_h + CHIP_GAP) + chip_h * 0.5;
                plx_ui::widgets::keyline_chip(p, cx, cy, chip, theme::TEXT_SECONDARY, measure);
            }
        }
        if let Some(summary) = detail.title.overview.as_deref().filter(|s| !s.is_empty()) {
            plx_ui::hero_synopsis(summary, "").with_measure(measure)
                .draw(p, Rect::new(x, rows.summary, detail_layout::HERO_TEXT_W, 0.0));
        }

        let labels = self.controls(seerr);
        for (elem, rect) in self.control_rects(seerr, measure) {
            let Some((_, label)) = labels.iter().find(|(e, _)| *e == elem) else { continue };
            let focused = f.cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == elem);
            Button::new(label.as_ptr(), theme::size::BODY, rect)
                .focused(focused)
                .ground(ControlGround::Keyed)
                .scale(self.pop.scale(elem as usize))
                .draw(&Env::inert(), p);
            let key = FocusKey { entry: self.entry, elem };
            f.stop(p, Stop { key, rect, rest_rect: rect, clip: Rect::FULL, hover: Hover::Focus, activate: Activate::Press });
        }
    }

    fn draw_verdict(&self, p: Painter, text: &str) {
        let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
        let Ok(text) = CString::new(text) else { return };
        let band = Rect::new(0.0, StatusOverlay::FULL_ANCHOR_TOP, plx_ui::consts::SCR_W, sz as f32);
        let label = Label::new(text.as_ptr(), sz, ink).h(HAlign::Center).v(VAlign::CapTop);
        if bold { label.bold() } else { label }.draw(p, band);
    }

    fn first_key(&self, seerr: Option<&SeerrStore>) -> Option<FocusKey<u32>> {
        self.controls(seerr).first().map(|(elem, _)| FocusKey { entry: self.entry, elem: *elem })
    }

    fn tick(&mut self, t: Tick, focused: Option<u32>) {
        self.pop.step(focused.map(|elem| elem as usize), t.dt());
    }
}

impl LogicalState for SeerrTitleScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).bool(self.kind == MediaKind::Tv).u64(self.tmdb as u64).bool(self.refreshed);
        match self.deciding {
            Some(id) => c.bool(true).u64(id as u64),
            None => c.bool(false).u64(0),
        };
        c.bool(self.decided);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("seerr_title tv={} refreshed={} deciding={:?}", self.kind == MediaKind::Tv, self.refreshed,
            self.deciding));
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
                let shown = self.controls(H::seerr(cx));
                let focused = cx.focus.current.filter(|k| k.entry == self.entry).map(|k| k.elem)
                    .filter(|elem| shown.iter().any(|(e, _)| e == elem));
                self.tick(*t, focused);
                self.follow(cx, fx);
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
        let len = self.controls(H::seerr(cx)).len();
        if len == 0 {
            return;
        }
        out.push(GroupSpec { id: GROUP, kind: GroupKind::Row { wrap: false }, seat: Seat::First, reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4], extent: Rect::FULL, len, elem: ElemKind::Control });
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.controls(H::seerr(cx)).iter().any(|(e, _)| e == elem).then_some(GROUP)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        let elems: Vec<u32> = self.controls(H::seerr(cx)).iter().map(|(e, _)| *e).collect();
        plx_ui::page_regions::row_neighbour(self.entry, &elems, key, dir)
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        let rects = self.control_rects(H::seerr(cx), cx.measure);
        let (index, (_, rect)) = rects.iter().enumerate().find(|(_, (e, _))| e == elem)?;
        Some(Placed { rect: *rect, rest_rect: *rect, clip: Rect::FULL, index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        let seerr = H::seerr(cx);
        if self.group_of(&want.elem, cx).is_some() {
            return want;
        }
        self.first_key(seerr).unwrap_or(want)
    }
    fn seat(&self, _: GroupId, _: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        self.first_key(H::seerr(cx)).unwrap_or(FocusKey { entry: self.entry, elem: PILL_ELEM })
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
