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


# Any dotted quad that is NOT loopback, link-local, RFC1918 or the unspecified address. The
# private ranges are excluded here only because the pass above has already replaced them with a
# stable `<lan-ip-N>` label; anything left is routable and belongs to somebody.
# The two trailing guards are not defensive padding, they are the difference between this pass
# and a pass that mangles every log it touches. PMS reports `version=1.43.3.10896-cb3ebc72d`,
# whose leading run is four dot-separated numbers -- so a build suffix (`-abc`) and a `version=`
# prefix both have to be excluded, or every server version in every log becomes `<peer-ip-1>`.
# Caught by `tools/test_scrub_logs.py` before this shipped, which is the only reason it is here.
PUBLIC_IP = re.compile(
    r"(?<!version=)"
    r"(?<![0-9.])(?!0\.)(?!127\.)(?!169\.254\.)(?!10(?:\.\d{1,3}){3}(?![0-9]))"
    r"(?!192\.168\.)(?!172\.(?:1[6-9]|2\d|3[01])\.)"
    r"(?:\d{1,3}\.){3}\d{1,3}(?![0-9])(?!\.\d)(?!-\w)")

# `auth: reached "handle" <addr>:<port> (shared)` -- the server's HANDLE, which names the
# machine and often its owner just as precisely as the address does.
PEER_HANDLE = re.compile(r'(auth: reached\s+)"([^"]+)"')

# **Three more shapes of the same address, and missing them is how the first fix of this stayed
# incomplete for a day.** Plex reaches a server through `https://<dashed-ip>.<hash>.plex.direct`,
# so the SAME address appears a second time with dashes for dots -- invisible to any dotted-quad
# pattern -- alongside a hash derived from the server's machineIdentifier, which identifies it just
# as uniquely. And `(shared by <user>)` names the OWNER outright, which is the disclosure the
# address is only a proxy for.
DASHED_HOST = re.compile(r"(?<![0-9.-])\d{1,3}(?:-\d{1,3}){3}(?=\.[0-9a-f]{20,}\.plex\.direct)")
PLEX_DIRECT_HASH = re.compile(r"(?<=\.)[0-9a-f]{20,}(?=\.plex\.direct)")
SHARED_BY = re.compile(r"(\(shared by\s+)([^)]+)(\))")


def _label(pattern, text, tag, group=0):
    """Replace `pattern`'s `group` with a stable `<tag-N>`, keeping the rest of the match.

    `(text, distinct_labels)`. Splicing by OFFSET rather than rebuilding the match from its groups
    is what lets one function serve a whole-match pattern and a "keep the surrounding literal"
    pattern alike -- `auth: reached "<peer-name-1>"` still reads as a reachability result, and
    `(shared by <peer-owner-1>)` still reads as a sharing note, without either spelling its own
    f-string. Numbering is per shape and by first appearance, so two distinct peers stay
    distinguishable in a scrubbed log while the same peer keeps one name throughout.
    """
    seen = {}
    def sub(m):
        name = seen.setdefault(m.group(group), f"<{tag}-{len(seen) + 1}>")
        whole, start = m.group(0), m.start()
        return whole[:m.start(group) - start] + name + whole[m.end(group) - start:]
    return pattern.sub(sub, text), len(seen)


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

    # **PUBLIC addresses too, and this pass is the one that matters.** Redacting RFC1918 while
    # publishing routable addresses is exactly backwards for the threat: a LAN address identifies
    # nobody off the LAN, whereas a public address and port identify a REAL HOST belonging to a
    # real person -- and the app reaches other people's servers by construction. `auth: reached
    # "<handle>" <ip>:<port> (shared...)` is logged for every server the account can see, so a
    # friend's shared server lands in EVERY captured log, under an address that appears in no
    # declared private file because it arrives at runtime from plex.tv.
    #
    # This was not hypothetical. 42 occurrences across 21 committed logs survived a commit whose
    # message was "scrub third-party and device identifiers from the captured logs", because both
    # passes above are blind to a routable address: the first only knows values read out of
    # `PRIVATE_FILES`, the second only matches RFC1918. Third-party data is not the maintainer's
    # to publish and this repository is public (`third-party-share-data`).
    peers = {}
    def _pub(m):
        if guard.CIDR_TAIL.match(text[m.end():m.end() + 2]):
            return m.group(0)
        return peers.setdefault(m.group(0), f"<peer-ip-{len(peers) + 1}>")
    text = PUBLIC_IP.sub(_pub, text)

    # The handle beside the address is the same disclosure by another route -- it names the machine
    # and usually its owner. Then the dashed form of the address, the machineIdentifier-derived
    # hash beside it, and the owner's account name outright. Each is a complete identifier alone.
    #
    # One pass per shape, through ONE `_label`, and the count comes back with it. Each shape used
    # to carry its own dict, its own closure and its own `setdefault` line, and the total was a
    # hand-written sum of six `len()`s at the end -- so a sixth identifier shape needed a remembered
    # term in that sum, and forgetting it under-reports. An under-count here does not read as a bug:
    # it reads as "nothing was scrubbed", on the tool whose whole job is proving otherwise.
    labelled = 0
    for pattern, tag, group in ((PEER_HANDLE, "peer-name", 2),
                                (DASHED_HOST, "peer-host", 0),
                                (PLEX_DIRECT_HASH, "plex-direct-hash", 0),
                                (SHARED_BY, "peer-owner", 2)):
        text, n = _label(pattern, text, tag, group)
        labelled += n

    return text, replaced + len(hosts) + len(peers) + labelled


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
