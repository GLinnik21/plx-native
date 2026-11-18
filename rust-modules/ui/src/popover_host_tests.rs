// Exercise the production host protocol with a CPU framebuffer standing in for GL copies.
// No host policy is mocked: begin_frame/page_pass/live/ground_drawn and gfx::culled are real.
// A copy replaces the framebuffer; a primitive appends ink unless the real freeze gate refuses it.
use super::mock::{FrameCache, PIXELS};
use super::*;

// Legacy host-only cases have no typed runtime damage; production passes it explicitly.
fn begin_frame(modal: &crate::containers::modal::ModalPresentation, page_moving: bool) {
    super::begin_frame(modal, 0.0, page_moving, false);
}

#[test]
fn typed_page_damage_refreshes_frozen_host_but_surface_damage_does_not() {
    use plx_machine::present::{Present, PresentEvent, Provenance, ResourceKind, Scope};
    use plx_machine::motion::report;
    let _serial = plx_base::testlock::serial();
    report::reset_for_test();
    let _reset = Reset::new(false);
    let mut present = Present::new();
    let draw = |present: &mut Present, ms, during_draw| {
        report::collect_into(present, ms);
        present.take(ms);
        super::begin_frame(&_reset.modal, 0.0, false, present.page_damaged());
        {
            let _page = page_pass(&_reset.modal);
            ink("page");
        }
        if during_draw {
            present.note(PresentEvent::Damage(Provenance::Resource(ResourceKind::Texture)));
        }
        present.acknowledge(ms);
    };
    draw(&mut present, 0, false);
    let captured = page_epoch();
    present.set_scope(Scope::Surface);
    present.note(PresentEvent::Damage(Provenance::Input));
    draw(&mut present, 16, false);
    assert_eq!(page_epoch(), captured, "typed surface damage keeps the frozen page");
    {
        plx_machine::machine::PresentHandle::of(&mut present)
            .note(PresentEvent::Damage(Provenance::Input));
    }
    draw(&mut present, 32, false);
    assert_eq!(page_epoch(), captured, "owned surface damage is not reclassified as page damage");
    present.set_scope(Scope::Page);
    present.note(PresentEvent::Damage(Provenance::Resource(ResourceKind::Texture)));
    draw(&mut present, 48, false);
    assert_eq!(page_epoch(), captured.wrapping_add(1), "typed page landing refreshes the frozen host");
    draw(&mut present, 64, true);
    assert!(present.page_damaged(), "swap cannot erase typed page damage raised during draw");
    draw(&mut present, 80, false);
    assert_eq!(page_epoch(), captured.wrapping_add(2));
}

#[test]
fn nested_decision_alert_discards_cached_pixels_without_publishing_modal_policy() {
    use crate::decision_alert::{Answers, DecisionAlert};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    // A Settings/consent-style parent already owns its modal presentation. The embedded
    // DecisionAlert has local choreography, but cannot change that parent's host/input policy.
    let (d, _, _) = crate::fixture::booted();
    let other_before = d.modal_presentation();
    embedded_alert_frame(&_reset.modal, true, true);
    assert!(matches!(held(), Held::Ground(_)));
    let epoch = page_epoch();
    let parent_before = _reset.modal.read;
    let mut alert = DecisionAlert::new();
    alert.open_card(c"Confirm", vec!["Synthetic disclosure".into()], Answers::Two);
    assert!(held() == Held::Nothing, "opening a nested outline discards the old pixels");
    embedded_alert_frame(&_reset.modal, true, true);
    assert_eq!(page_epoch(), epoch.wrapping_add(1));
    assert!(matches!(held(), Held::Ground(_)));
    alert.close();
    assert!(held() == Held::Nothing, "closing cannot keep the alert baked into a composite");
    embedded_alert_frame(&_reset.modal, true, true);
    assert_eq!(page_epoch(), epoch.wrapping_add(2));
    assert_eq!(_reset.modal.read, parent_before);
    assert_eq!(d.modal_presentation().read, other_before.read,
        "discarding context-local pixels never publishes another runtime's modal policy");
}

#[test]
fn same_thread_owner_switch_never_reuses_another_hosts_frozen_pixels() {
    use crate::dispatch::NoTap;
    use crate::fixture::{booted, tick, FixtureArg};
    use crate::containers::modal::Style;
    use plx_machine::machine::{MachineId, NavOp};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let (mut a, mut ar, _) = booted();
    let (mut b, mut br, _) = booted();
    a.nav.next_style = Style::Compact;
    a.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    a.frame(&mut ar, tick(16), vec![], vec![], &mut NoTap);
    let a_open = a.modal_presentation();
    let draw = |modal: &crate::containers::modal::ModalPresentation, page| {
        super::begin_frame(modal, 0.0, false, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        { let _page = page_pass(modal); ink(page); }
        PIXELS.with(|p| p.borrow().clone())
    };
    assert_eq!(draw(&a_open, "A"), ["A"]);
    let epoch = page_epoch();
    assert_eq!(draw(&a_open, "wrong"), ["A"], "same owner reuses its frozen host");
    assert_eq!(page_epoch(), epoch);
    b.frame(&mut br, tick(32), vec![], vec![], &mut NoTap);
    let b_rest = b.modal_presentation();
    assert_eq!(draw(&b_rest, "B"), ["B"], "a resting owner never inherits A's freeze");
    assert_eq!(draw(&a_open, "A-new"), ["A-new"], "returning A recaptures rather than serving B's pixels");
    b.nav.next_style = Style::Compact;
    b.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    b.frame(&mut br, tick(48), vec![], vec![], &mut NoTap);
    let b_open = b.modal_presentation();
    assert_eq!(a.nav.modals.top().unwrap().entry.id, b.nav.modals.top().unwrap().entry.id,
        "equal per-dispatcher entry ids are the aliasing case");
    assert_eq!(draw(&b_open, "B-open"), ["B-open"]);
    assert_eq!(draw(&a_open, "A-back"), ["A-back"]);
    assert_eq!(a.modal_presentation().read, a_open.read, "render cache switches cannot mutate modal authority");
}

/// **The container's own dismissal keeps the page's snapshot.** A caching surface (here a
/// `Style::Compact`; a Sheet below) starting to close changes the owner's cache key (its layer is `Closing` now), and `bind_owner`
/// answered with a full `invalidate()`: the close's first frame then redrew the page into the
/// snapshot, re-sourced the tab bar's blur and re-latched the dim field, the 131% of a 60 Hz
/// frame's GPU budget that `Popover::release` no longer costs a glass menu. A closing flip drops
/// what a close must not show (a GROUND-stage composite, the panel at full strength) and keeps the
/// page's: the page under a fading menu has not changed, and real page damage and motion still
/// re-take through `begin_frame`.
#[test]
fn a_surface_that_starts_closing_keeps_the_page_snapshot_and_drops_only_a_ground_stage() {
    use crate::dispatch::NoTap;
    use crate::fixture::{booted, tick, FixtureArg};
    use crate::containers::modal::Style;
    use plx_machine::machine::{MachineId, NavOp};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let (mut d, mut rig, _) = booted();
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
    d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
    let id = d.nav.modals.top().expect("presented").entry.id;
    let open = d.modal_presentation();
    assert!(open.read.host_cached && !open.read.closing_only, "premise: an open surface that caches its host");
    let draw = |modal: &crate::containers::modal::ModalPresentation, page| {
        super::begin_frame(modal, 0.0, false, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        { let _page = page_pass(modal); ink(page); }
        PIXELS.with(|p| p.borrow().clone())
    };
    assert_eq!(draw(&open, "page"), ["page"]);
    assert!(held() == Held::Page, "premise: the open surface froze the page");
    let epoch = page_epoch();

    d.request(MachineId::Nav, NavOp::Dismiss(id));
    d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    let closing = d.modal_presentation();
    assert!(closing.read.closing_only, "premise: the surface is closing");
    assert!(!open.cache_key().same_snapshot(&closing.cache_key()), "premise: the close changed the cache key");
    let before = invalidations();
    assert_eq!(draw(&closing, "retaken"), ["page"], "the close serves the page's snapshot, it does not redraw the page");
    assert_eq!(page_epoch(), epoch, "the close's first frame re-took nothing");
    assert_eq!(invalidations(), before, "…and threw nothing away");
    assert!(held() == Held::Page);

    // the guard: a GROUND-stage snapshot is the panel at full strength, which a fade must not be
    // drawn over, so the same flip still drops it
    let _ = embedded_alert_frame(&open, true, true);
    let _ = embedded_alert_frame(&open, true, true);
    assert!(matches!(held(), Held::Ground(_)), "premise: a caching panel's ground is held");
    super::begin_frame(&closing, 0.0, false, false);
    assert!(held() == Held::Nothing, "a closing surface keeps no ground snapshot");
}

/// **A closing surface keeps the page's snapshot whatever froze the host under it; the page's own
/// damage is what retakes it.** `surface_policy` freezes the host under a Sheet, an Alert and an
/// `Opaque { snapshot: true }` surface, and a frozen page gets no Tick, so its snapshot is as old
/// as the surface. What a page can have changed meanwhile is the page's to report: typed page damage
/// (`begin_frame`'s `typed_page_damage`, from the owning `Present`) retakes the snapshot, on the
/// close's first frame as on any other, and a page that reports none is the page it was (Settings
/// changing Under posters under a covered Library is the Library's damage, `step`'s head). The
/// close's cache-key flip itself is no retake: it cost a Sheet's close 14.9M GPU cycles against an
/// 11.2M budget on the TV, a dropped frame on every account menu BACK.
/// Presents `styles` bottom to top (a frame each, the page drawn and held in between), dismisses the
/// TOP one and answers whether the close's first frame, handed `page_damaged` as the page's typed
/// damage, redrew the page (the snapshot was thrown away) rather than served it.
fn the_close_retakes_the_page(styles: &[crate::containers::modal::Style], page_damaged: bool) -> bool {
    use crate::dispatch::NoTap;
    use crate::fixture::{booted, tick, FixtureArg};
    use plx_machine::machine::{MachineId, NavOp};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let (mut d, mut rig, _) = booted();
    let draw = |modal: &crate::containers::modal::ModalPresentation, page, damaged| {
        super::begin_frame(modal, 0.0, false, damaged);
        PIXELS.with(|p| p.borrow_mut().clear());
        { let _page = page_pass(modal); ink(page); }
        PIXELS.with(|p| p.borrow().clone())
    };
    let mut ms = 0;
    for style in styles {
        d.nav.next_style = *style;
        d.request(MachineId::Nav, NavOp::Present(FixtureArg::Modal));
        ms += 16;
        d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
        assert_eq!(draw(&d.modal_presentation(), "page", false).len(), 1, "premise: the page is drawn under {style:?}");
    }
    assert!(held() == Held::Page, "premise: the stack froze the page");
    let top = d.nav.modals.top().expect("presented").entry.id;
    let open = d.modal_presentation();
    d.request(MachineId::Nav, NavOp::Dismiss(top));
    ms += 16;
    d.frame(&mut rig, tick(ms), vec![], vec![], &mut NoTap);
    let closing = d.modal_presentation();
    assert!(!open.cache_key().same_snapshot(&closing.cache_key()), "premise: the close changed the cache key");
    draw(&closing, "retaken", page_damaged) == ["retaken"]
}

#[test]
fn a_sheet_that_closes_with_no_page_damage_keeps_the_page_snapshot() {
    use crate::containers::modal::Style;
    assert!(!the_close_retakes_the_page(&[Style::Sheet], false),
        "a Sheet froze the host, and the page reported no change: its close serves the snapshot it has");
}

#[test]
fn a_frozen_host_surface_that_starts_closing_retakes_the_page_after_the_page_changed_while_frozen() {
    use crate::containers::modal::Style;
    let kept: Vec<Style> = [Style::Sheet, Style::Alert, Style::Opaque { snapshot: true }]
        .into_iter().filter(|style| !the_close_retakes_the_page(&[*style], true)).collect();
    assert!(kept.is_empty(),
        "{kept:?} froze the host while open and the page reported damage: the close's first frame must retake it");
}

#[test]
fn a_live_host_surface_that_starts_closing_still_keeps_the_page() {
    use crate::containers::modal::Style;
    assert!(!the_close_retakes_the_page(&[Style::Compact], false),
        "a Compact surface kept the host live while open: the page's snapshot is current");
}

#[test]
fn a_mixed_stack_whose_frozen_host_layer_starts_closing_retakes_the_page_after_the_page_changed_while_frozen() {
    use crate::containers::modal::Style;
    assert!(the_close_retakes_the_page(&[Style::Compact, Style::Sheet], true),
        "the Sheet above a Compact menu froze the host and the page reported damage: its close retakes the page");
}

struct Reset {
    modal: crate::containers::modal::ModalPresentation,
    frozen: bool,
}
impl Reset {
    fn new(cache_off: bool) -> Self {
        let mut stack = crate::containers::modal::ModalStack::<crate::fixture::FixtureHost>::new();
        stack.present(&mut crate::containers::Minter::default(), crate::fixture::FixtureArg::Modal,
            crate::containers::modal::Style::Sheet);
        let modal = stack.presentation();
        CACHE_OWNER.set(None);
        let frozen = plx_gfx::gfx::set_page_frozen(false);
        cache(|c| *c = FrameCache { snapshot: None, off: cache_off, prev: None });
        HELD.set(Held::Nothing);
        CAPTURE_OWED.store(false, Relaxed);
        CAPTURE_POINTLESS.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
        Self { modal, frozen }
    }
}
impl Drop for Reset {
    fn drop(&mut self) {
        invalidate();
        CACHE_OWNER.set(None);
        plx_gfx::gfx::set_page_frozen(self.frozen);
        CAPTURE_OWED.store(false, Relaxed);
        CAPTURE_POINTLESS.store(false, Relaxed);
        GROUND_DRAWN.store(false, Relaxed);
    }
}

fn ink(label: &'static str) {
    if !plx_gfx::gfx::culled(0.0, 0.0, 1920.0, 1080.0) {
        PIXELS.with(|p| p.borrow_mut().push(label));
    }
}

fn embedded_alert_frame(modal: &crate::containers::modal::ModalPresentation, settled: bool, later_scope: bool) -> Vec<&'static str> {
    begin_frame(modal, false);
    // BUFFER_DESTROYED: every present starts with no usable prior framebuffer contents.
    PIXELS.with(|p| p.borrow_mut().clear());
    {
        let _page = page_pass(modal);
        ink("page");
        {
            let _scrim = live();
            ink("scrim");
        }
        {
            let _alert = live();
            ink("glass");
            ground_drawn(settled);
            ink("title/body/buttons");
        }
        // Dispatcher::draw_with enters its modal-scrim scope AFTER the page has drawn the
        // embedded DecisionAlert, even when the modal stack has no surfaces.
        if later_scope {
            let _container_scrims = live();
        }
    }
    assert!(!plx_gfx::gfx::page_frozen(), "the page scope must restore its caller");
    PIXELS.with(|p| p.borrow().clone())
}

#[test]
fn captured_ground_cannot_overwrite_an_embedded_alerts_foreground() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let expected = vec!["page", "scrim", "glass", "title/body/buttons"];
    assert_eq!(embedded_alert_frame(&_reset.modal, false, true), expected, "opening frame");
    assert_eq!(embedded_alert_frame(&_reset.modal, true, true), expected, "first settled frame");
    assert_eq!(embedded_alert_frame(&_reset.modal, true, true), expected, "cached input frame");
    // Capture attribution is now supplied by the owning Present.
    super::begin_frame(&_reset.modal, 0.0, false, true);
    assert_eq!(embedded_alert_frame(&_reset.modal, true, true), expected, "host damage recapture");
}

#[test]
fn cache_off_and_no_later_scope_explain_the_old_green_paths() {
    let _serial = plx_base::testlock::serial();
    let expected = vec!["page", "scrim", "glass", "title/body/buttons"];
    {
        let _reset = Reset::new(true);
        assert_eq!(embedded_alert_frame(&_reset.modal, true, true), expected, "cache-off simulator");
    }
    {
        let _reset = Reset::new(false);
        assert_eq!(embedded_alert_frame(&_reset.modal, true, false), expected, "alert drawn last");
    }
}

#[test]
fn reconciling_card_content_invalidates_its_cached_ground_only_when_changed() {
    use crate::decision_alert::{Answers, DecisionAlert};
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let mut alert = DecisionAlert::new();
    alert.open_card(c"Details", vec!["support".into()], Answers::One);
    embedded_alert_frame(&_reset.modal, true, true);
    assert!(matches!(held(), Held::Ground(_)));
    assert!(!alert.reconcile_card(c"Details", vec!["support".into()], Answers::One));
    assert!(matches!(held(), Held::Ground(_)), "unchanged content keeps its snapshot");
    assert!(alert.reconcile_card(c"Details", vec!["receipt".into(), "support".into()], Answers::Two));
    assert!(held() == Held::Nothing, "the taller card cannot reuse the old panel outline");
    assert_eq!(embedded_alert_frame(&_reset.modal, true, true), ["page", "scrim", "glass", "title/body/buttons"]);
}

// Moved here from `gfx.rs`'s tests (module-layers step L5): it drives the popover host's
// `begin_frame`, which the `gfx` layer may not name, to prove `gfx::dither_for_field` has no motion
// term to be reached through.
#[test]
fn a_field_keeps_its_dither_through_every_motion() {
    use plx_machine::motion::report::{frame_begin, note_spring, page_moving, present_moving, MotionScope};

    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    frame_begin(1.0 / 60.0);
    begin_frame(&_reset.modal, false);
    assert_eq!(plx_gfx::gfx::dither_for_field(700.0, 700.0), plx_gfx::gfx::DITHER_LSB, "at rest, the field pays");

    // A POPOVER's spring: 100 units from its target, stepped inside its own scope, the way
    // `Popover::update` steps every appear spring. The frame is in motion — and the page is not.
    frame_begin(1.0 / 60.0);
    let scope = MotionScope::open();
    note_spring(0.0, 100.0, 0.0);
    assert!(scope.close(), "the scope saw the spring");
    assert!(present_moving() && !page_moving());
    begin_frame(&_reset.modal, false);
    assert_eq!(
        plx_gfx::gfx::dither_for_field(700.0, 700.0),
        plx_gfx::gfx::DITHER_LSB,
        "a field still pays in motion — a focus spring on Settings must not strip its ground"
    );

    // The page's UNSCOPED springs (Detail updates outside `scoped_motion`) and the SCOPED
    // verdict app.rs threads in (Home, the Library, Search, the press dip). Both are real page
    // motion, and neither may reach this decision.
    frame_begin(1.0 / 60.0);
    note_spring(0.0, 100.0, 0.0);
    assert!(page_moving());
    begin_frame(&_reset.modal, true);
    assert_eq!(plx_gfx::gfx::dither_for_field(700.0, 700.0), plx_gfx::gfx::DITHER_LSB, "page motion is not a field's business");
}

#[test]
fn decorative_page_motion_does_not_count_as_layout_motion() {
    use plx_machine::motion::report::{decorative, frame_begin, note_motion, page_layout_moving, page_moving, present_moving};
    let _serial = plx_base::testlock::serial();
    frame_begin(1.0 / 60.0);
    decorative(note_motion);
    assert!(present_moving(), "decorative pixels still need a present");
    assert!(page_moving(), "decorative pixels belong to the page");
    assert!(!page_layout_moving(), "decoration must not hold page quiescence");
    note_motion();
    assert!(page_layout_moving(), "ordinary page motion must still hold quiescence");
}

/// A caller's own panel radius, deliberately not the board's 22: the morph must END on it.
const RAD: f32 = 26.0;

/// DS7: the morph open style on a real `Popover`, beside the unchanged fade+slide. The pill and
/// the panel are one surface: a row is only drawn once the fill is all glass, and a close lands
/// back on the pill.
#[test]
fn a_morphing_popover_is_one_surface_and_settles_both_ways() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Morph);
    pop.open_from(Some(pill));
    let one_surface = |f: crate::popover_morph::Frame| {
        if f.content_alpha > 0.0 {
            assert_eq!(f.ground_alpha, 1.0, "a row only on a surface that is all glass: {f:?}");
        }
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        one_surface(pop.morph_frame(Some(pill), panel, RAD).unwrap());
    }
    assert!(pop.appear_settled(), "the open settles, morph included");
    let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
    assert!((f.radius - RAD).abs() < 0.05 && (f.rect.h - panel.h).abs() < 0.5, "the morph ends on the caller's radius");
    pop.dismiss();
    // a close BULGES (iOS: ~7% of the travel at ~33 ms, `popover_morph::BULGE_KICK_H`) and only shrinks
    // from ~50 ms on
    let mut last = f.rect.h * 1.08;
    for n in 0..240 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        one_surface(f);
        // the slab shrinks into the opener until it is gone; after that the frame is the closed one
        // (the opener at rest, no glass), which stays until the appear fade lands
        if f.ground_alpha > 0.0 && n >= 3 {
            assert!(f.rect.h <= last + 1e-3, "the close only shrinks: {f:?} after {last}");
            last = f.rect.h;
        }
    }
    assert!(!pop.visible(), "the close lands and the panel is gone");
    let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
    assert_eq!(f.rect, pill, "and what is left is the pill");
    pop.close();
}

/// **N-b: the glass leaves its opener as a ramp, never as an opaque slab; and it comes back as a
/// drop that drains.** The first glass frame of an open used to be laid at FULL strength while the
/// springs had already pushed its rect ~25 px past the lit opener, so a ~165x62 dark slab hung under
/// the pill for one frame (5-menus/libsf-zoom n=155 / 247). The rule, over every header opener:
/// whenever an OPENING glass stands outside the opener's rect, its alpha AND its shadow are the
/// morph's one fill ramp, so it is never opaque on the frame it first appears. A CLOSE is the
/// droplet's (owner, 2026-11-17, measured from iOS 26): the frosted slab is whole glass all the way
/// into the opener, so the ramp does not apply; what is held instead is that once it has arrived
/// it hangs only in the drain band under the opener (no wider than the drain, no lower than it) and
/// that the last frame of glass is a bead a fraction of the drain's size, never a slab.
#[test]
fn the_glass_leaves_every_header_opener_as_a_ramp_and_drains_back_on_close() {
    let _g = plx_base::testlock::serial();
    let openers = [
        ("Library Sort & Filter", crate::Rect::new(1589.0, 146.0, 204.0, 60.0)),
        ("Collections +N", crate::Rect::new(1180.0, 146.0, 118.0, 60.0)),
        ("the libraries menu", crate::Rect::new(94.0, 146.0, 230.0, 60.0)),
        ("the account chip", crate::widgets::chip_cap(1.0, 60.0)),
        ("the account chip at rest, a circle", crate::widgets::chip_cap(0.0, 0.0)),
    ];
    for (name, pill) in openers {
        let panel = crate::popover::cover_opener(pill, (316.0, 400.0), crate::consts::SAFE);
        let drain = crate::popover_morph::drain_rect(pill);
        let outside = |r: crate::Rect| r.x < pill.x - 1.0 || r.y < pill.y - 1.0 || r.x + r.w > pill.x + pill.w + 1.0 || r.y + r.h > pill.y + pill.h + 1.0;
        let mut pop = crate::popover::Popover::new().morphing();
        pop.open_from(Some(pill));
        // the morph begins ON the opener, in its own corner: the lit opener's rect and its round
        // surround (half its height), so the glass first stands exactly under the control
        let begin = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert_eq!((begin.rect, begin.radius), (pill, pill.h * 0.5), "{name}: the first frame is the opener's rect and corner");
        let mut opening = Vec::new();
        for n in 0..240 {
            pop.update(1.0 / 60.0);
            let Some(g) = pop.ground(panel, RAD) else { continue };
            // the fill ramp is the HEIGHT's (it leads, so the glass draws from the first step): read it off the rect
            let fill = crate::popover_morph::fill_at(((g.rect.h - pill.h) / (panel.h - pill.h)).clamp(0.0, 1.0));
            opening.push(g.alpha);
            if outside(g.rect) {
                assert!(g.alpha <= fill + 1e-4 && g.shadow <= fill + 1e-4,
                    "{name} open frame {n}: glass {} / shadow {} past the opener outruns the fill ramp {fill}", g.alpha, g.shadow);
            }
        }
        assert!(opening.first().is_some_and(|a| *a < 1.0), "{name}: the first glass frame is not opaque: {opening:?}");
        assert!(opening.windows(2).all(|w| w[1] >= w[0] - 1e-4), "{name}: the glass only strengthens while opening");
        pop.dismiss();
        let mut bead = None;
        for _ in 0..240 {
            pop.update(1.0 / 60.0);
            if !pop.visible() {
                break;
            }
            let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
            if f.ground_alpha <= 0.0 {
                continue;
            }
            bead = Some(f.rect);
        }
        let bead = bead.expect("the close draws glass");
        assert!(bead.w * bead.h <= 0.15 * drain.w * drain.h, "{name}: the last glass frame {bead:?} is a bead, not a slab (drain {drain:?})");
        let end = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        assert_eq!((end.rect, end.radius, end.ground_alpha, end.opener_alpha), (pill, pill.h * 0.5, 0.0, 1.0),
            "{name}: the close lands on the opener's rect and corner, the opener whole");
        pop.close();
    }
}

/// **The opener fill a caller names reaches the glass the close draws**: the last frame of a
/// header menu's close that draws glass wears the opener's focused fill (`Popover::opener_fill`,
/// the colour [`crate::page_header::OPENER_FILL`] the lift paints the pill with) as the join's
/// tint, the glass it DECLARES says so (a different draw from the same frame with no fill), and a
/// popover that names none (the profile chip's menu) joins the glass as it always did.
#[test]
fn the_close_whitens_the_blob_toward_the_fill_its_caller_names() {
    use crate::popover_glass::{face_at, tests::{declared_glass, expected_morphing, latched, unlatch}};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let tex = 9_752;
    let field = latched(tex);
    let sharp = page_tex().unwrap_or(0);
    let (pill, panel) = (crate::Rect::new(1589.0, 146.0, 204.0, 60.0), crate::Rect::new(1380.0, 146.0, 316.0, 400.0));
    let fill = crate::page_header::OPENER_FILL;
    let root = crate::Painter::root();
    // the last close frame that draws glass, as declared and as the morph says it is
    let last_glass = |fill: Option<[f32; 4]>| {
        let mut pop = crate::popover::Popover::new().morphing();
        if let Some(c) = fill {
            pop = pop.opener_fill(c);
        }
        pop.open_from(Some(pill));
        for _ in 0..120 {
            pop.update(1.0 / 60.0);
        }
        pop.dismiss();
        let mut last = None;
        for _ in 0..240 {
            pop.update(1.0 / 60.0);
            if !pop.visible() {
                break;
            }
            let Some(g) = pop.ground(panel, RAD) else { continue };
            let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
            last = Some((g, pop.morph_frame(Some(pill), panel, RAD).unwrap(), declared));
        }
        pop.close();
        last.expect("the close draws glass")
    };
    let (g, f, declared) = last_glass(Some(fill));
    let tint = g.morph.merge.expect("B is joined on the last glass frame").tint;
    assert_eq!((tint[0], tint[1], tint[2]), (fill[0], fill[1], fill[2]), "the colour is the caller's, the one the lift paints");
    assert!(tint[3] >= 0.97, "the last glass frame wears the fill: {tint:?}");
    let a = f.ground_alpha;
    let want = expected_morphing(root.alpha(a), f.rect, f.radius, tex, sharp, face_at(a), f.glass_morph(Some(fill)));
    assert!(declared[0] == want[0], "the glass declares the join's tint: {f:?}");
    let (g_plain, _, plain) = last_glass(None);
    assert_eq!(g_plain.morph.merge.expect("B is joined").tint[3], 0.0, "no fill named, no whitening");
    assert!(declared[0] != plain[0], "the tint is part of the declared draw");
    unlatch(tex);
}

/// **NV1: a panel's near edge is its opener's outer edge**, one rule for every header menu: a control
/// in the right half hangs the panel from its right edge, one in the left half from its left, and
/// the safe frame only ever pulls a panel back, never off the opener's edge otherwise.
#[test]
fn a_panel_hangs_from_its_openers_outer_edge() {
    let keep = crate::consts::SAFE;
    let right = crate::Rect::new(1589.0, 146.0, 204.0, 60.0);
    let left = crate::Rect::new(120.0, 146.0, 230.0, 60.0);
    assert_eq!(crate::popover::hang_x(right, 316.0, keep) + 316.0, right.x + right.w, "right control: right edges flush");
    assert_eq!(crate::popover::hang_x(left, 316.0, keep), left.x, "left control: left edges flush");
    assert_eq!(crate::popover::hang_x(right, right.w, keep), right.x, "a panel as wide as its pill is the pill's own span");
    let at_edge = crate::Rect::new(keep.x - 2.0, 146.0, 100.0, 60.0);
    assert_eq!(crate::popover::hang_x(at_edge, 300.0, keep), keep.x, "held inside the safe frame");
}

/// **Owner, 2026-11-17: a morphing menu opens OVER its opener** (`popover::cover_opener`, the one
/// shared rule): the panel's top edge is the opener's, the near edge hangs by [`hang_x`]'s rule, the
/// panel is never narrower or shorter than the opener, and a panel taller than the room slides UP
/// inside `keep` rather than down off it.
#[test]
fn a_panel_opens_over_its_opener_top_edges_flush() {
    let keep = crate::consts::SAFE;
    let right = crate::Rect::new(1589.0, 146.0, 204.0, 60.0);
    let left = crate::Rect::new(120.0, 146.0, 230.0, 60.0);
    let chip = crate::widgets::chip_cap(1.0, 120.0);
    for pill in [right, left, chip] {
        let panel = crate::popover::cover_opener(pill, (316.0, 400.0), keep);
        assert_eq!(panel.y, pill.y, "{pill:?}: top edges flush");
        assert!(panel.x <= pill.x + 1e-3 && panel.x + panel.w >= pill.x + pill.w - 1e-3, "{pill:?}: {panel:?} covers it across");
        assert!(panel.y + panel.h >= pill.y + pill.h, "{pill:?}: {panel:?} covers it down");
    }
    assert_eq!(crate::popover::cover_opener(right, (316.0, 400.0), keep).x + 316.0, right.x + right.w, "a right control: right edges flush");
    assert_eq!(crate::popover::cover_opener(left, (316.0, 400.0), keep).x, left.x, "a left control: left edges flush");
    // never narrower than the opener, even when the rows are
    let narrow = crate::popover::cover_opener(right, (120.0, 400.0), keep);
    assert_eq!((narrow.x, narrow.w), (right.x, right.w), "a panel as narrow as its rows still spans the opener");
    // a tall panel slides up to keep its bottom on keep's, and still covers the opener
    let tall = crate::popover::cover_opener(right, (316.0, 900.0), keep);
    assert_eq!(tall.y + tall.h, keep.y + keep.h, "the panel's bottom is held inside the safe frame");
    assert!(tall.y < right.y && tall.y >= keep.y, "slid up, not down, and not off the top: {tall:?}");
}

/// DS7: every popover that does not opt in keeps the shared fade+slide, byte for byte.
#[test]
fn the_default_popover_keeps_the_shared_fade_and_slide() {
    let _g = plx_base::testlock::serial();
    let mut pop = crate::popover::Popover::new();
    assert_eq!(pop.open_style(), crate::popover::OpenStyle::Appear);
    assert!(pop.morph_frame(None, crate::Rect::FULL, RAD).is_none());
    pop.open();
    pop.update(1.0 / 60.0);
    let mut reference = crate::Spring::at(0.0);
    reference.step(1.0, crate::popover::K_APPEAR, 1.0 / 60.0);
    assert_eq!(pop.appear(), reference.pos, "the appear spring is the crate::popover::K_APPEAR one");
    pop.close();
}

/// A morphing popover's `panel` draws the GROUND at the morphing rect: nothing while the fill is
/// at the first frame, then the glass at FULL strength (over the lit opener), and the
/// settled panel's own rect at the end. Graded on what `Popover::panel` really DECLARES (a
/// discovery pass, the glass command), against the command built from the morph frame's own rect
/// and radius. Red on the build that faded the glass in under rows that led it: the glass command
/// there carried a part-faded face over the rows (frame dump, n181: rows outside a faint glass).
#[test]
fn a_morphing_panel_grows_from_the_pill_and_its_glass_is_whole_once_it_is_down() {
    use crate::popover_glass::{face_at, tests::{declared_glass, expected, expected_morphing, latched, unlatch}};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let tex = 9_751;
    let field = latched(tex);
    let sharp = page_tex().unwrap_or(0);
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = crate::Painter::root();
    // Frame 0: the opener only, no glass yet.
    assert!(declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu)).is_empty());
    let (mut grew, mut painted) = (false, 0);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        if f.ground_alpha <= 0.0 {
            assert!(declared.is_empty(), "no glass before the fill begins to cross");
            continue;
        }
        assert_eq!(declared.len(), 1);
        painted += 1;
        let want = |r, rad| expected(root, r, rad, tex, sharp, face_at(1.0));
        if declared[0] == want(panel, RAD)[0] {
            // The hand-over to the settled ground: no visible jump in rect OR radius.
            assert!((f.radius - RAD).abs() < 0.5 && (f.rect.h - panel.h).abs() < 1.0, "a snap at settle: {f:?}");
            continue;
        }
        // The glass rides the fill's one ramp, painter alpha and face together (N-b: no opaque slab
        // past the opener), and is whole once the fill is.
        let a = f.ground_alpha;
        // ... wearing the droplet's join (`Frame::glass_morph`, never clear): a morph is joined to the
        // opener's shrinking shape until it has grown, and the declared box is the one around both.
        let ramped = expected_morphing(root.alpha(a), f.rect, f.radius, tex, sharp, face_at(a), f.glass_morph(None));
        assert!(declared[0] == ramped[0], "mid-open: the morph rect and radius on the fill ramp {a}: {f:?}");
        grew = true;
        // the open rings past the panel (height ~6%, width ~4.6% of the travel) before it rests
        assert!(f.rect.w <= panel.w + 0.1 * (panel.w - pill.w) && f.rect.h <= panel.h + 0.1 * (panel.h - pill.h) && f.rect.w >= pill.w - 1e-3);
    }
    assert!(painted > 0 && grew, "mid-open the ground is the morphing rect, not the settled one");
    let settled = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
    assert_eq!(settled.len(), 1, "settled: the glass stands");
    pop.close();
    unlatch(tex);
}

/// **A menu's glass stands on ONE capture of its host for its whole life: the open, the stay and
/// the close.** Frames of a menu over a still page, through the real host protocol: the snapshot is
/// taken once ([`page_epoch`] moves once) for the open and the stay, however its own springs move,
/// and the dismiss throws nothing away (`Popover::release` drops only a GROUND-stage snapshot, which
/// a morphing panel's `Live` host never holds), so the close's first frame is not the frame that
/// re-draws the page into the snapshot, re-sources the tab bar's blur and re-latches the dim field
/// (131% of a 60 Hz frame's GPU budget on the TV, measured). The glass's rim reads the snapshot's
/// texture (the CPU stand-in's).
#[test]
fn a_menu_glass_reads_one_host_capture_through_open_stay_and_close() {
    use crate::popover_glass::tests::{declared_glass, expected, latched, unlatch};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let tex = 9_752;
    let field = latched(tex);
    let panel = crate::Rect::new(1380.0, 40.0, 316.0, 400.0);
    let epoch = page_epoch();
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(crate::Rect::new(1500.0, 40.0, 196.0, 38.0)));
    let (mut frames, mut drawn, mut closing) = (0, 0, false);
    while pop.visible() {
        pop.update(1.0 / 60.0);
        begin_frame(&_reset.modal, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        {
            let _page = page_pass(&_reset.modal);
            ink("page");
            let _scrim = live();
            ink("scrim");
        }
        let sharp = page_tex().expect("the host holds its undimmed page");
        let glass = declared_glass(|| pop.panel(crate::Painter::root(), panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        assert!(glass.len() <= 1);
        drawn += glass.len();
        frames += 1;
        // Not one frame of the close re-takes the host, the first included.
        if closing {
            assert_eq!(page_epoch().wrapping_sub(epoch), 1, "frame {frames} of the close re-took the host");
        }
        if !closing && pop.appear_settled() {
            assert_eq!(page_epoch().wrapping_sub(epoch), 1, "one host capture for the open and the stay");
            // The settled panel's glass, against one built with this capture's texture as its
            // rim's source and against one with none.
            let want = |sharp| expected(crate::Painter::root(), panel, RAD, tex, sharp, crate::popover_glass::FACE);
            assert!(sharp != 0 && glass[0] == want(sharp)[0], "the rim reads the host snapshot");
            assert!(glass[0] != want(0)[0]);
            // The app captures input attribution and addresses only its owned surface.
            assert!(!_reset.modal.read.closing_only, "premise: the close key is delivered to the open panel");
            let mut present = plx_machine::present::Present::new();
            present.note_in_scope(plx_machine::present::Scope::Surface,
                plx_machine::present::PresentEvent::Damage(plx_machine::present::Provenance::Input));
            assert!(!present.page_damaged());
            let invalidated = invalidations();
            pop.dismiss();
            assert_eq!(invalidations(), invalidated, "the dismiss threw the host snapshot away");
            closing = true;
        }
        assert!(frames < 600);
    }
    assert!(closing && drawn > 0, "the glass drew: {drawn}");
    assert_eq!(page_epoch().wrapping_sub(epoch), 1, "one capture for the open, the stay and the close, over {frames} frames");
    unlatch(tex);
}

/// **A morphing glass asks for its frost on the open's FIRST frame, before its field has latched, and
/// wears no clear body on any frame while that frost is still owed** (owner, 2026-11-18: frosted from
/// the first visible frame). The field's read lands a frame after the capture, so the first frame draws
/// no glass (`draw_shadowed` finds no ground) but the frost's build, which waits for a later frame's
/// head than the ask, is owed from that frame and stands when the glass first draws. A frame that still
/// finds no frost wears the field under the heavier face, the join and the lens of the morph and
/// nothing clear: it used to be forced CLEAR (`GlassMorph::clear` 1), the transparent flash.
#[test]
fn a_morphing_glass_asks_for_its_frost_before_its_field_latches_and_is_never_clear_while_it_is_owed() {
    use super::mock::{FROST_ASKED, FROST_PENDING};
    use crate::popover_glass::tests::{declared_glass, expected_morphing, latched, unlatch};
    let _g = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    FROST_PENDING.with(|p| p.set(true));
    FROST_ASKED.with(|a| a.set(None));
    let tex = 9_753;
    let field = latched(tex);
    let unlatched = crate::underlay::UnderlayField::new();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = crate::Painter::root();
    let mut judged = 0;
    for frame in 0..10 {
        pop.update(1.0 / 60.0);
        begin_frame(&_reset.modal, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        {
            let _page = page_pass(&_reset.modal);
            ink("page");
            let _scrim = live();
            ink("scrim");
        }
        let sharp = page_tex().expect("the host holds its undimmed page");
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        if frame == 0 {
            // the open's first frame: the field has not latched
            let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&unlatched), crate::widgets::Ground::Menu));
            assert!(declared.is_empty(), "no latched field, no glass");
            assert_eq!(FROST_ASKED.with(|a| a.get()), Some([panel.x, panel.y, panel.w, panel.h]), "the frost is asked for before the field has latched");
            continue;
        }
        let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        assert_eq!(declared.len(), 1, "frame {frame}: the glass draws on the latched field");
        // the morph's own join and lens, the field under the heavier face, and NO clarity
        let a = f.ground_alpha;
        let want = expected_morphing(root.alpha(a), f.rect, f.radius, tex, sharp, crate::popover_glass::face_at(a), f.glass_morph(None));
        assert!(declared[0] == want[0], "frame {frame}: a body that is not the field under the face: {f:?}");
        let clear = expected_morphing(root.alpha(a), f.rect, f.radius, tex, sharp, crate::popover_glass::face_at(a),
            plx_gfx::gfx::GlassMorph { clear: 1.0, ..f.glass_morph(None) });
        assert!(declared[0] != clear[0], "frame {frame}: the body is clear while a frost is owed");
        judged += 1;
    }
    assert!(judged >= 8, "frames were judged: {judged}");
    // ... and the CLOSE of that mid-open menu, frost still owed: no frame that draws the glass draws it
    // clear (what `Popover::ground` hands the panel is the morph's join and the face, never clarity)
    pop.dismiss();
    let mut closing = 0;
    for frame in 0..60 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        begin_frame(&_reset.modal, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        {
            let _page = page_pass(&_reset.modal);
            ink("page");
            let _scrim = live();
            ink("scrim");
        }
        let sharp = page_tex().expect("the host holds its undimmed page");
        let Some(g) = pop.ground(panel, RAD) else { continue };
        let declared = declared_glass(|| pop.panel(root, panel, RAD, Some(&field), crate::widgets::Ground::Menu));
        let Some(drawn) = declared.first() else { continue };
        let want = expected_morphing(root.alpha(g.alpha), g.rect, g.radius, tex, sharp, crate::popover_glass::face_at(g.alpha), g.morph);
        let clear = expected_morphing(root.alpha(g.alpha), g.rect, g.radius, tex, sharp, crate::popover_glass::face_at(g.alpha),
            plx_gfx::gfx::GlassMorph { clear: 1.0, ..g.morph });
        assert!(*drawn == want[0], "close frame {frame}: a body that is not the field under the face");
        assert!(*drawn != clear[0], "close frame {frame}: the body is clear");
        closing += 1;
    }
    assert!(closing >= 8, "close frames were judged: {closing}");
    FROST_PENDING.with(|p| p.set(false));
    FROST_ASKED.with(|a| a.set(None));
    pop.close();
    unlatch(tex);
}

/// **The dismiss still drops a GROUND-stage snapshot — the one thing `Popover::release` has to
/// protect.** A `Held::Ground` snapshot is page + scrim + lifted opener + the panel's own ground at
/// FULL strength, so a fade drawn over it would keep showing the settled panel; it must be retaken
/// from the live ground. (Only a `caching_host` popover reaches that stage: `decision_alert` is the
/// production one. A `Live` host, every glass menu, never holds a ground, and a plain page snapshot
/// is exactly what a fade-out wants to keep — see
/// `a_menu_glass_reads_one_host_capture_through_open_stay_and_close`.) Green before and after the
/// release narrowed from `invalidate` to `ground_invalidate`.
#[test]
fn a_dismiss_drops_the_ground_stage_of_a_caching_popover() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset::new(false);
    let mut pop = crate::popover::Popover::new().caching_host();
    pop.open();
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
    }
    assert!(pop.appear_settled(), "premise: the panel has landed");
    for _ in 0..3 {
        begin_frame(&_reset.modal, false);
        PIXELS.with(|p| p.borrow_mut().clear());
        let _page = page_pass(&_reset.modal);
        ink("page");
        let _scrim = live();
        ink("scrim");
        ground_drawn(pop.appear_settled());
    }
    assert!(matches!(held(), Held::Ground(_)), "premise: the settled panel's ground is held");
    pop.dismiss();
    assert!(held() == Held::Nothing, "the dismiss kept a snapshot of the panel at full strength");
    pop.close();
}

/// Finding 1: the ground's radius is the caller's through the whole life of the panel. The old
/// morph ended on 22 and `Popover::ground` snapped to the caller's 26 at settle, and back to 22
/// when the close began; the largest per-frame step across those two seams is graded here.
#[test]
fn the_ground_radius_does_not_snap_at_settle_or_at_the_start_of_a_close() {
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (crate::Rect::new(1500.0, 40.0, 196.0, 38.0), crate::Rect::new(1380.0, 40.0, 316.0, 400.0));
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let (mut prev, mut seam) = (None::<f32>, None::<f32>);
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        let Some(crate::popover::MorphGround { radius: rad, .. }) = pop.ground(panel, RAD) else { continue };
        if pop.appear_settled() && seam.is_none() {
            let before = prev.expect("frames before the settle");
            seam = Some((rad - before).abs());
            assert_eq!(rad, RAD, "settled: the caller's radius");
        }
        prev = Some(rad);
    }
    let seam = seam.expect("the open settled");
    assert!(seam < 1.0, "the radius jumps {seam} px at settle");
    // The close starts on the caller's radius from BOTH sources (the settled ground and the morph's
    // own curve at full progress), then follows the morph's one curve: the ground has no second
    // radius of its own to snap to.
    pop.dismiss();
    let at_rest = pop.morph_frame(Some(pill), panel, RAD).unwrap().radius;
    assert!((at_rest - RAD).abs() < 0.05, "the morph's curve starts the close on {at_rest}, not the caller's {RAD}");
    pop.update(1.0 / 60.0);
    let first_close = pop.ground(panel, RAD).expect("the close still has a ground").radius;
    assert_eq!(first_close, pop.morph_frame(Some(pill), panel, RAD).unwrap().radius, "one curve");
    pop.close();
}

/// Spec (c): the rows of an unsettled morph are clipped to the surface's ROUND LENS (its rect and radius, through the shared
/// painter's rounded clip, so a letter is cut by the arc and not by a straight edge) on every frame
/// they draw, scaled about the panel anchor by the row spring, at the content alpha, and not drawn
/// at all before the glass has taken over (but still walked by the text recorder, so their text is
/// resident in time); settled (and the fade+slide) they draw through `p` untouched. Red on the
/// build whose rows led the glass: rows drew at progress 0.05, on a surface that was still pill.
#[test]
fn morph_rows_are_clipped_to_the_surface_and_wait_for_the_glass() {
    use crate::{popover_morph::rows_anchor, screen::{ClipScope, RoundClip}, Painter, Rect};
    let _g = plx_base::testlock::serial();
    let (pill, panel) = (Rect::new(1500.0, 40.0, 196.0, 38.0), Rect::new(1380.0, 40.0, 316.0, 400.0));
    let row = Rect::new(panel.x + 10.0, panel.y + 100.0, panel.w - 20.0, 60.0);
    let mut pop = crate::popover::Popover::new().morphing();
    pop.open_from(Some(pill));
    let root = Painter::root();
    let mut drew_held = false;
    pop.morph_rows(Painter::recording(), panel, RAD, |_| drew_held = true);
    assert!(drew_held, "the recorder walks the rows even while they are invisible");
    let (mut mid, mut waited) = (0, 0);
    let mut check = |pop: &crate::popover::Popover| {
        let f = pop.morph_frame(Some(pill), panel, RAD).unwrap();
        let mut drew = false;
        pop.morph_rows(root, panel, RAD, |p| {
            drew = true;
            if p.clip_rect() == Rect::FULL {
                // The morph has settled (a visual-arrival test, within 0.1% of rest): untouched rows.
                assert!((f.rect.w - panel.w).abs() < 1.0 && (f.rows_scale - 1.0).abs() < 0.01, "only a settled morph is unclipped: {f:?}");
                assert_eq!(ClipScope::current(), None);
                assert_eq!(ClipScope::current_round(), None, "settled: the handover crops nothing");
                assert_eq!(p.place(row), row, "settled: rows at their final layout");
                return;
            }
            mid += 1;
            assert!(f.content_alpha > 0.0 && f.ground_alpha == 1.0, "rows drawn on a surface that is not yet glass: {f:?}");
            assert!((p.opacity() - f.content_alpha).abs() < 1e-6, "at the content alpha");
            let same = |a: Rect, b: Rect| (a.x - b.x).abs() + (a.y - b.y).abs() + (a.w - b.w).abs() + (a.h - b.h).abs() < 1e-2;
            assert!(same(p.clip_rect(), f.rect), "the cascade clip is the surface's rect");
            assert!(same(ClipScope::current().unwrap(), f.rect), "the scissor is the lens's bounding box");
            let round = ClipScope::current_round().expect("a morph frame's rows are cut by the lens");
            assert!(same(round.rect, f.rect) && (round.radius - f.radius).abs() < 1e-2, "the lens, rect and radius: {round:?} vs {f:?}");
            assert_eq!(round, RoundClip { rect: round.rect, radius: round.radius });
            let placed = p.place(row);
            assert!((placed.w - row.w * f.rows_scale).abs() < 1e-3 && (placed.h - row.h * f.rows_scale).abs() < 1e-3, "scaled by the lens spring");
            let (ax, ay) = rows_anchor(Some(pill), panel);
            let fixed = p.place(Rect::new(ax, ay, 1.0, 1.0));
            assert!((fixed.x - ax).abs() < 1e-3 && (fixed.y - ay).abs() < 1e-3, "the anchor does not move");
        });
        if !drew {
            assert_eq!(f.content_alpha, 0.0, "only invisible rows are skipped: {f:?}");
            waited += 1;
        }
        assert_eq!(ClipScope::current(), None, "the scissor is released");
        assert_eq!(ClipScope::current_round(), None, "the rounding is released");
    };
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        check(&pop);
    }
    pop.dismiss();
    for _ in 0..240 {
        pop.update(1.0 / 60.0);
        if !pop.visible() {
            break;
        }
        check(&pop);
    }
    assert!(mid > 5 && waited >= 4, "the morph ran through unsettled frames ({mid}) and frames without rows ({waited})");
    // The fade+slide popover is never clipped or scaled.
    let mut plain = crate::popover::Popover::new();
    plain.open();
    plain.update(1.0 / 60.0);
    plain.morph_rows(root, panel, RAD, |p| {
        assert_eq!(p.clip_rect(), Rect::FULL);
        assert_eq!(p.place(row), row);
    });
    plain.close();
    pop.close();
}

/// One frame of a surface over a page, as `embedded_alert_frame` draws it, with the page's ink
/// named so a test sees WHICH page was drawn. The page raises damage (it is moving).
fn reflow_frame(modal: &crate::containers::modal::ModalPresentation, page: &'static str, moving: bool) -> Vec<&'static str> {
    let mut present = plx_machine::present::Present::new();
    if moving {
        plx_machine::machine::PresentHandle::of(&mut present).note(
            plx_machine::present::PresentEvent::Damage(plx_machine::present::Provenance::Input));
    }
    super::begin_frame(modal, 1.0 / 60.0, moving, present.page_damaged());
    PIXELS.with(|p| p.borrow_mut().clear());
    {
        let _page = page_pass(modal);
        ink(page);
        {
            let _scrim = live();
            ink("scrim");
        }
        {
            let _panel = live();
            ink("glass");
            ground_drawn(true);
            ink("foreground");
        }
    }
    PIXELS.with(|p| p.borrow().clone())
}

fn counts() -> (u32, u32, u32, u32, usize) {
    (
        page_epoch(),
        mock::CAPTURES.with(|c| c.get()),
        mock::PREV_COPIES.with(|c| c.get()),
        mock::PREV_RELEASES.with(|c| c.get()),
        mock::PREV_DRAWS.with(|d| d.borrow().len()),
    )
}

/// **A page that reflows under a glass menu is drawn live, and the host is retaken ONCE when it
/// stops.** Open and settle (one page capture, one ground capture), then ten frames of a moving
/// page: each shows its own page ink (live, not the held picture), `page_epoch` does not move and
/// nothing is captured per frame. The reflow's end costs the one old-look copy on its exit frame
/// (still drawn live), then on the next frame exactly one page capture (epoch + 1) and one
/// ground capture. What this counts: `page_epoch` (the page stage), `CAPTURES` (every snapshot
/// into the main slot, page or ground), `PREV_COPIES` (the old-side copy, counted apart),
/// `PREV_RELEASES` (the old copy given back).
#[test]
fn a_reflowing_page_is_live_under_the_glass_and_the_host_is_retaken_once() {
    let _serial = plx_base::testlock::serial();
    let mut _reset = Reset::new(false);
    reset_reflow();
    mock::CAPTURES.with(|c| c.set(0));
    mock::PREV_COPIES.with(|c| c.set(0));
    mock::PREV_RELEASES.with(|c| c.set(0));
    mock::PREV_DRAWS.with(|d| d.borrow_mut().clear());
    // open: the page stage, then the settled ground
    for _ in 0..3 {
        reflow_frame(&_reset.modal, "page-open", false);
    }
    assert!(matches!(held(), Held::Ground(_)), "premise: the ground is held");
    let (epoch, caps, ..) = counts();
    assert_eq!(caps, 2, "premise: one page capture and one ground capture to open");

    _reset.modal.reflow = true;
    for k in 0..10 {
        let name: &'static str = ["r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9"][k];
        assert_eq!(reflow_frame(&_reset.modal, name, true), vec![name, "scrim", "glass", "foreground"], "frame {k}: the live page, under the glass");
        let (e, c, ..) = counts();
        assert_eq!((e, c), (epoch, caps), "frame {k}: nothing is captured while the page moves");
    }

    // it stops: the exit frame is still live and keeps the old look
    _reset.modal.reflow = false;
    assert_eq!(reflow_frame(&_reset.modal, "r-final", false), vec!["r-final", "scrim", "glass", "foreground"], "the exit frame is live");
    let (e, c, copies, released, _) = counts();
    assert_eq!((e, c, copies, released), (epoch, caps, 1, 0), "the exit frame copies the old look, once, and captures nothing");

    // the next frame takes the host once: page (epoch + 1) and ground (one more snapshot)
    reflow_frame(&_reset.modal, "r-final", false);
    let (e, c, copies, released, draws) = counts();
    assert_eq!(e, epoch + 1, "page_epoch moved exactly once for the whole reflow");
    assert_eq!(c, caps + 2, "one page capture and one ground capture, nothing per frame");
    assert_eq!((copies, released, draws), (1, 0, 1), "the old look is kept and drawn over the new ground");
    assert_eq!(crossfade(), Some(0.0), "the blend starts at 0: all old look");

    // the blend runs to 1 over about 150 ms of ticks, drawing the old look at 1 - b
    let mut frames = 0;
    while crossfade().is_some() && frames < 30 {
        reflow_frame(&_reset.modal, "r-final", false);
        frames += 1;
    }
    assert!(crossfade().is_none(), "the blend finished");
    assert!((8..=11).contains(&frames), "about 150 ms at 60 fps, got {frames} frames");
    let drawn: Vec<f32> = mock::PREV_DRAWS.with(|d| d.borrow().clone());
    assert!(drawn.windows(2).all(|w| w[1] < w[0]), "the old look fades monotonically: {drawn:?}");
    assert_eq!(drawn[0], 1.0);
    let (e, c, copies, released, _) = counts();
    assert_eq!((e, c, copies, released), (epoch + 1, caps + 2, 1, 1), "still one retake in all; the old texture is released at b = 1");
    assert!(!cache(|c| c.has_prev()), "and the cache keeps no second slot");
    let after = counts().4;
    reflow_frame(&_reset.modal, "r-final", false);
    assert_eq!(counts().4, after, "no old-look quad once it is released");
}
