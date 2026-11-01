//! **I12, the cost ceilings** (`docs/hud-plugins.md`, "Cost" and "Invariants" I12): a state's [`Cost`] must be
//! at or under its class's [`Ceiling`] in every term.
//!
//! **The class** ([`Class::of`]) is how many sections the state has open: none (`Compact`), one, or two. The
//! slot never changes the class (Up Next and Skip are compact states), and each class stands alone: the
//! two-section ceiling is not capped by the one-section numbers.
//!
//! **The price is one model on every side.** A cost is the compiler's accounting of a state laid out with the
//! whole-pixel advances of `appfont.ttf` and `appfont-bold.ttf` and a line height of 1.21 x the size
//! ([`WholePixelMeasure`](plx_base::fontcov::advances::WholePixelMeasure); in the tests
//! [`ShippedMeasure`](plx_base::fontcov::advances::ShippedMeasure), the same numbers read from `pkg/`). The
//! text FIT keeps the live measure of the set that lints, but nothing that is priced does: the same package
//! bytes cost the same, and get the same `HUD-E1401` verdict wherever the lint runs,
//! whatever each firmware's SDL_ttf does with hinting (`lint::lint_price`; there is no entry point that
//! prices with the fit measure). The model is not the drawn frame: it prices text with the whole-pixel advances and a line height of 1.21 x size,
//! and no drawn frame has been measured against it, so a ceiling is a number of the model's, for the states the
//! television ran.
//!
//! **The ceilings are fixed by the states the television ran, never chosen.** A class's ceiling in each of the
//! four terms is the MAXIMUM of that term, in the model, over the states of that class (`ARM_PROFILE=release`,
//! 500 or more frames of Mali HWCNT each, `dropped=0` on the pacing boot; PR8's "TV cost calibration", #1164,
//! and PR19's seven pairs below), not rounded and with no headroom: no ceiling admits a cost no such state
//! reached.
//!
//! **A state passes when no frame is late on the app's side, read from a kernel trace** (`tools/tv-sched-trace.sh`,
//! `tools/analyze-sched-trace.py --latch`), not when the set shows zero late frames, because it never does:
//! every state on this set has bursts of 24-35 ms frames about every 11 s, and in the three traced strip states the kernel traces class those as the
//! compositor's (the app's GPU job done 3-8 ms before the aim, the buffer taken two ticks late). The other four
//! pairs have not been traced yet. The three traces, one per strip state, found no CPU-late or GPU-late frame in
//! any window, with an on-time margin (GPU done minus aim) at the 99th percentile of -4.0 to -5.9 ms (`docs/hud-plugins.md`, "The ceilings (PR19)", has them). What makes the
//! compositor late is not determined; it is not specific to a HUD state. Every constant says which state and language it came from, and
//! `the_ceilings_are_the_maximum_over_every_state_of_classic_and_the_reference_in_every_language` recomputes
//! all twelve from `classic.plxhud.json` and `reference.plxhud.json` over all eleven languages, every state and
//! slot (330 rows), and asserts each is attained by the pair the constant names, so a layout change that moves
//! one fails until someone re-measures.
//!
//! **A strip is priced by what its Shelf paints at rest.** The chapters strip's cards are
//! `card_row::most_painted_at_rest` (`emit_strip`): the most cards the row paints at any resting scroll (scroll
//! 0, the two lattices `scroll_into_view` walks to, and the end), which no chapter count changes: ten on the
//! reference and seven on classic (a scrolled row paints a partial card on the left as well, which the strip's
//! width over its card pitch plus one, 8 and 6, did not count). The television ran the strips twice. First at scroll 0
//! with ten chapters, each with its own thumbnail, where the Shelf paints nine and six (GPU_ACTIVE 5.09, 5.10 and
//! 4.87 for pairs 4, 5 and 7). Then walked into a rest past the first screen, with 30 chapters each with its own
//! thumbnail, where the Shelf paints the priced ten and seven, on the Reference with the gear (#1199): 5.15, 5.18
//! and 5.17. The table carries the scrolled runs, the ones at the priced card count; the scroll-0 figures are
//! only "at scroll 0".
//!
//! **A measured state is (package, state, language).** The tab and button plates follow their labels' widths, so
//! `blended_px` moves with the language, and Up Next's still is as wide as the Next pill, so `texture_bytes` does
//! too: the English figure of a state is not the television's in Polish. Seven pairs attain the twelve maxima (no
//! six do: `seven_pairs_attain_the_twelve_maxima_and_no_six_rows_do`), all run with `--guest --mock` on the
//! dev TV (`ARM_PROFILE=release`, debug flavour) on an item that is an episode with chapters. Cost is the
//! compiler's: draw ops, glyph runs, texture bytes, blended px; GPU_ACTIVE is the mean in M cycles a frame
//! over the HWCNT boot (a pacing boot is separate, HWCNT serializes the GPU). No chapter strip of PR8's runs
//! had thumbnails (they drew blank cards), which is why their figures, 5.23, 4.72 and 4.92, are replaced by
//! pairs 5, 4 and 7. A strip pair's GPU figure is the scrolled run's (see above), at the priced card count:
//!
//! | pair | class | package, state, slot, language | GPU_ACTIVE | cost |
//! | --- | --- | --- | --- | --- |
//! | 1 | compact | reference, an episode, Discs, en | 3.72 | 13, 4, 102912, 838253 |
//! | 2 | compact | classic, an episode, Up Next, nl | 5.44 | 10, 9, 256120, 978752 |
//! | 3 | compact | classic, an episode, Up Next, pl | 5.50 | 10, 9, 215064, 985088 |
//! | 4 | one section | reference Chapters, an episode, Discs, en (ten strip cards, scrolled) | 5.15 | 55, 25, 963712, 1274323 |
//! | 5 | one section | classic Chapters, an episode, Discs, en (seven strip cards, scrolled) | 5.18 | 25, 16, 1306368, 919296 |
//! | 6 | one section | classic Info, an episode, Discs, be | 7.02 | 10, 14, 230400, 1395492 |
//! | 7 | two sections | reference Chapters and Info, an episode, Discs, en (ten strip cards, scrolled) | 5.17 | 62, 32, 970944, 1592851 |
//!
//! PR8's other runs stand and are under every ceiling (English; `earlier_runs_stand_and_are_within_their_class`
//! asserts their cost): classic's film with chapters (4.33; 9, 6, 49152, 945408), classic's episode with no
//! chapters (4.17; 10, 5, 81920, 942912), the reference's film (3.73; 11, 4, 81280, 838253) and classic's Info in
//! English (6.84; 10, 14, 230400, 1383936). None draws a strip.
//!
//! **What this does not know.** Not every one of the 330 rows was run: a row below a ceiling is admitted because
//! it costs no more, in each term, than the states the television did run, not because it was run itself. The
//! terms are the compiler's accounting of what the GPU draws, not the GPU's, so a package under every term can
//! still read more GPU_ACTIVE than a measured one; PR12 measures that.
//!
//! **The verdict** is `HUD-E1401` ([`check`]): every state, under every slot and in every language, whose
//! [`state_cost`] is above its class's ceiling in a term is one error at `/layout`. Classic and the reference pass
//! every state in every language (`every_state_of_classic_and_the_reference_is_within_its_class`), so neither can
//! be rejected by its own lint. `--hud-lint` prints every state's comparison ([`StateCostLine`]) whatever the
//! verdict.
//!
//! Pure: no IO, no global. [`over`] and [`state_cost`] take plain values, so the television can ask the same
//! question of a compiled state at selection (never in a frame) and the lint asks it of every state.

use super::compile::{Compiled, CompiledState, Cost, SkipKind};
use super::lint::{ctx_in, slot_name, state_name, states_of, LintLang};
use super::{codes, Finding};

// ---------------------------------------------------------------------------------------------------
// The twelve ceilings
// ---------------------------------------------------------------------------------------------------

/// **Compact, draw ops: 13**, from the reference's compact episode with chapters, Discs slot, English (pair 1 of
/// the television's seven; all eleven languages tie, and classic's compact states are 9 to 10).
pub const MAX_COMPACT_DRAW_OPS: usize = 13;
/// **Compact, glyph runs: 9**, from classic's Up Next, an episode, Dutch (pair 2; the count does not move with
/// the language, all eleven tie, and a film ties too).
pub const MAX_COMPACT_GLYPH_RUNS: usize = 9;
/// **Compact, texture bytes: 256120**, from classic's Up Next, an episode, Dutch (pair 2): the still is 16:9 and
/// as wide as the Next pill, which follows its label, so Dutch's 337 x 190 is the largest of the eleven
/// (English's 243 x 137 is 133164).
pub const MAX_COMPACT_TEXTURE_BYTES: u64 = 256_120;
/// **Compact, blended px: 985088**, from classic's Up Next, an episode, Polish (pair 3; only Polish attains it).
pub const MAX_COMPACT_BLENDED_PX: u64 = 985_088;

/// **One section, draw ops: 55**, from the reference's Chapters, an episode, Discs slot, English (pair 4; all
/// eleven languages tie): the strip priced at ten cards, four ops each, and the rest of the state.
pub const MAX_ONE_SECTION_DRAW_OPS: usize = 55;
/// **One section, glyph runs: 25**, from the reference's Chapters, an episode, Discs slot, English (pair 4; all
/// eleven languages tie, and a film ties): ten cards, two texts each, and the rest of the state.
pub const MAX_ONE_SECTION_GLYPH_RUNS: usize = 25;
/// **One section, texture bytes: 1306368**, from classic's Chapters, an episode, Discs slot, English (pair 5; all
/// eleven languages tie): seven strip cards of 288 x 162 at 4 bytes, and no discs: while a section is open
/// classic's transport bands are off (`when: "!sections"`).
pub const MAX_ONE_SECTION_TEXTURE_BYTES: u64 = 1_306_368;
/// **One section, blended px: 1395492**, from classic's Info, Discs slot, Belarusian (pair 6, the costliest state
/// on the television at 7.02 M GPU cycles a frame; only Belarusian attains it, English is 1383936).
pub const MAX_ONE_SECTION_BLENDED_PX: u64 = 1_395_492;

/// **Two sections, draw ops: 62**, from the reference's Chapters and Info, an episode, Discs slot, English (pair
/// 7, the only two-section package; all eleven languages tie).
pub const MAX_TWO_SECTIONS_DRAW_OPS: usize = 62;
/// **Two sections, glyph runs: 32**, from the reference's Chapters and Info, an episode, English (pair 7; all
/// eleven languages tie, and a film ties).
pub const MAX_TWO_SECTIONS_GLYPH_RUNS: usize = 32;
/// **Two sections, texture bytes: 970944**, from the reference's Chapters and Info, an episode, English (pair 7;
/// all eleven languages tie): ten strip cards of 192 x 84 and the 28 px "now playing" icon, 67648 bytes each, the
/// Info still of 292 x 164 and the transport's discs and icons (102912, the compact state's).
pub const MAX_TWO_SECTIONS_TEXTURE_BYTES: u64 = 970_944;
/// **Two sections, blended px: 1592851**, from the reference's Chapters and Info, an episode, English (pair 7;
/// all eleven languages tie, and a film ties).
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

// ---------------------------------------------------------------------------------------------------
// The check: HUD-E1401
// ---------------------------------------------------------------------------------------------------

/// Report this module's findings for `compiled`, the package compiled in `lang`: `HUD-E1401` for every state, under every slot, whose
/// [`state_cost`] is above its class's ceiling in at least one term. One finding per state: it names the class,
/// each term over as `value > ceiling` and the three ops that weigh most in the layout that cost the most (a Skip
/// state's credits pill is a layout of its own), each by the pointer of the node that drew it
/// ([`CompiledState::origin`]).
pub(super) fn check(lang: &LintLang, compiled: &Compiled, out: &mut Vec<Finding>) {
    for s in states_of(compiled) {
        let cost = state_cost(s.state);
        let class = Class::of(s.key.open);
        let over = over(ceiling(class), &cost);
        if over.is_empty() {
            continue;
        }
        let credits = s.state.skip_variant(SkipKind::Credits);
        let heaviest = if credits.cost.blended_px > s.state.cost.blended_px { credits } else { s.state };
        let terms = over.iter().map(|o| format!("{} {} > {}", o.term.name(), o.value, o.ceiling)).collect::<Vec<_>>().join(", ");
        let tops = heaviest
            .top_contributors()
            .iter()
            .map(|c| format!("{} (blended_px {}, texture_bytes {}, glyph_runs {})", heaviest.origin(c.op), c.blended_px, c.texture_bytes, c.glyph_runs))
            .collect::<Vec<_>>()
            .join("; ");
        out.push(
            Finding::new(
                &codes::E1401,
                "/layout",
                format!(
                    "state `{}` under `{}` is a {} state over its ceiling: {terms}. Heaviest ops: {tops}",
                    state_name(s.key),
                    slot_name(s.key),
                    class.name()
                ),
                "draw less in this state: drop or shrink the heaviest ops named, or fold their content into a section; \
                 a class's ceiling is the most any state of the class cost when run on the television",
            )
            .with(ctx_in(lang, &s)),
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// The report: every state's cost against its class's ceiling
// ---------------------------------------------------------------------------------------------------

/// **One state's cost, as `--hud-lint` prints it** (`docs/hud-plugins.md`, "LLM authoring" item 2): not a finding,
/// an information line. `over` lists the terms above the ceiling; the lint's verdict on them is the finding
/// `HUD-E1401`, never this line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateCostLine {
    /// the language it was compiled in; `None` when the cost is the same in every language that was linted
    pub lang: Option<String>,
    /// [`state_name`] of the state
    pub state: String,
    /// [`slot_name`] of the state
    pub slot: &'static str,
    pub class: Class,
    pub cost: Cost,
    pub ceiling: Ceiling,
    pub over: Vec<Over>,
}

/// The cost line of every state of `compiled`, in `Compiled::states()` order, tagged with the language `lang`.
pub(super) fn lines(lang: &LintLang, compiled: &Compiled) -> Vec<StateCostLine> {
    states_of(compiled)
        .map(|s| {
            let cost = state_cost(s.state);
            let class = Class::of(s.key.open);
            let ceiling = ceiling(class);
            StateCostLine {
                lang: Some(lang.tag.to_owned()),
                state: state_name(s.key),
                slot: slot_name(s.key),
                class,
                over: over(ceiling, &cost),
                cost,
                ceiling,
            }
        })
        .collect()
}

/// The lines of every language (`per_lang[i]` is language `i`'s, each tagged) as one list, in state order: a
/// state whose cost is the same in every language, term by term, is kept once with `lang` `None`; any other
/// state keeps one line per language. (`blended_px` moves with the language through the tab and button plates,
/// which follow their labels' widths, and `texture_bytes` through Up Next's still; the comparison is of every
/// term, so one that moves keeps the lines apart whichever it is.) With fewer than two languages nothing collapses.
pub(super) fn collapse_lines(per_lang: Vec<Vec<StateCostLine>>) -> Vec<StateCostLine> {
    let states = per_lang.first().map_or(0, Vec::len);
    // the state graph does not depend on the language; if it ever did, nothing is claimed to be shared
    if per_lang.len() < 2 || per_lang.iter().any(|lang| lang.len() != states) {
        return per_lang.into_iter().flatten().collect();
    }
    let mut out = Vec::new();
    for i in 0..states {
        let first = &per_lang[0][i];
        let shared = per_lang.iter().all(|lang| (&lang[i].state, lang[i].slot, &lang[i].cost) == (&first.state, first.slot, &first.cost));
        if shared {
            out.push(StateCostLine { lang: None, ..first.clone() });
        } else {
            out.extend(per_lang.iter().map(|lang| lang[i].clone()));
        }
    }
    out
}

#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};

/// The title line the invalid package of `HUD-E1401` adds, and the line it follows (each occurs once in the reference).
#[cfg(test)]
const TITLE_LINE: &str =
    r#"{ "text": { "bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } }"#;

/// The invalid package of `HUD-E1401` (`lint::testing::assert_case` runs it): the reference with one more text line
/// under its title. The reference's episode is AT the compact draw ops (13), Chapters at the one-section draw ops
/// and glyph runs (55, 25) and Chapters and Info at the two-section ones (62, 32), so one more text op puts every
/// state of the reference that has the title over its class; draw ops and glyph runs do not depend on the
/// measurer, so the case reads the same whichever advances price it.
#[cfg(test)]
pub(super) const INVALID: &[InvalidCase] = &[InvalidCase {
    name: "the reference with a second text line under its title",
    code: "HUD-E1401",
    pointer: "/layout",
    base: Base::Reference,
    edits: &[(
        TITLE_LINE,
        r#"{ "text": { "bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } },
          { "text": { "bind": "title.kicker", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" } }"#,
    )],
    langs: &["en"],
    skew: 0.0,
    also: &[],
}];

/// No lint code of this module is out of a parseable package's reach.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;

    use super::*;
    use crate::hud_format::compile::{compile, CompileCtx, Compiled, DrawOp, OPEN_CHAPTERS, OPEN_INFO};
    use crate::hud_format::lint::LintLang;
    use crate::hud_format::{parse, Finding};
    use crate::hud_format::reach::Slot;

    const CLASSIC: &str = include_str!("fixtures/classic.plxhud.json");
    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");

    /// `text` compiled the way the television compiles it in the language `lang`: the television's advances.
    fn compile_in(text: &str, lang: &LintLang) -> Compiled {
        let package = parse::parse(text.as_bytes()).package.expect("a package");
        compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() })
    }

    /// `text` in English.
    fn english(text: &str) -> Compiled {
        compile_in(text, &LintLang::for_tag("en").expect("English is shipped"))
    }

    /// The four terms as `(name, value)`.
    fn terms(c: &Cost) -> [(&'static str, u64); 4] {
        [("draw_ops", c.draw_ops as u64), ("glyph_runs", c.glyph_runs as u64), ("texture_bytes", c.texture_bytes), ("blended_px", c.blended_px)]
    }

    fn ceiling_terms(c: Ceiling) -> [(&'static str, u64); 4] {
        [("draw_ops", c.draw_ops as u64), ("glyph_runs", c.glyph_runs as u64), ("texture_bytes", c.texture_bytes), ("blended_px", c.blended_px)]
    }

    /// The two packages the ceilings are measured over.
    const PACKAGES: [(&str, &str); 2] = [("classic", CLASSIC), ("reference", REFERENCE)];

    /// **One pair the television ran**: a package, one of its states under one slot, and the language, with the
    /// cost the compiler reports for it (the doc table's last column) and the GPU_ACTIVE it read (M cycles a
    /// frame, `ARM_PROFILE=release`, 500 or more frames of Mali HWCNT, `dropped=0` on the pacing boot). `state` is
    /// the state in the lint's own spelling (`lint::state_name`): no `,movie` is an episode, no `,no-chapters` has
    /// chapters. Every pair ran an episode with chapters. `cards` is the number of strip cards the GPU figure was
    /// measured at (`None`: the state draws no strip); it must be the number the cost is priced at.
    struct Pair {
        n: u8,
        package: &'static str,
        state: &'static str,
        slot: &'static str,
        lang: &'static str,
        gpu: f32,
        cards: Option<usize>,
        cost: [u64; 4],
    }

    /// The seven pairs of the television's calibration (`docs/hud-plugins.md`, "The ceilings"): the smallest set of
    /// (package, state, language) that attains the twelve maxima. Pairs 4, 5 and 7 are the runs walked to a
    /// scrolled rest, where the Shelf paints the priced ten (reference) and seven (classic) cards; their runs at
    /// scroll 0 (nine and six cards: 5.09, 5.10, 4.87) are not the priced count and are not here.
    const PAIRS: [Pair; 7] = [
        Pair { n: 1, package: "reference", state: "compact", slot: "discs", lang: "en", gpu: 3.72, cards: None, cost: [13, 4, 102_912, 838_253] },
        Pair { n: 2, package: "classic", state: "compact", slot: "up_next", lang: "nl", gpu: 5.44, cards: None, cost: [10, 9, 256_120, 978_752] },
        Pair { n: 3, package: "classic", state: "compact", slot: "up_next", lang: "pl", gpu: 5.50, cards: None, cost: [10, 9, 215_064, 985_088] },
        Pair { n: 4, package: "reference", state: "chapters", slot: "discs", lang: "en", gpu: 5.15, cards: Some(10), cost: [55, 25, 963_712, 1_274_323] },
        Pair { n: 5, package: "classic", state: "chapters", slot: "discs", lang: "en", gpu: 5.18, cards: Some(7), cost: [25, 16, 1_306_368, 919_296] },
        Pair { n: 6, package: "classic", state: "info", slot: "discs", lang: "be", gpu: 7.02, cards: None, cost: [10, 14, 230_400, 1_395_492] },
        Pair { n: 7, package: "reference", state: "chapters+info", slot: "discs", lang: "en", gpu: 5.17, cards: Some(10), cost: [62, 32, 970_944, 1_592_851] },
    ];

    /// The pair each constant's doc names as its source, per class and term (draw ops, glyph runs, texture bytes,
    /// blended px).
    const SOURCES: [(Class, [u8; 4]); 3] = [(Class::Compact, [1, 2, 2, 3]), (Class::OneSection, [4, 4, 5, 6]), (Class::TwoSections, [7; 4])];

    /// One state of one package in one language, as the lint costs it.
    struct Row {
        package: &'static str,
        lang: &'static str,
        state: String,
        slot: &'static str,
        class: Class,
        cost: Cost,
        /// the cards its chapters strip is priced at (0: no strip)
        strip: usize,
    }

    /// **Every state and slot of `classic` and the reference in each of the eleven shipped languages** (330 rows),
    /// costed as `--hud-lint` prints them ([`state_cost`], the television's advances).
    fn every_row() -> Vec<Row> {
        let mut rows = Vec::new();
        for (package, text) in PACKAGES {
            for lang in LintLang::shipped() {
                for s in compile_in(text, &lang).states() {
                    let strip = s.ops.iter().filter_map(|op| if let DrawOp::Strip { visible, .. } = op { Some(*visible) } else { None }).max().unwrap_or(0);
                    rows.push(Row { package, lang: lang.tag, state: state_name(s.key), slot: slot_name(s.key), class: Class::of(s.key.open), cost: state_cost(s), strip });
                }
            }
        }
        rows
    }


    #[test]
    fn the_ceilings_are_the_maximum_over_every_state_of_classic_and_the_reference_in_every_language() {
        let rows = every_row();
        assert_eq!(rows.len(), 330, "classic 18 states x 11 languages, the reference 12 x 11");
        for (class, sources) in SOURCES {
            let of_class: Vec<&Row> = rows.iter().filter(|r| r.class == class).collect();
            assert!(!of_class.is_empty(), "{}: no state", class.name());
            for (i, (term, ceil)) in ceiling_terms(ceiling(class)).into_iter().enumerate() {
                let max = of_class.iter().map(|r| terms(&r.cost)[i].1).max().expect("a state");
                assert_eq!(ceil, max, "{} {term}: the ceiling is the maximum over every state", class.name());
                // ... attained by the pair the constant's doc names, and that pair is one the television ran
                let pair = PAIRS.iter().find(|p| p.n == sources[i]).expect("a pair of the calibration");
                let row = of_class
                    .iter()
                    .find(|r| (r.package, r.state.as_str(), r.slot, r.lang) == (pair.package, pair.state, pair.slot, pair.lang))
                    .unwrap_or_else(|| panic!("{} {term}: pair {} ({} {} {} {}) is not a {} state", class.name(), pair.n, pair.package, pair.state, pair.slot, pair.lang, class.name()));
                assert_eq!(terms(&row.cost)[i].1, ceil, "{} {term}: pair {} is the named source", class.name(), pair.n);
            }
        }
    }

    /// PR8's runs that stand (English, GPU_ACTIVE in M cycles a frame): the cost they were measured at.
    const EARLIER: [(&str, &str, &str, f32, [u64; 4]); 4] = [
        ("classic", "compact,movie", "discs", 4.33, [9, 6, 49_152, 945_408]),
        ("classic", "compact,no-chapters", "discs", 4.17, [10, 5, 81_920, 942_912]),
        ("reference", "compact,movie", "discs", 3.73, [11, 4, 81_280, 838_253]),
        ("classic", "info,movie", "discs", 6.84, [10, 14, 230_400, 1_383_936]),
    ];

    #[test]
    fn earlier_runs_stand_and_are_within_their_class() {
        let rows = every_row();
        for (package, state, slot, gpu, want) in EARLIER {
            let row = rows
                .iter()
                .find(|r| (r.package, r.state.as_str(), r.slot, r.lang) == (package, state, slot, "en"))
                .unwrap_or_else(|| panic!("{package} {state} {slot}: no such state"));
            assert_eq!(terms(&row.cost).map(|t| t.1), want, "{package} {state} {slot} ({gpu} M cycles) costs what it was measured at");
            assert!(over(ceiling(row.class), &row.cost).is_empty(), "{package} {state} {slot} is within its class");
        }
    }

    #[test]
    fn each_pair_costs_what_the_doc_table_says_and_its_gpu_figure_was_run_at_the_priced_card_count() {
        // The recomputation test above finds each pair by name and checks the terms it is the source of; this
        // one holds the table's whole cost column to the compiler (all four terms of all seven pairs), and the
        // GPU column to the one thing it must never be: a number measured at another card count than the one
        // the cost is priced at (the strip is priced at what its Shelf paints at a scrolled rest, ten and seven).
        let rows = every_row();
        for pair in &PAIRS {
            let row = rows
                .iter()
                .find(|r| (r.package, r.state.as_str(), r.slot, r.lang) == (pair.package, pair.state, pair.slot, pair.lang))
                .unwrap_or_else(|| panic!("pair {}: no such state", pair.n));
            assert_eq!(terms(&row.cost).map(|t| t.1), pair.cost, "pair {}: the cost the table prints", pair.n);
            assert_eq!(pair.cards.unwrap_or(0), row.strip, "pair {}: the GPU figure was run at the card count the cost is priced at", pair.n);
            assert!(pair.gpu > 0.0 && pair.gpu < 7.1, "pair {}: GPU_ACTIVE {} M cycles a frame", pair.n, pair.gpu);
        }
    }

    /// The (class, term) maxima a row attains, as a 12-bit mask: bit `3 * class + term`.
    fn attained(rows: &[Row]) -> Vec<(u16, usize)> {
        let classes = [Class::Compact, Class::OneSection, Class::TwoSections];
        let max: Vec<[u64; 4]> = classes
            .iter()
            .map(|&class| {
                let mut m = [0u64; 4];
                for r in rows.iter().filter(|r| r.class == class) {
                    for (i, (_, v)) in terms(&r.cost).into_iter().enumerate() {
                        m[i] = m[i].max(v);
                    }
                }
                m
            })
            .collect();
        rows.iter()
            .enumerate()
            .map(|(at, r)| {
                let c = classes.iter().position(|&class| class == r.class).expect("a class");
                let mask = terms(&r.cost).into_iter().enumerate().filter(|&(i, (_, v))| v == max[c][i]).fold(0u16, |m, (i, _)| m | 1 << (4 * c + i));
                (mask, at)
            })
            .collect()
    }

    #[test]
    fn seven_pairs_attain_the_twelve_maxima_and_no_six_rows_do() {
        const ALL: u16 = (1 << 12) - 1;
        let rows = every_row();
        let by_row = attained(&rows);
        // the table's seven attain all twelve ...
        let mask_of = |pair: &Pair| {
            let at = rows.iter().position(|r| (r.package, r.state.as_str(), r.slot, r.lang) == (pair.package, pair.state, pair.slot, pair.lang)).expect("a pair");
            by_row[at].0
        };
        assert_eq!(PAIRS.iter().fold(0, |m, p| m | mask_of(p)), ALL, "the seven pairs attain every maximum");
        // ... each is needed (none is a decoy) ...
        for skip in 0..PAIRS.len() {
            let rest = PAIRS.iter().enumerate().filter(|&(i, _)| i != skip).fold(0, |m, (_, p)| m | mask_of(p));
            assert_ne!(rest, ALL, "pair {} attains a maximum no other pair does", PAIRS[skip].n);
        }
        // ... and no six of the 330 rows attain all twelve: search the distinct masks, six at a time
        let mut masks: Vec<u16> = by_row.iter().map(|&(m, _)| m).filter(|&m| m != 0).collect();
        masks.sort_unstable();
        masks.dedup();
        fn covers(masks: &[u16], from: usize, left: usize, have: u16) -> bool {
            if have == ALL {
                return true;
            }
            left > 0 && (from..masks.len()).any(|i| covers(masks, i + 1, left - 1, have | masks[i]))
        }
        assert!(!covers(&masks, 0, 6, 0), "six rows attain all twelve maxima: the table's seven are not the smallest set");
        assert!(covers(&masks, 0, 7, 0), "seven rows attain all twelve");
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

    fn line(lang: &str, state: &str, blended_px: u64) -> StateCostLine {
        let cost = Cost { draw_ops: 9, glyph_runs: 6, texture_bytes: 49_152, blended_px };
        let class = Class::Compact;
        StateCostLine { lang: Some(lang.to_owned()), state: state.to_owned(), slot: "discs", class, over: over(ceiling(class), &cost), cost, ceiling: ceiling(class) }
    }

    #[test]
    fn a_state_with_the_same_cost_in_every_language_is_reported_once() {
        let per_lang = vec![
            vec![line("en", "compact", 945_408), line("en", "chapters", 900_000)],
            vec![line("de", "compact", 945_408), line("de", "chapters", 900_512)],
        ];
        let got = collapse_lines(per_lang.clone());
        assert_eq!(got.len(), 3, "compact once, chapters once per language: {got:?}");
        assert_eq!((got[0].lang.as_deref(), got[0].state.as_str()), (None, "compact"));
        assert_eq!((got[1].lang.as_deref(), got[2].lang.as_deref()), (Some("en"), Some("de")));
        // one language alone keeps its tag
        let one = collapse_lines(vec![per_lang[0].clone()]);
        assert!(one.iter().all(|l| l.lang.as_deref() == Some("en")) && one.len() == 2);
    }

    #[test]
    fn the_lint_reports_every_state_of_a_package_against_its_class() {
        let en = LintLang::for_tag("en").expect("en");
        let lint = crate::hud_format::lint::lint_report(CLASSIC, &ShippedMeasure, &[en], &Default::default());
        let compiled = english(CLASSIC);
        assert_eq!(lint.costs.len(), compiled.states().len(), "one line per state");
        let film = lint.costs.iter().find(|l| (l.state.as_str(), l.slot) == ("compact,movie", "discs")).expect("the film's line");
        assert_eq!((film.class, film.cost.blended_px, film.ceiling.blended_px), (Class::Compact, 945_408, MAX_COMPACT_BLENDED_PX));
        assert!(film.over.is_empty(), "the film is a measured state: {film:?}");
        assert!(lint.costs.iter().any(|l| l.class == Class::OneSection) && lint.costs.iter().all(|l| l.lang.as_deref() == Some("en")));
    }

    #[test]
    fn a_skip_state_costs_the_larger_of_its_two_pill_layouts() {
        let compiled = english(CLASSIC);
        let skip = compiled.states().iter().find(|s| s.key.slot == Slot::Skip).expect("a Skip state");
        // English lays the credits pill out no larger than the intro pill, so make it larger in two terms
        // and smaller in a third: the cost is the termwise maximum, not either layout
        let mut state = skip.clone();
        let mut credits = skip.clone();
        credits.skip_alt = None;
        credits.cost = Cost {
            draw_ops: skip.cost.draw_ops + 2,
            glyph_runs: skip.cost.glyph_runs.saturating_sub(1),
            texture_bytes: skip.cost.texture_bytes,
            blended_px: skip.cost.blended_px + 1_000,
        };
        state.skip_alt = Some(Box::new(credits));
        let c = state_cost(&state);
        assert_eq!(
            (c.draw_ops, c.glyph_runs, c.texture_bytes, c.blended_px),
            (skip.cost.draw_ops + 2, skip.cost.glyph_runs, skip.cost.texture_bytes, skip.cost.blended_px + 1_000)
        );
        // as compiled, English's credits layout is not larger, so the cost is the state's own
        assert_eq!(state_cost(skip), skip.cost);
        // a state with no credits layout of its own costs exactly itself
        let discs = compiled.lookup(0, Slot::Discs, true, false, false).expect("a compact state");
        assert_eq!(state_cost(discs), discs.cost);
    }

    #[test]
    fn the_lint_reports_a_state_once_when_every_language_linted_costs_it_the_same() {
        // the same language twice moves no cost: every state is one line, not two
        let en = || LintLang::for_tag("en").expect("en");
        let twice = crate::hud_format::lint::lint_report(CLASSIC, &ShippedMeasure, &[en(), en()], &Default::default());
        let once = crate::hud_format::lint::lint_report(CLASSIC, &ShippedMeasure, &[en()], &Default::default());
        assert_eq!(twice.costs.len(), once.costs.len(), "one line per state, not per state and language");
        assert!(twice.costs.iter().all(|l| l.lang.is_none()), "no language is named for a cost that is the same in all");
        // two different languages: a state whose tab plates differ is one line per language
        let de = LintLang::for_tag("de").expect("de");
        let both = crate::hud_format::lint::lint_report(CLASSIC, &ShippedMeasure, &[en(), de], &Default::default());
        assert!(both.costs.len() > once.costs.len() && both.costs.iter().any(|l| l.lang.as_deref() == Some("de")));
    }

    /// A text measurer half again as wide as the television's, so that text which fits the television does
    /// not fit it (a host whose SDL_ttf sums fractional advances is a few percent off, which the classic
    /// survives; this one makes the difference between the two measures visible in the fit findings too).
    struct Wider;

    impl plx_machine::machine::Measure for Wider {
        fn width(&self, s: &std::ffi::CStr, sz: i32, bold: bool) -> f32 {
            ShippedMeasure.width(s, sz, bold) * 1.5
        }
        fn cap_h(&self, sz: i32) -> f32 {
            ShippedMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            ShippedMeasure.line_h(sz)
        }
    }

    #[test]
    fn the_cost_is_priced_by_the_price_measure_and_everything_else_is_the_text_measures() {
        use crate::hud_format::lint::{lint_fit, lint_price, lint_report, lint_report_priced, testing::apply, Linted};
        // the reference with a chapter card too narrow for its name: a text-fit finding the measure decides
        let narrow = crate::hud_format::lint_text::INVALID
            .iter()
            .find(|c| c.code == "HUD-E0601" && c.base == Base::Reference)
            .expect("lint_text's chapter-card case");
        let text = apply(narrow.base, narrow.edits);
        let langs = || vec![LintLang::for_tag("pl").expect("pl"), LintLang::for_tag("en").expect("en")];
        let opts = Default::default();
        let tv = lint_report(&text, &ShippedMeasure, &langs(), &opts);
        let wide = lint_report(&text, &Wider, &langs(), &opts);
        let priced = lint_report_priced(&text, &Wider, &ShippedMeasure, &langs(), &opts);
        // the cost lines, and the cost verdict, are the price measure's
        assert_eq!(priced.costs, tv.costs);
        assert_ne!(wide.costs, tv.costs, "a wider text measure prices the same states differently");
        let verdict = |l: &Linted| l.findings.iter().filter(|f| f.code == "HUD-E1401").cloned().collect::<Vec<_>>();
        let rest = |l: &Linted| l.findings.iter().filter(|f| f.code != "HUD-E1401").cloned().collect::<Vec<_>>();
        assert_eq!(verdict(&priced), verdict(&tv));
        // every other finding is the text measure's
        assert_eq!(rest(&priced), rest(&wide));
        assert_ne!(rest(&wide), rest(&tv), "and the text measure does move the fit findings");
        // one measure for both is the plain report
        assert_eq!(lint_report_priced(&text, &Wider, &Wider, &langs(), &opts).costs, wide.costs);

        // the two passes alone: the fit pass has no cost verdict, whatever it is measured with; the price pass
        // has nothing BUT the cost verdict and the lines, and never reads the text measure
        let fit = lint_fit(&text, &Wider, &langs(), &opts);
        assert_eq!(fit, rest(&wide), "the fit pass is every finding but the cost");
        assert!(fit.iter().all(|f| f.code != "HUD-E1401"));
        let price = lint_price(&text, &ShippedMeasure, &langs());
        assert!(price.findings.iter().all(|f| f.code == "HUD-E1401"), "the price pass reports the cost verdict alone");
        assert_eq!((price.findings.clone(), price.costs.clone()), (verdict(&tv), tv.costs.clone()));
        assert_eq!(Linted::join(fit, price).findings, priced.findings, "joined, they are the whole report");
        // a package that does not parse has no cost to price: its findings are the fit pass's
        let broken = lint_price("{", &ShippedMeasure, &langs());
        assert!(broken.findings.is_empty() && broken.costs.is_empty());
        assert!(!lint_fit("{", &Wider, &langs(), &opts).is_empty());
    }

    /// every `HUD-E1401` of `text` linted in `langs` (all eleven when empty) with the television's advances
    fn e1401(text: &str, langs: &[&str]) -> Vec<Finding> {
        let langs: Vec<LintLang> = langs.iter().map(|t| LintLang::for_tag(t).expect("a shipped language")).collect();
        let langs = if langs.is_empty() { LintLang::shipped() } else { langs };
        let found = crate::hud_format::lint::lint_with(text, &ShippedMeasure, &langs, &Default::default());
        found.into_iter().filter(|f| f.code == "HUD-E1401").collect()
    }

    /// The reference with the second title line of [`INVALID`].
    fn reference_with_a_second_title_line() -> String {
        crate::hud_format::lint::testing::apply(Base::Reference, INVALID[0].edits)
    }

    #[test]
    fn a_state_over_its_class_is_one_finding_naming_each_term_over_and_three_contributors() {
        let found = e1401(&reference_with_a_second_title_line(), &["en"]);
        let at = |state: &str, slot: &str| {
            let hits: Vec<&Finding> = found.iter().filter(|f| (f.ctx.state.as_deref(), f.ctx.slot.as_deref()) == (Some(state), Some(slot))).collect();
            assert_eq!(hits.len(), 1, "one finding for `{state}` under `{slot}`: {:?}", found.iter().map(|f| (&f.ctx.state, &f.ctx.slot)).collect::<Vec<_>>());
            hits[0]
        };
        // the compact episode is at 13 draw ops; one more text op is 14 over 13, and it is the only term over
        let compact = at("compact", "discs");
        assert_eq!(compact.pointer, "/layout");
        assert!(compact.message.contains("a compact state over its ceiling: draw_ops 14 > 13. Heaviest ops: "), "{}", compact.message);
        // Chapters is at 55 draw ops and 25 glyph runs: two terms over, each as `value > ceiling`, in term order
        let chapters = at("chapters", "discs");
        let (head, tops) = chapters.message.split_once("Heaviest ops: ").expect("the contributors");
        assert!(head.contains("a one-section state over its ceiling: draw_ops 56 > 55, glyph_runs 26 > 25. "), "{head}");
        assert!(!head.contains("texture_bytes") && !head.contains("blended_px"), "only the terms over are named: {head}");
        // the three largest contributors, each by the pointer of the node that drew it
        assert_eq!(tops.split("; ").count(), 3, "the three largest contributors: {tops}");
        assert!(tops.split("; ").all(|t| t.starts_with("/layout/") && t.contains("(blended_px ")), "each by the pointer of its node: {tops}");
        assert!(!chapters.hint.is_empty());
        // Chapters and Info is a two-section state, over in its two terms as well; the taller title also
        // grows the scrim behind it, so the pixels blended are over too
        let both = at("chapters+info", "discs");
        let head = both.message.split_once(" Heaviest ops: ").expect("the contributors").0;
        assert!(head.contains("a two-sections state over its ceiling: draw_ops 63 > 62, glyph_runs 33 > 32, blended_px "), "{head}");
        assert!(head.ends_with(&format!("> {MAX_TWO_SECTIONS_BLENDED_PX}.")) && !head.contains("texture_bytes"), "{head}");
        // a state that has no title is not over: nothing but the findings of states that draw the title
        assert!(found.iter().all(|f| f.code == "HUD-E1401" && f.ctx.lang.as_deref() == Some("en")));
    }

    #[test]
    fn the_fixtures_every_state_of_which_fits_its_class_have_no_finding() {
        for (name, text) in [
            ("minimal", include_str!("fixtures/minimal.plxhud.json")),
            ("hero-center", include_str!("fixtures/hero-center.plxhud.json")),
            ("palette", include_str!("fixtures/palette.plxhud.json")),
            ("tabs", include_str!("fixtures/tabs.plxhud.json")),
        ] {
            let found = e1401(text, &[]);
            assert!(found.is_empty(), "{name}: {:?}", found.iter().map(|f| &f.message).collect::<Vec<_>>());
        }
    }

    #[test]
    fn every_state_of_classic_and_the_reference_is_within_its_class() {
        // The builtin classic must never be rejected by its own lint, and the reference is the design's own
        // example: no state of either, under any slot, in any of the eleven languages, is over a ceiling.
        let mut over = Vec::new();
        for (name, text) in PACKAGES {
            for f in e1401(text, &[]) {
                over.push(format!("{name}: {}", f.message));
            }
        }
        assert!(over.is_empty(), "{} states over their class:\n{}", over.len(), over.join("\n"));
    }
}
