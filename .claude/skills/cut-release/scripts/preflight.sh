#!/usr/bin/env bash
# preflight.sh — everything about a release that a command can decide.
#
# Each check here exists because it failed in a real release of this project. The point is not
# ceremony: it is that all four defects found in v0.2.1 were invisible from inside the step that
# introduced them, and every one of them is one command away from being obvious.
#
#   preflight.sh                 check the LOCAL tree and pkg/ before publishing
#   preflight.sh --published vX.Y.Z   check what the public can actually download
#
# Exit status is 0 only if every REQUIRED check passed. Advisory checks print and never fail the
# run — they are things a human should look at, not things a machine can settle.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

PASS=0; FAIL=0; NOTE=0
ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
note() { printf '  \033[33mnote\033[0m  %s\n' "$1"; NOTE=$((NOTE+1)); }
head_() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

# A build-machine path in a shipped file. Anchored on a non-path character so ordinary URLs do not
# trip it — the app talks to plex.tv's /api/v2/home/users, which is not a build path.
HOSTPATH='(^|[^A-Za-z0-9/_.-])/(Users|home)/[a-z]'
# The NDK's own location is unavoidable: --cross-prefix must be absolute (the wrapper gcc dies when
# invoked through PATH), so it rides in FFmpeg's recorded configure string. It is identical on every
# CI runner, which is precisely why releases must be built by CI.
ALLOWED='webos-ndk|/home/runner/'

# Extract each PATH individually rather than filtering whole lines. FFmpeg records its entire
# configure invocation as one long string, so the offending `--prefix=/Users/<me>/...` sits on the
# same "line" as the unavoidable `--cross-prefix=/Users/<me>/webos-ndk/...`. A per-line allowlist
# therefore sees "webos-ndk" and passes the whole blob — which is exactly the false pass this
# check existed to prevent, caught only by testing it against a release known to be dirty.
#
# The match KEEPS its leading boundary character and strips it afterwards, because dropping the
# anchor to tokenise reintroduces the opposite error: plex.tv's own `/api/v2/home/users` looks like
# `/home/users` once you extract without context. Both failure modes were observed; this shape is
# the one that gets both right.
scan_paths() { # $1 = file
  local hits
  hits=$(strings -a "$1" 2>/dev/null \
    | grep -aoE '(^|[^A-Za-z0-9/_.-])/(Users|home)/[A-Za-z0-9_./+-]+' \
    | sed -E 's#^[^/]##' \
    | grep -avE "$ALLOWED" | sort -u | head -3)
  [ -z "$hits" ] && return 0
  printf '%s\n' "$hits" | sed 's/^/          /'
  return 1
}

# ---------------------------------------------------------------- published mode
if [ "${1:-}" = "--published" ]; then
  TAG="${2:?usage: preflight.sh --published vX.Y.Z}"
  VER="${TAG#v}"
  WORK=$(mktemp -d); trap 'rm -rf "$WORK"' EXIT
  head_ "what the public downloads — $TAG"

  gh release download "$TAG" -D "$WORK" --clobber >/dev/null 2>&1 \
    || { bad "release $TAG has downloadable assets"; exit 1; }
  IPK="$WORK/com.beb.plxnative_${VER}_arm.ipk"
  [ -f "$IPK" ] && ok "the .ipk is published" || { bad "no .ipk asset named for $VER"; exit 1; }

  SHA=$(shasum -a 256 "$IPK" | cut -d' ' -f1)

  # The checksum file has to verify where a USER stands — beside the .ipk, not in pkg/.
  ( cd "$WORK" && shasum -a 256 -c ipk.sha256 >/dev/null 2>&1 ) \
    && ok "shasum -c works where the two assets land side by side" \
    || bad "ipk.sha256 does not verify beside the .ipk (a pkg/ prefix in it breaks this for everyone)"

  # Four copies of the hash must agree: the artifact, the checksum file, the manifest, the note.
  grep -q "$SHA" "$WORK/ipk.sha256" 2>/dev/null && ok "checksum file matches the artifact" || bad "checksum file disagrees with the artifact"
  python3 - "$WORK/com.beb.plxnative.manifest.json" "$SHA" "$IPK" <<'PY' && ok "manifest hash and size match the artifact" || bad "manifest disagrees with the artifact"
import json, sys, os
m = json.load(open(sys.argv[1]))
sys.exit(0 if m["ipkHash"]["sha256"] == sys.argv[2] and m["ipkSize"] == os.path.getsize(sys.argv[3]) else 1)
PY
  gh release view "$TAG" --json body --jq .body 2>/dev/null | grep -q "$SHA" \
    && ok "the release note quotes this hash" || bad "the note's hash is not the published artifact's"

  # Who built it. A person's name here means the gates did not run.
  UP=$(gh api "repos/GLinnik21/plx-native/releases/tags/$TAG" --jq '[.assets[].uploader.login] | unique | join(",")' 2>/dev/null)
  [ "$UP" = "github-actions[bot]" ] && ok "assets uploaded by CI" \
    || bad "assets uploaded by '$UP' — hand-published, so the build/verify gates were skipped"

  # Every payload file, not just the binary. check-elf.sh only ever looked at pkg/plxnative, which
  # is how the maintainer's working directory shipped inside three FFmpeg libraries.
  ( cd "$WORK" && python3 -c "
d=open('$(basename "$IPK")','rb').read(); i=d.find(b'data.tar.gz')
open('data.tar.gz','wb').write(d[i+60:i+60+int(d[i+48:i+58])])" && tar xzf data.tar.gz ) >/dev/null 2>&1
  DIRTY=0
  for f in "$WORK"/usr/palm/applications/com.beb.plxnative/*; do
    [ -f "$f" ] || continue
    scan_paths "$f" || { bad "$(basename "$f") carries a build-machine path"; DIRTY=1; }
  done
  [ "$DIRTY" = 0 ] && ok "no payload file carries a build-machine path"

  printf '\n%d passed, %d failed, %d to look at\n' "$PASS" "$FAIL" "$NOTE"
  [ "$FAIL" -eq 0 ] || exit 1
  exit 0
fi

# ---------------------------------------------------------------- local mode
head_ "version agreement"
python3 - <<'PY'
import json, re, sys, pathlib
app = json.loads(pathlib.Path("pkg/appinfo.json").read_text())["version"]
ctl = dict(l.split(": ", 1) for l in pathlib.Path("ipkroot/ctl/control").read_text().splitlines() if ": " in l)
crg = re.search(r'^version = "([^"]+)"', pathlib.Path("rust-modules/Cargo.toml").read_text(), re.M).group(1)
bad = []
if ctl.get("Version") != app: bad.append(f"control={ctl.get('Version')}")
if crg != app:                bad.append(f"Cargo.toml={crg} (the diagnostics panel prints this one)")
print(("OK " if not bad else "BAD ") + f"appinfo={app} " + " ".join(bad))
sys.exit(1 if bad else 0)
PY
[ $? -eq 0 ] && ok "appinfo, control and Cargo.toml agree" || bad "the version sources disagree"

VER=$(python3 -c "import json;print(json.load(open('pkg/appinfo.json'))['version'])")
IPK="pkg/com.beb.plxnative_${VER}_arm.ipk"

head_ "release note"
NOTE_MD="docs/release-notes/v${VER}.md"
if [ -f "$NOTE_MD" ]; then
  ok "docs/release-notes/v${VER}.md exists (CI publishes the body from it)"
  # Every "webOS N" claimed must be a firmware this repo actually knows about.
  UNKNOWN=""
  for n in $(grep -oE 'webOS [0-9]+(\.[0-9]+)*' "$NOTE_MD" | awk '{print $2}' | sort -u); do
    grep -qr -- "$n" tools/fwcompat.py docs/webos5-port.md 2>/dev/null || UNKNOWN="$UNKNOWN $n"
  done
  [ -z "$UNKNOWN" ] && ok "every webOS version named is one we have evidence for" \
    || bad "note names webOS versions with no evidence in the repo:$UNKNOWN"
  grep -qE '\b[0-9.]+ *(MB|KB|GB)\b' "$NOTE_MD" \
    && note "the note states a size — confirm it came from the manifest's ipkSize, with the unit named" \
    || ok "no hand-typed size in the note"
else
  bad "no docs/release-notes/v${VER}.md — write it from docs/release-notes/TEMPLATE.md"
fi

head_ "package"
if [ -f "$IPK" ]; then
  ok "$IPK is built"
  python3 ci/check-package.py >/dev/null 2>&1 && ok "ci/check-package.py passes" || bad "ci/check-package.py fails — run it for the detail"
  ( cd pkg && shasum -a 256 -c ipk.sha256 >/dev/null 2>&1 ) \
    && ok "ipk.sha256 verifies from pkg/ (bare filename, as a user gets it)" \
    || bad "ipk.sha256 does not verify — check it carries the bare filename, not a pkg/ path"
else
  bad "$IPK not built — run: make RELEASE=1 ipk"
fi

head_ "build configuration"
DIRTY=0
for f in pkg/plxnative pkg/*.so.*; do
  [ -f "$f" ] || continue
  scan_paths "$f" || { bad "$(basename "$f") carries a build-machine path"; DIRTY=1; }
done
[ "$DIRTY" = 0 ] && ok "no local build path in the staged binaries (beyond the NDK's own)"
# A dev build has the trigger surface compiled in; strings is the cheapest way to tell.
if [ -f pkg/plxnative ] && strings -a pkg/plxnative | grep -q "plxnative-autoplay"; then
  bad "pkg/plxnative is a DEV build (dev triggers compiled in) — rebuild with RELEASE=1 on EVERY invocation"
else
  [ -f pkg/plxnative ] && ok "pkg/plxnative looks like a RELEASE build"
fi

head_ "third-party notices"
python3 - <<'PY'
import pathlib, re, sys, glob, os
notices = pathlib.Path("THIRD-PARTY-NOTICES.md").read_text()
shipped = {os.path.basename(p) for p in glob.glob("pkg/*.so.*")}
named = set(re.findall(r'`(lib[a-z]+-plx\.so\.\d+)`', notices))
missing, extra = shipped - named, named - shipped
print("OK" if not (missing or extra) else f"BAD missing={sorted(missing)} named-but-not-shipped={sorted(extra)}")
sys.exit(1 if (missing or extra) else 0)
PY
[ $? -eq 0 ] && ok "THIRD-PARTY-NOTICES names exactly the libraries shipped" \
  || bad "THIRD-PARTY-NOTICES disagrees with what ships (RELEASE=1 drops swscale)"

head_ "listing and submission"
note "if compatibility changed, update the apps-repo PR body AND packages/com.beb.plxnative.yml"
note "releases are published by CI: gh workflow run release.yml -f version=$VER — a bare tag push builds nothing"

printf '\n%d passed, %d failed, %d to look at\n' "$PASS" "$FAIL" "$NOTE"
[ "$FAIL" -eq 0 ] || exit 1
