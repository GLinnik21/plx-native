#!/usr/bin/env python3
"""Resolve the app's design tokens from source, and diff a design-system tokens.json against them.

The design system (a `tokens.json` made from the Claude "Design System" artifact type) is a mirror of
`rust-modules/ui/src/theme.rs` and `rust-modules/gfx/src/gfx/tokens.rs`. This tool is the mechanical half
of keeping the two in step: it reads the colour and number constants, resolves every alias, `with_a(..)`,
`mix(..)` and `scrim_black(..)`, and either prints the result or reports where a design-system token has
drifted from it.

    tools/design-tokens.py export                      # resolved app tokens as JSON on stdout
    tools/design-tokens.py diff PATH/TO/tokens.json    # drift report; exit 1 when a mapped token differs

`diff` only judges tokens it can map to a constant by name (see `const_for`); the rest are listed as
unmapped, never as errors. Constants behind `#[cfg(feature = ..)]` and `#[cfg(test)]` are ignored: the
placeholder-sentinel build paints magenta on purpose and the threadcheck banner is developer-only.

Nothing here writes to the repository or the design system.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = [
    ROOT / "rust-modules/ui/src/theme.rs",
    ROOT / "rust-modules/gfx/src/gfx/tokens.rs",
    ROOT / "rust-modules/ui/src/consts.rs",
]
CONST_RE = re.compile(
    r"^[ \t]*(?:pub(?:\([a-z]+\))?\s+)?const\s+([A-Z][A-Z0-9_]*)\s*:\s*([^=]+?)=\s*(.*?);\s*$",
    re.S | re.M,
)
GATED_RE = re.compile(r'#\[cfg\((?:test|feature = "[a-z-]+")\)\]\s*(?:pub\s+)?const\s+[A-Z0-9_]+[^;]*;', re.S)


def load_defs():
    defs = {}
    for path in SOURCES:
        src = re.sub(r"//[^\n]*", "", path.read_text())
        src = GATED_RE.sub("", src)
        for name, ty, rhs in CONST_RE.findall(src):
            defs.setdefault(name, " ".join(rhs.split()))
    return defs


def hexa(c):
    h = "#%02x%02x%02x" % tuple(round(x * 255) for x in c[:3])
    return h if abs(c[3] - 1) < 1e-6 else h + "%02x" % round(c[3] * 255)


class Resolver:
    def __init__(self, defs):
        self.defs, self.cache = defs, {}

    def get(self, name):
        if name not in self.cache:
            try:
                self.cache[name] = self.expr(self.defs[name])
            except (ValueError, KeyError, RecursionError):
                self.cache[name] = None
        return self.cache[name]

    def expr(self, s):
        s = s.strip()
        if m := re.fullmatch(r"rgb8\(\s*0x(\w+),\s*0x(\w+),\s*0x(\w+)\s*\)", s):
            return [int(x, 16) / 255 for x in m.groups()] + [1.0]
        if m := re.fullmatch(r"with_a\((.+),\s*([\d.]+)\)", s):
            return self.expr(m.group(1))[:3] + [float(m.group(2))]
        if m := re.fullmatch(r"scrim_black\(([\d.]+)\)", s):
            return [0, 0, 0, float(m.group(1))]
        if m := re.fullmatch(r"scrim\(([\d.]+)\)", s):
            return self.get("NEUTRAL_950")[:3] + [float(m.group(1))]
        if m := re.fullmatch(r"mix\((.+?),\s*(.+?),\s*([\d.]+)\)", s):
            a, b, t = self.expr(m.group(1)), self.expr(m.group(2)), float(m.group(3))
            return [a[i] + (b[i] - a[i]) * t for i in range(4)]
        if m := re.fullmatch(r"(?:crate::)?(?:theme::)?([A-Z][A-Z0-9_]*)", s):
            v = self.get(m.group(1))
            if v is None:
                raise ValueError(s)
            return v
        if re.fullmatch(r"-?[\d.]+(?:_\d+)?", s):
            return float(s.replace("_", ""))
        raise ValueError(s)

    def value(self, name):
        v = self.get(name)
        return hexa(v) if isinstance(v, list) else v


def const_for(token):
    """The Rust constant a design-system token mirrors, or None."""
    if m := re.fullmatch(r"subtitle-ink-(\d)", token):
        return ("SUBTITLE_INKS", int(m.group(1)))
    if token == "underlay-tint":
        return "TINT"
    n = token.upper().replace("-", "_")
    return n[6:] if n.startswith("COLOR_") else n


def ds_value(tokens, name, seen=()):
    for fam, body in tokens.items():
        if not isinstance(body, dict) or "tokens" not in body:
            continue
        for t in body["tokens"]:
            if t["name"] == name:
                v = t["value"]
                v = v.get("dark", next(iter(v.values()))) if isinstance(v, dict) else v
                if m := re.fullmatch(r"\{(.+)\}", str(v)):
                    return ds_value(tokens, m.group(1))
                return v
    return None


def norm(v):
    """A comparable form: colours as (r, g, b, a), numbers as floats."""
    if isinstance(v, (int, float)):
        return float(v)
    s = str(v).strip().lower()
    if m := re.fullmatch(r"#([0-9a-f]{6})([0-9a-f]{2})?", s):
        rgb = [int(m.group(1)[i:i + 2], 16) for i in (0, 2, 4)]
        return tuple(rgb) + (round(int(m.group(2), 16) / 255, 3) if m.group(2) else 1.0,)
    if m := re.fullmatch(r"rgba?\(([^)]+)\)", s):
        p = [float(x) for x in m.group(1).split(",")]
        return (round(p[0]), round(p[1]), round(p[2]), p[3] if len(p) > 3 else 1.0)
    if m := re.fullmatch(r"(-?[\d.]*\d)(px|em)?", s):
        return float(m.group(1))
    return s


def close(a, b):
    if isinstance(a, tuple) and isinstance(b, tuple):
        return a[:3] == b[:3] and abs(a[3] - b[3]) < 0.006
    if isinstance(a, float) and isinstance(b, float):
        return abs(a - b) < 1e-3
    return a == b


def app_shadow(res, token):
    """A box-shadow string rebuilt from the card-shadow constants, or None."""
    if token == "card-shadow-focus":
        k = ("CARD_SHADOW_DY", "CARD_SHADOW_BLUR", None)
        alpha = res.get("CARD_SHADOW")[3]
    elif token == "card-shadow-rest":
        k = ("CARD_SHADOW_REST_DY", "CARD_SHADOW_REST_BLUR", None)
        alpha = res.get("CARD_SHADOW_REST_A")
    else:
        return None
    dy, blur = res.get(k[0]), res.get(k[1])
    return None if None in (dy, blur, alpha) else f"0 {dy:g}px {blur:g}px rgba(0,0,0,{alpha:g})"


def numbers(s):
    return [float(x) for x in re.findall(r"-?\d*\.?\d+", str(s))]


def app_value(res, token):
    if (sh := app_shadow(res, token)) is not None:
        return sh
    c = const_for(token)
    if isinstance(c, tuple):
        names = re.findall(r"[A-Z][A-Z0-9_]+", res.defs.get(c[0], ""))
        return res.value(names[c[1]]) if c[1] < len(names) else None
    return res.value(c) if c in res.defs else None


def cmd_export(res):
    out = {n: res.value(n) for n in sorted(res.defs) if res.value(n) is not None}
    json.dump(out, sys.stdout, indent=1)
    print()


def cmd_diff(res, path):
    tokens = json.loads(Path(path).read_text())
    drift, ok, unmapped = [], 0, []
    for fam, body in tokens.items():
        if not isinstance(body, dict) or "tokens" not in body:
            continue
        for t in body["tokens"]:
            name = t["name"]
            app = app_value(res, name)
            if app is None or (fam == "shadow" and app_shadow(res, name) is None):
                unmapped.append(name)
                continue
            ds = ds_value(tokens, name)
            if fam == "shadow":
                same = numbers(ds) == numbers(app)
            else:
                same = close(norm(ds), norm(app))
            if same:
                ok += 1
            else:
                drift.append((fam, name, ds, app))
    for fam, name, ds, app in drift:
        print(f"DRIFT  {fam:8s} {name:28s} design system {ds}  app {app}")
    print(f"{ok} tokens match, {len(drift)} drift, {len(unmapped)} not mapped to a constant")
    return 1 if drift else 0


def main(argv):
    res = Resolver(load_defs())
    if argv[1:2] == ["export"]:
        return cmd_export(res) or 0
    if argv[1:2] == ["diff"] and len(argv) == 3:
        return cmd_diff(res, argv[2])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
