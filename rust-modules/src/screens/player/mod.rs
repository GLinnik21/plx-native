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
#[cfg(test)]
mod overlay_tests;

use std::borrow::Cow;

use crate::screens::registry::{AppLike, PageMemory, PlayerLike};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Effects, EntryId, FocusKey, GroupId, Handled, InstanceId, LogicalState, Machine,
};
use crate::ui::player_hud::{ControlSlot, SubtitleBitmaps, TransportRow};
use crate::ui::screen::{
    At, Dir, DrawFrame, FocusSource, Focusable, GroupSpec, HitSource, Placed, RenderStrategy,
    Screen, ScreenEvent, Step,
};
use crate::ui::up_next::Countdown;

use input::{HeldKey, HudState, Scrub};

/// The player's heartbeat word — byte-identical to `app::route_word(Route::Player)`, which
/// `bridge::frame`'s `debug_assert_eq!` compares against on every frame and which `tests/run.py`
/// selects fps samples by (`tests/manifest.json`'s `route` field).
pub(crate) const WORD: &str = "player";

/// The fields [`PlayerScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
pub(crate) const SHAPE: &str =
    "PlayerScreen{hud:{focus:i32,btn:i32,tab:i32,until:u32,dismissed:bool,visible_at_press:bool,\
     offer:Option<(u32,i64)>,was_standin:bool},scrub:{dir:i32,hold:bool,reveal:bool,ns:i64,\
     commit_at:u32},held:{sym:u32,down_sym:u32},origin:Option<u32>}";

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
    /// The client-side hold-repeat for the bare transport's directions.
    pub(crate) held: HeldKey,
    // (`repeat: RepeatGate` is NOT here, and spec §9's target shape lists it: the paced
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
}

impl PlayerScreen {
    pub(crate) fn new(entry: EntryId) -> Self {
        Self {
            entry,
            hud: HudState::IDLE,
            scrub: Scrub::IDLE,
            held: HeldKey::IDLE,
            row: TransportRow::new(),
            up_next: Countdown::default(),
            slot: ControlSlot::Discs,
            busy: crate::ui::player_hud::Busy::None,
            transport: true,
            lifted: false,
            origin: None,
            render: PlayerRender::default(),
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

    /// The transport, with this instance's own springs, memo and countdown.
    pub(crate) fn draw_hud(&mut self, ps: &crate::route::PlaybackSession, now: u32) {
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
        );
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

impl<H: AppLike<Memory = PageMemory>> Machine<H> for PlayerScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(tick) => {
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
                self.publish();
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                // The GL names in the subtitle set are OWNED. A static could never be told that a
                // playback had ended; an instance is told exactly once.
                self.render.subs.release();
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

/// The player keeps its OWN cursor (`HudNav`) rather than seating the focus engine: the transport
/// is three rows of unlike things — a scrubber, a control row whose occupant changes under the
/// ring, and a pair of tabs — and phase 9 moves its state, not its focus model (§9: "`HudState`
/// (already owned loop locals) move in unchanged"). It therefore declares
/// `FocusSource::Legacy`/`HitSource::Legacy` below, for which this impl is inert.
impl<H: AppLike<Memory = PageMemory>> Focusable<H> for PlayerScreen {
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
            .u64(self.scrub.ns as u64)
            .u32(self.scrub.commit_at);
        c.u32(self.held.sym).u32(self.held.down_sym);
        c.option(self.origin.as_ref(), |c, o| {
            c.u32(o.entry.0);
        });
    }
    fn probe(&self, out: &mut String) {
        out.push_str(WORD);
    }
}

impl<H: PlayerLike> Screen<H> for PlayerScreen {
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
        self.draw_subtitle_bitmap(subs_lift); // PGS/VobSub image subs
        crate::ui::player_hud::draw_subtitles(subs_lift);
        if hud_up || self.lifted {
            self.draw_hud(ps, now);
        }
        // The read-out is NOT transport chrome — it is drawn whether or not the HUD is up, so a
        // terminal `Error` (which is not `is_busy()`, so it does not pin the HUD) keeps its message
        // instead of vanishing with the 4.5 s linger. AFTER the transport, so it is never dimmed by
        // the scrim; BEFORE the overlay panels, which the container draws above this page.
        crate::ui::player_hud::draw_readout(ps, self.busy, now);
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
