#!/usr/bin/env python3
"""Cases for the release-config hook's pure halves. `python3 .claude/hooks/release-config-check-test.py`.

HOST-ONLY, AND IT MUST STAY THAT WAY: it never invokes cargo, never writes to `target/`, and never
takes the build-directory lock. That is not a stylistic preference — this test is the thing that
runs while five agents are editing in parallel worktrees, and a test that shelled out to cargo
would queue behind whichever lane is building. The hook is therefore split so that everything
decided BEFORE the 0.55 s subprocess, and everything done with its output after, is pure:
`rust_src_target` (does this payload deserve a check), `verdict` (is that exit code a broken build
or an unusable toolchain), `first_error` (which line to quote when it could not run), `trim` (what
reaches the model), `nightly` (which toolchain), `timeout_secs` (the cap).

WHAT THIS FILE STRUCTURALLY CANNOT GRADE, and so must be done by hand after any change to
`run_check`, `report` or `main`: that the hook actually reaches exit 2 on a real break. Splice a
release-only failure into a file under `rust-modules/src` — an item spliced between a
`#[cfg(feature = "devtriggers")]` attribute and its `fn`, which orphans the gate exactly as
2026-08-21 did, or a bare `#[cfg(not(feature = "devtriggers"))] compile_error!(...)` — feed the
hook an Edit payload for that file, confirm exit 2 with real compiler text on stderr, and revert.
Both were run on 2026-08-23; the second is also what exposed the `text=True` decode fail-open now
fixed in `run_check`, because its message carried an em-dash.

The interesting cases are the NEGATIVES. A missed edit costs one unchecked release configuration
and the next edit re-runs it; a false fire costs 0.55 s on every `.rs` write in the session and
teaches the reader to reach for `PLX_RELEASE_CHECK_SKIP`. The three that are easy to get wrong are
`src/bin/sim.rs` (a real Rust file under the tree, which `--lib` does not compile and the release
set could not build anyway — `required-features = ["hostsim"]`), `build.rs` (compiled in EVERY
configuration, so already covered by `make check`), and a file in a sibling `.claude/worktrees/`
checkout (another lane's crate, another lane's target dir).

`/repo` below is a fabricated root: `rust_src_target` is pure path arithmetic, so nothing needs to
exist on disk. The one case that reads the real tree is `nightly()`, deliberately — it parses the
actual Makefile, so renaming `RUST_NIGHTLY` fails here instead of silently falling back.
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
spec = importlib.util.spec_from_file_location(
    "relcheck", os.path.join(HERE, "release-config-check.py"))
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

ROOT = "/repo"
WT = "/repo/.claude/worktrees/bridge-cse_01EXAMPLE"
FIRE, SKIP = True, False


def edit(path, tool="Edit", key="file_path", **extra):
    p = {"tool_name": tool, "tool_input": {key: path}, "cwd": ROOT}
    p.update(extra)
    return p


# (want, label, payload, cwd[, root]) — `cwd` is where the tool ran, which relative paths resolve
# against; `root` is the lane and defaults to /repo.
CASES = [
    # --- fires: this lane's lib sources ---------------------------------------
    (FIRE, "plain edit", edit("/repo/rust-modules/src/app.rs"), ROOT),
    (FIRE, "nested module", edit("/repo/rust-modules/src/ui/widgets.rs"), ROOT),
    (FIRE, "Write", edit("/repo/rust-modules/src/dev.rs", tool="Write"), ROOT),
    (FIRE, "MultiEdit", edit("/repo/rust-modules/src/gfx.rs", tool="MultiEdit"), ROOT),
    (FIRE, "relative to root", edit("rust-modules/src/route.rs"), ROOT),
    (FIRE, "relative to a subdir", edit("src/plex/client.rs"), "/repo/rust-modules"),
    (FIRE, "relative climbing out", edit("../rust-modules/src/net.rs"), "/repo/tools"),
    (FIRE, "un-normalised absolute", edit("/repo/rust-modules/src/ui/../ff.rs"), ROOT),
    # A worktree IS its own lane: same file, but `git rev-parse` from its own cwd returns ITS root.
    (FIRE, "worktree, from inside it",
     {"tool_name": "Edit", "tool_input": {"file_path": WT + "/rust-modules/src/app.rs"},
      "cwd": WT}, WT, WT),

    # --- does not fire: right extension, wrong target -------------------------
    (SKIP, "src/bin/sim.rs (not a --lib target, needs hostsim)",
     edit("/repo/rust-modules/src/bin/sim.rs"), ROOT),
    (SKIP, "build.rs (compiled in every configuration)",
     edit("/repo/rust-modules/build.rs"), ROOT),
    (SKIP, "tools/ script", edit("/repo/tools/some_helper.rs"), ROOT),
    (SKIP, "ci/ file", edit("/repo/ci/scratch.rs"), ROOT),
    (SKIP, "a .rs outside the crate entirely", edit("/elsewhere/lib.rs"), ROOT),
    (SKIP, "another lane's worktree, edited from this root",
     edit(WT + "/rust-modules/src/app.rs"), ROOT),

    # --- does not fire: not a Rust source edit --------------------------------
    (SKIP, "the crate's CLAUDE.md", edit("/repo/rust-modules/src/ui/CLAUDE.md"), ROOT),
    # Cargo.toml is where `[features]` itself lives, so this one is a real choice, not an
    # oversight — the hook grades a Rust edit's side effect on a configuration its author was not
    # thinking about, and someone editing the feature list is thinking about exactly that. Cost is
    # not the reason: a manifest touch measures the same 0.6 s as a source touch.
    (SKIP, "Cargo.toml (a deliberate scope line, see the hook's DOES NOT CHECK)",
     edit("/repo/rust-modules/Cargo.toml"), ROOT),
    (SKIP, "the C shim", edit("/repo/src/main.c"), ROOT),
    (SKIP, "a .rs.bak", edit("/repo/rust-modules/src/app.rs.bak"), ROOT),
    (SKIP, "Bash", {"tool_name": "Bash", "tool_input": {"command": "make check"}, "cwd": ROOT}, ROOT),
    (SKIP, "Read of a crate source",
     {"tool_name": "Read", "tool_input": {"file_path": "/repo/rust-modules/src/app.rs"}}, ROOT),
    (SKIP, "NotebookEdit (notebook_path, never .rs)",
     edit("/repo/rust-modules/src/notes.ipynb", tool="NotebookEdit", key="notebook_path"), ROOT),
    (SKIP, "a refused edit reported as such",
     edit("/repo/rust-modules/src/app.rs", tool_response={"success": False}), ROOT),

    # --- malformed payloads must not crash ------------------------------------
    (SKIP, "empty object", {}, ROOT),
    (SKIP, "no tool_input", {"tool_name": "Edit"}, ROOT),
    (SKIP, "tool_input is null", {"tool_name": "Edit", "tool_input": None}, ROOT),
    (SKIP, "tool_input is a string", {"tool_name": "Edit", "tool_input": "app.rs"}, ROOT),
    (SKIP, "file_path is null", {"tool_name": "Edit", "tool_input": {"file_path": None}}, ROOT),
    (SKIP, "file_path is a number", {"tool_name": "Edit", "tool_input": {"file_path": 42}}, ROOT),
    (SKIP, "file_path is empty", {"tool_name": "Edit", "tool_input": {"file_path": "   "}}, ROOT),
    # ...but a tool_response that is not a dict is not a refusal, and must not be read as one.
    (FIRE, "tool_response is a string, not a dict",
     edit("/repo/rust-modules/src/aq.rs", tool_response="edited"), ROOT),
]


# `verdict` decides whether the model is told about a break at all. Getting 'unusable' wrong is the
# expensive direction: it reports a missing pinned nightly as a release-configuration regression,
# which sends the reader to the diff instead of to rustup.
E0428 = ("error[E0428]: the name `density_max_sweep` is defined multiple times\n"
         " --> src/ui/widgets.rs:812:1\n"
         "error: could not compile `plxnative-modules` (lib) due to 1 previous error")
DENIED_LINT = ("error: unused import: `crate::dev`\n"
               " --> src/gfx.rs:44:5\n"
               "  = note: `-D unused-imports` implied by `-D warnings`\n"
               "error: could not compile `plxnative-modules` (lib) due to 1 previous error")

VERDICTS = [
    ("ok", 0, "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.55s"),
    ("ok", 0, ""),
    ("broken", 101, E0428),
    ("broken", 101, DENIED_LINT),                       # `warnings = "deny"` is the sharp edge
    ("unusable", 1, "error: toolchain 'nightly-2026-07-02' is not installed"),
    ("unusable", 127, "error: could not run cargo: [Errno 2] No such file or directory: 'cargo'"),
    ("unusable", 1, "error: failed to acquire package cache lock"),
]

# `first_error` picks the line quoted in the "could not run the check" note. The first NON-EMPTY
# line is the wrong choice and was the original one: on these paths cargo has usually printed its
# `Checking …` banner first, and a note that quotes the banner names nothing the reader can act on.
FIRST_ERROR = [
    ("error: toolchain 'nightly-2026-07-02' is not installed",
     "    Checking plxnative-modules v0.4.1 (/repo/rust-modules)\n"
     "error: toolchain 'nightly-2026-07-02' is not installed"),
    ("error: could not run cargo: [Errno 2] No such file or directory: 'cargo'",
     "error: could not run cargo: [Errno 2] No such file or directory: 'cargo'"),
    # No `error` line at all: fall back to the first non-empty one rather than saying nothing.
    ("warning: something odd", "\n\n   warning: something odd\nand a second line"),
    ("no output", ""),
    ("no output", "   \n\n"),
]

NOISY = """    Checking plxnative-modules v0.4.1 (/repo/rust-modules)
error[E0428]: the name `x` is defined multiple times
 --> src/ui/widgets.rs:812:1
    Finished `dev` profile in 0.55s"""


def main():
    fails = 0
    for case in CASES:
        want, label, payload, cwd = case[:4]
        root = case[4] if len(case) > 4 else ROOT
        try:
            got = hook.rust_src_target(payload, root, cwd) is not None
        except Exception as e:
            fails += 1
            print(f"  FAIL  raised {e!r}: {label}")
            continue
        if got != want:
            fails += 1
            print(f"  FAIL  expected {'FIRE' if want else 'SKIP'}, got "
                  f"{'FIRE' if got else 'SKIP'}: {label}")

    for want, rc, out in VERDICTS:
        got = hook.verdict(rc, out)
        if got != want:
            fails += 1
            print(f"  FAIL  verdict({rc}) expected {want}, got {got}: {out.splitlines()[:1]}")

    for want, out in FIRST_ERROR:
        got = hook.first_error(out)
        if got != want:
            fails += 1
            print(f"  FAIL  first_error expected {want!r}, got {got!r}")

    trimmed = hook.trim(NOISY)
    if "Checking plxnative-modules" in trimmed or "Finished" in trimmed:
        fails += 1
        print("  FAIL  trim kept cargo's progress lines")
    if "error[E0428]" not in trimmed or "--> src/ui/widgets.rs:812:1" not in trimmed:
        fails += 1
        print("  FAIL  trim dropped a diagnostic line or its location")
    capped = hook.trim("error: x\n" + ("y" * 9000), cap=200)
    if len(capped) > 320 or "trimmed at 200 bytes" not in capped:
        fails += 1
        print(f"  FAIL  trim did not cap ({len(capped)} bytes)")

    # The cap is what stands between a held build-directory lock and a wedged session, so its
    # parse must never raise: garbage falls back to the default, and nothing goes below 5 s.
    saved_to = os.environ.pop(hook.TIMEOUT_ENV, None)
    try:
        for want, val in ((hook.DEFAULT_TIMEOUT, None), (30, "30"),
                          (hook.DEFAULT_TIMEOUT, "soon"), (5, "1"), (5, "-9")):
            if val is None:
                os.environ.pop(hook.TIMEOUT_ENV, None)
            else:
                os.environ[hook.TIMEOUT_ENV] = val
            got = hook.timeout_secs()
            if got != want:
                fails += 1
                print(f"  FAIL  timeout_secs({val!r}) expected {want}, got {got}")
    finally:
        os.environ.pop(hook.TIMEOUT_ENV, None)
        if saved_to is not None:
            os.environ[hook.TIMEOUT_ENV] = saved_to

    # Reads the REAL Makefile, so a renamed variable fails here rather than silently defaulting.
    saved = os.environ.pop("RUST_NIGHTLY", None)
    try:
        tc = hook.nightly(REPO)
        if not tc or "nightly" not in tc:
            fails += 1
            print(f"  FAIL  nightly() parsed {tc!r} out of the Makefile's RUST_NIGHTLY")
        os.environ["RUST_NIGHTLY"] = "nightly-2026-07-02"
        if hook.nightly(REPO) != "nightly-2026-07-02":
            fails += 1
            print("  FAIL  a RUST_NIGHTLY in the environment must win, as make's ?= does")
    finally:
        os.environ.pop("RUST_NIGHTLY", None)
        if saved is not None:
            os.environ["RUST_NIGHTLY"] = saved

    if not hook.has_latched_flag(REPO):
        fails += 1
        print("  FAIL  dev::latched_flag! not found — the failure message cites a macro that is gone")

    total = len(CASES) + len(VERDICTS) + len(FIRST_ERROR) + 11
    print(f"release-config-check: {total - fails}/{total} checks correct")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
