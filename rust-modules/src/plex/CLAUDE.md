# plex/ — the Plex data layer (typed PMS + plex.tv client)

This is the **typed Plex client**: account/login (`account.rs`, `session.rs`), the HTTP client
(`client.rs`), library/hubs/metadata reads (`library.rs`/`hubs.rs`/`models.rs`), and the whole
playback protocol — the MDE/transcode decision + capability profile (`transcoder.rs`), the
timeline/PlayQueue/identity session ops (`timeline.rs`), stream selection + the direct-play
target (`library.rs`), with typed request params in `params.rs`. **Every PMS query in the app
is built here** (route.rs holds playback *state* + policy, never a query string). The
authoritative REST spec is **`docs/pms-api.md`** (verified) — read it before adding an
endpoint; don't reverse-engineer PMS from scratch.

## The guiding principle: direct-play first

The whole point of this client is that **the TV plays the library natively**. Prefer a **direct-play**
part over a transcode wherever the TV can decode it; when a transcode is unavoidable, request it as a
**progressive stream the existing demuxer already handles** (H264+AC3 MKV) rather than inventing a new
container/codec path. Every "just transcode it" shortcut pushes work onto the server *and* degrades
quality — reach for it only when direct-play genuinely can't work. See `[[server-hevc-encode-gating]]`
and the `soft-subs` note below for why "direct-play everything" keeps paying off.

## Gotchas that bite (all verified in code)

- **Content negotiation: send `Accept: application/json` explicitly.** PMS returns **XML** for
  `Accept: */*` (or no Accept), and only JSON for an explicit `application/json`. A request that
  forgets it silently gets XML → the JSON parser finds no `Metadata` → **0 items, empty home**. The
  raw-socket `stream.rs` used to force `*/*` on every request; the client sends JSON Accept unless the
  caller overrides it (playback/part/photo endpoints ignore Accept).
- **PMS string-encodes numbers, so deserializers must be lenient.** Fields like `size`/`viewOffset`/
  `duration` arrive as JSON **strings** (`"1234"`) on some endpoints and **numbers** on others; some
  bools (`Stream.default`/`selected`) arrive as `true`/`false` *and* as `"1"`/`"0"`. Every numeric
  field goes through `de_i64`/`de_f64` (in `models.rs`) whose untagged enum accepts int **and** string
  **and** bool. Omitting a lenient adapter doesn't just drop one field — serde fails the **whole
  `MediaContainer`** parse → empty result. When you add a model field, use the lenient adapter.
- **Track selection is server-side, via `PUT /library/parts/{id}`** (set the chosen audio/subtitle
  stream + subtitle burn), **not** query params on the stream URL. The server re-selects for the next
  decision; the client re-requests the part. See `[[audio-subtitle-track-switching]]`.
- **Subtitles: assume client rendering, not the server.** WebVTT sidecars do **not** deliver on our
  progressive-MKV pipeline (they come back empty / 501), so during a transcode the only server option
  is **burn** — which is why direct-play + our own subtitle renderer (see `player/`) is the real
  answer. Don't add a soft-subtitle path that only works on paper. See `[[soft-subs-during-transcode]]`.

## Where the bytes go next

This layer only *decides and locates*; the actual byte stream (direct-play or transcode) is pulled and
demuxed by `stream.rs`/`mkv.rs` and fed to Starfish by the `player/` engine — see `player/CLAUDE.md`
for the Load-payload/codec and subtitle-rendering rules on the other side of the handoff.
