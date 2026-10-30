//! **I5, `SAFE`** (`docs/hud-plugins.md`, "Invariants", I5; "Menus and other HUD popovers").
//!
//! Every interactive drawn and hit rect, and every text, image and icon, lies inside `SAFE`; a strip's
//! cards may run past `SAFE`'s side edges while the focused card is revealed inside it; scrims and `rect`
//! grounds may bleed; in every state that draws the bar the scrub-preview card's top stays inside `SAFE`.
//!
//! The contract every check module keeps: ONE entry, `pub(super) fn check(cx: &LangCtx<'_>, out: &mut
//! Vec<Finding>)`, called once per language after the package compiled; it walks every state (a Skip state
//! under both pills) and pushes a finding for each violation, built with `Finding::new(&codes::..)` and
//! `.with(ctx)` where `ctx` starts from `lint::ctx_for` and adds its `rects` (`lint::named`). The pointer
//! is the node's (`CompiledState::origin(op)` / `control_pointer(control)`). Pure: no IO, no globals.
//!
//! The rects are read the way I4 reads them (`lint_zorder::surfaces`), so the two invariants see one
//! geometry. The test is `plx_ui::consts::inside_safe`, the one test the layout audit asks of a rect.
//!
//! - **`HUD-E1101` safe-interactive.** A control, the scrub bar (its track AND its grab band), Up Next's
//!   buttons, the Skip pill or the chapter cards leave `SAFE`. The cards are the exception on the sides:
//!   a strip is a scrolling row, its cards run past `SAFE`'s left and right (E1103 asks for the focused
//!   one), so only their vertical extent is tested.
//! - **`HUD-E1102` safe-content.** A text, image, icon or fact box, Up Next's still and caption, or a
//!   strip card's content leaves `SAFE` (a card's content only vertically, as above), and so does a rule
//!   (a hairline is neither a scrim nor a `rect` ground). Scrims and `rect` grounds bleed by design and
//!   are not tested. A strip card's art is the strip's own target.
//! - **`HUD-E1103` safe-reveal.** The focused card of a strip (its art with every overlay and below op)
//!   is wider than the strip's viewport inside `SAFE`, so no scroll position reveals it whole; or, in a
//!   state that draws the bar, the scrub-preview card ([`crate::scrub_preview::card_rect`] over the
//!   track, at its widest) starts above `SAFE`'s top.

use plx_ui::consts::{inside_safe, EDGE_TOL, MARGIN_X, MARGIN_Y, SAFE};
use plx_ui::Rect;

use super::compile::CompiledState;
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};
use super::lint::{ctx_for, named, LangCtx};
use super::lint_zorder::{dedupe, surfaces, views, Content, Kind, StripFacts, Target, TargetKind};
use super::{codes, Context, Finding};
use crate::scrub_preview::{card_rect, CARD_W_CAP};

/// Whether `r` lies inside `SAFE` between its top and bottom edges, wherever it lies sideways: a strip
/// card's test.
pub(super) fn inside_safe_vertically(r: Rect) -> bool {
    inside_safe(Rect::new(SAFE.x, r.y, SAFE.w, r.h))
}

/// The rects of `target` that leave `SAFE`, named. A strip's cards are tested vertically only.
pub(super) fn interactive_outside(target: &Target) -> Vec<(String, Rect)> {
    let inside = |r: Rect| match target.kind {
        TargetKind::Cards => inside_safe_vertically(r),
        _ => inside_safe(r),
    };
    let mut out = Vec::new();
    if !inside(target.drawn) {
        out.push((target.name.clone(), target.drawn));
    }
    if target.hit != target.drawn && !inside(target.hit) {
        out.push((target.hit_name.clone(), target.hit));
    }
    out
}

/// Whether the decor `c` is where `SAFE` wants it: a text, image, icon, rule or fact box inside, a strip
/// card's content inside vertically. Only a `rect` ground bleeds (I5: "otherwise only scrims and `rect`
/// grounds bleed", and the scrim is no decor here); a card's art is its strip's target.
pub(super) fn content_inside(c: &Content) -> bool {
    match (c.kind, &c.card) {
        (Kind::Ground, _) => true,
        (_, Some(card)) if card.face => true,
        (_, Some(_)) => inside_safe_vertically(c.rect),
        (_, None) => inside_safe(c.rect),
    }
}

/// Whether a card of the extent `card` can be revealed whole inside both `viewport` and `SAFE`: it is no
/// wider than their common stretch.
pub(super) fn revealable(card: Rect, viewport: Rect) -> bool {
    let reach = viewport.intersect(Rect::new(SAFE.x, viewport.y, SAFE.w, viewport.h));
    card.w <= reach.w + EDGE_TOL
}

/// Where the scrub-preview card of the bar `track` starts at its widest, the one number `SAFE` bounds.
pub(super) fn preview_card(track: Rect) -> Rect {
    card_rect(track.x + track.w * 0.5, track.x, track.w, track.y, CARD_W_CAP)
}

/// How far above the track the scrub-preview card's top stands (its height and its gap to the bar), read off
/// the card's own placement so a hint never keeps a copy of it.
fn preview_rise() -> f32 {
    -preview_card(Rect::new(0.0, 0.0, 0.0, 0.0)).y
}

/// What a hint calls `SAFE`'s margins, from the constants that define the frame.
fn margins() -> String {
    format!("{MARGIN_X} px at the sides, {MARGIN_Y} px above and below")
}

/// Whether the scrub-preview card over `track` starts at or below `SAFE`'s top.
pub(super) fn preview_inside(track: Rect) -> bool {
    preview_card(track).y >= SAFE.y - EDGE_TOL
}

/// The extent of the focused card of `strip` against its viewport, as a finding's rects.
fn reveal_rects(strip: &StripFacts) -> Vec<(String, Rect)> {
    vec![("card".to_owned(), strip.card), ("viewport".to_owned(), strip.viewport), ("SAFE".to_owned(), SAFE)]
}

/// Whether `state` draws the bar, and its track.
fn bar_of(state: &CompiledState) -> Option<Rect> {
    state.playbar.map(|bar| bar.track)
}

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let from = out.len();
    for view in views(cx) {
        let s = surfaces(cx, &view);
        let at = |rects: Vec<(String, Rect)>| Context {
            rects: rects.into_iter().map(|(name, rect)| named(&name, rect)).collect(),
            ..ctx_for(cx, &view.state)
        };
        for target in &s.targets {
            let outside = interactive_outside(target);
            if outside.is_empty() {
                continue;
            }
            let names: Vec<String> = outside.iter().map(|(name, _)| format!("`{name}`")).collect();
            let mut rects = outside;
            rects.push(("SAFE".to_owned(), SAFE));
            out.push(
                Finding::new(
                    &codes::E1101,
                    target.pointer.clone(),
                    format!("{} leaves the safe area", names.join(" and ")),
                    format!("Move it or shrink it so every interactive rect lies inside SAFE ({}).", margins()),
                )
                .with(at(rects)),
            );
        }
        for c in s.content.iter().filter(|c| !content_inside(c)) {
            out.push(
                Finding::new(
                    &codes::E1102,
                    c.pointer.clone(),
                    format!("`{}` leaves the safe area", c.name),
                    format!("Move it or narrow it so the text, image, icon or rule lies inside SAFE ({}).", margins()),
                )
                .with(at(vec![(c.name.clone(), c.rect), ("SAFE".to_owned(), SAFE)])),
            );
        }
        for strip in s.strips.iter().filter(|strip| !revealable(strip.card, strip.viewport)) {
            out.push(
                Finding::new(
                    &codes::E1103,
                    strip.pointer.clone(),
                    "the focused chapter card is wider than the strip inside SAFE, so no scroll position reveals it whole",
                    "Narrow `card_w` (or its overlay and below content), or widen the strip's cell, so one card fits between the strip's ends and SAFE.",
                )
                .with(at(reveal_rects(strip))),
            );
        }
        if let Some(track) = bar_of(view.shown) {
            if !preview_inside(track) {
                let pointer = s
                    .targets
                    .iter()
                    .find(|t| t.kind == TargetKind::Scrub)
                    .map_or_else(String::new, |t| t.pointer.clone());
                out.push(
                    Finding::new(
                        &codes::E1103,
                        pointer,
                        "the scrub-preview card, floated above the bar, starts above the safe area",
                        format!(
                            "Lower the bar in this state, or give the bar's band more room above: the preview card stands {} px over the track and has to stay inside SAFE.",
                            preview_rise()
                        ),
                    )
                    .with(at(vec![("scrub preview".to_owned(), preview_card(track)), ("SAFE".to_owned(), SAFE)])),
                );
            }
        }
    }
    dedupe(out, from);
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[super::lint::testing::InvalidCase] = &[
    // Classic's scrub grab band with 540 px below the track (the schema's most): it reaches y 1430, past
    // SAFE's bottom edge (1026). The track and the controls are untouched.
    InvalidCase {
        name: "safe-interactive: the grab band leaves SAFE",
        code: "HUD-E1101",
        pointer: "/layout/block/1/cells/0/playbar",
        base: Base::Classic,
        edits: &[(r#""thickness": 8,"#, r#""thickness": 8, "grab": { "above": 72, "below": 540 },"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The reference's title text 1900 px wide from x 136: its box ends at 2036, past SAFE's right edge (1824).
    // No interactive rect shares the title band, so nothing else is wrong.
    InvalidCase {
        name: "safe-content: a text box runs past SAFE",
        code: "HUD-E1102",
        pointer: "/layout/block/0/cells/0/vstack/cells/1/text",
        base: Base::Reference,
        edits: &[(
            r#""bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill""#,
            r#""bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": 1900"#,
        )],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The reference's divider over the Info page drawn 1900 px long instead of the band's width: from x 96 it
    // ends at 1996, past SAFE's right edge (1824). A rule is neither a scrim nor a `rect` ground, and I5 lets
    // only those two bleed. The band holds nothing else, so nothing else is wrong.
    InvalidCase {
        name: "safe-content: a rule runs past SAFE",
        code: "HUD-E1102",
        pointer: "/layout/block/5/cells/0/rule",
        base: Base::Reference,
        edits: &[(r#""axis": "h", "length": "fill""#, r#""axis": "h", "length": 1900"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The reference's chapter cards 1800 px wide: wider than SAFE (1728) and than the strip they scroll in,
    // so the focused card cannot be revealed inside SAFE.
    InvalidCase {
        name: "safe-reveal: a chapter card wider than SAFE",
        code: "HUD-E1103",
        pointer: "/layout/block/4/cells/0/strip",
        base: Base::Reference,
        edits: &[(r#""card_w": 192,"#, r#""card_w": 1800,"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
    // The reference's Info still 460 px tall (164): with chapters and info open the bar is pushed up to y 260,
    // and the scrub-preview card (216 tall, 26 above the bar) would start at y 18, above SAFE's top (54).
    InvalidCase {
        name: "safe-reveal: the scrub preview rises above SAFE",
        code: "HUD-E1103",
        pointer: "/layout/block/1/cells/0/playbar",
        base: Base::Reference,
        edits: &[(r#""w": 292, "h": 164"#, r#""w": 292, "h": 460"#)],
        langs: &["en", "ru"],
        skew: 0.0,
        also: &[],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;

    use super::super::lint::testing::{apply, assert_case, Base};
    use super::super::lint::{lint, LintLang};
    use super::INVALID;

    /// The codes this module reports.
    const CODES: [&str; 3] = ["HUD-E1101", "HUD-E1102", "HUD-E1103"];

    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(!INVALID.is_empty());
        for case in INVALID {
            assert_case(case);
        }
    }

    #[test]
    fn classic_and_reference_have_no_findings_of_this_module_in_any_shipped_language() {
        let langs = LintLang::shipped();
        assert_eq!(langs.len(), 11);
        for base in [Base::Classic, Base::Reference] {
            let found: Vec<_> = lint(base.text(), &ShippedMeasure, &langs)
                .into_iter()
                .filter(|f| CODES.contains(&f.code))
                .collect();
            assert!(found.is_empty(), "{base:?}: {found:#?}");
        }
    }

    #[test]
    fn a_fault_in_every_state_is_one_finding_not_one_per_state_and_slot() {
        let langs = [LintLang::for_tag("en").expect("shipped")];
        // The reference's title 1900 px wide, in every state that draws the title band.
        let wide = [(
            r#""bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill""#,
            r#""bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": 1900"#,
        )];
        // Classic's grab band 540 px deep, in every state that draws the bar.
        let deep = [(r#""thickness": 8,"#, r#""thickness": 8, "grab": { "above": 72, "below": 540 },"#)];
        for (base, edits, code) in [(Base::Reference, &wide[..], "HUD-E1102"), (Base::Classic, &deep[..], "HUD-E1101")] {
            let text = apply(base, edits);
            let found: Vec<_> = lint(&text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code == code).collect();
            let faults: std::collections::BTreeSet<_> = found.iter().map(|f| (&f.pointer, &f.message)).collect();
            assert!(!found.is_empty(), "{code}: the edit is a fault");
            assert_eq!(found.len(), faults.len(), "{code}: one finding per fault, not per state: {found:#?}");
        }
    }

    // ---- the predicates, on synthetic rects (no package)

    use plx_ui::consts::{MARGIN_X, MARGIN_Y, SAFE};
    use plx_ui::Rect;

    use super::super::lint_zorder::{Card, Content, Kind, Target, TargetKind};
    use super::{
        content_inside, inside_safe_vertically, interactive_outside, margins, preview_card, preview_inside, preview_rise,
        revealable,
    };

    /// A rect inside `SAFE`.
    const INSIDE: Rect = Rect::new(200.0, 200.0, 300.0, 100.0);

    fn target(kind: TargetKind, rect: Rect) -> Target {
        Target::flat(kind, "t", "/t", rect, 0)
    }

    fn content(kind: Kind, rect: Rect, card: Option<Card>) -> Content {
        Content { kind, name: "c".to_owned(), pointer: "/c".to_owned(), rect, card }
    }

    fn card(face: bool) -> Option<Card> {
        Some(Card { strip: "/strip".to_owned(), face })
    }

    #[test]
    fn a_rect_is_inside_safe_to_within_a_hair_of_each_edge() {
        assert!(inside_safe_vertically(Rect::new(-500.0, SAFE.y, 4000.0, SAFE.h)), "any width, exactly SAFE tall");
        assert!(inside_safe_vertically(Rect::new(0.0, SAFE.y - 0.005, 10.0, 10.0)), "a hair above");
        assert!(!inside_safe_vertically(Rect::new(0.0, SAFE.y - 1.0, 10.0, 10.0)), "one pixel above the top");
        assert!(!inside_safe_vertically(Rect::new(0.0, SAFE.y + SAFE.h - 9.0, 10.0, 10.0)), "one pixel below the bottom");
    }

    #[test]
    fn an_interactive_rect_outside_safe_is_named_with_the_rect_that_leaves() {
        assert!(interactive_outside(&target(TargetKind::Control, INSIDE)).is_empty());
        let on_edge = Rect::new(SAFE.x, SAFE.y, SAFE.w, SAFE.h);
        assert!(interactive_outside(&target(TargetKind::Control, on_edge)).is_empty(), "exactly SAFE");
        let past_right = Rect::new(SAFE.x + SAFE.w - 10.0, 200.0, 20.0, 20.0);
        assert_eq!(interactive_outside(&target(TargetKind::Control, past_right)), [("t".to_owned(), past_right)]);
        let past_top = Rect::new(200.0, SAFE.y - 1.0, 20.0, 20.0);
        assert_eq!(interactive_outside(&target(TargetKind::Skip, past_top)), [("t".to_owned(), past_top)]);
    }

    #[test]
    fn the_scrub_bar_is_tested_at_the_track_and_at_the_grab_band() {
        let bar = |track, grab| Target {
            kind: TargetKind::Scrub,
            name: "scrub bar".to_owned(),
            hit_name: "scrub grab".to_owned(),
            pointer: "/bar".to_owned(),
            drawn: track,
            hit: grab,
            z: 0,
            hit_z: i64::MIN,
            standing_in: Vec::new(),
        };
        let track = Rect::new(200.0, 900.0, 1000.0, 8.0);
        assert!(interactive_outside(&bar(track, Rect::new(200.0, 828.0, 1000.0, 120.0))).is_empty());
        let deep = Rect::new(200.0, 828.0, 1000.0, 700.0);
        assert_eq!(interactive_outside(&bar(track, deep)), [("scrub grab".to_owned(), deep)], "only the grab leaves");
        let low = Rect::new(200.0, 1100.0, 1000.0, 8.0);
        let both = interactive_outside(&bar(low, deep));
        assert_eq!(both.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["scrub bar", "scrub grab"]);
    }

    #[test]
    fn strip_cards_may_run_past_the_sides_but_not_the_top_or_bottom() {
        let row = Rect::new(-300.0, 400.0, 2500.0, 180.0);
        assert!(interactive_outside(&target(TargetKind::Cards, row)).is_empty());
        let low = Rect::new(-300.0, SAFE.y + SAFE.h - 20.0, 2500.0, 180.0);
        assert_eq!(interactive_outside(&target(TargetKind::Cards, low)), [("t".to_owned(), low)]);
        assert_eq!(interactive_outside(&target(TargetKind::Control, row)), [("t".to_owned(), row)], "a control may not");
    }

    #[test]
    fn content_is_inside_safe_except_that_grounds_bleed_and_cards_run_sideways() {
        let wide = Rect::new(-300.0, 400.0, 2500.0, 40.0);
        let tall = Rect::new(200.0, SAFE.y - 10.0, 100.0, 40.0);
        for kind in [Kind::Text, Kind::Image, Kind::Icon, Kind::Facts, Kind::Rule] {
            assert!(content_inside(&content(kind, INSIDE, None)), "{kind:?} inside");
            assert!(!content_inside(&content(kind, wide, None)), "{kind:?} past the sides");
            assert!(!content_inside(&content(kind, tall, None)), "{kind:?} above the top");
            assert!(content_inside(&content(kind, wide, card(false))), "{kind:?} of a card runs sideways");
            assert!(!content_inside(&content(kind, tall, card(false))), "{kind:?} of a card is bound vertically");
        }
        assert!(content_inside(&content(Kind::Ground, wide, None)), "a ground bleeds");
        assert!(content_inside(&content(Kind::Image, tall, card(true))), "the card's art is its strip's target");
    }

    #[test]
    fn a_card_is_revealable_when_it_fits_between_the_viewport_and_safe() {
        let viewport = Rect::new(SAFE.x, 400.0, SAFE.w, 200.0);
        assert!(revealable(Rect::new(SAFE.x, 400.0, 192.0, 108.0), viewport));
        assert!(revealable(Rect::new(0.0, 400.0, SAFE.w, 108.0), viewport), "exactly as wide as SAFE");
        assert!(!revealable(Rect::new(0.0, 400.0, SAFE.w + 1.0, 108.0), viewport), "one pixel wider than SAFE");
        // A viewport that bleeds past SAFE reveals only what SAFE shows of it.
        let bleeding = Rect::new(0.0, 400.0, 1920.0, 200.0);
        assert!(!revealable(Rect::new(0.0, 400.0, 1800.0, 108.0), bleeding));
        // A narrow viewport is the limit.
        let narrow = Rect::new(300.0, 400.0, 150.0, 200.0);
        assert!(!revealable(Rect::new(300.0, 400.0, 192.0, 108.0), narrow));
        assert!(!revealable(Rect::new(0.0, 400.0, 10.0, 108.0), Rect::new(0.0, 400.0, 80.0, 200.0)), "a viewport outside SAFE");
    }

    #[test]
    fn the_preview_card_stands_over_the_track_and_its_top_must_clear_safe() {
        let at = |y: f32| Rect::new(SAFE.x, y, SAFE.w, 8.0);
        assert_eq!(preview_card(at(500.0)).y, 500.0 - 242.0);
        assert_eq!(preview_rise(), 242.0, "the rise the hint prints is the card's own: 216 tall, 26 above the bar");
        let said = margins();
        assert!(said.starts_with(&format!("{MARGIN_X} px")) && said.contains(&format!("{MARGIN_Y} px")), "{said}");
        assert_eq!(preview_card(at(500.0)).w, crate::scrub_preview::CARD_W_CAP);
        assert!(preview_inside(at(SAFE.y + 242.0)), "the card's top exactly on SAFE's top");
        assert!(!preview_inside(at(SAFE.y + 241.0)), "one pixel above");
        assert!(preview_inside(at(900.0)));
        // Where the bar sits sideways does not matter, only how high.
        assert_eq!(preview_inside(Rect::new(1500.0, 280.0, 300.0, 8.0)), preview_inside(Rect::new(100.0, 280.0, 300.0, 8.0)));
    }
}
