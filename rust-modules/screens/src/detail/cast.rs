//! Cast-and-crew shelf geometry and Person navigation.

use std::ffi::CString;

use plx_data::metadata::Detail;
use plx_plex::plex::ServerId;
use plx_ui::card_row::{self, CardRow, RowStyle};
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::marquee;
use plx_machine::machine::{GroupId, Measure};
use plx_ui::text_view::TextView;
use plx_ui::widgets::Art;
use plx_ui::{theme, Painter, Rect};

thread_local! {
    /// The focused headshot's marquee clock — its name AND role are one block on one clock and one
    /// cycle ([`marquee::Block`]). One tile holds focus app-wide, so one clock (a poster shelf's
    /// label block has its own in `card_row`, a menu row's in `table`).
    static LABEL_CLOCK: marquee::Clock = const { marquee::Clock::new() };
}

pub const CAST_ELEM_RANGE_START: u32 = 1152;
pub const CAST_ELEM_RANGE_END: u32 = 1664;
pub const CAST_GROUP: GroupId = GroupId(4);
/// Heading cap top to card top — the SHARED shelf pitch, stated as the sum rather than as the 60
/// it has always been, so the three detail shelves move together (see [`super::related::LABEL_H`]).
pub const LABEL_H: f32 = plx_ui::consts::TITLE_DY + plx_ui::consts::CARD_DY;
const SLOT: f32 = 230.0;
const NAME_GAP: f32 = theme::space::MD + theme::space::XS;
const ROLE_LEADING: f32 = theme::size::CAPTION as f32 + theme::space::XS;
const ROLE_LINES: usize = 2;
// Both caption lines and the largest focus drop are reserved by the shelf's layout owner.
const UNDER_H: f32 = NAME_GAP + theme::size::LABEL as f32 + theme::space::XS
    + ROLE_LEADING * ROLE_LINES as f32 + RowStyle::CAST.h * (RowStyle::CAST.focus_scale - 1.0) * 0.5;
/// How far a focused headshot grows past the row box. The fixed cast shelf reserves this
/// descent with its always-visible caption band, so labels cannot cross the next heading.
pub const FOCUS_POP: f32 = RowStyle::CAST.h * (RowStyle::CAST.focus_scale - 1.0) * 0.5;

pub fn elem(index: usize) -> Option<u32> {
    (index < (CAST_ELEM_RANGE_END - CAST_ELEM_RANGE_START) as usize)
        .then_some(CAST_ELEM_RANGE_START + index as u32)
}

pub fn locate(key: u32) -> Option<usize> {
    (CAST_ELEM_RANGE_START..CAST_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - CAST_ELEM_RANGE_START) as usize)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    OpenPerson {
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    },
}

pub fn action(d: &Detail, key: u32) -> Action {
    let Some(c) = locate(key).and_then(|i| d.credit(i)) else {
        return Action::None;
    };
    let person_key = c.person_key();
    if person_key.is_empty() {
        return Action::None;
    }
    Action::OpenPerson {
        sid: d.sid,
        key: person_key,
        guid: c.tag_key.clone(),
        name: c.tag.clone(),
        thumb: c.thumb.clone(),
    }
}

pub fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    let base = card_row::tile_rect(
        index,
        plx_ui::consts::MARGIN_X,
        SLOT,
        row.scroll_x(),
        top + LABEL_H,
        (RowStyle::CAST.w, RowStyle::CAST.h),
    );
    if at_drawn {
        base.scaled(row.scale(index))
    } else {
        base.scaled(RowStyle::CAST.focus_scale)
    }
}

/// The cast row's under-band is FIXED, and it is the one detail shelf that may not take the shared
/// collapse: it draws a name and a role under EVERY headshot, focused or not, so the room is
/// occupied on every frame. Related and Extras draw only the focused tile's label, which is what
/// lets them give it back ([`super::related::block_h`]). Measured on the panel first: collapsed,
/// the cast names printed straight through the Extras heading.
pub fn block_h() -> f32 {
    LABEL_H + RowStyle::CAST.h + UNDER_H.max(card_row::UNDER_LABEL_H + FOCUS_POP)
}

pub fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    measure: &dyn plx_machine::machine::Measure,
) {
    p.text(
        plx_platform::i18n::msg::browse_detail_cast_c().as_ptr(),
        plx_ui::consts::MARGIN_X,
        top - row.lift(),
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
        0,
        1,
    );
    let row_y = top + LABEL_H;
    if focused.is_none() {
        // focus has left the shelf: the next headshot to take it starts from its rest beat
        LABEL_CLOCK.with(|c| c.release());
    }
    card_row::strip(
        p,
        row,
        d.credits_len(),
        focused.map(|i| i as i32).unwrap_or(-1),
        row_y,
        (RowStyle::CAST.w, RowStyle::CAST.h),
        SLOT,
        &RowStyle::CAST,
        plx_ui::consts::SCR_W,
        |i| {
            d.credit(i)
                .map(|c| Art::Person {
                    sid: d.sid.raw(),
                    key: c.thumb.as_str(),
                    res: (300, 300),
                })
                .unwrap_or(Art::Person {
                    sid: d.sid.raw(),
                    key: "",
                    res: (300, 300),
                })
        },
        |_| None,
        |_| card_row::TileLabel::default(),
        |p, i, x, is_focused| {
            let Some(c) = d.credit(i) else { return };
            label(
                p,
                &c.tag,
                d.credit_role(i).unwrap_or_default(),
                x + RowStyle::CAST.w * 0.5,
                row_y,
                is_focused,
                if is_focused {
                    pop_drop(row.scale(i))
                } else {
                    0.0
                },
                measure,
            );
        },
        measure,
    );
}

fn pop_drop(scale: f32) -> f32 {
    (RowStyle::CAST.h * (scale - 1.0) * 0.5).max(0.0)
}

/// Each person's own text slot, centred under the headshot. The label is PART of its tile: it
/// scrolls past either panel edge with it, at the slot's full width, and is never narrowed or
/// re-elided against the safe frame — which is what printed early ellipses on the names of tiles
/// sliding in or out at both edges.
fn label_frames(cx: f32, row_y: f32, drop: f32, measure: &dyn Measure) -> (Rect, Rect) {
    let budget = SLOT - theme::space::SM;
    let left = cx - budget * 0.5;
    let top = row_y + RowStyle::CAST.h + NAME_GAP + drop;
    let name = Rect::new(left, top, budget, theme::size::LABEL as f32 + theme::space::XS);
    let role = Rect::new(left, top + measure.cap_h(theme::size::LABEL) + theme::space::XS,
        budget, ROLE_LEADING * ROLE_LINES as f32);
    (name, role)
}

fn name_caption(name: &str, width: f32, focused: bool, measure: &dyn Measure) -> String {
    plx_gfx::text::elide_by(name, width, false, |text| measure.width_str(text, theme::size::LABEL, focused))
}

fn role_view<'a>(role: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(role, theme::size::CAPTION, theme::TEXT_TERTIARY)
        .with_measure(measure).h(HAlign::Center).leading(ROLE_LEADING)
        .max_lines(ROLE_LINES).break_long_words()
}

/// A focused run's marquee window: its frame, plus air above and below so ascenders and
/// descenders glide inside the clip instead of being sliced by it (the poster title's allowance).
const MARQUEE_AIR: f32 = 6.0;

/// The headshot's name. Unfocused it is elided to the slot; FOCUSED it is never elided — one that
/// does not fit the slot scrolls through it ([`marquee::TITLE`], the poster title's own timing and
/// draw), so a long name can be read in full, exactly as under a poster. A fitting name stays
/// centred and still. `block` is the label's shared glide (`None` unless a line overflows).
fn draw_name(p: Painter, name: &str, frame: Rect, focused: bool, block: Option<&marquee::Block>, measure: &dyn Measure) {
    let full_w = name_run_w(name, frame, focused, measure);
    if let (Some(full_w), Some(block)) = (full_w, block) {
        let Ok(text) = CString::new(name) else { return };
        let label = Label::new(text.as_ptr(), theme::size::LABEL, theme::TEXT_PRIMARY)
            .bold().h(HAlign::Left).v(VAlign::CapTop);
        LABEL_CLOCK.with(|clock| marquee::TITLE.glide_in(
            clock, p, &block.key, full_w, block.cycle_w,
            Rect::new(frame.x, frame.y - MARQUEE_AIR, frame.w, frame.h + 2.0 * MARQUEE_AIR),
            |dx| { label.draw(p, Rect::new(frame.x + dx, frame.y, frame.w, frame.h)); },
        ));
        return;
    }
    if let Ok(name) = CString::new(name_caption(name, frame.w, focused, measure)) {
        let mut label = Label::new(name.as_ptr(), theme::size::LABEL,
            if focused { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY })
            .h(HAlign::Center).v(VAlign::CapTop);
        if focused { label = label.bold(); }
        label.draw(p, frame);
    }
}

/// The focused name's full width when it does NOT fit its slot (the marquee case); `None` for a
/// fitting or an unfocused name.
fn name_run_w(name: &str, frame: Rect, focused: bool, measure: &dyn Measure) -> Option<f32> {
    focused
        .then(|| measure.width_str(name, theme::size::LABEL, true))
        .filter(|w| *w > frame.w)
}

/// The focused role's full one-line width when it still does not fit its [`ROLE_LINES`] wrapped
/// lines (the marquee case); `None` for a role that wraps cleanly or an unfocused one.
fn role_run_w(role: &str, frame: Rect, focused: bool, measure: &dyn Measure) -> Option<f32> {
    (focused && !role.is_empty() && role_view(role, measure).truncates(frame.w))
        .then(|| measure.width_str(role, theme::size::CAPTION, false))
}

/// The character / job line: up to [`ROLE_LINES`] wrapped lines, centred. FOCUSED and still too
/// long for them, it becomes ONE line that scrolls like the name above it — in step with it, on
/// the label's shared `block` — instead of ending in an ellipsis; a role that wraps to fit keeps
/// its lines, so a focus move never reflows a short one.
fn draw_role(p: Painter, role: &str, frame: Rect, focused: bool, block: Option<&marquee::Block>, measure: &dyn Measure) {
    if let (Some(full_w), Some(block)) = (role_run_w(role, frame, focused, measure), block) {
        let Ok(text) = CString::new(role) else { return };
        let label = Label::new(text.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .h(HAlign::Left).v(VAlign::CapTop);
        LABEL_CLOCK.with(|clock| marquee::TITLE.glide_in(
            clock, p, &block.key, full_w, block.cycle_w,
            Rect::new(frame.x, frame.y - MARQUEE_AIR, frame.w, ROLE_LEADING + MARQUEE_AIR),
            |dx| { label.draw(p, Rect::new(frame.x + dx, frame.y, frame.w, 0.0)); },
        ));
        return;
    }
    role_view(role, measure).draw(p, frame);
}

fn label(
    p: Painter,
    name: &str,
    role: &str,
    cx: f32,
    row_y: f32,
    focused: bool,
    drop: f32,
    measure: &dyn Measure,
) {
    let (name_frame, role_frame) = label_frames(cx, row_y, drop, measure);
    if name_frame.w <= 0.0 { return; }
    let block = marquee::Block::of(
        (name, name_run_w(name, name_frame, focused, measure)),
        (role, role_run_w(role, role_frame, focused, measure)),
    );
    if focused && block.is_none() {
        LABEL_CLOCK.with(|c| c.release());
    }
    draw_name(p, name, name_frame, focused, block.as_ref(), measure);
    if !role.is_empty() {
        draw_role(p, role, role_frame, focused, block.as_ref(), measure);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LabelMeasure;
    impl Measure for LabelMeasure {
        fn width(&self, text: &std::ffi::CStr, size: i32, _: bool) -> f32 {
            text.to_string_lossy().chars().count() as f32 * size as f32 * 0.62
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.75 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }

    #[test]
    fn cast_name_elision_uses_the_remaining_width_for_a_partial_surname() {
        let measure = LabelMeasure;
        let center = plx_ui::consts::MARGIN_X + RowStyle::CAST.w * 0.5;
        let (frame, _) = label_frames(center, 100.0, 0.0, &measure);
        let text = name_caption("Алена Сяргеева 6", frame.w, true, &measure);
        assert!(text.starts_with("Алена С"), "single-line elision must not discard the whole surname: {text}");
        assert!(text.ends_with('…'));
        assert!(measure.width_str(&text, theme::size::LABEL, true) <= frame.w);
    }

    /// Issue 13: a cast label is part of its tile. A headshot scrolling out past either panel edge
    /// carries its name and role with it at the slot's full width — no clamp to the safe frame,
    /// which narrowed and re-elided the names of tiles near both edges. `label_frames` takes no
    /// painter: nothing about a label's frame depends on where the shelf has scrolled to.
    #[test]
    fn a_cast_label_moves_off_screen_with_its_tile_and_keeps_its_own_width() {
        let measure = LabelMeasure;
        let budget = SLOT - theme::space::SM;
        let scr_w = plx_ui::consts::SCR_W;
        for cx in [-40.0, 60.0, scr_w - 60.0, scr_w + 40.0, 2300.0 + scr_w - 60.0] {
            let (name, role) = label_frames(cx, 100.0, pop_drop(RowStyle::CAST.focus_scale), &measure);
            for frame in [name, role] {
                assert_eq!(frame.x, cx - budget * 0.5, "the label rides its tile at {cx}");
                assert_eq!(frame.w, budget, "the label is never re-truncated at {cx}");
            }
            assert!(role.y + role.h <= 100.0 + RowStyle::CAST.h + UNDER_H,
                "both caption lines fit in the space the next shelf reserves");
            let long = "Alexandra Wolkowicz-Harrington";
            assert_eq!(name_caption(long, name.w, false, &measure),
                name_caption(long, budget, false, &measure));
        }
    }

    #[test]
    fn combined_crew_captions_fit_two_caption_lines_at_both_safe_edges() {
        let measure = LabelMeasure;
        let safe = plx_ui::consts::SAFE;
        for preference in [plx_platform::i18n::Preference::En, plx_platform::i18n::Preference::Es, plx_platform::i18n::Preference::Be] {
            let locale = plx_platform::i18n::LocaleContext::resolve(preference, None, None, None, None);
            let caption = plx_platform::i18n::msg::browse_crew_director_writer_in(&locale);
            for center in [safe.x + RowStyle::CAST.w * 0.5,
                safe.x + safe.w - RowStyle::CAST.w * 0.5] {
                let (_, frame) = label_frames(center, 100.0, 0.0, &measure);
                let view = role_view(caption, &measure);
                assert!(!view.truncates(frame.w), "combined {preference:?} job must remain complete");
                assert!(view.measure_h(frame.w) <= frame.h);
                assert!(view.measure_h(frame.w) > ROLE_LEADING, "exercise the old one-line truncation");
            }
        }
    }

    /// The text runs the recording painter is handed by one cast label, split into the name's and
    /// the role's by which frame they fall in (`label_frames` stacks the role below the name).
    fn runs(name: &str, role: &str, focused: bool) -> (Vec<Rect>, Vec<Rect>) {
        let measure = LabelMeasure;
        let (name_frame, role_frame) = label_frames(500.0, 100.0, 0.0, &measure);
        let split = (name_frame.y + role_frame.y) * 0.5;
        let log = plx_ui::draw_census::capture(|| {
            label(Painter::recording(), name, role, 500.0, 100.0, focused, 0.0, &measure);
        });
        let (mut n, mut r) = (Vec::new(), Vec::new());
        for (_, rect) in log.into_iter().filter(|(tag, _)| *tag == 100) {
            if rect.y < split { n.push(rect) } else { r.push(rect) }
        }
        (n, r)
    }

    const LONG_NAME: &str = "Alexandra Wolkowicz-Harrington Smythe";
    const LONG_ROLE: &str = "Dr. Alexandra Wolkowicz-Harrington Smythe, chief consulting \
        physician to the royal household and director of the institute for applied diagnostics";

    fn restart_clocks() {
        LABEL_CLOCK.with(|c| c.release());
    }

    /// NIT 6: a poster's focused title scrolls when it does not fit; the focused headshot's name
    /// did not — it was elided. It rests, then glides (drawn as a run and its follower) and reports
    /// damage on every frame it moves, exactly like `card_row`'s title.
    #[test]
    fn a_focused_cast_name_that_does_not_fit_marquees() {
        let _serial = plx_base::testlock::serial();
        restart_clocks();
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = plx_machine::idle::take_local_damage();
        let (rest, _) = runs(LONG_NAME, "", true);
        assert_eq!(rest.len(), 2, "a run and its follower: {rest:?}");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "resting is not damage");
        let x0 = rest.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        plx_machine::idle::frame_begin((marquee::TITLE.hold_ms + 500.0) / 1000.0);
        let (gliding, _) = runs(LONG_NAME, "", true);
        let x1 = gliding.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        assert!(x1 < x0, "the name has moved left: {x0} -> {x1}");
        assert!(plx_machine::idle::take_local_damage() > 0, "a gliding frame reports");
    }

    /// And nothing else moves: a fitting focused name and any UNFOCUSED name draw one still run.
    #[test]
    fn a_fitting_or_unfocused_cast_name_stays_one_still_run() {
        let _serial = plx_base::testlock::serial();
        restart_clocks();
        for _ in 0..3 {
            plx_machine::idle::frame_begin(1.5);
            let _ = plx_machine::idle::take_local_damage();
            assert_eq!(runs("Ana", "", true).0.len(), 1, "a fitting focused name");
            assert_eq!(runs(LONG_NAME, "", false).0.len(), 1, "an unfocused name elides");
            assert_eq!(plx_machine::idle::take_local_damage(), 0);
        }
    }

    /// The role is wrapped to two lines; only when the FOCUSED one still does not fit in them does
    /// it become a one-line marquee. A role that wraps cleanly keeps its two lines.
    #[test]
    fn a_focused_role_that_overflows_its_two_lines_marquees_one_that_fits_does_not() {
        let _serial = plx_base::testlock::serial();
        restart_clocks();
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let _ = plx_machine::idle::take_local_damage();
        let (_, rest) = runs("Ana", LONG_ROLE, true);
        assert_eq!(rest.len(), 2, "a run and its follower: {rest:?}");
        let y = rest[0].y;
        assert!(rest.iter().all(|r| r.y == y), "one line, not a wrapped block");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "resting is not damage");
        plx_machine::idle::frame_begin((marquee::TITLE.hold_ms + 500.0) / 1000.0);
        let (_, gliding) = runs("Ana", LONG_ROLE, true);
        let x0 = rest.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let x1 = gliding.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        assert!(x1 < x0, "the role has moved left: {x0} -> {x1}");
        assert!(plx_machine::idle::take_local_damage() > 0);

        restart_clocks();
        let (_, wrapped) = runs("Ana", "Dr. Alexandra Smythe", true);
        assert_eq!(wrapped.len(), 2, "two wrapped lines, as before");
        assert_ne!(wrapped[0].y, wrapped[1].y);
        let (_, unfocused) = runs("Ana", LONG_ROLE, false);
        assert_eq!(unfocused.len(), 2, "an unfocused role still wraps and elides in two lines");
        assert_ne!(unfocused[0].y, unfocused[1].y);
    }

    /// **A headshot's name and role are one block**, like a poster's title and caption: when both
    /// overflow they start, glide and loop together on the longer one's cycle. The shorter name
    /// finishes its glide early and waits at rest — it does not loop under the still-gliding role.
    #[test]
    fn a_focused_name_and_role_that_both_overflow_glide_in_lockstep() {
        let _serial = plx_base::testlock::serial();
        restart_clocks();
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let (name0, role0) = runs(LONG_NAME, LONG_ROLE, true);
        let (name_x, role_x) = (name0[0].x, role0[0].x);
        assert_eq!((name0.len(), role0.len()), (2, 2));
        let measure = LabelMeasure;
        let name_period = marquee::TITLE.period(measure.width_str(LONG_NAME, theme::size::LABEL, true));
        let role_period = marquee::TITLE.period(measure.width_str(LONG_ROLE, theme::size::CAPTION, false));
        assert!(name_period + 3000.0 < role_period, "the fixture needs a much longer role");
        plx_machine::idle::frame_begin((name_period + 3000.0) / 1000.0);
        let (name, role) = runs(LONG_NAME, LONG_ROLE, true);
        assert!(name.iter().any(|r| (r.x - name_x).abs() < 0.5),
            "the name waits at rest for the role instead of looping: {name:?}");
        assert!(role.iter().any(|r| r.x < role_x - 100.0),
            "the role is still gliding: {role:?}");
    }

    #[test]
    fn cast_pop_never_crosses_the_next_slot() {
        assert!(RowStyle::CAST.w * RowStyle::CAST.focus_scale < SLOT);
        assert!(pop_drop(0.9) >= 0.0);
    }

    #[test]
    fn the_cast_pop_is_clearly_visible_and_never_touches_a_neighbour() {
        assert!(RowStyle::CAST.focus_scale > 1.10);
        assert!(RowStyle::CAST.w * RowStyle::CAST.focus_scale < SLOT);
        assert!(SLOT - RowStyle::CAST.w * RowStyle::CAST.focus_scale > 10.0);
    }

    #[test]
    fn cast_pop_drop_tracks_the_live_scale_and_never_goes_negative() {
        assert_eq!(pop_drop(1.0), 0.0);
        assert_eq!(pop_drop(0.95), 0.0);
        assert!(pop_drop(1.05) < pop_drop(RowStyle::CAST.focus_scale));
    }

    /// Cast is the one detail shelf whose band may NOT collapse, so its block reserves the label
    /// room on every frame — plus the focus pop, which falls below the row box and is the part a
    /// fixed `UNDER_H` used to swallow. Measured on the panel first: with the band collapsed, the
    /// always-drawn names printed through the next section's heading.
    #[test]
    fn the_cast_block_covers_its_always_drawn_labels_on_every_frame() {
        let under = block_h() - LABEL_H - RowStyle::CAST.h;
        assert!(under >= card_row::UNDER_LABEL_H + pop_drop(RowStyle::CAST.focus_scale));
        assert!(under > card_row::LABEL_BAND_COLLAPSED);
    }

    #[test]
    fn ok_on_a_crew_tile_opens_that_crew_members_page_not_an_actor_at_the_same_index() {
        fn credit(name: &str, role: &str, id: i64) -> plx_data::metadata::Cast {
            plx_data::metadata::Cast {
                tag: name.into(),
                role: role.into(),
                thumb: String::new(),
                id,
                tag_key: String::new(),
            }
        }
        let detail = Detail {
            sid: ServerId::UNSET,
            cast: vec![credit("Actor", "Role", 1)],
            crew: vec![credit("Writer", "Writer", 2)],
            ..Default::default()
        };
        assert_eq!(
            action(&detail, elem(1).unwrap()),
            Action::OpenPerson {
                sid: ServerId::UNSET,
                key: "2".into(),
                guid: String::new(),
                name: "Writer".into(),
                thumb: String::new(),
            }
        );
    }
}
