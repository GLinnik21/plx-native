//! The §6 container tests (spec §15.1), over the fixture bundle. What these grade is the
//! container's DECISIONS — which entry hears what, in which order, and who owns input — never a
//! pixel: the surfaces' scrims and the dip's colour are device captures.

use super::modal::{HostRender, HostUpdate, Phase, Style};
use super::stack::CAP;

#[test]
fn restored_live_and_evicted_bodies_receive_memory_before_enter() {
    for evict in [false, true] {
        let (mut d, mut rig, _) = booted();
        let root = d.nav.top_page().unwrap().id;
        let count = if evict { CAP + 1 } else { 1 };
        for i in 0..count {
            d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(20 + i as u32)));
            let report = d.frame(&mut rig, tick(16 + i as u32 * 16), vec![], vec![], &mut NoTap);
            d.prune(&report.unmounted);
        }
        assert_eq!(d.nav.entry(root).unwrap().inst.is_none(), evict);
        d.request(MachineId::Nav, NavOp::PopTo(root));
        let report = d.frame(&mut rig, tick(400), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        let events = events_of(&d, 0);
        let memory = events.rfind("\"restore_memory\"").expect("return hydration is a lifecycle event");
        let enter = events.rfind("\"enter\"").unwrap();
        assert!(memory < enter, "memory precedes restored Enter, evicted={evict}: {events}");
    }
}
use super::transition::PageDip;
use crate::ui::dispatch::{Dispatcher, NoTap};
use crate::ui::fixture::{booted, events_of, key, tick, FixtureArg, FixtureHost, FixtureRig};
use crate::ui::machine::{EntryId, FocusKey, InputOwner, Key, MachineId, NavOp};

fn open_modal(d: &mut Dispatcher<FixtureHost>, rig: &mut FixtureRig, style: Style, ms: u32) -> EntryId {
    d.nav.next_style = style;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(rig, tick(ms), vec![], vec![], &mut NoTap);
    d.nav.modals.top().expect("presented").entry.id
}

/// **A surface's modal dim is drawn INSIDE THE PAGE PASS, so the host snapshot carries it**
/// (spec §6.2, §8.3, and `ModalStack::draw_scrims`'s own doc).
///
/// The snapshot a Cached host is served from is taken at the end of the page pass, and it is what
/// the surface's own glass looks through. A dim drawn WITH the panel reaches the visible frame and
/// never that texture, so the frosted ground comes out at full page brightness inside a dimmed
/// screen — `account_menu`'s reported bug, answered until now by two hand-placed `draw_scrim()`
/// calls in the loop's page closure, a list no third panel could join.
///
/// What a host test can see of that is the ORDER, which is the property itself: the container asks
/// the surface for its `Scrim` after the host page has drawn and before the surface's own `draw`.
/// The capture itself is `popover::host::PagePass`'s drop and needs a GL context, so it is not
/// what this grades; the loop opens that guard around the whole closure, so "between the page and
/// the panel" IS "inside the snapshot".
///
/// Observed RED before `draw_scrims` was wired into `Dispatcher::draw_with`: with `Screen::scrim`
/// on the trait but nobody asking, `scrim_at` stayed 0 and the first assertion failed.
#[test]
fn a_cached_hosts_snapshot_carries_the_surfaces_scrim() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(d.host_policy().1, HostRender::Cached, "a Sheet caches its host");
    // The fixture's dim stays 0 for the DRAW: a host test has no GL context, so `draw_scrims`'
    // paint is unreachable here and the alpha ladder is graded separately, on the pure
    // `ModalStack::scrims` (`the_scrim_ladder_scales_by_appear_and_the_route_dip` below). Every
    // eligible surface is ASKED whatever its answer, which is what makes the order observable.
    d.draw(&mut rig, true);

    let page_at = d.nav.entry(home).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureScreen>().unwrap().draw_at;
    let modal = modal_of(&d, id);
    let (scrim_at, panel_at) = (modal.scrim_at.get(), modal.draw_at);
    assert!(scrim_at > 0, "the container never asked the surface for its scrim");
    assert!(
        page_at < scrim_at,
        "the dim went down before the host page: page={page_at} scrim={scrim_at}"
    );
    assert!(
        scrim_at < panel_at,
        "the dim went down with the panel rather than in the page pass: \
         scrim={scrim_at} panel={panel_at}"
    );
}

/// The other half of the rule the container owns: the SURFACES pass never asks for a scrim, so a
/// dim cannot be drawn twice, and a page is never asked at all.
#[test]
fn the_page_pass_is_the_only_place_a_scrim_is_asked_for() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    let once = modal_of(&d, id).scrim_at.get();
    assert!(once > 0, "the page pass asked");
    // `pages: false` is the loop's SECOND call, over the surfaces alone (`bridge::page_plan`'s
    // `LegacyThenSurfaces`/`SurfacesOnly`): the page pass did not run, so no dim is owed.
    d.draw(&mut rig, false);
    assert_eq!(modal_of(&d, id).scrim_at.get(), once, "a surfaces-only pass draws no dim");
}

/// An `Opaque` surface is the mechanism's one documented boundary: it REPLACES its host once the
/// ground is drawn, so there is no page pass to draw into and no snapshot for a dim to belong to
/// — its dim is composed with its own ground, in its own `draw`.
#[test]
fn an_opaque_surface_is_never_asked_for_a_page_scrim() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 16);
    modal_mut(&mut d, id).scrim_alpha = 0.5;
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    d.draw(&mut rig, true);
    assert_eq!(
        modal_of(&d, id).scrim_at.get(),
        0,
        "an opaque surface's dim is its own ground's, not the page pass's"
    );
    assert!(d.nav.modals.scrims(1.0).is_empty(), "…so it owes the page nothing");
}

/// The alpha the container computes, without the paint: the surface's declared peak times its own
/// appear spring times the route transition's dip. A panel left at full strength over a page
/// fading to the app ground is the one thing on screen saying the transition is not happening
/// (`Popover::painter`'s rule, carried over), and a surface that declares no dim owes none.
#[test]
fn the_scrim_ladder_scales_by_appear_and_the_route_dip() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    assert!(d.nav.modals.scrims(1.0).is_empty(), "a surface with no dim declared owes none");
    modal_mut(&mut d, id).scrim_alpha = 0.5;
    // mid-ramp: the dim rides the appear spring rather than snapping on with the panel
    d.nav.modals.surface_mut(id).unwrap().motion = super::modal::PopoverMotion::at(0.4);
    let ramping = d.nav.modals.scrims(1.0);
    assert_eq!(ramping.len(), 1);
    assert_eq!(ramping[0].0, id);
    assert!((ramping[0].1 - 0.2).abs() < 1e-6, "0.5 x 0.4 appear, got {}", ramping[0].1);
    // …and a route change dips it with the page underneath
    let dipped = d.nav.modals.scrims(0.5);
    assert!((dipped[0].1 - 0.1).abs() < 1e-6, "…x 0.5 page alpha, got {}", dipped[0].1);
    // settled and no transition: exactly what the surface declared
    d.nav.modals.surface_mut(id).unwrap().motion = super::modal::PopoverMotion::at(1.0);
    assert!((d.nav.modals.scrims(1.0)[0].1 - 0.5).abs() < 1e-6);
}

/// The production dispatcher supplies one borrowed chrome projection to the bare lift callback;
/// it must include the profile, labels and the material the normal chrome draw just published.
#[test]
fn the_scrim_callback_receives_the_normal_chromes_borrowed_frame_read() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SEEN: AtomicBool = AtomicBool::new(false);
    fn lift(read: crate::ui::screen::ScrimLiftRead<'_>) {
        let chrome = read.chrome.expect("the fixture rig supplied chrome");
        assert_eq!(chrome.profile.name.to_bytes(), b"Captured A");
        assert_eq!(chrome.profile.initial.to_bytes(), b"A");
        assert_eq!(chrome.labels.labels, ["Home", "Movies", ""]);
        assert!((chrome.chip_expand - 0.625).abs() < 1e-6);
        let face = read.bar_material.expect("the frame plan supplied the tab face");
        assert_eq!(face.scrim_top, [0.1, 0.2, 0.3, 0.4]);
        SEEN.store(true, Ordering::Relaxed);
    }

    let _guard = crate::testlock::serial();
    SEEN.store(false, Ordering::Relaxed);
    let (mut d, mut rig, _) = booted();
    rig.seed_scrim_chrome_for_test("Captured A", "A", &["Home", "Movies", ""], 0.625);
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    let modal = modal_mut(&mut d, id);
    modal.scrim_alpha = 0.5;
    modal.scrim_lift = Some(lift);
    d.nav.modals.surface_mut(id).unwrap().motion = super::modal::PopoverMotion::at(1.0);
    let mut glass = crate::ui::frame::glass::GlassPlan::new();
    glass.set_tab_face_for_test(crate::gfx::GlassFace {
        scrim_top: [0.1, 0.2, 0.3, 0.4],
        scrim_bot: [0.5, 0.6, 0.7, 0.8],
        rim: [0.0; 4], rim_lit: [0.0; 4], rim_w: 1.0,
    });
    let read = crate::ui::dispatch::scrim_lift_read::<FixtureHost>(&rig, Some(&glass));
    let lifts = d.nav.modals.scrims(1.0);
    assert_eq!(lifts.len(), 1, "the live surface contributes exactly one lift");
    (lifts[0].2)(read);
    assert!(SEEN.load(Ordering::Relaxed), "the dispatcher never invoked the lift callback");
}

// ── The inherited dim: one field per stack, latched from a page no dim has touched ────────────

/// A framebuffer that remembers what was painted on it: one grey level, darkened by every dim
/// exactly as `scrim_black(a)` over it would. `kick` queues it as a flat grid that `collect` hands
/// back once a frame has ended — `gfx::field_kick`/`field_collect`'s contract — so a field latched
/// from it says, in its `key`, which picture it saw, and the events say WHEN it was read.
struct FakeFb {
    level: f32,
    epoch: u32,
    refuse: bool,
    video_plane: bool,
    events: Vec<&'static str>,
    /// Drawn frames so far (`gfx::field_frame_end`).
    swaps: u32,
    /// Chain runs so far; a later kick reuses the one set of targets.
    runs: u32,
    /// What the last run reduced.
    reduced: f32,
    /// Is a read in flight (`DimSink::in_flight`)?
    in_flight: bool,
    /// Did this frame capture the host (`DimSink::captured`)?
    captured: bool,
}

impl FakeFb {
    fn new(level: f32) -> Self {
        Self {
            level,
            epoch: 0,
            refuse: false,
            video_plane: false,
            events: Vec::new(),
            swaps: 0,
            runs: 0,
            reduced: 0.0,
            in_flight: false,
            captured: false,
        }
    }
    /// The previous frame is swapped and the page is drawn again (live, or served from the
    /// snapshot) at the start of the next.
    fn frame(&mut self, level: f32) {
        self.swaps += 1;
        self.level = level;
        self.events.clear();
    }
}

impl super::modal::DimSink for FakeFb {
    fn video_plane(&self) -> bool {
        self.video_plane
    }
    fn page_epoch(&self) -> u32 {
        self.epoch
    }
    fn captured(&self) -> bool {
        self.captured
    }
    fn kick(&mut self) -> Option<crate::gfx::FieldTicket> {
        if self.refuse {
            return None;
        }
        self.events.push("kick");
        self.runs += 1;
        self.reduced = self.level;
        Some(crate::gfx::FieldTicket::for_test(self.runs, self.swaps))
    }
    fn collect(&mut self, t: crate::gfx::FieldTicket) -> crate::gfx::FieldRead {
        use crate::gfx::{field_ticket_state, FieldRead, TicketState};
        match field_ticket_state(t, self.runs, self.swaps, true) {
            TicketState::Due => {
                self.events.push("collect");
                FieldRead::Ready([[self.reduced; 3]; crate::gfx::FIELD_CELLS])
            }
            TicketState::Pending => FieldRead::Pending,
            TicketState::Lost => FieldRead::Lost,
        }
    }
    fn in_flight(&mut self, pending: bool) {
        self.in_flight = pending;
    }
    fn dim(&mut self, _field: &crate::ui::underlay::UnderlayField, alpha: f32) {
        self.events.push("dim");
        self.level *= 1.0 - alpha;
    }
}

fn two_dimming_sheets(d: &mut Dispatcher<FixtureHost>, rig: &mut FixtureRig) -> (EntryId, EntryId) {
    let a = open_modal(d, rig, Style::Sheet, 16);
    let b = open_modal(d, rig, Style::Sheet, 32);
    for id in [a, b] {
        modal_mut(d, id).scrim_alpha = 0.5;
        d.nav.modals.surface_mut(id).unwrap().motion = super::modal::PopoverMotion::at(1.0);
    }
    (a, b)
}

fn key_level(d: &Dispatcher<FixtureHost>) -> f32 {
    d.nav.modals.underlay.field().key()[0]
}

/// Draw one frame's dims through `fb`.
fn dims_frame(d: &mut Dispatcher<FixtureHost>, rig: &FixtureRig, fb: &mut FakeFb) {
    let read = crate::ui::dispatch::scrim_lift_read::<FixtureHost>(rig, None);
    d.nav.modals.draw_scrims_on(1.0, read, fb);
}

/// **The field is latched from the page BEFORE any dim is on it — and so it can never inherit its
/// own dim** (the one property the whole mechanism rests on: a dim keyed to a dimmed picture of
/// itself darkens a little more every time it is re-read).
///
/// Two sheets at 0.5 over a page of grey 0.5. The field must read 0.5, not 0.25 or 0.125, and the
/// reduction must be QUEUED before both dims in the frame's paint order. The next frame at the same
/// host epoch must not read again at all, even though the framebuffer it would see now carries two
/// dims.
///
/// Observed RED with `underlay.sync` moved after the dim loop in `draw_scrims_on`: the events were
/// `["dim", "dim", "sample"]` (the read was one synchronous `sample` then).
#[test]
fn the_dims_field_is_latched_from_the_undimmed_page_before_the_first_dim() {
    let (mut d, mut rig, _) = booted();
    let _ = two_dimming_sheets(&mut d, &mut rig);
    let mut fb = FakeFb::new(0.5);

    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["kick", "dim", "dim"], "the reduction is queued before every dim");
    assert!((fb.level - 0.125).abs() < 1e-6, "and both dims still landed, bottom to top");

    // The next frame reads what was queued — the undimmed page, whatever is on the framebuffer now.
    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["collect", "dim", "dim"], "the read lands before this frame's dims");
    assert!((key_level(&d) - 0.5).abs() < 2e-3, "latched from the UNDIMMED page, got {}", key_level(&d));

    // Same host snapshot: the page has not been re-captured, so the field is not re-read — even
    // though what is on the framebuffer between frames is the dimmed picture.
    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["dim", "dim"], "no re-read while the host snapshot stands");
    assert!((key_level(&d) - 0.5).abs() < 2e-3);
}

/// **The page read never stalls the frame that asked for it.** A `glReadPixels` of work queued on
/// the same frame waits for the GPU to draw everything submitted so far: measured on the
/// television (2026-09-19, `FRAMEDROP … spans=…fieldread:25.7`), 26–37 ms of every modal's
/// 60–69 ms open frame, when the read ran inside `draw_scrims` on the frame the host was captured.
/// So the read is due only once a drawn frame has ended since the kick — and until it lands, the
/// loop is kept turning for it (a settled stack may otherwise stop presenting, and the read would
/// wait for the keepalive) and the host's ground stage, which bakes the dim in, is held off.
#[test]
fn the_page_read_lands_a_frame_after_it_is_queued_and_holds_the_ground_until_then() {
    let (mut d, mut rig, _) = booted();
    let _ = two_dimming_sheets(&mut d, &mut rig);
    let mut fb = FakeFb::new(0.5);

    dims_frame(&mut d, &rig, &mut fb);
    assert!(!fb.events.contains(&"collect"), "no read on the frame the reduction was queued");
    assert!(!d.nav.modals.underlay.field().is_latched(), "the open frame's dim is the flat ink");
    assert!(fb.in_flight, "a read in flight keeps the loop turning and holds the ground");

    // A second draw of the SAME frame (a blur source pass re-renders the page) still may not read.
    dims_frame(&mut d, &rig, &mut fb);
    assert!(!fb.events.contains(&"collect"));
    assert_eq!(fb.events.iter().filter(|e| **e == "kick").count(), 1, "and does not queue twice");

    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert!(d.nav.modals.underlay.field().is_latched());
    assert!(!fb.in_flight, "landed: the ground may be taken and the loop may rest");
}

/// **The page is read on the frame that captured it, before any dim is seen.** That frame's GPU
/// work is waited out before anything else presents (`gfx::snapshot_frame_begin`), so the
/// reduction queued there costs no presented frame — queued a frame later, it was the GPU backlog
/// the frame after THAT paid (20–24 ms, television, 2026-09-19). Without a capture on the frame, a
/// held surface still queues nothing: there is no fence to hide the reduction behind.
#[test]
fn the_page_is_read_on_the_capture_frame_and_not_on_a_held_frame_without_one() {
    let (mut d, mut rig, _) = booted();
    let a = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    modal_mut(&mut d, a).scrim_alpha = 0.5;
    d.nav.modals.surface_mut(a).unwrap().motion = super::modal::PopoverMotion::at(0.0);
    let mut fb = FakeFb::new(0.5);

    dims_frame(&mut d, &rig, &mut fb);
    assert!(fb.events.is_empty(), "held, no capture this frame: nothing queued, nothing dimmed");

    fb.frame(0.5);
    fb.captured = true;
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["kick"], "the capture frame queues the read, and dims nothing");

    fb.frame(0.5);
    fb.captured = false;
    d.nav.modals.surface_mut(a).unwrap().motion = super::modal::PopoverMotion::at(1.0);
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["collect", "dim"], "the first dim stands on the landed field");
}

/// **A read whose targets were reused is asked for again, not adopted.** The chain's targets are
/// shared with every other reader (`RouteGround`'s ground latches off the same chain), so a run in
/// between leaves another page's field in them.
#[test]
fn a_lost_page_read_is_queued_again_rather_than_adopted() {
    let (mut d, mut rig, _) = booted();
    let _ = two_dimming_sheets(&mut d, &mut rig);
    let mut fb = FakeFb::new(0.5);
    dims_frame(&mut d, &rig, &mut fb);

    // Someone else ran the chain before the read was due.
    fb.runs += 1;
    fb.reduced = 0.9;
    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["kick", "dim", "dim"], "lost: re-queued from this frame's page");
    assert!(!d.nav.modals.underlay.field().is_latched(), "the other reader's field is never adopted");

    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert!((key_level(&d) - 0.5).abs() < 2e-3, "got {}", key_level(&d));
}

/// **A re-captured host re-latches; a refused read keeps what it had; the last dismissal resets.**
#[test]
fn a_recaptured_host_relatches_and_the_last_dismissal_resets_the_field() {
    let (mut d, mut rig, _) = booted();
    let (a, b) = two_dimming_sheets(&mut d, &mut rig);
    let mut fb = FakeFb::new(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    fb.frame(0.5);
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(d.nav.modals.underlay.held(), super::modal::Latched::Page(0));

    // The page under the stack changed and `popover::host` re-took its snapshot: the field keeps
    // the old page's light for the one frame the new read is in flight…
    fb.frame(0.8);
    fb.epoch = 1;
    dims_frame(&mut d, &rig, &mut fb);
    assert_eq!(fb.events, ["kick", "dim", "dim"], "a new host snapshot is read again, first");
    assert!((key_level(&d) - 0.5).abs() < 2e-3, "the old light stands while the read is in flight");
    assert_eq!(d.nav.modals.underlay.held(), super::modal::Latched::Page(0));
    // …and follows it once the read lands.
    fb.frame(0.8);
    dims_frame(&mut d, &rig, &mut fb);
    assert!((key_level(&d) - 0.8).abs() < 2e-3, "…and the field follows it, got {}", key_level(&d));
    assert_eq!(d.nav.modals.underlay.held(), super::modal::Latched::Page(1));

    // A frame with no honest read (a blur source pass, a frozen page): the field it had stands
    // rather than dropping to the flat ink for a frame, and the read is owed again next frame.
    fb.frame(0.3);
    fb.epoch = 2;
    fb.refuse = true;
    dims_frame(&mut d, &rig, &mut fb);
    assert!(d.nav.modals.underlay.field().is_latched(), "a refusal keeps the field");
    assert!((key_level(&d) - 0.8).abs() < 2e-3);
    assert_eq!(d.nav.modals.underlay.held(), super::modal::Latched::Page(1), "still owed");
    assert!(!fb.in_flight, "a refusal queued nothing to wait for");

    // Both sheets leave: once the stack is empty the field is re-armed for whatever comes next.
    // (`hide` rather than a dismissal stepped through `Dispatcher::frame`: a frame DRAWS, and a
    // host test has no GL context for a dim at a nonzero alpha to be painted into.) One leaving
    // is not enough — the other still dims the same page.
    assert!(d.nav.modals.hide(b));
    d.nav.modals.prune();
    assert!(d.nav.modals.underlay.field().is_latched(), "a surface is still up over the page");
    assert!(d.nav.modals.hide(a));
    d.nav.modals.prune();
    assert!(d.nav.modals.is_empty(), "both surfaces retired");
    assert!(!d.nav.modals.underlay.field().is_latched(), "the last dismissal resets the field");
    assert_eq!(d.nav.modals.underlay.held(), super::modal::Latched::Nothing);
}

/// The latch policy, as the pure table it is.
#[test]
fn the_latch_policy_reads_the_page_once_per_snapshot_and_never_over_the_video_plane() {
    use super::modal::{latch_step, LatchStep as S, Latched as L};
    use crate::ui::screen::UnderlaySource as U;
    let c = [[0.2, 0.4, 0.1]; 4];
    let c2 = [[0.4, 0.1, 0.2]; 4];
    // the page, never read / re-captured / unchanged
    assert_eq!(latch_step(Some(U::Page), L::Nothing, 3, false), S::SamplePage);
    assert_eq!(latch_step(Some(U::Page), L::Page(2), 3, false), S::SamplePage);
    assert_eq!(latch_step(Some(U::Page), L::Page(3), 3, false), S::Keep);
    // framebuffer 0 is the punch-through hole on a video-plane frame: never read it
    assert_eq!(latch_step(Some(U::Page), L::Nothing, 3, true), S::Keep);
    // the video plane's stand-in: an envelope, re-latched only when it changes
    assert_eq!(latch_step(Some(U::Corners(c)), L::Nothing, 0, true), S::Corners(c));
    assert_eq!(latch_step(Some(U::Corners(c)), L::Corners(c), 0, true), S::Keep);
    assert_eq!(latch_step(Some(U::Corners(c2)), L::Corners(c), 0, true), S::Corners(c2));
    // nothing to inherit, or nobody dimming: the flat ink
    assert_eq!(latch_step(Some(U::Flat), L::Corners(c), 0, true), S::Reset);
    assert_eq!(latch_step(None, L::Page(1), 1, false), S::Reset);
    assert_eq!(latch_step(None, L::Nothing, 1, false), S::Keep);
}

/// **At `TINT == 0` the inherited dim IS today's flat scrim, to the bit, at every role's weight** —
/// the one knob that turns the whole family's inheritance off must turn it off exactly. And an
/// unlatched field (nothing read yet, or a video-plane item with no envelope) is the flat ink
/// whatever `TINT` is.
#[test]
fn a_zero_tint_and_an_unlatched_field_are_the_flat_scrim_bit_for_bit() {
    use crate::ui::theme::{scrim_black, underlay as u};
    use crate::ui::underlay::{plan, Draw};
    for role in [u::DIM_COMPACT, u::DIM_PANEL, u::DIM_SHEET, u::DIM_DECISION, u::DIM_PLAYER, u::DIM_PROSE] {
        for appear in [0.0, 0.25, 1.0] {
            let a = role * appear;
            assert_eq!(plan(true, 0.0, a), Draw::Flat(scrim_black(a)));
            assert_eq!(plan(false, u::TINT, a), Draw::Flat(scrim_black(a)));
            assert_eq!(plan(true, u::TINT, a), Draw::Field([u::TINT, u::TINT, u::TINT, a]));
        }
    }
    assert!(u::TINT > 0.0 && u::TINT < 1.0, "the dim inherits some light and stays a dim");
}

/// **No surface states its own dim weight.** Every modal dim is a `theme::underlay` role; a literal
/// alpha handed to `Scrim::dim`/`lifting`/`over_video` or to `Popover::scrim`, a private
/// `SCRIM_A` constant, or a hand-drawn `scrim_black` in a panel that the container now dims, is
/// the per-screen number this mechanism exists to remove. Pinned from source because a value can
/// only be proven to come from the table by where it is spelled.
#[test]
fn no_surface_states_its_own_dim_weight() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut dirs = vec![root.join("ui"), root.join("screens")];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                files.push(p);
            }
        }
    }
    assert!(files.len() > 50, "the walk found the tree ({} files)", files.len());
    let literal_after = |line: &str, call: &str| -> bool {
        line.match_indices(call).any(|(i, _)| {
            line[i + call.len()..].trim_start().chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.')
        })
    };
    // the panels whose dim the container paints now: any black sheet here is a second dim
    let dimmed_by_the_container = ["more_menu.rs", "track_menu.rs", "modal.rs"];
    let mut bad = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        if name == "theme.rs" || name.ends_with("tests.rs") {
            continue;
        }
        let src = std::fs::read_to_string(f).unwrap();
        for (n, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let hit = ["Scrim::dim(", "Scrim::lifting(", "Scrim::over_video(", ".scrim("]
                .iter()
                .any(|c| literal_after(code, c))
                || code.contains("const SCRIM_A")
                || (dimmed_by_the_container.contains(&name.as_str()) && code.contains("scrim_black("));
            if hit {
                bad.push(format!("{}:{}: {}", f.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(bad.is_empty(), "a modal dim weight outside theme::underlay:\n{}", bad.join("\n"));
}

fn modal_of(d: &Dispatcher<FixtureHost>, id: EntryId) -> &crate::ui::fixture::FixtureModal {
    d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureModal>().unwrap()
}

fn modal_mut(d: &mut Dispatcher<FixtureHost>, id: EntryId) -> &mut crate::ui::fixture::FixtureModal {
    d.nav.entry_mut(id).unwrap().inst.as_mut().unwrap().screen.as_any_mut().unwrap()
        .downcast_mut::<crate::ui::fixture::FixtureModal>().unwrap()
}

#[test]
fn a_request_freezes_inactive_group_cursors_before_focus_moves_during_the_fade() {
    use crate::ui::machine::GroupId;
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.set_focus_in(Some(FocusKey { entry: home, elem: 2 }), Some(GroupId(71)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 1 }), Some(GroupId(1)));
    let saved = d.return_state();
    d.nav.tabs.stack.transition = Box::new(PageDip::new());
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 4 }), Some(GroupId(71)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    for ms in (32..=192).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, saved.focus);
    assert_eq!(d.nav.entry(home).unwrap().ret.remembered, saved.remembered);
    d.request(MachineId::Nav, NavOp::Pop);
    for ms in (208..=448).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.nav.top_page().unwrap().id, home);
    assert!(d.return_state().remembered.contains(&(GroupId(71), 2)));
    assert!(!d.return_state().remembered.contains(&(GroupId(71), 4)));
}

#[test]
fn filmography_detail_back_restores_the_same_modal_instance_and_cursor() {
    use crate::ui::machine::GroupId;
    let (mut d, mut rig, _) = booted();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(1)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let person = d.nav.top_page().unwrap().id;
    let person_focus = FocusKey { entry: person, elem: 1 };
    d.set_focus_in(Some(person_focus), Some(GroupId(1)));
    let filmography = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let instance = d.nav.instance_of(filmography).unwrap();
    let focus = FocusKey { entry: filmography, elem: 0 };
    d.set_focus_in(Some(focus), Some(GroupId(9)));
    // Page(2) deliberately pushes Page(3) on Enter in FixtureScreen; use a passive page.
    d.request(MachineId::Instance(instance), NavOp::Push(FixtureArg::Page(20)));
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert!(d.nav.modals.is_empty(), "Filmography is not drawn over Detail");
    assert_eq!(d.nav.instance_of(filmography), Some(instance), "the covered surface stays mounted");
    let report = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert!(!report.ticked.contains(&instance), "the covered Filmography does not animate");
    d.request(MachineId::Nav, NavOp::Pop);
    d.frame(&mut rig, tick(80), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.top_page().unwrap().id, person);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(filmography)));
    assert_eq!(d.nav.instance_of(filmography), Some(instance));
    assert_eq!(d.focus(), Some(focus));
    assert_eq!(d.nav.entry(person).unwrap().ret.focus, Some(person_focus), "the modal did not overwrite its host's return state");
    d.request(MachineId::Nav, NavOp::Dismiss(filmography));
    d.frame(&mut rig, tick(96), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(person)));
    assert_eq!(d.focus(), Some(person_focus), "dismiss restores the Person entry control, not Filmography's row key");
}

#[test]
fn scoped_surface_bodies_are_bounded_and_remount_after_their_owner_data_lands() {
    let (mut d, mut rig, _) = booted();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(901)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let owner = d.nav.top_page().unwrap().id;
    let child = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let original = d.nav.instance_of(child).unwrap();
    let focus = d.focus();
    let mut ms = 48;
    for n in 0..CAP + 2 {
        d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(100 + n as u32)));
        let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        ms += 16;
        open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, ms);
        ms += 16;
        assert!(d.nav.bodies().count() <= 2 * CAP, "a child body is bounded by its owner's lifetime");
    }
    assert!(d.nav.entry(owner).unwrap().inst.is_none());
    assert!(d.nav.entry(child).unwrap().inst.is_none());
    assert_eq!(d.nav.entry(child).unwrap().ret.focus, focus);
    d.request(MachineId::Nav, NavOp::PopTo(owner));
    let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    d.prune(&report.unmounted);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.pending_surface(), Some(child));
    assert!(d.nav.entry(child).unwrap().inst.is_none(), "remount waits for owner data, not just owner body");
    rig.store.view.items.push(7);
    d.store_changed(crate::ui::machine::StoreOrd(0), 1);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(child)));
    assert_ne!(d.nav.instance_of(child), Some(original), "eviction creates a fresh body");
    assert!(d.nav.instance_of(child).is_some());
    assert_eq!(d.focus(), focus, "the EntryId and its engine focus survive body eviction");
    assert!(d.nav.bodies().count() <= 3);
}

#[test]
fn an_owned_surface_finishes_opening_independently_of_legacy_page_alpha() {
    let (mut d, mut rig, _) = booted();
    rig.page_alpha = 0.52;
    let id = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 16);
    for ms in (32..=9616).step_by(16) {
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        if d.nav.modals.surface(id).unwrap().phase == Phase::Open { break; }
    }
    let surface = d.nav.modals.surface(id).unwrap();
    assert_eq!(surface.phase, Phase::Open, "the surface must settle within the bounded frame budget");
    assert!(surface.motion.settled());
    let page = d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureModal>().unwrap();
    assert_eq!(page.last_draw_alpha, 1.0, "legacy page fading cannot cap the surface's own slide/opacity");
}

#[test]
fn one_navigation_snapshot_reaches_draw_frames_without_mutating_logical_state() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    rig.page_alpha = 0.21;
    rig.chrome_alpha = 0.37;
    rig.view_tab = Some(2);
    rig.blur_amount = 0.63;
    let before = d.state_hash();
    let reads = rig.navigation_reads.get();
    d.draw(&mut rig, true);
    let modal = d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureModal>().unwrap();
    assert_eq!(modal.last_navigation.chrome_alpha, 0.37);
    assert_eq!(modal.last_navigation.view_tab, Some(2));
    assert_eq!(modal.last_navigation.blur_amount, 0.63);
    assert_eq!(rig.navigation_reads.get(), reads + 1, "one snapshot, not live reads per screen");
    assert_eq!(d.state_hash(), before, "presentation is render state, not a logical mutation");
}

/// Present a Sheet (the account menu's shape): the page beneath is FROZEN and CACHED and
/// receives no Tick; dismiss it: the phase is Closing on the SAME frame, the host is live again,
/// and the surface keeps stepping until `prune` clears it — then, and only then, it unmounts.
#[test]
fn the_closing_phase_is_stepped_even_when_the_host_is_frozen() {
    let (mut d, mut rig, home) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    let surf = d.nav.instance_of(id).unwrap();
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(r.host_update, Some(HostUpdate::Frozen));
    assert_eq!(r.host_render, Some(HostRender::Cached));
    assert!(!r.ticked.contains(&home), "a frozen host receives no Tick");
    assert!(r.ticked.contains(&surf), "the surface does");
    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));

    d.request(MachineId::Nav, NavOp::Dismiss(id));
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Closing, "Closing from the commit that dismissed");
    assert_ne!(d.nav.input_owner(), Some(InputOwner::Entry(id)), "input returned to the page");
    let r = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert_eq!(r.host_update, Some(HostUpdate::Live), "Closing: the host is live again");
    assert!(r.ticked.contains(&home));
    assert!(r.ticked.contains(&surf), "…and the closing surface still steps");
    // the fade runs down; prune clears it; the body unmounts
    let mut unmounted = false;
    for i in 0..120u32 {
        let r = d.frame(&mut rig, tick(80 + i * 16), vec![], vec![], &mut NoTap);
        if r.unmounted.contains(&surf) {
            unmounted = true;
            d.prune(&r.unmounted);
            break;
        }
        assert!(r.ticked.contains(&surf), "frame {i}: a Closing surface is stepped unconditionally");
    }
    assert!(unmounted, "prune cleared the Closing surface");
    assert!(d.nav.modals.is_empty());
    let home_ev = events_of(&d, 0);
    assert!(home_ev.contains("\"cover\""), "{home_ev}");
    assert!(home_ev.contains("\"uncover\""), "{home_ev}");
}

/// `host_page_lifecycle_tests` ported onto the fold table: a full-screen surface and the
/// profile menu freeze the hidden page; the item menu keeps it live.
#[test]
fn the_fold_freezes_the_page_under_a_sheet_and_keeps_it_live_under_a_compact_popover() {
    use super::modal::surface_policy;
    assert_eq!(surface_policy(Style::Sheet, Phase::Open, false).0, HostUpdate::Frozen);
    assert_eq!(surface_policy(Style::Opaque { snapshot: true }, Phase::Open, true), (HostUpdate::Frozen, HostRender::Replaced));
    assert_eq!(surface_policy(Style::Opaque { snapshot: false }, Phase::Opening, false), (HostUpdate::Frozen, HostRender::Live));
    assert_eq!(surface_policy(Style::Compact, Phase::Open, false), (HostUpdate::Live, HostRender::Cached));
    assert_eq!(surface_policy(Style::PlayerPanel { survives_failure: true }, Phase::Open, false), (HostUpdate::Live, HostRender::Live));
    // the fold: a Compact over a Sheet is still Frozen/Cached; Closing releases the freeze
    let (mut d, mut rig, _) = booted();
    open_modal(&mut d, &mut rig, Style::Sheet, 16);
    open_modal(&mut d, &mut rig, Style::Compact, 32);
    assert_eq!(d.nav.modals.host_policy(), (HostUpdate::Frozen, HostRender::Cached));
}

/// A screen the application never named: mounting it costs the fixture's `Mounter` one arm and
/// the container nothing.
#[test]
fn a_fixture_screen_mounts_with_no_app_change() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Compact, 16);
    let inst = d.nav.instance_of(id).unwrap();
    let s = d.nav.instance_mut(inst).unwrap();
    assert_eq!(s.screen.name(), "settings");
    let mut probe = String::new();
    s.screen.state().probe(&mut probe);
    assert!(probe.contains("\"mount\", \"enter\""), "{probe}");
}

/// The Settings family's BACK (§6.2 `SettingsSurface`): the surface walks its OWN stack — two
/// inner pushes, two inner pops — and only at its own depth 0 does BACK reach the container,
/// which dismisses the surface. The app's stack never moves.
#[test]
fn settings_back_walks_its_own_stack_not_the_apps() {
    let (mut d, mut rig, _) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap); // Home → Page(1)
    let depth = d.nav.tabs.stack.depth();
    let id = open_modal(&mut d, &mut rig, Style::Opaque { snapshot: true }, 32);
    let inner_depth = |d: &Dispatcher<FixtureHost>| {
        let inst = d.nav.instance_of(id).unwrap();
        let mut s = String::new();
        d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
        let _ = inst;
        s
    };
    d.frame(&mut rig, tick(48), vec![key(Key::Ok, tick(48))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(64), vec![key(Key::Ok, tick(64))], vec![], &mut NoTap);
    assert!(inner_depth(&d).contains("keys=2"), "two inner pushes: {}", inner_depth(&d));
    d.frame(&mut rig, tick(80), vec![key(Key::Back, tick(80))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(96), vec![key(Key::Back, tick(96))], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Opening, "two inner pops: the surface is still up");
    assert_eq!(d.nav.tabs.stack.depth(), depth, "the app's stack never moved");
    let r = d.frame(&mut rig, tick(112), vec![key(Key::Back, tick(112))], vec![], &mut NoTap);
    assert_eq!(d.nav.modals.top().unwrap().phase, Phase::Closing, "at its own depth 0, BACK dismisses");
    assert!(!r.back_at_root);
    assert_eq!(d.nav.tabs.stack.depth(), depth);
}

/// The spot a Detail was left at is captured at the PRESS (the request), on the entry, and a
/// BACK restores from it: `Uncover` + `Enter(Restored)` on the entry whose `ret` holds the key.
#[test]
fn back_off_a_detail_restores_the_spot_captured_at_the_press() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    let spot = FocusKey { entry: home, elem: 3 };
    d.set_focus(Some(spot));
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, Some(spot), "captured at the request");
    // focus moved on the page above; the capture is not overwritten
    d.set_focus(Some(FocusKey { entry: d.nav.top_page().unwrap().id, elem: 0 }));
    d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.nav.top_page().unwrap().id, home);
    assert_eq!(d.nav.entry(home).unwrap().ret.focus, Some(spot));
    let ev = events_of(&d, 0);
    assert!(ev.ends_with("\"uncover\", \"restore_memory\", \"enter\"]") || ev.contains("\"uncover\", \"restore_memory\", \"enter\""), "{ev}");
}

/// Detail → Person → Detail is three entries with three ids (§5.1: supersede-by-identity).
#[test]
fn person_detail_person_is_three_entries() {
    let (mut d, mut rig, _) = booted();
    for ms in [16, 32, 48] {
        d.frame(&mut rig, tick(ms), vec![key(Key::Ok, tick(ms))], vec![], &mut NoTap);
    }
    let ids: Vec<EntryId> = d.nav.tabs.stack.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 4, "Home + three pushes");
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "every entry has its own id: {ids:?}");
    assert_eq!(d.nav.tabs.stack.entries[1].arg, FixtureArg::Page(1));
    assert_eq!(d.nav.tabs.stack.entries[2].arg, FixtureArg::Page(2));
    assert_eq!(d.nav.tabs.stack.entries[3].arg, FixtureArg::Page(3));
}

/// A page's logical state survives a push over it and a pop back: the Library's section, scroll
/// and cursor are the instance's own while it lives (§6.1 tier 1) — nothing remounts.
#[test]
fn library_remembers_its_section_scroll_and_cursor_across_a_detail_push() {
    let (mut d, mut rig, home) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    let before = d.nav.instance_mut(home).unwrap().screen.state().hash();
    d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    let inst = d.nav.instance_of(d.nav.top_page().unwrap().id).unwrap();
    assert_eq!(inst, home, "the same body: no remount");
    let probe = events_of(&d, 0);
    assert!(probe.contains("keys=1"), "the key it handled is still counted: {probe}");
    assert_ne!(before, 0);
}

/// Search's query and shelves are the instance's; a result pushed over it and popped leaves them.
#[test]
fn search_keeps_its_query_and_shelves_across_a_result_push() {
    let (mut d, mut rig, home) = booted();
    // a result opened from Home (Page(1)), a key handled on Home first so its state is non-trivial
    d.frame(&mut rig, tick(16), vec![key(Key::Down, tick(16))], vec![], &mut NoTap);
    d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![], &mut NoTap);
    assert_eq!(d.nav.tabs.stack.depth(), 2);
    d.frame(&mut rig, tick(48), vec![key(Key::Back, tick(48))], vec![], &mut NoTap);
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.instance_of(d.nav.top_page().unwrap().id), Some(home));
    let probe = events_of(&d, 0);
    assert!(probe.contains("keys=1"), "{probe}");
    assert_eq!(probe.matches("\"cover\"").count(), 1);
    assert_eq!(probe.matches("\"uncover\"").count(), 1);
}

/// A profile switch drops every entry — surfaces first, then the pages top-down.
#[test]
fn switching_profile_drops_every_tab_instance() {
    let (mut d, mut rig, _) = booted();
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    open_modal(&mut d, &mut rig, Style::Compact, 32);
    assert_eq!(d.nav.bodies().count(), 3);
    d.reset_for_profile();
    let r = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert_eq!(r.unmounted.len(), 3);
    d.prune(&r.unmounted);
    assert_eq!(d.nav.bodies().count(), 0);
    assert_eq!(d.nav.tabs.stack.depth(), 0);
    assert!(d.nav.modals.is_empty());
    assert_eq!(d.nav.input_owner(), None);
    // the next Root rebuilds the tree
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert_eq!(d.top_screen().unwrap().name(), "home");
}

/// Push past `CAP`: the oldest body below the top is evicted (its `Unmount` delivered, its
/// inflight retired), the ENTRY and its `ReturnState` stay, and popping back to it remounts it
/// under the same `EntryId` with the focus it was left at.
#[test]
fn an_evicted_entry_keeps_its_focus_identity_on_remount() {
    let (mut d, mut rig, _) = booted();
    // The original test only inspected the saved key. The stronger engine assertion below
    // needs slot 5 to exist, so reconciliation cannot legitimately clamp it on remount.
    rig.store.view.items.resize(6, 0);
    d.store_changed(crate::ui::machine::StoreOrd(0), 1);
    d.frame(&mut rig, tick(1), vec![], vec![], &mut NoTap);
    let home = d.nav.top_page().unwrap().id;
    d.set_focus_in(Some(FocusKey { entry: home, elem: 2 }), Some(crate::ui::machine::GroupId(71)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 4 }), Some(crate::ui::machine::GroupId(72)));
    d.set_focus_in(Some(FocusKey { entry: home, elem: 5 }), Some(crate::ui::machine::GroupId(1)));
    let remembered = d.return_state().remembered;
    let mut evicted_at = None;
    for i in 0..(CAP as u32 + 2) {
        let ms = 16 * (i + 1);
        let r = d.frame(&mut rig, tick(ms), vec![key(Key::Ok, tick(ms))], vec![], &mut NoTap);
        if !r.unmounted.is_empty() && evicted_at.is_none() {
            evicted_at = Some(i);
        }
        d.prune(&r.unmounted);
    }
    assert!(evicted_at.is_some(), "an eviction happened past CAP");
    let home_entry = d.nav.entry(home).unwrap();
    assert!(home_entry.inst.is_none(), "Home's body was evicted");
    assert!(home_entry.evicted);
    assert_eq!(home_entry.ret.focus, Some(FocusKey { entry: home, elem: 5 }), "its return state stayed");
    let bodies = d.nav.bodies().count();
    assert!(bodies <= CAP, "bodies={bodies}");
    // pop all the way back down: Home remounts under the SAME id with its focus
    let mut ms = 1000;
    let mut guard = 0;
    while d.nav.tabs.stack.depth() > 1 {
        let r = d.frame(&mut rig, tick(ms), vec![key(Key::Back, tick(ms))], vec![], &mut NoTap);
        d.prune(&r.unmounted);
        ms += 16;
        guard += 1;
        assert!(guard < 64, "BACK stopped popping at depth {} (owner {:?}, top body {:?})", d.nav.tabs.stack.depth(), d.nav.input_owner(), d.nav.top_page().map(|e| (e.id, e.inst.is_some(), e.evicted)));
    }
    let e = d.nav.top_page().unwrap();
    assert_eq!(e.id, home, "the same EntryId");
    assert!(e.inst.is_some(), "remounted");
    assert!(!e.evicted);
    assert_eq!(e.ret.focus, Some(FocusKey { entry: home, elem: 5 }));
    assert_eq!(e.ret.remembered, remembered, "inactive groups survive body eviction");
    for cursor in remembered {
        assert!(d.return_state().remembered.contains(&cursor));
    }
    let ev = events_of(&d, 0);
    assert!(ev.contains("\"mount\", \"uncover\", \"restore_memory\", \"enter\""), "remount then restore: {ev}");
}

/// While a surface is up it owns input: a key goes to it and never to the page beneath.
#[test]
fn a_modal_scopes_focus_to_its_own_groups() {
    let (mut d, mut rig, home) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Compact, 16);
    d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![], &mut NoTap);
    let mut s = String::new();
    d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
    assert!(s.contains("keys=1"), "the surface got the key: {s}");
    let mut h = String::new();
    d.nav.instance_mut(home).unwrap().screen.state().probe(&mut h);
    assert!(h.contains("keys=0"), "the page did not: {h}");
    assert_eq!(d.nav.tabs.stack.depth(), 1, "and no page opened");
}

/// §8.3: the render set is checked over the WHOLE composition, with REAL numbers in all three
/// rules — a Cached host counts the one shared FrameCache and holds no render of its own; a
/// surface's own count is whatever that surface reports; the ceiling covers every byte term.
///
/// Rules (b) and (c) were both inert until phase 11: `draw_with` pushed a literal `1` per surface,
/// so "more than one render per surface" could not be spelled, and `Screen::render_bytes` had one
/// default `{ 0 }` and no override anywhere, so the byte sum was the FrameCache and nothing else.
#[test]
fn the_render_set_is_checked_over_the_whole_frame() {
    use crate::ui::frame::{RenderBreach, RenderReport, RenderSet, FRAME_CACHE_BYTES, RENDER_BYTES_MAX};
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    d.present.note(crate::ui::present::PresentEvent::Damage(crate::ui::present::Provenance::Input));
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert!(r.presented);
    assert_eq!(r.render_set.pages, 1);
    assert_eq!(
        r.render_set.surfaces,
        vec![(id, 0)],
        "the surface holds NO render of its own — it is a quad from the shared FrameCache"
    );
    assert_eq!(r.render_set.bytes, 0, "…and so owns no bytes either");
    assert_eq!(r.render_set.frame_cache_bytes, FRAME_CACHE_BYTES, "the Cached host is one FrameCache");
    assert!(r.render_set.check().is_ok());

    // (b) with a real number: one texture of the surface's OWN reaches the set as one, and its
    // bytes reach the sum. A literal cannot do this and a `{ 0 }` default cannot do the second.
    let own = RenderReport::one(400, 400);
    modal_mut(&mut d, id).render = own;
    d.present.note(crate::ui::present::PresentEvent::Damage(crate::ui::present::Provenance::Input));
    let r = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    assert!(r.presented);
    assert_eq!(r.render_set.surfaces, vec![(id, 1)], "one render of its own");
    assert_eq!(r.render_set.bytes, own.bytes, "400x400 RGBA8 = 640,000 bytes");
    assert!(r.render_set.check().is_ok(), "one render per surface is legal");

    // (b) breached, on numbers the set can now hold. (Driven through the dispatcher by
    // `a_surface_holding_two_renders_breaches_the_frames_render_set`, which is `should_panic`
    // because the debug policy for a breach is an assertion.)
    let two = RenderSet {
        pages: 1,
        surfaces: vec![(id, 2)],
        ..Default::default()
    };
    assert_eq!(two.check(), Err(RenderBreach::Surface(id, 2)));

    // (c) every byte term counts: the screens' own renders, the one FrameCache, and the shared
    // pools `extra_bytes` carries for the loop.
    let over = RenderSet {
        pages: 1,
        surfaces: vec![(id, 1)],
        bytes: RENDER_BYTES_MAX - FRAME_CACHE_BYTES,
        frame_cache_bytes: FRAME_CACHE_BYTES,
        extra_bytes: 0,
    };
    assert!(over.check().is_ok(), "exactly at the ceiling is not over it");
    let over = RenderSet { extra_bytes: 1, ..over };
    assert_eq!(
        over.check(),
        Err(RenderBreach::Bytes(RENDER_BYTES_MAX + 1)),
        "one byte of shared pool past the ceiling is a breach"
    );
    let three = RenderSet {
        pages: 3,
        ..Default::default()
    };
    assert_eq!(three.check(), Err(RenderBreach::Pages(3)));
}

/// Rule (b) end to end: a surface reporting two backing renders breaches the frame's set, and the
/// debug policy for a breach is an ASSERTION (§8.3) — so the frame that draws it dies here rather
/// than shipping a leak to a television. `on_breach`'s own tests grade the release half.
#[test]
#[should_panic(expected = "render set breach")]
fn a_surface_holding_two_renders_breaches_the_frames_render_set() {
    let (mut d, mut rig, _) = booted();
    let id = open_modal(&mut d, &mut rig, Style::Sheet, 16);
    modal_mut(&mut d, id).render = crate::ui::frame::RenderReport { textures: 2, bytes: 0 };
    d.present.note(crate::ui::present::PresentEvent::Damage(crate::ui::present::Provenance::Input));
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
}

/// §4.4 `MotionScope`: a surface's foreground spring reports `Motion` (so the frame presents)
/// without the PAGE reading as moving — the host snapshot is not re-taken for it.
#[test]
fn a_modal_foreground_spring_does_not_invalidate_the_host_snapshot() {
    let (mut d, mut rig, _) = booted();
    open_modal(&mut d, &mut rig, Style::Sheet, 16);
    let r = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert!(r.presented, "the appear spring and the surface's own pop report motion");
    assert!(!r.underlay_moving, "…none of it attributed to the page");
}

/// §4.4, the same claim in `ui::idle`'s vocabulary rather than the container gate's: the
/// dispatcher opens ONE motion scope per surface — around its `ModalStack::tick`, its body's step
/// and its draw — and NONE around the page's, so `idle::page_moving()` answers "did the HOST move"
/// on a frame that stepped both.
///
/// The other test above grades `FrameReport::underlay_moving`, which a screen reaches only by
/// calling `fx.note(Motion)`. This one grades the channel an owned screen actually animates
/// through — `gfx::spring` — which reaches `ui::idle` and nothing else, and which had no
/// per-surface attribution at all until phase 10: every spring a surface stepped read as the page
/// moving, and `app/bridge.rs` compensated by scoping the WHOLE dispatcher frame, which lost the
/// page's own motion in exchange (`a_host_page_spring_under_an_open_panel_is_host_motion`).
#[test]
fn a_surface_spring_and_a_page_spring_are_told_apart_by_the_idle_gate() {
    use crate::ui::fixture::ANIMATED_PAGE;
    let _serial = crate::testlock::serial();
    let (mut d, mut rig, _) = booted();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(ANIMATED_PAGE)));
    let mut ms = 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);

    // the page alone: its spring is the HOST's motion
    crate::ui::idle::frame_begin(1.0 / 60.0);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert!(crate::ui::idle::present_moving(), "the animated page steps a spring");
    assert!(crate::ui::idle::page_moving(), "…and it is the page's own");

    // a Compact surface leaves its host LIVE (`surface_policy`), so this frame steps BOTH bodies
    let _menu = open_modal(&mut d, &mut rig, Style::Compact, ms + 16);
    ms += 32;
    crate::ui::idle::frame_begin(1.0 / 60.0);
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert!(crate::ui::idle::present_moving(), "both bodies are still in flight");
    assert!(
        crate::ui::idle::page_moving(),
        "the page under a Compact panel is still the page: its spring is host motion"
    );

    // …and once the page has settled, the surface's own spring is NOT host motion. `open_modal`
    // mounted a fresh surface, so its pop window is still open here.
    for _ in 0..60 {
        crate::ui::idle::frame_begin(1.0 / 60.0);
        ms += 16;
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    }
    assert!(!crate::ui::idle::page_moving(), "everything settled");
    let surface = d.nav.modals.top().expect("the panel is up").entry.id;
    d.nav.modals.surface_mut(surface).unwrap().entry.inst.as_mut().unwrap()
        .screen.as_any_mut().and_then(|s| s.downcast_mut::<crate::ui::fixture::FixtureModal>())
        .expect("the fixture surface").pop = 0.0;
    crate::ui::idle::frame_begin(1.0 / 60.0);
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    assert!(crate::ui::idle::present_moving(), "the surface's own spring is in flight");
    assert!(
        !crate::ui::idle::page_moving(),
        "…and none of it is attributed to the page underneath"
    );
}

/// §5.4: a presenting frame's prepare pass leaves the logical-state hash where it was. The
/// dispatcher asserts it on every presenting frame of a debug build; this is the named test.
#[test]
fn prepare_does_not_change_the_logical_state_hash() {
    let (mut d, mut rig, _) = booted();
    // an event frame that presents: `r.state_hash` is taken after the drains and BEFORE prepare
    // and draw; the hash after the frame must be the same number
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert!(r.presented, "the key invalidated");
    let pre_prepare = r.state_hash.expect("an event frame hashes");
    assert_eq!(d.state_hash(), pre_prepare);
}

/// The dip lifted off `ui::nav`: over a `PageDip` the op applies at the FLOOR, several frames
/// after the press, while a cut applies at the same commit — and BACK inside the window
/// withdraws the transition with nothing mounted.
#[test]
fn a_page_dip_commits_at_its_floor_and_a_back_inside_the_window_withdraws_it() {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    // Root with nothing mounted yet: the dip runs out from alpha 1 and the floor mounts Home
    let mut mounted_at = None;
    for i in 0..20u32 {
        let r = d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
        if !r.mounted.is_empty() {
            mounted_at = Some(i);
            break;
        }
    }
    let at = mounted_at.expect("Home mounted at the floor");
    assert!(at >= 4, "70 ms out at 16 ms frames: the floor is ~5 frames in, not the press frame (got {at})");
    assert_eq!(d.top_screen().unwrap().name(), "home");
    // let the IN ramp settle (a request mid-ramp fades out from wherever the alpha is — a
    // retarget, whose floor comes sooner)
    for i in 0..20u32 {
        d.frame(&mut rig, tick(400 + i * 16), vec![], vec![], &mut NoTap);
    }
    assert!(!d.nav.tabs.stack.transition.in_flight());
    // a press opens a page: nothing mounts on the press frame
    let r = d.frame(&mut rig, tick(1000), vec![key(Key::Ok, tick(1000))], vec![], &mut NoTap);
    assert!(r.mounted.is_empty());
    assert!(d.nav.tabs.stack.is_pending());
    // a Cancel two frames in withdraws it: the pending transition reverses rather than committing
    let r = d.frame(&mut rig, tick(1016), vec![], vec![], &mut NoTap);
    assert!(r.mounted.is_empty(), "16 ms into a 70 ms fade: not the floor");
    d.request(MachineId::Nav, NavOp::Cancel);
    let r = d.frame(&mut rig, tick(1032), vec![], vec![], &mut NoTap);
    assert!(r.mounted.is_empty());
    assert!(!d.nav.tabs.stack.is_pending(), "withdrawn");
    for i in 0..30u32 {
        let r = d.frame(&mut rig, tick(1048 + i * 16), vec![], vec![], &mut NoTap);
        assert!(r.mounted.is_empty(), "a withdrawn transition never mounts");
    }
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.tabs.stack.page_alpha(), 1.0);
}

/// A pending destination is laid out through the text recorder while the old page is still the
/// visible top. The host has no GL context, so `text`'s test backend records residency at the same
/// cache boundary the device backend rasterises and uploads through.
#[test]
fn a_page_pushed_behind_a_dip_has_its_text_resident_before_it_is_seen() {
    let _g = crate::testlock::serial();
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    for i in 0..30u32 {
        d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
    }
    assert_eq!(d.top_screen().unwrap().name(), "home");

    crate::text::reset_prewarm_for_test();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(42)));
    d.frame(&mut rig, tick(600), vec![], vec![], &mut NoTap);
    d.frame(&mut rig, tick(616), vec![], vec![], &mut NoTap);

    assert_eq!(d.top_screen().unwrap().name(), "home", "the outgoing page remains visible");
    assert!(
        crate::text::prewarm_resident_for_test(b"pending page text", 24, 0),
        "the incoming page's text was warmed before the dip floor"
    );
}

/// **`has_pending_navigation` is a question about the PAGE stack**, and [`Navigation::moves_page`]
/// is the one classifier that answers it — the same one [`Navigation::request`] routes by, so the
/// guard and the commit cannot disagree about what a parked op is.
///
/// Its caller is `app::bridge::sync_page`, which mirrors the committed route onto the tree and
/// stands down for a frame whose page op the loop already parked. Reading a parked SURFACE op as
/// one is what stranded the player page under a route that had already left it: `exit_player`
/// parks a `Dismiss` for the panel that was up and flips the route in the same breath.
///
/// [`Navigation::moves_page`]: super::Navigation::moves_page
/// [`Navigation::request`]: super::Navigation::request
#[test]
fn only_a_parked_page_op_counts_as_a_pending_navigation() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    assert!(!d.has_pending_navigation(), "a settled tree has nothing parked");
    let surface = open_modal(&mut d, &mut rig, Style::Compact, 16);
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    assert!(!d.has_pending_navigation(), "presenting a surface does not move the page");
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    d.request(MachineId::Nav, NavOp::Dismiss(surface));
    assert!(!d.has_pending_navigation(), "…and neither does dismissing one");
    d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    let mut ms = 64;
    for op in [
        NavOp::Push(FixtureArg::Page(9)),
        NavOp::Pop,
        NavOp::Root(FixtureArg::Home),
        // a `Dismiss` naming a PAGE entry reaches `NavStack::apply`'s `PopTo` arm, so it is one
        NavOp::Dismiss(home),
        NavOp::PopTo(home),
    ] {
        d.request(MachineId::Nav, op);
        assert!(d.has_pending_navigation(), "a page op is a pending navigation");
        let report = d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
        ms += 16;
        assert!(!d.has_pending_navigation(), "…consumed at the commit");
    }
}

// =================================================================================================
// `NavOp::Root` / `NavOp::SelectTab` split (TV 2026-09-17: a per-frame `Root(Profiles)` follower
// remounted the picker and the first-run consent surface forever — see
// `app::session_picker_regression_tests::first_run_consent_over_the_picker_does_not_flip_mounts_every_frame`
// for the host-level repro). `Root` now truly replaces the stack, root included; `SelectTab` keeps
// the old shared arm's cover-and-mint pill semantics; and `NavStack::request` drops an
// exactly-redundant `Root`/`SelectTab`/`PopTo` before it ever touches `pending` or the transition.
// =================================================================================================

/// **`Root` retires EVERY entry, including the one under the caller's feet, and any surface
/// covering it is swept along with it** — never left covered and alive, which is what the shared
/// `Root`/`SelectTab` arm used to do and what let a never-retired root sit under every later mint.
#[test]
fn root_over_a_stacked_page_replaces_everything_and_sweeps_its_covered_surface() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let page = d.nav.top_page().unwrap().id;
    open_modal(&mut d, &mut rig, Style::Compact, 32);

    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(50)));
    let report = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    d.prune(&report.unmounted);

    assert_eq!(d.nav.tabs.stack.depth(), 1, "only the new root survives");
    assert!(d.nav.top_page().is_some_and(|e| e.arg == FixtureArg::Page(50)));
    assert!(
        !d.nav.tabs.stack.entries.iter().any(|e| e.id == home || e.id == page),
        "neither the old root nor the page above it survived the replace"
    );
    assert!(d.nav.modals.surfaces.is_empty(), "the live modal stack is fresh");
    assert!(
        d.nav.covered_modals.is_empty(),
        "the surface that covered the retired page was swept with it, not stranded"
    );
}

/// **A `Root`/`SelectTab`/`PopTo` the settled stack already satisfies is dropped before it
/// touches `pending` OR the transition.** Without that dedup, a per-frame re-request does not
/// quietly do nothing — it still COMMITS, in the sense that matters here: `request()` overwrites
/// `pending` and calls `transition.request()` again, and a `PageDip` answers that by restarting
/// its Out ramp from wherever it currently is. So re-asking it every frame (the Login/Profiles
/// follower's own shape) never lets the transition SETTLE — it is not stalled, it is continuously
/// RESTARTED — which is the mechanism behind the TV 2026-09-17 bug, independent of the
/// mount/unmount churn the `Root`/`SelectTab` split fixes on its own.
#[test]
fn redundant_root_select_tab_and_pop_to_requests_are_inert() {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    for i in 0..20u32 {
        d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
    }
    assert!(!d.nav.tabs.stack.transition.in_flight(), "the boot root settled");
    let home = d.nav.top_page().unwrap().id;

    // `Dispatcher::request` only enqueues (`has_pending_navigation` reads THAT raw queue); the
    // op does not reach `NavStack::request` — and so `is_inert` — until the next `frame()` drains
    // it. Checking the STACK's own `is_pending`/the transition therefore has to happen AFTER it.
    for (i, op) in [NavOp::Root(FixtureArg::Home), NavOp::SelectTab(FixtureArg::Home), NavOp::PopTo(home)]
        .into_iter()
        .enumerate()
    {
        d.request(MachineId::Nav, op);
        let report = d.frame(&mut rig, tick(1000 + i as u32 * 16), vec![], vec![], &mut NoTap);
        assert!(!d.nav.tabs.stack.is_pending(), "already satisfied: never parked on the stack");
        assert!(!d.nav.tabs.stack.transition.in_flight(), "…and never kicked the transition either");
        assert!(report.mounted.is_empty() && report.unmounted.is_empty(), "a true no-op mounts nothing");
    }
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.tabs.stack.page_alpha(), 1.0, "the page never dipped");
    let after = events_of(&d, 0);
    assert!(!after.contains("\"uncover\""), "not even an Uncover/Restored pair was delivered: {after}");
    assert_eq!(after.matches("\"enter\"").count(), 1, "no additional Enter either: {after}");
}

/// **An EVICTED entry is never "already satisfied", whichever op names it.** `Root` asks
/// [`NavStack::root_settled`], which requires a body; `SelectTab` and `PopTo` used to compare only
/// ids and `same_instance`, so a tab press or a `PopTo` returning to a root whose body `CAP`
/// eviction had dropped was refused as redundant — and the apply arm's own `Mount` for a bodyless
/// target (`stack.rs`'s `bodyless` branch) never ran, leaving the page permanently unmounted.
/// (Reported by review on PR #113; the sibling property is
/// `an_evicted_entry_keeps_its_focus_identity_on_remount`, which reaches the same remount by BACK.)
#[test]
fn a_select_tab_or_pop_to_naming_an_evicted_entry_remounts_it_rather_than_being_inert() {
    for op_is_select_tab in [true, false] {
        let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
        let mut rig = FixtureRig::new();
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
        for i in 0..20u32 {
            d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
        }
        let home = d.nav.top_page().unwrap().id;
        // What `CAP` eviction leaves behind: the entry, its `ReturnState` and its id, with the
        // body dropped and `evicted` set. Done by hand because reaching the cap here would put
        // other pages ON TOP of Home, and the case under test is the op that names the entry
        // that is ALREADY the top — the only shape `is_inert` can mistake for settled.
        {
            let e = d.nav.tabs.stack.entries.iter_mut().find(|e| e.id == home).unwrap();
            e.inst = None;
            e.evicted = true;
        }
        let op = if op_is_select_tab { NavOp::SelectTab(FixtureArg::Home) } else { NavOp::PopTo(home) };
        d.request(MachineId::Nav, op);
        let report = d.frame(&mut rig, tick(2000), vec![], vec![], &mut NoTap);
        let mounted_or_pending = !report.mounted.is_empty() || d.nav.tabs.stack.is_pending();
        assert!(
            mounted_or_pending,
            "select_tab={op_is_select_tab}: the request must reach the stack, not be dropped as satisfied",
        );
        // …and it really does come back, with the same identity.
        for i in 0..20u32 {
            let r = d.frame(&mut rig, tick(2016 + i * 16), vec![], vec![], &mut NoTap);
            d.prune(&r.unmounted);
        }
        let e = d.nav.top_page().unwrap();
        assert_eq!(e.id, home, "select_tab={op_is_select_tab}: the same EntryId");
        assert!(e.inst.is_some(), "select_tab={op_is_select_tab}: remounted");
    }
}

/// **A `Root` request while a DIFFERENT op is pending still replaces it — newest wins**, which
/// sign-out and a profile switch both depend on (they `Root` right after asking for something
/// else in the same breath). This is a property of `NavStack::is_inert` only refusing an
/// exactly-redundant repeat of the SAME kind, never a differently-shaped one.
#[test]
fn a_root_request_while_a_different_op_is_pending_still_wins() {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    for i in 0..20u32 {
        d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
    }
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.frame(&mut rig, tick(1000), vec![], vec![], &mut NoTap); // drains into the stack's own pending
    assert!(d.nav.tabs.stack.is_pending(), "the push is parked, mid fade-out");
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(50)));
    for i in 1..20u32 {
        let report = d.frame(&mut rig, tick(1000 + i * 16), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
    }
    assert_eq!(d.nav.tabs.stack.depth(), 1, "the Root committed; the superseded Push never did");
    assert!(d.nav.top_page().is_some_and(|e| e.arg == FixtureArg::Page(50)));
}

/// **`SelectTab` covers-and-mints over the existing root rather than replacing it, and BACK off
/// the pressed pill returns to the SAME root entry** — the pill semantics the old shared
/// `Root`/`SelectTab` arm had, kept exactly by the split's other half.
#[test]
fn select_tab_covers_the_root_rather_than_replacing_it_and_back_returns_to_it() {
    let (mut d, mut rig, _) = booted();
    let home = d.nav.top_page().unwrap().id;

    d.request(MachineId::Nav, NavOp::SelectTab(FixtureArg::Page(9)));
    let report = d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    d.prune(&report.unmounted);
    assert_eq!(d.nav.tabs.stack.depth(), 2, "the root is covered, not replaced");
    assert!(d.nav.tabs.stack.entries.iter().any(|e| e.id == home), "home survives, covered");
    assert!(d.nav.top_page().is_some_and(|e| e.arg == FixtureArg::Page(9)));

    let r = d.frame(&mut rig, tick(32), vec![key(Key::Back, tick(32))], vec![], &mut NoTap);
    d.prune(&r.unmounted);
    assert_eq!(d.nav.tabs.stack.depth(), 1);
    assert_eq!(d.nav.top_page().unwrap().id, home, "BACK off the pill lands on the SAME Home entry");

    // Re-cover, then `SelectTab(Home)` is a `PopTo(root)` — not a fresh mint of Home.
    d.request(MachineId::Nav, NavOp::SelectTab(FixtureArg::Page(9)));
    let r = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    d.prune(&r.unmounted);
    let covering = d.nav.top_page().unwrap().id;
    d.request(MachineId::Nav, NavOp::SelectTab(FixtureArg::Home));
    let r = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
    d.prune(&r.unmounted);
    assert_eq!(d.nav.tabs.stack.depth(), 1, "SelectTab(root) unwinds back to it");
    assert_eq!(d.nav.top_page().unwrap().id, home, "the SAME root entry survives — no remint");
    assert!(!d.nav.tabs.stack.entries.iter().any(|e| e.id == covering), "the covering page is gone");
}

/// **`reset_for_profile` also clears the stack's own pending op and due flag.** Without this, a
/// `Push` parked before the reset (mid fade-out, on the STACK rather than merely the dispatcher's
/// own queue) would still apply at its own floor — some frames later — over the tree the reset
/// just emptied, minting an entry nobody asked for post-reset.
#[test]
fn reset_for_profile_clears_a_pending_op_so_it_cannot_apply_over_the_emptied_tree() {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::with_transition(Box::new(PageDip::new()));
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    for i in 0..20u32 {
        d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap);
    }
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(9)));
    d.frame(&mut rig, tick(1000), vec![], vec![], &mut NoTap);
    assert!(d.nav.tabs.stack.is_pending(), "the push reached the STACK's own pending, mid fade-out");

    d.reset_for_profile();
    assert!(!d.nav.tabs.stack.is_pending(), "the reset drops it rather than letting it apply later");
    assert!(d.nav.tabs.stack.entries.is_empty(), "the reset itself emptied the tree");

    for i in 0..20u32 {
        let report = d.frame(&mut rig, tick(2000 + i * 16), vec![], vec![], &mut NoTap);
        d.prune(&report.unmounted);
    }
    assert!(d.nav.tabs.stack.entries.is_empty(), "nothing minted itself back in behind the reset");
}

#[derive(Default)]
struct CountingSnapshot(bool);
impl super::transition::PageSnapshot for CountingSnapshot {
    fn available(&self) -> bool { true }
    fn valid(&self) -> bool { self.0 }
    fn begin(&mut self) -> bool { self.0 = false; true }
    fn finish(&mut self) { self.0 = true; }
    fn release(&mut self) { self.0 = false; }
}

fn frozen_fixture() -> (Dispatcher<FixtureHost>, FixtureRig) {
    let (mut d, rig, _) = booted();
    d.page_snapshot = Box::<CountingSnapshot>::default();
    d.nav.tabs.stack.transition = Box::new(PageDip::new());
    (d, rig)
}
fn page_draw_order(d: &Dispatcher<FixtureHost>) -> usize {
    d.top_screen().unwrap().as_any().unwrap()
        .downcast_ref::<crate::ui::fixture::FixtureScreen>().unwrap().draw_at
}

#[test]
fn frozen_dispatch_skips_the_page_but_keeps_chrome_live() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = frozen_fixture();
    d.nav.tabs.stack.transition.request(true);
    d.draw(&mut rig, true); // capture
    let before = page_draw_order(&d);
    let chrome = rig.chrome_draws;
    d.draw(&mut rig, true); // held
    assert_eq!(page_draw_order(&d), before, "OUT must not invoke the live page");
    assert_eq!(rig.chrome_draws, chrome + 1, "chrome is outside the captured page");
}

#[test]
fn frozen_dispatch_in_reuses_the_floor_capture() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = frozen_fixture();
    d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(7)));
    for i in 1..=7 { d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap); }
    assert_eq!(d.top_arg(), Some(&FixtureArg::Page(7)));
    assert!(d.nav.tabs.stack.transition.in_flight());
    d.draw(&mut rig, true);
    let before = page_draw_order(&d);
    d.draw(&mut rig, true);
    assert_eq!(page_draw_order(&d), before, "IN must not invoke the live page");
}

#[test]
fn frozen_dispatch_resumes_live_after_settle() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = frozen_fixture();
    d.nav.tabs.stack.transition.request(true);
    d.draw(&mut rig, true);
    d.draw(&mut rig, true);
    let held = page_draw_order(&d);
    d.draw(&mut rig, true);
    assert_eq!(page_draw_order(&d), held, "premise: page is held before settle");
    d.nav.tabs.stack.transition.cancel();
    for i in 1..=20 { d.frame(&mut rig, tick(i * 16), vec![], vec![], &mut NoTap); }
    assert!(!d.nav.tabs.stack.transition.in_flight());
    d.draw(&mut rig, true);
    assert!(page_draw_order(&d) > held, "settled page is live again");
}

#[test]
fn frozen_dispatch_source_and_surfaces_passes_preserve_the_image() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = frozen_fixture();
    d.nav.tabs.stack.transition.request(true);
    d.draw(&mut rig, true);
    let before = page_draw_order(&d);
    {
        use crate::ui::frame::backdrop::{self, Sources};
        let _source = backdrop::discover(std::rc::Rc::new(std::cell::RefCell::new(Sources::default())));
        d.draw(&mut rig, true);
    }
    assert_eq!(page_draw_order(&d), before, "glass discovery must not walk a held page");
    d.draw(&mut rig, false);
    assert!(d.page_snapshot.valid(), "the surfaces pass cannot release the page image");
    d.draw(&mut rig, true);
    assert_eq!(page_draw_order(&d), before);
    let layers = d.backdrop_layers(0.5);
    assert!(layers.iter().any(|layer| layer.blocks && layer.z < crate::ui::frame::backdrop::Z::CHROME));
}

#[test]
fn frozen_dispatch_capture_refusal_keeps_the_live_fallback() {
    let _guard = crate::testlock::serial();
    struct Refused;
    impl super::transition::PageSnapshot for Refused {
        fn available(&self) -> bool { true }
    }
    let (mut d, mut rig) = frozen_fixture();
    d.page_snapshot = Box::new(Refused);
    d.nav.tabs.stack.transition.request(true);
    d.draw(&mut rig, true);
    let before = page_draw_order(&d);
    d.draw(&mut rig, true);
    assert!(page_draw_order(&d) > before);
    assert!(!d.page_snapshot.valid());
}

#[test]
fn frozen_dispatch_modal_takes_the_single_snapshot() {
    let _guard = crate::testlock::serial();
    let (mut d, mut rig) = frozen_fixture();
    d.nav.tabs.stack.transition.request(true);
    d.draw(&mut rig, true);
    assert!(d.page_snapshot.valid());
    open_modal(&mut d, &mut rig, Style::Sheet, 16);
    d.draw(&mut rig, true);
    assert!(!d.page_snapshot.valid(), "page-only pixels cannot serve a modal's chrome prefix");
}
