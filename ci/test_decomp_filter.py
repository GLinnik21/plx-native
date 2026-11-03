#!/usr/bin/env python3
"""Grade the output filter of `.agents/skills/decompile-tv-lib/decomp.sh`, the one `fn` and `xref`
share, against FIXTURES of Ghidra headless output. No Ghidra, no Java, no firmware, no network.

Ghidra's headless log writes every `println` of a GhidraScript as ONE log record:

    INFO  DecompMatch.java> <the message> (GhidraScript)<two spaces>

The prefix is on the record's FIRST line only, and the ` (GhidraScript)  ` suffix is on its LAST line.
A message with newlines in it (a decompiled C body, or an `xref` string literal that ends in "\\n") is
therefore several lines, and only the first has the prefix. The filter used to be
`sed -n 's/^INFO  [A-Za-z]*\\.java> //p'`, which keeps the prefixed lines and nothing else: `fn` printed
the `======== addr name` header and a blank line, never the function, and `xref` lost the address of any
string containing a newline.

What it pins, through the real script (`decomp.sh filter`, and `fn` / `xref` themselves against a fake
`analyzeHeadless` that replays the fixture):
  * every line of a multi-line message is kept, blank lines and indentation included;
  * the prefix and the ` (GhidraScript)  ` suffix are gone, and the empty tail the suffix leaves after a
    message's final newline is not turned into an extra blank line;
  * Ghidra's own log lines, including its MULTI-line records (the script path list, a warning with a
    stack trace) and the unprefixed text the JVM prints before the log starts, are dropped, also
    when one falls between two bodies (it must end the script's message, and the next prefixed line
    must start a new one).

The fixture lines come from a real `analyzeHeadless` 12.1.4 run over a stripped LG DILE library, with
the home directory, the lab path and the user name replaced; the records marked SYNTHETIC are the
ones a quiet library never produced. awk is whatever `awk` is on PATH (BSD awk on a Mac, mawk on a
CI runner): the filter is written in POSIX awk and both must give the same bytes.
"""
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / ".agents" / "skills" / "decompile-tv-lib" / "decomp.sh"

SUFFIX = " (GhidraScript)  "  # Ghidra's logger-name column: a space, "(GhidraScript)", two spaces


def lines(*text):
    return "\n".join(text) + "\n"


# --- fn: Ghidra's headless log for `fn dile_vt DILE_VT_Create 3` ---------------------------------

FN_RAW = lines(
    # the JVM's own banner, before the log starts: no log-level prefix at all
    'openjdk version "21.0.12.1" 2026-08-18',
    "OpenJDK Runtime Environment Homebrew (build 21.0.12.1)",
    "OpenJDK 64-Bit Server VM Homebrew (build 21.0.12.1, mixed mode)",
    "INFO  Using log config file: jar:file:/opt/ghidra/Generic.jar!/generic.log4j.xml (LoggingInitialization)  ",
    "INFO  Headless startup complete (1313 ms) (AnalyzeHeadless)  ",
    # a MULTI-line record of Ghidra's own: continuation lines carry no prefix either
    "INFO  HEADLESS Script Paths:",
    "    /opt/ghidra/Features/Base/ghidra_scripts",
    "    /tmp/tvlab/scripts",
    "    /opt/ghidra/Processors/Atmel/ghidra_scripts (HeadlessAnalyzer)  ",
    "INFO  REPORT: Execute script: DecompMatch.java 'DILE_VT_Create' '3'  (HeadlessAnalyzer)  ",
    "INFO  SCRIPT: /tmp/tvlab/scripts/DecompMatch.java (HeadlessAnalyzer)  ",
    # function 1: the header is one message, the body another that starts with an EMPTY line
    "INFO  DecompMatch.java> ======== 00012068  DILE_VT_CreateEx" + SUFFIX,
    "INFO  DecompMatch.java> ",
    "void DILE_VT_CreateEx(void)",
    "",
    "{",
    "  (*(code *)PTR_DILE_VT_CreateEx_00031130)();",
    "  return;",
    "}",
    "",
    SUFFIX,
    # SYNTHETIC: Ghidra's own multi-line warning BETWEEN two bodies. It ends the script's message.
    "WARN  Decompiler timed out on a function (DecompileCallback)  ",
    "java.lang.IllegalStateException: stuck",
    "\tat ghidra.app.decompiler.DecompileProcess.send(DecompileProcess.java:123)",
    "\tat ghidra.app.script.GhidraScript.run(GhidraScript.java:99)",
    # function 2: a body with an indented BLANK line (the decompiler's blank after its declarations)
    "INFO  DecompMatch.java> ======== 000133cc  DILE_VT_Create" + SUFFIX,
    "INFO  DecompMatch.java> ",
    "void * DILE_VT_Create(undefined4 param_1)",
    "",
    "{",
    "  void *__ptr;",
    "  pthread_mutex_t *__mutex;",
    "  ",
    "  __mutex = (pthread_mutex_t *)(DAT_00013458 + 0x133e8);",
    "  pthread_mutex_lock(__mutex);",
    "  __ptr = (void *)FUN_00012954(param_1);",
    "  if (__ptr == (void *)0x0) {",
    "    pthread_mutex_unlock(__mutex);",
    "    return (void *)0x0;",
    "  }",
    "  return __ptr;",
    "}",
    "",
    SUFFIX,
    # a single-line body: the decompiler's own failure marker
    "INFO  DecompMatch.java> ======== 00014000  FUN_00014000" + SUFFIX,
    "INFO  DecompMatch.java> // DECOMPILE FAILED" + SUFFIX,
    "INFO  DecompMatch.java> MATCHED=3 for 'dile_vt_create'" + SUFFIX,
    "INFO  REPORT: Save succeeded for processed file: /libdile_vt.so.0.1.0 (HeadlessAnalyzer)  ",
)

FN_WANT = lines(
    "======== 00012068  DILE_VT_CreateEx",
    "",
    "void DILE_VT_CreateEx(void)",
    "",
    "{",
    "  (*(code *)PTR_DILE_VT_CreateEx_00031130)();",
    "  return;",
    "}",
    "",
    "======== 000133cc  DILE_VT_Create",
    "",
    "void * DILE_VT_Create(undefined4 param_1)",
    "",
    "{",
    "  void *__ptr;",
    "  pthread_mutex_t *__mutex;",
    "  ",
    "  __mutex = (pthread_mutex_t *)(DAT_00013458 + 0x133e8);",
    "  pthread_mutex_lock(__mutex);",
    "  __ptr = (void *)FUN_00012954(param_1);",
    "  if (__ptr == (void *)0x0) {",
    "    pthread_mutex_unlock(__mutex);",
    "    return (void *)0x0;",
    "  }",
    "  return __ptr;",
    "}",
    "",
    "======== 00014000  FUN_00014000",
    "// DECOMPILE FAILED",
    "MATCHED=3 for 'dile_vt_create'",
)

# --- xref: `xref dile_vt '['`, where two of the strings end in a newline -------------------------

XREF_RAW = lines(
    "INFO  SCRIPT: /tmp/tvlab/scripts/XrefString.java (HeadlessAnalyzer)  ",
    'INFO  XrefString.java> ---- "[func:%s][line:%d] this is dummy callback function!',
    '" @ 0001abb0' + SUFFIX,
    "INFO  XrefString.java>         from 000126d4  (no function)" + SUFFIX,
    "INFO  XrefString.java>         from 000126ee  (no function)" + SUFFIX,
    'INFO  XrefString.java> ---- "[func:%s][line:%d] (%s), %s',
    '" @ 0001ac18' + SUFFIX,
    "INFO  XrefString.java>         from 0001485e  in DILE_VT_WaitVsync" + SUFFIX,
    'INFO  XrefString.java> ---- "[plain]" @ 0001ac90' + SUFFIX,
    "INFO  XrefString.java>         from 0001490e  in FUN_00014800" + SUFFIX,
    "INFO  XrefString.java> STRINGS_MATCHED=3 for '['" + SUFFIX,
    "INFO  REPORT: Save succeeded for processed file: /libdile_vt.so.0.1.0 (HeadlessAnalyzer)  ",
)

XREF_WANT = lines(
    '---- "[func:%s][line:%d] this is dummy callback function!',
    '" @ 0001abb0',
    "        from 000126d4  (no function)",
    "        from 000126ee  (no function)",
    '---- "[func:%s][line:%d] (%s), %s',
    '" @ 0001ac18',
    "        from 0001485e  in DILE_VT_WaitVsync",
    '---- "[plain]" @ 0001ac90',
    "        from 0001490e  in FUN_00014800",
    "STRINGS_MATCHED=3 for '['",
)

LIB = "libfake.so.1.0"


def run(args, stdin=None, env=None, cwd=None):
    return subprocess.run(["bash", str(SCRIPT), *args], input=stdin, env=env, cwd=cwd,
                          capture_output=True, text=True, timeout=60)


class FilterSubcommand(unittest.TestCase):
    """`decomp.sh filter`: the filter `fn` and `xref` pipe Ghidra's log through, on stdin."""

    def test_fn_keeps_every_line_of_every_body(self):
        r = run(["filter"], FN_RAW)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, FN_WANT)

    def test_xref_keeps_the_address_after_a_string_with_a_newline(self):
        r = run(["filter"], XREF_RAW)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, XREF_WANT)
        self.assertIn('" @ 0001abb0\n', r.stdout)

    def test_ghidras_own_log_never_reaches_the_output(self):
        out = run(["filter"], FN_RAW + XREF_RAW).stdout
        for noise in ("openjdk", "OpenJDK", "REPORT:", "SCRIPT:", "HEADLESS", "/opt/ghidra", "/tmp/tvlab",
                      "WARN", "IllegalStateException", "\tat ghidra", "LoggingInitialization"):
            self.assertNotIn(noise, out)

    def test_no_prefix_and_no_logger_suffix_survive(self):
        out = run(["filter"], FN_RAW + XREF_RAW).stdout
        self.assertNotIn("(GhidraScript)", out)
        self.assertNotIn("java>", out)
        for line in out.splitlines():
            self.assertFalse(line.startswith("INFO "), line)

    def test_a_log_level_line_between_two_bodies_ends_the_first_message(self):
        # The warning's own continuation lines come BEFORE the next prefixed line and must not be
        # taken for the first body's tail, and the second body must be printed whole.
        out = run(["filter"], FN_RAW).stdout
        self.assertIn("  pthread_mutex_lock(__mutex);\n", out)
        self.assertLess(out.index("return;"), out.index("======== 000133cc"))
        self.assertNotIn("stuck", out)

    def test_only_ghidras_own_logger_column_is_taken_off(self):
        # A body line that merely ends like a logger column, or a bare "INFO" type name that is not
        # at a line start, is the function's and stays exactly as printed.
        raw = lines(
            "INFO  DecompMatch.java> ======== 00010000  FUN_00010000" + SUFFIX,
            "INFO  DecompMatch.java> ",
            "void FUN_00010000(void)",
            "{",
            "  call_again(retry)  ",
            "  x = INFO + 1;",
            "}",
            "",
            SUFFIX,
        )
        want = lines("======== 00010000  FUN_00010000", "", "void FUN_00010000(void)", "{",
                     "  call_again(retry)  ", "  x = INFO + 1;", "}", "")
        self.assertEqual(run(["filter"], raw).stdout, want)

    def test_a_continuation_line_that_starts_with_a_level_word_stays_in_the_record(self):
        # A firmware string is not indented the way a decompiled body is: a line of it may start
        # with "INFO  ", "WARN " or "ERROR ", and the record's last line (the address) comes after it.
        raw = lines(
            'INFO  XrefString.java> ---- "usage: tool [-v]',
            "INFO  dumps state",
            "WARN again",
            "ERROR  and ERROR",
            '" @ 0001abb0' + SUFFIX,
            "INFO  XrefString.java>         from 000126d4  in FUN_000126d0" + SUFFIX,
            "INFO  XrefString.java> STRINGS_MATCHED=1 for 'usage'" + SUFFIX,
            "INFO  REPORT: Save succeeded for processed file: /libfake.so (HeadlessAnalyzer)  ",
        )
        want = lines(
            '---- "usage: tool [-v]',
            "INFO  dumps state",
            "WARN again",
            "ERROR  and ERROR",
            '" @ 0001abb0',
            "        from 000126d4  in FUN_000126d0",
            "STRINGS_MATCHED=1 for 'usage'",
        )
        r = run(["filter"], raw)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, want)

    def test_a_ghidra_record_after_a_terminated_record_is_still_dropped(self):
        # Once a record has ended on its own " (GhidraScript)  " column, a level word starts a
        # Ghidra record again, and so does any unprefixed line, whatever follows it.
        raw = lines(
            "INFO  XrefString.java> first" + SUFFIX,
            "INFO  something of Ghidra's own (Analyzer)  ",
            "INFO  continuation of that one",
            "INFO  XrefString.java> second",
            "INFO  inside second",
            "tail" + SUFFIX,
            "WARN  and a warning (Analyzer)  ",
            "and its second line",
            "INFO  XrefString.java> third" + SUFFIX,
        )
        want = lines("first", "second", "INFO  inside second", "tail", "third")
        self.assertEqual(run(["filter"], raw).stdout, want)

    def test_an_empty_println_is_an_empty_line_and_digits_in_a_script_name_match(self):
        raw = lines(
            "INFO  Script_2.java> " + SUFFIX,
            "INFO  Script_2.java> after" + SUFFIX,
        )
        self.assertEqual(run(["filter"], raw).stdout, lines("", "after"))

    def test_nothing_in_nothing_out(self):
        r = run(["filter"], "")
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        r = run(["filter"], 'openjdk version "21"\nINFO  Headless startup complete (AnalyzeHeadless)  \n')
        self.assertEqual((r.returncode, r.stdout), (0, ""))

    def test_the_usage_lists_filter(self):
        r = run(["help"])
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("decomp.sh filter", r.stdout)
        self.assertIn("decomp.sh xref", r.stdout)

    def test_the_usage_is_the_header_comment_and_nothing_after_it(self):
        for args in (["help"], []):
            r = run(args)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertNotIn("set -euo", r.stdout)
            self.assertTrue(r.stdout.startswith("decomp.sh \u2014 decompile"), r.stdout)
            self.assertTrue(r.stdout.rstrip("\n").endswith("`decomp.sh clean` drops the cache."), r.stdout)
            self.assertFalse(r.stdout.endswith("\n\n"), repr(r.stdout[-40:]))


class FnAndXrefThroughAFakeGhidra(unittest.TestCase):
    """`fn` and `xref` end to end: the wiring from the real commands into the filter, against a
    stand-in `analyzeHeadless` that answers `-import` with nothing and `-process` with a canned log."""

    FAKE_HEADLESS = (
        "#!/bin/sh\n"
        'for a in "$@"; do\n'
        '  case "$a" in\n'
        "    -import) exit 0 ;;\n"
        '    -process) cat "$FAKE_RAW"; exit 0 ;;\n'
        "  esac\n"
        "done\n"
        "exit 2\n"
    )

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="decomp-filter-test.")
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        (tmp / "lab" / "bin").mkdir(parents=True)
        (tmp / "lab" / "bin" / LIB).write_bytes(b"\x7fELF fake")
        headless = tmp / "analyzeHeadless"
        headless.write_text(self.FAKE_HEADLESS)
        headless.chmod(headless.stat().st_mode | stat.S_IXUSR)
        (tmp / "jdk").mkdir()
        self.env = dict(
            os.environ,
            HOME=str(tmp / "home"),
            DECOMP_LAB=str(tmp / "lab"),
            GHIDRA_HEADLESS=str(headless),
            JAVA_HOME=str(tmp / "jdk"),
            FAKE_RAW=str(tmp / "raw.log"),
        )
        self.raw = tmp / "raw.log"

    def query(self, raw, *args):
        self.raw.write_text(raw)
        return run(list(args), env=self.env)

    def test_fn_prints_the_whole_function(self):
        r = self.query(FN_RAW, "fn", "fake", "DILE_VT_Create", "3")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, FN_WANT)
        self.assertIn("analysing " + LIB, r.stderr)

    def test_xref_prints_the_address_of_a_multiline_string(self):
        r = self.query(XREF_RAW, "xref", "fake", "[")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, XREF_WANT)


if __name__ == "__main__":
    unittest.main()
