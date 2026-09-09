#!/usr/bin/env python3
"""Grade the content-navigation smoke flows by what ran, not merely by log volume."""
import argparse
import re


def fields(lines):
    return [dict(re.findall(r"(\w+)=([^ ]+)", line.strip()))
            for line in lines if line.startswith("focus ")]


def check(flow, lines):
    lines = list(lines)
    records = fields(lines)
    if flow == 1:
        home = [r for r in records if r.get("route") == "home"]
        wanted = [lambda r: r.get("snapt") == "0" and r.get("hf") == "-1",
                  lambda r: r.get("snapt") == "0" and r.get("hf") == "0",
                  lambda r: r.get("snapt") == "1" and r.get("row") == "0",
                  lambda r: r.get("snapt") == "1" and r.get("row") == "1"]
        stage = 0
        for record in home:
            if stage < len(wanted) and wanted[stage](record):
                stage += 1
        if stage != len(wanted) or not home or not wanted[-1](home[-1]):
            return "Home must walk chip → hero → first shelf → second shelf; DOWN must not return to the hero"
    elif flow == 2:
        detail = next((i for i, r in enumerate(records) if r.get("route") == "detail"), None)
        if detail is None:
            return "the grid card never opened Detail"
        before = [r for r in records[:detail] if r.get("route") == "home"]
        after = [r for r in records[detail + 1:] if r.get("route") == "home"]
        if not before or not after:
            return "BACK never returned from Detail to the Home grid"
        if any(before[-1].get(k) in (None, "-") or after[-1].get(k) in (None, "-")
               for k in ("sid", "rk", "row", "col")):
            return "the Home fingerprints omit the card identity or position"
        if any(before[-1].get(k) != after[-1].get(k) for k in ("sid", "rk", "row", "col")):
            return "BACK returned to a different Home card"
    elif flow == 3:
        # Keep the route walk as a sequence of focus-owned segments. A heartbeat that merely says
        # "library" is not enough: the Library must have emitted its own focus fingerprint before
        # the card opened Detail and after BACK restored the grid.
        segments = []
        for record in records:
            route = record.get("route")
            if not segments or segments[-1][0] != route:
                segments.append((route, []))
            segments[-1][1].append(record)
        wanted = ["home", "library", "detail", "library"]
        start = next((i for i in range(len(segments) - len(wanted) + 1)
                      if [route for route, _ in segments[i:i + len(wanted)]] == wanted), None)
        if start is None:
            return "the Home → Library → Detail → BACK → Library sequence never ran"

        before = segments[start + 1][1][-1]
        after = segments[start + 3][1][0]
        for record in (before, after):
            if tuple(record.get(key) for key in ("pill", "card", "menu")) != ("-1", "1", "0"):
                return "Library must focus a card with no menu before Detail and after BACK"
        for key in ("sid", "rk"):
            if before.get(key) in (None, "-") or after.get(key) in (None, "-"):
                return "the Library fingerprints omit the selected card identity"
            if before.get(key) != after.get(key):
                return "BACK returned to a different Library card"
            if segments[start + 2][1][0].get(key) != before.get(key):
                return "Library opened Detail for a different card or omitted its identity"

        # The current probe supplies pill/card/menu; future probe revisions may add row/col or an
        # explicit viewport. Compare every non-route field supplied by either side, refusing a
        # field that disappears on return instead of silently grading only the identity pair.
        focus_fields = (set(before) | set(after)) - {"route", "press"}
        for key in sorted(focus_fields):
            if before.get(key) in (None, "-") or after.get(key) in (None, "-"):
                return "the Library fingerprints omit focus or viewport field: " + key
            if before.get(key) != after.get(key):
                return "BACK changed the Library focus or viewport field: " + key

        # BACK emits no navigation input after it restores the grid; later focus shots are the
        # evidence that the restored card stayed selected. Validate the entire restored segment,
        # not just its first record. `press` is probe metadata and may legitimately vary between
        # shots, but every other field must remain the pre-Detail fingerprint.
        fingerprint = {key: value for key, value in before.items()
                       if key not in ("route", "press")}
        for record in segments[start + 3][1]:
            candidate = {key: value for key, value in record.items()
                         if key not in ("route", "press")}
            if candidate != fingerprint:
                for key in sorted(set(fingerprint) | set(candidate)):
                    if candidate.get(key) in (None, "-"):
                        return "the Library fingerprints omit focus or viewport field: " + key
                    if fingerprint.get(key) != candidate.get(key):
                        return "BACK changed the Library focus or viewport field: " + key

        # Flow 3 sends no further navigation after BACK. A later route change therefore cannot be
        # explained by the flow and must not be hidden by an earlier valid Library restoration.
        if any(route != "library" for route, _ in segments[start + 3:]):
            return "the flow left Library after BACK"
    elif flow == 5:
        routes = []
        visits = []
        for r in records:
            route = r.get("route")
            if not routes or routes[-1] != route:
                routes.append(route)
                visits.append(r)
            else:
                visits[-1] = r
        wanted = ["person", "detail", "person", "detail"]
        start = next((i for i in range(len(routes)) if routes[i:i + len(wanted)] == wanted), None)
        if start is None:
            return "the Person → Detail → BACK → Person → BACK → Detail sequence never ran"
        before, after = visits[start], visits[start + 2]
        if any(before.get(k) in (None, "-") or after.get(k) in (None, "-") for k in ("sid", "rk")):
            return "the Person fingerprints omit the selected card identity"
        if any(before.get(k) != after.get(k) for k in ("sid", "rk", "group", "elem")):
            return "BACK returned to a different Person card or focus key"
    elif flow == 6:
        stages = []
        for line in lines:
            if line.startswith("hb "):
                record = dict(re.findall(r"(\w+)=([^ ]+)", line.strip()))
                stage = record.get("overlay", record.get("route"))
                if not stages or stages[-1] != stage:
                    stages.append(stage)
        wanted = ["settings", "privacy", "settings", "legal", "settings", "home"]
        if not any(stages[i:i + len(wanted)] == wanted for i in range(len(stages))):
            return "the Settings → Privacy → Settings → Legal → Settings → Home sequence never ran"
    elif flow == 8:
        menu = next((i for i, r in enumerate(records)
                     if r.get("route") == "itemmenu" and r.get("over") == "detail"), None)
        if menu is None:
            return "holding Related never opened ItemMenu over Detail"
        before = [r for r in records[:menu] if r.get("route") == "detail"]
        after = [r for r in records[menu + 1:] if r.get("route") == "detail"]
        if not before or before[-1].get("sec") != "3" or before[-1].get("card") != "1":
            return "the held control was not a Related card"
        if not after:
            return "BACK never dismissed ItemMenu to Detail"
        if any(before[-1].get(k) in (None, "-") or after[-1].get(k) in (None, "-")
               for k in ("sid", "rk", "sec", "col")):
            return "the Detail fingerprints omit the item identity or position"
        if any(before[-1].get(k) != after[-1].get(k) for k in ("sid", "rk", "sec", "col")):
            return "dismissing ItemMenu lost the Related card's position"
    elif flow == 12:
        film = lambda r: r.get("route") == "person" and r.get("filmography") == "1"
        first = next((i for i, r in enumerate(records) if film(r)), None)
        if first is None:
            return "Filmography never owned input"
        detail = next((i for i in range(first + 1, len(records))
                       if records[i].get("route") == "detail"), None)
        if detail is None:
            return "the library-matched Filmography credit never opened Detail"
        before = [r for r in records[first:detail] if film(r)]
        after = [r for r in records[detail + 1:] if film(r)]
        if not after:
            return "BACK discarded Filmography instead of restoring it"
        for key in ("group", "elem"):
            if before[-1].get(key) in (None, "-") or after[-1].get(key) != before[-1][key]:
                return "BACK lost the Filmography credit's focus"
        restored = max(i for i, r in enumerate(records) if film(r))
        if not any(r.get("route") == "person" and r.get("filmography") == "0"
                   for r in records[restored + 1:]):
            return "the second BACK never dismissed Filmography to Person"
    return None


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("flow", type=int)
    parser.add_argument("fingerprints")
    args = parser.parse_args()
    with open(args.fingerprints, encoding="utf-8") as src:
        error = check(args.flow, src)
    if error:
        print(error)
        raise SystemExit(1)
