#!/usr/bin/env python3
"""Run `sentry-cli debug-files check` on each file and require it to show symbols.

    SENTRY_CLI="npx --yes @sentry/cli@3.7.0" ci/check-debug-files.py FILE...

`debug-files check` exits 0 for a stripped object that holds nothing to symbolicate, so the exit
status alone proves nothing: the files listed here exist to name frames, and a file whose reported
features contain neither `debug` nor `symtab` would upload cleanly and then fail, silently, on the
first real crash inside it. Each file's output is printed in full (the workflow log is the record of
what Sentry will be able to do with it), then one summary line per file.

Fails when a file is missing, reports `Usable: no`, or reports no `debug`/`symtab` feature. The
feature list is read from the block between `Contained debug information` and `Usable`; a feature
spelled `name: no|false|none` does not count as present.
"""
import os
import re
import shlex
import subprocess
import sys


def features(output):
    """The wanted features (`debug`, `symtab`) the output reports, or None without the block."""
    block = re.search(r"Contained debug information[^\n]*\n(.*?)(?:^\s*Usable\b|\Z)", output, re.S | re.M | re.I)
    if not block:
        return None
    found = set()
    for match in re.finditer(r"\b(debug|symtab)\b(\s*[:=]\s*(?:no|false|none|0)\b)?", block.group(1), re.I):
        if not match.group(2):
            found.add(match.group(1).lower())
    return found


def usable_no(output):
    return re.search(r"^\s*Usable\s*:\s*no\b", output, re.M | re.I) is not None


def main(argv):
    files = argv[1:]
    if not files:
        print("usage: SENTRY_CLI=... check-debug-files.py FILE...", file=sys.stderr)
        return 2
    cli = shlex.split(os.environ.get("SENTRY_CLI", "sentry-cli"))
    failures = []
    summary = []
    for path in files:
        if not os.path.isfile(path) or os.path.getsize(path) == 0:
            failures.append(f"{path}: missing or empty")
            continue
        result = subprocess.run([*cli, "debug-files", "check", path], stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True)
        print(f"--- {path}\n{result.stdout}", flush=True)
        if result.returncode:
            failures.append(f"{path}: debug-files check exited {result.returncode}")
            continue
        if usable_no(result.stdout):
            failures.append(f"{path}: reported Usable: no")
            continue
        found = features(result.stdout)
        if found is None:
            failures.append(f"{path}: no 'Contained debug information' block in the output "
                            "(has the sentry-cli output format changed?)")
        elif not found:
            failures.append(f"{path}: reports neither debug nor symtab features - nothing to symbolicate with")
        else:
            summary.append(f"{path}: {', '.join(sorted(found))}")
    for line in summary:
        print("ok   " + line)
    for line in failures:
        print("FAIL " + line, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
