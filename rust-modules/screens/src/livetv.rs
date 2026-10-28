//! **The Live TV page**: the shared strip's "TV en directo" destination. A header row with the
//! page's title and the six-column 250 x 375 grid of the server's channels, each its cover, its
//! name and what is on now; OK on one tunes it.
//!
//! The page is a `plx_ui::cards::Stack` of two sections, the Watchlist's shape without its
//! controls: the head (the title band, which nothing focuses) and the grid. It reads the channels
//! the Home catalog already holds (`HomeLike::hubs`: the tiles `plx_data::pms::live_channel_tile`
//! made, `KIND_LIVE_CHANNEL`), so it fetches nothing of its own and shows what Home's Live TV row
//! shows.
//!
//! **Routing.** A screen names no sibling, so a press leaves as `AppFx::LiveTv` ([`LiveTvOut`]) for
//! the application to route: the tune, the strip's tabs and the profile chip, and BACK.
use std::sync::Arc;

use plx_data::pms::PmsMovie;
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, Enter, EntryId, FocusKey, FocusTarget, Fx, GroupId, Handled, InputEvent,
    InputKind, Key, LogicalState, Machine, Tick,
};
use plx_machine::present::Provenance;
use plx_plex::plex::ServerId;
use plx_ui::cards::{CardEvent, CardSource, GridSpec, Kind, SectionSpec, Stack, StackEvent, StackPage, TileLabel};
use plx_ui::consts::*;
use plx_ui::page_header::{HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, SharedRow};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::screen::{
    AxisMask, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Link,
    RenderStrategy, Screen, ScreenEvent, Seat,
};
use plx_ui::theme;
use plx_ui::widgets::{Art, PageGround};
use plx_ui::Rect;

use crate::registry::{tile_facts, AppFx, HomeLike, HomeTab};

/// What the page asks the app to do. The app owns routing (a screen names no sibling).
#[derive(Clone, Debug, PartialEq)]
pub enum LiveTvOut {
    /// OK on a channel: tune `channel` on DVR `dvr` of server `sid` and watch it.
    Tune { sid: ServerId, dvr: String, channel: String, title: String },
    /// BACK from the top of the page.
    Back,
    /// A press on the shared strip's other tabs.
    Tab(HomeTab),
    /// A press on the profile chip.
    Account,
}

/// The grid's first row of posters, under the header row (the Watchlist's and the Library's).
const GRID_TOP: f32 = plx_ui::page_header::GRID_TOP;
/// Six columns of 250 posters from the row's text edge, the Watchlist's grid.
const COLS: usize = 6;
const GRID_GAP: f32 = (plx_ui::page_header::TEXT_X1 - MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);

fn grid_spec() -> GridSpec {
    GridSpec::resting(GRID_TOP).columns(COLS, plx_ui::cards::RowStyle { gap: GRID_GAP, ..plx_ui::cards::RowStyle::HOME }, MARGIN_X)
}

/// The first card's element; channel `i` is `FIRST_CARD_ELEM + i` (the channels are few and keep
/// their guide order, so a position is an identity for as long as the page shows them).
const FIRST_CARD_ELEM: u32 = 0x1000;

const GRID_GROUP: GroupId = GroupId(0);
const HEAD_GROUP: GroupId = GroupId(1);

/// The page's sections, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    /// The title band (not focusable).
    Head,
    /// The channel grid.
    Items,
}

/// The page's content and the state its sections read.
struct Page {
    entry: EntryId,
    /// The channels, in the catalog's order (the guide's channel numbers).
    channels: Vec<Arc<PmsMovie>>,
    /// What `channels` was built from (rk, title, what is on, cover): a catalog landing that moved
    /// none of it leaves the page as it is.
    sig: u64,
    /// Moves whenever anything the sections read moves (`StackPage::revision`).
    rev: u64,
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    handoff: plx_ui::scroll_chrome::Handoff,
    header_rows: SharedRow<HeaderRow>,
}

impl Page {
    /// Take the catalog's channels when they differ from the ones shown.
    fn sync<H: HomeLike>(&mut self, cx: &Cx<'_, H>) {
        use std::hash::{Hash, Hasher};
        let view = H::hubs(cx);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut found = Vec::new();
        for i in 0..view.hub_count() {
            let Some(hub) = view.hub(i) else { continue };
            for item in hub.items.iter().filter(|m| plx_data::pms::live_channel_of(m).is_some()) {
                (item.sid.raw(), &item.rk, &item.title, &item.show_title, &item.thumb).hash(&mut h);
                found.push(Arc::clone(item));
            }
        }
        let sig = h.finish();
        if sig != self.sig {
            self.sig = sig;
            self.channels = found;
            self.rev += 1;
        }
    }

    fn channel_of(&self, elem: u32) -> Option<&PmsMovie> {
        elem.checked_sub(FIRST_CARD_ELEM).and_then(|i| self.channels.get(i as usize)).map(|m| &**m)
    }

    fn regions(&self) -> PageRegions<'static> {
        PageRegions {
            header: None,
            content: if self.channels.is_empty() { &[] } else { &[GRID_GROUP] },
            rail: None,
            back_walks_header: false,
            arrival: Arrival::Content,
            pending: None,
        }
    }

    /// The header row: the page's title alone, rebuilt only when the locale moves.
    fn header_row(&self, measure: &dyn plx_machine::machine::Measure) -> std::rc::Rc<HeaderRow> {
        let key = std::ptr::from_ref(plx_platform::i18n::current()) as usize as u64;
        self.header_rows.get(key, || {
            let left = vec![HeaderItem {
                label: plx_platform::i18n::msg::browse_home_live_tv().to_owned(),
                title: true,
                ..Default::default()
            }];
            let page = PageHeader::new(PageHeaderSpec { left, ..Default::default() }, &[], measure);
            let layout = page.layout(SCR_W, measure);
            HeaderRow::new(page, layout, Vec::new())
        })
    }
}

/// The grid's content: the channels, each its cover and its caption.
struct Channels<'a> {
    page: &'a Page,
}

impl<H: HomeLike> CardSource<H> for Channels<'_> {
    fn len(&self) -> usize {
        self.page.channels.len()
    }
    fn elem(&self, i: usize) -> u32 {
        FIRST_CARD_ELEM + i as u32
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        e.checked_sub(FIRST_CARD_ELEM).map(|i| i as usize).filter(|&i| i < self.page.channels.len())
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Poster(self.page.channels.get(i).map(|m| tile_facts::of(m)))
    }
    fn label(&self, i: usize) -> TileLabel {
        match self.page.channels.get(i) {
            Some(m) => crate::registry::captions::shelf_label(&crate::registry::captions::policy(), m, false),
            None => TileLabel::default(),
        }
    }
}

impl<H: HomeLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Channels<'a>;

    fn revision(&self, _cx: &Cx<'_, H>) -> u64 {
        self.rev
    }

    fn sections(&self, _cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: GRID_TOP, focusable: false }, HEAD_GROUP));
        out.push(SectionSpec::new(Sec::Items, Kind::Grid { spec: grid_spec() }, GRID_GROUP)
            .seated(plx_ui::page_regions::CONTENT_HEAD_SEAT));
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend([Sec::Items, Sec::Head]);
    }

    fn cards<'a>(&'a self, _cx: &'a Cx<'_, H>, k: Sec) -> Option<Channels<'a>> {
        (k == Sec::Items && !self.channels.is_empty()).then_some(Channels { page: self })
    }

    fn caption_policy(&self, k: Sec) -> Option<plx_ui::caption_policy::CaptionPolicy> {
        (k == Sec::Items).then(crate::registry::captions::policy)
    }

    fn plain_group(&self, _cx: &Cx<'_, H>, _k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
            extent,
            len: 0,
            elem: ElemKind::Bare,
        }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        if k != Sec::Head {
            return;
        }
        // The row rides the page and leaves with the top chrome, as the Watchlist's does.
        let p = f.painter.alpha(f.page_alpha).alpha(self.handoff.head_alpha(r.y));
        self.header_row(f.measure).draw(p, r.y, &self.header_capsules, self.header_pop.scale_with(0, f.press.scale), None);
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

    pub const SHAPE: &'static str = "LiveTvScreen{entry:u32,channels:[{sid:u32,rk:str}],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Grid{bands,pop,landing}]},handoff:Handoff{tucked:bool,chrome:f32,capsule:f32}}";

    pub fn new(entry: EntryId) -> Self {
        Self {
            page: Page {
                entry,
                channels: Vec::new(),
                sig: 0,
                rev: 0,
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

    /// The channels the page shows, in order.
    pub fn channels(&self) -> &[Arc<PmsMovie>] {
        &self.page.channels
    }

    /// OK on the channel keyed `elem`.
    fn press(&self, elem: u32) -> Option<LiveTvOut> {
        let m = self.page.channel_of(elem)?;
        let (dvr, channel) = plx_data::pms::live_channel_of(m)?;
        Some(LiveTvOut::Tune { sid: m.sid, dvr: dvr.to_owned(), channel: channel.to_owned(), title: m.title.clone() })
    }

    fn tick<H: HomeLike>(&mut self, t: Tick, cx: &Cx<'_, H>) {
        self.page.sync(cx);
        self.page.header_row(cx.measure).step(self.page.entry, cx.focus.current, &mut self.page.header_capsules, &mut self.page.header_pop, t.dt());
        self.page.handoff.step(t.dt(), self.stack.scroll(), self.stack.target(), grid_spec().pitch(), false);
        let flat = [theme::SURFACE_APP; 4];
        if self.ground_seeded {
            self.ground.key_target(flat, t.dt());
        } else {
            self.ground.jump_target(flat);
            self.ground_seeded = true;
        }
    }

    fn route<H: HomeLike>(&mut self, out: LiveTvOut, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::LiveTv(out)));
        fx.invalidate(Provenance::Input);
    }

    /// Seat the arrival in the grid once it has a channel; until then seat nothing (the
    /// Watchlist's `seat_first`).
    fn seat_first<H: HomeLike>(&mut self, fx: &mut Effects<'_, H>) {
        let regions = self.page.regions();
        let want = regions.first_seat();
        let target = want.map(FocusTarget::FirstInGroupAnimated).or_else(|| regions.unseat());
        self.seat = (want != Some(GRID_GROUP)).then_some(want);
        if let Some(target) = target {
            self.seat_own(target, fx);
        }
    }

    fn follow_seat<H: HomeLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(seated) = self.seat else { return };
        if seated != self.page.regions().first_seat() {
            self.seat_first(fx);
        }
    }

    fn seat_own<H: HomeLike>(&mut self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        self.own_enters = self.own_enters.saturating_add(1);
        fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    /// BACK, decided by [`PageRegions::back`]: a scrolled page first returns to its top, on the
    /// first row in the column focus was in; from there it leaves for Home.
    fn back<H: HomeLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.page.entry);
        let regions = self.page.regions();
        let at = focus.and_then(|key| regions.region_of(key.elem, Focusable::<H>::group_of(self, &key.elem, cx)));
        match regions.back(at, self.stack.target() > 0.5) {
            BackStep::Top => {
                let column = focus
                    .and_then(|key| key.elem.checked_sub(FIRST_CARD_ELEM))
                    .map_or(0, |i| i as usize % COLS)
                    .min(self.page.channels.len().saturating_sub(1));
                self.seat_own(FocusTarget::Elem(FocusKey { entry: self.page.entry, elem: FIRST_CARD_ELEM + column as u32 }), fx);
            }
            BackStep::Content | BackStep::Header => self.seat_own(FocusTarget::ContainerGroup(GRID_GROUP), fx),
            BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => self.route(LiveTvOut::Back, fx),
        }
        Handled::Yes
    }

    /// A non-card element committed: the strip's pills and the chip.
    fn activate<H: HomeLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        match Self::strip_press(elem) {
            Some(Some(out)) => self.route(out, fx),
            Some(None) => {}
            None => return Handled::No,
        }
        Handled::Yes
    }

    /// A press on the shared strip: the other tabs and the chip are requests; the Live TV pill is
    /// the page the reader is already on.
    fn strip_press(elem: u32) -> Option<Option<LiveTvOut>> {
        use plx_ui::widgets::{
            TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_LIVETV, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST,
        };
        match elem {
            TAB_KEY_HOME => Some(Some(LiveTvOut::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(LiveTvOut::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(LiveTvOut::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(LiveTvOut::Tab(HomeTab::Search))),
            TAB_KEY_WATCHLIST => Some(Some(LiveTvOut::Tab(HomeTab::Watchlist))),
            TAB_KEY_CHIP => Some(Some(LiveTvOut::Account)),
            TAB_KEY_LIVETV => Some(None),
            _ => None,
        }
    }

    fn view<H: HomeLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    /// The page's top edge once the chrome has tucked away (the Watchlist's).
    fn draw_chrome<H: HomeLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let tuck = self.page.handoff.tuck();
        if tuck <= 0.0 {
            return;
        }
        let ground = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
        plx_ui::scroll_capsule::draw_top_fade(f.painter.alpha(f.page_alpha), SCR_W, ground, tuck, None);
    }
}

plx_ui::focusable_via_view!(LiveTvScreen, H: [HomeLike], view);

impl LogicalState for LiveTvScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.page.entry.0);
        c.seq(self.page.channels.len());
        for m in &self.page.channels {
            c.u32(u32::from(m.sid.raw())).str(&m.rk);
        }
        self.stack.write(c);
        self.page.handoff.write(c);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!("livetv channels={}", self.page.channels.len()));
    }
}

impl<H: HomeLike> Machine<H> for LiveTvScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
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
                        // The catalog is already here: take the channels before seating on them.
                        self.page.sync(cx);
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
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    if let Some(out) = self.press(elem) {
                        self.route(out, fx);
                    }
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    self.activate(key.elem, fx);
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

impl<H: HomeLike> Screen<H> for LiveTvScreen {
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
