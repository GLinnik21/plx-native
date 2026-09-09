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
    if flow == 2:
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
