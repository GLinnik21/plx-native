//! **The catalog page** (plan §1.7, §2.4 W6; boards DetailNotOnServer, DetailOffline, DetailOnList):
//! the page for a title no server holds, opened by `ContentArg::Catalog { hex }` from a Watchlist
//! tile that has no playable copy, and by the hold menu's *Go to ...* row.
//!
//! It is NOT a Detail mode: it never reads the metadata singleton (no MORE, no Tracks, no Play, no
//! Restart, no Also available, no Mark watched, no extras, seasons, episodes, cast or Related), and
//! its one focus group holds one element, the primary pill. The view-model is the screen's OWN type
//! (the store-side `plx_data::catalog::{Load, Membership}` maps onto it in the app's mount glue), so
//! the screen is drawn and tested without a store.
//!
//! **What it draws.** The art ground (the discover art through a reachable server's photo proxy,
//! else the neutral app ground), the title (a text title: a discover item has no clearLogo), the
//! meta line (kind, up to two genres, year), the status line with its icon (clock / circle-slash / server-slash /
//! none), the *Coming 12 March* line, the line-clamped summary and the primary pill, *Add to
//! Watchlist* with the outline bookmark / *On Watchlist* with the filled one. The hero's rung
//! constants and the pill's width come from `appkit::hero_layout`, the same numbers the Detail
//! action row spends.
//!
//! **Trailer: omitted in v1.** The board DetailNotOnServer shows a *Trailer* button beside the
//! pill; the plan's owner question Q4 defaults it OFF because probe P5 (does the discover record
//! carry a playable trailer extra) is unverified. This is a default awaiting the owner, not a
//! decision: the string `watchlist.detail.trailer` is shipped and the pill row is a slice, so the
//! second control is an addition, not a rework.
//!
//! **A press is a request.** OK on the pill asks the app for the write ([`CatalogOut`], carried as
//! `AppFx::Catalog`); the screen flips nothing itself. The Watchlist store's intent ledger flips the
//! membership at once and reverts it when plex.tv refuses, and the app hands the screen the new
//! [`CatalogViewModel`] either way (`CatalogScreen::input`), so a press that failed reads as the
//! face going back. The page keeps no membership of its own.
//!
//! **Board differences (shipped code wins).** The boards place the pill at a fixed `top: 820` and
//! the text at `top: 380`; this page stacks its rows the way the Detail hero does, each row a named
//! `theme::space` rung under the last measured one, from `detail_layout::TITLE_BOTTOM`. The
//! board's *Coming 12 March* is 36 px; the type ladder has no 36 rung (`theme::size` stops at
//! HEADLINE 32 and TITLE 40, and text may not sit between rungs), so the line is HEADLINE bold.
//! The board's 26 px status icon is `size::LABEL`, the line's own rung.
use std::ffi::CString;

use plx_appkit::hero_layout;
use plx_data::watchlist::model::Kind;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, Measure,
    Tick,
};
use plx_machine::present::Provenance;
use plx_plex::plex::ServerId;
use plx_ui::hero_logo::{HeroLogo, LogoRung};
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::icons::Icon;
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use plx_ui::vstack::VFlow;
use plx_ui::widgets::{Button, ControlGround, CtlPop, StatusKind, StatusOverlay};
use plx_ui::{detail_layout, theme, Env, Painter, Rect, View};

use crate::registry::{AppFx, ContentLike, ContentReq};


/// Is the title on the Watchlist? `Unknown` wears the Add face (the PUT is idempotent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Membership {
    On,
    Off,
    #[default]
    Unknown,
}

/// The status line's meaning, which picks its icon (clock / circle-slash / server-slash / none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    None,
    ComingSoon,
    NotOnAnyServer,
    ServerOffline,
}

/// Where the page's record stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Loading,
    Loaded {
        title: String,
        year: Option<i32>,
        kind: Kind,
        summary: String,
        genres: Vec<String>,
        /// Absolute discover art URL; empty = a neutral ground.
        art_url: String,
        /// `YYYY-MM-DD`.
        release_date: Option<String>,
        status: Status,
    },
    NotFound,
    Unreachable,
}

/// Everything the catalog page draws and decides from, in one value.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogViewModel {
    pub load: Load,
    pub membership: Membership,
}

impl Default for CatalogViewModel {
    fn default() -> Self {
        Self { load: Load::Loading, membership: Membership::Unknown }
    }
}

impl From<plx_data::catalog::Membership> for Membership {
    fn from(m: plx_data::catalog::Membership) -> Self {
        match m {
            plx_data::catalog::Membership::On => Self::On,
            plx_data::catalog::Membership::Off => Self::Off,
            plx_data::catalog::Membership::Unknown => Self::Unknown,
        }
    }
}

impl From<plx_data::catalog::CatalogStatus> for Status {
    fn from(s: plx_data::catalog::CatalogStatus) -> Self {
        match s {
            plx_data::catalog::CatalogStatus::None => Self::None,
            plx_data::catalog::CatalogStatus::ComingSoon => Self::ComingSoon,
            plx_data::catalog::CatalogStatus::NotOnAnyServer => Self::NotOnAnyServer,
            plx_data::catalog::CatalogStatus::ServerOffline => Self::ServerOffline,
        }
    }
}

impl From<&plx_data::catalog::Load> for Load {
    fn from(l: &plx_data::catalog::Load) -> Self {
        use plx_data::catalog::Load as S;
        match l {
            S::Loading => Self::Loading,
            S::NotFound => Self::NotFound,
            S::Unreachable => Self::Unreachable,
            S::Loaded(r) => Self::Loaded {
                title: r.title.clone(),
                year: r.year,
                kind: r.kind,
                summary: r.summary.clone(),
                genres: r.genres.clone(),
                art_url: r.art_url.clone(),
                release_date: r.release_date.clone(),
                status: r.status.into(),
            },
        }
    }
}

// ---- what a press asks of the app ------------------------------------------------------------

/// What the page asks of the application (`AppFx::Catalog`): the write the pill stands for. The
/// app queues it on the Watchlist store and owns the tab pulse; the screen only names the title
/// (the 12 bytes of its discover id, captured when the page was opened).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogOut {
    /// The pill on its Add face: put the title on the Watchlist.
    Add { hex: [u8; 12] },
    /// The pill on its filled face: take it off.
    Remove { hex: [u8; 12] },
}

// ---- the pure decisions (host-testable without a frame) ------------------------------------------

/// The pill's focus element: the page's only one.
pub const PILL_ELEM: u32 = 0;
const GROUP: GroupId = GroupId(0);

/// The pill's face: the bookmark it wears. `Unknown` wears the Add face (the PUT is idempotent),
/// so a page that cannot read the list still lets the person add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PillFace {
    pub filled: bool,
    pub icon: Icon,
}

pub fn pill_face(m: Membership) -> PillFace {
    match m {
        Membership::On => PillFace { filled: true, icon: Icon::BookmarkFill },
        Membership::Off | Membership::Unknown => PillFace { filled: false, icon: Icon::Bookmark },
    }
}

fn pill_label(m: Membership) -> &'static std::ffi::CStr {
    match m {
        Membership::On => plx_platform::i18n::msg::watchlist_action_on_list_c(),
        Membership::Off | Membership::Unknown => plx_platform::i18n::msg::watchlist_action_add_c(),
    }
}

/// What a press on the pill asks for, given the membership the page is showing.
pub fn press_out(m: Membership, hex: [u8; 12]) -> CatalogOut {
    match m {
        Membership::On => CatalogOut::Remove { hex },
        Membership::Off | Membership::Unknown => CatalogOut::Add { hex },
    }
}

/// The *Coming 12 March* line: only an unreleased title with a usable date has one.
pub fn coming_line(status: Status, release: Option<&str>) -> Option<String> {
    if status != Status::ComingSoon {
        return None;
    }
    plx_ui::fmt::coming_on(release?, plx_platform::i18n::MonthWidth::Long)
}

/// The status line's icon. A dated coming-soon title says it in the larger *Coming ...* line
/// (the board draws that line bare), so the clock marks the dateless one.
pub fn status_icon(status: Status, release: Option<&str>) -> Option<Icon> {
    match status {
        Status::ComingSoon if coming_line(status, release).is_none() => Some(Icon::Clock),
        Status::NotOnAnyServer => Some(Icon::CircleSlash),
        Status::ServerOffline => Some(Icon::ServerSlash),
        Status::ComingSoon | Status::None => None,
    }
}

/// The read-out of a page with no record to show. A discover 404 reuses "Not on your servers": the service
/// knows no such title, so no library of the household's can hold it, and the shipped locales carry
/// no separate not-found string (docs/watchlist.md, "A record the service does not know").
pub fn verdict_text(load: &Load) -> Option<&'static str> {
    match load {
        Load::NotFound => Some(plx_platform::i18n::msg::watchlist_state_on_no_server()),
        Load::Unreachable => Some(plx_platform::i18n::msg::watchlist_empty_unreachable()),
        Load::Loaded { .. } | Load::Loading => None,
    }
}

/// The status line's words, present exactly when [`status_icon`] is.
pub fn status_text(status: Status, release: Option<&str>) -> Option<&'static str> {
    match (status_icon(status, release)?, status) {
        (Icon::Clock, _) => Some(plx_platform::i18n::msg::watchlist_state_coming_soon()),
        (_, Status::ServerOffline) => Some(plx_platform::i18n::msg::watchlist_state_server_offline()),
        _ => Some(plx_platform::i18n::msg::watchlist_state_on_no_server()),
    }
}

/// The meta line's runs: kind, up to two genres, year.
pub fn meta_parts(kind: Kind, genres: &[String], year: Option<i32>) -> Vec<String> {
    let mut parts = vec![match kind {
        Kind::Movie => plx_platform::i18n::msg::browse_kind_movie().to_string(),
        Kind::Show => plx_platform::i18n::msg::browse_kind_tv_show().to_string(),
    }];
    parts.extend(genres.iter().filter(|g| !g.is_empty()).take(2).cloned());
    parts.extend(year.filter(|&y| y > 0).map(|y| y.to_string()));
    parts
}

/// The status icon's box: the status line's own rung, as the board's 26 px glyph.
const STATUS_ICON_D: f32 = theme::size::LABEL as f32;

/// The rows of the hero, top to bottom, each placed a named rung under the last one's measured
/// bottom (`detail_layout::hero_chain`'s grammar): `SM` inside the identity group, `MD` into the
/// prose, `LG` into the action row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stack {
    pub coming: Option<f32>,
    pub meta: f32,
    pub status: Option<f32>,
    pub summary: f32,
    pub summary_h: f32,
    pub pill: f32,
}

pub fn stack(has_coming: bool, has_status: bool, summary: &str, measure: &dyn Measure) -> Stack {
    // The title's bottom edge is a zero-height first item, so the first row's SM is an ordinary gap.
    let mut flow = VFlow::new(detail_layout::TITLE_BOTTOM, theme::space::SM);
    flow.push(Item::new(0.0));
    let mut row = |present: bool, h: f32| flow.push(if present { Item::new(h) } else { Item::absent() });
    let coming = row(has_coming, measure.cap_h(theme::size::HEADLINE)).map(|s| s.y);
    let meta = row(true, measure.cap_h(theme::size::BODY)).map_or(0.0, |s| s.y);
    let status = row(has_status, STATUS_ICON_D).map(|s| s.y);
    let summary_h = if summary.is_empty() {
        0.0
    } else {
        plx_ui::hero_synopsis(summary, "").with_measure(measure).measure_h(detail_layout::HERO_TEXT_W)
    };
    // An empty summary is a present, zero-high row with no gap of its own (`before(0)`): it stands
    // where the last row ends and the action pill still sits one LG under that.
    let summary_item = if summary.is_empty() { Item::new(0.0).before(0.0) } else { Item::new(summary_h).before(theme::space::MD) };
    let summary_y = flow.push(summary_item).map_or(0.0, |s| s.y);
    let pill = flow.push(Item::new(hero_layout::CD).before(theme::space::LG)).map_or(0.0, |s| s.y);
    Stack { coming, meta, status, summary: summary_y, summary_h, pill }
}

// ---- the screen -----------------------------------------------------------------------------------

/// The catalog page of one discover title.
pub struct CatalogScreen {
    entry: EntryId,
    hex: [u8; 12],
    vm: CatalogViewModel,
    /// A reachable server whose photo proxy fetches the discover art; `None` draws the neutral ground.
    art_server: Option<ServerId>,
    pop: CtlPop<1>,
}

impl CatalogScreen {
    /// The heartbeat word, byte-identical to the route word (`app/words.rs`).
    pub const WORD: &'static str = "catalog";

    pub const SHAPE: &'static str = "CatalogScreen{entry:u32,hex:[u8;12],load:u32,status:u32,membership:u32}";

    /// The conformance mount: the page of `hex`, loading, as the registry mounts it.
    pub fn new(entry: EntryId, hex: [u8; 12]) -> Self {
        Self { entry, hex, vm: CatalogViewModel::default(), art_server: None, pop: CtlPop::new() }
    }

    /// The app hands the page its store snapshot: on the first frames, and again whenever the
    /// record, the membership or a server match moved. Only what is drawn from it changes; a
    /// match landing while the page is open re-derives the status line and nothing else.
    pub fn input(&mut self, vm: CatalogViewModel) {
        if self.vm != vm {
            self.vm = vm;
            plx_machine::idle::invalidate();
        }
    }

    /// The reachable server that proxies the art (`None`: neutral ground).
    pub fn set_art_server(&mut self, server: Option<ServerId>) {
        if self.art_server != server {
            self.art_server = server;
            plx_machine::idle::invalidate();
        }
    }

    pub fn view_model(&self) -> &CatalogViewModel {
        &self.vm
    }

    fn key(&self) -> FocusKey<u32> {
        FocusKey { entry: self.entry, elem: PILL_ELEM }
    }

    fn loaded(&self) -> bool {
        matches!(self.vm.load, Load::Loaded { .. })
    }

    /// The rows' placement, for a loaded record.
    fn stack(&self, measure: &dyn Measure) -> Option<Stack> {
        let Load::Loaded { summary, release_date, status, .. } = &self.vm.load else { return None };
        Some(stack(
            coming_line(*status, release_date.as_deref()).is_some(),
            status_icon(*status, release_date.as_deref()).is_some(),
            summary,
            measure,
        ))
    }

    /// The pill's frame: the hero row's first (and only) control.
    fn pill_rect(&self, measure: &dyn Measure) -> Option<Rect> {
        let stack = self.stack(measure)?;
        let w = hero_layout::pill_w(measure, pill_label(self.vm.membership), theme::size::BODY, true, None)
            .max(hero_layout::PW);
        Some(hero_layout::row_rect(&[w], 0, stack.pill))
    }

    fn activate<H: ContentLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        if elem != PILL_ELEM || !self.loaded() {
            return Handled::No;
        }
        fx.push(Fx::App(AppFx::Catalog(press_out(self.vm.membership, self.hex))));
        fx.invalidate(Provenance::Input);
        Handled::Yes
    }

    fn draw_ground(&self, p: Painter) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        if let (Load::Loaded { art_url, .. }, Some(server)) = (&self.vm.load, self.art_server) {
            if !art_url.is_empty() {
                let (tex, w, h) = plx_ui::widgets::resolve_tex_wh_on(
                    server.raw(),
                    art_url,
                    plx_ui::consts::SCR_W as i32,
                    plx_ui::consts::SCR_H as i32,
                    0,
                );
                if tex != 0 {
                    p.tex_uv(tex, plx_gfx::gfx::UV_FULL, Rect::FULL.cover(w, h), 0.0, theme::TINT_WHITE);
                }
            }
        }
        plx_ui::widgets::hero_scrim(p, 1.0, false);
    }

    fn draw_record<H: ContentLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
        let Load::Loaded { title, year, kind, genres, release_date, status, summary, .. } = &self.vm.load else {
            return;
        };
        let measure = f.measure;
        let Some(stack) = self.stack(measure) else { return };
        let x = plx_ui::consts::MARGIN_X;
        let release = release_date.as_deref();

        let band_h = plx_ui::hero_logo::band_h(LogoRung::Hero);
        HeroLogo::new(0, "", title, LogoRung::Hero).draw(
            p,
            Rect::new(x, detail_layout::TITLE_BOTTOM - band_h, detail_layout::HERO_TEXT_W, band_h),
            measure,
        );

        if let (Some(y), Some(line)) = (stack.coming, coming_line(*status, release)) {
            if let Ok(line) = CString::new(line) {
                Label::new(line.as_ptr(), theme::size::HEADLINE, theme::TEXT_PRIMARY)
                    .bold()
                    .v(VAlign::CapTop)
                    .h(HAlign::Left)
                    .draw(p, Rect::new(x, y, detail_layout::HERO_TEXT_W, measure.cap_h(theme::size::HEADLINE)));
            }
        }

        let parts = meta_parts(*kind, genres, *year);
        let runs: Vec<&str> = parts.iter().map(String::as_str).collect();
        let (cap_top, _) = plx_gfx::text::text_cap_band(theme::size::BODY, 0);
        plx_ui::widgets::dotted_run(
            p,
            &runs,
            x,
            stack.meta - cap_top,
            theme::size::BODY,
            theme::TEXT_SECONDARY,
            theme::space::SM,
        );

        if let (Some(y), Some(icon), Some(text)) =
            (stack.status, status_icon(*status, release), status_text(*status, release))
        {
            // The status row: the icon box, one SM, the label in the rest of the hero column.
            let row = [Item::new(STATUS_ICON_D), Item::new(detail_layout::HERO_TEXT_W - STATUS_ICON_D - theme::space::SM)];
            let row = HStack::new(&row, theme::space::SM, Align::Leading(x));
            let (Some((icon_x, icon_w)), Some((text_x, text_w))) = (row.span(0), row.span(1)) else { return };
            plx_ui::icons::draw(p, icon, Rect::new(icon_x, y, icon_w, STATUS_ICON_D), theme::TEXT_SECONDARY);
            if let Ok(text) = CString::new(text) {
                Label::new(text.as_ptr(), theme::size::LABEL, theme::TEXT_SECONDARY)
                    .v(VAlign::Middle)
                    .draw(p, Rect::new(text_x, y, text_w, STATUS_ICON_D));
            }
        }

        if !summary.is_empty() {
            plx_ui::hero_synopsis(summary, "")
                .with_measure(measure)
                .draw(p, Rect::new(x, stack.summary, detail_layout::HERO_TEXT_W, 0.0));
        }

        let Some(rect) = self.pill_rect(measure) else { return };
        let focused = f.cx.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == PILL_ELEM);
        let face = pill_face(self.vm.membership);
        Button::new(pill_label(self.vm.membership).as_ptr(), theme::size::BODY, rect)
            .icon(face.icon)
            .focused(focused)
            .ground(ControlGround::Keyed)
            .scale(self.pop.scale(0))
            .draw(&Env::inert(), p);
        f.stop(
            p,
            Stop {
                key: self.key(),
                rect,
                rest_rect: rect,
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Press,
            },
        );
    }

    /// The verdict of a page with no record to show: the shipped read-out faces, at the shared anchor.
    fn draw_verdict(&self, p: Painter, text: &str) {
        let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
        let Ok(text) = CString::new(text) else { return };
        let band = Rect::new(0.0, StatusOverlay::FULL_ANCHOR_TOP, plx_ui::consts::SCR_W, sz as f32);
        let label = Label::new(text.as_ptr(), sz, ink).h(HAlign::Center).v(VAlign::CapTop);
        if bold { label.bold() } else { label }.draw(p, band);
    }

    fn tick(&mut self, t: Tick, focus: Option<FocusKey<u32>>) {
        let focused = focus.is_some_and(|k| k.entry == self.entry && k.elem == PILL_ELEM) && self.loaded();
        self.pop.step(focused.then_some(0), t.dt());
    }
}

impl LogicalState for CatalogScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0);
        for b in self.hex {
            c.u8(b);
        }
        let (load, status) = match &self.vm.load {
            Load::Loading => (0, 0),
            Load::Loaded { status, .. } => (1, *status as u32),
            Load::NotFound => (2, 0),
            Load::Unreachable => (3, 0),
        };
        c.u32(load).u32(status).u32(self.vm.membership as u32);
    }

    fn probe(&self, out: &mut String) {
        let load = match &self.vm.load {
            Load::Loading => "loading",
            Load::Loaded { .. } => "loaded",
            Load::NotFound => "not_found",
            Load::Unreachable => "unreachable",
        };
        out.push_str(&format!("catalog load={load} membership={:?}", self.vm.membership));
    }
}

impl<H: ContentLike> Machine<H> for CatalogScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx.focus.current);
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
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            ScreenEvent::PressCommit(_) => match cx.focus.current.filter(|k| k.entry == self.entry) {
                Some(key) => self.activate(key.elem, fx),
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

impl<H: ContentLike> Focusable<H> for CatalogScreen {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if !self.loaded() {
            return;
        }
        out.push(GroupSpec {
            id: GROUP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: Rect::FULL,
            len: 1,
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        (*elem == PILL_ELEM && self.loaded()).then_some(GROUP)
    }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, H>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _: At) -> Option<Placed> {
        if *elem != PILL_ELEM {
            return None;
        }
        let rect = self.pill_rect(cx.measure)?;
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
    }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, H>) -> FocusKey<u32> {
        if self.loaded() { self.key() } else { want }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        self.key()
    }
}

impl<H: ContentLike> Screen<H> for CatalogScreen {
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
        self.draw_ground(p);
        match &self.vm.load {
            Load::Loaded { .. } => self.draw_record(f, p),
            Load::Loading => {}
            load => {
                if let Some(text) = verdict_text(load) {
                    self.draw_verdict(p, text);
                }
            }
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
#[path = "catalog_tests.rs"]
mod tests;
