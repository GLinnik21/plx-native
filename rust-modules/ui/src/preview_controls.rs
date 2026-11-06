//! Full-image preview chrome: one shared CircleButton and TabStrip, one engine-owned row.
//! Focus never chooses a scene. Activation is handled by the owner.
use crate::screen::{AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step};
use crate::widgets::{self, CircleButton, ControlGround, SelMark, StripLay, TabGround, TabStrip};
use crate::{consts::SAFE, icons::Icon, theme, Env, Painter, Rect, View};
use plx_machine::machine::{EntryId, FocusKey, GroupId, Measure};

pub struct PreviewControls {
    lays: Vec<StripLay>,
    back: Rect,
}

impl PreviewControls {
    pub fn new(labels: impl Iterator<Item = String>, measure: &dyn Measure) -> Self {
        let mut back = CircleButton::new(std::ptr::null()).frame;
        back.x = SAFE.x;
        back.y = SAFE.y;
        let mut lays = widgets::strip_layout_measured(
            labels,
            0.0,
            theme::size::BODY,
            widgets::STRIP_GAP,
            measure,
        );
        if let (Some(first), Some(last)) = (lays.first(), lays.last()) {
            let left = widgets::strip_pill_rect(first, back.y, back.h).x;
            let right = widgets::strip_pill_rect(last, back.y, back.h);
            let width = right.x + right.w - left;
            let shift =
                (SAFE.x + (SAFE.w - width) * 0.5).max(back.x + back.w + theme::space::XL) - left;
            for lay in &mut lays {
                lay.x += shift;
            }
        }
        Self { lays, back }
    }

    pub fn len(&self) -> usize {
        self.lays.len() + 1
    }
    pub fn contains(&self, elem: u32) -> bool {
        (elem as usize) < self.len()
    }
    pub fn group(&self, group: GroupId) -> GroupSpec {
        GroupSpec {
            id: group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: Rect::new(SAFE.x, SAFE.y, SAFE.w, self.back.h),
            len: self.len(),
            elem: ElemKind::Control,
        }
    }

    pub fn place(&self, elem: u32) -> Option<Placed> {
        let rect = if elem == 0 {
            self.back
        } else {
            widgets::strip_pill_rect(self.lays.get(elem as usize - 1)?, self.back.y, self.back.h)
        };
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: SAFE,
            index: Some(elem),
        })
    }

    pub fn neighbour(&self, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        let stops = (0..self.len() as u32).collect::<Vec<_>>();
        crate::page_regions::row_neighbour(key.entry, &stops, key, dir)
    }

    pub fn reconcile(&self, want: FocusKey<u32>, selected: usize) -> FocusKey<u32> {
        if self.contains(want.elem) {
            want
        } else {
            self.seat(want.entry, selected)
        }
    }

    pub fn seat(&self, entry: EntryId, selected: usize) -> FocusKey<u32> {
        FocusKey {
            entry,
            elem: if selected < self.lays.len() {
                selected as u32 + 1
            } else {
                0
            },
        }
    }

    pub fn update(&self, strip: &mut TabStrip, selected: usize, focused: Option<u32>, dt: f32) {
        strip.update(
            selected as i32,
            focused
                .and_then(|e| e.checked_sub(1))
                .map_or(-1, |e| e as i32),
            |i| widgets::strip_span(&self.lays, i, self.back.h),
            SelMark::Travels,
            dt,
        );
    }

    pub fn draw(&self, p: Painter, strip: &TabStrip, focused: Option<u32>) {
        CircleButton::new(std::ptr::null())
            .icon(Icon::ChevronLeft)
            .frame(self.back)
            .ground(ControlGround::Unkeyed)
            .focused(focused == Some(0))
            .draw(&Env::inert(), p);
        widgets::draw_strip(
            p,
            strip,
            &self.lays,
            self.back.y,
            self.back.h,
            0.0,
            TabGround::Bare {
                pop: 1.0,
                focus: true,
            },
        );
    }
}
