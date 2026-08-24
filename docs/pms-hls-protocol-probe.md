# PMS HLS protocol probe

Measured 2026-08-24 with the host-only `tools/pms-hls-probe.py`. This report deliberately omits
the server address and identity, account identifiers, token, library item identifier, raw session
identifiers, titles, paths, and segment hashes. The source media was a configured test item which
forces both video and audio transcoding. The exported evidence intentionally does not retain an
account identity.

## Result: FixedSession

For this PMS and the probe's Generic HLS/MPEG-TS/H.264/AAC client profile, one universal-transcoder
session has one fixed rendition. It did not expose a client-selectable variant ladder, and its
actual segment resolution did not change after the client reported a large bandwidth increase and
decrease.

| leg | request | master | decision / actual segment |
|---|---|---|---|
| fixed low | 720 kbps, 854x480 | one variant | H.264 480x270, 425 kbps advertised; sampled segment 480x270 |
| fixed high | 12,000 kbps, 1920x1080 | one variant | H.264 1920x1080, about 11.356 Mbps advertised; sampled segment 1920x1080 |
| Auto, paced | 720 kbps initial, 20,000 kbps peak, `autoAdjustQuality=1` | one variant and one media playlist | all 61 sampled segments remained H.264 480x200 |

The corrected Auto leg fetched one initial segment, then 30 consecutive two-second segments while
reporting 20,000 kbps, followed by 30 while reporting 512 kbps. Timeline reports used
`bufferedTime=10000` in PMS's observed millisecond wire units. The master continued to contain one
variant; the actual video stream stayed 480x200 throughout; and PMS's returned bandwidth record
stayed at its initial 575 kbps / SD entry.

This distinguishes the three protocol shapes as follows:

- It is not **ClientVariants**: the master contained one `#EXT-X-STREAM-INF`, not a ladder of
  independently selectable child playlists.
- It is not **ServerManaged** in the wire-observable sense: neither the unchanged media-playlist
  path nor the decoded segment shape changed within the live session. A status or timeline hint
  alone would not establish a rendition change.
- It is **FixedSession** for the measured PMS/profile: the fixed low and high control sessions
  produced materially different outputs, proving that the server honors a quality choice when a
  session is created, while the long Auto session did not change its output after opposite
  bandwidth signals.

This is not a claim about every PMS version, client profile, or media shape. In particular, Plex's
[Automatically Adjust Quality](https://support.plex.tv/articles/115007570148-automatically-adjust-quality-when-streaming/)
documentation describes an app-controlled feature; the `autoAdjustQuality` flag by itself is not
evidence that a server autonomously switches the wire rendition.

## Session identifiers

A follow-up mismatch probe measured the three wire fields with distinct redacted sentinels. PMS
used the legacy `session=` query value as the physical encoder identity and the documented
`X-Plex-Session-Identifier` header as the playback/timeline identity. The canonical
`transcodeSessionId=` query value was not adopted by this PMS/profile.

After a timeline signal, the allowlisted `/status/sessions` entry reported the header sentinel as
`Session.id` and the legacy-query sentinel as `TranscodeSession.key`. Cleanup independently
confirmed the ownership result: stopping the legacy-query candidate returned 200, while the
canonical-query and header candidates each returned 404. The same encoder precedence was observed
in a fixed leg without a timeline signal.

A subsequent simultaneous-encoder TV spike established the stronger lifecycle rule. A candidate
with a new legacy `session=` but the old playback's `X-Plex-Session-Identifier` could return its
master and media playlists, but its segment zero remained 404; after the candidate was abandoned,
the old encoder's next segment also remained 404. Sharing the X-Plex identity therefore replaces
or invalidates the old session before prime completes on this PMS. PlxNative keeps a stable
playback generation internally, but couples `session=` and `X-Plex-Session-Identifier` to one new
value for each physical encoder. Timeline, seek, and teardown follow the currently published
encoder wire identity.

The tool now constructs that experiment without putting raw IDs into its report:

- `baseline` preserves the measured equal legacy/query-X layout;
- `legacy` and `canonical` pair the named query field with the X-Plex header;
- `matched` puts one alias on legacy, canonical, and header wires;
- `mismatch` generates three distinct values; and
- `explicit` accepts selected legacy, canonical, and header values.

The report stores only aliases such as `sid-1`. Before the decision request, the cleanup ledger
arms every distinct candidate from all active wires. Cleanup sends the existing universal
transcoder stop request for each candidate. Read-only `/status/sessions` snapshots after start and
after sampling keep only entries belonging to the probe client, alias the playback `Session.id` and
the encoder ID in `TranscodeSession.key`, and add any newly observed ID to the same cleanup ledger.
A 2xx stop response or a 404 (candidate absent) settles an entry, while transport and other HTTP
failures remain visibly pending.

## Production implementation and device result

The measured `FixedSession` result determines the production architecture. Auto does not ask PMS
to mutate one encoder and does not select among variants. It keeps one internal playback
generation, but gives each physical encoder the same fresh value on the legacy `session=` and
`X-Plex-Session-Identifier` wires. A move is proposed from the current segment measurements, a new
encoder is registered at the current content offset, and its first segment is downloaded and fully
demuxed off-screen. Only a candidate with an in-bounds decoded raster, a complete decodable H.264
IDR, valid AAC framing/timestamps, enough network and PMS-production headroom, and a surviving A/V
buffer reserve is committed. The old encoder is stopped only after that commit.

The client parser intentionally implements the observed subset rather than generic HLS: one master
variant; one MPEG-TS media playlist; media sequence, target duration, signed `EXT-X-START`, segment
durations, `ENDLIST`, relative or exact-same-origin absolute URLs, and the measured legacy
`ALLOW-CACHE` spelling. Encryption, byte ranges, init maps/fMP4, discontinuities, multiple variants,
cross-origin children and ambiguous path forms fail closed. Each TS segment owns a fresh FFmpeg
context. Segment-local FFmpeg timestamps never reach the controller: video and audio are normalized
onto one millisecond content timeline, including seek/start offsets and recovered AAC frame clocks.

The controller combines three signals: body-byte throughput, total segment acquisition time divided
by media duration (which catches a JIT transcoder running near or below real time), and buffered
content duration `min(video_tail, audio_tail) - playback_position`. A downshift needs any critical
signal to fail and a measured link collapse jumps directly to its sustainable rung. An upshift
requires sustained agreement from every signal, then has an absolute prime deadline equal to 80%
of one segment in both raw-socket and libcurl reads; after that point the same production gate could
not accept it, so the candidate is abandoned before it can drain the active playback reserve.

On the target LG television, the four shaped links settled as follows:

| shaped link | committed request | decoded video |
|---|---:|---:|
| 512 Kbps | 320 Kbps | 320×134 |
| 1 Mbps | 720 Kbps | 480×200 |
| 7 Mbps | 4 Mbps | 1280×536 |
| 17.5 Mbps | 8 Mbps | 1920×804 |

All resolution changes stayed inside one Starfish Load. A 720p→1080p→720p fixture independently
proved that the LG pipeline accepts in-band H.264 parameter-set/raster changes on one Load. The live
PMS run retained the full 1:40:03 playlist duration instead of exposing only the downloaded tail; a
mid-movie seek selected the correct playlist segment and continued on a freshly normalized A/V
timeline; and a live high-to-512 Kbps collapse returned directly to 320 Kbps without walking or
blocking on oversized intermediate encoders.

## Safety and artifacts

The probe keeps the token in an HTTP header and rejects cross-origin playlist children and
redirects before a credential-bearing redirected request can be sent. Its output directory is
private (`0700`), unique by default, and must remain outside the repository. Playlists replace the
origin, every candidate session ID, token spellings (including URL-encoded forms), and the item
path. Decision and timeline bodies are reduced to allowlisted technical fields. Segment bytes live
only in a temporary file long enough for `ffprobe`; the report retains derived codec/raster facts,
timing, byte counts, and a hash.

Each invocation also uses a unique probe client identifier. That keeps `/status/sessions` filtering
and cleanup from adopting a concurrent probe process's encoder; the identifier itself is not
written to the report.

All measured legs confirmed the universal-transcoder stop request with HTTP 200. The tool does not
send scrobble, progress-reset, play-queue, stream-selection, or device commands. Auto mode does
send timeline bandwidth reports as the managed test profile; that is why it is opt-in.

Offline verification (no PMS or device access):

```sh
PYTHONPYCACHEPREFIX=/private/tmp/plxnative-pycache \
  python3 -m py_compile tools/pms-hls-probe.py tools/test_pms_hls_probe.py
python3 tools/test_pms_hls_probe.py
python3 tools/pms-hls-probe.py --help
```
