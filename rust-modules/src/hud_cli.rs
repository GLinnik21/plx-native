//! Native HUD authoring tools. The process shell snapshots files and publishes output artifacts.
//! The command runner takes injected IO and measurers, and every renderer consumes the same verified
//! bundle representation used by installation and the player.

use plx_appkit::hud_format::bundle::{self, Decoded};
use plx_appkit::hud_format::lint::{lint_decoded, LintLang, LintOptions};
use plx_appkit::hud_format::{report, schema, schema_emit, BUILTIN_CLASSIC};
use plx_machine::machine::Measure;
use crate::app::hud_source::{self, Source};

const LINT: &str = "--hud-lint";
const SCHEMA: &str = "--hud-schema";
const EXPORT: &str = "--hud-export";
const PACK: &str = "--hud-pack";
const INFO: &str = "--hud-info";
const METRICS: &str = "--hud-text-metrics";
const INSPECT: &str = "--hud-inspect";
const EXIT_CLEAN: i32 = 0;
const EXIT_FINDINGS: i32 = 1;
const EXIT_USAGE: i32 = 2;
const USAGE: &str = "usage: plxnative-sim --hud-lint <file|directory> [--format text|json] [--lang all|<tag>] [--text-skew <profile>] | --hud-pack <directory> --out <file> [--format text|json] | --hud-schema manifest|hud --version 1.0 | --hud-export builtin:classic --out <file> | --hud-info | --hud-text-metrics [--lang all|<tag>] | --hud-inspect <file|directory> [--lang all|<tag>]";

pub fn wants(args: &[String]) -> bool {
    args.iter().any(|arg| matches!(arg.as_str(), LINT | SCHEMA | EXPORT | PACK | INFO | METRICS | INSPECT))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format { Text, Json }

enum Command {
    Lint { file: String, format: Format, langs: Vec<LintLang>, skew: Option<String> },
    Pack { directory: String, output: String, format: Format },
    Inspect { file: String, langs: Vec<LintLang> },
    Schema { manifest: bool },
    Export { id: String, output: String },
    Info,
    Metrics { langs: Vec<LintLang> },
}

fn value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str, what: &str) -> Result<&'a str, String> {
    match it.next() {
        Some(value) if !value.starts_with("--") => Ok(value),
        _ => Err(format!("{flag} needs {what}")),
    }
}

fn languages(tag: Option<&str>) -> Result<Vec<LintLang>, String> {
    match tag {
        None | Some("all") => Ok(LintLang::shipped()),
        Some(tag) => LintLang::for_tag(tag).map(|lang| vec![lang]).ok_or_else(|| {
            format!("--lang is all or one of {}, not {tag:?}", LintLang::shipped().iter().map(|lang| lang.tag).collect::<Vec<_>>().join(" "))
        }),
    }
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut mode: Option<(&str, String)> = None;
    let mut format = None;
    let mut lang = None;
    let mut skew = None;
    let mut output = None;
    let mut version = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let flag = arg.as_str();
        if matches!(flag, LINT | PACK | INSPECT | SCHEMA | EXPORT | INFO | METRICS) {
            let operand = if matches!(flag, INFO | METRICS) { String::new() } else {
                value(&mut it, flag, if flag == SCHEMA { "manifest or hud" } else if flag == EXPORT { "a package id" } else { "an input path" })?.to_owned()
            };
            if mode.replace((flag, operand)).is_some() { return Err("only one HUD tool mode is allowed".into()); }
            continue;
        }
        let target = match flag {
            "--format" => &mut format,
            "--lang" => &mut lang,
            "--text-skew" => &mut skew,
            "--out" => &mut output,
            "--version" => &mut version,
            _ => return Err(format!("unknown argument {arg:?}")),
        };
        if target.replace(value(&mut it, flag, "a value")?.to_owned()).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let (mode, operand) = mode.ok_or("nothing to do: give a HUD tool mode")?;
    if format.is_some() && !matches!(mode, LINT | PACK)
        || lang.is_some() && !matches!(mode, LINT | INSPECT | METRICS)
        || skew.is_some() && mode != LINT
        || output.is_some() && !matches!(mode, PACK | EXPORT)
        || version.is_some() && mode != SCHEMA
    { return Err("options are not accepted by this HUD tool mode".into()); }
    let format = match format.as_deref() {
        None | Some("text") => Format::Text,
        Some("json") => Format::Json,
        Some(other) => return Err(format!("--format is text or json, not {other:?}")),
    };
    match mode {
        LINT => Ok(Command::Lint { file: operand, format, langs: languages(lang.as_deref())?, skew }),
        PACK => Ok(Command::Pack { directory: operand, output: output.ok_or("--hud-pack requires --out")?, format }),
        INSPECT => Ok(Command::Inspect { file: operand, langs: languages(lang.as_deref())? }),
        METRICS => Ok(Command::Metrics { langs: languages(lang.as_deref())? }),
        INFO => Ok(Command::Info),
        EXPORT => Ok(Command::Export { id: operand, output: output.ok_or("--hud-export requires --out")? }),
        SCHEMA => {
            if version.as_deref() != Some("1.0") { return Err("--hud-schema requires --version 1.0".into()); }
            match operand.as_str() {
                "manifest" => Ok(Command::Schema { manifest: true }),
                "hud" => Ok(Command::Schema { manifest: false }),
                _ => Err("--hud-schema needs manifest or hud".into()),
            }
        }
        _ => unreachable!("mode was checked"),
    }
}

pub(crate) struct Io<'a> {
    pub read: &'a dyn Fn(&str) -> Result<Source, String>,
    pub text: &'a dyn Fn(&str) -> Result<String, String>,
    pub write: &'a dyn Fn(&str, &[u8]) -> Result<(), String>,
}

fn prepared(source: Source) -> (Option<Vec<u8>>, Decoded) {
    match source {
        Source::Archive(bytes) => {
            let decoded = bundle::read(&bytes, None);
            (Some(bytes), decoded)
        }
        Source::Directory(files) => match bundle::pack(&files) {
            Ok((bytes, decoded)) => (Some(bytes), decoded),
            Err(decoded) => (None, decoded),
        },
        Source::Rejected(decoded) => (None, decoded),
    }
}

fn usage(err: &mut String, reason: &str, help: bool) -> i32 {
    err.push_str("plxnative-sim: ");
    err.push_str(reason);
    if help { err.push_str(" ("); err.push_str(USAGE); err.push(')'); }
    err.push('\n');
    EXIT_USAGE
}

fn emit(out: &mut String, value: &serde_json::Value) {
    out.push_str(&format!("{value:#}\n"));
}

/// Run over immutable inputs. The shell injects filesystem operations and the real measurers.
pub(crate) fn run(
    args: &[String], io: &Io<'_>,
    measure: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    price: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    fonts: &dyn Fn() -> Result<std::collections::BTreeMap<String, String>, String>,
    out: &mut String, err: &mut String,
) -> i32 {
    let command = match parse(args) { Ok(command) => command, Err(reason) => return usage(err, &reason, true) };
    match command {
        Command::Info => { emit(out, &crate::hud_tools::info()); EXIT_CLEAN }
        Command::Schema { manifest } => {
            emit(out, &if manifest { schema_emit::emit_manifest(bundle::CURRENT) } else { schema_emit::emit(schema::CURRENT) });
            EXIT_CLEAN
        }
        Command::Export { id, output } => {
            if id != "builtin:classic" { return usage(err, &format!("unknown package {id:?} to export; known: builtin:classic"), false); }
            match (io.write)(&output, BUILTIN_CLASSIC) { Ok(()) => EXIT_CLEAN, Err(reason) => usage(err, &reason, false) }
        }
        Command::Metrics { langs } => {
            let measure = match measure() { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
            let fonts = match fonts() { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
            emit(out, &crate::hud_tools::metrics(measure.as_ref(), &langs, &fonts));
            EXIT_CLEAN
        }
        Command::Pack { directory, output, format } => {
            let source = match (io.read)(&directory) { Ok(source) => source, Err(reason) => return usage(err, &format!("cannot read {directory}: {reason}"), false) };
            if matches!(source, Source::Archive(_)) { return usage(err, "--hud-pack requires a source directory", false); }
            let (bytes, decoded) = prepared(source);
            if decoded.package.is_some() {
                if let Some(bytes) = &bytes {
                    if let Err(reason) = (io.write)(&output, bytes) { return usage(err, &reason, false); }
                }
            }
            let mut json = report::with_digest(report::to_json(&directory, &decoded.findings), decoded.sha256);
            json["bytes"] = bytes.as_ref().map_or(serde_json::Value::Null, |bytes| serde_json::Value::from(bytes.len()));
            json["manifest"] = decoded.manifest.as_ref().map_or(serde_json::Value::Null, |manifest| manifest.json().clone());
            match format {
                Format::Json => emit(out, &json),
                Format::Text => {
                    out.push_str(&report::text(&directory, &decoded.findings));
                    if let Some(bytes) = bytes.filter(|_| decoded.package.is_some()) {
                        out.push_str(&format!("Wrote {output} ({} bytes, SHA256 {})\n", bytes.len(), report::digest_hex(decoded.sha256.expect("packed artifact digest"))));
                    }
                }
            }
            if decoded.findings.is_empty() { EXIT_CLEAN } else { EXIT_FINDINGS }
        }
        Command::Inspect { file, langs } => {
            let source = match (io.read)(&file) { Ok(source) => source, Err(reason) => return usage(err, &format!("cannot read {file}: {reason}"), false) };
            let (_, decoded) = prepared(source);
            let Some(package) = &decoded.package else {
                let mut json = report::with_digest(report::to_json(&file, &decoded.findings), decoded.sha256);
                json["manifest"] = decoded.manifest.as_ref().map_or(serde_json::Value::Null, |manifest| manifest.json().clone());
                json["languages"] = serde_json::json!([]);
                emit(out, &json);
                return EXIT_FINDINGS;
            };
            let measure = match measure() { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
            let mut json = crate::hud_tools::inspect(package, measure.as_ref(), &langs);
            let mut findings = report::to_json(&file, &decoded.findings);
            json["findings"] = findings["findings"].take();
            emit(out, &json);
            if decoded.findings.is_empty() { EXIT_CLEAN } else { EXIT_FINDINGS }
        }
        Command::Lint { file, format, langs, skew } => {
            let source = match (io.read)(&file) { Ok(source) => source, Err(reason) => return usage(err, &format!("cannot read {file}: {reason}"), false) };
            let (_, decoded) = prepared(source);
            let (findings, costs) = if decoded.has_layout() {
                let measure = match measure() { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
                let price = match price() { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
                let mut opts = LintOptions::default();
                if let Some(path) = skew {
                    let text = match (io.text)(&path) { Ok(text) => text, Err(reason) => return usage(err, &reason, false) };
                    opts.text_skew_by_size = match crate::hud_tools::skew_profile(&text) { Ok(value) => value, Err(reason) => return usage(err, &reason, false) };
                }
                let linted = lint_decoded(&decoded, measure.as_ref(), price.as_ref(), &langs, &opts);
                (linted.findings, linted.costs)
            } else { (decoded.findings, Vec::new()) };
            match format {
                Format::Text => out.push_str(&report::text_with_costs(&file, &findings, &costs)),
                Format::Json => emit(out, &report::with_digest(report::to_json_with_costs(&file, &findings, &costs), decoded.sha256)),
            }
            if findings.is_empty() { EXIT_CLEAN } else { EXIT_FINDINGS }
        }
    }
}

fn ttf_measure() -> Result<Box<dyn Measure>, String> {
    plx_gfx::text::init_fonts_headless().map_err(|reason| format!("cannot load fonts: {reason}"))?;
    Ok(Box::new(plx_gfx::text::TtfMeasure))
}

fn price_measure() -> Result<Box<dyn Measure>, String> {
    plx_gfx::text::read_price_model(&plx_gfx::text::app_font_path(false), &plx_gfx::text::app_font_path(true))
        .map(|model| Box::new(model) as Box<dyn Measure>).map_err(|reason| format!("cannot load fonts: {reason}"))
}

pub fn main_from(args: &[String]) -> i32 {
    use std::io::Write;
    let (mut out, mut err) = (String::new(), String::new());
    let io = Io { read: &hud_source::read_source, text: &|path| std::fs::read_to_string(path).map_err(|error| error.to_string()), write: &hud_source::write_artifact };
    let code = run(args, &io, &ttf_measure, &price_measure, &crate::hud_tools::font_hashes, &mut out, &mut err);
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().write_all(err.as_bytes());
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_appkit::hud_format::lint::Linted;
    use plx_appkit::hud_format::lint_cost::StateCostLine;
    use plx_appkit::hud_format::Finding;
    use plx_base::fontcov::advances::ShippedMeasure;
    use std::cell::Cell;
    use std::ffi::CStr;
    use std::rc::Rc;

    const BUILTIN_CLASSIC: &str = plx_appkit::hud_format::CLASSIC_HUD;
    #[cfg(feature = "hostsim")]
    const CLASSIC_FIXTURE: &str = include_str!("../appkit/src/hud_format/fixtures/classic/hud.json");
    const REFERENCE_FIXTURE: &str = include_str!("../appkit/src/hud_format/fixtures/reference/hud.json");

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    /// The whole lint with the tests' one measure, `ShippedMeasure`, for both passes.
    fn lint_report(text: &str, langs: &[LintLang], opts: &LintOptions) -> Linted {
        let bytes = plx_appkit::hud_format::fixtures::with_hud_resources("reference", text);
        let decoded = bundle::read(&bytes, None);
        lint_decoded(&decoded, &ShippedMeasure, &ShippedMeasure, langs, opts)
    }

    fn expected_json(file: &str, hud: &str, findings: &[Finding], costs: &[StateCostLine]) -> String {
        let bytes = plx_appkit::hud_format::fixtures::with_hud_resources("reference", hud);
        let digest = bundle::read(&bytes, None).sha256;
        format!("{:#}\n", report::with_digest(report::to_json_with_costs(file, findings, costs), digest))
    }

    /// The findings of [`lint_report`] in every language, with the default options.
    fn lint(text: &str, langs: &[LintLang]) -> Vec<Finding> {
        lint_report(text, langs, &LintOptions::default()).findings
    }

    /// The shipped faces' real advances, the measurer the lint goldens use.
    fn shipped() -> Result<Box<dyn Measure>, String> {
        Ok(Box::new(ShippedMeasure))
    }

    /// A measurer factory that fails the test if it is called.
    fn never() -> Result<Box<dyn Measure>, String> {
        panic!("the measurer was built where it must not be");
    }

    /// `ShippedMeasure` that counts the widths it answers, to see how much work a run did.
    struct Counting(Rc<Cell<usize>>);

    impl Measure for Counting {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.0.set(self.0.get() + 1);
            ShippedMeasure.width(s, sz, bold)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            ShippedMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            ShippedMeasure.line_h(sz)
        }
    }

    fn serve(text: &'static str) -> impl Fn(&str) -> Result<String, String> {
        move |_| Ok(text.to_owned())
    }

    /// Run with a result of `(exit code, stdout, stderr)`; the cost is priced by [`shipped`], the same
    /// advances as `measure`'s in every test that passes `shipped`.
    fn go(
        list: &[&str],
        read: &dyn Fn(&str) -> Result<String, String>,
        measure: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    ) -> (i32, String, String) {
        go_priced(list, read, measure, &shipped)
    }

    /// [`go`] with the pricing measurer given.
    fn go_priced(
        list: &[&str],
        read: &dyn Fn(&str) -> Result<String, String>,
        measure: &dyn Fn() -> Result<Box<dyn Measure>, String>,
        price: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (String::new(), String::new());
        let source = |path: &str| read(path).map(|text| Source::Archive(plx_appkit::hud_format::fixtures::with_hud_resources("reference", &text)));
        let io = Io { read: &source, text: read, write: &|_, _| Ok(()) };
        let code = run(&args(list), &io, measure, price, &|| panic!("font identities requested outside metrics"), &mut out, &mut err);
        (code, out, err)
    }

    #[test]
    fn registry_info_and_inspection_use_only_their_injected_inputs() {
        let (code,out,err) = go(&["--hud-info"], &|_| panic!("info read a file"), &never);
        assert_eq!((code,err.as_str()),(0,""));
        let info: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(info["languages"].as_array().unwrap().len(),12);
        let (code,out,err) = go(&["--hud-inspect","ref.json","--lang","de"], &serve(REFERENCE_FIXTURE), &shipped);
        assert_eq!((code,err.as_str()),(0,""));
        let info: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(info["languages"][0]["lang"],"de");
        assert!(info["languages"][0]["states"].as_array().unwrap().iter().any(|s|
            s["controls"].as_array().unwrap().iter().any(|c| c["command"] == "open_tracks")));
        for flags in [vec!["--hud-info","--text-skew","p.json"],vec!["--hud-inspect","p.json","--format","json"]] {
            assert_eq!(go(&flags,&|_| panic!("bad arguments read a file"),&never).0,2);
        }
    }

    #[test]
    fn inspect_retains_bundle_warnings_alongside_the_admitted_manifest_and_languages() {
        let before = r#""face": "fwd30" }"#;
        let after = r#""face": "fwd30" },
          { "id": "skip_back", "command": "seek", "by": "setting", "dir": "back", "widget": "disc", "size": 52,
            "face": { "default": "back10", "by_interval": { "10": "back10", "30": "fwd30" } } }"#;
        assert_eq!(REFERENCE_FIXTURE.matches(before).count(), 1);
        let hud = REFERENCE_FIXTURE.replacen(before, after, 1);
        let read = |_: &str| Ok(hud.clone());
        let bytes = plx_appkit::hud_format::fixtures::with_hud_resources("reference", &hud);
        let decoded = bundle::read(&bytes, None);
        assert!(decoded.package.is_some(), "a warning does not prevent typed bundle admission");
        assert_eq!(decoded.findings.len(), 1);
        assert_eq!(decoded.findings[0].code, "HUD-W0903");
        let (code, out, err) = go(&["--hud-inspect", "warning.plxhud", "--lang", "en"], &read, &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        let report: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(report["findings"], report::to_json("warning.plxhud", &decoded.findings)["findings"]);
        assert_eq!(report["findings"][0]["file"], "hud.json");
        assert_eq!(report["package_sha256"], report::digest_hex(decoded.sha256.unwrap()));
        assert_eq!(report["manifest"]["hud"]["format"], "1.0");
        assert_eq!(report["languages"][0]["lang"], "en");
    }

    #[test]
    fn registry_metrics_inject_font_identity_and_never_load_the_price_model() {
        let (mut out,mut err) = (String::new(),String::new());
        let fonts = std::collections::BTreeMap::from([("appfont.ttf".into(),"identity".into())]);
        let code = run(&args(&["--hud-text-metrics","--lang","en"]),
            &Io { read: &|_| panic!("metrics read a package"), text: &|_| panic!("metrics read text"), write: &|_, _| panic!("metrics wrote a file") },&shipped,&never,&|| Ok(fonts.clone()),&mut out,&mut err);
        assert_eq!((code,err.as_str()),(0,""));
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["font_hashes"]["appfont.ttf"],"identity");
        assert!(value["metrics"].as_array().unwrap().iter().all(|row| row["lang"] == "en"));
    }

    /// How many widths a lint of the classic package in `lang_args` asks the measurer for.
    fn widths(lang_args: &[&str]) -> usize {
        let calls = Rc::new(Cell::new(0));
        let factory = {
            let calls = Rc::clone(&calls);
            move || -> Result<Box<dyn Measure>, String> { Ok(Box::new(Counting(Rc::clone(&calls)))) }
        };
        let mut list = vec!["--hud-lint", "classic.plxhud"];
        list.extend_from_slice(lang_args);
        let (code, _, err) = go(&list, &serve(BUILTIN_CLASSIC), &factory);
        assert_eq!((code, err.as_str()), (0, ""), "{list:?}");
        calls.get()
    }

    /// The cost lines of a report: every line that is not a finding (`<file>: cost ...`).
    fn cost_lines(out: &str) -> Vec<&str> {
        out.lines().filter(|l| l.contains(": cost ")).collect()
    }

    #[test]
    fn hud_lint_exits_0_for_the_builtin_classic_and_prints_its_cost_lines_and_nothing_else() {
        let (code, out, err) = go(&["--hud-lint", "classic.plxhud"], &serve(BUILTIN_CLASSIC), &shipped);
        assert_eq!((code, err.as_str()), (0, ""));
        let lines = cost_lines(&out);
        assert!(!lines.is_empty(), "{out}");
        assert_eq!(lines.len(), out.lines().count(), "a clean package prints cost lines and no finding: {out}");
        assert!(lines.iter().all(|l| l.starts_with("classic.plxhud: cost ")), "{out}");
        let (code, out, err) = go(
            &["--hud-lint", "classic.plxhud", "--format", "json", "--lang", "all"],
            &serve(BUILTIN_CLASSIC),
            &shipped,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        let costs = lint_report(BUILTIN_CLASSIC, &LintLang::shipped(), &LintOptions::default()).costs;
        assert_eq!(out, expected_json("classic.plxhud", BUILTIN_CLASSIC, &[], &costs));
        assert!(out.contains("\"findings\": []"), "{out}");
        assert!(out.contains("\"costs\": ["), "{out}");
    }

    #[test]
    fn a_cost_line_names_the_state_its_class_and_each_term_against_its_ceiling() {
        let (code, out, _) = go(&["--hud-lint", "c.json", "--lang", "en"], &serve(BUILTIN_CLASSIC), &shipped);
        assert_eq!(code, 0);
        let film = cost_lines(&out)
            .into_iter()
            .find(|l| l.contains(": cost compact,movie discs (compact):"))
            .unwrap_or_else(|| panic!("the film's line: {out}"));
        assert!(film.contains("draw_ops 9/13, glyph_runs 6/9, texture_bytes 49152/256120, blended_px 945408/985088"), "{film}");
        assert!(film.ends_with("[en]"), "one language keeps its tag: {film}");
    }

    #[test]
    fn hud_lint_exits_1_for_a_text_that_fails_to_parse() {
        let (code, out, err) = go(&["--hud-lint", "broken.plxhud"], &serve("{"), &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        assert!(out.starts_with("hud.json#"), "{out}");
        assert!(out.contains(" error HUD-E"), "{out}");
        assert!(out.ends_with('\n'), "{out}");
    }

    #[test]
    fn hud_lint_exits_1_for_a_json_document_that_is_not_a_package() {
        // Well-formed JSON that is no package: it fails to PARSE (the required keys are missing), so the
        // report is parse findings and no crash. The converse, a package that parses and compiles and
        // fails a geometric check, is the next test.
        let (code, out, _) = go(&["--hud-lint", "empty.plxhud"], &serve("{}"), &shipped);
        assert_eq!(code, 1, "{out}");
        assert!(!out.is_empty());
    }

    /// **Exit 1 from a stage-3 finding, with the JSON `context`.** The reference with its info disc drawn at
    /// 20 px (`lint_size`'s own `HUD-E0603` case): the package parses, validates, is reachable and compiles,
    /// and then fails the minimum-control-size check in one language. The unedited reference is clean, so
    /// the one finding is the edit's; its JSON carries the `context` object (language, slot, state, rects)
    /// that a parse finding does not have.
    #[test]
    fn hud_lint_exits_1_for_a_package_that_compiles_and_fails_a_geometric_check() {
        const BEFORE: &str = r#""section": "info", "widget": "disc", "size": 52,"#;
        const AFTER: &str = r#""section": "info", "widget": "disc", "size": 20,"#;
        assert_eq!(REFERENCE_FIXTURE.matches(BEFORE).count(), 1, "the edit must find its text exactly once");
        let edited = REFERENCE_FIXTURE.replacen(BEFORE, AFTER, 1);
        let en = [LintLang::for_tag("en").expect("en is shipped")];
        let reads = |text: String| move |_: &str| -> Result<String, String> { Ok(text.clone()) };

        // the base is clean through `run`, so the exit 1 below is the edit's
        let (code, out, err) = go(
            &["--hud-lint", "ref.plxhud", "--lang", "en"],
            &reads(REFERENCE_FIXTURE.to_owned()),
            &shipped,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(cost_lines(&out).len(), out.lines().count(), "no finding, only cost lines: {out}");

        let linted = lint_report(&edited, &en, &LintOptions::default());
        let expected = linted.findings.clone();
        assert_eq!(expected.len(), 1, "{}", report::text("small-disc.plxhud", &expected));
        assert_eq!(expected[0].code, "HUD-E0603");
        let (code, out, err) = go(
            &["--hud-lint", "small-disc.plxhud", "--format", "json", "--lang", "en"],
            &reads(edited.clone()),
            &shipped,
        );
        assert_eq!((code, err.as_str()), (1, ""));
        assert_eq!(out, expected_json("small-disc.plxhud", &edited, &expected, &linted.costs));
        assert!(out.contains("\"context\""), "a geometric finding carries its context: {out}");
        assert!(out.contains("\"code\": \"HUD-E0603\""), "{out}");
        assert!(out.contains("\"protocol\": 1"), "{out}");

        let (code, text, err) =
            go(&["--hud-lint", "small-disc.plxhud", "--lang", "en"], &reads(edited), &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        assert_eq!(text, report::text_with_costs("small-disc.plxhud", &expected, &linted.costs));
        assert!(text.contains(" error HUD-E0603"), "{text}");
    }

    #[test]
    fn every_usage_error_exits_2_with_one_line_on_stderr_and_nothing_on_stdout() {
        let unreadable = |_: &str| -> Result<String, String> { Err("No such file or directory".to_owned()) };
        let cases: &[(&[&str], &str)] = &[
            (&[], "nothing to do"),
            (&["--hud-lint"], "--hud-lint needs an input path"),
            (&["--hud-lint", "--format", "json"], "--hud-lint needs an input path"),
            (&["--hud-lint", "p.json", "--bogus"], "unknown argument \"--bogus\""),
            (&["--hud-lint", "p.json", "extra.json"], "unknown argument \"extra.json\""),
            (&["--hud-lint", "p.json", "--format"], "--format needs a value"),
            (&["--hud-lint", "p.json", "--format", "xml"], "--format is text or json, not \"xml\""),
            (&["--hud-lint", "p.json", "--format", "json", "--format", "text"], "--format given twice"),
            (&["--hud-lint", "p.json", "--lang"], "--lang needs a value"),
            (&["--hud-lint", "p.json", "--lang", "xx"], "--lang is all or one of en es be ca fr it pt de hu ru pl nl, not"),
            (&["--hud-lint", "p.json", "--lang", "system"], "not \"system\""),
            (&["--hud-lint", "p.json", "--lang", "de", "--lang", "fr"], "--lang given twice"),
            (&["--hud-export"], "--hud-export needs a package id"),
            (&["--hud-export", "nope", "--out", "out.plxhud"], "unknown package \"nope\" to export; known: builtin:classic"),
            (&["--hud-schema", "hud", "--version", "1.0", "--hud-export", "builtin:classic", "--out", "out.plxhud"], "only one HUD tool mode"),
            (&["--hud-lint", "p.json", "--hud-schema", "hud", "--version", "1.0"], "only one HUD tool mode"),
            (&["--hud-schema", "extra", "--version", "1.0"], "--hud-schema needs manifest or hud"),
            (&["--hud-schema", "hud", "--format", "json", "--version", "1.0"], "options are not accepted by this HUD tool mode"),
            (&["--hud-export", "builtin:classic", "--lang", "de"], "options are not accepted by this HUD tool mode"),
        ];
        for (list, reason) in cases {
            let (code, out, err) = go(list, &unreadable, &never);
            assert_eq!(code, 2, "{list:?}: {err}");
            assert_eq!(out, "", "{list:?}");
            assert!(err.starts_with("plxnative-sim: "), "{list:?}: {err}");
            assert!(err.contains(reason), "{list:?}: expected {reason:?} in {err}");
            assert_eq!(err.matches('\n').count(), 1, "{list:?}: one line: {err:?}");
            assert!(err.ends_with('\n'), "{list:?}: {err:?}");
        }
    }

    #[test]
    fn an_unreadable_file_is_exit_2_and_names_the_path_and_the_reason() {
        let unreadable = |_: &str| -> Result<String, String> { Err("No such file or directory".to_owned()) };
        let (code, out, err) = go(&["--hud-lint", "gone.plxhud"], &unreadable, &never);
        assert_eq!((code, out.as_str()), (2, ""));
        assert_eq!(err, "plxnative-sim: cannot read gone.plxhud: No such file or directory\n");
    }

    #[test]
    fn a_measurer_that_cannot_be_built_is_exit_2_and_never_a_report() {
        let no_fonts = || -> Result<Box<dyn Measure>, String> { Err("cannot load fonts: no appfont.ttf".to_owned()) };
        let (code, out, err) = go(&["--hud-lint", "p.json"], &serve(BUILTIN_CLASSIC), &no_fonts);
        assert_eq!((code, out.as_str()), (2, ""));
        assert_eq!(err, "plxnative-sim: cannot load fonts: no appfont.ttf\n");
        // the pricing measurer too: no cost is read off a measure that is not the television's
        let (code, out, err) = go_priced(&["--hud-lint", "p.json"], &serve(BUILTIN_CLASSIC), &shipped, &no_fonts);
        assert_eq!((code, out.as_str()), (2, ""));
        assert_eq!(err, "plxnative-sim: cannot load fonts: no appfont.ttf\n");
    }

    /// A measurer a fraction wider than the television's, standing for the simulator's fractional sums.
    struct Wider;

    impl Measure for Wider {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            ShippedMeasure.width(s, sz, bold) * 1.07
        }
        fn cap_h(&self, sz: i32) -> f32 {
            ShippedMeasure.cap_h(sz)
        }
        fn line_h(&self, sz: i32) -> f32 {
            ShippedMeasure.line_h(sz)
        }
    }

    #[test]
    fn the_cost_is_priced_by_the_price_measurer_and_the_text_is_fitted_by_the_other() {
        let wider = || -> Result<Box<dyn Measure>, String> { Ok(Box::new(Wider)) };
        let list = ["--hud-lint", "classic.plxhud", "--lang", "all"];
        // the television's own prices, from a run that measures everything with its advances
        let (code, tv, _) = go_priced(&list, &serve(BUILTIN_CLASSIC), &shipped, &shipped);
        assert_eq!(code, 0, "{tv}");
        // the text fitted by a wider measurer, the cost priced by the television's: the same cost lines
        let (_, priced, _) = go_priced(&list, &serve(BUILTIN_CLASSIC), &wider, &shipped);
        assert_eq!(cost_lines(&priced), cost_lines(&tv));
        // and the converse, so the assertion above can fail: the wider measurer alone prices differently
        let (_, wide, _) = go_priced(&list, &serve(BUILTIN_CLASSIC), &wider, &wider);
        assert_ne!(cost_lines(&wide), cost_lines(&tv), "a wider measurer moves a cost");
    }

    #[test]
    fn the_file_is_read_by_the_name_given_and_the_measurer_is_built_once() {
        let read = Cell::new(Vec::<String>::new());
        let reader = |path: &str| -> Result<String, String> {
            let mut seen = read.take();
            seen.push(path.to_owned());
            read.set(seen);
            Ok(BUILTIN_CLASSIC.to_owned())
        };
        let (built, priced) = (Cell::new(0), Cell::new(0));
        let factory = || -> Result<Box<dyn Measure>, String> {
            built.set(built.get() + 1);
            Ok(Box::new(ShippedMeasure))
        };
        let price = || -> Result<Box<dyn Measure>, String> {
            priced.set(priced.get() + 1);
            Ok(Box::new(ShippedMeasure))
        };
        let (code, _, _) = go_priced(&["--hud-lint", "dir/my hud.json", "--lang", "all"], &reader, &factory, &price);
        assert_eq!(code, 0);
        assert_eq!(read.take(), ["dir/my hud.json"]);
        assert_eq!((built.get(), priced.get()), (1, 1), "each measurer is built once, for all twelve languages");
    }

    #[test]
    fn lang_de_lints_one_language_and_lang_all_lints_them_all() {
        let de = widths(&["--lang", "de"]);
        let all = widths(&["--lang", "all"]);
        assert!(de > 0, "a compile measures text");
        assert_eq!(widths(&[]), all, "all is the default");
        let tags = ["en", "es", "be", "ca", "fr", "it", "pt", "de", "hu", "ru", "pl", "nl"];
        let each: usize = tags.iter().map(|tag| widths(&["--lang", tag])).sum();
        assert_eq!(all, each, "the all run is the twelve single-language runs");
        assert!(all > de);
        assert!(all >= 5 * de, "twelve languages, not two: all {all}, de {de}");
    }

    #[test]
    fn json_output_is_report_json_text_of_the_findings_lint_returns() {
        let bytes = "{\n  \"format\": \"1.0\" ,}";
        let expected = lint(bytes, &LintLang::shipped());
        assert!(!expected.is_empty());
        let (code, out, err) = go(&["--hud-lint", "bad.plxhud", "--format", "json"], &serve(bytes), &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        // a package that does not parse has no states: an empty `costs`
        assert_eq!(out, expected_json("bad.plxhud", bytes, &expected, &[]));
        assert!(out.contains("\"costs\": []"), "{out}");
        assert!(out.starts_with("{\n"), "{out}");
        assert!(out.contains("\"file\": \"bad.plxhud\""), "{out}");
        assert!(out.contains("\"protocol\": 1"), "{out}");
        assert!(out.ends_with("}\n"), "{out}");
        let (code, text, _) = go(&["--hud-lint", "bad.plxhud", "--format", "text"], &serve(bytes), &shipped);
        assert_eq!(code, 1);
        assert_eq!(text, report::text("bad.plxhud", &expected), "no cost line without a package");
        let (_, default, _) = go(&["--hud-lint", "bad.plxhud"], &serve(bytes), &shipped);
        assert_eq!(default, text, "text is the default format");
    }

    #[test]
    fn pack_publishes_once_and_inspection_identifies_the_same_archive_and_manifest() {
        let reads = Cell::new(0);
        let source = |path: &str| {
            assert_eq!(path, "my-source");
            reads.set(reads.get() + 1);
            Ok(Source::Directory(plx_appkit::hud_format::fixtures::files("minimal")))
        };
        let written = std::cell::RefCell::new(Vec::new());
        let writer = |path: &str, bytes: &[u8]| {
            written.borrow_mut().push((path.to_owned(), bytes.to_vec()));
            Ok(())
        };
        let (mut out, mut err) = (String::new(), String::new());
        let io = Io { read: &source, text: &|_| panic!("pack read text profile"), write: &writer };
        let code = run(&args(&["--hud-pack", "my-source", "--out", "result.plxhud", "--format", "json"]), &io, &never, &never, &|| panic!("pack read font identities"), &mut out, &mut err);
        assert_eq!((code, err.as_str(), reads.get()), (0, "", 1));
        let packed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let written = written.into_inner();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "result.plxhud");
        let bytes = &written[0].1;
        assert_eq!(packed["protocol"], 1);
        assert_eq!(packed["bytes"], bytes.len());
        assert_eq!(packed["package_sha256"], report::digest_hex(plx_base::sha256::sha256(bytes)));
        assert_eq!(packed["findings"], serde_json::json!([]));
        let inspect = |path: &str| { assert_eq!(path, "result.plxhud"); Ok(Source::Archive(bytes.clone())) };
        let io = Io { read: &inspect, text: &|_| panic!("inspect read text"), write: &|_, _| panic!("inspect wrote an archive") };
        out.clear();
        let code = run(&args(&["--hud-inspect", "result.plxhud", "--lang", "en"]), &io, &shipped, &never, &|| panic!("inspect read font identities"), &mut out, &mut err);
        assert_eq!((code, err.as_str()), (0, ""));
        let inspected: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(inspected["manifest"], packed["manifest"]);
        assert_eq!(inspected["package_sha256"], packed["package_sha256"]);
        assert_eq!(inspected["languages"][0]["lang"], "en");
    }

    #[test]
    fn malformed_archive_reports_its_complete_digest_without_loading_fonts_or_writing_output() {
        let bytes = b"a complete malformed archive";
        let io = Io { read: &|_| Ok(Source::Archive(bytes.to_vec())), text: &|_| panic!("read text"), write: &|_, _| panic!("wrote output") };
        for mode in [LINT, INSPECT] {
            let mut flags = vec![mode, "broken.plxhud"];
            if mode == LINT { flags.extend(["--format", "json"]); }
            let (mut out, mut err) = (String::new(), String::new());
            let code = run(&args(&flags), &io, &never, &never, &|| panic!("font identity"), &mut out, &mut err);
            assert_eq!((code, err.as_str()), (1, ""));
            let report: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(report["package_sha256"], report::digest_hex(plx_base::sha256::sha256(bytes)));
            let finding = &report["findings"][0];
            assert!(finding["file"].is_null());
            assert_eq!(finding["path"], "");
        }
    }

    #[test]
    fn each_document_schema_uses_the_native_emitter_without_loading_files_or_fonts() {
        for (kind, expected) in [("manifest", schema_emit::emit_manifest(bundle::CURRENT)), ("hud", schema_emit::emit(schema::CURRENT))] {
            let (code, out, err) = go(&["--hud-schema", kind, "--version", "1.0"], &|_| panic!("schema read input"), &never);
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out, format!("{expected:#}\n"));
        }
    }

    #[test]
    fn export_writes_the_exact_embedded_archive_without_reading_files_or_loading_fonts() {
        let written = std::cell::RefCell::new(Vec::new());
        let writer = |path: &str, bytes: &[u8]| {
            written.borrow_mut().push((path.to_owned(), bytes.to_vec()));
            Ok(())
        };
        let io = Io { read: &|_| panic!("export read input"), text: &|_| panic!("export read text"), write: &writer };
        let (mut out, mut err) = (String::new(), String::new());
        let code = run(&args(&["--hud-export", "builtin:classic", "--out", "classic.plxhud"]), &io, &never, &never, &|| panic!("font identity"), &mut out, &mut err);
        assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
        assert_eq!(written.into_inner(), [("classic.plxhud".to_owned(), super::BUILTIN_CLASSIC.to_vec())]);
    }

    /// **The real measurer, run: the shipped faces opened with no window and no GL context.** Every other
    /// test here injects `ShippedMeasure`, so nothing else runs [`ttf_measure`], `init_fonts_headless` or
    /// the glyph-band branch of `TtfMeasure::cap_h`/`line_h`. That branch is the one that matters: a
    /// `line_h`/`cap_h` that went through `text_tex` would upload a GL texture with no context and take
    /// the whole binary down with a SIGSEGV, which CI would not see and `--hud-lint` would.
    ///
    /// **In a child process, and only under `hostsim`.** The font switch (`TEXT_OK`), the resolved
    /// application directory and the face paths are each set once per process and never taken back, so a
    /// test that shares a process with any other width or path test would grade that test's state (and
    /// leave this one's behind). `PLXNATIVE_APP_DIR` is honoured only by a `hostsim` build
    /// (`paths::ENV_STEERABLE`), and the child must START with it: the directory is resolved on first
    /// use. A crash in the child is the parent's assertion failure, not a dead test binary.
    #[cfg(feature = "hostsim")]
    #[test]
    fn the_headless_fonts_measure_and_lint_the_classic_without_a_gl_context() {
        const NAME: &str = "the_headless_fonts_measure_and_lint_the_classic_without_a_gl_context";
        let pkg = concat!(env!("CARGO_MANIFEST_DIR"), "/../pkg");
        if !plx_base::testlock::fresh_process_with_env(module_path!(), NAME, &[("PLXNATIVE_APP_DIR", pkg)]) {
            return;
        }
        // Only ever reached in the child: nothing here has opened a font, and no GL context exists.
        let measure = ttf_measure().expect("the shipped faces open with no window");
        // With no font open the line height is the bare size (28), which this bound refuses: the numbers
        // are the shipped face's, at the `ShippedMeasure` ratios (1.21 and 0.73 of the size) to within
        // a pixel and a half. The cap height has no such discrimination (its fallback, 21, is inside the
        // bound); what it adds is that the call returns at all with no GL context.
        let (line, cap) = (measure.line_h(28), measure.cap_h(28));
        assert!((line - 28.0 * 1.21).abs() <= 1.5, "line height at 28 px: {line}");
        assert!((cap - 28.0 * 0.73).abs() <= 1.5, "cap height at 28 px: {cap}");
        assert!(measure.line_h(56) > 1.8 * line, "the line height follows the size");
        let c = std::ffi::CString::new("Resume playback").expect("no NUL");
        let (regular, bold) = (measure.width(&c, 28, false), measure.width(&c, 28, true));
        assert!(regular > 0.0 && bold > regular, "real widths, bold wider: {regular}, {bold}");
        // and the whole tool over it: one language of the classic, from the file, exit 0, nothing printed
        let classic = concat!(env!("CARGO_MANIFEST_DIR"), "/appkit/src/hud_format/fixtures/classic.plxhud");
        let (mut out, mut err) = (String::new(), String::new());
        let code = run(
            &args(&["--hud-lint", classic, "--lang", "de"]),
            &Io { read: &hud_source::read_source, text: &|_| panic!("no text profile"), write: &|_, _| panic!("lint wrote output") },
            &ttf_measure,
            &price_measure,
            &crate::hud_tools::font_hashes,
            &mut out,
            &mut err,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(cost_lines(&out).len(), out.lines().count(), "no finding, only cost lines: {out}");

        // The pricing measurer: no SDL, the same files, and the host tests' whole-pixel advances (the child's
        // application directory is `pkg/`, which `ShippedMeasure` reads), so the simulator prices what the
        // ceilings were taken with.
        let price = price_measure().expect("the shipped faces' metrics read");
        for (text, size, bold) in [("Resume playback", 28, false), ("Resume playback", 28, true), ("Пачаць прагляд спачатку", 20, false), ("Następny odcinek", 40, true)] {
            let c = std::ffi::CString::new(text).expect("no NUL");
            assert_eq!(price.width(&c, size, bold), ShippedMeasure.width(&c, size, bold), "{text:?} at {size} bold={bold}");
        }
        assert_eq!((price.line_h(28), price.cap_h(28)), (ShippedMeasure.line_h(28), ShippedMeasure.cap_h(28)));
        // and the whole tool over both real measurers: the classic and the reference in all twelve languages
        // are not rejected, and the cost lines are the television's (`ShippedMeasure`'s), not SDL_ttf's
        for (name, text) in [("classic", CLASSIC_FIXTURE), ("reference", REFERENCE_FIXTURE)] {
            let file = format!("{name}.plxhud");
            let read = move |_: &str| -> Result<String, String> { Ok(text.to_owned()) };
            let (mut out, mut err) = (String::new(), String::new());
            let source = |path: &str| read(path).map(|text| Source::Archive(plx_appkit::hud_format::fixtures::with_hud_resources("reference", &text)));
            let io = Io { read: &source, text: &read, write: &|_, _| Ok(()) };
            let code = run(&args(&["--hud-lint", &file, "--lang", "all"]), &io, &ttf_measure, &price_measure, &crate::hud_tools::font_hashes, &mut out, &mut err);
            assert_eq!((code, err.as_str()), (0, ""), "{name}: {out}");
            let tv = lint_report(text, &LintLang::shipped(), &LintOptions::default()).costs;
            assert_eq!(out, report::text_with_costs(&file, &[], &tv), "{name}: the cost lines are the television's");
        }
    }

    #[test]
    fn wants_is_true_for_every_hud_tool_flag_and_for_nothing_else() {
        for mode in [LINT, PACK, SCHEMA, EXPORT, INFO, METRICS, INSPECT] {
            assert!(wants(&args(&[mode])), "{mode}");
        }
        assert!(wants(&args(&["--hud-lint", "p.plxhud"])));
        assert!(wants(&args(&["--hud-schema"])));
        assert!(wants(&args(&["--hud-export", "builtin:classic"])));
        assert!(wants(&args(&["--format", "json", "--hud-schema"])));
        assert!(!wants(&args(&[])));
        assert!(!wants(&args(&["192.168.1.2", "32400"])));
        assert!(!wants(&args(&["--write-synthetic-initial", "/tmp/x", "1", "2"])));
        assert!(!wants(&args(&["--hud-lintt"])));
    }
}
