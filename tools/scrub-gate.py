#!/usr/bin/env python3
"""Refuse to publish a file that carries a value from a gitignored private file.

WHY THIS EXISTS. On 2026-08-26 a batch of captured device logs was committed and pushed carrying a
third-party server's address, port, machine hash, custom domain and its owner's handle. The
pre-commit check that should have stopped it *computed the match count and printed it* instead of
gating on it, and the push went out anyway. This is that check, written so it cannot be ignored:
a match is a non-zero exit.

It NEVER prints the matched value -- that is the same leak by a shorter route -- only the count.

It reuses `.claude/hooks/outbound-guard.py`'s own `load_secrets` and `findings`, so the gate and the
PreToolUse hook cannot drift apart. The hook guards command TEXT (a PR body, a heredoc); this guards
file CONTENT, which is the hole the 2026-08-26 leak went through.

    tools/scrub-gate.py FILE...          # explicit files
    tools/scrub-gate.py --staged         # everything git has staged
    tools/scrub-gate.py --untracked      # staged + untracked, i.e. anything a commit -A would take
"""
import importlib.util, os, subprocess, sys


def repo_root():
    return subprocess.run(["git", "rev-parse", "--show-toplevel"],
                          capture_output=True, text=True, check=True).stdout.strip()


def load_guard(root):
    path = os.path.join(root, ".claude", "hooks", "outbound-guard.py")
    if not os.path.isfile(path):
        sys.exit("scrub-gate: outbound-guard.py not found; refusing rather than guessing.")
    spec = importlib.util.spec_from_file_location("outbound_guard", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def git_files(root, mode):
    args = ["git", "-C", root, "diff", "--cached", "--name-only", "--diff-filter=ACMR"]
    out = subprocess.run(args, capture_output=True, text=True).stdout.split()
    if mode == "untracked":
        out += subprocess.run(["git", "-C", root, "ls-files", "--others", "--exclude-standard"],
                              capture_output=True, text=True).stdout.split()
    return [os.path.join(root, f) for f in dict.fromkeys(out)]


def main(argv):
    root = repo_root()
    guard = load_guard(root)
    secrets = guard.load_secrets(root)
    if not secrets:
        print("scrub-gate: no private files present — nothing to compare against.", file=sys.stderr)

    mode = None
    files = []
    for a in argv:
        if a == "--staged":
            mode = mode or "staged"
        elif a == "--untracked":
            mode = "untracked"
        else:
            files.append(a)
    if mode:
        files += git_files(root, mode)
    files = [f for f in dict.fromkeys(files) if os.path.isfile(f)]
    if not files:
        print("scrub-gate: no files to check.")
        return 0

    print(f"scrub-gate: {len(secrets)} private literal(s) from "
          f"{len(guard.PRIVATE_FILES)} declared file(s); {len(files)} file(s) to check")
    blocked = []
    for f in files:
        try:
            body = open(f, encoding="utf-8", errors="replace").read()
        except OSError as e:
            print(f"  SKIP     {os.path.relpath(f, root)} ({e.strerror})")
            continue
        hits = guard.findings(body, secrets)
        rel = os.path.relpath(f, root)
        if hits:
            labels = sorted({h[0] if isinstance(h, (tuple, list)) else str(h) for h in hits})
            print(f"  BLOCKED  {rel}: {len(hits)} match(es) from {', '.join(labels)}")
            blocked.append(rel)
        else:
            print(f"  clean    {rel}")

    if blocked:
        print(f"\nREFUSED: {len(blocked)} file(s) carry private data.")
        print("Scrub to placeholders before committing. The repo is PUBLIC and some of this data")
        print("is not the maintainer's to publish; see docs/shared-servers.md for the stand-in table.")
        return 1
    print("\nOK: no private literals found.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
