//! `TestApp` (restructure spec §15.1 `a_new_screen_is_unit_tested_with_no_sdl`): a screen under
//! test with no SDL, no GL and no television — the dispatcher over the fixture rig (its stub
//! adapters and `FixtureMeasure`) on a `VirtualClock`. Every migrated screen ships one test
//! through this: boot it, press keys, read what it did.
#![cfg(test)]

use super::dispatch::{Dispatcher, FrameReport, NoTap};
use super::fixture::{key, FixtureArg, FixtureHost, FixtureRig};
use super::machine::{Clock, Key, MachineId, NavOp, Tick, VirtualClock};

pub struct TestApp {
    pub d: Dispatcher<FixtureHost>,
    pub rig: FixtureRig,
    pub clock: VirtualClock,
    pub last: FrameReport,
}

impl TestApp {
    /// A booted app: `Root(Home)` committed on frame 1 of a 60 Hz virtual clock.
    pub fn boot() -> Self {
        let ticks = (0..10_000u32).map(|i| Tick {
            ms: i * 16,
            dt_us: 16_000,
        });
        let mut app = Self {
            d: Dispatcher::new(),
            rig: FixtureRig::new(),
            clock: VirtualClock::new(ticks),
            last: FrameReport::default(),
        };
        app.d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
        app.frame();
        app
    }

    /// One frame with no input.
    pub fn frame(&mut self) -> &FrameReport {
        let t = self.clock.tick();
        self.last = self.d.frame(&mut self.rig, t, vec![], vec![], &mut NoTap);
        let un = self.last.unmounted.clone();
        self.d.prune(&un);
        &self.last
    }

    /// Run `n` frames with no input.
    pub fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    /// One frame carrying one key press (the DOWN edge).
    pub fn press(&mut self, k: Key) -> &FrameReport {
        let t = self.clock.tick();
        self.last = self.d.frame(&mut self.rig, t, vec![key(k, t)], vec![], &mut NoTap);
        let un = self.last.unmounted.clone();
        self.d.prune(&un);
        &self.last
    }

    /// The heartbeat word of the top page.
    pub fn top_name(&self) -> &'static str {
        self.d.top_screen().map_or("", |s| s.name())
    }

    /// The input owner's screen state, probed.
    pub fn owner_probe(&self) -> String {
        let mut s = String::new();
        if let Some(super::machine::InputOwner::Entry(e)) = self.d.nav.input_owner() {
            if let Some(i) = self.d.nav.entry(e).and_then(|e| e.inst.as_ref()) {
                i.screen.state().probe(&mut s);
            }
        }
        s
    }
}

/// Spec §15.1: a screen is driven through the whole contract — mount, input, structural ops,
/// prepare, draw, the hit map — with nothing linked but the library.
#[test]
fn a_new_screen_is_unit_tested_with_no_sdl() {
    let mut app = TestApp::boot();
    assert_eq!(app.top_name(), "home");
    assert!(app.last.presented, "boot presents");
    app.press(Key::Ok);
    assert_eq!(app.top_name(), "detail", "OK opened a page in the same frame");
    assert_eq!(app.d.nav.tabs.stack.depth(), 2);
    assert!(app.owner_probe().contains("\"mount\", \"enter\""));
    app.frames(3);
    assert!(!app.last.presented, "a settled page stops presenting");
    app.press(Key::Back);
    assert_eq!(app.top_name(), "home", "BACK resolved over the page stack");
    assert_eq!(app.d.nav.tabs.stack.depth(), 1);
    let r = app.press(Key::Back);
    assert!(r.back_at_root, "BACK at Home's root is the application's call");
    assert_eq!(app.d.nav.tabs.stack.depth(), 1, "…and pops nothing");
}
