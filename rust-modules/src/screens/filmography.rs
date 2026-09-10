//! Filmography is an independently mounted, opaque modal screen. It keeps the legacy route's
//! two-column composition, department strip, table, preview dwell and cross-fade, but owns no
//! cursor: every focus-dependent query receives the engine's `FocusKey`.
//!
//! The person store remains a legacy singleton during phase 7. This screen snapshots the matching
//! person's name and filmography into its instance and refreshes that snapshot only from matching
//! store notices. A covered instance therefore cannot start rendering a different person's data,
//! and restoring it does not reset its department, preview, table motion, or engine focus.

use std::borrow::Cow;
use std::ffi::CString;
use std::os::raw::c_int;

use crate::person::{Credit, Department};
use crate::plex::ServerId;
use crate::ui::card_row;
use crate::ui::consts::*;
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Effects, EntryId, GroupId, Handled, InputEvent, InputKind, Key, LogicalState,
    Machine, Measure,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind,
    GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use crate::ui::table::{Row as TRow, Section, TableView};
use crate::ui::widgets::{self, SelMark, TabGround, TabPill, TabStrip};
use crate::ui::xfade::Xfade;
use crate::ui::{theme, Env, Rect, Spring, View};

use super::registry::{
    AppFx, ContentArg, ContentLike, ContentReq, FilmographyKey, FilmographyMemory, PageMemory,
};

const COPY_W: f32 = 480.0;
const PILL_H: f32 = 60.0;
const CLIP_VPAD: f32 = theme::space::LG;
const TAB_FADE_W: f32 = theme::space::XL;
const STRIP_BAND: f32 = 128.0;
const PV: Rect = Rect::new(MARGIN_X, 330.0 + theme::space::SM, 420.0, 630.0);
const PV_SETTLE: f32 = 0.180;
const STRIP_INSET: f32 = 20.0;
const TAB_PAD: f32 = 26.0;
const TAB_GAP: f32 = widgets::STRIP_GAP_WIDE;

const TAB_GROUP: GroupId = GroupId(0);
const LIST_GROUP: GroupId = GroupId(1);
const FIRST_ELEM: u32 = 0x1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Located {
    Tab(usize),
    Row(usize),
}

fn table_frame() -> Rect {
    let l = RouteLayout::screen_with_copy_w(COPY_W);
    let top = l.sectioned_table().y + STRIP_BAND - crate::ui::table::TOP_PAD;
    Rect::new(
        l.content.x,
        top,
        l.content.w,
        (l.content.y + l.content.h - top + crate::ui::table::BOT_PAD).max(0.0),
    )
}

fn opaque_ground_ready(alpha: f32) -> bool {
    alpha >= 0.995
}

impl LogicalState for FilmographyScreen {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sid.raw() as u32)
            .str(&self.key)
            .str(&self.department)
            .u32(self.next_elem)
            .u32(self.keys.len() as u32);
        for key in &self.keys {
            w.str(&key.department);
            match &key.catalog_id {
                Some(id) => {
                    w.bool(true).str(id);
                }
                None => {
                    w.bool(false);
                }
            }
            w.u32(key.elem);
        }
        match &self.preview {
            Some((department, catalog_id)) => {
                w.bool(true).str(department).str(catalog_id);
            }
            None => {
                w.bool(false);
            }
        }
        w.str(&self.name).u32(self.model.len() as u32);
        // This is an owned snapshot, not a cache reconstructible from the singleton store:
        // another Person may have displaced that store while this modal was covered.
        for department in &self.model {
            w.str(&department.title).u32(department.total as u32).u32(department.rows.len() as u32);
            for credit in &department.rows {
                w.str(&credit.catalog_id).str(&credit.title).str(&credit.thumb)
                    .str(&credit.role).u32(credit.year as u32);
                match &credit.local {
                    Some((sid, rk)) => { w.bool(true).u32(sid.raw() as u32).str(rk); }
                    None => { w.bool(false); }
                }
            }
        }
        match &self.pv_want {
            Some((department, id)) => { w.bool(true).str(department).str(id); }
            None => { w.bool(false); }
        }
        w.f32(self.pv_still);
        self.pv_fade.write(w);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "filmography sid={} key={} department={} keys={} next_elem={} preview={}",
            self.sid.raw(),
            self.key,
            self.department,
            self.keys.len(),
            self.next_elem,
            self.preview.is_some()
        ));
    }
}

pub(crate) struct FilmographyScreen {
    entry: EntryId,
    sid: ServerId,
    key: String,
    name: String,
    /// Stable department identity, not a raw tab index.
    department: String,
    /// Stable element interner for both department tabs (`catalog_id=None`) and credit rows.
    keys: Vec<FilmographyKey>,
    next_elem: u32,
    key_by_identity: std::collections::HashMap<(String, Option<String>), u32>,
    identity_by_elem: std::collections::HashMap<u32, (String, Option<String>)>,
    department_index: std::collections::HashMap<String, usize>,
    row_index: std::collections::HashMap<String, usize>,

    // Render/cache state. `table.sel` mirrors the engine only to animate/draw TableView; it is
    // never read as the logical cursor by navigation, activation, snapshots, or typed queries.
    tabs: TabStrip,
    tab_hscroll: Spring,
    pop: widgets::CtlPop<1>,
    table: TableView,
    model: Vec<Department>,
    dirty: bool,
    tab_c: Vec<CString>,
    preview: Option<(String, String)>,
    pv_want: Option<(String, String)>,
    pv_still: f32,
    pv_fade: Xfade,
    ground: RouteGround,
    /// Render-policy latch only; ModalStack remains the sole lifecycle/phase owner.
    ground_ready: bool,
}

impl FilmographyScreen {
    pub(crate) const SHAPE: &'static str = "FilmographyScreen{sid:ServerId,key:String,department:String,next_elem:u32,keys:[{department:String,catalog_id:Option<String>,elem:u32}],preview:Option<(String,String)>,name:String,model:[{title:String,total:usize,rows:[{catalog_id:String,title:String,thumb:String,role:String,year:i32,local:Option<(ServerId,String)>}]}],pv_want:Option<(String,String)>,pv_still:f32,pv_fade:{phase:Idle|Out|Hold|In,t:f32}}";

    pub(crate) fn new(entry: EntryId, sid: ServerId, key: String) -> Self {
        let mut out = Self {
            entry,
            sid,
            key,
            name: String::new(),
            department: String::new(),
            keys: Vec::new(),
            next_elem: FIRST_ELEM,
            key_by_identity: std::collections::HashMap::new(),
            identity_by_elem: std::collections::HashMap::new(),
            department_index: std::collections::HashMap::new(),
            row_index: std::collections::HashMap::new(),
            tabs: TabStrip::new(),
            tab_hscroll: Spring::at(0.0),
            pop: widgets::CtlPop::new(),
            table: TableView::new(),
            model: Vec::new(),
            dirty: true,
            tab_c: Vec::new(),
            preview: None,
            pv_want: None,
            pv_still: 0.0,
            pv_fade: Xfade::new(),
            ground: RouteGround::new(),
            ground_ready: false,
        };
        out.rebuild(None);
        out
    }

    fn person(&self) -> Option<&'static crate::person::Person> {
        crate::person::current().filter(|p| {
            crate::plex::same_item((p.sid, p.key.as_str()), (self.sid, self.key.as_str()))
        })
    }

    fn selected_tab(&self) -> usize {
        self.department_index
            .get(&self.department)
            .copied()
            .unwrap_or(0)
    }

    fn dept(&self) -> Option<&Department> {
        self.model.get(self.selected_tab())
    }

    fn rows(&self) -> &[Credit] {
        self.dept().map(|d| d.rows.as_slice()).unwrap_or(&[])
    }

    fn current_row(&self, focus: Option<crate::ui::machine::FocusKey<u32>>) -> Option<usize> {
        let k = focus.filter(|k| k.entry == self.entry)?;
        match self.locate(k.elem)? {
            Located::Row(i) if i < self.rows().len() => Some(i),
            _ => None,
        }
    }

    fn current_tab(&self, focus: Option<crate::ui::machine::FocusKey<u32>>) -> Option<usize> {
        let k = focus.filter(|k| k.entry == self.entry)?;
        match self.locate(k.elem)? {
            Located::Tab(i) if i < self.model.len() => Some(i),
            _ => None,
        }
    }

    fn focused_target(
        &self,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
    ) -> Option<(ServerId, &str)> {
        let i = self.current_row(focus)?;
        self.rows()
            .get(i)
            .and_then(|c| c.local.as_ref())
            .map(|(sid, rk)| (*sid, rk.as_str()))
    }

    fn credit_by_identity(&self, department: &str, catalog_id: &str) -> Option<&Credit> {
        self.model
            .iter()
            .find(|d| d.title == department)?
            .rows
            .iter()
            .find(|credit| credit.catalog_id == catalog_id)
    }

    fn locate(&self, elem: u32) -> Option<Located> {
        let (department, catalog_id) = self.identity_by_elem.get(&elem)?;
        match catalog_id {
            None => self
                .department_index
                .get(department)
                .copied()
                .map(Located::Tab),
            Some(catalog_id) if *department == self.department => {
                self.row_index.get(catalog_id).copied().map(Located::Row)
            }
            Some(_) => None,
        }
    }

    fn elem_for_tab(&self, i: usize) -> Option<u32> {
        let department = &self.model.get(i)?.title;
        self.key_by_identity
            .get(&(department.clone(), None))
            .copied()
    }

    fn elem_for_row(&self, i: usize) -> Option<u32> {
        let catalog_id = &self.rows().get(i)?.catalog_id;
        self.key_by_identity
            .get(&(self.department.clone(), Some(catalog_id.clone())))
            .copied()
    }

    fn focus_key(&self, located: Located) -> crate::ui::machine::FocusKey<u32> {
        let elem = match located {
            Located::Tab(i) => self.elem_for_tab(i),
            Located::Row(i) => self.elem_for_row(i),
        }
        .unwrap_or(FIRST_ELEM);
        crate::ui::machine::FocusKey {
            entry: self.entry,
            elem,
        }
    }

    fn sync_keys(&mut self) {
        let identities: Vec<(String, Option<String>)> = self
            .model
            .iter()
            .flat_map(|department| {
                std::iter::once((department.title.clone(), None)).chain(
                    department
                        .rows
                        .iter()
                        .map(|credit| (department.title.clone(), Some(credit.catalog_id.clone())))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut known: std::collections::HashSet<(String, Option<String>)> = self
            .keys
            .iter()
            .map(|key| (key.department.clone(), key.catalog_id.clone()))
            .collect();
        for (department, catalog_id) in identities {
            if !known.insert((department.clone(), catalog_id.clone())) {
                continue;
            }
            let elem = self.next_elem;
            self.next_elem = self
                .next_elem
                .checked_add(1)
                .expect("filmography element-key space exhausted");
            self.keys.push(FilmographyKey {
                department,
                catalog_id,
                elem,
            });
        }
        self.reindex();
    }

    fn reindex(&mut self) {
        self.key_by_identity = self
            .keys
            .iter()
            .map(|key| ((key.department.clone(), key.catalog_id.clone()), key.elem))
            .collect();
        self.identity_by_elem = self
            .keys
            .iter()
            .map(|key| (key.elem, (key.department.clone(), key.catalog_id.clone())))
            .collect();
        self.department_index = self
            .model
            .iter()
            .enumerate()
            .map(|(i, department)| (department.title.clone(), i))
            .collect();
        let selected = self.selected_tab();
        let row_index = self
            .model
            .get(selected)
            .map(|department| {
                department
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, credit)| (credit.catalog_id.clone(), i))
                    .collect()
            })
            .unwrap_or_default();
        self.row_index = row_index;
    }

    pub(crate) fn restore(&mut self, memory: &FilmographyMemory) {
        self.keys = memory.keys.clone();
        let after_last = self
            .keys
            .iter()
            .map(|key| key.elem)
            .max()
            .and_then(|elem| elem.checked_add(1))
            .unwrap_or(FIRST_ELEM);
        self.next_elem = memory.next_elem.max(FIRST_ELEM).max(after_last);
        self.department = memory.department.clone();
        self.preview = memory.preview.clone();
        self.pv_want = self.preview.clone();
        self.pv_still = 0.0;
        self.pv_fade = Xfade::new();
        self.rebuild(None);
    }

    fn memory(&self) -> FilmographyMemory {
        FilmographyMemory {
            keys: self.keys.clone(),
            next_elem: self.next_elem,
            department: self.department.clone(),
            preview: self.preview.clone(),
        }
    }

    fn pill_rects(&self, measure: &dyn Measure) -> Vec<Rect> {
        let l = RouteLayout::screen_with_copy_w(COPY_W);
        let mut x = l.content.x + STRIP_INSET;
        self.tab_c
            .iter()
            .map(|c| {
                let w = measure.width(c.as_c_str(), theme::size::BODY, true) + 2.0 * TAB_PAD;
                let rect = Rect::new(x - self.tab_hscroll.pos, l.content.y, w, PILL_H);
                x += w + TAB_GAP;
                rect
            })
            .collect()
    }

    fn rebuild(&mut self, focus: Option<crate::ui::machine::FocusKey<u32>>) {
        self.dirty = false;
        if let Some(p) = self.person() {
            self.name = p.name.clone();
            self.model = crate::person::filmography(p);
        }
        if !self.model.is_empty() && !self.model.iter().any(|d| d.title == self.department) {
            self.department = self
                .model
                .first()
                .map(|d| d.title.clone())
                .unwrap_or_default();
        }
        self.sync_keys();
        self.tab_c = self
            .model
            .iter()
            .map(|d| CString::new(format!("{} · {}", d.title, d.total)).unwrap_or_default())
            .collect();
        let sel = self
            .current_row(focus)
            .unwrap_or(self.table.sel.max(0) as usize);
        self.sync_rows(sel, self.current_row(focus).is_some());
        self.table.list_focused = self.current_row(focus).is_some();
    }

    fn sync_rows(&mut self, sel: usize, slide: bool) {
        let rows: Vec<TRow> =
            self.rows()
                .iter()
                .map(|c| {
                    let sub = match c.local.as_ref().and_then(|(sid, _)| {
                        crate::plex::server_facts(*sid).map(|f| f.name.clone())
                    }) {
                        Some(name) if c.role.is_empty() => name,
                        Some(name) => format!("{} · {}", c.role, name),
                        None => c.role.clone(),
                    };
                    TRow::new(c.title.clone())
                        .detail(sub)
                        .value(match c.year {
                            0 => "—".to_string(),
                            year => year.to_string(),
                        })
                        .value_dim(true)
                        .chevron(c.local.is_some())
                        .ticon_slot(true)
                })
                .collect();
        self.table.tall_rows(true);
        let mut section = Section::new("");
        section.rows = rows;
        self.table.set_sections(vec![section], sel as i32, slide);
    }

    fn pick_tab<H: ContentLike>(&mut self, i: usize, fx: &mut Effects<'_, H>) {
        let i = i.min(self.model.len().saturating_sub(1));
        let Some(department) = self.model.get(i).map(|d| d.title.clone()) else {
            return;
        };
        if department == self.department {
            return;
        }
        self.department = department;
        self.reindex();
        self.sync_rows(0, false);
        fx.invalidate(Provenance::Input);
    }

    fn tab_hscroll_target(&self, measure: &dyn Measure, focused_tab: Option<usize>) -> f32 {
        let Some(i) = focused_tab else {
            return self.tab_hscroll.pos;
        };
        let content = RouteLayout::screen_with_copy_w(COPY_W).content;
        let Some(rect) = self.pill_rects(measure).get(i).copied() else {
            return self.tab_hscroll.pos;
        };
        let x = rect.x + self.tab_hscroll.pos;
        let lo = x + rect.w + TAB_GAP - (content.x + content.w);
        let hi = x - TAB_FADE_W - content.x;
        card_row::reveal(self.tab_hscroll.pos, lo, hi, f32::MAX)
    }

    fn step_preview(&mut self, dt: f32, focus: Option<crate::ui::machine::FocusKey<u32>>) -> bool {
        let want = self
            .current_row(focus)
            .and_then(|i| self.rows().get(i))
            .map(|credit| (self.department.clone(), credit.catalog_id.clone()))
            .or_else(|| self.preview.clone());
        if want != self.pv_want {
            self.pv_want = want.clone();
            self.pv_still = 0.0;
        }
        let waiting = want != self.preview;
        if waiting {
            self.pv_still += dt;
            if self.pv_still >= PV_SETTLE && !self.pv_fade.is_swapping() {
                self.pv_fade.reload();
            }
        } else {
            self.pv_still = 0.0;
        }
        if self.pv_fade.tick(dt, true) {
            self.preview = want;
            self.pv_still = 0.0;
        }
        waiting || self.pv_fade.is_swapping()
    }

    fn tick<H: ContentLike>(&mut self, dt: f32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if self.dirty {
            self.rebuild(cx.focus.current);
        }
        let row = self.current_row(cx.focus.current);
        self.table.list_focused = row.is_some();
        if let Some(i) = row {
            self.table.sel = i as i32;
        }

        let tab = self.current_tab(cx.focus.current);
        self.pop.step(tab.map(|_| 0), dt);
        let spans: Vec<(f32, f32)> = self
            .pill_rects(cx.measure)
            .into_iter()
            .map(|r| (r.x + self.tab_hscroll.pos, r.w))
            .collect();
        self.tabs.update(
            self.selected_tab() as i32,
            tab.map_or(-1, |i| i as i32),
            |i| spans.get(i).copied(),
            SelMark::Travels,
            dt,
        );
        let before = (self.tab_hscroll.pos, self.tab_hscroll.vel);
        let target = self.tab_hscroll_target(cx.measure, tab);
        self.tab_hscroll.step(target, 240.0, dt);
        self.table.update(dt, table_frame().h);
        let preview_motion = self.step_preview(dt, cx.focus.current);
        if preview_motion || before != (self.tab_hscroll.pos, self.tab_hscroll.vel) {
            fx.note(PresentEvent::Motion);
        }
    }

    fn activate_focus<H: ContentLike>(
        &mut self,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
        fx: &mut Effects<'_, H>,
    ) {
        if let Some(i) = self.current_tab(focus) {
            self.pick_tab(i, fx);
            return;
        }
        if let Some((sid, rk)) = self.focused_target(focus) {
            fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                ContentReq::Push(ContentArg::Detail {
                    sid,
                    rk: rk.to_string(),
                }),
            )));
        }
    }

    fn draw_content<H: ContentLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f
            .painter
            .translate((1.0 - f.page_alpha) * SCR_W, 0.0)
            .alpha(f.page_alpha);
        self.ground.draw_host(p);
        self.ground_ready = opaque_ground_ready(f.page_alpha);
        let layout = RouteLayout::screen_with_copy_w(COPY_W);
        let total: usize = self.model.iter().map(|d| d.total).sum();
        layout.draw_narrative(
            p,
            Some(&self.name),
            "Filmography",
            &format!("{total} credits · Newest first."),
            theme::size::LABEL,
        );

        let rects = self.pill_rects(f.measure);
        if !self.tab_c.is_empty() {
            let clip_top = layout.content.y - CLIP_VPAD;
            let clip_h = PILL_H + 2.0 * CLIP_VPAD;
            p.clip(Rect::new(
                layout.content.x,
                clip_top,
                SCR_W - layout.content.x,
                clip_h,
            ));
            self.tabs.draw(
                p,
                layout.content.y,
                PILL_H,
                TabGround::Plated {
                    pop: self.pop.scale(0),
                },
            );
            let env = Env::inert();
            for (label, rect) in self.tab_c.iter().zip(rects.iter().copied()) {
                let content_x = rect.x + self.tab_hscroll.pos;
                let (focus_mix, selected_mix) = self.tabs.mixes((content_x, rect.w));
                TabPill::new(label.as_ptr(), theme::size::BODY, rect)
                    .plated()
                    .mix(focus_mix, selected_mix)
                    .draw(&env, p);
            }
            p.clip_clear();

            if self.tab_hscroll.pos > 0.5 {
                let fade = Rect::new(layout.content.x, clip_top, TAB_FADE_W, clip_h);
                let ground = &self.ground;
                let corner = |x: f32, y: f32, a: f32| {
                    let c = ground.sample(x, y);
                    [c[0], c[1], c[2], a]
                };
                p.grad4(
                    fade,
                    [
                        corner(fade.x, fade.y, 1.0),
                        corner(fade.x + fade.w, fade.y, 0.0),
                        corner(fade.x + fade.w, fade.y + fade.h, 0.0),
                        corner(fade.x, fade.y + fade.h, 1.0),
                    ],
                );
            }
        }

        let frame = table_frame();
        self.table.draw(p, frame);
        if let Some(thumb) = self
            .preview
            .as_ref()
            .and_then(|(department, catalog_id)| self.credit_by_identity(department, catalog_id))
            .map(|c| c.thumb.as_str())
            .filter(|s| !s.is_empty())
        {
            widgets::card(
                p.alpha(self.pv_fade.alpha()),
                PV,
                widgets::Art::Thumb {
                    sid: self.sid,
                    key: thumb,
                    res: (PV.w as c_int, PV.h as c_int),
                },
                theme::CARD_RING_RAD,
                false,
                1.0,
                0.0,
            );
        }
        widgets::continuous_scroll_rail(
            p,
            Rect::new(
                frame.x + frame.w - widgets::RAIL_W,
                frame.y,
                widgets::RAIL_W,
                frame.h,
            ),
            self.table.scroll_pos(),
            self.table.measured_height(),
            frame.h,
        );

        let clip = Rect::new(layout.content.x, 0.0, SCR_W - layout.content.x, SCR_H);
        for (i, rect) in rects.into_iter().enumerate() {
            f.stop(
                p,
                Stop {
                    key: self.focus_key(Located::Tab(i)),
                    rect,
                    rest_rect: rect,
                    clip,
                    hover: Hover::Ignore,
                    activate: Activate::Immediate,
                },
            );
        }
        for i in 0..self.rows().len() {
            let Some(rect) = self.table.row_rect(frame, i as i32) else {
                continue;
            };
            f.stop(
                p,
                Stop {
                    key: self.focus_key(Located::Row(i)),
                    rect,
                    rest_rect: rect,
                    clip: frame,
                    hover: Hover::Focus,
                    activate: Activate::Press,
                },
            );
        }
    }
}

impl<H: ContentLike> Focusable<H> for FilmographyScreen {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let layout = RouteLayout::screen_with_copy_w(COPY_W);
        if !self.model.is_empty() {
            out.push(GroupSpec {
                id: TAB_GROUP,
                kind: GroupKind::Row { wrap: false },
                seat: Seat::Remembered,
                reachable: AxisMask::VERTICAL,
                edge: [
                    EdgeRule::Stop,
                    EdgeRule::Geometric,
                    EdgeRule::Stop,
                    EdgeRule::Stop,
                ],
                extent: Rect::new(layout.content.x, layout.content.y, layout.content.w, PILL_H),
                len: self.model.len(),
                elem: ElemKind::Control,
            });
        }
        if !self.rows().is_empty() {
            out.push(GroupSpec {
                id: LIST_GROUP,
                kind: GroupKind::Column,
                seat: Seat::First,
                reachable: AxisMask::VERTICAL,
                edge: [
                    EdgeRule::Geometric,
                    EdgeRule::Stop,
                    EdgeRule::Stop,
                    EdgeRule::Stop,
                ],
                extent: table_frame(),
                len: self.rows().len(),
                elem: ElemKind::Card,
            });
        }
    }

    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        match self.locate(*elem)? {
            Located::Tab(i) if i < self.model.len() => Some(TAB_GROUP),
            Located::Row(i) if i < self.rows().len() => Some(LIST_GROUP),
            _ => None,
        }
    }

    fn neighbour(
        &self,
        current: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        _cx: &Cx<'_, H>,
    ) -> Step<u32> {
        match self.locate(current.elem) {
            Some(Located::Tab(i)) => match dir {
                Dir::Left if i > 0 => Step::Move(self.focus_key(Located::Tab(i - 1))),
                Dir::Right if i + 1 < self.model.len() => {
                    Step::Move(self.focus_key(Located::Tab(i + 1)))
                }
                _ => Step::Edge,
            },
            Some(Located::Row(i)) => match dir {
                Dir::Up if i > 0 => Step::Move(self.focus_key(Located::Row(i - 1))),
                Dir::Down if i + 1 < self.rows().len() => {
                    Step::Move(self.focus_key(Located::Row(i + 1)))
                }
                _ => Step::Edge,
            },
            None => Step::Edge,
        }
    }

    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        match self.locate(*elem)? {
            Located::Tab(i) => {
                let rect = *self.pill_rects(cx.measure).get(i)?;
                let layout = RouteLayout::screen_with_copy_w(COPY_W);
                Some(Placed {
                    rect,
                    rest_rect: rect,
                    clip: Rect::new(layout.content.x, 0.0, SCR_W - layout.content.x, SCR_H),
                    index: Some(i as u32),
                })
            }
            Located::Row(i) => {
                let rect = self.table.row_frame(table_frame(), i as i32)?;
                Some(Placed {
                    rect,
                    rest_rect: rect,
                    clip: table_frame(),
                    index: Some(i as u32),
                })
            }
        }
    }

    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        match self.locate(want.elem) {
            Some(Located::Tab(i)) if !self.model.is_empty() => {
                self.focus_key(Located::Tab(i.min(self.model.len() - 1)))
            }
            Some(Located::Row(i)) if !self.rows().is_empty() => {
                self.focus_key(Located::Row(i.min(self.rows().len() - 1)))
            }
            _ if !self.model.is_empty() => self.focus_key(Located::Tab(self.selected_tab())),
            _ => crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: FIRST_ELEM,
            },
        }
    }

    fn seat(
        &self,
        group: GroupId,
        from: Placed,
        cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        if group == LIST_GROUP {
            return self.focus_key(Located::Row(0));
        }
        let centre = from.rect.x + from.rect.w * 0.5;
        let i = self
            .pill_rects(cx.measure)
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.x + a.w * 0.5 - centre).abs();
                let db = (b.x + b.w * 0.5 - centre).abs();
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
            .unwrap_or_else(|| self.selected_tab());
        self.focus_key(Located::Tab(i))
    }
}

impl<H: ContentLike> Machine<H> for FilmographyScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Filmography(saved)) => {
                // The live body may have interned another landing after the request snapshot.
                // Preserve those identities while hydrating the request-time UI choices.
                let mut memory = saved.clone();
                for key in &self.keys {
                    if !memory.keys.iter().any(|saved| saved.department == key.department
                        && saved.catalog_id == key.catalog_id) {
                        assert!(!memory.keys.iter().any(|saved| saved.elem == key.elem),
                            "restored Filmography key collision");
                        memory.keys.push(key.clone());
                    }
                }
                memory.next_elem = memory.next_elem.max(self.next_elem);
                self.restore(&memory);
                Handled::Yes
            }
            ScreenEvent::Tick(tick) => {
                self.tick(tick.dt(), cx, fx);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                match self.locate(to.elem) {
                    Some(Located::Tab(i)) => {
                        self.table.list_focused = false;
                        if !matches!(by, crate::ui::screen::By::Pointer) {
                            self.pick_tab(i, fx);
                        }
                    }
                    Some(Located::Row(i)) => {
                        self.table.list_focused = true;
                        self.table.sel = i as i32;
                    }
                    None => {}
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.activate_focus(
                    Some(crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: *elem,
                    }),
                    fx,
                );
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                self.activate_focus(cx.focus.current, fx);
                Handled::Yes
            }
            // Credits never fabricate a `PmsMovie`, including locally joined ones. The rows are
            // Cards retain the ordinary hold gesture, but decline its action; a held row must
            // neither fabricate a catalog item/menu nor activate when its key is released.
            ScreenEvent::PressHold(_) => Handled::No,
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Back, .. },
                ..
            }) => {
                fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                    ContentReq::Back,
                )));
                Handled::Yes
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == crate::stores::StoreId::Person.ord() => {
                if self.person().is_some() {
                    self.rebuild(cx.focus.current);
                }
                Handled::Yes
            }
            // Cover/restore is deliberately inert. The modal entry owns its state and the engine
            // restores its focus from ReturnState; resetting here would lose the exact route the
            // legacy person→filmography→detail→BACK flow preserved.
            ScreenEvent::Enter(_) | ScreenEvent::Cover | ScreenEvent::Uncover => Handled::Yes,
            ScreenEvent::WillLeave(crate::ui::machine::Leave::ForGood) | ScreenEvent::Unmount => {
                self.ground.reset();
                self.ground_ready = false;
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike> Screen<H> for FilmographyScreen {
    fn name(&self) -> &'static str {
        // The test manifest intentionally records this opaque modal as route=person.
        super::registry::word::PERSON
    }

    fn state(&self) -> &dyn LogicalState {
        self
    }

    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        Some(Cow::Owned(self.name.clone()))
    }

    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}

    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        self.draw_content(f);
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

    fn ground_ready(&self) -> bool {
        self.ground_ready
    }

    fn memory_at(&self, _focus: Option<crate::ui::machine::FocusKey<u32>>) -> PageMemory {
        PageMemory::Filmography(self.memory())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{
        Edge, FocusRead, Host, InputOwner, PressId, PressRead, Source, Stamped, Tick,
    };

    struct FilmographyHost;

    impl Host for FilmographyHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = super::super::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = super::super::family::NoInit;
        type Memory = PageMemory;
    }

    fn dept(title: &str, rows: Vec<Credit>) -> Department {
        Department {
            title: title.to_string(),
            total: rows.len(),
            rows,
        }
    }

    fn credit(title: &str, role: &str, year: i32, local: Option<(ServerId, &str)>) -> Credit {
        Credit {
            catalog_id: format!("catalog-{title}"),
            title: title.to_string(),
            thumb: String::new(),
            role: role.to_string(),
            year,
            local: local.map(|(sid, rk)| (sid, rk.to_string())),
        }
    }

    fn screen(entry: u32, _serial: &crate::testlock::Serial) -> FilmographyScreen {
        let mut s =
            FilmographyScreen::new(EntryId(entry), ServerId::UNSET, format!("person-{entry}"));
        s.name = format!("Person {entry}");
        s.model = vec![
            dept(
                "Actor",
                (0..40)
                    .map(|i| credit(&format!("Film {i}"), &format!("Role {i}"), 2020 - i, None))
                    .collect(),
            ),
            dept(
                "Writer",
                vec![credit(
                    "Written",
                    "Writer",
                    2012,
                    Some((ServerId::UNSET, "77")),
                )],
            ),
        ];
        s.tab_c = vec![
            CString::new("Actor · 40").unwrap(),
            CString::new("Writer · 1").unwrap(),
        ];
        s.department = "Actor".to_string();
        s.sync_keys();
        s.sync_rows(0, false);
        s
    }

    /// Tests that seed a model directly must maintain the same derived-index invariant the only
    /// production writer (`pick_tab`) does before rebuilding the shared table.
    fn select(s: &mut FilmographyScreen, department: &str) {
        s.department = department.to_string();
        s.reindex();
        s.sync_rows(0, false);
    }

    fn focus(s: &FilmographyScreen, located: Located) -> crate::ui::machine::FocusKey<u32> {
        s.focus_key(located)
    }

    fn cx<'a>(
        measure: &'a FixtureMeasure,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
    ) -> Cx<'a, FilmographyHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead { current: focus , ..Default::default() },
            owner: InputOwner::Entry(focus.map_or(EntryId(0), |k| k.entry)),
        }
    }

    fn step_screen(
        s: &mut FilmographyScreen,
        ev: &ScreenEvent<FilmographyHost>,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
    ) -> (Handled, Vec<Stamped<FilmographyHost>>, bool) {
        let measure = FixtureMeasure;
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let handled = {
            let mut fx = Effects::new(
                &mut out,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
                &mut present,
            );
            Machine::<FilmographyHost>::step(s, ev, &cx(&measure, focus), &mut fx)
        };
        (handled, out, present.peek(0))
    }

    fn has_content(out: &[Stamped<FilmographyHost>], pred: impl Fn(&ContentReq) -> bool) -> bool {
        out.iter().any(|st| match &st.fx {
            crate::ui::machine::Fx::App(AppFx::Content(req)) => pred(req),
            _ => false,
        })
    }

    /// Replaces legacy `the_cursor_lives_in_the_widget_and_nowhere_else`: the engine query owns
    /// navigation; TableView receives only the render projection on Tick.
    #[test]
    fn the_cursor_lives_in_the_engine_and_nowhere_in_logical_state() {
        let _serial = crate::testlock::serial();
        let mut s = screen(7, &_serial);
        let measure = FixtureMeasure;
        let first = focus(&s, Located::Row(0));
        let second = focus(&s, Located::Row(1));
        assert!(matches!(
            Focusable::<FilmographyHost>::neighbour(
                &s,
                first,
                Dir::Down,
                &cx(&measure, Some(first))
            ),
            Step::Move(k) if k == second
        ));
        let _ = step_screen(
            &mut s,
            &ScreenEvent::Tick(Tick {
                ms: 16,
                dt_us: 16_667,
            }),
            Some(second),
        );
        assert_eq!(s.table.sel, 1);
        assert_eq!(
            s.department, "Actor",
            "focus is not the selected department"
        );
    }

    #[test]
    fn the_viewport_is_a_whole_number_of_rows() {
        let l = RouteLayout::screen_with_copy_w(COPY_W);
        assert_eq!(l.narrative.w, 480.0);
        assert_eq!(l.content.x, 760.0);
        assert_eq!(l.content.w, 1064.0);
        let frame = table_frame();
        let h = frame.h - crate::ui::table::TOP_PAD - crate::ui::table::BOT_PAD;
        assert_eq!(h, 784.0);
        assert_eq!(h / crate::ui::table::ROW_H_ART, 8.0);
    }

    /// Replaces the legacy left-cut pointer test through the engine's placed clip.
    #[test]
    fn a_tab_stop_is_clipped_at_the_content_columns_left_edge() {
        let _serial = crate::testlock::serial();
        let mut s = screen(3, &_serial);
        s.tab_hscroll.jump(650.0);
        let measure = FixtureMeasure;
        let placed = Focusable::<FilmographyHost>::place(
            &s,
            &focus(&s, Located::Tab(1)).elem,
            &cx(&measure, None),
            At::Drawn,
        )
        .unwrap();
        assert_eq!(
            placed.clip.x,
            RouteLayout::screen_with_copy_w(COPY_W).content.x
        );
        assert!(placed.rect.x < placed.clip.x);
    }

    #[test]
    fn a_held_dpad_does_not_swap_the_preview() {
        let _serial = crate::testlock::serial();
        let mut s = screen(4, &_serial);
        s.preview = Some(("Actor".to_string(), "catalog-Film 0".to_string()));
        s.pv_want = s.preview.clone();
        let dt = 1.0 / 60.0;
        let mut since_repeat = 0.0;
        let mut row = 0usize;
        let mut swaps = 0;
        let mut last = s.preview.clone();
        for _ in 0..120 {
            since_repeat += dt;
            if since_repeat >= 0.110 {
                since_repeat = 0.0;
                row = (row + 1).min(39);
            }
            let row_focus = focus(&s, Located::Row(row));
            s.step_preview(dt, Some(row_focus));
            if s.preview != last {
                swaps += 1;
                last = s.preview.clone();
            }
        }
        assert_eq!(swaps, 0);
        for _ in 0..60 {
            let row_focus = focus(&s, Located::Row(row));
            s.step_preview(dt, Some(row_focus));
        }
        assert_eq!(
            s.preview,
            Some(("Actor".to_string(), format!("catalog-Film {row}")))
        );
    }

    #[test]
    fn only_a_joined_credit_exposes_a_real_target_and_no_catalog_item() {
        let _serial = crate::testlock::serial();
        let mut s = screen(9, &_serial);
        select(&mut s, "Writer");
        let held = focus(&s, Located::Row(0));
        assert_eq!(s.focused_target(Some(held)), Some((ServerId::UNSET, "77")));
        let (handled, out, _) =
            step_screen(&mut s, &ScreenEvent::PressHold(PressId(1)), Some(held));
        assert_eq!(handled, Handled::No);
        assert!(!has_content(&out, |r| matches!(r, ContentReq::ItemMenu)));

        select(&mut s, "Actor");
        assert!(s.focused_target(Some(focus(&s, Located::Row(0)))).is_none());
    }

    #[test]
    fn holding_a_joined_credit_does_not_activate_it_on_release() {
        use crate::ui::input::{InputMachine, PressEvent};
        use crate::ui::machine::{InstanceId, MachineId, PressArm, PressFrom};
        let _serial = crate::testlock::serial();
        let mut s = screen(9, &_serial);
        select(&mut s, "Writer");
        let key = focus(&s, Located::Row(0));
        let mut groups = Vec::new();
        Focusable::<FilmographyHost>::groups(&s, &cx(&FixtureMeasure, Some(key)), &mut groups);
        let kind = groups.iter().find(|g| g.id == LIST_GROUP).unwrap().elem;
        let mut input = InputMachine::new();
        input.arm(PressArm { key, from: PressFrom::Key, holdable: kind == ElemKind::Card },
                  MachineId::Instance(InstanceId(1)), 16);
        let mut holds = 0;
        let mut commits = 0;
        for ms in (32..=960).step_by(16) {
            if ms == 656 { input.release(ms); }
            for event in input.tick(ms, 0.016) {
                match event {
                    PressEvent::Hold(id, _, _) => {
                        holds += 1;
                        let (handled, out, _) = step_screen(&mut s, &ScreenEvent::PressHold(id), Some(key));
                        assert_eq!(handled, Handled::No);
                        assert!(!has_content(&out, |_| true));
                    }
                    PressEvent::Commit(_, _, _) => commits += 1,
                }
            }
        }
        assert_eq!(holds, 1, "credits retain the card hold gesture but decline its action");
        assert_eq!(commits, 0, "a declined hold must not turn into navigation on release");
    }

    #[test]
    fn cached_credit_activation_targets_participate_in_logical_state() {
        let _serial = crate::testlock::serial();
        let mut s = screen(9, &_serial);
        select(&mut s, "Writer");
        let key = focus(&s, Located::Row(0));
        assert_eq!(s.focused_target(Some(key)), Some((ServerId::UNSET, "77")));
        let before = s.hash();
        let tab = s.selected_tab();
        s.model[tab].rows[0].local = Some((ServerId::UNSET, "78".into()));
        assert_eq!(s.focused_target(Some(key)), Some((ServerId::UNSET, "78")));
        assert_ne!(s.hash(), before, "a displaced store cannot stand in for this retained model");
    }

    #[test]
    fn pending_preview_identity_and_commit_deadline_participate_in_logical_state() {
        let _serial = crate::testlock::serial();
        let mut s = screen(9, &_serial);
        let before = s.hash();
        s.pv_want = Some(("Writer".into(), "catalog-Film 0".into()));
        let wanted = s.hash();
        assert_ne!(wanted, before);
        s.pv_still = PV_SETTLE / 2.0;
        let waiting = s.hash();
        assert_ne!(waiting, wanted);
        s.pv_fade.reload();
        assert_ne!(s.hash(), waiting, "the fade's outgoing phase controls the preview commit");
    }

    #[test]
    fn live_return_hydrates_the_request_time_department_and_preview() {
        let _serial = crate::testlock::serial();
        let mut s = screen(9, &_serial);
        select(&mut s, "Writer");
        let saved = s.memory();
        select(&mut s, "Actor");
        s.preview = Some(("Actor".into(), "catalog-Film 0".into()));
        let (handled, _, _) = step_screen(&mut s,
            &ScreenEvent::RestoreMemory(PageMemory::Filmography(saved.clone())), None);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(s.department, saved.department);
        assert_eq!(s.preview, saved.preview);
    }

    #[test]
    fn switching_department_resets_the_list_render_anchor() {
        let _serial = crate::testlock::serial();
        let mut s = screen(5, &_serial);
        s.table.sel = 8;
        let from = focus(&s, Located::Tab(0));
        let to = focus(&s, Located::Tab(1));
        let (_, out, invalidated) = step_screen(
            &mut s,
            &ScreenEvent::FocusMoved {
                from: Some(from),
                to,
                by: crate::ui::screen::By::Dir,
            },
            Some(to),
        );
        assert_eq!(s.department, "Writer");
        assert_eq!(s.table.sel, 0);
        assert!(out.is_empty());
        assert!(invalidated);
    }

    #[test]
    fn back_emits_modal_dismissal_through_the_content_contract() {
        let _serial = crate::testlock::serial();
        let mut s = screen(6, &_serial);
        let ev = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Replay,
            kind: InputKind::Key {
                key: Key::Back,
                sym: 0,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        let entry = focus(&s, Located::Tab(0));
        let (handled, out, _) = step_screen(&mut s, &ev, Some(entry));
        assert_eq!(handled, Handled::Yes);
        assert!(has_content(&out, |r| matches!(r, ContentReq::Back)));
    }

    #[test]
    fn joined_row_commit_pushes_detail_and_external_row_does_nothing() {
        let _serial = crate::testlock::serial();
        let mut s = screen(10, &_serial);
        select(&mut s, "Writer");
        let held = focus(&s, Located::Row(0));
        let (_, out, _) = step_screen(&mut s, &ScreenEvent::PressCommit(PressId(1)), Some(held));
        assert!(has_content(&out, |r| matches!(
            r,
            ContentReq::Push(ContentArg::Detail { sid, rk })
                if *sid == ServerId::UNSET && rk == "77"
        )));

        select(&mut s, "Actor");
        let external = focus(&s, Located::Row(0));
        let (_, out, _) = step_screen(
            &mut s,
            &ScreenEvent::PressCommit(PressId(2)),
            Some(external),
        );
        assert!(!has_content(&out, |_| true));
    }

    #[test]
    fn independent_instances_do_not_share_department_or_preview() {
        let _serial = crate::testlock::serial();
        let mut a = screen(20, &_serial);
        let b = screen(21, &_serial);
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::<FilmographyHost>::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(1)),
            &mut present,
        );
        a.pick_tab(1, &mut fx);
        a.preview = Some(("Writer".to_string(), "catalog-Written".to_string()));
        assert_eq!(a.department, "Writer");
        assert_eq!(b.department, "Actor");
        assert_eq!(b.preview, None);
        assert_ne!(a.entry, b.entry);
    }

    #[test]
    fn restore_preserves_department_preview_and_table_motion() {
        let _serial = crate::testlock::serial();
        let mut original = screen(30, &_serial);
        select(&mut original, "Writer");
        original.preview = Some(("Writer".to_string(), "catalog-Written".to_string()));
        let old_focus = focus(&original, Located::Row(0));
        let old_tab_focus = focus(&original, Located::Tab(1));
        let PageMemory::Filmography(memory) =
            Screen::<FilmographyHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Filmography must persist stable identities in PageMemory");
        };

        let mut remounted = screen(30, &_serial);
        let writer = remounted
            .model
            .iter_mut()
            .find(|department| department.title == "Writer")
            .unwrap();
        writer
            .rows
            .insert(0, credit("Earlier", "Writer", 2013, None));
        writer.total += 1;
        remounted.model.reverse();
        remounted.restore(&memory);
        let restored = Focusable::<FilmographyHost>::reconcile(
            &remounted,
            old_focus,
            &cx(&FixtureMeasure, Some(old_focus)),
        );
        assert_eq!(restored.elem, old_focus.elem);
        assert_eq!(
            remounted.focused_target(Some(restored)),
            Some((ServerId::UNSET, "77")),
            "the held credit, not the old row index, survives reorder"
        );
        let restored_tab = Focusable::<FilmographyHost>::reconcile(
            &remounted,
            old_tab_focus,
            &cx(&FixtureMeasure, Some(old_tab_focus)),
        );
        assert_eq!(restored_tab.elem, old_tab_focus.elem);
        assert_eq!(
            remounted.current_tab(Some(restored_tab)),
            Some(0),
            "the Writer tab moved to index 0 but kept its engine identity"
        );

        let before_scroll = remounted.table.scroll_pos();
        let (_, out, _) = step_screen(
            &mut remounted,
            &ScreenEvent::Enter(crate::ui::screen::Enter::Restored),
            Some(restored),
        );
        assert!(out.is_empty());
        assert_eq!(remounted.department, "Writer");
        assert_eq!(
            remounted.preview,
            Some(("Writer".to_string(), "catalog-Written".to_string()))
        );
        assert_eq!(remounted.table.scroll_pos(), before_scroll);
    }

    #[test]
    fn heartbeat_stays_person_for_the_separately_mounted_modal() {
        let _serial = crate::testlock::serial();
        let s = screen(40, &_serial);
        assert_eq!(Screen::<FilmographyHost>::name(&s), "person");
    }

    #[test]
    fn opaque_host_is_replaced_only_after_the_ground_reaches_full_strength() {
        assert!(!opaque_ground_ready(0.0));
        assert!(!opaque_ground_ready(0.994));
        assert!(opaque_ground_ready(0.995));
        assert!(opaque_ground_ready(1.0));
    }

    #[test]
    fn restore_holds_stable_department_and_preview_until_async_model_arrives() {
        let _serial = crate::testlock::serial();
        let mut source = screen(50, &_serial);
        select(&mut source, "Writer");
        source.preview = Some(("Writer".to_string(), "catalog-Written".to_string()));
        let memory = source.memory();
        let model = std::mem::take(&mut source.model);

        let mut remounted =
            FilmographyScreen::new(EntryId(50), ServerId::UNSET, "person-50".to_string());
        assert!(remounted.model.is_empty());
        remounted.restore(&memory);
        assert_eq!(remounted.department, "Writer");
        assert_eq!(remounted.preview, memory.preview);

        remounted.model = model;
        remounted.rebuild(None);
        assert_eq!(remounted.selected_tab(), 1);
        assert!(remounted
            .credit_by_identity("Writer", "catalog-Written")
            .is_some());
    }
}
