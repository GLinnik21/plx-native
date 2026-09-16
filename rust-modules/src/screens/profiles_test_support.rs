//! Shared fixtures and helpers for the `screens::profiles` test modules split out below.

use super::*;
pub(super) use crate::ui::fixture::FixtureMeasure;
pub(super) use crate::ui::machine::{
    Edge, FocusKey, FocusRead, InputEvent, InputKind, InputOwner, InstanceId, MachineId,
    PressRead, Source, Stamped, Tick,
};
pub(super) use crate::ui::present::Present;
pub(super) use std::sync::LazyLock;

pub(super) struct SessionHost;

impl crate::ui::machine::Host for SessionHost {
    type Arg = super::super::family::SettingsPage;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = auth::SessionRead<'a>;
    type Init = super::super::family::NoInit;
    type Memory = ();
}

impl AuthLike for SessionHost {
    fn auth<'a>(cx: &Cx<'a, Self>) -> auth::SessionRead<'a> {
        cx.views
    }
}

pub(super) fn snapshot(phase: Phase, users: Vec<auth::UserTile>) -> auth::owner::SessionSnapshot {
    auth::owner::SessionSnapshot {
        flow_epoch: 0,
        phase,
        qr_generation: 0,
        code: Arc::from(""),
        png: Arc::from(Vec::<u8>::new()),
        code_replaced: false,
        users: Arc::from(users),
        error: Arc::from(""),
        pin_denied: false,
        profile: None,
        scope: auth::owner::ProfileScope(0),
        delete_leftovers: 0,
    }
}

pub(super) fn snapshot_at(
    flow_epoch: u64,
    phase: Phase,
    users: Vec<auth::UserTile>,
) -> auth::owner::SessionSnapshot {
    let mut snapshot = snapshot(phase, users);
    snapshot.flow_epoch = flow_epoch;
    snapshot
}

pub(super) static EMPTY_SNAPSHOT: LazyLock<auth::owner::SessionSnapshot> =
    LazyLock::new(|| snapshot(Phase::Profiles, Vec::new()));

pub(super) static MEASURE: FixtureMeasure = FixtureMeasure;

pub(super) fn cx_with<'a>(
    focus: Option<FocusKey<u32>>,
    snapshot: &'a auth::owner::SessionSnapshot,
) -> Cx<'a, SessionHost> {
    Cx {
        views: snapshot.read(),
        tick: Tick::default(),
        measure: &MEASURE,
        press: PressRead::default(),
        focus: FocusRead {
            current: focus,
            ..Default::default()
        },
        owner: InputOwner::Entry(EntryId(0)),
    }
}

pub(super) fn cx(focus: Option<FocusKey<u32>>) -> Cx<'static, SessionHost> {
    cx_with(focus, &EMPTY_SNAPSHOT)
}

/// A screen assembled without a constructor publication, for pure pad geometry/state tests.
/// Live read/command tests use the explicit local [`SessionHost`] above.
pub(super) fn bare(pad: Pad) -> ProfilesScreen {
    ProfilesScreen {
        entry: EntryId(0),
        row: card_row::CardRow::new(),
        row_sty: card_row::RowStyle::PROFILES,
        footer_pop: CtlPop::new(),
        spin_ms: 0.0,
        spin_phase: crate::ui::motion::Phase::default(),
        ground: RouteGround::new(),
        pad,
        users: Arc::from(Vec::<auth::UserTile>::new()),
        phase: Phase::Profiles,
        error: Arc::from(""),
        pin_denied: false,
        flow_epoch: 0,
        next_correlation: Some(1),
        pending_selection: None,
        state: ProfilesState {
            roster_n: 0,
            roster: Arc::from(Vec::<auth::UserTile>::new()),
            flow_epoch: 0,
            phase: phase_disc(Phase::Profiles),
            pin_denied: false,
            pad_open: false,
            pad_target: 0,
            pad_len: 0,
            pad_submitting: false,
            pad_flashing: false,
            next_correlation: Some(1),
            selection_correlation: None,
            selection_epoch: None,
        },
    }
}

pub(super) fn step_ev(
    s: &mut ProfilesScreen,
    ev: &ScreenEvent<SessionHost>,
    focus: Option<FocusKey<u32>>,
) -> (Handled, Vec<Stamped<SessionHost>>) {
    step_ev_with(s, ev, focus, &EMPTY_SNAPSHOT, InstanceId(0))
}

pub(super) fn step_ev_with(
    s: &mut ProfilesScreen,
    ev: &ScreenEvent<SessionHost>,
    focus: Option<FocusKey<u32>>,
    snapshot: &auth::owner::SessionSnapshot,
    instance: InstanceId,
) -> (Handled, Vec<Stamped<SessionHost>>) {
    let c = cx_with(focus, snapshot);
    let mut present = Present::new();
    let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
    let handled = {
        let mut fx = Effects::new(&mut buf, MachineId::Instance(instance), &mut present);
        Machine::<SessionHost>::step(s, ev, &c, &mut fx)
    };
    (handled, buf)
}

pub(super) fn key_down(key: Key, sym: u32, wcode: u32) -> ScreenEvent<SessionHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Script,
        kind: InputKind::Key {
            key,
            sym,
            wcode,
            edge: Edge::Down,
            at_edge: false,
        },
    })
}

pub(super) fn click(hit: Option<u32>) -> ScreenEvent<SessionHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Script,
        kind: InputKind::Click {
            x: 0.0,
            y: 0.0,
            hit,
        },
    })
}

pub(super) fn commit_avatar(s: &mut ProfilesScreen, index: u32, read: &auth::owner::SessionSnapshot) -> Vec<Stamped<SessionHost>> {
    let entry = s.entry;
    step_ev_with(s, &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(FocusKey { entry, elem: index }), read, InstanceId(17)).1
}

pub(super) fn accept_selection(s: &mut ProfilesScreen, correlation: u32, epoch: u64, read: &auth::owner::SessionSnapshot) {
    step_ev_with(s, &ScreenEvent::Async(crate::ui::machine::RequestId(correlation),
        AppMsg::SelectionReply { correlation, accepted: true, flow_epoch: epoch }),
        None, read, InstanceId(17));
}

pub(super) fn user(title: &str, protected: bool) -> auth::UserTile {
    auth::UserTile {
        title: title.into(),
        protected,
        uuid: format!("{title}-uuid"),
        ..Default::default()
    }
}

pub(super) fn submit_locked(
    screen: &mut ProfilesScreen,
    index: usize,
    instance: InstanceId,
) -> Vec<Stamped<SessionHost>> {
    let mut effects = Vec::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(&mut effects, MachineId::Instance(instance), &mut present);
        screen.select(index, &mut fx);
        for digit in b"1234" {
            screen.press(*digit, &mut fx);
        }
    }
    effects
}
