//! **The HUD lint** (`docs/hud-plugins.md`, "LLM authoring" item 2): a package's TEXT in, every finding
//! out, in every language asked for. It is two passes over the one text, and `plxnative-sim --hud-lint` runs
//! both ([`lint_report_priced`]) and prints what they return through [`super::report`]; the registry's CI
//! reads the same JSON.
//!
//! **Two passes, two questions, two measures.**
//! - **The FIT pass** ([`lint_fit`]) is every check but the cost: the parse and validate findings, reachability,
//!   and the geometric checks over each language's compile. It is measured by the LIVE text measure
//!   (`measure`: the television's SDL_ttf there, the simulator's here), because whether a line fits is a
//!   question about the text the set draws.
//! - **The PRICE pass** ([`lint_price`]) is `HUD-E1401` and the cost lines alone, and nothing else. It is
//!   priced by one model on every side, `price`: the whole-pixel advances of `appfont.ttf` and
//!   `appfont-bold.ttf` with a line height of 1.21 x size
//!   ([`WholePixelMeasure`](plx_base::fontcov::advances::WholePixelMeasure)). The ceilings
//!   ([`lint_cost`]) are numbers taken with that model, so the same bytes get the same verdict wherever the
//!   lint runs, whatever each firmware's SDL_ttf does with hinting.
//!
//! **There is no entry point that prices with the fit measure.** A caller that has one measure for both
//! questions must say so by passing it twice to [`lint_report_priced`]; the tests' `lint`, `lint_with` and
//! `lint_report` do exactly that and exist under `cfg(test)` only. A driver that runs the passes on separate
//! ticks (the television's lint already takes a whole frame's budget) calls [`lint_fit`] and [`lint_price`]
//! one tick each and joins them with [`Linted::join`].
//!
//! **Pure.** No IO, no global, no clock, no `cfg(test)` fork in production code. The text measurers are the
//! existing [`Measure`] capability (the one [`compile`] takes, so every side
//! measures identically), and the language is an ARGUMENT: [`LintLang`] carries the tag and the app's
//! own strings looked up in THAT language's catalog (`LocaleContext::resolve` and the generated
//! `msg::*_in`), so nothing here reads `i18n::current()` or any other process-wide registry.
//!
//! **The stages of the fit pass**, in this order. The run reports EVERY finding ("LLM authoring" item 2: one
//! round fixes many), so the only stop is a package that does not exist: [`parse::parse`] returns
//! `Some(package)` exactly when it found no error, and [`compile`] is total over a package that parsed, so a
//! reachability or package-level error (stage 2) does not skip stage 3.
//! 1. the parse and validate findings ([`parse::parse`]); an error here leaves no package to go on with and
//!    ends the run (the price pass parses again, finds no package and is empty: its parse findings are the
//!    fit pass's);
//! 2. reachability and the package-level invariants ([`check`]); its findings are kept and the run goes on;
//! 3. [`compile`] once per language, then the six geometric checks over that language's [`Compiled`]
//!    (`lint_zorder` I4, `lint_safe` I5, `lint_band` I6, `lint_menus` I7, `lint_text` I10 and
//!    `lint_size` for `MIN_CONTROL_SIZE` and the compile's own item-keyed finding). Each check module
//!    exports exactly `pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>)` and walks
//!    [`LangCtx::states`] (state x slot) itself.
//!
//! The price pass is the third stage's other half: [`compile`] once per language with `price`, then
//! [`lint_cost::check`] and [`lint_cost::lines`] over that compile.
//!
//! A finding of stage 3 carries a [`Context`](super::Context): the language ([`LintLang::tag`], set here
//! on every finding a check did not already tag), the state ([`state_name`]) and the slot
//! ([`slot_name`]), and the rects and the measured text where they apply. A finding that is the same in
//! EVERY linted language (when more than one was asked for) is reported once, with no `lang`
//! ([`collapse`]). The result is [`report::sorted_owned`]: by pointer, ties by code, language, state,
//! slot and message.
//!
//! **A state's name** ([`state_name`]) is the open sections, `compact` (none), `chapters`, `info` or
//! `chapters+info`, then `,movie` when the valuation is a non-episode, `,no-chapters` when the title has no
//! chapters and `,paused` when `transport.paused` is an axis and holds: `compact`, `chapters,movie`,
//! `compact,movie,no-chapters,paused`. A condition the package does not name has its default value and
//! adds nothing, so the name is the same string whichever package it came from. **A slot's name**
//! ([`slot_name`]) is `discs`, `up_next` or `skip`.

use plx_machine::machine::Measure;
use plx_platform::i18n::{LocaleContext, Preference};
use plx_ui::Rect;

use super::compile::{
    compile, AppLabels, Compiled, CompileCtx, CompiledState, StateKey, OPEN_CHAPTERS, OPEN_INFO,
};
use super::parse::{self, Package};
use super::reach::{check, Slot};
use super::report::sorted_owned;
use super::lint_cost::StateCostLine;
use super::{lint_band, lint_cost, lint_menus, lint_safe, lint_size, lint_text, lint_zorder};
use super::{Context, Finding, NamedRect};

/// One language to lint in: its tag (`de`) and the app's own strings in it (tab labels, the Up Next and
/// Skip labels, the Info card's buttons). Built from the catalog of THAT language, never from the
/// process's current one.
#[derive(Clone, Debug)]
pub struct LintLang {
    /// The shipped language tag (`en`, `de`, `be`, ...): what `Context::lang` reports.
    pub tag: &'static str,
    pub labels: AppLabels,
}

impl LintLang {
    /// The eleven shipped languages, in the order of `locales/languages.json`.
    pub fn shipped() -> Vec<LintLang> {
        Preference::SHIPPED.into_iter().map(LintLang::of).collect()
    }

    /// The shipped language called `tag`, or `None` for a tag that is not one (`system` included).
    pub fn for_tag(tag: &str) -> Option<LintLang> {
        Preference::SHIPPED.into_iter().find(|p| p.tag() == tag).map(LintLang::of)
    }

    fn of(preference: Preference) -> LintLang {
        let cx = LocaleContext::resolve(preference, None, None, None, None);
        LintLang { tag: preference.tag(), labels: AppLabels::in_language(&cx) }
    }
}

/// What a lint run is told besides the text, the measurers and the languages (the FIT pass's; the price pass
/// takes none).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LintOptions {
    /// `CI_TEXT_SKEW` in px: `HUD-W0602` is reported for app text that fits with less than this to spare
    /// (`lint_text`). `0.0` until PR12 measures it, so locally no text is within the margin.
    pub text_skew: f32,
}

impl Default for LintOptions {
    fn default() -> Self {
        LintOptions { text_skew: lint_text::CI_TEXT_SKEW }
    }
}

/// What a whole lint run says: the findings of both passes, and (not findings) every state's cost against its
/// class's ceiling.
#[derive(Clone, Debug)]
pub struct Linted {
    /// the fit pass's findings and the price pass's `HUD-E1401`, sorted
    pub findings: Vec<Finding>,
    /// one line per state of the package, collapsed across the languages (see [`lint_cost::collapse_lines`]);
    /// empty when the package did not parse
    pub costs: Vec<StateCostLine>,
}

/// What the PRICE pass says: `HUD-E1401` alone, and the cost lines.
#[derive(Clone, Debug, Default)]
pub struct Priced {
    /// every state over its class's ceiling, as [`lint_cost::check`] reports it, sorted
    pub findings: Vec<Finding>,
    /// one line per state, collapsed across the languages; empty when the package did not parse
    pub costs: Vec<StateCostLine>,
}

impl Linted {
    /// The two passes of one text as one report: the fit pass's findings and the price pass's, sorted as
    /// [`sorted_owned`] sorts a single list, and the price pass's cost lines.
    pub fn join(fit: Vec<Finding>, priced: Priced) -> Linted {
        let mut findings = fit;
        findings.extend(priced.findings);
        Linted { findings: sorted_owned(&findings), costs: priced.costs }
    }
}

/// **The FIT pass**: every finding of the package in `text` but the cost verdict, linted in each of `langs`
/// with the live text measure `measure`, sorted (see the module doc). `langs` may be empty: then only the
/// language-free stages run.
pub fn lint_fit(text: &str, measure: &dyn Measure, langs: &[LintLang], opts: &LintOptions) -> Vec<Finding> {
    let parse::Parsed { package, mut findings } = parse::parse(text.as_bytes());
    let Some(package) = package else {
        return sorted_owned(&findings);
    };
    // No `has_error` stop here: the package parsed, so it compiles, and the geometric findings of this
    // round are reported with the reach and validate ones (see the module doc).
    findings.extend(check(&package).findings);
    let mut per_lang = Vec::with_capacity(langs.len());
    for lang in langs {
        let compiled = compile(&package, measure, CompileCtx { labels: lang.labels.clone() });
        let cx = LangCtx { package: &package, compiled: &compiled, measure, lang, opts };
        let mut out = Vec::new();
        lint_zorder::check(&cx, &mut out);
        lint_safe::check(&cx, &mut out);
        lint_band::check(&cx, &mut out);
        lint_menus::check(&cx, &mut out);
        lint_text::check(&cx, &mut out);
        lint_size::check(&cx, &mut out);
        tag_language(&mut out, lang.tag);
        per_lang.push(out);
    }
    findings.extend(collapse(per_lang));
    sorted_owned(&findings)
}

/// **The PRICE pass**: `HUD-E1401` and the cost lines of the package in `text`, in each of `langs`, priced
/// by `price` alone. The ceilings ([`lint_cost`]) are the numbers taken with the whole-pixel advances and a
/// line height of 1.21 x size, so `price` is
/// [`WholePixelMeasure`](plx_base::fontcov::advances::WholePixelMeasure) wherever this runs, never the live
/// text measure: a lint whose text measurer sums fractional advances (the simulator's SDL_ttf) or hints
/// differently (another firmware's) would price the same bytes a few pixels away and could refuse a package
/// that another set accepts. Nothing here reads `price` for a fit question.
///
/// The package is parsed again and an unparseable one gives an empty [`Priced`]: its parse findings are the
/// fit pass's.
pub fn lint_price(text: &str, price: &dyn Measure, langs: &[LintLang]) -> Priced {
    let Some(package) = parse::parse(text.as_bytes()).package else {
        return Priced::default();
    };
    let mut per_lang = Vec::with_capacity(langs.len());
    let mut per_lang_costs = Vec::with_capacity(langs.len());
    for lang in langs {
        let compiled = compile(&package, price, CompileCtx { labels: lang.labels.clone() });
        let mut out = Vec::new();
        lint_cost::check(lang, &compiled, &mut out);
        tag_language(&mut out, lang.tag);
        per_lang.push(out);
        per_lang_costs.push(lint_cost::lines(lang, &compiled));
    }
    Priced { findings: sorted_owned(&collapse(per_lang)), costs: lint_cost::collapse_lines(per_lang_costs) }
}

/// **Both passes**: [`lint_fit`] with `measure` and [`lint_price`] with `price`, joined ([`Linted::join`]).
/// What `--hud-lint` prints.
pub fn lint_report_priced(
    text: &str,
    measure: &dyn Measure,
    price: &dyn Measure,
    langs: &[LintLang],
    opts: &LintOptions,
) -> Linted {
    Linted::join(lint_fit(text, measure, langs, opts), lint_price(text, price, langs))
}

/// **Test only: one measure for both passes.** The tests have one measure (`ShippedMeasure`) and no firmware,
/// so they say it once; production passes two to [`lint_report_priced`].
#[cfg(test)]
pub fn lint_report(text: &str, measure: &dyn Measure, langs: &[LintLang], opts: &LintOptions) -> Linted {
    lint_report_priced(text, measure, measure, langs, opts)
}

/// Test only: the findings of [`lint_report`].
#[cfg(test)]
pub fn lint_with(text: &str, measure: &dyn Measure, langs: &[LintLang], opts: &LintOptions) -> Vec<Finding> {
    lint_report(text, measure, langs, opts).findings
}

/// Test only: [`lint_with`] with the default [`LintOptions`].
#[cfg(test)]
pub fn lint(text: &str, measure: &dyn Measure, langs: &[LintLang]) -> Vec<Finding> {
    lint_with(text, measure, langs, &LintOptions::default())
}

/// **Test only: the pipeline as it was before the two passes**, one compile per language with one measure and
/// the cost check inside the same loop as the geometric ones. It is the oracle `lint_tests` holds
/// [`lint_report`] against, so that splitting the lint changed no byte of what it says.
#[cfg(test)]
pub(super) fn single_pipeline(text: &str, measure: &dyn Measure, langs: &[LintLang], opts: &LintOptions) -> Linted {
    let parse::Parsed { package, mut findings } = parse::parse(text.as_bytes());
    let Some(package) = package else {
        return Linted { findings: sorted_owned(&findings), costs: Vec::new() };
    };
    findings.extend(check(&package).findings);
    let mut per_lang = Vec::with_capacity(langs.len());
    let mut per_lang_costs = Vec::with_capacity(langs.len());
    for lang in langs {
        let compiled = compile(&package, measure, CompileCtx { labels: lang.labels.clone() });
        let cx = LangCtx { package: &package, compiled: &compiled, measure, lang, opts };
        let mut out = Vec::new();
        lint_zorder::check(&cx, &mut out);
        lint_safe::check(&cx, &mut out);
        lint_band::check(&cx, &mut out);
        lint_menus::check(&cx, &mut out);
        lint_text::check(&cx, &mut out);
        lint_size::check(&cx, &mut out);
        lint_cost::check(lang, &compiled, &mut out);
        tag_language(&mut out, lang.tag);
        per_lang.push(out);
        per_lang_costs.push(lint_cost::lines(lang, &compiled));
    }
    findings.extend(collapse(per_lang));
    Linted { findings: sorted_owned(&findings), costs: lint_cost::collapse_lines(per_lang_costs) }
}

/// Everything one check needs, for one language: the parsed package, its compile in that language, the
/// measurer, the language and the options.
pub struct LangCtx<'a> {
    pub package: &'a Package,
    pub compiled: &'a Compiled,
    pub measure: &'a dyn Measure,
    pub lang: &'a LintLang,
    pub opts: &'a LintOptions,
}

/// One compiled state of the package, with the key it was compiled under.
#[derive(Clone, Copy)]
pub struct StateCtx<'a> {
    pub key: StateKey,
    pub state: &'a CompiledState,
}

impl<'a> LangCtx<'a> {
    /// Every compiled state, in `Compiled::states()` order (state x slot, valuation by valuation). A Skip
    /// state's credits layout is `state.skip_variant(SkipKind::Credits)`.
    pub fn states(&self) -> impl Iterator<Item = StateCtx<'a>> {
        states_of(self.compiled)
    }
}

/// Every compiled state of `compiled`, in `Compiled::states()` order: [`LangCtx::states`] for a check that
/// has no [`LangCtx`] (the price pass has a compile and a language, no package, measure or options).
pub fn states_of(compiled: &Compiled) -> impl Iterator<Item = StateCtx<'_>> {
    compiled.states().iter().map(|state| StateCtx { key: state.key, state })
}

/// The name of a state as the report spells it (see the module doc): `compact`, `chapters`, `info` or
/// `chapters+info`, then `,movie`, `,no-chapters`, `,paused` where the valuation is not the default.
pub fn state_name(key: StateKey) -> String {
    let mut name = match key.open {
        0 => "compact",
        OPEN_CHAPTERS => "chapters",
        OPEN_INFO => "info",
        _ => "chapters+info",
    }
    .to_owned();
    if !key.episode {
        name.push_str(",movie");
    }
    if !key.chapters {
        name.push_str(",no-chapters");
    }
    if key.paused {
        name.push_str(",paused");
    }
    name
}

/// The name of the slot a state compiled: `discs`, `up_next` or `skip`.
pub fn slot_name(key: StateKey) -> &'static str {
    match key.slot {
        Slot::Discs => "discs",
        Slot::UpNext => "up_next",
        Slot::Skip => "skip",
    }
}

/// The [`Context`] of a finding in `state` under `cx`: the language, the state and the slot. The check
/// adds the `rects` and the `text` it has.
pub fn ctx_for(cx: &LangCtx<'_>, state: &StateCtx<'_>) -> Context {
    ctx_in(cx.lang, state)
}

/// The [`Context`] of a finding in `state` in the language `lang`: [`ctx_for`] without the rest of a [`LangCtx`].
pub fn ctx_in(lang: &LintLang, state: &StateCtx<'_>) -> Context {
    Context {
        lang: Some(lang.tag.to_owned()),
        state: Some(state_name(state.key)),
        slot: Some(slot_name(state.key).to_owned()),
        ..Context::default()
    }
}

/// `r` called `name`, for a finding's `rects`.
pub fn named(name: &str, r: Rect) -> NamedRect {
    NamedRect { name: name.to_owned(), x: r.x, y: r.y, w: r.w, h: r.h }
}

/// Every finding that does not name a language is the language `tag`'s: a check need not set `lang` itself.
pub(super) fn tag_language(findings: &mut [Finding], tag: &str) {
    for finding in findings {
        if finding.ctx.lang.is_none() {
            finding.ctx.lang = Some(tag.to_owned());
        }
    }
}

/// `f` without its language: what two languages' findings are compared as.
fn language_free(f: &Finding) -> Finding {
    let mut free = f.clone();
    free.ctx.lang = None;
    free
}

/// The findings of every language (`per_lang[i]` is language `i`'s, each tagged with its `lang`) as one
/// list: a finding that appears in every language, identical but for `lang`, is kept once with `lang`
/// `None`; every other finding keeps its tag. With fewer than two languages nothing collapses (a single
/// language's findings keep their tag). A finding repeated k times in every language collapses k times.
pub(super) fn collapse(mut per_lang: Vec<Vec<Finding>>) -> Vec<Finding> {
    let mut shared = Vec::new();
    if per_lang.len() > 1 {
        let mut i = 0;
        while i < per_lang[0].len() {
            let probe = language_free(&per_lang[0][i]);
            let at: Option<Vec<usize>> = per_lang[1..]
                .iter()
                .map(|list| list.iter().position(|f| language_free(f) == probe))
                .collect();
            match at {
                Some(at) => {
                    for (list, position) in per_lang[1..].iter_mut().zip(at) {
                        list.remove(position);
                    }
                    per_lang[0].remove(i);
                    shared.push(probe);
                }
                None => i += 1,
            }
        }
    }
    shared.extend(per_lang.into_iter().flatten());
    shared
}

/// **The invalid-case support every check module's tests use.** An [`testing::InvalidCase`] is a base package
/// (`classic` or the reference), a few exact-once text edits, the code and pointer the edited package must
/// yield (plus any `also`), the languages to lint in and the text skew; [`testing::assert_case`] lints it with
/// the real font advances (`ShippedMeasure`) and asserts the SET of `(code, pointer)` pairs, nothing more and
/// nothing less, except that the cost verdict `HUD-E1401` is set aside unless the case is about it (see
/// [`testing::without_cost`]). Each check module holds its own `INVALID` and `UNIT_ONLY`; `lint_tests` unions
/// them and requires every lint code to appear.
#[cfg(test)]
pub(super) mod testing {
    use std::collections::BTreeSet;

    use plx_base::fontcov::advances::ShippedMeasure;

    use super::{lint_with, LintLang, LintOptions};
    use crate::hud_format::{codes, Finding, BUILTIN_CLASSIC};

    /// The package an edit starts from.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Base {
        /// the embedded `builtin:classic` ([`BUILTIN_CLASSIC`])
        Classic,
        /// `fixtures/reference.plxhud.json`, the design's reference package
        Reference,
        /// `fixtures/minimal.plxhud.json`: it places only `open_more`, so it folds the most rows of any fixture
        /// that lays out no section
        Minimal,
    }

    impl Base {
        pub fn text(self) -> &'static str {
            match self {
                Base::Classic => BUILTIN_CLASSIC,
                Base::Reference => include_str!("fixtures/reference.plxhud.json"),
                Base::Minimal => include_str!("fixtures/minimal.plxhud.json"),
            }
        }
    }

    /// One invalid package and what the lint must say about it.
    pub struct InvalidCase {
        /// names the case in a failure
        pub name: &'static str,
        /// the code the edit must yield ...
        pub code: &'static str,
        /// ... at this pointer
        pub pointer: &'static str,
        pub base: Base,
        /// `(find, replace)` pairs, each `find` occurring exactly once in the text as it stands, applied in order
        pub edits: &'static [(&'static str, &'static str)],
        /// the languages to lint in (tags); empty is all eleven
        pub langs: &'static [&'static str],
        /// `LintOptions::text_skew` for this case
        pub skew: f32,
        /// further `(code, pointer)` pairs the same edit is expected to yield
        pub also: &'static [(&'static str, &'static str)],
    }

    /// `base` with each `(find, replace)` applied in order. Fails the test when a `find` is not there
    /// exactly once, so a failure names the edit that moved.
    pub fn apply(base: Base, edits: &[(&str, &str)]) -> String {
        let mut text = base.text().to_owned();
        for (find, replace) in edits {
            let n = text.matches(find).count();
            assert_eq!(n, 1, "`{find}` must occur exactly once in the text, found {n}");
            text = text.replace(find, replace);
        }
        text
    }

    /// `found` without the cost verdict (`HUD-E1401`). A cost is a fact about a whole state and the ceilings
    /// are the television's exact maxima, with no headroom: an invalid case is an edit of the classic or the
    /// reference, and any edit that adds a pixel or a draw op to their heaviest state is over a ceiling,
    /// whatever fault the case is about. The tests that pin another rule therefore set the cost aside; the
    /// verdict has its own case in `lint_cost` and its own tests, and the classic and the reference are
    /// asserted within every ceiling there.
    pub fn without_cost(found: Vec<Finding>) -> Vec<Finding> {
        found.into_iter().filter(|f| f.code != codes::E1401.id).collect()
    }

    fn dump(findings: &[Finding]) -> String {
        findings
            .iter()
            .map(|f| format!("  {}@{:?} [{:?}]: {}", f.code, f.pointer, f.ctx.lang, f.message))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Lint the case and assert the set of `(code, pointer)` equals `{(code, pointer)}` plus `also`.
    pub fn assert_case(case: &InvalidCase) {
        let text = apply(case.base, case.edits);
        let langs: Vec<LintLang> = if case.langs.is_empty() {
            LintLang::shipped()
        } else {
            case.langs
                .iter()
                .map(|tag| {
                    LintLang::for_tag(tag)
                        .unwrap_or_else(|| panic!("case `{}`: `{tag}` is not a shipped language", case.name))
                })
                .collect()
        };
        let found = lint_with(&text, &ShippedMeasure, &langs, &LintOptions { text_skew: case.skew });
        let found = if case.code == codes::E1401.id { found } else { without_cost(found) };
        let got: BTreeSet<(String, String)> =
            found.iter().map(|f| (f.code.to_owned(), f.pointer.clone())).collect();
        let want: BTreeSet<(String, String)> = std::iter::once((case.code, case.pointer))
            .chain(case.also.iter().copied())
            .map(|(code, pointer)| (code.to_owned(), pointer.to_owned()))
            .collect();
        assert_eq!(got, want, "case `{}`: findings were\n{}", case.name, dump(&found));
    }
}
