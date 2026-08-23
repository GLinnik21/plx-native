#!/usr/bin/env python3
"""Build the .ipk — the `ar` container and both `.tar.gz` members — deterministically.

Why not `tar czf`: it embeds the developer's uid/gid/username (`gleblinnik/staff` was shipping
inside every .ipk), the current mtime, a gzip header timestamp, and readdir order. That makes the
sha256 in the manifest — which every user's TV verifies at install time — different on every run,
so you cannot tell a rebuild from a tampered artifact.

Why Python rather than tar flags: the flags differ irreconcilably between GNU tar (Linux CI:
--owner=/--group=/--sort=) and bsdtar (the dev Mac: --uid/--gid/--uname/--gname, no --sort).
tarfile gives both hosts the same bytes with no branching.

WHICH INSTALL a package is for comes from `$FLAVOR` (see `ci/flavor.py`): unset or `stable` builds
the app users install, `debug` builds `com.beb.plxnative.debug`, which sits beside it on the same
television. Every id in the archive — the control `Package:`, `packageinfo.json`, the staged
`applications/<id>/` directory and the `.ipk` filename — comes from that one transform, and the
directory is additionally what the installed binary reads to learn which install it is
(`paths::app_id`). They are asserted equal here and again in `ci/check-package.py`.

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
  * `resources/<locale>/appinfo.json` — the LOCALIZED descriptors, staged from the tracked
    `pkg/resources/` tree. Same reason as the two above: the ipk is the only artifact that carries
    them, `make deploy` has no use for them (the launcher tile of a developer install is not what
    LG's QA looks at), and the Makefile's `APP_FILES` is a flat `cp … $(STAGE)/` that cannot
    express a two-level tree. `stage_resources` is where the FLAVOUR suffix is reapplied — see its
    docstring for why a flavoured package that skipped that step would lose its badge in every
    non-English locale.

WHY NOT `ares-package` (LG's own packager, which webosbrew's submission check asks for): it is NOT
REPRODUCIBLE. Measured with ares-cli 2.4.0 on this payload — two runs of byte-identical input give
two different sha256. That breaks the one integrity property this distribution has: nothing in the
chain is code-signed, so the manifest hash a user's TV verifies at install is only meaningful if a
third party can rebuild the package and get the same bytes, which the README tells them to do.
Everything else ares-package does, this file already does identically — the same bare `ar` member
names, the same `usr/palm/applications/<id>` + `usr/palm/packages/<id>/packageinfo.json` layout
(verified by diffing an ares-built package against ours) — and one thing it does better:
`Installed-Size` here is KiB, per Debian and what opkg expects, where ares-package writes BYTES.
The control file carries `webOS-Package-Format-Version` and `webOS-Packager-Version` so the
submission check's presence heuristic is satisfied honestly; the packager string names this file
rather than impersonating theirs.

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
import os
import shutil
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import flavor  # noqa: E402  — ci/flavor.py, the one descriptor transform

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


def stage_resources(repo: Path, data: Path, app: dict) -> list:
    """Stage `pkg/resources/<locale>/appinfo.json` into the app dir. Returns the locales staged.

    webOS reads a locale's app title and description from `resources/<locale>/appinfo.json` inside
    the application directory, merging it over the top-level descriptor when the television's
    language changes (LG, *App Localization*; webOS OSE, *appinfo.json*). Only `title` and
    `appDescription` belong in one — `ci/check-package.py` gates that, because a full copy would put
    the version and the id in a file `ci/bump-version.py` has never heard of.

    THE FLAVOUR SUFFIX IS REAPPLIED HERE, and it is the whole reason this is a transform rather than
    a copy. `flavor.appinfo_for` renames a flavoured install's tile `PlxNative debug` precisely so
    two tiles are not a coin flip; a localized descriptor carrying the bare tracked title would
    override that back to `PlxNative` the moment the set was switched to Korean — restoring the
    ambiguity in exactly the locale nobody developing this app is looking at.

    The suffix is read off the transform's OUTPUT rather than re-derived from `flavor.FLAVORS`,
    so there is still only one place that decides what a flavoured title looks like. If that rule
    ever stops being a suffix, the assertion below fails loudly instead of silently mislabelling.
    """
    src = repo / "pkg" / "resources"
    dest = data / "usr" / "palm" / "applications" / app["id"] / "resources"
    if dest.exists():
        # Idempotence: a locale deleted from the tree must not survive in a stale stage, and two
        # runs of this script must produce the same archive.
        shutil.rmtree(dest)
    if not src.is_dir():
        return []
    base_title = json.loads((repo / "pkg" / "appinfo.json").read_text())["title"]
    if not app["title"].startswith(base_title):
        raise SystemExit(f"flavour title {app['title']!r} is not {base_title!r} plus a suffix — "
                         "stage_resources can no longer reapply it; see ci/flavor.py")
    suffix = app["title"][len(base_title):]
    locales = []
    for loc_dir in sorted(p for p in src.iterdir() if p.is_dir()):
        f = loc_dir / "appinfo.json"
        if not f.is_file():
            continue
        loc = json.loads(f.read_text(encoding="utf-8"))
        if "title" not in loc:
            # Packaging runs BEFORE ci/check-package.py, so say what is wrong rather than dying in
            # a KeyError traceback that names neither the file nor the rule.
            raise SystemExit(f"{f} has no `title` — a localized appinfo.json carries exactly "
                             "`title` and `appDescription` (ci/check-package.py gates it)")
        out_dir = dest / loc_dir.name
        out_dir.mkdir(parents=True, exist_ok=True)
        # Rewritten rather than copied, so the staged bytes are this script's output on every host:
        # `ensure_ascii=False` keeps the translations readable in the archive, UTF-8 with no BOM is
        # LG's stated requirement for non-Latin text, and the key order is the input file's.
        body = json.dumps({**loc, "title": loc["title"] + suffix},
                          ensure_ascii=False, indent=2) + "\n"
        (out_dir / "appinfo.json").write_bytes(body.encode("utf-8"))
        locales.append(loc_dir.name)
    return locales


def control_with_size(ctl: Path, data: Path, flav: str) -> tuple:
    """The control file's text with a real Installed-Size. Returns (text, size in KiB).

    Deliberately NOT written back to `ipkroot/ctl/control`: the size depends on the binary, so it
    differs between a dev and a RELEASE build (10117 vs 10120 KiB today). Rewriting the tracked file
    would make every `make ipk` dirty the worktree and invite someone to commit whichever value
    happened to be last — a number that is then wrong for the other configuration. The tracked file
    stays the source of the fields a human maintains; this one is assembled at package time.
    """
    kib = (sum(p.stat().st_size for p in data.rglob("*") if p.is_file()) + 1023) // 1024
    # `Package:` is re-pointed here for exactly the same reason and by the same rule — a flavoured
    # package must declare its own id, and the tracked file must stay the stable one.
    lines = [ln for ln in flavor.control_for(ctl.read_text(), flav).splitlines()
             if not ln.startswith("Installed-Size:")]
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
    # WHICH INSTALL this package is for. `--emit-appinfo <flavour> <path>` writes just the derived
    # descriptor and stops — that is how `make deploy` gets the appinfo it scp's, through THIS
    # transform rather than a second writer, so the file on the television and the file in the .ipk
    # cannot say different things about which app they belong to.
    if sys.argv[1:2] == ["--emit-appinfo"]:
        flav, out = sys.argv[2], Path(sys.argv[3])
        out.parent.mkdir(parents=True, exist_ok=True)
        # Same shape `pkg/appinfo.json` is committed in, so a diff of the two shows only the values.
        out.write_text(json.dumps(flavor.appinfo_for(flav), indent=2) + "\n")
        print(f"wrote {out} — id={flavor.app_id(flav)}")
        return 0

    root = repo / "ipkroot"
    # The Makefile passes FLAVOR; a bare invocation packages the app users install, which is the
    # right default for anything that does not know about flavours (release.yml pins it anyway).
    flav = os.environ.get("FLAVOR", "stable")
    app = flavor.appinfo_for(flav)
    # The staged application directory's NAME is a fourth witness of the id, and the one the
    # installed app READS at runtime (`paths::app_id`). It is laid down by the Makefile rather than
    # here, so this is the one place that can see both at once — and a package whose directory and
    # descriptor disagree is one whose binary would identify as something its own appinfo denies.
    #
    # IT RUNS BEFORE ANYTHING IS WRITTEN, and that ordering is load-bearing rather than tidy:
    # `stage_resources` mkdirs `applications/<id>/resources/…`, so it would CREATE the very
    # directory this reads and turn "nothing was staged" into a package containing the localized
    # descriptors and nothing else — exit 0, no binary, no top-level appinfo. Checked at the end,
    # as it was until the resources tree existed, this guard could no longer see its own subject.
    staged = sorted((root / "data/usr/palm/applications").glob("*"))
    if [p.name for p in staged] != [app["id"]]:
        sys.exit(f"staged applications/{[p.name for p in staged]} does not match appinfo id {app['id']}")
    write_packageinfo(root / "data", app)
    # Before `control_with_size`, which sums the staged tree for `Installed-Size`.
    locales = stage_resources(repo, root / "data", app)
    control, kib = control_with_size(root / "ctl" / "control", root / "data", flav)
    write_targz(root / "control.tar.gz", root / "ctl", "",
                extra={"control": control.encode()})
    write_targz(root / "data.tar.gz", root / "data", "")
    (root / "debian-binary").write_bytes(b"2.0\n")
    ipk = repo / "pkg" / f"{app['id']}_{app['version']}_arm.ipk"
    # debian-binary MUST come first; the other two are read by name.
    write_ar(ipk, [(n, (root / n).read_bytes())
                   for n in ("debian-binary", "control.tar.gz", "data.tar.gz")])
    print(f"wrote {ipk.name} ({ipk.stat().st_size} bytes) — packageinfo.json for {app['id']}, "
          f"Installed-Size {kib} KiB, {len(locales)} localized appinfo "
          f"({' '.join(locales) or 'none'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
