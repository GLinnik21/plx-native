#!/usr/bin/env python3
"""PostToolUse: type-check the RELEASE feature set after a Rust edit — the one configuration
nothing else in this project compiles until a release cut.

THE GAP, MEASURED. `rust-modules/Cargo.toml` declares `default = ["devtools", "devtriggers"]`, and
`make RELEASE=1` drops both (`RUST_FEATFLAGS = $(if $(RELEASE),--no-default-features,)`, Makefile
line 304). Everything that routinely compiles this crate builds the DEFAULT set: `make check`
(`cargo +$(RUST_NIGHTLY) test --lib`, line 666), `make lint` (line 699), the plain cross-build
(line 438), and `make sim` (line 875 — `--features hostsim` ON TOP of the defaults, not instead of
them). So does CI's pull-request gate — `.github/workflows/ci.yml` runs `make check` (line 50),
then the plain cross-build (line 98), then `make ipk FLAVOR=debug` (line 108), and never drops a
feature. The release job is the only CI job that does, and it turns the flag on through an **`env:`
block** — `RELEASE: "1"`, `release.yml` line 236 — so a grep for the literal `RELEASE=1` does not
find the build at all: `grep -rnE 'no-default-features|RELEASE=1' .github/` returns exactly two
hits, `release.yml` lines 252-253, and both are the ASSERTION that the flag took effect, fired
after `make ipk` has already run. Exactly one other command in the tree drops both features —
`make macapp` (`ci/mkmacapp.py` line 84: `--release --no-default-features --features hostsim`) —
and it is neither routine nor the same configuration: it is run to mail somebody a Mac bundle, and
it puts `hostsim` back on top, which swaps `player::ffi`'s whole seam.

Measured on this tree, 2026-08-23: **27** `cfg(feature = "devtriggers")` sites and **7**
`cfg(feature = "devtools")` sites in `rust-modules/src` — every one of them a place where the
shipping build compiles different text from the tested build, first COMPILED during a release cut.
**24 of those 27 have a `cfg(not(feature = "devtriggers"))` twin**, i.e. are one half of a
hand-written PAIR — which is not a footnote, it is the exact shape that broke (below), and the
count of live instances of it. Re-count both with (no backslashes on purpose — this is a
docstring, and Python 3.12 turns an invalid escape into a SyntaxWarning printed on every edit):

    grep -rho 'cfg(feature = "[a-z]*"' rust-modules/src --include='*.rs' | sort | uniq -c
    grep -rho 'cfg(not(feature = "[a-z]*"' rust-modules/src --include='*.rs' | sort | uniq -c

And `[lints.rust] warnings = "deny"` makes the gap sharper, not softer: an import that only the
`devtriggers` arm consumed does not warn in the release configuration, it FAILS it.

WHY A HOOK AND NOT A CONVENTION. Because the convention exists, is written down, and did not fire.
The project memory note `release-config-not-covered-by-check` records the shape and one instance:
on 2026-08-21 a new `#[cfg(feature = "devtriggers")]`-gated function inserted BETWEEN an existing
pair's attribute and its `fn` orphaned that attribute onto the new function's doc comment.
`density_max_sweep`'s devtriggers arm lost its gate and collided with its own `#[cfg(not(...))]`
twin — `error[E0428]: defined multiple times`, only under `--no-default-features`, with 786/786
tests green and every other target clean throughout. That break is invisible from inside the
editing session: nothing you can run reproduces it unless you already suspect it, which is the
definition of a check that should not be a habit.

WHAT IT COSTS. Measured on the dev Mac, 2026-08-23, warm target dir, after `touch`ing one source
file: **0.52 / 0.54 / 0.60 s** over three trials for `cargo +nightly check --lib
--no-default-features`, and **0.52 s** for the same command with the default set — call it 0.55 s
for either. An edit to anything else costs **0.02 s**, the whole hook, measured both ways: a
non-`.rs` path is rejected on the extension before even the `git rev-parse`, and a `.rs` outside
this crate's `src/` is rejected on pure path arithmetic after it. Cargo is never started for
either. A second run of the DEFAULT set happens only when the release set already failed — see the message below, which needs to know
whether the break is release-specific or shared, and cannot know from one run. Both runs share ONE
budget (`PLX_RELEASE_CHECK_TIMEOUT`, default 90 s) rather than one each, because a cap that can be
doubled is not a cap. The harness applies its OWN per-hook timeout on top and the smaller of the
two wins, so wire this in `.claude/settings.json` with a `timeout` at least as large as the cap
(`"timeout": 120` beside the default 90) or the 90 is decorative. Being killed that way is benign —
a dead hook is a missed check, exactly like the lock-wait path — but it is not what the number
says, and `tv-lock-guard.py` is wired at `timeout: 15`, which would truncate this one hard.

WHY THE SHARED `target/`, AND NOT A PRIVATE `--target-dir`. Cargo keys unit fingerprints by
feature set, so both configurations coexist in one directory without evicting each other. Measured
by alternating the two commands with no edit in between, 2026-08-23: after each has run once,
EVERY subsequent run of either reports `Finished … in 0.03s`. That is the whole reason the number
above is 0.55 s and not a rebuild. A private target dir was measured the same day — **12.99 s** to
populate cold and **339 MB** on disk, per worktree — and disk is not a theoretical cost here:
agents run in parallel `.claude/worktrees/` checkouts — `git worktree list` showed three beside
this one on 2026-08-23 — and this project has already had a disk fill from per-worktree builds
(memory `worktree-fleet-hazards`). The price of sharing is the build-directory LOCK, handled below.

WHAT IT DELIBERATELY DOES NOT CHECK.
  * **The default feature set.** `make check`, `make lint`, CI and the `rust-analyzer-lsp` plugin
    enabled in `.claude/settings.json` all cover it, the last of them live in the editor. This
    hook is for the configuration with no other reader. (A break in the default set usually shows
    up here anyway, since almost all of the crate is shared text — so when it does, the message
    says so rather than blaming the release configuration for a plain compile error.)
  * **`hostsim`.** A third configuration, off by default, built only by `make sim`, and it ships to
    nobody. It has the same structural exposure and a much smaller blast radius.
  * **`rust-modules/src/bin/sim.rs`** and anything else under `src/bin/`: `--lib` does not compile
    them, and `required-features = ["hostsim"]` means the release configuration never does either.
    Firing there would spend the 0.55 s and grade nothing.
  * **`rust-modules/build.rs`.** It early-returns on `CARGO_FEATURE_HOSTSIM` being unset and emits
    nothing, and cargo compiles it in every configuration regardless — so a break in it is a break
    `make check` already sees.
  * **`rust-modules/Cargo.toml`**, where the feature list itself lives. Firing there would cost the
    same 0.6 s (measured — a manifest touch invalidates no more than a source touch does), so cost
    is not the reason; scope is. This hook grades a Rust edit's SIDE EFFECT on a configuration the
    author was not thinking about. Someone editing `[features]` is thinking about exactly that, and
    a one-line change here would extend it if that stops being true.
  * **`#[cfg(test)]` code**, which `cargo check --lib` does not compile. If a release-only break
    ever hides in a test module, this will not be what finds it.
  * **A file in ANOTHER lane's worktree.** The crate checked is the one under the repo root
    resolved from this call's `cwd` (`git rev-parse --show-toplevel`, so a worktree is its own
    lane, exactly as `tv-lock-guard.py` does it). An edit reaching sideways into
    `.claude/worktrees/<other>/rust-modules/src/` is not checked here: running cargo in another
    lane's target dir is the per-worktree build cost this hook just argued against, and that lane
    will check the file on its own next edit.

THE BUILD-DIRECTORY LOCK IS A WAIT, NOT A FAILURE. Cargo takes an exclusive lock on `target/`, so a
concurrent `make check` in this lane makes this call BLOCK ("Blocking waiting for file lock on
build directory") rather than fail. On timeout the hook prints a note and exits 0. Reporting a lock
wait as a broken build would be a lie that costs the model a debugging detour, and the next edit
re-runs the check anyway.

WHICH NIGHTLY. The same one the Makefile uses — `RUST_NIGHTLY` is parsed out of the Makefile
(`?= nightly`, line 175; CI pins a date), and a `RUST_NIGHTLY` in the environment wins, matching
make's `?=`. The toolchain is not incidental: CLAUDE.md records `task.rs`'s refused-spawn test
passing 284/284 on stable while panicking inside `std` on nightly, and `-Z build-std` means
nightly is what ships. If that toolchain is not installed the check is SKIPPED, not failed —
and `RUSTUP_AUTO_INSTALL=0` is set for the subprocess so rustup refuses instead of quietly starting
a several-hundred-MB download behind an ordinary Edit. Measured 2026-08-23 on rustup 1.29.0, with a
name that does not exist: **0.24 s** of network ("info: syncing channel updates for …") before
giving up, versus **0.01-0.02 s** refusing outright with the variable set. The quarter-second is
not the point and should not be quoted as the cost — it is what a MISS costs; a toolchain name
that really exists is where the hundreds of megabytes are, and there is no timing to measure
because nobody would sit through it. A `RUSTFLAGS` already in the environment is inherited rather
than cleared, so `Cargo.toml`'s documented `RUSTFLAGS="--cap-lints=warn"` mid-edit escape keeps
working through this hook too.

THE ESCAPE HATCH is `PLX_RELEASE_CHECK_SKIP=1` in the environment (and
`PLX_RELEASE_CHECK_TIMEOUT=<seconds>` to move the cap, default 90). Unlike the TV lock's bypass it
is NOT a command prefix, and that asymmetry is deliberate: this hook fires on Edit/Write, which
carry no command line for an agent to decorate, so the hatch is reachable only by the human who
started the session — `export PLX_RELEASE_CHECK_SKIP=1`, or an `env` entry in
`.claude/settings.json`. It is for a human deliberately holding the release arm broken across a
long refactor. There is no reason for it during a release cut; that is when the check is the point.

FAIL-OPEN ON ITS OWN BUGS, and fail-open on cargo's too. A crash here must not wedge every edit in
the session, and neither must a missing toolchain, a missing cargo, or a lock wait — only real
compiler diagnostics reach exit 2. Contract: exit 0 = silent pass, exit 2 = stderr is fed back to
the model as a problem to fix, anything else is a hook error shown to the user.

The cost of that generosity is that a bug in this file reads as a PASS, so the one place it must
not be casual is decoding cargo's output — see `run_check`, where `text=True`'s locale default was
a real fail-open (found 2026-08-23, by feeding the hook a release-only `compile_error!` whose
message carried an em-dash). The general rule, which this project files under
`[[silent-instrument-trap]]`: before trusting the silence, prove the instrument can see the thing.
`python3 .claude/hooks/release-config-check-test.py` grades the pure halves; the half it cannot
reach is deliberately proven by hand — splice a release-only break into a file under
`rust-modules/src`, confirm exit 2 with real compiler text, and revert.
"""
import json
import os
import re
import subprocess
import sys
import time

# EVERY byte this hook reads or writes is UTF-8, stated rather than inherited. The interpreter's
# default is the LOCALE codec, and under an ascii one (`LC_ALL=C` with PEP 538 coercion off, which
# is how a lean launcher can plausibly start a hook) a strict decode RAISES — whereupon the
# fail-open catch at the bottom turns a proven release break into exit 0. Measured 2026-08-23: with
# a release-only `compile_error!` whose message carried an em-dash, the hook exited 0 saying
# "internal error, allowing ('ascii' codec can't decode byte 0xe2 …)". Three separate readers had
# to be fixed for one bug — cargo's output, the Makefile that `nightly()` parses, and the `dev.rs`
# that `has_latched_flag()` greps — because this repo's Makefile, sources and diagnostics are all
# full of em-dashes, and each reader failed in turn as the previous one was corrected. The stream
# reconfiguration below is the write half: an em-dash in the report itself would otherwise
# UnicodeEncodeError on the same terminal.
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except Exception:       # pre-3.7, or a stream that is not a TextIOWrapper — not worth failing
        pass

SKIP_ENV = "PLX_RELEASE_CHECK_SKIP"
TIMEOUT_ENV = "PLX_RELEASE_CHECK_TIMEOUT"
DEFAULT_TIMEOUT = 90            # warm 0.55s, cold-into-an-empty-dir 13.0s; the rest is lock headroom
                                # — and it is the budget for BOTH runs together, see main()
DIAG_CAP = 4000                 # bytes of compiler output forwarded; the reproduce command has the rest

# The tools that can change a source file. NotebookEdit is here for completeness of the set and can
# never actually match — it only edits `.ipynb`, and its path arrives under a different key.
EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")
PATH_KEYS = ("file_path", "notebook_path")

# Cargo's own progress chatter, dropped before the diagnostics are forwarded. rustc's rendering
# never begins with one of these words, so this cannot eat a diagnostic body.
CARGO_NOISE = re.compile(
    r"^\s*(Compiling|Checking|Finished|Blocking|Updating|Downloading|Downloaded|Locking|Fresh"
    r"|Adding|Removing)\b")

# What separates "the code is broken" from "the check could not run". A denied lint renders as a
# plain `error:` line and still ends in `could not compile`, so that phrase carries the deny gate
# as well as ordinary type errors.
COMPILE_FAIL = re.compile(
    r"^error\[E\d+\]|^error: could not compile|^error: aborting due to", re.M)


def rust_src_target(payload, root, cwd=None):
    """The crate source file this payload edited, or None if the hook must not fire.

    PURE — no subprocess, no filesystem beyond path normalisation — so the test file can drive it
    with a table. Everything the hook decides before spending 0.55 s lives here.

    `root` is the lane (the repo root); relative paths resolve against `cwd`, which is where the
    tool actually ran and is not always the root.
    """
    if payload.get("tool_name") not in EDIT_TOOLS:
        return None
    ti = payload.get("tool_input")
    if not isinstance(ti, dict):
        return None
    # PostToolUse normally only follows a successful call, but a payload that says otherwise is
    # taken at its word: there is nothing on disk to check after a refused edit.
    resp = payload.get("tool_response")
    if isinstance(resp, dict) and resp.get("success") is False:
        return None

    path = ""
    for key in PATH_KEYS:
        val = ti.get(key)
        if isinstance(val, str) and val.strip():
            path = val.strip()
            break
    if not path.endswith(".rs"):
        return None

    if not os.path.isabs(path):
        path = os.path.join(cwd or root, path)
    path = os.path.realpath(path)

    src = os.path.realpath(os.path.join(root, "rust-modules", "src"))
    if not path.startswith(src + os.sep):
        return None
    # `--lib` compiles no bin target, and `sim.rs` needs `hostsim`, which the release set never has.
    if path.startswith(os.path.join(src, "bin") + os.sep):
        return None
    return path


def verdict(returncode, output):
    """'ok' | 'broken' | 'unusable' — the last meaning cargo never got as far as compiling.

    PURE. 'unusable' is the fail-open bucket: an uninstalled pinned nightly, no cargo on PATH, a
    corrupt lock. Those must not be reported to the model as a release-configuration break, because
    they are not one and the fix is nowhere near the edit.
    """
    if returncode == 0:
        return "ok"
    if COMPILE_FAIL.search(output or ""):
        return "broken"
    return "unusable"


def first_error(text):
    """The line worth quoting when the check could not RUN. PURE.

    The first line that says `error`, not the first non-empty one: on the paths that land here
    cargo has usually printed `Checking plxnative-modules v0.4.1 (…)` first, and a note reading
    "could not run the check (Checking plxnative-modules)" tells the reader nothing about the
    uninstalled toolchain that actually stopped it.
    """
    lines = [l.strip() for l in (text or "").splitlines() if l.strip()]
    for l in lines:
        if l.lower().startswith("error"):
            return l[:160]
    return (lines[0][:160] if lines else "no output")


def trim(text, cap=DIAG_CAP):
    """Compiler output, stripped of cargo's progress lines and capped. PURE."""
    body = "\n".join(l for l in (text or "").splitlines() if not CARGO_NOISE.match(l)).strip()
    if len(body) > cap:
        body = body[:cap].rstrip() + f"\n… [trimmed at {cap} bytes — the command below has the rest]"
    return body


def nightly(root):
    """The toolchain the Makefile would use. Env wins, matching make's `RUST_NIGHTLY ?=`."""
    env = os.environ.get("RUST_NIGHTLY", "").strip()
    if env:
        return env
    try:
        with open(os.path.join(root, "Makefile"), encoding="utf-8", errors="replace") as f:
            for line in f:
                m = re.match(r"^RUST_NIGHTLY\s*\??=\s*(\S+)", line)
                if m:
                    return m.group(1)
    except OSError:
        pass
    return "nightly"


def has_latched_flag(root):
    """Whether `dev::latched_flag!` is really in this tree — the message must not cite a ghost."""
    try:
        with open(os.path.join(root, "rust-modules", "src", "dev.rs"),
                  encoding="utf-8", errors="replace") as f:
            return "macro_rules! latched_flag" in f.read()
    except OSError:
        return False


def timeout_secs():
    try:
        return max(5, int(os.environ.get(TIMEOUT_ENV, "")))
    except ValueError:
        return DEFAULT_TIMEOUT


def repo_root(cwd):
    try:
        out = subprocess.run(["git", "-C", cwd, "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, encoding="utf-8",
                             errors="replace", timeout=5)
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except Exception:
        pass
    return os.path.abspath(cwd)


def run_check(root, flags, toolchain, secs):
    """(returncode, combined output), or None if it hit the timeout (a lock wait, most likely)."""
    env = dict(os.environ)
    # The Makefile prefixes this on every cargo line; a hook can be launched with a leaner PATH.
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + env.get("PATH", "")
    # A `cargo +<toolchain>` for a toolchain rustup does not have DOWNLOADS IT. Measured 2026-08-23
    # on rustup 1.29.0: `cargo +nightly-1999-01-01 check` spends ~0.24 s reaching the network
    # ("info: syncing channel updates for …") before giving up, and a name that really EXISTS would
    # instead pull a few hundred MB — kicked off silently behind an ordinary Edit, on a hook whose
    # whole budget is 0.55 s. With this set it refuses in 0.01-0.02 s and `verdict` files it as
    # 'unusable', which is the honest answer: this lane cannot run the check, and that is not a
    # broken build.
    env["RUSTUP_AUTO_INSTALL"] = "0"
    cmd = ["cargo", f"+{toolchain}", "check", "--lib"] + list(flags)
    try:
        # `encoding`/`errors` EXPLICIT, not `text=True`'s locale default — see the note at the top
        # of the module. rustc echoes the offending SOURCE LINE into every diagnostic, so this is
        # the reader that meets a non-ASCII byte on exactly the run that found a break.
        p = subprocess.run(cmd, cwd=os.path.join(root, "rust-modules"), env=env,
                           capture_output=True, text=True, encoding="utf-8",
                           errors="replace", timeout=secs)
    except subprocess.TimeoutExpired:
        return None
    except OSError as e:
        return (127, f"error: could not run cargo: {e}")
    return (p.returncode, (p.stderr or "") + (p.stdout or ""))


def report(root, edited, diags, default_also_broken, toolchain):
    rel = os.path.relpath(edited, root)
    if default_also_broken:
        seen_by = ("  default set:   ALSO fails — so this is NOT release-specific. `make check`\n"
                   "                 catches it too; fix it once and both configurations clear.\n")
    else:
        seen_by = ("  default set:   compiles CLEAN — so `make check`, `make lint`, the CI\n"
                   "                 pull-request gate and rust-analyzer in the editor all stay\n"
                   "                 green on this. Nothing but a release cut would have found it.\n")
    msg = (
        "RELEASE-CONFIG BREAK: the crate does not compile with --no-default-features.\n"
        f"  edited:        {rel}\n"
        "  configuration: --no-default-features (devtools OFF, devtriggers OFF) — what\n"
        "                 `make RELEASE=1` builds and what ships to users.\n"
        + seen_by +
        "\n" + (diags or "(cargo reported a failure but produced no diagnostics)") + "\n"
        "\nReproduce, verbatim:\n"
        f"  cd rust-modules && cargo +{toolchain} check --lib --no-default-features\n")
    if not default_also_broken and has_latched_flag(root):
        msg += (
            "\nIf this is a hand-written #[cfg(feature = \"devtriggers\")] / #[cfg(not(...))] PAIR:\n"
            "that shape is what produced this class of break on 2026-08-21, when a function spliced\n"
            "in between an attribute and its `fn` swallowed the neighbour's gate (E0428, only under\n"
            "--no-default-features, 786/786 tests green). Most such pairs are unnecessary —\n"
            "`dev::flag` is already compile-time `false` and `dev::read` `None` without the feature,\n"
            "so a helper that only wraps them needs no cfg at all. Prefer `crate::dev::latched_flag!`\n"
            "(rust-modules/src/dev.rs) over hand-rolling a pair.\n")
    return msg


def main():
    if os.environ.get(SKIP_ENV, "").strip() not in ("", "0", "false"):
        return 0
    try:
        payload = json.loads(sys.stdin.read())
    except Exception:
        return 0
    if not isinstance(payload, dict):
        return 0

    cwd = payload.get("cwd") or os.getcwd()
    # Cheap classification first: an edit to anything but this crate's lib sources costs no
    # subprocess at all, not even the `git rev-parse`.
    ti = payload.get("tool_input")
    if payload.get("tool_name") not in EDIT_TOOLS or not isinstance(ti, dict):
        return 0
    if not any(isinstance(ti.get(k), str) and ti[k].strip().endswith(".rs") for k in PATH_KEYS):
        return 0

    root = repo_root(cwd)
    edited = rust_src_target(payload, root, cwd)
    if not edited:
        return 0

    toolchain, secs = nightly(root), timeout_secs()
    t0 = time.monotonic()
    res = run_check(root, ["--no-default-features"], toolchain, secs)
    if res is None:
        print(f"release-config-check: cargo did not finish in {secs}s — most likely another build "
              f"holds the target/ lock. Skipped; the next edit re-runs it.")
        return 0
    rc, out = res
    state = verdict(rc, out)
    if state == "ok":
        return 0
    if state == "unusable":
        print(f"release-config-check: could not run the check ({first_error(out)}). Skipped.")
        return 0

    # It IS broken. One more run — the default set — because "release-only" is the whole claim this
    # hook makes, and a shared compile error would otherwise be reported as a release regression.
    # ONE budget across both runs, not one each: the configured cap is what a caller reasoned
    # about, and `.claude/settings.json`'s per-hook `timeout` has to exceed it or the harness kills
    # the hook mid-report. (Unreachable in practice — the second run only happens because the first
    # COMPILED and errored, so it was fast — but a cap that can be doubled is not a cap.)
    left = max(5, int(secs - (time.monotonic() - t0)))
    dres = run_check(root, [], toolchain, left)
    default_also_broken = dres is not None and verdict(dres[0], dres[1]) == "broken"

    sys.stderr.write(report(root, edited, trim(out), default_also_broken, toolchain))
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:      # a bug in this hook must not wedge every edit in the session
        sys.stderr.write(f"release-config-check: internal error, allowing ({e})\n")
        sys.exit(0)
