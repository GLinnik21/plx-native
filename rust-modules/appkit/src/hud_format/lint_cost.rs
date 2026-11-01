//! **I12, the cost ceilings** (`docs/hud-plugins.md`, "Cost" and "Invariants" I12): a state's [`Cost`] must be
//! at or under its class's [`Ceiling`] in every term.
//!
//! **The class** ([`Class::of`]) is how many sections the state has open: none (`Compact`), one, or two. The
//! slot never changes the class (Up Next and Skip are compact states), and each class stands alone: the
//! two-section ceiling is not capped by the one-section numbers.
//!
//! **The ceilings are measured, never chosen.** A class's ceiling in each of the four terms is the MAXIMUM of
//! that term over the states of that class that were run on the television (`ARM_PROFILE=release`, 500 or more
//! frames of Mali HWCNT each, every one holding 59-61 fps with `dropped=0`; PR8's "TV cost calibration",
//! #1164), taken from what the compiler reports for those exact states, not rounded and with no headroom: no
//! ceiling admits a cost no measured state reached. Every constant says which state it came from, and
//! `the_ceilings_are_the_maximum_over_the_measured_states` recomputes all twelve from `classic.plxhud.json`
//! and `reference.plxhud.json`, so a layout change that moves one fails until someone re-measures.
//!
//! **The measured states**, with what the compiler reports for each (English, the television's advances
//! [`ShippedMeasure`](plx_base::fontcov::advances::ShippedMeasure); draw ops, glyph runs, texture bytes,
//! blended px; GPU_ACTIVE in M cycles a frame in brackets):
//!
//! | class | state | cost |
//! | --- | --- | --- |
//! | compact | classic, a film with chapters: Discs slot, More, Audio, Subtitles, tabs Info and Chapters (4.33) | 9, 6, 49152, 945408 |
//! | compact | classic, an episode with no chapters: Previous, Next, Subtitles, Audio, More, tab Info (4.17) | 10, 5, 81920, 942912 |
//! | compact | reference, a film (3.73) | 11, 4, 81280, 838253 |
//! | one section | classic Chapters (5.23) | 22, 14, 1119744, 919296 |
//! | one section | classic Info (6.84) | 10, 14, 230400, 1383936 |
//! | one section | reference Chapters (4.72) | 45, 21, 769920, 1274323 |
//! | two sections | reference Chapters and Info (4.92) | 52, 28, 814016, 1592851 |
//!
//! **What this does not know.** The calibration notes record neither the television's language nor its
//! chapters (the mock film has them, the mock has no episode with them). The numbers above are English, a
//! film with chapters, and a state of a package or language nobody ran is a state nobody measured: the
//! pushed head enforces nothing on those (the handoff of PR19 lists them with their four numbers).
//!
//! Pure: no IO, no global. [`over`] and [`state_cost`] take plain values, so the television can ask the same
//! question of a compiled state at selection (never in a frame) and the lint asks it of every state.

use super::compile::{CompiledState, Cost, SkipKind};

// ---------------------------------------------------------------------------------------------------
// The twelve ceilings
// ---------------------------------------------------------------------------------------------------

/// **Compact, draw ops: 11**, from the reference's compact film (11 draw ops; classic's film is 9 and its
/// episode with no chapters 10).
pub const MAX_COMPACT_DRAW_OPS: usize = 11;
/// **Compact, glyph runs: 6**, from classic's compact film (6; the episode with no chapters is 5, the
/// reference's film 4).
pub const MAX_COMPACT_GLYPH_RUNS: usize = 6;
/// **Compact, texture bytes: 81920**, from classic's compact episode with no chapters (five discs at
/// 64 x 64 x 4; the reference's film is 81280, classic's film 49152).
pub const MAX_COMPACT_TEXTURE_BYTES: u64 = 81_920;
/// **Compact, blended px: 945408**, from classic's compact film (the scrim and the two tab plates; the
/// episode with no chapters is 942912, the reference's film 838253).
pub const MAX_COMPACT_BLENDED_PX: u64 = 945_408;

/// **One section, draw ops: 45**, from the reference's Chapters (classic's Chapters is 22, its Info 10).
pub const MAX_ONE_SECTION_DRAW_OPS: usize = 45;
/// **One section, glyph runs: 21**, from the reference's Chapters (classic's Chapters and Info are 14).
pub const MAX_ONE_SECTION_GLYPH_RUNS: usize = 21;
/// **One section, texture bytes: 1119744**, from classic's Chapters (six 288 x 162 cards at 4 bytes and
/// five discs; the reference's Chapters is 769920, classic's Info 230400).
pub const MAX_ONE_SECTION_TEXTURE_BYTES: u64 = 1_119_744;
/// **One section, blended px: 1383936**, from classic's Info, the costliest state measured (6.84 M cycles; the
/// reference's Chapters is 1274323, classic's Chapters 919296).
pub const MAX_ONE_SECTION_BLENDED_PX: u64 = 1_383_936;

/// **Two sections, draw ops: 52**, from the reference's Chapters and Info, the only two-section state.
pub const MAX_TWO_SECTIONS_DRAW_OPS: usize = 52;
/// **Two sections, glyph runs: 28**, from the reference's Chapters and Info.
pub const MAX_TWO_SECTIONS_GLYPH_RUNS: usize = 28;
/// **Two sections, texture bytes: 814016**, from the reference's Chapters and Info.
pub const MAX_TWO_SECTIONS_TEXTURE_BYTES: u64 = 814_016;
/// **Two sections, blended px: 1592851**, from the reference's Chapters and Info.
pub const MAX_TWO_SECTIONS_BLENDED_PX: u64 = 1_592_851;

// ---------------------------------------------------------------------------------------------------
// Classes, ceilings, the comparison
// ---------------------------------------------------------------------------------------------------

/// The state class a ceiling is calibrated for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// no section open (every slot: Discs, Up Next, Skip)
    Compact,
    /// one section open
    OneSection,
    /// both sections open
    TwoSections,
}

impl Class {
    /// The class of a state whose `StateKey::open` is `open`.
    pub fn of(open: u8) -> Class {
        match open.count_ones() {
            0 => Class::Compact,
            1 => Class::OneSection,
            _ => Class::TwoSections,
        }
    }

    /// The class as the report spells it.
    pub fn name(self) -> &'static str {
        match self {
            Class::Compact => "compact",
            Class::OneSection => "one-section",
            Class::TwoSections => "two-sections",
        }
    }
}

/// A class's ceiling: the most each cost term may reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceiling {
    pub draw_ops: usize,
    pub glyph_runs: usize,
    pub texture_bytes: u64,
    pub blended_px: u64,
}

/// The ceiling of `class`.
pub const fn ceiling(class: Class) -> Ceiling {
    match class {
        Class::Compact => Ceiling {
            draw_ops: MAX_COMPACT_DRAW_OPS,
            glyph_runs: MAX_COMPACT_GLYPH_RUNS,
            texture_bytes: MAX_COMPACT_TEXTURE_BYTES,
            blended_px: MAX_COMPACT_BLENDED_PX,
        },
        Class::OneSection => Ceiling {
            draw_ops: MAX_ONE_SECTION_DRAW_OPS,
            glyph_runs: MAX_ONE_SECTION_GLYPH_RUNS,
            texture_bytes: MAX_ONE_SECTION_TEXTURE_BYTES,
            blended_px: MAX_ONE_SECTION_BLENDED_PX,
        },
        Class::TwoSections => Ceiling {
            draw_ops: MAX_TWO_SECTIONS_DRAW_OPS,
            glyph_runs: MAX_TWO_SECTIONS_GLYPH_RUNS,
            texture_bytes: MAX_TWO_SECTIONS_TEXTURE_BYTES,
            blended_px: MAX_TWO_SECTIONS_BLENDED_PX,
        },
    }
}

/// One of the four cost terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    DrawOps,
    GlyphRuns,
    TextureBytes,
    BlendedPx,
}

impl Term {
    /// The term as the report spells it: [`Cost`]'s field name.
    pub fn name(self) -> &'static str {
        match self {
            Term::DrawOps => "draw_ops",
            Term::GlyphRuns => "glyph_runs",
            Term::TextureBytes => "texture_bytes",
            Term::BlendedPx => "blended_px",
        }
    }
}

/// A term over its ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Over {
    pub term: Term,
    pub value: u64,
    pub ceiling: u64,
}

/// Every term of `cost` above `ceiling`, in the order draw ops, glyph runs, texture bytes, blended px; empty
/// when the state is within its class.
pub fn over(ceiling: Ceiling, cost: &Cost) -> Vec<Over> {
    let terms = [
        (Term::DrawOps, cost.draw_ops as u64, ceiling.draw_ops as u64),
        (Term::GlyphRuns, cost.glyph_runs as u64, ceiling.glyph_runs as u64),
        (Term::TextureBytes, cost.texture_bytes, ceiling.texture_bytes),
        (Term::BlendedPx, cost.blended_px, ceiling.blended_px),
    ];
    terms.into_iter().filter(|&(_, value, ceiling)| value > ceiling).map(|(term, value, ceiling)| Over { term, value, ceiling }).collect()
}

/// **What a state costs for the ceiling's purpose**: its own [`Cost`], or, for a Skip state whose credits pill
/// lays out differently ([`CompiledState::skip_variant`]), the larger of the two layouts term by term, so a
/// ceiling holds whichever label the viewer's marker draws.
pub fn state_cost(state: &CompiledState) -> Cost {
    let credits = state.skip_variant(SkipKind::Credits);
    let (a, b) = (&state.cost, &credits.cost);
    Cost {
        draw_ops: a.draw_ops.max(b.draw_ops),
        glyph_runs: a.glyph_runs.max(b.glyph_runs),
        texture_bytes: a.texture_bytes.max(b.texture_bytes),
        blended_px: a.blended_px.max(b.blended_px),
    }
}

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;

    use super::*;
    use crate::hud_format::compile::{compile, CompileCtx, Compiled, StateKey, OPEN_CHAPTERS, OPEN_INFO};
    use crate::hud_format::lint::LintLang;
    use crate::hud_format::parse;
    use crate::hud_format::reach::Slot;

    const CLASSIC: &str = include_str!("fixtures/classic.plxhud.json");
    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");

    /// `text` compiled the way the television compiles it: English, the television's advances.
    fn english(text: &str) -> Compiled {
        let package = parse::parse(text.as_bytes()).package.expect("a package");
        let en = LintLang::for_tag("en").expect("English is shipped");
        compile(&package, &ShippedMeasure, CompileCtx { labels: en.labels })
    }

    /// One state the television ran: the package, the state, and the class's term it set.
    struct Measured {
        name: &'static str,
        text: &'static str,
        key: StateKey,
    }

    const fn key(open: u8, chapters: bool, episode: bool) -> StateKey {
        StateKey { open, slot: Slot::Discs, chapters, episode, paused: false }
    }

    /// The seven states of the calibration, by class. A film is a non-episode with chapters; the one episode
    /// has none.
    fn measured(class: Class) -> Vec<Measured> {
        let m = |name, text, key| Measured { name, text, key };
        match class {
            Class::Compact => vec![
                m("classic, a film", CLASSIC, key(0, true, false)),
                m("classic, an episode with no chapters", CLASSIC, key(0, false, true)),
                m("reference, a film", REFERENCE, key(0, true, false)),
            ],
            Class::OneSection => vec![
                m("classic Chapters", CLASSIC, key(OPEN_CHAPTERS, true, false)),
                m("classic Info", CLASSIC, key(OPEN_INFO, true, false)),
                m("reference Chapters", REFERENCE, key(OPEN_CHAPTERS, true, false)),
            ],
            Class::TwoSections => vec![m("reference Chapters and Info", REFERENCE, key(OPEN_CHAPTERS | OPEN_INFO, true, false))],
        }
    }

    fn cost_of(m: &Measured) -> Cost {
        let compiled = english(m.text);
        state_cost(compiled.state(m.key).unwrap_or_else(|| panic!("{}: the package has no such state", m.name)))
    }

    /// The four terms as `(name, value)`.
    fn terms(c: &Cost) -> [(&'static str, u64); 4] {
        [("draw_ops", c.draw_ops as u64), ("glyph_runs", c.glyph_runs as u64), ("texture_bytes", c.texture_bytes), ("blended_px", c.blended_px)]
    }

    fn ceiling_terms(c: Ceiling) -> [(&'static str, u64); 4] {
        [("draw_ops", c.draw_ops as u64), ("glyph_runs", c.glyph_runs as u64), ("texture_bytes", c.texture_bytes), ("blended_px", c.blended_px)]
    }

    /// The state each constant's doc names as its source, per class and term.
    const SOURCES: [(Class, [&str; 4]); 3] = [
        (Class::Compact, ["reference, a film", "classic, a film", "classic, an episode with no chapters", "classic, a film"]),
        (Class::OneSection, ["reference Chapters", "reference Chapters", "classic Chapters", "classic Info"]),
        (Class::TwoSections, ["reference Chapters and Info"; 4]),
    ];

    #[test]
    fn the_ceilings_are_the_maximum_over_the_measured_states() {
        for (class, sources) in SOURCES {
            let states = measured(class);
            let costs: Vec<Cost> = states.iter().map(cost_of).collect();
            let want = ceiling_terms(ceiling(class));
            for (i, (term, ceil)) in want.into_iter().enumerate() {
                let max = costs.iter().map(|c| terms(c)[i].1).max().expect("a measured state");
                assert_eq!(ceil, max, "{} {term}: the ceiling is the maximum over the measured states", class.name());
                // ... and the state the constant's doc names as its source is a state that reaches it
                let source = states.iter().position(|m| m.name == sources[i]).unwrap_or_else(|| panic!("{}: no state `{}`", class.name(), sources[i]));
                assert_eq!(terms(&costs[source])[i].1, ceil, "{} {term}: `{}` is the named source", class.name(), sources[i]);
            }
        }
    }

    #[test]
    fn a_class_stands_alone() {
        // two sections is not capped by one section's numbers (its draw ops are above, its blended px above too)
        let (one, two) = (ceiling(Class::OneSection), ceiling(Class::TwoSections));
        assert!(two.draw_ops > one.draw_ops && two.blended_px > one.blended_px);
        // and one section's texture bytes are above two sections', so no class is the max of its neighbours
        assert!(one.texture_bytes > two.texture_bytes);
        assert_eq!([Class::of(0), Class::of(OPEN_CHAPTERS), Class::of(OPEN_INFO), Class::of(OPEN_CHAPTERS | OPEN_INFO)], [
            Class::Compact,
            Class::OneSection,
            Class::OneSection,
            Class::TwoSections
        ]);
    }

    #[test]
    fn over_names_each_term_above_its_ceiling_and_only_those() {
        let ceil = Ceiling { draw_ops: 10, glyph_runs: 6, texture_bytes: 1000, blended_px: 5000 };
        let at = Cost { draw_ops: 10, glyph_runs: 6, texture_bytes: 1000, blended_px: 5000 };
        assert!(over(ceil, &at).is_empty(), "a term AT its ceiling passes");
        let above = Cost { draw_ops: 11, glyph_runs: 6, texture_bytes: 1000, blended_px: 5001 };
        let got = over(ceil, &above);
        assert_eq!(
            got,
            [Over { term: Term::DrawOps, value: 11, ceiling: 10 }, Over { term: Term::BlendedPx, value: 5001, ceiling: 5000 }]
        );
        assert_eq!(got.iter().map(|o| o.term.name()).collect::<Vec<_>>(), ["draw_ops", "blended_px"]);
    }

    #[test]
    fn a_skip_state_costs_the_larger_of_its_two_pill_layouts() {
        let compiled = english(CLASSIC);
        let skip = compiled.states().iter().find(|s| s.key.slot == Slot::Skip).expect("a Skip state");
        let c = state_cost(skip);
        assert!(c.blended_px >= skip.cost.blended_px && c.blended_px >= skip.skip_variant(SkipKind::Credits).cost.blended_px);
        // a state with no credits layout of its own costs exactly itself
        let discs = compiled.lookup(0, Slot::Discs, true, false, false).expect("a compact state");
        assert_eq!(state_cost(discs), discs.cost);
    }
}
