#!/usr/bin/env bash
# verify-published.sh — check what the PUBLIC can actually download, after a release exists.
#
#   ci/verify-published.sh vX.Y.Z
#
# Everything that can be asserted BEFORE publishing lives in ci/check-package.py, which CI runs on
# every build. This script exists for the half that cannot: it needs a published release to look
# at. It downloads the real assets and re-derives every claim from them — the hash in four places,
# whether `shasum -c` works where a user stands, who uploaded the files, and whether any payload
# file carries a build machine's directory layout.
#
# It is a CI job, not a checklist item, for the reason the whole exercise exists: v0.2.1 was
# published by hand, which skipped every gate, and a checklist would have been skipped with them.
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
TAG="${1:?usage: ci/verify-published.sh vX.Y.Z}"
if true; then
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
fi
