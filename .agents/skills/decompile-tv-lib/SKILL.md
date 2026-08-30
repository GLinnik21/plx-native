---
name: decompile-tv-lib
description: >
  Read the TV's own closed, stripped native libraries — libplayerAPIs (StarfishMediaAPIs),
  libpf (the media pipeline), libAcbAPI, libcbe (the Chromium media stack), the DILE/VPQ
  layers — by harvesting them off the device and decompiling them locally with Ghidra.
  Use when a question can only be answered by what the firmware actually DOES: which JSON
  keys a Load payload parser accepts and at what path, what an enum→string table contains,
  what a struct's field layout is, whether a symbol that exists is reachable from the mode
  we run in, or why a call that "should" work does nothing. Also use before binding any new
  FFI into a TV library, as the evidence half of `bind-tv-lib-abi` — that skill requires
  offsets and symbol behaviour to be PROVEN, and this is how you prove them. Reach for it
  instead of guessing from a symbol name, a header found online, or another client's source:
  those tell you what some firmware does, not what THIS one does.
---

# decompile-tv-lib — read the firmware instead of guessing at it

The television's media stack is closed, **stripped**, and has no public documentation. LG
renamed payload keys between webOS 4 and 5, ships symbols that exist but are unreachable from
the mode we run in, and parses fields nobody outside LG has written down. Every one of those
has already cost this project a wrong assumption. The binaries are the only authority.

`decomp.sh` is the whole interface.

```sh
D=.agents/skills/decompile-tv-lib/decomp.sh

$D pull                          # harvest the media stack off the TV (needs the TV awake)
$D list                          # what is harvested
$D str  playerAPIs dolby         # .rodata strings   — always start here, it is instant
$D syms playerAPIs getHdrType    # exported symbols  — dynsym only, see "stripped" below
$D fn   libpf DolbyHdrInfo 4     # DECOMPILE matching functions
$D xref libpf "immersive"        # which functions reference a string literal
$D clean                         # drop the analysis cache, keep the binaries
```

`<lib>` is any substring of a harvested filename. The lab defaults to `/tmp/tvlab`; override
with `DECOMP_LAB`. Analysis is cached per library — the first `fn` costs ~15–60 s, everything
after it is seconds.

## Method: strings first, then the code that reads them

For the question this skill exists for — *"does this firmware understand key X?"* — the fast
path is that **JSON key paths are literal strings in `.rodata`**, and the code that parses them
references those strings. So:

1. `str` for the key. If it is absent, the firmware cannot be parsing it under that name.
2. `xref` to find which function reads it.
3. `fn` on that function to see what it does with the value, and what it demands alongside.

A key that exists in `.rodata` but is referenced only from a code path we cannot reach is a
false positive. Say so when you cannot tell rather than reporting the string as an answer.

## What this cannot tell you

- **The libraries are stripped.** Only *exported* (dynsym) names survive — for C++ that means
  mangled `_ZN3smp4util16getHdrTypeStringEi` and friends. Internal functions come out as
  `FUN_0005dd18`. Start from an export and walk inward; do not expect to grep for an internal
  name.
- **A string is not a code path.** Presence proves the parser knows the name, not that the
  value reaches the hardware.
- **Data tables are data.** `smp::util::getHdrTypeString(int)` decompiles to a red-black tree
  walk returning a pointer at `+0x14` — i.e. a `std::map` built at load time. The *contents*
  are not in the disassembly; recovering them means reading the initialiser or observing the
  function at runtime.

## Setup, and the two traps in it

```sh
brew install ghidra openjdk@21
```

- **Ghidra is a FORMULA, not a cask.** `brew install --cask ghidra` fails with *"No Cask with
  this name exists"*.
- **The JDK is required and macOS does not have one.** Ghidra is a Java application, headless
  analyzer included; `/usr/bin/java` on macOS is a stub that only offers to install Java, and
  `/usr/libexec/java_home -V` reports *"Unable to locate a Java Runtime"*. Homebrew installs
  `openjdk@21` **keg-only** (deliberately not on `PATH`), which is fine — `decomp.sh` finds it
  and exports `JAVA_HOME` itself.
- Scripts are **Java**, not Python: Ghidra 12 dropped Jython, and PyGhidra would mean requiring
  a Python environment. Headless compiles a `.java` script on the fly, so the driver writes the
  two it needs into the lab at run time.
- `radare2` (`brew install radare2`) is a fine quick alternative for a disassembly glance, but
  its C output on C++ binaries is much weaker than Ghidra's. Use it for a peek, Ghidra for an
  answer.

## Worked example — the one this skill was built for

*Question:* our app feeds coded access units to Starfish in `BUFFERSTREAM` mode and Dolby Vision
never engages. Kodi signals DV through Load-payload fields, but Kodi ships for webOS 5+ and
nobody had shown those keys work on 4.x. Does **our** firmware parse them?

```sh
$D str playerAPIs DolbyHdrInfo
#   option.externalStreamingInfo.contents.DolbyHdrInfo
$D str libpf-1.0 profileId
#   option.externalStreamingInfo.contents.DolbyHdrInfo.profileId
$D str libpf-1.0 ac3PlusInfo
#   option.externalStreamingInfo.contents.ac3PlusInfo
#   option.externalStreamingInfo.contents.ac3PlusInfo.channels
```

Answer in three commands: **yes**, at exactly those paths, on webOS 4.5 — the parser was there
all along and we simply never sent the fields.

Two further results from the same pass, both of which would have been expensive mistakes:

- The subkeys are **not** in `libplayerAPIs`; they are in **`libpf`**. Checking only the library
  whose API you call would have produced a confident "not supported".
- `mediaSei` / `mediaVui` are present, which are the **webOS-4 legacy** names for what webOS 5+
  calls `sei` / `vui`. Copying a modern client's payload verbatim would have sent keys this
  firmware ignores, and the failure would have been silent.

## House rules

- **Never modify anything on the device.** `pull` is read-only `scp`; the TV is a shared mutex
  (`tools/tv-lock.sh` / the `tv-lock` skill — harvesting takes no lock, but anything that RUNS the
  app does)
  and other work may be running against it. Wake it with `.agents/skills/wake-tv/wake-tv.sh`.
- Record what you harvested. `pull` writes `MANIFEST.txt` with a sha256 per file, so a finding
  can be tied to the exact binary it came from — firmware updates change these.
- **Findings from decompilation are for interoperability**, on hardware the owner owns. Keep
  recovered LG internals in the lab and in commit messages as *reasoning*; do not paste vendor
  code into the repository.
- Prefer a claim you can cite as `library + symbol/string + what the code does` over one that
  rests on a name looking right. `bind-tv-lib-abi` exists because a wrong offset is silent
  memory corruption on a device with no debugger, and this skill is how that bar gets met.
