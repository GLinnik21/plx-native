//! **A Settings page's first open rasterises none of its text in a visible frame.**
//!
//! Measured on the television (release build): the first open of Video & playback or Language cost
//! one late frame of 28-30 ms whose `spans=` read `textx33:18.2` / `textx23:18.2` — the destination's
//! strings met the glyph cache cold in the push's first drawn frame. The push commits at once
//! (the page is mounted and owns input), so nothing walked the destination through the text
//! recorder ahead of the spring. The surface now does: while a push waits for its page's text
//! ([`RoutePush::begin_warm`]) the page is walked off-screen ([`Screen::draw_warm`]), the
//! dispatcher drains the queue under the shared prewarm budget, and the spring starts when the
//! latched readiness says the queue is empty.
//!
//! The host has no GL, so "rasterised" is the text module's residency ledger
//! (`prewarm_resident_any_size_for_test`), written at the same boundary the device backend
//! rasterises and uploads through, and the loop below is the dispatcher's surface pass in miniature:
//! latch the queue, tick, draw live, walk what the surface holds back, drain under a counted budget.
//! `plx_ui::containers::tests` runs the same contract through the real `Dispatcher`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_machine::present::Present;

/// One dispatcher iteration over a lone surface. Returns, once the push has left 0 (a live draw
/// would show the incoming page), the strings of that page that were NOT resident at that moment.
struct Frames {
    ms: u32,
    /// Every string of the destination, learnt from a recording walk before the push begins.
    strings: Vec<String>,
}

impl Frames {
    fn new(strings: Vec<String>) -> Self {
        Self { ms: 1000, strings }
    }

    fn frame(&mut self, s: &mut RouteSurface) -> Option<Vec<String>> {
        self.ms += 16;
        let outer = cx(None);
        let c = inner_cx(&outer);
        // the product loop latches the queue once, before the springs step
        plx_gfx::text::latch_surface_text_pending(plx_gfx::text::prewarm_pending());
        {
            let mut out = Vec::new();
            let mut present = Present::new();
            let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
            let t = Tick { ms: self.ms, dt_us: 16_000 };
            s.tick(t, &outer, &mut fx);
        }
        let seen = (s.push.amount() > 0.01 && s.push.open).then(|| {
            self.strings
                .iter()
                .filter(|text| !plx_gfx::text::prewarm_resident_any_size_for_test(text.as_bytes()))
                .cloned()
                .collect::<Vec<_>>()
        });
        // (the live draw of a real page needs GL, which the host lacks: what it WOULD show is
        // `seen` above, the push having left 0 with the child at the top)
        // the surface pass: whatever the surface holds back is walked through the recorder and
        // drained under the frame's prewarm budget
        plx_gfx::text::clear_prewarm();
        if Screen::<InnerHost>::warms_text(s) {
            let mut w = DrawFrame::new(&c, Painter::recording());
            plx_ui::record_walk(|| Screen::<InnerHost>::draw_warm(s, &mut w));
            let mut clock = 0u64;
            plx_gfx::text::drain_prewarm(plx_ui::containers::transition::TEXT_PREWARM_BUDGET_US, || {
                clock += 500;
                clock
            });
        }
        seen
    }
}

/// Every string `page` hands a recording painter, drawn at rest.
fn strings_of(page: SettingsPage) -> Vec<String> {
    let outer = cx(None);
    let c = inner_cx(&outer);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let mut screen = mount_page(EntryId(1), page, &c, &mut fx);
    let mut f = DrawFrame::new(&c, Painter::recording());
    let runs = plx_gfx::text::capture_text_runs_for_test(|| plx_ui::record_walk(|| screen.draw(&mut f)));
    plx_gfx::text::reset_prewarm_for_test();
    runs
}

fn push(s: &mut RouteSurface, page: SettingsPage) {
    let outer = cx(None);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    s.request(NavOp::Push(page), &outer, &mut fx);
}

fn pop(s: &mut RouteSurface) {
    let outer = cx(None);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    s.request(NavOp::Pop, &outer, &mut fx);
}

/// Run frames until the push has settled; the first frame a live draw showed the incoming page is
/// where the cold strings are counted.
fn run_push(frames: &mut Frames, s: &mut RouteSurface) -> Vec<String> {
    let mut first_visible = None;
    for _ in 0..120 {
        let seen = frames.frame(s);
        if first_visible.is_none() {
            first_visible = seen;
        }
        if s.at_rest() {
            break;
        }
    }
    assert!(s.at_rest(), "the push settled");
    first_visible.expect("the incoming page was shown")
}

#[test]
fn a_pushed_settings_page_has_its_text_resident_before_it_is_first_shown() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("settings-warm-first-open");
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    plx_gfx::text::reset_prewarm_for_test();
    for page in [SettingsPage::Playback, SettingsPage::Language] {
        let strings = strings_of(page);
        assert!(strings.len() >= 10, "premise: {page:?} is a page of real text ({})", strings.len());
        plx_gfx::text::reset_prewarm_for_test();

        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let mut frames = Frames::new(strings);
        for _ in 0..4 {
            frames.frame(&mut s);
        }
        push(&mut s, page);
        let cold = run_push(&mut frames, &mut s);
        assert!(
            cold.is_empty(),
            "{page:?}: {} strings were still to be rasterised in the first frame the page was shown: {cold:?}",
            cold.len()
        );
        assert!(!plx_gfx::text::prewarm_pending(), "{page:?}: the queue is empty once it is shown");
        plx_gfx::text::reset_prewarm_for_test();
    }
}

#[test]
fn opening_the_same_page_again_rasterises_nothing_new() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("settings-warm-second-open");
    let _en = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    plx_gfx::text::reset_prewarm_for_test();
    let strings = strings_of(SettingsPage::Playback);
    plx_gfx::text::reset_prewarm_for_test();

    let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
    step(&mut s, ScreenEvent::Mount, None);
    let mut frames = Frames::new(strings);
    for _ in 0..4 {
        frames.frame(&mut s);
    }
    push(&mut s, SettingsPage::Playback);
    assert!(run_push(&mut frames, &mut s).is_empty(), "premise: the first open is warm");
    let after_first = plx_gfx::text::rasterised_for_test();
    assert!(after_first > 0, "premise: the first open rasterised its strings ahead of the page");

    pop(&mut s);
    for _ in 0..60 {
        frames.frame(&mut s);
        if s.at_rest() {
            break;
        }
    }
    push(&mut s, SettingsPage::Playback);
    assert!(run_push(&mut frames, &mut s).is_empty(), "the second open is warm too");
    assert_eq!(
        plx_gfx::text::rasterised_for_test(),
        after_first,
        "a page that is already resident rasterised more strings on its second open"
    );
    plx_gfx::text::reset_prewarm_for_test();
}
