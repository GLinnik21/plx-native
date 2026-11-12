//! Conformance mount of the real Live TV channel shelf.
use super::*;
use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::{At, By, Dir, Placed, Step};

struct LiveHost;
impl Host for LiveHost {
    type Arg = crate::registry::SettingsPage;
    type Fx = AppFx;
    type Msg = crate::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = LiveTvView<'a>;
    type Init = crate::registry::NoInit;
    type Memory = crate::registry::PageMemory;
}
impl LiveTvLike for LiveHost {
    fn livetv<'a>(cx: &Cx<'a, Self>) -> LiveTvView<'a> { cx.views }
}
const ENTRY: EntryId = EntryId(9);
struct Harness {
    store: plx_data::stores::livetv::LiveTvStore,
    screen: LiveTvScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
    n: usize,
}
pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(n)) }
impl Harness {
    fn new(n: usize) -> Self {
        let mut store = plx_data::stores::livetv::LiveTvStore::default();
        store.install_for_test((0..n).map(|i| plx_data::livetv::Channel {
            sid: plx_plex::plex::ServerId::UNSET, dvr: "1".into(), channel: format!("c{i}"),
            vcn: i.to_string(), name: format!("Channel {i}"), thumb: String::new(),
            now: format!("Programme {i}"), now_thumb: String::new(), ends_at: 0,
        }).collect());
        let mut h = Self { store, screen: LiveTvScreen::new(ENTRY), focus: None, press: 1.0, ms: 0, n };
        h.step(ScreenEvent::Tick(Tick { ms: 0, dt_us: 16_667 }));
        h
    }
    fn cx(&self) -> Cx<'_, LiveHost> {
        Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }
    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }
    fn step(&mut self, ev: ScreenEvent<LiveHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(9)), &mut present);
            Machine::<LiveHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }
}
impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> { (0..self.n).map(|i| FIRST_CARD_ELEM + i as u32 * 2).collect() }
    fn focused(&self) -> Option<u32> { self.focus.map(|k| k.elem) }
    fn focus(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.step(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }
    fn tick(&mut self, frames: u32) -> bool {
        let mut moving = false;
        for _ in 0..frames { self.ms += 16; moving |= self.step(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 })); }
        moving
    }
    fn cover(&mut self) { self.step(ScreenEvent::Cover); }
    fn uncover(&mut self) { self.step(ScreenEvent::Uncover); }
    fn set_press(&mut self, scale: f32) { self.press = scale; }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match Focusable::<LiveHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem), Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> { Focusable::<LiveHost>::place(&self.screen, &elem, &self.cx(), at) }
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let cx = Cx { press: PressRead { scale: press, owner: self.focus, ..Default::default() }, ..self.cx() };
        let mut frame = DrawFrame::new(&cx, plx_ui::Painter::root());
        self.screen.stack.view(&self.screen.page).record_stops(&mut frame);
        frame.stops().iter().find(|s| s.key.elem == elem).map(|s| s.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> { self.screen.stack.view(&self.screen.page).scale_of(&self.cx(), &elem) }
    fn focus_scale(&self) -> f32 { STYLE.focus_scale }
    fn canon(&self) -> u64 { self.screen.hash() }
    fn identity(&self, elem: u32) -> String { format!("c{}", (elem - FIRST_CARD_ELEM) / 2) }
    fn scroll(&self) -> Option<f32> { Some(self.screen.stack.scroll()) }
    fn landing(&mut self, _landing: Landing) -> Result<(), &'static str> { Err("channel lineup uses guide order") }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let mut fresh = Self::new(self.n);
        let elem = self.focus.ok_or("nothing focused")?.elem;
        fresh.focus(elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
