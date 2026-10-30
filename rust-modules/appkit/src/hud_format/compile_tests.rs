//! **The layout compiler's tests** (`compile.rs`).
//!
//! The headline is `compiled_classic_matches_the_hand_written_layout`: the shipped classic package,
//! compiled, reports the SAME rects as the hand-written layout (`classic::geometry`, since deleted) reported, field for field and
//! to the exact `f32`, in every compact slot, for a movie and an episode, with and without chapters, in
//! every shipped language and under both measurers (`FixtureMeasure`, whose advances are arithmetic, and
//! `ShippedMeasure`, the real fonts' advances, which is headless here). A mismatch means the compiler or
//! the fixture is wrong, never the frozen numbers. (The deleted layout's numbers are pinned as literals:
//! see `frozen_classic_geometry`.) Then every fixture compiles in every state
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
use crate::player_hud::geometry::{HudGeometry, RowAnchor};
use crate::player_hud::{classic, Controls};
use crate::{chapters_panel, info_panel};

const CLASSIC: &str = crate::hud_format::CLASSIC_PACKAGE;
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
    cmp("band", format!("{:?}", got.band), format!("{:?}", want.band));
    out
}

/// **The hand-written classic layout's numbers, frozen.** `fixtures/classic-geometry.golden.tsv` is what
/// `classic::geometry(..)` reported for every case the golden below covers (11 languages x 2 measurers x
/// movie/episode x without/with chapters x Discs, Up Next, Skip at each label), written from the
/// HAND-WRITTEN side at 70a7aa66a's layout and re-confirmed at 93fc5091d (the head the freeze commit sits
/// on), before that layout was deleted: one line per case, `key TAB items TAB tabs`, an item or tab as
/// `x,y,w,h` (`f32` `{:?}`, which round-trips exactly) or `-`. The `nl` rows were written from the same
/// hand-written side at edd3d0da6 with the Dutch catalog applied, after the language arrived. The facts that
/// are the same in every case are literals in [`frozen_base`].
const FROZEN_CLASSIC_GEOMETRY: &str = include_str!("fixtures/classic-geometry.golden.tsv");

/// The slot-independent facts of the hand-written layout, the same in all 352 frozen cases.
fn frozen_base() -> HudGeometry {
    HudGeometry {
        grab: Rect::new(96.0, 810.0, 1728.0, 160.0),
        playbar: Rect::new(96.0, 882.0, 1728.0, 8.0),
        row: Rect::new(1592.0, 792.0, 232.0, 64.0),
        row_anchor: RowAnchor { right: 1824.0, top: 792.0, h: 64.0 },
        items: [None; crate::player_hud::geometry::MAX_ROW_ITEMS],
        tab_row: Rect::new(96.0, 949.0, 0.0, 64.0),
        title: Rect::new(96.0, 768.0, 0.0, 0.0),
        tabs: [None; 2],
        band: HudBand { ceil_y: 780.0, controls_top_y: 792.0, lift: 200.0 },
    }
}

fn frozen_rect(s: &str) -> Option<Rect> {
    if s == "-" {
        return None;
    }
    let v: Vec<f32> = s.split(',').map(|n| n.parse().expect("a frozen number")).collect();
    Some(Rect::new(v[0], v[1], v[2], v[3]))
}

/// The frozen hand-written geometry of the case `key` (`{language:?}/{measure}/episode=../chapters=../{slot:?}/{skip:?}`).
fn frozen_classic_geometry(key: &str) -> HudGeometry {
    let line = FROZEN_CLASSIC_GEOMETRY.lines().find(|l| l.split('\t').next() == Some(key)).unwrap_or_else(|| panic!("no frozen case {key}"));
    let mut cols = line.split('\t').skip(1);
    let mut g = frozen_base();
    for (i, item) in cols.next().expect("items").split('|').enumerate() {
        g.items[i] = frozen_rect(item);
    }
    for (i, tab) in cols.next().expect("tabs").split('|').enumerate() {
        g.tabs[i] = frozen_rect(tab);
    }
    g
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
            for episode in [false, true] {
                for chapters in [false, true] {
                    let slots = [
                        (Slot::Discs, SkipKind::Intro),
                        (Slot::UpNext, SkipKind::Intro),
                        (Slot::Skip, SkipKind::Intro),
                        (Slot::Skip, SkipKind::Credits),
                    ];
                    for (kind, skip) in slots {
                        let key = StateKey { open: 0, slot: kind, chapters, episode, paused: false };
                        let got = compiled.geometry(key, skip).expect("the compact state exists");
                        compared += 1;
                        let frozen = frozen_classic_geometry(&format!("{language:?}/{measure_name}/episode={episode}/chapters={chapters}/{kind:?}/{skip:?}"));
                        for line in diff(&got, &frozen) {
                            mismatches.push(format!(
                                "[{language:?} {measure_name} episode={episode} chapters={chapters} {kind:?}/{skip:?}] {line}"
                            ));
                        }
                    }
                }
            }
        }
    }
    // 11 languages x 2 measurers x (movie, episode) x (without, with chapters) x (Discs, Up Next, Skip at
    // each label): a count, so a loop that quietly compares less cannot pass.
    assert_eq!(compared, SHIPPED.len() * 2 * 2 * 2 * 4, "the golden's comparisons");
    assert_eq!(FROZEN_CLASSIC_GEOMETRY.lines().count(), compared, "the frozen file holds one line per case");
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// **The hand-written title's right edge, frozen.** `fixtures/classic-title-right.golden.tsv` is what
/// `classic::title_right(..)` reported for the cases the two tests below cover, written from the
/// HAND-WRITTEN side on 0768cf4a1 (the layout PR5c's golden held at 70a7aa66a, unchanged since), before that function was deleted:
/// one line per case, `key TAB right edge` (`f32` `{:?}`, which round-trips exactly). Keys are
/// `{Slot:?}/episode={..}` (the compact slots, English, `FixtureMeasure`) and
/// `{language:?}/{measure}/episode={..}/{SkipKind:?}` (the skip pill actually shown, 11 languages x 2
/// measurers). The `nl` rows were written from the same hand-written side at edd3d0da6 with the Dutch
/// catalog applied, after the language arrived.
const FROZEN_CLASSIC_TITLE_RIGHT: &str = include_str!("fixtures/classic-title-right.golden.tsv");

fn frozen_title_right(key: &str) -> f32 {
    let line = FROZEN_CLASSIC_TITLE_RIGHT.lines().find(|l| l.split('\t').next() == Some(key)).unwrap_or_else(|| panic!("no frozen title case {key}"));
    line.split('\t').nth(1).expect("a value").parse().expect("a frozen number")
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
    for episode in [false, true] {
        for kind in [Slot::Discs, Slot::UpNext] {
            let state = compiled.lookup(0, kind, true, episode, false).unwrap();
            let frozen = frozen_title_right(&format!("{kind:?}/episode={episode}"));
            assert_eq!(title_right(state), Some(frozen), "{kind:?} episode={episode}");
        }
    }
    assert_eq!(FROZEN_CLASSIC_TITLE_RIGHT.lines().filter(|l| !l.contains("/Intro") && !l.contains("/Credits")).count(), 4, "the frozen file holds the four compact cases");
}

#[test]
fn compiled_classic_title_yields_to_the_skip_pill_actually_shown() {
    // Two labels, two widths: the title's fill stops short of the pill that is on the screen, as the
    // hand-written title does, in every language and under both measurers.
    let title_right = |s: &CompiledState| {
        s.ops.iter().find_map(|op| match op {
            DrawOp::Text { bind, rect, .. } if bind == "title.main" => Some(rect.x + rect.w),
            _ => None,
        })
    };
    let measures: [(&str, &dyn Measure); 2] = [("FixtureMeasure", &FM), ("ShippedMeasure", &ShippedMeasure)];
    let mut differing = 0;
    let mut compared = 0;
    for language in SHIPPED {
        let _language = language_on_this_thread_for_test(language);
        for (measure_name, measure) in measures {
            let compiled = build(CLASSIC, measure);
            for episode in [false, true] {
                let state = compiled.lookup(0, Slot::Skip, true, episode, false).unwrap();
                let mut rights = Vec::new();
                for skip in [SkipKind::Intro, SkipKind::Credits] {
                    let frozen = frozen_title_right(&format!("{language:?}/{measure_name}/episode={episode}/{skip:?}"));
                    let got = title_right(state.skip_variant(skip));
                    assert_eq!(got, Some(frozen), "{language:?} {measure_name} episode={episode} {skip:?}");
                    rights.extend(got);
                    compared += 1;
                }
                differing += usize::from(rights[0] != rights[1]);
            }
        }
    }
    assert_eq!(compared, SHIPPED.len() * 2 * 2 * 2, "the comparisons");
    assert_eq!(FROZEN_CLASSIC_TITLE_RIGHT.lines().count(), compared + 4, "the frozen file holds one line per case");
    assert!(differing > 0, "the two pills must differ somewhere, or this test pins nothing");
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
    // classic: 18 (see `reach_tests`). The reference places `previous` and `next`, so it has the movie form
    // and the episode form, 2 valuations x 6 states = 12 (the compact state under three slots, plus the
    // three non-empty subsets of its two inline sections). Minimal places neither: 1 x 3.
    for (name, text, states) in [("classic", CLASSIC, 18), ("reference", REFERENCE, 12), ("minimal", MINIMAL, 3)] {
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
    let a = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let b = compiled.lookup(0, Slot::Discs, false, false, false).unwrap();
    assert_eq!(a.key, b.key, "a package that names neither condition has one valuation");
    assert!(compiled.state(StateKey { open: 0, slot: Slot::Discs, chapters: false, episode: false, paused: false }).is_none());
}

#[test]
fn compiled_classic_info_section_matches_the_info_panel() {
    for (name, measure) in [("fixture", &FM as &dyn Measure), ("shipped", &ShippedMeasure)] {
        let compiled = build(CLASSIC, measure);
        let state = compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).unwrap();
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
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap();
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
    // Whatever the strip's top, the card is the same column: the name 26 px and the timestamp 34 px
    // under the one before it (`CH_NAME_DROP`, `CH_TIME_DROP`), which `label_ys` stands for.
    let template = state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Strip { template, .. } => Some(template),
            _ => None,
        })
        .expect("the strip");
    let y_of = |bind: &str| {
        template.iter().find_map(|c| match &c.op {
            DrawOp::Text { bind: b, rect, .. } if b == bind => Some(rect.y),
            _ => None,
        })
    };
    let (name_y, time_y) = chapters_panel::label_ys();
    for (bind, hand) in [("item.title", name_y), ("item.start", time_y)] {
        let compiled_drop = y_of(bind).expect(bind) - card.y;
        let hand_drop = hand - want.y;
        assert!((compiled_drop - hand_drop).abs() < 0.001, "{bind}: {compiled_drop} under the card, the hand-written layout has {hand_drop}");
    }
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
    let state = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let c = &state.cost;
    assert_eq!(
        (c.draw_ops, c.glyph_runs, c.texture_bytes, c.blended_px),
        (11, 6, 81_920, 953_088),
        "classic's compact cost: the scrim, the two title lines, five discs, the bar and two tabs"
    );
    let top: Vec<(&str, u64)> = state.top_contributors().iter().map(|t| (kind_of(&state.ops[t.op]), t.blended_px)).collect();
    assert_eq!(top, [("scrim", 902_400), ("playbar", 13_824), ("control", 9_984)], "the three largest contributors");
    // the report is a sum of its parts, and deterministic
    assert_eq!(c.draw_ops, state.ops.len(), "no strip or Up Next offer in the compact Discs state");
    assert_eq!(build(CLASSIC, &FM).lookup(0, Slot::Discs, true, true, false).unwrap().cost, *c);
}

#[test]
fn classic_cost_of_the_chapters_strip_counts_its_visible_cards() {
    let compiled = build(CLASSIC, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap();
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
    let state = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
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
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap();
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
        .find_map(|c| match &c.op {
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
    assert!(compiled.geometry(StateKey { open: OPEN_CHAPTERS, slot: Slot::Discs, chapters: true, episode: true, paused: false }, SkipKind::Intro).is_none());
    assert!(compiled.lookup(OPEN_INFO, Slot::Discs, true, true, false).is_none());
}

#[test]
fn the_skip_pill_is_end_anchored_at_both_widths() {
    for language in SHIPPED {
        let _language = language_on_this_thread_for_test(language);
        for measure in [&FM as &dyn Measure, &ShippedMeasure] {
            let compiled = build(CLASSIC, measure);
            let state = compiled.lookup(0, Slot::Skip, true, false, false).unwrap();
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
    let state = compiled.lookup(0, Slot::UpNext, true, true, false).unwrap();
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
    let discs = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let up_next = compiled.lookup(0, Slot::UpNext, true, true, false).unwrap();
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
    let rect = |c: &Compiled| c.lookup(0, Slot::Discs, true, true, false).unwrap().control("tab_info").unwrap().rect;
    assert!(rect(&widened).w > rect(&narrow).w, "a different language is a different compile");
    // and the chapters tab, packed after it, moved right by exactly the difference
    let next = |c: &Compiled| c.lookup(0, Slot::Discs, true, true, false).unwrap().control("tab_chapters").unwrap().rect.x;
    assert_eq!(next(&widened) - next(&narrow), rect(&widened).w - rect(&narrow).w);
}

#[test]
fn a_block_band_covers_the_whole_extent_not_just_the_controls() {
    // `band: block` (the reference) counts every drawn extent; the title above the bar is higher than any
    // control, so the caption band must clear it.
    let compiled = build(REFERENCE, &FM);
    let state = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let controls_top = state.controls.iter().map(|c| c.rect.y).fold(f32::MAX, f32::min);
    assert!(compiled.band().controls_top_y < controls_top, "{:?} vs the highest control at {controls_top}", compiled.band());
    // `band: controls` (hero-center) counts the interactive extents only, and the bar's extent is the bar
    // AND its knob: the highest thing the bar can draw is the focused knob's glow, a disc of
    // `KNOB_GLOW_R` (17) around the track's middle, so it stands `17 - thickness / 2` above the track
    // whether or not the bar has the focus (the caption must not move with the focus).
    let hero = build(HERO, &FM);
    let state = hero.lookup(0, Slot::Discs, true, true, false).unwrap();
    let track = state.playbar.unwrap().track;
    let knob_top = track.y + track.h * 0.5 - classic::KNOB_GLOW_R;
    assert!(knob_top < track.y, "the knob stands above the track");
    assert_eq!(hero.band().controls_top_y, knob_top, "the track is at {}", track.y);
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

// ---------------------------------------------------------------------------------------------------
// The frame's conditions: `transport.paused` and `item.*`
// ---------------------------------------------------------------------------------------------------

/// `text` with the one occurrence of `from` replaced by `to`.
fn edit(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "`{from}` must occur once in the fixture");
    text.replacen(from, to, 1)
}

/// The strip of `state`: its card template.
fn template_of(state: &CompiledState) -> &[CardOp] {
    state
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Strip { template, .. } => Some(template.as_slice()),
            _ => None,
        })
        .expect("the strip")
}

fn text_y(ops: impl IntoIterator<Item = DrawOp>, bind: &str) -> Option<f32> {
    ops.into_iter().find_map(|op| match op {
        DrawOp::Text { bind: b, rect, .. } if b == bind => Some(rect.y),
        _ => None,
    })
}

#[test]
fn the_references_equaliser_is_drawn_on_the_current_card_only() {
    // `overlay: [{ "when": "item.is_current", .. "icon": eq }]` (reference.plxhud.json): a card's condition
    // is the frame's, so the op is in every card's template and each card asks `item.is_current`.
    let compiled = build(REFERENCE, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap();
    let template = template_of(state);
    let eq = template.iter().find(|c| matches!(&c.op, DrawOp::Icon { asset, .. } if asset == "eq")).expect("the equaliser");
    assert!(eq.present.at(|name| name == "item.is_current"), "the current card draws it");
    assert!(!eq.present.at(|_| false), "no other card does");
    for card_op in template.iter().filter(|c| !matches!(&c.op, DrawOp::Icon { .. })) {
        assert!(card_op.present.base && card_op.present.cases.is_empty(), "{:?} is on every card", card_op.op);
    }
}

#[test]
fn an_item_gated_cell_under_the_art_is_present_per_card_too() {
    // The below-art cells and a `vstack`'s cells gate the same way as a row's: through `when`.
    let text = edit(
        REFERENCE,
        r#"{ "text": { "bind": "item.start""#,
        r#"{ "when": ["item.is_current", "!item.focused"], "text": { "bind": "item.start""#,
    );
    let compiled = build(&text, &FM);
    let state = compiled.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap();
    let start = template_of(state)
        .iter()
        .find(|c| matches!(&c.op, DrawOp::Text { bind, .. } if bind == "item.start"))
        .expect("the timestamp");
    // every term must hold: current AND not focused
    let drawn = |current: bool, focused: bool| start.present.at(|name| (name == "item.is_current" && current) || (name == "item.focused" && focused));
    assert!(drawn(true, false));
    assert!(!drawn(true, true) && !drawn(false, false) && !drawn(false, true));
    // and its space stays reserved: the strip is as tall as the unconditional one
    let plain = build(REFERENCE, &FM);
    let height = |c: &Compiled| {
        c.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap().ops.iter().find_map(|op| match op {
            DrawOp::Strip { rect, .. } => Some(rect.h),
            _ => None,
        })
    };
    assert_eq!(height(&compiled), height(&plain));
}

#[test]
fn a_band_that_asks_for_paused_is_present_exactly_in_the_paused_states() {
    let text = edit(REFERENCE, r#"{ "id": "title", "cells": ["#, r#"{ "id": "title", "when": "transport.paused", "cells": ["#);
    let compiled = build(&text, &FM);
    // paused is an axis of the compile, not of reach's walk (R1 counts it as holding either way)
    assert_eq!(compiled.states().len(), 2 * check(&package(&text)).states, "every state, paused and playing");
    for s in compiled.states() {
        let has_title = s.bands.iter().any(|(id, _)| id == "title");
        assert_eq!(has_title, s.key.paused, "{:?}", s.key);
        // (the info panel binds `title.main` too, so the draw list is read where only the band does)
        if s.key.open == 0 {
            assert_eq!(s.ops.iter().any(|op| matches!(op, DrawOp::Text { bind, .. } if bind == "title.main")), s.key.paused, "{:?}", s.key);
        }
    }
    let playing = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let paused = compiled.lookup(0, Slot::Discs, true, true, true).unwrap();
    assert!(!playing.key.paused && paused.key.paused);
    // a package that does not ask never gets the axis: asking for it finds the one state there is
    let classic = build(CLASSIC, &FM);
    assert_eq!(classic.lookup(0, Slot::Discs, true, true, true).unwrap().key, classic.lookup(0, Slot::Discs, true, true, false).unwrap().key);
}

#[test]
fn a_gap_keyed_on_paused_moves_the_geometry_only_while_paused() {
    // The bar's `gap_before` is the gap up to the band above it, the title: a longer gap pushes the
    // title up and leaves the bar, which hangs from the row, where it is.
    let text = edit(
        REFERENCE,
        r#"{ "id": "bar", "gap_before": "SM","#,
        r#"{ "id": "bar", "gap_before": { "value": "SM", "if": { "transport.paused": 100 } },"#,
    );
    let compiled = build(&text, &FM);
    assert_eq!(compiled.states().len(), 2 * check(&package(&text)).states);
    let playing = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let paused = compiled.lookup(0, Slot::Discs, true, true, true).unwrap();
    let sm = super::schema::space_token("SM").unwrap();
    let title = |s: &CompiledState| text_y(s.ops.clone(), "title.main").unwrap();
    assert_eq!(title(playing) - title(paused), 100.0 - sm, "the title is higher by the longer gap");
    assert_eq!(playing.playbar.unwrap().track, paused.playbar.unwrap().track, "the bar does not move");
    assert_eq!(playing.control("more").unwrap().rect, paused.control("more").unwrap().rect, "nor does the row");
    // the unpaused state is the unedited package's
    let plain = build(REFERENCE, &FM);
    assert_eq!(title(playing), title(plain.lookup(0, Slot::Discs, true, true, false).unwrap()));
}

#[test]
fn classic_names_no_paused_geometry_so_paused_is_no_axis_of_it() {
    for (name, text) in [("classic", CLASSIC), ("reference", REFERENCE), ("palette", PALETTE), ("hero-center", HERO)] {
        let compiled = build(text, &FM);
        assert_eq!(compiled.states().len(), check(&package(text)).states, "{name}: the state count is reach's");
        assert!(compiled.states().iter().all(|s| !s.key.paused), "{name}");
    }
    assert_eq!(build(CLASSIC, &FM).states().len(), 18);
}

#[test]
fn a_number_or_gap_keyed_on_an_item_term_is_a_finding_not_a_silent_default() {
    // A card is laid out once for every card, so a gap cannot depend on which one is focused. The base
    // value is used, and the compile says so.
    let text = edit(
        REFERENCE,
        r#""below_gap": "XS","#,
        r#""below_gap": { "value": "XS", "if": { "item.focused": "SM" } },"#,
    );
    let compiled = build(&text, &FM);
    let [finding] = compiled.findings() else { panic!("one finding, got {:?}", compiled.findings()) };
    assert!(finding.pointer.ends_with("/card/below_gap"), "{}", finding.pointer);
    assert_eq!(finding.term, "item.focused");
    assert!(finding.message.contains("below_gap") && finding.message.contains("item.focused"), "{}", finding.message);
    // the layout is the base's: the same as the unedited reference
    let plain = build(REFERENCE, &FM);
    let state = |c: &Compiled| c.lookup(OPEN_CHAPTERS, Slot::Discs, true, true, false).unwrap().clone();
    assert_eq!(state(&compiled).ops, state(&plain).ops);
    // inks and bold keyed on an item term are fine, they are `Live`: classic and the reference are clean
    assert!(plain.findings().is_empty() && build(CLASSIC, &FM).findings().is_empty());
}

// ---------------------------------------------------------------------------------------------------
// Review fixes: the stand-in's edge, the band, the report's width
// ---------------------------------------------------------------------------------------------------

/// Classic with a second end-aligned segment, `menu`, to the right of the `row` the offer replaces.
fn classic_with_a_menu() -> String {
    edit(
        CLASSIC,
        "        ] } }\n      ] },\n      { \"id\": \"bar\"",
        "        ] } },\n        { \"align\": \"end\", \"valign\": \"bottom\", \"segment\": { \"id\": \"menu\", \"gap\": 0, \"items\": [\n          { \"id\": \"stats\", \"command\": \"toggle_stats\", \"widget\": \"disc\", \"size\": 64, \"rest\": \"film\", \"focus\": \"accent\" }\n        ] } }\n      ] },\n      { \"id\": \"bar\"",
    )
}

#[test]
fn an_end_aligned_offer_lands_on_the_end_of_what_it_replaces_not_the_bands() {
    let compiled = build(&classic_with_a_menu(), &FM);
    let discs = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let menu = discs.control("stats").expect("the menu disc").rect;
    let more = discs.control("more").expect("the last replaced disc").rect;
    assert!(more.x + more.w < menu.x, "the replaced row ends before the menu");
    let edge = more.x + more.w;
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    let up = compiled.lookup(0, Slot::UpNext, true, true, false).unwrap();
    let offer = up.standin.up_next.expect("the offer");
    assert!(near(offer.next.x + offer.next.w, edge), "Up Next ends at {} not {edge}", offer.next.x + offer.next.w);
    assert!(offer.next.x + offer.next.w <= menu.x, "and does not overlap the menu at {}", menu.x);
    let skip = compiled.lookup(0, Slot::Skip, true, true, false).unwrap();
    for pill in skip.standin.skip.expect("both pills") {
        assert!(near(pill.x + pill.w, edge), "the Skip pill ends at {} not {edge}", pill.x + pill.w);
    }
    assert!(near(skip.standin.row.x + skip.standin.row.w, edge), "the row the offers stand for");
    assert!(near(skip.standin.anchor.right, edge) || near(up.standin.anchor.right, edge));
    // without the menu the end of the replaced row IS the band's edge: classic's own numbers
    let plain = build(CLASSIC, &FM);
    let plain_more = plain.lookup(0, Slot::Discs, true, true, false).unwrap().control("more").unwrap().rect;
    assert!(near(plain.lookup(0, Slot::UpNext, true, true, false).unwrap().standin.up_next.unwrap().next.x + plain.lookup(0, Slot::UpNext, true, true, false).unwrap().standin.up_next.unwrap().next.w, plain_more.x + plain_more.w));
}

#[test]
fn the_band_is_one_value_in_the_geometry_and_in_the_compiled_package() {
    for (name, text) in [("classic", CLASSIC), ("reference", REFERENCE), ("minimal", MINIMAL), ("hero-center", HERO), ("tabs", TABS), ("palette", PALETTE)] {
        let compiled = build(text, &FM);
        for s in compiled.states() {
            for skip in [SkipKind::Intro, SkipKind::Credits] {
                let g = s.geometry(skip);
                assert_eq!(g.band, compiled.band(), "{name} {:?}: the geometry's band field", s.key);
                assert_eq!(g.band(), compiled.band(), "{name} {:?}: and its reader", s.key);
            }
        }
    }
    // hero-center is the package where the row's top and the highest extent differ
    let hero = build(HERO, &FM);
    let state = hero.lookup(0, Slot::Discs, true, true, false).unwrap();
    assert_ne!(HudBand::from_controls_top(state.standin.anchor.top), hero.band(), "the row's top is not hero-center's band");
    assert_eq!(state.geometry(SkipKind::Intro).band(), hero.band());
}

#[test]
fn the_references_movie_form_drops_previous_and_next_and_its_episode_form_keeps_them() {
    // The reference places the pair and names `title.is_episode` nowhere: the spec's command table makes the
    // pair absent for a non-episode all the same, so the compile has both forms (reach's valuations).
    let compiled = build(REFERENCE, &FM);
    let commands = |episode| -> Vec<String> {
        let state = compiled.lookup(0, Slot::Discs, true, episode, false).expect("the compact state");
        assert_eq!(state.key.episode, episode, "the form asked for is the form found");
        state.controls.iter().map(|c| c.command.clone()).collect()
    };
    let (movie, episode) = (commands(false), commands(true));
    for command in ["previous", "next"] {
        assert!(!movie.iter().any(|c| c == command), "{command} is absent from a movie: {movie:?}");
        assert!(episode.iter().any(|c| c == command), "{command} is in an episode: {episode:?}");
    }
    assert_eq!((movie.len(), episode.len()), (7, 9), "the movie loses exactly the pair: 9 - 2");
    assert_eq!(compiled.states().iter().filter(|s| s.key.episode).count(), 6);
    assert_eq!(compiled.states().iter().filter(|s| !s.key.episode).count(), 6);
}

#[test]
fn a_row_wider_than_the_report_keeps_every_control() {
    // The reference's row has nine controls and R1 guarantees More. The compiler drops none, and since PR7
    // the report (`HudGeometry::items`, `MAX_ROW_ITEMS` wide) holds them all; before it held the first
    // `BTN_N` = 5, classic's row.
    let compiled = build(REFERENCE, &FM);
    let state = compiled.lookup(0, Slot::Discs, true, true, false).unwrap();
    let ids: Vec<&str> = state.controls.iter().map(|c| c.id.as_str()).collect();
    for id in ["subs", "back", "prev", "play", "next", "fwd", "chapters", "info", "more"] {
        assert!(ids.contains(&id), "{id} is in the compiled controls: {ids:?}");
    }
    let nine: Vec<_> = state.row_ids.iter().flatten().collect();
    assert_eq!(nine.len(), 9, "all nine stay addressable by index in `row_ids`");
    let g = state.geometry(SkipKind::Intro);
    assert_eq!(g.items.iter().flatten().count(), 9, "the widened report holds the whole row");
}

// ---------------------------------------------------------------------------------------------------
// PR7: what the renderer reads besides the rects
// ---------------------------------------------------------------------------------------------------

fn compact(c: &Compiled, slot: Slot) -> &CompiledState {
    c.states().iter().find(|s| s.key.open == 0 && s.key.slot == slot).expect("a compact state")
}

/// **Every op belongs to exactly one band's span**, the spans tile `ops` in draw order, and a Skip
/// state's credits layout is tiled too: the renderer picks the chrome alpha per span.
#[test]
fn every_state_tiles_its_ops_with_band_spans() {
    for package in [CLASSIC, REFERENCE, HERO, TABS, PALETTE, MINIMAL] {
        let c = build(package, &FM);
        for state in c.states() {
            for s in std::iter::once(state).chain(state.skip_alt.as_deref()) {
                let mut next = 0;
                for span in &s.spans {
                    assert_eq!(span.ops.start, next, "{:?}: spans are contiguous", s.key);
                    next = span.ops.end;
                }
                assert_eq!(next, s.ops.len(), "{:?}: spans cover every op", s.key);
            }
        }
    }
}

/// **Classic's chrome split**: while a scrub is live the title block, the row and the tabs fade and the
/// bar stays at full alpha (`draw_hud`'s `pc` and `p`); the scrim never fades. The Up Next offer and the
/// Skip pill take their host band's chrome (`top`: they fade with the row they replace).
#[test]
fn classic_fades_the_title_the_row_and_the_tabs_and_keeps_the_bar_and_the_scrim() {
    let c = build(CLASSIC, &FM);
    for slot in [Slot::Discs, Slot::UpNext, Slot::Skip] {
        let state = compact(&c, slot);
        for span in &state.spans {
            let want = match span.band.as_str() {
                "bar" | "" => Chrome::Steady,
                "top" | "tabs" => Chrome::Fades,
                other => panic!("an unexpected band {other:?}"),
            };
            assert_eq!(span.chrome, want, "{slot:?} band {:?}", span.band);
        }
        assert_eq!(state.spans.first().map(|s| s.band.as_str()), Some(""), "the scrim is the first span");
    }
    let up = compact(&c, Slot::UpNext);
    let last = up.spans.last().expect("a span");
    assert_eq!(last.band, "top");
    assert!(matches!(up.ops[last.ops.start], DrawOp::UpNext { .. }), "the offer is the host band's last span");
    let skip = compact(&c, Slot::Skip);
    assert!(matches!(skip.ops[skip.spans.last().unwrap().ops.start], DrawOp::SkipPill { .. }));
}

/// The faces and knobs a package picks reach the draw list: classic's are the defaults; the reference
/// asks for the permanent knob, end clocks, the busy-only mark, a ring on the play disc and bare discs.
#[test]
fn the_focus_face_and_the_playbar_looks_reach_the_draw_list() {
    let bar = |c: &Compiled| match compact(c, Slot::Discs).ops.iter().find(|o| matches!(o, DrawOp::Playbar { .. })) {
        Some(DrawOp::Playbar { knob, clocks, mark, .. }) => (*knob, *clocks, *mark),
        _ => panic!("no playbar"),
    };
    let classic = build(CLASSIC, &FM);
    assert_eq!(bar(&classic), (KnobMode::Tick, ClockMode::FollowHead, MarkMode::AfterElapsed));
    assert!(compact(&classic, Slot::Discs).controls.iter().all(|c| c.focus == Focus::Accent && c.rest == Rest::Film));
    let reference = build(REFERENCE, &FM);
    assert_eq!(bar(&reference), (KnobMode::Always, ClockMode::Ends, MarkMode::BusyOnly));
    let play = compact(&reference, Slot::Discs).control("play").expect("the play disc").clone();
    assert_eq!((play.focus, play.rest), (Focus::Ring, Rest::Solid));
}

/// **The report holds the whole row.** `HudGeometry::items` was five stops wide (classic's row) and the
/// zip dropped every control past the fifth; the reference places nine in a row (forward, chapters, info
/// and More among them) and each must have the rect it is drawn at, so the focus group and the pointer
/// stops can take it.
#[test]
fn the_geometry_reports_every_item_of_the_references_row() {
    let c = build(REFERENCE, &FM);
    let state = compact(&c, Slot::Discs);
    let placed: Vec<&str> = state.row_ids.iter().flatten().map(String::as_str).collect();
    assert!(placed.len() > crate::player_hud::BTN_N as usize, "the reference's row is wider than classic's: {placed:?}");
    let g = state.geometry(SkipKind::Intro);
    for (i, id) in state.row_ids.iter().enumerate() {
        let want = id.as_deref().and_then(|id| state.control(id)).map(|c| c.rect);
        assert_eq!(g.items[i], want, "row item {i} ({id:?})");
    }
    assert!(state.row_ids.len() <= crate::player_hud::geometry::MAX_ROW_ITEMS);
}

// ---------------------------------------------------------------------------------------------------
// PR7 stage 1d: a control carries its face and the command it performs
// ---------------------------------------------------------------------------------------------------

fn ctl<'a>(state: &'a CompiledState, id: &str) -> &'a ControlRect {
    state.control(id).unwrap_or_else(|| panic!("no control `{id}`"))
}

/// **A control's command comes out typed, with its parameters**, ready for the app's one performer: a
/// literal `seek by` keeps its sign, `open_tracks` its tab, `toggle_section` its section.
#[test]
fn a_control_carries_the_command_with_its_parameters() {
    use crate::player_hud::command::{HudCommand, MorePage, SeekBy, Section, TrackTab};
    use std::num::NonZeroI32;
    let c = build(REFERENCE, &FM);
    let state = c.lookup(0, Slot::Discs, true, true, false).unwrap();
    let secs = |n| HudCommand::Seek(SeekBy::Secs(NonZeroI32::new(n).unwrap()));
    assert_eq!(ctl(state, "play").act, Some(HudCommand::TogglePlay));
    assert_eq!(ctl(state, "back").act, Some(secs(-10)));
    assert_eq!(ctl(state, "fwd").act, Some(secs(30)));
    assert_eq!(ctl(state, "prev").act, Some(HudCommand::Previous));
    assert_eq!(ctl(state, "next").act, Some(HudCommand::Next));
    assert_eq!(ctl(state, "subs").act, Some(HudCommand::OpenTracks(TrackTab::Subtitles)));
    assert_eq!(ctl(state, "more").act, Some(HudCommand::OpenMore(MorePage::Root)));
    assert_eq!(ctl(state, "chapters").act, Some(HudCommand::ToggleSection(Section::Chapters)));
    assert_eq!(ctl(state, "info").act, Some(HudCommand::ToggleSection(Section::Info)));
}

/// **A control carries its face**: a declared asset, a built-in icon, or the per-state map, and classic's
/// controls (which write none) carry none.
#[test]
fn a_control_carries_its_face() {
    use plx_ui::icons::Icon;
    let c = build(REFERENCE, &FM);
    let state = c.lookup(0, Slot::Discs, true, true, false).unwrap();
    let subs = &ctl(state, "subs").face;
    assert_eq!(subs.pick("any"), Some(&FaceRef::Asset("subs-bubble".to_owned())));
    assert!(c.asset("subs-bubble").is_some_and(|svg| svg.starts_with("<svg")), "the asset's source travels with the compile");
    assert_eq!(c.asset("nope"), None);
    assert_eq!(ctl(state, "next").face.pick("any"), Some(&FaceRef::Builtin(Icon::Next)));
    let play = &ctl(state, "play").face;
    assert_eq!(play.pick("playing"), Some(&FaceRef::Builtin(Icon::Pause)));
    assert_eq!(play.pick("paused"), Some(&FaceRef::Builtin(Icon::Play)));
    let prev = &ctl(state, "prev").face;
    assert_eq!(prev.pick("default"), Some(&FaceRef::Builtin(Icon::Previous)));
    assert_eq!(prev.pick("restart"), Some(&FaceRef::Builtin(Icon::Restart)));
    let classic = build(CLASSIC, &FM);
    let state = classic.lookup(0, Slot::Discs, true, true, false).unwrap();
    assert!(state.controls.iter().filter(|c| c.widget == Widget::Disc).all(|c| c.face == Face::default()), "classic writes no face");
}

/// A `seek by: setting` face picks the viewer's rung of its ladder, else its `default`.
#[test]
fn a_ladder_face_picks_the_viewers_interval_then_its_default() {
    let mut face = Face::default();
    face.states.push(("default".to_owned(), FaceRef::Asset("d".to_owned())));
    face.ladder.push((10, FaceRef::Asset("ten".to_owned())));
    assert_eq!(face.pick_interval(10), Some(&FaceRef::Asset("ten".to_owned())));
    assert_eq!(face.pick_interval(30), Some(&FaceRef::Asset("d".to_owned())));
    assert_eq!(Face::default().pick_interval(10), None);
}

// ---------------------------------------------------------------------------------------------------
// The playbar's clocks: app text the layout reserves room for
// ---------------------------------------------------------------------------------------------------

/// The playbar of `state`: its track, clock rects, the mode its clocks stand in and the rung they are drawn at.
fn bar_of(state: &CompiledState) -> (Rect, super::clocks::ClockRects, ClockMode, i32) {
    match state.ops.iter().find(|o| matches!(o, DrawOp::Playbar { .. })) {
        Some(DrawOp::Playbar { track, clock_rects, clocks, clock_size, .. }) => (*track, *clock_rects, *clocks, *clock_size),
        _ => panic!("no playbar"),
    }
}

/// The widest width any clock the app can print has at `size` in `bold`: every `H:MM:SS` of up to two hour
/// digits printed through `fmt_time` and measured one by one, so the claim that each position takes its widest
/// digit is checked against the strings themselves and not assumed.
fn widest_printed(measure: &dyn Measure, size: i32, bold: bool, remaining: bool) -> f32 {
    let mut widest = 0.0f32;
    for hours in 0..=99i64 {
        for minutes in 0..60i64 {
            for seconds in (0..60i64).step_by(3) {
                let ns = ((hours * 60 + minutes) * 60 + seconds) * 1_000_000_000;
                widest = widest.max(measure.width_str(&classic::fmt_time(ns, remaining), size, bold));
            }
        }
    }
    widest
}

/// **The clocks have rects, and they are where `draw_playbar_look` puts the clocks**: on the line `CLOCK_TOP`
/// under the track's top, as tall as the measurer's line at the clock's rung. Under `ends` the elapsed clock
/// starts at the bar's start, the remaining one ends at its end, and each is as wide as the widest string the
/// app can print in its drawn weight (the elapsed bold, the remaining regular and minus-led). Under
/// `follow_head` the elapsed clock travels the whole bar, so its rect is the track's width.
#[test]
fn the_playbars_clocks_stand_where_the_playbar_draws_them_as_wide_as_the_widest_clock() {
    let measures: [(&str, &dyn Measure); 2] = [("FixtureMeasure", &FM), ("ShippedMeasure", &ShippedMeasure)];
    for (measure_name, measure) in measures {
        let size = theme::size::CAPTION;
        let elapsed_w = widest_printed(measure, size, super::clocks::ELAPSED_BOLD, false);
        let remaining_w = widest_printed(measure, size, super::clocks::REMAINING_BOLD, true);
        for (name, text, mode) in [("reference", REFERENCE, ClockMode::Ends), ("classic", CLASSIC, ClockMode::FollowHead)] {
            let c = build(text, measure);
            let (track, clocks, got_mode, got_size) = bar_of(compact(&c, Slot::Discs));
            let at = format!("{name} under {measure_name}");
            assert_eq!((got_mode, got_size), (mode, size), "{at}");
            let y = track.y + classic::CLOCK_TOP;
            for rect in [clocks.elapsed, clocks.remaining] {
                assert_eq!((rect.y, rect.h), (y, measure.line_h(size)), "{at}: the clock line");
            }
            assert_eq!(clocks.remaining.x + clocks.remaining.w, track.x + track.w, "{at}: the remaining clock ends at the bar's end");
            assert_eq!(clocks.remaining.w, remaining_w, "{at}: the widest remaining clock");
            assert_eq!(clocks.elapsed.x, track.x, "{at}: the elapsed lane starts at the bar's start");
            match mode {
                ClockMode::Ends => assert_eq!(clocks.elapsed.w, elapsed_w, "{at}: the widest elapsed clock"),
                ClockMode::FollowHead => assert_eq!(clocks.elapsed.w, track.w, "{at}: the elapsed clock travels the bar"),
            }
        }
    }
}

/// **The bands are placed in `z` order, equal `z` in declared order.** `z_order` replaced a stable sort by key;
/// every sequence of up to five bands over three `z` values (duplicates included) comes out the same.
#[test]
fn bands_fill_in_z_order_and_equal_z_keep_their_declared_order() {
    let mut seen = 0;
    for n in 0..=5usize {
        for code in 0..3usize.pow(n as u32) {
            let z: Vec<i64> = (0..n).map(|i| (code / 3usize.pow(i as u32) % 3) as i64 - 1).collect();
            let mut want: Vec<usize> = (0..n).collect();
            want.sort_by_key(|&i| z[i]);
            assert_eq!(z_order(n, |i| z[i]), want, "z {z:?}");
            seen += 1;
        }
    }
    assert_eq!(seen, 1 + 3 + 9 + 27 + 81 + 243);
}

/// **A Skip state carries a credits layout exactly when its two pill rects differ.** The kind reaches the layout
/// only through the pill's rect, so equal rects mean one layout, and the compile does not work the second out.
#[test]
fn a_skip_state_carries_a_credits_layout_exactly_when_its_pill_rects_differ() {
    let mut alt = 0;
    for text in [CLASSIC, REFERENCE] {
        let compiled = build(text, &FM);
        for state in compiled.states().iter().filter(|s| s.key.slot == Slot::Skip) {
            let [intro, credits] = state.standin.skip.expect("a Skip state places the pill");
            assert_eq!(state.skip_alt.is_some(), intro != credits, "{:?}", state.key);
            alt += usize::from(state.skip_alt.is_some());
        }
    }
    assert!(alt > 0, "no Skip state differs between the two labels: the test saw nothing");
}

/// **The clocks' reserved line is a whole number of pixels at every rung.** `CLOCK_LINE` is a ratio, and 1.25 of
/// LABEL (26) is 32.5 and of MICRO (22) is 27.5: a half pixel under the bar that moves every band beneath it
/// onto a half-pixel edge. The reserve rounds up to whole pixels, never below the ratio, and CAPTION (24), the
/// rung classic's clocks are drawn at, stays at its 30.
#[test]
fn the_clocks_reserved_line_is_a_whole_number_of_pixels_at_every_rung() {
    for rung in super::schema::SIZE_RUNGS.iter().map(|&(_, px)| px) {
        let line = classic::clock_line_h(rung);
        assert_eq!(line, line.ceil(), "the reserved line at {rung} px is {line}, a fraction of a pixel");
        assert!(line >= rung as f32 * classic::CLOCK_LINE, "the reserved line at {rung} px is {line}, under the ratio");
    }
    assert_eq!(classic::clock_line_h(theme::size::CAPTION), 30.0, "classic's clock line moved");
    assert_eq!(classic::clock_line_h(theme::size::LABEL), 33.0);
    assert_eq!(classic::clock_line_h(theme::size::MICRO), 28.0);
}

/// **The bar's cell holds the clocks' line, so no band under it starts on it.** Every control of every state of
/// every fixture that stands under a track, over the track's own columns, starts below the bottom of that
/// track's clock line (`CLOCK_TOP` plus `clock_line_h(clock rung)` from the track's top), and the reserved
/// line is at least as tall as the line either test measurer reports for any rung.
#[test]
fn no_band_under_the_bar_starts_on_the_clocks_line() {
    for rung in super::schema::SIZE_RUNGS.iter().map(|&(_, px)| px) {
        for measure in [&FM as &dyn Measure, &ShippedMeasure] {
            assert!(classic::clock_line_h(rung) >= measure.line_h(rung), "the reserved line at {rung} px is shorter than the font's");
        }
    }
    let fixtures = [("classic", CLASSIC), ("reference", REFERENCE), ("minimal", MINIMAL), ("hero-center", HERO), ("tabs", TABS), ("palette", PALETTE)];
    for (measure_name, measure) in [("FixtureMeasure", &FM as &dyn Measure), ("ShippedMeasure", &ShippedMeasure)] {
        let mut checked = std::collections::BTreeMap::new();
        for (name, text) in fixtures {
            let c = build(text, measure);
            for state in c.states() {
                let Some(DrawOp::Playbar { track, clock_size, .. }) = state.ops.iter().find(|o| matches!(o, DrawOp::Playbar { .. })) else {
                    continue;
                };
                let line_bottom = track.y + classic::CLOCK_TOP + classic::clock_line_h(*clock_size);
                let columns = track.x..track.x + track.w;
                for control in state.controls.iter().filter(|c| c.rect.y >= track.y + track.h && columns.contains(&c.rect.x)) {
                    *checked.entry(name).or_insert(0) += 1;
                    assert!(
                        control.rect.y >= line_bottom - plx_ui::consts::EDGE_TOL,
                        "{name} under {measure_name} {:?}: `{}` at y {} starts on the clocks' line, which ends at {line_bottom}",
                        state.key,
                        control.id,
                        control.rect.y
                    );
                }
            }
        }
        for name in ["classic", "reference"] {
            assert!(checked.contains_key(name), "{name} under {measure_name}: the census found no control under the bar ({checked:?})");
        }
    }
}

/// **Classic's tabs did not move.** The bar's cell now holds the clocks' line (`CLOCK_TOP` plus
/// `clock_line_h(CAPTION)`, 60 px from the track's top), so the `tabs` band's `gap_before` in
/// `classic.plxhud.json` is the distance from the cell's foot to the tabs' top, `tab_y`: 949 less 882 + 60. The
/// literal in the fixture is that arithmetic, spelt here from the constants.
#[test]
fn classics_tabs_gap_is_the_distance_left_after_the_clocks_line() {
    let key = StateKey { open: 0, slot: Slot::Discs, chapters: false, episode: false, paused: false };
    let geometry = build(CLASSIC, &FM).geometry(key, SkipKind::Intro).expect("the compact state exists");
    let cell_foot = geometry.playbar.y + classic::CLOCK_TOP + classic::clock_line_h(theme::size::CAPTION);
    let gap = geometry.tab_row.y - cell_foot;
    assert_eq!(gap, 7.0, "the tabs sit 7 px under the bar's cell");
    let parsed = package(CLASSIC);
    let bands = parsed.layout.get("block").and_then(parse::Val::as_list).expect("the bands");
    let tabs = bands
        .iter()
        .filter_map(parse::Val::as_object)
        .find(|band| band.get("id").and_then(parse::Val::as_str) == Some("tabs"))
        .expect("the tabs band");
    let Some(parse::Val::Keyed(gap_before)) = tabs.get("gap_before") else { panic!("the tabs' gap is keyed on the open section") };
    assert_eq!(gap_before.base.as_f64(), Some(f64::from(gap)), "the fixture's literal");
}
