//! Owned Detail page (restructure phase 7).
//!
//! The six focus groups are composed here; section modules own their geometry and paint. Focus and
//! remembered group cursors belong exclusively to the input engine. This instance stores only
//! content decisions (season debounce/restoration) and render state (springs, metrics and caches).

mod about;
mod cast;
mod episodes;
mod hero;
mod related;
mod season;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod geometry_tests;
#[cfg(test)]
mod identity_tests;

use crate::metadata::{Detail, Spot};
use crate::plex::ServerId;
use crate::stores::metadata::{apply as apply_metadata, MetadataCmd};
use crate::stores::viewstate::{apply as apply_viewstate, ViewStateCmd};
use crate::stores::StoreId;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::frame::Budget;
use crate::ui::hero_logo::{HeroLogo, LogoRung};
use crate::ui::label::HAlign;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent, InputKind, Key,
    Leave, LogicalState, Machine,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::screen::{
    Activate, At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable,
    GroupKind, GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat,
    Step, Stop,
};
use crate::ui::widgets::{
    AmbientWash, Button, CircleButton, ControlPalette, CtlPop, PosterMark, TabStrip,
};
use crate::ui::{hero_alpha, theme, Env, Painter, Rect, Spring, View};
use std::borrow::Cow;

use super::registry::{AppFx, AppMsg, ContentArg, ContentLike, ContentReq, PageMemory, DetailIdentity, DetailKey, DetailMemory};

const FIRST_ITEM_ELEM: u32 = 2048;

const SECTION_GAP: f32 = theme::space::XL;
const TAB_EP_GAP: f32 = theme::space::MD;
const HERO_FADE: f32 = 400.0;
const EP_SCALE_MAX: usize = 40;
const K_SCROLL: f32 = crate::ui::consts::K_SCROLL;
const K_STRIP_SCROLL: f32 = 240.0;

pub(crate) const SHAPE: &str = "DetailScreen{return_pending:bool,next_elem:u32,keys:[DetailKey{identity:DetailIdentity,elem:u32}],sid:u32,rk:str,pending_season:opt<u32>,season_settle:f32,restore:opt<RestoreIntent{spot:Spot{section:u32,col:u32,ep_text:bool,saved_col:[u32;6],season:opt<u64>},episode:opt<str>,season_requested:bool}>,panel:u8}";

const _: () = assert!(hero::HERO_ELEM_RANGE_END == season::SEASON_ELEM_RANGE_START);
const _: () = assert!(season::SEASON_ELEM_RANGE_END == episodes::EPISODES_ELEM_RANGE_START);
const _: () = assert!(episodes::EPISODES_ELEM_RANGE_END == related::RELATED_ELEM_RANGE_START);
const _: () = assert!(related::RELATED_ELEM_RANGE_END == cast::CAST_ELEM_RANGE_START);
const _: () = assert!(cast::CAST_ELEM_RANGE_END == about::ABOUT_ELEM_RANGE_START);
const _: () = assert!(about::ABOUT_ELEM_RANGE_START < about::ABOUT_ELEM_RANGE_END);

#[derive(Clone)]
struct RestoreIntent {
    spot: Spot,
    episode: Option<String>,
    season_requested: bool,
}

pub(crate) struct DetailScreen {
    entry: EntryId,
    sid: ServerId,
    rk: String,
    keys: Vec<DetailKey>,
    next_elem: u32,
    // Published identity projections, rebuilt only at mount/landings, never while drawing.
    key_by_local: std::collections::HashMap<u32, u32>,
    local_by_key: std::collections::HashMap<u32, u32>,
    return_pending: bool,

    // Logical decisions. None is a focus cursor.
    pending_season: Option<usize>,
    season_settle: f32,
    restore_intent: Option<RestoreIntent>,

    // Render state.
    scroll: Spring,
    scroll_target: f32,
    episode_scroll: Spring,
    tab_scroll: Spring,
    episode_scale: [Spring; EP_SCALE_MAX],
    related: CardRow,
    cast: CardRow,
    tabs: TabStrip,
    season_pop: CtlPop<1>,
    ctl_pop: CtlPop<4>,
    disc_unfurl: [Spring; 2],
    season_metrics: season::Metrics,
    about_rows: about::Rows,
    ground: AmbientWash,
    spin_ms: f32,
}

impl DetailScreen {
    pub(crate) fn new(entry: EntryId, sid: ServerId, rk: String) -> Self {
        let selected = crate::pms::movie(crate::pms::index_of_rk(sid, &rk).max(0) as usize)
            .filter(|_| crate::pms::index_of_rk(sid, &rk) >= 0);
        let mut ground = AmbientWash::flat(theme::SURFACE_APP);
        if let Some(m) = selected.filter(|m| m.has_blur) {
            ground.jump(AmbientWash::keyed(m.blur, [AmbientWash::GROUND_W; 4]));
        }
        apply_metadata(MetadataCmd::RequestDetail {
            sid,
            rk: rk.clone(),
        });
        Self {
            entry,
            sid,
            rk,
            keys: Vec::new(),
            next_elem: FIRST_ITEM_ELEM,
            key_by_local: Default::default(),
            local_by_key: Default::default(),
            return_pending: false,
            pending_season: None,
            season_settle: 0.0,
            restore_intent: None,
            scroll: Spring::at(0.0),
            scroll_target: 0.0,
            episode_scroll: Spring::at(0.0),
            tab_scroll: Spring::at(0.0),
            episode_scale: [Spring::at(1.0); EP_SCALE_MAX],
            related: CardRow::new(),
            cast: CardRow::new(),
            tabs: TabStrip::new(),
            season_pop: CtlPop::new(),
            ctl_pop: CtlPop::new(),
            disc_unfurl: [Spring::at(0.0); 2],
            season_metrics: season::Metrics::new(),
            about_rows: about::Rows::new(),
            ground,
            spin_ms: 0.0,
        }
    }

    pub(crate) fn restore_memory(&mut self, memory: &DetailMemory) {
        // A covered live body may have interned a landing after the request snapshot. Never
        // rewind its registry/counter: those integers still belong to the identities it minted.
        for saved in &memory.keys {
            if !self.keys.iter().any(|key| key.identity == saved.identity) {
                assert!(!self.keys.iter().any(|key| key.elem == saved.elem), "restored key collision");
                self.keys.push(saved.clone());
            }
        }
        self.next_elem = self.next_elem.max(memory.next_elem).max(FIRST_ITEM_ELEM).max(
            self.keys.iter().map(|key| key.elem).max().and_then(|n| n.checked_add(1)).unwrap_or(FIRST_ITEM_ELEM));
        self.restore(&memory.spot);
        self.return_pending = true;
        self.sync_keys();
    }

    fn engine_key(&self, local: u32) -> Option<u32> {
        if local < season::SEASON_ELEM_RANGE_START || local >= about::ABOUT_ELEM_RANGE_START && local < about::ABOUT_ELEM_RANGE_END {
            Some(local)
        } else {
            self.key_by_local.get(&local).copied()
        }
    }

    fn key_of(&self, located: Located) -> Option<u32> {
        self.engine_key(located.local_key()?)
    }

    fn sync_keys(&mut self) {
        let pending_key = self.pending_season.and_then(season::elem).and_then(|local| self.engine_key(local));
        let mut identities = Vec::new();
        if let Some(d) = self.detail() {
            for (i, season) in d.seasons.iter().enumerate() {
                if let Some(local) = season::elem(i) {
                    identities.push((local, DetailIdentity::Season { sid: d.sid, show: d.rk.clone(), rk: season.rk.clone() }));
                }
            }
            for (i, episode) in d.episodes.iter().enumerate() {
                for row in [episodes::Row::Still, episodes::Row::Text] {
                    if let Some(local) = episodes::elem(i, row) {
                        identities.push((local, DetailIdentity::Episode { sid: d.sid, rk: episode.rk.clone(), text: row == episodes::Row::Text }));
                    }
                }
            }
            for (i, related) in d.related.iter().enumerate() {
                if let Some(local) = related::elem(i) {
                    identities.push((local, DetailIdentity::Related { sid: related.sid, rk: related.rk.clone() }));
                }
            }
            for i in 0..d.credits_len() {
                if let (Some(local), Some(cast)) = (cast::elem(i), d.credit(i)) {
                    let key = cast.person_key();
                    let name = if key.is_empty() && cast.tag_key.is_empty() { cast.tag.clone() } else { String::new() };
                    identities.push((local, DetailIdentity::Cast { sid: d.sid, key, guid: cast.tag_key.clone(), name, role: cast.role.clone() }));
                }
            }
        }
        let mut interned: std::collections::HashMap<DetailIdentity, u32> = self.keys.iter().map(|key| (key.identity.clone(), key.elem)).collect();
        self.key_by_local.clear();
        self.local_by_key.clear();
        for (local, identity) in identities {
            let identity = match &identity {
                DetailIdentity::Season { rk, .. } | DetailIdentity::Episode { rk, .. } | DetailIdentity::Related { rk, .. } if rk.is_empty() => DetailIdentity::Slot(local),
                _ => identity,
            };
            let elem = *interned.entry(identity.clone()).or_insert_with(|| {
                let elem = self.next_elem;
                self.next_elem = elem.checked_add(1).expect("detail element-key space exhausted");
                assert!(self.next_elem < crate::ui::dispatch::STRIP_BASE, "detail keys must not overlap chrome");
                self.keys.push(DetailKey { identity, elem });
                elem
            });
            self.key_by_local.insert(local, elem);
            self.local_by_key.insert(elem, local);
        }
        if let Some(key) = pending_key {
            self.pending_season = self.local_by_key.get(&key).and_then(|local| season::locate(*local));
        }
    }

    pub(crate) fn restore(&mut self, spot: &Spot) {
        self.restore_episode(spot, None);
    }

    pub(crate) fn restore_episode(&mut self, spot: &Spot, episode: Option<&str>) {
        self.sync_keys();
        self.return_pending = false;
        self.restore_intent = Some(RestoreIntent {
            spot: spot.clone(),
            episode: episode.map(str::to_owned),
            season_requested: false,
        });
        self.scroll_target = 0.0;
    }

    pub(crate) fn spot(&self, focus: Option<FocusKey<u32>>) -> Spot {
        let (section, col, ep_text) = focus
            .filter(|k| k.entry == self.entry)
            .and_then(|k| self.locate(k.elem))
            .map(|l| {
                let col = match l {
                    Located::Hero(control) => hero::index_of(self.hero_set(), control).unwrap_or(0),
                    _ => l.index(),
                };
                (
                    l.section(),
                    col as i32,
                    matches!(l, Located::Episode(_, episodes::Row::Text)),
                )
            })
            .unwrap_or((0, 0, false));
        let season = self
            .detail()
            .and_then(|d| d.seasons.get(d.cur_season))
            .map(|s| s.index);
        Spot {
            section,
            col,
            ep_text,
            // Engine-owned remembered group cursors ride ReturnState separately. These fields stay
            // for legacy focusprobe/trail serialization only and are not a second authority.
            saved_col: [0; 6],
            season,
        }
    }

    pub(crate) fn focused_episode(
        &self,
        focus: Option<FocusKey<u32>>,
    ) -> Option<(String, PosterMark)> {
        if crate::metadata::season_loading() {
            return None;
        }
        let (i, row) = self.focused_episode_index(focus)?;
        if row != episodes::Row::Still {
            return None;
        }
        let ep = self.detail()?.episodes.get(i)?;
        Some((ep.rk.clone(), episodes::watch_state(ep)))
    }

    pub(crate) fn focused_season(
        &self,
        focus: Option<FocusKey<u32>>,
    ) -> Option<(String, PosterMark)> {
        if crate::metadata::season_loading() {
            return None;
        }
        let i = self.focused_index(focus, season::SEASON_GROUP)?;
        let s = self.detail()?.seasons.get(i)?;
        (!s.rk.is_empty()).then(|| (s.rk.clone(), season::watch_state(s)))
    }

    pub(crate) fn focused_related(
        &self,
        focus: Option<FocusKey<u32>>,
    ) -> Option<&'static crate::pms::PmsMovie> {
        let key = focus.filter(|k| k.entry == self.entry)?.elem;
        related::item(self.detail()?, self.locate(key)?.local_key()?)
    }

    pub(crate) fn focused_rect<H: ContentLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        cx: &Cx<'_, H>,
        at: At,
    ) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        self.place(&key.elem, cx, at).map(|p| p.rect)
    }

    pub(crate) fn redraw_focused<H: ContentLike>(
        &self,
        f: &mut DrawFrame<'_, H>,
        focus: Option<FocusKey<u32>>,
    ) {
        let Some(d) = self.detail() else { return };
        match focus.and_then(|k| self.locate(k.elem)) {
            Some(Located::Season(i)) => season::draw(
                f.painter
                    .translate(0.0, self.section_top(1, d) - self.scroll.pos),
                &self.season_metrics,
                self.tabs,
                d.cur_season,
                Some(i),
                self.tab_scroll.pos,
                self.season_pop.scale(0),
            ),
            Some(Located::Episode(i, row)) => episodes::draw_focused(
                f.painter,
                d,
                i,
                row,
                self.section_top(2, d) - self.scroll.pos,
                self.episode_scroll.pos,
                self.episode_scale.get(i).map(|s| s.pos).unwrap_or(1.0) * f.press.scale,
            ),
            Some(Located::Related(i)) => related::draw_focused(
                f.painter,
                d,
                &self.related,
                i,
                self.section_top(3, d) - self.scroll.pos,
                f.press.scale,
            ),
            _ => {}
        }
    }

    fn detail(&self) -> Option<&'static Detail> {
        crate::metadata::current().filter(|d| {
            crate::plex::same_item((d.sid, d.rk.as_str()), (self.sid, self.rk.as_str()))
        })
    }

    fn selected(&self) -> Option<&'static crate::pms::PmsMovie> {
        let i = crate::pms::index_of_rk(self.sid, &self.rk);
        (i >= 0).then(|| crate::pms::movie(i as usize)).flatten()
    }

    fn hero_chain(&self) -> crate::ui::detail_layout::HeroChain {
        let (lead, synopsis) = hero_blurb(self.detail(), self.selected());
        let synopsis_h = crate::ui::hero_synopsis(&synopsis, &lead)
            .measure_h(crate::ui::detail_layout::HERO_TEXT_W);
        crate::ui::detail_layout::hero_chain(
            synopsis_h,
            self.detail()
                .is_some_and(|detail| !detail.ratings.is_empty()),
        )
    }

    fn content_top(&self) -> f32 {
        self.hero_chain().btn_y + hero::CD + theme::space::XL
    }

    fn sections(&self, d: Option<&Detail>) -> ([i32; 6], usize) {
        let mut out = [0; 6];
        let mut n = 1;
        if let Some(d) = d {
            if d.is_show && !d.seasons.is_empty() {
                out[n] = 1;
                n += 1;
            }
            if d.is_show && !d.episodes.is_empty() {
                out[n] = 2;
                n += 1;
            }
            if d.credits_len() > 0 {
                out[n] = 4;
                n += 1;
            }
            if !d.related.is_empty() {
                out[n] = 3;
                n += 1;
            }
            out[n] = 5;
            n += 1;
        }
        (out, n)
    }

    fn section_top(&self, section: i32, d: &Detail) -> f32 {
        let (sections, n) = self.sections(Some(d));
        let mut y = self.content_top();
        for (pos, &sec) in sections[..n].iter().enumerate().skip(1) {
            if sec == section {
                return y;
            }
            y += self.block_h(sec, d);
            let next = sections.get(pos + 1).copied();
            y += if sec == 1 && next == Some(2) {
                TAB_EP_GAP
            } else {
                SECTION_GAP
            };
        }
        y
    }

    fn block_h(&self, section: i32, d: &Detail) -> f32 {
        match section {
            1 => season::ROW_H,
            2 => episodes::block_h(d),
            3 => related::block_h(),
            4 => cast::block_h(),
            _ => 0.0,
        }
    }

    fn locate(&self, elem: u32) -> Option<Located> {
        let local = if elem >= FIRST_ITEM_ELEM {
            // The projections describe only our currently published item.
            self.detail()?;
            *self.local_by_key.get(&elem)?
        } else if elem < season::SEASON_ELEM_RANGE_START || elem >= about::ABOUT_ELEM_RANGE_START {
            elem
        } else {
            return None;
        };
        Self::locate_local(local)
    }

    fn locate_local(elem: u32) -> Option<Located> {
        if let Some(c) = hero::HeroCtl::of_elem(elem) {
            return Some(Located::Hero(c));
        }
        if let Some(i) = season::locate(elem) {
            return Some(Located::Season(i));
        }
        if let Some((i, row)) = episodes::locate(elem) {
            return Some(Located::Episode(i, row));
        }
        if let Some(i) = related::locate(elem) {
            return Some(Located::Related(i));
        }
        if let Some(i) = cast::locate(elem) {
            return Some(Located::Cast(i));
        }
        about::locate(elem, crate::ui::tracks_panel::is_available()).map(Located::About)
    }

    fn focused_index(&self, focus: Option<FocusKey<u32>>, group: GroupId) -> Option<usize> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        let located = self.locate(key.elem)?;
        (located.group() == group).then(|| located.index())
    }

    fn focused_episode_index(
        &self,
        focus: Option<FocusKey<u32>>,
    ) -> Option<(usize, episodes::Row)> {
        match focus
            .filter(|k| k.entry == self.entry)
            .and_then(|k| self.locate(k.elem))?
        {
            Located::Episode(i, row) => Some((i, row)),
            _ => None,
        }
    }

    fn restore_focus(&self) -> Option<u32> {
        if self.return_pending { return None; }
        let intent = self.restore_intent.as_ref()?;
        let d = self.detail()?;
        if season::restore_step(
            Some(d),
            intent.spot.season,
            intent.season_requested,
            crate::metadata::season_loading(),
        ) != season::RestoreStep::Ready
        {
            return None;
        }
        let clamp = |col: i32, len: usize| (col.max(0) as usize).min(len.saturating_sub(1));
        let local = match intent.spot.section {
            0 => {
                let set = self.hero_set();
                let (_, n) = hero::hero_ctls(set);
                hero::ctl_at(set, clamp(intent.spot.col, n)).map(hero::HeroCtl::elem)
            }
            1 if !d.seasons.is_empty() => season::elem(d.cur_season.min(d.seasons.len() - 1)),
            2 if !d.episodes.is_empty() => {
                let index = match &intent.episode {
                    Some(rk) => d
                        .episodes
                        .iter()
                        .position(|episode| &episode.rk == rk)
                        .unwrap_or(0),
                    None => clamp(intent.spot.col, d.episodes.len()),
                };
                episodes::elem(
                    index,
                    if intent.spot.ep_text {
                        episodes::Row::Text
                    } else {
                        episodes::Row::Still
                    },
                )
            }
            3 if !d.related.is_empty() => related::elem(clamp(intent.spot.col, d.related.len())),
            4 if d.credits_len() > 0 => cast::elem(clamp(intent.spot.col, d.credits_len())),
            5 => Some(
                if intent.spot.col > 0 && crate::ui::tracks_panel::is_available() {
                    about::LANGUAGES_ELEM
                } else {
                    about::CARD_ELEM
                },
            ),
            _ => Some(hero::ELEM_PLAY),
        }?;
        self.engine_key(local)
    }
}

impl<H: ContentLike> Focusable<H> for DetailScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let set = self.hero_set();
        let widths = hero::hero_widths(
            cx.measure,
            set,
            set.restart,
            [self.disc_unfurl[0].pos, self.disc_unfurl[1].pos],
            self.named_show(),
        );
        let (_, hero_n) = hero::hero_ctls(set);
        let hero_y = self.hero_chain().btn_y;
        let hero_last = hero::hero_btn_rect_at(set, hero_n.saturating_sub(1), hero_y, widths);
        out.push(GroupSpec {
            id: hero::HERO_GROUP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [
                EdgeRule::Stop,
                EdgeRule::Geometric,
                EdgeRule::Stop,
                EdgeRule::Stop,
            ],
            extent: Rect::new(
                crate::ui::consts::MARGIN_X,
                hero_y - self.scroll_target,
                hero_last.x + hero_last.w - crate::ui::consts::MARGIN_X,
                hero::CD,
            ),
            len: hero_n,
            elem: ElemKind::Control,
        });

        let Some(d) = self.detail() else { return };
        let (sections, n) = self.sections(Some(d));
        for &section in &sections[1..n] {
            let top = self.section_top(section, d) - self.scroll_target;
            match section {
                1 => out.push(GroupSpec {
                    id: season::SEASON_GROUP,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::Nearest,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(
                        crate::ui::consts::MARGIN_X,
                        top,
                        crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                        season::ROW_H,
                    ),
                    len: d.seasons.len().min(64),
                    elem: ElemKind::Card,
                }),
                2 => out.push(GroupSpec {
                    id: episodes::EPISODES_GROUP,
                    kind: GroupKind::Grid {
                        cols: d.episodes.len().min(episodes::MAX_ITEMS).max(1),
                        holes: &[],
                    },
                    seat: Seat::Nearest,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(
                        crate::ui::consts::MARGIN_X,
                        top,
                        crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                        episodes::block_h(d),
                    ),
                    len: d.episodes.len().min(episodes::MAX_ITEMS) * 2,
                    elem: ElemKind::Card,
                }),
                3 => out.push(GroupSpec {
                    id: related::RELATED_GROUP,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::Remembered,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(
                        crate::ui::consts::MARGIN_X,
                        top,
                        crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                        related::block_h(),
                    ),
                    len: d.related.len().min(512),
                    elem: ElemKind::Card,
                }),
                4 => out.push(GroupSpec {
                    id: cast::CAST_GROUP,
                    kind: GroupKind::Row { wrap: false },
                    seat: Seat::Remembered,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                    extent: Rect::new(
                        crate::ui::consts::MARGIN_X,
                        top,
                        crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                        cast::block_h(),
                    ),
                    len: d.credits_len().min(512),
                    elem: ElemKind::Card,
                }),
                5 => {
                    let tracks = crate::ui::tracks_panel::is_available();
                    out.push(GroupSpec {
                        id: about::ABOUT_GROUP,
                        kind: GroupKind::Column,
                        seat: Seat::First,
                        reachable: AxisMask::BOTH,
                        edge: [
                            EdgeRule::Geometric,
                            EdgeRule::Stop,
                            EdgeRule::Stop,
                            EdgeRule::Stop,
                        ],
                        extent: Rect::new(
                            crate::ui::consts::MARGIN_X,
                            top,
                            crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                            crate::ui::consts::SCR_H - crate::ui::detail_layout::TOP_MARGIN,
                        ),
                        len: 1 + usize::from(tracks),
                        elem: ElemKind::Control,
                    });
                }
                _ => {}
            }
        }
    }

    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        let located = self.locate(*key)?;
        self.valid(located).then(|| located.group())
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let Some(located) = self.locate(key.elem).filter(|l| self.valid(*l)) else {
            return Step::Edge;
        };
        let moved = match located {
            Located::Hero(ctl) => {
                let set = self.hero_set();
                let Some(i) = hero::index_of(set, ctl) else {
                    return Step::Edge;
                };
                let (controls, n) = hero::hero_ctls(set);
                match dir {
                    Dir::Left if i > 0 => Some(controls[i - 1].elem()),
                    Dir::Right if i + 1 < n => Some(controls[i + 1].elem()),
                    _ => None,
                }
            }
            Located::Season(i) => {
                row_move(i, self.detail().map(|d| d.seasons.len()).unwrap_or(0), dir)
                    .and_then(season::elem)
            }
            Located::Episode(i, row) => match dir {
                Dir::Left if i > 0 => episodes::elem(i - 1, row),
                Dir::Right if self.detail().is_some_and(|d| i + 1 < d.episodes.len()) => {
                    episodes::elem(i + 1, row)
                }
                Dir::Down if row == episodes::Row::Still => episodes::elem(i, episodes::Row::Text),
                Dir::Up if row == episodes::Row::Text => episodes::elem(i, episodes::Row::Still),
                _ => None,
            },
            Located::Related(i) => {
                row_move(i, self.detail().map(|d| d.related.len()).unwrap_or(0), dir)
                    .and_then(related::elem)
            }
            Located::Cast(i) => {
                row_move(i, self.detail().map(|d| d.credits_len()).unwrap_or(0), dir)
                    .and_then(cast::elem)
            }
            Located::About(i) => match (i, dir, crate::ui::tracks_panel::is_available()) {
                (0, Dir::Down, true) => Some(about::LANGUAGES_ELEM),
                (1, Dir::Up, _) => Some(about::CARD_ELEM),
                _ => None,
            },
        };
        moved
            .and_then(|local| self.engine_key(local))
            .map(|elem| {
                Step::Move(FocusKey {
                    entry: key.entry,
                    elem,
                })
            })
            .unwrap_or(Step::Edge)
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let located = self.locate(*key).filter(|l| self.valid(*l))?;
        let d = self.detail();
        let vertical = if at == At::Drawn {
            self.scroll.pos
        } else {
            self.scroll_target
        };
        let (rect, rest_rect, index) = match located {
            Located::Hero(ctl) => {
                let set = self.hero_set();
                let i = hero::index_of(set, ctl)?;
                let widths = hero::hero_widths(
                    cx.measure,
                    set,
                    set.restart,
                    [self.disc_unfurl[0].pos, self.disc_unfurl[1].pos],
                    self.named_show(),
                );
                let base =
                    hero::hero_btn_rect_at(set, i, self.hero_chain().btn_y - vertical, widths);
                (
                    base.scaled(self.ctl_pop.scale(i)),
                    base.scaled(crate::ui::widgets::CTRL_FOCUS_SCALE),
                    Some(i as u32),
                )
            }
            Located::Season(i) => {
                let base = self.season_metrics.rect(
                    i,
                    self.section_top(1, d?) - vertical,
                    self.tab_scroll.pos,
                )?;
                (
                    base.scaled(self.season_pop.scale(0)),
                    base.scaled(crate::ui::widgets::CTRL_FOCUS_SCALE),
                    Some(i as u32),
                )
            }
            Located::Episode(i, row) => {
                let d = d?;
                let top = self.section_top(2, d) - vertical;
                let base = match row {
                    episodes::Row::Still => episodes::still_rect(i, top, self.episode_scroll.pos),
                    episodes::Row::Text => {
                        episodes::meta_rect(d.episodes.get(i)?, i, top, self.episode_scroll.pos)
                    }
                };
                let drawn = if row == episodes::Row::Still {
                    base.scaled(self.episode_scale.get(i).map(|s| s.pos).unwrap_or(1.0))
                } else {
                    base
                };
                let rest = if row == episodes::Row::Still {
                    base.scaled(crate::ui::widgets::CARD_FOCUS_SCALE)
                } else {
                    base
                };
                (drawn, rest, Some(i as u32))
            }
            Located::Related(i) => {
                let top = self.section_top(3, d?) - vertical;
                (
                    related::rect(&self.related, i, top, at == At::Drawn),
                    related::rect(&self.related, i, top, false),
                    Some(i as u32),
                )
            }
            Located::Cast(i) => {
                let top = self.section_top(4, d?) - vertical;
                (
                    cast::rect(&self.cast, i, top, at == At::Drawn),
                    cast::rect(&self.cast, i, top, false),
                    Some(i as u32),
                )
            }
            Located::About(i) => {
                let d = d?;
                let top = self.section_top(5, d) - vertical;
                let base = if i == 0 {
                    self.about_rows.card_rect(d, top)
                } else {
                    self.about_rows.languages_rect(top)
                };
                (base, base, Some(i as u32))
            }
        };
        Some(Placed {
            rect,
            rest_rect,
            clip: Rect::FULL,
            index,
        })
    }

    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let known = self.keys.iter().any(|key| key.elem == want.elem);
        if known && (self.return_pending || self.restore_intent.is_some()) && (crate::metadata::detail_request_status(self.sid, &self.rk) == Some(true)
            || self.detail().is_some() && (crate::metadata::season_loading() || self.return_waiting())) {
            return want;
        }
        if self.detail().is_some() {
            if let Some(DetailIdentity::Slot(local)) = self.keys.iter().find(|key| key.elem == want.elem).map(|key| &key.identity) {
                if let Some(elem) = self.engine_key(*local) {
                    return FocusKey { entry: want.entry, elem };
                }
            }
        }
        if let Some(elem) = self.restore_focus() {
            return FocusKey {
                entry: want.entry,
                elem,
            };
        }
        if let Some(Located::Hero(ctl)) = self.locate(want.elem) {
            let set = self.hero_set();
            if hero::index_of(set, ctl).is_some() {
                return want;
            }
            if ctl.is_watch() {
                let (controls, _) = hero::hero_ctls(set);
                return FocusKey {
                    entry: want.entry,
                    elem: controls[hero::watch_index(set).unwrap()].elem(),
                };
            }
        }
        if self
            .locate(want.elem)
            .is_some_and(|located| self.valid(located))
        {
            return want;
        }
        FocusKey {
            entry: want.entry,
            elem: hero::HeroCtl::Play.elem(),
        }
    }

    fn seat(&self, group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let d = self.detail();
        let from_i = from.index.unwrap_or(0) as usize;
        let elem = if group == hero::HERO_GROUP {
            hero::HeroCtl::Play.elem()
        } else if group == season::SEASON_GROUP {
            let n = d.map(|d| d.seasons.len()).unwrap_or(0).min(64);
            let i = nearest_variable(
                &self.season_metrics,
                from.rect.cx(),
                self.tab_scroll.pos,
                n,
                from_i,
            );
            season::elem(i).unwrap_or(season::SEASON_ELEM_RANGE_START)
        } else if group == episodes::EPISODES_GROUP {
            let n = d
                .map(|d| d.episodes.len())
                .unwrap_or(0)
                .min(episodes::MAX_ITEMS);
            let i = card_row::column_near_x(
                from.rect.cx(),
                crate::ui::consts::MARGIN_X,
                episodes::W + episodes::GAP,
                episodes::W,
                self.episode_scroll.pos,
                n,
                from_i,
            );
            let row = if d.is_some_and(|d| {
                from.rect.cy() > self.section_top(2, d) - self.scroll_target + episodes::block_h(d)
            }) {
                episodes::Row::Text
            } else {
                episodes::Row::Still
            };
            episodes::elem(i, row).unwrap_or(episodes::EPISODES_ELEM_RANGE_START)
        } else if group == related::RELATED_GROUP {
            let n = d.map(|d| d.related.len()).unwrap_or(0).min(512);
            related::elem(card_row::column_near_x(
                from.rect.cx(),
                crate::ui::consts::MARGIN_X,
                RowStyle::HOME.w + RowStyle::HOME.gap,
                RowStyle::HOME.w,
                self.related.scroll_x(),
                n,
                from_i,
            ))
            .unwrap_or(related::RELATED_ELEM_RANGE_START)
        } else if group == cast::CAST_GROUP {
            let n = d.map(|d| d.credits_len()).unwrap_or(0).min(512);
            cast::elem(card_row::column_near_x(
                from.rect.cx(),
                crate::ui::consts::MARGIN_X,
                RowStyle::CAST.w + RowStyle::CAST.gap,
                RowStyle::CAST.w,
                self.cast.scroll_x(),
                n,
                from_i,
            ))
            .unwrap_or(cast::CAST_ELEM_RANGE_START)
        } else {
            about::CARD_ELEM
        };
        FocusKey {
            entry: self.entry,
            elem: Self::locate_local(elem).and_then(|located| self.key_of(located)).unwrap_or(hero::ELEM_PLAY),
        }
    }
}

impl DetailScreen {
    fn valid(&self, located: Located) -> bool {
        let d = self.detail();
        match located {
            Located::Hero(c) => hero::index_of(self.hero_set(), c).is_some(),
            Located::Season(i) => d.is_some_and(|d| i < d.seasons.len().min(64)),
            Located::Episode(i, _) => {
                d.is_some_and(|d| i < d.episodes.len().min(episodes::MAX_ITEMS))
            }
            Located::Related(i) => d.is_some_and(|d| i < d.related.len().min(512)),
            Located::Cast(i) => d.is_some_and(|d| i < d.credits_len().min(512)),
            Located::About(0) => d.is_some(),
            Located::About(1) => d.is_some() && crate::ui::tracks_panel::is_available(),
            Located::About(_) => false,
        }
    }
}

fn row_move(index: usize, len: usize, dir: Dir) -> Option<usize> {
    match dir {
        Dir::Left if index > 0 => Some(index - 1),
        Dir::Right if index + 1 < len => Some(index + 1),
        _ => None,
    }
}

fn nearest_variable(metrics: &season::Metrics, x: f32, scroll: f32, n: usize, tie: usize) -> usize {
    (0..n)
        .min_by(|a, b| {
            let da = metrics
                .rect(*a, 0.0, scroll)
                .map(|r| (r.cx() - x).abs())
                .unwrap_or(f32::MAX);
            let db = metrics
                .rect(*b, 0.0, scroll)
                .map(|r| (r.cx() - x).abs())
                .unwrap_or(f32::MAX);
            da.total_cmp(&db)
                .then_with(|| a.abs_diff(tie).cmp(&b.abs_diff(tie)))
        })
        .unwrap_or(0)
}

impl<H: ContentLike> Machine<H> for DetailScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.sync_keys();
                Handled::Yes
            }
            ScreenEvent::RestoreMemory(PageMemory::Detail(memory)) => {
                self.restore_memory(memory);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), cx, fx);
                Handled::Yes
            }
            ScreenEvent::Enter(_) => {
                if self.detail().is_none() && crate::metadata::detail_request_status(self.sid, &self.rk) != Some(true) {
                    apply_metadata(MetadataCmd::RequestDetail {
                        sid: self.sid,
                        rk: self.rk.clone(),
                    });
                }
                self.reveal_focus(cx.focus.current);
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::StoreChanged(ord, _) => {
                if *ord == StoreId::Metadata.ord() {
                    self.sync_keys();
                    self.season_metrics.invalidate();
                    self.about_rows.invalidate();
                    if let Some(detail) = self.detail() {
                        self.season_metrics.update(detail, cx.measure);
                        self.about_rows.update(detail);
                    }
                }
                self.pump_restore();
                self.reveal_focus(cx.focus.current);
                fx.invalidate(Provenance::Landing(fx.from()));
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer) {
                    self.return_pending = false;
                    self.restore_intent = None;
                }
                self.reveal_focus(Some(*to));
                if let Some(located) = self.locate(to.elem) {
                    if let Located::Season(i) = located {
                        if matches!(by, By::Dir | By::Pointer) {
                            self.pending_season = Some(i);
                            self.season_settle = 0.0;
                        }
                    }
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
            ScreenEvent::PressHold(_) => {
                let supported = cx
                    .focus
                    .current
                    .filter(|k| k.entry == self.entry)
                    .and_then(|k| self.locate(k.elem))
                    .is_some_and(|located| {
                        matches!(
                            located,
                            Located::Season(_)
                                | Located::Episode(_, episodes::Row::Still)
                                | Located::Related(_)
                        )
                    });
                if supported {
                    self.content(fx, ContentReq::ItemMenu);
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            ScreenEvent::Input(input) => {
                if matches!(input.kind, InputKind::Key { key: Key::Up | Key::Down | Key::Left | Key::Right, edge: Edge::Down, .. }) {
                    self.restore_intent = None;
                    self.return_pending = false;
                }
                if let Some(handled) = self.panel_input(input, fx) {
                    return handled;
                }
                if matches!(
                    input.kind,
                    InputKind::Key {
                        key: Key::Ok,
                        edge: Edge::Down,
                        ..
                    }
                ) {
                    if let Some(elem) = cx
                        .focus
                        .current
                        .filter(|key| key.entry == self.entry)
                        .and_then(|key| {
                            matches!(
                                self.locate(key.elem),
                                Some(Located::Episode(_, episodes::Row::Text))
                            )
                            .then_some(key.elem)
                        })
                    {
                        // The text block is a link, not a holdable still. Spend OK on its DOWN edge
                        // before the engine can arm the episode group's Card press.
                        self.activate(elem, cx, fx);
                        return Handled::Yes;
                    }
                }
                if matches!(
                    input.kind,
                    InputKind::Key {
                        key: Key::Back,
                        edge: Edge::Down,
                        ..
                    }
                ) {
                    self.content(fx, ContentReq::Back);
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::App(AppMsg::DetailRestore { spot, episode }) => {
                self.restore_episode(spot, episode.as_deref());
                fx.invalidate(Provenance::Landing(fx.from()));
                Handled::Yes
            }
            ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount => {
                self.pending_season = None;
                self.season_settle = 0.0;
                self.restore_intent = None;
                self.return_pending = false;
                crate::ui::alt_sources::reset(ServerId::UNSET, "");
                crate::ui::about_panel::hide();
                crate::ui::tracks_panel::hide();
                if self.detail().is_some() {
                    apply_metadata(MetadataCmd::Clear);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl DetailScreen {
    fn restore_target_matches(&self, located: Located) -> bool {
        self.restore_focus().and_then(|elem| self.locate(elem)) == Some(located)
    }
}

impl<H: ContentLike> Screen<H> for DetailScreen {
    fn name(&self) -> &'static str {
        "detail"
    }

    fn state(&self) -> &dyn LogicalState {
        self
    }

    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }

    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut DrawFrame<'_, H>) {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter;
        let d = self.detail();
        self.draw_backdrop(p, d);
        let hero_vis = hero_alpha(self.scroll.pos, HERO_FADE);
        if hero_vis > 0.01 {
            self.draw_hero(p.translate(0.0, -self.scroll.pos).alpha(hero_vis), f, d);
        }
        if let Some(d) = d {
            self.draw_compact_title(p, d, hero_vis);
            let focus = f
                .focus
                .current
                .filter(|k| k.entry == self.entry)
                .and_then(|k| self.locate(k.elem));
            let (sections, n) = self.sections(Some(d));
            for &section in &sections[1..n] {
                let top = self.section_top(section, d) - self.scroll.pos;
                if top > crate::ui::consts::SCR_H || top + self.block_h(section, d) < 0.0 {
                    continue;
                }
                match section {
                    1 => season::draw(
                        p.translate(0.0, top),
                        &self.season_metrics,
                        self.tabs,
                        d.cur_season,
                        match focus {
                            Some(Located::Season(i)) => Some(i),
                            _ => None,
                        },
                        self.tab_scroll.pos,
                        self.season_pop.scale(0),
                    ),
                    2 => {
                        episodes::draw(
                            p,
                            d,
                            top,
                            self.episode_scroll.pos,
                            match focus {
                                Some(Located::Episode(i, row)) => Some((i, row)),
                                _ => None,
                            },
                            |i| self.episode_scale.get(i).map(|s| s.pos).unwrap_or(1.0),
                        );
                        if crate::metadata::season_loading() {
                            crate::ui::widgets::Spinner::new(
                                crate::ui::consts::SCR_W * 0.5,
                                top + episodes::H * 0.5,
                                26.0,
                            )
                            .phase(self.spin_ms as u32)
                            .tint(theme::TEXT_PRIMARY)
                            .draw(&Env::inert(), p);
                        }
                    }
                    3 => related::draw(
                        p,
                        d,
                        &self.related,
                        top,
                        match focus {
                            Some(Located::Related(i)) => Some(i),
                            _ => None,
                        },
                    ),
                    4 => cast::draw(
                        p,
                        d,
                        &self.cast,
                        top,
                        match focus {
                            Some(Located::Cast(i)) => Some(i),
                            _ => None,
                        },
                    ),
                    5 => self
                        .about_rows
                        .draw(p, d, top, f.focus.current.map(|k| k.elem)),
                    _ => {}
                }
            }
        } else if crate::metadata::detail_loading() {
            crate::ui::widgets::Spinner::new(
                crate::ui::consts::SCR_W * 0.5,
                (self.content_top() + crate::ui::consts::SCR_H) * 0.5 - self.scroll.pos,
                26.0,
            )
            .phase(self.spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(&Env::inert(), p);
        }

        self.record_stops(f);
        crate::ui::alt_sources::draw();
        crate::ui::about_panel::draw_scrim();
        crate::ui::about_panel::draw();
        crate::ui::tracks_panel::draw();
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

    fn memory_at(&self, focus: Option<FocusKey<u32>>) -> PageMemory {
        PageMemory::Detail(DetailMemory {
            spot: self.restore_intent.as_ref().filter(|_| self.return_pending).map(|intent| intent.spot.clone()).unwrap_or_else(|| self.spot(focus)),
            keys: self.keys.clone(), next_elem: self.next_elem,
        })
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl DetailScreen {
    fn art_identity(&self, d: Option<&Detail>) -> (ServerId, String, String) {
        if let Some(d) = d {
            let path = if d.is_show {
                hero::hero_episode(d)
                    .map(|ep| ep.thumb.clone())
                    .filter(|path| !path.is_empty())
                    .unwrap_or_else(|| d.art.clone())
            } else if d.kind == "episode" {
                if d.thumb.is_empty() {
                    d.art.clone()
                } else {
                    d.thumb.clone()
                }
            } else {
                d.art.clone()
            };
            return (d.sid, d.rk.clone(), path);
        }
        self.selected()
            .map(|m| (m.sid, m.rk.clone(), m.art.clone()))
            .unwrap_or((self.sid, self.rk.clone(), String::new()))
    }

    fn draw_backdrop(&self, p: Painter, d: Option<&Detail>) {
        let sf = (self.scroll.pos / (self.content_top() - crate::ui::detail_layout::TOP_MARGIN))
            .clamp(0.0, 1.0);
        let art_alpha = 1.0 - sf;
        let (sid, _, path) = self.art_identity(d);
        let (texture, width, height) = if art_alpha > 0.01 {
            crate::ui::widgets::resolve_tex_wh_on(sid, &path, 1920, 1080, 0)
        } else {
            (0, 0.0, 0.0)
        };
        if (texture == 0 || art_alpha < 0.99)
            && !self
                .ground
                .is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS)
        {
            self.ground.draw_with(
                p,
                Rect::FULL,
                crate::gfx::page_wash_dither(self.scroll.vel.abs() < 15.0),
            );
        }
        if texture != 0 {
            p.tex(
                texture,
                Rect::FULL.cover(width, height),
                0.0,
                theme::with_a(theme::dim(theme::TINT_WHITE, 1.0 - sf * 0.55), art_alpha),
            );
        }
        let visible = hero_alpha(self.scroll.pos, HERO_FADE);
        if visible > 0.01 {
            p.rect(
                Rect::new(
                    0.0,
                    crate::ui::widgets::HERO_BASE_SCRIM_Y0,
                    crate::ui::consts::SCR_W,
                    crate::ui::consts::SCR_H - crate::ui::widgets::HERO_BASE_SCRIM_Y0,
                ),
                0.0,
                theme::scrim(0.0),
                theme::scrim(crate::ui::detail_layout::base_scrim_a(
                    crate::ui::consts::SCR_H,
                    visible,
                )),
                0.0,
            );
            crate::ui::widgets::hero_scrim(p, visible, d.is_some_and(hero::has_people));
        }
    }

    fn draw_hero<H: ContentLike>(&self, p: Painter, cx: &Cx<'_, H>, d: Option<&Detail>) {
        use crate::ui::detail_layout::{HERO_TEXT_W, TITLE_BOTTOM};

        let (_, rk, _) = self.art_identity(d);
        let title = d
            .map(|d| d.title.as_str())
            .or_else(|| self.selected().map(|m| m.title.as_str()))
            .unwrap_or("Loading…");
        let band = crate::ui::hero_logo::band_h(LogoRung::Hero);
        HeroLogo::new(self.sid, &rk, title, LogoRung::Hero).draw(
            p,
            Rect::new(
                crate::ui::consts::MARGIN_X,
                TITLE_BOTTOM - band,
                HERO_TEXT_W,
                band,
            ),
        );

        let (lead, synopsis) = hero_blurb(d, self.selected());
        let synopsis_view = crate::ui::hero_synopsis(&synopsis, &lead);
        let chain = self.hero_chain();
        if let Some(d) = d {
            self.draw_identity_line(p, d, chain.meta_y);
            self.draw_ratings(p, d, chain.ratings_y);
        }
        if !synopsis.is_empty() {
            synopsis_view.draw(
                p,
                Rect::new(crate::ui::consts::MARGIN_X, chain.syn_y, HERO_TEXT_W, 0.0),
            );
        }
        if let Some(d) = d {
            hero::draw_facts(p, d, chain.facts_y);
            hero::draw_people(p, d, chain.btn_y);
        }
        self.draw_buttons(p, cx, chain.btn_y);
    }

    fn draw_identity_line(&self, p: Painter, d: &Detail, y: f32) {
        let ordinal = (d.kind == "episode" && d.season > 0 && d.index > 0)
            .then(|| crate::ui::fmt::episode_ordinal(d.season, d.index))
            .unwrap_or_default();
        let mut parts: Vec<&str> = Vec::new();
        if d.kind == "episode" {
            if !d.show_title.is_empty() {
                parts.push(&d.show_title);
            }
            if !ordinal.is_empty() {
                parts.push(&ordinal);
            }
        } else {
            parts.push(if d.is_show { "TV Show" } else { "Movie" });
            parts.extend(d.genres.iter().take(2).map(String::as_str));
        }
        if !d.rating.is_empty() {
            parts.push(&d.rating);
        }
        let mut x = crate::ui::consts::MARGIN_X
            + crate::ui::widgets::dotted_run(
                p,
                &parts,
                crate::ui::consts::MARGIN_X,
                y,
                theme::size::BODY,
                theme::TEXT_SECONDARY,
                theme::space::SM,
            );
        if x > crate::ui::consts::MARGIN_X {
            x += theme::space::SM;
        }
        let (top, base) = crate::text::text_cap_band(theme::size::BODY, 0);
        let cy = y + (top + base) * 0.5;
        if let Some(res) = crate::ui::fmt::resolution(&d.video_resolution, d.width, d.height) {
            x += crate::ui::widgets::badge(
                p,
                x,
                cy,
                &res,
                None,
                crate::ui::widgets::BadgeStyle::Filled,
            ) + theme::space::XS;
        }
        for (present, label) in [
            (!d.subs.is_empty(), "CC"),
            (d.subs.iter().any(|s| s.sdh), "SDH"),
            (d.audio.iter().any(|s| s.ad), "AD"),
        ] {
            if present {
                x += crate::ui::widgets::keyline_chip(p, x, cy, label, theme::TEXT_SECONDARY)
                    + theme::space::XS;
            }
        }
    }

    fn draw_ratings(&self, p: Painter, d: &Detail, y: f32) {
        let (top, base) = crate::text::text_cap_band(theme::size::LABEL, 1);
        let cy = y + (top + base) * 0.5;
        let mut x = crate::ui::consts::MARGIN_X;
        let mut i = 0;
        while i < d.ratings.len() {
            let provider = d.ratings[i].art.provider();
            let end = i + d.ratings[i..].partition_point(|r| r.art.provider() == provider);
            let scores: Vec<String> = d.ratings[i..end]
                .iter()
                .map(|r| crate::ui::fmt::rating_score(r.art, r.value))
                .collect();
            let cells: Vec<crate::ui::widgets::RatingCell<'_>> = d.ratings[i..end]
                .iter()
                .zip(scores.iter())
                .map(|(r, score)| crate::ui::widgets::RatingCell {
                    mark: rating_mark(r.art),
                    value: score,
                    suffix: crate::ui::fmt::rating_suffix(r.art),
                })
                .collect();
            let width = crate::ui::widgets::rating_group_w(provider, &cells);
            if x + width > crate::ui::consts::SCR_W - crate::ui::consts::MARGIN_X {
                break;
            }
            x += crate::ui::widgets::rating_group(p, x, cy, provider, &cells) + 32.0;
            i = end;
        }
    }

    fn draw_buttons<H: ContentLike>(&self, p: Painter, cx: &Cx<'_, H>, y: f32) {
        let set = self.hero_set();
        let widths = hero::hero_widths(
            cx.measure,
            set,
            set.restart,
            [self.disc_unfurl[0].pos, self.disc_unfurl[1].pos],
            self.named_show(),
        );
        let current = cx.focus.current.map(|k| k.elem);
        let (controls, n) = hero::hero_ctls(set);
        let last = hero::hero_btn_rect_at(set, n.saturating_sub(1), y, widths);
        let row = [
            crate::ui::consts::MARGIN_X,
            y - self.scroll.pos,
            last.x + last.w - crate::ui::consts::MARGIN_X,
            hero::CD,
        ];
        let may_read =
            crate::ui::nav::page_alpha() >= 0.999 && hero_alpha(self.scroll.pos, HERO_FADE) > 0.99;
        let palette = crate::gfx::sample_control_ground(row, may_read)
            .map(ControlPalette::ambient)
            .unwrap_or_default();
        for (i, ctl) in controls[..n].iter().copied().enumerate() {
            let rect = hero::hero_btn_rect_at(set, i, y, widths);
            let focused = current == Some(ctl.elem());
            let scale = self.ctl_pop.scale(i);
            match ctl {
                hero::HeroCtl::Play => Button::new(
                    hero::hero_pill_label(set.restart).as_ptr(),
                    theme::size::BODY,
                    rect,
                )
                .icon(crate::ui::icons::Icon::Play)
                .focused(focused)
                .palette(palette)
                .scale(scale)
                .draw(&Env::inert(), p),
                hero::HeroCtl::Alt => {
                    Button::new(hero::ALT_LABEL.as_ptr(), theme::size::BODY, rect)
                        .trailing_icon(crate::ui::icons::Icon::ChevronDown)
                        .focused(focused)
                        .palette(palette)
                        .scale(scale)
                        .draw(&Env::inert(), p)
                }
                ctl => {
                    let icon = match ctl {
                        hero::HeroCtl::Restart => crate::ui::icons::Icon::Restart,
                        hero::HeroCtl::MarkWatched => crate::ui::icons::Icon::Check,
                        hero::HeroCtl::MarkUnwatched => crate::ui::icons::Icon::Minus,
                        _ => unreachable!(),
                    };
                    let mut button = CircleButton::new(c"".as_ptr())
                        .icon(icon)
                        .frame(rect)
                        .focused(focused)
                        .palette(palette)
                        .scale(scale);
                    if let Some((slot, label)) = hero::disc_verb(ctl, self.named_show()) {
                        button = button.label(label.as_ptr(), self.disc_unfurl[slot].pos);
                    }
                    button.draw(&Env::inert(), p);
                }
            }
        }
    }

    fn draw_compact_title(&self, p: Painter, d: &Detail, hero_visible: f32) {
        if hero_visible >= 0.99 {
            return;
        }
        let (sections, n) = self.sections(Some(d));
        let hide_at = compact_title_hide_pos(&sections, n, d.is_show)
            .map(|position| {
                (self.section_top(sections[position], d) - crate::ui::detail_layout::TOP_MARGIN)
                    .max(0.0)
            })
            .unwrap_or(f32::MAX);
        let deep_visible = ((hide_at - self.scroll.pos) / 300.0).clamp(0.0, 1.0);
        let alpha = (1.0 - hero_visible) * deep_visible;
        if alpha <= 0.01 {
            return;
        }
        let band = crate::ui::hero_logo::band_h(LogoRung::Compact);
        HeroLogo::new(d.sid, &d.rk, &d.title, LogoRung::Compact)
            .align(HAlign::Center)
            .draw(
                p.alpha(alpha),
                Rect::new(
                    crate::ui::consts::MARGIN_X,
                    crate::ui::detail_layout::COMPACT_TITLE_BOT - band,
                    crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                    band,
                ),
            );
    }

    fn record_stops<H: ContentLike>(&self, f: &mut DrawFrame<'_, H>) {
        let mut elems = Vec::new();
        let set = self.hero_set();
        let (controls, n) = hero::hero_ctls(set);
        elems.extend(controls[..n].iter().map(|c| (c.elem(), Activate::Press)));
        if let Some(d) = self.detail() {
            elems.extend(
                (0..d.seasons.len().min(64))
                    .filter_map(|i| season::elem(i).map(|e| (e, Activate::Press))),
            );
            for i in 0..d.episodes.len().min(episodes::MAX_ITEMS) {
                if let Some(e) = episodes::elem(i, episodes::Row::Still) {
                    elems.push((e, Activate::Press));
                }
                if let Some(e) = episodes::elem(i, episodes::Row::Text) {
                    elems.push((e, Activate::Immediate));
                }
            }
            elems.extend(
                (0..d.related.len().min(512))
                    .filter_map(|i| related::elem(i).map(|e| (e, Activate::Press))),
            );
            elems.extend(
                (0..d.credits_len().min(512))
                    .filter_map(|i| cast::elem(i).map(|e| (e, Activate::Press))),
            );
            elems.push((about::CARD_ELEM, Activate::Press));
            if crate::ui::tracks_panel::is_available() {
                elems.push((about::LANGUAGES_ELEM, Activate::Press));
            }
        }
        for (elem, activate) in elems {
            let Some(elem) = self.engine_key(elem) else { continue };
            let Some(placed) = self.place(&elem, f, At::Drawn) else {
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
                    activate,
                },
            );
        }
    }
}

fn compact_title_hide_pos(sections: &[i32], n: usize, is_show: bool) -> Option<usize> {
    let wanted = usize::from(!is_show);
    let mut seen = 0;
    let mut first = None;
    for (position, &section) in sections[..n].iter().enumerate() {
        if section >= 3 && position >= 1 {
            first.get_or_insert(position);
            if seen == wanted {
                return Some(position);
            }
            seen += 1;
        }
    }
    first
}

fn play_resume_ns(from_start: bool, resume_ms: i64, duration_ms: i64) -> i64 {
    if from_start {
        0
    } else {
        crate::metadata::resume_ns(resume_ms, duration_ms)
    }
}

fn hero_blurb<'a>(
    d: Option<&'a Detail>,
    row: Option<&'a crate::pms::PmsMovie>,
) -> (String, String) {
    if let Some(d) = d {
        if d.is_show {
            if let Some(ep) = hero::hero_episode(d) {
                let ordinal = crate::ui::fmt::episode_ordinal(ep.season, ep.index);
                let lead = if ep.title.is_empty() {
                    format!("{ordinal}: ")
                } else {
                    format!("{ordinal} \u{b7} {}: ", ep.title)
                };
                return (lead, ep.summary.clone());
            }
        }
        return (String::new(), d.summary.clone());
    }
    (
        String::new(),
        row.map(|m| m.summary.clone()).unwrap_or_default(),
    )
}

fn rating_mark(art: crate::metadata::RatingArt) -> &'static [crate::ui::widgets::MarkLayer] {
    use crate::metadata::RatingArt as A;
    use crate::ui::icons::Icon;
    use crate::ui::widgets::MarkLayer;
    static FRESH: &[MarkLayer] = &[
        (Icon::Tomato, theme::RATING_FRESH),
        (Icon::TomatoCalyx, theme::RATING_LEAF),
    ];
    static CERTIFIED: &[MarkLayer] = &[
        (Icon::Tomato, theme::RATING_CERTIFIED),
        (Icon::TomatoCalyx, theme::RATING_LEAF),
    ];
    static ROTTEN: &[MarkLayer] = &[
        (Icon::TomatoHollow, theme::RATING_MUTED),
        (Icon::TomatoCalyx, theme::RATING_MUTED),
    ];
    static CROWD_UP: &[MarkLayer] = &[(Icon::Crowd, theme::RATING_AUDIENCE)];
    static CROWD_DOWN: &[MarkLayer] = &[(Icon::Crowd, theme::RATING_MUTED)];
    match art {
        A::TomatoFresh => FRESH,
        A::TomatoCertified => CERTIFIED,
        A::TomatoRotten => ROTTEN,
        A::PopcornUpright => CROWD_UP,
        A::PopcornSpilled => CROWD_DOWN,
        A::Imdb | A::Tmdb => &[],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Located {
    Hero(hero::HeroCtl),
    Season(usize),
    Episode(usize, episodes::Row),
    Related(usize),
    Cast(usize),
    About(usize),
}

impl Located {
    fn local_key(self) -> Option<u32> {
        match self {
            Self::Hero(control) => Some(control.elem()),
            Self::Season(index) => season::elem(index),
            Self::Episode(index, row) => episodes::elem(index, row),
            Self::Related(index) => related::elem(index),
            Self::Cast(index) => cast::elem(index),
            Self::About(0) => Some(about::CARD_ELEM),
            Self::About(_) => Some(about::LANGUAGES_ELEM),
        }
    }
    fn section(self) -> i32 {
        match self {
            Self::Hero(_) => 0,
            Self::Season(_) => 1,
            Self::Episode(_, _) => 2,
            Self::Related(_) => 3,
            Self::Cast(_) => 4,
            Self::About(_) => 5,
        }
    }

    fn group(self) -> GroupId {
        match self {
            Self::Hero(_) => hero::HERO_GROUP,
            Self::Season(_) => season::SEASON_GROUP,
            Self::Episode(_, _) => episodes::EPISODES_GROUP,
            Self::Related(_) => related::RELATED_GROUP,
            Self::Cast(_) => cast::CAST_GROUP,
            Self::About(_) => about::ABOUT_GROUP,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Hero(c) => c.elem() as usize,
            Self::Season(i) | Self::Episode(i, _) | Self::Related(i) | Self::Cast(i) => i,
            Self::About(0) => 0,
            // Legacy Spot/focusprobe vocabulary keeps the four visual About columns numbered
            // Card=0, Information=1, Languages=2, Accessibility=3 even though only two are stops.
            Self::About(_) => 2,
        }
    }
}

impl LogicalState for DetailScreen {
    fn write(&self, w: &mut Canon) {
        debug_assert!(!SHAPE.is_empty());
        w.bool(self.return_pending).u32(self.next_elem).seq(self.keys.len());
        for key in &self.keys { key.identity.write(w); w.u32(key.elem); }
        w.u32(u32::from(self.sid.raw())).str(&self.rk);
        match self.pending_season {
            Some(index) => {
                w.bool(true).u32(index as u32);
            }
            None => {
                w.bool(false).u32(0);
            }
        }
        w.f32(self.season_settle);
        match &self.restore_intent {
            Some(intent) => {
                w.bool(true)
                    .u32(intent.spot.section as u32)
                    .u32(intent.spot.col as u32)
                    .bool(intent.spot.ep_text);
                for col in intent.spot.saved_col {
                    w.u32(col as u32);
                }
                match intent.spot.season {
                    Some(number) => {
                        w.bool(true).u64(number as u64);
                    }
                    None => {
                        w.bool(false).u64(0);
                    }
                }
                match &intent.episode {
                    Some(rk) => {
                        w.bool(true).str(rk);
                    }
                    None => {
                        w.bool(false).str("");
                    }
                }
                w.bool(intent.season_requested);
            }
            None => {
                w.bool(false);
            }
        }
        w.u8(if crate::ui::alt_sources::is_open() {
            1
        } else if crate::ui::about_panel::is_open() {
            2
        } else if crate::ui::tracks_panel::is_open() {
            3
        } else {
            0
        });
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "detail sid={} pending_season={} settle_us={} restore={} restore_season_sent={} panel={}",
            self.sid.raw(),
            self.pending_season
                .map(|index| index.to_string())
                .unwrap_or_else(|| "-".into()),
            (self.season_settle * 1_000_000.0).round() as u64,
            self.restore_intent.is_some(),
            self.restore_intent
                .as_ref()
                .is_some_and(|intent| intent.season_requested),
            if crate::ui::alt_sources::is_open() {
                "alt"
            } else if crate::ui::about_panel::is_open() {
                "about"
            } else if crate::ui::tracks_panel::is_open() {
                "tracks"
            } else {
                "none"
            }
        ));
    }
}

impl DetailScreen {
    fn reveal_focus(&mut self, focus: Option<FocusKey<u32>>) {
        if self.return_waiting() { return; }
        let Some(located) = focus.filter(|key| key.entry == self.entry).and_then(|key| self.locate(key.elem)) else { return };
        let Some(detail) = self.detail() else { return };
        self.scroll_target = if located.section() == 0 { 0.0 } else {
            (self.section_top(located.section(), detail) - crate::ui::detail_layout::TOP_MARGIN).max(0.0)
        };
    }

    fn return_waiting(&self) -> bool {
        self.return_pending && self.restore_intent.as_ref().is_some_and(|intent| {
            self.detail().is_none() || season::restore_step(self.detail(), intent.spot.season,
                intent.season_requested, crate::metadata::season_loading()) != season::RestoreStep::Ready
        })
    }

    fn pump_restore(&mut self) {
        if self.detail().is_none() && crate::metadata::detail_request_status(self.sid, &self.rk) == Some(false) {
            self.restore_intent = None;
            self.return_pending = false;
            return;
        }
        let Some(d) = self.detail() else { return };
        let step = self
            .restore_intent
            .as_ref()
            .map(|intent| {
                season::restore_step(
                    Some(d),
                    intent.spot.season,
                    intent.season_requested,
                    crate::metadata::season_loading(),
                )
            })
            .unwrap_or(season::RestoreStep::Ready);
        match step {
            season::RestoreStep::Request(index) => {
                if let Some(intent) = self.restore_intent.as_mut() {
                    intent.season_requested = true;
                }
                apply_metadata(MetadataCmd::LoadSeason(index));
            }
            season::RestoreStep::Retire => {
                self.restore_intent = None;
                self.return_pending = false;
            }
            season::RestoreStep::Wait | season::RestoreStep::Ready => {}
        }
    }

    fn tick<H: ContentLike>(&mut self, dt: f32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.pump_restore();
        self.spin_ms += dt * 1000.0;
        let d = self.detail();
        let loaded = d.is_some();
        if let Some(d) = d {
            self.season_metrics.update(d, cx.measure);
            self.about_rows.update(d);
        }

        let focused = cx
            .focus
            .current
            .filter(|k| k.entry == self.entry)
            .and_then(|k| self.locate(k.elem));
        let hero_set = self.hero_set();
        let hero_index = match focused {
            Some(Located::Hero(c)) => hero::index_of(hero_set, c),
            _ => None,
        };
        self.ctl_pop.step(hero_index, dt);
        self.season_pop
            .step(matches!(focused, Some(Located::Season(_))).then_some(0), dt);
        let disc = match focused {
            Some(Located::Hero(c)) => hero::disc_verb(c, self.named_show()).map(|(i, _)| i),
            _ => None,
        };
        for (i, spring) in self.disc_unfurl.iter_mut().enumerate() {
            spring.step(
                f32::from(disc == Some(i)),
                crate::ui::widgets::K_DISC_UNFURL,
                dt,
            );
        }

        let episode_focus = match focused {
            Some(Located::Episode(i, episodes::Row::Still)) => Some(i),
            _ => None,
        };
        for (i, spring) in self.episode_scale.iter_mut().enumerate() {
            spring.step(
                if episode_focus == Some(i) {
                    crate::ui::widgets::CARD_FOCUS_SCALE
                } else {
                    1.0
                },
                300.0,
                dt,
            );
        }

        if let Some(d) = d {
            let related_focus = match focused {
                Some(Located::Related(i)) => Some(i),
                _ => None,
            };
            self.related
                .update(d.related.len(), related_focus, &RowStyle::HOME, dt);
            let cast_focus = match focused {
                Some(Located::Cast(i)) => Some(i),
                _ => None,
            };
            self.cast
                .update(d.credits_len(), cast_focus, &RowStyle::CAST, dt);

            if let Some(i) = match focused {
                Some(Located::Episode(i, _)) => Some(i),
                _ => None,
            } {
                let target = card_row::scroll_into_view(
                    self.episode_scroll.pos,
                    i,
                    d.episodes.len(),
                    episodes::W,
                    episodes::GAP,
                    crate::ui::consts::SCR_W - 2.0 * crate::ui::consts::MARGIN_X,
                );
                self.episode_scroll.step(target, K_STRIP_SCROLL, dt);
            }
            let tab_focus = match focused {
                Some(Located::Season(i)) => Some(i),
                _ => None,
            };
            if let Some(i) = tab_focus {
                let target = self.season_metrics.scroll_target(self.tab_scroll.pos, i);
                self.tab_scroll.step(target, K_STRIP_SCROLL, dt);
            }
            season::update_tabs(
                &mut self.tabs,
                &self.season_metrics,
                Some(d.cur_season),
                tab_focus,
                dt,
            );
            let target = if d.has_blur {
                AmbientWash::keyed(d.blur, [AmbientWash::GROUND_W; 4])
            } else {
                [theme::SURFACE_APP; 4]
            };
            self.ground.step(target, AmbientWash::K, dt);
        }

        self.scroll.step(self.scroll_target, K_SCROLL, dt);
        crate::ui::alt_sources::pump();
        crate::ui::alt_sources::update(dt);
        crate::ui::about_panel::update(dt);
        crate::ui::tracks_panel::update(dt);

        if self.pending_season.is_some() {
            self.season_settle += dt;
            if self.season_settle >= season::SETTLE_S {
                let index = self.pending_season.take().unwrap_or(0);
                self.season_settle = 0.0;
                if self
                    .detail()
                    .is_some_and(|d| d.cur_season != index && index < d.seasons.len())
                {
                    apply_metadata(MetadataCmd::LoadSeason(index));
                }
            }
        }

        let moving = self.scroll.vel.abs() > 0.01
            || self.episode_scroll.vel.abs() > 0.01
            || self.tab_scroll.vel.abs() > 0.01
            || self.disc_unfurl.iter().any(|s| s.vel.abs() > 0.01);
        if !crate::metadata::detail_loading()
            && !crate::metadata::season_loading()
            && focused.is_some_and(|located| self.return_pending && !self.return_waiting() || self.restore_target_matches(located))
        {
            self.restore_intent = None;
            self.return_pending = false;
        }
        if moving || !loaded || crate::metadata::season_loading() {
            fx.note(PresentEvent::Motion);
        }
    }

    fn hero_set(&self) -> hero::HeroSet {
        let (restart, mark) = self
            .detail()
            .map(|d| {
                (
                    hero::has_restart(hero::hero_resume_ns(d)),
                    hero::hero_mark(d),
                )
            })
            .unwrap_or((false, PosterMark::None));
        hero::HeroSet {
            restart,
            alt: crate::ui::alt_sources::is_available(),
            mark,
        }
    }

    fn named_show(&self) -> bool {
        self.detail().is_some_and(hero::watch_names_show)
    }

    fn activate<H: ContentLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let local = self.locate(elem).and_then(Located::local_key).unwrap_or(elem);
        match self.locate(elem) {
            Some(Located::Hero(ctl)) => self.activate_hero(ctl, cx, fx),
            Some(Located::Season(i)) => {
                self.pending_season = Some(i);
                self.season_settle = season::SETTLE_S;
            }
            Some(Located::Episode(_, _)) => {
                let action = self
                    .detail()
                    .map(|d| episodes::action(d, local, crate::metadata::season_loading()))
                    .unwrap_or(episodes::Action::None);
                match action {
                    episodes::Action::Play(i) => {
                        self.play_episode_at(i, false, fx);
                    }
                    episodes::Action::OpenDetail(sid, rk) => {
                        self.content(fx, ContentReq::Push(ContentArg::Detail { sid, rk }))
                    }
                    episodes::Action::None => {}
                }
            }
            Some(Located::Related(_)) => {
                let action = self
                    .detail()
                    .map(|d| related::action(d, local))
                    .unwrap_or(related::Action::None);
                if let related::Action::OpenDetail(sid, rk) = action {
                    self.content(fx, ContentReq::Push(ContentArg::Detail { sid, rk }));
                }
            }
            Some(Located::Cast(_)) => {
                let action = self
                    .detail()
                    .map(|d| cast::action(d, local))
                    .unwrap_or(cast::Action::None);
                if let cast::Action::OpenPerson {
                    sid,
                    key,
                    guid,
                    name,
                    thumb,
                } = action
                {
                    self.content(
                        fx,
                        ContentReq::Push(ContentArg::Person {
                            sid,
                            key,
                            guid,
                            name,
                            thumb,
                        }),
                    );
                }
            }
            Some(Located::About(0)) => crate::ui::about_panel::open(),
            Some(Located::About(1)) => crate::ui::tracks_panel::open(),
            _ => {}
        }
        fx.invalidate(Provenance::Input);
    }

    fn activate_hero<H: ContentLike>(
        &mut self,
        ctl: hero::HeroCtl,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) {
        match ctl {
            hero::HeroCtl::Play => {
                self.play_hero(false, fx);
            }
            hero::HeroCtl::Restart => {
                self.play_hero(true, fx);
            }
            hero::HeroCtl::Alt => {
                let set = self.hero_set();
                if let Some(i) = hero::index_of(set, ctl) {
                    let widths = hero::hero_widths(
                        cx.measure,
                        set,
                        set.restart,
                        [self.disc_unfurl[0].pos, self.disc_unfurl[1].pos],
                        self.named_show(),
                    );
                    let mut rect = hero::hero_btn_rect_at(set, i, self.hero_chain().btn_y, widths);
                    rect.y -= self.scroll.pos;
                    crate::ui::alt_sources::open_for(self.sid, &self.rk, rect);
                }
            }
            hero::HeroCtl::MarkWatched | hero::HeroCtl::MarkUnwatched => {
                if let Some(d) = self.detail() {
                    apply_viewstate(ViewStateCmd::Request {
                        sid: d.sid,
                        rk: d.rk.clone(),
                        write: if ctl == hero::HeroCtl::MarkWatched {
                            crate::viewstate::Write::Watched
                        } else {
                            crate::viewstate::Write::Unwatched
                        },
                        detail: Some(String::new()),
                        guid: d.guid.clone(),
                    });
                }
            }
        }
    }

    fn play_hero<H: ContentLike>(&mut self, from_start: bool, fx: &mut Effects<'_, H>) -> bool {
        let Some(d) = self.detail() else { return false };
        if d.is_show {
            let i = hero::hero_episode(d).and_then(|ep| {
                d.episodes
                    .iter()
                    .position(|candidate| candidate.rk == ep.rk)
            });
            match i {
                Some(i) => self.play_episode_at(i, from_start, fx),
                None => self.play_episode_value(
                    hero::hero_episode(d).or_else(|| d.episodes.first()),
                    d,
                    from_start,
                    fx,
                ),
            }
        } else {
            let started = self.selected().map_or_else(
                || {
                    crate::route::request_play(
                        crate::route::item_sid(d.sid),
                        &d.rk,
                        &d.part,
                        &d.vcodec,
                        &d.acodec,
                        &d.title,
                        "",
                    )
                },
                crate::route::request_play_movie,
            );
            if started {
                let resume_ns = play_resume_ns(from_start, d.resume_ms, d.dur_ms);
                self.content(fx, ContentReq::Play { resume_ns });
            }
            started
        }
    }

    fn play_episode_at<H: ContentLike>(
        &mut self,
        index: usize,
        from_start: bool,
        fx: &mut Effects<'_, H>,
    ) -> bool {
        if crate::metadata::season_loading() {
            return false;
        }
        let Some(d) = self.detail() else { return false };
        self.play_episode_value(d.episodes.get(index), d, from_start, fx)
    }

    fn play_episode_value<H: ContentLike>(
        &mut self,
        episode: Option<&crate::metadata::Episode>,
        d: &Detail,
        from_start: bool,
        fx: &mut Effects<'_, H>,
    ) -> bool {
        let Some(ep) = episode else { return false };
        // Clone the complete command/request payload before the store write. `Detail` and
        // `Episode` live in the metadata store's replaceable slot; no borrow of either may cross a
        // mutation, even when this particular command currently replaces only NowPlaying.
        let sid = d.sid;
        let play_rk = ep.rk.clone();
        let part = ep.part.clone();
        let vcodec = ep.vcodec.clone();
        let acodec = ep.acodec.clone();
        let title = if ep.title.is_empty() {
            d.title.clone()
        } else {
            ep.title.clone()
        };
        let context = format!("{}  \u{b7}  S{} E{}", d.title, ep.season, ep.index);
        let resume_ns = play_resume_ns(from_start, ep.resume_ms, ep.dur_ms);
        let now_playing = crate::metadata::NowPlaying {
            is_episode: true,
            title: d.title.clone(),
            ep_title: ep.title.clone(),
            season: ep.season,
            index: ep.index,
            summary: ep.summary.clone(),
            year: ep
                .aired
                .get(0..4)
                .and_then(|year| year.parse::<i64>().ok())
                .unwrap_or(0),
            dur_ms: ep.dur_ms,
            rating: ep.rating.clone(),
            thumb: ep.thumb.clone(),
            detail_rk: d.rk.clone(),
        };
        apply_metadata(MetadataCmd::SetNowPlaying(Some(now_playing)));
        let started = crate::route::request_play(
            crate::route::item_sid(sid),
            &play_rk,
            &part,
            &vcodec,
            &acodec,
            &title,
            &context,
        );
        if started {
            self.content(fx, ContentReq::Play { resume_ns });
        }
        started
    }

    fn content<H: ContentLike>(&self, fx: &mut Effects<'_, H>, req: ContentReq) {
        fx.push(Fx::App(AppFx::Content(req)));
    }

    fn panel_input<H: ContentLike>(
        &mut self,
        event: &InputEvent<u32>,
        fx: &mut Effects<'_, H>,
    ) -> Option<Handled> {
        if crate::ui::alt_sources::is_open() {
            match event.kind {
                InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } => crate::ui::alt_sources::close(),
                InputKind::Key {
                    key: Key::Ok,
                    edge: Edge::Down,
                    ..
                } => {
                    if let crate::ui::alt_sources::Action::Open { sid, rk } =
                        crate::ui::alt_sources::on_ok()
                    {
                        self.content(fx, ContentReq::Present(ContentArg::Detail { sid, rk }));
                    }
                }
                InputKind::Key {
                    sym,
                    edge: Edge::Down | Edge::Repeat,
                    ..
                } => crate::ui::alt_sources::move_focus(sym as i32),
                InputKind::Pointer { x, y, .. } => crate::ui::alt_sources::pointer_focus(x, y),
                InputKind::Click { x, y, .. } => {
                    if let crate::ui::alt_sources::Action::Open { sid, rk } =
                        crate::ui::alt_sources::click(x, y)
                    {
                        self.content(fx, ContentReq::Present(ContentArg::Detail { sid, rk }));
                    }
                }
                _ => {}
            }
            fx.invalidate(Provenance::Input);
            return Some(Handled::Yes);
        }
        if crate::ui::about_panel::is_open() {
            match event.kind {
                InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } => crate::ui::about_panel::close(),
                InputKind::Key {
                    key: Key::Ok,
                    edge: Edge::Down,
                    ..
                } => crate::ui::about_panel::on_ok(),
                InputKind::Click { x, y, .. } => crate::ui::about_panel::click(x, y),
                _ => {}
            }
            fx.invalidate(Provenance::Input);
            return Some(Handled::Yes);
        }
        if crate::ui::tracks_panel::is_open() {
            match event.kind {
                InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } => crate::ui::tracks_panel::close(),
                InputKind::Key {
                    sym,
                    edge: Edge::Down | Edge::Repeat,
                    ..
                } => crate::ui::tracks_panel::move_focus(sym as i32),
                _ => {}
            }
            fx.invalidate(Provenance::Input);
            return Some(Handled::Yes);
        }
        None
    }
}
