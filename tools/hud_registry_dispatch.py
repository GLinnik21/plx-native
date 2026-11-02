#!/usr/bin/env python3
"""Notify the HUD registry, or test the same credential without inventing an app release."""
import argparse
import json
from pathlib import Path
import re
import subprocess


def dispatch(pin=None, tag="", probe=""):
    if probe:
        if pin is not None or tag or not re.fullmatch(r"[0-9a-f]{40}", probe):
            raise ValueError("a dispatch probe requires only an exact source commit")
        body = {"event_type": "app-release-probe", "client_payload": {"source_sha": probe}}
    else:
        if (pin is None or pin.get("development", False)
            or not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag)
            or tag != "v" + pin["app_version"]):
            raise ValueError("release dispatch requires a stable app simulator and matching tag")
        simulator = dict(pin)
        simulator["url"] = ("https://github.com/GLinnik21/plx-native/releases/download/" + tag
                            + "/plxnative-hud-simulator-linux-x86_64.tar.gz")
        body = {"event_type": "app-release", "client_payload": {"simulator": simulator}}
    # GH_TOKEN is supplied by the workflow environment; it is never read or logged here.
    subprocess.run(["gh", "api", "repos/GLinnik21/plxnative-huds/dispatches", "--input", "-"],
                   input=json.dumps(body), text=True, check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--pin", type=Path)
    modes.add_argument("--probe", default="")
    parser.add_argument("--release-tag", default="")
    args = parser.parse_args()
    dispatch(json.loads(args.pin.read_text()) if args.pin else None, args.release_tag, args.probe)
