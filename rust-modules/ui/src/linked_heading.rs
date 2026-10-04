//! A linked shelf heading: one measured run and focus contract with two presentations. `Entry`
//! preserves the filled 60px Filmography control; `Heading` is a bare card-row heading at rest
//! and becomes that same control face on focus. The component owns no application or Plex type.

use super::card_row;
use super::icons::{self, Icon};
use plx_machine::machine::{FocusKey, FocusRead, GroupId, Host, Measure};
use super::screen::{
    Activate, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Link,
    Seat, Stop,
};
use super::theme;
use super::widgets::{self, ControlGround};
use super::{Painter, Rect, ACCENT, ACCENT_INK};

const FACE_H: f32 = 60.0;
const SIDE_PAD: f32 = theme::space::MD;
const CHEVRON_SIZE: f32 = 24.0;
const CHEVRON_GAP: f32 = theme::space::SM;
const ENTRY_RUN_GAP: f32 = theme::space::XS;
const CHEVRON_INK: (f32, f32) = icons::ink_x(Icon::Chevron);
const CHEVRON_BEARING_L: f32 = CHEVRON_SIZE * CHEVRON_INK.0;
const CHEVRON_BEARING_R: f32 = CHEVRON_SIZE * (1.0 - CHEVRON_INK.1);

/// The face's scale at focus progress `focus_t`: 1 at rest, `CTRL_FOCUS_SCALE` focused.
fn focus_scale(focus_t: f32) -> f32 {
    1.0 + (widgets::CTRL_FOCUS_SCALE - 1.0) * focus_t.clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Entry,
    Heading,
}

/// One text run of a BOUNDED `Heading`, already elided: what [`LinkedHeading::draw`] paints
/// without flowing and eliding the heading a second time.
#[derive(Clone, Debug)]
struct Run {
    text: String,
    dx: f32,
    size: std::os::raw::c_int,
    bold: std::os::raw::c_int,
    ink: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct LinkedMeasure {
    run_w: f32,
    face_w: f32,
    /// A bounded `Heading`'s elided runs, kept so the draw reuses the measure's elision. Empty for
    /// every other heading, whose flow draws straight from the strings at no elision cost.
    runs: Vec<Run>,
    /// A `Heading`'s annotation as drawn: its member count, its source, or "N · source".
    note: String,
}

#[derive(Clone, Copy, Debug)]
pub struct LinkedHeading<'a> {
    title: &'a str,
    count: &'a str,
    presentation: Presentation,
    /// The heading's room from its origin, chevron included — `card_row::draw_heading`'s
    /// `max_w`. `INFINITY` (the default) never elides.
    max_w: f32,
    /// How many members the linked collection holds — the "· N" a `Heading` carries after its
    /// title (`Collections.dc.html` D1/E1). 0 draws no count.
    total: usize,
}

impl<'a> LinkedHeading<'a> {
    pub const HEIGHT: f32 = FACE_H;

    pub const fn entry(title: &'a str, count: &'a str) -> Self {
        Self {
            title,
            count,
            presentation: Presentation::Entry,
            max_w: f32::INFINITY,
            total: 0,
        }
    }

    pub const fn heading(title: &'a str, count: &'a str) -> Self {
        Self {
            title,
            count,
            presentation: Presentation::Heading,
            max_w: f32::INFINITY,
            total: 0,
        }
    }

    /// Bound a `Heading` to `max_w` from its origin (the Library's rail sits to the right of its
    /// shelves). The text runs elide through `card_row::bounded_heading_flow`, the same rule an
    /// unlinked shelf heading follows; the chevron always keeps its room.
    pub const fn bounded(mut self, max_w: f32) -> Self {
        self.max_w = max_w;
        self
    }

    /// The linked collection's member count, drawn after a `Heading`'s title and before any
    /// source annotation it was built with.
    pub const fn total(mut self, total: usize) -> Self {
        self.total = total;
        self
    }

    /// A `Heading`'s annotation: the count, the source, or both, count first.
    fn note(&self) -> String {
        match (self.total, self.count.is_empty()) {
            (0, _) => self.count.to_owned(),
            (n, true) => n.to_string(),
            (n, false) => format!("{n} \u{b7} {}", self.count),
        }
    }

    fn text_room(&self) -> f32 {
        self.max_w - (CHEVRON_GAP + CHEVRON_SIZE - CHEVRON_BEARING_L - CHEVRON_BEARING_R)
    }

    pub fn measure(&self, measure: &dyn Measure) -> LinkedMeasure {
        let mut runs = Vec::new();
        let note = if self.presentation == Presentation::Heading { self.note() } else { String::new() };
        let run_w = match self.presentation {
            Presentation::Entry => {
                let mut w = measure.width_str(self.title, theme::size::LABEL, true);
                if !self.count.is_empty() {
                    w += ENTRY_RUN_GAP
                        + measure.width_str("\u{b7}", theme::size::LABEL, false)
                        + ENTRY_RUN_GAP
                        + measure.width_str(self.count, theme::size::LABEL, false);
                }
                w
            }
            Presentation::Heading => {
                let keep = self.max_w.is_finite();
                card_row::bounded_heading_flow(
                    self.title,
                    &note,
                    self.text_room(),
                    measure,
                    |s, dx, size, bold, ink| {
                        if keep {
                            runs.push(Run { text: s.to_string(), dx, size, bold, ink });
                        }
                        measure.width_str(s, size, bold != 0)
                    },
                )
            }
        };
        LinkedMeasure {
            runs,
            note,
            run_w,
            face_w: 2.0 * SIDE_PAD + run_w + CHEVRON_GAP + CHEVRON_SIZE
                - CHEVRON_BEARING_L
                - CHEVRON_BEARING_R,
        }
    }

    /// `Entry` takes the face's top-left. `Heading` takes the title texture's resting top and
    /// centres its face on the cap band inside that texture; the face starts one side-padding rung
    /// to the left. Focus growth is left-anchored in both cases.
    ///
    /// The two grow differently because their content sits differently. `Entry`'s face scales
    /// whole and centres its run (person.rs's Filmography pill). A focused `Heading` is the whole
    /// pill scaled from its left edge (D1b/E1): its PADDING grows with the face while the text,
    /// set at the fixed size rungs, keeps its width — so the air after the chevron stays the side
    /// padding rather than collecting the whole width's 7% as empty face.
    pub fn face_rect(&self, x: f32, y: f32, focus_t: f32, m: &LinkedMeasure) -> Rect {
        let scale = focus_scale(focus_t);
        match self.presentation {
            Presentation::Entry => Rect::new(
                x,
                y - (FACE_H * scale - FACE_H) * 0.5,
                m.face_w * scale,
                FACE_H * scale,
            ),
            Presentation::Heading => {
                let (cap_top, cap_bottom) =
                    plx_gfx::text::text_cap_band(theme::size::HEADLINE, 1);
                let cy = y + (cap_top + cap_bottom) * 0.5;
                Rect::new(
                    x - SIDE_PAD,
                    cy - FACE_H * scale * 0.5,
                    m.face_w + 2.0 * SIDE_PAD * (scale - 1.0),
                    FACE_H * scale,
                )
            }
        }
    }

    fn content_x(&self, face: Rect, m: &LinkedMeasure) -> f32 {
        match self.presentation {
            Presentation::Entry => {
                face.x
                    + (face.w - m.run_w - CHEVRON_GAP - CHEVRON_SIZE
                        + CHEVRON_BEARING_L
                        + CHEVRON_BEARING_R)
                        * 0.5
            }
            Presentation::Heading => face.x + SIDE_PAD * face.h / FACE_H,
        }
    }

    pub fn draw(
        &self,
        p: Painter,
        x: f32,
        y: f32,
        focus_t: f32,
        m: &LinkedMeasure,
        measure: &dyn Measure,
    ) {
        let focused = focus_t > 0.0;
        let face = self.face_rect(x, y, focus_t, m);
        if self.presentation == Presentation::Entry || focused {
            widgets::draw_control_face(
                p,
                face,
                if focused {
                    ACCENT
                } else {
                    theme::CONTROL_IDLE_FILL
                },
                focused,
                ControlGround::Keyed,
            );
        }

        let content_x = self.content_x(face, m);
        let mut rx = content_x;
        match self.presentation {
            Presentation::Entry => {
                let cy = face.cy();
                rx += draw_middle(
                    p,
                    self.title,
                    rx,
                    cy,
                    theme::size::LABEL,
                    true,
                    if focused {
                        ACCENT_INK
                    } else {
                        theme::TEXT_PRIMARY
                    },
                );
                if !self.count.is_empty() {
                    rx += ENTRY_RUN_GAP;
                    rx += draw_middle(
                        p,
                        "\u{b7}",
                        rx,
                        cy,
                        theme::size::LABEL,
                        false,
                        theme::TEXT_SEPARATOR,
                    );
                    rx += ENTRY_RUN_GAP;
                    rx += draw_middle(
                        p,
                        self.count,
                        rx,
                        cy,
                        theme::size::LABEL,
                        false,
                        if focused {
                            theme::ROW_VALUE_INK_ON
                        } else {
                            theme::TEXT_TERTIARY
                        },
                    );
                }
            }
            Presentation::Heading if !m.runs.is_empty() => {
                let ink = |own: [f32; 4]| if focused { ACCENT_INK } else { own };
                for r in &m.runs {
                    draw_cap(p, &r.text, content_x + r.dx, y, r.size, r.bold != 0, ink(r.ink));
                }
                rx += m.run_w;
            }
            Presentation::Heading => {
                let cap_y = y;
                card_row::bounded_heading_flow(
                    self.title,
                    &m.note,
                    self.text_room(),
                    measure,
                    |s, dx, size, bold, ink| {
                        draw_cap(
                            p,
                            s,
                            content_x + dx,
                            cap_y,
                            size,
                            bold != 0,
                            if focused { ACCENT_INK } else { ink },
                        )
                    },
                );
                rx += m.run_w;
            }
        }
        rx += CHEVRON_GAP - CHEVRON_BEARING_L;
        icons::draw(
            p,
            Icon::Chevron,
            Rect::new(
                rx,
                face.cy() - CHEVRON_SIZE * 0.5,
                CHEVRON_SIZE,
                CHEVRON_SIZE,
            ),
            match (self.presentation, focused) {
                (_, true) => ACCENT_INK,
                (Presentation::Entry, false) => theme::TEXT_PRIMARY,
                (Presentation::Heading, false) => theme::TEXT_TERTIARY,
            },
        );
    }

    /// Register after the row's tile stops so an overlapping heading wins pointer z-order.
    pub fn stop<H: Host>(
        &self,
        frame: &mut DrawFrame<'_, '_, H>,
        rect: Rect,
        key: FocusKey<H::Elem>,
    ) {
        frame.stop(
            frame.painter,
            Stop {
                key,
                rect,
                rest_rect: rect,
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Direct,
            },
        );
    }
}

fn draw_middle(
    p: Painter,
    text: &str,
    x: f32,
    cy: f32,
    size: i32,
    bold: bool,
    ink: [f32; 4],
) -> f32 {
    let Ok(text) = std::ffi::CString::new(text) else {
        return 0.0;
    };
    p.text(
        text.as_ptr(),
        x,
        plx_gfx::text::text_vcenter_y(size, i32::from(bold), cy),
        size,
        ink,
        0,
        i32::from(bold),
    )
}

fn draw_cap(
    p: Painter,
    text: &str,
    x: f32,
    cap_y: f32,
    size: i32,
    bold: bool,
    ink: [f32; 4],
) -> f32 {
    let Ok(text) = std::ffi::CString::new(text) else {
        return 0.0;
    };
    p.text(
        text.as_ptr(),
        x,
        plx_gfx::text::baseline_y(size, i32::from(bold), theme::size::HEADLINE, 1, cap_y),
        size,
        ink,
        0,
        i32::from(bold),
    )
}

pub fn group_spec(id: GroupId, rect: Rect) -> GroupSpec {
    GroupSpec {
        id,
        kind: GroupKind::Row { wrap: false },
        seat: Seat::First,
        reachable: AxisMask::VERTICAL,
        edge: [
            EdgeRule::Geometric,
            EdgeRule::Geometric,
            EdgeRule::Stop,
            EdgeRule::Stop,
        ],
        extent: rect,
        len: 1,
        elem: ElemKind::Bare,
    }
}

pub const fn links(heading: GroupId, shelf: GroupId) -> [Link; 2] {
    [
        Link {
            from: shelf,
            dir: Dir::Up,
            to: heading,
        },
        Link {
            from: heading,
            dir: Dir::Down,
            to: shelf,
        },
    ]
}

/// Select the linked-heading seat only for a DOWN entry from `heading_elem`. All other doors keep
/// the shelf's existing policy, including ordinary row-to-row vertical projection.
pub fn shelf_seat<K: Copy + Eq>(
    focus: &FocusRead<K>,
    heading_elem: K,
    shelf: GroupId,
    otherwise: Seat,
) -> Seat {
    if focus.current.is_some_and(|key| key.elem == heading_elem) {
        if focus.remembered(shelf).is_some() {
            Seat::RememberedFirst
        } else {
            Seat::First
        }
    } else {
        otherwise
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::focus::{
        tree::{key, split, Tree},
        FocusEngine, Outcome,
    };
    use plx_machine::machine::{Cx, EntryId, InputOwner, PressRead, Tick};
    use crate::screen::By;

    const ENTRY: EntryId = EntryId(1);
    const OWNER: InputOwner = InputOwner::Entry(ENTRY);
    const HEADING: GroupId = GroupId(1);
    const SHELF: GroupId = GroupId(2);

    fn cx<'a>(
        measure: &'a FixtureMeasure,
        view: &'a FixtureView,
        focus: FocusRead<u32>,
    ) -> Cx<'a, FixtureHost> {
        Cx {
            views: FixtureViews { store: view },
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus,
            owner: OWNER,
        }
    }

    #[test]
    fn heading_geometry_uses_the_drawn_cap_band_and_keeps_the_title_at_96() {
        let measure = FixtureMeasure;
        let heading = LinkedHeading::heading("Title", "12");
        let m = heading.measure(&measure);
        let x = super::super::consts::MARGIN_X;
        let y = 200.0;
        let rest = heading.face_rect(x, y, 0.0, &m);
        let focused = heading.face_rect(x, y, 1.0, &m);
        let (cap_top, cap_bottom) =
            plx_gfx::text::text_cap_band(theme::size::HEADLINE, 1);
        assert_eq!(rest.x, 72.0);
        assert_eq!(focused.x, rest.x);
        assert!(focused.w > rest.w && focused.h > rest.h);
        assert_eq!(rest.cy(), y + (cap_top + cap_bottom) * 0.5);
        assert_eq!(focused.cy(), rest.cy());
        assert_eq!(heading.content_x(rest, &m), 96.0);
    }

    /// **D1b/E1: the focused heading grows as ONE pill from its left edge** — its padding scales
    /// with the face, so the air after the chevron stays the side padding (×1.07) rather than the
    /// whole width's growth piling up as empty face after it.
    #[test]
    fn a_focused_heading_scales_its_padding_not_its_empty_face() {
        let measure = FixtureMeasure;
        let heading = LinkedHeading::heading("Starfall Saga Collection", "12");
        let m = heading.measure(&measure);
        let x = super::super::consts::MARGIN_X;
        let s = widgets::CTRL_FOCUS_SCALE;
        let face = heading.face_rect(x, 200.0, 1.0, &m);
        let content = m.face_w - 2.0 * SIDE_PAD;
        let left = heading.content_x(face, &m) - face.x;
        let right = face.x + face.w - (heading.content_x(face, &m) + content);
        assert_eq!(face.x, x - SIDE_PAD, "grown from the left edge");
        assert!((left - SIDE_PAD * s).abs() < 0.01, "leading air {left}");
        assert!((right - SIDE_PAD * s).abs() < 0.01, "air after the chevron {right}");
    }

    /// `(leading, trailing)` air of a face around its inked content — the text's left edge to the
    /// chevron's ink right edge — at a focus amount, measured from what `draw` paints.
    fn air(h: LinkedHeading<'_>, focus_t: f32) -> (f32, f32) {
        let m = h.measure(&FixtureMeasure);
        let face = h.face_rect(super::super::consts::MARGIN_X, 300.0, focus_t, &m);
        let lead = h.content_x(face, &m) - face.x;
        let chevron_ink_right = h.content_x(face, &m) + m.run_w + CHEVRON_GAP - CHEVRON_BEARING_L
            + CHEVRON_SIZE
            - CHEVRON_BEARING_R;
        (lead, face.x + face.w - chevron_ink_right)
    }

    /// **Issue 16: a focused linked heading's pill is even on its content**, like the Filmography
    /// pill: the same air before the title as after the chevron, at every point of the pop, for
    /// both presentations (the owner saw `SIDE_PAD + 7%` of the face after the chevron against
    /// `SIDE_PAD` before the title).
    #[test]
    fn a_focused_heading_pill_has_equal_air_either_side_like_the_filmography_pill() {
        let heading = LinkedHeading::heading("Collections", "");
        let entry = LinkedHeading::entry("Filmography", "23");
        for h in [heading, entry] {
            let (lead, trail) = air(h, 0.0);
            assert!((lead - SIDE_PAD).abs() < 0.01, "{:?} resting lead {lead}", h.presentation);
            assert!((trail - SIDE_PAD).abs() < 0.01, "{:?} resting trail {trail}", h.presentation);
            for step in 1..=10 {
                let t = step as f32 / 10.0;
                let (lead, trail) = air(h, t);
                assert!(
                    (lead - trail).abs() < 0.01,
                    "{:?} at focus {t}: {lead} before the text, {trail} after the chevron",
                    h.presentation
                );
            }
        }
    }

    /// D1/E1: "Starfall Saga Collection · 12 ›" — the count rides after the title, ahead of any
    /// source annotation, and a zero count draws none.
    #[test]
    fn a_heading_carries_its_collections_count() {
        let measure = FixtureMeasure;
        let plain = LinkedHeading::heading("Saga", "").measure(&measure);
        let counted = LinkedHeading::heading("Saga", "").total(12);
        assert_eq!(counted.measure(&measure).note, "12");
        assert!(counted.measure(&measure).run_w > plain.run_w);
        assert_eq!(LinkedHeading::heading("Saga", "friend").total(12).measure(&measure).note,
            "12 \u{b7} friend");
        assert_eq!(LinkedHeading::heading("Saga", "friend").measure(&measure).note, "friend");
        assert_eq!(LinkedHeading::heading("Saga", "").total(0).measure(&measure).note, "");
    }

    #[test]
    fn entry_measure_and_face_keep_equal_focused_air() {
        let measure = FixtureMeasure;
        let entry = LinkedHeading::entry("Filmography", "23");
        let m = entry.measure(&measure);
        let face = entry.face_rect(400.0, 300.0, 1.0, &m);
        let content = m.run_w + CHEVRON_GAP + CHEVRON_SIZE - CHEVRON_BEARING_L - CHEVRON_BEARING_R;
        let leading = (face.w - content) * 0.5;
        assert!((leading - (face.w - leading - content)).abs() < 0.01);
        assert!(leading > SIDE_PAD);
    }

    #[test]
    fn linked_heading_round_trip_restores_the_last_shelf_tile() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let mut tree = Tree::new(ENTRY);
        let heading = tree.row(HEADING.0, 1, 96.0, 100.0, 260.0, 60.0, 280.0);
        heading.spec = group_spec(HEADING, heading.spec.extent);
        tree.row(SHELF.0, 5, 96.0, 220.0, 200.0, 300.0, 220.0);
        let pair = links(HEADING, SHELF);
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(ENTRY, SHELF.0, 4), Some(SHELF), By::Restore);
        let context = cx(&measure, &view, engine.read(OWNER));
        let Outcome::Moved { to, .. } = engine.move_dir(OWNER, &tree, &pair, Dir::Up, &context)
        else {
            panic!("UP from the last tile must reach the heading");
        };
        assert_eq!(split(to.elem), (HEADING.0, 0));

        let focus = engine.read(OWNER);
        let context = cx(&measure, &view, focus.clone());
        assert!(matches!(
            engine.move_dir(OWNER, &tree, &pair, Dir::Left, &context),
            Outcome::Nothing
        ));
        assert!(matches!(
            engine.move_dir(OWNER, &tree, &pair, Dir::Right, &context),
            Outcome::Nothing
        ));
        tree.groups
            .iter_mut()
            .find(|g| g.spec.id == SHELF)
            .unwrap()
            .spec
            .seat = shelf_seat(&focus, key(ENTRY, HEADING.0, 0).elem, SHELF, Seat::Nearest);
        let context = cx(&measure, &view, focus);
        let Outcome::Moved { to, .. } = engine.move_dir(OWNER, &tree, &pair, Dir::Down, &context)
        else {
            panic!("DOWN from the heading must return to the shelf");
        };
        assert_eq!(split(to.elem), (SHELF.0, 4));
    }

    #[test]
    fn shelf_without_a_heading_keeps_vertical_projection() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let mut tree = Tree::new(ENTRY);
        tree.row(2, 5, 96.0, 220.0, 200.0, 300.0, 220.0);
        tree.row(3, 5, 96.0, 600.0, 200.0, 300.0, 220.0);
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(ENTRY, 3, 4), Some(GroupId(3)), By::Restore);
        let focus = engine.read(OWNER);
        tree.groups[0].spec.seat = shelf_seat(&focus, 1000, GroupId(2), Seat::Nearest);
        let context = cx(&measure, &view, focus);
        let Outcome::Moved { to, .. } = engine.move_dir(OWNER, &tree, &[], Dir::Up, &context)
        else {
            panic!("ordinary shelf UP must still move geometrically");
        };
        assert_eq!(split(to.elem), (2, 4));
    }

    #[test]
    fn heading_without_shelf_memory_falls_back_to_the_first_tile() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let mut tree = Tree::new(ENTRY);
        let heading = tree.row(HEADING.0, 1, 96.0, 100.0, 260.0, 60.0, 280.0);
        heading.spec = group_spec(HEADING, heading.spec.extent);
        tree.row(SHELF.0, 5, 500.0, 220.0, 200.0, 300.0, 220.0);
        let pair = links(HEADING, SHELF);
        let mut engine = FocusEngine::new();
        engine.set(OWNER, key(ENTRY, HEADING.0, 0), Some(HEADING), By::Restore);
        let focus = engine.read(OWNER);
        tree.groups.iter_mut().find(|g| g.spec.id == SHELF).unwrap().spec.seat =
            shelf_seat(&focus, key(ENTRY, HEADING.0, 0).elem, SHELF, Seat::Nearest);
        let context = cx(&measure, &view, focus);
        let Outcome::Moved { to, .. } = engine.move_dir(OWNER, &tree, &pair, Dir::Down, &context)
        else {
            panic!("DOWN from a fresh heading must enter its shelf");
        };
        assert_eq!(split(to.elem), (SHELF.0, 0));
    }
}
