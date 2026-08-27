# The pipeline ABR tier, once it stopped entering HLS through a bug

**Measured 2026-08-27, on the dev television.** Companion to `local-original-blind.md`: that one
records the defect, this one records what the test tier looked like after it was fixed.

## 1. Why the tier moved at all

Eighteen `pipe_abr_*` cases reached the HLS controller by declaring an Original source rate no link
could carry — `auto_network.source_kbps: 900000` — and letting `ImminentStarvation` fire. On an
UNSHAPED link the reserve was *filling* while that happened, so the entry depended entirely on the
starvation horizon not checking whether anything was draining. Requiring that check closed it.

`route::arm_auto_fixture(..., start_hls: true)` replaces it: the post-fallback state is installed
directly and the controller runs from the first segment, at the **bootstrap rung**.

**That is a fairer plant, not merely a working one.** A fresh Auto HLS playback in the field starts
at 480p/720 kbps and climbs — `auto_link_squeeze` on the server tier does exactly that (720 → 2000
→ 4000). The old entry started at whatever rung `original_fallback_rung` picked from a measured
link, which no production playback of an infeasible-Original item ever does.

## 2. Result: 14 of 18

Four cases fail, and **all four fail on bounds written against the old entry**. None of them is a
new regression in the controller; two are findings the tier could not previously state.

| case | bound | got |
|---|---|---|
| `pipe_abr_slow_start_then_fast` | `floor_kbps: 10000` | reached **8000**, settled there, 2 commits |
| `pipe_abr_down_collapse` | `settle_max_kbps: 720` | settled **14000**, `max_stall_s=47` |
| `pipe_abr_down_staircase` | `settle_max_kbps: 720` | settled **10000**, `max_stall_s=33` |
| `pipe_abr_down_outrun` | `settle_max_kbps: 720` | settled **18000**, no stall |

### 2.1 The climb is the slow half, and that is arithmetic

`slow_start_then_fast` shapes 2 000 kbps for 10 s and then 40 000. From the bootstrap rung the
ladder has to be walked, and the §4 admission window needs **19 samples per rung** before it will
decide — so 720 → 2000 → 4000 → … → 10000 cannot happen inside the case's 43 segments. It got two
commits and settled on 8000. The bound was reachable only because the old entry *jumped* to a
fallback-selected rung.

This is the under-reach the plan of record already tracks; the tier can now see it.

### 2.2 `down_collapse` STALLS instead of coming down, and that is the finding

The profile is 40 000 kbps for 25 s, then **500**. The controller climbs to 14 000 on the fast leg
— correctly — and when the link collapses it produces **47 s of continuous stall**, 27 segments in
117 beats, and a media clock at **526 pm mean / 0 pm worst**. It does not reach the floor.

`down_staircase` is the same shape (33 s). `down_outrun` settles at 18 000 with NO stall, which is
its own question: its `segment_profile` keys the collapse to request index rather than wall clock,
and 51 segments went by.

At 500 kbps the bottom rung is itself unaffordable (320 video + ~192 audio), so *some* slowness is
required — but a 47 s stall at 14 000 kbps is not that. This is squarely the emergency/admission
law the plan sequences as **I5** (N3 + N4 + N5), and it is now reproducible on demand, on a tier
that needs no Plex and no library.

## 3. What the four bounds should become

Nothing here yet, deliberately. `settle_max_kbps: 720` was written for a controller that entered at
a measured rung; re-deriving it needs the I5 law, not a number chosen to make today's trace pass.
The traces are in `pipeline-hls-entry-logs/` so the before-and-after of that increment is
apples-to-apples.

## 4. Also new in these traces

`ff: v=#0 codec=… WxH` now appears on the HLS path, logged on CHANGE (2–5 lines per case). Until
this run no log on either tier stated the codec or raster of the stream the HLS demuxer was
actually decoding — `abr: committed … 1280x720` is the catalog raster of the rung that was
*requested*. `docs/adaptive-playback-plan.md` §7.B names that gap; this closes the codec/raster
half of it.
