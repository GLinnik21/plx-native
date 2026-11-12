//! Live TV reads its own retained store and composes two adjacent shared card shelves.
//! Channel cards and current programmes both ask the content owner to play that channel.
use plx_data::livetv::{LiveTvStatus, LiveTvView};
use plx_data::stores::{StoreCmd, StoreId};
use plx_data::stores::livetv::LiveTvCmd;
use plx_machine::machine::{Canon, Cx, Delivery, Edge, Effects, Enter, EntryId, FocusKey, FocusTarget,
    Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, Tick};
use plx_machine::present::Provenance;
use plx_ui::cards::{CardEvent, CardSource, Kind, RowStyle, SectionSpec, Stack, StackEvent, StackPage, TileLabel};
use plx_ui::consts::*;
use plx_ui::page_header::{HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, SharedRow};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::screen::{AxisMask, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind,
    GroupSpec, HitSource, Link, RenderStrategy, Screen, ScreenEvent, Seat};
use plx_ui::theme;
use plx_ui::widgets::{Art, PageGround, StatusKind, StatusOverlay};
use plx_ui::{Env, Rect};
use crate::registry::{captions, tile_facts, AppFx, ContentReq, HomeReq, HomeTab, LiveTvLike, PlayIntent};

#[cfg(test)]
pub(crate) mod cards_harness;

const FIRST_CARD_ELEM: u32 = 0x1000;
const RETRY_ELEM: u32 = 1;
const CHANNELS_GROUP: GroupId = GroupId(0);
const NOW_GROUP: GroupId = GroupId(1);
const HEAD_GROUP: GroupId = GroupId(2);
const STATUS_GROUP: GroupId = GroupId(3);
const SHELF_HEADING: f32 = TITLE_DY + CARD_DY;
const HEAD_H: f32 = plx_ui::page_header::GRID_TOP;
const STYLE: RowStyle = RowStyle::HOME;
const STATUS_FRAME: Rect = Rect { x: MARGIN_X, y: HEAD_H, w: SCR_W - 2.0 * MARGIN_X, h: SCR_H - HEAD_H - MARGIN_Y };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec { Head, Channels, Now, Status }

struct Page {
    entry: EntryId,
    channels: Vec<(plx_plex::plex::ServerId, String)>,
    airings: Vec<usize>,
    status: LiveTvStatus,
    synced: Option<u32>,
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    handoff: plx_ui::scroll_chrome::Handoff,
    header_rows: SharedRow<HeaderRow>,
}

impl Page {
    fn sync(&mut self, view: LiveTvView<'_>) {
        if self.synced == Some(view.revision()) { return; }
        self.synced = Some(view.revision());
        self.status = view.status;
        self.channels = view.tiles.iter().map(|m| (m.sid, m.rk.clone())).collect();
        self.airings = view.channels.iter().enumerate().filter_map(|(i, c)| (!c.now.is_empty()).then_some(i)).collect();
    }

    fn regions(&self) -> PageRegions<'static> {
        PageRegions { header: None, content: if self.status == LiveTvStatus::Ready && !self.channels.is_empty() {
            if self.airings.is_empty() { &[CHANNELS_GROUP] } else { &[CHANNELS_GROUP, NOW_GROUP] }
        } else if self.status == LiveTvStatus::Failed { &[STATUS_GROUP] } else { &[] },
            rail: None, back_walks_header: false, arrival: Arrival::Content, pending: None }
    }

    fn header_row(&self, measure: &dyn plx_machine::machine::Measure) -> std::rc::Rc<HeaderRow> {
        let key = std::ptr::from_ref(plx_platform::i18n::current()) as usize as u64;
        self.header_rows.get(key, || {
            let left = vec![HeaderItem { label: plx_platform::i18n::msg::browse_home_live_tv().to_owned(), title: true, ..Default::default() }];
            let page = PageHeader::new(PageHeaderSpec { left, ..Default::default() }, &[], measure);
            let layout = page.layout(SCR_W, measure);
            HeaderRow::new(page, layout, Vec::new())
        })
    }

    fn status_overlay(&self, tick: u32) -> StatusOverlay<'static> {
        match self.status {
            // placeholder-exempt: built here, counted by StatusOverlay's Working draw.
            LiveTvStatus::Loading => StatusOverlay::new(STATUS_FRAME, plx_platform::i18n::msg::browse_livetv_loading_c(), StatusKind::Working).phase(tick),
            LiveTvStatus::Empty => StatusOverlay::new(STATUS_FRAME, plx_platform::i18n::msg::browse_livetv_empty_c(), StatusKind::Empty),
            LiveTvStatus::Failed => StatusOverlay::new(Rect::FULL, plx_platform::i18n::msg::browse_home_failed_c(), StatusKind::Failed)
                .page(plx_ui::icons::Icon::ServerBadgeMinus).action(plx_platform::i18n::msg::browse_action_retry_c()),
            LiveTvStatus::Ready => StatusOverlay::new(STATUS_FRAME, c"", StatusKind::Empty),
        }
    }
}

struct Cards<'a> { view: LiveTvView<'a>, airings: &'a [usize], now: bool }
impl Cards<'_> {
    fn channel_index(&self, i: usize) -> Option<usize> {
        if self.now { self.airings.get(i).copied() } else { (i < self.view.channels.len()).then_some(i) }
    }
}
impl<H: LiveTvLike> CardSource<H> for Cards<'_> {
    fn len(&self) -> usize { if self.now { self.airings.len() } else { self.view.channels.len() } }
    fn elem(&self, i: usize) -> u32 { FIRST_CARD_ELEM + self.channel_index(i).unwrap_or(0) as u32 * 2 + u32::from(self.now) }
    fn index_of(&self, e: &u32) -> Option<usize> {
        let offset = e.checked_sub(FIRST_CARD_ELEM)?;
        if offset % 2 != u32::from(self.now) { return None; }
        let i = offset as usize / 2;
        if self.now { self.airings.iter().position(|&n| n == i) } else { (i < self.view.channels.len()).then_some(i) }
    }
    fn art(&self, i: usize) -> Art<'_> {
        let facts = self.channel_index(i).and_then(|i| {
            let mut facts = tile_facts::of(self.view.tiles.get(i)?);
            if self.now {
                let channel = self.view.channels.get(i)?;
                facts.owner = channel as *const _ as usize;
                facts.title = &channel.now;
                facts.show_title = "";
                facts.thumb = &channel.now_thumb;
                facts.still = "";
                facts.art = "";
            }
            Some(facts)
        });
        Art::Poster(facts)
    }
    fn label(&self, i: usize) -> TileLabel {
        let Some(index) = self.channel_index(i) else { return TileLabel::default() };
        let Some(item) = self.view.tiles.get(index) else { return TileLabel::default() };
        if !self.now { return captions::shelf_label(&captions::policy(), item, false); }
        let channel = &self.view.channels[index];
        let mut facts = tile_facts::caption_facts(item);
        facts.title = Some(channel.now.clone());
        captions::label(&captions::live_tv_policy(), &facts,
            Some(plx_platform::i18n::msg::browse_livetv_channel(&channel.name, &channel.vcn)))
    }
}

impl<H: LiveTvLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Cards<'a>;
    fn revision(&self, cx: &Cx<'_, H>) -> u64 { u64::from(H::livetv(cx).revision()) }
    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: HEAD_H, focusable: false }, HEAD_GROUP));
        out.push(SectionSpec::new(Sec::Channels, Kind::Shelf { style: &STYLE, heading: SHELF_HEADING }, CHANNELS_GROUP).seated(plx_ui::page_regions::CONTENT_HEAD_SEAT));
        out.push(SectionSpec::new(Sec::Now, Kind::Shelf { style: &STYLE, heading: SHELF_HEADING }, NOW_GROUP));
        let retry = (self.status == LiveTvStatus::Failed).then(|| self.status_overlay(cx.tick.ms).action_frame_measured(cx.measure)).flatten();
        out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: retry.unwrap_or(STATUS_FRAME), focusable: retry.is_some() }, STATUS_GROUP));
    }
    fn shelf_foot(&self, _cx: &Cx<'_, H>, k: Sec) -> f32 { if k == Sec::Channels && !self.airings.is_empty() { UNDER_LABEL_AIR } else { 0.0 } }
    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) { out.extend([Sec::Channels, Sec::Now, Sec::Status, Sec::Head]); }
    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<Cards<'a>> {
        let view = H::livetv(cx);
        (view.status == LiveTvStatus::Ready && matches!(k, Sec::Channels | Sec::Now)).then_some(Cards { view, airings: &self.airings, now: k == Sec::Now })
    }
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool { false }
    fn caption_policy(&self, k: Sec) -> Option<plx_ui::caption_policy::CaptionPolicy> { matches!(k, Sec::Channels | Sec::Now).then(captions::live_tv_policy) }
    fn elem_of(&self, k: Sec) -> Option<u32> { (k == Sec::Status).then_some(RETRY_ELEM) }
    fn plain_group(&self, _cx: &Cx<'_, H>, _k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        GroupSpec { id, kind: GroupKind::Free, seat: Seat::First, reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop], extent, len: 1, elem: ElemKind::Bare }
    }
    fn draw_heading(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, lift: f32) {
        let title = match k { Sec::Channels => plx_platform::i18n::msg::browse_livetv_channels_c(), Sec::Now => plx_platform::i18n::msg::browse_livetv_on_now_c(), _ => return };
        plx_ui::cards::draw_heading_c(f.painter.alpha(f.page_alpha).translate(0.0, r.y), title, c"", c"", MARGIN_X, -lift, 1.0, f.measure);
    }
    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        if k == Sec::Head {
            let p = f.painter.alpha(f.page_alpha).alpha(self.handoff.head_alpha(r.y));
            self.header_row(f.measure).draw(p, r.y, &self.header_capsules, self.header_pop.scale_with(0, f.press.scale), None);
        } else if k == Sec::Status && self.status != LiveTvStatus::Ready {
            self.status_overlay(f.cx.tick.ms).focused(f.focus.current.is_some_and(|k| k.entry == self.entry && k.elem == RETRY_ELEM))
                .draw_measured(&Env::inert(), f.painter.alpha(f.page_alpha), f.measure);
        }
    }
}

pub struct LiveTvScreen {
    page: Page,
    stack: Stack<Sec>,
    ground: PageGround,
    ground_seeded: bool,
    /// The page's own arrival seat while nobody has chosen another (the Watchlist's rule): `None`
    /// once the reader acts or the page is restored.
    seat: Option<Option<GroupId>>,
    /// Seats this page asked for whose `Enter` has not come back yet.
    own_enters: u8,
    /// How many of those were queued before the reader acted.
    stale_enters: u8,
}

impl LiveTvScreen {
    /// The heartbeat word, byte-identical to the route word.
    pub const WORD: &'static str = "livetv";

    pub const SHAPE: &'static str = "LiveTvScreen{entry:u32,status:u32,channels:[{sid:u32,rk:str}],airings:[u32],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Shelf{row,pop},Shelf{row,pop}]},handoff:Handoff{tucked:bool,chrome:f32,capsule:f32}}";

    pub fn new(entry: EntryId) -> Self {
        Self {
            page: Page {
                entry,
                channels: Vec::new(),
                airings: Vec::new(),
                status: LiveTvStatus::Loading,
                synced: None,
                header_capsules: plx_ui::widgets::TabStrip::new(),
                header_pop: plx_ui::widgets::CtlPop::new(),
                handoff: plx_ui::scroll_chrome::Handoff::new(),
                header_rows: SharedRow::new(),
            },
            stack: Stack::new(entry).home_when_unfocused(true),
            ground: PageGround::new(),
            ground_seeded: false,
            seat: None,
            own_enters: 0,
            stale_enters: 0,
        }
    }

    /// Both shelves name the channel that supplies their card.
    fn press<H: LiveTvLike>(&self, elem: u32, cx: &Cx<'_, H>) -> Option<ContentReq> {
        let index = elem.checked_sub(FIRST_CARD_ELEM)? as usize / 2;
        let view = H::livetv(cx);
        let channel = view.channels.get(index)?;
        if elem % 2 == 1 && channel.now.is_empty() { return None; }
        Some(ContentReq::Play { play: PlayIntent::Live {
            sid: channel.sid, dvr: channel.dvr.clone(), channel: channel.channel.clone(), title: channel.name.clone(),
        }, resume_ns: 0 })
    }

    fn tick<H: LiveTvLike>(&mut self, t: Tick, cx: &Cx<'_, H>) {
        self.page.header_row(cx.measure).step(self.page.entry, cx.focus.current, &mut self.page.header_capsules, &mut self.page.header_pop, t.dt());
        self.page.handoff.step(t.dt(), self.stack.scroll(), self.stack.target(), plx_ui::cards::RowStyle::HOME.pitch_fixed(), false);
        let flat = [theme::SURFACE_APP; 4];
        if self.ground_seeded {
            self.ground.key_target(flat, t.dt());
        } else {
            self.ground.jump_target(flat);
            self.ground_seeded = true;
        }
    }

    fn route<H: LiveTvLike>(&self, out: HomeReq, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Home(out)));
        fx.invalidate(Provenance::Input);
    }

    /// Seat the arrival in the first shelf once it has a channel; until then seat nothing (the
    /// Watchlist's `seat_first`).
    fn seat_first<H: LiveTvLike>(&mut self, fx: &mut Effects<'_, H>) {
        let regions = self.page.regions();
        let want = regions.first_seat();
        let target = want.map(FocusTarget::FirstInGroupAnimated).or_else(|| regions.unseat());
        self.seat = (want != Some(CHANNELS_GROUP)).then_some(want);
        if let Some(target) = target {
            self.seat_own(target, fx);
        }
    }

    fn follow_seat<H: LiveTvLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(seated) = self.seat else { return };
        if seated != self.page.regions().first_seat() {
            self.seat_first(fx);
        }
    }

    fn seat_own<H: LiveTvLike>(&mut self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        self.own_enters = self.own_enters.saturating_add(1);
        fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    /// BACK, decided by [`PageRegions::back`]: a scrolled page first returns to its top, on the
    /// first channel; from there it leaves for Home.
    fn back<H: LiveTvLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.page.entry);
        let regions = self.page.regions();
        let at = focus.and_then(|key| regions.region_of(key.elem, Focusable::<H>::group_of(self, &key.elem, cx)));
        match regions.back(at, self.stack.target() > 0.5) {
            BackStep::Top => {
                self.seat_own(FocusTarget::FirstInGroupAnimated(CHANNELS_GROUP), fx);
            }
            BackStep::Content | BackStep::Header => self.seat_own(FocusTarget::ContainerGroup(CHANNELS_GROUP), fx),
            BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => self.route(HomeReq::Tab(HomeTab::Home), fx),
        }
        Handled::Yes
    }

    /// A non-card element committed: the strip's pills and the chip.
    fn activate<H: LiveTvLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        match Self::strip_press(elem) {
            Some(Some(out)) => self.route(out, fx),
            Some(None) => {}
            None => return Handled::No,
        }
        Handled::Yes
    }

    /// A press on the shared strip: the other tabs and the chip are requests; the Live TV pill is
    /// the page the reader is already on.
    fn strip_press(elem: u32) -> Option<Option<HomeReq>> {
        use plx_ui::widgets::{
            TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST,
        };
        match elem {
            TAB_KEY_HOME => Some(Some(HomeReq::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(HomeReq::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(HomeReq::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(HomeReq::Tab(HomeTab::Search))),
            TAB_KEY_WATCHLIST => Some(Some(HomeReq::Tab(HomeTab::Watchlist))),
            TAB_KEY_CHIP => Some(Some(HomeReq::Account)),
            TAB_KEY_LIVETV => Some(None),
            _ => None,
        }
    }

    fn view<H: LiveTvLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    /// The page's top edge once the chrome has tucked away (the Watchlist's).
    fn draw_chrome<H: LiveTvLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let tuck = self.page.handoff.tuck();
        if tuck <= 0.0 {
            return;
        }
        let ground = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
        plx_ui::scroll_capsule::draw_top_fade(f.painter.alpha(f.page_alpha), SCR_W, ground, tuck, None);
    }
}

plx_ui::focusable_via_view!(LiveTvScreen, H: [LiveTvLike], view);

impl LogicalState for LiveTvScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.page.entry.0);
        c.u32(match self.page.status { LiveTvStatus::Loading => 0, LiveTvStatus::Ready => 1, LiveTvStatus::Empty => 2, LiveTvStatus::Failed => 3 });
        c.seq(self.page.channels.len());
        for (sid, rk) in &self.page.channels { c.u32(u32::from(sid.raw())).str(rk); }
        c.seq(self.page.airings.len());
        for &i in &self.page.airings { c.u32(i as u32); }
        self.stack.write(c);
        self.page.handoff.write(c);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!("livetv channels={}", self.page.channels.len()));
    }
}

impl<H: LiveTvLike> Machine<H> for LiveTvScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(_) | ScreenEvent::Enter(Enter::Fresh { .. }) => self.page.sync(H::livetv(cx)),
            ScreenEvent::StoreChanged(ord, _) if *ord == StoreId::LiveTv.ord() => {
                self.page.synced = None;
                self.page.sync(H::livetv(cx));
            }
            _ => {}
        }
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        // The reader acting makes the seat theirs (claimed on the event, as the Watchlist does).
        if let ScreenEvent::Input(InputEvent { kind, .. }) = ev {
            if matches!(kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Pointer { .. }
                | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. }) {
                self.seat = None;
                self.stale_enters = self.own_enters;
            }
        }
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx);
                self.follow_seat(fx);
                Handled::Yes
            }
            ScreenEvent::Enter(enter) => {
                match enter {
                    Enter::Fresh { .. } if self.own_enters > 0 => {
                        self.own_enters -= 1;
                        if self.stale_enters > 0 {
                            self.stale_enters -= 1;
                            if let Some(held) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                                self.own_enters = self.own_enters.saturating_add(1);
                                fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                                    focus: FocusTarget::Elem(held),
                                }))));
                            }
                        }
                    }
                    Enter::Fresh { .. } => {
                        self.seat_first(fx);
                    }
                    Enter::Restored => {
                        self.seat = None;
                        self.stale_enters = self.own_enters;
                    }
                }
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                if *elem == RETRY_ELEM && self.page.status == LiveTvStatus::Failed {
                    fx.push(Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))));
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                } else { self.activate(*elem, fx) }
            },
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    if let Some(out) = self.press(elem, cx) {
                        fx.push(Fx::App(AppFx::Content(out)));
                        fx.invalidate(Provenance::Input);
                    }
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    if key.elem == RETRY_ELEM && self.page.status == LiveTvStatus::Failed {
                        fx.push(Fx::App(AppFx::Store(StoreId::LiveTv, StoreCmd::LiveTv(LiveTvCmd::Retry))));
                        fx.invalidate(Provenance::Input);
                    } else { self.activate(key.elem, fx); }
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                self.back(cx, fx)
            }
            _ => Handled::No,
        }
    }
}

impl<H: LiveTvLike> Screen<H> for LiveTvScreen {
    fn focused_card_rect(&self, cx: &Cx<'_, H>, focus: Option<FocusKey<u32>>, at: plx_ui::screen::At) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.page.entry && k.elem >= FIRST_CARD_ELEM)?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|placed| placed.rect)
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
    fn opener_departing(&self, key: FocusKey<u32>) -> bool {
        self.stack.view(&self.page).opener_departing::<H>(key)
    }
    fn name(&self) -> &'static str {
        Self::WORD
    }
    fn chrome_tuck(&self) -> f32 {
        self.page.handoff.tuck()
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
        self.stack.view(&self.page).paint(f);
        self.draw_chrome(f);
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
    fn links(&self, out: &mut Vec<Link>) {
        self.page.regions().links(out);
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.page.regions().holds(dir)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "livetv_tests.rs"]
mod tests;
