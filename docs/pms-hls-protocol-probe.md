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
encoder wire identity. A transcode seek is itself a new encoder registration: it first gets a
fresh coupled value, atomically publishes the replacement URL/identity after `/decision` succeeds,
then stops the previous exact opaque key. Re-registering the old key is not a seek primitive — the
server archive showed the same key starting twice and retaining stale resource state.

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
IDR, valid AAC framing/timestamps, and a passing direction-specific acquisition/reserve transaction
is committed. The old encoder is stopped only after that commit.

The client parser intentionally implements the observed subset rather than generic HLS: one master
variant; one MPEG-TS media playlist; media sequence, target duration, signed `EXT-X-START`, segment
durations, `ENDLIST`, relative or exact-same-origin absolute URLs, and the measured legacy
`ALLOW-CACHE` spelling. Encryption, byte ranges, init maps/fMP4, discontinuities, multiple variants,
cross-origin children and ambiguous path forms fail closed. Each TS segment owns a fresh FFmpeg
context. Segment-local FFmpeg timestamps never reach the controller: video and audio are normalized
onto one millisecond content timeline, including seek/start offsets and recovered AAC frame clocks.

The controller measures three things: body-byte throughput, total segment acquisition time divided
by media duration (which catches a JIT transcoder running near or below real time), and buffered
content duration `min(video_tail, audio_tail) - playback_position`. **What it does with them was
rewritten on 2026-08-25** and this paragraph used to describe the earlier rule — "a downshift needs
any critical signal to fail, an upshift requires sustained agreement from every signal" — which is
no longer how either direction is decided. Today they become a delivery estimate with uncertainty,
an end-to-end acquisition diagnostic/projection, and a buffer level plus slope. The acquisition
ratio spans PMS wait, pacing and transfer, so it is not an independent production gate.
`docs/adaptive-playback.md` is the current design. An upshift transaction now spends the exact
disposable exploration reserve `E = max(B - max(R_s,D), 0)` as one absolute end-to-end deadline through decision, playlists and
candidate media; the old fixed 80%-of-segment prime deadline no longer exists.

One measurement in this document is a property of the SERVER and one is a property of the CLIENT's
ladder, and the second one moved. The bitrate/raster boundary below is the server's and stands. The
settle table is the client's and was taken when Auto had six actuators: on today's thirteen the
17.5 Mbit/s leg lands on the 10 Mbps rung rather than 8 (there was nothing between 8 and 20 to
choose then, so 12 Mbit/s of a measured link went unspent), and it reaches it in two moves rather
than one. Read the table as what that ladder did on those links, not as what today's ladder would
do.

On the target LG television, the four shaped links settled as follows (six-rung ladder,
2026-08-24):

| shaped link | committed request | decoded video |
|---|---:|---:|
| 512 Kbps | 320 Kbps | 320×134 |
| 1 Mbps | 720 Kbps | 480×200 |
| 7 Mbps | 4 Mbps | 1280×536 |
| 17.5 Mbps | 8 Mbps | 1920×804 |

## The bitrate ceiling is an actuator, not a promise

A follow-up leg measured what this PMS does with the top of the range, and both halves matter to a
client that has to spend a budget:

| request | resolution ceiling | decision / actual |
|---:|---|---|
| ≤ 21,750 kbps | 3840×2160 | 1920×1080, decision reaching about 20,011 kbps |
| 22,000 kbps | 3840×2160 | **3840×2160**, advertised about 20,895 kbps |
| 22,000 – 60,000 kbps | 3840×2160 | the same 3840×2160 output |

So the raster changes at a request boundary the wire rate does not follow: asking for 20,895 kbps
gets 1080p, and asking for 22,000 does not get 22 Mbit/s of bits. The 1080p point produced segments
at a 0.21 acquisition ratio and the 4K point at 0.44: **4% more bits for roughly double the
calibrated server work**. That difference remains a recurring cost in Original/HLS utility. Live
HLS admission does not turn the total acquisition ratio into an independent production constraint;
the candidate's complete acquisition already includes the same service episode.

As with everything else here, this is one PMS, one client profile and one media shape. It is not a
claim about a universal Plex maximum, and the client's transaction grades the actual segment rather
than trusting the table.

> **Corrected 2026-08-27 — the table above is ITEM-specific, and it was read as server-specific.**
> A full-ladder sweep on three items (`docs/measurements/p2h-pms-ladder.md`) reproduces the
> *boundary* exactly — 21,750 kbps still returns the lower operating point and 21,999 the higher —
> but **not the value below it**: on a 1080p 2.39:1 film the lower point declares **16,150 kbps**,
> not the "about 20,011" recorded here. The 20,011 was that probe's item. The boundary generalises;
> the rate does not, and `HlsActuatorCatalog::measured()` stored the rate.
>
> Two further corrections to the reading, both measured rather than argued. **The resolution
> ceiling is not what unlocks the higher rate** — 20,000 kbps returns 16,150 with a 3840x2160
> ceiling and with a 1920x1080 one alike, so "asking for 20,895 gets 1080p" is about the requested
> BITRATE, not the box. And **"22,000 flips the output to 3840x2160" holds only when the SOURCE is
> 4K**: on a 1080p master, 22,000 with a 4K ceiling still returns 1918x802, because PMS never
> upscales. The 4K item in the sweep does return 3840x2160 there, which is what makes the original
> sentence true of its own measurement and false as a general rule.

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
