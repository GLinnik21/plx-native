#!/usr/bin/env python3
"""Registry HUD preview matrix through the app's shared synthetic screenshot driver.

The app compiler supplies each reachable section combination and the actual opener geometry.
No server/account credentials, TV, screen-local renderer or duplicated HUD layout is involved.
"""
from __future__ import annotations
import argparse
import copy
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

import screenshots

ROOT = Path(__file__).resolve().parents[1]
PREROLL = 600
DEFAULTS = {"canvas": "1920x1080", "settle_ms": 500, "after_ms": 1000, "timeout_s": 180, "stillclock_ms": 4000}
LONG_TITLES = {
    "en": "The extraordinarily long chapter in which the last dragon returns across the frozen mountains",
    "es": "El extraordinario capítulo en el que el último dragón regresa a través de las montañas heladas",
    "be": "Надзвычай доўгі раздзел, у якім апошні цмок вяртаецца праз заснежаныя горы",
    "fr": "Le chapitre extraordinairement long où le dernier dragon revient à travers les montagnes glacées",
    "it": "Il capitolo straordinariamente lungo in cui l’ultimo drago ritorna attraverso le montagne ghiacciate",
    "pt": "O capítulo extraordinariamente longo em que o último dragão regressa pelas montanhas geladas",
    "de": "Das außergewöhnlich lange Kapitel, in dem der letzte Drache über die gefrorenen Berge zurückkehrt",
    "hu": "A rendkívül hosszú fejezet, amelyben az utolsó sárkány visszatér a fagyos hegyeken keresztül",
    "ru": "Необычайно длинная глава, в которой последний дракон возвращается через заснеженные горы",
    "pl": "Niezwykle długi rozdział, w którym ostatni smok powraca przez zamarznięte góry",
    "nl": "Het buitengewoon lange hoofdstuk waarin de laatste draak over de bevroren bergen terugkeert",
}


def plan(inspection):
    """Cover orthogonal states, every allowed section combination, and every distinct native opener."""
    languages = [item["lang"] for item in inspection["languages"]]
    if set(languages) != set(LONG_TITLES):
        raise ValueError("update the authored long-title corpus for the shipped language list")
    english = next(item for item in inspection["languages"] if item["lang"] == "en")
    scenes = []
    def add(name, **changes):
        scenes.append({"name": name, "lang": "en", "open": 0, "paused": True, "navigation_pair": False,
                       "chapters": True, "real_episode": False, "background": "dark", **changes})
    seen, openers = set(), set()
    for state in english["states"]:
        if state["slot"] != "discs" or not state["chapters"] and state["open"] & 1:
            continue
        # Navigation belongs to episodes. A lone episode is a different meaningful valuation:
        # it retains its episode identity and kicker while having no Previous/Next pair.
        if not state["episode"] and state["navigation_pair"]:
            continue
        signature = state["open"], state["paused"], state["navigation_pair"], state["episode"], state["chapters"]
        props = {k: state[k] for k in ("open", "paused", "navigation_pair", "chapters")}
        props["real_episode"] = bool(state["episode"])
        if signature not in seen:
            seen.add(signature)
            kind = "episode" if state["navigation_pair"] else "isolated-episode" if state["episode"] else "movie"
            absent = "-no-chapters" if not state["chapters"] else ""
            add(f'sections-{state["open"]}-{kind}-{"paused" if state["paused"] else "playing"}{absent}', **props)
        for control in state["controls"]:
            if control.get("menu") not in {"tracks", "more"}:
                continue
            # An opener that moves between section states needs a preview at each authored position.
            key = control["id"], control["menu"], tuple(control["rect"]), state["open"], control["label"]
            if key in openers:
                continue
            openers.add(key)
            x, y, width, height = control["rect"]
            add(f'menu-{len(openers):03d}-{control["menu"]}', **props,
                opener={"id": control["id"], "menu": control["menu"], "point": [x + width / 2, y + height / 2]})
    compact = next(state for state in english["states"] if state["slot"] == "discs" and state["open"] == 0
                   and state["paused"] and not state["navigation_pair"] and state["chapters"])
    if compact["scrub"] is None:
        raise ValueError("compiler returned no scrub hit area")
    x,y,width,height = compact["scrub"]
    add("seeking", seeking=True, scrub_point=[x+width/2,y+height/2])
    add("up-next", paused=False, navigation_pair=True, real_episode=True, slot="up_next")
    for kind in ("intro", "credits"):
        add("skip-" + kind, paused=False, slot="skip", marker=kind)
    add("failure", failure=True)
    for background in ("bright", "dark"):
        add("frame-" + background, background=background)
    opens = sorted({scene["open"] for scene in scenes})
    for opened in opens:
        add(f"captions-huge-high-sections-{opened}", open=opened, captions=True)
    for lang in languages:
        add("long-title-" + lang, lang=lang, long_title=True, open=max(opens))
    return scenes


def fixture(spec, hud):
    def prepare(pms, runtime):
        source = pms.lib.items[102]
        # Episode metadata/queue is real mock catalog data; only the media is the openly licensed
        # Sintel stream, so every state exercises the same actual demux/video path.
        if spec["navigation_pair"]:
            source_part = source["Media"][0]["Part"][0]["id"]
            media_file = pms.lib.media_files[int(source_part)]
            for rk in (2011001, 2011002, 2011003):
                item = pms.lib.items[rk]
                part_id = int(item["Media"][0]["Part"][0]["id"])
                media, duration = pms.lib._verified_media(media_file, rk, part_id)
                item.update(Media=[media], duration=duration)
                item["Chapter"] = copy.deepcopy(source.get("Chapter", []))
                item["Marker"] = copy.deepcopy(source.get("Marker", []))
                if spec.get("slot") == "up_next":
                    item["Marker"] = [{"id": rk, "type": "credits", "startTimeOffset": 173000, "endTimeOffset": duration}]
            source = pms.lib.items[2011002]
        if spec.get("real_episode") and not spec["navigation_pair"]:
            # The Home frame renders before the play trigger lands. Give the isolated episode
            # real synthetic parent records and catalog artwork too, so that boot never owes
            # an image for a nonexistent parent. Its show has only this episode in the queue.
            for old,new in ((201,99100),(2011,99101)):
                parent = copy.deepcopy(pms.lib.items[old])
                parent.update(ratingKey=str(new),key=f"/library/metadata/{new}")
                pms.lib.items[new] = parent
                pms.lib.images[new] = dict(pms.lib.images[old])
            source.update(type="episode", grandparentRatingKey="99100", parentRatingKey="99101",
                          grandparentTitle="Design Review", parentTitle="Season 1", parentIndex=1,index=1)
        start_at = 445000
        if spec["background"] == "bright":
            start_at = 30000
        if spec.get("slot") == "up_next":
            start_at = 174750
        if spec.get("marker"):
            start_at = 25000 if spec["marker"] == "intro" else 174750
        source["viewOffset"] = start_at
        if spec.get("marker"):
            start = 20000 if spec["marker"] == "intro" else 173000
            source["Marker"] = [{"id": 99002, "type": spec["marker"], "startTimeOffset": start,
                                 "endTimeOffset": start + 60000, "final": spec["marker"] == "credits"}]
        if not spec.get("chapters", True):
            source.pop("Chapter", None)
        part = source["Media"][0]["Part"][0]
        for chapter in source.get("Chapter", []):
            chapter["thumb"] = f'/library/parts/{part["id"]}/indexes/sd/{chapter["startTimeOffset"]}'
        if spec.get("long_title"):
            title = LONG_TITLES[spec["lang"]]
            source["title"] = title
            source["summary"] = title + ". " + title + ". " + title
            for chapter in source.get("Chapter", []):
                chapter["tag"] = title
        if spec.get("captions"):
            sub = {"id": 99001, "streamType": 3, "codec": "srt", "format": "srt", "language": "English",
                   "languageCode": "eng", "index": 2, "key": "/library/streams/99001", "selected": True, "displayTitle": "English"}
            part.setdefault("Stream", []).append(sub)
            pms.lib.sidecars[99001] = b'1\n00:07:20,000 --> 00:07:45,000\nFirst line of a mock subtitle.\nSecond line stays clear of the controls.\n'
            pms.lib.verification_streams[int(part["id"])] = part["Stream"]
            # A fresh synthetic, credential-free legacy preference document. The normal boot
            # migration and profile fallback read it; no production identity is opened or modified.
            (runtime / "auth.json").write_text(json.dumps({"subtitle_size": "huge", "subtitle_position": "high"}))
        (runtime / "preview.plxhud.json").write_bytes(hud.read_bytes())
        (runtime / "plxnative-hud").write_text(str(runtime / "preview.plxhud.json"))
    return prepare


def capture_scene(spec):
    triggers = {"play": "2011002" if spec["navigation_pair"] else "102", "clocksink": "", "simvideo": "", "focus": "", "hintoff": ""}
    virtual = {"preroll": PREROLL}
    expect = [f'plxnative-hud: compiled for {spec["lang"]}', "simvideo: first picture decoded"]
    position = 31020 if spec["background"] == "bright" else 446020
    triggers["clockstop"] = str(position)
    if spec["paused"]:
        triggers["autopause"] = f"at={position}"
        expect.append("autopause: Pause accepted")
    else:
        virtual["keys"] = "550:up"
        if not spec.get("slot"):
            expect.append(f"clockstop: clock held at {position} ms")
    if spec["open"] & 1:
        triggers["chapters"] = ""
    if spec["open"] & 2:
        triggers["info"] = ""
    if spec.get("opener"):
        # Reveal first, then click through the ordinary hit map. No manual command dispatch.
        virtual.update(keys="420:up" if not spec["paused"] else "", overlay=True)
        x, y = spec["opener"]["point"]
        virtual["clicks"] = f"450:{x}:{y}"
        expect.append("ov=" + ("menu" if spec["opener"]["menu"] == "tracks" else "more"))
    if spec.get("seeking"):
        # Twelve frames let the normal six-frame key press release, while frame three records
        # the active scrub. This preserves the existing input-release contract.
        x,y = spec["scrub_point"]
        virtual.update(frames=12, select=3, clicks=f"{PREROLL}:{x}:{y}")
    if spec.get("slot") == "up_next":
        triggers.update(clockstop="175750")
        expect += ["slot=upnext", "clockstop: clock held at 175750 ms"]
    if spec.get("marker"):
        # Deterministic markers around the frozen playhead, including a separate credits case.
        triggers["clockstop"] = "175750" if spec["marker"] == "credits" else "27020"
        expect += ["slot=skip", "clockstop: clock held at " + triggers["clockstop"] + " ms"]
    if spec.get("captions"):
        expect.append("server-selected sidecar subtitle: sid=99001")
    if spec.get("failure"):
        triggers["failtest"] = ""
    focus = {"slot": "upnext" if spec.get("slot") == "up_next" else spec.get("slot", "discs"),
             "chap": str(int(bool(spec["open"] & 1))), "info": str(int(bool(spec["open"] & 2))),
             "ov": "none"}
    if not spec.get("slot"):
        focus.update(pair=str(int(spec["navigation_pair"])), haschap=str(int(spec["chapters"])))
    if spec.get("opener"):
        focus["ov"] = "menu" if spec["opener"]["menu"] == "tracks" else "more"
    if spec.get("failure"):
        focus = {"hidden":"1"}
    return {"name": spec["name"], "lang": spec["lang"], "triggers": triggers, "virtual": virtual,
            "expect": expect, "expect_focus": focus}


def run(binary, hud, output, only=None):
    env = {k: v for k, v in screenshots.os.environ.items() if not k.startswith("PLXNATIVE_")}
    env["PLXNATIVE_APP_DIR"] = str(ROOT / "pkg")
    info = json.loads(subprocess.check_output([str(binary), "--hud-info"], env=env, text=True))
    inspection = json.loads(subprocess.check_output([str(binary), "--hud-inspect", str(hud)], env=env, text=True))
    matrix = plan(inspection)
    selected = [scene for scene in matrix if only is None or scene["name"] in only]
    if only is not None and set(only) != {scene["name"] for scene in selected}:
        raise ValueError("unknown preview scene selection")
    output.mkdir(parents=True, exist_ok=True)
    scenes = []
    for spec in selected:
        png, log = screenshots.capture(binary, capture_scene(spec), DEFAULTS, "sintel", False,
                                       prepare=fixture(spec, hud), assets=ROOT / "pkg")
        name = spec["name"] + ".png"
        (output / name).write_bytes(png)
        (output / (spec["name"] + ".log")).write_text(log)
        scenes.append({"file": name, "sha256": hashlib.sha256(png).hexdigest(), "state": spec["name"], "lang": spec["lang"]})
    manifest = {"protocol": 1, "package_sha256": hashlib.sha256(hud.read_bytes()).hexdigest(), "simulator": info,
                "required": [scene["name"] for scene in matrix], "complete": len(selected) == len(matrix), "scenes": scenes}
    (output / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--hud", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--only", help="comma-separated scene names for iteration; incomplete output cannot publish")
    parser.add_argument("--plan", action="store_true")
    args = parser.parse_args()
    if args.plan:
        env = {**screenshots.os.environ, "PLXNATIVE_APP_DIR": str(ROOT / "pkg")}
        inspection = json.loads(subprocess.check_output([str(args.bin.resolve()), "--hud-inspect", str(args.hud.resolve())], env=env))
        print(json.dumps(plan(inspection), indent=2, ensure_ascii=False))
    else:
        run(args.bin.resolve(), args.hud.resolve(), args.out, args.only.split(",") if args.only else None)


if __name__ == "__main__":
    main()
