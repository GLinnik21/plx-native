//! A linked shelf heading: one measured run and focus contract with two presentations. `Entry`
//! preserves the filled 60px Filmography control; `Heading` is a bare card-row heading at rest
//! and becomes that same control face on focus. The component owns no application or Plex type.

use super::card_row;
use super::hstack::{Align, HStack, Item};
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
/// Where the chevron sits in [`LinkedMeasure::items`].
const CHEVRON_AT: usize = 3;

/// The room the chevron takes after the run, read off the face's own stack: its full width less
/// the width without the chevron, for an empty run.
fn chevron_room() -> f32 {
    let empty = LinkedMeasure {
        runs: Vec::new(),
        total: String::new(),
        run_w: 0.0,
        lead_w: 0.0,
        note: None,
        face_w: 0.0,
    };
    let items = empty.items();
    HStack::new(&items, ENTRY_RUN_GAP, Align::Leading(0.0)).width()
        - HStack::new(&items[..CHEVRON_AT], ENTRY_RUN_GAP, Align::Leading(0.0)).width()
}

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
    /// The text run: an `Entry`'s title, dot and count at `ENTRY_RUN_GAP`, or a `Heading`'s one
    /// elided flow. The chevron is not in it.
    run_w: f32,
    /// The run's first item: an `Entry`'s title advance, or a `Heading`'s whole flow (`run_w`).
    lead_w: f32,
    /// An `Entry`'s dot and count advances; `None` when it has no count (and for a `Heading`).
    note: Option<(f32, f32)>,
    face_w: f32,
    /// A bounded `Heading`'s elided runs, kept so the draw reuses the measure's elision. Empty for
    /// every other heading, whose flow draws straight from the strings at no elision cost.
    runs: Vec<Run>,
    /// A `Heading`'s member count as drawn ("" when it has none); the source is `LinkedHeading::count`.
    total: String,
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

impl LinkedMeasure {
    /// **The face's one run**, as `HStack` items at `ENTRY_RUN_GAP`: the title (or a `Heading`'s
    /// whole elided flow), the dot and the count (absent without one), then the chevron by its ink
    /// `CHEVRON_GAP` off the run. `measure` sizes the face from it, `draw` stands the pieces
    /// from it (`LinkedHeading::content_x` is its first span), so the face and what it holds
    /// cannot disagree.
    fn items(&self) -> [Item; 4] {
        let (dot, count) = match self.note {
            Some((d, c)) => (Item::new(d), Item::new(c)),
            None => (Item::absent(), Item::absent()),
        };
        [
            Item::new(self.lead_w),
            dot,
            count,
            Item::glyph(Icon::Chevron, CHEVRON_SIZE).before(CHEVRON_GAP),
        ]
    }
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

    /// A `Heading`'s member count as the text `card_row::heading_parts` flows after the title.
    fn total_text(&self) -> String {
        if self.total == 0 { String::new() } else { self.total.to_string() }
    }

    fn text_room(&self) -> f32 {
        self.max_w - chevron_room()
    }

    pub fn measure(&self, measure: &dyn Measure) -> LinkedMeasure {
        let mut runs = Vec::new();
        let total = if self.presentation == Presentation::Heading { self.total_text() } else { String::new() };
        let (lead_w, note) = match self.presentation {
            Presentation::Entry => {
                let title = measure.width_str(self.title, theme::size::LABEL, true);
                let note = (!self.count.is_empty()).then(|| {
                    (
                        measure.width_str("\u{b7}", theme::size::LABEL, false),
                        measure.width_str(self.count, theme::size::LABEL, false),
                    )
                });
                (title, note)
            }
            Presentation::Heading => {
                let keep = self.max_w.is_finite();
                let flow = card_row::bounded_heading_flow(
                    self.title,
                    &total,
                    self.count,
                    self.text_room(),
                    measure,
                    |s, dx, size, bold, ink| {
                        if keep {
                            runs.push(Run { text: s.to_string(), dx, size, bold, ink });
                        }
                        measure.width_str(s, size, bold != 0)
                    },
                );
                (flow, None)
            }
        };
        let mut m = LinkedMeasure { runs, total, run_w: lead_w, lead_w, note, face_w: 0.0 };
        let items = m.items();
        m.run_w = HStack::new(&items[..CHEVRON_AT], ENTRY_RUN_GAP, Align::Leading(0.0)).width();
        m.face_w = 2.0 * SIDE_PAD + HStack::new(&items, ENTRY_RUN_GAP, Align::Leading(0.0)).width();
        m
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

    /// The text run's leading edge: where the face's stack starts.
    fn content_x(&self, face: Rect, m: &LinkedMeasure) -> f32 {
        let items = m.items();
        HStack::new(&items, ENTRY_RUN_GAP, self.align(face))
            .span(0)
            .map_or(face.x, |(x, _)| x)
    }

    /// How the face's content stands in it, the ONE stack `measure`, `content_x` and `draw` read.
    /// `Entry` centres its run and chevron whole (the Filmography pill); a `Heading` starts one
    /// side-padding rung (scaled with the face) in.
    fn align(&self, face: Rect) -> Align {
        match self.presentation {
            Presentation::Entry => Align::Centre(face.x + face.w * 0.5),
            Presentation::Heading => Align::Leading(face.x + SIDE_PAD * face.h / FACE_H),
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

        let items = m.items();
        let stack = HStack::new(&items, ENTRY_RUN_GAP, self.align(face));
        let at = |i: usize| stack.span(i).map_or(face.x, |(x, _)| x);
        let content_x = self.content_x(face, m);
        match self.presentation {
            Presentation::Entry => {
                let cy = face.cy();
                draw_middle(
                    p,
                    self.title,
                    content_x,
                    cy,
                    theme::size::LABEL,
                    true,
                    if focused {
                        ACCENT_INK
                    } else {
                        theme::TEXT_PRIMARY
                    },
                );
                if m.note.is_some() {
                    draw_middle(
                        p,
                        "\u{b7}",
                        at(1),
                        cy,
                        theme::size::LABEL,
                        false,
                        theme::TEXT_SEPARATOR,
                    );
                    draw_middle(
                        p,
                        self.count,
                        at(2),
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
            }
            Presentation::Heading => {
                let cap_y = y;
                card_row::bounded_heading_flow(
                    self.title,
                    &m.total,
                    self.count,
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
            }
        }
        let chevron_x = stack.placed().find(|s| s.i == CHEVRON_AT).map_or(face.x, |s| s.box_x);
        icons::draw(
            p,
            Icon::Chevron,
            Rect::new(
                chevron_x,
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
        assert_eq!(counted.measure(&measure).total, "12");
        assert!(counted.measure(&measure).run_w > plain.run_w);
        // the count and the source are two notes of the one idiom, each behind its own dot
        let both = LinkedHeading::heading("Saga", "friend").total(12);
        let notes = card_row::heading_parts("Saga", &both.total_text(), "friend", |_, _, _, _, _, _| 1.0);
        assert_eq!(notes, 1.0 + 2.0 * (2.0 * card_row::HEADING_SOURCE_PAD + 2.0));
        assert_eq!(LinkedHeading::heading("Saga", "").total(0).measure(&measure).total, "");
    }

    /// Q4, painted: a linked heading is the one heading + count idiom. The title, the count and the
    /// source are PAINTED through a recording painter and read back run by run, each note behind
    /// its own dot on `HEADING_SOURCE_PAD`; a heading with no count and no source is its title alone.
    #[test]
    fn a_painted_linked_heading_keeps_the_pad_between_title_dot_and_count() {
        let _g = plx_base::testlock::serial();
        let measure = widgets::LegacyMeasure;
        let paint = |heading: LinkedHeading| {
            let m = heading.measure(&measure);
            let mut runs = crate::draw_census::capture_runs(|| {
                heading.draw(Painter::recording(), 72.0, 200.0, 0.0, &m, &measure)
            });
            runs.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
            runs
        };
        let runs = paint(LinkedHeading::heading("Saga", "friend").total(12));
        assert_eq!(
            runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["Saga", "\u{b7}", "12", "\u{b7}", "friend"]
        );
        for pair in runs.windows(2) {
            let gap = pair[1].at.x - (pair[0].at.x + pair[0].at.w);
            assert!((gap - card_row::HEADING_SOURCE_PAD).abs() < 0.5, "{} -> {}: {gap}", pair[0].text, pair[1].text);
        }
        assert_eq!((runs[2].size, runs[2].ink), (theme::size::BODY, theme::TEXT_TERTIARY));
        assert_eq!(paint(LinkedHeading::heading("Saga", "").total(0)).len(), 1, "no count, no source, no dot");
    }

    /// The Filmography `Entry`, painted: title, dot and count stand where the hand-summed pen put
    /// them (`content_x` centred from the measured run, then `+ advance + ENTRY_RUN_GAP` per piece),
    /// so folding the pen into the one `HStack` moved no run.
    #[test]
    fn a_painted_entry_stands_its_title_dot_and_count_where_the_pen_did() {
        let _g = plx_base::testlock::serial();
        let measure = widgets::LegacyMeasure;
        for (count, focus_t) in [("23", 0.0), ("23", 1.0), ("", 0.0)] {
            let entry = LinkedHeading::entry("Filmography", count);
            let m = entry.measure(&measure);
            let mut runs = crate::draw_census::capture_runs(|| {
                entry.draw(Painter::recording(), 400.0, 300.0, focus_t, &m, &measure)
            });
            runs.sort_by(|a, b| a.at.x.total_cmp(&b.at.x));
            let face = entry.face_rect(400.0, 300.0, focus_t, &m);
            let title = measure.width_str("Filmography", theme::size::LABEL, true);
            let dot = measure.width_str("\u{b7}", theme::size::LABEL, false);
            let run_w = if count.is_empty() {
                title
            } else {
                title + 2.0 * ENTRY_RUN_GAP + dot + measure.width_str(count, theme::size::LABEL, false)
            };
            assert_eq!(m.run_w, run_w);
            let x0 = face.x
                + (face.w - run_w - CHEVRON_GAP - CHEVRON_SIZE + CHEVRON_BEARING_L + CHEVRON_BEARING_R)
                    * 0.5;
            let mut want = vec![x0];
            if !count.is_empty() {
                want.push(x0 + title + ENTRY_RUN_GAP);
                want.push(x0 + title + ENTRY_RUN_GAP + dot + ENTRY_RUN_GAP);
            }
            let got: Vec<f32> = runs.iter().map(|r| r.at.x).collect();
            assert_eq!(got.len(), want.len(), "count {count:?}");
            for (g, w) in got.iter().zip(&want) {
                assert!((g - w).abs() < 0.001, "count {count:?} focus {focus_t}: {got:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn chevron_room_is_the_gap_plus_the_chevron_ink_width() {
        let by_hand = CHEVRON_GAP + CHEVRON_SIZE - CHEVRON_BEARING_L - CHEVRON_BEARING_R;
        assert!((chevron_room() - by_hand).abs() < 0.001, "{} vs {by_hand}", chevron_room());
        let h = LinkedHeading::heading("T", "").bounded(500.0);
        assert!((h.text_room() - (500.0 - by_hand)).abs() < 0.001);
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
