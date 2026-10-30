//! **`plxnative-sim --hud-lint | --hud-schema | --hud-export`**: the HUD plugin tools that run without
//! opening a window (`docs/hud-plugins.md`, "LLM authoring" item 2 and "Schema").
//!
//! ```text
//! plxnative-sim --hud-lint <file> [--format text|json] [--lang all|<tag>]
//! plxnative-sim --hud-schema
//! plxnative-sim --hud-export builtin:classic
//! ```
//!
//! **Exit codes.** `--hud-lint`: 0 when the package has no finding, 1 when it has any (a warning is a
//! finding: the registry's CI treats `HUD-W0602` as an error), 2 for a usage error. A usage error is
//! a bad command line, a file that cannot be read as UTF-8 text, or fonts that cannot be loaded; each
//! is ONE line on stderr and nothing on stdout. A package that does not parse is not a usage error: its
//! parse findings are the report, so the exit code is 1. `--hud-schema` and `--hud-export` exit 0, or 2
//! for a usage error (an unknown export id included).
//!
//! **Everything that decides is [`run`]**, a pure function: the file read and the text measurer are
//! INJECTED (`read_file`, `measure`) and the output goes into two strings, so the exit codes and the
//! bytes are unit-tested without a file, a font or a process. The measurer is a FACTORY and is called
//! only by `--hud-lint` and only once the command line and the file have been accepted: `--hud-schema`
//! and `--hud-export` never touch a font. [`main_from`] is the thin process shell and the ONLY IO here:
//! it wires `std::fs`, the real measurer and the two standard streams.
//!
//! **The real measurer** is [`plx_gfx::text::TtfMeasure`] over the shipped `appfont*.ttf` faces from
//! `PLXNATIVE_APP_DIR` (the simulator's `plx_base::paths::app_dir`), opened by
//! [`plx_gfx::text::init_fonts_headless`] with no window and no GL context. There is no unmeasured
//! fallback: when the faces cannot be opened the run is a usage error, `cannot load fonts: ...`.
//! The simulator measures with the host's SDL_ttf, which sums fractional advances, where the
//! television's 2.0.x sums whole-pixel ones (`plx_base::fontcov::advances`), so a width here can be
//! a few pixels off the television's: the TV's own compile decides (`docs/hud-plugins.md`, "Text fit").

use plx_appkit::hud_format::lint::{lint, LintLang};
use plx_appkit::hud_format::{report, schema, schema_emit, BUILTIN_CLASSIC};
use plx_machine::machine::Measure;

/// The three flags that make the simulator binary run a HUD tool instead of the application.
const LINT: &str = "--hud-lint";
const SCHEMA: &str = "--hud-schema";
const EXPORT: &str = "--hud-export";

/// The packages `--hud-export` can print: id and bytes. One entry today; PR6's examples add theirs.
const EXPORTS: &[(&str, &str)] = &[("builtin:classic", BUILTIN_CLASSIC)];

/// Every finding of the linted package is reported on stdout; 0 means there was none.
const EXIT_CLEAN: i32 = 0;
/// The package has at least one finding.
const EXIT_FINDINGS: i32 = 1;
/// The command line, the file or the fonts were not usable; nothing was linted.
const EXIT_USAGE: i32 = 2;

/// The shape of a usage error, one line: the reason, then (for an argument error) the usage.
const USAGE: &str = "usage: plxnative-sim --hud-lint <file> [--format text|json] [--lang all|<tag>] \
                     | --hud-schema | --hud-export <id>";

/// Does the command line ask for a HUD tool? True when any of the three flags is an argument, so the
/// binary hands the whole line to [`main_from`] before it reads a PMS host from it.
pub fn wants(args: &[String]) -> bool {
    args.iter().any(|a| matches!(a.as_str(), LINT | SCHEMA | EXPORT))
}

/// How a lint report is printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

/// What the command line asked for.
enum Command {
    Lint { file: String, format: Format, langs: Vec<LintLang> },
    Schema,
    Export(String),
}

/// The value after `flag`: the next argument unless it is missing or is itself a flag.
fn value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str, what: &str) -> Result<&'a str, String> {
    match it.next() {
        Some(v) if !v.starts_with("--") => Ok(v),
        _ => Err(format!("{flag} needs {what}")),
    }
}

/// Parse the command line. The first mode flag decides the command; a second one, an option that
/// belongs to another mode, a repeated option and an argument no mode takes are usage errors.
fn parse(args: &[String]) -> Result<Command, String> {
    enum Mode {
        Lint(String),
        Schema,
        Export(String),
    }
    let mut mode: Option<Mode> = None;
    let mut format: Option<Format> = None;
    let mut lang: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let next_mode = match arg.as_str() {
            LINT => Some(Mode::Lint(value(&mut it, LINT, "a file")?.to_owned())),
            SCHEMA => Some(Mode::Schema),
            EXPORT => Some(Mode::Export(value(&mut it, EXPORT, "a package id")?.to_owned())),
            "--format" => {
                let v = value(&mut it, "--format", "text or json")?;
                let parsed = match v {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => return Err(format!("--format is text or json, not {other:?}")),
                };
                if format.replace(parsed).is_some() {
                    return Err("--format given twice".to_owned());
                }
                None
            }
            "--lang" => {
                let v = value(&mut it, "--lang", "all or a language tag")?;
                if lang.replace(v.to_owned()).is_some() {
                    return Err("--lang given twice".to_owned());
                }
                None
            }
            other => return Err(format!("unknown argument {other:?}")),
        };
        if let Some(next_mode) = next_mode {
            if mode.replace(next_mode).is_some() {
                return Err(format!("only one of {LINT}, {SCHEMA} and {EXPORT} may be given"));
            }
        }
    }
    let command = match mode {
        None => return Err(format!("nothing to do: give {LINT}, {SCHEMA} or {EXPORT}")),
        Some(Mode::Schema) => Command::Schema,
        Some(Mode::Export(id)) => Command::Export(id),
        Some(Mode::Lint(file)) => {
            let langs = match lang.as_deref() {
                None | Some("all") => LintLang::shipped(),
                Some(tag) => match LintLang::for_tag(tag) {
                    Some(one) => vec![one],
                    None => {
                        let tags: Vec<&str> = LintLang::shipped().iter().map(|l| l.tag).collect();
                        return Err(format!("--lang is all or one of {}, not {tag:?}", tags.join(" ")));
                    }
                },
            };
            return Ok(Command::Lint { file, format: format.unwrap_or(Format::Text), langs });
        }
    };
    if format.is_some() || lang.is_some() {
        return Err(format!("--format and --lang belong to {LINT}"));
    }
    Ok(command)
}

/// Run one HUD tool. `args` is the command line after the program name. `read_file` reads a package
/// as UTF-8 text (the error is the reason, without the path), `measure` builds the text measurer and is
/// called at most once, by `--hud-lint` only. The report goes to `out`, the one-line reason of a usage
/// error to `err`. Returns the process exit code (see the module doc).
pub fn run(
    args: &[String],
    read_file: &dyn Fn(&str) -> Result<String, String>,
    measure: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    out: &mut String,
    err: &mut String,
) -> i32 {
    let mut usage = |reason: &str, with_usage: bool| {
        err.push_str("plxnative-sim: ");
        err.push_str(reason);
        if with_usage {
            err.push_str(" (");
            err.push_str(USAGE);
            err.push(')');
        }
        err.push('\n');
        EXIT_USAGE
    };
    let command = match parse(args) {
        Ok(command) => command,
        Err(reason) => return usage(&reason, true),
    };
    match command {
        Command::Schema => {
            out.push_str(&schema_emit::emit_text(schema::CURRENT));
            EXIT_CLEAN
        }
        Command::Export(id) => match EXPORTS.iter().find(|(known, _)| *known == id) {
            Some((_, text)) => {
                out.push_str(text);
                EXIT_CLEAN
            }
            None => {
                let ids: Vec<&str> = EXPORTS.iter().map(|(known, _)| *known).collect();
                usage(&format!("unknown package {id:?} to export; known: {}", ids.join(" ")), false)
            }
        },
        Command::Lint { file, format, langs } => {
            let text = match read_file(&file) {
                Ok(text) => text,
                Err(reason) => return usage(&format!("cannot read {file}: {reason}"), false),
            };
            let measure = match measure() {
                Ok(measure) => measure,
                Err(reason) => return usage(&reason, false),
            };
            let findings = lint(&text, measure.as_ref(), &langs);
            out.push_str(&match format {
                Format::Text => report::text(&file, &findings),
                Format::Json => report::json_text(&file, &findings),
            });
            if findings.is_empty() {
                EXIT_CLEAN
            } else {
                EXIT_FINDINGS
            }
        }
    }
}

/// The real text measurer: the shipped faces opened without a window, then [`TtfMeasure`] over them.
///
/// [`TtfMeasure`]: plx_gfx::text::TtfMeasure
fn ttf_measure() -> Result<Box<dyn Measure>, String> {
    plx_gfx::text::init_fonts_headless().map_err(|reason| format!("cannot load fonts: {reason}"))?;
    Ok(Box::new(plx_gfx::text::TtfMeasure))
}

/// A package file as UTF-8 text, with the reason as a short string when it is not one.
fn read_package(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8 (a package is a UTF-8 JSON file)".to_owned())
}

/// The process shell of the three flags: [`run`] over the real file system, the real measurer and the
/// two standard streams. Returns the exit code for the binary to end the process with. The only IO of
/// this module.
pub fn main_from(args: &[String]) -> i32 {
    use std::io::Write;
    let (mut out, mut err) = (String::new(), String::new());
    let code = run(args, &read_package, &ttf_measure, &mut out, &mut err);
    // Written whole and flushed here, not by `exit_now` alone: a closed pipe (`| head`) is the
    // reader's choice and must not turn the exit code into a panic.
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    let _ = std::io::stdout().lock().flush();
    let _ = std::io::stderr().lock().write_all(err.as_bytes());
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;
    use std::cell::Cell;
    use std::ffi::CStr;
    use std::rc::Rc;

    const SCHEMA_GOLDEN: &str = include_str!("../../site/hud/schema/1.0.json");
    const CLASSIC_FIXTURE: &str = include_str!("../appkit/src/hud_format/fixtures/classic.plxhud.json");

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
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

    /// Run with a result of `(exit code, stdout, stderr)`.
    fn go(
        list: &[&str],
        read: &dyn Fn(&str) -> Result<String, String>,
        measure: &dyn Fn() -> Result<Box<dyn Measure>, String>,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (String::new(), String::new());
        let code = run(&args(list), read, measure, &mut out, &mut err);
        (code, out, err)
    }

    /// How many widths a lint of the classic package in `lang_args` asks the measurer for.
    fn widths(lang_args: &[&str]) -> usize {
        let calls = Rc::new(Cell::new(0));
        let factory = {
            let calls = Rc::clone(&calls);
            move || -> Result<Box<dyn Measure>, String> { Ok(Box::new(Counting(Rc::clone(&calls)))) }
        };
        let mut list = vec!["--hud-lint", "classic.plxhud.json"];
        list.extend_from_slice(lang_args);
        let (code, _, err) = go(&list, &serve(BUILTIN_CLASSIC), &factory);
        assert_eq!((code, err.as_str()), (0, ""), "{list:?}");
        calls.get()
    }

    #[test]
    fn hud_lint_exits_0_and_prints_nothing_for_the_builtin_classic() {
        let (code, out, err) = go(&["--hud-lint", "classic.plxhud.json"], &serve(BUILTIN_CLASSIC), &shipped);
        assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
        let (code, out, err) = go(
            &["--hud-lint", "classic.plxhud.json", "--format", "json", "--lang", "all"],
            &serve(BUILTIN_CLASSIC),
            &shipped,
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, report::json_text("classic.plxhud.json", &[]));
        assert!(out.contains("\"findings\": []"), "{out}");
    }

    #[test]
    fn hud_lint_exits_1_for_a_text_that_fails_to_parse() {
        let (code, out, err) = go(&["--hud-lint", "broken.plxhud.json"], &serve("{"), &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        assert!(out.starts_with("broken.plxhud.json#"), "{out}");
        assert!(out.contains(" error HUD-E"), "{out}");
        assert!(out.ends_with('\n'), "{out}");
    }

    #[test]
    fn hud_lint_exits_1_for_a_package_that_parses_but_fails_a_rule() {
        // A well-formed document that is not a package: parse and validate findings, no crash.
        let (code, out, _) = go(&["--hud-lint", "empty.plxhud.json"], &serve("{}"), &shipped);
        assert_eq!(code, 1, "{out}");
        assert!(!out.is_empty());
    }

    #[test]
    fn every_usage_error_exits_2_with_one_line_on_stderr_and_nothing_on_stdout() {
        let unreadable = |_: &str| -> Result<String, String> { Err("No such file or directory".to_owned()) };
        let cases: &[(&[&str], &str)] = &[
            (&[], "nothing to do"),
            (&["--hud-lint"], "--hud-lint needs a file"),
            (&["--hud-lint", "--format", "json"], "--hud-lint needs a file"),
            (&["--hud-lint", "p.json", "--bogus"], "unknown argument \"--bogus\""),
            (&["--hud-lint", "p.json", "extra.json"], "unknown argument \"extra.json\""),
            (&["--hud-lint", "p.json", "--format"], "--format needs text or json"),
            (&["--hud-lint", "p.json", "--format", "xml"], "--format is text or json, not \"xml\""),
            (&["--hud-lint", "p.json", "--format", "json", "--format", "text"], "--format given twice"),
            (&["--hud-lint", "p.json", "--lang"], "--lang needs all or a language tag"),
            (&["--hud-lint", "p.json", "--lang", "xx"], "--lang is all or one of en es be fr it pt de hu ru pl"),
            (&["--hud-lint", "p.json", "--lang", "system"], "not \"system\""),
            (&["--hud-lint", "p.json", "--lang", "de", "--lang", "fr"], "--lang given twice"),
            (&["--hud-export"], "--hud-export needs a package id"),
            (&["--hud-export", "nope"], "unknown package \"nope\" to export; known: builtin:classic"),
            (&["--hud-schema", "--hud-export", "builtin:classic"], "only one of"),
            (&["--hud-lint", "p.json", "--hud-schema"], "only one of"),
            (&["--hud-schema", "extra"], "unknown argument \"extra\""),
            (&["--hud-schema", "--format", "json"], "--format and --lang belong to --hud-lint"),
            (&["--hud-export", "builtin:classic", "--lang", "de"], "--format and --lang belong to --hud-lint"),
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
        let (code, out, err) = go(&["--hud-lint", "gone.plxhud.json"], &unreadable, &never);
        assert_eq!((code, out.as_str()), (2, ""));
        assert_eq!(err, "plxnative-sim: cannot read gone.plxhud.json: No such file or directory\n");
    }

    #[test]
    fn a_measurer_that_cannot_be_built_is_exit_2_and_never_a_report() {
        let no_fonts = || -> Result<Box<dyn Measure>, String> { Err("cannot load fonts: no appfont.ttf".to_owned()) };
        let (code, out, err) = go(&["--hud-lint", "p.json"], &serve(BUILTIN_CLASSIC), &no_fonts);
        assert_eq!((code, out.as_str()), (2, ""));
        assert_eq!(err, "plxnative-sim: cannot load fonts: no appfont.ttf\n");
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
        let built = Cell::new(0);
        let factory = || -> Result<Box<dyn Measure>, String> {
            built.set(built.get() + 1);
            Ok(Box::new(ShippedMeasure))
        };
        let (code, _, _) = go(&["--hud-lint", "dir/my hud.json", "--lang", "all"], &reader, &factory);
        assert_eq!(code, 0);
        assert_eq!(read.take(), ["dir/my hud.json"]);
        assert_eq!(built.get(), 1);
    }

    #[test]
    fn lang_de_lints_one_language_and_lang_all_lints_the_ten() {
        let de = widths(&["--lang", "de"]);
        let all = widths(&["--lang", "all"]);
        assert!(de > 0, "a compile measures text");
        assert_eq!(widths(&[]), all, "all is the default");
        let tags = ["en", "es", "be", "fr", "it", "pt", "de", "hu", "ru", "pl"];
        let each: usize = tags.iter().map(|tag| widths(&["--lang", tag])).sum();
        assert_eq!(all, each, "the all run is the ten single-language runs");
        assert!(all > de);
        assert!(all >= 5 * de, "ten languages, not two: all {all}, de {de}");
    }

    #[test]
    fn json_output_is_report_json_text_of_the_findings_lint_returns() {
        let bytes = "{\n  \"format\": \"1.0\" ,}";
        let expected = lint(bytes, &ShippedMeasure, &LintLang::shipped());
        assert!(!expected.is_empty());
        let (code, out, err) = go(&["--hud-lint", "bad.plxhud.json", "--format", "json"], &serve(bytes), &shipped);
        assert_eq!((code, err.as_str()), (1, ""));
        assert_eq!(out, report::json_text("bad.plxhud.json", &expected));
        assert!(out.starts_with("{\n"), "{out}");
        assert!(out.contains("\"file\": \"bad.plxhud.json\""), "{out}");
        assert!(out.contains("\"lint\": 1"), "{out}");
        assert!(out.ends_with("}\n"), "{out}");
        let (code, text, _) = go(&["--hud-lint", "bad.plxhud.json", "--format", "text"], &serve(bytes), &shipped);
        assert_eq!(code, 1);
        assert_eq!(text, report::text("bad.plxhud.json", &expected));
        let (_, default, _) = go(&["--hud-lint", "bad.plxhud.json"], &serve(bytes), &shipped);
        assert_eq!(default, text, "text is the default format");
    }

    #[test]
    fn hud_schema_output_is_byte_identical_to_the_site_golden() {
        let (code, out, err) = go(&["--hud-schema"], &|_| Err("no file".to_owned()), &never);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, SCHEMA_GOLDEN);
        assert_eq!(out, schema_emit::emit_text(schema::CURRENT));
    }

    #[test]
    fn hud_export_classic_is_byte_identical_to_the_fixture() {
        let (code, out, err) = go(&["--hud-export", "builtin:classic"], &|_| Err("no file".to_owned()), &never);
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, CLASSIC_FIXTURE);
    }

    #[test]
    fn schema_and_export_never_build_a_measurer_or_read_a_file() {
        let built = Cell::new(0);
        let factory = || -> Result<Box<dyn Measure>, String> {
            built.set(built.get() + 1);
            Ok(Box::new(ShippedMeasure))
        };
        let read = Cell::new(0);
        let reader = |_: &str| -> Result<String, String> {
            read.set(read.get() + 1);
            Err("unreachable".to_owned())
        };
        for list in [&["--hud-schema"][..], &["--hud-export", "builtin:classic"]] {
            let (code, _, _) = go(list, &reader, &factory);
            assert_eq!(code, 0, "{list:?}");
        }
        assert_eq!((built.get(), read.get()), (0, 0));
    }

    #[test]
    fn wants_is_true_for_each_of_the_three_flags_and_for_nothing_else() {
        assert!(wants(&args(&["--hud-lint", "p.json"])));
        assert!(wants(&args(&["--hud-schema"])));
        assert!(wants(&args(&["--hud-export", "builtin:classic"])));
        assert!(wants(&args(&["--format", "json", "--hud-schema"])));
        assert!(!wants(&args(&[])));
        assert!(!wants(&args(&["192.168.1.2", "32400"])));
        assert!(!wants(&args(&["--write-synthetic-initial", "/tmp/x", "1", "2"])));
        assert!(!wants(&args(&["--hud-lintt"])));
    }
}
