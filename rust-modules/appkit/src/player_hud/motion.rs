//! **The HUD's animation state.** [`ClockWarm`] prewarms the glyphs of the clock string the next
//! frame will draw, [`RowPop`] is the control row's pop-in when its occupant changes, and
//! [`HudMotion`] (formerly `TransportRow`) is everything the transport animates frame to frame: the
//! knob spring, the per-button focus fade, the play / pause mark and the row pop.
//!
//! `HudMotion::step` and `note_transport` still sample the transport globals themselves: they run at
//! input and Tick time, and moving them onto a [`super::model::HudModel`] sampled at prepare time
//! would shift them by a frame. The prewarm ([`ClockWarm::queue`]) is a draw-side read and so takes
//! the model; the page's `prepare` refreshes the model's scrub target from its own scrub first,
//! because the app's snapshot is a frame old by then.
use super::classic::fmt_time;
use super::geometry::GROUP_ROW;
use super::model::HudModel;
use super::state::*;
use crate::knob_spring::KnobSpring;
use plx_machine::machine::GroupId;
use plx_ui::theme;
use std::ffi::CString;
use std::os::raw::{c_int};
use std::sync::atomic::Ordering::Relaxed;

/// **The transport's two clocks, rasterised AHEAD of the draw's back-buffer wait.**
///
/// The elapsed and remaining clocks are new strings once a second, and the draw met each of them
/// cold: `FRAMEDROP … clear:12.4 … textx2:1.1` on the TV (1 to 3.5 ms depending on the digits),
/// once a second for as long as the transport is up, which under an open Tracks/More popover is
/// the whole time. On the player route the frame's first framebuffer command waits for the
/// compositor to release a back buffer, so anything rasterised after it is added to the frame's
/// commit time and moves the commit toward (and past) the compositor's next repaint; anything
/// rasterised before it overlaps the wait and costs nothing (`PanelMotion::prewarm_text` is the
/// same argument for a page's rows).
///
/// So the page's `prepare` hands the strings to the text queue, which `app::run::prepare_window`
/// drains before the clear: this second's pair, and the NEXT second's, so the string a tick is
/// about to need was rasterised up to a second earlier and a playhead that crosses the boundary
/// between `prepare` and the draw still finds it resident. The strings are formatted only when a
/// displayed second changes; the queue is offered every prepared frame, because the draw empties
/// it (`ui::dispatch`) and an offer for a resident string is one cache probe.
#[derive(Default)]
pub struct ClockWarm {
    /// The (elapsed, remaining) seconds `strings` were formatted for.
    key: Option<(i64, i64)>,
    /// `(text, bold, size)` exactly as [`draw_playbar`] hands them to `Painter::text`.
    strings: Vec<(CString, c_int, c_int)>,
}

impl ClockWarm {
    /// Queue the clocks for the playhead the playbar would draw this frame, from the frame's
    /// [`HudModel`]: the same samples [`Playbar::live`](super::classic::Playbar) resolves its
    /// position from, so the prewarm and the draw agree.
    pub fn queue(&mut self, model: &HudModel) {
        let pos = display_pos_ns(model.loading, model.seek_display_ns, model.scrub_ns, model.playpos_ns);
        self.queue_at(pos, model.duration_ns);
    }

    /// The (elapsed, remaining) seconds the last queue was formatted for.
    #[cfg(any(test, feature = "test-support"))]
    pub fn warmed_for_test(&self) -> Option<(i64, i64)> {
        self.key
    }

    /// Queue the clocks [`draw_playbar`](super::classic::draw_playbar) draws at `pos_ns` of
    /// `dur_ns`, and one second on.
    fn queue_at(&mut self, pos_ns: i64, dur_ns: i64) {
        const SEC_NS: i64 = 1_000_000_000;
        // `fmt::clock` floors to whole seconds and clamps at zero; the key is that same second.
        let key = ((pos_ns / SEC_NS).max(0), ((dur_ns - pos_ns) / SEC_NS).max(0));
        if self.key != Some(key) {
            self.key = Some(key);
            self.strings.clear();
            let (el, rem) = key;
            let (cap, grown) = (theme::size::CAPTION, crate::scrub_preview::CLOCK_GROWN_SZ);
            let clocks = [
                (fmt_time(el * SEC_NS, false), 1, cap),
                (fmt_time(rem * SEC_NS, true), 0, cap),
                (fmt_time((el + 1) * SEC_NS, false), 1, cap),
                (fmt_time((rem - 1).max(0) * SEC_NS, true), 0, cap),
                // the elapsed clock also draws at the size it grows to during a scrub with no card
                // (`scrub_preview::clock_faces`); cold, that is a font open and a rasterisation in
                // the middle of a hold. Last, so the resting sizes are never starved of the frame's
                // budget.
                (fmt_time(el * SEC_NS, false), 1, grown),
                (fmt_time((el + 1) * SEC_NS, false), 1, grown),
            ];
            self.strings
                .extend(clocks.into_iter().filter_map(|(s, bold, sz)| Some((CString::new(s).ok()?, bold, sz))));
        }
        for (s, bold, sz) in &self.strings {
            plx_gfx::text::queue_prewarm(s.as_ptr(), *sz, *bold);
        }
    }
}

/// The CONTROL ROW's focus pop — one spring per item ([`plx_ui::widgets::CtlPop`]), so the
/// control being left shrinks while the arriving one grows instead of both snapping.
///
/// **One array for all three slot occupants**, not one per module. The transport discs, the Skip
/// pill and the Up Next pair are the same row at the same right edge, only ever one at a time, and
/// the cursor walking them is `hud_nav.btn` for all three ([`ControlSlot::items`]) — so the pop is a
/// property of that row and not of whichever module happens to draw it this frame. The episode row has the widest occupant,
/// with room for its Previous/Next pair plus the three existing discs. `up_next` and `skip_pill` read theirs through [`row_pop`].
///
/// A field of [`HudMotion`], which the player's instance owns: the HUD keeps no view struct
/// — it is drawn entirely from the caller's arguments — so this and the resume clock beside it are
/// the whole of the route's retained motion state, and phase 9 gave them the one owner the rest of
/// the route already has.
pub type RowPop = plx_ui::widgets::CtlPop<{ super::geometry::MAX_ROW_ITEMS }>;

/// How many items of the row can pop for `slot`: the discs of a package are as many as the format lets one
/// row hold, a stand-in has its own count. (A stale cursor left by a wider occupant pops nothing.)
fn row_pops(slot: ControlSlot) -> usize {
    match slot {
        ControlSlot::Discs(_) => super::geometry::MAX_ROW_ITEMS,
        other => other.items().max(0) as usize,
    }
}

/// **Everything the transport row remembers between frames**: the control row's per-item focus pop
/// and the paused→playing edge the state read-out's `Play` mark is timed from.
///
/// Both were `static mut`s (`ROW_POP`, `PLAY_AT`, `PAUSE_SEEN`) until restructure phase 9. They are
/// LOGICAL state, not a render cache — the pop is a spring whose position a viewer can see and the
/// edge is a per-SESSION clock — so they live on `PlayerScreen` and are stepped from its `Tick`,
/// exactly where [`step`](Self::step)'s doc says they must be.
pub struct HudMotion {
    pub(super) pop: RowPop,
    /// The paused title's one-shot reveal clock; no motion remains after it reaches the end.
    pub(super) title_clock: plx_ui::marquee::Clock,
    /// The paused→playing EDGE, as an `SDL_GetTicks` stamp — the clock behind
    /// [`TransportMark::Play`]. `None` until this session has been resumed at least once.
    pub(super) play_at: Option<u32>,
    /// Last frame's `TX.paused`, for the edge above. `None` while there is no session, which is
    /// what makes the edge per-SESSION: `TX::reset` clears both `started` and `paused` on stop, so
    /// without this a session that ended paused would hand the next one a spurious resume edge on
    /// its first frame and flash `Play` over a start nobody pressed play for.
    pub(super) pause_seen: Option<bool>,
    /// **The scrub preview's memo** — a RENDER memo: which BIF still the draw can
    /// show and which one is in flight. Stepped where that art is probed, in the draw ([`super::render::draw`]); see
    /// [`crate::scrub_preview`].
    pub(super) preview: crate::scrub_preview::ScrubPreview,
    /// The knob's return to the playhead when a scrub is cancelled ([`crate::knob_spring`]).
    pub(super) spring: KnobSpring,
}

impl Default for HudMotion {
    fn default() -> Self {
        Self::new()
    }
}

impl HudMotion {
    pub fn new() -> Self {
        Self {
            pop: RowPop::new(),
            title_clock: plx_ui::marquee::Clock::new(),
            play_at: None,
            pause_seen: None,
            preview: crate::scrub_preview::ScrubPreview::new(),
            spring: KnobSpring::new(),
        }
    }

    /// Step the control row's focus pop AND the resume clock — once per FRAME, from the player
    /// instance's `Tick`.
    ///
    /// **Not from [`super::render::draw`]**, though that is where every other number this row draws comes
    /// from: a spring advanced inside a draw advances once per DRAW, and this row is not drawn on
    /// every frame of the route (a failure read-out owns the frame, the Info card and the Chapters
    /// strip take the transport away). The pop would then run at a rate that depended on which
    /// overlay was open, and a resume pressed with the Info card open would stamp its 2 s from the
    /// moment the card CLOSED, or never.
    ///
    /// `focus == GROUP_ROW` is the control column; anything else closes every pop. The index is bounded by
    /// the CURRENT occupant's item count, so a stale `btn` left over from a wider slot cannot pop a
    /// control the narrower one does not have.
    pub fn step(&mut self, slot: ControlSlot, focus: GroupId, btn: i32, dt: f32, now: u32) {
        self.note_transport(now);
        // Here and not in the draw: a scrub can end with the transport hidden (UP-hide), when
        // nothing draws, and the next one must not open on this one's last still.
        let scrub_ns = plx_media::player::TX.scrub_ns.load(Relaxed);
        let live = scrub_ns >= 0;
        if !live {
            self.preview.release();
        }
        self.preview.motion.step_live(live, now);
        self.spring.step(scrub_ns, now);
        let f = (focus == GROUP_ROW)
            .then(|| usize::try_from(btn).ok())
            .flatten()
            .filter(|&i| i < row_pops(slot));
        self.pop.step(f, dt);
    }

    /// Do the title block, control row and bottom tabs take the pointer? Not while a scrub has
    /// them faded away, nor until they are back ([`crate::scrub_preview::Motion`]).
    pub fn chrome_interactive(&self) -> bool {
        self.preview.motion.chrome_interactive()
    }

    /// Does the scrub's choreography (chrome fade, card, knob clock) still move at `now`? Owed a
    /// frame for as long as it does.
    pub fn scrub_motion_animating(&self, now: u32) -> bool {
        self.preview.motion.animating(now)
            || self.spring.busy(now)
    }

    /// Control `i`'s focus pop this frame — the read half of the row's springs, for the two
    /// stand-ins that draw into this row from their own modules (`up_next`, `skip_pill`).
    pub fn scale(&self, i: c_int) -> f32 {
        self.pop.scale(i.max(0) as usize)
    }

    /// Stamp the resume edge directly. `note_transport` derives it from `player::TX`, which is
    /// process-wide and moved by every playback test in the crate; a test about THIS clock must
    /// not be able to fail because another one flipped `started` between two of its lines.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_play_at_for_test(&mut self, at: u32) {
        self.play_at = Some(at);
    }

    pub fn reset_title(&self) {
        self.title_clock.release();
    }

    pub fn note_transport(&mut self, now: u32) {
        if !plx_media::player::is_started() {
            self.reset_title();
            self.play_at = None;
            self.pause_seen = None;
            return;
        }
        let paused = plx_media::player::TX.paused.load(Relaxed);
        if !paused {
            self.reset_title();
        }
        if self.pause_seen == Some(true) && !paused {
            self.play_at = Some(now);
        }
        self.pause_seen = Some(paused);
    }

    /// ms since the last resume edge — the read half of [`Self::play_at`].
    ///
    /// **A report IS owed for this clock, and it is owed through
    /// `PlayerScreen::clock_fingerprint`**
    /// (phase 9). This comment used to say the opposite, on a premise that was true and is now
    /// gone: the gate was `idle::should_present(now) || player`, so the player ROUTE presented
    /// unconditionally, and this was the one raw-time animation in the app that could not ship
    /// frozen. The gate is keyed on the video plane being BOUND now, and the two-second `Play`
    /// mark is very often still on screen after an unbind — where a frozen glyph is exactly the
    /// failure `Xfade::tick` and `Spinner::draw` each shipped once.
    ///
    /// It reports through the fingerprint rather than with an `invalidate` of its own for the
    /// reason the fingerprint exists: an unconditional report from a clock that is READ every
    /// frame would present every frame, which is the gate turned off by another name. What is
    /// owed is a frame when the ANSWER CHANGES, and `PLAY_MARK_MS` is one of that value's terms.
    pub fn since_play_ms(&self, now: u32) -> Option<u32> {
        self.play_at.map(|t| now.wrapping_sub(t))
    }
}

