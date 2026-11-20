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
use crate::registry::{AppLike, AppMsg, MetadataLike, PageMemory};
use plx_ui::a11y::{A11y, A11yPath};
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
    /// `can_play` is whether the platform composed any playback driver (`ScreenInputs::can_play`):
    /// without one every row is disabled and says why, and presses nothing.
    pub fn new(entry: EntryId, arg: VersionsArg, versions: &[MediaVersion], can_play: bool) -> Self {
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        // rows carry a sub-line: the track menu's size class
        form.table.compact = false;
        let unavailable = (!can_play).then(plx_os::i18n::msg::browse_detail_no_playback);
        form.set(Form::new().section(plx_appkit::version_list::section(versions, VersionRow, |v| v.id, unavailable)), Some(&VersionRow(0)));
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
    /// names the stop it hit (`AltSourcesScreen::commit`). OK on a DISABLED row (no playback driver
    /// composed) does nothing and the panel stays up, as on every disabled row (`ui/CLAUDE.md`: "OK
    /// does nothing"); BACK still closes it.
    fn commit<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let activation = self.form.index_of_key(RowKey(elem)).map(|i| self.form.activate(i));
        if matches!(activation, Some(None)) {
            return;
        }
        let media = match activation.flatten() {
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
impl<H: AppLike<Memory = PageMemory> + MetadataLike> Focusable<H> for VersionsPanelScreen {
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

    /// **Row `n` is `row.<n>`**, the shared table convention
    /// ([`plx_ui::geom::table_a11y`]): `n` is the row's ordinal among the focusable rows (the same
    /// count `GroupSpec::len` states), so a separator or note never shifts an id. The label is the
    /// row's own text, for the state file only. The elements and the inverse are the shared ones.
    ///
    /// **A row plays, so it names what it plays.** OK delivers `AppMsg::PlayVersion` to the page, which plays the
    /// hero's title as its Play pill would, in the version the row names: the subject is the Play pill's own
    /// (`registry::versions_subject`: the page's loaded item, read from the metadata store the way the mount reads the
    /// versions), so `versions.row.<n>` needs a press that names the title. The key is derived from `cx`, not kept on the
    /// screen: it is no part of the screen's recorded state (`SHAPE`).
    fn a11y(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        let slot = self.form.index_of_key(RowKey(*elem))?;
        let row = plx_ui::geom::table_a11y(&self.form.table, Some(&self.form), slot)?;
        Some(match crate::registry::versions_subject(cx, &self.arg) {
            Some(rk) => row.with_subject(rk),
            None => row,
        })
    }
    fn a11y_elems(&self, g: GroupId, _cx: &Cx<'_, H>, out: &mut Vec<u32>) {
        if g == GroupId(0) {
            out.extend(
                plx_ui::geom::table_elems(&self.form.table, Some(&self.form))
                    .filter_map(|i| self.form.key_at(i))
                    .map(|k| k.0),
            );
        }
    }
    fn a11y_find(&self, path: &A11yPath, _cx: &Cx<'_, H>) -> Option<u32> {
        let slot = plx_ui::geom::table_a11y_find(&self.form.table, Some(&self.form), path)?;
        self.form.key_at(slot).map(|k| k.0)
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

impl<H: AppLike<Memory = PageMemory> + MetadataLike> Screen<H> for VersionsPanelScreen {
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
pub(crate) mod tests {
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

    thread_local! {
        // TEST ONLY: the page a panel reads its subject off, as `screens::detail::tests`'s `TEST_METADATA`.
        static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
    }
    fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
        TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
    }
    impl MetadataLike for HostFixture {
        fn metadata<'a>(_cx: &Cx<'a, Self>) -> plx_data::metadata::MetadataView<'a> {
            test_store().view()
        }
    }
    /// Make `d` the page the metadata store holds (or none), and keep the shared store to this test.
    fn page(d: Option<plx_data::metadata::Detail>) -> plx_base::testlock::Serial {
        let guard = plx_base::testlock::serial();
        plx_data::metadata::set_current_for_test(test_store().state_mut(), d);
        guard
    }

    fn panel() -> VersionsPanelScreen {
        let anchor = [500.0f32, 600.0, 64.0, 64.0].map(f32::to_bits);
        VersionsPanelScreen::new(
            EntryId(3),
            VersionsArg { host: InstanceId(9), sid: ServerId::from_raw(1), rk: "42".into(), anchor },
            &plx_appkit::version_list::sample(),
            true,
        )
    }

    fn cx() -> Cx<'static, HostFixture> {
        use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &plx_ui::fixture::FixtureMeasure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
            owner: InputOwner::Entry(EntryId(3)),
        }
    }

    /// The *Play Version* list over the sample versions, run through the id conformance
    /// (`AppArg::VersionsPanel`; the coverage table calls this).
    pub(crate) fn versions_panel_conforms() -> plx_ui::a11y::Namespaces {
        vec![plx_ui::a11y::conform::<HostFixture, _>(&panel(), &cx())]
    }

    /// Every version is a row with an id, `row.<n>` in the server's order, and the id finds it
    /// again.
    #[test]
    fn every_version_answers_an_id_in_the_servers_order() {
        let p = panel();
        let cx = cx();
        plx_ui::a11y::conform::<HostFixture, _>(&p, &cx);
        let n = plx_appkit::version_list::sample().len();
        assert!(n >= 2, "the sample has several versions");
        for i in 0..n {
            let path = plx_ui::a11y::A11yPath::parse_relative(&format!("row.{i}")).expect("a well-formed path");
            assert_eq!(Focusable::<HostFixture>::a11y_find(&p, &path, &cx), Some(i as u32), "row.{i} is version {i}");
        }
        let past = plx_ui::a11y::A11yPath::parse_relative(&format!("row.{n}")).expect("a well-formed path");
        assert_eq!(Focusable::<HostFixture>::a11y_find(&p, &past, &cx), None);
    }

    fn row_subject(p: &VersionsPanelScreen, row: u32) -> Option<String> {
        let cx = cx();
        Focusable::<HostFixture>::a11y(p, &row, &cx)?.subject.map(Cow::into_owned)
    }

    /// **A row plays, so it names the title it plays** (`versions.row.<n>` is in `SUBJECT_BEARING`): the subject is
    /// what the page's Play pill plays, the key `AppMsg::PlayVersion` acts on (`DetailScreen::play_hero`), and not the
    /// route's. A movie page plays itself; a show page plays the episode its hero plays, so the route's show key
    /// would name the wrong title. With no page loaded (or another one) a row names none, and the guarded doors
    /// cannot press it.
    #[test]
    fn a_row_names_the_title_its_page_plays() {
        use plx_data::metadata::{Detail, Episode};
        let sid = ServerId::from_raw(1);
        let p = panel();
        {
            let _guard = page(Some(Detail { sid, rk: "42".into(), kind: "movie".into(), ..Default::default() }));
            for row in 0..plx_appkit::version_list::sample().len() as u32 {
                assert_eq!(row_subject(&p, row).as_deref(), Some("42"), "row {row} plays the movie");
            }
        }
        {
            let episode = Episode { rk: "e1".into(), index: 1, season: 1, ..Default::default() };
            let _guard = page(Some(Detail {
                sid, rk: "42".into(), kind: "show".into(), is_show: true, episodes: vec![episode].into(), ..Default::default()
            }));
            assert_eq!(row_subject(&p, 0).as_deref(), Some("e1"), "a show's rows play the hero's episode, not the show");
        }
        {
            let _guard = page(Some(Detail { sid, rk: "another".into(), kind: "movie".into(), ..Default::default() }));
            assert_eq!(row_subject(&p, 0), None, "another page is loaded: the panel names no title");
        }
        let _guard = page(None);
        assert_eq!(row_subject(&p, 0), None, "no page loaded");
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

    /// A platform that composed no playback driver opens the same list with every row disabled and the
    /// one reason noted under them (`browse.detail.no_playback`, the Detail facts line's own word): OK
    /// on a row does nothing, as on every disabled row (`ui/CLAUDE.md`), so the panel stays up and
    /// nothing is asked of a driver that is not there, while BACK still closes it.
    #[test]
    fn with_no_playback_driver_every_row_is_disabled_and_reports_nothing() {
        use plx_machine::machine::{InputEvent, Source, Tick};
        let _lang = plx_os::i18n::language_on_this_thread_for_test(plx_os::i18n::Preference::En);
        let why = plx_os::i18n::msg::browse_detail_no_playback();
        let anchor = [500.0f32, 600.0, 64.0, 64.0].map(f32::to_bits);
        let arg = VersionsArg { host: InstanceId(9), sid: ServerId::from_raw(1), rk: "42".into(), anchor };
        // What one event does to a panel: the effects it pushes.
        let drive = |p: &mut VersionsPanelScreen, ev: ScreenEvent<HostFixture>| {
            let mut out = Vec::new();
            let mut present = plx_machine::present::Present::new();
            let mut fx: Effects<'_, HostFixture> =
                Effects::new(&mut out, MachineId::Instance(InstanceId(4)), &mut present);
            Machine::<HostFixture>::step(p, &ev, &cx(), &mut fx);
            out.into_iter().map(|e| e.fx).collect::<Vec<_>>()
        };
        let closes = |fx: &[Fx<HostFixture>]| fx.iter().any(|f| matches!(f, Fx::Nav(NavOp::Dismiss(EntryId(3)))));
        let reports = |fx: &[Fx<HostFixture>]| fx.iter().any(|f| matches!(f,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::App(AppMsg::PlayVersion(_))))));
        for (can_play, disabled) in [(true, false), (false, true)] {
            let mut p = VersionsPanelScreen::new(EntryId(3), arg.clone(), &plx_appkit::version_list::sample(), can_play);
            let rows: Vec<_> = p.form.table.sections.iter().flat_map(|s| s.rows.iter()).collect();
            let versions: Vec<_> = rows.iter().filter(|row| !row.is_note()).collect();
            let notes: Vec<_> = rows.iter().filter(|row| row.is_note()).collect();
            assert_eq!(versions.len(), 2);
            for row in versions {
                assert_eq!(row.disabled, disabled, "can_play={can_play}");
            }
            assert_eq!(notes.iter().map(|row| row.label.as_str()).collect::<Vec<_>>(),
                disabled.then_some(why).into_iter().collect::<Vec<_>>(), "can_play={can_play}: the one note says why");
            // OK, through the handler's own arm (`Activate`, what a click and the engine's OK deliver).
            let ok = drive(&mut p, ScreenEvent::Activate(1));
            assert_eq!((closes(&ok), reports(&ok)), (!disabled, !disabled),
                "can_play={can_play}: OK on a row closes the panel and reports it only while the row is enabled");
            if disabled {
                assert!(ok.is_empty(), "OK on a disabled row does nothing at all, got {} effects", ok.len());
            }
            // BACK closes the panel whichever state it is in, and reports nothing.
            let back = drive(&mut p, ScreenEvent::Input(InputEvent {
                at: Tick::default(),
                source: Source::Script,
                kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
            }));
            assert!(closes(&back) && !reports(&back), "can_play={can_play}: BACK closes the panel and plays nothing");
        }
    }
}
