//! **The player's four overlays, as entries on the page's own `ModalStack`** (restructure spec §6.2
//! "Page-owned panels … Player: its four overlays", §9, phase 9).
//!
//! The track menu, the Info card, the Chapters strip and the `…` options popover were four
//! `Route::Player { overlay }` values driven by four arms of `app/run.rs`'s key ladder, over four
//! modules' worth of `static mut`. They are now four `AppArg` variants presented on the player
//! page's stack, mounted by the one `Mounter`, styled `PlayerPanel { survives_failure }`, and each
//! owning its panel's state as a field. The container owns the PHASE and the appear spring; the
//! surface owns input while it is `Opening | Open`.
//!
//! **One screen type with an inner enum, not four types**, because the four differ only in which
//! panel they hold and share every rule that matters here: what a transport key does, how a held
//! direction is paced, when a panel dismisses itself, and how a decision reaches the loop. Four
//! copies of those rules is exactly the drift `overlay_swallows_key` was written to end.
//!
//! **The transport-key rule is the one behaviour a reader must not lose.** A viewer holding the
//! track menu, the Info card or the Chapters strip open still expects PAUSE/PLAY to work, and the
//! panel to stay up (issue 28). While the ladder owned these keys that was expressed by
//! `overlay_swallows_key` answering `false` for exactly those three keys so the press FELL THROUGH
//! to the ordinary transport arms. A surface cannot fall through — the dispatcher hands it the key
//! and the ladder never sees it — so it FORWARDS instead: `PlayerReq::Transport`, which the loop
//! spends on the same toggle, leaving this panel untouched. `More` keeps the old
//! swallow-everything answer for the same reason it always did.
//!
//! **The `Focusable` half is real now (restructure phase 12, D2).** Each panel's own
//! `*Part` wrapper (`ui::track_menu::TrackMenuPart`, `ui::chapters_panel::ChaptersPart`,
//! `ui::info_panel::InfoPanelPart`, `ui::more_menu::MoreMenuPart`) answers the Engine's query
//! protocol (§7.1) over that panel's real row geometry, and [`PlayerOverlayScreen::step`] no
//! longer moves focus BY HAND: a direction falls through (`Handled::No`) to the engine's own
//! `neighbour`/`EdgeRule` unless this panel's own cadence gate is still waiting
//! ([`PANEL_REPEAT_MS`]) or the engine has just reached this panel's group EDGE and re-delivered
//! the key (`edge_key`, the one place a panel still decides something outside its own scope — a
//! tab switch, or dropping focus onto the HUD tabs below). OK is answered the same way: the
//! engine's own `Activate`/press machinery (§7.4) fires `ScreenEvent::Activate`/`PressCommit`,
//! which [`PlayerOverlayScreen::activate`] spends. A click resolves against the real per-row hit
//! map [`PlayerOverlayScreen::draw`] registers, not a hand-rolled pixel scan.

use std::borrow::Cow;
use std::os::raw::c_int;

use crate::screens::registry::{AppFx, AppLike, PlayerReq};
use crate::ui::chapters_panel::ChaptersPart;
use crate::ui::consts;
use crate::ui::frame::Budget;
use crate::ui::info_panel::InfoPanelPart;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, LogicalState,
    Machine, NavOp,
};
use crate::ui::more_menu::MoreMenuPart;
use crate::ui::screen::{
    At, Dir, DrawFrame, FocusSource, Focusable, GroupSpec, HitSource, Part, Placed, RenderStrategy,
    Screen, ScreenEvent, Step,
};
use crate::ui::track_menu::TrackMenuPart;
use crate::ui::Rect;

use super::input::{HUD_LINGER_MS, HUD_MENU_MS};
use crate::screens::registry::{RepeatGate, PANEL_REPEAT_MS};

/// The fields [`PlayerOverlayScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
/// The selected ROW is in it deliberately: these panels' UP/DOWN changes nothing else in the app,
/// so without it a replay grades a panel opening and closing and nothing between.
pub(crate) const SHAPE: &str = "PlayerOverlayScreen{kind:str,sel:u32}";

/// Which panel a [`PlayerOverlayArg`] names, and — since the argument is what the container holds
/// for the whole life of the entry — the identity `ScreenArg::same_instance` compares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    /// The audio/subtitle picker (`ui/track_menu.rs`). `tab` is the panel it opens on: the two
    /// on-screen discs open it directly on theirs.
    Tracks { tab: c_int },
    /// The now-playing Info card (`ui/info_panel.rs`).
    Info,
    /// The chapter strip (`ui/chapters_panel.rs`).
    Chapters,
    /// The `…` overflow popover (`ui/more_menu.rs`). `quality` opens it focused on the active
    /// quality rung — the failure read-out's recovery path, which is also why this is the one
    /// overlay whose style `survives_failure`.
    More { quality: bool },
}

impl OverlayKind {
    /// The heartbeat's `overlay=` word. These four spellings are the ones `tests/manifest.json`'s
    /// fps scenes select by; changing one silently disarms a scene rather than failing anything
    /// visible (§15.3). Since phase 10 item 4 they reach the heartbeat DIRECTLY — `app::overlay_word`
    /// is the topmost surface's own `Screen::name`, so there is no mapping table between this
    /// function and the printed line, and `app::heartbeat_word_tests` derives its alphabet by
    /// presenting every surface argument and reading the word back.
    /// **WHICH of the four this is, with the parameter thrown away** — the identity
    /// `ScreenArg::same_instance` compares, and the reason it is not the whole `OverlayKind`.
    ///
    /// The two on-screen discs open the SAME track menu on their own tab, and the failure
    /// read-out opens the SAME `…` popover focused on the quality rung. So `tab` and `quality` are
    /// a boot ADDRESS, not an identity — exactly as `AppArg::Settings`'s root page is — and a
    /// container that compared them would stack a second track menu on top of the first when the
    /// viewer moved from Subtitles to Audio.
    pub(crate) fn slot(self) -> u8 {
        match self {
            OverlayKind::Tracks { .. } => 0,
            OverlayKind::Info => 1,
            OverlayKind::Chapters => 2,
            OverlayKind::More { .. } => 3,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            OverlayKind::Tracks { .. } => "menu",
            OverlayKind::Info => "info",
            OverlayKind::Chapters => "chapters",
            OverlayKind::More { .. } => "more",
        }
    }

    /// **Does this panel stay up over the terminal failure read-out?**
    ///
    /// Only the `…` popover, and the reason is the read-out's own escape: OK on a failed playback
    /// opens the shared quality ladder, so that panel must remain visible and drivable over the
    /// black failure ground. The other three are stale content about a stream that is not playing
    /// and are gone with the transport — the same rule `app/run.rs`'s `panels` guard applied when
    /// they were routes.
    pub(crate) fn survives_failure(self) -> bool {
        matches!(self, OverlayKind::More { .. })
    }

    /// **Does this panel swallow a TRANSPORT key rather than letting it reach the toggle?**
    ///
    /// The successor of `app::playback::overlay_swallows_key`'s `key` term, and the same answer:
    /// the three modal panels let PAUSE/PLAY/PLAYPAUSE through and keep themselves up; `More` is
    /// deliberately excluded from that exception — it was reported and reproduced against
    /// Info/Chapters/Tracks, and its own arm never consulted the predicate at all.
    pub(crate) fn swallows_transport(self) -> bool {
        matches!(self, OverlayKind::More { .. })
    }
}

/// What the container is asked to present. The `host` is the player instance the panel reports back
/// to; it rides on the argument rather than being looked up, for `LibraryMenuArg`'s reason — the
/// entry outlives any one frame's idea of which page is on top.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PlayerOverlayArg {
    pub(crate) kind: OverlayKind,
}

impl LogicalState for PlayerOverlayArg {
    fn write(&self, c: &mut Canon) {
        match self.kind {
            OverlayKind::Tracks { tab } => {
                c.u32(0).u32(tab as u32);
            }
            OverlayKind::Info => {
                c.u32(1);
            }
            OverlayKind::Chapters => {
                c.u32(2);
            }
            OverlayKind::More { quality } => {
                c.u32(3).bool(quality);
            }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(self.kind.word());
    }
}

/// The panel itself — the state four modules kept in `static mut`s until phase 9.
pub(crate) enum Panel {
    Tracks(crate::ui::track_menu::TrackMenuState),
    Info(crate::ui::info_panel::InfoPanelState),
    Chapters(crate::ui::chapters_panel::ChaptersState),
    More(crate::ui::more_menu::MoreMenuState),
}

pub(crate) struct PlayerOverlayScreen {
    entry: EntryId,
    kind: OverlayKind,
    panel: Panel,
    /// The cadence a HELD direction walks this panel's list at. The loop's own client-side repeat
    /// timer used to do this at 110 ms for exactly these four panels; the dispatcher delivers the
    /// hardware's ~50 ms `Edge::Repeat` instead, so the surface applies the cadence itself
    /// ([`PANEL_REPEAT_MS`]).
    repeat: RepeatGate,
}

impl PlayerOverlayScreen {
    /// Every panel here answers on exactly one focus group — see each `*Part`'s own `groups()`
    /// (restructure phase 12); this screen never holds more than one panel at a time, so there is
    /// no second id to reserve.
    const GROUP: GroupId = GroupId(0);

    pub(crate) fn new(ps: &crate::route::PlaybackSession, entry: EntryId, kind: OverlayKind) -> Self {
        let panel = match kind {
            OverlayKind::Tracks { tab } => {
                Panel::Tracks(crate::ui::track_menu::TrackMenuState::new(ps, tab))
            }
            OverlayKind::Info => Panel::Info(crate::ui::info_panel::InfoPanelState::new()),
            OverlayKind::Chapters => {
                Panel::Chapters(crate::ui::chapters_panel::ChaptersState::new())
            }
            OverlayKind::More { quality: false } => {
                Panel::More(crate::ui::more_menu::MoreMenuState::new(ps))
            }
            OverlayKind::More { quality: true } => {
                Panel::More(crate::ui::more_menu::MoreMenuState::new_quality(ps))
            }
        };
        Self {
            entry,
            kind,
            panel,
            repeat: RepeatGate::IDLE,
        }
    }

    pub(crate) fn kind(&self) -> OverlayKind {
        self.kind
    }

    /// **Re-address a panel that is already up.** The two disc icons open the one track menu on
    /// their own tab, so the second press has to move the tab of the entry that exists rather than
    /// present a second one — `same_instance` says they ARE the same instance, and this is the
    /// other half of that: what "the same instance, at a different address" does.
    pub(crate) fn retarget(&mut self, ps: &crate::route::PlaybackSession, kind: OverlayKind) {
        if kind.slot() != self.kind.slot() {
            return;
        }
        self.kind = kind;
        match (&mut self.panel, kind) {
            (Panel::Tracks(p), OverlayKind::Tracks { tab }) => p.focus_tab(ps, tab),
            _ => {}
        }
    }

    pub(crate) fn panel(&self) -> &Panel {
        &self.panel
    }

    /// The highlighted row, for the focus probe — a READ of the cursor a key press moves, and the
    /// reason it exists: these panels' UP/DOWN changes nothing else, so without it the fingerprint
    /// records a panel opening and closing and nothing between.
    pub(crate) fn sel(&self) -> i32 {
        match &self.panel {
            Panel::Tracks(p) => p.sel(),
            Panel::Info(p) => p.sel(),
            Panel::Chapters(p) => p.sel(),
            Panel::More(p) => p.sel(),
        }
    }

    /// **The headless track pick** (`/tmp/plxnative-menupick=<tab>,<row>`): seat the cursor on
    /// `row` and confirm it, exactly as a viewer's DOWN…DOWN…OK would. It is a method rather than
    /// two calls at the trigger's site because the panel's state is an INSTANCE now — the trigger
    /// presents the surface and the body is mounted at nav commit, a frame later — so the loop
    /// reaches the cursor through the container or not at all.
    ///
    /// Dismissing afterwards is the caller's business, and deliberately is not done here: the
    /// trigger exists to leave the chosen track's panel on screen for a capture.
    pub(crate) fn pick_track_row(
        &mut self,
        ps: &crate::route::PlaybackSession,
        row: c_int,
    ) -> Option<crate::ui::track_menu::TrackCommit> {
        if let Panel::Tracks(p) = &mut self.panel {
            p.focus_row(row);
            return p.on_ok(ps);
        }
        None
    }

    /// **The Info card's DEFERRED press, read back on the spring-back.** Its two actions are
    /// control faces with a pop of their own, so its OK arm only asks the loop for
    /// [`PlayerReq::ArmInfoPress`]; the loop's press machine commits one frame later, and this is
    /// how it asks the panel what the press meant. `None` for the other three, whose OK acts at
    /// once — asking any of them is a caller confusion rather than a state, so it cannot be a
    /// silent no-op that returns an action.
    pub(crate) fn info_press_action(&mut self) -> Option<crate::ui::info_panel::InfoAction> {
        match &mut self.panel {
            Panel::Info(p) => Some(p.on_ok()),
            _ => None,
        }
    }

    fn ask<H: AppLike>(fx: &mut Effects<'_, H>, req: PlayerReq) {
        fx.push(Fx::App(AppFx::Player(req)));
    }

    fn dismiss<H: AppLike>(&self, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// A direction moved this panel's cursor: the transport stays up for a MENU's read time, not
    /// the plain linger.
    fn moved<H: AppLike>(&self, fx: &mut Effects<'_, H>) {
        Self::ask(fx, PlayerReq::ExtendHud(HUD_MENU_MS));
    }

    /// This panel is going away by a viewer's own press: hand the transport the ordinary linger.
    fn closing<H: AppLike>(&self, fx: &mut Effects<'_, H>) {
        Self::ask(fx, PlayerReq::ExtendHud(HUD_LINGER_MS));
    }

    /// **Apply whatever the focused element decided, at once** (§7.4). Reached from a `Bare` row's
    /// (`Tracks`/`More`) key-down/pointer-click `Activate`, from `Chapters`'s (`Card`) `PressCommit`
    /// on release, and — for `Info` (`Control`) only — from a POINTER click's `Activate` rather
    /// than its `PressCommit` (see [`Self::key`]'s caller: a mouse click is already a precise,
    /// instantaneous gesture with nothing to animate, unlike a keyboard OK's deferred dip). Either
    /// way the panel's own cursor is already correct — every `FocusMoved` this screen sees writes
    /// it back (`step`'s own arm below) — so this reads the panel's OWN `on_ok`, exactly as the
    /// old ladder's `Key::Ok` arms did.
    fn activate<H: AppLike>(&mut self, ps: &crate::route::PlaybackSession, fx: &mut Effects<'_, H>) {
        match &mut self.panel {
            Panel::Tracks(p) => {
                if let Some(commit) = p.on_ok(ps) {
                    fx.push(Fx::App(AppFx::Player(PlayerReq::CommitTrack(commit))));
                }
                self.dismiss(fx);
                self.closing(fx);
            }
            Panel::More(p) => {
                let action = p.on_ok();
                self.dismiss(fx);
                Self::ask(fx, PlayerReq::More(action));
                self.closing(fx);
            }
            Panel::Info(p) => {
                let action = p.on_ok();
                self.dismiss(fx);
                Self::ask(fx, PlayerReq::Info(action));
                self.closing(fx);
            }
            Panel::Chapters(p) => {
                let ns = p.on_ok();
                self.dismiss(fx);
                if ns >= 0 {
                    Self::ask(fx, PlayerReq::SeekTo(ns));
                }
                self.closing(fx);
            }
        }
    }

    /// **The engine reached this panel's group EDGE and re-delivered the direction**
    /// (`EdgeRule::Screen`, §7.3 step 3) — the one thing left that a panel decides outside its own
    /// scope, because it moves focus OFF this screen (Chapters'/Info's DOWN) or re-addresses the
    /// panel entirely (Tracks' LEFT/RIGHT tab switch, `TrackMenuState::focus_tab`). More declares
    /// no `Screen` edge at all (its four sides are `Stop`), so it never reaches here.
    fn edge_key<H: AppLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        key: consts::Key,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        use consts::Key;
        match (&mut self.panel, key) {
            (Panel::Tracks(p), Key::Left { .. } | Key::Right { .. }) => {
                p.focus_tab(ps, if matches!(key, Key::Left { .. }) { 0 } else { 1 });
                self.moved(fx);
            }
            (Panel::Chapters(_), Key::Down) | (Panel::Info(_), Key::Down) => {
                self.dismiss(fx);
                Self::ask(fx, PlayerReq::FocusTabs);
                self.closing(fx);
            }
            _ => {} // no other panel declares any other edge escape
        }
        Handled::Yes
    }

    /// The one key ladder these four share, for everything the ENGINE does not already resolve
    /// (§7.3 step 1). Transport keys always fall through here first (module doc) — the only keys
    /// this ladder still fully owns. A direction is paced by [`PANEL_REPEAT_MS`] and then handed
    /// to the engine's own `neighbour`/`EdgeRule` (`Handled::No`) unless the engine has already
    /// reached this panel's group edge and re-delivered it (`edge_key`, above). OK is likewise
    /// left to the engine's own `Activate`/press machinery (§7.4; see [`Self::activate`]). BACK
    /// dismisses — Tracks and More close silently, exactly as the old ladder did; Info and
    /// Chapters also hand the transport the ordinary linger.
    fn key<H: AppLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        key: consts::Key,
        edge: Edge,
        at_edge: bool,
        now: u32,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        use consts::Key;
        // Transport first, above every panel's own arms: this is the rule that outranks modality.
        if matches!(key, Key::Pause | Key::Play | Key::PlayPause) {
            if self.kind.swallows_transport() {
                return Handled::Yes;
            }
            if edge == Edge::Down {
                Self::ask(
                    fx,
                    PlayerReq::Transport(match key {
                        Key::Play => Some(true),
                        Key::Pause => Some(false),
                        _ => None,
                    }),
                );
            }
            return Handled::Yes;
        }
        // A held direction is paced; a FRESH press is never swallowed by the cadence before it.
        let directional = matches!(key, Key::Up | Key::Down | Key::Left { .. } | Key::Right { .. });
        if directional {
            match edge {
                Edge::Down => self.repeat.rearm(now),
                Edge::Repeat if !self.repeat.ready_every(now, PANEL_REPEAT_MS) => {
                    return Handled::Yes
                }
                Edge::Up => return Handled::Yes,
                _ => {}
            }
            if at_edge {
                return self.edge_key(ps, key, fx);
            }
            // an interior move: let the engine's own `neighbour`/`EdgeRule` answer it (§7.3 steps
            // 2-3) rather than moving the panel's cursor by hand.
            return Handled::No;
        }
        if edge != Edge::Down {
            return Handled::Yes;
        }
        match key {
            Key::Back => {
                self.dismiss(fx);
                if matches!(self.panel, Panel::Info(_) | Panel::Chapters(_)) {
                    self.closing(fx);
                }
                Handled::Yes
            }
            // Not consumed here: the engine's own `Activate`/`PressArm` machinery answers OK by
            // the focused element's `ElemKind` (§7.4), and `Machine::step`'s `Activate`/
            // `PressCommit` arms below spend what it decided (`Self::activate`).
            Key::Ok => Handled::No,
            _ => Handled::Yes,
        }
    }
}

impl<H: crate::screens::registry::PlayerLike> Machine<H> for PlayerOverlayScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // The frame's publication of the playback session (spec §2.3): a screen reads the Player
        // machine's decisions here and asks for a change through an effect, never by writing them.
        let ps = H::session(cx);
        match ev {
            ScreenEvent::Input(input) => {
                // The dispatcher's `machine::Key` is the four directions plus OK/BACK; the
                // transport alphabet this ladder turns on lives in `consts::Key`, so the raw
                // pair is classified here exactly as `app/input.rs` classifies it.
                if let InputKind::Key {
                    sym, wcode, edge, at_edge, ..
                } = input.kind
                {
                    return self.key(ps, consts::classify(sym, wcode), edge, at_edge, input.at.ms, fx);
                }
                // **An open panel owns the click and closes on it.** Four `modal_of` arms of
                // the loop's pointer path said this — "the transport is partly hidden while a
                // panel is up, so its rects must not be consulted" — and it is one answer here.
                // `More`'s rows are ACTIONS, so a click that lands on one COMMITS it first and a
                // click outside reports `None`; the other three simply dismiss.
                //
                // The dispatcher hands a `Click`/`Pointer` to the owner whatever its
                // `hit_source()` is. (This comment said `HitSource::Legacy`; this surface has
                // answered `HitSource::Engine` since phase 12's contract freeze, and the property
                // it relies on was never the Legacy answer but the delivery.)
                if let InputKind::Click { .. } | InputKind::Pointer { .. } = input.kind {
                    // The Engine's own hit map now resolves every row (`Focusable::place` below,
                    // `DrawFrame::stop` registered in `draw`): a hit delivers `Activate` directly
                    // (§7.5-7.6, spent by `Self::activate` the same as a key OK), a hover parks
                    // focus through the engine, and a miss reaches `Style::PlayerPanel`'s own
                    // `OnMiss::Dismiss` (`ui/containers/modal.rs`) — this arm no longer scans
                    // pixels or a panel's own cursor by hand.
                    return Handled::No;
                }
                Handled::No
            }
            ScreenEvent::FocusMoved { to, .. } => {
                // §7.3 step 5: the owner's `step` is the only place that mutates in response to a
                // move the engine made — write the new cursor back into whichever panel is open,
                // and extend the transport's read time exactly as a hand-moved cursor used to.
                let i = to.elem as i32;
                match &mut self.panel {
                    Panel::Tracks(p) => p.set_sel(i),
                    Panel::Info(p) => p.set_focus(i),
                    Panel::Chapters(p) => p.set_sel(i),
                    Panel::More(p) => p.set_sel(i),
                }
                self.moved(fx);
                Handled::No
            }
            // A pointer click on ANY row (`Activate::Immediate`/`Direct`, every `*Part::draw`
            // above) always applies at once. A keyboard OK on a `Control`/`Card` row
            // (`Info`/`Chapters`) arms an engine press first and only reaches here as
            // `PressCommit`, on release — except `Info`, whose `PressCommit` defers instead to
            // the loop's own tvOS dip (`Self::activate`'s doc explains the split).
            ScreenEvent::Activate(_) => {
                self.activate(ps, fx);
                Handled::No
            }
            ScreenEvent::PressCommit(_) => {
                if let Panel::Info(_) = &self.panel {
                    Self::ask(fx, PlayerReq::ArmInfoPress);
                } else {
                    self.activate(ps, fx);
                }
                Handled::No
            }
            ScreenEvent::Tick(tick) => {
                let dt = tick.dt();
                match &mut self.panel {
                    Panel::Tracks(p) => p.update(dt),
                    Panel::Info(p) => p.update(dt),
                    Panel::Chapters(p) => p.update(dt),
                    Panel::More(p) => p.update(dt),
                }
                // The transport must not auto-hide out from under a panel a viewer is reading —
                // the rule `app/run.rs` kept as "keep the HUD alive while the track menu / Info
                // card / Chapters strip is open", stated once here by the surface that IS open.
                Self::ask(fx, PlayerReq::ExtendHud(HUD_LINGER_MS));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

/// **Real per-row groups, delegated to whichever panel is ACTIVE** (restructure phase 12, D2 —
/// see `screens/player/mod.rs`'s own `Focusable` impl for the sibling case). Each panel exposes
/// its own row geometry through a `*Part` wrapper (`ui::track_menu::TrackMenuPart`,
/// `ui::chapters_panel::ChaptersPart`, `ui::info_panel::InfoPanelPart`,
/// `ui::more_menu::MoreMenuPart`), built fresh per query over a SHARED reference to the panel's
/// own state — every method here is `&self`, and so is every method on this screen's own
/// `Focusable` impl (§7.1: "the engine never mutates a screen"), which is why each wrapper's
/// `state` field is `&'a StateType` rather than `&'a mut` (see each wrapper's own doc). This
/// screen holds exactly one panel at a time, so there is no `Composed`/`layout()` here — that
/// trait concatenates SEVERAL simultaneous parts, and these four never coexist.
impl<H: crate::screens::registry::PlayerLike> Focusable<H> for PlayerOverlayScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
        }
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
        }
    }
}

impl LogicalState for PlayerOverlayScreen {
    fn write(&self, c: &mut Canon) {
        c.str(self.kind.word());
        c.u32(self.sel() as u32);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(self.kind.word());
    }
}

impl<H: crate::screens::registry::PlayerLike> Screen<H> for PlayerOverlayScreen {
    fn name(&self) -> &'static str {
        self.kind.word()
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let ps = H::session(f.cx);
        // Stale content panels are gone with the transport when a playback has FAILED; the `…`
        // popover is the deliberate exception, because the read-out opened it as its own recovery
        // path (`OverlayKind::survives_failure`).
        if crate::ui::player_hud::transport_hidden(ps) && !self.kind.survives_failure() {
            return;
        }
        // The container owns the appear spring; `DrawFrame::page_alpha` IS `Surface::motion.appear`
        // for a surface, which is what the panels' own `Popover` used to hold.
        let appear = f.page_alpha;
        let measure = f.measure;
        match &mut self.panel {
            Panel::Tracks(p) => p.draw(appear, measure),
            Panel::Info(p) => p.draw(ps, appear, measure),
            Panel::Chapters(p) => p.draw(ps, appear, measure),
            Panel::More(p) => p.draw(appear, measure),
        }
        // Every visible row registers its own stop now (§7.6) — the same per-row geometry
        // `Focusable::place` (above) answers for focus movement, so the hit map and the focus
        // engine agree on every rect. Each panel's own `*Part::draw` does the registration (it
        // needs the module-private row geometry this screen cannot reach directly); the actual
        // paint already happened above, on the owned, mutable `Panel`.
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
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
