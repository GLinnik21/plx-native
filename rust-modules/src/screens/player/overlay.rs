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

use std::borrow::Cow;
use std::os::raw::c_int;

use crate::screens::registry::{AppFx, AppLike, PageMemory, PlayerReq};
use crate::ui::consts::{self, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_UP};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, LogicalState,
    Machine, NavOp,
};
use crate::ui::screen::{
    At, Dir, DrawFrame, FocusSource, Focusable, GroupSpec, HitSource, Placed, RenderStrategy,
    Screen, ScreenEvent, Step,
};

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

    /// The one key ladder these four share. Returns `Handled::Yes` for everything except a
    /// transport key on a panel that lets it through — those are FORWARDED, which is the surface's
    /// version of the old fall-through (module doc).
    fn key<H: AppLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        key: consts::Key,
        sym: u32,
        edge: Edge,
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
        // A held direction is paced; a FRESH press is never swallowed by the press before it.
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
        } else if edge != Edge::Down {
            return Handled::Yes;
        }
        let dir_sym = match key {
            Key::Up => SDLK_UP,
            Key::Down => SDLK_DOWN,
            Key::Left { .. } => SDLK_LEFT,
            Key::Right { .. } => SDLK_RIGHT,
            _ => sym,
        } as c_int;
        match &mut self.panel {
            Panel::Tracks(p) => match key {
                Key::Up | Key::Down | Key::Left { .. } | Key::Right { .. } => {
                    p.move_focus(ps, dir_sym);
                    self.moved(fx);
                }
                Key::Ok => {
                    if let Some(commit) = p.on_ok(ps) {
                        fx.push(Fx::App(AppFx::Player(PlayerReq::CommitTrack(commit))));
                    }
                    self.dismiss(fx);
                    self.closing(fx);
                }
                Key::Back => self.dismiss(fx),
                _ => {}
            },
            Panel::More(p) => match key {
                Key::Up | Key::Down => {
                    p.move_focus(dir_sym);
                    self.moved(fx);
                }
                Key::Ok => {
                    let action = p.on_ok();
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::More(action));
                    self.closing(fx);
                }
                Key::Back => self.dismiss(fx),
                // ONE column, so LEFT/RIGHT are swallowed without moving anything rather than
                // falling through to the scrubber.
                _ => {}
            },
            Panel::Info(p) => match key {
                // past the bottom of the card → drop focus back onto the tabs
                Key::Down if p.at_last() => {
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::FocusTabs);
                    self.closing(fx);
                }
                Key::Up | Key::Down => {
                    p.move_focus(dir_sym);
                    self.moved(fx);
                }
                // The card's two actions are control faces with a pop of their own, so OK takes
                // the tvOS press and the loop spends it on the spring-back. The card stays up
                // through the dip, so the whole animation is on screen.
                Key::Ok if p.focus_is_ctl() => Self::ask(fx, PlayerReq::ArmInfoPress),
                Key::Ok => {
                    let action = p.on_ok();
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::Info(action));
                    self.closing(fx);
                }
                Key::Back => {
                    self.dismiss(fx);
                    self.closing(fx);
                }
                _ => {}
            },
            Panel::Chapters(p) => match key {
                Key::Left { .. } | Key::Right { .. } => {
                    p.move_focus(dir_sym);
                    self.moved(fx);
                }
                Key::Ok => {
                    let ns = p.on_ok();
                    self.dismiss(fx);
                    if ns >= 0 {
                        Self::ask(fx, PlayerReq::SeekTo(ns));
                    }
                    self.closing(fx);
                }
                // drop focus back onto the tabs below the strip
                Key::Down => {
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::FocusTabs);
                    self.closing(fx);
                }
                Key::Back => {
                    self.dismiss(fx);
                    self.closing(fx);
                }
                _ => {}
            },
        }
        Handled::Yes
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
                    sym, wcode, edge, ..
                } = input.kind
                {
                    return self.key(ps, consts::classify(sym, wcode), sym, edge, input.at.ms, fx);
                }
                // **An open panel owns the click and closes on it.** Four `modal_of` arms of
                // the loop's pointer path said this — "the transport is partly hidden while a
                // panel is up, so its rects must not be consulted" — and it is one answer here.
                // `More`'s rows are ACTIONS, so a click that lands on one COMMITS it first and a
                // click outside reports `None`; the other three simply dismiss.
                //
                // The dispatcher hands a `Click`/`Pointer` to the owner whatever its
                // `hit_source()` is, which is what lets a `HitSource::Legacy` surface answer for
                // its own geometry (`Dispatcher::hit_page`).
                if let InputKind::Click { x, y, .. } = input.kind {
                    let action = match &mut self.panel {
                        Panel::More(p) => Some(p.click(x, y)),
                        _ => None,
                    };
                    self.dismiss(fx);
                    if let Some(action) = action {
                        Self::ask(fx, PlayerReq::More(action));
                    }
                    self.closing(fx);
                    return Handled::Yes;
                }
                // Hover: focus follows the cursor over the `…` popover's rows, which is the only
                // one of the four with a hover model at all.
                if let InputKind::Pointer { x, y, .. } = input.kind {
                    if let Panel::More(p) = &mut self.panel {
                        p.pointer_focus(x, y);
                    }
                    return Handled::Yes;
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
                let _ = cx;
                Self::ask(fx, PlayerReq::ExtendHud(HUD_LINGER_MS));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

/// These panels keep their own cursors (a `TableView`'s row, the card's button column, the strip's
/// slot), exactly as the player page keeps `HudNav`: phase 9 moves their state, not their focus
/// model. `FocusSource::Legacy`/`HitSource::Legacy` is therefore the honest answer, and this impl
/// is inert.
impl<H: AppLike<Memory = PageMemory>> Focusable<H> for PlayerOverlayScreen {
    fn groups(&self, _cx: &Cx<'_, H>, _out: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        None
    }
    fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, _key: &u32, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        None
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: 0,
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
        match &mut self.panel {
            Panel::Tracks(p) => p.draw(appear),
            Panel::Info(p) => p.draw(ps, appear),
            Panel::Chapters(p) => p.draw(ps, appear),
            Panel::More(p) => p.draw(appear),
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Legacy
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Legacy
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
