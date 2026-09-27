# Global artwork cache — TV verification, 2026-09-26

The shared disk tier persisted all 1,200 distinct fixture images across a process restart.
The warm run made **zero image requests**, with the image endpoint explicitly returning failures
if called. Its **2,538 disk hits** replaced the RAM-only control's **2,538 image fetches**.
Metadata continued loading normally, so this exercised the real paged Library and its renderer.
The cache occupied 39,512,400 bytes, including file headers, for 1,200 files.

These measurements describe the original feature build identified by the checksum in the JSON.
The later integration with main retains its newer scrolling deferral, transient retry and
residency-backoff policies; the recorded FPS values are not measurements of that integrated build.

## Frame pacing and memory

Optimized ARM debug build, 1920×1080 UI, on the LG development television. The same binary and
six-column, 1,200-item scroll oscillator ran each leg. Stack/GPU/CPU profilers were absent from
these pacing runs. The existing slow-frame detector and new interval counters were armed in all
three. The other installed app was settled (`fps=0`), with no playback route active.

| Run | Image fetches | Median 1 s FPS | Intervals >33.3 ms | >50 ms | Longest interval | Max sampled RSS |
|---|---:|---:|---:|---:|---:|---:|
| RAM-only control | 2,538 | 56 | 154/8,453 (1.82%) | 4 | 57.6 ms | 72.8 MiB |
| Cold disk cache | 1,200 | 56 | 154/9,535 (1.61%) | 4 | 65.4 ms | 74.3 MiB |
| Warm disk cache | 0 | 56 | 109/8,529 (1.28%) | 2 | 53.7 ms | 73.2 MiB |

There were no intervals over 100 ms in these runs. The warm run reduced the observed frequency
of >33.3 ms intervals relative to the control, while median FPS was identical. This is a measured
comparison, not a guarantee of steady 60 FPS: visible stalls remain. The cold run's longest
65.4 ms frame spent 64.3 ms in drawing with no image upload. A cache hit alone does not establish
the cause of a graphics stall.

Intervals are consecutive Swap-to-Swap times, excluding intentional idle gaps. Counts and maxima
are exact to the performance counter; displayed milliseconds are rounded. First five Library
heartbeat windows and the incomplete final window are excluded. The JSON includes worst per-window
p95/p99 bounds; these are not whole-run percentiles. Process IDs were distinct: cold 6215, warm
13444, control 22244. The GPU tier stayed within its existing 44 MiB limit; peak queued pixels
were 2.25 MB cold and 3 MB warm, below the 8 MiB admission threshold. In-flight decodes can exceed
that threshold, so it is not advertised as an absolute process-memory limit.

## What the stack profile found

A separate, invasive render-thread profile collected 1,200 usable stacks against a verified
matching binary before the catalog-snapshot optimization. It is excluded from pacing results.
334 samples contained `LibraryKey` cloning or destruction. A representative CPU stack was:

```text
Dispatcher::drain
  Dispatcher::absorb
    Dispatcher::return_state
      LibraryScreen::page_memory
        Vec<LibraryKey>::clone
          String::clone
            malloc
```

The Library now shares immutable key storage between the registry and saved page memory.
Reconciliation uses copy-on-write on actual changes; ordinary scroll frames reuse the storage.
The 1,200-key regression failed before the fix and passed afterward, including snapshot isolation
and unchanged canonical state. Stack samples also included a Wayland poll wait under
`GfxUploader::warm`; remaining slow-frame phase timings include drawing and texture preparation.

## Evidence and scope

- [Sanitized aggregate results](global-image-cache-2026-09-26.json), including raw counters,
  memory distributions, slow CPU-frame attribution, process IDs and binary checksums.
- [Architecture and reproducible fixture procedure](../image-cache.md).
- Device captures were opened and inspected, including tile 1,027 during the image-disabled
  warm run. The fixture uses unique source keys and JPEG bytes with shared raster art, so it
  tests cache cardinality rather than 1,200 different decoder encodings.
- Real Plex browsing was also checked visually: ordinary posters, Home backdrops and transparent
  title artwork. The original debug cache was preserved for the stress run and restored before
  this check; the stable installation was not modified. A further real-Plex process restart
  recorded 15 disk hits and zero image fetches while displaying Home artwork.
- Host tests cover byte/count eviction, corrupt/oversized files, atomic write failure, legacy
  avatar migration, freshness versus recency, account-generation races, GPU eviction reload,
  pending-byte accounting and refresh-queue bounds. The 40 MB device working set does not
  establish on-device behavior above the 128 MiB eviction limit.
- `make check`, the shipping `--no-default-features` check, the ARM build, and the stress report
  passed. The firmware import audit passed every inventoried release at or above 4.4.2.
  The host compatibility suite reports its native-Linux comparison as skipped on macOS.
