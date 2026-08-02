#!/usr/bin/env python3
"""Build the .ipk — the `ar` container and both `.tar.gz` members — deterministically.

Why not `tar czf`: it embeds the developer's uid/gid/username (`gleblinnik/staff` was shipping
inside every .ipk), the current mtime, a gzip header timestamp, and readdir order. That makes the
sha256 in the manifest — which every user's TV verifies at install time — different on every run,
so you cannot tell a rebuild from a tampered artifact.

Why Python rather than tar flags: the flags differ irreconcilably between GNU tar (Linux CI:
--owner=/--group=/--sort=) and bsdtar (the dev Mac: --uid/--gid/--uname/--gname, no --sort).
tarfile gives both hosts the same bytes with no branching.

This module also SYNTHESISES the two descriptors an ipk must carry and a scp-based dev loop never
needed, both derived from `pkg/appinfo.json` so the version stays single-sourced:

  * `usr/palm/packages/<id>/packageinfo.json` — webOS's *package* descriptor, distinct from the
    *application* descriptor `appinfo.json`. `com.webos.appInstallService` reads it to learn which
    app ids a package owns; without it an install leaves nothing registered. It was absent from
    every ipk this repo ever built, undetected because the dev loop is `make deploy` (scp into an
    already-registered app dir), which never consults it. `webosbrew-ipk-verify` opens it first and
    reports the miss as `Failed to open <the ipk>: No such file or directory` — the ipk's own name,
    not the member's, so the error reads like a corrupt archive.
  * `Installed-Size` in the control file — opkg checks it against free space before unpacking.
    Verifiers ignore the literal `1234`, the dummy `ares-package` emits, so it must be real.

And it writes the `ar` container itself rather than shelling out to `ar`, because **GNU `ar` produces
an ipk LG's installer cannot read**. GNU format terminates every short member name with `/`, so the
members arrive as `debian-binary/`, `control.tar.gz/`, `data.tar.gz/`; `appinstalld` looks up the
exact names and fails the whole package with `error_code -5, "Failed to extract package"` before
anything is unpacked. `dpkg-deb` and `ares-package` both write the bare names, which is what the
format actually calls for — the trailing slash is a GNU archiver convention for disambiguating
names from its long-name table, not part of `ar`. 60 bytes of header per member is less machinery
than depending on any particular archiver's conventions, and it drops the cross-`ar` requirement
from packaging, so `make ipk` no longer needs the NDK.
"""
import gzip
import io
import json
import sys
import tarfile
from pathlib import Path

# Any fixed epoch works; 2010-01-01 is safely inside the range old opkg builds accept.
EPOCH = 1262304000


def add_tree(tf: tarfile.TarFile, src: Path, arc_root: str, skip: set = ()) -> None:
    """Add src's contents under arc_root, sorted, with all identity fields normalised."""
    entries = sorted(
        [p for p in src.rglob("*")],
        key=lambda p: p.relative_to(src).as_posix(),
    )
    for p in entries:
        rel = p.relative_to(src).as_posix()
        if rel in skip:
            continue
        ti = tf.gettarinfo(str(p), arcname=f"{arc_root}/{rel}" if arc_root else rel)
        ti.uid = ti.gid = 0
        ti.uname = ti.gname = "root"
        ti.mtime = EPOCH
        # Normalise mode: the binary and directories executable, everything else 0644. Otherwise
        # a stray local chmod changes the archive.
        ti.mode = 0o755 if (ti.isdir() or p.name == "plxnative") else 0o644
        if ti.isfile():
            with open(p, "rb") as fh:
                tf.addfile(ti, fh)
        else:
            tf.addfile(ti)


def write_targz(out: Path, src: Path, arc_root: str, extra: dict = None) -> None:
    """Tar+gzip src under arc_root. `extra` maps arcname -> bytes for members built in memory."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tf:
        add_tree(tf, src, arc_root, skip=set((extra or {}).keys()))
        for name, blob in sorted((extra or {}).items()):
            ti = tarfile.TarInfo(f"{arc_root}/{name}" if arc_root else name)
            ti.size, ti.mode, ti.mtime = len(blob), 0o644, EPOCH
            ti.uid = ti.gid = 0
            ti.uname = ti.gname = "root"
            tf.addfile(ti, io.BytesIO(blob))
    # mtime=0 and an empty filename keep the gzip HEADER deterministic too — `tar czf` writes
    # both, and they are outside the tar stream so they survive any tar-level normalisation.
    with open(out, "wb") as fh:
        with gzip.GzipFile(filename="", mode="wb", fileobj=fh, mtime=0, compresslevel=9) as gz:
            gz.write(raw.getvalue())


def write_packageinfo(data: Path, app: dict) -> Path:
    """Emit usr/palm/packages/<id>/packageinfo.json beside the application dir."""
    pkg_dir = data / "usr" / "palm" / "packages" / app["id"]
    pkg_dir.mkdir(parents=True, exist_ok=True)
    out = pkg_dir / "packageinfo.json"
    # Key order and 4-space indent match what `ares-package` emits, so a diff against a
    # stock-tooling package shows only the values.
    out.write_text(json.dumps({
        "app": app["id"],
        "id": app["id"],
        "loc_name": app["title"],
        "package_format_version": 2,
        "vendor": app["vendor"],
        "version": app["version"],
    }, indent=4) + "\n")
    return out


def control_with_size(ctl: Path, data: Path) -> tuple:
    """The control file's text with a real Installed-Size. Returns (text, size in KiB).

    Deliberately NOT written back to `ipkroot/ctl/control`: the size depends on the binary, so it
    differs between a dev and a RELEASE build (10117 vs 10120 KiB today). Rewriting the tracked file
    would make every `make ipk` dirty the worktree and invite someone to commit whichever value
    happened to be last — a number that is then wrong for the other configuration. The tracked file
    stays the source of the fields a human maintains; this one is assembled at package time.
    """
    kib = (sum(p.stat().st_size for p in data.rglob("*") if p.is_file()) + 1023) // 1024
    lines = [ln for ln in ctl.read_text().splitlines() if not ln.startswith("Installed-Size:")]
    # Debian orders Installed-Size after Architecture; opkg does not care, but a stock-shaped
    # control file is one less difference when someone diffs this against a reference package.
    at = next((i for i, ln in enumerate(lines) if ln.startswith("Architecture:")), len(lines) - 1)
    lines.insert(at + 1, f"Installed-Size: {kib}")
    return "\n".join(lines) + "\n", kib


def write_ar(out: Path, members: list) -> None:
    """Write a common-format `ar` archive of (name, bytes), names verbatim — no trailing slash."""
    with open(out, "wb") as fh:
        fh.write(b"!<arch>\n")
        for name, blob in members:
            assert len(name) <= 16, f"{name} needs the GNU long-name table this writer omits"
            # name[16] mtime[12] uid[6] gid[6] mode[8] size[10] magic[2]. Identity is zeroed and
            # the mtime fixed for the same reason the tar members are: the manifest sha256 the TV
            # verifies at install must not change between two builds of one commit.
            fh.write(f"{name:<16}{EPOCH:<12}{0:<6}{0:<6}{'100644':<8}{len(blob):<10}".encode())
            fh.write(b"\x60\n")
            fh.write(blob)
            if len(blob) % 2:
                fh.write(b"\n")   # members are 2-byte aligned


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    root = repo / "ipkroot"
    app = json.loads((repo / "pkg" / "appinfo.json").read_text())
    write_packageinfo(root / "data", app)
    control, kib = control_with_size(root / "ctl" / "control", root / "data")
    write_targz(root / "control.tar.gz", root / "ctl", "",
                extra={"control": control.encode()})
    write_targz(root / "data.tar.gz", root / "data", "")
    (root / "debian-binary").write_bytes(b"2.0\n")
    ipk = repo / "pkg" / f"{app['id']}_{app['version']}_arm.ipk"
    # debian-binary MUST come first; the other two are read by name.
    write_ar(ipk, [(n, (root / n).read_bytes())
                   for n in ("debian-binary", "control.tar.gz", "data.tar.gz")])
    print(f"wrote {ipk.name} ({ipk.stat().st_size} bytes) — packageinfo.json for {app['id']}, "
          f"Installed-Size {kib} KiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
