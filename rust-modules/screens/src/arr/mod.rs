//! **Radarr and Sonarr integration screens** (external media library view).
//!
//! Features:
//! 1. **Grid Mode (Browsing)**:
//!    - Standard 6-column vertical poster grid matching `LibraryScreen` / `WatchlistScreen`.
//!    - Clean Header row at `y = 148.0` with screen title and filter pills.
//!    - Under focused poster: Title (bold) and Year/status (caption), matching Photo 1.
//!    - Hardware-smooth critically-damped `Spring` scrolling at 120 FPS.
//!    - Strict clipping below header.
//! 2. **Detail Mode (Single Click / OK)**:
//!    - Rich native detail view with fanart backdrop, scrim ramp, metadata tags,
//!      overview synopsis, and action buttons (Toggle Monitored, Trigger Search, Back).
//!    - Pressing Back / Esc returns smoothly to the grid with previous card focused.
//! 3. **Navigation & Key Handling**:
//!    - Full `PressCommit` support so pressing Enter on Navbar pills (Home, Movies, TV Shows, etc.)
//!      or Filter pills immediately activates without needing Esc.
//!
//! Follows PlxNative Sans-IO architecture: IO is performed via `AppFx::Preferences`,
//! and results arrive through addressed typed channels.

use std::borrow::Cow;

use plx_data::integrations::{ArrFilter, ArrKind, ArrMediaItem};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, GroupId, Handled,
    InputKind, Key, LogicalState, Machine,
};
use plx_machine::present::{PresentEvent, Provenance};
use plx_ui::cards::{CardEvent, CardSource, Grid, GridSpec, GRID_STYLE, TileLabel};
use plx_ui::consts::*;
use plx_ui::frame::Budget;
use plx_ui::landing_hero::base_scrim_ramp;
use plx_ui::screen::{
    At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
};
use plx_ui::theme;
use plx_ui::widgets::{self, Art, PageGround, StatusKind, StatusOverlay, Button, badge, BadgeStyle, CONTROL_GAP};
use plx_ui::{Rect, Spring, View};

use crate::registry::{AppFx, ContentReq, HomeTab, PreferenceCmd};

// Geometry Constants (6-column vertical grid matching Library / Watchlist)
pub const COLS: usize = 6;
pub const CARD_W: f32 = 250.0;
pub const CARD_H: f32 = 375.0;
pub const PITCH_X: f32 = 285.0; // gap = 35.0
pub const PITCH_Y: f32 = CARD_H + plx_ui::cards::LABEL_BAND_COLLAPSED + plx_ui::consts::UNDER_LABEL_AIR; // 415.0
pub const GRID_TOP: f32 = 244.0;
pub const HEADER_Y: f32 = 148.0;

// Header / Filter Pills
pub const FILTER_ALL_ELEM: u32 = 1;
pub const FILTER_MONITORED_ELEM: u32 = 2;
pub const FILTER_UNMONITORED_ELEM: u32 = 3;
pub const FILTER_MISSING_ELEM: u32 = 4;
pub const REFRESH_ELEM: u32 = 5;

// Backward-compat aliases
pub const HERO_BTN_MONITOR: u32 = FILTER_ALL_ELEM;
pub const HERO_BTN_SEARCH: u32 = FILTER_MONITORED_ELEM;
pub const HERO_BTN_FILTER: u32 = FILTER_UNMONITORED_ELEM;
pub const HERO_BTN_REFRESH: u32 = REFRESH_ELEM;

// Shelf / Grid Cards start here
pub const FIRST_CARD_ELEM: u32 = 0x1000;

// Detail Action Buttons
pub const DETAIL_BTN_MONITOR: u32 = 0x2001;
pub const DETAIL_BTN_SEARCH: u32 = 0x2002;

// Groups
pub const HEADER_GROUP: GroupId = GroupId(0);
pub const HERO_GROUP: GroupId = HEADER_GROUP; // alias
pub const GRID_GROUP: GroupId = GroupId(1);
pub const SHELF_GROUP: GroupId = GRID_GROUP; // alias
pub const DETAIL_GROUP: GroupId = GroupId(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrMode {
    Grid,
    Detail(usize), // index into self.items
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrScreenState {
    Loading,
    Ready,
    Failed,
}

pub struct ArrScreen {
    entry: EntryId,
    pub kind: ArrKind,
    pub items: Vec<ArrMediaItem>,
    pub filter: ArrFilter,
    pub mode: ArrMode,
    pub state: ArrScreenState,
    pub error_msg: String,

    pub featured_idx: usize,

    rx_items: Option<std::sync::mpsc::Receiver<Result<Vec<ArrMediaItem>, String>>>,
    rx_action: Option<std::sync::mpsc::Receiver<Result<bool, String>>>,
    rx_search: Option<std::sync::mpsc::Receiver<Result<String, String>>>,

    toast_msg: Option<(String, bool)>,
    toast_ticks: u32,

    pub scroll: Spring,
    pub scroll_target: f32,
    pub grid: Grid,
    pub focus_elem: Option<u32>,
    ground: PageGround,
}

pub struct ArrGridSource<'a> {
    pub kind: ArrKind,
    pub items: &'a [ArrMediaItem],
    pub filtered: &'a [usize],
}

impl<'a, H: plx_machine::machine::Host<Elem = u32>> CardSource<H> for ArrGridSource<'a> {
    fn len(&self) -> usize {
        self.filtered.len()
    }

    fn elem(&self, i: usize) -> u32 {
        FIRST_CARD_ELEM + i as u32
    }

    fn index_of(&self, e: &u32) -> Option<usize> {
        let elem = *e;
        if elem >= FIRST_CARD_ELEM && elem < plx_ui::dispatch::STRIP_BASE {
            let idx = (elem - FIRST_CARD_ELEM) as usize;
            if idx < self.filtered.len() {
                return Some(idx);
            }
        }
        None
    }

    fn art(&self, i: usize) -> Art<'_> {
        let item_idx = self.filtered[i];
        let item = &self.items[item_idx];
        let sid = if self.kind == ArrKind::Radarr { 0xFFE1 } else { 0xFFE2 };
        if !item.poster_url.is_empty() {
            Art::Thumb {
                sid,
                key: &item.poster_url,
                res: (250, 375),
            }
        } else {
            Art::Poster(None)
        }
    }

    fn label(&self, i: usize) -> TileLabel {
        let item_idx = self.filtered[i];
        let item = &self.items[item_idx];
        let title_c = std::ffi::CString::new(item.title.as_str()).ok();
        let status_str = if item.has_file {
            "Downloaded"
        } else if item.monitored {
            "Missing"
        } else {
            "Unmonitored"
        };
        let sub_text = if item.year > 0 {
            format!("{} · {}", item.year, status_str)
        } else {
            status_str.to_string()
        };
        let caption_c = std::ffi::CString::new(sub_text).ok();
        TileLabel {
            title: title_c,
            caption: caption_c,
            shadowed: true,
            within_tile: false,
            ..Default::default()
        }
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;

    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1} TB", b / TIB)
    } else if b >= GIB {
        format!("{:.1} GB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MB", b / MIB)
    } else {
        format!("{:.1} KB", b / KIB)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

impl ArrScreen {
    pub fn new(entry: EntryId, kind: ArrKind) -> Self {
        Self {
            entry,
            kind,
            items: Vec::new(),
            filter: ArrFilter::All,
            mode: ArrMode::Grid,
            state: ArrScreenState::Loading,
            error_msg: String::new(),
            featured_idx: 0,
            rx_items: None,
            rx_action: None,
            rx_search: None,
            toast_msg: None,
            toast_ticks: 0,
            scroll: Spring::at(0.0),
            scroll_target: 0.0,
            grid: Grid::new(entry, GridSpec::new(GRID_TOP, 0.0).columns(COLS, GRID_STYLE, MARGIN_X).external()),
            focus_elem: Some(FIRST_CARD_ELEM),
            ground: PageGround::new(),
        }
    }

    pub fn with_items(entry: EntryId, kind: ArrKind, items: Vec<ArrMediaItem>) -> Self {
        let mut screen = Self::new(entry, kind);
        screen.items = items;
        screen.state = ArrScreenState::Ready;
        screen
    }

    pub fn max_scroll(&self) -> f32 {
        let n = self.filtered_indices().len();
        if n == 0 {
            return 0.0;
        }
        let rows = n.div_ceil(COLS);
        let doc_bottom = GRID_TOP + rows as f32 * PITCH_Y;
        (doc_bottom - (SCR_H - MARGIN_Y)).max(0.0)
    }

    pub fn row_reveal(&self, row: usize) -> f32 {
        let max = self.max_scroll();
        let growth = plx_ui::cards::grid_growth_before_for(
            self.grid.band_h(),
            row,
            &plx_ui::cards::grid_settled(Some(row)),
        );
        plx_ui::cards::grid_rest_scroll(row, PITCH_Y, growth, max)
    }

    pub fn grid_source<'a>(&'a self, filtered: &'a [usize]) -> ArrGridSource<'a> {
        Self::grid_source_from(self.kind, &self.items, filtered)
    }

    pub fn grid_source_from<'a>(
        kind: ArrKind,
        items: &'a [ArrMediaItem],
        filtered: &'a [usize],
    ) -> ArrGridSource<'a> {
        ArrGridSource {
            kind,
            items,
            filtered,
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| match self.filter {
                ArrFilter::All => true,
                ArrFilter::Monitored => item.monitored,
                ArrFilter::Unmonitored => !item.monitored,
                ArrFilter::Missing => item.monitored && !item.has_file,
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut total = 0;
        let mut monitored = 0;
        let mut unmonitored = 0;
        let mut missing = 0;
        for item in &self.items {
            total += 1;
            if item.monitored {
                monitored += 1;
                if !item.has_file {
                    missing += 1;
                }
            } else {
                unmonitored += 1;
            }
        }
        (total, monitored, unmonitored, missing)
    }

    pub fn header_pill_rect(&self, elem: u32) -> Rect {
        let pills: [(u32, f32); 5] = [
            (FILTER_ALL_ELEM, 140.0),
            (FILTER_MONITORED_ELEM, 220.0),
            (FILTER_UNMONITORED_ELEM, 240.0),
            (FILTER_MISSING_ELEM, 195.0),
            (REFRESH_ELEM, 155.0),
        ];

        let right_x = MARGIN_X + 5.0 * PITCH_X + CARD_W; // 1749.0
        let total_w: f32 = pills.iter().map(|(_, w)| *w).sum::<f32>() + 4.0 * 14.0;
        let mut cur_x = right_x - total_w;

        for (btn_elem, w) in pills {
            let r = Rect::new(cur_x, HEADER_Y + 2.0, w, 40.0);
            if btn_elem == elem {
                return r;
            }
            cur_x += w + 14.0;
        }

        Rect::new(MARGIN_X, HEADER_Y, 140.0, 40.0)
    }

    pub fn detail_button_info(monitored: bool) -> [(&'static str, plx_ui::icons::Icon, u32); 2] {
        let (mon_label, mon_icon) = if monitored {
            ("Monitored", plx_ui::icons::Icon::Check)
        } else {
            ("Monitor", plx_ui::icons::Icon::Bookmark)
        };
        [
            (mon_label, mon_icon, DETAIL_BTN_MONITOR),
            ("Search Now", plx_ui::icons::Icon::Search, DETAIL_BTN_SEARCH),
        ]
    }

    pub fn detail_button_rect(&self, elem: u32) -> Rect {
        let start_x = 360.0;
        let y = 450.0;
        let h = plx_ui::widgets::StatusOverlay::CTRL_H;
        let monitored = if let ArrMode::Detail(idx) = self.mode {
            self.items.get(idx).map_or(false, |it| it.monitored)
        } else {
            false
        };
        let buttons = Self::detail_button_info(monitored);
        let mut cur_x = start_x;
        for (label, _icon, btn_elem) in buttons {
            let c_label = std::ffi::CString::new(label).unwrap_or_default();
            let w = Button::pill_w(c_label.as_ptr(), theme::size::BODY, true).max(180.0);
            let rect = Rect::new(cur_x, y, w, h);
            if btn_elem == elem {
                return rect;
            }
            cur_x += w + CONTROL_GAP;
        }
        Rect::new(start_x, y, 200.0, h)
    }

    pub fn hero_button_rect(&self, elem: u32) -> Rect {
        self.header_pill_rect(elem)
    }

    pub fn key(&self, elem: u32) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem,
        }
    }

    pub fn request_library<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>>(
        &mut self,
        fx: &mut Effects<'_, H>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.rx_items = Some(rx);
        self.state = ArrScreenState::Loading;
        fx.push(plx_machine::machine::Fx::App(AppFx::Preferences(
            PreferenceCmd::FetchArrLibrary {
                service: self.kind,
                reply: tx,
            },
        )));
        fx.invalidate(Provenance::Input);
    }

    pub fn toggle_monitored<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>>(
        &mut self,
        item_idx: usize,
        fx: &mut Effects<'_, H>,
    ) {
        if let Some(item) = self.items.get_mut(item_idx) {
            let new_state = !item.monitored;
            item.monitored = new_state;
            let item_id = item.id;
            let title = item.title.clone();

            let (tx, rx) = std::sync::mpsc::channel();
            self.rx_action = Some(rx);

            let status_txt = if new_state { "Monitored" } else { "Unmonitored" };
            self.toast_msg = Some((format!("'{title}' is now {status_txt}"), true));
            self.toast_ticks = 180;

            fx.push(plx_machine::machine::Fx::App(AppFx::Preferences(
                PreferenceCmd::ToggleArrMonitored {
                    service: self.kind,
                    id: item_id,
                    monitored: new_state,
                    reply: tx,
                },
            )));
            fx.invalidate(Provenance::Input);
        }
    }

    pub fn trigger_search<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>>(
        &mut self,
        item_idx: usize,
        fx: &mut Effects<'_, H>,
    ) {
        if let Some(item) = self.items.get(item_idx) {
            let item_id = item.id;
            let title = item.title.clone();

            let (tx, rx) = std::sync::mpsc::channel();
            self.rx_search = Some(rx);

            self.toast_msg = Some((format!("Search initiated for '{title}'"), true));
            self.toast_ticks = 180;

            fx.push(plx_machine::machine::Fx::App(AppFx::Preferences(
                PreferenceCmd::TriggerArrSearch {
                    service: self.kind,
                    id: item_id,
                    reply: tx,
                },
            )));
            fx.invalidate(Provenance::Input);
        }
    }

    fn activate_elem<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>>(
        &mut self,
        elem: u32,
        _cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        // 1. Top navbar keys (Home, Movies, TV Shows, Watchlist, Radarr, Sonarr, Search, Chip)
        if elem >= plx_ui::dispatch::STRIP_BASE {
            use plx_ui::widgets::{
                TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_RADARR,
                TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_SONARR, TAB_KEY_WATCHLIST,
            };
            let out = match elem {
                TAB_KEY_HOME => Some(ContentReq::Tab(HomeTab::Home)),
                TAB_KEY_MOVIES => Some(ContentReq::Tab(HomeTab::Movies)),
                TAB_KEY_SHOWS => Some(ContentReq::Tab(HomeTab::Shows)),
                TAB_KEY_WATCHLIST => Some(ContentReq::Tab(HomeTab::Watchlist)),
                TAB_KEY_SEARCH => Some(ContentReq::Tab(HomeTab::Search)),
                TAB_KEY_RADARR => Some(ContentReq::Tab(HomeTab::Radarr)),
                TAB_KEY_SONARR => Some(ContentReq::Tab(HomeTab::Sonarr)),
                TAB_KEY_CHIP => Some(ContentReq::Account),
                _ => None,
            };
            if let Some(req) = out {
                fx.push(plx_machine::machine::Fx::App(AppFx::Content(req)));
                return Handled::Yes;
            }
        }

        match self.mode {
            ArrMode::Grid => match elem {
                FILTER_ALL_ELEM => {
                    self.filter = ArrFilter::All;
                    self.grid.reset_bands();
                    self.scroll_target = 0.0;
                    self.scroll.jump(0.0);
                    self.grid.restore_scroll(0.0);
                    self.grid.set_page(GRID_TOP, 0.0);
                    self.grid.arrive();
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                }
                FILTER_MONITORED_ELEM => {
                    self.filter = ArrFilter::Monitored;
                    self.grid.reset_bands();
                    self.scroll_target = 0.0;
                    self.scroll.jump(0.0);
                    self.grid.restore_scroll(0.0);
                    self.grid.set_page(GRID_TOP, 0.0);
                    self.grid.arrive();
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                }
                FILTER_UNMONITORED_ELEM => {
                    self.filter = ArrFilter::Unmonitored;
                    self.grid.reset_bands();
                    self.scroll_target = 0.0;
                    self.scroll.jump(0.0);
                    self.grid.restore_scroll(0.0);
                    self.grid.set_page(GRID_TOP, 0.0);
                    self.grid.arrive();
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                }
                FILTER_MISSING_ELEM => {
                    self.filter = ArrFilter::Missing;
                    self.grid.reset_bands();
                    self.scroll_target = 0.0;
                    self.scroll.jump(0.0);
                    self.grid.restore_scroll(0.0);
                    self.grid.set_page(GRID_TOP, 0.0);
                    self.grid.arrive();
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                }
                REFRESH_ELEM => {
                    self.request_library(fx);
                    Handled::Yes
                }
                card_elem if card_elem >= FIRST_CARD_ELEM && card_elem < plx_ui::dispatch::STRIP_BASE => {
                    let filtered = self.filtered_indices();
                    let card_idx = (card_elem - FIRST_CARD_ELEM) as usize;
                    if let Some(&item_idx) = filtered.get(card_idx) {
                        self.mode = ArrMode::Detail(item_idx);
                        self.featured_idx = item_idx;
                        self.focus_elem = Some(DETAIL_BTN_MONITOR);
                        fx.invalidate(Provenance::Input);
                    }
                    Handled::Yes
                }
                _ => Handled::No,
            },
            ArrMode::Detail(item_idx) => match elem {
                DETAIL_BTN_MONITOR => {
                    self.toggle_monitored(item_idx, fx);
                    Handled::Yes
                }
                DETAIL_BTN_SEARCH => {
                    self.trigger_search(item_idx, fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
        }
    }
}

impl LogicalState for ArrScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.kind as u32);
        c.u32(self.filter as u32);
        c.u32(self.items.len() as u32);
        match self.mode {
            ArrMode::Grid => {
                c.u32(0);
                c.f32(self.scroll.pos).f32(self.scroll.vel).f32(self.scroll_target);
            }
            ArrMode::Detail(idx) => {
                c.u32(1);
                c.u32(idx as u32);
            }
        }
        self.grid.write(c);
    }

    fn probe(&self, out: &mut String) {
        out.push_str("arr_screen");
    }
}

impl<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>> Focusable<H> for ArrScreen {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        match self.mode {
            ArrMode::Grid => {
                out.push(GroupSpec {
                    id: HEADER_GROUP,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::First,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(MARGIN_X, HEADER_Y, 1771.0 - MARGIN_X, 44.0),
                    len: 5,
                    elem: ElemKind::Control,
                });
                let filtered = self.filtered_indices();
                if !filtered.is_empty() {
                    out.push(self.grid.group_spec(GRID_GROUP, filtered.len()));
                }
            }
            ArrMode::Detail(_) => {
                out.push(GroupSpec {
                    id: DETAIL_GROUP,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::First,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(360.0, 450.0, 600.0, plx_ui::widgets::StatusOverlay::CTRL_H),
                    len: 2,
                    elem: ElemKind::Control,
                });
            }
        }
    }

    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        let elem = *elem;
        match self.mode {
            ArrMode::Grid => {
                if elem >= FILTER_ALL_ELEM && elem <= REFRESH_ELEM {
                    Some(HEADER_GROUP)
                } else if elem >= FIRST_CARD_ELEM && elem < plx_ui::dispatch::STRIP_BASE {
                    Some(GRID_GROUP)
                } else {
                    None
                }
            }
            ArrMode::Detail(_) => {
                if elem >= DETAIL_BTN_MONITOR && elem <= DETAIL_BTN_SEARCH {
                    Some(DETAIL_GROUP)
                } else {
                    None
                }
            }
        }
    }

    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let elem = *elem;
        match self.mode {
            ArrMode::Grid => {
                if elem >= FILTER_ALL_ELEM && elem <= REFRESH_ELEM {
                    let r = self.header_pill_rect(elem);
                    return Some(Placed {
                        rect: r,
                        rest_rect: r,
                        clip: Rect::FULL,
                        index: Some(elem - FILTER_ALL_ELEM),
                    });
                }
                if elem >= FIRST_CARD_ELEM && elem < plx_ui::dispatch::STRIP_BASE {
                    let filtered = self.filtered_indices();
                    let src = self.grid_source(&filtered);
                    return self.grid.place(cx, &src, &elem, at);
                }
            }
            ArrMode::Detail(_) => {
                if elem >= DETAIL_BTN_MONITOR && elem <= DETAIL_BTN_SEARCH {
                    let r = self.detail_button_rect(elem);
                    return Some(Placed {
                        rect: r,
                        rest_rect: r,
                        clip: Rect::FULL,
                        index: Some(elem - DETAIL_BTN_MONITOR),
                    });
                }
            }
        }
        None
    }

    fn neighbour(&self, from: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let elem = from.elem;
        match self.mode {
            ArrMode::Grid => {
                if elem >= FILTER_ALL_ELEM && elem <= REFRESH_ELEM {
                    match dir {
                        Dir::Left => {
                            if elem > FILTER_ALL_ELEM {
                                Step::Move(self.key(elem - 1))
                            } else {
                                Step::Edge
                            }
                        }
                        Dir::Right => {
                            if elem < REFRESH_ELEM {
                                Step::Move(self.key(elem + 1))
                            } else {
                                Step::Edge
                            }
                        }
                        Dir::Up => Step::Edge,
                        Dir::Down => Step::Edge,
                    }
                } else if elem >= FIRST_CARD_ELEM && elem < plx_ui::dispatch::STRIP_BASE {
                    let filtered = self.filtered_indices();
                    let src = self.grid_source(&filtered);
                    self.grid.neighbour::<H, _>(&src, from, dir)
                } else {
                    Step::Edge
                }
            }
            ArrMode::Detail(_) => {
                if elem >= DETAIL_BTN_MONITOR && elem <= DETAIL_BTN_SEARCH {
                    match dir {
                        Dir::Left => {
                            if elem > DETAIL_BTN_MONITOR {
                                Step::Move(self.key(elem - 1))
                            } else {
                                Step::Edge
                            }
                        }
                        Dir::Right => {
                            if elem < DETAIL_BTN_SEARCH {
                                Step::Move(self.key(elem + 1))
                            } else {
                                Step::Edge
                            }
                        }
                        Dir::Up => Step::Edge,
                        Dir::Down => Step::Edge,
                    }
                } else {
                    Step::Edge
                }
            }
        }
    }

    fn seat(&self, g: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        match self.mode {
            ArrMode::Grid => {
                if g == HEADER_GROUP {
                    let target = match self.filter {
                        ArrFilter::All => FILTER_ALL_ELEM,
                        ArrFilter::Monitored => FILTER_MONITORED_ELEM,
                        ArrFilter::Unmonitored => FILTER_UNMONITORED_ELEM,
                        ArrFilter::Missing => FILTER_MISSING_ELEM,
                    };
                    self.key(target)
                } else if g == GRID_GROUP {
                    let filtered = self.filtered_indices();
                    let src = self.grid_source(&filtered);
                    self.grid.seat::<H, _>(&src, from).unwrap_or_else(|| self.key(FIRST_CARD_ELEM))
                } else {
                    self.key(FILTER_ALL_ELEM)
                }
            }
            ArrMode::Detail(_) => {
                if let Some(elem) = self.focus_elem {
                    if elem >= DETAIL_BTN_MONITOR && elem <= DETAIL_BTN_SEARCH {
                        return self.key(elem);
                    }
                }
                self.key(DETAIL_BTN_MONITOR)
            }
        }
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        match self.mode {
            ArrMode::Grid => {
                if want.elem >= FILTER_ALL_ELEM && want.elem <= REFRESH_ELEM {
                    want
                } else if want.elem >= FIRST_CARD_ELEM && want.elem < plx_ui::dispatch::STRIP_BASE {
                    let card_idx = (want.elem - FIRST_CARD_ELEM) as usize;
                    let filtered = self.filtered_indices();
                    if card_idx < filtered.len() {
                        want
                    } else if !filtered.is_empty() {
                        self.key(FIRST_CARD_ELEM)
                    } else {
                        self.key(FILTER_ALL_ELEM)
                    }
                } else {
                    want
                }
            }
            ArrMode::Detail(_) => {
                if want.elem >= DETAIL_BTN_MONITOR && want.elem <= DETAIL_BTN_SEARCH {
                    want
                } else if want.elem >= plx_ui::dispatch::STRIP_BASE {
                    want
                } else if let Some(focus) = self.focus_elem {
                    self.key(focus)
                } else {
                    self.key(DETAIL_BTN_MONITOR)
                }
            }
        }
    }
}

impl<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>> Machine<H> for ArrScreen {
    type Ev = ScreenEvent<H>;

    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        match ev {
            ScreenEvent::Enter(_) => {
                self.request_library(fx);
                if self.mode == ArrMode::Grid {
                    let filtered = self.filtered_indices();
                    let src = Self::grid_source_from(self.kind, &self.items, &filtered);
                    let _ = self.grid.on(ev, cx, &src, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Tick(_tick) => {
                if let Some(rx) = &self.rx_items {
                    if let Ok(result) = rx.try_recv() {
                        match result {
                            Ok(items) => {
                                self.items = items;
                                self.state = ArrScreenState::Ready;
                                self.error_msg.clear();
                            }
                            Err(e) => {
                                self.state = ArrScreenState::Failed;
                                self.error_msg = e;
                            }
                        }
                        self.rx_items = None;
                        fx.invalidate(Provenance::Input);
                    }
                }

                if let Some(rx) = &self.rx_action {
                    if let Ok(result) = rx.try_recv() {
                        match result {
                            Ok(_) => {}
                            Err(e) => {
                                self.toast_msg = Some((format!("Action failed: {e}"), false));
                                self.toast_ticks = 240;
                            }
                        }
                        self.rx_action = None;
                        fx.invalidate(Provenance::Input);
                    }
                }

                if let Some(rx) = &self.rx_search {
                    if let Ok(result) = rx.try_recv() {
                        match result {
                            Ok(_) => {}
                            Err(e) => {
                                self.toast_msg = Some((format!("Search failed: {e}"), false));
                                self.toast_ticks = 240;
                            }
                        }
                        self.rx_search = None;
                        fx.invalidate(Provenance::Input);
                    }
                }

                if self.toast_ticks > 0 {
                    self.toast_ticks -= 1;
                    if self.toast_ticks == 0 {
                        self.toast_msg = None;
                        fx.invalidate(Provenance::Input);
                    }
                }

                if self.mode == ArrMode::Grid {
                    let dt = _tick.dt();
                    self.scroll.step(self.scroll_target, K_SCROLL, dt);
                    self.grid.set_page(GRID_TOP, self.scroll.pos);
                    if (self.scroll.pos - self.scroll_target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
                        fx.note(PresentEvent::Motion);
                    }
                    let filtered = self.filtered_indices();
                    let src = Self::grid_source_from(self.kind, &self.items, &filtered);
                    if let Some(card_ev) = self.grid.on(ev, cx, &src, fx) {
                        match card_ev {
                            CardEvent::Activate(elem) => {
                                self.activate_elem(elem, cx, fx);
                            }
                            CardEvent::Hold(elem) => {
                                let card_idx = (elem - FIRST_CARD_ELEM) as usize;
                                if let Some(&item_idx) = filtered.get(card_idx) {
                                    self.trigger_search(item_idx, fx);
                                }
                            }
                            _ => {}
                        }
                    }
                }

                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.focus_elem = Some(to.elem);
                if self.mode == ArrMode::Grid {
                    if to.elem >= FIRST_CARD_ELEM && to.elem < plx_ui::dispatch::STRIP_BASE {
                        let card_idx = (to.elem - FIRST_CARD_ELEM) as usize;
                        let filtered = self.filtered_indices();
                        if let Some(&item_idx) = filtered.get(card_idx) {
                            self.featured_idx = item_idx;
                        }
                        let row = card_idx / COLS;
                        self.scroll_target = self.row_reveal(row);
                    } else {
                        self.scroll_target = 0.0;
                    }
                    self.grid.set_page(GRID_TOP, self.scroll.pos);
                    let filtered = self.filtered_indices();
                    let src = Self::grid_source_from(self.kind, &self.items, &filtered);
                    let _ = self.grid.on(ev, cx, &src, fx);
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if let Some(focus) = cx.focus.current {
                    if focus.entry == self.entry {
                        if self.mode == ArrMode::Grid && focus.elem >= FIRST_CARD_ELEM && focus.elem < plx_ui::dispatch::STRIP_BASE {
                            let card_idx = (focus.elem - FIRST_CARD_ELEM) as usize;
                            let filtered = self.filtered_indices();
                            if let Some(&item_idx) = filtered.get(card_idx) {
                                self.trigger_search(item_idx, fx);
                                return Handled::Yes;
                            }
                        }
                    }
                }
                Handled::No
            }
            // A click from mouse/pointer
            ScreenEvent::Activate(elem) => {
                self.activate_elem(*elem, cx, fx)
            }
            // A committed press from keyboard / remote control Enter/OK key!
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate_elem(key.elem, cx, fx)
                } else if let Some(elem) = self.focus_elem {
                    self.activate_elem(elem, cx, fx)
                } else {
                    Handled::No
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Wheel { dy } = input.kind {
                    if self.mode == ArrMode::Grid && dy.is_finite() {
                        self.scroll_target = (self.scroll_target - dy * PITCH_Y).clamp(0.0, self.max_scroll());
                        fx.invalidate(Provenance::Input);
                        return Handled::Yes;
                    }
                }
                if let InputKind::Key { key: Key::Back, edge: Edge::Down, .. } = input.kind {
                    match self.mode {
                        ArrMode::Detail(item_idx) => {
                            self.mode = ArrMode::Grid;
                            let filtered = self.filtered_indices();
                            let target_card = filtered.iter().position(|&idx| idx == item_idx).unwrap_or(0);
                            self.focus_elem = Some(FIRST_CARD_ELEM + target_card as u32);
                            let row = target_card / COLS;
                            self.scroll_target = self.row_reveal(row);
                            self.grid.set_page(GRID_TOP, self.scroll.pos);
                            fx.invalidate(Provenance::Input);
                            Handled::Yes
                        }
                        ArrMode::Grid => {
                            fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Back)));
                            Handled::Yes
                        }
                    }
                } else {
                    Handled::No
                }
            }
            _ => Handled::No,
        }
    }
}

impl<H: plx_machine::machine::Host<Elem = u32, Fx = AppFx>> Screen<H> for ArrScreen {
    fn name(&self) -> &'static str {
        match self.kind {
            ArrKind::Radarr => "radarr",
            ArrKind::Sonarr => "sonarr",
        }
    }

    fn state(&self) -> &dyn LogicalState {
        self
    }

    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }

    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}

    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }

    fn strip_reachable(&self) -> bool {
        true
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn links(&self, out: &mut Vec<Link>) {
        match self.mode {
            ArrMode::Grid => {
                out.push(Link {
                    from: plx_ui::containers::tabs::STRIP,
                    dir: Dir::Down,
                    to: HEADER_GROUP,
                });
                out.push(Link {
                    from: HEADER_GROUP,
                    dir: Dir::Up,
                    to: plx_ui::containers::tabs::STRIP,
                });
                out.push(Link {
                    from: HEADER_GROUP,
                    dir: Dir::Down,
                    to: GRID_GROUP,
                });
                out.push(Link {
                    from: GRID_GROUP,
                    dir: Dir::Up,
                    to: HEADER_GROUP,
                });
            }
            ArrMode::Detail(_) => {
                out.push(Link {
                    from: plx_ui::containers::tabs::STRIP,
                    dir: Dir::Down,
                    to: DETAIL_GROUP,
                });
                out.push(Link {
                    from: DETAIL_GROUP,
                    dir: Dir::Up,
                    to: plx_ui::containers::tabs::STRIP,
                });
            }
        }
    }

    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter;
        self.ground.draw(p, Rect::FULL);

        let sid = if self.kind == ArrKind::Radarr { 0xFFE1 } else { 0xFFE2 };
        let service_name = match self.kind {
            ArrKind::Radarr => "Radarr",
            ArrKind::Sonarr => "Sonarr",
        };

        // 1. Draw Content based on State
        match self.state {
            ArrScreenState::Loading => {
                let rect = Rect::new(0.0, 300.0, SCR_W, 300.0);
                StatusOverlay::new(rect, c"Loading library...", StatusKind::Working).draw(&plx_ui::Env::inert(), p);
            }
            ArrScreenState::Failed => {
                let rect = Rect::new(0.0, 300.0, SCR_W, 300.0);
                let error_c = std::ffi::CString::new(self.error_msg.as_str()).unwrap_or_default();
                StatusOverlay::new(rect, c"Could not load library", StatusKind::Failed)
                    .reason(error_c.as_c_str())
                    .draw(&plx_ui::Env::inert(), p);
            }
            ArrScreenState::Ready => {
                match self.mode {
                    ArrMode::Grid => {
                        // 1. Draw Grid first!
                        let filtered = self.filtered_indices();
                        if filtered.is_empty() {
                            let rect = Rect::new(MARGIN_X, GRID_TOP + 40.0, 600.0, 100.0);
                            let empty_msg = format!("No items found under current filter in {service_name}.");
                            let empty_c = std::ffi::CString::new(empty_msg).unwrap_or_default();
                            StatusOverlay::new(rect, empty_c.as_c_str(), StatusKind::Empty).draw(&plx_ui::Env::inert(), p);
                        } else {
                            self.grid.set_page(GRID_TOP, self.scroll.pos);
                            let src = self.grid_source(&filtered);
                            self.grid.draw(f, p, &src);
                        }

                        // 2. Top Fade wash: when scrolled, cards fade out smoothly under the header
                        if self.scroll.pos > 0.0 {
                            let tuck = (self.scroll.pos / 100.0).clamp(0.0, 1.0);
                            let ground_col = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
                            let solid_top = [ground_col[0], ground_col[1], ground_col[2], 0.98 * tuck];
                            let fade_bot = [ground_col[0], ground_col[1], ground_col[2], 0.0];
                            p.rect(Rect::new(0.0, 0.0, SCR_W, 210.0), 0.0, solid_top, fade_bot, 0.0);
                        }

                        // 3. Draw Header Row ON TOP of the grid at y = 148
                        let (tot, mon, unmon, miss) = self.counts();
                        let header_title = format!("{service_name} ({tot})");
                        if let Ok(c_htitle) = std::ffi::CString::new(header_title) {
                            plx_ui::label::Label::new(
                                c_htitle.as_ptr(),
                                theme::size::TITLE,
                                theme::TEXT_HEADING,
                            )
                            .bold()
                            .draw(p, Rect::new(MARGIN_X, HEADER_Y + 4.0, 350.0, 36.0));
                        }

                        // Filter Pills on the right with proper spacious padding and size::CAPTION
                        let pills: [(u32, String, bool); 5] = [
                            (FILTER_ALL_ELEM, format!("All ({tot})"), self.filter == ArrFilter::All),
                            (FILTER_MONITORED_ELEM, format!("Monitored ({mon})"), self.filter == ArrFilter::Monitored),
                            (FILTER_UNMONITORED_ELEM, format!("Unmonitored ({unmon})"), self.filter == ArrFilter::Unmonitored),
                            (FILTER_MISSING_ELEM, format!("Missing ({miss})"), self.filter == ArrFilter::Missing),
                            (REFRESH_ELEM, "↺ Refresh".to_string(), false),
                        ];

                        for (btn_elem, label_str, is_active) in pills {
                            let r = self.header_pill_rect(btn_elem);
                            let is_focused = self.focus_elem == Some(btn_elem);

                            let bg_color = if is_focused {
                                [1.0, 1.0, 1.0, 0.95]
                            } else if is_active {
                                [0.28, 0.32, 0.40, 0.92]
                            } else {
                                [0.18, 0.20, 0.24, 0.75]
                            };

                            let text_color = if is_focused {
                                [0.08, 0.08, 0.10, 1.0]
                            } else if is_active {
                                theme::TEXT_HEADING
                            } else {
                                [0.75, 0.78, 0.82, 0.95]
                            };

                            if is_focused {
                                let focus_rect = widgets::focus_pill(r);
                                p.rrect(focus_rect, 22.0, 22.0, [1.0, 1.0, 1.0, 0.35]);
                            }
                            p.rrect(r, 20.0, 20.0, bg_color);
                            if !is_focused {
                                let rim_col = if is_active {
                                    [1.0, 1.0, 1.0, 0.32]
                                } else {
                                    [1.0, 1.0, 1.0, 0.15]
                                };
                                p.rring(r, 20.0, 1.0, rim_col);
                            }

                            if let Ok(c_label) = std::ffi::CString::new(label_str) {
                                plx_ui::label::Label::new(
                                    c_label.as_ptr(),
                                    theme::size::CAPTION,
                                    text_color,
                                )
                                .bold()
                                .h(plx_ui::label::HAlign::Center)
                                .v(plx_ui::label::VAlign::Middle)
                                .draw(p, r);
                            }
                        }
                    }
                    ArrMode::Detail(item_idx) => {
                        // ── Item Detail View (Two-Column Cinematic Hero) ──
                        if let Some(item) = self.items.get(item_idx) {
                            let fanart_key = if !item.fanart_url.is_empty() {
                                &item.fanart_url
                            } else if !item.poster_url.is_empty() {
                                &item.poster_url
                            } else {
                                ""
                            };

                            // 1. Full-Bleed Fanart Backdrop
                            if !fanart_key.is_empty() {
                                widgets::card(
                                    p,
                                    Rect::new(0.0, 0.0, SCR_W, 850.0),
                                    Art::Thumb {
                                        sid,
                                        key: fanart_key,
                                        res: (1920, 1080),
                                    },
                                    0.0,
                                    false,
                                    1.0,
                                    0.0,
                                );
                            }

                            // 2. Scrim Gradient Ramp & Solid Ground Cover
                            let [y0, knee, mid, foot] = base_scrim_ramp(1.0);
                            p.rect(Rect::new(0.0, y0, SCR_W, knee - y0), 0.0, theme::scrim(0.0), theme::scrim(mid), 0.0);
                            p.rect(Rect::new(0.0, knee, SCR_W, SCR_H - knee), 0.0, theme::scrim(mid), theme::scrim(foot), 0.0);
                            widgets::hero_scrim(p, 1.0, false);
                            p.rect(Rect::new(0.0, 520.0, SCR_W, SCR_H - 520.0), 0.0, [0.08, 0.09, 0.11, 0.92], [0.08, 0.09, 0.11, 0.98], 0.0);

                            // 3. Left Column: Portrait Poster Card (240 x 360)
                            let poster_rect = Rect::new(MARGIN_X, 165.0, 240.0, 360.0);
                            p.rrect(Rect::new(MARGIN_X + 4.0, 169.0, 240.0, 360.0), 12.0, 12.0, [0.0, 0.0, 0.0, 0.60]);
                            p.rrect(poster_rect, 12.0, 12.0, [0.12, 0.14, 0.18, 0.90]);
                            if !item.poster_url.is_empty() {
                                widgets::card(
                                    p,
                                    poster_rect,
                                    Art::Thumb {
                                        sid,
                                        key: &item.poster_url,
                                        res: (360, 540),
                                    },
                                    12.0,
                                    false,
                                    1.0,
                                    0.0,
                                );
                            }
                            p.rring(poster_rect, 12.0, 1.0, [1.0, 1.0, 1.0, 0.20]);

                            // 4. Right Column: Title (at x = 360, y = 165)
                            let title_rect = Rect::new(360.0, 165.0, 1400.0, 50.0);
                            let elided_title = plx_gfx::text::elide_by(&item.title, 1400.0, false, |t| {
                                f.measure.width_str(t, theme::size::HERO, true)
                            });
                            if let Ok(c_title) = std::ffi::CString::new(elided_title.as_bytes()) {
                                plx_ui::label::Label::new(
                                    c_title.as_ptr(),
                                    theme::size::HERO,
                                    theme::TEXT_HEADING,
                                )
                                .bold()
                                .draw(p, title_rect);
                            }

                            // 5. Facts Line (at y = 226)
                            let mut facts: Vec<String> = Vec::new();
                            if item.year > 0 {
                                facts.push(item.year.to_string());
                            }
                            facts.push(service_name.to_string());
                            if !item.status.is_empty() {
                                facts.push(capitalize(&item.status));
                            }
                            if item.size_bytes > 0 {
                                facts.push(format_file_size(item.size_bytes));
                            }
                            if !item.quality_profile_name.is_empty() {
                                facts.push(item.quality_profile_name.clone());
                            }
                            let facts_str = facts.join("  ·  ");
                            if let Ok(c_facts) = std::ffi::CString::new(facts_str) {
                                plx_ui::label::Label::new(
                                    c_facts.as_ptr(),
                                    theme::size::CAPTION,
                                    theme::TEXT_SECONDARY,
                                )
                                .draw(p, Rect::new(360.0, 226.0, 1400.0, 26.0));
                            }

                            // 6. Status Badges Row (at y = 268)
                            let badges_y = 268.0;
                            let mut cur_badge_x = 360.0;

                            // Service badge (RADARR / SONARR)
                            let srv_text = if self.kind == ArrKind::Radarr { "RADARR" } else { "SONARR" };
                            let srv_w = badge(
                                p,
                                cur_badge_x,
                                badges_y,
                                srv_text,
                                None,
                                BadgeStyle::OverArt,
                                f.measure,
                            );
                            cur_badge_x += srv_w + theme::space::SM;

                            // Monitored badge
                            let (mon_text, mon_icon) = if item.monitored {
                                ("Monitored", Some(plx_ui::icons::Icon::Check))
                            } else {
                                ("Unmonitored", None)
                            };
                            let mon_w = badge(
                                p,
                                cur_badge_x,
                                badges_y,
                                mon_text,
                                mon_icon,
                                BadgeStyle::Filled,
                                f.measure,
                            );
                            cur_badge_x += mon_w + theme::space::SM;

                            // File status badge
                            let file_str = if item.has_file {
                                if self.kind == ArrKind::Sonarr && item.total_episode_count > 0 {
                                    format!("Downloaded ({}/{})", item.episode_count, item.total_episode_count)
                                } else {
                                    "Downloaded".to_string()
                                }
                            } else if item.monitored {
                                if self.kind == ArrKind::Sonarr && item.total_episode_count > 0 {
                                    format!("Missing (0/{})", item.total_episode_count)
                                } else {
                                    "Missing".to_string()
                                }
                            } else {
                                "No File".to_string()
                            };

                            let _file_w = badge(
                                p,
                                cur_badge_x,
                                badges_y,
                                &file_str,
                                None,
                                BadgeStyle::Filled,
                                f.measure,
                            );

                            // 7. Overview / Synopsis (at y = 308)
                            let syn_w = 1400.0;
                            let syn_text = if !item.overview.is_empty() {
                                &item.overview
                            } else {
                                "No description available."
                            };
                            let elided_syn = plx_gfx::text::elide_by(syn_text, syn_w * 2.8, true, |t| {
                                f.measure.width_str(t, theme::size::BODY, false)
                            });
                            if let Ok(c_syn) = std::ffi::CString::new(elided_syn.as_bytes()) {
                                plx_ui::label::Label::new(
                                    c_syn.as_ptr(),
                                    theme::size::BODY,
                                    theme::TEXT_SECONDARY,
                                )
                                .draw(p, Rect::new(360.0, 308.0, syn_w, 115.0));
                            }

                            // 8. Action Buttons (at y = 450)
                            let buttons = Self::detail_button_info(item.monitored);
                            for (label, icon, btn_elem) in buttons {
                                let btn_rect = self.detail_button_rect(btn_elem);
                                let is_focused = self.focus_elem == Some(btn_elem);
                                if let Ok(c_label) = std::ffi::CString::new(label) {
                                    Button::new(
                                        c_label.as_ptr(),
                                        theme::size::BODY,
                                        btn_rect,
                                    )
                                    .icon(icon)
                                    .focused(is_focused)
                                    .draw(&plx_ui::Env::inert(), p);
                                }
                            }

                            // 9. Lower Content Block (y = 540)
                            // Divider
                            p.rect(Rect::new(MARGIN_X, 540.0, SCR_W - 2.0 * MARGIN_X, 1.0), 0.0, [1.0, 1.0, 1.0, 0.12], [1.0, 1.0, 1.0, 0.12], 0.0);

                            // Section Title
                            if let Ok(c_sec) = std::ffi::CString::new("TECHNICAL SPECIFICATIONS & STORAGE") {
                                plx_ui::label::Label::new(
                                    c_sec.as_ptr(),
                                    theme::size::CAPTION,
                                    [0.60, 0.64, 0.70, 0.85],
                                )
                                .bold()
                                .draw(p, Rect::new(MARGIN_X, 564.0, 500.0, 24.0));
                            }

                            // 4 Glass Spec Cards
                            let card_y = 600.0;
                            let card_h = 96.0;
                            let gap = 18.0;
                            let card_w = (SCR_W - 2.0 * MARGIN_X - 3.0 * gap) / 4.0; // 421.5

                            let card3_header = if self.kind == ArrKind::Sonarr { "EPISODE PROGRESS" } else { "MEDIA FILE STATUS" };
                            let card3_val = if self.kind == ArrKind::Sonarr {
                                if item.total_episode_count > 0 {
                                    format!("{}/{} Episodes Downloaded", item.episode_count, item.total_episode_count)
                                } else {
                                    "0 Episodes".to_string()
                                }
                            } else {
                                if item.has_file { "Movie Downloaded".to_string() } else { "Missing File".to_string() }
                            };

                            let spec_cards = [
                                (
                                    "QUALITY PROFILE",
                                    if !item.quality_profile_name.is_empty() {
                                        item.quality_profile_name.clone()
                                    } else {
                                        "HD-1080p (Default)".to_string()
                                    },
                                ),
                                (
                                    "DISK USAGE",
                                    format_file_size(item.size_bytes),
                                ),
                                (
                                    card3_header,
                                    card3_val,
                                ),
                                (
                                    "STORAGE PATH",
                                    if !item.path.is_empty() {
                                        item.path.clone()
                                    } else {
                                        "Managed Library".to_string()
                                    },
                                ),
                            ];

                            for (i, (lbl, val)) in spec_cards.iter().enumerate() {
                                let card_x = MARGIN_X + (i as f32) * (card_w + gap);
                                let card_r = Rect::new(card_x, card_y, card_w, card_h);

                                p.rrect(card_r, 14.0, 14.0, [0.14, 0.16, 0.20, 0.75]);
                                p.rring(card_r, 14.0, 1.0, [1.0, 1.0, 1.0, 0.12]);

                                if let Ok(c_lbl) = std::ffi::CString::new(*lbl) {
                                    plx_ui::label::Label::new(
                                        c_lbl.as_ptr(),
                                        theme::size::CAPTION,
                                        theme::TEXT_TERTIARY,
                                    )
                                    .bold()
                                    .draw(p, Rect::new(card_x + 20.0, card_y + 16.0, card_w - 40.0, 20.0));
                                }

                                let elided_val = plx_gfx::text::elide_by(val, card_w - 40.0, false, |t| {
                                    f.measure.width_str(t, theme::size::BODY, true)
                                });
                                if let Ok(c_val) = std::ffi::CString::new(elided_val.as_bytes()) {
                                    plx_ui::label::Label::new(
                                        c_val.as_ptr(),
                                        theme::size::BODY,
                                        theme::TEXT_HEADING,
                                    )
                                    .bold()
                                    .draw(p, Rect::new(card_x + 20.0, card_y + 44.0, card_w - 40.0, 32.0));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Toast feedback message (bottom center)
        if let Some((msg, is_success)) = &self.toast_msg {
            let toast_rect = Rect::new((SCR_W - 440.0) / 2.0, SCR_H - 90.0, 440.0, 44.0);
            let bg = if *is_success {
                [0.10, 0.35, 0.18, 0.95]
            } else {
                [0.45, 0.15, 0.15, 0.95]
            };
            p.rrect(toast_rect, 22.0, 22.0, bg);
            if let Ok(c_msg) = std::ffi::CString::new(msg.as_str()) {
                plx_ui::label::Label::new(
                    c_msg.as_ptr(),
                    theme::size::BODY,
                    theme::TEXT_HEADING,
                )
                .bold()
                .h(plx_ui::label::HAlign::Center)
                .v(plx_ui::label::VAlign::Middle)
                .draw(p, toast_rect);
            }
        }
    }
}

#[cfg(test)]
mod tests;

