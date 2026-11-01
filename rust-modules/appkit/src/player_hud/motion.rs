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
use super::geometry::{GROUP_ROW, GROUP_SECTION, GROUP_STRIP, MAX_ROW_ITEMS};
use super::model::HudModel;
use super::state::*;
use crate::knob_spring::KnobSpring;
use super::command::Section;
use plx_machine::machine::{EntryId, GroupId, PresentHandle, Tick};
use plx_ui::cards::Shelf;
use plx_ui::containers::modal::PopoverMotion;
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
    /// An open section's own buttons' focus pop ([`super::geometry::GROUP_SECTION`]), one spring per button:
    /// the renderer's twin of `InfoPanelState::ctl_pop`. It is stepped while the screen focuses
    /// `GROUP_SECTION` (the open Info page is the section; `info_panel.rs`'s pop belongs to the retained overlay).
    pub(super) section_pop: RowPop,
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
    /// **The chapters strip's Shelf** ([`super::strip`]): its pop, scroll and caption springs are LOGICAL state
    /// the screen's tick steps (`Shelf::on`), so the shelf is held here with the rest of the HUD's motion and a
    /// draw only paints it. Built by [`strip`](Self::strip) on first use.
    strip: Option<Shelf>,
    /// **Each section's appear spring**, the shared `PopoverMotion` the panels' surfaces ran (Chapters, then Info;
    /// [`section_index`]): at 1 when the section is up and at rest, `0` when it is not, and in between while it
    /// opens or fades. LOGICAL state like the Shelf's springs, stepped by [`step_appear`](Self::step_appear) from
    /// the screen's tick and only read by a draw. Its target follows `HudState::sections` ([`follow`](Self::follow)).
    appear: [PopoverMotion; 2],
    /// The sections the springs were last told are open (`StateKey::open` bits).
    wants: u8,
    /// **The focus a section had when it was put away**, per section (Chapters' card, Info's button): a section
    /// fading out paints it, as the panels drew from their own cursor, though the page's focus has already moved
    /// to the control that opened it. Set by [`hold_focus`](Self::hold_focus) at the dismissal, `None` while the
    /// section is open or had no focus, and cleared when it opens again or has gone. It is paint only: a closing
    /// section registers no stop and answers no key.
    held: [Option<usize>; 2],
    /// **The frame (`Tick::ms`) a section was dismissed on**, per section: its spring takes no step on that frame
    /// ([`step_appear`](Self::step_appear)). A panel's surface stepped its spring ahead of the frame's keys, so the
    /// frame a BACK landed on drew the card whole and the fade began on the next; the page steps after its keys.
    dismissed: [Option<u32>; 2],
}

/// Which spring is `section`'s.
fn section_index(section: Section) -> usize {
    match section {
        Section::Chapters => 0,
        Section::Info => 1,
    }
}

/// Both sections, in spring order.
const SECTIONS: [Section; 2] = [Section::Chapters, Section::Info];

impl Default for HudMotion {
    fn default() -> Self {
        Self::new()
    }
}

/// **A section's spring has left**: at rest at 0, which is when a panel's `Surface` was pruned
/// (`ModalStack::prune`, `PopoverMotion::settled`), not at `CLOSING_GONE`: the tail between the two is a few frames
/// of an almost invisible card, and the old panel drew them.
fn faded(spring: &PopoverMotion) -> bool {
    spring.settled() && spring.appear < 0.5
}

impl HudMotion {
    pub fn new() -> Self {
        Self {
            pop: RowPop::new(),
            section_pop: RowPop::new(),
            title_clock: plx_ui::marquee::Clock::new(),
            play_at: None,
            pause_seen: None,
            preview: crate::scrub_preview::ScrubPreview::new(),
            spring: KnobSpring::new(),
            strip: None,
            appear: [PopoverMotion::at(0.0); 2],
            wants: 0,
            held: [None; 2],
            dismissed: [None; 2],
        }
    }

    /// **Point the appear springs at the sections that are open now** (`StateKey::open` bits, the screen's
    /// `HudState::sections`). A section that opens from nothing starts at appear 0 and holds that one frame
    /// (`PopoverMotion::hold_one_frame`): the presented frame draws it invisible and the spring runs from the
    /// next, as a surface's does. One that opens while still fading out is turned round where it is. A section
    /// that closed fades back down. A call with nothing changed does nothing.
    pub fn follow(&mut self, open: u8) {
        let changed = open ^ self.wants;
        for section in SECTIONS {
            if changed & section.open_bit() == 0 {
                continue;
            }
            let spring = &mut self.appear[section_index(section)];
            if open & section.open_bit() != 0 {
                self.held[section_index(section)] = None;
                self.dismissed[section_index(section)] = None;
                let fresh = faded(spring);
                spring.to(1.0);
                if fresh {
                    spring.hold_one_frame();
                    // a new card, as a panel's surface was: every button at rest, not a hair off it
                    if section == Section::Info {
                        self.section_pop.reset();
                    }
                }
            } else {
                spring.to(0.0);
            }
        }
        self.wants = open;
    }

    /// **Take every section away at once**, nothing composited of them again (`PopoverMotion::at(0.0)`, the jump
    /// `ModalStack::hide` makes): a failed playback, a new film, the page leaving. The strip's Shelf goes with them.
    pub fn cut_sections(&mut self) {
        self.appear = [PopoverMotion::at(0.0); 2];
        self.wants = 0;
        self.held = [None; 2];
        self.dismissed = [None; 2];
        self.strip = None;
    }

    /// **Remember where `section`'s focus was as the page puts it away**: `focus` and `btn` are the page's cursor
    /// BEFORE it is handed back to the opener (the strip's card, or Info's button, when the cursor is in that
    /// section's own group; any other group means the section was not what the focus was on, and it fades with
    /// none). Called before [`follow`](Self::follow) drops the section, by every path that closes one.
    pub fn hold_focus(&mut self, section: Section, focus: GroupId, btn: i32) {
        let own = match section {
            Section::Chapters => GROUP_STRIP,
            Section::Info => GROUP_SECTION,
        };
        self.held[section_index(section)] = (focus == own).then(|| usize::try_from(btn).ok()).flatten();
    }

    /// **`section` was dismissed on the frame `now`** (the `Tick::ms` of the key or event that put it away): its spring
    /// takes its first step on the next frame. Called with [`follow`](Self::follow) by every path that closes one.
    pub fn dismissed_on(&mut self, section: Section, now: u32) {
        self.dismissed[section_index(section)] = Some(now);
    }

    /// The chapter card the closing strip paints as focused, if it fades with one.
    pub fn held_card(&self) -> Option<usize> {
        self.held[section_index(Section::Chapters)]
    }

    /// The button the closing Info card paints as focused, if it fades with one.
    pub fn held_button(&self) -> Option<usize> {
        self.held[section_index(Section::Info)]
    }

    /// Step the sections' appear springs, once per frame from the screen's tick and never from a draw. A frame is
    /// owed for as long as one moves: the spring notes `PresentEvent::Motion` on the handle the dispatcher gives
    /// every machine and its surfaces alike, so no fingerprint term is needed. The Chapters strip's Shelf is
    /// forgotten once its section has faded out.
    pub fn step_appear(&mut self, t: Tick, present: &mut PresentHandle<'_>) {
        for (spring, dismissed) in self.appear.iter_mut().zip(self.dismissed) {
            if dismissed == Some(t.ms) {
                present.note(plx_machine::present::PresentEvent::Motion);
                continue;
            }
            spring.tick(t, present);
        }
        for section in SECTIONS {
            if faded(&self.appear[section_index(section)]) && self.wants & section.open_bit() == 0 {
                self.held[section_index(section)] = None;
            }
        }
        if faded(&self.appear[section_index(Section::Chapters)]) && self.wants & Section::Chapters.open_bit() == 0 {
            self.strip = None;
        }
    }

    /// Is `section`'s spring still held at its first frame (`PopoverMotion::held`)?
    pub fn section_held(&self, section: Section) -> bool {
        self.appear[section_index(section)].held()
    }

    /// `section`'s appear this frame: `0` held or gone, `1` at rest.
    pub fn section_appear(&self, section: Section) -> f32 {
        self.appear[section_index(section)].appear
    }

    /// Put `section` at appear `a`, opening: the springs a test draws at a chosen point of the motion instead of
    /// stepping to it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_section_appear_for_test(&mut self, section: Section, a: f32) {
        let mut spring = PopoverMotion::at(a);
        spring.to(1.0);
        self.appear[section_index(section)] = spring;
        self.wants |= section.open_bit();
    }

    /// **The sections that are PRESENTED**: open, or closed and still fading out (`StateKey::open` bits). The
    /// draw's mask; the logical one, the screen's `HudState::sections`, still decides stops, groups and keys.
    pub fn presented(&self) -> u8 {
        SECTIONS
            .into_iter()
            .filter(|s| !faded(&self.appear[section_index(*s)]))
            .fold(0, |bits, s| bits | s.open_bit())
    }

    /// The strip's Shelf for a strip of this `geo`, under `entry`: held across frames so its springs live, and
    /// rebuilt when the strip it drew is not this one (another entry or another card geometry).
    pub fn strip(&mut self, entry: EntryId, geo: &super::strip::Geometry) -> &mut Shelf {
        let style = geo.style();
        let same = |s: &Shelf| {
            let old = s.style();
            s.entry() == entry
                && (old.w, old.h, old.gap, old.margin_x, old.radius) == (style.w, style.h, style.gap, style.margin_x, style.radius)
                && old.label_drop == style.label_drop
        };
        if !self.strip.as_ref().is_some_and(same) {
            self.strip = Some(Shelf::new(entry, style));
        }
        self.strip.as_mut().expect("the strip's shelf was just built")
    }

    /// The strip's Shelf as it is, for a read that must not build one (the screen's `Focusable` and its stops).
    /// `None` until [`strip`](Self::strip) has built it, which the screen does when the Chapters section opens.
    pub fn strip_shelf(&self) -> Option<&Shelf> {
        self.strip.as_ref()
    }

    /// Forget the strip's Shelf: the Chapters section closed, so the next opening builds one at rest rather than
    /// resuming the last opening's scroll.
    pub fn drop_strip(&mut self) {
        self.strip = None;
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
    /// `focus == GROUP_ROW` pops the control column's `pop` and `focus == GROUP_SECTION` pops
    /// [`section_pop`](Self::section_pop); any other group closes both. The index is bounded by
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
        // an open section's buttons pop on `GROUP_SECTION`; the index is bounded by what a segment can hold. A card
        // fading out keeps the pop of the button it was on (the page's focus has already left it).
        let section = match focus == GROUP_SECTION {
            true => usize::try_from(btn).ok(),
            false => self.held_button(),
        }
        .filter(|&i| i < MAX_ROW_ITEMS);
        self.section_pop.step(section, dt);
    }

    /// Section button `i`'s focus pop this frame.
    pub fn section_scale(&self, i: usize) -> f32 {
        self.section_pop.scale(i)
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

