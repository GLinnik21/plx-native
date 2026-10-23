//! **Play Version** — the Detail hero's list of a movie's or episode's versions, opened by the
//! Play Version disc.
//!
//! A `Style::Compact` surface anchored to the disc that opened it, the *Also available* panel's
//! shape (`screens::alt_sources`): the anchor rides on [`VersionsArg`], the surface owns input
//! while it is up, and it REPORTS the choice. A row delivers [`AppMsg::PlayVersion`] to the Detail
//! instance named on its argument and the PAGE plays it, the way its Play pill does (resuming), so
//! "what Play means on this page" stays in one place.
//!
//! The rows are `plx_appkit::version_list`, the same builder the item menu's Play Version page
//! draws, captured when the panel opens: a refetch landing behind an open list does not move a
//! row under the cursor. It opens on the version Play takes unasked, the server's first
//! (`plx_plex::plex::Metadata::media_version(None)`).

use std::borrow::Cow;
use std::convert::Infallible;

use plx_data::pms::MediaVersion;
use plx_plex::plex::{MediaId, ServerId};
use crate::registry::{AppLike, AppMsg, PageMemory};
use plx_ui::frame::Budget;
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InstanceId, Key, LogicalState, Machine, MachineId, NavOp,
};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind,
    GroupSpec, Hover, HitSource, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step,
    Stop,
};
use plx_ui::form::{Form, FormId, FormTable, RowKey};
use plx_ui::Rect;

/// The fields [`VersionsPanelScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
pub const SHAPE: &str =
    "VersionsPanel{arg:VersionsArg{host:u32,sid:u32,rk:str,anchor:[u32;4]},versions:[i64],sel:i32,table:TableViewMotion}";

/// What the container is asked to present: the Detail instance the choice is reported to, the item
/// whose versions these are, and the drawn rect of the disc it hangs off (bit-preserving, so a
/// canonical argument holds it without float equality).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VersionsArg {
    pub host: InstanceId,
    pub sid: ServerId,
    pub rk: String,
    pub anchor: [u32; 4],
}

impl LogicalState for VersionsArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.host.0).u32(u32::from(self.sid.raw())).str(&self.rk);
        for value in self.anchor {
            c.u32(value);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str("versions_arg");
    }
}

/// A row's identity: its position in the list captured at open (a dynamic list's documented key).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VersionRow(pub u16);

impl FormId for VersionRow {
    fn key(&self) -> RowKey {
        RowKey(u32::from(self.0))
    }
}

/// The pinned ~20px corner radius, the item menu's and *Also available*'s.
const PANEL_RAD: f32 = 20.0;
const RISE: f32 = plx_ui::popover::Popover::RISE;

pub struct VersionsPanelScreen {
    entry: EntryId,
    arg: VersionsArg,
    /// The versions the rows were built from, in the server's order.
    versions: Vec<MediaId>,
    pub form: FormTable<VersionRow, MediaId, Infallible>,
}

impl VersionsPanelScreen {
    pub fn new(entry: EntryId, arg: VersionsArg, versions: &[MediaVersion]) -> Self {
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        // rows carry a sub-line: the track menu's size class
        form.table.compact = false;
        form.set(Form::new().section(plx_appkit::version_list::section(versions, VersionRow, |v| v.id)), Some(&VersionRow(0)));
        Self { entry, arg, versions: versions.iter().map(|v| v.id).collect(), form }
    }

    /// Under the disc when there is room, above it when there is not.
    pub fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        plx_ui::popover::under_or_above(Rect::new(x, y, w, h), self.form.table.measured_width(measure), self.form.table.measured_height())
    }

    fn dismiss<H: AppLike>(&self, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// Close, and report row `elem`'s version to the page. Takes the elem, not the cursor: a click
    /// names the stop it hit (`AltSourcesScreen::commit`).
    fn commit<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let media = match self.form.index_of_key(RowKey(elem)).and_then(|i| self.form.activate(i)) {
            Some(plx_ui::form::Activation::Action(media)) => Some(media),
            _ => None,
        };
        self.dismiss(fx);
        if let Some(media) = media {
            fx.push(Fx::Deliver(
                MachineId::Instance(self.arg.host),
                Delivery::Screen(ScreenEvent::App(AppMsg::PlayVersion(media))),
            ));
        }
    }
}

impl<H: AppLike<Memory = PageMemory>> Machine<H> for VersionsPanelScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(tick) => {
                self.form.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .and_then(|key| self.form.index_of_key(RowKey(key.elem)))
                    .map_or(self.form.table.sel, |i| i as i32);
                self.form.table.update(tick.dt(), self.frame(cx.measure).h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(i) = self.form.index_of_key(RowKey(to.elem)) {
                    self.form.table.sel = i as i32;
                }
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.commit(*elem, fx);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.commit(key.elem, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(input) => match input.kind {
                InputKind::Key { key: Key::Back, edge: Edge::Down, .. } => {
                    self.dismiss(fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

/// One column of `Bare` rows over the table's own cursor, *Also available*'s `Focusable`.
impl<H: AppLike<Memory = PageMemory>> Focusable<H> for VersionsPanelScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: self.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.form.index_of_key(RowKey(*elem)).map(|_| GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.form.step_key(RowKey(key.elem), delta) {
            Some(next) => Step::Move(FocusKey { entry: self.entry, elem: next.0 }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let index = self.form.index_of_key(RowKey(*elem))?;
        let rect = self.form.table.row_frame(self.frame(cx.measure), index as i32)?;
        Some(Placed { rect, rest_rect: rect, clip: self.frame(cx.measure), index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey { entry: self.entry, elem: self.form.opening_key().map_or(0, |k| k.0) }
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.form.selected_key().or_else(|| self.form.opening_key()).map_or(0, |k| k.0),
        }
    }
}

impl LogicalState for VersionsPanelScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        c.seq(self.versions.len());
        for id in &self.versions {
            c.u64(id.0 as u64);
        }
        c.u32(self.form.table.sel as u32);
        self.form.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("versions");
    }
}

impl<H: AppLike<Memory = PageMemory>> Screen<H> for VersionsPanelScreen {
    fn name(&self) -> &'static str {
        "versions"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn scrim(&self) -> Scrim {
        Scrim::dim(plx_ui::theme::underlay::DIM_PANEL)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let appear = f.page_alpha;
        let r = self.frame(f.measure);
        let p = f.painter.alpha(appear).translate(0.0, RISE * (1.0 - appear));
        let measure = f.measure;
        let field = f.underlay;
        plx_ui::profile::phase("dt.versions", || {
            plx_ui::widgets::panel_ground(p, r, PANEL_RAD, field, plx_ui::widgets::Ground::Sheet);
            self.form.table.draw(p, r, measure);
        });
        let hit_p = f.painter.alpha(appear);
        for index in 0..self.form.table.n_rows() as usize {
            let Some(elem) = self.form.key_at(index).map(|k| k.0) else {
                continue;
            };
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    hit_p,
                    Stop {
                        key: FocusKey { entry: self.entry, elem },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
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
mod tests {
    use super::*;
    use plx_machine::machine::Host;
    use plx_ui::screen::ScreenArg;

    #[derive(Clone)]
    struct Arg;
    impl LogicalState for Arg {
        fn write(&self, _: &mut Canon) {}
        fn probe(&self, _: &mut String) {}
    }
    impl ScreenArg for Arg {
        fn chrome(&self) -> plx_machine::machine::Chrome {
            plx_machine::machine::Chrome::None
        }
        fn id(&self) -> plx_machine::machine::ScreenId {
            plx_machine::machine::ScreenId(1)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, _: &Self) -> bool {
            true
        }
    }
    struct HostFixture;
    impl Host for HostFixture {
        type Arg = Arg;
        type Fx = crate::registry::AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = Arg;
        type Memory = PageMemory;
    }

    fn panel() -> VersionsPanelScreen {
        let anchor = [500.0f32, 600.0, 64.0, 64.0].map(f32::to_bits);
        VersionsPanelScreen::new(
            EntryId(3),
            VersionsArg { host: InstanceId(9), sid: ServerId::from_raw(1), rk: "42".into(), anchor },
            &plx_appkit::version_list::sample(),
        )
    }

    /// OK on a row closes the panel and reports THAT version to the page that opened it.
    #[test]
    fn a_row_reports_its_version_to_the_host_page() {
        let mut p = panel();
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx: Effects<'_, HostFixture> =
            Effects::new(&mut out, MachineId::Instance(InstanceId(4)), &mut present);
        p.commit(1, &mut fx);
        let fx: Vec<_> = out.into_iter().map(|e| e.fx).collect();
        assert!(matches!(fx.first(), Some(Fx::Nav(NavOp::Dismiss(EntryId(3))))), "the panel closes");
        let played = fx.iter().find_map(|f| match f {
            Fx::Deliver(MachineId::Instance(InstanceId(9)), Delivery::Screen(ScreenEvent::App(AppMsg::PlayVersion(m)))) => Some(*m),
            _ => None,
        });
        assert_eq!(played, Some(plx_appkit::version_list::sample()[1].id), "the host page is told which version");
    }
}
