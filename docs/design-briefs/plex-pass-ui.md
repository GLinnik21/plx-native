# Design brief — Plex Pass awareness in the player UI

> **Resolution (2026-08-11, owner's directive): the design is the VISUAL baseline; the logic
> follows Plex's own Pass documentation.** The design's second revision cut deliverable B's
> third state on a premise the docs contradict ("a server without Plex Pass cannot convert at
> all" — h264 software encoding is free; only HEVC/hardware transcode and HDR tone mapping are
> Pass-gated). So: the HDR→SDR warning STANDS (chip + capsule, `detail.rs::hdr_degrades` is
> the docs-derived truth table), the two quiet states stand, and the mock's C-variant wording
> ("cannot encode video without PLEX PASS") was not adopted — the read-out keeps the causally
> honest reason with the capsule as a separately stated fact.

Paste this whole brief into the Claude Design project. Deliverables come back as preview cards
(HTML mockups + a short spec each) so they can be pulled into the codebase via design-sync, the
same flow the Continue Watching cards used.

## Product context (read first)

**PlxNative** — a native, unofficial Plex client for LG webOS televisions. 10-foot UI in the
Apple TV design language: near-black grounds, rounded-rect cards, focus shown by scale + soft
glow (spring-animated), generous spacing. Authored on a fixed **1920x1080** canvas. Input is a
D-pad plus LG's Magic Remote pointer — every interactive element needs a focus state, but two of
the three deliverables here are **non-interactive read-outs**.

Type ladder (tokens, px at 1080p): TITLE 40 · BODY 28 · CAPTION 24 · MICRO 22 (MICRO is banned
for content — never fit more by shrinking). All colors ship as named theme tokens; a mockup
should name intents ("warning ink", "capsule fill") rather than only hex.

Existing pieces these designs must sit beside, not fight:
- The detail page already has a **filled badge row** (e.g. a `4K` version badge) in the facts
  line under the title — deliverable B extends this row.
- Menus are **popover + table view**, never full-screen sheets.
- The theme already carries a warm amber `#fab82e` (Continue Watching progress fill). The Plex
  Pass brand gold is `#e5a00d`. Decide deliberately: share one amber or introduce the second as
  its own token — do not let two near-misses coexist by accident.

## Why these designs exist

The app now knows two things it never knew before: whether **the user's Plex server has a Plex
Pass** (some server abilities — HEVC encoding, HDR tone-mapping — exist only with one), and how
an item will actually reach the TV (direct play vs server conversion). One real situation cannot
be fixed in code and must be communicated instead: **on a server without Plex Pass, an HDR film
the TV cannot decode natively gets converted without tone-mapping — washed-out, gray-ish
colors.** The user should learn this *before* pressing Play, and in Plex's own visual language:
the official apps mark subscription-gated features with a Plex Pass badge.

**Trademark constraint, non-negotiable:** we are an unofficial client and must NOT reproduce the
Plex Pass *logo artwork*. The badge is a **text capsule reading "PLEX PASS"** — referential use
of the name only. Color may reference the brand gold.

## Deliverable A — the "PLEX PASS" capsule (component)

A small text capsule: the word pair "PLEX PASS", likely CAPTION-size or smaller-cap styling,
gold treatment on the near-black ground. Design:
- geometry (corner radius, padding, letter-spacing; all-caps?);
- fill vs outline treatment, and ink color on the gold if filled;
- how it sits inline beside BODY text, and inside the detail badge row next to a filled `4K` badge;
- a grayscale check: it must still read as a badge with hue removed (severity/meaning may never
  be carried by color alone).

## Deliverable B — "how this plays" in the detail facts row

Extends the existing badge row on the detail page. Three states, only ever one shown:

1. **Direct Play** — neutral fact, quiet. (Most content on most setups.)
2. **Converts on server** — neutral fact, quiet. Not a warning: conversion usually just works.
3. **HDR → SDR** + the PLEX PASS capsule — the one warning state: "this HDR item will be
   converted without tone-mapping on this server; proper HDR conversion needs Plex Pass."
   Warning *tone*, not error tone — the film still plays.

Design the row's wording (short — it shares a line with year/runtime/rating facts), the visual
weight of each state (1 and 2 must be quieter than the `4K` badge; 3 may not be louder than the
Play button), and how state 3 composes badge + capsule + a few words. Non-interactive.

## Deliverable C — the playback error read-out

Today a failed playback shows one centered caption line over black. The app now produces a
*reason* alongside the verdict. Design a centered read-out for the video area (no card chrome
needed — the ground IS black), composed of:
- a failure glyph (we have an icon set; describe the shape, e.g. a triangle/exclaim);
- the verdict line (TITLE or BODY weight — pick): "Playback failed";
- the reason line (BODY/CAPTION): e.g. "The server sent audio only — it cannot encode video
  without Plex Pass" — this variant includes the PLEX PASS capsule inline;
- a quiet action hint: "Press BACK to return".

Variants to show: (1) generic failure, no reason; (2) audio-only / no Plex Pass, with capsule;
(3) "This file has no video track". Must be legible from a couch (10 ft) and survive a phone
photograph — high luminance contrast, no hue-only signals.

## Deliverable D (optional — only if it excites you)

A post-play **Up Next** card: today the next-episode offer is a tile in the player control row
with a fill-sweep countdown. A richer end-of-episode treatment (poster, title, countdown ring,
"watch credits" escape) in the Apple TV style is welcome as a proposal — the mechanics exist,
this is purely presentational. Not required.

> **Resolution (2026-08-12, owner's call): the TILE stays, with a second button.** The card came
> back as proposed and shipped at `84ff9328`; the owner then set the weight rule — *interruption
> weight matches event weight*. The next episode of the show you are already watching is a small
> event and a corner tile is enough; a full-screen poster to say "S2E4 next" shows you the show
> you have been looking at for an hour. `Player Screen.dc.html`'s update is what is in the tree:
> the tile plus **Watch Credits**, so declining is a thing you can press. The countdown is the
> button's fill sweep, not a ring — the owner's line was that the app already has one visual
> language for a timer. The full card survives at `84ff9328` for the CROSS-SHOW case (a finished
> show, a different one queued), which has no trigger: `continuous=1` is per-show and a final
> episode returns `totalCount=1`, so sourcing it from Continue Watching is an open decision.

## Out of scope

The diagnostics ("Stats for nerds") panel — deliberately undesigned, debug surface. Server rows
in menus — covered by the existing popover/table idiom. Any use of Plex's logo assets — see the
trademark constraint.
