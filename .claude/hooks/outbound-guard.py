#!/usr/bin/env python3
"""PreToolUse guard: nothing carrying this repo's private values may be published.

WHY A HOOK AND NOT A CONVENTION. The convention already exists, in writing, and it already failed.
`docs/shared-servers.md` opens with "none of that data is ours" and a table of stand-ins — and its
own next paragraph records that the rule "stood here, in these words, while the repo published the
friend's handle, their machine name, their library name, their real port and their LAN address
across ~139 sites in committed code and four PR bodies (2026-08-14)". Those four (PRs #28-31) were
a batch of subagents told to write device-verification recipes "executable without me"; each did
the obviously helpful thing and pasted the real credentials block in. All four were redacted, but
GitHub keeps PR-body edit history, so those values are permanently public. The data was the
friend's, not the maintainer's, and they were not a party to any of it.

That is the shape this guards: not malice and not carelessness, but an agent being helpful with a
value it can read. A memory file is not in the loop at the moment a PR body is composed; a
PreToolUse hook is. github.com/GLinnik21/plx-native is PUBLIC.

WHAT IT COSTS. Classification is pure string work over the command line, and nothing is opened —
not the gitignored files, not even the repo root — unless a segment is first graded outbound, so
the 99% of Bash calls that are `cargo check`, `grep`, `make -s print-*` do no I/O here at all. That
last clause was not free: `main` used to resolve the repo root before asking whether anything was
outbound, which forked a `git rev-parse` on every Bash call in the session and cost 30 ms of it.
`grade()` is split out of `verdict` so the question can be asked without one. Re-measured on the
dev Mac 2026-08-23, median of 15: **23 ms** for a command that is not outbound, **31 ms** for one
that is, **46 ms** when a shape hit forces the `git grep` described below. Bare `python3 -c pass`
is 13 ms on this machine, so most of the floor is interpreter start-up and none of it is ours to
remove. No network, and the television is never contacted from here.

WHAT IT BLOCKS, AND WHY THAT LIST. Exactly the commands whose text lands somewhere this repo's
author cannot take it back from — somebody else's server, or the permanent history of a public
repository:
  * `gh pr create|edit|comment|review`, `gh issue create|edit|comment`, `gh release create|edit`,
    and ANY `gh` invocation carrying `--body`/`--body-file`/`-b`/`--notes`/`--notes-file`;
  * `gh release upload`, where the payload is a FILE rather than text — the .ipk is fine and is
    the normal case, `pkg/auth.json` is the shape to stop, and the path check below is what
    separates them;
  * `gh api` with a body flag (`-f`/`-F`/`--field`/`--raw-field`/`--input`) or a non-GET method.
    Bare `gh api … --jq` is a READ and stays allowed — `ci/verify-published.sh` and
    `docs/release-audits/README.md` are built on it, as are `gh release view|download` and
    `gh pr view`. `--json body` is a read too: this matches the FLAG `--body`, never the word.
  * `git commit` with `-m`/`-F`/`--message`/`--file` (short clusters like `-am` included),
    `git tag -m/-F`, `git notes … -m/-F`;
  * `curl`/`wget` carrying a request body or a query string — but see the destination gate below;
  * any of the above naming one of the gitignored private files as an ARGUMENT, plus `git add -f`
    on one (plain `git add` of an ignored path already fails on git's own refusal, so `-f` is the
    only reachable spelling) — but never as the value of a flag that carries TEXT. `gh pr create
    --body ".tv-host"` publishes eight characters, and refusing it would refuse the exact advice
    this hook's own refusal message ends on: name the file, do not paste the value. Verification
    found that false positive on 2026-08-23; `TEXT_FLAGS` is the fix and is scoped per command,
    because `-f` is a string field on `gh` and a boolean on `git add` whose next word is the path.
  * a private file read by a `$( )` or a backtick INSIDE an outbound payload — `--body "tv is
    $(cat .tv-host)"` names the file, reads it and publishes the contents on one visible line.
    Also found on 2026-08-23, and also previously allowed: the substitution's own segment is a
    `cat`, which is not outbound, and the outer token is quoted prose, which the path check skips.

WHY `git push` IS DELIBERATELY NOT THE GATE. By push time the message is already an object in the
local history and the interesting half of the leak has happened; `git commit` is where the text is
composed and is therefore where a refusal can still be acted on. Push is also the ordinary end of
every task and is reachable a dozen other ways (`gh pr create` pushes, so does any IDE), so gating
it would buy nothing and would teach the reader to reach for the bypass on a command they run all
day. The one thing push-time gating would add — catching a secret in the FILE CONTENTS of a commit
— this hook cannot do at any point, and `.gitignore` is that gate (see WHAT IT CANNOT SEE).

HEREDOC BODIES ARE SCANNED — THE EXACT OPPOSITE OF `tv-lock-guard.py`, ON PURPOSE. That hook strips
heredoc bodies before classifying, because there a body is data being written and reading it turns
`cat > SKILL.md <<'EOF'` … `make deploy` … `EOF` into a refused deploy. Here the body IS the
payload: `gh pr create --body-file - <<'EOF' … EOF` is the normal way a PR body of any length is
passed, and a guard that strips it would scan the word `--body-file` and nothing else. So this file
does both, at different stages — bodies are stripped for CLASSIFICATION (a `gh pr create` quoted
inside a doc being written is not a publish) and kept for SCANNING, attached to the segment that
opened them so a heredoc on one line cannot condemn a `gh` command on the next. Named
`--body-file`/`--notes-file`/`-F`/`--input`/`-T`/`-d @file` paths are read and scanned the same way.

`git commit` WITH NO MESSAGE FLAG IS NOT SCANNED, and `.git/COMMIT_EDITMSG` is deliberately left
alone. At PreToolUse time that file still holds the PREVIOUS commit's message — git rewrites it
when it opens the editor, which is after this hook has already answered — so scanning it grades
text that was committed hours ago and produces a refusal about a message the author cannot even
see. The flagless form is also not the shape that leaks from an agent: a non-interactive Bash tool
has no editor, so `git commit` with no `-m` hangs or fails rather than committing. `--amend
--no-edit` reuses a message this hook saw when it was first written.

WHAT IT MATCHES. Two halves, and they fail in opposite directions on purpose.

(a) LITERAL VALUES, read at the moment the hook runs out of the gitignored files that hold them —
    `.tv-host` (the dev television's address), `.tv-mac` (its Wake-on-LAN MAC), `src/config.local.h`
    (`PMS_HOST`/`PMS_TOKEN`, the real Plex owner token), `tests/manifest.local.json` (PMS host, the
    test user's plex.tv id, this library's ratingKeys, and the shared_server block when there is
    one), `pkg/auth.json` (the persisted session: `account_token` and each source's per-server
    token), plus three more that `.gitignore` names and its comments explain — `local.env` ("real
    PMS host + X-Plex-Token"), `.tv-dpad-pass` (a generated password; "NEVER commit a password")
    and `.tv-remote-url` ("never commit a URL that is live while it is live"). Missing files are
    skipped, so a checkout with none of them simply has less to match. An exact substring hit on a
    real secret is never a false positive, which makes this the precise half.

    THE FLOOR IS 8 CHARACTERS, and values containing whitespace are skipped. Both are earned by the
    actual contents. Under 8 the files hold only things that are already public or that identify
    nothing: the port (`32400`, in every doc), a cosmetic display name (the template calls it
    "display name, cosmetic"), and the `items` ratingKeys — which are 1 to 4 digits and which
    CLAUDE.md prints in its own prose ("`rk=4` is shared by five cases, `rk=1804` by three"). A
    guard that matched those would refuse any commit message with a standalone 4 in it, starting with
    that sentence of CLAUDE.md's. The whitespace
    rule drops the templates' `_comment` prose, which is verbatim public in the tracked
    `.example` files; keys beginning `_` are skipped for the same reason. Nothing that is actually
    dialable falls under the floor — the shortest RFC1918 address is `10.0.0.1`, exactly 8 — and
    the shape rules below backstop it anyway. Matching is case-insensitive because a MAC and a
    40-hex id get rewritten in either case constantly.

(b) SHAPE PATTERNS, for values a checkout might not hold but that still must not go out:
    `X-Plex-Token=`/`plexToken=` with a real-looking value, a 20-character mixed-case-and-digit
    alnum run inside a URL (the shape of a Plex token), a 40-hex id in a machineIdentifier context,
    and a private-range IPv4 (10/8, 172.16/12, 192.168/16). This half is the imprecise one, so its
    three sharpest edges are filed down deliberately:

    * A 40-HEX RUN IS ONLY MATCHED IN CONTEXT (`machineIdentifier=`, `machine_id:`,
      `clientIdentifier=`, `X-Plex-Client-Identifier`). A git sha1 is also 40 hex, and this repo's
      release audits are full of them — `docs/release-audits/README.md` greps hashes and shas out
      of published bodies and artifacts as a matter of routine. A guard that refuses `git commit -m "revert
      <40-hex>"` is a guard that gets switched off.
    * A URL TOKEN MUST CARRY A DIGIT, A LOWERCASE AND AN UPPERCASE. An all-lowercase 20-char run in
      a URL is far more likely to be a path segment than a token. This trades a possible miss (the
      literal half covers the token that actually exists here) for not refusing ordinary links.
    * A PRIVATE QUAD THAT A TRACKED FILE ALREADY CONTAINS IS NOT A LEAK, so shape hits on the
      IPv4 and 40-hex rules are dropped when `git grep -F` finds the value in a tracked file. This
      is not a loophole, it is the whole point: `docs/shared-servers.md`'s stand-in table nominates
      `10.9.9.7` as the LAN-address placeholder — RFC1918 on purpose, "as the real one is", and it
      is now in five tracked files including `plex/account.rs` and `plex/probe.rs` — and
      `docs/plex-openapi.json` carries `10.0.0.42` eight times as upstream Plex's own example.
      Without this rule the guard refuses the very placeholder the docs tell you to use. It is
      applied to those two shape rules and NOT to the literal half or the token rules: a real
      credential sitting in a tracked file is an incident to clean up, never a licence to republish.

    THE DESTINATION GATE ON curl/wget IS LOAD-BEARING. `curl "http://<pms>:32400/library/sections?
    X-Plex-Token=<real token>"` is the single most common debugging command in this repo, and it
    contains two real secrets by necessity — but it sends them to the machine they belong to. So a
    curl or wget whose URLs are all loopback, RFC1918, a dotless host, one under `.local`/`.lan`/
    `.home`/`.internal`, or one under `plex.tv` (where a Plex token legitimately goes — that is how
    `tests/run.py` resolves the test user's per-server token) is not outbound at all.
    Anything aimed off the LAN is.

WHAT IT DELIBERATELY DOES NOT MATCH, beyond the floor: RFC 5737 documentation addresses
(`192.0.2.x`, `198.51.100.x`, `203.0.113.x`) — this repo uses them as placeholders on purpose, and
so does `tv-lock-guard-test.py`; `1.2.3.4`, which is the Makefile's own example; `127.0.0.1`,
`0.0.0.0` and `localhost`; a partial range like `192.168.x` or `172.20.x.x` (naming a range is not
naming a host, it is not dialable, and the manifest template does it); a quad written as CIDR
(`172.16.0.0/12`); `<placeholder>` and `YOUR_*` forms, so nothing in
`tests/manifest.local.json.example` or `src/config.local.h.example` can ever trip it; and `alpine`,
the webosbrew dev-mode root password, which CLAUDE.md and `docs/distribution.md` both keep on
purpose because it is published, identical on every rooted set, and identifies nobody.

WHAT IT CANNOT SEE, stated so nobody reads a pass as proof:
  * FILE CONTENTS in a commit. This grades command TEXT, not the diff — `git add -A && git commit
    -m "wip"` with a secret inside a newly tracked file is `.gitignore`'s job, not this hook's.
  * A SHELL VARIABLE. `--body "$BODY"` and `X-Plex-Token=$TOKEN` expand after the hook has answered;
    refusing an unexpanded `$VAR` would be a false positive on every scripted call, so it does not.
    Its close relative IS caught, and the difference is worth stating: a `$( )` or backtick that
    NAMES a gitignored file is visible text and is refused (above), while `TV=$(cat .tv-host)` on
    one line and `--body "$TV"` on the next is not — the value crosses through a variable and this
    hook sees only one command line at a time.
  * A SCRIPT THAT PUBLISHES INTERNALLY. Only the top-level command line is visible, so
    `ci/verify-published.sh` is one word here whatever `gh` it runs — the same structural limit
    `tv-lock-guard.py` has.
  * AN EDITOR SESSION, per the COMMIT_EDITMSG note above. This is a scanner, not a permission
    system: it refuses commands whose visible payload contains a private value, and it does not try
    to have an opinion about publishing as such.

IT NEVER PRINTS THE MATCHED VALUE. The refusal names which secret and which file it came from, and
stops there — writing the value into the transcript is the same leak by a shorter route. For the
same reason the refusal does not advertise the bypass: the next step after this message is a
placeholder, not a way past it.

That sentence stood here while it was FALSE, which is the worst way for a claim like it to be
wrong — a reader has no reason to re-check a guard's own promise. The refusal echoes the offending
command back so the author can see what was refused, and until 2026-08-23 it echoed the value with
it: `git commit -m "server at <the TV's address>"` came back verbatim, address included. Found by
driving the real binary against a throwaway root of fake secrets and grepping its stderr for them,
which is the only way to check this and is now a step in the test. `redact()` is the fix; the
heredoc body and any `--body-file` contents were never echoed and still are not.

THE ESCAPE HATCH is a prefix on the command itself — `PLX_PUBLISH_BYPASS=1 gh pr create …`. It is
for a human publishing something the scanner mis-read (a doc that must quote a real-looking address
is the realistic case). An agent reaching for it is deciding, on the maintainer's behalf and
usually on a third party's behalf, to publish an address that cannot be un-published. Use a
stand-in from `docs/shared-servers.md`'s table instead, or ask.

FAIL-OPEN ON ITS OWN BUGS, fail-closed on the answer: a crash in this file must not wedge every
Bash call in the session, but a match is a refusal. Contract: exit 0 allows, exit 2 blocks and
feeds stderr back to the model.
"""
import json
import os
import re
import subprocess
import sys

BYPASS = re.compile(r"\bPLX_PUBLISH_BYPASS=1\b")

# The gitignored files that hold private values. All eight are named in `.gitignore`, and seven of
# them carry their own reason in its comments — `src/config.local.h` and `local.env` under "local
# dev overrides (real PMS host + X-Plex-Token)", `.tv-host` as "the maintainer's home network, not
# a project fact", `tests/manifest.local.json` as "the private library inventory", `pkg/auth.json`
# as holding "plex.tv and PMS tokens, and this repository is public", and `.tv-dpad-pass` /
# `.tv-remote-url` under "NEVER commit a password, and never commit a URL that is live while it is
# live". `.tv-mac` is the one bare line. (An earlier draft of this comment split the list five/three
# by whether a reason was attached; checked against the file, that split does not exist.)
# Logs (`.tv-stream.log`) are deliberately absent: they are large, they churn, and the private
# thing in them is the TV address, which `.tv-host` already supplies.
PRIVATE_FILES = (
    ".tv-host",
    ".tv-mac",
    "src/config.local.h",
    "tests/manifest.local.json",
    "pkg/auth.json",
    # The Lab Diagnostics session (`docs/lab-diagnostics.md`): a bearer secret, a certificate pin
    # and an endpoint that is the developer's own static address. Written by
    # `tools/plxnative-lab start`, staged into a `make LAB=1` package, and gitignored — the exact
    # shape of thing this hook exists to keep out of a PR body.
    "pkg/lab.json",
    "local.env",
    ".tv-dpad-pass",
    ".tv-remote-url",
)

MIN_LITERAL = 8                 # see the docstring: below this the files hold only public things
MAX_READ = 512 * 1024           # a payload file this big is not a PR body

# Values that are in these files sometimes but identify nothing, plus the two the project keeps on
# purpose. Compared lowercased.
GENERIC = frozenset({
    "localhost", "127.0.0.1", "0.0.0.0", "255.255.255.255", "1.2.3.4",
    "32400", "8910", "8911", "alpine", "example.com", "stable", "debug",
})
PLACEHOLDER = re.compile(
    r"^(?:<.*>|\{\{.*\}\}|\$\{?\w+\}?|(?:your|my|the)[_\-].+|x{3,}|redacted|changeme"
    r"|placeholder|dummy|fake|unset|none|null|true|false)$", re.I)
DOC_NET = re.compile(r"^(?:192\.0\.2|198\.51\.100|203\.0\.113)\.\d{1,3}$")
DOTTED = re.compile(r"^[0-9.]+$")


# ---------------------------------------------------------------- secrets ---

def usable(value):
    """Is this value specific enough that an exact match on it is evidence of a leak?"""
    v = str(value).strip()
    if len(v) < MIN_LITERAL or re.search(r"\s", v):
        return False
    if "<" in v or ">" in v:
        return False
    if v.lower() in GENERIC or PLACEHOLDER.match(v) or DOC_NET.match(v):
        return False
    return True


def _add(out, label, value):
    v = str(value).strip().strip('"').strip("'")
    if usable(v):
        out.append((label, v))


def _walk_json(out, node, path, origin):
    if isinstance(node, dict):
        for k, v in node.items():
            if k.startswith("_"):       # the templates' own explanatory prose, public verbatim
                continue
            _walk_json(out, v, "%s.%s" % (path, k) if path else k, origin)
    elif isinstance(node, list):
        for i, v in enumerate(node):
            _walk_json(out, v, "%s[%d]" % (path, i), origin)
    elif node is not None and not isinstance(node, bool):
        _add(out, "%s (%s)" % (path, origin), node)


def load_secrets(root):
    """[(label, value)] read from the gitignored files under `root`. Missing files are skipped.

    `root` is a parameter rather than a module constant so the test can point it at a temporary
    directory of FAKE secrets — no real value belongs in a tracked test file.
    """
    out = []
    for rel in PRIVATE_FILES:
        path = os.path.join(root, rel)
        try:
            if not os.path.isfile(path) or os.path.getsize(path) > MAX_READ:
                continue
            body = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        if rel.endswith(".json"):
            try:
                _walk_json(out, json.loads(body), "", rel)
            except Exception:
                pass                    # a half-written session file is not worth a refusal
        elif rel.endswith(".h"):
            for m in re.finditer(r"^[ \t]*#[ \t]*define[ \t]+(\w+)[ \t]+(.+?)[ \t]*$", body, re.M):
                _add(out, "%s (%s)" % (m.group(1), rel), m.group(2))
        elif rel.endswith(".env"):
            for m in re.finditer(r"^[ \t]*(?:export[ \t]+)?(\w+)[ \t]*=[ \t]*(.+?)[ \t]*$",
                                 body, re.M):
                _add(out, "%s (%s)" % (m.group(1), rel), m.group(2))
        else:
            first = (body.strip().splitlines() or [""])[0]
            _add(out, rel, first)
    return out


def literal_re(value):
    """Boundary-anchored, case-insensitive match for one secret.

    The boundary class differs by shape and it is asymmetric, which is not obvious. For a dotted
    value the neighbour to exclude on the right is a digit, or a further `.digit`: `10.0.0.1` must
    not match inside `10.0.0.11`, but it MUST still match in "the set is at 10.0.0.1." — a full
    stop after an address is prose, not another octet. The first version of this rule excluded
    digits and dots alike, so every address at the end of a sentence went straight through; the
    test caught it on a `--body-file`, which is exactly where a body ends in a full stop. For
    everything else (tokens, MACs, numeric ids) alphanumeric boundaries are right — a 9-digit
    account id must not match inside a 10-digit one.
    """
    if DOTTED.match(value):
        return re.compile(r"(?<![0-9.])" + re.escape(value) + r"(?![0-9])(?!\.\d)", re.I)
    return re.compile(r"(?<![0-9A-Za-z])" + re.escape(value) + r"(?![0-9A-Za-z])", re.I)


# ------------------------------------------------------------ shape rules ---

TOKEN_KV = re.compile(
    r"(?i)\b(x[-_]plex[-_]token|plextoken|plex[-_]token)[\"']?\s*[=:]\s*[\"']?([A-Za-z0-9_\-]{8,})")
URL_TOKEN = re.compile(r"https?://[^\s\"'<>]*?(?<![A-Za-z0-9])([A-Za-z0-9]{20})(?![A-Za-z0-9])")
MACHINE_ID = re.compile(
    r"(?i)\b(machineidentifier|machine[_-]?id|clientidentifier|x-plex-client-identifier)"
    r"[\"']?\s*[=:]\s*[\"']?([0-9a-f]{40})\b")
PRIV_IP = re.compile(
    r"(?<![0-9.])(?:"
    r"10(?:\.\d{1,3}){3}"
    r"|192\.168(?:\.\d{1,3}){2}"
    r"|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2}"
    r")(?![0-9])(?!\.\d)")
CIDR_TAIL = re.compile(r"^/\d")


def plausible_secret(v):
    return not (PLACEHOLDER.match(v) or v.lower() in GENERIC or v.startswith("$"))


def variable_reference(text, match):
    """Is this `X-Plex-Token` value a NAME the same text defines, rather than a secret?

    `{"X-Plex-Token": auth_token}` in source is a reference; `X-Plex-Token=<a-real-20-char-token>`
    is a leak. Both match TOKEN_KV, and "looks like an identifier" cannot separate them -- a real
    Plex token is twenty alphanumerics and matches that too.

    Two conditions, and BOTH are required. The value must be UNQUOTED, because a quoted string
    after the key is a literal whatever it spells. And the same text must assign that exact name
    at statement level, which is what a variable has and a secret does not: for a real token to
    pass here the file would have to open a line with the token itself followed by `=`, i.e. use
    it as a variable name.

    This exists because `tools/scrub-gate.py` reads FILE CONTENT rather than command text, and on
    its first real run it refused `tests/run.py` -- the harness's own source -- over two variable
    references. A gate that cries wolf on the files it is meant to wave through is a gate people
    stop reading, which is precisely the failure that produced the leak it was built to prevent.
    """
    if match.start(2) > 0 and text[match.start(2) - 1] in "\"'":
        return False
    return re.search(r"(?m)^\s*%s\s*=(?!=)" % re.escape(match.group(2)), text) is not None


def mixed_run(v):
    """A Plex-token-shaped run: 20 alnum with a digit, a lowercase AND an uppercase in it."""
    return (any(c.isdigit() for c in v) and any(c.islower() for c in v)
            and any(c.isupper() for c in v))


def findings(text, secrets, published=None):
    """[(what matched, how)] for one piece of outbound text. Never returns the value itself.

    `published(value) -> bool` answers "is this already in a tracked file", and is injectable so
    the test can supply one without a git repo. See the docstring: it suppresses the two shape
    rules that collide with this repo's own documented placeholders, and nothing else.
    """
    pub = published or (lambda _v: False)
    hits, seen = [], set()

    def note(what, how):
        if what not in seen:
            seen.add(what)
            hits.append((what, how))

    for label, value in secrets:
        if literal_re(value).search(text):
            note("the literal value of %s" % label, "exact match on a gitignored value")

    for m in TOKEN_KV.finditer(text):
        if plausible_secret(m.group(2)) and not variable_reference(text, m):
            note("an %s= parameter with a real-looking value" % m.group(1), "shape rule")
            break
    for m in URL_TOKEN.finditer(text):
        if mixed_run(m.group(1)) and plausible_secret(m.group(1)):
            note("a 20-character Plex-token-shaped run inside a URL", "shape rule")
            break
    for m in MACHINE_ID.finditer(text):
        if not pub(m.group(2)):
            note("a 40-hex machineIdentifier", "shape rule")
            break
    for m in PRIV_IP.finditer(text):
        if CIDR_TAIL.match(text[m.end():m.end() + 2]):
            continue                    # `172.16.0.0/12` names a range, not a host
        if pub(m.group(0)):
            continue                    # already in a tracked file — see docs/shared-servers.md
        note("a private-range IPv4 address", "shape rule")
        break
    return hits


MASK = "[redacted]"


def redact(text, secrets):
    """`text` with every value either half of this file can match replaced by `[redacted]`.

    THE REFUSAL ECHOES THE COMMAND BACK, and until 2026-08-23 it echoed the secret with it:
    `git commit -m "server at <the TV's address>"` produced a refusal whose second line was that
    command verbatim, address included. The docstring above already promised the opposite, which
    is the worst version of this — a claim the reader has no reason to re-check. Found by driving
    the real binary against a throwaway root of fake secrets and grepping the stderr for them.

    Echoing the command line back is not the same leak as printing a file's contents (the command
    was in the transcript before this hook ran; a `--body-file`'s contents were not), but it is a
    leak by any route where hook stderr outlives the tool call, and the fix is four lines. The
    heredoc body and the payload file are never echoed at all.

    Deliberately does NOT apply the suppressions `findings` applies — no `published()`, no
    `mixed_run`, no DOC_NET. Over-redacting costs the reader a placeholder they can still see in
    their own command; under-redacting costs the thing this function exists for. Redaction runs
    BEFORE the 140-character truncation, so a cut cannot leave half an address standing.
    """
    spans = []
    for _label, value in secrets:
        spans += [m.span() for m in literal_re(value).finditer(text)]
    for rx, grp in ((TOKEN_KV, 2), (URL_TOKEN, 1), (MACHINE_ID, 2)):
        spans += [m.span(grp) for m in rx.finditer(text)]
    spans += [m.span() for m in PRIV_IP.finditer(text)]
    if not spans:
        return text
    out, at = [], 0
    for start, end in sorted(spans):
        if start < at:
            continue                    # overlapping match, already masked
        out.append(text[at:start])
        out.append(MASK)
        at = end
    out.append(text[at:])
    return "".join(out)


def default_published(root):
    """`git grep -F` memoized: is this exact value already in a tracked file under `root`?"""
    cache = {}

    def is_pub(value):
        if value not in cache:
            try:
                r = subprocess.run(["git", "-C", root, "grep", "-qFI", "-e", value],
                                   capture_output=True, timeout=5)
                cache[value] = (r.returncode == 0)
            except Exception:
                cache[value] = False    # cannot prove it is public → do not suppress
        return cache[value]
    return is_pub


# --------------------------------------------------------- command parsing ---

def tokens(seg):
    """[(token, was_quoted)] — a quote-aware split, so `--body "a b"` is ONE value token.

    `str.split()` cannot be used here: it would turn a prose body into a dozen tokens and the
    path check below would then read the words of a sentence as filenames.
    """
    out, cur, quoted, quote, i = [], [], False, None, 0
    while i < len(seg):
        c = seg[i]
        if quote:
            if c == "\\" and quote == '"' and i + 1 < len(seg):
                cur.append(seg[i + 1])
                i += 2
                continue
            if c == quote:
                quote = None
                i += 1
                continue
            cur.append(c)
            i += 1
            continue
        if c in "'\"":
            quote, quoted = c, True
            i += 1
            continue
        if c == "\\" and i + 1 < len(seg):
            cur.append(seg[i + 1])
            i += 2
            continue
        if c.isspace():
            if cur or quoted:
                out.append(("".join(cur), quoted))
            cur, quoted = [], False
            i += 1
            continue
        cur.append(c)
        i += 1
    if cur or quoted:
        out.append(("".join(cur), quoted))
    return out


def words(seg):
    return [t for t, _q in tokens(seg)]


def quote_state(text, quote=None):
    """The open quote character at the end of `text`, or None. Used to join continued lines."""
    i = 0
    while i < len(text):
        c = text[i]
        if quote:
            if c == "\\" and quote == '"' and i + 1 < len(text):
                i += 2
                continue
            if c == quote:
                quote = None
            i += 1
            continue
        if c in "'\"":
            quote = c
        elif c == "\\" and i + 1 < len(text):
            i += 1
        i += 1
    return quote


def segments(cmd):
    """Split a shell command into segments on ; && || | & — QUOTE-AWARE.

    DELIBERATE COPY of `tv-lock-guard.py::segments`, not an import. Two hooks that must both keep
    working when the other is deleted, ~40 lines, no state: a third file to share it would add a
    dependency edge between two independent guards for less code than this comment saves. The
    reason it is quote-aware is the same there as here — a plain regex split cuts inside
    `grep -n "a|b"` and hands the tail to the classifier as if it were a command.
    """
    out, cur, quote, i = [], [], None, 0
    while i < len(cmd):
        c = cmd[i]
        if quote:
            cur.append(c)
            if c == quote and cmd[i - 1:i] != "\\":
                quote = None
            i += 1
            continue
        if c in "'\"":
            quote = c
            cur.append(c)
            i += 1
            continue
        if c == "\\" and i + 1 < len(cmd):
            cur.append(c)
            cur.append(cmd[i + 1])
            i += 2
            continue
        if c in ";\n|&":
            i += 2 if cmd[i:i + 2] in ("&&", "||") else 1
            out.append("".join(cur))
            cur = []
            continue
        cur.append(c)
        i += 1
    out.append("".join(cur))
    for inner in re.findall(r"\$\(([^()]*)\)", cmd):
        if inner.strip():
            out.extend(segments(inner))
    return [seg for seg in out if seg.strip()]


HEREDOC = re.compile(r"<<-?\s*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")


def continued(text):
    # RAW docstring, and it has to stay raw: the prose below quotes `\` and `` \` `` as shell
    # syntax, and in a plain string those are invalid escape sequences. Python 3.12 turned that
    # from a silent DeprecationWarning into a SyntaxWarning printed on stderr at import — which
    # this hook's own selftest reads, so four malformed-payload cases failed on CI while passing
    # on a 3.9 Mac. A docstring broke a gate on one machine and not another.
    r"""Does this line end in a shell line-continuation backslash?

    Found by verification on 2026-08-23, and it was a hole through the whole hook rather than a
    corner of one: `gh pr create --title x \` / newline / `  --body "<secret>"` was allowed. The
    per-line unit for the second line has `--body` as its command word, which is not `gh`, `git`
    or `curl`, so the segment was never graded outbound and its text was never scanned. A
    multi-line `gh` invocation is the ordinary way an agent writes one, so this was not an exotic
    input — the existing test table simply had no case with a backslash in it.

    An EVEN number of trailing backslashes is an escaped backslash, not a continuation, and
    trailing whitespace after the `\` means the shell does not continue either — hence the exact
    `endswith` rather than an `rstrip`.
    """
    if not text.endswith("\\"):
        return False
    return (len(text) - len(text.rstrip("\\"))) % 2 == 1


def units(cmd):
    """[(command text, heredoc bodies opened on it)] — the payload attribution.

    Two things this does that a plain `splitlines()` does not, both of which are holes otherwise:
    a line whose quote is still open swallows the next line (so a multi-line `--body "…"` stays one
    unit and its later lines are scanned), and a heredoc body is attached to the line that OPENED
    it rather than to the whole command. The second is what keeps `cat > notes.md <<EOF` (secret)
    `EOF` `gh pr create --body "clean"` from being a refusal — the body belongs to the `cat`.
    """
    lines = cmd.split("\n")
    out, i = [], 0
    while i < len(lines):
        text = lines[i]
        i += 1
        while i < len(lines) and (quote_state(text) or continued(text)):
            if quote_state(text):
                text += "\n" + lines[i]
            else:
                text = text[:-1] + " " + lines[i]   # the `\` joins; it is not content
            i += 1
        bodies = []
        for m in HEREDOC.finditer(text):
            if text[m.start() - 1:m.start()] == "<":
                continue                # `<<<'x'` is a herestring, not a heredoc
            delim = m.group(2)
            body = []
            while i < len(lines) and lines[i].strip() != delim:
                body.append(lines[i])
                i += 1
            i += 1                      # consume the terminator
            bodies.append("\n".join(body))
        out.append((text, "\n".join(bodies)))
    return out


def command_word(w):
    """First word that is not an env assignment or a wrapper/keyword left by the split."""
    for tok in w:
        if not tok.startswith("/") and re.match(r"^\w+=", tok):
            continue
        if tok in ("sudo", "time", "env", "nohup", "exec", "command",
                   "do", "then", "else", "{", "(", "!", "&&", "||"):
            continue
        return tok
    return ""


# `gh` flags that carry text to github.com. `-b`/`-F` are `--body`/`--body-file` on pr|issue and
# `--field`/`--raw-field` on api; either way the argument is published.
GH_BODY_FLAGS = {"--body", "-b", "--body-file", "-F", "--notes", "--notes-file",
                 "-f", "--field", "--raw-field", "--input"}
GH_WRITE = {("pr", "create"), ("pr", "edit"), ("pr", "comment"), ("pr", "review"),
            ("issue", "create"), ("issue", "edit"), ("issue", "comment"),
            ("release", "create"), ("release", "edit"), ("release", "upload"),
            ("gist", "create")}
GIT_MSG_FLAGS = {"-m", "--message", "-F", "--file"}
CURL_BODY_FLAGS = {"-d", "--data", "--data-raw", "--data-binary", "--data-urlencode", "--data-ascii",
                   "--json", "--form", "--form-string", "-T", "--upload-file",
                   "--post-data", "--post-file", "--body-data", "--body-file"}
# Flags whose argument is a PATH whose CONTENTS get published.
FILE_FLAGS = {"--body-file", "--notes-file", "-F", "--file", "--input", "-T", "--upload-file",
              "-d", "--data", "--data-binary", "--data-raw", "--form", "--post-file"}
WRITE_METHODS = {"post", "put", "patch", "delete"}
LOCAL_HOST = re.compile(
    r"^(?:localhost|127(?:\.\d{1,3}){3}|0\.0\.0\.0|\[::1\]"
    r"|10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2}"
    r"|[^./]+|[^/]*\.(?:local|lan|home|internal)|(?:[^/]*\.)?plex\.tv)$", re.I)


def flag_values(w, names):
    """Values of `--flag V` / `--flag=V` for any flag in `names`."""
    out = []
    for i, tok in enumerate(w):
        if "=" in tok and tok.split("=", 1)[0] in names:
            out.append(tok.split("=", 1)[1])
        elif tok in names and i + 1 < len(w):
            out.append(w[i + 1])
    return out


def has_flag(w, names):
    return any(t in names or (("=" in t) and t.split("=", 1)[0] in names) for t in w)


# Global options that take a SEPARATE value, and so hide the subcommand behind them.
# `git -C /repo commit -m x` and `gh -R owner/repo pr create` both read as the wrong command
# without this — the first non-flag word is the flag's argument, not the verb.
VALUE_GLOBALS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path",
                 "-R", "--repo", "--hostname"}


def subcommands(args):
    out, skip = [], False
    for tok in args:
        if skip:
            skip = False
            continue
        if tok in VALUE_GLOBALS:
            skip = True
            continue
        if tok.startswith("-"):
            continue
        out.append(tok)
    return out


def local_url(u):
    m = re.match(r"^[a-z][a-z0-9+.\-]*://(?:[^/@]*@)?([^/:?#]+)", u, re.I)
    return bool(m) and bool(LOCAL_HOST.match(m.group(1)))


def outbound(seg):
    """Reason string if this segment publishes its text, else None."""
    w = words(seg)
    if not w:
        return None
    cw = command_word(w)
    base = os.path.basename(cw)
    args = w[w.index(cw) + 1:] if cw in w else w[1:]

    if base == "gh":
        subs = subcommands(args)
        pair = tuple(subs[:2])
        if has_flag(args, GH_BODY_FLAGS):
            return "a gh command carrying text (--body/--body-file/--notes/--field)"
        if pair in GH_WRITE:
            return "gh %s %s publishes to github.com" % pair
        if subs[:1] == ["api"]:
            if any(m.lower() in WRITE_METHODS for m in flag_values(args, {"-X", "--method"})):
                return "gh api with a write method"
        return None

    if base == "git":
        subs = subcommands(args)
        sub = subs[0] if subs else ""
        cluster = [t for t in args
                   if re.match(r"^-[A-Za-z]+$", t) and ("m" in t[1:] or "F" in t[1:])]
        msg = has_flag(args, GIT_MSG_FLAGS) or bool(cluster)
        if sub in ("commit", "tag") and msg:
            return "git %s writes this text into the repository's permanent history" % sub
        if sub == "notes" and msg:
            return "git notes writes this text into the repository"
        if sub == "add" and has_flag(args, {"-f", "--force"}):
            return "git add -f overrides .gitignore"
        if sub == "commit":
            # A path argument on `git commit` stages that file. No message flag, so nothing to
            # scan — but naming a private file is caught by the path check.
            return "git commit stages the paths it names"
        return None

    if base in ("curl", "wget"):
        urls = [t for t in args if re.match(r"^[a-z][a-z0-9+.\-]*://", t, re.I)]
        body = has_flag(args, CURL_BODY_FLAGS)
        query = any("?" in u for u in urls)
        if not (body or query):
            return None
        if urls and all(local_url(u) for u in urls):
            return None                 # sending a Plex token to the Plex server is not a leak
        return "%s sends a body or query string to a host off this LAN" % base
    return None


# Flags whose argument is TEXT, per command. A path-shaped string here is a STRING — `gh pr create
# --body ".tv-host"` publishes those eight characters and nothing else. Scoped per command because
# one letter means two things: `-f` is a string field on `gh api`/`gh workflow run`, while
# `git add -f` is a boolean whose NEXT word is the path this hook exists to catch. A single flat
# set gets one of those two wrong whichever way it is written.
TEXT_FLAGS = {
    "gh": {"--body", "-b", "--notes", "--title", "-t", "-f", "--raw-field", "-n"},
    "git": {"-m", "--message"},
    "curl": {"--data-raw", "--form-string", "--body-data"},
    "wget": {"--post-data", "--body-data"},
}
SUBST = re.compile(r"\$\(([^()]*)\)|`([^`]*)`")


def named_private_paths(seg, root, cwd):
    """[(gitignored file, how it got here)] for private files this segment actually READS or SENDS.

    Two things it must separate, and the guard's own refusal message depends on getting the second
    one right — it ends by telling the reader to "name the gitignored FILE it comes from" instead
    of pasting the value. A hook that then refuses `gh pr create --body ".tv-host"` is refusing the
    advice it just gave. So a text flag's argument is never treated as a path (verification found
    that false positive on 2026-08-23; the case is now in the table).

    The other direction is the one a `$VAR` cannot excuse: `--body "tv is $(cat .tv-host)"` names
    the file, reads it, and publishes the contents, all on the visible command line. That was
    allowed until the same pass — the substitution's own segment is a `cat`, which is not outbound,
    and the outer token is quoted prose, which is skipped. Backticks are read here too, unlike in
    `tv-lock-guard.py::segments`, and for the opposite reason: there a backtick is overwhelmingly
    markdown in a document being written, while here it can only be inside a payload that is
    already graded outbound.
    """
    hits, seen = [], set()
    targets = {os.path.realpath(os.path.join(root, rel)): rel for rel in PRIVATE_FILES}

    def look(text, how, skip_after):
        prev = None
        for tok, quoted in tokens(text):
            was, prev = prev, tok
            if was in skip_after:
                continue                # the value of a text flag is text, not a filename
            if quoted and re.search(r"\s", tok):
                continue                # prose: "the token lives in src/config.local.h"
            t = tok.lstrip("@")
            if "@" in t:
                t = t.split("@", 1)[1]  # curl's `-F body=@file`
            if not t or t.startswith("-"):
                continue
            rp = os.path.realpath(t if os.path.isabs(t) else os.path.join(cwd, t))
            rel = targets.get(rp)
            if rel and rel not in seen:
                seen.add(rel)
                hits.append((rel, how))

    base = os.path.basename(command_word(words(seg)))
    look(seg, "named as an argument", TEXT_FLAGS.get(base, frozenset()))
    for m in SUBST.finditer(seg):
        inner = m.group(1) or m.group(2) or ""
        if inner.strip():
            look(inner, "read by a command substitution inside the payload", frozenset())
    return hits


def payload_parts(seg, bodies, cwd):
    """[(where, text)] — everything this segment publishes, including files it points at."""
    parts = [("the command line", seg)]
    if bodies.strip():
        parts.append(("the heredoc body", bodies))
    for val in flag_values(words(seg), FILE_FLAGS):
        v = val.lstrip("@")
        if "@" in v:
            v = v.split("@", 1)[1]
        if not v or v == "-":
            continue                    # `--body-file -` is the heredoc, already attached
        path = v if os.path.isabs(v) else os.path.join(cwd, v)
        try:
            if os.path.isfile(path) and os.path.getsize(path) <= MAX_READ:
                parts.append((v, open(path, encoding="utf-8", errors="replace").read()))
        except OSError:
            pass
        except Exception:
            pass
    return parts


def grade(cmd):
    """[(segment, reason, heredoc bodies)] for the parts of `cmd` that publish their text.

    Split out of `verdict` so `main` can ask the question WITHOUT a repo root, because resolving
    one costs a `git rev-parse` fork. Measured 2026-08-23: with that fork on the common path an
    ordinary `cargo check` paid 30 ms here, against 13.5 ms for starting Python and doing nothing;
    asking this first drops it to ~15 ms and makes the docstring's "no I/O on a command that is
    not outbound" true rather than nearly true. Re-grading inside `verdict` afterwards is pure
    string work and does not show up in the measurement.

    Bodies are stripped for CLASSIFICATION (a `gh pr create` quoted inside a document being
    written is not a publish) and kept for SCANNING. That split is the whole inversion this file
    makes against tv-lock-guard.py; the docstring explains why each half is right.
    """
    graded = []
    for text, bodies in units(cmd):
        for seg in segments(text):
            reason = outbound(seg)
            if reason:
                graded.append((seg, reason, bodies))
    return graded


def verdict(cmd, root, cwd=None, secrets=None, published=None):
    """(segment, reason, [(what, how, where)], secrets) if the command must be refused, else None.

    `secrets` / `published` are injectable for the test — see `load_secrets`. The loaded set is
    handed back so `main` can mask the command it echoes with exactly the values that condemned
    it, without loading them a second time.
    """
    if BYPASS.search(cmd):
        return None
    cwd = cwd or root

    graded = grade(cmd)
    if not graded:
        return None

    if secrets is None:
        secrets = load_secrets(root)
    if published is None:
        published = default_published(root)

    for seg, reason, bodies in graded:
        hits = []
        for rel, how in named_private_paths(seg, root, cwd):
            hits.append(("%s, which is gitignored because it holds private values" % rel,
                         how, "the command line"))
        for where, text in payload_parts(seg, bodies, cwd):
            for what, how in findings(text, secrets, published):
                hits.append((what, how, where))
        if hits:
            return (seg.strip(), reason, hits, secrets)
    return None


def repo_root(cwd):
    try:
        out = subprocess.run(["git", "-C", cwd, "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, timeout=5)
        if out.returncode == 0 and out.stdout.strip():
            return out.stdout.strip()
    except Exception:
        pass
    return os.path.abspath(cwd)


def main():
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw)
    except Exception:
        return 0
    # Every field is type-checked rather than duck-typed. The fail-open catch below would allow
    # these anyway, but it would also write `outbound-guard: internal error` into the transcript,
    # which reads as the guard being broken. Measured 2026-08-23 against the real binary: a JSON
    # array, a string `tool_input` and a null `command` each produced that line.
    if not isinstance(payload, dict) or payload.get("tool_name") != "Bash":
        return 0
    ti = payload.get("tool_input")
    cmd = ti.get("command") if isinstance(ti, dict) else None
    if not isinstance(cmd, str) or not cmd.strip():
        return 0

    cwd = payload.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        cwd = os.getcwd()

    # Nothing outbound → answer before paying for a repo root, a file read or a `git grep`.
    if BYPASS.search(cmd) or not grade(cmd):
        return 0
    v = verdict(cmd, repo_root(cwd), cwd=cwd)
    if not v:
        return 0
    seg, reason, hits, secrets = v

    lines = ["BLOCKED: this command would publish a private value out of this repo's "
             "gitignored files.",
             "  the command: %s" % (redact(seg, secrets)[:140]),
             "  why graded:  %s" % reason,
             "  what matched:"]
    for what, how, where in hits[:6]:
        lines.append("    - %s  [%s, in %s]" % (what, how, where))
    lines.append("  (the value itself is not printed — putting it in this transcript is the same "
                 "leak by a shorter route)")
    sys.stderr.write("\n".join(lines) + "\n\n" + (
        "github.com/GLinnik21/plx-native is PUBLIC. On 2026-08-14 four PR bodies (#28-31) carried a\n"
        "third party's Plex server address, port, machineIdentifier and handle. All four were\n"
        "redacted, but GitHub keeps PR-body edit history, so those values are permanently public —\n"
        "and they were the FRIEND's, not this project's to publish.\n"
        "\n"
        "Use a stand-in. docs/shared-servers.md carries the table this repo actually uses:\n"
        "  handle `friend` · machine `nas-home` · library `Film Club` · port `31234`\n"
        "  LAN address `10.9.9.7` · public address `203.0.113.9` (RFC 5737 TEST-NET-3)\n"
        "  a machine id: `aaaabbbb…` runs, never a real 40-hex one\n"
        "and tests/manifest.local.json.example uses `<pms-host>`, `<tv-host>`, `<ratingKey>`.\n"
        "\n"
        "If the reader genuinely needs the real value, name the gitignored FILE it comes from\n"
        "(.tv-host, src/config.local.h, tests/manifest.local.json) and let them read their own copy.\n"))
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:      # a bug in the guard must not wedge every Bash call
        sys.stderr.write("outbound-guard: internal error, allowing (%s)\n" % e)
        sys.exit(0)
