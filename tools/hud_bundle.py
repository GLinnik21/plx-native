#!/usr/bin/env python3
"""Package the Linux HUD authoring simulator, its native libraries and trusted fixture tools.

Run after `HOST=1 ci/build-ffmpeg.sh` and `make site-video-sim`. Mesa's driver and Xvfb remain
system dependencies; fonts, the app, SDL/TTF, FFmpeg, schema and corpus are bound by bundle.json.
Only explicit tracked inputs are included. No developer configuration or runtime state is read.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

from hud_package import native_json

ROOT = Path(__file__).resolve().parents[1]


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def native_dependencies(inputs):
    """Resolve the loader's actual transitive libraries, preserving its SONAME basenames."""
    found = {}
    # These libraries deliberately have no build-time rpath. Resolve their sibling SONAMEs
    # using only known build inputs, not a developer's inherited loader override.
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = ":".join(sorted({str(path.parent.resolve()) for path in inputs}))
    for path in inputs:
        report = subprocess.run(["ldd", str(path)], capture_output=True, text=True, check=True, env=env).stdout
        if "not found" in report:
            raise ValueError(f"unresolved native dependency of {path.name}")
        for name, absolute in re.findall(r"^\s*(\S+) => (/\S+)", report, re.M):
            # The host glibc/loader must agree with the runner. GL drivers are selected at runtime
            # by the installed Mesa stack, not copied from a build VM's graphics device.
            if name.startswith(("libc.so", "libm.so", "libpthread.so", "librt.so", "libdl.so")):
                continue
            if name in found and found[name].resolve() != Path(absolute).resolve():
                raise ValueError(f"two different native libraries have SONAME {name}")
            found[name] = Path(absolute)
    return found


def tracked():
    return subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")[:-1]



def prepare_authoring(stage, inputs, binary, env, info):
    """Bind both native schemas and a document-aware source/archive fixture corpus."""
    if (info.get("protocol") != 1 or not info.get("supported_formats")
        or set(info.get("schemas", {})) != {"manifest", "hud"} or not info.get("limits")):
        raise ValueError("native SDK must report bundle formats, schemas and profile limits")
    for kind, versions in info["schemas"].items():
        for version, identity in versions.items():
            schema = native_json(binary, "--hud-schema", kind, "--version", version, env=env)
            mirror = ROOT / "site/hud/schema" / kind / (version + ".json")
            if schema.get("$id") != identity or json.loads(mirror.read_text()) != schema:
                raise ValueError("native " + kind + " schema differs from its committed mirror/identity")
            output = stage / "schema" / kind / (version + ".json")
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(json.dumps(schema, ensure_ascii=False, indent=2) + "\n")
    prefixes = {
        "rust-modules/appkit/src/hud_format/fixtures/": "valid",
        "site/hud/examples/invalid/": "invalid",
    }
    selected = {}
    for name in inputs:
        for prefix, category in prefixes.items():
            if not name.startswith(prefix):
                continue
            relative = Path(name.removeprefix(prefix))
            if len(relative.parts) < 2 or relative.parts[0].startswith("."):
                continue
            key = category, relative.parts[0]
            selected.setdefault(key, []).append((name, relative))
    cases = []
    for (category, name), files in sorted(selected.items()):
        if not any(relative.name == "manifest.json" and len(relative.parts) == 2 for _, relative in files):
            continue
        destination = stage / "corpus" / category
        for original, relative in files:
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / original, target)
        source = destination / name
        metadata = json.loads((source / "manifest.json").read_text())
        entry = metadata.get("hud", {}).get("entry", "hud.json")
        # Corpus members are trusted, committed app fixtures; keep the index relative and portable.
        if Path(entry).is_absolute() or ".." in Path(entry).parts:
            raise ValueError("invalid app corpus document path")
        relative_source = category + "/" + name
        documents = [{"kind": "manifest", "version": "1.0", "file": "manifest.json",
                      "path": relative_source + "/manifest.json"},
                     {"kind": "hud", "version": "1.0", "file": entry,
                      "path": relative_source + "/" + entry}]
        cases.append({"input": relative_source, "documents": documents, "invalid": category == "invalid"})
    valid = [case for case in cases if not case["invalid"]]
    if not valid or not any(case["invalid"] for case in cases):
        raise ValueError("app corpus must include valid sources and invalid document fixtures")
    source = stage / "corpus" / next((case["input"] for case in valid if case["input"].endswith("/minimal")), valid[0]["input"])
    canonical = stage / "corpus" / "canonical.plxhud"
    report = native_json(binary, "--hud-pack", source, "--out", canonical, "--format", "json", env=env)
    content = canonical.read_bytes()
    if (report.get("protocol") != 1 or report.get("findings") or report.get("bytes") != len(content)
        or report.get("package_sha256") != hashlib.sha256(content).hexdigest()):
        raise ValueError("corpus packer did not identify its exact canonical archive")
    cases.append({"input": "canonical.plxhud", "documents": [], "invalid": False})
    # The valid archive uses the same JSON documents as its canonical source. This also lets stock
    # schema agreement distinguish malformed containers (which intentionally have no documents).
    cases[-1]["documents"] = next(case["documents"] for case in valid if stage / "corpus" / case["input"] == source)
    malformed = {"trailing": content + b"x", "truncated": content[:-1]}
    if len(content) > 30:
        import struct
        changed = bytearray(content)
        length = struct.unpack_from("<H", content, 26)[0]
        changed[30 + length] ^= 1
        malformed["crc"] = bytes(changed)
        changed = bytearray(content)
        struct.pack_into("<H", changed, len(changed)-12, 65535)
        malformed["entry-count"] = bytes(changed)
    for name, content in malformed.items():
        relative = "archives/" + name + ".plxhud"
        target = stage / "corpus" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        cases.append({"input": relative, "documents": [], "invalid": True})
    (stage / "corpus/manifest.json").write_text(json.dumps({"protocol": 1, "cases": cases}, indent=2) + "\n")


def pin_contract(manifest):
    """Carry the native SDK contract unchanged into the external artifact pin."""
    return {key: manifest[key] for key in ("protocol", "app_version", "source_sha",
            "supported_formats", "schemas", "limits", "development")}


def pack(binary: Path, destination: Path):
    if subprocess.check_output(["git", "diff", "HEAD", "--name-only"], cwd=ROOT):
        raise ValueError("commit the app tree before producing a pinned simulator")
    source = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    epoch = int(subprocess.check_output(["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT))
    with tempfile.TemporaryDirectory(prefix="hud-bundle-") as temp:
        stage = Path(temp)
        def copy(path, target):
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target, follow_symlinks=True)
            target.chmod(0o755 if os.access(path, os.X_OK) else 0o644)
        copy(binary, stage / "bin/plxnative-sim.real")
        wrapper = stage / "bin/plxnative-sim"
        wrapper.write_text('#!/bin/sh\nset -eu\nbundle=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)\nexport LD_LIBRARY_PATH="$bundle/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nexport PLXNATIVE_APP_DIR="${PLXNATIVE_APP_DIR:-$bundle/pkg}"\nexec "$bundle/bin/plxnative-sim.real" "$@"\n')
        wrapper.chmod(0o755)
        native_text = ROOT / "pkg/libass-plx-host.so.0"
        if not native_text.is_file():
            raise ValueError("build libass-host before packaging")
        copy(native_text, stage / "pkg" / native_text.name)
        inputs = tracked()
        for name in inputs:
            if (name in {"LICENSE", "LICENSING.md", "THIRD-PARTY-NOTICES.md", "docs/screenshots/CREDITS.md", "tools/screenshots.py", "tools/demo_library.py",
                         "tools/hud_preview.py", "tools/hud_package.py", "tests/mock_pms.py", "tests/link_conditioner.py"}
                or name.startswith(("tests/demo_library/", "licenses/"))
                or name.startswith("pkg/") and (name.endswith(".ttf") or name in {"pkg/OFL.txt", "pkg/le-roots.pem", "pkg/fonts/subs/manifest.json"})):
                copy(ROOT / name, stage / name)
        ffmpeg = list((ROOT / "vendor/ffmpeg-prefix-host/lib").glob("lib*-plx.so.[0-9]*"))
        ffmpeg = {p.name: p for p in ffmpeg if re.fullmatch(r"lib.+-plx\.so\.\d+", p.name)}
        if len(ffmpeg) < 4:
            raise ValueError("build the Linux host FFmpeg before packaging")
        for name, path in ffmpeg.items():
            copy(path, stage / "pkg" / name)
        # SDL/TTF are opened dynamically, so the executable's ldd does not enumerate them.
        inventory = subprocess.check_output(["ldconfig", "-p"], text=True)
        direct = []
        for name in ["libSDL2-2.0.so.0", "libSDL2_ttf-2.0.so.0", "libGL.so.1", "libcurl.so.4"]:
            matches = re.findall(r"\s" + re.escape(name) + r"\s+[^\n]*=> (/\S+)", inventory)
            if not matches:
                raise ValueError(f"required host library is missing: {name}")
            path = Path(matches[0]); direct.append(path)
            copy(path, stage / "lib" / name)
        dependencies = native_dependencies([binary, native_text, *direct, *ffmpeg.values()])
        for name, path in dependencies.items():
            copy(path, stage / "lib" / name)
        # Preserve the distribution's copyright/license notices for every copied system library.
        # The app, facade and FFmpeg source/licensing are identified by the repository notices.
        system_packages = {}
        for path in set(direct + list(dependencies.values())):
            if str(path.resolve()).startswith(str(ROOT)):
                continue
            query = subprocess.run(["dpkg-query", "-S", str(path)],capture_output=True,text=True)
            if query.returncode:
                query = subprocess.run(["dpkg-query", "-S", "*/" + path.name],capture_output=True,text=True,check=True)
            for line in query.stdout.splitlines():
                package = line.split(": ",1)[0]
                copyright = Path("/usr/share/doc") / package.split(":",1)[0] / "copyright"
                if not copyright.is_file():
                    raise ValueError("missing system-library copyright: " + package)
                copy(copyright, stage / "licenses/system" / (package.replace(":","_") + ".txt"))
                version = subprocess.check_output(["dpkg-query","-W","-f=${Version}",package],text=True)
                system_packages[package] = version
        (stage / "licenses/system-packages.json").write_text(json.dumps(system_packages,sort_keys=True,indent=2)+"\n")
        for path in [stage / "bin/plxnative-sim.real", *(stage / "lib").iterdir(), *(stage / "pkg").glob("*.so.*")]:
            relative = "$ORIGIN/../lib" if path.parent.name in {"bin", "pkg"} else "$ORIGIN"
            subprocess.run(["patchelf", "--set-rpath", relative, str(path)], check=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith("PLXNATIVE_")}
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        info = json.loads(subprocess.check_output([str(wrapper), "--hud-info"], env=env, text=True))
        if not info["source_sha"] or not source.startswith(info["source_sha"]):
            raise ValueError("binary does not report the source being packaged")
        # Both app text shaping and native ASS use this dlopen-only library. Prove that the
        # relocated tree can load its exports instead of trusting ldd's view of the executable.
        subprocess.run(["python3", "-c", "import ctypes,sys; lib=ctypes.CDLL(sys.argv[1]); assert lib.plx_ass_abi_version() == 4; assert lib.plx_ass_create; assert lib.plx_ass_text_measure", str(stage / "pkg" / native_text.name)],
                       env={**env,"LD_LIBRARY_PATH":str(stage / "lib")},check=True)
        prepare_authoring(stage, inputs, wrapper, env, info)
        metrics = subprocess.check_output([str(wrapper), "--hud-text-metrics"], env=env)
        (stage / "text-metrics.json").write_bytes(metrics)
        version = info["app_version"]
        manifest = {"protocol": 1, "app_version": version.removesuffix("-dev"), "reported_version": version,
                    "development": version.endswith("-dev"), "source_sha": source, "supported_formats": info["supported_formats"],
                    "schemas": info["schemas"], "limits": info["limits"],
                    "languages": info["languages"], "font_hashes": json.loads(metrics)["font_hashes"],
                    "platform": "linux-x86_64-ubuntu24.04", "files": {}}
        manifest["files"] = {str(p.relative_to(stage)): sha(p) for p in sorted(stage.rglob("*")) if p.is_file()}
        (stage / "bundle.json").write_text(json.dumps(manifest, sort_keys=True, indent=2) + "\n")
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open("wb") as output, gzip.GzipFile(filename="", mode="wb", fileobj=output, mtime=epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                for path in sorted(stage.rglob("*")):
                    if not path.is_file():
                        continue
                    member = archive.gettarinfo(str(path), arcname=str(path.relative_to(stage)))
                    member.uid = member.gid = 0; member.uname = member.gname = ""; member.mtime = epoch
                    with path.open("rb") as content:
                        archive.addfile(member, content)
        pin = pin_contract(manifest)
        pin.update(sha256=sha(destination), bytes=destination.stat().st_size, manifest_sha256=sha(stage / "bundle.json"))
        destination.with_suffix(".pin.json").write_text(json.dumps(pin, sort_keys=True, indent=2) + "\n")
        destination.with_suffix(".text-metrics.json").write_bytes(metrics)
        print(json.dumps(pin, sort_keys=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    pack(args.bin.resolve(), args.out.resolve())


if __name__ == "__main__":
    main()
