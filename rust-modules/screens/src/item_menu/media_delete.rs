//! The shared item menu's destructive confirmation. Every operation leaves as an addressed effect.
use super::*;
use crate::registry::{MediaDeleteRequest, ScreenInputsLike};
use plx_plex::plex::DeleteResult;
use plx_ui::decision_alert::{Answers, Choice, DecisionAlert};
use plx_platform::i18n::msg;

const CANCEL: u32 = 240;
const DELETE: u32 = 241;

pub(super) struct DeleteUi {
    pub request: Option<MediaDeleteRequest>,
    title: String,
    alert: DecisionAlert,
    // 0: menu, 1: confirmation, 2: waiting, 3: terminal error.
    phase: u32,
    result: Option<DeleteResult>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{HostFixture, with_cx};
    use plx_machine::machine::{InputEvent, InstanceId, MachineId, PressId, Source, Tick};

    fn question() -> DeleteUi {
        let mut ui = DeleteUi::new();
        ui.title = "Disposable movie".into();
        ui.request = Some(MediaDeleteRequest { sid: plx_plex::plex::ServerId::UNSET,
            rk: "42".into(), kind: 0, show_rk: String::new(), profile: 7,
            key: plx_plex::plex::ClientKey { instance: 1, token: 2, grant: 3 } });
        ui.open();
        ui
    }
    fn key(key: Key) -> ScreenEvent<HostFixture> {
        ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::Sdl,
            kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } })
    }
    fn send(ui: &mut DeleteUi, ev: &ScreenEvent<HostFixture>, elem: u32) -> (bool, usize) {
        with_cx(|base| {
            let cx = Cx { views: (), tick: base.tick, measure: base.measure, press: base.press,
                owner: base.owner, focus: plx_machine::machine::FocusRead {
                    current: Some(FocusKey { entry: EntryId(7), elem }), ..Default::default() } };
            let mut out = Vec::new();
            let mut present = plx_machine::present::Present::new();
            let handled = ui.step(EntryId(7), ev, &cx,
                &mut Effects::new(&mut out, MachineId::Instance(InstanceId(8)), &mut present));
            (handled, out.iter().filter(|effect| matches!(effect.fx, Fx::App(AppFx::MediaDelete(_)))).count())
        })
    }
    #[test]
    fn confirmation_keeps_arrow_and_ok_input_in_the_shared_engine() {
        let mut ui = question();
        for key in [Key::Right, Key::Left, Key::Ok] {
            assert_eq!(send(&mut ui, &self::key(key), CANCEL), (false, 0));
        }
        with_cx(|cx| {
            let mut groups = Vec::new();
            assert!(ui.groups(cx, &mut groups));
            assert!(matches!(groups[0].elem, ElemKind::Control), "keyboard OK must use the shared press machine");
        });
        assert_eq!(ui.reconcile(EntryId(7), FocusKey { entry: EntryId(7), elem: 13 }).elem, CANCEL);
        assert!(matches!(ui.neighbour(ui.cancel_key(EntryId(7)), Dir::Right), Step::Move(k) if k.elem == DELETE));
    }
    #[test]
    fn back_cancels_even_if_a_confirm_commit_was_already_queued() {
        let mut ui = question();
        assert_eq!(send(&mut ui, &key(Key::Back), DELETE).1, 0);
        assert!(!ui.alert.is_open());
        assert_eq!(send(&mut ui, &ScreenEvent::PressCommit(PressId(0)), DELETE).1, 0);
    }
    #[test]
    fn changed_profile_or_permission_is_a_failure_without_a_write() {
        let mut ui = question();
        assert_eq!(send(&mut ui, &ScreenEvent::PressCommit(PressId(0)), DELETE).1, 0);
        assert_eq!(ui.phase, 3);
        assert_eq!(ui.alert.answers(), Answers::One);
    }
    #[test]
    fn a_repeat_commit_while_waiting_never_submits_another_delete() {
        let mut ui = question();
        ui.phase = 2;
        assert_eq!(send(&mut ui, &ScreenEvent::PressCommit(PressId(0)), DELETE).1, 0);
        assert_eq!(ui.phase, 2);
    }
}
impl DeleteUi {
    pub fn new() -> Self { Self { request: None, title: String::new(), alert: DecisionAlert::new(), phase: 0, result: None } }
    pub fn capture(&mut self, arg: &ItemMenuArg, meta: plx_data::metadata::MetadataView<'_>, inputs: &crate::registry::ScreenInputs) {
        let Some((_, key)) = inputs.deletable.iter().find(|(sid, _)| *sid == arg.sid) else { return };
        let (kind, title, show_rk) = match &arg.kind {
            ItemMenuKind::Detail { kind, title, show_rk, .. } => (*kind, title.clone(), show_rk.clone()),
            ItemMenuKind::Card { row, .. } if (0..=3).contains(&row.kind) =>
                (row.kind, row.title.clone(), row.show_rk.clone()),
            ItemMenuKind::Episode { .. } => {
                let Some(d) = meta.current().filter(|d| d.sid == arg.sid) else { return };
                let Some(ep) = d.episodes.iter_loaded().map(|(_, ep)| ep).chain(d.on_deck.iter()).find(|ep| ep.rk == arg.rk) else { return };
                (3, format!("{} · S{} E{} · {}", d.title, ep.season, ep.index, ep.title), d.rk.clone())
            }
            ItemMenuKind::Season { show_rk, index, .. } => {
                let Some(d) = meta.current().filter(|d| d.sid == arg.sid && d.rk == *show_rk) else { return };
                (2, format!("{} · {}", d.title, d.seasons.iter().find(|season| season.index == *index)
                    .map(|season| season.title.clone()).unwrap_or_else(|| index.to_string())), show_rk.clone())
            }
            _ => return,
        };
        if arg.rk.is_empty() || title.is_empty() { return; }
        self.request = Some(MediaDeleteRequest { sid: arg.sid, rk: arg.rk.clone(), kind, show_rk,
            profile: inputs.profile.generation, key: *key });
        self.title = title;
    }
    pub fn open(&mut self) {
        let Some(req) = &self.request else { return };
        let scope = match req.kind { 1 => msg::browse_delete_show(), 2 => msg::browse_delete_season(), _ => msg::browse_delete_leaf() };
        self.phase = 1;
        self.alert.open_card(msg::browse_delete_question_c(), vec![Cow::Owned(self.title.clone()), Cow::Borrowed(scope)], Answers::Two);
    }
    fn fail(&mut self, result: DeleteResult) {
        self.result = Some(result);
        let body = match result {
            DeleteResult::Forbidden => msg::browse_delete_forbidden(),
            DeleteResult::Missing => msg::browse_delete_missing(),
            DeleteResult::IdentityChanged => msg::browse_delete_changed(),
            _ => msg::browse_delete_uncertain(),
        };
        self.phase = 3;
        self.alert.open_card(msg::browse_delete_failed_c(), vec![Cow::Borrowed(body)], Answers::One);
    }
    pub fn visible(&self) -> bool { self.alert.visible() }
    pub fn cancel_key(&self, entry: EntryId) -> FocusKey<u32> { FocusKey { entry, elem: CANCEL } }
    pub fn reconcile(&self, entry: EntryId, want: FocusKey<u32>) -> FocusKey<u32> {
        if self.group_of(want.elem).is_some() { want } else { self.cancel_key(entry) }
    }
    pub fn group_of(&self, elem: u32) -> Option<GroupId> {
        (elem == CANCEL || (elem == DELETE && self.phase == 1)).then_some(GroupId(1))
    }
    pub fn groups<H: AppLike>(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) -> bool {
        if !self.visible() { return false; }
        out.push(GroupSpec { id: GroupId(1), kind: GroupKind::Row { wrap: false }, seat: Seat::First,
            reachable: AxisMask::BOTH, edge: [EdgeRule::Stop; 4], extent: Rect::FULL,
            len: if self.phase == 1 { 2 } else { 1 }, elem: ElemKind::Control });
        true
    }
    pub fn neighbour(&self, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        match (key.elem, dir, self.phase) {
            (CANCEL, Dir::Right, 1) => Step::Move(FocusKey { elem: DELETE, ..key }),
            (DELETE, Dir::Left, _) => Step::Move(FocusKey { elem: CANCEL, ..key }),
            _ => Step::Edge,
        }
    }
    pub fn place<H: AppLike>(&self, elem: u32, cx: &Cx<'_, H>) -> Option<Placed> {
        self.group_of(elem)?;
        let frames = self.alert.frames(cx.measure);
        let rect = if elem == DELETE { frames.1 } else { frames.0 };
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(u32::from(elem == DELETE)) })
    }
    pub fn step<H: AppLike + ScreenInputsLike>(&mut self, entry: EntryId, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> bool {
        if let ScreenEvent::App(crate::registry::AppMsg::MediaDelete(Some(result))) = ev {
            if self.phase == 2 {
                if *result == DeleteResult::Deleted { self.alert.dismiss(); fx.push(Fx::Nav(NavOp::Dismiss(entry))); }
                else { self.fail(*result); }
            }
            return true;
        }
        if !self.visible() { return false; }
        match ev {
            ScreenEvent::Tick(tick) => self.alert.update(tick.dt()),
            ScreenEvent::FocusMoved { to, .. } => self.alert.set_choice(if to.elem == DELETE { Choice::Destructive } else { Choice::Cancel }),
            ScreenEvent::Input(input) if matches!(input.kind, InputKind::Key { key: Key::Back, edge: Edge::Down, .. }) => {
                self.alert.dismiss(); fx.push(Fx::Nav(NavOp::Dismiss(entry)));
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key { key: Key::Up | Key::Down, edge: Edge::Down | Edge::Repeat, .. } = input.kind {
                    let delta = if matches!(input.kind, InputKind::Key { key: Key::Up, .. }) { -1 } else { 1 };
                    self.alert.scroll_by(cx.measure, delta);
                } else {
                    return false; // arrows and OK belong to the shared focus/press engine
                }
            }
            ScreenEvent::Activate(_) => {}, // pointer commits only through the shared press machine
            ScreenEvent::PressCommit(_) => {
                if !self.alert.is_open() { return true; }
                let elem = cx.focus.current.map_or(CANCEL, |k| k.elem);
                if elem == DELETE && self.phase == 1 {
                    let Some(req) = self.request.clone() else { return true };
                    let inputs = H::screen_inputs(cx);
                    if inputs.profile.generation != req.profile || !inputs.deletable.contains(&(req.sid, req.key)) {
                        self.fail(DeleteResult::IdentityChanged);
                    } else {
                        self.phase = 2;
                        self.alert.open_card(msg::browse_delete_working_c(), vec![Cow::Borrowed(msg::browse_delete_wait())], Answers::One);
                        fx.push(Fx::App(AppFx::MediaDelete(req)));
                    }
                } else if elem == CANCEL {
                    self.alert.dismiss(); fx.push(Fx::Nav(NavOp::Dismiss(entry)));
                }
            }
            _ => {},
        }
        true
    }
    pub fn draw<H: AppLike>(&mut self, entry: EntryId, f: &mut DrawFrame<'_, '_, H>) {
        self.alert.draw_scrim(f);
        let cancel = if self.phase == 1 { msg::settings_cancel_c() } else { msg::settings_login_close_c() };
        self.alert.draw(cancel, msg::settings_delete_c(), f.measure, f.route_navigation(), f.press);
        if self.alert.is_open() && self.alert.settled() {
            for elem in [CANCEL, DELETE] {
                let Some(placed) = self.place(elem, f.cx) else { continue };
                f.stop(plx_ui::Painter::root(), Stop { key: FocusKey { entry, elem }, rect: placed.rect,
                    rest_rect: placed.rest_rect, clip: Rect::FULL, hover: Hover::Focus, activate: Activate::Press });
            }
        }
    }
    pub fn write(&self, c: &mut Canon) {
        c.u32(self.phase).str(&self.title).bool(self.alert.is_open()).u32(self.alert.choice() as u32).u32(self.alert.scroll_target_bits());
        c.option(self.result.as_ref(), |c, result| { c.u32(*result as u32); });
        c.option(self.request.as_ref(), |c, req| { c.u32(req.sid.raw() as u32).str(&req.rk).u32(req.kind as u32).str(&req.show_rk).u32(req.profile).u32(req.key.instance).u32(req.key.token).u32(req.key.grant); });
    }
}
