//! **The player's overlays, as entries on the page's own `ModalStack`** (restructure spec §6.2
//! "Page-owned panels … Player: its four overlays", §9, phase 9).
//!
//! The track menu, the Info card, the Chapters strip and the `…` options popover were four
//! `Route::Player { overlay }` values driven by four arms of `app/run.rs`'s key ladder, over four
//! modules' worth of `static mut`. They are now `AppArg` variants presented on the player
//! page's stack, mounted by the one `Mounter`, styled `PlayerPanel { survives_failure }`, and each
//! owning its panel's state as a field. The container owns the PHASE and the appear spring; the
//! surface owns input while it is `Opening | Open`.
//!
//! **One screen type with an inner enum, not one type per panel**, because they differ only in which
//! panel they hold and share every rule that matters here: what a transport key does, how a held
//! direction is paced, when a panel dismisses itself, and how a decision reaches the loop. A copy
//! of those rules per panel is exactly the drift `overlay_swallows_key` was written to end.
//!
//! **The transport-key rule is the one behaviour a reader must not lose.** A viewer holding the
//! track menu, the Info card or the Chapters strip open still expects PAUSE/PLAY to work, and the
//! panel to stay up (issue 28). While the ladder owned these keys that was expressed by
//! `overlay_swallows_key` answering `false` for exactly those three keys so the press FELL THROUGH
//! to the ordinary transport arms. A surface cannot fall through — the dispatcher hands it the key
//! and the ladder never sees it — so it FORWARDS instead: `PlayerReq::Transport`, which the loop
//! spends on the same toggle, leaving this panel untouched. `More` keeps the old
//! swallow-everything answer for the same reason it always did. `More`'s Quality row is a page
//! (`ui::page_stack`): BACK, LEFT at the edge and a title click pop it, RIGHT and OK push it, and
//! only BACK at the root dismisses; its replay canon carries the page path like Tracks'.
//!
//! **The `Focusable` half is real now (restructure phase 12, D2).** Each panel's own
//! `*Part` wrapper (`appkit::track_menu::TrackMenuPart`, `appkit::chapters_panel::ChaptersPart`,
//! `appkit::info_panel::InfoPanelPart`, `appkit::more_menu::MoreMenuPart`) answers the Engine's query
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
use crate::appkit::chapters_panel::{chapter_count, ChaptersPart};
use crate::ui::consts;
use crate::ui::frame::Budget;
use crate::appkit::info_panel::InfoPanelPart;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, LogicalState,
    Machine, NavOp,
};
use crate::appkit::more_menu::{MoreMenuPart, MoreOk};
use crate::ui::screen::{
    At, Dir, DrawFrame, FocusSource, Focusable, GroupSpec, HitSource, Part, Placed, RenderStrategy,
    Screen, ScreenEvent, Step,
};
use crate::appkit::timing_capsule::{CapsuleOut, TimingCapsule};
use crate::appkit::track_menu::{TrackMenuPart, TrackOk};

use super::HudPolicy;
use crate::ui::Rect;

use super::input::{HUD_LINGER_MS, HUD_MENU_MS};
use crate::screens::registry::{RepeatGate, PANEL_REPEAT_MS};

/// The fields [`PlayerOverlayScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
/// The selected ROW is in it deliberately: these panels' UP/DOWN changes nothing else in the app,
/// so without it a replay grades a panel opening and closing and nothing between.
///
/// `sel` is the highlighted row's index for every panel but the track menu and More, whose own
/// canons ([`crate::appkit::track_menu::TrackMenuState::canon`], [`crate::appkit::more_menu::MoreMenuState::canon`])
/// replace it: the track menu's `tab`, then for both the page-path depth, each pushed page's
/// `{page,return_key}` and the selected row's KEY.
pub(crate) const SHAPE: &str =
    "PlayerOverlayScreen{kind:str,sel:u32|Tracks:{tab:u32,depth:u32,pages:[{page:u32,return_key:u32}],key:u32}|More:{depth:u32,pages:[{page:u32,return_key:u32}],key:u32}}";

/// Which panel a [`PlayerOverlayArg`] names, and — since the argument is what the container holds
/// for the whole life of the entry — the identity `ScreenArg::same_instance` compares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OverlayKind {
    /// The audio/subtitle picker (`appkit/track_menu.rs`). `tab` is the panel it opens on: the two
    /// on-screen discs open it directly on theirs.
    Tracks { tab: c_int },
    /// The now-playing Info card (`appkit/info_panel.rs`).
    Info,
    /// The chapter strip (`appkit/chapters_panel.rs`).
    Chapters,
    /// The `…` overflow popover (`appkit/more_menu.rs`). `quality` opens it focused on the active
    /// quality rung — the failure read-out's recovery path, which is also why this is the one
    /// overlay whose style `survives_failure`.
    More { quality: bool },
    /// The on-video Subtitle Timing capsule (`appkit/timing_capsule.rs`, plan `subtitle-menu-capsule`
    /// §4) — the Subtitles panel's Timing row hands off here
    /// (`appkit::track_menu::TrackOk::OpenTiming`) rather than stepping the offset itself. The only
    /// overlay whose [`Self::hud_policy`] is [`HudPolicy::Hidden`] — the capsule sits where the
    /// transport's caption band and scrubber would, so the two must never be up together.
    Timing,
}

impl OverlayKind {
    /// **One of every kind**, parameters at their plain-open value — the list every "each panel"
    /// census iterates (the heartbeat alphabet, `registry::every_surface_arg`, the surface
    /// navigation tests), so a new kind is added here once rather than to each of them.
    /// `every_kind_is_listed_once` fails to compile when a variant is added and not listed.
    #[cfg(test)]
    pub(crate) const ALL: [OverlayKind; 5] = [
        OverlayKind::Tracks { tab: 0 },
        OverlayKind::Info,
        OverlayKind::Chapters,
        OverlayKind::More { quality: false },
        OverlayKind::Timing,
    ];

    /// The heartbeat's `overlay=` word. These spellings are the ones `tests/manifest.json`'s
    /// fps scenes select by; changing one silently disarms a scene rather than failing anything
    /// visible (§15.3). Since phase 10 item 4 they reach the heartbeat DIRECTLY — `app::overlay_word`
    /// is the topmost surface's own `Screen::name`, so there is no mapping table between this
    /// function and the printed line, and `app::heartbeat_word_tests` derives its alphabet by
    /// presenting every surface argument and reading the word back.
    /// **WHICH panel this is, with the parameter thrown away** — the identity
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
            OverlayKind::Timing => 4,
        }
    }

    pub(crate) fn word(self) -> &'static str {
        match self {
            OverlayKind::Tracks { .. } => "menu",
            OverlayKind::Info => "info",
            OverlayKind::Chapters => "chapters",
            OverlayKind::More { .. } => "more",
            OverlayKind::Timing => "timing",
        }
    }

    /// **What this panel does to the transport HUD while it is up** (any phase but `Hidden` —
    /// `app::bridge::player_hud_policy`, plan `subtitle-menu-capsule` §4). Only the Timing capsule
    /// HIDES it: it sits at the caption's own band, over the scrubber, so the transport and the
    /// capsule must never both be drawn. The other panels leave it up and lift the captions clear
    /// of it — the same distinction their own `survives_failure`/`swallows_transport` answer for
    /// different questions.
    pub(crate) fn hud_policy(self) -> HudPolicy {
        match self {
            OverlayKind::Timing => HudPolicy::Hidden,
            OverlayKind::Tracks { .. } | OverlayKind::Info | OverlayKind::Chapters | OverlayKind::More { .. } => {
                HudPolicy::Lifted
            }
        }
    }

    /// **Does this panel keep the transport alive while a viewer reads it** (the per-frame
    /// `ExtendHud` of `PlayerOverlayScreen`'s `Tick`, and the ordinary linger a hand-off INTO it
    /// leaves behind)? Every panel that shows the HUD; never one that hides it, since extending a
    /// HUD it is hiding would fight [`HudPolicy::Hidden`].
    pub(crate) fn extends_hud(self) -> bool {
        self.hud_policy() != HudPolicy::Hidden
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
        c.u32(self.kind.slot() as u32);
        match self.kind {
            OverlayKind::Tracks { tab } => {
                c.u32(tab as u32);
            }
            OverlayKind::More { quality } => {
                c.bool(quality);
            }
            OverlayKind::Info | OverlayKind::Chapters | OverlayKind::Timing => {}
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(self.kind.word());
    }
}

/// The panel itself — the state four modules kept in `static mut`s until phase 9.
pub(crate) enum Panel {
    Tracks(crate::appkit::track_menu::TrackMenuState),
    Info(crate::appkit::info_panel::InfoPanelState),
    Chapters(crate::appkit::chapters_panel::ChaptersState),
    More(crate::appkit::more_menu::MoreMenuState),
    Timing(TimingCapsule),
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
    /// The playing item's UltraBlur corners, read once at open — what this panel's dim inherits
    /// ([`Scrim::over_video`](crate::ui::screen::Scrim::over_video)): the page under it is the
    /// hardware video plane, which GL cannot sample.
    corners: Option<[[f32; 3]; 4]>,
    /// The panel draws nothing this frame (a FAILED playback hides the content panels with the
    /// transport), so it must ask for no dim either. Resolved in `prepare`, which runs before the
    /// page pass the container paints dims in; `draw` is too late for that.
    suppressed: bool,
}

/// **"Your languages"**, in preference order, for the Subtitles menu's grouping
/// (`metadata::sub_layout::sub_sections`, plan `subtitle-menu-capsule` §2-3): the subtitle-language
/// preference this play resolved under (the show's own, else the account's —
/// `route::cur_sub_pref_lang`), then the playing audio's language, then the current subtitle's
/// own — each only if it names one. `ui/` never sees a Plex account type, only these codes.
fn subtitle_yours_langs(ps: &crate::route::PlaybackSession, meta: crate::metadata::MetadataView<'_>) -> Vec<String> {
    let mut yours = Vec::new();
    if let Some(pref) = crate::route::cur_sub_pref_lang(ps) {
        if !pref.trim().is_empty() {
            yours.push(pref.to_string());
        }
    }
    if let Some(item) = meta.playing() {
        let asid = crate::route::cur_audio_sid(ps);
        if let Some(a) = item.audio.iter().find(|s| s.id == asid) {
            if !a.lang_code.trim().is_empty() {
                yours.push(a.lang_code.clone());
            }
        }
        let ssid = crate::route::cur_sub_sid(ps);
        if let Some(s) = item.subs.iter().find(|s| s.id == ssid) {
            if !s.lang_code.trim().is_empty() {
                yours.push(s.lang_code.clone());
            }
        }
    }
    yours
}

impl PlayerOverlayScreen {
    /// Every panel here answers on exactly one focus group — see each `*Part`'s own `groups()`
    /// (restructure phase 12); this screen never holds more than one panel at a time, so there is
    /// no second id to reserve.
    const GROUP: GroupId = GroupId(0);

    pub(crate) fn new(ps: &crate::route::PlaybackSession, meta: crate::metadata::MetadataView<'_>, entry: EntryId, kind: OverlayKind) -> Self {
        let panel = match kind {
            OverlayKind::Tracks { tab } => Panel::Tracks(plx_base::diag::spans::span("tmnew", || {
                crate::appkit::track_menu::TrackMenuState::new(ps, meta, tab, subtitle_yours_langs(ps, meta))
            })),
            OverlayKind::Info => Panel::Info(crate::appkit::info_panel::InfoPanelState::new()),
            OverlayKind::Chapters => {
                Panel::Chapters(crate::appkit::chapters_panel::ChaptersState::new(meta))
            }
            OverlayKind::More { quality: false } => {
                Panel::More(crate::appkit::more_menu::MoreMenuState::new(ps))
            }
            // Force Direct Play offers no Quality section, so there is no rung to land on: a
            // quality entry is the ordinary menu then (`more_menu::rows_for`).
            OverlayKind::More { quality: true } if crate::route::forced_direct_play(ps) => {
                Panel::More(crate::appkit::more_menu::MoreMenuState::new(ps))
            }
            OverlayKind::More { quality: true } => {
                Panel::More(crate::appkit::more_menu::MoreMenuState::new_quality(ps))
            }
            OverlayKind::Timing => {
                let (lo, hi) = crate::player::subtitle_offset_range_ms();
                Panel::Timing(TimingCapsule::new(crate::player::subtitle_offset_ms(), lo, hi))
            }
        };
        Self {
            entry,
            kind,
            panel,
            repeat: RepeatGate::IDLE,
            corners: meta.playing().and_then(|p| p.blur),
            suppressed: false,
        }
    }

    pub(crate) fn kind(&self) -> OverlayKind {
        self.kind
    }

    /// **Re-address a panel that is already up.** The two disc icons open the one track menu on
    /// their own tab, so the second press has to move the tab of the entry that exists rather than
    /// present a second one — `same_instance` says they ARE the same instance, and this is the
    /// other half of that: what "the same instance, at a different address" does.
    pub(crate) fn retarget(&mut self, ps: &crate::route::PlaybackSession, meta: crate::metadata::MetadataView<'_>, kind: OverlayKind) {
        if kind.slot() != self.kind.slot() {
            return;
        }
        self.kind = kind;
        match (&mut self.panel, kind) {
            (Panel::Tracks(p), OverlayKind::Tracks { tab }) => p.focus_tab(ps, meta, tab),
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
            // Timing exposes no rows and no cursor — the capsule is driven entirely by its own
            // `key()`, never by the engine's focus movement.
            Panel::Timing(_) => 0,
        }
    }

    /// Resolve `/tmp/plxnative-menupick`'s second field to an absolute row: a plain row number
    /// parses as itself (the original contract); the Audio tab's `"boost"`/`"loudness"` are tried
    /// as NAMED targets through [`crate::appkit::track_menu::TrackMenuState::row_for_audio_target`].
    /// `None` when neither applies — an unparseable number, a name on the wrong tab, or an
    /// unrecognized name. The Subtitles tab's `"track:N"` is not a row at all (the track may sit
    /// on a page that is not showing): see [`Self::resolve_menupick_track`].
    pub(crate) fn resolve_menupick_row(&self, target: &str) -> Option<c_int> {
        if let Ok(row) = target.parse::<c_int>() {
            return Some(row);
        }
        match &self.panel {
            Panel::Tracks(p) => p.row_for_audio_target(target),
            _ => None,
        }
    }

    /// Resolve a `"track:N"` target of `/tmp/plxnative-menupick` to the subtitle's index in the
    /// item's list ([`crate::appkit::track_menu::TrackMenuState::sub_track_for_target`]) — root tracks
    /// first, then the ones behind Other languages. `None` for any other target.
    pub(crate) fn resolve_menupick_track(&self, target: &str) -> Option<usize> {
        match &self.panel {
            Panel::Tracks(p) => p.sub_track_for_target(target),
            _ => None,
        }
    }

    /// `submenuosc`'s read of the Tracks panel: `(tab, page depth, has Other languages)`; `None`
    /// when the panel on screen is not Tracks.
    pub(crate) fn tracks_probe(&self) -> Option<(c_int, usize, bool)> {
        match &self.panel {
            Panel::Tracks(p) => Some(p.osc_probe()),
            _ => None,
        }
    }

    /// `moreosc`'s read of the More panel: how many pages are pushed; `None` when the panel on
    /// screen is not More.
    pub(crate) fn more_probe(&self) -> Option<usize> {
        match &self.panel {
            Panel::More(p) => Some(p.osc_depth()),
            _ => None,
        }
    }

    /// `moreosc`'s cursor seat: put the More cursor on the Quality row so the next real RIGHT key
    /// enters it.
    /// `false` when the panel is not More or its root offers no Quality row (Force Direct Play).
    pub(crate) fn seat_more_quality(&mut self) -> bool {
        match &mut self.panel {
            Panel::More(p) => p.focus_key(crate::ui::form::FormId::key(&crate::appkit::more_menu::MoreRow::OpenQuality).0),
            _ => false,
        }
    }

    /// `submenuosc`'s cursor seat: put the Tracks cursor on `row` so the next real RIGHT key
    /// enters it. `false` when this page has no such row.
    pub(crate) fn seat_track_row(&mut self, row: crate::appkit::track_menu::TrackRow) -> bool {
        match &mut self.panel {
            Panel::Tracks(p) => p.focus_id(row),
            _ => false,
        }
    }

    /// **The headless pick of a named subtitle track**: commit the track at `i` by its own index
    /// ([`crate::appkit::track_menu::TrackMenuState::commit_sub_track`]), whatever page its row is on.
    /// Like [`Self::pick_track_row`] it leaves the panel on screen.
    pub(crate) fn pick_sub_track(
        &mut self,
        meta: crate::metadata::MetadataView<'_>,
        i: usize,
    ) -> Option<crate::appkit::track_menu::TrackCommit> {
        match &mut self.panel {
            Panel::Tracks(p) => match p.commit_sub_track(i, meta) {
                TrackOk::Commit { commit, .. } => Some(commit),
                TrackOk::Dismiss | TrackOk::OpenTiming | TrackOk::Inert | TrackOk::Navigated => None,
            },
            _ => None,
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
        meta: crate::metadata::MetadataView<'_>,
        row: c_int,
    ) -> Option<crate::appkit::track_menu::TrackCommit> {
        if let Panel::Tracks(p) = &mut self.panel {
            p.focus_row(row);
            return match p.on_ok(meta) {
                TrackOk::Commit { commit, .. } => Some(commit),
                TrackOk::Dismiss | TrackOk::OpenTiming | TrackOk::Inert | TrackOk::Navigated => None,
            };
        }
        None
    }

    /// **The Info card's DEFERRED press, read back on the spring-back.** Its two actions are
    /// control faces with a pop of their own, so its OK arm only asks the loop for
    /// [`PlayerReq::ArmInfoPress`]; the loop's press machine commits one frame later, and this is
    /// how it asks the panel what the press meant. `None` for the other three, whose OK acts at
    /// once — asking any of them is a caller confusion rather than a state, so it cannot be a
    /// silent no-op that returns an action.
    pub(crate) fn info_press_action(
        &mut self,
        meta: crate::metadata::MetadataView<'_>,
    ) -> Option<crate::appkit::info_panel::InfoAction> {
        match &mut self.panel {
            Panel::Info(p) => Some(p.on_ok(meta)),
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
    fn activate<H: AppLike + crate::screens::registry::MetadataLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) {
        match &mut self.panel {
            Panel::Tracks(p) => match p.on_ok(H::metadata(cx)) {
                TrackOk::Commit { commit, keep_open } => {
                    fx.push(Fx::App(AppFx::Player(PlayerReq::CommitTrack(commit))));
                    if keep_open {
                        // a Style pick (or an audio toggle): the viewer is watching the change, so
                        // the transport keeps a menu's read time rather than starting to close
                        self.moved(fx);
                    } else {
                        self.dismiss(fx);
                        self.closing(fx);
                    }
                }
                TrackOk::Dismiss => {
                    self.dismiss(fx);
                    self.closing(fx);
                }
                // a dim row (Timing while subtitles are Off, a locked Style row): nothing happens and
                // the panel stays. A Nav row opened a page: the panel stays with its rows replaced.
                TrackOk::Inert | TrackOk::Navigated => self.moved(fx),
                TrackOk::OpenTiming => {
                    // The Tracks→Timing hand-off (plan §4): dismiss THIS entry and ask for a fresh
                    // `Timing` overlay. `open_player_overlay` sees a different slot
                    // (`OverlayKind::slot`) than the one being dismissed, so it presents a new
                    // entry rather than re-addressing this one. The ordinary linger is left only
                    // if the panel taking over keeps the HUD at all — the capsule hides it.
                    let next = OverlayKind::Timing;
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::OpenOverlay(next));
                    if next.extends_hud() {
                        self.closing(fx);
                    }
                }
            },
            Panel::More(p) => match p.on_ok(ps) {
                MoreOk::Action(action) => {
                    self.dismiss(fx);
                    Self::ask(fx, PlayerReq::More(action));
                    self.closing(fx);
                }
                // the Quality row opened its page: the panel stays with its rows replaced
                MoreOk::Navigated => self.moved(fx),
            },
            Panel::Info(p) => {
                let action = p.on_ok(H::metadata(cx));
                self.dismiss(fx);
                Self::ask(fx, PlayerReq::Info(action));
                self.closing(fx);
            }
            Panel::Chapters(p) => {
                let ns = p.on_ok(H::metadata(cx));
                self.dismiss(fx);
                if ns >= 0 {
                    Self::ask(fx, PlayerReq::SeekTo(ns));
                }
                self.closing(fx);
            }
            // Unreachable in practice: Timing publishes no focus group and no `ElemKind::Bare`
            // row (`Focusable::groups` answers empty, `key()` returns `Handled::Yes` for every
            // key before `Machine::step` ever reaches its `Activate`/`PressCommit` arms), so
            // nothing ever calls `activate()` while this variant is up. Exhaustiveness only.
            Panel::Timing(_) => {}
        }
    }

    /// **The engine reached this panel's group EDGE and re-delivered the direction**
    /// (`EdgeRule::Screen`, §7.3 step 3) — the one thing left that a panel decides outside its own
    /// scope, because it moves focus OFF this screen (Chapters'/Info's DOWN) or re-addresses the
    /// panel entirely (Tracks' LEFT/RIGHT: LEFT pops a sub-page, else switches tab
    /// (`TrackMenuState::focus_tab`); RIGHT enters a Nav row, else switches tab). More's LEFT/RIGHT
    /// are `Screen` edges too, for its Quality page: LEFT pops it, RIGHT enters the Quality row,
    /// and either does nothing otherwise (no tabs to switch).
    fn edge_key<H: AppLike + crate::screens::registry::MetadataLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        cx: &Cx<'_, H>,
        key: consts::Key,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        use consts::Key;
        match (&mut self.panel, key) {
            // LEFT pops a sub-page, else switches tab as before; RIGHT on a Nav row enters it (inert
            // when that row is disabled), else switches tab as before
            (Panel::Tracks(p), Key::Left { .. }) => {
                p.on_left(ps, H::metadata(cx));
                self.moved(fx);
            }
            (Panel::Tracks(p), Key::Right { .. }) => {
                p.on_right(ps, H::metadata(cx));
                self.moved(fx);
            }
            // More's one drill-in page: LEFT pops it (nothing at the root), RIGHT enters a Nav row
            (Panel::More(p), Key::Left { .. }) => {
                p.pop(ps);
                self.moved(fx);
            }
            (Panel::More(p), Key::Right { .. }) => {
                p.on_right(ps);
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

    /// The one key ladder these panels share, for everything the ENGINE does not already resolve
    /// (§7.3 step 1). Transport keys always fall through here first (module doc) — the only keys
    /// this ladder still fully owns. A direction is paced by [`PANEL_REPEAT_MS`] and then handed
    /// to the engine's own `neighbour`/`EdgeRule` (`Handled::No`) unless the engine has already
    /// reached this panel's group edge and re-delivered it (`edge_key`, above). OK is likewise
    /// left to the engine's own `Activate`/press machinery (§7.4; see [`Self::activate`]). BACK
    /// dismisses — except on a Tracks or More sub-page, where it pops one page first (the root, and the
    /// other panels, dismiss). Tracks and More close silently, exactly as the old ladder did; Info
    /// and Chapters also hand the transport the ordinary linger.
    fn key<H: AppLike + crate::screens::registry::MetadataLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        cx: &Cx<'_, H>,
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
        // The Timing capsule owns every key itself — it has no focus group for the engine to
        // walk, so it never falls through to the directional block below and OK/BACK must not
        // reach `Machine::step`'s `Activate` arm either (the capsule has nothing for that
        // machinery to activate). The cadence is the SAME `RepeatGate` every other panel here
        // paces a held direction with — one admission rule, reused rather than a second copy of
        // it living in `appkit::timing_capsule`.
        if let Panel::Timing(cap) = &mut self.panel {
            match edge {
                Edge::Down => self.repeat.rearm(now),
                Edge::Repeat if !self.repeat.ready_every(now, PANEL_REPEAT_MS) => {
                    return Handled::Yes
                }
                Edge::Up => return Handled::Yes,
                _ => {}
            }
            match cap.key(key, edge, now) {
                Some(CapsuleOut::Step(v)) => {
                    Self::ask(
                        fx,
                        PlayerReq::CommitTrack(crate::appkit::track_menu::TrackCommit::SubtitleOffset(v)),
                    );
                }
                // The transport stays down after the capsule leaves — not asked for here, since a
                // pointer miss closes it without reaching this ladder; `PlayerScreen::set_hud_policy`
                // turns ANY close into a dismissed HUD on the frame the surface is gone.
                Some(CapsuleOut::Close) => self.dismiss(fx),
                Some(CapsuleOut::Bump) | None => {}
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
                return self.edge_key(ps, cx, key, fx);
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
                // BACK on a Subtitles / Quality sub-page goes up one page; on a root it dismisses
                if let Panel::Tracks(p) = &mut self.panel {
                    if p.pop(ps, H::metadata(cx)) {
                        self.moved(fx);
                        return Handled::Yes;
                    }
                }
                if let Panel::More(p) = &mut self.panel {
                    if p.pop(ps) {
                        self.moved(fx);
                        return Handled::Yes;
                    }
                }
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

impl<H: crate::screens::registry::PlayerLike + crate::screens::registry::MetadataLike> Machine<H> for PlayerOverlayScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // The frame's publication of the playback session (spec §2.3): a screen reads the Player
        // machine's decisions here and asks for a change through an effect, never by writing them.
        let ps = H::session(cx);
        match ev {
            ScreenEvent::Input(input) => {
                // The dispatcher's `machine::Key` is the four directions plus OK/BACK; the
                // transport alphabet this ladder turns on lives in `consts::Key`. Preserve the
                // canonical navigation key and consult raw fields only for `Other`.
                if let InputKind::Key {
                    key, sym, wcode, edge, at_edge,
                } = input.kind
                {
                    return self.key(ps, cx, consts::classify_input(key, sym, wcode), edge, at_edge, input.at.ms, fx);
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
                // Tracks' element is a row KEY (`appkit::track_menu::TrackRow`), the rest's an index.
                let i = to.elem as i32;
                match &mut self.panel {
                    Panel::Tracks(p) => p.focus_key(to.elem),
                    Panel::Info(p) => p.set_focus(i),
                    Panel::Chapters(p) => p.set_sel(i),
                    Panel::More(p) => { p.focus_key(to.elem); }
                    // Timing has no cursor the engine could have moved — its own `key()` owns
                    // every press and always returns `Handled::Yes`, so this arm is unreached for
                    // it in practice.
                    Panel::Timing(_) => {}
                }
                self.moved(fx);
                Handled::No
            }
            // A pointer click on ANY row (`Activate::Immediate`/`Direct`, every `*Part::draw`
            // above) always applies at once. A keyboard OK on a `Control`/`Card` row
            // (`Info`/`Chapters`) arms an engine press first and only reaches here as
            // `PressCommit`, on release — except `Info`, whose `PressCommit` defers instead to
            // the loop's own tvOS dip (`Self::activate`'s doc explains the split).
            ScreenEvent::Activate(elem) => {
                // the event names the row that was hit (its key for Tracks): seat the panel on
                // THAT row before `activate` reads the panel's own cursor, so a click never acts
                // on a neighbour the cursor happened to still hold
                if let Panel::Tracks(p) = &mut self.panel {
                    if crate::ui::page_stack::is_title_key(*elem) {
                        // the "< TITLE" band: a click goes back one page, and acts on no row
                        p.pop(ps, H::metadata(cx));
                        self.moved(fx);
                        return Handled::No;
                    }
                    p.focus_key(*elem);
                }
                if let Panel::More(p) = &mut self.panel {
                    if crate::ui::page_stack::is_title_key(*elem) {
                        // the "< QUALITY" band: a click goes back one page, and acts on no row
                        p.pop(ps);
                        self.moved(fx);
                        return Handled::No;
                    }
                    p.focus_key(*elem);
                }
                self.activate(ps, cx, fx);
                Handled::No
            }
            ScreenEvent::PressCommit(_) => {
                if let Panel::Info(_) = &self.panel {
                    Self::ask(fx, PlayerReq::ArmInfoPress);
                } else {
                    self.activate(ps, cx, fx);
                }
                Handled::No
            }
            ScreenEvent::Tick(tick) => {
                let dt = tick.dt();
                match &mut self.panel {
                    Panel::Tracks(p) => p.update(dt, cx.measure, ps, H::metadata(cx)),
                    Panel::Info(p) => p.update(dt),
                    Panel::Chapters(p) => p.update(dt, H::metadata(cx)),
                    Panel::More(p) => p.update(dt, cx.measure, ps),
                    Panel::Timing(p) => p.update(dt),
                }
                // The transport must not auto-hide out from under a panel a viewer is reading —
                // the rule `app/run.rs` kept as "keep the HUD alive while the track menu / Info
                // card / Chapters strip is open", stated once here by the surface that IS open.
                // A panel that HIDES the HUD (the Timing capsule) does not share its read time.
                if self.kind.extends_hud() {
                    Self::ask(fx, PlayerReq::ExtendHud(HUD_LINGER_MS));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl PlayerOverlayScreen {
    /// Every visible row registers its own stop (§7.6) — the same per-row geometry
    /// `Focusable::place` answers for focus movement, so the hit map and the focus engine agree on
    /// every rect. Each panel's own `*Part::draw` does the registration (it needs the
    /// module-private row geometry this screen cannot reach directly); the paint happens first, on
    /// the owned, mutable `Panel`. Paint-free, so issue #162's pointer census runs it on the host.
    pub(crate) fn record_stops<H>(&self, f: &mut DrawFrame<'_, '_, H>)
    where
        H: crate::screens::registry::PlayerLike + crate::screens::registry::MetadataLike,
    {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(f.cx)) }.draw(f, Rect::FULL),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.draw(f, Rect::FULL),
            // Timing owns its own key ladder and never asks the engine for focus, so it has no
            // per-row geometry to register — `draw`, below, paints the capsule itself.
            Panel::Timing(_) => {}
        }
    }
}

/// **Real per-row groups, delegated to whichever panel is ACTIVE** (restructure phase 12, D2 —
/// see `screens/player/mod.rs`'s own `Focusable` impl for the sibling case). Each panel exposes
/// its own row geometry through a `*Part` wrapper (`appkit::track_menu::TrackMenuPart`,
/// `appkit::chapters_panel::ChaptersPart`, `appkit::info_panel::InfoPanelPart`,
/// `appkit::more_menu::MoreMenuPart`), built fresh per query over a SHARED reference to the panel's
/// own state — every method here is `&self`, and so is every method on this screen's own
/// `Focusable` impl (§7.1: "the engine never mutates a screen"), which is why each wrapper's
/// `state` field is `&'a StateType` rather than `&'a mut` (see each wrapper's own doc). This
/// screen holds exactly one panel at a time, so there is no `Composed`/`layout()` here — that
/// trait concatenates SEVERAL simultaneous parts, and one instance's panels never coexist.
impl<H: crate::screens::registry::PlayerLike + crate::screens::registry::MetadataLike> Focusable<H> for PlayerOverlayScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.groups(cx, out),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.groups(cx, out),
            // Timing declares no group at all — the capsule's own `key()` swallows every press
            // (`Handled::Yes`) before the engine's focus machinery ever sees one.
            Panel::Timing(_) => {}
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.group_of(key, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.group_of(key, cx),
            Panel::Timing(_) => None,
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.neighbour(key, dir, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.neighbour(key, dir, cx),
            Panel::Timing(_) => Step::Edge,
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.place(key, cx, at),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.place(key, cx, at),
            Panel::Timing(_) => None,
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.reconcile(want, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.reconcile(want, cx),
            Panel::Timing(_) => want,
        }
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        match &self.panel {
            Panel::Tracks(p) => TrackMenuPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::Info(p) => InfoPanelPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::Chapters(p) => ChaptersPart { state: p, entry: self.entry, group: Self::GROUP, count: chapter_count(H::metadata(cx)) }.seat(g, from, cx),
            Panel::More(p) => MoreMenuPart { state: p, entry: self.entry, group: Self::GROUP }.seat(g, from, cx),
            Panel::Timing(_) => FocusKey { entry: self.entry, elem: 0 },
        }
    }
}

impl LogicalState for PlayerOverlayScreen {
    fn write(&self, c: &mut Canon) {
        c.str(self.kind.word());
        match &self.panel {
            // the tab, the page path with each opener, and the selected row's KEY
            Panel::Tracks(p) => p.canon(c),
            // the Quality page's depth and opener, and the selected row's KEY
            Panel::More(p) => p.canon(c),
            _ => {
                c.u32(self.sel() as u32);
            }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(self.kind.word());
    }
}

impl<H: crate::screens::registry::PlayerLike + crate::screens::registry::MetadataLike> Screen<H> for PlayerOverlayScreen {
    fn name(&self) -> &'static str {
        self.kind.word()
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, cx: &Cx<'_, H>) {
        self.suppressed = crate::appkit::player_hud::transport_hidden(H::session(cx))
            && !self.kind.survives_failure();
        // The first `update` is a frame behind the mount, so the root page's strings are queued
        // here for the open frame's drain instead (`TrackMenuState::warm_open`).
        if !self.suppressed {
            match &self.panel {
                Panel::Tracks(p) => p.warm_open(cx.measure),
                Panel::More(p) => p.warm_open(cx.measure),
                Panel::Info(_) | Panel::Chapters(_) | Panel::Timing(_) => {}
            }
        }
    }
    /// **The panel's dim, through the container like every other surface's.** It used to be
    /// hand-drawn at the top of each panel's `draw`; the container paints it now, at the end of the
    /// player's page pass — the same z-order (over the transport and subtitles, under the panel) —
    /// through the stack's one field, latched from the playing item's UltraBlur corners because
    /// the page under it is the video plane. The Info card and the Chapters strip deliberately ask
    /// for none: they sit in the transport's own band and dim nothing.
    fn scrim(&self) -> crate::ui::screen::Scrim {
        use crate::ui::theme::underlay::{DIM_PLAYER, DIM_SHEET};
        if self.suppressed {
            return crate::ui::screen::Scrim::NONE;
        }
        match self.panel {
            Panel::Tracks(_) => crate::ui::screen::Scrim::over_video(DIM_PLAYER, self.corners),
            Panel::More(_) => crate::ui::screen::Scrim::over_video(DIM_SHEET, self.corners),
            // The capsule sits over the video like the Info card and the Chapters strip — no dim.
            Panel::Info(_) | Panel::Chapters(_) | Panel::Timing(_) => crate::ui::screen::Scrim::NONE,
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let ps = H::session(f.cx);
        // Stale content panels are gone with the transport when a playback has FAILED; the `…`
        // popover is the deliberate exception, because the read-out opened it as its own recovery
        // path (`OverlayKind::survives_failure`).
        if crate::appkit::player_hud::transport_hidden(ps) && !self.kind.survives_failure() {
            return;
        }
        // The container owns the appear spring; `DrawFrame::page_alpha` IS `Surface::motion.appear`
        // for a surface, which is what the panels' own `Popover` used to hold.
        let appear = f.page_alpha;
        let measure = f.measure;
        match &mut self.panel {
            Panel::Tracks(p) => p.draw(appear, measure),
            Panel::Info(p) => p.draw(ps, appear, measure, H::metadata(f.cx)),
            Panel::Chapters(p) => p.draw(ps, appear, measure, H::metadata(f.cx)),
            Panel::More(p) => p.draw(appear, measure),
            // Fixed y, never the live caption's own baseline — see `player_hud::CAPSULE_BOTTOM_Y`.
            Panel::Timing(p) => p.draw(crate::appkit::player_hud::CAPSULE_BOTTOM_Y, appear),
        }
        self.record_stops(f);
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
    /// **The pointer is held while the panel's card resizes or a page slides** (`ui::panel_motion`):
    /// the dispatcher swallows it before hit resolution, so a click on a row in motion is neither
    /// a hit on whatever sits under it nor a miss that would dismiss the surface.
    fn pointer_held(&self) -> bool {
        match &self.panel {
            Panel::Tracks(p) => p.transitioning(),
            Panel::More(p) => p.transitioning(),
            Panel::Info(_) | Panel::Chapters(_) | Panel::Timing(_) => false,
        }
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
