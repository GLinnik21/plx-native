#!/usr/bin/env python3
"""Generate the machine-derived half of a release audit, from the artifact itself.

    ci/gen-release-audit.py --tag v0.5.0 --dist dist --write

WHY THIS EXISTS. `docs/release-notes/vX.Y.Z.md` is what a television owner reads, and it is prose
a person wrote. `docs/release-audits/vX.Y.Z.md` is what a reviewer, a contributor or we-in-a-year
read when the question is "what was actually in that package" — and prose is the wrong instrument
for that, because every measurable number a human has typed into a note in this project's history
has eventually been wrong (`docs/release-notes/README.md` records three). So the audit's evidence
half is not written. It is READ OUT OF THE .ipk that is about to be published, by this script.

Everything below is derived from bytes:

  * the ar container's members, in order, with their sizes;
  * the control file, the app descriptor and the package descriptor;
  * every payload file, with its size, mode, owner and sha256;
  * the binary's ELF class, machine and complete DT_NEEDED list (parsed here, in pure Python —
    no readelf, so this runs on the runner, on a Mac, and on a machine with no NDK);
  * the bundled FFmpeg libraries, their SONAMEs, and the configure invocation FFmpeg records
    inside libavutil — which is where the LGPL position is actually decided, and where v0.2.1's
    build-machine path was found;
  * the dev-trigger witnesses, counted rather than asserted, so "none" is a measurement;
  * host- and path-shaped strings in the shipped binary, as a FLOOR on what it can reach.

WHAT IT CANNOT DO, said here rather than discovered later. A string table is evidence of presence,
never of absence: a host the binary assembles at runtime from pieces does not appear, and a string
that appears may be a log message rather than a destination (the `strings` output of a release
build has always contained `/tmp/plxnative-url` for exactly that reason). The scan is reported as
what it is — a floor — and the audit's authored half is where a claim about the whole surface is
made and attributed. Anything needing a human, a television or a third party is authored: this
script never writes a sentence about device testing or compatibility, and refuses to guess one.

Stdlib only, and no network. Reads the artifacts it is pointed at and nothing else.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import re
import struct
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

BEGIN = "<!-- BEGIN GENERATED — ci/gen-release-audit.py. Do not edit by hand. -->"
END = "<!-- END GENERATED -->"

# Literals only a `devtriggers` build emits: `dev.rs`'s DIAG array is the one place the full
# trigger names are written out, and it is `#[cfg(feature = "devtriggers")]`. Several rather than
# one, because a single witness that gets renamed goes vacuous silently — which is exactly what
# happened to `check-package.py`'s first witness (`plxnative-autoplay`, which matched nothing in
# either configuration from the day it was written).
# MEASURED FROM BOTH SIDES, against the published v0.5.0 .ipk and a local dev build, because a
# witness that cannot fail is not evidence:
#
#     plxnative-noidle       release 0   dev 2
#     plxnative-remote       release 0   dev 2      the world-writable FIFO
#     plxnative-capture      release 0   dev 1      the TCP capture listener
#     plxnative-drawmask     release 0   dev 1
#     plxnative-heroground   release 0   dev 2
#
# `plxnative-overdraw` is NOT in this list and must not be added: it counted **1 in the release
# binary**, because `shaders/fs_hero.frag` names it in a COMMENT and the shader source is
# `include_str!`'d, so the literal ships in every configuration. That is the whole hazard of
# string-table evidence in one example — the string is there and the code is not.
DEV_WITNESSES = (
    b"plxnative-noidle",
    b"plxnative-remote",
    b"plxnative-capture",
    b"plxnative-drawmask",
    b"plxnative-heroground",
)

# A build machine's directory layout inside a shipped file. Same shape, and the same two
# calibration bugs designed around, as `ci/check-package.py` and `ci/verify-published.sh`: the
# match keeps its leading boundary character so plex.tv's own `/api/v2/home/users` is not read as
# `/home/users`, and it is applied per PATH rather than per line so that FFmpeg's single
# configure blob cannot have one allowed token vouch for a forbidden one beside it.
HOSTPATH = re.compile(rb"(?:^|[^A-Za-z0-9/_.-])(/(?:Users|home)/[A-Za-z0-9_./+-]+)")
ALLOWED_PATH = re.compile(rb"webos-ndk|^/home/runner/")

HOSTNAME = re.compile(rb"\b((?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.){1,4}(?:tv|com|org|net|io|dev))\b")
RUNTIME_PATH = re.compile(rb"(/(?:tmp|media|etc|var|proc|dev)/[A-Za-z0-9_.%/-]{2,60})")


# ---------------------------------------------------------------------------- ar / tar readers
def ar_members(blob: bytes) -> list[tuple[str, int, bytes]]:
    """The ar container's members in file order: (name, size, payload)."""
    if blob[:8] != b"!<arch>\n":
        raise SystemExit("not an ar archive — this is not an .ipk")
    out, off = [], 8
    while off + 60 <= len(blob):
        name = blob[off:off + 16].decode("latin-1").rstrip()
        size = int(blob[off + 48:off + 58].decode("latin-1").strip() or 0)
        out.append((name, size, blob[off + 60:off + 60 + size]))
        off += 60 + size + (size % 2)
    return out


def tar_files(gz: bytes) -> list[tuple[tarfile.TarInfo, bytes]]:
    with tarfile.open(fileobj=io.BytesIO(gzip.decompress(gz))) as t:
        return [(m, t.extractfile(m).read()) for m in t.getmembers() if m.isfile()]


# ---------------------------------------------------------------------------- ELF, in pure Python
def elf_info(blob: bytes) -> dict:
    """ELF class/machine/SONAME/DT_NEEDED, without readelf.

    The runner has binutils and a Mac does not, and the NDK's cross-readelf is only on a machine
    that can build this. An audit that can only be produced on one of the three is an audit
    nobody re-runs, so this is parsed here.
    """
    if blob[:4] != b"\x7fELF":
        return {}
    is64 = blob[4] == 2
    end = "<" if blob[5] == 1 else ">"
    machine, = struct.unpack_from(end + "H", blob, 18)
    if is64:
        phoff, = struct.unpack_from(end + "Q", blob, 32)
        phentsize, phnum = struct.unpack_from(end + "HH", blob, 54)
    else:
        phoff, = struct.unpack_from(end + "I", blob, 28)
        phentsize, phnum = struct.unpack_from(end + "HH", blob, 42)

    loads, dyn = [], None
    for i in range(phnum):
        o = phoff + i * phentsize
        p_type, = struct.unpack_from(end + "I", blob, o)
        if is64:
            p_offset, p_vaddr = struct.unpack_from(end + "QQ", blob, o + 8)
            p_filesz, = struct.unpack_from(end + "Q", blob, o + 32)
        else:
            p_offset, p_vaddr = struct.unpack_from(end + "II", blob, o + 4)
            p_filesz, = struct.unpack_from(end + "I", blob, o + 16)
        if p_type == 1:            # PT_LOAD
            loads.append((p_vaddr, p_offset, p_filesz))
        elif p_type == 2:          # PT_DYNAMIC
            dyn = (p_offset, p_filesz)

    def v2o(vaddr: int) -> int | None:
        for va, off, sz in loads:
            if va <= vaddr < va + sz:
                return off + (vaddr - va)
        return None

    needed_off, soname_off, strtab, strsz = [], None, None, 0
    if dyn:
        step = 16 if is64 else 8
        one = end + ("Q" if is64 else "I")
        for o in range(dyn[0], dyn[0] + dyn[1], step):
            tag, = struct.unpack_from(one, blob, o)
            val, = struct.unpack_from(one, blob, o + (8 if is64 else 4))
            if tag == 0:
                break
            if tag == 1:
                needed_off.append(val)
            elif tag == 14:
                soname_off = val
            elif tag == 5:
                strtab = v2o(val)
            elif tag == 10:
                strsz = val

    def s(at: int) -> str:
        if strtab is None:
            return "?"
        start = strtab + at
        stop = blob.find(b"\0", start)
        return blob[start:stop].decode("latin-1", "replace")

    return {
        "class": "ELF64" if is64 else "ELF32",
        "machine": {40: "ARM", 62: "x86-64", 183: "AArch64"}.get(machine, f"machine {machine}"),
        "soname": s(soname_off) if soname_off is not None else None,
        "needed": [s(n) for n in needed_off],
        "strtab_bytes": strsz,
    }


# ---------------------------------------------------------------------------- small helpers
def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def row(k: str, v: str) -> str:
    return f"| {k} | {v} |"


def table(rows: list[str], head: tuple[str, ...] = ("", "")) -> str:
    return "\n".join([f"| {' | '.join(head)} |", "|" + "|".join(["---"] * len(head)) + "|", *rows])


def human(n: int) -> str:
    return f"{n:,}".replace(",", " ")


def ffmpeg_configuration(blob: bytes) -> str | None:
    """FFmpeg records its whole configure invocation as one string inside libavutil."""
    m = re.search(rb"--prefix=[ -~]{20,4000}", blob)
    return m.group(0).decode("latin-1") if m else None


# Rust packs its string literals with no NUL between them, so a path match routinely carries the
# HEAD OF THE NEXT LITERAL — `/proc/self/maps` comes back as `/proc/self/mapsCouldn`. Three rules
# cut that back without inventing anything: stop at a lowercase-to-uppercase transition, stop after
# a recognised extension, and when one match is a prefix of another keep only the shorter. What
# survives is still a raw string-table match and is labelled as one.
_UPPER_RUN = re.compile(r"(?<=[a-z0-9])(?=[A-Z])")
_EXT = re.compile(r"^(.*\.(?:json|log|txt|ttf|png|so|md))")


def _trim(s: str) -> str:
    s = _UPPER_RUN.split(s)[0]
    m = _EXT.match(s)
    return m.group(1) if m else s


def scan_strings(blob: bytes, rx: re.Pattern, limit: int = 40, trim: bool = False) -> list[str]:
    seen = {m.decode("latin-1") for m in rx.findall(blob)}
    if trim:
        seen = {_trim(s) for s in seen}
        # Drop a longer match only when its tail is a run-on of the NEXT literal rather than a
        # real deeper path: `/dev/nullUnixListener` goes, `/media/developer/apps/...` stays.
        seen = {s for s in seen
                if not any(o != s and s.startswith(o) and "/" not in s[len(o):] for o in seen)}
    return sorted(seen)[:limit]


# ---------------------------------------------------------------------------- the generated block
def generate(args) -> str:
    dist = Path(args.dist)
    version = args.tag.lstrip("v")
    ipks = sorted(dist.glob("*.ipk"))
    if len(ipks) != 1:
        raise SystemExit(f"expected exactly one .ipk in {dist} (saw {[p.name for p in ipks]})")
    ipk = ipks[0]
    blob = ipk.read_bytes()
    ipk_sha = sha256(blob)

    members = ar_members(blob)
    by_name = {n: p for n, _, p in members}
    control_files = dict((Path(m.name).name, d) for m, d in tar_files(by_name["control.tar.gz"]))
    control = dict(
        line.split(": ", 1)
        for line in control_files["control"].decode().splitlines() if ": " in line
    )
    payload = tar_files(by_name["data.tar.gz"])
    files = {m.name.lstrip("./"): (m, d) for m, d in payload}

    app_dir = next((p.rsplit("/", 1)[0] for p in files if p.endswith("/appinfo.json")
                    and "/resources/" not in p), None)
    appinfo = json.loads(files[f"{app_dir}/appinfo.json"][1])
    app_id = appinfo["id"]
    binary = files.get(f"{app_dir}/plxnative", (None, b""))[1]

    manifest = None
    mf = dist / f"{app_id}.manifest.json"
    if mf.exists():
        manifest = json.loads(mf.read_text())
    shafile = (dist / "ipk.sha256").read_text().strip() if (dist / "ipk.sha256").exists() else ""

    out: list[str] = [BEGIN, ""]
    out.append(f"*Every value below was read out of `{ipk.name}` by `ci/gen-release-audit.py`. "
               f"Nothing in this block was typed by a person.*")
    out.append("")

    # ---- identity and provenance
    out.append("### Identity and provenance")
    out.append("")
    rows = [row("tag", f"`{args.tag}`")]
    if args.commit:
        rows.append(row("commit", f"`{args.commit}`"))
    if args.run_url:
        rows.append(row("workflow run", args.run_url))
    if args.published_at:
        rows.append(row("published", args.published_at))
    if args.uploader:
        rows.append(row("asset uploader", f"`{args.uploader}`"
                        + ("" if args.uploader == "github-actions[bot]"
                           else " — **not CI**: the build and verify gates did not run")))
    # NOT a constant: it contradicted the uploader row directly above it on the one release where
    # the answer is interesting. v0.2.1 was cut from a laptop, and a fixed "built by GitHub
    # Actions" line printed under `asset uploader: GLinnik21` is the audit telling two stories.
    by_ci = args.uploader in (None, "github-actions[bot]")
    rows.append(row("built by",
                    "GitHub Actions (`.github/workflows/release.yml`, job `build + verify`)" if by_ci
                    else f"**not GitHub Actions** — the assets were uploaded by `{args.uploader}`, "
                         "so the build and verify jobs did not produce them"))
    out += [table(rows, ("field", "value")), ""]

    # ---- the package
    out.append("### The package")
    out.append("")
    inst = control.get("Installed-Size")
    rows = [
        row("filename", f"`{ipk.name}`"),
        row("app id", f"`{app_id}`"),
        row("version", f"`{appinfo['version']}`" + ("" if appinfo["version"] == version
                                                    else f" — **disagrees with the tag** (`{version}`)")),
        row("type", f"`{appinfo['type']}`"),
        row("sha256", f"`{ipk_sha}`"),
        row("download size", f"{human(len(blob))} bytes"),
        row("installed size", f"{human(int(inst))} KiB (control `Installed-Size`)" if inst else "not declared"),
        row("ar members", ", ".join(f"`{n}` ({human(s)} B)" for n, s, _ in members)),
    ]
    out += [table(rows, ("field", "value")), ""]

    agree = []
    agree.append(("the artifact's own sha256", ipk_sha))
    if manifest:
        agree.append(("`" + mf.name + "` → `ipkHash.sha256`", manifest["ipkHash"]["sha256"]))
    if shafile:
        agree.append(("`ipk.sha256`", shafile.split()[0]))
    same = len({v for _, v in agree}) == 1
    out.append(f"**Hash agreement:** {'all ' + str(len(agree)) + ' copies agree' if same else 'MISMATCH'}.")
    out.append("")
    out += [table([row(k, f"`{v}`") for k, v in agree], ("source", "sha256")), ""]
    if shafile:
        bare = not shafile.split()[-1].startswith("pkg/")
        out.append(f"`ipk.sha256` names `{shafile.split()[-1]}`, so `shasum -a 256 -c ipk.sha256` "
                   f"{'works' if bare else '**fails**'} in the directory a user downloads into.")
        out.append("")
    if manifest:
        ok = manifest["ipkSize"] == len(blob)
        out.append(f"The Homebrew Channel manifest declares `ipkSize` {human(manifest['ipkSize'])} bytes "
                   f"and `installedSize` {human(manifest['installedSize'])} KiB; the size it declares "
                   f"{'matches' if ok else '**does not match**'} the artifact. `rootRequired` is "
                   f"`{json.dumps(manifest.get('rootRequired'))}`.")
        out.append("")

    # ---- control provenance
    out.append("### Control file")
    out.append("")
    out += [table([row(f"`{k}`", v.replace("|", "\\|")) for k, v in sorted(control.items())],
                  ("field", "value")), ""]

    # ---- build configuration
    out.append("### Build configuration")
    out.append("")
    dev_hits = {w.decode(): binary.count(w) for w in DEV_WITNESSES}
    is_release = sum(dev_hits.values()) == 0
    rows = [
        row("flavour", f"`{app_id}` — "
            + ("the stable id, which is what users install"
               if app_id == "com.beb.plxnative" else "**not the stable id**")),
        row("cargo features", f"`{args.build_config}`" if args.build_config
            else "not recorded in the assets — the dev-trigger row below is the same property, "
                 "measured on the bytes"),
        row("dev-trigger surface",
            ("absent — " if is_release else "**PRESENT** — ")
            + ", ".join(f"`{k}`×{v}" for k, v in dev_hits.items())),
    ]
    out += [table(rows, ("field", "value")), ""]
    out.append("The witnesses are literals from `dev.rs`'s `DIAG` array, which is "
               "`#[cfg(feature = \"devtriggers\")]`; two of them name the surfaces a reviewer "
               "reading this repository's source would rightly ask about — the world-writable "
               "`plxnative-remote` FIFO that can drive the UI, and the unauthenticated TCP capture "
               "listener. Zero of all five is what `RELEASE=1` looks like in the bytes, and it is a "
               "property of this package rather than of the command line that produced it. Each "
               "witness has been measured from both sides (0 here, 1-2 in a dev build), because a "
               "witness that cannot fail is not evidence.")
    out.append("")

    # ---- package surface
    out.append("### Package surface")
    out.append("")
    perms = appinfo.get("requiredPermissions")
    rows = [
        row("declared permissions", "none — `appinfo.json` declares no `requiredPermissions`"
            if not perms else f"**{perms}**"),
        row("`requiredMemory`", f"{appinfo.get('requiredMemory', 'not declared')} MB"),
        row("`id` prefix", "not one of LG's reserved prefixes"
            if not app_id.startswith(("com.palm", "com.webos", "com.lge", "com.palmdts"))
            else "**reserved prefix**"),
        row("localized descriptors",
            f"{len([p for p in files if p.endswith('/appinfo.json') and '/resources/' in p])} locales"),
    ]
    out += [table(rows, ("field", "value")), ""]

    hosts = scan_strings(binary, HOSTNAME)
    paths = scan_strings(binary, RUNTIME_PATH, trim=True)
    out.append("**Host-shaped strings in the shipped binary.** A floor, not a proof: a string here "
               "may be a log message rather than a destination, and a host assembled at runtime "
               "would not appear at all.")
    out.append("")
    out.append("```")
    out += hosts or ["(none)"]
    out.append("```")
    out.append("")
    out.append("**Absolute paths outside the app's own directory, same caveat.**")
    out.append("")
    out.append("```")
    out += paths or ["(none)"]
    out.append("```")
    out.append("")

    # ---- build-machine paths
    dirty = []
    for name, (m, data) in sorted(files.items()):
        bad = sorted({h for h in HOSTPATH.findall(data) if not ALLOWED_PATH.search(h)})
        if bad:
            dirty.append((name, bad[0].decode("latin-1", "replace")))
    out.append("### Reproducibility evidence")
    out.append("")
    if dirty:
        out.append("**A build machine's directory layout is inside this package**, so a rebuild "
                   "cannot produce this hash and a reader cannot tell a different build directory "
                   "from tampering.")
        out.append("")
        out += [table([row(f"`{n}`", f"`{p}`") for n, p in dirty], ("payload file", "path found")), ""]
    else:
        out.append("No payload file carries a build machine's directory layout "
                   f"(all {len(files)} files scanned; the NDK's own `--cross-prefix`, which FFmpeg "
                   "records and which is identical on every runner, is the one allowed exception). "
                   "That is what makes two builds of this commit on one machine byte-identical. It "
                   "is not yet a cross-machine claim.")
        out.append("")

    # ---- payload inventory
    out.append("### Payload inventory")
    out.append("")
    rows = []
    for name, (m, data) in sorted(files.items()):
        rows.append("| `" + name + f"` | {human(m.size)} | `{oct(m.mode)[2:]:0>4}` | "
                    f"{m.uname or 'root'}:{m.gname or 'root'} | `{sha256(data)[:16]}` |")
    out += [table(rows, ("path", "bytes", "mode", "owner", "sha256 (first 16)")), ""]

    # ---- linkage
    elf = elf_info(binary)
    out.append("### Linkage")
    out.append("")
    if elf:
        out.append(f"`plxnative` is {elf['class']} / {elf['machine']}, with "
                   f"**{len(elf['needed'])} `DT_NEEDED` entries**:")
        out.append("")
        out.append("```")
        out += elf["needed"]
        out.append("```")
        out.append("")
        expected = (ROOT / "ci/expected-dt-needed.txt")
        if expected.exists():
            want = [l.strip() for l in expected.read_text().splitlines() if l.strip()]
            drift = sorted(set(elf["needed"]) ^ set(want))
            out.append(f"`ci/check-elf.sh` asserts this list against `ci/expected-dt-needed.txt` on "
                       f"every build: {'they agree' if not drift else '**they differ: ' + ', '.join(drift) + '**'}. "
                       "Two library families are deliberately absent because their SONAME moves "
                       "between firmwares and a `DT_NEEDED` entry cannot express \"either of these\" "
                       "— libcurl and libAcbAPI are `dlopen`ed by candidate list instead "
                       "(`rust-modules/src/dynlib.rs`, `src/starfish.c`).")
            out.append("")
    sos = {n: d for n, (m, d) in files.items() if re.search(r"\.so(\.\d+)*$", n)}
    if sos:
        rows = []
        for name, data in sorted(sos.items()):
            e = elf_info(data)
            rows.append(row(f"`{name.rsplit('/', 1)[-1]}`",
                            f"SONAME `{e.get('soname') or '?'}`, {human(len(data))} bytes, "
                            f"sha256 `{sha256(data)[:16]}`"))
        out.append("Shared libraries shipped **beside** the binary and opened by absolute path out "
                   "of the app's own directory, so they can neither shadow nor be shadowed by the "
                   "television's own FFmpeg:")
        out.append("")
        out += [table(rows, ("file", "identity")), ""]

    # ---- FFmpeg + licence
    out.append("### FFmpeg and the LGPL position")
    out.append("")
    cfg = next((ffmpeg_configuration(d) for n, d in sorted(sos.items()) if "avutil" in n), None)
    tarballs = sorted(dist.glob("ffmpeg-*.tar.xz"))
    rows = []
    if tarballs:
        tb = tarballs[0]
        rows.append(row("corresponding source",
                        f"`{tb.name}` attached to this release, sha256 `{sha256(tb.read_bytes())}`"))
    script = dist / "build-ffmpeg.sh"
    if script.exists():
        text = script.read_text()
        pinned = re.search(r"^SHA256=([0-9a-f]{64})", text, re.M)
        code = "\n".join(l for l in text.splitlines() if not l.strip().startswith("#"))
        banned = sorted({f for f in ("--enable-gpl", "--enable-nonfree", "--enable-version3")
                         if f in code})
        rows.append(row("build script", f"`{script.name}` attached, sha256 `{sha256(script.read_bytes())[:16]}`"))
        if pinned:
            rows.append(row("upstream tarball pinned in the script", f"`{pinned.group(1)}`"
                            + ("" if not tarballs or pinned.group(1) == sha256(tarballs[0].read_bytes())
                               else " — **does not match the attached tarball**")))
        rows.append(row("copyleft-widening flags",
                        "none of `--enable-gpl`, `--enable-nonfree`, `--enable-version3` "
                        "(comments excluded from the scan)" if not banned
                        else f"**{', '.join(banned)} present**"))
    lic = sorted(n.rsplit("/", 1)[-1] for n in files if "/licenses/" in n)
    rows.append(row("licence texts in the payload", ", ".join(f"`{n}`" for n in lic) or "**none**"))
    for doc in ("THIRD-PARTY-NOTICES.md", "LICENSE", "TRADEMARKS.md"):
        rows.append(row(f"`{doc}` in the payload",
                        "yes" if any(n.endswith("/" + doc) for n in files) else "**no**"))
    out += [table(rows, ("field", "value")), ""]
    if cfg:
        out.append("The configure invocation FFmpeg records inside `libavutil`, verbatim — this is "
                   "the primary evidence for both the licence position and the reproducibility "
                   "scan above:")
        out.append("")
        out.append("```")
        out.append(cfg)
        out.append("```")
        out.append("")

    # ---- gates
    out.append("### Verification gates")
    out.append("")
    if args.gates:
        out.append(Path(args.gates).read_text().strip())
    else:
        out.append("*(not supplied — pass `--gates FILE` with the run's gate verdicts)*")
    out.append("")

    # ---- static firmware compatibility
    out.append("### Static firmware compatibility")
    out.append("")
    out.append("This grades whether the process STARTS: it resolves the binary's libraries and "
               "undefined symbols against webosbrew's firmware inventories. It says nothing about "
               "whether video plays. Playback evidence is in the authored half above.")
    out.append("")
    if args.fwcompat:
        out.append("```")
        out.append(Path(args.fwcompat).read_text().strip())
        out.append("```")
    else:
        out.append("*(not supplied — pass `--fwcompat FILE` with `webosbrew-ipk-verify --details "
                   "--format markdown` output, or `tools/fwcompat.py`'s matrix)*")
    out.append("")

    # ---- published verification
    if args.published:
        out.append("### What the public can download")
        out.append("")
        out.append("`ci/verify-published.sh` re-derives every claim from the **published** assets "
                   "after the release exists — the hash in every place it appears, `shasum -c` "
                   "working where a user stands, CI rather than a person as the uploader, and no "
                   "payload file carrying a build machine's paths.")
        out.append("")
        out.append("```")
        out.append(Path(args.published).read_text().strip())
        out.append("```")
        out.append("")

    out.append(END)
    return "\n".join(out).rstrip() + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--tag", required=True, help="vX.Y.Z")
    ap.add_argument("--dist", default="dist", help="directory holding the built release assets")
    ap.add_argument("--commit")
    ap.add_argument("--run-url")
    ap.add_argument("--uploader")
    ap.add_argument("--published-at")
    ap.add_argument("--build-config", help="the contents of pkg/.build-config")
    ap.add_argument("--gates", help="file holding the run's gate verdicts (markdown)")
    ap.add_argument("--fwcompat", help="file holding webosbrew-ipk-verify / fwcompat.py output")
    ap.add_argument("--published", help="file holding ci/verify-published.sh output")
    ap.add_argument("--out", help="write the generated block here instead of stdout")
    ap.add_argument("--write", action="store_true",
                    help="splice into docs/release-audits/<tag>.md between its markers")
    args = ap.parse_args()

    block = generate(args)

    if args.write:
        target = ROOT / f"docs/release-audits/{args.tag}.md"
        if not target.exists():
            print(f"::error::{target.relative_to(ROOT)} does not exist — the authored half of an "
                  f"audit is written and reviewed BEFORE the release, from "
                  f"docs/release-audits/TEMPLATE.md", file=sys.stderr)
            return 1
        text = target.read_text()
        if BEGIN not in text or END not in text:
            print(f"::error::{target.relative_to(ROOT)} has no generated block "
                  f"— it must carry the BEGIN/END markers from the template", file=sys.stderr)
            return 1
        head, rest = text.split(BEGIN, 1)
        _, tail = rest.split(END, 1)
        target.write_text(head + block.rstrip("\n") + tail)
        print(f"wrote the generated block into {target.relative_to(ROOT)}")
    elif args.out:
        Path(args.out).write_text(block)
        print(f"wrote {args.out}")
    else:
        sys.stdout.write(block)
    return 0


if __name__ == "__main__":
    sys.exit(main())
