//! Cast-and-crew shelf geometry and Person navigation.

use plx_data::metadata::Detail;
use plx_plex::plex::ServerId;
use plx_ui::cards::{self as ui_cards, RowStyle};
use plx_ui::marquee;
use plx_machine::machine::{GroupId, Measure};
use plx_ui::widgets::Art;
use plx_ui::{theme, Painter, Rect};

thread_local! {
    /// The focused headshot's marquee clock — its name AND role are one block on one clock and one
    /// cycle ([`marquee::Block`]). One tile holds focus app-wide, so one clock (a poster shelf's
    /// label block has its own in `card_row`, a menu row's in `table`).
    static LABEL_CLOCK: marquee::Clock = const { marquee::Clock::new() };
}

pub const CAST_ELEM_RANGE_START: u32 = 3 * super::SECTION_BLOCK;
pub const CAST_ELEM_RANGE_END: u32 = 4 * super::SECTION_BLOCK;
pub const CAST_GROUP: GroupId = GroupId(4);
/// Heading cap top to card top — the SHARED shelf pitch, stated as the sum rather than as the 60
/// it has always been, so the three detail shelves move together (see [`super::related::LABEL_H`]).
pub const LABEL_H: f32 = plx_ui::consts::TITLE_DY + plx_ui::consts::CARD_DY;
pub(super) const SLOT: f32 = 230.0;
const NAME_GAP: f32 = theme::space::MD + theme::space::XS;
/// Lines of room the shelf reserves under the name. A credit label draws ONE role line now (see
/// [`card_row::draw_credit_label`]); the second line's room is kept deliberately — everything below
/// the shelf is laid out from [`block_h`], so tightening it is a layout change of its own, not part
/// of making a label one shape.
const ROLE_LINES_RESERVED: f32 = 2.0;
// The name, the role's reserved room and the largest focus drop are reserved by the shelf's layout
// owner.
const UNDER_H: f32 = NAME_GAP + theme::size::LABEL as f32 + theme::space::XS
    + ui_cards::CREDIT_ROLE_LEADING * ROLE_LINES_RESERVED
    + RowStyle::CAST.h * (RowStyle::CAST.focus_scale - 1.0) * 0.5;
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

/// The cast row's under-band is FIXED, and it is the one detail shelf that may not take the shared
/// collapse: it draws a name and a role under EVERY headshot, focused or not, so the room is
/// occupied on every frame. Related and Extras draw only the focused tile's label, which is what
/// lets them give it back ([`super::related::block_h`]). Measured on the panel first: collapsed,
/// the cast names printed straight through the Extras heading.
pub fn block_h() -> f32 {
    LABEL_H + RowStyle::CAST.h + UNDER_H.max(ui_cards::UNDER_LABEL_H + FOCUS_POP)
}

/// The shelf's heading, `lift` being the row's live label lift (`Shelf::heading_lift`). A shelf
/// that does not hold focus releases the names' marquee clock, so the next headshot to take it
/// starts from its rest beat.
pub fn draw_heading(p: Painter, top: f32, lift: f32, focused: bool) {
    p.text(
        plx_platform::i18n::msg::browse_detail_cast_c().as_ptr(),
        plx_ui::consts::MARGIN_X,
        top - lift,
        theme::size::HEADLINE,
        theme::TEXT_HEADING,
        0,
        1,
    );
    if !focused {
        LABEL_CLOCK.with(|c| c.release());
    }
}

/// Credit `i`'s headshot.
pub fn art(d: &Detail, i: usize) -> Art<'_> {
    Art::Person {
        sid: d.sid.raw(),
        key: d.credit(i).map_or("", |c| c.thumb.as_str()),
        res: (300, 300),
    }
}

/// Credit `i`'s name and role under its headshot, centred on `cx`. `pop` is the tile's focus pop
/// (no press): the focused label drops by its descent.
pub fn draw_label(
    p: Painter,
    d: &Detail,
    i: usize,
    cx: f32,
    row_y: f32,
    focused: bool,
    pop: f32,
    measure: &dyn Measure,
) {
    let Some(c) = d.credit(i) else { return };
    label(
        p,
        &c.tag,
        d.credit_role(i).unwrap_or_default(),
        cx,
        row_y,
        focused,
        if focused { pop_drop(pop) } else { 0.0 },
        measure,
    );
}

fn pop_drop(scale: f32) -> f32 {
    (RowStyle::CAST.h * (scale - 1.0) * 0.5).max(0.0)
}

/// Each person's own text slot, centred under the headshot: its left edge and width (the budget),
/// and the cap top of the name line. The label is PART of its tile: it scrolls past either panel
/// edge with it, at the slot's full width, and is never narrowed or re-elided against the safe
/// frame — which is what printed early ellipses on the names of tiles sliding in or out at both
/// edges. `h` spans the name line and the one role line under it.
fn label_frame(cx: f32, row_y: f32, drop: f32, measure: &dyn Measure) -> Rect {
    let budget = SLOT - theme::space::SM;
    let top = row_y + RowStyle::CAST.h + NAME_GAP + drop;
    let h = measure.cap_h(theme::size::LABEL) + theme::space::XS + ui_cards::CREDIT_ROLE_LEADING;
    Rect::new(cx - budget * 0.5, top, budget, h)
}

/// A headshot's label — the shared [`card_row::draw_credit_label`]: a name line and a role line in
/// every state, an ellipsis unfocused, a lockstep glide focused. The drop is the focus pop's
/// descent, so the focused label clears the grown headshot.
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
    let at = label_frame(cx, row_y, drop, measure);
    LABEL_CLOCK.with(|clock| ui_cards::draw_credit_label(p, clock, at, name, role, focused, measure));
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

    /// Issue 13: a cast label is part of its tile. A headshot scrolling out past either panel edge
    /// carries its name and role with it at the slot's full width — no clamp to the safe frame,
    /// which narrowed and re-elided the names of tiles near both edges. `label_frame` takes no
    /// painter: nothing about a label's frame depends on where the shelf has scrolled to.
    #[test]
    fn a_cast_label_moves_off_screen_with_its_tile_and_keeps_its_own_width() {
        let measure = LabelMeasure;
        let budget = SLOT - theme::space::SM;
        let scr_w = plx_ui::consts::SCR_W;
        for cx in [-40.0, 60.0, scr_w - 60.0, scr_w + 40.0, 2300.0 + scr_w - 60.0] {
            let at = label_frame(cx, 100.0, pop_drop(RowStyle::CAST.focus_scale), &measure);
            assert_eq!(at.x, cx - budget * 0.5, "the label rides its tile at {cx}");
            assert_eq!(at.w, budget, "the label is never re-truncated at {cx}");
            assert!(at.y + at.h <= 100.0 + RowStyle::CAST.h + UNDER_H,
                "the name and role lines fit in the space the next shelf reserves");
        }
    }

    /// A combined crew caption ("Director, Writer", longer in es / be) is ONE role line like every
    /// other, at both safe edges: elided at rest, and complete — it glides — when focused.
    #[test]
    fn a_combined_crew_caption_is_one_role_line_resting_and_glides_whole_when_focused() {
        let _serial = plx_base::testlock::serial();
        let safe = plx_ui::consts::SAFE;
        for preference in [plx_platform::i18n::Preference::En, plx_platform::i18n::Preference::Es, plx_platform::i18n::Preference::Be] {
            let locale = plx_platform::i18n::LocaleContext::resolve(preference, None, None, None, None);
            let caption = plx_platform::i18n::msg::browse_crew_director_writer_in(&locale);
            for center in [safe.x + RowStyle::CAST.w * 0.5,
                safe.x + safe.w - RowStyle::CAST.w * 0.5] {
                let at = label_frame(center, 100.0, 0.0, &LabelMeasure);
                restart_clocks();
                plx_machine::idle::frame_begin(1.0 / 60.0);
                let log = |focused| plx_ui::draw_census::capture(|| {
                    label(Painter::recording(), "Ana", caption, center, 100.0, focused, 0.0, &LabelMeasure);
                });
                let role_runs = |focused| log(focused).into_iter()
                    .filter(|(tag, r)| *tag == 100 && r.y > at.y + 10.0).count();
                assert_eq!(role_runs(false), 1, "{preference:?}: one elided role line");
                let whole = LabelMeasure.width_str(caption, theme::size::CAPTION, false) > at.w;
                assert_eq!(role_runs(true), if whole { 2 } else { 1 },
                    "{preference:?}: focused it is whole — a plain run, or a gliding run and follower");
            }
        }
    }

    /// The text runs the recording painter is handed by one cast label, split into the name's and
    /// the role's by where they fall (`label_frame` stacks the role line below the name line).
    fn runs(name: &str, role: &str, focused: bool) -> (Vec<Rect>, Vec<Rect>) {
        let measure = LabelMeasure;
        let at = label_frame(500.0, 100.0, 0.0, &measure);
        let split = at.y + (measure.cap_h(theme::size::LABEL) + theme::space::XS) * 0.5;
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

    /// The role is ONE line. Focused and too long for the budget it glides (a run and its
    /// follower) and reports damage while it moves; one that fits, or any unfocused one, is a
    /// single still run.
    #[test]
    fn a_focused_role_that_overflows_marquees_one_that_fits_does_not() {
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
        assert_eq!(runs("Ana", "Director", true).1.len(), 1, "a fitting role is one still run");
        assert_eq!(runs("Ana", LONG_ROLE, false).1.len(), 1, "an unfocused role elides in one line");
    }

    /// **A headshot's label has one shape.** Unfocused or focused, short role or one far too long,
    /// the name is one line and the role is one line, on the same two baselines — a role that
    /// wrapped to two lines at rest and became one clipped line on focus made the block change
    /// height and jump when focus moved (the owner's "looks ugly").
    #[test]
    fn a_cast_label_is_one_name_line_and_one_role_line_focused_or_not() {
        let _serial = plx_base::testlock::serial();
        plx_machine::idle::frame_begin(1.0 / 60.0);
        let ys = |runs: &[Rect]| -> Vec<i32> {
            let mut v: Vec<i32> = runs.iter().map(|r| r.y.round() as i32).collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        restart_clocks();
        let (n, r) = runs("Ana", "Director", false);
        let (name_y, role_y) = (ys(&n), ys(&r));
        assert_eq!((name_y.len(), role_y.len()), (1, 1));
        for (name, role) in [
            ("Ana", "Director"),
            ("Sergio Hasselbaink", "Barley, the lumberjack"),
            ("Alexandra Wolkowicz-Harrington Smythe", LONG_ROLE),
        ] {
            for focused in [false, true] {
                restart_clocks();
                let (n, r) = runs(name, role, focused);
                assert_eq!(ys(&n), name_y, "{name:?} focused={focused}: one name line {n:?}");
                assert_eq!(ys(&r), role_y, "{role:?} focused={focused}: one role line {r:?}");
            }
        }
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
        assert!(under >= ui_cards::UNDER_LABEL_H + pop_drop(RowStyle::CAST.focus_scale));
        assert!(under > ui_cards::LABEL_BAND_COLLAPSED);
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
