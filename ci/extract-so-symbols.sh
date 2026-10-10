#!/bin/sh
# Separate the symbols of one BUNDLED shared object into a `.debug` file Sentry can match.
#
#   TOOLPREFIX=<cross prefix> ci/extract-so-symbols.sh SHIPPED UNSTRIPPED OUT.debug
#
# SHIPPED is the stripped library the package carries; UNSTRIPPED is the link output it was made
# from (`vendor/ffmpeg-prefix/lib/unstripped/*`, `vendor/libass-prefix/lib/*`). The shipped copy has
# no `.symtab`, so a frame inside it symbolicates to nothing; the `.debug` made here keeps `.symtab`
# and whatever DWARF the build left, and `.text` becomes NOBITS, exactly as `make symbols` treats
# pkg/plxnative.
#
# FAILS CLOSED, because `sentry-cli debug-files check` passes a stripped object with no symbols and
# an upload of a file that matches nothing is silent until a real crash fails to symbolicate:
#   * the shipped library must carry a GNU build id (`-Wl,--build-id=sha1` on its link), and the
#     `.debug` must carry the SAME one. Without it the id is a hash of `.text`, which a `.text`-less
#     `.debug` cannot reproduce;
#   * the `.debug` must hold a `.symtab` with function symbols.
set -eu

[ "$#" -eq 3 ] || { echo "usage: $0 SHIPPED UNSTRIPPED OUT.debug" >&2; exit 2; }
shipped=$1 unstripped=$2 out=$3
pre=${TOOLPREFIX-}
name=$(basename "$shipped")

fail() { echo "symbols-libs: $name: $*" >&2; exit 1; }

[ -f "$shipped" ] || fail "shipped library $shipped does not exist"
[ -f "$unstripped" ] || fail "no unstripped copy at $unstripped (a prefix built before the unstripped copies were kept: remove vendor/ffmpeg-prefix and rebuild)"

tmp="$out.tmp.$$"
trap 'rm -f "$tmp"' EXIT
mkdir -p "$(dirname "$out")"

"${pre}objcopy" --only-keep-debug "$unstripped" "$tmp"

idof() { "${pre}readelf" -n "$1" 2>/dev/null | sed -n 's/.*Build ID: //p'; }
bin=$(idof "$shipped")
dbg=$(idof "$tmp")
[ -n "$bin" ] || fail "the shipped library has NO build id - is -Wl,--build-id=sha1 still on its link?"
[ "$bin" = "$dbg" ] || fail "build id mismatch: shipped $bin, debug ${dbg:-none} (the unstripped copy is not the one the shipped library was made from)"

"${pre}readelf" -S "$tmp" | grep -q '[.]symtab' || fail "the .debug has no .symtab, so it would name no function"
"${pre}readelf" -sW "$tmp" | grep -q ' FUNC ' || fail "the .debug's .symtab holds no function symbols"

mv "$tmp" "$out"
echo "symbols-libs: $out  build-id $bin  ($(du -h "$out" | cut -f1))"
