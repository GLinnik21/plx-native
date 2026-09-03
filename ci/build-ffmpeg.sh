#!/bin/sh
# Cross-compile the FFmpeg the app SHIPS, for 32-bit ARM, with the webOS NDK.
#
# WHY WE BUNDLE. The app used to demux with the television's own FFmpeg, which meant reading its
# structs at byte offsets we could not check: LG ships no headers, the binaries are stripped, and
# the major moves with the firmware (55 -> 57 -> 58 -> 59 -> 60 across webOS 2 to 11). That was
# survivable while the offsets could be re-derived against upstream headers at the matching
# version — but only for the LAYOUT half of the problem. The other half is not checkable at all:
# demuxers, bitstream filters, encoders and muxers live in a REGISTRY, as data, not as exported
# symbols. No symbol table and no firmware database can answer "does this set's libavcodec have
# h264_mp4toannexb", and the author has no hardware past webOS 4.5 to ask. Bundling turns both
# halves into compile-time facts.
#
# It also collapses the maintenance: one FFmpeg on every television, so adding a webOS release
# costs nothing and the demuxer stops being a variable in every bug report.
#
# THIS IS ALSO WEBOSBREW'S PUBLISHED GUIDANCE, which is worth knowing before anyone re-opens the
# decision. https://www.webosbrew.org/develop/caniuse/?q=ffmpeg carries a warning rather than a
# compatibility table: "Don't use system FFmpeg libraries! They will cause linkage issues and
# doesn't come with usable video codecs either." Both halves are the reasoning above — the SONAME
# drift, and the component list that cannot be inspected from outside.
#
# LICENCE. Built SHARED, deliberately. FFmpeg here is LGPL-2.1 (no --enable-gpl, no --enable-
# nonfree), and shipping it as separate .so files keeps us in §6(b) — the user can replace the
# library — exactly as dynamic linking against the TV's copy did. Static linking would pull in
# §6(a)'s relinkable-object obligation for no benefit. The source tarball this builds from is
# published beside each release; see THIRD-PARTY-NOTICES.md.
#
# BUILD SUFFIX. The libraries are named libavformat-plx.so.60 and so on. webOS 11.2.0 ships
# FFmpeg 6 too, so without the suffix our file names and SONAMEs would collide with the
# television's own — and "which libavformat did we actually get" is precisely the question that
# cannot be answered remotely. With it, the answer is structural.
#
# NO RPATH, DELIBERATELY. The obvious way to make our libavformat find our libavcodec is
# -Wl,-rpath,$ORIGIN, and it does not survive: FFmpeg's configure evals its flags, so `$$ORIGIN`
# becomes the shell's PID and `\$$ORIGIN` becomes nothing. Both produce a library that silently
# loads a DIFFERENT libavcodec. Instead ff.rs dlopens the four in dependency order with
# RTLD_GLOBAL — libavutil, then libavcodec, then libavformat — so each is already in the global
# scope by SONAME when the next one names it. That is ordinary loader behaviour, it needs no build
# flag to survive quoting, and unlike an rpath it can be asserted from inside the app.
#
# HOST=1 BUILDS THE SAME FFMPEG FOR THIS MAC INSTEAD, and it is the same script on purpose.
#
# The desktop simulator (`make sim`) runs the real app core on macOS, but `ff.rs` dlopens the
# bundled libraries by absolute path out of the app directory — where they are 32-bit ARM ELF. So
# the ENTIRE streaming half of the app (the AVIO transports, the HLS demux, the AU queues and
# therefore the whole adaptive controller) was device-only, and the one instrument that can run
# them without a television could not reach them: `ff: FFmpeg unavailable — the app runs, playback
# will refuse`, measured 2026-08-28.
#
# **One script, and specifically ONE COMPONENT LIST.** Everything below `set --` is shared: same
# version, same demuxers, parsers, bitstream filters and decoders, same `--disable-network` with
# `file` as the only protocol, same `-plx` suffix, same LGPL flags. A second script would drift,
# and the drift would be invisible — the host would demux something the television cannot, or
# stop demuxing something it can, and the simulator would be answering about a different FFmpeg
# while looking identical. The cross block is the only difference, and it is three lines.
#
# The output is `vendor/ffmpeg-prefix-host` and `libavformat-plx.63.dylib` — Mach-O naming, so
# nothing here can be confused with the shipped ELF even by basename, and `make ipk`'s
# `lib*-plx.so.*` glob cannot pick it up.
set -eu

VERSION=9.0
SHA256=7f607a00dd0d28a729d5a4811205812eef01cf6ef6155025febb6f36a9062d52
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}
SYSROOT="$NDK/arm-webos-linux-gnueabi/sysroot"
HOST=${HOST:-}
# WHERE EACH HALF LIVES, and why they are no longer in the same place.
#
# The PREFIX — the 3.8 MB of installed libraries and headers — stays inside the checkout, exactly
# where it was. `make`'s staleness sentinel is a header inside it, the configuration stamp deletes
# that header to force a rebuild on a RELEASE flip, CI caches the directory by that path and
# `ci/ffabi-assert.c` compiles against its headers. Nothing about the prefix moved.
#
# The WORK tree did. It is the extracted upstream source plus every object file — 122 MB for the
# cross build and 123 MB for the host one, against that 3.8 MB of output — and it is pure means:
# once the prefix exists, nothing ever reads it again. A fleet of parallel worktrees
# (`.agents/skills/fleet-plan`) got ONE OF EACH PER LANE, byte-identical, each compiled from
# scratch. Measured 2026-09-03: twelve lanes held 2.6 GB of them on a volume with 3.2 GiB free,
# and every new lane paid the two-minute compile again to produce bytes that already existed
# eleven times over.
#
# So it lives under $PLX_BUILD_CACHE (default ~/.cache/plxnative), machine-wide, KEYED BY THE
# CONFIGURE FLAGS. The key is the half that makes sharing safe, and it is precisely what the
# manual `ln -s` workaround in fleet-plan could not express: RELEASE=1 drops swscale and the
# mpeg1/mpegts pair, so a dev lane and a release lane MUST NOT share one build tree. Different
# flags hash to different keys and the two never meet — where the symlink recipe silently handed
# one configuration the other's libraries. The cross and host builds key apart for the same
# reason, as do two different NDKs.
#
# Set PLX_BUILD_CACHE= (empty) to keep the build tree in the checkout as it was; CI does not need
# to, because a runner is ephemeral and caches the prefix rather than the objects.
#
# The TARBALL is deliberately NOT in the cache. It is the LGPL corresponding source that
# `release.yml` publishes beside each release (`ls vendor/ffmpeg-build/ffmpeg-*.tar.xz`), so it
# belongs with the checkout, it is 10 MB, and both the cross and host builds now share the one
# copy instead of downloading it twice.
if [ -n "$HOST" ]; then
  PREFIX="$ROOT/vendor/ffmpeg-prefix-host"
else
  PREFIX="$ROOT/vendor/ffmpeg-prefix"
fi
TARDIR="$ROOT/vendor/ffmpeg-build"
# `--prefix` is the LITERAL /plx, not $PREFIX, and the install is redirected with DESTDIR below.
#
# WHY: FFmpeg records its entire configure INVOCATION inside libavutil (`avutil_configuration()`),
# so every absolute path on that command line ends up in the shipped `.so`. A real `--prefix` put
# the maintainer's working directory in all three libraries, and with it the `.ipk` sha — which
# silently made "builds are reproducible" false in the v0.2.0 and v0.2.1 release notes, on the one
# number a user has to check an unsigned download. `/plx` is a stand-in that exists nowhere.
#
# It does NOT make a local build reproducible, and nothing here can: `--cross-prefix` must be
# absolute (see below — the NDK gcc is a wrapper that dies when invoked through PATH), so the
# configure string still carries wherever the NDK lives. Reproducibility is therefore a property
# of the CI BUILD, whose path is fixed for every GitHub runner — which is the real reason releases
# must be published by the workflow and never by hand. Do not restore a "rebuild and compare"
# claim to the notes without checking `strings` on all three libraries first.
#
# Do NOT "fix" this with -ffile-prefix-map: that flag is itself part of the configure line, so
# passing absolute paths to it ADDS two more leaks than it removes. Tried, measured, reverted.

# The NDK's gcc is a WRAPPER that resolves its own path at startup. Invoked through PATH it dies
# with "toolchain-wrapper.c: readlink: No such file or directory" and configure reports the
# misleading "C compiler test failed", so --cross-prefix must be absolute.
CROSS="$NDK/bin/arm-webos-linux-gnueabi-"

if [ -z "$HOST" ]; then
  [ -x "${CROSS}gcc" ] || { echo "ffmpeg: no NDK at $NDK — run 'make setup-env'" >&2; exit 1; }
fi


# Wipe the prefix whenever the version changes. `make install` only ADDS files, so a version bump
# leaves the previous major's libraries sitting beside the new ones — and since the Makefile stages
# by SONAME glob, the package would quietly ship whichever the shell listed first. Found the hard
# way going 6.1 -> 8.1: the prefix held libavformat 60 AND 62.
STAMP="$PREFIX/.plx-version"
if [ ! -f "$STAMP" ] || [ "$(cat "$STAMP")" != "$VERSION" ]; then
  rm -rf "$PREFIX"
  mkdir -p "$PREFIX"
  printf '%s' "$VERSION" > "$STAMP"
fi

# Components: exactly what the app uses, and nothing else — the size is almost entirely a
# function of this list. Each line maps to real code:
#   demuxers   matroska/mov/mpegts are the containers PMS direct-plays or transcodes to;
#              h264/hevc are the raw Annex-B paths (/tmp/sample.h264 and the dev triggers).
#   parsers    needed by avformat_find_stream_info to fill AVCodecParameters.
#   bsf        AVCC -> Annex-B for mp4-family video, which the Starfish pipeline requires.
#   decoders   SUBTITLES ONLY. Video and audio are decoded by the TV's hardware via Starfish;
#              FFmpeg here never touches a video frame.
#   encoder/   mpeg1video + mpegts are the DEV capture stream only, dropped by RELEASE=1 along
#   muxer      with swscale, which nothing else uses.
#
# `--disable-autodetect` is NOT redundant beside `--disable-everything`, which governs the
# component registry and says nothing about EXTERNAL libraries. Without it configure goes looking
# through pkg-config and folds in whatever the machine happens to have — on a Mac with Homebrew
# that is real: the host dylibs pick up dependencies of packages this project never asked for. In
# a shared, keyed cache that is worse than untidy, because the key cannot see it: uninstall the
# package later and a fresh checkout still selects the same work tree, decides the objects are
# current, and stages dylibs referencing a library that is no longer on the disk. It also makes
# the two builds agree — the cross build had no such packages to find, so the host one was the
# only half whose output depended on the machine it was built on.
if [ -n "$HOST" ]; then
  # No --arch/--cpu/--target-os: configure detects this Mac, which is the point.
  set -- --prefix=/plx
else
  set -- --prefix=/plx \
    --enable-cross-compile --cross-prefix="$CROSS" --host-cc=cc \
    --arch=arm --cpu=cortex-a9 --target-os=linux --sysroot="$SYSROOT"
fi
set -- "$@" \
  --build-suffix=-plx \
  --enable-shared --disable-static --enable-pic \
  --extra-cflags='-Dstatic_assert=_Static_assert' \
  --disable-programs --disable-doc --disable-avdevice --disable-avfilter \
  --disable-network --disable-swresample \
  --disable-debug --enable-small \
  --disable-everything \
  --disable-autodetect \
  --enable-demuxer=matroska,mov,mpegts,h264,hevc \
  --enable-parser=h264,hevc,aac,ac3,dvdsub,dvbsub \
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,extract_extradata \
  --enable-decoder=pgssub,dvdsub,dvbsub,ass,srt,subrip,text,mov_text,webvtt \
  --enable-protocol=file

if [ "${RELEASE:-}" = "1" ]; then
  set -- "$@" --disable-swscale
else
  set -- "$@" --enable-encoder=mpeg1video --enable-muxer=mpegts
fi

# ---- the shared, flag-keyed build tree ---------------------------------------------------------
# Everything above decided WHAT to build; the flags are now settled, so they can name WHERE. The
# key covers every input that changes an object file: the version, which toolchain (a cross build
# and a host build share this script and must not share a tree), the NDK and sysroot paths that
# end up on the configure line, and the flag list itself — which is what keeps a RELEASE tree and
# a dev tree apart.
if [ -n "$HOST" ]; then ARCHTAG=host; else ARCHTAG=arm; fi
CACHE_ROOT=${PLX_BUILD_CACHE-$HOME/.cache/plxnative}
# ABSOLUTE, because this script `cd`s into $SRC before it uses $WORK again. A relative root such
# as `PLX_BUILD_CACHE=.plx-cache` would leave $WORK relative too, and after the `cd` every later
# reference — the configure/build/install logs, $WORK/destdir — would resolve UNDER THE FFMPEG
# SOURCE TREE instead, where the first redirection fails and takes the build with it.
if [ -n "$CACHE_ROOT" ]; then
  mkdir -p "$CACHE_ROOT"
  CACHE_ROOT=$(cd "$CACHE_ROOT" && pwd)
fi
FLAGS_ALL=$*
if [ -n "$CACHE_ROOT" ]; then
  # THE COMPILER IS AN INPUT, and naming only the PATH to it is not naming it. `WEBOS_SDK` is a
  # fixed location — `make setup-env` reinstalls the NDK in place — so an upgraded toolchain
  # produces an unchanged key, and FFmpeg's makefiles have no dependency on the compiler's own
  # bytes: a fresh checkout would link objects built by the toolchain that was there last month
  # and never rebuild them. The version banner catches an upgrade, the size-and-mtime stamp
  # catches a same-version reinstall, and both cost one exec.
  # `-dumpmachine` FIRST, because it is the only one of the three that names what the compiler
  # EMITS. A Mac running the host build under Rosetta reports a byte-identical `cc --version`
  # while targeting x86_64 instead of arm64, so a version banner alone would hand one architecture
  # the other's objects. And `ls -ln` on the cross gcc stats a SYMLINK — every
  # `arm-webos-linux-gnueabi-*` name in the NDK points at one `toolchain-wrapper` — whose metadata
  # does not move when the wrapper behind it is replaced; `-L` follows to the file that actually
  # changed.
  if [ -n "$HOST" ]; then
    TOOLCHAIN="$(cc -dumpmachine 2>/dev/null)|$(cc --version 2>/dev/null | head -1)"
  else
    # `${CROSS}gcc` is a symlink to `toolchain-wrapper`, and so is every other tool name in this
    # NDK — so `-L` follows to the WRAPPER, one file that all of them share and that a sysroot or
    # binutils update does not touch. The things that actually change are behind it: the real
    # compiler (`…-gcc.br_real`), the linker, and the sysroot's libc. This NDK ships no version
    # file, so their size-and-mtime is the fingerprint available, and `ld --version` catches a
    # binutils bump that keeps the GCC banner. Missing names drop out silently, which is right:
    # another toolchain layout contributes what it has rather than failing here.
    TOOLCHAIN="$("${CROSS}gcc" -dumpmachine 2>/dev/null)|$("${CROSS}gcc" --version 2>/dev/null | head -1)|$("${CROSS}ld" --version 2>/dev/null | head -1)|$(ls -lnL "${CROSS}gcc.br_real" "${CROSS}ld" "$SYSROOT/lib/libc.so.6" 2>/dev/null | awk '{print $5,$6,$7,$8}' | tr '\n' ' ')"
  fi
  # $SHA256 IS IN THE KEY, and leaving it out was a real hole: a re-pin that keeps the same
  # VERSION — upstream re-rolling a tarball, or this file being corrected — would select the same
  # $WORK, find $SRC already extracted, skip extraction entirely and install objects built from
  # the OLD source, while `release.yml` attached the new tarball as the corresponding source. The
  # two would disagree about what the shipped libraries are, which is the one claim the LGPL
  # position rests on.
  # INHERITED BUILD VARIABLES ARE INPUTS TOO. FFmpeg's configure honours CFLAGS, CPPFLAGS and
  # LDFLAGS from the environment, and neither the flag list above nor `.plx-flags` records them —
  # so the first checkout to build with something in CFLAGS would bake it into objects that every
  # later, clean checkout then reuses. One shell's codegen or SDK override, silently inherited by
  # every lane on the machine, in libraries that ship. They are in the key, which means such a
  # build gets its OWN tree rather than contaminating the shared one.
  INHERITED="${CFLAGS-}|${CPPFLAGS-}|${LDFLAGS-}|${CXXFLAGS-}|${PKG_CONFIG_PATH-}"
  KEY=$(printf '%s|%s|%s|%s|%s|%s|%s|%s' "$VERSION" "$SHA256" "$ARCHTAG" "$CROSS" "$SYSROOT" "$TOOLCHAIN" "$INHERITED" "$FLAGS_ALL" \
        | shasum -a 256 | cut -c1-16)
  WORK="$CACHE_ROOT/ffmpeg/$ARCHTAG-$VERSION-$KEY"
  # One tree, N checkouts, so two of them can arrive at once — a fleet launches its lanes
  # together, which is exactly the case that would have them configure over each other's
  # half-written Makefile. mkdir is the atomic primitive every POSIX sh has (macOS ships no
  # flock(1)), the pid lets a lock whose owner died be reclaimed rather than blocking the machine
  # forever, and the wait is bounded well past the ~2 minute cold build.
  LOCK="$WORK.lock"
  mkdir -p "$(dirname "$WORK")"
  waited=0
  while ! mkdir "$LOCK" 2>/dev/null; do
    # RECLAIMING A DEAD OWNER'S LOCK IS ITSELF A RACE, and `rm -rf` loses it. Two waiters that
    # both read the same dead pid would both delete — and the second `rm` lands AFTER the first
    # has already reacquired, so it deletes a LIVE owner's lock and takes it too. Two builds in
    # the shared tree, which is the one thing the lock exists to prevent. `mv` is the atomic
    # compare-and-claim every POSIX filesystem gives us: exactly one waiter can rename the stale
    # directory away, and the loser's rename fails because the source no longer exists, so it
    # simply goes back to waiting on whoever won.
    #
    # The age test closes the other end. `mkdir` and the `echo $$` that follows it are two
    # operations, so there is a window in which the lock exists with NO pid file — indistinguishable
    # from a dead owner by content alone, and a young lock in that window is the LIKELIEST one to
    # find, since it belongs to a build that just started. A lock is therefore only a reclaim
    # candidate once it is over a minute old, which no healthy owner's window comes close to.
    # A DEAD OWNER PID IS NOT AN EMPTY TREE. SIGKILL the script and its foreground `make` child
    # survives — still compiling, still writing into $WORK — while the pid in the lock is gone and
    # a cold build has long since aged the lock past a minute. The next checkout would reclaim on
    # the spot and start writing the same tree beside it. So the lock records the owner's PROCESS
    # GROUP as well, which its children inherit: while anything from that group is alive there is
    # still a writer, whatever happened to the shell itself.
    # `kill -0 0` DOES NOT MEAN "pid 0 is dead" — POSIX defines pid 0 as the CALLER's process
    # group, so it succeeds, and the owner reads as alive forever. That is the value this code
    # produces whenever the pid file is missing: the window between `mkdir` and the write, or a
    # write that failed on a full disk. The lock would then never be reclaimed and every build on
    # the machine would wait the full thirty minutes and give up. Anything not a positive integer
    # is treated as absent.
    lock_pid=$(cat "$LOCK/pid" 2>/dev/null || echo 0)
    lock_pgid=$(cat "$LOCK/pgid" 2>/dev/null || echo 0)
    case "$lock_pid"  in ''|*[!0-9]*) lock_pid=0  ;; esac
    case "$lock_pgid" in ''|*[!0-9]*) lock_pgid=0 ;; esac
    owner_alive=no
    if [ "$lock_pid" -gt 0 ] && kill -0 "$lock_pid" 2>/dev/null; then owner_alive=yes; fi
    if [ "$lock_pgid" -gt 0 ] && pgrep -g "$lock_pgid" >/dev/null 2>&1; then owner_alive=yes; fi
    if [ "$owner_alive" = no ] \
       && [ -n "$(find "$LOCK" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      if mv "$LOCK" "$LOCK.stale.$$" 2>/dev/null; then
        echo "ffmpeg: reclaiming a lock whose owner and its children are gone"
        rm -rf "$LOCK.stale.$$"
      fi
      continue
    fi
    if [ "$waited" -eq 0 ]; then
      echo "ffmpeg: another checkout is building this configuration — waiting"
    fi
    sleep 2
    waited=$((waited + 2))
    if [ "$waited" -ge 1800 ]; then
      echo "ffmpeg: gave up after 30 min waiting for $LOCK — remove it if no build is running" >&2
      exit 1
    fi
  done
  echo $$ > "$LOCK/pid"
  ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' > "$LOCK/pgid" || true
  # EXIT releases the lock. INT/TERM must ALSO END THE SCRIPT, and one trap for all three does
  # not: a signal handler that returns simply resumes the interrupted command, so a ^C would
  # release the lock and then carry on configuring, building and installing — while the checkout
  # that was waiting takes the lock and starts writing the same tree. Two builds in one directory,
  # arrived at by pressing ^C once. Clearing the trap and re-raising is the idiom: the shell dies
  # of the signal it was sent, so `make` sees a real interrupt rather than a spurious success.
  # RELEASE ONLY WHAT WE STILL HOLD, and re-raise with the EXIT trap already cleared. A signal
  # handler that removes the lock and re-raises leaves `/bin/sh` to run the EXIT trap on the way
  # out — a SECOND removal, and by then a waiter may have taken the lock, so the dying process
  # deletes the new owner's mutex and two builds proceed into one tree. Clearing EXIT closes the
  # ordinary case; comparing the pid inside the lock closes it for good, since a release can then
  # never touch a directory somebody else created.
  release_lock() {
    if [ -d "$LOCK" ] && [ "$(cat "$LOCK/pid" 2>/dev/null)" = "$$" ]; then rm -rf "$LOCK"; fi
  }
  trap 'release_lock' EXIT
  trap 'release_lock; trap - INT  EXIT; kill -INT  $$' INT
  trap 'release_lock; trap - TERM EXIT; kill -TERM $$' TERM
else
  if [ -n "$HOST" ]; then WORK="$ROOT/vendor/ffmpeg-build-host"; else WORK="$ROOT/vendor/ffmpeg-build"; fi
fi
SRC="$WORK/ffmpeg-$VERSION"
mkdir -p "$WORK" "$TARDIR"

TAR="$TARDIR/ffmpeg-$VERSION.tar.xz"
# The tarball stays in the checkout (release.yml publishes it as the LGPL corresponding source),
# but it is FETCHED once per machine: a fresh worktree copies the cached one rather than pulling
# 11 MB over the network again. The sha256 below is checked either way, so a corrupt or
# substituted cache copy fails exactly as a bad download would.
# EVERY WRITE HERE IS PID-UNIQUE THEN RENAMED, and the lock above does not cover it. That lock is
# per CONFIGURATION, and this file is shared by all of them: `make -j all sim` runs the ARM and the
# host build at once, they hold DIFFERENT locks by design, and both want this one tarball. Writing
# a fixed `$TAR.part` had them share that name too — one `mv` unlinking the path the other `curl`
# was still writing into, which is a failed build if you are lucky and a truncated 11 MB tarball
# published to the machine-wide cache if you are not. `mv`/`cp`-then-`mv` within one filesystem is
# atomic, so every reader sees either no file or a complete one, and the sha256 below still has
# the last word.
# The CACHED copy is named by the PIN, not by the version. Two pins of one version are two
# different tarballs, and a cache that cannot tell them apart hands every fresh checkout the one
# that no longer matches — a hard failure, on bytes the checkout never fetched, with no way to
# recover but to know to delete the file.
CACHED_TAR="$CACHE_ROOT/ffmpeg/ffmpeg-$VERSION-$(printf '%s' "$SHA256" | cut -c1-12).tar.xz"
# THE SHARED PATHNAME IS NEVER UNLINKED, only ever replaced by rename. The ARM and host builds
# hold different configuration locks by design and share this one file, so after a same-version
# re-pin both can find it stale at once — and an `rm` by the second would delete the good copy the
# first had just put there, failing that build's own verification a moment later. Everything
# happens on a pid-local candidate; `mv` publishes it, and two processes publishing identical
# verified bytes is harmless.
tarball_ok() { [ -f "$1" ] && [ "$(shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1)" = "$SHA256" ]; }
ensure_tar() {
  tarball_ok "$TAR" && return 0
  cand="$TAR.$$.part"
  rm -f "$cand"
  if [ -n "$CACHE_ROOT" ] && [ -f "$CACHED_TAR" ]; then cp "$CACHED_TAR" "$cand" 2>/dev/null || true; fi
  if ! tarball_ok "$cand"; then
    rm -f "$cand"
    echo "ffmpeg: fetching $VERSION"
    curl -fsSL "https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz" -o "$cand"
  fi
  mv "$cand" "$TAR"
}
ensure_tar
# Verify before extracting, and before CACHING: this tarball becomes code inside the shipped
# package. The order matters more than it looks now that the cache is machine-wide — publishing
# first meant a bad download, or a corrupt tarball already sitting in the checkout, was copied to
# a location every future worktree copies FROM. This build would still have failed safely, and
# every fresh checkout after it would have failed identically, on poison none of them fetched.
# `ensure_tar` has already replaced anything that did not match — a tarball this checkout was
# carrying from before the pin moved, most likely, `vendor/` being gitignored and surviving every
# branch switch. So reaching a mismatch HERE means the bytes upstream served do not match the pin,
# which is the case that must stop the build.
have=$(shasum -a 256 "$TAR" | cut -d' ' -f1)
if [ "$have" != "$SHA256" ]; then
  echo "ffmpeg: SHA256 MISMATCH for $TAR" >&2
  echo "  expected $SHA256" >&2
  echo "  got      $have" >&2
  exit 1
fi
if [ -n "$CACHE_ROOT" ] && [ ! -f "$CACHED_TAR" ]; then
  cp "$TAR" "$CACHED_TAR.$$.part" && mv "$CACHED_TAR.$$.part" "$CACHED_TAR"
fi

# EXTRACTION IS TRANSACTIONAL, because `[ -d "$SRC" ]` is not a test of completeness. Interrupt
# the first `tar xf` — a ^C, a full disk, a laptop lid — and the top-level directory is there with
# a fraction of the tree under it. Every later invocation in every checkout then skips extraction,
# builds against a partial source tree, and fails in a way that reads as an FFmpeg problem; the
# cache being machine-wide is what turns one interrupted build into everybody's. Extracting to a
# pid-unique staging directory and renaming means $SRC appears only once it is whole.
if [ ! -d "$SRC" ]; then
  # A staging directory whose process was killed outright leaves nothing behind that any later
  # run consults, but it does leave BYTES — and the point of this cache is that it does not grow
  # a copy per accident. Sweep any that no longer have a live owner before adding our own.
  for old_stage in "$WORK"/.extract.*; do
    case "$old_stage" in *'*'*) continue ;; esac
    old_pid=${old_stage##*.}
    if ! kill -0 "$old_pid" 2>/dev/null; then rm -rf "$old_stage"; fi
  done
  stage="$WORK/.extract.$$"
  rm -rf "$stage"
  mkdir -p "$stage"
  tar xf "$TAR" -C "$stage"
  if [ -d "$SRC" ]; then rm -rf "$stage"; else mv "$stage/ffmpeg-$VERSION" "$SRC"; rm -rf "$stage"; fi
fi

cd "$SRC"
# Reconfigure only when the flags change; FFmpeg's configure is slow and this script is a
# prerequisite of every build.
FLAGS_FILE="$SRC/.plx-flags"
NEW_FLAGS=$*
if [ ! -f "$FLAGS_FILE" ] || [ "$(cat "$FLAGS_FILE")" != "$NEW_FLAGS" ]; then
  echo "ffmpeg: configuring ($VERSION, $([ "${RELEASE:-}" = 1 ] && echo release || echo dev))"
  ./configure "$@" > "$WORK/configure.log" 2>&1 || {
    echo "ffmpeg: CONFIGURE FAILED — $WORK/configure.log" >&2; tail -20 "$WORK/configure.log" >&2; exit 1; }
  printf '%s' "$NEW_FLAGS" > "$FLAGS_FILE"
fi

echo "ffmpeg: building"
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" > "$WORK/build.log" 2>&1 || {
  echo "ffmpeg: BUILD FAILED — $WORK/build.log" >&2; tail -20 "$WORK/build.log" >&2; exit 1; }
# DESTDIR redirects the /plx prefix onto the real tree: the libraries believe they were
# configured for /plx (so they record no build path) and land in $PREFIX regardless.
make install DESTDIR="$WORK/destdir" > "$WORK/install.log" 2>&1
mkdir -p "$PREFIX"
cp -R "$WORK/destdir/plx/." "$PREFIX/"

# Strip: these ship, and the debug info is ~4x the code. Only the shipped build — the host copy
# never leaves this machine, and `strip` on a Mach-O dylib is a different flag set for no gain.
if [ -z "$HOST" ]; then
  for f in "$PREFIX"/lib/lib*-plx.so.*; do
    [ -f "$f" ] && [ ! -L "$f" ] && "${CROSS}strip" --strip-unneeded "$f"
  done
fi

# LAST USED, not last built. `--cache` prunes trees nobody has needed for a month, and a
# directory's own mtime only moves when its DIRECT children change — so the hottest configuration
# on the machine, rebuilt into and copied out of every day, keeps the mtime it was created with
# and gets collected on its thirtieth day. Touching a marker on every successful run, warm ones
# included, is what makes the age mean what the prune reads it as.
if [ -n "$CACHE_ROOT" ]; then : > "$WORK/.last-used"; fi

echo "ffmpeg: installed to $PREFIX"
# **`if`, not `&&`, and that is the whole bug this shape exists to avoid.** This loop is the LAST
# command in the script, so its status is the script's, and one of the two globs never matches:
# a cross build produces no `.dylib` and a host build no `.so.*`. With `[ -f ] && [ ! -L ] && printf`
# the non-matching glob leaves the unexpanded pattern in `$f`, the test is false, and the whole
# script exits 1 — after having done every byte of its work and printed the libraries it just
# installed. Make then fails the target on a COLD tree while a second run succeeds, because by then
# the header exists and the recipe never re-runs. That reads as a flake and is not one; CI builds
# clean, so it fails there every time.
for f in "$PREFIX"/lib/lib*-plx.so.* "$PREFIX"/lib/lib*-plx.*.dylib; do
  if [ -f "$f" ] && [ ! -L "$f" ]; then
    printf '  %-28s %6s KB\n' "$(basename "$f")" "$(( $(wc -c < "$f") / 1024 ))"
  fi
done
