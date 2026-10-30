//! **The layout compiler's tests** (`compile.rs`).
//!
//! The headline is `compiled_classic_matches_the_hand_written_layout`: the shipped classic package,
//! compiled, reports the SAME rects as the hand-written layout (`classic::geometry`), field for field and
//! to the exact `f32`, in every compact slot, for a movie and an episode, with and without chapters, in
//! every shipped language and under both measurers (`FixtureMeasure`, whose advances are arithmetic, and
//! `ShippedMeasure`, the real fonts' advances, which is headless here). A mismatch means the compiler or
//! the fixture is wrong, never the hand-written layout. Then every fixture compiles in every state
//! `reach` enumerates, the cost of classic's compact state is pinned, and the section states are held
//! against the panels' own constants.

use plx_base::fontcov::advances::ShippedMeasure;
use plx_data::metadata::{Marker, MarkerKind};
use plx_machine::machine::Measure;
use plx_media::player::text_style::HudBand;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference, SHIPPED};
use plx_ui::consts::{MARGIN_X, SCR_H, SCR_W};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::{theme, Rect};

use super::compile::*;
use super::parse::{self, Package};
use super::{check, Slot};
use crate::player_hud::geometry::HudGeometry;
use crate::player_hud::motion::HudMotion;
use crate::player_hud::{classic, ControlSlot, Controls};
use crate::skip_pill::prompt_for;
use crate::{chapters_panel, info_panel};

const CLASSIC: &str = include_str!("fixtures/classic.plxhud.json");
const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");
const HERO: &str = include_str!("fixtures/hero-center.plxhud.json");
const TABS: &str = include_str!("fixtures/tabs.plxhud.json");
const PALETTE: &str = include_str!("fixtures/palette.plxhud.json");

const FM: FixtureMeasure = FixtureMeasure;

fn package(text: &str) -> Package {
    let parsed = parse::parse(text.as_bytes());
    assert!(parsed.findings.is_empty(), "the fixture must parse clean: {:?}", parsed.findings);
    parsed.package.expect("a package")
}

/// The app's strings in the language the calling thread is set to.
fn labels() -> AppLabels {
    AppLabels {
        tab_info: msg::widgets_player_info().to_owned(),
        tab_chapters: msg::widgets_player_chapters().to_owned(),
        next_episode: msg::widgets_next_episode().to_owned(),
        watch_credits: msg::widgets_next_credits().to_owned(),
        skip_intro: msg::widgets_skip_intro().to_owned(),
        skip_credits: msg::widgets_skip_credits().to_owned(),
        from_beginning: msg::widgets_info_from_beginning().to_owned(),
        go_to_show: msg::widgets_info_go_show().to_owned(),
        go_to_movie: msg::widgets_info_go_movie().to_owned(),
    }
}

fn build(text: &str, measure: &dyn Measure) -> Compiled {
    compile(&package(text), measure, CompileCtx { labels: labels() })
}

fn marker(kind: MarkerKind, final_seg: bool) -> Marker {
    Marker { kind, start_ms: 1_000, end_ms: 2_000, final_seg }
}

const PAIR: Controls = Controls { previous: true, next: true, restart: false };

/// Every field of two geometries that differ, as text; empty when they are equal.
fn diff(got: &HudGeometry, want: &HudGeometry) -> Vec<String> {
    let mut out = Vec::new();
    let mut cmp = |name: &str, g: String, w: String| {
        if g != w {
            out.push(format!("{name}: compiled {g} != hand-written {w}"));
        }
    };
    cmp("grab", format!("{:?}", got.grab), format!("{:?}", want.grab));
    cmp("playbar", format!("{:?}", got.playbar), format!("{:?}", want.playbar));
    cmp("row", format!("{:?}", got.row), format!("{:?}", want.row));
    cmp("row_anchor", format!("{:?}", got.row_anchor), format!("{:?}", want.row_anchor));
    for i in 0..got.items.len() {
        cmp(&format!("items[{i}]"), format!("{:?}", got.items[i]), format!("{:?}", want.items[i]));
    }
    cmp("tab_row", format!("{:?}", got.tab_row), format!("{:?}", want.tab_row));
    cmp("title", format!("{:?}", got.title), format!("{:?}", want.title));
    for i in 0..got.tabs.len() {
        cmp(&format!("tabs[{i}]"), format!("{:?}", got.tabs[i]), format!("{:?}", want.tabs[i]));
    }
    out
}

#[test]
fn compiled_classic_matches_the_hand_written_layout() {
    let measures: [(&str, &dyn Measure); 2] = [("FixtureMeasure", &FM), ("ShippedMeasure", &ShippedMeasure)];
    let mut mismatches = Vec::new();
    let mut compared = 0;
    for language in SHIPPED {
        let _language = language_on_this_thread_for_test(language);
        for (measure_name, measure) in measures {
            let compiled = build(CLASSIC, measure);
            for (episode, controls) in [(false, Controls::NONE), (true, PAIR)] {
                for (chapters, tabs) in [(false, 1), (true, 2)] {
                    let slots = [
                        (ControlSlot::Discs(controls), Slot::Discs, SkipKind::Intro),
                        (ControlSlot::UpNext(marker(MarkerKind::Credits, true)), Slot::UpNext, SkipKind::Intro),
                        (ControlSlot::Skip(prompt_for(marker(MarkerKind::Intro, false))), Slot::Skip, SkipKind::Intro),
                        (ControlSlot::Skip(prompt_for(marker(MarkerKind::Credits, false))), Slot::Skip, SkipKind::Credits),
                    ];
                    for (slot, kind, skip) in slots {
                        let want = classic::geometry(slot, &HudMotion::new(), tabs, measure);
                        let key = StateKey { open: 0, slot: kind, chapters, episode };
                        let got = compiled.geometry(key, skip).expect("the compact state exists");
                        compared += 1;
                        for line in diff(&got, &want) {
                            mismatches.push(format!(
                                "[{language:?} {measure_name} episode={episode} chapters={chapters} {kind:?}/{skip:?}] {line}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(compared >= 2 * 2 * 2 * 4, "the golden must compare something: {compared}");
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

#[test]
fn compiled_classic_title_stops_where_the_hand_written_title_does() {
    let compiled = build(CLASSIC, &FM);
    let title_right = |s: &CompiledState| {
        s.ops.iter().find_map(|op| match op {
            DrawOp::Text { bind, rect, .. } if bind == "title.main" => Some(rect.x + rect.w),
            _ => None,
        })
    };
    for (episode, controls) in [(false, Controls::NONE), (true, PAIR)] {
        for (kind, slot) in [
            (Slot::Discs, ControlSlot::Discs(controls)),
            (Slot::UpNext, ControlSlot::UpNext(marker(MarkerKind::Credits, true))),
        ] {
            let state = compiled.lookup(0, kind, true, episode).unwrap();
            let want = classic::title_right(slot, &HudMotion::new(), &FM);
            assert_eq!(title_right(state), Some(want), "{kind:?} episode={episode}");
        }
        // two pills, two widths: the title yields to the wider one
        let state = compiled.lookup(0, Slot::Skip, true, episode).unwrap();
        let wide = [MarkerKind::Intro, MarkerKind::Credits]
            .map(|k| classic::title_right(ControlSlot::Skip(prompt_for(marker(k, false))), &HudMotion::new(), &FM))
            .into_iter()
            .fold(f32::MAX, f32::min);
        assert_eq!(title_right(state), Some(wide), "skip episode={episode}");
    }
}

#[test]
fn compiled_classic_band_is_the_classic_band() {
    for measure in [&FM as &dyn Measure, &ShippedMeasure] {
        let compiled = build(CLASSIC, measure);
        assert_eq!(compiled.band(), HudBand::CLASSIC);
    }
}

#[test]
fn compiled_classic_has_the_states_reach_counts() {
    for (name, text, states) in [("classic", CLASSIC, 18), ("reference", REFERENCE, 6), ("minimal", MINIMAL, 3)] {
        assert_eq!(build(text, &FM).states().len(), states, "{name}");
        assert_eq!(check(&package(text)).states, states, "{name}");
    }
}

#[test]
fn every_fixture_compiles_in_every_state_inside_the_canvas() {
    let inside = |r: Rect| r.x >= -0.001 && r.y >= -0.001 && r.x + r.w <= SCR_W + 0.001 && r.y + r.h <= SCR_H + 0.001;
    for language in SHIPPED {
        let _language = language_on_this_thread_for_test(language);
        for (name, text) in
            [("classic", CLASSIC), ("reference", REFERENCE), ("minimal", MINIMAL), ("hero-center", HERO), ("tabs", TABS), ("palette", PALETTE)]
        {
            for measure in [&FM as &dyn Measure, &ShippedMeasure] {
                let pkg = package(text);
                let compiled = compile(&pkg, measure, CompileCtx { labels: labels() });
                assert_eq!(compiled.states().len(), check(&pkg).states, "{name}: the state count is reach's");
                for s in compiled.states() {
                    let at = format!("{name} {:?} ({language:?})", s.key);
                    for c in &s.controls {
                        assert!(inside(c.rect), "{at}: control {} at {:?} leaves the canvas", c.id, c.rect);
                        assert!(c.rect.w > 0.0 && c.rect.h > 0.0, "{at}: control {} is empty", c.id);
                    }
                    for (id, r) in &s.bands {
                        assert!(inside(*r), "{at}: band {id} at {r:?} leaves the canvas");
                    }
                    for skip in [SkipKind::Intro, SkipKind::Credits] {
                        let g = s.geometry(skip);
                        assert!(inside(g.grab) && inside(g.playbar), "{at}: the bar leaves the canvas");
                    }
                    assert!(s.cost.draw_ops >= s.controls.len(), "{at}");
                }
            }
        }
    }
}

#[test]
fn state_lookup_ignores_the_content_a_package_does_not_name() {
    let compiled = build(MINIMAL, &FM);
    let a = compiled.lookup(0, Slot::Discs, true, true).unwrap();
    let b = compiled.lookup(0, Slot::Discs, false, false).unwrap();
    assert_eq!(a.key, b.key, "a package that names neither condition has one valuation");
    assert!(compiled.state(StateKey { open: 0, slot: Slot::Discs, chapters: false, episode: false }).is_none());
}

#[test]
fn compiled_classic_info_section_matches_the_info_panel() {
    for (name, measure) in [("fixture", &FM as &dyn Measure), ("shipped", &ShippedMeasure)] {
        let compiled = build(CLASSIC, measure);
        let state = compiled.lookup(OPEN_INFO, Slot::Discs, true, true).unwrap();
        let (card, pad) = info_panel::card_geometry();
        let ground = state.ops.iter().find_map(|op| match op {
            DrawOp::Ground { rect, .. } => Some(*rect),
            _ => None,
        });
        assert_eq!(ground, Some(card), "{name}: the info card's ground");
        // the still: 320 x 180 at the card's padding, centred in its height (`InfoPanelState::draw`)
        let still = state.ops.iter().find_map(|op| match op {
            DrawOp::Image { rect, bind, .. } if bind == "info.still" => Some(*rect),
            _ => None,
        });
        assert_eq!(still, Some(Rect::new(card.x + pad, card.y + (card.h - 180.0) * 0.5, 320.0, 180.0)), "{name}: the still");
        for (i, id) in ["from_beginning", "go_to_detail"].into_iter().enumerate() {
            let got = state.control(id).unwrap_or_else(|| panic!("{name}: no control {id}")).rect;
            assert_eq!(got, info_panel::button_rect(i, measure), "{name}: {id}");
        }
    }
}

#[test]
fn compiled_classic_chapters_section_matches_the_chapters_panel_but_for_its_top() {
    // The hand-written strip is placed by its constants (`CH_TOP`), the compiled one by the chain from the
    // tabs up, in which the timestamp is a LINE with a height: the strip's top is that line higher. The
    // delta is reported here, not forced: a fixture note (PR5b) names the chapters gap a placeholder.
    let compiled = build(CLASSIC, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true).unwrap();
    let (card, pitch, rect) = state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Strip { card, pitch, rect, .. } => Some((*card, *pitch, *rect)),
            _ => None,
        })
        .expect("the chapters strip");
    let want = chapters_panel::card_rect(0, 0.0);
    assert_eq!((card.x, card.w, card.h), (want.x, want.w, want.h), "the card's own shape");
    assert_eq!(rect.x, MARGIN_X);
    assert_eq!(pitch, chapters_panel::card_rect(1, 0.0).x - want.x, "the cards' pitch");
    // `want.y - card.y` is a difference of two sums, so it equals the line's height to the last f32 bit
    // only by luck: compare to a thousandth of a pixel.
    let caption = FM.line_h(theme::size::CAPTION);
    assert!(
        (want.y - card.y - caption).abs() < 0.001,
        "the chapters strip sits one CAPTION line ({caption}) above the hand-written CH_TOP, not {}",
        want.y - card.y
    );
}

/// What an op is, for the cost report's readability.
fn kind_of(op: &DrawOp) -> &'static str {
    match op {
        DrawOp::Scrim { .. } => "scrim",
        DrawOp::Ground { .. } => "ground",
        DrawOp::Text { .. } => "text",
        DrawOp::Image { .. } => "image",
        DrawOp::Icon { .. } => "icon",
        DrawOp::Rule { .. } => "rule",
        DrawOp::Facts { .. } => "facts",
        DrawOp::Playbar { .. } => "playbar",
        DrawOp::Strip { .. } => "strip",
        DrawOp::Control(_) => "control",
        DrawOp::UpNext { .. } => "up_next",
        DrawOp::SkipPill { .. } => "skip_pill",
    }
}

#[test]
fn classic_compact_cost_is_pinned() {
    // Pinned so a later change to what a state costs is VISIBLE in review, not so the numbers are right
    // for a television: the ceilings are PR7's and PR8's to calibrate. English and FixtureMeasure, because
    // the two tab pills' plates are part of the blended area and their width is the label's.
    let _en = language_on_this_thread_for_test(Preference::En);
    let compiled = build(CLASSIC, &FM);
    let state = compiled.lookup(0, Slot::Discs, true, true).unwrap();
    let c = &state.cost;
    assert_eq!(
        (c.draw_ops, c.glyph_runs, c.texture_bytes, c.blended_px),
        (11, 6, 81_920, 953_088),
        "classic's compact cost: the scrim, the two title lines, five discs, the bar and two tabs"
    );
    let top: Vec<(&str, u64)> = c.top.iter().map(|t| (kind_of(&state.ops[t.op]), t.blended_px)).collect();
    assert_eq!(top, [("scrim", 902_400), ("playbar", 13_824), ("control", 9_984)], "the three largest contributors");
    // the report is a sum of its parts, and deterministic
    assert_eq!(c.draw_ops, state.ops.len(), "no strip or Up Next offer in the compact Discs state");
    assert_eq!(build(CLASSIC, &FM).lookup(0, Slot::Discs, true, true).unwrap().cost, *c);
}

#[test]
fn classic_cost_of_the_chapters_strip_counts_its_visible_cards() {
    let compiled = build(CLASSIC, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true).unwrap();
    let (visible, template) = state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Strip { visible, template, .. } => Some((*visible, template.len())),
            _ => None,
        })
        .expect("the strip");
    // 1728 / (288 + 24) = 5.5: five whole cards and a sixth that peeks in
    assert_eq!(visible, 6);
    assert_eq!(template, 3, "the card's art and its two text lines");
    assert_eq!(state.cost.texture_bytes, 6 * 4 * 288 * 162, "one 288 x 162 texture per visible card");
}

#[test]
fn defaults_the_compiler_reads_parse() {
    use super::schema;
    for (node, key) in [("grab", "above"), ("grab", "below"), ("text", "alpha")] {
        let text = schema::field(schema::node(node).expect("node"), key).and_then(|f| f.default);
        let text = text.unwrap_or_else(|| panic!("{node}.{key} has a default"));
        assert!(text.parse::<f32>().is_ok(), "{node}.{key} default `{text}` is a number");
    }
}

#[test]
fn a_live_ink_carries_both_values_and_the_frame_picks() {
    // The palette's title ink flips on `transport.paused`: geometry is compiled once, with `paused` false,
    // so the case is carried for the renderer rather than folded.
    let compiled = build(PALETTE, &FM);
    let state = compiled.lookup(0, Slot::Discs, true, true).unwrap();
    let ink = state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Text { ink, .. } if !ink.cases.is_empty() => Some(ink.clone()),
            _ => None,
        })
        .expect("a text keyed on transport.paused");
    assert_eq!(ink.cases.len(), 1);
    assert_eq!(ink.cases[0].0.name, "transport.paused");
    let playing = ink.at(|name| name != "transport.paused");
    let paused = ink.at(|name| name == "transport.paused");
    assert_eq!(playing, ink.base);
    assert_ne!(playing, paused, "sand playing, ember paused");
    // a negated term holds when its name does not
    let negated = Live { base: 1, cases: vec![(super::parse::CondTerm { negated: true, name: "transport.paused".into() }, 2)] };
    assert_eq!((negated.at(|_| false), negated.at(|_| true)), (2, 1));
}

#[test]
fn a_strip_card_keeps_its_item_conditions_live() {
    let compiled = build(CLASSIC, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true).unwrap();
    let template = state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Strip { template, .. } => Some(template),
            _ => None,
        })
        .unwrap();
    let title_ink = template
        .iter()
        .find_map(|op| match op {
            DrawOp::Text { bind, ink, .. } if bind == "item.title" => Some(ink.clone()),
            _ => None,
        })
        .expect("the card's title line");
    assert_eq!(title_ink.cases.len(), 1);
    assert_eq!(title_ink.cases[0].0.name, "item.focused");
    assert_ne!(title_ink.at(|_| true), title_ink.at(|_| false));
}

#[test]
fn a_state_the_package_cannot_reach_has_no_geometry() {
    let compiled = build(MINIMAL, &FM);
    // minimal lays out no section: the chapters section is not a state of it
    assert!(compiled.geometry(StateKey { open: OPEN_CHAPTERS, slot: Slot::Discs, chapters: true, episode: true }, SkipKind::Intro).is_none());
    assert!(compiled.lookup(OPEN_INFO, Slot::Discs, true, true).is_none());
}

#[test]
fn the_skip_pill_is_end_anchored_at_both_widths() {
    for language in SHIPPED {
        let _language = language_on_this_thread_for_test(language);
        for measure in [&FM as &dyn Measure, &ShippedMeasure] {
            let compiled = build(CLASSIC, measure);
            let state = compiled.lookup(0, Slot::Skip, true, false).unwrap();
            let [intro, credits] = state.standin.skip.expect("both pills");
            assert_eq!(intro.x + intro.w, credits.x + credits.w, "{language:?}: one right edge");
            assert!(intro.w >= state.standin.row.w && credits.w >= state.standin.row.w, "{language:?}: never narrower than the row it replaces");
            assert_eq!(state.geometry(SkipKind::Intro).items[0], Some(intro));
            assert_eq!(state.geometry(SkipKind::Credits).items[0], Some(credits));
        }
    }
}

#[test]
fn a_centred_offer_stays_centred_on_what_it_replaces() {
    // The reference replaces its centre and right segments and centres the offer on their union.
    let compiled = build(REFERENCE, &FM);
    let state = compiled.lookup(0, Slot::UpNext, true, true).unwrap();
    let u = state.standin.up_next.expect("the offer");
    let run_mid = (u.credits.x + u.next.x + u.next.w) * 0.5;
    let union_mid = state.standin.row.x + state.standin.row.w * 0.5;
    assert!((run_mid - union_mid).abs() < 0.01, "the pair's centre {run_mid} vs the replaced segments' {union_mid}");
    assert_eq!(u.credits.y, state.standin.row.y, "on the row it replaces");
    assert!(u.still.is_none() && u.caption.is_none(), "the reference asks for neither");
}

#[test]
fn the_replaced_segments_leave_the_frame_but_not_the_layout() {
    let compiled = build(CLASSIC, &FM);
    let discs = compiled.lookup(0, Slot::Discs, true, true).unwrap();
    let up_next = compiled.lookup(0, Slot::UpNext, true, true).unwrap();
    let ids = |s: &CompiledState| s.controls.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(discs), ["prev", "next", "subs", "audio", "more", "tab_info", "tab_chapters"]);
    assert_eq!(ids(up_next), ["tab_info", "tab_chapters"], "the discs are replaced, the tabs stay");
    assert_eq!(discs.standin, up_next.standin, "the offer lands in the same place in every slot's state");
    assert_eq!(discs.bands, up_next.bands, "the transport does not jump when the offer appears");
}

#[test]
fn a_pill_is_as_wide_as_its_label() {
    let mut wide = labels();
    wide.tab_info = "An extraordinarily long tab label".to_owned();
    let narrow = build(CLASSIC, &FM);
    let widened = compile(&package(CLASSIC), &FM, CompileCtx { labels: wide });
    let rect = |c: &Compiled| c.lookup(0, Slot::Discs, true, true).unwrap().control("tab_info").unwrap().rect;
    assert!(rect(&widened).w > rect(&narrow).w, "a different language is a different compile");
    // and the chapters tab, packed after it, moved right by exactly the difference
    let next = |c: &Compiled| c.lookup(0, Slot::Discs, true, true).unwrap().control("tab_chapters").unwrap().rect.x;
    assert_eq!(next(&widened) - next(&narrow), rect(&widened).w - rect(&narrow).w);
}

#[test]
fn a_block_band_covers_the_whole_extent_not_just_the_controls() {
    // `band: block` (the reference) counts every drawn extent; the title above the bar is higher than any
    // control, so the caption band must clear it.
    let compiled = build(REFERENCE, &FM);
    let state = compiled.lookup(0, Slot::Discs, true, true).unwrap();
    let controls_top = state.controls.iter().map(|c| c.rect.y).fold(f32::MAX, f32::min);
    assert!(compiled.band().controls_top_y < controls_top, "{:?} vs the highest control at {controls_top}", compiled.band());
    // `band: controls` (hero-center) counts the interactive extents only: the bar's track is the highest
    let hero = build(HERO, &FM);
    let state = hero.lookup(0, Slot::Discs, true, true).unwrap();
    assert_eq!(hero.band().controls_top_y, state.playbar.unwrap().track.y);
}

#[test]
fn compiling_twice_is_the_same_value() {
    for measure in [&FM as &dyn Measure, &ShippedMeasure] {
        assert_eq!(build(CLASSIC, measure), build(CLASSIC, measure));
    }
}

#[test]
fn the_cache_key_is_a_plain_value() {
    let key = CompileKey { sha256: [7; 32], app_version: "0.9.0".into(), lang: "en".into() };
    let mut seen = std::collections::HashSet::new();
    assert!(seen.insert(key.clone()));
    assert!(!seen.insert(key.clone()));
    assert!(seen.insert(CompileKey { lang: "be".into(), ..key }), "a language is a different compile");
}
