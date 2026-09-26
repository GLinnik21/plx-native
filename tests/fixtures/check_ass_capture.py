#!/usr/bin/env python3
"""Check the static authored marks in a TV capture of styled-ass.mkv (Pillow).

Run with the HUD hidden. This complements manual inspection and does not grade
animation or FPS. The original plain-caption renderer fails the green/blue checks.
"""
import argparse
from PIL import Image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("--second-track", action="store_true")
    args = parser.parse_args()
    image = Image.open(args.image).convert("RGB")
    sx, sy = image.width / 1920, image.height / 1080

    def count(rect, matches):
        box = tuple(round(value * (sx if i % 2 == 0 else sy))
                    for i, value in enumerate(rect))
        pixels = image.crop(box).getdata()
        return sum(matches(*pixel) for pixel in pixels) / (sx * sy)

    sign = ((lambda r, g, b: r > g * 1.5 + 15 and b > g * 1.5 + 15)
            if args.second_track else
            (lambda r, g, b: g > r * 1.5 + 15 and g > b * 1.5 + 15))
    # Even the darkest existing subtitle tone is71/255; the fixture ground is32.
    # Keep the check valid without changing the viewer's saved tone preference.
    marks = {
        "authored sign": (count((80, 50, 1000, 240), sign), 1000),
        "blue vector drawing": (count((1350, 80, 1780, 290),
                                      lambda r, g, b: b > r * 1.5 + 15 and b > g * 1.5 + 15), 3000),
        "overlapping bottom dialogue": (count((200, 900, 1750, 1080),
                                             lambda r, g, b: min(r, g, b) > 50), 1000),
    }
    for name, (pixels, minimum) in marks.items():
        print(f"{name}: {pixels:.0f} pixels (minimum {minimum})")
    if any(pixels < minimum for pixels, minimum in marks.values()):
        raise SystemExit("FAIL: authored subtitle marks are absent or misplaced")
    print("PASS: authored sign, vector and simultaneous dialogue")


if __name__ == "__main__":
    main()
