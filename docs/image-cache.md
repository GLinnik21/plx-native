# Image cache

## Storage and ownership

All reusable remote artwork enters `app/adapters/poster.rs` through the shared `ui::tex::Source`.
Its two demand workers read the disk tier before fetching and decode off the render thread.
`imgcache.rs` owns persistence; `ui::tex::TexCache` owns GPU residency. Screens choose the image
and size, without choosing a caching policy. The sign-in QR code is transient authentication data
and remains outside the artwork cache; bundled icons and fonts already live in the app package.

Disk entries contain compressed image bytes and a small versioned header with fetch time and
payload length. A SHA-256 filename identifies the stable Plex machine id (canonical server origin
until an id is known), source and every requested transform. The outer `X-Plex-Token` is excluded.
Source version queries, size, cover mode and format remain distinct. Only the recognized Plex
avatar roster's volatile `c=` stamp is ignored. No request URL or token is stored as metadata.
Existing avatar files migrate on their first hit while preserving their original refresh time.

The cache chooses a writable persistent directory through `paths::image_cache_candidates`,
isolated by installation flavour (or simulator instance). It caps committed storage at **128 MiB**
and **8,192 files**, with **4 MiB** maximum compressed image size. One temporary write can consume
up to another image's size. A lazy startup scan builds a byte-accounted LRU index; normal reads
and writes use that index, avoiding repeated directory scans. Reads advance recency in memory;
file modification times persist recency at most once an hour per accessed entry. The header's
fetch timestamp never moves on a hit. Writes use a temporary file and rename, cache reads are
size-bounded, and corrupt entries are removed and fetched again. Unavailable storage is a cache
miss; a failed write cannot prevent the image from displaying.

On a disk hit older than **24 hours**, stale artwork is displayed immediately. A separate refresh worker starts work
when demand loading is idle, with at most 32 queued jobs and a bounded recent-attempt set to
suppress retry storms. Refresh validates decoding before replacing a file and affects the next
load; it never holds the source-store lock while accessing the network. A failed refresh keeps
the old image. Changed versioned source paths miss immediately rather than waiting for expiry.

RAM residency remains independent of library size: 64 source slots and a 44 MiB GPU texture
budget. Demand workers stop claiming requests when combined decoded and pending-upload pixels
reach 8 MiB; already active decodes can temporarily exceed this admission threshold. If the GPU evicts an image while its source slot survives, the next draw requests its
pixels again through the same disk-first path, respecting scrolling deferral and the source's
residency backoff. Sign-out advances the cache epoch and sweeps all
candidate directories; requests capture that epoch when queued, so old work cannot read, remove,
write or publish images after account erasure. Profile switching within one account retains
reusable artwork. Filesystem deletion failures remain best effort, like the existing avatar cache.

## Reproducible television stress test

`tools/image-cache-stress.py` serves a synthetic Plex library with 1,200 movies. Each has its own
rating key, thumbnail path and image-cache key. The approximately 33 KB JPEG fixture shares its
raster artwork between tiles; an ordinal JPEG comment also makes each response's bytes distinct.
This tests 1,200 independent cached objects, rather than drawing one cached poster 1,200 times.
The fixture needs only Python's standard library and contains no credentials or private media.

The server and report commands are host-only and do not operate the television:

```sh
python3 tools/image-cache-stress.py serve --access-log /tmp/image-cache-access.jsonl
python3 tools/image-cache-stress.py control --phase warm --images off
python3 tools/image-cache-stress.py report \
  --access-log /tmp/image-cache-access.jsonl \
  --cold-events /tmp/image-cache-cold.log \
  --warm-events /tmp/image-cache-warm.log \
  --cold-memory /tmp/image-cache-cold-memory.jsonl \
  --warm-memory /tmp/image-cache-warm-memory.jsonl
```

Use a new access-log path for every run; the server refuses to overwrite existing evidence.
`--count` can raise the library size to 10,000. `--port` defaults to 8027. The loopback-only control
endpoint changes the phase label and enables or refuses image responses without affecting metadata.
`GET /__stress/stats` reports both phases' request and distinct-key counts. Neither endpoint logs
Plex query tokens.

The device-owning lane follows [tv-lock](../.agents/skills/tv-lock/SKILL.md) and
[tv-session](../.agents/skills/tv-session/SKILL.md), using the debug installation. Read its runtime
root and event-log path with `make -s print-rundir FLAVOR=debug` and
`make -s print-eventlog FLAVOR=debug`. `tv-session.sh up` clears triggers, so custom fixture triggers
must be installed for a subsequent close-first `make FLAVOR=debug run` launch under the same lease.

1. Build and deploy the current debug binary. Confirm its deployed hash, then close the app. Set
   aside its image-cache directory for a cold run, preserving any pre-existing cache to restore
   afterwards. Do not touch the stable installation or sign out to clear the cache.
2. In the debug runtime root, arm `plxnative-pms-origin` with `http://<HOST_LAN_IP>:8027`,
   `plxnative-token` with a synthetic value such as `image-cache-fixture`, and empty
   `plxnative-library`, `plxnative-libosc`, `plxnative-framedrop` and `plxnative-imagecache-stats`
   files. Remove the GPU/CPU profiler and recorder triggers for quotable frame rates.
3. Launch the app and verify the `route=library` heartbeat and 1,200-item grid. The existing
   oscillator traverses the real grid at one row per 350 ms and reverses at the document ends.
   At six columns, allow **at least 160 seconds** per leg for a full down-and-up traversal, including
   startup. Longer legs may be needed if discovery or the host firewall delays requests. Finish
   based on the distinct-image and disk-hit evidence, not the elapsed time alone.
4. While running, sample the selected binary's PID (use its executable inode, not a bare
   `pidof plxnative`) and `/proc/<pid>/status`. Save JSONL memory rows shaped as
   `{"at_s":0,"rss_kib":123456,"hwm_kib":123456}`. Capture and inspect the grid near both ends;
   take screenshots outside the pacing interval. Keep the complete cold event log and PID.
5. Close the process, retaining the disk cache, then run the control command above. Relaunch
   the same binary with the same triggers; confirm a **new process** and repeat the traversal,
   memory sampling and captures. The fixture still answers metadata, but any image request now
   returns HTTP 503 and is counted as a failed warm request. Preserve the warm event log and PID.
6. Run the report. It requires over 1,000 distinct fetched images in the cold leg, over 1,000
   listing items paged and disk-cache hits in the warm leg, zero warm network image requests and
   fetches, populated cache entries/bytes, and at least 90 Library heartbeat samples per leg.
   Simulator, recorder and profiler logs are refused as pacing evidence. At least 1,000 actual
   frame intervals must also be recorded. The report includes frame/loop-rate distributions,
   exact counts above 16.7/33.3/50/100 ms, longest presentation gap, worst per-second p95/p99,
   slow CPU-frame phase attribution, and RSS distributions when supplied. The first five Library
   heartbeat samples and the unfinished last window are excluded from timing summaries. It does not claim
   that the two PIDs differ or grade visual completeness: keep that evidence alongside it.
7. Stop the fixture, close the automated app, restore any cache set aside for the test, clear the
   fixture triggers, restore the interactive app through `tv-session.sh down`, and release the TV.

The workload's encoded working set is approximately 40 MB. A larger real library or cache-capacity
test must separately exceed the configured cache limit; this cold/warm experiment establishes
persistence across process restarts and the cost of traversing more than 1,000 unique image keys.
The ordinary 18-second `fps:library-scroll` scene is too short to establish that coverage.

Host checks for the fixture and its evidence gates are `python3 tests/test_image_cache_stress.py`.

## Large-grid snapshot cost

The device stack profile exposed another cost that grows with tile count: the Library used to
clone and destroy the entire `LibraryKey` catalog whenever the dispatcher saved its return state,
including ordinary scrolling frames. The registry and saved page memory now share immutable key
storage. Reconciliation detaches it only when identities or recovery positions actually change;
saved routes retain their old values, and canonical state serialization is unchanged. A 1,200-key
regression checks both reuse on scrolling frames and snapshot isolation after catalog changes.

## Frame-drop control run

A 60 FPS average does not rule out a noticeable stall. With `plxnative-framedrop` armed, the
heartbeat now measures consecutive Swap-to-Swap intervals as well as CPU work. Intentional idle
gaps are excluded. `frame_gt16`, `frame_gt33`, `frame_gt50` and `frame_gt100` count intervals
strictly longer than 1/60, 1/30, 1/20 and 1/10 second. The fixed-memory histogram reports rounded-up
millisecond percentile bounds per heartbeat, with an overflow percentile bounded by the observed
maximum. The report labels the worst per-window percentiles rather than presenting them as
percentiles of the entire run. `FRAMEDROP` lines separately explain slow CPU work by phase and
upload count. The existing `dropped=` field counts undeliverable UI messages and is **not** a
frame-drop counter.

For an A/B control on the same binary, arm the diagnostic `plxnative-imagecache-bypass`, label the
fixture phase `baseline` with images on, restart, and repeat the same traversal. This bypasses
only disk reads/writes; the bounded RAM/GPU tiers and frame budget stay the same. Remove the
trigger before cold/warm legs. Pass `--baseline-events` and optionally `--baseline-memory` to
the report to compare actual gap frequencies and longest stalls. Capture screenshots outside
the measured intervals. A cache hit saving network bytes does not establish a frame-pacing
improvement; compare the distributions before making that claim.
