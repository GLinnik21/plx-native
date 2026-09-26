//! **The player as an owned `Screen`** (restructure spec §9, phase 9).
//!
//! `PlayerScreen` is the instance the container mounts for the player route. It owns every piece
//! of state the route used to keep in two places at once — the HUD's timer, cursor and dismissal,
//! the scrub gesture, the held-key timer, the repeat gate, the control row's springs and resume
//! clock, the Up Next countdown and the image-subtitle texture set — and it draws the whole route:
//! subtitles, the transport, the read-out, and (through the page-owned `ModalStack`) its overlays.
//!
//! It answers `RenderStrategy::VideoPlane`, which is what tells the frame that what is behind this
//! page is a hardware plane GL cannot read: no `popover::host` snapshot, no `Glass` source pass, no
//! `RouteGround`, no capture. The CONSEQUENCES of that strategy — the present gate, the opaque
//! region and the capture skip — are the loop's three privileged calls (§3.3 steps 1, 9 and 10) and
//! are not made here.
//!
//! **What is here and what is not.** Phase 9 moves the STATE and the DRAW; the key ladder in
//! `app/run.rs` remains a CALLER of this screen for the bare transport, exactly as `ui::press`'s
//! typed facade left the ladders callers of `App.input.press` in phase 2 (§14, "Press during
//! coexistence"). The four overlays are different: they are entries on this page's own
//! `ModalStack` and own their input outright, so their arms left the ladder with them.
//!
//! Two fields are PUBLISHED rather than owned by `player::TX` (§2.3): `hud.until` and `scrub.ns`.
//! [`PlayerScreen::publish`] mirrors both into the atomics the draw path and the engine read, and
//! is the only writer of either.

pub(crate) mod input;
pub(crate) mod overlay;
/// The Skip Intro / Skip Credits pill — a `ControlSlot` occupant of this screen's HUD, not a
/// screen of its own (phase 10: `ui/skip_pill.rs` moved here, where its one consumer lives).
pub(crate) mod skip_pill;
#[cfg(test)]
mod overlay_tests;

use std::borrow::Cow;

use crate::screens::registry::{AppFx, AppLike, PlayerLike, PlayerReq};
use crate::ui::consts;
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, Fx, FocusKey, GroupId, Handled, InputKind, InstanceId,
    LogicalState, Machine,
};
use crate::ui::player_hud::{self, ControlSlot, SubtitleBitmaps, TransportRow};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, FocusSource, Focusable, GroupKind, GroupSpec,
    HitSource, Hover, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop,
};
use crate::ui::up_next::Countdown;
use crate::ui::Rect;

use input::{HudState, Scrub};

/// The player's heartbeat word — byte-identical to `app::words::route_word(&AppArg::Player)`, and
/// what `tests/run.py` selects fps samples by (`tests/manifest.json`'s `route` field).
///
/// **This constant IS the heartbeat's `route=` now.** `bridge::frame` used to print
/// `route_word(app.route)` and `debug_assert_eq!` it against the top screen's `Screen::name`; phase
/// 12 (D1) deleted the route mirror, so there is one source and the assert has nothing left to
/// compare. Keeping the two tables equal is `app::words::heartbeat_word_tests`' job, and it derives
/// both rather than transcribing either — a word the app cannot print reads on the television as
/// "0 post-warmup samples", which is indistinguishable from a real regression.
pub(crate) const WORD: &str = "player";

/// The fields [`PlayerScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
pub(crate) const SHAPE: &str =
    "PlayerScreen{hud:{focus:i32,btn:i32,tab:i32,until:u32,dismissed:bool,visible_at_press:bool,\
     offer:Option<(u32,i64)>,was_standin:bool},scrub:{dir:i32,hold:bool,reveal:bool,drag:bool,\
     ns:i64,commit_at:u32},origin:Option<u32>,repair_alert:{open:bool,confirm:bool}}";

/// **The page this playback was launched from**, as the container's own identity.
///
/// A pair rather than the `EntryId` alone: an entry can be EVICTED at `CAP` and remounted, so its
/// `InstanceId` is what says "the same body", while the `EntryId` is what survives that eviction
/// and is what a `PopTo` names (§6.1's three tiers). Recorded once at `Mount`, from whatever page
/// was on top when the player was pushed, and never rewritten — an overlay opening on this page's
/// own `ModalStack` changes the input owner and must not be mistaken for a new origin, which is
/// the whole reason this is captured at `Mount` and not read live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Origin {
    pub(crate) entry: EntryId,
    pub(crate) instance: Option<InstanceId>,
}

/// The route's RENDER resources — recreated freely, never read as logical state (§6.1).
#[derive(Default)]
pub(crate) struct PlayerRender {
    /// The decoded image-subtitle display set and its cache key (`player_hud`'s `SET`/`KEY`/`SEL`).
    pub(crate) subs: SubtitleBitmaps,
    pub(crate) ass: crate::ui::ass_subtitles::AssSubtitles,
}

impl Default for SubtitleBitmaps {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct PlayerScreen {
    entry: EntryId,
    /// The transport HUD's timer, cursor, dismissal and control-row edges.
    pub(crate) hud: HudState,
    /// The scrub-seek gesture, including its preview position.
    pub(crate) scrub: Scrub,
    // (`held: HeldKey` stood here — "the client-side hold-repeat for the bare transport's
    // directions". Nothing armed it after phase 9 put the panels' input on the dispatcher, so it
    // wrote two constant zeros into this screen's canonical state; the type is deleted with the
    // loop's timer in phase 10 and this field with it.
    //
    // `repeat: RepeatGate` is NOT here either, and spec §9's target shape lists it: the paced
    // 110 ms admission is the OVERLAY's — `PlayerOverlayScreen` is what has a list to walk under
    // a held key. The bare transport's own directions run the continuous scrub instead, which is
    // a ramp rather than a discrete move and was never gated.)
    /// The control row's focus springs, its label-width memo and the resume clock
    /// (`player_hud`'s `ROW_POP`/`MEMO`/`PLAY_AT`/`PAUSE_SEEN`).
    pub(crate) row: TransportRow,
    /// The Up Next auto-advance countdown (`up_next`'s `DEADLINE`/`CANCELLED`).
    pub(crate) up_next: Countdown,
    /// This frame's control-row occupant, sampled ONCE by the loop and pushed here (`slot()`'s own
    /// doc: `playpos_ns` is written by LG's media thread and `player::pump` runs between the input
    /// handlers and the draw, so re-deriving it per call site let a keypress dispatch to a control
    /// that the same frame then declined to draw).
    pub(crate) slot: ControlSlot,
    /// Who owns the "pipeline is working" signal this frame — resolved once, after `player::pump`,
    /// and handed to both the transport and the read-out so they can never both light.
    pub(crate) busy: crate::ui::player_hud::Busy,
    /// Is the transport's MIDDLE drawn at all this frame? False while an Info card or Chapters
    /// strip has taken it — which panel is up is the container's answer, so the loop pushes it.
    pub(crate) transport: bool,
    /// Is ANY panel up this frame? Subtitles lift clear of the transport while one is, since that
    /// is exactly when the user is reading the bottom of the screen.
    pub(crate) lifted: bool,
    /// Where this playback returns to — see [`Origin`].
    pub(crate) origin: Option<Origin>,
    render: PlayerRender,
    repair_alert: crate::ui::decision_alert::DecisionAlert,
    repair_frames: std::cell::Cell<Option<(Rect, Rect)>>,
}

impl PlayerScreen {
    pub(crate) fn new(entry: EntryId) -> Self {
        Self {
            entry,
            hud: HudState::IDLE,
            scrub: Scrub::IDLE,
            row: TransportRow::new(),
            up_next: Countdown::default(),
            slot: ControlSlot::Discs,
            busy: crate::ui::player_hud::Busy::None,
            transport: true,
            lifted: false,
            origin: None,
            render: PlayerRender::default(),
            repair_alert: {
                let mut alert = crate::ui::decision_alert::DecisionAlert::new();
                alert.set_tone(crate::ui::decision_alert::Tone::Neutral);
                alert
            },
            repair_frames: std::cell::Cell::new(None),
        }
    }

    /// **Mirror the two published fields into `player::TX`** (§2.3).
    ///
    /// `hud.until` and `scrub.ns` are this screen's decisions; the draw path
    /// (`player_hud::draw_hud` reads `TX.scrub_ns`) and the engine read them, so the owner writes
    /// a snapshot after every step rather than letting either reader reach into the screen. The
    /// loop calls this once per iteration, after the ladders and the dispatcher frame have both
    /// had their turn — one publication per frame, and nothing between them can observe a
    /// half-written pair.
    pub(crate) fn publish(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        crate::player::TX.hud_until.store(self.hud.until, Relaxed);
        crate::player::TX.scrub_ns.store(self.scrub.ns, Relaxed);
    }

    /// **A teardown or an engine reload has retired the transport's mailboxes.**
    ///
    /// `Transport::reset`/`reset_for_reload` used to clear `hud_until`/`scrub_ns` themselves, which
    /// is a write to another owner's state (§2.3). They no longer touch either; `player::teardown`
    /// runs on the main thread and the loop delivers this instead, so the screen clears its own
    /// copy and the next [`publish`](Self::publish) republishes it.
    ///
    /// `for_reload` is the same distinction the transport draws: a reload is the SAME playback at a
    /// new position (a seek, a quality pick, an app-switch resume), so only the in-flight gesture
    /// is retired; a real stop ends the session, so the HUD's timer, its dismissal and the Up Next
    /// countdown go with it.
    pub(crate) fn transport_reset(&mut self, for_reload: bool) {
        self.scrub.ns = -1;
        self.scrub.disengage();
        self.scrub.commit_at = 0;
        if !for_reload {
            self.hud.until = 0;
            self.hud.dismissed = false;
            self.hud.last_offer = None;
            self.up_next.reset();
        }
        self.publish();
    }

    /// Draw the image-subtitle display set — the render half this screen owns.
    pub(crate) fn draw_subtitle_bitmap(&mut self, hud_up: bool) {
        crate::ui::player_hud::draw_subtitle_bitmap(&mut self.render.subs, hud_up);
    }

    /// The transport, with this instance's own springs, memo and countdown. Paint only: what a
    /// pointer can hit is registered by [`Self::record_stops`], from this screen's `Focusable`.
    pub(crate) fn draw_hud(
        &mut self,
        ps: &crate::route::PlaybackSession,
        now: u32,
        measure: &dyn crate::ui::machine::Measure,
        meta: crate::metadata::MetadataView<'_>,
    ) {
        crate::ui::player_hud::draw_hud(
            ps,
            &mut self.row,
            &self.up_next,
            self.slot,
            self.busy,
            self.hud.nav.focus,
            self.hud.nav.btn,
            self.hud.nav.tab,
            now,
            self.transport,
            measure,
            meta,
        );
    }

    /// **Every element this screen's `Focusable` declares and the frame DRAWS, registered as a
    /// pointer stop at the rect `place` gives it** — the rect D-pad focus uses, so "reachable by
    /// the D-pad" and "clickable with the pointer" are one statement (issue #162).
    ///
    /// Until #162 the stops were a by-product of painting: `player_hud::draw_hud` pushed each rect
    /// as it drew, in PAINT order, and the hit map resolves the LAST stop under the pointer. The
    /// scrubber's grab band (160 px tall) was pushed after the control row it overlaps, so a click
    /// on the middle of any disc resolved to the scrubber and seeked instead; and a stand-in
    /// (Skip / Up Next) got one floor-width region, so *Watch Credits* had no stop at all and a
    /// click on *Next Episode* activated item 0. Here the ORDER of [`Focusable::groups`] is the
    /// z-order — the scrubber's band first, underneath every control it overlaps — and every item
    /// of every group is registered, so neither can recur without the census test in this module
    /// failing.
    ///
    /// `hud_drawn` is the draw's own gate (the transport or a panel lifting it is on screen); the
    /// per-group half is [`Self::group_drawn`].
    fn record_stops<H: PlayerLike + crate::screens::registry::MetadataLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        hud_drawn: bool,
    ) {
        let ps = H::session(f.cx);
        let mut groups = Vec::new();
        Focusable::<H>::groups(self, f.cx, &mut groups);
        for g in groups.iter().filter(|g| self.group_drawn(g.id, ps, hud_drawn)) {
            // The repair alert's buttons are ordinary pressable controls (hover parks focus, the
            // press dips and commits on release); the transport's are `Direct` — see `draw`.
            let (hover, activate) = if g.id == GROUP_REPAIR {
                (Hover::Focus, Activate::Press)
            } else {
                (Hover::Ignore, Activate::Direct)
            };
            for i in 0..g.len as u32 {
                let elem = Self::elem_of(g.id, i);
                let Some(p) = Focusable::<H>::place(self, &elem, f.cx, At::Drawn) else { continue };
                f.stop(
                    crate::ui::Painter::root(),
                    Stop {
                        key: FocusKey { entry: self.entry, elem },
                        rect: p.rect,
                        rest_rect: p.rest_rect,
                        clip: p.clip,
                        hover,
                        activate,
                    },
                );
            }
        }
    }

    /// Is group `g` on screen this frame — the same gates `player_hud::draw_hud`/`draw_readout`
    /// paint behind. A failure owns the frame outright; the scrubber and control row are the
    /// transport's MIDDLE, which an open Info card or Chapters strip hides.
    fn group_drawn(&self, g: GroupId, ps: &crate::route::PlaybackSession, hud_drawn: bool) -> bool {
        let failed = player_hud::readout_owns_frame(self.busy);
        match g {
            GROUP_REPAIR => self.repair_alert.is_open() && self.repair_alert.settled(),
            GROUP_FAILURE => !self.repair_alert.visible() && player_hud::failure_ok_drawn(ps, self.busy),
            GROUP_SCRUB | GROUP_ROW => hud_drawn && !failed && self.transport,
            GROUP_TABS => hud_drawn && !failed,
            _ => false,
        }
    }

    /// Element `i` of group `g` — the inverse of `group_of`.
    fn elem_of(g: GroupId, i: u32) -> u32 {
        match g {
            GROUP_SCRUB => player_hud::ELEM_SCRUB,
            GROUP_ROW => player_hud::ELEM_ROW_BASE + i,
            GROUP_TABS => player_hud::ELEM_TAB_BASE + i,
            GROUP_FAILURE => player_hud::ELEM_FAILURE_OK,
            _ => REPAIR_CANCEL + i,
        }
    }

    /// **Everything this page draws from a CLOCK, folded into ONE value** (spec §8.3, §9).
    ///
    /// The loop compares it with the previous frame's and raises `ui::idle::invalidate()` when it
    /// changes; `Player::note_clock` owns the comparison. This is what replaces the exemption the
    /// player route used to have from the present gate.
    ///
    /// **Why a fingerprint and not an `invalidate` per animator.** `note_spring` sees every spring
    /// in the app for free, because a spring KNOWS whether it is still travelling. None of the
    /// terms below does: each is a value read fresh from a clock (`playpos_ns`, written by LG's
    /// media thread), from a DEADLINE (the HUD's auto-hide, the two-second `Play` mark) or from
    /// pipeline state, and an animator that reported unconditionally from such a read would
    /// present every frame — the gate turned off by another name, which is exactly what the route
    /// exemption was. A frame is owed when the ANSWER CHANGES, and that is one comparison for the
    /// whole page instead of a dozen hand-placed calls with nothing keeping them in step.
    ///
    /// The terms, and the animator class each stands for (the inventory this replaces):
    /// * `playpos_ns` / `seek_display_ns` / `scrub_ns`, WHILE THE TRANSPORT IS UP — the scrub bar,
    ///   the playhead knob, both clocks and the accelerating hold-to-scrub ramp: the MEDIA CLOCK
    ///   class. Conditional because none of it is drawn with the HUD hidden, which is ~99% of a
    ///   playback; reporting it there would present every frame for a picture nobody draws.
    /// * `subtitle_cue_id` and `active_bitmap_key` — text and image subtitles, a whole-screen
    ///   content change with no spring behind it, drawn on every frame of the route.
    /// * `hud_up` — the auto-hide DEADLINE. At `now == until` the entire transport disappears.
    /// * `since_play_ms < PLAY_MARK_MS` — the two-second `Play` mark, plus `paused`: the transport
    ///   MARK class, which is otherwise pure arithmetic on the same clocks.
    /// * `slot` — the Skip Intro / Skip Credits / Up Next occupant, which changes when the
    ///   playhead crosses a marker boundary. (`up_next`'s countdown reports from its own draw and
    ///   is not repeated here.)
    /// * `busy` — the read-out appearing and vanishing, including the `Playing -> Error` edge with
    ///   the HUD auto-hidden, where nothing else in the frame moves at all.
    pub(crate) fn clock_fingerprint(&self, ps: &crate::route::PlaybackSession, now: u32) -> u64 {
        use std::sync::atomic::Ordering::Relaxed;
        let pos = crate::player::playpos_ns();
        let hud_up = self.hud_up(ps, now);
        let mut h: u64 = 0;
        let mut mix = |v: u64| {
            // FNV-1a's constants, one round per term: this is an equality fingerprint, not a
            // digest, and the cost is paid on every frame of the route.
            h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        };
        // The playhead's three readings move the SCRUB BAR and the two clocks, which exist only
        // while the transport is up. Folding them in unconditionally would report motion for a
        // picture nobody is drawing — playback spends ~99% of its time with the HUD auto-hidden,
        // where the frame is already 0 draw calls, and a report there is the gate turned off.
        if hud_up {
            mix(pos as u64);
            mix(crate::player::seek_display_ns() as u64);
            mix(crate::player::TX.scrub_ns.load(Relaxed) as u64);
        }
        mix(u64::from(crate::player::TX.paused.load(Relaxed)));
        // …subtitles are NOT conditional: they are drawn on every frame of the route, HUD or no
        // HUD, which is exactly why a cue appearing had to become a report.
        mix(self.render.ass.fingerprint());
        mix(crate::player::subtitle_cue_id(pos) as u64);
        mix(crate::player::active_bitmap_key(pos).unwrap_or(0) as u64);
        mix(u64::from(hud_up));
        mix(u64::from(
            self.row
                .since_play_ms(now)
                .is_some_and(|d| d < crate::ui::player_hud::PLAY_MARK_MS),
        ));
        mix(match self.slot {
            ControlSlot::Discs => 1,
            ControlSlot::Skip(_) => 2,
            ControlSlot::UpNext(_) => 3,
        });
        mix(match crate::ui::player_hud::busy(ps) {
            crate::ui::player_hud::Busy::None => 1,
            crate::ui::player_hud::Busy::Transport => 2,
            // the caption is a `&'static CStr`: its ADDRESS is a stable identity for the message
            crate::ui::player_hud::Busy::Readout(k, c) => {
                4 ^ ((k as u64) << 32) ^ (c.as_ptr() as u64)
            }
        });
        h
    }

    /// Is the transport on screen right now?
    pub(crate) fn hud_up(&self, ps: &crate::route::PlaybackSession, now: u32) -> bool {
        self.hud.visible(ps, now, crate::player::TX.paused.load(std::sync::atomic::Ordering::Relaxed))
    }
}

/// The four focus groups [`PlayerScreen`] registers (restructure phase 12, D2): the scrubber, the
/// control row (discs, or the one region a Skip/Up Next stand-in occupies), the bottom tabs, and —
/// only while a failure owns the frame — the read-out's own recovery escape. Element addresses
/// within each are [`player_hud::ELEM_SCRUB`]/[`player_hud::ELEM_ROW_BASE`]/
/// [`player_hud::ELEM_TAB_BASE`]/[`player_hud::ELEM_FAILURE_OK`].
const GROUP_SCRUB: GroupId = GroupId(0);
const GROUP_ROW: GroupId = GroupId(1);
const GROUP_TABS: GroupId = GroupId(2);
const GROUP_FAILURE: GroupId = GroupId(3);
const GROUP_REPAIR: GroupId = GroupId(4);
const REPAIR_CANCEL: u32 = 40_000;
const REPAIR_CONFIRM: u32 = REPAIR_CANCEL + 1;
const REPAIR_BODY: &str = "Use Homebrew Channel’s root access to update PlxNative’s sandbox with LG’s native profile. This requires a rooted TV. Close and reopen PlxNative afterward.";

impl<H: PlayerLike + crate::screens::registry::MetadataLike> Machine<H> for PlayerScreen {
    type Ev = ScreenEvent<H>;
    /// **The transport's key/click ladder, as the receiving end of the phase-12 contract freeze.**
    ///
    /// `Tick`/`Unmount` are unchanged from before this phase; `Input` is this package's addition.
    /// A raw key is classified with [`consts::classify`] into the full [`consts::Key`] alphabet —
    /// never a type under `crate::app::`, which `ci/check-deps.sh`'s `layer` gate forbids `screens/`
    /// from naming — and dispatched to one of the private `key_*` helpers below, each a direct port
    /// of the `app/run.rs` arm it replaces (`key_ok`/`key_player_updown`/`key_scrub`'s Route::Player
    /// cases). A raw click's `hit` (filled by the dispatcher from this screen's own registered
    /// [`Stop`]s — see [`Screen::draw`]) is resolved the same way `PlayerOverlayScreen::step`
    /// resolves one. Anything privileged (pausing/resuming, seeking, presenting an overlay, leaving
    /// the player) is a `fx.push(Fx::App(AppFx::Player(PlayerReq::…)))` request, exactly as
    /// [`PlayerOverlayScreen::ask`] performs one — `PlayerReq::OpenOverlay`/`ArmControlRow`/`Exit`
    /// are the three variants this package added for it. `Exit`, the remote's own key, is the one
    /// this screen deliberately does NOT try to own (see the doc this replaced); it is not matched
    /// below and falls to `Handled::No`.
    ///
    /// **The scrub gesture has exactly ONE owner, and it is this screen** (restructure phase 12,
    /// PX-PLAYER). Phase 9 left the loop a CO-owner of `PlayerScreen::scrub`: `app/input.rs`'s
    /// `on_key_up`/`on_auto_repeat` wrote it on every key edge, and `app/run.rs` ran the continuous
    /// advance, the `SCRUB_LOST_MS` safety commit and the tap debounce on it once per frame. The
    /// three "documented simplifications" that stood in this doc were consequences of that split
    /// rather than choices, and all three are gone with the arms that forced them:
    /// 1. A plain LEFT/RIGHT hop commits on the `TAP_COMMIT_MS` DEBOUNCE again — armed by the
    ///    release, fired from this screen's own `Tick` — so a rapid burst is ONE seek and not one
    ///    per tap. (Each commit is a full reopen + prime on the engine, and back-to-back in-flight
    ///    seeks are what race the demux.) With the loop also arming that same field, the interim
    ///    behaviour was worse than either: one tap issued TWO seeks.
    /// 2. A committed scrub asks for [`PlayerReq::CommitSeek`], which holds a paused film paused,
    ///    rather than [`PlayerReq::SeekTo`], which resumes it. `App::repause_at` is still the
    ///    loop's — the screen states the INTENT and the loop performs it, which is what a request
    ///    is for.
    /// 3. The lost-keyup safety commit is reproduced, on the same `Tick` as the ramp it guards.
    ///
    /// The POINTER half is here for the same reason. A click on the scrubber's registered stop
    /// seeds the preview under the pointer, an `InputKind::Drag` carries it (§7.5: a drag drives
    /// its control with no hover and no click), and the button coming up — which reaches a screen
    /// as `Ok`/`Edge::Up`, the only release the dispatcher's `InputKind` has — commits it. Before
    /// this the stop landed on `key_ok`'s final `else` and toggled play/pause, and the drag flag
    /// was a field of the loop's pointer machine whose producer had become unreachable.
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if self.repair_alert.visible() {
            match ev {
                ScreenEvent::FocusMoved { to, .. } => {
                    self.repair_alert.set_choice(if to.elem == REPAIR_CONFIRM { crate::ui::decision_alert::Choice::Destructive } else { crate::ui::decision_alert::Choice::Cancel });
                    return Handled::Yes;
                }
                ScreenEvent::PressCommit(_) => {
                    if let Some(key) = cx.focus.current {
                        if matches!(key.elem, REPAIR_CANCEL | REPAIR_CONFIRM) {
                            self.repair_answer(key.elem == REPAIR_CONFIRM, fx);
                        }
                    }
                    return Handled::Yes;
                }
                ScreenEvent::Activate(_) => return Handled::Yes,
                ScreenEvent::Input(input) => {
                    if !self.repair_alert.is_open() { return Handled::Yes; }
                    return match input.kind {
                        InputKind::Key { sym, wcode, edge, .. } => match consts::classify(sym, wcode) {
                            consts::Key::Back | consts::Key::Stop if edge == Edge::Down => {
                                self.repair_answer(false, fx); Handled::Yes
                            }
                            consts::Key::Left { .. } | consts::Key::Right { .. } | consts::Key::Ok | consts::Key::Exit => Handled::No,
                            _ => Handled::Yes,
                        },
                        InputKind::Pointer { .. } | InputKind::Click { .. } => Handled::No,
                        _ => Handled::Yes,
                    };
                }
                _ => {}
            }
        }
        match ev {
            ScreenEvent::Tick(tick) => {
                self.render.ass.update(H::session(cx), tick.ms);
                if self.repair_alert.visible() && !H::session(cx).jail_load_blocked { self.repair_alert.close(); }
                self.repair_alert.update(tick.dt());
                // The control row's springs and the resume clock are stepped once per FRAME and
                // never from `draw_hud` — this row is not drawn on every frame of the route, so a
                // spring advanced in the draw would run at a rate that depended on which overlay
                // was open (`TransportRow::step`'s own doc).
                self.row
                    .step(self.slot, self.hud.nav.focus, self.hud.nav.btn, tick.dt(), tick.ms);
                // …and the Up Next countdown, which arms on APPEARANCE and is dropped with the
                // segment. Stepped for the whole route, not per-overlay: the countdown must keep
                // running whichever panel is up (`up_next::tick`).
                self.up_next.tick(self.slot, tick.ms);
                // The scrub gesture's whole per-frame half — `app/run.rs`'s old
                // per-loop-iteration block, now on this screen's own Tick: the continuous
                // accelerating advance with its lost-keyup safety commit, then the tap debounce.
                // In that order, because the safety commit disengages the gesture and a debounce
                // armed by a real release must not then be answered twice.
                self.step_scrub_hold(tick.ms, fx);
                self.step_tap_commit(tick.ms, fx);
                self.publish();
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                // The GL names in the subtitle set are OWNED. A static could never be told that a
                // playback had ended; an instance is told exactly once.
                self.render.subs.release();
                self.render.ass.release();
                Handled::Yes
            }
            ScreenEvent::Input(input) => {
                let ps = H::session(cx);
                match &input.kind {
                    InputKind::Key { sym, wcode, edge, .. } => {
                        self.handle_key(ps, consts::classify(*sym, *wcode), *edge, input.at.ms, fx, H::metadata(cx))
                    }
                    InputKind::Click { hit, x, .. } => {
                        self.handle_click(ps, *hit, *x, input.at.ms, fx)
                    }
                    // §7.5: a drag drives its CONTROL. Only the scrubber has one here, and only
                    // once a click has seated the gesture on it — a drag that began anywhere else
                    // must not capture the bar the moment it passes over it.
                    InputKind::Drag { x, .. } => self.handle_drag(*x, input.at.ms),
                    // Pointer MOTION over the bare transport raises it and clears a hand
                    // dismissal, which is how a Magic Remote user finds the HUD over full-screen
                    // video. `app/run.rs`'s own motion arm did this and then RETURNED, so the page
                    // never saw a pointer event at all.
                    InputKind::Pointer { .. } => {
                        self.hud.dismissed = false;
                        self.hud.extend(input.at.ms, input::HUD_LINGER_MS);
                        self.publish();
                        Handled::Yes
                    }
                    _ => Handled::No,
                }
            }
            _ => Handled::No,
        }
    }
}

impl PlayerScreen {
    fn ask<H: AppLike>(fx: &mut Effects<'_, H>, req: PlayerReq) {
        fx.push(Fx::App(AppFx::Player(req)));
    }

    fn repair_focus<H: AppLike>(fx: &mut Effects<'_, H>, group: GroupId) {
        fx.push(Fx::Deliver(
            fx.from(),
            crate::ui::machine::Delivery::Screen(ScreenEvent::Enter(crate::ui::screen::Enter::Fresh {
                focus: crate::ui::screen::FocusTarget::ContainerGroup(group),
            })),
        ));
    }
    fn failure_action<H: AppLike>(&mut self, ps: &crate::route::PlaybackSession, fx: &mut Effects<'_, H>) {
        if crate::player::error_now(ps).kind == crate::player::FailureKind::JailMissingRtkmem {
            if ps.repair_status == crate::webos::jail_repair::State::Idle && !self.repair_alert.visible() {
                self.repair_alert.open_with_body(c"Repair PlxNative’s sandbox?", REPAIR_BODY);
                Self::repair_focus(fx, GROUP_REPAIR);
            }
        } else {
            Self::ask(fx, PlayerReq::OpenOverlay(overlay::OverlayKind::More { quality: true }));
        }
    }
    fn repair_answer<H: AppLike>(&mut self, confirm: bool, fx: &mut Effects<'_, H>) {
        if !self.repair_alert.is_open() { return; }
        self.repair_alert.dismiss();
        if confirm { Self::ask(fx, PlayerReq::RepairSandbox); }
        Self::repair_focus(fx, GROUP_FAILURE);
    }
    fn repair_rect(&self, confirm: bool) -> Rect {
        self.repair_frames.get().map(|(cancel, repair)| if confirm { repair } else { cancel }).unwrap_or(Rect::FULL)
    }

    /// One registered click's element resolves to an action — the pointer twin of `handle_key`'s
    /// `Ok`/`Left`/`Right` arms.
    ///
    /// The scrubber's arm takes the click's own `x`, which is the whole difference from the phase-9
    /// version: a click on the bar is a SEEK TO THE POINTED POSITION and the opening of a possible
    /// drag, not the play/pause toggle it fell through to. Nothing is committed here — the commit
    /// is the button coming up ([`Self::handle_release`]), exactly as it was when `app/run.rs`
    /// owned this: a click with no motion is simply a drag of length zero, so one mechanism serves
    /// both and a click cannot commit a position the user never saw previewed.
    ///
    /// A click can only be resolved to an element the frame REGISTERED, and the transport's stops
    /// are registered only while it is drawn ([`Screen::draw`]) — which is the same protection the
    /// old ladder spelled out by sampling `hud_vis` before re-arming the HUD, after "a click in the
    /// invisible timed-out scrub band committed a blind seek".
    fn handle_click<H: AppLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        hit: Option<u32>,
        x: f32,
        now: u32,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        use player_hud::{ELEM_FAILURE_OK, ELEM_ROW_BASE, ELEM_SCRUB, ELEM_TAB_BASE};
        let failed = crate::ui::player_hud::transport_hidden(ps);
        let Some(elem) = hit else {
            // **A click that lands on no control at all toggles play/pause** — the `else` arm of
            // the old pointer block, and the reason clicking the PICTURE works: over full-screen
            // video most of the frame is not a control. Not while a failure owns it, where the old
            // block returned before reaching any of this: there is nothing to toggle and the one
            // drawn target is the read-out's own escape above.
            if failed {
                return Handled::Yes;
            }
            self.hud.dismissed = false;
            self.hud.extend(now, input::HUD_LINGER_MS);
            self.publish();
            Self::ask(fx, PlayerReq::Transport(None));
            return Handled::Yes;
        };
        // A click on a control is a fresh interaction with the transport, exactly as a key is: it
        // re-arms the linger. Without it a pointer that had rested still for most of the 4.5 s
        // could click a disc whose deferred press (~210 ms) then committed AFTER the auto-hide had
        // reset `hud.nav` to HOME — opening Subtitles for a click on Audio.
        self.hud.extend(now, input::HUD_LINGER_MS);
        self.publish();
        match elem {
            e if e == ELEM_FAILURE_OK => {
                self.failure_action(ps, fx);
                Handled::Yes
            }
            e if e == ELEM_SCRUB => {
                let dur = crate::player::duration_ns();
                if dur > 0 {
                    self.hud.nav.focus = 0;
                    self.scrub.ns = self.pointed_ns(x, dur);
                    self.scrub.drag = true;
                    self.scrub.commit_at = 0; // the release commits; no debounce owns this one
                    self.hud.extend(now, input::HUD_LINGER_MS);
                    self.publish();
                }
                Handled::Yes
            }
            e if (ELEM_ROW_BASE..ELEM_TAB_BASE).contains(&e) => {
                self.hud.nav.focus = 1;
                self.hud.nav.btn = (e - ELEM_ROW_BASE) as i32;
                Self::ask(fx, PlayerReq::ArmControlRow);
                Handled::Yes
            }
            e if (ELEM_TAB_BASE..ELEM_FAILURE_OK).contains(&e) => {
                let tab = e - ELEM_TAB_BASE;
                self.hud.nav.focus = 2;
                self.hud.nav.tab = tab as i32;
                let kind = if tab == 0 {
                    overlay::OverlayKind::Info
                } else {
                    overlay::OverlayKind::Chapters
                };
                Self::ask(fx, PlayerReq::OpenOverlay(kind));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    /// Where on the timeline a pointer at `x` is pointing, bounded by [`Scrub::clamp_target`].
    /// `scrub_frac_x` is x-only on purpose: an engaged drag tracks the pointer even when it
    /// wanders off the band vertically, which is why the drag arm below never re-tests the hit.
    fn pointed_ns(&self, x: f32, duration_ns: i64) -> i64 {
        let frac = player_hud::scrub_frac_x(x) as f64;
        Scrub::clamp_target((frac * duration_ns as f64) as i64, duration_ns)
    }

    /// **A pointer DRAG carries the preview.** Only while a click has seated the gesture on the
    /// bar (`Scrub::drag`): a drag that began on some other control, or on nothing, must not
    /// capture the scrubber the moment it passes over it. Nothing is committed — that is the
    /// release's job.
    fn handle_drag(&mut self, x: f32, now: u32) -> Handled {
        if !self.scrub.drag {
            return Handled::No;
        }
        let dur = crate::player::duration_ns();
        if dur > 0 {
            self.scrub.ns = self.pointed_ns(x, dur);
        }
        self.hud.dismissed = false;
        self.hud.extend(now, input::HUD_LINGER_MS);
        self.publish();
        Handled::Yes
    }

    /// **The pointer button coming up commits the drag it was previewing.**
    ///
    /// It arrives as `Ok`/`Edge::Up` because that is the only release `InputKind` has: the loop
    /// turns a `SDL_MOUSEBUTTONUP` into one (`bridge::release_input`, whose own doc carries the
    /// reasoning) and the dispatcher's press machine reads it the same way. A real OK key-up
    /// reaches here too and is inert, since nothing but a click on the bar ever sets `drag`.
    fn handle_release<H: AppLike>(&mut self, now: u32, fx: &mut Effects<'_, H>) {
        if !self.scrub.drag {
            return;
        }
        self.scrub.drag = false;
        if self.scrub.ns >= 0 {
            Self::ask(fx, PlayerReq::CommitSeek(self.scrub.ns));
            self.scrub.ns = -1;
        }
        self.scrub.disengage();
        self.hud.extend(now, input::HUD_LINGER_MS);
        self.publish();
    }

    /// Classify → dispatch, the one entry every `Input(Key)` event goes through.
    fn handle_key<H: AppLike>(
        &mut self,
        ps: &crate::route::PlaybackSession,
        key: consts::Key,
        edge: Edge,
        now: u32,
        fx: &mut Effects<'_, H>,
        meta: crate::metadata::MetadataView<'_>,
    ) -> Handled {
        use consts::Key;
        // **A terminal failure owns the whole frame, so almost nothing may be driven on it.**
        // `draw_hud` returns before painting anything under a `Failed` read-out, and a control
        // that is not drawn must not be activatable — two blind presses (DOWN, OK) once opened the
        // Info card over a read-out from a tab row nothing had painted. Port of `key_player_failed`,
        // whose loop arm this replaces; the panel-first half of its BACK is the container's now
        // (an open `…` popover is a surface and answers the key before this screen sees it).
        if crate::ui::player_hud::transport_hidden(ps) && !matches!(key, Key::Exit) {
            // …every key but EXIT, which is the remote's own and ends the PROCESS. It is not a
            // control on this frame, so "nothing that is not drawn may be driven" does not reach
            // it, and swallowing it would make a failed playback the one screen in the app an
            // EXIT press cannot leave (LG checklist item 38). It falls to `Handled::No` below and
            // the loop's own arm takes it, exactly as it does everywhere else.
            if edge == Edge::Down {
                match input::failed_key_action(
                    matches!(key, Key::Ok),
                    matches!(key, Key::Back | Key::Stop),
                ) {
                    input::FailedKeyAction::ChooseQuality => self.failure_action(ps, fx),
                    input::FailedKeyAction::Return => Self::ask(fx, PlayerReq::Exit),
                    input::FailedKeyAction::Ignore => {}
                }
            }
            // Swallowed either way: that is what "the read-out owns the frame" means.
            return Handled::Yes;
        }
        match key {
            Key::Play | Key::Pause | Key::PlayPause => {
                if edge == Edge::Down {
                    let play = match key {
                        Key::Play => Some(true),
                        Key::Pause => Some(false),
                        _ => None,
                    };
                    Self::ask(fx, PlayerReq::Transport(play));
                }
                Handled::Yes
            }
            // The STOP key and a plain BACK (nothing else open — an overlay's own BACK is answered
            // by `PlayerOverlayScreen::key` before this screen ever sees the press, since the
            // overlay is a SURFACE and owns input while it is up) both perform the same ritual
            // `key_back`/the Stop arm did.
            Key::Stop | Key::Back => {
                if edge == Edge::Down {
                    Self::ask(fx, PlayerReq::Exit);
                }
                Handled::Yes
            }
            Key::Up | Key::Down => {
                if edge == Edge::Down {
                    self.key_updown(key, now);
                }
                Handled::Yes
            }
            Key::Left { .. } | Key::Right { .. } => {
                match edge {
                    Edge::Down => self.key_scrub_fresh(ps, key, now, meta),
                    Edge::Repeat => self.key_scrub_repeat(now),
                    Edge::Up => self.key_scrub_release(now, fx),
                }
                Handled::Yes
            }
            Key::Ok => {
                match edge {
                    Edge::Down => self.key_ok(now, fx),
                    // …and the pointer button's own release, which the loop spells as this edge.
                    Edge::Up => self.handle_release(now, fx),
                    Edge::Repeat => {}
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }

    /// **OK, off a panel** — the deferred press for the control row (`PlayerReq::ArmControlRow`,
    /// which the loop's existing commit-frame dispatch resolves through `activate_player_row`), the
    /// immediate tabs-row overlay open, or — everywhere else, exactly as the old ladder's `else`
    /// arm — a plain play/pause toggle. Port of `key_ok`'s `Route::Player` arm.
    fn key_ok<H: AppLike>(&mut self, now: u32, fx: &mut Effects<'_, H>) {
        let vis = self.hud.visible_at_press;
        if vis && self.hud.nav.focus == 1 {
            Self::ask(fx, PlayerReq::ArmControlRow);
        } else if vis && self.hud.nav.focus == 2 {
            let kind = if self.hud.nav.tab == 0 {
                Some(overlay::OverlayKind::Info)
            } else if self.hud.nav.tab == 1 {
                Some(overlay::OverlayKind::Chapters)
            } else {
                None
            };
            if let Some(kind) = kind {
                Self::ask(fx, PlayerReq::OpenOverlay(kind));
            }
        } else {
            Self::ask(fx, PlayerReq::Transport(None));
        }
        self.hud.extend(now, input::HUD_LINGER_MS);
        self.publish();
    }

    /// Port of `key_player_updown`: walk the HUD's ring (scrubber ↔ control row ↔ tabs), or reveal
    /// a hidden transport and move nothing.
    fn key_updown(&mut self, key: consts::Key, now: u32) {
        let vis = self.hud.visible_at_press;
        let mut hide = false;
        if !vis {
            self.hud.nav.focus = 0;
        } else if matches!(key, consts::Key::Up) {
            match self.hud.nav.focus {
                0 => self.hud.nav.focus = 1,
                2 => self.hud.nav.focus = 0,
                _ => {
                    hide = true;
                    self.hud.nav.focus = 0;
                }
            }
        } else {
            match self.hud.nav.focus {
                0 => self.hud.nav.focus = 2,
                1 => self.hud.nav.focus = 0,
                _ => {}
            }
        }
        if self.hud.nav.focus != 0 || hide {
            self.scrub.ns = -1;
            self.scrub.disengage();
        }
        if hide {
            self.hud.dismissed = true;
        } else {
            self.hud.extend(now, input::HUD_LINGER_MS);
        }
    }

    /// A FRESH LEFT/RIGHT press: port of `key_scrub`'s `ScrubPress` dispatch. It commits NOTHING —
    /// the release does (`key_scrub_release`), on the debounce for a tap and at once for a hold.
    fn key_scrub_fresh(
        &mut self,
        ps: &crate::route::PlaybackSession,
        key: consts::Key,
        now: u32,
        meta: crate::metadata::MetadataView<'_>,
    ) {
        let fwd = matches!(key, consts::Key::Right { .. });
        let dur = crate::player::duration_ns();
        // A key gesture SUPERSEDES a pointer one, and throws its preview away rather than hopping
        // from it — `key_scrub`'s own first act, when the flag was `Pointer::drag`. The pointer's
        // release is never going to arrive as far as this gesture is concerned, so a preview left
        // standing would be committed later by a debounce that knows nothing about it.
        if self.scrub.drag {
            self.scrub.drag = false;
            self.scrub.ns = -1;
        }
        let act = input::scrub_press(self.hud.visible_at_press, self.hud.nav.focus, dur > 0);
        self.hud.extend(now, input::HUD_LINGER_MS);
        match act {
            input::ScrubPress::Reveal => {
                self.hud.nav.focus = 0;
                if dur > 0 {
                    self.scrub.begin(now, fwd);
                    self.scrub.reveal = true;
                    self.scrub.ns = crate::player::intended_pos_ns(ps);
                }
            }
            input::ScrubPress::Row => {
                self.hud.nav.btn =
                    (self.hud.nav.btn + if fwd { 1 } else { -1 }).clamp(0, self.slot.items() - 1);
            }
            input::ScrubPress::Tabs => {
                let max_tab = if crate::ui::chapters_panel::has_chapters(meta) { 1 } else { 0 };
                self.hud.nav.tab =
                    (self.hud.nav.tab + if fwd { 1 } else { -1 }).clamp(0, max_tab);
            }
            input::ScrubPress::Jump => {
                // scrubber focus, FRESH press: the fixed 10 s hop, and NOTHING committed. A held
                // key's repeats engage the continuous ramp below and the release commits; a tap's
                // release arms the debounce so that a rapid burst coalesces into one seek. The
                // commit belonging to the press was this package's first regression.
                self.scrub.commit_at = 0; // more input → cancel a pending tap commit
                self.scrub.alive = now;
                self.scrub.reveal = false; // a visible press is a real gesture whatever raised the HUD
                if self.scrub.dir == 0 && self.scrub.ns < 0 {
                    self.scrub.ns = crate::player::intended_pos_ns(ps);
                }
                if !self.scrub.hold {
                    let step = if fwd { input::SCRUB_STEP_NS } else { -input::SCRUB_STEP_NS };
                    self.scrub.ns = Scrub::clamp_target(self.scrub.ns.max(0) + step, dur);
                }
                self.scrub.dir = if fwd { 1 } else { -1 };
            }
            input::ScrubPress::Nothing => {}
        }
    }

    /// A hardware auto-repeat while the scrubber holds a direction: port of `on_auto_repeat`'s
    /// engage-only half — the continuous ramp's per-frame advance is `step_scrub_hold`, driven by
    /// `Tick` rather than by the repeat event itself (a held key's repeats and the frame rate are
    /// two different clocks).
    fn key_scrub_repeat(&mut self, now: u32) {
        if self.hud.nav.focus == 0 && self.scrub.dir != 0 {
            self.scrub.alive = now;
            self.scrub.commit_at = 0;
            if !self.scrub.hold {
                self.scrub.hold = true;
                self.scrub.hold_since = now;
                self.scrub.t = now;
                // `reveal` is deliberately NOT cleared here. Engaging the hold is not the same
                // event as the preview MOVING: `t` is stamped now, so the advance's first pass
                // computes `sdt ≈ 0` and travels nothing, and at ~10 s/s it takes ~100 ms before
                // the preview has moved even a second. A firm tap that trips one hardware repeat
                // and releases inside that window would otherwise commit a seek to the spot
                // playback is already sitting on — a full reopen + prime and a visible stall, out
                // of a press the reveal rule promises moves nothing. `step_scrub_hold` clears it
                // once there is real travel.
                crate::log("scrub: hold engaged (0x101 repeat)");
            }
        }
    }

    /// **The release**, port of `on_key_up`'s scrub half — the arm `app/input.rs` kept running on
    /// this very field after phase 9 had given the gesture to this screen.
    ///
    /// Three outcomes, and the ORDER of the first two is load-bearing. A reveal-only gesture
    /// throws the preview away: the press was spent raising the transport and the preview sits on
    /// the seed, so committing it is a full reopen + prime to the spot playback is already on.
    /// Tested BEFORE `hold`, because a hold that engaged but has not TRAVELLED yet is still that
    /// case — the ramp is what retires `reveal`, on real travel. A travelled hold commits now. A
    /// plain tap arms the debounce instead, so that a rapid ±10 s burst becomes one seek.
    fn key_scrub_release<H: AppLike>(&mut self, now: u32, fx: &mut Effects<'_, H>) {
        if self.scrub.dir == 0 {
            return;
        }
        if self.scrub.reveal {
            self.scrub.ns = -1;
            self.scrub.disengage();
        } else if self.scrub.hold {
            crate::log(&format!(
                "scrub: keyup commit (held) {}s",
                self.scrub.ns / 1_000_000_000
            ));
            self.commit_scrub(fx);
            self.scrub.disengage();
        } else {
            // a tap → commit on a short debounce so quick taps accumulate first
            self.scrub.commit_at = now.wrapping_add(input::TAP_COMMIT_MS).max(1);
        }
    }

    /// Ask the loop to perform the scrub, and retire the preview that asked for it.
    ///
    /// [`PlayerReq::CommitSeek`] and not [`PlayerReq::SeekTo`]: the scrub bar is how a PAUSED film
    /// is moved, and the loop's arm for it holds that pause across the seek. Clearing `ns` here
    /// rather than in the loop is the §2.3 half of the same change — `app::playback::commit_seek`
    /// used to reach into this gesture and blank it, which is a write to another owner's state.
    fn commit_scrub<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        if self.scrub.ns < 0 {
            return;
        }
        Self::ask(fx, PlayerReq::CommitSeek(self.scrub.ns));
        self.scrub.ns = -1;
    }

    /// **The tap DEBOUNCE**, fired from `Tick` — port of `app/run.rs`'s per-frame block.
    ///
    /// `commit_at` is an absolute instant and `0` means none; the wrapping comparison is the
    /// loop's own, so a `SDL_GetTicks` wrap cannot fire every frame for 24 days. A pending commit
    /// whose preview was thrown away in the meantime simply retires.
    fn step_tap_commit<H: AppLike>(&mut self, now: u32, fx: &mut Effects<'_, H>) {
        if self.scrub.commit_at == 0
            || now.wrapping_sub(self.scrub.commit_at) >= 0x8000_0000
        {
            return;
        }
        if self.scrub.ns >= 0 {
            crate::log(&format!(
                "scrub: tap commit {}s",
                self.scrub.ns / 1_000_000_000
            ));
            self.commit_scrub(fx);
        }
        self.scrub.ns = -1;
        self.scrub.disengage();
        self.scrub.commit_at = 0;
    }

    /// The continuous accelerating scrub's per-frame advance while a KEY is held, and the
    /// lost-keyup safety commit that bounds it — port of `app/run.rs`'s per-loop-iteration block,
    /// formula for formula.
    ///
    /// Skipped while a pointer drag owns the preview: the hand says where it is, and an advance
    /// underneath it would fight the pointer. That `!dragging` term was the loop's too, read off
    /// `app::input::Pointer::drag`; the flag moved onto the gesture with the gesture.
    ///
    /// **The safety commit is not optional.** This remote emits a held key as auto-repeat keydowns
    /// followed by ONE keyup, and a dropped keyup leaves `hold` armed with the preview slewing at
    /// up to `SCRUB_MAX` for as long as the page is up. `alive` is stamped by every repeat, so
    /// `SCRUB_LOST_MS` without one is the release that never arrived.
    fn step_scrub_hold<H: AppLike>(&mut self, now: u32, fx: &mut Effects<'_, H>) {
        if self.scrub.dir == 0 || !self.scrub.hold || self.scrub.ns < 0 || self.scrub.drag {
            return;
        }
        let held = now.wrapping_sub(self.scrub.hold_since) as f32 / 1000.0;
        let speed = (input::SCRUB_BASE + input::SCRUB_ACCEL * held).min(input::SCRUB_MAX);
        let mut sdt = now.wrapping_sub(self.scrub.t) as f32 / 1000.0;
        if sdt > 0.1 {
            sdt = 0.1;
        }
        let was = self.scrub.ns;
        let dur = crate::player::duration_ns();
        let s = Scrub::clamp_target(
            was + (self.scrub.dir as f64 * speed as f64 * sdt as f64 * 1e9) as i64,
            dur,
        );
        self.scrub.ns = s;
        // Real travel is what turns a reveal into a scrub — not the hold edge, which fires a beat
        // earlier with nothing moved yet. Once the preview has left the seed the release commits
        // like any other held gesture.
        if s != was {
            self.scrub.reveal = false;
        }
        self.hud.extend(now, input::HUD_LINGER_MS);
        self.scrub.t = now;
        if now.wrapping_sub(self.scrub.alive) > input::SCRUB_LOST_MS {
            crate::log(&format!("scrub: lost keyup commit {}s", s / 1_000_000_000));
            self.commit_scrub(fx);
            self.scrub.disengage();
        }
    }
}

/// **The four registered regions, as real `Focusable` groups** (restructure phase 12, D2): the
/// scrubber, the control row, the bottom tabs and — only while it is drawn — the failure read-out's
/// own escape. `neighbour` still declines the engine's own geometric stepping (`Step::Edge`
/// unconditionally): every direction this screen cares about is intercepted first by its own
/// `Machine::step` (`ui/dispatch.rs`'s `handled == Handled::No && engine` guard never sees one this
/// screen answers `Handled::Yes` for), so `HudNav` stays the single source of truth for WHICH of
/// these is highlighted — these groups exist so the engine's hit map and stop bookkeeping have real
/// geometry to test a click or a simulator mouse against, not so the engine drives the ring itself.
///
/// **The order `groups` pushes in is the pointer's z-order**, lowest first: `record_stops`
/// registers in it, and the hit map resolves the last stop under the pointer. The scrubber's tall
/// grab band therefore comes FIRST, under the control row and tabs it overlaps (issue #162).
impl<H: PlayerLike + crate::screens::registry::MetadataLike> Focusable<H> for PlayerScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.repair_alert.visible() {
            out.push(GroupSpec { id: GROUP_REPAIR, kind: GroupKind::Row { wrap: false }, seat: Seat::First,
                reachable: AxisMask::BOTH, edge: [EdgeRule::Stop; 4], extent: self.repair_rect(false).union(self.repair_rect(true)), len: 2, elem: crate::ui::screen::ElemKind::Control });
            return;
        }
        out.push(GroupSpec {
            id: GROUP_SCRUB,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Screen; 4],
            extent: player_hud::scrub_hit_rect(),
            len: 1,
            elem: crate::ui::screen::ElemKind::Control,
        });
        // Every item of whatever occupies the row — the Up Next PAIR included, which is two
        // controls the D-pad walks and so two stops (`ControlSlot::item_rect`).
        let row_len = self.slot.items().max(1) as usize;
        out.push(GroupSpec {
            id: GROUP_ROW,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Screen; 4],
            extent: player_hud::ctrl_row_hit_rect(),
            len: row_len,
            elem: crate::ui::screen::ElemKind::Control,
        });
        let has_ch = crate::ui::chapters_panel::has_chapters(H::metadata(cx));
        out.push(GroupSpec {
            id: GROUP_TABS,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Screen; 4],
            extent: player_hud::tab_hit_rect(0, has_ch).unwrap_or(Rect::FULL),
            len: if has_ch { 2 } else { 1 },
            elem: crate::ui::screen::ElemKind::Control,
        });
        if matches!(
            self.busy,
            crate::ui::player_hud::Busy::Readout(crate::ui::widgets::StatusKind::Failed, _)
        ) {
            out.push(GroupSpec {
                id: GROUP_FAILURE,
                kind: GroupKind::Free,
                seat: Seat::First,
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Screen; 4],
                extent: player_hud::failure_ok_hit_rect(),
                len: 1,
                elem: crate::ui::screen::ElemKind::Control,
            });
        }
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        use player_hud::{ELEM_FAILURE_OK, ELEM_ROW_BASE, ELEM_SCRUB, ELEM_TAB_BASE};
        if self.repair_alert.visible() {
            return matches!(*key, REPAIR_CANCEL | REPAIR_CONFIRM).then_some(GROUP_REPAIR);
        }
        match *key {
            e if e == ELEM_SCRUB => Some(GROUP_SCRUB),
            e if (ELEM_ROW_BASE..ELEM_TAB_BASE).contains(&e) => Some(GROUP_ROW),
            e if (ELEM_TAB_BASE..ELEM_FAILURE_OK).contains(&e) => Some(GROUP_TABS),
            e if e == ELEM_FAILURE_OK => Some(GROUP_FAILURE),
            _ => None,
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        if self.repair_alert.is_open() {
            let elem = match (key.elem, dir) {
                (REPAIR_CANCEL, Dir::Right) => REPAIR_CONFIRM,
                (REPAIR_CONFIRM, Dir::Left) => REPAIR_CANCEL,
                _ => return Step::Edge,
            };
            return Step::Move(FocusKey { entry: self.entry, elem });
        }
        Step::Edge
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        use player_hud::{ELEM_FAILURE_OK, ELEM_ROW_BASE, ELEM_SCRUB, ELEM_TAB_BASE};
        if self.repair_alert.visible() {
            if !matches!(*key, REPAIR_CANCEL | REPAIR_CONFIRM) { return None; }
            let rect = self.repair_rect(*key == REPAIR_CONFIRM);
            return Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: None });
        }
        let rect = match *key {
            e if e == ELEM_SCRUB => player_hud::scrub_hit_rect(),
            e if (ELEM_ROW_BASE..ELEM_TAB_BASE).contains(&e) => {
                self.slot.item_rect(&self.row, (e - ELEM_ROW_BASE) as i32, cx.measure)?
            }
            e if (ELEM_TAB_BASE..ELEM_FAILURE_OK).contains(&e) => player_hud::tab_hit_rect(
                (e - ELEM_TAB_BASE) as i32,
                crate::ui::chapters_panel::has_chapters(H::metadata(cx)),
            )?,
            e if e == ELEM_FAILURE_OK => player_hud::failure_ok_hit_rect(),
            _ => return None,
        };
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: None })
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        want
    }
    fn seat(&self, g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        let elem = match g {
            GROUP_SCRUB => player_hud::ELEM_SCRUB,
            GROUP_ROW => player_hud::ELEM_ROW_BASE,
            GROUP_TABS => player_hud::ELEM_TAB_BASE,
            GROUP_FAILURE => player_hud::ELEM_FAILURE_OK,
            GROUP_REPAIR => REPAIR_CANCEL,
            _ => player_hud::ELEM_SCRUB,
        };
        FocusKey { entry: self.entry, elem }
    }
}

impl LogicalState for PlayerScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.hud.nav.focus as u32)
            .u32(self.hud.nav.btn as u32)
            .u32(self.hud.nav.tab as u32)
            .u32(self.hud.until)
            .bool(self.hud.dismissed)
            .bool(self.hud.visible_at_press)
            .bool(self.hud.was_standin);
        c.option(self.hud.last_offer.as_ref(), |c, (kind, at)| {
            c.u32(*kind as u32).u64(*at as u64);
        });
        c.u32(self.scrub.dir as u32)
            .bool(self.scrub.hold)
            .bool(self.scrub.reveal)
            .bool(self.scrub.drag)
            .u64(self.scrub.ns as u64)
            .u32(self.scrub.commit_at);
        c.option(self.origin.as_ref(), |c, o| {
            c.u32(o.entry.0);
        });
        c.bool(self.repair_alert.is_open()).bool(self.repair_alert.choice() == crate::ui::decision_alert::Choice::Destructive);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(WORD);
    }
}

impl<H: PlayerLike + crate::screens::registry::MetadataLike> Screen<H> for PlayerScreen {
    fn name(&self) -> &'static str {
        WORD
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        // The frame's publication of the playback session (spec §2.3) — see `AppViews::session`.
        let ps = H::session(f.cx);
        let now = f.cx.tick.ms;
        let hud_up = self.hud_up(ps, now);
        // Both subtitle paths lift clear of the transport for the same reason and by the same
        // test — an open track menu counts, since that is exactly when the user is reading the
        // bottom of the screen. `transport` is false while the Info card or Chapters strip owns
        // the middle, and the lift follows the panel rather than the transport there.
        let subs_lift = hud_up || self.lifted;
        self.render.ass.draw();
        if let Some(message) = self.render.ass.error() {
            crate::ui::player_hud::draw_subtitle_message(message, subs_lift);
        }
        self.draw_subtitle_bitmap(subs_lift); // PGS/VobSub image subs
        crate::ui::player_hud::draw_subtitles(subs_lift, crate::route::is_transcoding(ps));
        // What a pointer can hit is registered AFTER the paint, by `record_stops`, from this
        // screen's own `Focusable` — the rects D-pad focus uses, in the z-order `groups` states.
        // `Hover::Ignore` on the transport's stops: the old pointer path never followed the mouse
        // into a HOVER-driven focus change on this route, and a DRAG is `§7.5`'s own case (no
        // hover, no click) which the hit map answers whatever this says.
        //
        // `Activate::Direct` on them, and it is not the same statement `ElemKind` makes
        // elsewhere: the transport's control row DOES dip, but its press is the LOOP's
        // (`PlayerReq::ArmControlRow` → `press.begin_ctl` → `activate_player_row` on the
        // spring-back), so letting the dispatcher's own press machine arm it instead would leave
        // that commit-frame dispatch with nothing to fire. Direct delivery, and the screen decides
        // what the element means — including the scrubber, whose meaning needs the click's `x`
        // and so cannot be carried by a bare `ScreenEvent::Activate` at all.
        let hud_drawn = (hud_up || self.lifted) && !self.repair_alert.visible();
        if hud_drawn {
            self.draw_hud(ps, now, f.measure, H::metadata(f.cx));
        }
        // The read-out is NOT transport chrome — it is drawn whether or not the HUD is up, so a
        // terminal `Error` (which is not `is_busy()`, so it does not pin the HUD) keeps its message
        // instead of vanishing with the 4.5 s linger. AFTER the transport, so it is never dimmed by
        // the scrim; BEFORE the overlay panels, which the container draws above this page.
        crate::ui::player_hud::draw_readout(ps, self.busy, now, f.measure);
        if self.repair_alert.visible() {
            self.repair_alert.draw_scrim();
            self.repair_alert.draw(c"Cancel", c"Repair");
            let frames = self.repair_alert.frames();
            self.repair_frames.set(Some(frames));
        }
        self.record_stops(f, hud_drawn);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::VideoPlane
    }
    /// The PGS/VobSub display set is the only render this screen owns (§8.3 rule (c)): the picture
    /// is the hardware video plane, the HUD is drawn immediate-mode, and its glyphs and icons come
    /// from the shared caches. `SubtitleBitmaps` uploads one texture per rect of the active cue and
    /// deletes them itself, so it is the one thing on this screen whose bytes belong to this
    /// instance rather than to a pool.
    fn render_report(&self) -> crate::ui::frame::RenderReport {
        let bitmap = self.render.subs.render_report();
        let ass = self.render.ass.render_report();
        crate::ui::frame::RenderReport { textures: bitmap.textures + ass.textures, bytes: bitmap.bytes + ass.bytes }
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

/// **The player page's clock-driven animators, one test per CLASS** (spec §8.3, §9, phase 9).
///
/// Until phase 9 the loop's gate read `idle::should_present(now) || fr.player`, so nothing drawn
/// on this route ever had to report motion: the ROUTE presented unconditionally. The gate is keyed
/// on the video plane being BOUND now, which leaves two windows where the player page is on screen
/// and the plane is not under it — the pre-bind spinner and everything after an unbind — and in
/// those windows an unreported animator is a FROZEN one. `Xfade::tick` and `Spinner::draw` each
/// shipped exactly that way before they were taught to report.
///
/// Each test below drives ONE class through the whole path the loop uses — the fingerprint, the
/// machine's comparison, the live gate — with the plane UNBOUND, and asserts the frame presents.
///
/// **Observed RED, simulated in every case** (the fix changes the signatures the old code called,
/// so these cannot be compiled against 88841d3e): deleting that class's `mix(...)` term from
/// `PlayerScreen::clock_fingerprint` — which is precisely "this animator does not report" — makes
/// the fingerprint identical across the change and the assertion fails at "…must present".
#[cfg(test)]
mod clock_animator_tests {
    use super::*;
    use crate::player::machine::Player;
    use crate::route::PlaybackSession;

    /// One iteration of the loop's motion report, with the plane UNBOUND — the fingerprint and
    /// the machine's comparison, exactly as `app::run`'s prepare phase runs them. `true` = THIS
    /// path raised damage, i.e. this frame is owed a present.
    ///
    /// Graded on `idle::take_local_damage`, which counts what this THREAD reported, rather than on
    /// `should_present`, which reads the process-wide `DIRTY`/`DAMAGE_GEN` that every unlocked
    /// test repainting anything also moves — the flake `ui::idle`'s own `LOCAL_DAMAGE` doc
    /// describes, and which cost this test two red runs in six before it was written this way.
    /// The gate's own half is asserted once, below, where it cannot be made false by another
    /// thread: a spurious `invalidate` elsewhere can only make `should_present` MORE true.
    fn reported(pl: &mut Player, screen: &PlayerScreen, ps: &PlaybackSession, now: u32) -> bool {
        let _ = crate::ui::idle::take_local_damage();
        if pl.note_clock(screen.clock_fingerprint(ps, now)) {
            crate::ui::idle::invalidate();
        }
        crate::ui::idle::take_local_damage() > 0
    }

    /// Run the page to a standstill, so that a report below can only be this test's own doing.
    fn settled(pl: &mut Player, screen: &PlayerScreen, ps: &PlaybackSession, now: u32) {
        crate::ui::idle::set_enabled(true);
        crate::ui::idle::frame_begin(1.0 / 60.0);
        for t in 0..4 {
            reported(pl, screen, ps, now + t);
        }
        assert!(
            !reported(pl, screen, ps, now + 4),
            "the fixture must start from a page that reports nothing, or what follows proves \
             nothing",
        );
    }

    /// The other half, asserted once per test: a report really does open the gate.
    fn presents(now: u32) -> bool {
        crate::ui::idle::should_present(now)
    }

    /// **The MEDIA CLOCK class** — the scrub bar, the playhead, both clocks, the hold-to-scrub
    /// ramp. Everything derived from `playpos_ns`, which LG's media thread writes and no spring
    /// can see.
    ///
    /// With the transport UP, which is the only state in which any of it is drawn: the fingerprint
    /// deliberately drops the playhead's readings while the HUD is hidden, since a report for a
    /// picture nobody draws is the gate turned off.
    #[test]
    fn a_moving_playhead_presents_a_frame_while_the_plane_is_unbound() {
        let _g = crate::testlock::serial();
        let ps = PlaybackSession::IDLE;
        let mut pl = Player::new();
        let mut screen = PlayerScreen::new(crate::ui::machine::EntryId(1));
        screen.hud.extend(1_000, 60_000);
        assert!(screen.hud_up(&ps, 1_005), "the fixture: the scrub bar is on screen");
        assert!(!pl.video_plane_bound, "the window this test is about");

        crate::player::SHARED.playpos_ns.store(0, std::sync::atomic::Ordering::Relaxed);
        settled(&mut pl, &screen, &ps, 1_000);
        crate::player::SHARED
            .playpos_ns
            .store(500_000_000, std::sync::atomic::Ordering::Relaxed);
        assert!(
            reported(&mut pl, &screen, &ps, 1_005),
            "the playhead moved half a second: the scrub bar and both clocks must report",
        );
        assert!(presents(1_005), "…and a report is a frame");
        crate::player::SHARED.playpos_ns.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// **The DEADLINE class** — the HUD's auto-hide. At `now == until` the whole transport leaves
    /// the screen, from a countdown, with nothing else in the frame moving at all.
    #[test]
    fn the_huds_auto_hide_deadline_presents_the_frame_it_expires_on() {
        let _g = crate::testlock::serial();
        let ps = PlaybackSession::IDLE;
        let mut pl = Player::new();
        let mut screen = PlayerScreen::new(crate::ui::machine::EntryId(1));
        screen.hud.extend(2_000, 500); // on screen until 2_500

        assert!(screen.hud_up(&ps, 2_400), "the fixture: still up before the deadline");
        settled(&mut pl, &screen, &ps, 2_100);
        assert!(!screen.hud_up(&ps, 2_600), "…and gone after it");
        assert!(
            reported(&mut pl, &screen, &ps, 2_600),
            "the transport disappeared on this frame: it must report",
        );
        assert!(presents(2_600), "…and a report is a frame");
    }

    /// **The TRANSPORT MARK class** — the two-second `Play` glyph, whose own doc used to record
    /// that no report was owed for it because the player route presented unconditionally.
    #[test]
    fn the_play_marks_two_second_expiry_presents_a_frame() {
        let _g = crate::testlock::serial();
        let ps = PlaybackSession::IDLE;
        let mut pl = Player::new();
        let mut screen = PlayerScreen::new(crate::ui::machine::EntryId(1));
        // A resume edge at t=5s. Stamped directly rather than derived from `player::TX`, which
        // every playback test in the crate moves: what is under test is the CLOCK, not the
        // observer that reads the transport.
        screen.row.force_play_at_for_test(5_000);
        assert!(
            screen.row.since_play_ms(5_100).is_some_and(|d| d < crate::ui::player_hud::PLAY_MARK_MS),
            "the fixture: the Play mark is on screen",
        );

        settled(&mut pl, &screen, &ps, 5_100);
        let after = 5_000 + crate::ui::player_hud::PLAY_MARK_MS + 1;
        assert!(
            reported(&mut pl, &screen, &ps, after),
            "the Play mark stopped being drawn on this frame: it must report",
        );
        assert!(presents(after), "…and a report is a frame");
    }

    /// **The READ-OUT class** — `busy`, i.e. the centred status surface appearing and vanishing.
    /// The `Playing -> Error` edge with the HUD auto-hidden is the one that matters: nothing else
    /// in that frame moves, and what the viewer would otherwise keep looking at is a black screen.
    #[test]
    fn a_read_out_appearing_presents_a_frame_while_the_plane_is_unbound() {
        let _g = crate::testlock::serial();
        let ps = PlaybackSession::IDLE;
        let mut pl = Player::new();
        let mut screen = PlayerScreen::new(crate::ui::machine::EntryId(1));
        screen.busy = crate::ui::player_hud::Busy::None;

        settled(&mut pl, &screen, &ps, 9_000);
        // The fingerprint reads `player_hud::busy(ps)` for itself rather than this field, so the
        // state has to move where the read-out reads it — the pipeline state the pump publishes.
        crate::player::SHARED.pb_state.store(
            crate::player::PlaybackState::Error as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        assert!(
            reported(&mut pl, &screen, &ps, 9_005),
            "the failure read-out took the screen on this frame: it must report",
        );
        assert!(presents(9_005), "…and a report is a frame");
        crate::player::SHARED.pb_state.store(
            crate::player::PlaybackState::Idle as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
mod render_residency_tests {
    use super::*;
    use crate::ui::frame::RenderReport;
    use crate::ui::player_hud::SubtitleBitmaps;

    /// **The player's own render is its image-subtitle display set, and only that** (§8.3 rule
    /// (c)). The picture is the hardware video plane, which is not ours at all; the HUD is drawn
    /// immediate-mode from the shared glyph and icon caches. A screen that owns a texture has to
    /// SAY so, or the frame's byte term is a sum over an empty inventory — which is what
    /// `Screen::render_bytes`'s single `{ 0 }` default and zero overrides made it until phase 11.
    #[test]
    fn the_player_reports_its_image_subtitle_set_and_nothing_else() {
        let mut s = PlayerScreen::new(EntryId(1));
        assert_eq!(
            Screen::<crate::screens::player::overlay_tests::TestHost>::render_report(&s),
            RenderReport::NONE,
            "no image cue up: this screen holds no render of its own",
        );
        s.render.subs = SubtitleBitmaps::stub(&[(720, 120)]);
        assert_eq!(
            Screen::<crate::screens::player::overlay_tests::TestHost>::render_report(&s),
            RenderReport::one(720, 120),
            "the cue on screen is one texture and its pixels",
        );
    }
}

/// **The transport ladder, ported onto `PlayerScreen::step`** (restructure phase 12, D2 Part C).
///
/// Each case here was watched RED first against `88841d3e` (the contract-freeze commit, whose
/// `Input` arm was a stub returning `Handled::No` unconditionally) before this package's `step`
/// body was written — the same shape `overlay_tests.rs`'s own suite already uses for this screen's
/// sibling, and the reason both suites share `overlay_tests::TestHost`/`cx` rather than each
/// inventing a fixture.
///
/// **Every case holds `testlock::serial()` since phase 12 (PX-PLAYER), and none did before.**
/// `handle_key`'s first test is now `player_hud::transport_hidden`, which reads the crate-global
/// playback state — so a case in another module driving that state to `Error` turns every key
/// here into the failure read-out's policy. It was not hypothetical: the failure-escape case in
/// `scrub_ownership_tests` below does exactly that, and two cases here failed on the first run
/// after it was written, in the shape `docs/agent-reference.md` describes — plausible wrong data
/// rather than a clean failure (`ok_elsewhere_toggles_play_pause` asked for the quality ladder).
/// A screen that reads a crate global from its input path owes the lock to every test that drives
/// that path.
#[cfg(test)]
mod step_ladder_tests {
    use super::*;
    use crate::screens::player::overlay_tests::TestHost;
    use crate::screens::registry::AppFx;
    use crate::ui::consts::{
        SDLK_DOWN, SDLK_RETURN, SDLK_UP, WCODE_BACK, WCODE_PAUSE, WCODE_PLAY, WCODE_PLAYPAUSE,
        WCODE_STOP,
    };
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{
        Cx, Edge, Effects, EntryId, Fx, Handled, InputEvent, InputKind, InputOwner, MachineId,
        Source, Tick,
    };

    const ENTRY: EntryId = EntryId(9);
    const INST: InstanceId = InstanceId(3);

    fn cx() -> Cx<'static, TestHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &FixtureMeasure,
            focus: Default::default(),
            press: Default::default(),
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// One key press through `PlayerScreen::step`, returning `(handled, the PlayerReqs it asked
    /// for)` — the same shape `overlay_tests.rs::press` returns for the sibling surface.
    fn press(page: &mut PlayerScreen, sym: u32, wcode: u32, edge: Edge) -> (Handled, Vec<PlayerReq>) {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let ev = ScreenEvent::<TestHost>::Input(InputEvent {
            kind: InputKind::Key {
                key: crate::ui::machine::Key::Other,
                sym,
                wcode,
                edge,
                at_edge: false,
            },
            at: Tick { ms: 1_000, dt_us: 0 },
            source: Source::Sdl,
        });
        let handled = page.step(
            &ev,
            &cx(),
            &mut Effects::new(&mut out, MachineId::Instance(INST), &mut present),
        );
        let reqs = out
            .into_iter()
            .filter_map(|e| match e.fx {
                Fx::App(AppFx::Player(req)) => Some(req),
                _ => None,
            })
            .collect();
        (handled, reqs)
    }

    fn click(page: &mut PlayerScreen, elem: u32) -> (Handled, Vec<PlayerReq>) {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let ev = ScreenEvent::<TestHost>::Input(InputEvent {
            kind: InputKind::Click { x: 0.0, y: 0.0, hit: Some(elem) },
            at: Tick { ms: 1_000, dt_us: 0 },
            source: Source::Sdl,
        });
        let handled = page.step(
            &ev,
            &cx(),
            &mut Effects::new(&mut out, MachineId::Instance(INST), &mut present),
        );
        let reqs = out
            .into_iter()
            .filter_map(|e| match e.fx {
                Fx::App(AppFx::Player(req)) => Some(req),
                _ => None,
            })
            .collect();
        (handled, reqs)
    }

    fn fresh(page: &mut PlayerScreen) {
        page.hud.visible_at_press = true;
    }

    /// The census's other half: a control the frame does NOT draw registers no stop, so a click
    /// there reaches the picture (play/pause) rather than a blind action. A hidden HUD registers
    /// nothing; with an Info card or Chapters strip over the middle (`transport == false`) only
    /// the tabs remain.
    #[test]
    fn a_control_the_frame_does_not_draw_registers_no_stop() {
        use crate::ui::player_hud::ELEM_TAB_BASE;
        use crate::ui::screen::DrawFrame;
        let _g = crate::testlock::serial();
        for (transport, hud_drawn, want) in [
            (true, false, vec![]),
            (false, false, vec![]),
            (false, true, vec![ELEM_TAB_BASE]),
        ] {
            let mut page = PlayerScreen::new(ENTRY);
            page.transport = transport;
            let cx = cx();
            let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
            page.record_stops(&mut f, hud_drawn);
            let got: Vec<u32> = f.into_stops().iter().map(|s| s.key.elem).collect();
            assert_eq!(got, want, "transport={transport} hud_drawn={hud_drawn}");
        }
    }

    /// **Issue #162's census: every control the D-pad can activate is clickable with the Magic
    /// Remote pointer, all over its drawn rect, and the click acts.**
    ///
    /// The element lists below are written out per state rather than read back from `groups`, so
    /// a control the D-pad reaches but the registration forgets is a failure here, not a shorter
    /// list. Each is clicked on a grid (`ui::hit::pointer_gaps`) against the map the real
    /// `record_stops` filled — which is what caught the scrubber's grab band registered ABOVE the
    /// control row (every disc's middle seeked) and the stand-ins' single floor-width region
    /// (*Watch Credits* unclickable, *Next Episode* activating item 0). The scrubber itself is
    /// judged at a point no control covers, since controls sit on top of its band by design.
    #[test]
    fn every_control_the_dpad_reaches_is_clickable_with_the_pointer_and_acts() {
        use crate::metadata::{Marker, MarkerKind};
        use crate::screens::registry::PlayerLike;
        use crate::ui::hit::{pointer_gaps, HitMap, PointerKind};
        use crate::ui::machine::FocusKey;
        use crate::ui::player_hud::{Busy, ControlSlot, ELEM_FAILURE_OK, ELEM_ROW_BASE, ELEM_SCRUB, ELEM_TAB_BASE};
        use crate::ui::screen::{At, DrawFrame};
        let _g = crate::testlock::serial();
        let marker = |kind| Marker { kind, start_ms: 1_000, end_ms: 2_000, final_seg: false };
        let skip = player_hud::slot_for(Some(marker(MarkerKind::Intro)), false);
        let up_next = player_hud::slot_for(Some(marker(MarkerKind::Credits)), true);
        assert!(matches!(skip, ControlSlot::Skip(_)) && matches!(up_next, ControlSlot::UpNext(_)));
        let failed = Busy::Readout(crate::ui::widgets::StatusKind::Failed, c"Playback failed");
        let row = |n: u32| (0..n).map(|i| ELEM_ROW_BASE + i).collect::<Vec<_>>();
        let cases: [(&str, ControlSlot, Busy, Vec<u32>); 4] = [
            ("discs", ControlSlot::Discs, Busy::None, [row(3), vec![ELEM_TAB_BASE]].concat()),
            ("skip", skip, Busy::None, [row(1), vec![ELEM_TAB_BASE]].concat()),
            ("up next", up_next, Busy::None, [row(2), vec![ELEM_TAB_BASE]].concat()),
            ("failed", ControlSlot::Discs, failed, vec![ELEM_FAILURE_OK]),
        ];
        for (name, slot, busy, controls) in cases {
            let mut page = PlayerScreen::new(ENTRY);
            page.slot = slot;
            page.busy = busy;
            page.transport = true;
            let cx = cx();
            let mut f = DrawFrame::new(&cx, crate::ui::Painter::root());
            page.record_stops(&mut f, true);
            let mut map = HitMap::new();
            map.fill(f.into_stops());
            map.swap();
            let placed: Vec<_> = controls
                .iter()
                .map(|&elem| {
                    let p = Focusable::<TestHost>::place(&page, &elem, &cx, At::Drawn)
                        .unwrap_or_else(|| panic!("{name}: {elem} is a control the D-pad reaches"));
                    (FocusKey { entry: ENTRY, elem }, p.rect)
                })
                .collect();
            let gaps = pointer_gaps(&mut map, ENTRY, &placed);
            assert!(gaps.is_empty(), "{name}: controls the pointer cannot click:\n{}", gaps.join("\n"));
            if !player_hud::failure_ok_drawn(TestHost::session(&cx), busy) {
                let band = player_hud::scrub_hit_rect();
                let hit = map.resolve(Some(ENTRY), PointerKind::Click, band.cx(), band.cy(), None);
                assert_eq!(hit.activate.map(|(k, _)| k.elem), Some(ELEM_SCRUB), "{name}: the scrubber");
            }
            // …and a click on each one ACTS, never falling through to the picture's play/pause.
            for &elem in &controls {
                let (handled, reqs) = click(&mut page, elem);
                assert_eq!(handled, Handled::Yes, "{name}: {elem}");
                assert!(!reqs.is_empty(), "{name}: a click on {elem} asked for nothing");
                assert!(!reqs.iter().any(|r| matches!(r, PlayerReq::Transport(_))), "{name}: {elem} -> {reqs:?}");
                assert!(page.hud.until >= 1_000 + input::HUD_LINGER_MS, "{name}: a click re-arms the linger");
                if (ELEM_ROW_BASE..ELEM_TAB_BASE).contains(&elem) {
                    assert_eq!(page.hud.nav.btn, (elem - ELEM_ROW_BASE) as i32, "{name}: the CLICKED item arms");
                    assert_eq!(reqs, vec![PlayerReq::ArmControlRow]);
                }
                if elem == ELEM_TAB_BASE {
                    assert_eq!(reqs, vec![PlayerReq::OpenOverlay(overlay::OverlayKind::Info)], "{name}: Info");
                }
            }
        }
    }

    /// PAUSE/PLAY/PLAYPAUSE all reach the shared `Transport` request unchanged — the port of
    /// `key_pause`/`key_play`'s Route::Player arms.
    #[test]
    fn transport_keys_ask_for_the_shared_toggle() {
        let _g = crate::testlock::serial();
        for (wcode, want) in [
            (WCODE_PAUSE, Some(false)),
            (WCODE_PLAY, Some(true)),
            (WCODE_PLAYPAUSE, None),
        ] {
            let mut page = PlayerScreen::new(ENTRY);
            let (handled, reqs) = press(&mut page, 0, wcode, Edge::Down);
            assert_eq!(handled, Handled::Yes);
            assert_eq!(reqs, vec![PlayerReq::Transport(want)], "wcode {wcode}");
        }
    }

    /// STOP and a plain BACK both ask to leave the player — port of the Stop arm and `key_back`'s
    /// Route::Player arm (`exit_player`, unreachable from a host test directly since it pulls in
    /// the Starfish/ACB seam — this is the request it is now reached through).
    #[test]
    fn stop_and_back_both_ask_to_exit() {
        let _g = crate::testlock::serial();
        for wcode in [WCODE_STOP, WCODE_BACK] {
            let mut page = PlayerScreen::new(ENTRY);
            let (handled, reqs) = press(&mut page, 0, wcode, Edge::Down);
            assert_eq!(handled, Handled::Yes);
            assert_eq!(reqs, vec![PlayerReq::Exit], "wcode {wcode}");
        }
    }

    /// Port of `key_player_updown`: from the scrubber, UP walks to the control row and DOWN to the
    /// tabs; from either, the return trip goes back to the scrubber.
    #[test]
    fn updown_walks_the_huds_three_rows() {
        let _g = crate::testlock::serial();
        let mut page = PlayerScreen::new(ENTRY);
        fresh(&mut page);
        assert_eq!(page.hud.nav.focus, 0);
        press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(page.hud.nav.focus, 1, "scrubber -> control row");
        press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(page.hud.nav.focus, 0, "control row has nothing above: hides+reparks");
        press(&mut page, SDLK_DOWN, 0, Edge::Down);
        assert_eq!(page.hud.nav.focus, 2, "scrubber -> tabs");
        press(&mut page, SDLK_DOWN, 0, Edge::Down);
        assert_eq!(page.hud.nav.focus, 2, "tabs has nothing below: stays");
        press(&mut page, SDLK_UP, 0, Edge::Down);
        assert_eq!(page.hud.nav.focus, 0, "tabs -> scrubber");
    }

    /// OK on the control row is the DEFERRED press (`ArmControlRow`), not an immediate action —
    /// port of `key_ok`'s `focus == 1` arm.
    #[test]
    fn ok_on_the_control_row_arms_the_deferred_press() {
        let _g = crate::testlock::serial();
        let mut page = PlayerScreen::new(ENTRY);
        fresh(&mut page);
        page.hud.nav.focus = 1;
        let (handled, reqs) = press(&mut page, SDLK_RETURN, 0, Edge::Down);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(reqs, vec![PlayerReq::ArmControlRow]);
    }

    /// OK on the tabs row opens the named overlay AT ONCE — port of `key_ok`'s `focus == 2` arm.
    #[test]
    fn ok_on_the_tabs_row_opens_the_named_overlay() {
        let _g = crate::testlock::serial();
        let mut page = PlayerScreen::new(ENTRY);
        fresh(&mut page);
        page.hud.nav.focus = 2;
        page.hud.nav.tab = 0;
        let (_, reqs) = press(&mut page, SDLK_RETURN, 0, Edge::Down);
        assert_eq!(
            reqs,
            vec![PlayerReq::OpenOverlay(overlay::OverlayKind::Info)]
        );
    }

    /// OK anywhere else (the scrubber, or a hidden transport) is a plain play/pause toggle —
    /// port of `key_ok`'s final `else` arm.
    #[test]
    fn ok_elsewhere_toggles_play_pause() {
        let _g = crate::testlock::serial();
        let mut page = PlayerScreen::new(ENTRY);
        fresh(&mut page);
        page.hud.nav.focus = 0;
        let (_, reqs) = press(&mut page, SDLK_RETURN, 0, Edge::Down);
        assert_eq!(reqs, vec![PlayerReq::Transport(None)]);
    }

    /// A click on a registered control-row stop seats the ring on that button and asks the same
    /// deferred press OK would — the pointer twin of `ok_on_the_control_row_arms_the_deferred_press`.
    #[test]
    fn a_click_on_a_disc_seats_the_ring_and_arms_the_press() {
        let _g = crate::testlock::serial();
        let mut page = PlayerScreen::new(ENTRY);
        let (handled, reqs) = click(&mut page, player_hud::ELEM_ROW_BASE + 1);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(page.hud.nav.focus, 1);
        assert_eq!(page.hud.nav.btn, 1);
        assert_eq!(reqs, vec![PlayerReq::ArmControlRow]);
    }

    /// The scrubber's registered `Stop` is the exact band `scrub_hit` tests, so a click resolved
    /// by the dispatcher's hit map lands on the same geometry the old bare pointer path did.
    #[test]
    fn the_scrubber_group_places_at_the_hit_testers_own_band() {
        let _g = crate::testlock::serial();
        let page = PlayerScreen::new(ENTRY);
        let placed = Focusable::<TestHost>::place(
            &page,
            &player_hud::ELEM_SCRUB,
            &cx(),
            At::Drawn,
        )
        .expect("the scrubber always places");
        let want = player_hud::scrub_hit_rect();
        assert_eq!((placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h), (want.x, want.y, want.w, want.h));
    }

    /// Every group this screen declares resolves back through `group_of` to the group it came
    /// from — the property `Focusable`'s two queries must agree on for the engine's own
    /// bookkeeping to be internally consistent.
    #[test]
    fn every_declared_group_round_trips_through_group_of() {
        let _g = crate::testlock::serial();
        let page = PlayerScreen::new(ENTRY);
        let mut groups = Vec::new();
        Focusable::<TestHost>::groups(&page, &cx(), &mut groups);
        assert!(groups.len() >= 3, "scrubber, control row, tabs at least");
        for g in &groups {
            let seated = Focusable::<TestHost>::seat(
                &page,
                g.id,
                Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None },
                &cx(),
            );
            assert_eq!(
                Focusable::<TestHost>::group_of(&page, &seated.elem, &cx()),
                Some(g.id),
                "group {:?}'s own seat must resolve back to it",
                g.id
            );
        }
    }
}

/// **The scrub gesture has exactly ONE owner** (restructure phase 12, PX-PLAYER).
///
/// Phase 12's first swarm made `PlayerScreen` answer `FocusSource::Engine`/`HitSource::Engine`
/// without deleting the loop's own player input path, so two implementations drove one piece of
/// state — `PlayerScreen::scrub` — at the same time. Each case below is one of the three
/// regressions that produced, watched RED against `2790f47a` before the retirement commit:
///
/// 1. [`one_right_tap_issues_exactly_one_seek`] — the step's `Jump` arm committed on the PRESS
///    while `app/input.rs`'s key-up arm armed `Scrub::commit_at` on the same gesture and
///    `app/run.rs`'s per-frame block committed it a second time.
/// 2. [`a_scrub_seek_taken_while_paused_holds_the_pause`] — the screen asked for
///    [`PlayerReq::SeekTo`], whose loop arm resumes a paused film; a scrub commit must not.
/// 3. [`a_click_on_the_scrubber_seeks_and_a_drag_previews_before_it`] — the scrubber's registered
///    stop landed on the play/pause arm, and the drag preview had lost its producer entirely.
///
/// **Where the loop is part of the defect it is simulated, minimally and by name.** Case 1's
/// second commit came from `app/run.rs`:1413-1426, which no host test can call; what is graded
/// here instead is the contract that removes it — the screen is the only thing that commits a tap,
/// and it commits it on the DEBOUNCE, not on the press. A second committer on that field can then
/// only be a second seek, which is the symptom.
#[cfg(test)]
mod scrub_ownership_tests {
    use super::*;
    use crate::screens::player::overlay_tests::TestHost;
    use crate::screens::registry::AppFx;
    use crate::ui::consts::{SDLK_RETURN, SDLK_RIGHT, WCODE_BACK, WCODE_EXIT};
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{
        Cx, Edge, Effects, EntryId, Fx, Handled, InputEvent, InputKind, InputOwner, MachineId,
        Source, Tick,
    };

    const ENTRY: EntryId = EntryId(9);
    const INST: InstanceId = InstanceId(3);
    /// 100 s, so the 3 s tail cap (`duration - 3s`) is far from every position these cases name.
    const DUR: i64 = 100_000_000_000;

    fn cx() -> Cx<'static, TestHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &FixtureMeasure,
            focus: Default::default(),
            press: Default::default(),
            owner: InputOwner::Entry(ENTRY),
        }
    }

    /// The playback globals these cases read, restored on the way out. Held together with
    /// `testlock::serial()` by every caller: `duration_ns`/`playpos_ns`/`paused` are crate-wide.
    struct Fixture(i64, i64, bool);
    impl Fixture {
        fn new(paused: bool) -> Self {
            use std::sync::atomic::Ordering::Relaxed;
            let was = Fixture(
                crate::player::SHARED.duration_ns.load(Relaxed),
                crate::player::SHARED.playpos_ns.load(Relaxed),
                crate::player::TX.paused.load(Relaxed),
            );
            crate::player::SHARED.duration_ns.store(DUR, Relaxed);
            crate::player::SHARED.playpos_ns.store(0, Relaxed);
            crate::player::TX.paused.store(paused, Relaxed);
            was
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering::Relaxed;
            crate::player::SHARED.duration_ns.store(self.0, Relaxed);
            crate::player::SHARED.playpos_ns.store(self.1, Relaxed);
            crate::player::TX.paused.store(self.2, Relaxed);
        }
    }

    /// A page with the transport ON SCREEN and the ring on the scrubber — the state every gesture
    /// below begins from.
    fn page_on_the_bar() -> PlayerScreen {
        let mut page = PlayerScreen::new(ENTRY);
        page.hud.visible_at_press = true;
        page.hud.nav.focus = 0;
        page
    }

    fn run(page: &mut PlayerScreen, ev: ScreenEvent<TestHost>) -> (Handled, Vec<PlayerReq>) {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        let handled = page.step(
            &ev,
            &cx(),
            &mut Effects::new(&mut out, MachineId::Instance(INST), &mut present),
        );
        let reqs = out
            .into_iter()
            .filter_map(|e| match e.fx {
                Fx::App(AppFx::Player(req)) => Some(req),
                _ => None,
            })
            .collect();
        (handled, reqs)
    }

    fn key(page: &mut PlayerScreen, sym: u32, edge: Edge, ms: u32) -> (Handled, Vec<PlayerReq>) {
        key_w(page, sym, 0, edge, ms)
    }

    /// …and the same with a `wcode`, which is the only field BACK, STOP and EXIT arrive in.
    fn key_w(
        page: &mut PlayerScreen,
        sym: u32,
        wcode: u32,
        edge: Edge,
        ms: u32,
    ) -> (Handled, Vec<PlayerReq>) {
        run(
            page,
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key {
                    key: crate::ui::machine::Key::Other,
                    sym,
                    wcode,
                    edge,
                    at_edge: false,
                },
                at: Tick { ms, dt_us: 0 },
                source: Source::Sdl,
            }),
        )
    }

    fn pointer(
        page: &mut PlayerScreen,
        kind: InputKind<u32>,
        ms: u32,
    ) -> (Handled, Vec<PlayerReq>) {
        run(
            page,
            ScreenEvent::Input(InputEvent { kind, at: Tick { ms, dt_us: 0 }, source: Source::Sdl }),
        )
    }

    fn tick(page: &mut PlayerScreen, ms: u32) -> (Handled, Vec<PlayerReq>) {
        run(page, ScreenEvent::Tick(Tick { ms, dt_us: 16_000 }))
    }

    /// Every request that MOVES THE PLAYHEAD, whichever variant carries it — so a case counts
    /// seeks rather than asserting on one spelling of them.
    fn seeks(reqs: Vec<PlayerReq>) -> Vec<PlayerReq> {
        reqs.into_iter()
            .filter(|r| matches!(r, PlayerReq::SeekTo(_) | PlayerReq::CommitSeek(_)))
            .collect()
    }

    /// A point on the scrubber's registered band, `frac` of the way along it.
    fn on_the_bar(frac: f32) -> (f32, f32) {
        let r = player_hud::scrub_hit_rect();
        (r.x + r.w * frac, r.y + r.h * 0.5)
    }

    /// **(a) ONE tap, ONE seek — and the seek arrives on the DEBOUNCE, not on the press.**
    ///
    /// `Scrub::commit_at`'s whole reason for existing is that a rapid ±10 s burst must coalesce
    /// into a single seek: each commit is a full reopen + prime on the engine, and back-to-back
    /// in-flight seeks are what race the demux. A press that commits immediately spends that,
    /// and — with the loop's own key-up arm still arming the debounce on the same field — issued
    /// a second seek 450 ms later for the same tap.
    #[test]
    fn one_right_tap_issues_exactly_one_seek() {
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);
        let mut page = page_on_the_bar();

        let (handled, reqs) = key(&mut page, SDLK_RIGHT, Edge::Down, 1_000);
        assert_eq!(handled, Handled::Yes);
        assert!(
            seeks(reqs).is_empty(),
            "the press moves the PREVIEW and commits nothing — the debounce is what coalesces \
             a burst, and a press that seeks at once has already spent it",
        );
        assert_eq!(page.scrub.ns, input::SCRUB_STEP_NS, "…and the preview hopped 10 s");

        let (_, reqs) = key(&mut page, SDLK_RIGHT, Edge::Up, 1_020);
        assert!(seeks(reqs).is_empty(), "the release ARMS the debounce; it does not commit");

        let (_, reqs) = tick(&mut page, 1_020 + input::TAP_COMMIT_MS + 1);
        assert_eq!(
            seeks(reqs).len(),
            1,
            "the debounce expiring is the ONE commit this tap earns",
        );

        let (_, reqs) = tick(&mut page, 1_020 + input::TAP_COMMIT_MS + 500);
        assert!(seeks(reqs).is_empty(), "…and it is not repeated on every frame after it");
    }

    /// **(b) A scrub taken while PAUSED leaves the film paused.**
    ///
    /// [`PlayerReq::SeekTo`] is the Chapters strip's request — seek and RESUME — and it is the
    /// wrong one for a scrub: the loop's arm for it calls `resume_if_paused`, so paused scrubbing
    /// started the film. The scrub commit's own request is [`PlayerReq::CommitSeek`], whose arm is
    /// `app::playback::commit_seek` — a seek-preroll override that decodes the landed frame
    /// without publishing a viewer Resume (`repause_at`, `resume_pend`, `TX::begin_paused_seek`,
    /// all three of them the loop's to hold).
    #[test]
    fn a_scrub_seek_taken_while_paused_holds_the_pause() {
        let _g = crate::testlock::serial();
        let _f = Fixture::new(true);
        let mut page = page_on_the_bar();

        key(&mut page, SDLK_RIGHT, Edge::Down, 1_000);
        let target = page.scrub.ns;
        key(&mut page, SDLK_RIGHT, Edge::Up, 1_020);
        let (_, reqs) = tick(&mut page, 1_020 + input::TAP_COMMIT_MS + 1);
        assert_eq!(
            seeks(reqs),
            vec![PlayerReq::CommitSeek(target)],
            "a scrub commit must be the request that HOLDS the pause, not the one that resumes",
        );
    }

    /// **A HELD direction runs the ramp and commits on the release** — and, when the release never
    /// arrives, on the `SCRUB_LOST_MS` net.
    ///
    /// This remote emits a held key as auto-repeat keydowns followed by ONE keyup, so a dropped
    /// keyup leaves `hold` armed with the preview slewing at up to `SCRUB_MAX` for as long as the
    /// page is up. The net was `app/run.rs`'s and phase 9 did not reproduce it; it is the screen's
    /// `Tick` now, beside the ramp it guards.
    #[test]
    fn a_held_direction_commits_on_release_and_on_the_lost_keyup_net() {
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);

        // …the release that does arrive.
        let mut page = page_on_the_bar();
        key(&mut page, SDLK_RIGHT, Edge::Down, 1_000);
        key(&mut page, SDLK_RIGHT, Edge::Repeat, 1_050);
        assert!(page.scrub.hold, "a hardware repeat engages the continuous scrub");
        tick(&mut page, 1_400); // the ramp travels
        let travelled = page.scrub.ns;
        assert!(travelled > input::SCRUB_STEP_NS, "the ramp advanced past the press's own hop");
        let (_, reqs) = key(&mut page, SDLK_RIGHT, Edge::Up, 1_420);
        assert_eq!(
            seeks(reqs),
            vec![PlayerReq::CommitSeek(travelled)],
            "a travelled hold commits AT ONCE on release — no debounce, it is not a tap",
        );

        // …and the release that never does.
        let mut page = page_on_the_bar();
        key(&mut page, SDLK_RIGHT, Edge::Down, 1_000);
        key(&mut page, SDLK_RIGHT, Edge::Repeat, 1_050);
        let (_, reqs) = tick(&mut page, 1_050 + input::SCRUB_LOST_MS / 2);
        assert!(seeks(reqs).is_empty(), "inside the net's window the ramp just runs");
        let (_, reqs) = tick(&mut page, 1_050 + input::SCRUB_LOST_MS + 1);
        assert_eq!(seeks(reqs).len(), 1, "the repeats stopped without a keyup: commit and let go");
        assert_eq!(page.scrub.dir, 0, "…and the gesture is over, not slewing forever");
    }

    /// A key gesture SUPERSEDES a pointer one and throws its preview away rather than hopping from
    /// it — `key_scrub`'s own first act back when the flag was `app::input::Pointer::drag`. The
    /// pointer's release will never reach this gesture, so a preview left standing would be
    /// committed later by a debounce that knows nothing about where it came from.
    #[test]
    fn a_direction_press_during_a_drag_discards_the_drags_preview() {
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);
        let mut page = page_on_the_bar();

        let (mx, my) = on_the_bar(0.75);
        pointer(
            &mut page,
            InputKind::Click { x: mx, y: my, hit: Some(player_hud::ELEM_SCRUB) },
            1_000,
        );
        assert!(page.scrub.drag && page.scrub.ns > DUR / 2, "the fixture: a drag is under way");

        let (_, reqs) = key(&mut page, SDLK_RIGHT, Edge::Down, 1_100);
        assert!(seeks(reqs).is_empty());
        assert!(!page.scrub.drag, "the pointer no longer owns the gesture");
        assert_eq!(
            page.scrub.ns, input::SCRUB_STEP_NS,
            "…and the hop is seeded from the PLAYHEAD, not from the abandoned drag preview",
        );
    }

    /// **A click that lands on NO control toggles play/pause** — the `else` arm of the old pointer
    /// block, and the reason clicking the picture works at all: over full-screen video most of the
    /// frame is not a control. It is the one part of that block that was never about geometry, and
    /// the easiest to lose in a port that dispatches on a resolved element.
    #[test]
    fn a_click_on_the_picture_toggles_play_pause_but_not_over_a_failure() {
        use std::sync::atomic::Ordering::Relaxed;
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);
        let mut page = page_on_the_bar();

        let (handled, reqs) = pointer(
            &mut page,
            InputKind::Click { x: 960.0, y: 400.0, hit: None },
            1_000,
        );
        assert_eq!(handled, Handled::Yes);
        assert_eq!(reqs, vec![PlayerReq::Transport(None)]);

        // …and not while a failure owns the frame: there is nothing to toggle, and the read-out's
        // one drawn target is its own escape.
        let was = crate::player::SHARED.pb_state.load(Relaxed);
        crate::player::SHARED
            .pb_state
            .store(crate::player::PlaybackState::Error as u8, Relaxed);
        let mut page = page_on_the_bar();
        let (handled, reqs) = pointer(
            &mut page,
            InputKind::Click { x: 960.0, y: 400.0, hit: None },
            1_000,
        );
        assert_eq!(handled, Handled::Yes, "swallowed, as the old block's early return did");
        assert!(reqs.is_empty());
        crate::player::SHARED.pb_state.store(was, Relaxed);
    }

    /// **The failure read-out's own two escapes, as the page's first test rather than a ladder
    /// arm's height.**
    ///
    /// `key_player_failed` expressed this as PRECEDENCE — its guard sat above every other player
    /// arm in `app/run.rs`'s chain and `return`ed — and by the time phase 12 found it, it had
    /// grown a `!matches!(app.route, Route::Player)` term beside a `Route::Player` test, so it
    /// could never run at all: OK on a failed playback reached `key_ok`'s final `else` and asked
    /// the pipeline to toggle a film that was not playing. `PlayerScreen::handle_key` asks
    /// `transport_hidden` FIRST, which is the same precedence expressed as one condition.
    #[test]
    fn a_failed_playback_answers_only_its_two_drawn_escapes() {
        use std::sync::atomic::Ordering::Relaxed;
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);
        let was = crate::player::SHARED.pb_state.load(Relaxed);
        crate::player::SHARED
            .pb_state
            .store(crate::player::PlaybackState::Error as u8, Relaxed);

        let mut page = page_on_the_bar();
        let (handled, reqs) = key(&mut page, SDLK_RETURN, Edge::Down, 1_000);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(
            reqs,
            vec![PlayerReq::OpenOverlay(overlay::OverlayKind::More { quality: true })],
            "OK is the read-out's forward escape: the quality ladder on its current rung",
        );

        let mut page = page_on_the_bar();
        let (handled, reqs) = key_w(&mut page, 0, WCODE_BACK, Edge::Down, 1_000);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(reqs, vec![PlayerReq::Exit], "…and BACK is the way out of it");

        let mut page = page_on_the_bar();
        let (handled, reqs) = key_w(&mut page, SDLK_RIGHT, 0, Edge::Down, 1_000);
        assert_eq!(handled, Handled::Yes, "everything else is SWALLOWED, not acted on");
        assert!(reqs.is_empty());
        assert_eq!(page.scrub.ns, -1, "…and nothing that is not drawn was driven");

        // …but EXIT is the remote's own and ends the PROCESS, so it is not this screen's to
        // swallow: a failed playback would otherwise be the one screen an EXIT press cannot
        // leave (LG checklist item 38).
        let mut page = page_on_the_bar();
        let (handled, reqs) = key_w(&mut page, 0, WCODE_EXIT, Edge::Down, 1_000);
        assert_eq!(handled, Handled::No, "EXIT falls through to the loop's own arm");
        assert!(reqs.is_empty());

        crate::player::SHARED.pb_state.store(was, Relaxed);
    }

    /// **(c) The scrubber's stop is a SEEK target, and a drag previews across it.**
    ///
    /// The pointer half of the same gesture. A click on the bar puts the preview under the
    /// pointer; a drag carries it (`§7.5`: a drag drives its control with no hover and no click);
    /// the button coming up — which reaches a screen as `Ok`/`Edge::Up`, the one release the
    /// dispatcher's `InputKind` has — commits it. Before this, the registered stop fell through to
    /// `key_ok`'s final `else` and TOGGLED PLAY/PAUSE, and `app.ptr.drag` had no producer left at
    /// all, so pointer scrubbing was gone.
    #[test]
    fn a_click_on_the_scrubber_seeks_and_a_drag_previews_before_it() {
        let _g = crate::testlock::serial();
        let _f = Fixture::new(false);
        let mut page = page_on_the_bar();
        let tol = DUR / 1_000; // 100 ms: the band's own f32 arithmetic, not a behaviour

        let (mx, my) = on_the_bar(0.5);
        let (handled, reqs) = pointer(
            &mut page,
            InputKind::Click { x: mx, y: my, hit: Some(player_hud::ELEM_SCRUB) },
            1_000,
        );
        assert_eq!(handled, Handled::Yes);
        assert!(
            reqs.is_empty(),
            "a click on the bar is a SEEK, never the transport toggle",
        );
        assert!(
            (page.scrub.ns - DUR / 2).abs() < tol,
            "the preview lands where the pointer did (got {})",
            page.scrub.ns,
        );

        let (dx, dy) = on_the_bar(0.25);
        let (_, reqs) = pointer(
            &mut page,
            InputKind::Drag { x: dx, y: dy, hit: Some(player_hud::ELEM_SCRUB) },
            1_040,
        );
        assert!(reqs.is_empty(), "a drag PREVIEWS: it commits on release, not per motion");
        assert!(
            (page.scrub.ns - DUR / 4).abs() < tol,
            "…and the preview follows it (got {})",
            page.scrub.ns,
        );

        let previewed = page.scrub.ns;
        let (_, reqs) = key(&mut page, SDLK_RETURN, Edge::Up, 1_080);
        assert_eq!(
            seeks(reqs),
            vec![PlayerReq::CommitSeek(previewed)],
            "the button coming up commits exactly what the drag was showing",
        );
    }
}
#[cfg(test)]
mod repair_confirmation_tests {
    use super::*;
    use crate::ui::machine::{Host, InputEvent, InputOwner, MachineId, PressId, Source, Tick};
    struct TestHost;
    impl Host for TestHost {
        type Arg = crate::ui::fixture::FixtureArg;
        type Fx = AppFx;
        type Msg = crate::screens::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = &'a crate::route::PlaybackSession;
        type Init = crate::ui::fixture::FixtureArg;
        type Memory = crate::screens::registry::PageMemory;
    }
    impl PlayerLike for TestHost {
        fn session<'a>(cx: &Cx<'a, Self>) -> &'a crate::route::PlaybackSession { cx.views }
    }
    thread_local! {
        static TEST_METADATA: std::cell::UnsafeCell<crate::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(crate::stores::metadata::MetadataStore::default());
    }
    fn test_store() -> &'static mut crate::stores::metadata::MetadataStore {
        TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
    }
    impl crate::screens::registry::MetadataLike for TestHost {
        fn metadata<'a>(_cx: &Cx<'a, Self>) -> crate::metadata::MetadataView<'a> {
            test_store().view()
        }
    }
    fn context(ps: &crate::route::PlaybackSession, elem: Option<u32>) -> Cx<'_, TestHost> {
        let mut cx = Cx { views: ps, tick: Tick::default(), measure: &crate::ui::fixture::FixtureMeasure,
            focus: Default::default(), press: Default::default(), owner: InputOwner::Entry(EntryId(1)) };
        cx.focus.current = elem.map(|elem| FocusKey { entry: EntryId(1), elem });
        cx
    }
    fn deliver(page: &mut PlayerScreen, ps: &crate::route::PlaybackSession, elem: Option<u32>, ev: ScreenEvent<TestHost>) -> Vec<PlayerReq> {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(&ev, &context(ps, elem), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(1)), &mut present));
        for effect in &out {
            if let Fx::Deliver(MachineId::Instance(id), _) = &effect.fx {
                assert_eq!(*id, InstanceId(1), "focus requests must address this screen instance");
            }
        }
        out.into_iter().filter_map(|e| match e.fx { Fx::App(AppFx::Player(req)) => Some(req), _ => None }).collect()
    }
    fn key_event(sym: u32, wcode: u32) -> ScreenEvent<TestHost> {
        ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: crate::ui::machine::Key::Other, sym, wcode, edge: Edge::Down, at_edge: false }, at: Tick::default(), source: Source::Sdl })
    }
    fn blocked() -> crate::route::PlaybackSession {
        let mut ps = crate::route::PlaybackSession::IDLE;
        ps.jail_load_blocked = true;
        ps
    }
    #[test]
    #[cfg(feature = "devtriggers")]
    fn jail_fixture_keeps_repair_open_through_the_screen_tick() {
        let _g = crate::testlock::serial();
        struct Trigger(std::path::PathBuf, Option<Vec<u8>>);
        impl Drop for Trigger {
            fn drop(&mut self) {
                if let Some(bytes) = &self.1 { std::fs::write(&self.0, bytes).unwrap(); }
                else { std::fs::remove_file(&self.0).unwrap(); }
            }
        }
        let path = crate::paths::in_runtime_dir("plxnative-failtest");
        let previous = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("cannot read the prior fixture: {e}"),
        };
        let _trigger = Trigger(path.clone(), previous);
        std::fs::write(&path, "jail").unwrap();
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::reset_player_control_for_test(&ps);
        let hardware_verdict = crate::webos::jail_blocks_native_video();
        crate::dev::scenarios::failure_fixture(&mut ps);
        let mut page = PlayerScreen::new(EntryId(1));
        assert!(deliver(&mut page, &ps, None, key_event(consts::SDLK_RETURN, 0)).is_empty());
        assert!(page.repair_alert.is_open(), "fixture must offer the real confirmation");
        assert!(deliver(&mut page, &ps, Some(REPAIR_CANCEL), ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_000 })).is_empty());
        assert!(page.repair_alert.is_open(), "Tick must not retire the jail fixture confirmation");
        assert!(ps.jail_load_blocked);
        assert_eq!(crate::webos::jail_blocks_native_video(), hardware_verdict, "the fixture must not alter the cached hardware verdict");
        ps.jail_load_blocked = false;
        deliver(&mut page, &ps, None, ScreenEvent::Tick(Tick { ms: 32, dt_us: 16_000 }));
        assert!(!page.repair_alert.is_open(), "retiring the session still closes its confirmation");
        std::fs::write(&path, "tv").unwrap();
        crate::dev::scenarios::failure_fixture(&mut ps);
        assert!(!ps.jail_load_blocked, "other failure fixtures must not claim a jail refusal");
    }

    #[test]
    fn repair_requires_second_explicit_answer_and_cancel_is_the_default() {
        let _g = crate::testlock::serial();
        let ps = blocked();
        crate::route::reset_player_control_for_test(&ps);
        let mut page = PlayerScreen::new(EntryId(1));
        assert!(deliver(&mut page, &ps, None, key_event(consts::SDLK_RETURN, 0)).is_empty());
        assert!(page.repair_alert.is_open());
        assert_eq!(page.repair_alert.choice(), crate::ui::decision_alert::Choice::Cancel);
        let cx = context(&ps, Some(REPAIR_CANCEL));
        let from = Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None };
        assert_eq!(Focusable::<TestHost>::seat(&page, GROUP_REPAIR, from, &cx).elem, REPAIR_CANCEL);
        assert!(matches!(Focusable::<TestHost>::neighbour(&page, FocusKey { entry: EntryId(1), elem: REPAIR_CANCEL }, Dir::Right, &cx), Step::Move(FocusKey { elem: REPAIR_CONFIRM, .. })));
        let req = deliver(&mut page, &ps, Some(REPAIR_CONFIRM), ScreenEvent::PressCommit(PressId(1)));
        assert_eq!(req, vec![PlayerReq::RepairSandbox]);
        assert!(deliver(&mut page, &ps, Some(REPAIR_CONFIRM), ScreenEvent::PressCommit(PressId(1))).is_empty());
        page.repair_alert.close();
    }
    #[test]
    fn back_then_stale_commit_and_underlying_click_cannot_repair_or_reopen() {
        let _g = crate::testlock::serial();
        let ps = blocked();
        crate::route::reset_player_control_for_test(&ps);
        let mut page = PlayerScreen::new(EntryId(1));
        deliver(&mut page, &ps, None, key_event(consts::SDLK_RETURN, 0));
        assert!(deliver(&mut page, &ps, Some(REPAIR_CONFIRM), key_event(0, consts::WCODE_BACK)).is_empty());
        assert!(!page.repair_alert.is_open());
        assert!(deliver(&mut page, &ps, Some(REPAIR_CONFIRM), ScreenEvent::PressCommit(PressId(1))).is_empty());
        assert!(deliver(&mut page, &ps, None, ScreenEvent::Activate(player_hud::ELEM_FAILURE_OK)).is_empty());
        assert!(!page.repair_alert.is_open());
        page.repair_alert.close();
    }
    #[test]
    fn accepted_attempt_hides_forward_action_after_screen_recreation() {
        let _g = crate::testlock::serial();
        for state in [crate::webos::jail_repair::State::Running, crate::webos::jail_repair::State::Repaired, crate::webos::jail_repair::State::Failed(crate::webos::jail_repair::Failure::Timeout)] {
            let mut ps = blocked(); ps.repair_status = state;
            crate::route::reset_player_control_for_test(&ps);
            let mut page = PlayerScreen::new(EntryId(1));
            assert!(deliver(&mut page, &ps, None, key_event(consts::SDLK_RETURN, 0)).is_empty());
            assert!(!page.repair_alert.is_open());
        }
    }
}
