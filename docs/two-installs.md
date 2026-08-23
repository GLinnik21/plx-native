# Two installs on one television

**Status, 2026-08-21: BUILT ENTIRELY OFF-DEVICE. NOTHING BELOW HAS BEEN ON A TELEVISION.**

Everything here is either a fact provable on a desk — a make variable, a `/proc/self/exe` read, a
byte in a JSON payload — and then it is stated flatly with how to re-derive it; or it is an
assumption that needs the set, and then it is in §6 and nowhere else. The two are never mixed. §6
is the short list of what one afternoon with the television would settle, ordered by how much
damage a wrong assumption does, and the first item is the one the whole change exists to make
safe: **the ACB video bind under a second app id.**

The mechanism is deliberately small. There is one axis — `FLAVOR` — one derived id, one derived
runtime root, and no codegen input anywhere. `pkg/plxnative` is still one artifact.

---

## 1. What this prevents

`make deploy` scp's a binary into `/media/developer/apps/usr/palm/applications/com.beb.plxnative/`.
That directory is the app the household watches with. Every deploy overwrote it — with a debug
build, with a half-finished feature, with whatever was on the branch — and there was no way to keep
a working copy while developing, because there was exactly one place a copy could go.

The cost is not a broken build; it is a broken *evening*. A deploy lands the moment it lands, so
"I'll put the good one back afterwards" is a promise made to somebody who is already watching
something. And the reverse failure — a run of the harness picking up whatever the last
manual session left armed, and grading it — is the "plausible wrong data" class this project's
testing section is built around.

So: two ids, two installs, two tiles.

| | stable | debug |
| --- | --- | --- |
| app id | `com.beb.plxnative` | `com.beb.plxnative.debug` |
| install dir | `…/applications/com.beb.plxnative` | `…/applications/com.beb.plxnative.debug` |
| runtime root | `/tmp` | `/tmp/com.beb.plxnative.debug` |
| session file | `/media/developer/com.beb.plxnative-auth.json` | `/media/developer/com.beb.plxnative.debug-auth.json` |
| launcher title | `PlxNative` | `PlxNative debug` |
| launcher artwork | `pkg/icon.png` | `pkg/dev/icon.png` (amber DEV bar) |
| plex.tv device name | `PlxNative (LG TV)` | `PlxNative debug (LG TV)` |

**`FLAVOR ?= debug`, in the tracked Makefile.** The asymmetry is deliberate and it is the whole
safety argument: every command in this repo's muscle memory is spelled `make deploy` / `make run` /
`./tests/run.py` with no flavour, and each one used to overwrite the only install there was.
Deploying to `debug` when you meant `stable` costs one retyped command; deploying to `stable` when
you meant `debug` destroys a working install with no undo. So the safe one is free and the other
has to be typed. Tracked rather than a gitignored dotfile (which is how `.tv-host` works, and was
the obvious thing to copy) because a fresh clone — and especially a fresh worktree in an agent
fleet — has no dotfile, so the dangerous default would be inherited invisibly by exactly the
checkouts nobody is watching.

**Ask the Makefile, never restate it.** Seven query targets are the only supported way for a tool to
learn any of this, and they are real echo recipes:

```sh
make -s print-flavor   FLAVOR=debug     # debug
make -s print-appid    FLAVOR=debug     # com.beb.plxnative.debug
make -s print-appdir   FLAVOR=debug     # /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug
make -s print-rundir   FLAVOR=debug     # /tmp/com.beb.plxnative.debug
make -s print-eventlog FLAVOR=debug     # /tmp/com.beb.plxnative.debug/plxnative-events.log
make -s print-appport  FLAVOR=debug     # 8911  (the capture listener's port — §3.1)
make -s print-tv                        # the television's address, expanded
```

**Not `make -p` / `make -pn`.** That prints a recursive variable's *unexpanded definition*, so `TV`
comes back as the literal `$(strip $(shell cat .tv-host 2>/dev/null))` — every ssh built from it
then fails, and the tool reports "TV unreachable" against a television that is awake and answering.
`tools/tv-session.sh`'s `tv_host()` documents that trap because it hit it.

An unknown value is a parse-time `$(error)`, not a fallback. `make FLAVOR=stabel deploy` would
otherwise mint a third registered app called `com.beb.plxnative.stabel` on the television — LG's id
charset accepts it and nothing downstream objects — and the symptom is a mystery tile on a TV
rather than a message on a terminal.

## 2. The identity model: the id is the directory, read at runtime

**The app id is not compiled in. It is the name of the directory the binary is running out of**,
read from `/proc/self/exe` at first use (`rust-modules/src/paths.rs::app_id`, via
`std::env::current_exe`).

That is sound because **our own package lays that directory down from the appinfo `id`, and three
gates assert the two agree** — so `applications/<id>/` names the id this app declared itself to be.
None of the three needs a television:

- `ci/mkipk.py` writes `packages/<id>/packageinfo.json` straight from the descriptor and then exits
  non-zero unless the staged `data/usr/palm/applications/*` is exactly `[appinfo["id"]]`. The
  Makefile lays the directory down (`STAGE`) and `mkipk.py` reads the descriptor, so it is the one
  place that sees both at once.
- `ci/check-package.py` goes the other way: it **derives** the id FROM the staged directory name and
  then checks it against that directory's own `appinfo.json`, the control file's `Package:` and
  `packageinfo.json` — four spellings, asserted equal to each other rather than to a constant.
  Deriving rather than reading `pkg/appinfo.json` is deliberate: taking the stable id on faith would
  grade a debug package against a directory that is not staged, and every path below it would go
  vacuous — an empty glob prints nothing, fails nothing, and reports success. That is the same
  defect class that let a missing `packageinfo.json` hide for months.
- `make deploy` checks it once more, immediately before the scp: the emitted `appinfo.json`'s `id`
  must equal the `APPDIR` it is deploying into. Packaging asserts this several times over and the
  path used a hundred times a day had no equivalent at all, which is the asymmetry that check
  exists to close.

So the directory is authoritative *for us*, by construction. **What the firmware does with a
disagreement is a separate question and this document does not answer it** — see §6.7. An earlier
version of this paragraph asserted that webOS refuses to register an app whose `appinfo.json` `id`
differs from its directory, and called reading the path "reading the authority". Nothing in this
tree observes that, the same sentence had been copied into four code comments, and it has been
removed from all of them. The design does not need it: the id is right because our own packaging
made it right.

Two consequences, and they are the reason for the design rather than side effects of it:

- **`pkg/plxnative` stays ONE artifact.** Nothing about the flavour reaches codegen. No second
  `--target-dir`, no second `pkg/.build-config` stamp, no `cfg!`, no rebuild when you flip
  `FLAVOR`, and — the expensive one — no second FFmpeg build. This project's classic failure is the
  stale artifact make believes is fresh (the Makefile's `pkg/.build-config` comment is the
  account); a compiled-in id would have added a fresh axis of exactly that, on top of the
  `RELEASE=1` axis that already deletes the binary at parse time to stay honest.
- **A mis-deployed binary tells the truth.** Copy the debug build into the stable directory by hand
  and it identifies as `com.beb.plxnative`, uses `/tmp`, reads the stable session file and puts the
  stable id in its Load payload. It is in the wrong place, but it is not *lying about where it is*,
  which is the difference between a bug you can see and a log you cannot trust.

The shape test is structural rather than name-based: the parent directory is the id only when ITS
parent is literally `applications`. That makes both webOS prefixes answer — `/media/developer/apps/…`
under Developer Mode and `/media/cryptofs/apps/…` under the Homebrew Channel — while a host build,
where the binary sits in `target-sim/debug/`, falls through to the stable id instead of inventing an
app called `debug`.

`paths::flavour()` is the one place that knows how a flavour is *spelled*: it strips
`STABLE_APP_ID` and then a `.`, giving `None` for the shipped app and `Some("debug")` otherwise.
Everything that must differ asks that, so no second parser exists.

**The stable id is spelled in three languages that cannot see each other** — `APPID_STABLE` in the
Makefile, `paths::STABLE_APP_ID` in Rust, `STABLE_ID` in `ci/flavor.py`. `python3 ci/flavor.py
--selftest` reads the other two and compares, and `make check` runs it. Three copies of one string
is only safe while something checks.

**Three is now the whole count, and it was four.** `src/starfish.c`'s `acb_create` carried a fourth
— an inline literal it substituted for a NULL `appId` — and that copy was the far half of a double
fallback: Rust read `getenv("APPID")` and passed NULL when SAM had not exported it, and C then
quietly claimed to be the app users install. With two installs on one set that binds the video
plane for the wrong application, which is a black plane with working audio and no error line.
The near half is gone (`engine::acb_init_acb` passes `paths::app_id()`, which cannot be absent) and
the far half now refuses and logs instead of guessing. Worth recording rather than just fixing: it
was a bare literal in C, so no selftest could have seen it — the guarantee above is only as wide as
the languages it names.

**One descriptor transform, and the stable one is asserted to be the identity.** `ci/flavor.py`
patches the tracked `pkg/appinfo.json` rather than duplicating it: exactly two of its fourteen
fields may move (`id`, `title`), and the selftest asserts the set of moved keys is exactly that, so
widening it is a decision somebody has to make on purpose. The stable transform coming out
byte-identical is what guarantees the released artifact's sha256 — the entire integrity story,
since nothing here is code-signed — cannot be perturbed by the existence of a second identity.

## 3. The shared-resource inventory

The interesting question is never "are they separate" but "what did we forget to separate". Both
halves are listed, because the second half is the one that decides what you can and cannot do with
two installs — and the list is **re-derived against the tree, not inherited from the last version
of itself**, which is the only way it stays worth reading. Three surfaces have been separated since
this section was first written: the two local sample payloads and the capture listener's port. None
of the three appeared in *either* half beforehand, and that is exactly the point — what gets missed
is what nobody wrote down, so the way to check this section is `grep` for what still resolves
outside `paths::in_runtime_dir` (plus the resources a directory cannot separate at all: ports, and
anything keyed by SAM). A fourth, the legacy session-file candidate, was worse than missing: it sat
inside a bullet already claiming to be separate. It is written out in full below for that reason.

### 3.1 Separate

- **The runtime root, and therefore everything in it.** `paths::resolve_runtime_dir`: the stable
  install keeps `/tmp` byte for byte; a flavoured one gets `/tmp/<app id>`. Because every runtime
  surface already composes on `paths::in_runtime_dir`, moving the root separated all of them at
  once and left every name unchanged — all but the last of these, which composed on nothing and had
  to be moved by hand:
  - the three logs — `plxnative-events.log` (truncated per launch), `plxnative-crash.log`
    (append-only), `plxnative-stderr.log`;
  - all ~40 `plxnative-*` dev triggers, `dev::DIAG` and `dev::any_trigger_present`'s scan;
  - the `plxnative-remote` FIFO;
  - the capture listener's file trigger (`plxnative-capture`) and the two profiler JSONLs;
  - the two local Annex-B sample payloads, `sample.h264` and `sample.h265` — the only runtime files
    that are *not* spelled `plxnative-*`, which is why they have their own door
    (`dev::read_sample`). They took an absolute `/tmp` path until after the split landed, so they
    were the last two surfaces still pinned to a shared root while every other one had moved. Two
    installs reading one sample is harmless in itself; a rule with a hole in it is not, because it
    stops being checkable. They now live in the install's own root —
    `$(make -s print-rundir)/sample.h264`.

  Shared, these produced a set of collisions whose symptom is never an error and always evidence
  about the wrong process: the launching install truncates the other's event log, one append-only
  crash log holds two binaries' faults with nothing saying which is which, one FIFO takes keys
  meant for the other, and one trigger namespace boots both to somebody else's screen.
- **The session file.** `/media/developer/<id>-auth.json` (and the `/media/internal/.<id>-auth.json`
  fallback for the Homebrew-Channel jail, which mounts the opposite pair rw/ro). Two installs, two
  sign-ins, two rosters, two `X-Plex-Client-Identifier`s.

  **Naming the file by id was not enough, and the gap is worth the paragraph.**
  `paths::session_candidates` offers a fourth, legacy entry — the in-app-dir `auth.json` under
  `LEGACY_APP_DIR`, a MIGRATION source spelled as a literal that names the **shipped** install's
  directory. `session::load` takes the first candidate that EXISTS, and a freshly installed debug
  build has none of its own; so that one ungated entry would have handed a developer build the
  other install's account token, every per-(user, server) PMS token and the whole Plex Home roster
  — which it would then write back under its own name. Exactly the sharing the id-keyed candidates
  exist to prevent, arriving through the one line that was not made flavour-aware with them. It is
  now gated on `flavour().is_none()`, so only the shipped install may offer it, and the host test
  grades the **whole** candidate list rather than its first two entries (asserting only those is
  what could not see this).
- **The plex.tv identity.** The client identifier is minted per session file, so each install is its
  own authorized device; and `plex::identity::device_name()` appends the flavour, so the account's
  device list reads `PlxNative (LG TV)` and `PlxNative debug (LG TV)` rather than two entries
  spelled identically — where revoking "the one on the TV" is a coin flip. The shipped name is
  unchanged, deliberately: it is already in every existing user's device list, and a rename there
  reads as a new, unknown device.
- **The launcher tile** — its own `title` (`PlxNative debug`) and its own badged artwork from
  `pkg/dev/` (`docs/distribution.md` §10.3).
- **The capture listener's TCP port.** 8910 for the shipped install, 8911 for a flavoured one
  (`capture::default_port`, and `make -s print-appport` is the same rule for the shell;
  `ci/flavor.py --selftest` compares the two, and is what will object when a third flavour needs a
  real decision). A port is the one runtime resource the runtime root cannot separate, and sharing
  it fails silently on both sides: the second `bind` loses with one line in a log nobody is tailing
  (`capture: bind/listen … failed`), and the operator then watches one install's picture while
  every key they type goes into the other install's FIFO. Reachable only with a dev build on both
  ids at once — which `release-guard` makes deliberate rather than accidental, but the point of a
  named hatch (§5) is that somebody will use it.
- **The Load payload's `option.appId` and the ACB id.** `player/engine.rs` carries `@APPID@` in
  every BUFFERSTREAM payload variant and substitutes `paths::app_id()` at the one choke point they
  all pass through (asserted to match exactly once per payload, in the host suite — a `replace`
  matching twice would leave a second `appId` key for `with_window_id` to splice a duplicate
  `windowId` onto, and one matching nothing would hand LG's parser the placeholder as a literal id;
  both are silent). `acb_init_acb` passes the same string to `AcbAPI_initialize`. It used to be
  `env::var("APPID")` with a NULL fallback that `starfish.c` turned into the shipped id — a double
  fallback under which a developer install announces itself as the released app on any launch where
  SAM does not export `APPID`. See §7 for what is and is not known about who reads that key.
- **The registration itself** — SAM's app record and `closeByAppId`, both keyed on the id and so
  naming exactly one install (§4.2's table). Which is why a flavour must be *installed*, not
  `mkdir`'d (§5).
  The Dev Mode installer's **LS2 role file is ASSUMED to follow it, not observed.** One exists for
  the shipped id (`docs/distribution.md` §3.5); whether `dev/install` writes a second keyed on the
  flavoured id has never been checked on a set — §6.3, which is also the assumption §6.1 leans on
  hardest. Listed here because the registration is what makes a flavour a real app, but the role
  file is the half of it this document cannot assert.

### 3.2 Still shared, and always will be

- **The jail template and the group set.** The profile is chosen by install *prefix*, not by id
  (`docs/distribution.md` §3.5), and both installs sit under `/media/developer/apps/…`, so both get
  `jail_native_devmode.conf`: same mounts, same `video,audio,luna,compositor,crashd,se`. The chroot
  *directory* is per id (`/var/palm/jail/<id>`), but the rules inside it are one file. Whether SAM
  hands two app ids the same numeric uid is §6.4.
- **One hardware video plane and one decoder.** The whole playback architecture is a single
  overlay plane composited under a single foreground app's surface. Two installs cannot play at
  once, and nothing about this change makes them able to — which is also why §6.6 (the
  background/foreground handoff with one install suspended mid-playback) is on the device list
  rather than assumed.
- **`/media/developer`.** Both app directories and both session files live on it. A second install
  costs roughly 13 MB unpacked — a 9.3 MB binary, ~2.1 MB of bundled FFmpeg, the 1.5 MB splash and
  the two fonts — which is small but is not nothing on a partition nobody here has measured (§6.5).
- **`pkg/splash.png`, deliberately.** Both flavours ship the same 1.5 MB image. A second copy would
  exist to label two seconds of a boot you *just chose from the launcher*, having already read the
  tile that says `PlxNative debug` under artwork with an amber DEV bar on it. It is the one place
  the flavour is knowingly not carried through, and it is recorded here so nobody re-derives it as
  an oversight.
- **`requiredMemory: 160`.** SAM's memory budget is claimed once per *registered* app, so two
  installs are two claims. Only one runs at a time, so this is a budgeting question rather than a
  runtime one, but it is a second claim on the same set. The number is the measured **152 MiB**
  peak rounded up (`docs/distribution.md` §6.10 — boot, browsing and 4K HDR playback on the dev
  set); this line said `60` against "a measured ~74 MB peak" long after both halves were retired,
  and §6.10 records that the ~74 MB was never a measurement at all. 60 was worse than declaring
  nothing, since webOS substitutes 120 for an app that declares none.
- **The host build tree, deliberately — the one entry here that is not on the television.**
  `FLAVOR` never reaches codegen (§2), so `pkg/plxnative`,
  the `pkg/.build-config` stamp and the cargo `--target-dir` are one set for both installs — that
  is the point, not an oversight. The consequence to know is at the ipk stage: `make ipk` does
  `rm -rf ipkroot/data/usr` and re-stages, so **exactly one flavour is staged at a time** (which is
  what lets `check-package.py` derive the packaged id from `applications/*` at all), while the
  built `.ipk`s themselves coexist in `pkg/` under their two filenames — `make ipk` deletes only
  `pkg/$(APPID)_*_arm.ipk`, never the other flavour's.
- **The television.** One set, one app instance, no lock. Two installs do not make the TV a
  non-mutex; they make it possible to *keep* a working install while iterating on another. Two
  harness jobs still kill each other's app.

## 4. Three traps, and every one of them is silent

Two are name collisions and the third is a file mode. None of the three produces an error;
all three produce evidence about the wrong thing, which is worse.

### 4.1 `plxnative-` — why the runtime root's separator is a DOT

`dev::any_trigger_present` decides whether the boot is automated by scanning the runtime root for
entries whose name begins `plxnative-`, and suppressing the who's-watching picker if it finds one.
It is the one surface in `dev.rs` that names no path at all.

A second install's root named `/tmp/plxnative-debug` would therefore sit in `/tmp` reading, to the
*other* install, as a permanently armed trigger — silently changing which screen the released app
boots to, with no line in any log. The full app id contains no `plxnative-`, so
`/tmp/com.beb.plxnative.debug` cannot. That is the first reason.

The second is independent, because a failure this quiet deserves two: `any_trigger_present` now
also requires the entry to be a **file**, so a directory matching the prefix cannot arm anything
whatever it is called. The host suite grades it (`dev.rs::a_directory_is_not_an_armed_trigger`,
holding `testlock::serial()` — the runtime root is a crate global in exactly the sense that lock
exists for).

### 4.2 `com.beb.plxnative` is a PREFIX of `com.beb.plxnative.debug`

Any match on the app id must be **anchored on a delimiter**. A bare `grep com.beb.plxnative`,
`case … in com.beb.plxnative*)`, or a `startswith` matches both installs and reports the wrong one
— or kills it.

This is the same shape `src/main.c:57-61` already documents from the other side: the crash tracer
matches `/proc/self/maps` lines on `/plxnative\n` and `/plxnative ` rather than on the app-directory
name, precisely because the directory is *itself* called `…com.beb.plxnative/` and a bare substring
test would also match every library deployed beside the binary.

Three scopes are now three different questions, and picking the wrong one is how a tool grades the
other install:

| test | scope | matches |
| --- | --- | --- |
| `pidof plxnative` | **name** | BOTH installs — both binaries are named `plxnative` |
| `fuser <appdir>/plxnative` | **inode** | exactly one install |
| `closeByAppId {"id":…}` | **id** | exactly one install |

`pidof plxnative` returns two pids in an order busybox does not promise, so every liveness check
built on it had to move to `fuser $(make -s print-appdir)/plxnative` — or to a loop resolving
`readlink /proc/<pid>/exe` per pid, when the pid itself is what you need. The Makefile's `CLOSE_SH`
kills by path for the same reason: a name-based kill (`pidof`, `killall`) takes down the other
install too.

### 4.3 The runtime root must be 1777, and the `chmod` must be separate

Two uids write into that directory and neither can be made to go second: `tests/run.py` and
`tools/tv-session.sh` arm triggers there over ssh **as root, before the app has ever booted**,
while the app runs jailed under its own uid and creates its logs there. Whoever gets there first
sets the mode, so any owner-only mode locks the other out.

`create_dir_all` (and `mkdir -p`) applies the process umask, which silently drops the group and
other bits — hence `paths::ensure_runtime_dir` follows it with an explicit
`set_permissions(0o1777)`, and the Makefile's `BOOT_SH` follows `mkdir -p` with an explicit
`chmod 1777`. `/tmp` on the television is 1777 for the same reason; a per-install root inside it
must not be stricter.

The failure it prevents is already recorded from the other side: a root-owned event log the jailed
app cannot write stays 0 bytes, and **every tool in this repo reports a 0-byte log as "no line
found" — i.e. exactly like a total regression.**

## 5. Installing a flavour, and removing one

A flavour must be **installed once** before `make deploy` can reach it.

```sh
make FLAVOR=debug install     # build its .ipk, install it via appInstallService, then deploy into it
make FLAVOR=debug uninstall   # remove it (refuses the stable id)
```

`install` goes through `luna://com.webos.appInstallService/dev/install` — the same path a user
takes — because creating a second *app* is not the same operation as writing files into one that
exists. SAM has to learn the id, and the LS2 role file that permits `com.webos.media.*` is written
by the installer, not declared by us.

**And then it deploys, deliberately.** `appinstalld` replaces `applications/<id>/` **wholesale** —
the same fact that keeps the session file outside the app directory — so an install that stopped
after installing would leave the *packaged* binary in place and you would be looking at a build you
did not make. That is this project's least favourite failure mode wearing a different hat.

`make deploy` `test -d`s the app directory and fails naming `make FLAVOR=… install`, rather than
`mkdir -p`-ing a directory SAM knows nothing about. A hand-made app directory gets no registration
and no role file, so the app would launch and then be denied the LS2 calls the ACB bind needs — a
stuck pipeline rather than an error.

`uninstall` refuses the stable id outright: removing the app the household watches with is not
something a make target should make easy, and `appinstalld` gives no undo. It also `rm -rf`s that
install's runtime root, which takes its three logs with it — so pull anything you still want out of
`$(make -s print-rundir FLAVOR=…)` first.

**Two guards keep a dev build off the released id.** `release-guard` refuses `deploy`/`ipk` on
`FLAVOR=stable` without `RELEASE=1`, naming `ALLOW_DEV_ON_STABLE=1` as the deliberate hatch (a gate
with no hatch gets deleted rather than respected — the legitimate use is reproducing a user's report
against the shipped id with instrumentation on). `ci/check-package.py` then asserts the same rule on
the **packaged bytes**, which is the half that survives somebody reaching for the hatch and
forgetting: the stable package must not contain the `plxnative-noidle` dev witness.

That gate is graded **unconditionally**, outside the build-configuration branch, and the nesting is
why it is worth a paragraph rather than a clause. `pkg/.build-config` records a feature set, and it
maps to `None` for anything that is neither shipped configuration — the Makefile's own header
documents a third (`--no-default-features --features devtriggers`, the README-screenshot recipe).
Nested under that stamp, exactly that combination satisfied `release-guard` (`RELEASE` is
non-empty), printed `SKIP — neither shipped configuration`, and would have packaged a dev-trigger
binary under the released id on an all-green run. "This package carries no dev-trigger surface" is a
property of the bytes and needs no stamp to grade, so it no longer asks for one. The witness is
graded from *both* sides for the same reason — the dev leg asserts `plxnative-noidle` is still
emitted — because the previous witness (`plxnative-autoplay`) matched nothing in *either*
configuration, and so printed `ok` over every build for as long as it existed.

**Which install produced a log** is the first line of the event log, written before anything can
fail:

```
install: id=com.beb.plxnative.debug flavour=debug runtime=/tmp/com.beb.plxnative.debug features=dev APPID_env=<value|unset>
appdir: /media/developer/apps/usr/palm/applications/com.beb.plxnative.debug (from current_exe)
```

It exists because none of the obvious witnesses work. Both binaries are named `plxnative`, so
`pidof` cannot tell them apart on this busybox set; `pkg/plxnative` is a path every configuration
writes, so an md5 against the local build proves only that *some* flavour of *some* configuration
matches. `features=` is `dev` or `release`. `APPID_env=` is evidence rather than configuration —
see §6.2.

## 6. What only a television can settle

Ranked by how much damage a wrong assumption does. Every one of these is answerable in a single
session with the set; several are answerable from one run's event log.

1. **Does the ACB video bind work under a second app id?** This is the one thing you most need the
   debug install for, and it is the one thing the debug install's existence is riskiest about.
   Two app ids leave this app: the third argument of `AcbAPI_initialize`, and the Load payload's
   `option.appId`. **That the ACB argument is an app id is all that is established** — both
   reference implementations pass one, and the call shape this repo actually quotes is Kodi's
   (`AcbAPI_initialize(…, PLAYER_TYPE_MSE, getenv("APPID"), …)`, `docs/distribution.md` §3.5).
   What either consumer *does* with the value has not been read out of this television's binaries,
   so no mechanism is claimed here; §7 grades both halves. This paragraph used to say the argument
   locates the app's own compositor window, which was invented — the same sentence `engine.rs`'s
   `@APPID@` comment now refuses to make.
   Both ids now say `com.beb.plxnative.debug`. If LG's stack keys on the *registered* id and finds
   it, this is fine; if anything keys on a hardcoded id, or on a `getenv("APPID")` that disagrees
   with ours, it is not.
   **The failure shape is audio over a black video plane, with no error line anywhere** — exactly
   the shape of the SDL-version handshake bug in `docs/webos5-port.md` §3.4: black screen, working
   audio, clean log.
   *Settled by:* play anything on the debug install and look at the panel. In the event log,
   `acb create=` must be non-zero and `setMediaVideoData sent` must appear — the latter is the exact
   line `tests/run.py`'s `require_video_bound` grades, so any harness run that reaches this install
   answers it as a side effect. Cross-check the same item on the stable install.
2. **Does SAM export `APPID` to a native app on this firmware, and to what?** Nothing readable off
   a desk answers this. Kodi's webOS backend passes `getenv("APPID")` straight into
   `AcbAPI_initialize`, which is evidence that *some* webOS sets it, not that ours does.
   `engine::acb_init_acb` no longer depends on it — the install directory is the authority — so this
   is now free information rather than a dependency.
   *Settled by:* the `APPID_env=` field of the boot `install:` line, in any run's log, on either
   install. `unset` is as interesting an answer as a value, and a value that is *not* the id we
   were installed as is the most interesting of the three.
3. **Does a second LS2 role file appear?** `/var/palm/ls2-dev/roles/pub/com.beb.plxnative.json`
   exists for the stable install and grants `com.webos.media.client.*`, `com.webos.rm.client.*` and
   `com.webos.pipeline.*` (`docs/distribution.md` §3.5). The Dev Mode installer writes it; whether
   `dev/install` writes a second one keyed on the flavoured id has never been observed.
   *Settled by:* `ls /var/palm/ls2-dev/roles/pub/` after `make FLAVOR=debug install`. If there is
   only one file, item 1 is very likely to fail and this is why.
4. **Do both installs run under the same uid?** The stable app is `Uid: 6910` (device-proven
   2026-08-01, `docs/distribution.md` §3.5). Whether SAM allocates per-app-id uids or one uid for
   all dev-mode native apps decides nothing about correctness here — the 1777 runtime root is
   correct either way — but it decides whether the two installs can read each other's files at all,
   which is worth knowing before assuming isolation that is not there.
   *Settled by:* `cat /proc/<pid>/status | grep Uid` for each, resolving pids via
   `readlink /proc/<pid>/exe` (not `pidof`, §4.2).
5. **Both tiles in the launcher, and the free space.** Two tiles side by side, the debug one
   readable in the 115x115 box the launcher actually draws into (`docs/distribution.md` §10.1 —
   every icon is resampled into that box regardless of source, and the file selected is the 130 px
   `largeIcon.png`, not the 80 px `icon.png`), the amber bar unmistakable, the titles
   distinguishable in SAM's own dialogs (which show the title, never the icon). And `df` on the app
   partition before and after, against the ~13 MB estimate in §3.2.
   *Settled by:* a photograph of the launcher and one `df -h /media/developer`.
6. **The background/foreground handoff between two installs.** `app.rs` suspends the buffer-feed on
   SDL's `0x103`/`0x104` and reloads on `0x106`. That path was built for an OS app-switch to
   *something else*; switching to the other copy of this app, while the first is suspended
   mid-playback with a live Starfish session and an ACB bind, is a case it has never seen. One
   video plane, two claimants.
   *Settled by:* start playback on stable, launch debug from the launcher, come back.
7. **Does webOS police `appinfo.json`'s `id` against the directory it was unpacked into?** Last,
   because nothing depends on the answer — §2's soundness argument is that *our* packaging makes
   the two agree, asserted three times off a desk, so a firmware that never looks changes nothing.
   It is here because the opposite claim ("webOS refuses to register a mismatch, so reading the
   path is reading the authority") was stated flatly in this document and in four code comments,
   and this repo cannot observe it. The interesting half is the failure MODE if it does police:
   a rejected install says so, whereas one that registers nothing and reports success is the same
   silent shape as everything else in this section.
   *Settled by:* hand-edit the `id` inside an installed app directory's `appinfo.json`, relaunch,
   and see whether SAM still launches it by the directory's name — a deliberate experiment on the
   **debug** install, never the stable one.

## 7. The `appId` evidence, graded honestly

**No tool in this repository can answer "does libpf read `option.appId`, and what does it do with
it".** That is worth stating plainly because the tool that looks like it should is right there.

`tools/fwcompat.py` reads webosbrew's firmware **symbol inventories** — `name`, `package`,
`needed`, `symbols`, and nothing else, for 14 real LG images. It answers "does this release export
that function". A JSON payload key path lives in `.rodata`, so it is **invisible** to that
database; CLAUDE.md is explicit about this, using
`option.externalStreamingInfo.contents.DolbyHdrInfo` as the example. Proving a payload key across
releases needs the actual `.so` files, which for other releases we do not have at all.

What we *do* have is this television's own binaries, and the way to read them is
`.claude/skills/decompile-tv-lib/` (`decomp.sh str` for the string pool, `xref` for who references
a literal, `fn` to decompile). The `DolbyHdrInfo` and `contents.immersive` nodes in
`docs/dolby-vision.md` §2 were both recovered exactly that way and neither was guessable.

What is already known, and its provenance:

- **The third argument of `AcbAPI_initialize` is an app id — and that is the whole of it.** The
  provenance is call shape in another client's source, this repo's third evidence tier: Kodi's
  webOS backend passes `getenv("APPID")` into that position
  (`AcbAPI_initialize(…, PLAYER_TYPE_MSE, getenv("APPID"), …)`, quoted in `docs/distribution.md`
  §3.5), and `mariotaku/ss4s` binds the same symbols in the same sequence — though only Kodi's
  argument list is quoted anywhere in this repo. What the argument is *for* — whether ACB resolves
  a window with it, forwards it, or merely stores it — does not follow from that and is not
  established anywhere in this tree. `engine::acb_init_acb`'s comment says the same in the same
  words, deliberately: two places stating one honest limit is safer than one stating it and
  another inventing a mechanism.
- **ACB posts the app id onward.** `docs/dolby-vision.md` §3 records
  `ACB::AcbCore::setMediaAudioData` @0xfda4 parsing its JSON, dedup'ing against a cached copy, and
  posting `luna://com.webos.service.acb/setAudioInfo` with `{"appId":…,"pipelineId":<mediaId>,
  "audioInfo":…}`. That `appId` is the one handed to `AcbAPI_initialize`, and it travels off-process
  into a Luna service — so it is not an inert string that ACB merely stores.
- **`libcbe` builds the same envelope.** `media::MediaAPIsWrapper::SetDolbyAtmosInfoToACB`
  @0x01b976f0 is the only caller of the ACB audio entry point in ~70 harvested libraries and is the
  path LG's own web apps take; the `context` it carries is the identical string `acb_bind` passes to
  `setMediaId`.

What is **not** known, and would need a decompile session against the harvested
`libpf-1.0.so.1.0.0`, `libplayerAPIs.so` and `libAcbAPI.so.1`:

- whether `CustomPipeline::parseOptionStringSpi` (the function that *does* read
  `option.externalStreamingInfo.contents.*`, per `docs/dolby-vision.md` §2a) also reads
  `option.appId`, or whether that key is consumed higher up in
  `libplayerAPIs::generateJsonPayloadForPlayer`;
- whether any of it is compared against SAM's notion of the calling app rather than merely carried;
- whether an id SAM does not recognise is rejected, ignored, or silently binds nothing;
- what `AcbAPI_initialize` does with its third argument at all — the first bullet above establishes
  that it *is* an app id and nothing further. This is the cheapest of the four: it is the same
  binary `docs/dolby-vision.md` §3's `setMediaAudioData` finding was recovered from, so the harvest
  already exists and only that one function has not been read.

Until somebody runs that, §6.1 is the answer: play something on the debug install and look at the
panel. A picture settles it in one attempt, and the decompile only becomes necessary if there is
no picture.
