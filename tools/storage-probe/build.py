#!/usr/bin/env python3
"""Build an isolated, network-free storage diagnostic; never builds the Plex app."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "ci"))
from mkipk import write_ar  # common-format ar names required by LG's installer

APP_ID = "com.beb.plxnative.storageprobe"
EPOCH = 1262304000


def archive(entries):
    """Entries are explicit (path, bytes-or-None, mode, gid), never a tree walk."""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as tar:
        for name, data, mode, gid in sorted(entries):
            assert not name.startswith("/") and ".." not in Path(name).parts
            member = tarfile.TarInfo(name)
            member.uid, member.gid, member.mode, member.mtime = 0, gid, mode, EPOCH
            # Leave names empty so a local passwd/group name cannot override numeric IDs.
            member.uname = member.gname = ""
            if data is None:
                member.type = tarfile.DIRTYPE
                tar.addfile(member)
            else:
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data))
    return gzip.compress(raw.getvalue(), mtime=0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="new output directory")
    parser.add_argument("--sdk", type=Path, default=Path(os.environ.get(
        "WEBOS_SDK", Path.home() / "webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot")))
    parser.add_argument("--version", default="1.0.0")
    args = parser.parse_args()
    if not re.fullmatch(r"1\.0\.[0-9]+", args.version):
        parser.error("diagnostic version must be 1.0.N (not the PlxNative version)")
    args.out.mkdir(parents=True, exist_ok=False)
    executable = args.out / "storageprobe"
    sysroot = args.sdk / "arm-webos-linux-gnueabi/sysroot"
    # SDL_ttf's quoted SDL.h include would otherwise select the newer NDK SDL
    # beside it, mixing two header generations. Use the real ttf header in an
    # overlay, with every SDL include resolved against the TV's existing headers.
    overlay = args.out / "headers/SDL2"
    overlay.mkdir(parents=True)
    shutil.copyfile(sysroot / "usr/include/SDL2/SDL_ttf.h", overlay / "SDL_ttf.h")
    subprocess.run([
        str(args.sdk / "bin/arm-webos-linux-gnueabi-gcc"), f"--sysroot={sysroot}",
        "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fmax-errors=5", "-fno-ident",
        "-I" + str(overlay.parent), "-I" + str(REPO / "include/SDL2"),
        "-I" + str(REPO / "include"),
        "-Wl,--build-id=sha1", "-Wl,-z,relro,-z,now",
        str(REPO / "tools/storage-probe/probe.c"), "-o", str(executable),
        "-lSDL2", "-lSDL2_ttf",
    ], check=True)
    app = {
        "id": APP_ID, "version": args.version, "vendor": "beb",
        "type": "native", "main": "storageprobe", "title": "Storage probe (test)",
        "appDescription": "Issue 76 diagnostic. No Plex account or network access.",
        "icon": "icon.png", "nativeLifeCycleInterfaceVersion": 2,
        "handlesRelaunch": False, "requiredMemory": 64,
    }
    package = {"app": APP_ID, "id": APP_ID, "loc_name": app["title"],
               "package_format_version": 2, "vendor": "beb", "version": args.version}
    appdir = f"usr/palm/applications/{APP_ID}"
    pkgdir = f"usr/palm/packages/{APP_ID}"
    directories = ["usr", "usr/palm", "usr/palm/applications", appdir,
                   "usr/palm/packages", pkgdir]
    entries = [(name, None, 0o755, 0) for name in directories]
    for name, mode, gid in [("test0755", 0o755, 0), ("test0775", 0o775, 5000),
                            ("test0777", 0o777, 0)]:
        entries.append((f"{appdir}/{name}", None, mode, gid))
    payload = {
        "storageprobe": executable.read_bytes(),
        "appinfo.json": (json.dumps(app, indent=2) + "\n").encode(),
        "appfont.ttf": (REPO / "pkg/appfont.ttf").read_bytes(),
        "icon.png": (REPO / "pkg/dev/icon.png").read_bytes(),
        "OFL.txt": (REPO / "pkg/OFL.txt").read_bytes(),
        "LICENSE": (REPO / "LICENSE").read_bytes(),
        "README.txt": (REPO / "tools/storage-probe/README.md").read_bytes(),
    }
    for name, data in payload.items():
        entries.append((f"{appdir}/{name}", data, 0o755 if name == "storageprobe" else 0o644, 0))
    entries.append((f"{pkgdir}/packageinfo.json", json.dumps(package).encode(), 0o644, 0))
    control = (
        f"Package: {APP_ID}\nVersion: {args.version}\nArchitecture: arm\n"
        f"Installed-Size: {(sum(len(e[1]) for e in entries if e[1] is not None) + 1023) // 1024}\n"
        "Maintainer: beb\nDescription: Isolated storage diagnostic for issue 76\n"
        "Section: misc\nPriority: optional\nwebOS-Package-Format-Version: 2\n"
        "webOS-Packager-Version: plxnative-storage-probe\n"
    ).encode()
    data = archive(entries)
    # Check the bytes, not the staging filesystem's umask.
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
        for name, _, mode, gid in entries:
            actual = tar.getmember(name)
            assert actual.mode == mode and actual.uid == 0 and actual.gid == gid
        assert tar.getmember(f"{appdir}/test0777").isdir()
        assert all("probe.dat" not in name for name in tar.getnames())
        assert json.load(tar.extractfile(f"{appdir}/appinfo.json"))["id"] == APP_ID
    ipk = args.out / f"{APP_ID}_{args.version}_arm.ipk"
    write_ar(ipk, [("debian-binary", b"2.0\n"),
                   ("control.tar.gz", archive([("control", control, 0o644, 0)])),
                   ("data.tar.gz", data)])
    digest = hashlib.sha256(ipk.read_bytes()).hexdigest()
    (args.out / "SHA256SUMS").write_text(f"{digest}  {ipk.name}\n")
    (args.out / "README.md").write_bytes(payload["README.txt"])
    print(f"{ipk}\nSHA256 {digest}\nNo PlxNative app files or credentials were packaged.")


if __name__ == "__main__":
    main()
