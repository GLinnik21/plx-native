#!/usr/bin/env python3
"""Rewrite captured device logs into a committable form, replacing private values with labels.

The counterpart to `tools/scrub-gate.py`: the gate REFUSES a file that carries a private value,
this one produces the version that passes. Both read the private set through
`.claude/hooks/outbound-guard.py`, so neither can drift from the PreToolUse hook.

WHY IT EXISTS. On 2026-08-26 captured logs were committed and pushed carrying a third party's
server address, port, machine hash and handle. The app enumerates the signed-in account's servers
at boot, so **even a no-Plex pipeline case logs them** -- which is the trap: a tier that needs no
token still produces a log that cannot be published. Scrubbing by hand is how the leak happened.

Placeholders are STABLE and DISTINCT: one private value always becomes the same label and two
different values never share one, so a reader can still tell two hosts apart and follow a session
across lines. Structure survives; identity does not.

    tools/scrub-logs.py --out docs/measurements/p1-logs /tmp/abr-p1-logs/*.log

It VERIFIES ITS OWN OUTPUT through the same `findings()` the gate calls, and exits non-zero if
anything survived -- because a scrubber that quietly misses one is worse than none: it produces a
file everybody now believes is safe. It never prints a matched value.
"""
import argparse, importlib.util, os, re, shutil, sys


def load_guard(root):
    path = os.path.join(root, ".claude", "hooks", "outbound-guard.py")
    if not os.path.isfile(path):
        sys.exit("scrub-logs: outbound-guard.py not found; refusing rather than guessing.")
    spec = importlib.util.spec_from_file_location("outbound_guard", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def slug(label):
    """`PMS_HOST (src/config.local.h)` -> `pms-host`; `.tv-host` -> `tv-host`."""
    base = label.split("(")[0].strip().lstrip(".")
    return re.sub(r"[^a-z0-9]+", "-", base.lower()).strip("-") or "private"


def scrub(text, guard, secrets):
    """(scrubbed text, how many distinct values were replaced)."""
    replaced = 0
    seen = {}
    # Longest first: a value that contains another must be replaced before its substring, or the
    # inner one masks part of the outer and leaves a recognisable fragment behind.
    for label, value in sorted(secrets, key=lambda lv: -len(lv[1])):
        if not value or value not in text:
            continue
        name = seen.setdefault(value, slug(label))
        text, n = guard.literal_re(value).subn(f"<{name}>", text)
        if n:
            replaced += 1
    # Any remaining RFC1918 address is a host this repo has not declared -- the dev Mac, a
    # router, a second television. Numbered so two of them stay distinguishable.
    hosts = {}
    def _ip(m):
        if guard.CIDR_TAIL.match(text[m.end():m.end() + 2]):
            return m.group(0)
        return hosts.setdefault(m.group(0), f"<lan-ip-{len(hosts) + 1}>")
    text = guard.PRIV_IP.sub(_ip, text)
    return text, replaced + len(hosts)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("logs", nargs="+")
    ap.add_argument("--out", required=True, help="directory to write scrubbed copies into")
    args = ap.parse_args(argv)

    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    guard = load_guard(root)
    secrets = guard.load_secrets(root)
    if not secrets:
        print("scrub-logs: no private files present — nothing to scrub against.", file=sys.stderr)
    os.makedirs(args.out, exist_ok=True)

    failed = []
    for path in args.logs:
        body = open(path, encoding="utf-8", errors="replace").read()
        clean, n = scrub(body, guard, secrets)
        dest = os.path.join(args.out, os.path.basename(path))
        with open(dest, "w", encoding="utf-8") as fh:
            fh.write(clean)
        # The whole point: prove the output is clean rather than assume the substitution was.
        left = guard.findings(clean, secrets, guard.default_published(root))
        status = "clean" if not left else f"STILL DIRTY ({len(left)} finding(s))"
        if left:
            failed.append(dest)
        print(f"  {status:<28} {os.path.relpath(dest, root)}  ({n} value(s) replaced)")

    if failed:
        print(f"\nFAILED: {len(failed)} scrubbed file(s) still carry private data. "
              "Not safe to commit.", file=sys.stderr)
        for f in failed:
            os.remove(f)
        print("The unsafe outputs were deleted rather than left for someone to trust.",
              file=sys.stderr)
        return 1
    print(f"\nOK: {len(args.logs)} file(s) scrubbed and verified.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
