//! Owned Home page for restructure phase 8.
//!
//! The instance owns carousel, snap and render motion. The input engine owns the only focus and
//! every group cursor; this module reads them through `Cx::focus` and never mirrors them. Catalog
//! reads come exclusively from the rig-retained `HubsView`, and stable provider/item identities
//! are interned into `HomeMemory` so reorder and eviction preserve meaning rather than position.
//! Shared top chrome is container-owned: Home declares the strip doors and handles semantic strip
//! activation, but neither draws the bar nor stores a bar cursor.

use std::borrow::Cow;
use std::ffi::CString;

use crate::pms::{HeroRef, HubIdentity, HubRef, HubsView, PmsMovie};
use crate::stores::hubs::HubsCmd;
use crate::stores::{StoreCmd, StoreId, StoreWork};
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::frame::Budget;
use crate::ui::hero_logo::{self, HeroLogo, LogoRung};
use crate::ui::icons::Icon;
use crate::ui::label::{Label, VAlign};
use crate::ui::landing_hero::{
    base_scrim_ramp, stack_top as hero_stack_top, COL_W as HERO_COL_W,
    TEXT_BOTTOM as HERO_TEXT_BOTTOM,
};
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent,
    InputKind, InstanceId, Key, LogicalState, Machine, MachineId, Measure, Tick,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::screen::{
    Activate, At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable,
    GroupKind, GroupSpec, Hover, Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
    Stop,
};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{
    AmbientWash, Art, Button, CircleButton, ControlPalette, CtlPop, PageDots, PageGround,
    StatusKind, StatusOverlay,
};
use crate::ui::{hero_alpha, on_axis, Env, Painter, Rect, Spring, View};

use super::registry::{
    AppFx, AppMsg, HomeCmd, HomeGroupKey, HomeHubIdentity, HomeItemIdentity, HomeItemKey, HomeLike,
    HomeMemory, HomeReq, HomeTab, LoopReq, PageMemory,
};

const HERO_GROUP: GroupId = GroupId(0);
const FIRST_HUB_GROUP: u32 = 0x100;
const HERO_PLAY_ELEM: u32 = 0;
const HERO_INFO_ELEM: u32 = 1;
const FIRST_ITEM_ELEM: u32 = 0x1000;
const HERO_NBTN: usize = 2;

/// Parent/container semantic strip keys. Positions are deliberately not encoded here.
pub(crate) const STRIP_HOME_ELEM: u32 = crate::ui::dispatch::STRIP_BASE;
pub(crate) const STRIP_MOVIES_ELEM: u32 = crate::ui::dispatch::STRIP_BASE + 1;
pub(crate) const STRIP_SHOWS_ELEM: u32 = crate::ui::dispatch::STRIP_BASE + 2;
pub(crate) const STRIP_SEARCH_ELEM: u32 = crate::ui::dispatch::STRIP_BASE + 3;
pub(crate) const STRIP_ACCOUNT_ELEM: u32 = crate::ui::dispatch::STRIP_BASE + 4;

const MAX_HUBS: usize = crate::pms::MAX_SHELVES;
const MAX_ITEMS: usize = crate::pms::MAX_SHELF_ITEMS;
const HERO_FLIP_CD: f32 = 0.35;
const HERO_AUTO_S: f32 = 8.0;
const K_SLIDE: f32 = 130.0;
const HERO_SLIDE_REST_PX: f32 = 0.5;
const HERO_PREFETCH: usize = 1;
const HERO_ART_CULL: f32 = 0.996;
const HERO_WASH_W: [f32; 4] = [0.55, 0.55, 0.40, 0.40];

const HERO_ROW_Y: f32 = HERO_TEXT_BOTTOM + theme::space::MD;
const HERO_CTRL_D: f32 = StatusOverlay::CTRL_H;
const HERO_CTRL_GAP: f32 = crate::ui::widgets::CTRL_GAP;
const HERO_PAGER_D: f32 = HERO_CTRL_D * crate::ui::widgets::DISC_ICON_RATIO;
const HERO_PAGER_BEARING: f32 =
    HERO_PAGER_D * crate::ui::icons::ink_x(crate::ui::icons::Icon::Chevron).0;
const HERO_PAGER_PAD: f32 = HERO_CTRL_GAP - HERO_PAGER_BEARING;

const HERO_META_R: f32 = 0.60 * SCR_W;
const META_FLOW_W: f32 = HERO_META_R - MARGIN_X;
const SOURCE_PAD: f32 = theme::space::XS;
// The same screen-side measurement seam as `screens/detail/hero.rs`. The frame produced here is
// handed to `Button::draw`, so focus geometry and paint share one capability-derived width. The
// widget should eventually expose these two private terms through `Button::pill_w_with`.
const HERO_ICON_RATIO: f32 = 1.15;
const HERO_ICON_GAP: f32 = 12.0;

pub(crate) const SHAPE: &str = "HomeScreen{snap_target:f32,snap:{pos:f32,vel:f32},hero_flip_cd:f32,hero_auto:f32,visible_activation:Option<u32>,cta_available:bool,strip_chosen:bool,next_group:u32,next_elem:u32,groups:[HomeGroupKey{identity:HomeHubIdentity,group:u32}],items:[HomeItemKey{identity:HomeItemIdentity,elem:u32,last_row:u32,last_col:u32}],carousel:Option<(sid:u32,rk:str)>,outgoing:Option<(sid:u32,rk:str)>,hero_slide:{pos:f32,vel:f32},hero_dir:f32,hero_pop:{sp:[Spring{pos:f32,vel:f32};2],focused:Option<u32>},projected_generation:Option<u32>,grid:{scroll_y:{pos:f32,vel:f32},scroll_target:f32,rows:[{identity:HomeHubIdentity,group:u32,elems:[u32],motion:CardRow{scale:[Spring;24],overflow:Spring,scroll_x:Spring,lift:Spring,band:Spring,focus:i32,base_y:f32}}]},restored_scroll:[(group:u32,scroll:f32)],restore_reveal:bool}";

#[derive(Clone)]
struct HubProjection {
    identity: HomeHubIdentity,
    group: GroupId,
    elems: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Located {
    Hero(usize),
    Item(usize, usize),
}

struct Grid {
    shelves: [CardRow; MAX_HUBS],
    scroll_y: Spring,
    scroll_target: f32,
}

impl Grid {
    fn new() -> Self {
        Self {
            shelves: [CardRow::new(); MAX_HUBS],
            scroll_y: Spring::at(0.0),
            scroll_target: 0.0,
        }
    }

    fn eff_scroll(&self, row: usize, snap: f32) -> f32 {
        self.shelves[row].scroll_x() * snap
    }
}

struct Backdrop {
    wash: AmbientWash,
    grid_target: [[f32; 4]; 4],
    art: Spring,
    outgoing_art: Spring,
    tex: (u32, f32, f32),
    outgoing_tex: (u32, f32, f32),
    keyed: Option<(crate::plex::ServerId, String)>,
}

impl Backdrop {
    fn new() -> Self {
        Self {
            wash: AmbientWash::flat(theme::SURFACE_APP),
            grid_target: [theme::SURFACE_APP; 4],
            art: Spring::at(0.0),
            outgoing_art: Spring::at(0.0),
            tex: (0, 0.0, 0.0),
            outgoing_tex: (0, 0.0, 0.0),
            keyed: None,
        }
    }

    fn update(
        &mut self,
        hero: Option<HeroRef<'_>>,
        outgoing: Option<HeroRef<'_>>,
        grid_item: Option<&PmsMovie>,
        selected: Option<&(crate::plex::ServerId, String)>,
        snap: f32,
        sliding: bool,
        dt: f32,
    ) {
        let resolve = |h: Option<HeroRef<'_>>| {
            h.map(|h| crate::ui::widgets::resolve_tex_wh_on(h.item.sid, &h.item.art, 1280, 720, 0))
                .unwrap_or((0, 0.0, 0.0))
        };
        if snap < HERO_ART_CULL {
            self.tex = resolve(hero);
            self.outgoing_tex = resolve(outgoing);
        } else {
            self.tex = (0, 0.0, 0.0);
            self.outgoing_tex = (0, 0.0, 0.0);
        }

        let changed = match (self.keyed.as_ref(), selected) {
            (Some((a, x)), Some((b, y))) => a != b || x != y,
            (None, None) => false,
            _ => true,
        };
        if changed {
            crate::gfx::control_ground_invalidate();
            self.outgoing_art.jump(self.art.pos);
            self.art.jump(f32::from(self.tex.0 != 0));
            if self.keyed.is_none() {
                self.wash
                    .jump(wash_corners(hero.map(|h| h.item), self.grid_target, snap));
            }
            self.keyed = selected.cloned();
        }
        if self.tex.0 != 0 {
            self.art.step(1.0, AmbientWash::K, dt);
        }
        if self.outgoing_tex.0 != 0 {
            self.outgoing_art.step(1.0, AmbientWash::K, dt);
        }
        if let Some(item) = grid_item.filter(|m| m.has_blur) {
            self.grid_target = AmbientWash::keyed(item.blur, PageGround::CARD_W);
        }
        self.wash.step(
            wash_corners(hero.map(|h| h.item), self.grid_target, snap),
            AmbientWash::K,
            dt,
        );
        let _ = sliding;
    }

    fn draw(&self, p: Painter, env: &Env, slide: Option<(f32, f32)>) {
        let incoming_a = reveal(self.tex.0, &self.art);
        let outgoing_a = reveal(self.outgoing_tex.0, &self.outgoing_art);
        if !wash_hidden(env.sp, incoming_a, slide.map(|_| outgoing_a))
            && !self.wash.is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS)
        {
            let still = (env.sp <= 0.005 || env.sp >= 0.995) && slide.is_none();
            self.wash
                .draw_with(p, Rect::FULL, crate::gfx::page_wash_dither(still));
        }

        let folded = env.hero_a > 0.01
            && env.sp < 0.001
            && slide.is_none()
            && self.tex.0 != 0
            && incoming_a > 0.01
            && crate::ui::widgets::hero_ground_armed();
        if folded {
            crate::ui::widgets::hero_ground(
                p,
                self.tex.0,
                art_rect(self.tex, env.sp, 0.0),
                incoming_a * (1.0 - env.sp),
                base_scrim_ramp(env.hero_a),
                env.hero_a,
            );
        }
        if !folded && env.sp < HERO_ART_CULL {
            if let Some((out_x, in_x)) = slide {
                backdrop_art(p, self.outgoing_tex, env.sp, out_x, outgoing_a);
                backdrop_art(p, self.tex, env.sp, in_x, incoming_a);
            } else {
                backdrop_art(p, self.tex, env.sp, 0.0, incoming_a);
            }
        }
        if !folded && env.hero_a > 0.01 {
            let [y0, knee, mid, foot] = base_scrim_ramp(env.hero_a);
            p.rect(
                Rect::new(0.0, y0, SCR_W, knee - y0),
                0.0,
                theme::scrim(0.0),
                theme::scrim(mid),
                0.0,
            );
            p.rect(
                Rect::new(0.0, knee, SCR_W, SCR_H - knee),
                0.0,
                theme::scrim(mid),
                theme::scrim(foot),
                0.0,
            );
            crate::ui::widgets::hero_scrim(p, env.hero_a, false);
        }
    }
}

pub(crate) struct HomeScreen {
    entry: EntryId,
    instance: InstanceId,

    groups: Vec<HomeGroupKey>,
    items: Vec<HomeItemKey>,
    next_group: u32,
    next_elem: u32,
    rows: Vec<HubProjection>,
    projected_generation: Option<u32>,
    restored_scroll: Vec<(u32, f32)>,
    restore_reveal: bool,

    /// Selected hero ITEM identity. It is data, not focus.
    carousel: Option<(crate::plex::ServerId, String)>,
    outgoing: Option<(crate::plex::ServerId, String)>,
    hero_flip_cd: f32,
    hero_slide: Spring,
    hero_dir: f32,
    /// Seconds until the carousel auto-advances, hashed as logical state (`SHAPE`'s
    /// `hero_auto:f32`). Deliberately still a raw per-frame decrement (phase 12 D4 did NOT move
    /// this onto `motion::Ramp`): it is hashed across three committed replay fixtures, and a
    /// `Ramp`'s absolute-`Tick.ms` math computes the same real quantity through a different float
    /// operation sequence that measurably diverges the hash. `tick`'s own doc explains the fix
    /// that DID land — the countdown now reports `Motion`, which it never did before.
    hero_auto: f32,

    snap_target: f32,
    /// Stable element still visible while the engine has already crossed the hero/grid door.
    /// This is a press landing policy, not current focus, and retires at the visual midpoint.
    visible_activation: Option<u32>,
    cta_available: bool,
    strip_chosen: bool,
    snap: Spring,
    status_ms: f32,
    hero_pop: CtlPop<HERO_NBTN>,
    backdrop: Backdrop,
    grid: Grid,
}

impl HomeScreen {
    pub(crate) fn new(entry: EntryId, id: InstanceId) -> Self {
        Self {
            entry,
            instance: id,
            groups: Vec::new(),
            items: Vec::new(),
            next_group: FIRST_HUB_GROUP,
            next_elem: FIRST_ITEM_ELEM,
            rows: Vec::new(),
            projected_generation: None,
            restored_scroll: Vec::new(),
            restore_reveal: false,
            carousel: None,
            outgoing: None,
            hero_flip_cd: 0.0,
            hero_slide: Spring::at(1.0),
            hero_dir: 1.0,
            hero_auto: HERO_AUTO_S,
            snap_target: 0.0,
            visible_activation: None,
            cta_available: false,
            strip_chosen: false,
            snap: Spring::at(0.0),
            status_ms: 0.0,
            hero_pop: CtlPop::new(),
            backdrop: Backdrop::new(),
            grid: Grid::new(),
        }
    }

    pub(crate) fn restore(&mut self, memory: &HomeMemory) {
        for saved in &memory.groups {
            if !self.groups.iter().any(|k| k.identity == saved.identity) {
                assert!(
                    !self.groups.iter().any(|k| k.group == saved.group),
                    "restored Home group collision"
                );
                self.groups.push(saved.clone());
            }
        }
        for saved in &memory.items {
            if let Some(existing) = self.items.iter_mut().find(|k| k.identity == saved.identity) {
                existing.last_row = saved.last_row;
                existing.last_col = saved.last_col;
            } else {
                assert!(
                    !self.items.iter().any(|k| k.elem == saved.elem),
                    "restored Home element collision"
                );
                self.items.push(saved.clone());
            }
        }
        self.next_group = self.next_group.max(memory.next_group).max(FIRST_HUB_GROUP);
        self.next_elem = self.next_elem.max(memory.next_elem).max(FIRST_ITEM_ELEM);
        self.carousel = memory.carousel.clone();
        self.strip_chosen = memory.strip_chosen;
        self.grid.scroll_y.jump(memory.scroll_y);
        self.grid.scroll_target = memory.scroll_y;
        self.restored_scroll = memory.row_scroll.clone();
        self.restore_reveal = true;
        self.projected_generation = None;
    }

    fn hub_identity(view: HubsView<'_>, row: usize, hub: HubRef<'_>) -> HomeHubIdentity {
        match hub.identity {
            Some(HubIdentity::ContinueWatching) => HomeHubIdentity::ContinueWatching,
            Some(HubIdentity::Identifier { sid, id }) => HomeHubIdentity::Identifier {
                sid,
                id: id.to_owned(),
            },
            Some(HubIdentity::Key { sid, key }) => HomeHubIdentity::Key {
                sid,
                key: key.to_owned(),
            },
            None => HomeHubIdentity::Ephemeral {
                generation: view.generation,
                ordinal: row as u32,
            },
        }
    }

    fn group_for(&mut self, identity: &HomeHubIdentity) -> GroupId {
        if let Some(k) = self.groups.iter().find(|k| &k.identity == identity) {
            return GroupId(k.group);
        }
        let group = self.next_group.max(FIRST_HUB_GROUP);
        self.next_group = group
            .checked_add(1)
            .expect("Home group-key space exhausted");
        assert!(
            group < crate::ui::containers::tabs::STRIP.0,
            "Home groups overlap chrome"
        );
        self.groups.push(HomeGroupKey {
            identity: identity.clone(),
            group,
        });
        GroupId(group)
    }

    fn item_registration(&mut self, identity: HomeItemIdentity) -> &mut HomeItemKey {
        if let Some(index) = self.items.iter().position(|k| k.identity == identity) {
            return &mut self.items[index];
        }
        let elem = self.next_elem.max(FIRST_ITEM_ELEM);
        self.next_elem = elem
            .checked_add(1)
            .expect("Home element-key space exhausted");
        assert!(
            elem < crate::ui::dispatch::STRIP_BASE,
            "Home elements overlap chrome"
        );
        self.items.push(HomeItemKey { identity, elem, last_row: 0, last_col: 0 });
        self.items.last_mut().unwrap()
    }

    #[cfg(test)]
    fn elem_for(&mut self, identity: HomeItemIdentity) -> u32 {
        self.item_registration(identity).elem
    }

    fn sync_catalog<H: HomeLike>(&mut self, cx: &Cx<'_, H>) {
        let view = H::hubs(cx);
        if self.projected_generation == Some(view.generation) {
            return;
        }
        let mut rows = Vec::with_capacity(n_hubs_of(view.hub_count()));
        for row in 0..n_hubs_of(view.hub_count()) {
            let Some(hub) = view.hub(row) else { continue };
            let identity = Self::hub_identity(view, row, hub);
            let group = self.group_for(&identity);
            let mut elems = Vec::with_capacity(hub.items.len().min(MAX_ITEMS));
            for (col, item) in hub.items.iter().take(MAX_ITEMS).enumerate() {
                let item_identity = if item.rk.is_empty() {
                    HomeItemIdentity::Slot {
                        hub: identity.clone(),
                        generation: view.generation,
                        ordinal: col as u32,
                    }
                } else {
                    HomeItemIdentity::Item {
                        hub: identity.clone(),
                        sid: item.sid,
                        rk: item.rk.clone(),
                    }
                };
                let key = self.item_registration(item_identity);
                key.last_row = row as u32;
                key.last_col = col as u32;
                elems.push(key.elem);
            }
            rows.push(HubProjection {
                identity,
                group,
                elems,
            });
        }
        let old_shelves = self.grid.shelves;
        self.grid.shelves = [CardRow::new(); MAX_HUBS];
        for (index, row) in rows.iter().enumerate() {
            if let Some(old) = self.rows.iter().position(|old| old.identity == row.identity) {
                self.grid.shelves[index] = old_shelves[old];
            }
            if let Some(&(_, scroll)) = self.restored_scroll.iter().find(|(group, _)| *group == row.group.0) {
                self.grid.shelves[index].restore_scroll(scroll, row.elems.len(), &RowStyle::HOME);
            }
        }
        self.rows = rows;
        self.restored_scroll.retain(|(group, _)| {
            view.state != crate::pms::HubState::Ready
                && !self.rows.iter().any(|row| row.group.0 == *group)
        });
        self.projected_generation = Some(view.generation);
        self.reconcile_carousel(view);
    }

    fn identity_of(item: &PmsMovie) -> Option<(crate::plex::ServerId, String)> {
        (!item.rk.is_empty()).then(|| (item.sid, item.rk.clone()))
    }

    fn same_item(item: &PmsMovie, identity: &(crate::plex::ServerId, String)) -> bool {
        item.sid == identity.0 && item.rk == identity.1
    }

    fn hero_by_identity<'a>(
        view: HubsView<'a>,
        identity: &(crate::plex::ServerId, String),
    ) -> Option<HeroRef<'a>> {
        (0..view.hero_count())
            .filter_map(|i| view.hero(i))
            .find(|h| Self::same_item(h.item, identity))
    }

    fn first_hero(view: HubsView<'_>) -> Option<HeroRef<'_>> {
        view.hero(0).or_else(|| {
            let hub = view.hub(0)?;
            Some(HeroRef {
                item: hub.items.first()?,
                source: hub.source,
            })
        })
    }

    fn reconcile_carousel(&mut self, view: HubsView<'_>) {
        if self
            .carousel
            .as_ref()
            .and_then(|id| Self::hero_by_identity(view, id))
            .is_some()
        {
            return;
        }
        self.outgoing = None;
        self.hero_slide.jump(1.0);
        self.carousel = Self::first_hero(view).and_then(|h| Self::identity_of(h.item));
    }

    fn selected_hero<'a>(&self, view: HubsView<'a>) -> Option<HeroRef<'a>> {
        self.carousel
            .as_ref()
            .and_then(|id| Self::hero_by_identity(view, id))
            .or_else(|| Self::first_hero(view))
    }

    fn outgoing_hero<'a>(&self, view: HubsView<'a>) -> Option<HeroRef<'a>> {
        self.outgoing
            .as_ref()
            .and_then(|id| Self::hero_by_identity(view, id))
    }

    fn carousel_index(&self, view: HubsView<'_>) -> usize {
        self.carousel
            .as_ref()
            .and_then(|id| {
                (0..view.hero_count())
                    .find(|&i| view.hero(i).is_some_and(|h| Self::same_item(h.item, id)))
            })
            .unwrap_or(0)
    }

    fn flip(&mut self, view: HubsView<'_>, dir: i32) -> bool {
        let n = view.hero_count();
        if n <= 1 || self.hero_flip_cd > 0.0 {
            return false;
        }
        let cur = self.carousel_index(view).min(n - 1);
        let next = (cur as i32 + dir).rem_euclid(n as i32) as usize;
        let Some(current) = view.hero(cur).and_then(|h| Self::identity_of(h.item)) else {
            return false;
        };
        let Some(incoming) = view.hero(next).and_then(|h| Self::identity_of(h.item)) else {
            return false;
        };
        self.outgoing = Some(current);
        self.carousel = Some(incoming);
        self.hero_dir = dir as f32;
        self.hero_slide.jump(0.0);
        self.hero_flip_cd = HERO_FLIP_CD;
        self.hero_auto = HERO_AUTO_S;
        true
    }

    fn slide_offsets(&self) -> Option<(f32, f32)> {
        self.outgoing.as_ref()?;
        Some((
            -self.hero_dir * self.hero_slide.pos * SCR_W,
            self.hero_dir * (1.0 - self.hero_slide.pos) * SCR_W,
        ))
    }

    fn locate(&self, elem: u32) -> Option<Located> {
        match elem {
            HERO_PLAY_ELEM => return Some(Located::Hero(0)),
            HERO_INFO_ELEM => return Some(Located::Hero(1)),
            _ => {}
        }
        self.rows.iter().enumerate().find_map(|(row, hub)| {
            hub.elems
                .iter()
                .position(|&e| e == elem)
                .map(|col| Located::Item(row, col))
        })
    }

    fn focused_loc(&self, focus: Option<FocusKey<u32>>) -> Option<Located> {
        focus
            .filter(|k| k.entry == self.entry)
            .and_then(|k| self.locate(k.elem))
    }

    fn focused_grid(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        match self.focused_loc(focus)? {
            Located::Item(row, col) => Some((row, col)),
            Located::Hero(_) => None,
        }
    }

    fn visible_focus(&self, focus: Option<FocusKey<u32>>) -> Option<FocusKey<u32>> {
        focus.map(|key| FocusKey {
            entry: key.entry,
            elem: self.activation_elem(key.elem),
        })
    }

    fn hub<'a>(&self, view: HubsView<'a>, row: usize) -> Option<HubRef<'a>> {
        (row < self.rows.len()).then(|| view.hub(row)).flatten()
    }

    fn item_at<'a>(&self, view: HubsView<'a>, row: usize, col: usize) -> Option<&'a PmsMovie> {
        self.hub(view, row)?.items.get(col)
    }

    fn env(&self, dt: f32) -> Env {
        let mut env = Env::inert();
        env.dt = dt;
        env.sp = self.snap.pos;
        env.hero_a = hero_alpha(self.snap.pos, 0.55);
        env
    }

    fn layout_grid(&mut self) {
        let top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * self.snap.pos;
        let mut flow = 0.0;
        for row in 0..MAX_HUBS {
            self.grid.shelves[row].base_y = top + flow - self.grid.scroll_y.pos * self.snap.pos;
            flow += card_row::ROW_PITCH_FIXED + self.grid.shelves[row].under_band();
        }
    }

    fn update_grid<H: HomeLike>(&mut self, view: HubsView<'_>, cx: &Cx<'_, H>, dt: f32) {
        let focused = self.focused_grid(cx.focus.current);
        let grid_live = self.snap.pos > 0.5;
        for row in 0..MAX_HUBS {
            let count = self
                .hub(view, row)
                .map_or(0, |h| h.items.len().min(MAX_ITEMS));
            let col = focused
                .filter(|&(r, _)| grid_live && r == row)
                .map(|(_, c)| c);
            self.grid.shelves[row].update(count, col, &RowStyle::HOME, dt);
        }
        if let Some((row, _)) = focused.filter(|(r, _)| *r < self.rows.len()) {
            let (lo, hi) = row_reveal_band(shelf_top_settled(row, row));
            self.grid.scroll_target = card_row::reveal(
                self.grid.scroll_y.pos,
                lo,
                hi,
                grid_max_scroll(self.rows.len()),
            );
        }
        self.grid
            .scroll_y
            .step(self.grid.scroll_target, K_SCROLL, dt);
        self.layout_grid();
    }

    fn tick<H: HomeLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let dt = t.dt();
        self.sync_catalog(cx);
        let view = H::hubs(cx);
        let cta_available = hero_group_len(view) > 0;
        if cta_available
            && !self.cta_available
            && !self.strip_chosen
            && cx
                .focus
                .current
                .is_none_or(|key| key.elem >= crate::ui::dispatch::STRIP_BASE)
        {
            self.reseat(FocusTarget::ContainerGroup(HERO_GROUP), fx);
        }
        self.cta_available = cta_available;
        if self.rows.is_empty() {
            self.snap_target = 0.0;
            self.snap.jump(0.0);
        }
        self.status_ms = (self.status_ms + dt * 1000.0)
            % (crate::ui::widgets::Spinner::PERIOD_MS as f32 * 1000.0);
        self.hero_flip_cd = (self.hero_flip_cd - dt).max(0.0);
        if self.outgoing.is_some() {
            self.hero_slide.step(1.0, K_SLIDE, dt);
            if (1.0 - self.hero_slide.pos).abs() * SCR_W < HERO_SLIDE_REST_PX {
                self.hero_slide.jump(1.0);
                self.outgoing = None;
            }
        }
        if self.snap.pos < 0.05 && view.hero_count() > 1 {
            // Spelled as an assignment, not `-= dt`: bit-for-bit identical arithmetic to the
            // pre-D4 accumulator, deliberately UNCHANGED — `hero_auto` is HASHED `LogicalState`
            // (`SHAPE`'s `hero_auto:f32`) across three committed replay fixtures, and a
            // `motion::Ramp`'s absolute-`Tick.ms` math computes the same real quantity through a
            // different float operation sequence that measurably diverges the hash (verified:
            // `tests/focusfp.sh --replay` on flow 1 disagreed from frame 7 on, once the arithmetic
            // changed). What WAS a real bug — this countdown never reported `Motion`, so a hero
            // left mid-count-down on an otherwise-settled screen could silently freeze under
            // `ui::idle`'s present gate, exactly the regression class this whole conversion
            // exists to catch — is fixed by the explicit `note` just below, with no change to the
            // number itself.
            self.hero_auto = self.hero_auto - dt;
            fx.present().note(PresentEvent::Motion);
            if self.hero_auto <= 0.0 {
                self.hero_flip_cd = 0.0;
                self.flip(view, 1);
            }
        } else {
            self.hero_auto = HERO_AUTO_S;
        }
        self.snap
            .step(pinned_snap(self.snap_target, self.rows.len()), K_SNAP, dt);
        let engine_on_grid = self.focused_grid(cx.focus.current).is_some();
        let picture_is_grid = self.snap.pos >= 0.5;
        if engine_on_grid == picture_is_grid {
            self.visible_activation = None;
        }
        let visible_focus = self.visible_focus(cx.focus.current);
        let hero_focus = match self.focused_loc(visible_focus) {
            Some(Located::Hero(i)) if status_read(view).is_none() => Some(i),
            _ => None,
        };
        self.hero_pop.step(hero_focus, dt);
        let visible_cx: Cx<'_, H> = Cx {
            views: cx.views,
            tick: cx.tick,
            measure: cx.measure,
            press: cx.press,
            focus: crate::ui::machine::FocusRead {
                current: visible_focus,
            ..Default::default() },
            owner: cx.owner,
        };
        self.update_grid(view, &visible_cx, dt);

        let grid_item = self
            .focused_grid(visible_focus)
            .and_then(|(r, c)| self.item_at(view, r, c));
        let selected = self.selected_hero(view);
        let outgoing = self.outgoing_hero(view);
        self.backdrop.update(
            selected,
            outgoing,
            grid_item,
            self.carousel.as_ref(),
            self.snap.pos,
            self.outgoing.is_some(),
            dt,
        );
        self.prefetch(view);

        let moving = self.outgoing.is_some()
            || (self.snap.pos - self.snap_target).abs() > 0.002
            || self.snap.vel.abs() > 0.01
            || matches!(status_read(view), Some((_, StatusKind::Working, _)));
        if moving {
            fx.note(PresentEvent::Motion);
        }
        // Route-owned work is queued only after this read-only step has completed.
        fx.push(Fx::App(AppFx::StoreWork(StoreWork::Hubs)));
        fx.push(Fx::App(AppFx::StoreWork(StoreWork::BrowseDiscovery)));
    }

    fn prefetch(&self, view: HubsView<'_>) {
        use crate::ui::tex::Warm;
        if !(prefetch_armed(self.snap.pos, self.outgoing.is_some())
            && crate::ui::tex::source_idle())
        {
            return;
        }
        let mut order = [0i32; 2 * HERO_PREFETCH];
        let count = prefetch_order(
            self.carousel_index(view) as i32,
            view.hero_count() as i32,
            &mut order,
        );
        for &index in &order[..count] {
            let Some(hero) = view.hero(index as usize) else {
                continue;
            };
            if crate::ui::widgets::warm_tex_on(hero.item.sid, &hero.item.art, 1280, 720, 0)
                == Warm::Claimed
            {
                return;
            }
            if crate::ui::tex::logo_warm(hero.item.sid.raw(), hero_logo_rk(hero.item))
                == Warm::Claimed
            {
                return;
            }
        }
    }

    fn request_for_strip(elem: u32) -> Option<HomeReq> {
        Some(match elem {
            STRIP_HOME_ELEM => HomeReq::Tab(HomeTab::Home),
            STRIP_MOVIES_ELEM => HomeReq::Tab(HomeTab::Movies),
            STRIP_SHOWS_ELEM => HomeReq::Tab(HomeTab::Shows),
            STRIP_SEARCH_ELEM => HomeReq::Tab(HomeTab::Search),
            STRIP_ACCOUNT_ELEM => HomeReq::Account,
            _ => return None,
        })
    }

    fn reseat<H: HomeLike>(&self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Deliver(
            MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus })),
        ));
    }

    fn emit_item_menu<H: HomeLike>(&self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if self.snap.pos < 0.5 {
            return Handled::No;
        }
        let Some((row, col)) = self.focused_grid(cx.focus.current) else {
            return Handled::No;
        };
        let Some(item) = self.item_at(H::hubs(cx), row, col) else {
            return Handled::No;
        };
        if item.rk.is_empty() {
            return Handled::No;
        }
        fx.push(Fx::App(AppFx::Home(HomeReq::ItemMenu {
            sid: item.sid,
            rk: item.rk.clone(),
        })));
        fx.invalidate(Provenance::Input);
        Handled::Yes
    }

    fn command<H: HomeLike>(
        &mut self,
        command: HomeCmd,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        match command {
            HomeCmd::FocusGrid { row, col } => {
                let Some(key) = self.dev_focus_key(row, col, cx) else {
                    return Handled::No;
                };
                self.reseat(FocusTarget::Elem(key), fx);
            }
            HomeCmd::Hero => {
                self.strip_chosen = false;
                self.reseat(FocusTarget::ContainerGroup(HERO_GROUP), fx);
            }
            HomeCmd::FocusStrip(tab) => {
                let elem = match tab {
                    HomeTab::Home => STRIP_HOME_ELEM,
                    HomeTab::Movies => STRIP_MOVIES_ELEM,
                    HomeTab::Shows => STRIP_SHOWS_ELEM,
                    HomeTab::Search => STRIP_SEARCH_ELEM,
                };
                self.strip_chosen = true;
                self.reseat(
                    FocusTarget::Elem(FocusKey {
                        entry: self.entry,
                        elem,
                    }),
                    fx,
                );
            }
            HomeCmd::Flip(direction) => {
                if direction == 0 || !self.flip(H::hubs(cx), direction.signum()) {
                    return Handled::No;
                }
                fx.invalidate(Provenance::Input);
            }
            HomeCmd::SelectHero(index) => {
                let view = H::hubs(cx);
                if view.hero_count() == 0 {
                    return Handled::No;
                }
                let index = (index.max(0) as usize).min(view.hero_count() - 1);
                let Some(hero) = view.hero(index) else {
                    return Handled::No;
                };
                let Some(identity) = Self::identity_of(hero.item) else {
                    return Handled::No;
                };
                self.carousel = Some(identity);
                self.outgoing = None;
                self.hero_slide.jump(1.0);
                fx.invalidate(Provenance::Input);
            }
            HomeCmd::ItemMenu => return self.emit_item_menu(cx, fx),
        }
        Handled::Yes
    }

    fn activate<H: HomeLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if let Some(req) = Self::request_for_strip(elem) {
            fx.push(Fx::App(AppFx::Home(req)));
            return;
        }
        let view = H::hubs(cx);
        if status_read(view).is_some() && elem == HERO_PLAY_ELEM {
            fx.push(Fx::App(AppFx::Store(
                StoreId::Hubs,
                StoreCmd::Hubs(HubsCmd::Retry),
            )));
            return;
        }
        let elem = self.activation_elem(elem);
        let Some(located) = self.locate(elem) else {
            return;
        };
        let (item, req) = match located {
            Located::Hero(0) => {
                let Some(item) = self.selected_hero(view).map(|h| h.item) else {
                    return;
                };
                let resume_ns = crate::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000);
                (
                    item,
                    HomeReq::Play {
                        sid: item.sid,
                        rk: item.rk.clone(),
                        resume_ns,
                    },
                )
            }
            Located::Hero(1) => {
                let Some(item) = self.selected_hero(view).map(|h| h.item) else {
                    return;
                };
                (
                    item,
                    HomeReq::Detail {
                        sid: item.sid,
                        rk: item.rk.clone(),
                    },
                )
            }
            Located::Hero(_) => return,
            Located::Item(row, col) => {
                let Some(item) = self.item_at(view, row, col) else {
                    return;
                };
                if self
                    .rows
                    .get(row)
                    .is_some_and(|h| h.identity == HomeHubIdentity::ContinueWatching)
                {
                    let resume_ns =
                        crate::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000);
                    (
                        item,
                        HomeReq::Play {
                            sid: item.sid,
                            rk: item.rk.clone(),
                            resume_ns,
                        },
                    )
                } else {
                    (
                        item,
                        HomeReq::Detail {
                            sid: item.sid,
                            rk: item.rk.clone(),
                        },
                    )
                }
            }
        };
        if !item.rk.is_empty() {
            fx.push(Fx::App(AppFx::Home(req)));
        }
    }

    fn draw_page<H: HomeLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let view = H::hubs(f.cx);
        let visible_focus = self.visible_focus(f.focus.current);
        let env = self.env(0.0);
        let focus = self.focused_loc(visible_focus);
        let grid_focus = self.focused_grid(visible_focus);
        let p = f.painter.alpha(f.page_alpha);
        let slide = self.slide_offsets();
        crate::ui::profile::phase("hm.backdrop", || self.backdrop.draw(p, &env, slide));
        if env.hero_a > 0.01 {
            crate::ui::profile::phase("hm.hero", || {
                self.draw_hero(
                    view,
                    &env,
                    p.alpha(env.hero_a),
                    f.page_alpha,
                    focus,
                    f.measure,
                    f.press.scale,
                )
            });
        }
        crate::ui::profile::phase("hm.grid", || {
            self.draw_grid(view, &env, p, f.press.scale, grid_focus, f.measure)
        });
        crate::ui::profile::phase("hm.status", || self.draw_status(view, &env, p, focus));
        crate::ui::testpat::underlay(p);
        crate::ui::testpat::draw(p);
        self.record_stops(f, view);
    }

    fn draw_hero(
        &self,
        view: HubsView<'_>,
        env: &Env,
        p: Painter,
        page_alpha: f32,
        focus: Option<Located>,
        measure: &dyn Measure,
        press_scale: f32,
    ) {
        let Some(hero) = self.selected_hero(view) else {
            return;
        };
        if let Some((out_x, in_x)) = self.slide_offsets() {
            if let Some(old) = self.outgoing_hero(view) {
                let po = p.translate(out_x, 0.0);
                hero_content(old.item, display_source(old.source), po, out_x, measure);
                self.draw_hero_actions(old.item, env, po, out_x, false, page_alpha, focus, measure, press_scale);
            }
            let pi = p.translate(in_x, 0.0);
            hero_content(hero.item, display_source(hero.source), pi, in_x, measure);
            self.draw_hero_actions(hero.item, env, pi, in_x, true, page_alpha, focus, measure, press_scale);
        } else {
            hero_content(hero.item, display_source(hero.source), p, 0.0, measure);
            self.draw_hero_actions(hero.item, env, p, 0.0, true, page_alpha, focus, measure, press_scale);
        }
        if view.hero_count() > 1 {
            PageDots::new(view.hero_count())
                .active(self.carousel_index(view))
                .centered_at(SCR_W * 0.5, HERO_ROW_Y + HERO_CTRL_D + theme::space::SM)
                .draw(env, p);
        }
    }

    fn draw_hero_actions(
        &self,
        hero: &PmsMovie,
        env: &Env,
        p: Painter,
        dx: f32,
        live: bool,
        page_alpha: f32,
        focus: Option<Located>,
        measure: &dyn Measure,
        press_scale: f32,
    ) {
        let resumes = crate::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
        let label = if resumes { c"Continue" } else { c"Play" };
        let pill = Rect::new(
            MARGIN_X,
            HERO_ROW_Y,
            hero_pill_w(measure, label),
            HERO_CTRL_D,
        );
        let info = Rect::new(
            pill.x + pill.w + HERO_CTRL_GAP,
            HERO_ROW_Y,
            HERO_CTRL_D,
            HERO_CTRL_D,
        );
        let mark_d = HERO_PAGER_D.round();
        let mark = Rect::new(
            info.x + info.w + HERO_PAGER_PAD,
            HERO_ROW_Y + (HERO_CTRL_D - mark_d) * 0.5,
            mark_d,
            mark_d,
        );
        if !on_axis(
            MARGIN_X + dx,
            mark.x + mark.w + HERO_PAGER_PAD - MARGIN_X,
            SCR_W,
            0.0,
        ) {
            return;
        }
        let may_read = live && dx.abs() < 0.5 && page_alpha >= 0.999;
        let palette = crate::gfx::sample_control_ground(
            [pill.x + dx, pill.y, info.x + info.w - pill.x, pill.h],
            may_read,
        )
        .map(ControlPalette::ambient)
        .unwrap_or_default();
        let pop = |i| if live { self.hero_pop.scale_with(i, press_scale) } else { 1.0 };
        Button::new(label.as_ptr(), theme::size::BODY, pill)
            .icon(Icon::Play)
            .focused(focus == Some(Located::Hero(0)))
            .palette(palette)
            .scale(pop(0))
            .draw(env, p);
        CircleButton::new(c"".as_ptr())
            .icon(Icon::Info)
            .at(info.x, info.y)
            .focused(focus == Some(Located::Hero(1)))
            .palette(palette)
            .scale(pop(1))
            .draw(env, p);
        crate::ui::icons::draw(p, Icon::Chevron, mark, theme::TEXT_SECONDARY);
    }

    fn draw_grid(
        &self,
        view: HubsView<'_>,
        env: &Env,
        p: Painter,
        press_scale: f32,
        focused: Option<(usize, usize)>,
        measure: &dyn Measure,
    ) {
        for row in 0..self.rows.len() {
            let Some(hub) = self.hub(view, row) else {
                continue;
            };
            let row_y = self.grid.shelves[row].base_y;
            if !on_axis(row_y, CARD_H, SCR_H, 0.0) {
                continue;
            }
            if env.sp > 0.02 {
                card_row::draw_heading(
                    p.alpha(env.sp),
                    hub.title,
                    hub.source,
                    MARGIN_X,
                    heading_y(row_y, self.grid.shelves[row].lift()),
                    f32::INFINITY,
                    measure,
                );
            }
            for (col, item) in hub.items.iter().take(MAX_ITEMS).enumerate() {
                if focused == Some((row, col)) && env.sp > 0.5 {
                    continue;
                }
                let x = card_x(col, self.grid.eff_scroll(row, env.sp));
                if !on_axis(x, CARD_W, SCR_W, GLOW_PAD) {
                    continue;
                }
                let (rect, scale) = self.drawn_card_geometry(row, col, 1.0);
                card_row::draw_tile(
                    p,
                    Art::Poster(Some(item)),
                    rect,
                    scale,
                    &RowStyle::HOME,
                    item.resume_frac(),
                );
            }
        }
        self.draw_focused_cell(view, env, p, press_scale, focused, measure);
    }

    fn draw_focused_cell(
        &self,
        view: HubsView<'_>,
        env: &Env,
        p: Painter,
        press_scale: f32,
        focused: Option<(usize, usize)>,
        measure: &dyn Measure,
    ) {
        let Some((row, col)) = focused.filter(|_| env.sp > 0.5) else {
            return;
        };
        let Some(hub) = self.hub(view, row) else {
            return;
        };
        let Some(item) = hub.items.get(col) else {
            return;
        };
        let (rect, scale) = self.drawn_card_geometry(row, col, press_scale);
        let cw = self.rows[row].identity == HomeHubIdentity::ContinueWatching;
        let mut label = if cw {
            card_row::TileLabel::played(&item.title)
        } else {
            card_row::TileLabel::title(&item.title)
        };
        label.caption = card_row::focused_caption(item, cw);
        let label = label.revealed(self.grid.shelves[row].band_reveal());
        card_row::draw_focused(
            p,
            Art::Poster(Some(item)),
            rect,
            scale,
            &RowStyle::HOME,
            item.resume_frac(),
            &label,
            measure,
        );
    }

    /// One drawn rectangle for the tile painter and the input map, including the press bounce.
    fn drawn_card_geometry(&self, row: usize, col: usize, press_scale: f32) -> (Rect, f32) {
        let scale = self.grid.shelves[row].scale(col)
            * if press_scale > 0.0 { press_scale } else { 1.0 };
        let rect = Rect::new(
            card_x(col, self.grid.eff_scroll(row, self.snap.pos)),
            self.grid.shelves[row].base_y + CARD_DY,
            CARD_W,
            CARD_H,
        )
        .scaled(scale);
        (rect, scale)
    }

    fn draw_status(&self, view: HubsView<'_>, env: &Env, p: Painter, focus: Option<Located>) {
        let Some((caption, kind, action)) = status_read(view) else {
            return;
        };
        let focused = focus == Some(Located::Hero(0));
        let mut overlay = StatusOverlay::new(Rect::FULL, caption, kind)
            .phase(self.status_ms as u32)
            .focused(focused);
        if let Some(label) = action {
            overlay = overlay.action(label);
        }
        overlay.draw(env, p);
    }

    pub(crate) fn record_stops<H: HomeLike>(&self, f: &mut DrawFrame<'_, '_, H>, view: HubsView<'_>) {
        let focus = f.focus.current;
        let status = status_read(view);
        let hero_len = if status.is_some() {
            usize::from(status.is_some_and(|(_, _, a)| a.is_some()))
        } else {
            HERO_NBTN
        };
        if self.snap.pos < 0.5 {
            for i in 0..hero_len {
                let elem = if i == 0 {
                    HERO_PLAY_ELEM
                } else {
                    HERO_INFO_ELEM
                };
                let Some(placed) = Focusable::<H>::place(self, &elem, f.cx, At::Drawn) else {
                    continue;
                };
                f.stop(
                    f.painter,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Press,
                    },
                );
            }
        }
        if self.snap.pos >= 0.5 {
            for row in &self.rows {
                // Keep the whole focused row hoverable while vertical reveal is in flight.
                // Other partially visible rows must not steal focus as they pass the pointer.
                let row_focused = focus.is_some_and(|key| {
                    key.entry == self.entry && row.elems.contains(&key.elem)
                });
                for &elem in &row.elems {
                    let Some(placed) = Focusable::<H>::place(self, &elem, f.cx, At::Drawn) else {
                        continue;
                    };
                    let fully_visible = placed.rest_rect.y >= 40.0
                        && placed.rest_rect.y + placed.rest_rect.h <= SCR_H - 20.0;
                    let hover = if fully_visible || row_focused {
                        Hover::Focus
                    } else {
                        Hover::OnlyIfFocused
                    };
                    f.stop(
                        f.painter,
                        Stop {
                            key: FocusKey {
                                entry: self.entry,
                                elem,
                            },
                            rect: placed.rect,
                            rest_rect: placed.rest_rect,
                            clip: placed.clip,
                            hover,
                            activate: Activate::Press,
                        },
                    );
                }
            }
        }
    }

    pub(crate) fn hero_item<'a, H: HomeLike>(&self, cx: &Cx<'a, H>) -> Option<&'a PmsMovie> {
        self.selected_hero(H::hubs(cx)).map(|h| h.item)
    }

    pub(crate) fn focused_item<'a, H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        cx: &Cx<'a, H>,
    ) -> Option<&'a PmsMovie> {
        let view = H::hubs(cx);
        match self.focused_loc(focus)? {
            Located::Hero(_) => self.selected_hero(view).map(|h| h.item),
            Located::Item(row, col) => self.item_at(view, row, col),
        }
    }

    pub(crate) fn grid_position<H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        _cx: &Cx<'_, H>,
    ) -> Option<(usize, usize)> {
        self.focused_grid(focus)
    }

    pub(crate) fn snap_target(&self) -> f32 {
        self.snap_target
    }

    pub(crate) fn focused_rect<H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        cx: &Cx<'_, H>,
        at: At,
    ) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        self.focused_grid(Some(key))?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|p| p.rect)
    }

    pub(crate) fn redraw_focused<H: HomeLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        focus: Option<FocusKey<u32>>,
    ) {
        let Some(key) = focus.filter(|k| k.entry == self.entry) else {
            return;
        };
        let Some((row, col)) = self.focused_grid(Some(key)) else {
            return;
        };
        let view = H::hubs(f.cx);
        let env = self.env(0.0);
        let p = f.painter.alpha(f.page_alpha);
        let cut = crate::ui::widgets::TOP_BAR_BOTTOM;
        let _clip = f.clip(p, Rect::new(0.0, cut, SCR_W, SCR_H - cut));
        self.draw_focused_cell(view, &env, p, f.press.scale, Some((row, col)), f.measure);
    }

    pub(crate) fn dev_focus_key<H: HomeLike>(
        &self,
        row: usize,
        col: usize,
        _cx: &Cx<'_, H>,
    ) -> Option<FocusKey<u32>> {
        Some(FocusKey {
            entry: self.entry,
            elem: *self.rows.get(row)?.elems.get(col)?,
        })
    }
}

impl LogicalState for HomeScreen {
    fn write(&self, c: &mut Canon) {
        // Exhaustive field census. Entry/instance identity is encoded by Navigation; spinner
        // phase and backdrop resources only paint. Every motion value queried by input/reveal
        // is encoded below, including velocities that determine the next Tick's answer.
        let Self { entry: _, instance: _, groups: _, items: _, next_group: _, next_elem: _,
            rows: _, projected_generation: _, restored_scroll: _, restore_reveal: _, carousel: _,
            outgoing: _, hero_flip_cd: _, hero_slide: _, hero_dir: _, hero_auto: _,
            snap_target: _,
            visible_activation: _, cta_available: _, strip_chosen: _, snap: _, status_ms: _,
            hero_pop: _, backdrop: _, grid: _ } = self;
        c.f32(self.snap_target)
            .f32(self.snap.pos).f32(self.snap.vel)
            .f32(self.hero_flip_cd)
            .f32(self.hero_auto)
            .option(self.visible_activation, |c, elem| {
                c.u32(elem);
            })
            .bool(self.cta_available)
            .bool(self.strip_chosen)
            .u32(self.next_group)
            .u32(self.next_elem)
            .seq(self.groups.len());
        for key in &self.groups {
            write_hub_identity(&key.identity, c);
            c.u32(key.group);
        }
        c.seq(self.items.len());
        for key in &self.items {
            write_item_identity(&key.identity, c);
            c.u32(key.elem).u32(key.last_row).u32(key.last_col);
        }
        c.option(self.carousel.as_ref(), |c, (sid, rk)| {
            c.u32(u32::from(sid.raw())).str(rk);
        });
        c.option(self.outgoing.as_ref(), |c, (sid, rk)| {
            c.u32(u32::from(sid.raw())).str(rk);
        });
        c.f32(self.hero_slide.pos).f32(self.hero_slide.vel).f32(self.hero_dir);
        self.hero_pop.write_motion(c);
        c.option(self.projected_generation, |c, generation| { c.u32(generation); });
        c.f32(self.grid.scroll_y.pos).f32(self.grid.scroll_y.vel)
            .f32(self.grid.scroll_target).seq(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            write_hub_identity(&row.identity, c);
            c.u32(row.group.0).seq(row.elems.len());
            for elem in &row.elems { c.u32(*elem); }
            self.grid.shelves[index].write_motion(c);
        }
        c.seq(self.restored_scroll.len());
        for &(group, scroll) in &self.restored_scroll { c.u32(group).f32(scroll); }
        c.bool(self.restore_reveal);
    }

    fn probe(&self, out: &mut String) {
        out.push_str("home");
    }
}

impl<H: HomeLike> Focusable<H> for HomeScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let view = H::hubs(cx);
        let hero_len = hero_group_len(view);
        let hero_end = self
            .hero_button_rect(view, hero_len.saturating_sub(1), cx.measure)
            .unwrap_or(Rect::new(MARGIN_X, HERO_ROW_Y, 0.0, HERO_CTRL_D));
        out.push(GroupSpec {
            id: HERO_GROUP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            // The hero is reached only through declared strip/first-shelf links. Its fixed
            // billboard extent remains on the page while folded and must never win a geometric
            // search from a lower shelf.
            reachable: AxisMask(0),
            edge: [
                EdgeRule::Geometric,
                EdgeRule::Geometric,
                EdgeRule::Screen,
                EdgeRule::Screen,
            ],
            extent: Rect::new(
                MARGIN_X,
                HERO_ROW_Y,
                hero_end.x + hero_end.w - MARGIN_X,
                HERO_CTRL_D,
            ),
            len: hero_len,
            elem: ElemKind::Control,
        });
        let focus_row = self
            .focused_grid(cx.focus.current)
            .map(|(r, _)| r)
            .unwrap_or(0);
        for (row, projection) in self.rows.iter().enumerate() {
            let top = GRID_TOP_Y + shelf_top_settled(row, focus_row) - self.grid.scroll_target;
            out.push(GroupSpec {
                id: projection.group,
                kind: GroupKind::Row { wrap: false },
                seat: Seat::Nearest,
                reachable: AxisMask::BOTH,
                edge: [
                    EdgeRule::Geometric,
                    EdgeRule::Geometric,
                    EdgeRule::Stop,
                    EdgeRule::Stop,
                ],
                extent: Rect::new(
                    MARGIN_X,
                    top + CARD_DY,
                    SCR_W - 2.0 * MARGIN_X,
                    CARD_H + card_row::UNDER_LABEL_H,
                ),
                len: projection.elems.len(),
                elem: ElemKind::Card,
            });
        }
    }

    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        match self.locate(*key)? {
            Located::Hero(index) => (index < hero_group_len(H::hubs(cx))).then_some(HERO_GROUP),
            Located::Item(row, _) => self.rows.get(row).map(|r| r.group),
        }
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        let hero_len = hero_group_len(H::hubs(cx));
        let next = match self.locate(key.elem) {
            Some(Located::Hero(i)) => match dir {
                Dir::Left if i > 0 => Some(HERO_PLAY_ELEM),
                Dir::Right if i + 1 < hero_len => Some(HERO_INFO_ELEM),
                _ => None,
            },
            Some(Located::Item(row, col)) => match dir {
                Dir::Left if col > 0 => self
                    .rows
                    .get(row)
                    .and_then(|r| r.elems.get(col - 1))
                    .copied(),
                Dir::Right => self
                    .rows
                    .get(row)
                    .and_then(|r| r.elems.get(col + 1))
                    .copied(),
                _ => None,
            },
            None => None,
        };
        next.map(|elem| {
            Step::Move(FocusKey {
                entry: key.entry,
                elem,
            })
        })
        .unwrap_or(Step::Edge)
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let view = H::hubs(cx);
        match self.locate(*key)? {
            Located::Hero(index) => {
                let mut base = self.hero_button_rect(view, index, cx.measure)?;
                if status_read(view).is_some() {
                    // StatusOverlay's lone action has no focus pop or press scale by design.
                    return Some(Placed { rect: base, rest_rect: base, clip: Rect::FULL,
                        index: Some(index as u32) });
                }
                if matches!(at, At::Drawn) {
                    base.x += self.slide_offsets().map_or(0.0, |(_, incoming)| incoming);
                }
                let scale = match at {
                    At::Drawn => self.hero_pop.scale_with(index, cx.press.scale),
                    At::SpringTarget => {
                        if self.focused_loc(cx.focus.current) == Some(Located::Hero(index)) {
                            crate::ui::widgets::CTRL_FOCUS_SCALE
                        } else {
                            1.0
                        }
                    }
                };
                Some(Placed {
                    rect: base.scaled(scale),
                    rest_rect: base.scaled(crate::ui::widgets::CTRL_FOCUS_SCALE),
                    clip: Rect::FULL,
                    index: Some(index as u32),
                })
            }
            Located::Item(row, col) => {
                let shelf = self.grid.shelves.get(row)?;
                let snap = match at {
                    At::Drawn => self.snap.pos,
                    At::SpringTarget => self.snap_target,
                };
                let y = match at {
                    At::Drawn => shelf.base_y,
                    At::SpringTarget => {
                        let focus_row = self
                            .focused_grid(cx.focus.current)
                            .map(|(r, _)| r)
                            .unwrap_or(row);
                        GRID_TOP_Y + shelf_top_settled(row, focus_row) - self.grid.scroll_target
                    }
                };
                let base = Rect::new(
                    card_x(col, self.grid.eff_scroll(row, snap)),
                    y + CARD_DY,
                    CARD_W,
                    CARD_H,
                );
                let scale = match at {
                    At::Drawn => shelf.scale(col),
                    At::SpringTarget => {
                        if self.focused_grid(cx.focus.current) == Some((row, col)) {
                            RowStyle::HOME.focus_scale
                        } else {
                            1.0
                        }
                    }
                };
                Some(Placed {
                    rect: if matches!(at, At::Drawn) {
                        let focused = self.snap.pos > 0.5
                            && self.focused_grid(self.visible_focus(cx.focus.current)) == Some((row, col));
                        self.drawn_card_geometry(row, col, if focused { cx.press.scale } else { 1.0 }).0
                    } else { base.scaled(scale) },
                    rest_rect: base.scaled(RowStyle::HOME.focus_scale),
                    clip: Rect::new(
                        0.0,
                        crate::ui::widgets::TOP_BAR_BOTTOM,
                        SCR_W,
                        SCR_H - crate::ui::widgets::TOP_BAR_BOTTOM,
                    ),
                    index: Some(col as u32),
                })
            }
        }
    }

    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(located) = self.locate(want.elem) {
            let valid = match located {
                Located::Hero(index) => index < hero_group_len(H::hubs(cx)),
                Located::Item(_, _) => true,
            };
            if valid {
                return want;
            }
        }
        if let Some(saved) = self.items.iter().find(|k| k.elem == want.elem) {
            let hub_identity = match &saved.identity {
                HomeItemIdentity::Item { hub, .. } | HomeItemIdentity::Slot { hub, .. } => hub,
            };
            if let Some(row) = self.rows.iter().find(|r| &r.identity == hub_identity)
                .or_else(|| self.rows.get((saved.last_row as usize).min(self.rows.len().saturating_sub(1)))) {
                let col = (saved.last_col as usize).min(row.elems.len().saturating_sub(1));
                if let Some(&elem) = row.elems.get(col) {
                    return FocusKey {
                        entry: want.entry,
                        elem,
                    };
                }
            }
        }
        self.rows
            .iter()
            .find_map(|r| r.elems.first().copied())
            .map(|elem| FocusKey {
                entry: want.entry,
                elem,
            })
            .unwrap_or(FocusKey {
                entry: want.entry,
                elem: HERO_PLAY_ELEM,
            })
    }

    fn seat(&self, group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if group == HERO_GROUP {
            return FocusKey {
                entry: self.entry,
                elem: HERO_PLAY_ELEM,
            };
        }
        let Some((row_index, row)) = self.rows.iter().enumerate().find(|(_, r)| r.group == group)
        else {
            return FocusKey {
                entry: self.entry,
                elem: HERO_PLAY_ELEM,
            };
        };
        let col = card_row::column_near_x(
            from.rect.cx(),
            MARGIN_X,
            CARD_W + GAP,
            CARD_W,
            self.grid.shelves[row_index].scroll_x(),
            row.elems.len(),
            from.index.unwrap_or(0) as usize,
        );
        FocusKey {
            entry: self.entry,
            elem: row.elems.get(col).copied().unwrap_or(HERO_PLAY_ELEM),
        }
    }
}

impl HomeScreen {
    fn activation_elem(&self, engine_elem: u32) -> u32 {
        let engine_grid = matches!(self.locate(engine_elem), Some(Located::Item(_, _)));
        let picture_grid = self.snap.pos >= 0.5;
        if engine_grid != picture_grid {
            self.visible_activation.unwrap_or(engine_elem)
        } else {
            engine_elem
        }
    }

    fn hero_button_rect(
        &self,
        view: HubsView<'_>,
        index: usize,
        measure: &dyn Measure,
    ) -> Option<Rect> {
        if let Some((_, _, action)) = status_read(view) {
            if index != 0 || action.is_none() {
                return None;
            }
            let overlay = StatusOverlay::new(Rect::FULL, c"", StatusKind::Empty).action(action?);
            return overlay.action_frame_measured(measure);
        }
        let hero = self.selected_hero(view)?.item;
        let resumes = crate::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
        let label = if resumes { c"Continue" } else { c"Play" };
        let pill = Rect::new(
            MARGIN_X,
            HERO_ROW_Y,
            hero_pill_w(measure, label),
            HERO_CTRL_D,
        );
        match index {
            0 => Some(pill),
            1 => Some(Rect::new(
                pill.x + pill.w + HERO_CTRL_GAP,
                HERO_ROW_Y,
                HERO_CTRL_D,
                HERO_CTRL_D,
            )),
            _ => None,
        }
    }
}

impl<H: HomeLike> Machine<H> for HomeScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.sync_catalog(cx);
                Handled::Yes
            }
            ScreenEvent::RestoreMemory(PageMemory::Home(memory)) => {
                self.restore(memory);
                self.sync_catalog(cx);
                self.cta_available = hero_group_len(H::hubs(cx)) > 0;
                Handled::Yes
            }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover => {
                self.sync_catalog(cx);
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Tick(tick) => {
                self.tick(*tick, cx, fx);
                Handled::Yes
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == StoreId::Hubs.ord() => {
                self.projected_generation = None;
                self.sync_catalog(cx);
                if self.rows.is_empty() {
                    self.snap_target = 0.0;
                }
                fx.invalidate(Provenance::Landing(fx.from()));
                Handled::Yes
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                let from_loc = from.and_then(|key| self.locate(key.elem));
                let to_loc = self.locate(to.elem);
                if matches!(
                    (from_loc, to_loc),
                    (Some(Located::Hero(_)), Some(Located::Item(_, _)))
                        | (Some(Located::Item(_, _)), Some(Located::Hero(_)))
                ) {
                    self.visible_activation = from.map(|key| key.elem);
                }
                match to_loc {
                    Some(Located::Hero(_)) => self.snap_target = 0.0,
                    Some(Located::Item(_, _)) => self.snap_target = 1.0,
                    None => {}
                }
                if matches!(by, By::Restore) && self.restore_reveal {
                    // A restored page opens at its saved region, rather than replaying the
                    // hero-to-grid door with an invisible engine cursor during the transition.
                    self.restore_reveal = false;
                    self.snap.jump(self.snap_target);
                    self.visible_activation = None;
                    if let Some(Located::Item(row, col)) = to_loc {
                        // The saved viewport is a preference, not an assertion about today's
                        // catalog order. Minimally reveal the reconciled item before first draw.
                        let shelf = &mut self.grid.shelves[row];
                        let count = self.rows[row].elems.len();
                        let scroll = card_row::scroll_into_view(shelf.scroll_x(), col, count,
                            CARD_W, GAP, SCR_W - 2.0 * MARGIN_X);
                        shelf.restore_scroll(scroll, count, &RowStyle::HOME);
                        let (lo, hi) = row_reveal_band(shelf_top_settled(row, row));
                        self.grid.scroll_target = card_row::reveal(self.grid.scroll_y.pos,
                            lo, hi, grid_max_scroll(self.rows.len()));
                        self.grid.scroll_y.jump(self.grid.scroll_target);
                    }
                    self.layout_grid();
                }
                if to.elem >= crate::ui::dispatch::STRIP_BASE && matches!(by, By::Dir | By::Pointer)
                {
                    self.strip_chosen = true;
                } else if to_loc.is_some() {
                    self.strip_chosen = false;
                }
                if matches!(by, By::Dir | By::Pointer) {
                    self.hero_auto = HERO_AUTO_S;
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(elem) = cx
                    .focus
                    .current
                    .filter(|k| k.entry == self.entry)
                    .map(|k| k.elem)
                {
                    self.activate(elem, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.activate(*elem, cx, fx);
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => self.emit_item_menu(cx, fx),
            ScreenEvent::App(AppMsg::Home(command)) => {
                self.sync_catalog(cx);
                self.command(*command, cx, fx)
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key,
                        edge: Edge::Down | Edge::Repeat,
                        at_edge,
                        ..
                    },
                ..
            }) if *at_edge && matches!(key, Key::Left | Key::Right) => {
                let direction = if *key == Key::Left { -1 } else { 1 };
                if matches!(self.focused_loc(cx.focus.current), Some(Located::Hero(_)))
                    && self.flip(H::hubs(cx), direction)
                {
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key: Key::Back,
                        edge: Edge::Down,
                        ..
                    },
                ..
            }) => {
                if self.snap_target >= 0.5
                    || self.snap.pos >= 0.5
                    || self.focused_grid(cx.focus.current).is_some()
                {
                    self.snap_target = 0.0;
                    self.visible_activation = cx
                        .focus
                        .current
                        .filter(|key| key.entry == self.entry)
                        .map(|_| HERO_PLAY_ELEM);
                    fx.push(Fx::App(AppFx::Home(HomeReq::FoldToHero)));
                    fx.invalidate(Provenance::Input);
                } else {
                    fx.push(Fx::App(AppFx::Loop(LoopReq::BackAtRoot)));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: HomeLike> Screen<H> for HomeScreen {
    fn name(&self) -> &'static str {
        super::registry::word::HOME
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        self.draw_page(f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn strip_reachable(&self) -> bool {
        self.snap.pos < 0.5
    }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link {
            from: crate::ui::containers::tabs::STRIP,
            dir: Dir::Down,
            to: HERO_GROUP,
        });
        out.push(Link {
            from: HERO_GROUP,
            dir: Dir::Up,
            to: crate::ui::containers::tabs::STRIP,
        });
        if let Some(first) = self.rows.first() {
            out.push(Link {
                from: HERO_GROUP,
                dir: Dir::Down,
                to: first.group,
            });
            out.push(Link {
                from: first.group,
                dir: Dir::Up,
                to: HERO_GROUP,
            });
        }
    }
    fn memory(&self) -> H::Memory {
        PageMemory::Home(HomeMemory {
            groups: self.groups.clone(),
            items: self.items.clone(),
            next_group: self.next_group,
            next_elem: self.next_elem,
            carousel: self.carousel.clone(),
            strip_chosen: self.strip_chosen,
            scroll_y: self.grid.scroll_y.pos,
            row_scroll: self.rows.iter().enumerate()
                .map(|(index, row)| (row.group.0, self.grid.shelves[index].scroll_x()))
                .chain(self.restored_scroll.iter().copied()).collect(),
        })
    }
    fn memory_at(&self, _focus: Option<FocusKey<u32>>) -> H::Memory {
        <HomeScreen as Screen<H>>::memory(self)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn write_hub_identity(identity: &HomeHubIdentity, c: &mut Canon) {
    match identity {
        HomeHubIdentity::ContinueWatching => {
            c.u32(0);
        }
        HomeHubIdentity::Identifier { sid, id } => {
            c.u32(1).u32(u32::from(sid.raw())).str(id);
        }
        HomeHubIdentity::Key { sid, key } => {
            c.u32(2).u32(u32::from(sid.raw())).str(key);
        }
        HomeHubIdentity::Ephemeral {
            generation,
            ordinal,
        } => {
            c.u32(3).u32(*generation).u32(*ordinal);
        }
    }
}

fn write_item_identity(identity: &HomeItemIdentity, c: &mut Canon) {
    match identity {
        HomeItemIdentity::Item { hub, sid, rk } => {
            c.u32(0);
            write_hub_identity(hub, c);
            c.u32(u32::from(sid.raw())).str(rk);
        }
        HomeItemIdentity::Slot {
            hub,
            generation,
            ordinal,
        } => {
            c.u32(1);
            write_hub_identity(hub, c);
            c.u32(*generation).u32(*ordinal);
        }
    }
}

fn n_hubs_of(server_hubs: usize) -> usize {
    server_hubs.min(MAX_HUBS)
}
fn pinned_snap(target: f32, rows: usize) -> f32 {
    if rows == 0 {
        0.0
    } else {
        target
    }
}

fn status_read(
    view: HubsView<'_>,
) -> Option<(
    &'static std::ffi::CStr,
    StatusKind,
    Option<&'static std::ffi::CStr>,
)> {
    if view.hub_count() > 0 {
        return None;
    }
    Some(match view.state {
        crate::pms::HubState::Loading => {
            (c"Loading your library\u{2026}", StatusKind::Working, None)
        }
        crate::pms::HubState::Failed => (
            c"Can't reach your Plex server",
            StatusKind::Failed,
            Some(c"Try Again"),
        ),
        crate::pms::HubState::Ready => (
            c"Nothing on this server yet",
            StatusKind::Empty,
            Some(c"Refresh"),
        ),
    })
}

fn hero_group_len(view: HubsView<'_>) -> usize {
    match status_read(view) {
        Some((_, _, action)) => usize::from(action.is_some()),
        None => HERO_NBTN,
    }
}

fn hero_pill_w(measure: &dyn Measure, label: &std::ffi::CStr) -> f32 {
    theme::size::BODY as f32 * HERO_ICON_RATIO
        + HERO_ICON_GAP
        + measure.width(label, theme::size::BODY, true)
        + crate::ui::widgets::BTN_PILL_AIR
}

fn card_x(col: usize, scroll: f32) -> f32 {
    MARGIN_X + col as f32 * (CARD_W + GAP) - scroll
}
#[cfg(test)]
fn col_at(x: f32, scroll: f32, count: usize) -> Option<usize> {
    (0..count).find(|&col| {
        let left = card_x(col, scroll);
        x >= left && x <= left + CARD_W
    })
}
fn heading_y(row_y: f32, lift: f32) -> f32 {
    row_y - TITLE_DY - lift
}
fn row_reveal_band(top: f32) -> (f32, f32) {
    let lo = top + GRID_TOP_Y + CARD_DY + CARD_H + 96.0 - (SCR_H - MARGIN_Y);
    (lo, top)
}
fn grid_max_scroll(rows: usize) -> f32 {
    let n = rows.max(1);
    let h = card_row::settled_top(n, Some(n - 1), card_row::ROW_PITCH_FIXED);
    (h - (SCR_H - CONTENT_Y) + 60.0).max(0.0)
}
fn shelf_top_settled(row: usize, focus_row: usize) -> f32 {
    card_row::settled_top(row, Some(focus_row), card_row::ROW_PITCH_FIXED)
}

fn wash_corners(hero: Option<&PmsMovie>, grid: [[f32; 4]; 4], snap: f32) -> [[f32; 4]; 4] {
    let hero = hero
        .filter(|m| m.has_blur)
        .map(|m| AmbientWash::keyed(m.blur, HERO_WASH_W))
        .unwrap_or([theme::SURFACE_APP; 4]);
    let t = snap.clamp(0.0, 1.0);
    std::array::from_fn(|i| theme::mix(hero[i], grid[i], t))
}
fn wash_hidden(snap: f32, art: f32, outgoing: Option<f32>) -> bool {
    snap <= 0.005 && art > 0.995 && outgoing.map(|a| a > 0.995).unwrap_or(true)
}
fn reveal(tex: u32, spring: &Spring) -> f32 {
    if tex == 0 {
        0.0
    } else {
        spring.pos
    }
}
fn art_rect(tex: (u32, f32, f32), snap: f32, dx: f32) -> Rect {
    Rect::new(dx, -snap * (SCR_H - 120.0), SCR_W, SCR_H).cover(tex.1, tex.2)
}
fn backdrop_art(p: Painter, tex: (u32, f32, f32), snap: f32, dx: f32, alpha: f32) {
    if tex.0 == 0 || alpha <= 0.01 || !on_axis(dx, SCR_W, SCR_W, 0.0) {
        return;
    }
    p.tex(
        tex.0,
        art_rect(tex, snap, dx),
        0.0,
        theme::with_a(theme::TINT_WHITE, alpha * (1.0 - snap)),
    );
}
fn hero_logo_rk(item: &PmsMovie) -> &str {
    if item.kind == 3 && !item.show_rk.is_empty() {
        &item.show_rk
    } else {
        &item.rk
    }
}
fn prefetch_order(cur: i32, n: i32, out: &mut [i32; 2 * HERO_PREFETCH]) -> usize {
    if n <= 1 {
        return 0;
    }
    let cur = cur.rem_euclid(n);
    let mut count = 0;
    for distance in 1..=HERO_PREFETCH as i32 {
        for step in [distance, -distance] {
            let index = (cur + step).rem_euclid(n);
            if index != cur && !out[..count].contains(&index) {
                out[count] = index;
                count += 1;
            }
        }
    }
    count
}
fn prefetch_armed(snap: f32, sliding: bool) -> bool {
    snap < 0.05 && !sliding
}

fn display_source(real: &str) -> &str {
    static OVERRIDE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| crate::dev::read("shared"))
        .as_deref()
        .unwrap_or(real)
}

fn shared_by(source: &str) -> String {
    crate::ui::fmt::shared_by(source).unwrap_or_default()
}
fn meta_source_flow(
    base_w: f32,
    source: &str,
    mut run: impl FnMut(&str, f32, f32, i32, i32, [f32; 4]) -> f32,
) -> f32 {
    if source.is_empty() {
        return base_w;
    }
    let mut dx = base_w + SOURCE_PAD;
    dx += run(
        "\u{b7}",
        dx,
        META_FLOW_W - dx,
        theme::size::BODY,
        0,
        theme::TEXT_SEPARATOR,
    );
    dx += SOURCE_PAD;
    dx += run(
        &shared_by(source),
        dx,
        META_FLOW_W - dx,
        theme::size::BODY,
        0,
        theme::TEXT_TERTIARY,
    );
    dx
}
fn draw_meta_source(p: Painter, source: &str, x: f32, y: f32, base_w: f32, measure: &dyn Measure) {
    meta_source_flow(base_w, source, |text, dx, budget, size, bold, ink| {
        let elided = crate::text::elide_by(text, budget, false, |t| {
            measure.width_str(t, size, bold != 0)
        });
        let Ok(text) = CString::new(elided) else {
            return 0.0;
        };
        let mut label = Label::new(text.as_ptr(), size, ink).v(VAlign::CapTop);
        if bold == 1 {
            label = label.bold();
        }
        label.draw(p, Rect::new(x + dx, y, budget, 0.0))
    });
}
fn meta_drawn_w(view: &TextView<'_>, text: &str, width: f32, measure: &dyn Measure) -> f32 {
    if view.truncates(width) {
        return width;
    }
    measure.width_str(text, theme::size::BODY, false).min(width)
}
fn hero_content(hero: &PmsMovie, source: &str, p: Painter, dx: f32, measure: &dyn Measure) {
    if !on_axis(
        MARGIN_X + dx,
        if source.is_empty() {
            HERO_COL_W
        } else {
            META_FLOW_W
        },
        SCR_W,
        0.0,
    ) {
        return;
    }
    let episode = hero.kind == 3;
    let title = if episode && !hero.show_title.is_empty() {
        &hero.show_title
    } else {
        &hero.title
    };
    let title_h = hero_logo::band_h(LogoRung::Hero);
    let meta = if episode {
        let mut text = String::new();
        if hero.season_index > 0 {
            text.push_str(&format!("S{} ", hero.season_index));
        }
        if hero.ep_index > 0 {
            text.push_str(&format!("E{}", hero.ep_index));
        }
        if !text.is_empty() && !hero.title.is_empty() {
            text.push_str(" \u{b7} ");
        }
        text.push_str(&hero.title);
        text
    } else {
        format!(
            "{} \u{b7} {} \u{b7} {}",
            if hero.kind == 1 { "Show" } else { "Movie" },
            hero.year,
            if hero.rating.is_empty() {
                "NR"
            } else {
                &hero.rating
            }
        )
    };
    let meta_view = TextView::new(&meta, theme::size::BODY, theme::TEXT_SECONDARY).max_lines(1);
    let meta_h = meta_view.measure_h(HERO_COL_W);
    let synopsis = (!hero.summary.is_empty()).then(|| crate::ui::hero_synopsis(&hero.summary, ""));
    let synopsis_h = synopsis
        .as_ref()
        .map(|v| theme::space::SM + v.measure_h(HERO_COL_W))
        .unwrap_or(0.0);
    let mut y = hero_stack_top(title_h, meta_h, synopsis_h);
    HeroLogo::new(hero.sid, hero_logo_rk(hero), title, LogoRung::Hero)
        .draw(p, Rect::new(MARGIN_X, y, HERO_COL_W, title_h), measure);
    y += title_h + theme::space::MD;
    meta_view.draw(p, Rect::new(MARGIN_X, y, HERO_COL_W, 0.0));
    if !source.is_empty() {
        draw_meta_source(
            p,
            source,
            MARGIN_X,
            y,
            meta_drawn_w(&meta_view, &meta, HERO_COL_W, measure),
            measure,
        );
    }
    y += meta_h;
    if let Some(view) = synopsis {
        view.draw(
            p,
            Rect::new(MARGIN_X, y + theme::space::SM, HERO_COL_W, 0.0),
        );
    }
}

#[cfg(test)]
mod tests;
