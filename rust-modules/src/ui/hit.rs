//! The hit map (restructure spec §7.5, §7.6), owned by `Input` and DOUBLE-BUFFERED: the draw
//! fills the back map through `DrawFrame::stop` (rects already in screen space, the cascade's
//! translate, pop and clip folded in — painter order IS z), the swap happens only on a PRESENTED
//! frame, and a non-drawn frame resolves against the previous map. Resolution is `top_at(x, y)`
//! — the LAST stop whose visible part (`rect ∩ clip`) contains the point — and a miss is the
//! `ModalStack`'s to answer.
//!
//! The pointer policies live on the STOP (`hover`, `activate`); the gates live here. There are
//! two, and **only the first of them is wired to the product today**:
//!
//! * `dpad_mode` suppresses hover until the pointer has travelled `DPAD_TRAVEL_PX` since the last
//!   D-pad press. Live: `Dispatcher`'s ingest calls [`HitMap::note_dpad`] on every direction key.
//! * `suppressed` is the fading-surface gate — below an alpha threshold a surface should answer
//!   no hover and no click at all. **The mechanism exists and nothing sets it**: outside this
//!   module's own unit test, `suppressed` is written nowhere, because no container feeds a
//!   surface's `Opening`/`Closing` phase or its appear-spring alpha into the map. This paragraph
//!   used to state the gate as a fact, which is how it went unnoticed — the honest reading is
//!   that a click during a surface's ~0.5 s dismissal fade still resolves against the stops that
//!   were registered on the last presented frame.
//!
//! Screens that must not be clickable through their own fade therefore have to say so themselves
//! for now — `screens/consent.rs` gates its commit arms on `DecisionAlert::visible()` rather than
//! `is_open()` for exactly this reason, and says so at each guard. Wiring the container phase in
//! here would let those screens drop that defence; it is a library change with a blast radius
//! across every surface, so it belongs to the phase that owns the hit map's remaining work
//! (spec §7.5-§7.6) rather than to the screen migration that found it (2026-09-07).

use super::machine::{EntryId, FocusKey};
use super::screen::{Activate, Hover, Stop};

/// How far the pointer must travel after a D-pad press before hover parks focus again.
pub const DPAD_TRAVEL_PX: f32 = 120.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointerKind {
    Move,
    Click,
    Drag,
}

/// What a pointer event resolved to (§7.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolution<K> {
    /// The stop under the pointer, if any (what the owner's `Input` event carries as `hit`).
    pub hit: Option<FocusKey<K>>,
    /// Park focus here (a hover that the stop's policy and the gates allow).
    pub focus: Option<FocusKey<K>>,
    /// A click landed: what the stop asks for.
    pub activate: Option<(FocusKey<K>, Activate)>,
    /// A click landed on no stop.
    pub miss: bool,
}

pub struct HitMap<K> {
    front: Vec<Stop<K>>,
    back: Vec<Stop<K>>,
    /// The D-pad was used since the pointer last moved far enough: hover is suppressed.
    pub dpad_mode: bool,
    dpad_at: (f32, f32),
    last: (f32, f32),
    /// A fading surface below the threshold: no hover, no click. **Nothing outside this module's
    /// tests sets this yet** — see the module doc; it is honoured by [`Self::resolve`] but never
    /// raised, so it is a mechanism waiting for its caller rather than a live gate.
    pub suppressed: bool,
}

impl<K: Copy + Eq> Default for HitMap<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq> HitMap<K> {
    pub fn new() -> Self {
        Self {
            front: Vec::new(),
            back: Vec::new(),
            dpad_mode: false,
            dpad_at: (0.0, 0.0),
            last: (0.0, 0.0),
            suppressed: false,
        }
    }

    /// The draw's stops for this frame, into the BACK map.
    pub fn fill(&mut self, stops: Vec<Stop<K>>) {
        self.back = stops;
    }

    /// A frame presented: the back map is what the panel shows.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.front, &mut self.back);
        self.back.clear();
    }

    /// The map a click resolves against (the last PRESENTED frame's).
    pub fn front(&self) -> &[Stop<K>] {
        &self.front
    }

    /// The topmost stop whose visible part contains the point.
    pub fn top_at(&self, x: f32, y: f32) -> Option<&Stop<K>> {
        self.front
            .iter()
            .rev()
            .find(|s| s.rect.intersect(s.clip).contains(x, y))
    }

    /// A D-pad press: hover is suppressed until the pointer travels.
    pub fn note_dpad(&mut self) {
        self.dpad_mode = true;
        self.dpad_at = self.last;
    }

    /// Resolve only the current input owner's stops. Covered entries may still be drawn, but
    /// neither their controls nor a stale presented map may impersonate the active surface.
    pub fn resolve(&mut self, owner: Option<EntryId>, kind: PointerKind, x: f32, y: f32, focused: Option<FocusKey<K>>) -> Resolution<K> {
        self.last = (x, y);
        if self.dpad_mode {
            let (dx, dy) = (x - self.dpad_at.0, y - self.dpad_at.1);
            if (dx * dx + dy * dy).sqrt() >= DPAD_TRAVEL_PX {
                self.dpad_mode = false;
            }
        }
        let hit = self.front.iter().rev().find(|s| owner == Some(s.key.entry)
            && s.rect.intersect(s.clip).contains(x, y)).copied();
        let mut r = Resolution {
            hit: hit.map(|s| s.key),
            focus: None,
            activate: None,
            miss: false,
        };
        if self.suppressed {
            return r;
        }
        match kind {
            PointerKind::Move => {
                if self.dpad_mode {
                    return r;
                }
                if let Some(s) = hit {
                    r.focus = match s.hover {
                        Hover::Focus => Some(s.key),
                        Hover::Ignore => None,
                        Hover::OnlyIfFocused => (focused == Some(s.key)).then_some(s.key),
                    };
                }
            }
            PointerKind::Click => match hit {
                Some(s) => {
                    r.activate = Some((s.key, s.activate));
                    if s.hover != Hover::Ignore {
                        r.focus = Some(s.key);
                    }
                }
                None => r.miss = true,
            },
            PointerKind::Drag => {
                // a drag drives its control (the scrubber) with no hover and no click
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::EntryId;

    #[test]
    fn a_foreign_entry_cannot_capture_the_active_owners_hit_or_suppress_a_miss() {
        let mut m = HitMap::new();
        let own = stop(1, Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL);
        let mut covered = stop(2, own.rect, Rect::FULL);
        covered.key.entry = EntryId(2);
        m.fill(vec![own, covered]);
        m.swap();
        let hit = m.resolve(Some(EntryId(1)), PointerKind::Click, 50.0, 50.0, None);
        assert_eq!(hit.hit, Some(own.key));
        assert_eq!(hit.focus, Some(own.key));
        let outside = m.resolve(Some(EntryId(3)), PointerKind::Click, 50.0, 50.0, None);
        assert!(outside.hit.is_none() && outside.focus.is_none() && outside.activate.is_none());
        assert!(outside.miss);
    }
    use crate::ui::Rect;

    fn stop(elem: u32, r: Rect, clip: Rect) -> Stop<u32> {
        Stop {
            key: FocusKey {
                entry: EntryId(1),
                elem,
            },
            rect: r,
            rest_rect: r,
            clip,
            hover: Hover::Focus,
            activate: Activate::Press,
        }
    }

    /// §7.6: a stop under a clip is hit only in its visible part.
    #[test]
    fn a_click_on_a_clipped_stop_hits_only_its_visible_part() {
        let mut m = HitMap::new();
        m.fill(vec![stop(1, Rect::new(0.0, 0.0, 400.0, 400.0), Rect::new(0.0, 0.0, 200.0, 400.0))]);
        m.swap();
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Click, 100.0, 100.0, None).hit.map(|k| k.elem), Some(1));
        let r = m.resolve(Some(EntryId(1)), PointerKind::Click, 300.0, 100.0, None);
        assert_eq!(r.hit, None, "inside the rect, outside the clip");
        assert!(r.miss);
    }

    /// The map is double-buffered: the back map registered by a frame that did not present is
    /// not what a click resolves against.
    #[test]
    fn a_click_on_an_idle_frame_resolves_against_the_last_presented_map() {
        let mut m = HitMap::new();
        m.fill(vec![stop(1, Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL)]);
        m.swap(); // presented
        m.fill(vec![stop(2, Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL)]); // drawn, not presented
        assert_eq!(m.top_at(50.0, 50.0).map(|s| s.key.elem), Some(1));
        m.swap();
        assert_eq!(m.top_at(50.0, 50.0).map(|s| s.key.elem), Some(2));
    }

    /// §7.6: a focused tile drawn through `scaled(pop)` registers its POPPED rect — a click in
    /// the margin the pop grew into hits it (`DrawFrame::stop` folds the cascade; this is the
    /// map's half).
    #[test]
    fn a_scaled_focused_tile_registers_its_popped_rect() {
        use crate::ui::Painter;
        let mut m = HitMap::new();
        let rest = Rect::new(100.0, 100.0, 100.0, 100.0);
        let p = Painter::root().scaled(1.2);
        let (popped, _, _) = p.to_screen(rest);
        let mut s = stop(1, popped, Rect::FULL);
        s.rest_rect = rest;
        m.fill(vec![s]);
        m.swap();
        assert!(popped.w > rest.w);
        let edge = popped.x + popped.w - 2.0;
        assert!(edge > rest.x + rest.w, "the pop grew past the rest rect");
        assert_eq!(m.top_at(edge, popped.y + 10.0).map(|s| s.key.elem), Some(1));
        assert_eq!(m.top_at(edge, popped.y + 10.0).map(|s| s.rest_rect.w), Some(100.0), "…and the rest rect is what an Opener anchors to");
    }

    /// Painter order is z: the later stop wins where two overlap.
    #[test]
    fn the_later_stop_is_on_top() {
        let mut m = HitMap::new();
        m.fill(vec![
            stop(1, Rect::new(0.0, 0.0, 300.0, 300.0), Rect::FULL),
            stop(2, Rect::new(100.0, 100.0, 100.0, 100.0), Rect::FULL),
        ]);
        m.swap();
        assert_eq!(m.top_at(150.0, 150.0).map(|s| s.key.elem), Some(2));
        assert_eq!(m.top_at(20.0, 20.0).map(|s| s.key.elem), Some(1));
    }

    /// `dpad_mode`: after a D-pad press, hover parks nothing until the pointer travels 120 px.
    #[test]
    fn hover_is_suppressed_after_a_dpad_press_until_the_pointer_travels() {
        let mut m = HitMap::new();
        m.fill(vec![stop(1, Rect::new(0.0, 0.0, 1000.0, 1000.0), Rect::FULL)]);
        m.swap();
        m.resolve(Some(EntryId(1)), PointerKind::Move, 500.0, 500.0, None);
        m.note_dpad();
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 510.0, 500.0, None).focus, None);
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 600.0, 500.0, None).focus, None, "100 px: not yet");
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 630.0, 500.0, None).focus.map(|k| k.elem), Some(1), "130 px: hover is back");
        assert!(!m.dpad_mode);
    }

    /// The fly-away guard: `Hover::OnlyIfFocused` parks focus only on the already-focused stop;
    /// `Hover::Ignore` never; a `Direct` click activates without a press.
    #[test]
    fn the_stops_hover_and_activate_policies_are_honoured() {
        let mut m = HitMap::new();
        let mut a = stop(1, Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL);
        a.hover = Hover::OnlyIfFocused;
        let mut b = stop(2, Rect::new(200.0, 0.0, 100.0, 100.0), Rect::FULL);
        b.hover = Hover::Ignore;
        b.activate = Activate::Direct;
        m.fill(vec![a, b]);
        m.swap();
        let focused = Some(FocusKey { entry: EntryId(1), elem: 1 });
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 50.0, 50.0, None).focus, None);
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 50.0, 50.0, focused).focus, focused);
        assert_eq!(m.resolve(Some(EntryId(1)), PointerKind::Move, 250.0, 50.0, None).focus, None);
        let r = m.resolve(Some(EntryId(1)), PointerKind::Click, 250.0, 50.0, None);
        assert_eq!(r.activate.map(|(k, a)| (k.elem, a)), Some((2, Activate::Direct)));
        assert_eq!(r.focus, None, "a click on an Ignore stop parks nothing");
        // a suppressed map (a fading surface) answers nothing
        m.suppressed = true;
        let r = m.resolve(Some(EntryId(1)), PointerKind::Click, 50.0, 50.0, None);
        assert!(r.activate.is_none() && !r.miss);
    }
}
