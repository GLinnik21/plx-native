#!/usr/bin/env python3
"""Attach an immutable authoring bundle in CI, safely retrying partial uploads."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def attach(directory, source, tag="", development=False):
    pin = json.loads((directory / "plxnative-hud-simulator-linux-x86_64.tar.pin.json").read_text())
    if not re.fullmatch(r"[0-9a-f]{40}",source) or source != pin["source_sha"]:
        raise ValueError("simulator must identify the exact workflow source commit")
    if development:
        if tag or not pin["development"]:
            raise ValueError("development publication requires an unreleased simulator and no app release tag")
        tag = "hud-simulator-" + source
    elif tag != "v" + pin["app_version"] or pin["development"]:
        raise ValueError("app release tag and simulator version must agree")
    if not development:
        # A manual run on main can still report the last release version under RELEASE=1.
        # The release tag must identify these exact compiler bytes, not just the same version.
        tagged = subprocess.run(["gh","api",f"repos/{{owner}}/{{repo}}/commits/{tag}","--jq",".sha"],
                                capture_output=True,text=True,check=True).stdout.strip()
        if tagged != source:
            raise ValueError("app release tag does not resolve to the pinned source commit")
    existing = subprocess.run(["gh","release","view",tag,"--json","assets"],capture_output=True,text=True)
    if existing.returncode:
        if not development or "release not found" not in existing.stderr.lower():
            raise RuntimeError(existing.stderr)
        subprocess.run(["gh","release","create",tag,"--target",source,"--prerelease",
                        "--title","Player HUD authoring simulator (development)","--notes",
                        f'Development utility for HUD registry validation. Source: {source}. Target app: {pin["app_version"]}-dev.'],check=True)
        names = set()
    else:
        names = {item["name"] for item in json.loads(existing.stdout)["assets"]}
    for path in sorted(directory.glob("plxnative-*")):
        if path.name in names:
            with tempfile.TemporaryDirectory(prefix="hud-release-") as temporary:
                subprocess.run(["gh","release","download",tag,"--pattern",path.name,"--dir",temporary],check=True)
                before = Path(temporary) / path.name
                if hashlib.sha256(before.read_bytes()).digest() != hashlib.sha256(path.read_bytes()).digest():
                    raise ValueError("immutable simulator asset differs: " + path.name)
        else:
            subprocess.run(["gh","release","upload",tag,str(path)],check=True)
    return tag


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts",type=Path,required=True)
    parser.add_argument("--source",required=True)
    parser.add_argument("--tag",default="")
    parser.add_argument("--development",action="store_true")
    args = parser.parse_args()
    print(attach(args.artifacts,args.source,args.tag,args.development))
